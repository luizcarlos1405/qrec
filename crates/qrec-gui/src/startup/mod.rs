//! Startup (G§3.3) and the takeover flows (G§3.4.3).
//!
//! The host publishes its socket before GStreamer, doctor and the Session:
//! a niri `qrec record toggle` sent while the window opens queues in this
//! host instead of spawning a headless recorder.
//!
//! | # | where | work |
//! | --- | --- | --- |
//! | 1 | main, before GTK ([`prepare`]) | the folder rule (D-14), `config.toml`, `keys.toml`, the keymap |
//! | 2 | GTK `activate` (`app.rs`) | the window (or X2 / the unreadable folder), keymap problems as toasts, a source discovery on `qrec-gui-io` |
//! | 3 | `qrec-gst` | `gst_init::init()` (≤ 30 s), `ges_render::init()`; stuck → the stuck page (exit 5) |
//! | 4 | `qrec-startup` | the lock (or the takeover pages), then `GuiHost::start` |
//! | 5 | GTK, after 3 | both `gtk4paintablesink`s (`MediaHub::create_sinks`) |
//! | 6 | `qrec-doctor`, after 3 | `doctor::run`; a required check failed → `GuiHost::fail` and S23 (exit 5) |
//! | 7 | once 4, 5 and 6 succeeded | `GuiHost::open(GuiOpener)`: `Opened`, or X3 on a broken `qrec.json` (exit 1) |
//! | 8 | GTK, on `Opened` | the player and the preview cache; the meter monitor follows |
//!
//! Steps 3 and 4 start together. The stage says "Opening <name>…" if step
//! 7 hasn't finished after 1 s. `GuiHost::fail` and a failed open answer
//! every queued request with that error and release the lock at once, so
//! the CLI works in-process while the page shows (`qrec history restore
//! <seq> --yes` can repair an X3 project). "Check again" on S23 re-runs
//! step 6 and, when it passes, steps 4 and 7. A signal before step 4 quits
//! with 130; from step 4 on, the host handles signals.
//!
//! Workers report through the queue (`Inbound::Startup`); the takeover
//! pages answer the `qrec-startup` worker through a [`Choice`] channel.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use gtk::glib;

use qrec_core::config::UserConfig;
use qrec_core::keymap::{self, Keymap};
use qrec_core::model::PROJECT_FILE;
use qrec_core::on_event::output::export_readout;
use qrec_core::state::{HostKind, JobKind, RecordPhase};
use qrec_core::view::blocking::{BlockKind, BlockingAction};
use qrec_core::view::format;
use qrec_core::{Command, CoreError, ErrorCode};
use qrec_shell::capture::LiveControl;
use qrec_shell::doctor::{self, DoctorOptions, DoctorReport};
use qrec_shell::facts::{ConfigFile, SharedSources};
use qrec_shell::host::lockfile::LockRecord;
use qrec_shell::host::signals::Interrupts;
use qrec_shell::host::takeover::{self, Holder, LOCK_TIMEOUT, STARTING_TIMEOUT, STOP_TIMEOUT};
use qrec_shell::host::{ExitRules, GuiHost, GuiHostSpec, GuiPort, HostEvent};
use qrec_shell::ipc::resolve::{Folder, classify};
use qrec_shell::msg::Request;
use qrec_shell::paths::{Env, UserDirs};
use qrec_shell::{config_file, gst_init, keys_file, sources};

use crate::GuiArgs;
use crate::app::App;
use crate::host::Port;
use crate::host::opener::{GuiOpener, PREVIEW_MAX};
use crate::host::queue::{Inbound, Queue};
use crate::store::local::StartupPhase;
use crate::testctl;

/// How long before the stage says "Opening <name>…".
pub const SLOW_OPEN: Duration = Duration::from_secs(1);
/// How often the X1 page asks the recording host again.
pub const RECORDING_POLL: Duration = Duration::from_secs(1);
/// How often the export wait asks the exporting host again.
pub const EXPORT_POLL: Duration = Duration::from_millis(500);

/// What a startup worker reports (through `Inbound::Startup`).
#[derive(Debug)]
pub enum StartupEvent {
    /// Step 3 finished.
    Gst(Result<(), GstFailure>),
    /// Step 6 finished.
    Doctor(Arc<DoctorReport>),
    /// Step 4: the host serves (the lock is ours, the socket published).
    HostStarted(Arc<GuiHost>),
    /// Step 4 couldn't start the host.
    HostFailed(CoreError),
    /// A takeover page to show or update (X1, D-29, D-13, not responding).
    Page(BlockKind),
    /// A takeover step failed: toast it; the page stays.
    TakeoverError(CoreError),
    /// A signal came before step 4: quit with 130.
    Interrupted,
    /// The capture engine's live control (from the opener).
    LiveControl(LiveControl),
}

/// Why GStreamer isn't usable (step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GstFailure {
    /// `gst::init()` didn't return within 30 s (or `QREC_GUI_TEST=stuck-gst`).
    Stuck,
    /// It returned an error.
    Failed(String),
}

/// What a takeover page's buttons tell the `qrec-startup` worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// X1: stop the other host's recording, then open.
    StopAndOpen,
    /// D-29: wait for the export, then open.
    Wait,
    /// The person quit (the window exits; the worker stops waiting).
    Quit,
}

// ------------------------------------------------------------------ step 1

/// What startup step 1 found, before GTK.
pub struct Prepared {
    /// The canonical project folder (as given when it can't be read).
    pub dir: PathBuf,
    /// The folder's name: the window title.
    pub name: String,
    /// Open it, or show this page first.
    pub folder: Result<FolderKind, BlockKind>,
    pub env: Env,
    pub dirs: Option<UserDirs>,
    pub config: UserConfig,
    /// The `config.toml` the Session reports problems of (A§4.2).
    pub config_file: Option<ConfigFile>,
    pub keymap: Keymap,
    /// `keymap::problems`, plus why `keys.toml` couldn't be read.
    pub keymap_problems: Vec<String>,
    pub interrupts: Interrupts,
}

/// The folder at step 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderKind {
    /// A project (or one that lost its `qrec.json`).
    Project,
    /// An empty folder: `Init` runs before any request (D-35).
    Empty,
}

/// Step 1 (G§3.3): canonicalize the folder, apply the folder rule (no live
/// host fallback, no ancestor project: D-14), read `config.toml` and
/// `keys.toml` (≤ 1 MiB each) and resolve the keymap.
pub fn prepare(args: &GuiArgs, interrupts: Interrupts) -> Prepared {
    let env = Env::from_process();
    let dirs = UserDirs::resolve(&env);
    let home = env.get("HOME").map(|h| h.to_string_lossy().into_owned());
    let shown = |dir: &Path| {
        let text = dir.to_string_lossy();
        format::display_path(&text, "", home.as_deref())
    };
    let (dir, folder) = match fs::canonicalize(&args.project) {
        Err(e) => (
            args.project.clone(),
            Err(BlockKind::FolderUnreadable {
                dir: shown(&args.project),
                error: e.to_string(),
            }),
        ),
        Ok(dir) => {
            let folder = match classify(&dir, true) {
                Ok(Folder::Project | Folder::MissingFile) => Ok(FolderKind::Project),
                Ok(Folder::Empty) => Ok(FolderKind::Empty),
                Ok(Folder::Other) => Err(BlockKind::NotMySwamp {
                    dir: shown(&dir),
                    inside: dir
                        .ancestors()
                        .skip(1)
                        .find(|a| fs::symlink_metadata(a.join(PROJECT_FILE)).is_ok())
                        .map(shown),
                }),
                Err(e) => Err(BlockKind::FolderUnreadable {
                    dir: shown(&dir),
                    error: e.to_string(),
                }),
            };
            (dir, folder)
        }
    };
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned());
    let mut keymap_problems = Vec::new();
    let (config, config_file, keys_text) = match &dirs {
        Ok(dirs) => {
            let (config, file) = config_file::read(&dirs.config_file());
            let keys = keys_file::read(&dirs.keys_file()).unwrap_or_else(|why| {
                keymap_problems.push(format!("{why}; using the default keys"));
                String::new()
            });
            (config, Some(file), keys)
        }
        Err(e) => {
            tracing::warn!("no user dirs ({e}): default settings and keys");
            (UserConfig::default(), None, String::new())
        }
    };
    let keymap = keymap::resolve(&keys_text);
    keymap_problems.extend(keymap::problems(&keymap));
    let folder = match (folder, &dirs) {
        (Ok(_), Err(e)) => Err(BlockKind::HostFailed { error: e.clone() }),
        (folder, _) => folder,
    };
    Prepared {
        dir,
        name,
        folder,
        env,
        dirs: dirs.ok(),
        config,
        config_file,
        keymap,
        keymap_problems,
        interrupts,
    }
}

// --------------------------------------------------------------- workers

/// Step 3 on `qrec-gst`: GStreamer and GES, or stuck. Under
/// `QREC_GUI_TEST=stuck-gst` it reports stuck without initializing.
pub fn spawn_gst(queue: Queue) {
    let spawned = thread::Builder::new()
        .name("qrec-gst".into())
        .spawn(move || {
            let result = if testctl::mode().stuck_gst {
                tracing::info!("test mode: GStreamer reported stuck without starting it");
                Err(GstFailure::Stuck)
            } else {
                match qrec_shell::ges_render::init() {
                    Ok(()) => Ok(()),
                    Err(qrec_shell::ges_render::GesError::Init(gst_init::InitError::Stuck)) => {
                        Err(GstFailure::Stuck)
                    }
                    Err(e) => Err(GstFailure::Failed(e.to_string())),
                }
            };
            queue.push(Inbound::Startup(StartupEvent::Gst(result)));
        });
    if let Err(e) = spawned {
        tracing::error!("can't start the GStreamer thread: {e}");
    }
}

/// Step 6 on `qrec-doctor`.
pub fn spawn_doctor(queue: Queue, config_vaapi_device: Option<String>) {
    let spawned = thread::Builder::new()
        .name("qrec-doctor".into())
        .spawn(move || {
            let opts = DoctorOptions {
                fake_backend: sources::fake_backend(),
                project_vaapi_device: None,
                config_vaapi_device,
                probe_vaapi: false,
            };
            let report = doctor::run(&opts);
            queue.push(Inbound::Startup(StartupEvent::Doctor(Arc::new(report))));
        });
    if let Err(e) = spawned {
        tracing::error!("can't start the doctor thread: {e}");
    }
}

/// What the `qrec-startup` worker needs.
pub struct TakeoverWorker {
    pub dir: PathBuf,
    pub env: Env,
    pub interrupts: Interrupts,
    pub queue: Queue,
    pub port: Arc<dyn GuiPort>,
    /// D-35: `Init` for an empty folder.
    pub first: Option<Request>,
    pub choices: Receiver<Choice>,
}

/// What the worker does after looking at a live holder.
enum Next {
    /// Look again (the holder changed, or let go).
    Retry,
    /// Done: the host started, a page shows, or the person quit.
    Done,
}

impl TakeoverWorker {
    /// Step 4 on `qrec-startup`.
    pub fn spawn(self) {
        let spawned = thread::Builder::new()
            .name("qrec-startup".into())
            .spawn(move || self.run());
        if let Err(e) = spawned {
            tracing::error!("can't start the startup thread: {e}");
        }
    }

    fn push(&self, event: StartupEvent) {
        self.queue.push(Inbound::Startup(event));
    }

    fn interrupted(&self) -> bool {
        self.interrupts.count() > 0
    }

    fn run(self) {
        let mut starting_since: Option<Instant> = None;
        loop {
            if self.interrupted() {
                self.push(StartupEvent::Interrupted);
                return;
            }
            match takeover::find_holder(&self.dir, &self.env) {
                Err(e) => {
                    self.push(StartupEvent::HostFailed(e));
                    return;
                }
                Ok(Holder::Free(lock)) => {
                    self.start(lock);
                    return;
                }
                Ok(Holder::Starting) => {
                    let since = *starting_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= STARTING_TIMEOUT {
                        let page = match takeover::unresponsive(&self.dir) {
                            Holder::Unresponsive(record) => not_responding(record.as_ref()),
                            _ => not_responding(None),
                        };
                        self.push(StartupEvent::Page(page));
                        return;
                    }
                    thread::sleep(takeover::RETRY);
                }
                Ok(Holder::Unresponsive(record)) => {
                    self.push(StartupEvent::Page(not_responding(record.as_ref())));
                    return;
                }
                Ok(Holder::Host { hello, socket }) => {
                    starting_since = None;
                    let next = if hello.host == HostKind::Gui {
                        self.push(StartupEvent::Page(BlockKind::HeldByWindow {
                            pid: hello.pid,
                        }));
                        Next::Done
                    } else if !hello.record.is_idle() {
                        self.recording(&hello.record, hello.host, &socket)
                    } else if let Some(job) = export_job(&hello.jobs) {
                        self.export_wait(job, hello.host, &socket)
                    } else {
                        // Idle: a silent `shutdown`, then the lock.
                        tracing::info!(
                            "asking the idle {} host (pid {}) to let go of the project",
                            hello.host.as_str(),
                            hello.pid
                        );
                        self.shut_down_and_take(&socket)
                    };
                    match next {
                        Next::Retry => continue,
                        Next::Done => return,
                    }
                }
            }
        }
    }

    /// The lock is ours: start the host.
    fn start(&self, lock: qrec_shell::lock::ProjectLock) {
        if self.interrupted() {
            self.push(StartupEvent::Interrupted);
            return;
        }
        let spec = GuiHostSpec {
            dir: self.dir.clone(),
            lock,
            env: self.env.clone(),
            interrupts: Some(self.interrupts.clone()),
            rules: ExitRules::default(),
            first: self.first.clone(),
        };
        match GuiHost::start(spec, Arc::clone(&self.port)) {
            Ok(host) => self.push(StartupEvent::HostStarted(Arc::new(host))),
            Err(e) => self.push(StartupEvent::HostFailed(e)),
        }
    }

    /// `shutdown` to the holder, then its lock (≤ 20 s); the host starts.
    fn shut_down_and_take(&self, socket: &Path) -> Next {
        if let Err(e) = takeover::request_shutdown(socket) {
            tracing::warn!("the holder refused to shut down: {e}");
        }
        match takeover::wait_for_lock(&self.dir, LOCK_TIMEOUT) {
            Ok(lock) => {
                self.start(lock);
                Next::Done
            }
            Err(e) => {
                self.push(StartupEvent::HostFailed(e));
                Next::Done
            }
        }
    }

    /// X1: another host records. The page shows the take and its time
    /// (`hello` every second); Stop & open stops it (≤ 60 s), shuts the
    /// host down and takes the lock; Quit leaves it recording.
    fn recording(&self, phase: &RecordPhase, kind: HostKind, socket: &Path) -> Next {
        let Some(page) = recording_page(phase, false) else {
            // A countdown or a finalizing take: ask again shortly.
            thread::sleep(RECORDING_POLL);
            return Next::Retry;
        };
        self.push(StartupEvent::Page(page));
        let mut latest = phase.clone();
        loop {
            match self.choices.recv_timeout(RECORDING_POLL) {
                Ok(Choice::Quit) | Err(RecvTimeoutError::Disconnected) => return Next::Done,
                Ok(Choice::Wait) => {}
                Ok(Choice::StopAndOpen) => {
                    if let Some(page) = recording_page(&latest, true) {
                        self.push(StartupEvent::Page(page));
                    }
                    match takeover::stop_recording(socket, STOP_TIMEOUT) {
                        Ok(_) => {}
                        // It ended meanwhile: fine.
                        Err(e) if e.code == ErrorCode::Invalid => {
                            tracing::info!("the recording ended meanwhile: {e}");
                        }
                        Err(e) => {
                            self.push(StartupEvent::TakeoverError(e));
                            return Next::Retry;
                        }
                    }
                    tracing::info!("stopped the {} host's recording", kind.as_str());
                    return self.shut_down_and_take(socket);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            if self.interrupted() {
                self.push(StartupEvent::Interrupted);
                return Next::Done;
            }
            // Ask again: the time goes on, or the take ended.
            match qrec_shell::ipc::client::hello(socket, qrec_shell::ipc::HELLO_TIMEOUT) {
                Ok(hello) if !hello.record.is_idle() => {
                    if let Some(page) = recording_page(&hello.record, false) {
                        self.push(StartupEvent::Page(page));
                    }
                    latest = hello.record;
                }
                _ => return Next::Retry,
            }
        }
    }

    /// D-29: an export from the CLI runs. The page shows its percent
    /// (`hello` every 500 ms); Wait waits until no job runs, then takes the
    /// lock; Quit leaves it running.
    fn export_wait(&self, job: &qrec_core::state::Job, kind: HostKind, socket: &Path) -> Next {
        let mut waiting = false;
        self.push(StartupEvent::Page(export_page(job, waiting)));
        loop {
            match self.choices.recv_timeout(EXPORT_POLL) {
                Ok(Choice::Quit) | Err(RecvTimeoutError::Disconnected) => return Next::Done,
                Ok(Choice::Wait) => waiting = true,
                Ok(Choice::StopAndOpen) | Err(RecvTimeoutError::Timeout) => {}
            }
            if self.interrupted() {
                self.push(StartupEvent::Interrupted);
                return Next::Done;
            }
            match qrec_shell::ipc::client::hello(socket, qrec_shell::ipc::HELLO_TIMEOUT) {
                Ok(hello) => match export_job(&hello.jobs) {
                    Some(job) => self.push(StartupEvent::Page(export_page(job, waiting))),
                    None if waiting => {
                        // A headless host stays: ask it to go. A CLI command's
                        // host exits by itself.
                        if kind == HostKind::Headless {
                            return self.shut_down_and_take(socket);
                        }
                        return match takeover::wait_for_lock(&self.dir, LOCK_TIMEOUT) {
                            Ok(lock) => {
                                self.start(lock);
                                Next::Done
                            }
                            Err(e) => {
                                self.push(StartupEvent::HostFailed(e));
                                Next::Done
                            }
                        };
                    }
                    None => return Next::Retry,
                },
                Err(_) if waiting => {
                    return match takeover::wait_for_lock(&self.dir, LOCK_TIMEOUT) {
                        Ok(lock) => {
                            self.start(lock);
                            Next::Done
                        }
                        Err(e) => {
                            self.push(StartupEvent::HostFailed(e));
                            Next::Done
                        }
                    };
                }
                Err(_) => return Next::Retry,
            }
        }
    }
}

/// The X1 page for a recording phase; `None` for a countdown or a
/// finalizing take (no take to show yet, or about to end).
pub fn recording_page(phase: &RecordPhase, stopping: bool) -> Option<BlockKind> {
    match phase {
        RecordPhase::Starting { take, .. } => Some(BlockKind::CliRecording {
            take: take.number(),
            elapsed_secs: 0,
            stopping,
        }),
        RecordPhase::Recording {
            take, elapsed_ns, ..
        } => Some(BlockKind::CliRecording {
            take: take.number(),
            elapsed_secs: elapsed_ns / 1_000_000_000,
            stopping,
        }),
        RecordPhase::Idle | RecordPhase::Countdown { .. } | RecordPhase::Finalizing { .. } => None,
    }
}

/// The running export of a host's jobs, if any.
pub fn export_job(jobs: &[qrec_core::state::Job]) -> Option<&qrec_core::state::Job> {
    jobs.iter()
        .find(|job| matches!(job.kind, JobKind::Export { .. }))
}

/// The D-29 page for an export job.
pub fn export_page(job: &qrec_core::state::Job, waiting: bool) -> BlockKind {
    let name = match &job.kind {
        JobKind::Export { out, .. } => out.clone(),
        _ => String::new(),
    };
    BlockKind::ExportWait {
        name,
        percent: export_readout(job, Utc::now()).map(|r| r.percent),
        waiting,
    }
}

/// The "isn't responding" page from what the lock record says.
pub fn not_responding(record: Option<&LockRecord>) -> BlockKind {
    BlockKind::NotResponding {
        pid: record.map(|r| r.pid),
        kind: record.map(|r| format!("{} host", r.kind.as_str())),
        since: record.map(|r| {
            r.since
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        }),
    }
}

/// S23's report: each failed required check and its hint.
pub fn missing_report(report: &DoctorReport) -> String {
    let missing = report.missing();
    let width = missing.iter().map(|c| c.id.len()).max().unwrap_or(0);
    let mut out = String::new();
    for check in missing {
        out.push_str(&format!("{:<width$}  {}\n", check.id, check.detail));
        if let Some(hint) = &check.hint {
            out.push_str(&format!("{:<width$}  → {hint}\n", ""));
        }
    }
    out.trim_end().to_owned()
}

// ------------------------------------------------------------ the machine

/// Where a step is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step<T> {
    Pending,
    Done(T),
}

/// The main thread's side of startup: which steps finished, the host, the
/// page. All methods take `&self` (the App holds it while they call back).
pub struct Startup {
    prepared: Prepared,
    queue: Queue,
    port: Arc<Port>,
    sources: SharedSources,
    gst: RefCell<Step<Result<(), GstFailure>>>,
    sinks: Cell<bool>,
    doctor: RefCell<Step<Arc<DoctorReport>>>,
    host: RefCell<Option<Arc<GuiHost>>>,
    host_stopped: Cell<bool>,
    /// The error to fail the host with once it exists (stuck, S23).
    fail_pending: RefCell<Option<CoreError>>,
    opened: Cell<bool>,
    open_sent: Cell<bool>,
    /// Re-run steps 4 and 7 once the failed host stopped ("Check again").
    restart_when_stopped: Cell<bool>,
    choices: RefCell<Option<Sender<Choice>>>,
    signal_watch: RefCell<Option<glib::SourceId>>,
}

impl Startup {
    pub fn new(prepared: Prepared, queue: Queue) -> Self {
        let port = Arc::new(Port::new(queue.clone()));
        Self {
            prepared,
            queue,
            port,
            sources: SharedSources::new(),
            gst: RefCell::new(Step::Pending),
            sinks: Cell::new(false),
            doctor: RefCell::new(Step::Pending),
            host: RefCell::new(None),
            host_stopped: Cell::new(false),
            fail_pending: RefCell::new(None),
            opened: Cell::new(false),
            open_sent: Cell::new(false),
            restart_when_stopped: Cell::new(false),
            choices: RefCell::new(None),
            signal_watch: RefCell::new(None),
        }
    }

    /// What step 1 found.
    pub fn prepared(&self) -> &Prepared {
        &self.prepared
    }

    /// The device facts cache (refreshed on `qrec-gui-io`).
    pub fn sources(&self) -> &SharedSources {
        &self.sources
    }

    /// The host, while it runs.
    fn host(&self) -> Option<Arc<GuiHost>> {
        self.host.borrow().clone()
    }

    /// A host exists and hasn't stopped.
    pub fn host_alive(&self) -> bool {
        self.host.borrow().is_some() && !self.host_stopped.get()
    }

    /// The window closes (D-12): the host stops accepting and exits once
    /// quiet (within 60 s); `Stopped` follows.
    pub fn close_host(&self) {
        let host = self.host.borrow().clone();
        if let Some(host) = host {
            host.close();
        }
    }

    /// The Session is open.
    pub fn is_open(&self) -> bool {
        self.opened.get()
    }

    /// Steps 3 and 4, together, right after step 1 (before GTK starts, so
    /// the lock and the socket are there within milliseconds of launch: a
    /// `qrec record toggle` sent while the window opens queues in this host
    /// instead of spawning a headless recorder). Not for X2 and the
    /// unreadable folder. Their results wait in the queue for the window.
    pub fn start_workers(&self) {
        spawn_gst(self.queue.clone());
        self.spawn_takeover();
    }

    /// The queue the workers report to.
    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// From `activate`: the "Opening <name>…" caption after 1 s.
    pub fn begin(&self, app: &Rc<App>) {
        let weak = Rc::downgrade(app);
        glib::timeout_add_local_once(SLOW_OPEN, move || {
            if let Some(app) = weak.upgrade() {
                app.ctx.store.update_local(|l| {
                    if l.startup == (StartupPhase::Opening { slow: false }) {
                        l.startup = StartupPhase::Opening { slow: true };
                    }
                });
            }
        });
    }

    /// Watches for a signal while no host handles them (before step 4, on
    /// pages after the host stopped): quit with 130.
    pub fn watch_signals(&self, app: &Rc<App>) {
        let weak = Rc::downgrade(app);
        let id = glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(app) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if app.startup.prepared.interrupts.count() > 0 && !app.startup.host_alive() {
                tracing::info!("a signal while no host runs: quitting");
                app.quit(130);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.signal_watch.borrow_mut() = Some(id);
    }

    fn spawn_takeover(&self) {
        let (tx, rx) = crossbeam_channel::unbounded();
        *self.choices.borrow_mut() = Some(tx);
        let first = match self.prepared.folder {
            Ok(FolderKind::Empty) => Some(Request::command(
                Command::Init {
                    canvas: None,
                    name: None,
                },
                false,
            )),
            _ => None,
        };
        TakeoverWorker {
            dir: self.prepared.dir.clone(),
            env: self.prepared.env.clone(),
            interrupts: self.prepared.interrupts.clone(),
            queue: self.queue.clone(),
            port: Arc::clone(&self.port) as Arc<dyn GuiPort>,
            first,
            choices: rx,
        }
        .spawn();
    }

    /// A takeover page's button.
    pub fn choose(&self, choice: Choice) {
        if let Some(tx) = &*self.choices.borrow() {
            let _ = tx.send(choice);
        }
    }

    /// A startup worker's result.
    pub fn on_event(&self, app: &Rc<App>, event: StartupEvent) {
        tracing::debug!("startup: {event:?}");
        match event {
            StartupEvent::Gst(result) => self.on_gst(app, result),
            StartupEvent::Doctor(report) => self.on_doctor(app, report),
            StartupEvent::HostStarted(host) => {
                app.ctx.store.set_host(Some(Arc::clone(&host)));
                *self.host.borrow_mut() = Some(host);
                self.host_stopped.set(false);
                let pending = self.fail_pending.borrow().clone();
                if let Some(err) = pending {
                    self.fail_host(err);
                }
                self.try_open(app);
            }
            StartupEvent::HostFailed(error) => {
                tracing::warn!("the host can't start: {error}");
                self.block(app, BlockKind::HostFailed { error });
            }
            StartupEvent::Page(page) => self.block(app, page),
            StartupEvent::TakeoverError(error) => {
                app.ctx.store.notify(
                    qrec_core::view::notices::Notice::new(
                        qrec_core::protocol::ToastLevel::Error,
                        format::capitalized(&error.message),
                    )
                    .with_body(error.hint.clone().unwrap_or_default()),
                );
            }
            StartupEvent::Interrupted => app.quit(130),
            StartupEvent::LiveControl(control) => app.ctx.media.set_live_control(control),
        }
    }

    fn on_gst(&self, app: &Rc<App>, result: Result<(), GstFailure>) {
        *self.gst.borrow_mut() = Step::Done(result.clone());
        match result {
            Ok(()) => {
                // Step 5 on this (the GTK) thread, then step 6.
                match app.ctx.media.create_sinks() {
                    Ok(()) => {}
                    Err(e) => tracing::warn!("no paintable sinks: {e}"),
                }
                self.sinks.set(true);
                self.run_doctor();
            }
            Err(GstFailure::Stuck) => {
                let err = CoreError::new(ErrorCode::MissingDependency, gst_init::STUCK_MESSAGE);
                self.fail_host(err);
                self.block(app, BlockKind::GstStuck);
            }
            Err(GstFailure::Failed(message)) => {
                // The doctor says what is missing (its GStreamer check fails).
                tracing::warn!("GStreamer: {message}");
                self.run_doctor();
            }
        }
    }

    fn run_doctor(&self) {
        *self.doctor.borrow_mut() = Step::Pending;
        spawn_doctor(
            self.queue.clone(),
            self.prepared.config.encoder.vaapi_device.clone(),
        );
    }

    fn on_doctor(&self, app: &Rc<App>, report: Arc<DoctorReport>) {
        *self.doctor.borrow_mut() = Step::Done(Arc::clone(&report));
        let shared = Arc::clone(&report);
        app.ctx.store.update_local(move |l| l.doctor = Some(shared));
        match report.missing_dependency() {
            Some(err) => {
                tracing::warn!("startup: {err}");
                self.fail_host(err);
                self.block(
                    app,
                    BlockKind::MissingDeps {
                        report: missing_report(&report),
                    },
                );
            }
            None => {
                *self.fail_pending.borrow_mut() = None;
                if matches!(
                    app.ctx.store.local().startup,
                    StartupPhase::Blocked(BlockKind::MissingDeps { .. })
                ) {
                    // "Check again" passed: steps 4 and 7 again.
                    app.ctx
                        .store
                        .update_local(|l| l.startup = StartupPhase::Opening { slow: true });
                    if self.host.borrow().is_none() || self.host_stopped.get() {
                        self.restart_host();
                    } else {
                        self.restart_when_stopped.set(true);
                    }
                }
                self.try_open(app);
            }
        }
    }

    /// `GuiHost::fail` now, or as soon as the host exists.
    fn fail_host(&self, err: CoreError) {
        *self.fail_pending.borrow_mut() = Some(err.clone());
        if let Some(host) = self.host.borrow().as_ref()
            && !self.open_sent.get()
        {
            host.fail(err);
        }
    }

    fn restart_host(&self) {
        *self.host.borrow_mut() = None;
        self.host_stopped.set(false);
        self.restart_when_stopped.set(false);
        self.spawn_takeover();
    }

    /// Step 7, once steps 4, 5 and 6 succeeded.
    fn try_open(&self, app: &Rc<App>) {
        let ready = self.sinks.get()
            && matches!(&*self.gst.borrow(), Step::Done(Ok(())))
            && matches!(&*self.doctor.borrow(), Step::Done(r) if r.ok())
            && self.fail_pending.borrow().is_none();
        if !ready || self.open_sent.get() {
            return;
        }
        let Some(host) = self.host() else {
            return;
        };
        if self.host_stopped.get() {
            return;
        }
        self.open_sent.set(true);
        let scale = u32::try_from(app.scale_factor().max(1)).unwrap_or(1);
        let queue = self.queue.clone();
        let opener = GuiOpener {
            port: Arc::clone(&self.port) as Arc<dyn GuiPort>,
            config: self
                .prepared
                .config_file
                .clone()
                .unwrap_or_else(|| ConfigFile::new(PathBuf::from("/nonexistent/config.toml"))),
            sources: self.sources.clone(),
            side: app.ctx.media.side_sender(),
            live_sink: app.ctx.media.live_sink(),
            max_size: (PREVIEW_MAX.0 * scale, PREVIEW_MAX.1 * scale),
            on_control: Box::new(move |control| {
                queue.push(Inbound::Startup(StartupEvent::LiveControl(control)));
            }),
        };
        tracing::info!("opening {}", self.prepared.dir.display());
        host.open(Box::new(opener));
    }

    /// The host's life.
    pub fn on_host(&self, app: &Rc<App>, event: HostEvent) {
        tracing::info!("host: {event:?}");
        match event {
            HostEvent::Ready => {}
            HostEvent::Opened => {
                self.opened.set(true);
                app.ctx.store.update_local(|l| {
                    if !matches!(l.startup, StartupPhase::Closing) {
                        l.startup = StartupPhase::Open;
                    }
                });
                app.ctx.media.on_opened(&self.prepared.dir);
                app.on_opened();
            }
            HostEvent::OpenFailed(error) => {
                let page = if is_unreadable(&error) {
                    BlockKind::Unreadable { error }
                } else {
                    BlockKind::HostFailed { error }
                };
                self.block(app, page);
            }
            HostEvent::Failed(error) => tracing::info!("the host stopped serving: {error}"),
            HostEvent::Crashed(message) => self.block(
                app,
                BlockKind::HostFailed {
                    error: CoreError::internal(format!("the Qrec session crashed: {message}")),
                },
            ),
            HostEvent::Stopped { end } => {
                self.host_stopped.set(true);
                app.ctx.store.set_host(None);
                if self.restart_when_stopped.get() {
                    self.open_sent.set(false);
                    self.restart_host();
                    return;
                }
                self.open_sent.set(false);
                app.on_host_stopped(end);
            }
        }
    }

    /// Shows a blocking page (it replaces the editor).
    fn block(&self, app: &Rc<App>, page: BlockKind) {
        app.ctx
            .store
            .update_local(move |l| l.startup = StartupPhase::Blocked(page));
    }

    /// A blocking page's button.
    pub fn on_page_action(&self, app: &Rc<App>, action: BlockingAction) {
        match action {
            BlockingAction::Quit => app.quit_from_page(),
            BlockingAction::CheckAgain => {
                tracing::info!("checking the dependencies again");
                self.open_sent.set(false);
                self.run_doctor();
            }
            BlockingAction::StopAndOpen => self.choose(Choice::StopAndOpen),
            BlockingAction::Wait => self.choose(Choice::Wait),
        }
    }
}

/// A failed open that X3 shows: `qrec.json` invalid or missing.
fn is_unreadable(error: &CoreError) -> bool {
    error.message == "qrec.json is missing"
        || error
            .hint
            .as_deref()
            .is_some_and(|h| h.contains("qrec history restore"))
        || error.message.contains("qrec.json")
}

#[cfg(test)]
mod tests {
    use qrec_core::state::{Job, RecordMode};
    use qrec_core::{JobId, TakeId, testkit};

    use super::*;

    #[test]
    fn recording_pages() {
        let recording = RecordPhase::Recording {
            take: TakeId::numbered(11),
            mode: RecordMode::Append,
            elapsed_ns: 72_400_000_000,
            waiters: vec![],
        };
        assert_eq!(
            recording_page(&recording, false),
            Some(BlockKind::CliRecording {
                take: 11,
                elapsed_secs: 72,
                stopping: false
            })
        );
        assert!(matches!(
            recording_page(&recording, true),
            Some(BlockKind::CliRecording { stopping: true, .. })
        ));
        assert_eq!(recording_page(&RecordPhase::Idle, false), None);
    }

    #[test]
    fn export_wait_pages() {
        let state = testkit::design_state();
        let job = Job {
            id: JobId(1),
            kind: JobKind::Export {
                out: "exports/rust-traits-talk-20261014-153012.mp4".into(),
                files: Default::default(),
                total_frames: 7391,
                seg_frames: vec![7391],
                seg_done: vec![3449],
                stitch_total_ns: 1,
                stitch_done_ns: 0,
                stitching: false,
            },
            req: None,
            started: state.project.created_at,
        };
        let jobs = vec![job];
        let found = export_job(&jobs).unwrap();
        assert_eq!(
            export_page(found, true),
            BlockKind::ExportWait {
                name: "exports/rust-traits-talk-20261014-153012.mp4".into(),
                percent: Some(42),
                waiting: true
            }
        );
        assert!(export_job(&[]).is_none());
    }

    #[test]
    fn not_responding_names_what_the_record_says() {
        assert_eq!(
            not_responding(None),
            BlockKind::NotResponding {
                pid: None,
                kind: None,
                since: None
            }
        );
        let record = LockRecord {
            pid: 4242,
            kind: HostKind::Headless,
            socket: "/run/user/1000/qrec/x.sock".into(),
            since: Utc::now(),
            state: qrec_shell::host::lockfile::HostState::Ready,
        };
        match not_responding(Some(&record)) {
            BlockKind::NotResponding { pid, kind, since } => {
                assert_eq!(pid, Some(4242));
                assert_eq!(kind.as_deref(), Some("headless host"));
                assert_eq!(since.unwrap().len(), 5);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_missing_report_lists_failed_required_checks() {
        use qrec_shell::doctor::Check;
        let report = DoctorReport {
            checks: vec![
                Check {
                    id: "ffmpeg".into(),
                    required: true,
                    ok: false,
                    detail: "ffmpeg wasn't found on PATH".into(),
                    hint: Some("install ffmpeg".into()),
                },
                Check {
                    id: "gstreamer".into(),
                    required: true,
                    ok: true,
                    detail: "1.26".into(),
                    hint: None,
                },
                Check {
                    id: "wl-copy".into(),
                    required: false,
                    ok: false,
                    detail: "not found".into(),
                    hint: Some("install wl-clipboard".into()),
                },
            ],
            niri_snippet: String::new(),
            va: None,
        };
        assert_eq!(
            missing_report(&report),
            "ffmpeg  ffmpeg wasn't found on PATH\n        → install ffmpeg"
        );
    }

    #[test]
    fn unreadable_projects_get_x3() {
        assert!(is_unreadable(&CoreError::invalid("qrec.json is missing")));
        assert!(is_unreadable(
            &CoreError::invalid("qrec.json is invalid: clips[3].out")
                .with_hint("restore with `qrec history restore 41 --yes`")
        ));
        assert!(!is_unreadable(&CoreError::io(
            "can't start the capture engine"
        )));
    }

    #[test]
    fn step_one_applies_the_folder_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("talk");
        fs::create_dir(&project).unwrap();
        fs::write(project.join(PROJECT_FILE), "{}").unwrap();
        let inside = project.join("exports");
        fs::create_dir(&inside).unwrap();
        fs::write(inside.join("a.mp4"), "x").unwrap();
        let empty = tmp.path().join("empty");
        fs::create_dir(&empty).unwrap();
        let other = tmp.path().join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("notes.txt"), "x").unwrap();
        let args = |dir: &Path| GuiArgs {
            project: dir.to_path_buf(),
            explicit: true,
            verbose: 0,
        };
        let interrupts = Interrupts::new();
        let p = prepare(&args(&project), interrupts.clone());
        assert_eq!(p.name, "talk");
        assert!(p.folder.is_ok() || matches!(p.folder, Err(BlockKind::HostFailed { .. })));
        if p.dirs.is_some() {
            assert_eq!(p.folder, Ok(FolderKind::Project));
            assert_eq!(
                prepare(&args(&empty), interrupts.clone()).folder,
                Ok(FolderKind::Empty)
            );
            match prepare(&args(&inside), interrupts.clone()).folder {
                Err(BlockKind::NotMySwamp {
                    inside: Some(_), ..
                }) => {}
                other => panic!("{other:?}"),
            }
            match prepare(&args(&other), interrupts.clone()).folder {
                Err(BlockKind::NotMySwamp { inside: None, .. }) => {}
                other => panic!("{other:?}"),
            }
        }
        match prepare(&args(&tmp.path().join("gone")), interrupts).folder {
            Err(BlockKind::FolderUnreadable { .. }) => {}
            other => panic!("{other:?}"),
        }
    }
}
