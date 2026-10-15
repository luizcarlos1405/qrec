//! Running jobs and their cancellation (A§8.6, A§8.7): `cancel(job)` marks
//! the job and stops its children (the child runner sends SIGTERM to each
//! process group at once and SIGKILL after the grace period); the job thread
//! then deletes its temp outputs and reports how it ended. Every temp output
//! being written is also in a process-wide list, so a hard exit (a second
//! signal in a headless host, A§8.6), which no job thread outlives, deletes
//! them too ([`remove_temps_in_flight`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use qrec_core::JobId;

use super::child::StopFlag;

/// One job's controls, shared by its threads.
#[derive(Debug, Default)]
pub(crate) struct JobCtl {
    cancelled: AtomicBool,
    /// Stops every child of the job: a cancel, or a failed sibling segment.
    pub stop: StopFlag,
    temps: Mutex<Vec<PathBuf>>,
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
    pub(crate) fn add_temp(&self, path: PathBuf) {
        in_flight().insert(path.clone());
        self.temps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(path);
    }

    /// The temp file became an output (or is kept on purpose).
    pub(crate) fn forget_temp(&self, path: &Path) {
        self.temps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|p| p != path);
        in_flight().remove(path);
    }

    /// Deletes every remembered temp file (cancel, failure).
    pub(crate) fn remove_temps(&self) {
        let temps = std::mem::take(&mut *self.temps.lock().unwrap_or_else(PoisonError::into_inner));
        for path in temps {
            remove_temp(&path);
            in_flight().remove(&path);
        }
    }
}

/// The temp outputs the jobs of this process are writing.
static IN_FLIGHT: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

fn in_flight() -> MutexGuard<'static, BTreeSet<PathBuf>> {
    IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner)
}

fn remove_temp(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("can't delete the temp file {}: {e}", path.display()),
    }
}

/// Deletes the temp output of every job still running in this process
/// (outputs already placed, or kept on purpose, are not in the list), for
/// a process about to `_exit` with its children killed (A§8.6). Returns
/// how many it deleted or found gone.
pub fn remove_temps_in_flight() -> usize {
    let temps = std::mem::take(&mut *in_flight());
    for path in &temps {
        remove_temp(path);
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
                .filter(|p| p.starts_with(dir.path()))
                .cloned()
                .collect()
        };
        let ctl = JobCtl::default();
        ctl.add_temp(path("writing"));
        ctl.add_temp(path("placed"));
        ctl.forget_temp(&path("placed"));
        let other = JobCtl::default();
        other.add_temp(path("removed"));
        other.remove_temps();
        assert_eq!(ours(), BTreeSet::from([path("writing")]));
        ctl.remove_temps();
        assert!(ours().is_empty(), "a job that cleaned up leaves nothing");
    }
}
