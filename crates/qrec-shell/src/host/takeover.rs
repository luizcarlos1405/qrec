//! Takeover when the GUI starts (docs/GUI.md G§3.4.3, A§8.6). Owner: ipc.
//!
//! The window wants the project lock; another process may hold it. These
//! are the steps the window's pages drive (WP0b; WP8 restyles them):
//!
//! - [`find_holder`]: take the lock if it is free, else ask the holder
//!   `hello` (2 s): a live host and what it does (its kind, a recording,
//!   its jobs), one that is starting or stopping (ask again in 50 ms; after
//!   3 s it isn't responding), or one that doesn't answer.
//! - A recording (X1, "Stop & open"): [`stop_recording`] (`RecordStop`,
//!   its reply once the take is finalized, ≤ 60 s), then
//!   [`request_shutdown`], then [`wait_for_lock`] (≤ 20 s). "Quit" leaves
//!   the recording alone.
//! - An export (D-29): `hello` every 500 ms until no job runs (the percent
//!   from `export_progress_view` over `hello.jobs`), then
//!   [`request_shutdown`] (a headless host; a CLI command's host exits by
//!   itself) and [`wait_for_lock`].
//! - Idle: [`request_shutdown`], [`wait_for_lock`].
//! - A GUI host: its window holds the project (D-13); it refuses
//!   `shutdown` with `busy`.
//!
//! Every wait has a deadline; each request uses a connection of its own.

use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use qrec_core::CoreError;
use qrec_core::protocol::{
    Command, HelloInfo, WIRE_VERSION, WireBody, WireMessage, WireRequest, from_wire,
};
use serde_json::Value;

use super::lockfile::{self, HostState, LockRecord};
use crate::ipc::client::{self, ConnectError, HelloError, lock_held_too_long, not_responding};
use crate::ipc::framing::{LineReader, ReadError, encode_line, is_disconnect, write_all_before};
use crate::ipc::{HELLO_TIMEOUT, MAX_REPLY_LINE, ROUTE_RETRY};
use crate::lock::ProjectLock;
use crate::paths::{Env, ensure_runtime_dir, socket_path};

/// How long the window waits for a recording's `RecordStop` reply (X1).
pub const STOP_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the window waits for the lock after `shutdown` (A§8.6).
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a starting or stopping holder may take before the window calls
/// it not responding (G§3.4.3).
pub const STARTING_TIMEOUT: Duration = Duration::from_secs(3);
/// How often the window asks a holder that is starting or stopping again.
pub const RETRY: Duration = ROUTE_RETRY;

/// Who holds the project when the window starts.
// Made a few times while the window starts and matched at once: boxing the
// `HelloInfo` would only change G§3.4.3's signature.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Holder {
    /// Nobody did: the lock is ours now (`GuiHost::start` takes it).
    Free(ProjectLock),
    /// A live host answered `hello` on `socket`: its kind, whether it
    /// records and its jobs.
    Host { hello: HelloInfo, socket: PathBuf },
    /// The lock is held but no host answers yet: it is starting or stopping
    /// (or exiting after its Session failed). Ask again every 50 ms; after
    /// 3 s it is not responding ([`unresponsive`]).
    Starting,
    /// The holder's socket took the connection but `hello` got no answer
    /// within 2 s. The lock record names it when that process still runs.
    Unresponsive(Option<LockRecord>),
}

/// Takes the lock of the project in `dir` (canonical; the caller applied
/// the folder rule) if it is free, else finds out who holds it: `hello` on
/// this runtime's socket, then on the one the lock record names (a host
/// started with another runtime dir).
pub fn find_holder(dir: &Path, env: &Env) -> Result<Holder, CoreError> {
    if let Some(lock) = ProjectLock::try_acquire(dir)? {
        return Ok(Holder::Free(lock));
    }
    let runtime = ensure_runtime_dir(env)?;
    let ours = socket_path(&runtime, dir)?;
    let record = lockfile::read(dir).ok().flatten();
    let mut sockets = vec![ours];
    if let Some(theirs) = record
        .as_ref()
        .filter(|r| r.state == HostState::Ready)
        .map(|r| PathBuf::from(&r.socket))
        .filter(|theirs| *theirs != sockets[0])
    {
        sockets.push(theirs);
    }
    for socket in sockets {
        match client::hello(&socket, HELLO_TIMEOUT) {
            Ok(hello) => return Ok(Holder::Host { hello, socket }),
            Err(HelloError::Timeout) => {
                return Ok(Holder::Unresponsive(record.filter(LockRecord::is_live)));
            }
            // Alive, but its Session couldn't open: it is going away.
            Err(HelloError::Refused(_)) => return Ok(Holder::Starting),
            Err(
                HelloError::NoHost
                | HelloError::Disconnected
                | HelloError::Protocol(_)
                | HelloError::Io(_),
            ) => {}
        }
    }
    Ok(Holder::Starting)
}

/// The holder that stayed [`Holder::Starting`] for [`STARTING_TIMEOUT`]:
/// what the lock record says about it (a pid only while that process still
/// runs), for the "isn't responding" page.
pub fn unresponsive(dir: &Path) -> Holder {
    Holder::Unresponsive(lockfile::read_live(dir).ok().flatten())
}

/// The X1 "Stop & open": `RecordStop` to the host on `socket` and its
/// reply, which comes once the take is finalized (the `CmdResult` whose
/// `result` is a `TakeFinalizedView`). The host's own errors come back as
/// they are (`invalid` "not recording" when the take ended meanwhile);
/// no reply within `timeout` is `busy`.
pub fn stop_recording(socket: &Path, timeout: Duration) -> Result<Value, CoreError> {
    let body = WireBody::Command(Command::RecordStop);
    match exchange(socket, &body, Instant::now() + timeout) {
        Ok(reply) => reply,
        Err(Failure::Timeout) => Err(CoreError::busy(format!(
            "the recording didn't stop within {} s",
            timeout.as_secs_f64()
        ))
        .with_hint("the recorder may be stuck; Qrec recovers its take once that process is gone")),
        Err(failure) => Err(failure.into_error("stopping the recording")),
    }
}

/// `shutdown` to the host on `socket` (A§8.6): it stops accepting,
/// finalizes a recording, lets its jobs finish and exits. A host that is
/// already gone (or goes as we ask) is fine: [`wait_for_lock`] tells. A GUI
/// host refuses (`busy`, D-13); no answer within 2 s is `busy`.
pub fn request_shutdown(socket: &Path) -> Result<(), CoreError> {
    match exchange(socket, &WireBody::Shutdown, Instant::now() + HELLO_TIMEOUT) {
        Ok(Ok(_)) | Err(Failure::NoHost | Failure::Gone) => Ok(()),
        Ok(Err(err)) => Err(err),
        Err(Failure::Timeout) => Err(CoreError::busy(
            "the project's host didn't answer the request to shut down",
        )
        .with_hint("it may be stuck; find it with `fuser .qrec/lock`")),
        Err(failure) => Err(failure.into_error("asking the host to shut down")),
    }
}

/// Takes the lock of the project in `dir` as soon as it is free (tries
/// every 50 ms) and within `timeout`; past it, `busy` naming the holder
/// while it still runs.
pub fn wait_for_lock(dir: &Path, timeout: Duration) -> Result<ProjectLock, CoreError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(lock) = ProjectLock::try_acquire(dir)? {
            return Ok(lock);
        }
        let now = Instant::now();
        if now >= deadline {
            let record = lockfile::read_live(dir).ok().flatten();
            return Err(lock_held_too_long(record.as_ref()));
        }
        thread::sleep(RETRY.min(deadline - now));
    }
}

/// Why [`exchange`] got no reply.
#[derive(Debug)]
enum Failure {
    /// Nothing listens there.
    NoHost,
    /// The connection closed before the reply.
    Gone,
    /// No reply before the deadline.
    Timeout,
    /// Something that isn't a Qrec host answered, or the socket failed.
    Other(String),
}

impl Failure {
    fn into_error(self, doing: &str) -> CoreError {
        match self {
            Failure::NoHost | Failure::Gone => {
                CoreError::io(format!("{doing}: the host went away before it answered"))
                    .with_hint("check with `qrec status`")
            }
            Failure::Timeout => not_responding(None),
            Failure::Other(why) => CoreError::io(format!("{doing}: {why}")),
        }
    }
}

/// One request on its own connection, until its reply or `deadline`.
fn exchange(
    socket: &Path,
    body: &WireBody,
    deadline: Instant,
) -> Result<Result<Value, CoreError>, Failure> {
    let stream = match client::connect(socket) {
        Ok(stream) => stream,
        Err(ConnectError::NoHost) => return Err(Failure::NoHost),
        Err(ConnectError::Backlog) => return Err(Failure::Timeout),
        Err(ConnectError::Io(e)) => return Err(Failure::Other(e.to_string())),
    };
    let line = encode_line(&WireRequest {
        v: WIRE_VERSION,
        id: "qrec-gui-takeover".to_owned(),
        confirmed: false,
        body: body.clone(),
    });
    if let Err(e) = write_all_before(&stream, &line, deadline) {
        return Err(match e.kind() {
            io::ErrorKind::TimedOut => Failure::Timeout,
            _ if is_disconnect(&e) => Failure::Gone,
            _ => Failure::Other(e.to_string()),
        });
    }
    let mut reader = LineReader::new(stream);
    loop {
        let line = match reader.read_line(MAX_REPLY_LINE, deadline) {
            Ok(line) => line,
            Err(ReadError::Timeout) => return Err(Failure::Timeout),
            Err(ReadError::Eof | ReadError::Truncated) => return Err(Failure::Gone),
            Err(ReadError::Io(e)) if is_disconnect(&e) => return Err(Failure::Gone),
            Err(e) => return Err(Failure::Other(e.to_string())),
        };
        let msg: WireMessage = serde_json::from_slice(&line)
            .map_err(|e| Failure::Other(format!("unexpected line from the host: {e}")))?;
        if let WireMessage::Reply { reply, .. } = msg {
            return from_wire(&reply).map_err(Failure::Other);
        }
    }
}

#[cfg(test)]
#[path = "takeover_tests.rs"]
mod tests;
