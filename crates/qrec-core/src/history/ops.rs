//! Undo, redo, restore and compaction over `History` (A§7.1, A§7.3).
//! Owner: history. Stub: signatures only.
#![allow(clippy::todo, unused_variables)]

use json_patch::Patch;

use crate::error::CoreError;
use crate::model::Project;
use crate::protocol::effect::HistoryOp;
use crate::state::{History, State};

/// Applies a patch to `view::project_value(project)`, deserializes and
/// validates the result, and re-derives take durations when the canvas fps
/// changed. A failing `test` op means the state drifted: `Invalid`.
pub fn apply_patch(project: &Project, patch: &Patch) -> Result<Project, CoreError> {
    todo!("history: apply a patch to the project value (A§7.2)")
}

/// Undoes `steps` entries (by position), all or nothing. Returns the new
/// state; its `history.cursor` is the new cursor. A failing step is
/// `Invalid` naming its seq.
pub fn undo(state: &State, steps: u32) -> Result<State, CoreError> {
    todo!("history: undo N (A§7.1)")
}

/// Redoes `steps` entries, all or nothing (like [`undo`]).
pub fn redo(state: &State, steps: u32) -> Result<State, CoreError> {
    todo!("history: redo N (A§7.1)")
}

/// Undoes or redoes to `seq` (`base_seq` or an existing seq, else `NotFound`).
pub fn restore(state: &State, seq: u64) -> Result<State, CoreError> {
    todo!("history: restore (A§7.3)")
}

/// Above 5,000 entries: drops the oldest down to 4,000 and returns the new
/// history with the `Rewrite` op (new base = the project before the oldest
/// kept entry). `None` when no compaction is due.
pub fn compact(history: &History, current: &Project) -> Option<(History, HistoryOp)> {
    todo!("history: compaction (A§7.3)")
}
