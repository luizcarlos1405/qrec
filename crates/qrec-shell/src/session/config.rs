//! What a Session is opened with besides `Deps`: the per-user dirs, the
//! capture backend switch, capability detection settings, where toasts go,
//! device discovery, and (for hosts) a lock and an inbox taken earlier.

use std::path::PathBuf;

use crossbeam_channel::Receiver;
use qrec_core::protocol::Toast;
use qrec_core::protocol::ToastLevel;

use crate::caps::CapsOptions;
use crate::error::ShellError;
use crate::facts::Sources;
use crate::lock::ProjectLock;
use crate::msg::{Inbox, Msg};
use crate::paths::{Env, UserDirs};

/// `QREC_CAPTURE_BACKEND`: `fake` swaps in test sources (A§8.11).
pub const BACKEND_ENV: &str = "QREC_CAPTURE_BACKEND";

/// Where people see a toast (`Effect::Notify`): the GUI's toasts, the
/// CLI's stderr, a headless host's log.
pub trait Notifier: Send {
    fn notify(&mut self, toast: &Toast);
}

impl<F: FnMut(&Toast) + Send> Notifier for F {
    fn notify(&mut self, toast: &Toast) {
        self(toast)
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
        let mut caps = CapsOptions::system(Some(dirs.capabilities_cache()), Some(dirs.state.join("logs")));
        caps.probe_va = !fake_backend;
        Self {
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
    /// `None`: the fake inventory under the fake backend, else the system's.
    pub sources: Option<Box<dyn Sources>>,
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
            .finish_non_exhaustive()
    }
}
