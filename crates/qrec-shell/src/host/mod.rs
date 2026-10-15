//! Hosts: the process that holds the project lock and serves the socket
//! (A§8.3, A§8.5, A§8.6). The submodules belong to two owners, so each
//! edits only its own files:
//!
//! - `lockfile`, `publish` — owner: store
//! - `headless`, `cli_host`, `signals`, `spawn` and this file — owner: ipc
//!
//! A host runs like this (A§8.3): take the lock → record `starting` →
//! publish the socket and start the IPC server → record `ready` → open the
//! Session (early requests wait in the server) → attach it → run the Session
//! until the exit rules say so → stop accepting, unlink the socket, flush
//! replies → drop the lock.
//!
//! **Exit rules** (A§8.6, Headless and Cli): exit once Idle (not recording,
//! no jobs, no take waiting for its Recover probe, so a crashed take's
//! recovery is finished and saved instead of killed with the host) with no
//! open connection has held for 2 s; a headless host that
//! never got a request exits 60 s after `ready`; an in-process CLI host whose
//! own command finished exits at once if nobody else used it. `shutdown`:
//! stop accepting, finalize a recording, let jobs finish, exit. The first
//! signal dispatches `RecordStop` and `Msg::Interrupt` (cancel jobs), ahead
//! of requests parked while the Session opened; the Session then runs no
//! command that hadn't started but those that stop something; exit once
//! idle (a recovery probe still out counts) or after 15 s. The second
//! signal kills child process groups and `_exit(130)`s.
//!
//! The Session only re-evaluates the exit rules when it handles a message,
//! so a waker thread sends a cheap nudge (a `Query::Status` nobody waits
//! for) whenever a deadline of the rules comes due.

pub mod cli_host;
pub mod headless;
pub mod lockfile;
pub mod publish;
pub mod signals;
pub mod spawn;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{SubsecRound, Utc};
use qrec_core::protocol::{Command, Progress, Query};
use qrec_core::state::HostKind;
use qrec_core::{CoreError, State};
use serde_json::Value;

pub use cli_host::{
    CliContext, CliExit, CliRequest, Fallback, Finish, RequestContext, RequestEvents, RouteOpts,
    request, run_cli,
};
pub use headless::{HeadlessArgs, main_headless, run_headless};
pub use spawn::HostProgram;

use crate::deps::{Deps, OpenOpts};
use crate::error::ShellError;
use crate::ipc::server::{Hook, IpcServer, ServerConfig, ServerEvent};
use crate::lock::ProjectLock;
use crate::msg::{Inbox, Msg, Request, Responder};
use crate::paths::ProjectPaths;
use crate::session::Session;
use lockfile::{self as lock_record, HostState, LockRecord};
use publish::publish;
use signals::{Interrupts, SignalWatch};

/// What a host needs from its Session (A§8.1). [`Session`] implements it;
/// tests use a stand-in.
pub trait HostSession {
    /// The Session's queue.
    fn inbox(&self) -> Inbox;
    /// Handles messages until `done(state)` holds. `done` is evaluated after
    /// each handled message; it may also be evaluated at other times.
    fn run_until(&mut self, done: &mut dyn FnMut(&State) -> bool);
    fn state(&self) -> &State;
}

impl HostSession for Session {
    fn inbox(&self) -> Inbox {
        Session::inbox(self)
    }

    fn run_until(&mut self, done: &mut dyn FnMut(&State) -> bool) {
        Session::run_until(self, done);
    }

    fn state(&self) -> &State {
        Session::state(self)
    }
}

/// Opens the Session of a project for a host or an in-process command.
pub trait Opener {
    /// `opts.mutating`: the caller holds the project lock and passes a
    /// second descriptor of it in `lock` (same open file description, so
    /// the same `flock`): the Session keeps it to write the project and
    /// only closes it when dropped, never asking for the lock again (a
    /// second `open` of `.qrec/lock` in this process couldn't take it).
    /// Read-only opens get `None` and hold no lock.
    fn open(
        &mut self,
        dir: &Path,
        opts: OpenOpts,
        lock: Option<SessionLock>,
    ) -> Result<Box<dyn HostSession>, ShellError>;
}

/// The project lock as the Session holds it (see [`Opener::open`]).
pub type SessionLock = crate::lock::ProjectLock;

/// The production [`Opener`]: [`Session::open_with`] with `Deps` from a
/// factory (the capture engine depends on the host kind and
/// `QREC_CAPTURE_BACKEND`) and the host's lock.
pub struct SessionOpener<F> {
    make_deps: F,
}

impl<F> SessionOpener<F>
where
    F: FnMut(&OpenOpts) -> Result<Deps, ShellError>,
{
    pub fn new(make_deps: F) -> Self {
        Self { make_deps }
    }
}

impl<F> Opener for SessionOpener<F>
where
    F: FnMut(&OpenOpts) -> Result<Deps, ShellError>,
{
    fn open(
        &mut self,
        dir: &Path,
        opts: OpenOpts,
        lock: Option<SessionLock>,
    ) -> Result<Box<dyn HostSession>, ShellError> {
        let deps = (self.make_deps)(&opts)?;
        let cfg = crate::session::SessionConfig {
            lock,
            ..Default::default()
        };
        Ok(Box::new(Session::open_with(dir, opts, deps, cfg)?))
    }
}

// ------------------------------------------------------------- exit rules

/// The A§8.6 timings (tests shorten them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitRules {
    /// Idle with no open connection for this long ⇒ exit.
    pub idle_grace: Duration,
    /// A headless host with no request this long after `ready` exits.
    pub unused_timeout: Duration,
    /// After the first signal, exit at the latest after this long.
    pub signal_grace: Duration,
    /// At exit, open connections get this long to flush their replies.
    pub flush_timeout: Duration,
}

impl Default for ExitRules {
    fn default() -> Self {
        Self {
            idle_grace: Duration::from_secs(2),
            unused_timeout: Duration::from_secs(60),
            signal_grace: Duration::from_secs(15),
            flush_timeout: Duration::from_secs(10),
        }
    }
}

/// Why a host stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEnd {
    /// Idle long enough, or (in-process) its own command finished.
    Idle,
    /// Headless, never used: 60 s after `ready` without a request.
    Unused,
    /// A `shutdown` request, once jobs finished.
    Shutdown,
    /// A signal, once idle.
    Signal,
    /// A signal, and still busy when the grace period ended.
    SignalTimeout,
    /// The Session stopped running on its own while the rules said wait.
    SessionStopped,
}

impl HostEnd {
    pub fn signalled(self) -> bool {
        matches!(self, HostEnd::Signal | HostEnd::SignalTimeout)
    }
}

/// What the exit decision looks at.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExitInputs {
    pub now: Instant,
    /// Recording (any phase but Idle), running jobs or takes being recovered.
    pub busy: bool,
    /// Open IPC connections.
    pub open: usize,
    /// Since when no IPC connection is open.
    pub conns_idle_since: Option<Instant>,
    /// Since when the state is not busy.
    pub state_quiet_since: Option<Instant>,
    /// IPC commands and queries accepted so far.
    pub requests: u64,
    pub shutdown: bool,
    pub signalled_at: Option<Instant>,
    /// In-process CLI host: whether its own command got its reply.
    pub own_done: Option<bool>,
    pub ready_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Exit(HostEnd),
    /// Keep running; look again at this time (if any).
    Wait(Option<Instant>),
}

/// The A§8.6 exit rules as a pure function.
pub(crate) fn decide(i: &ExitInputs, rules: &ExitRules) -> Decision {
    let quiet = !i.busy && i.open == 0;
    if let Some(at) = i.signalled_at {
        if quiet {
            return Decision::Exit(HostEnd::Signal);
        }
        let limit = at + rules.signal_grace;
        if i.now >= limit {
            return Decision::Exit(HostEnd::SignalTimeout);
        }
        return Decision::Wait(Some(limit));
    }
    if i.shutdown {
        return if quiet {
            Decision::Exit(HostEnd::Shutdown)
        } else {
            Decision::Wait(None)
        };
    }
    match i.own_done {
        Some(false) => return Decision::Wait(None),
        // Nobody else used this in-process host: done means done (A§8.5).
        Some(true) if quiet && i.requests == 0 => return Decision::Exit(HostEnd::Idle),
        Some(true) => {}
        None if i.requests == 0 => {
            let limit = i.ready_at + rules.unused_timeout;
            if i.now < limit {
                return Decision::Wait(Some(limit));
            }
            // Past the limit: exit once quiet (a closing connection nudges).
            return if quiet {
                Decision::Exit(HostEnd::Unused)
            } else {
                Decision::Wait(None)
            };
        }
        None => {}
    }
    if !quiet {
        return Decision::Wait(None);
    }
    let since = match (i.conns_idle_since, i.state_quiet_since) {
        (Some(a), Some(b)) => a.max(b),
        (a, b) => a.or(b).unwrap_or(i.now),
    };
    let limit = since + rules.idle_grace;
    if i.now >= limit {
        Decision::Exit(HostEnd::Idle)
    } else {
        Decision::Wait(Some(limit))
    }
}

// ------------------------------------------------------------------ waker

/// Sends a nudge into the Session when a deadline comes due.
pub(crate) struct Waker {
    inner: Arc<WakerInner>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub(crate) struct WakerInner {
    st: Mutex<WakeState>,
    cv: Condvar,
}

#[derive(Default)]
struct WakeState {
    at: Option<Instant>,
    stop: bool,
}

impl WakerInner {
    /// Nudge at `at` (the earliest pending time wins).
    pub fn wake_at(&self, at: Instant) {
        let mut st = self.st.lock().unwrap_or_else(PoisonError::into_inner);
        st.at = Some(st.at.map_or(at, |t| t.min(at)));
        self.cv.notify_all();
    }

    pub fn wake_now(&self) {
        self.wake_at(Instant::now());
    }
}

impl Waker {
    /// `nudge` posts one wake-up message; it returns `false` once the
    /// Session is gone.
    pub fn start(nudge: impl Fn() -> bool + Send + 'static) -> std::io::Result<Waker> {
        let inner = Arc::new(WakerInner::default());
        let shared = Arc::clone(&inner);
        let handle = thread::Builder::new()
            .name("qrec-host-waker".into())
            .spawn(move || {
                let mut st = shared.st.lock().unwrap_or_else(PoisonError::into_inner);
                while !st.stop {
                    match st.at {
                        Some(at) if at <= Instant::now() => {
                            st.at = None;
                            drop(st);
                            let alive = nudge();
                            st = shared.st.lock().unwrap_or_else(PoisonError::into_inner);
                            if !alive {
                                return;
                            }
                        }
                        Some(at) => {
                            let wait = at.saturating_duration_since(Instant::now());
                            st = shared
                                .cv
                                .wait_timeout(st, wait)
                                .unwrap_or_else(PoisonError::into_inner)
                                .0;
                        }
                        None => {
                            st = shared.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
                        }
                    }
                }
            })?;
        Ok(Waker {
            inner,
            handle: Some(handle),
        })
    }

    pub fn handle(&self) -> Arc<WakerInner> {
        Arc::clone(&self.inner)
    }

    pub fn wake_at(&self, at: Instant) {
        self.inner.wake_at(at);
    }

    pub fn wake_now(&self) {
        self.inner.wake_now();
    }
}

impl Drop for Waker {
    fn drop(&mut self) {
        {
            let mut st = self.inner.st.lock().unwrap_or_else(PoisonError::into_inner);
            st.stop = true;
        }
        self.inner.cv.notify_all();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A responder for requests nobody waits for (nudges, a host's own
/// `RecordStop` on a signal or `shutdown`).
pub(crate) struct NullResponder;

impl Responder for NullResponder {
    fn accepted(&mut self) {}
    fn progress(&mut self, _p: &Progress) {}
    fn reply(&mut self, _r: Result<Value, CoreError>) {}
}

/// Makes the Session handle a message, so `run_until` re-evaluates its
/// `done`: a status query (pure, cheap) whose reply is discarded.
pub(crate) fn nudge_msg() -> Msg {
    Msg::Request {
        req: Request::query(Query::Status),
        to: Box::new(NullResponder),
    }
}

/// `RecordStop` from the host itself (finalize, never cancel); a no-op
/// error when nothing records.
pub(crate) fn record_stop_msg() -> Msg {
    Msg::Request {
        req: Request::command(Command::RecordStop, false),
        to: Box::new(NullResponder),
    }
}

// ------------------------------------------------------------- the host

/// How a host is set up beyond its kind.
#[derive(Default)]
pub struct HostOptions {
    pub rules: ExitRules,
    /// The counter signals raise (from [`signals::install`]); `None` means
    /// signals aren't watched (tests drive [`Interrupts::raise`] instead).
    pub interrupts: Option<Interrupts>,
    /// Interrupts counted before this host's work began (e.g. when the CLI
    /// started); later ones are the "first" and "second" signals.
    pub signal_baseline: usize,
    /// The second signal. `None`: unlink the socket, SIGKILL the child
    /// process groups and `_exit(130)` ([`signals::hard_exit`]).
    pub on_second_signal: Option<Box<dyn FnOnce() + Send>>,
}

impl std::fmt::Debug for HostOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostOptions")
            .field("rules", &self.rules)
            .field("interrupts", &self.interrupts)
            .finish_non_exhaustive()
    }
}

/// What the signal thread and the server's hook share with the loop.
#[derive(Default)]
pub(crate) struct HostCtl {
    pub signalled_at: Mutex<Option<Instant>>,
    pub waker: OnceLock<Arc<WakerInner>>,
}

impl HostCtl {
    fn wake_now(&self) {
        if let Some(waker) = self.waker.get() {
            waker.wake_now();
        }
    }

    fn signalled_at(&self) -> Option<Instant> {
        *self
            .signalled_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// A host that holds the lock and serves its socket.
pub(crate) struct Host {
    pub paths: ProjectPaths,
    lock: ProjectLock,
    record: LockRecord,
    pub server: IpcServer,
    waker: Waker,
    ctl: Arc<HostCtl>,
    _signals: Option<SignalWatch>,
    pub ready_at: Instant,
    rules: ExitRules,
}

impl Host {
    /// A§8.3 step 1 under a held lock: record `starting`, publish the
    /// socket, start the IPC server (requests wait for [`attach`](Self::attach)),
    /// watch signals, record `ready`.
    pub fn start(
        kind: HostKind,
        paths: ProjectPaths,
        lock: ProjectLock,
        socket: &Path,
        opts: HostOptions,
    ) -> Result<Host, CoreError> {
        let mut record = LockRecord::starting(kind, socket, Utc::now().trunc_subsecs(0));
        lock_record::write(&lock, &record)?;
        let published = publish(socket)?;
        let published_path = published.path().to_path_buf();
        let ctl = Arc::new(HostCtl::default());
        let hook_ctl = Arc::clone(&ctl);
        let hook: Hook = Arc::new(move |event| match event {
            ServerEvent::Request | ServerEvent::Idle | ServerEvent::Shutdown => {
                hook_ctl.wake_now();
            }
        });
        let server = IpcServer::start(published, ServerConfig::new(paths.root(), kind), hook)?;
        let sender = server.sender();
        let waker = Waker::start(move || sender.send(nudge_msg()))
            .map_err(|e| CoreError::io(format!("can't start the host waker: {e}")))?;
        let _ = ctl.waker.set(waker.handle());
        let signals = match &opts.interrupts {
            Some(interrupts) => Some(watch_signals(
                interrupts,
                opts.signal_baseline,
                &ctl,
                &server,
                published_path,
                opts.on_second_signal,
            )?),
            None => None,
        };
        lock_record::set_state(&lock, &mut record, HostState::Ready)?;
        Ok(Host {
            paths,
            lock,
            record,
            server,
            waker,
            ctl,
            _signals: signals,
            ready_at: Instant::now(),
            rules: opts.rules,
        })
    }

    /// Connects the opened Session; parked requests go to it in order.
    pub fn attach(&self, inbox: Inbox) {
        self.server.attach(inbox);
    }

    /// The lock for this host's Session ([`Opener::open`]): a second
    /// descriptor of the lock the host holds. Closing it (when the Session
    /// is dropped) releases nothing while the host keeps its own.
    pub fn session_lock(&self) -> Result<SessionLock, CoreError> {
        let fd = self
            .lock
            .try_clone_fd()
            .map_err(|e| CoreError::io(format!("can't share the project lock: {e}")))?;
        Ok(SessionLock::adopt(self.paths.root(), fd)?)
    }

    /// Runs the Session until the exit rules say stop. `own` (in-process CLI
    /// host) is polled after each message and says whether the host's own
    /// command got its reply.
    ///
    /// Exiting is atomic for clients (A§8.3 step 3): the rules only say
    /// stop with no connection open, and the server stops accepting only
    /// while that still holds, under the lock admission takes. Late clients
    /// get `ENOENT`, a reset or EOF before `accepted` and route again,
    /// safely: their request never ran. Nothing a client was told
    /// `accepted` is dropped.
    pub fn serve(
        &self,
        session: &mut dyn HostSession,
        mut own: Option<&mut dyn FnMut() -> bool>,
    ) -> HostEnd {
        let mut state_quiet_since = Some(self.ready_at);
        let mut end = None;
        let mut stopping_noted = false;
        let mut spontaneous = 0u32;
        self.waker.wake_now();
        loop {
            let mut done = |state: &State| {
                let now = Instant::now();
                let own_done = own.as_mut().map(|own| own());
                // Recording, jobs, or a startup recovery still out (A§8.6).
                let busy = state.runtime.is_busy();
                if busy {
                    state_quiet_since = None;
                } else if state_quiet_since.is_none() {
                    state_quiet_since = Some(now);
                }
                let stats = self.server.stats();
                if stats.shutdown && !stopping_noted {
                    stopping_noted = true;
                    self.on_shutdown_request();
                }
                let inputs = ExitInputs {
                    now,
                    busy,
                    open: stats.open,
                    conns_idle_since: stats.idle_since,
                    state_quiet_since,
                    requests: stats.requests,
                    shutdown: stats.shutdown,
                    signalled_at: self.ctl.signalled_at(),
                    own_done,
                    ready_at: self.ready_at,
                };
                match decide(&inputs, &self.rules) {
                    Decision::Exit(why) => {
                        end = Some(why);
                        true
                    }
                    Decision::Wait(at) => {
                        if let Some(at) = at {
                            self.waker.wake_at(at);
                        }
                        false
                    }
                }
            };
            let started = Instant::now();
            session.run_until(&mut done);
            if let Some(why) = end.take() {
                // Out of time after a signal: go, whoever is connected.
                if why == HostEnd::SignalTimeout {
                    return why;
                }
                // The rules saw no open connection; stop accepting only if
                // that still holds, atomically with admission, so no client
                // is admitted (told `accepted`) by a host that then drops
                // its request. One that got in first is served; then the
                // rules decide again.
                if self.server.stop_accepting_if_idle() {
                    self.mark_stopping();
                    return why;
                }
                continue;
            }
            // The Session returned without our rules saying so (it stopped
            // itself, e.g. after `Msg::Shutdown`). Keep it running while it
            // still makes progress; give up if it keeps returning at once.
            if started.elapsed() < Duration::from_millis(50) {
                spontaneous += 1;
                if spontaneous >= 100 {
                    return HostEnd::SessionStopped;
                }
                thread::sleep(Duration::from_millis(50));
            } else {
                spontaneous = 0;
            }
        }
    }

    /// `shutdown` (A§8.6): the record says `stopping`, a recording is
    /// finalized, the Session hears `Msg::Shutdown`; jobs finish.
    fn on_shutdown_request(&self) {
        self.mark_stopping();
        self.server.send(record_stop_msg());
        self.server.send(Msg::Shutdown);
    }

    /// The lock record says `stopping` (best effort: it only words errors).
    fn mark_stopping(&self) {
        let mut record = self.record.clone();
        let _ = lock_record::set_state(&self.lock, &mut record, HostState::Stopping);
    }

    /// The exit sequence (A§8.3 step 3): record `stopping`, stop accepting
    /// and unlink the socket, drop the Session (requests it still held are
    /// answered), let replies flush (≤ `flush_timeout`), then release the
    /// lock by closing it.
    pub fn finish(self, session: Option<Box<dyn HostSession>>, why: CoreError) {
        self.mark_stopping();
        self.server.stop_accepting();
        drop(session);
        self.server.finish(why, self.rules.flush_timeout);
        let Host {
            lock,
            server,
            waker,
            _signals,
            ..
        } = self;
        drop(_signals);
        drop(waker);
        drop(server);
        drop(lock);
    }

    pub fn signalled(&self) -> bool {
        self.ctl.signalled_at().is_some()
    }
}

/// The first signal: note it, finalize a recording, cancel jobs
/// (`Msg::Interrupt`), wake the loop. Both messages go ahead of requests
/// that were parked while the Session opened, so the Session refuses those
/// instead of running them first. The second: `on_second` or the hard exit.
fn watch_signals(
    interrupts: &Interrupts,
    baseline: usize,
    ctl: &Arc<HostCtl>,
    server: &IpcServer,
    socket: PathBuf,
    on_second: Option<Box<dyn FnOnce() + Send>>,
) -> Result<SignalWatch, CoreError> {
    let first_ctl = Arc::clone(ctl);
    let sender = server.sender();
    let first = move || {
        {
            let mut at = first_ctl
                .signalled_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            at.get_or_insert_with(Instant::now);
        }
        sender.send_urgent(record_stop_msg());
        sender.send_urgent(Msg::Interrupt);
        first_ctl.wake_now();
    };
    let second = move || match on_second {
        Some(second) => second(),
        None => {
            // We still hold the lock, so the path is ours (A§8.3 step 2).
            let _ = std::fs::remove_file(&socket);
            signals::hard_exit();
        }
    };
    signals::watch(interrupts, baseline, first, second)
        .map_err(|e| CoreError::io(format!("can't watch signals: {e}")))
}

/// The host log of a host, if it has one (for "host disconnected").
pub(crate) fn find_host_log(paths: &ProjectPaths, pid: u32) -> Option<PathBuf> {
    let suffix = format!("-{pid}-host.log");
    let mut logs: Vec<PathBuf> = std::fs::read_dir(paths.logs_dir())
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
        .collect();
    logs.sort();
    logs.pop()
}

#[cfg(test)]
#[path = "ipc_hosts_tests.rs"]
mod tests;
