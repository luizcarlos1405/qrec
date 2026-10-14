//! The frame-save planner (A§9.4). Owner: output. Stub: signatures only.
#![allow(clippy::todo, unused_variables)]

use crate::error::CoreError;
use crate::ids::{Frame, LayerId};
use crate::model::Project;
use crate::plans::FramePlan;

/// The PNG of timeline frame `at` through the export graph as a one-frame
/// unit (sRGB). With `layer`: that layer's raw source frame at native
/// resolution. `out` is the absolute output path.
pub fn plan_frame(
    project: &Project,
    at: Frame,
    layer: Option<&LayerId>,
    out: &str,
) -> Result<FramePlan, CoreError> {
    todo!("output: plan_frame")
}
