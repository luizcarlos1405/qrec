//! Running jobs and their cancellation (A§8.6, A§8.7): `cancel(job)` marks
//! the job and stops its children (the child runner sends SIGTERM to each
//! process group at once and SIGKILL after the grace period); the job thread
//! then deletes its temp outputs and reports how it ended. Every temp output
//! being written is also in a process-wide list, so a hard exit (a second
//! signal in a headless host, A§8.6), which no job thread outlives, deletes
//! them too ([`remove_temps_in_flight`]). An output's temp file is deleted
//! through the folder the job holds open (A§8.8), never by a path that may
//! lead elsewhere by then.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use qrec_core::JobId;

use super::child::StopFlag;
use super::place::OutDir;

/// A temp file a job writes, deleted if the job doesn't finish.
#[derive(Debug, Clone)]
pub(crate) enum Temp {
    /// A path in the project's own folders: a segment, the concat list.
    Path(PathBuf),
    /// A name in an output's open folder (A§8.8).
    In(OutDir, OsString),
}

impl PartialEq for Temp {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Temp::Path(a), Temp::Path(b)) => a == b,
            (Temp::In(dir_a, a), Temp::In(dir_b, b)) => dir_a.same(dir_b) && a == b,
            _ => false,
        }
    }
}

impl From<PathBuf> for Temp {
    fn from(path: PathBuf) -> Self {
        Temp::Path(path)
    }
}

impl From<&Path> for Temp {
    fn from(path: &Path) -> Self {
        Temp::Path(path.to_path_buf())
    }
}

impl From<&PathBuf> for Temp {
    fn from(path: &PathBuf) -> Self {
        Temp::Path(path.clone())
    }
}

impl Temp {
    fn remove(&self) -> io::Result<()> {
        match self {
            Temp::Path(path) => fs::remove_file(path),
            Temp::In(dir, name) => dir.unlink(name),
        }
    }

    fn shown(&self) -> PathBuf {
        match self {
            Temp::Path(path) => path.clone(),
            Temp::In(dir, name) => dir.shown(name),
        }
    }
}

/// One job's controls, shared by its threads.
#[derive(Debug, Default)]
pub(crate) struct JobCtl {
    cancelled: AtomicBool,
    /// Stops every child of the job: a cancel, or a failed sibling segment.
    pub stop: StopFlag,
    temps: Mutex<Vec<Temp>>,
}

impl JobCtl {
    /// The person (or the session) cancelled the job.
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.stop.set();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Stops the job's other children after a failure.
    pub(crate) fn abort(&self) {
        self.stop.set();
    }

    /// Undoes [`JobCtl::abort`] before a retry, but never a cancel: `cancel`
    /// sets `cancelled` before `stop`, so a cancel racing this still ends
    /// with `stop` set.
    pub(crate) fn resume(&self) {
        self.stop.clear();
        if self.is_cancelled() {
            self.stop.set();
        }
    }

    /// Remembers a temp file to delete if the job doesn't finish.
    pub(crate) fn add_temp(&self, temp: impl Into<Temp>) {
        let temp = temp.into();
        let mut all = in_flight();
        if !all.contains(&temp) {
            all.push(temp.clone());
        }
        drop(all);
        self.temps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(temp);
    }

    /// The temp file became an output (or is kept on purpose).
    pub(crate) fn forget_temp(&self, temp: impl Into<Temp>) {
        let temp = temp.into();
        self.temps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|t| *t != temp);
        in_flight().retain(|t| *t != temp);
    }

    /// Deletes every remembered temp file (cancel, failure).
    pub(crate) fn remove_temps(&self) {
        let temps = std::mem::take(&mut *self.temps.lock().unwrap_or_else(PoisonError::into_inner));
        for temp in temps {
            remove_temp(&temp);
            in_flight().retain(|t| *t != temp);
        }
    }
}

/// The temp outputs the jobs of this process are writing.
static IN_FLIGHT: Mutex<Vec<Temp>> = Mutex::new(Vec::new());

fn in_flight() -> MutexGuard<'static, Vec<Temp>> {
    IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner)
}

fn remove_temp(temp: &Temp) {
    match temp.remove() {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("can't delete the temp file {}: {e}", temp.shown().display()),
    }
}

/// Deletes the temp output of every job still running in this process
/// (outputs already placed, or kept on purpose, are not in the list), for
/// a process about to `_exit` with its children killed (A§8.6). Returns
/// how many it deleted or found gone.
pub fn remove_temps_in_flight() -> usize {
    let temps = std::mem::take(&mut *in_flight());
    for temp in &temps {
        remove_temp(temp);
    }
    temps.len()
}

/// The running jobs. A job id may have several runs at once (an autocut
/// job analyzes several silence keys), so entries are reference-counted.
#[derive(Debug, Default)]
pub(crate) struct Registry {
    jobs: Mutex<BTreeMap<JobId, (Arc<JobCtl>, usize)>>,
}

impl Registry {
    /// The job's controls; the run is counted until the guard drops.
    pub(crate) fn enter(self: &Arc<Self>, job: JobId) -> RunGuard {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = jobs
            .entry(job)
            .or_insert_with(|| (Arc::new(JobCtl::default()), 0));
        entry.1 += 1;
        RunGuard {
            registry: Arc::clone(self),
            job,
            ctl: Arc::clone(&entry.0),
        }
    }

    /// Cancels a running job; `false` when it isn't running (finished, or
    /// never started here).
    pub(crate) fn cancel(&self, job: JobId) -> bool {
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        match jobs.get(&job) {
            Some((ctl, _)) => {
                ctl.cancel();
                true
            }
            None => false,
        }
    }

    pub(crate) fn is_running(&self, job: JobId) -> bool {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&job)
    }

    fn leave(&self, job: JobId) {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = jobs.get_mut(&job) {
            entry.1 -= 1;
            if entry.1 == 0 {
                jobs.remove(&job);
            }
        }
    }
}

/// One run of a job; leaving the registry when dropped (even on a panic).
#[derive(Debug)]
pub(crate) struct RunGuard {
    registry: Arc<Registry>,
    job: JobId,
    pub ctl: Arc<JobCtl>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.registry.leave(self.job);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn runs_share_one_control_and_leave_on_drop() {
        let registry = Arc::new(Registry::default());
        let a = registry.enter(JobId(1));
        let b = registry.enter(JobId(1));
        assert!(Arc::ptr_eq(&a.ctl, &b.ctl));
        assert!(registry.cancel(JobId(1)));
        assert!(a.ctl.is_cancelled() && b.ctl.stop.is_set());
        drop(a);
        assert!(registry.is_running(JobId(1)));
        drop(b);
        assert!(!registry.is_running(JobId(1)));
        assert!(
            !registry.cancel(JobId(1)),
            "a finished job can't be cancelled"
        );
        // A new run starts fresh.
        let c = registry.enter(JobId(1));
        assert!(!c.ctl.is_cancelled());
    }

    #[test]
    fn temps_are_deleted_unless_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, gone) = (dir.path().join("kept"), dir.path().join("gone"));
        fs::write(&kept, "").unwrap();
        fs::write(&gone, "").unwrap();
        let ctl = JobCtl::default();
        ctl.add_temp(kept.clone());
        ctl.add_temp(gone.clone());
        ctl.add_temp(dir.path().join("never-created"));
        ctl.forget_temp(&kept);
        ctl.remove_temps();
        assert!(kept.exists() && !gone.exists());
        ctl.abort();
        assert!(ctl.stop.is_set() && !ctl.is_cancelled());
        ctl.resume();
        assert!(!ctl.stop.is_set(), "an abort can be undone");
        ctl.cancel();
        ctl.resume();
        assert!(ctl.stop.is_set(), "a cancel can't");
    }

    /// A hard exit deletes what running jobs are writing: never an output
    /// already placed (or kept), nor a temp a job already removed. (Other
    /// tests' jobs share the process-wide list: only ours are looked at.)
    #[test]
    fn temps_in_flight_are_the_unfinished_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name);
        let ours = || -> BTreeSet<PathBuf> {
            in_flight()
                .iter()
                .map(Temp::shown)
                .filter(|p| p.starts_with(dir.path()))
                .collect()
        };
        let ctl = JobCtl::default();
        ctl.add_temp(path("writing"));
        ctl.add_temp(path("placed"));
        ctl.forget_temp(path("placed"));
        let other = JobCtl::default();
        other.add_temp(path("removed"));
        other.remove_temps();
        assert_eq!(ours(), BTreeSet::from([path("writing")]));
        ctl.remove_temps();
        assert!(ours().is_empty(), "a job that cleaned up leaves nothing");
    }

    /// An output's temp is deleted in the folder the job opened, wherever
    /// that folder is now and wherever its old path leads (A§8.8). (A hard
    /// exit deletes them the same way; it isn't called here: other tests'
    /// jobs share the process-wide list.)
    #[test]
    fn temps_in_an_open_folder_are_deleted_there() {
        let tmp = tempfile::tempdir().unwrap();
        let (out, moved, other) = (
            tmp.path().join("out"),
            tmp.path().join("moved"),
            tmp.path().join("other"),
        );
        fs::create_dir(&out).unwrap();
        fs::create_dir(&other).unwrap();
        let dir = OutDir::open(&out).unwrap();
        for name in ["a.part", "b.part"] {
            drop(dir.create(name.as_ref()).unwrap());
            fs::write(other.join(name), "not a temp").unwrap();
        }
        let ctl = JobCtl::default();
        ctl.add_temp(Temp::In(dir.clone(), "a.part".into()));
        ctl.add_temp(Temp::In(dir.clone(), "b.part".into()));
        fs::rename(&out, &moved).unwrap();
        std::os::unix::fs::symlink(&other, &out).unwrap();
        // One became an output: it stays.
        ctl.forget_temp(Temp::In(dir.clone(), "b.part".into()));
        ctl.remove_temps();
        assert!(!moved.join("a.part").exists() && moved.join("b.part").exists());
        assert!(other.join("a.part").exists() && other.join("b.part").exists());
        assert!(
            !in_flight()
                .iter()
                .any(|t| matches!(t, Temp::In(d, _) if d.same(&dir))),
            "nothing left for a hard exit to delete"
        );
    }
}
