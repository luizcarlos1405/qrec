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
//!   lines); a headless host's go to its log.
//! - `doctor` and `sources list` are the media owner's report and inventory
//!   (the fake inventory under the fake backend).
//! - Logging: `tracing` to stderr, filtered by `-q`/`-v` (or `QREC_LOG`).
//!   A headless host's stderr is its `.qrec/logs/…-<pid>-host.log`.

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
use qrec_shell::gpu::GpuInputs;
use qrec_shell::host::signals::{self, HOST_SIGNALS, Interrupts};
use qrec_shell::host::{
    self, ExitRules, HeadlessArgs, HostProgram, HostSession, Opener, RequestContext, RequestEvents,
    SessionLock,
};
use qrec_shell::media::FfmpegRunner;
use qrec_shell::msg::Request;
use qrec_shell::paths::{Env, UserDirs};
use qrec_shell::session::{Notifier, Session, SessionConfig, SessionEnv};
use qrec_shell::sources::{self, Discover, LiveDevices, SourceInventory};
use qrec_shell::{ShellError, caps, ipc};
use serde_json::Value;

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
        }
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
            Role::Host => tracing_subscriber::fmt()
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

/// Who logs: a CLI command, or a host whose stderr is its log file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Cli,
    Host,
}

/// Qrec's own crates, which `-v` makes chattier.
const OWN_CRATES: [&str; 4] = ["qrec", "qrec_cli", "qrec_shell", "qrec_core"];

fn default_directives(role: Role, verbose: u8, quiet: bool, json: bool) -> String {
    let own = |level: &str| {
        let mut directives = vec!["warn".to_owned()];
        directives.extend(OWN_CRATES.iter().map(|c| format!("{c}={level}")));
        directives.join(",")
    };
    match (role, verbose, quiet) {
        (_, 3.., _) => "trace".to_owned(),
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
        let (notices, received) = mpsc::channel();
        let mut opener = ShellOpener {
            notices: Some(notices),
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
        let mut opener = ShellOpener { notices: None };
        let args = HeadlessArgs {
            project: project.to_path_buf(),
            lock_fd: args.lock_fd,
            ready_fd: args.ready_fd,
        };
        Ok(host::main_headless(args, &mut opener))
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
/// about requests of other clients it still serves: they aren't shown.
struct Bridge<'e> {
    events: &'e mut dyn Events,
    notices: mpsc::Receiver<Toast>,
    replied: bool,
}

impl Bridge<'_> {
    fn flush(&mut self) {
        while let Ok(toast) = self.notices.try_recv() {
            if !self.replied {
                self.events.notice(&toast);
            }
        }
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
    notices: Option<mpsc::Sender<Toast>>,
}

impl Opener for ShellOpener {
    fn open(
        &mut self,
        dir: &Path,
        opts: OpenOpts,
        lock: Option<SessionLock>,
    ) -> Result<Box<dyn HostSession>, ShellError> {
        let mut env = SessionEnv::from_process()?;
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
            let mut engine = EngineOptions::new(dir, capture::Backend::from_env(), &env.dirs);
            engine.devices = Some(Arc::new(LiveDevices));
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
        let notifier = self.notices.clone().map(|notices| {
            Box::new(move |toast: &Toast| {
                let _ = notices.send(toast.clone());
            }) as Box<dyn Notifier>
        });
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
    }

    #[test]
    fn json_is_seen_before_the_terminator() {
        let args = |line: &str| -> Vec<OsString> { line.split(' ').map(OsString::from).collect() };
        assert!(wants_json(&args("qrec clip list --json")));
        assert!(!wants_json(&args("qrec clip list -- --json")));
        assert!(!wants_json(&args("qrec clip list")));
    }
}
