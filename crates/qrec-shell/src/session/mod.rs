//! The Session (A§8.1): one per open project, on one session thread.
//! Owner: session.
//!
//! - [`Session::open`] applies the folder rule, takes the lock (mutating
//!   opens), loads and reconciles the files (A§7.6, A§7.7), detects the
//!   capabilities, merges an external edit found on disk, starts the
//!   recovery probes of takes still `recording` and the `qrec.json`
//!   watcher.
//! - [`Session::handle`] takes one [`Msg`]. A request gets a `ReqId`, its
//!   facts (`facts_needed`), `apply`, then its effects in the A§7.6 order
//!   (history → `qrec.json` → head, then the world), then its reply. Events
//!   go to `on_event` the same way. An empty folder is initialized lazily
//!   before its first command (A§8.4).
//! - The session thread only blocks on its own small writes (and the
//!   bounded `pactl` run of an `AudioAdd`); media jobs, capture, timers and
//!   the watcher answer through the [`Inbox`].
//!
//! Every state change goes through here.

mod config;
mod dispatch;
mod effects;
mod open;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use qrec_core::apply::project::is_initialized;
use qrec_core::state::JobKind;
use qrec_core::{ReqId, State};

pub use config::{BACKEND_ENV, LogNotifier, Notifier, SessionConfig, SessionEnv};

use crate::deps::{Deps, OpenOpts};
use crate::error::ShellError;
use crate::facts::FactsGatherer;
use crate::lock::ProjectLock;
use crate::msg::{Inbox, Msg, Responder};
use crate::paths::ProjectPaths;
use crate::store::{Check, Store};
use crate::timers::Timers;
use crate::watcher::Watcher;

/// How long [`Session::run_until`] sleeps at most between checks of `done`.
pub const POLL: Duration = Duration::from_millis(250);

thread_local! {
    static CURRENT: RefCell<Option<ReqId>> = const { RefCell::new(None) };
}

/// The `ReqId` of the request whose [`Responder`] is being called, on the
/// session thread. An IPC responder reads it in `accepted()` so that, when
/// its client disconnects, it can post `Event::RequesterGone { req }`.
pub fn current_request() -> Option<ReqId> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Runs `f` (a responder call) with `req` as the [`current_request`].
pub(crate) fn with_current<R>(req: &ReqId, f: impl FnOnce() -> R) -> R {
    let previous = CURRENT.with(|c| c.replace(Some(req.clone())));
    let result = f();
    CURRENT.with(|c| *c.borrow_mut() = previous);
    result
}

/// One open project.
pub struct Session {
    /// The canonical project folder.
    dir: PathBuf,
    paths: ProjectPaths,
    state: State,
    deps: Deps,
    inbox: Inbox,
    rx: Receiver<Msg>,
    store: Store,
    opts: OpenOpts,
    check: Check,
    facts: FactsGatherer,
    timers: Timers,
    watcher: Option<Watcher>,
    /// Requests waiting for a late `Effect::Reply`.
    pending: BTreeMap<ReqId, Box<dyn Responder>>,
    next_req: u64,
    notifier: Box<dyn Notifier>,
    shutdown: bool,
    interrupts: u32,
    /// Tests: a simulated crash stopped this "process"; nothing runs anymore.
    #[cfg(test)]
    crashed: bool,
    /// Dropped last: closing it releases the project.
    lock: Option<ProjectLock>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("dir", &self.dir)
            .field("opts", &self.opts)
            .field("host", &self.lock.is_some())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Opens the project in `dir` (A§8.3, A§8.4, A§7.6 recovery on open)
    /// with the process's environment. A mutating open takes the project
    /// lock (`Busy` when another process holds it); a read-only one takes
    /// no lock and writes nothing.
    pub fn open(dir: &Path, opts: OpenOpts, deps: Deps) -> Result<Session, ShellError> {
        Self::open_with(dir, opts, deps, SessionConfig::default())
    }

    /// [`Session::open`] with a lock or inbox taken earlier, another
    /// environment, a toast sink or other device discovery.
    pub fn open_with(
        dir: &Path,
        opts: OpenOpts,
        deps: Deps,
        cfg: SessionConfig,
    ) -> Result<Session, ShellError> {
        open::open(dir, opts, deps, cfg)
    }

    /// A sender for workers, IPC connections and the GUI.
    pub fn inbox(&self) -> Inbox {
        self.inbox.clone()
    }

    /// Handles one message (request, event, disk change, interrupt, shutdown).
    pub fn handle(&mut self, msg: Msg) {
        #[cfg(test)]
        if self.crashed {
            return;
        }
        match msg {
            Msg::Request { req, to } => self.handle_request(req, to),
            Msg::Event(ev) => self.handle_event(ev),
            Msg::DiskChanged => self.handle_disk_changed(),
            Msg::Interrupt => self.handle_interrupt(),
            Msg::Shutdown => self.shutdown = true,
        }
        self.tick();
    }

    /// Drains the inbox until `done(state)` holds (CLI and hosts). `done`
    /// is checked after every message and at least every [`POLL`].
    pub fn run_until(&mut self, mut done: impl FnMut(&State) -> bool) {
        loop {
            self.tick();
            if done(&self.state) {
                return;
            }
            let wait = self.next_deadline().map_or(POLL, |due| {
                due.saturating_duration_since(Instant::now()).min(POLL)
            });
            match self.rx.recv_timeout(wait) {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Handles every message already queued (without waiting).
    pub fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        self.tick();
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// The project directory (canonical).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The project lock, when this session is the project's host.
    pub fn lock(&self) -> Option<&ProjectLock> {
        self.lock.as_ref()
    }

    /// This session holds the lock (and writes the project).
    pub fn is_host(&self) -> bool {
        self.lock.is_some()
    }

    /// A `shutdown` request arrived (A§8.6): stop accepting, let jobs finish.
    pub fn shutdown_requested(&self) -> bool {
        self.shutdown
    }

    /// How many interrupts (Ctrl+C, SIGTERM) arrived.
    pub fn interrupts(&self) -> u32 {
        self.interrupts
    }

    /// Requests still waiting for their reply.
    pub fn pending_requests(&self) -> usize {
        self.pending.len()
    }

    /// Not recording, no jobs, no recovery probe out and no request waiting:
    /// a headless or CLI host may exit (after its own grace period, A§8.6).
    pub fn is_idle(&self) -> bool {
        !self.state.runtime.is_busy() && self.pending.is_empty()
    }

    /// When [`Session::tick`] has work (a coalesced `ui.json` write).
    pub fn next_deadline(&self) -> Option<Instant> {
        self.store.ui_deadline()
    }

    /// Does what is due: a coalesced `ui.json` write.
    pub fn tick(&mut self) {
        if let Err(e) = self.store.flush_ui(&self.state.ui, Instant::now(), false) {
            tracing::warn!("can't write .qrec/ui.json: {e}");
        }
    }

    fn alloc_req(&mut self) -> ReqId {
        self.next_req += 1;
        ReqId(format!("r{}", self.next_req))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        #[cfg(test)]
        let crashed = self.crashed;
        #[cfg(not(test))]
        let crashed = false;
        // Facts an event couldn't save (a failed commit keeps them in memory
        // and the store dirty) get one last try before the host goes away;
        // never over a qrec.json that failed to load (A§7.7).
        if !crashed
            && self.store.is_dirty()
            && !self.opts.recovery
            && self.state.runtime.disk_invalid.is_none()
            && is_initialized(&self.state.project)
            && let Err(e) = self.store.write_all(&self.state)
        {
            tracing::error!("couldn't save the project before closing it: {e}");
        }
        if let Err(e) = self.store.flush_ui(&self.state.ui, Instant::now(), true) {
            tracing::warn!("can't write .qrec/ui.json: {e}");
        }
        // A host that goes away doesn't leave ffmpeg jobs behind (a purge
        // finishes: half-deleted takes are worse than a slow exit).
        let jobs: Vec<_> = self
            .state
            .runtime
            .jobs
            .values()
            .filter(|job| !matches!(job.kind, JobKind::Purge { .. }))
            .map(|job| job.id)
            .collect();
        for job in jobs {
            self.deps.media.cancel(job);
        }
        self.deps.capture.shutdown();
    }
}
