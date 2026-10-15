//! How the CLI runs one request (A§8.3 step 4, A§8.5, A§8.6). Owner: ipc.
//!
//! [`request`] takes a resolved project folder and routes the request:
//!
//! - a live host answers `hello` ⇒ the request goes there over IPC;
//! - no host and [`Fallback::InProcess`] without the lock (queries) ⇒ a
//!   read-only Session in this process: no lock, nothing written (A§8.4);
//! - no host and [`Fallback::InProcess`] with the lock (commands) ⇒ this
//!   process takes the lock and **is** the host (`HostKind::Cli`) until its
//!   command is done: short commands finish at once, async ones keep serving
//!   IPC so other commands run against the same Session (A§8.5). Its reply
//!   is reported as soon as it comes ([`RequestEvents::replied`]); then the
//!   host takes no new job and exits once what it still runs for others is
//!   over. It never takes a recording: a client that finds such a host
//!   doesn't send one, and waits for the host to go, then spawns a headless
//!   one — unless the host's `hello` lists an export: recording is blocked
//!   during an export (SPEC §13.1), so the client answers at once with the
//!   core's `Busy` (`export_blocks_recording`), as any host would;
//! - no host and [`Fallback::SpawnHeadless`] (`record start|toggle`) ⇒ take
//!   the lock, hand it to a spawned headless host (A§8.3 step 5), send the
//!   request there;
//! - no host and [`Fallback::None`] (GUI-only commands, `--over-selection`,
//!   `hello`, `shutdown`) ⇒ [`Finish::NoHost`].
//!
//! [`run_cli`] is the whole CLI path in one call: resolve the project
//! (A§8.4, `ipc::resolve`), pick the route options from the request
//! ([`RouteOpts::for_request`]) and run it.
//!
//! The caller installs signal handlers first (`signals::install(&HOST_SIGNALS)`)
//! and passes the counter: Ctrl+C on an IPC request closes the connection
//! (exit 130; for `record … --wait` the first one stops the recording and
//! the second detaches); in an in-process host the first cancels jobs, the
//! second kills child processes and ends the request as soon as its jobs
//! report it, so the caller still gets [`Finish::Interrupted`] (A§8.6).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use qrec_core::CoreError;
use qrec_core::apply::record::export_blocks_recording;
use qrec_core::protocol::{Command, HelloInfo, InsertAt, Progress, RecordModeArg, WireBody};
use qrec_core::state::HostKind;
use serde_json::Value;

use super::signals::Interrupts;
use super::spawn::{HostProgram, SpawnError, SpawnedHost, spawn_headless};
use super::{ExitRules, Host, HostOptions, Opener, find_host_log};
use crate::deps::OpenOpts;
use crate::ipc::client::{
    Replies, RouteError, RouteMode, Routed, Router, SendError, SendOpts, Sent, route, send_request,
    stops_on_interrupt,
};
use crate::ipc::resolve::{Folder, classify, resolve_project};
use crate::ipc::{RECORD_ROUTE_DEADLINE, ROUTE_DEADLINE, ROUTE_RETRY};
use crate::lock::ProjectLock;
use crate::msg::{Msg, Request, Responder};
use crate::paths::{Env, ProjectPaths, UserDirs, ensure_runtime_dir, socket_path};

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ------------------------------------------------------------ the port

/// What happens when no host serves the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// Run it in this process (A§8.5); with [`RouteOpts::lock`] this
    /// process becomes the project's `cli` host.
    InProcess,
    /// Take the lock, spawn `qrec host --headless` with it and send the
    /// request there (A§8.3 step 5): `record start` and `record toggle`.
    SpawnHeadless,
    /// Don't run it: [`Finish::NoHost`].
    None,
}

/// How a request is routed (A§8.3 step 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteOpts {
    /// How long to look for the host: 3 s, 20 s for `record *`.
    pub deadline: Duration,
    pub fallback: Fallback,
    /// In-process: take the lock and become the host (commands). `false`
    /// for queries, which take no lock and write nothing (A§8.4).
    pub lock: bool,
    /// Open the Session in recovery mode (`qrec history [restore]`, A§7.7).
    pub recovery: bool,
    /// `record … --wait`: the first Ctrl+C sends `RecordStop` and keeps
    /// streaming until the take is finalized; the second detaches (A§8.6).
    pub stop_on_interrupt: bool,
}

impl RouteOpts {
    /// The A§8.3–A§8.5 routing of `req`: queries in-process without the
    /// lock; commands in-process under it; `record start|toggle` spawn a
    /// headless host; GUI-only commands, `--over-selection`, `hello` and
    /// `shutdown` need a live host; 20 s deadlines for `record *`.
    pub fn for_request(req: &Request) -> RouteOpts {
        let query = RouteOpts {
            deadline: ROUTE_DEADLINE,
            fallback: Fallback::InProcess,
            lock: false,
            recovery: false,
            stop_on_interrupt: false,
        };
        let cmd = match &req.body {
            WireBody::Query(_) => return query,
            WireBody::Hello | WireBody::Shutdown => {
                return RouteOpts {
                    fallback: Fallback::None,
                    ..query
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
        // The live selection lives in a running host (A§8.4).
        let needs_selection = matches!(
            cmd,
            Command::RecordStart {
                mode: RecordModeArg::OverSelection,
                ..
            } | Command::ClipAdd {
                at: InsertAt::AfterSelection,
                ..
            }
        );
        let fallback = if cmd.gui_only() || needs_selection {
            Fallback::None
        } else if matches!(
            cmd,
            Command::RecordStart { .. } | Command::RecordToggle { .. }
        ) {
            Fallback::SpawnHeadless
        } else {
            Fallback::InProcess
        };
        RouteOpts {
            deadline: if record {
                RECORD_ROUTE_DEADLINE
            } else {
                ROUTE_DEADLINE
            },
            fallback,
            lock: true,
            recovery: false,
            stop_on_interrupt: stops_on_interrupt(req),
        }
    }

    fn route_mode(&self) -> RouteMode {
        match (self.fallback, self.lock) {
            (Fallback::SpawnHeadless, _) | (Fallback::InProcess, true) => RouteMode::Lock,
            (Fallback::InProcess, false) | (Fallback::None, _) => RouteMode::NoLock,
        }
    }
}

/// How a request ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Finish {
    /// The reply: a `CmdResult`, a query answer, or the error that stopped
    /// the request (including routing failures such as `busy`).
    Reply(Result<Value, CoreError>),
    /// [`Fallback::None`] and no host serves the project.
    NoHost,
    /// Ctrl+C (exit 130): in-process, jobs were cancelled; routed, the
    /// connection was closed (the host may still finish the request);
    /// `--wait`, the second Ctrl+C detached. `reply` is the answer that
    /// came anyway (an in-process command that finished or committed before
    /// the signal, or that failed because its jobs were cancelled), so the
    /// CLI can still report it.
    Interrupted {
        reply: Option<Result<Value, CoreError>>,
    },
}

/// What a request reports while it runs.
pub trait RequestEvents {
    /// The request reached a host: `Some(hello)` of a live host (`hello`
    /// may be missing if the host answered it with an error), `None` when
    /// it runs in this process.
    fn routed(&mut self, host: Option<&HelloInfo>);
    /// The host parsed and queued the request.
    fn accepted(&mut self);
    /// Progress of a long request (export, analysis, recording).
    fn progress(&mut self, progress: &Progress);
    /// The reply of a request this process runs itself, as soon as it came
    /// and before any signal (A§8.5): the process may go on hosting other
    /// requests for a while, and the person shouldn't wait for those to see
    /// their result. At most once; the same reply is in the [`Finish`].
    fn replied(&mut self, reply: &Result<Value, CoreError>);
}

/// What running a request needs besides the request.
pub struct RequestContext<'a> {
    /// For the runtime dir (sockets) and the GUI's session log.
    pub env: &'a Env,
    /// Opens the Session when this process runs the request itself.
    pub opener: &'a mut dyn Opener,
    /// What `record start|toggle` spawns ([`HostProgram::current_exe`]).
    pub host_program: &'a HostProgram,
    /// Ctrl+C / SIGTERM / SIGHUP, from `signals::install`.
    pub interrupts: &'a Interrupts,
    /// Exit rules of an in-process host.
    pub rules: ExitRules,
    /// The second signal in an in-process host; `None` kills the child
    /// process groups and ends the request as soon as its jobs report it
    /// (a watchdog exits 130 if that hangs, [`HostOptions::on_second_signal`]).
    pub on_second_signal: Option<Box<dyn FnOnce() + Send>>,
}

/// Sends one request for the project in `project` (a folder that holds
/// `qrec.json`, or an empty one), streaming what happens into `events`
/// (A§8.3–A§8.6).
pub fn request(
    mut cx: RequestContext<'_>,
    project: &Path,
    request: Request,
    opts: &RouteOpts,
    events: &mut dyn RequestEvents,
) -> Finish {
    let mut sink = EventsSink {
        events,
        reply: None,
        settled: false,
    };
    match run_request(&mut cx, project, &request, opts, &mut sink) {
        Ran::Replied => Finish::Reply(
            sink.reply
                .unwrap_or_else(|| Err(CoreError::internal("the request ended without a reply"))),
        ),
        Ran::NoHost => Finish::NoHost,
        Ran::Interrupted => Finish::Interrupted { reply: sink.reply },
    }
}

/// Where [`run_request`] reports: the request's responder, and which host
/// it reached.
trait Sink {
    fn routed(&mut self, host: Option<&HelloInfo>);
    fn out(&mut self) -> &mut dyn Replies;
    /// The reply `out` got is final and came before any signal, while this
    /// process may go on hosting others' requests: report it now.
    fn settled(&mut self) {}
}

/// [`request`]'s sink: events as they come, the reply kept for [`Finish`]
/// (and reported early once settled).
struct EventsSink<'e> {
    events: &'e mut dyn RequestEvents,
    reply: Option<Result<Value, CoreError>>,
    settled: bool,
}

impl Replies for EventsSink<'_> {
    fn accepted(&mut self) {
        self.events.accepted();
    }

    fn progress(&mut self, p: &Progress) {
        self.events.progress(p);
    }

    fn reply(&mut self, r: Result<Value, CoreError>) {
        self.reply.get_or_insert(r);
    }
}

impl Sink for EventsSink<'_> {
    fn routed(&mut self, host: Option<&HelloInfo>) {
        self.events.routed(host);
    }

    fn out(&mut self) -> &mut dyn Replies {
        self
    }

    fn settled(&mut self) {
        if let Some(reply) = self.reply.as_ref().filter(|_| !self.settled) {
            self.settled = true;
            self.events.replied(reply);
        }
    }
}

/// [`run_cli`]'s sink: the caller's responder, and a stderr note when the
/// host runs another Qrec version.
struct CliSink<'o, 'n> {
    out: &'o mut dyn Replies,
    notice: &'n mut dyn FnMut(&str),
}

impl Sink for CliSink<'_, '_> {
    fn routed(&mut self, host: Option<&HelloInfo>) {
        if let Some(hello) = host.filter(|h| h.version != VERSION) {
            (self.notice)(&format!(
                "qrec: the running {} host (pid {}) is qrec {}; this is qrec {VERSION}",
                hello.host.as_str(),
                hello.pid,
                hello.version
            ));
        }
    }

    fn out(&mut self) -> &mut dyn Replies {
        &mut *self.out
    }
}

// --------------------------------------------------------------- run_cli

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
    /// The second signal in an in-process host (see
    /// [`RequestContext::on_second_signal`]).
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

/// The whole CLI path: resolve the project (A§8.4), route the request with
/// [`RouteOpts::for_request`], stream the outcome to `out`. A request that
/// needs a live host and finds none is answered `Unavailable`.
pub fn run_cli(cx: CliContext<'_>, req: CliRequest, out: &mut dyn Replies) -> CliExit {
    let CliContext {
        env,
        cwd,
        project,
        opener,
        host_program,
        interrupts,
        notice,
        rules,
        on_second_signal,
    } = cx;
    let runtime = match ensure_runtime_dir(env) {
        Ok(runtime) => runtime,
        Err(e) => return fail(out, e),
    };
    let resolved = match resolve_project(project, cwd, &runtime) {
        Ok(resolved) => resolved,
        Err(e) => return fail(out, e),
    };
    if let Some(line) = resolved.notice() {
        notice(&line);
    }
    let mut opts = RouteOpts::for_request(&req.request);
    opts.recovery = req.recovery;
    let mut rcx = RequestContext {
        env,
        opener,
        host_program,
        interrupts,
        rules,
        on_second_signal,
    };
    let ran = {
        let mut sink = CliSink {
            out: &mut *out,
            notice,
        };
        run_request(&mut rcx, &resolved.dir, &req.request, &opts, &mut sink)
    };
    match ran {
        Ran::Replied => CliExit::Replied,
        Ran::Interrupted => CliExit::Interrupted,
        Ran::NoHost => fail(out, CoreError::unavailable(no_host_reason(&req.request))),
    }
}

/// The `Unavailable` message for a request that needs a live host.
fn no_host_reason(req: &Request) -> &'static str {
    match &req.body {
        WireBody::Command(cmd) if cmd.gui_only() => {
            "this command needs the Qrec GUI, and it isn't running for this project"
        }
        WireBody::Command(Command::RecordStart {
            mode: RecordModeArg::OverSelection,
            ..
        }) => "--over-selection needs clips selected in a running Qrec GUI",
        WireBody::Command(Command::ClipAdd {
            at: InsertAt::AfterSelection,
            ..
        }) => "--after-selection needs clips selected in a running Qrec GUI",
        _ => "no Qrec host is running for this project",
    }
}

/// Reports `err` as the request's reply.
fn fail(out: &mut dyn Replies, err: CoreError) -> CliExit {
    out.reply(Err(err));
    CliExit::Replied
}

// --------------------------------------------------------------- routing

/// How [`run_request`] ended.
enum Ran {
    /// `out` got exactly one reply.
    Replied,
    NoHost,
    Interrupted,
}

fn reply_err(out: &mut dyn Replies, err: CoreError) -> Ran {
    out.reply(Err(err));
    Ran::Replied
}

/// The routing loop around [`route`]: send to a host, or fall back.
fn run_request(
    cx: &mut RequestContext<'_>,
    project: &Path,
    req: &Request,
    opts: &RouteOpts,
    sink: &mut dyn Sink,
) -> Ran {
    let baseline = cx.interrupts.count();
    let runtime = match ensure_runtime_dir(cx.env) {
        Ok(runtime) => runtime,
        Err(e) => return reply_err(sink.out(), e),
    };
    // Canonical, so it maps to the one socket every client computes.
    let dir = match fs::canonicalize(project) {
        Ok(dir) => dir,
        Err(e) => {
            return reply_err(
                sink.out(),
                CoreError::not_project().with_hint(format!("{}: {e}", project.display())),
            );
        }
    };
    let paths = ProjectPaths::new(&dir);
    let socket = match socket_path(&runtime, &dir) {
        Ok(socket) => socket,
        Err(e) => return reply_err(sink.out(), e),
    };
    let mut deadline = Instant::now() + opts.deadline;
    let mut extended = false;
    let mut spawned: Option<SpawnedHost> = None;
    // Which host `sink.routed` last heard about (a re-route may find the same).
    let mut reported: Option<Option<u32>> = None;
    loop {
        let mut router = Router::new(&paths, &socket, opts.route_mode(), deadline, cx.interrupts);
        router.baseline = baseline;
        match route(&router) {
            Err(RouteError::Interrupted) => return Ran::Interrupted,
            Err(RouteError::Failed(e)) => return reply_err(sink.out(), e),
            Ok(Routed::Host {
                socket: host_socket,
                hello,
            }) => {
                // A CLI command's own host never records (A§8.5): don't
                // even ask it (it would bounce the request).
                let refused = match hello.as_deref() {
                    Some(cli) if cli.host == HostKind::Cli && starts_recording(req) => {
                        // Recording is blocked during an export, whichever
                        // process runs it (SPEC §13.1): the core's `Busy`,
                        // at once, as a GUI or headless host answers it
                        // (A§5.3). Waiting would only start an unannounced
                        // take once the export is over.
                        if let Some(busy) = export_blocks_recording(&cli.jobs) {
                            return reply_err(sink.out(), busy);
                        }
                        // A short command: wait for it to go like for a
                        // bounce, then start a headless host.
                        true
                    }
                    _ => false,
                };
                let sent = if refused {
                    Err(SendError::NotAccepted)
                } else {
                    let pid = hello.as_ref().map(|h| h.pid);
                    if reported != Some(pid) {
                        reported = Some(pid);
                        sink.routed(hello.as_deref());
                    }
                    let mut send = SendOpts::new(cx.interrupts);
                    send.baseline = baseline;
                    send.stop_on_interrupt = opts.stop_on_interrupt;
                    send_request(&host_socket, req, sink.out(), &send)
                };
                match sent {
                    Ok(Sent::Replied) => return Ran::Replied,
                    Ok(Sent::Interrupted) => return Ran::Interrupted,
                    // EOF before `accepted`: it never ran (the host stopped
                    // accepting as we connected, or it is a CLI command's
                    // own host, which bounces recordings while it runs no
                    // export, and new jobs once its command is done,
                    // A§8.5). Route again after a
                    // pause, like for a host that is starting or stopping,
                    // until the deadline (given once more from the first
                    // bounce) passes.
                    Err(SendError::NotAccepted) if Instant::now() < deadline || !extended => {
                        if !extended {
                            extended = true;
                            deadline = deadline.max(Instant::now() + opts.deadline);
                        }
                        let pause =
                            ROUTE_RETRY.min(deadline.saturating_duration_since(Instant::now()));
                        if cx.interrupts.wait_past(baseline, pause) > baseline {
                            return Ran::Interrupted;
                        }
                    }
                    Err(e) => {
                        if e == SendError::NotAccepted
                            && let Some(cli) = hello.as_deref().filter(|h| h.host == HostKind::Cli)
                        {
                            return reply_err(sink.out(), bounced_by_cli_host(cli, req));
                        }
                        let log = host_log(&paths, cx.env, hello.as_deref(), spawned.as_ref());
                        return reply_err(sink.out(), send_failure(e, hello.as_deref(), log));
                    }
                }
            }
            Ok(Routed::NoHost) => {
                return match opts.fallback {
                    Fallback::InProcess => {
                        sink.routed(None);
                        run_read_only(cx, &dir, req, opts.recovery, sink.out(), baseline)
                    }
                    Fallback::None => Ran::NoHost,
                    Fallback::SpawnHeadless => reply_err(
                        sink.out(),
                        CoreError::internal("routing returned no host without taking the lock"),
                    ),
                };
            }
            Ok(Routed::Locked(lock)) => match opts.fallback {
                Fallback::SpawnHeadless if spawned.is_none() => {
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
                        Err(SpawnError::Interrupted) => return Ran::Interrupted,
                        Err(SpawnError::Failed(e)) => return reply_err(sink.out(), e),
                    }
                }
                Fallback::SpawnHeadless => {
                    drop(lock);
                    // The host we spawned is gone. If its Session couldn't
                    // open (a corrupt qrec.json), it said why on the ready
                    // pipe before freeing the lock: that is the answer.
                    let err = match spawned.as_mut() {
                        Some(host) => {
                            let see = format!("see {}", host.log.display());
                            match host.open_error(Duration::from_millis(500)) {
                                Some(err) => {
                                    let hint = match err.hint.as_deref() {
                                        Some(hint) => format!("{hint}; {see}"),
                                        None => see,
                                    };
                                    err.with_hint(hint)
                                }
                                None => {
                                    CoreError::io("the background host exited right after starting")
                                        .with_hint(see)
                                }
                            }
                        }
                        None => CoreError::io("the background host exited right after starting"),
                    };
                    return reply_err(sink.out(), err);
                }
                Fallback::InProcess => {
                    sink.routed(None);
                    let in_process = InProcess {
                        paths: &paths,
                        socket: &socket,
                        recovery: opts.recovery,
                        baseline,
                    };
                    return run_in_process(cx, lock, &in_process, req, sink);
                }
                Fallback::None => {
                    drop(lock);
                    return reply_err(
                        sink.out(),
                        CoreError::internal("routing took the lock for a request that never does"),
                    );
                }
            },
        }
    }
}

/// `record start|toggle`: a request a CLI command's host never takes.
fn starts_recording(req: &Request) -> bool {
    matches!(&req.body, WireBody::Command(cmd) if cmd.may_start_recording())
}

/// A query with no host: a read-only Session, no lock (A§8.4).
fn run_read_only(
    cx: &mut RequestContext<'_>,
    dir: &Path,
    req: &Request,
    recovery: bool,
    out: &mut dyn Replies,
    baseline: usize,
) -> Ran {
    let opts = OpenOpts {
        host: HostKind::Cli,
        mutating: false,
        recovery,
    };
    let mut session = match cx.opener.open(dir, opts, None) {
        Ok(session) => session,
        Err(e) => return reply_err(out, e.to_core()),
    };
    let (mut own, responder) = OwnRequest::new(|| {});
    if !session.inbox().send(Msg::Request {
        req: req.clone(),
        to: Box::new(responder),
    }) {
        return reply_err(out, CoreError::internal("the Session isn't running"));
    }
    let interrupts = cx.interrupts.clone();
    session.run_until(&mut |_state| own.forward(out) || interrupts.count() > baseline);
    if own.forward(out) {
        Ran::Replied
    } else if interrupts.count() > baseline {
        Ran::Interrupted
    } else {
        reply_err(
            out,
            CoreError::internal("the Session stopped before answering"),
        )
    }
}

/// Where an in-process host lives and how it was asked for.
struct InProcess<'a> {
    paths: &'a ProjectPaths,
    socket: &'a Path,
    recovery: bool,
    /// Interrupts counted when the request started.
    baseline: usize,
}

/// No host and a command: this process becomes the host (A§8.5). Its own
/// reply is reported as soon as it comes (`Sink::settled`), before the
/// host is done serving others; from then on it takes no new job.
fn run_in_process(
    cx: &mut RequestContext<'_>,
    lock: ProjectLock,
    at: &InProcess<'_>,
    req: &Request,
    sink: &mut dyn Sink,
) -> Ran {
    let InProcess {
        paths,
        socket,
        recovery,
        baseline,
    } = *at;
    let dir = paths.root();
    // A§8.4 init order: the folder rule again, under the lock (`.qrec/`
    // alone counts as empty; with a history, it is a project that lost its
    // qrec.json: `history restore` rebuilds it, A§7.7).
    match classify(dir, true) {
        Ok(Folder::Project | Folder::MissingFile | Folder::Empty) => {}
        Ok(Folder::Other) => return reply_err(sink.out(), CoreError::not_project()),
        Err(e) => {
            return reply_err(
                sink.out(),
                CoreError::io(format!("can't read {}: {e}", dir.display())),
            );
        }
    }
    let opts = HostOptions {
        rules: cx.rules,
        interrupts: Some(cx.interrupts.clone()),
        signal_baseline: baseline,
        on_second_signal: cx.on_second_signal.take(),
    };
    let host = match Host::start(HostKind::Cli, paths.clone(), lock, socket, opts) {
        Ok(host) => host,
        Err(e) => return reply_err(sink.out(), e),
    };
    let open = OpenOpts {
        host: HostKind::Cli,
        mutating: true,
        recovery,
    };
    let opened = host
        .session_lock()
        .map_err(Into::into)
        .and_then(|lock| cx.opener.open(dir, open, Some(lock)));
    let mut session = match opened {
        Ok(session) => session,
        Err(e) => {
            let e = e.to_core();
            host.server.fail(e.clone());
            host.finish(None, e.clone());
            return reply_err(sink.out(), e);
        }
    };
    host.attach(session.inbox());
    // A§8.6: interrupted before it started (while the Session opened), the
    // command never runs; the host only winds down. Once its command is
    // answered the host takes no new job, from the moment of the reply (the
    // session thread sends it).
    let mut own = if host.signalled() {
        OwnRequest::unanswered()
    } else {
        let server = host.server.sender();
        let (own, responder) = OwnRequest::new(move || server.own_command_done());
        host.server.send(Msg::Request {
            req: req.clone(),
            to: Box::new(responder),
        });
        own
    };
    let interrupts = cx.interrupts.clone();
    let end = {
        let mut forward = || {
            let replied = own.forward(sink.out());
            // Answered before any signal: the person sees it now, not once
            // the host is done with requests from other clients (A§8.5).
            if replied && interrupts.count() <= baseline {
                sink.settled();
            }
            replied
        };
        host.serve(&mut *session, Some(&mut forward))
    };
    let signalled = host.signalled() || end.signalled();
    host.finish(
        Some(session),
        CoreError::io("the qrec command hosting this project is exiting"),
    );
    let replied = own.forward(sink.out());
    if signalled {
        Ran::Interrupted
    } else if replied {
        Ran::Replied
    } else {
        reply_err(
            sink.out(),
            CoreError::io("the host stopped before answering"),
        )
    }
}

// ----------------------------------------------------- the own request

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
    /// Runs with the reply, on the session thread.
    on_reply: Option<Box<dyn FnOnce() + Send>>,
}

impl OwnRequest {
    /// `on_reply` runs when the Session answers (or drops) the request.
    fn new(on_reply: impl FnOnce() + Send + 'static) -> (OwnRequest, ProxyResponder) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (
            OwnRequest { rx, replied: false },
            ProxyResponder {
                tx,
                replied: false,
                on_reply: Some(Box::new(on_reply)),
            },
        )
    }

    /// A request that was never sent: nothing will answer it.
    fn unanswered() -> OwnRequest {
        let (_, rx) = crossbeam_channel::unbounded();
        OwnRequest { rx, replied: false }
    }

    /// Forwards what arrived; `true` once the reply was forwarded.
    fn forward(&mut self, out: &mut dyn Replies) -> bool {
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
            if let Some(on_reply) = self.on_reply.take() {
                on_reply();
            }
            let _ = self.tx.send(OwnEvent::Reply(r));
        }
    }
}

impl Drop for ProxyResponder {
    fn drop(&mut self) {
        if !self.replied {
            self.reply(Err(CoreError::io(
                "the Session dropped the request without an answer",
            )));
        }
    }
}

// --------------------------------------------------------------- errors

/// The routing deadline passed while a CLI command's own host kept
/// bouncing the request (A§8.5): it never records, and takes no new job
/// once its own command is done.
fn bounced_by_cli_host(hello: &HelloInfo, req: &Request) -> CoreError {
    let (what, again) = if starts_recording(req) {
        (
            "doesn't record",
            "record again once it finishes (`qrec status` shows it)",
        )
    } else {
        (
            "takes no new job while it finishes",
            "run this again once it finishes",
        )
    };
    CoreError::busy(format!(
        "the project is held by another qrec command (pid {}), which {what}",
        hello.pid
    ))
    .with_hint(again)
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
        SendError::NotAccepted => CoreError::busy(format!(
            "{who} closed the connection before accepting the request"
        ))
        .with_hint("it is probably shutting down; run the command again"),
        SendError::NotResponding => CoreError::busy(format!("{who} is not responding"))
            .with_hint(see_log("it may be stuck; the request may still run")),
        SendError::Disconnected => CoreError::io("host disconnected").with_hint(see_log(
            "the request may have run; check with `qrec status`",
        )),
        SendError::LostContact => CoreError::busy(format!(
            "{who} stopped responding while running the request"
        ))
        .with_hint(see_log("it may still finish; check with `qrec status`")),
        SendError::Protocol(msg) => CoreError::io(format!("unexpected answer from {who}: {msg}")),
    }
}

/// Where a host logs: the one this CLI spawned, a headless host's
/// `.qrec/logs/…-<pid>-host.log`, or the GUI's log of this project
/// (`UserDirs::gui_log`). `paths` is the canonical project folder.
fn host_log(
    paths: &ProjectPaths,
    env: &Env,
    hello: Option<&HelloInfo>,
    spawned: Option<&SpawnedHost>,
) -> Option<PathBuf> {
    if let Some(spawned) = spawned.filter(|s| hello.is_none_or(|h| h.pid == s.pid)) {
        return Some(spawned.log.clone());
    }
    let hello = hello?;
    match hello.host {
        HostKind::Headless => find_host_log(paths, hello.pid),
        HostKind::Gui => UserDirs::resolve(env).ok().map(|d| d.gui_log(paths.root())),
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

    /// "Host disconnected" names the GUI's log of this project (G§3.2).
    #[test]
    fn a_gui_hosts_log_is_its_projects_gui_log() {
        let env = Env::new(1000)
            .with("HOME", "/home/shrek")
            .with("XDG_STATE_HOME", "/st");
        let paths = ProjectPaths::new("/videos/swamp");
        let hello = HelloInfo {
            pid: 42,
            project_dir: "/videos/swamp".into(),
            project_dir_hex: None,
            host: HostKind::Gui,
            record: qrec_core::state::RecordPhase::Idle,
            jobs: Vec::new(),
            version: "0.1.0".into(),
        };
        let log = host_log(&paths, &env, Some(&hello), None).unwrap();
        let dirs = UserDirs::resolve(&env).unwrap();
        assert_eq!(log, dirs.gui_log(Path::new("/videos/swamp")));
        assert!(
            log.to_str().unwrap().starts_with("/st/qrec/gui-"),
            "{}",
            log.display()
        );
        let cli = HelloInfo {
            host: HostKind::Cli,
            ..hello
        };
        assert_eq!(host_log(&paths, &env, Some(&cli), None), None);
    }

    #[test]
    fn route_options_follow_the_request() {
        let opts = RouteOpts::for_request(&Request::query(Query::ClipList));
        assert_eq!(
            (opts.fallback, opts.lock, opts.deadline),
            (Fallback::InProcess, false, ROUTE_DEADLINE)
        );
        assert_eq!(opts.route_mode(), RouteMode::NoLock);

        let opts = RouteOpts::for_request(&command(Command::Undo { steps: 1 }));
        assert_eq!((opts.fallback, opts.lock), (Fallback::InProcess, true));
        assert_eq!(opts.route_mode(), RouteMode::Lock);

        let start = Command::RecordStart {
            mode: RecordModeArg::Append,
            countdown: None,
            wait: false,
        };
        let opts = RouteOpts::for_request(&command(start));
        assert_eq!(
            (opts.fallback, opts.deadline, opts.stop_on_interrupt),
            (Fallback::SpawnHeadless, RECORD_ROUTE_DEADLINE, false)
        );
        assert_eq!(opts.route_mode(), RouteMode::Lock);
        let opts = RouteOpts::for_request(&command(Command::RecordToggle { wait: true }));
        assert_eq!(
            (opts.fallback, opts.stop_on_interrupt),
            (Fallback::SpawnHeadless, true)
        );
        let opts = RouteOpts::for_request(&command(Command::RecordStop));
        assert_eq!(
            (opts.fallback, opts.deadline),
            (Fallback::InProcess, RECORD_ROUTE_DEADLINE)
        );

        let over_selection = Command::RecordStart {
            mode: RecordModeArg::OverSelection,
            countdown: None,
            wait: false,
        };
        let opts = RouteOpts::for_request(&command(over_selection.clone()));
        assert_eq!(opts.fallback, Fallback::None);
        assert_eq!(opts.route_mode(), RouteMode::NoLock);
        assert!(no_host_reason(&command(over_selection)).contains("--over-selection"));
        assert_eq!(
            RouteOpts::for_request(&command(Command::Play)).fallback,
            Fallback::None
        );
        assert!(no_host_reason(&command(Command::Play)).contains("GUI"));
        let hello = Request {
            body: WireBody::Hello,
            confirmed: false,
        };
        assert_eq!(RouteOpts::for_request(&hello).fallback, Fallback::None);
    }

    #[test]
    fn failures_name_the_host_and_its_log() {
        let hello = HelloInfo {
            pid: 42,
            project_dir: "/p".into(),
            project_dir_hex: None,
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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = |calls: &Arc<AtomicUsize>| {
            let calls = Arc::clone(calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
            }
        };
        let (mut own, mut responder) = OwnRequest::new(counter(&calls));
        let mut out = crate::ipc::testing::Collect::default();
        assert!(!own.forward(&mut out));
        responder.accepted();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        responder.reply(Ok(Value::Null));
        responder.reply(Err(CoreError::internal("again")));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "told once, with the reply");
        assert!(own.forward(&mut out));
        assert_eq!((out.accepted, out.replies.len()), (1, 1));
        // Dropped without a reply: an error reply, and the host is told.
        let (mut own, responder) = OwnRequest::new(counter(&calls));
        drop(responder);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let mut out = crate::ipc::testing::Collect::default();
        assert!(own.forward(&mut out));
        assert!(out.reply().is_err());
    }

    #[test]
    fn a_cli_host_that_keeps_bouncing_is_named() {
        let hello = HelloInfo {
            pid: 42,
            project_dir: "/p".into(),
            project_dir_hex: None,
            host: HostKind::Cli,
            record: qrec_core::RecordPhase::Idle,
            jobs: vec![],
            version: VERSION.into(),
        };
        let record = command(Command::RecordToggle { wait: false });
        let e = bounced_by_cli_host(&hello, &record);
        assert_eq!(e.code, qrec_core::ErrorCode::Busy);
        assert_eq!(
            e.message,
            "the project is held by another qrec command (pid 42), which doesn't record"
        );
        assert!(e.hint.unwrap().starts_with("record again once it finishes"));
        let export = command(Command::ExportCancel { job: None });
        assert!(
            bounced_by_cli_host(&hello, &export)
                .message
                .ends_with("which takes no new job while it finishes")
        );
    }
}
