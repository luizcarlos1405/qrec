//! The main-loop probe (G§8.6, `QREC_GUI_PROBE=1`): a 10 ms GLib timeout
//! on the main thread that logs every drift over 50 ms (a stall: the loop
//! was busy) with the dispatch that held it: the slowest guarded callback
//! (its G§3.12 name, timed by `guard::catch`) that ran since the previous
//! tick, with the slowest one it ran inside it, and so on down (`the queue's
//! drain (84 ms) > an inbound item (83 ms) > a subscriber (80 ms)`). A stall
//! that no guarded callback explains was GTK's own work: layout, styles,
//! drawing.
//!
//! It starts once the window is built (the end of `activate`), counts its
//! ticks, keeps the newest [`KEPT`] stalls, logs a summary when the app
//! quits ([`finish`]), and the test channel's `probe` op reports it
//! ([`report`]): the smoke run (G§8.5) fails on a stall only under gles2
//! (with pixman the compositor, llvmpipe and ffmpeg share the CPU).
//!
//! Off (the default) it costs a thread-local read per guarded callback.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use gtk::glib;
use serde_json::{Value, json};

use crate::guard;

/// The probe's period.
pub const PERIOD: Duration = Duration::from_millis(10);
/// A tick later than this past its period is a stall.
pub const STALL: Duration = Duration::from_millis(50);
/// How many stalls the report keeps (the newest).
pub const KEPT: usize = 64;
/// The probe's own callback, which it doesn't time.
const TICK: &str = "main-loop probe: a tick";

/// Whether a `QREC_GUI_PROBE` value asks for this probe: `1` or `main-loop`
/// among its comma-separated names (`timeline` is the timeline's
/// frame-time probe, G§4.5).
pub fn wanted(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        v.split(',')
            .map(str::trim)
            .any(|p| p == "1" || p == "main-loop")
    })
}

/// A guarded callback that ran, its time, and the slowest one it ran inside
/// it, and so on down: the outermost first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub chain: Vec<(String, Duration)>,
}

impl Ran {
    /// The outermost callback's time.
    pub fn took(&self) -> Duration {
        self.chain.first().map_or(Duration::ZERO, |(_, took)| *took)
    }
}

impl fmt::Display for Ran {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (what, took)) in self.chain.iter().enumerate() {
            if i > 0 {
                f.write_str(" > ")?;
            }
            write!(f, "{what} ({} ms)", took.as_millis())?;
        }
        Ok(())
    }
}

/// A tick that came late.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stall {
    /// When the tick came, since the probe started.
    pub at: Duration,
    /// How much later than its period.
    pub drift: Duration,
    /// The slowest dispatch since the tick before (`None`: no guarded
    /// callback ran, GTK's own work held the loop).
    pub ran: Option<Ran>,
}

impl fmt::Display for Stall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a stall of {} ms, {:.1} s in; the slowest dispatch since the last tick: ",
            self.drift.as_millis(),
            self.at.as_secs_f64()
        )?;
        match &self.ran {
            Some(ran) => write!(f, "{ran}"),
            None => f.write_str("none guarded (GTK's own work: layout, styles, drawing)"),
        }
    }
}

/// A guarded callback that is running.
#[derive(Debug)]
struct Frame {
    start: Instant,
    /// The slowest callback that ended inside it.
    inner: Option<Ran>,
}

/// The probe's counts, its stalls and what runs now.
#[derive(Debug)]
pub struct Probe {
    started: Instant,
    last: Instant,
    ticks: u64,
    stalls: u64,
    longest: Duration,
    kept: VecDeque<Stall>,
    /// The guarded callbacks running now, the outermost first.
    stack: Vec<Frame>,
    /// The slowest outermost callback that ended since the last tick.
    slowest: Option<Ran>,
}

impl Probe {
    /// A probe started at `now` (its first tick is due a period later).
    pub fn new(now: Instant) -> Self {
        Self {
            started: now,
            last: now,
            ticks: 0,
            stalls: 0,
            longest: Duration::ZERO,
            kept: VecDeque::new(),
            stack: Vec::new(),
            slowest: None,
        }
    }

    /// A guarded callback starts at `now`.
    fn enter(&mut self, now: Instant) {
        self.stack.push(Frame {
            start: now,
            inner: None,
        });
    }

    /// The innermost running callback, `what`, ends at `now`: it is the
    /// slowest of its level so far, or not.
    fn leave(&mut self, what: &str, now: Instant) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        let took = now.saturating_duration_since(frame.start);
        let best = match self.stack.last_mut() {
            Some(parent) => &mut parent.inner,
            None => &mut self.slowest,
        };
        if best.as_ref().is_none_or(|b| took > b.took()) {
            let mut chain = vec![(what.to_owned(), took)];
            chain.extend(frame.inner.map(|inner| inner.chain).unwrap_or_default());
            *best = Some(Ran { chain });
        }
    }

    /// The tick at `now`: a stall when it came more than [`STALL`] after
    /// its period.
    pub fn tick(&mut self, now: Instant) -> Option<Stall> {
        let interval = now.saturating_duration_since(self.last);
        self.last = now;
        self.ticks += 1;
        let ran = self.slowest.take();
        let drift = interval.saturating_sub(PERIOD);
        if drift <= STALL {
            return None;
        }
        self.stalls += 1;
        self.longest = self.longest.max(drift);
        let stall = Stall {
            at: now.saturating_duration_since(self.started),
            drift,
            ran,
        };
        if self.kept.len() == KEPT {
            self.kept.pop_front();
        }
        self.kept.push_back(stall.clone());
        Some(stall)
    }

    /// What the test channel's `probe` op answers.
    pub fn report(&self, now: Instant) -> Value {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        json!({
            "ok": true,
            "on": true,
            "periodMs": ms(PERIOD),
            "stallMs": ms(STALL),
            "runningMs": ms(now.saturating_duration_since(self.started)),
            "ticks": self.ticks,
            "stalls": self.stalls,
            "longestMs": ms(self.longest),
            "kept": self.kept.iter().map(|s| json!({
                "atMs": ms(s.at),
                "driftMs": ms(s.drift),
                "ran": s.ran.as_ref().map(ToString::to_string),
            })).collect::<Vec<_>>(),
        })
    }

    /// The line [`finish`] logs.
    pub fn summary(&self, now: Instant) -> String {
        format!(
            "main-loop probe: {} ticks in {:.1} s, {} stalls over {} ms, the longest {} ms",
            self.ticks,
            now.saturating_duration_since(self.started).as_secs_f64(),
            self.stalls,
            STALL.as_millis(),
            self.longest.as_millis()
        )
    }
}

thread_local! {
    /// The probe runs (a cheap check for every guarded callback).
    static ON: Cell<bool> = const { Cell::new(false) };
    static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    static SLOT: guard::Slot = guard::Slot::new();
}

/// Times one guarded callback while the probe runs: it ends when this is
/// dropped. `guard::catch` holds one around every callback.
pub struct Timing<'a> {
    what: &'a str,
}

impl Drop for Timing<'_> {
    fn drop(&mut self) {
        let now = Instant::now();
        PROBE.with(|p| {
            if let Ok(mut probe) = p.try_borrow_mut()
                && let Some(probe) = probe.as_mut()
            {
                probe.leave(self.what, now);
            }
        });
    }
}

/// A guarded callback named `what` starts (`None` while the probe is off,
/// and for its own tick).
pub fn enter(what: &str) -> Option<Timing<'_>> {
    if !ON.with(Cell::get) || what == TICK {
        return None;
    }
    let now = Instant::now();
    PROBE.with(|p| {
        let mut probe = p.try_borrow_mut().ok()?;
        probe.as_mut()?.enter(now);
        Some(Timing { what })
    })
}

/// Starts the probe when `QREC_GUI_PROBE` asks for it (main thread).
pub fn start_if_wanted() {
    if wanted(std::env::var("QREC_GUI_PROBE").ok().as_deref()) {
        start();
    }
}

/// Starts the probe on this thread's main loop (once; main thread).
pub fn start() {
    if !begin(Instant::now()) {
        return;
    }
    tracing::info!(
        "main-loop probe: on (a {} ms timeout; a tick over {} ms late is a stall)",
        PERIOD.as_millis(),
        STALL.as_millis()
    );
    SLOT.with(|slot| {
        slot.timeout(PERIOD, TICK, || {
            let stall = PROBE.with(|p| p.borrow_mut().as_mut()?.tick(Instant::now()));
            if let Some(stall) = stall {
                tracing::warn!("main-loop probe: {stall}");
            }
            glib::ControlFlow::Continue
        });
    });
}

/// Turns the probe's bookkeeping on (no timeout yet): `false` when it
/// already was.
fn begin(now: Instant) -> bool {
    let fresh = PROBE.with(|p| {
        let mut probe = p.borrow_mut();
        if probe.is_some() {
            return false;
        }
        *probe = Some(Probe::new(now));
        true
    });
    if fresh {
        ON.with(|on| on.set(true));
    }
    fresh
}

/// Whether the probe runs on this thread.
pub fn running() -> bool {
    ON.with(Cell::get)
}

/// The test channel's `probe` op: the counts and the stalls kept, or `on:
/// false`.
pub fn report() -> Value {
    PROBE.with(|p| match p.borrow().as_ref() {
        Some(probe) => probe.report(Instant::now()),
        None => json!({"ok": true, "on": false}),
    })
}

/// The app quits: the summary goes to the log and the timeout stops (while
/// the main loop still runs).
pub fn finish() {
    let summary = PROBE.with(|p| {
        p.borrow()
            .as_ref()
            .map(|probe| probe.summary(Instant::now()))
    });
    if let Some(summary) = summary {
        tracing::info!("{summary}");
    }
    SLOT.with(guard::Slot::stop);
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn only_1_or_main_loop_starts_it() {
        assert!(wanted(Some("1")));
        assert!(wanted(Some("main-loop")));
        assert!(wanted(Some("timeline, 1")));
        assert!(!wanted(Some("timeline")));
        assert!(!wanted(Some("0")));
        assert!(!wanted(Some("")));
        assert!(!wanted(None));
    }

    /// A tick within its period and 50 ms is no stall; a later one is, with
    /// the slowest dispatch since the tick before (and only that one).
    #[test]
    fn a_late_tick_is_a_stall_with_the_dispatch_that_held_the_loop() {
        let t0 = Instant::now();
        let mut probe = Probe::new(t0);
        assert_eq!(probe.tick(t0 + 10 * MS), None);
        assert_eq!(probe.tick(t0 + 70 * MS), None, "50 ms late is no stall");
        let at = t0 + 70 * MS;
        probe.enter(at);
        probe.enter(at + MS);
        probe.leave("a subscriber", at + 79 * MS);
        probe.leave("an inbound item", at + 80 * MS);
        probe.enter(at + 80 * MS);
        probe.leave("a faster one", at + 81 * MS);
        let stall = probe.tick(at + 90 * MS).expect("80 ms late");
        assert_eq!(stall.drift, 80 * MS);
        assert_eq!(stall.at, 160 * MS);
        let ran = stall.ran.expect("what ran");
        assert_eq!(
            ran.to_string(),
            "an inbound item (80 ms) > a subscriber (78 ms)"
        );
        assert_eq!(
            Stall {
                at: 2500 * MS,
                drift: 61 * MS,
                ran: None
            }
            .to_string(),
            "a stall of 61 ms, 2.5 s in; the slowest dispatch since the last tick: none \
             guarded (GTK's own work: layout, styles, drawing)"
        );
        // The next stall doesn't blame the callbacks of the one before.
        let stall = probe.tick(at + 200 * MS).expect("100 ms late");
        assert_eq!(stall.ran, None);
        assert_eq!(probe.stalls, 2);
        assert_eq!(probe.longest, 100 * MS);
        let report = probe.report(at + 200 * MS);
        assert_eq!(report["ticks"], 4);
        assert_eq!(report["stalls"], 2);
        assert_eq!(report["kept"][0]["driftMs"], 80.0);
        assert!(probe.summary(at + 200 * MS).contains("2 stalls over 50 ms"));
    }

    #[test]
    fn it_keeps_the_newest_stalls() {
        let t0 = Instant::now();
        let mut probe = Probe::new(t0);
        let mut now = t0;
        for _ in 0..KEPT + 5 {
            now += 100 * MS;
            assert!(probe.tick(now).is_some());
        }
        assert_eq!(probe.kept.len(), KEPT);
        assert_eq!(probe.stalls, (KEPT + 5) as u64);
        assert_eq!(probe.kept.back().map(|s| s.at), Some(now - t0));
    }

    /// `guard::catch` times every guarded callback while the probe runs:
    /// the slowest one and the slowest inside it, by their G§3.12 names.
    #[test]
    fn guarded_callbacks_are_timed_by_their_names() {
        assert!(enter("before the probe").is_none(), "off: nothing is timed");
        assert!(begin(Instant::now()));
        assert!(running());
        guard::run("test: outer", || {
            guard::run("test: quick", || ());
            guard::run("test: slow", || std::thread::sleep(30 * MS));
        });
        guard::run::<bool>("test: a panic", || panic!("caught"));
        let ran = PROBE
            .with(|p| p.borrow_mut().as_mut().and_then(|p| p.slowest.take()))
            .expect("the slowest callback");
        let names: Vec<&str> = ran.chain.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["test: outer", "test: slow"]);
        assert!(ran.took() >= 30 * MS, "{ran}");
        // The panic unwound through its timing: nothing is left running.
        PROBE.with(|p| assert!(p.borrow().as_ref().is_some_and(|p| p.stack.is_empty())));
        assert!(enter(TICK).is_none(), "the probe's own tick isn't timed");
    }
}
