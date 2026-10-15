//! The header bar (G§4.4, C04, C01 Record, C05): title and subtitle, the
//! record button, the export slot, the mode switch. Owner: WP6. Stub until
//! WP6 lands: the signatures are final, the bodies return defaults.

use serde::{Deserialize, Serialize};

use crate::keymap::Keymap;
use crate::state::State;
use crate::view::GuiFacts;

/// Everything the header shows.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderView {
    /// `project.name`.
    pub title: String,
    pub subtitle: Subtitle,
    pub record: RecordButton,
    pub export: ExportSlot,
    /// The 2 px `record` line under the header (recording, finalizing).
    pub record_line: bool,
    /// The narrow layout's layers button (G§3.15).
    pub layers_button: bool,
}

/// The subtitle under the title.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Subtitle {
    /// "No clips yet".
    #[default]
    NoClips,
    /// `format::clips_summary`: "10 clips · " and "00:04:06:11".
    Clips { count: String, duration: String },
    /// "Recovering take 7…" with a 12 px spinner (D-47).
    Recovering { text: String },
}

/// The record button's look (C01 Record).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordButtonKind {
    /// `● Record [R]`.
    #[default]
    Record,
    /// Outlined `Cancel [R]` during the countdown.
    Cancel,
    /// `■ Stop [R]`, starting or recording.
    Stop,
    /// `Saving…` with a spinner, finalizing.
    Saving,
}

/// The record button.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordButton {
    pub kind: RecordButtonKind,
    pub enabled: bool,
    /// The key caps of `record.toggle` (`[["R"]]`); empty when unbound.
    pub keys: Vec<Vec<String>>,
    /// "Record", "Add a layer or audio track first", …
    pub tooltip: String,
}

/// The export slot: the `Export…` button, or the running export's
/// progress (`export_readout`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSlot {
    /// `Some(42)` while an export runs: "42%" and the mini mud bar.
    pub percent: Option<u8>,
    pub enabled: bool,
    /// Pressed while the export panel is open.
    pub pressed: bool,
    /// "Exporting 42%", "The timeline is empty", "Not while recording".
    pub tooltip: String,
    /// The key caps of `export`.
    pub keys: Vec<Vec<String>>,
}

/// The header for `state`.
pub fn header(state: &State, facts: &GuiFacts, keymap: &Keymap) -> HeaderView {
    let _ = (state, facts, keymap);
    HeaderView::default()
}
