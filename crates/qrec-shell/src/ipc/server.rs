//! The host side of the project socket (A§4.9, A§8.3).
//!
//! An acceptor thread admits same-uid peers (`SO_PEERCRED`) and gives each
//! connection its own thread. A connection reads one request line (≤ 1 MiB
//! within 5 s), answers `{"id","accepted":true}` once the request is queued
//! for the Session, then writes what the Session sends through the
//! connection's [`Responder`] from a bounded 64-message queue (progress is
//! coalesced, replies are never dropped), each line within 10 s, and closes
//! after the reply.
//!
//! The server can start before its Session exists (A§8.3 step 1: the socket
//! is published before recovery and `gst::init()`): until [`IpcServer::attach`]
//! requests are parked in order and `hello` is answered by the server itself
//! (a host that hasn't opened its Session is idle and has no jobs).
//! `shutdown` is handled here: stop accepting, unlink the socket, tell the
//! host ([`ServerEvent::Shutdown`]), reply.
//!
//! Contract for clients: keep the connection open until the reply. Closing
//! (or half-closing) it means the requester is gone (A§5.8).

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use qrec_core::protocol::{
    CmdResult, HelloInfo, Progress, WIRE_VERSION, WireBody, WireMessage, WireRequest,
};
use qrec_core::state::{HostKind, RecordPhase};
use qrec_core::{CoreError, Event, ReqId};
use serde_json::Value;

use super::framing::{LineReader, ReadError, encode_line, write_all_before};
use crate::host::publish::PublishedSocket;
use super::{CONN_QUEUE_CAP, MAX_REQUEST_LINE, REQUEST_READ_TIMEOUT, WRITE_TIMEOUT, peer};
use crate::msg::{Inbox, Msg, Request, Responder};

/// How often an idle connection checks whether its client left.
const GONE_CHECK: Duration = Duration::from_millis(200);

/// How a server identifies itself and the limits it enforces.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// The canonical project directory (`HelloInfo::project_dir`).
    pub project_dir: String,
    pub host: HostKind,
    pub pid: u32,
    /// The Qrec version (`HelloInfo::version`).
    pub version: String,
    /// Peers must have this uid (our effective uid).
    pub allowed_uid: u32,
    /// The whole request line must arrive within this time.
    pub read_timeout: Duration,
    /// Each message line must be written within this time.
    pub write_timeout: Duration,
    /// The longest request line.
    pub max_line: usize,
}

impl ServerConfig {
    /// The A§8.3 limits for this process.
    pub fn new(project_dir: &Path, host: HostKind) -> Self {
        Self {
            project_dir: project_dir.to_string_lossy().into_owned(),
            host,
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            allowed_uid: peer::our_uid(),
            read_timeout: REQUEST_READ_TIMEOUT,
            write_timeout: WRITE_TIMEOUT,
            max_line: MAX_REQUEST_LINE,
        }
    }

    /// What `hello` answers before the Session exists: a host that hasn't
    /// opened its Session records nothing and runs no jobs.
    pub fn starting_hello(&self) -> HelloInfo {
        HelloInfo {
            pid: self.pid,
            project_dir: self.project_dir.clone(),
            host: self.host,
            record: RecordPhase::Idle,
            jobs: Vec::new(),
            version: self.version.clone(),
        }
    }
}

/// What the server tells its host, from connection threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerEvent {
    /// A command or query was accepted (`hello` and `shutdown` don't count).
    Request,
    /// The last open connection closed.
    Idle,
    /// A `shutdown` request arrived; the server stopped accepting.
    Shutdown,
}

/// The host's callback for [`ServerEvent`]s. Must be quick and never block.
pub type Hook = Arc<dyn Fn(ServerEvent) + Send + Sync>;

/// A snapshot of the server for the host's exit rules (A§8.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerStats {
    /// Open connections (each is one request until its reply is flushed).
    pub open: usize,
    /// Since when no connection is open; `None` while one is.
    pub idle_since: Option<Instant>,
    /// Commands and queries accepted so far.
    pub requests: u64,
    /// A `shutdown` request arrived.
    pub shutdown: bool,
    pub accepting: bool,
}

/// The project socket's server. Dropping it stops accepting, unlinks the
/// socket and closes every connection.
pub struct IpcServer {
    shared: Arc<Shared>,
    acceptor: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for IpcServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcServer")
            .field("config", &self.shared.cfg)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl IpcServer {
    /// Serves a published socket. Requests are parked until [`attach`](Self::attach).
    pub fn start(
        published: PublishedSocket,
        cfg: ServerConfig,
        hook: Hook,
    ) -> Result<IpcServer, CoreError> {
        let io_err = |what: &str, e: io::Error| CoreError::io(format!("{what}: {e}"));
        // The acceptor's own handle; `O_NONBLOCK` is shared with the original.
        let listener = published
            .try_clone_listener()
            .map_err(|e| io_err("can't clone the listening socket", e))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| io_err("can't configure the socket", e))?;
        let (wake_rd, wake_wr) = pipe_cloexec().map_err(|e| io_err("can't create a pipe", e))?;
        let shared = Arc::new(Shared {
            cfg,
            hook,
            link: Mutex::new(Link::Pending(Vec::new())),
            accepting: AtomicBool::new(true),
            shutdown: AtomicBool::new(false),
            socket: Mutex::new(Some(published)),
            wake_wr,
            conns: Mutex::new(Conns {
                open: BTreeMap::new(),
                idle_since: Some(Instant::now()),
            }),
            conns_cv: Condvar::new(),
            requests: AtomicU64::new(0),
            next_conn: AtomicU64::new(1),
        });
        let for_thread = Arc::clone(&shared);
        let acceptor = thread::Builder::new()
            .name("qrec-ipc-accept".into())
            .spawn(move || accept_loop(&for_thread, &listener, &wake_rd))
            .map_err(|e| io_err("can't start the IPC acceptor", e))?;
        Ok(IpcServer {
            shared,
            acceptor: Mutex::new(Some(acceptor)),
        })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.shared.cfg
    }

    /// Connects the Session: parked messages go to `inbox` in arrival order,
    /// then everything goes there directly.
    pub fn attach(&self, inbox: Inbox) {
        let mut link = lock(&self.shared.link);
        match std::mem::replace(&mut *link, Link::Attached(inbox.clone())) {
            Link::Pending(parked) => {
                for msg in parked {
                    inbox.send(msg);
                }
            }
            Link::Attached(_) => {}
            failed @ Link::Failed(_) => *link = failed,
        }
    }

    /// The Session couldn't open (or is gone): parked and future requests
    /// get `err` as their reply.
    pub fn fail(&self, err: CoreError) {
        self.shared.fail(err);
    }

    /// Sends a message to the Session, parking it until [`attach`](Self::attach).
    /// `false` when it can't be delivered (a failed request is answered with the failure).
    pub fn send(&self, msg: Msg) -> bool {
        self.shared.submit(msg)
    }

    /// A handle other threads use like [`send`](Self::send).
    pub fn sender(&self) -> ServerSender {
        ServerSender(Arc::clone(&self.shared))
    }

    pub fn stats(&self) -> ServerStats {
        let conns = lock(&self.shared.conns);
        ServerStats {
            open: conns.open.len(),
            idle_since: conns.idle_since,
            requests: self.shared.requests.load(Ordering::SeqCst),
            shutdown: self.shared.shutdown.load(Ordering::SeqCst),
            accepting: self.shared.accepting.load(Ordering::SeqCst),
        }
    }

    /// A§8.3 step 3 begins: stop accepting and unlink the socket. Open
    /// connections keep being served. Idempotent.
    pub fn stop_accepting(&self) {
        self.shared.stop_accepting();
        let handle = lock(&self.acceptor).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// The rest of the exit sequence: stop accepting, refuse anything new
    /// with `why`, let open connections flush for up to `timeout`, then give
    /// the stragglers `why` as their reply and close them. Drop the Session
    /// before calling this, so requests it still held are answered.
    pub fn finish(&self, why: CoreError, timeout: Duration) {
        self.stop_accepting();
        self.shared.fail(why.clone());
        if self.shared.wait_closed(Instant::now() + timeout) {
            return;
        }
        for out in self.shared.open_outgoing() {
            out.push_reply(&Err(why.clone()));
        }
        if self
            .shared
            .wait_closed(Instant::now() + Duration::from_secs(1))
        {
            return;
        }
        self.shared.force_close();
        self.shared
            .wait_closed(Instant::now() + Duration::from_secs(1));
    }
}

/// Sends messages to the Session through the server (parked until attach).
#[derive(Clone)]
pub struct ServerSender(Arc<Shared>);

impl ServerSender {
    /// Like [`IpcServer::send`].
    pub fn send(&self, msg: Msg) -> bool {
        self.0.submit(msg)
    }
}

impl std::fmt::Debug for ServerSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerSender")
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop_accepting();
        self.shared.fail(CoreError::io("the host is exiting"));
        self.shared.force_close();
    }
}

// ----------------------------------------------------------------- shared

struct Shared {
    cfg: ServerConfig,
    hook: Hook,
    link: Mutex<Link>,
    accepting: AtomicBool,
    shutdown: AtomicBool,
    /// Taken (unpublished and closed) when the server stops accepting; the
    /// acceptor closes its own handle when it exits.
    socket: Mutex<Option<PublishedSocket>>,
    /// Wakes the acceptor out of `poll`.
    wake_wr: OwnedFd,
    conns: Mutex<Conns>,
    conns_cv: Condvar,
    requests: AtomicU64,
    next_conn: AtomicU64,
}

/// Where requests go.
enum Link {
    /// No Session yet: messages wait here, in order.
    Pending(Vec<Msg>),
    Attached(Inbox),
    /// The Session failed to open or is gone: requests get this error.
    Failed(CoreError),
}

struct Conns {
    open: BTreeMap<u64, ConnEntry>,
    idle_since: Option<Instant>,
}

struct ConnEntry {
    /// A second handle, to force the connection closed.
    stream: UnixStream,
    out: Arc<Outgoing>,
}

impl Shared {
    fn stop_accepting(&self) {
        if self.accepting.swap(false, Ordering::SeqCst) {
            let byte = [1u8];
            // SAFETY: writing one byte from a valid buffer to our pipe fd.
            unsafe { libc::write(self.wake_wr.as_raw_fd(), byte.as_ptr().cast(), 1) };
        }
        if let Some(mut socket) = lock(&self.socket).take() {
            if let Err(e) = socket.unpublish() {
                tracing::warn!("IPC: {e}");
            }
        }
    }

    fn submit(&self, msg: Msg) -> bool {
        let mut link = lock(&self.link);
        match &mut *link {
            Link::Pending(parked) => {
                parked.push(msg);
                true
            }
            // Sent under the lock so nothing overtakes the parked messages
            // `attach` flushes. A failed send drops `msg`; a dropped request
            // answers itself (`ConnResponder::drop`).
            Link::Attached(inbox) => inbox.send(msg),
            Link::Failed(err) => {
                let err = err.clone();
                drop(link);
                answer_with(msg, err);
                false
            }
        }
    }

    fn fail(&self, err: CoreError) {
        let parked = {
            let mut link = lock(&self.link);
            match std::mem::replace(&mut *link, Link::Failed(err.clone())) {
                Link::Pending(parked) => parked,
                Link::Attached(_) | Link::Failed(_) => Vec::new(),
            }
        };
        for msg in parked {
            answer_with(msg, err.clone());
        }
    }

    fn begin_shutdown(&self) {
        if !self.shutdown.swap(true, Ordering::SeqCst) {
            self.stop_accepting();
            (self.hook)(ServerEvent::Shutdown);
        }
    }

    fn admit(self: &Arc<Self>, stream: UnixStream) {
        if let Err(e) = stream.set_nonblocking(false) {
            tracing::warn!("IPC: can't configure a connection: {e}");
            return;
        }
        match peer::peer_cred(&stream) {
            Ok(cred) if cred.uid == self.cfg.allowed_uid => {}
            Ok(cred) => {
                tracing::warn!(
                    "IPC: refused a connection from uid {} (pid {})",
                    cred.uid,
                    cred.pid
                );
                return;
            }
            Err(e) => {
                tracing::warn!("IPC: can't read peer credentials: {e}");
                return;
            }
        }
        let handle = match stream.try_clone() {
            Ok(handle) => handle,
            Err(e) => {
                tracing::warn!("IPC: can't clone a connection: {e}");
                return;
            }
        };
        let id = self.next_conn.fetch_add(1, Ordering::SeqCst);
        let out = Arc::new(Outgoing::default());
        {
            let mut conns = lock(&self.conns);
            conns.open.insert(
                id,
                ConnEntry {
                    stream: handle,
                    out: Arc::clone(&out),
                },
            );
            conns.idle_since = None;
        }
        let shared = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name(format!("qrec-ipc-{id}"))
            .spawn(move || {
                let _closed = ConnGuard {
                    shared: Arc::clone(&shared),
                    id,
                };
                serve_conn(&shared, &stream, &out);
            });
        if let Err(e) = spawned {
            tracing::warn!("IPC: can't start a connection thread: {e}");
            self.close_conn(id);
        }
    }

    fn close_conn(&self, id: u64) {
        let became_idle = {
            let mut conns = lock(&self.conns);
            if conns.open.remove(&id).is_none() {
                return;
            }
            let idle = conns.open.is_empty();
            if idle {
                conns.idle_since = Some(Instant::now());
            }
            idle
        };
        self.conns_cv.notify_all();
        if became_idle {
            (self.hook)(ServerEvent::Idle);
        }
    }

    fn open_outgoing(&self) -> Vec<Arc<Outgoing>> {
        lock(&self.conns)
            .open
            .values()
            .map(|entry| Arc::clone(&entry.out))
            .collect()
    }

    fn force_close(&self) {
        let conns = lock(&self.conns);
        for entry in conns.open.values() {
            entry.out.mark_gone();
            let _ = entry.stream.shutdown(Shutdown::Both);
        }
    }

    /// Waits until no connection is open; `false` at the deadline.
    fn wait_closed(&self, deadline: Instant) -> bool {
        let mut conns = lock(&self.conns);
        loop {
            if conns.open.is_empty() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            conns = self
                .conns_cv
                .wait_timeout(conns, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// Answers a request that can't reach the Session with `err`.
fn answer_with(msg: Msg, err: CoreError) {
    if let Msg::Request { mut to, .. } = msg {
        to.reply(Err(err));
    }
}

struct ConnGuard {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.shared.close_conn(self.id);
    }
}

// --------------------------------------------------------------- acceptor

fn accept_loop(shared: &Arc<Shared>, listener: &UnixListener, wake_rd: &OwnedFd) {
    while shared.accepting.load(Ordering::SeqCst) {
        let mut fds = [
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_rd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `fds` is a valid array of two pollfds. The 1 s timeout is a
        // safety net in case a wake-up byte gets lost.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, 1000) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                tracing::warn!("IPC: poll failed: {e}");
                thread::sleep(Duration::from_millis(100));
            }
            continue;
        }
        if !shared.accepting.load(Ordering::SeqCst) || fds[1].revents != 0 {
            break;
        }
        if fds[0].revents & libc::POLLIN == 0 {
            if fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                tracing::warn!("IPC: the listening socket failed");
                break;
            }
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => shared.admit(stream),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                ) => {}
            Err(e) => {
                // EMFILE and friends: back off instead of spinning.
                tracing::warn!("IPC: accept failed: {e}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

// ------------------------------------------------------------- connection

fn serve_conn(shared: &Arc<Shared>, stream: &UnixStream, out: &Arc<Outgoing>) {
    let read_half = match stream.try_clone() {
        Ok(read_half) => read_half,
        Err(e) => {
            tracing::warn!("IPC: can't clone a connection: {e}");
            return;
        }
    };
    let mut reader = LineReader::new(read_half);
    let deadline = Instant::now() + shared.cfg.read_timeout;
    let line = match reader.read_line(shared.cfg.max_line, deadline) {
        Ok(line) => line,
        Err(ReadError::Eof | ReadError::Truncated) => return,
        Err(ReadError::Io(e)) => {
            tracing::debug!("IPC: read failed: {e}");
            return;
        }
        Err(ReadError::TooLong) => {
            out.push_reply(&Err(CoreError::usage(format!(
                "request line longer than {} bytes",
                shared.cfg.max_line
            ))));
            return pump(shared, stream, out);
        }
        Err(ReadError::Timeout) => {
            out.push_reply(&Err(CoreError::usage(format!(
                "no complete request line within {} s",
                shared.cfg.read_timeout.as_secs_f64()
            ))));
            return pump(shared, stream, out);
        }
    };
    let request = match parse_request(&line, &shared.cfg) {
        Ok(request) => request,
        Err((id, err)) => {
            out.set_id(id);
            out.push_reply(&Err(err));
            return pump(shared, stream, out);
        }
    };
    out.set_id(request.id);
    // Accepted first: it goes out before anything the Session sends.
    out.push_accepted();
    let req = Request {
        body: request.body,
        confirmed: request.confirmed,
    };
    let responder = || Box::new(ConnResponder::new(Arc::clone(out), Arc::downgrade(shared)));
    match &req.body {
        WireBody::Shutdown => {
            shared.begin_shutdown();
            let result = CmdResult::new(format!(
                "the {} host (pid {}) is shutting down",
                shared.cfg.host.as_str(),
                shared.cfg.pid
            ));
            out.push_reply(&Ok(to_value(&result)));
        }
        WireBody::Hello => {
            let attached = {
                let link = lock(&shared.link);
                match &*link {
                    Link::Pending(_) => None,
                    Link::Attached(inbox) => Some(Ok(inbox.clone())),
                    Link::Failed(err) => Some(Err(err.clone())),
                }
            };
            match attached {
                None => out.push_reply(&Ok(to_value(&shared.cfg.starting_hello()))),
                Some(Ok(inbox)) => {
                    inbox.send(Msg::Request {
                        req,
                        to: responder(),
                    });
                }
                Some(Err(err)) => out.push_reply(&Err(err)),
            }
        }
        WireBody::Command(_) | WireBody::Query(_) => {
            shared.requests.fetch_add(1, Ordering::SeqCst);
            (shared.hook)(ServerEvent::Request);
            shared.submit(Msg::Request {
                req,
                to: responder(),
            });
        }
    }
    pump(shared, stream, out);
}

/// Parses a request line; on failure returns the id (if one was readable)
/// and a usage error.
fn parse_request(line: &[u8], cfg: &ServerConfig) -> Result<WireRequest, (String, CoreError)> {
    let value: Value = serde_json::from_slice(line).map_err(|e| {
        (
            String::new(),
            CoreError::usage(format!("bad request line: {e}")),
        )
    })?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let version = value.get("v");
    if version.is_some_and(|v| v.as_u64() != Some(u64::from(WIRE_VERSION))) {
        let v = version.cloned().unwrap_or_default();
        return Err((
            id,
            CoreError::usage(format!(
                "unsupported protocol version {v}; this host (qrec {}) speaks version {WIRE_VERSION}",
                cfg.version
            ))
            .with_hint("use the same qrec version for the CLI and the running host"),
        ));
    }
    serde_json::from_value(value).map_err(|e| (id, CoreError::usage(format!("bad request: {e}"))))
}

fn to_value(view: &impl serde::Serialize) -> Value {
    serde_json::to_value(view).expect("wire views serialize")
}

/// Writes queued messages until the reply is out, the client leaves or the
/// connection is closed; then shuts the write side so the client sees EOF.
fn pump(shared: &Shared, stream: &UnixStream, out: &Outgoing) {
    loop {
        match out.next(GONE_CHECK) {
            Next::Message(msg) => {
                let deadline = Instant::now() + shared.cfg.write_timeout;
                if let Err(e) = write_all_before(stream, &encode_line(&msg), deadline) {
                    tracing::debug!("IPC: client gone while writing: {e}");
                    out.mark_gone();
                    return;
                }
            }
            Next::Done => break,
            Next::Closed => return,
            Next::Idle => {
                if client_gone(stream) {
                    out.mark_gone();
                    return;
                }
            }
        }
    }
    let _ = stream.shutdown(Shutdown::Write);
}

/// Whether the client closed (or half-closed) its end. Junk it sends after
/// the request line is discarded.
fn client_gone(stream: &UnixStream) -> bool {
    let fd = stream.as_raw_fd();
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLRDHUP,
        revents: 0,
    };
    // SAFETY: one valid pollfd, zero timeout.
    let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
    if rc <= 0 {
        return false;
    }
    if pfd.revents & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return true;
    }
    if pfd.revents & libc::POLLIN != 0 {
        let mut junk = [0u8; 4096];
        // SAFETY: reading into a valid local buffer without blocking.
        let n = unsafe { libc::recv(fd, junk.as_mut_ptr().cast(), junk.len(), libc::MSG_DONTWAIT) };
        if n == 0 {
            return true;
        }
        if n < 0 {
            let e = io::Error::last_os_error();
            return !matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            );
        }
    }
    false
}

// --------------------------------------------------------------- outgoing

/// One connection's outgoing messages, shared by its thread and its responder.
#[derive(Default)]
struct Outgoing {
    st: Mutex<OutState>,
    cv: Condvar,
}

#[derive(Default)]
struct OutState {
    /// The client's request id.
    id: String,
    queue: VecDeque<WireMessage>,
    accepted: bool,
    /// The reply is queued (or was given up on); nothing follows it.
    replied: bool,
    /// The client left or the connection was closed: messages are dropped.
    gone: bool,
    /// The Session request this connection serves, for `RequesterGone`.
    bound: Option<(ReqId, Inbox)>,
    gone_reported: bool,
}

enum Next {
    Message(WireMessage),
    /// The reply was written.
    Done,
    Closed,
    /// Nothing to write for a while.
    Idle,
}

impl Outgoing {
    fn state(&self) -> MutexGuard<'_, OutState> {
        lock(&self.st)
    }

    fn set_id(&self, id: String) {
        self.state().id = id;
    }

    fn push_accepted(&self) {
        let mut st = self.state();
        if st.accepted || st.gone || st.replied {
            return;
        }
        st.accepted = true;
        let msg = WireMessage::accepted(st.id.clone());
        st.queue.push_back(msg);
        self.cv.notify_all();
    }

    /// Queues progress; while the client lags, newer progress replaces the
    /// queued one, and a full queue drops its oldest progress.
    fn push_progress(&self, progress: &Progress) {
        let mut st = self.state();
        if st.gone || st.replied {
            return;
        }
        if let Some(WireMessage::Progress { progress: last, .. }) = st.queue.back_mut() {
            *last = progress.clone();
            return;
        }
        if st.queue.len() >= CONN_QUEUE_CAP {
            match st
                .queue
                .iter()
                .position(|m| matches!(m, WireMessage::Progress { .. }))
            {
                Some(oldest) => {
                    st.queue.remove(oldest);
                }
                None => return,
            }
        }
        let msg = WireMessage::progress(st.id.clone(), progress.clone());
        st.queue.push_back(msg);
        self.cv.notify_all();
    }

    /// Queues the reply; only the first one counts, and it is never dropped.
    fn push_reply(&self, result: &Result<Value, CoreError>) {
        let mut st = self.state();
        if st.replied {
            return;
        }
        st.replied = true;
        if !st.gone {
            let msg = WireMessage::reply(st.id.clone(), result);
            st.queue.push_back(msg);
        }
        self.cv.notify_all();
    }

    fn has_reply(&self) -> bool {
        self.state().replied
    }

    /// The client is gone. Tells the Session once if it still owed a reply.
    fn mark_gone(&self) {
        let report = {
            let mut st = self.state();
            if st.gone {
                return;
            }
            st.gone = true;
            st.queue.clear();
            Self::take_report(&mut st)
        };
        self.cv.notify_all();
        if let Some((req, inbox)) = report {
            inbox.event(Event::RequesterGone { req });
        }
    }

    fn bind(&self, req: ReqId, inbox: Inbox) {
        let report = {
            let mut st = self.state();
            st.bound = Some((req, inbox));
            if st.gone {
                Self::take_report(&mut st)
            } else {
                None
            }
        };
        if let Some((req, inbox)) = report {
            inbox.event(Event::RequesterGone { req });
        }
    }

    fn take_report(st: &mut OutState) -> Option<(ReqId, Inbox)> {
        if st.gone && !st.replied && !st.gone_reported && st.bound.is_some() {
            st.gone_reported = true;
            st.bound.clone()
        } else {
            None
        }
    }

    fn next(&self, wait: Duration) -> Next {
        let mut st = self.state();
        loop {
            if st.gone {
                return Next::Closed;
            }
            if let Some(msg) = st.queue.pop_front() {
                return Next::Message(msg);
            }
            if st.replied {
                return Next::Done;
            }
            let (guard, timeout) = self
                .cv
                .wait_timeout(st, wait)
                .unwrap_or_else(PoisonError::into_inner);
            st = guard;
            if timeout.timed_out() && st.queue.is_empty() && !st.replied && !st.gone {
                return Next::Idle;
            }
        }
    }
}

/// How the Session answers an IPC request: messages go to the connection's
/// queue and never block the session thread on the network. A responder
/// dropped without replying answers with an error, so every accepted
/// request gets exactly one reply.
///
/// When the Session accepts the request it learns the request's `ReqId`
/// (`session::current_request`), so a client that disconnects before its
/// reply becomes `Event::RequesterGone` (A§5.8: e.g. Ctrl+C on a routed
/// export cancels it in a headless or CLI host).
pub struct ConnResponder {
    out: Arc<Outgoing>,
    /// The server, for the Session's inbox.
    shared: Weak<Shared>,
}

impl ConnResponder {
    fn new(out: Arc<Outgoing>, shared: Weak<Shared>) -> Self {
        Self { out, shared }
    }

    /// The Session's inbox, once attached.
    fn session_inbox(&self) -> Option<Inbox> {
        let shared = self.shared.upgrade()?;
        let link = lock(&shared.link);
        match &*link {
            Link::Attached(inbox) => Some(inbox.clone()),
            Link::Pending(_) | Link::Failed(_) => None,
        }
    }

    /// Tells the connection which Session request it carries, so a client
    /// that disconnects before its reply becomes `Event::RequesterGone`
    /// (A§5.8). [`Responder::accepted`] does this on its own when the
    /// Session accepts the request (it knows the `ReqId` then).
    pub fn bind(&self, req: ReqId, inbox: Inbox) {
        self.out.bind(req, inbox);
    }
}

impl Responder for ConnResponder {
    fn accepted(&mut self) {
        self.out.push_accepted();
        // Called by the Session on its thread, with the request's id current.
        if let (Some(req), Some(inbox)) =
            (crate::session::current_request(), self.session_inbox())
        {
            self.out.bind(req, inbox);
        }
    }

    fn progress(&mut self, p: &Progress) {
        self.out.push_progress(p);
    }

    fn reply(&mut self, r: Result<Value, CoreError>) {
        self.out.push_reply(&r);
    }
}

impl Drop for ConnResponder {
    fn drop(&mut self) {
        if !self.out.has_reply() {
            self.out.push_reply(&Err(CoreError::io(
                "the host dropped this request without an answer",
            )));
        }
    }
}

// ------------------------------------------------------------------ utils

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A pipe with `O_CLOEXEC` on both ends: (read, write).
pub(crate) fn pipe_cloexec() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two fds pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both fds are fresh and owned by us.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

#[cfg(test)]
mod tests;
