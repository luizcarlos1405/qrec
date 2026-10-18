//! WP9's cases (docs/GUI.md G§7 WP9, G§8.4) in a nested headless sway: the
//! command palette (S19, S19b) and the rows it runs (D-19's Clear undo
//! history through its confirmation), the cheat sheet (S20, S20b),
//! Settings › Appearance (S21) and › Keyboard (S22: the conflict banner,
//! Q5's inert key, the capture and the edits), the Recording and Autocut
//! pages (not drawn), and S25's toasts, each triggered for real (#8: the
//! fake engine's `QREC_FAKE_CAMERA_STALL_MS` hangs the camera mid-take;
//! #6 from the autocut page's Apply and from `c`).
//!
//! Each screen's shot gets a `-compare.png` next to its frame
//! (`design/screens/`), cropped to the overlay where the window under it
//! differs from the frame's sample (G§7.2); each toast's sits next to its
//! S25 card. The cases also walk the a11y tree, check the regions and the
//! hint bar, the focus traps and `Esc`.
//!
//! `cargo test -p qrec --test gui_overlays -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon). Screenshots and
//! logs go to `tmp/gui-shots/wp9/`.

mod gui_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use gui_support::{HeldKeys, Launch, Nested, Options, assert_regions, poll, wait_for_gui_host};
use qrec_core::ClipId;
use qrec_core::state::{CanvasMode, UiState};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use qrec_shell::testkit::run_tool;
use serde_json::{Value, json};

const AREA: &str = "wp9";
const OPEN: Duration = Duration::from_secs(60);
const EXIT: Duration = Duration::from_secs(20);
/// How long a settings change may take to show (the follower's 200 ms
/// debounce and the write come on top; other suites share the machine).
const APPLY: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------- helpers

/// `design_fixture(variant)` in a folder `name` with `ui` as its `ui.json`.
fn fixture(nested: &Nested, name: &str, variant: DesignVariant, ui: &UiState) -> PathBuf {
    let dir = nested.env.empty_dir(name);
    design_fixture(&dir, variant);
    write_ui(&dir, ui);
    dir
}

/// S06's preset: Program, c2d0 selected, the playhead at 2437.
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

/// Launches the window on `project` and waits until it is open and idle.
fn open(nested: &mut Nested, project: &Path, launch: Launch) {
    nested.launch(project, launch);
    wait_for_gui_host(nested, project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
}

/// `ffmpeg … hstack` of a shot and its frame (G§8.4 check 4), both cropped
/// to `crop` (`w:h:x:y`, logical px) when given.
fn compare(nested: &Nested, shot: &Path, frame: &str, crop: Option<&str>) {
    let png = gui_support::repo().join("design/screens").join(frame);
    if !png.exists() {
        return;
    }
    let suffix = crop.map_or_else(|| "compare".to_owned(), |_| "compare-crop".to_owned());
    let out = shot.with_file_name(format!(
        "{}-{suffix}.png",
        shot.file_stem().unwrap_or_default().to_string_lossy()
    ));
    let s = nested.opts.scale;
    let up = format!("scale=iw*{s}:ih*{s}");
    let filter = match crop {
        Some(c) => {
            let c: Vec<String> = c
                .split(':')
                .map(|v| {
                    nested
                        .opts
                        .px(v.parse().expect("a crop's number"))
                        .to_string()
                })
                .collect();
            let c = c.join(":");
            format!("[0]crop={c}[a];[1]{up},crop={c}[b];[a][b]hstack=inputs=2")
        }
        None => format!("[1]{up}[b];[0][b]hstack=inputs=2"),
    };
    let run = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(shot)
            .arg("-i")
            .arg(&png)
            .args(["-filter_complex", &filter])
            .arg(&out),
        gui_support::TOOL_TIMEOUT,
    );
    assert!(
        run.status.success(),
        "ffmpeg hstack failed for {}: {}",
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
            if keys.is_empty() {
                label.to_owned()
            } else {
                format!("{} {label}", keys.join(" "))
            }
        })
        .collect()
}

/// The overlays `local` reports, as their debug names.
fn overlays(nested: &mut Nested) -> Vec<String> {
    let local = nested.window(0).local();
    local["overlays"]
        .as_array()
        .unwrap_or_else(|| panic!("local: {local}"))
        .iter()
        .filter_map(|o| o.as_str().map(str::to_owned))
        .collect()
}

/// The a11y walk is empty (launched with `Launch::a11y()`).
fn assert_a11y(nested: &mut Nested) {
    let reply = nested.window(0).ctl(json!({"op": "a11y"}));
    assert_eq!(reply["ok"], true, "a11y: {reply}");
}

/// No panic was caught, no critical printed.
fn assert_clean(nested: &mut Nested) {
    let gui = nested.window(0);
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    gui.assert_no_criticals();
}

/// The focus is inside the top layer (`local`'s `focusInTopLayer`).
fn assert_focus_trapped(nested: &mut Nested, what: &str) {
    let local = nested.window(0).local();
    assert_eq!(local["focusInTopLayer"], true, "{what}: {local}");
}

/// A key run (`wtype -k`), then the window idle.
fn key(nested: &mut Nested, key: &str) {
    nested.keys(&["-k", key]);
    nested.window(0).wait_idle();
}

/// Ctrl+`key`, then the window idle.
fn ctrl(nested: &mut Nested, key: &str) {
    nested.keys(&["-M", "ctrl", "-k", key, "-m", "ctrl"]);
    nested.window(0).wait_idle();
}

/// Types `text` while the keyboard stays (`keys_held`: the window is
/// active, so the focus ring and caret draw), until `done` says so.
fn typed_held(nested: &mut Nested, text: &str, hold_ms: u32) -> HeldKeys {
    nested.keys_held(&[text], hold_ms)
}

// ------------------------------------------------------------ the palette

/// S19: `Ctrl+K` over S06, "sp": the palette at (480, 96, 640, ·) over
/// the scrim down to the hint bar, which names the palette's keys; Split
/// clip first and highlighted; S19's six rows with their caps and CLI
/// forms; `Esc` closes it and gives the focus back.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s19_the_palette_finds_sp() {
    let mut nested = Nested::start(AREA, "s19", Options::default());
    let scale = nested.opts.scale;
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, &s06());
    open(&mut nested, &project, Launch::a11y());
    ctrl(&mut nested, "k");
    assert_eq!(overlays(&mut nested), ["Palette"]);
    assert_focus_trapped(&mut nested, "the palette's field");
    let held = typed_held(&mut nested, "sp", 4000);
    let gui = nested.window(0);
    poll(Duration::from_secs(10), "the query typed", || {
        (gui.entries("palette")
            .get("palette-query")
            .map(String::as_str)
            == Some("sp"))
        .then_some(())
    });
    gui.wait_idle();
    let shot = nested.screenshot("sp");
    held.wait();
    let gui = nested.window(0);
    let regions = gui.regions(&[
        "palette",
        "scrim-palette",
        "palette-field",
        "palette-row-clip.split",
        "palette-row-frame.save",
    ]);
    assert_regions(
        &regions,
        &[
            ("scrim-palette", [0.0, 0.0, 1600.0, 968.0]),
            ("palette-field", [497.0, 113.0, 606.0, 40.0]),
            ("palette-row-clip.split", [489.0, 178.0, 622.0, 40.0]),
        ],
        &["scrim-palette", "palette-field", "palette-row-clip.split"],
        scale,
    );
    let [px, py, pw, _] = regions["palette"];
    assert_eq!([px, py, pw], [480.0, 96.0, 640.0], "{regions:?}");
    let labels = gui.labels("palette-rows");
    for want in [
        "Split clip",
        "qrec clip split",
        "Save frame as PNG",
        "qrec frame save",
        "Autocut settings and preview",
        "qrec autocut --dry-run",
        "Toggle snapping",
        "Select previous clip",
        "Seek to previous edit point",
        "qrec seek --edit prev",
    ] {
        assert!(labels.iter().any(|l| l == want), "{want:?} in {labels:?}");
    }
    assert_eq!(
        hints(&mut nested),
        ["[\u{2191}] [\u{2193}] Choose", "[Enter] Run", "[Esc] Close"]
    );
    assert_a11y(&mut nested);
    compare(&nested, &shot, "S19-command-palette.png", None);
    compare(
        &nested,
        &shot,
        "S19-command-palette.png",
        Some("640:340:480:96"),
    );
    key(&mut nested, "Escape");
    assert!(overlays(&mut nested).is_empty());
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// S19b: the empty query over S06 with two clips selected that aren't
/// side by side: "Recent" (rows run this session), then the groups; Record
/// over selection off with S19's reason; 560 px high, the rest scrolls.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s19b_the_empty_query_with_recent() {
    let mut nested = Nested::start(AREA, "s19b", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        &UiState {
            selection: vec![ClipId::from_raw("c3e9"), ClipId::from_raw("c7f3")],
            ..s06()
        },
    );
    open(&mut nested, &project, Launch::a11y());
    // Recent as S19b draws it (running its rows would record a take).
    let seeded = nested.window(0).ctl(json!({
        "op": "paletteRecent",
        "ids": ["clip.split", "autocut.run", "record.toggle"],
    }));
    assert_eq!(seeded["ok"], true, "{seeded}");
    let _ = query_status(&nested, &project);
    ctrl(&mut nested, "k");
    let held = nested.keys_held(&["-k", "Shift_L"], 3000);
    let gui = nested.window(0);
    gui.wait_idle();
    let shot = nested.screenshot("empty-query");
    held.wait();
    let gui = nested.window(0);
    let labels = gui.labels("palette-rows");
    assert_eq!(
        labels.first().map(String::as_str),
        Some("Recent"),
        "{labels:?}"
    );
    assert!(
        labels.contains(&"Needs a contiguous selection".to_owned()),
        "{labels:?}"
    );
    let regions = gui.regions(&["palette"]);
    let [_, _, _, h] = regions["palette"];
    assert!((h - 560.0).abs() <= 1.0, "560 high: {regions:?}");
    compare(&nested, &shot, "S19b-command-palette-empty-query.png", None);
    compare(
        &nested,
        &shot,
        "S19b-command-palette-empty-query.png",
        Some("640:560:480:96"),
    );
    key(&mut nested, "Escape");
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// `qrec status --json`'s data.
fn query_status(nested: &Nested, project: &Path) -> Value {
    nested.cli_json(project, &["status"])["data"].clone()
}

// --------------------------------------------------------- the cheat sheet

/// S20: `?` over S06 with Save frame rebound to F: the sheet at (40, 40,
/// 1520, 920) over `scrim-strong`, three columns, "default P", the
/// footer; `Esc` closes it.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s20_the_cheat_sheet() {
    let mut nested = Nested::start(AREA, "s20", Options::default());
    let scale = nested.opts.scale;
    let (config, launch) = home_config(&nested);
    fs::write(config.join("keys.toml"), "[bindings]\nframe.save = \"f\"\n").expect("keys.toml");
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, &s06());
    open(&mut nested, &project, launch);
    nested.keys(&["?"]);
    let gui = nested.window(0);
    gui.wait_idle();
    let shot = nested.screenshot("sheet");
    compare(&nested, &shot, "S20-cheat-sheet.png", None);
    assert_eq!(overlays(&mut nested), ["CheatSheet"]);
    let regions =
        nested
            .window(0)
            .regions(&["cheatsheet", "scrim-cheatsheet", "cheatsheet-search-field"]);
    assert_regions(
        &regions,
        &[
            ("cheatsheet", [40.0, 40.0, 1520.0, 920.0]),
            ("scrim-cheatsheet", [0.0, 0.0, 1600.0, 1000.0]),
        ],
        &["cheatsheet", "scrim-cheatsheet"],
        scale,
    );
    // The field: 320×32 at y 60, 16 px before Close; S20 has it at x 1086,
    // the Close button's Esc cap is 2 px wider here (whole-pixel advances,
    // G§0's M4 decision), so it sits up to 2 px left.
    let [fx, fy, fw, fh] = regions["cheatsheet-search-field"];
    assert!(
        (fy - 60.0).abs() <= 1.0 && (fw - 320.0).abs() <= 1.0 && (fh - 32.0).abs() <= 1.0,
        "{regions:?}"
    );
    assert!((1083.0..=1087.0).contains(&fx), "{regions:?}");
    let labels = nested.window(0).labels("cheatsheet");
    for want in [
        "Keyboard shortcuts",
        "Toggle record (start / stop)",
        "default P",
        "Copy niri snippet",
        "Shuttle reverse / stop / forward",
        "Nudge trim edge 1 frame (Shift: 10)",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert_a11y(&mut nested);
    assert_focus_trapped(&mut nested, "the sheet's search");
    // S20b: "trim".
    let held = typed_held(&mut nested, "trim", 3000);
    let gui = nested.window(0);
    poll(Duration::from_secs(10), "the search", || {
        (gui.entries("cheatsheet")
            .get("cheatsheet-search-query")
            .map(String::as_str)
            == Some("trim"))
        .then_some(())
    });
    assert!(
        gui.labels("cheatsheet")
            .contains(&"3 shortcuts match \u{201c}trim\u{201d}".to_owned()),
        "S20b's count"
    );
    gui.wait_idle();
    let shot = nested.screenshot("trim");
    held.wait();
    compare(&nested, &shot, "S20b-cheat-sheet-search.png", None);
    // The trap: Tab moves inside the sheet.
    key(&mut nested, "Tab");
    assert_focus_trapped(&mut nested, "Tab in the sheet");
    assert_eq!(overlays(&mut nested), ["CheatSheet"]);
    key(&mut nested, "Escape");
    assert!(overlays(&mut nested).is_empty());
    // The footer's [Ctrl][,]: Settings › Keyboard in its place.
    nested.keys(&["?"]);
    nested.window(0).wait_idle();
    assert_eq!(overlays(&mut nested), ["CheatSheet"]);
    ctrl(&mut nested, "comma");
    assert_eq!(overlays(&mut nested), [settings_overlay("keyboard")]);
    key(&mut nested, "Escape");
    assert!(overlays(&mut nested).is_empty());
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

// ---------------------------------------------------------- Settings' pages

/// The widget named `name`'s `[x, y, w, h]`.
fn rect(nested: &mut Nested, name: &str) -> [f64; 4] {
    let regions = nested.window(0).regions(&[name]);
    *regions
        .get(name)
        .unwrap_or_else(|| panic!("no {name}: {regions:?}"))
}

/// A click in the middle of the widget named `name`, then the window idle.
fn click(nested: &mut Nested, name: &str) {
    let [x, y, w, h] = rect(nested, name);
    nested.pointer().click(x + w / 2.0, y + h / 2.0);
    nested.window(0).wait_idle();
}

/// The pointer over the scrim at the left, so nothing shows its hover.
fn away(nested: &mut Nested) {
    nested.pointer().move_to(24.0, 600.0);
    nested.window(0).wait_idle();
}

/// `~/.config/qrec` (made) and a launch whose `XDG_CONFIG_HOME` is
/// `~/.config`: the footers read "~/.config/qrec/config.toml", as S21 and
/// S22 draw them.
fn home_config(nested: &Nested) -> (PathBuf, Launch) {
    let config_home = nested.env.home().join(".config");
    let dir = config_home.join("qrec");
    fs::create_dir_all(&dir).expect("~/.config/qrec");
    let launch = Launch::a11y().with_env("XDG_CONFIG_HOME", &config_home.to_string_lossy());
    (dir, launch)
}

/// `page`'s overlay as `local` names it ("Settings(Appearance)").
fn settings_overlay(page: &str) -> String {
    let mut name = page.to_owned();
    if let Some(first) = name.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    format!("Settings({name})")
}

/// Opens Settings (the header's button: Project), then `page` with a click
/// on its tab (the pointer's: no key leaves a focus ring to shoot).
fn open_settings(nested: &mut Nested, page: &str) {
    click(nested, "settings-button");
    let project = [settings_overlay("project")];
    poll(Duration::from_secs(10), "Settings", || {
        (overlays(nested) == project).then_some(())
    });
    if page != "project" {
        click(nested, &format!("settings-tab-{page}"));
        let want = [settings_overlay(page)];
        poll(Duration::from_secs(10), page, || {
            (overlays(nested) == want).then_some(())
        });
    }
    nested.window(0).wait_idle();
}

/// Waits until `config.toml` at `path` sets `key` to `want` (as the core
/// reads it), and returns the file's text.
fn wait_setting(path: &Path, key: &str, want: &Value) -> String {
    poll(
        APPLY,
        &format!("{key} = {want} in {}", path.display()),
        || {
            let text = fs::read_to_string(path).ok()?;
            let config = qrec_core::config::parse(&text).ok()?;
            let got = qrec_core::config::get_json(&config, key).ok()?;
            // 5 and 5.0 are the same setting.
            let same = match (got.as_f64(), want.as_f64()) {
                (Some(a), Some(b)) => a == b,
                _ => got == *want,
            };
            same.then_some(text)
        },
    )
}

/// Waits until `local[key]` is `want`.
fn wait_local(nested: &mut Nested, key: &str, want: &Value) {
    poll(APPLY, &format!("local {key} = {want}"), || {
        (nested.window(0).local()[key] == *want).then_some(())
    });
    nested.window(0).wait_idle();
}

/// Dismisses the toasts shown and queued (`keys.toml`'s problems at start).
fn dismiss_toasts(nested: &mut Nested) {
    for _ in 0..12 {
        if !nested
            .window(0)
            .regions(&["toast-dismiss"])
            .contains_key("toast-dismiss")
        {
            return;
        }
        click(nested, "toast-dismiss");
        std::thread::sleep(Duration::from_millis(400));
    }
    panic!("the toasts keep coming");
}

/// `rect` (logical px) of `shot` is mostly `theme`'s `token`.
fn mostly(nested: &Nested, shot: &Path, rect: [f64; 4], theme: &str, token: &str) -> bool {
    let px = nested.opts.rect_px(rect);
    let n = gui_support::count_near(shot, px, gui_support::token_rgb(theme, token), 3);
    n * 10 >= (px[2] * px[3]) as usize * 9
}

/// S21: Settings › Appearance over S02, `config.toml` holding a comment:
/// the four C32 cards (Swamp Night selected), Interface size at 100 %, the
/// switches off, D-30's caption, the footer naming
/// ~/.config/qrec/config.toml. A click on Swamp Day writes `theme =
/// "swamp-day"` under the comment and the window takes it at once, the
/// dialog still open; 125 % and Reduce motion the same; `Esc` closes it.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s21_appearance_applies_each_pick_at_once() {
    let mut nested = Nested::start(AREA, "s21", Options::default());
    let scale = nested.opts.scale;
    let (config, launch) = home_config(&nested);
    let file = config.join("config.toml");
    fs::write(&file, "# mine\n").expect("config.toml");
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, &s02());
    open(&mut nested, &project, launch);
    open_settings(&mut nested, "appearance");
    assert_focus_trapped(&mut nested, "the dialog");
    away(&mut nested);
    let shot = nested.screenshot("appearance");
    compare(&nested, &shot, "S21-settings-appearance.png", None);
    compare(
        &nested,
        &shot,
        "S21-settings-appearance.png",
        Some("840:640:380:180"),
    );
    let gui = nested.window(0);
    let names = [
        "settings",
        "scrim-settings",
        "settings-tab-appearance",
        "theme-card-swamp-night",
        "theme-card-swamp-day",
        "theme-card-dragons-keep",
        "theme-card-ogre-mode",
        "settings-scale",
        "settings-reduced-motion",
        "settings-sounds",
    ];
    let regions = gui.regions(&names);
    eprintln!("S21 regions: {regions:?}");
    assert_regions(
        &regions,
        &[
            ("settings", [380.0, 180.0, 840.0, 640.0]),
            ("scrim-settings", [0.0, 0.0, 1600.0, 968.0]),
        ],
        &["settings", "scrim-settings"],
        scale,
    );
    // The cards: 180×132, spread over the row.
    let mut xs = Vec::new();
    for card in &names[3..7] {
        let [x, y, w, h] = regions[*card];
        assert_eq!([w, h], [180.0, 132.0], "{card}: {regions:?}");
        assert!((y - 310.0).abs() <= 2.0, "{card} at y {y}");
        xs.push(x);
    }
    let gaps: Vec<f64> = xs.windows(2).map(|p| p[1] - p[0] - 180.0).collect();
    assert!(gaps.iter().all(|g| (g - 18.0).abs() <= 1.0), "{gaps:?}");
    let [_, _, sw, sh] = regions["settings-reduced-motion"];
    assert_eq!([sw, sh], [40.0, 22.0], "C27's switch: {regions:?}");
    let labels = gui.labels("settings");
    for want in [
        "Theme",
        "Changes apply right away. Every theme keeps the same layout and keys.",
        "Interface",
        "Interface size",
        "Scales text, controls and the timeline together.",
        "100%",
        "125%",
        "150%",
        "Reduce motion",
        "Also on when your system asks for less motion.",
        "Sound",
        "Squelch on record start and stop",
        "A short, wet squelch. Nothing else in Qrec makes a sound.",
        "Changes are saved to ",
        "~/.config/qrec/config.toml",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert_eq!(hints(&mut nested), ["[Tab] Move focus", "[Esc] Close"]);
    assert_a11y(&mut nested);

    // Swamp Day, live: the file gets the one line, the window the theme.
    click(&mut nested, "theme-card-swamp-day");
    let text = wait_setting(&file, "theme", &json!("swamp-day"));
    assert!(text.starts_with("# mine\n"), "the comment stays: {text:?}");
    wait_local(&mut nested, "theme", &json!("swamp-day"));
    assert_eq!(overlays(&mut nested), [settings_overlay("appearance")]);
    away(&mut nested);
    let day = nested.screenshot("swamp-day");
    // The dialog's `raised` is Swamp Day's now (the page's right margin).
    assert!(
        mostly(
            &nested,
            &day,
            [1196.0, 640.0, 16.0, 80.0],
            "swamp-day",
            "raised"
        ),
        "Swamp Day's raised in the dialog"
    );

    // 125 %: the scale at once; then 100 % again.
    click(&mut nested, "settings-scale-1.25");
    wait_setting(&file, "ui.scale", &json!(1.25));
    wait_local(&mut nested, "scale", &json!(1.25));
    click(&mut nested, "settings-scale-1");
    wait_local(&mut nested, "scale", &json!(1.0));

    // Reduce motion: the config's (test mode: only the config counts).
    click(&mut nested, "settings-reduced-motion");
    wait_setting(&file, "ui.reduced_motion", &json!(true));
    wait_local(&mut nested, "reducedMotion", &json!(true));
    // The squelch.
    click(&mut nested, "settings-sounds");
    let text = wait_setting(&file, "ui.sounds", &json!(true));
    assert!(text.starts_with("# mine\n"), "{text:?}");

    key(&mut nested, "Escape");
    assert!(overlays(&mut nested).is_empty());
    // Opened again, the page shows the file as it is now.
    open_settings(&mut nested, "appearance");
    assert_eq!(nested.window(0).local()["theme"], "swamp-day");
    key(&mut nested, "Escape");
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// The ids of the timeline's clips in `qrec.json`, in order.
fn clip_ids(project: &Path) -> Vec<String> {
    let text = fs::read_to_string(project.join("qrec.json")).expect("qrec.json");
    let json: Value = serde_json::from_str(&text).expect("qrec.json is JSON");
    json["timeline"]["clips"]
        .as_array()
        .expect("timeline.clips")
        .iter()
        .filter_map(|c| c["id"].as_str().map(str::to_owned))
        .collect()
}

/// Waits until `keys.toml` at `path` gives binding `id` the keys `want`
/// (as the core resolves it), and returns the file's text.
fn wait_binding(path: &Path, id: &str, want: &[&str]) -> String {
    let want: Vec<qrec_core::keymap::KeyChord> = want
        .iter()
        .map(|k| qrec_core::keymap::KeyChord::parse(k).expect("a chord"))
        .collect();
    poll(APPLY, &format!("{id} = {want:?} in keys.toml"), || {
        let text = fs::read_to_string(path).ok()?;
        let map = qrec_core::keymap::resolve(&text);
        (map.binding(id)?.keys == want).then_some(text)
    })
}

/// Waits until the labels under `name` hold `want` (and returns them).
fn wait_labels(nested: &mut Nested, name: &str, want: &str) -> Vec<String> {
    let gui = nested.window(0);
    poll(APPLY, &format!("{want:?} under {name}"), || {
        let labels = gui.labels(name);
        labels.iter().any(|l| l == want).then_some(labels)
    })
}

/// Whether the widget named `name` shows.
fn shows(nested: &mut Nested, name: &str) -> bool {
    nested.window(0).regions(&[name]).contains_key(name)
}

/// S22: Settings › Keyboard over S06 with `keys.toml` binding Autocut to X
/// (Remove clips' key too) and Save frame to F. The banner names the
/// conflict and the file (Open keys.toml); the rows flag X, "default C",
/// "default P". X runs neither binding (Q5): not by key, and neither the
/// palette's rows nor the toolbar's Autocut offer it. Change shortcut
/// captures (S22's capture row): F8 binds Toggle snapping in `keys.toml`,
/// its comment kept, and the window takes it at once (the toolbar's Snap
/// shows F8); Reset puts N back; `Esc` ends a capture and leaves the
/// dialog open; Backspace unbinds; `Esc` in the search clears it first.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s22_keyboard_conflicts_captures_and_edits() {
    let mut nested = Nested::start(AREA, "s22", Options::default());
    let (config, launch) = home_config(&nested);
    let file = config.join("keys.toml");
    fs::write(
        &file,
        "# my keys\n[bindings]\n\"autocut.run\" = \"x\"\n\"frame.save\" = \"f\"\n",
    )
    .expect("keys.toml");
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, &s06());
    open(&mut nested, &project, launch);
    dismiss_toasts(&mut nested);

    // Q5 by key: x runs neither (c2d0 is selected: Remove clips would
    // take it; Autocut would toast).
    let before = clip_ids(&project);
    key(&mut nested, "x");
    std::thread::sleep(Duration::from_millis(800));
    nested.window(0).wait_idle();
    assert_eq!(clip_ids(&project), before, "x removed nothing");
    assert!(!shows(&mut nested, "toast-host"), "x ran nothing");
    // The toolbar's Autocut offers no key: X runs nothing, C isn't its.
    let toolbar = nested.window(0).labels("timeline-toolbar");
    assert!(toolbar.contains(&"Autocut".to_owned()), "{toolbar:?}");
    assert!(
        !toolbar.iter().any(|l| l == "X" || l == "C"),
        "no X or C cap: {toolbar:?}"
    );
    // The palette's Autocut row has no cap either.
    ctrl(&mut nested, "k");
    nested.keys(&["autocut"]);
    let gui = nested.window(0);
    poll(Duration::from_secs(10), "the palette's query", || {
        (gui.entries("palette")
            .get("palette-query")
            .map(String::as_str)
            == Some("autocut"))
        .then_some(())
    });
    let row = gui.labels("palette-row-autocut.run");
    assert!(row.contains(&"Autocut".to_owned()), "{row:?}");
    assert!(!row.iter().any(|l| l == "X" || l == "C"), "no cap: {row:?}");
    key(&mut nested, "Escape");

    open_settings(&mut nested, "keyboard");
    assert_focus_trapped(&mut nested, "the dialog");
    away(&mut nested);
    let shot = nested.screenshot("keyboard");
    compare(&nested, &shot, "S22-settings-keyboard.png", None);
    compare(
        &nested,
        &shot,
        "S22-settings-keyboard.png",
        Some("840:640:380:180"),
    );
    let labels = nested.window(0).labels("settings-keyboard");
    for want in [
        "1 conflict in",
        "~/.config/qrec/keys.toml",
        "X is bound to both Autocut and Remove clips. Neither runs until one is changed.",
        "Open keys.toml",
        "Also bound to Autocut",
        "default C",
        "Also bound to Remove clips",
        "default P",
        "Changes are saved to ",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    let regions = nested
        .window(0)
        .regions(&["keys-banner", "keys-search-field", "keys-open"]);
    eprintln!("S22 regions: {regions:?}");
    // S22: the banner 32 in and 12 under the header, 56 high (the frame's
    // 248–303), the search 8 under it.
    let [bx, by, bw, bh] = regions["keys-banner"];
    assert!(
        (bx - 412.0).abs() <= 1.0 && (by - 248.0).abs() <= 1.0,
        "{regions:?}"
    );
    assert!(
        (bw - 776.0).abs() <= 1.0 && (bh - 56.0).abs() <= 1.0,
        "{regions:?}"
    );
    let [sx, sy, sw, sh] = regions["keys-search-field"];
    assert!(
        (sx - 412.0).abs() <= 1.0 && (sy - (by + bh + 8.0)).abs() <= 1.0,
        "{regions:?}"
    );
    assert_eq!([sw, sh], [776.0, 32.0], "{regions:?}");
    assert_a11y(&mut nested);

    // S22's capture row: Toggle snapping, searched up.
    click(&mut nested, "keys-search-field");
    nested.keys(&["snapping"]);
    let gui = nested.window(0);
    poll(Duration::from_secs(10), "the search", || {
        (gui.entries("settings-keyboard")
            .get("keys-search-query")
            .map(String::as_str)
            == Some("snapping"))
        .then_some(())
    });
    gui.wait_idle();
    click(&mut nested, "keys-change-snapping.toggle");
    poll(Duration::from_secs(10), "the capture", || {
        shows(&mut nested, "keys-capture").then_some(())
    });
    away(&mut nested);
    let shot = nested.screenshot("capture");
    compare(
        &nested,
        &shot,
        "S22-settings-keyboard.png",
        Some("840:640:380:180"),
    );
    assert!(
        nested
            .window(0)
            .labels("keys-row-snapping.toggle")
            .contains(&"Press a key \u{b7} Esc cancels \u{b7} Backspace clears".to_owned()),
        "S22's capture caption"
    );
    // F8 binds it: one line of keys.toml, the comment kept; the window
    // takes the keymap at once.
    key(&mut nested, "F8");
    let text = wait_binding(&file, "snapping.toggle", &["F8"]);
    assert!(text.starts_with("# my keys\n"), "{text:?}");
    let row = wait_labels(&mut nested, "keys-row-snapping.toggle", "F8");
    assert!(row.contains(&"default N".to_owned()), "{row:?}");
    wait_labels(&mut nested, "timeline-toolbar", "F8");
    // Reset: N again, the line gone.
    click(&mut nested, "keys-reset-snapping.toggle");
    let text = wait_binding(&file, "snapping.toggle", &["n"]);
    assert!(!text.contains("snapping"), "{text:?}");
    wait_labels(&mut nested, "keys-row-snapping.toggle", "N");
    // Esc ends a capture; the dialog stays.
    click(&mut nested, "keys-change-snapping.toggle");
    poll(Duration::from_secs(10), "the capture", || {
        shows(&mut nested, "keys-capture").then_some(())
    });
    key(&mut nested, "Escape");
    poll(Duration::from_secs(10), "the capture over", || {
        (!shows(&mut nested, "keys-capture")).then_some(())
    });
    assert_eq!(overlays(&mut nested), [settings_overlay("keyboard")]);
    // The keyboard is back on the row's Change shortcut.
    let local = nested.window(0).local();
    assert_eq!(
        local["focusPath"][0], "keys-change-snapping.toggle",
        "{local}"
    );
    // Backspace unbinds.
    click(&mut nested, "keys-change-snapping.toggle");
    poll(Duration::from_secs(10), "the capture", || {
        shows(&mut nested, "keys-capture").then_some(())
    });
    key(&mut nested, "BackSpace");
    wait_binding(&file, "snapping.toggle", &[]);
    wait_labels(&mut nested, "keys-row-snapping.toggle", "No key");
    click(&mut nested, "keys-reset-snapping.toggle");
    wait_binding(&file, "snapping.toggle", &["n"]);
    // Esc in the search clears it, then closes the dialog.
    click(&mut nested, "keys-search-field");
    key(&mut nested, "Escape");
    let gui = nested.window(0);
    poll(Duration::from_secs(10), "the search cleared", || {
        (gui.entries("settings-keyboard")
            .get("keys-search-query")
            .map(String::as_str)
            == Some(""))
        .then_some(())
    });
    assert_eq!(overlays(&mut nested), [settings_overlay("keyboard")]);
    key(&mut nested, "Escape");
    assert!(overlays(&mut nested).is_empty());
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// The Recording and Autocut pages (not drawn: S21's patterns, G§7.2):
/// their tabs open; each control writes its setting as `qrec config set`
/// does; a refused value says why under its field and leaves the file as
/// it was; `Esc` in an untouched field closes the dialog, in an edited one
/// puts the value back first.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_recording_and_autocut_pages_write_their_settings() {
    let mut nested = Nested::start(AREA, "pages", Options::default());
    let (config, launch) = home_config(&nested);
    let file = config.join("config.toml");
    fs::write(&file, "# mine\n").expect("config.toml");
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, &s02());
    open(&mut nested, &project, launch);
    open_settings(&mut nested, "recording");
    away(&mut nested);
    nested.screenshot("recording");
    let labels = nested.window(0).labels("settings-recording");
    for want in [
        "New projects",
        "Countdown",
        "Seconds before each take starts; 0 starts at once.",
        "Encoder",
        "H.264 encoder",
        "Auto",
        "VA-API",
        "x264",
        "VA-API device",
        "Auto picks the first Intel or AMD render node, never NVIDIA.",
        "~/.config/qrec/config.toml",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert!(
        !labels
            .iter()
            .any(|l| l.contains("NVIDIA") && l.contains("/dev/dri")),
        "no NVIDIA node offered: {labels:?}"
    );
    // `config.toml` sets none: new projects count down 0 s.
    assert_eq!(
        nested
            .window(0)
            .entries("settings-recording")
            .get("settings-countdown")
            .map(String::as_str),
        Some("0")
    );
    assert_a11y(&mut nested);
    // In Countdown, untouched: Esc closes the dialog (the field has nothing
    // to put back).
    click(&mut nested, "settings-countdown");
    assert_focus_trapped(&mut nested, "the countdown");
    key(&mut nested, "Escape");
    assert!(
        overlays(&mut nested).is_empty(),
        "Esc from the field closes it"
    );

    open_settings(&mut nested, "recording");
    click(&mut nested, "settings-countdown");
    nested.keys(&["-M", "ctrl", "-k", "a", "-m", "ctrl", "5", "-k", "Return"]);
    let text = wait_setting(&file, "recording.countdown_secs", &json!(5));
    assert!(text.starts_with("# mine\n"), "{text:?}");
    // Refused: why under the field, the file as it was; Esc puts 5 back.
    nested.keys(&["-M", "ctrl", "-k", "a", "-m", "ctrl", "99", "-k", "Return"]);
    let helper = poll(APPLY, "the field's helper", || {
        let labels = nested.window(0).labels("settings-countdown-helper");
        labels.first().cloned()
    });
    assert!(helper.contains("10"), "why: {helper:?}");
    assert_eq!(fs::read_to_string(&file).expect("config.toml"), text);
    key(&mut nested, "Escape");
    assert_eq!(overlays(&mut nested), [settings_overlay("recording")]);
    assert_eq!(
        nested
            .window(0)
            .entries("settings-recording")
            .get("settings-countdown")
            .map(String::as_str),
        Some("5")
    );
    assert!(!shows(&mut nested, "settings-countdown-helper"));
    // The encoder.
    click(&mut nested, "settings-encoder-x264");
    wait_setting(&file, "encoder.preference", &json!("x264"));

    // Autocut.
    click(&mut nested, "settings-tab-autocut");
    let want = [settings_overlay("autocut")];
    poll(Duration::from_secs(10), "Autocut", || {
        (overlays(&mut nested) == want).then_some(())
    });
    away(&mut nested);
    nested.screenshot("autocut");
    let labels = nested.window(0).labels("settings-autocut");
    for want in [
        "Run autocut after every take",
        "Threshold",
        "dB",
        "Min silence",
        "Padding",
        "Min keep",
        "ms",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    click(&mut nested, "settings-auto-run");
    wait_setting(&file, "autocut.auto_run_after_take", &json!(true));
    click(&mut nested, "settings-autocut-threshold-db");
    // (wtype reads "-40" as an option: the minus is its key.)
    nested.keys(&[
        "-M", "ctrl", "-k", "a", "-m", "ctrl", "-k", "minus", "40", "-k", "Return",
    ]);
    wait_setting(&file, "autocut.threshold_db", &json!(-40.0));
    click(&mut nested, "settings-autocut-min-silence-ms");
    nested.keys(&["-M", "ctrl", "-k", "a", "-m", "ctrl", "500", "-k", "Return"]);
    let text = wait_setting(&file, "autocut.min_silence_ms", &json!(500));
    assert!(text.starts_with("# mine\n"), "{text:?}");
    key(&mut nested, "Escape");
    assert!(overlays(&mut nested).is_empty());
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// The palette runs what it lists (G§4.12): Split clip at S06's playhead
/// (one clip more), Settings (a window action), and D-19's Clear undo
/// history through G§4.9's confirmation, whose Clear history empties the
/// history. `Tab` keeps the focus in the palette; `Esc` closes it.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_palette_runs_its_rows() {
    let mut nested = Nested::start(AREA, "palette-runs", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, &s06());
    open(&mut nested, &project, Launch::a11y());
    let run = |nested: &mut Nested, query: &str, row: &str| {
        ctrl(nested, "k");
        nested.keys(&[query]);
        let gui = nested.window(0);
        poll(Duration::from_secs(10), query, || {
            gui.regions(&[row]).contains_key(row).then_some(())
        });
        gui.wait_idle();
        key(nested, "Return");
    };
    // Split clip: the highlighted first row.
    let before = clip_ids(&project).len();
    run(&mut nested, "split", "palette-row-clip.split");
    poll(APPLY, "the split", || {
        (clip_ids(&project).len() == before + 1).then_some(())
    });
    assert!(
        overlays(&mut nested).is_empty(),
        "the palette closed to run it"
    );
    // A window action: Settings.
    run(&mut nested, "settings", "palette-row-settings.open");
    poll(Duration::from_secs(10), "Settings", || {
        (overlays(&mut nested) == [settings_overlay("project")]).then_some(())
    });
    key(&mut nested, "Escape");
    // The trap: Tab moves inside the palette.
    ctrl(&mut nested, "k");
    key(&mut nested, "Tab");
    assert_focus_trapped(&mut nested, "Tab in the palette");
    assert_eq!(overlays(&mut nested), ["Palette"]);
    key(&mut nested, "Escape");
    assert!(overlays(&mut nested).is_empty());

    // D-19: Clear undo history asks first (G§4.9), then clears.
    let entries = |nested: &Nested| {
        nested.cli_json(&project, &["history"])["data"]
            .as_array()
            .map_or(0, Vec::len)
    };
    assert!(entries(&nested) > 0, "the fixture has a history");
    run(&mut nested, "clear undo", "palette-row-history.clear");
    poll(Duration::from_secs(10), "the confirmation", || {
        overlays(&mut nested)
            .first()
            .is_some_and(|o| o.starts_with("Confirm"))
            .then_some(())
    });
    let labels = nested.window(0).labels("modal");
    for want in ["Clear undo history?", "Clear history", "Cancel"] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("This removes ") && l.contains(".qrec/history.jsonl")),
        "{labels:?}"
    );
    nested.screenshot("clear-history");
    click(&mut nested, "modal-confirm");
    poll(APPLY, "the history cleared", || {
        (entries(&nested) == 0).then_some(())
    });
    assert!(overlays(&mut nested).is_empty());
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

// ------------------------------------------------------------ S25's toasts

/// S25's cards (design/screens/S25-toasts.png, 1600×1000 at 1:1), measured
/// from their 1 px `border` strokes: the frame draws each toast 440 wide in
/// its level's row.
const S25_CARDS: [(&str, [f64; 4]); 11] = [
    ("1-export-done", [196.0, 144.0, 440.0, 114.0]),
    ("4-frame-saved", [656.0, 144.0, 440.0, 98.0]),
    ("6-autocut-applied", [1116.0, 144.0, 440.0, 80.0]),
    ("2-recovered-take", [196.0, 306.0, 440.0, 100.0]),
    ("5-last-take-discarded", [656.0, 306.0, 440.0, 100.0]),
    ("7-nothing-to-discard", [1116.0, 306.0, 440.0, 64.0]),
    ("3-take-too-short", [196.0, 454.0, 440.0, 120.0]),
    ("8-camera-lost", [656.0, 454.0, 440.0, 82.0]),
    ("9-record-blocked-by-export", [1116.0, 454.0, 440.0, 118.0]),
    ("10-invalid-external-edit", [196.0, 622.0, 440.0, 118.0]),
    ("11-recording-didnt-start", [656.0, 622.0, 440.0, 118.0]),
];

/// Waits until a toast titled `title` shows, then for the window to settle;
/// returns the toast's labels.
fn wait_toast(nested: &mut Nested, title: &str) -> Vec<String> {
    let labels = poll(Duration::from_secs(60), title, || {
        let labels = nested.window(0).labels("toast-host");
        labels.iter().any(|l| l == title).then_some(labels)
    });
    nested.window(0).wait_idle();
    labels
}

/// Dismisses the toast shown (its close button) and waits until it went
/// (the next in the queue may take its place at once).
fn dismiss(nested: &mut Nested) {
    let title = nested.window(0).labels("toast-title").first().cloned();
    click(nested, "toast-dismiss");
    poll(Duration::from_secs(10), "the toast gone", || {
        let now = nested.window(0).labels("toast-title").first().cloned();
        (now.is_none() || now != title).then_some(())
    });
}

/// The toast shown, shot: placed as S25 says (centered on 800, its bottom
/// 12 px above the hint bar, 360–560 wide) and next to S25's card `card`
/// (`tmp/gui-shots/wp9/<case>-<card>-compare.png`).
fn shoot_toast(nested: &mut Nested, card: &str) -> PathBuf {
    away(nested);
    let toast = rect(nested, "toast-host");
    let [x, y, w, h] = toast;
    assert!(
        (y + h - 956.0).abs() <= 1.0,
        "{card}: the bottom at {}",
        y + h
    );
    assert!(
        (x + w / 2.0 - 800.0).abs() <= 1.0,
        "{card}: centered: {toast:?}"
    );
    assert!((360.0..=561.0).contains(&w), "{card}: {w} wide");
    let shot = nested.screenshot(card);
    let frame = S25_CARDS
        .iter()
        .find(|(name, _)| *name == card)
        .map(|(_, r)| *r)
        .unwrap_or_else(|| panic!("no S25 card {card}"));
    compare_rects(nested, &shot, toast, frame, "S25-toasts.png");
    shot
}

/// `shot`'s `ours` next to `frame`'s `theirs` (logical px; padded to one
/// height): `<shot>-compare.png`.
fn compare_rects(nested: &Nested, shot: &Path, ours: [f64; 4], theirs: [f64; 4], frame: &str) {
    let png = gui_support::repo().join("design/screens").join(frame);
    let out = shot.with_file_name(format!(
        "{}-compare.png",
        shot.file_stem().unwrap_or_default().to_string_lossy()
    ));
    let s = nested.opts.scale;
    let [ax, ay, aw, ah] = nested.opts.rect_px(ours);
    let [bx, by, bw, bh] = nested.opts.rect_px(theirs);
    let high = ah.max(bh);
    let filter = format!(
        "[0]crop={aw}:{ah}:{ax}:{ay},pad={aw}:{high}:0:0:color=0x12170F[a];\
         [1]scale=iw*{s}:ih*{s},crop={bw}:{bh}:{bx}:{by},pad={bw}:{high}:0:0:color=0x12170F[b];\
         [a][b]hstack=inputs=2"
    );
    let run = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(shot)
            .arg("-i")
            .arg(&png)
            .args(["-filter_complex", &filter])
            .arg(&out),
        gui_support::TOOL_TIMEOUT,
    );
    assert!(run.status.success(), "ffmpeg: {}", run.stderr);
}

/// Asserts `labels` (a toast's) hold each of `want`.
fn assert_toast(labels: &[String], want: &[&str]) {
    for w in want {
        assert!(labels.iter().any(|l| l == w), "{w:?} in {labels:?}");
    }
}

/// S25's editing toasts, each triggered for real over Take 10 Landed (S06,
/// c3e9 selected): #4 `p` (Frame saved: the path, Copy image, Open
/// folder); #6 the autocut page's Apply on take 8 (`Shift+C`: Autocut take
/// 8: removed 4 silences, Undo); #5 `d` (Take 10 moved to the bin, Undo
/// [Ctrl][Z]); #7 `d` again (Nothing to discard: take 10 has no clips);
/// #10 an invalid `qrec.json` written over it (kept the last good state,
/// Copy details).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s25_the_editing_toasts() {
    let mut nested = Nested::start(AREA, "s25-edit", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Take10Landed,
        &UiState {
            selection: vec![ClipId::from_raw("c3e9")],
            ..s06()
        },
    );
    open(&mut nested, &project, Launch::a11y());

    key(&mut nested, "p");
    let labels = wait_toast(&mut nested, "Frame saved");
    assert_toast(&labels, &["Copy image", "Open folder"]);
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("frames/rust-traits-talk-take-") && l.ends_with(".png")),
        "the path: {labels:?}"
    );
    shoot_toast(&mut nested, "4-frame-saved");
    assert_a11y(&mut nested);
    dismiss(&mut nested);

    // The autocut page on c3e9's take (`Shift+C`), its dry run, Apply.
    nested.keys(&["-M", "shift", "-k", "c", "-m", "shift"]);
    poll(Duration::from_secs(120), "the dry run", || {
        let labels = nested.window(0).labels("autocut-page");
        labels
            .iter()
            .any(|l| l.starts_with("Removes 4 silences"))
            .then_some(())
    });
    click(&mut nested, "autocut-apply");
    let title = poll(Duration::from_secs(120), "the autocut toast", || {
        let labels = nested.window(0).labels("toast-host");
        labels
            .iter()
            .find(|l| l.starts_with("Autocut take 8: removed "))
            .cloned()
    });
    nested.window(0).wait_idle();
    eprintln!("S25 #6: {title:?} (S25: \"Autocut take 8: removed 4 silences (6.2 s)\")");
    let labels = nested.window(0).labels("toast-host");
    assert_toast(&labels, &["Undo", "Ctrl", "Z"]);
    shoot_toast(&mut nested, "6-autocut-applied");
    dismiss(&mut nested);

    key(&mut nested, "d");
    let labels = wait_toast(&mut nested, "Take 10 moved to the bin");
    assert_toast(
        &labels,
        &[
            "Its clip left the timeline. Files are kept.",
            "Undo",
            "Ctrl",
            "Z",
        ],
    );
    shoot_toast(&mut nested, "5-last-take-discarded");
    dismiss(&mut nested);

    key(&mut nested, "d");
    let labels = wait_toast(&mut nested, "Nothing to discard");
    assert_toast(&labels, &["Take 10 has no clips on the timeline."]);
    shoot_toast(&mut nested, "7-nothing-to-discard");
    dismiss(&mut nested);

    // #10: clip 4's out past its take's end, written by hand.
    let file = project.join("qrec.json");
    let mut json: Value =
        serde_json::from_str(&fs::read_to_string(&file).expect("qrec.json")).expect("JSON");
    json["timeline"]["clips"][4]["out"] = json!(1400);
    fs::write(&file, serde_json::to_string_pretty(&json).expect("JSON")).expect("qrec.json");
    let labels = wait_toast(&mut nested, "qrec.json changed on disk but isn't valid");
    assert_toast(&labels, &["Copy details"]);
    // The design's clip 4 is take 3's c1b4, as S25's: its words exactly.
    assert_toast(
        &labels,
        &[
            "/timeline/clips/4/out: 1400 is past the end of take 3 (1,377 frames). \
           Keeping the last good state.",
        ],
    );
    shoot_toast(&mut nested, "10-invalid-external-edit");
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// S25 #6 from the key: `c` (`autocut.run`) on S06's c3e9 cuts it as the
/// autocut page's Apply does and says so the same way, "Autocut take 8:
/// removed 4 silences (6.2 s)" with Undo [Ctrl][Z] (the window waits for
/// the run's reply, so the core doesn't toast it: the window's default
/// reply policy does). The toast's Undo puts the clips back.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s25_autocut_from_the_key() {
    let mut nested = Nested::start(AREA, "s25-autocut-key", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Take10Landed,
        &UiState {
            selection: vec![ClipId::from_raw("c3e9")],
            ..s06()
        },
    );
    open(&mut nested, &project, Launch::a11y());
    let before = clip_ids(&project);
    key(&mut nested, "c");
    let title = poll(Duration::from_secs(120), "the autocut toast", || {
        let labels = nested.window(0).labels("toast-host");
        labels
            .iter()
            .find(|l| l.starts_with("Autocut take ") && l.contains(": removed "))
            .cloned()
    });
    nested.window(0).wait_idle();
    eprintln!("S25 #6 from c: {title:?} (S25: \"Autocut take 8: removed 4 silences (6.2 s)\")");
    let labels = nested.window(0).labels("toast-host");
    assert_toast(&labels, &["Undo", "Ctrl", "Z"]);
    let cut = clip_ids(&project);
    assert!(
        cut.len() > before.len(),
        "the cut: {before:?}, then {cut:?}"
    );
    shoot_toast(&mut nested, "6-autocut-applied");
    // Undo from the toast.
    click(&mut nested, "toast-action-0");
    poll(Duration::from_secs(30), "the cut undone", || {
        (clip_ids(&project) == before).then_some(())
    });
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// S25 #8, for real: the fake engine's camera hangs 1 s into take 10
/// (`QREC_FAKE_CAMERA_STALL_MS`, fake backend only), so `STALL_LIMIT` later
/// the take loses L2 Face and records on without it: "Camera disconnected"
/// / "Logitech C920 stopped sending frames. Take 10 keeps recording without
/// L2 Face." (S25's words: the design inventory's camera). The take goes on
/// recording and lands with L2 missing.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s25_a_camera_lost_mid_take() {
    let mut nested = Nested::start(AREA, "s25-camera", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, &s02());
    open(
        &mut nested,
        &project,
        Launch::a11y().with_env("QREC_FAKE_CAMERA_STALL_MS", "1000"),
    );
    dismiss_toasts(&mut nested);
    let started = nested.cli_json(&project, &["record", "start", "--countdown", "0"]);
    assert_eq!(started["ok"], true, "{started}");
    let labels = wait_toast(&mut nested, "Camera disconnected");
    assert_toast(
        &labels,
        &["Logitech C920 stopped sending frames. Take 10 keeps recording without L2 Face."],
    );
    assert_eq!(
        state(&nested, &project)["runtime"]["record"]["phase"],
        "recording",
        "the take records on"
    );
    shoot_toast(&mut nested, "8-camera-lost");
    assert_a11y(&mut nested);
    let stopped = nested.cli_json(&project, &["record", "stop"]);
    assert_eq!(stopped["ok"], true, "{stopped}");
    wait_record_phase(&nested, &project, "idle");
    // Take 10 lands (`qrec.json`, as the Session wrote it) with L2 missing.
    let take = poll(Duration::from_secs(30), "take 10 landed", || {
        let text = fs::read_to_string(project.join("qrec.json")).ok()?;
        let json: Value = serde_json::from_str(&text).ok()?;
        let take = json["takes"]["take-0010"].clone();
        (take["status"] == "ok").then_some(take)
    });
    let face = take["scene"]["layers"]
        .as_array()
        .and_then(|layers| layers.iter().find(|l| l["id"] == "L2"))
        .cloned()
        .unwrap_or_else(|| panic!("L2 in take 10's scene: {take}"));
    assert_eq!(face["missing"], true, "{face}");
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// The project's state as `qrec status --json` gives it.
fn state(nested: &Nested, project: &Path) -> Value {
    nested.cli_json(project, &["status"])["data"].clone()
}

/// Waits until the take's phase (`qrec status`) is `want`.
fn wait_record_phase(nested: &Nested, project: &Path, want: &str) {
    poll(Duration::from_secs(30), &format!("phase {want}"), || {
        (state(nested, project)["runtime"]["record"]["phase"] == want).then_some(())
    });
}

/// S25's recording and export toasts, each triggered for real: #3 a take
/// stopped right after its first frames (That was shorter than Lord
/// Farquaad…, Open bin); #9 `r` while a CLI export of the Smoke fixture
/// runs (Can't record while exporting, Show export [Ctrl][E]); #1 that
/// export done (Layers! Like an onion…, the path · size, Open file, Open
/// folder).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s25_the_recording_and_export_toasts() {
    let mut nested = Nested::start(AREA, "s25-record", Options::default());
    let project = fixture(&nested, "smoke", DesignVariant::Smoke, &s02());
    // At 12 fps: the shortest take the engine makes (a stop waits out the
    // start's 500 ms health window: 0.6 s, 18 frames at 30 fps) is 7
    // frames, under the 10 a take needs (S25's "lasted 7 frames").
    let rate = nested.cli_json(&project, &["project", "set", "--canvas", "@12", "--yes"]);
    assert_eq!(rate["ok"], true, "{rate}");
    open(&mut nested, &project, Launch::a11y());

    // #3: stopped as soon as it starts.
    let started = nested.cli_json(&project, &["record", "start", "--countdown", "0"]);
    assert_eq!(started["ok"], true, "{started}");
    let stopped = nested.cli_json(&project, &["record", "stop"]);
    assert_eq!(stopped["ok"], true, "{stopped}");
    let labels = wait_toast(
        &mut nested,
        "That was shorter than Lord Farquaad. Take moved to the bin.",
    );
    assert_toast(&labels, &["Open bin"]);
    assert!(
        labels
            .iter()
            .any(|l| l.ends_with(" frames. Takes need at least 10.") && l.starts_with("Take ")),
        "the body: {labels:?}"
    );
    shoot_toast(&mut nested, "3-take-too-short");
    dismiss(&mut nested);

    // #9 and #1: an export from the CLI; `r` while it runs.
    let export = nested.cli_background(&project, &["export", "--quality", "draft"]);
    poll(Duration::from_secs(30), "the export running", || {
        let jobs = state(&nested, &project)["runtime"]["jobs"].clone();
        jobs.as_object()
            .is_some_and(|j| !j.is_empty())
            .then_some(())
    });
    key(&mut nested, "r");
    let labels = wait_toast(&mut nested, "Can't record while exporting");
    assert_toast(
        &labels,
        &[
            "Recording works again once the export finishes or you cancel it.",
            "Show export",
            "Ctrl",
            "E",
        ],
    );
    shoot_toast(&mut nested, "9-record-blocked-by-export");
    dismiss(&mut nested);
    let done = gui_support::reap_within(export, Duration::from_secs(180), "the export");
    assert!(done.success(), "the export: {done:?}");
    let labels = wait_toast(&mut nested, "Layers! Like an onion. Your video is ready.");
    assert_toast(&labels, &["Open file", "Open folder"]);
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("exports/rust-traits-talk-") && l.contains(".mp4 \u{b7} ")),
        "the body: {labels:?}"
    );
    shoot_toast(&mut nested, "1-export-done");
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// S25 #2: a take a crash cut short (its recording host killed) is
/// recovered when the window opens the project: "Recovered take 10 from
/// the bog." / "Qrec closed while it was recording. The take is in the
/// bin." / Open bin.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s25_a_recovered_take() {
    let mut nested = Nested::start(AREA, "s25-recovered", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, &s02());
    let started = nested.cli_json(&project, &["record", "start", "--countdown", "0"]);
    assert_eq!(started["ok"], true, "{started}");
    wait_record_phase(&nested, &project, "recording");
    std::thread::sleep(Duration::from_millis(2500));
    let host = nested.cli_json(&project, &["status"]);
    let pid = host["data"]["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("the recording host's pid: {host}"));
    assert_eq!(host["data"]["host"], "headless", "{host}");
    // A crash: nothing is finalized.
    let killed = run_tool(
        Command::new("kill").args(["-KILL", &pid.to_string()]),
        gui_support::TOOL_TIMEOUT,
    );
    assert!(killed.status.success(), "kill: {}", killed.stderr);
    poll(Duration::from_secs(10), "the host gone", || {
        (!Path::new(&format!("/proc/{pid}")).exists()).then_some(())
    });
    open(&mut nested, &project, Launch::a11y());
    let labels = wait_toast(&mut nested, "Recovered take 10 from the bog.");
    assert_toast(
        &labels,
        &[
            "Qrec closed while it was recording. The take is in the bin.",
            "Open bin",
        ],
    );
    shoot_toast(&mut nested, "2-recovered-take");
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// S25 #11 (S24): the desktop refuses the screen (`QREC_FAKE_PORTAL=deny`)
/// and `r`'s take stops before any frames: "Recording didn't start" /
/// "Screen access to FAKE-1 was denied, so take 10 stopped before any
/// frames. It's marked broken in the bin." / Try again [R], Open bin (S25
/// names DP-4: the fake screens keep their own output names).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s25_recording_didnt_start() {
    let mut nested = Nested::start(AREA, "s25-denied", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, &s02());
    open(
        &mut nested,
        &project,
        Launch::a11y().with_env("QREC_FAKE_PORTAL", "deny"),
    );
    dismiss_toasts(&mut nested);
    key(&mut nested, "r");
    let labels = wait_toast(&mut nested, "Recording didn't start");
    assert_toast(
        &labels,
        &[
            "Screen access to FAKE-1 was denied, so take 10 stopped before any frames. \
             It's marked broken in the bin.",
            "Try again",
            "R",
            "Open bin",
        ],
    );
    shoot_toast(&mut nested, "11-recording-didnt-start");
    assert_clean(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}
