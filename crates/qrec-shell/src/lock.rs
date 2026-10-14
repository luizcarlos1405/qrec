//! The project lock `.qrec/lock` (A§8.3): an exclusive `flock` taken with
//! `try_lock` (never blocking) on a file opened with `O_CLOEXEC`. Whoever
//! holds it is the project's host. Owner: store.
//!
//! The lock belongs to the open file description, not to the process: a
//! spawned headless host that inherits the descriptor (`--lock-fd 3`) keeps
//! it after the CLI that took it closes its copy. So dropping a
//! [`ProjectLock`] only closes the descriptor; it never calls `unlock`
//! (which would release the lock for every copy). The lock file also holds
//! the host's JSON record (`host::lockfile`).
//!
//! `flock` is called through `libc`: std's `File::try_lock` is the same
//! system call but needs Rust 1.89, above the workspace MSRV (1.85), which
//! `clippy::incompatible_msrv` rejects.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use qrec_core::CoreError;

use crate::error::ShellError;
use crate::paths::ProjectPaths;

/// An exclusive hold on a project's `.qrec/lock`.
#[derive(Debug)]
pub struct ProjectLock {
    file: File,
    path: PathBuf,
}

impl ProjectLock {
    /// Takes the lock of the project in `project_dir` without blocking:
    /// `Ok(None)` when another process (or another open of the file in this
    /// process) holds it. Creates `.qrec/` (one level, not `-p`: the
    /// project folder must exist) and the lock file when missing.
    ///
    /// Callers check the folder rule first: this creates `.qrec/` in any
    /// folder it is given.
    pub fn try_acquire(project_dir: &Path) -> Result<Option<ProjectLock>, ShellError> {
        let paths = ProjectPaths::new(project_dir);
        let qrec = paths.qrec_dir();
        match fs::create_dir(&qrec) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(ShellError::io(format!("create {}", qrec.display()), e)),
        }
        let path = paths.lock_file();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o644)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| ShellError::io(format!("open {}", path.display()), e))?;
        if !file
            .metadata()
            .map_err(|e| ShellError::io(format!("stat {}", path.display()), e))?
            .is_file()
        {
            return Err(CoreError::invalid(format!("{} is not a regular file", path.display())).into());
        }
        match try_flock(&file) {
            Ok(true) => {
                // A previous host's record is stale now; an empty record
                // reads as "starting" until the new host writes its own.
                file.set_len(0)
                    .map_err(|e| ShellError::io(format!("truncate {}", path.display()), e))?;
                Ok(Some(ProjectLock { file, path }))
            }
            Ok(false) => Ok(None),
            Err(e) => Err(ShellError::io(format!("lock {}", path.display()), e)),
        }
    }

    /// Adopts a lock inherited as a descriptor (`qrec host --headless
    /// --lock-fd 3`, A§8.3 step 5). The descriptor must be the project's
    /// `.qrec/lock` (same device and inode), and the lock must be held
    /// through it: taking it again on the same open file description
    /// succeeds, while it fails if the lock belongs to another process.
    pub fn adopt(project_dir: &Path, fd: OwnedFd) -> Result<ProjectLock, ShellError> {
        let path = ProjectPaths::new(project_dir).lock_file();
        let file = File::from(fd);
        let mine = file
            .metadata()
            .map_err(|e| ShellError::io("stat the inherited lock descriptor", e))?;
        let on_disk = fs::metadata(&path).map_err(|e| ShellError::io(format!("stat {}", path.display()), e))?;
        if (mine.dev(), mine.ino()) != (on_disk.dev(), on_disk.ino()) {
            return Err(CoreError::internal(format!(
                "the inherited lock descriptor is not {}",
                path.display()
            ))
            .into());
        }
        set_cloexec(&file).map_err(|e| ShellError::io("set FD_CLOEXEC on the lock", e))?;
        match try_flock(&file) {
            Ok(true) => Ok(ProjectLock { file, path }),
            Ok(false) => Err(CoreError::busy(format!(
                "{} is held by another process, not through the inherited descriptor",
                path.display()
            ))
            .into()),
            Err(e) => Err(ShellError::io(format!("lock {}", path.display()), e)),
        }
    }

    /// The lock file (`.qrec/lock`), where the host record is written.
    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A duplicate of the locked descriptor for a child to inherit (the
    /// spawn hand-off): it shares the open file description, so the child
    /// holds the same lock. `O_CLOEXEC` is set on the duplicate; the spawner
    /// moves it to the child's fd 3 in `pre_exec` (where `dup2` clears it).
    pub fn try_clone_fd(&self) -> io::Result<OwnedFd> {
        self.file.as_fd().try_clone_to_owned()
    }
}

impl AsFd for ProjectLock {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// `flock(LOCK_EX | LOCK_NB)`: `Ok(false)` when someone else holds it.
fn try_flock(file: &File) -> io::Result<bool> {
    loop {
        // SAFETY: flock only reads the descriptor number, which `file` keeps open.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) => return Ok(false),
            Some(libc::EINTR) => continue,
            _ => return Err(err),
        }
    }
}

fn set_cloexec(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    // SAFETY: fcntl on a descriptor `file` keeps open; F_GETFD/F_SETFD
    // only read and set its flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let first = ProjectLock::try_acquire(tmp.path()).unwrap().expect("free");
        assert!(tmp.path().join(".qrec/lock").is_file());
        // Another open file description, even in this process, is refused.
        assert!(ProjectLock::try_acquire(tmp.path()).unwrap().is_none());
        drop(first);
        let again = ProjectLock::try_acquire(tmp.path()).unwrap();
        assert!(again.is_some(), "dropping released the lock");
    }

    #[test]
    fn a_shared_descriptor_keeps_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = ProjectLock::try_acquire(tmp.path()).unwrap().unwrap();
        let dup = lock.try_clone_fd().unwrap();
        // The original holder goes away (the CLI after the hand-off) …
        drop(lock);
        // … but the copy (the headless host) still holds it.
        assert!(ProjectLock::try_acquire(tmp.path()).unwrap().is_none());
        let adopted = ProjectLock::adopt(tmp.path(), dup).unwrap();
        assert_eq!(adopted.path(), tmp.path().join(".qrec/lock"));
        assert!(ProjectLock::try_acquire(tmp.path()).unwrap().is_none());
        drop(adopted);
        assert!(ProjectLock::try_acquire(tmp.path()).unwrap().is_some());
    }

    #[test]
    fn adopting_a_foreign_descriptor_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let held = ProjectLock::try_acquire(tmp.path()).unwrap().unwrap();
        // A fresh open of the same file doesn't hold the lock.
        let other: OwnedFd = File::open(tmp.path().join(".qrec/lock")).unwrap().into();
        let err = CoreError::from(ProjectLock::adopt(tmp.path(), other).unwrap_err());
        assert_eq!(err.code, qrec_core::ErrorCode::Busy);
        drop(held);
        // Some other file isn't the lock at all.
        fs::write(tmp.path().join("x"), "").unwrap();
        let wrong: OwnedFd = File::open(tmp.path().join("x")).unwrap().into();
        let err = CoreError::from(ProjectLock::adopt(tmp.path(), wrong).unwrap_err());
        assert_eq!(err.code, qrec_core::ErrorCode::Internal);
    }

    #[test]
    fn the_project_folder_must_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let err = ProjectLock::try_acquire(&tmp.path().join("missing/deeper")).unwrap_err();
        assert_eq!(CoreError::from(err).code, qrec_core::ErrorCode::Io);
    }
}
