//! Takeover tests (G§3.4.3, WP0a acceptance) against hosts on threads of
//! this process with a fake Session. The fake-backend recording that a real
//! headless host makes is `crates/qrec/tests/cli.rs`'s
//! `a_gui_takes_over_a_headless_recording`.

use std::fs;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use chrono::Utc;
use qrec_core::ErrorCode;
use qrec_core::on_event::output::export_progress_view;
use qrec_core::protocol::{ExportFormat, Quality, RecordModeArg};
use qrec_core::state::{HostKind, JobKind, RecordPhase};

use super::*;
use crate::host::signals::Interrupts;
use crate::host::{
    ExitRules, GuiHost, GuiHostSpec, GuiPort, HeadlessArgs, HostEnd, HostOptions, run_headless,
};
use crate::ipc::client::{SendOpts, send_request};
use crate::ipc::server::GUI_REFUSES_SHUTDOWN;
use crate::ipc::testing::{
    Collect, FakeOpener, FakePort, hold_lock, line_id, quick_rules, raw_server, send_line, wait_for,
};
use crate::msg::Request;
use crate::testkit::TestEnv;

struct Fixture {
    _env: TestEnv,
    paths_env: Env,
    dir: PathBuf,
    socket: PathBuf,
}

fn fixture() -> Fixture {
    let env = TestEnv::new();
    let paths_env = env.paths_env();
    let dir = fs::canonicalize(env.empty_dir("swamp")).unwrap();
    let runtime = ensure_runtime_dir(&paths_env).unwrap();
    let socket = socket_path(&runtime, &dir).unwrap();
    Fixture {
        _env: env,
        paths_env,
        dir,
        socket,
    }
}

/// A headless host on a thread that never idles out by itself, so only a
/// `shutdown` ends it (unless `opener` ends its work).
fn headless(f: &Fixture, opener: FakeOpener) -> JoinHandle<u8> {
    let env = f.paths_env.clone();
    let args = HeadlessArgs {
        project: f.dir.clone(),
        lock_fd: None,
        ready_fd: None,
    };
    let host = thread::spawn(move || {
        let mut opener = opener;
        let opts = HostOptions {
            rules: ExitRules {
                idle_grace: Duration::from_secs(30),
                unused_timeout: Duration::from_secs(30),
                ..quick_rules()
            },
            ..HostOptions::default()
        };
        run_headless(args, &env, &mut opener, opts)
    });
    assert!(
        wait_for(Duration::from_secs(10), || client::hello(
            &f.socket,
            Duration::from_secs(1)
        )
        .is_ok()),
        "the host never answered hello"
    );
    host
}

fn send(socket: &Path, cmd: Command) -> Collect {
    let mut out = Collect::default();
    send_request(
        socket,
        &Request::command(cmd, false),
        &mut out,
        &SendOpts::new(&Interrupts::new()),
    )
    .unwrap();
    out
}

fn held_by(f: &Fixture) -> (HelloInfo, PathBuf) {
    match find_holder(&f.dir, &f.paths_env).unwrap() {
        Holder::Host { hello, socket } => (hello, socket),
        other => panic!("expected a live host, got {other:?}"),
    }
}

#[test]
fn a_free_project_is_ours() {
    let f = fixture();
    let Holder::Free(lock) = find_holder(&f.dir, &f.paths_env).unwrap() else {
        panic!("the project is free");
    };
    assert!(ProjectLock::is_held(&f.dir).unwrap());
    drop(lock);
    // (A child another test forks meanwhile may hold a copy until its exec.)
    assert!(wait_for(Duration::from_secs(5), || !ProjectLock::is_held(
        &f.dir
    )
    .unwrap()));
}

/// Idle: `shutdown`, then the lock.
#[test]
fn an_idle_host_is_shut_down_and_the_lock_becomes_ours() {
    let f = fixture();
    let host = headless(&f, FakeOpener::default());
    let (hello, socket) = held_by(&f);
    assert_eq!(socket, f.socket);
    assert_eq!(hello.host, HostKind::Headless);
    assert_eq!(hello.record, RecordPhase::Idle);
    assert!(hello.jobs.is_empty());
    request_shutdown(&socket).unwrap();
    let lock = wait_for_lock(&f.dir, LOCK_TIMEOUT).unwrap();
    assert_eq!(host.join().unwrap(), 0);
    assert!(ProjectLock::is_held(&f.dir).unwrap(), "ours");
    assert!(!f.socket.exists());
    drop(lock);
}

/// X1 with a fake Session: Stop & open finalizes the take, then the host
/// goes and the lock is ours.
#[test]
fn stop_and_open_finalizes_the_take() {
    let f = fixture();
    let opener = FakeOpener::default();
    let log = opener.log.clone();
    let host = headless(&f, opener);
    let start = Command::RecordStart {
        mode: RecordModeArg::Append,
        countdown: None,
        wait: false,
    };
    assert_eq!(send(&f.socket, start).ok()["recording"], true);
    let (hello, socket) = held_by(&f);
    assert!(
        matches!(hello.record, RecordPhase::Recording { .. }),
        "{hello:?}"
    );
    let finalized = stop_recording(&socket, STOP_TIMEOUT).unwrap();
    assert_eq!(finalized["finalized"], true, "{finalized}");
    request_shutdown(&socket).unwrap();
    let _lock = wait_for_lock(&f.dir, LOCK_TIMEOUT).unwrap();
    assert_eq!(host.join().unwrap(), 0);
    let names = log.names();
    assert!(names.contains(&"finalized".to_owned()), "{names:?}");
    // A second stop finds nothing to stop: the host's own answer.
    let f = fixture();
    let host = headless(&f, FakeOpener::default());
    let err = stop_recording(&f.socket, STOP_TIMEOUT).unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    request_shutdown(&f.socket).unwrap();
    assert_eq!(host.join().unwrap(), 0);
}

/// D-29: the window polls `hello` until the export is over, then shuts the
/// host down and takes the lock.
#[test]
fn the_export_wait() {
    let f = fixture();
    let host = headless(&f, FakeOpener::default());
    let export = {
        let socket = f.socket.clone();
        thread::spawn(move || {
            send(
                &socket,
                Command::Export {
                    format: ExportFormat::Mp4,
                    quality: Quality::Standard,
                    codec: None,
                    out: Some("ms:1200:export".into()),
                },
            )
        })
    };
    assert!(wait_for(Duration::from_secs(5), || client::hello(
        &f.socket,
        HELLO_TIMEOUT
    )
    .is_ok_and(|h| !h.jobs.is_empty())));
    let (hello, socket) = held_by(&f);
    let job = &hello.jobs[0];
    assert!(matches!(job.kind, JobKind::Export { .. }), "{job:?}");
    let progress = export_progress_view(job, Utc::now()).expect("an export's progress");
    assert!(progress.done <= progress.total);
    // Polled until no job runs (the window: every 500 ms).
    let waited = Instant::now();
    while client::hello(&socket, HELLO_TIMEOUT)
        .unwrap()
        .jobs
        .iter()
        .any(|j| matches!(j.kind, JobKind::Export { .. }))
    {
        assert!(
            waited.elapsed() < Duration::from_secs(10),
            "the export never ended"
        );
        thread::sleep(Duration::from_millis(100));
    }
    request_shutdown(&socket).unwrap();
    let _lock = wait_for_lock(&f.dir, LOCK_TIMEOUT).unwrap();
    assert_eq!(host.join().unwrap(), 0);
    assert_eq!(export.join().unwrap().ok()["exported"], true);
}

/// A holder that takes the connection and never answers `hello`.
#[test]
fn an_unresponsive_holder() {
    let f = fixture();
    let lock = hold_lock(&f.dir);
    let mut record = LockRecord::starting(HostKind::Headless, &f.socket, Utc::now());
    lockfile::set_state(&lock, &mut record, HostState::Ready).unwrap();
    let runtime = f.socket.parent().unwrap().to_path_buf();
    let name = f.socket.file_name().unwrap().to_string_lossy().into_owned();
    let _server = raw_server(&runtime, &name, |_line, stream| {
        thread::sleep(Duration::from_secs(3));
        drop(stream);
    });
    let asked = Instant::now();
    let holder = find_holder(&f.dir, &f.paths_env).unwrap();
    assert!(asked.elapsed() >= HELLO_TIMEOUT);
    match holder {
        Holder::Unresponsive(Some(rec)) => {
            assert_eq!(
                (rec.pid, rec.kind),
                (std::process::id(), HostKind::Headless)
            );
        }
        other => panic!("expected an unresponsive holder, got {other:?}"),
    }
    // Its `shutdown` and `RecordStop` aren't answered either.
    let err = request_shutdown(&f.socket).unwrap_err();
    assert_eq!(err.code, ErrorCode::Busy);
    let err = stop_recording(&f.socket, Duration::from_millis(300)).unwrap_err();
    assert_eq!(err.code, ErrorCode::Busy);
    assert_eq!(err.message, "the recording didn't stop within 0.3 s");
    drop(lock);
}

/// The lock is held, but no host answers: starting (or no Qrec host at
/// all). After 3 s the window says it isn't responding, naming the record.
#[test]
fn a_holder_without_a_socket_is_starting() {
    let f = fixture();
    let lock = hold_lock(&f.dir);
    assert!(matches!(
        find_holder(&f.dir, &f.paths_env).unwrap(),
        Holder::Starting
    ));
    assert!(matches!(unresponsive(&f.dir), Holder::Unresponsive(None)));
    let mut record = LockRecord::starting(HostKind::Cli, &f.socket, Utc::now());
    lockfile::write(&lock, &record).unwrap();
    assert!(matches!(
        find_holder(&f.dir, &f.paths_env).unwrap(),
        Holder::Starting
    ));
    let Holder::Unresponsive(Some(named)) = unresponsive(&f.dir) else {
        panic!("the record names the holder");
    };
    assert_eq!(named.kind, HostKind::Cli);
    // The lock never frees: `wait_for_lock` gives up, naming it.
    record.state = HostState::Stopping;
    lockfile::write(&lock, &record).unwrap();
    let waited = Instant::now();
    let err = wait_for_lock(&f.dir, Duration::from_millis(300)).unwrap_err();
    assert!(waited.elapsed() >= Duration::from_millis(300));
    assert_eq!(err.code, ErrorCode::Busy);
    assert!(err.message.contains("is shutting down"), "{}", err.message);
    drop(lock);
    let _ours = wait_for_lock(&f.dir, Duration::from_millis(300)).unwrap();
}

/// D-13: another window holds the project; it refuses `shutdown`.
#[test]
fn a_gui_holder_refuses_shutdown() {
    let f = fixture();
    let port = Arc::new(FakePort::default());
    let spec = GuiHostSpec {
        dir: f.dir.clone(),
        lock: hold_lock(&f.dir),
        env: f.paths_env.clone(),
        interrupts: None,
        rules: quick_rules(),
        first: None,
    };
    let gui = GuiHost::start(spec, port.clone() as Arc<dyn GuiPort>).unwrap();
    gui.open(Box::new(FakeOpener::default()));
    let (hello, socket) = held_by(&f);
    assert_eq!(hello.host, HostKind::Gui);
    let err = request_shutdown(&socket).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (ErrorCode::Busy, GUI_REFUSES_SHUTDOWN)
    );
    assert!(wait_for_lock(&f.dir, Duration::from_millis(200)).is_err());
    gui.close();
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Closed)
    );
    let _ours = wait_for_lock(&f.dir, LOCK_TIMEOUT).unwrap();
}

/// A host that is gone (or goes as it is asked) needs no shutdown.
#[test]
fn shutting_down_nothing_is_fine() {
    let f = fixture();
    request_shutdown(&f.socket).unwrap();
    // One that closes the connection without answering is going away.
    let runtime = f.socket.parent().unwrap().to_path_buf();
    let name = f.socket.file_name().unwrap().to_string_lossy().into_owned();
    let _server = raw_server(&runtime, &name, |line, stream| {
        let _ = line_id(&line);
        drop(stream);
    });
    request_shutdown(&f.socket).unwrap();
    let err = stop_recording(&f.socket, STOP_TIMEOUT).unwrap_err();
    assert_eq!(err.code, ErrorCode::Io, "{err:?}");
}

/// What a host that isn't Qrec says is an error, never a success.
#[test]
fn a_reply_that_isnt_qrecs_is_an_error() {
    let f = fixture();
    let runtime = f.socket.parent().unwrap().to_path_buf();
    let name = f.socket.file_name().unwrap().to_string_lossy().into_owned();
    let _server = raw_server(&runtime, &name, |_line, stream| {
        send_line(&stream, &serde_json::json!({ "hello": "swamp" }));
    });
    let err = request_shutdown(&f.socket).unwrap_err();
    assert_eq!(err.code, ErrorCode::Io, "{err:?}");
}
