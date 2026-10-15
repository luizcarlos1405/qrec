//! The Onion layers panel (G§4.7, C09, C10, C12, C13): header and take
//! banner, layer rows, audio rows and the inspector. Owner: WP5. Stub until
//! WP5 lands: the signatures are final, the bodies return defaults.

use serde::{Deserialize, Serialize};

use crate::ids::{LayerId, TrackId};
use crate::model::{AudioRole, Fit};
use crate::protocol::command::{Command, SceneDest, SceneTarget};
use crate::state::State;
use crate::view::LiveSources;

/// The panel header (C13).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelHeader {
    /// "Onion layers".
    pub title: String,
    /// "Project scene · new takes use this", "Take 5 layout · edits this
    /// take only", or `ui_normalize::NO_EDIT_TARGET` (D-06).
    pub subtitle: String,
    /// "Changes here only affect take 5." with the `Layout ▾` menu.
    pub banner: Option<String>,
    /// The `Layout ▾` items, when the banner shows.
    pub layout_menu: Vec<LayoutItem>,
}

/// A `Layout ▾` item and the command it sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutItem {
    /// "Apply this layout to all takes", …
    pub label: String,
    pub from: SceneTarget,
    pub to: SceneDest,
}

/// The header for `state`.
pub fn header(state: &State) -> PanelHeader {
    let _ = state;
    PanelHeader::default()
}

/// One layer row, topmost first (C09).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerRow {
    pub layer: LayerId,
    pub name: String,
    /// "Camera · Logitech C920", "Screen · DP-4 · GIGA-BYTE GS27QA".
    pub source: String,
    pub selected: bool,
    pub hidden: bool,
    pub locked: bool,
    /// Live: "Access denied by the desktop portal", "Screen picker
    /// closed", or the source's message.
    pub error: Option<String>,
    /// "Missing in this take".
    pub missing: Option<String>,
    /// D-43, or no edit target (D-06).
    pub enabled: bool,
}

/// The layer rows of the scene the canvas shows; `live` gives the errors
/// in Live mode.
pub fn layer_rows(state: &State, live: &LiveSources) -> Vec<LayerRow> {
    let _ = (state, live);
    Vec::new()
}

/// One audio row (C10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioRow {
    pub track: TrackId,
    pub role: AudioRole,
    pub label: String,
    /// The "Autocut" tag.
    pub autocut: bool,
    pub muted: bool,
    /// "0.0 dB".
    pub gain: String,
    pub gain_db: f64,
    /// "+0 ms".
    pub sync: String,
    pub sync_offset_ms: i32,
    /// "Device not found. Plug it in, or pick another source."
    pub device_missing: Option<String>,
    pub enabled: bool,
}

/// The audio rows of the scene the canvas shows.
pub fn audio_rows(state: &State, live: &LiveSources) -> Vec<AudioRow> {
    let _ = (state, live);
    Vec::new()
}

/// The inspector of the selected layer (C12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspector {
    pub layer: LayerId,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub crop: [u32; 4],
    /// 0–100.
    pub opacity_percent: u32,
    pub fit: Fit,
    pub enabled: bool,
}

/// The inspector, or `None` with no layer selected (the panel then reads
/// "Select a layer on the canvas or in the list to edit its position, size
/// and crop.").
pub fn inspector(state: &State) -> Option<Inspector> {
    let _ = state;
    None
}

/// An inspector field's text checked and turned into its `LayerSet`, or the
/// helper text ("Width must be at least 1", "X must be between −8192 and
/// 8192", "Crop leaves nothing to show").
pub fn inspector_commit(
    state: &State,
    field: InspectorField,
    text: &str,
) -> Result<Command, String> {
    let _ = (state, field, text);
    Err(String::new())
}

/// The inspector's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InspectorField {
    X,
    Y,
    W,
    H,
    CropTop,
    CropRight,
    CropBottom,
    CropLeft,
    Opacity,
}
