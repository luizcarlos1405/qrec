//! The history rewrite that goes with a take purge (A§7.5). Owner: history.
//! Stub: signatures only.
#![allow(clippy::todo, unused_variables)]

use crate::error::CoreError;
use crate::ids::TakeId;
use crate::protocol::effect::HistoryOp;
use crate::state::{History, State};

/// What a purge does to the history.
#[derive(Debug, Clone, PartialEq)]
pub struct PurgeRewrite {
    /// The history after the rewrite.
    pub history: History,
    /// `HistoryOp::Rewrite` for the shell.
    pub op: HistoryOp,
    /// Entries dropped (for the confirmation `details`).
    pub entries_lost: usize,
    /// Label of the oldest surviving entry.
    pub oldest_kept: Option<String>,
}

/// Drops the entries that could bring `takes` back and strips `/takes/<id>`
/// ops from the rest (A§7.5).
pub fn purge_rewrite(state: &State, takes: &[TakeId]) -> Result<PurgeRewrite, CoreError> {
    todo!("history: purge rewrite (A§7.5)")
}
