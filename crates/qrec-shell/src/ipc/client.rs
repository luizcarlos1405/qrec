//! The CLI side of the project socket (A§8.3 steps 4 and 6).
//!
//! - [`hello`]: one `hello` on its own connection, answered within 2 s by a
//!   live host (a host that doesn't is "not responding").
//! - [`route`]: the routing loop. Connect; on ENOENT/ECONNREFUSED try the
//!   lock (mutating commands) and become the host if it's free; otherwise
//!   read the lock record and retry every 50 ms until the holder is ready or
//!   the deadline (3 s, 20 s for `record *`) passes.
//! - [`send_request`]: one request on its own connection, streaming
//!   `accepted`, progress and the reply to [`Replies`]. EOF before
//!   `accepted` is [`SendError::NotAccepted`] (safe to route again, once);
//!   EOF after it is [`SendError::Disconnected`] (never retried: it may have
//!   run). Ctrl+C closes the connection, except for `record … --wait`, where
//!   the first one sends `RecordStop` on a second connection and keeps
//!   streaming and the second detaches. While a long request is silent the
//!   client checks the host with `hello` every 30 s.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use qrec_core::CoreError;
use qrec_core::protocol::{
    Command, HelloInfo, Progress, WIRE_VERSION, WireBody, WireMessage, WireRequest, from_wire,
};
use serde_json::Value;

use super::framing::{LineReader, ReadError, encode_line, is_disconnect, write_all_before};
use super::lock_glue::ProjectLock;
use super::lock_glue::lockfile::{self, HostState, LockRecord};
use super::{
    ACCEPT_TIMEOUT, HEARTBEAT_AFTER, HELLO_TIMEOUT, MAX_REPLY_LINE, ROUTE_RETRY, WRITE_TIMEOUT,
    peer,
};
use crate::host::signals::Interrupts;
use crate::msg::Request;
use crate::paths::ProjectPaths;

/// How often the streaming and routing loops look at [`Interrupts`].
const POLL_SLICE: Duration = Duration::from_millis(100);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A client request id, unique in this process.
fn next_id() -> String {
    NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string()
}

// ------------------------------------------------------------------ connect

/// Why [`connect`] failed.
#[derive(Debug)]
pub enum ConnectError {
    /// Nothing listens at the path (ENOENT, ECONNREFUSED, ENOTDIR).
    NoHost,
    /// The listener's backlog is full: whoever listens isn't accepting.
    Backlog,
    Io(io::Error),
}

/// Connects without blocking on a stuck listener.
pub fn connect(socket: &Path) -> Result<UnixStream, ConnectError> {
    peer::connect_nonblocking(socket).map_err(|e| match e.raw_os_error() {
        Some(libc::ENOENT | libc::ECONNREFUSED | libc::ENOTDIR) => ConnectError::NoHost,
        Some(libc::EAGAIN) => ConnectError::Backlog,
        _ => ConnectError::Io(e),
    })
}

/// One request on one connection.
struct Exchange {
    reader: LineReader,
    id: String,
}

enum OpenError {
    Connect(ConnectError),
    Write(io::Error),
}

enum NextError {
    Read(ReadError),
    Protocol(String),
}

impl Exchange {
    fn open(
        socket: &Path,
        body: &WireBody,
        confirmed: bool,
        write_deadline: Instant,
    ) -> Result<Exchange, OpenError> {
        let stream = connect(socket).map_err(OpenError::Connect)?;
        let id = next_id();
        let line = encode_line(&WireRequest {
            v: WIRE_VERSION,
            id: id.clone(),
            confirmed,
            body: body.clone(),
        });
        write_all_before(&stream, &line, write_deadline).map_err(OpenError::Write)?;
        Ok(Exchange {
            reader: LineReader::new(stream),
            id,
        })
    }

    fn next(&mut self, deadline: Instant) -> Result<WireMessage, NextError> {
        let line = self
            .reader
            .read_line(MAX_REPLY_LINE, deadline)
            .map_err(NextError::Read)?;
        let msg: WireMessage = serde_json::from_slice(&line)
            .map_err(|e| NextError::Protocol(format!("unexpected line from the host: {e}")))?;
        // The host answers a request it couldn't parse with id "".
        if msg.id() != self.id && !msg.id().is_empty() {
            return Err(NextError::Protocol(format!(
                "the host answered request {} on request {}'s connection",
                msg.id(),
                self.id
            )));
        }
        Ok(msg)
    }
}

// -------------------------------------------------------------------- hello

/// Why [`hello`] got no `HelloInfo`.
#[derive(Debug)]
pub enum HelloError {
    /// Nothing listens there: no host.
    NoHost,
    /// Connected, but no answer in time (or the backlog is full).
    Timeout,
    /// The host closed the connection without answering (it may be exiting).
    Disconnected,
    /// The host is alive but answered with an error.
    Refused(CoreError),
    /// Something that isn't a Qrec host answered.
    Protocol(String),
    Io(io::Error),
}

/// Sends `hello` and waits up to `timeout` for the host's [`HelloInfo`].
pub fn hello(socket: &Path, timeout: Duration) -> Result<HelloInfo, HelloError> {
    let deadline = Instant::now() + timeout;
    let mut exchange = match Exchange::open(socket, &WireBody::Hello, false, deadline) {
        Ok(exchange) => exchange,
        Err(OpenError::Connect(ConnectError::NoHost)) => return Err(HelloError::NoHost),
        Err(OpenError::Connect(ConnectError::Backlog)) => return Err(HelloError::Timeout),
        Err(OpenError::Connect(ConnectError::Io(e))) => return Err(HelloError::Io(e)),
        Err(OpenError::Write(e)) if is_disconnect(&e) => return Err(HelloError::Disconnected),
        Err(OpenError::Write(e)) if e.kind() == io::ErrorKind::TimedOut => {
            return Err(HelloError::Timeout);
        }
        Err(OpenError::Write(e)) => return Err(HelloError::Io(e)),
    };
    loop {
        match exchange.next(deadline) {
            Ok(WireMessage::Accepted { .. } | WireMessage::Progress { .. }) => {}
            Ok(WireMessage::Reply { reply, .. }) => {
                return match from_wire(&reply) {
                    Ok(Ok(data)) => serde_json::from_value(data)
                        .map_err(|e| HelloError::Protocol(format!("bad hello reply: {e}"))),
                    Ok(Err(err)) => Err(HelloError::Refused(err)),
                    Err(msg) => Err(HelloError::Protocol(msg)),
                };
            }
            Err(NextError::Read(ReadError::Timeout)) => return Err(HelloError::Timeout),
            Err(NextError::Read(ReadError::Eof | ReadError::Truncated)) => {
                return Err(HelloError::Disconnected);
            }
            Err(NextError::Read(ReadError::TooLong)) => {
                return Err(HelloError::Protocol("the hello reply is too long".into()));
            }
            Err(NextError::Read(ReadError::Io(e))) if is_disconnect(&e) => {
                return Err(HelloError::Disconnected);
            }
            Err(NextError::Read(ReadError::Io(e))) => return Err(HelloError::Io(e)),
            Err(NextError::Protocol(msg)) => return Err(HelloError::Protocol(msg)),
        }
    }
}

// ------------------------------------------------------------------ request

/// Where a request's messages go on the client side. Unlike
/// [`Responder`](crate::msg::Responder) it needn't be `Send`: it never
/// leaves the calling thread.
pub trait Replies {
    /// The host queued the request.
    fn accepted(&mut self);
    fn progress(&mut self, p: &Progress);
    /// The one reply.
    fn reply(&mut self, r: Result<Value, CoreError>);
}

/// How [`send_request`] behaves.
#[derive(Debug, Clone)]
pub struct SendOpts {
    pub interrupts: Interrupts,
    /// Interrupts counted before this request; only later ones act.
    pub baseline: usize,
    /// `record start|toggle --wait`: the first interrupt sends `RecordStop`
    /// on a second connection and keeps streaming; the second detaches.
    pub stop_on_interrupt: bool,
    /// `accepted` must arrive within this time.
    pub accept_timeout: Duration,
    /// After this long without a message, check the host with `hello`.
    pub heartbeat_after: Duration,
}

impl SendOpts {
    /// The A§8.3 timeouts; interrupts from now on count.
    pub fn new(interrupts: &Interrupts) -> Self {
        Self {
            interrupts: interrupts.clone(),
            baseline: interrupts.count(),
            stop_on_interrupt: false,
            accept_timeout: ACCEPT_TIMEOUT,
            heartbeat_after: HEARTBEAT_AFTER,
        }
    }
}

/// How a request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// The responder got the reply.
    Replied,
    /// Ctrl+C: the connection was closed before the reply (exit 130).
    Interrupted,
}

/// Why a request got no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// Nothing was accepted (no host at the socket, or EOF before
    /// `accepted`): routing again is safe.
    NotAccepted,
    /// No `accepted` in time. The request may still run.
    NotResponding,
    /// The connection broke after `accepted`. The request may have run.
    Disconnected,
    /// Accepted, then the host stopped answering `hello`. It may still finish.
    LostContact,
    /// The peer doesn't speak the protocol.
    Protocol(String),
}

/// Sends `req` on a new connection and streams what comes back to `out`.
pub fn send_request(
    socket: &Path,
    req: &Request,
    out: &mut dyn Replies,
    opts: &SendOpts,
) -> Result<Sent, SendError> {
    let mut exchange = match Exchange::open(
        socket,
        &req.body,
        req.confirmed,
        Instant::now() + WRITE_TIMEOUT,
    ) {
        Ok(exchange) => exchange,
        Err(OpenError::Connect(ConnectError::Backlog)) => return Err(SendError::NotResponding),
        Err(OpenError::Connect(_)) => return Err(SendError::NotAccepted),
        Err(OpenError::Write(e)) if e.kind() == io::ErrorKind::TimedOut => {
            return Err(SendError::NotResponding);
        }
        Err(OpenError::Write(_)) => return Err(SendError::NotAccepted),
    };
    let accept_deadline = Instant::now() + opts.accept_timeout;
    let mut accepted = false;
    let mut last_message = Instant::now();
    let mut seen = opts.baseline;
    let mut stop_sent = false;
    loop {
        let count = opts.interrupts.count();
        if count > seen {
            seen = count;
            if opts.stop_on_interrupt && accepted && !stop_sent {
                stop_sent = true;
                send_record_stop(socket);
            } else {
                // Dropping the exchange closes the connection.
                return Ok(Sent::Interrupted);
            }
        }
        match exchange.next(Instant::now() + POLL_SLICE) {
            Ok(msg) => {
                last_message = Instant::now();
                match msg {
                    WireMessage::Accepted { .. } => {
                        if !accepted {
                            accepted = true;
                            out.accepted();
                        }
                    }
                    WireMessage::Progress { progress, .. } => {
                        accepted = true;
                        out.progress(&progress);
                    }
                    WireMessage::Reply { reply, .. } => {
                        return match from_wire(&reply) {
                            Ok(result) => {
                                out.reply(result);
                                Ok(Sent::Replied)
                            }
                            Err(msg) => Err(SendError::Protocol(msg)),
                        };
                    }
                }
            }
            Err(NextError::Read(ReadError::Timeout)) => {
                let now = Instant::now();
                if !accepted && now >= accept_deadline {
                    return Err(SendError::NotResponding);
                }
                if accepted && now.duration_since(last_message) >= opts.heartbeat_after {
                    if let Err(HelloError::Timeout) = hello(socket, HELLO_TIMEOUT) {
                        return Err(SendError::LostContact);
                    }
                    last_message = Instant::now();
                }
            }
            Err(NextError::Read(ReadError::Eof | ReadError::Truncated)) => {
                return Err(if accepted {
                    SendError::Disconnected
                } else {
                    SendError::NotAccepted
                });
            }
            Err(NextError::Read(ReadError::Io(e))) if is_disconnect(&e) => {
                return Err(if accepted {
                    SendError::Disconnected
                } else {
                    SendError::NotAccepted
                });
            }
            Err(NextError::Read(ReadError::Io(e))) => {
                return Err(if accepted {
                    SendError::Disconnected
                } else {
                    SendError::Protocol(format!("reading from the host: {e}"))
                });
            }
            Err(NextError::Read(ReadError::TooLong)) => {
                return Err(SendError::Protocol(format!(
                    "a line from the host is longer than {MAX_REPLY_LINE} bytes"
                )));
            }
            Err(NextError::Protocol(msg)) => return Err(SendError::Protocol(msg)),
        }
    }
}

/// `record … --wait` and the first Ctrl+C: `RecordStop` on a second
/// connection, waiting only for `accepted` (≤ 2 s). Best effort: the
/// original connection receives the finalized reply.
fn send_record_stop(socket: &Path) {
    let deadline = Instant::now() + HELLO_TIMEOUT;
    let body = WireBody::Command(Command::RecordStop);
    let Ok(mut exchange) = Exchange::open(socket, &body, false, deadline) else {
        return;
    };
    while let Ok(msg) = exchange.next(deadline) {
        if matches!(
            msg,
            WireMessage::Accepted { .. } | WireMessage::Reply { .. }
        ) {
            return;
        }
    }
}

/// `record start|toggle --wait`: what the first Ctrl+C stops (A§8.6).
pub fn stops_on_interrupt(req: &Request) -> bool {
    matches!(
        req.body,
        WireBody::Command(
            Command::RecordStart { wait: true, .. } | Command::RecordToggle { wait: true }
        )
    )
}

// ------------------------------------------------------------------ routing

/// Whether routing may take the project lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteMode {
    /// Mutating commands: with no host, take the lock and become one.
    Lock,
    /// Everything else: never take the lock (read-only commands run
    /// in-process without it; GUI-only commands need a host).
    NoLock,
}

/// Where a request goes.
#[derive(Debug)]
pub enum Routed {
    /// A live host answered `hello` at `socket` (`hello` is `None` when it
    /// answered with an error: it is alive but its Session didn't say more).
    Host {
        socket: PathBuf,
        hello: Option<Box<HelloInfo>>,
    },
    /// No host: this process holds the lock now (`RouteMode::Lock`).
    Locked(ProjectLock),
    /// No host and nobody holds the lock (`RouteMode::NoLock`).
    NoHost,
}

/// Why routing gave up.
#[derive(Debug)]
pub enum RouteError {
    Interrupted,
    Failed(CoreError),
}

/// What [`route`] needs.
#[derive(Debug, Clone)]
pub struct Router<'a> {
    pub paths: &'a ProjectPaths,
    /// `<runtime>/<hash>.sock` for the canonical project dir.
    pub socket: &'a Path,
    pub mode: RouteMode,
    /// When to give up: [`super::ROUTE_DEADLINE`] or
    /// [`super::RECORD_ROUTE_DEADLINE`] from the start.
    pub deadline: Instant,
    pub interrupts: &'a Interrupts,
    /// Interrupts counted before routing started.
    pub baseline: usize,
    pub hello_timeout: Duration,
    pub retry: Duration,
}

impl<'a> Router<'a> {
    pub fn new(
        paths: &'a ProjectPaths,
        socket: &'a Path,
        mode: RouteMode,
        deadline: Instant,
        interrupts: &'a Interrupts,
    ) -> Self {
        Self {
            paths,
            socket,
            mode,
            deadline,
            interrupts,
            baseline: interrupts.count(),
            hello_timeout: HELLO_TIMEOUT,
            retry: ROUTE_RETRY,
        }
    }
}

/// The routing loop of A§8.3 step 4.
pub fn route(r: &Router<'_>) -> Result<Routed, RouteError> {
    let dir = r.paths.root();
    let mut record: Option<LockRecord> = None;
    loop {
        if r.interrupts.count() > r.baseline {
            return Err(RouteError::Interrupted);
        }
        // Our socket path, then the one the lock record names (a host started
        // with another runtime dir).
        let mut candidates = vec![r.socket.to_path_buf()];
        if let Some(rec) = record
            .as_ref()
            .filter(|rec| rec.state == HostState::Ready && Path::new(&rec.socket) != r.socket)
        {
            candidates.push(PathBuf::from(&rec.socket));
        }
        for socket in candidates {
            match hello(&socket, r.hello_timeout) {
                Ok(info) => {
                    return Ok(Routed::Host {
                        socket,
                        hello: Some(Box::new(info)),
                    });
                }
                Err(HelloError::Refused(_)) => {
                    return Ok(Routed::Host {
                        socket,
                        hello: None,
                    });
                }
                Err(HelloError::Timeout) => {
                    let rec = record
                        .clone()
                        .or_else(|| lockfile::read(dir).ok().flatten());
                    return Err(RouteError::Failed(not_responding(rec.as_ref())));
                }
                Err(
                    HelloError::NoHost
                    | HelloError::Disconnected
                    | HelloError::Protocol(_)
                    | HelloError::Io(_),
                ) => {}
            }
        }
        match r.mode {
            RouteMode::Lock => match ProjectLock::try_acquire(dir) {
                Ok(Some(lock)) => return Ok(Routed::Locked(lock)),
                Ok(None) => {}
                Err(e) => return Err(RouteError::Failed(e.into())),
            },
            RouteMode::NoLock => match ProjectLock::is_held(dir) {
                Ok(false) => return Ok(Routed::NoHost),
                Ok(true) => {}
                Err(e) => return Err(RouteError::Failed(e.into())),
            },
        }
        // Someone holds the lock: wait for it to be ready (or to go away).
        if let Ok(Some(rec)) = lockfile::read(dir) {
            record = Some(rec);
        }
        let now = Instant::now();
        if now >= r.deadline {
            return Err(RouteError::Failed(lock_held_too_long(record.as_ref())));
        }
        let pause = r.retry.min(r.deadline - now);
        if r.interrupts.wait_past(r.baseline, pause) > r.baseline {
            return Err(RouteError::Interrupted);
        }
    }
}

/// `headless host (pid 4242, since 2026-10-14 12:00:03)`.
pub fn describe(record: &LockRecord) -> String {
    format!(
        "{} host (pid {}, since {})",
        record.kind.as_str(),
        record.pid,
        record
            .since
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
    )
}

/// `hello` timed out (A§8.3 step 4).
pub fn not_responding(record: Option<&LockRecord>) -> CoreError {
    match record {
        Some(rec) => CoreError::busy(format!("host pid {} is not responding", rec.pid))
            .with_hint(format!(
                "the project's {} may be stuck; stop it with `kill {}` (a second signal force-quits it)",
                describe(rec),
                rec.pid
            )),
        None => CoreError::busy("the project's host is not responding")
            .with_hint("it may be stuck; find it with `fuser .qrec/lock`"),
    }
}

/// The routing deadline passed while another process held the lock.
pub fn lock_held_too_long(record: Option<&LockRecord>) -> CoreError {
    match record {
        Some(rec) => {
            let what = match rec.state {
                HostState::Starting => "hasn't finished starting",
                HostState::Ready => "isn't accepting connections",
                HostState::Stopping => "is shutting down",
            };
            CoreError::busy(format!("the project's {} {what}", describe(rec))).with_hint(format!(
                "try again in a moment; if it persists, stop pid {} (`kill {}`)",
                rec.pid, rec.pid
            ))
        }
        None => CoreError::busy(
            "another process holds the project lock (.qrec/lock) but runs no Qrec host",
        )
        .with_hint("wait for it to finish, or find it with `fuser .qrec/lock`"),
    }
}

#[cfg(test)]
mod tests;
