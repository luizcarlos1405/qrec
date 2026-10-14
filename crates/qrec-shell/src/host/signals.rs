//! Signals (A§8.6). Owner: ipc.
//!
//! Handlers only write the signal number to a non-blocking self-pipe (the
//! one async-signal-safe thing worth doing); a pump thread turns each byte
//! into [`Interrupts::raise`]. Everything else happens on ordinary threads:
//!
//! - **Hosts** (headless, in-process CLI): the first SIGTERM/SIGINT/SIGHUP
//!   dispatches `RecordStop` (finalize, not cancel) and cancels jobs; the
//!   host exits once idle or after 15 s. A second signal SIGKILLs the child
//!   process groups and `_exit(130)`s; startup recovery handles the take
//!   ([`watch`], [`hard_exit`]).
//! - **IPC clients**: the routing and streaming loops poll
//!   [`Interrupts::count`]; Ctrl+C closes the connection (exit 130), and for
//!   `record … --wait` the first sends `RecordStop` and the second detaches.
//!
//! Handlers reset to the default on `exec`, so child processes never
//! inherit them.

use std::fs;
use std::io;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Signals an IPC client or in-process command reacts to.
pub const CLIENT_SIGNALS: [libc::c_int; 2] = [libc::SIGINT, libc::SIGTERM];
/// Signals a host reacts to.
pub const HOST_SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// A counter of interrupts that threads can wait on. Real signals raise it
/// once [`install`]ed; code (and tests) can [`raise`](Self::raise) it too.
#[derive(Clone, Default)]
pub struct Interrupts {
    inner: Arc<Counter>,
}

#[derive(Default)]
struct Counter {
    count: Mutex<usize>,
    cv: Condvar,
}

impl std::fmt::Debug for Interrupts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interrupts")
            .field("count", &self.count())
            .finish()
    }
}

impl Interrupts {
    /// A counter no signal raises (until [`install`] hands out the real one).
    pub fn new() -> Self {
        Self::default()
    }

    /// Counts one interrupt and wakes every waiter.
    pub fn raise(&self) {
        *self
            .inner
            .count
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += 1;
        self.inner.cv.notify_all();
    }

    /// Interrupts so far.
    pub fn count(&self) -> usize {
        *self
            .inner
            .count
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until the count exceeds `seen` or `timeout` passes; returns the count.
    pub fn wait_past(&self, seen: usize, timeout: Duration) -> usize {
        let deadline = Instant::now() + timeout;
        let mut count = self
            .inner
            .count
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while *count <= seen {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            count = self
                .inner
                .cv
                .wait_timeout(count, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *count
    }
}

/// The write end of the self-pipe, for the handler. -1 until installed.
static WAKE_WR: AtomicI32 = AtomicI32::new(-1);
/// The process-wide counter the pump thread raises.
static INSTALLED: Mutex<Option<Interrupts>> = Mutex::new(None);

extern "C" fn on_signal(sig: libc::c_int) {
    // SAFETY: only async-signal-safe calls: reading/writing errno and write(2).
    unsafe {
        let errno = libc::__errno_location();
        let saved = *errno;
        let fd = WAKE_WR.load(Ordering::SeqCst);
        if fd >= 0 {
            let byte = sig as u8;
            libc::write(fd, (&byte as *const u8).cast(), 1);
        }
        *errno = saved;
    }
}

/// Installs handlers for `signals` (e.g. [`HOST_SIGNALS`]) and returns the
/// process-wide [`Interrupts`] they raise. Later calls add signals and
/// return the same counter.
pub fn install(signals: &[libc::c_int]) -> io::Result<Interrupts> {
    let mut installed = INSTALLED.lock().unwrap_or_else(PoisonError::into_inner);
    let interrupts = match &*installed {
        Some(interrupts) => interrupts.clone(),
        None => {
            let (read, write) = crate::ipc::server::pipe_cloexec()?;
            // The handler must never block on a full pipe.
            // SAFETY: fcntl on our own fd.
            if unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let interrupts = Interrupts::new();
            let pump = interrupts.clone();
            thread::Builder::new()
                .name("qrec-signals".into())
                .spawn(move || pump_signals(&read, &pump))?;
            // The write end lives as long as the process.
            WAKE_WR.store(write.into_raw_fd(), Ordering::SeqCst);
            *installed = Some(interrupts.clone());
            interrupts
        }
    };
    for &sig in signals {
        // SAFETY: a zeroed sigaction is valid; we fill the handler, an empty
        // mask and SA_RESTART, then install it.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as *const () as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(sig, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(interrupts)
}

fn pump_signals(read: &OwnedFd, interrupts: &Interrupts) {
    let mut buf = [0u8; 64];
    loop {
        // SAFETY: reading into a valid local buffer from our pipe.
        let n = unsafe { libc::read(read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            for _ in 0..n {
                interrupts.raise();
            }
        } else if n == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}

/// Runs `on_first` on the first interrupt after `baseline` and `on_second`
/// on the second, from its own thread, until the returned guard is dropped.
pub fn watch(
    interrupts: &Interrupts,
    baseline: usize,
    on_first: impl FnOnce() + Send + 'static,
    on_second: impl FnOnce() + Send + 'static,
) -> io::Result<SignalWatch> {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let interrupts = interrupts.clone();
    let handle = thread::Builder::new()
        .name("qrec-host-signals".into())
        .spawn(move || {
            let mut on_first = Some(on_first);
            let mut on_second = Some(on_second);
            let mut seen = baseline;
            while !stopped.load(Ordering::SeqCst) {
                let count = interrupts.wait_past(seen, Duration::from_millis(200));
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                if count <= seen {
                    continue;
                }
                seen = count;
                if let Some(first) = on_first.take() {
                    first();
                }
                if count >= baseline + 2 {
                    if let Some(second) = on_second.take() {
                        second();
                    }
                    return;
                }
            }
        })?;
    Ok(SignalWatch {
        stop,
        handle: Some(handle),
    })
}

/// Stops a [`watch`] thread when dropped.
pub struct SignalWatch {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for SignalWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            // The thread wakes within 200 ms; a callback running right now
            // finishes first.
            let _ = handle.join();
        }
    }
}

/// The direct children of this process: `(pid, process group)`.
pub fn child_processes() -> Vec<(i32, i32)> {
    let me = std::process::id() as i32;
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // "pid (comm) state ppid pgrp …"; comm may contain spaces and parens.
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let mut fields = stat[close + 1..].split_whitespace().skip(1);
        let ppid = fields.next().and_then(|f| f.parse::<i32>().ok());
        let pgrp = fields.next().and_then(|f| f.parse::<i32>().ok());
        if let (Some(ppid), Some(pgrp)) = (ppid, pgrp) {
            if ppid == me {
                children.push((pid, pgrp));
            }
        }
    }
    children
}

/// SIGKILLs every child's process group (children run in their own group,
/// A§8.7), or the child alone when it shares ours. Returns how many it hit.
pub fn kill_child_groups() -> usize {
    // SAFETY: getpgrp has no failure mode.
    let ours = unsafe { libc::getpgrp() };
    let mut killed = 0;
    for (pid, pgrp) in child_processes() {
        let target = if pgrp > 1 && pgrp != ours { -pgrp } else { pid };
        // SAFETY: kill(2) with a pid or a negated process group.
        if unsafe { libc::kill(target, libc::SIGKILL) } == 0 {
            killed += 1;
        }
    }
    killed
}

/// The second signal in a host (A§8.6): SIGKILL the child process groups
/// and `_exit(130)` without running destructors.
pub fn hard_exit() -> ! {
    kill_child_groups();
    // SAFETY: _exit never returns and is always safe to call.
    unsafe { libc::_exit(130) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn waiters_see_raised_interrupts() {
        let interrupts = Interrupts::new();
        assert_eq!(interrupts.wait_past(0, Duration::from_millis(20)), 0);
        let raiser = interrupts.clone();
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            raiser.raise();
        });
        assert_eq!(interrupts.wait_past(0, Duration::from_secs(5)), 1);
        t.join().unwrap();
        assert_eq!(interrupts.count(), 1);
    }

    #[test]
    fn watch_runs_first_then_second() {
        let interrupts = Interrupts::new();
        interrupts.raise(); // before the baseline: ignored
        let firsts = Arc::new(AtomicUsize::new(0));
        let seconds = Arc::new(AtomicUsize::new(0));
        let (f, s) = (Arc::clone(&firsts), Arc::clone(&seconds));
        let guard = watch(
            &interrupts,
            1,
            move || {
                f.fetch_add(1, Ordering::SeqCst);
            },
            move || {
                s.fetch_add(1, Ordering::SeqCst);
            },
        )
        .unwrap();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(firsts.load(Ordering::SeqCst), 0);
        interrupts.raise();
        assert!(crate::ipc::testing::wait_for(
            Duration::from_secs(2),
            || firsts.load(Ordering::SeqCst) == 1
        ));
        assert_eq!(seconds.load(Ordering::SeqCst), 0);
        interrupts.raise();
        assert!(crate::ipc::testing::wait_for(
            Duration::from_secs(2),
            || seconds.load(Ordering::SeqCst) == 1
        ));
        drop(guard);
        assert_eq!(firsts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn two_signals_at_once_run_both_callbacks() {
        let interrupts = Interrupts::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (a, b) = (Arc::clone(&calls), Arc::clone(&calls));
        let _guard = watch(
            &interrupts,
            0,
            move || a.lock().unwrap().push("first"),
            move || b.lock().unwrap().push("second"),
        )
        .unwrap();
        interrupts.raise();
        interrupts.raise();
        assert!(crate::ipc::testing::wait_for(
            Duration::from_secs(2),
            || calls.lock().unwrap().len() == 2
        ));
        assert_eq!(*calls.lock().unwrap(), ["first", "second"]);
    }

    #[test]
    fn finds_its_own_children() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let found = child_processes();
        assert!(
            found.iter().any(|&(pid, _)| pid == child.id() as i32),
            "{found:?}"
        );
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
