//! What a Session is opened with besides `Deps`: the per-user dirs, the
//! capture backend switch, capability detection settings, where toasts go,
//! who sees the state change, device discovery, and (for hosts) a lock and
//! an inbox taken earlier.

use std::path::PathBuf;

use crossbeam_channel::Receiver;
use qrec_core::State;
use qrec_core::protocol::Toast;
use qrec_core::protocol::ToastLevel;

use crate::caps::CapsOptions;
use crate::error::ShellError;
use crate::facts::{ConfigFile, Sources};
use crate::lock::ProjectLock;
use crate::msg::{Inbox, Msg};
use crate::paths::{Env, UserDirs};

/// `QREC_CAPTURE_BACKEND`: `fake` swaps in test sources (A§8.11).
pub const BACKEND_ENV: &str = "QREC_CAPTURE_BACKEND";

/// Where people see a toast (`Effect::Notify`): the GUI's toasts, the
/// CLI's stderr, a headless host's log.
pub trait Notifier: Send {
    fn notify(&mut self, toast: &Toast);

    /// A toast of the startup recovery of takes a crash left `recording`
    /// (A§5.3): the host's own news, not about a request. An in-process
    /// command finishes the recovery its open began even after its reply
    /// (A§8.6), so its CLI shows these after that reply too, unlike the
    /// toasts of other clients' requests. Default: like any toast.
    fn notify_recovery(&mut self, toast: &Toast) {
        self.notify(toast);
    }
}

impl<F: FnMut(&Toast) + Send> Notifier for F {
    fn notify(&mut self, toast: &Toast) {
        self(toast)
    }
}

/// Who sees every state the Session holds (GUI H-6, docs/GUI.md G§3.5):
/// the GUI's snapshots.
///
/// The Session calls [`StateObserver::changed`] right after each assignment
/// of its state, durable or not: every outcome of a command or an event
/// (runtime-only ones too: `RecordClock`, export progress, playing,
/// recovery, autocut), a change kept in memory (recovery mode), and the
/// opened state before `open_with` returns (the states of the open's own
/// outcomes come before it). It runs before that outcome's toasts, progress
/// and replies, so a reply never reaches anyone before the state it
/// describes. `rev` grows by one with each call; an event the core ignores
/// still hands over a (possibly equal) state.
///
/// It runs on the session thread: it must return quickly and never wait
/// on whoever reads the snapshots (the GUI clones the state and queues it).
pub trait StateObserver: Send {
    fn changed(&mut self, rev: u64, state: &State);
}

impl<F: FnMut(u64, &State) + Send> StateObserver for F {
    fn changed(&mut self, rev: u64, state: &State) {
        self(rev, state)
    }
}

/// The default: toasts go to the log.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn notify(&mut self, toast: &Toast) {
        let body = toast.body.as_deref().unwrap_or_default();
        match toast.level {
            ToastLevel::Error => tracing::error!("{} {body}", toast.title),
            ToastLevel::Warning => tracing::warn!("{} {body}", toast.title),
            ToastLevel::Info | ToastLevel::Success => tracing::info!("{} {body}", toast.title),
        }
    }
}

/// The user-level environment of a session.
#[derive(Debug, Clone)]
pub struct SessionEnv {
    pub dirs: UserDirs,
    /// `QREC_CAPTURE_BACKEND=fake`: device facts come from the fake inventory.
    pub fake_backend: bool,
    pub caps: CapsOptions,
    /// `config.toml` (`dirs.config_file()`). Clones share what was read and
    /// reported: Sessions of one process opened with the same one warn
    /// about a file that can't be used once between them (a CLI run that
    /// initializes an empty folder opens two, A§4.2).
    pub config: ConfigFile,
}

impl SessionEnv {
    /// From this process's environment (`HOME`, `XDG_*`, `QREC_*`).
    pub fn from_process() -> Result<Self, ShellError> {
        let dirs = UserDirs::resolve(&Env::from_process())?;
        let fake = std::env::var(BACKEND_ENV).is_ok_and(|v| v == "fake");
        Ok(Self::for_dirs(dirs, fake))
    }

    /// The machine's ffmpeg and render nodes; the capabilities cache and
    /// probe logs under `dirs`. The fake backend (tests, CI) never runs
    /// the VA-API check, so nothing touches a GPU and exports plan the
    /// same everywhere.
    pub fn for_dirs(dirs: UserDirs, fake_backend: bool) -> Self {
        let mut caps = CapsOptions::system(
            Some(dirs.capabilities_cache()),
            Some(dirs.state.join("logs")),
        );
        caps.probe_va = !fake_backend;
        Self {
            config: ConfigFile::new(dirs.config_file()),
            dirs,
            fake_backend,
            caps,
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.dirs.config_file()
    }
}

/// Optional parts of [`Session::open_with`](super::Session::open_with).
#[derive(Default)]
pub struct SessionConfig {
    /// A lock this process already holds (CLI routing took it, or a
    /// spawned host inherited it with `--lock-fd`). Without one, a
    /// mutating open takes it and fails with `Busy` when it is held.
    pub lock: Option<ProjectLock>,
    /// A queue whose inbox was handed out before the session existed (IPC
    /// accepts connections before recovery, A§8.3): requests wait there.
    pub channel: Option<(Inbox, Receiver<Msg>)>,
    /// `None`: [`SessionEnv::from_process`].
    pub env: Option<SessionEnv>,
    /// `None`: [`LogNotifier`].
    pub notifier: Option<Box<dyn Notifier>>,
    /// Who sees every state the Session holds (the GUI); `None`: nobody.
    pub observer: Option<Box<dyn StateObserver>>,
    /// `None`: the fake inventory under the fake backend, else the system's.
    /// The GUI passes a [`SharedSources`](crate::facts::SharedSources), so
    /// device facts never block the session thread (GUI H-5).
    pub sources: Option<Box<dyn Sources>>,
    /// How long after opening commands wait for the recovery probes before
    /// they run anyway; `None`: [`RECOVERY_WAIT`](super::RECOVERY_WAIT).
    pub recovery_wait: Option<std::time::Duration>,
    /// Tests: how projects read from disk are checked (default A§2.3).
    #[cfg(test)]
    pub check: Option<crate::store::Check>,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConfig")
            .field("lock", &self.lock)
            .field("channel", &self.channel.is_some())
            .field("env", &self.env)
            .field("observer", &self.observer.is_some())
            .finish_non_exhaustive()
    }
}
