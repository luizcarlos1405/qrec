//! The timeline (G§4.5, C14–C18): the viewport and its zoom, clip boxes,
//! lanes and their segments, thumbnails, silence and autocut hatches, the
//! ghost of a take being recorded, the move and trim previews, the hit
//! test, snapping, drops from the bin, the toolbar, the track headers, the
//! clip tooltip and the empty state. Owner: WP3.
//!
//! Geometry is in logical px at 100 % (D-46), relative to the time area
//! (the timeline minus the 176 px track headers) and, vertically, to the
//! rows below the ruler: a clip starts at `x = start × px_per_frame −
//! scroll_px` and is `len × px_per_frame` wide, with no minimum.
//!
//! - **Fit** is `(width − 8) / total` (an empty timeline has no viewport),
//!   so the playhead parked at the end is never half clipped (README known
//!   gap: never measure a clip's x off a fit-zoom mock). While a take is
//!   recorded the total includes the ghost ([`shown_total`]).
//! - **Zoom** stays within `[fit, 128]` px per frame; `ui.zoom = None` is
//!   fit, with no scroll; frame level starts at 24 px per frame.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::autocut::{analysis_source, to_take_time};
use crate::ctx::FileFact;
use crate::export_plan::shown_frame;
use crate::ids::{ClipId, Frame, TakeId, TrackId};
use crate::keymap::{Action, Keymap, action_for, usable_keys};
use crate::model::{AudioRole, AudioTrack, Clip, Fps, Layer, Stream, StreamKind, Take, TakeStatus};
use crate::protocol::command::{Command, InsertAt, MoveTo, Nullable, UiPatch};
use crate::protocol::event::Interval;
use crate::state::{JobKind, RecordMode, RecordPhase, SilenceKey, State, UiState};
use crate::time::{NS_PER_MS, NS_PER_SEC, TimeArg, ns_to_frame_floor, ns_to_frame_round};
use crate::timeline::{position_of, total_frames};
use crate::view::{HintItem, format};

/// The most a frame may be zoomed to.
pub const MAX_PX_PER_FRAME: f64 = 128.0;
/// From this zoom on the timeline draws single frames (S09).
pub const FRAME_LEVEL_PX: f64 = 24.0;
/// Fit leaves this much room at the end: `(width − 8) / total`.
pub const FIT_INSET_PX: f64 = 8.0;
/// The track header column (C17).
pub const HEADER_WIDTH: f64 = 176.0;
/// A wheel notch scrolls this far (D-39).
pub const WHEEL_NOTCH_PX: f64 = 64.0;
/// `Ctrl`+wheel zooms by this per notch, at the pointer (D-39).
pub const WHEEL_ZOOM: f64 = 1.25;
/// `=` and `−` zoom by this (G§4.5).
pub const ZOOM_STEP: f64 = 2.0;
/// Snapping reaches this far (G§4.5).
pub const SNAP_PX: f64 = 8.0;
/// A move drag starts after this much travel (G§4.5).
pub const MOVE_START_PX: f64 = 4.0;
/// The trim zone inside each clip edge (D-45).
pub const TRIM_ZONE_PX: f64 = 4.0;
/// Clips narrower than this have no trim zones (D-45).
pub const TRIM_ZONE_MIN_CLIP_PX: f64 = 6.0;
/// A filmstrip tile at fit and seconds zoom (C16).
pub const TILE_PX: f64 = 128.0;
/// The take-color stripe on top of a clip (C16).
pub const STRIPE_PX: f64 = 4.0;
/// A waveform reaches this far from the lane's center line (C17).
pub const WAVE_HALF_PX: f64 = 22.0;
/// While playing, the view pages once the playhead passes `width − 8`.
pub const AUTO_PAGE_PX: f64 = 8.0;
/// Why `z` does nothing (D-34; `hint_enabled` shows it disabled, S07).
pub const ZOOM_TO_NOTHING: &str = "Select the clips to zoom to";

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

/// The compact rows: video 72, lanes 48, no gaps (DESIGN Layout: toolbar
/// 36 + ruler 28 + video 72 + two 48 px lanes = the 232 px minimum).
pub const COMPACT_ROWS: Rows = Rows {
    toolbar: 36.0,
    ruler: 28.0,
    video: 72.0,
    lane: 48.0,
    gap: 0.0,
};

impl Rows {
    /// The rows for a compact window or a regular one.
    pub fn for_layout(compact: bool) -> Rows {
        if compact { COMPACT_ROWS } else { ROWS }
    }

    /// The top of lane `i` in the rows area (below the ruler).
    pub fn lane_y(&self, i: usize) -> f64 {
        self.video + self.gap + i as f64 * (self.lane + self.gap)
    }

    /// The rows' height with `lanes` lanes: the video row, then a gap and
    /// a lane per lane.
    pub fn content_height(&self, lanes: usize) -> f64 {
        self.video + lanes as f64 * (self.gap + self.lane)
    }

    /// The filmstrip's height (the video row under its stripe).
    pub fn filmstrip(&self) -> f64 {
        self.video - STRIPE_PX
    }

    /// The rows in whole widget px at UI scale `scale` (D-46): the video
    /// row's height, then each lane's gap above it and its height. Each is
    /// the difference of two rounded tops (`round(y × scale)`), so widgets
    /// stacked at these heights (the track headers, the named rows) stay
    /// on the canvas, which paints the rows at `y × scale`, whatever the
    /// scale and the number of lanes.
    pub fn px_heights(&self, lanes: usize, scale: f64) -> (i32, Vec<(i32, i32)>) {
        let px = |y: f64| (y * scale).round() as i32;
        let video = px(self.video);
        let mut bottom = video;
        let lanes = (0..lanes)
            .map(|i| {
                let top = px(self.lane_y(i));
                let end = px(self.lane_y(i) + self.lane);
                let gap = top - bottom;
                bottom = end;
                (gap, end - top)
            })
            .collect();
        (video, lanes)
    }
}

// ------------------------------------------------------------- viewport

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

/// A little slack for float round trips (`frame_at_x(x_of_frame(f))`).
const FRAME_EPS: f64 = 1e-6;

impl Viewport {
    /// Fit's px per frame for `total` frames in a time area `width` wide:
    /// `(width − 8) / total`; `None` on an empty timeline.
    pub fn fit_px_per_frame(width: f64, total: Frame) -> Option<f64> {
        (total > 0).then(|| (width - FIT_INSET_PX).max(1.0) / total as f64)
    }

    /// The fit viewport of a timeline of `total` frames in a time area
    /// `width` wide: `(width − 8) / total` px per frame, no scroll. `None`
    /// on an empty timeline.
    pub fn fit(width: f64, total: Frame) -> Option<Viewport> {
        Self::fit_px_per_frame(width, total).map(|px_per_frame| Viewport {
            width,
            px_per_frame,
            scroll_px: 0.0,
            lanes_scroll_px: 0.0,
        })
    }

    /// The viewport `ui.zoom` and `ui.scroll_px` describe over `total`
    /// shown frames: fit for `None`; the zoom kept within `[fit, 128]`, the
    /// scroll within the timeline.
    pub fn from_ui(ui: &UiState, width: f64, total: Frame) -> Option<Viewport> {
        let fit = Self::fit_px_per_frame(width, total)?;
        let (px_per_frame, scroll_px) = match ui.zoom {
            None => (fit, 0.0),
            Some(zoom) => (clamp_zoom(zoom, fit), ui.scroll_px),
        };
        Some(
            Viewport {
                width,
                px_per_frame,
                scroll_px,
                lanes_scroll_px: 0.0,
            }
            .clamped(total),
        )
    }

    /// The viewport `ui.zoom` and `ui.scroll_px` describe (fit for `None`),
    /// over the timeline and the ghost of a take being recorded.
    pub fn from_state(state: &State, width: f64) -> Option<Viewport> {
        Self::from_ui(
            &state.ui,
            width,
            shown_total(state, recording_elapsed_ns(state)),
        )
    }

    /// The x of frame `f`'s left edge.
    pub fn x_of_frame(&self, f: Frame) -> f64 {
        f as f64 * self.px_per_frame - self.scroll_px
    }

    /// The x of a fractional frame position.
    pub fn x_of(&self, frames: f64) -> f64 {
        frames * self.px_per_frame - self.scroll_px
    }

    /// The fractional frame position at `x`.
    pub fn time_at_x(&self, x: f64) -> f64 {
        if self.px_per_frame > 0.0 {
            (x + self.scroll_px) / self.px_per_frame
        } else {
            0.0
        }
    }

    /// The frame at `x` (the inverse of [`Viewport::x_of_frame`]).
    pub fn frame_at_x(&self, x: f64) -> Frame {
        let t = self.time_at_x(x) + FRAME_EPS;
        if t <= 0.0 { 0 } else { t.floor() as Frame }
    }

    /// How far the view can scroll over `total` frames: the end shows with
    /// fit's 8 px inset.
    pub fn max_scroll(&self, total: Frame) -> f64 {
        (total as f64 * self.px_per_frame + FIT_INSET_PX - self.width).max(0.0)
    }

    /// The scroll kept within `[0, max_scroll]`.
    pub fn clamped(mut self, total: Frame) -> Viewport {
        self.scroll_px = clamp_scroll(self.scroll_px, self.max_scroll(total));
        self
    }

    /// Zoomed by `factor`, keeping the frame under `anchor_x` there;
    /// clamped to `[fit, 128]` px per frame, and the scroll to the
    /// timeline (`fit` gives its length at this width).
    pub fn zoom_at(&self, factor: f64, anchor_x: f64, fit: f64) -> Viewport {
        self.with_zoom(self.px_per_frame * factor, anchor_x, fit)
    }

    /// At `px_per_frame` (clamped to `[fit, 128]`), keeping the frame under
    /// `anchor_x` there as far as the scroll range allows.
    pub fn with_zoom(&self, px_per_frame: f64, anchor_x: f64, fit: f64) -> Viewport {
        let ppf = clamp_zoom(px_per_frame, fit);
        let anchor = self.time_at_x(anchor_x);
        let scroll = anchor * ppf - anchor_x;
        let max = if fit > 0.0 {
            ((self.width - FIT_INSET_PX).max(1.0) * (ppf / fit - 1.0)).max(0.0)
        } else {
            f64::INFINITY
        };
        Viewport {
            px_per_frame: ppf,
            scroll_px: clamp_scroll(scroll, max),
            ..*self
        }
    }

    /// The scroll after a wheel event (D-39): 64 px per notch, `dx` and
    /// `dy` (in notches) both scroll horizontally.
    pub fn scroll_for_wheel(&self, dx: f64, dy: f64, total: Frame) -> f64 {
        clamp_scroll(
            self.scroll_px + (dx + dy) * WHEEL_NOTCH_PX,
            self.max_scroll(total),
        )
    }

    /// The frame a ruler press or scrub at `x` seeks to, within `[0, total]`.
    pub fn scrub_frame(&self, x: f64, total: Frame) -> Frame {
        self.frame_at_x(x).min(total)
    }

    /// The frames on screen, `[first, last)` (fractional).
    pub fn visible_frames(&self) -> (f64, f64) {
        (self.time_at_x(0.0), self.time_at_x(self.width))
    }

    /// At frame level (≥ 24 px per frame): one thumbnail and one ruler cell
    /// per frame (S09).
    pub fn is_frame_level(&self) -> bool {
        self.px_per_frame >= FRAME_LEVEL_PX - FRAME_EPS
    }
}

fn clamp_scroll(scroll: f64, max: f64) -> f64 {
    if scroll.is_finite() {
        scroll.clamp(0.0, max.max(0.0))
    } else {
        0.0
    }
}

/// A zoom within `[fit, 128]` (just `fit` for a timeline so short that fit
/// is closer than 128).
pub fn clamp_zoom(px_per_frame: f64, fit: f64) -> f64 {
    let top = MAX_PX_PER_FRAME.max(fit);
    if px_per_frame.is_finite() {
        px_per_frame.clamp(fit, top)
    } else {
        fit
    }
}

/// Whether `px_per_frame` is fit's.
pub fn is_fit(px_per_frame: f64, fit: f64) -> bool {
    (px_per_frame - fit).abs() <= fit * 1e-9 + 1e-12
}

/// The frames the timeline shows: its clips, plus what the take being
/// recorded adds (G§4.5: fit includes the ghost). `elapsed_ns` is the
/// record clock (the GUI keeps the last one through finalizing).
pub fn shown_total(state: &State, elapsed_ns: u64) -> Frame {
    let total = total_frames(&state.project.timeline.clips);
    match ghost_slot(state) {
        None => total,
        Some(slot) => {
            let ghost = ghost_frames(elapsed_ns, state.project.canvas.fps);
            total.saturating_add(ghost.saturating_sub(slot.replaced_len))
        }
    }
}

/// The record clock in `state` (0 unless `Recording`).
pub fn recording_elapsed_ns(state: &State) -> u64 {
    match &state.runtime.record {
        RecordPhase::Recording { elapsed_ns, .. } => *elapsed_ns,
        _ => 0,
    }
}

/// The frames a take has after `elapsed_ns` (`ns_to_frame_floor`).
pub fn ghost_frames(elapsed_ns: u64, fps: Fps) -> Frame {
    ns_to_frame_floor(i64::try_from(elapsed_ns).unwrap_or(i64::MAX), fps).max(0) as Frame
}

/// The zoom readout: "Fit"; "10 px/s" below 8 px per frame; "32 px/frame".
pub fn zoom_readout(viewport: &Viewport, fit: f64, fps: Fps) -> String {
    let ppf = viewport.px_per_frame;
    if is_fit(ppf, fit) {
        "Fit".to_owned()
    } else if ppf < 8.0 {
        format!("{} px/s", (ppf * fps.as_f64()).round())
    } else {
        format!("{} px/frame", ppf.round())
    }
}

/// The zoom slider's position for `px_per_frame`: `s` with `fit · (128 /
/// fit)^s = px_per_frame`.
pub fn slider_value(px_per_frame: f64, fit: f64) -> f64 {
    let span = (MAX_PX_PER_FRAME / fit).ln();
    if !(span.is_finite() && span > 0.0) {
        return 0.0;
    }
    ((px_per_frame / fit).ln() / span).clamp(0.0, 1.0)
}

/// The zoom at slider position `s ∈ [0, 1]`: `fit · (128 / fit)^s`.
pub fn slider_zoom(s: f64, fit: f64) -> f64 {
    clamp_zoom(
        fit * (MAX_PX_PER_FRAME / fit).max(1.0).powf(s.clamp(0.0, 1.0)),
        fit,
    )
}

/// `=` (zoom in) or `−` (zoom out): ×2 or ÷2, clamped, anchored at the
/// playhead when it is on screen, else at the viewport's center.
pub fn zoom_step(viewport: &Viewport, fit: f64, playhead: Frame, zoom_in: bool) -> Viewport {
    let px = viewport.x_of_frame(playhead);
    let anchor = if (0.0..=viewport.width).contains(&px) {
        px
    } else {
        viewport.width / 2.0
    };
    let factor = if zoom_in { ZOOM_STEP } else { 1.0 / ZOOM_STEP };
    viewport.zoom_at(factor, anchor, fit)
}

/// Whether `=` can zoom further in.
pub fn can_zoom_in(viewport: &Viewport, fit: f64) -> bool {
    viewport.px_per_frame < MAX_PX_PER_FRAME.max(fit) * (1.0 - 1e-9)
}

/// Whether `−` can zoom further out.
pub fn can_zoom_out(viewport: &Viewport, fit: f64) -> bool {
    !is_fit(viewport.px_per_frame, fit) && viewport.px_per_frame > fit
}

/// `z` (G§4.5): the selection fills the time area exactly, no margin:
/// `width ÷` the selection's frame span, scrolled to its start (S12: 1040 /
/// 3105 = 0.335 px/frame). When that is past 128 px per frame, the
/// selection is centered. `Err` with nothing selected.
pub fn zoom_to_selection(
    state: &State,
    viewport: &Viewport,
    total: Frame,
) -> Result<Viewport, String> {
    let clips = &state.project.timeline.clips;
    let mut span: Option<(Frame, Frame)> = None;
    let mut start = 0;
    for clip in clips {
        let end = start + clip.len();
        if state.ui.selection.contains(&clip.id) {
            span = Some(match span {
                None => (start, end),
                Some((a, _)) => (a, end),
            });
        }
        start = end;
    }
    let Some((from, to)) = span.filter(|(a, b)| b > a) else {
        return Err(ZOOM_TO_NOTHING.to_owned());
    };
    let fit = Viewport::fit_px_per_frame(viewport.width, total).unwrap_or(viewport.px_per_frame);
    let len = (to - from) as f64;
    let wanted = viewport.width / len;
    let ppf = clamp_zoom(wanted, fit);
    let scroll = if (ppf - wanted).abs() <= wanted * 1e-12 {
        from as f64 * ppf
    } else {
        (from as f64 + len / 2.0) * ppf - viewport.width / 2.0
    };
    Ok(Viewport {
        px_per_frame: ppf,
        scroll_px: scroll,
        ..*viewport
    }
    .clamped(total))
}

/// What `SetUi` stores for `viewport`: fit is `zoom: null` with no
/// scroll; otherwise the zoom and the scroll.
pub fn ui_patch(viewport: &Viewport, fit: f64) -> UiPatch {
    if is_fit(viewport.px_per_frame, fit) {
        UiPatch {
            zoom: Nullable::Clear,
            scroll_px: Some(0.0),
            ..UiPatch::default()
        }
    } else {
        UiPatch {
            zoom: Nullable::Set(viewport.px_per_frame),
            scroll_px: Some(viewport.scroll_px),
            ..UiPatch::default()
        }
    }
}

/// While playing, the scroll that pages the view once the playhead passes
/// the right edge − 8 px: it lands 8 px from the left edge. `None` while it
/// is on screen.
pub fn auto_page(viewport: &Viewport, playhead: Frame, total: Frame) -> Option<f64> {
    let x = viewport.x_of_frame(playhead);
    if x <= viewport.width - AUTO_PAGE_PX && x >= 0.0 {
        return None;
    }
    let scroll = clamp_scroll(
        playhead as f64 * viewport.px_per_frame - AUTO_PAGE_PX,
        viewport.max_scroll(total),
    );
    ((scroll - viewport.scroll_px).abs() > 0.5).then_some(scroll)
}

/// The frame the playhead is drawn at: while a take is recorded, the
/// ghost's end (the newest waveform is where the playhead is parked);
/// while the player plays, its frame; otherwise `ui.playhead`, at most the
/// timeline's end, as the transport reads it (`view::canvas::transport`;
/// the WP7 integration audit saw the ruler's corner read a playhead past
/// the end while the transport read the end).
pub fn shown_playhead(state: &State, elapsed_ns: u64, player_frame: Option<Frame>) -> Frame {
    if let Some(slot) = ghost_slot(state) {
        return slot.start + ghost_frames(elapsed_ns, state.project.canvas.fps);
    }
    player_frame
        .unwrap_or(state.ui.playhead)
        .min(total_frames(&state.project.timeline.clips))
}

// ------------------------------------------------------------- clips

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

impl ClipLabel {
    /// The label of a clip of `take` that is `w` px wide.
    pub fn for_width(take: &TakeId, w: f64) -> ClipLabel {
        if w >= 64.0 {
            ClipLabel::Full(format!("Take {}", take.number()))
        } else if w >= 24.0 {
            ClipLabel::Short(take.number().to_string())
        } else if w >= 12.0 {
            ClipLabel::None
        } else {
            ClipLabel::Sliver
        }
    }

    /// How much it shows: sliver 0, none 1, the number 2, "Take N" 3.
    pub fn rank(&self) -> u8 {
        match self {
            ClipLabel::Sliver => 0,
            ClipLabel::None => 1,
            ClipLabel::Short(_) => 2,
            ClipLabel::Full(_) => 3,
        }
    }

    /// The plate's text, if it has one.
    pub fn text(&self) -> Option<&str> {
        match self {
            ClipLabel::Full(t) | ClipLabel::Short(t) => Some(t),
            ClipLabel::None | ClipLabel::Sliver => None,
        }
    }
}

/// A clip edge being trimmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrimEdge {
    Start,
    End,
}

/// One clip on the video row (C16).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipBox {
    pub clip: ClipId,
    pub take: TakeId,
    pub x: f64,
    pub w: f64,
    /// The take frame at `x` (the clip's `in`, or a trim preview's new one).
    pub in_frame: Frame,
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
    /// The edge a trim drag moves (its 6 px `accent` bar).
    pub trimming: Option<TrimEdge>,
}

/// The clip boxes of the timeline in `viewport`.
pub fn clip_boxes(state: &State, viewport: &Viewport, hovered: Option<&ClipId>) -> Vec<ClipBox> {
    clip_layout(state, viewport, hovered, recording_elapsed_ns(state))
}

/// The clip boxes with the ghost `elapsed_ns` long: a record-over pushes
/// the clips after the replaced run once the ghost outgrows it.
pub fn clip_layout(
    state: &State,
    viewport: &Viewport,
    hovered: Option<&ClipId>,
    elapsed_ns: u64,
) -> Vec<ClipBox> {
    let clips = &state.project.timeline.clips;
    let push = ghost_slot(state)
        .filter(|slot| slot.replaced_len > 0)
        .map(|slot| {
            let ghost = ghost_frames(elapsed_ns, state.project.canvas.fps);
            (
                slot.start + slot.replaced_len,
                ghost.saturating_sub(slot.replaced_len),
            )
        });
    let mut start: Frame = 0;
    clips
        .iter()
        .map(|clip| {
            let mut at = start;
            if let Some((after, by)) = push
                && at >= after
            {
                at += by;
            }
            start += clip.len();
            clip_box(state, viewport, clip, at, hovered)
        })
        .collect()
}

/// The box of `clip` starting at timeline frame `start`.
fn clip_box(
    state: &State,
    viewport: &Viewport,
    clip: &Clip,
    start: Frame,
    hovered: Option<&ClipId>,
) -> ClipBox {
    let x = viewport.x_of_frame(start);
    let w = clip.len() as f64 * viewport.px_per_frame;
    ClipBox {
        clip: clip.id.clone(),
        take: clip.take.clone(),
        x,
        w,
        in_frame: clip.in_,
        label: ClipLabel::for_width(&clip.take, w),
        color: format::take_color(&clip.take),
        selected: state.ui.selection.contains(&clip.id),
        hovered: hovered == Some(&clip.id),
        analyzing: analyzing(state, &clip.id),
        missing: state
            .project
            .takes
            .get(&clip.take)
            .is_some_and(thumbnail_missing),
        recorded_over: recorded_over(state).contains(&clip.id),
        opacity: clip_opacity(state),
        trimming: None,
    }
}

/// D-43: 0.7 during the countdown, 0.6 while recording or finalizing.
pub fn clip_opacity(state: &State) -> f64 {
    match state.runtime.record {
        RecordPhase::Idle => 1.0,
        RecordPhase::Countdown { .. } => 0.7,
        _ => 0.6,
    }
}

fn analyzing(state: &State, clip: &ClipId) -> bool {
    state.runtime.jobs.values().any(|job| match &job.kind {
        JobKind::Autocut {
            dry_run: false,
            targets,
            ..
        } => targets.iter().any(|t| &t.clip.id == clip),
        _ => false,
    })
}

/// The clips a record-over replaces while it records (S04b).
fn recorded_over(state: &State) -> BTreeSet<ClipId> {
    match &state.runtime.record {
        RecordPhase::Recording {
            mode: RecordMode::Over { clips },
            ..
        }
        | RecordPhase::Finalizing {
            mode: RecordMode::Over { clips },
            cancelled: false,
            ..
        } => clips.iter().map(|c| c.id.clone()).collect(),
        _ => BTreeSet::new(),
    }
}

/// D-43: whether gestures that edit the timeline (trims, moves, drops)
/// start; why not otherwise ("Not while recording").
pub fn edits_enabled(state: &State) -> Result<(), String> {
    crate::view::enabled(state, &Command::ClipRemove { clips: Vec::new() })
}

// ------------------------------------------------------------- the ghost

/// Where the take being recorded goes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GhostSlot {
    take: TakeId,
    /// The timeline frame it starts at.
    start: Frame,
    /// The frames of the clips it replaces (record over).
    replaced_len: Frame,
    replaced: Vec<ClipId>,
    saving: bool,
}

/// The ghost's slot while a take records or finalizes (not after `Esc`):
/// the end, or the first replaced clip's start for a record-over whose
/// clips are still where they were.
fn ghost_slot(state: &State) -> Option<GhostSlot> {
    let (take, mode, saving) = match &state.runtime.record {
        RecordPhase::Recording { take, mode, .. } => (take, mode, false),
        RecordPhase::Finalizing {
            take,
            mode,
            cancelled: false,
            ..
        } => (take, mode, true),
        _ => return None,
    };
    let clips = &state.project.timeline.clips;
    let append = GhostSlot {
        take: take.clone(),
        start: total_frames(clips),
        replaced_len: 0,
        replaced: Vec::new(),
        saving,
    };
    let RecordMode::Over { clips: over } = mode else {
        return Some(append);
    };
    let indices: Option<Vec<usize>> = over
        .iter()
        .map(|r| clips.iter().position(|c| r.matches(c)))
        .collect();
    let Some(mut indices) = indices.filter(|i| !i.is_empty()) else {
        return Some(append);
    };
    indices.sort_unstable();
    let first = indices[0];
    let contiguous = indices.windows(2).all(|w| w[1] == w[0] + 1);
    if !contiguous {
        return Some(append);
    }
    Some(GhostSlot {
        take: take.clone(),
        start: position_of(clips, first),
        replaced_len: indices.iter().map(|&i| clips[i].len()).sum(),
        replaced: indices.iter().map(|&i| clips[i].id.clone()).collect(),
        saving,
    })
}

/// The ghost of the take being recorded (S04, S04b, S05).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ghost {
    pub take: TakeId,
    /// Index into `format::TAKE_COLORS` (its stripe).
    pub color: usize,
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
    let slot = ghost_slot(state)?;
    let frames = ghost_frames(elapsed_ns, state.project.canvas.fps);
    let n = slot.take.number();
    let plate = if slot.saving {
        format!("Take {n} \u{00b7} saving\u{2026}")
    } else {
        format!("\u{25cf} Take {n} \u{00b7} recording")
    };
    let over = match slot.replaced.as_slice() {
        [] => None,
        [one] => {
            let take = state
                .project
                .clip(one)
                .map(|c| c.take.number())
                .unwrap_or_default();
            Some(format!("over {one} (take {take})"))
        }
        many => Some(format!("over {} clips", many.len())),
    };
    Some(Ghost {
        color: format::take_color(&slot.take),
        take: slot.take,
        x: viewport.x_of_frame(slot.start),
        w: frames as f64 * viewport.px_per_frame,
        plate,
        over,
        replaced: slot.replaced,
        saving: slot.saving,
    })
}

/// The px columns of a ghost's live waveform that show (G§4.5): column `c`
/// counts from the ghost's right edge leftward (0 is the newest) and is at
/// x = ⌊x + w⌋ − 1 − c; only those in the time area, `[0, width)`, are
/// drawn, so a long take zoomed in costs one view of columns, not its
/// whole length.
#[derive(Debug, Clone, PartialEq)]
pub struct GhostColumns {
    /// The leftmost visible column's x.
    pub x0: f64,
    /// The visible columns, counted from the right edge: in screen order
    /// they run from `columns.end − 1` (at `x0`) down to `columns.start`.
    pub columns: std::ops::Range<usize>,
}

/// The columns of `ghost` that show in a time area `width` wide; `None`
/// when none does.
pub fn ghost_columns(ghost: &Ghost, width: f64) -> Option<GhostColumns> {
    let (x, w) = (ghost.x, ghost.w);
    if !(w > 0.0 && x.is_finite() && w.is_finite()) || x >= width || x + w <= 0.0 {
        return None;
    }
    let right = (x + w).floor();
    let all = w.ceil();
    // x = right − 1 − c ≥ 0 and < width.
    let end = all.min(right.max(0.0));
    let start = (right - width).floor().max(0.0);
    (start < end).then_some(GhostColumns {
        x0: right - end,
        columns: start as usize..end as usize,
    })
}

// ------------------------------------------------------------- lanes

/// One audio lane: a track of the project scene or of a take on the
/// timeline (project-scene order first, then by id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lane {
    pub track: TrackId,
    pub label: String,
    pub role: AudioRole,
    /// Autocut listens to it (scissors): `autocut.track`, else a track the
    /// timeline's clips are analyzed on (`autocut::analyzed_tracks`: each
    /// take's first mic, else its first track; takes of other scenes may
    /// put the scissors on several lanes, R77's audit).
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
    let project = &state.project;
    let mut tracks: Vec<&AudioTrack> = project.scene.audio.iter().collect();
    let mut others: BTreeMap<&TrackId, &AudioTrack> = BTreeMap::new();
    for clip in &project.timeline.clips {
        let Some(take) = project.takes.get(&clip.take) else {
            continue;
        };
        for track in &take.scene.audio {
            if !tracks.iter().any(|t| t.id == track.id) {
                others.entry(&track.id).or_insert(track);
            }
        }
    }
    tracks.extend(others.into_values());
    let autocut = autocut_lanes(state);
    tracks
        .into_iter()
        .map(|track| {
            let muted = state.ui.lane_mute.contains(&track.id);
            let solo = state.ui.solo.as_ref() == Some(&track.id);
            Lane {
                track: track.id.clone(),
                label: track.label.clone(),
                role: track.role,
                autocut: autocut.contains(&track.id),
                muted,
                solo,
                dimmed: muted || (state.ui.solo.is_some() && !solo),
            }
        })
        .collect()
}

/// The lanes with the scissors: `autocut.track`, else the tracks the dry
/// run analyzes the timeline's clips on (`autocut::analyzed_tracks`), else,
/// with no clip, the project scene's (what a take recorded from it is
/// analyzed on). The lanes' own rule (the first mic among every lane) put
/// them on a track another take's clips aren't analyzed on (R77's audit).
fn autocut_lanes(state: &State) -> Vec<TrackId> {
    let project = &state.project;
    if let Some(track) = &project.autocut.track {
        return vec![track.clone()];
    }
    let analyzed = crate::autocut::analyzed_tracks(project, &project.timeline.clips, None);
    if !analyzed.is_empty() {
        return analyzed;
    }
    crate::autocut::default_track(&project.scene)
        .map(|t| vec![t.id.clone()])
        .unwrap_or_default()
}

/// One clip's part of a lane (C17).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneSegment {
    pub clip: ClipId,
    pub take: TakeId,
    pub x: f64,
    pub w: f64,
    /// Index into `format::TAKE_COLORS` (the 12 % tint).
    pub color: usize,
    /// The take's file for this track; `None` when the take has no media
    /// for it (no track, a missing one, or the take still records).
    pub file: Option<String>,
    /// The file time at `x`, in ms (negative before the file starts): the
    /// take time minus the stream's start offset and the track's sync
    /// offset, so the waveform sits where it plays.
    pub file_ms_at_x: f64,
    /// File ms per px.
    pub ms_per_px: f64,
    /// `wave-dim` (the lane is muted, or another one soloed).
    pub dimmed: bool,
    /// The clip is selected (the `selected` tint).
    pub selected: bool,
}

/// The segments of `lane` under `boxes` (the clip boxes as drawn).
pub fn lane_segments(
    state: &State,
    viewport: &Viewport,
    boxes: &[ClipBox],
    lane: &Lane,
) -> Vec<LaneSegment> {
    let fps = state.project.canvas.fps;
    let frame_ms = 1000.0 * f64::from(fps.den) / f64::from(fps.num);
    let recording = state.runtime.record.take();
    boxes
        .iter()
        .map(|b| {
            let take = state.project.takes.get(&b.take);
            let media = take
                .filter(|_| recording != Some(&b.take))
                .and_then(|t| audio_media(t, &lane.track));
            let (file, file_ms_at_x) = match media {
                Some((track, stream)) => {
                    let shift_ms = stream.start_offset_ns as f64 / NS_PER_MS as f64
                        + f64::from(track.sync_offset_ms);
                    (
                        Some(stream.file.clone()),
                        b.in_frame as f64 * frame_ms - shift_ms,
                    )
                }
                None => (None, 0.0),
            };
            LaneSegment {
                clip: b.clip.clone(),
                take: b.take.clone(),
                x: b.x,
                w: b.w,
                color: b.color,
                file,
                file_ms_at_x,
                ms_per_px: frame_ms / viewport.px_per_frame,
                dimmed: lane.dimmed,
                selected: b.selected,
            }
        })
        .collect()
}

/// `take`'s track `track` and its audio stream, unless it is missing.
fn audio_media<'t>(take: &'t Take, track: &TrackId) -> Option<(&'t AudioTrack, &'t Stream)> {
    let t = take.scene.track(track).filter(|t| !t.missing)?;
    let stream = take
        .stream(track.as_str())
        .filter(|s| s.kind == StreamKind::Audio)?;
    Some((t, stream))
}

// ------------------------------------------------------------- thumbnails

/// The layer whose frames a clip of `take` shows (Q4): the topmost visible
/// layer of the take scene.
pub fn thumbnail_layer(take: &Take) -> Option<&Layer> {
    take.scene.layers.iter().rev().find(|l| l.visible)
}

/// The thumbnail layer's stream, when it has media.
pub fn thumbnail_stream(take: &Take) -> Option<&Stream> {
    let layer = thumbnail_layer(take)?;
    if layer.missing {
        return None;
    }
    take.stream(layer.id.as_str())
        .filter(|s| s.kind == StreamKind::Video)
}

/// The thumbnail layer is there but its media isn't (C16's missing clip).
fn thumbnail_missing(take: &Take) -> bool {
    thumbnail_layer(take).is_some() && thumbnail_stream(take).is_none()
}

/// What a filmstrip tile shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ThumbImage {
    /// The strip image at file second `second` (fit and seconds zoom).
    Strip { file: String, second: u32 },
    /// Recorded frame `recorded` (frame level), with the strip image of
    /// `second` while it loads.
    Frame {
        file: String,
        recorded: u64,
        second: u32,
    },
}

/// One filmstrip tile of a clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbTile {
    pub x: f64,
    pub w: f64,
    pub image: ThumbImage,
}

/// The visible filmstrip tiles of `clip_box`: 128 px tiles from the clip's
/// x, each showing the strip image at the take time of its left edge (file
/// time = take time − the stream's start offset, nearest second); at
/// frame level one tile per frame, its recorded frame by `shown_frame`.
/// None when the clip's take has no thumbnail media.
pub fn thumb_tiles(state: &State, viewport: &Viewport, clip_box: &ClipBox) -> Vec<ThumbTile> {
    let Some(take) = state.project.takes.get(&clip_box.take) else {
        return Vec::new();
    };
    if state.runtime.record.take() == Some(&clip_box.take) {
        return Vec::new();
    }
    let Some(stream) = thumbnail_stream(take) else {
        return Vec::new();
    };
    let fps = state.project.canvas.fps;
    let (left, right) = (
        clip_box.x.max(0.0),
        (clip_box.x + clip_box.w).min(viewport.width),
    );
    if right <= left {
        return Vec::new();
    }
    let ppf = viewport.px_per_frame;
    let second_of = |take_frame: f64| -> u32 {
        let take_s = take_frame * f64::from(fps.den) / f64::from(fps.num);
        let file_s = take_s - stream.start_offset_ns as f64 / NS_PER_SEC as f64;
        file_s.round().max(0.0) as u32
    };
    let mut tiles = Vec::new();
    if viewport.is_frame_level() {
        let first = ((left - clip_box.x) / ppf).floor().max(0.0) as u64;
        let last = ((right - clip_box.x) / ppf).ceil() as u64;
        let recorded_fps = stream.fps.unwrap_or(fps);
        for j in first..last {
            let x = clip_box.x + j as f64 * ppf;
            let w = ppf.min(clip_box.x + clip_box.w - x);
            if w <= 0.0 {
                break;
            }
            let take_frame = clip_box.in_frame + j;
            let recorded = shown_frame(recorded_fps, stream.start_offset_ns, fps, take_frame as i64)
                .max(0) as u64;
            tiles.push(ThumbTile {
                x,
                w,
                image: ThumbImage::Frame {
                    file: stream.file.clone(),
                    recorded,
                    second: second_of(take_frame as f64),
                },
            });
        }
    } else {
        let first = ((left - clip_box.x) / TILE_PX).floor().max(0.0) as u64;
        let last = ((right - clip_box.x) / TILE_PX).ceil() as u64;
        for i in first..last {
            let x = clip_box.x + i as f64 * TILE_PX;
            let w = TILE_PX.min(clip_box.x + clip_box.w - x);
            if w <= 0.0 {
                break;
            }
            let take_frame = clip_box.in_frame as f64 + i as f64 * TILE_PX / ppf;
            tiles.push(ThumbTile {
                x,
                w,
                image: ThumbImage::Strip {
                    file: stream.file.clone(),
                    second: second_of(take_frame),
                },
            });
        }
    }
    tiles
}

// ------------------------------------------------------------- hatches

/// A known silence on a lane, in timeline frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SilenceSpan {
    pub clip: ClipId,
    pub track: TrackId,
    pub from: Frame,
    pub to: Frame,
}

/// The audio files whose silences the overlay can show: the analysis
/// source of every clip's take (the GUI stats them and asks the preview
/// cache for their cached analyses).
pub fn silence_files(state: &State) -> Vec<String> {
    let project = &state.project;
    let mut files = BTreeSet::new();
    for clip in &project.timeline.clips {
        if state.runtime.record.take() == Some(&clip.take) {
            continue;
        }
        if let Some(take) = project.takes.get(&clip.take)
            && let Ok((_, stream)) = analysis_source(project, &clip.take, take, None)
        {
            files.insert(stream.file.clone());
        }
    }
    files.into_iter().collect()
}

/// The silences to hatch on the autocut track's lane (S08), at the
/// project's threshold and minimum silence: from `runtime.silence` and the
/// cached analyses, keyed with the files' facts. Each is the analysis
/// interval in timeline frames (rounded), within its clip.
pub fn silence_spans(
    state: &State,
    files: &BTreeMap<String, FileFact>,
    cached: &BTreeMap<SilenceKey, Vec<Interval>>,
) -> Vec<SilenceSpan> {
    let project = &state.project;
    let fps = project.canvas.fps;
    let settings = &project.autocut;
    let mut spans = Vec::new();
    let mut start: Frame = 0;
    for clip in &project.timeline.clips {
        let pos = start;
        start += clip.len();
        if state.runtime.record.take() == Some(&clip.take) {
            continue;
        }
        let Some(take) = project.takes.get(&clip.take) else {
            continue;
        };
        let Ok((track, stream)) = analysis_source(project, &clip.take, take, None) else {
            continue;
        };
        let Some(fact) = files.get(&stream.file) else {
            continue;
        };
        let key = SilenceKey::compute(
            &stream.file,
            fact.bytes,
            fact.mtime_ns,
            settings.threshold_db,
            settings.min_silence_ms,
        );
        let Some(found) = state.runtime.silence.get(&key).or_else(|| cached.get(&key)) else {
            continue;
        };
        for silence in to_take_time(found, stream, track) {
            let from = ns_to_frame_round(silence.start_ns, fps).max(clip.in_ as i64) as Frame;
            let to = ns_to_frame_round(silence.end_ns, fps).min(clip.out as i64);
            if to <= from as i64 {
                continue;
            }
            spans.push(SilenceSpan {
                clip: clip.id.clone(),
                track: track.id.clone(),
                from: pos + (from - clip.in_),
                to: pos + (to as Frame - clip.in_),
            });
        }
    }
    spans
}

// ------------------------------------------------------------- drops

/// The slot a bin drop at `x` inserts at (D-38): the nearest clip
/// boundary, `0..=clips.len()` (the lower one on a tie).
pub fn drop_slot(viewport: &Viewport, clips: &[Clip], x: f64) -> usize {
    let mut best = (0, f64::INFINITY);
    let mut at: Frame = 0;
    for slot in 0..=clips.len() {
        let distance = (viewport.x_of_frame(at) - x).abs();
        if distance < best.1 {
            best = (slot, distance);
        }
        if let Some(clip) = clips.get(slot) {
            at += clip.len();
        }
    }
    best.0
}

/// The x of clip boundary `slot` (`0..=clips.len()`).
pub fn boundary_x(viewport: &Viewport, clips: &[Clip], slot: usize) -> f64 {
    viewport.x_of_frame(position_of(clips, slot))
}

/// A bin drop's tooltip: "Insert take 4 · slot 3 of 11".
pub fn drop_tooltip(take: &TakeId, slot: usize, clips: usize) -> String {
    format!(
        "Insert take {} \u{00b7} slot {} of {}",
        take.number(),
        slot + 1,
        clips + 1
    )
}

/// A bin drop's command: `ClipAdd{take, at: Index(slot)}`.
pub fn drop_command(take: &TakeId, slot: usize) -> Command {
    Command::ClipAdd {
        take: take.clone(),
        at: InsertAt::Index(slot),
    }
}

// ------------------------------------------------------------- snapping

/// Where snapping can land (G§4.5): the playhead and every clip boundary,
/// but `exclude` (the dragged edges).
pub fn snap_targets(state: &State, playhead: Frame, exclude: &[Frame]) -> Vec<Frame> {
    let mut targets: BTreeSet<Frame> = crate::timeline::edit_points(&state.project.timeline.clips)
        .into_iter()
        .collect();
    targets.insert(playhead);
    targets
        .into_iter()
        .filter(|t| !exclude.contains(t))
        .collect()
}

/// The target within 8 px of `candidate` (fractional frames), the nearest
/// one, if any.
pub fn snap(candidate: f64, targets: &[Frame], px_per_frame: f64) -> Option<Frame> {
    targets
        .iter()
        .map(|&t| (t, (t as f64 - candidate).abs() * px_per_frame))
        .filter(|(_, px)| *px <= SNAP_PX)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(t, _)| t)
}

// ------------------------------------------------------------- previews

/// Where a ripple tag ("← 00:00:00:29") sits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TagAnchor {
    /// Centered on `x`, on the video row's lower edge (S11).
    Center { x: f64 },
    /// After the label plate of `clip`, on the label's line (S10).
    AfterLabel { clip: ClipId },
}

/// The tag on clips a gesture shifts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RippleTag {
    /// "← 00:00:15:07" or "→ 00:00:00:12".
    pub text: String,
    pub anchor: TagAnchor,
}

fn ripple_text(delta: i64, fps: Fps) -> String {
    let arrow = if delta < 0 { '\u{2190}' } else { '\u{2192}' };
    format!(
        "{arrow} {}",
        format::timecode(delta.unsigned_abs() as Frame, fps)
    )
}

/// A move drag (S11): where the block would land and how the timeline
/// looks meanwhile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovePreview {
    /// The moved clips, in timeline order.
    pub block: Vec<ClipId>,
    /// Where they go: an index into the clips without them.
    pub slot: usize,
    /// How many slots there are (the other clips + 1).
    pub slots: usize,
    /// The insertion caret (C18).
    pub caret_x: f64,
    /// The timeline frame the block would start at.
    pub starts_at: Frame,
    /// "Move after c1b6 · slot 7 of 10", "Starts at 00:00:54:07".
    pub tooltip: [String; 2],
    /// The timeline order after the move.
    pub order: Vec<ClipId>,
    /// Every clip but the block, where it sits during the drag: the others
    /// close the block's old place and leave its new one open.
    pub boxes: Vec<ClipBox>,
    pub ripple: Option<RippleTag>,
    /// `ClipMove{clips: block, to: Index(slot)}`; `None` when the block
    /// would land where it is.
    pub command: Option<Command>,
}

/// The preview of moving `block` with the pointer at `pointer_x`: the slot
/// is the boundary of the other clips nearest to the pointer. `None` when
/// no clip of `block` is on the timeline.
pub fn move_preview(
    state: &State,
    viewport: &Viewport,
    block: &[ClipId],
    pointer_x: f64,
) -> Option<MovePreview> {
    let clips = &state.project.timeline.clips;
    let wanted: BTreeSet<&ClipId> = block.iter().collect();
    let moved: Vec<&Clip> = clips.iter().filter(|c| wanted.contains(&c.id)).collect();
    if moved.is_empty() {
        return None;
    }
    let rest: Vec<&Clip> = clips.iter().filter(|c| !wanted.contains(&c.id)).collect();
    let block_len: Frame = moved.iter().map(|c| c.len()).sum();
    let mut bounds = Vec::with_capacity(rest.len() + 1);
    let mut at: Frame = 0;
    bounds.push(at);
    for clip in &rest {
        at += clip.len();
        bounds.push(at);
    }
    let slot = bounds
        .iter()
        .enumerate()
        .map(|(i, &f)| (i, (viewport.x_of_frame(f) - pointer_x).abs()))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(0, |(i, _)| i);
    let mut order: Vec<ClipId> = rest[..slot].iter().map(|c| c.id.clone()).collect();
    order.extend(moved.iter().map(|c| c.id.clone()));
    order.extend(rest[slot..].iter().map(|c| c.id.clone()));
    let current: Vec<&ClipId> = clips.iter().map(|c| &c.id).collect();
    let unchanged = order.iter().eq(current.iter().copied());
    let block_ids: Vec<ClipId> = moved.iter().map(|c| c.id.clone()).collect();
    let fps = state.project.canvas.fps;
    let slots = rest.len() + 1;
    let first_line = if slot == 0 {
        format!("Move to the start \u{00b7} slot 1 of {slots}")
    } else {
        format!(
            "Move after {} \u{00b7} slot {} of {slots}",
            rest[slot - 1].id,
            slot + 1
        )
    };
    let boxes: Vec<ClipBox> = rest
        .iter()
        .enumerate()
        .map(|(k, clip)| {
            let start = bounds[k] + if k >= slot { block_len } else { 0 };
            clip_box(state, viewport, clip, start, None)
        })
        .collect();
    // The clips whose start changes once the block lands.
    let mut old_start = BTreeMap::new();
    let mut s: Frame = 0;
    for clip in clips {
        old_start.insert(&clip.id, s);
        s += clip.len();
    }
    let mut shifted: Option<(f64, f64, i64)> = None;
    for (k, clip) in rest.iter().enumerate() {
        let new = bounds[k] + if k >= slot { block_len } else { 0 };
        let old = old_start[&clip.id];
        if new != old {
            let b = &boxes[k];
            let delta = new as i64 - old as i64;
            shifted = Some(match shifted {
                None => (b.x, b.x + b.w, delta),
                Some((a, e, d)) => (a.min(b.x), e.max(b.x + b.w), d),
            });
        }
    }
    let ripple = shifted.map(|(a, e, delta)| {
        let (a, e) = (a.max(0.0), e.min(viewport.width));
        RippleTag {
            text: ripple_text(delta, fps),
            anchor: TagAnchor::Center { x: (a + e) / 2.0 },
        }
    });
    Some(MovePreview {
        block: block_ids.clone(),
        slot,
        slots,
        caret_x: viewport.x_of_frame(bounds[slot]),
        starts_at: bounds[slot],
        tooltip: [
            first_line,
            format!("Starts at {}", format::timecode(bounds[slot], fps)),
        ],
        order,
        boxes,
        ripple,
        command: (!unchanged).then_some(Command::ClipMove {
            clips: block_ids,
            to: MoveTo::Index(slot),
        }),
    })
}

/// A trim drag (S10): the moved edge, the trimmed (or added) media, the
/// tags and the release's command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrimPreview {
    pub clip: ClipId,
    pub take: TakeId,
    pub edge: TrimEdge,
    /// The frames the edge moved (snapped and clamped to the take).
    pub d: i64,
    /// The 6 px `accent` bar's x (the new edge).
    pub edge_x: f64,
    /// The dimmed filmstrip with the dashed outline: the media trimmed
    /// away, or what is left to the take's bound when extending (x, w).
    pub ghost: Option<(f64, f64)>,
    /// Extending: the media the clip gains (x, w).
    pub extension: Option<(f64, f64)>,
    /// "−29 f" / "+12 f": how the clip's length changes.
    pub tag: String,
    /// The tag plate's x (4 px into the trimmed or added media).
    pub tag_x: f64,
    /// "In · take 5 frame 41 · 00:00:01:11", "Δ −29 f".
    pub tooltip: [String; 2],
    pub ripple: Option<RippleTag>,
    /// The frame the stage shows (`LocalUi.trim_frame`): the new first
    /// frame, or the new last one.
    pub trim_frame: (TakeId, Frame),
    /// The snap target's x when the edge snapped (the guide's flash).
    pub snapped_x: Option<f64>,
    /// The clips as drawn meanwhile: the trimmed one from its new edge.
    pub boxes: Vec<ClipBox>,
    /// `ClipTrim{clip, start | end: ±d, timeline: false}`; `None` at d = 0.
    pub command: Option<Command>,
}

/// The preview of dragging `clip`'s `edge` by `dx` px, snapping to the
/// playhead and the clip boundaries when `snapping`. `None` for a clip
/// that isn't on the timeline.
pub fn trim_preview(
    state: &State,
    viewport: &Viewport,
    clip: &ClipId,
    edge: TrimEdge,
    dx: f64,
    snapping: bool,
    playhead: Frame,
) -> Option<TrimPreview> {
    let clips = &state.project.timeline.clips;
    let i = clips.iter().position(|c| &c.id == clip)?;
    let c = &clips[i];
    let take = state.project.takes.get(&c.take)?;
    let fps = state.project.canvas.fps;
    let ppf = viewport.px_per_frame;
    let pos = position_of(clips, i);
    let edge_frame = match edge {
        TrimEdge::Start => pos,
        TrimEdge::End => pos + c.len(),
    };
    let candidate = edge_frame as f64 + dx / ppf;
    let snapped = snapping
        .then(|| {
            snap(
                candidate,
                &snap_targets(state, playhead, &[edge_frame]),
                ppf,
            )
        })
        .flatten();
    let raw = match snapped {
        Some(t) => t as i64 - edge_frame as i64,
        None => (dx / ppf).round() as i64,
    };
    let len = c.len() as i64;
    let (lo, hi) = match edge {
        TrimEdge::Start => (-(c.in_ as i64), len - 1),
        TrimEdge::End => (1 - len, take.duration_frames as i64 - c.out as i64),
    };
    let d = raw.clamp(lo, hi.max(lo));
    let new_edge = (edge_frame as i64 + d) as f64;
    let edge_x = viewport.x_of(new_edge);
    let old_x = viewport.x_of_frame(edge_frame);
    let (new_in, new_out) = match edge {
        TrimEdge::Start => ((c.in_ as i64 + d) as Frame, c.out),
        TrimEdge::End => (c.in_, (c.out as i64 + d) as Frame),
    };
    let shortening = match edge {
        TrimEdge::Start => d > 0,
        TrimEdge::End => d < 0,
    };
    let span = |a: f64, b: f64| (a.min(b), (a - b).abs());
    let (ghost, extension) = if d == 0 {
        (None, None)
    } else if shortening {
        (Some(span(old_x, edge_x)), None)
    } else {
        let beyond = match edge {
            TrimEdge::Start => new_in as f64 * ppf,
            TrimEdge::End => (take.duration_frames - new_out) as f64 * ppf,
        };
        let ghost = match edge {
            TrimEdge::Start => (edge_x - beyond, beyond),
            TrimEdge::End => (edge_x, beyond),
        };
        ((beyond > 0.0).then_some(ghost), Some(span(old_x, edge_x)))
    };
    let delta_len = match edge {
        TrimEdge::Start => -d,
        TrimEdge::End => d,
    };
    let tag = signed_frames(delta_len);
    let tag_x = shortening
        .then_some(ghost)
        .flatten()
        .or(extension)
        .map_or(edge_x, |(x, _)| x)
        + 4.0;
    let n = c.take.number();
    let first_line = match edge {
        TrimEdge::Start => format!(
            "In \u{00b7} take {n} frame {new_in} \u{00b7} {}",
            format::timecode(new_in, fps)
        ),
        TrimEdge::End => format!(
            "Out \u{00b7} take {n} frame {new_out} \u{00b7} {}",
            format::timecode(new_out, fps)
        ),
    };
    let ripple = (d != 0).then(|| {
        let anchor = match edge {
            TrimEdge::Start => Some(c.id.clone()),
            TrimEdge::End => clips.get(i + 1).map(|next| next.id.clone()),
        };
        anchor.map(|clip| RippleTag {
            text: ripple_text(delta_len, fps),
            anchor: TagAnchor::AfterLabel { clip },
        })
    });
    let mut boxes = clip_layout(state, viewport, None, recording_elapsed_ns(state));
    if let Some(b) = boxes.get_mut(i) {
        match edge {
            TrimEdge::Start => {
                b.x = edge_x;
                b.in_frame = new_in;
            }
            TrimEdge::End => {}
        }
        b.w = (new_out - new_in) as f64 * ppf;
        b.label = ClipLabel::for_width(&c.take, b.w);
        b.trimming = Some(edge);
    }
    let command = (d != 0).then(|| {
        let arg = if d > 0 {
            TimeArg::plus_frames(d as u64)
        } else {
            TimeArg::minus_frames(d.unsigned_abs())
        };
        let (start, end) = match edge {
            TrimEdge::Start => (Some(arg), None),
            TrimEdge::End => (None, Some(arg)),
        };
        Command::ClipTrim {
            clip: c.id.clone(),
            start,
            end,
            timeline: false,
        }
    });
    Some(TrimPreview {
        clip: c.id.clone(),
        take: c.take.clone(),
        edge,
        d,
        edge_x,
        ghost,
        extension,
        tag: tag.clone(),
        tag_x,
        tooltip: [first_line, format!("\u{0394} {tag}")],
        ripple: ripple.flatten(),
        trim_frame: (
            c.take.clone(),
            match edge {
                TrimEdge::Start => new_in,
                TrimEdge::End => new_out.saturating_sub(1),
            },
        ),
        snapped_x: snapped.map(|t| viewport.x_of_frame(t)),
        boxes,
        command,
    })
}

/// "−29 f", "+12 f", "0 f" (the true minus).
fn signed_frames(n: i64) -> String {
    match n.signum() {
        -1 => format!("\u{2212}{} f", n.unsigned_abs()),
        1 => format!("+{n} f"),
        _ => "0 f".to_owned(),
    }
}

// ------------------------------------------------------------- hit test

/// What a press in the rows hits (D-37, D-45).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "clip", rename_all = "camelCase")]
pub enum Hit {
    /// The 4 px inside a clip's start edge (8 px when only it is selected).
    TrimStart(ClipId),
    TrimEnd(ClipId),
    /// The clip's body: select and move.
    Body(ClipId),
    /// No clip there: an empty row.
    Empty,
}

/// What a press at `x` (time-area px) in the rows hits, over `boxes` (the
/// clip boxes in timeline order). Trim zones (D-45): the 4 px inside each
/// clip edge; at a cut the left clip owns its end zone and the right clip
/// its start zone, and when only one of the two is selected its edge takes
/// both (8 px). Clips under 6 px wide have no zones and lend none.
pub fn hit(boxes: &[ClipBox], x: f64) -> Hit {
    const Z: f64 = TRIM_ZONE_PX;
    let zoned = |b: &&ClipBox| b.w >= TRIM_ZONE_MIN_CLIP_PX;
    for k in 0..=boxes.len() {
        let left = k.checked_sub(1).and_then(|i| boxes.get(i));
        let right = boxes.get(k);
        let (l, r) = (left.filter(zoned), right.filter(zoned));
        // Only clips that meet at one x share a cut.
        let cut = matches!((left, right), (Some(a), Some(b)) if (a.x + a.w - b.x).abs() < 0.5);
        // A selected edge takes the neighbour's zone too, unless the
        // neighbour is too narrow for zones (it lends none) or selected.
        let lends = |other: Option<&ClipBox>| other.is_none_or(|o| zoned(&o) && !o.selected);
        let l_both = l.is_some_and(|b| b.selected) && (!cut || lends(right));
        let r_both = r.is_some_and(|b| b.selected) && (!cut || lends(left));
        if let Some(l) = l.filter(|_| !(r_both && cut)) {
            let end = l.x + l.w;
            let hi = if l_both { end + Z } else { end };
            if x >= end - Z && x < hi {
                return Hit::TrimEnd(l.clip.clone());
            }
        }
        if let Some(r) = r.filter(|_| !(l_both && cut)) {
            let lo = if r_both { r.x - Z } else { r.x };
            if x >= lo && x < r.x + Z {
                return Hit::TrimStart(r.clip.clone());
            }
        }
    }
    boxes
        .iter()
        .find(|b| x >= b.x && x < b.x + b.w)
        .map_or(Hit::Empty, |b| Hit::Body(b.clip.clone()))
}

// ------------------------------------------------------------- tooltip

/// The clip tooltip (C33 clip, S06, S07).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipTooltip {
    /// "Take 5 · c2d1".
    pub title: String,
    /// "In 00:00:36:24 · Out 00:01:13:24 · 00:00:37:00": take-relative in
    /// and out, and the length.
    pub times: String,
    /// `S` Split · `[` `]` Trim · `X` Remove (the user's keys).
    pub keys: Vec<HintItem>,
}

/// The tooltip of `clip`; `None` when it isn't on the timeline.
pub fn clip_tooltip(state: &State, clip: &ClipId, keymap: &Keymap) -> Option<ClipTooltip> {
    let c = state.project.clip(clip)?;
    let fps = state.project.canvas.fps;
    let caps = |id: &str, prefer: Option<&str>| -> Vec<Vec<String>> {
        let Some(binding) = keymap.binding(id) else {
            return Vec::new();
        };
        let keys = usable_keys(keymap, binding);
        let key = prefer
            .and_then(|p| keys.iter().find(|k| k.key == p))
            .or_else(|| keys.first());
        key.map(|k| vec![k.cap_legends()]).unwrap_or_default()
    };
    let mut trim = caps("clip.trim-start", None);
    trim.extend(caps("clip.trim-end", None));
    let item = |keys: Vec<Vec<String>>, label: &str| HintItem {
        keys,
        label: label.to_owned(),
        enabled: true,
        emphasized: false,
    };
    let keys: Vec<HintItem> = [
        item(caps("clip.split", None), "Split"),
        item(trim, "Trim"),
        item(caps("clip.remove", Some("x")), "Remove"),
    ]
    .into_iter()
    .filter(|i| !i.keys.is_empty())
    .collect();
    Some(ClipTooltip {
        title: format!("Take {} \u{00b7} {}", c.take.number(), c.id),
        times: format!(
            "In {} \u{00b7} Out {} \u{00b7} {}",
            format::timecode(c.in_, fps),
            format::timecode(c.out, fps),
            format::timecode(c.len(), fps)
        ),
        keys,
    })
}

// ------------------------------------------------------------- toolbar

/// The toolbar's variant (C14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolbarVariant {
    Normal,
    /// No clips (S01a, S01b): Bin without a count, Autocut, its settings
    /// and the zoom cluster disabled, "No clips yet".
    Empty,
    /// Not Idle (S03–S05): Autocut and its settings disabled.
    Recording,
}

/// The timeline toolbar (C14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolbarView {
    pub variant: ToolbarVariant,
    /// The bin's takes; `None` when there are none.
    pub bin_count: Option<usize>,
    pub bin_open: bool,
    pub snapping: bool,
    /// `autocut.run` runs (else why not).
    pub autocut: Result<(), String>,
    /// `autocut.preview` (the autocut page) opens.
    pub autocut_settings: bool,
    /// ("10 clips · ", "00:04:06:11"), or ("No clips yet", "").
    pub summary: (String, String),
    /// The zoom cluster works (a viewport exists).
    pub zoom: bool,
    pub zoom_in: bool,
    pub zoom_out: bool,
    pub fit: bool,
    /// The slider's position, `s ∈ [0, 1]`.
    pub slider: f64,
    /// "Fit", "48 px/s", "32 px/frame".
    pub readout: String,
}

/// The toolbar for `state` with the timeline shown at `viewport` (`None`
/// on an empty timeline).
pub fn toolbar(state: &State, viewport: Option<&Viewport>, total: Frame) -> ToolbarView {
    let clips = &state.project.timeline.clips;
    let idle = state.runtime.record.is_idle();
    let variant = if !idle {
        ToolbarVariant::Recording
    } else if clips.is_empty() {
        ToolbarVariant::Empty
    } else {
        ToolbarVariant::Normal
    };
    let fps = state.project.canvas.fps;
    let summary = format::clips_summary(clips, fps)
        .unwrap_or_else(|| (format::NO_CLIPS.to_owned(), String::new()));
    let autocut = match action_for("autocut.run", state) {
        Some(Action::Run(_)) => Ok(()),
        Some(Action::Unavailable(why)) => Err(why),
        _ => Err("Not available".to_owned()),
    };
    let fit = viewport.and_then(|v| Viewport::fit_px_per_frame(v.width, total));
    let (zoom, zoom_in, zoom_out, slider, readout) = match (viewport, fit) {
        (Some(v), Some(fit)) => (
            true,
            can_zoom_in(v, fit),
            can_zoom_out(v, fit),
            slider_value(v.px_per_frame, fit),
            zoom_readout(v, fit, fps),
        ),
        _ => (false, false, false, 0.0, "Fit".to_owned()),
    };
    ToolbarView {
        variant,
        bin_count: Some(bin_count(state)).filter(|n| *n > 0),
        bin_open: state.ui.bin_open,
        snapping: state.ui.snapping,
        autocut,
        autocut_settings: idle && !clips.is_empty(),
        summary,
        zoom,
        zoom_in,
        zoom_out,
        fit: zoom && !clips.is_empty(),
        slider,
        readout,
    }
}

/// The bin's takes (G§4.8): `ok` takes no clip uses, broken and too-short
/// takes.
pub fn bin_count(state: &State) -> usize {
    let used: BTreeSet<&TakeId> = state
        .project
        .timeline
        .clips
        .iter()
        .map(|c| &c.take)
        .collect();
    state
        .project
        .takes
        .iter()
        .filter(|(id, take)| match take.status {
            TakeStatus::Ok => !used.contains(id),
            TakeStatus::Broken | TakeStatus::TooShort => true,
            TakeStatus::Recording => false,
        })
        .count()
}

// ------------------------------------------------------------- headers

/// The video row's header (C17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoHeader {
    /// "Video".
    pub title: String,
    /// "Thumbnails: Face (L2)": the project scene's topmost visible layer.
    pub caption: Option<String>,
}

/// The video row's header.
pub fn video_header(state: &State) -> VideoHeader {
    let caption = state
        .project
        .scene
        .layers
        .iter()
        .rev()
        .find(|l| l.visible)
        .map(|l| format!("Thumbnails: {} ({})", l.name, l.id));
    VideoHeader {
        title: "Video".to_owned(),
        caption,
    }
}

/// A lane's header (C17): its `M` (playback only, D-08) and `S` toggles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneHeader {
    pub lane: Lane,
    /// "Mute A1 while playing (export unaffected)".
    pub mute_tooltip: String,
    /// "Solo A1 while playing (export unaffected)".
    pub solo_tooltip: String,
    /// `SetUi{lane_mute}` with the lane toggled in the whole set.
    pub mute: Command,
    /// `SetUi{solo}`: this lane, or none when it is the solo.
    pub solo: Command,
}

/// The lane headers, in lane order.
pub fn lane_headers(state: &State) -> Vec<LaneHeader> {
    lanes(state)
        .into_iter()
        .map(|lane| {
            let mut muted = state.ui.lane_mute.clone();
            if !muted.remove(&lane.track) {
                muted.insert(lane.track.clone());
            }
            let solo = if lane.solo {
                Nullable::Clear
            } else {
                Nullable::Set(lane.track.clone())
            };
            LaneHeader {
                mute_tooltip: format!("Mute {} while playing (export unaffected)", lane.track),
                solo_tooltip: format!("Solo {} while playing (export unaffected)", lane.track),
                mute: Command::SetUi {
                    patch: UiPatch {
                        lane_mute: Some(muted),
                        ..UiPatch::default()
                    },
                },
                solo: Command::SetUi {
                    patch: UiPatch {
                        solo,
                        ..UiPatch::default()
                    },
                },
                lane,
            }
        })
        .collect()
}

// ------------------------------------------------------------- empty state

/// The empty timeline (S01b, C31 Timeline).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyState {
    /// "This swamp is quiet." (`display`).
    pub title: String,
    /// "Press", the cap of `record.toggle`, "and make some noise."; "Click
    /// Record" and no cap when `record.toggle` has no key that runs.
    pub before: String,
    pub key: Option<Vec<String>>,
    pub after: String,
}

/// The empty state with the user's record key. Without one that runs
/// (unbound, or in a keys.toml conflict) the line names the header's Record
/// button instead, as the hint bar leaves its Record item out.
pub fn empty_state(keymap: &Keymap) -> EmptyState {
    let key = keymap
        .binding("record.toggle")
        .and_then(|b| usable_keys(keymap, b).first().map(|k| k.cap_legends()));
    EmptyState {
        title: "This swamp is quiet.".to_owned(),
        before: if key.is_some() {
            "Press"
        } else {
            "Click Record"
        }
        .to_owned(),
        key,
        after: "and make some noise.".to_owned(),
    }
}

#[cfg(test)]
#[path = "timeline_tests.rs"]
mod tests;
