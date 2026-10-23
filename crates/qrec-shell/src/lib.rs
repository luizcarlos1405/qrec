//! Qrec's imperative shell (A§8).
//!
//! Everything the pure core can't do: files and the history store, the
//! project lock and socket, hosts and IPC, child processes (ffmpeg,
//! ffprobe, pactl), GStreamer capture and the portal, device discovery and
//! `doctor`. One `Session` per project runs on one thread, drains an
//! `Inbox` of `Msg`s, calls the core and executes the `Effect`s it returns.
//!
//! Module owners (A§1.4): `msg`, `deps`, `error`, `wire`, `platform` are
//! foundation; `store`, `lock`, `host::{lockfile,publish}`, `watcher` →
//! store; `session`, `facts`, `caps`, `timers`, `proc`, `spawn` → session;
//! `ipc`, `host::{headless,cli_host,signals,spawn}` → ipc; `media`,
//! `sources`, `doctor` → media; `capture`, `gpu`, `gst_init`, `ges_render`,
//! `player` → capture; `config_file`, `keys_file` → meta.

pub mod caps;
pub mod capture;
pub mod config_file;
pub mod deps;
pub mod doctor;
pub mod error;
pub mod facts;
#[cfg(feature = "gst")]
pub mod ges_render;
pub mod gpu;
#[cfg(feature = "gst")]
pub mod gst_init;
pub mod host;
pub mod ipc;
pub mod keys_file;
pub mod lock;
pub mod media;
pub mod msg;
pub mod paths;
pub mod platform;
#[cfg(feature = "gst")]
pub mod player;
pub mod proc;
pub mod session;
pub mod sources;
pub(crate) mod spawn;
pub mod store;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
pub mod timers;
pub mod watcher;
pub mod wire;

pub use deps::{CaptureEngine, Deps, MediaRunner, NoCapture, OpenOpts};
pub use error::ShellError;
pub use msg::{Inbox, Msg, Request, Responder};
pub use session::Session;
