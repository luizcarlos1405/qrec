//! History commands and take purge (A§7, A§5.6). Owner: history.
//! Stub: signatures only.
//!
//! Undo/redo/restore emit `History(SetCursor)` + `WriteProject`; purge emits
//! `History(Rewrite)`, `WriteProject`, then `DeleteTakeDirs` (A§5.6) and
//! replies when `TakeDirsDeleted` arrives (`on_event::disk`).
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::ids::TakeId;
use crate::state::State;

/// `qrec undo [N]`: all or nothing; `CmdResult.seq` is the new cursor.
pub fn undo(state: &State, ctx: &Ctx, steps: u32) -> Result<Outcome, CoreError> {
    todo!("history: Undo")
}

/// `qrec redo [N]`.
pub fn redo(state: &State, ctx: &Ctx, steps: u32) -> Result<Outcome, CoreError> {
    todo!("history: Redo")
}

/// `qrec history restore <seq> --yes`.
pub fn history_restore(state: &State, ctx: &Ctx, seq: u64) -> Result<Outcome, CoreError> {
    todo!("history: HistoryRestore")
}

/// `qrec history clear --yes`: no entries, base = current project.
pub fn history_clear(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    todo!("history: HistoryClear")
}

/// `qrec take purge <take>… --yes` (A§5.6, A§7.5). Not undoable.
pub fn take_purge(state: &State, ctx: &Ctx, takes: Vec<TakeId>) -> Result<Outcome, CoreError> {
    todo!("history: TakePurge")
}
