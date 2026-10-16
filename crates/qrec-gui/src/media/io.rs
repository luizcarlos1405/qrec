//! `qrec-gui-io` (G§3.12): one worker thread for the window's blocking jobs
//! (bin sizes, silence keys, wl-copy, source discovery). GTK-free, so it is
//! tested without a display; [`super::MediaHub`] holds the window's.
//!
//! - **Panics are contained.** Every job runs under `catch_unwind`: a panic
//!   ends that job only (the worker goes on with the next), and is told on
//!   the main thread like any caught panic (`guard`, G§3.12 R10). Nothing
//!   of the main thread ever reaches the worker: a [`IoWorker::run_then`]
//!   continuation stays in a main-thread table, and only the work's result
//!   (`Send`) travels, through the window's one ordered queue (R3) as
//!   [`Inbound::Io`]. (A `ThreadGuard`ed continuation dropped on the worker
//!   by a panic's unwinding aborted the process.)
//! - **Accounted for `waitIdle`** (G§3.18): [`IoWorker::idle`] is true once
//!   every job sent finished, and a job's result (or panic) is in the queue
//!   before the job stops counting, so a check of it before the queue's
//!   misses nothing. [`IoWorker::hold_busy`] counts too, for work that waits
//!   to be sent (a debounce timer).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crossbeam_channel::Sender;

use crate::guard;
use crate::host::queue::{Inbound, Queue};

/// A job for `qrec-gui-io`.
pub type IoJob = Box<dyn FnOnce() + Send>;

/// A finished job, for the main thread ([`Inbound::Io`]): a `run_then`
/// result for its continuation, or a panic to tell.
pub struct IoDone {
    /// The continuation waiting for it (`run_then`); `None`: a plain job's
    /// panic.
    id: Option<u64>,
    what: &'static str,
    result: Result<Box<dyn Any + Send>, String>,
}

impl std::fmt::Debug for IoDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoDone")
            .field("id", &self.id)
            .field("what", &self.what)
            .field("ok", &self.result.is_ok())
            .finish()
    }
}

type Then = Box<dyn FnOnce(Box<dyn Any + Send>)>;

/// The window's `qrec-gui-io`. Cheap to clone; main thread only.
#[derive(Clone)]
pub struct IoWorker {
    inner: Rc<Inner>,
}

struct Inner {
    queue: Queue,
    tx: RefCell<Option<Sender<IoJob>>>,
    /// Jobs sent and not finished, plus [`BusyToken`]s held.
    pending: Arc<AtomicUsize>,
    /// The continuations of `run_then` jobs, by id (main thread).
    waiting: RefCell<BTreeMap<u64, (&'static str, Then)>>,
    next: Cell<u64>,
}

impl std::fmt::Debug for IoWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoWorker")
            .field("pending", &self.inner.pending.load(Ordering::SeqCst))
            .field("waiting", &self.inner.waiting.borrow().len())
            .finish()
    }
}

/// Counts as io work until dropped ([`IoWorker::hold_busy`]).
#[derive(Debug)]
pub struct BusyToken {
    pending: Arc<AtomicUsize>,
}

impl Drop for BusyToken {
    fn drop(&mut self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

impl IoWorker {
    /// The worker starts with the first job; results come through `queue`.
    pub fn new(queue: Queue) -> Self {
        Self {
            inner: Rc::new(Inner {
                queue,
                tx: RefCell::new(None),
                pending: Arc::new(AtomicUsize::new(0)),
                waiting: RefCell::new(BTreeMap::new()),
                next: Cell::new(0),
            }),
        }
    }

    /// Runs `job` on `qrec-gui-io`, after the jobs before it. A panic in it
    /// is told on the main thread (as `what`).
    pub fn run(&self, what: &'static str, job: impl FnOnce() + Send + 'static) {
        let queue = self.inner.queue.clone();
        self.send(Box::new(move || {
            if let Err(panic) = catch_unwind(AssertUnwindSafe(job)) {
                queue.push(Inbound::Io(IoDone {
                    id: None,
                    what,
                    result: Err(guard::message(panic.as_ref())),
                }));
            }
        }));
    }

    /// Runs `work` on `qrec-gui-io`, then `then` with its result on the main
    /// thread, in the queue's order. A panic in `work` drops `then` (on the
    /// main thread) and is told.
    pub fn run_then<T: Send + 'static>(
        &self,
        what: &'static str,
        work: impl FnOnce() -> T + Send + 'static,
        then: impl FnOnce(T) + 'static,
    ) {
        let id = self.inner.next.get();
        self.inner.next.set(id + 1);
        let then: Then = Box::new(
            move |value: Box<dyn Any + Send>| match value.downcast::<T>() {
                Ok(value) => then(*value),
                Err(_) => tracing::error!("{what}: an io result of the wrong type"),
            },
        );
        self.inner.waiting.borrow_mut().insert(id, (what, then));
        let queue = self.inner.queue.clone();
        let sent = self.send(Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(work))
                .map(|value| Box::new(value) as Box<dyn Any + Send>)
                .map_err(|panic| guard::message(panic.as_ref()));
            queue.push(Inbound::Io(IoDone {
                id: Some(id),
                what,
                result,
            }));
        }));
        if !sent {
            self.inner.waiting.borrow_mut().remove(&id);
        }
    }

    /// Nothing sent is unfinished and no [`BusyToken`] is held (the test
    /// channel's `waitIdle` checks it before the queue: a job's result is
    /// in the queue before it stops counting).
    pub fn idle(&self) -> bool {
        self.inner.pending.load(Ordering::SeqCst) == 0
    }

    /// Counts as pending io work until dropped: a debounce that will send a
    /// job (WP7's bin sizes) keeps `waitIdle` waiting meanwhile.
    pub fn hold_busy(&self) -> BusyToken {
        self.inner.pending.fetch_add(1, Ordering::SeqCst);
        BusyToken {
            pending: Arc::clone(&self.inner.pending),
        }
    }

    /// A finished job from the queue (main thread): its continuation runs,
    /// or its panic is told.
    pub fn on_done(&self, done: IoDone) {
        let IoDone { id, what, result } = done;
        let then = id.and_then(|id| self.inner.waiting.borrow_mut().remove(&id));
        match (result, then) {
            (Ok(value), Some((what, then))) => guard::run(what, || then(value)),
            (Ok(_), None) => tracing::warn!("{what}: an io result nobody waits for"),
            (Err(message), then) => {
                // Dropped here, on the thread that made it.
                drop(then);
                guard::report_message(what, &message);
            }
        }
    }

    /// Sends `job` (counted until it finished); false when the worker can't
    /// take it.
    fn send(&self, job: IoJob) -> bool {
        let Some(tx) = self.sender() else {
            return false;
        };
        self.inner.pending.fetch_add(1, Ordering::SeqCst);
        if let Err(e) = tx.send(job) {
            self.inner.pending.fetch_sub(1, Ordering::SeqCst);
            tracing::error!("the io worker is gone: {e}");
            // The next job starts a new one.
            self.inner.tx.borrow_mut().take();
            return false;
        }
        true
    }

    fn sender(&self) -> Option<Sender<IoJob>> {
        if let Some(tx) = &*self.inner.tx.borrow() {
            return Some(tx.clone());
        }
        let (tx, rx) = crossbeam_channel::unbounded::<IoJob>();
        let pending = Arc::clone(&self.inner.pending);
        let spawned = thread::Builder::new()
            .name("qrec-gui-io".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    // A job contains its own panics (`run`, `run_then`); this
                    // keeps the worker (and the queued jobs) alive whatever
                    // a job does.
                    if catch_unwind(AssertUnwindSafe(job)).is_err() {
                        tracing::error!("an io job panicked (caught; the worker goes on)");
                    }
                    pending.fetch_sub(1, Ordering::SeqCst);
                }
            });
        if let Err(e) = spawned {
            tracing::error!("can't start the io worker: {e}");
            return None;
        }
        *self.inner.tx.borrow_mut() = Some(tx.clone());
        Some(tx)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn worker() -> (IoWorker, Queue) {
        let queue = Queue::new(|| {});
        (IoWorker::new(queue.clone()), queue)
    }

    /// Hands what the queue holds to the worker until `done`, like the
    /// window's drain.
    fn drain_until(worker: &IoWorker, queue: &Queue, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "not within 10 s");
            for item in queue.take_all() {
                match item {
                    Inbound::Io(io) => worker.on_done(io),
                    other => panic!("{other:?}"),
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// A panic in a job (plain or `run_then`'s work) ends that job only:
    /// the worker runs the jobs after it, the continuations of the others
    /// run on this thread, and each panic is counted like the window's
    /// caught panics. (A panic killed the worker for good, or aborted the
    /// process when it unwound through a `run_then` continuation.)
    #[test]
    fn a_panicking_job_never_takes_the_worker_or_the_process() {
        let (worker, queue) = worker();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let before = guard::panics();
        worker.run("a plain job", || panic!("plain"));
        let got = Rc::clone(&seen);
        worker.run_then(
            "a failing job",
            || -> u32 { panic!("in the work") },
            move |v| got.borrow_mut().push(v),
        );
        let got = Rc::clone(&seen);
        worker.run_then("a good job", || 7u32, move |v| got.borrow_mut().push(v));
        drain_until(&worker, &queue, || {
            seen.borrow().len() == 1 && worker.idle() && queue.is_empty()
        });
        assert_eq!(*seen.borrow(), [7]);
        assert_eq!(guard::panics(), before + 2);
        assert!(worker.inner.waiting.borrow().is_empty());
    }

    /// `idle` is false while a job runs and while a token is held, and a
    /// job's result is in the queue by the time it is true: `waitIdle`
    /// checks the worker before the queue.
    #[test]
    fn idle_comes_after_the_result_is_queued() {
        let (worker, queue) = worker();
        let (go, wait) = crossbeam_channel::bounded::<()>(1);
        worker.run_then(
            "a slow job",
            move || wait.recv_timeout(Duration::from_secs(10)).is_ok(),
            |_| {},
        );
        assert!(!worker.idle(), "running");
        go.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !worker.idle() {
            assert!(Instant::now() < deadline, "never idle");
            thread::yield_now();
        }
        assert_eq!(queue.len(), 1, "the result is queued first");
        drain_until(&worker, &queue, || queue.is_empty());
        let token = worker.hold_busy();
        assert!(!worker.idle(), "a debounce holds it");
        drop(token);
        assert!(worker.idle());
    }
}
