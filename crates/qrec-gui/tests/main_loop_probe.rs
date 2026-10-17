//! The main-loop probe (G§8.6) on the default main context, alone in its own
//! test binary (it acquires the context): a guarded callback that holds the
//! loop for 150 ms is a stall the probe reports under the callback's name,
//! and its report counts the ticks.

use std::time::{Duration, Instant};

use gtk::glib;
use qrec_gui::{guard, probe};
use serde_json::Value;

/// Iterates the default main context until `done` (10 s at most).
fn until(what: &str, done: impl Fn() -> bool) {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{what} never happened");
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The stall the probe blamed on "test: a slow dispatch", if any (a busy
/// machine may add stalls of its own).
fn the_slow_dispatchs_stall() -> Option<Value> {
    probe::report()["kept"]
        .as_array()?
        .iter()
        .find(|s| {
            s["ran"]
                .as_str()
                .is_some_and(|r| r.starts_with("test: a slow dispatch ("))
        })
        .cloned()
}

#[test]
fn a_dispatch_that_holds_the_loop_is_a_stall_named_after_it() {
    let context = glib::MainContext::default();
    let _owner = context.acquire().expect("the default main context");
    assert_eq!(probe::report()["on"], false, "off until started");
    probe::start();
    assert!(probe::running());
    until("a few ticks", || {
        probe::report()["ticks"].as_u64() >= Some(5)
    });

    guard::idle_local_once("test: a slow dispatch", || {
        guard::run("test: its slow part", || {
            std::thread::sleep(Duration::from_millis(150));
        });
    });
    until("the slow dispatch's stall", || {
        the_slow_dispatchs_stall().is_some()
    });
    let stall = the_slow_dispatchs_stall().expect("found above");
    let ran = stall["ran"].as_str().unwrap_or_default();
    assert!(ran.contains(" > test: its slow part ("), "{ran}");
    let drift = stall["driftMs"].as_f64().unwrap_or_default();
    assert!(
        drift > 100.0,
        "the 150 ms dispatch held the tick back: {stall}"
    );
    let report = probe::report();
    assert!(report["stalls"].as_u64() >= Some(1), "{report}");
    assert!(report["longestMs"].as_f64() >= Some(drift), "{report}");
    probe::finish();
}
