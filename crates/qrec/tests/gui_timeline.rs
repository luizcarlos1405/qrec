//! WP3's screenshot cases (docs/GUI.md G§8.4, G§7 WP3): the timeline of
//! S01b, S04, S04b, S05, S05b, S07, S08, S09, S10, S11 and S12, S09 at
//! scale 2, the compact timeline at 1600×800, and the frame-time probe on
//! the 300-clip fixture (G§8.6). The header, the stage, the panel and the
//! hint bar are WP4's, WP5's and WP6's (merged); only the bin and the export
//! panel are WP0b's stubs until WP7 (G§7.0). S12 opens WP5's autocut page
//! and checks the preview the canvas draws.
//!
//! Each case checks the regions it can (BRIEF §2.1 and the timeline's named
//! parts, ±1 px), takes a screenshot into `tmp/gui-shots/wp3/` and puts it
//! next to the approved PNG (`<case>-compare.png`, by eye).
//!
//! `cargo test -p qrec --test gui_timeline -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon, ffmpeg).
//! `QREC_TEST_WLR=gles2` runs on the iGPU (the probe asserts p99 there).

mod gui_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use gui_support::{
    BRIEF_REGIONS, Launch, Nested, Options, WP0B_REGIONS, assert_regions, wait_for_gui_host,
};
use qrec_core::ClipId;
use qrec_core::state::{CanvasMode, UiState};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, perf_fixture, write_ui};
use qrec_shell::testkit::run_tool;
use serde_json::{Value, json};

const OPEN: Duration = Duration::from_secs(90);
const AREA: &str = "wp3";

/// The timeline's named parts (G§8.4) every non-empty case checks.
const TIMELINE_PARTS: [&str; 7] = [
    "region-timeline",
    "timeline-toolbar",
    "ruler",
    "track-headers",
    "time-area",
    "video-row",
    "lane-A1",
];

fn id(s: &str) -> ClipId {
    s.parse().unwrap()
}

/// `design_fixture(variant)` in a folder named `rust-traits-talk`, with
/// `ui` as its `.qrec/ui.json`.
fn fixture(nested: &Nested, variant: DesignVariant, ui: &UiState) -> PathBuf {
    let dir = nested.env.empty_dir("rust-traits-talk");
    design_fixture(&dir, variant);
    write_ui(&dir, ui);
    dir
}

/// Program mode with `selection`, the playhead at `playhead`, zoom and
/// scroll as given.
fn program(selection: &[&str], playhead: u64, zoom: Option<(f64, u64)>) -> UiState {
    UiState {
        canvas_mode: CanvasMode::Program,
        selection: selection.iter().map(|s| id(s)).collect(),
        playhead,
        zoom: zoom.map(|(z, _)| z),
        scroll_px: zoom.map_or(0.0, |(z, from)| z * from as f64),
        ..UiState::default()
    }
}

/// Launches the window on `project` and waits until it is open and idle.
fn open(nested: &mut Nested, project: &Path, launch: Launch) {
    nested.launch(project, launch);
    wait_for_gui_host(nested, project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
}

/// A screenshot, and the comparison with the approved PNG next to it.
fn shoot(nested: &Nested, name: &str, png: Option<&str>) -> PathBuf {
    let shot = nested.screenshot(name);
    if let Some(png) = png {
        let reference = gui_support::repo().join("design/screens").join(png);
        let out = nested
            .shots
            .join(format!("{}-{name}-compare.png", nested.case));
        let run = run_tool(
            Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-i"])
                .arg(&shot)
                .arg("-i")
                .arg(&reference)
                .args([
                    "-filter_complex",
                    "[0]scale=1600:1000[a];[1]scale=1600:1000[b];[a][b]hstack=inputs=2",
                ])
                .arg(&out),
            gui_support::TOOL_TIMEOUT,
        );
        assert!(
            run.status.success(),
            "ffmpeg compare for {name}: {}",
            run.stderr
        );
    }
    shot
}

/// BRIEF §2.1's rects for the parts that are there (the timeline's are
/// required).
fn check_regions(nested: &mut Nested, extra_required: &[&str]) {
    let gui = nested.window(0);
    let regions = gui.regions(&[]);
    let mut required: Vec<&str> = WP0B_REGIONS.to_vec();
    required.extend(TIMELINE_PARTS);
    required.extend(extra_required);
    assert_regions(&regions, &BRIEF_REGIONS, &required, nested.opts.scale);
}

/// The test channel's `local`.
fn local(nested: &mut Nested) -> Value {
    nested.window(0).local()
}

/// No caught panic, no ERROR line in the window's log.
fn assert_clean(nested: &mut Nested) {
    let gui = nested.window(0);
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    gui.assert_no_criticals();
    let log = gui.gui_log();
    let errors: Vec<&str> = log.lines().filter(|l| l.contains(" ERROR ")).collect();
    assert!(errors.is_empty(), "ERROR lines: {errors:#?}");
    assert!(!log.contains(" CSS "), "CSS errors:\n{log}");
}

/// The empty a11y walk (launched with `Launch::a11y()`).
fn assert_a11y(nested: &mut Nested) {
    let a11y = nested.window(0).ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "{a11y}");
}

/// Polls `qrec status --json` until the record phase is `want`.
fn wait_record_phase(nested: &Nested, project: &Path, want: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = nested.cli_json(project, &["status"]);
        if status["data"]["runtime"]["record"]["phase"] == want {
            return status;
        }
        assert!(Instant::now() < deadline, "never {want}: {status}");
        thread::sleep(Duration::from_millis(100));
    }
}

/// A point inside the video row at window `x`.
const VIDEO_Y: f64 = 758.0;

// ------------------------------------------------------------- fit (S07)

/// S07: fit zoom, nothing selected, the clip tooltip on c1b6; labels
/// collapse with width (with the 8 px inset c8a1 is 63.8 px: "1").
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s07_fit() {
    let mut nested = Nested::start(AREA, "s07", Options::default());
    let project = fixture(&nested, DesignVariant::Base, &program(&[], 2437, None));
    open(&mut nested, &project, Launch::a11y());
    check_regions(&mut nested, &["lane-A2"]);
    assert_a11y(&mut nested);
    // c1b6 is at window x 445.3 → 466.9 at fit.
    nested.pointer().move_to(452.0, 784.0);
    let gui = nested.window(0);
    gui.wait_idle();
    let tooltip = wait_for_labels(&mut nested, "timeline-tooltip", "Take 3 · c1b6");
    assert!(
        tooltip.contains(&"In 00:00:40:22 · Out 00:00:45:27 · 00:00:05:05".to_owned()),
        "{tooltip:?}"
    );
    assert_eq!(
        local(&mut nested)["drag"],
        Value::Null,
        "hovering isn't dragging"
    );
    let toolbar = nested.window(0).labels("timeline-toolbar");
    assert!(toolbar.contains(&"Fit".to_owned()), "{toolbar:?}");
    assert!(
        toolbar.contains(&"5".to_owned()),
        "the bin's count: {toolbar:?}"
    );
    // The toggles' count chip and key caps sit centered in the 36 px
    // toolbar at their 16 px (S02, S07), like the labeled buttons' caps:
    // they used to fill the 28 px buttons.
    let parts = nested
        .window(0)
        .regions(&["tl-bin-count", "tl-cap-bin", "tl-cap-snap"]);
    let bar = parts["timeline-toolbar"];
    for name in ["tl-bin-count", "tl-cap-bin", "tl-cap-snap"] {
        let [_, y, _, h] = parts
            .get(name)
            .copied()
            .unwrap_or_else(|| panic!("no {name}: {parts:?}"));
        let off = (y + h / 2.0) - (bar[1] + bar[3] / 2.0);
        assert!(
            (h - 16.0).abs() <= 1.0 && off.abs() <= 1.0,
            "{name}: y {y}, h {h} (centered 16 px tall in {bar:?})"
        );
    }
    shoot(&nested, "fit", Some("S07-timeline-fit.png"));
    assert_clean(&mut nested);
}

/// Polls the labels under `name` until one equals `want`.
fn wait_for_labels(nested: &mut Nested, name: &str, want: &str) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let labels = nested.window(0).labels(name);
        if labels.iter().any(|l| l == want) {
            return labels;
        }
        assert!(
            Instant::now() < deadline,
            "no {want:?} under {name}: {labels:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// The clips as `qrec clip list` has them: (id, in, out).
fn clip_rows(nested: &Nested, project: &Path) -> Vec<(String, u64, u64)> {
    let list = nested.cli_json(project, &["clip", "list"]);
    let rows = list["data"]
        .as_array()
        .or_else(|| list["data"]["clips"].as_array())
        .unwrap_or_else(|| panic!("clip list: {list}"));
    rows.iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap_or_default().to_owned(),
                c["in"].as_u64().unwrap_or_default(),
                c["out"].as_u64().unwrap_or_default(),
            )
        })
        .collect()
}

/// A left-button drag from (`x0`, `y`) to (`x1`, `y`) in `steps`, left
/// pressed (the case captures, then cancels with `Esc` and releases).
fn press_and_drag(nested: &mut Nested, (x0, y): (f64, f64), x1: f64, steps: u32) {
    let pointer = nested.pointer();
    pointer.move_to(x0, y);
    pointer.button(gui_support::pointer::BUTTON_LEFT, true);
    for i in 1..=steps {
        let t = f64::from(i) / f64::from(steps);
        pointer.move_to(x0 + (x1 - x0) * t, y);
        thread::sleep(Duration::from_millis(16));
    }
}

fn release(nested: &mut Nested) {
    nested
        .pointer()
        .button(gui_support::pointer::BUTTON_LEFT, false);
}

/// Polls `local` until its drag is `want` (the pointer and the test channel
/// are two sockets).
fn wait_drag(nested: &mut Nested, want: Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let local = local(nested);
        if local["drag"] == want {
            return local;
        }
        assert!(Instant::now() < deadline, "drag never {want}: {local}");
        thread::sleep(Duration::from_millis(50));
    }
}

// ------------------------------------------------------------- S08, S09

/// S08: 48 px/s from 00:01:05:00 with c2d0's silences (from `qrec autocut
/// --dry-run`'s cache) hatched on A1.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s08_seconds() {
    let mut nested = Nested::start(AREA, "s08", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        &program(&[], 2437, Some((1.6, 1950))),
    );
    let dry = nested.cli_json(&project, &["autocut", "--clips", "c2d0", "--dry-run"]);
    assert_eq!(dry["ok"], true, "{dry}");
    open(&mut nested, &project, Launch::default());
    check_regions(&mut nested, &["lane-A2"]);
    let toolbar = nested.window(0).labels("timeline-toolbar");
    assert!(toolbar.contains(&"48 px/s".to_owned()), "{toolbar:?}");
    shoot(&nested, "seconds", Some("S08-timeline-seconds.png"));
    assert_clean(&mut nested);
}

/// S09: 32 px/frame from frame 3160, c2d1 selected, the playhead at the
/// c2d0 | c2d1 cut (x 688).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s09_frame_level() {
    frame_level("s09", Options::default());
}

/// S09 at `QREC_TEST_SCALE=2` (G§8.4 scale runs): the same regions, the
/// thumbnails at 144 px.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s09_frame_level_at_scale_2() {
    frame_level("s09-scale-2", Options::at_scale(1600, 1000, 2.0));
}

/// S09 at `QREC_TEST_SCALE=1.5`: GTK draws at the whole scale factor 2.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s09_frame_level_at_scale_1_5() {
    frame_level("s09-scale-1.5", Options::at_scale(1600, 1000, 1.5));
}

fn frame_level(case: &str, opts: Options) {
    let mut nested = Nested::start(AREA, case, opts);
    let project = fixture(
        &nested,
        DesignVariant::Base,
        &program(&["c2d1"], 3176, Some((32.0, 3160))),
    );
    open(&mut nested, &project, Launch::a11y());
    check_regions(&mut nested, &["lane-A2"]);
    assert_a11y(&mut nested);
    let toolbar = nested.window(0).labels("timeline-toolbar");
    assert!(toolbar.contains(&"32 px/frame".to_owned()), "{toolbar:?}");
    let corner = nested.window(0).labels("ruler");
    assert!(corner.contains(&"00:01:45:26".to_owned()), "{corner:?}");
    // The frames (and strips) are 72 px × the window's scale factor high:
    // 144 at 2 and at 1.5 (G§3.11, G§8.4 scale runs).
    let want = 72 * gui_support::scale_factor(nested.opts.scale);
    let heights = gui_support::poll(Duration::from_secs(20), "the frame images", || {
        nested.window(0).wait_idle();
        let heights = gui_support::preview_heights(&project);
        (!heights.is_empty()).then_some(heights)
    });
    assert_eq!(
        heights,
        std::collections::BTreeSet::from([want]),
        "frame images {want} px high"
    );
    shoot(&nested, "frame-level", Some("S09-timeline-frame-level.png"));
    assert_clean(&mut nested);
}

// ------------------------------------------------------------- S10, S11

/// S10: c2d0's start edge dragged +46.4 px (29 frames) at 48 px/s: the
/// trim preview, `LocalUi.drag` trimStart; `Esc` cancels and nothing
/// changes; released, one trim (one undo).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s10_trimming() {
    trimming("s10", Options::default());
}

/// S10's scripted trim at `QREC_TEST_SCALE=2` (G§8.4 scale runs): the same
/// logical drag hits the same edge and trims the same 29 frames.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s10_trimming_at_scale_2() {
    trimming("s10-scale-2", Options::at_scale(1600, 1000, 2.0));
}

/// S10's scripted trim at `QREC_TEST_SCALE=1.5`.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s10_trimming_at_scale_1_5() {
    trimming("s10-scale-1.5", Options::at_scale(1600, 1000, 1.5));
}

fn trimming(case: &str, opts: Options) {
    let mut nested = Nested::start(AREA, case, opts);
    let project = fixture(
        &nested,
        DesignVariant::Base,
        &program(&["c2d0"], 2437, Some((1.6, 1950))),
    );
    let dry = nested.cli_json(&project, &["autocut", "--clips", "c2d0", "--dry-run"]);
    assert_eq!(dry["ok"], true, "{dry}");
    open(&mut nested, &project, Launch::default());
    let before = clip_rows(&nested, &project);
    // c2d0 starts at window x 390.4.
    press_and_drag(&mut nested, (391.0, VIDEO_Y), 437.4, 8);
    wait_drag(&mut nested, json!("trimStart"));
    nested.window(0).wait_idle();
    let tip = wait_for_labels(&mut nested, "timeline-tooltip", "00:00:01:11");
    assert!(tip.contains(&"In".to_owned()), "{tip:?}");
    shoot(&nested, "trimming", Some("S10-trimming.png"));
    nested.keys(&["-k", "Escape"]);
    wait_drag(&mut nested, Value::Null);
    release(&mut nested);
    nested.window(0).wait_idle();
    assert_eq!(clip_rows(&nested, &project), before, "Esc sends nothing");
    // The same drag released trims (one command, one undo).
    press_and_drag(&mut nested, (391.0, VIDEO_Y), 437.4, 8);
    wait_drag(&mut nested, json!("trimStart"));
    release(&mut nested);
    wait_drag(&mut nested, Value::Null);
    nested.window(0).wait_idle();
    let after = clip_rows(&nested, &project);
    let c2d0 = after.iter().find(|c| c.0 == "c2d0").unwrap();
    assert_eq!((c2d0.1, c2d0.2), (41, 1104), "{after:?}");
    let undo = nested.cli_json(&project, &["undo"]);
    assert_eq!(undo["ok"], true, "{undo}");
    assert_eq!(clip_rows(&nested, &project), before);
    assert_clean(&mut nested);
}

/// S11: c8a1 dragged after c1b6 at fit: the caret, the lifted copy, "Move
/// after c1b6 · slot 7 of 10"; `Esc` cancels; released, it moves.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s11_drag_move() {
    let mut nested = Nested::start(AREA, "s11", Options::default());
    let project = fixture(&nested, DesignVariant::Base, &program(&[], 2437, None));
    open(&mut nested, &project, Launch::default());
    let before = clip_rows(&nested, &project);
    // c8a1 is at window x 233.2 → 297.1 at fit.
    press_and_drag(&mut nested, (265.0, VIDEO_Y), 405.0, 10);
    wait_drag(&mut nested, json!("move"));
    nested.window(0).wait_idle();
    wait_for_labels(
        &mut nested,
        "timeline-tooltip",
        "Move after c1b6 · slot 7 of 10",
    );
    shoot(&nested, "drag-move", Some("S11-drag-move.png"));
    nested.keys(&["-k", "Escape"]);
    wait_drag(&mut nested, Value::Null);
    release(&mut nested);
    nested.window(0).wait_idle();
    assert_eq!(clip_rows(&nested, &project), before, "Esc sends nothing");
    press_and_drag(&mut nested, (265.0, VIDEO_Y), 405.0, 10);
    wait_drag(&mut nested, json!("move"));
    release(&mut nested);
    wait_drag(&mut nested, Value::Null);
    nested.window(0).wait_idle();
    let order: Vec<String> = clip_rows(&nested, &project)
        .into_iter()
        .map(|c| c.0)
        .collect();
    assert_eq!(
        order,
        [
            "c7f3", "c1b2", "c1b3", "c1b4", "c1b5", "c1b6", "c8a1", "c2d0", "c2d1", "c3e9"
        ]
    );
    assert_clean(&mut nested);
}

// ------------------------------------------------------------- S12

/// The center of widget `name` (it must show).
fn center_of(nested: &mut Nested, name: &str) -> (f64, f64) {
    let regions = nested.window(0).regions(&[name]);
    let [x, y, w, h] = *regions
        .get(name)
        .unwrap_or_else(|| panic!("no {name}: {regions:#?}"));
    (x + w / 2.0, y + h / 2.0)
}

/// S12's timeline: `z` with c3e9 selected fills the time area with it
/// (0.335 px/frame, "10 px/s"); the `SetUi` follows 300 ms later. The
/// toolbar's settings icon (`autocut.preview`) then opens WP5's autocut
/// page, whose preview (`LocalUi.autocut_preview`) the canvas draws: each
/// region hatched in `warning` on A1, its tag 22 px above the lane, the
/// same range dimmed on A2. The icon again closes the page: the preview
/// goes (the A2 lane isn't dimmed any more).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s12_zoom_to_c3e9() {
    let mut nested = Nested::start(AREA, "s12", Options::default());
    let project = fixture(
        &nested,
        DesignVariant::Base,
        &program(&["c3e9"], 4842, None),
    );
    open(&mut nested, &project, Launch::default());
    nested.keys(&["-k", "z"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let toolbar = nested.window(0).labels("timeline-toolbar");
        if toolbar.contains(&"10 px/s".to_owned()) {
            break;
        }
        assert!(Instant::now() < deadline, "no zoom to c3e9: {toolbar:?}");
        thread::sleep(Duration::from_millis(100));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = nested.cli_json(&project, &["status"]);
        let zoom = status["data"]["ui"]["zoom"].as_f64().unwrap_or_default();
        if (zoom - 1040.0 / 3105.0).abs() < 1e-6 {
            break;
        }
        assert!(Instant::now() < deadline, "SetUi never came: {status}");
        thread::sleep(Duration::from_millis(100));
    }
    nested.window(0).wait_idle();
    check_regions(&mut nested, &["lane-A2"]);
    shoot(&nested, "zoom-c3e9", None);

    // The settings icon opens the autocut page; its preview is drawn.
    let (x, y) = center_of(&mut nested, "tl-autocut-settings");
    nested.pointer().click(x, y);
    wait_for_labels(&mut nested, "region-panel", "Removes 4 silences");
    nested.window(0).wait_idle();
    let open_local = local(&mut nested);
    assert_eq!(open_local["panelPage"], "autocut", "{open_local}");
    let spans = open_local["autocutPreview"]["spans"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(spans.len(), 4, "{open_local}");
    let ui = nested.cli_json(&project, &["status"])["data"]["ui"].clone();
    let zoom = ui["zoom"].as_f64().expect("a zoom");
    let scroll = ui["scrollPx"].as_f64().unwrap_or_default();
    // Window x of a timeline frame (the time area starts at 176).
    let x_of = |f: &Value| 176.0 + f.as_f64().expect("a frame") * zoom - scroll;
    let xs: Vec<(f64, f64)> = spans
        .iter()
        .map(|s| (x_of(&s["from"]), x_of(&s["to"])))
        .collect();
    let shot = shoot(&nested, "page", Some("S12-autocut-preview.png"));
    let s = nested.opts.scale;
    let px = |v: f64| (v * s).round().max(1.0) as u32;
    let warning = gui_support::token_rgb("swamp-night", "warning");
    // A2 (860–916) over `range` (x0, width), and just left of it.
    let a2 = |shot: &Path, x0: f64, w: f64| {
        let inside = gui_support::mean_grey(shot, [px(x0 + 2.0), px(864.0), px(w - 4.0), px(48.0)]);
        let outside = gui_support::mean_grey(shot, [px(x0 - 14.0), px(864.0), px(8.0), px(48.0)]);
        (inside, outside)
    };
    for (span, &(x0, x1)) in spans.iter().zip(&xs) {
        let w = x1 - x0;
        // The hatch on A1 (800–856), inside the edges.
        let hatch = gui_support::count_near(
            &shot,
            [px(x0 + 3.0), px(804.0), px(w - 6.0), px(48.0)],
            warning,
            40,
        );
        assert!(hatch > 20, "no hatch over {span}: {hatch} px");
        // The tag ("−1.6 s", `warning-text` on a plate) 22 px above A1.
        let tag = gui_support::count_near(
            &shot,
            [px(x0 + 2.0), px(778.0), px(30.0), px(18.0)],
            warning,
            60,
        );
        assert!(tag > 3, "no tag over {span}: {tag} px");
        let (inside, outside) = a2(&shot, x0, w);
        assert!(
            inside + 3.0 < outside,
            "A2 not dimmed over {span}: {inside:.1} vs {outside:.1}"
        );
    }
    // Nothing between the first two regions.
    let (a, b) = (xs[0].1, xs[1].0);
    let between = gui_support::count_near(
        &shot,
        [px(a + 10.0), px(804.0), px(b - a - 20.0), px(48.0)],
        warning,
        40,
    );
    assert_eq!(between, 0, "a hatch between the regions");

    // The icon again: the page closes and the preview goes.
    let (x, y) = center_of(&mut nested, "tl-autocut-settings");
    nested.pointer().click(x, y);
    wait_for_labels(&mut nested, "region-panel", "Onion layers");
    nested.window(0).wait_idle();
    let closed_local = local(&mut nested);
    assert_eq!(closed_local["panelPage"], "layers", "{closed_local}");
    assert_eq!(
        closed_local["autocutPreview"],
        Value::Null,
        "{closed_local}"
    );
    let shot = shoot(&nested, "page-closed", None);
    let (x0, x1) = xs[0];
    let (inside, outside) = a2(&shot, x0, x1 - x0);
    assert!(
        (inside - outside).abs() < 3.0,
        "A2 still dimmed: {inside:.1} vs {outside:.1}"
    );
    assert_clean(&mut nested);
}

// ------------------------------------------------------------- ghosts

/// S04: recording with the clock at 00:02:41:12: fit includes the ghost,
/// the clips at 0.6.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s04_recording_ghost() {
    let mut nested = Nested::start(AREA, "s04", Options::default());
    let project = fixture(&nested, DesignVariant::Base, &UiState::default());
    open(
        &mut nested,
        &project,
        Launch::default().with_env("QREC_FAKE_RECORD_OFFSET_FRAMES", "4842"),
    );
    let start = nested.cli_json(&project, &["record", "start"]);
    assert_eq!(start["ok"], true, "{start}");
    wait_record_phase(&nested, &project, "recording");
    nested.window(0).wait_idle();
    check_regions(&mut nested, &["lane-A2"]);
    shoot(&nested, "recording", Some("S04-recording.png"));
    let stop = nested.cli_json(&project, &["record", "stop"]);
    assert_eq!(stop["ok"], true, "{stop}");
    assert_clean(&mut nested);
}

/// S04b: `o` with c2d0 selected records over it: the ghost in its slot,
/// c2d0 hatched.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s04b_recording_over() {
    let mut nested = Nested::start(AREA, "s04b", Options::default());
    let ui = UiState {
        selection: vec![id("c2d0")],
        ..UiState::default()
    };
    let project = fixture(&nested, DesignVariant::Base, &ui);
    open(
        &mut nested,
        &project,
        Launch::default().with_env("QREC_FAKE_RECORD_OFFSET_FRAMES", "932"),
    );
    nested.keys(&["-k", "o"]);
    wait_record_phase(&nested, &project, "recording");
    nested.window(0).wait_idle();
    shoot(
        &nested,
        "recording-over",
        Some("S04b-recording-over-c2d0.png"),
    );
    let stop = nested.cli_json(&project, &["record", "stop"]);
    assert_eq!(stop["ok"], true, "{stop}");
    assert_clean(&mut nested);
}

/// S05: finalizing (the files take 8 s to close): the saving ghost.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s05_finalizing_ghost() {
    let mut nested = Nested::start(AREA, "s05", Options::default());
    let project = fixture(&nested, DesignVariant::Base, &UiState::default());
    open(
        &mut nested,
        &project,
        Launch::default()
            .with_env("QREC_FAKE_RECORD_OFFSET_FRAMES", "4842")
            .with_env("QREC_FAKE_EOS_DELAY_MS", "8000"),
    );
    let start = nested.cli_json(&project, &["record", "start"]);
    assert_eq!(start["ok"], true, "{start}");
    wait_record_phase(&nested, &project, "recording");
    let mut stop = nested
        .env
        .command(&gui_support::qrec_bin())
        .arg("--project")
        .arg(&project)
        .args(["record", "stop", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("qrec record stop");
    wait_record_phase(&nested, &project, "finalizing");
    nested.window(0).wait_idle();
    shoot(&nested, "finalizing", Some("S05-finalizing.png"));
    let deadline = Instant::now() + Duration::from_secs(60);
    while stop.try_wait().ok().flatten().is_none() {
        assert!(Instant::now() < deadline, "record stop never returned");
        thread::sleep(Duration::from_millis(100));
    }
    assert_clean(&mut nested);
}

/// S05b: take 10 landed (11 clips, 12,233 frames at fit), the playhead at
/// the end.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s05b_take_lands() {
    let mut nested = Nested::start(AREA, "s05b", Options::default());
    let ui = UiState {
        playhead: 12_233,
        ..UiState::default()
    };
    let project = fixture(&nested, DesignVariant::Take10Landed, &ui);
    open(&mut nested, &project, Launch::default());
    check_regions(&mut nested, &["lane-A2"]);
    let toolbar = nested.window(0).labels("timeline-toolbar");
    assert!(
        toolbar.contains(&"11 clips · ".to_owned()) && toolbar.contains(&"00:06:47:23".to_owned()),
        "{toolbar:?}"
    );
    shoot(&nested, "take-lands", Some("S05b-take-lands.png"));
    assert_clean(&mut nested);
}

// ------------------------------------------------------------- S01b

/// S01b: sources added, no takes: the toolbar's Empty variant, the headers
/// and the empty state ("This swamp is quiet."). Stub-based: the first run
/// (WP4) isn't there, so the case adds the sources through the test
/// channel's `dispatch`, in D-16's order.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s01b_empty_timeline() {
    let mut nested = Nested::start(AREA, "s01b", Options::default());
    let project = nested.env.empty_dir("rust-traits-talk");
    open(&mut nested, &project, Launch::a11y());
    for (i, command) in [
        json!({"cmd": "layerAdd", "target": "project", "source": {"screen": {"output": "FAKE-1"}}, "name": "Main screen"}),
        json!({"cmd": "layerAdd", "target": "project", "source": {"camera": {"device": "/dev/fake0"}}, "name": "Face"}),
        json!({"cmd": "audioAdd", "target": "project", "role": "mic", "source": "alsa_input.fake", "label": "Yeti Nano"}),
        json!({"cmd": "audioAdd", "target": "project", "role": "system", "source": "alsa_output.fake.monitor", "label": "System audio"}),
    ]
    .into_iter()
    .enumerate()
    {
        let reply = nested
            .window(0)
            .ctl(json!({"op": "dispatch", "command": command}));
        assert_eq!(reply["ok"], true, "{reply}");
        nested.window(0).wait_idle();
        // Each source rebuilds the live pipeline. The audio tracks' keep
        // both video sources' states and sizes (no new status): waitIdle
        // waits for the engine's progress, or the picture is the new
        // pipeline's black (G§3.18).
        gui_support::assert_live_picture_drawn(&nested.screenshot(&format!("step-{}", i + 1)));
    }
    let gui = nested.window(0);
    let regions = gui.regions(&[]);
    assert!(!regions.contains_key("ruler"), "no ruler: {regions:?}");
    assert!(!regions.contains_key("video-row"), "{regions:?}");
    assert_regions(
        &regions,
        &[
            ("region-timeline", [0.0, 656.0, 1216.0, 312.0]),
            ("timeline-toolbar", [0.0, 656.0, 1216.0, 36.0]),
            ("track-headers", [0.0, 692.0, 176.0, 276.0]),
            ("time-area", [176.0, 692.0, 1040.0, 276.0]),
        ],
        &[
            "region-timeline",
            "timeline-toolbar",
            "track-headers",
            "time-area",
        ],
        1.0,
    );
    let labels = gui.labels("region-timeline");
    for want in [
        "This swamp is quiet.",
        "Press",
        "and make some noise.",
        "No clips yet",
        "Thumbnails: Face (L2)",
        "Yeti Nano",
        "System audio",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert_a11y(&mut nested);
    let shot = shoot(&nested, "empty", Some("S01b-first-run-sources-added.png"));
    gui_support::assert_live_picture_drawn(&shot);
    assert_clean(&mut nested);
}

// ------------------------------------------------------------- ui.scale

/// `ui.scale = scale` in the user's `config.toml` (the interface scale,
/// D-46; the scale runs' `QREC_TEST_SCALE` is the output's).
fn ui_scale(nested: &Nested, scale: f64) {
    let config = nested.env.config_home().join("qrec");
    fs::create_dir_all(&config).expect("the config folder");
    fs::write(
        config.join("config.toml"),
        format!("[ui]\nscale = {scale}\n"),
    )
    .expect("config.toml");
}

/// At `ui.scale` 1.5 the track headers line up with their rows: each
/// header's top and center are its row's (±1 px). They asked for their
/// 100 % heights (A1's header 40 px above its lane, A2's 51 px).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn headers_line_up_at_ui_scale_1_5() {
    headers_line_up("headers-ui-1.5", 1.5, Options::default());
}

/// The same at 1.25 in the compact rows.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn headers_line_up_at_ui_scale_1_25_compact() {
    headers_line_up("headers-ui-1.25-compact", 1.25, Options::size(1600, 800));
}

/// The same at 1.5 in the compact rows.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn headers_line_up_at_ui_scale_1_5_compact() {
    headers_line_up("headers-ui-1.5-compact", 1.5, Options::size(1600, 800));
}

fn headers_line_up(case: &str, scale: f64, opts: Options) {
    let mut nested = Nested::start(AREA, case, opts);
    ui_scale(&nested, scale);
    let project = fixture(&nested, DesignVariant::Base, &program(&[], 2437, None));
    open(&mut nested, &project, Launch::default());
    assert_eq!(local(&mut nested)["scale"], scale);
    let pairs = [
        ("header-video", "video-row"),
        ("header-A1", "lane-A1"),
        ("header-A2", "lane-A2"),
    ];
    let names: Vec<&str> = pairs.iter().flat_map(|(h, r)| [*h, *r]).collect();
    let regions = nested.window(0).regions(&names);
    for (header, row) in pairs {
        let (Some(h), Some(r)) = (regions.get(header), regions.get(row)) else {
            panic!("no {header} or {row}: {regions:#?}");
        };
        // On its row: its top and center within 1 px. A compact lane
        // header's content is 1.5 px taller than its 60 px row at 125 %
        // (its 18 px line rounds up), centered on it.
        let center = |v: &[f64; 4]| v[1] + v[3] / 2.0;
        assert!(
            (h[1] - r[1]).abs() <= 1.0
                && (center(h) - center(r)).abs() <= 1.0
                && (h[3] - r[3]).abs() <= 2.0,
            "{header} {h:?} beside {row} {r:?}"
        );
    }
    shoot(&nested, "headers", None);
    assert_clean(&mut nested);
}

// ------------------------------------------------------------- compact

/// G§3.15 at 1600×800: the compact rows (toolbar 36, ruler 28, video 72,
/// lanes 48: 232 px).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn compact_timeline_at_1600x800() {
    let mut nested = Nested::start(AREA, "compact", Options::size(1600, 800));
    let project = fixture(&nested, DesignVariant::Base, &program(&[], 2437, None));
    open(&mut nested, &project, Launch::default());
    let regions = nested.window(0).regions(&[]);
    let parts = [
        ("region-timeline", [0.0, 536.0, 1216.0, 232.0]),
        ("timeline-toolbar", [0.0, 536.0, 1216.0, 36.0]),
        ("ruler", [0.0, 572.0, 1216.0, 28.0]),
        ("track-headers", [0.0, 600.0, 176.0, 168.0]),
        ("time-area", [176.0, 600.0, 1040.0, 168.0]),
        ("video-row", [176.0, 600.0, 1040.0, 72.0]),
        ("lane-A1", [176.0, 672.0, 1040.0, 48.0]),
        ("lane-A2", [176.0, 720.0, 1040.0, 48.0]),
    ];
    let names: Vec<&str> = parts.iter().map(|(n, _)| *n).collect();
    assert_regions(&regions, &parts, &names, 1.0);
    shoot(&nested, "compact", None);
    assert_clean(&mut nested);
}

// ------------------------------------------------------------- the probe

/// G§8.6: frame-clock intervals while the timeline zooms and scrolls the
/// 300-clip fixture (its scripted probe, `QREC_GUI_PROBE=timeline`), twice:
/// p99 < 16.7 ms is asserted on the warm pass under `QREC_TEST_WLR=gles2`;
/// both passes are recorded, under pixman too (where the compositor and the
/// window share the CPU).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn frame_times_on_300_clips() {
    let mut nested = Nested::start(AREA, "perf-300", Options::default());
    let project = nested.env.empty_dir("perf");
    perf_fixture(&project, 300);
    write_ui(&project, &program(&[], 0, None));
    open(
        &mut nested,
        &project,
        Launch::default().with_env("QREC_GUI_PROBE", "timeline"),
    );
    // The first pass compiles GSK's shaders (the test HOME's Mesa cache
    // starts empty: 35–80 ms frames, a handful); the second measures.
    let cold = probe(&mut nested, 1);
    let line = probe(&mut nested, 2);
    let number = |key: &str| -> f64 {
        line.split(key)
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .unwrap_or(f64::NAN)
    };
    let (p50, p99) = (number("p50"), number("p99"));
    let renderer = std::env::var("QREC_TEST_WLR").unwrap_or_else(|_| "pixman".to_owned());
    let record = nested
        .shots
        .join(format!("perf-300-frame-times-{renderer}.txt"));
    fs::write(
        &record,
        format!("{renderer} cold: {cold}\n{renderer} warm: {line}\n"),
    )
    .expect("write the record");
    eprintln!("frame times ({renderer}): p50 {p50} ms, p99 {p99} ms");
    if renderer == "gles2" {
        assert!(p99 < 16.7, "p99 {p99} ms ≥ 16.7 ms: {line}");
    }
    shoot(&nested, "perf", None);
    assert_clean(&mut nested);
}

/// Runs the timeline's probe and returns its `n`-th result line (60 s at
/// most).
fn probe(nested: &mut Nested, n: usize) -> String {
    let reply = nested
        .window(0)
        .ctl(json!({"op": "action", "id": "timeline.probe"}));
    assert_eq!(reply["ok"], true, "{reply}");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let log = nested.window(0).gui_log();
        if let Some(line) = log
            .lines()
            .filter(|l| l.contains("timeline probe:") && l.contains("p99"))
            .nth(n - 1)
        {
            return line.to_owned();
        }
        assert!(Instant::now() < deadline, "no probe result {n}");
        thread::sleep(Duration::from_millis(200));
    }
}
