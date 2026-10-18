//! Who runs beside whom (G§8.4's parallel runs: `--test-threads` above 1
//! in one suite, or every suite at once under `cargo nextest`).
//!
//! - [`Slot`]: every case holds one from before its compositor starts
//!   until its env is gone. Most cases hold it shared and run side by side;
//!   a timing-sensitive case holds it exclusively
//!   ([`Options::exclusive`](super::Options::exclusive)) and runs alone: it
//!   waits for the running cases to end, and no case starts until it has.
//!   Two `flock(2)` locks on files in Cargo's tmp dir of the target
//!   (`CARGO_TARGET_TMPDIR`), so the threads of a suite and the suites of a
//!   run agree (each case opens the files itself: a `flock` belongs to an
//!   open file, not a process): `gui-nested.lock`, held shared or exclusive
//!   for the whole case, and `gui-nested.gate`, which a case that runs alone
//!   holds while it waits for the running ones. A shared case passes the
//!   gate only while no exclusive case waits, so an exclusive one never
//!   waits for the end of the run. A lock goes with its process: a killed
//!   run leaves none held (and a fork in another thread holds one a moment
//!   longer, until its child execs: every take polls).
//! - [`Claim`]: a case's shots and logs (`<shots>/<case>-…`) are its own
//!   while it runs; a second case of the same area and name running at the
//!   same time fails at its start instead of overwriting them (a probe
//!   would read the other's screenshot).
//!
//! For the runners: each slot given back adds a line to `gui-slots.log`
//! there (the case, its sharing, how long it waited and ran), and the
//! cases that ran alone are listed in `gui-exclusive/` (one empty file,
//! `<suite>--<test>`, made by an exclusive slot and removed by a shared
//! one), so `scripts/check-quick` and `check-full` run them after the
//! others instead of draining the parallel run for each. The locks decide
//! who runs beside whom; the list only orders the run.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant};

/// How long a case waits for its slot: for the exclusive cases before it,
/// or, exclusive, for the cases running when it came.
pub const SLOT_WAIT: Duration = Duration::from_secs(30 * 60);
const POLL: Duration = Duration::from_millis(100);
const LOCK: &str = "gui-nested.lock";
const GATE: &str = "gui-nested.gate";

/// The folder of the run's lock files: Cargo's tmp dir of the target.
pub fn slot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
}

/// How a case shares the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sharing {
    /// Beside the other shared cases.
    Shared,
    /// Alone.
    Exclusive,
}

/// A case's hold on the machine, released when the last [`Slot`] of its
/// thread goes (a case running two compositors at once holds one).
pub struct Slot {
    _held: Arc<Held>,
}

struct Held {
    sharing: Sharing,
    /// Closed (so unlocked) when the last slot goes.
    _lock: File,
    /// `gui-slots.log`'s line: who held it, how long it waited and ran.
    log: PathBuf,
    who: String,
    waited: Duration,
    since: Instant,
}

impl Drop for Held {
    fn drop(&mut self) {
        let line = format!(
            "{} {:?} waited {:.1} s ran {:.1} s\n",
            self.who,
            self.sharing,
            self.waited.as_secs_f64(),
            self.since.elapsed().as_secs_f64()
        );
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .and_then(|mut log| log.write_all(line.as_bytes()));
    }
}

thread_local! {
    /// The slot this thread's case holds, if any.
    static HELD: RefCell<Weak<Held>> = const { RefCell::new(Weak::new()) };
}

impl Slot {
    /// The slot of case `case` on the lock files in `dir`, within `within`
    /// (else the case fails, naming the lock).
    pub fn take(dir: &Path, sharing: Sharing, case: &str, within: Duration) -> Slot {
        if let Some(held) = HELD.with(|h| h.borrow().upgrade()) {
            assert!(
                held.sharing == Sharing::Exclusive || sharing == Sharing::Shared,
                "{case}: a case can't run alone once it runs beside others: make its first \
                 compositor exclusive (Options::exclusive)"
            );
            return Slot { _held: held };
        }
        let started = Instant::now();
        let lock = lock(dir, sharing, case, within);
        // The test (libtest names its thread after it) in this suite.
        let test = thread::current().name().map(str::to_owned);
        if let Some(test) = &test {
            let listed = dir
                .join("gui-exclusive")
                .join(format!("{}--{test}", env!("CARGO_CRATE_NAME")));
            let _ = match sharing {
                Sharing::Exclusive => fs::create_dir_all(dir.join("gui-exclusive"))
                    .and_then(|()| fs::write(&listed, b"")),
                Sharing::Shared => fs::remove_file(&listed),
            };
        }
        let held = Arc::new(Held {
            sharing,
            _lock: lock,
            log: dir.join("gui-slots.log"),
            who: format!(
                "{}::{} ({case})",
                env!("CARGO_CRATE_NAME"),
                test.as_deref().unwrap_or("?")
            ),
            waited: started.elapsed(),
            since: Instant::now(),
        });
        HELD.with(|h| *h.borrow_mut() = Arc::downgrade(&held));
        Slot { _held: held }
    }
}

fn open(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Whether a `try_lock` got the lock (`false`: someone else holds it).
fn got(result: Result<(), TryLockError>, path: &Path) -> bool {
    match result {
        Ok(()) => true,
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Error(e)) => panic!("flock {}: {e}", path.display()),
    }
}

/// `dir`'s lock, held as `sharing` says.
fn lock(dir: &Path, sharing: Sharing, case: &str, within: Duration) -> File {
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    let (lock_path, gate_path) = (dir.join(LOCK), dir.join(GATE));
    let (lock, gate) = (open(&lock_path), open(&gate_path));
    let deadline = Instant::now() + within;
    let wait = |what: &str, ready: &mut dyn FnMut() -> bool| {
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "{case} waited {within:?} for {what} ({}): is a case that holds it stuck?",
                lock_path.display()
            );
            thread::sleep(POLL);
        }
    };
    match sharing {
        Sharing::Exclusive => {
            // No case starts from now on, then the running ones end.
            wait("the gate", &mut || got(gate.try_lock(), &gate_path));
            wait("the running cases to end", &mut || {
                got(lock.try_lock(), &lock_path)
            });
            // The gate goes with its file: the next exclusive case may
            // queue behind this one.
        }
        Sharing::Shared => wait("the cases running alone", &mut || {
            if !got(gate.try_lock_shared(), &gate_path) {
                return false;
            }
            let held = got(lock.try_lock_shared(), &lock_path);
            let _ = gate.unlock();
            held
        }),
    }
    lock
}

/// A running case's claim on its shots and logs.
pub struct Claim {
    _file: File,
}

impl Claim {
    /// The claim of case `case` writing into `shots`, its claim file in
    /// `dir`: a panic if another running case has it.
    pub fn take(dir: &Path, shots: &Path, case: &str) -> Claim {
        Self::take_within(dir, shots, case, Duration::from_secs(2))
    }

    fn take_within(dir: &Path, shots: &Path, case: &str, within: Duration) -> Claim {
        let dir = dir.join("gui-claims");
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        // One file per shots folder and case: `wp4--S02`, `scale-2-wp4--S02`.
        let parts: Vec<String> = shots
            .components()
            .filter_map(|c| match c {
                Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        let from = parts
            .iter()
            .rposition(|p| p == "gui-shots")
            .map_or(0, |i| i + 1);
        let path = dir.join(format!("{}--{case}.lock", parts[from..].join("-")));
        let file = open(&path);
        // A case that just ended may hold it a moment longer: a fork in
        // another thread of this process (a tool's start) keeps its file
        // open until the child execs.
        let deadline = Instant::now() + within;
        while !got(file.try_lock(), &path) {
            assert!(
                Instant::now() < deadline,
                "another running case writes {case}'s shots and logs in {}: give one of them \
                 another name",
                shots.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
        Claim { _file: file }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const SHORT: Duration = Duration::from_secs(10);

    /// Shared cases run side by side; an exclusive one waits for them, and
    /// a case that comes meanwhile waits for it (the gate), so it doesn't
    /// wait for the end of the run.
    #[test]
    fn exclusive_cases_run_alone() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let dir = dir.path().to_path_buf();
        let a = Slot::take(&dir, Sharing::Shared, "a", SHORT);
        let b = thread::spawn({
            let dir = dir.clone();
            move || Slot::take(&dir, Sharing::Shared, "b", SHORT)
        })
        .join()
        .expect("b beside a");
        let (tx, rx) = mpsc::channel();
        let alone = thread::spawn({
            let dir = dir.clone();
            move || {
                let slot = Slot::take(&dir, Sharing::Exclusive, "alone", SHORT);
                tx.send(Instant::now()).expect("send");
                thread::sleep(Duration::from_millis(150));
                drop(slot);
            }
        });
        thread::sleep(Duration::from_millis(150));
        assert!(rx.try_recv().is_err(), "it ran beside a and b");
        // A case that comes now waits for the exclusive one.
        let (late_tx, late_rx) = mpsc::channel();
        let late = thread::spawn({
            let dir = dir.clone();
            move || {
                let _slot = Slot::take(&dir, Sharing::Shared, "late", SHORT);
                late_tx.send(Instant::now()).expect("send");
            }
        });
        thread::sleep(Duration::from_millis(150));
        assert!(late_rx.try_recv().is_err(), "it passed the gate");
        drop(a);
        drop(b);
        let ran = rx.recv_timeout(SHORT).expect("it ran once they ended");
        let late_ran = late_rx.recv_timeout(SHORT).expect("it ran after");
        assert!(
            late_ran >= ran + Duration::from_millis(100),
            "it ran beside the exclusive one"
        );
        alone.join().expect("alone");
        late.join().expect("late");
    }

    /// A thread's second compositor shares its case's slot; a case that
    /// holds a shared slot can't turn exclusive (it would wait for itself).
    #[test]
    fn a_case_holds_one_slot() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let first = Slot::take(dir.path(), Sharing::Exclusive, "x", SHORT);
        let second = Slot::take(dir.path(), Sharing::Shared, "x", SHORT);
        drop(first);
        let other = {
            let dir = dir.path().to_path_buf();
            thread::spawn(move || {
                Slot::take(&dir, Sharing::Shared, "y", Duration::from_millis(150))
            })
        };
        assert!(other.join().is_err(), "the second still holds it alone");
        drop(second);
        let shared = Slot::take(dir.path(), Sharing::Shared, "z", SHORT);
        let caught = std::panic::catch_unwind(|| {
            Slot::take(dir.path(), Sharing::Exclusive, "z", SHORT);
        });
        assert!(caught.is_err(), "shared, then exclusive");
        drop(shared);
    }

    /// A case that ran alone is listed for the runners, and taken off the
    /// list once it runs beside others; each slot given back is logged.
    #[test]
    fn the_cases_that_ran_alone_are_listed() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let me = thread::current()
            .name()
            .expect("libtest names the test's thread")
            .to_owned();
        let listed = dir
            .path()
            .join("gui-exclusive")
            .join(format!("{}--{me}", env!("CARGO_CRATE_NAME")));
        drop(Slot::take(dir.path(), Sharing::Exclusive, "x", SHORT));
        assert!(listed.is_file(), "{} listed", listed.display());
        drop(Slot::take(dir.path(), Sharing::Shared, "x", SHORT));
        assert!(!listed.exists(), "{} still listed", listed.display());
        let log = fs::read_to_string(dir.path().join("gui-slots.log")).expect("the log");
        assert_eq!(log.lines().count(), 2, "{log}");
        assert!(
            log.lines()
                .all(|l| l.contains(&format!("::{me} (x)")) && l.contains(" waited ")),
            "{log}"
        );
    }

    /// Two running cases can't write the same shots and logs; one after
    /// the other can.
    #[test]
    fn a_running_cases_shots_are_its_own() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let shots = Path::new("/r/tmp/gui-shots/scale/2/wp4");
        let first = Claim::take(dir.path(), shots, "S02");
        let other = Claim::take(dir.path(), Path::new("/r/tmp/gui-shots/wp4"), "S02");
        let caught = std::panic::catch_unwind(|| {
            Claim::take_within(dir.path(), shots, "S02", Duration::from_millis(100))
        });
        assert!(caught.is_err(), "the same case twice at once");
        drop(first);
        let _again = Claim::take(dir.path(), shots, "S02");
        assert!(
            dir.path()
                .join("gui-claims/scale-2-wp4--S02.lock")
                .is_file()
        );
        drop(other);
    }
}
