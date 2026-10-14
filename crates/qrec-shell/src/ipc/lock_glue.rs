//! Interim copies of the store owner's lock primitives (A§8.3 steps 1–3),
//! with the same names and signatures as `crate::lock::ProjectLock`,
//! `crate::host::lockfile` and `crate::host::publish` (store owner,
//! `wip/session-store`), which were stubs in this branch's base. Integrating
//! means pointing the IPC/host imports at those modules and deleting this
//! file. Two additions are requested from the store owner and live here
//! until then: [`ProjectLock::is_held`] (the read-only routing probe) and
//! [`lockfile::HostState::Stopping`].
//!
//! - `.qrec/lock` is opened `O_CLOEXEC | O_NOFOLLOW`, never truncated on
//!   open, and locked with `flock(LOCK_EX | LOCK_NB)` (never blocking). The
//!   flock belongs to the open file description: closing the fd releases it
//!   and a child that inherited the description keeps it. Nothing here ever
//!   unlocks explicitly.
//! - The record `{pid, kind, socket, since, state}` lives in the lock file
//!   and is rewritten in place: replacing the file would orphan the flock.
//! - Publishing unlinks `<hash>.sock`, binds `<hash>.sock.<pid>`, listens and
//!   renames it over `<hash>.sock`, so the socket appears atomically.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
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
    /// `mkdir .qrec` (A§8.4 init order), then takes the lock without
    /// blocking: `Ok(None)` when another open file description holds it.
    /// On success the previous holder's stale record is cleared (an empty
    /// record reads as "starting").
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
        match flock(&file, libc::LOCK_EX) {
            Ok(true) => {
                file.set_len(0)
                    .map_err(|e| ShellError::io(format!("truncate {}", path.display()), e))?;
                Ok(Some(ProjectLock { file, path }))
            }
            Ok(false) => Ok(None),
            Err(e) => Err(ShellError::io(format!("lock {}", path.display()), e)),
        }
    }

    /// Whether some process holds the lock, without taking it or creating
    /// anything (read-only commands, A§8.4): a shared lock coexists with
    /// other probes and fails only against a host's exclusive one. A missing
    /// lock file means no host. (Requested from the store owner.)
    pub fn is_held(project_dir: &Path) -> Result<bool, ShellError> {
        let path = ProjectPaths::new(project_dir).lock_file();
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(false);
            }
            Err(e) => return Err(ShellError::io(format!("open {}", path.display()), e)),
        };
        // Closing `file` releases the probe's shared lock.
        match flock(&file, libc::LOCK_SH) {
            Ok(free) => Ok(!free),
            Err(e) => Err(ShellError::io(format!("probe {}", path.display()), e)),
        }
    }

    /// Adopts a lock inherited as a descriptor (`qrec host --headless
    /// --lock-fd 3`, A§8.3 step 5). The descriptor must be the project's
    /// `.qrec/lock` (same device and inode) and the lock must be held
    /// through it (or by nobody). `FD_CLOEXEC` is set so our own children
    /// never inherit it.
    pub fn adopt(project_dir: &Path, fd: OwnedFd) -> Result<ProjectLock, ShellError> {
        let path = ProjectPaths::new(project_dir).lock_file();
        let file = File::from(fd);
        let mine = file
            .metadata()
            .map_err(|e| ShellError::io("stat the inherited lock descriptor", e))?;
        let on_disk =
            fs::metadata(&path).map_err(|e| ShellError::io(format!("stat {}", path.display()), e))?;
        if (mine.dev(), mine.ino()) != (on_disk.dev(), on_disk.ino()) {
            return Err(CoreError::internal(format!(
                "the inherited lock descriptor is not {}",
                path.display()
            ))
            .into());
        }
        set_cloexec(file.as_raw_fd())
            .map_err(|e| ShellError::io("set FD_CLOEXEC on the lock", e))?;
        match flock(&file, libc::LOCK_EX) {
            Ok(true) => Ok(ProjectLock { file, path }),
            Ok(false) => Err(CoreError::busy(format!(
                "{} is held by another process, not through the inherited descriptor",
                path.display()
            ))
            .into()),
            Err(e) => Err(ShellError::io(format!("lock {}", path.display()), e)),
        }
    }

    /// The lock file, where the host record is written.
    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A duplicate of the locked descriptor (same open file description, so
    /// the same lock) for the spawn hand-off. `O_CLOEXEC` is set on it.
    pub fn try_clone_fd(&self) -> io::Result<OwnedFd> {
        self.file.as_fd().try_clone_to_owned()
    }
}

impl AsFd for ProjectLock {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// `flock(op | LOCK_NB)`: `Ok(false)` when someone else holds it.
fn flock(file: &File, op: libc::c_int) -> io::Result<bool> {
    loop {
        // SAFETY: flock only reads the descriptor number, which `file` keeps open.
        if unsafe { libc::flock(file.as_raw_fd(), op | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) => return Ok(false),
            Some(libc::EINTR) => {}
            _ => return Err(err),
        }
    }
}

/// Sets `FD_CLOEXEC` on `fd`.
pub fn set_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl on an fd number; errors are reported.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The lock record (`crate::host::lockfile` in the store's version).
pub mod lockfile {
    use std::fs;
    use std::io;
    use std::os::unix::fs::FileExt;
    use std::path::Path;

    use chrono::{DateTime, Utc};
    use qrec_core::state::HostKind;
    use serde::{Deserialize, Serialize};

    use super::ProjectLock;
    use crate::error::ShellError;
    use crate::paths::ProjectPaths;

    /// What the lock holder says about itself (A§8.3 step 1).
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct LockRecord {
        pub pid: u32,
        pub kind: HostKind,
        /// The socket the host serves (`<runtime>/<hash>.sock`).
        pub socket: String,
        pub since: DateTime<Utc>,
        pub state: HostState,
    }

    /// Whether the host accepts connections.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum HostState {
        /// Locked, socket not published yet: retry.
        Starting,
        /// The socket is published: connect.
        Ready,
        /// Stopped accepting, finishing its work before exiting: retry.
        /// (An addition requested from the store owner; readers that don't
        /// know it treat the record as unreadable, i.e. "retry".)
        Stopping,
    }

    impl LockRecord {
        /// This process as a `starting` host of `kind` that will serve `socket`.
        pub fn starting(kind: HostKind, socket: &Path, since: DateTime<Utc>) -> Self {
            Self {
                pid: std::process::id(),
                kind,
                socket: socket.to_string_lossy().into_owned(),
                since,
                state: HostState::Starting,
            }
        }
    }

    /// Writes `record` into the lock file in place.
    pub fn write(lock: &ProjectLock, record: &LockRecord) -> Result<(), ShellError> {
        let mut bytes = serde_json::to_vec(record)
            .map_err(|e| ShellError::Internal(format!("serialize the lock record: {e}")))?;
        bytes.push(b'\n');
        let what = || format!("write {}", lock.path().display());
        lock.file()
            .write_all_at(&bytes, 0)
            .map_err(|e| ShellError::io(what(), e))?;
        lock.file()
            .set_len(bytes.len() as u64)
            .map_err(|e| ShellError::io(what(), e))
    }

    /// Sets the record's state and rewrites it.
    pub fn set_state(
        lock: &ProjectLock,
        record: &mut LockRecord,
        state: HostState,
    ) -> Result<(), ShellError> {
        record.state = state;
        write(lock, record)
    }

    /// The record in the project's `.qrec/lock`: `Ok(None)` when the file
    /// is missing, empty or caught mid-write. Never writes anything.
    pub fn read(project_dir: &Path) -> Result<Option<LockRecord>, ShellError> {
        let path = ProjectPaths::new(project_dir).lock_file();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ShellError::io(format!("read {}", path.display()), e)),
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }
}

/// Socket publishing (`crate::host::publish` in the store's version).
pub mod publish {
    use std::fs;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    use crate::error::ShellError;
    use crate::paths;

    /// A listening socket published at its final path. Dropping it
    /// unpublishes (unlinks the path if it is still ours).
    #[derive(Debug)]
    pub struct PublishedSocket {
        listener: UnixListener,
        path: PathBuf,
        id: (u64, u64),
        unpublished: bool,
    }

    /// Publishes a listening socket at `socket`, under the project lock.
    /// The runtime dir (its parent) is created 0700 or checked.
    pub fn publish(socket: &Path) -> Result<PublishedSocket, ShellError> {
        let runtime = socket
            .parent()
            .ok_or_else(|| ShellError::Internal(format!("{} has no parent dir", socket.display())))?;
        paths::ensure_private_dir(runtime, rustix::process::getuid().as_raw())?;
        let temp = paths::socket_publish_path(socket, std::process::id())?;
        remove_if_present(socket)
            .map_err(|e| ShellError::io(format!("unlink {}", socket.display()), e))?;
        remove_if_present(&temp)
            .map_err(|e| ShellError::io(format!("unlink {}", temp.display()), e))?;
        let listener = UnixListener::bind(&temp)
            .map_err(|e| ShellError::io(format!("bind {}", temp.display()), e))?;
        let published = fs::symlink_metadata(&temp)
            .and_then(|meta| fs::rename(&temp, socket).map(|()| (meta.dev(), meta.ino())));
        match published {
            Ok(id) => Ok(PublishedSocket {
                listener,
                path: socket.to_path_buf(),
                id,
                unpublished: false,
            }),
            Err(e) => {
                let _ = fs::remove_file(&temp);
                Err(ShellError::io(format!("publish {}", socket.display()), e))
            }
        }
    }

    fn remove_if_present(path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    impl PublishedSocket {
        pub fn listener(&self) -> &UnixListener {
            &self.listener
        }

        /// Another handle to the listening socket (for an accept thread).
        pub fn try_clone_listener(&self) -> io::Result<UnixListener> {
            self.listener.try_clone()
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        /// Unlinks the socket if the path is still the one this host bound;
        /// `Ok(false)` when it is gone or belongs to someone else. Idempotent.
        pub fn unpublish(&mut self) -> Result<bool, ShellError> {
            if self.unpublished {
                return Ok(false);
            }
            self.unpublished = true;
            match fs::symlink_metadata(&self.path) {
                Ok(meta) if (meta.dev(), meta.ino()) == self.id => fs::remove_file(&self.path)
                    .map(|()| true)
                    .map_err(|e| ShellError::io(format!("unlink {}", self.path.display()), e)),
                Ok(_) => Ok(false),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(ShellError::io(format!("stat {}", self.path.display()), e)),
            }
        }
    }

    impl Drop for PublishedSocket {
        fn drop(&mut self) {
            let _ = self.unpublish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::lockfile::{HostState, LockRecord, read, set_state, write};
    use super::publish::publish;
    use super::*;
    use crate::ipc::peer::connect_nonblocking;
    use crate::ipc::testing::wait_for;
    use chrono::Utc;
    use std::time::Duration;
    use qrec_core::ErrorCode;
    use qrec_core::state::HostKind;
    use std::os::fd::FromRawFd;
    use std::os::unix::net::UnixListener;

    #[test]
    fn one_holder_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::new(dir.path());
        assert!(!ProjectLock::is_held(dir.path()).unwrap(), "no lock file yet");
        assert!(!paths.qrec_dir().exists(), "probing creates nothing");

        let lock = ProjectLock::try_acquire(dir.path()).unwrap().expect("free");
        assert!(paths.lock_file().is_file());
        // A second open file description can't take it, even in this process.
        assert!(ProjectLock::try_acquire(dir.path()).unwrap().is_none());
        assert!(ProjectLock::is_held(dir.path()).unwrap());

        let mut record = LockRecord::starting(HostKind::Cli, Path::new("/run/x.sock"), Utc::now());
        write(&lock, &record).unwrap();
        assert_eq!(read(dir.path()).unwrap(), Some(record.clone()));
        // Rewriting in place: a shorter record leaves no trailing bytes.
        record.socket = "/s".into();
        set_state(&lock, &mut record, HostState::Stopping).unwrap();
        assert_eq!(read(dir.path()).unwrap(), Some(record.clone()));

        drop(lock);
        // Other test threads' fork/exec windows can hold a copy of the fd
        // for a moment, so "released" is waited for.
        assert!(wait_for(Duration::from_secs(2), || !ProjectLock::is_held(dir.path()).unwrap()));
        assert_eq!(read(dir.path()).unwrap(), Some(record), "stale until the next holder");
        let _again = acquire_soon(dir.path());
        assert_eq!(read(dir.path()).unwrap(), None, "the next holder clears it");
    }

    /// Takes the lock, allowing for other threads' fork/exec windows.
    fn acquire_soon(dir: &Path) -> ProjectLock {
        let mut lock = None;
        assert!(wait_for(Duration::from_secs(2), || {
            lock = ProjectLock::try_acquire(dir).unwrap();
            lock.is_some()
        }));
        lock.unwrap()
    }

    #[test]
    fn torn_records_read_as_none_and_symlinks_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::new(dir.path());
        assert_eq!(read(dir.path()).unwrap(), None);
        fs::create_dir(paths.qrec_dir()).unwrap();
        fs::write(paths.lock_file(), b"{\"pid\":1,\"ki").unwrap();
        assert_eq!(read(dir.path()).unwrap(), None);

        fs::remove_file(paths.lock_file()).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), paths.lock_file()).unwrap();
        assert_eq!(
            CoreError::from(ProjectLock::try_acquire(dir.path()).unwrap_err()).code,
            ErrorCode::Io
        );
        assert!(!dir.path().join("elsewhere").exists());
    }

    #[test]
    fn adopting_an_inherited_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let lock = ProjectLock::try_acquire(dir.path()).unwrap().unwrap();
        // A duplicate shares the open file description, like fork + exec.
        let adopted = ProjectLock::adopt(dir.path(), lock.try_clone_fd().unwrap()).unwrap();
        // SAFETY: fcntl query on our fd.
        let flags = unsafe { libc::fcntl(adopted.as_fd().as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        // The parent closes its fd and never unlocks: the adopter keeps it.
        drop(lock);
        assert!(ProjectLock::try_acquire(dir.path()).unwrap().is_none());
        drop(adopted);
        let holder = acquire_soon(dir.path());

        // Another file, or a description that doesn't hold the lock, is refused.
        let other = File::create(dir.path().join("other")).unwrap();
        let err = ProjectLock::adopt(dir.path(), other.as_fd().try_clone_to_owned().unwrap());
        assert_eq!(CoreError::from(err.unwrap_err()).code, ErrorCode::Internal);
        let reopened = File::open(holder.path()).unwrap();
        // SAFETY: a dup we own.
        let fresh = unsafe { OwnedFd::from_raw_fd(libc::dup(reopened.as_raw_fd())) };
        let err = ProjectLock::adopt(dir.path(), fresh).unwrap_err();
        assert_eq!(CoreError::from(err).code, ErrorCode::Busy);
    }

    #[test]
    fn publishing_replaces_a_stale_socket_atomically() {
        let dir = tempfile::Builder::new().prefix("qp-").tempdir_in("/tmp").unwrap();
        let runtime = dir.path().join("run");
        let socket = runtime.join("p.sock");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        // A crashed host left its socket file behind: connecting is refused.
        drop(UnixListener::bind(&socket).unwrap());
        assert_eq!(
            connect_nonblocking(&socket).unwrap_err().raw_os_error(),
            Some(libc::ECONNREFUSED)
        );
        let mut published = publish(&socket).unwrap();
        assert!(!runtime.join(format!("p.sock.{}", std::process::id())).exists());
        let _client = connect_nonblocking(&socket).unwrap();
        assert!(published.listener().accept().is_ok());

        // Someone else's socket at the path is never unlinked.
        let theirs = publish(&socket).unwrap();
        assert!(!published.unpublish().unwrap());
        assert!(socket.exists());
        drop(theirs);
        assert!(!socket.exists());
    }
}
