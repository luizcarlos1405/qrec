//! Events: results of effects and outside changes, fed to `on_event` (A§4.7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{JobId, ReqId, TakeId, TimerId};
use crate::model::{Fps, Project, StreamKind};
use crate::protocol::effect::ProbePurpose;
use crate::state::{Capabilities, SilenceKey};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Event {
    Capabilities(Capabilities),
    CaptureStarted {
        take: TakeId,
        t0_ns: u64,
        files: Vec<String>,
    },
    CaptureFailed {
        take: Option<TakeId>,
        source: Option<String>,
        message: String,
        /// The GStreamer element that isn't installed, when that is why:
        /// the reply is then `missing_dependency` (exit 5), not
        /// `capture_failed`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        missing: Option<String>,
    },
    StreamLost {
        take: TakeId,
        stream: String,
        message: String,
    },
    /// ~4 Hz, from the pipeline clock.
    RecordClock {
        take: TakeId,
        elapsed_ns: u64,
    },
    CaptureStopped {
        take: TakeId,
        streams: Vec<RecordedStream>,
        timed_out: bool,
    },
    EngineWedged {
        message: String,
    },
    TakeProbed {
        take: TakeId,
        purpose: ProbePurpose,
        results: Vec<ProbeResult>,
    },
    TimerFired {
        timer: TimerId,
    },
    SilenceDetected {
        job: JobId,
        key: SilenceKey,
        silences: Vec<Interval>,
    },
    SilenceFailed {
        job: JobId,
        key: SilenceKey,
        message: String,
    },
    ExportProgress {
        job: JobId,
        segment: usize,
        frames_done: u64,
    },
    ExportStitchProgress {
        job: JobId,
        out_time_ns: u64,
    },
    ExportFinished {
        job: JobId,
        path: String,
        bytes: u64,
    },
    ExportFailed {
        job: JobId,
        message: String,
        stderr_tail: Vec<String>,
        log: String,
    },
    ExportCancelled {
        job: JobId,
    },
    FrameSaved {
        job: JobId,
        path: String,
    },
    FrameFailed {
        job: JobId,
        message: String,
        log: String,
    },
    TakeDirsDeleted {
        job: JobId,
        deleted: Vec<String>,
        errors: Vec<String>,
    },
    /// Parsed and migrated by the shell; `Err` = the file is invalid (A§7.7).
    ProjectChangedOnDisk {
        project: Result<Box<Project>, String>,
    },
    /// An IPC client disconnected.
    RequesterGone {
        req: ReqId,
    },
}

/// One file a record bin wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordedStream {
    pub id: String,
    pub file: String,
    /// First passed PTS − T0.
    pub start_offset_ns: i64,
    /// The bin reached EOS.
    pub eos: bool,
}

/// The outcome of one `ProbeJob`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeResult {
    pub id: String,
    pub file: String,
    pub info: Result<MediaInfo, String>,
}

/// What `ffprobe` says about a stream (`parse::ffprobe`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaInfo {
    pub kind: StreamKind,
    /// Video packet count (`-count_packets`).
    pub frames: Option<u64>,
    pub fps: Option<Fps>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub duration_ns: Option<i64>,
    pub start_time_ns: i64,
    pub color_range: Option<String>,
    pub color_space: Option<String>,
}

/// A half-open time range `[start_ns, end_ns)` in file time.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Interval {
    pub start_ns: i64,
    pub end_ns: i64,
}
