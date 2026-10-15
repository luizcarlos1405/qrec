//! The stage (G§4.6, C06–C08): its geometry, layer boxes and their edits,
//! the overlays (countdown, REC, transport, badges) and the first-run
//! source list. Owner: WP4. Stub until WP4 lands (but for
//! [`edit_target`], WP0a's): the signatures are final, the bodies return
//! defaults.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ctx::{AudioDeviceInfo, CameraInfo, ScreenInfo};
use crate::ids::{Frame, LayerId, TakeId};
use crate::model::{Canvas, Scene};
use crate::protocol::command::{Command, SceneTarget};
use crate::state::State;
use crate::view::Rect;

/// The scene layout edits go to (D-06): `ui_normalize::edit_target`. `None`
/// in Program mode with nothing selected: rows, inspector and canvas
/// drags are disabled.
pub fn edit_target(state: &State) -> Option<SceneTarget> {
    crate::ui_normalize::edit_target(&state.project, &state.ui)
}

/// Where the preview and the transport row sit in the stage (stage-local).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageGeometry {
    pub preview: Rect,
    pub transport: Rect,
}

/// The canvas aspect fitted into `(stage_w − 32) × (stage_h − 68)`; the
/// 36 px transport row anchored to the stage bottom (G§3.15).
pub fn stage_geometry(stage_w: f64, stage_h: f64, canvas: &Canvas) -> StageGeometry {
    let _ = (stage_w, stage_h, canvas);
    StageGeometry::default()
}

/// A layer's box on the preview (C08).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerBox {
    pub layer: LayerId,
    /// Preview-local.
    pub rect: Rect,
    pub visible: bool,
    pub locked: bool,
    /// Array order: the last is on top.
    pub z: usize,
}

/// The boxes of `scene`'s layers on a preview at `preview`; `sizes` are
/// the sources' sizes (Live: the live status; Program: the take's streams;
/// unknown: the rect).
pub fn layer_boxes(
    scene: &Scene,
    canvas: &Canvas,
    sizes: &BTreeMap<LayerId, (u32, u32)>,
    preview: Rect,
) -> Vec<LayerBox> {
    let _ = (scene, canvas, sizes, preview);
    Vec::new()
}

/// A nudge burst's rect (D-33): `(dx, dy)` canvas px, ×10 with `shift`.
pub fn nudge(rect: crate::model::Rect, dx: i32, dy: i32, shift: bool) -> crate::model::Rect {
    let _ = (dx, dy, shift);
    rect
}

/// The position readout: "x 1280 · y 760 · 384 × 288".
pub fn readout(rect: crate::model::Rect) -> String {
    let _ = rect;
    String::new()
}

/// The countdown overlay (C07).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Countdown {
    pub digit: u32,
    /// The ring's sweep, 0..1 over the second.
    pub sweep: f64,
    /// The take about to start.
    pub take: TakeId,
}

/// The countdown, `ms_since_tick` into its current second.
pub fn countdown(state: &State, ms_since_tick: u64) -> Option<Countdown> {
    let _ = (state, ms_since_tick);
    None
}

/// The REC cluster (C06).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RecBadge {
    #[default]
    None,
    /// "Starting take 10…" (D-04).
    Starting {
        take: TakeId,
    },
    Recording {
        take: TakeId,
        elapsed_ns: u64,
    },
    Saving {
        take: TakeId,
    },
}

/// The REC cluster of `state`.
pub fn rec(state: &State) -> RecBadge {
    let _ = state;
    RecBadge::None
}

/// The transport row's left side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TransportLeft {
    /// Program: "00:01:21:07" / "00:04:06:11".
    Timecode { now: String, total: String },
    /// Live: "Live · 1920×1080 · 30 fps".
    Live { text: String },
}

/// The transport row (C06).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transport {
    pub left: TransportLeft,
    pub enabled: bool,
    /// The shuttle speed after the total: "· 2×", "· −4×" (D-26).
    pub rate: Option<String>,
}

/// The transport row: the player's frame while playing.
pub fn transport(state: &State, player_frame: Frame, playing: bool, rate: f64) -> Transport {
    let _ = (state, player_frame, playing, rate);
    Transport {
        left: TransportLeft::Live {
            text: String::new(),
        },
        enabled: false,
        rate: None,
    }
}

/// A stage badge (C06).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Badge {
    Live,
    Program,
    /// The bin preview (D-17): `TAKE 4`.
    Take {
        take: TakeId,
    },
    /// A preview-size player (H-9).
    Proxy,
}

/// The badges, right to left: the mode (or `TAKE 4`), then `PROXY`.
pub fn badges(state: &State, take_preview: Option<&TakeId>, proxy: bool) -> Vec<Badge> {
    let _ = (state, take_preview, proxy);
    Vec::new()
}

/// What a first-run row adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    Screen,
    Camera,
    Mic,
    SystemAudio,
}

/// One source of the first-run list (S01a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRow {
    pub kind: SourceKind,
    /// The output, device or source name it adds.
    pub source: String,
    /// "Screen · DP-4 · GIGA-BYTE GS27QA", "Mic · Yeti Nano".
    pub label: String,
    /// "2560×1440".
    pub meta: Option<String>,
    pub checked: bool,
}

/// The first-run rows: screens, cameras, mics, monitors; the first screen
/// and camera and the default mic and monitor checked (D-16).
pub fn first_run_rows(
    screens: &[ScreenInfo],
    cameras: &[CameraInfo],
    mics: &[AudioDeviceInfo],
    monitors: &[AudioDeviceInfo],
    default_mic: Option<&str>,
    default_monitor: Option<&str>,
) -> Vec<SourceRow> {
    let _ = (
        screens,
        cameras,
        mics,
        monitors,
        default_mic,
        default_monitor,
    );
    Vec::new()
}

/// The commands "Add N sources" sends, in one FIFO: screens, cameras, the
/// mic, system audio (D-16).
pub fn first_run_commands(rows: &[SourceRow]) -> Vec<Command> {
    let _ = rows;
    Vec::new()
}

/// The first-run button: "Add 4 sources", "Add 1 source".
pub fn add_sources_label(checked: usize) -> String {
    let _ = checked;
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::CanvasMode;
    use crate::testkit;

    #[test]
    fn edit_target_is_the_core_rule() {
        let mut state = testkit::design_state();
        assert_eq!(edit_target(&state), Some(SceneTarget::Project));
        state.ui.canvas_mode = CanvasMode::Program;
        assert_eq!(edit_target(&state), None, "S07: nothing selected");
        state.ui.selection = vec![crate::ids::ClipId::from_raw("c2d0")];
        assert_eq!(
            edit_target(&state),
            Some(SceneTarget::Take(TakeId::numbered(5)))
        );
    }
}
