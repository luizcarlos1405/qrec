//! WP0b's acceptance flows (docs/GUI.md G§7 WP0b) in a nested headless
//! sway (G§8.4): the window skeleton on the design fixture, the startup
//! order, the interim pages and their takeover flows, the exit codes, the
//! theme and keys from the user's config, and the adaptive layouts.
//!
//! `cargo test -p qrec --test gui_shell -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon). Screenshots and
//! logs go to `tmp/gui-shots/wp0b/`.

mod gui_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use gui_support::{
    BRIEF_REGIONS, Launch, Nested, Options, WP0B_REGIONS, assert_regions, wait_for_gui_host,
};
use qrec_shell::testkit::design::{DesignVariant, design_fixture};
use serde_json::{Value, json};

const OPEN: Duration = Duration::from_secs(60);
const EXIT: Duration = Duration::from_secs(20);

/// `design_fixture(variant)` in a folder named `name`.
fn fixture(nested: &Nested, name: &str, variant: DesignVariant) -> PathBuf {
    let dir = nested.env.empty_dir(name);
    design_fixture(&dir, variant);
    dir
}

fn ctrl_q(nested: &Nested) {
    nested.keys(&["-M", "ctrl", "-k", "q", "-m", "ctrl"]);
}

/// The headless hosts running in the env (`qrec host --headless …`).
fn headless_hosts(nested: &Nested) -> Vec<String> {
    let marker = format!("QREC_RUNTIME_DIR={}", nested.env.runtime_dir().display());
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(environ) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        if !environ
            .split(|&b| b == 0)
            .any(|var| var == marker.as_bytes())
        {
            continue;
        }
        let cmdline = fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
        if cmdline.contains("--headless") {
            found.push(cmdline);
        }
    }
    found
}

/// The project's lock record (`.qrec/lock`), if one is written.
fn lock_record(project: &Path) -> Option<Value> {
    let bytes = fs::read(project.join(".qrec/lock")).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let end = text.rfind('}')?;
    serde_json::from_str(&text[..=end]).ok()
}

fn phase(status: &Value) -> String {
    status["data"]["runtime"]["record"]["phase"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Waits until `qrec status` shows record phase `want` (a host answers).
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

/// The page a blocked window shows (`local`'s `startup.page.page`).
fn page_of(local: &Value) -> String {
    local["startup"]["page"]["page"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn assert_lock_free(nested: &Nested, project: &Path) {
    let status = nested.cli_json(project, &["status"]);
    assert_eq!(
        status["data"]["host"],
        Value::Null,
        "the lock is free: {status}"
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

/// A color token of a theme, from `design/tokens/qrec-tokens.json`.
fn token(theme: &str, name: &str) -> String {
    let path = gui_support::repo().join("design/tokens/qrec-tokens.json");
    let tokens: Value =
        serde_json::from_str(&fs::read_to_string(path).expect("the tokens")).expect("tokens JSON");
    tokens["colors"][name][theme]
        .as_str()
        .expect("a color")
        .chars()
        .take(7)
        .collect::<String>()
        .to_uppercase()
}

// --------------------------------------------------------------- the window

/// The skeleton on `design_fixture(Base)`: 1600×1000, the GUI host with the
/// window's pid, the header following CLI edits, BRIEF §2.1's regions, the
/// stand-in hints, an empty a11y walk, the fonts, the icons and the art;
/// closing the window exits 0 and frees the lock.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_skeleton_on_the_design_fixture() {
    let mut nested = Nested::start("wp0b", "base", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let pid = nested.launch(&project, Launch::a11y()).pid();
    let status = wait_for_gui_host(&nested, &project, OPEN);
    assert_eq!(status["data"]["pid"].as_u64(), Some(u64::from(pid)));
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
    assert_eq!(nested.window_rect(), Some([0.0, 0.0, 1600.0, 1000.0]));

    let gui = nested.window(0);
    let regions = gui.regions(&[]);
    assert_regions(&regions, &BRIEF_REGIONS, &WP0B_REGIONS, 1.0);
    let header = gui.labels("region-header");
    assert!(
        header.contains(&"rust-traits-talk".to_owned()),
        "{header:?}"
    );
    // The summary is two labels on one baseline (G§3.14): "10 clips · "
    // and "00:04:06:11".
    assert!(
        header.join("").contains("10 clips · 00:04:06:11"),
        "{header:?}"
    );
    nested.screenshot("open");

    // The header follows the CLI's edits through the GUI host.
    let split = nested.cli_json(&project, &["clip", "split", "--timeline", "--at", "2437f"]);
    assert_eq!(split["ok"], true, "{split}");
    let gui = nested.window(0);
    gui.wait_idle();
    let header = gui.labels("region-header");
    // The summary is two labels on one baseline (G§3.14): "11 clips · "
    // and "00:04:06:11".
    assert!(
        header.join("").contains("11 clips · 00:04:06:11"),
        "{header:?}"
    );
    nested.screenshot("split");
    let undo = nested.cli_json(&project, &["undo"]);
    assert_eq!(undo["ok"], true, "{undo}");
    let gui = nested.window(0);
    gui.wait_idle();
    let header = gui.labels("region-header");
    // The summary is two labels on one baseline (G§3.14): "10 clips · "
    // and "00:04:06:11".
    assert!(
        header.join("").contains("10 clips · 00:04:06:11"),
        "{header:?}"
    );

    // The hint bar's stand-in: the user's keys.
    let hints = gui.ctl(json!({"op": "hints"}));
    assert_eq!(hints["items"][0]["label"], "Record", "{hints}");
    assert_eq!(hints["items"][0]["keys"], json!([["R"]]), "{hints}");
    assert_eq!(
        hints["status"], "Snapping on · 1920×1080 · 30 fps",
        "{hints}"
    );

    // Objective checks: a11y, fonts, icons, art.
    for op in ["a11y", "fonts", "icons", "art"] {
        let reply = gui.ctl(json!({"op": op}));
        assert_eq!(reply["ok"], true, "{op}: {reply}");
    }
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    assert_eq!(local["theme"], "swamp-night", "{local}");

    // The stage's ground is the theme's `bg`, the hint bar's `surface`.
    let shot = nested.screenshot("final");
    assert_eq!(pixel(&shot, 8, 600), token("swamp-night", "bg"));
    // The hint bar's right padding (x 1588–1600), under its top hairline.
    assert_eq!(pixel(&shot, 1596, 976), token("swamp-night", "surface"));

    // Closing the window: exit 0, the lock free.
    nested.close_window();
    let gui = nested.window(0);
    assert_eq!(gui.wait_exit(EXIT), Some(0), "log {}", gui.log.display());
    assert_lock_free(&nested, &project);
    let log = nested.window(0).gui_log();
    assert!(!log.contains(" CSS "), "CSS errors:\n{log}");
    assert!(!log.contains(" ERROR "), "errors:\n{log}");
}

/// The header title drawn after a partial redraw equals a full redraw's: the
/// placeholder title (the folder's name) leaves no trace above the
/// project's name. Before GTK's manual font rendering
/// (`theme::fonts::configure_rendering`) the old title's ascender tops
/// stayed above the new one (the stray mark). The folder is named so that
/// its title is all ascenders. The mark came with the GL renderer: run it
/// with `QREC_TEST_WLR=gles2` (without the fix it fails there by 8 px; the
/// pixman runs draw with Cairo, which never showed it).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_header_title_leaves_no_trace_of_the_old_one() {
    let crop = |shot: &Path| -> Vec<u8> {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(shot)
            .args([
                "-vf",
                "crop=480:48:560:0,format=rgb24",
                "-f",
                "rawvideo",
                "-",
            ])
            .output()
            .expect("run ffmpeg");
        out.stdout
    };
    let mut shots = Vec::new();
    for (n, debug) in [(1, None), (2, Some("full-redraw"))] {
        let mut nested = Nested::start("wp0b", &format!("title-{n}"), Options::default());
        let project = fixture(&nested, "hhhhhhhhhhhhhhhh", DesignVariant::Base);
        let mut launch = Launch::default();
        if let Some(debug) = debug {
            launch = launch.with_env("GSK_DEBUG", debug);
        }
        nested.launch(&project, launch);
        wait_for_gui_host(&nested, &project, OPEN);
        let gui = nested.window(0);
        gui.wait_phase("open", OPEN);
        gui.wait_idle();
        let split = nested.cli_json(&project, &["clip", "split", "--timeline", "--at", "2437f"]);
        assert_eq!(split["ok"], true, "{split}");
        nested.window(0).wait_idle();
        shots.push(nested.screenshot("header"));
    }
    let (partial, full) = (crop(&shots[0]), crop(&shots[1]));
    assert_eq!(partial.len(), 480 * 48 * 3);
    let differing = partial
        .chunks_exact(3)
        .zip(full.chunks_exact(3))
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        differing,
        0,
        "the title differs from a full redraw's in {differing} px: {} vs {}",
        shots[0].display(),
        shots[1].display()
    );
}

/// The test control channel's ops (G§3.18): `dispatch` goes through the
/// Store (a confirmation opens its modal), `action` runs a binding or a
/// GUI action as its key would, `local` follows.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_control_channel_ops_work() {
    let mut nested = Nested::start("wp0b", "control", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    nested.launch(&project, Launch::default());
    wait_for_gui_host(&nested, &project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();

    // dispatch: the bin opens (BRIEF §2.8: it pushes the stage).
    let reply = gui.ctl(json!({
        "op": "dispatch",
        "command": {"cmd": "setUi", "patch": {"binOpen": true}},
    }));
    assert_eq!(reply["ok"], true, "{reply}");
    gui.wait_idle();
    let regions = gui.regions(&[]);
    assert_regions(
        &regions,
        &[
            ("region-bin", [0.0, 48.0, 352.0, 608.0]),
            ("region-stage", [352.0, 48.0, 864.0, 608.0]),
        ],
        &["region-bin", "region-stage"],
        1.0,
    );
    let status = nested.cli_json(&project, &["status"]);
    assert_eq!(status["data"]["ui"]["binOpen"], true, "{status}");
    nested.screenshot("bin");

    // action: a binding (b toggles the bin) and a GUI action (the palette's
    // "Not built yet" toast, G§1.1).
    let gui = nested.window(0);
    assert_eq!(
        gui.ctl(json!({"op": "action", "id": "bin.toggle"}))["ok"],
        true
    );
    gui.wait_idle();
    assert!(!gui.regions(&[]).contains_key("region-bin"));
    assert_eq!(
        gui.ctl(json!({"op": "action", "id": "palette.open"}))["ok"],
        true
    );
    gui.wait_idle();
    let toast = gui.labels("toast-host");
    assert!(toast.contains(&"Not built yet".to_owned()), "{toast:?}");
    let unknown = gui.ctl(json!({"op": "action", "id": "no.such.thing"}));
    assert_eq!(unknown["ok"], false, "{unknown}");

    // A confirmation opens the modal; Esc closes it, nothing purged.
    let reply = gui.ctl(json!({
        "op": "dispatch",
        "command": {"cmd": "takePurge", "takes": ["take-0002"]},
    }));
    assert_eq!(reply["ok"], true, "{reply}");
    gui.wait_idle();
    let local = gui.local();
    assert!(
        local["overlays"][0]
            .as_str()
            .is_some_and(|o| o.starts_with("Confirm")),
        "{local}"
    );
    nested.screenshot("modal");
    nested.keys(&["-k", "Escape"]);
    let gui = nested.window(0);
    gui.wait_idle();
    assert_eq!(gui.local()["overlays"], json!([]));
    let takes = nested.cli_json(&project, &["take", "list"]);
    assert!(
        takes["data"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| t["id"] == "take-0002")),
        "nothing purged: {takes}"
    );
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// A signal: 130 within 15 s, the lock free.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn sigterm_exits_130() {
    let mut nested = Nested::start("wp0b", "sigterm", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    nested.launch(&project, Launch::default());
    wait_for_gui_host(&nested, &project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.signal("TERM");
    assert_eq!(gui.wait_exit(Duration::from_secs(15)), Some(130));
    assert_lock_free(&nested, &project);
}

// ------------------------------------------------------------ startup order

/// G§3.3: the lock and the socket come before GStreamer, doctor and the
/// window, so a `record toggle` sent 50 ms after launch is the GUI host's:
/// recording with host `gui`, the lock record says `gui`, no headless host.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_record_toggle_50_ms_after_launch_is_the_windows() {
    let mut nested = Nested::start("wp0b", "startup-order", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    nested.launch(&project, Launch::default());
    thread::sleep(Duration::from_millis(50));
    let toggle = nested.cli_json(&project, &["record", "toggle"]);
    assert_eq!(toggle["ok"], true, "{toggle}");
    let status = wait_phase(&nested, &project, "recording", Duration::from_secs(30));
    assert_eq!(status["data"]["host"], "gui", "{status}");
    assert_eq!(
        lock_record(&project).map(|r| r["kind"].clone()),
        Some(json!("gui"))
    );
    assert!(
        headless_hosts(&nested).is_empty(),
        "{:?}",
        headless_hosts(&nested)
    );
    nested.window(0).wait_phase("open", OPEN);
    nested.screenshot("recording");
    let stop = nested.cli_json(&project, &["record", "toggle"]);
    assert_eq!(stop["ok"], true, "{stop}");
    wait_phase(&nested, &project, "idle", Duration::from_secs(30));
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

// ------------------------------------------------------------ interim pages

/// `QREC_GUI_TEST=stuck-gst`: the stuck page, the lock free while it
/// shows, Quit exits 5.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn gstreamer_stuck_shows_its_page_and_exits_5() {
    let mut nested = Nested::start("wp0b", "stuck-gst", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    nested.launch(
        &project,
        Launch::a11y().with_env("QREC_GUI_TEST", "1,stuck-gst"),
    );
    let gui = nested.window(0);
    let local = gui.wait_phase("blocked", OPEN);
    assert_eq!(page_of(&local), "gstStuck", "{local}");
    gui.wait_idle();
    let a11y = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "{a11y}");
    nested.screenshot("page");
    // The host failed and let go: the CLI works in-process.
    assert_lock_free(&nested, &project);
    let list = nested.cli_json(&project, &["clip", "list"]);
    assert_eq!(list["ok"], true, "{list}");
    // Enter on the page's only button (Quit) quits; so does Ctrl+Q.
    nested.keys(&["-k", "Return"]);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(5));
}

/// No ffmpeg on the window's PATH: interim S23 with the lock free (the CLI
/// works in-process meanwhile); Check again with ffmpeg back opens it.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_missing_ffmpeg_shows_s23_until_check_again_finds_it() {
    let mut nested = Nested::start("wp0b", "s23", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let bin = nested.env.root().join("bin");
    fs::create_dir(&bin).expect("a bin folder");
    let path = std::env::var_os("PATH").expect("PATH");
    let mut ffmpeg_dir = None;
    let mut dirs = vec![bin.clone()];
    for dir in std::env::split_paths(&path) {
        if dir.join("ffmpeg").exists() || dir.join("ffprobe").exists() {
            ffmpeg_dir.get_or_insert(dir);
        } else {
            dirs.push(dir);
        }
    }
    let ffmpeg_dir = ffmpeg_dir.expect("ffmpeg on PATH");
    let launch = Launch {
        path: Some(std::env::join_paths(dirs).expect("a PATH")),
        ..Launch::default()
    };
    nested.launch(&project, launch);
    let gui = nested.window(0);
    let local = gui.wait_phase("blocked", OPEN);
    assert_eq!(page_of(&local), "missingDeps", "{local}");
    let text = gui.labels("region-blocking").join("\n");
    assert!(text.contains("ffmpeg"), "{text}");
    gui.wait_idle();
    nested.screenshot("page");
    assert_lock_free(&nested, &project);
    let list = nested.cli_json(&project, &["clip", "list"]);
    assert_eq!(list["ok"], true, "the CLI works in-process: {list}");
    // ffmpeg is back: Check again (the primary button, focused) opens.
    for tool in ["ffmpeg", "ffprobe"] {
        std::os::unix::fs::symlink(ffmpeg_dir.join(tool), bin.join(tool)).expect("a symlink");
    }
    nested.keys(&["-k", "Return"]);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    wait_for_gui_host(&nested, &project, OPEN);
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// A corrupt `qrec.json`: interim X3 with the restore command; `qrec
/// history restore <seq> --yes` works while the page shows; Quit exits 1.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_corrupt_project_shows_x3_and_can_be_restored_meanwhile() {
    let mut nested = Nested::start("wp0b", "x3", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let split = nested.cli_json(&project, &["clip", "split", "--timeline", "--at", "2437f"]);
    let seq = split["data"]["seq"].as_u64().expect("a seq");
    fs::write(project.join("qrec.json"), "{ \"version\": 1, broken").expect("corrupt it");
    nested.launch(&project, Launch::default());
    let gui = nested.window(0);
    let local = gui.wait_phase("blocked", OPEN);
    assert_eq!(page_of(&local), "unreadable", "{local}");
    let text = gui.labels("region-blocking").join("\n");
    let restore = format!("qrec history restore {seq} --yes");
    assert!(text.contains(&restore), "{text}");
    gui.wait_idle();
    nested.screenshot("page");
    let restored = nested.cli_json(&project, &["history", "restore", &seq.to_string(), "--yes"]);
    assert_eq!(restored["ok"], true, "{restored}");
    let list = nested.cli_json(&project, &["clip", "list"]);
    assert_eq!(list["data"].as_array().map(Vec::len), Some(11), "{list}");
    ctrl_q(&nested);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(1));
}

/// D-35: an empty folder gets a `qrec.json` named after it, by the `Init`
/// the GUI host queues ahead of every IPC request: the first request
/// that reaches the host already sees the project (the ordering itself is
/// `host::gui`'s test).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn an_empty_folder_is_initialized_before_any_request() {
    let mut nested = Nested::start("wp0b", "empty-folder", Options::default());
    let project = nested.env.empty_dir("fresh-swamp");
    nested.launch(&project, Launch::default());
    let status = wait_for_gui_host(&nested, &project, OPEN);
    assert_eq!(status["data"]["initialized"], true, "{status}");
    assert_eq!(status["data"]["project"]["name"], "fresh-swamp", "{status}");
    assert!(project.join("qrec.json").is_file());
    let list = nested.cli_json(&project, &["clip", "list"]);
    assert_eq!(list["ok"], true, "{list}");
    assert_eq!(list["data"], json!([]), "{list}");
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
    nested.screenshot("open");
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// X2: a folder that isn't empty and has no `qrec.json` (exit 3, nothing
/// written); a folder inside a project gets D-14's line.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn not_my_swamp_and_inside_a_project() {
    let mut nested = Nested::start("wp0b", "x2", Options::default());
    let downloads = nested.env.empty_dir("Downloads");
    fs::write(downloads.join("notes.txt"), "x").expect("a file");
    nested.launch(&downloads, Launch::default());
    let gui = nested.window(0);
    let local = gui.wait_phase("blocked", OPEN);
    assert_eq!(page_of(&local), "notMySwamp", "{local}");
    let text = gui.labels("region-blocking").join("\n");
    assert!(
        text.contains("Not my swamp: this folder isn't empty and has no qrec.json"),
        "{text}"
    );
    assert!(!text.contains("It's inside the project"), "{text}");
    gui.wait_idle();
    nested.screenshot("page");
    ctrl_q(&nested);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(3));
    let names: Vec<String> = fs::read_dir(&downloads)
        .expect("list")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["notes.txt"], "nothing written");

    // Inside a project: D-14's line.
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let inside = project.join("exports");
    fs::create_dir_all(&inside).expect("exports");
    fs::write(inside.join("talk.mp4"), "x").expect("a file");
    nested.launch(&inside, Launch::default());
    let gui = nested.window(1);
    let local = gui.wait_phase("blocked", OPEN);
    assert_eq!(page_of(&local), "notMySwamp", "{local}");
    let text = gui.labels("region-blocking").join("\n");
    assert!(text.contains("It's inside the project"), "{text}");
    assert!(text.contains("run qrec there"), "{text}");
    gui.wait_idle();
    nested.screenshot("inside");
    ctrl_q(&nested);
    assert_eq!(nested.window(1).wait_exit(EXIT), Some(3));
}

/// D-13: a second `qrec` on a project a window holds gets the held page and
/// exits 6; the first stays.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_second_window_gets_the_held_page_and_exits_6() {
    let mut nested = Nested::start("wp0b", "second-window", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    nested.launch(&project, Launch::default());
    wait_for_gui_host(&nested, &project, OPEN);
    nested.window(0).wait_phase("open", OPEN);
    let second = nested.launch(&project, Launch::default()).pid();
    // The second window has no test channel (the first holds it): its log.
    let deadline = Instant::now() + OPEN;
    while !nested.window(1).gui_log().contains("HeldByWindow") {
        assert!(Instant::now() < deadline, "no held page");
        thread::sleep(Duration::from_millis(100));
    }
    thread::sleep(Duration::from_millis(500));
    nested.screenshot("held");
    nested.swaymsg(&[&format!("[pid={second}]"), "kill"]);
    assert_eq!(nested.window(1).wait_exit(EXIT), Some(6));
    assert!(nested.window(0).running(), "the first window stays");
    let status = nested.cli_json(&project, &["status"]);
    assert_eq!(status["data"]["host"], "gui", "{status}");
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// X1: a headless recording holds the project. Stop & open finalizes the
/// take and opens it; Quit exits 6 and the recording goes on.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_cli_recording_shows_x1_stop_and_open_or_quit() {
    let mut nested = Nested::start("wp0b", "x1", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);

    // Quit: exit 6, still recording.
    let start = nested.cli_json(&project, &["record", "start"]);
    assert_eq!(start["ok"], true, "{start}");
    wait_phase(&nested, &project, "recording", Duration::from_secs(30));
    nested.launch(&project, Launch::default());
    let gui = nested.window(0);
    let local = gui.wait_phase("blocked", OPEN);
    assert_eq!(page_of(&local), "cliRecording", "{local}");
    gui.wait_idle();
    nested.screenshot("page");
    ctrl_q(&nested);
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(6));
    let status = nested.cli_json(&project, &["status"]);
    assert_eq!(phase(&status), "recording", "{status}");
    assert_eq!(status["data"]["runtime"]["host"], "headless", "{status}");

    // Stop & open: the take is finalized and placed, the window opens.
    let clips = nested.cli_json(&project, &["clip", "list"])["data"]
        .as_array()
        .map_or(0, Vec::len);
    nested.launch(&project, Launch::default());
    let gui = nested.window(1);
    let local = gui.wait_phase("blocked", OPEN);
    assert_eq!(page_of(&local), "cliRecording", "{local}");
    gui.wait_idle();
    nested.keys(&["-k", "Return"]);
    let gui = nested.window(1);
    gui.wait_phase("open", OPEN);
    let status = wait_for_gui_host(&nested, &project, OPEN);
    assert_eq!(phase(&status), "idle", "{status}");
    let after = nested.cli_json(&project, &["clip", "list"])["data"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(after, clips + 1, "the take landed");
    assert!(headless_hosts(&nested).is_empty(), "the headless host went");
    nested.window(1).wait_idle();
    nested.screenshot("opened");
    nested.close_window();
    assert_eq!(nested.window(1).wait_exit(EXIT), Some(0));
}

// --------------------------------------------------------------- the config

/// `theme = "swamp-day"` gives Swamp Day's tokens from the first frame;
/// `record.toggle = "F9"` shows in the hints; a conflict toasts once per
/// problem.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_theme_and_keys_come_from_the_users_config() {
    let mut nested = Nested::start("wp0b", "config", Options::default());
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    let config = nested.env.config_home().join("qrec");
    fs::create_dir_all(&config).expect("the config folder");
    fs::write(config.join("config.toml"), "theme = \"swamp-day\"\n").expect("config.toml");
    fs::write(
        config.join("keys.toml"),
        "[bindings]\n\"record.toggle\" = \"F9\"\n\"autocut.run\" = \"x\"\n",
    )
    .expect("keys.toml");
    nested.launch(&project, Launch::default());
    wait_for_gui_host(&nested, &project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
    let local = gui.local();
    assert_eq!(local["theme"], "swamp-day", "{local}");
    let hints = gui.ctl(json!({"op": "hints"}));
    assert_eq!(hints["items"][0]["keys"], json!([["F9"]]), "{hints}");
    let problems = local["keymapProblems"].as_array().map_or(0, Vec::len);
    assert!(problems > 0, "the x conflict is a problem: {local}");
    let log = gui.gui_log();
    let toasts = log
        .lines()
        .filter(|l| l.contains("toast: keys.toml has a problem"))
        .count();
    assert_eq!(toasts, problems, "one toast per problem");
    // Swamp Day before the first frame: its tokens were loaded before the
    // window was presented, and the grounds are its colors.
    let loaded = log
        .find("theme swamp-day at scale 1 before the first frame")
        .expect("the theme line");
    let presented = log
        .find("the window is presented")
        .expect("the present line");
    assert!(loaded < presented);
    let shot = nested.screenshot("swamp-day");
    assert_eq!(pixel(&shot, 8, 600), token("swamp-day", "bg"));
    assert_eq!(pixel(&shot, 1596, 976), token("swamp-day", "surface"));
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

// --------------------------------------------------------------- adaptive

fn adaptive(case: &str, width: u32, height: u32) -> (Nested, PathBuf) {
    let mut nested = Nested::start("wp0b", case, Options::size(width, height));
    let project = fixture(&nested, "rust-traits-talk", DesignVariant::Base);
    nested.launch(&project, Launch::default());
    wait_for_gui_host(&nested, &project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
    (nested, project)
}

/// G§3.15 at 1300×1000: the panel docks at 320 (Medium).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn medium_windows_dock_the_panel_at_320() {
    let (mut nested, _) = adaptive("adaptive-1300", 1300, 1000);
    let gui = nested.window(0);
    let regions = gui.regions(&[]);
    assert_regions(
        &regions,
        &[
            ("region-header", [0.0, 0.0, 1300.0, 48.0]),
            ("region-stage", [0.0, 48.0, 980.0, 608.0]),
            ("region-panel", [980.0, 48.0, 320.0, 920.0]),
            ("region-timeline", [0.0, 656.0, 980.0, 312.0]),
            ("region-hintbar", [0.0, 968.0, 1300.0, 32.0]),
        ],
        &WP0B_REGIONS,
        1.0,
    );
    assert_eq!(gui.local()["layout"]["width"], "Medium");
    nested.screenshot("open");
}

/// G§3.15 at 1000×1000: the panel is an overlay sheet the header's layers
/// button toggles; `Esc` closes it.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn narrow_windows_show_the_panel_as_a_sheet() {
    let (mut nested, _) = adaptive("adaptive-1000", 1000, 1000);
    let gui = nested.window(0);
    let regions = gui.regions(&["layers-button"]);
    assert!(!regions.contains_key("region-panel"), "{regions:?}");
    assert_regions(
        &regions,
        &[
            ("region-stage", [0.0, 48.0, 1000.0, 608.0]),
            ("region-timeline", [0.0, 656.0, 1000.0, 312.0]),
        ],
        &["region-stage", "region-timeline"],
        1.0,
    );
    assert_eq!(gui.local()["layout"]["width"], "Narrow");
    nested.screenshot("closed");
    let [x, y, w, h] = regions["layers-button"];
    nested.pointer().click(x + w / 2.0, y + h / 2.0);
    // The click and the test channel are two sockets: wait for the sheet.
    let open = wait_for_region(nested.window(0), "region-panel", true);
    let [px, py, pw, ph] = open["region-panel"];
    assert_eq!(
        (px + pw, py, pw, ph),
        (1000.0, 48.0, 384.0, 920.0),
        "{open:?}"
    );
    nested.window(0).wait_idle();
    nested.screenshot("sheet");
    nested.keys(&["-k", "Escape"]);
    wait_for_region(nested.window(0), "region-panel", false);
}

/// Polls `regions` until `name` is there (or gone), at most 10 s.
fn wait_for_region(
    gui: &gui_support::Gui,
    name: &str,
    present: bool,
) -> std::collections::BTreeMap<String, [f64; 4]> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        gui.wait_idle();
        let regions = gui.regions(&[]);
        if regions.contains_key(name) == present {
            return regions;
        }
        assert!(
            Instant::now() < deadline,
            "{name} never {}: {regions:?}",
            if present { "appeared" } else { "went" }
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// G§3.15 at 1600×800: the timeline keeps 232 px (compact).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn short_windows_keep_a_232_px_timeline() {
    let (mut nested, _) = adaptive("adaptive-800", 1600, 800);
    let gui = nested.window(0);
    let regions = gui.regions(&[]);
    assert_regions(
        &regions,
        &[
            ("region-header", [0.0, 0.0, 1600.0, 48.0]),
            ("region-stage", [0.0, 48.0, 1216.0, 488.0]),
            ("region-panel", [1216.0, 48.0, 384.0, 720.0]),
            ("region-timeline", [0.0, 536.0, 1216.0, 232.0]),
            ("region-hintbar", [0.0, 768.0, 1600.0, 32.0]),
        ],
        &WP0B_REGIONS,
        1.0,
    );
    assert_eq!(gui.local()["layout"]["compact"], true);
    nested.screenshot("open");
}
