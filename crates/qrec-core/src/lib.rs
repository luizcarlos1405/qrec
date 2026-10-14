//! Qrec's functional core.
//!
//! Pure functions over plain data: no I/O, no clocks, no randomness. The
//! shell (`qrec-shell`) feeds commands and events in, runs the effects that
//! come out, and turns their results into events (docs/ARCHITECTURE.md).
//!
//! Entry points (A§4.1): [`facts_needed()`], [`apply()`], [`on_event()`], [`query()`].

#![forbid(unsafe_code)]
#![deny(clippy::disallowed_methods, clippy::disallowed_types)]

pub mod agent_guide;
pub mod apply;
pub mod autocut;
pub mod capture_plan;
pub mod config;
pub mod ctx;
pub mod error;
pub mod export_plan;
pub mod frame_plan;
pub mod ges_plan;
pub mod hash;
pub mod history;
pub mod ids;
pub mod keymap;
pub mod layout;
pub mod media_argv;
pub mod migrate;
pub mod model;
pub mod naming;
pub mod on_event;
pub mod parse;
pub mod plans;
pub mod protocol;
pub mod query;
pub mod requantize;
pub mod schema;
pub mod state;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
pub mod time;
pub mod timeline;
pub mod ui_normalize;
pub mod validate;

pub use apply::{Outcome, apply, facts_needed};
pub use ctx::{Ctx, Facts, FactsNeeded, IdGen};
pub use error::{CoreError, ErrorCode};
pub use ids::{ClipId, Frame, JobId, LayerId, ReqId, TakeId, TimerId, TrackId};
pub use model::*;
pub use on_event::{facts_needed_for_event, on_event};
pub use protocol::{CmdResult, Command, Effect, Event, Query};
pub use query::query;
pub use state::{History, HostKind, RecordPhase, Runtime, State, UiState};
pub use time::TimeArg;
