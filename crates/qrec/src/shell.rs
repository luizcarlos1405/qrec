//! The CLI's [`Backend`] over the imperative shell (SPEC §4.1: the binary
//! wires everything).
//!
//! - Routing (A§8.3–A§8.6) is the ipc owner's `host::request`: a live host
//!   gets the request over its socket; with none, a command runs in this
//!   process, which becomes the project's `cli` host until it is done;
//!   `record start|toggle` spawn a headless host; GUI-only commands get
//!   [`Finish::NoHost`]. Ctrl+C is handled by the shell's signal counter,
//!   installed only for project requests (afterwards SIGINT no longer kills
//!   the process).
//! - Sessions are opened with the real collaborators: the capture engine
//!   for hosts that record (GUI and headless; GStreamer, initialized lazily,
//!   or the fake backend under `QREC_CAPTURE_BACKEND=fake`; a CLI command's
//!   own host never records), the ffmpeg media runner, the system
//!   clock and OS entropy. Hosts hand their Session a share of the project
//!   lock they hold.
//! - Toasts of an in-process host go to the CLI (stderr, or JSON notice
//!   lines; after the command's reply only the startup recovery's); a
//!   headless host's go to its log. A `config.toml` that can't be used is
//!   reported once per run, before the request is routed (whichever host
//!   answers it), and the Sessions of the run share that reader, so they
//!   don't say it again (A§4.2).
//! - `doctor` and `sources list` are the media owner's report and inventory
//!   (the fake inventory under the fake backend).
//! - Logging: `tracing` to stderr, filtered by `-q`/`-v` (or `QREC_LOG`).
//!   A headless host's stderr is its `.qrec/logs/…-<pid>-host.log`. The
//!   GUI (`qrec` without a command) logs to its project's GUI log
//!   (`UserDirs::gui_log`: appended, a header per run, rotated past 8 MiB;
//!   `info`, `debug` with `-v`), warnings also on stderr (docs/GUI.md
//!   G§3.2), then runs `qrec_gui::run`.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

use qrec_cli::{Backend, Events, Fallback, Finish, HostArgs, ProjectRequest, RouteOpts};
use qrec_core::CoreError;
use qrec_core::protocol::{HelloInfo, Progress, Toast, ToastLevel};
use qrec_core::state::HostKind;
use qrec_shell::capture::{self, EngineOptions};
use qrec_shell::deps::{CaptureEngine, Deps, NoCapture, OpenOpts, os_entropy, system_clock};
use qrec_shell::doctor::{self, DoctorOptions};
use qrec_shell::facts::{ConfigFile, config_problem_toast};
use qrec_shell::gpu::GpuInputs;
use qrec_shell::host::signals::{self, HOST_SIGNALS, Interrupts};
use qrec_shell::host::{
    self, ExitRules, HeadlessArgs, HostProgram, HostSession, Opener, RequestContext, RequestEvents,
    SessionLock,
};
use qrec_shell::media::FfmpegRunner;
use qrec_shell::msg::Request;
use qrec_shell::paths::{self, Env, UserDirs};
use qrec_shell::session::{Notifier, Session, SessionConfig, SessionEnv};
use qrec_shell::sources::{self, Discover, LiveDevices, SourceInventory};
use qrec_shell::{ShellError, caps, ipc};
use serde_json::Value;
use tracing_subscriber::fmt::writer::MakeWriterExt;

/// This build's version, compared with a live host's `hello`.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The `qrec` binary's backend.
pub struct Shell {
    /// The process environment as path resolution reads it.
    env: Env,
    /// The VA devices `main` resolved before anything else ran.
    gpu: GpuInputs,
    /// `--json`: stderr carries only JSON lines, so logs stay off unless `-v`.
    json: bool,
    /// `-v` count and `-q`, once the CLI parsed them.
    verbosity: Option<(u8, bool)>,
    logging_ready: bool,
    interrupts: Option<Interrupts>,
    /// `config.toml` as every Session of this run reads it, so one that
    /// can't be used is reported once per run: an empty folder's lazy init
    /// opens a Session before the command's own (A§4.2, A§8.4).
    config: Option<ConfigFile>,
}

impl Shell {
    /// `args` are the process arguments (for `--json`); `gpu` what `main`
    /// handed to `gpu::prepare_gpu_env`.
    pub fn new(args: &[OsString], gpu: GpuInputs) -> Self {
        Self {
            env: Env::from_process(),
            gpu,
            json: wants_json(args),
            verbosity: None,
            logging_ready: false,
            interrupts: None,
            config: None,
        }
    }

    /// The run's [`ConfigFile`], made on first use; `None` without user
    /// dirs (then no Session opens either).
    fn config_file(&mut self) -> Option<ConfigFile> {
        if self.config.is_none() {
            self.config = self
                .dirs()
                .ok()
                .map(|dirs| ConfigFile::new(dirs.config_file()));
        }
        self.config.clone()
    }

    /// Installs the log subscriber the first time a command needs the
    /// shell. A headless host logs at `info` by default (stderr is its log
    /// file); a CLI only warnings (nothing at all under `--json`).
    fn init_logging(&mut self, role: Role) {
        if self.logging_ready {
            return;
        }
        self.logging_ready = true;
        let (verbose, quiet) = self.verbosity.unwrap_or((0, false));
        let directives = match std::env::var("QREC_LOG") {
            Ok(custom) if !custom.trim().is_empty() => custom,
            _ => default_directives(role, verbose, quiet, self.json),
        };
        let filter = tracing_subscriber::EnvFilter::builder().parse_lossy(directives);
        let ansi = std::io::stderr().is_terminal();
        let _ = match role {
            Role::Host | Role::Gui => tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_target(false)
                .with_thread_names(true)
                .with_timer(LocalTime)
                .try_init(),
            Role::Cli => tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_ansi(ansi)
                .with_target(false)
                .without_time()
                .try_init(),
        };
    }

    /// The GUI's logging (docs/GUI.md G§3.2): the GUI log of the project in
    /// `project` (its canonical path names the file, so GUIs on two projects
    /// never share one), opened for append with a header line, rotated past
    /// 8 MiB and never truncated (a second GUI on the project, which exits
    /// at once, doesn't erase the first one's); warnings also on stderr
    /// (errors only with `-q`). Without user dirs or a writable log, all of
    /// it goes to stderr.
    fn init_gui_logging(&mut self, project: &Path) {
        if self.logging_ready {
            return;
        }
        self.logging_ready = true;
        let (verbose, quiet) = self.verbosity.unwrap_or((0, false));
        let directives = match std::env::var("QREC_LOG") {
            Ok(custom) if !custom.trim().is_empty() => custom,
            _ => default_directives(Role::Gui, verbose, quiet, self.json),
        };
        let filter = tracing_subscriber::EnvFilter::builder().parse_lossy(directives);
        let builder = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_target(false)
            .with_thread_names(true)
            .with_timer(LocalTime);
        let dir = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
        let Ok(dirs) = self.dirs() else {
            let _ = builder.with_writer(std::io::stderr).try_init();
            return;
        };
        let log = dirs.gui_log(&dir);
        match paths::open_gui_log(&log, &dir, chrono::Local::now()) {
            Ok(file) => {
                let terminal = if quiet {
                    tracing::Level::ERROR
                } else {
                    tracing::Level::WARN
                };
                let writer = Arc::new(file).and(std::io::stderr.with_max_level(terminal));
                let _ = builder.with_writer(writer).try_init();
            }
            Err(e) => {
                let _ = builder.with_writer(std::io::stderr).try_init();
                tracing::warn!("can't write the GUI log {}: {e}", log.display());
            }
        }
    }

    /// The process-wide interrupt counter (SIGINT, SIGTERM, SIGHUP),
    /// installed on first use: only project requests react to Ctrl+C
    /// themselves (A§8.6); other commands keep the default (die on it).
    fn interrupts(&mut self) -> Interrupts {
        if let Some(interrupts) = &self.interrupts {
            return interrupts.clone();
        }
        let interrupts = signals::install(&HOST_SIGNALS).unwrap_or_else(|e| {
            tracing::warn!("Ctrl+C won't be handled gracefully: {e}");
            Interrupts::new()
        });
        self.interrupts = Some(interrupts.clone());
        interrupts
    }

    fn dirs(&self) -> Result<UserDirs, CoreError> {
        UserDirs::resolve(&self.env)
    }
}

/// Log lines stamped in local time, like the host's lifecycle lines and the
/// log file names.
struct LocalTime;

impl tracing_subscriber::fmt::time::FormatTime for LocalTime {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(
            w,
            "{}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f")
        )
    }
}

/// Who logs: a CLI command, a host whose stderr is its log file, or the
/// GUI, which logs to its project's GUI log (warnings also on stderr).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Cli,
    Host,
    Gui,
}

/// Qrec's own crates, which `-v` makes chattier.
const OWN_CRATES: [&str; 5] = ["qrec", "qrec_cli", "qrec_shell", "qrec_core", "qrec_gui"];

fn default_directives(role: Role, verbose: u8, quiet: bool, json: bool) -> String {
    let own = |level: &str| {
        let mut directives = vec!["warn".to_owned()];
        directives.extend(OWN_CRATES.iter().map(|c| format!("{c}={level}")));
        directives.join(",")
    };
    match (role, verbose, quiet) {
        (_, 3.., _) => "trace".to_owned(),
        // The GUI's log file: `info`, `debug` with -v, whatever -q says
        // (it only quiets the terminal, G§3.2).
        (Role::Gui, 1.., _) => own("debug"),
        (Role::Gui, _, _) => own("info"),
        (_, 2, _) => own("debug"),
        (_, 1, _) => own("info"),
        // stderr is for JSON lines only, unless -v asked for logs.
        (Role::Cli, _, _) if json => "off".to_owned(),
        (_, _, true) => "error".to_owned(),
        (Role::Host, _, _) => own("info"),
        (Role::Cli, _, _) => "warn".to_owned(),
    }
}

/// `--json` before a `--` terminator (the CLI's own rule).
fn wants_json(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| *a != "--")
        .any(|a| *a == "--json")
}

impl Backend for Shell {
    fn logging(&mut self, verbose: u8, quiet: bool) {
        self.verbosity = Some((verbose, quiet));
    }

    fn config_dir(&mut self) -> Result<PathBuf, CoreError> {
        Ok(self.dirs()?.config)
    }

    /// The hosts' own bounded reader (A§8.2): `qrec config` and `qrec keys`
    /// read the settings files the way every host does.
    fn settings_text(&mut self, path: &Path) -> Result<Option<String>, CoreError> {
        qrec_shell::config_file::read_text(path).map_err(CoreError::io)
    }

    /// The writer the window's Settings pages use (WP9): `qrec config set`
    /// replaces `config.toml` the same way (its permissions kept, a
    /// symlink to it written through).
    fn write_settings(&mut self, path: &Path, text: &str) -> Result<(), CoreError> {
        qrec_shell::config_file::write_text(path, text).map_err(CoreError::io)
    }

    fn live_hosts(&mut self) -> Result<Vec<HelloInfo>, CoreError> {
        let runtime = self.dirs()?.runtime;
        Ok(ipc::resolve::live_hosts(&runtime, ipc::HELLO_TIMEOUT))
    }

    fn request(
        &mut self,
        project: &Path,
        request: ProjectRequest,
        opts: &RouteOpts,
        events: &mut dyn Events,
    ) -> Finish {
        self.init_logging(Role::Cli);
        let interrupts = self.interrupts();
        // What `record start|toggle` spawns: this executable, else the
        // `qrec` on PATH.
        let program = HostProgram::current_exe().unwrap_or_else(|e| {
            tracing::warn!(
                "can't find this executable ({e}); a recording host runs `qrec` from PATH"
            );
            HostProgram {
                program: PathBuf::from("qrec"),
                args: vec!["host".into(), "--headless".into()],
                env: Vec::new(),
            }
        });
        // A config.toml that can't be used is said once per run, whichever
        // host answers: also a live one, or the headless host a recording
        // spawns, whose own warning goes to the GUI or its log (A§4.2). The
        // Sessions this run opens share the file, so they don't repeat it.
        let config = self.config_file();
        if let Some(problem) = config.as_ref().and_then(|file| {
            file.get();
            file.take_problem()
        }) {
            events.notice(&config_problem_toast(problem));
        }
        let (notices, received) = mpsc::channel();
        let mut opener = ShellOpener {
            notices: Some(notices),
            config,
        };
        let cx = RequestContext {
            env: &self.env,
            opener: &mut opener,
            host_program: &program,
            interrupts: &interrupts,
            rules: ExitRules::default(),
            // The second Ctrl+C kills child process groups and ends the
            // command at once, still with its final output (A§8.6).
            on_second_signal: None,
        };
        let request = Request {
            body: request.body,
            confirmed: request.confirmed,
        };
        let mut bridge = Bridge {
            events,
            notices: received,
            replied: false,
        };
        let finish = host::request(cx, project, request, &route_opts(opts), &mut bridge);
        bridge.flush();
        match finish {
            host::Finish::Reply(reply) => Finish::Reply(reply),
            host::Finish::NoHost => Finish::NoHost,
            host::Finish::Interrupted { reply } => Finish::Interrupted { reply },
        }
    }

    fn doctor(&mut self) -> Result<Value, CoreError> {
        self.init_logging(Role::Cli);
        let fake_backend = sources::fake_backend();
        let opts = DoctorOptions {
            fake_backend,
            project_vaapi_device: self.gpu.project_device.clone(),
            config_vaapi_device: self.gpu.config_device.clone(),
            // Test sources record without a GPU: the VA-API check (an
            // encode on the render node) is left out, like the Session's
            // capability detection does under the fake backend.
            probe_vaapi: !fake_backend,
        };
        Ok(doctor::run(&opts).to_json())
    }

    fn sources(&mut self) -> Result<Value, CoreError> {
        self.init_logging(Role::Cli);
        Ok(SourceInventory::discover(Discover::ALL).to_json())
    }

    fn host(&mut self, project: &Path, args: &HostArgs) -> Result<u8, CoreError> {
        self.init_logging(Role::Host);
        let mut opener = ShellOpener {
            notices: None,
            config: None,
        };
        let args = HeadlessArgs {
            project: project.to_path_buf(),
            lock_fd: args.lock_fd,
            ready_fd: args.ready_fd,
        };
        Ok(host::main_headless(args, &mut opener))
    }

    /// `qrec` without a command (docs/GUI.md G§3.2): the GUI's logging,
    /// then the window until it closes; its exit code.
    fn gui(&mut self, project: &Path, explicit: bool) -> Result<u8, CoreError> {
        self.init_gui_logging(project);
        tracing::info!(
            "qrec {VERSION}: the window for {} (from {})",
            project.display(),
            if explicit {
                "--project"
            } else {
                "the current folder"
            }
        );
        let (verbose, _) = self.verbosity.unwrap_or((0, false));
        qrec_gui::run(qrec_gui::GuiArgs {
            project: project.to_path_buf(),
            explicit,
            verbose,
        })
    }
}

/// The CLI's routing options as the shell's (field for field).
fn route_opts(opts: &RouteOpts) -> host::RouteOpts {
    host::RouteOpts {
        deadline: opts.deadline,
        fallback: match opts.fallback {
            Fallback::InProcess => host::Fallback::InProcess,
            Fallback::SpawnHeadless => host::Fallback::SpawnHeadless,
            Fallback::None => host::Fallback::None,
        },
        lock: opts.lock,
        recovery: opts.recovery,
        stop_on_interrupt: opts.stop_on_interrupt,
    }
}

/// The shell's request events, plus the toasts of an in-process host, into
/// the CLI's [`Events`]. Toasts are passed on with the next event and when
/// the request ends, so they come out in order with progress and before the
/// reply. Once the command's own reply is out, the host's later toasts are
/// about requests of other clients it still serves: they aren't shown,
/// except the startup recovery's, which the process finishes for itself
/// after a reply that didn't wait for it (A§5.3, A§8.6).
struct Bridge<'e> {
    events: &'e mut dyn Events,
    notices: mpsc::Receiver<Notice>,
    replied: bool,
}

impl Bridge<'_> {
    fn flush(&mut self) {
        while let Ok(notice) = self.notices.try_recv() {
            if !self.replied || notice.recovery {
                self.events.notice(&notice.toast);
            }
        }
    }
}

/// A toast of an in-process host, and whether it is the startup
/// recovery's (`Notifier::notify_recovery`).
struct Notice {
    toast: Toast,
    recovery: bool,
}

/// An in-process host's toasts, sent to the CLI's [`Bridge`].
struct ToCli(mpsc::Sender<Notice>);

impl Notifier for ToCli {
    fn notify(&mut self, toast: &Toast) {
        let _ = self.0.send(Notice {
            toast: toast.clone(),
            recovery: false,
        });
    }

    fn notify_recovery(&mut self, toast: &Toast) {
        let _ = self.0.send(Notice {
            toast: toast.clone(),
            recovery: true,
        });
    }
}

impl RequestEvents for Bridge<'_> {
    fn routed(&mut self, host: Option<&HelloInfo>) {
        self.flush();
        // A host left running across an upgrade still gets the request
        // (so a recording can be stopped), with a word about it.
        if let Some(hello) = host.filter(|h| h.version != VERSION) {
            self.events.notice(&Toast::new(
                ToastLevel::Warning,
                format!(
                    "the running {} host (pid {}) is qrec {}; this is qrec {VERSION}",
                    hello.host.as_str(),
                    hello.pid,
                    hello.version
                ),
            ));
        }
        self.events.routed(host);
    }

    fn accepted(&mut self) {
        self.flush();
        self.events.accepted();
    }

    fn progress(&mut self, progress: &Progress) {
        self.flush();
        self.events.progress(progress);
    }

    fn replied(&mut self, reply: &Result<Value, CoreError>) {
        // The command's own toasts (e.g. "your video is ready") first.
        self.flush();
        self.replied = true;
        self.events.replied(reply);
    }
}

/// Opens real Sessions for hosts and in-process commands.
struct ShellOpener {
    /// Where an in-process host's toasts go (the CLI shows them); `None`:
    /// the log (a headless host).
    notices: Option<mpsc::Sender<Notice>>,
    /// The run's shared `config.toml` ([`Shell::config_file`]); `None`:
    /// each Session reads its own (a headless host opens one).
    config: Option<ConfigFile>,
}

impl Opener for ShellOpener {
    fn open(
        &mut self,
        dir: &Path,
        opts: OpenOpts,
        lock: Option<SessionLock>,
    ) -> Result<Box<dyn HostSession>, ShellError> {
        let mut env = SessionEnv::from_process()?;
        // The same file for every Session of this run, so a broken one is
        // reported once (the path check only guards against a mix-up).
        if let Some(config) = self.config.clone()
            && config.path() == env.config.path()
        {
            env.config = config;
        }
        // Hosts that record find the `va*h264enc` bound to their render node
        // in a probe child (`main` runs it); never under the fake backend,
        // which records without a GPU.
        if matches!(opts.host, HostKind::Gui | HostKind::Headless) && !env.fake_backend {
            env.caps.gst_factory = Some(caps::gst_factory_probe);
        }
        // Only hosts that record get the engine: a CLI command's own host
        // never records (A§8.5), so a recording never runs inside a command
        // someone started in a terminal.
        let capture: Box<dyn CaptureEngine> = if opts.mutating && opts.host.records() {
            // GStreamer starts with the first live scene or take, so commands
            // that only need ffmpeg never initialize it.
            let backend = capture::Backend::from_env();
            let mut engine = EngineOptions::new(dir, backend, &env.dirs);
            engine.devices = Some(Arc::new(LiveDevices));
            // The fake backend's test hooks are this process's (G§8.4).
            if backend == capture::Backend::Fake {
                engine.fake = capture::FakeHooks::from_env();
            }
            capture::new_engine(engine)
        } else {
            Box::new(NoCapture)
        };
        let deps = Deps {
            capture,
            media: Box::new(FfmpegRunner::new(dir)),
            clock: Box::new(system_clock),
            entropy: Box::new(os_entropy),
        };
        let notifier = self
            .notices
            .clone()
            .map(|notices| Box::new(ToCli(notices)) as Box<dyn Notifier>);
        let cfg = SessionConfig {
            lock,
            env: Some(env),
            notifier,
            ..SessionConfig::default()
        };
        Ok(Box::new(Session::open_with(dir, opts, deps, cfg)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_follow_the_flags() {
        assert_eq!(default_directives(Role::Cli, 0, false, false), "warn");
        assert_eq!(default_directives(Role::Cli, 0, false, true), "off");
        assert_eq!(default_directives(Role::Cli, 0, true, false), "error");
        assert_eq!(default_directives(Role::Cli, 0, true, true), "off");
        assert!(default_directives(Role::Cli, 1, false, true).contains("qrec_shell=info"));
        assert!(default_directives(Role::Host, 0, false, false).contains("qrec_shell=info"));
        assert!(default_directives(Role::Cli, 2, false, false).contains("qrec_core=debug"));
        assert_eq!(default_directives(Role::Host, 3, false, false), "trace");
        // The GUI's log: info, debug with -v, whatever -q or --json say.
        for (quiet, json) in [(false, false), (true, false), (false, true)] {
            let info = default_directives(Role::Gui, 0, quiet, json);
            assert!(info.contains("qrec_gui=info"), "{info}");
            assert!(info.contains("qrec_shell=info"), "{info}");
            let debug = default_directives(Role::Gui, 1, quiet, json);
            assert!(debug.contains("qrec_gui=debug"), "{debug}");
        }
        assert!(OWN_CRATES.contains(&"qrec_gui"));
    }

    #[test]
    fn json_is_seen_before_the_terminator() {
        let args = |line: &str| -> Vec<OsString> { line.split(' ').map(OsString::from).collect() };
        assert!(wants_json(&args("qrec clip list --json")));
        assert!(!wants_json(&args("qrec clip list -- --json")));
        assert!(!wants_json(&args("qrec clip list")));
    }
}
