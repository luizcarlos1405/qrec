//! `gst::init()` on a helper thread with a deadline (A§8.10).
//!
//! The first `gst::init()` of a machine (or after a GStreamer update) scans
//! every plugin, and some plugins open GPU devices while being scanned: a
//! hung driver (NVIDIA's `renderD128` on the dev machine) then blocks the
//! scan forever. So GStreamer is initialized on a helper thread and callers
//! wait at most [`INIT_TIMEOUT`]; a timeout leaves the helper blocked (it
//! can't be cancelled) and is reported as [`InitError::Stuck`].
//!
//! Rules: commands that only need ffmpeg never call this; it never runs on
//! the GTK main thread; concurrent callers share one helper thread and one
//! result. The capture engine calls [`init`] lazily on its own thread and
//! turns an error into `CaptureFailed`; commands that need GStreamer up
//! front call [`init_or_exit`] (`process::exit(5)`, A§4.8
//! `missing_dependency`).

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// How long callers wait for `gst::init()`.
pub const INIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Printed when `gst::init()` doesn't return in time.
pub const STUCK_MESSAGE: &str = "GStreamer is stuck scanning plugins (a GPU driver is probably hung); see `qrec doctor`";

/// The exit code of [`init_or_exit`] (`missing_dependency`).
pub const EXIT_CODE: i32 = 5;

/// Why GStreamer isn't usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InitError {
    /// `gst::init()` didn't return within the deadline.
    #[error("{STUCK_MESSAGE}")]
    Stuck,
    /// `gst::init()` returned an error (or its thread couldn't start).
    #[error("GStreamer failed to initialize: {0}")]
    Failed(String),
}

enum InitState {
    NotStarted,
    Running,
    Done(Result<(), String>),
}

static STATE: Mutex<InitState> = Mutex::new(InitState::NotStarted);
static DONE: Condvar = Condvar::new();

fn lock() -> MutexGuard<'static, InitState> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Initializes GStreamer, waiting at most [`INIT_TIMEOUT`].
pub fn init() -> Result<(), InitError> {
    init_with_timeout(INIT_TIMEOUT)
}

/// Initializes GStreamer on the shared helper thread, waiting at most
/// `timeout` for it. Later calls return the same result; a call after a
/// timeout waits again (the scan may still finish).
pub fn init_with_timeout(timeout: Duration) -> Result<(), InitError> {
    let deadline = Instant::now() + timeout;
    let mut state = lock();
    loop {
        match &*state {
            InitState::Done(Ok(())) => return Ok(()),
            InitState::Done(Err(message)) => return Err(InitError::Failed(message.clone())),
            InitState::NotStarted => {
                *state = InitState::Running;
                let spawned = thread::Builder::new()
                    .name("qrec-gst-init".into())
                    .spawn(|| {
                        let result = gst::init().map_err(|e| e.to_string());
                        *lock() = InitState::Done(result);
                        DONE.notify_all();
                    });
                if let Err(e) = spawned {
                    let message = format!("can't start the GStreamer init thread: {e}");
                    *state = InitState::Done(Err(message.clone()));
                    return Err(InitError::Failed(message));
                }
            }
            InitState::Running => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(InitError::Stuck);
                }
                state = DONE
                    .wait_timeout(state, deadline - now)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
    }
}

/// Whether GStreamer finished initializing successfully.
pub fn is_initialized() -> bool {
    matches!(&*lock(), InitState::Done(Ok(())))
}

/// [`init`], or print the reason to stderr and exit with code 5.
pub fn init_or_exit() {
    if let Err(e) = init() {
        eprintln!("qrec: {e}");
        std::process::exit(EXIT_CODE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_once_and_shares_the_result() {
        let threads: Vec<_> = (0..4).map(|_| thread::spawn(init)).collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), Ok(()));
        }
        assert!(is_initialized());
        // Already done: returns at once even with a zero deadline.
        assert_eq!(init_with_timeout(Duration::ZERO), Ok(()));
        assert!(gst::ElementFactory::find("fakesink").is_some());
    }

    #[test]
    fn errors_read_like_the_spec() {
        assert_eq!(
            InitError::Stuck.to_string(),
            "GStreamer is stuck scanning plugins (a GPU driver is probably hung); see `qrec doctor`"
        );
        assert!(
            InitError::Failed("no registry".into())
                .to_string()
                .ends_with("no registry")
        );
    }
}
