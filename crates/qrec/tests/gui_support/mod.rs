//! The GUI test harness (docs/GUI.md G§8.4): `qrec`'s window in a nested
//! headless sway, never on the developer's display.
//!
//! - [`Nested::start`]: a private `TestEnv` (its own `XDG_RUNTIME_DIR`
//!   under `/tmp`), a private session bus without service activation, and
//!   sway headless on one output (`HEADLESS-1`, 1600×1000 logical px @60 by
//!   default, scale `QREC_TEST_SCALE`: the mode is 1600·s × 1000·s device
//!   px). Renderer: pixman by default, with the window drawing in software
//!   (`GDK_DISABLE=gl,vulkan`: nothing opens a GPU node); `QREC_TEST_WLR=gles2`
//!   renders on the first Intel or AMD render node, chosen by vendor (node
//!   numbers swap between boots), and the window uses GL with Mesa's EGL
//!   pinned. Never an NVIDIA node.
//!   Dropping it kills the window, sway and the bus (whole process groups,
//!   by `kill(2)`), and each also dies with the test's thread
//!   (`PR_SET_PDEATHSIG`), so a test binary killed by Ctrl-C or a runner's
//!   timeout leaves none running.
//! - [`Nested::launch`]: `qrec --project <dir> -v` in test mode
//!   (`QREC_GUI_TEST=1`, the fake capture backend, the design inventory);
//!   [`Gui`] talks to its own window's test control channel (G§3.18,
//!   `qrec-gui-test-<pid>.sock`): `waitIdle`, `regions`, `hints`, `local`,
//!   `a11y`, … Its variables can't name a display or a runtime dir: the
//!   window can only reach the nested compositor.
//! - Input: [`Nested::pointer`], a virtual pointer that lives as long as it
//!   is held (headless sway has no pointer device, and one-shot clicks were
//!   lost); [`Nested::keys`], one `wtype` run per key sequence, `-s 400`
//!   first so the compositor has the keyboard's keymap;
//!   [`Nested::keys_held`], a run in the background that keeps its
//!   keyboard (and so the window active: focus rings, inline edits) for a
//!   while after its keys, for a shot taken meanwhile ([`poll`] for what
//!   the keys did, then `waitIdle`).
//! - Output: [`Nested::screenshot`] (`grim -o HEADLESS-1`) into
//!   `<repo>/tmp/gui-shots/<area>/`, with the case's logs in `logs/`: sway,
//!   the bus, each window's output, and (copied when the case ends, passed
//!   or failed) each window's own GUI log and the projects' `.qrec/logs`.
//!   Every tool run (grim, wtype, swaymsg, the CLI) has a deadline.
//! - Checks: [`assert_regions`] (BRIEF §2.1's rects, ±1 px), the `a11y`
//!   walk (launch with `a11y: true`, `GTK_A11Y=test`).
//!
//! The `qrec` binary is the one Cargo built for this test package
//! (`CARGO_BIN_EXE_qrec`); without it the harness panics, never falling
//! back to `target/debug/qrec`.

#![allow(dead_code)]

pub mod pointer;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use qrec_shell::gpu::{VENDOR_AMD, VENDOR_INTEL, VENDOR_NVIDIA, render_nodes};
use qrec_shell::testkit::process::{self, Signal};
use qrec_shell::testkit::{TestEnv, run_tool};
use serde_json::{Value, json};

pub use pointer::VirtualPointer;

/// Mesa's EGL vendor file (A§8.10): glvnd never picks NVIDIA's.
pub const MESA_EGL: &str = "/run/opengl-driver/share/glvnd/egl_vendor.d/50_mesa.json";
/// How long one grim, wtype or swaymsg run may take.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one CLI run may take (`record stop` finalizes a take).
pub const CLI_TIMEOUT: Duration = Duration::from_secs(120);
/// Variables a window can't be given ([`Launch::with_env`]): they would
/// point it at another display than the nested one (G§8.4).
pub const DISPLAY_VARS: [&str; 5] = [
    "XDG_RUNTIME_DIR",
    "WAYLAND_DISPLAY",
    "WAYLAND_SOCKET",
    "DISPLAY",
    "GDK_BACKEND",
];

/// The render node the GL runs use: the first Intel or AMD one, by its PCI
/// vendor (the node numbers aren't stable: on the dev machine NVIDIA's is
/// renderD128 on most boots and renderD129 on others). Never NVIDIA's.
pub fn gl_node() -> String {
    let nodes = render_nodes();
    nodes
        .iter()
        .find(|n| matches!(n.vendor, Some(VENDOR_INTEL | VENDOR_AMD)))
        .filter(|n| n.vendor != Some(VENDOR_NVIDIA))
        .map(|n| n.path.clone())
        .unwrap_or_else(|| {
            panic!("QREC_TEST_WLR=gles2 needs an Intel or AMD render node; found {nodes:?}")
        })
}
/// The nested output's name.
pub const OUTPUT: &str = "HEADLESS-1";

/// The `qrec` binary Cargo built for this package.
pub fn qrec_bin() -> PathBuf {
    match option_env!("CARGO_BIN_EXE_qrec") {
        Some(path) => PathBuf::from(path),
        None => panic!("CARGO_BIN_EXE_qrec isn't set: run the GUI cases with cargo test -p qrec"),
    }
}

/// The repository's root.
pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

/// The compositor's renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renderer {
    /// Software: nothing opens a GPU node (the default; CI).
    Pixman,
    /// `gles2` on [`GL_NODE`] (`QREC_TEST_WLR=gles2`).
    Gles2,
}

impl Renderer {
    fn from_env() -> Self {
        match std::env::var("QREC_TEST_WLR").as_deref() {
            Ok("gles2") => Renderer::Gles2,
            _ => Renderer::Pixman,
        }
    }
}

/// The nested output.
#[derive(Debug, Clone)]
pub struct Options {
    /// The output's mode in device px (logical px × `scale`).
    pub width: u32,
    pub height: u32,
    /// The output's scale (`QREC_TEST_SCALE`, default 1).
    pub scale: f64,
    pub renderer: Renderer,
}

/// `QREC_TEST_SCALE` (default 1).
fn env_scale() -> f64 {
    std::env::var("QREC_TEST_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|s: &f64| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0)
}

impl Default for Options {
    /// BRIEF's 1600×1000 logical px at `QREC_TEST_SCALE`: at 2 the mode is
    /// 3200×2000 device px, the window still 1600×1000 (a 1600×1000 mode
    /// at 2 was 800×500 logical, under the window's minimum).
    fn default() -> Self {
        Self::at_scale(1600, 1000, env_scale())
    }
}

impl Options {
    /// An output of `width`×`height` logical px at the default scale.
    pub fn size(width: u32, height: u32) -> Self {
        Self::at_scale(width, height, env_scale())
    }

    /// An output of `width`×`height` logical px at `scale`.
    pub fn at_scale(width: u32, height: u32, scale: f64) -> Self {
        Self {
            width: (f64::from(width) * scale).round() as u32,
            height: (f64::from(height) * scale).round() as u32,
            scale,
            renderer: Renderer::from_env(),
        }
    }

    /// The output's size in logical px.
    pub fn logical(&self) -> (f64, f64) {
        (
            f64::from(self.width) / self.scale,
            f64::from(self.height) / self.scale,
        )
    }
}

/// A child in its own process group, killed (the whole group) on drop.
struct Group {
    child: Child,
    name: &'static str,
}

impl Group {
    /// `command` in a process group of its own, which also dies with this
    /// thread ([`process::die_with_parent`]: a killed test leaves nothing).
    fn spawn(name: &'static str, mut command: Command, log: &Path) -> Group {
        let out = fs::File::create(log).unwrap_or_else(|e| panic!("{}: {e}", log.display()));
        let err = out.try_clone().expect("clone the log");
        command
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .process_group(0);
        process::die_with_parent(&mut command);
        let child = command
            .spawn()
            .unwrap_or_else(|e| panic!("can't start {name}: {e}"));
        Group { child, name }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn signal_group(&self, signal: Signal) -> bool {
        process::signal_group(self.pid(), signal)
    }

    /// Whether it exited within `within`.
    fn exits_within(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        self.signal_group(Signal::Term);
        if self.exits_within(Duration::from_secs(3)) {
            return;
        }
        if !self.signal_group(Signal::Kill) {
            let _ = self.child.kill();
        }
        if !self.exits_within(Duration::from_secs(5)) {
            eprintln!(
                "gui_support: {} (pid {}) outlived SIGKILL (stuck in the kernel?); left behind",
                self.name,
                self.pid()
            );
        }
    }
}

/// A nested headless sway with a private environment.
pub struct Nested {
    /// The window (dropped first), if launched.
    windows: Vec<Gui>,
    /// The seat's pointer, from before any window maps to the end.
    pointer: VirtualPointer,
    sway: Group,
    bus: Group,
    pub env: TestEnv,
    pub opts: Options,
    /// `WAYLAND_DISPLAY` inside [`TestEnv::xdg_runtime_dir`].
    pub display: String,
    swaysock: PathBuf,
    schemas: String,
    /// Screenshots and logs: `<repo>/tmp/gui-shots/<area>/`.
    pub shots: PathBuf,
    /// The case's name (file name prefix).
    pub case: String,
    /// The projects the case launched or ran the CLI on (their logs are
    /// kept when it ends).
    projects: RefCell<BTreeSet<PathBuf>>,
}

impl Nested {
    /// Starts sway for case `case` of `area` (`wp0b`, …).
    pub fn start(area: &str, case: &str, opts: Options) -> Nested {
        let env = TestEnv::new();
        let runtime = env.xdg_runtime_dir();
        // Never the developer's session (G§8.4): a private runtime dir, never
        // a user's.
        assert!(
            !runtime.starts_with("/run/user/"),
            "the nested compositor's runtime dir {} is a user's",
            runtime.display()
        );
        if let Some(user) = std::env::var_os("XDG_RUNTIME_DIR") {
            assert_ne!(
                Path::new(&user),
                runtime,
                "the nested compositor must not share the user's runtime dir"
            );
        }
        let shots = repo().join("tmp/gui-shots").join(area);
        fs::create_dir_all(shots.join("logs")).expect("create the shots folder");
        let log = |what: &str| shots.join("logs").join(format!("{case}-{what}.log"));

        let bus_conf = runtime.join("bus.conf");
        fs::write(
            &bus_conf,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <busconfig><type>session</type><listen>unix:path={}/bus</listen>\
                 <auth>EXTERNAL</auth><policy context=\"default\">\
                 <allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/>\
                 <allow own=\"*\"/></policy></busconfig>\n",
                runtime.display()
            ),
        )
        .expect("write the bus config");
        let mut bus = env.command(Path::new("dbus-daemon"));
        bus.arg(format!("--config-file={}", bus_conf.display()))
            .args(["--nofork", "--nopidfile"]);
        let bus = Group::spawn("dbus-daemon", bus, &log("bus"));

        let config = runtime.join("sway.cfg");
        fs::write(
            &config,
            format!(
                "output {OUTPUT} mode --custom {}x{}@60Hz position 0 0 scale {}\n\
                 default_border none\ndefault_floating_border none\ngaps inner 0\n\
                 gaps outer 0\nxwayland disable\nswaybg_command -\nswaynag_command -\n\
                 focus_follows_mouse no\n",
                opts.width, opts.height, opts.scale
            ),
        )
        .expect("write the sway config");
        let mut sway = env.command(Path::new("sway"));
        for key in ["SWAYSOCK", "I3SOCK", "NIRI_SOCKET"] {
            sway.env_remove(key);
        }
        sway.env("WLR_BACKENDS", "headless")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .args(["--unsupported-gpu", "-c"])
            .arg(&config);
        match opts.renderer {
            Renderer::Pixman => {
                sway.env("WLR_RENDERER", "pixman");
            }
            Renderer::Gles2 => {
                sway.env("WLR_RENDERER", "gles2")
                    .env("WLR_RENDER_DRM_DEVICE", gl_node())
                    .env("__EGL_VENDOR_LIBRARY_FILENAMES", MESA_EGL);
            }
        }
        let sway = Group::spawn("sway", sway, &log("sway"));

        let mut sway = sway;
        let deadline = Instant::now() + Duration::from_secs(15);
        let display = loop {
            let found = fs::read_dir(&runtime).ok().and_then(|entries| {
                entries.flatten().find_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    (name.starts_with("wayland-") && !name.ends_with(".lock")).then_some(name)
                })
            });
            if let Some(name) = found {
                break name;
            }
            assert!(
                !matches!(sway.child.try_wait(), Ok(Some(_))),
                "sway exited; see {}",
                log("sway").display()
            );
            assert!(
                Instant::now() < deadline,
                "sway didn't start; see {}",
                log("sway").display()
            );
            thread::sleep(Duration::from_millis(50));
        };
        // The display is a socket right in the private runtime dir: a
        // relative name, never a path out of it.
        assert!(
            !display.contains('/'),
            "the nested display {display:?} is a path"
        );
        let socket = runtime.join(&display);
        assert!(
            fs::metadata(&socket).is_ok_and(|m| m.file_type().is_socket())
                && socket.parent() == Some(runtime.as_path()),
            "the nested display {} isn't a socket of the private runtime dir",
            socket.display()
        );
        let swaysock = loop {
            let found = fs::read_dir(&runtime).ok().and_then(|entries| {
                entries.flatten().map(|e| e.path()).find(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("sway-ipc."))
                })
            });
            if let Some(path) = found {
                break path;
            }
            assert!(Instant::now() < deadline, "sway has no IPC socket");
            thread::sleep(Duration::from_millis(50));
        };
        // The pointer exists before any window: windows bind it at once.
        let pointer = VirtualPointer::connect(&runtime.join(&display), opts.logical());
        Nested {
            windows: Vec::new(),
            pointer,
            sway,
            bus,
            env,
            opts,
            display,
            swaysock,
            schemas: schema_dirs(),
            shots,
            case: case.to_owned(),
            projects: RefCell::new(BTreeSet::new()),
        }
    }

    /// The variables a window gets on top of the `TestEnv`'s (G§8.4).
    pub fn display_env(&self) -> Vec<(&'static str, OsString)> {
        let runtime = self.env.xdg_runtime_dir();
        let mut vars: Vec<(&'static str, OsString)> = vec![
            ("WAYLAND_DISPLAY", self.display.clone().into()),
            ("GDK_BACKEND", "wayland".into()),
            ("GSETTINGS_BACKEND", "memory".into()),
            ("GSETTINGS_SCHEMA_DIR", self.schemas.clone().into()),
            (
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/bus", runtime.display()).into(),
            ),
            ("QREC_GUI_TEST", "1".into()),
            ("QREC_FAKE_INVENTORY", "design".into()),
            ("__EGL_VENDOR_LIBRARY_FILENAMES", MESA_EGL.into()),
        ];
        if self.opts.renderer == Renderer::Pixman {
            // No GL at all: nothing opens a render node.
            vars.push(("GDK_DISABLE", "gl,vulkan".into()));
        }
        // The bundled fonts (the dev shell's; `TestEnv` drops `QREC_*`).
        if let Some(fonts) = std::env::var_os("QREC_FONT_DIR") {
            vars.push(("QREC_FONT_DIR", fonts));
        }
        vars
    }

    /// `qrec <args>` as the CLI, in the env (no display), within
    /// [`CLI_TIMEOUT`].
    pub fn cli(&self, project: &Path, args: &[&str]) -> Output {
        self.projects.borrow_mut().insert(project.to_path_buf());
        let out = run_tool(
            self.env
                .command(&qrec_bin())
                .arg("--project")
                .arg(project)
                .args(args),
            CLI_TIMEOUT,
        );
        Output {
            status: out.status,
            stdout: out.stdout,
            stderr: out.stderr.into_bytes(),
        }
    }

    /// `qrec <args>` as the CLI in the background (a `record stop` that
    /// waits for its take, an export): its output goes to
    /// `<shots>/logs/<case>-cli.log`, it dies with the test's thread, and
    /// [`reap_within`] waits for it.
    pub fn cli_background(&self, project: &Path, args: &[&str]) -> Child {
        self.projects.borrow_mut().insert(project.to_path_buf());
        let log = self
            .shots
            .join("logs")
            .join(format!("{}-cli.log", self.case));
        let mut out = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap_or_else(|e| panic!("{}: {e}", log.display()));
        let _ = writeln!(out, "== qrec {}", args.join(" "));
        let err = out.try_clone().expect("clone the log");
        let mut command = self.env.command(&qrec_bin());
        command
            .arg("--project")
            .arg(project)
            .args(args)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        process::die_with_parent(&mut command);
        command
            .spawn()
            .unwrap_or_else(|e| panic!("can't run qrec {args:?}: {e}"))
    }

    /// `qrec <args> --json`'s reply object (the last stdout line).
    pub fn cli_json(&self, project: &Path, args: &[&str]) -> Value {
        let mut all: Vec<&str> = args.to_vec();
        all.push("--json");
        let out = self.cli(project, &all);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let last = stdout
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("{}");
        serde_json::from_str(last).unwrap_or_else(|e| {
            panic!(
                "qrec {args:?}: {e}\nstdout: {stdout}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    /// Launches the window on `project` with `extra` variables (none may
    /// name a display or a runtime dir: [`DISPLAY_VARS`]).
    pub fn launch(&mut self, project: &Path, launch: Launch) -> &mut Gui {
        for (key, _) in &launch.env {
            assert!(
                !DISPLAY_VARS.contains(&key.as_str()),
                "a window can't be given {key}: it would leave the nested compositor (G§8.4)"
            );
        }
        self.projects.borrow_mut().insert(project.to_path_buf());
        let n = self.windows.len();
        let suffix = if n == 0 {
            String::new()
        } else {
            format!("-{}", n + 1)
        };
        let log = self
            .shots
            .join("logs")
            .join(format!("{}-client{suffix}.log", self.case));
        let mut command = self.env.command(&qrec_bin());
        command.envs(self.display_env());
        command.env("GTK_A11Y", if launch.a11y { "test" } else { "none" });
        for (key, value) in &launch.env {
            command.env(key, value);
        }
        if let Some(path) = &launch.path {
            command.env("PATH", path);
        }
        command.arg("--project").arg(project).arg("-v");
        let group = Group::spawn("qrec", command, &log);
        let gui = Gui {
            group,
            runtime: self.env.xdg_runtime_dir(),
            log,
            state_home: self.env.state_home(),
        };
        self.windows.push(gui);
        self.windows.last_mut().expect("just pushed")
    }

    /// The window launched `n`th (0: the first).
    pub fn window(&mut self, n: usize) -> &mut Gui {
        &mut self.windows[n]
    }

    /// A command for a tool on the nested display (never another: the
    /// display variables are the nested ones, `WAYLAND_SOCKET` gone).
    fn nested_tool(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env_remove("WAYLAND_SOCKET")
            .env_remove("DISPLAY")
            .env("WAYLAND_DISPLAY", &self.display)
            .env("XDG_RUNTIME_DIR", self.env.xdg_runtime_dir());
        command
    }

    /// A screenshot of the output: `<shots>/<case>-<name>.png`.
    pub fn screenshot(&self, name: &str) -> PathBuf {
        let path = self.shots.join(format!("{}-{name}.png", self.case));
        let out = run_tool(
            self.nested_tool("grim").args(["-o", OUTPUT]).arg(&path),
            TOOL_TIMEOUT,
        );
        assert!(out.status.success(), "grim failed: {}", out.stderr);
        path
    }

    /// One `wtype` run (one key sequence), after a 400 ms pause for the
    /// virtual keyboard's keymap: `keys(&["-M", "ctrl", "-k", "q", "-m", "ctrl"])`.
    pub fn keys(&self, args: &[&str]) {
        let out = run_tool(
            self.nested_tool("wtype").args(["-s", "400"]).args(args),
            TOOL_TIMEOUT,
        );
        assert!(
            out.status.success(),
            "wtype {args:?} failed: {}",
            out.stderr
        );
    }

    /// One `wtype` run in the background whose virtual keyboard stays
    /// `hold_ms` after its keys: the seat has a keyboard only while a
    /// `wtype` runs, so only then is the window active, with focus rings
    /// drawn and inline edits kept (G§8.4). Its output goes to
    /// `<shots>/logs/<case>-wtype.log`; [`HeldKeys::wait`] reaps it, and
    /// dropping it kills it.
    pub fn keys_held(&self, args: &[&str], hold_ms: u32) -> HeldKeys {
        let log = self
            .shots
            .join("logs")
            .join(format!("{}-wtype.log", self.case));
        let out = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap_or_else(|e| panic!("{}: {e}", log.display()));
        let err = out.try_clone().expect("clone the log");
        let mut command = self.nested_tool("wtype");
        command
            .args(["-s", "400"])
            .args(args)
            .args(["-s", &hold_ms.to_string()])
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        process::die_with_parent(&mut command);
        let child = command.spawn().expect("run wtype");
        HeldKeys {
            child: Some(child),
            within: Duration::from_millis(u64::from(hold_ms)) + TOOL_TIMEOUT,
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    /// A virtual pointer on the output (keep it while clicking).
    pub fn pointer(&mut self) -> &mut VirtualPointer {
        &mut self.pointer
    }

    /// `swaymsg <args>`.
    pub fn swaymsg(&self, args: &[&str]) -> String {
        let out = run_tool(
            Command::new("swaymsg")
                .args(args)
                .env("SWAYSOCK", &self.swaysock),
            TOOL_TIMEOUT,
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The Qrec window's rect as sway lays it out (x, y, w, h).
    pub fn window_rect(&self) -> Option<[f64; 4]> {
        let tree: Value = serde_json::from_str(&self.swaymsg(&["-t", "get_tree"])).ok()?;
        fn find(node: &Value) -> Option<[f64; 4]> {
            if node["app_id"] == "com.rsensato.qrec" {
                let r = &node["rect"];
                return Some([
                    r["x"].as_f64()?,
                    r["y"].as_f64()?,
                    r["width"].as_f64()?,
                    r["height"].as_f64()?,
                ]);
            }
            for key in ["nodes", "floating_nodes"] {
                for child in node[key].as_array().into_iter().flatten() {
                    if let Some(rect) = find(child) {
                        return Some(rect);
                    }
                }
            }
            None
        }
        find(&tree)
    }

    /// Closes the focused window the way the compositor would.
    pub fn close_window(&self) {
        self.swaymsg(&["kill"]);
    }
}

impl Drop for Nested {
    fn drop(&mut self) {
        // The windows first (they exit and flush their logs), then sway and
        // the bus (field order), and the env last: its logs are copied
        // before it goes.
        self.windows.clear();
        self.keep_logs();
    }
}

impl Nested {
    /// Copies the case's logs out of the env (deleted with it), whether
    /// the case passed or failed: each window's own GUI log and the
    /// projects' `.qrec/logs` (headless hosts, recorders, previews,
    /// exports), into `<shots>/logs/<case>-…`. Errors are ignored: this
    /// runs while a failed case unwinds.
    fn keep_logs(&self) {
        let logs = self.shots.join("logs");
        let gui = self.env.state_home().join("qrec");
        for entry in fs::read_dir(&gui).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("gui-") && name.contains(".log") {
                let _ = fs::copy(entry.path(), logs.join(format!("{}-{name}", self.case)));
            }
        }
        for project in self.projects.borrow().iter() {
            let label = project.file_name().map_or_else(
                || "project".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            );
            let dir = project.join(".qrec/logs");
            for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let _ = fs::copy(
                    entry.path(),
                    logs.join(format!("{}-{label}-{name}", self.case)),
                );
            }
        }
    }
}

/// A `wtype` run holding its keyboard ([`Nested::keys_held`]).
pub struct HeldKeys {
    child: Option<Child>,
    /// Its hold plus a tool's deadline.
    within: Duration,
    args: Vec<String>,
}

impl HeldKeys {
    /// Waits for the run to end (its keys typed and its hold over) and
    /// checks it succeeded.
    pub fn wait(mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let deadline = Instant::now() + self.within;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(status.success(), "wtype {:?} failed: {status}", self.args);
                    return;
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("wtype {:?} never exited", self.args);
                }
            }
        }
    }
}

impl Drop for HeldKeys {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take()
            && matches!(child.try_wait(), Ok(None))
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// How to launch a window.
#[derive(Debug, Clone, Default)]
pub struct Launch {
    /// `GTK_A11Y=test` for the `a11y` walk (else `none`).
    pub a11y: bool,
    /// Extra variables (`QREC_GUI_TEST=1,stuck-gst`, fake hooks…).
    pub env: Vec<(String, String)>,
    /// A `PATH` of its own (a missing ffmpeg).
    pub path: Option<OsString>,
}

impl Launch {
    pub fn a11y() -> Self {
        Self {
            a11y: true,
            ..Self::default()
        }
    }

    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }
}

/// A launched window.
pub struct Gui {
    group: Group,
    runtime: PathBuf,
    /// Its stdout and stderr.
    pub log: PathBuf,
    state_home: PathBuf,
}

impl Gui {
    /// The window's process id.
    pub fn pid(&self) -> u32 {
        self.group.pid()
    }

    /// One request on its window's test control channel (G§3.18,
    /// `qrec-gui-test-<pid>.sock`) and its reply.
    pub fn ctl(&self, request: Value) -> Value {
        let socket = self
            .runtime
            .join(format!("qrec-gui-test-{}.sock", self.pid()));
        let wait = request["timeoutMs"].as_u64().unwrap_or(10_000);
        let deadline = Instant::now() + Duration::from_secs(20);
        let stream = loop {
            match UnixStream::connect(&socket) {
                Ok(stream) => break stream,
                Err(e) => {
                    assert!(
                        Instant::now() < deadline,
                        "no test channel at {} ({e}); log {}",
                        socket.display(),
                        self.log.display()
                    );
                    thread::sleep(Duration::from_millis(50));
                }
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_millis(wait + 5_000)))
            .expect("a timeout");
        let mut writer = stream.try_clone().expect("clone the stream");
        writeln!(writer, "{request}").expect("send the request");
        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("no reply to {request}: {e}"));
        serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("{line:?}: {e}"))
    }

    /// `waitIdle` (G§3.18): panics when the window doesn't settle.
    pub fn wait_idle(&self) -> u64 {
        let reply = self.ctl(json!({"op": "waitIdle", "timeoutMs": 30_000}));
        assert_eq!(reply["ok"], true, "waitIdle: {reply}");
        reply["rev"].as_u64().unwrap_or(0)
    }

    /// `regions`, with the widgets named in `names` too.
    pub fn regions(&self, names: &[&str]) -> BTreeMap<String, [f64; 4]> {
        let reply = self.ctl(json!({"op": "regions", "names": names}));
        serde_json::from_value(reply["regions"].clone()).unwrap_or_else(|e| panic!("{reply}: {e}"))
    }

    /// The text of the labels under widget `name`.
    pub fn labels(&self, name: &str) -> Vec<String> {
        let reply = self.ctl(json!({"op": "labels", "name": name}));
        serde_json::from_value(reply["labels"].clone()).unwrap_or_default()
    }

    /// The values of the text fields under widget `name`, by their nearest
    /// given name (`"inspector-x": "1280"`).
    pub fn entries(&self, name: &str) -> BTreeMap<String, String> {
        let reply = self.ctl(json!({"op": "labels", "name": name}));
        serde_json::from_value(reply["entries"].clone()).unwrap_or_default()
    }

    /// The `local` summary.
    pub fn local(&self) -> Value {
        self.ctl(json!({"op": "local"}))
    }

    /// Waits until `local`'s startup phase is `phase` (`open`, `blocked`).
    pub fn wait_phase(&self, phase: &str, within: Duration) -> Value {
        let deadline = Instant::now() + within;
        loop {
            let local = self.local();
            assert_eq!(
                local["pid"].as_u64(),
                Some(u64::from(self.pid())),
                "another window answered: {local}"
            );
            if local["startup"]["phase"] == phase {
                return local;
            }
            assert!(
                Instant::now() < deadline,
                "the window never got to {phase}: {local}; log {}",
                self.log.display()
            );
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Sends `signal` (`TERM`, `INT`) to the window's process.
    pub fn signal(&self, signal: &str) {
        let sig = Signal::from_name(signal).unwrap_or_else(|| panic!("no signal {signal}"));
        assert!(process::signal(self.pid(), sig), "kill -{signal} failed");
    }

    /// Its exit code once it exits within `within` (`None`: still running).
    pub fn wait_exit(&mut self, within: Duration) -> Option<i32> {
        let deadline = Instant::now() + within;
        loop {
            if let Ok(Some(status)) = self.group.child.try_wait() {
                return status.code();
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Whether it still runs.
    pub fn running(&mut self) -> bool {
        matches!(self.group.child.try_wait(), Ok(None))
    }

    /// The window's own log (`$XDG_STATE_HOME/qrec/gui-*.log`): every one of
    /// the env, concatenated.
    pub fn gui_log(&self) -> String {
        let dir = self.state_home.join("qrec");
        let mut text = String::new();
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("gui-") && name.ends_with(".log") {
                text.push_str(&fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
        text
    }

    /// Its stdout and stderr so far.
    pub fn output(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

/// GSettings schema folders from `XDG_DATA_DIRS` (Nix keeps them under
/// `share/gsettings-schemas/<pkg>/glib-2.0/schemas`; libadwaita's fallback
/// without a settings portal needs `org.gnome.desktop.interface`).
fn schema_dirs() -> String {
    let mut dirs = Vec::new();
    let data = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    for base in data.split(':').filter(|d| !d.is_empty()) {
        let base = Path::new(base);
        let mut candidates = vec![base.join("glib-2.0/schemas")];
        for entry in fs::read_dir(base.join("gsettings-schemas"))
            .into_iter()
            .flatten()
            .flatten()
        {
            candidates.push(entry.path().join("glib-2.0/schemas"));
        }
        for dir in candidates {
            if dir.join("gschemas.compiled").is_file() {
                let text = dir.display().to_string();
                if !dirs.contains(&text) {
                    dirs.push(text);
                }
            }
        }
    }
    dirs.join(":")
}

/// BRIEF §2.1 at 1600×1000 (G§8.4's base set; the ruler's corner cell is in
/// the ruler, so the headers and the time area start at y 720).
pub const BRIEF_REGIONS: [(&str, [f64; 4]); 14] = [
    ("region-header", [0.0, 0.0, 1600.0, 48.0]),
    ("region-stage", [0.0, 48.0, 1216.0, 608.0]),
    ("preview", [128.0, 64.0, 960.0, 540.0]),
    ("transport", [128.0, 612.0, 960.0, 36.0]),
    ("region-panel", [1216.0, 48.0, 384.0, 920.0]),
    ("region-timeline", [0.0, 656.0, 1216.0, 312.0]),
    ("timeline-toolbar", [0.0, 656.0, 1216.0, 36.0]),
    ("ruler", [0.0, 692.0, 1216.0, 28.0]),
    ("track-headers", [0.0, 720.0, 176.0, 248.0]),
    ("time-area", [176.0, 720.0, 1040.0, 248.0]),
    ("video-row", [176.0, 720.0, 1040.0, 76.0]),
    ("lane-A1", [176.0, 800.0, 1040.0, 56.0]),
    ("lane-A2", [176.0, 860.0, 1040.0, 56.0]),
    ("region-hintbar", [0.0, 968.0, 1600.0, 32.0]),
];

/// The regions every window has from WP0b on (the named parts come with
/// their owners: WP3, WP4).
pub const WP0B_REGIONS: [&str; 5] = [
    "region-header",
    "region-stage",
    "region-panel",
    "region-timeline",
    "region-hintbar",
];

/// The regions every window on the design fixture has since round 3
/// (WP3, WP4 and WP6 merged; the panel is WP5's since its merge): all of
/// BRIEF §2.1's, the stage's preview and transport (WP4) and the timeline's
/// parts (WP3) with them.
pub const MERGED_REGIONS: [&str; 14] = [
    "region-header",
    "region-stage",
    "preview",
    "transport",
    "region-panel",
    "region-timeline",
    "timeline-toolbar",
    "ruler",
    "track-headers",
    "time-area",
    "video-row",
    "lane-A1",
    "lane-A2",
    "region-hintbar",
];

/// Checks `actual` against `expected` (±1 px × `scale`): every name in
/// `required` must be there; the others are checked when present.
pub fn assert_regions(
    actual: &BTreeMap<String, [f64; 4]>,
    expected: &[(&str, [f64; 4])],
    required: &[&str],
    scale: f64,
) {
    let mut wrong = Vec::new();
    for (name, want) in expected {
        match actual.get(*name) {
            Some(got) => {
                let off = got
                    .iter()
                    .zip(want)
                    .any(|(g, w)| (g - w).abs() > 1.0 * scale.max(1.0));
                if off {
                    wrong.push(format!("{name}: {got:?}, want {want:?}"));
                }
            }
            None if required.contains(name) => wrong.push(format!("{name}: missing")),
            None => {}
        }
    }
    assert!(
        wrong.is_empty(),
        "regions off (±1 px): {wrong:#?}\nall: {actual:#?}"
    );
}

/// The mean grey level (0–255) of `rect` (x, y, w, h in the PNG's px) of
/// a screenshot: ffmpeg's 16×9 grey thumbnail of it.
pub fn mean_grey(png: &Path, [x, y, w, h]: [u32; 4]) -> f64 {
    let out = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(png)
            .args([
                "-vf",
                &format!("crop={w}:{h}:{x}:{y},scale=16:9,format=gray"),
                "-f",
                "rawvideo",
                "-",
            ]),
        TOOL_TIMEOUT,
    );
    assert_eq!(
        out.stdout.len(),
        16 * 9,
        "a 16×9 thumbnail of {}: {}",
        png.display(),
        out.stderr
    );
    out.stdout.iter().map(|&b| f64::from(b)).sum::<f64>() / (16.0 * 9.0)
}

/// The pixels (RGB) of `rect` (x, y, w, h in the PNG's px) of a screenshot,
/// row by row.
pub fn rgb_pixels(png: &Path, [x, y, w, h]: [u32; 4]) -> Vec<[u8; 3]> {
    let out = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(png)
            .args([
                "-vf",
                &format!("crop={w}:{h}:{x}:{y},format=rgb24"),
                "-f",
                "rawvideo",
                "-",
            ]),
        TOOL_TIMEOUT,
    );
    assert_eq!(
        out.stdout.len(),
        (w * h * 3) as usize,
        "{w}×{h} pixels of {}: {}",
        png.display(),
        out.stderr
    );
    out.stdout
        .chunks_exact(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect()
}

/// How many pixels of `rect` are within `tolerance` of `color` on every
/// channel.
pub fn count_near(png: &Path, rect: [u32; 4], color: [u8; 3], tolerance: u8) -> usize {
    rgb_pixels(png, rect)
        .iter()
        .filter(|p| p.iter().zip(color).all(|(a, b)| a.abs_diff(b) <= tolerance))
        .count()
}

/// A color token of a theme (`design/tokens/qrec-tokens.json`), as RGB (its
/// alpha left out).
pub fn token_rgb(theme: &str, name: &str) -> [u8; 3] {
    let path = repo().join("design/tokens/qrec-tokens.json");
    let tokens: Value =
        serde_json::from_str(&fs::read_to_string(path).expect("the tokens")).expect("tokens JSON");
    let hex = tokens["colors"][name][theme]
        .as_str()
        .unwrap_or_else(|| panic!("no color {name} in {theme}"));
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).expect("a hex color");
    [byte(1), byte(3), byte(5)]
}

/// The live picture at BRIEF's 1:2 preview (128, 64, 960, 540) of a
/// 1600×1000 shot isn't black: the fake sources' bars show (a pipeline the
/// engine just rebuilt draws black until its sources deliver, G§3.18).
pub fn assert_live_picture_drawn(png: &Path) {
    let grey = mean_grey(png, [128, 64, 960, 540]);
    assert!(
        grey > 40.0,
        "the live picture is black (mean grey {grey:.1}): {}",
        png.display()
    );
}

/// Polls `check` every 50 ms until it gives `Some` (within `within`): what
/// a held key run (`Nested::keys_held`) waits on before a shot.
pub fn poll<T>(within: Duration, what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "{what} never happened");
        thread::sleep(Duration::from_millis(50));
    }
}

/// Waits for a background child ([`Nested::cli_background`]) to exit
/// within `within`; past that it is killed and the case fails (every wait
/// has a deadline, G§8.4).
pub fn reap_within(mut child: Child, within: Duration, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + within;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            other => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{what} didn't exit within {within:?} ({other:?})");
            }
        }
    }
}

/// Waits until `qrec status --json` says the project's host is a GUI.
pub fn wait_for_gui_host(nested: &Nested, project: &Path, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let status = nested.cli_json(project, &["status"]);
        if status["data"]["host"] == "gui" {
            return status;
        }
        assert!(Instant::now() < deadline, "no GUI host answered: {status}");
        thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default output is BRIEF's 1600×1000 logical px at any scale (at
    /// 2 it was a 1600×1000 mode: 800×500 logical, under the window's
    /// 800×600 minimum, Narrow and compact).
    #[test]
    fn the_default_output_is_1600_by_1000_logical_at_any_scale() {
        for scale in [1.0, 1.5, 2.0] {
            let opts = Options::at_scale(1600, 1000, scale);
            assert_eq!(opts.logical(), (1600.0, 1000.0), "at {scale}");
        }
        let two = Options::at_scale(1600, 1000, 2.0);
        assert_eq!((two.width, two.height), (3200, 2000));
        let default = Options::default();
        assert_eq!(default.logical(), (1600.0, 1000.0));
        assert_eq!(Options::size(1300, 1000).logical(), (1300.0, 1000.0));
    }

    /// A background child that exits is reaped with its status.
    #[test]
    fn reap_within_returns_the_exit_status() {
        let child = Command::new("sh")
            .args(["-c", "exit 3"])
            .spawn()
            .expect("run sh");
        let status = reap_within(child, Duration::from_secs(10), "sh");
        assert_eq!(status.code(), Some(3));
    }

    /// One that outlives its deadline is killed and the case fails (a
    /// `Child::wait` would have waited forever).
    #[test]
    fn reap_within_kills_a_child_past_its_deadline() {
        let child = Command::new("sleep").arg("30").spawn().expect("run sleep");
        let pid = child.id();
        let started = Instant::now();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reap_within(child, Duration::from_millis(200), "sleep 30")
        }));
        let message = caught.expect_err("a panic past the deadline");
        let text = message
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(text.contains("sleep 30 didn't exit within"), "{text}");
        assert!(started.elapsed() < Duration::from_secs(10));
        // Killed and reaped: no such process any more.
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "pid {pid} lives"
        );
    }
}
