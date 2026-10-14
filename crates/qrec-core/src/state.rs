//! Runtime state (A§3): the project plus UI state, history and runtime data.
//!
//! `State` is what `apply`/`on_event` take and return. The shell persists
//! `project` (`qrec.json`), `ui` (`.qrec/ui.json`) and `history`
//! (`.qrec/history.*`) as the effects say; `runtime` lives in memory only.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hash::sha256_hex;
use crate::history::HistoryEntry;
use crate::ids::{ClipId, Frame, JobId, LayerId, ReqId, TakeId, TimerId, TrackId};
use crate::model::{Clip, Project};
use crate::protocol::event::Interval;

/// Everything `apply` and `on_event` work on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct State {
    pub project: Project,
    /// `.qrec/ui.json`; never undoable.
    pub ui: UiState,
    /// The core decides, the shell persists.
    pub history: History,
    /// Never persisted.
    pub runtime: Runtime,
}

impl State {
    /// A state with a fresh runtime (`Idle`, no jobs, capabilities unknown).
    pub fn new(project: Project, history: History, ui: UiState, host: HostKind) -> Self {
        Self {
            project,
            ui,
            history,
            runtime: Runtime::new(host),
        }
    }
}

// ------------------------------------------------------------------- ui

/// View state stored in `.qrec/ui.json`. Not undoable; unknown or missing
/// fields fall back to the defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct UiState {
    /// Timeline order, kept valid by `ui_normalize`.
    pub selection: Vec<ClipId>,
    pub playhead: Frame,
    pub canvas_mode: CanvasMode,
    pub selected_layer: Option<LayerId>,
    /// Default true.
    pub snapping: bool,
    pub bin_open: bool,
    /// Pixels per frame; `None` = fit the whole project.
    pub zoom: Option<f64>,
    pub scroll_px: f64,
    pub solo: Option<TrackId>,
    pub panels: BTreeMap<String, f64>,
}

impl Default for UiState {
    /// A manual impl because `snapping` defaults to true (A§3).
    fn default() -> Self {
        Self {
            selection: Vec::new(),
            playhead: 0,
            canvas_mode: CanvasMode::default(),
            selected_layer: None,
            snapping: true,
            bin_open: false,
            zoom: None,
            scroll_px: 0.0,
            solo: None,
            panels: BTreeMap::new(),
        }
    }
}

/// What the preview canvas shows (SPEC §12).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CanvasMode {
    /// Live sources composited with the project scene.
    #[default]
    Live,
    /// Timeline playback at the playhead.
    Program,
}

// -------------------------------------------------------------- history

/// The undo history the core keeps in memory (A§7). The shell stores it in
/// `.qrec/history.jsonl`, `history.base.json` and `history.head.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct History {
    /// Oldest first. Use `Arc::make_mut` (or [`History::append`]) to change it.
    #[serde(with = "arc_serde")]
    pub entries: Arc<Vec<HistoryEntry>>,
    /// Seq of the last applied entry, or `base_seq`.
    pub cursor: u64,
    /// Seq of the base snapshot (0 for a new project).
    pub base_seq: u64,
    /// Seqs are never reused.
    pub next_seq: u64,
    /// Derived: every clip id in `entries` and in the base snapshot. `IdGen`
    /// never hands these out again, so undo can't resurrect a clip whose id
    /// was reused.
    #[serde(with = "arc_serde")]
    pub seen_clip_ids: Arc<BTreeSet<ClipId>>,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// An empty history for a new project: cursor and base at 0, first seq 1.
    pub fn new() -> Self {
        Self {
            entries: Arc::default(),
            cursor: 0,
            base_seq: 0,
            next_seq: 1,
            seen_clip_ids: Arc::default(),
        }
    }

    /// Appends `entry` like `HistoryOp::Append`: drops the redo tail (entries
    /// after the cursor), moves the cursor to the entry and bumps `next_seq`.
    /// Also remembers the clip ids the entry mentions.
    pub fn append(&mut self, entry: HistoryEntry) {
        let mut ids = BTreeSet::new();
        entry_clip_ids(&entry, &mut ids);
        self.remember_clip_ids(ids.iter());
        let cursor = self.cursor;
        let entries = Arc::make_mut(&mut self.entries);
        entries.retain(|e| e.seq <= cursor);
        self.cursor = entry.seq;
        self.next_seq = self.next_seq.max(entry.seq + 1);
        entries.push(entry);
    }

    /// Adds clip ids to `seen_clip_ids` (copies the set only when one is new).
    pub fn remember_clip_ids<'a>(&mut self, ids: impl IntoIterator<Item = &'a ClipId>) {
        let new: Vec<&ClipId> = ids
            .into_iter()
            .filter(|id| !self.seen_clip_ids.contains(*id))
            .collect();
        if !new.is_empty() {
            let seen = Arc::make_mut(&mut self.seen_clip_ids);
            seen.extend(new.into_iter().cloned());
        }
    }

    /// Recomputes `seen_clip_ids` from the base snapshot and every entry's
    /// patches (after loading or rewriting the history).
    pub fn rebuild_seen_clip_ids(&mut self, base: &Project) {
        let mut seen: BTreeSet<ClipId> = base.timeline.clips.iter().map(|c| c.id.clone()).collect();
        for entry in self.entries.iter() {
            entry_clip_ids(entry, &mut seen);
        }
        self.seen_clip_ids = Arc::new(seen);
    }
}

/// Collects every clip id an entry's patches mention: strings at
/// `/timeline/clips/<n>/id`, wherever they sit inside an operation's value.
pub fn entry_clip_ids(entry: &HistoryEntry, out: &mut BTreeSet<ClipId>) {
    for op in entry.forward.0.iter().chain(entry.inverse.0.iter()) {
        let value = match op {
            json_patch::PatchOperation::Add(o) => &o.value,
            json_patch::PatchOperation::Replace(o) => &o.value,
            json_patch::PatchOperation::Test(o) => &o.value,
            _ => continue,
        };
        let mut path: Vec<String> = op
            .path()
            .tokens()
            .map(|t| t.decoded().into_owned())
            .collect();
        walk_clip_ids(&mut path, value, out);
    }
}

fn walk_clip_ids(path: &mut Vec<String>, value: &Value, out: &mut BTreeSet<ClipId>) {
    match value {
        Value::String(s) => {
            if matches!(path.as_slice(), [a, b, _, d] if a == "timeline" && b == "clips" && d == "id")
            {
                out.insert(ClipId::from_raw(s.clone()));
            }
        }
        Value::Object(map) => {
            for (key, child) in map {
                path.push(key.clone());
                walk_clip_ids(path, child, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                path.push(i.to_string());
                walk_clip_ids(path, child, out);
                path.pop();
            }
        }
        _ => {}
    }
}

/// Serializes an `Arc<T>` as its contents (serde's `rc` feature is off).
mod arc_serde {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(value: &Arc<T>, s: S) -> Result<S::Ok, S::Error> {
        T::serialize(value, s)
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Arc<T>, D::Error> {
        T::deserialize(d).map(Arc::new)
    }
}

// -------------------------------------------------------------- runtime

/// In-memory state of the running host. Never persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Runtime {
    pub host: HostKind,
    pub record: RecordPhase,
    pub jobs: BTreeMap<JobId, Job>,
    /// Silence analysis results, in file time.
    pub silence: BTreeMap<SilenceKey, Vec<Interval>>,
    pub capabilities: Capabilities,
    /// Allocates `JobId`s and `TimerId`s (one counter for both).
    pub next_id: u64,
    pub playing: bool,
    /// `qrec.json` on disk failed to load (A§7.7).
    pub disk_invalid: Option<String>,
    /// The capture engine wedged (A§5.3 `EngineWedged`): new takes can't
    /// start until Qrec restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_stuck: Option<String>,
}

impl Runtime {
    /// Idle, no jobs, capabilities not detected yet, ids starting at 1.
    pub fn new(host: HostKind) -> Self {
        Self {
            host,
            record: RecordPhase::Idle,
            jobs: BTreeMap::new(),
            silence: BTreeMap::new(),
            capabilities: Capabilities::default(),
            next_id: 1,
            playing: false,
            disk_invalid: None,
            engine_stuck: None,
        }
    }

    pub fn alloc_job_id(&mut self) -> JobId {
        let id = JobId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn alloc_timer_id(&mut self) -> TimerId {
        let id = TimerId(self.next_id);
        self.next_id += 1;
        id
    }
}

/// Which kind of process holds the project lock (A§8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HostKind {
    Gui,
    Headless,
    Cli,
}

impl HostKind {
    /// `gui`, `headless` or `cli`.
    pub fn as_str(self) -> &'static str {
        match self {
            HostKind::Gui => "gui",
            HostKind::Headless => "headless",
            HostKind::Cli => "cli",
        }
    }
}

// ------------------------------------------------------------ recording

/// The recording state machine (A§5.3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "phase",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RecordPhase {
    #[default]
    Idle,
    Countdown {
        remaining: u32,
        mode: RecordMode,
        timer: TimerId,
        start_req: Option<ReqId>,
        wait: bool,
    },
    Starting {
        take: TakeId,
        mode: RecordMode,
        start_req: Option<ReqId>,
        wait: bool,
    },
    Recording {
        take: TakeId,
        mode: RecordMode,
        elapsed_ns: u64,
        waiters: Vec<ReqId>,
    },
    Finalizing {
        take: TakeId,
        mode: RecordMode,
        cancelled: bool,
        stop_reqs: Vec<ReqId>,
        waiters: Vec<ReqId>,
        discard: Option<DiscardRequest>,
    },
}

impl RecordPhase {
    pub fn is_idle(&self) -> bool {
        matches!(self, RecordPhase::Idle)
    }

    /// The take being started, recorded or finalized.
    pub fn take(&self) -> Option<&TakeId> {
        match self {
            RecordPhase::Starting { take, .. }
            | RecordPhase::Recording { take, .. }
            | RecordPhase::Finalizing { take, .. } => Some(take),
            RecordPhase::Idle | RecordPhase::Countdown { .. } => None,
        }
    }
}

/// A `TakeDiscardLast` received while finalizing (applied after placement, A§5.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscardRequest {
    pub req: Option<ReqId>,
}

/// Where a new take goes on the timeline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RecordMode {
    /// A clip `[0, durationFrames)` at the end.
    #[default]
    Append,
    /// Replaces this contiguous run of clips (record over).
    Over { clips: Vec<ClipRef> },
}

/// A clip as it was when a command looked at it; later steps act only if
/// the clip still matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipRef {
    pub id: ClipId,
    pub take: TakeId,
    #[serde(rename = "in")]
    pub in_: Frame,
    pub out: Frame,
}

impl ClipRef {
    pub fn of(clip: &Clip) -> Self {
        Self {
            id: clip.id.clone(),
            take: clip.take.clone(),
            in_: clip.in_,
            out: clip.out,
        }
    }

    /// Same id, take and range.
    pub fn matches(&self, clip: &Clip) -> bool {
        self.id == clip.id && self.take == clip.take && self.in_ == clip.in_ && self.out == clip.out
    }
}

// --------------------------------------------------------- capabilities

/// What the machine can do, detected by the shell (`Event::Capabilities`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capabilities {
    /// `None` until detected; `Some` before the first dispatch.
    pub ffmpeg: Option<FfmpegCaps>,
    pub vaapi: Option<VaapiCaps>,
    pub cores: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FfmpegCaps {
    pub version: String,
    /// The resolved binary path (part of export cache keys).
    pub identity: String,
    pub encoders: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VaapiCaps {
    /// The effective render node (A§2.1).
    pub device: String,
    /// PCI vendor id (`0x8086` Intel, `0x1002` AMD).
    pub vendor: u16,
    /// The exact VA export chain encoded 2 frames (A§9.4).
    pub ffmpeg_h264: bool,
    /// The `va*h264enc` factory whose `device-path` is this node.
    pub gst_h264_factory: Option<String>,
}

// ----------------------------------------------------------------- jobs

/// A running background job (A§5.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    /// Who waits for the reply; cleared by `RequesterGone`.
    pub req: Option<ReqId>,
    pub started: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum JobKind {
    Autocut {
        /// The original command, for the history entry.
        cmd: Value,
        targets: Vec<AutocutTarget>,
        params: AutocutParams,
        dry_run: bool,
        /// Silence keys still being analyzed.
        pending: BTreeSet<SilenceKey>,
        /// Analysis finished while recording; apply after placement.
        ready: bool,
    },
    Export {
        out: String,
        /// Files the job reads (purge is `Busy` while they're in use).
        files: BTreeSet<String>,
        total_frames: u64,
        seg_frames: Vec<u64>,
        seg_done: Vec<u64>,
        stitch_total_ns: u64,
        stitch_done_ns: u64,
        stitching: bool,
    },
    FrameSave {
        out: String,
        files: BTreeSet<String>,
    },
    Purge {
        takes: Vec<TakeId>,
    },
}

/// One clip × track an autocut job analyzes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutocutTarget {
    pub clip: ClipRef,
    pub track: TrackId,
    pub key: SilenceKey,
}

/// Autocut parameters snapshotted when the command runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutocutParams {
    pub threshold_db: f64,
    pub min_silence_ms: u32,
    pub padding_ms: u32,
    pub min_keep_ms: u32,
}

/// Identifies one silence analysis: the file's identity plus the parameters
/// that change its result.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SilenceKey(pub String);

impl SilenceKey {
    /// `sha256_hex("{file}\n{size}\n{mtime_ns}\n{threshold_db:.2}\n{min_silence_ms}")` (A§3).
    pub fn compute(
        file: &str,
        size: u64,
        mtime_ns: i64,
        threshold_db: f64,
        min_silence_ms: u32,
    ) -> Self {
        let text = format!("{file}\n{size}\n{mtime_ns}\n{threshold_db:.2}\n{min_silence_ms}");
        Self(sha256_hex(text.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_defaults() {
        let ui: UiState = serde_json::from_str("{}").unwrap();
        assert!(ui.snapping);
        assert_eq!(ui, UiState::default());
        assert_eq!(
            serde_json::to_value(CanvasMode::Program).unwrap(),
            "program"
        );
    }

    #[test]
    fn record_phase_json() {
        let phase = RecordPhase::Recording {
            take: TakeId::numbered(3),
            mode: RecordMode::Append,
            elapsed_ns: 5,
            waiters: vec![],
        };
        assert_eq!(
            serde_json::to_string(&phase).unwrap(),
            r#"{"phase":"recording","take":"take-0003","mode":"append","elapsedNs":5,"waiters":[]}"#
        );
        assert_eq!(
            serde_json::to_string(&RecordPhase::Idle).unwrap(),
            r#"{"phase":"idle"}"#
        );
    }

    #[test]
    fn silence_key_is_stable() {
        let a = SilenceKey::compute("takes/take-0001/A1-mic.mka", 10, 20, -35.0, 400);
        let b = SilenceKey::compute("takes/take-0001/A1-mic.mka", 10, 20, -35.004, 400);
        assert_eq!(a, b, "threshold is keyed at 2 decimals");
        assert_eq!(a.0.len(), 64);
        assert_ne!(
            a,
            SilenceKey::compute("takes/take-0001/A1-mic.mka", 11, 20, -35.0, 400)
        );
    }

    #[test]
    fn ids_come_from_one_counter() {
        let mut rt = Runtime::new(HostKind::Cli);
        assert_eq!(rt.alloc_job_id(), JobId(1));
        assert_eq!(rt.alloc_timer_id(), TimerId(2));
        assert_eq!(rt.next_id, 3);
    }
}
