//! GUI host tests (G§3.4.1, WP0a acceptance): the GUI exit rules as a pure
//! function, `GuiHost` on threads of this process with a fake port, a fake
//! Session and shortened rules, and the second signal in a child process
//! (it `_exit`s).

use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use qrec_core::protocol::{
    Command, ExportFormat, Progress, Quality, Query, RecordModeArg, Toast, ToastLevel, UiPatch,
    WireBody,
};
use qrec_core::{CoreError, ErrorCode};
use serde_json::{Value, json};

use super::*;
use crate::host::lockfile::{self, HostState};
use crate::host::{Decision, ExitInputs, decide};
use crate::ipc::client;
use crate::ipc::framing::{LineReader, write_all_before};
use crate::ipc::server::GUI_REFUSES_SHUTDOWN;
use crate::ipc::testing::{
    FakeOpener, FakePort, PROJECT, PortEvent, RULES, STUBBORN, child_command, quick_rules,
    raw_exchange, read_events, request_line, rules_env, wait_for,
};
use crate::testkit::TestEnv;

// ------------------------------------------------------------- exit rules

fn gui_inputs(now: Instant) -> ExitInputs {
    ExitInputs {
        now,
        busy: false,
        open: 0,
        conns_idle_since: Some(now),
        state_quiet_since: Some(now),
        requests: 0,
        shutdown: false,
        signalled_at: None,
        hurried_at: None,
        own_done: None,
        ready_at: now,
        gui: true,
        close_requested: None,
    }
}

#[test]
fn gui_exit_rules() {
    let rules = ExitRules::default();
    let t0 = Instant::now();
    let s = |n: u64| t0 + Duration::from_secs(n);

    // Never unused, never idle: a window holds it, used or not.
    let mut i = gui_inputs(s(3600));
    i.ready_at = s(0);
    assert_eq!(decide(&i, &rules), Decision::Wait(None));
    i.requests = 7;
    i.conns_idle_since = Some(s(1));
    i.state_quiet_since = Some(s(1));
    assert_eq!(decide(&i, &rules), Decision::Wait(None));
    // `shutdown` never reaches the rules (refused), and wouldn't end it.
    i.shutdown = true;
    assert_eq!(decide(&i, &rules), Decision::Wait(None));
    i.shutdown = false;

    // The window closed: at once when quiet …
    i.close_requested = Some(s(3600));
    assert_eq!(decide(&i, &rules), Decision::Exit(HostEnd::Closed));
    // … else once quiet, or 60 s after the close whatever still runs.
    i.busy = true;
    assert_eq!(decide(&i, &rules), Decision::Wait(Some(s(3660))));
    i.busy = false;
    i.open = 1;
    i.conns_idle_since = None;
    i.now = s(3659);
    assert_eq!(decide(&i, &rules), Decision::Wait(Some(s(3660))));
    i.now = s(3660);
    assert_eq!(decide(&i, &rules), Decision::Exit(HostEnd::Closed));

    // Signals as for every host: once quiet, or after 15 s.
    let mut signal = gui_inputs(s(5));
    signal.signalled_at = Some(s(1));
    signal.busy = true;
    assert_eq!(decide(&signal, &rules), Decision::Wait(Some(s(16))));
    signal.now = s(16);
    assert_eq!(
        decide(&signal, &rules),
        Decision::Exit(HostEnd::SignalTimeout)
    );
    signal.busy = false;
    signal.now = s(6);
    assert_eq!(decide(&signal, &rules), Decision::Exit(HostEnd::Signal));
    // A signal wins over a close.
    signal.close_requested = Some(s(2));
    assert_eq!(decide(&signal, &rules), Decision::Exit(HostEnd::Signal));
}

// ------------------------------------------------------------ in-process

struct Fixture {
    env: TestEnv,
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
        env,
        paths_env,
        dir,
        socket,
    }
}

impl Fixture {
    fn lock_held(&self) -> bool {
        ProjectLock::is_held(&self.dir).unwrap()
    }

    fn released(&self) -> bool {
        wait_for(Duration::from_secs(5), || !self.lock_held())
    }

    fn spec(&self, rules: ExitRules) -> GuiHostSpec {
        GuiHostSpec {
            dir: self.dir.clone(),
            lock: ProjectLock::try_acquire(&self.dir)
                .unwrap()
                .expect("the lock is free"),
            env: self.paths_env.clone(),
            interrupts: None,
            rules,
            first: None,
        }
    }

    fn start(&self, spec: GuiHostSpec) -> (GuiHost, Arc<FakePort>) {
        let port = Arc::new(FakePort::default());
        let host = GuiHost::start(spec, Arc::clone(&port) as Arc<dyn GuiPort>).unwrap();
        (host, port)
    }

    fn hello(&self) -> Option<qrec_core::protocol::HelloInfo> {
        client::hello(&self.socket, Duration::from_secs(2)).ok()
    }
}

/// The request bodies a fake Session ran, without nudges.
fn bodies(opener: &FakeOpener) -> Vec<Value> {
    opener
        .log
        .events()
        .into_iter()
        .filter(|e| e["event"] == "request")
        .map(|e| e["body"].clone())
        .collect()
}

fn body(body: WireBody) -> Value {
    serde_json::to_value(body).unwrap()
}

fn export_ms(spec: &str) -> Command {
    Command::Export {
        format: ExportFormat::Mp4,
        quality: Quality::Standard,
        codec: None,
        out: Some(spec.into()),
    }
}

/// A request whose client waits on its own connection: `accepted` came, so
/// the host queued it.
struct Waiting {
    reader: LineReader,
}

impl Waiting {
    fn send(socket: &Path, id: &str, wire: WireBody) -> Waiting {
        let stream = UnixStream::connect(socket).expect("connect");
        let deadline = Instant::now() + Duration::from_secs(10);
        write_all_before(
            &stream,
            format!("{}\n", request_line(id, body(wire))).as_bytes(),
            deadline,
        )
        .unwrap();
        let mut reader = LineReader::new(stream);
        let line = reader.read_line(1 << 20, deadline).expect("accepted");
        let accepted: Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(accepted["accepted"], true, "{accepted}");
        Waiting { reader }
    }

    /// The reply (`{"ok":…}`), after any progress lines.
    fn reply(mut self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let line = self.reader.read_line(64 << 20, deadline).expect("a reply");
            let msg: Value = serde_json::from_slice(&line).unwrap();
            if msg.get("reply").is_some() {
                return msg["reply"].clone();
            }
        }
    }
}

/// Replies the window's own requests got.
#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<Result<Value, CoreError>>>>);

impl Replies {
    fn responder(&self) -> Box<dyn Responder> {
        Box::new(ReplyTo(self.clone()))
    }

    fn all(&self) -> Vec<Result<Value, CoreError>> {
        self.0.lock().unwrap().clone()
    }
}

struct ReplyTo(Replies);

impl Responder for ReplyTo {
    fn accepted(&mut self) {}

    fn progress(&mut self, _p: &Progress) {}

    fn reply(&mut self, r: Result<Value, CoreError>) {
        self.0.0.lock().unwrap().push(r);
    }
}

#[test]
fn a_gui_host_never_exits_after_the_unused_or_idle_timeouts() {
    let f = fixture();
    let rules = ExitRules {
        idle_grace: Duration::from_millis(100),
        unused_timeout: Duration::from_millis(200),
        ..quick_rules()
    };
    let (host, port) = f.start(f.spec(rules));
    host.open(Box::new(FakeOpener::default()));
    assert!(port.wait_host(Duration::from_secs(5), |e| *e == HostEvent::Opened));
    // Unused for longer than the unused timeout …
    thread::sleep(Duration::from_millis(600));
    assert!(!host.is_finished());
    // … then used, and idle for longer than the idle grace.
    let set_ui = Command::SetUi {
        patch: UiPatch {
            bin_open: Some(true),
            ..UiPatch::default()
        },
    };
    let reply = Waiting::send(&f.socket, "u", WireBody::Command(set_ui)).reply();
    assert_eq!(reply["ok"], true, "{reply}");
    thread::sleep(Duration::from_millis(600));
    assert!(!host.is_finished());
    assert_eq!(port.stopped(), None);
    let hello = f.hello().expect("still serving");
    assert_eq!(hello.host, HostKind::Gui);
    assert_eq!(
        lockfile::read(&f.dir).unwrap().unwrap().state,
        HostState::Ready
    );

    host.close();
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Closed)
    );
    assert!(host.is_finished());
    assert!(f.released(), "the lock was released");
    assert!(!f.socket.exists());
}

/// Events in order: `Ready`, the first snapshot, `Opened`, …, `Stopped`.
#[test]
fn events_come_in_order() {
    let f = fixture();
    let (host, port) = f.start(f.spec(quick_rules()));
    host.open(Box::new(FakeOpener::default()));
    assert!(port.wait_host(Duration::from_secs(5), |e| *e == HostEvent::Opened));
    host.close();
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Closed)
    );
    assert_eq!(
        port.events(),
        [
            PortEvent::Host(HostEvent::Ready),
            PortEvent::Snapshot(0),
            PortEvent::Host(HostEvent::Opened),
            PortEvent::Host(HostEvent::Stopped {
                end: HostEnd::Closed
            }),
        ]
    );
    // Only the first `open` counts.
    host.open(Box::new(FakeOpener::default()));
    assert_eq!(port.host_events().len(), 3);
}

/// D-35: `first` runs ahead of a request that came while the window opened.
#[test]
fn first_runs_before_requests_queued_during_startup() {
    let f = fixture();
    let mut spec = f.spec(quick_rules());
    let init = Command::Init {
        canvas: None,
        name: None,
    };
    spec.first = Some(Request::command(init.clone(), false));
    let (host, port) = f.start(spec);
    // Queued while the Session doesn't exist yet.
    let early = Waiting::send(&f.socket, "early", WireBody::Query(Query::ClipList));
    let opener = FakeOpener::default();
    host.open(Box::new(opener.clone()));
    assert_eq!(early.reply()["ok"], true);
    assert_eq!(
        bodies(&opener),
        [
            body(WireBody::Command(init)),
            body(WireBody::Query(Query::ClipList)),
        ]
    );
    // The fake project already exists: `Init` answers "already a project",
    // which is no news for the window.
    assert!(
        !port
            .events()
            .iter()
            .any(|e| matches!(e, PortEvent::Toast(..))),
        "{:?}",
        port.events()
    );
    drop(host);
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Closed)
    );
}

/// The window's requests and the CLI's go through one FIFO.
#[test]
fn cli_requests_and_submit_share_one_fifo() {
    let f = fixture();
    let (host, port) = f.start(f.spec(quick_rules()));
    let replies = Replies::default();
    assert!(host.submit(Request::query(Query::ClipList), replies.responder()));
    let cli = Waiting::send(
        &f.socket,
        "cli",
        WireBody::Query(Query::TakeList { unused_only: true }),
    );
    assert!(host.submit(
        Request::query(Query::History { limit: Some(3) }),
        replies.responder()
    ));
    let opener = FakeOpener::default();
    host.open(Box::new(opener.clone()));
    assert_eq!(cli.reply()["ok"], true);
    assert!(wait_for(Duration::from_secs(5), || replies.all().len() == 2));
    // Once attached too.
    assert!(host.submit(Request::query(Query::Keys), replies.responder()));
    let cli = Waiting::send(&f.socket, "cli2", WireBody::Query(Query::ClipList));
    assert_eq!(cli.reply()["ok"], true);
    assert!(wait_for(Duration::from_secs(5), || replies.all().len() == 3));
    assert_eq!(
        bodies(&opener),
        [
            body(WireBody::Query(Query::ClipList)),
            body(WireBody::Query(Query::TakeList { unused_only: true })),
            body(WireBody::Query(Query::History { limit: Some(3) })),
            body(WireBody::Query(Query::Keys)),
            body(WireBody::Query(Query::ClipList)),
        ]
    );
    assert!(replies.all().iter().all(Result::is_ok));
    // `retry_live` takes the same queue (D-50).
    host.retry_live();
    assert!(wait_for(Duration::from_secs(5), || opener
        .log
        .names()
        .contains(&"retryLive".to_owned())));
    host.close();
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Closed)
    );
    // A stopped host answers the window's requests with why.
    let late = Replies::default();
    assert!(!host.submit(Request::query(Query::ClipList), late.responder()));
    assert!(late.all()[0].is_err());
}

#[test]
fn fail_answers_the_queue_and_frees_the_lock() {
    let f = fixture();
    let (host, port) = f.start(f.spec(quick_rules()));
    let parked = Waiting::send(&f.socket, "p", WireBody::Query(Query::ClipList));
    let replies = Replies::default();
    assert!(host.submit(
        Request::query(Query::TakeList { unused_only: false }),
        replies.responder()
    ));
    let missing = CoreError::new(ErrorCode::MissingDependency, "GStreamer didn't start")
        .with_hint("see the log");
    host.fail(missing.clone());
    let reply = parked.reply();
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"]["code"], "missing_dependency", "{reply}");
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Failed)
    );
    assert!(host.is_finished());
    assert_eq!(replies.all(), [Err(missing.clone())]);
    assert_eq!(
        port.host_events(),
        [
            HostEvent::Ready,
            HostEvent::Failed(missing.clone()),
            HostEvent::Stopped {
                end: HostEnd::Failed
            },
        ]
    );
    // The lock is free at once: the CLI works in-process now.
    assert!(f.released(), "the lock was released");
    assert!(!f.socket.exists());
    let late = Replies::default();
    assert!(!host.submit(Request::query(Query::ClipList), late.responder()));
    assert_eq!(late.all(), [Err(missing)]);
    // Nothing more happens after that.
    host.open(Box::new(FakeOpener::default()));
    host.close();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(port.host_events().len(), 3);
}

/// X3: a Session that can't open answers the queue with its error and
/// frees the lock (so `qrec history restore` can run in-process).
#[test]
fn a_failed_open_frees_the_lock() {
    let f = fixture();
    let (host, port) = f.start(f.spec(quick_rules()));
    let parked = Waiting::send(&f.socket, "p", WireBody::Query(Query::ClipList));
    let broken = CoreError::invalid("qrec.json is not valid JSON")
        .with_hint("rebuild it from the history with `qrec history restore 1 --yes`");
    let opener = FakeOpener {
        fail: Some(broken.clone()),
        ..FakeOpener::default()
    };
    host.open(Box::new(opener.clone()));
    let reply = parked.reply();
    assert_eq!(reply["error"]["message"], "qrec.json is not valid JSON");
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Failed)
    );
    assert_eq!(
        port.host_events(),
        [
            HostEvent::Ready,
            HostEvent::OpenFailed(broken),
            HostEvent::Stopped {
                end: HostEnd::Failed
            },
        ]
    );
    assert!(f.released(), "the lock was released");
    assert!(!f.socket.exists());
    let opened = opener.opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    assert!(opened[0].mutating && opened[0].host == HostKind::Gui && !opened[0].recovery);
}

/// D-12: the window sends what ends the job, then closes; the host stops
/// accepting at once and exits once the job is over.
#[test]
fn close_stops_accepting_and_exits_once_quiet() {
    let f = fixture();
    let rules = ExitRules {
        close_grace: Duration::from_secs(30),
        ..quick_rules()
    };
    let (host, port) = f.start(f.spec(rules));
    let opener = FakeOpener::default();
    host.open(Box::new(opener.clone()));
    let job = Waiting::send(&f.socket, "job", WireBody::Command(export_ms("ms:900")));
    assert!(wait_for(Duration::from_secs(5), || opener
        .log
        .names()
        .contains(&"job".to_owned())));
    let closed = Instant::now();
    host.close();
    // Nobody new gets in, and routing clients hear it is stopping.
    assert!(wait_for(Duration::from_secs(2), || !f.socket.exists()));
    assert!(f.hello().is_none());
    assert!(wait_for(Duration::from_secs(2), || lockfile::read(&f.dir)
        .ok()
        .flatten()
        .is_some_and(|r| r.state == HostState::Stopping)));
    assert!(!host.is_finished(), "the job still runs");
    // The job ends, its client gets the reply, the host goes.
    assert_eq!(job.reply()["data"]["exported"], true);
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Closed)
    );
    assert!(closed.elapsed() < Duration::from_secs(5));
    assert!(f.released(), "the lock was released");
}

/// A job that doesn't end: the host goes when the close grace runs out.
#[test]
fn close_gives_up_after_the_grace() {
    let f = fixture();
    let rules = ExitRules {
        close_grace: Duration::from_millis(600),
        flush_timeout: Duration::from_millis(500),
        ..quick_rules()
    };
    let (host, port) = f.start(f.spec(rules));
    let opener = FakeOpener {
        stubborn: true,
        ..FakeOpener::default()
    };
    host.open(Box::new(opener.clone()));
    let job = Waiting::send(&f.socket, "job", WireBody::Command(export_ms("ms:30000")));
    assert!(wait_for(Duration::from_secs(5), || opener
        .log
        .names()
        .contains(&"job".to_owned())));
    let closed = Instant::now();
    host.close();
    assert_eq!(
        port.wait_stopped(Duration::from_secs(10)),
        Some(HostEnd::Closed)
    );
    let took = closed.elapsed();
    assert!(took >= Duration::from_millis(600), "{took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert_eq!(job.reply()["ok"], false, "its client hears it ended");
    assert!(f.released(), "the lock was released");
}

#[test]
fn close_before_open_stops_at_once() {
    let f = fixture();
    let (host, port) = f.start(f.spec(quick_rules()));
    let parked = Waiting::send(&f.socket, "p", WireBody::Query(Query::ClipList));
    host.close();
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Closed)
    );
    assert_eq!(parked.reply()["ok"], false);
    assert!(f.released(), "the lock was released");
    assert!(!f.socket.exists());
}

#[test]
fn a_signal_finalizes_the_recording_and_stops_the_host() {
    let f = fixture();
    let interrupts = Interrupts::new();
    let mut spec = f.spec(quick_rules());
    spec.interrupts = Some(interrupts.clone());
    let (host, port) = f.start(spec);
    let opener = FakeOpener::default();
    host.open(Box::new(opener.clone()));
    let start = Command::RecordStart {
        mode: RecordModeArg::Append,
        countdown: None,
        wait: false,
    };
    let reply = Waiting::send(&f.socket, "r", WireBody::Command(start)).reply();
    assert_eq!(reply["data"]["recording"], true, "{reply}");
    interrupts.raise();
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Signal)
    );
    assert!(opener.log.names().contains(&"finalized".to_owned()));
    assert!(f.released(), "the lock was released");
    assert!(!f.socket.exists());
}

/// From startup step 4 on, the host handles signals, also before the
/// Session is open (G§3.3): it stops, and what was queued hears why.
#[test]
fn a_signal_before_open_stops_the_host() {
    let f = fixture();
    let interrupts = Interrupts::new();
    let mut spec = f.spec(quick_rules());
    spec.interrupts = Some(interrupts.clone());
    let (host, port) = f.start(spec);
    let parked = Waiting::send(&f.socket, "p", WireBody::Query(Query::ClipList));
    interrupts.raise();
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Signal)
    );
    assert_eq!(parked.reply()["ok"], false);
    assert!(host.is_finished());
    assert!(f.released(), "the lock was released");
    // A signal counted before the host started is its first signal too.
    let f = fixture();
    let interrupts = Interrupts::new();
    interrupts.raise();
    let mut spec = f.spec(quick_rules());
    spec.interrupts = Some(interrupts);
    let (_host, port) = f.start(spec);
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Signal)
    );
    assert!(f.released(), "the lock was released");
}

/// D-13: a GUI host refuses the wire `shutdown` and keeps serving.
#[test]
fn shutdown_is_refused() {
    let f = fixture();
    let (host, port) = f.start(f.spec(quick_rules()));
    let refuse = || {
        let lines = raw_exchange(&f.socket, &request_line("s", json!("shutdown")));
        assert_eq!(lines[1]["reply"]["error"]["code"], "busy", "{lines:?}");
        assert_eq!(lines[1]["reply"]["error"]["message"], GUI_REFUSES_SHUTDOWN);
    };
    refuse();
    host.open(Box::new(FakeOpener::default()));
    assert!(port.wait_host(Duration::from_secs(5), |e| *e == HostEvent::Opened));
    refuse();
    thread::sleep(Duration::from_millis(300));
    assert!(!host.is_finished());
    assert!(f.hello().is_some());
    host.close();
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Closed)
    );
}

/// The folder rule again under the lock: a folder that stopped being empty
/// is refused, and the lock goes with the error.
#[test]
fn a_folder_that_is_no_project_is_refused() {
    let f = fixture();
    let spec = f.spec(quick_rules());
    fs::write(f.dir.join("notes.txt"), "not a swamp").unwrap();
    let port = Arc::new(FakePort::default());
    let err = GuiHost::start(spec, port.clone() as Arc<dyn GuiPort>).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotProject);
    assert!(f.released(), "the lock was released");
    assert!(port.events().is_empty());
}

/// A Session whose loop panics.
struct Panicking(Box<dyn HostSession>);

impl HostSession for Panicking {
    fn inbox(&self) -> crate::msg::Inbox {
        self.0.inbox()
    }

    fn run_until(&mut self, _done: &mut dyn FnMut(&State) -> bool) {
        panic!("the swamp flooded");
    }

    fn state(&self) -> &State {
        self.0.state()
    }
}

struct PanickingOpener;

impl Opener for PanickingOpener {
    fn open(
        &mut self,
        dir: &Path,
        opts: OpenOpts,
        lock: Option<crate::host::SessionLock>,
    ) -> Result<Box<dyn HostSession>, crate::error::ShellError> {
        let inner = FakeOpener::default().open(dir, opts, lock)?;
        Ok(Box::new(Panicking(inner)))
    }
}

#[test]
fn a_panic_on_the_session_thread_is_a_crash() {
    let f = fixture();
    let (host, port) = f.start(f.spec(quick_rules()));
    host.open(Box::new(PanickingOpener));
    assert_eq!(
        port.wait_stopped(Duration::from_secs(5)),
        Some(HostEnd::Failed)
    );
    let events = port.host_events();
    assert!(
        events.contains(&HostEvent::Crashed("the swamp flooded".into())),
        "{events:?}"
    );
    assert!(f.released(), "the lock was released");
    let late = Replies::default();
    assert!(!host.submit(Request::query(Query::ClipList), late.responder()));
    assert_eq!(
        late.all()[0].as_ref().unwrap_err().code,
        ErrorCode::Internal
    );
}

/// An error of the `first` request is toasted while the host runs: the
/// window would otherwise show an empty swamp without saying why.
#[test]
fn first_request_errors_are_toasted_while_the_host_runs() {
    let port = Arc::new(FakePort::default());
    let stopping = Arc::new(AtomicBool::new(false));
    let mut first = FirstResponder {
        port: port.clone(),
        stopping: Arc::clone(&stopping),
    };
    first.reply(Err(
        CoreError::io("can't write qrec.json: no space left").with_hint("free some space")
    ));
    first.reply(Err(CoreError::invalid("already a project")));
    first.reply(Ok(json!({})));
    stopping.store(true, Ordering::SeqCst);
    first.reply(Err(CoreError::busy("this Qrec host is stopping")));
    let toast = Toast::new(ToastLevel::Error, "can't write qrec.json: no space left")
        .with_body("free some space");
    assert_eq!(port.events(), [PortEvent::Toast(toast, false)]);
}

#[test]
fn port_adapters_pass_states_and_toasts_on() {
    let port = Arc::new(FakePort::default());
    let mut observer = PortObserver(port.clone());
    let state = State::new(
        qrec_core::Project::new(
            "swamp",
            chrono::Utc::now(),
            &qrec_core::model::ProjectDefaults::default(),
        ),
        qrec_core::state::History::new(),
        qrec_core::state::UiState::default(),
        HostKind::Gui,
    );
    observer.changed(7, &state);
    let mut notifier = PortNotifier(port.clone());
    let toast = Toast::new(ToastLevel::Info, "Take 7 recovered");
    notifier.notify(&toast);
    notifier.notify_recovery(&toast);
    assert_eq!(
        port.events(),
        [
            PortEvent::Snapshot(7),
            PortEvent::Toast(toast.clone(), false),
            PortEvent::Toast(toast, true),
        ]
    );
}

// ------------------------------------------------------- a real process

fn wait_exit(child: &mut Child, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        thread::sleep(Duration::from_millis(20));
    }
    None
}

fn alive(pid: u32) -> bool {
    // SAFETY: kill(2) with signal 0 only checks for existence.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Kills the child when a test ends early.
struct Reap(Child);

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The first signal waits for a stubborn job; the second kills the child
/// process groups, unlinks the socket and `_exit(130)`s (A§8.6).
#[test]
fn a_second_signal_hard_exits_the_gui_host() {
    let f = fixture();
    let log = f.env.root().join("gui-second.jsonl");
    let child = child_command(&f.env, "gui", &log)
        .env(PROJECT, &f.dir)
        .env(
            RULES,
            rules_env(&ExitRules {
                signal_grace: Duration::from_secs(30),
                ..quick_rules()
            }),
        )
        .env(STUBBORN, "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut reap = Reap(child);
    assert!(
        wait_for(Duration::from_secs(10), || f
            .hello()
            .is_some_and(|h| h.pid == pid && h.host == HostKind::Gui)),
        "{:#?}",
        read_events(&log)
    );
    // A job that ignores cancellation and runs `sleep 60` in its own group;
    // its client stays connected.
    let _job = Waiting::send(
        &f.socket,
        "job",
        WireBody::Command(export_ms("ms:60000:sleep")),
    );
    let child_pid = || {
        read_events(&log)
            .iter()
            .find(|e| e["event"] == "child")
            .and_then(|e| e["child"].as_u64())
    };
    assert!(wait_for(Duration::from_secs(10), || child_pid().is_some()));
    let sleep_pid = child_pid().unwrap() as u32;
    // SAFETY: kill(2) on our child.
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    assert!(wait_for(Duration::from_secs(5), || read_events(&log)
        .iter()
        .any(|e| e["event"] == "interrupt")));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(reap.0.try_wait().unwrap(), None, "busy: the first waits");
    let second = Instant::now();
    // SAFETY: kill(2) on our child.
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    assert_eq!(wait_exit(&mut reap.0, Duration::from_secs(5)), Some(130));
    assert!(second.elapsed() < Duration::from_secs(3));
    assert!(
        wait_for(Duration::from_secs(5), || !alive(sleep_pid)),
        "the child group was killed"
    );
    assert!(f.released(), "the lock died with the process");
    assert!(!f.socket.exists(), "the socket was unlinked first");
}

/// The first signal in a real process: the host ends once quiet and the
/// window's process exits 130 (the GUI maps `Stopped { Signal }`).
#[test]
fn a_signal_ends_a_gui_host_process_with_130() {
    let f = fixture();
    let log = f.env.root().join("gui-first.jsonl");
    let child = child_command(&f.env, "gui", &log)
        .env(PROJECT, &f.dir)
        .env(RULES, rules_env(&quick_rules()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut reap = Reap(child);
    assert!(wait_for(Duration::from_secs(10), || f
        .hello()
        .is_some_and(|h| h.pid == pid)));
    // SAFETY: kill(2) on our child.
    unsafe { libc::kill(pid as i32, libc::SIGINT) };
    assert_eq!(wait_exit(&mut reap.0, Duration::from_secs(10)), Some(130));
    let events = read_events(&log);
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "hostEvent" && e["detail"] == "Stopped { end: Signal }"),
        "{events:#?}"
    );
    assert!(f.released());
}
