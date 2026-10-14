//! The lock file's JSON record (`{pid, kind, socket, since,
//! state:"starting"|"ready"|"stopping"}`) and reading it while routing (A§8.3).
//! Owner: store.
//!
//! The record lives in `.qrec/lock` itself, the file the host holds the
//! `flock` on, so it is written in place (never replaced: a new inode
//! wouldn't be locked). A reader can catch it mid-write; that reads as "no
//! record yet", which routing treats like `starting` (retry in 50 ms).
//! Only trust a record while the lock is held: a free lock means the
//! record is stale.

use std::fs;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use chrono::{DateTime, Utc};
use qrec_core::state::HostKind;
use serde::{Deserialize, Serialize};

use crate::error::ShellError;
use crate::lock::ProjectLock;
use crate::paths::ProjectPaths;

/// What the lock holder says about itself.
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
    /// Stopped accepting, finishing its work before exiting: retry (and
    /// say so when the deadline passes).
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

/// Writes `record` into the lock file: one positioned write at offset 0,
/// then the file is cut to its length. No fsync: the record only matters
/// while the lock is held.
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

/// The record in the project's `.qrec/lock`: `Ok(None)` when the file is
/// missing, empty or caught mid-write. Never writes or deletes anything.
pub fn read(project_dir: &Path) -> Result<Option<LockRecord>, ShellError> {
    read_file(&ProjectPaths::new(project_dir).lock_file())
}

/// [`read`] for a lock file path.
pub fn read_file(path: &Path) -> Result<Option<LockRecord>, ShellError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ShellError::io(format!("read {}", path.display()), e)),
    };
    Ok(serde_json::from_slice(&bytes).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn since() -> DateTime<Utc> {
        "2026-10-14T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn records_round_trip_in_the_lock_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read(tmp.path()).unwrap(), None, "no .qrec yet");
        let lock = ProjectLock::try_acquire(tmp.path()).unwrap().unwrap();
        assert_eq!(read(tmp.path()).unwrap(), None, "empty: starting");

        let mut record =
            LockRecord::starting(HostKind::Headless, Path::new("/run/q/abc.sock"), since());
        write(&lock, &record).unwrap();
        assert_eq!(read(tmp.path()).unwrap().as_ref(), Some(&record));
        let text = fs::read_to_string(lock.path()).unwrap();
        assert_eq!(
            text,
            format!(
                "{{\"pid\":{},\"kind\":\"headless\",\"socket\":\"/run/q/abc.sock\",\"since\":\"2026-10-14T12:00:00Z\",\"state\":\"starting\"}}\n",
                std::process::id()
            )
        );

        // A shorter record leaves no tail of the longer one behind.
        record.socket = "/s".into();
        set_state(&lock, &mut record, HostState::Ready).unwrap();
        let read_back = read(tmp.path()).unwrap().unwrap();
        assert_eq!(read_back.state, HostState::Ready);
        assert_eq!(read_back.socket, "/s");
        set_state(&lock, &mut record, HostState::Stopping).unwrap();
        assert!(
            fs::read_to_string(lock.path())
                .unwrap()
                .contains("\"state\":\"stopping\"")
        );
        assert_eq!(read(tmp.path()).unwrap().unwrap().state, HostState::Stopping);
        // The record is written through the locked file itself.
        assert!(ProjectLock::try_acquire(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn a_torn_record_reads_as_none_and_a_new_host_clears_a_stale_one() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = ProjectLock::try_acquire(tmp.path()).unwrap().unwrap();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(lock.path())
            .unwrap();
        file.write_all(b"{\"pid\":12,\"kind\":\"gui\",\"soc")
            .unwrap();
        assert_eq!(read(tmp.path()).unwrap(), None);

        let record = LockRecord::starting(HostKind::Gui, Path::new("/x.sock"), since());
        write(&lock, &record).unwrap();
        drop(lock);
        // The record outlives its host…
        assert!(read(tmp.path()).unwrap().is_some());
        // …until the next host takes the lock.
        let _next = crate::lock::acquire_eventually(tmp.path());
        assert_eq!(read(tmp.path()).unwrap(), None);
    }
}
