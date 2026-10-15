//! Plans: pure descriptions of media work that the shell executes verbatim
//! (A§9.3–A§9.6, A§8.11). Built by `export_plan`, `frame_plan`,
//! `capture_plan` and `ges_plan`; snapshot-tested with `insta`.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ctx::{CameraMode, FileId};
use crate::ids::{ClipId, Frame, LayerId, TakeId, TrackId};
use crate::layout::Placement;
use crate::model::{Canvas, Fps, Scene, VideoSource};

// --------------------------------------------------------------- export

/// An export: render segments (cached by key), then stitch them (A§9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportPlan {
    /// The final output path: absolute when hand-picked (`--out`), else a
    /// project-relative auto name (`exports/…`, `frames/…`) that the runner
    /// bumps with `-2`, `-3`, … instead of overwriting (A§8.8).
    pub out: String,
    /// See [`FramePlan::replace`].
    #[serde(default)]
    pub replace: Option<FileId>,
    /// Extension of the temp output (`.<stem>.qrec-part-<pid>-<job>.<ext>`).
    pub tmp_ext: String,
    pub segments: Vec<SegmentPlan>,
    pub stitch: StitchPlan,
    pub total_frames: u64,
    /// Every file the job reads.
    pub files: BTreeSet<String>,
}

/// One render unit: a maximal run of continuous clips of one take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SegmentPlan {
    pub clips: Vec<ClipId>,
    pub take: TakeId,
    #[serde(rename = "in")]
    pub in_: Frame,
    pub out: Frame,
    /// Video packets the segment must have.
    pub frames: u64,
    /// Audio samples (48 kHz) the segment must have.
    pub samples: u64,
    /// Cache key (A§9.3).
    pub key: String,
    /// `.qrec/cache/segments/<key>.<ext>`.
    pub path: String,
    /// The ffmpeg argv; the output path is the last element.
    pub argv: Vec<String>,
}

/// The concat step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StitchPlan {
    /// The concat demuxer list, in the segments dir.
    pub list_path: String,
    pub list_content: String,
    /// The ffmpeg argv; the output path is the last element.
    pub argv: Vec<String>,
}

// ---------------------------------------------------------------- frame

/// A frame save: one ffmpeg run writing a PNG (A§9.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FramePlan {
    /// The final output path: absolute when hand-picked (`--out`), else a
    /// project-relative auto name (`exports/…`, `frames/…`) that the runner
    /// bumps with `-2`, `-3`, … instead of overwriting (A§8.8).
    pub out: String,
    /// The file at `out` the runner may replace, by identity: set only for
    /// a hand-picked path that existed when the person confirmed the
    /// command (A§5.9), and only that very file is replaced. Otherwise
    /// nothing at `out` is ever replaced: an auto name moves on to `-2`,
    /// `-3`, …, and a hand-picked path that appeared (or now holds another
    /// file) while the job ran ends it with `OutputAppeared`, the result
    /// kept (A§8.8).
    #[serde(default)]
    pub replace: Option<FileId>,
    /// Extension of the temp output (`png`).
    pub tmp_ext: String,
    /// The ffmpeg argv; the output path is the last element.
    pub argv: Vec<String>,
    /// Every file the job reads.
    pub files: BTreeSet<String>,
}

// -------------------------------------------------------------- capture

/// What to record for one take (A§9.5). Paths are project-relative; the
/// engine joins them with the project dir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapturePlan {
    pub take: TakeId,
    /// `takes/<take>`.
    pub dir: String,
    /// `takes/<take>/recorder.log`.
    pub log: String,
    pub fps: Fps,
    pub health_check_ms: u32,
    pub first_buffer_ms: u32,
    pub eos_timeout_ms: u32,
    pub video: Vec<VideoRecord>,
    pub audio: Vec<AudioRecord>,
}

/// One video record bin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VideoRecord {
    pub id: LayerId,
    pub source: VideoSource,
    pub file: String,
    pub encoder: VideoEncoder,
    pub key_int_max: u32,
    /// Cameras: the mode to negotiate.
    pub camera_mode: Option<CameraMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum VideoEncoder {
    /// The `va*h264enc` factory bound to the effective render node, CQP.
    Va { factory: String, qp: u32 },
    X264 {
        pass: String,
        quantizer: u32,
        speed_preset: String,
        tune: String,
    },
}

/// One audio record bin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioRecord {
    pub id: TrackId,
    /// PulseAudio/PipeWire source name.
    pub device: String,
    pub file: String,
    pub sample_rate: u32,
}

/// The live pipeline the engine should run (A§8.11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveScene {
    pub canvas: Canvas,
    pub scene: Scene,
    /// Build the preview branch (GUI only).
    pub preview: bool,
}

// ------------------------------------------------------------------ GES

/// The timeline as a GES description for preview playback (A§9.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GesTimeline {
    pub canvas: Canvas,
    pub duration_ns: u64,
    pub video: Vec<GesVideoClip>,
    pub audio: Vec<GesAudioClip>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GesVideoClip {
    /// The take and the layer the clip shows (the player drops or labels
    /// clips by them; the renderer only needs the file).
    pub take: TakeId,
    pub layer: LayerId,
    pub file: String,
    pub layer_priority: u32,
    pub start_ns: u64,
    pub inpoint_ns: u64,
    pub duration_ns: u64,
    pub placement: Placement,
    pub alpha: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GesAudioClip {
    /// The take and the audio track the clip plays (the player leaves out
    /// muted or unsoloed tracks by these, GUI D-08).
    pub take: TakeId,
    pub track: TrackId,
    pub file: String,
    pub start_ns: u64,
    pub inpoint_ns: u64,
    pub duration_ns: u64,
    /// Linear gain.
    pub volume: f64,
    pub fade_in_samples: u32,
    pub fade_out_samples: u32,
}
