//! The end-to-end smoke of the window (docs/GUI.md G§8.5, SPEC §17): the
//! GUI host on `design_fixture(Smoke)` in a nested headless sway, with the
//! fake capture backend. WP0b writes steps 1–3 and 10; WP10 completes it.
//!
//! Throughout: no `Crashed`, no `ERROR` line in the GUI log, no panic
//! caught, and the a11y walk is empty.
//!
//! `cargo test -p qrec --test gui_smoke -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon).

mod gui_support;

use std::time::{Duration, Instant};

use gui_support::{Launch, Nested, Options, wait_for_gui_host};
use qrec_shell::testkit::design::{DesignVariant, design_fixture};
use serde_json::Value;

fn clip_count(nested: &Nested, project: &std::path::Path) -> usize {
    let list = nested.cli_json(project, &["clip", "list"]);
    list["data"]
        .as_array()
        .or_else(|| list["data"]["clips"].as_array())
        .map_or_else(|| panic!("clip list: {list}"), Vec::len)
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn smoke_steps_1_to_3_and_10() {
    let mut nested = Nested::start("wp0b", "smoke", Options::default());
    let project = nested.env.empty_dir("smoke");
    design_fixture(&project, DesignVariant::Smoke);
    // The main-loop probe records the stalls (G§8.6).
    let launch = Launch::a11y().with_env("QREC_GUI_PROBE", "1");
    let pid = nested.launch(&project, launch).pid();

    // 1. The GUI host answers, with the window's pid.
    let status = wait_for_gui_host(&nested, &project, Duration::from_secs(60));
    assert_eq!(status["data"]["runtime"]["host"], "gui", "{status}");
    assert_eq!(
        status["data"]["pid"].as_u64(),
        Some(u64::from(pid)),
        "{status}"
    );
    let gui = nested.window(0);
    gui.wait_phase("open", Duration::from_secs(30));
    gui.wait_idle();

    // 2. A split through the GUI host: a seq, one more clip, a screenshot.
    let before = clip_count(&nested, &project);
    let split = nested.cli_json(&project, &["clip", "split", "--timeline", "--at", "45f"]);
    assert_eq!(split["ok"], true, "{split}");
    assert!(split["data"]["seq"].as_u64().is_some(), "{split}");
    assert_eq!(clip_count(&nested, &project), before + 1);
    nested.window(0).wait_idle();
    nested.screenshot("split");

    // 3. Undo, redo, undo.
    for step in ["undo", "redo", "undo"] {
        let reply = nested.cli_json(&project, &[step]);
        assert_eq!(reply["ok"], true, "{step}: {reply}");
    }
    assert_eq!(clip_count(&nested, &project), before);

    // Throughout: the a11y walk is empty.
    let gui = nested.window(0);
    gui.wait_idle();
    let a11y = gui.ctl(serde_json::json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "{a11y}");

    // 10. SIGTERM: 130 within 15 s, the lock free.
    let started = Instant::now();
    gui.signal("TERM");
    let code = gui.wait_exit(Duration::from_secs(15));
    assert_eq!(
        code,
        Some(130),
        "exit after SIGTERM ({:?})",
        started.elapsed()
    );
    let after = nested.cli_json(&project, &["status"]);
    assert_eq!(
        after["data"]["host"],
        Value::Null,
        "the lock is free: {after}"
    );

    let gui = nested.window(0);
    let log = gui.gui_log();
    assert!(!log.contains("Crashed"), "the session crashed:\n{log}");
    let errors: Vec<&str> = log.lines().filter(|l| l.contains(" ERROR ")).collect();
    assert!(errors.is_empty(), "ERROR lines in the GUI log: {errors:#?}");
    assert!(
        !log.contains("panicked") && !gui.output().contains("panicked"),
        "a panic: {}",
        gui.log.display()
    );
    // Throughout: the main loop's stalls, kept with the shots; one over
    // 50 ms fails the run under gles2 only (G§8.5).
    let stalls = nested.check_main_loop(0);
    eprintln!("main-loop stalls: {}", stalls.len());
}
