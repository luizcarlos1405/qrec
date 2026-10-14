//! Project creation and settings (A§5.4). Owner: scene. Stub: signatures only.
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::model::{Color, EncoderPref};
use crate::protocol::command::{CanvasSpec, Nullable};
use crate::state::State;

/// `qrec init`: `Invalid` "already a project" if one exists. Uses
/// `facts.config` (`UserConfig::project_defaults`) and `facts.folder_name`.
pub fn init(
    state: &State,
    ctx: &Ctx,
    canvas: Option<CanvasSpec>,
    name: Option<String>,
) -> Result<Outcome, CoreError> {
    todo!("scene: Init")
}

/// `qrec project set`: an fps change with takes needs confirmation listing
/// every affected clip and re-quantizes them (`requantize`).
#[allow(clippy::too_many_arguments)]
pub fn project_set(
    state: &State,
    ctx: &Ctx,
    name: Option<String>,
    canvas: Option<CanvasSpec>,
    background: Option<Color>,
    countdown_secs: Option<u32>,
    encoder: Option<EncoderPref>,
    vaapi_device: Nullable<String>,
) -> Result<Outcome, CoreError> {
    todo!("scene: ProjectSet")
}
