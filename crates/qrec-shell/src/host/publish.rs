//! Socket publishing under the lock (A§8.3 step 1): unlink `<hash>.sock`,
//! bind `<hash>.sock.<pid>`, listen, `rename` it to `<hash>.sock`, so the
//! socket appears atomically and a client never connects to a half-set-up
//! host. Only the lock holder unlinks its socket (step 2): unpublishing
//! removes the path only while it is still the socket this host bound.
//! Scanners never delete anything. Owner: store.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use crate::error::ShellError;
use crate::paths;

/// A listening socket published at its final path.
#[derive(Debug)]
pub struct PublishedSocket {
    listener: UnixListener,
    path: PathBuf,
    /// Device and inode of the socket file this host bound.
    id: (u64, u64),
    unpublished: bool,
}

/// Publishes a listening socket at `socket` (call it while holding the
/// project lock). The runtime dir (its parent) is created 0700 or checked.
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

    /// Another handle to the listening socket (e.g. for an accept thread).
    pub fn try_clone_listener(&self) -> io::Result<UnixListener> {
        self.listener.try_clone()
    }

    /// The published path (`<runtime>/<hash>.sock`).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Unlinks the socket if the path is still the one this host bound;
    /// `Ok(false)` when it is gone or belongs to someone else. Idempotent.
    /// Existing connections keep working; new clients get `ENOENT`.
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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use super::*;
    use crate::testkit::TestEnv;

    #[test]
    fn publish_connect_unpublish() {
        let env = TestEnv::new();
        let socket = env.runtime_dir().join(format!("{}.sock", "a".repeat(64)));
        let mut published = publish(&socket).unwrap();
        assert_eq!(published.path(), socket);
        assert!(
            !paths::socket_publish_path(&socket, std::process::id())
                .unwrap()
                .exists()
        );
        let runtime_mode = fs::metadata(env.runtime_dir()).unwrap().mode() & 0o777;
        assert_eq!(runtime_mode, 0o700);

        let mut client = UnixStream::connect(&socket).unwrap();
        let (mut server, _) = published.listener().accept().unwrap();
        client.write_all(b"hi").unwrap();
        let mut buf = [0u8; 2];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hi");

        assert!(published.unpublish().unwrap());
        assert!(!socket.exists());
        assert!(!published.unpublish().unwrap(), "idempotent");
        assert!(UnixStream::connect(&socket).is_err());
    }

    #[test]
    fn a_stale_socket_is_replaced_and_a_foreign_one_is_left_alone() {
        let env = TestEnv::new();
        let socket = env.runtime_dir().join(format!("{}.sock", "b".repeat(64)));
        let mut first = publish(&socket).unwrap();
        // A crashed host leaves its socket file behind; the next lock
        // holder replaces it.
        first.unpublished = true; // pretend it died without cleaning up
        drop(first);
        assert!(socket.exists());
        let mut second = publish(&socket).unwrap();
        assert!(UnixStream::connect(&socket).is_ok());

        // If the path changed under it, the old host doesn't remove the new socket.
        let third_path = env.runtime_dir().join(format!("{}.sock", "c".repeat(64)));
        let third = publish(&third_path).unwrap();
        fs::rename(third.path(), &socket).unwrap();
        assert!(!second.unpublish().unwrap());
        assert!(socket.exists());
        drop(third); // its own path is gone: nothing to remove
        assert!(socket.exists());
    }
}
