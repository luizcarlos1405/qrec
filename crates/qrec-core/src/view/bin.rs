//! The bin drawer (G§4.8, C21, S14, S18): rows of the takes no clip uses,
//! plus broken and too-short takes, newest first. Owner: WP7. Stub until
//! WP7 lands: the signatures are final, the bodies return defaults.
//!
//! Secondary lines (D-18), first that applies: tooShort "7 frames; takes
//! need 10."; broken with `on_event::record::EOS_TIMEOUT_REASON` "Files
//! didn't close within 10 s."; broken "recovery failed: …" "Couldn't be
//! recovered: …."; other broken takes the error's first sentence; ok and
//! `recovered` "Recovered after Qrec closed mid-take."; never placed
//! "Cancelled with Esc."; replaced "Replaced by take 5."; discarded
//! "Discarded with D."; otherwise "Removed from the timeline.".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::TakeId;
use crate::protocol::command::{Command, InsertAt};
use crate::protocol::toast::UiAction;
use crate::state::State;

/// A status chip (C22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Chip {
    /// "Broken", error tone.
    Broken,
    /// "Too short", warning tone.
    TooShort,
    /// "Recovered", neutral.
    Recovered,
    /// "Unused", neutral.
    Unused,
}

impl Chip {
    pub fn label(self) -> &'static str {
        match self {
            Chip::Broken => "Broken",
            Chip::TooShort => "Too short",
            Chip::Recovered => "Recovered",
            Chip::Unused => "Unused",
        }
    }
}

/// A row's action (BRIEF §3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BinAction {
    /// Plays the whole take on the stage (D-17).
    Preview,
    /// `ClipAdd{take, at: End}`.
    InsertAtEnd { command: Command },
    /// `ClipAdd{take, at: AfterSelection}`; disabled with "Select a clip on
    /// the timeline first" when nothing is selected.
    InsertAfterSelection {
        command: Command,
        disabled: Option<String>,
    },
    /// `RevealPath(takes/<id>/)`.
    Reveal { action: UiAction },
    /// `OpenPath(takes/<id>/recorder.log)`, for broken takes.
    OpenLog { action: UiAction },
    /// `TakePurge{takes}`; the modal comes from `confirmation_required`.
    Purge { command: Command },
}

/// One bin row (C21).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BinRow {
    pub take: TakeId,
    /// "Take 4".
    pub title: String,
    pub chip: Chip,
    /// "00:00:51:18 · 14:12 · 129.0 MB" (duration "—" when broken).
    pub mono: String,
    /// D-18's line.
    pub secondary: String,
    pub actions: Vec<BinAction>,
    /// Index into `format::TAKE_COLORS`.
    pub color: usize,
}

/// The bin's rows, newest first. `sizes` are the takes' bytes on disk (a
/// size not known yet shows "—"); recording times are local at
/// `utc_offset_secs`.
pub fn bin_rows(state: &State, sizes: &BTreeMap<TakeId, u64>, utc_offset_secs: i32) -> Vec<BinRow> {
    let _ = (state, sizes, utc_offset_secs);
    Vec::new()
}

/// The bin header: "5 takes · 436.4 MB".
pub fn header(state: &State, sizes: &BTreeMap<TakeId, u64>) -> String {
    let _ = (state, sizes);
    String::new()
}

/// The multi-select footer (D-37): "2 takes selected · 184.1 MB · Purge
/// from disk", with its command; `None` below two takes.
pub fn footer(selection: &[TakeId], sizes: &BTreeMap<TakeId, u64>) -> Option<(String, Command)> {
    let _ = (selection, sizes);
    None
}

/// Inserting a take from the bin (a drop at slot `k`, D-38).
pub fn insert_at(take: &TakeId, at: InsertAt) -> Command {
    Command::ClipAdd {
        take: take.clone(),
        at,
    }
}
