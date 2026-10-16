//! The autocut page's dry runs (D-15), GTK-free: at most one in flight; a
//! change made meanwhile is kept and sent once the reply arrives (the
//! newest wins); changes are debounced 250 ms; a reply that is no longer
//! the newest is ignored, as is everything after the page closed.
//!
//! The page asks [`DryRuns`] what to do and carries it out: send a
//! command, arm one timer, or nothing. Time is milliseconds on a monotonic
//! clock the caller reads (not the animation clock, which test mode
//! freezes).

use qrec_core::Command;

/// The debounce of a tuning change.
pub const DEBOUNCE_MS: u64 = 250;

/// What the page does next.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Send `cmd` as dry run number `id` (its reply comes back with it).
    Send { id: u64, cmd: Command },
    /// Ask again at `at_ms` (one timer; a later step replaces it).
    WaitUntil { at_ms: u64 },
    /// Nothing to do until a reply or a change.
    Idle,
}

/// One change waiting to be sent.
#[derive(Debug, Clone, PartialEq)]
struct Pending {
    cmd: Command,
    due_ms: u64,
}

/// The dry-run scheduler of one page (it lives as long as the panel, so a
/// reply from a page closed earlier is recognized).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DryRuns {
    open: bool,
    next_id: u64,
    /// The dry run in flight (the core analyzes it), even one the page
    /// closed on: a new page waits for it.
    in_flight: Option<u64>,
    /// The newest one sent while the page is open.
    latest: Option<u64>,
    pending: Option<Pending>,
}

impl DryRuns {
    pub fn new() -> Self {
        Self::default()
    }

    /// The page opened: its first dry run goes at once (after one still in
    /// flight from before, if any).
    pub fn open(&mut self, cmd: Command, now_ms: u64) -> Step {
        self.open = true;
        self.latest = None;
        self.pending = Some(Pending {
            cmd,
            due_ms: now_ms,
        });
        self.poll(now_ms)
    }

    /// The draft or the scope changed: send `cmd` 250 ms after the last
    /// change, once nothing is in flight.
    pub fn change(&mut self, cmd: Command, now_ms: u64) -> Step {
        if !self.open {
            return Step::Idle;
        }
        self.pending = Some(Pending {
            cmd,
            due_ms: now_ms + DEBOUNCE_MS,
        });
        self.poll(now_ms)
    }

    /// The timer fired (or the page asks).
    pub fn poll(&mut self, now_ms: u64) -> Step {
        if !self.open || self.in_flight.is_some() {
            return Step::Idle;
        }
        match &self.pending {
            None => Step::Idle,
            Some(p) if p.due_ms > now_ms => Step::WaitUntil { at_ms: p.due_ms },
            Some(_) => {
                let Some(p) = self.pending.take() else {
                    return Step::Idle;
                };
                self.next_id += 1;
                let id = self.next_id;
                self.in_flight = Some(id);
                self.latest = Some(id);
                Step::Send { id, cmd: p.cmd }
            }
        }
    }

    /// Dry run `id` answered: whether the page shows that answer (it is the
    /// newest and nothing newer waits), and what to do next.
    pub fn replied(&mut self, id: u64, now_ms: u64) -> (bool, Step) {
        if self.in_flight == Some(id) {
            self.in_flight = None;
        }
        let show = self.open && self.latest == Some(id) && self.pending.is_none();
        if self.latest == Some(id) {
            // Answered: a repeat of it is nobody's.
            self.latest = None;
        }
        (show, self.poll(now_ms))
    }

    /// The page closed: nothing more is sent, and the replies still coming
    /// are ignored.
    pub fn close(&mut self) {
        self.open = false;
        self.latest = None;
        self.pending = None;
    }

    /// A dry run runs or waits: the summary shows "Analyzing…".
    pub fn analyzing(&self) -> bool {
        self.open && (self.in_flight.is_some() || self.pending.is_some())
    }

    /// The dry run in flight, if any (one at most).
    pub fn in_flight(&self) -> Option<u64> {
        self.in_flight
    }
}

#[cfg(test)]
mod tests {
    use qrec_core::protocol::command::{AutocutOverrides, ClipScope};

    use super::*;

    fn run(threshold: f64) -> Command {
        Command::Autocut {
            scope: ClipScope::All,
            overrides: AutocutOverrides {
                threshold_db: Some(threshold),
                ..AutocutOverrides::default()
            },
            dry_run: true,
        }
    }

    fn sent(step: &Step) -> Option<(u64, f64)> {
        match step {
            Step::Send {
                id,
                cmd: Command::Autocut { overrides, .. },
            } => Some((*id, overrides.threshold_db.unwrap_or_default())),
            _ => None,
        }
    }

    #[test]
    fn the_first_dry_run_goes_at_once() {
        let mut runs = DryRuns::new();
        let step = runs.open(run(-35.0), 0);
        assert_eq!(sent(&step), Some((1, -35.0)));
        assert!(runs.analyzing());
        assert_eq!(runs.replied(1, 900), (true, Step::Idle));
        assert!(!runs.analyzing());
    }

    #[test]
    fn changes_are_debounced_and_one_runs_at_a_time() {
        let mut runs = DryRuns::new();
        runs.open(run(-35.0), 0);
        // Changes while #1 runs: kept, the newest wins.
        assert_eq!(runs.change(run(-40.0), 100), Step::Idle);
        assert_eq!(runs.change(run(-42.0), 150), Step::Idle);
        assert_eq!(runs.in_flight(), Some(1));
        // #1 answers after the debounce: stale (a newer change waits), and
        // the newest goes at once.
        let (show, step) = runs.replied(1, 600);
        assert!(!show, "a change waits: the answer is stale");
        assert_eq!(sent(&step), Some((2, -42.0)));
        // A change right after: debounced 250 ms, and only once #2 is back.
        assert_eq!(runs.change(run(-45.0), 610), Step::Idle);
        let (show, step) = runs.replied(2, 700);
        assert!(!show);
        assert_eq!(step, Step::WaitUntil { at_ms: 860 });
        assert_eq!(runs.poll(800), Step::WaitUntil { at_ms: 860 });
        assert_eq!(sent(&runs.poll(860)), Some((3, -45.0)));
        let (show, step) = runs.replied(3, 1200);
        assert!(show, "the newest, nothing waiting");
        assert_eq!(step, Step::Idle);
        // With nothing in flight a change waits for its debounce only.
        assert_eq!(
            runs.change(run(-30.0), 2000),
            Step::WaitUntil { at_ms: 2250 }
        );
        assert_eq!(
            runs.change(run(-31.0), 2100),
            Step::WaitUntil { at_ms: 2350 }
        );
        assert_eq!(sent(&runs.poll(2350)), Some((4, -31.0)));
    }

    #[test]
    fn a_closed_page_ignores_its_replies() {
        let mut runs = DryRuns::new();
        runs.open(run(-35.0), 0);
        runs.change(run(-40.0), 10);
        runs.close();
        assert!(!runs.analyzing());
        assert_eq!(runs.change(run(-50.0), 20), Step::Idle);
        // #1 is still in the core: a new page waits for it.
        assert_eq!(runs.open(run(-36.0), 30), Step::Idle);
        let (show, step) = runs.replied(1, 40);
        assert!(!show, "the old page's answer");
        assert_eq!(sent(&step), Some((2, -36.0)));
        // Unknown and repeated replies change nothing.
        assert_eq!(runs.replied(7, 50), (false, Step::Idle));
        let (show, _) = runs.replied(2, 60);
        assert!(show);
        assert_eq!(runs.replied(2, 70), (false, Step::Idle));
    }
}
