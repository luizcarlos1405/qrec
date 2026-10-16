//! The GUI test harness (docs/GUI.md G§8.4): `qrec`'s window in a nested
//! headless sway, never on the developer's display.
//!
//! - [`Nested::start`]: a private `TestEnv` (its own `XDG_RUNTIME_DIR`
//!   under `/tmp`), a private session bus without service activation, and
//!   sway headless on one output (`HEADLESS-1`, 1600×1000@60 by default,
//!   scale `QREC_TEST_SCALE`). Renderer: pixman by default, with the window
//!   drawing in software (`GDK_DISABLE=gl,vulkan`: nothing opens a GPU
//!   node); `QREC_TEST_WLR=gles2` renders on `/dev/dri/renderD129` and the
//!   window uses GL with Mesa's EGL pinned. Never `/dev/dri/renderD128`.
//!   Dropping it kills the window, sway and the bus (whole process groups).
//! - [`Nested::launch`]: `qrec --project <dir> -v` in test mode
//!   (`QREC_GUI_TEST=1`, the fake capture backend, the design inventory);
//!   [`Gui`] talks to its test control channel (G§3.18): `waitIdle`,
//!   `regions`, `hints`, `local`, `a11y`, …
//! - Input: [`Nested::pointer`], a virtual pointer that lives as long as it
//!   is held (headless sway has no pointer device, and one-shot clicks were
//!   lost); [`Nested::keys`], one `wtype` run per key sequence, `-s 400`
//!   first so the compositor has the keyboard's keymap.
//! - Output: [`Nested::screenshot`] (`grim -o HEADLESS-1`) into
//!   `<repo>/tmp/gui-shots/<area>/`, with every log of the case.
//! - Checks: [`assert_regions`] (BRIEF §2.1's rects, ±1 px), the `a11y`
//!   walk (launch with `a11y: true`, `GTK_A11Y=test`).
//!
//! The `qrec` binary is the one Cargo built for this test package
//! (`CARGO_BIN_EXE_qrec`); without it the harness panics, never falling
//! back to `target/debug/qrec`.

#![allow(dead_code)]

pub mod pointer;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use qrec_shell::testkit::TestEnv;
use serde_json::{Value, json};

pub use pointer::VirtualPointer;

/// The render node the GL runs use (the Intel iGPU on the dev machine).
pub const GL_NODE: &str = "/dev/dri/renderD129";
/// Mesa's EGL vendor file (A§8.10): glvnd never picks NVIDIA's.
pub const MESA_EGL: &str = "/run/opengl-driver/share/glvnd/egl_vendor.d/50_mesa.json";
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
    /// The output's mode in device px.
    pub width: u32,
    pub height: u32,
    /// The output's scale (`QREC_TEST_SCALE`, default 1).
    pub scale: f64,
    pub renderer: Renderer,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            width: 1600,
            height: 1000,
            scale: std::env::var("QREC_TEST_SCALE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1.0),
            renderer: Renderer::from_env(),
        }
    }
}

impl Options {
    /// An output of `width`×`height` logical px at the default scale.
    pub fn size(width: u32, height: u32) -> Self {
        let base = Self::default();
        Self {
            width: (f64::from(width) * base.scale).round() as u32,
            height: (f64::from(height) * base.scale).round() as u32,
            ..base
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
    fn spawn(name: &'static str, mut command: Command, log: &Path) -> Group {
        let out = fs::File::create(log).unwrap_or_else(|e| panic!("{}: {e}", log.display()));
        let err = out.try_clone().expect("clone the log");
        command
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .process_group(0);
        let child = command
            .spawn()
            .unwrap_or_else(|e| panic!("can't start {name}: {e}"));
        Group { child, name }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn signal_group(&self, signal: &str) {
        let _ = Command::new("kill")
            .args([
                format!("-{signal}"),
                "--".into(),
                format!("-{}", self.pid()),
            ])
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        self.signal_group("TERM");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.signal_group("KILL");
        let _ = self.child.wait();
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
}

impl Nested {
    /// Starts sway for case `case` of `area` (`wp0b`, …).
    pub fn start(area: &str, case: &str, opts: Options) -> Nested {
        let env = TestEnv::new();
        let runtime = env.xdg_runtime_dir();
        // Never the developer's session (G§8.4).
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
                    .env("WLR_RENDER_DRM_DEVICE", GL_NODE)
                    .env("__EGL_VENDOR_LIBRARY_FILENAMES", MESA_EGL);
            }
        }
        let sway = Group::spawn("sway", sway, &log("sway"));

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
                Instant::now() < deadline,
                "sway didn't start; see {}",
                log("sway").display()
            );
            thread::sleep(Duration::from_millis(50));
        };
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

    /// `qrec <args>` as the CLI, in the env (no display).
    pub fn cli(&self, project: &Path, args: &[&str]) -> Output {
        self.env
            .command(&qrec_bin())
            .arg("--project")
            .arg(project)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run qrec")
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

    /// Launches the window on `project` with `extra` variables.
    pub fn launch(&mut self, project: &Path, launch: Launch) -> &mut Gui {
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

    /// A screenshot of the output: `<shots>/<case>-<name>.png`.
    pub fn screenshot(&self, name: &str) -> PathBuf {
        let path = self.shots.join(format!("{}-{name}.png", self.case));
        let status = Command::new("grim")
            .args(["-o", OUTPUT])
            .arg(&path)
            .env("WAYLAND_DISPLAY", &self.display)
            .env("XDG_RUNTIME_DIR", self.env.xdg_runtime_dir())
            .stderr(Stdio::null())
            .status()
            .expect("run grim");
        assert!(status.success(), "grim failed");
        path
    }

    /// One `wtype` run (one key sequence), after a 400 ms pause for the
    /// virtual keyboard's keymap: `keys(&["-M", "ctrl", "-k", "q", "-m", "ctrl"])`.
    pub fn keys(&self, args: &[&str]) {
        let status = Command::new("wtype")
            .args(["-s", "400"])
            .args(args)
            .env("WAYLAND_DISPLAY", &self.display)
            .env("XDG_RUNTIME_DIR", self.env.xdg_runtime_dir())
            .status()
            .expect("run wtype");
        assert!(status.success(), "wtype {args:?} failed");
    }

    /// A virtual pointer on the output (keep it while clicking).
    pub fn pointer(&mut self) -> &mut VirtualPointer {
        &mut self.pointer
    }

    /// `swaymsg <args>`.
    pub fn swaymsg(&self, args: &[&str]) -> String {
        let out = Command::new("swaymsg")
            .args(args)
            .env("SWAYSOCK", &self.swaysock)
            .output()
            .expect("run swaymsg");
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
        // The windows first, then sway and the bus (field order).
        self.windows.clear();
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

    /// One request on the test control channel (G§3.18) and its reply.
    pub fn ctl(&self, request: Value) -> Value {
        let socket = self.runtime.join("qrec-gui-test.sock");
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

    /// The `local` summary.
    pub fn local(&self) -> Value {
        self.ctl(json!({"op": "local"}))
    }

    /// Waits until `local`'s startup phase is `phase` (`open`, `blocked`).
    pub fn wait_phase(&self, phase: &str, within: Duration) -> Value {
        let deadline = Instant::now() + within;
        loop {
            let local = self.local();
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
        let status = Command::new("kill")
            .args([format!("-{signal}"), self.pid().to_string()])
            .status()
            .expect("run kill");
        assert!(status.success(), "kill -{signal} failed");
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
