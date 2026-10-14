//! Undo history (A§7): entries with forward/inverse JSON patches over the
//! undoable view of the project.
//!
//! `commit.rs` is foundation-owned (every handler finishes through it);
//! `view`, `diff`, `ops`, `purge` and `external` belong to the history owner.

pub mod commit;
pub mod diff;
pub mod external;
pub mod ops;
pub mod purge;
pub mod view;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use commit::{Committed, commit, persist};

/// One line of `.qrec/history.jsonl` (A§7.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryEntry {
    /// Never reused.
    pub seq: u64,
    pub ts: DateTime<Utc>,
    /// "Split clip c7f3 at 00:00:13:20".
    pub label: String,
    /// The `Command`, `{"event":…}` or `{"external":true}`.
    pub cmd: Value,
    /// Applied to `view::project_value` to redo.
    pub forward: json_patch::Patch,
    /// Applied to `view::project_value` to undo.
    pub inverse: json_patch::Patch,
    /// Informational: hash of the undoable view after the entry (`qrec history`).
    pub after_hash: String,
}
