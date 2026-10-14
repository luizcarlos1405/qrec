//! The export planner (A§9.3): render units, ffmpeg filter graphs, cache
//! keys and the stitch step. Owner: output. Stub: signatures only.
#![allow(clippy::todo, unused_variables)]

use crate::error::CoreError;
use crate::ids::{ClipId, Frame, TakeId};
use crate::model::Project;
use crate::plans::ExportPlan;
use crate::protocol::command::{ExportFormat, Quality, VideoCodec};
use crate::state::Capabilities;

/// Bumped whenever generated argv change meaning (part of every cache key).
pub const PLAN_VERSION: u32 = 1;

/// What the user asked for, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub quality: Quality,
    /// Already checked against the format (mp4 ⇒ h264; webm ⇒ vp9 | av1).
    pub codec: VideoCodec,
    /// The absolute output path.
    pub out: String,
    /// Use the VA export chain when `caps.vaapi.ffmpeg_h264`; false re-plans
    /// with libx264 after a VA failure.
    pub allow_vaapi: bool,
}

/// A maximal run of continuous clips (same take, `out == next.in`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderUnit {
    pub clips: Vec<ClipId>,
    pub take: TakeId,
    pub in_: Frame,
    pub out: Frame,
    /// Timeline position of the unit's first frame.
    pub position: Frame,
}

/// The render units of the timeline. Property: a split never changes them.
pub fn render_units(project: &Project) -> Vec<RenderUnit> {
    todo!("output: render_units")
}

/// The whole export plan. `Invalid` "nothing to export" for an empty timeline.
pub fn plan_export(
    project: &Project,
    caps: &Capabilities,
    opts: &ExportOptions,
) -> Result<ExportPlan, CoreError> {
    todo!("output: plan_export")
}
