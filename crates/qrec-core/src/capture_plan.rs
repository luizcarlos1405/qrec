//! The capture planner (A§9.5): which record bins to attach for a take.
//! Owner: record. Stub: signatures only.
//!
//! File names: `L1-screen-DP-4.mkv`, `L2-camera-0.mkv`, `A1-mic.mka`,
//! `A2-system.mka` (sanitized to `[A-Za-z0-9._-]`). Visible and hidden
//! layers are both recorded. Encoder: `x264` ⇒ X264; else Va when
//! `caps.vaapi.gst_h264_factory` is known; else X264.
#![allow(clippy::todo, unused_variables)]

use crate::ctx::{CameraMode, Facts};
use crate::error::CoreError;
use crate::ids::TakeId;
use crate::model::{Fps, Project};
use crate::plans::CapturePlan;
use crate::state::Capabilities;

/// The plan for recording `take` with the project scene. Camera modes come
/// from `facts.cameras`, the encoder preference from the project and
/// `facts.config`.
pub fn plan_capture(
    project: &Project,
    take: &TakeId,
    caps: &Capabilities,
    facts: &Facts,
) -> Result<CapturePlan, CoreError> {
    todo!("record: plan_capture")
}

/// The closest mode ≥ `fps`, preferring the largest raw/MJPEG size.
pub fn choose_camera_mode(modes: &[CameraMode], fps: Fps) -> Option<CameraMode> {
    todo!("record: choose_camera_mode")
}
