//! Merging an external edit of `qrec.json` (A§7.4). Owner: history.
//! Stub: signatures only.
#![allow(clippy::todo, unused_variables)]

use crate::error::CoreError;
use crate::model::Project;
use crate::state::State;

/// The merged project and the warnings to show.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalMerge {
    pub project: Project,
    /// "takes can only be removed with `qrec take purge`", ignored pointers, …
    pub warnings: Vec<String>,
}

/// Merges the undoable fields of `file` into the current project; facts
/// (take status, streams, durations, …) stay as in memory (A§7.4). The
/// caller commits the result as an "External edit" entry.
pub fn merge_external(state: &State, file: Project) -> Result<ExternalMerge, CoreError> {
    todo!("history: external edit merge (A§7.4)")
}
