//! The project lock, the lock record and socket publishing (A§8.3 steps
//! 1–3), as the hosts and the router use them.
//!
//! These are interim implementations of what the store owner's `lock.rs`,
//! `host::lockfile` and `host::publish` document; the IPC layer and the hosts
//! use only the items below, so switching to the store's versions is a
//! one-module change.
//!
//! - `.qrec/lock` is opened `O_CLOEXEC | O_NOFOLLOW`, never truncated on open,
//!   and locked with `File::try_lock` (exclusive, never blocking). The flock
//!   belongs to the open file description: closing the fd releases it, and a
//!   child that inherited the description keeps it. Nothing here ever
//!   unlocks explicitly.
//! - The record `{pid, kind, socket, since, state}` lives in the lock file
//!   itself and is rewritten in place (truncate + write): replacing the file
//!   would orphan the flock.
//! - Publishing unlinks `<hash>.sock`, binds `<hash>.sock.<pid>`, listens and
//!   renames it over `<hash>.sock`, so the socket appears atomically.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, SubsecRound, Utc};
use qrec_core::CoreError;
use qrec_core::state::HostKind;
use serde::{Deserialize, Serialize};

use crate::paths::ProjectPaths;

/// The biggest lock record a reader accepts.
const MAX_RECORD: u64 = 64 * 1024;

/// The exclusive lock on a project's `.qrec/lock`. Dropping it closes the
/// fd, which releases the lock unless a child inherited the description.
#[derive(Debug)]
pub struct ProjectLock {
    file: File,
}

impl ProjectLock {
    /// `mkdir .qrec` (A§8.4 init order), then tries the lock without
    /// blocking. `Ok(None)`: another open file description holds it.
    pub fn try_acquire(paths: &ProjectPaths) -> Result<Option<ProjectLock>, CoreError> {
        let qrec = paths.qrec_dir();
        match fs::create_dir(&qrec) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_error(format!("can't create {}", qrec.display()), &e)),
        }
        let path = paths.lock_file();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| io_error(format!("can't open {}", path.display()), &e))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(ProjectLock { file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => {
                Err(io_error(format!("can't lock {}", path.display()), &e))
            }
        }
    }

    /// Whether some process holds the lock, without taking it or creating
    /// anything (read-only commands, A§8.4). A missing lock file means no host.
    pub fn is_held(paths: &ProjectPaths) -> Result<bool, CoreError> {
        let path = paths.lock_file();
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => {
                return Ok(false);
            }
            Err(e) => return Err(io_error(format!("can't open {}", path.display()), &e)),
        };
        // A shared lock coexists with other probes and fails only against a
        // host's exclusive one. Closing `file` releases it.
        match file.try_lock_shared() {
            Ok(()) => Ok(false),
            Err(TryLockError::WouldBlock) => Ok(true),
            Err(TryLockError::Error(e)) => {
                Err(io_error(format!("can't probe {}", path.display()), &e))
            }
        }
    }

    /// Adopts the lock a parent handed down as `--lock-fd` (A§8.3 step 5):
    /// the fd must be this project's `.qrec/lock`, and the lock must be ours
    /// (the inherited description holds it, or nobody does and we take it).
    /// The fd gets `FD_CLOEXEC` so our own children never inherit the lock.
    pub fn adopt_fd(fd: RawFd, paths: &ProjectPaths) -> Result<ProjectLock, CoreError> {
        if fd <= 2 {
            return Err(CoreError::usage(format!(
                "--lock-fd {fd}: expected an inherited descriptor above 2"
            )));
        }
        let path = paths.lock_file();
        // SAFETY: an all-zero `stat` is a valid value; fstat only writes it.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fstat on an arbitrary fd number is safe; EBADF is reported.
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            let e = io::Error::last_os_error();
            return Err(io_error(format!("--lock-fd {fd}"), &e));
        }
        let meta = fs::symlink_metadata(&path)
            .map_err(|e| io_error(format!("can't inspect {}", path.display()), &e))?;
        if st.st_dev as u64 != meta.dev() || st.st_ino as u64 != meta.ino() {
            return Err(CoreError::invalid(format!(
                "--lock-fd {fd} is not {}",
                path.display()
            )));
        }
        set_cloexec(fd).map_err(|e| io_error(format!("--lock-fd {fd}: can't set FD_CLOEXEC"), &e))?;
        // SAFETY: the fd is open (fstat succeeded), refers to the lock file,
        // and the caller hands its ownership to us (it came from our parent).
        let file = unsafe { File::from_raw_fd(fd) };
        match file.try_lock() {
            Ok(()) => Ok(ProjectLock { file }),
            Err(TryLockError::WouldBlock) => Err(CoreError::busy(format!(
                "another process holds {}",
                path.display()
            ))),
            Err(TryLockError::Error(e)) => {
                Err(io_error(format!("can't lock {}", path.display()), &e))
            }
        }
    }

    /// The locked fd, for handing the lock to a spawned host.
    pub fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Replaces the lock record (in place: the lock file is the record).
    pub fn write_record(&self, record: &LockRecord) -> Result<(), CoreError> {
        let mut bytes = serde_json::to_vec(record).expect("lock records serialize");
        bytes.push(b'\n');
        self.file
            .set_len(0)
            .and_then(|()| self.file.write_all_at(&bytes, 0))
            .map_err(|e| io_error("can't write the lock record".to_owned(), &e))
    }
}

/// The host's state in its lock record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LockState {
    /// Locked; the socket isn't published yet.
    Starting,
    /// The socket is published and accepting.
    Ready,
    /// The host stopped accepting and finishes its work before exiting.
    /// (An extension to A§8.3's `starting | ready`: readers treat any state
    /// but `ready` as "retry".)
    Stopping,
}

/// `.qrec/lock`'s content: who holds the project (A§8.3 step 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockRecord {
    pub pid: u32,
    pub kind: HostKind,
    /// The socket the host publishes.
    pub socket: String,
    /// When the host took the lock.
    pub since: DateTime<Utc>,
    pub state: LockState,
}

impl LockRecord {
    /// A record for this process, taken now.
    pub fn new(kind: HostKind, socket: &Path, state: LockState) -> Self {
        Self {
            pid: std::process::id(),
            kind,
            socket: socket.to_string_lossy().into_owned(),
            since: Utc::now().trunc_subsecs(0),
            state,
        }
    }

    /// The same record in another state.
    pub fn with_state(&self, state: LockState) -> Self {
        Self {
            state,
            ..self.clone()
        }
    }

    /// `headless host (pid 4242, since 2026-10-14 12:00:03)`.
    pub fn describe(&self) -> String {
        format!(
            "{} host (pid {}, since {})",
            self.kind.as_str(),
            self.pid,
            self.since
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
        )
    }
}

/// The current lock record, or `None` when the file is missing, empty
/// (a host between taking the lock and writing it) or unreadable.
pub fn read_record(paths: &ProjectPaths) -> Option<LockRecord> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(paths.lock_file())
        .ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_RECORD).read_to_end(&mut bytes).ok()?;
    serde_json::from_slice(bytes.trim_ascii()).ok()
}

/// A listening socket published at its final path.
#[derive(Debug)]
pub struct Published {
    pub listener: UnixListener,
    pub socket: PublishedSocket,
}

/// The path a host published, remembered by identity so the host unlinks
/// only its own socket (A§8.3 step 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedSocket {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl PublishedSocket {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Unlinks the socket if the path still names the one we published.
    /// Returns whether it did.
    pub fn unlink(&self) -> bool {
        match fs::symlink_metadata(&self.path) {
            Ok(meta) if meta.dev() == self.dev && meta.ino() == self.ino => {
                fs::remove_file(&self.path).is_ok()
            }
            _ => false,
        }
    }
}

/// A§8.3 step 1, under the lock: unlink `socket`, bind `<socket>.<pid>`,
/// listen, rename it to `socket`.
pub fn publish_socket(socket: &Path, pid: u32) -> Result<Published, CoreError> {
    let tmp = crate::paths::socket_publish_path(socket, pid)?;
    // A crashed host with our pid may have left its publish name behind.
    remove_if_exists(&tmp)?;
    remove_if_exists(socket)?;
    let listener = UnixListener::bind(&tmp)
        .map_err(|e| io_error(format!("can't listen on {}", tmp.display()), &e))?;
    if let Err(e) = fs::rename(&tmp, socket) {
        let _ = fs::remove_file(&tmp);
        return Err(io_error(format!("can't publish {}", socket.display()), &e));
    }
    let meta = fs::symlink_metadata(socket)
        .map_err(|e| io_error(format!("can't inspect {}", socket.display()), &e))?;
    Ok(Published {
        listener,
        socket: PublishedSocket {
            path: socket.to_path_buf(),
            dev: meta.dev(),
            ino: meta.ino(),
        },
    })
}

fn remove_if_exists(path: &Path) -> Result<(), CoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(format!("can't remove {}", path.display()), &e)),
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

fn io_error(what: String, e: &io::Error) -> CoreError {
    CoreError::io(format!("{what}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::peer::connect_nonblocking;
    use qrec_core::ErrorCode;

    #[test]
    fn one_holder_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::new(dir.path());
        assert!(!ProjectLock::is_held(&paths).unwrap(), "no lock file yet");
        assert!(!paths.qrec_dir().exists(), "probing creates nothing");

        let lock = ProjectLock::try_acquire(&paths).unwrap().expect("free");
        assert!(paths.lock_file().is_file());
        // A second open file description can't take it, even in this process.
        assert!(ProjectLock::try_acquire(&paths).unwrap().is_none());
        assert!(ProjectLock::is_held(&paths).unwrap());

        let record = LockRecord::new(HostKind::Cli, Path::new("/run/x.sock"), LockState::Starting);
        lock.write_record(&record).unwrap();
        assert_eq!(read_record(&paths), Some(record.clone()));
        // Rewriting in place: a shorter record leaves no trailing bytes.
        let ready = LockRecord {
            socket: "/s".into(),
            ..record.with_state(LockState::Ready)
        };
        lock.write_record(&ready).unwrap();
        assert_eq!(read_record(&paths), Some(ready.clone()));
        assert!(ready.describe().starts_with("cli host (pid "), "{}", ready.describe());

        drop(lock);
        assert!(!ProjectLock::is_held(&paths).unwrap());
        let again = ProjectLock::try_acquire(&paths).unwrap().expect("released on close");
        // The record of the previous holder stays until the new one writes.
        assert_eq!(read_record(&paths), Some(ready));
        drop(again);
    }

    #[test]
    fn records_that_are_missing_or_torn_read_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::new(dir.path());
        assert_eq!(read_record(&paths), None);
        fs::create_dir(paths.qrec_dir()).unwrap();
        fs::write(paths.lock_file(), b"").unwrap();
        assert_eq!(read_record(&paths), None);
        fs::write(paths.lock_file(), b"{\"pid\":1,\"ki").unwrap();
        assert_eq!(read_record(&paths), None);
    }

    #[test]
    fn a_symlinked_lock_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::new(dir.path());
        fs::create_dir(paths.qrec_dir()).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), paths.lock_file()).unwrap();
        assert_eq!(
            ProjectLock::try_acquire(&paths).unwrap_err().code,
            ErrorCode::Io
        );
        assert!(!dir.path().join("elsewhere").exists());
    }

    #[test]
    fn adopting_an_inherited_fd() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::new(dir.path());
        let lock = ProjectLock::try_acquire(&paths).unwrap().unwrap();
        // Duplicating shares the open file description, like fork + exec does.
        // SAFETY: plain dup of an fd we own.
        let dup = unsafe { libc::dup(lock.as_raw_fd()) };
        assert!(dup > 2);
        let adopted = ProjectLock::adopt_fd(dup, &paths).unwrap();
        // SAFETY: fcntl query on our fd.
        let flags = unsafe { libc::fcntl(dup, libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        // The parent closes its fd and never unlocks: the adopter keeps it.
        drop(lock);
        assert!(ProjectLock::try_acquire(&paths).unwrap().is_none());
        drop(adopted);
        assert!(ProjectLock::try_acquire(&paths).unwrap().is_some());

        // Some other file, or another holder, is refused.
        let other = File::create(dir.path().join("other")).unwrap();
        // SAFETY: plain dup.
        let other_fd = unsafe { libc::dup(other.as_raw_fd()) };
        let err = ProjectLock::adopt_fd(other_fd, &paths).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        // SAFETY: closing the dup we made (adopt_fd refused it).
        unsafe { libc::close(other_fd) };
        let holder = ProjectLock::try_acquire(&paths).unwrap().unwrap();
        let reopened = File::open(paths.lock_file()).unwrap();
        // SAFETY: plain dup.
        let fresh = unsafe { libc::dup(reopened.as_raw_fd()) };
        assert_eq!(
            ProjectLock::adopt_fd(fresh, &paths).unwrap_err().code,
            ErrorCode::Busy
        );
        drop(holder);
        assert_eq!(
            ProjectLock::adopt_fd(1, &paths).unwrap_err().code,
            ErrorCode::Usage
        );
    }

    #[test]
    fn publishing_replaces_a_stale_socket_atomically() {
        let dir = tempfile::Builder::new().prefix("qp-").tempdir_in("/tmp").unwrap();
        let socket = dir.path().join("p.sock");
        // A crashed host left its socket file behind: connecting is refused.
        drop(UnixListener::bind(&socket).unwrap());
        assert_eq!(
            connect_nonblocking(&socket).unwrap_err().raw_os_error(),
            Some(libc::ECONNREFUSED)
        );
        let published = publish_socket(&socket, 4242).unwrap();
        assert!(!dir.path().join("p.sock.4242").exists());
        let _client = connect_nonblocking(&socket).unwrap();
        assert!(published.listener.accept().is_ok());

        // Someone else's socket at the path is never unlinked.
        let theirs = publish_socket(&socket, 4243).unwrap();
        assert!(!published.socket.unlink());
        assert!(socket.exists());
        assert!(theirs.socket.unlink());
        assert!(!socket.exists());
        assert!(!theirs.socket.unlink(), "already gone");
    }
}
