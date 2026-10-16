//! Children of a test that live in process groups of their own (the GUI
//! harness's sway, session bus and window, docs/GUI.md G§8.4): a terminal's
//! Ctrl-C or a runner's timeout only reaches the test's own group, and a
//! killed test binary runs no `Drop`, so they would run on for good.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// A signal for [`signal`] and [`signal_group`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Int,
    Term,
    Kill,
}

impl Signal {
    fn raw(self) -> libc::c_int {
        match self {
            Signal::Int => libc::SIGINT,
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        }
    }

    /// `TERM`, `INT`, `KILL` (as `kill -<name>` takes them).
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim_start_matches("SIG") {
            "INT" => Some(Signal::Int),
            "TERM" => Some(Signal::Term),
            "KILL" => Some(Signal::Kill),
            _ => None,
        }
    }
}

/// Makes `command`'s child get SIGTERM when the thread that spawns it ends
/// (`PR_SET_PDEATHSIG`): the test's thread once its `Drop`s ran, or every
/// thread at once when the test binary is killed. SIGTERM, not SIGKILL, so
/// a window still finalizes a take and stops its own children. A child
/// whose parent is gone before it got there exits at once.
pub fn die_with_parent(command: &mut Command) {
    // SAFETY: getpid(2) only reads.
    let parent = unsafe { libc::getpid() };
    // SAFETY: the closure runs in the forked child before exec and only
    // makes async-signal-safe calls (prctl, getppid, _exit).
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(io::Error::last_os_error());
            }
            // The parent died between the fork and the prctl.
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
}

/// Sends `signal` to the process `pid`; false when it is gone.
pub fn signal(pid: u32, signal: Signal) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: kill(2) only sends a signal.
    unsafe { libc::kill(pid, signal.raw()) == 0 }
}

/// Sends `signal` to every process of the group `pgid` (a child started
/// with `process_group(0)`: its pid); false when none is left.
pub fn signal_group(pgid: u32, signal: Signal) -> bool {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return false;
    };
    if pgid <= 1 {
        return false;
    }
    // SAFETY: kill(2) with a negative pid signals that process group.
    unsafe { libc::kill(-pgid, signal.raw()) == 0 }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use super::*;

    /// A grouped child dies with the thread that started it, and the group
    /// signal reaches it.
    #[test]
    fn a_child_dies_with_its_thread_and_its_group() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut command = Command::new("sleep");
            command.arg("30").stdin(Stdio::null()).process_group(0);
            die_with_parent(&mut command);
            let child = command.spawn().expect("sleep");
            tx.send(child).unwrap();
            // The thread ends here: the child gets SIGTERM.
        });
        let mut child = rx.recv().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
                break;
            }
            assert!(Instant::now() < deadline, "the child outlived its thread");
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut command = Command::new("sleep");
        command.arg("30").stdin(Stdio::null()).process_group(0);
        let mut child = command.spawn().expect("sleep");
        assert!(signal_group(child.id(), Signal::Kill));
        let status = child.wait().unwrap();
        assert!(!status.success());
        assert!(!signal_group(child.id(), Signal::Term), "gone");
        assert_eq!(Signal::from_name("TERM"), Some(Signal::Term));
        assert_eq!(Signal::from_name("SIGINT"), Some(Signal::Int));
        assert_eq!(Signal::from_name("HUP"), None);
    }
}
