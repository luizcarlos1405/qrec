//! Qrec on X11 (issue #9): the real binary with the real capture backend
//! against a private Xvfb (`testkit::x11::Xvfb`: its own display number,
//! never 0 or the developer's, and its own cookie), whose screen is split
//! into two RandR monitors as `xrandr --setmonitor` would. Every command
//! gets only that display (`TestEnv::command` removes the session's
//! variables), no session bus (nothing can reach the developer's portal),
//! no PulseAudio server and a `niri` that tells when it runs.
//!
//! Ignored: they start an X server. `scripts/check-full` runs them, as
//! does `scripts/check-quick x11`; the take runs alone
//! (`.config/nextest.toml`):
//!
//! ```sh
//! cargo nextest run -p qrec --run-ignored only --test x11
//! ```

use std::ffi::OsString;
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use qrec_shell::testkit::process::{Signal, die_with_parent, signal_group};
use qrec_shell::testkit::x11::Xvfb;
use qrec_shell::testkit::{TestEnv, ffprobe, run_tool};
use serde_json::Value;

/// The `qrec` under test, as `cli.rs` picks it.
fn qrec_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_qrec")
        .map_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_qrec")), PathBuf::from)
}

/// A private X server whose 1920×1080 screen holds `VIRT-L` (the left
/// half) and `VIRT-R` (the right half), and an isolated env whose `PATH`
/// starts with a `niri` that records its arguments in `niri-calls`.
struct X11Setup {
    xvfb: Xvfb,
    env: TestEnv,
    bin: PathBuf,
}

impl X11Setup {
    fn new() -> Self {
        let xvfb = Xvfb::start(1920, 1080);
        xvfb.set_monitor("VIRT-L", 0, 0, 960, 1080);
        xvfb.set_monitor("VIRT-R", 960, 0, 960, 1080);
        let env = TestEnv::new();
        let bin = env.root().join("bin");
        fs::create_dir(&bin).unwrap();
        let niri = bin.join("niri");
        let calls = env.root().join("niri-calls");
        fs::write(
            &niri,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\necho '{{}}'\n",
                calls.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&niri, fs::Permissions::from_mode(0o755)).unwrap();
        Self { xvfb, env, bin }
    }

    /// `qrec <args>` in `dir` on the X server, with `vars` on top; it must
    /// succeed.
    fn qrec(&self, dir: &Path, args: &[&str], vars: &[(&str, &str)]) -> (Value, String) {
        let (success, reply, stderr) = self.run(dir, args, vars);
        assert!(success, "qrec {args:?}: {reply}\n{stderr}");
        (reply, stderr)
    }

    /// `qrec <args>` in `dir` on the X server, with `vars` on top: whether
    /// it succeeded, its one JSON object on stdout and its stderr.
    fn run(&self, dir: &Path, args: &[&str], vars: &[(&str, &str)]) -> (bool, Value, String) {
        let (success, stdout, stderr) = self.run_text(dir, args, vars);
        let reply = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("qrec {args:?}: {stdout}: {e}\n{stderr}"));
        (success, reply, stderr)
    }

    /// `qrec <args>` in `dir` on the X server, with `vars` on top: whether
    /// it succeeded, its stdout and its stderr.
    fn run_text(&self, dir: &Path, args: &[&str], vars: &[(&str, &str)]) -> (bool, String, String) {
        let mut path = OsString::from(&self.bin);
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let mut command = self.env.command(&qrec_bin());
        command
            .current_dir(dir)
            .args(args)
            .envs(self.xvfb.vars())
            .env("PATH", path)
            .env("QREC_CAPTURE_BACKEND", "real")
            .env("PULSE_SERVER", "unix:/nonexistent")
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", self.env.root().join("no-bus").display()),
            )
            .envs(vars.iter().copied());
        let out = run_tool(&mut command, Duration::from_secs(120));
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        (out.status.success(), stdout, out.stderr)
    }

    /// What the fake `niri` was run with, one line per run.
    fn niri_calls(&self) -> String {
        fs::read_to_string(self.env.root().join("niri-calls")).unwrap_or_default()
    }
}

fn screens(data: &Value) -> Vec<(String, i64, i64, i64, i64)> {
    data["screens"]
        .as_array()
        .unwrap_or_else(|| panic!("no screens in {data}"))
        .iter()
        .map(|s| {
            assert_eq!(s["scale"], 1.0, "{s}");
            let n = |key: &str| s[key].as_i64().unwrap();
            (
                s["output"].as_str().unwrap().to_owned(),
                n("x"),
                n("y"),
                n("width"),
                n("height"),
            )
        })
        .collect()
}

/// `sources list` shows the RandR monitors (and the automatic one of the
/// output they split), and never asks niri; a screen layer takes one. The
/// probe of issue #9 listed no screens and got `not_found` here.
#[test]
#[ignore = "starts a private Xvfb (scripts/check-full)"]
fn x11_screens_are_the_randr_monitors() {
    let x = X11Setup::new();
    let dir = x.env.empty_dir("x11");

    let (list, stderr) = x.qrec(&dir, &["--json", "sources", "list"], &[]);
    let data = &list["data"];
    assert_eq!(
        screens(data),
        [
            ("VIRT-L".to_owned(), 0, 0, 960, 1080),
            ("screen".to_owned(), 0, 0, 1920, 1080),
            ("VIRT-R".to_owned(), 960, 0, 960, 1080),
        ],
        "{stderr}"
    );
    // X11's: no picker, which the first run's caption says (W10).
    assert_eq!(data["screenBackend"], "x11", "{data}");
    let warnings = data["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        !warnings
            .iter()
            .any(|w| w.as_str().unwrap().starts_with("screens:")),
        "{warnings:?}"
    );

    let (add, stderr) = x.qrec(
        &dir,
        &[
            "--json", "scene", "layer", "add", "screen", "--output", "VIRT-R",
        ],
        &[],
    );
    assert_eq!(add["data"]["created"][0], "L1", "{add}\n{stderr}");
    let (status, _) = x.qrec(&dir, &["--json", "status"], &[]);
    let layer = &status["data"]["project"]["scene"]["layers"][0];
    assert_eq!(layer["source"]["output"], "VIRT-R", "{layer}");
    assert_eq!(x.niri_calls(), "", "niri msg never runs on X11");
}

/// `qrec doctor` on the X server (W11): the session line says X11 and
/// its display; ximagesrc and the server (Xvfb's RandR 1.6 and the three
/// screens discovery lists) are what screen capture needs; no portal is
/// asked (there is no session bus here, which X11 doesn't need) and no
/// PipeWire socket looked for; exit 0. A display nothing listens on turns
/// screen capture off in discovery's words. A configured VA device that
/// is no render node keeps the GPU probes out of it.
#[test]
#[ignore = "starts a private Xvfb (scripts/check-full)"]
fn x11_doctor_checks_the_x_server_and_no_portal() {
    let x = X11Setup::new();
    let dir = x.env.empty_dir("doctor");
    let config_dir = x.env.config_home().join("qrec");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "[encoder]\nvaapi_device = \"/nonexistent/renderD999\"\n",
    )
    .unwrap();
    let display = x.xvfb.display();
    let randr = "RandR 1.6: 3 screens (VIRT-L, screen, VIRT-R)";

    let (success, out, stderr) = x.run_text(&dir, &["doctor"], &[]);
    assert!(success, "{out}\n{stderr}");
    assert!(
        out.starts_with(&format!(
            "Session: X11 · screens: x11 (DISPLAY={display})\n\n"
        )),
        "{out}"
    );
    let row = |id: &str| {
        out.lines()
            .find(|line| line.split_whitespace().nth(1) == Some(id))
            .unwrap_or_else(|| panic!("no {id} row in:\n{out}"))
    };
    assert!(row("gst.ximagesrc").starts_with("ok "), "{out}");
    let x11_row = row("x11.randr");
    assert!(
        x11_row.starts_with("ok ") && x11_row.ends_with(randr),
        "{out}"
    );
    for absent in ["portal.", "pipewire.daemon", "gst.pipewiresrc", "wl-copy"] {
        assert!(!out.contains(absent), "{absent}: {out}");
    }
    assert!(!out.contains("Screen capture is off"), "{out}");

    let (success, reply, stderr) = x.run(&dir, &["--json", "doctor"], &[]);
    assert!(success, "{reply}\n{stderr}");
    let report = &reply["data"];
    assert_eq!(report["screenBackend"], "x11", "{report}");
    assert_eq!(report["platform"]["session"], "x11", "{report}");
    let checks = report["checks"].as_array().expect("the checks");
    let check = |id: &str| {
        checks
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("no {id}: {report}"))
    };
    for id in ["gst.ximagesrc", "x11.randr"] {
        let c = check(id);
        assert_eq!(
            (&c["ok"], &c["neededFor"]),
            (&Value::Bool(true), &serde_json::json!(["screens"])),
            "{c}"
        );
    }
    assert_eq!(check("x11.randr")["detail"], randr);
    let off: Vec<&str> = report["featuresOff"]
        .as_array()
        .expect("featuresOff")
        .iter()
        .map(|f| f["feature"].as_str().expect("a feature"))
        .collect();
    assert!(!off.contains(&"screens"), "{report}");

    // Nothing listens on that display (a number no test server takes):
    // screen capture is off, as discovery says why.
    let gone = ":59000";
    let (success, reply, stderr) = x.run(&dir, &["--json", "doctor"], &[("DISPLAY", gone)]);
    assert!(success, "{reply}\n{stderr}");
    let report = &reply["data"];
    let why = format!("X11 (DISPLAY={gone}): can't connect: no X server listens there");
    let randr = report["checks"]
        .as_array()
        .expect("the checks")
        .iter()
        .find(|c| c["id"] == "x11.randr")
        .unwrap_or_else(|| panic!("no x11.randr: {report}"));
    assert_eq!(
        (&randr["ok"], &randr["detail"]),
        (&Value::Bool(false), &Value::from(why.as_str()))
    );
    assert_eq!(
        report["featuresOff"][0]["message"],
        format!("Screen capture is off: {why}"),
        "{report}"
    );
}

/// `QREC_SCREEN_BACKEND=portal` overrides the session: the Wayland path
/// (no compositor here, then `niri msg`) instead of RandR.
#[test]
#[ignore = "starts a private Xvfb (scripts/check-full)"]
fn the_screen_backend_override_wins_over_the_session() {
    let x = X11Setup::new();
    let dir = x.env.empty_dir("portal");
    let (list, stderr) = x.qrec(
        &dir,
        &["--json", "sources", "list"],
        &[("QREC_SCREEN_BACKEND", "portal")],
    );
    assert_eq!(screens(&list["data"]), [], "{stderr}");
    assert_eq!(list["data"]["screenBackend"], "portal", "{list}");
    assert_eq!(x.niri_calls(), "msg --json outputs\n");
}

/// Moving content for the takes, from `gst-launch-1.0`: a 1920×1080
/// `ximagesink` window (no window manager: at 0,0, over both monitors)
/// whose left half (`VIRT-L`) is red and whose right half (`VIRT-R`) is
/// `videotestsrc`'s white ball moving on black. It logs to `content.log`
/// in the env and stops by itself after a minute, or when dropped.
struct Content {
    child: Child,
}

impl Content {
    const LOG: &str = "content.log";

    fn start(x: &X11Setup) -> Self {
        let log = File::create(x.env.root().join(Self::LOG)).unwrap();
        let mut command = x.env.command(Path::new("gst-launch-1.0"));
        command
            .args([
                "videotestsrc",
                "is-live=true",
                "pattern=ball",
                "num-buffers=1800",
            ])
            .args(["!", "video/x-raw,width=960,height=1080,framerate=30/1"])
            .args(["!", "videobox", "left=-960", "fill=red"])
            .args(["!", "videoconvert", "!", "ximagesink"])
            .envs(x.xvfb.vars())
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0);
        die_with_parent(&mut command);
        let child = command
            .spawn()
            .expect("run gst-launch-1.0 (the Nix dev shell has it)");
        let content = Self { child };
        content.wait_shown(x);
        content
    }

    /// Waits (15 s at most) until the window shows: the X server's pixel
    /// at (10, 10) is the left half's red.
    fn wait_shown(&self, x: &X11Setup) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let out = run_tool(
                x.env
                    .command(Path::new("ffmpeg"))
                    .args(["-v", "error", "-f", "x11grab", "-video_size", "2x2", "-i"])
                    .arg(format!("{}+10,10", x.xvfb.display()))
                    .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
                    .envs(x.xvfb.vars()),
                Duration::from_secs(30),
            );
            if out.stdout.starts_with(&[255, 0, 0]) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the content never showed ({:?}, {}); {}:\n{}",
                out.stdout,
                out.stderr,
                Self::LOG,
                fs::read_to_string(x.env.root().join(Self::LOG)).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Content {
    fn drop(&mut self) {
        signal_group(self.child.id(), Signal::Kill);
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Frame `index` of `file`, as 8-bit RGB.
fn frame_rgb(file: &Path, index: u64) -> Vec<u8> {
    let out = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(format!("file:{}", file.display()))
            .args(["-vf", &format!("select=eq(n\\,{index})"), "-frames:v", "1"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"]),
        Duration::from_secs(60),
    );
    assert!(out.status.success(), "{}", out.stderr);
    out.stdout
}

/// How many pixels of an RGB frame are VIRT-L's red.
fn red_pixels(rgb: &[u8]) -> usize {
    rgb.as_chunks::<3>()
        .0
        .iter()
        .filter(|p| p[0] > 128 && p[1] < 64 && p[2] < 64)
        .count()
}

/// Every frame of `file` (960×1080), scaled down to 96×108, as 8-bit RGB.
fn small_frames(file: &Path) -> Vec<Vec<u8>> {
    let out = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(format!("file:{}", file.display()))
            .args([
                "-vf",
                "scale=96:108",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-",
            ]),
        Duration::from_secs(60),
    );
    assert!(out.status.success(), "{}", out.stderr);
    out.stdout
        .chunks(96 * 108 * 3)
        .map(<[u8]>::to_vec)
        .collect()
}

/// The number in `text` between `before` and the next `after`.
fn number_between(text: &str, before: &str, after: &str) -> f64 {
    text.split_once(before)
        .and_then(|(_, rest)| rest.split_once(after))
        .and_then(|(number, _)| number.parse().ok())
        .unwrap_or_else(|| panic!("no number between {before:?} and {after:?} in:\n{text}"))
}

/// `qrec.json`'s take `take` once it is no longer recording (30 s at
/// most): the headless host finalizes a take that lost its last source.
fn landed(dir: &Path, take: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let project: Value = fs::read_to_string(dir.join("qrec.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let found = &project["takes"][take];
        if found.is_object() && found["status"] != "recording" {
            return found.clone();
        }
        assert!(Instant::now() < deadline, "{take} never landed: {found}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A screen layer on X11 records its monitor through `ximagesrc`, with no
/// portal, dialog or restore token (adding the layer is the consent): a
/// take of `VIRT-R` with only that layer (so no audio source is built),
/// 3 s long, is the right half of the screen and nothing of the red left
/// half (`ximagesrc`'s inclusive ends), 960×1080 rather than the whole
/// 1920×1080 screen, at the canvas rate for as long as it recorded (the
/// source's own buffers too, which the record bin's `videorate` would
/// hide), with square pixels on an X screen whose millimetres aren't
/// (`ximagesrc` would say 11:10), and its ball moved between the first
/// frame and the last. Once the monitor is gone, a take of it fails
/// naming the layer instead of grabbing the whole screen (`ximagesrc`'s
/// fallback for a rectangle it can't grab).
#[test]
#[ignore = "starts a private Xvfb (scripts/check-full)"]
fn an_x11_screen_is_recorded_without_the_portal() {
    let x = X11Setup::new();
    // 508×200 mm: `ximagesrc` derives a non-square pixel aspect ratio.
    x.xvfb.set_screen_size(1920, 1080, (508, 200));
    let dir = x.env.empty_dir("take");
    x.qrec(&dir, &["--json", "init", "--canvas", "1280x720@30"], &[]);
    // Not a VA encoder this machine may have.
    x.qrec(
        &dir,
        &["--json", "project", "set", "--encoder", "x264"],
        &[],
    );
    x.qrec(
        &dir,
        &[
            "--json", "scene", "layer", "add", "screen", "--output", "VIRT-R",
        ],
        &[],
    );
    let content = Content::start(&x);

    let began = Instant::now();
    let (start, stderr) = x.qrec(&dir, &["--json", "record", "start"], &[]);
    let started = &start["data"]["result"];
    assert_eq!(started["take"], "take-0001", "{start}\n{stderr}");
    assert_eq!(
        started["files"],
        serde_json::json!(["takes/take-0001/L1-screen-VIRT-R.mkv"])
    );
    std::thread::sleep(Duration::from_secs(3));
    let (stop, stderr) = x.qrec(&dir, &["--json", "record", "stop"], &[]);
    let wall = began.elapsed().as_secs_f64();
    drop(content);
    let finalized = &stop["data"]["result"];
    assert_eq!(finalized["status"], "ok", "{stop}\n{stderr}");

    let file = dir.join("takes/take-0001/L1-screen-VIRT-R.mkv");
    let probe = ffprobe(&file);
    let video = &probe["streams"][0];
    assert_eq!(video["codec_name"], "h264", "{probe}");
    assert_eq!(
        (video["width"].as_u64(), video["height"].as_u64()),
        (Some(960), Some(1080)),
        "VIRT-R, not the whole screen: {probe}"
    );
    assert_eq!(video["r_frame_rate"], "30/1", "the canvas rate: {probe}");
    assert_eq!(
        video["sample_aspect_ratio"], "1:1",
        "square pixels: {probe}"
    );
    let frames: u64 = video["nb_read_packets"].as_str().unwrap().parse().unwrap();
    let duration: f64 = probe["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse().ok())
        .unwrap();
    // From before the start's attach to after the stop: at least the 3 s
    // slept, at most the whole exchange.
    let frame = 1.0 / 30.0;
    assert!(
        duration >= 3.0 - frame && duration <= wall + frame,
        "{duration} s recorded in {wall:.3} s"
    );
    assert!(
        frames.abs_diff((duration * 30.0).round() as u64) <= 1,
        "{frames} frames in {duration} s"
    );
    let take_frames = finalized["durationFrames"].as_u64().unwrap();
    assert!(take_frames.abs_diff(frames) <= 1, "{finalized}");
    // The file's rate is the record bin's `videorate` whatever the source
    // runs at (25 fps without the rate's capsfilter): its tee's buffers,
    // which recorder.log counts, are one per 1/30 s.
    let log = fs::read_to_string(dir.join("takes/take-0001/recorder.log")).unwrap();
    let first_ms = number_between(&log, "L1: first buffer at T0 + ", " ms");
    let buffers = number_between(&log, "L1: closed after ", " buffers");
    let last_ms = number_between(&log, "buffers (last at T0 + ", " ms)");
    let expected = (last_ms - first_ms) * 30.0 / 1000.0 + 1.0;
    assert!(
        (buffers - expected).abs() <= 2.0,
        "{buffers} buffers from T0 + {first_ms} ms to T0 + {last_ms} ms, not 30 fps:\n{log}"
    );

    let first = frame_rgb(&file, 0);
    let last = frame_rgb(&file, frames - 1);
    assert_eq!(first.len(), 960 * 1080 * 3);
    assert_eq!(
        (red_pixels(&first), red_pixels(&last)),
        (0, 0),
        "VIRT-L's red"
    );
    let luma = |p: &[u8; 3]| (u16::from(p[0]) + u16::from(p[1]) + u16::from(p[2])) / 3;
    let moved = first
        .as_chunks::<3>()
        .0
        .iter()
        .zip(last.as_chunks::<3>().0)
        .filter(|&(a, b)| luma(a).abs_diff(luma(b)) > 96)
        .count();
    assert!(moved > 500, "the ball didn't move: {moved} pixels changed");

    assert!(!log.contains("portal"), "{log}");
    let tokens = x.env.state_home().join("qrec/portal-tokens.json");
    assert!(!tokens.exists(), "no restore token was asked for or saved");
    assert_eq!(x.niri_calls(), "", "niri msg never runs on X11");

    // VIRT-R is gone: the take fails, naming the layer.
    x.xvfb.delete_monitor("VIRT-R");
    let (ok, reply, stderr) = x.run(&dir, &["--json", "record", "start"], &[]);
    assert!(!ok, "{reply}\n{stderr}");
    assert_eq!(reply["error"]["code"], "capture_failed", "{reply}");
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("L1 screen VIRT-R")
            && message.contains(&format!(
                "the X server (DISPLAY={}) has no such monitor",
                x.xvfb.display()
            )),
        "{message}"
    );
    let left: Vec<String> = fs::read_dir(dir.join("takes/take-0002"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !left.iter().any(|name| name.ends_with(".mkv")),
        "nothing was recorded: {left:?}"
    );
}

/// A screen layer stops recording once its monitor isn't where the take
/// began grabbing it, instead of grabbing that rectangle on (something
/// else is there now, maybe a monitor nobody added): the take loses L1
/// (`StreamLost`; it was the take's only source, so the take stops and
/// lands broken, with L1 missing). First VIRT-R is deleted while the
/// screen stays as it was, which only the watch on the X server's
/// monitors sees. Then, set again, VIRT-R is left outside a screen that
/// shrank to VIRT-L's size, which `ximagesrc` answers by grabbing VIRT-L
/// (its fallback to the whole root; the guard on it and the watch both
/// see that): no frame of VIRT-L's red reaches the take.
#[test]
#[ignore = "starts a private Xvfb (scripts/check-full)"]
fn an_x11_screen_is_lost_when_its_monitor_goes() {
    let x = X11Setup::new();
    let dir = x.env.empty_dir("lost");
    x.qrec(&dir, &["--json", "init", "--canvas", "1280x720@30"], &[]);
    x.qrec(
        &dir,
        &["--json", "project", "set", "--encoder", "x264"],
        &[],
    );
    x.qrec(
        &dir,
        &[
            "--json", "scene", "layer", "add", "screen", "--output", "VIRT-R",
        ],
        &[],
    );
    let _content = Content::start(&x);
    // Records VIRT-R for 1.5 s, then makes `change` on the X server: the
    // take lands by itself, with L1 lost. Its file and why L1 was lost.
    let lose = |take: &str, change: &dyn Fn()| -> (PathBuf, String) {
        let (start, stderr) = x.qrec(&dir, &["--json", "record", "start"], &[]);
        assert_eq!(start["data"]["result"]["take"], take, "{start}\n{stderr}");
        std::thread::sleep(Duration::from_millis(1500));
        change();
        let landed = landed(&dir, take);
        assert_eq!(landed["status"], "broken", "{landed}");
        assert_eq!(landed["scene"]["layers"][0]["missing"], true, "{landed}");
        let log = fs::read_to_string(dir.join(format!("takes/{take}/recorder.log"))).unwrap();
        let why = log
            .lines()
            .find_map(|line| line.split_once("L1 lost: screen VIRT-R: "))
            .map(|(_, why)| why.to_owned())
            .unwrap_or_else(|| panic!("L1 wasn't lost:\n{log}"));
        let file = dir.join(format!("takes/{take}/L1-screen-VIRT-R.mkv"));
        (file, why)
    };

    let (_, why) = lose("take-0001", &|| x.xvfb.delete_monitor("VIRT-R"));
    assert_eq!(
        why,
        "the X server no longer has this monitor (unplugged, turned off or renamed)"
    );

    x.xvfb.set_monitor("VIRT-R", 960, 0, 960, 1080);
    let (file, why) = lose("take-0002", &|| {
        x.xvfb.set_screen_size(960, 1080, (254, 286));
    });
    assert!(
        why.starts_with("the X screen shrank and no longer holds this monitor")
            || why.starts_with("the X server no longer has this monitor"),
        "{why}"
    );
    let frames = small_frames(&file);
    assert!(
        frames.len() >= 15,
        "{} frames before the loss",
        frames.len()
    );
    for (n, frame) in frames.iter().enumerate() {
        assert_eq!(red_pixels(frame), 0, "VIRT-L's red in frame {n}");
    }
}
