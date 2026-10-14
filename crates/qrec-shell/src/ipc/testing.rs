//! Test support for the IPC layer and the hosts: short socket dirs, raw
//! clients and servers, a recording responder, a Session stand-in
//! ([`FakeSession`]) and the entry point that lets a test re-execute this
//! test binary as a CLI, a headless host or a spawner ([`child_entry`]).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::Utc;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use qrec_core::model::{Project, ProjectDefaults};
use qrec_core::protocol::{Command as CoreCommand, HelloInfo, Progress, ProgressPhase, Query, WireBody};
use qrec_core::state::{History, HostKind, Job, JobKind, RecordMode, RecordPhase, UiState};
use qrec_core::{CoreError, Ctx, Event, Facts, JobId, ReqId, State, TakeId};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::framing::{LineReader, ReadError, write_all_before};
use super::lock_glue::ProjectLock;
use super::lock_glue::publish::publish;
use super::server::{Hook, IpcServer, ServerConfig, ServerEvent};
use crate::deps::OpenOpts;
use crate::error::ShellError;
use crate::host::signals::{self, HOST_SIGNALS, Interrupts};
use crate::host::spawn::{HostProgram, spawn_headless};
use crate::host::{
    CliContext, CliExit, CliRequest, ExitRules, HeadlessArgs, HostOptions, HostSession, Opener,
    run_cli, run_headless,
};
use crate::msg::{Inbox, Msg, Request, Responder};
use crate::paths::{Env, ProjectPaths, ensure_runtime_dir, socket_path};
use crate::testkit::TestEnv;

/// A private (0700) dir under `/tmp` with room for `<64 hex>.sock.<pid>`
/// names, like a runtime dir.
pub(crate) fn socket_dir() -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .prefix("qri-")
        .tempdir_in("/tmp")
        .expect("create a socket dir");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    dir
}

/// A started server on `<dir>/p.sock` with a hook that records its events.
pub(crate) struct TestServer {
    pub server: IpcServer,
    pub socket: PathBuf,
    pub events: Arc<Mutex<Vec<ServerEvent>>>,
}

pub(crate) fn start_server(dir: &Path, edit: impl FnOnce(&mut ServerConfig)) -> TestServer {
    let socket = dir.join("p.sock");
    let published = publish(&socket).expect("publish");
    let mut cfg = ServerConfig::new(Path::new("/projects/swamp"), HostKind::Headless);
    edit(&mut cfg);
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    let hook: Hook = Arc::new(move |ev| seen.lock().unwrap().push(ev));
    let server = IpcServer::start(published, cfg, hook).expect("start the server");
    TestServer {
        server,
        socket,
        events,
    }
}

/// Sends one raw line and collects every line until the host closes.
pub(crate) fn raw_exchange(socket: &Path, line: &str) -> Vec<Value> {
    let stream = UnixStream::connect(socket).expect("connect");
    let deadline = Instant::now() + Duration::from_secs(10);
    write_all_before(&stream, format!("{line}\n").as_bytes(), deadline).expect("send");
    read_all(stream, deadline)
}

/// Every line until EOF, parsed as JSON.
pub(crate) fn read_all(stream: UnixStream, deadline: Instant) -> Vec<Value> {
    let mut reader = LineReader::new(stream);
    let mut lines = Vec::new();
    loop {
        match reader.read_line(64 << 20, deadline) {
            Ok(line) => lines.push(serde_json::from_slice(&line).expect("a JSON line")),
            Err(ReadError::Eof) => return lines,
            Err(e) => panic!("reading host lines: {e} (after {lines:?})"),
        }
    }
}

/// `{"v":1,"id":<id>,"body":…}`.
pub(crate) fn request_line(id: &str, body: Value) -> String {
    json!({ "v": 1, "id": id, "body": body }).to_string()
}

/// A thread playing the Session: every message goes to `handler` until the
/// inbox's senders are gone (or `handler` returns false).
pub(crate) fn session_thread(
    mut handler: impl FnMut(Msg) -> bool + Send + 'static,
) -> (Inbox, JoinHandle<()>) {
    let (inbox, rx) = Inbox::channel();
    let handle = thread::spawn(move || {
        while let Ok(msg) = rx.recv_timeout(Duration::from_secs(30)) {
            if !handler(msg) {
                break;
            }
        }
    });
    (inbox, handle)
}

/// Polls `cond` every 10 ms for up to `timeout`.
pub(crate) fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Appends one JSON line to `path` (the multi-process tests' event log).
pub(crate) fn log_event(path: &Path, event: &Value) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open the event log");
    let mut line = serde_json::to_vec(event).expect("serialize");
    line.push(b'\n');
    // One write per line: appends from several processes don't interleave.
    file.write_all(&line).expect("write the event log");
}

/// The lines of an event log (empty if it doesn't exist yet).
pub(crate) fn read_events(path: &Path) -> Vec<Value> {
    match fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("an event line"))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Holds a project's lock from this process (a stand-in for another host).
pub(crate) fn hold_lock(dir: &Path) -> ProjectLock {
    ProjectLock::try_acquire(dir)
        .expect("lock")
        .expect("the lock is free")
}

// ------------------------------------------------------------ raw server

/// A socket whose connections run a scripted handler: for protocol
/// failures a real host never produces (silence, EOF at odd moments).
pub(crate) struct RawServer {
    pub socket: PathBuf,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// Serves `<dir>/<name>`: each connection's first line goes to `handler`
/// with the stream, on its own thread.
pub(crate) fn raw_server(
    dir: &Path,
    name: &str,
    handler: impl Fn(Vec<u8>, UnixStream) + Send + Sync + 'static,
) -> RawServer {
    let socket = dir.join(name);
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let handler = Arc::new(handler);
    let handle = thread::spawn(move || {
        while !stopped.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let handler = Arc::clone(&handler);
                    thread::spawn(move || {
                        let mut reader = LineReader::new(stream.try_clone().unwrap());
                        let deadline = Instant::now() + Duration::from_secs(5);
                        if let Ok(line) = reader.read_line(1 << 20, deadline) {
                            handler(line, stream);
                        }
                    });
                }
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
    });
    RawServer {
        socket,
        stop,
        handle: Some(handle),
    }
}

impl Drop for RawServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The id of a raw request line.
pub(crate) fn line_id(line: &[u8]) -> String {
    let value: Value = serde_json::from_slice(line).expect("a JSON request");
    value["id"].as_str().unwrap_or_default().to_owned()
}

/// Writes one JSON line to a raw connection.
pub(crate) fn send_line(stream: &UnixStream, value: &Value) {
    let _ = write_all_before(
        stream,
        format!("{value}\n").as_bytes(),
        Instant::now() + Duration::from_secs(5),
    );
}

// ------------------------------------------------------- collecting replies

/// A responder that records everything, for asserting on a request's life.
#[derive(Debug, Default, Clone)]
pub(crate) struct Collect {
    pub accepted: usize,
    pub progress: Vec<u64>,
    pub replies: Vec<Result<Value, CoreError>>,
}

impl super::client::Replies for Collect {
    fn accepted(&mut self) {
        self.accepted += 1;
    }

    fn progress(&mut self, p: &Progress) {
        self.progress.push(p.done);
    }

    fn reply(&mut self, r: Result<Value, CoreError>) {
        self.replies.push(r);
    }
}

impl Collect {
    /// The one reply.
    pub fn reply(&self) -> &Result<Value, CoreError> {
        assert_eq!(self.replies.len(), 1, "exactly one reply: {self:?}");
        &self.replies[0]
    }

    pub fn ok(&self) -> &Value {
        match self.reply() {
            Ok(value) => value,
            Err(e) => panic!("expected a success, got {e:?}"),
        }
    }

    pub fn err(&self) -> &CoreError {
        match self.reply() {
            Ok(value) => panic!("expected an error, got {value}"),
            Err(e) => e,
        }
    }
}

// ----------------------------------------------------------- fake session

/// What a [`FakeSession`] records, shared with the test.
#[derive(Clone, Default)]
pub(crate) struct FakeLog {
    file: Option<PathBuf>,
    memory: Arc<Mutex<Vec<Value>>>,
}

impl FakeLog {
    pub fn to_file(path: &Path) -> Self {
        Self {
            file: Some(path.to_path_buf()),
            memory: Arc::default(),
        }
    }

    pub fn push(&self, mut event: Value) {
        event["pid"] = json!(std::process::id());
        if let Some(path) = &self.file {
            log_event(path, &event);
        }
        self.memory.lock().unwrap().push(event);
    }

    /// Events recorded in this process.
    pub fn events(&self) -> Vec<Value> {
        self.memory.lock().unwrap().clone()
    }

    /// The `event` names recorded in this process, in order.
    pub fn names(&self) -> Vec<String> {
        self.events()
            .iter()
            .filter_map(|e| e["event"].as_str().map(str::to_owned))
            .collect()
    }
}

/// A Session stand-in for hosts. It answers `hello` from its state, runs
/// commands through `qrec_core::apply` (timeline, scene and history work),
/// and fakes the slow parts:
///
/// - `Export { out: Some("ms:N") }` is an N ms job with five progress steps
///   (`"ms:N:sleep"` also runs `sleep 60` in its own process group);
///   `ExportCancel` and `Msg::Interrupt` cancel jobs (unless `stubborn`);
/// - `RecordStart`/`RecordToggle` record until `RecordStop` (waiters with
///   `wait` get the finalized reply).
///
/// `done` is evaluated only after a handled message — the strictest reading
/// of `run_until` — so hosts must nudge it; after 30 s without a message it
/// panics.
pub(crate) struct FakeSession {
    state: State,
    inbox: Inbox,
    rx: Receiver<Msg>,
    dir: String,
    log: FakeLog,
    next_req: u64,
    jobs: Vec<FakeJob>,
    record_waiters: Vec<Box<dyn Responder>>,
    stubborn: bool,
}

struct FakeJob {
    id: JobId,
    to: Box<dyn Responder>,
    child: Option<Child>,
}

impl FakeSession {
    pub fn new(dir: &Path, host: HostKind, log: FakeLog, stubborn: bool) -> Self {
        let project = Project::new("fake", Utc::now(), &ProjectDefaults::default());
        let (inbox, rx) = Inbox::channel();
        Self {
            state: State::new(project, History::new(), UiState::default(), host),
            inbox,
            rx,
            dir: dir.to_string_lossy().into_owned(),
            log,
            next_req: 0,
            jobs: Vec::new(),
            record_waiters: Vec::new(),
            stubborn,
        }
    }

    fn hello(&self) -> HelloInfo {
        HelloInfo {
            pid: std::process::id(),
            project_dir: self.dir.clone(),
            host: self.state.runtime.host,
            record: self.state.runtime.record.clone(),
            jobs: self.state.runtime.jobs.values().cloned().collect(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Request { req, mut to } => {
                self.next_req += 1;
                to.accepted();
                let body = serde_json::to_value(&req.body).unwrap();
                let is_nudge = matches!(req.body, WireBody::Query(Query::Status));
                if !is_nudge {
                    self.log.push(json!({ "event": "request", "body": body }));
                }
                match req.body {
                    WireBody::Hello => to.reply(Ok(serde_json::to_value(self.hello()).unwrap())),
                    WireBody::Shutdown => {
                        to.reply(Err(CoreError::internal("shutdown reached the session")));
                    }
                    WireBody::Query(Query::Status) => to.reply(Ok(json!({ "fake": true }))),
                    WireBody::Query(query) => to.reply(Ok(json!({
                        "query": format!("{query:?}"),
                        "handledBy": std::process::id(),
                    }))),
                    WireBody::Command(cmd) => self.command(cmd, req.confirmed, to),
                }
            }
            Msg::Event(event) => self.event(event),
            Msg::Interrupt => {
                self.log.push(json!({ "event": "interrupt" }));
                if !self.stubborn {
                    self.cancel_jobs("interrupted");
                }
            }
            Msg::Shutdown => self.log.push(json!({ "event": "shutdown" })),
            Msg::DiskChanged => self.log.push(json!({ "event": "diskChanged" })),
        }
    }

    fn handled(value: Value) -> Value {
        let mut value = value;
        value["handledBy"] = json!(std::process::id());
        value
    }

    fn command(&mut self, cmd: CoreCommand, confirmed: bool, mut to: Box<dyn Responder>) {
        let recording = !self.state.runtime.record.is_idle();
        match cmd {
            CoreCommand::RecordStart { wait, .. } | CoreCommand::RecordToggle { wait } if !recording => {
                self.state.runtime.record = RecordPhase::Recording {
                    take: TakeId::numbered(1),
                    mode: RecordMode::Append,
                    elapsed_ns: 0,
                    waiters: Vec::new(),
                };
                self.log.push(json!({ "event": "recording" }));
                if wait {
                    self.record_waiters.push(to);
                } else {
                    to.reply(Ok(Self::handled(json!({ "recording": true }))));
                }
            }
            CoreCommand::RecordStop | CoreCommand::RecordToggle { .. } if recording => {
                self.state.runtime.record = RecordPhase::Idle;
                self.log.push(json!({ "event": "finalized" }));
                let done = Self::handled(json!({ "finalized": true }));
                for mut waiter in self.record_waiters.drain(..) {
                    waiter.reply(Ok(done.clone()));
                }
                to.reply(Ok(done));
            }
            CoreCommand::RecordStart { .. } => to.reply(Err(CoreError::busy("already recording"))),
            CoreCommand::RecordStop | CoreCommand::RecordCancel => {
                to.reply(Err(CoreError::invalid("not recording")));
            }
            CoreCommand::Export { out: Some(spec), .. } if spec.starts_with("ms:") => {
                self.start_job(&spec, to);
            }
            CoreCommand::ExportCancel { .. } => {
                self.cancel_jobs("cancelled");
                to.reply(Ok(Self::handled(json!({ "cancelled": true }))));
            }
            other => {
                let ctx = Ctx {
                    now: Utc::now(),
                    utc_offset_secs: 0,
                    entropy: self.next_req,
                    confirmed,
                    req: Some(ReqId(self.next_req.to_string())),
                    facts: Facts::default(),
                };
                match qrec_core::apply(&self.state, &ctx, other) {
                    Ok(outcome) => {
                        self.state = outcome.state;
                        match outcome.reply {
                            Some(reply) => to.reply(
                                reply.map(|r| Self::handled(serde_json::to_value(r).unwrap())),
                            ),
                            None => to.reply(Err(CoreError::internal(
                                "the fake session can't run async commands",
                            ))),
                        }
                    }
                    Err(e) => to.reply(Err(e)),
                }
            }
        }
    }

    fn start_job(&mut self, spec: &str, to: Box<dyn Responder>) {
        let mut parts = spec.split(':').skip(1);
        let ms: u64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(300);
        let child = (parts.next() == Some("sleep")).then(|| {
            let child = Command::new("sleep")
                .arg("60")
                .process_group(0)
                .spawn()
                .expect("spawn sleep");
            self.log
                .push(json!({ "event": "child", "child": child.id() }));
            child
        });
        let id = self.state.runtime.alloc_job_id();
        self.state.runtime.jobs.insert(
            id,
            Job {
                id,
                kind: JobKind::Purge { takes: Vec::new() },
                req: Some(ReqId(self.next_req.to_string())),
                started: Utc::now(),
            },
        );
        self.log.push(json!({ "event": "job", "job": id.0, "ms": ms }));
        let inbox = self.inbox.clone();
        thread::spawn(move || {
            for step in 1..=5 {
                thread::sleep(Duration::from_millis(ms / 5));
                if !inbox.event(Event::ExportProgress {
                    job: id,
                    segment: 0,
                    frames_done: step,
                }) {
                    return;
                }
            }
            inbox.event(Event::ExportFinished {
                job: id,
                path: "/fake.mp4".into(),
                bytes: 1,
            });
        });
        self.jobs.push(FakeJob { id, to, child });
    }

    fn cancel_jobs(&mut self, why: &str) {
        for mut job in self.jobs.drain(..) {
            self.state.runtime.jobs.remove(&job.id);
            if let Some(mut child) = job.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            job.to.reply(Err(CoreError::io(why)));
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::ExportProgress {
                job, frames_done, ..
            } => {
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == job) {
                    job.to.progress(&Progress {
                        phase: ProgressPhase::Render,
                        done: frames_done,
                        total: 5,
                        eta_secs: None,
                        label: "fake".into(),
                        take: None,
                    });
                }
            }
            Event::ExportFinished { job, .. } => {
                if let Some(pos) = self.jobs.iter().position(|j| j.id == job) {
                    let mut job = self.jobs.remove(pos);
                    self.state.runtime.jobs.remove(&job.id);
                    if let Some(mut child) = job.child.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    self.log.push(json!({ "event": "jobDone", "job": job.id.0 }));
                    job.to.reply(Ok(Self::handled(json!({ "exported": true }))));
                }
            }
            other => self
                .log
                .push(json!({ "event": "event", "detail": format!("{other:?}") })),
        }
    }
}

impl HostSession for FakeSession {
    fn inbox(&self) -> Inbox {
        self.inbox.clone()
    }

    fn run_until(&mut self, done: &mut dyn FnMut(&State) -> bool) {
        loop {
            match self.rx.recv_timeout(Duration::from_secs(30)) {
                Ok(msg) => {
                    self.handle(msg);
                    if done(&self.state) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("fake session: no message for 30 s (did the host forget to nudge?)")
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn state(&self) -> &State {
        &self.state
    }
}

/// Opens [`FakeSession`]s and records how.
#[derive(Clone, Default)]
pub(crate) struct FakeOpener {
    pub log: FakeLog,
    pub opened: Arc<Mutex<Vec<OpenOpts>>>,
    pub fail: Option<CoreError>,
    pub stubborn: bool,
    /// How long opening takes (a slow recovery).
    pub delay: Duration,
}

impl FakeOpener {
    pub fn to_file(path: &Path) -> Self {
        Self {
            log: FakeLog::to_file(path),
            ..Self::default()
        }
    }
}

impl Opener for FakeOpener {
    fn open(&mut self, dir: &Path, opts: OpenOpts) -> Result<Box<dyn HostSession>, ShellError> {
        self.opened.lock().unwrap().push(opts);
        self.log.push(json!({
            "event": "open",
            "host": opts.host,
            "mutating": opts.mutating,
            "recovery": opts.recovery,
        }));
        thread::sleep(self.delay);
        if let Some(e) = &self.fail {
            return Err(ShellError::Core(e.clone()));
        }
        Ok(Box::new(FakeSession::new(
            dir,
            opts.host,
            self.log.clone(),
            self.stubborn,
        )))
    }
}

/// Short exit rules for test hosts, so a failed test never leaves one
/// running for long.
pub(crate) fn quick_rules() -> ExitRules {
    ExitRules {
        idle_grace: Duration::from_millis(300),
        unused_timeout: Duration::from_secs(5),
        signal_grace: Duration::from_secs(3),
        flush_timeout: Duration::from_secs(2),
    }
}

// ------------------------------------------------------------ child roles

pub(crate) const ROLE: &str = "QREC_IPC_TEST_ROLE";
pub(crate) const LOG: &str = "QREC_IPC_TEST_LOG";
pub(crate) const RULES: &str = "QREC_IPC_TEST_RULES";
pub(crate) const REQUEST: &str = "QREC_IPC_TEST_REQUEST";
pub(crate) const PROJECT: &str = "QREC_IPC_TEST_PROJECT";
pub(crate) const STUBBORN: &str = "QREC_IPC_TEST_STUBBORN";

/// [`child_entry`]'s full name, for `--exact`.
const CHILD_TEST: &str = "ipc::testing::child_entry";

/// libtest arguments that run only [`child_entry`]; anything after them
/// (after `--`) is ignored by the harness and read by the role.
fn child_args() -> Vec<&'static str> {
    vec!["--exact", CHILD_TEST, "--nocapture", "--test-threads=1", "--"]
}

/// This test binary as a child playing `role`, in `env`'s isolation.
pub(crate) fn child_command(env: &TestEnv, role: &str, log: &Path) -> Command {
    let mut command = env.command(&std::env::current_exe().expect("the test binary"));
    command.args(child_args()).env(ROLE, role).env(LOG, log);
    command
}

/// `idle_ms,unused_ms,signal_ms` for [`RULES`].
pub(crate) fn rules_env(rules: &ExitRules) -> String {
    format!(
        "{},{},{}",
        rules.idle_grace.as_millis(),
        rules.unused_timeout.as_millis(),
        rules.signal_grace.as_millis()
    )
}

fn rules_from_env() -> ExitRules {
    let Ok(text) = std::env::var(RULES) else {
        return quick_rules();
    };
    let ms: Vec<u64> = text.split(',').filter_map(|p| p.parse().ok()).collect();
    match ms.as_slice() {
        [idle, unused, signal] => ExitRules {
            idle_grace: Duration::from_millis(*idle),
            unused_timeout: Duration::from_millis(*unused),
            signal_grace: Duration::from_millis(*signal),
            flush_timeout: Duration::from_secs(2),
        },
        _ => quick_rules(),
    }
}

/// What a `cli` child spawns for `record start`: this binary as a headless
/// child logging to `log`.
pub(crate) fn test_host_program(log: &Path, rules: &str) -> HostProgram {
    HostProgram {
        program: std::env::current_exe().expect("the test binary"),
        args: child_args().into_iter().map(Into::into).collect(),
        env: vec![
            (ROLE.into(), "headless".into()),
            (LOG.into(), log.into()),
            (RULES.into(), rules.into()),
        ],
    }
}

/// Re-executed copies of this test binary play a role chosen by
/// `QREC_IPC_TEST_ROLE` and exit with its code. Without the variable this
/// test does nothing.
#[test]
fn child_entry() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let log = PathBuf::from(std::env::var_os(LOG).expect("QREC_IPC_TEST_LOG"));
    let code = match role.as_str() {
        "headless" => child_headless(&log),
        "cli" => child_cli(&log),
        "spawner" => child_spawner(&log),
        other => panic!("unknown test role {other}"),
    };
    std::process::exit(i32::from(code));
}

/// Arguments after `--`: `--project DIR [--lock-fd N] [--ready-fd N]`.
fn host_args() -> HeadlessArgs {
    let args: Vec<String> = std::env::args().collect();
    let after = args.iter().position(|a| a == "--").map_or(&args[..0], |i| &args[i + 1..]);
    let value = |flag: &str| {
        after
            .iter()
            .position(|a| a == flag)
            .and_then(|i| after.get(i + 1))
            .cloned()
    };
    HeadlessArgs {
        project: PathBuf::from(value("--project").expect("--project")),
        lock_fd: value("--lock-fd").map(|v| v.parse().unwrap()),
        ready_fd: value("--ready-fd").map(|v| v.parse().unwrap()),
    }
}

fn child_headless(log: &Path) -> u8 {
    let args = host_args();
    let opts = HostOptions {
        rules: rules_from_env(),
        interrupts: signals::install(&HOST_SIGNALS).ok(),
        ..HostOptions::default()
    };
    let mut opener = FakeOpener::to_file(log);
    opener.stubborn = std::env::var_os(STUBBORN).is_some();
    opener.log.push(json!({ "event": "hostStart", "role": "headless" }));
    let code = run_headless(args, &Env::from_process(), &mut opener, opts);
    opener
        .log
        .push(json!({ "event": "hostExit", "role": "headless", "code": code }));
    code
}

fn child_cli(log: &Path) -> u8 {
    let interrupts = signals::install(&HOST_SIGNALS).expect("signal handlers");
    let body: WireBody =
        serde_json::from_str(&std::env::var(REQUEST).expect("QREC_IPC_TEST_REQUEST")).unwrap();
    let project = PathBuf::from(std::env::var_os(PROJECT).expect("QREC_IPC_TEST_PROJECT"));
    let env = Env::from_process();
    let cwd = std::env::current_dir().unwrap();
    let rules = rules_from_env();
    let program = test_host_program(log, &rules_env(&rules));
    let mut opener = FakeOpener::to_file(log);
    let mut notices = Vec::new();
    let mut out = Collect::default();
    let exit = run_cli(
        CliContext {
            env: &env,
            cwd: &cwd,
            project: Some(&project),
            opener: &mut opener,
            host_program: &program,
            interrupts: &interrupts,
            notice: &mut |n: &str| notices.push(n.to_owned()),
            rules,
            on_second_signal: None,
        },
        CliRequest {
            request: Request {
                body,
                confirmed: false,
            },
            recovery: false,
        },
        &mut out,
    );
    let reply = out.replies.first().map(|r| match r {
        Ok(v) => json!({ "ok": true, "data": v }),
        Err(e) => json!({ "ok": false, "error": e }),
    });
    opener.log.push(json!({
        "event": "cliDone",
        "role": "cli",
        "exit": format!("{exit:?}"),
        "reply": reply,
        "progress": out.progress,
        "notices": notices,
    }));
    match (exit, out.replies.first()) {
        (CliExit::Interrupted, _) => 130,
        (CliExit::Replied, Some(Ok(_))) => 0,
        (CliExit::Replied, Some(Err(e))) => e.exit_code(),
        (CliExit::Replied, None) => 99,
    }
}

/// Takes the lock, hands it to a spawned headless child and exits at once:
/// the host must keep the lock without its parent.
fn child_spawner(log: &Path) -> u8 {
    let project = PathBuf::from(std::env::var_os(PROJECT).expect("QREC_IPC_TEST_PROJECT"));
    let dir = fs::canonicalize(project).unwrap();
    let paths = ProjectPaths::new(&dir);
    let runtime = ensure_runtime_dir(&Env::from_process()).unwrap();
    let socket = socket_path(&runtime, &dir).unwrap();
    let lock = ProjectLock::try_acquire(&dir)
        .unwrap()
        .expect("the lock is free");
    let program = test_host_program(log, &rules_env(&rules_from_env()));
    let interrupts = Interrupts::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let fake_log = FakeLog::to_file(log);
    match spawn_headless(lock, &paths, &socket, &program, deadline, &interrupts, 0) {
        Ok(host) => {
            fake_log.push(json!({
                "event": "spawned",
                "role": "spawner",
                "host": host.pid,
                "log": host.log,
            }));
            0
        }
        Err(e) => {
            fake_log.push(json!({ "event": "spawnFailed", "error": format!("{e:?}") }));
            1
        }
    }
}
