//! Read-only requests (A§4.5). Answered by `crate::query::query` with JSON
//! (the typed views are in `protocol::reply`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::TakeId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "query",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Query {
    /// `StatusView`.
    Status,
    /// The whole `State` (`qrec status --json`).
    State,
    /// `[TakeRow]`; `unused_only` = the bin.
    TakeList {
        #[serde(default)]
        unused_only: bool,
    },
    TakeShow {
        take: TakeId,
    },
    /// `[ClipRow]`.
    ClipList,
    /// `[HistoryRow]`, newest last.
    History {
        limit: Option<usize>,
    },
    /// The effective keymap.
    Keys,
}
