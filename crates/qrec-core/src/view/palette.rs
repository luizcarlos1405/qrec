//! The command palette (G§4.11, S19, C34), once G§0.2 approves it. Owner:
//! WP9. Stub until WP9 lands: the signatures are final, the bodies return
//! defaults.
//!
//! Rows come from `keymap::palette` plus `history.clear`; the fuzzy match is
//! a subsequence with bonuses for consecutive characters and word starts
//! (ties keep the table order); an empty query shows "Recent" (the last
//! three ids run this session), then Record, Playback, Edit, Output, View,
//! Canvas.

use serde::{Deserialize, Serialize};

use crate::keymap::Keymap;
use crate::state::State;

/// The palette's content for a query.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteView {
    pub groups: Vec<PaletteGroup>,
}

/// A group of rows ("Recent", "Record", …; one unnamed group for a query).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteGroup {
    pub title: Option<String>,
    pub rows: Vec<PaletteItem>,
}

/// One row (C34).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteItem {
    pub id: String,
    pub label: String,
    /// Char indices of `label` the query matched (bold).
    pub matched: Vec<usize>,
    /// Key-cap legends per chord.
    pub keys: Vec<Vec<String>>,
    /// The short CLI form.
    pub cli: Option<String>,
    pub enabled: bool,
    pub reason: Option<String>,
}

/// The palette for `query`; `recent` are the ids run this session, newest
/// first.
pub fn palette(keymap: &Keymap, state: &State, query: &str, recent: &[String]) -> PaletteView {
    let _ = (keymap, state, query, recent);
    PaletteView::default()
}

/// The fuzzy match of `query` in `text`: a score (higher is better) and
/// the matched char indices; `None` when `query` isn't a subsequence.
pub fn fuzzy_match(query: &str, text: &str) -> Option<(u32, Vec<usize>)> {
    let _ = (query, text);
    None
}
