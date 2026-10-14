//! Progress of long-running requests (A§4.5): sent with `Effect::Progress`
//! to the requester, shown as a progress line or bar.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::TakeId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Progress {
    pub phase: ProgressPhase,
    pub done: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<u64>,
    /// "Rendering clip 4/12", "Stitching", …
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take: Option<TakeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ProgressPhase {
    Analyze,
    Render,
    Stitch,
    Record,
    Finalize,
}
