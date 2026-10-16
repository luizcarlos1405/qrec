//! The one ordered queue into the GTK main thread (G§3.5, R2, R3).
//!
//! Everything the window hears from other threads goes through it: the
//! Session's snapshots and toasts (the port), the replies to the window's
//! own requests, the host's life, the startup workers, the player, the
//! preview cache, source discovery and the test control channel.
//!
//! - [`Queue::push`] works from any thread and never waits for GTK: a
//!   short mutex, then at most one wake-up pending (an `AtomicBool`).
//! - A snapshot replaces a snapshot at the tail only (adjacent ones
//!   coalesce), so a reply never overtakes the state it describes and the
//!   window re-renders from the newest state without diffing.
//! - The wake-up schedules one drain on the main thread, which handles
//!   everything queued, in order ([`Queue::take_all`]).
//!
//! High-rate data (levels, preview frames, the player's position) never
//! enters the queue: it goes into latest-wins slots (`media`).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use qrec_core::protocol::{Progress, Toast};
use qrec_core::{CoreError, State};
use qrec_shell::host::HostEvent;
use qrec_shell::media::preview::PreviewData;
use qrec_shell::player::PlayerEvent;
use qrec_shell::sources::SourceInventory;
use serde_json::Value;

use crate::startup::StartupEvent;
use crate::testctl::TestRequest;

/// Which request of the window a reply answers ([`crate::store::Store`]
/// numbers them).
pub type Ticket = u64;

/// Something for the main thread, in arrival order.
pub enum Inbound {
    /// A state the Session holds now (`rev` grows by one per assignment).
    Snapshot(u64, Arc<State>),
    /// The window's request `Ticket` was queued by the Session.
    Accepted(Ticket),
    Progress(Ticket, Progress),
    /// The answer to request `ticket`; `late` for an event's reply
    /// (`Responder::reply_late`, H-7).
    Reply {
        ticket: Ticket,
        result: Result<Value, CoreError>,
        late: bool,
    },
    /// A core toast (`Effect::Notify`); `recovery` for the startup
    /// recovery's own news.
    Toast {
        toast: Toast,
        recovery: bool,
    },
    /// The GUI host's life.
    Host(HostEvent),
    /// A startup worker's result (G§3.3).
    Startup(StartupEvent),
    Player(PlayerEvent),
    Preview(PreviewData),
    /// A source discovery finished (`SharedSources::refresh`).
    Sources(SourceInventory),
    /// A test control request (G§3.18, test mode only).
    Test(TestRequest),
}

impl std::fmt::Debug for Inbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Inbound::Snapshot(rev, _) => write!(f, "Snapshot({rev})"),
            Inbound::Accepted(t) => write!(f, "Accepted({t})"),
            Inbound::Progress(t, _) => write!(f, "Progress({t})"),
            Inbound::Reply {
                ticket,
                result,
                late,
            } => write!(
                f,
                "Reply({ticket}, {}, late {late})",
                if result.is_ok() { "ok" } else { "err" }
            ),
            Inbound::Toast { toast, recovery } => {
                write!(f, "Toast({:?}, recovery {recovery})", toast.title)
            }
            Inbound::Host(event) => write!(f, "Host({event:?})"),
            Inbound::Startup(event) => write!(f, "Startup({event:?})"),
            Inbound::Player(event) => write!(f, "Player({event:?})"),
            Inbound::Preview(_) => f.write_str("Preview"),
            Inbound::Sources(_) => f.write_str("Sources"),
            Inbound::Test(_) => f.write_str("Test"),
        }
    }
}

/// The queue. Cheap to clone; clones share it.
#[derive(Clone)]
pub struct Queue {
    inner: Arc<Inner>,
}

struct Inner {
    items: Mutex<VecDeque<Inbound>>,
    /// A drain is scheduled and hasn't started taking items yet.
    pending: AtomicBool,
    /// Schedules one drain on the main thread (never runs it in place).
    wake: Box<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for Queue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Queue")
            .field("len", &self.len())
            .field("pending", &self.inner.pending.load(Ordering::SeqCst))
            .finish()
    }
}

impl Queue {
    /// A queue whose pushes call `wake` when no drain is pending; `wake`
    /// must only schedule the drain (the window uses an idle source on the
    /// default main context), never run it.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Inner {
                items: Mutex::new(VecDeque::new()),
                pending: AtomicBool::new(false),
                wake: Box::new(wake),
            }),
        }
    }

    /// Queues `item` (from any thread). A snapshot right behind another
    /// snapshot replaces it.
    pub fn push(&self, item: Inbound) {
        {
            let mut items = self.lock();
            match (items.back_mut(), item) {
                (Some(last @ Inbound::Snapshot(..)), next @ Inbound::Snapshot(..)) => *last = next,
                (_, item) => items.push_back(item),
            }
        }
        if !self.inner.pending.swap(true, Ordering::SeqCst) {
            (self.inner.wake)();
        }
    }

    /// The drain: everything queued, oldest first. Clears the pending
    /// wake-up first, so a push while the items are handled schedules the
    /// next drain.
    pub fn take_all(&self) -> Vec<Inbound> {
        self.inner.pending.store(false, Ordering::SeqCst);
        self.lock().drain(..).collect()
    }

    /// The drain ran before anyone could handle the items (the app isn't
    /// up yet): keep them, and let the next push wake again.
    pub fn clear_pending(&self) {
        self.inner.pending.store(false, Ordering::SeqCst);
    }

    /// Schedules a drain now if items wait and none is scheduled (the app
    /// just came up).
    pub fn kick(&self) {
        if !self.is_empty() && !self.inner.pending.swap(true, Ordering::SeqCst) {
            (self.inner.wake)();
        }
    }

    /// How many items wait.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Nothing waits (the test channel's `waitIdle`).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Inbound>> {
        self.inner
            .items
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use qrec_core::testkit;

    use super::*;

    fn counting() -> (Queue, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let queue = Queue::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        (queue, wakes)
    }

    fn snapshot(rev: u64) -> Inbound {
        Inbound::Snapshot(rev, Arc::new(testkit::design_state()))
    }

    fn reply(ticket: Ticket) -> Inbound {
        Inbound::Reply {
            ticket,
            result: Ok(Value::Null),
            late: false,
        }
    }

    fn names(items: &[Inbound]) -> Vec<String> {
        items.iter().map(|i| format!("{i:?}")).collect()
    }

    #[test]
    fn adjacent_snapshots_coalesce_and_replies_keep_their_place() {
        let (queue, _) = counting();
        queue.push(snapshot(1));
        queue.push(snapshot(2));
        queue.push(reply(7));
        queue.push(snapshot(3));
        queue.push(snapshot(4));
        queue.push(Inbound::Accepted(8));
        queue.push(snapshot(5));
        assert_eq!(
            names(&queue.take_all()),
            [
                "Snapshot(2)",
                "Reply(7, ok, late false)",
                "Snapshot(4)",
                "Accepted(8)",
                "Snapshot(5)"
            ]
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn one_wake_up_per_drain() {
        let (queue, wakes) = counting();
        queue.push(snapshot(1));
        queue.push(reply(1));
        queue.push(reply(2));
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "one pending drain");
        assert_eq!(queue.take_all().len(), 3);
        queue.push(reply(3));
        assert_eq!(wakes.load(Ordering::SeqCst), 2, "the next push wakes again");
        // A push while the drain handles its items (after take_all) wakes.
        let taken = queue.take_all();
        queue.push(reply(4));
        assert_eq!(wakes.load(Ordering::SeqCst), 3);
        assert_eq!(names(&taken), ["Reply(3, ok, late false)"]);
    }

    #[test]
    fn items_wait_for_the_app() {
        let (queue, wakes) = counting();
        queue.push(reply(1));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        // The drain ran before the app was up: nothing is taken.
        queue.clear_pending();
        queue.push(reply(2));
        assert_eq!(wakes.load(Ordering::SeqCst), 2, "the next push wakes again");
        queue.clear_pending();
        queue.kick();
        assert_eq!(wakes.load(Ordering::SeqCst), 3, "the app kicks a drain");
        queue.kick();
        assert_eq!(wakes.load(Ordering::SeqCst), 3, "one pending at a time");
        assert_eq!(queue.take_all().len(), 2);
        queue.kick();
        assert_eq!(wakes.load(Ordering::SeqCst), 3, "nothing to drain");
    }

    #[test]
    fn pushes_from_other_threads_arrive_in_order_per_thread() {
        let (queue, _) = counting();
        let threads: Vec<_> = (0..4u64)
            .map(|t| {
                let queue = queue.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        queue.push(reply(t * 1000 + i));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let items = queue.take_all();
        assert_eq!(items.len(), 400);
        for t in 0..4u64 {
            let mine: Vec<Ticket> = items
                .iter()
                .filter_map(|i| match i {
                    Inbound::Reply { ticket, .. } if ticket / 1000 == t => Some(*ticket),
                    _ => None,
                })
                .collect();
            assert!(mine.windows(2).all(|w| w[0] < w[1]), "thread {t} in order");
        }
    }
}
