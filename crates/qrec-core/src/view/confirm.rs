//! The confirmation modal (G§4.9, C24, SPEC §11): purge (S18), an fps
//! change (S18b), an overwrite, clearing the history, and quitting while
//! exporting (D-12). Owner: WP6. Stub until WP6 lands: the signatures are
//! final and the bodies return defaults, except the overwrite modal (WP0a:
//! it needs the GUI's facts, [`ConfirmFacts`], so its copy is built here
//! with them).

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::CoreError;
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
const TITLE: &str = "Get out of my swamp?";

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
    match cmd {
        Command::Export { .. } => overwrite(error, facts, "Exporting here overwrites it."),
        Command::FrameSave { .. } => {
            overwrite(error, facts, "Saving the frame here overwrites it.")
        }
        _ => ModalContent::default(),
    }
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
    ModalContent {
        title: TITLE.to_owned(),
        lead: format!("Replace {name}?"),
        body: Some(format!("{shown} already exists{what}. {consequence}")),
        list: Vec::new(),
        cancel: "Cancel".to_owned(),
        confirm: "Replace file".to_owned(),
    }
}

/// The modal of quitting while an export runs at `percent` (D-12): "Quit
/// while exporting?", Keep exporting (focused) and Cancel export and quit.
pub fn quit_while_exporting(percent: u8) -> ModalContent {
    let _ = percent;
    ModalContent::default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::protocol::command::{ExportFormat, Quality};
    use crate::testkit;
    use crate::time::TimeArg;

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
}
