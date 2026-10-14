//! Autocut commands (A§5.7). Owner: autocut. Stub: signatures only.
//!
//! `Autocut` snapshots params and targets into a `JobKind::Autocut` job,
//! emits `DetectSilence` for keys missing from `runtime.silence` (file facts
//! in `ctx.facts.files`, keys from `SilenceKey::compute`) and completes in
//! `on_event::autocut` — or right away when every key is known.
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::protocol::command::{AutocutOverrides, AutocutPatch, ClipScope};
use crate::state::State;

/// `qrec autocut [--clips …|--all] [--dry-run] …`.
pub fn autocut(
    state: &State,
    ctx: &Ctx,
    scope: ClipScope,
    overrides: AutocutOverrides,
    dry_run: bool,
) -> Result<Outcome, CoreError> {
    todo!("autocut: Autocut")
}

/// `qrec autocut settings …`: edits `project.autocut` (undoable).
pub fn autocut_settings(
    state: &State,
    ctx: &Ctx,
    patch: AutocutPatch,
) -> Result<Outcome, CoreError> {
    todo!("autocut: AutocutSettings")
}
