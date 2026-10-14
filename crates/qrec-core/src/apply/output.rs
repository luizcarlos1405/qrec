//! Export and frame save (A§5.8, A§5.9). Owner: output. Stub: signatures only.
//!
//! One export at a time (`Busy`); frame saves may run alongside. Each job
//! snapshots its plan and the files it reads. Hand-picked `out` paths are
//! absolute and checked through `ctx.facts.out`.
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::ids::{JobId, LayerId};
use crate::protocol::command::{ExportFormat, Quality, VideoCodec};
use crate::state::State;
use crate::time::TimeArg;

/// `qrec export`: plans the export and emits `RenderExport`.
pub fn export(
    state: &State,
    ctx: &Ctx,
    format: ExportFormat,
    quality: Quality,
    codec: Option<VideoCodec>,
    out: Option<String>,
) -> Result<Outcome, CoreError> {
    todo!("output: Export")
}

/// `qrec export cancel`: `job: None` cancels the running export.
pub fn export_cancel(state: &State, ctx: &Ctx, job: Option<JobId>) -> Result<Outcome, CoreError> {
    todo!("output: ExportCancel")
}

/// `qrec frame save --at T [--layer L] [--out P]`.
pub fn frame_save(
    state: &State,
    ctx: &Ctx,
    at: TimeArg,
    layer: Option<LayerId>,
    out: Option<String>,
) -> Result<Outcome, CoreError> {
    todo!("output: FrameSave")
}
