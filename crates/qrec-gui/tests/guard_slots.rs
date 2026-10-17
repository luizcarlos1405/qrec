//! `guard::Slot` on the default main context, alone in its own test binary
//! (it acquires the context): a slot empties however its callback ends, a
//! panic included, so its owner can start it again, and stopping it never
//! removes a source GLib already destroyed (`SourceId::remove` panics on
//! one: the timeline's tick stayed dead after a caught panic, and a meter's
//! or the export clock's next stop panicked; review R09, R65).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use qrec_gui::guard::{self, Slot};

/// Iterates the default main context until `done` (5 s at most).
fn until(what: &str, done: impl Fn() -> bool) {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "{what} never happened");
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_slot_empties_however_its_callback_ends() {
    let context = glib::MainContext::default();
    let _owner = context.acquire().expect("the default main context");
    let slot = Slot::new();

    // A repeating source that panics: caught, it stops, the slot empties,
    // stopping it is safe and a new one starts.
    let before = guard::panics();
    slot.timeout(Duration::ZERO, "test: a panicking source", || {
        panic!("boom")
    });
    assert!(slot.is_running());
    until("the panic", || guard::panics() == before + 1);
    until("the slot emptying", || !slot.is_running());
    slot.stop();
    let ticks = Rc::new(Cell::new(0));
    let seen = Rc::clone(&ticks);
    slot.timeout(Duration::ZERO, "test: a source", move || {
        seen.set(seen.get() + 1);
        if seen.get() < 3 {
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
    until("three turns", || ticks.get() == 3);
    until("its end", || !slot.is_running());

    // A one-shot: empty while it runs (so it can start the next) and after.
    let fired = Rc::new(Cell::new(0));
    let once = Rc::new(Slot::new());
    let (seen, again) = (Rc::clone(&fired), Rc::downgrade(&once));
    once.timeout_once(Duration::ZERO, "test: a timer", move || {
        seen.set(seen.get() + 1);
        if let Some(slot) = again.upgrade() {
            assert!(!slot.is_running(), "empty while it runs");
            let seen = Rc::clone(&seen);
            slot.timeout_once(Duration::ZERO, "test: the next timer", move || {
                seen.set(seen.get() + 10);
            });
        }
    });
    until("both timers", || fired.get() == 11);
    assert!(!once.is_running());
    once.stop();

    // Stopped before it fires: it never does, and a second stop is safe.
    let late = Rc::new(Cell::new(false));
    let seen = Rc::clone(&late);
    slot.timeout_once(
        Duration::from_millis(50),
        "test: a stopped timer",
        move || seen.set(true),
    );
    slot.stop();
    slot.stop();
    let deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < deadline {
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!late.get());
    assert_eq!(guard::panics(), before + 1, "only the one panic");
}
