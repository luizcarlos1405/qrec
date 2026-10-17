//! WP10's live settings (docs/GUI.md G§3.14, WP10) in a nested headless
//! sway (G§8.4): `qrec config set theme | ui.scale | ui.reduced_motion`
//! apply to the open window, and the keymap reloads when `keys.toml`
//! changes, with its problems as toasts.
//!
//! Each case changes a setting as a person would (the CLI; an editor's
//! write of `keys.toml`), waits until the window applied it (`local`'s
//! `settingsApplied`, then `waitIdle`) and probes the window: a
//! token-colored pixel per theme, CSS-drawn and custom-drawn, and none of
//! the theme before; libadwaita's color scheme; the regions at the new
//! scale against a window started at it; the `.reduced-motion` class; a
//! rebound key working and the old one not; a conflicting binding's toast.
//!
//! `cargo test -p qrec --test gui_live -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon). Screenshots and
//! logs go to `tmp/gui-shots/wp10-live/`.

mod gui_support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use gui_support::{Gui, Launch, Nested, Options, assert_regions, poll, wait_for_gui_host};
use qrec_core::state::CanvasMode;
use qrec_core::{ClipId, UiState};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use serde_json::{Value, json};

const AREA: &str = "wp10-live";
const OPEN: Duration = Duration::from_secs(60);
const EXIT: Duration = Duration::from_secs(20);
/// How long a change may take to show: its debounce is 200 ms, the CLI's
/// write and the reload come on top (other suites may share the machine).
const APPLY: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------- helpers

/// The design fixture in a folder `name`, with `ui` as its `ui.json`.
fn fixture(nested: &Nested, name: &str, ui: &UiState) -> PathBuf {
    let dir = nested.env.empty_dir(name);
    design_fixture(&dir, DesignVariant::Base);
    write_ui(&dir, ui);
    dir
}

/// S06's preset: Program, c2d0 selected, the playhead at 2437
/// (00:01:21:07).
fn s06() -> UiState {
    UiState {
        canvas_mode: CanvasMode::Program,
        selection: vec![ClipId::from_raw("c2d0")],
        playhead: 2437,
        ..UiState::default()
    }
}

/// S02's: Live, the playhead at the end.
fn s02() -> UiState {
    UiState {
        canvas_mode: CanvasMode::Live,
        playhead: 7391,
        ..UiState::default()
    }
}

/// Launches window `n` (0 first) on `project` and waits until it is open
/// and idle.
fn open(nested: &mut Nested, n: usize, project: &Path, launch: Launch) {
    nested.launch(project, launch);
    wait_for_gui_host(nested, project, OPEN);
    let gui = nested.window(n);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
}

/// `qrec config set <key> <value>`, as a person runs it.
fn config_set(nested: &Nested, project: &Path, key: &str, value: &str) {
    let out = nested.cli(project, &["config", "set", key, value]);
    assert!(
        out.status.success(),
        "qrec config set {key} {value}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The user's config dir (`$XDG_CONFIG_HOME/qrec`), made.
fn config_dir(nested: &Nested) -> PathBuf {
    let dir = nested.env.config_home().join("qrec");
    fs::create_dir_all(&dir).expect("the config folder");
    dir
}

/// Writes `keys.toml` as an editor saves it: a temp file renamed over it.
fn write_keys(nested: &Nested, text: &str) {
    let dir = config_dir(nested);
    let tmp = dir.join(".keys.toml.swp");
    fs::write(&tmp, text).expect("keys.toml's temp file");
    fs::rename(&tmp, dir.join("keys.toml")).expect("keys.toml");
}

/// How many settings changes the window applied so far.
fn applied_count(gui: &Gui) -> u64 {
    gui.local()["settingsApplied"].as_u64().unwrap_or(0)
}

/// `local` once the window applied more settings changes than `before`,
/// has none pending, and drew a frame after (`waitIdle`).
fn applied(gui: &Gui, before: u64) -> Value {
    poll(APPLY, "the settings change applied", || {
        let local = gui.local();
        let n = local["settingsApplied"].as_u64().unwrap_or(0);
        (n > before && local["settingsPending"] == false).then_some(())
    });
    gui.wait_idle();
    gui.local()
}

/// The pixel at (`x`, `y`) of a shot (in its px).
fn pixel(png: &Path, x: f64, y: f64) -> [u8; 3] {
    let px = |v: f64| v.round() as u32;
    gui_support::rgb_pixels(png, [px(x), px(y), 1, 1])[0]
}

/// `rect` (logical px) in a shot's px at output scale `scale`.
fn device(rect: [f64; 4], scale: f64) -> [u32; 4] {
    rect.map(|v| (v * scale).round() as u32)
}

/// A window with no caught panic, no critical, no ERROR line and no CSS
/// error.
fn assert_clean(gui: &Gui) {
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    gui.assert_no_criticals();
    let log = gui.gui_log();
    let errors: Vec<&str> = log.lines().filter(|l| l.contains(" ERROR ")).collect();
    assert!(errors.is_empty(), "ERROR lines: {errors:#?}");
    assert!(!log.contains(" CSS "), "CSS errors:\n{log}");
}

/// The empty a11y walk (launched with `Launch::a11y()`).
fn assert_a11y(gui: &Gui) {
    let a11y = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "{a11y}");
}

/// `qrec status --json`'s data.
fn status(nested: &Nested, project: &Path) -> Value {
    nested.cli_json(project, &["status"])["data"].clone()
}

/// The toast lines of the window's log with title `title`.
fn toasts(gui: &Gui, title: &str) -> Vec<String> {
    let marker = format!("toast: {title} ");
    gui.gui_log()
        .lines()
        .filter_map(|l| l.split_once(&marker).map(|(_, body)| body.to_owned()))
        .collect()
}

// ----------------------------------------------------------------- theme

/// The lanes A1 and A2 (logical px): the timeline's custom-drawn canvas
/// under the playhead, with no video in it.
const LANES: [f64; 4] = [176.0, 800.0, 1040.0, 116.0];

/// `qrec config set theme <t>` for each theme in turn, on S06: libadwaita's
/// color scheme follows (light for Swamp Day only); the CSS takes the
/// theme's tokens (the stage's ground `bg`, the hint bar `surface`); the
/// custom-drawn timeline redraws (its playhead in the theme's `playhead`,
/// none left in the previous theme's); and the window stays clean.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_theme_follows_qrec_config_set() {
    let mut nested = Nested::start(AREA, "theme", Options::default());
    let s = nested.opts.scale;
    let project = fixture(&nested, "rust-traits-talk", &s06());
    open(&mut nested, 0, &project, Launch::a11y());
    let local = nested.window(0).local();
    assert_eq!(local["theme"], "swamp-night", "{local}");
    assert_eq!(local["colorScheme"]["scheme"], "force-dark", "{local}");
    let subscribers = local["themeSubscribers"].clone();
    let mut previous = "swamp-night";
    for theme in ["swamp-day", "dragons-keep", "ogre-mode", "swamp-night"] {
        let gui = nested.window(0);
        let before = applied_count(gui);
        config_set(&nested, &project, "theme", theme);
        let gui = nested.window(0);
        let local = applied(gui, before);
        assert_eq!(local["theme"], theme, "{local}");
        let light = theme == "swamp-day";
        let scheme = if light { "force-light" } else { "force-dark" };
        assert_eq!(local["colorScheme"]["scheme"], scheme, "{theme}: {local}");
        assert_eq!(local["colorScheme"]["dark"], !light, "{theme}: {local}");
        assert_eq!(
            local["themeSubscribers"], subscribers,
            "no widget subscribed twice"
        );
        let shot = nested.screenshot(theme);
        let token = |name: &str| gui_support::token_rgb(theme, name);
        assert_eq!(
            pixel(&shot, 8.0 * s, 600.0 * s),
            token("bg"),
            "{theme}: the stage's ground"
        );
        assert_eq!(
            pixel(&shot, 1596.0 * s, 976.0 * s),
            token("surface"),
            "{theme}: the hint bar"
        );
        // The 1 px playhead crosses both lanes (116 px at scale 1; at a
        // fractional scale the compositor's downscale blends it, so a
        // looser tolerance and half the height).
        let lanes = device(LANES, s);
        let tolerance = if s.fract() == 0.0 { 4 } else { 16 };
        let now = gui_support::count_near(&shot, lanes, token("playhead"), tolerance);
        let old = gui_support::token_rgb(previous, "playhead");
        let stale = gui_support::count_near(&shot, lanes, old, tolerance);
        assert!(now >= (58.0 * s) as usize, "{theme}: {now} playhead pixels");
        assert_eq!(stale, 0, "{theme}: {previous}'s playhead is still drawn");
        previous = theme;
    }
    let gui = nested.window(0);
    assert_a11y(gui);
    assert_clean(gui);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// The empty timeline's art (the quiet swamp, drawn through `gtk::Svg` in
/// the palette's `border` and `muted`) takes the new theme's colors too:
/// nothing of the old `border` is left in it.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_empty_timelines_art_takes_the_new_theme() {
    let mut nested = Nested::start(AREA, "art", Options::default());
    let s = nested.opts.scale;
    let project = nested.env.empty_dir("rust-traits-talk");
    open(&mut nested, 0, &project, Launch::default());
    let art = |nested: &mut Nested| {
        let regions = nested.window(0).regions(&["tl-empty-art"]);
        *regions
            .get("tl-empty-art")
            .unwrap_or_else(|| panic!("no art: {regions:#?}"))
    };
    let rect = device(art(&mut nested), s);
    let shot = nested.screenshot("swamp-night");
    let night = gui_support::token_rgb("swamp-night", "border");
    assert!(
        gui_support::count_near(&shot, rect, night, 2) > 500,
        "the art in Swamp Night's border"
    );
    let gui = nested.window(0);
    let before = applied_count(gui);
    config_set(&nested, &project, "theme", "swamp-day");
    applied(nested.window(0), before);
    let rect = device(art(&mut nested), s);
    let shot = nested.screenshot("swamp-day");
    let day = gui_support::token_rgb("swamp-day", "border");
    let now = gui_support::count_near(&shot, rect, day, 2);
    let stale = gui_support::count_near(&shot, rect, night, 2);
    assert!(now > 500, "{now} pixels of Swamp Day's border");
    assert_eq!(stale, 0, "Swamp Night's border is still drawn");
    assert_clean(nested.window(0));
}

// ----------------------------------------------------------------- scale

/// The regions of the window and the named parts (logical px).
fn regions(gui: &Gui) -> BTreeMap<String, [f64; 4]> {
    gui.regions(&["region-bin"])
}

/// `qrec config set ui.scale 1.5` re-scales the open window as a window
/// started at 1.5 lays out (its regions, ±1 px), and `ui.scale 1` brings
/// back BRIEF's.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn ui_scale_follows_qrec_config_set() {
    let mut nested = Nested::start(AREA, "scale", Options::default());
    let s = nested.opts.scale;
    // The reference: a window started at 1.5.
    let started = fixture(&nested, "started-at-1.5", &s06());
    let config = config_dir(&nested);
    fs::write(config.join("config.toml"), "[ui]\nscale = 1.5\n").expect("config.toml");
    open(&mut nested, 0, &started, Launch::default());
    let reference = regions(nested.window(0));
    assert_eq!(nested.window(0).local()["scale"], 1.5);
    nested.screenshot("started-at-1.5");
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));

    // A window at 1, re-scaled live.
    fs::remove_file(config.join("config.toml")).expect("config.toml");
    let project = fixture(&nested, "rust-traits-talk", &s06());
    open(&mut nested, 1, &project, Launch::default());
    let gui = nested.window(1);
    assert_eq!(gui.local()["scale"], 1.0);
    assert_regions(
        &regions(gui),
        &gui_support::BRIEF_REGIONS,
        &gui_support::MERGED_REGIONS,
        s,
    );
    let before = applied_count(gui);
    config_set(&nested, &project, "ui.scale", "1.5");
    let gui = nested.window(1);
    let local = applied(gui, before);
    assert_eq!(local["scale"], 1.5, "{local}");
    let live = regions(gui);
    let expected: Vec<(&str, [f64; 4])> = reference.iter().map(|(n, r)| (n.as_str(), *r)).collect();
    let names: Vec<&str> = reference.keys().map(String::as_str).collect();
    assert_regions(&live, &expected, &names, s);
    assert_eq!(
        live.keys().collect::<Vec<_>>(),
        names.iter().collect::<Vec<_>>(),
        "the same regions"
    );
    nested.screenshot("live-1.5");

    let gui = nested.window(1);
    let before = applied_count(gui);
    config_set(&nested, &project, "ui.scale", "1");
    let gui = nested.window(1);
    let local = applied(gui, before);
    assert_eq!(local["scale"], 1.0, "{local}");
    assert_regions(
        &regions(gui),
        &gui_support::BRIEF_REGIONS,
        &gui_support::MERGED_REGIONS,
        s,
    );
    nested.screenshot("live-1");
    assert_clean(nested.window(1));
}

// ---------------------------------------------------------------- motion

/// `qrec config set ui.reduced_motion true` reduces the open window's
/// motion: the policy, the window's `.reduced-motion` class and the panel's
/// page transition follow, and `false` brings them back (D-30).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn reduced_motion_follows_qrec_config_set() {
    let mut nested = Nested::start(AREA, "motion", Options::default());
    let project = fixture(&nested, "rust-traits-talk", &s06());
    open(&mut nested, 0, &project, Launch::default());
    let local = nested.window(0).local();
    assert_eq!(local["reducedMotion"], false, "{local}");
    assert_eq!(local["reducedMotionClass"], false, "{local}");
    assert_eq!(local["panelTransition"], "slide-left-right", "{local}");
    for reduced in [true, false] {
        let gui = nested.window(0);
        let before = applied_count(gui);
        config_set(&nested, &project, "ui.reduced_motion", &reduced.to_string());
        let gui = nested.window(0);
        let local = applied(gui, before);
        assert_eq!(local["reducedMotion"], reduced, "{local}");
        assert_eq!(local["reducedMotionClass"], reduced, "{local}");
        let transition = if reduced { "none" } else { "slide-left-right" };
        assert_eq!(local["panelTransition"], transition, "{local}");
        assert_eq!(local["theme"], "swamp-night", "the rest of the look stays");
        assert_eq!(local["scale"], 1.0);
    }
    assert_clean(nested.window(0));
}

/// A burst of writes is one change, debounced while `waitIdle` waits for
/// it (the follower holds an io token from the burst's first event): right
/// after the burst, `waitIdle` comes back with the last write applied.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_burst_of_writes_is_one_change_that_wait_idle_waits_for() {
    let mut nested = Nested::start(AREA, "burst", Options::default());
    let project = fixture(&nested, "rust-traits-talk", &s02());
    open(&mut nested, 0, &project, Launch::default());
    let config = config_dir(&nested);
    let gui = nested.window(0);
    let before = applied_count(gui);
    for theme in ["swamp-day", "dragons-keep", "ogre-mode"] {
        let tmp = config.join(".config.toml.tmp");
        fs::write(&tmp, format!("theme = \"{theme}\"\n")).expect("the temp file");
        fs::rename(&tmp, config.join("config.toml")).expect("config.toml");
    }
    // Well inside the 200 ms debounce: the window heard of the change and
    // hasn't applied it yet; `waitIdle` must wait for it (without the
    // follower's io token it came back at once, with Swamp Night).
    thread::sleep(Duration::from_millis(50));
    gui.wait_idle();
    let local = gui.local();
    assert_eq!(local["theme"], "ogre-mode", "{local}");
    assert_eq!(local["settingsPending"], false, "{local}");
    assert_eq!(
        local["settingsApplied"].as_u64(),
        Some(before + 1),
        "one change for the burst: {local}"
    );
    assert_clean(gui);
}

// ---------------------------------------------------------------- keymap

/// The key caps the window shows for `bin.toggle` (the toolbar's Bin),
/// `frame.save` (the transport's Save frame) and `canvas.mode` (the
/// header's switch).
fn shown_caps(gui: &Gui) -> (Vec<String>, Vec<String>, Vec<String>) {
    let caps = |labels: Vec<String>, names: &[&str]| -> Vec<String> {
        labels
            .into_iter()
            .filter(|l| names.contains(&l.as_str()))
            .collect()
    };
    (
        gui.labels("tl-cap-bin"),
        caps(gui.labels("transport"), &["P", "F7"]),
        caps(gui.labels("region-header"), &["Tab", "F2"]),
    )
}

/// `keys.toml` rebinding `bin.toggle` to F8 (and `frame.save`,
/// `canvas.mode`) while the window is open: F8 toggles the bin, `b` (its
/// old key) does nothing, and the key caps built with the window follow
/// (the toolbar's, the transport's, the header's); removing the file
/// brings the default keys back. No toast for a clean file.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_rebound_key_works_once_keys_toml_changes() {
    let mut nested = Nested::start(AREA, "rebound", Options::default());
    let project = fixture(&nested, "rust-traits-talk", &s02());
    open(&mut nested, 0, &project, Launch::a11y());
    let bin_open = |nested: &Nested| status(nested, &project)["ui"]["binOpen"] == true;
    assert!(!bin_open(&nested));
    let defaults = (
        vec!["B".to_owned()],
        vec!["P".to_owned()],
        vec!["Tab".to_owned()],
    );
    assert_eq!(shown_caps(nested.window(0)), defaults);

    let gui = nested.window(0);
    let before = applied_count(gui);
    write_keys(
        &nested,
        "[bindings]\n\"bin.toggle\" = \"F8\"\n\"frame.save\" = \"F7\"\n\"canvas.mode\" = \"F2\"\n",
    );
    let local = applied(nested.window(0), before);
    assert_eq!(local["keymapProblems"], json!([]), "{local}");
    assert_eq!(
        shown_caps(nested.window(0)),
        (
            vec!["F8".to_owned()],
            vec!["F7".to_owned()],
            vec!["F2".to_owned()]
        ),
        "the caps follow"
    );
    assert_a11y(nested.window(0));
    let keys = nested.cli_json(&project, &["keys"]);
    let bin = keys["data"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == "bin.toggle"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        bin["keys"],
        json!(["F8"]),
        "the CLI reads the same file: {bin}"
    );

    // The old key does nothing now; the new one toggles the bin.
    nested.keys(&["-k", "b"]);
    nested.window(0).wait_idle();
    assert!(!bin_open(&nested), "b is unbound");
    nested.keys(&["-k", "F8"]);
    poll(Duration::from_secs(10), "F8 opening the bin", || {
        bin_open(&nested).then_some(())
    });
    nested.window(0).wait_idle();
    let regions = nested.window(0).regions(&[]);
    assert!(regions.contains_key("region-bin"), "{regions:#?}");
    nested.screenshot("f8-bin");
    nested.keys(&["-k", "F8"]);
    poll(Duration::from_secs(10), "F8 closing the bin", || {
        (!bin_open(&nested)).then_some(())
    });

    // keys.toml gone: the default keys again.
    let gui = nested.window(0);
    let before = applied_count(gui);
    fs::remove_file(config_dir(&nested).join("keys.toml")).expect("keys.toml");
    applied(nested.window(0), before);
    assert_eq!(shown_caps(nested.window(0)), defaults);
    nested.keys(&["-k", "b"]);
    poll(Duration::from_secs(10), "b opening the bin", || {
        bin_open(&nested).then_some(())
    });
    let gui = nested.window(0);
    assert!(
        toasts(gui, "keys.toml has a problem").is_empty(),
        "no toast for a clean keys.toml"
    );
    assert_clean(gui);
}

/// A `keys.toml` with a conflict (`x` for autocut too) and an unknown id,
/// written while the window is open: one toast per problem, worded as the
/// core reports them, the conflicting key runs neither binding; an edit
/// that keeps the conflict and fixes the id toasts nothing new.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_conflicting_binding_toasts_once_keys_toml_changes() {
    let mut nested = Nested::start(AREA, "conflict", Options::default());
    let project = fixture(&nested, "rust-traits-talk", &s06());
    open(&mut nested, 0, &project, Launch::a11y());
    let clips = |nested: &Nested| {
        nested.cli_json(&project, &["clip", "list"])["data"]
            .as_array()
            .map_or(0, Vec::len)
    };
    let count = clips(&nested);
    assert!(count > 0);
    let toolbar_has_c = |gui: &Gui| gui.labels("timeline-toolbar").iter().any(|l| l == "C");
    assert!(toolbar_has_c(nested.window(0)), "Autocut's C");

    let gui = nested.window(0);
    let before = applied_count(gui);
    write_keys(
        &nested,
        "[bindings]\n\"autocut.run\" = \"x\"\n\"nope.nothing\" = \"q\"\n",
    );
    let local = applied(nested.window(0), before);
    let problems: Vec<String> = serde_json::from_value(local["keymapProblems"].clone())
        .unwrap_or_else(|e| panic!("{local}: {e}"));
    assert_eq!(
        problems,
        [
            "x is bound to clip.remove and autocut.run in global; neither runs until keys.toml \
             changes one",
            "keys.toml: unknown binding `nope.nothing` (`qrec keys` lists the ids)",
        ],
        "{local}"
    );
    let gui = nested.window(0);
    assert_eq!(
        toasts(gui, "keys.toml has a problem"),
        problems,
        "one toast each"
    );
    // Autocut's button shows no key now: x runs nothing, c is unbound.
    assert!(!toolbar_has_c(gui), "{:?}", gui.labels("timeline-toolbar"));
    // The first shows (the second waits behind it).
    let labels = gui.labels("toast-host");
    assert!(
        labels.iter().any(|l| l == "keys.toml has a problem"),
        "{labels:?}"
    );
    nested.screenshot("conflict-toast");

    // x runs neither clip.remove nor autocut.run.
    nested.keys(&["-k", "x"]);
    nested.window(0).wait_idle();
    assert_eq!(clips(&nested), count, "x removed nothing");
    let jobs = status(&nested, &project)["runtime"]["jobs"].clone();
    assert!(
        jobs.as_object().is_none_or(|j| j.is_empty()),
        "x started no autocut: {jobs}"
    );

    // The conflict kept, the id fixed: a new keymap, no new toast.
    let gui = nested.window(0);
    let before = applied_count(gui);
    write_keys(&nested, "[bindings]\n\"autocut.run\" = \"x\"\n");
    let local = applied(nested.window(0), before);
    assert_eq!(local["keymapProblems"], json!(problems[..1]), "{local}");
    let gui = nested.window(0);
    assert_eq!(
        toasts(gui, "keys.toml has a problem").len(),
        2,
        "no new toast"
    );
    assert_a11y(gui);
    assert_clean(gui);
}
