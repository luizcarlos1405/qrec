//! The Settings dialog's shell and its Project page (G§4.10, S18b). Owner:
//! WP6. Stub until WP6 lands: the signatures are final, the bodies return
//! defaults.

use serde::{Deserialize, Serialize};

use crate::model::Fps;
use crate::protocol::command::Command;
use crate::state::State;
use crate::view::GuiFacts;

/// The dialog's pages, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsPage {
    Appearance,
    Recording,
    Project,
    Autocut,
    Keyboard,
}

impl SettingsPage {
    pub const ALL: [SettingsPage; 5] = [
        SettingsPage::Appearance,
        SettingsPage::Recording,
        SettingsPage::Project,
        SettingsPage::Autocut,
        SettingsPage::Keyboard,
    ];
}

/// One tab: the page, its label and whether it opens (only Project until
/// WP9, D-48; the others show the tooltip "Not built yet").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsTab {
    pub page: SettingsPage,
    pub label: String,
    pub enabled: bool,
}

/// The tabs, Appearance first.
pub fn tabs(facts: &GuiFacts) -> Vec<SettingsTab> {
    let _ = facts;
    Vec::new()
}

/// The Project page (S18b): the "Canvas" group.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPage {
    /// "Canvas".
    pub group: String,
    pub size: SizeRow,
    pub rate: RateRow,
    pub background: BackgroundRow,
    /// "Saved in this project's ~/videos/rust-traits-talk/qrec.json."
    pub footer: String,
}

/// The Size dropdown: "1920×1080 (16:9)", …, plus the current size when it
/// isn't listed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeRow {
    pub label: String,
    pub caption: String,
    pub options: Vec<SizeOption>,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeOption {
    pub width: u32,
    pub height: u32,
    /// "1920×1080 (16:9)".
    pub label: String,
}

/// The Frame rate segmented control: 24 / 25 / 30 / 50 / 60; none
/// selected for another rate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateRow {
    pub label: String,
    pub caption: String,
    pub rates: Vec<u32>,
    pub selected: Option<usize>,
}

/// The Background field (`#RRGGBB`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundRow {
    pub label: String,
    pub caption: String,
    pub value: String,
}

/// The Project page of `state`; `dir_display` is the project folder as
/// people read it (`format::display_path`).
pub fn project_page(state: &State, dir_display: &str) -> ProjectPage {
    let _ = (state, dir_display);
    ProjectPage::default()
}

/// Picking a size: `ProjectSet{canvas}` with it.
pub fn size_command(state: &State, width: u32, height: u32) -> Command {
    let _ = state;
    Command::ProjectSet {
        name: None,
        canvas: Some(crate::protocol::command::CanvasSpec {
            width: Some(width),
            height: Some(height),
            fps: None,
        }),
        background: None,
        countdown_secs: None,
        encoder: None,
        vaapi_device: crate::protocol::command::Nullable::Keep,
    }
}

/// Picking a frame rate: `ProjectSet{canvas}` with it (with takes, the
/// core answers `confirmation_required` and the S18b modal opens).
pub fn rate_command(state: &State, fps: Fps) -> Command {
    let _ = state;
    Command::ProjectSet {
        name: None,
        canvas: Some(crate::protocol::command::CanvasSpec {
            width: None,
            height: None,
            fps: Some(fps),
        }),
        background: None,
        countdown_secs: None,
        encoder: None,
        vaapi_device: crate::protocol::command::Nullable::Keep,
    }
}

/// The Background field committed (Enter or blur): `ProjectSet{background}`,
/// or the field's error ("Use #RRGGBB").
pub fn background_command(text: &str) -> Result<Command, String> {
    let _ = text;
    Err(String::new())
}
