//! Effects: I/O the core asks the shell to perform (A§4.6). The shell runs
//! them in the order of A§7.6 and answers with `Event`s.

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::history::HistoryEntry;
use crate::ids::{JobId, ReqId, TakeId, TimerId};
use crate::model::{Project, StreamKind};
use crate::plans::{CapturePlan, ExportPlan, FramePlan, LiveScene};
use crate::protocol::progress::Progress;
use crate::protocol::reply::CmdResult;
use crate::protocol::toast::Toast;
use crate::state::SilenceKey;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Effect {
    // -- persistence: committed by the shell in the order of A§7.6
    /// Write `State::project` to `qrec.json` (atomically).
    WriteProject,
    /// Write `State::ui` to `.qrec/ui.json`; coalesced by the shell (≤ 1 write / 500 ms, no fsync).
    WriteUi,
    History(HistoryOp),
    /// `mkdir`, NOT `-p`: `EEXIST` aborts the take.
    CreateTakeDir {
        path: String,
    },
    /// Purge only, after the commit is durable (A§5.6).
    DeleteTakeDirs {
        job: JobId,
        takes: Vec<TakeId>,
    },

    // -- capture (A§8.11)
    /// The desired live pipeline (GUI Live mode, recording); `None` stops it.
    SetLive(Option<LiveScene>),
    /// → `CaptureStarted` | `CaptureFailed`.
    StartCapture {
        plan: CapturePlan,
    },
    /// → `CaptureStopped`.
    StopCapture {
        take: TakeId,
        cancel: bool,
    },

    // -- media jobs: argv built in the core (A§9.2)
    /// → `TakeProbed`.
    Probe {
        take: TakeId,
        purpose: ProbePurpose,
        jobs: Vec<ProbeJob>,
    },
    /// → `SilenceDetected` | `SilenceFailed`.
    DetectSilence {
        job: JobId,
        key: SilenceKey,
        argv: Vec<String>,
    },
    /// → `ExportProgress`… then `ExportFinished` | `ExportFailed` |
    /// `ExportCancelled` | `OutputAppeared`.
    RenderExport {
        job: JobId,
        plan: ExportPlan,
    },
    /// → `FrameSaved` | `FrameFailed` | `OutputAppeared`.
    SaveFrame {
        job: JobId,
        plan: FramePlan,
    },
    CancelJob {
        job: JobId,
    },

    // -- time
    /// → `TimerFired`.
    StartTimer {
        timer: TimerId,
        after_ms: u64,
    },
    CancelTimer {
        timer: TimerId,
    },

    // -- people and requesters
    Notify(Toast),
    Progress {
        req: ReqId,
        progress: Progress,
    },
    /// The late reply of an async command.
    Reply {
        req: ReqId,
        reply: Result<CmdResult, CoreError>,
    },
    Log {
        level: LogLevel,
        message: String,
    },
}

/// Changes to the persisted history (`.qrec/history.*`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum HistoryOp {
    /// Drops the redo tail first; cursor = entry.seq.
    Append(HistoryEntry),
    /// Undo / redo / restore.
    SetCursor(u64),
    /// Purge and compaction: replaces the whole history.
    Rewrite {
        entries: Vec<HistoryEntry>,
        base: Box<Project>,
        base_seq: u64,
        cursor: u64,
        next_seq: u64,
    },
}

/// Why streams are probed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProbePurpose {
    /// A take just stopped.
    Finalize,
    /// A take found still `recording` at startup.
    Recover,
}

/// One `ffprobe` run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeJob {
    /// The stream id (layer or track id).
    pub id: String,
    /// Project-relative.
    pub file: String,
    pub kind: StreamKind,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}
