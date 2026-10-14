//! Hosts: the process that holds the project lock and serves the socket
//! (A§8.3, A§8.5, A§8.6). The submodules belong to two owners, so each
//! edits only its own files:
//!
//! - `lockfile`, `publish` — owner: store
//! - `headless`, `cli_host`, `signals`, `spawn` — owner: ipc

pub mod cli_host;
pub mod headless;
pub mod lockfile;
pub mod publish;
pub mod signals;
pub mod spawn;
