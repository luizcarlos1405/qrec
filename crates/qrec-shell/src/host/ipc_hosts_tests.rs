//! Host tests (owner: ipc): the exit rules as a pure function, hosts on
//! threads of this process, and real processes — re-executed copies of this
//! test binary playing a CLI, a headless host or a spawner
//! (`ipc::testing::child_entry`).

use std::fs;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use qrec_core::protocol::{ExportFormat, Quality, RecordModeArg, UiPatch, WireBody};
use qrec_core::{ErrorCode, Query};
use serde_json::{Value, json};

use super::*;
use crate::host::lockfile::{self, HostState};
use crate::ipc::client::{self, SendOpts, Sent, send_request};
use crate::ipc::testing::{
    Collect, FakeLog, FakeOpener, PROJECT, REQUEST, RULES, STUBBORN, child_command, hold_lock,
    quick_rules, raw_exchange, read_events, request_line, rules_env, wait_for,
};
use crate::lock::ProjectLock;
use crate::paths::{Env, ensure_runtime_dir, socket_path};
use crate::testkit::TestEnv;
use signals::Interrupts;

// ------------------------------------------------------------- exit rules

fn inputs(now: Instant) -> ExitInputs {
    ExitInputs {
        now,
        busy: false,
        open: 0,
        conns_idle_since: Some(now),
        state_quiet_since: Some(now),
        requests: 1,
        shutdown: false,
        signalled_at: None,
        hurried_at: None,
        own_done: None,
        ready_at: now,
        gui: false,
        close_requested: None,
    }
}

#[test]
fn exit_rules() {
    let rules = ExitRules::default();
    let t0 = Instant::now();
    let s = |n: u64| t0 + Duration::from_secs(n);

    // Headless: idle for 2 s after the last connection closed.
    let mut i = inputs(s(10));
    i.conns_idle_since = Some(s(9));
    i.state_quiet_since = Some(s(5));
    assert_eq!(decide(&i, &rules), Decision::Wait(Some(s(11))));
    i.now = s(11);
    assert_eq!(decide(&i, &rules), Decision::Exit(HostEnd::Idle));
    // …counted from whichever went quiet last.
    i.state_quiet_since = Some(s(10));
    assert_eq!(decide(&i, &rules), Decision::Wait(Some(s(12))));
    // Recording or jobs, or an open connection: keep running.
    let mut busy = inputs(s(100));
    busy.busy = true;
    assert_eq!(decide(&busy, &rules), Decision::Wait(None));
    let mut open = inputs(s(100));
    open.open = 1;
    open.conns_idle_since = None;
    assert_eq!(decide(&open, &rules), Decision::Wait(None));

    // Headless without a request: 60 s after ready.
    let mut unused = inputs(s(30));
    unused.requests = 0;
    unused.ready_at = s(0);
    assert_eq!(decide(&unused, &rules), Decision::Wait(Some(s(60))));
    unused.now = s(60);
    assert_eq!(decide(&unused, &rules), Decision::Exit(HostEnd::Unused));
    unused.open = 1;
    assert_eq!(decide(&unused, &rules), Decision::Wait(None));

    // shutdown: once quiet, however long that takes.
    let mut shutdown = inputs(s(0));
    shutdown.shutdown = true;
    shutdown.busy = true;
    assert_eq!(decide(&shutdown, &rules), Decision::Wait(None));
    shutdown.busy = false;
    assert_eq!(decide(&shutdown, &rules), Decision::Exit(HostEnd::Shutdown));

    // A signal: once quiet, or 15 s later regardless.
    let mut signal = inputs(s(5));
    signal.signalled_at = Some(s(1));
    signal.busy = true;
    assert_eq!(decide(&signal, &rules), Decision::Wait(Some(s(16))));
    signal.now = s(16);
    assert_eq!(
        decide(&signal, &rules),
        Decision::Exit(HostEnd::SignalTimeout)
    );
    signal.now = s(5);
    signal.busy = false;
    assert_eq!(decide(&signal, &rules), Decision::Exit(HostEnd::Signal));
    // A second signal hurries an in-process command: 2 s, not 15.
    signal.busy = true;
    signal.now = s(4);
    signal.hurried_at = Some(s(3));
    assert_eq!(decide(&signal, &rules), Decision::Wait(Some(s(5))));
    signal.now = s(5);
    assert_eq!(
        decide(&signal, &rules),
        Decision::Exit(HostEnd::SignalTimeout)
    );
    signal.busy = false;
    assert_eq!(decide(&signal, &rules), Decision::Exit(HostEnd::Signal));

    // In-process CLI host: its own command first, then it exits as soon as
    // nothing keeps it, whoever else used it (A§8.5): no idle grace.
    let mut cli = inputs(s(0));
    cli.requests = 0;
    cli.own_done = Some(false);
    assert_eq!(decide(&cli, &rules), Decision::Wait(None));
    cli.own_done = Some(true);
    assert_eq!(decide(&cli, &rules), Decision::Exit(HostEnd::Idle));
    cli.requests = 5;
    assert_eq!(decide(&cli, &rules), Decision::Exit(HostEnd::Idle));
    // What it already runs for others (a job, an open request) keeps it.
    cli.busy = true;
    assert_eq!(decide(&cli, &rules), Decision::Wait(None));
    cli.busy = false;
    cli.open = 1;
    cli.conns_idle_since = None;
    assert_eq!(decide(&cli, &rules), Decision::Wait(None));
}

// ------------------------------------------------------- in-process hosts

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
    fn paths(&self) -> ProjectPaths {
        ProjectPaths::new(&self.dir)
    }

    fn lock_held(&self) -> bool {
        ProjectLock::is_held(&self.dir).unwrap()
    }

    fn record(&self) -> lockfile::LockRecord {
        lockfile::read(&self.dir).unwrap().expect("a lock record")
    }

    /// The record's state; `None` while it is caught mid-write (it is
    /// rewritten in place, so a reader can see half of it).
    fn record_state(&self) -> Option<HostState> {
        lockfile::read(&self.dir).ok().flatten().map(|r| r.state)
    }
}

fn command(cmd: qrec_core::Command) -> CliRequest {
    CliRequest {
        request: Request::command(cmd, false),
        recovery: false,
    }
}

fn query(q: Query) -> CliRequest {
    CliRequest {
        request: Request::query(q),
        recovery: false,
    }
}

fn export(ms: u64) -> qrec_core::Command {
    export_spec(&format!("ms:{ms}"))
}

fn export_spec(spec: &str) -> qrec_core::Command {
    qrec_core::Command::Export {
        format: ExportFormat::Mp4,
        quality: Quality::Standard,
        codec: None,
        out: Some(spec.into()),
    }
}

fn set_ui() -> qrec_core::Command {
    qrec_core::Command::SetUi {
        patch: UiPatch {
            bin_open: Some(true),
            ..UiPatch::default()
        },
    }
}

/// Runs `run_cli` like the CLI would, with a fake Session.
fn cli(
    f: &Fixture,
    opener: &mut FakeOpener,
    interrupts: &Interrupts,
    req: CliRequest,
    rules: ExitRules,
    on_second: Option<Box<dyn FnOnce() + Send>>,
) -> (CliExit, Collect, Vec<String>) {
    let program = HostProgram::current_exe().unwrap();
    let mut notices = Vec::new();
    let mut out = Collect::default();
    let exit = run_cli(
        CliContext {
            env: &f.paths_env,
            cwd: &f.dir,
            project: None,
            opener,
            host_program: &program,
            interrupts,
            notice: &mut |n: &str| notices.push(n.to_owned()),
            rules,
            on_second_signal: on_second,
        },
        req,
        &mut out,
    );
    (exit, out, notices)
}

#[test]
fn a_short_command_without_a_host_runs_in_process_and_exits_at_once() {
    let f = fixture();
    let mut opener = FakeOpener::default();
    let started = Instant::now();
    let (exit, out, notices) = cli(
        &f,
        &mut opener,
        &Interrupts::new(),
        command(set_ui()),
        quick_rules(),
        None,
    );
    assert_eq!(exit, CliExit::Replied);
    // The real core ran the command (SetUi is a timeline-owner command).
    assert_eq!(out.ok()["handledBy"], std::process::id());
    assert!(notices.is_empty());
    assert!(started.elapsed() < quick_rules().idle_grace + Duration::from_millis(200));
    let opened = opener.opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    assert!(opened[0].mutating && opened[0].host == HostKind::Cli);
    // The host is gone: lock released, socket unlinked, record says so.
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
    assert!(!f.socket.exists());
    assert_eq!(f.record().state, HostState::Stopping);
}

#[test]
fn a_query_without_a_host_takes_no_lock_and_writes_nothing() {
    let f = fixture();
    let mut opener = FakeOpener::default();
    let (exit, out, _) = cli(
        &f,
        &mut opener,
        &Interrupts::new(),
        query(Query::ClipList),
        quick_rules(),
        None,
    );
    assert_eq!(exit, CliExit::Replied);
    assert_eq!(out.ok()["query"], "ClipList");
    let opened = opener.opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    assert!(!opened[0].mutating);
    assert_eq!(
        fs::read_dir(&f.dir).unwrap().count(),
        0,
        "still strictly empty"
    );
}

#[test]
fn gui_only_commands_need_a_running_gui() {
    let f = fixture();
    let mut opener = FakeOpener::default();
    let (exit, out, _) = cli(
        &f,
        &mut opener,
        &Interrupts::new(),
        command(qrec_core::Command::Play),
        quick_rules(),
        None,
    );
    assert_eq!(exit, CliExit::Replied);
    assert_eq!(out.err().code, ErrorCode::Unavailable);
    let over_selection = qrec_core::Command::RecordStart {
        mode: RecordModeArg::OverSelection,
        countdown: None,
        wait: false,
    };
    let (_, out, _) = cli(
        &f,
        &mut opener,
        &Interrupts::new(),
        command(over_selection),
        quick_rules(),
        None,
    );
    assert_eq!(out.err().code, ErrorCode::Unavailable);
    assert!(opener.opened.lock().unwrap().is_empty());
    assert!(!f.dir.join(".qrec").exists());
}

#[test]
fn a_failed_open_is_the_reply_and_frees_the_project() {
    let f = fixture();
    let mut opener = FakeOpener {
        fail: Some(qrec_core::CoreError::invalid(
            "qrec.json on disk is invalid",
        )),
        ..FakeOpener::default()
    };
    let (exit, out, _) = cli(
        &f,
        &mut opener,
        &Interrupts::new(),
        command(set_ui()),
        quick_rules(),
        None,
    );
    assert_eq!(exit, CliExit::Replied);
    assert_eq!(out.err().code, ErrorCode::Invalid);
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
    assert!(!f.socket.exists());
}

#[test]
fn an_in_process_host_serves_other_commands_while_its_job_runs() {
    let f = std::sync::Arc::new(fixture());
    let opener = FakeOpener::default();
    let log = opener.log.clone();
    // A long idle grace: a host that waited for it would be seen.
    let rules = ExitRules {
        idle_grace: Duration::from_secs(5),
        ..quick_rules()
    };
    let host = {
        let f = std::sync::Arc::clone(&f);
        let mut opener = opener.clone();
        thread::spawn(move || {
            let started = Instant::now();
            let (exit, out, _) = cli(
                &f,
                &mut opener,
                &Interrupts::new(),
                command(export(800)),
                rules,
                None,
            );
            (exit, out, started.elapsed())
        })
    };
    assert!(wait_for(Duration::from_secs(5), || client::hello(
        &f.socket,
        Duration::from_secs(1)
    )
    .is_ok()));
    // A second CLI routes to it instead of opening the project itself.
    let mut second = FakeOpener::default();
    let (exit, out, _) = cli(
        &f,
        &mut second,
        &Interrupts::new(),
        command(set_ui()),
        quick_rules(),
        None,
    );
    assert_eq!(exit, CliExit::Replied);
    assert_eq!(out.ok()["handledBy"], std::process::id());
    assert!(
        second.opened.lock().unwrap().is_empty(),
        "routed, not opened"
    );
    let (exit, out, took) = host.join().unwrap();
    assert_eq!(exit, CliExit::Replied);
    assert_eq!(out.ok()["exported"], true);
    assert_eq!(out.progress, [1, 2, 3, 4, 5]);
    // Someone else used it, yet once its own 800 ms job was answered it
    // exited at once: no idle grace for a command's own host (A§8.5).
    assert!(took >= Duration::from_millis(800), "{took:?}");
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
    let names = log.names();
    assert_eq!(
        names.iter().filter(|n| *n == "open").count(),
        1,
        "{names:?}"
    );
    assert!(names.contains(&"jobDone".to_owned()), "{names:?}");
}

/// Request events shared with the test thread: `(event, when)`.
#[derive(Clone, Default)]
struct SharedEvents(std::sync::Arc<std::sync::Mutex<Vec<(String, Instant)>>>);

impl SharedEvents {
    fn when(&self, event: &str) -> Option<Instant> {
        let events = self.0.lock().unwrap();
        events.iter().find(|(e, _)| e == event).map(|(_, at)| *at)
    }

    fn push(&self, event: &str) {
        self.0
            .lock()
            .unwrap()
            .push((event.to_owned(), Instant::now()));
    }
}

impl RequestEvents for SharedEvents {
    fn routed(&mut self, _host: Option<&qrec_core::protocol::HelloInfo>) {}

    fn accepted(&mut self) {}

    fn progress(&mut self, _p: &qrec_core::protocol::Progress) {}

    fn replied(&mut self, reply: &Result<Value, qrec_core::CoreError>) {
        assert!(reply.is_ok(), "{reply:?}");
        self.push("replied");
    }
}

/// A finished in-process command shows its result at once, even while its
/// process still runs another client's job, and from then on takes no new
/// job: that one bounces and finds its own host later (A§8.5). Its result
/// used to wait until the host was done with everyone (forever, for a
/// recording someone started meanwhile).
#[test]
fn an_in_process_command_answers_at_once_and_takes_no_new_job() {
    let f = std::sync::Arc::new(fixture());
    let opener = FakeOpener::default();
    let log = opener.log.clone();
    let events = SharedEvents::default();
    let host = {
        let (f, events) = (std::sync::Arc::clone(&f), events.clone());
        let mut opener = opener.clone();
        thread::spawn(move || {
            let mut events = events;
            let req = Request::command(export(1500), false);
            let finish = backend(&f, &mut opener, &req, &mut events);
            events.push("returned");
            finish
        })
    };
    assert!(wait_for(Duration::from_secs(5), || client::hello(
        &f.socket,
        Duration::from_secs(1)
    )
    .is_ok_and(|h| h.host == HostKind::Cli)));
    // Another client's long job, taken while the host's own command runs.
    let other = {
        let socket = f.socket.clone();
        thread::spawn(move || send(&socket, Request::command(export(3500), false)))
    };
    assert!(
        wait_for(Duration::from_secs(5), || events.when("replied").is_some()),
        "the own reply was shown while the host still served the other job"
    );
    assert!(events.when("returned").is_none());
    // Done: a new job bounces, before `accepted` (it never runs here) …
    let mut out = Collect::default();
    let bounced = send_request(
        &f.socket,
        &Request::command(export(100), false),
        &mut out,
        &SendOpts::new(&Interrupts::new()),
    );
    assert_eq!(bounced, Err(client::SendError::NotAccepted));
    assert_eq!(out.accepted, 0);
    // … while quick requests are still answered.
    assert_eq!(
        send(&f.socket, Request::command(set_ui(), false))
            .replies
            .len(),
        1
    );
    assert_eq!(other.join().unwrap().ok()["exported"], true);
    let finish = host.join().unwrap();
    assert!(
        matches!(&finish, Finish::Reply(Ok(v)) if v["exported"] == true),
        "{finish:?}"
    );
    let (replied, returned) = (
        events.when("replied").unwrap(),
        events.when("returned").unwrap(),
    );
    assert!(
        returned.duration_since(replied) >= Duration::from_millis(1000),
        "shown well before the host finished the other job"
    );
    let jobs = log.names().iter().filter(|n| *n == "job").count();
    assert_eq!(
        jobs,
        2,
        "its own export and the other one: {:?}",
        log.names()
    );
}

#[test]
fn ctrl_c_in_an_in_process_host_cancels_its_job() {
    let f = fixture();
    let mut opener = FakeOpener::default();
    let interrupts = Interrupts::new();
    let raiser = interrupts.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        raiser.raise();
    });
    let started = Instant::now();
    let (exit, out, _) = cli(
        &f,
        &mut opener,
        &interrupts,
        command(export(20_000)),
        quick_rules(),
        None,
    );
    assert_eq!(exit, CliExit::Interrupted);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(out.err().message, "interrupted");
    let names = opener.log.names();
    assert!(names.contains(&"interrupt".to_owned()), "{names:?}");
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
}

/// The commands a fake Session ran (requests other than the host's own
/// `RecordStop` on a signal), by `cmd`.
fn commands_run(log: &FakeLog) -> Vec<String> {
    log.events()
        .iter()
        .filter(|e| e["event"] == "request")
        .filter_map(|e| e["body"]["command"]["cmd"].as_str().map(str::to_owned))
        .filter(|cmd| cmd != "recordStop")
        .collect()
}

/// A§8.6: Ctrl+C while the in-process host's Session opens (it waited for
/// a slow recovery before) means the command never started, so it never
/// runs; the host winds down and the run is interrupted. It used to be sent
/// after the open anyway (an interrupted `take purge --yes` deleted the
/// take).
#[test]
fn a_command_interrupted_while_its_host_opens_never_runs() {
    let f = fixture();
    let mut opener = FakeOpener {
        delay: Duration::from_millis(800),
        ..FakeOpener::default()
    };
    let interrupts = Interrupts::new();
    let raiser = interrupts.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        raiser.raise();
    });
    let started = Instant::now();
    let (exit, out, _) = cli(
        &f,
        &mut opener,
        &interrupts,
        command(set_ui()),
        quick_rules(),
        None,
    );
    assert_eq!(exit, CliExit::Interrupted);
    assert!(out.replies.is_empty(), "{:?}", out.replies);
    // Only the host's own `RecordStop` reached the Session (quiet right
    // after it, the host may exit before the queued `Interrupt`).
    assert!(
        commands_run(&opener.log).is_empty(),
        "{:?}",
        opener.log.events()
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
}

#[test]
fn a_second_ctrl_c_runs_the_hard_exit() {
    let f = fixture();
    let mut opener = FakeOpener {
        stubborn: true,
        ..FakeOpener::default()
    };
    let interrupts = Interrupts::new();
    let raiser = interrupts.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        raiser.raise();
        thread::sleep(Duration::from_millis(300));
        raiser.raise();
    });
    let hard = std::sync::Arc::new(AtomicBool::new(false));
    let called = std::sync::Arc::clone(&hard);
    let rules = ExitRules {
        signal_grace: Duration::from_millis(1500),
        ..quick_rules()
    };
    let started = Instant::now();
    let (exit, _out, _) = cli(
        &f,
        &mut opener,
        &interrupts,
        command(export(20_000)),
        rules,
        Some(Box::new(move || called.store(true, Ordering::SeqCst))),
    );
    assert_eq!(exit, CliExit::Interrupted);
    assert!(
        hard.load(Ordering::SeqCst),
        "the second signal ran the hard exit"
    );
    // A stubborn job: the host gave up after the signal grace.
    assert!(started.elapsed() >= Duration::from_millis(1500));
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// What the Backend-shaped entry reported.
#[derive(Default)]
struct Events {
    routed: Vec<Option<u32>>,
    accepted: usize,
    progress: Vec<u64>,
    /// Early replies, and when they came.
    replied: Vec<(Result<Value, qrec_core::CoreError>, Instant)>,
}

impl RequestEvents for Events {
    fn routed(&mut self, host: Option<&qrec_core::protocol::HelloInfo>) {
        self.routed.push(host.map(|h| h.pid));
    }

    fn accepted(&mut self) {
        self.accepted += 1;
    }

    fn progress(&mut self, p: &qrec_core::protocol::Progress) {
        self.progress.push(p.done);
    }

    fn replied(&mut self, reply: &Result<Value, qrec_core::CoreError>) {
        self.replied.push((reply.clone(), Instant::now()));
    }
}

fn backend(
    f: &Fixture,
    opener: &mut FakeOpener,
    req: &Request,
    events: &mut dyn RequestEvents,
) -> Finish {
    let program = HostProgram::current_exe().unwrap();
    let interrupts = Interrupts::new();
    let cx = RequestContext {
        env: &f.paths_env,
        opener,
        host_program: &program,
        interrupts: &interrupts,
        rules: quick_rules(),
        on_second_signal: None,
    };
    request(
        cx,
        &f.dir,
        req.clone(),
        &RouteOpts::for_request(req),
        events,
    )
}

/// Interrupted in-process, the answer the Session gave anyway (here its
/// cancelled job's error) still reaches the caller, so the CLI can end
/// `--json` output with it (A§8.6).
#[test]
fn an_interrupted_request_keeps_the_reply_it_got() {
    let f = fixture();
    let req = Request::command(export(20_000), false);
    let program = HostProgram::current_exe().unwrap();
    let interrupts = Interrupts::new();
    let raiser = interrupts.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        raiser.raise();
    });
    let mut opener = FakeOpener::default();
    let cx = RequestContext {
        env: &f.paths_env,
        opener: &mut opener,
        host_program: &program,
        interrupts: &interrupts,
        rules: quick_rules(),
        on_second_signal: None,
    };
    let mut events = Events::default();
    let finish = request(
        cx,
        &f.dir,
        req.clone(),
        &RouteOpts::for_request(&req),
        &mut events,
    );
    match finish {
        Finish::Interrupted {
            reply: Some(Err(e)),
        } => assert_eq!(e.message, "interrupted"),
        other => panic!("expected the interrupted job's reply, got {other:?}"),
    }
    assert_eq!(events.routed, [None], "it ran in this process");
}

/// A Session that plays a client arriving right as its host decides to
/// exit: the first time `done` says stop, a request is sent and admitted
/// before `run_until` returns.
struct LateClient {
    inner: Box<dyn HostSession>,
    socket: PathBuf,
    client: Option<thread::JoinHandle<Vec<Value>>>,
}

impl HostSession for LateClient {
    fn inbox(&self) -> crate::msg::Inbox {
        self.inner.inbox()
    }

    fn run_until(&mut self, done: &mut dyn FnMut(&State) -> bool) {
        let socket = self.socket.clone();
        let client = &mut self.client;
        self.inner.run_until(&mut |state| {
            let stop = done(state);
            if stop && client.is_none() {
                let socket = socket.clone();
                let body = serde_json::to_value(WireBody::Command(set_ui())).unwrap();
                *client = Some(thread::spawn(move || {
                    raw_exchange(&socket, &request_line("late", body))
                }));
                // Admitted, read and queued for the Session meanwhile.
                thread::sleep(Duration::from_millis(300));
            }
            stop
        });
    }

    fn state(&self) -> &State {
        self.inner.state()
    }
}

/// A§8.3 step 3: a request admitted between the exit decision and the end
/// of accepting gets its real reply; the host never drops a request it
/// told `accepted` (it used to: "the host dropped this request without an
/// answer").
#[test]
fn a_request_that_arrives_as_the_host_exits_is_answered() {
    let f = fixture();
    let lock = ProjectLock::try_acquire(&f.dir).unwrap().unwrap();
    let opts = HostOptions {
        rules: quick_rules(),
        ..HostOptions::default()
    };
    let host = Host::start(HostKind::Cli, f.paths(), lock, &f.socket, opts).unwrap();
    let open = OpenOpts {
        host: HostKind::Cli,
        mutating: true,
        recovery: false,
    };
    let inner = FakeOpener::default()
        .open(&f.dir, open, Some(host.session_lock().unwrap()))
        .unwrap();
    host.attach(inner.inbox());
    let mut session = LateClient {
        inner,
        socket: f.socket.clone(),
        client: None,
    };
    // Its own command is done and nobody else used it: it decides to exit.
    let end = host.serve(&mut session, Some(&mut || true));
    assert_eq!(end, HostEnd::Idle);
    let client = session.client.take().expect("the late client ran");
    host.finish(Some(Box::new(session)), qrec_core::CoreError::io("exiting"));
    let lines = client.join().unwrap();
    assert_eq!(lines[0]["accepted"], true, "{lines:?}");
    assert_eq!(lines[1]["reply"]["ok"], true, "{lines:?}");
    assert_eq!(
        lines[1]["reply"]["data"]["handledBy"],
        std::process::id(),
        "{lines:?}"
    );
    assert!(!f.socket.exists());
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
}

/// A host that closes the request connection before `accepted` (it was
/// exiting) never ran it: the CLI routes again until the deadline instead
/// of giving up after one bounce with `busy` (A§8.3 steps 4 and 6).
#[test]
fn requests_bounced_by_exiting_hosts_are_routed_again() {
    use std::sync::atomic::AtomicUsize;
    let f = fixture();
    let runtime = f.socket.parent().unwrap().to_path_buf();
    let name = f.socket.file_name().unwrap().to_string_lossy().into_owned();
    let hello = json!({
        "pid": std::process::id(),
        "projectDir": f.dir,
        "host": "cli",
        "record": { "phase": "idle" },
        "jobs": [],
        "version": env!("CARGO_PKG_VERSION"),
    });
    let bounced = std::sync::Arc::new(AtomicUsize::new(0));
    let seen = std::sync::Arc::clone(&bounced);
    let _server = crate::ipc::testing::raw_server(&runtime, &name, move |line, stream| {
        let id = crate::ipc::testing::line_id(&line);
        let request: Value = serde_json::from_slice(&line).unwrap();
        let send = |msg: qrec_core::protocol::WireMessage| {
            crate::ipc::testing::send_line(&stream, &serde_json::to_value(msg).unwrap());
        };
        if request["body"] == "hello" {
            send(qrec_core::protocol::WireMessage::reply(
                id,
                &Ok(hello.clone()),
            ));
        } else if seen.fetch_add(1, Ordering::SeqCst) < 3 {
            // Closed before `accepted`: what a host that stops accepting does.
            drop(stream);
        } else {
            send(qrec_core::protocol::WireMessage::accepted(id.clone()));
            send(qrec_core::protocol::WireMessage::reply(
                id,
                &Ok(json!({ "answered": true })),
            ));
        }
    });
    let mut events = Events::default();
    let mut unused = FakeOpener::default();
    let req = Request::command(set_ui(), false);
    let finish = backend(&f, &mut unused, &req, &mut events);
    assert!(
        matches!(&finish, Finish::Reply(Ok(v)) if v["answered"] == true),
        "{finish:?}"
    );
    assert_eq!(bounced.load(Ordering::SeqCst), 4);
    assert!(unused.opened.lock().unwrap().is_empty());
}

#[test]
fn the_backend_entry_reports_where_each_request_ran() {
    let f = fixture();
    let req = Request::command(export(100), false);
    // No host: this process runs it.
    let mut events = Events::default();
    let finish = backend(&f, &mut FakeOpener::default(), &req, &mut events);
    assert!(
        matches!(&finish, Finish::Reply(Ok(v)) if v["exported"] == true),
        "{finish:?}"
    );
    assert_eq!(events.routed, [None]);
    assert_eq!(events.accepted, 1);
    assert_eq!(events.progress, [1, 2, 3, 4, 5]);
    // A live host: routed there, with its hello.
    let host = headless(&f, FakeOpener::default(), quick_rules(), None);
    wait_ready(&f);
    let mut events = Events::default();
    let mut unused = FakeOpener::default();
    let finish = backend(&f, &mut unused, &req, &mut events);
    assert!(matches!(&finish, Finish::Reply(Ok(_))), "{finish:?}");
    assert_eq!(events.routed, [Some(std::process::id())]);
    assert_eq!(events.accepted, 1);
    assert!(unused.opened.lock().unwrap().is_empty());
    assert_eq!(host.join().unwrap(), 0);
    // Needs a live host and there is none.
    let play = Request::command(qrec_core::Command::Play, false);
    let mut events = Events::default();
    let finish = backend(&f, &mut FakeOpener::default(), &play, &mut events);
    assert_eq!(finish, Finish::NoHost);
    assert!(events.routed.is_empty());
}

/// A lock a host just dropped can still be held for a moment by a copy of
/// its descriptor in a child another test forked: between `fork` and
/// `exec` a child holds every descriptor of this process, the lock's
/// `O_CLOEXEC` one too (`lock::acquire_eventually`). A host started then
/// takes the lock once and gives up ("another Qrec host holds …": the
/// flake of `the_backend_entry_reports_where_each_request_ran`, whose
/// headless host starts right after its in-process one, once in a full
/// workspace run), so [`headless`] waits for the copy to go. Here the
/// child takes 500 ms from its fork to its exec, on purpose.
#[test]
fn a_host_started_on_a_lock_just_dropped_waits_for_forked_copies() {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let f = fixture();
    let lock = hold_lock(&f.dir);
    let (mut forked, tell) = std::io::pipe().unwrap();
    let child = thread::spawn(move || {
        let fd = tell.as_raw_fd();
        let mut command = std::process::Command::new("true");
        // SAFETY: runs in the forked child before its exec, and calls only
        // async-signal-safe functions (write, nanosleep).
        unsafe {
            command.pre_exec(move || {
                libc::write(fd, b"f".as_ptr().cast(), 1);
                let pause = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 500_000_000,
                };
                libc::nanosleep(&pause, std::ptr::null_mut());
                Ok(())
            });
        }
        let status = command.status().expect("run true");
        drop(tell);
        status
    });
    let mut byte = [0u8; 1];
    forked.read_exact(&mut byte).unwrap();
    drop(lock);
    assert!(f.lock_held(), "the child's copy holds it until its exec");
    let host = headless(&f, FakeOpener::default(), quick_rules(), None);
    wait_ready(&f);
    let out = send(&f.socket, Request::command(set_ui(), false));
    assert_eq!(out.accepted, 1);
    assert!(child.join().unwrap().success());
    assert_eq!(host.join().unwrap(), 0);
}

/// A project with one 300-frame take placed as clip `c001`.
fn one_clip_project() -> qrec_core::Project {
    serde_json::from_value(json!({
        "version": 1,
        "name": "swamp",
        "createdAt": "2026-10-14T12:00:00Z",
        "canvas": { "width": 1920, "height": 1080, "fps": { "num": 30, "den": 1 }, "background": "#000000" },
        "scene": { "layers": [], "audio": [] },
        "recording": { "countdownSecs": 0, "encoder": "auto", "vaapiDevice": null },
        "autocut": {
            "autoRunAfterTake": false, "track": null, "thresholdDb": -35.0,
            "minSilenceMs": 400, "paddingMs": 120, "minKeepMs": 200,
        },
        "takes": {
            "take-0001": {
                "status": "ok", "recordedAt": "2026-10-14T12:00:00Z", "durationFrames": 300,
                "scene": { "layers": [], "audio": [] }, "streams": [], "placed": true,
            },
        },
        "nextTakeNumber": 2,
        "timeline": { "clips": [{ "id": "c001", "take": "take-0001", "in": 0, "out": 300 }] },
    }))
    .expect("a valid project")
}

#[test]
fn real_timeline_commands_round_trip_through_a_host() {
    let f = fixture();
    let opener = FakeOpener {
        project: Some(one_clip_project()),
        ..FakeOpener::default()
    };
    let host = headless(&f, opener, quick_rules(), None);
    wait_ready(&f);
    // The core's split runs in the host; its CmdResult crosses the wire.
    let split = qrec_core::Command::ClipSplit {
        clip: Some("c001".parse().unwrap()),
        at: "100f".parse().unwrap(),
        timeline: false,
    };
    let out = send(&f.socket, Request::command(split, false));
    let result = out.ok();
    assert_eq!(result["seq"], 1, "{result}");
    assert_eq!(
        result["created"].as_array().map(Vec::len),
        Some(1),
        "{result}"
    );
    // A bad id is the core's not_found, delivered as the reply.
    let missing = qrec_core::Command::ClipRemove {
        clips: vec!["cfff".parse().unwrap()],
    };
    let out = send(&f.socket, Request::command(missing, false));
    assert_eq!(out.err().code, ErrorCode::NotFound);
    // Undo moves the cursor back.
    let out = send(
        &f.socket,
        Request::command(qrec_core::Command::Undo { steps: 1 }, false),
    );
    assert_eq!(out.ok()["seq"], 0);
    assert_eq!(host.join().unwrap(), 0);
}

#[test]
fn a_host_running_another_version_is_noted() {
    let f = fixture();
    let published = crate::host::publish::publish(&f.socket).unwrap();
    let mut cfg = crate::ipc::server::ServerConfig::new(&f.dir, HostKind::Gui);
    cfg.version = "0.0.1".into();
    let hello = cfg.starting_hello();
    let server = IpcServer::start(published, cfg, Arc::new(|_| {})).unwrap();
    let (inbox, _session) = crate::ipc::testing::session_thread(move |msg| {
        if let Msg::Request { req, mut to } = msg {
            match req.body {
                WireBody::Hello => to.reply(Ok(serde_json::to_value(&hello).unwrap())),
                _ => to.reply(Ok(json!({ "answered": true }))),
            }
        }
        true
    });
    server.attach(inbox);
    let (exit, out, notices) = cli(
        &f,
        &mut FakeOpener::default(),
        &Interrupts::new(),
        query(Query::ClipList),
        quick_rules(),
        None,
    );
    assert_eq!(exit, CliExit::Replied);
    assert_eq!(out.ok()["answered"], true);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("is qrec 0.0.1"), "{notices:?}");
    assert!(notices[0].contains("gui host"), "{notices:?}");
}

// -------------------------------------------------- headless host (thread)

fn headless(
    f: &Fixture,
    opener: FakeOpener,
    rules: ExitRules,
    interrupts: Option<Interrupts>,
) -> thread::JoinHandle<u8> {
    // The host takes the lock once. One this test's last host dropped can
    // still be held for a moment by a child another test forked meanwhile
    // (between `fork` and `exec` it has a copy of every descriptor, the
    // lock's `O_CLOEXEC` one too): wait for that copy to go first.
    assert!(
        wait_for(Duration::from_secs(5), || !f.lock_held()),
        "{} stays locked",
        f.dir.display()
    );
    let env = f.paths_env.clone();
    let args = HeadlessArgs {
        project: f.dir.clone(),
        lock_fd: None,
        ready_fd: None,
    };
    thread::spawn(move || {
        let mut opener = opener;
        let opts = HostOptions {
            rules,
            interrupts,
            on_second_signal: Some(Box::new(|| {})),
            ..HostOptions::default()
        };
        run_headless(args, &env, &mut opener, opts)
    })
}

fn wait_ready(f: &Fixture) {
    assert!(
        wait_for(Duration::from_secs(10), || client::hello(
            &f.socket,
            Duration::from_secs(1)
        )
        .is_ok()),
        "the host never answered hello"
    );
}

fn send(socket: &Path, req: Request) -> Collect {
    let mut out = Collect::default();
    let sent = send_request(socket, &req, &mut out, &SendOpts::new(&Interrupts::new())).unwrap();
    assert_eq!(sent, Sent::Replied);
    out
}

#[test]
fn an_unused_headless_host_exits_after_its_timeout() {
    let f = fixture();
    let rules = ExitRules {
        unused_timeout: Duration::from_millis(400),
        ..quick_rules()
    };
    let started = Instant::now();
    let host = headless(&f, FakeOpener::default(), rules, None);
    wait_ready(&f);
    assert_eq!(host.join().unwrap(), 0);
    assert!(started.elapsed() >= Duration::from_millis(400));
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
    assert!(!f.socket.exists());
}

#[test]
fn a_headless_host_idles_out_after_serving() {
    let f = fixture();
    let host = headless(&f, FakeOpener::default(), quick_rules(), None);
    wait_ready(&f);
    let out = send(&f.socket, Request::command(set_ui(), false));
    assert_eq!(out.accepted, 1);
    let replied = Instant::now();
    assert_eq!(host.join().unwrap(), 0);
    assert!(replied.elapsed() >= Duration::from_millis(250));
    assert!(replied.elapsed() < Duration::from_secs(3));
}

#[test]
fn requests_made_while_the_session_opens_wait_for_it() {
    let f = fixture();
    let opener = FakeOpener {
        delay: Duration::from_millis(500),
        ..FakeOpener::default()
    };
    let log = opener.log.clone();
    let host = headless(&f, opener, quick_rules(), None);
    // `hello` is answered while the Session is still opening…
    wait_ready(&f);
    assert!(log.names().is_empty() || !log.names().contains(&"request".to_owned()));
    // …and a command waits for it.
    let out = send(&f.socket, Request::command(set_ui(), false));
    assert_eq!(out.ok()["handledBy"], std::process::id());
    let names = log.names();
    let open = names.iter().position(|n| n == "open").unwrap();
    let request = names.iter().position(|n| n == "request").unwrap();
    assert!(open < request, "{names:?}");
    assert_eq!(host.join().unwrap(), 0);
}

#[test]
fn shutdown_stops_accepting_finalizes_and_lets_jobs_finish() {
    let f = fixture();
    let opener = FakeOpener::default();
    let log = opener.log.clone();
    let host = headless(&f, opener, quick_rules(), None);
    wait_ready(&f);
    let socket = f.socket.clone();
    let job = thread::spawn(move || send(&socket, Request::command(export(700), false)));
    assert!(wait_for(Duration::from_secs(5), || log
        .names()
        .contains(&"job".to_owned())));
    let lines = raw_exchange(&f.socket, &request_line("s", json!("shutdown")));
    assert_eq!(lines[1]["reply"]["ok"], true, "{lines:?}");
    // No new connections (and the record soon says why)…
    assert!(client::hello(&f.socket, Duration::from_millis(300)).is_err());
    assert!(wait_for(Duration::from_secs(2), || f.record_state()
        == Some(HostState::Stopping)));
    // …but the running job finishes and its client gets the reply.
    let out = job.join().unwrap();
    assert_eq!(out.ok()["exported"], true);
    assert_eq!(host.join().unwrap(), 0);
    let names = log.names();
    assert!(names.contains(&"shutdown".to_owned()), "{names:?}");
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
}

#[test]
fn a_signal_finalizes_the_recording_then_the_host_exits() {
    let f = fixture();
    let opener = FakeOpener::default();
    let log = opener.log.clone();
    let interrupts = Interrupts::new();
    let host = headless(&f, opener, quick_rules(), Some(interrupts.clone()));
    wait_ready(&f);
    let start = qrec_core::Command::RecordStart {
        mode: RecordModeArg::Append,
        countdown: None,
        wait: false,
    };
    assert_eq!(
        send(&f.socket, Request::command(start, false)).ok()["recording"],
        true
    );
    interrupts.raise();
    assert_eq!(host.join().unwrap(), 130);
    // RecordStop goes first (finalize, not cancel). Quiet right after it, the
    // host may exit before the queued `Interrupt` (nothing left to cancel).
    let names = log.names();
    let finalized = names
        .iter()
        .position(|n| n == "finalized")
        .expect("finalized");
    if let Some(interrupt) = names.iter().position(|n| n == "interrupt") {
        assert!(finalized < interrupt, "{names:?}");
    }
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
}

/// A signal while a headless host's Session opens: its `RecordStop` and
/// `Interrupt` reach the Session before a `record start` that came in
/// meanwhile (A§8.6), so the Session can refuse the recording instead of
/// starting it first.
#[test]
fn a_signal_while_the_session_opens_goes_before_the_requests_parked_meanwhile() {
    let f = fixture();
    let opener = FakeOpener {
        delay: Duration::from_millis(800),
        ..FakeOpener::default()
    };
    let log = opener.log.clone();
    let interrupts = Interrupts::new();
    let host = headless(&f, opener, quick_rules(), Some(interrupts.clone()));
    wait_ready(&f);
    let socket = f.socket.clone();
    let start = qrec_core::Command::RecordStart {
        mode: RecordModeArg::Append,
        countdown: None,
        wait: false,
    };
    let client = thread::spawn(move || send(&socket, Request::command(start, false)));
    // The request is parked in the server (the Session is still opening).
    thread::sleep(Duration::from_millis(200));
    interrupts.raise();
    let _ = client.join();
    let code = host.join().unwrap();
    assert_eq!(code, 130);
    let names = log.names();
    let interrupt = names.iter().position(|n| n == "interrupt");
    let recording = names.iter().position(|n| n == "recording");
    if let Some(recording) = recording {
        let interrupt = interrupt.expect("the interrupt reached the Session");
        assert!(interrupt < recording, "{names:?}");
    }
    assert!(interrupt.is_some(), "{names:?}");
}

#[test]
fn startup_errors_go_to_the_ready_fd() {
    let f = fixture();
    let other = hold_lock(&f.dir);
    let mut fds = [0; 2];
    // SAFETY: room for two fds.
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    // SAFETY: fresh fds we own.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // The host takes ownership of the write end's number.
    let ready_fd = std::os::fd::IntoRawFd::into_raw_fd(write);
    let args = HeadlessArgs {
        project: f.dir.clone(),
        lock_fd: None,
        ready_fd: Some(ready_fd),
    };
    let code = run_headless(
        args,
        &f.paths_env,
        &mut FakeOpener::default(),
        HostOptions {
            rules: quick_rules(),
            ..HostOptions::default()
        },
    );
    assert_eq!(code, 1);
    let mut file = fs::File::from(read);
    let mut line = String::new();
    std::io::Read::read_to_string(&mut file, &mut line).unwrap();
    let value: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "busy");
    drop(other);
}

// ------------------------------------------------------- real processes

/// Kills a child process (or a pid) when a test ends early.
struct Reap {
    child: Option<Child>,
    pid: Option<i32>,
}

impl Drop for Reap {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(pid) = self.pid {
            // SAFETY: kill(2); the pid is a test host that may be gone.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

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

fn events_named(log: &Path, name: &str) -> Vec<Value> {
    read_events(log)
        .into_iter()
        .filter(|e| e["event"] == name)
        .collect()
}

fn cli_child(f: &Fixture, log: &Path, body: &WireBody) -> Child {
    child_command(&f.env, "cli", log)
        .env(REQUEST, serde_json::to_string(body).unwrap())
        .env(PROJECT, &f.dir)
        .env(RULES, rules_env(&quick_rules()))
        .current_dir(&f.dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn headless_child(f: &Fixture, log: &Path, stubborn: bool) -> Child {
    let mut command = child_command(&f.env, "headless", log);
    command
        .args(["--project".as_ref(), f.dir.as_os_str()])
        .env(
            RULES,
            rules_env(&ExitRules {
                unused_timeout: Duration::from_secs(10),
                signal_grace: Duration::from_secs(5),
                ..quick_rules()
            }),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if stubborn {
        command.env(STUBBORN, "1");
    }
    command.spawn().unwrap()
}

#[test]
fn processes_racing_to_become_the_host_share_one() {
    let f = fixture();
    let log = f.env.root().join("race.jsonl");
    let body = WireBody::Command(export(600));
    let mut children: Vec<Reap> = (0..3)
        .map(|_| Reap {
            child: Some(cli_child(&f, &log, &body)),
            pid: None,
        })
        .collect();
    for reap in &mut children {
        let code = wait_exit(reap.child.as_mut().unwrap(), Duration::from_secs(30));
        assert_eq!(code, Some(0), "{:#?}", read_events(&log));
    }
    let opens = events_named(&log, "open");
    assert_eq!(opens.len(), 1, "exactly one became the host: {opens:?}");
    let host_pid = opens[0]["pid"].clone();
    let done = events_named(&log, "cliDone");
    assert_eq!(done.len(), 3);
    for d in &done {
        assert_eq!(d["exit"], "Replied");
        assert_eq!(d["reply"]["data"]["handledBy"], host_pid, "{d}");
    }
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
}

#[test]
fn the_spawned_host_keeps_the_lock_after_its_parent_exits() {
    let f = fixture();
    let log = f.env.root().join("spawn.jsonl");
    let mut spawner = child_command(&f.env, "spawner", &log)
        .env(PROJECT, &f.dir)
        .env(
            RULES,
            rules_env(&ExitRules {
                unused_timeout: Duration::from_secs(10),
                ..quick_rules()
            }),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert_eq!(
        wait_exit(&mut spawner, Duration::from_secs(30)),
        Some(0),
        "{:#?}",
        read_events(&log)
    );
    let spawned = events_named(&log, "spawned");
    assert_eq!(spawned.len(), 1, "{:#?}", read_events(&log));
    let host_pid = spawned[0]["host"].as_u64().unwrap() as u32;
    let _reap = Reap {
        child: None,
        pid: Some(host_pid as i32),
    };
    assert_ne!(host_pid, spawner.id());
    // The spawner is gone; the host it handed the lock to holds it.
    assert!(f.lock_held());
    let hello = client::hello(&f.socket, Duration::from_secs(2)).unwrap();
    assert_eq!((hello.pid, hello.host), (host_pid, HostKind::Headless));
    let record = f.record();
    assert_eq!((record.pid, record.state), (host_pid, HostState::Ready));
    // Its stdout/stderr go to the host log, named after its pid.
    let host_log = PathBuf::from(spawned[0]["log"].as_str().unwrap());
    assert!(host_log.is_file());
    assert!(
        host_log
            .to_string_lossy()
            .ends_with(&format!("-{host_pid}-host.log"))
    );
    assert_eq!(find_host_log(&f.paths(), host_pid), Some(host_log));
    // The lock goes away with the host.
    let lines = raw_exchange(&f.socket, &request_line("s", json!("shutdown")));
    assert_eq!(lines[1]["reply"]["ok"], true);
    assert!(wait_for(Duration::from_secs(10), || !f.lock_held()));
    assert!(wait_for(Duration::from_secs(5), || !alive(host_pid)));
    let exits = events_named(&log, "hostExit");
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0]["code"], 0);
}

/// The spawned host gets the lock, the ready pipe and its log, and nothing
/// else of its parent's (A§8.3 step 5): a descriptor the CLI inherited from
/// its own caller — a pipe on fd 9 here, like `qrec record start 9>&1 |
/// …` or a key-bind script's flock — is closed in the detached host, so
/// that pipe ends when the CLI does, not when the take does. The host used
/// to keep every inherited descriptor until it exited.
#[test]
fn the_spawned_host_keeps_none_of_its_parents_other_descriptors() {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let f = fixture();
    let log = f.env.root().join("spawn-fds.jsonl");
    // Close-on-exec here, so only the spawner (fd 9) gets the write end.
    let (read, write) = crate::ipc::server::pipe_cloexec().unwrap();
    let pipe = fs::read_link(format!("/proc/self/fd/{}", read.as_raw_fd())).unwrap();
    let write_fd = write.as_raw_fd();
    let mut command = child_command(&f.env, "spawner", &log);
    command
        .env(PROJECT, &f.dir)
        .env(
            RULES,
            rules_env(&ExitRules {
                unused_timeout: Duration::from_secs(10),
                ..quick_rules()
            }),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: dup2 between fork and exec on a descriptor open in the child.
    unsafe {
        command.pre_exec(move || {
            // dup2 clears close-on-exec: the spawner inherits it as fd 9.
            if libc::dup2(write_fd, 9) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut spawner = command.spawn().unwrap();
    drop(write);
    assert_eq!(
        wait_exit(&mut spawner, Duration::from_secs(30)),
        Some(0),
        "{:#?}",
        read_events(&log)
    );
    let spawned = events_named(&log, "spawned");
    assert_eq!(spawned.len(), 1, "{:#?}", read_events(&log));
    let host_pid = spawned[0]["host"].as_u64().unwrap() as u32;
    let _reap = Reap {
        child: None,
        pid: Some(host_pid as i32),
    };
    assert!(alive(host_pid));
    // The host runs, yet nobody holds the write end any more: EOF.
    let mut pfd = libc::pollfd {
        fd: read.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll(2) on one valid pollfd.
    let ready = unsafe { libc::poll(&mut pfd, 1, 5000) };
    let held: Vec<PathBuf> = fs::read_dir(format!("/proc/{host_pid}/fd"))
        .unwrap()
        .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
        .collect();
    assert!(!held.contains(&pipe), "the host kept fd 9: {held:?}");
    assert_eq!(ready, 1, "the pipe never ended: {held:?}");
    let mut byte = [0u8; 1];
    // SAFETY: read(2) into a local buffer from our pipe.
    let n = unsafe { libc::read(read.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
    assert_eq!(n, 0, "EOF");
    // It has what the hand-off gives it, and serves as before.
    assert!(f.lock_held());
    assert!(
        held.iter().any(|p| p
            .to_string_lossy()
            .ends_with(&format!("-{host_pid}-host.log"))),
        "{held:?}"
    );
    let hello = client::hello(&f.socket, Duration::from_secs(2)).unwrap();
    assert_eq!((hello.pid, hello.host), (host_pid, HostKind::Headless));
    let lines = raw_exchange(&f.socket, &request_line("s", json!("shutdown")));
    assert_eq!(lines[1]["reply"]["ok"], true);
    assert!(wait_for(Duration::from_secs(10), || !f.lock_held()));
}

#[test]
fn record_start_without_a_host_spawns_a_headless_one() {
    let f = fixture();
    let log = f.env.root().join("record.jsonl");
    let start = WireBody::Command(qrec_core::Command::RecordStart {
        mode: RecordModeArg::Append,
        countdown: None,
        wait: false,
    });
    let mut cli = cli_child(&f, &log, &start);
    let code = wait_exit(&mut cli, Duration::from_secs(40));
    assert_eq!(code, Some(0), "{:#?}", read_events(&log));
    let done = events_named(&log, "cliDone");
    let reply = &done[0]["reply"];
    assert_eq!(reply["data"]["recording"], true, "{reply}");
    let host_pid = reply["data"]["handledBy"].as_u64().unwrap() as u32;
    assert_ne!(host_pid, cli.id(), "a separate host records");
    let _reap = Reap {
        child: None,
        pid: Some(host_pid as i32),
    };
    // The CLI exited; the recorder keeps running and holds the project.
    assert!(alive(host_pid));
    assert!(f.lock_held());
    let out = send(
        &f.socket,
        Request::command(qrec_core::Command::RecordStop, false),
    );
    assert_eq!(out.ok()["finalized"], true);
    // Idle again: it exits on its own.
    assert!(wait_for(Duration::from_secs(10), || !f.lock_held()));
    assert!(wait_for(Duration::from_secs(5), || !alive(host_pid)));
}

/// `record start` spawns a host whose Session can't open (a corrupt
/// qrec.json): the CLI reports that error and its restore hint, whether its
/// request reached the host before it exited or only found it gone (the
/// host also says why on the ready pipe). It used to answer a generic
/// "the background host exited right after starting" about a time in four.
#[test]
fn record_start_reports_why_the_spawned_host_could_not_open() {
    for round in 0..3 {
        let f = fixture();
        let log = f.env.root().join(format!("open-fail-{round}.jsonl"));
        let start = WireBody::Command(qrec_core::Command::RecordStart {
            mode: RecordModeArg::Append,
            countdown: None,
            wait: false,
        });
        let mut cli = child_command(&f.env, "cli", &log)
            .env(REQUEST, serde_json::to_string(&start).unwrap())
            .env(PROJECT, &f.dir)
            .env(RULES, rules_env(&quick_rules()))
            .env(crate::ipc::testing::OPEN_FAIL, "1")
            .current_dir(&f.dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let code = wait_exit(&mut cli, Duration::from_secs(40));
        let done = events_named(&log, "cliDone");
        let reply = &done[0]["reply"];
        let expected = crate::ipc::testing::corrupt_project_error();
        assert_eq!(reply["ok"], false, "{reply}");
        assert_eq!(reply["error"]["code"], "invalid", "{reply}");
        assert_eq!(reply["error"]["message"], expected.message, "{reply}");
        assert!(
            reply["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("qrec history restore 1 --yes"),
            "{reply}"
        );
        assert_eq!(code, Some(1));
        assert!(wait_for(Duration::from_secs(5), || !f.lock_held()));
    }
}

#[test]
fn sigterm_finalizes_the_take_and_exits_130() {
    let f = fixture();
    let log = f.env.root().join("sigterm.jsonl");
    let mut host = headless_child(&f, &log, false);
    let reap = Reap {
        child: None,
        pid: Some(host.id() as i32),
    };
    wait_ready(&f);
    let start = qrec_core::Command::RecordStart {
        mode: RecordModeArg::Append,
        countdown: None,
        wait: false,
    };
    assert_eq!(
        send(&f.socket, Request::command(start, false)).ok()["recording"],
        true
    );
    // SAFETY: kill(2) on our child.
    unsafe { libc::kill(host.id() as i32, libc::SIGTERM) };
    assert_eq!(wait_exit(&mut host, Duration::from_secs(10)), Some(130));
    drop(reap);
    let names: Vec<String> = read_events(&log)
        .iter()
        .filter_map(|e| e["event"].as_str().map(str::to_owned))
        .collect();
    assert!(names.contains(&"finalized".to_owned()), "{names:?}");
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
    assert!(!f.socket.exists());
}

#[test]
fn a_second_signal_kills_child_processes_and_exits_130() {
    let f = fixture();
    let log = f.env.root().join("second.jsonl");
    let mut host = headless_child(&f, &log, true);
    let _reap = Reap {
        child: None,
        pid: Some(host.id() as i32),
    };
    wait_ready(&f);
    // A job that ignores cancellation and runs `sleep 60` in its own group.
    let socket = f.socket.clone();
    let _job = thread::spawn(move || {
        let mut out = Collect::default();
        let _ = send_request(
            &socket,
            &Request::command(export_spec("ms:60000:sleep"), false),
            &mut out,
            &SendOpts::new(&Interrupts::new()),
        );
        out
    });
    assert!(wait_for(Duration::from_secs(10), || !events_named(
        &log, "child"
    )
    .is_empty()));
    let sleep_pid = events_named(&log, "child")[0]["child"].as_u64().unwrap() as u32;
    assert!(alive(sleep_pid));
    // SAFETY: kill(2) on our child.
    unsafe { libc::kill(host.id() as i32, libc::SIGTERM) };
    assert!(wait_for(Duration::from_secs(5), || !events_named(
        &log,
        "interrupt"
    )
    .is_empty()));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        host.try_wait().unwrap(),
        None,
        "busy: the first signal waits"
    );
    let second = Instant::now();
    // SAFETY: kill(2) on our child.
    unsafe { libc::kill(host.id() as i32, libc::SIGTERM) };
    assert_eq!(wait_exit(&mut host, Duration::from_secs(5)), Some(130));
    assert!(second.elapsed() < Duration::from_secs(3));
    assert!(
        wait_for(Duration::from_secs(5), || !alive(sleep_pid)),
        "the child group was killed"
    );
    // The lock died with the process; the socket was unlinked first.
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
    assert!(!f.socket.exists());
}

/// A recording asked while a CLI command holds the project waits for that
/// command to end, then runs in a headless host of its own: never inside
/// the command's process, which it used to keep alive (its result held
/// back) for as long as the take lasted (A§8.5). The command's job here
/// isn't an export (the fake's plain job, like a frame save or an
/// autocut): an export refuses the recording at once instead (next test).
#[test]
fn a_recording_never_runs_in_a_cli_commands_host() {
    let f = fixture();
    let log = f.env.root().join("cli-record.jsonl");
    let mut command = Reap {
        child: Some(cli_child(&f, &log, &WireBody::Command(export(1500)))),
        pid: None,
    };
    let command_pid = command.child.as_ref().unwrap().id();
    assert!(wait_for(Duration::from_secs(10), || client::hello(
        &f.socket,
        Duration::from_secs(1)
    )
    .is_ok_and(|h| h.host == HostKind::Cli && h.pid == command_pid)));
    let start = WireBody::Command(qrec_core::Command::RecordStart {
        mode: RecordModeArg::Append,
        countdown: None,
        wait: false,
    });
    let mut recorder = Reap {
        child: Some(cli_child(&f, &log, &start)),
        pid: None,
    };
    let recorder_pid = recorder.child.as_ref().unwrap().id();
    assert_eq!(
        wait_exit(command.child.as_mut().unwrap(), Duration::from_secs(30)),
        Some(0),
        "{:#?}",
        read_events(&log)
    );
    assert_eq!(
        wait_exit(recorder.child.as_mut().unwrap(), Duration::from_secs(40)),
        Some(0),
        "{:#?}",
        read_events(&log)
    );
    let done = events_named(&log, "cliDone");
    let by = |pid: u32| {
        done.iter()
            .find(|d| d["pid"] == pid)
            .unwrap_or_else(|| panic!("no cliDone from {pid}: {done:#?}"))
    };
    let exported = &by(command_pid)["reply"];
    assert_eq!(exported["data"]["exported"], true, "{exported}");
    assert_eq!(exported["data"]["handledBy"], command_pid);
    let recording = &by(recorder_pid)["reply"];
    assert_eq!(recording["data"]["recording"], true, "{recording}");
    let host_pid = recording["data"]["handledBy"].as_u64().unwrap() as u32;
    let _host_reap = Reap {
        child: None,
        pid: Some(host_pid as i32),
    };
    assert_ne!(host_pid, command_pid, "not in the command's process");
    assert_ne!(host_pid, recorder_pid);
    let hello = client::hello(&f.socket, Duration::from_secs(2)).unwrap();
    assert_eq!((hello.pid, hello.host), (host_pid, HostKind::Headless));
    // The command's Session never even saw the request (only `hello`s and
    // its own export).
    assert!(
        read_events(&log)
            .iter()
            .filter(|e| e["pid"] == command_pid && e["event"] == "request")
            .filter(|e| e["body"]["command"].is_object())
            .all(|e| e["body"]["command"]["cmd"] == "export"),
        "{:#?}",
        read_events(&log)
    );
    let out = send(
        &f.socket,
        Request::command(qrec_core::Command::RecordStop, false),
    );
    assert_eq!(out.ok()["finalized"], true);
    assert!(wait_for(Duration::from_secs(10), || !f.lock_held()));
}

/// Recording is blocked during an export, whichever process runs it (SPEC
/// §13.1, A§5.3): a `record toggle|start` while a CLI command's own host
/// exports is refused at once with the core's `Busy` (exit 6), as a GUI or
/// headless host refuses it. It used to wait silently for the export to
/// end and then start a take nobody was told about (or, past 20 s, blame
/// the command for not recording).
#[test]
fn a_recording_asked_while_a_cli_command_exports_is_refused_at_once() {
    let f = fixture();
    let log = f.env.root().join("cli-export.jsonl");
    let export = WireBody::Command(export_spec("ms:10000:export"));
    let mut command = Reap {
        child: Some(cli_child(&f, &log, &export)),
        pid: None,
    };
    let command_pid = command.child.as_ref().unwrap().id();
    // Its own host runs the export.
    assert!(wait_for(Duration::from_secs(10), || client::hello(
        &f.socket,
        Duration::from_secs(1)
    )
    .is_ok_and(|h| h.host == HostKind::Cli
        && h.pid == command_pid
        && h.jobs
            .iter()
            .any(|j| matches!(j.kind, JobKind::Export { .. })))));
    let toggle = WireBody::Command(qrec_core::Command::RecordToggle { wait: false });
    let start = WireBody::Command(qrec_core::Command::RecordStart {
        mode: RecordModeArg::Append,
        countdown: None,
        wait: false,
    });
    for body in [toggle, start] {
        let mut recorder = Reap {
            child: Some(cli_child(&f, &log, &body)),
            pid: None,
        };
        let recorder_pid = recorder.child.as_ref().unwrap().id();
        assert_eq!(
            wait_exit(recorder.child.as_mut().unwrap(), Duration::from_secs(20)),
            Some(6),
            "{:#?}",
            read_events(&log)
        );
        assert_eq!(
            command.child.as_mut().unwrap().try_wait().unwrap(),
            None,
            "answered while the export runs, not once it ended"
        );
        let done = events_named(&log, "cliDone");
        let reply = &done
            .iter()
            .find(|d| d["pid"] == recorder_pid)
            .unwrap_or_else(|| panic!("no cliDone from {recorder_pid}: {done:#?}"))["reply"];
        assert_eq!(reply["error"]["code"], "busy", "{reply}");
        assert_eq!(reply["error"]["message"], "an export is running (job-1)");
        assert_eq!(
            reply["error"]["hint"],
            "wait for it to finish, or cancel it with qrec export cancel"
        );
    }
    // A client that sends it anyway isn't bounced: the host's core refuses
    // it, accepted and answered at once.
    let raw = send(
        &f.socket,
        Request::command(qrec_core::Command::RecordToggle { wait: false }, false),
    );
    assert_eq!(raw.accepted, 1);
    assert_eq!(raw.err().message, "an export is running (job-1)");
    // The export ends normally; nothing recorded, no headless host started.
    assert_eq!(
        wait_exit(command.child.as_mut().unwrap(), Duration::from_secs(30)),
        Some(0),
        "{:#?}",
        read_events(&log)
    );
    let events = read_events(&log);
    assert!(
        events.iter().all(|e| e["event"] != "recording"),
        "{events:#?}"
    );
    assert!(
        events.iter().all(|e| e["role"] != "headless"),
        "{events:#?}"
    );
    assert_eq!(events_named(&log, "recordRefused").len(), 1, "{events:#?}");
    assert!(wait_for(Duration::from_secs(5), || !f.lock_held()));
}

/// The second Ctrl+C of an in-process command kills its children at once
/// and still ends it the normal way, so the CLI can print its final object
/// (A§8.6). It used to `_exit(130)` on the spot: no final object, no
/// warning, temp outputs left behind.
#[test]
fn a_second_signal_ends_an_in_process_command_with_an_answer() {
    let f = fixture();
    let log = f.env.root().join("cli-second.jsonl");
    // A job that ignores cancellation and runs `sleep 60` in its own group;
    // a long signal grace, so only the second signal explains a quick end.
    let body = WireBody::Command(export_spec("ms:60000:sleep"));
    let child = child_command(&f.env, "cli", &log)
        .env(REQUEST, serde_json::to_string(&body).unwrap())
        .env(PROJECT, &f.dir)
        .env(
            RULES,
            rules_env(&ExitRules {
                signal_grace: Duration::from_secs(30),
                ..quick_rules()
            }),
        )
        .env(STUBBORN, "1")
        .current_dir(&f.dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    let mut reap = Reap {
        child: Some(child),
        pid: None,
    };
    let cli = reap.child.as_mut().unwrap();
    assert!(wait_for(Duration::from_secs(10), || !events_named(
        &log, "child"
    )
    .is_empty()));
    let sleep_pid = events_named(&log, "child")[0]["child"].as_u64().unwrap() as u32;
    // SAFETY: kill(2) on our child.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    assert!(wait_for(Duration::from_secs(5), || !events_named(
        &log,
        "interrupt"
    )
    .is_empty()));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        cli.try_wait().unwrap(),
        None,
        "busy: the first signal waits"
    );
    let second = Instant::now();
    // SAFETY: kill(2) on our child.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(wait_exit(cli, Duration::from_secs(10)), Some(130));
    assert!(
        second.elapsed() < Duration::from_secs(5),
        "{:?}",
        second.elapsed()
    );
    assert!(
        wait_for(Duration::from_secs(5), || !alive(sleep_pid)),
        "the child group was killed"
    );
    // It returned like any interrupted request, so the CLI could print.
    let done = events_named(&log, "cliDone");
    assert_eq!(done.len(), 1, "{:#?}", read_events(&log));
    assert_eq!(done[0]["exit"], "Interrupted");
    assert!(
        wait_for(Duration::from_secs(2), || !f.lock_held()),
        "the lock was released"
    );
    assert!(!f.socket.exists());
}

#[test]
fn a_host_started_by_hand_takes_the_lock_itself() {
    let f = fixture();
    let log = f.env.root().join("byhand.jsonl");
    let mut host = headless_child(&f, &log, false);
    let _reap = Reap {
        child: None,
        pid: Some(host.id() as i32),
    };
    wait_ready(&f);
    let hello = client::hello(&f.socket, Duration::from_secs(2)).unwrap();
    assert_eq!(hello.pid, host.id());
    // A second one finds the project taken.
    let mut second = headless_child(&f, &f.env.root().join("second.jsonl"), false);
    assert_eq!(wait_exit(&mut second, Duration::from_secs(10)), Some(1));
    let lines = raw_exchange(&f.socket, &request_line("s", json!("shutdown")));
    assert_eq!(lines[1]["reply"]["ok"], true);
    assert_eq!(wait_exit(&mut host, Duration::from_secs(10)), Some(0));
}
