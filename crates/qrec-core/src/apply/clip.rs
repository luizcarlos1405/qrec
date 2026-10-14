//! Timeline edits (A§5.2). Owner: timeline. Stub: signatures only.
//!
//! Each handler builds the next state with `crate::timeline` and finishes
//! with `history::commit` (label e.g. "Split clip c7f3 at 00:00:13:20",
//! `cmd` = the command's JSON). New clip ids come from
//! `IdGen::for_state(state, ctx.entropy)`.
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::ids::{ClipId, TakeId};
use crate::protocol::command::{InsertAt, MoveTo};
use crate::state::State;
use crate::time::TimeArg;

/// `qrec clip add <take> [--at N | --after <clip>]`: a clip for the whole take.
pub fn clip_add(
    state: &State,
    ctx: &Ctx,
    take: TakeId,
    at: InsertAt,
) -> Result<Outcome, CoreError> {
    todo!("timeline: ClipAdd")
}

/// `qrec clip move`: the clips move as one block in their original order.
pub fn clip_move(
    state: &State,
    ctx: &Ctx,
    clips: Vec<ClipId>,
    to: MoveTo,
) -> Result<Outcome, CoreError> {
    todo!("timeline: ClipMove")
}

/// `qrec clip trim`: clip-relative times unless `timeline`; values outside
/// the take are clamped and reported.
pub fn clip_trim(
    state: &State,
    ctx: &Ctx,
    clip: ClipId,
    start: Option<TimeArg>,
    end: Option<TimeArg>,
    timeline: bool,
) -> Result<Outcome, CoreError> {
    todo!("timeline: ClipTrim")
}

/// `qrec clip split`: `[in, p)` keeps the id, `[p, out)` gets a new one.
/// `clip: None` (with `timeline`) splits the clip under `at`.
pub fn clip_split(
    state: &State,
    ctx: &Ctx,
    clip: Option<ClipId>,
    at: TimeArg,
    timeline: bool,
) -> Result<Outcome, CoreError> {
    todo!("timeline: ClipSplit")
}

/// `qrec clip remove`: takes and files stay.
pub fn clip_remove(state: &State, ctx: &Ctx, clips: Vec<ClipId>) -> Result<Outcome, CoreError> {
    todo!("timeline: ClipRemove")
}
