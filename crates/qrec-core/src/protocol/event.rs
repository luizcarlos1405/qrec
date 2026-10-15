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
    /// Ends an export or a frame save: a hand-picked `out` that the job
    /// may not replace (`plan.replace` is false) appeared while it ran, so
    /// the result was kept at `kept` (A§8.8). `bytes` is the size of the
    /// file now at `out`.
    OutputAppeared {
        job: JobId,
        out: String,
        bytes: u64,
        kept: String,
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
    pub info: Result<MediaInfo, ProbeError>,
}

/// Why a `ProbeJob` has no `MediaInfo` (A§4.7, A§5.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeError {
    pub kind: ProbeFailure,
    pub message: String,
}

/// Whose fault a failed probe is: the file's (a fact about the take) or
/// the probe's own (nothing is known about the file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProbeFailure {
    /// Missing, empty or unreadable media: ffprobe ran and couldn't use it.
    Media,
    /// ffprobe isn't installed (or can't be run at all).
    MissingTool,
    /// ffprobe didn't finish (timed out, cancelled) or the prober itself
    /// failed (no log file, no worker thread).
    Tool,
}

impl ProbeError {
    pub fn media(message: impl Into<String>) -> Self {
        Self {
            kind: ProbeFailure::Media,
            message: message.into(),
        }
    }

    pub fn missing_tool(message: impl Into<String>) -> Self {
        Self {
            kind: ProbeFailure::MissingTool,
            message: message.into(),
        }
    }

    pub fn tool(message: impl Into<String>) -> Self {
        Self {
            kind: ProbeFailure::Tool,
            message: message.into(),
        }
    }

    /// The probe says nothing about the file: it must not change the
    /// take's facts.
    pub fn is_tool_failure(&self) -> bool {
        self.kind != ProbeFailure::Media
    }
}

/// A bare message is a media failure (what the file says).
impl From<String> for ProbeError {
    fn from(message: String) -> Self {
        Self::media(message)
    }
}

impl From<&str> for ProbeError {
    fn from(message: &str) -> Self {
        Self::media(message)
    }
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
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
