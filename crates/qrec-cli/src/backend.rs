//! The port between the command line and the imperative shell.
//!
//! The CLI maps arguments to requests, resolves the project folder
//! (A§8.4), renders replies and picks exit codes. Everything that needs
//! processes, sockets, locks or devices goes through a [`Backend`]:
//! routing a request to the project's host (A§8.3), running it in-process
//! when no host runs (A§8.5), spawning a headless host for `record`
//! (A§8.3 step 5), Ctrl+C handling (A§8.6), `doctor` and `sources list`
//! (A§8.12) and the hidden `qrec host --headless`.
//!
//! The `qrec` binary implements it over `qrec-shell`; [`crate::Standalone`]
//! is the implementation for a build without the shell; tests use fakes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use qrec_core::CoreError;
use qrec_core::protocol::{HelloInfo, Progress, Toast, WireBody};
use serde_json::Value;

use crate::args::HostArgs;

/// A request for a project's host: what `qrec_shell::Request` carries.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectRequest {
    pub body: WireBody,
    /// `--yes` (`Ctx::confirmed`).
    pub confirmed: bool,
}

/// What happens when no host serves the project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// Run the request in this process, which is the project's `cli` host
    /// until it is done and keeps serving IPC while async jobs run (A§8.5).
    InProcess,
    /// Take the lock, spawn `qrec host --headless` with it and send the
    /// request there (A§8.3 step 5): `record start` and `record toggle`.
    SpawnHeadless,
    /// Don't run it: the backend returns [`Finish::NoHost`]. For requests
    /// only a live host can answer: `--over-selection`, `--after-selection`
    /// (the host's selection) and seek/play/pause (the GUI).
    None,
}

/// How a request is routed (A§8.3 step 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteOpts {
    /// How long to look for the host (connect, `hello`, wait for a
    /// starting host's `ready`): 3 s, 20 s for `record *`.
    pub deadline: Duration,
    pub fallback: Fallback,
    /// In-process: take the project lock and become the host (commands).
    /// `false` for queries, which take no lock and write nothing (A§8.4).
    pub lock: bool,
    /// Open the project from `history.head.json` and the history, never
    /// reading `qrec.json` (`OpenOpts::recovery`: `qrec history` and
    /// `qrec history restore`, A§7.7).
    pub recovery: bool,
    /// `record … --wait`: the first Ctrl+C sends `RecordStop` (on a second
    /// connection when routed) and keeps streaming until the take is
    /// finalized; a second one detaches (A§8.6).
    pub stop_on_interrupt: bool,
}

/// The routing deadline of ordinary commands (A§8.3 step 4).
pub const DEADLINE: Duration = Duration::from_secs(3);
/// The routing deadline of `record *`, which may wait for a host that is
/// starting its capture engine (A§8.3 step 4).
pub const RECORD_DEADLINE: Duration = Duration::from_secs(20);

impl RouteOpts {
    /// A query: routed to the live host, else answered in-process without
    /// the lock.
    pub fn query() -> Self {
        Self {
            deadline: DEADLINE,
            fallback: Fallback::InProcess,
            lock: false,
            recovery: false,
            stop_on_interrupt: false,
        }
    }

    /// A command: routed to the live host, else run in-process under the lock.
    pub fn command() -> Self {
        Self {
            lock: true,
            ..Self::query()
        }
    }
}

/// How a request ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Finish {
    /// The host's reply: a `CmdResult`, a query answer or an error.
    Reply(Result<Value, CoreError>),
    /// [`Fallback::None`] and no host serves the project.
    NoHost,
    /// Ctrl+C or SIGTERM (A§8.6): in-process, running jobs were cancelled
    /// and their temp outputs deleted (the second signal kills them at
    /// once); routed, the connection was closed (a GUI host may still
    /// finish the request; headless and CLI hosts cancel the export, frame
    /// save or autocut it started, A§5.8); `--wait`, the second Ctrl+C
    /// detached. `reply` is the host's answer when one came anyway (an
    /// in-process command that finished, or failed because its jobs were
    /// cancelled).
    Interrupted {
        reply: Option<Result<Value, CoreError>>,
    },
}

/// What a request reports while it runs. The CLI prints these as they come.
pub trait Events {
    /// The request reached a host: `Some(hello)` of a live host, `None`
    /// when it runs in this process.
    fn routed(&mut self, host: Option<&HelloInfo>);
    /// The host parsed and queued the request.
    fn accepted(&mut self);
    /// Progress of a long request (export, analysis, recording).
    fn progress(&mut self, progress: &Progress);
    /// A toast for people (`Effect::Notify` of an in-process host).
    fn notice(&mut self, toast: &Toast);
    /// The reply, as soon as it came, of a command this process runs
    /// itself and that no signal interrupted (A§8.5): the process may go on
    /// hosting other clients' requests for a while, and the result
    /// shouldn't wait for them. At most once; the [`Finish`] carries the
    /// same reply. Backends may report replies only through the [`Finish`].
    fn replied(&mut self, reply: &Result<Value, CoreError>);
}

/// The imperative side of the CLI.
pub trait Backend {
    /// `-v` (`verbose` > 0) and `-q`: set up logging before anything runs.
    fn logging(&mut self, verbose: u8, quiet: bool) {
        let _ = (verbose, quiet);
    }

    /// Qrec's user config dir, `$XDG_CONFIG_HOME/qrec` (A§8.2): where
    /// `config.toml` and `keys.toml` live.
    fn config_dir(&mut self) -> Result<PathBuf, CoreError>;

    /// The hosts that answer `hello` on `<runtime>/*.sock` (A§8.4): the
    /// fallback project when the current folder isn't one.
    fn live_hosts(&mut self) -> Result<Vec<HelloInfo>, CoreError>;

    /// Sends one request to the host of the project in `project` (an
    /// absolute folder that holds `qrec.json`, or an empty folder for
    /// `init`), streaming what happens into `events` (A§8.3–A§8.6).
    fn request(
        &mut self,
        project: &Path,
        request: ProjectRequest,
        opts: &RouteOpts,
        events: &mut dyn Events,
    ) -> Finish;

    /// `qrec doctor` (A§8.12): the `DoctorReport` as JSON
    /// (`{checks:[{id,required,ok,detail,hint}], niriSnippet, va}`).
    fn doctor(&mut self) -> Result<Value, CoreError>;

    /// `qrec sources list` (A§8.12): the `SourceInventory` as JSON
    /// (`{screens, cameras, mics, monitors, defaultMic, defaultMonitor}`).
    fn sources(&mut self) -> Result<Value, CoreError>;

    /// `qrec host --headless` (A§8.3 step 5, A§8.6): serves the project
    /// until it is idle, then returns the process's exit code: 0 after
    /// idling out or `shutdown`, 130 after a signal, 1 when it couldn't
    /// start (the host already logged why).
    fn host(&mut self, project: &Path, args: &HostArgs) -> Result<u8, CoreError>;
}
