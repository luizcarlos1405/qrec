//! The autocut planner (A§5.7, SPEC §9). Owner: autocut. Stub: signatures only.
//!
//! Per clip and track: shift file-time silences to take time (`t = x +
//! startOffsetNs + syncOffsetMs·10⁶`), shrink them by `paddingMs`, keep the
//! complement inside the clip, drop fragments shorter than `minKeepMs`,
//! snap outward to frames and merge touching fragments. Nothing kept ⇒ the
//! clip is left unchanged ("entirely silent at −35 dB").
#![allow(clippy::todo, unused_variables)]

use crate::ids::{Frame, TrackId};
use crate::model::{AudioTrack, Clip, Fps, Project, Stream, Take};
use crate::protocol::event::Interval;
use crate::state::AutocutParams;

/// The planned cut of one clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipCut {
    /// Kept fragments as `[in, out)` take frames; empty = entirely silent
    /// (the clip stays as it is).
    pub kept: Vec<[Frame; 2]>,
    /// Silences removed.
    pub silences: usize,
    pub removed_frames: Frame,
}

/// Plans the cut of `clip` from the silences of `stream` (its `track` in the take scene).
pub fn plan_clip(
    clip: &Clip,
    stream: &Stream,
    track: &AudioTrack,
    silences: &[Interval],
    params: &AutocutParams,
    fps: Fps,
) -> ClipCut {
    todo!("autocut: plan_clip")
}

/// The track that drives detection for `take`: the override, else
/// `autocut.track`, else the take scene's first mic, else its first track.
/// `None` when the take lacks it (the clip is skipped with a warning).
pub fn choose_track(
    project: &Project,
    take: &Take,
    override_track: Option<&TrackId>,
) -> Option<TrackId> {
    todo!("autocut: choose_track")
}
