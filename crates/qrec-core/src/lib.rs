//! Qrec's functional core.
//!
//! Pure functions over plain data: no I/O, no clocks, no randomness. The
//! shell (`qrec-shell`) feeds commands and events in, runs the effects that
//! come out, and turns their results into events (docs/ARCHITECTURE.md).

#![forbid(unsafe_code)]
#![deny(clippy::disallowed_methods, clippy::disallowed_types)]

pub mod error;
pub mod hash;
pub mod ids;
pub mod layout;
pub mod model;
pub mod time;

pub use error::{CoreError, ErrorCode};
pub use ids::{ClipId, Frame, JobId, LayerId, ReqId, TakeId, TimerId, TrackId};
pub use model::*;
pub use time::TimeArg;
