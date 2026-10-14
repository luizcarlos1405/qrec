//! `query`: read-only answers as JSON (A§4.5). Owner: meta. Stub: the
//! dispatch is real, the answers are signatures only.
//!
//! Views live in `protocol::reply` (`StatusView`, `TakeRow`, `ClipRow`,
//! `HistoryRow`). Queries work in every record phase and while
//! `qrec.json` on disk is invalid.
#![allow(clippy::todo, unused_variables)]

use serde_json::Value;

use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::ids::TakeId;
use crate::protocol::query::Query;
use crate::state::State;

/// Answers one query (pure).
pub fn query(state: &State, ctx: &Ctx, q: &Query) -> Result<Value, CoreError> {
    match q {
        Query::Status => status(state, ctx),
        Query::State => state_json(state),
        Query::TakeList { unused_only } => take_list(state, *unused_only),
        Query::TakeShow { take } => take_show(state, take),
        Query::ClipList => clip_list(state),
        Query::History { limit } => history(state, *limit),
        Query::Keys => keys(state, ctx),
    }
}

/// `StatusView` (`path`/`pid` are left `None` for the shell to fill).
pub fn status(state: &State, ctx: &Ctx) -> Result<Value, CoreError> {
    todo!("meta: Query::Status")
}

/// The whole `State` as JSON.
pub fn state_json(state: &State) -> Result<Value, CoreError> {
    todo!("meta: Query::State")
}

/// `[TakeRow]`, all takes or only the bin.
pub fn take_list(state: &State, unused_only: bool) -> Result<Value, CoreError> {
    todo!("meta: Query::TakeList")
}

/// One take in full (`NotFound` for an unknown id).
pub fn take_show(state: &State, take: &TakeId) -> Result<Value, CoreError> {
    todo!("meta: Query::TakeShow")
}

/// `[ClipRow]` in timeline order.
pub fn clip_list(state: &State) -> Result<Value, CoreError> {
    todo!("meta: Query::ClipList")
}

/// `[HistoryRow]`, the newest `limit` entries.
pub fn history(state: &State, limit: Option<usize>) -> Result<Value, CoreError> {
    todo!("meta: Query::History")
}

/// The effective keymap (`keymap::resolve`).
pub fn keys(state: &State, ctx: &Ctx) -> Result<Value, CoreError> {
    todo!("meta: Query::Keys")
}
