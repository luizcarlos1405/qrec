//! The export panel (G§4.8, S15a–S17): setup captions, the running
//! readout, done and failed. Owner: WP7. Stub until WP7 lands: the
//! signatures are final, the bodies return defaults.
//!
//! The running numbers come from `on_event::output::export_readout` (WP0a).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::protocol::command::{ExportFormat, VideoCodec};
use crate::state::{Capabilities, Job, State};

/// The setup step's caption for a format: "H.264 + AAC 192 kbps · VA-API
/// on renderD129", "VP9 + Opus 128 kbps · software encode", …
pub fn caption(format: ExportFormat, codec: Option<VideoCodec>, caps: &Capabilities) -> String {
    let _ = (format, codec, caps);
    String::new()
}

/// Why the AV1 switch is off: "This ffmpeg has no libsvtav1"; `None` when
/// it can be used.
pub fn av1_unavailable(caps: &Capabilities) -> Option<String> {
    let _ = caps;
    None
}

/// The auto output name: `naming::export_file_name(name, local now)`.
pub fn output_preview(
    state: &State,
    format: ExportFormat,
    now: DateTime<Utc>,
    utc_offset_secs: i32,
) -> String {
    let _ = (state, format, now, utc_offset_secs);
    String::new()
}

/// "1920×1080 · 30 fps · 00:04:06:11 · 10 clips".
pub fn summary(state: &State) -> String {
    let _ = state;
    String::new()
}

/// The running panel (S15b).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Running {
    /// "42%".
    pub percent: String,
    pub fraction: u8,
    /// "Rendering clip 6/10" or "Stitching".
    pub label: String,
    /// "3,449 / 7,391 frames".
    pub frames: String,
    /// "Elapsed 00:00:38 · About 00:00:52 left".
    pub times: String,
    /// "Exporting the timeline as it was at 15:30:12. You can keep editing."
    pub note: String,
}

/// The running panel of `job` at `now` (local times at `utc_offset_secs`).
pub fn running(job: &Job, now: DateTime<Utc>, utc_offset_secs: i32) -> Option<Running> {
    let _ = (job, now, utc_offset_secs);
    None
}

/// The done panel (S16).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Done {
    /// The output as people read it.
    pub path: String,
    /// "88.4 MB · 1920×1080 · 30 fps · 00:04:06:11 · took 00:01:31".
    pub details: String,
}

/// The done panel for an export written to `path` (`bytes`, after
/// `took_secs`).
pub fn done(state: &State, path: &str, bytes: u64, took_secs: u64) -> Done {
    let _ = (state, path, bytes, took_secs);
    Done::default()
}

/// The failed panel (S17).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failed {
    /// "ffmpeg stopped while rendering clip 3/10 (take 3, c1b2). Nothing
    /// was written to exports/."
    pub lead: String,
    /// From a pattern table over the stderr tail ("The disk is full. Free
    /// some space and try again."); none when nothing matches.
    pub hint: Option<String>,
    /// "Failed at 37%".
    pub at: String,
    /// At most 30 lines (`details.stderrTail`).
    pub stderr_tail: Vec<String>,
    /// `details.log`.
    pub log: Option<String>,
}

/// The failed panel from the last progress seen (`percent`, `label`) and
/// the error's details.
pub fn failed(
    state: &State,
    percent: u8,
    label: &str,
    stderr_tail: &[String],
    log: Option<&str>,
) -> Failed {
    let _ = (state, percent, label, stderr_tail, log);
    Failed::default()
}
