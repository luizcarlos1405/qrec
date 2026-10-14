//! Peer credentials of a Unix socket (`SO_PEERCRED`, A§8.3 step 7) and the
//! non-blocking connect the client uses, so a host whose accept backlog is
//! full can't make the CLI hang.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Who is at the other end of a connection, as of its `connect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCred {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

/// The credentials of the process that connected `stream` (`SO_PEERCRED`).
pub fn peer_cred(stream: &UnixStream) -> io::Result<PeerCred> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let expected = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let mut len = expected;
    // SAFETY: the fd is open for the lifetime of `stream`; `cred` and `len`
    // are valid for writes of `len` bytes.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if len != expected {
        return Err(io::Error::other("short SO_PEERCRED answer"));
    }
    Ok(PeerCred {
        pid: u32::try_from(cred.pid).unwrap_or(0),
        uid: cred.uid,
        gid: cred.gid,
    })
}

/// The uid peers must have: our effective uid (what `SO_PEERCRED` reports).
pub fn our_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Connects to a Unix socket without ever blocking: a listener whose
/// backlog is full gives [`io::ErrorKind::WouldBlock`] instead of a hang.
/// The returned stream is in blocking mode.
pub fn connect_nonblocking(path: &Path) -> io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: an all-zero sockaddr_un is a valid value.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path too long or contains NUL",
        ));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
        *dst = *src as libc::c_char;
    }
    let len = (std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
    // SAFETY: plain socket(2) call; the result is checked.
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a fresh fd we own.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    loop {
        // SAFETY: `addr` is initialized and `len` covers the path and its NUL.
        let rc = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&addr as *const libc::sockaddr_un).cast(),
                len,
            )
        };
        if rc == 0 {
            break;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn peer_is_us() {
        let (a, _b) = UnixStream::pair().unwrap();
        let cred = peer_cred(&a).unwrap();
        assert_eq!(cred.uid, our_uid());
        assert_eq!(cred.pid, std::process::id());
    }

    #[test]
    fn connects_and_reports_missing_or_dead_sockets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let err = connect_nonblocking(&path).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));

        let listener = UnixListener::bind(&path).unwrap();
        let stream = connect_nonblocking(&path).unwrap();
        let (server_side, _) = listener.accept().unwrap();
        assert_eq!(peer_cred(&server_side).unwrap().uid, our_uid());
        drop((stream, server_side));

        // A socket file nobody listens on any more.
        drop(listener);
        let err = connect_nonblocking(&path).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ECONNREFUSED));

        let long = Path::new("/tmp").join("x".repeat(200));
        assert_eq!(
            connect_nonblocking(&long).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
