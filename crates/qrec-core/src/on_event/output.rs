//! Export and frame-save job events (A§5.8, A§5.9), and requesters going
//! away. Owner: output. Stub: signatures only.
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::ids::{JobId, ReqId};
use crate::state::State;

/// Progress = `0.9·Σseg_done/total + 0.1·stitch`; ETA from `ctx.now − started`.
pub fn export_progress(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    segment: usize,
    frames_done: u64,
) -> Outcome {
    todo!("output: ExportProgress")
}

pub fn export_stitch_progress(state: &State, ctx: &Ctx, job: JobId, out_time_ns: u64) -> Outcome {
    todo!("output: ExportStitchProgress")
}

/// Reply `ExportDone`; toast "Layers! Like an onion. Your video is ready."
pub fn export_finished(state: &State, ctx: &Ctx, job: JobId, path: String, bytes: u64) -> Outcome {
    todo!("output: ExportFinished")
}

pub fn export_failed(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    message: String,
    stderr_tail: Vec<String>,
    log: String,
) -> Outcome {
    todo!("output: ExportFailed")
}

pub fn export_cancelled(state: &State, ctx: &Ctx, job: JobId) -> Outcome {
    todo!("output: ExportCancelled")
}

/// Reply `FrameSavedView`; toast with Copy image / Open folder.
pub fn frame_saved(state: &State, ctx: &Ctx, job: JobId, path: String) -> Outcome {
    todo!("output: FrameSaved")
}

pub fn frame_failed(state: &State, ctx: &Ctx, job: JobId, message: String, log: String) -> Outcome {
    todo!("output: FrameFailed")
}

/// An IPC client disconnected (A§5.8): clear `job.req` for every job (export,
/// frame, autocut, purge); in Headless/Cli hosts cancel the jobs it started
/// that nobody else waits for. Never stops a recording.
pub fn requester_gone(state: &State, ctx: &Ctx, req: ReqId) -> Outcome {
    todo!("output: RequesterGone")
}
