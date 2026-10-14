//! Command results and the typed views commands and queries return (A§4.5).
//! All camelCase.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{ClipId, Frame, TakeId};
use crate::model::TakeStatus;
use crate::state::{HostKind, Job, RecordMode, RecordPhase};

/// What a successful command returns (`{"ok":true,"data":CmdResult}` on the wire).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CmdResult {
    /// The new history cursor for undoable changes and undo/redo.
    pub seq: Option<u64>,
    /// Ids created.
    #[serde(default)]
    pub created: Vec<String>,
    /// Ids removed.
    #[serde(default)]
    pub removed: Vec<String>,
    /// One human line.
    pub message: String,
    /// A command-specific payload (one of the views below), or null.
    #[serde(default)]
    pub result: Value,
}

impl CmdResult {
    /// A result with only a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            seq: None,
            created: Vec::new(),
            removed: Vec::new(),
            message: message.into(),
            result: Value::Null,
        }
    }

    pub fn with_seq(mut self, seq: Option<u64>) -> Self {
        self.seq = seq;
        self
    }

    pub fn with_created(mut self, ids: impl IntoIterator<Item = impl ToString>) -> Self {
        self.created
            .extend(ids.into_iter().map(|id| id.to_string()));
        self
    }

    pub fn with_removed(mut self, ids: impl IntoIterator<Item = impl ToString>) -> Self {
        self.removed
            .extend(ids.into_iter().map(|id| id.to_string()));
        self
    }

    /// Sets `result` to the JSON of a view.
    pub fn with_result(mut self, view: &impl Serialize) -> Self {
        self.result = serde_json::to_value(view).expect("views always serialize");
        self
    }
}

/// `Query::Status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusView {
    /// The project name.
    pub project: String,
    /// The project directory; filled in by the shell (`None` from the core).
    pub path: Option<String>,
    pub initialized: bool,
    pub host: HostKind,
    /// The host's pid; filled in by the shell (`None` from the core).
    pub pid: Option<u32>,
    pub record: RecordPhase,
    pub jobs: Vec<Job>,
    /// Number of takes in the pool.
    pub takes: usize,
    /// Number of clips on the timeline.
    pub clips: usize,
    pub duration_frames: Frame,
    /// The history cursor.
    pub cursor: u64,
}

/// One row of `Query::TakeList`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TakeRow {
    pub id: TakeId,
    pub status: TakeStatus,
    pub recorded_at: DateTime<Utc>,
    pub duration_frames: Frame,
    /// Clips on the timeline that use this take.
    pub clips: usize,
    /// No clip uses it (it is in the bin).
    pub unused: bool,
    pub error: Option<String>,
}

/// One row of `Query::ClipList`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipRow {
    pub id: ClipId,
    pub take: TakeId,
    #[serde(rename = "in")]
    pub in_: Frame,
    pub out: Frame,
    /// Timeline position of the first frame.
    pub start: Frame,
    pub duration: Frame,
}

/// One row of `Query::History`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryRow {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub label: String,
    /// This entry is the cursor.
    pub current: bool,
}

/// `Autocut` with `dryRun` (and the result of a real run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutocutPlanView {
    pub clips: Vec<AutocutClipView>,
    pub skipped: Vec<AutocutSkipView>,
}

/// The planned cut of one clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutocutClipView {
    pub clip: ClipId,
    /// Kept fragments as `[in, out)` take frames.
    pub kept: Vec<[Frame; 2]>,
    pub removed_frames: Frame,
    /// Number of silences removed.
    pub silences: usize,
}

/// A clip autocut left alone, and why ("changed while analyzing", "entirely
/// silent at −35 dB", "take has no track A2").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutocutSkipView {
    pub clip: ClipId,
    pub reason: String,
}

/// A finished export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportDone {
    /// The real (absolute) path written.
    pub path: String,
    pub bytes: u64,
    pub frames: u64,
}

/// A saved frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrameSavedView {
    pub path: String,
}

/// The reply to `record stop`/`toggle` (and `--wait`) once the take is finalized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TakeFinalizedView {
    pub take: TakeId,
    pub status: TakeStatus,
    pub duration_frames: Frame,
    pub placed: bool,
    /// The clips placed on the timeline.
    pub clips: Vec<ClipId>,
}

/// The reply to `record start` once capture runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordStartedView {
    pub take: TakeId,
    pub mode: RecordMode,
    /// Project-relative files being written.
    pub files: Vec<String>,
}
