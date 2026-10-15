//! The cheat sheet (G§4.11, S20), once G§0.2 approves it. Owner: WP9. Stub
//! until WP9 lands: the signatures are final, the bodies return defaults.
//!
//! Three columns: Record, Output, Global (niri) with "Copy niri snippet" │
//! Playback, View │ Edit, Canvas (D-22). Rebound keys show "default P";
//! a search bolds its matches ("3 shortcuts match “trim”").

use serde::{Deserialize, Serialize};

use crate::keymap::Keymap;

/// The sheet: three columns of groups.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatSheet {
    pub columns: Vec<Vec<CheatGroup>>,
    /// "3 shortcuts match “trim”" while searching.
    pub matches: Option<String>,
}

/// A group of the sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatGroup {
    pub title: String,
    pub rows: Vec<CheatRow>,
}

/// One shortcut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatRow {
    pub id: String,
    pub label: String,
    /// Char indices of `label` the search matched.
    pub matched: Vec<usize>,
    /// Key-cap legends per chord.
    pub keys: Vec<Vec<String>>,
    /// "default P" when rebound.
    pub default_keys: Option<String>,
}

/// The sheet for `search` (empty: everything).
pub fn cheat_sheet(keymap: &Keymap, search: &str) -> CheatSheet {
    let _ = (keymap, search);
    CheatSheet::default()
}
