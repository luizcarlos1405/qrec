//! WP4's screenshot cases (docs/GUI.md G§7 WP4, G§8.4): the stage of the
//! approved slice in a nested headless sway on the design fixture: the
//! regions WP4 owns checked against BRIEF (±1 px × scale), the stage's
//! labels, the a11y walk, no panic caught, and a side-by-side with the
//! approved PNG for the eye (`<case>-compare.png`). Other WPs' regions are
//! their stubs until they merge (G§7.0: stub-based).
//!
//! `cargo test -p qrec --test gui_stage -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon; pixman, or GL
//! with `QREC_TEST_WLR=gles2`). Screenshots and logs go to
//! `tmp/gui-shots/wp4/`.

mod gui_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use gui_support::pointer::BUTTON_LEFT;
use gui_support::{Launch, Nested, Options, assert_regions, qrec_bin, wait_for_gui_host};
use qrec_core::state::CanvasMode;
use qrec_core::{ClipId, UiState};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use serde_json::{Value, json};

const OPEN: Duration = Duration::from_secs(60);
const AREA: &str = "wp4";

// ---------------------------------------------------------------- helpers

/// The design fixture as `variant` in a folder named `rust-traits-talk`,
/// with `ui` as its `ui.json`.
fn fixture(nested: &Nested, variant: DesignVariant, ui: Option<UiState>) -> PathBuf {
    let dir = nested.env.empty_dir("rust-traits-talk");
    design_fixture(&dir, variant);
    if let Some(ui) = ui {
        write_ui(&dir, &ui);
    }
    dir
}

/// The design state's UI: `mode`, the playhead at `playhead`.
fn ui(mode: CanvasMode, playhead: u64) -> UiState {
    UiState {
        canvas_mode: mode,
        playhead,
        ..UiState::default()
    }
}

/// `ui` with `clips` selected.
fn selecting(mut ui: UiState, clips: &[&str]) -> UiState {
    ui.selection = clips.iter().map(|c| ClipId::from_raw(*c)).collect();
    ui
}

/// `ui.reduced_motion = true` in the user's `config.toml`.
fn reduced_motion(nested: &Nested) {
    let config = nested.env.config_home().join("qrec");
    fs::create_dir_all(&config).expect("the config folder");
    fs::write(config.join("config.toml"), "[ui]\nreduced_motion = true\n").expect("config.toml");
}

/// Launches the window on `project` and waits until it is open and idle.
fn open(nested: &mut Nested, project: &Path, launch: Launch) {
    nested.launch(project, launch);
    wait_for_gui_host(nested, project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
}

/// `qrec status --json`'s data.
fn status(nested: &Nested, project: &Path) -> Value {
    nested.cli_json(project, &["status"])["data"].clone()
}

/// The record phase.
fn phase(nested: &Nested, project: &Path) -> String {
    status(nested, project)["runtime"]["record"]["phase"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn wait_phase(nested: &Nested, project: &Path, want: &str, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let now = phase(nested, project);
        if now == want {
            return;
        }
        assert!(Instant::now() < deadline, "never {want} (is {now})");
        thread::sleep(Duration::from_millis(50));
    }
}

/// The shot next to the approved frame (G§8.4 check 4: for the eye).
fn compare(nested: &Nested, shot: &Path, frame: &str) {
    let png = gui_support::repo().join("design/screens").join(frame);
    let out = nested.shots.join(format!("{}-compare.png", nested.case));
    let (w, h) = nested.opts.logical();
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(shot)
        .arg("-i")
        .arg(&png)
        .args([
            "-filter_complex",
            &format!("[0:v]scale={w}:{h}[a];[1:v]scale={w}:{h}[b];[a][b]hstack=inputs=2"),
        ])
        .arg(&out)
        .stdout(Stdio::null())
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "the comparison of {}", shot.display());
}

/// The stage's regions at 1600×1000 (BRIEF §2.1, G§3.15).
const STAGE: [(&str, [f64; 4]); 3] = [
    ("region-stage", [0.0, 48.0, 1216.0, 608.0]),
    ("preview", [128.0, 64.0, 960.0, 540.0]),
    ("transport", [128.0, 612.0, 960.0, 36.0]),
];

fn assert_stage_regions(nested: &mut Nested, expected: &[(&str, [f64; 4])]) {
    let scale = nested.opts.scale;
    let regions = nested.window(0).regions(&[]);
    let names: Vec<&str> = expected.iter().map(|(n, _)| *n).collect();
    assert_regions(&regions, expected, &names, scale);
}

/// The objective checks every case ends with: the a11y walk and no panic.
/// The window's log stays next to the shots (`logs/<case>-gui.log`).
fn assert_clean(nested: &mut Nested) {
    keep_gui_log(nested);
    let gui = nested.window(0);
    let reply = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(reply["ok"], true, "a11y: {reply}");
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
}

/// Copies the window's log to `logs/<case>-gui.log`.
fn keep_gui_log(nested: &mut Nested) {
    let log = gui_log(nested);
    let path = nested
        .shots
        .join("logs")
        .join(format!("{}-gui.log", nested.case));
    fs::write(&path, log).expect("the GUI log copy");
}

/// The last `n` lines of the window's log (for a failure's message).
fn gui_log_tail(nested: &mut Nested, n: usize) -> String {
    let log = gui_log(nested);
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn stage_labels(nested: &mut Nested) -> Vec<String> {
    nested.window(0).labels("region-stage")
}

/// `labels` contains each of `texts`.
fn assert_shows(labels: &[String], texts: &[&str]) {
    for text in texts {
        assert!(labels.iter().any(|l| l == text), "{text}: {labels:?}");
    }
}

fn assert_hides(labels: &[String], texts: &[&str]) {
    for text in texts {
        assert!(!labels.iter().any(|l| l == text), "{text}: {labels:?}");
    }
}

/// The window's GUI log.
fn gui_log(nested: &mut Nested) -> String {
    nested.window(0).gui_log()
}

// ------------------------------------------------------------- first run

/// The rows `QREC_FAKE_INVENTORY=design` gives S01a (BRIEF §5, the fake
/// outputs for DP-4 and HDMI-A-1, G§8.4 known differences).
const FIRST_RUN_ROWS: [&str; 5] = [
    "Screen · FAKE-1 · GIGA-BYTE GS27QA",
    "Screen · FAKE-2 · Dell U2720Q",
    "Camera · Logitech C920",
    "Mic · Yeti Nano",
    "System audio · Built-in Audio Analog Stereo",
];

/// S01a: an empty folder: "What should Qrec record?", the five detected
/// sources with the first screen, the camera, the mic and system audio
/// checked, "Add 4 sources" and its caption; no preview, no badge.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s01a_first_run() {
    let mut nested = Nested::start(AREA, "S01a", Options::default());
    let project = nested.env.empty_dir("rust-traits-talk");
    open(&mut nested, &project, Launch::a11y());
    assert_stage_regions(&mut nested, &STAGE);
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["What should Qrec record?", "Add 4 sources"]);
    assert_shows(&labels, &FIRST_RUN_ROWS);
    assert_shows(
        &labels,
        &["Screens ask for permission once. Your desktop shows a picker for each one."],
    );
    assert_hides(&labels, &["LIVE"]);
    assert_clean(&mut nested);
    let shot = nested.screenshot("first-run");
    compare(&nested, &shot, "S01a-first-run-no-sources.png");
}

/// S01b: `Enter` on S01a adds the four sources (D-16: the screen, the
/// camera, the mic, system audio, in that order, so the camera is on top),
/// and the Live picture shows them.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s01b_sources_added() {
    let mut nested = Nested::start(AREA, "S01b", Options::default());
    let project = nested.env.empty_dir("rust-traits-talk");
    open(&mut nested, &project, Launch::a11y());
    nested.keys(&["-k", "Return"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    let scene = loop {
        let scene = status(&nested, &project)["project"]["scene"].clone();
        let (layers, audio) = (
            scene["layers"].as_array().map_or(0, Vec::len),
            scene["audio"].as_array().map_or(0, Vec::len),
        );
        if layers == 2 && audio == 2 {
            break scene;
        }
        if Instant::now() >= deadline {
            keep_gui_log(&mut nested);
            let local = nested.window(0).local();
            let tail = gui_log_tail(&mut nested, 40);
            panic!("the sources: {scene}\nlocal: {local}\n{tail}");
        }
        thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(scene["layers"][0]["source"]["kind"], "screen", "{scene}");
    assert_eq!(scene["layers"][1]["source"]["kind"], "camera", "{scene}");
    assert_eq!(scene["audio"][0]["role"], "mic", "{scene}");
    assert_eq!(scene["audio"][1]["role"], "system", "{scene}");
    let gui = nested.window(0);
    gui.wait_idle();
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["LIVE", "Live · 1920×1080 · 30 fps"]);
    assert_hides(&labels, &["What should Qrec record?"]);
    assert_stage_regions(&mut nested, &STAGE);
    assert_clean(&mut nested);
    let shot = nested.screenshot("sources-added");
    compare(&nested, &shot, "S01b-first-run-sources-added.png");
}

// ------------------------------------------------------------------ Live

/// S02: Live, idle, the playhead at the end: the live picture (the fake
/// screen and camera), LIVE, "Live · 1920×1080 · 30 fps", the transport
/// off, Save frame on.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s02_live_idle() {
    let mut nested = Nested::start(AREA, "S02", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(&mut nested, &project, Launch::a11y());
    assert_stage_regions(&mut nested, &STAGE);
    let labels = stage_labels(&mut nested);
    assert_shows(
        &labels,
        &["LIVE", "Live · 1920×1080 · 30 fps", "Save frame"],
    );
    assert_hides(&labels, &["REC", "PROGRAM"]);
    assert_clean(&mut nested);
    let shot = nested.screenshot("live");
    compare(&nested, &shot, "S02-live-idle.png");
    assert!(
        gui_log(&mut nested).contains("the live picture is 960×540"),
        "the sink composites at 960×540"
    );
}

/// S02 at `QREC_TEST_SCALE=2`: the regions scale with the output, and the
/// live sink composites at 1920×1080 (960×540 × the window's scale).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s02_at_scale_2() {
    let opts = Options {
        scale: 2.0,
        width: 3200,
        height: 2000,
        ..Options::default()
    };
    let mut nested = Nested::start(AREA, "S02-scale2", opts);
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(&mut nested, &project, Launch::a11y());
    assert_stage_regions(&mut nested, &STAGE);
    assert_clean(&mut nested);
    let shot = nested.screenshot("live");
    compare(&nested, &shot, "S02-live-idle.png");
    assert!(
        gui_log(&mut nested).contains("the live picture is 1920×1080"),
        "the sink composites at 1920×1080 at scale 2"
    );
}

/// S05b: take 10 landed (Live, idle): the Live picture as S02.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s05b_take_lands() {
    let mut nested = Nested::start(AREA, "S05b", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Take10Landed,
        Some(ui(CanvasMode::Live, 12233)),
    );
    open(&mut nested, &project, Launch::a11y());
    assert_stage_regions(&mut nested, &STAGE);
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["LIVE", "Live · 1920×1080 · 30 fps"]);
    assert_hides(&labels, &["REC"]);
    assert_clean(&mut nested);
    let shot = nested.screenshot("landed");
    compare(&nested, &shot, "S05b-take-lands.png");
}

// ------------------------------------------------------------- recording

/// `qrec record start` in the background: it answers when the take
/// starts (after the countdown) or the countdown is cancelled. Its output
/// goes to `logs/<case>-record-start-<n>.log`.
fn record_start_in_background(nested: &Nested, project: &Path, n: u32) -> Child {
    let log = nested
        .shots
        .join("logs")
        .join(format!("{}-record-start-{n}.log", nested.case));
    let out = fs::File::create(&log).expect("the record start log");
    let err = out.try_clone().expect("the record start log");
    nested
        .env
        .command(&qrec_bin())
        .arg("--project")
        .arg(project)
        .args(["record", "start", "--json"])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .expect("run qrec record start")
}

/// Waits up to 20 s for the background `record start` to answer, then
/// kills it.
fn reap(mut child: Child) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("qrec record start never answered the cancel");
}

/// Cancels the countdown (or the take) and waits for idle.
fn cancel_record(nested: &Nested, project: &Path, start: Child) {
    let cancel = nested.cli_json(project, &["record", "cancel"]);
    assert_eq!(cancel["ok"], true, "{cancel}");
    reap(start);
    wait_phase(nested, project, "idle", Duration::from_secs(20));
}

/// S03 (and its reduced-motion twin): `recording.countdownSecs` 3 and
/// `qrec record start`: the dimmed preview, the full ring, the digit 3 and
/// "Take 10" / "Esc or R cancels". The countdown ticks in real time, so a
/// shot that missed the first second (a loaded machine) is taken again.
fn countdown_case(case: &str, reduced: bool) {
    let mut nested = Nested::start(AREA, case, Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    let set = nested.cli_json(&project, &["project", "set", "--countdown", "3"]);
    assert_eq!(set["ok"], true, "{set}");
    if reduced {
        reduced_motion(&nested);
    }
    open(&mut nested, &project, Launch::a11y());
    let mut attempt = 0;
    let (shot, start) = loop {
        attempt += 1;
        let start = record_start_in_background(&nested, &project, attempt);
        wait_phase(&nested, &project, "countdown", Duration::from_secs(20));
        nested.window(0).wait_idle();
        let shot = nested.screenshot("countdown");
        // Still on 3 after the shot: the shot shows 3.
        let record = status(&nested, &project)["runtime"]["record"].clone();
        if record["phase"] == "countdown" && record["remaining"] == 3 {
            break (shot, start);
        }
        assert!(attempt < 3, "never shot on 3: {record}");
        cancel_record(&nested, &project, start);
    };
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["Take 10", "Esc or R cancels", "LIVE"]);
    assert_stage_regions(&mut nested, &STAGE);
    cancel_record(&nested, &project, start);
    assert_clean(&mut nested);
    compare(&nested, &shot, "S03-countdown.png");
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s03_countdown() {
    countdown_case("S03", false);
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s03_countdown_reduced_motion() {
    countdown_case("S03-reduced", true);
}

/// S04 (and its reduced-motion twin: a static badge without its glow):
/// recording take 10 with `QREC_FAKE_RECORD_OFFSET_FRAMES=4842`: REC, the
/// timer from 00:02:41:12, "Take 10", LIVE.
fn recording_case(case: &str, reduced: bool) {
    let mut nested = Nested::start(AREA, case, Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    if reduced {
        reduced_motion(&nested);
    }
    open(
        &mut nested,
        &project,
        Launch::a11y().with_env("QREC_FAKE_RECORD_OFFSET_FRAMES", "4842"),
    );
    let start = nested.cli_json(&project, &["record", "start"]);
    assert_eq!(start["ok"], true, "{start}");
    wait_phase(&nested, &project, "recording", Duration::from_secs(30));
    nested.window(0).wait_idle();
    let shot = nested.screenshot("recording");
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["REC", "Take 10", "LIVE"]);
    assert!(
        labels.iter().any(|l| l.starts_with("00:02:4")),
        "the timer: {labels:?}"
    );
    assert_stage_regions(&mut nested, &STAGE);
    assert_clean(&mut nested);
    compare(&nested, &shot, "S04-recording.png");
    let stop = nested.cli_json(&project, &["record", "stop"]);
    assert_eq!(stop["ok"], true, "{stop}");
    wait_phase(&nested, &project, "idle", Duration::from_secs(30));
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s04_recording() {
    recording_case("S04", false);
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s04_recording_reduced_motion() {
    recording_case("S04-reduced", true);
}

/// S04b: `o` with c2d0 selected records over it (offset 932: 00:00:31:02).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s04b_record_over() {
    let mut nested = Nested::start(AREA, "S04b", Options::default());
    let preset = selecting(ui(CanvasMode::Live, 2437), &["c2d0"]);
    let project = fixture(&nested, DesignVariant::Base, Some(preset));
    open(
        &mut nested,
        &project,
        Launch::a11y().with_env("QREC_FAKE_RECORD_OFFSET_FRAMES", "932"),
    );
    nested.keys(&["-k", "o"]);
    wait_phase(&nested, &project, "recording", Duration::from_secs(30));
    let record = status(&nested, &project)["runtime"]["record"].clone();
    assert!(record["mode"]["over"].is_object(), "record over: {record}");
    nested.window(0).wait_idle();
    let shot = nested.screenshot("recording-over");
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["REC", "Take 10"]);
    assert!(
        labels.iter().any(|l| l.starts_with("00:00:3")),
        "the timer: {labels:?}"
    );
    assert_clean(&mut nested);
    compare(&nested, &shot, "S04b-recording-over-c2d0.png");
    let stop = nested.cli_json(&project, &["record", "stop"]);
    assert_eq!(stop["ok"], true, "{stop}");
    wait_phase(&nested, &project, "idle", Duration::from_secs(30));
}

/// S05: finalizing take 10 (EOS held back 8 s): "Saving take 10…" with a
/// spinner instead of REC and its timer.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s05_finalizing() {
    let mut nested = Nested::start(AREA, "S05", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(
        &mut nested,
        &project,
        Launch::a11y()
            .with_env("QREC_FAKE_RECORD_OFFSET_FRAMES", "4842")
            .with_env("QREC_FAKE_EOS_DELAY_MS", "8000"),
    );
    let start = nested.cli_json(&project, &["record", "start"]);
    assert_eq!(start["ok"], true, "{start}");
    wait_phase(&nested, &project, "recording", Duration::from_secs(30));
    // The stop waits for the take: in the background.
    let mut stop = nested
        .env
        .command(&gui_support::qrec_bin())
        .arg("--project")
        .arg(&project)
        .args(["record", "stop"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("qrec record stop");
    wait_phase(&nested, &project, "finalizing", Duration::from_secs(20));
    nested.window(0).wait_idle();
    let shot = nested.screenshot("finalizing");
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["Saving take 10…", "LIVE"]);
    assert_hides(&labels, &["REC"]);
    assert_clean(&mut nested);
    compare(&nested, &shot, "S05-finalizing.png");
    wait_phase(&nested, &project, "idle", Duration::from_secs(30));
    let _ = stop.wait();
}

// --------------------------------------------------------------- Program

/// S06: Program, c2d0 selected, the playhead at 00:01:21:07: the program
/// picture at that frame, PROGRAM, "00:01:21:07" / "00:04:06:11", the
/// transport on.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s06_program_with_selection() {
    let mut nested = Nested::start(AREA, "S06", Options::default());
    let preset = selecting(ui(CanvasMode::Program, 2437), &["c2d0"]);
    let project = fixture(&nested, DesignVariant::Base, Some(preset));
    open(&mut nested, &project, Launch::a11y());
    assert_stage_regions(&mut nested, &STAGE);
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["PROGRAM", "00:01:21:07", " / 00:04:06:11"]);
    assert_hides(&labels, &["LIVE"]);
    assert_clean(&mut nested);
    let shot = nested.screenshot("program");
    compare(&nested, &shot, "S06-program-with-selection.png");
}

/// S14's stage: the bin open pushes it; the preview is 832×468 at (368,
/// 100) and the transport stays at y 612 (G§3.15).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s14_stage_with_the_bin() {
    let mut nested = Nested::start(AREA, "S14", Options::default());
    let mut preset = selecting(ui(CanvasMode::Program, 2437), &["c2d0"]);
    preset.bin_open = true;
    let project = fixture(&nested, DesignVariant::Base, Some(preset));
    open(&mut nested, &project, Launch::a11y());
    assert_stage_regions(
        &mut nested,
        &[
            ("region-stage", [352.0, 48.0, 864.0, 608.0]),
            ("preview", [368.0, 100.0, 832.0, 468.0]),
            ("transport", [368.0, 612.0, 832.0, 36.0]),
        ],
    );
    assert_clean(&mut nested);
    let shot = nested.screenshot("bin");
    compare(&nested, &shot, "S14-bin-drawer.png");
}

/// D-27: Program on an empty timeline: the canvas background and PROGRAM
/// only (no stale picture).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn program_on_an_empty_timeline() {
    let mut nested = Nested::start(AREA, "D27", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Program, 0)),
    );
    for clip in [
        "c7f3", "c8a1", "c1b2", "c1b3", "c1b4", "c1b5", "c1b6", "c2d0", "c2d1", "c3e9",
    ] {
        let removed = nested.cli_json(&project, &["clip", "remove", clip]);
        assert_eq!(removed["ok"], true, "{removed}");
    }
    open(&mut nested, &project, Launch::a11y());
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["PROGRAM", "00:00:00:00", " / 00:00:00:00"]);
    assert_clean(&mut nested);
    let shot = nested.screenshot("empty-program");
    // The preview is the canvas background (black) inside its border.
    let png = shot.display().to_string();
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            &png,
            "-vf",
            "crop=1:1:600:300,format=rgb24",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .expect("ffmpeg");
    assert_eq!(out.stdout, [0, 0, 0], "the canvas background");
}

// --------------------------------------------------------- layer editing

/// The window coordinates of L2's box at rest (BRIEF §3.1: (1500, 760, 384,
/// 288) on a 1:2 preview at (128, 64)): (878, 444, 192, 144).
const L2_BOX: [f64; 4] = [878.0, 444.0, 192.0, 144.0];

/// S13: dragging L2 in Live: its left edge snaps to the right third (x
/// 1280); the box, the guide tagged "Right third" and the readout "x 1280 ·
/// y 760 · 384 × 288" show mid-drag; the release sends one `LayerSet`
/// (D-32, D-41), which `undo` takes back.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s13_layer_editing() {
    let mut nested = Nested::start(AREA, "S13", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(&mut nested, &project, Launch::a11y());
    let history_before = status(&nested, &project)["history"]["cursor"].clone();
    let [x, y, w, h] = L2_BOX;
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let pointer = nested.pointer();
    pointer.move_to(cx, cy);
    pointer.button(BUTTON_LEFT, true);
    // 107 px left: 3 px from the right third, which it snaps to.
    for i in 1..=12 {
        let t = f64::from(i) / 12.0;
        pointer.move_to(cx - 107.0 * t, cy);
        thread::sleep(Duration::from_millis(16));
    }
    nested.window(0).wait_idle();
    let shot = nested.screenshot("dragging");
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["Right third", "x 1280 · y 760 · 384 × 288"]);
    let local = nested.window(0).local();
    assert_eq!(local["drag"], "layerMove", "{local}");
    compare(&nested, &shot, "S13-layer-editing.png");
    nested.pointer().button(BUTTON_LEFT, false);
    nested.window(0).wait_idle();
    // One `LayerSet`: L2 at x 1280, one history entry.
    let data = status(&nested, &project);
    let l2 = &data["project"]["scene"]["layers"][1];
    assert_eq!(
        l2["rect"],
        json!({"x": 1280, "y": 760, "w": 384, "h": 288}),
        "{l2}"
    );
    let cursor = data["history"]["cursor"].as_u64().unwrap_or(0);
    assert_eq!(
        cursor,
        history_before.as_u64().unwrap_or(0) + 1,
        "one entry: {}",
        data["history"]
    );
    let labels = stage_labels(&mut nested);
    assert_hides(&labels, &["Right third"]);
    assert_eq!(nested.window(0).local()["drag"], Value::Null);
    assert_clean(&mut nested);
    let undo = nested.cli_json(&project, &["undo"]);
    assert_eq!(undo["ok"], true, "{undo}");
}

/// D-33: an arrow key pressed three times with the canvas focused is three
/// bursts of one press (`wtype` releases each key): three `LayerSet`s, each
/// 1 px; Shift+arrow nudges 10 px.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn arrow_keys_nudge_the_selected_layer() {
    let mut nested = Nested::start(AREA, "nudge", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(&mut nested, &project, Launch::a11y());
    // A click on L2 selects it and focuses the canvas.
    let [x, y, w, h] = L2_BOX;
    nested.pointer().click(x + w / 2.0, y + h / 2.0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while status(&nested, &project)["ui"]["selectedLayer"] != "L2" {
        assert!(Instant::now() < deadline, "L2 never selected");
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(nested.window(0).local()["focus"], "stage");
    nested.keys(&["-k", "Left", "-k", "Left", "-k", "Up"]);
    nested.keys(&["-M", "shift", "-k", "Right", "-m", "shift"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rect = status(&nested, &project)["project"]["scene"]["layers"][1]["rect"].clone();
        if rect == json!({"x": 1508, "y": 759, "w": 384, "h": 288}) {
            break;
        }
        assert!(Instant::now() < deadline, "the nudges: {rect}");
        thread::sleep(Duration::from_millis(50));
    }
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(gui.local()["drag"], Value::Null);
    nested.screenshot("nudged");
    assert_clean(&mut nested);
}

/// S10's canvas: a trim of c2d0's start edge (S08's seconds zoom, +29
/// frames) shows take 5 frame 41 with the 2 px accent stroke and "Trim
/// start · take 5 · frame 41". The trim drag is WP3's timeline: until it
/// merges this case has nothing to press and stops there.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s10_trim_scrub_on_the_stage() {
    let mut nested = Nested::start(AREA, "S10", Options::default());
    let mut preset = selecting(ui(CanvasMode::Program, 2437), &["c2d0"]);
    preset.zoom = Some(1.6);
    preset.scroll_px = 1950.0 * 1.6;
    let project = fixture(&nested, DesignVariant::Base, Some(preset));
    open(&mut nested, &project, Launch::a11y());
    let regions = nested.window(0).regions(&[]);
    let Some(row) = regions.get("video-row").copied() else {
        eprintln!("S10: no WP3 timeline yet (no `video-row`): stub-based, nothing to trim");
        return;
    };
    // c2d0's start (frame 2084) at 1.6 px/frame from 00:01:05:00.
    let edge = row[0] + (2084.0 - 1950.0) * 1.6;
    let y = row[1] + row[3] / 2.0;
    let pointer = nested.pointer();
    pointer.move_to(edge + 1.0, y);
    pointer.button(BUTTON_LEFT, true);
    for i in 1..=10 {
        pointer.move_to(edge + 1.0 + 46.4 * f64::from(i) / 10.0, y);
        thread::sleep(Duration::from_millis(16));
    }
    nested.window(0).wait_idle();
    let shot = nested.screenshot("trimming");
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["Trim start", " · take 5 · frame ", "41"]);
    compare(&nested, &shot, "S10-trimming.png");
    nested.keys(&["-k", "Escape"]);
    nested.pointer().button(BUTTON_LEFT, false);
    nested.window(0).wait_idle();
    assert_clean(&mut nested);
}
