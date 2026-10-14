//! Silence analysis results (A§5.7). Owner: autocut. Stub: signatures only.
//!
//! Store the intervals in `runtime.silence`, drop the key from the job's
//! `pending`; when nothing is pending apply the plan (one history entry) or,
//! while recording, mark the job `ready` for after placement.
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::ids::JobId;
use crate::protocol::event::Interval;
use crate::state::{SilenceKey, State};

pub fn silence_detected(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    key: SilenceKey,
    silences: Vec<Interval>,
) -> Outcome {
    todo!("autocut: SilenceDetected")
}

pub fn silence_failed(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    key: SilenceKey,
    message: String,
) -> Outcome {
    todo!("autocut: SilenceFailed")
}
