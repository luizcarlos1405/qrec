//! WP7's cases (docs/GUI.md G§7 WP7, G§8.4) in a nested headless sway: the
//! S14 bin with its selection, keys, preview and drag; the export panel's
//! setup (S15a, S15c), running (S15b, passive), done (S16), failed (S17);
//! the S16b toast; a CLI export in the panel; the overwrite modal and a
//! refused path (D-24); the S18 purge from the bin. Each checks its
//! regions, labels and hints, the a11y walk and no panic, and leaves a
//! `-compare.png` next to its approved frame for the eye.
//!
//! `cargo test -p qrec --test gui_bin_export -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon; pixman, or GL
//! with `QREC_TEST_WLR=gles2`). Screenshots and logs go to
//! `tmp/gui-shots/wp7/`. "Change…" never opens a portal here: the window
//! takes its paths from `QREC_GUI_TEST_SAVE_AS` (G§9 R7).

#[path = "support/fail_ffmpeg.rs"]
mod fail_ffmpeg;
mod gui_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use gui_support::{
    BRIEF_REGIONS, CLI_TIMEOUT, Launch, MERGED_REGIONS, Nested, Options, TOOL_TIMEOUT,
    assert_regions, reap_within, wait_for_gui_host,
};
use qrec_core::state::{CanvasMode, UiState};
use qrec_core::{ClipId, LayerId};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use qrec_shell::testkit::run_tool;
use serde_json::{Value, json};

const OPEN: Duration = Duration::from_secs(60);
const EXIT: Duration = Duration::from_secs(20);
const AREA: &str = "wp7";
/// S15a's local now (G§8.4): the auto name reads BRIEF's.
const NOW: &str = "2026-10-14T15:30:12";

// ---------------------------------------------------------------- helpers

fn fixture(nested: &Nested, name: &str, variant: DesignVariant, ui: Option<UiState>) -> PathBuf {
    let dir = nested.env.empty_dir(name);
    design_fixture(&dir, variant);
    if let Some(ui) = ui {
        write_ui(&dir, &ui);
    }
    dir
}

/// S06's UI: Program, c2d0 selected, the playhead at 2437, Face (L2)
/// selected (the inspector the frames draw under the panel, as
/// gui_panel's S06).
fn program_ui() -> UiState {
    UiState {
        canvas_mode: CanvasMode::Program,
        selection: vec![ClipId::from_raw("c2d0")],
        playhead: 2437,
        selected_layer: Some(LayerId::numbered(2)),
        ..UiState::default()
    }
}

/// S14's: S06 with the bin open.
fn bin_ui() -> UiState {
    UiState {
        bin_open: true,
        ..program_ui()
    }
}

/// `ui.reduced_motion = true` in the user's `config.toml`.
fn reduced_motion(nested: &Nested) {
    let config = nested.env.config_home().join("qrec");
    fs::create_dir_all(&config).expect("the config folder");
    fs::write(config.join("config.toml"), "[ui]\nreduced_motion = true\n").expect("config.toml");
}

fn open(nested: &mut Nested, project: &Path, launch: Launch) {
    nested.launch(project, launch);
    wait_for_gui_host(nested, project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
}

/// BRIEF's meter readings, pinned (G§8.4 `QREC_GUI_TEST_LEVELS`, as
/// gui_panel's): A1 at −22/−16 dBFS, A2 at −52/−48.
const DESIGN_LEVELS: &str = "alsa_input.fake=-22/-16,alsa_output.fake.monitor=-52/-48";

/// The launch every case uses: the a11y walk on, S15a's now, the meters
/// pinned.
fn launch() -> Launch {
    Launch::a11y()
        .with_env("QREC_GUI_TEST_NOW", NOW)
        .with_env("QREC_GUI_TEST_LEVELS", DESIGN_LEVELS)
}

/// The shot next to the approved frame (G§8.4 check 4), whole or both
/// cropped to `crop` (`w:h:x:y`).
fn compare(shot: &Path, frame: &str, crop: Option<&str>, suffix: &str) {
    let png = gui_support::repo().join("design/screens").join(frame);
    if !png.exists() {
        return;
    }
    let out = shot.with_file_name(format!(
        "{}-compare{suffix}.png",
        shot.file_stem().unwrap_or_default().to_string_lossy()
    ));
    let filter = match crop {
        Some(c) => format!("[0]crop={c}[a];[1]crop={c}[b];[a][b]hstack=inputs=2"),
        None => "[0]scale=1600:1000[a];[1]scale=1600:1000[b];[a][b]hstack=inputs=2".to_owned(),
    };
    let run = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(shot)
            .arg("-i")
            .arg(&png)
            .args(["-filter_complex", &filter])
            .arg(&out),
        TOOL_TIMEOUT,
    );
    assert!(
        run.status.success(),
        "the comparison of {}: {}",
        shot.display(),
        run.stderr
    );
}

/// The hint bar's items as BRIEF writes them (`[Ctrl][K] Commands`).
fn hints(nested: &mut Nested) -> Vec<String> {
    let reply = nested.window(0).ctl(json!({"op": "hints"}));
    reply["items"]
        .as_array()
        .unwrap_or_else(|| panic!("hints: {reply}"))
        .iter()
        .map(|item| {
            let keys: Vec<String> = item["keys"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|chord| {
                    chord
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|cap| format!("[{}]", cap.as_str().unwrap_or("?")))
                        .collect()
                })
                .collect();
            let label = item["label"].as_str().unwrap_or("?");
            let mut text = if keys.is_empty() {
                label.to_owned()
            } else {
                format!("{} {label}", keys.join(" "))
            };
            if item["enabled"] == false {
                text.push_str(" (disabled)");
            }
            text
        })
        .collect()
}

/// The a11y walk is empty and no panic was caught.
fn assert_clean(nested: &mut Nested) {
    let gui = nested.window(0);
    let reply = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(reply["ok"], true, "a11y: {reply}");
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
}

fn close(nested: &mut Nested) {
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// `labels` has each of `texts`.
fn assert_shows(labels: &[String], texts: &[&str]) {
    for text in texts {
        assert!(labels.iter().any(|l| l == text), "{text:?} in {labels:?}");
    }
}

/// S18's footer: "2 takes selected", their size (the fixture's files are
/// smaller than BRIEF's: G§8.4 known differences), "Purge from disk".
fn assert_footer(labels: &[String]) {
    assert_shows(labels, &["2 takes selected", "Purge from disk"]);
    assert!(
        labels
            .iter()
            .any(|l| l.ends_with(" MB") || l.ends_with(" KB")),
        "the size: {labels:?}"
    );
}

/// The center of the widget named `name`.
fn center(nested: &mut Nested, name: &str) -> (f64, f64) {
    let regions = nested.window(0).regions(&[name]);
    let [x, y, w, h] = *regions
        .get(name)
        .unwrap_or_else(|| panic!("no {name}: {regions:?}"));
    (x + w / 2.0, y + h / 2.0)
}

/// The x of `clip`'s middle in a video row `width` wide at fit zoom, from
/// the clip list (`start`, `duration`).
fn clip_middle(nested: &Nested, project: &Path, clip: &str, width: f64) -> f64 {
    let clips = nested.cli_json(project, &["clip", "list"])["data"].clone();
    let clips = clips.as_array().cloned().unwrap_or_default();
    let frames = |c: &Value, key: &str| c[key].as_u64().unwrap_or(0) as f64;
    let total = clips
        .iter()
        .map(|c| frames(c, "start") + frames(c, "duration"))
        .fold(0.0, f64::max);
    let c = clips
        .iter()
        .find(|c| c["id"] == clip)
        .unwrap_or_else(|| panic!("no {clip}: {clips:?}"));
    let middle = frames(c, "start") + frames(c, "duration") / 2.0;
    middle * (width - qrec_core::view::timeline::FIT_INSET_PX) / total
}

/// A click, then `waitIdle`: the window's own export settles once its job
/// runs (G§3.18), so this returns while it renders.
fn click(nested: &mut Nested, name: &str) {
    press(nested, name);
    nested.window(0).wait_idle();
}

/// A click and nothing after it: S16b's `Esc` has to beat the short Smoke
/// export, so it doesn't wait for the job to start.
fn press(nested: &mut Nested, name: &str) {
    let (x, y) = center(nested, name);
    nested.pointer().click(x, y);
}

/// Waits until the labels under `name` have one `pred` likes, then for
/// the window to settle (`waitIdle`: drawn as the labels say; a running
/// export of its own doesn't hold it, G§3.18).
fn wait_label(
    nested: &mut Nested,
    name: &str,
    what: &str,
    within: Duration,
    pred: impl Fn(&str) -> bool,
) -> Vec<String> {
    let deadline = Instant::now() + within;
    loop {
        let labels = nested.window(0).labels(name);
        if labels.iter().any(|l| pred(l)) {
            nested.window(0).wait_idle();
            return nested.window(0).labels(name);
        }
        assert!(Instant::now() < deadline, "no {what} in {labels:?}");
        thread::sleep(Duration::from_millis(100));
    }
}

/// The `local` summary's overlays.
fn overlays(nested: &mut Nested) -> Vec<String> {
    let local = nested.window(0).local();
    local["overlays"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o.as_str().map(str::to_owned))
        .collect()
}

/// Waits until the overlays are `want`.
fn wait_overlays(nested: &mut Nested, want: &[&str], within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let now = overlays(nested);
        if now == want {
            return;
        }
        assert!(Instant::now() < deadline, "overlays {now:?}, want {want:?}");
        thread::sleep(Duration::from_millis(100));
    }
}

/// `qrec status --json`'s data.
fn status(nested: &Nested, project: &Path) -> Value {
    nested.cli_json(project, &["status"])["data"].clone()
}

/// The running export's percent (`qrec status`), if one runs.
fn export_percent(nested: &Nested, project: &Path) -> Option<u64> {
    let jobs = status(nested, project)["runtime"]["jobs"].clone();
    jobs.as_object()?
        .values()
        .chain(jobs.as_array().into_iter().flatten())
        .find(|j| j["kind"]["kind"] == "export" || j["kind"] == "export")
        .and_then(|j| {
            let k = &j["kind"];
            let total = k["totalFrames"].as_u64()?;
            let done: u64 = k["segDone"]
                .as_array()?
                .iter()
                .filter_map(Value::as_u64)
                .sum();
            (total > 0).then(|| 90 * done / total)
        })
}

/// Waits until an export runs at `percent` or more (`qrec status`).
fn wait_export_at(nested: &Nested, project: &Path, percent: u64, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        if export_percent(nested, project).is_some_and(|p| p >= percent) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the export never got to {percent}%: {}",
            status(nested, project)["runtime"]["jobs"]
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// Waits until no export runs.
fn wait_export_gone(nested: &Nested, project: &Path, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let jobs = status(nested, project)["runtime"]["jobs"].clone();
        let empty = jobs.as_object().is_none_or(|o| o.is_empty())
            && jobs.as_array().is_none_or(|a| a.is_empty());
        if empty {
            return;
        }
        assert!(Instant::now() < deadline, "the export still runs: {jobs}");
        thread::sleep(Duration::from_millis(200));
    }
}

fn exports(project: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(project.join("exports"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "mp4" || e == "webm"))
        .collect();
    out.sort();
    out
}

/// The takes `qrec take list` names.
fn takes(nested: &Nested, project: &Path) -> Vec<String> {
    let list = nested.cli_json(project, &["take", "list"]);
    list["data"]
        .as_array()
        .unwrap_or_else(|| panic!("take list: {list}"))
        .iter()
        .filter_map(|t| t["id"].as_str().map(str::to_owned))
        .collect()
}

/// Opens the export panel (`Ctrl+E`, S15a's step).
fn ctrl_e(nested: &mut Nested) {
    nested.keys(&["-M", "ctrl", "-k", "e", "-m", "ctrl"]);
    nested.window(0).wait_idle();
}

/// The export panel's body where BRIEF §2.8 puts it: 440 wide, 8 px from
/// the right edge, its top at y 52 (the arrow above it from y 46).
fn assert_panel_placed(nested: &mut Nested) -> [f64; 4] {
    let regions = nested.window(0).regions(&["export-body", "export-panel"]);
    let body = *regions
        .get("export-body")
        .unwrap_or_else(|| panic!("no export panel: {regions:?}"));
    assert_regions(
        &regions,
        &[
            ("export-body", [1152.0, 52.0, 440.0, body[3]]),
            ("export-panel", [1152.0, 46.0, 440.0, body[3] + 6.0]),
        ],
        &["export-body", "export-panel"],
        1.0,
    );
    body
}

// ---------------------------------------------------------------- the bin

/// S14: the bin open over S06; a click on take 4's row selects it (R1)
/// and reveals its actions; the stage is pushed to (368, 100, 832, 468);
/// the rows read BRIEF §3.4 with D-18's lines; the hints are the bin's.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s14_the_bin_drawer() {
    let mut nested = Nested::start(AREA, "s14", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(bin_ui()),
    );
    open(&mut nested, &project, launch());
    click(&mut nested, "bin-take-4");
    let gui = nested.window(0);
    let local = gui.local();
    assert_eq!(local["binSelection"], json!(["take-0004"]), "{local}");
    assert_eq!(local["focus"], "bin", "{local}");
    let regions = gui.regions(&["bin-preview", "bin-purge"]);
    assert_regions(
        &regions,
        &[
            ("region-bin", [0.0, 48.0, 352.0, 608.0]),
            ("region-stage", [352.0, 48.0, 864.0, 608.0]),
            ("preview", [368.0, 100.0, 832.0, 468.0]),
            ("transport", [368.0, 612.0, 832.0, 36.0]),
        ],
        &["region-bin", "preview", "transport"],
        1.0,
    );
    assert!(regions.contains_key("bin-preview"), "take 4's actions show");
    let labels = gui.labels("region-bin");
    assert_shows(
        &labels,
        &[
            "Bin",
            "Take 9",
            "Broken",
            "Files didn't close within 10 s.",
            "Open log",
            "Take 7",
            "Recovered",
            "Recovered after Qrec closed mid-take.",
            "Take 6",
            "Too short",
            "7 frames; takes need 10.",
            "Take 4",
            "Unused",
            "Replaced by take 5.",
            "Take 2",
            "Removed from the timeline.",
            "Preview",
            "Insert at end",
            "Insert after selection",
            "Purge from disk",
        ],
    );
    assert!(
        labels.iter().any(|l| l.starts_with("5 takes")),
        "the header counts the takes: {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("00:00:51:18 · 14:12 · ")),
        "take 4's mono line: {labels:?}"
    );
    assert_eq!(
        hints(&mut nested),
        [
            "[B] Close bin",
            "[↑] [↓] Select take",
            "[Enter] Insert at end",
            "[Space] Preview",
        ]
    );
    let shot = nested.screenshot("bin");
    compare(&shot, "S14-bin-drawer.png", None, "");
    compare(&shot, "S14-bin-drawer.png", Some("352:608:0:48"), "-bin");
    assert_clean(&mut nested);
    close(&mut nested);
}

/// S14 at `QREC_TEST_SCALE=2` (G§8.4 scale runs).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s14_the_bin_drawer_at_scale_2() {
    bin_at_scale("s14-scale-2", 2.0);
}

/// S14 at `QREC_TEST_SCALE=1.5`: GTK draws at the whole scale factor 2.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s14_the_bin_drawer_at_scale_1_5() {
    bin_at_scale("s14-scale-1.5", 1.5);
}

/// S14 at `scale`: the bin and the stage it pushes where BRIEF puts them
/// (logical px, ±1 px × the scale), take 4 selected with its actions, its
/// rows' lines, and the thumbnails' (and the timeline's) strip images 72 px
/// × the window's scale factor high.
fn bin_at_scale(case: &str, scale: f64) {
    let mut nested = Nested::start(AREA, case, Options::at_scale(1600, 1000, scale));
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(bin_ui()),
    );
    open(&mut nested, &project, launch());
    click(&mut nested, "bin-take-4");
    let gui = nested.window(0);
    assert_eq!(gui.local()["binSelection"], json!(["take-0004"]));
    let regions = gui.regions(&["bin-preview"]);
    assert_regions(
        &regions,
        &[
            ("region-bin", [0.0, 48.0, 352.0, 608.0]),
            ("region-stage", [352.0, 48.0, 864.0, 608.0]),
            ("preview", [368.0, 100.0, 832.0, 468.0]),
            ("transport", [368.0, 612.0, 832.0, 36.0]),
        ],
        &["region-bin", "preview", "transport"],
        scale,
    );
    assert!(regions.contains_key("bin-preview"), "take 4's actions show");
    assert_shows(
        &gui.labels("region-bin"),
        &["Take 9", "Take 4", "Replaced by take 5.", "Insert at end"],
    );
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
    let shot = nested.screenshot("bin");
    compare(&shot, "S14-bin-drawer.png", None, "");
    assert_clean(&mut nested);
    close(&mut nested);
}

/// D-37's keys and D-17's preview: `↓` moves the selection, `Shift+↓`
/// extends it (the S18 footer, no row actions); `Space` previews the
/// selected take in the Program picture with its TAKE badge and pauses
/// it; `Esc` ends it; `Enter` inserts the take at the end.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_bin_keys_select_preview_and_insert() {
    let mut nested = Nested::start(AREA, "bin-keys", Options::default());
    // No layer selected: the arrows are the bin's, never a nudge.
    let ui = UiState {
        canvas_mode: CanvasMode::Live,
        selected_layer: None,
        ..bin_ui()
    };
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, Some(ui));
    open(&mut nested, &project, launch());
    click(&mut nested, "bin-take-6");
    nested.keys(&["-k", "Down"]);
    nested.window(0).wait_idle();
    assert_eq!(
        nested.window(0).local()["binSelection"],
        json!(["take-0004"])
    );
    nested.keys(&["-M", "shift", "-k", "Down", "-m", "shift"]);
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(
        gui.local()["binSelection"],
        json!(["take-0004", "take-0002"])
    );
    assert_footer(&gui.labels("bin-footer"));
    assert!(
        !gui.regions(&["bin-preview"]).contains_key("bin-preview"),
        "no row actions with two selected"
    );
    nested.screenshot("two-selected");

    // Back to one, and Space previews it: Live switches to Program.
    nested.keys(&["-k", "Up"]);
    nested.keys(&["-k", "space"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let gui = nested.window(0);
        gui.wait_idle();
        let stage = gui.labels("region-stage");
        if stage.iter().any(|l| l == "TAKE 4") {
            break;
        }
        assert!(Instant::now() < deadline, "no TAKE badge: {stage:?}");
        thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        status(&nested, &project)["ui"]["canvasMode"],
        "program",
        "Live switched to Program (D-17)"
    );
    let shot = nested.screenshot("preview");
    compare(&shot, "S14-bin-drawer.png", None, "");
    // Esc ends it (the Esc chain, G§3.8).
    nested.keys(&["-k", "Escape"]);
    let gui = nested.window(0);
    gui.wait_idle();
    assert!(
        !gui.labels("region-stage").iter().any(|l| l == "TAKE 4"),
        "Esc ended the preview"
    );
    // Enter inserts the selected take at the end.
    let before = nested.cli_json(&project, &["clip", "list"])["data"]
        .as_array()
        .map_or(0, Vec::len);
    nested.keys(&["-k", "Return"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let clips = nested.cli_json(&project, &["clip", "list"])["data"].clone();
        let clips = clips.as_array().cloned().unwrap_or_default();
        if clips.len() == before + 1 {
            assert_eq!(clips.last().unwrap()["take"], "take-0004", "{clips:?}");
            break;
        }
        assert!(Instant::now() < deadline, "no clip added: {}", clips.len());
        thread::sleep(Duration::from_millis(200));
    }
    assert_clean(&mut nested);
    close(&mut nested);
}

/// D-38: take 4's row dragged onto the timeline inserts it at the slot it
/// was dropped on (`ClipAdd{at: Index(k)}`); `LocalUi.drag` is `BinInsert`
/// meanwhile.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_bin_row_drags_onto_the_timeline() {
    let mut nested = Nested::start(AREA, "bin-drag", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(bin_ui()),
    );
    open(&mut nested, &project, launch());
    let (x, y) = center(&mut nested, "bin-take-4");
    let regions = nested.window(0).regions(&[]);
    let [tx, ty, _, th] = regions["video-row"];
    let pointer = nested.pointer();
    pointer.move_to(x, y);
    pointer.button(gui_support::pointer::BUTTON_LEFT, true);
    pointer.drag_to(x + 40.0, y + 40.0, 6);
    // Over the video row, at its very start: slot 1 of 11.
    pointer.drag_to(tx + 2.0, ty + th / 2.0, 12);
    thread::sleep(Duration::from_millis(400));
    pointer.drag_to(tx + 3.0, ty + th / 2.0, 2);
    let drag = nested.window(0).local()["drag"].clone();
    nested.screenshot("over-timeline");
    nested
        .pointer()
        .button(gui_support::pointer::BUTTON_LEFT, false);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let clips = nested.cli_json(&project, &["clip", "list"])["data"].clone();
        let clips = clips.as_array().cloned().unwrap_or_default();
        if clips.len() == 11 {
            assert_eq!(
                clips[0]["take"], "take-0004",
                "dropped at slot 1: {clips:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no clip added ({} clips; drag was {drag})",
            clips.len()
        );
        thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(drag, "binInsert", "the drag's kind while over the timeline");
    nested.window(0).wait_idle();
    assert_eq!(nested.window(0).local()["drag"], Value::Null);
    assert_clean(&mut nested);
    close(&mut nested);
}

/// S18's base: takes 2 and 4 selected in the bin (a click, `Ctrl`+click),
/// "Purge from disk" in the footer asks first (the modal over the bin);
/// confirming purges them (`TakePurge`, confirmed).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s18_purging_two_takes_from_the_bin() {
    let mut nested = Nested::start(AREA, "s18", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(bin_ui()),
    );
    open(&mut nested, &project, launch());
    click(&mut nested, "bin-take-2");
    // Ctrl+click take 4: a held `wtype` keeps Ctrl down from 400 ms to
    // 1600 ms after it starts (`Nested::keys_held`: reaped within its
    // deadline); the click lands in between.
    let (x, y) = center(&mut nested, "bin-take-4");
    let held = nested.keys_held(&["-M", "ctrl", "-s", "1200", "-m", "ctrl"], 200);
    thread::sleep(Duration::from_millis(900));
    nested.pointer().click(x, y);
    held.wait();
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(
        gui.local()["binSelection"],
        json!(["take-0004", "take-0002"])
    );
    assert_footer(&gui.labels("bin-footer"));
    click(&mut nested, "bin-purge-selected");
    let gui = nested.window(0);
    let labels = gui.labels("modal");
    assert_shows(
        &labels,
        &[
            "Get out of my swamp?",
            "Purge take 2 and take 4 from disk?",
            "Cancel",
            "Purge 2 takes",
        ],
    );
    let shot = nested.screenshot("modal");
    compare(&shot, "S18-destructive-modal.png", None, "");
    compare(
        &shot,
        "S18-destructive-modal.png",
        Some("352:608:0:48"),
        "-bin",
    );
    assert_clean(&mut nested);
    click(&mut nested, "modal-confirm");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let all = takes(&nested, &project);
        if !all.contains(&"take-0002".to_owned()) && !all.contains(&"take-0004".to_owned()) {
            break;
        }
        assert!(Instant::now() < deadline, "not purged: {all:?}");
        thread::sleep(Duration::from_millis(200));
    }
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(gui.local()["binSelection"], json!([]));
    let labels = gui.labels("region-bin");
    assert!(
        labels.iter().any(|l| l.starts_with("3 takes")),
        "{labels:?}"
    );
    nested.screenshot("purged");
    assert_clean(&mut nested);
    close(&mut nested);
}

// ---------------------------------------------------------------- export

/// S15a: Ctrl+E opens the setup step under the Export slot (440 wide, its
/// top at y 52), MP4 and Standard, the auto name at S15a's now, the
/// summary; the hints are its own; the slot looks pressed.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s15a_export_setup() {
    let mut nested = Nested::start(AREA, "s15a", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(program_ui()),
    );
    open(&mut nested, &project, launch());
    ctrl_e(&mut nested);
    assert_eq!(overlays(&mut nested), ["ExportPanel(Setup)"]);
    assert_panel_placed(&mut nested);
    let gui = nested.window(0);
    let labels = gui.labels("export-panel");
    assert_shows(
        &labels,
        &[
            "Export",
            "Format",
            "MP4",
            "WebM",
            "H.264 + AAC 192 kbps · software encode (x264)",
            "Quality",
            "Draft",
            "Standard",
            "High",
            "Output",
            "Change…",
            "exports/rust-traits-talk-20261014-153012.mp4",
            "1920×1080",
            "00:04:06:11",
            "Enter",
        ],
    );
    assert_eq!(
        hints(&mut nested),
        ["[Enter] Export", "[Tab] Move focus", "[Esc] Close"]
    );
    let regions = nested.window(0).regions(&[]);
    assert_regions(&regions, &BRIEF_REGIONS, &MERGED_REGIONS, 1.0);
    let shot = nested.screenshot("setup");
    compare(&shot, "S15a-export-setup.png", None, "");
    compare(
        &shot,
        "S15a-export-setup.png",
        Some("456:380:1144:40"),
        "-panel",
    );
    assert_clean(&mut nested);
    // Esc closes it, nothing exported.
    nested.keys(&["-k", "Escape"]);
    nested.window(0).wait_idle();
    assert!(overlays(&mut nested).is_empty());
    assert!(exports(&project).is_empty());
    close(&mut nested);
}

/// S15b, then a cancel: Export starts the export (Draft, to be quick); the
/// panel follows it passively (no trap: the S06 hints with the export
/// prefix), and once `qrec status` has it at 42 % or more it is shot.
/// Cancel export ends it: the panel closes and the core says so.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s15b_export_running_then_cancelled() {
    let mut nested = Nested::start(AREA, "s15b", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(program_ui()),
    );
    open(&mut nested, &project, launch());
    export_running(&mut nested, &project, "running");
    let items = hints(&mut nested);
    assert_eq!(
        items[0], "[Esc] Close panel (export continues)",
        "{items:?}"
    );
    assert_eq!(items[1], "[Ctrl][E] Show export", "{items:?}");
    assert!(items.contains(&"[S] Split".to_owned()), "{items:?}");
    click(&mut nested, "export-cancel");
    wait_export_gone(&nested, &project, Duration::from_secs(30));
    wait_overlays(&mut nested, &[], Duration::from_secs(10));
    let gui = nested.window(0);
    gui.wait_idle();
    assert_shows(&gui.labels("toast-host"), &["Export cancelled"]);
    assert!(exports(&project).is_empty(), "nothing written");
    assert_clean(&mut nested);
    close(&mut nested);
}

/// Opens the panel, picks Draft, exports, waits for 42 %, checks the
/// running panel and shoots it as `name` (`waitIdle` while it runs: the
/// window's own export settles once its job runs, G§3.18).
fn export_running(nested: &mut Nested, project: &Path, name: &str) {
    ctrl_e(nested);
    let regions = nested.window(0).regions(&["export-quality"]);
    let [qx, qy, qw, qh] = regions["export-quality"];
    // Draft: the first of three segments.
    nested.pointer().click(qx + qw / 6.0, qy + qh / 2.0);
    nested.window(0).wait_idle();
    click(nested, "export-start");
    assert_eq!(
        overlays(nested),
        ["ExportPanel(Running)"],
        "waitIdle returned with the export running"
    );
    wait_export_at(nested, project, 42, Duration::from_secs(300));
    let labels = wait_label(
        nested,
        "export-panel",
        "42 %",
        Duration::from_secs(20),
        |l| {
            l.strip_suffix('%')
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|p| p >= 42)
        },
    );
    assert_eq!(overlays(nested), ["ExportPanel(Running)"]);
    assert_panel_placed(nested);
    let gui = nested.window(0);
    assert_shows(&labels, &["Exporting", "Cancel export"]);
    // The label counts the timeline's 10 clips, not the 9 render units.
    assert!(
        labels
            .iter()
            .any(|l| (l.starts_with("Rendering clip ") && l.ends_with("/10")) || l == "Stitching"),
        "{labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|l| l.ends_with(" frames") && l.contains(" / 7,391")),
        "{labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("Exporting the timeline as it was at ")),
        "{labels:?}"
    );
    let local = gui.local();
    assert_ne!(
        local["focusInTopLayer"], true,
        "passive: it doesn't take the focus"
    );
    let shot = nested.screenshot(name);
    compare(&shot, "S15b-export-running.png", None, "");
    compare(
        &shot,
        "S15b-export-running.png",
        Some("456:340:1144:0"),
        "-panel",
    );
}

/// S15c: the WebM setup (the AV1 switch and its caption), then a running
/// export under reduced motion (a flat mud bar); D-49: with the passive
/// panel open, `s` splits and a click on the timeline selects a clip.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s15c_webm_setup_and_the_reduced_motion_bar() {
    let mut nested = Nested::start(AREA, "s15c", Options::default());
    reduced_motion(&nested);
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(program_ui()),
    );
    open(&mut nested, &project, launch());
    ctrl_e(&mut nested);
    let regions = nested.window(0).regions(&["export-format"]);
    let [fx, fy, fw, fh] = regions["export-format"];
    nested.pointer().click(fx + fw * 0.75, fy + fh / 2.0);
    let gui = nested.window(0);
    gui.wait_idle();
    let labels = gui.labels("export-panel");
    assert_shows(
        &labels,
        &[
            "VP9 + Opus 128 kbps · software encode",
            "Use AV1 (slower, smaller)",
            "exports/rust-traits-talk-20261014-153012.webm",
        ],
    );
    let shot = nested.screenshot("webm");
    compare(
        &shot,
        "S15c-export-panel-frames.png",
        Some("456:420:1144:40"),
        "-webm",
    );
    // Back to MP4, then a running export with the reduced-motion bar.
    nested.pointer().click(fx + fw * 0.25, fy + fh / 2.0);
    nested.window(0).wait_idle();
    nested.keys(&["-k", "Escape"]);
    nested.window(0).wait_idle();
    export_running(&mut nested, &project, "reduced");
    // D-49: keys and clicks reach the window under the passive panel.
    let before = nested.cli_json(&project, &["clip", "list"])["data"]
        .as_array()
        .map_or(0, Vec::len);
    nested.keys(&["-k", "s"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let clips = nested.cli_json(&project, &["clip", "list"])["data"]
            .as_array()
            .map_or(0, Vec::len);
        if clips == before + 1 {
            break;
        }
        assert!(Instant::now() < deadline, "s didn't split: {clips}");
        thread::sleep(Duration::from_millis(200));
    }
    let regions = nested.window(0).regions(&[]);
    let [vx, vy, vw, vh] = regions["video-row"];
    let x = vx + clip_middle(&nested, &project, "c1b2", vw);
    nested.pointer().click(x, vy + vh / 2.0);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let selection = status(&nested, &project)["ui"]["selection"].clone();
        if selection == json!(["c1b2"]) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the first click didn't select c1b2: {selection}"
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(overlays(&mut nested), ["ExportPanel(Running)"]);
    // Esc closes the passive panel (the export goes on).
    nested.keys(&["-k", "Escape"]);
    wait_overlays(&mut nested, &[], Duration::from_secs(10));
    assert!(
        export_percent(&nested, &project).is_some(),
        "still exporting"
    );
    let _ = nested.cli_json(&project, &["export", "cancel"]);
    wait_export_gone(&nested, &project, Duration::from_secs(30));
    nested.window(0).wait_idle();
    assert_clean(&mut nested);
    close(&mut nested);
}

/// S16: the Smoke fixture's export from the panel ends in its Done state:
/// "Export finished", the path, the details, Open file focused; the core's
/// done toast is dropped meanwhile.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s16_export_done() {
    let mut nested = Nested::start(AREA, "s16", Options::default());
    let project = fixture(&nested, "smoke", DesignVariant::Smoke, Some(program_ui()));
    open(&mut nested, &project, launch());
    ctrl_e(&mut nested);
    click(&mut nested, "export-start");
    wait_overlays(
        &mut nested,
        &["ExportPanel(Done)"],
        Duration::from_secs(120),
    );
    let gui = nested.window(0);
    gui.wait_idle();
    assert_panel_placed(&mut nested);
    let gui = nested.window(0);
    let labels = gui.labels("export-panel");
    assert_shows(
        &labels,
        &[
            "Export finished",
            "Layers! Like an onion. Your video is ready.",
            "Copy path",
            "Open folder",
            "Open file",
        ],
    );
    let written = exports(&project);
    assert_eq!(written.len(), 1, "{written:?}");
    let name = written[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        labels.iter().any(|l| *l == format!("exports/{name}")),
        "the path: {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|l| l.contains("1920×1080 · 30 fps · 00:00:03:00")),
        "the details: {labels:?}"
    );
    assert!(
        gui.labels("toast-host").is_empty(),
        "the panel shows it: no toast"
    );
    assert_eq!(
        hints(&mut nested),
        ["[Enter] Open file", "[Tab] Move focus", "[Esc] Close"]
    );
    let shot = nested.screenshot("done");
    compare(&shot, "S16-export-done.png", None, "");
    compare(
        &shot,
        "S16-export-done.png",
        Some("456:300:1144:0"),
        "-panel",
    );
    assert_clean(&mut nested);
    close(&mut nested);
}

/// S16b: the export from the panel, which is closed (`Esc`) before it ends:
/// the core's success toast, "<path> · <size>" with Open file and Open
/// folder (the copy WP7 changed).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s16b_the_done_toast_when_the_panel_is_closed() {
    let mut nested = Nested::start(AREA, "s16b", Options::default());
    let project = fixture(&nested, "smoke", DesignVariant::Smoke, Some(program_ui()));
    open(&mut nested, &project, launch());
    ctrl_e(&mut nested);
    press(&mut nested, "export-start");
    nested.keys(&["-k", "Escape"]);
    let deadline = Instant::now() + Duration::from_secs(120);
    let labels = loop {
        let gui = nested.window(0);
        gui.wait_idle();
        let labels = gui.labels("toast-host");
        if labels
            .iter()
            .any(|l| l == "Layers! Like an onion. Your video is ready.")
        {
            break labels;
        }
        assert!(Instant::now() < deadline, "no toast: {labels:?}");
        thread::sleep(Duration::from_millis(200));
    };
    assert!(overlays(&mut nested).is_empty());
    let written = exports(&project);
    assert_eq!(written.len(), 1, "{written:?}");
    let name = written[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with(&format!("exports/{name} · ")) && l.ends_with('B')),
        "the body: {labels:?}"
    );
    assert_shows(&labels, &["Open file", "Open folder"]);
    assert!(!labels.contains(&"Copy path".to_owned()), "{labels:?}");
    let regions = nested.window(0).regions(&["toast-host"]);
    let [x, y, w, h] = regions["toast-host"];
    assert!((y + h - 956.0).abs() <= 1.0, "{regions:?}");
    assert!((x + w / 2.0 - 800.0).abs() <= 1.0, "{regions:?}");
    let shot = nested.screenshot("toast");
    compare(&shot, "S16b-export-done-panel-closed.png", None, "");
    compare(
        &shot,
        "S16b-export-done-panel-closed.png",
        Some("520:120:540:848"),
        "-toast",
    );
    assert_clean(&mut nested);
    close(&mut nested);
}

/// S17: an ffmpeg that fails the third segment (c1b2) with a VA-API
/// stderr: the failed panel names the clip, the hint, where it stopped,
/// the tail and the log; Try again sends the export again.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s17_export_failed() {
    let mut nested = Nested::start(AREA, "s17", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(program_ui()),
    );
    let path = fail_ffmpeg::failing_path(nested.env.root());
    let launch = Launch {
        path: Some(path),
        ..launch()
    };
    open(&mut nested, &project, launch);
    ctrl_e(&mut nested);
    click(&mut nested, "export-start");
    // While unit 3 waits for units 1 and 2, the label counts the timeline's
    // clips: the third of ten is next.
    wait_label(
        &mut nested,
        "export-panel",
        "clip 3/10",
        Duration::from_secs(300),
        |l| l == "Rendering clip 3/10",
    );
    wait_overlays(
        &mut nested,
        &["ExportPanel(Failed)"],
        Duration::from_secs(300),
    );
    let gui = nested.window(0);
    gui.wait_idle();
    assert_panel_placed(&mut nested);
    let gui = nested.window(0);
    let labels = gui.labels("export-panel");
    // Units 1 and 2 rendered (867 of 7,391 frames), unit 3 (c1b2, the
    // timeline's third clip) failed: BRIEF's lead, at 11 % (the frame's 37).
    let at = format!("Failed at {}%", fail_ffmpeg::FAILED_PERCENT);
    assert_shows(
        &labels,
        &[
            "Export failed",
            "ffmpeg stopped while rendering clip 3/10 (take 3, c1b2). Nothing was written to exports/.",
            "VA-API couldn't start on /dev/dri/renderD129. Try Settings › Recording › Encoder: x264.",
            &at,
            "Open full log",
            "Copy error",
            "Close",
            "Try again",
        ],
    );
    assert!(
        labels
            .iter()
            .any(|l| l.contains("Error opening output files: Input/output error")),
        "the stderr tail: {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with(".qrec/logs/") && l.ends_with(".log")),
        "the log: {labels:?}"
    );
    assert_eq!(
        hints(&mut nested),
        ["[Enter] Try again", "[Tab] Move focus", "[Esc] Close"]
    );
    // The panel shows the failure: the core's toast is dropped (S17 draws
    // none), as the done toast is under S16's Done page.
    assert!(
        nested.window(0).labels("toast-host").is_empty(),
        "no toast over the failed panel"
    );
    assert!(exports(&project).is_empty(), "nothing written");
    let shot = nested.screenshot("failed");
    compare(&shot, "S17-export-failed.png", None, "");
    compare(
        &shot,
        "S17-export-failed.png",
        Some("456:560:1144:40"),
        "-panel",
    );
    assert_clean(&mut nested);
    // Close: the panel goes.
    click(&mut nested, "export-close");
    assert!(overlays(&mut nested).is_empty());
    close(&mut nested);
}

/// A CLI export shows in the panel (Running, passive); when it ends the
/// panel closes and the core's toast says it is ready.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_cli_export_shows_running_then_the_core_toast() {
    let mut nested = Nested::start(AREA, "cli-export", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(program_ui()),
    );
    open(&mut nested, &project, launch());
    let export = nested.cli_background(&project, &["export", "--quality", "draft"]);
    wait_export_at(&nested, &project, 1, Duration::from_secs(120));
    let reply = nested
        .window(0)
        .ctl(json!({"op": "action", "id": "export.panel"}));
    assert_eq!(reply["ok"], true, "{reply}");
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(overlays(&mut nested), ["ExportPanel(Running)"]);
    assert_shows(&nested.window(0).labels("export-panel"), &["Exporting"]);
    nested.screenshot("cli-running");
    let done = reap_within(export, CLI_TIMEOUT, "qrec export");
    assert!(done.success(), "qrec export: {done}");
    wait_overlays(&mut nested, &[], Duration::from_secs(30));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let gui = nested.window(0);
        gui.wait_idle();
        let labels = gui.labels("toast-host");
        if labels
            .iter()
            .any(|l| l == "Layers! Like an onion. Your video is ready.")
        {
            break;
        }
        assert!(Instant::now() < deadline, "no toast: {labels:?}");
        thread::sleep(Duration::from_millis(200));
    }
    nested.screenshot("cli-toast");
    assert_clean(&mut nested);
    close(&mut nested);
}

/// D-24: "Change…" picks a file that exists (the test's stand-in for the
/// portal dialog): Export asks first (the Replace modal), and confirming
/// sends it again, confirmed, as the panel's own export: the file is
/// replaced and the panel ends on its Done page (the core's toast is
/// dropped). A path inside takes/ is refused with the core's message under
/// the Output field.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn d24_an_existing_file_asks_and_a_forbidden_path_says_why() {
    let mut nested = Nested::start(AREA, "d24", Options::default());
    let project = fixture(&nested, "smoke", DesignVariant::Smoke, Some(program_ui()));
    let elsewhere = nested.env.empty_dir("videos");
    let existing = elsewhere.join("talk-final.mp4");
    fs::write(&existing, b"an older export").expect("the existing file");
    let forbidden = project.join("takes/take-0001/out.mp4");
    let paths = format!("{}|{}", forbidden.display(), existing.display());
    let launch = launch().with_env("QREC_GUI_TEST_SAVE_AS", &paths);
    open(&mut nested, &project, launch);
    ctrl_e(&mut nested);
    // A path inside takes/: refused, said under the field.
    click(&mut nested, "export-change");
    click(&mut nested, "export-start");
    let gui = nested.window(0);
    let labels = gui.labels("export-panel");
    let error = labels
        .iter()
        .find(|l| l.starts_with("Can't export to "))
        .unwrap_or_else(|| panic!("no inline error: {labels:?}"));
    assert!(error.contains("out.mp4"), "{error}");
    assert!(
        gui.regions(&["export-output-error"])
            .contains_key("export-output-error")
    );
    assert_eq!(overlays(&mut nested), ["ExportPanel(Setup)"]);
    let shot = nested.screenshot("forbidden");
    compare(
        &shot,
        "S15a-export-setup.png",
        Some("456:420:1144:40"),
        "-panel",
    );
    // An existing file: the Replace modal, then the confirmed export.
    click(&mut nested, "export-change");
    click(&mut nested, "export-start");
    let gui = nested.window(0);
    let labels = gui.labels("modal");
    assert_shows(
        &labels,
        &["Replace talk-final.mp4?", "Replace file", "Cancel"],
    );
    nested.screenshot("replace");
    // Confirmed, it is still the panel's own export: Running, then its
    // Done page (not only the core's toast).
    click(&mut nested, "modal-confirm");
    wait_overlays(
        &mut nested,
        &["ExportPanel(Done)"],
        Duration::from_secs(120),
    );
    let bytes = fs::read(&existing).unwrap_or_default();
    assert!(bytes.len() > 1000, "replaced: {} bytes", bytes.len());
    wait_export_gone(&nested, &project, Duration::from_secs(60));
    assert!(exports(&project).is_empty(), "written where it was picked");
    let gui = nested.window(0);
    gui.wait_idle();
    let labels = gui.labels("export-panel");
    assert_shows(&labels, &["Export finished", "Open file"]);
    assert!(
        labels.iter().any(|l| l.ends_with("talk-final.mp4")),
        "the picked path: {labels:?}"
    );
    assert!(
        gui.labels("toast-host").is_empty(),
        "the panel shows it: no toast"
    );
    nested.screenshot("replaced-done");
    assert_clean(&mut nested);
    close(&mut nested);
}
