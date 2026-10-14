//! Running jobs and their cancellation (A§8.6, A§8.7): `cancel(job)` marks
//! the job and stops its children (the child runner sends SIGTERM to each
//! process group at once and SIGKILL after the grace period); the job thread
//! then deletes its temp outputs and reports how it ended.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

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

    /// Remembers a temp file to delete if the job doesn't finish.
    pub(crate) fn add_temp(&self, path: PathBuf) {
        self.temps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(path);
    }

    /// The temp file became an output (or is kept on purpose).
    pub(crate) fn forget_temp(&self, path: &std::path::Path) {
        self.temps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|p| p != path);
    }

    /// Deletes every remembered temp file (cancel, failure).
    pub(crate) fn remove_temps(&self) {
        let temps = std::mem::take(&mut *self.temps.lock().unwrap_or_else(PoisonError::into_inner));
        for path in temps {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("can't delete the temp file {}: {e}", path.display()),
            }
        }
    }
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
    }
}
