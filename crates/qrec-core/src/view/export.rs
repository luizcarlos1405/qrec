//! The export panel (G§4.8, S15a–S17): setup captions, the running
//! readout, done and failed. Owner: WP7. Stub until WP7 lands: the
//! signatures are final, the bodies return defaults, except [`caption`]
//! (WP0a: it names the encoder the export will use, so it shares the
//! export's rule, `export_plan::export_vaapi`).
//!
//! The running numbers come from `on_event::output::export_readout` (WP0a).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::UserConfig;
use crate::export_plan::export_vaapi;
use crate::protocol::command::{ExportFormat, VideoCodec};
use crate::state::{Capabilities, Job, State};

/// The setup step's caption for a format (G§4.8). MP4: "H.264 + AAC 192
/// kbps · VA-API on renderD129" when the export would encode on that node
/// (`export_plan::export_vaapi`: the effective encoder isn't x264, the
/// node is the pinned one, its chain works), else "H.264 + AAC 192 kbps ·
/// software encode (x264)". WebM: "VP9 + Opus 128 kbps · software
/// encode", with AV1 "AV1 + Opus 128 kbps · software encode (libsvtav1)".
/// `config` is the user config the GUI read at startup (the one it gives
/// `Ctx::preview`).
pub fn caption(
    format: ExportFormat,
    codec: Option<VideoCodec>,
    state: &State,
    config: &UserConfig,
) -> String {
    match (format, codec) {
        (ExportFormat::Webm, Some(VideoCodec::Av1)) => {
            "AV1 + Opus 128 kbps \u{b7} software encode (libsvtav1)".to_owned()
        }
        (ExportFormat::Webm, _) => "VP9 + Opus 128 kbps \u{b7} software encode".to_owned(),
        (ExportFormat::Mp4, _) => {
            match export_vaapi(&state.project, config, &state.runtime.capabilities) {
                Some(va) => {
                    let node = va.device.rsplit('/').next().unwrap_or(&va.device);
                    format!("H.264 + AAC 192 kbps \u{b7} VA-API on {node}")
                }
                None => "H.264 + AAC 192 kbps \u{b7} software encode (x264)".to_owned(),
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EncoderPref;
    use crate::state::VaapiCaps;
    use crate::testkit;

    const X264: &str = "H.264 + AAC 192 kbps \u{b7} software encode (x264)";
    const VA: &str = "H.264 + AAC 192 kbps \u{b7} VA-API on renderD129";

    /// A machine whose VA chain works on renderD129.
    fn va_state() -> State {
        let mut state = testkit::design_state();
        state.runtime.capabilities.vaapi = Some(VaapiCaps {
            device: "/dev/dri/renderD129".into(),
            vendor: 0x8086,
            ffmpeg_h264: true,
            gst_h264_factory: None,
        });
        state
    }

    /// The caption names the encoder the export will use: VA-API only where
    /// `export_vaapi` (the export's own rule) picks it, so x264 preferred by
    /// the project or the config, or a pin on another node, read x264
    /// although the machine's VA chain works.
    #[test]
    fn the_mp4_caption_names_the_encoder_the_export_uses() {
        let config = UserConfig::default();
        let mp4 =
            |state: &State, config: &UserConfig| caption(ExportFormat::Mp4, None, state, config);
        let mut state = va_state();
        assert_eq!(mp4(&state, &config), VA);
        assert_eq!(
            caption(ExportFormat::Mp4, Some(VideoCodec::H264), &state, &config),
            VA
        );

        // The project prefers x264 (`qrec project set --encoder x264`).
        state.project.recording.encoder = EncoderPref::X264;
        assert_eq!(mp4(&state, &config), X264);
        state.project.recording.encoder = EncoderPref::Auto;

        // config.toml prefers x264.
        let mut prefers_x264 = config.clone();
        prefers_x264.encoder.preference = EncoderPref::X264;
        assert_eq!(mp4(&state, &prefers_x264), X264);

        // A pin on a node other than the verified one.
        let mut pinned = config.clone();
        pinned.encoder.vaapi_device = Some("/dev/dri/renderD128".into());
        assert_eq!(mp4(&state, &pinned), X264);
        state.project.recording.vaapi_device = Some("/dev/dri/renderD128".into());
        assert_eq!(mp4(&state, &config), X264);
        state.project.recording.vaapi_device = Some("/dev/dri/renderD129".into());
        assert_eq!(mp4(&state, &pinned), VA, "the project's pin wins");

        // No VA, or a chain that doesn't encode.
        assert_eq!(mp4(&testkit::design_state(), &config), X264);
        let mut broken = va_state();
        broken
            .runtime
            .capabilities
            .vaapi
            .as_mut()
            .unwrap()
            .ffmpeg_h264 = false;
        assert_eq!(mp4(&broken, &config), X264);
    }

    #[test]
    fn webm_captions() {
        let state = va_state();
        let config = UserConfig::default();
        let vp9 = "VP9 + Opus 128 kbps \u{b7} software encode";
        assert_eq!(caption(ExportFormat::Webm, None, &state, &config), vp9);
        assert_eq!(
            caption(ExportFormat::Webm, Some(VideoCodec::Vp9), &state, &config),
            vp9
        );
        assert_eq!(
            caption(ExportFormat::Webm, Some(VideoCodec::Av1), &state, &config),
            "AV1 + Opus 128 kbps \u{b7} software encode (libsvtav1)"
        );
    }
}
