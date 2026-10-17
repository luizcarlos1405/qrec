//! The main-loop probe (G§8.6, `QREC_GUI_PROBE=1`): a 10 ms GLib timeout
//! whose runs come late when something holds the main loop. Every drift
//! over 50 ms is logged with what held it: the longest guarded dispatch
//! (`guard::catch`'s `what`, the outermost one: the callback GLib or GTK was
//! running) since the probe's previous run. The window's quit logs a
//! summary and stops it, so the close's own work isn't counted. The smoke
//! run turns it on and reads the log (G§8.5): a stall fails it only under
//! `QREC_TEST_WLR=gles2` (with pixman the compositor, the software renderer
//! and ffmpeg share the CPU).
//!
//! The arithmetic ([`Meter`], [`Longest`]) is GTK-free; [`enter`] and
//! [`leave`] are `guard::catch`'s hooks, a thread-local read when the probe
//! is off.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use crate::guard;

/// How often the probe's timeout asks to run.
pub const INTERVAL: Duration = Duration::from_millis(10);
/// A drift past [`INTERVAL`] over this is a stall.
pub const STALL: Duration = Duration::from_millis(50);
/// The probe's own timeout (never what held the loop).
const WHAT: &str = "probe: the main loop";
/// The log lines' prefix (the smoke run reads them).
pub const LOG_PREFIX: &str = "main-loop probe:";

/// Whether `QREC_GUI_PROBE` asks for the main-loop probe: `1` or `main` in
/// its comma-separated list (`timeline` is the frame-time probe's).
pub fn requested() -> bool {
    std::env::var("QREC_GUI_PROBE").is_ok_and(|v| asks_for_main(&v))
}

fn asks_for_main(value: &str) -> bool {
    value.split(',').any(|p| matches!(p.trim(), "1" | "main"))
}

/// The drift of a repeating timer: each run's lateness past its interval,
/// and the stalls among them.
#[derive(Debug, Clone)]
pub struct Meter {
    interval: Duration,
    threshold: Duration,
    last: Option<Instant>,
    started: Option<Instant>,
    /// Drifts over the threshold so far.
    pub stalls: u64,
    /// The longest drift so far.
    pub worst: Duration,
}

impl Meter {
    pub fn new(interval: Duration, threshold: Duration) -> Self {
        Self {
            interval,
            threshold,
            last: None,
            started: None,
            stalls: 0,
            worst: Duration::ZERO,
        }
    }

    /// A run at `now`: its drift when that is a stall (over the threshold).
    pub fn tick(&mut self, now: Instant) -> Option<Duration> {
        self.started.get_or_insert(now);
        let drift = self.last.map_or(Duration::ZERO, |last| {
            now.saturating_duration_since(last)
                .saturating_sub(self.interval)
        });
        self.last = Some(now);
        self.worst = self.worst.max(drift);
        (drift > self.threshold).then(|| {
            self.stalls += 1;
            drift
        })
    }

    /// How long it has run (from its first tick to `now`).
    pub fn elapsed(&self, now: Instant) -> Duration {
        self.started
            .map_or(Duration::ZERO, |s| now.saturating_duration_since(s))
    }
}

/// The longest dispatch since it was last taken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Longest(Option<(String, Duration)>);

impl Longest {
    /// `what` ran `took`.
    pub fn record(&mut self, what: &str, took: Duration) {
        if self.0.as_ref().is_none_or(|(_, t)| took > *t) {
            self.0 = Some((what.to_owned(), took));
        }
    }

    /// The longest one, and a fresh start.
    pub fn take(&mut self) -> Option<(String, Duration)> {
        self.0.take()
    }
}

/// A stall's log line.
pub fn stall_line(drift: Duration, longest: Option<&(String, Duration)>) -> String {
    let held = match longest {
        Some((what, took)) => format!("{what} ran {} ms", took.as_millis()),
        None => "no guarded callback ran (GTK's own layout or paint)".to_owned(),
    };
    format!(
        "{LOG_PREFIX} a {} ms stall (the timer ran {} ms late); the longest dispatch: {held}",
        drift.as_millis() + INTERVAL.as_millis(),
        drift.as_millis()
    )
}

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    /// How deep in guarded callbacks this thread is.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static LONGEST: RefCell<Longest> = RefCell::new(Longest::default());
    static METER: RefCell<Option<Meter>> = const { RefCell::new(None) };
    static TIMER: guard::Slot = guard::Slot::new();
}

/// `guard::catch`'s hook before a callback: its start when the probe runs
/// and it is the outermost one.
pub(crate) fn enter() -> Option<Instant> {
    if !ON.with(Cell::get) {
        return None;
    }
    let depth = DEPTH.with(|d| d.replace(d.get() + 1));
    (depth == 0).then(Instant::now)
}

/// `guard::catch`'s hook after it (`started` is [`enter`]'s).
pub(crate) fn leave(what: &str, started: Option<Instant>) {
    if !ON.with(Cell::get) {
        return;
    }
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    if let Some(started) = started
        && what != WHAT
    {
        LONGEST.with(|l| l.borrow_mut().record(what, started.elapsed()));
    }
}

/// Starts the probe on the main thread (once).
pub fn start() {
    if ON.with(|on| on.replace(true)) {
        return;
    }
    METER.with(|m| *m.borrow_mut() = Some(Meter::new(INTERVAL, STALL)));
    tracing::info!(
        "{LOG_PREFIX} on (every {} ms; stalls over {} ms)",
        INTERVAL.as_millis(),
        STALL.as_millis()
    );
    TIMER.with(|t| {
        t.timeout(INTERVAL, WHAT, || {
            let now = Instant::now();
            let stall = METER.with(|m| m.borrow_mut().as_mut().and_then(|m| m.tick(now)));
            let longest = LONGEST.with(|l| l.borrow_mut().take());
            if let Some(drift) = stall {
                tracing::warn!("{}", stall_line(drift, longest.as_ref()));
            }
            gtk::glib::ControlFlow::Continue
        });
    });
}

/// Stops it with a summary line (the quit: its work isn't counted).
pub fn stop() {
    if !ON.with(|on| on.replace(false)) {
        return;
    }
    TIMER.with(guard::Slot::stop);
    DEPTH.with(|d| d.set(0));
    if let Some(meter) = METER.with(|m| m.borrow_mut().take()) {
        tracing::info!(
            "{LOG_PREFIX} {} stalls over {} ms in {} s; the worst timer run {} ms late",
            meter.stalls,
            STALL.as_millis(),
            meter.elapsed(Instant::now()).as_secs(),
            meter.worst.as_millis()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Only a run more than 50 ms past its 10 ms interval is a stall.
    #[test]
    fn a_stall_is_a_run_over_50_ms_late() {
        let t0 = Instant::now();
        let mut meter = Meter::new(INTERVAL, STALL);
        assert_eq!(
            meter.tick(t0),
            None,
            "the first run has nothing to be late for"
        );
        assert_eq!(meter.tick(t0 + ms(10)), None);
        assert_eq!(meter.tick(t0 + ms(70)), None, "50 ms late is no stall");
        assert_eq!(meter.tick(t0 + ms(141)), Some(ms(61)));
        assert_eq!(meter.tick(t0 + ms(151)), None);
        assert_eq!((meter.stalls, meter.worst), (1, ms(61)));
        assert_eq!(meter.elapsed(t0 + ms(1151)), ms(1151));
    }

    /// The probe names the longest dispatch since its previous run.
    #[test]
    fn the_longest_dispatch_is_named() {
        let mut longest = Longest::default();
        longest.record("a GUI action", ms(3));
        longest.record("an inbound item", ms(80));
        longest.record("timeline: a frame", ms(12));
        let taken = longest.take();
        assert_eq!(taken, Some(("an inbound item".to_owned(), ms(80))));
        assert_eq!(longest.take(), None, "taking starts afresh");
        let line = stall_line(ms(75), taken.as_ref());
        assert_eq!(
            line,
            "main-loop probe: a 85 ms stall (the timer ran 75 ms late); the longest dispatch: \
             an inbound item ran 80 ms"
        );
        assert!(stall_line(ms(60), None).contains("no guarded callback ran"));
    }

    /// `QREC_GUI_PROBE=1` (G§8.6), or `main` in a list with the timeline's.
    #[test]
    fn the_variable_asks_for_it() {
        for (value, on) in [
            ("1", true),
            ("main", true),
            ("timeline,1", true),
            (" main , timeline", true),
            ("timeline", false),
            ("", false),
            ("0", false),
        ] {
            assert_eq!(asks_for_main(value), on, "{value:?}");
        }
    }

    /// The hooks record only the outermost dispatch, only while it runs.
    #[test]
    fn the_hooks_record_the_outermost_dispatch_while_on() {
        assert_eq!(enter(), None, "off: nothing is timed");
        leave("nothing", None);
        ON.with(|on| on.set(true));
        let outer = enter();
        assert!(outer.is_some());
        let inner = enter();
        assert_eq!(inner, None, "a nested callback isn't timed on its own");
        leave("inner", inner);
        std::thread::sleep(ms(2));
        leave("outer", outer);
        let taken = LONGEST.with(|l| l.borrow_mut().take());
        assert_eq!(taken.map(|(what, _)| what).as_deref(), Some("outer"));
        let own = enter();
        leave(WHAT, own);
        assert_eq!(
            LONGEST.with(|l| l.borrow_mut().take()),
            None,
            "not its own timer"
        );
        ON.with(|on| on.set(false));
        assert_eq!(DEPTH.with(Cell::get), 0);
    }
}
