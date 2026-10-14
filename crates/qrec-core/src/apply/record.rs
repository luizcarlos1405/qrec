//! Recording commands (A§5.3 state machine, A§5.5 discard). Owner: record.
//! Stub: signatures only.
//!
//! `apply` has already rejected non-R commands while not `Idle`; these
//! handlers implement the transitions of the A§5.3 table. Begin take:
//! `ctx::next_take_number`, insert the take with `persist`, then
//! `CreateTakeDir`, `SetLive` (if needed) and `StartCapture`.
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::protocol::command::RecordModeArg;
use crate::state::State;

/// `qrec record start`: countdown > 0 ⇒ Countdown + `StartTimer`; else begin take.
pub fn record_start(
    state: &State,
    ctx: &Ctx,
    mode: RecordModeArg,
    countdown: Option<u32>,
    wait: bool,
) -> Result<Outcome, CoreError> {
    todo!("record: RecordStart")
}

/// `qrec record stop`: finalize (the caller waits for `TakeFinalizedView`).
pub fn record_stop(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    todo!("record: RecordStop")
}

/// `qrec record cancel`: files kept, take goes to the bin, no clip.
pub fn record_cancel(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    todo!("record: RecordCancel")
}

/// `qrec record toggle`: start when idle, stop otherwise.
pub fn record_toggle(state: &State, ctx: &Ctx, wait: bool) -> Result<Outcome, CoreError> {
    todo!("record: RecordToggle")
}

/// `qrec take discard-last` (A§5.5); while finalizing it is remembered and
/// applied after placement.
pub fn take_discard_last(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    todo!("record: TakeDiscardLast")
}
