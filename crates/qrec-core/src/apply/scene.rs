//! Scene commands: layers, audio tracks, scene apply (A§4.4 scene rules,
//! A§2.1 defaults). Owner: scene. Stub: signatures only.
//!
//! New ids: `ctx::next_layer_id` / `ctx::next_track_id`. Labels come from
//! `ctx.facts` (screens, cameras, audio devices).
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::ids::{LayerId, TrackId};
use crate::model::{AudioRole, Rect};
use crate::protocol::command::{AudioPatch, LayerPatch, SceneDest, SceneTarget, VideoSourceArg};
use crate::state::State;

/// Only `SceneTarget::Project`; unknown output/device ⇒ `NotFound`.
pub fn layer_add(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    source: VideoSourceArg,
    name: Option<String>,
    rect: Option<Rect>,
) -> Result<Outcome, CoreError> {
    todo!("scene: LayerAdd")
}

pub fn layer_set(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    layer: LayerId,
    patch: LayerPatch,
) -> Result<Outcome, CoreError> {
    todo!("scene: LayerSet")
}

pub fn layer_order(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    layer: LayerId,
    to: usize,
) -> Result<Outcome, CoreError> {
    todo!("scene: LayerOrder")
}

/// On a take: removes the layer from that snapshot only; the stream stays.
pub fn layer_remove(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    layer: LayerId,
) -> Result<Outcome, CoreError> {
    todo!("scene: LayerRemove")
}

/// Only `SceneTarget::Project`; source from `facts.default_mic` /
/// `default_monitor` unless given.
pub fn audio_add(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    role: AudioRole,
    source: Option<String>,
    label: Option<String>,
) -> Result<Outcome, CoreError> {
    todo!("scene: AudioAdd")
}

pub fn audio_set(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    track: TrackId,
    patch: AudioPatch,
) -> Result<Outcome, CoreError> {
    todo!("scene: AudioSet")
}

/// Removing the track named by `autocut.track` also sets it to null (same entry).
pub fn audio_remove(
    state: &State,
    ctx: &Ctx,
    target: SceneTarget,
    track: TrackId,
) -> Result<Outcome, CoreError> {
    todo!("scene: AudioRemove")
}

/// Copies layout fields only, matched by id; never `missing` or sources.
pub fn scene_apply(
    state: &State,
    ctx: &Ctx,
    from: SceneTarget,
    to: SceneDest,
) -> Result<Outcome, CoreError> {
    todo!("scene: SceneApply")
}
