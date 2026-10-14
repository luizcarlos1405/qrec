//! How the CLI runs one request (A§8.3 step 4, A§8.4, A§8.5). Owner: ipc.
//!
//! [`run_cli`] resolves the project, then routes:
//!
//! - a live host answers `hello` ⇒ the request goes there over IPC;
//! - no host, a query ⇒ it runs in-process on a read-only Session (no lock,
//!   nothing written);
//! - no host, `record start|toggle` ⇒ the CLI takes the lock, hands it to a
//!   spawned headless host (A§8.3 step 5) and sends the request to it;
//! - no host, any other command ⇒ the CLI takes the lock and **is** the host
//!   (`HostKind::Cli`) until its command is done: short commands finish at
//!   once, async ones keep serving IPC so other commands run against the
//!   same Session (SPEC §13.1);
//! - no host for GUI-only commands, `--over-selection`, `hello` or
//!   `shutdown` ⇒ `Unavailable`.
//!
//! The caller installs signal handlers first (`signals::install(&HOST_SIGNALS)`)
//! and passes the counter: Ctrl+C on an IPC request closes the connection
//! (exit 130; for `record … --wait` the first one stops the recording and
//! the second detaches); in an in-process host the first cancels jobs and
//! the second kills child processes and exits 130.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use qrec_core::protocol::{Command, HelloInfo, Progress, RecordModeArg, WireBody};
use qrec_core::state::HostKind;
use qrec_core::CoreError;
use serde_json::Value;

use super::signals::Interrupts;
use super::spawn::{HostProgram, SpawnError, SpawnedHost, spawn_headless};
use super::{ExitRules, Host, HostOptions, Opener, find_host_log};
use crate::deps::OpenOpts;
use crate::ipc::client::{
    RouteError, RouteMode, Routed, Router, SendError, SendOpts, Sent, route, send_request,
    stops_on_interrupt,
};
use crate::ipc::lock_glue::ProjectLock;
use crate::ipc::resolve::{Folder, classify, resolve_project};
use crate::ipc::{RECORD_ROUTE_DEADLINE, ROUTE_DEADLINE};
use crate::msg::{Msg, Request, Responder};
use crate::paths::{Env, ProjectPaths, UserDirs, ensure_runtime_dir, socket_path};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Everything [`run_cli`] needs besides the request.
pub struct CliContext<'a> {
    /// For the runtime dir (sockets) and the GUI's session log.
    pub env: &'a Env,
    /// The CLI's working directory (absolute).
    pub cwd: &'a Path,
    /// `--project DIR`.
    pub project: Option<&'a Path>,
    /// Opens the Session when this process runs the request itself.
    pub opener: &'a mut dyn Opener,
    /// What `record start|toggle` spawns ([`HostProgram::current_exe`]).
    pub host_program: &'a HostProgram,
    /// Ctrl+C / SIGTERM / SIGHUP, from `signals::install`.
    pub interrupts: &'a Interrupts,
    /// Lines for stderr (`qrec: using project <dir> (pid N)`, version notes).
    pub notice: &'a mut dyn FnMut(&str),
    /// Exit rules of an in-process host.
    pub rules: ExitRules,
    /// The second signal in an in-process host; `None` kills the child
    /// process groups and exits 130.
    pub on_second_signal: Option<Box<dyn FnOnce() + Send>>,
}

/// One request from the command line.
#[derive(Debug, Clone, PartialEq)]
pub struct CliRequest {
    pub request: Request,
    /// `qrec history` / `history restore`: an in-process Session opens in
    /// recovery mode (A§7.7).
    pub recovery: bool,
}

/// How [`run_cli`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliExit {
    /// The responder got exactly one reply (a result or the error that
    /// stopped the request); the exit code follows from it.
    Replied,
    /// Interrupted (exit 130). The responder may have seen a reply.
    Interrupted,
}

/// Runs one request: resolve the project, route it, stream the outcome to
/// `out` (A§8.3, A§8.4, A§8.5).
pub fn run_cli(mut cx: CliContext<'_>, req: CliRequest, out: &mut dyn Responder) -> CliExit {
    let baseline = cx.interrupts.count();
    let plan = Plan::of(&req.request);
    let runtime = match ensure_runtime_dir(cx.env) {
        Ok(runtime) => runtime,
        Err(e) => return fail(out, e),
    };
    let project = match resolve_project(cx.project, cx.cwd, &runtime) {
        Ok(project) => project,
        Err(e) => return fail(out, e),
    };
    if let Some(notice) = project.notice() {
        (cx.notice)(&notice);
    }
    let paths = ProjectPaths::new(&project.dir);
    let socket = match socket_path(&runtime, &project.dir) {
        Ok(socket) => socket,
        Err(e) => return fail(out, e),
    };
    let mut deadline = Instant::now() + plan.deadline;
    let mut rerouted = false;
    let mut spawned: Option<SpawnedHost> = None;
    loop {
        let mut router = Router::new(&paths, &socket, plan.route_mode(), deadline, cx.interrupts);
        router.baseline = baseline;
        match route(&router) {
            Err(RouteError::Interrupted) => return CliExit::Interrupted,
            Err(RouteError::Failed(e)) => return fail(out, e),
            Ok(Routed::Host {
                socket: host_socket,
                hello,
            }) => {
                if let Some(hello) = &hello
                    && hello.version != VERSION
                {
                    (cx.notice)(&format!(
                        "qrec: the running {} host (pid {}) is qrec {}; this is qrec {VERSION}",
                        hello.host.as_str(),
                        hello.pid,
                        hello.version
                    ));
                }
                let mut opts = SendOpts::new(cx.interrupts);
                opts.baseline = baseline;
                opts.stop_on_interrupt = stops_on_interrupt(&req.request);
                match send_request(&host_socket, &req.request, out, &opts) {
                    Ok(Sent::Replied) => return CliExit::Replied,
                    Ok(Sent::Interrupted) => return CliExit::Interrupted,
                    // EOF before `accepted`: the request never ran; route once more.
                    Err(SendError::NotAccepted) if !rerouted => {
                        rerouted = true;
                        deadline = Instant::now() + plan.deadline;
                    }
                    Err(e) => {
                        let log = host_log(&paths, cx.env, hello.as_deref(), spawned.as_ref());
                        return fail(out, send_failure(e, hello.as_deref(), log));
                    }
                }
            }
            Ok(Routed::NoHost) => {
                return match plan.kind {
                    PlanKind::ReadOnly => run_read_only(&mut cx, &project.dir, &req, out, baseline),
                    PlanKind::NeedsHost(why) => fail(out, CoreError::unavailable(why)),
                    PlanKind::InProcess | PlanKind::Spawn => fail(
                        out,
                        CoreError::internal("routing returned no host without taking the lock"),
                    ),
                };
            }
            Ok(Routed::Locked(lock)) => match plan.kind {
                PlanKind::Spawn if spawned.is_none() => {
                    match spawn_headless(
                        lock,
                        &paths,
                        &socket,
                        cx.host_program,
                        deadline,
                        cx.interrupts,
                        baseline,
                    ) {
                        Ok(host) => {
                            spawned = Some(host);
                            deadline = deadline.max(Instant::now() + ROUTE_DEADLINE);
                        }
                        Err(SpawnError::Interrupted) => return CliExit::Interrupted,
                        Err(SpawnError::Failed(e)) => return fail(out, e),
                    }
                }
                PlanKind::Spawn => {
                    drop(lock);
                    let mut err = CoreError::io("the background host exited right after starting");
                    if let Some(host) = &spawned {
                        err = err.with_hint(format!("see {}", host.log.display()));
                    }
                    return fail(out, err);
                }
                PlanKind::InProcess => return run_in_process(&mut cx, lock, &paths, &socket, &req, out),
                PlanKind::ReadOnly | PlanKind::NeedsHost(_) => {
                    drop(lock);
                    return fail(
                        out,
                        CoreError::internal("routing took the lock for a request that never does"),
                    );
                }
            },
        }
    }
}

/// What the routing does with a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanKind {
    /// Queries: a host, else in-process without the lock.
    ReadOnly,
    /// Commands: a host, else this process becomes one.
    InProcess,
    /// `record start|toggle`: a host, else a spawned headless one.
    Spawn,
    /// Needs a running host; the message for `Unavailable` otherwise.
    NeedsHost(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct Plan {
    kind: PlanKind,
    deadline: std::time::Duration,
}

impl Plan {
    fn of(req: &Request) -> Plan {
        let cmd = match &req.body {
            WireBody::Query(_) => {
                return Plan {
                    kind: PlanKind::ReadOnly,
                    deadline: ROUTE_DEADLINE,
                };
            }
            WireBody::Hello | WireBody::Shutdown => {
                return Plan {
                    kind: PlanKind::NeedsHost("no Qrec host is running for this project"),
                    deadline: ROUTE_DEADLINE,
                };
            }
            WireBody::Command(cmd) => cmd,
        };
        let record = matches!(
            cmd,
            Command::RecordStart { .. }
                | Command::RecordStop
                | Command::RecordCancel
                | Command::RecordToggle { .. }
        );
        let kind = if cmd.gui_only() {
            PlanKind::NeedsHost("this command needs the Qrec GUI, and it isn't running for this project")
        } else if matches!(
            cmd,
            Command::RecordStart {
                mode: RecordModeArg::OverSelection,
                ..
            }
        ) {
            PlanKind::NeedsHost("--over-selection needs clips selected in a running Qrec GUI")
        } else if matches!(cmd, Command::RecordStart { .. } | Command::RecordToggle { .. }) {
            PlanKind::Spawn
        } else {
            PlanKind::InProcess
        };
        Plan {
            kind,
            deadline: if record {
                RECORD_ROUTE_DEADLINE
            } else {
                ROUTE_DEADLINE
            },
        }
    }

    fn route_mode(&self) -> RouteMode {
        match self.kind {
            PlanKind::InProcess | PlanKind::Spawn => RouteMode::Lock,
            PlanKind::ReadOnly | PlanKind::NeedsHost(_) => RouteMode::NoLock,
        }
    }
}

/// Reports `err` as the request's reply.
fn fail(out: &mut dyn Responder, err: CoreError) -> CliExit {
    out.reply(Err(err));
    CliExit::Replied
}

/// A query with no host: a read-only Session, no lock (A§8.4).
fn run_read_only(
    cx: &mut CliContext<'_>,
    dir: &Path,
    req: &CliRequest,
    out: &mut dyn Responder,
    baseline: usize,
) -> CliExit {
    let opts = OpenOpts {
        host: HostKind::Cli,
        mutating: false,
        recovery: req.recovery,
    };
    let mut session = match cx.opener.open(dir, opts) {
        Ok(session) => session,
        Err(e) => return fail(out, e.to_core()),
    };
    let (mut own, responder) = OwnRequest::new();
    if !session.inbox().send(Msg::Request {
        req: req.request.clone(),
        to: Box::new(responder),
    }) {
        return fail(out, CoreError::internal("the Session isn't running"));
    }
    let interrupts = cx.interrupts.clone();
    session.run_until(&mut |_state| own.forward(out) || interrupts.count() > baseline);
    if own.forward(out) {
        CliExit::Replied
    } else if interrupts.count() > baseline {
        CliExit::Interrupted
    } else {
        fail(out, CoreError::internal("the Session stopped before answering"))
    }
}

/// No host and a command: this process becomes the host (A§8.5).
fn run_in_process(
    cx: &mut CliContext<'_>,
    lock: ProjectLock,
    paths: &ProjectPaths,
    socket: &Path,
    req: &CliRequest,
    out: &mut dyn Responder,
) -> CliExit {
    let dir = paths.root();
    // A§8.4 init order: the folder rule again, under the lock (`.qrec/`
    // alone counts as empty).
    match classify(dir, true) {
        Ok(Folder::Project | Folder::Empty) => {}
        Ok(Folder::Other) => return fail(out, CoreError::not_project()),
        Err(e) => {
            return fail(
                out,
                CoreError::io(format!("can't read {}: {e}", dir.display())),
            );
        }
    }
    let opts = HostOptions {
        rules: cx.rules,
        interrupts: Some(cx.interrupts.clone()),
        on_second_signal: cx.on_second_signal.take(),
    };
    let host = match Host::start(HostKind::Cli, paths.clone(), lock, socket, opts) {
        Ok(host) => host,
        Err(e) => return fail(out, e),
    };
    let open = OpenOpts {
        host: HostKind::Cli,
        mutating: true,
        recovery: req.recovery,
    };
    let mut session = match cx.opener.open(dir, open) {
        Ok(session) => session,
        Err(e) => {
            let e = e.to_core();
            host.server.fail(e.clone());
            host.finish(None, e.clone());
            return fail(out, e);
        }
    };
    host.attach(session.inbox());
    let (mut own, responder) = OwnRequest::new();
    host.server.send(Msg::Request {
        req: req.request.clone(),
        to: Box::new(responder),
    });
    let end = {
        let mut forward = || own.forward(out);
        host.serve(&mut *session, Some(&mut forward))
    };
    let signalled = host.signalled() || end.signalled();
    host.finish(
        Some(session),
        CoreError::io("the qrec command hosting this project is exiting"),
    );
    let replied = own.forward(out);
    if signalled {
        CliExit::Interrupted
    } else if replied {
        CliExit::Replied
    } else {
        fail(out, CoreError::io("the host stopped before answering"))
    }
}

/// The in-process request's events, carried from the session thread's
/// responder to the caller's (which needn't be `Send` or `'static`).
struct OwnRequest {
    rx: Receiver<OwnEvent>,
    replied: bool,
}

enum OwnEvent {
    Accepted,
    Progress(Progress),
    Reply(Result<Value, CoreError>),
}

struct ProxyResponder {
    tx: Sender<OwnEvent>,
    replied: bool,
}

impl OwnRequest {
    fn new() -> (OwnRequest, ProxyResponder) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (
            OwnRequest { rx, replied: false },
            ProxyResponder { tx, replied: false },
        )
    }

    /// Forwards what arrived; `true` once the reply was forwarded.
    fn forward(&mut self, out: &mut dyn Responder) -> bool {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                OwnEvent::Accepted => out.accepted(),
                OwnEvent::Progress(p) => out.progress(&p),
                OwnEvent::Reply(r) if !self.replied => {
                    self.replied = true;
                    out.reply(r);
                }
                OwnEvent::Reply(_) => {}
            }
        }
        self.replied
    }
}

impl Responder for ProxyResponder {
    fn accepted(&mut self) {
        let _ = self.tx.send(OwnEvent::Accepted);
    }

    fn progress(&mut self, p: &Progress) {
        let _ = self.tx.send(OwnEvent::Progress(p.clone()));
    }

    fn reply(&mut self, r: Result<Value, CoreError>) {
        if !self.replied {
            self.replied = true;
            let _ = self.tx.send(OwnEvent::Reply(r));
        }
    }
}

impl Drop for ProxyResponder {
    fn drop(&mut self) {
        if !self.replied {
            let _ = self.tx.send(OwnEvent::Reply(Err(CoreError::io(
                "the Session dropped the request without an answer",
            ))));
        }
    }
}

/// The error for a request that got no reply over IPC (A§8.3 step 6).
fn send_failure(e: SendError, hello: Option<&HelloInfo>, log: Option<PathBuf>) -> CoreError {
    let who = match hello {
        Some(h) => format!("the {} host (pid {})", h.host.as_str(), h.pid),
        None => "the host".to_owned(),
    };
    let see_log = |hint: &str| match &log {
        Some(log) => format!("{hint}; see {}", log.display()),
        None => hint.to_owned(),
    };
    match e {
        SendError::NotAccepted => {
            CoreError::busy(format!("{who} closed the connection before accepting the request"))
                .with_hint("it is probably shutting down; run the command again")
        }
        SendError::NotResponding => CoreError::busy(format!("{who} is not responding"))
            .with_hint(see_log("it may be stuck; the request may still run")),
        SendError::Disconnected => CoreError::io("host disconnected")
            .with_hint(see_log("the request may have run; check with `qrec status`")),
        SendError::LostContact => {
            CoreError::busy(format!("{who} stopped responding while running the request"))
                .with_hint(see_log("it may still finish; check with `qrec status`"))
        }
        SendError::Protocol(msg) => CoreError::io(format!("unexpected answer from {who}: {msg}")),
    }
}

/// Where a host logs: the one this CLI spawned, a headless host's
/// `.qrec/logs/…-<pid>-host.log`, or the GUI's session log.
fn host_log(
    paths: &ProjectPaths,
    env: &Env,
    hello: Option<&HelloInfo>,
    spawned: Option<&SpawnedHost>,
) -> Option<PathBuf> {
    if let Some(spawned) = spawned
        && hello.is_none_or(|h| h.pid == spawned.pid)
    {
        return Some(spawned.log.clone());
    }
    let hello = hello?;
    match hello.host {
        HostKind::Headless => find_host_log(paths, hello.pid),
        HostKind::Gui => UserDirs::resolve(env).ok().map(|d| d.last_session_log()),
        HostKind::Cli => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrec_core::protocol::Query;

    fn command(cmd: Command) -> Request {
        Request::command(cmd, false)
    }

    #[test]
    fn plans() {
        let plan = Plan::of(&Request::query(Query::ClipList));
        assert_eq!((plan.kind, plan.deadline), (PlanKind::ReadOnly, ROUTE_DEADLINE));
        assert_eq!(plan.route_mode(), RouteMode::NoLock);

        let plan = Plan::of(&command(Command::Undo { steps: 1 }));
        assert_eq!((plan.kind, plan.deadline), (PlanKind::InProcess, ROUTE_DEADLINE));
        assert_eq!(plan.route_mode(), RouteMode::Lock);

        let start = Command::RecordStart {
            mode: RecordModeArg::Append,
            countdown: None,
            wait: false,
        };
        let plan = Plan::of(&command(start));
        assert_eq!((plan.kind, plan.deadline), (PlanKind::Spawn, RECORD_ROUTE_DEADLINE));
        let plan = Plan::of(&command(Command::RecordToggle { wait: true }));
        assert_eq!(plan.kind, PlanKind::Spawn);
        let plan = Plan::of(&command(Command::RecordStop));
        assert_eq!((plan.kind, plan.deadline), (PlanKind::InProcess, RECORD_ROUTE_DEADLINE));

        let over_selection = Command::RecordStart {
            mode: RecordModeArg::OverSelection,
            countdown: None,
            wait: false,
        };
        assert!(matches!(
            Plan::of(&command(over_selection)).kind,
            PlanKind::NeedsHost(_)
        ));
        assert!(matches!(Plan::of(&command(Command::Play)).kind, PlanKind::NeedsHost(_)));
        let hello = Request {
            body: WireBody::Hello,
            confirmed: false,
        };
        assert_eq!(Plan::of(&hello).route_mode(), RouteMode::NoLock);
    }

    #[test]
    fn failures_name_the_host_and_its_log() {
        let hello = HelloInfo {
            pid: 42,
            project_dir: "/p".into(),
            host: HostKind::Headless,
            record: qrec_core::RecordPhase::Idle,
            jobs: vec![],
            version: VERSION.into(),
        };
        let e = send_failure(
            SendError::Disconnected,
            Some(&hello),
            Some(PathBuf::from("/p/.qrec/logs/x-42-host.log")),
        );
        assert_eq!(e.code, qrec_core::ErrorCode::Io);
        assert_eq!(e.message, "host disconnected");
        assert!(e.hint.unwrap().ends_with("see /p/.qrec/logs/x-42-host.log"));
        let e = send_failure(SendError::NotResponding, Some(&hello), None);
        assert_eq!(e.code, qrec_core::ErrorCode::Busy);
        assert_eq!(e.message, "the headless host (pid 42) is not responding");
    }

    #[test]
    fn own_request_forwards_once() {
        struct Collect(Vec<String>);
        impl Responder for Collect {
            fn accepted(&mut self) {
                self.0.push("accepted".into());
            }
            fn progress(&mut self, p: &Progress) {
                self.0.push(format!("progress {}", p.done));
            }
            fn reply(&mut self, r: Result<Value, CoreError>) {
                self.0.push(format!("reply {}", r.is_ok()));
            }
        }
        let (mut own, mut responder) = OwnRequest::new();
        let mut out = Collect(Vec::new());
        assert!(!own.forward(&mut out));
        responder.accepted();
        responder.reply(Ok(Value::Null));
        responder.reply(Err(CoreError::internal("again")));
        assert!(own.forward(&mut out));
        assert_eq!(out.0, ["accepted", "reply true"]);
        // Dropped without a reply: an error reply.
        let (mut own, responder) = OwnRequest::new();
        drop(responder);
        let mut out = Collect(Vec::new());
        assert!(own.forward(&mut out));
        assert_eq!(out.0, ["reply false"]);
    }
}
