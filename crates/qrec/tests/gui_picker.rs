//! What `qrec` opens and the project picker (SPEC §4.4, docs/GUI.md G§4.12)
//! in a nested headless sway (G§8.4): a project in the current folder, the
//! picker elsewhere, `projects.open_last`, and the picker opening a recent
//! project and making a new one. A project the picker opens replaces its
//! process, so the case's window goes on being the same pid.
//!
//! `cargo test -p qrec --test gui_picker -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon).
//! Shots and logs: `tmp/gui-shots/picker/`.

mod gui_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use gui_support::{Launch, Nested, Options, TOOL_TIMEOUT, poll, wait_for_gui_host};
use qrec_shell::paths::UserDirs;
use qrec_shell::projects;
use qrec_shell::testkit::design::{DesignVariant, design_fixture};
use qrec_shell::testkit::run_tool;

const AREA: &str = "picker";
const OPEN: Duration = Duration::from_secs(60);

fn dirs(nested: &Nested) -> UserDirs {
    UserDirs::resolve(&nested.env.paths_env()).expect("the user dirs")
}

fn fixture(nested: &Nested, name: &str) -> PathBuf {
    let dir = nested.env.empty_dir(name);
    design_fixture(&dir, DesignVariant::Base);
    fs::canonicalize(dir).expect("the project")
}

fn busy_folder(nested: &Nested) -> PathBuf {
    let dir = nested.env.empty_dir("Downloads");
    fs::write(dir.join("notes.txt"), "x").expect("a file");
    fs::canonicalize(dir).expect("the folder")
}

/// `dirs`, the first one opened last.
fn remember(nested: &Nested, list: &[&Path]) {
    let user = dirs(nested);
    for dir in list.iter().rev() {
        projects::note_opened(&user, dir, Utc::now());
    }
}

fn last_project(nested: &Nested) -> Option<String> {
    projects::read_recents(&dirs(nested))
        .last()
        .map(str::to_owned)
}

/// The picker logged that it is up.
fn wait_picker(nested: &Nested) {
    let log = dirs(nested).picker_log();
    poll(OPEN, "the project picker", || {
        fs::read_to_string(&log)
            .ok()?
            .contains("the project picker is up")
            .then_some(())
    });
    // Its first frame, and the folders looked at.
    std::thread::sleep(Duration::from_millis(1500));
}

/// One `wtype` run on the nested display. The picker has no test channel
/// to say when it has the keyboard: `-s` is the pause before the keys.
fn wtype(nested: &Nested, args: &[&str]) {
    let out = run_tool(
        nested.nested_tool("wtype").args(["-s", "1500"]).args(args),
        TOOL_TIMEOUT,
    );
    assert!(out.status.success(), "wtype {args:?}: {}", out.stderr);
}

/// A click in the middle of the widget named `name`, from a pixel aside
/// (GTK presses the widget its last motion found).
fn click(nested: &mut Nested, name: &str) {
    let regions = nested.window(0).regions(&[name]);
    let [x, y, w, h] = *regions
        .get(name)
        .unwrap_or_else(|| panic!("no {name}: {regions:?}"));
    let pointer = nested.pointer();
    pointer.move_to(x + w / 2.0 + 1.0, y + h / 2.0);
    pointer.click(x + w / 2.0, y + h / 2.0);
}

/// The window's project opened; then it is closed and exits 0.
fn opened_then_close(nested: &mut Nested, project: &Path) {
    let status = wait_for_gui_host(nested, project, OPEN);
    assert_eq!(status["data"]["host"], "gui", "{status}");
    poll(OPEN, "the recent list naming it", || {
        (last_project(nested).as_deref() == project.to_str()).then_some(())
    });
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(OPEN), Some(0));
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_project_in_the_current_folder_opens() {
    let mut nested = Nested::start(AREA, "here", Options::default());
    let project = fixture(&nested, "here");
    nested.launch(&project, Launch::bare_in(&project, &[]));
    opened_then_close(&mut nested, &project);
    assert!(!dirs(&nested).picker_log().exists(), "no picker");
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_picker_opens_a_recent_project() {
    let mut nested = Nested::start(AREA, "recent", Options::default());
    let elsewhere = busy_folder(&nested);
    let project = fixture(&nested, "rust-traits-talk");
    let other = fixture(&nested, "onion-layers");
    let gone = elsewhere.join("moved-away");
    remember(&nested, &[&gone, &project, &other]);
    nested.launch(&elsewhere, Launch::bare_in(&elsewhere, &[]));
    wait_picker(&nested);
    nested.screenshot("list");
    // The first row that can be opened is selected: the search has the
    // focus, and Enter there opens it.
    wtype(&nested, &["-k", "Return"]);
    opened_then_close(&mut nested, &project);
    let list = projects::read_recents(&dirs(&nested));
    assert_eq!(list.projects.len(), 3, "the moved folder stays listed");
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_picker_makes_a_new_project() {
    let mut nested = Nested::start(AREA, "new", Options::default());
    let elsewhere = nested.env.empty_dir("empty-here");
    let elsewhere = fs::canonicalize(elsewhere).expect("the folder");
    let projects_home = fs::canonicalize(nested.env.empty_dir("projects")).expect("the folder");
    let set = nested.cli_json(
        &elsewhere,
        &[
            "config",
            "set",
            "projects.folder",
            projects_home.to_str().unwrap(),
        ],
    );
    assert_eq!(set["ok"], true, "{set}");
    nested.launch(&elsewhere, Launch::bare_in(&elsewhere, &[]));
    wait_picker(&nested);
    nested.screenshot("empty");
    assert_eq!(
        fs::read_dir(&elsewhere).expect("the folder").count(),
        0,
        "an empty folder isn't made a project by just running qrec"
    );
    wtype(&nested, &["-M", "ctrl", "-k", "n", "-m", "ctrl"]);
    wtype(&nested, &["demo"]);
    nested.screenshot("new");
    wtype(&nested, &["-k", "Return"]);
    let project = projects_home.join("demo");
    poll(OPEN, "the new project's file", || {
        project.join("qrec.json").is_file().then_some(())
    });
    opened_then_close(&mut nested, &project);
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn open_last_skips_the_picker() {
    let mut nested = Nested::start(AREA, "open-last", Options::default());
    let elsewhere = busy_folder(&nested);
    let project = fixture(&nested, "last-one");
    remember(&nested, &[&project]);
    let set = nested.cli_json(&elsewhere, &["config", "set", "projects.open_last", "true"]);
    assert_eq!(set["ok"], true, "{set}");
    nested.launch(&project, Launch::bare_in(&elsewhere, &[]));
    opened_then_close(&mut nested, &project);
    assert!(!dirs(&nested).picker_log().exists(), "no picker");
}

/// `projects.open_last` on a project that can't be read is no dead end: X3
/// offers the picker, which opens in a process of its own.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_broken_last_project_offers_the_picker() {
    let mut nested = Nested::start(AREA, "broken-last", Options::default());
    let elsewhere = busy_folder(&nested);
    let project = fixture(&nested, "broken");
    fs::write(project.join("qrec.json"), "not json").expect("break it");
    remember(&nested, &[&project]);
    let set = nested.cli_json(&elsewhere, &["config", "set", "projects.open_last", "true"]);
    assert_eq!(set["ok"], true, "{set}");
    nested.launch(&project, Launch::bare_in(&elsewhere, &[]));
    poll(OPEN, "X3", || {
        let local = nested.window(0).local();
        (local["startup"]["phase"] == "blocked").then_some(())
    });
    nested.window(0).wait_idle();
    let labels = nested.window(0).labels("region-blocking");
    assert!(
        labels.iter().any(|l| l == "Open another project…"),
        "{labels:?}"
    );
    nested.screenshot("x3");
    click(&mut nested, "blocking-choose-project");
    wait_picker(&nested);
    assert_eq!(nested.window(0).wait_exit(OPEN), Some(1), "X3's exit code");
}

/// Settings › Project shows the machine's settings under the canvas, and
/// they reach `config.toml`.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_settings_page_has_the_projects_settings() {
    let mut nested = Nested::start(AREA, "settings", Options::default());
    let project = fixture(&nested, "rust-traits-talk");
    nested.launch(&project, Launch::default());
    wait_for_gui_host(&nested, &project, OPEN);
    poll(OPEN, "the project open", || {
        (nested.window(0).local()["startup"]["phase"] == "open").then_some(())
    });
    nested.window(0).wait_idle();
    click(&mut nested, "settings-button");
    poll(OPEN, "Settings", || {
        nested
            .window(0)
            .regions(&["settings-open-last"])
            .get("settings-open-last")
            .filter(|r| r[2] > 0.0)
            .map(|_| ())
    });
    nested.window(0).wait_idle();
    nested.screenshot("project");
    let labels = nested.window(0).labels("settings-all-projects");
    for want in [
        "All projects",
        "Open the last project on start",
        "Projects folder",
        "Not set",
    ] {
        assert!(labels.iter().any(|l| l == want), "{want}: {labels:?}");
    }
    click(&mut nested, "settings-open-last");
    let config = dirs(&nested).config_file();
    poll(OPEN, "the setting saved", || {
        fs::read_to_string(&config)
            .ok()?
            .contains("open_last = true")
            .then_some(())
    });
}

/// The palette's "Open or create a project…": the picker in a process of
/// its own, and this window carries on.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_palette_opens_the_picker_beside_the_window() {
    let mut nested = Nested::start(AREA, "palette", Options::default());
    let project = fixture(&nested, "rust-traits-talk");
    nested.launch(&project, Launch::default());
    wait_for_gui_host(&nested, &project, OPEN);
    poll(OPEN, "the project open", || {
        (nested.window(0).local()["startup"]["phase"] == "open").then_some(())
    });
    let reply = nested
        .window(0)
        .ctl(serde_json::json!({"op": "action", "id": "projects.open"}));
    assert_eq!(reply["ok"], true, "{reply}");
    wait_picker(&nested);
    assert_eq!(
        nested.window(0).local()["startup"]["phase"],
        "open",
        "the window is still on its project"
    );
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn pick_shows_the_picker_even_in_a_project() {
    let mut nested = Nested::start(AREA, "pick", Options::default());
    let project = fixture(&nested, "in-a-project");
    nested.launch(&project, Launch::bare_in(&project, &["--pick"]));
    wait_picker(&nested);
    wtype(&nested, &["-M", "ctrl", "-k", "q", "-m", "ctrl"]);
    assert_eq!(nested.window(0).wait_exit(OPEN), Some(0));
}
