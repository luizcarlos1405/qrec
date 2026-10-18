//! WP8's blocking pages as the approved frames draw them (docs/GUI.md G§7
//! WP8, G§0.2) in a nested headless sway (G§8.4): S23 and its
//! GStreamer-stuck variant, X1 with D-29's export wait and the idle host,
//! X2 with D-14's line, X3 and its missing-file variant, D-13's page and the
//! not-responding one. Each case shoots its page next to its frame
//! (`<case>-<page>-compare.png`), checks its regions against the frame's
//! (±1 px × the scale), its hint bar, the keyboard focus, the a11y walk, no
//! caught panic and no GLib or GTK critical, and the exit code of G§3.2.
//! The flows behind the buttons are WP0b's (`gui_shell`).
//!
//! `cargo test -p qrec --test gui_blocking -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, wl-clipboard, dbus-daemon).
//! Shots and logs: `tmp/gui-shots/wp8/`.

#[path = "support/fail_ffmpeg.rs"]
mod fail_ffmpeg;
mod gui_support;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use gui_support::{Gui, Launch, Nested, Options, TOOL_TIMEOUT, poll, wait_for_gui_host};
use qrec_core::state::HostKind;
use qrec_shell::host::lockfile::{self, HostState, LockRecord};
use qrec_shell::lock::ProjectLock;
use qrec_shell::testkit::design::{DesignVariant, design_fixture};
use qrec_shell::testkit::run_tool;
use serde_json::{Value, json};

const AREA: &str = "wp8";
const OPEN: Duration = Duration::from_secs(60);
const EXIT: Duration = Duration::from_secs(20);

// ------------------------------------------------------------- helpers

/// `design_fixture(variant)` in a folder named `name`.
fn fixture(nested: &Nested, name: &str, variant: DesignVariant) -> PathBuf {
    let dir = nested.env.empty_dir(name);
    design_fixture(&dir, variant);
    dir
}

/// The page a blocked window shows (`local`'s `startup.page.page`).
fn page_of(local: &Value) -> String {
    local["startup"]["page"]["page"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Waits until the window shows page `page`, then until it settles: the
/// keyboard is on the page with its ring showing (the frames draw it).
fn wait_page(gui: &Gui, page: &str, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let local = gui.local();
        if local["startup"]["phase"] == "blocked" && page_of(&local) == page {
            gui.wait_idle();
            let local = gui.local();
            assert_eq!(local["focusVisible"], true, "the focus ring: {local}");
            return local;
        }
        assert!(
            Instant::now() < deadline,
            "the window never showed {page}: {local}; log {}",
            gui.log.display()
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// The hint bar's items as BRIEF writes them (`[Ctrl][Q] Quit`).
fn hints(gui: &Gui) -> Vec<String> {
    let reply = gui.ctl(json!({"op": "hints"}));
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
            let mut text = format!("{} {label}", keys.join(" "));
            if item["enabled"] == false {
                text.push_str(" (disabled)");
            }
            text
        })
        .collect()
}

/// The focused widget's name (`local`'s `focusPath`, the widget first).
fn focused(gui: &Gui) -> String {
    gui.local()["focusPath"][0]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The a11y walk is empty, no panic was caught and no critical printed.
fn assert_clean(gui: &Gui) {
    let reply = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(reply["ok"], true, "a11y: {reply}");
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    gui.assert_no_criticals();
}

/// The widget `name`'s rect (logical px), which must be there.
fn rect(gui: &Gui, name: &str) -> [f64; 4] {
    let regions = gui.regions(&[name]);
    *regions
        .get(name)
        .unwrap_or_else(|| panic!("no {name}: {regions:#?}"))
}

/// `name`'s rect is the frame's `want` (x, y, w, h), within a pixel (× the
/// run's scale, G§8.4); a `NaN` in `want` isn't checked.
fn assert_rect(gui: &Gui, name: &str, want: [f64; 4]) -> [f64; 4] {
    let got = rect(gui, name);
    let slack = gui_support::suite_scale().unwrap_or(1.0).max(1.0);
    let off: Vec<usize> = (0..4)
        .filter(|&i| !want[i].is_nan() && (got[i] - want[i]).abs() > slack)
        .collect();
    assert!(
        off.is_empty(),
        "{name}: {got:?}, the frame's {want:?} (±{slack})"
    );
    println!("measure {name}: {got:?} (frame {want:?})");
    got
}

/// The shot next to its frame (G§8.4 check 4): `<shot>-compare.png`, both
/// at 1600×1000.
fn compare(shot: &Path, frame: &str) {
    let png = gui_support::repo().join("design/screens").join(frame);
    let out = shot.with_file_name(format!(
        "{}-compare.png",
        shot.file_stem().unwrap_or_default().to_string_lossy()
    ));
    let run = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(shot)
            .arg("-i")
            .arg(&png)
            .args([
                "-filter_complex",
                "[0]scale=1600:1000[a];[1]scale=1600:1000[b];[a][b]hstack=inputs=2",
            ])
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

/// The width in device px of `rect`'s (logical px) bright ink in `png`:
/// every channel at 150 or more (`text` over the header's `surface`).
fn ink_width(nested: &Nested, png: &Path, rect: [f64; 4]) -> f64 {
    let px = nested.opts.rect_px(rect);
    let w = px[2] as usize;
    let xs: Vec<usize> = gui_support::rgb_pixels(png, px)
        .iter()
        .enumerate()
        .filter(|(_, p)| p.iter().all(|&c| c >= 150))
        .map(|(i, _)| i % w)
        .collect();
    match (xs.iter().min(), xs.iter().max()) {
        (Some(a), Some(b)) => (b - a + 1) as f64,
        _ => 0.0,
    }
}

/// The pixel at logical (`x`, `y`) of a shot, as `#RRGGBB`.
fn pixel(nested: &Nested, png: &Path, x: f64, y: f64) -> String {
    let (x, y) = nested.opts.pixel_px(x, y);
    let [r, g, b] = gui_support::rgb_pixels(png, [x, y, 1, 1])[0];
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// Clicks the widget named `name` (its center) and lets the window settle.
fn click(nested: &mut Nested, n: usize, name: &str) {
    let [x, y, w, h] = rect(nested.window(n), name);
    nested.pointer().click(x + w / 2.0, y + h / 2.0);
    nested.window(n).wait_idle();
}

/// The clipboard of the nested display (`wl-paste`), once it reads `want`.
fn wait_clipboard(nested: &Nested, want: impl Fn(&str) -> bool) -> String {
    poll(Duration::from_secs(15), "the clipboard", || {
        let out = run_tool(
            nested.nested_tool("wl-paste").arg("--no-newline"),
            TOOL_TIMEOUT,
        );
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        (out.status.success() && want(&text)).then_some(text)
    })
}

/// Waits until the labels under `name` have `text`.
fn wait_label(gui: &Gui, name: &str, text: &str, within: Duration) -> Vec<String> {
    let deadline = Instant::now() + within;
    loop {
        let labels = gui.labels(name);
        if labels.iter().any(|l| l.contains(text)) {
            return labels;
        }
        assert!(
            Instant::now() < deadline,
            "no {text:?} under {name}: {labels:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn assert_shows(labels: &[String], texts: &[&str]) {
    for text in texts {
        assert!(
            labels.iter().any(|l| l == text),
            "{text:?} isn't in {labels:?}"
        );
    }
}

fn ctrl_q(nested: &Nested) {
    nested.keys(&["-M", "ctrl", "-k", "q", "-m", "ctrl"]);
}

fn phase(status: &Value) -> String {
    status["data"]["runtime"]["record"]["phase"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Waits until `qrec status` shows record phase `want`.
fn wait_phase(nested: &Nested, project: &Path, want: &str, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let status = nested.cli_json(project, &["status"]);
        if phase(&status) == want {
            return status;
        }
        assert!(Instant::now() < deadline, "never {want}: {status}");
        thread::sleep(Duration::from_millis(100));
    }
}

/// A `PATH` whose `ffmpeg` hides encoders `hidden` from `ffmpeg -encoders`
/// (so the doctor misses them) and whose `pactl` answers like PipeWire's
/// (the nested env has no sound server: its checks would add two rows the
/// frame doesn't draw), then the usual `PATH`. In `<env>/s23-bin/`.
fn s23_path(nested: &Nested, hidden: &[&str]) -> OsString {
    let bin = nested.env.root().join("s23-bin");
    fs::create_dir_all(&bin).expect("the wrappers' folder");
    let real = fail_ffmpeg::real_ffmpeg();
    let filter: Vec<String> = hidden.iter().map(|e| format!(" {e} ")).collect();
    let ffmpeg = format!(
        "#!/bin/sh\n\
         # WP8's S23: ffmpeg without {hidden:?} for the doctor's encoder list.\n\
         case \" $* \" in\n  \
           *' -encoders '*)\n    \
             '{real}' \"$@\" | grep -v -F -e '{patterns}'\n    \
             exit 0 ;;\n\
         esac\n\
         exec '{real}' \"$@\"\n",
        real = real.display(),
        patterns = filter.join("' -e '"),
    );
    let pactl = "#!/bin/sh\n\
                 # WP8's S23: the doctor's sound server check passes.\n\
                 echo '{\"server_name\":\"PulseAudio (on PipeWire 1.4.7)\"}'\n";
    for (name, script) in [("ffmpeg", ffmpeg.as_str()), ("pactl", pactl)] {
        let path = bin.join(name);
        fs::write(&path, script).expect("a wrapper");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).expect("a PATH")
}

// ------------------------------------------------------------------ S23

/// S23 as drawn: libx264 hidden from ffmpeg (a required check fails) and
/// libsvtav1 too (optional), and the nested env's missing ScreenCast portal
/// (optional with the fake backend): a row each, required first, BRIEF's
/// copy for the portal's and libsvtav1's, the rest behind "✓ N other checks
/// passed" (it opens: the passed checks); Check again focused, Copy report
/// copies the doctor's report, a row's Copy its command; the hint bar,
/// regions and a11y; Ctrl+Q exits 5.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s23_missing_dependency() {
    let mut nested = Nested::start(AREA, "s23", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let path = s23_path(&nested, &["libx264", "libsvtav1"]);
    nested.launch(
        &project,
        Launch {
            path: Some(path),
            ..Launch::a11y()
        },
    );
    let gui = nested.window(0);
    wait_page(gui, "missingDeps", OPEN);
    let labels = gui.labels("region-blocking");
    assert_shows(
        &labels,
        &[
            "The swamp is missing something",
            "Qrec can't start until these are installed.",
            "Run",
            "qrec doctor",
            "in a terminal for the same report.",
            "ffmpeg.libx264",
            "ScreenCast portal backend",
            "Needed to capture screens on Wayland.",
            "xdg-desktop-portal-gnome",
            "libsvtav1",
            "in ffmpeg",
            "Optional",
            "AV1 export will be unavailable. Everything else works.",
            "Check again",
            "Copy report",
            "Quit",
        ],
    );
    assert!(
        labels.iter().any(|l| l.ends_with(" other checks passed")),
        "{labels:?}"
    );
    assert_eq!(
        hints(gui),
        ["[Enter] Check again", "[Ctrl][Q] Quit"],
        "S23's hint bar"
    );
    assert_eq!(focused(gui), "blocking-check-again");
    assert_clean(gui);
    let shot = nested.screenshot("page");
    compare(&shot, "S23-missing-dependency.png");
    let gui = nested.window(0);
    assert_rect(gui, "region-blocking", [0.0, 48.0, 1600.0, 920.0]);
    assert_rect(gui, "blocking-art", [720.0, 120.0, 160.0, 160.0]);
    assert_rect(gui, "blocking-title", [480.0, 296.0, 640.0, 38.0]);
    let checks = assert_rect(gui, "blocking-checks", [480.0, 432.0, 640.0, f64::NAN]);
    // A 72 px row per failed check, the required one first, then the
    // optional ones in the doctor's order; the 44 px disclosure.
    let rows = ["ffmpeg.libx264", "ffmpeg.libsvtav1", "portal.screencast"];
    let mut last = 0.0;
    for (n, id) in rows.iter().enumerate() {
        let name = format!("blocking-check-{id}");
        let [_, y, _, h] = rect(gui, &name);
        assert!((h - 72.0).abs() <= 1.0, "{name} is {h} tall");
        if n == 0 {
            assert!((y - 432.0).abs() <= 1.0, "{name} at {y}");
        }
        assert!(y > last, "{name} at {y}, after {last}");
        last = y;
    }
    let toggle = rect(gui, "blocking-passed-toggle");
    assert!(
        (toggle[3] - 44.0).abs() <= 1.0,
        "the disclosure: {toggle:?}"
    );
    assert!(
        (toggle[1] + toggle[3] - (checks[1] + checks[3])).abs() <= 1.0,
        "the disclosure ends the box: {toggle:?} in {checks:?}"
    );
    // The actions 24 px under the box (16 + 8), centered, 40 tall.
    let actions = rect(gui, "blocking-actions");
    assert!(
        (actions[1] - (checks[1] + checks[3] + 24.0)).abs() <= 1.0,
        "{actions:?} under {checks:?}"
    );
    assert!(
        (actions[0] + actions[2] / 2.0 - 800.0).abs() <= 1.0,
        "{actions:?}"
    );
    let again = rect(gui, "blocking-check-again");
    assert!((again[3] - 40.0).abs() <= 1.0, "{again:?}");
    println!("measure the box {checks:?}, the actions {actions:?}");

    // The disclosure opens: the checks that passed.
    click(&mut nested, 0, "blocking-passed-toggle");
    let gui = nested.window(0);
    let passed = rect(gui, "blocking-passed");
    assert!(passed[3] > 28.0, "the passed checks show: {passed:?}");
    let labels = gui.labels("blocking-passed");
    assert!(labels.iter().any(|l| l == "gstreamer"), "{labels:?}");
    nested.screenshot("passed");
    click(&mut nested, 0, "blocking-passed-toggle");
    assert!(
        !nested
            .window(0)
            .regions(&["blocking-passed"])
            .contains_key("blocking-passed"),
        "the disclosure closes"
    );

    // Copy report: the doctor's report; a row's Copy: its command.
    click(&mut nested, 0, "blocking-copy-report");
    let report = wait_clipboard(&nested, |t| t.starts_with("Qrec doctor"));
    assert!(report.contains("ffmpeg.libx264"), "{report}");
    wait_label(nested.window(0), "toast-host", "Report copied", OPEN);
    click(&mut nested, 0, "blocking-copy-portal.screencast");
    wait_clipboard(&nested, |t| t == "xdg-desktop-portal-gnome");
    assert_clean(nested.window(0));
    ctrl_q(&nested);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(5));
}

/// S23's GStreamer-stuck variant (not drawn: S23's shape): GStreamer's row,
/// no disclosure, Quit alone and focused; `Return` exits 5.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s23_gstreamer_stuck() {
    let mut nested = Nested::start(AREA, "s23-stuck", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    nested.launch(
        &project,
        Launch::a11y().with_env("QREC_GUI_TEST", "1,stuck-gst"),
    );
    let gui = nested.window(0);
    wait_page(gui, "gstStuck", OPEN);
    let labels = gui.labels("region-blocking");
    assert_shows(
        &labels,
        &[
            "GStreamer didn't start",
            "Qrec can't record or draw the preview without it.",
            "qrec doctor",
            "in a terminal, then start Qrec again.",
            "GStreamer",
            "Quit",
        ],
    );
    assert!(
        !labels.iter().any(|l| l.contains("checks passed")),
        "{labels:?}"
    );
    assert_eq!(hints(gui), ["[Ctrl][Q] Quit"]);
    assert_eq!(focused(gui), "blocking-quit");
    assert_clean(gui);
    let shot = nested.screenshot("page");
    compare(&shot, "S23-missing-dependency.png");
    let gui = nested.window(0);
    assert_rect(gui, "blocking-art", [720.0, 120.0, 160.0, 160.0]);
    assert_rect(gui, "blocking-checks", [480.0, 432.0, 640.0, 72.0]);
    assert_rect(gui, "blocking-actions", [f64::NAN, 528.0, f64::NAN, 40.0]);
    nested.keys(&["-k", "Return"]);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(5));
}

// ------------------------------------------------------------------- X2

/// X2 as drawn (exit 3, nothing written): the folder's row, why, the
/// hidden-files note, Quit focused (`Return` quits); then a folder inside a
/// project: D-14's line.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn x2_not_my_swamp() {
    let mut nested = Nested::start(AREA, "x2", Options::default());
    let downloads = nested.env.empty_dir("Downloads");
    fs::write(downloads.join("notes.txt"), "x").expect("a file");
    nested.launch(&downloads, Launch::a11y());
    let gui = nested.window(0);
    wait_page(gui, "notMySwamp", OPEN);
    let labels = gui.labels("region-blocking");
    assert_shows(
        &labels,
        &[
            "Not my swamp: this folder isn't empty and has no qrec.json",
            "Start Qrec in an empty folder to create a project, or in a folder that has qrec.json.",
            "Not empty \u{b7} no qrec.json",
            "Hidden files count too, such as .git or .envrc.",
            "Quit",
        ],
    );
    assert!(
        labels.iter().any(|l| l.ends_with("/Downloads")),
        "the folder: {labels:?}"
    );
    assert!(!labels.iter().any(|l| l.contains("inside the project")));
    assert_eq!(hints(gui), ["[Ctrl][Q] Quit"]);
    assert_eq!(focused(gui), "blocking-quit");
    assert_clean(gui);
    let shot = nested.screenshot("page");
    compare(&shot, "X2-not-my-swamp.png");
    let gui = nested.window(0);
    assert_rect(gui, "blocking-art", [720.0, 120.0, 160.0, 160.0]);
    assert_rect(gui, "blocking-title", [480.0, 296.0, 640.0, 76.0]);
    assert_rect(gui, "blocking-folder", [480.0, 432.0, 640.0, 56.0]);
    assert_rect(gui, "blocking-note", [480.0, 496.0, 640.0, 16.0]);
    assert_rect(gui, "blocking-quit", [770.0, 536.0, 60.0, 40.0]);
    // The minimal header: the icon, the wordmark, close; no title.
    let header = gui.labels("minimal-header");
    assert_eq!(header, ["Qrec"], "the minimal header's labels");
    // Its close is the frames' 20 px `x`: 10 px of bright ink, as the
    // frame's and the main header's (it drew GTK's default 16 px: 8).
    let s = nested.opts.scale;
    let ink = ink_width(&nested, &shot, [1564.0, 10.0, 28.0, 28.0]);
    assert!(
        (ink - 10.0 * s).abs() <= s,
        "the close x's bright ink: {ink} px wide, the frame's 10"
    );
    nested.keys(&["-k", "Return"]);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(3));
    let names: Vec<String> = fs::read_dir(&downloads)
        .expect("list")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["notes.txt"], "nothing written");

    // Inside a project: D-14's line under the row.
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let inside = project.join("exports");
    fs::create_dir_all(&inside).expect("exports");
    fs::write(inside.join("talk.mp4"), "x").expect("a file");
    nested.launch(&inside, Launch::a11y());
    let gui = nested.window(1);
    wait_page(gui, "notMySwamp", OPEN);
    let labels = gui.labels("region-blocking");
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("It's inside the project ")
                && l.ends_with("/rust-traits-talk; run qrec there.")),
        "D-14's line: {labels:?}"
    );
    assert_clean(gui);
    let shot = nested.screenshot("inside");
    compare(&shot, "X2-not-my-swamp.png");
    ctrl_q(&nested);
    assert_eq!(nested.window(1).wait_exit(EXIT), Some(3));
}

// ------------------------------------------------------------------- X3

/// The fixture's `qrec.json` with clip c1b4 ending past take 3, as X3's
/// frame: after a split, so the history has an entry to restore (its seq).
fn break_project(nested: &Nested, project: &Path) -> u64 {
    let split = nested.cli_json(project, &["clip", "split", "--timeline", "--at", "2437f"]);
    let seq = split["data"]["seq"].as_u64().expect("a seq");
    let path = project.join("qrec.json");
    let mut json: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("qrec.json")).expect("JSON");
    let clips = json["timeline"]["clips"].as_array_mut().expect("clips");
    let c1b4 = clips
        .iter_mut()
        .find(|c| c["id"] == "c1b4")
        .expect("clip c1b4");
    c1b4["out"] = json!(1400);
    fs::write(&path, serde_json::to_string_pretty(&json).expect("JSON")).expect("break it");
    seq
}

/// X3 as drawn: the validation box (the file, the first error's pointer,
/// what it means), the restore command focused (`Return` copies it), the
/// entry, Quit; `qrec history restore` works meanwhile; Ctrl+Q exits 1.
/// Then the missing file: "Rebuild it from the history:".
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn x3_project_cant_be_read() {
    let mut nested = Nested::start(AREA, "x3", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let seq = break_project(&nested, &project);
    let restore = format!("qrec history restore {seq} --yes");
    nested.launch(&project, Launch::a11y());
    let gui = nested.window(0);
    wait_page(gui, "unreadable", OPEN);
    let labels = gui.labels("region-blocking");
    assert_shows(
        &labels,
        &[
            "qrec.json can't be read",
            "Qrec won't open this project, and it never overwrites a broken qrec.json.",
            "/timeline/clips/4/out",
            "Clip c1b4 ends at frame 1400, past the end of take 3 (1377 frames).",
            "Restore the last good state from history:",
            &restore,
            "Copy command",
            &format!("Entry {seq} is the last good state."),
            "Quit",
        ],
    );
    assert!(
        labels
            .iter()
            .any(|l| l.ends_with("/rust-traits-talk/qrec.json")),
        "the file: {labels:?}"
    );
    assert_eq!(hints(gui), ["[Enter] Copy command", "[Ctrl][Q] Quit"]);
    assert_eq!(focused(gui), "blocking-copy-command");
    assert_clean(gui);
    let shot = nested.screenshot("page");
    compare(&shot, "X3-project-cant-be-read.png");
    let gui = nested.window(0);
    assert_rect(gui, "blocking-title", [480.0, 296.0, 640.0, 30.0]);
    assert_rect(gui, "blocking-problem", [480.0, 386.0, 640.0, 86.0]);
    assert_rect(gui, "blocking-restore", [480.0, 496.0, 640.0, 92.0]);
    assert_rect(gui, "blocking-command-well", [480.0, 524.0, f64::NAN, 40.0]);
    assert_rect(
        gui,
        "blocking-copy-command",
        [f64::NAN, 524.0, f64::NAN, 40.0],
    );
    assert_rect(gui, "blocking-quit", [770.0, 612.0, 60.0, 40.0]);
    // Return copies the command (the focused button).
    nested.keys(&["-k", "Return"]);
    wait_clipboard(&nested, |t| t == restore);
    wait_label(nested.window(0), "toast-host", "Command copied", OPEN);
    // The CLI works in-process while the page shows (G§3.3).
    let restored = nested.cli_json(&project, &["history", "restore", &seq.to_string(), "--yes"]);
    assert_eq!(restored["ok"], true, "{restored}");
    assert_clean(nested.window(0));
    ctrl_q(&nested);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(1));

    // The missing file: rebuilt from the history.
    fs::remove_file(project.join("qrec.json")).expect("remove qrec.json");
    nested.launch(&project, Launch::a11y());
    let gui = nested.window(1);
    wait_page(gui, "unreadable", OPEN);
    let labels = gui.labels("region-blocking");
    assert_shows(
        &labels,
        &[
            "qrec.json is missing",
            "Qrec won't open this project without it.",
            "Rebuild it from the history:",
            "Copy command",
        ],
    );
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("qrec history restore ")),
        "{labels:?}"
    );
    assert!(!labels.iter().any(|l| l == "/timeline/clips/4/out"));
    assert_eq!(focused(gui), "blocking-copy-command");
    assert_clean(gui);
    let shot = nested.screenshot("missing");
    compare(&shot, "X3-project-cant-be-read.png");
    ctrl_q(&nested);
    assert_eq!(nested.window(1).wait_exit(EXIT), Some(1));
}

// ------------------------------------------------------------------- X1

/// X1 as drawn: the modal over the scrim (the minimal header dims under
/// it, the hint bar doesn't), its well, Quit focused; the time goes on;
/// `Esc` exits 6 and the take goes on. Then Tab and Return: Stop & open
/// turns "Stopping…" while the CLI's take finalizes (its EOS held 6 s),
/// and the window opens with the take on the timeline.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn x1_cli_recording() {
    let mut nested = Nested::start(AREA, "x1", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let slow_eos = [("QREC_FAKE_EOS_DELAY_MS", OsString::from("6000"))];
    let start = nested.cli_json_with(&project, &["record", "start"], &slow_eos);
    assert_eq!(start["ok"], true, "{start}");
    wait_phase(&nested, &project, "recording", Duration::from_secs(30));
    nested.launch(&project, Launch::a11y());
    let gui = nested.window(0);
    wait_page(gui, "cliRecording", OPEN);
    let labels = gui.labels("blocking-modal");
    assert_shows(
        &labels,
        &[
            "A take is being recorded from the CLI.",
            "Stop it to open the project here. Quit leaves it recording.",
            "REC",
            "Take 10",
            "Quit",
            "Stop & open",
        ],
    );
    let lead = labels
        .iter()
        .find(|l| l.starts_with("Take 10 started from the terminal "))
        .cloned()
        .unwrap_or_else(|| panic!("the lead: {labels:?}"));
    assert_eq!(
        hints(gui),
        [
            "[Tab] Move focus",
            "[Enter] Activate focused button",
            "[Esc] Quit"
        ]
    );
    assert_eq!(focused(gui), "blocking-quit");
    assert_eq!(gui.local()["focusInTopLayer"], true);
    assert_clean(gui);
    let shot = nested.screenshot("page");
    compare(&shot, "X1-cli-recording-at-gui-start.png");
    let gui = nested.window(0);
    assert_rect(gui, "blocking-modal", [540.0, 351.0, 520.0, 266.0]);
    assert_rect(gui, "scrim-blocking", [0.0, 0.0, 1600.0, 968.0]);
    assert_rect(gui, "blocking-well", [564.0, 485.0, 472.0, 44.0]);
    assert_rect(gui, "blocking-rec", [572.0, 493.0, 68.0, 28.0]);
    assert_rect(
        gui,
        "blocking-stop-and-open",
        [f64::NAN, 553.0, f64::NAN, 40.0],
    );
    let quit = assert_rect(gui, "blocking-quit", [f64::NAN, 553.0, f64::NAN, 40.0]);
    let stop = rect(gui, "blocking-stop-and-open");
    assert!(
        (stop[0] + stop[2] - 1036.0).abs() <= 1.0,
        "the footer ends 24 px in: {stop:?}"
    );
    assert!(
        (stop[0] - (quit[0] + quit[2] + 12.0)).abs() <= 1.0,
        "12 px apart: {quit:?} {stop:?}"
    );
    // The scrim over the minimal header (`scrim` over `surface`), the page's
    // `bg` under it, the hint bar live.
    assert_eq!(
        pixel(&nested, &shot, 300.0, 24.0),
        "#141910",
        "the header dims"
    );
    assert_eq!(pixel(&nested, &shot, 300.0, 500.0), "#12170F");
    assert_eq!(
        pixel(&nested, &shot, 800.0, 975.0),
        "#1C2416",
        "the hint bar"
    );
    // The time goes on (`hello` every second).
    thread::sleep(Duration::from_millis(2_500));
    let later = nested.window(0).labels("blocking-modal");
    assert!(
        later
            .iter()
            .any(|l| l.starts_with("Take 10 started from the terminal ") && *l != lead),
        "{lead:?} still: {later:?}"
    );
    nested.keys(&["-k", "Escape"]);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(6));
    let status = nested.cli_json(&project, &["status"]);
    assert_eq!(phase(&status), "recording", "the take goes on: {status}");

    // Stop & open: Tab to it, Return; "Stopping…" while the take ends.
    let clips = nested.cli_json(&project, &["clip", "list"])["data"]
        .as_array()
        .map_or(0, Vec::len);
    nested.launch(&project, Launch::a11y());
    let gui = nested.window(1);
    wait_page(gui, "cliRecording", OPEN);
    assert_eq!(focused(gui), "blocking-quit");
    nested.keys(&["-k", "Tab", "-k", "Return"]);
    let gui = nested.window(1);
    wait_label(gui, "blocking-modal", "Stopping\u{2026}", OPEN);
    gui.wait_idle();
    assert_eq!(focused(gui), "blocking-quit", "the keyboard stays on Quit");
    assert_clean(gui);
    nested.screenshot("stopping");
    let gui = nested.window(1);
    gui.wait_phase("open", OPEN);
    let status = wait_for_gui_host(&nested, &project, OPEN);
    assert_eq!(phase(&status), "idle", "{status}");
    let after = nested.cli_json(&project, &["clip", "list"])["data"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(after, clips + 1, "the take landed");
    let gui = nested.window(1);
    gui.wait_idle();
    assert!(
        !gui.regions(&["blocking-modal"])
            .contains_key("blocking-modal"),
        "the modal went with the page"
    );
    assert_clean(gui);
    nested.close_window();
    assert_eq!(nested.window(1).wait_exit(EXIT), Some(0));
}

/// D-29's export wait (not drawn: X1's modal): a CLI export runs (its
/// render units slowed); the modal, its well with the output and percent;
/// `Esc` exits 6 and the export goes on. Then Wait: "Waiting…", and the
/// window opens once the export is done.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn x1_export_wait() {
    let mut nested = Nested::start(AREA, "x1-export", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let slow = fail_ffmpeg::slow_path(nested.env.root(), 4);
    let export = nested.cli_background_with(
        &project,
        &["export", "--quality", "draft"],
        &[("PATH", slow)],
    );
    poll(OPEN, "the CLI's export job", || {
        let status = nested.cli_json(&project, &["status"]);
        status["data"]["runtime"]["jobs"]
            .as_object()
            .is_some_and(|jobs| !jobs.is_empty())
            .then_some(())
    });
    nested.launch(&project, Launch::a11y());
    let gui = nested.window(0);
    wait_page(gui, "exportWait", OPEN);
    let labels = gui.labels("blocking-modal");
    assert_shows(
        &labels,
        &[
            "An export from the CLI is running.",
            "Wait to open the project here once it's done. Quit leaves it running.",
            "Quit",
            "Wait",
        ],
    );
    assert!(
        labels.iter().any(|l| l.starts_with("exports/")),
        "the output: {labels:?}"
    );
    assert_eq!(
        hints(gui),
        [
            "[Tab] Move focus",
            "[Enter] Activate focused button",
            "[Esc] Quit"
        ]
    );
    assert_eq!(focused(gui), "blocking-quit");
    assert_clean(gui);
    let shot = nested.screenshot("page");
    compare(&shot, "X1-cli-recording-at-gui-start.png");
    let gui = nested.window(0);
    assert_rect(gui, "blocking-modal", [540.0, 351.0, 520.0, 266.0]);
    assert_rect(gui, "blocking-well", [564.0, 485.0, 472.0, 44.0]);
    // The mini mud bar's trough shows in the `field` well (`border`): its
    // right end, which the slowed export hasn't filled yet.
    let [x, y, w, h] = assert_rect(gui, "blocking-mini-mud", [f64::NAN, f64::NAN, 40.0, 4.0]);
    assert_eq!(
        pixel(&nested, &shot, x + w - 2.0, y + h / 2.0),
        "#3A4A2C",
        "the trough"
    );
    nested.keys(&["-k", "Escape"]);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(6));

    // Wait: the window opens once the export is done.
    nested.launch(&project, Launch::a11y());
    let gui = nested.window(1);
    wait_page(gui, "exportWait", OPEN);
    nested.keys(&["-k", "Tab", "-k", "Return"]);
    let gui = nested.window(1);
    wait_label(gui, "blocking-modal", "Waiting\u{2026}", OPEN);
    gui.wait_idle();
    assert_clean(gui);
    nested.screenshot("waiting");
    let status = gui_support::reap_within(export, Duration::from_secs(240), "qrec export");
    assert!(status.success(), "the CLI's export: {status}");
    let gui = nested.window(1);
    gui.wait_phase("open", OPEN);
    wait_for_gui_host(&nested, &project, OPEN);
    nested.window(1).wait_idle();
    assert_clean(nested.window(1));
    nested.close_window();
    assert_eq!(nested.window(1).wait_exit(EXIT), Some(0));
}

/// D-29's idle host: a CLI take finalizing when the window starts shows no
/// page; the window waits silently ("Opening rust-traits-talk…" after 1 s,
/// G§3.3) and opens once the host let go, the take on the timeline.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn x1_idle_host_opens_without_a_page() {
    let mut nested = Nested::start(AREA, "x1-idle", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let slow_eos = [("QREC_FAKE_EOS_DELAY_MS", OsString::from("8000"))];
    let start = nested.cli_json_with(&project, &["record", "start"], &slow_eos);
    assert_eq!(start["ok"], true, "{start}");
    wait_phase(&nested, &project, "recording", Duration::from_secs(30));
    let clips = nested.cli_json(&project, &["clip", "list"])["data"]
        .as_array()
        .map_or(0, Vec::len);
    let stop = nested.cli_background(&project, &["record", "stop"]);
    wait_phase(&nested, &project, "finalizing", Duration::from_secs(30));
    nested.launch(&project, Launch::a11y());
    let gui = nested.window(0);
    let local = poll(OPEN, "the slow caption", || {
        let local = gui.local();
        (local["startup"] == json!({"phase": "opening", "slow": true})).then_some(local)
    });
    assert_eq!(page_of(&local), "", "no page: {local}");
    let caption = gui.labels("region-stage");
    assert!(
        caption
            .iter()
            .any(|l| l == "Opening rust-traits-talk\u{2026}"),
        "{caption:?}"
    );
    nested.screenshot("opening");
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    let _ = gui_support::reap_within(stop, gui_support::CLI_TIMEOUT, "qrec record stop");
    wait_for_gui_host(&nested, &project, OPEN);
    let after = nested.cli_json(&project, &["clip", "list"])["data"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(after, clips + 1, "the take landed");
    let gui = nested.window(0);
    gui.wait_idle();
    assert_clean(gui);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

// -------------------------------------------- D-13 and not responding

/// D-13's page (not drawn: X2's shape): a second window on a project a
/// window holds; the project's row names that window's process; exit 6, the
/// first window stays.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn d13_held_by_another_window() {
    let mut nested = Nested::start(AREA, "d13", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let first = nested.launch(&project, Launch::a11y()).pid();
    wait_for_gui_host(&nested, &project, OPEN);
    nested.window(0).wait_phase("open", OPEN);
    // The first window out of the way, so the second takes the output.
    nested.swaymsg(&[&format!("[pid={first}]"), "move", "to", "workspace", "2"]);
    nested.launch(&project, Launch::a11y());
    let gui = nested.window(1);
    wait_page(gui, "heldByWindow", OPEN);
    let labels = gui.labels("region-blocking");
    assert_shows(
        &labels,
        &[
            "This project is open in another Qrec window",
            "A project opens in one Qrec window at a time.",
            &format!("Open in process {first}"),
            "Switch to that window, or close it and start Qrec here again.",
            "Quit",
        ],
    );
    assert_eq!(hints(gui), ["[Ctrl][Q] Quit"]);
    assert_eq!(focused(gui), "blocking-quit");
    assert_clean(gui);
    let shot = nested.screenshot("page");
    compare(&shot, "X2-not-my-swamp.png");
    let gui = nested.window(1);
    assert_rect(gui, "blocking-folder", [480.0, f64::NAN, 640.0, 56.0]);
    let reply = gui.ctl(json!({"op": "action", "id": "blocking.quit"}));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(nested.window(1).wait_exit(EXIT), Some(6));
    assert!(nested.window(0).running(), "the first window stays");
    assert_clean(nested.window(0));
    nested.swaymsg(&[&format!("[pid={first}]"), "kill"]);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// The not-responding page (not drawn: X2's shape): this test holds the
/// project's lock with a record that names it and no socket; after the 3 s
/// a starting holder gets, the page names the process, its kind and since
/// when; Ctrl+Q exits 6.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_holder_that_doesnt_respond() {
    let mut nested = Nested::start(AREA, "not-responding", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let lock = ProjectLock::try_acquire(&project)
        .expect("the lock")
        .expect("free");
    let record = LockRecord {
        pid: std::process::id(),
        kind: HostKind::Headless,
        socket: nested
            .env
            .runtime_dir()
            .join("nobody.sock")
            .display()
            .to_string(),
        since: Utc::now(),
        state: HostState::Ready,
    };
    lockfile::write(&lock, &record).expect("the lock record");
    nested.launch(&project, Launch::a11y());
    let gui = nested.window(0);
    wait_page(gui, "notResponding", OPEN);
    let labels = gui.labels("region-blocking");
    assert_shows(
        &labels,
        &[
            "The Qrec process holding this project isn't responding",
            "Qrec can't open the project while that process holds it.",
            &format!(
                "If it stays stuck, end process {} and start Qrec again.",
                record.pid
            ),
            "Quit",
        ],
    );
    let why = format!("Process {} \u{b7} headless host \u{b7} since ", record.pid);
    assert!(
        labels.iter().any(|l| l.starts_with(&why)),
        "{why:?} in {labels:?}"
    );
    assert_eq!(hints(gui), ["[Ctrl][Q] Quit"]);
    assert_eq!(focused(gui), "blocking-quit");
    assert_clean(gui);
    let shot = nested.screenshot("page");
    compare(&shot, "X2-not-my-swamp.png");
    ctrl_q(&nested);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(6));
    drop(lock);
}
