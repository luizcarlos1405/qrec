//! `Effect::StartTimer` / `CancelTimer` → `Event::TimerFired`, from a timer
//! thread through the `Inbox` (A§8.1). Owner: session.
//!
//! One thread per [`Timers`] keeps the pending deadlines and sleeps until
//! the next one (or a command). A cancelled timer never fires; a timer
//! that fired just before its cancel arrived is ignored by the core
//! (`on_event` ignores timers that don't match the current phase).

use std::collections::BTreeMap;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use qrec_core::{Event, TimerId};

use crate::msg::Inbox;

enum Command {
    Start(TimerId, Instant),
    Cancel(TimerId),
    Stop,
}

/// The timer service of one session.
pub struct Timers {
    tx: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Timers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timers").finish_non_exhaustive()
    }
}

impl Timers {
    /// Starts the timer thread; fired timers go to `inbox`.
    pub fn new(inbox: Inbox) -> Self {
        let (tx, rx) = unbounded();
        let thread = thread::Builder::new()
            .name("qrec-timers".into())
            .spawn(move || run(&rx, &inbox))
            .map_err(|e| tracing::error!("can't start the timer thread: {e}"))
            .ok();
        Self { tx, thread }
    }

    /// Fires `Event::TimerFired { timer }` after `after_ms`. Starting an id
    /// again replaces its deadline.
    pub fn start(&self, timer: TimerId, after_ms: u64) {
        let at = Instant::now() + Duration::from_millis(after_ms);
        let _ = self.tx.send(Command::Start(timer, at));
    }

    /// The timer won't fire (unless it already did).
    pub fn cancel(&self, timer: TimerId) {
        let _ = self.tx.send(Command::Cancel(timer));
    }
}

impl Drop for Timers {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(rx: &Receiver<Command>, inbox: &Inbox) {
    let mut pending: BTreeMap<TimerId, Instant> = BTreeMap::new();
    loop {
        let now = Instant::now();
        let due: Vec<TimerId> = pending
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            pending.remove(&id);
            if !inbox.event(Event::TimerFired { timer: id }) {
                return; // the session is gone
            }
        }
        let next = pending.values().min().copied();
        let command = match next {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(command) => command,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(command) => command,
                Err(_) => return,
            },
        };
        match command {
            Command::Start(id, at) => {
                pending.insert(id, at);
            }
            Command::Cancel(id) => {
                pending.remove(&id);
            }
            Command::Stop => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::Msg;

    fn fired(rx: &Receiver<Msg>, within: Duration) -> Option<TimerId> {
        match rx.recv_timeout(within) {
            Ok(Msg::Event(Event::TimerFired { timer })) => Some(timer),
            Ok(other) => panic!("unexpected {other:?}"),
            Err(_) => None,
        }
    }

    #[test]
    fn timers_fire_in_deadline_order() {
        let (inbox, rx) = Inbox::channel();
        let timers = Timers::new(inbox);
        let started = Instant::now();
        timers.start(TimerId(2), 120);
        timers.start(TimerId(1), 40);
        assert_eq!(fired(&rx, Duration::from_secs(2)), Some(TimerId(1)));
        assert!(started.elapsed() >= Duration::from_millis(40));
        assert_eq!(fired(&rx, Duration::from_secs(2)), Some(TimerId(2)));
        assert!(started.elapsed() >= Duration::from_millis(120));
        assert_eq!(fired(&rx, Duration::from_millis(100)), None);
    }

    #[test]
    fn cancelled_timers_never_fire() {
        let (inbox, rx) = Inbox::channel();
        let timers = Timers::new(inbox);
        timers.start(TimerId(7), 50);
        timers.start(TimerId(8), 60);
        timers.cancel(TimerId(7));
        assert_eq!(fired(&rx, Duration::from_secs(2)), Some(TimerId(8)));
        assert_eq!(fired(&rx, Duration::from_millis(150)), None);
        // Restarting replaces the deadline.
        timers.start(TimerId(9), 10_000);
        timers.start(TimerId(9), 10);
        assert_eq!(fired(&rx, Duration::from_secs(2)), Some(TimerId(9)));
    }

    #[test]
    fn dropping_stops_the_thread() {
        let (inbox, rx) = Inbox::channel();
        let timers = Timers::new(inbox);
        timers.start(TimerId(1), 50);
        drop(timers);
        assert_eq!(fired(&rx, Duration::from_millis(200)), None);
    }
}
