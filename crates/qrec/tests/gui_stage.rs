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
//! `tmp/gui-shots/wp4/`, S24's (the portal-denied state) to
//! `tmp/gui-shots/s24/`.

mod gui_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use gui_support::pointer::BUTTON_LEFT;
use gui_support::{Launch, Nested, Options, assert_regions, reap_within, wait_for_gui_host};
use qrec_core::state::CanvasMode;
use qrec_core::{ClipId, UiState};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use qrec_shell::testkit::run_tool;
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
    let run = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(shot)
            .arg("-i")
            .arg(&png)
            .args([
                "-filter_complex",
                &format!("[0:v]scale={w}:{h}[a];[1:v]scale={w}:{h}[b];[a][b]hstack=inputs=2"),
            ])
            .arg(&out),
        gui_support::TOOL_TIMEOUT,
    );
    assert!(
        run.status.success(),
        "the comparison of {}: {}",
        shot.display(),
        run.stderr
    );
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

/// The hint bar draws its items whole (G§3.15): what it shows is the first
/// `n` of them (all when `all`, fewer otherwise: a narrow window leaves the
/// trailing ones out) and none runs past the bar. Returns the labels shown.
fn assert_hints_whole(nested: &mut Nested, all: bool) -> Vec<String> {
    let reply = nested.window(0).ctl(json!({"op": "hints"}));
    let labels: Vec<String> = reply["items"]
        .as_array()
        .unwrap_or_else(|| panic!("hints: {reply}"))
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_owned))
        .collect();
    let shown: Vec<String> = reply["shown"]
        .as_array()
        .unwrap_or_else(|| panic!("hints: {reply}"))
        .iter()
        .filter_map(|l| l.as_str().map(str::to_owned))
        .collect();
    assert_eq!(reply["cut"], false, "an item runs past the bar: {reply}");
    assert!(
        labels.starts_with(&shown),
        "the bar shows {shown:?}, not the first of {labels:?}"
    );
    if all {
        assert_eq!(shown, labels, "every item fits");
    } else {
        assert!(
            !shown.is_empty() && shown.len() < labels.len(),
            "a narrow bar leaves the trailing items out: {shown:?} of {labels:?}"
        );
    }
    shown
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
    gui.assert_no_criticals();
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
    // The panel (WP5): both sections empty, no inspector (BRIEF S01a).
    let panel = nested.window(0).labels("region-panel");
    assert_shows(&panel, &["No layers yet", "No audio tracks"]);
    assert_hides(&panel, &["Inspector"]);
    assert_clean(&mut nested);
    let shot = nested.screenshot("first-run");
    compare(&nested, &shot, "S01a-first-run-no-sources.png");
    // WP10's shot review: S01a's card is 204 px for its five rows, 40 px
    // apart (39 for the first and the last, under the card's own stroke),
    // each separator straight across its whole inside (x 368–847: the
    // rounded check buttons bent their ends up, the card was 202 tall).
    let border = gui_support::token_rgb("swamp-night", "border");
    let px = |x: f64, y: f64| {
        let (dx, dy) = nested.opts.pixel_px(x, y);
        gui_support::rgb_pixels(&shot, [dx, dy, 1, 1])[0]
    };
    for y in [208.0, 248.0, 289.0, 330.0, 371.0, 411.0] {
        assert_eq!(px(600.0, y), border, "the card's line at y {y}");
    }
    for y in [248.0, 289.0, 330.0, 371.0] {
        for x in [368.0, 847.0] {
            assert_eq!(px(x, y), border, "the separator at y {y} reaches x {x}");
        }
    }
    // The icons and art lane's measure: a checked row's check is the
    // frames' 12 px `check` centered in its 16 px box (C20: 8 px of ink
    // across), where GTK drew it over the box's whole 16 px content (11).
    // The first row's box is at x 380–395, y 221–236. The stroke's span
    // is 9 px (x 383.5–392.5): at 1 the faint edge columns stay under the
    // threshold (8), at 1.5 and 2 an edge's device pixel counts (9.3 and
    // 9.5), and the 16 px draw is 11 or more at every scale, so the bound
    // sits between them (M5's scale runs).
    let accent = gui_support::token_rgb("swamp-night", "accent");
    let inside = nested.opts.rect_px([382.0, 223.0, 12.0, 12.0]);
    let columns = inside[2] as usize;
    let mut inked = vec![false; columns];
    for (i, p) in gui_support::rgb_pixels(&shot, inside).iter().enumerate() {
        if p.iter().zip(accent).any(|(a, b)| a.abs_diff(b) > 40) {
            inked[i % columns] = true;
        }
    }
    let across = inked.iter().filter(|&&c| c).count() as f64 / nested.opts.scale;
    assert!(
        (6.0..=10.5).contains(&across),
        "the first row's check: {across} px of ink across (the frame's 8)"
    );
}

/// R25 (its audit: the GTK half was untested): the first run keeps the
/// person's picks and the focus when discovery answers again with
/// something else. The camera unchecked ("Add 3 sources"), a new mic comes
/// (the test channel's `sources` op stands in for discovery's answer): its
/// row is there unchecked, the camera stays unchecked with the focus on
/// its row, and the button still reads "Add 3 sources", which it adds.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_first_run_keeps_its_picks_across_a_new_inventory() {
    let mut nested = Nested::start(AREA, "first-run-picks", Options::default());
    let project = nested.env.empty_dir("rust-traits-talk");
    open(&mut nested, &project, Launch::a11y());
    assert_shows(&stage_labels(&mut nested), &["Add 4 sources"]);
    // The camera's row (screens, then cameras, mics, system audio).
    let row = nested.window(0).regions(&["first-run-row-2"])["first-run-row-2"];
    nested
        .pointer()
        .click(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    nested.window(0).wait_idle();
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["Camera · Logitech C920", "Add 3 sources"]);
    let reply = nested.window(0).ctl(json!({"op": "sources"}));
    assert_eq!(reply["ok"], true, "{reply}");
    let mut inventory = reply["inventory"].clone();
    assert!(inventory["mics"].is_array(), "no mics: {inventory}");
    inventory["mics"]
        .as_array_mut()
        .expect("the mics")
        .push(json!({
            "name": "alsa_input.usb-Elgato_Wave_3-00.mono-fallback",
            "description": "Wave:3",
            "isMonitor": false,
        }));
    let pushed = nested
        .window(0)
        .ctl(json!({"op": "sources", "inventory": inventory}));
    assert_eq!(pushed["ok"], true, "{pushed}");
    let labels = gui_support::poll(Duration::from_secs(10), "the new mic's row", || {
        let labels = stage_labels(&mut nested);
        labels.iter().any(|l| l == "Mic · Wave:3").then_some(labels)
    });
    assert_shows(&labels, &["Add 3 sources"]);
    assert_hides(&labels, &["Add 4 sources", "Add 5 sources"]);
    // The camera's new row has the focus, in the window (GTK moves a
    // focus whose widget went to the list's first row after a frame).
    nested.window(0).wait_idle();
    let local = nested.window(0).local();
    let path: Vec<&str> = local["focusPath"]
        .as_array()
        .map(|p| p.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        path.first() == Some(&"first-run-row-2") && path.contains(&"region-stage"),
        "the camera's row keeps the focus: {path:?}"
    );
    // The button adds what it says: the first screen, the default mic and
    // system audio, no camera.
    let row = nested.window(0).regions(&["first-run-add"]);
    let add = row
        .get("first-run-add")
        .copied()
        .unwrap_or_else(|| panic!("no Add button: {row:?}"));
    nested
        .pointer()
        .click(add[0] + add[2] / 2.0, add[1] + add[3] / 2.0);
    let scene = gui_support::poll(Duration::from_secs(20), "the three sources", || {
        let scene = status(&nested, &project)["project"]["scene"].clone();
        let layers = scene["layers"].as_array().map_or(0, Vec::len);
        let audio = scene["audio"].as_array().map_or(0, Vec::len);
        (layers + audio == 3).then_some(scene)
    });
    assert_eq!(scene["layers"].as_array().map_or(0, Vec::len), 1, "{scene}");
    assert_eq!(scene["layers"][0]["source"]["kind"], "screen", "{scene}");
    assert_eq!(scene["audio"][0]["role"], "mic", "{scene}");
    assert_eq!(scene["audio"][1]["role"], "system", "{scene}");
    nested.window(0).wait_idle();
    assert_clean(&mut nested);
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
    // The four adds rebuilt the live pipeline; the last rebuilds (the
    // audio tracks) send no status (G§3.18).
    gui_support::assert_live_picture_drawn(&shot);
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
    // 960×540 × the window's scale factor (G§8.4 scale runs).
    let f = gui_support::scale_factor(nested.opts.scale);
    let caps = format!("the live picture is {}×{}", 960 * f, 540 * f);
    assert!(
        gui_log(&mut nested).contains(&caps),
        "the sink composites: {caps}"
    );
}

/// S02 at `QREC_TEST_SCALE=2`: the regions scale with the output, and the
/// live sink composites at 1920×1080 (960×540 × the window's scale).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s02_at_scale_2() {
    live_at_scale("S02-scale2", 2.0);
}

/// S02 at `QREC_TEST_SCALE=1.5` (G§8.4 scale runs): GTK draws at the
/// window's whole scale factor (2) and the compositor scales it down, so
/// the sink composites at 1920×1080 here too.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s02_at_scale_1_5() {
    live_at_scale("S02-scale1.5", 1.5);
}

/// S02 at `scale`: the regions (logical px, ±1 px × scale), the live
/// picture drawn (probed in device px), the sink at 960×540 × the scale
/// factor.
fn live_at_scale(case: &str, scale: f64) {
    let mut nested = Nested::start(AREA, case, Options::at_scale(1600, 1000, scale));
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(&mut nested, &project, Launch::a11y());
    assert_stage_regions(&mut nested, &STAGE);
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["LIVE", "Live · 1920×1080 · 30 fps"]);
    assert_clean(&mut nested);
    let shot = nested.screenshot("live");
    let device = |v: f64| (v * scale).round() as u32;
    let grey = gui_support::mean_grey(
        &shot,
        [device(128.0), device(64.0), device(960.0), device(540.0)],
    );
    assert!(grey > 40.0, "the live picture is black ({grey:.1})");
    compare(&nested, &shot, "S02-live-idle.png");
    let f = gui_support::scale_factor(scale);
    let caps = format!("the live picture is {}×{}", 960 * f, 540 * f);
    assert!(
        gui_log(&mut nested).contains(&caps),
        "the sink composites at {caps} at scale {scale}"
    );
}

/// S06 at `QREC_TEST_SCALE=2` (G§8.4 scale runs).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s06_at_scale_2() {
    program_at_scale("S06-scale2", 2.0);
}

/// S06 at `QREC_TEST_SCALE=1.5`.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s06_at_scale_1_5() {
    program_at_scale("S06-scale1.5", 1.5);
}

/// S06 at `scale`: BRIEF's regions (logical px, ±1 px × scale), the
/// program picture's labels, and the timeline's strip images 72 px × the
/// window's scale factor high (144 at 2). As the frame: Face (L2) selected
/// (the inspector), BRIEF's meter levels pinned and the pointer over c2d1
/// with its tooltip, like gui_panel's S06 (the scale runs had no pointer
/// until the WP7 integration audit: no tooltip in their shots).
fn program_at_scale(case: &str, scale: f64) {
    let mut nested = Nested::start(AREA, case, Options::at_scale(1600, 1000, scale));
    let mut preset = selecting(ui(CanvasMode::Program, 2437), &["c2d0"]);
    preset.selected_layer = Some(qrec_core::LayerId::numbered(2));
    let project = fixture(&nested, DesignVariant::Base, Some(preset));
    let launch = Launch::a11y().with_env(
        "QREC_GUI_TEST_LEVELS",
        "alsa_input.fake=-22/-16,alsa_output.fake.monitor=-52/-48",
    );
    open(&mut nested, &project, launch);
    let regions = nested.window(0).regions(&[]);
    assert_regions(
        &regions,
        &gui_support::BRIEF_REGIONS,
        &gui_support::MERGED_REGIONS,
        scale,
    );
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["PROGRAM", "00:01:21:07", " / 00:04:06:11"]);
    let want = 72 * gui_support::scale_factor(scale);
    let heights = gui_support::poll(Duration::from_secs(20), "the strip images", || {
        nested.window(0).wait_idle();
        let heights = gui_support::preview_heights(&project);
        (!heights.is_empty()).then_some(heights)
    });
    assert_eq!(
        heights,
        std::collections::BTreeSet::from([want]),
        "strip images {want} px high at scale {scale}"
    );
    // The pointer over c2d1 shows its tooltip, as S06 draws it and the
    // scale-1 case (gui_panel's) shoots it: the same logical point at every
    // scale (the pointer moves in the output layout's logical px), where
    // S06's tooltip puts it (692, 756).
    nested.pointer().move_to(692.0, 756.0);
    gui_support::poll(Duration::from_secs(10), "c2d1's tooltip", || {
        let tip = nested.window(0).labels("timeline-tooltip");
        tip.iter().any(|l| l == "Take 5 · c2d1").then_some(())
    });
    nested.window(0).wait_idle();
    assert_clean(&mut nested);
    let shot = nested.screenshot("program");
    compare(&nested, &shot, "S06-program-with-selection.png");
}

// --------------------------------------------------------------- adaptive

/// The stage's preview and transport for a stage `w`×`h` at (0, 48)
/// (G§3.15's fit: the canvas into (w − 32) × (h − 68), the transport 44 px
/// above the stage's bottom).
fn stage_parts(w: f64, h: f64) -> [(&'static str, [f64; 4]); 3] {
    let canvas = qrec_core::testkit::design_state().project.canvas;
    let g = qrec_core::view::canvas::stage_geometry(w, h, &canvas);
    let at = |r: qrec_core::view::Rect| [r.x, 48.0 + r.y, r.w, r.h];
    [
        ("region-stage", [0.0, 48.0, w, h]),
        ("preview", at(g.preview)),
        ("transport", at(g.transport)),
    ]
}

/// S02 at 1300×1000 (G§0.3, G§3.15, DESIGN Layout): Medium, the panel
/// docked at 320 with the inspector 2-up, the stage 980 wide with the
/// preview fitted in it. No frame: the shot goes to the M4 review.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s02_at_1300_wide() {
    let mut nested = Nested::start(AREA, "S02-1300", Options::size(1300, 1000));
    let scale = nested.opts.scale;
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(&mut nested, &project, Launch::a11y());
    let gui = nested.window(0);
    assert_eq!(gui.local()["layout"]["width"], "Medium");
    let regions = gui.regions(&[]);
    let mut want = vec![
        ("region-header", [0.0, 0.0, 1300.0, 48.0]),
        ("region-panel", [980.0, 48.0, 320.0, 920.0]),
        ("region-timeline", [0.0, 656.0, 980.0, 312.0]),
        ("region-hintbar", [0.0, 968.0, 1300.0, 32.0]),
    ];
    want.extend(stage_parts(980.0, 608.0));
    let names: Vec<&str> = want.iter().map(|(n, _)| *n).collect();
    assert_regions(&regions, &want, &names, scale);
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["LIVE", "Live · 1920×1080 · 30 fps"]);
    assert_hints_whole(&mut nested, true);
    assert_clean(&mut nested);
    let shot = nested.screenshot("live");
    let preview = nested.opts.device(stage_parts(980.0, 608.0)[1].1);
    gui_support::assert_live_picture_drawn_at(&shot, preview);
}

/// S02 at 1000×1000: Narrow, the panel an overlay sheet: closed, the stage
/// takes the whole width; the header's layers button opens the sheet over
/// the stage's right (384 wide, under the header, down to the hint bar),
/// and `Esc` closes it.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s02_at_1000_wide_with_the_sheet() {
    let mut nested = Nested::start(AREA, "S02-1000", Options::size(1000, 1000));
    let scale = nested.opts.scale;
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(&mut nested, &project, Launch::a11y());
    let gui = nested.window(0);
    assert_eq!(gui.local()["layout"]["width"], "Narrow");
    let regions = gui.regions(&["layers-button"]);
    assert!(!regions.contains_key("region-panel"), "{regions:?}");
    let mut want = vec![
        ("region-header", [0.0, 0.0, 1000.0, 48.0]),
        ("region-timeline", [0.0, 656.0, 1000.0, 312.0]),
        ("region-hintbar", [0.0, 968.0, 1000.0, 32.0]),
    ];
    want.extend(stage_parts(1000.0, 608.0));
    let names: Vec<&str> = want.iter().map(|(n, _)| *n).collect();
    assert_regions(&regions, &want, &names, scale);
    // 1000 px can't hold S02's eight hints and the status: the last ones
    // are left out whole (the `?` cap used to show without "All keys").
    let shown = assert_hints_whole(&mut nested, false);
    assert!(!shown.contains(&"All keys".to_owned()), "{shown:?}");
    assert_clean(&mut nested);
    let shot = nested.screenshot("closed");
    let preview = nested.opts.device(stage_parts(1000.0, 608.0)[1].1);
    gui_support::assert_live_picture_drawn_at(&shot, preview);
    let [x, y, w, h] = regions["layers-button"];
    nested.pointer().click(x + w / 2.0, y + h / 2.0);
    let open = gui_support::poll(Duration::from_secs(10), "the sheet", || {
        let gui = nested.window(0);
        gui.wait_idle();
        let regions = gui.regions(&[]);
        regions.contains_key("region-panel").then_some(regions)
    });
    assert_regions(
        &open,
        &[("region-panel", [616.0, 48.0, 384.0, 920.0])],
        &["region-panel"],
        scale,
    );
    let overlays = nested.window(0).local()["overlays"].clone();
    assert_eq!(overlays, json!(["LayersSheet"]), "{overlays}");
    assert_clean(&mut nested);
    nested.screenshot("sheet");
    nested.keys(&["-k", "Escape"]);
    gui_support::poll(Duration::from_secs(10), "the sheet closing", || {
        let gui = nested.window(0);
        gui.wait_idle();
        (!gui.regions(&[]).contains_key("region-panel")).then_some(())
    });
}

/// S06 at 1600×800 (DESIGN Layout: compact under 820 high): the timeline
/// keeps 232 px (toolbar 36, ruler 28, video 72, lanes 48) and the stage
/// shrinks to 488, its preview fitted in it; the panel spans the stage and
/// the timeline.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s06_at_1600_by_800() {
    let mut nested = Nested::start(AREA, "S06-800", Options::size(1600, 800));
    let scale = nested.opts.scale;
    let preset = selecting(ui(CanvasMode::Program, 2437), &["c2d0"]);
    let project = fixture(&nested, DesignVariant::Base, Some(preset));
    open(&mut nested, &project, Launch::a11y());
    let gui = nested.window(0);
    assert_eq!(gui.local()["layout"]["compact"], true);
    let regions = gui.regions(&[]);
    let mut want = vec![
        ("region-header", [0.0, 0.0, 1600.0, 48.0]),
        ("region-panel", [1216.0, 48.0, 384.0, 720.0]),
        ("region-timeline", [0.0, 536.0, 1216.0, 232.0]),
        ("timeline-toolbar", [0.0, 536.0, 1216.0, 36.0]),
        ("ruler", [0.0, 572.0, 1216.0, 28.0]),
        ("region-hintbar", [0.0, 768.0, 1600.0, 32.0]),
    ];
    want.extend(stage_parts(1216.0, 488.0));
    let names: Vec<&str> = want.iter().map(|(n, _)| *n).collect();
    assert_regions(&regions, &want, &names, scale);
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["PROGRAM", "00:01:21:07", " / 00:04:06:11"]);
    assert_hints_whole(&mut nested, true);
    assert_clean(&mut nested);
    nested.screenshot("program");
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

/// `qrec record start` in the background (`Nested::cli_background`, G§3.17:
/// its output in `logs/<case>-cli.log`, and it dies with the test's thread,
/// so a failed assert never leaves it running, R69): it answers when the
/// take starts (after the countdown) or the countdown is cancelled.
fn record_start_in_background(nested: &Nested, project: &Path) -> Child {
    nested.cli_background(project, &["record", "start", "--json"])
}

/// Cancels the countdown (or the take), waits up to 20 s for the
/// background `record start` to answer, then for idle.
fn cancel_record(nested: &Nested, project: &Path, start: Child) {
    let cancel = nested.cli_json(project, &["record", "cancel"]);
    assert_eq!(cancel["ok"], true, "{cancel}");
    reap_within(start, Duration::from_secs(20), "qrec record start");
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
        let start = record_start_in_background(&nested, &project);
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
    // The digit's plate (C07; G§0's M4 decision: the digit over the dim
    // alone read 2.53:1 over a white screen): inside the ring, off the
    // digit, the preview is `video-scrim` darker than the dim alone, so no
    // channel there is brighter than `video-scrim` over the dim over white
    // (58): the fake screen's green bar under it reads 152 dimmed alone.
    let inside = nested.opts.rect_px([666.0, 392.0, 4.0, 4.0]);
    let brightest = gui_support::rgb_pixels(&shot, inside)
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(255);
    assert!(
        brightest <= 62,
        "the countdown's plate under the digit: a channel at {brightest}"
    );
    // The hint bar (WP6, round 3) says the countdown's keys, as S03 draws.
    let hints = nested.window(0).ctl(json!({"op": "hints"}));
    let line: Vec<String> = hints["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| format!("{} {}", item["keys"], item["label"].as_str().unwrap_or("?")))
        .collect();
    assert_eq!(
        line,
        [r#"[["Esc"]] Cancel"#, r#"[["R"]] Cancel"#],
        "{hints}"
    );
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

/// A two-digit countdown (not drawn: C07 and S03 draw one digit):
/// `recording.countdownSecs` 10 and `qrec record start`. In the `countdown`
/// type "10" would run into the ring (about 190 px of ink in a 188 px
/// opening), so it shrinks to stay inside it (`countdown::digit_fit`):
/// every bright pixel of the ring's disc (the digit's `video-text`) is
/// inside the ring's inner edge, and there is a digit's worth of them.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_two_digit_countdown_fits_its_ring() {
    let mut nested = Nested::start(AREA, "countdown-10", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    let set = nested.cli_json(&project, &["project", "set", "--countdown", "10"]);
    assert_eq!(set["ok"], true, "{set}");
    open(&mut nested, &project, Launch::a11y());
    let mut attempt = 0;
    let (shot, start) = loop {
        attempt += 1;
        let start = record_start_in_background(&nested, &project);
        wait_phase(&nested, &project, "countdown", Duration::from_secs(20));
        nested.window(0).wait_idle();
        let shot = nested.screenshot("countdown");
        // Still on 10 after the shot: the shot shows 10.
        let record = status(&nested, &project)["runtime"]["record"].clone();
        if record["phase"] == "countdown" && record["remaining"] == 10 {
            break (shot, start);
        }
        assert!(attempt < 3, "never shot on 10: {record}");
        cancel_record(&nested, &project, start);
    };
    // The ring is centered on the preview (STAGE's (128, 64, 960, 540)):
    // its outer radius 100, its 6 px stroke inside that.
    let s = nested.opts.scale;
    let (cx, cy) = (608.0 * s, 334.0 * s);
    let outer = 100.0 * s;
    let inner = (100.0 - 6.0) * s;
    let square = nested.opts.rect_px([508.0, 234.0, 200.0, 200.0]);
    let pixels = gui_support::rgb_pixels(&shot, square);
    let mut bright = 0;
    let mut farthest: f64 = 0.0;
    for (i, p) in pixels.iter().enumerate() {
        let x = f64::from(square[0]) + (i % square[2] as usize) as f64 + 0.5;
        let y = f64::from(square[1]) + (i / square[2] as usize) as f64 + 0.5;
        let r = (x - cx).hypot(y - cy);
        if r < outer && p.iter().all(|&c| c >= 200) {
            bright += 1;
            farthest = farthest.max(r);
        }
    }
    assert!(
        bright as f64 >= 2000.0 * s * s,
        "the digits' ink: {bright} bright pixels"
    );
    assert!(
        farthest < inner,
        "the digits reach {farthest:.1} px from the center, the ring's inner edge is at {inner:.1}"
    );
    cancel_record(&nested, &project, start);
    assert_clean(&mut nested);
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
    // WP10's shot review: the clip recorded over is hatched in its lanes
    // too, and it shows no selection: S04b draws neither its 2 px stroke
    // (c2d0's right edge at x 617–618) nor the lanes' selected tint (the
    // lanes were a flat tint + `selected`, the stroke dimmed `accent`).
    let lane = gui_support::rgb_pixels(&shot, nested.opts.rect_px([460.0, 801.0, 140.0, 12.0]));
    let stripes = lane.iter().filter(|p| p[0] >= 0x44).count();
    assert!(stripes > 50, "the hatch over A1 under c2d0: {stripes} px");
    // An `accent` stroke at the recording's 0.6 opacity (D-43) keeps every
    // channel at 0.6 of `accent`'s or more, whatever is under it; the
    // thumbnails under c2d0's right edge stay under 0.55 of it (128 and 137
    // of these 216 px were the stroke's before).
    let accent = gui_support::token_rgb("swamp-night", "accent");
    let edge = gui_support::rgb_pixels(&shot, nested.opts.rect_px([616.0, 722.0, 3.0, 72.0]));
    let stroked = edge
        .iter()
        .filter(|p| {
            p.iter()
                .zip(accent)
                .all(|(&c, a)| f64::from(c) >= 0.55 * f64::from(a))
        })
        .count();
    assert_eq!(
        stroked, 0,
        "c2d0 shows no selection stroke while recorded over"
    );
    // Between the stripes, the lane's ground is its take's tint, as S04b's
    // `#342920` (with the selection's tint over it, `#3C3421`): a third of
    // the lane at least, within the suites' 4 levels.
    let ground = lane
        .iter()
        .filter(|p| {
            p.iter()
                .zip([0x34, 0x29, 0x20])
                .all(|(a, b)| a.abs_diff(b) <= 4)
        })
        .count();
    assert!(
        ground * 3 >= lane.len(),
        "the lane's ground under c2d0 is its take's tint: {ground} of {} px",
        lane.len()
    );
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
    let stop = nested.cli_background(&project, &["record", "stop"]);
    wait_phase(&nested, &project, "finalizing", Duration::from_secs(20));
    nested.window(0).wait_idle();
    let shot = nested.screenshot("finalizing");
    let labels = stage_labels(&mut nested);
    assert_shows(&labels, &["Saving take 10…", "LIVE"]);
    assert_hides(&labels, &["REC"]);
    assert_clean(&mut nested);
    compare(&nested, &shot, "S05-finalizing.png");
    wait_phase(&nested, &project, "idle", Duration::from_secs(30));
    let _ = gui_support::reap_within(stop, gui_support::CLI_TIMEOUT, "qrec record stop");
}

/// S05's saving ghost at the frozen clock's `t_ms` (`QREC_GUI_TEST_T_MS`),
/// motion reduced or not: the pixels of its hatch's lower half, under the
/// label and its spinner (which spins under reduced motion too, G§1.3).
fn saving_ghost(case: &str, reduced: bool, t_ms: &str) -> Vec<[u8; 3]> {
    let mut nested = Nested::start(
        AREA,
        case,
        Options::default().exclusive(
            "the ghosts of three takes compared pixel for pixel: a take's length, and so the \
             view's fit, is its real recording time",
        ),
    );
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
        Launch::a11y()
            .with_env("QREC_FAKE_RECORD_OFFSET_FRAMES", "4842")
            .with_env("QREC_FAKE_EOS_DELAY_MS", "8000")
            .with_env("QREC_GUI_TEST_T_MS", t_ms),
    );
    let start = nested.cli_json(&project, &["record", "start"]);
    assert_eq!(start["ok"], true, "{start}");
    wait_phase(&nested, &project, "recording", Duration::from_secs(30));
    let stop = nested.cli_background(&project, &["record", "stop"]);
    wait_phase(&nested, &project, "finalizing", Duration::from_secs(20));
    nested.window(0).wait_idle();
    let shot = nested.screenshot("saving-ghost");
    assert_clean(&mut nested);
    // The ghost spans x 798–1208 of the video row (y 720–796).
    let pixels = gui_support::rgb_pixels(&shot, nested.opts.rect_px([830.0, 760.0, 340.0, 30.0]));
    wait_phase(&nested, &project, "idle", Duration::from_secs(30));
    let _ = gui_support::reap_within(stop, gui_support::CLI_TIMEOUT, "qrec record stop");
    pixels
}

/// The saving ghost's stripes hold still under reduced motion (DESIGN
/// Progress: static stripes; WP10's audit, `draw::Scene::still`): finalizing
/// at two moments of the frozen clock half a stripe's step apart (0 and
/// 600 ms of its 1.2 s step), its hatch is the same picture; with motion
/// allowed the later moment moves it (the check has teeth). WP10's code
/// audit: only `saving_phase` was tested, not the timeline's use of the
/// motion policy.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s05_the_saving_ghost_holds_still_under_reduced_motion() {
    let still_at_0 = saving_ghost("S05-still-0", true, "0");
    let still_at_600 = saving_ghost("S05-still-600", true, "600");
    let moving_at_600 = saving_ghost("S05-moving-600", false, "600");
    assert!(
        still_at_0 == still_at_600,
        "under reduced motion the stripes moved with the clock"
    );
    assert!(
        moving_at_600 != still_at_600,
        "with motion allowed the clock didn't move the stripes"
    );
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
    let (x, y) = nested.opts.pixel_px(600.0, 300.0);
    let out = run_tool(
        Command::new("ffmpeg").args([
            "-v",
            "error",
            "-i",
            &png,
            "-vf",
            &format!("crop=1:1:{x}:{y},format=rgb24"),
            "-f",
            "rawvideo",
            "-",
        ]),
        gui_support::TOOL_TIMEOUT,
    );
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
    // `undo` takes that one entry back: L2 where it was (R26).
    let undo = nested.cli_json(&project, &["undo"]);
    assert_eq!(undo["ok"], true, "{undo}");
    let data = status(&nested, &project);
    assert_eq!(
        data["project"]["scene"]["layers"][1]["rect"],
        json!({"x": 1500, "y": 760, "w": 384, "h": 288}),
        "undone"
    );
    assert_eq!(
        data["history"]["cursor"], history_before,
        "{}",
        data["history"]
    );
}

/// The history cursor of `project`.
fn cursor(nested: &Nested, project: &Path) -> u64 {
    status(nested, project)["history"]["cursor"]
        .as_u64()
        .unwrap_or(0)
}

/// Waits (10 s) until L2 sits at `(x, y)` with the history at `cursor` and
/// no burst runs, then checks nothing more came: a burst is one entry.
fn wait_nudged(nested: &mut Nested, project: &Path, cursor_at: u64, (x, y): (i64, i64)) {
    let want = json!({"x": x, "y": y, "w": 384, "h": 288});
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let data = status(nested, project);
        let rect = data["project"]["scene"]["layers"][1]["rect"].clone();
        let at = data["history"]["cursor"].as_u64().unwrap_or(0);
        let idle = nested.window(0).local()["drag"] == Value::Null;
        if rect == want && at == cursor_at && idle {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the nudges: {rect} at cursor {at} (want {want} at {cursor_at}), burst over: {idle}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    nested.window(0).wait_idle();
    assert_eq!(cursor(nested, project), cursor_at, "one entry per burst");
}

/// D-33: an arrow key pressed three times with the canvas focused is three
/// bursts of one press (`wtype` releases each key): three `LayerSet`s, each
/// 1 px; Shift+arrow nudges 10 px. A key held past the repeat delay (sway's
/// 600 ms, then 25 a second) is one burst: one `LayerSet`, which one `undo`
/// takes back (R19, R26: the releases never reached the stage, so the taps
/// merged into one entry and a hold split into two).
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
    let c0 = cursor(&nested, &project);
    nested.keys(&["-k", "Left", "-k", "Left", "-k", "Up"]);
    wait_nudged(&mut nested, &project, c0 + 3, (1498, 759));
    nested.keys(&["-M", "shift", "-k", "Right", "-m", "shift"]);
    wait_nudged(&mut nested, &project, c0 + 4, (1508, 759));
    nested.screenshot("nudged");
    // Held for 1.5 s: the press, the repeats from 600 ms, the release.
    nested.keys(&["-P", "Left", "-s", "1500", "-p", "Left"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let held = loop {
        let data = status(&nested, &project);
        let x = data["project"]["scene"]["layers"][1]["rect"]["x"]
            .as_i64()
            .unwrap_or(0);
        let at = data["history"]["cursor"].as_u64().unwrap_or(0);
        if at > c0 + 4 && nested.window(0).local()["drag"] == Value::Null {
            break x;
        }
        assert!(
            Instant::now() < deadline,
            "the held key: x {x} at cursor {at}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    nested.window(0).wait_idle();
    assert!(held < 1507, "the repeats moved it: x {held}");
    assert_eq!(cursor(&nested, &project), c0 + 5, "a held key is one entry");
    let undo = nested.cli_json(&project, &["undo"]);
    assert_eq!(undo["ok"], true, "{undo}");
    let rect = status(&nested, &project)["project"]["scene"]["layers"][1]["rect"].clone();
    assert_eq!(
        rect,
        json!({"x": 1508, "y": 759, "w": 384, "h": 288}),
        "one undo takes the hold back"
    );
    assert_clean(&mut nested);
}

/// Selects L2 on the 1:2 Live preview (a click; it focuses the canvas) and
/// waits for the `SetUi` (up to 20 s: a send delay holds it).
fn select_l2(nested: &mut Nested, project: &Path) {
    let [x, y, w, h] = L2_BOX;
    nested.pointer().click(x + w / 2.0, y + h / 2.0);
    let deadline = Instant::now() + Duration::from_secs(20);
    while status(nested, project)["ui"]["selectedLayer"] != "L2" {
        assert!(Instant::now() < deadline, "L2 never selected");
        thread::sleep(Duration::from_millis(50));
    }
    nested.window(0).wait_idle();
    assert_eq!(nested.window(0).local()["focus"], "stage");
}

/// R23 at the stage (its audit): a commit's reply that comes after a newer
/// commit leaves the newer one's layout. Three Left taps 150 ms apart with
/// every command held 225 ms on its way (`QREC_GUI_TEST_SEND_DELAY_MS`, a
/// slow Session): the first reply comes between the second and the third
/// tap, before the second's snapshot. The third goes on from the second's
/// layout: x 1497, three entries. With any reply clearing the newest
/// commit's layout, the third went on from the first's snapshot (1498).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_late_reply_leaves_the_newer_commit() {
    let mut nested = Nested::start(AREA, "late-reply", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(
        &mut nested,
        &project,
        Launch::a11y().with_env("QREC_GUI_TEST_SEND_DELAY_MS", "225"),
    );
    select_l2(&mut nested, &project);
    let c0 = cursor(&nested, &project);
    nested.keys(&[
        "-k", "Left", "-s", "150", "-k", "Left", "-s", "150", "-k", "Left",
    ]);
    wait_nudged(&mut nested, &project, c0 + 3, (1497, 760));
    assert_clean(&mut nested);
}

/// R23 at the stage, the press (its audit): a drag that starts while a
/// commit waits for its reply starts from that commit's layout (the box
/// the canvas shows: `newest_scene`). A Left tap, then a 10 px drag of L2
/// (20 canvas px, snapping off) within the 2 s every command is held on
/// its way: x 1519, two entries. Pressed on the snapshot's layout, it
/// moved L2 from 1500 and dropped the tap (1520).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_drag_during_a_commit_starts_from_it() {
    let mut nested = Nested::start(AREA, "drag-during-commit", Options::default());
    let mut preset = ui(CanvasMode::Live, 7391);
    preset.snapping = false;
    let project = fixture(&nested, DesignVariant::Base, Some(preset));
    open(
        &mut nested,
        &project,
        Launch::a11y().with_env("QREC_GUI_TEST_SEND_DELAY_MS", "2000"),
    );
    select_l2(&mut nested, &project);
    let c0 = cursor(&nested, &project);
    let before = gui_log(&mut nested).lines().count();
    nested.keys(&["-k", "Left"]);
    // The tap's `LayerSet` went out (to be held 2 s): its layout shows.
    gui_support::poll(Duration::from_secs(5), "the tap's LayerSet", || {
        let log = gui_log(&mut nested);
        log.lines()
            .skip(before)
            .any(|l| l.contains("dispatch") && l.contains("layerSet"))
            .then_some(())
    });
    assert_eq!(cursor(&nested, &project), c0, "the tap's commit waits");
    let [x, y, w, h] = L2_BOX;
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let pointer = nested.pointer();
    pointer.move_to(cx, cy);
    pointer.button(BUTTON_LEFT, true);
    for i in 1..=5 {
        pointer.move_to(cx + 2.0 * f64::from(i), cy);
        thread::sleep(Duration::from_millis(16));
    }
    pointer.button(BUTTON_LEFT, false);
    assert_eq!(
        cursor(&nested, &project),
        c0,
        "the drag came before the tap's reply"
    );
    wait_nudged(&mut nested, &project, c0 + 2, (1519, 760));
    assert_clean(&mut nested);
}

/// R22's wiring (its audit): a key while the program plays seeks the
/// playhead to the player's frame first (R18), and the sync takes that
/// seek as its own (`KeyRouter::player_frame` tells it,
/// `key_took_player_frame`), so the playing player isn't pulled back. With
/// every command held 200 ms on its way, the second `l` (2×) seeks to the
/// player's frame, whose snapshot comes 200 ms of playing later: the
/// player is 6 and more frames on, past the 2-frame tolerance, and a seek
/// the sync didn't know for its own sought it back ("program sync: seek").
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_keys_seek_while_playing_never_pulls_the_player_back() {
    let mut nested = Nested::start(
        AREA,
        "key-seek-playing",
        Options::default()
            .exclusive("the Program plays in real time while the case reads where it is"),
    );
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Program, 1200)),
    );
    open(
        &mut nested,
        &project,
        Launch::a11y().with_env("QREC_GUI_TEST_SEND_DELAY_MS", "200"),
    );
    nested.keys(&["-k", "l"]);
    gui_support::poll(Duration::from_secs(20), "the program playing", || {
        let local = nested.window(0).local();
        (local["programPlaying"] == true).then_some(())
    });
    thread::sleep(Duration::from_millis(500));
    let before = gui_log(&mut nested).lines().count();
    nested.keys(&["-k", "l"]);
    gui_support::poll(Duration::from_secs(10), "2x", || {
        let local = nested.window(0).local();
        (local["playingRate"] == 2.0).then_some(())
    });
    // The pre-step's snapshot, 200 ms on, and the sync's step after it.
    thread::sleep(Duration::from_millis(1000));
    let log = gui_log(&mut nested);
    let after: Vec<&str> = log.lines().skip(before).collect();
    assert!(
        after
            .iter()
            .any(|l| l.contains("dispatch") && l.contains("seek")),
        "the key's pre-step seek went out: {after:#?}"
    );
    let pulled: Vec<&&str> = after
        .iter()
        .filter(|l| l.contains("program sync: seek"))
        .collect();
    assert!(pulled.is_empty(), "the player was sought back: {pulled:#?}");
    let pause = nested
        .window(0)
        .ctl(json!({"op": "dispatch", "command": {"cmd": "pause"}}));
    assert_eq!(pause["ok"], true, "{pause}");
    nested.window(0).wait_idle();
    assert_clean(&mut nested);
}

/// D-51 (R18's follow-up): a step while the program plays pauses it and
/// goes from the frame the picture showed. `→` by its key lands one frame
/// on; `Shift+→` and `↓`, run as their transport buttons run them
/// (`KeyRouter::run_binding`), a second on and on the next edit point.
/// Each ends paused, the picture on the playhead. Before, the step's seek
/// landed within the sync's 2-frame tolerance of the playing player, which
/// played on: a frame step barely moved, and the playhead went on with the
/// next once-a-second seek.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_step_while_playing_pauses_and_goes_from_the_frame_shown() {
    let mut nested = Nested::start(
        AREA,
        "step-playing",
        Options::default()
            .exclusive("the Program plays in real time while the case reads where it is"),
    );
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Program, 1200)),
    );
    open(&mut nested, &project, Launch::a11y());
    // The frame a player position shows at the fixture's 30 fps (as
    // `ns_to_frame_floor`).
    let frame_of = |ns: u64| ((ns + 1) * 30 - 1) / 1_000_000_000;
    let shown = |nested: &mut Nested| {
        let local = nested.window(0).local();
        frame_of(
            local["playerNs"]
                .as_u64()
                .unwrap_or_else(|| panic!("{local}")),
        )
    };
    let play = |nested: &mut Nested| {
        let reply = nested
            .window(0)
            .ctl(json!({"op": "dispatch", "command": {"cmd": "play"}}));
        assert_eq!(reply["ok"], true, "{reply}");
        gui_support::poll(Duration::from_secs(20), "the program playing", || {
            (nested.window(0).local()["programPlaying"] == true).then_some(())
        });
        thread::sleep(Duration::from_millis(600));
    };
    // Where the step ended: paused, and the picture on the playhead.
    let landed = |nested: &mut Nested| {
        gui_support::poll(Duration::from_secs(10), "the step's pause", || {
            (status(nested, &project)["runtime"]["playing"] == false).then_some(())
        });
        gui_support::poll(
            Duration::from_secs(10),
            "the picture on the playhead",
            || {
                nested.window(0).wait_idle();
                let at = status(nested, &project)["ui"]["playhead"].as_u64()?;
                let ns = nested.window(0).local()["playerNs"].as_u64()?;
                (frame_of(ns) == at).then_some(at)
            },
        )
    };
    let run = |nested: &mut Nested, id: &str| {
        let reply = nested.window(0).ctl(json!({"op": "action", "id": id}));
        assert_eq!(reply["ok"], true, "{id}: {reply}");
    };

    // →, its key (wtype pauses 400 ms for its keymap first: the picture
    // plays on meanwhile, so the bound is loose).
    play(&mut nested);
    let before = shown(&mut nested);
    nested.keys(&["-k", "Right"]);
    let at = landed(&mut nested);
    assert!(
        (before + 1..=before + 45).contains(&at),
        "→ landed at {at}; the picture showed {before} before the key"
    );

    // Shift+→ (`seek.forward`), at once: a second on from the frame shown.
    play(&mut nested);
    let before = shown(&mut nested);
    run(&mut nested, "seek.forward");
    let at = landed(&mut nested);
    assert!(
        (before + 30..=before + 36).contains(&at),
        "Shift+→ landed at {at}; the picture showed {before}"
    );

    // ↓ (`seek.next-edit`, the transport's Next edit): the next clip
    // boundary after the frame shown.
    play(&mut nested);
    let clips = nested.cli_json(&project, &["clip", "list"])["data"].clone();
    let mut edits: Vec<u64> = clips
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| {
            let start = c["start"].as_u64().unwrap_or(0);
            [start, start + c["duration"].as_u64().unwrap_or(0)]
        })
        .collect();
    edits.sort_unstable();
    let before = shown(&mut nested);
    run(&mut nested, "seek.next-edit");
    let at = landed(&mut nested);
    let next_after = |f: u64| edits.iter().copied().find(|&e| e > f);
    let candidates: Vec<u64> = (before..=before + 6).filter_map(next_after).collect();
    assert!(
        candidates.contains(&at),
        "↓ landed at {at}; the picture showed {before}, the next edits {candidates:?}"
    );
    assert_clean(&mut nested);
}

/// D-51's other half: Home while the program plays (an absolute seek: the
/// program plays on from the start) and a click on the ruler land, whatever
/// the sync's once-a-second seek does meanwhile. Every command is held
/// 400 ms on its way (`QREC_GUI_TEST_SEND_DELAY_MS`, a slow Session) and
/// each seek goes out 700 ms into playback, so that seek falls due while
/// the window's is on its way, its snapshot not in yet: sent then, to the
/// player's frame, it came after the window's seek and undid it, and the
/// player went back with the playhead (at full speed the window is the
/// 20–30 ms a seek takes to come back, each second). The sync holds its
/// seek while a seek the window sent is unanswered (G§3.10 step 3).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_seek_while_playing_lands_and_plays_on() {
    let mut nested = Nested::start(AREA, "seek-playing", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Program, 1200)),
    );
    open(
        &mut nested,
        &project,
        Launch::a11y().with_env("QREC_GUI_TEST_SEND_DELAY_MS", "400"),
    );
    // The frame a player position shows at the fixture's 30 fps (as
    // `ns_to_frame_floor`).
    let frame_of = |ns: u64| ((ns + 1) * 30 - 1) / 1_000_000_000;
    let shown = |nested: &mut Nested| {
        let local = nested.window(0).local();
        frame_of(
            local["playerNs"]
                .as_u64()
                .unwrap_or_else(|| panic!("{local}")),
        )
    };
    let ctl_ok = |nested: &mut Nested, request: Value| {
        let reply = nested.window(0).ctl(request.clone());
        assert_eq!(reply["ok"], true, "{request}: {reply}");
    };
    // Plays from 1200 (the CLI's seek, not held: after the window's last
    // pause and its park are answered), 700 ms in.
    let play_from_1200 = |nested: &mut Nested| {
        nested.window(0).wait_idle();
        nested.cli_json(&project, &["seek", "--to", "1200f"]);
        nested.window(0).wait_idle();
        ctl_ok(
            nested,
            json!({"op": "dispatch", "command": {"cmd": "play"}}),
        );
        gui_support::poll(Duration::from_secs(20), "the program playing", || {
            (nested.window(0).local()["programPlaying"] == true).then_some(())
        });
        let from = shown(nested);
        assert!((1200..1230).contains(&from), "plays from {from}, not 1200");
        thread::sleep(Duration::from_millis(700));
    };
    // 1.6 s after the seek (held 400 ms of it): the picture plays on from
    // the start (5 s at most: a pull back puts it past 1200), and the
    // playhead is there too.
    let plays_on_from_the_start = |nested: &mut Nested, what: &str| {
        thread::sleep(Duration::from_millis(1600));
        let at = shown(nested);
        assert!(
            at < 150,
            "{what} while playing: the picture at {at} 1.6 s after it"
        );
        let local = nested.window(0).local();
        assert_eq!(local["programPlaying"], true, "{what}: plays on: {local}");
        let playhead = status(nested, &project)["ui"]["playhead"]
            .as_u64()
            .expect("a playhead");
        assert!(playhead < 150, "{what}: the playhead at {playhead}");
        ctl_ok(
            nested,
            json!({"op": "dispatch", "command": {"cmd": "pause"}}),
        );
        gui_support::poll(Duration::from_secs(10), "the pause", || {
            (status(nested, &project)["runtime"]["playing"] == false).then_some(())
        });
    };

    // Home (`seek.start`, as its key and a button run it).
    play_from_1200(&mut nested);
    ctl_ok(&mut nested, json!({"op": "action", "id": "seek.start"}));
    plays_on_from_the_start(&mut nested, "Home");

    // A click on the ruler, 10 px into the time area: a frame near the
    // start (some 40 at fit).
    let regions = nested.window(0).regions(&["ruler", "time-area"]);
    let part = |name: &str| {
        *regions
            .get(name)
            .unwrap_or_else(|| panic!("no {name}: {regions:#?}"))
    };
    let ([_, ry, _, rh], [tx, ..]) = (part("ruler"), part("time-area"));
    play_from_1200(&mut nested);
    nested.pointer().click(tx + 10.0, ry + rh / 2.0);
    plays_on_from_the_start(&mut nested, "A click on the ruler");
    assert_clean(&mut nested);
}

/// The seeks the window dispatched so far (its log's `dispatch #n seek`).
fn seeks_dispatched(nested: &mut Nested) -> usize {
    nested
        .window(0)
        .gui_log()
        .lines()
        .filter(|l| l.contains(" dispatch #") && l.ends_with(" seek"))
        .count()
}

/// Waits for the window to dispatch a seek after the first `seen` (while
/// the program plays: the sync's once-a-second seek). Returns the count.
fn next_seek_dispatched(nested: &mut Nested, seen: usize) -> usize {
    gui_support::poll(
        Duration::from_secs(5),
        "the window's once-a-second seek",
        || {
            let now = seeks_dispatched(nested);
            (now > seen).then_some(now)
        },
    )
}

/// G§3.10 step 3: a seek from outside the window while the program plays
/// (the CLI's, an agent's) lands and the program plays on from it. Every
/// command of the window is held 600 ms on its way
/// (`QREC_GUI_TEST_SEND_DELAY_MS`), and the CLI's seek goes right after the
/// sync's once-a-second seek: that seek, to the player's frame before the
/// CLI's, came after it and undid it (the playhead, and the player with
/// it), until the core dropped it for a seek that landed since (it carries
/// the playhead revision it was based on). A relative one (`--by +10s`)
/// goes from the player's frame, not from the core's playhead a second and
/// more behind it (the Session reads the player's position as a fact).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_cli_seek_while_playing_lands_over_the_windows_sync() {
    let mut nested = Nested::start(AREA, "cli-seek-playing", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Program, 1200)),
    );
    open(
        &mut nested,
        &project,
        Launch::a11y().with_env("QREC_GUI_TEST_SEND_DELAY_MS", "600"),
    );
    let reply = nested
        .window(0)
        .ctl(json!({"op": "dispatch", "command": {"cmd": "play"}}));
    assert_eq!(reply["ok"], true, "{reply}");
    gui_support::poll(Duration::from_secs(20), "the program playing", || {
        (nested.window(0).local()["programPlaying"] == true).then_some(())
    });
    // Where the program is 1.6 s after a seek to `to`: playing on from it,
    // the picture and the playhead.
    let plays_on_from = |nested: &mut Nested, to: u64, what: &str| {
        thread::sleep(Duration::from_millis(1600));
        let at = frame_of(player_ns(nested));
        let head = playhead(nested, &project);
        let local = nested.window(0).local();
        assert_eq!(local["programPlaying"], true, "{what}: plays on: {local}");
        assert!(
            (to..to + 100).contains(&at) && (to..to + 100).contains(&head),
            "{what} to {to}: 1.6 s later the picture is at {at}, the playhead at {head}"
        );
    };

    // Absolute.
    let seen = seeks_dispatched(&mut nested);
    let seen = next_seek_dispatched(&mut nested, seen);
    nested.cli_json(&project, &["seek", "--to", "300f"]);
    plays_on_from(&mut nested, 300, "qrec seek --to 300f");

    // Relative, from the picture.
    next_seek_dispatched(&mut nested, seen);
    let from = frame_of(player_ns(&mut nested));
    let reply = nested.cli_json(&project, &["seek", "--by", "+10s"]);
    let landed = reply["data"]["result"]["playhead"]
        .as_u64()
        .unwrap_or_else(|| panic!("{reply}"));
    assert!(
        (from + 300..from + 330).contains(&landed),
        "--by +10s with the picture at {from} landed at {landed}"
    );
    plays_on_from(&mut nested, landed, "qrec seek --by +10s");

    // The pause (held too) shows the frame it parks at and stays there: the
    // once-a-second seek sent before the pause landed came after it, and the
    // paused picture went back to that seek's older frame, then forward to
    // the park's (step 2's hold).
    let before = gui_log(&mut nested).lines().count();
    let (at, ns) = pause_and_settle(&mut nested, &project);
    assert_eq!(frame_of(ns), at, "the player on the parked playhead");
    let log = gui_log(&mut nested);
    let seeks: Vec<&str> = log
        .lines()
        .skip(before)
        .skip_while(|l| !l.contains("program sync: pause"))
        .filter(|l| l.contains("program sync: seek"))
        .collect();
    assert_eq!(seeks.len(), 1, "the paused picture moved: {seeks:#?}");
    assert_clean(&mut nested);
}

// ------------------------------------------------- what a pause shows

/// The frame a player position shows at the fixture's 30 fps (as
/// `ns_to_frame_floor`).
fn frame_of(ns: u64) -> u64 {
    ((ns + 1) * 30 - 1) / 1_000_000_000
}

/// The Program player's position (`local.playerNs`).
fn player_ns(nested: &mut Nested) -> u64 {
    let local = nested.window(0).local();
    local["playerNs"]
        .as_u64()
        .unwrap_or_else(|| panic!("no player: {local}"))
}

/// The core's playhead (`qrec status`).
fn playhead(nested: &Nested, project: &Path) -> u64 {
    status(nested, project)["ui"]["playhead"]
        .as_u64()
        .expect("a playhead")
}

/// Where timeline frame `at` is in the media: the take file of L1 (the
/// full-canvas screen) and the take frame (the fixture's streams start at
/// the take's start: file frame = take frame).
fn l1_frame_at(nested: &Nested, project: &Path, at: u64) -> (PathBuf, u64) {
    let clips = nested.cli_json(project, &["clip", "list"])["data"].clone();
    let clip = clips
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| {
            let start = c["start"].as_u64().unwrap_or(u64::MAX);
            (start..start + c["duration"].as_u64().unwrap_or(0)).contains(&at)
        })
        .unwrap_or_else(|| panic!("no clip at {at}: {clips}"))
        .clone();
    let take = clip["take"].as_str().expect("a take").to_owned();
    let frame = clip["in"].as_u64().expect("an in point") + at - clip["start"].as_u64().unwrap();
    let saved = saved_project(project);
    let file = saved["takes"][&take]["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["id"] == "L1")
        .and_then(|s| s["file"].as_str())
        .unwrap_or_else(|| panic!("{take} has no L1: {saved}"))
        .to_owned();
    (project.join(file), frame)
}

/// The fixture's screens (`design::SCREEN_SIZE`) and the top-left corner
/// of them that [`shown_take_frame`] reads: testsrc2's clock and frame
/// counter, orange in a dark box (`TEXT_BOX`: x, y, w, h), over the first
/// color bar.
const SCREEN: (u32, u32) = (640, 360);
const CORNER: (u32, u32) = (128, 40);
const TEXT_BOX: [u32; 4] = [2, 2, 100, 36];
/// The text's luma (about 146 of 255) against its box's (under 70).
const TEXT_LUMA: f64 = 90.0;

/// RGB bytes of `filter`'s output for `input` (raw rgb24 on stdout).
fn ffmpeg_rgb(input: &Path, filter: &str) -> Vec<u8> {
    let out = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(input)
            .args(["-vf", filter, "-fps_mode", "passthrough"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"]),
        Duration::from_secs(60),
    );
    assert!(
        out.status.success(),
        "ffmpeg -vf {filter} on {}: {}",
        input.display(),
        out.stderr
    );
    out.stdout
}

/// The pixels of an RGB corner ([`CORNER`]) inside [`TEXT_BOX`] that are
/// testsrc2's text (luma over [`TEXT_LUMA`]).
fn text_mask(rgb: &[u8]) -> Vec<bool> {
    let [x0, y0, w, h] = TEXT_BOX;
    let mut mask = Vec::with_capacity((w * h) as usize);
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let i = ((y * CORNER.0 + x) * 3) as usize;
            let luma = 0.2126 * f64::from(rgb[i])
                + 0.7152 * f64::from(rgb[i + 1])
                + 0.0722 * f64::from(rgb[i + 2]);
            mask.push(luma > TEXT_LUMA);
        }
    }
    mask
}

/// The take frame the Program picture of `shot` shows, read from the
/// fixture's pixels: the corner of L1 (full canvas in the preview at
/// `preview`, device px) scaled back to the file's size, its text (the
/// clock and the frame counter, which change every frame) against the
/// text of `file`'s own frames `around - 3 ..= around + 3` as ffmpeg
/// decodes them. The one whose text differs in the fewest pixels, by far
/// (the others differ in 60 and more; a picture none matches fails), with
/// every candidate's count for the messages.
fn shown_take_frame(shot: &Path, preview: [f64; 4], file: &Path, around: u64) -> (u64, String) {
    let (cw, ch) = CORNER;
    let [x, y, w, h] = preview;
    let crop = [
        x.round(),
        y.round(),
        (w * f64::from(cw) / f64::from(SCREEN.0)).round(),
        (h * f64::from(ch) / f64::from(SCREEN.1)).round(),
    ];
    let shown = ffmpeg_rgb(
        shot,
        &format!(
            "crop={}:{}:{}:{},scale={cw}:{ch}:flags=area",
            crop[2], crop[3], crop[0], crop[1]
        ),
    );
    let size = (cw * ch * 3) as usize;
    assert_eq!(shown.len(), size, "the shot's corner");
    let first = around.saturating_sub(3);
    let frames = ffmpeg_rgb(
        file,
        &format!(
            "select=between(n\\,{first}\\,{}),crop={cw}:{ch}:0:0,\
             scale=in_color_matrix=bt709:in_range=tv,format=rgb24",
            around + 3
        ),
    );
    assert!(
        frames.len() >= size && frames.len().is_multiple_of(size),
        "{} bytes of frames {first}…{} of {}",
        frames.len(),
        around + 3,
        file.display()
    );
    let shown = text_mask(&shown);
    let mut scores: Vec<(usize, u64)> = frames
        .chunks_exact(size)
        .enumerate()
        .map(|(i, frame)| {
            let differ = text_mask(frame)
                .iter()
                .zip(&shown)
                .filter(|(a, b)| a != b)
                .count();
            (differ, first + i as u64)
        })
        .collect();
    let text = scores
        .iter()
        .map(|(n, f)| format!("{f}: {n} px"))
        .collect::<Vec<_>>()
        .join(", ");
    scores.sort_unstable();
    let [(best, frame), (second, _), ..] = scores[..] else {
        panic!("fewer than two candidates: {text}");
    };
    assert!(
        best * 3 < second && best < 40,
        "the picture's text matches no frame of {} around {around} ({text}): {}",
        file.display(),
        shot.display()
    );
    (frame, text)
}

/// Pauses the playing program (the window's own `Pause`) and waits until it
/// settled: the core paused, the window idle, the playhead parked (the
/// same twice, within a frame of the player's). Returns the playhead and
/// the player's position.
fn pause_and_settle(nested: &mut Nested, project: &Path) -> (u64, u64) {
    let reply = nested
        .window(0)
        .ctl(json!({"op": "dispatch", "command": {"cmd": "pause"}}));
    assert_eq!(reply["ok"], true, "{reply}");
    gui_support::poll(Duration::from_secs(10), "the pause", || {
        (status(nested, project)["runtime"]["playing"] == false).then_some(())
    });
    let mut last = None;
    gui_support::poll(Duration::from_secs(10), "the parked playhead", || {
        nested.window(0).wait_idle();
        let at = playhead(nested, project);
        let ns = player_ns(nested);
        let parked = frame_of(ns).abs_diff(at) <= 1 && last == Some(at);
        last = Some(at);
        parked.then_some((at, ns))
    })
}

/// G§3.10 step 2 (G§8.5's step 6 shot): a pause shows the frame it parks
/// the playhead at, the one the player's position is in. A sink that
/// pauses shows its preroll buffer, the frame after the one on screen (the
/// shot showed take 1's frame 29 at playhead 28, the player at 28.97
/// frames), so D-51's step "from the frame shown" started one frame behind
/// the picture. Each round plays from 1200 (c1b3: take 3 from its frame
/// 304) for a while, pauses, and reads the take frame the picture shows
/// from the fixture's pixels; then `→` moves the picture one frame on.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_pause_shows_the_frame_it_parks_at() {
    let mut nested = Nested::start(AREA, "pause-frame", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Program, 1200)),
    );
    open(&mut nested, &project, Launch::a11y());
    let preview = nested.window(0).regions(&["preview"])["preview"];
    let preview = nested.opts.device(preview);
    let ctl_ok = |nested: &mut Nested, request: Value| {
        let reply = nested.window(0).ctl(request.clone());
        assert_eq!(reply["ok"], true, "{request}: {reply}");
    };
    let mut wrong = Vec::new();
    let mut last_shown = 0;
    for (round, ms) in [500u64, 730, 990].into_iter().enumerate() {
        nested.cli_json(&project, &["seek", "--to", "1200f"]);
        nested.window(0).wait_idle();
        ctl_ok(
            &mut nested,
            json!({"op": "dispatch", "command": {"cmd": "play"}}),
        );
        gui_support::poll(Duration::from_secs(20), "the program playing", || {
            (nested.window(0).local()["programPlaying"] == true).then_some(())
        });
        thread::sleep(Duration::from_millis(ms));
        let (at, ns) = pause_and_settle(&mut nested, &project);
        let shot = nested.screenshot(&format!("paused-{round}"));
        let (file, frame) = l1_frame_at(&nested, &project, at);
        let (shown, scores) = shown_take_frame(&shot, preview, &file, frame);
        if (shown, frame_of(ns)) != (frame, at) {
            wrong.push(format!(
                "round {round}: paused at {at} (take frame {frame}), the player at {ns} ns \
                 (frame {}), the picture shows take frame {shown} ({scores})",
                frame_of(ns)
            ));
        }
        last_shown = shown;
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    // → (D-51 from the frame shown): the picture one frame on, the playhead
    // on it.
    let before = playhead(&nested, &project);
    nested.keys(&["-k", "Right"]);
    let at = gui_support::poll(Duration::from_secs(10), "the step", || {
        nested.window(0).wait_idle();
        let at = playhead(&nested, &project);
        (at != before && frame_of(player_ns(&mut nested)) == at).then_some(at)
    });
    assert_eq!(at, before + 1, "→ from {before}");
    let shot = nested.screenshot("stepped");
    let (file, frame) = l1_frame_at(&nested, &project, at);
    let (shown, scores) = shown_take_frame(&shot, preview, &file, frame);
    assert_eq!(
        (shown, shown),
        (frame, last_shown + 1),
        "→ to {at} (take frame {frame}) from a picture of take frame {last_shown}: it shows \
         take frame {shown} ({scores})"
    );
    assert_clean(&mut nested);
}

/// Closes the window through `app.quit` and waits for its exit (0): what
/// it debounced went out before its host closed, or never.
fn quit_and_wait(nested: &mut Nested) {
    let gui = nested.window(0);
    gui.quit_action("app.quit");
    assert_eq!(gui.wait_exit(Duration::from_secs(70)), Some(0), "the exit");
    gui.assert_no_criticals();
    let log = gui.gui_log();
    assert!(
        !log.contains("window is closing"),
        "a request refused by the closing host:\n{log}"
    );
}

/// The project's `qrec.json` (the window gone: its host saved it).
fn saved_project(project: &Path) -> Value {
    let text = fs::read(project.join("qrec.json")).expect("qrec.json");
    serde_json::from_slice(&text).expect("its JSON")
}

/// G§3.6 (A1's audit: only the timeline's `SetUi` went out before the
/// close): a playing program's playhead goes out before the host closes.
/// Closed 600 ms after a once-a-second seek landed, `ui.json` has the
/// player's frame, not that seek's: the next one would have come after the
/// close, refused, and the reopened project showed the playhead up to a
/// second earlier.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_playing_programs_playhead_is_saved_at_the_close() {
    let mut nested = Nested::start(
        AREA,
        "close-playing",
        Options::default()
            .exclusive("the Program plays in real time while the case reads where it is"),
    );
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Program, 1200)),
    );
    open(&mut nested, &project, Launch::a11y());
    // Walked before it plays: the close's timing is the case's.
    let a11y = nested.window(0).ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "a11y: {a11y}");
    let play = nested
        .window(0)
        .ctl(json!({"op": "dispatch", "command": {"cmd": "play"}}));
    assert_eq!(play["ok"], true, "{play}");
    let seeked = gui_support::poll(Duration::from_secs(20), "a once-a-second seek", || {
        let at = status(&nested, &project)["ui"]["playhead"].as_u64()?;
        (at > 1200).then_some(at)
    });
    thread::sleep(Duration::from_millis(600));
    let local = nested.window(0).local();
    let ns = local["playerNs"]
        .as_u64()
        .unwrap_or_else(|| panic!("no player: {local}"));
    // 30 fps.
    let player = ns * 30 / 1_000_000_000;
    assert!(
        player > seeked + 10,
        "it played on: {player} after {seeked}"
    );
    quit_and_wait(&mut nested);
    let path = project.join(".qrec").join("ui.json");
    let ui: Value = serde_json::from_slice(&fs::read(&path).expect("ui.json")).expect("its JSON");
    let saved = ui["playhead"].as_u64().unwrap_or_else(|| panic!("{ui}"));
    assert!(
        (player..=player + 15).contains(&saved),
        "the playhead saved at {saved}: the player was at {player}, the last seek at {seeked}"
    );
}

/// G§3.6 (A1's audit): a nudge burst still waiting for its key's release
/// goes out before the host closes. Left pressed and not released (the
/// burst waits up to a second for the release) and the window closed: L2
/// is saved moved. Its timer fired after the close, refused, and the burst
/// was lost.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_nudge_burst_is_saved_at_the_close() {
    let mut nested = Nested::start(AREA, "close-nudging", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(&mut nested, &project, Launch::a11y());
    select_l2(&mut nested, &project);
    // Walked before the burst (it closes the window within its second).
    let a11y = nested.window(0).ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "a11y: {a11y}");
    let held = nested.keys_held(&["-P", "Left"], 4000);
    gui_support::poll(Duration::from_secs(5), "the burst", || {
        (nested.window(0).local()["drag"] == "layerNudge").then_some(())
    });
    quit_and_wait(&mut nested);
    drop(held);
    let saved = saved_project(&project);
    let x = saved["scene"]["layers"][1]["rect"]["x"]
        .as_i64()
        .unwrap_or_else(|| panic!("no L2: {saved}"));
    assert!(x < 1500, "the burst wasn't saved: L2 at x {x}");
}

/// S10's canvas: a trim of c2d0's start edge (S08's seconds zoom, +29
/// frames) shows take 5 frame 41 with the 2 px accent stroke and "Trim
/// start · take 5 · frame 41". The trim drag is WP3's timeline: its rows
/// are required where BRIEF puts them (R70: without `video-row` the case
/// stopped, passing, before WP3 merged and on any regression since).
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
    assert_regions(
        &regions,
        &gui_support::BRIEF_REGIONS,
        &gui_support::MERGED_REGIONS,
        nested.opts.scale,
    );
    let row = regions["video-row"];
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
    // The press started the trim (a missed press says so here).
    gui_support::poll(Duration::from_secs(10), "the trim drag", || {
        (nested.window(0).local()["drag"] == "trimStart").then_some(())
    });
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

// ------------------------------------------------------------------ S24

/// S24's shots and logs: `tmp/gui-shots/s24/`.
const S24: &str = "s24";
/// S24's block over L1 and its Try again (`stage/denied.rs` names them
/// after the layer).
const DENIED_L1: &str = "denied-L1";
const RETRY_L1: &str = "denied-try-again-L1";
/// L1's area on the 1:2 preview: the whole preview (BRIEF §3.1).
const L1_AREA: [f64; 4] = [128.0, 64.0, 960.0, 540.0];
/// L2's box on it (BRIEF §3.1), inside its edge.
const L2_INSIDE: [f64; 4] = [882.0, 448.0, 184.0, 136.0];
/// `muted` at 20 % over the canvas's black: the hatch's stripe (S24's
/// "Hatch · muted 20", `#9BA58233`).
const STRIPE: [u8; 3] = [31, 33, 26];
/// S24's meters (as S06's: −16.0 and −48.0 peaks).
const S24_LEVELS: &str = "alsa_input.fake=-22/-16,alsa_output.fake.monitor=-52/-48";

/// S24's window: the fake portal answering `portal`, the frame's meters.
fn s24_launch(portal: &str) -> Launch {
    Launch::a11y()
        .with_env("QREC_FAKE_PORTAL", portal)
        .with_env("QREC_GUI_TEST_LEVELS", S24_LEVELS)
}

/// `take`'s row of `qrec take list`.
fn take_row(nested: &Nested, project: &Path, take: &str) -> Option<Value> {
    let list = nested.cli_json(project, &["take", "list"]);
    list["data"]
        .as_array()?
        .iter()
        .find(|t| t["id"] == take)
        .cloned()
}

/// Waits (20 s) until `take` is broken and the phase idle again, then for
/// the window.
fn wait_broken(nested: &mut Nested, project: &Path, take: &str) {
    gui_support::poll(Duration::from_secs(20), &format!("{take} broken"), || {
        take_row(nested, project, take).filter(|t| t["status"] == "broken")
    });
    wait_phase(nested, project, "idle", Duration::from_secs(20));
    nested.window(0).wait_idle();
}

/// Closes the toasts shown (their close buttons), within 10 s.
fn dismiss_toasts(nested: &mut Nested) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        nested.window(0).wait_idle();
        if nested.window(0).labels("toast-host").is_empty() {
            return;
        }
        assert!(Instant::now() < deadline, "the toasts never closed");
        let close = nested.window(0).regions(&["toast-dismiss"]);
        if let Some([x, y, w, h]) = close.get("toast-dismiss").copied() {
            nested.pointer().click(x + w / 2.0, y + h / 2.0);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// S24's start: the design fixture in Live with the fake portal answering
/// `portal` (`deny`; `cancel`: the picker closed), so the live preview's
/// screen is refused as the window opens (its "Live preview problem"
/// warning closed, as its 8 s would have before S24's `R`); then `R`: take
/// 10 asks the portal for its screen again, is refused and aborts (broken,
/// into the bin) before any frame, within S24's 500 ms of the key (the
/// window's log: its `recordToggle` and the Session's "take-0010 is
/// broken"). Returns the project with the window settled.
fn refused_take(nested: &mut Nested, portal: &str) -> PathBuf {
    let project = fixture(
        nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    open(nested, &project, s24_launch(portal));
    // The refusal is the canvas's block and the layer's row: no "Live
    // preview problem" toast repeats it (it came with the live statuses
    // `waitIdle` waits for).
    let toasts = nested.window(0).labels("toast-host");
    assert!(
        !toasts.iter().any(|l| l == "Live preview problem"),
        "the refusal toasted: {toasts:?}"
    );
    dismiss_toasts(nested);
    nested.keys(&["-k", "r"]);
    wait_broken(nested, &project, "take-0010");
    let log = gui_log(nested);
    let at = |what: &str| {
        let line = log
            .lines()
            .find(|l| l.contains(what))
            .unwrap_or_else(|| panic!("no {what:?} in the window's log"));
        log_ms(line).unwrap_or_else(|| panic!("no time in {line:?}"))
    };
    let took = at("take-0010 is broken") - at("recordToggle");
    assert!(
        (0..500).contains(&took),
        "take 10 aborted {took} ms after its recordToggle"
    );
    project
}

/// The time of a GUI log line ("2026-10-18 03:24:35.242 …") in ms of its
/// day.
fn log_ms(line: &str) -> Option<i64> {
    let time = line.split_whitespace().nth(1)?;
    let (hms, ms) = time.split_once('.')?;
    let mut parts = hms.split(':').map(|p| p.parse::<i64>().ok());
    let (h, m, s) = (parts.next()??, parts.next()??, parts.next()??);
    Some(((h * 60 + m) * 60 + s) * 1000 + ms.parse::<i64>().ok()?)
}

/// The block over L1's area: the title, the caption and Try again with its
/// key (`R`, `record.toggle`'s), on the LIVE preview without REC. Returns
/// the plate's rect.
fn assert_denied_block(nested: &mut Nested, title: &str, caption: &str) -> [f64; 4] {
    let labels = stage_labels(nested);
    assert_shows(&labels, &[title, caption, "Try again", "R", "LIVE"]);
    assert_hides(&labels, &["REC"]);
    let regions = nested.window(0).regions(&[DENIED_L1, RETRY_L1]);
    let part = |name: &str| {
        *regions
            .get(name)
            .unwrap_or_else(|| panic!("no {name}: {regions:#?}"))
    };
    let (plate, retry) = (part(DENIED_L1), part(RETRY_L1));
    let slack = nested.opts.scale.max(1.0);
    let near = |got: f64, want: f64, what: &str| {
        assert!(
            (got - want).abs() <= slack,
            "{what}: {got}, want {want} (the plate {plate:?}, Try again {retry:?})"
        );
    };
    // S24's plate, centered on L1's area: 190 tall (24, the 32 px icon, 16,
    // the title's 22, 4, the caption's 16, 20, the 32 px button, 24).
    let [x, y, w, h] = L1_AREA;
    near(plate[3], 190.0, "the plate's height");
    near(
        plate[0] + plate[2] / 2.0,
        x + w / 2.0,
        "the plate's center x",
    );
    near(
        plate[1] + plate[3] / 2.0,
        y + h / 2.0,
        "the plate's center y",
    );
    // Try again: 32 tall, centered, 24 above the plate's bottom.
    near(retry[3], 32.0, "Try again's height");
    near(
        retry[0] + retry[2] / 2.0,
        x + w / 2.0,
        "Try again's center x",
    );
    near(
        retry[1] + retry[3],
        plate[1] + plate[3] - 24.0,
        "Try again's bottom",
    );
    plate
}

/// The share of `rect`'s pixels (logical px) that are the hatch's stripe
/// over black (within 6 levels).
fn stripe_share(nested: &Nested, shot: &Path, rect: [f64; 4]) -> f64 {
    let pixels = gui_support::rgb_pixels(shot, nested.opts.rect_px(rect));
    let near = pixels
        .iter()
        .filter(|p| p.iter().zip(STRIPE).all(|(a, b)| a.abs_diff(b) <= 6))
        .count();
    near as f64 / pixels.len().max(1) as f64
}

/// S24's hatch: over L1's area, away from the plate and L2, the canvas's
/// black shows 45° stripes of `muted` at 20 % (none brighter); L2 Face
/// stays on top, unhatched (the fake camera's ball on black).
fn assert_hatch(nested: &Nested, shot: &Path) {
    let open = [150.0, 90.0, 320.0, 140.0];
    let share = stripe_share(nested, shot, open);
    assert!(
        share >= 0.1,
        "the hatch over L1: {share:.3} of the pixels are stripes ({})",
        shot.display()
    );
    let brightest = gui_support::rgb_pixels(shot, nested.opts.rect_px(open))
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0);
    assert!(
        brightest <= 40,
        "the hatch over black: a channel at {brightest}"
    );
    let share = stripe_share(nested, shot, L2_INSIDE);
    assert!(share < 0.02, "L2 Face is hatched: {share:.3} stripe pixels");
}

/// The hint bar's items as "<keys> <label>".
fn hint_line(nested: &mut Nested) -> Vec<String> {
    let hints = nested.window(0).ctl(json!({"op": "hints"}));
    hints["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| format!("{} {}", item["keys"], item["label"].as_str().unwrap_or("?")))
        .collect()
}

/// S24: the portal refused the screen, so take 10 aborted when `R` asked
/// for it (`QREC_FAKE_PORTAL=deny`): over L1's area the `muted-20` hatch
/// with the block ("Screen access denied", "FAKE-1 · GIGA-BYTE GS27QA" from
/// L1's source, Try again with R), L2 Face live on top, LIVE and no REC;
/// the panel's L1 row in its error state; the header idle; take 10 in the
/// bin (6); the core's error toast about take 10; the hint bar's
/// PortalDenied items. Try again runs `record.toggle` as `R` does (one
/// `recordToggle` through the Session): take 11 asks the portal again and
/// aborts too (7 in the bin). Program shows no block.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s24_portal_denied() {
    let mut nested = Nested::start(
        S24,
        "S24",
        Options::default()
            .exclusive("S24's take aborts within 500 ms of its key (the window's log)"),
    );
    let project = refused_take(&mut nested, "deny");
    assert_denied_block(
        &mut nested,
        "Screen access denied",
        "FAKE-1 \u{b7} GIGA-BYTE GS27QA",
    );
    // The panel's L1 row in its error state (S13b's, as S24 draws it).
    let row = nested.window(0).labels("layer-row-L1");
    assert_shows(
        &row,
        &["Main screen", "Access denied by the desktop portal"],
    );
    // The header idle; take 10 in the bin.
    assert_shows(&nested.window(0).labels("record-button"), &["Record"]);
    assert_eq!(
        nested.window(0).labels("tl-bin-count"),
        ["6"],
        "take 10 in the bin"
    );
    // The core's error toast (on_event's copy), about take 10.
    let toast = nested.window(0).labels("toast-host");
    assert!(
        toast.iter().any(|l| l.to_lowercase().contains("take 10")),
        "the toast: {toast:?}"
    );
    assert_eq!(
        hint_line(&mut nested),
        [
            r#"[["R"]] Try again"#,
            r#"[["Tab"]] Program"#,
            r#"[["B"]] Bin"#,
            r#"[["?"]] All keys"#
        ]
    );
    assert_stage_regions(&mut nested, &STAGE);
    assert_clean(&mut nested);
    let shot = nested.screenshot("denied");
    assert_hatch(&nested, &shot);
    compare(&nested, &shot, "S24-portal-denied-error.png");

    // Try again: `record.toggle`, as R: take 11 asks the portal for the
    // screen again (one `recordToggle` through the Session) and aborts too.
    let before = gui_log(&mut nested).lines().count();
    let retry = nested.window(0).regions(&[RETRY_L1])[RETRY_L1];
    nested
        .pointer()
        .click(retry[0] + retry[2] / 2.0, retry[1] + retry[3] / 2.0);
    wait_broken(&mut nested, &project, "take-0011");
    let log = gui_log(&mut nested);
    let sent: Vec<&str> = log
        .lines()
        .skip(before)
        .filter(|l| l.contains("dispatch") && l.contains("recordToggle"))
        .collect();
    assert_eq!(sent.len(), 1, "one recordToggle: {sent:#?}");
    assert_eq!(
        nested.window(0).labels("tl-bin-count"),
        ["7"],
        "take 11 in the bin"
    );
    assert_denied_block(
        &mut nested,
        "Screen access denied",
        "FAKE-1 \u{b7} GIGA-BYTE GS27QA",
    );

    // Program shows its own picture: no block.
    nested.keys(&["-k", "Tab"]);
    gui_support::poll(Duration::from_secs(10), "Program", || {
        (status(&nested, &project)["ui"]["canvasMode"] == "program").then_some(())
    });
    nested.window(0).wait_idle();
    assert_hides(
        &stage_labels(&mut nested),
        &["Screen access denied", "Try again"],
    );
    // ...and the hints are Program's: Try again is the block's, in Live.
    let hints = hint_line(&mut nested);
    assert!(
        !hints.iter().any(|h| h.contains("Try again"))
            && hints.iter().any(|h| h.ends_with(" Live")),
        "Program's hints: {hints:?}"
    );
    assert_clean(&mut nested);
}

/// S24's variant: the person closed the picker (`QREC_FAKE_PORTAL=cancel`):
/// the block reads "Screen picker closed" and "Choose FAKE-1 in your
/// desktop's dialog to record it." (BRIEF S24's variant note), the panel's
/// row "Screen picker closed".
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s24_screen_picker_closed() {
    let mut nested = Nested::start(
        S24,
        "S24-picker-closed",
        Options::default()
            .exclusive("S24's take aborts within 500 ms of its key (the window's log)"),
    );
    refused_take(&mut nested, "cancel");
    assert_denied_block(
        &mut nested,
        "Screen picker closed",
        "Choose FAKE-1 in your desktop's dialog to record it.",
    );
    let row = nested.window(0).labels("layer-row-L1");
    assert_shows(&row, &["Main screen", "Screen picker closed"]);
    assert_clean(&mut nested);
    let shot = nested.screenshot("picker-closed");
    assert_hatch(&nested, &shot);
    compare(&nested, &shot, "S24-portal-denied-error.png");
}

/// Not drawn (a departure, G§4.6): the block shows as soon as the live
/// preview's screen is refused, before any take; a countdown over it hides
/// the block (its Try again is `R`, which cancels the countdown then; the
/// ring and its plate take the preview's middle), and the block is back
/// once the take aborted.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s24_the_block_steps_aside_for_a_countdown() {
    let mut nested = Nested::start(S24, "S24-countdown", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        Some(ui(CanvasMode::Live, 7391)),
    );
    // The longest countdown (10 s): the stage is read while it runs, on a
    // loaded machine too.
    let set = nested.cli_json(&project, &["project", "set", "--countdown", "10"]);
    assert_eq!(set["ok"], true, "{set}");
    open(&mut nested, &project, s24_launch("deny"));
    assert_denied_block(
        &mut nested,
        "Screen access denied",
        "FAKE-1 \u{b7} GIGA-BYTE GS27QA",
    );
    assert_eq!(
        nested.window(0).labels("tl-bin-count"),
        ["5"],
        "no take yet"
    );
    // From the CLI, as S03: a start request of the window's own stays
    // unanswered through the countdown, and `waitIdle` waits for it.
    let start = record_start_in_background(&nested, &project);
    wait_phase(&nested, &project, "countdown", Duration::from_secs(20));
    nested.window(0).wait_idle();
    nested.screenshot("countdown");
    let labels = stage_labels(&mut nested);
    assert_eq!(phase(&nested, &project), "countdown", "the shot is late");
    assert_shows(&labels, &["Take 10", "Esc or R cancels"]);
    assert_hides(&labels, &["Screen access denied", "Try again"]);
    // The take asks the portal once the countdown ends, and aborts.
    let started = reap_within(start, Duration::from_secs(30), "qrec record start");
    assert!(!started.success(), "the refused take started: {started}");
    wait_broken(&mut nested, &project, "take-0010");
    assert_denied_block(
        &mut nested,
        "Screen access denied",
        "FAKE-1 \u{b7} GIGA-BYTE GS27QA",
    );
    assert_clean(&mut nested);
}
