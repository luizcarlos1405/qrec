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
//!
//! Built by WP0a: [`recorded_hm`], the mono line's local time (each take at
//! the UTC offset in force when it was recorded).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::TakeId;
use crate::protocol::command::{Command, InsertAt};
use crate::protocol::toast::UiAction;
use crate::state::State;
use crate::view::format;

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
/// size not known yet shows "—"). Recording times are local at the offset
/// in force when each take was recorded ([`recorded_hm`]): `offsets`, which
/// the GUI fills per take from its zone
/// (`Local.offset_from_utc_datetime(&take.recorded_at.naive_utc())`), else
/// `fallback_offset_secs`, the offset now. One offset for every take would
/// show a take recorded before a DST change an hour off.
pub fn bin_rows(
    state: &State,
    sizes: &BTreeMap<TakeId, u64>,
    offsets: &BTreeMap<TakeId, i32>,
    fallback_offset_secs: i32,
) -> Vec<BinRow> {
    let _ = (state, sizes, offsets, fallback_offset_secs);
    Vec::new()
}

/// When `take` was recorded, as its row's mono line shows it: "14:12",
/// local at its own offset (`offsets`, else `fallback_offset_secs`; see
/// [`bin_rows`]). `None` for a take the project doesn't have.
pub fn recorded_hm(
    state: &State,
    take: &TakeId,
    offsets: &BTreeMap<TakeId, i32>,
    fallback_offset_secs: i32,
) -> Option<String> {
    let recorded = state.project.takes.get(take)?.recorded_at;
    let offset = offsets.get(take).copied().unwrap_or(fallback_offset_secs);
    Some(format::local_hm(recorded, offset))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    /// Take 4 was recorded at 14:12 EDT (UTC−4), take 2 at 10:06 EST (UTC−5)
    /// after the clocks went back; the bin opens under EST. Each reads its
    /// own wall clock: one offset for both would show take 4 at 13:12.
    #[test]
    fn each_take_reads_the_offset_in_force_when_it_was_recorded() {
        let mut state = testkit::design_state();
        let (take_4, take_2) = (TakeId::numbered(4), TakeId::numbered(2));
        let takes = &mut state.project.takes;
        takes.get_mut(&take_4).unwrap().recorded_at = "2026-10-14T18:12:00Z".parse().unwrap();
        takes.get_mut(&take_2).unwrap().recorded_at = "2026-11-02T15:06:00Z".parse().unwrap();
        let est = -5 * 3600;
        let offsets = BTreeMap::from([(take_4.clone(), -4 * 3600), (take_2.clone(), est)]);
        let hm = |take: &TakeId, offsets: &BTreeMap<TakeId, i32>| {
            recorded_hm(&state, take, offsets, est)
        };
        assert_eq!(hm(&take_4, &offsets).as_deref(), Some("14:12"));
        assert_eq!(hm(&take_2, &offsets).as_deref(), Some("10:06"));
        // A take the GUI has no offset for falls back to the offset now.
        assert_eq!(hm(&take_4, &BTreeMap::new()).as_deref(), Some("13:12"));
        assert_eq!(hm(&TakeId::numbered(99), &offsets), None);
    }
}
