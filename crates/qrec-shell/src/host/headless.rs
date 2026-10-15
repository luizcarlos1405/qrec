//! `qrec host --headless --project DIR [--lock-fd 3] [--ready-fd 4]`
//! (A§8.3 step 5, A§8.6). Owner: ipc.
//!
//! The recorder `record start` leaves behind: it adopts the lock its parent
//! handed down (or takes it itself when started by hand), publishes the
//! socket, reports `{"ok":true}` on the ready fd, then opens the Session and
//! serves until the exit rules say stop: idle (no recording, no jobs, no
//! connection) for 2 s after its first request, 60 s after `ready` without
//! one, `shutdown` once jobs finish, a signal once idle or after 15 s.
//! Its stdout/stderr are the host log; it writes one line per lifecycle step.

use std::fs::{self, File};
use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;

use chrono::Local;
use qrec_core::CoreError;
use qrec_core::state::HostKind;
use serde_json::json;

use super::signals::{self, HOST_SIGNALS};
use super::{Host, HostEnd, HostOptions, Opener};
use crate::deps::OpenOpts;
use crate::ipc::framing::encode_line;
use crate::ipc::resolve::{Folder, classify};
use crate::lock::{ProjectLock, set_cloexec};
use crate::paths::{Env, ProjectPaths, ensure_runtime_dir, socket_path};

/// The arguments of `qrec host --headless`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessArgs {
    pub project: PathBuf,
    /// An inherited fd that holds the project lock.
    pub lock_fd: Option<RawFd>,
    /// An inherited fd to report readiness on: one JSON line, then close.
    pub ready_fd: Option<RawFd>,
}

/// `qrec host --headless` as a process: the process environment, handlers
/// for SIGINT/SIGTERM/SIGHUP and the A§8.6 timings. Returns the exit code.
pub fn main_headless(args: HeadlessArgs, opener: &mut dyn Opener) -> u8 {
    let interrupts = match signals::install(&HOST_SIGNALS) {
        Ok(interrupts) => Some(interrupts),
        Err(e) => {
            log(&format!("signals are not handled: {e}"));
            None
        }
    };
    let opts = HostOptions {
        interrupts,
        ..HostOptions::default()
    };
    run_headless(args, &Env::from_process(), opener, opts)
}

/// Runs the headless host until its exit rules say stop. Exit code: 0 after
/// idling out or `shutdown`, 130 after a signal, 1 if it couldn't start.
pub fn run_headless(
    args: HeadlessArgs,
    env: &Env,
    opener: &mut dyn Opener,
    opts: HostOptions,
) -> u8 {
    let mut ready = match args.ready_fd.map(ReadyPipe::adopt).transpose() {
        Ok(ready) => ready,
        Err(e) => {
            log(&format!("can't use --ready-fd: {e}"));
            return 1;
        }
    };
    let host = match start(&args, env, opts) {
        Ok(host) => host,
        Err(e) => {
            log(&format!("can't start: {e}"));
            if let Some(mut ready) = ready.take() {
                ready.send(Err(&e));
            }
            return 1;
        }
    };
    log(&format!("serving {} (ready)", host.paths.root().display()));
    if let Some(ready) = ready.as_mut() {
        ready.send(Ok(()));
    }
    let open = OpenOpts {
        host: HostKind::Headless,
        mutating: true,
        recovery: false,
    };
    let opened = host
        .session_lock()
        .map_err(Into::into)
        .and_then(|lock| opener.open(host.paths.root(), open, Some(lock)));
    let mut session = match opened {
        Ok(session) => session,
        Err(e) => {
            let e = e.to_core();
            log(&format!("can't open the project: {e}"));
            // The spawner's request may come after this host is gone: the
            // cause goes down the ready pipe too, before the lock is freed.
            if let Some(mut ready) = ready.take() {
                ready.send(Err(&e));
            }
            host.server.fail(e.clone());
            host.finish(None, e);
            return 1;
        }
    };
    // Opened: the spawner reads nothing more (EOF).
    drop(ready);
    host.attach(session.inbox());
    let end = host.serve(&mut *session, None);
    let signalled = host.signalled() || end.signalled();
    log(&format!("exiting ({end:?})"));
    host.finish(Some(session), CoreError::io("the headless host is exiting"));
    if signalled {
        130
    } else if end == HostEnd::SessionStopped {
        1
    } else {
        0
    }
}

/// Lock, folder check, socket, server.
fn start(args: &HeadlessArgs, env: &Env, opts: HostOptions) -> Result<Host, CoreError> {
    let dir = fs::canonicalize(&args.project)
        .map_err(|e| CoreError::io(format!("can't open {}: {e}", args.project.display())))?;
    let paths = ProjectPaths::new(&dir);
    let lock = match args.lock_fd {
        Some(fd) => ProjectLock::adopt(&dir, inherited_fd(fd, "--lock-fd")?)?,
        None => ProjectLock::try_acquire(&dir)?.ok_or_else(|| {
            CoreError::busy(format!("another Qrec host holds {}", dir.display()))
                .with_hint("send it commands instead; they are routed automatically")
        })?,
    };
    match classify(&dir, true) {
        // A project that lost its qrec.json opens to say how to restore it.
        Ok(Folder::Project | Folder::MissingFile | Folder::Empty) => {}
        Ok(Folder::Other) => return Err(CoreError::not_project()),
        Err(e) => return Err(CoreError::io(format!("can't read {}: {e}", dir.display()))),
    }
    let runtime = ensure_runtime_dir(env)?;
    let socket = socket_path(&runtime, &dir)?;
    Host::start(HostKind::Headless, paths, lock, &socket, opts)
}

/// Takes ownership of a descriptor our parent passed down: it must be above
/// the standard streams and open (checked before owning it, so a bad number
/// is an error, not a double close). `FD_CLOEXEC` keeps it from our children.
fn inherited_fd(fd: RawFd, flag: &str) -> Result<OwnedFd, CoreError> {
    if fd <= 2 {
        return Err(CoreError::usage(format!(
            "{flag} {fd}: expected an inherited descriptor above 2"
        )));
    }
    set_cloexec(fd).map_err(|e| CoreError::io(format!("{flag} {fd}: {e}")))?;
    // SAFETY: the fd is open (fcntl succeeded) and nothing else in this
    // process owns it: it was inherited for us.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The write end of the ready pipe (A§8.3 step 5): `{"ok":true}` once
/// listening (or the start error), then, if opening the Session fails, that
/// error as a second line. Closed when dropped.
struct ReadyPipe(File);

impl ReadyPipe {
    fn adopt(fd: RawFd) -> Result<ReadyPipe, CoreError> {
        inherited_fd(fd, "--ready-fd").map(|fd| ReadyPipe(File::from(fd)))
    }

    /// One line. A parent that already gave up is ignored.
    fn send(&mut self, result: Result<(), &CoreError>) {
        let line = match result {
            Ok(()) => json!({ "ok": true }),
            Err(err) => json!({ "ok": false, "error": err }),
        };
        let _ = self.0.write_all(&encode_line(&line));
    }
}

/// One lifecycle line on stderr (the host log).
fn log(message: &str) {
    eprintln!(
        "{} qrec host[{}]: {message}",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        std::process::id()
    );
}
