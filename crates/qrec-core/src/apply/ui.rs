//! UI-state commands (selection, playhead, view). Owner: timeline.
//! Stub: signatures only.
//!
//! These change `State::ui` / `runtime.playing` and finish with
//! `history::persist` (never undoable). `apply` already rejects the G
//! commands outside a GUI host.
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::protocol::command::{SeekTarget, SelectOp, ShuttleDir, UiPatch};
use crate::state::{CanvasMode, State};

pub fn select(state: &State, ctx: &Ctx, op: SelectOp) -> Result<Outcome, CoreError> {
    todo!("timeline: Select")
}

/// `qrec seek`: `To` is a timeline frame, `By` relative; clamped to the timeline.
pub fn seek(state: &State, ctx: &Ctx, to: SeekTarget) -> Result<Outcome, CoreError> {
    todo!("timeline: Seek")
}

pub fn play(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    todo!("timeline: Play")
}

pub fn pause(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    todo!("timeline: Pause")
}

/// `j` / `k` / `l`.
pub fn shuttle(state: &State, ctx: &Ctx, dir: ShuttleDir) -> Result<Outcome, CoreError> {
    todo!("timeline: Shuttle")
}

/// `Tab`.
pub fn set_canvas_mode(state: &State, ctx: &Ctx, mode: CanvasMode) -> Result<Outcome, CoreError> {
    todo!("timeline: SetCanvasMode")
}

/// Zoom, scroll, panels, bin, snapping, solo, selected layer.
pub fn set_ui(state: &State, ctx: &Ctx, patch: UiPatch) -> Result<Outcome, CoreError> {
    todo!("timeline: SetUi")
}
