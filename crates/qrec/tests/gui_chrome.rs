//! WP6's cases (docs/GUI.md G§7 WP6, G§8.4) in a nested headless sway: the
//! header in every record phase (S01a, S02, S03, S04, S05, S06, S15b), the
//! narrow header (layers button), the S16b toast, the S18 purge modal and
//! its keyboard focus, and Settings › Project with the S18b fps
//! confirmation over it.
//!
//! Stub-based where other work packages' regions aren't merged (G§7.0):
//! the stage, timeline, panel, bin and export panel show WP0b's stubs, so
//! S16b and S18 get their final look after WP7 (G§7.2).
//!
//! `cargo test -p qrec --test gui_chrome -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon). Screenshots and
//! logs go to `tmp/gui-shots/wp6/`; each shot has a `-compare.png` next to
//! its approved frame where one exists.

mod gui_support;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use gui_support::{
    BRIEF_REGIONS, Launch, Nested, Options, WP0B_REGIONS, assert_regions, wait_for_gui_host,
};
use qrec_core::state::{CanvasMode, UiState};
use qrec_core::{ClipId, TakeId};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use serde_json::{Value, json};

const OPEN: Duration = Duration::from_secs(60);
const EXIT: Duration = Duration::from_secs(20);
const AREA: &str = "wp6";

/// `design_fixture(variant)` in a folder named `name`, with `ui` as its
/// `ui.json` preset when given.
fn fixture(nested: &Nested, name: &str, variant: DesignVariant, ui: Option<UiState>) -> PathBuf {
    let dir = nested.env.empty_dir(name);
    design_fixture(&dir, variant);
    if let Some(ui) = ui {
        write_ui(&dir, &ui);
    }
    dir
}

/// The design fixture's UI with the playhead at the end (S02).
fn live_ui() -> UiState {
    UiState {
        playhead: 7391,
        ..UiState::default()
    }
}

/// S06's: Program, c2d0 selected, the playhead at 2437.
fn program_ui() -> UiState {
    UiState {
        canvas_mode: CanvasMode::Program,
        selection: vec![ClipId::from_raw("c2d0")],
        playhead: 2437,
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

/// The record phase `qrec status` reports.
fn phase(status: &Value) -> String {
    status["data"]["runtime"]["record"]["phase"]
        .as_str()
        .or_else(|| status["data"]["record"]["phase"].as_str())
        .unwrap_or("?")
        .to_owned()
}

fn wait_phase(nested: &Nested, project: &Path, want: &str, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let status = nested.cli_json(project, &["status"]);
        if phase(&status) == want {
            return status;
        }
        assert!(Instant::now() < deadline, "never got to {want}: {status}");
        thread::sleep(Duration::from_millis(100));
    }
}

/// The text of the header's labels, joined: the subtitle is two labels on
/// one baseline ("10 clips · " and "00:04:06:11", G§3.14).
fn header_text(nested: &mut Nested) -> String {
    nested.window(0).labels("region-header").join("")
}

/// `ffmpeg … hstack` of a shot and its approved frame (G§8.4 check 4),
/// optionally both cropped to `crop` (`w:h:x:y`).
fn compare(shot: &Path, frame: &str, crop: Option<&str>) {
    let png = gui_support::repo().join("design/screens").join(frame);
    if !png.exists() {
        return;
    }
    let out = shot.with_file_name(format!(
        "{}-compare.png",
        shot.file_stem().unwrap_or_default().to_string_lossy()
    ));
    let filter = match crop {
        Some(c) => format!("[0]crop={c}[a];[1]crop={c}[b];[a][b]hstack=inputs=2"),
        None => "[0][1]hstack=inputs=2".to_owned(),
    };
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(shot)
        .arg("-i")
        .arg(&png)
        .args(["-filter_complex", &filter])
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(
        status.success(),
        "ffmpeg hstack failed for {}",
        shot.display()
    );
}

/// The pixel at (`x`, `y`) of a PNG, as `#RRGGBB`.
fn pixel(png: &Path, x: u32, y: u32) -> String {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(png)
        .args([
            "-vf",
            &format!("crop=1:1:{x}:{y},format=rgb24"),
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .expect("run ffmpeg");
    let rgb = out.stdout;
    assert_eq!(rgb.len(), 3, "one pixel from {}", png.display());
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

/// A color token of Swamp Night, from `design/tokens/qrec-tokens.json`.
fn token(name: &str) -> String {
    let path = gui_support::repo().join("design/tokens/qrec-tokens.json");
    let tokens: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("the tokens"))
        .expect("tokens JSON");
    tokens["colors"][name]["swamp-night"]
        .as_str()
        .expect("a color")
        .chars()
        .take(7)
        .collect::<String>()
        .to_uppercase()
}

/// The hint bar's items as BRIEF writes them (`[Ctrl][K] Commands`,
/// `(disabled)` marks), from the test channel's `hints`.
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
            if item["emphasized"] == true {
                text.push_str(" (emphasized)");
            }
            text
        })
        .collect()
}

/// The `a11y` walk is empty (launched with `Launch::a11y()`).
fn assert_a11y(nested: &mut Nested) {
    let reply = nested.window(0).ctl(json!({"op": "a11y"}));
    assert_eq!(reply["ok"], true, "a11y: {reply}");
}

/// No panic was caught in the window.
fn assert_no_panics(nested: &mut Nested) {
    let local = nested.window(0).local();
    assert_eq!(local["panics"], 0, "{local}");
}

/// The header's own controls (BRIEF §2.3 at 1600 wide).
const HEADER_PARTS: [(&str, [f64; 4]); 6] = [
    ("mode-switch", [110.0, 8.0, 176.0, 32.0]),
    ("record-button", [1240.0, 8.0, 120.0, 32.0]),
    ("export-button", [1368.0, 8.0, 104.0, 32.0]),
    ("commands-button", [1484.0, 8.0, 32.0, 32.0]),
    ("settings-button", [1520.0, 8.0, 32.0, 32.0]),
    ("close-button", [1560.0, 8.0, 32.0, 32.0]),
];

fn assert_header_parts(nested: &mut Nested) {
    let names: Vec<&str> = HEADER_PARTS.iter().map(|(n, _)| *n).collect();
    let regions = nested.window(0).regions(&names);
    assert_regions(&regions, &HEADER_PARTS, &names, 1.0);
    assert_regions(&regions, &BRIEF_REGIONS, &WP0B_REGIONS, 1.0);
}

// -------------------------------------------------------------- the header

/// S02: Live, idle. The header's parts sit where BRIEF §2.3 puts them, the
/// title over its two-label summary, the hint bar is G§4.3's Live idle
/// list, the a11y walk is empty.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s02_the_header_at_rest() {
    let mut nested = Nested::start(AREA, "s02", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(live_ui()),
    );
    open(&mut nested, &project, Launch::a11y());
    let shot = nested.screenshot("header");
    compare(&shot, "S02-live-idle.png", Some("1600:48:0:0"));
    compare(&shot, "S02-live-idle.png", None);
    assert_header_parts(&mut nested);
    let labels = nested.window(0).labels("region-header");
    for want in [
        "Qrec",
        "rust-traits-talk",
        "10 clips \u{b7} ",
        "00:04:06:11",
        "Record",
        "Export\u{2026}",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert_eq!(
        hints(&mut nested),
        [
            "[R] Record",
            "[D] Discard last take",
            "[Space] Play (disabled)",
            "[Tab] Program",
            "[C] Autocut all",
            "[B] Bin",
            "[Ctrl][K] Commands",
            "[?] All keys",
        ]
    );
    let status = nested.window(0).ctl(json!({"op": "hints"}))["status"].clone();
    assert_eq!(status, "Snapping on \u{b7} 1920\u{d7}1080 \u{b7} 30 fps");
    assert_a11y(&mut nested);
    // The header's ground is `surface`, its hairline `border`; the record
    // button is `record`.
    assert_eq!(pixel(&shot, 1000, 20), token("surface"));
    assert_eq!(pixel(&shot, 1000, 47), token("border"));
    assert_eq!(pixel(&shot, 1244, 12), token("record"));
    // The hint bar's ground, under its top hairline.
    assert_eq!(pixel(&shot, 1596, 976), token("surface"));
    assert_no_panics(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// S06: Program with c2d0 selected: the switch on Program, the clip hints.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s06_the_header_in_program() {
    let mut nested = Nested::start(AREA, "s06", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(program_ui()),
    );
    open(&mut nested, &project, Launch::a11y());
    let shot = nested.screenshot("header");
    compare(&shot, "S06-program-with-selection.png", Some("1600:48:0:0"));
    assert_header_parts(&mut nested);
    assert_eq!(
        hints(&mut nested),
        [
            "[S] Split",
            "[[] []] Trim to playhead",
            "[<] [>] Move",
            "[X] Remove",
            "[C] Autocut",
            "[O] Record over",
            "[Z] Zoom to clip",
            "[?] All keys",
        ]
    );
    // The mode switch switches like Tab: a click on Live sends it.
    let regions = nested.window(0).regions(&["mode-switch"]);
    let [x, y, _, h] = regions["mode-switch"];
    nested.pointer().click(x + 36.0, y + h / 2.0);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = nested.cli_json(&project, &["status"]);
        if status["data"]["ui"]["canvasMode"] == "live" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the switch didn't switch: {status}"
        );
        thread::sleep(Duration::from_millis(100));
    }
    nested.window(0).wait_idle();
    nested.screenshot("live-again");
    assert_a11y(&mut nested);
    assert_no_panics(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// S01a: an empty folder. "No clips yet"; Record disabled, its tooltip
/// says why (hovered, 8 px under it); the first-run hints.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s01a_the_header_of_an_empty_project() {
    let mut nested = Nested::start(AREA, "s01a", Options::default());
    let project = nested.env.empty_dir("rust-traits-talk");
    open(&mut nested, &project, Launch::a11y());
    let text = header_text(&mut nested);
    assert!(text.contains("rust-traits-talkNo clips yet"), "{text:?}");
    let first = hints(&mut nested);
    assert_eq!(first[1], "[R] Record (disabled)", "{first:?}");
    assert_eq!(
        &first[2..],
        ["[Tab] Program", "[Ctrl][K] Commands", "[?] All keys"]
    );
    let regions = nested.window(0).regions(&["record-button"]);
    let [x, y, w, h] = regions["record-button"];
    nested.pointer().move_to(x + w - 6.0, y + h / 2.0);
    // The tooltip shows after GTK's hover delay.
    thread::sleep(Duration::from_millis(1500));
    nested.window(0).wait_idle();
    let shot = nested.screenshot("header-tooltip");
    compare(&shot, "S01a-first-run-no-sources.png", Some("1600:80:0:0"));
    assert_a11y(&mut nested);
    assert_no_panics(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// S03, S04, S05: the record button through a take (countdown, recording
/// with the 2 px line, finalizing), and the hints of each phase.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s03_s04_s05_the_header_through_a_take() {
    let mut nested = Nested::start(AREA, "take", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(live_ui()),
    );
    let set = nested.cli_json(&project, &["project", "set", "--countdown", "3"]);
    assert_eq!(set["ok"], true, "{set}");
    open(
        &mut nested,
        &project,
        Launch::a11y()
            .with_env("QREC_FAKE_RECORD_OFFSET_FRAMES", "4842")
            .with_env("QREC_FAKE_EOS_DELAY_MS", "8000"),
    );

    // S03: the countdown (3 s, real time: shoot it at once). `record
    // start` answers once the take starts: it runs in the background.
    let mut start = background_cli(&nested, &project, &["record", "start"]);
    wait_phase(&nested, &project, "countdown", Duration::from_secs(10));
    nested.window(0).wait_idle();
    let countdown = hints(&mut nested);
    let shot = nested.screenshot("s03-countdown");
    compare(&shot, "S03-countdown.png", Some("1600:48:0:0"));
    assert_eq!(countdown, ["[Esc] Cancel", "[R] Cancel"]);
    let text = header_text(&mut nested);
    assert!(text.contains("Cancel"), "{text:?}");

    // S04: recording.
    wait_phase(&nested, &project, "recording", Duration::from_secs(20));
    let _ = start.wait();
    nested.window(0).wait_idle();
    let recording = hints(&mut nested);
    let shot = nested.screenshot("s04-recording");
    compare(&shot, "S04-recording.png", Some("1600:48:0:0"));
    assert_eq!(
        recording,
        [
            "[R] Stop",
            "[Esc] Cancel take (files kept)",
            "[Tab] Switch view",
            "[=] [\u{2212}] Zoom",
            "[\\] Fit",
        ]
    );
    assert!(header_text(&mut nested).contains("Stop"));
    // The 2 px `record` line under the header.
    assert_eq!(pixel(&shot, 1000, 47), token("record"));
    assert_eq!(pixel(&shot, 1000, 46), token("record"));
    assert_a11y(&mut nested);

    // S05: finalizing (the fake EOS waits 8 s).
    let mut stop = background_cli(&nested, &project, &["record", "stop"]);
    wait_phase(&nested, &project, "finalizing", Duration::from_secs(20));
    nested.window(0).wait_idle();
    let finalizing = hints(&mut nested);
    let shot = nested.screenshot("s05-finalizing");
    compare(&shot, "S05-finalizing.png", Some("1600:48:0:0"));
    assert_eq!(
        finalizing[0],
        "Finalizing take 10. Waiting for every file to close (up to 10 s)."
    );
    assert!(header_text(&mut nested).contains("Saving\u{2026}"));
    let _ = stop.wait();
    wait_phase(&nested, &project, "idle", Duration::from_secs(30));
    nested.window(0).wait_idle();
    let text = header_text(&mut nested);
    assert!(text.contains("11 clips"), "the take landed: {text:?}");
    nested.screenshot("landed");
    assert_no_panics(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// `qrec <args>` in the background (its output goes nowhere).
fn background_cli(nested: &Nested, project: &Path, args: &[&str]) -> Child {
    nested
        .env
        .command(&gui_support::qrec_bin())
        .arg("--project")
        .arg(project)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("run qrec")
}

/// S15b: an export from the CLI runs: the slot shows its percent and the
/// mini mud bar ("Exporting 42%" in the tooltip), Record waits, the hints
/// lead with Show export.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s15b_the_header_while_exporting() {
    let mut nested = Nested::start(AREA, "s15b", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(program_ui()),
    );
    open(&mut nested, &project, Launch::a11y());
    let mut export = background_cli(&nested, &project, &["export", "--quality", "draft"]);
    let deadline = Instant::now() + Duration::from_secs(120);
    let percent = loop {
        let status = nested.cli_json(&project, &["status"]);
        let jobs = status["data"]["runtime"]["jobs"].clone();
        let shown = nested.window(0).labels("export-button");
        if let Some(p) = shown
            .iter()
            .find_map(|l| l.strip_suffix('%').and_then(|n| n.parse::<u32>().ok()))
            && p >= 20
        {
            break p;
        }
        assert!(
            Instant::now() < deadline,
            "the export never got going: {jobs} / {shown:?}"
        );
        thread::sleep(Duration::from_millis(200));
    };
    nested.window(0).wait_idle();
    let shot = nested.screenshot("header-exporting");
    compare(&shot, "S15b-export-running.png", Some("1600:48:0:0"));
    let items = hints(&mut nested);
    assert_eq!(items[0], "[Ctrl][E] Show export", "{items:?}");
    assert!(
        items.contains(&"[O] Record over (disabled)".to_owned()),
        "{items:?}"
    );
    assert!(percent <= 100);
    let cancel = nested.cli_json(&project, &["export", "cancel"]);
    assert_eq!(cancel["ok"], true, "{cancel}");
    let _ = export.wait();
    assert_a11y(&mut nested);
    assert_no_panics(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// The header at 1000 wide: the Narrow layout's layers button, left of
/// Record; Record stays 120 wide.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_narrow_header_has_the_layers_button() {
    let mut nested = Nested::start(AREA, "narrow", Options::size(1000, 1000));
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(live_ui()),
    );
    open(&mut nested, &project, Launch::a11y());
    let regions = nested
        .window(0)
        .regions(&["layers-button", "record-button", "close-button"]);
    assert_regions(
        &regions,
        &[
            ("region-header", [0.0, 0.0, 1000.0, 48.0]),
            ("layers-button", [600.0, 8.0, 32.0, 32.0]),
            ("record-button", [640.0, 8.0, 120.0, 32.0]),
            ("close-button", [960.0, 8.0, 32.0, 32.0]),
        ],
        &[
            "region-header",
            "layers-button",
            "record-button",
            "close-button",
        ],
        1.0,
    );
    nested.screenshot("header");
    assert_a11y(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

// --------------------------------------------------------------- the toast

/// S16b: a CLI export of the Smoke fixture ends while the export panel is
/// closed: the core's success toast, bottom-centered 12 px above the hint
/// bar (bottom 956, centered on 800), with Open file and Open folder.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s16b_the_export_done_toast() {
    let mut nested = Nested::start(AREA, "s16b", Options::default());
    let project = fixture(&nested, "smoke", DesignVariant::Smoke, Some(program_ui()));
    open(&mut nested, &project, Launch::a11y());
    let done = nested.cli_json(&project, &["export", "--quality", "draft"]);
    assert_eq!(done["ok"], true, "{done}");
    let gui = nested.window(0);
    gui.wait_idle();
    let regions = gui.regions(&["toast-host"]);
    let [x, y, w, h] = regions["toast-host"];
    assert!(
        (y + h - 956.0).abs() <= 1.0,
        "the toast's bottom: {regions:?}"
    );
    assert!((x + w / 2.0 - 800.0).abs() <= 1.0, "centered: {regions:?}");
    assert!((360.0..=561.0).contains(&w), "360–560 wide: {w}");
    let labels = gui.labels("toast-host");
    for want in [
        "Layers! Like an onion. Your video is ready.",
        "Open file",
        "Open folder",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert!(
        !labels.contains(&"Copy path".to_owned()),
        "two actions at most"
    );
    let shot = nested.screenshot("toast");
    compare(&shot, "S16b-export-done-panel-closed.png", None);
    compare(
        &shot,
        "S16b-export-done-panel-closed.png",
        Some("520:120:540:848"),
    );
    assert_a11y(&mut nested);
    assert_no_panics(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// The history cursor `qrec status` reports.
fn cursor(nested: &Nested, project: &Path) -> u64 {
    let status = nested.cli_json(project, &["status"]);
    status["data"]["history"]["cursor"]
        .as_u64()
        .unwrap_or_else(|| panic!("no cursor: {status}"))
}

/// Toasts come one at a time with a "+N" chip for those waiting; dismissing
/// one shows the next. A core toast's Undo stays usable only while the
/// history stands still (G§3.7): once something else was done, a click on
/// it undoes nothing.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn toasts_queue_and_their_undo_waits_for_nothing_else() {
    let mut nested = Nested::start(AREA, "toasts", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(live_ui()),
    );
    open(&mut nested, &project, Launch::a11y());
    // Three "Not built yet" toasts (G§1.1): one shows, two wait.
    for id in ["palette.open", "keys.help", "palette.open"] {
        let reply = nested.window(0).ctl(json!({"op": "action", "id": id}));
        assert_eq!(reply["ok"], true, "{reply}");
    }
    let gui = nested.window(0);
    gui.wait_idle();
    let labels = gui.labels("toast-host");
    assert!(labels.contains(&"Not built yet".to_owned()), "{labels:?}");
    assert!(labels.contains(&"+2".to_owned()), "{labels:?}");
    assert!(
        labels.contains(&"The command palette comes with milestone M5.".to_owned()),
        "{labels:?}"
    );
    nested.screenshot("queue");
    let regions = nested.window(0).regions(&["toast-dismiss"]);
    let [x, y, w, h] = regions["toast-dismiss"];
    assert_eq!((w, h), (24.0, 24.0), "a 24 px close button: {regions:?}");
    nested.pointer().click(x + w / 2.0, y + h / 2.0);
    let gui = nested.window(0);
    gui.wait_idle();
    let labels = gui.labels("toast-host");
    assert!(
        labels.contains(&"The cheat sheet comes with milestone M5.".to_owned()),
        "the next one: {labels:?}"
    );
    assert!(labels.contains(&"+1".to_owned()), "{labels:?}");
    for _ in 0..2 {
        let regions = nested.window(0).regions(&["toast-dismiss"]);
        let [x, y, w, h] = regions["toast-dismiss"];
        nested.pointer().click(x + w / 2.0, y + h / 2.0);
        nested.window(0).wait_idle();
    }
    assert!(nested.window(0).labels("toast-host").is_empty());

    // An external edit of qrec.json: the core toasts it with Undo.
    let split = nested.cli_json(&project, &["clip", "split", "--timeline", "--at", "2437f"]);
    assert_eq!(split["ok"], true, "{split}");
    let file = project.join("qrec.json");
    let mut json: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("qrec.json")).expect("JSON");
    json["name"] = json!("rust-traits-talk-edited");
    std::fs::write(&file, serde_json::to_string_pretty(&json).expect("JSON")).expect("write");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let gui = nested.window(0);
        gui.wait_idle();
        let labels = gui.labels("toast-host");
        if labels.contains(&"Merged an external edit of qrec.json".to_owned()) {
            assert!(labels.contains(&"Undo".to_owned()), "{labels:?}");
            break;
        }
        assert!(Instant::now() < deadline, "no merge toast: {labels:?}");
        thread::sleep(Duration::from_millis(200));
    }
    assert!(header_text(&mut nested).contains("rust-traits-talk-edited"));
    nested.screenshot("undo-toast");
    // Something else is done: the toast's Undo is stale.
    let again = nested.cli_json(&project, &["clip", "split", "--timeline", "--at", "100f"]);
    assert_eq!(again["ok"], true, "{again}");
    let before = cursor(&nested, &project);
    let gui = nested.window(0);
    gui.wait_idle();
    let regions = gui.regions(&["toast-action-0"]);
    let [x, y, w, h] = regions["toast-action-0"];
    nested.pointer().click(x + w / 2.0, y + h / 2.0);
    nested.window(0).wait_idle();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        cursor(&nested, &project),
        before,
        "a stale Undo undid something"
    );
    assert_a11y(&mut nested);
    assert_no_panics(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

// --------------------------------------------------------------- the modal

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

/// S18: purging takes 2 and 4 asks first. The modal is 520 wide on the
/// window's center over a scrim that ends at y 968 (the hint bar stays
/// live with the modal's hints); Cancel has the focus: `Enter` right after
/// it opens purges nothing; `Tab` + `Enter` purges.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s18_the_purge_modal_and_its_focus() {
    let mut nested = Nested::start(AREA, "s18", Options::default());
    let mut ui = program_ui();
    ui.bin_open = true;
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base, Some(ui));
    open(&mut nested, &project, Launch::a11y());
    let purge = json!({
        "op": "dispatch",
        "command": {"cmd": "takePurge", "takes": [TakeId::numbered(2), TakeId::numbered(4)]},
    });
    let reply = nested.window(0).ctl(purge.clone());
    assert_eq!(reply["ok"], true, "{reply}");
    let gui = nested.window(0);
    gui.wait_idle();
    let regions = gui.regions(&["modal", "scrim-modal", "modal-cancel", "modal-confirm"]);
    let [mx, my, mw, mh] = regions["modal"];
    assert_eq!(mw, 520.0, "{regions:?}");
    assert!((mx + mw / 2.0 - 800.0).abs() <= 1.0, "{regions:?}");
    // Centered on the window like the frame (S18: 277 to 723).
    assert!((my + mh / 2.0 - 500.0).abs() <= 1.0, "{regions:?}");
    assert_regions(
        &regions,
        &[("scrim-modal", [0.0, 0.0, 1600.0, 968.0])],
        &["scrim-modal"],
        1.0,
    );
    let labels = gui.labels("modal");
    for want in [
        "Get out of my swamp?",
        "Purge take 2 and take 4 from disk?",
        "takes/take-0002/recorder.log",
        "Cancel",
        "Purge 2 takes",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert_eq!(
        hints(&mut nested),
        [
            "[Tab] Move focus",
            "[Enter] Activate focused button",
            "[Esc] Cancel",
        ]
    );
    assert_a11y(&mut nested);
    let shot = nested.screenshot("modal");
    compare(&shot, "S18-destructive-modal.png", None);
    compare(&shot, "S18-destructive-modal.png", Some("540:460:530:270"));
    // The hint bar is outside the scrim: its ground is plain `surface`.
    assert_eq!(pixel(&shot, 1596, 976), token("surface"));

    // Enter right away: Cancel. Nothing is purged.
    nested.keys(&["-k", "Return"]);
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(gui.local()["overlays"], json!([]));
    let all = takes(&nested, &project);
    assert!(
        all.contains(&"take-0002".to_owned()) && all.contains(&"take-0004".to_owned()),
        "nothing purged: {all:?}"
    );

    // Again: Tab to the destructive button, Enter.
    let reply = nested.window(0).ctl(purge);
    assert_eq!(reply["ok"], true, "{reply}");
    nested.window(0).wait_idle();
    // Tab with its keyboard kept: the window stays active, so the purge
    // button wears its focus ring in the shot (G§8.4).
    let held = nested.keys_held(&["-k", "Tab"], 2000);
    gui_support::poll(
        Duration::from_secs(10),
        "the focus on the purge button",
        || (nested.window(0).local()["focusPath"][0] == "modal-confirm").then_some(()),
    );
    nested.window(0).wait_idle();
    nested.screenshot("modal-tab");
    held.wait();
    nested.keys(&["-k", "Return"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let all = takes(&nested, &project);
        if !all.contains(&"take-0002".to_owned()) && !all.contains(&"take-0004".to_owned()) {
            break;
        }
        assert!(Instant::now() < deadline, "not purged: {all:?}");
        thread::sleep(Duration::from_millis(200));
    }
    nested.window(0).wait_idle();
    assert_eq!(nested.window(0).local()["overlays"], json!([]));
    assert_no_panics(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

// --------------------------------------------------------------- Settings

/// S18b: Ctrl+, opens Settings › Project at (380, 180, 840, 640); picking
/// 25 fps opens the confirmation over it, both scrims ending at y 968 and
/// the modal's hints live; the rate still reads 30 under it. Change fps
/// sends the confirmed `ProjectSet`: the canvas is 25 fps and the clips
/// re-quantized.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s18b_settings_and_the_fps_confirmation() {
    let mut nested = Nested::start(AREA, "s18b", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(live_ui()),
    );
    open(&mut nested, &project, Launch::a11y());
    nested.keys(&["-M", "ctrl", "-k", "comma", "-m", "ctrl"]);
    let gui = nested.window(0);
    gui.wait_idle();
    let regions = gui.regions(&["settings", "scrim-settings", "settings-rate"]);
    assert_regions(
        &regions,
        &[
            ("settings", [380.0, 180.0, 840.0, 640.0]),
            ("scrim-settings", [0.0, 0.0, 1600.0, 968.0]),
        ],
        &["settings", "scrim-settings"],
        1.0,
    );
    let labels = gui.labels("settings");
    for want in [
        "Settings",
        "Appearance",
        "Project",
        "Keyboard",
        "Canvas",
        "Size",
        "Frame rate",
        "Background",
        "Saved in this project's ",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert_eq!(hints(&mut nested), ["[Tab] Move focus", "[Esc] Close"]);
    assert_a11y(&mut nested);
    nested.screenshot("settings");

    // Frame rate 25: the second of five 44 px segments in a 2 px well.
    let [rx, ry, rw, rh] = regions["settings-rate"];
    assert!((rw - 224.0).abs() <= 2.0, "{regions:?}");
    assert!(
        (rx + rw - 1188.0).abs() <= 1.0,
        "right-aligned at 1188: {regions:?}"
    );
    nested.pointer().click(rx + 2.0 + 44.0 * 1.5, ry + rh / 2.0);
    let gui = nested.window(0);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        gui.wait_idle();
        let local = gui.local();
        let overlays = local["overlays"].as_array().cloned().unwrap_or_default();
        if overlays.len() == 2 {
            assert!(
                overlays[1]
                    .as_str()
                    .is_some_and(|o| o.starts_with("Confirm")),
                "{local}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "no confirmation: {local}");
        thread::sleep(Duration::from_millis(100));
    }
    let regions = gui.regions(&["settings", "scrim-settings", "modal", "scrim-modal"]);
    assert_regions(
        &regions,
        &[
            ("settings", [380.0, 180.0, 840.0, 640.0]),
            ("scrim-settings", [0.0, 0.0, 1600.0, 968.0]),
            ("scrim-modal", [0.0, 0.0, 1600.0, 968.0]),
        ],
        &["settings", "scrim-settings", "scrim-modal"],
        1.0,
    );
    let [mx, _, mw, _] = regions["modal"];
    assert!((mw - 520.0).abs() <= 1.0 && (mx + mw / 2.0 - 800.0).abs() <= 1.0);
    let labels = gui.labels("modal");
    for want in [
        "Change the canvas to 25 fps?",
        "c7f3 \u{b7} take 1 \u{b7} [0, 410) \u{2192} [0, 342)",
        "Change fps",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert_eq!(
        hints(&mut nested),
        [
            "[Tab] Move focus",
            "[Enter] Activate focused button",
            "[Esc] Cancel",
        ]
    );
    let shot = nested.screenshot("confirmation");
    compare(&shot, "S18b-change-fps-confirmation.png", None);
    compare(
        &shot,
        "S18b-change-fps-confirmation.png",
        Some("840:640:380:180"),
    );
    // Both scrims end at the hint bar: above it the panel's `surface` is
    // dimmed; the bar's own `surface` isn't.
    assert_eq!(pixel(&shot, 1596, 976), token("surface"));
    assert_ne!(pixel(&shot, 1596, 964), token("surface"));
    let status = nested.cli_json(&project, &["status"]);
    assert_eq!(
        status["data"]["project"]["canvas"]["fps"],
        json!({"num": 30, "den": 1}),
        "nothing changed yet: {status}"
    );

    // Change fps: the confirmed command.
    let regions = nested.window(0).regions(&["modal-confirm"]);
    let [cx, cy, cw, ch] = regions["modal-confirm"];
    nested.pointer().click(cx + cw / 2.0, cy + ch / 2.0);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = nested.cli_json(&project, &["status"]);
        if status["data"]["project"]["canvas"]["fps"] == json!({"num": 25, "den": 1}) {
            break;
        }
        assert!(Instant::now() < deadline, "not 25 fps: {status}");
        thread::sleep(Duration::from_millis(200));
    }
    let clips = nested.cli_json(&project, &["clip", "list"]);
    let first = &clips["data"].as_array().expect("clips")[0];
    assert_eq!(first["id"], "c7f3", "{clips}");
    let gui = nested.window(0);
    gui.wait_idle();
    let local = gui.local();
    assert_eq!(
        local["overlays"].as_array().map(Vec::len),
        Some(1),
        "Settings stays: {local}"
    );
    nested.screenshot("after");

    // The Background field: an invalid color says so under it; a valid one
    // is the project's on Enter.
    let regions = nested
        .window(0)
        .regions(&["settings-background", "settings-size"]);
    let [bx, by, bw, bh] = regions["settings-background"];
    nested.pointer().click(bx + bw / 2.0, by + bh / 2.0);
    nested.window(0).wait_idle();
    nested.keys(&["-M", "ctrl", "-k", "a", "-m", "ctrl", "-k", "BackSpace"]);
    nested.keys(&["#12", "-k", "Return"]);
    let gui = nested.window(0);
    gui.wait_idle();
    let labels = gui.labels("settings");
    assert!(labels.contains(&"Use #RRGGBB".to_owned()), "{labels:?}");
    nested.screenshot("invalid-background");
    nested.keys(&["-M", "ctrl", "-k", "a", "-m", "ctrl", "-k", "BackSpace"]);
    nested.keys(&["#2a3b4c", "-k", "Return"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = nested.cli_json(&project, &["status"]);
        if status["data"]["project"]["canvas"]["background"] == "#2A3B4C" {
            break;
        }
        assert!(Instant::now() < deadline, "no new background: {status}");
        thread::sleep(Duration::from_millis(200));
    }
    let gui = nested.window(0);
    gui.wait_idle();
    assert!(!gui.labels("settings").contains(&"Use #RRGGBB".to_owned()));

    // The Size list: open, it is the top overlay (a Menu: its keys are its
    // own, Esc closes it, not Settings); a key pick sends the size.
    let [sx, sy, sw, sh] = regions["settings-size"];
    nested.pointer().click(sx + sw / 2.0, sy + sh / 2.0);
    let gui = nested.window(0);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        gui.wait_idle();
        let overlays = gui.local()["overlays"].clone();
        if overlays.as_array().map(Vec::len) == Some(2) {
            assert_eq!(overlays[1], "Menu", "{overlays}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the list didn't open: {overlays}"
        );
        thread::sleep(Duration::from_millis(100));
    }
    nested.screenshot("size-list");
    // The open list keeps Settings' hints.
    assert_eq!(hints(&mut nested), ["[Tab] Move focus", "[Esc] Close"]);
    nested.keys(&["-k", "Escape"]);
    let gui = nested.window(0);
    gui.wait_idle();
    let overlays = gui.local()["overlays"].clone();
    assert_eq!(
        overlays.as_array().map(Vec::len),
        Some(1),
        "Settings stays: {overlays}"
    );
    nested.pointer().click(sx + sw / 2.0, sy + sh / 2.0);
    nested.window(0).wait_idle();
    nested.keys(&["-k", "Down", "-k", "Return"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = nested.cli_json(&project, &["status"]);
        let width = status["data"]["project"]["canvas"]["width"].as_u64();
        if width.is_some_and(|w| w != 1920) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the size didn't change: {width:?}"
        );
        thread::sleep(Duration::from_millis(200));
    }

    // Esc closes Settings.
    let gui = nested.window(0);
    gui.wait_idle();
    nested.keys(&["-k", "Escape"]);
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(gui.local()["overlays"], json!([]));
    assert_no_panics(&mut nested);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

// --------------------------------------------------------------- D-12

/// D-12: closing the window while an export runs asks first ("Quit while
/// exporting?", Keep exporting focused): `Enter` keeps exporting; Cancel
/// export and quit cancels it and the window closes (exit 0).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn d12_quitting_while_exporting_asks_first() {
    let mut nested = Nested::start(AREA, "d12", Options::default());
    let project = fixture(
        &nested,
        "rust-traits-talk",
        DesignVariant::Base,
        Some(live_ui()),
    );
    open(&mut nested, &project, Launch::a11y());
    let mut export = background_cli(&nested, &project, &["export", "--quality", "draft"]);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let shown = nested.window(0).labels("export-button");
        if shown.iter().any(|l| l.ends_with('%')) {
            break;
        }
        assert!(Instant::now() < deadline, "no export: {shown:?}");
        thread::sleep(Duration::from_millis(100));
    }
    nested.close_window();
    let gui = nested.window(0);
    gui.wait_idle();
    let labels = gui.labels("modal");
    for want in [
        "Quit while exporting?",
        "Keep exporting",
        "Cancel export and quit",
    ] {
        assert!(labels.contains(&want.to_owned()), "{want:?} in {labels:?}");
    }
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("The export is ") && l.ends_with(" % done.")),
        "{labels:?}"
    );
    assert_a11y(&mut nested);
    nested.screenshot("modal");
    // Enter: Keep exporting (it has the focus).
    nested.keys(&["-k", "Return"]);
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(gui.local()["overlays"], json!([]));
    assert!(gui.running(), "the window stays");
    // Again, Tab + Enter: Cancel export and quit.
    nested.close_window();
    nested.window(0).wait_idle();
    nested.keys(&["-k", "Tab"]);
    nested.keys(&["-k", "Return"]);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
    let _ = export.wait();
    let status = nested.cli_json(&project, &["status"]);
    assert_eq!(
        status["data"]["host"],
        Value::Null,
        "the lock is free: {status}"
    );
    let exported = std::fs::read_dir(project.join("exports"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(exported, 0, "nothing written to exports/");
}
