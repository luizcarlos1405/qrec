//! The timeline ruler (G§4.5, C15): label interval, ticks, labels, the
//! frame-level cells and the corner timecode. Owner: WP3. Stub until WP3
//! lands: the signatures are final, the bodies return defaults.

use serde::{Deserialize, Serialize};

use crate::ids::Frame;
use crate::model::Fps;
use crate::view::timeline::Viewport;

/// How far apart labels are: the smallest of 1f, 2f, 5f, 10f, 15f, 1s,
/// 2s, 5s, 10s, 15s, 30s, 1min, 2min, 5min, 10min, 15min, 30min, 1h that
/// is at least 90 px wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "unit", content = "n", rename_all = "camelCase")]
pub enum LabelInterval {
    Frames(u32),
    Seconds(u32),
    Minutes(u32),
    Hours(u32),
}

/// The label interval at `px_per_frame`.
pub fn label_interval(px_per_frame: f64, fps: Fps) -> LabelInterval {
    let _ = (px_per_frame, fps);
    LabelInterval::Seconds(1)
}

/// What a tick is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TickKind {
    /// 10 px, with a label.
    Major,
    /// 5 px.
    Minor,
    /// Frame level: 5 px per frame, with its `FF` number centered.
    Frame,
    /// Frame level: 28 px at a second, with the full timecode in `text`.
    Second,
}

/// One tick, x relative to the time area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tick {
    pub x: f64,
    pub kind: TickKind,
    pub label: Option<String>,
}

/// The ticks visible in `viewport` on a timeline of `total` frames.
pub fn ticks(viewport: &Viewport, fps: Fps, total: Frame) -> Vec<Tick> {
    let _ = (viewport, fps, total);
    Vec::new()
}

/// The corner cell: the playhead's timecode.
pub fn corner(playhead: Frame, fps: Fps) -> String {
    let _ = (playhead, fps);
    String::new()
}
