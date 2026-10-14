//! IPC over the project socket (A§4.9, A§8.3). Owner: ipc.
//!
//! One request per connection, JSON lines, `v: 1`. A host answers with
//! `{"id","accepted":true}` once the request is queued for its Session, then
//! progress lines, then exactly one reply line, and closes the connection.
//!
//! - [`server`]: the host side. An acceptor thread and one thread per
//!   connection; same-uid peers only (`SO_PEERCRED`); request lines capped at
//!   1 MiB and read within 5 s; a bounded 64-message queue per connection
//!   (progress coalesced, replies never dropped) written with a 10 s timeout.
//!   `hello` is answered by the Session (or by the server while the Session
//!   is still opening); `shutdown` stops accepting and lets jobs finish.
//! - [`client`]: the CLI side. `hello` with a 2 s timeout, the routing loop of
//!   A§8.3 step 4 (3 s deadline, 20 s for `record *`, retries every 50 ms),
//!   re-route once on EOF before `accepted`, "host disconnected" after it.
//! - [`resolve`]: which project a command is for (A§8.4), and the live
//!   hosts on `<runtime>/*.sock`.
//! - [`framing`], [`peer`]: bounded JSON lines, peer credentials and a
//!   connect that can't block on a stuck listener.
//!
//! The hosts that run a server (`host::headless`, `host::cli_host`) and the
//! CLI's entry points (`host::request`, `host::run_cli`) live in `host`.

pub mod client;
pub mod framing;
pub mod peer;
pub mod resolve;
pub mod server;
#[cfg(test)]
pub(crate) mod testing;

use std::time::Duration;

/// The longest request line a host reads, newline excluded (A§8.3 step 7).
pub const MAX_REQUEST_LINE: usize = 1 << 20;
/// A client must send its whole request line within this time (A§8.3 step 7).
pub const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Messages queued per connection; progress is coalesced, replies are never dropped.
pub const CONN_QUEUE_CAP: usize = 64;
/// One message line must be written within this time, or the client is gone.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// A host that doesn't answer `hello` within this time is "not responding".
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the CLI tries to reach or become the host (A§8.3 step 4).
pub const ROUTE_DEADLINE: Duration = Duration::from_secs(3);
/// The routing deadline for `record *` (a host may be starting its capture).
pub const RECORD_ROUTE_DEADLINE: Duration = Duration::from_secs(20);
/// The pause between routing attempts while the lock holder is starting.
pub const ROUTE_RETRY: Duration = Duration::from_millis(50);
/// After the request line is written, `accepted` must arrive within this time.
pub const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);
/// With no message for this long, the client checks the host with `hello`.
pub const HEARTBEAT_AFTER: Duration = Duration::from_secs(30);
/// The longest host message line the client reads (a `State` can be large).
pub const MAX_REPLY_LINE: usize = 256 << 20;
