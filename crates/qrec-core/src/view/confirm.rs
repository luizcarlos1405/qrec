//! The confirmation modal (G§4.9, C24, SPEC §11): purge (S18), an fps
//! change (S18b), an overwrite, clearing the history, and quitting while
//! exporting (D-12). Owner: WP6. Stub until WP6 lands: the signatures are
//! final, the bodies return defaults.

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::protocol::command::Command;
use crate::state::State;

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

/// The modal for `cmd`, which the core refused with `error`
/// (`confirmation_required`, its `details` listing what would be lost).
/// Confirming re-dispatches `cmd` with `confirmed: true`.
pub fn modal(cmd: &Command, error: &CoreError, state: &State) -> ModalContent {
    let _ = (cmd, error, state);
    ModalContent::default()
}

/// The modal of quitting while an export runs at `percent` (D-12): "Quit
/// while exporting?", Keep exporting (focused) and Cancel export and quit.
pub fn quit_while_exporting(percent: u8) -> ModalContent {
    let _ = percent;
    ModalContent::default()
}
