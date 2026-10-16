//! The confirmation modal (G§4.9, C24, SPEC §11): purge (S18), an fps
//! change (S18b), an overwrite, clearing the history, and quitting while
//! exporting (D-12). Owner: WP6.
//!
//! Every modal reads its facts from the refusal's `details` (what would be
//! lost, A§4.8); the overwrite also needs [`ConfirmFacts`], what the GUI saw
//! of the file. The flavored title comes first, then a plain lead with
//! exact counts and sizes, then the loss list (DESIGN Copy).

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::CoreError;
use crate::ids::TakeId;
use crate::model::Fps;
use crate::protocol::command::Command;
use crate::state::State;
use crate::view::format;

/// A modal's content. The cancel button has the initial focus.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModalContent {
    /// "Get out of my swamp?"
    pub title: String,
    /// "Purge take 2 and take 4 from disk?"
    pub lead: String,
    pub body: Option<String>,
    /// Rows: text and an optional right column (a size, a range).
    pub list: Vec<(String, Option<String>)>,
    /// "Cancel"; "Keep exporting" for [`quit_while_exporting`].
    pub cancel: String,
    /// The destructive button: "Purge 2 takes", "Change fps", "Replace
    /// file", "Clear history", "Continue".
    pub confirm: String,
}

/// What the GUI knows about a confirmation that the core doesn't (G§4.9):
/// how it shows each file the error's `details.files` name, when each was
/// last modified (it stats them on a worker), and the local time now.
/// Modals that name no existing file (purge, an fps change, clearing the
/// history) take `ConfirmFacts::default()`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmFacts {
    /// By the path `details.files[].path` gives (absolute for a hand-picked
    /// output).
    pub files: BTreeMap<String, SeenFile>,
    /// Now on the local clock, for "today".
    pub now: DateTime<FixedOffset>,
}

/// A file a confirmation names, as the GUI saw it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenFile {
    /// `format::display_path(path, project_dir, home)`:
    /// "~/Videos/talk-final.mp4", "exports/talk.mp4".
    pub display: String,
    /// Its mtime on the local clock of that instant (the offset in force
    /// then); `None` when the stat failed.
    pub modified: Option<DateTime<FixedOffset>>,
}

/// The title every confirmation shares (S18).
pub const TITLE: &str = "Get out of my swamp?";

/// The prefix of every `confirmation_required` message.
const REQUIRED: &str = "confirmation required: ";

/// The modal for `cmd`, which the core refused with `error`
/// (`confirmation_required`, its `details` listing what would be lost),
/// with what the GUI saw of the files it names (`facts`). Confirming
/// re-dispatches `cmd` with `confirmed: true`.
pub fn modal(
    cmd: &Command,
    error: &CoreError,
    state: &State,
    facts: &ConfirmFacts,
) -> ModalContent {
    let _ = state;
    let details = error.details.as_ref().unwrap_or(&Value::Null);
    match cmd {
        Command::TakePurge { takes } => purge(details, takes),
        Command::ProjectSet {
            canvas: Some(spec), ..
        } if spec.fps.is_some() && details.get("clips").is_some() => fps_change(details),
        Command::Export { .. } => overwrite(error, facts, "Exporting here overwrites it."),
        Command::FrameSave { .. } => {
            overwrite(error, facts, "Saving the frame here overwrites it.")
        }
        Command::HistoryClear => history_clear(details),
        _ => fallback(error),
    }
}

/// The confirmation's buttons with the destructive verb `confirm`.
fn content(
    lead: String,
    body: Option<String>,
    list: Vec<(String, Option<String>)>,
    confirm: String,
) -> ModalContent {
    ModalContent {
        title: TITLE.to_owned(),
        lead,
        body,
        list,
        cancel: "Cancel".to_owned(),
        confirm,
    }
}

/// The number of a take id the details give (`take-0004` → 4; 0 for
/// anything else).
fn take_number(id: &str) -> u32 {
    id.parse::<TakeId>().map_or(0, |t| t.number())
}

/// `n` and the noun, singular or plural: "1 file", "10 files".
fn counted(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", format::count(n), if n == 1 { one } else { many })
}

/// S18, `TakePurge` (`details {takes, files: [{path, bytes}], bytes,
/// historyEntriesLost}`): "Purge take 2 and take 4 from disk?" / "This
/// permanently deletes 10 files (184.1 MB). It can't be undone, and the 6
/// history entries that mention these takes are removed too." / the files
/// with their sizes / "Purge 2 takes".
fn purge(details: &Value, takes: &[TakeId]) -> ModalContent {
    // The core's own list (deduplicated, in order) when it gave one.
    let numbers: Vec<u32> = match details.get("takes").and_then(Value::as_array) {
        Some(ids) => ids
            .iter()
            .filter_map(Value::as_str)
            .map(take_number)
            .collect(),
        None => takes.iter().map(TakeId::number).collect(),
    };
    let lead = match numbers.as_slice() {
        [one] => format!("Purge take {one} from disk?"),
        [a, b] => format!("Purge take {a} and take {b} from disk?"),
        many => format!("Purge {} takes from disk?", many.len()),
    };
    let confirm = match numbers.as_slice() {
        [one] => format!("Purge take {one}"),
        many => format!("Purge {} takes", many.len()),
    };
    let files: Vec<(String, Option<u64>)> = details
        .get("files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .map(|f| {
                    (
                        f.get("path")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_owned(),
                        f.get("bytes").and_then(Value::as_u64),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let bytes = details
        .get("bytes")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| files.iter().filter_map(|(_, b)| *b).sum());
    let lost = details
        .get("historyEntriesLost")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut body = format!(
        "This permanently deletes {} ({}). It can't be undone",
        counted(files.len() as u64, "file", "files"),
        format::size(bytes)
    );
    if lost > 0 {
        let these = if numbers.len() == 1 {
            "this take"
        } else {
            "these takes"
        };
        if lost == 1 {
            body.push_str(&format!(
                ", and the 1 history entry that mentions {these} is removed too"
            ));
        } else {
            body.push_str(&format!(
                ", and the {} history entries that mention {these} are removed too",
                format::count(lost)
            ));
        }
    }
    body.push('.');
    let mut files = files;
    in_take_order(&mut files);
    let list = files
        .into_iter()
        .map(|(path, bytes)| (path, bytes.map(format::size)))
        .collect();
    content(lead, Some(body), list, confirm)
}

/// The loss list as BRIEF S18 writes it: each take's layers (`L1…`), then
/// its tracks (`A1…`), then its other files (the log), each group by name;
/// the takes stay in the core's order. (The shell lists a take's files by
/// path, which puts the tracks first.)
fn in_take_order(files: &mut [(String, Option<u64>)]) {
    let folders: Vec<String> =
        files
            .iter()
            .map(|(path, _)| folder(path).to_owned())
            .fold(Vec::new(), |mut seen, f| {
                if !seen.contains(&f) {
                    seen.push(f);
                }
                seen
            });
    let rank = |name: &str| {
        let mut chars = name.chars();
        match (chars.next(), chars.next()) {
            (Some('L'), Some(d)) if d.is_ascii_digit() => 0,
            (Some('A'), Some(d)) if d.is_ascii_digit() => 1,
            _ => 2,
        }
    };
    files.sort_by_cached_key(|(path, _)| {
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        let at = folders.iter().position(|f| f == folder(path)).unwrap_or(0);
        (at, rank(&name), name)
    });
}

/// A path's folder ("takes/take-0002"), "" for a bare name.
fn folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The rate in `details[key]` (`{"num": 25, "den": 1}`).
fn fps_of(details: &Value, key: &str) -> Option<Fps> {
    serde_json::from_value(details.get(key)?.clone()).ok()
}

/// A `[in, out)` pair of `details`.
fn bounds(value: Option<&Value>) -> Option<(u64, u64)> {
    let pair = value?.as_array()?;
    Some((pair.first()?.as_u64()?, pair.get(1)?.as_u64()?))
}

/// S18b, `ProjectSet` with a new fps (`details {from, to, clips: [{id,
/// take, before, after}], removedClips: [{id, take, before}]}`): "Change
/// the canvas to 25 fps?" / "Clip bounds are re-quantized to the new frame
/// rate. These 10 clips move by up to 1 frame:" / every clip "c7f3 · take
/// 1 · [0, 410) → [0, 342)" / "Change fps". A clip that ends up empty is
/// named before the list ("c1b6 becomes empty and is removed.").
fn fps_change(details: &Value) -> ModalContent {
    let from = fps_of(details, "from");
    let to = fps_of(details, "to");
    let lead = match to {
        Some(to) => format!("Change the canvas to {}?", format::fps(to)),
        None => "Change the canvas's frame rate?".to_owned(),
    };
    let rows = |key: &str| -> Vec<Value> {
        details
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let take_number = |row: &Value| {
        row.get("take")
            .and_then(Value::as_str)
            .map_or(0, take_number)
    };
    let id = |row: &Value| {
        row.get("id")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned()
    };
    let clips = rows("clips");
    let removed = rows("removedClips");
    // How far a bound lands from its exact time at the new rate, in new
    // frames (rounded up; at least 1: "up to").
    let mut shift = 1u128;
    if let (Some(from), Some(to)) = (from, to) {
        // exact = before × to.num × from.den / (to.den × from.num)
        let n = u128::from(to.num) * u128::from(from.den);
        let d = (u128::from(to.den) * u128::from(from.num)).max(1);
        for row in &clips {
            if let (Some(before), Some(after)) =
                (bounds(row.get("before")), bounds(row.get("after")))
            {
                for (b, a) in [(before.0, after.0), (before.1, after.1)] {
                    let off = (u128::from(a) * d).abs_diff(u128::from(b) * n);
                    shift = shift.max(off.div_ceil(d));
                }
            }
        }
    }
    let mut body = "Clip bounds are re-quantized to the new frame rate.".to_owned();
    for row in &removed {
        body.push_str(&format!(" {} becomes empty and is removed.", id(row)));
    }
    let frames = if shift == 1 { "frame" } else { "frames" };
    match clips.len() {
        0 => body.push_str(" No clips are on the timeline: only the takes' lengths change."),
        1 => body.push_str(&format!(" This clip moves by up to {shift} {frames}:")),
        n => body.push_str(&format!(
            " These {} clips move by up to {shift} {frames}:",
            format::count(n as u64)
        )),
    }
    let list = clips
        .iter()
        .map(|row| {
            let range = |b: Option<(u64, u64)>| {
                b.map_or_else(|| "?".to_owned(), |(i, o)| format!("[{i}, {o})"))
            };
            (
                format!(
                    "{} \u{b7} take {} \u{b7} {} \u{2192} {}",
                    id(row),
                    take_number(row),
                    range(bounds(row.get("before"))),
                    range(bounds(row.get("after")))
                ),
                None,
            )
        })
        .collect();
    content(lead, Some(body), list, "Change fps".to_owned())
}

/// An `Export` or `FrameSave` onto an existing file (`details {files:
/// [{path, bytes}]}`): "Replace talk-final.mp4?" / "~/Videos/talk-final.mp4
/// already exists (91.2 MB, modified today at 15:02). Exporting here
/// overwrites it." / Replace file. A size or time nobody knows is left out.
fn overwrite(error: &CoreError, facts: &ConfirmFacts, consequence: &str) -> ModalContent {
    let file = error
        .details
        .as_ref()
        .and_then(|d| d.get("files"))
        .and_then(Value::as_array)
        .and_then(|files| files.first());
    let path = file
        .and_then(|f| f.get("path"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let bytes = file.and_then(|f| f.get("bytes")).and_then(Value::as_u64);
    let seen = facts.files.get(path);
    let shown = seen.map_or(path, |s| s.display.as_str());
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut known = Vec::new();
    if let Some(bytes) = bytes {
        known.push(format::size(bytes));
    }
    if let Some(at) = seen.and_then(|s| s.modified) {
        known.push(format!("modified {}", format::local_when(at, facts.now)));
    }
    let what = if known.is_empty() {
        String::new()
    } else {
        format!(" ({})", known.join(", "))
    };
    content(
        format!("Replace {name}?"),
        Some(format!("{shown} already exists{what}. {consequence}")),
        Vec::new(),
        "Replace file".to_owned(),
    )
}

/// `HistoryClear` (`details {historyEntriesLost}`, WP9's palette row):
/// "Clear undo history?" / "This removes 128 undo steps from
/// .qrec/history.jsonl. The timeline stays as it is, but you can't undo
/// anything done before now." / Clear history.
fn history_clear(details: &Value) -> ModalContent {
    let lost = details
        .get("historyEntriesLost")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    content(
        "Clear undo history?".to_owned(),
        Some(format!(
            "This removes {} from .qrec/history.jsonl. The timeline stays as it is, but you \
             can't undo anything done before now.",
            counted(lost, "undo step", "undo steps")
        )),
        Vec::new(),
        "Clear history".to_owned(),
    )
}

/// Any other confirmation: the core's message as the lead ("Restore entry
/// 12 …"), without its "confirmation required: " prefix; the CLI's hint
/// (`--yes`) isn't the window's.
fn fallback(error: &CoreError) -> ModalContent {
    let what = error
        .message
        .strip_prefix(REQUIRED)
        .unwrap_or(&error.message);
    content(
        format::capitalized(what),
        None,
        Vec::new(),
        "Continue".to_owned(),
    )
}

/// The modal of quitting while an export runs at `percent` (D-12): "Quit
/// while exporting?", Keep exporting (focused) and Cancel export and quit.
pub fn quit_while_exporting(percent: u8) -> ModalContent {
    ModalContent {
        title: "Quit while exporting?".to_owned(),
        lead: format!("The export is {percent} % done."),
        body: Some("Quitting cancels it. Nothing is written to exports/.".to_owned()),
        list: Vec::new(),
        cancel: "Keep exporting".to_owned(),
        confirm: "Cancel export and quit".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::Fps;
    use crate::protocol::command::{CanvasSpec, ExportFormat, Nullable, Quality};
    use crate::testkit;
    use crate::time::TimeArg;
    use crate::{Ctx, apply};

    const PATH: &str = "/home/me/Videos/talk-final.mp4";

    /// The core's refusal of a hand-picked output that exists
    /// (`apply::output::checked_out`).
    fn exists(bytes: Option<u64>) -> CoreError {
        let mut file = json!({ "path": PATH });
        if let Some(bytes) = bytes {
            file["bytes"] = json!(bytes);
        }
        CoreError::confirmation_required(format!("overwrite {PATH}"))
            .with_details(json!({ "files": [file] }))
    }

    fn at(local: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(local).unwrap()
    }

    fn export() -> Command {
        Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Standard,
            codec: None,
            out: Some(PATH.into()),
        }
    }

    fn fps_command(fps: u32) -> Command {
        Command::ProjectSet {
            name: None,
            canvas: Some(CanvasSpec {
                width: None,
                height: None,
                fps: Some(Fps::new(fps, 1)),
            }),
            background: None,
            countdown_secs: None,
            encoder: None,
            vaapi_device: Nullable::Keep,
        }
    }

    /// The core's real refusal of `cmd` on `state`.
    fn refusal(state: &State, ctx: &Ctx, cmd: &Command) -> CoreError {
        let err = apply(state, ctx, cmd.clone()).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ConfirmationRequired);
        err
    }

    /// The shell lists a take's files by path (the tracks first): the loss
    /// list reads as BRIEF S18's, the layers, the tracks, then the log of
    /// each take, the takes in the core's order.
    #[test]
    fn the_loss_list_reads_layers_tracks_then_the_log() {
        let names = [
            "A1-mic.mka",
            "A2-system.mka",
            "L1-screen-DP-4.mkv",
            "L2-camera-0.mkv",
            "recorder.log",
        ];
        let files: Vec<Value> = [4, 2]
            .iter()
            .flat_map(|take| {
                names.iter().map(move |name| {
                    json!({"path": format!("takes/take-{take:04}/{name}"), "bytes": 1000})
                })
            })
            .collect();
        let takes = vec![TakeId::numbered(4), TakeId::numbered(2)];
        let error = CoreError::confirmation_required("purge take-0004 and take-0002")
            .with_details(json!({"takes": takes, "files": files, "bytes": 10_000}));
        let cmd = Command::TakePurge { takes };
        let content = modal(
            &cmd,
            &error,
            &testkit::design_state(),
            &ConfirmFacts::default(),
        );
        let paths: Vec<&str> = content.list.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            [
                "takes/take-0004/L1-screen-DP-4.mkv",
                "takes/take-0004/L2-camera-0.mkv",
                "takes/take-0004/A1-mic.mka",
                "takes/take-0004/A2-system.mka",
                "takes/take-0004/recorder.log",
                "takes/take-0002/L1-screen-DP-4.mkv",
                "takes/take-0002/L2-camera-0.mkv",
                "takes/take-0002/A1-mic.mka",
                "takes/take-0002/A2-system.mka",
                "takes/take-0002/recorder.log",
            ]
        );
    }

    /// S18: the purge of takes 2 and 4 (BRIEF §5: 10 files, 184.1 MB, 6
    /// history entries), with the core's details as `qrec take purge` sees
    /// them.
    #[test]
    fn the_purge_modal_is_s18() {
        let state = testkit::design_state();
        let sizes: [(u32, [(&str, u64); 5]); 2] = [
            (
                2,
                [
                    ("L1-screen-DP-4.mkv", 33_000_000),
                    ("L2-camera-0.mkv", 16_600_000),
                    ("A1-mic.mka", 2_000_000),
                    ("A2-system.mka", 3_500_000),
                    ("recorder.log", 12_000),
                ],
            ),
            (
                4,
                [
                    ("L1-screen-DP-4.mkv", 77_300_000),
                    ("L2-camera-0.mkv", 38_800_000),
                    ("A1-mic.mka", 4_600_000),
                    ("A2-system.mka", 8_300_000),
                    ("recorder.log", 14_000),
                ],
            ),
        ];
        let files: Vec<Value> = sizes
            .iter()
            .flat_map(|(take, files)| {
                files.iter().map(move |(name, bytes)| {
                    json!({"path": format!("takes/take-{take:04}/{name}"), "bytes": bytes})
                })
            })
            .collect();
        let bytes: u64 = sizes
            .iter()
            .flat_map(|(_, f)| f.iter().map(|(_, b)| b))
            .sum();
        let takes = vec![TakeId::numbered(2), TakeId::numbered(4)];
        let error =
            CoreError::confirmation_required("purge take-0002 and take-0004").with_details(json!({
                "takes": takes,
                "files": files,
                "bytes": bytes,
                "historyEntriesLost": 6,
                "oldestKept": 3,
            }));
        let cmd = Command::TakePurge {
            takes: takes.clone(),
        };
        let content = modal(&cmd, &error, &state, &ConfirmFacts::default());
        assert_eq!(content.title, "Get out of my swamp?");
        assert_eq!(content.lead, "Purge take 2 and take 4 from disk?");
        assert_eq!(
            content.body.as_deref(),
            Some(
                "This permanently deletes 10 files (184.1 MB). It can't be undone, and the 6 \
                 history entries that mention these takes are removed too."
            )
        );
        assert_eq!(content.list.len(), 10);
        assert_eq!(
            content.list[0],
            (
                "takes/take-0002/L1-screen-DP-4.mkv".to_owned(),
                Some("33.0 MB".to_owned())
            )
        );
        assert_eq!(content.list[4].1.as_deref(), Some("12 KB"));
        assert_eq!(
            (content.cancel.as_str(), content.confirm.as_str()),
            ("Cancel", "Purge 2 takes")
        );
        insta::assert_json_snapshot!(content);

        // One take, one file, no history lost; three takes.
        let one = CoreError::confirmation_required("purge take-0004").with_details(json!({
            "takes": ["take-0004"],
            "files": [{"path": "takes/take-0004/recorder.log", "bytes": 14_000}],
            "bytes": 14_000,
            "historyEntriesLost": 0,
        }));
        let content = modal(
            &Command::TakePurge {
                takes: vec![TakeId::numbered(4)],
            },
            &one,
            &state,
            &ConfirmFacts::default(),
        );
        assert_eq!(content.lead, "Purge take 4 from disk?");
        assert_eq!(
            content.body.as_deref(),
            Some("This permanently deletes 1 file (14 KB). It can't be undone.")
        );
        assert_eq!(content.confirm, "Purge take 4");
        let three = CoreError::confirmation_required("purge").with_details(json!({
            "takes": ["take-0002", "take-0004", "take-0006"],
            "files": [],
            "bytes": 0,
            "historyEntriesLost": 1,
        }));
        let content = modal(
            &Command::TakePurge {
                takes: vec![
                    TakeId::numbered(2),
                    TakeId::numbered(4),
                    TakeId::numbered(6),
                ],
            },
            &three,
            &state,
            &ConfirmFacts::default(),
        );
        assert_eq!(content.lead, "Purge 3 takes from disk?");
        assert_eq!(
            content.body.as_deref(),
            Some(
                "This permanently deletes 0 files (0 KB). It can't be undone, and the 1 history \
                 entry that mentions these takes is removed too."
            )
        );
        assert_eq!(content.confirm, "Purge 3 takes");
    }

    /// S18b: 30 → 25 fps on the design project, from the core's real
    /// refusal: every clip listed, "up to 1 frame".
    #[test]
    fn the_fps_change_modal_is_s18b() {
        let state = testkit::design_state();
        let ctx = testkit::ctx(testkit::CREATED_AT, 1);
        let cmd = fps_command(25);
        let error = refusal(&state, &ctx, &cmd);
        let content = modal(&cmd, &error, &state, &ConfirmFacts::default());
        assert_eq!(content.lead, "Change the canvas to 25 fps?");
        assert_eq!(
            content.body.as_deref(),
            Some(
                "Clip bounds are re-quantized to the new frame rate. These 10 clips move by up \
                 to 1 frame:"
            )
        );
        let rows: Vec<&str> = content.list.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            &rows[..4],
            [
                "c7f3 \u{b7} take 1 \u{b7} [0, 410) \u{2192} [0, 342)",
                "c8a1 \u{b7} take 1 \u{b7} [455, 912) \u{2192} [379, 760)",
                "c1b2 \u{b7} take 3 \u{b7} [0, 268) \u{2192} [0, 224)",
                "c1b3 \u{b7} take 3 \u{b7} [304, 611) \u{2192} [253, 510)",
            ]
        );
        assert_eq!(rows.len(), 10);
        assert!(content.list.iter().all(|(_, right)| right.is_none()));
        assert_eq!(content.confirm, "Change fps");
        insta::assert_json_snapshot!(content);
    }

    /// A clip that ends up empty is named; an exact change (30 → 60) still
    /// reads "up to 1 frame"; one clip reads in the singular.
    #[test]
    fn fps_change_variants() {
        let state = testkit::design_state();
        let details = json!({
            "from": {"num": 60, "den": 1},
            "to": {"num": 30, "den": 1},
            "clips": [{"id": "c001", "take": "take-0001", "before": [3, 11], "after": [1, 6]}],
            "removedClips": [{"id": "c002", "take": "take-0001", "before": [11, 12]}],
        });
        let error = CoreError::confirmation_required("change").with_details(details);
        let content = modal(&fps_command(30), &error, &state, &ConfirmFacts::default());
        assert_eq!(
            content.body.as_deref(),
            Some(
                "Clip bounds are re-quantized to the new frame rate. c002 becomes empty and is \
                 removed. This clip moves by up to 1 frame:"
            )
        );
        let ctx = testkit::ctx(testkit::CREATED_AT, 1);
        let double = fps_command(60);
        let error = refusal(&state, &ctx, &double);
        let content = modal(&double, &error, &state, &ConfirmFacts::default());
        assert_eq!(content.lead, "Change the canvas to 60 fps?");
        assert!(
            content
                .body
                .as_deref()
                .is_some_and(|b| b.ends_with("These 10 clips move by up to 1 frame:")),
            "{content:?}"
        );
    }

    /// S18's export overwrite: the path as the GUI shows it and the mtime
    /// it read, neither of which the core has (G§4.9).
    #[test]
    fn the_overwrite_modal_shows_what_the_gui_saw() {
        let state = testkit::design_state();
        let facts = ConfirmFacts {
            files: [(
                PATH.to_owned(),
                SeenFile {
                    display: format::display_path(
                        PATH,
                        "/home/me/videos/rust-traits-talk",
                        Some("/home/me"),
                    ),
                    modified: Some(at("2026-10-15T15:02:41-04:00")),
                },
            )]
            .into(),
            now: at("2026-10-15T15:31:00-04:00"),
        };
        let content = modal(&export(), &exists(Some(91_234_567)), &state, &facts);
        assert_eq!(
            content,
            ModalContent {
                title: "Get out of my swamp?".into(),
                lead: "Replace talk-final.mp4?".into(),
                body: Some(
                    "~/Videos/talk-final.mp4 already exists (91.2 MB, modified today at \
                     15:02). Exporting here overwrites it."
                        .into()
                ),
                list: Vec::new(),
                cancel: "Cancel".into(),
                confirm: "Replace file".into(),
            }
        );

        // A frame save says so; a file modified on another day reads its date.
        let frame = Command::FrameSave {
            at: TimeArg::frames(0),
            layer: None,
            out: Some(PATH.into()),
        };
        let mut older = facts.clone();
        older.files.get_mut(PATH).unwrap().modified = Some(at("2026-10-14T23:59:00-04:00"));
        let content = modal(&frame, &exists(Some(91_234_567)), &state, &older);
        assert_eq!(
            content.body.as_deref(),
            Some(
                "~/Videos/talk-final.mp4 already exists (91.2 MB, modified Oct 14 at 23:59). \
                 Saving the frame here overwrites it."
            )
        );

        // Nothing the GUI saw (the stat failed) and no size: the path as given.
        let content = modal(&export(), &exists(None), &state, &ConfirmFacts::default());
        assert_eq!(
            content.body.as_deref(),
            Some("/home/me/Videos/talk-final.mp4 already exists. Exporting here overwrites it.")
        );
    }

    #[test]
    fn clearing_the_history_and_other_confirmations() {
        let state = testkit::design_state();
        let error = CoreError::confirmation_required("clear the undo history (128 entries)")
            .with_details(json!({"historyEntriesLost": 128}));
        let content = modal(
            &Command::HistoryClear,
            &error,
            &state,
            &ConfirmFacts::default(),
        );
        assert_eq!(content.lead, "Clear undo history?");
        assert_eq!(
            content.body.as_deref(),
            Some(
                "This removes 128 undo steps from .qrec/history.jsonl. The timeline stays as it \
                 is, but you can't undo anything done before now."
            )
        );
        assert_eq!(content.confirm, "Clear history");

        let restore = CoreError::confirmation_required("restore entry 12: undoes 3 entries");
        let content = modal(
            &Command::HistoryRestore { seq: 12 },
            &restore,
            &state,
            &ConfirmFacts::default(),
        );
        assert_eq!(content.lead, "Restore entry 12: undoes 3 entries");
        assert_eq!(content.body, None);
        assert_eq!(content.confirm, "Continue");
    }

    #[test]
    fn quitting_while_exporting() {
        let content = quit_while_exporting(42);
        assert_eq!(
            content,
            ModalContent {
                title: "Quit while exporting?".into(),
                lead: "The export is 42 % done.".into(),
                body: Some("Quitting cancels it. Nothing is written to exports/.".into()),
                list: Vec::new(),
                cancel: "Keep exporting".into(),
                confirm: "Cancel export and quit".into(),
            }
        );
    }
}
