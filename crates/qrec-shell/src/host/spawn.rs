//! The spawn hand-off (A§8.3 step 5). Owner: ipc.
//!
//! `record start|toggle` with no host: the CLI holds the lock and spawns
//! `qrec host --headless --project DIR --lock-fd 3 --ready-fd 4`. In the
//! child (`pre_exec`) the lock fd and the ready pipe are `dup2`ed onto 3
//! and 4 (which clears `FD_CLOEXEC`) and `setsid` detaches it from the
//! terminal; stdin is null and stdout/stderr append to
//! `.qrec/logs/<stamp>-<pid>-host.log`. flock belongs to the open file
//! description, so the child keeps the lock: the CLI closes its fd and
//! **never unlocks**. The host writes one JSON line on fd 4 (`{"ok":true}`
//! once listening, or the error) and closes it; EOF without a line means it
//! died, and the CLI points at the log.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{Local, SubsecRound, Utc};
use qrec_core::CoreError;
use qrec_core::state::HostKind;
use serde_json::Value;

use super::signals::Interrupts;
use crate::error::ShellError;
use crate::ipc::lock_glue::ProjectLock;
use crate::ipc::lock_glue::lockfile::{self, LockRecord};
use crate::ipc::server::pipe_cloexec;
use crate::paths::ProjectPaths;

/// The fds the child finds its lock and ready pipe on.
const CHILD_LOCK_FD: RawFd = 3;
const CHILD_READY_FD: RawFd = 4;
/// Parent-side duplicates live above this, so `dup2` onto 3 and 4 in the
/// child can't clobber one with the other.
const HIGH_FD: RawFd = 10;
/// The longest ready line we read.
const MAX_READY_LINE: usize = 64 * 1024;
/// How often the wait for the ready line looks at interrupts.
const READY_SLICE: Duration = Duration::from_millis(100);

/// What to run as the headless host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProgram {
    pub program: PathBuf,
    /// Arguments before `--project DIR --lock-fd 3 --ready-fd 4`.
    pub args: Vec<OsString>,
    /// Extra environment for the host (on top of the CLI's).
    pub env: Vec<(OsString, OsString)>,
}

impl HostProgram {
    /// `<this executable> host --headless`.
    pub fn current_exe() -> io::Result<HostProgram> {
        Ok(HostProgram {
            program: std::env::current_exe()?,
            args: vec!["host".into(), "--headless".into()],
            env: Vec::new(),
        })
    }
}

/// A host that reported ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedHost {
    pub pid: u32,
    /// Its stdout/stderr.
    pub log: PathBuf,
}

/// Why the hand-off failed.
#[derive(Debug)]
pub enum SpawnError {
    /// Ctrl+C while waiting; the half-started host got SIGTERM.
    Interrupted,
    Failed(CoreError),
}

impl From<CoreError> for SpawnError {
    fn from(e: CoreError) -> Self {
        SpawnError::Failed(e)
    }
}

impl From<ShellError> for SpawnError {
    fn from(e: ShellError) -> Self {
        SpawnError::Failed(e.into())
    }
}

/// Spawns the headless host with `lock` and waits (until `deadline`) for
/// its ready line. `socket` is what the host will publish (for the lock
/// record the CLI writes meanwhile, so routing clients wait for it).
pub fn spawn_headless(
    lock: ProjectLock,
    paths: &ProjectPaths,
    socket: &Path,
    program: &HostProgram,
    deadline: Instant,
    interrupts: &Interrupts,
    baseline: usize,
) -> Result<SpawnedHost, SpawnError> {
    // Routing clients wait for a `starting` host instead of reading a stale record.
    let since = Utc::now().trunc_subsecs(0);
    lockfile::write(&lock, &LockRecord::starting(HostKind::Headless, socket, since))?;
    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let (log_path, log) = create_log(paths, &stamp, std::process::id(), "spawn")?;
    let io_err = |what: &str, e: io::Error| CoreError::io(format!("{what}: {e}"));
    let (ready_read, ready_write) =
        pipe_cloexec().map_err(|e| io_err("can't create the ready pipe", e))?;
    let lock_high =
        dup_high(lock.as_fd().as_raw_fd()).map_err(|e| io_err("can't pass the lock", e))?;
    let ready_high =
        dup_high(ready_write.as_raw_fd()).map_err(|e| io_err("can't pass the ready pipe", e))?;
    let stdout = log
        .try_clone()
        .map_err(|e| io_err("can't open the host log", e))?;

    let mut command = Command::new(&program.program);
    command
        .args(&program.args)
        .arg("--project")
        .arg(paths.root())
        .arg("--lock-fd")
        .arg(CHILD_LOCK_FD.to_string())
        .arg("--ready-fd")
        .arg(CHILD_READY_FD.to_string())
        .envs(program.env.iter().map(|(k, v)| (k, v)))
        .current_dir(paths.root())
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(log);
    let (lock_fd, ready_fd) = (lock_high.as_raw_fd(), ready_high.as_raw_fd());
    // SAFETY: the closure runs between fork and exec and only makes
    // async-signal-safe calls (dup2, setsid) on fds that are open in the child.
    unsafe {
        command.pre_exec(move || {
            // dup2 clears FD_CLOEXEC on the new descriptor.
            if libc::dup2(lock_fd, CHILD_LOCK_FD) < 0 || libc::dup2(ready_fd, CHILD_READY_FD) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let spawned = command.spawn();
    // The CLI keeps none of the lock's fds and never unlocks: the child's
    // copy of the open file description holds the flock now.
    drop((lock_high, ready_high, ready_write, lock));
    let mut child = spawned.map_err(|e| {
        CoreError::io(format!(
            "can't start the background host {}: {e}",
            program.program.display()
        ))
    })?;
    let pid = child.id();
    let log_path = rename_log(paths, &stamp, pid, log_path);
    let outcome = wait_ready(&ready_read, deadline, interrupts, baseline);
    let see_log = || format!("see {}", log_path.display());
    match outcome {
        Ready::Ok => {
            reap_later(child);
            Ok(SpawnedHost { pid, log: log_path })
        }
        Ready::Failed(err) => {
            reap_later(child);
            let hint = match err.hint.as_deref() {
                Some(hint) => format!("{hint}; {}", see_log()),
                None => see_log(),
            };
            Err(SpawnError::Failed(err.with_hint(hint)))
        }
        Ready::Eof => {
            let status = exit_status(&mut child);
            reap_later(child);
            Err(SpawnError::Failed(
                CoreError::io(format!(
                    "the background host exited before it was ready{status}"
                ))
                .with_hint(see_log()),
            ))
        }
        Ready::Timeout => {
            terminate(&child);
            reap_later(child);
            Err(SpawnError::Failed(
                CoreError::busy("the background host didn't become ready in time")
                    .with_hint(see_log()),
            ))
        }
        Ready::Interrupted => {
            terminate(&child);
            reap_later(child);
            Err(SpawnError::Interrupted)
        }
    }
}

/// `.qrec/logs/<stamp>-<pid>-<step>.log`, created exclusively (with a
/// `-2`, `-3`… suffix if the name is taken).
fn create_log(
    paths: &ProjectPaths,
    stamp: &str,
    pid: u32,
    step: &str,
) -> Result<(PathBuf, File), CoreError> {
    let dir = paths.logs_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| CoreError::io(format!("can't create {}: {e}", dir.display())))?;
    let mut last = None;
    for attempt in 1..=20 {
        let name = if attempt == 1 {
            step.to_owned()
        } else {
            format!("{step}-{attempt}")
        };
        let path = paths.log_file(stamp, pid, &name);
        match OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o644)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => {
                return Err(CoreError::io(format!(
                    "can't create {}: {e}",
                    path.display()
                )));
            }
        }
    }
    Err(CoreError::io(format!(
        "can't create a host log in {}: {}",
        dir.display(),
        last.map_or_else(String::new, |e| e.to_string())
    )))
}

/// Names the log after the host's pid (`…-<pid>-host.log`), so a client
/// that only knows the pid finds it. Keeps the old name if that fails.
fn rename_log(paths: &ProjectPaths, stamp: &str, pid: u32, current: PathBuf) -> PathBuf {
    let target = paths.log_file(stamp, pid, "host");
    if fs::symlink_metadata(&target).is_err() && fs::rename(&current, &target).is_ok() {
        target
    } else {
        current
    }
}

/// `fd` duplicated at or above [`HIGH_FD`], with `FD_CLOEXEC`.
fn dup_high(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC on an fd we own returns a new fd or -1.
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, HIGH_FD) };
    if dup < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh fd we own.
    Ok(unsafe { OwnedFd::from_raw_fd(dup) })
}

enum Ready {
    Ok,
    Failed(CoreError),
    Eof,
    Timeout,
    Interrupted,
}

/// Reads the host's one ready line.
fn wait_ready(read: &OwnedFd, deadline: Instant, interrupts: &Interrupts, baseline: usize) -> Ready {
    let mut line = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if interrupts.count() > baseline {
            return Ready::Interrupted;
        }
        let now = Instant::now();
        if now >= deadline {
            return Ready::Timeout;
        }
        let slice = READY_SLICE.min(deadline - now);
        let mut pfd = libc::pollfd {
            fd: read.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let rc = unsafe { libc::poll(&mut pfd, 1, slice.as_millis().max(1) as libc::c_int) };
        if rc < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Ready::Eof;
        }
        if rc == 0 {
            continue;
        }
        // SAFETY: reading into a valid local buffer from our pipe.
        let n = unsafe { libc::read(read.as_raw_fd(), chunk.as_mut_ptr().cast(), chunk.len()) };
        if n < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Ready::Eof;
        }
        if n == 0 {
            return if line.is_empty() {
                Ready::Eof
            } else {
                parse_ready(&line)
            };
        }
        line.extend_from_slice(&chunk[..n as usize]);
        if let Some(end) = line.iter().position(|&b| b == b'\n') {
            return parse_ready(&line[..end]);
        }
        if line.len() > MAX_READY_LINE {
            return Ready::Failed(CoreError::io("the background host's ready line is too long"));
        }
    }
}

/// `{"ok":true}`, `{"ok":false,"error":…}` or a bare `CoreError`.
fn parse_ready(line: &[u8]) -> Ready {
    let Ok(value) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
        return Ready::Failed(CoreError::io(format!(
            "the background host said: {}",
            String::from_utf8_lossy(line).trim()
        )));
    };
    match value.get("ok").and_then(Value::as_bool) {
        Some(true) => Ready::Ok,
        Some(false) => match value.get("error").cloned().map(serde_json::from_value) {
            Some(Ok(err)) => Ready::Failed(err),
            _ => Ready::Failed(CoreError::io("the background host failed to start")),
        },
        None => match serde_json::from_value::<CoreError>(value) {
            Ok(err) => Ready::Failed(err),
            Err(_) => Ready::Failed(CoreError::io("the background host sent a bad ready line")),
        },
    }
}

/// ", exit status 1" when the child already exited.
fn exit_status(child: &mut Child) -> String {
    // The pipe closed: give a dying child a moment to be reapable.
    for _ in 0..20 {
        match child.try_wait() {
            Ok(Some(status)) => return format!(" ({status})"),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }
    String::new()
}

/// SIGTERM to a host that isn't ready: with nothing to do it exits at once.
/// The child isn't reaped yet, so its pid can't have been reused.
fn terminate(child: &Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill(2) on our own unreaped child.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

/// The CLI may outlive the host (`record … --wait`): reap it on a thread
/// so it never lingers as a zombie. The thread dies with the CLI.
/// Without a thread the host is reaped when the CLI exits (init adopts it).
fn reap_later(mut child: Child) {
    let _ = thread::Builder::new()
        .name("qrec-host-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_lines() {
        assert!(matches!(parse_ready(b"{\"ok\":true}"), Ready::Ok));
        match parse_ready(br#"{"ok":false,"error":{"code":"busy","message":"x"}}"#) {
            Ready::Failed(e) => assert_eq!(e.code, qrec_core::ErrorCode::Busy),
            _ => panic!(),
        }
        match parse_ready(br#"{"code":"not_project","message":"swamp"}"#) {
            Ready::Failed(e) => assert_eq!(e.message, "swamp"),
            _ => panic!(),
        }
        assert!(matches!(parse_ready(b"panicked at foo"), Ready::Failed(_)));
    }

    #[test]
    fn logs_are_created_exclusively_and_renamed_by_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::new(tmp.path());
        let (first, _) = create_log(&paths, "20261014-120000", 7, "spawn").unwrap();
        let (second, _) = create_log(&paths, "20261014-120000", 7, "spawn").unwrap();
        assert_ne!(first, second);
        assert!(second.to_string_lossy().ends_with("-7-spawn-2.log"));
        let renamed = rename_log(&paths, "20261014-120000", 99, first);
        assert_eq!(renamed, paths.log_file("20261014-120000", 99, "host"));
        assert!(renamed.is_file());
        // The name is taken: keep the old one.
        let kept = rename_log(&paths, "20261014-120000", 99, second.clone());
        assert_eq!(kept, second);
    }

    #[test]
    fn high_dups_are_cloexec_and_above_ten() {
        let file = tempfile::tempfile().unwrap();
        let dup = dup_high(file.as_raw_fd()).unwrap();
        assert!(dup.as_raw_fd() >= HIGH_FD);
        // SAFETY: fcntl query on our fd.
        let flags = unsafe { libc::fcntl(dup.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }
}
