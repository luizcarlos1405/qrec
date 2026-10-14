use std::thread;
use std::time::{Duration, Instant};

use qrec_core::ErrorCode;
use qrec_core::protocol::{Progress, ProgressPhase, RecordModeArg};
use qrec_core::state::HostKind;
use serde_json::json;

use super::*;
use crate::host::lockfile::{self, HostState, LockRecord};
use crate::ipc::testing::{
    Collect, hold_lock, line_id, raw_server, send_line, session_thread, socket_dir, start_server,
    wait_for,
};
use crate::msg::{Msg, Responder};

fn progress(done: u64) -> Progress {
    Progress {
        phase: ProgressPhase::Render,
        done,
        total: 3,
        eta_secs: None,
        label: String::new(),
        take: None,
    }
}

fn undo() -> Request {
    Request::command(Command::Undo { steps: 1 }, false)
}

fn quick(interrupts: &Interrupts) -> SendOpts {
    let mut opts = SendOpts::new(interrupts);
    opts.accept_timeout = Duration::from_millis(300);
    opts
}

#[test]
fn hello_reaches_live_hosts_and_classifies_failures() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let info = hello(&t.socket, HELLO_TIMEOUT).unwrap();
    assert_eq!(
        (info.pid, info.host),
        (std::process::id(), HostKind::Headless)
    );

    assert!(matches!(
        hello(&dir.path().join("missing.sock"), HELLO_TIMEOUT),
        Err(HelloError::NoHost)
    ));
    // A socket file left by a dead host: refused, i.e. no host.
    let stale = dir.path().join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    assert!(matches!(
        hello(&stale, HELLO_TIMEOUT),
        Err(HelloError::NoHost)
    ));

    // Accepts but never answers: a timeout, quickly.
    let silent = raw_server(dir.path(), "silent.sock", |_, _stream| {
        thread::sleep(Duration::from_secs(2));
    });
    let started = Instant::now();
    assert!(matches!(
        hello(&silent.socket, Duration::from_millis(200)),
        Err(HelloError::Timeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));

    // Closes without answering.
    let closer = raw_server(dir.path(), "closer.sock", |_, stream| drop(stream));
    assert!(matches!(
        hello(&closer.socket, HELLO_TIMEOUT),
        Err(HelloError::Disconnected)
    ));

    // A failed host answers with its error: alive, but no HelloInfo.
    t.server.fail(CoreError::invalid("broken project"));
    assert!(matches!(
        hello(&t.socket, HELLO_TIMEOUT),
        Err(HelloError::Refused(e)) if e.code == ErrorCode::Invalid
    ));
}

#[test]
fn requests_stream_accepted_progress_and_the_reply() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let (inbox, _session) = session_thread(|msg| {
        if let Msg::Request { mut to, .. } = msg {
            for done in 1..=3 {
                to.progress(&progress(done));
                thread::sleep(Duration::from_millis(20));
            }
            to.reply(Ok(json!({ "seq": 7 })));
        }
        true
    });
    t.server.attach(inbox);
    let interrupts = Interrupts::new();
    let mut out = Collect::default();
    let sent = send_request(&t.socket, &undo(), &mut out, &quick(&interrupts)).unwrap();
    assert_eq!(sent, Sent::Replied);
    assert_eq!(out.accepted, 1);
    assert_eq!(out.progress.last(), Some(&3));
    assert_eq!(out.ok()["seq"], 7);

    // An error reply is a reply too.
    let (inbox, _session) = session_thread(|msg| {
        if let Msg::Request { mut to, .. } = msg {
            to.reply(Err(CoreError::not_found("no clip c1")));
        }
        true
    });
    let dir2 = socket_dir();
    let t2 = start_server(dir2.path(), |_| {});
    t2.server.attach(inbox);
    let mut out = Collect::default();
    send_request(&t2.socket, &undo(), &mut out, &quick(&interrupts)).unwrap();
    assert_eq!(out.err().code, ErrorCode::NotFound);
}

#[test]
fn eof_before_accepted_is_safe_to_reroute_and_after_is_not() {
    let dir = socket_dir();
    let interrupts = Interrupts::new();
    let before = raw_server(dir.path(), "before.sock", |_, stream| drop(stream));
    let mut out = Collect::default();
    assert_eq!(
        send_request(&before.socket, &undo(), &mut out, &quick(&interrupts)),
        Err(SendError::NotAccepted)
    );
    assert!(out.replies.is_empty());

    let after = raw_server(dir.path(), "after.sock", |line, stream| {
        send_line(&stream, &json!({ "id": line_id(&line), "accepted": true }));
        drop(stream);
    });
    let mut out = Collect::default();
    assert_eq!(
        send_request(&after.socket, &undo(), &mut out, &quick(&interrupts)),
        Err(SendError::Disconnected)
    );
    assert_eq!(out.accepted, 1);
    assert!(out.replies.is_empty());

    let nobody = dir.path().join("nobody.sock");
    assert_eq!(
        send_request(
            &nobody,
            &undo(),
            &mut Collect::default(),
            &quick(&interrupts)
        ),
        Err(SendError::NotAccepted)
    );
}

#[test]
fn a_host_that_never_accepts_is_not_responding() {
    let dir = socket_dir();
    let silent = raw_server(dir.path(), "silent.sock", |_, _stream| {
        thread::sleep(Duration::from_secs(2));
    });
    let interrupts = Interrupts::new();
    let started = Instant::now();
    assert_eq!(
        send_request(
            &silent.socket,
            &undo(),
            &mut Collect::default(),
            &quick(&interrupts)
        ),
        Err(SendError::NotResponding)
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn heartbeat_notices_a_host_that_stopped_answering() {
    let dir = socket_dir();
    // Accepts requests, then goes silent; never answers hello either.
    let stuck = raw_server(dir.path(), "stuck.sock", |line, stream| {
        let request: serde_json::Value = serde_json::from_slice(&line).unwrap();
        if request["body"] != json!("hello") {
            send_line(&stream, &json!({ "id": line_id(&line), "accepted": true }));
        }
        thread::sleep(Duration::from_secs(4));
    });
    let interrupts = Interrupts::new();
    let mut opts = quick(&interrupts);
    opts.heartbeat_after = Duration::from_millis(200);
    let started = Instant::now();
    assert_eq!(
        send_request(&stuck.socket, &undo(), &mut Collect::default(), &opts),
        Err(SendError::LostContact)
    );
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn ctrl_c_closes_the_connection() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let keep = std::sync::Arc::clone(&held);
    let (inbox, _session) = session_thread(move |msg| {
        keep.lock().unwrap().push(msg);
        true
    });
    t.server.attach(inbox);
    let interrupts = Interrupts::new();
    let raiser = interrupts.clone();
    let opts = quick(&interrupts);
    let socket = t.socket.clone();
    let client =
        thread::spawn(move || send_request(&socket, &undo(), &mut Collect::default(), &opts));
    assert!(wait_for(Duration::from_secs(5), || held
        .lock()
        .unwrap()
        .len()
        == 1));
    raiser.raise();
    assert_eq!(client.join().unwrap(), Ok(Sent::Interrupted));
    // The host sees the client leave.
    assert!(wait_for(Duration::from_secs(5), || t.server.stats().open == 0));
}

#[test]
fn record_wait_ctrl_c_stops_then_detaches() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    // Holds `record start --wait`; `record stop` finalizes it when `finish`.
    let finish = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let finishing = std::sync::Arc::clone(&finish);
    let stops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stops_seen = std::sync::Arc::clone(&stops);
    let mut waiter: Option<Box<dyn Responder>> = None;
    let (inbox, _session) = session_thread(move |msg| {
        if let Msg::Request { req, mut to } = msg {
            match req.body {
                WireBody::Command(Command::RecordStart { .. }) => {
                    to.progress(&progress(1));
                    waiter = Some(to);
                }
                WireBody::Command(Command::RecordStop) => {
                    stops_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if finishing.load(std::sync::atomic::Ordering::SeqCst) {
                        if let Some(mut w) = waiter.take() {
                            w.reply(Ok(json!({ "finalized": true })));
                        }
                    }
                    to.reply(Ok(json!({ "stopping": true })));
                }
                _ => {}
            }
        }
        true
    });
    t.server.attach(inbox);
    let start = Request::command(
        Command::RecordStart {
            mode: RecordModeArg::Append,
            countdown: None,
            wait: true,
        },
        false,
    );
    assert!(stops_on_interrupt(&start));
    assert!(!stops_on_interrupt(&undo()));

    // First Ctrl+C: RecordStop on a second connection; the finalized reply
    // still arrives on the first.
    let interrupts = Interrupts::new();
    let mut opts = quick(&interrupts);
    opts.stop_on_interrupt = true;
    let raiser = interrupts.clone();
    let socket = t.socket.clone();
    let request = start.clone();
    let client = thread::spawn(move || {
        let mut out = Collect::default();
        let sent = send_request(&socket, &request, &mut out, &opts);
        (sent, out)
    });
    assert!(wait_for(Duration::from_secs(5), || t
        .server
        .stats()
        .requests
        == 1));
    thread::sleep(Duration::from_millis(100));
    raiser.raise();
    let (sent, out) = client.join().unwrap();
    assert_eq!(sent, Ok(Sent::Replied));
    assert_eq!(out.ok()["finalized"], true);
    assert_eq!(stops.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Second Ctrl+C before the take is finalized: detach.
    finish.store(false, std::sync::atomic::Ordering::SeqCst);
    let interrupts = Interrupts::new();
    let mut opts = quick(&interrupts);
    opts.stop_on_interrupt = true;
    let raiser = interrupts.clone();
    let socket = t.socket.clone();
    let client =
        thread::spawn(move || send_request(&socket, &start, &mut Collect::default(), &opts));
    assert!(wait_for(Duration::from_secs(5), || t
        .server
        .stats()
        .requests
        == 2));
    thread::sleep(Duration::from_millis(100));
    raiser.raise();
    assert!(wait_for(Duration::from_secs(5), || stops
        .load(std::sync::atomic::Ordering::SeqCst)
        == 2));
    raiser.raise();
    assert_eq!(client.join().unwrap(), Ok(Sent::Interrupted));
}

// ------------------------------------------------------------------ routing

struct Project {
    _tmp: tempfile::TempDir,
    runtime: tempfile::TempDir,
    paths: ProjectPaths,
    socket: PathBuf,
}

fn project() -> Project {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = socket_dir();
    let socket = runtime.path().join("p.sock");
    Project {
        paths: ProjectPaths::new(tmp.path()),
        _tmp: tmp,
        runtime,
        socket,
    }
}

/// A lock record of this process.
fn record(kind: HostKind, socket: &Path, state: HostState) -> LockRecord {
    let mut record = LockRecord::starting(kind, socket, chrono::Utc::now());
    record.state = state;
    record
}

fn router<'a>(p: &'a Project, mode: RouteMode, ms: u64, interrupts: &'a Interrupts) -> Router<'a> {
    let mut r = Router::new(
        &p.paths,
        &p.socket,
        mode,
        Instant::now() + Duration::from_millis(ms),
        interrupts,
    );
    r.hello_timeout = Duration::from_millis(300);
    r
}

#[test]
fn routes_to_a_live_host() {
    let p = project();
    let t = start_server(p.runtime.path(), |_| {});
    assert_eq!(t.socket, p.socket);
    let interrupts = Interrupts::new();
    match route(&router(&p, RouteMode::Lock, 1000, &interrupts)).unwrap() {
        Routed::Host { socket, hello } => {
            assert_eq!(socket, p.socket);
            assert_eq!(hello.unwrap().pid, std::process::id());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn without_a_host_mutating_requests_take_the_lock() {
    let p = project();
    // A stale socket from a crashed host doesn't stop anyone.
    drop(std::os::unix::net::UnixListener::bind(&p.socket).unwrap());
    let interrupts = Interrupts::new();
    match route(&router(&p, RouteMode::Lock, 1000, &interrupts)).unwrap() {
        Routed::Locked(lock) => {
            assert!(ProjectLock::is_held(p.paths.root()).unwrap());
            drop(lock);
        }
        other => panic!("{other:?}"),
    }
    assert!(p.socket.exists(), "routing never deletes sockets");
    // Read-only requests never take it.
    match route(&router(&p, RouteMode::NoLock, 1000, &interrupts)).unwrap() {
        Routed::NoHost => {}
        other => panic!("{other:?}"),
    }
    assert!(!ProjectLock::is_held(p.paths.root()).unwrap());
}

#[test]
fn a_starting_host_is_waited_for() {
    let p = project();
    let lock = hold_lock(p.paths.root());
    lockfile::write(
        &lock,
        &record(HostKind::Cli, &p.socket, HostState::Starting),
    )
    .unwrap();
    let runtime = p.runtime.path().to_path_buf();
    let starter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        let t = start_server(&runtime, |_| {});
        lockfile::write(&lock, &record(HostKind::Cli, &t.socket, HostState::Ready)).unwrap();
        thread::sleep(Duration::from_secs(2));
        drop((t, lock));
    });
    let interrupts = Interrupts::new();
    let started = Instant::now();
    match route(&router(&p, RouteMode::Lock, 3000, &interrupts)).unwrap() {
        Routed::Host { .. } => {}
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() >= Duration::from_millis(250));
    starter.join().unwrap();
}

#[test]
fn a_lock_held_too_long_is_busy_naming_the_holder() {
    let p = project();
    let lock = hold_lock(p.paths.root());
    let rec = record(HostKind::Headless, &p.socket, HostState::Starting);
    lockfile::write(&lock, &rec).unwrap();
    let interrupts = Interrupts::new();
    let started = Instant::now();
    let err = match route(&router(&p, RouteMode::Lock, 300, &interrupts)) {
        Err(RouteError::Failed(e)) => e,
        other => panic!("{other:?}"),
    };
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert_eq!(err.code, ErrorCode::Busy);
    assert!(
        err.message.contains(&format!("pid {}", rec.pid)),
        "{}",
        err.message
    );
    assert!(err.message.contains("headless host"), "{}", err.message);
    assert!(
        err.message.contains("hasn't finished starting"),
        "{}",
        err.message
    );

    // Read-only requests wait for a host too, rather than reading under it.
    let err = match route(&router(&p, RouteMode::NoLock, 200, &interrupts)) {
        Err(RouteError::Failed(e)) => e,
        other => panic!("{other:?}"),
    };
    assert_eq!(err.code, ErrorCode::Busy);

    // No record at all: someone else holds the flock.
    lockfile::write(
        &lock,
        &record(HostKind::Headless, &p.socket, HostState::Ready),
    )
    .unwrap();
    std::fs::write(p.paths.lock_file(), b"").unwrap();
    let err = match route(&router(&p, RouteMode::Lock, 200, &interrupts)) {
        Err(RouteError::Failed(e)) => e,
        other => panic!("{other:?}"),
    };
    assert!(err.message.contains("runs no Qrec host"), "{}", err.message);
    drop(lock);
}

#[test]
fn a_host_that_never_answers_hello_is_not_responding() {
    let p = project();
    let lock = hold_lock(p.paths.root());
    let rec = record(HostKind::Gui, &p.socket, HostState::Ready);
    lockfile::write(&lock, &rec).unwrap();
    let _silent = raw_server(p.runtime.path(), "p.sock", |_, _stream| {
        thread::sleep(Duration::from_secs(2));
    });
    let interrupts = Interrupts::new();
    let err = match route(&router(&p, RouteMode::Lock, 3000, &interrupts)) {
        Err(RouteError::Failed(e)) => e,
        other => panic!("{other:?}"),
    };
    assert_eq!(err.code, ErrorCode::Busy);
    assert_eq!(
        err.message,
        format!("host pid {} is not responding", rec.pid)
    );
    drop(lock);
}

#[test]
fn routing_stops_on_ctrl_c() {
    let p = project();
    let lock = hold_lock(p.paths.root());
    lockfile::write(
        &lock,
        &record(HostKind::Cli, &p.socket, HostState::Starting),
    )
    .unwrap();
    let interrupts = Interrupts::new();
    let raiser = interrupts.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        raiser.raise();
    });
    let started = Instant::now();
    assert!(matches!(
        route(&router(&p, RouteMode::Lock, 10_000, &interrupts)),
        Err(RouteError::Interrupted)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(lock);
}

#[test]
fn the_record_names_a_socket_in_another_runtime_dir() {
    let p = project();
    let elsewhere = socket_dir();
    let t = start_server(elsewhere.path(), |_| {});
    let lock = hold_lock(p.paths.root());
    lockfile::write(&lock, &record(HostKind::Gui, &t.socket, HostState::Ready)).unwrap();
    let interrupts = Interrupts::new();
    match route(&router(&p, RouteMode::Lock, 2000, &interrupts)).unwrap() {
        Routed::Host { socket, .. } => assert_eq!(socket, t.socket),
        other => panic!("{other:?}"),
    }
    drop(lock);
}
