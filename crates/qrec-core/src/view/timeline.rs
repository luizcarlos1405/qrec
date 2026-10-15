//! The timeline (G§4.5, C14–C18): the viewport and its zoom, clip boxes,
//! lanes, silence and autocut hatches, the ghost of a take being recorded,
//! drops from the bin. Owner: WP3. Stub until WP3 lands: the signatures are
//! final, the bodies return defaults.
//!
//! Geometry is in logical px at 100 % (D-46), relative to the time area
//! (the timeline minus the 176 px track headers): a clip starts at
//! `x = start × px_per_frame − scroll_px` and is `len × px_per_frame` wide.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ctx::FileFact;
use crate::ids::{ClipId, Frame, TakeId, TrackId};
use crate::model::{AudioRole, Clip};
use crate::protocol::event::Interval;
use crate::state::{SilenceKey, State};

/// The most a frame may be zoomed to.
pub const MAX_PX_PER_FRAME: f64 = 128.0;
/// From this zoom on the timeline draws single frames (S09).
pub const FRAME_LEVEL_PX: f64 = 24.0;
/// Fit leaves this much room at the end: `(width − 8) / total`.
pub const FIT_INSET_PX: f64 = 8.0;

/// Row heights (C14–C17), regular and compact (height < 820, G§3.15).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rows {
    pub toolbar: f64,
    pub ruler: f64,
    /// The video row: a 4 px stripe and the thumbnails.
    pub video: f64,
    pub lane: f64,
    pub gap: f64,
}

/// The regular rows: toolbar 36, ruler 28, video 76, lanes 56, gaps 4.
pub const ROWS: Rows = Rows {
    toolbar: 36.0,
    ruler: 28.0,
    video: 76.0,
    lane: 56.0,
    gap: 4.0,
};

/// The compact rows: video 72, lanes 48.
pub const COMPACT_ROWS: Rows = Rows {
    toolbar: 36.0,
    ruler: 28.0,
    video: 72.0,
    lane: 48.0,
    gap: 4.0,
};

/// What of the timeline is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    /// The time area's width.
    pub width: f64,
    pub px_per_frame: f64,
    pub scroll_px: f64,
    /// The lanes' vertical scroll (D-25).
    pub lanes_scroll_px: f64,
}

impl Viewport {
    /// The fit viewport of a timeline of `total` frames in a time area
    /// `width` wide: `(width − 8) / total` px per frame, no scroll. `None`
    /// on an empty timeline.
    pub fn fit(width: f64, total: Frame) -> Option<Viewport> {
        let _ = (width, total);
        None
    }

    /// The viewport `ui.zoom` and `ui.scroll_px` describe (fit for `None`).
    pub fn from_state(state: &State, width: f64) -> Option<Viewport> {
        let _ = (state, width);
        None
    }

    /// The x of frame `f`'s left edge.
    pub fn x_of_frame(&self, f: Frame) -> f64 {
        let _ = f;
        0.0
    }

    /// The frame at `x` (the inverse of [`Viewport::x_of_frame`]).
    pub fn frame_at_x(&self, x: f64) -> Frame {
        let _ = x;
        0
    }

    /// Zoomed by `factor`, keeping the frame under `anchor_x` there;
    /// clamped to `[fit, 128]` px per frame.
    pub fn zoom_at(&self, factor: f64, anchor_x: f64, fit: f64) -> Viewport {
        let _ = (factor, anchor_x, fit);
        *self
    }

    /// The scroll after a wheel event (D-39): 64 px per notch, `dx` and
    /// `dy` both scroll horizontally.
    pub fn scroll_for_wheel(&self, dx: f64, dy: f64, total: Frame) -> f64 {
        let _ = (dx, dy, total);
        self.scroll_px
    }

    /// The frame a ruler press or scrub at `x` seeks to, within `[0, total]`.
    pub fn scrub_frame(&self, x: f64, total: Frame) -> Frame {
        let _ = (x, total);
        0
    }
}

/// The zoom readout: "Fit"; "10 px/s" below 8 px per frame; "32 px/frame".
pub fn zoom_readout(viewport: &Viewport, fit: f64, fps: crate::model::Fps) -> String {
    let _ = (viewport, fit, fps);
    String::new()
}

/// What a clip box is labeled with at its width (G§4.5).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "camelCase")]
pub enum ClipLabel {
    /// ≥ 64 px: "Take 5".
    Full(String),
    /// 24–63 px: "5".
    Short(String),
    /// 12–23 px: no label.
    #[default]
    None,
    /// < 12 px: the stripe and a 1 px edge marker.
    Sliver,
}

/// One clip on the video row (C16).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipBox {
    pub clip: ClipId,
    pub take: TakeId,
    pub x: f64,
    pub w: f64,
    pub label: ClipLabel,
    /// Index into `format::TAKE_COLORS`.
    pub color: usize,
    pub selected: bool,
    pub hovered: bool,
    /// A non-dry-run autocut job analyzes it (D-15).
    pub analyzing: bool,
    /// The thumbnail layer's media is missing in its take (C19 hatch).
    pub missing: bool,
    /// A record-over is replacing it (S04b).
    pub recorded_over: bool,
    /// 1, or 0.7 during the countdown and 0.6 while recording or
    /// finalizing (D-43).
    pub opacity: f64,
}

/// The clip boxes of the timeline in `viewport`.
pub fn clip_boxes(state: &State, viewport: &Viewport, hovered: Option<&ClipId>) -> Vec<ClipBox> {
    let _ = (state, viewport, hovered);
    Vec::new()
}

/// One audio lane: a track of the project scene or of a take on the
/// timeline (project-scene order first, then by id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lane {
    pub track: TrackId,
    pub label: String,
    pub role: AudioRole,
    /// The `autocut.track` (scissors).
    pub autocut: bool,
    /// In `ui.lane_mute` (D-08).
    pub muted: bool,
    /// `ui.solo`.
    pub solo: bool,
    /// Drawn with `wave-dim`: muted, or another track is soloed.
    pub dimmed: bool,
}

/// The lanes of `state`.
pub fn lanes(state: &State) -> Vec<Lane> {
    let _ = state;
    Vec::new()
}

/// A known silence on a lane, in timeline frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SilenceSpan {
    pub clip: ClipId,
    pub track: TrackId,
    pub from: Frame,
    pub to: Frame,
}

/// The silences to hatch on the autocut track's lane (S08), at the
/// project's threshold and minimum silence: from `runtime.silence` and the
/// cached analyses, keyed with the files' facts.
pub fn silence_spans(
    state: &State,
    files: &BTreeMap<String, FileFact>,
    cached: &BTreeMap<SilenceKey, Vec<Interval>>,
) -> Vec<SilenceSpan> {
    let _ = (state, files, cached);
    Vec::new()
}

/// The slot a bin drop at `x` inserts at (D-38): the nearest clip
/// boundary, `0..=clips.len()`.
pub fn drop_slot(viewport: &Viewport, clips: &[Clip], x: f64) -> usize {
    let _ = (viewport, clips, x);
    0
}

/// The ghost of the take being recorded (S04, S04b, S05).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ghost {
    pub x: f64,
    pub w: f64,
    /// "● Take 10 · recording" or "Take 10 · saving…".
    pub plate: String,
    /// "over c2d0 (take 5)" for a record-over.
    pub over: Option<String>,
    /// The clips a record-over replaces (hatched `muted-40`).
    pub replaced: Vec<ClipId>,
    /// Finalizing: the animated `record-25` stripes.
    pub saving: bool,
}

/// The ghost while a take is recorded or finalized; `elapsed_ns` is the
/// clock interpolated on the frame clock.
pub fn ghost(state: &State, viewport: &Viewport, elapsed_ns: u64) -> Option<Ghost> {
    let _ = (state, viewport, elapsed_ns);
    None
}
