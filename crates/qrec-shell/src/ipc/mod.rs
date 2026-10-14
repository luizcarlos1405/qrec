//! IPC over the project socket (A§4.9, A§8.3): JSON lines, one request per
//! connection, same-uid peers (`SO_PEERCRED`), 1 MiB request lines with a
//! 5 s read timeout, a bounded 64-message queue per connection (progress
//! coalesced, replies never dropped) and a 10 s write timeout; plus the CLI
//! client side: routing, `hello`, re-route on EOF before `accepted`.
//! Owner: ipc. Empty stub.
