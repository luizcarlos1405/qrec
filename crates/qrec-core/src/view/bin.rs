//! The bin drawer (G§4.8, C21, S14, S18): rows of the takes no clip uses,
//! plus broken and too-short takes, newest first. Owner: WP7.
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

use crate::apply::record::MIN_TAKE_FRAMES;
use crate::ids::TakeId;
use crate::model::{Take, TakeStatus};
use crate::on_event::record::EOS_TIMEOUT_REASON;
use crate::protocol::command::{Command, InsertAt};
use crate::protocol::toast::UiAction;
use crate::state::State;
use crate::time::NS_PER_SEC;
use crate::view::format;
use crate::view::timeline::thumbnail_stream;

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
    /// `disabled` says why it can't run now (D-43, `view::enabled`: while
    /// recording, with `qrec.json` invalid on disk).
    Purge {
        command: Command,
        disabled: Option<String>,
    },
}

/// What a row's 96×54 thumbnail shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Thumbnail {
    /// The strip image of `file` (project-relative) at file second
    /// `second`: the middle of the take, on its thumbnail layer (Q4, the
    /// topmost visible one, as the timeline's filmstrip).
    Image { file: String, second: u32 },
    /// A broken take: a hatch with an error icon (S14 take 9).
    Broken,
    /// No picture to show (an audio-only take, a missing layer).
    Blank,
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
    pub thumbnail: Thumbnail,
    /// The take can be dragged onto the timeline (D-38): ok takes.
    pub insertable: bool,
}

impl BinRow {
    /// The row's Open log link (broken takes), shown on the row itself.
    pub fn open_log(&self) -> Option<&UiAction> {
        self.actions.iter().find_map(|a| match a {
            BinAction::OpenLog { action } => Some(action),
            _ => None,
        })
    }

    /// Why the row's Purge from disk can't run now (D-43), if it can't.
    pub fn purge_disabled(&self) -> Option<&str> {
        self.actions.iter().find_map(|a| match a {
            BinAction::Purge { disabled, .. } => disabled.as_deref(),
            _ => None,
        })
    }
}

/// "—", the em dash of an unknown value (a broken take's duration, a size
/// not measured yet).
pub const UNKNOWN: &str = "\u{2014}";

/// "Select a clip on the timeline first": Insert after selection without
/// a timeline selection.
pub const NO_SELECTION: &str = "Select a clip on the timeline first";

/// The takes the bin lists, newest first (G§4.8): ok takes no clip of the
/// timeline uses, and every broken and too-short take. A take being
/// recorded isn't one.
pub fn bin_takes(state: &State) -> Vec<TakeId> {
    let project = &state.project;
    let used: std::collections::BTreeSet<&TakeId> =
        project.timeline.clips.iter().map(|c| &c.take).collect();
    let mut takes: Vec<&TakeId> = project
        .takes
        .iter()
        .filter(|(id, take)| match take.status {
            TakeStatus::Ok => !used.contains(id),
            TakeStatus::Broken | TakeStatus::TooShort => true,
            TakeStatus::Recording => false,
        })
        .map(|(id, _)| id)
        .collect();
    takes.sort_by(|a, b| b.number().cmp(&a.number()).then_with(|| b.cmp(a)));
    takes.into_iter().cloned().collect()
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
    bin_takes(state)
        .into_iter()
        .filter_map(|id| {
            let take = state.project.takes.get(&id)?;
            Some(row(state, &id, take, sizes, offsets, fallback_offset_secs))
        })
        .collect()
}

fn row(
    state: &State,
    id: &TakeId,
    take: &Take,
    sizes: &BTreeMap<TakeId, u64>,
    offsets: &BTreeMap<TakeId, i32>,
    fallback_offset_secs: i32,
) -> BinRow {
    let fps = state.project.canvas.fps;
    let chip = chip(take);
    let duration = match take.status {
        TakeStatus::Broken => UNKNOWN.to_owned(),
        _ => format::timecode(take.duration_frames, fps),
    };
    let hm = recorded_hm(state, id, offsets, fallback_offset_secs).unwrap_or_default();
    let size = sizes
        .get(id)
        .map_or_else(|| UNKNOWN.to_owned(), |b| format::size(*b));
    let folder = format!("takes/{id}/");
    let reveal = BinAction::Reveal {
        action: UiAction::RevealPath(folder.clone()),
    };
    let purge_command = Command::TakePurge {
        takes: vec![id.clone()],
    };
    let purge = BinAction::Purge {
        disabled: crate::view::enabled(state, &purge_command).err(),
        command: purge_command,
    };
    let insertable = take.status == TakeStatus::Ok;
    let actions = match take.status {
        TakeStatus::Broken => vec![
            BinAction::OpenLog {
                action: UiAction::OpenPath(format!("{folder}recorder.log")),
            },
            reveal,
            purge,
        ],
        TakeStatus::TooShort => vec![reveal, purge],
        TakeStatus::Ok | TakeStatus::Recording => vec![
            BinAction::Preview,
            BinAction::InsertAtEnd {
                command: insert_at(id, InsertAt::End),
            },
            BinAction::InsertAfterSelection {
                command: insert_at(id, InsertAt::AfterSelection),
                disabled: state
                    .ui
                    .selection
                    .is_empty()
                    .then(|| NO_SELECTION.to_owned()),
            },
            reveal,
            purge,
        ],
    };
    BinRow {
        take: id.clone(),
        title: format!("Take {}", id.number()),
        chip,
        mono: format!("{duration} \u{b7} {hm} \u{b7} {size}"),
        secondary: secondary_line(state, id, take),
        actions,
        color: format::take_color(id),
        thumbnail: thumbnail(state, take),
        insertable,
    }
}

fn chip(take: &Take) -> Chip {
    match take.status {
        TakeStatus::Broken => Chip::Broken,
        TakeStatus::TooShort => Chip::TooShort,
        _ if take.recovered => Chip::Recovered,
        _ => Chip::Unused,
    }
}

/// What a row's thumbnail shows: the middle second of the take on its
/// thumbnail layer (file time: take time − the stream's start offset).
fn thumbnail(state: &State, take: &Take) -> Thumbnail {
    if take.status == TakeStatus::Broken {
        return Thumbnail::Broken;
    }
    let Some(stream) = thumbnail_stream(take) else {
        return Thumbnail::Blank;
    };
    let fps = state.project.canvas.fps;
    let middle_s = take.duration_frames as f64 / 2.0 * f64::from(fps.den) / f64::from(fps.num);
    let file_s = middle_s - stream.start_offset_ns as f64 / NS_PER_SEC as f64;
    Thumbnail::Image {
        file: stream.file.clone(),
        second: file_s.floor().max(0.0) as u32,
    }
}

/// D-18: why `take` is in the bin.
pub fn secondary_line(state: &State, id: &TakeId, take: &Take) -> String {
    match take.status {
        TakeStatus::TooShort => {
            let n = take.duration_frames;
            let frames = if n == 1 { "frame" } else { "frames" };
            format!("{n} {frames}; takes need {MIN_TAKE_FRAMES}.")
        }
        TakeStatus::Broken => broken_line(take.error.as_deref()),
        TakeStatus::Ok | TakeStatus::Recording => {
            if take.recovered {
                "Recovered after Qrec closed mid-take.".to_owned()
            } else if !take.placed {
                "Cancelled with Esc.".to_owned()
            } else if let Some(by) = replaced_by(state, id) {
                format!("Replaced by take {}.", by.number())
            } else if discarded(state, id) {
                "Discarded with D.".to_owned()
            } else {
                "Removed from the timeline.".to_owned()
            }
        }
    }
}

fn broken_line(error: Option<&str>) -> String {
    let Some(error) = error.map(str::trim).filter(|e| !e.is_empty()) else {
        return "The recording failed.".to_owned();
    };
    if error == EOS_TIMEOUT_REASON {
        return "Files didn't close within 10 s.".to_owned();
    }
    if let Some(rest) = error.strip_prefix("recovery failed: ") {
        return format!("Couldn't be recovered: {}.", first_sentence(rest));
    }
    format!("{}.", format::capitalized(first_sentence(error)))
}

/// `text` up to its first sentence's end, without the period.
fn first_sentence(text: &str) -> &str {
    let text = text.trim();
    let end = text.find(". ").unwrap_or(text.len());
    text[..end].trim_end_matches('.').trim_end()
}

/// The newest other take whose record-over replaced a clip of `id`.
fn replaced_by(state: &State, id: &TakeId) -> Option<TakeId> {
    state
        .project
        .takes
        .iter()
        .filter(|(other, take)| *other != id && take.replaced.iter().any(|c| c.take == *id))
        .map(|(other, _)| other.clone())
        .max_by_key(TakeId::number)
}

/// A history entry "Discard take N" at or before the cursor (`d`, A§5.5).
fn discarded(state: &State, id: &TakeId) -> bool {
    let label = format!("Discard take {}", id.number());
    let history = &state.history;
    history
        .entries
        .iter()
        .any(|e| e.seq <= history.cursor && e.seq > history.base_seq && e.label == label)
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

/// "5 takes", "1 take", "No takes".
fn takes_count(n: usize) -> String {
    match n {
        0 => "No takes".to_owned(),
        1 => "1 take".to_owned(),
        n => format!("{n} takes"),
    }
}

/// The sum of `takes`' sizes, when every one is known.
fn total_size(takes: &[TakeId], sizes: &BTreeMap<TakeId, u64>) -> Option<u64> {
    takes
        .iter()
        .map(|t| sizes.get(t).copied())
        .try_fold(0u64, |sum, b| b.map(|b| sum.saturating_add(b)))
}

/// The bin header: "5 takes · 436.4 MB" ("5 takes" until every size is
/// known; "No takes").
pub fn header(state: &State, sizes: &BTreeMap<TakeId, u64>) -> String {
    let takes = bin_takes(state);
    let count = takes_count(takes.len());
    match total_size(&takes, sizes) {
        Some(bytes) if !takes.is_empty() => format!("{count} \u{b7} {}", format::size(bytes)),
        _ => count,
    }
}

/// The multi-select footer (D-37), in parts: "2 takes selected", "184.1
/// MB" (once every size is known), "Purge from disk" and its command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Footer {
    pub selected: String,
    pub size: Option<String>,
    pub action: String,
    /// `TakePurge` of the selection, oldest take first.
    pub command: Command,
}

/// The footer of a selection of two or more takes; `None` below two.
pub fn footer_view(selection: &[TakeId], sizes: &BTreeMap<TakeId, u64>) -> Option<Footer> {
    if selection.len() < 2 {
        return None;
    }
    let mut takes = selection.to_vec();
    takes.sort_by_key(TakeId::number);
    takes.dedup();
    Some(Footer {
        selected: format!("{} takes selected", takes.len()),
        size: total_size(&takes, sizes).map(format::size),
        action: "Purge from disk".to_owned(),
        command: Command::TakePurge { takes },
    })
}

/// The multi-select footer (D-37): "2 takes selected · 184.1 MB · Purge
/// from disk", with its command; `None` below two takes.
pub fn footer(selection: &[TakeId], sizes: &BTreeMap<TakeId, u64>) -> Option<(String, Command)> {
    let f = footer_view(selection, sizes)?;
    let text = match &f.size {
        Some(size) => format!("{} \u{b7} {size} \u{b7} {}", f.selected, f.action),
        None => format!("{} \u{b7} {}", f.selected, f.action),
    };
    Some((text, f.command))
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
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::history::HistoryEntry;
    use crate::testkit;

    /// BRIEF §3.4's sizes, from the S14 frame.
    fn design_sizes() -> BTreeMap<TakeId, u64> {
        [
            (9, 95_400_000),
            (7, 155_800_000),
            (6, 1_100_000),
            (4, 129_000_000),
            (2, 55_100_000),
        ]
        .into_iter()
        .map(|(n, b)| (TakeId::numbered(n), b))
        .collect()
    }

    /// The fixture's bin (S14, BRIEF §3.4, D-18): take 9 broken by the EOS
    /// timeout, 7 recovered, 6 too short, 4 replaced by take 5, 2 placed and
    /// removed (the fixture has no history: "Discarded with D." needs one).
    #[test]
    fn the_design_bin_is_s14() {
        let mut state = testkit::design_state();
        state.ui.selection = vec![crate::ClipId::from_raw("c2d0")];
        let rows = bin_rows(&state, &design_sizes(), &BTreeMap::new(), 0);
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["Take 9", "Take 7", "Take 6", "Take 4", "Take 2"]);
        assert_eq!(rows[0].mono, "\u{2014} \u{b7} 14:31 \u{b7} 95.4 MB");
        assert_eq!(rows[3].mono, "00:00:51:18 \u{b7} 14:12 \u{b7} 129.0 MB");
        assert_eq!(header(&state, &design_sizes()), "5 takes \u{b7} 436.4 MB");
        insta::assert_json_snapshot!("design_bin_rows", rows);
    }

    /// D-18's lines for what the fixture doesn't have: a discard in the
    /// history, other broken takes, one frame, a cancelled take.
    #[test]
    fn secondary_lines_follow_d18() {
        let mut state = testkit::design_state();
        let entry = HistoryEntry {
            seq: 1,
            ts: "2026-10-14T14:07:00Z".parse().unwrap(),
            label: "Discard take 2".into(),
            cmd: json!({"cmd": "takeDiscardLast"}),
            forward: json_patch::Patch(vec![]),
            inverse: json_patch::Patch(vec![]),
            after_hash: String::new(),
        };
        state.history.entries = Arc::new(vec![entry]);
        state.history.cursor = 1;
        state.history.next_seq = 2;
        let line = |state: &State, n: u32| {
            let id = TakeId::numbered(n);
            secondary_line(state, &id, &state.project.takes[&id])
        };
        assert_eq!(line(&state, 2), "Discarded with D.");
        // Undone: the discard is after the cursor.
        let mut undone = state.clone();
        undone.history.cursor = 0;
        assert_eq!(line(&undone, 2), "Removed from the timeline.");
        assert_eq!(line(&state, 4), "Replaced by take 5.");
        assert_eq!(line(&state, 6), "7 frames; takes need 10.");
        assert_eq!(line(&state, 7), "Recovered after Qrec closed mid-take.");
        assert_eq!(line(&state, 9), "Files didn't close within 10 s.");

        let takes = &mut state.project.takes;
        takes.get_mut(&TakeId::numbered(6)).unwrap().duration_frames = 1;
        let t9 = takes.get_mut(&TakeId::numbered(9)).unwrap();
        t9.error = Some("recovery failed: ffprobe can't read L1-screen-DP-4.mkv".into());
        assert_eq!(line(&state, 6), "1 frame; takes need 10.");
        assert_eq!(
            line(&state, 9),
            "Couldn't be recovered: ffprobe can't read L1-screen-DP-4.mkv."
        );
        let t9 = state.project.takes.get_mut(&TakeId::numbered(9)).unwrap();
        t9.error = Some("the camera vanished. It was unplugged.".into());
        assert_eq!(line(&state, 9), "The camera vanished.");
        state
            .project
            .takes
            .get_mut(&TakeId::numbered(9))
            .unwrap()
            .error = None;
        assert_eq!(line(&state, 9), "The recording failed.");
        // Cancelled with Esc: ok, never placed, not recovered.
        let t7 = state.project.takes.get_mut(&TakeId::numbered(7)).unwrap();
        t7.recovered = false;
        assert_eq!(line(&state, 7), "Cancelled with Esc.");
    }

    #[test]
    fn rows_carry_their_actions_and_thumbnails() {
        let state = testkit::design_state();
        let rows = bin_rows(&state, &BTreeMap::new(), &BTreeMap::new(), 0);
        let kinds = |row: &BinRow| -> Vec<&'static str> {
            row.actions
                .iter()
                .map(|a| match a {
                    BinAction::Preview => "preview",
                    BinAction::InsertAtEnd { .. } => "end",
                    BinAction::InsertAfterSelection { .. } => "after",
                    BinAction::Reveal { .. } => "reveal",
                    BinAction::OpenLog { .. } => "log",
                    BinAction::Purge { .. } => "purge",
                })
                .collect()
        };
        assert_eq!(kinds(&rows[0]), ["log", "reveal", "purge"], "take 9");
        assert_eq!(kinds(&rows[2]), ["reveal", "purge"], "take 6");
        assert_eq!(
            kinds(&rows[3]),
            ["preview", "end", "after", "reveal", "purge"]
        );
        // Nothing selected on the timeline: Insert after selection waits.
        assert!(rows[3].actions.iter().any(|a| matches!(
            a,
            BinAction::InsertAfterSelection { disabled: Some(why), .. } if why == NO_SELECTION
        )));
        assert_eq!(
            rows[0].open_log(),
            Some(&UiAction::OpenPath("takes/take-0009/recorder.log".into()))
        );
        assert_eq!(rows[0].thumbnail, Thumbnail::Broken);
        assert!(matches!(
            &rows[3].thumbnail,
            Thumbnail::Image { second: 25, .. }
        ));
        assert!(rows[3].insertable && !rows[2].insertable && !rows[0].insertable);
        // Sizes not measured yet read "—", and the header counts only.
        assert!(
            rows[3].mono.ends_with("\u{b7} \u{2014}"),
            "{}",
            rows[3].mono
        );
        assert_eq!(header(&state, &BTreeMap::new()), "5 takes");
    }

    /// D-43: Purge from disk follows `view::enabled` (the core refuses a
    /// purge while recording and with `qrec.json` invalid: the button said
    /// nothing until the refusal's toast).
    #[test]
    fn purge_waits_for_the_recording_and_a_valid_qrec_json() {
        use crate::ids::TimerId;
        use crate::state::{RecordMode, RecordPhase};
        let reasons = |state: &State| -> Vec<Option<String>> {
            bin_rows(state, &BTreeMap::new(), &BTreeMap::new(), 0)
                .iter()
                .map(|row| row.purge_disabled().map(str::to_owned))
                .collect()
        };
        let mut state = testkit::design_state();
        assert_eq!(reasons(&state), [None, None, None, None, None]);
        let take = TakeId::numbered(10);
        state.runtime.record = RecordPhase::Recording {
            take: take.clone(),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        let all = |why: &str| vec![Some(why.to_owned()); 5];
        assert_eq!(reasons(&state), all("Not while recording"));
        state.runtime.record = RecordPhase::Finalizing {
            take,
            mode: RecordMode::Append,
            cancelled: false,
            stop_reqs: vec![],
            waiters: vec![],
            discard: None,
        };
        assert_eq!(reasons(&state), all("Not while take 10 is finalizing"));
        state.runtime.record = RecordPhase::Countdown {
            remaining: 3,
            mode: RecordMode::Append,
            timer: TimerId(1),
            start_req: None,
            wait: false,
        };
        assert_eq!(reasons(&state), all("Not during the countdown"));
        state.runtime.record = RecordPhase::Idle;
        state.runtime.disk_invalid = Some("expected `,`".into());
        assert_eq!(reasons(&state), all("qrec.json on disk is invalid"));
    }

    #[test]
    fn the_footer_counts_two_or_more() {
        let sizes = design_sizes();
        let (t2, t4) = (TakeId::numbered(2), TakeId::numbered(4));
        assert_eq!(footer(std::slice::from_ref(&t4), &sizes), None);
        let (text, cmd) = footer(&[t4.clone(), t2.clone()], &sizes).unwrap();
        assert_eq!(
            text,
            "2 takes selected \u{b7} 184.1 MB \u{b7} Purge from disk"
        );
        assert_eq!(
            cmd,
            Command::TakePurge {
                takes: vec![t2.clone(), t4.clone()]
            },
            "oldest first"
        );
        let unknown = footer_view(&[t2, TakeId::numbered(99)], &sizes).unwrap();
        assert_eq!(unknown.size, None);
    }

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
        let rows = bin_rows(&state, &BTreeMap::new(), &offsets, est);
        let row4 = rows.iter().find(|r| r.take == take_4).unwrap();
        assert!(row4.mono.contains("14:12"), "{}", row4.mono);
    }
}
