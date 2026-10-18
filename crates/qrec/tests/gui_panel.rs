//! WP5's screenshot cases (docs/GUI.md G§7 WP5, G§8.4): the Onion layers
//! panel of S02, S06 and S13, S13b's row states (each produced through the
//! CLI, the fixture or a fake hook, and cropped), the S12 autocut page and
//! the panel at 1300 wide, in a nested headless sway on the design
//! fixture. The other regions are their owners' (merged) but the bin and
//! the export panel (WP0b's stubs until WP7). S13 drags on WP4's canvas.
//!
//! `cargo test -p qrec --test gui_panel -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon; `QREC_TEST_WLR=
//! gles2` for the GL run). Screenshots, crops, side-by-side comparisons
//! with the approved PNGs and logs go to `tmp/gui-shots/wp5/`.
//!
//! The meters show pinned levels in these cases (`QREC_GUI_TEST_LEVELS`,
//! test mode only): BRIEF's −22/−16 dBFS on A1 and −52/−48 on A2 instead
//! of the fake tone's ticks, and a clipping A1 for S13b's latch.

mod gui_support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use gui_support::pointer::BUTTON_LEFT;
use gui_support::{Gui, Launch, Nested, OUTPUT, Options, assert_regions, poll, wait_for_gui_host};
use qrec_core::state::{CanvasMode, UiState};
use qrec_core::view::autocut;
use qrec_core::view::panel::{ACCESS_DENIED, INSPECTOR_EMPTY, MISSING_IN_TAKE};
use qrec_core::{ClipId, LayerId};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use qrec_shell::testkit::{TestEnv, run_tool};
use serde_json::{Value, json};

const OPEN: Duration = Duration::from_secs(60);
const AREA: &str = "wp5";
/// The panel region at 1600×1000 (BRIEF §2.1).
const PANEL: [f64; 4] = [1216.0, 48.0, 384.0, 920.0];
/// The design levels of the meters (S02).
const DESIGN_LEVELS: &str = "alsa_input.fake=-22/-16,alsa_output.fake.monitor=-52/-48";
/// The icons GTK's and libadwaita's own widgets look up for images Qrec
/// never shows (G§5.2): the switches' (transparent, common.css) and the
/// dropdown button's selected-item check (GTK hides it in the button).
const GTK_HIDDEN_ICONS: [&str; 3] = [
    "switch-on-symbolic",
    "switch-off-symbolic",
    "object-select-symbolic",
];

type Regions = BTreeMap<String, [f64; 4]>;

/// `design_fixture(variant)` in a folder named `rust-traits-talk`, with
/// `ui` as its `ui.json`.
fn fixture(nested: &Nested, variant: DesignVariant, ui: &UiState) -> PathBuf {
    let dir = nested.env.empty_dir("rust-traits-talk");
    design_fixture(&dir, variant);
    write_ui(&dir, ui);
    dir
}

/// The window on `project`, open and idle.
fn open<'a>(nested: &'a mut Nested, project: &Path, launch: Launch) -> &'a mut Gui {
    nested.launch(project, launch);
    wait_for_gui_host(nested, project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
    gui
}

/// A launch with the a11y walk and the meters pinned at `levels`.
fn levels(levels: &str) -> Launch {
    Launch::a11y().with_env("QREC_GUI_TEST_LEVELS", levels)
}

/// The base preset (S02: Live, playhead at the end) changed by `f`.
fn ui(f: impl FnOnce(&mut UiState)) -> UiState {
    let mut ui = UiState {
        playhead: 7391,
        ..UiState::default()
    };
    f(&mut ui);
    ui
}

fn clip(id: &str) -> ClipId {
    id.parse().expect("a clip id")
}

/// `ffmpeg` on `args` (a crop, a side by side), within the harness's
/// deadline for a tool.
fn ffmpeg(args: &[&str]) {
    let out = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(args),
        gui_support::TOOL_TIMEOUT,
    );
    assert!(out.status.success(), "ffmpeg {args:?}: {}", out.stderr);
}

/// Crops `rect` (logical px × the output scale) of `shot` to
/// `<shot>-<name>.png`.
fn crop(nested: &Nested, shot: &Path, rect: [f64; 4], name: &str) -> PathBuf {
    let s = nested.opts.scale;
    let [x, y, w, h] = rect.map(|v| (v * s).round() as i64);
    let out = shot.with_file_name(format!(
        "{}-{name}.png",
        shot.file_stem().unwrap().to_string_lossy()
    ));
    ffmpeg(&[
        "-i",
        &shot.display().to_string(),
        "-vf",
        &format!("crop={w}:{h}:{x}:{y}"),
        &out.display().to_string(),
    ]);
    out
}

/// `rect` of `shot` next to `design_rect` of the approved frame `design`
/// (`<shot>-compare.png`).
fn compare(nested: &Nested, shot: &Path, design: &str, rect: [f64; 4], design_rect: [f64; 4]) {
    let s = nested.opts.scale;
    let [x, y, w, h] = rect.map(|v| (v * s).round() as i64);
    let [dx, dy, dw, dh] = design_rect.map(|v| v.round() as i64);
    let png = gui_support::repo().join("design/screens").join(design);
    let out = shot.with_file_name(format!(
        "{}-compare.png",
        shot.file_stem().unwrap().to_string_lossy()
    ));
    ffmpeg(&[
        "-i",
        &shot.display().to_string(),
        "-i",
        &png.display().to_string(),
        "-filter_complex",
        &format!(
            "[0]crop={w}:{h}:{x}:{y},scale={dw}:{dh}[a];[1]crop={dw}:{dh}:{dx}:{dy}[b];[a][b]hstack=inputs=2"
        ),
        &out.display().to_string(),
    ]);
}

/// The labels under the panel.
fn panel_labels(gui: &Gui) -> Vec<String> {
    gui.labels("region-panel")
}

fn assert_has(labels: &[String], wanted: &[&str]) {
    let missing: Vec<&&str> = wanted
        .iter()
        .filter(|w| !labels.iter().any(|l| l == **w))
        .collect();
    assert!(missing.is_empty(), "missing {missing:?} in {labels:#?}");
}

/// The objective checks of a case: no panic caught, the a11y walk empty.
fn assert_clean(gui: &Gui) {
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    gui.assert_no_criticals();
    let a11y = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "a11y: {a11y}");
}

/// `name`'s bounds (it must show).
fn region(gui: &Gui, name: &str) -> [f64; 4] {
    let regions: Regions = gui.regions(&[name]);
    *regions
        .get(name)
        .unwrap_or_else(|| panic!("no {name} on screen: {regions:#?}"))
}

fn center([x, y, w, h]: [f64; 4]) -> (f64, f64) {
    (x + w / 2.0, y + h / 2.0)
}

/// Polls `labels("region-panel")` until `wanted` shows (10 s).
fn wait_for_label(gui: &Gui, wanted: &str) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        gui.wait_idle();
        let labels = panel_labels(gui);
        if labels.iter().any(|l| l == wanted) {
            return labels;
        }
        assert!(
            Instant::now() < deadline,
            "{wanted:?} never showed: {labels:#?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// `qrec <args> --json` succeeded.
fn cli_ok(nested: &Nested, project: &Path, args: &[&str]) -> Value {
    let reply = nested.cli_json(project, args);
    assert_eq!(reply["ok"], true, "qrec {args:?}: {reply}");
    reply
}

fn history_cursor(nested: &Nested, project: &Path) -> u64 {
    let status = nested.cli_json(project, &["status"]);
    status["data"]["history"]["cursor"]
        .as_u64()
        .unwrap_or_else(|| panic!("no history cursor: {status}"))
}

/// Layer `id` in the project scene (`take: None`) or `take`'s.
fn layer(nested: &Nested, project: &Path, take: Option<&str>, id: &str) -> Value {
    let status = nested.cli_json(project, &["status"]);
    let p = &status["data"]["project"];
    let scene = match take {
        Some(take) => &p["takes"][take]["scene"],
        None => &p["scene"],
    };
    scene["layers"]
        .as_array()
        .and_then(|layers| layers.iter().find(|l| l["id"] == id))
        .cloned()
        .unwrap_or_else(|| panic!("no {id} in {take:?}: {status}"))
}

fn clip_count(nested: &Nested, project: &Path) -> usize {
    let list = nested.cli_json(project, &["clip", "list"]);
    list["data"].as_array().map_or(0, Vec::len)
}

// ------------------------------------------------------------------ S02

/// S02's panel: Live, the project scene, the meters live, the inspector's
/// empty line; then M mutes A1 and Add layer adds the other screen.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s02_the_panel_in_live_mode() {
    let mut nested = Nested::start(AREA, "s02", Options::default());
    let scale = nested.opts.scale;
    let project = fixture(&nested, DesignVariant::Base, &ui(|_| {}));
    // Every icon lookup in the window's log (below).
    let launch = levels(DESIGN_LEVELS).with_env("GTK_DEBUG", "icontheme");
    let gui = open(&mut nested, &project, launch);
    let regions = gui.regions(&[]);
    assert_regions(
        &regions,
        &[("region-panel", PANEL)],
        &["region-panel"],
        scale,
    );
    // BRIEF §2.5's blocks.
    let rows: Regions = gui.regions(&[
        "layer-row-L2",
        "layer-row-L1",
        "add-layer",
        "audio-row-A1",
        "audio-row-A2",
        "add-audio",
        "inspector",
    ]);
    assert_regions(
        &rows,
        &[
            ("layer-row-L2", [1232.0, 128.0, 352.0, 44.0]),
            ("layer-row-L1", [1232.0, 172.0, 352.0, 44.0]),
            ("audio-row-A1", [1232.0, 300.0, 352.0, 92.0]),
            ("audio-row-A2", [1232.0, 392.0, 352.0, 92.0]),
        ],
        &[
            "layer-row-L2",
            "layer-row-L1",
            "audio-row-A1",
            "audio-row-A2",
        ],
        scale,
    );
    assert_has(
        &panel_labels(gui),
        &[
            "Onion layers",
            "Project scene · new takes use this",
            "Layers",
            "2",
            "Face",
            "Camera · Logitech C920",
            "Main screen",
            "Screen · FAKE-1 · GIGA-BYTE GS27QA",
            "Add layer",
            "Audio",
            "Yeti Nano",
            "A1",
            "Autocut",
            "−16.0",
            "System audio",
            "A2",
            "−48.0",
            "Gain",
            "Sync",
            "Add audio track",
            "Inspector",
            INSPECTOR_EMPTY,
        ],
    );
    assert_clean(gui);
    let shot = nested.screenshot("live");
    crop(&nested, &shot, PANEL, "panel");
    compare(&nested, &shot, "S02-live-idle.png", PANEL, PANEL);
    // WP10's shot review: the bin's count chip is `raised` (S02; it was
    // the `pressed` overlay).
    let (cx, cy) = nested.opts.pixel_px(68.0, 674.0);
    assert_eq!(
        gui_support::rgb_pixels(&shot, [cx, cy, 1, 1])[0],
        gui_support::token_rgb("swamp-night", "raised"),
        "the count chip's fill"
    );

    // M: AudioSet{muted} on the project scene.
    let (x, y) = center(region(nested.window(0), "audio-mute-A1"));
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    let status = nested.cli_json(&project, &["status"]);
    assert_eq!(
        status["data"]["project"]["scene"]["audio"][0]["muted"], true,
        "{status}"
    );
    nested.screenshot("muted");

    // Add layer ▸ Screen ▸ the screen not in the scene (the one in use is
    // disabled, so the keyboard skips it).
    let (x, y) = center(region(nested.window(0), "add-layer"));
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    nested.keys(&["-k", "Return"]);
    nested.window(0).wait_idle();
    nested.screenshot("add-layer-menu");
    // The submenu's title (back) has the focus; Down skips FAKE-1.
    nested.keys(&["-k", "Down", "-k", "Return"]);
    let gui = nested.window(0);
    let labels = wait_for_label(gui, "Screen FAKE-2");
    assert_has(&labels, &["3", "Screen · FAKE-2 · Dell U2720Q"]);
    assert_clean(nested.window(0));

    // G§5.2: every icon the window looked up (`GTK_DEBUG=icontheme`'s
    // "looking up icon <name> for scale <n>") is Qrec's, the Add layer
    // menu's and its submenu's too, but the hidden ones of GTK's own
    // widgets. The icons and art lane's lookup audit: the submenu's arrow
    // and its back arrow were libadwaita's `go-next` and `go-previous`.
    let log = std::fs::read_to_string(&nested.window(0).log).expect("the window's log");
    let looked_up: BTreeSet<&str> = log
        .lines()
        .filter_map(|l| l.strip_prefix("looking up icon "))
        .filter_map(|l| l.split(" for scale ").next())
        .collect();
    assert!(
        looked_up.contains("qrec-plus-symbolic"),
        "GTK_DEBUG=icontheme logged no lookup: {looked_up:?}"
    );
    let foreign: Vec<&str> = looked_up
        .iter()
        .copied()
        .filter(|name| !name.starts_with("qrec-") && !GTK_HIDDEN_ICONS.contains(name))
        .collect();
    assert!(foreign.is_empty(), "icons that aren't Qrec's: {foreign:?}");
}

// ------------------------------------------------------------------ S06

/// S06's panel: Program with c2d0 selected edits take 5's layout (the
/// banner and `Layout ▾`); L2 Face is selected and the inspector shows it.
/// Then one inspector commit is one `LayerSet` on take 5, an invalid value
/// sends nothing, and `Layout ▸ Reset take to project scene` restores it.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s06_the_take_layout_and_the_inspector() {
    let mut nested = Nested::start(AREA, "s06", Options::default());
    let scale = nested.opts.scale;
    let preset = ui(|u| {
        u.canvas_mode = CanvasMode::Program;
        u.selection = vec![clip("c2d0")];
        u.selected_layer = Some(LayerId::numbered(2));
        u.playhead = 2437;
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    let gui = open(&mut nested, &project, levels(DESIGN_LEVELS));
    let rows: Regions = gui.regions(&["take-banner", "layer-row-L2", "inspector"]);
    assert_regions(
        &rows,
        &[
            ("take-banner", [1232.0, 96.0, 352.0, 40.0]),
            ("layer-row-L2", [1232.0, 176.0, 352.0, 44.0]),
        ],
        &["take-banner", "layer-row-L2", "inspector"],
        scale,
    );
    // C13's `Layout ▾` as S06 draws it (the icons and art lane measured
    // its chevron 4 px and its label 8 px left of the frame's): the button
    // 4 px from the banner's end, 8 px before its label, a 4 px gap, the
    // 16 px chevron, 6 px after it.
    let parts = gui.regions(&[
        "take-banner",
        "layout-menu",
        "layout-menu-label",
        "layout-menu-chevron",
    ]);
    let part = |name: &str| {
        *parts
            .get(name)
            .unwrap_or_else(|| panic!("no {name}: {parts:#?}"))
    };
    let right = |r: [f64; 4]| r[0] + r[2];
    let (banner, menu) = (part("take-banner"), part("layout-menu"));
    let (label, chevron) = (part("layout-menu-label"), part("layout-menu-chevron"));
    let c13 = [
        ("the banner's end padding", right(banner) - right(menu), 4.0),
        ("after the chevron", right(menu) - right(chevron), 6.0),
        ("the chevron", chevron[2], 16.0),
        ("the gap", chevron[0] - right(label), 4.0),
        ("before the label", label[0] - menu[0], 8.0),
    ];
    for (what, got, want) in c13 {
        assert!(
            (got - want).abs() <= 0.5,
            "Layout ▾, {what}: {got} px for C13's {want} ({parts:?})"
        );
    }
    assert_has(
        &panel_labels(gui),
        &[
            "Take 5 layout · edits this take only",
            "Changes here only affect take 5.",
            "Layout",
            "Inspector",
            "Face · L2",
            "Position X",
            "Position Y",
            "Width",
            "Height",
            "Crop top",
            "Right",
            "Bottom",
            "Left",
            "Opacity",
            "Fit",
            "Contain",
            "Cover",
        ],
    );
    assert_clean(gui);
    // The pointer over c2d1 (the second take 5 clip, at fit) shows its
    // tooltip, as S06 draws it: at (692, 764), 8 px under the pointer
    // (BRIEF's tooltips), so the pointer is at (692, 756).
    nested.pointer().move_to(692.0, 756.0);
    poll(Duration::from_secs(10), "c2d1's tooltip", || {
        let tip = nested.window(0).labels("timeline-tooltip");
        tip.iter().any(|l| l == "Take 5 · c2d1").then_some(())
    });
    nested.window(0).wait_idle();
    let shot = nested.screenshot("program");
    crop(&nested, &shot, PANEL, "panel");
    compare(
        &nested,
        &shot,
        "S06-program-with-selection.png",
        PANEL,
        PANEL,
    );
    // WP10's shot review, as S06 draws them: Fit's selected segment 26 px
    // tall 3 px inside its 32 px well (`field` at y 861–862 and 889–890,
    // the segment's `border` at 863 and 888; it was 28 px, 1 px inside),
    // and Opacity's knob 8 px in from the track's end (the `accent` fill
    // ends at x 1491: its thumb travelled to 2 px from the ends, 1497).
    let token = |name: &str| gui_support::token_rgb("swamp-night", name);
    let px = |x: f64, y: f64| {
        let (dx, dy) = nested.opts.pixel_px(x, y);
        gui_support::rgb_pixels(&shot, [dx, dy, 1, 1])[0]
    };
    for (y, name) in [
        (861.0, "field"),
        (862.0, "field"),
        (863.0, "border"),
        (888.0, "border"),
        (889.0, "field"),
        (890.0, "field"),
    ] {
        assert_eq!(px(1340.0, y), token(name), "Fit at y {y}: {name}");
    }
    assert_eq!(
        px(1490.0, 819.0),
        token("accent"),
        "Opacity's fill to x 1491"
    );
    assert_ne!(
        px(1493.0, 819.0),
        token("accent"),
        "Opacity's fill ends at x 1491"
    );

    // X: type 1280, Enter: one LayerSet on take 5's layout. One held
    // `wtype` run from the typing to past Enter: the window is active only
    // while one runs, and the end of a run blurs the field, which commits
    // too (R68; leaving the field is the W case below).
    let before = history_cursor(&nested, &project);
    let (x, y) = center(region(nested.window(0), "inspector-x"));
    nested.pointer().click(x, y);
    // Return comes ≈ 2.9 s into the run, which holds its keyboard 5 s more:
    // what lands before `in_the_hold` came from Enter, not from the blur
    // at the run's end.
    let started = Instant::now();
    let in_the_hold =
        || (started + Duration::from_millis(6500)).saturating_duration_since(Instant::now());
    let held = nested.keys_held(
        &[
            "-M", "ctrl", "-k", "a", "-m", "ctrl", "1280", "-s", "2500", "-k", "Return",
        ],
        5000,
    );
    poll(Duration::from_secs(10), "1280 typed in X", || {
        let fields = nested.window(0).entries("inspector");
        (fields.get("inspector-x").map(String::as_str) == Some("1280")).then_some(())
    });
    assert_eq!(
        history_cursor(&nested, &project),
        before,
        "nothing sent while typing"
    );
    poll(in_the_hold(), "Enter's LayerSet, the keyboard held", || {
        (history_cursor(&nested, &project) == before + 1).then_some(())
    });
    held.wait();
    nested.window(0).wait_idle();
    let l2 = layer(&nested, &project, Some("take-0005"), "L2");
    assert_eq!(l2["rect"]["x"], 1280, "{l2}");
    assert_eq!(
        layer(&nested, &project, None, "L2")["rect"]["x"],
        1500,
        "the project scene is untouched"
    );
    let after = history_cursor(&nested, &project);
    assert_eq!(
        after,
        before + 1,
        "one entry: the blur commits nothing again"
    );
    // W: 0 and Enter (held again) is refused with the helper text, nothing
    // sent.
    let (x, y) = center(region(nested.window(0), "inspector-w"));
    nested.pointer().click(x, y);
    let started = Instant::now();
    let in_the_hold =
        || (started + Duration::from_millis(6500)).saturating_duration_since(Instant::now());
    let held = nested.keys_held(
        &[
            "-M", "ctrl", "-k", "a", "-m", "ctrl", "0", "-s", "2500", "-k", "Return",
        ],
        5000,
    );
    poll(Duration::from_secs(10), "0 typed in W", || {
        let fields = nested.window(0).entries("inspector");
        (fields.get("inspector-w").map(String::as_str) == Some("0")).then_some(())
    });
    assert!(
        !panel_labels(nested.window(0))
            .iter()
            .any(|l| l == "Width must be at least 1"),
        "no helper before Enter"
    );
    poll(
        in_the_hold(),
        "Enter's helper text, the keyboard held",
        || {
            panel_labels(nested.window(0))
                .iter()
                .any(|l| l == "Width must be at least 1")
                .then_some(())
        },
    );
    assert_eq!(history_cursor(&nested, &project), after, "nothing sent");
    held.wait();
    nested.window(0).wait_idle();
    nested.screenshot("invalid");
    nested.keys(&["-k", "Escape"]);
    nested.window(0).wait_idle();
    // Esc puts 384 back, and the helper goes with the stroke (R45).
    let width = |nested: &mut Nested| {
        let fields = nested.window(0).entries("inspector");
        fields.get("inspector-w").cloned()
    };
    assert_eq!(width(&mut nested).as_deref(), Some("384"));
    let helper_shown = |nested: &mut Nested| {
        panel_labels(nested.window(0))
            .iter()
            .any(|l| l == "Width must be at least 1")
    };
    assert!(!helper_shown(&mut nested), "Esc clears the helper");
    // Leaving the field commits: 0 typed, then the run ends (the window
    // deactivates, which blurs the field) and the helper shows. A snapshot
    // then (a click on the ruler seeks) puts the model's value back, and
    // no helper.
    let (x, y) = center(region(nested.window(0), "inspector-w"));
    nested.pointer().click(x, y);
    nested.keys(&["-M", "ctrl", "-k", "a", "-m", "ctrl", "0"]);
    let gui = nested.window(0);
    wait_for_label(gui, "Width must be at least 1");
    nested.pointer().click(600.0, 706.0);
    nested.window(0).wait_idle();
    assert_eq!(width(&mut nested).as_deref(), Some("384"));
    assert!(!helper_shown(&mut nested), "the snapshot clears the helper");
    assert_eq!(history_cursor(&nested, &project), after, "nothing sent");
    // Layout ▸ Reset take to project scene: one SceneApply.
    let (x, y) = center(region(nested.window(0), "layout-menu"));
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    nested.screenshot("layout-menu");
    nested.keys(&["-k", "Up", "-k", "Return"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while history_cursor(&nested, &project) == after {
        assert!(Instant::now() < deadline, "the layout item sent nothing");
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        layer(&nested, &project, Some("take-0005"), "L2")["rect"]["x"],
        1500,
        "reset to the project scene"
    );
    assert_clean(nested.window(0));
}

/// Track `id` in the project scene (`take: None`) or `take`'s.
fn track(nested: &Nested, project: &Path, take: Option<&str>, id: &str) -> Value {
    let status = nested.cli_json(project, &["status"]);
    let p = &status["data"]["project"];
    let scene = match take {
        Some(take) => &p["takes"][take]["scene"],
        None => &p["scene"],
    };
    scene["audio"]
        .as_array()
        .and_then(|tracks| tracks.iter().find(|t| t["id"] == id))
        .cloned()
        .unwrap_or_else(|| panic!("no {id} in {take:?}: {status}"))
}

/// R38: an edit belongs to the scene it was typed for. When the scene the
/// panel edits changes under a field or a rename being edited (here the
/// canvas mode, sent the way an agent's command comes: not by a click in
/// the window), the field shows the new scene's value at once, the rename
/// ends unsaved, and the edit's Enter sends nothing to either scene. Each
/// edit is one held `wtype` run (the window is active only while one runs).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn an_edit_stays_with_its_scene() {
    let mut nested = Nested::start(AREA, "edit-scene", Options::default());
    let preset = ui(|u| {
        u.selection = vec![clip("c2d0")];
        u.selected_layer = Some(LayerId::numbered(2));
        u.playhead = 2437;
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    let before = history_cursor(&nested, &project);
    let set_mode = |nested: &mut Nested, mode: &str| {
        let command = json!({"cmd": "setCanvasMode", "mode": mode});
        let reply = nested
            .window(0)
            .ctl(json!({"op": "dispatch", "command": command}));
        assert_eq!(reply["ok"], true, "{reply}");
    };
    let entry = |nested: &mut Nested, under: &str, name: &str| {
        nested.window(0).entries(under).get(name).cloned()
    };
    // Position X, typed for the project scene (Live); the panel turns to
    // take 5's layout (Program) before Enter.
    let (x, y) = center(region(nested.window(0), "inspector-x"));
    nested.pointer().click(x, y);
    let held = nested.keys_held(
        &[
            "-M", "ctrl", "-k", "a", "-m", "ctrl", "200", "-s", "5000", "-k", "Return",
        ],
        1500,
    );
    poll(Duration::from_secs(10), "200 typed in X", || {
        (entry(&mut nested, "inspector", "inspector-x").as_deref() == Some("200")).then_some(())
    });
    set_mode(&mut nested, "program");
    wait_for_label(nested.window(0), "Take 5 layout · edits this take only");
    assert_eq!(
        entry(&mut nested, "inspector", "inspector-x").as_deref(),
        Some("1500"),
        "take 5's value, the field still focused"
    );
    held.wait();
    nested.window(0).wait_idle();
    let x_of = |nested: &Nested, take| layer(nested, &project, take, "L2")["rect"]["x"].clone();
    assert_eq!(x_of(&nested, Some("take-0005")), 1500, "take 5 untouched");
    assert_eq!(x_of(&nested, None), 1500, "the project scene untouched");
    assert_eq!(history_cursor(&nested, &project), before, "nothing sent");

    // A1's gain, typed for take 5 (Program); back to Live before Enter.
    let (x, y) = center(region(nested.window(0), "audio-gain-A1"));
    nested.pointer().click(x, y);
    let held = nested.keys_held(
        &[
            "-M",
            "ctrl",
            "-k",
            "a",
            "-m",
            "ctrl",
            "\u{2212}6",
            "-s",
            "5000",
            "-k",
            "Return",
        ],
        1500,
    );
    poll(Duration::from_secs(10), "−6 typed in A1's gain", || {
        (entry(&mut nested, "audio-row-A1", "audio-gain-A1").as_deref() == Some("\u{2212}6"))
            .then_some(())
    });
    set_mode(&mut nested, "live");
    wait_for_label(nested.window(0), "Project scene · new takes use this");
    assert_eq!(
        entry(&mut nested, "audio-row-A1", "audio-gain-A1").as_deref(),
        Some("0.0"),
        "the project scene's gain, the field still focused"
    );
    held.wait();
    nested.window(0).wait_idle();
    let gain_of = |nested: &Nested, take| track(nested, &project, take, "A1")["gainDb"].clone();
    assert_eq!(gain_of(&nested, Some("take-0005")), 0.0, "take 5 untouched");
    assert_eq!(gain_of(&nested, None), 0.0, "the project scene untouched");
    assert_eq!(history_cursor(&nested, &project), before, "nothing sent");

    // L2's rename (the overflow's Rename), started in Live; Program before
    // Enter: the edit ends, unsaved.
    let (x, y) = center(region(nested.window(0), "layer-menu-L2"));
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    let held = nested.keys_held(
        &[
            "-k", "Return", "-s", "300", "-M", "ctrl", "-k", "a", "-m", "ctrl", "Webcam", "-s",
            "5000", "-k", "Return",
        ],
        1500,
    );
    poll(Duration::from_secs(10), "Webcam typed in L2's name", || {
        (entry(&mut nested, "layer-row-L2", "layer-row-L2").as_deref() == Some("Webcam"))
            .then_some(())
    });
    set_mode(&mut nested, "program");
    wait_for_label(nested.window(0), "Take 5 layout · edits this take only");
    poll(Duration::from_secs(5), "the rename to end", || {
        entry(&mut nested, "layer-row-L2", "layer-row-L2")
            .is_none()
            .then_some(())
    });
    held.wait();
    nested.window(0).wait_idle();
    let name_of = |nested: &Nested, take| layer(nested, &project, take, "L2")["name"].clone();
    assert_eq!(name_of(&nested, Some("take-0005")), "Face", "take 5's name");
    assert_eq!(name_of(&nested, None), "Face", "the project scene's name");
    assert_eq!(history_cursor(&nested, &project), before, "nothing sent");
    assert_has(&panel_labels(nested.window(0)), &["Face"]);
    assert_clean(nested.window(0));
}

/// R38's remainder (G§3.12 R12): a press anywhere outside an edit in
/// progress ends it first, so the edit goes to the scene it was typed for
/// (one undo step) ahead of what the press does. Chrome buttons never take
/// the focus (D-11): the field kept it, and the mode switch's change
/// dropped the edit (R38's reset), the transport's Play left it unsent. X
/// typed for the project scene, then a click on its own unit (inside the
/// edit: nothing sent) and one on Program; A1's gain typed for take 5,
/// then Live; L2's rename in Live, then Program; X typed for take 5, then
/// Play. Each edit is one held `wtype` run (the window is active only
/// while one runs, and a window going inactive commits its field): the
/// checks run while it holds, so only the press committed.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_press_outside_an_edit_commits_it_first() {
    let mut nested = Nested::start(AREA, "edit-press", Options::default());
    let preset = ui(|u| {
        u.selection = vec![clip("c2d0")];
        u.selected_layer = Some(LayerId::numbered(2));
        u.playhead = 2437;
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    let before = history_cursor(&nested, &project);
    // C05 (BRIEF §2.3): Live 72 wide, then Program 100.
    let [sx, sy, sw, sh] = region(nested.window(0), "mode-switch");
    let live = (sx + 36.0, sy + sh / 2.0);
    let program = (sx + sw - 50.0, sy + sh / 2.0);
    let ui_now = |nested: &Nested| nested.cli_json(&project, &["status"])["data"].clone();
    let mode_is = |nested: &Nested, mode: &str| {
        poll(Duration::from_secs(10), &format!("{mode} mode"), || {
            (ui_now(nested)["ui"]["canvasMode"] == mode).then_some(())
        });
    };
    let entry = |nested: &mut Nested, under: &str, name: &str| {
        nested.window(0).entries(under).get(name).cloned()
    };
    let x_of = |nested: &Nested, take| layer(nested, &project, take, "L2")["rect"]["x"].clone();
    let not_typing = |nested: &mut Nested| {
        let local = nested.window(0).local();
        assert_ne!(
            local["focus"], "field",
            "the keys are the window's: {local}"
        );
    };

    // X, typed for the project scene (Live); a click on Program.
    let (x, y) = center(region(nested.window(0), "inspector-x"));
    nested.pointer().click(x, y);
    let held = nested.keys_held(
        &["-M", "ctrl", "-k", "a", "-m", "ctrl", "200", "-s", "8000"],
        500,
    );
    poll(Duration::from_secs(10), "200 typed in X", || {
        (entry(&mut nested, "inspector", "inspector-x").as_deref() == Some("200")).then_some(())
    });
    // A press inside the edit (the field's own unit, at its right end)
    // leaves it alone: nothing sent, the text still the person's, typing.
    let [fx, fy, fw, fh] = region(nested.window(0), "inspector-x");
    nested.pointer().click(fx + fw - 6.0, fy + fh / 2.0);
    nested.window(0).wait_idle();
    assert_eq!(
        history_cursor(&nested, &project),
        before,
        "nothing sent yet"
    );
    assert_eq!(x_of(&nested, None), 1500, "not committed by its own press");
    assert_eq!(
        entry(&mut nested, "inspector", "inspector-x").as_deref(),
        Some("200")
    );
    assert_eq!(nested.window(0).local()["focus"], "field", "still typing");
    nested.pointer().click(program.0, program.1);
    mode_is(&nested, "program");
    // The commit went first (one FIFO): the mode reads Program, so it ran.
    assert_eq!(x_of(&nested, None), 200, "the project scene's X");
    assert_eq!(x_of(&nested, Some("take-0005")), 1500, "take 5 untouched");
    assert_eq!(history_cursor(&nested, &project), before + 1, "one step");
    wait_for_label(nested.window(0), "Take 5 layout · edits this take only");
    assert_eq!(
        entry(&mut nested, "inspector", "inspector-x").as_deref(),
        Some("1500"),
        "take 5's X shows"
    );
    not_typing(&mut nested);
    held.wait();

    // A1's gain, typed for take 5 (Program); a click on Live.
    let (x, y) = center(region(nested.window(0), "audio-gain-A1"));
    nested.pointer().click(x, y);
    let held = nested.keys_held(
        &[
            "-M",
            "ctrl",
            "-k",
            "a",
            "-m",
            "ctrl",
            "\u{2212}6",
            "-s",
            "8000",
        ],
        500,
    );
    poll(Duration::from_secs(10), "−6 typed in A1's gain", || {
        (entry(&mut nested, "audio-row-A1", "audio-gain-A1").as_deref() == Some("\u{2212}6"))
            .then_some(())
    });
    nested.pointer().click(live.0, live.1);
    mode_is(&nested, "live");
    let gain_of = |nested: &Nested, take| track(nested, &project, take, "A1")["gainDb"].clone();
    assert_eq!(gain_of(&nested, Some("take-0005")), -6.0, "take 5's gain");
    assert_eq!(gain_of(&nested, None), 0.0, "the project scene untouched");
    assert_eq!(history_cursor(&nested, &project), before + 2, "one step");
    not_typing(&mut nested);
    held.wait();

    // L2's rename (the overflow's Rename), in Live; a click on Program.
    let (x, y) = center(region(nested.window(0), "layer-menu-L2"));
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    let held = nested.keys_held(
        &[
            "-k", "Return", "-s", "300", "-M", "ctrl", "-k", "a", "-m", "ctrl", "Webcam", "-s",
            "8000",
        ],
        500,
    );
    poll(Duration::from_secs(10), "Webcam typed in L2's name", || {
        (entry(&mut nested, "layer-row-L2", "layer-row-L2").as_deref() == Some("Webcam"))
            .then_some(())
    });
    nested.pointer().click(program.0, program.1);
    mode_is(&nested, "program");
    let name_of = |nested: &Nested, take| layer(nested, &project, take, "L2")["name"].clone();
    assert_eq!(name_of(&nested, None), "Webcam", "the project scene's L2");
    assert_eq!(name_of(&nested, Some("take-0005")), "Face", "take 5's name");
    assert_eq!(history_cursor(&nested, &project), before + 3, "one step");
    not_typing(&mut nested);
    held.wait();

    // X, typed for take 5 (Program); a click on the transport's Play (the
    // middle of its five buttons, at the row's center, BRIEF §2.4).
    let (x, y) = center(region(nested.window(0), "inspector-x"));
    nested.pointer().click(x, y);
    let held = nested.keys_held(
        &["-M", "ctrl", "-k", "a", "-m", "ctrl", "300", "-s", "8000"],
        500,
    );
    poll(Duration::from_secs(10), "300 typed in X", || {
        (entry(&mut nested, "inspector", "inspector-x").as_deref() == Some("300")).then_some(())
    });
    let (x, y) = center(region(nested.window(0), "transport"));
    nested.pointer().click(x, y);
    poll(Duration::from_secs(10), "playing", || {
        (ui_now(&nested)["runtime"]["playing"] == true).then_some(())
    });
    assert_eq!(x_of(&nested, Some("take-0005")), 300, "take 5's X");
    assert_eq!(x_of(&nested, None), 200, "the project scene's stays");
    assert_eq!(history_cursor(&nested, &project), before + 4, "one step");
    not_typing(&mut nested);
    held.wait();
    let reply = nested
        .window(0)
        .ctl(json!({"op": "dispatch", "command": {"cmd": "pause"}}));
    assert_eq!(reply["ok"], true, "{reply}");
    nested.window(0).wait_idle();
    assert_clean(nested.window(0));
}

/// R46: a rename that sends nothing (cleared, then Enter) or that the
/// core refuses (a recording started meanwhile: scene edits wait for
/// Idle, D-43) shows the name again, not what was typed.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_rename_that_lands_nowhere_shows_the_name() {
    let mut nested = Nested::start(AREA, "rename-back", Options::default());
    let project = fixture(&nested, DesignVariant::Base, &ui(|_| {}));
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    let before = history_cursor(&nested, &project);
    // The overflow's Rename (its first item), the name cleared, Enter.
    let rename_blank = |nested: &mut Nested, menu: &str| {
        let (x, y) = center(region(nested.window(0), menu));
        nested.pointer().click(x, y);
        nested.window(0).wait_idle();
        let held = nested.keys_held(
            &[
                "-k",
                "Return",
                "-s",
                "300",
                "-M",
                "ctrl",
                "-k",
                "a",
                "-m",
                "ctrl",
                "-k",
                "BackSpace",
                "-k",
                "Return",
            ],
            300,
        );
        held.wait();
        nested.window(0).wait_idle();
    };
    rename_blank(&mut nested, "layer-menu-L2");
    assert_has(&nested.window(0).labels("layer-row-L2"), &["Face"]);
    rename_blank(&mut nested, "audio-menu-A1");
    assert_has(&nested.window(0).labels("audio-row-A1"), &["Yeti Nano"]);
    assert_eq!(history_cursor(&nested, &project), before, "nothing sent");

    // A recording starts while "Webcam" is typed: edits stop (D-43), so
    // the rename ends unsaved at once instead of staying open in the
    // disabled row, and the name shows.
    let (x, y) = center(region(nested.window(0), "layer-menu-L2"));
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    let started = Instant::now();
    let held = nested.keys_held(
        &[
            "-k", "Return", "-s", "300", "-M", "ctrl", "-k", "a", "-m", "ctrl", "Webcam", "-s",
            "10000", "-k", "Return",
        ],
        1000,
    );
    poll(Duration::from_secs(10), "Webcam typed in L2's name", || {
        let fields = nested.window(0).entries("layer-row-L2");
        (fields.get("layer-row-L2").map(String::as_str) == Some("Webcam")).then_some(())
    });
    cli_ok(&nested, &project, &["record", "start", "--countdown", "0"]);
    let phase = |nested: &Nested| {
        nested.cli_json(&project, &["status"])["data"]["runtime"]["record"]["phase"].clone()
    };
    poll(Duration::from_secs(30), "recording", || {
        (phase(&nested) == "recording").then_some(())
    });
    // Before the run's Enter (≈ 10.7 s in).
    let before_enter =
        (started + Duration::from_millis(10_000)).saturating_duration_since(Instant::now());
    let name_back = |nested: &mut Nested| {
        let gui = nested.window(0);
        let labels = gui.labels("layer-row-L2");
        let editing = gui.entries("layer-row-L2").contains_key("layer-row-L2");
        (labels.iter().any(|l| l == "Face") && !editing).then_some(labels)
    };
    let labels = poll(before_enter, "the rename to end, the name back", || {
        name_back(&mut nested)
    });
    assert!(!labels.iter().any(|l| l == "Webcam"), "{labels:#?}");
    held.wait();
    nested.window(0).wait_idle();
    assert_eq!(layer(&nested, &project, None, "L2")["name"], "Face");
    cli_ok(&nested, &project, &["record", "stop"]);
    poll(Duration::from_secs(30), "idle again", || {
        (phase(&nested) == "idle").then_some(())
    });
    nested.window(0).wait_idle();

    // Refused by the core: a pasted name with a tab (no control
    // characters in a name). The name shows again, and the refusal toasts.
    // `wl-copy` on the nested display (`nested_tool`: its runtime dir, never
    // the person's session) serves the clipboard from the background until
    // that display goes; its output goes to the run's files.
    let runtime = nested.env.xdg_runtime_dir();
    assert!(
        !runtime.starts_with("/run/user/"),
        "the nested runtime dir, never the session's: {}",
        runtime.display()
    );
    let copy = run_tool(
        nested.nested_tool("wl-copy").arg("Web\tcam"),
        gui_support::TOOL_TIMEOUT,
    );
    assert!(copy.status.success(), "wl-copy: {}", copy.stderr);
    let (x, y) = center(region(nested.window(0), "layer-menu-L2"));
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    let held = nested.keys_held(
        &[
            "-k", "Return", "-s", "300", "-M", "ctrl", "-k", "a", "-k", "v", "-m", "ctrl", "-s",
            "3000", "-k", "Return",
        ],
        1500,
    );
    poll(
        Duration::from_secs(10),
        "the pasted name in L2's field",
        || {
            let fields = nested.window(0).entries("layer-row-L2");
            (fields.get("layer-row-L2").map(String::as_str) == Some("Web\tcam")).then_some(())
        },
    );
    held.wait();
    nested.window(0).wait_idle();
    let labels = poll(
        Duration::from_secs(10),
        "the name back after the refusal",
        || name_back(&mut nested),
    );
    assert!(!labels.iter().any(|l| l.contains("cam")), "{labels:#?}");
    assert_eq!(layer(&nested, &project, None, "L2")["name"], "Face");
    let toast = nested.window(0).labels("toast-host");
    assert!(
        toast.iter().any(|t| t.contains("control characters")),
        "the refusal toasts: {toast:?}"
    );
    assert_clean(nested.window(0));
}

/// R43+R63: the rows a rebuild drops go (nothing a row's own callbacks
/// hold keeps it alive, G§3.12 R11). A reorder, a removed track and its
/// undo make the lists again, and only the rows shown live on; a rename
/// open on a row that goes ends unsaved, and nothing panics.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn rebuilt_rows_go() {
    let mut nested = Nested::start(AREA, "rows-go", Options::default());
    let project = fixture(&nested, DesignVariant::Base, &ui(|_| {}));
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    let alive = |nested: &mut Nested| {
        let local = nested.window(0).local();
        (
            local["layerRowsAlive"].as_u64(),
            local["audioRowsAlive"].as_u64(),
        )
    };
    let settle = |nested: &mut Nested, want: (u64, u64), what: &str| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            nested.window(0).wait_idle();
            let now = alive(nested);
            if now == (Some(want.0), Some(want.1)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{what}: {now:?} rows alive (layers, audio), {want:?} shown"
            );
            thread::sleep(Duration::from_millis(200));
        }
    };
    settle(&mut nested, (2, 2), "the design rows");
    // L1 to the top: the layer rows are made again.
    cli_ok(
        &nested,
        &project,
        &["scene", "layer", "order", "L1", "--to", "1"],
    );
    settle(&mut nested, (2, 2), "after a reorder");
    // A2 removed, then back: the audio rows are made again twice.
    cli_ok(&nested, &project, &["scene", "audio", "remove", "A2"]);
    settle(&mut nested, (2, 1), "after a track's removal");
    cli_ok(&nested, &project, &["undo"]);
    settle(&mut nested, (2, 2), "after its undo");
    // A rename open on L2 when a reorder makes the rows again.
    let (x, y) = center(region(nested.window(0), "layer-menu-L2"));
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    let held = nested.keys_held(
        &[
            "-k", "Return", "-s", "300", "-M", "ctrl", "-k", "a", "-m", "ctrl", "Webcam",
        ],
        4000,
    );
    poll(Duration::from_secs(10), "Webcam typed in L2's name", || {
        let fields = nested.window(0).entries("layer-row-L2");
        (fields.get("layer-row-L2").map(String::as_str) == Some("Webcam")).then_some(())
    });
    cli_ok(
        &nested,
        &project,
        &["scene", "layer", "order", "L2", "--to", "1"],
    );
    settle(&mut nested, (2, 2), "after a reorder mid-rename");
    let fields = nested.window(0).entries("layer-row-L2");
    assert!(
        !fields.contains_key("layer-row-L2"),
        "the rename ended: {fields:?}"
    );
    held.wait();
    nested.window(0).wait_idle();
    assert_eq!(layer(&nested, &project, None, "L2")["name"], "Face");
    assert_has(&nested.window(0).labels("layer-row-L2"), &["Face"]);
    assert_clean(nested.window(0));
}

/// R44: a snapshot during an opacity drag (here a seek from the CLI)
/// leaves the slider and its field where the pointer has them, and the
/// release sends that value (it used to snap back to the model's and send
/// nothing).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_snapshot_mid_drag_keeps_the_slider() {
    let mut nested = Nested::start(AREA, "opacity-drag", Options::default());
    let preset = ui(|u| u.selected_layer = Some(LayerId::numbered(2)));
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    let opacity = |nested: &mut Nested| {
        let fields = nested.window(0).entries("inspector");
        fields.get("inspector-opacity").cloned()
    };
    assert_eq!(opacity(&mut nested).as_deref(), Some("100"));
    let [x, y, w, h] = region(nested.window(0), "inspector-opacity-slider");
    let row = y + h / 2.0;
    // The thumb sits at the trough's right end; drag it to about 40 %.
    let pointer = nested.pointer();
    pointer.move_to(x + w - 6.0, row);
    pointer.button(BUTTON_LEFT, true);
    for i in 1..=12 {
        let to = x + w - 6.0 - (w * 0.6) * f64::from(i) / 12.0;
        pointer.move_to(to, row);
        thread::sleep(Duration::from_millis(16));
    }
    nested.window(0).wait_idle();
    let dragged = opacity(&mut nested).expect("the opacity field");
    let percent: u32 = dragged.parse().unwrap_or_else(|_| panic!("{dragged:?}"));
    assert!((30..=55).contains(&percent), "dragged to {dragged}");
    cli_ok(&nested, &project, &["seek", "--to", "30f"]);
    nested.window(0).wait_idle();
    assert_eq!(
        opacity(&mut nested).as_deref(),
        Some(dragged.as_str()),
        "the snapshot kept the drag's value"
    );
    nested.pointer().button(BUTTON_LEFT, false);
    nested.window(0).wait_idle();
    let l2 = layer(&nested, &project, None, "L2");
    let saved = l2["opacity"].as_f64().unwrap_or_else(|| panic!("{l2}"));
    assert!(
        (saved - f64::from(percent) / 100.0).abs() < 1e-9,
        "the release sent {dragged} %: {l2}"
    );
    assert_eq!(opacity(&mut nested).as_deref(), Some(dragged.as_str()));
    assert_clean(nested.window(0));
}

/// R42: the wheel over a slider the keyboard isn't on scrolls what holds
/// it and changes no value, as over a field: the panel's column over the
/// inspector's opacity, the autocut page over its threshold (both used to
/// move by ten, the opacity shown but never sent). With the keyboard on the
/// slider (Shift+Tab from its field, held) the wheel moves it, and a burst
/// of notches is one `LayerSet`.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_wheel_over_a_slider_scrolls_the_panel() {
    // 800 high: the panel's column (y 48 to 768, take 8's banner on top)
    // ends under the inspector.
    let mut nested = Nested::start(AREA, "slider-wheel", Options::size(1600, 800));
    let preset = ui(|u| {
        u.canvas_mode = CanvasMode::Program;
        u.selection = vec![clip("c3e9")];
        u.selected_layer = Some(LayerId::numbered(2));
        u.playhead = 4842;
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    let take = Some("take-0008");
    // L2 at 50 %: a notch up a slider that takes the wheel shows 60.
    cli_ok(
        &nested,
        &project,
        &[
            "scene",
            "layer",
            "set",
            "L2",
            "--take",
            "take-0008",
            "--opacity",
            "0.5",
        ],
    );
    nested.window(0).wait_idle();
    let opacity = |nested: &mut Nested| {
        let fields = nested.window(0).entries("inspector");
        fields.get("inspector-opacity").cloned()
    };
    assert_eq!(opacity(&mut nested).as_deref(), Some("50"));
    let below = region(nested.window(0), "inspector-opacity-slider");
    assert!(below[1] > 768.0, "the slider starts under the column's end");
    // The wheel over the column's top (the rows: no slider) scrolls it to
    // its end, where the slider shows.
    let to_the_end = |nested: &mut Nested| {
        nested.pointer().move_to(PANEL[0] + PANEL[2] / 2.0, 200.0);
        nested.pointer().wheel(12);
        nested.window(0).wait_idle();
        let slider = region(nested.window(0), "inspector-opacity-slider");
        assert!(
            slider[1] + slider[3] <= 768.0,
            "the slider shows: {slider:?}"
        );
        slider
    };
    let [x, y, w, h] = to_the_end(&mut nested);
    // Two notches up over it (GTK's column takes back up to 100 px it ran
    // past its end first): the column scrolls back, nothing moves or goes.
    nested.pointer().move_to(x + w / 2.0, y + h / 2.0);
    nested.pointer().wheel(-2);
    nested.window(0).wait_idle();
    assert_eq!(
        opacity(&mut nested).as_deref(),
        Some("50"),
        "the wheel moved no value"
    );
    let moved = region(nested.window(0), "inspector-opacity-slider");
    assert!(
        moved[1] > y,
        "the column scrolled: the slider went from y {y} to {}",
        moved[1]
    );
    assert_eq!(layer(&nested, &project, take, "L2")["opacity"], 0.5);
    // The keyboard on the slider: three notches up are one change, sent
    // once when the burst ends.
    let [x, y, w, h] = to_the_end(&mut nested);
    let before = history_cursor(&nested, &project);
    let (fx, fy) = center(region(nested.window(0), "inspector-opacity"));
    nested.pointer().click(fx, fy);
    nested.window(0).wait_idle();
    // (GTK moves back on Shift with ISO_Left_Tab: wtype's own keymap has no
    // shifted level for Tab.)
    let held = nested.keys_held(&["-M", "shift", "-k", "ISO_Left_Tab", "-m", "shift"], 6000);
    poll(Duration::from_secs(5), "the keyboard on the slider", || {
        let local = nested.window(0).local();
        (local["focusPath"][0] == "inspector-opacity-slider").then_some(())
    });
    nested.pointer().move_to(x + w / 2.0, y + h / 2.0);
    nested.pointer().wheel(-3);
    let saved = poll(Duration::from_secs(5), "the burst's LayerSet", || {
        let saved = layer(&nested, &project, take, "L2")["opacity"].as_f64()?;
        (saved > 0.5).then_some(saved)
    });
    held.wait();
    nested.window(0).wait_idle();
    let shown = opacity(&mut nested).expect("the opacity field");
    assert_eq!(
        format!("{}", (saved * 100.0).round()),
        shown,
        "sent as shown"
    );
    assert_eq!(
        history_cursor(&nested, &project),
        before + 1,
        "one LayerSet for the burst"
    );
    // The autocut page: the wheel over its threshold scrolls the page.
    nested.keys(&["-M", "shift", "-k", "c", "-m", "shift"]);
    wait_for_label(nested.window(0), "Removes 4 silences");
    let threshold = |nested: &mut Nested| {
        let fields = nested.window(0).entries("autocut-page");
        fields.get("knob-field-0").cloned()
    };
    assert_eq!(threshold(&mut nested).as_deref(), Some("\u{2212}35"));
    let (x, y) = center(region(nested.window(0), "knob-scale-0"));
    nested.pointer().move_to(x, y);
    nested.pointer().wheel(1);
    nested.window(0).wait_idle();
    assert_eq!(
        threshold(&mut nested).as_deref(),
        Some("\u{2212}35"),
        "the threshold is the project's"
    );
    assert_has(&panel_labels(nested.window(0)), &["Removes 4 silences"]);
    assert_clean(nested.window(0));
}

/// R42's audit: on GTK 4.22 a pointer press makes the slider the window's
/// focus widget (`focus_on_click(false)` notwithstanding), so after a drag
/// the wheel moved the slider whenever the window was active, and saved
/// it. A pointer drag gives the keyboard nothing: with the keyboard held
/// (the window active) two notches over the slider scroll the column, and
/// neither the value nor the history moves.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_wheel_after_a_drag_scrolls_past_the_slider() {
    let mut nested = Nested::start(AREA, "slider-wheel-after-drag", Options::size(1600, 800));
    let preset = ui(|u| {
        u.canvas_mode = CanvasMode::Program;
        u.selection = vec![clip("c3e9")];
        u.selected_layer = Some(LayerId::numbered(2));
        u.playhead = 4842;
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    let take = Some("take-0008");
    cli_ok(
        &nested,
        &project,
        &[
            "scene",
            "layer",
            "set",
            "L2",
            "--take",
            "take-0008",
            "--opacity",
            "0.5",
        ],
    );
    nested.window(0).wait_idle();
    let opacity = |nested: &mut Nested| {
        let fields = nested.window(0).entries("inspector");
        fields.get("inspector-opacity").cloned()
    };
    // The column to its end, where the slider shows.
    nested.pointer().move_to(PANEL[0] + PANEL[2] / 2.0, 200.0);
    nested.pointer().wheel(12);
    nested.window(0).wait_idle();
    let [x, y, w, h] = region(nested.window(0), "inspector-opacity-slider");
    assert!(y + h <= 768.0, "the slider shows: {:?}", [x, y, w, h]);
    // Drag the thumb (at 50 %) a tenth of the trough left, and let go.
    let before = history_cursor(&nested, &project);
    let row = y + h / 2.0;
    let pointer = nested.pointer();
    pointer.move_to(x + w / 2.0, row);
    pointer.button(BUTTON_LEFT, true);
    for i in 1..=8 {
        pointer.move_to(x + w / 2.0 - (w * 0.1) * f64::from(i) / 8.0, row);
        thread::sleep(Duration::from_millis(16));
    }
    pointer.button(BUTTON_LEFT, false);
    nested.window(0).wait_idle();
    let saved = poll(Duration::from_secs(5), "the drag's LayerSet", || {
        let saved = layer(&nested, &project, take, "L2")["opacity"].as_f64()?;
        (saved < 0.5).then_some(saved)
    });
    assert_eq!(
        history_cursor(&nested, &project),
        before + 1,
        "the drag's one entry"
    );
    let dragged = opacity(&mut nested).expect("the opacity field");
    let local = nested.window(0).local();
    assert_eq!(
        local["focusPath"][0], "inspector-opacity-slider",
        "GTK 4.22 focused the slider on the press: {local}"
    );
    // The window active (a held keyboard), two notches over the slider.
    let held = nested.keys_held(&[], 3000);
    thread::sleep(Duration::from_millis(800));
    nested.pointer().move_to(x + w / 2.0, row);
    nested.pointer().wheel(-2);
    // Past a wheel burst's end (300 ms).
    thread::sleep(Duration::from_millis(600));
    nested.window(0).wait_idle();
    let moved = region(nested.window(0), "inspector-opacity-slider");
    assert_eq!(
        opacity(&mut nested).as_deref(),
        Some(dragged.as_str()),
        "the wheel moved the value"
    );
    assert_eq!(
        layer(&nested, &project, take, "L2")["opacity"].as_f64(),
        Some(saved),
        "the wheel saved a value"
    );
    assert_eq!(history_cursor(&nested, &project), before + 1, "no entry");
    assert!(
        moved[1] > y,
        "the column scrolled: the slider went from y {y} to {}",
        moved[1]
    );
    held.wait();
    assert_clean(nested.window(0));
}

/// R44's audit: a focused slider follows the model (the half the drag
/// case can't see: a press focuses the slider and GTK puts it back under
/// the pointer on the next frame). With the keyboard on the slider (Shift+
/// Tab from its field, held): Left saves 0.99, Ctrl+Z puts 1.0 back and
/// the slider follows it, so the next Left saves 0.99 again. A focused
/// slider was never refreshed: it stayed at 99 and the next Left saved
/// 0.98.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_focused_slider_follows_an_undo() {
    let mut nested = Nested::start(AREA, "slider-undo", Options::default());
    let preset = ui(|u| u.selected_layer = Some(LayerId::numbered(2)));
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    let opacity = |nested: &mut Nested| {
        let fields = nested.window(0).entries("inspector");
        fields.get("inspector-opacity").cloned()
    };
    assert_eq!(opacity(&mut nested).as_deref(), Some("100"));
    let saved = |nested: &Nested| layer(nested, &project, None, "L2")["opacity"].as_f64();
    assert_eq!(saved(&nested), Some(1.0));
    let (fx, fy) = center(region(nested.window(0), "inspector-opacity"));
    nested.pointer().click(fx, fy);
    nested.window(0).wait_idle();
    // One keyboard for it all (the slider keeps the focus): Shift+Tab to
    // the slider, Left, Ctrl+Z, Left. Within 3 s of the last arrow: GTK
    // hides the focus ring then (`gets_focus_visible`), and the router
    // gives the arrows to the timeline once it is hidden (G§3.8 step 7).
    let held = nested.keys_held(
        &[
            "-M",
            "shift",
            "-k",
            "ISO_Left_Tab",
            "-m",
            "shift",
            "-s",
            "300",
            "-k",
            "Left",
            "-s",
            "800",
            "-M",
            "ctrl",
            "-k",
            "z",
            "-m",
            "ctrl",
            "-s",
            "1000",
            "-k",
            "Left",
        ],
        1500,
    );
    poll(Duration::from_secs(5), "Left's LayerSet", || {
        (saved(&nested)? < 1.0).then_some(())
    });
    assert_eq!(saved(&nested), Some(0.99), "the first Left");
    let local = nested.window(0).local();
    assert_eq!(local["focusPath"][0], "inspector-opacity-slider", "{local}");
    poll(Duration::from_secs(5), "Ctrl+Z", || {
        (saved(&nested)? == 1.0).then_some(())
    });
    poll(Duration::from_secs(5), "the second Left's LayerSet", || {
        (saved(&nested)? < 1.0).then_some(())
    });
    held.wait();
    nested.window(0).wait_idle();
    assert_eq!(
        saved(&nested),
        Some(0.99),
        "the second Left went on from 100, not from a stale 99"
    );
    assert_eq!(opacity(&mut nested).as_deref(), Some("99"));
    assert_clean(nested.window(0));
}

// ------------------------------------------------------------------ S13

/// L2's box on the 1:2 Live preview at rest (BRIEF §3.1: (1500, 760, 384,
/// 288) from (128, 64)).
const L2_BOX: [f64; 4] = [878.0, 444.0, 192.0, 144.0];

/// S13's panel: Live, L2 selected and dragged on the canvas until its left
/// edge snaps to the right third: the inspector reads the drag's x 1280
/// before anything is sent (`LocalUi.layer_drag`, WP4's), then the one
/// `LayerSet` lands; the L1 row hovered (its grip, eye and lock show).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s13_a_selected_layer_and_a_hovered_row() {
    let mut nested = Nested::start(AREA, "s13", Options::default());
    let preset = ui(|u| u.selected_layer = Some(LayerId::numbered(2)));
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    let before = history_cursor(&nested, &project);
    let (cx, cy) = center(L2_BOX);
    let pointer = nested.pointer();
    pointer.move_to(cx, cy);
    pointer.button(BUTTON_LEFT, true);
    // 107 px left: 3 px from the right third, which it snaps to.
    for i in 1..=12 {
        pointer.move_to(cx - 107.0 * f64::from(i) / 12.0, cy);
        thread::sleep(Duration::from_millis(16));
    }
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(gui.local()["drag"], "layerMove");
    let fields = gui.entries("inspector");
    assert_eq!(
        fields.get("inspector-x").map(String::as_str),
        Some("1280"),
        "the inspector follows the drag: {fields:?}"
    );
    assert_eq!(
        history_cursor(&nested, &project),
        before,
        "nothing sent yet"
    );
    let shot = nested.screenshot("dragging");
    crop(&nested, &shot, PANEL, "panel");
    nested.pointer().button(BUTTON_LEFT, false);
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(layer(&nested, &project, None, "L2")["rect"]["x"], 1280);
    assert_eq!(history_cursor(&nested, &project), before + 1, "one entry");
    let row = region(nested.window(0), "layer-row-L1");
    let (x, y) = center(row);
    nested.pointer().move_to(x, y);
    let gui = nested.window(0);
    gui.wait_idle();
    assert_has(&panel_labels(gui), &["Face · L2"]);
    assert_eq!(
        gui.entries("inspector")
            .get("inspector-x")
            .map(String::as_str),
        Some("1280")
    );
    assert_clean(gui);
    let shot = nested.screenshot("layer-editing");
    crop(&nested, &shot, PANEL, "panel");
    compare(&nested, &shot, "S13-layer-editing.png", PANEL, PANEL);
}

// ----------------------------------------------------------------- S13b

/// The S13b frame: the panel alone, 384×920.
const S13B: &str = "S13b-onion-layers-panel-states.png";

/// S13b's states, each produced for real and cropped: hidden and locked
/// (the CLI), renaming (the overflow's Rename), dragging (a held drag, then
/// `Esc`), focus (the keyboard), disabled (a recording), clipping (a pinned
/// level); the portal's refusal and a take's missing source in the next
/// cases.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s13b_row_states() {
    let mut nested = Nested::start(AREA, "s13b", Options::default());
    let project = fixture(&nested, DesignVariant::Base, &ui(|_| {}));
    open(
        &mut nested,
        &project,
        levels("alsa_input.fake=-3/0,alsa_output.fake.monitor=-52/-48"),
    );
    let row_shot = |nested: &mut Nested, layer: &str, name: &str| {
        let rect = region(nested.window(0), &format!("layer-row-{layer}"));
        let shot = nested.screenshot(name);
        crop(nested, &shot, rect, "row");
        shot
    };

    // Hidden (eye-off, the name muted), then locked too.
    cli_ok(
        &nested,
        &project,
        &["scene", "layer", "set", "L2", "--visible", "false"],
    );
    nested.window(0).wait_idle();
    row_shot(&mut nested, "L2", "hidden");
    cli_ok(
        &nested,
        &project,
        &[
            "scene",
            "layer",
            "set",
            "L2",
            "--visible",
            "true",
            "--locked",
            "true",
        ],
    );
    nested.window(0).wait_idle();
    row_shot(&mut nested, "L2", "locked");
    cli_ok(
        &nested,
        &project,
        &["scene", "layer", "set", "L2", "--locked", "false"],
    );
    nested.window(0).wait_idle();

    // Clipping: the red latch holds until clicked.
    let rect = region(nested.window(0), "audio-row-A1");
    let shot = nested.screenshot("clipping");
    crop(&nested, &shot, rect, "row");
    let gui = nested.window(0);
    assert_has(&panel_labels(gui), &["0.0"]);

    // Focus: the keyboard reaches a row (F6 to the panel, then down).
    for _ in 0..6 {
        if nested.window(0).local()["focus"] == "panel" {
            break;
        }
        nested.keys(&["-k", "F6"]);
        nested.window(0).wait_idle();
    }
    assert_eq!(nested.window(0).local()["focus"], "panel");
    // Down reaches the first row, Down again the list's next row (the
    // list's own arrow keys), which wears the inset ring while the
    // keyboard is attached.
    nested.keys(&["-k", "Down"]);
    nested.window(0).wait_idle();
    let held = nested.keys_held(&["-k", "Down"], 3000);
    poll(Duration::from_secs(10), "the focus on L1's row", || {
        (nested.window(0).local()["focusPath"][0] == "layer-row-L1").then_some(())
    });
    nested.window(0).wait_idle();
    row_shot(&mut nested, "L1", "focus");
    held.wait();

    // Renaming: L2's overflow, Rename; the name becomes a field; a new
    // name then Esc cancels and nothing is sent. One `wtype` run keeps its
    // keyboard from Rename to Esc: the window deactivates between runs,
    // which ends an edit.
    let before = history_cursor(&nested, &project);
    let (x, y) = center(region(nested.window(0), "layer-menu-L2"));
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    nested.screenshot("overflow");
    // The pointer leaves the row (no hover reveal in the shot).
    nested.pointer().move_to(1400.0, 940.0);
    let held = nested.keys_held(
        &["-k", "Return", "-s", "2500", "Webcam", "-k", "Escape"],
        500,
    );
    // Rename (the menu's first item) makes the name a field.
    poll(Duration::from_secs(10), "L2's name as a field", || {
        let fields = nested.window(0).entries("layer-row-L2");
        (fields.get("layer-row-L2").map(String::as_str) == Some("Face")).then_some(())
    });
    nested.window(0).wait_idle();
    row_shot(&mut nested, "L2", "renaming");
    // The field reaches a screen reader as a named text box (WP10).
    let a11y = nested.window(0).ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "a11y while renaming: {a11y}");
    held.wait();
    let labels = panel_labels(nested.window(0));
    assert_has(&labels, &["Face"]);
    assert!(!labels.iter().any(|l| l == "Webcam"), "{labels:#?}");
    assert_eq!(
        history_cursor(&nested, &project),
        before,
        "Esc cancels the rename"
    );

    // Dragging: L1's row lifted over the drop line above L2; Esc cancels.
    let l1 = region(nested.window(0), "layer-row-L1");
    let (x, y) = center(l1);
    nested.pointer().move_to(x, y);
    nested.pointer().button(BUTTON_LEFT, true);
    for dy in [4.0, 12.0, 24.0, 36.0, 48.0, 60.0] {
        nested.pointer().move_to(x, y - dy);
        thread::sleep(Duration::from_millis(16));
    }
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(gui.local()["drag"], "layerReorder");
    let shot = nested.screenshot("dragging");
    crop(&nested, &shot, [1216.0, 120.0, 384.0, 104.0], "rows");
    nested.keys(&["-k", "Escape"]);
    nested.pointer().button(BUTTON_LEFT, false);
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(gui.local()["drag"], Value::Null);
    assert_eq!(
        history_cursor(&nested, &project),
        before,
        "Esc cancels the drag"
    );

    // Disabled while recording (0.45); the meters stay live.
    cli_ok(&nested, &project, &["record", "start"]);
    let deadline = Instant::now() + Duration::from_secs(30);
    while nested.cli_json(&project, &["status"])["data"]["runtime"]["record"]["phase"]
        != "recording"
    {
        assert!(Instant::now() < deadline, "never recording");
        thread::sleep(Duration::from_millis(100));
    }
    nested.window(0).wait_idle();
    let shot = nested.screenshot("recording");
    crop(&nested, &shot, PANEL, "panel");
    cli_ok(&nested, &project, &["record", "stop"]);
    let deadline = Instant::now() + Duration::from_secs(30);
    while nested.cli_json(&project, &["status"])["data"]["runtime"]["record"]["phase"] != "idle" {
        assert!(Instant::now() < deadline, "never idle again");
        thread::sleep(Duration::from_millis(100));
    }
    nested.window(0).wait_idle();

    // Renaming with Enter saves the new name (LayerSet{name}).
    let (x, y) = center(region(nested.window(0), "layer-menu-L2"));
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    let held = nested.keys_held(
        &[
            "-k", "Return", "-s", "300", "-M", "ctrl", "-k", "a", "-m", "ctrl", "Webcam", "-k",
            "Return",
        ],
        300,
    );
    held.wait();
    wait_for_label(nested.window(0), "Webcam");
    assert_eq!(layer(&nested, &project, None, "L2")["name"], "Webcam");
    assert_clean(nested.window(0));
}

/// S13b's error row: the portal refused the fake screens
/// (`QREC_FAKE_PORTAL=deny`), so L1 reads "Access denied by the desktop
/// portal" with the record-text icon.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s13b_a_refused_screen() {
    let mut nested = Nested::start(AREA, "s13b-error", Options::default());
    let project = fixture(&nested, DesignVariant::Base, &ui(|_| {}));
    let gui = open(
        &mut nested,
        &project,
        levels(DESIGN_LEVELS).with_env("QREC_FAKE_PORTAL", "deny"),
    );
    let labels = wait_for_label(gui, ACCESS_DENIED);
    assert_has(&labels, &["Main screen", ACCESS_DENIED]);
    let rect = region(nested.window(0), "layer-row-L1");
    let shot = nested.screenshot("error");
    crop(&nested, &shot, rect, "row");
    assert_clean(nested.window(0));
}

/// S13b's missing row: take 5 has no L2 file (`MissingL2`); with c2d0
/// selected the panel edits take 5, whose L2 reads "Missing in this take".
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s13b_a_source_missing_in_a_take() {
    let mut nested = Nested::start(AREA, "s13b-missing", Options::default());
    let preset = ui(|u| {
        u.canvas_mode = CanvasMode::Program;
        u.selection = vec![clip("c2d0")];
        u.playhead = 2437;
    });
    let project = fixture(&nested, DesignVariant::MissingL2, &preset);
    let gui = open(&mut nested, &project, levels(DESIGN_LEVELS));
    assert_has(&panel_labels(gui), &["Face", MISSING_IN_TAKE]);
    let rect = region(gui, "layer-row-L2");
    let shot = nested.screenshot("missing");
    crop(&nested, &shot, rect, "row");
    compare(
        &nested,
        &shot,
        S13B,
        [1216.0, 48.0, 384.0, 920.0],
        [0.0, 0.0, 384.0, 920.0],
    );
    assert_clean(nested.window(0));
}

// ------------------------------------------------------------------ S12

/// S12: `Z` then `Shift+C` on c3e9 opens the autocut page; its first dry
/// run reads like the frame (four regions, 6.2 s); a region click seeks
/// and marks the row; `Esc` closes the page and sends nothing; reopened,
/// Apply cuts (D-15) and the page closes.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s12_the_autocut_page() {
    let mut nested = Nested::start(AREA, "s12", Options::default());
    let preset = ui(|u| {
        u.canvas_mode = CanvasMode::Program;
        u.selection = vec![clip("c3e9")];
        u.playhead = 4842;
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    let before = history_cursor(&nested, &project);
    nested.keys(&["-k", "z"]);
    nested.keys(&["-M", "shift", "-k", "c", "-m", "shift"]);
    let gui = nested.window(0);
    let labels = wait_for_label(gui, "Removes 4 silences");
    assert_has(
        &labels,
        &[
            "Autocut",
            "Previewing take 8 · c3e9",
            "Detect silence on",
            "Threshold",
            "Min silence",
            "Padding",
            "Min keep",
            "(6.2 s)",
            "Take 8",
            "00:01:43:15",
            "00:01:37:09",
            "Timeline",
            "00:04:06:11",
            "00:04:00:05",
            "00:02:41:12 → 00:02:43:00",
            "48 f · 1.6 s",
            "00:03:51:18 → 00:03:53:15",
            "57 f · 1.9 s",
            "Run autocut after every take",
            autocut::NOTE,
            "Cancel",
            "Apply autocut",
        ],
    );
    assert_eq!(gui.local()["panics"], 0);
    // A region click seeks and marks its row.
    nested.cli_json(&project, &["seek", "--to", "0f"]);
    let (x, y) = center(region(nested.window(0), "autocut-region-1"));
    nested.pointer().click(x, y);
    let gui = nested.window(0);
    gui.wait_idle();
    let status = nested.cli_json(&project, &["status"]);
    assert_eq!(status["data"]["ui"]["playhead"], 4842, "{status}");
    // The pointer off the row (over the hint bar, which shows no hover): the
    // frame draws the clicked row selected, not hovered too (WP10's shot
    // review: `#374524` for its `#2E3E1B`).
    nested.pointer().move_to(1500.0, 984.0);
    nested.window(0).wait_idle();
    assert_clean(nested.window(0));
    let shot = nested.screenshot("page");
    crop(&nested, &shot, PANEL, "panel");
    compare(&nested, &shot, "S12-autocut-preview.png", PANEL, PANEL);
    // WP10's shot review: the clicked region row is selected, not hovered
    // too (S12's `#2E3E1B`; with the pointer on it `#374524`), and the
    // selected clip's stroke stays `accent` inside the regions (the
    // autocut dim covered it: `#496422`).
    let px = |x: f64, y: f64| {
        let (dx, dy) = nested.opts.pixel_px(x, y);
        gui_support::rgb_pixels(&shot, [dx, dy, 1, 1])[0]
    };
    let row = px(1300.0, 650.0);
    assert!(
        row.iter()
            .zip([0x2E, 0x3E, 0x1B])
            .all(|(a, b)| a.abs_diff(b) <= 2),
        "the clicked region row, selected only: {row:02X?}"
    );
    assert_eq!(
        px(610.0, 720.0),
        gui_support::token_rgb("swamp-night", "accent"),
        "c3e9's stroke inside a region"
    );
    // The summary's heading 8 px under the knobs' end labels, on S12's
    // baseline: its title's ink from y 563 (its glyphs a pixel shorter than
    // the frame's, from 562; it sat a pixel lower, from 564), its rows' from
    // 587. The footer's Esc cap is a medium one, its `keycap` fill 17 px
    // tall (20 with the stroke; the small one's was 13). WP10's shot
    // review.
    let surface = px(1300.0, 552.0);
    let heading = ink_top(&nested, &shot, [1228.0, 550.0, 154.0, 30.0], surface);
    assert!(
        (heading - 563.0).abs() <= 0.75,
        "the summary's heading from y {heading}"
    );
    let rows = ink_top(&nested, &shot, [1300.0, 580.0, 190.0, 20.0], surface);
    assert!(
        (rows - 587.0).abs() <= 1.0,
        "the summary's rows from y {rows}"
    );
    let caps = gui_support::keycap_fills(&nested.opts, &shot, [1340.0, 915.0, 100.0, 40.0]);
    assert!(
        caps.len() == 1 && (caps[0][3] - 17.0).abs() <= 1.0,
        "the Esc cap's fill: {caps:?}"
    );
    // A tuning change sends its dry run 250 ms later: one waitIdle covers
    // the debounce (the page holds the io worker busy) and the reply.
    let (x, y) = center(region(nested.window(0), "knob-field-2"));
    nested.pointer().click(x, y);
    nested.keys(&["-M", "ctrl", "-k", "a", "-m", "ctrl", "500", "-k", "Return"]);
    let gui = nested.window(0);
    gui.wait_idle();
    let labels = panel_labels(gui);
    assert!(
        !labels
            .iter()
            .any(|l| l == "(6.2 s)" || l == autocut::ANALYZING),
        "one waitIdle after a change shows its dry run: {labels:#?}"
    );
    // Esc: back to the layers, nothing sent (out of the field first: in a
    // field, Esc is the field's, G§3.8 step 2).
    nested.keys(&["-k", "Tab", "-k", "Escape"]);
    let labels = wait_for_label(nested.window(0), "Onion layers");
    assert!(!labels.iter().any(|l| l == "Apply autocut"), "{labels:#?}");
    assert_eq!(
        history_cursor(&nested, &project),
        before,
        "Cancel sends nothing"
    );
    assert_eq!(clip_count(&nested, &project), 10);
    // Apply: the cut (the settings are unchanged: one entry), the page closes.
    nested.keys(&["-M", "shift", "-k", "c", "-m", "shift"]);
    wait_for_label(nested.window(0), "Removes 4 silences");
    let (x, y) = center(region(nested.window(0), "autocut-apply"));
    nested.pointer().click(x, y);
    let deadline = Instant::now() + Duration::from_secs(30);
    while clip_count(&nested, &project) != 14 {
        assert!(Instant::now() < deadline, "the cut never landed");
        thread::sleep(Duration::from_millis(100));
    }
    let gui = nested.window(0);
    wait_for_label(gui, "Onion layers");
    let toast = gui.labels("toast-host");
    assert!(
        toast
            .iter()
            .any(|t| t.starts_with("Autocut take 8: removed 4 silences")),
        "{toast:?}"
    );
    nested.screenshot("applied");
    assert_clean(nested.window(0));
}

/// The first row (logical y) of `rect` (logical px) of a shot with text on
/// it: a pixel far from `ground` (its channels 100 apart, summed).
fn ink_top(nested: &Nested, png: &Path, rect: [f64; 4], ground: [u8; 3]) -> f64 {
    let [x, y, w, h] = nested.opts.rect_px(rect);
    let pixels = gui_support::rgb_pixels(png, [x, y, w, h]);
    let far = |p: &[u8; 3]| {
        p.iter()
            .zip(ground)
            .map(|(a, b)| u32::from(a.abs_diff(b)))
            .sum::<u32>()
            > 100
    };
    (0..h)
        .find(|&r| (0..w).any(|c| far(&pixels[(r * w + c) as usize])))
        .map_or(f64::NAN, |r| rect[1] + f64::from(r) / nested.opts.scale)
}

/// The window's overlays, as `local` names them.
fn overlays(gui: &Gui) -> Value {
    gui.local()["overlays"].clone()
}

/// R32, R41: the page's "Detect silence on" list is a menu while it is
/// open (`Overlay::Menu`, G§3.6; it said nothing open). Its keys are the
/// list's: `Esc` closes the list and leaves the page and its tuning, and
/// the arrows move in a list the pointer opened, never seeking the
/// timeline (GTK hands them to the list's popover; the window's key
/// router never sees them, so these held before the overlay too).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_track_list_keeps_its_keys() {
    let mut nested = Nested::start(AREA, "track-list", Options::default());
    let preset = ui(|u| {
        u.canvas_mode = CanvasMode::Program;
        u.selection = vec![clip("c3e9")];
        u.playhead = 4842;
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    nested.keys(&["-M", "shift", "-k", "c", "-m", "shift"]);
    wait_for_label(nested.window(0), "Removes 4 silences");
    // A tuning: Padding 500 (Enter, then out of the field).
    let (x, y) = center(region(nested.window(0), "knob-field-2"));
    nested.pointer().click(x, y);
    nested.keys(&[
        "-M", "ctrl", "-k", "a", "-m", "ctrl", "500", "-k", "Return", "-k", "Tab",
    ]);
    nested.window(0).wait_idle();
    let padding = |nested: &mut Nested| {
        let fields = nested.window(0).entries("autocut-page");
        fields.get("knob-field-2").cloned()
    };
    assert_eq!(padding(&mut nested).as_deref(), Some("500"));
    let list = center(region(nested.window(0), "autocut-track"));
    let open_list = |nested: &mut Nested| {
        nested.pointer().click(list.0, list.1);
        poll(Duration::from_secs(5), "the list open as a menu", || {
            (overlays(nested.window(0)) == json!(["Menu"])).then_some(())
        });
    };
    let closed = |nested: &mut Nested, what: &str| {
        poll(Duration::from_secs(5), what, || {
            (overlays(nested.window(0)) == json!([])).then_some(())
        });
        nested.window(0).wait_idle();
    };
    // Esc closes the list only.
    open_list(&mut nested);
    nested.keys(&["-k", "Escape"]);
    closed(&mut nested, "the list closed by Esc");
    let local = nested.window(0).local();
    assert_eq!(local["panelPage"], "autocut", "the page stays: {local}");
    assert_has(&panel_labels(nested.window(0)), &["Apply autocut"]);
    assert_eq!(
        padding(&mut nested).as_deref(),
        Some("500"),
        "the tuning stays"
    );
    // Down and Enter pick A2 in the list; the playhead stays.
    open_list(&mut nested);
    nested.keys(&["-k", "Down", "-k", "Return"]);
    closed(&mut nested, "the list closed by its pick");
    let status = nested.cli_json(&project, &["status"]);
    assert_eq!(status["data"]["ui"]["playhead"], 4842, "no seek: {status}");
    let labels = panel_labels(nested.window(0));
    assert!(
        labels.iter().any(|l| l.starts_with("A2 \u{b7} ")),
        "A2 picked: {labels:#?}"
    );
    // The list closed, Esc is the page's Cancel.
    nested.keys(&["-k", "Escape"]);
    wait_for_label(nested.window(0), "Onion layers");
    assert_clean(nested.window(0));
}

/// R39: in a Narrow window the autocut page shows in the layers sheet and
/// goes with it. `Esc` (the sheet's close) and the header's layers button
/// close the page too, its preview with it, and `Shift+C` opens it again;
/// its Cancel takes away the sheet it opened, not one opened before it.
/// (`Esc` used to hide the sheet and leave the page open, its hatches
/// drawn, and the next `Shift+C` closed the hidden page.)
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_autocut_page_goes_with_the_narrow_sheet() {
    let mut nested = Nested::start(AREA, "narrow-autocut", Options::size(1000, 1000));
    let preset = ui(|u| {
        u.canvas_mode = CanvasMode::Program;
        u.selection = vec![clip("c3e9")];
        u.playhead = 4842;
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    assert_eq!(nested.window(0).local()["layout"]["width"], "Narrow");
    // (the page, the overlays, whether the timeline draws a preview)
    let page = |nested: &mut Nested| {
        let local = nested.window(0).local();
        (
            local["panelPage"].as_str().unwrap_or_default().to_owned(),
            local["overlays"].clone(),
            !local["autocutPreview"].is_null(),
        )
    };
    let shown = ("autocut".to_owned(), json!(["LayersSheet"]), true);
    let gone = ("layers".to_owned(), json!([]), false);
    let settle = |nested: &mut Nested, want: &(String, Value, bool), what: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            nested.window(0).wait_idle();
            let now = page(nested);
            if now == *want {
                return;
            }
            assert!(Instant::now() < deadline, "{what}: {now:?}, not {want:?}");
            thread::sleep(Duration::from_millis(100));
        }
    };
    let shift_c = |nested: &mut Nested| {
        nested.keys(&["-M", "shift", "-k", "c", "-m", "shift"]);
        wait_for_label(nested.window(0), "Removes 4 silences");
    };
    shift_c(&mut nested);
    settle(&mut nested, &shown, "Shift+C: the page in the sheet");
    // Esc closes the sheet, and the page with it.
    nested.keys(&["-k", "Escape"]);
    settle(&mut nested, &gone, "Esc: the sheet and the page go");
    // Shift+C opens it again (it used to close the hidden page).
    shift_c(&mut nested);
    settle(&mut nested, &shown, "Shift+C again");
    // The header's layers button closes the sheet: the page goes too.
    let layers = center(region(nested.window(0), "layers-button"));
    nested.pointer().click(layers.0, layers.1);
    settle(
        &mut nested,
        &gone,
        "the layers button: the sheet and the page go",
    );
    // The page's Cancel takes away the sheet it opened.
    shift_c(&mut nested);
    settle(&mut nested, &shown, "Shift+C a third time");
    let (x, y) = center(region(nested.window(0), "autocut-cancel"));
    nested.pointer().click(x, y);
    settle(&mut nested, &gone, "Cancel: the sheet it opened goes");
    // A sheet opened before the page stays after its Cancel, on the layers.
    nested.pointer().click(layers.0, layers.1);
    let layers_page = ("layers".to_owned(), json!(["LayersSheet"]), false);
    settle(
        &mut nested,
        &layers_page,
        "the layers button opens the sheet",
    );
    let reply = nested
        .window(0)
        .ctl(json!({"op": "action", "id": "autocut.page"}));
    assert_eq!(reply["ok"], true, "{reply}");
    wait_for_label(nested.window(0), "Removes 4 silences");
    settle(&mut nested, &shown, "the page in the open sheet");
    let (x, y) = center(region(nested.window(0), "autocut-cancel"));
    nested.pointer().click(x, y);
    settle(
        &mut nested,
        &layers_page,
        "Cancel: the sheet opened before stays",
    );
    wait_for_label(nested.window(0), "Onion layers");
    // The window widens under the open page (it docks), then narrows
    // again: the sheet shows it (it used to stay open behind the hidden
    // panel). Keys only from here: the pointer knows the first output.
    nested.keys(&["-k", "Escape"]);
    settle(&mut nested, &gone, "Esc: the sheet goes");
    shift_c(&mut nested);
    settle(&mut nested, &shown, "Shift+C a fourth time");
    let mode = |nested: &mut Nested, width: f64| {
        let s = nested.opts.scale;
        let mode = format!("{}x{}@60Hz", (width * s).round(), (1000.0 * s).round());
        nested.swaymsg(&["output", OUTPUT, "mode", &mode]);
        poll(Duration::from_secs(10), "the window resized", || {
            let rect = nested.window_rect()?;
            (rect[2] == width).then_some(())
        });
    };
    mode(&mut nested, 1600.0);
    let docked = ("autocut".to_owned(), json!([]), true);
    settle(&mut nested, &docked, "Wide: the page docked");
    assert_eq!(nested.window(0).local()["layout"]["width"], "Wide");
    mode(&mut nested, 1000.0);
    settle(&mut nested, &shown, "Narrow again: the page in the sheet");
    nested.keys(&["-k", "Escape"]);
    settle(&mut nested, &gone, "Esc: the sheet and the page go");
    assert_clean(nested.window(0));
}

/// The autocut preview's spans (from, to) as `local` reports them.
fn preview_spans(gui: &Gui) -> Vec<(u64, u64)> {
    let local = gui.local();
    local["autocutPreview"]["spans"]
        .as_array()
        .map(|spans| {
            spans
                .iter()
                .filter_map(|s| Some((s["from"].as_u64()?, s["to"].as_u64()?)))
                .collect()
        })
        .unwrap_or_default()
}

/// R40, R76: the page's preview follows the timeline, not only the
/// selection and the knobs. A trim of c2d1 (before c3e9: the scope stays
/// c3e9) moves c3e9's hatches and regions 30 frames earlier and the
/// Timeline row 30 frames shorter; its undo moves them back; c3e9 trimmed
/// to before its first silence leaves nothing to cut. (No dry run went:
/// the page kept the first plan's frames, past the trim.)
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_autocut_preview_follows_the_timeline() {
    let mut nested = Nested::start(AREA, "autocut-follows", Options::default());
    let preset = ui(|u| {
        u.canvas_mode = CanvasMode::Program;
        u.selection = vec![clip("c3e9")];
        u.playhead = 4842;
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    open(&mut nested, &project, levels(DESIGN_LEVELS));
    nested.keys(&["-M", "shift", "-k", "c", "-m", "shift"]);
    wait_for_label(nested.window(0), "Removes 4 silences");
    let s12 = vec![(4842, 4890), (5570, 5612), (6276, 6315), (6948, 7005)];
    assert_eq!(preview_spans(nested.window(0)), s12);
    let settle = |nested: &mut Nested, want: &[(u64, u64)], what: &str| {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            nested.window(0).wait_idle();
            let now = preview_spans(nested.window(0));
            if now == want {
                return;
            }
            assert!(Instant::now() < deadline, "{what}: {now:?}, not {want:?}");
            thread::sleep(Duration::from_millis(100));
        }
    };
    // c2d1 ends 30 frames earlier: c3e9 starts at 4256.
    cli_ok(
        &nested,
        &project,
        &["clip", "trim", "c2d1", "--end", "-30f"],
    );
    let earlier: Vec<(u64, u64)> = s12.iter().map(|(a, b)| (a - 30, b - 30)).collect();
    settle(&mut nested, &earlier, "the hatches after c2d1's trim");
    assert_has(
        &wait_for_label(nested.window(0), "Removes 4 silences"),
        &["00:02:40:12 → 00:02:42:00", "00:04:05:11", "00:03:59:05"],
    );
    // Its undo: S12's frames again.
    cli_ok(&nested, &project, &["undo"]);
    settle(&mut nested, &s12, "the hatches after the undo");
    assert_has(
        &wait_for_label(nested.window(0), "Removes 4 silences"),
        &["00:02:41:12 → 00:02:43:00", "00:04:06:11", "00:04:00:05"],
    );
    // c3e9 kept up to its first silence: nothing to cut.
    cli_ok(
        &nested,
        &project,
        &["clip", "trim", "c3e9", "--end", "556f"],
    );
    settle(&mut nested, &[], "no hatches on the trimmed clip");
    let labels = wait_for_label(nested.window(0), autocut::NOTHING_TO_CUT);
    assert!(
        !labels.iter().any(|l| l.starts_with("00:02:41:12")),
        "no region left: {labels:#?}"
    );
    assert_clean(nested.window(0));
}

// ------------------------------------------------------- 1300 wide (Medium)

/// The panel docked at 320 (G§3.15 Medium): the inspector's crop fields
/// 2-up, the meters 232 wide.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_panel_at_1300_wide() {
    let mut nested = Nested::start(AREA, "medium", Options::size(1300, 1000));
    let scale = nested.opts.scale;
    let preset = ui(|u| {
        u.canvas_mode = CanvasMode::Program;
        u.selection = vec![clip("c2d0")];
        u.selected_layer = Some(LayerId::numbered(2));
        u.playhead = 2437;
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    let gui = open(&mut nested, &project, levels(DESIGN_LEVELS));
    let panel = [980.0, 48.0, 320.0, 920.0];
    assert_regions(
        &gui.regions(&[]),
        &[("region-panel", panel)],
        &["region-panel"],
        scale,
    );
    assert_eq!(gui.local()["layout"]["width"], "Medium");
    let rows: Regions = gui.regions(&["layer-row-L2", "audio-row-A1"]);
    assert_regions(
        &rows,
        &[
            ("layer-row-L2", [996.0, 176.0, 288.0, 44.0]),
            ("audio-row-A1", [996.0, 348.0, 288.0, 92.0]),
        ],
        &["layer-row-L2", "audio-row-A1"],
        scale,
    );
    assert_clean(gui);
    let shot = nested.screenshot("panel");
    crop(&nested, &shot, panel, "panel");
}

/// The layers and autocut pages slide (200 ms), and switch at once under
/// reduced motion (D-30; `ui.reduced_motion` in `config.toml`, the only
/// source in test mode). Test mode turns GTK's animations off, so the
/// stack's transition is what the case reads (`local`'s
/// `panelTransition`); the page still opens and closes.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_pages_slide_unless_motion_is_reduced() {
    for reduced in [false, true] {
        let case = if reduced { "reduced-motion" } else { "motion" };
        let mut nested = Nested::start(AREA, case, Options::default());
        if reduced {
            let config = nested.env.config_home().join("qrec");
            std::fs::create_dir_all(&config).expect("the config folder");
            std::fs::write(config.join("config.toml"), "[ui]\nreduced_motion = true\n")
                .expect("config.toml");
        }
        let preset = ui(|u| {
            u.canvas_mode = CanvasMode::Program;
            u.selection = vec![clip("c3e9")];
        });
        let project = fixture(&nested, DesignVariant::Base, &preset);
        let gui = open(&mut nested, &project, Launch::a11y());
        let local = gui.local();
        assert_eq!(local["reducedMotion"], reduced, "{local}");
        let want = if reduced { "none" } else { "slide-left-right" };
        assert_eq!(local["panelTransition"], want, "{local}");
        let reply = gui.ctl(json!({"op": "action", "id": "autocut.page"}));
        assert_eq!(reply["ok"], true, "{reply}");
        wait_for_label(gui, "Removes 4 silences");
        assert_eq!(gui.local()["panelPage"], "autocut");
        let reply = gui.ctl(json!({"op": "action", "id": "autocut.page"}));
        assert_eq!(reply["ok"], true, "{reply}");
        gui.wait_idle();
        let local = gui.local();
        assert_eq!(local["panelPage"], "layers", "{local}");
        assert_eq!(local["panelTransition"], want, "{local}");
        assert_clean(gui);
    }
}

/// R65: a caught panic in a meter's refresh timer (test mode's
/// `QREC_GUI_TEST_PANIC_ONCE`) stops that meter until the next change: the
/// panel's next refresh (a CLI seek's snapshot) starts it again. Unmapped
/// (the autocut page over the layers page) and mapped again, every meter
/// stops and starts with no dead source touched. The meter kept the dead
/// timer's id: it stood still until mapped again, and its unmap removed
/// the dead id (GLib's critical, then a second caught panic, "Failed to
/// remove source").
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_meter_comes_back_after_a_caught_panic() {
    let mut nested = Nested::start(AREA, "meter-panic", Options::default());
    let preset = ui(|u| {
        u.canvas_mode = CanvasMode::Program;
        u.selection = vec![clip("c3e9")];
    });
    let project = fixture(&nested, DesignVariant::Base, &preset);
    let launch = levels(DESIGN_LEVELS).with_env("QREC_GUI_TEST_PANIC_ONCE", "meter: refresh");
    open(&mut nested, &project, launch);
    let ticking = |nested: &mut Nested| {
        let local = nested.window(0).local();
        let n = local["metersTicking"]
            .as_u64()
            .unwrap_or_else(|| panic!("no metersTicking: {local}"));
        (n, local["panics"].as_u64().unwrap_or(0))
    };
    let after = poll(Duration::from_secs(10), "the meter's panic", || {
        let (n, panics) = ticking(&mut nested);
        (panics == 1).then_some(n)
    });
    // A1's and A2's rows: one stopped.
    assert_eq!(after, 1, "one meter of two stopped");
    cli_ok(&nested, &project, &["seek", "--to", "30f"]);
    nested.window(0).wait_idle();
    assert_eq!(ticking(&mut nested), (2, 1), "the next change started it");
    let gui = nested.window(0);
    let reply = gui.ctl(json!({"op": "action", "id": "autocut.page"}));
    assert_eq!(reply["ok"], true, "{reply}");
    wait_for_label(gui, "Removes 4 silences");
    assert_eq!(ticking(&mut nested), (0, 1), "unmapped, none runs");
    let gui = nested.window(0);
    let reply = gui.ctl(json!({"op": "action", "id": "autocut.page"}));
    assert_eq!(reply["ok"], true, "{reply}");
    gui.wait_idle();
    assert_eq!(ticking(&mut nested), (2, 1), "mapped again");
    nested.window(0).assert_no_criticals();
    let a11y = nested.window(0).ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "a11y: {a11y}");
}

// ---------------------------------------------------- not in a compositor

/// The fixture's dry run (BRIEF §3.6), as the CLI returns it, reads like
/// S12's summary through `view::autocut`.
#[test]
fn the_fixtures_dry_run_reads_like_s12() {
    let env = TestEnv::new();
    let dir = env.empty_dir("rust-traits-talk");
    let project = design_fixture(&dir, DesignVariant::Base);
    let out = run_tool(
        env.command(&gui_support::qrec_bin())
            .arg("--project")
            .arg(&dir)
            .args(["autocut", "--clips", "c3e9", "--dry-run", "--json"]),
        gui_support::CLI_TIMEOUT,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last = stdout
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("{}");
    let reply: Value = serde_json::from_str(last).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(reply["ok"], true, "{reply}");
    let plan = serde_json::from_value(reply["data"]["result"].clone()).expect("a plan");
    let mut state = qrec_core::testkit::state(project);
    state.ui.canvas_mode = CanvasMode::Program;
    state.ui.selection = vec![clip("c3e9")];
    let message = reply["data"]["message"].as_str();
    let summary = autocut::summary_with(&plan, &state, message);
    assert_eq!(summary.title, "Removes 4 silences (6.2 s)");
    assert_eq!(
        summary.rows,
        [
            ("Take 8".to_owned(), "00:01:43:15 → 00:01:37:09".to_owned()),
            (
                "Timeline".to_owned(),
                "00:04:06:11 → 00:04:00:05".to_owned()
            ),
        ]
    );
    let regions: Vec<(&str, &str)> = summary
        .regions
        .iter()
        .map(|r| (r.range.as_str(), r.size.as_str()))
        .collect();
    assert_eq!(
        regions,
        [
            ("00:02:41:12 → 00:02:43:00", "48 f · 1.6 s"),
            ("00:03:05:20 → 00:03:07:02", "42 f · 1.4 s"),
            ("00:03:29:06 → 00:03:30:15", "39 f · 1.3 s"),
            ("00:03:51:18 → 00:03:53:15", "57 f · 1.9 s"),
        ]
    );
    let spans = autocut::preview_spans(&plan, &state);
    assert_eq!(
        spans.iter().map(|s| (s.from, s.to)).collect::<Vec<_>>(),
        [(4842, 4890), (5570, 5612), (6276, 6315), (6948, 7005)]
    );
}
