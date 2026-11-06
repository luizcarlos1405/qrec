//! The capture planner (A§9.5, A§8.11): which record bins to attach for a
//! take. Owner: record.
//!
//! File names: `L1-screen-DP-4.mkv`, `L2-camera-0.mkv`, `A1-mic.mka`,
//! `A2-system.mka` (sanitized to `[A-Za-z0-9._-]`). Visible and hidden
//! layers are both recorded (visibility is a layout choice). Encoder
//! ([`choose_encoder`]): `x264` ⇒ X264; otherwise Va with the effective
//! render node's `va*h264enc`, else its `va*h264lpenc`; else X264 unless
//! GStreamer has only `openh264enc`, then OpenH264, which can't record a
//! source larger than [`OPENH264_MAX_PIXELS`] (the plan refuses it).

use crate::ctx::{CameraMode, Facts};
use crate::error::{CoreError, ErrorCode};
use crate::ids::TakeId;
use crate::model::{AudioTrack, EncoderPref, Fps, Layer, Project, Stream, StreamKind, VideoSource};
use crate::plans::{AudioRecord, CapturePlan, VideoEncoder, VideoRecord};
use crate::state::{Capabilities, GstH264Caps};

/// A record bin that errors within this window aborts the take (SPEC §5.3).
pub const HEALTH_CHECK_MS: u32 = 500;
/// Every filesink must see its first buffer within this time, else `CaptureFailed`.
pub const FIRST_BUFFER_MS: u32 = 3_000;
/// EOS on every filesink within this time, else the take is `broken`.
pub const EOS_TIMEOUT_MS: u32 = 10_000;
/// Constant quantizer of the record encoders (x264 `quantizer`, VA `qpi/qpp/qpb`).
pub const RECORD_QP: u32 = 18;
/// OpenH264's constant QP (`qp-min` = `qp-max`). Measured with OpenH264
/// 2.6 (GStreamer 1.26) on a 2560×1440 screen clip (still, scrolling, cuts)
/// and a noisy 1920×1080 camera clip, pinned to 4.6 GHz E-cores: at 22
/// the screen's text stays as sharp as the source, still and scrolling,
/// where x264's CRF 18 rings and smears (PSNR-Y 43.5 dB, SSIM 0.995 vs
/// 36.1 dB, 0.978), in about 4 Mbit/s. QP 18 gains 0.9 dB on the screen but
/// costs the noisy camera 40 ms of CPU a frame (under 30 fps on one core)
/// and 52 Mbit/s; 22 halves the CPU and cuts the bitrate to 17.6 Mbit/s.
pub const OPENH264_QP: u32 = 22;
/// OpenH264's rate control: a mode that honors `qp-min`/`qp-max` (`off`
/// ignores them and encodes everything at QP 26).
pub const OPENH264_RATE_CONTROL: &str = "quality";
/// OpenH264's bitrate target, bits/s: with the QP pinned it only nudges
/// the rate control (outputs within 3% from 128 kbit/s to 100 Mbit/s), so
/// it sits above anything [`OPENH264_QP`] needs.
pub const OPENH264_BITRATE: u32 = 100_000_000;
/// OpenH264's `complexity`: medium and high cost 12–18% more time for the
/// same quality and 2% more bits.
pub const OPENH264_COMPLEXITY: &str = "low";
/// OpenH264's encoder threads, one slice each: one thread encodes a
/// 2560×1440 screen at 45 fps on that core, too close to 30 for slower
/// ones; four do 76 fps for 13% more CPU a frame and 7% more bits.
pub const OPENH264_THREADS: u32 = 4;
/// The largest frame OpenH264 encodes, in pixels: 36,864 macroblocks (the
/// largest frame of H.264 level 5.2), 4096×2304 in 16:9, whatever the
/// shape (5120×1440 and 7680×1200 encode; checked with OpenH264 2.6). A
/// larger frame fails when the stream starts, with a bare not-negotiated
/// error (OpenH264 logs `ParamValidationExt(), … width * height <=
/// 9437184`), so [`plan_capture`] refuses a take that would record one
/// with OpenH264 ([`openh264_too_large`]).
pub const OPENH264_MAX_PIXELS: u64 = 9_437_184;
/// Every audio track is resampled to this rate.
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;
/// Samples in every FLAC frame (packet) a record bin writes: flacenc's
/// `blocksize`, which the engine sets (A§8.11). Only the last frame of a
/// file that was closed normally can be shorter, so a file cut short by a
/// crash, which has no container duration, is `packets × FLAC_BLOCK_SAMPLES`
/// samples long (A§2.2, `parse::ffprobe`).
pub const FLAC_BLOCK_SAMPLES: u32 = 4608;
/// The log the engine writes next to the take's files.
pub const RECORDER_LOG: &str = "recorder.log";
/// Longest sanitized source part of a file name (output name, device number).
const MAX_PART_BYTES: usize = 64;
/// PCI vendor id of NVIDIA: never used for VA-API unless configured explicitly.
const NVIDIA: u16 = 0x10de;

/// `takes/<take>`.
pub fn take_dir(take: &TakeId) -> String {
    format!("takes/{take}")
}

/// The plan for recording `take` with the project scene. Camera modes come
/// from `facts.cameras`, the encoder preference from the project and
/// `facts.config` (A§2.1). `Invalid` when the scene has nothing to record;
/// `MissingDependency` when OpenH264 would record a layer larger than it
/// encodes (screens by their size in `facts.screens`, which a take's start
/// asks for then: `FactsNeeded::screens`).
pub fn plan_capture(
    project: &Project,
    take: &TakeId,
    caps: &Capabilities,
    facts: &Facts,
) -> Result<CapturePlan, CoreError> {
    let scene = &project.scene;
    if scene.is_empty() {
        return Err(empty_scene());
    }
    let fps = project.canvas.fps;
    let dir = take_dir(take);
    let encoder = choose_encoder(project, caps, facts);
    let key_int_max = key_int_max(fps);
    let video: Vec<VideoRecord> = scene
        .layers
        .iter()
        .map(|layer| VideoRecord {
            id: layer.id.clone(),
            source: layer.source.clone(),
            file: format!("{dir}/{}", video_file_name(layer)),
            encoder: encoder.clone(),
            key_int_max,
            camera_mode: match &layer.source {
                VideoSource::Camera { device, .. } => facts
                    .cameras
                    .iter()
                    .find(|c| &c.device == device)
                    .and_then(|c| choose_camera_mode(&c.modes, fps)),
                VideoSource::Screen { .. } => None,
            },
        })
        .collect();
    if matches!(encoder, VideoEncoder::OpenH264 { .. }) {
        for (layer, rec) in scene.layers.iter().zip(&video) {
            if let Some((width, height)) = frame_size(layer, rec, facts)
                && u64::from(width) * u64::from(height) > OPENH264_MAX_PIXELS
            {
                return Err(openh264_too_large(layer, width, height));
            }
        }
    }
    let audio = scene
        .audio
        .iter()
        .map(|track| AudioRecord {
            id: track.id.clone(),
            device: track.source.clone(),
            file: format!("{dir}/{}", audio_file_name(track)),
            sample_rate: AUDIO_SAMPLE_RATE,
        })
        .collect();
    Ok(CapturePlan {
        take: take.clone(),
        log: format!("{dir}/{RECORDER_LOG}"),
        dir,
        fps,
        health_check_ms: HEALTH_CHECK_MS,
        first_buffer_ms: FIRST_BUFFER_MS,
        eos_timeout_ms: EOS_TIMEOUT_MS,
        video,
        audio,
    })
}

/// The error for recording an empty scene.
pub fn empty_scene() -> CoreError {
    CoreError::invalid("the scene is empty: there is nothing to record").with_hint(
        "add a source first: qrec scene layer add screen --output <name>, or qrec scene audio add mic",
    )
}

/// The size of the frames `rec` records, when known: a screen's from
/// `facts.screens` (the output's current mode), a camera's from the mode
/// the plan picked.
fn frame_size(layer: &Layer, rec: &VideoRecord, facts: &Facts) -> Option<(u32, u32)> {
    match &layer.source {
        VideoSource::Screen { output, .. } => facts
            .screens
            .iter()
            .find(|s| s.output == *output)
            .map(|s| (s.width, s.height)),
        VideoSource::Camera { .. } => rec.camera_mode.as_ref().map(|m| (m.width, m.height)),
    }
}

/// The error for recording `layer`, whose frames are `width`×`height`,
/// with OpenH264, which encodes at most [`OPENH264_MAX_PIXELS`] a frame: a
/// missing dependency, since x264enc would record it.
pub fn openh264_too_large(layer: &Layer, width: u32, height: u32) -> CoreError {
    let source = match &layer.source {
        VideoSource::Screen { output, .. } => output,
        VideoSource::Camera { device, .. } => device,
    };
    CoreError::new(
        ErrorCode::MissingDependency,
        format!(
            "can't record {} ({source}): its frames are {width}×{height}, and OpenH264, which \
             records here, encodes at most {OPENH264_MAX_PIXELS} pixels a frame (4096×2304)",
            layer.id
        ),
    )
    .with_hint(
        "install x264enc (gst-plugins-ugly), which records frames that large; qrec doctor names this system's packages",
    )
}

/// The streams a take starts with: one per record bin, media fields absent
/// until the take is probed, offsets 0 until the engine reports them.
pub fn planned_streams(plan: &CapturePlan) -> Vec<Stream> {
    let empty = |id: String, kind: StreamKind, file: &str| Stream {
        id,
        kind,
        file: file.to_owned(),
        width: None,
        height: None,
        sample_rate: None,
        channels: None,
        start_offset_ns: 0,
        duration_ns: None,
        frames: None,
        fps: None,
    };
    plan.video
        .iter()
        .map(|v| empty(v.id.to_string(), StreamKind::Video, &v.file))
        .chain(
            plan.audio
                .iter()
                .map(|a| empty(a.id.to_string(), StreamKind::Audio, &a.file)),
        )
        .collect()
}

/// Every file the plan writes (videos, then audio), project-relative.
pub fn plan_files(plan: &CapturePlan) -> Vec<String> {
    plan.video
        .iter()
        .map(|v| v.file.clone())
        .chain(plan.audio.iter().map(|a| a.file.clone()))
        .collect()
}

/// `L1-screen-DP-4.mkv` / `L2-camera-0.mkv`: the layer id, the source kind
/// and the output name or the camera's device number.
pub fn video_file_name(layer: &Layer) -> String {
    let what = match &layer.source {
        VideoSource::Screen { output, .. } => format!("screen-{}", part(output)),
        VideoSource::Camera { device, .. } => format!("camera-{}", camera_number(device)),
    };
    sanitize(&format!("{}-{what}.mkv", layer.id))
}

/// `A1-mic.mka` / `A2-system.mka`.
pub fn audio_file_name(track: &AudioTrack) -> String {
    sanitize(&format!("{}-{}.mka", track.id, track.role.as_str()))
}

/// The trailing digits of a device path (`/dev/video0` ⇒ `0`), `0` without any.
fn camera_number(device: &str) -> String {
    let digits: Vec<char> = device
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return "0".to_owned();
    }
    part(&digits.into_iter().rev().collect::<String>())
}

/// Every character outside `[A-Za-z0-9._-]` becomes `_`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A sanitized name part capped at 64 bytes (ASCII, so any cut is a char boundary).
fn part(s: &str) -> String {
    let mut clean = sanitize(s);
    clean.truncate(MAX_PART_BYTES);
    clean
}

/// `round(num / den)`, at least 1: a keyframe every second (SPEC §5.2).
pub fn key_int_max(fps: Fps) -> u32 {
    let den = u64::from(fps.den.max(1));
    let rounded = (u64::from(fps.num) * 2 + den) / (2 * den);
    rounded.clamp(1, u64::from(u32::MAX)) as u32
}

/// The record encoder (A§2.1, A§9.5): the preference ([`encoder_pref`])
/// `x264` ⇒ X264; otherwise [`auto_encoder`].
pub fn choose_encoder(project: &Project, caps: &Capabilities, facts: &Facts) -> VideoEncoder {
    if encoder_pref(project, facts) == EncoderPref::X264 {
        return x264();
    }
    auto_encoder(project, caps, facts)
}

/// The encoder preference that applies (A§2.1): the project's
/// `recording.encoder` unless `auto`, else the config's
/// `encoder.preference`.
pub fn encoder_pref(project: &Project, facts: &Facts) -> EncoderPref {
    match project.recording.encoder {
        EncoderPref::Auto => facts.config.encoder.preference,
        explicit => explicit,
    }
}

/// What records unless the preference says x264: VA when the shell found
/// an encoder bound to the effective render node, its `va*h264enc`, else
/// its low-power `va*h264lpenc`, both CQP ([`va_factory`]). Then the
/// software encoders GStreamer has (`caps.gst_h264`): x264enc, else
/// openh264enc. Without that answer, or with neither, X264: a missing
/// x264enc is then the engine's `CaptureFailed`, which names it
/// (`missing_dependency`, A§4.8).
pub fn auto_encoder(project: &Project, caps: &Capabilities, facts: &Facts) -> VideoEncoder {
    if let Some(factory) = va_factory(project, caps, facts) {
        return VideoEncoder::Va {
            factory: factory.to_owned(),
            qp: RECORD_QP,
        };
    }
    if may_record_with_openh264(caps) {
        openh264()
    } else {
        x264()
    }
}

/// Whether OpenH264 may record: GStreamer has it and not x264enc (a VA
/// encoder bound to the node still comes first, [`auto_encoder`]).
pub fn may_record_with_openh264(caps: &Capabilities) -> bool {
    caps.gst_h264
        == Some(GstH264Caps {
            x264: false,
            openh264: true,
        })
}

/// The VA encoder recordings may use: the `va*h264enc` bound to the node
/// the capabilities describe, else its `va*h264lpenc`. That node must be
/// the configured one (project, then config) when one is, and an NVIDIA
/// node is never used unless it was configured.
pub fn va_factory<'a>(project: &Project, caps: &'a Capabilities, facts: &Facts) -> Option<&'a str> {
    let va = caps.vaapi.as_ref()?;
    let configured = project.recording.vaapi_device.as_deref().or(facts
        .config
        .encoder
        .vaapi_device
        .as_deref());
    let node_ok = match configured {
        Some(device) => device == va.device,
        None => va.vendor != NVIDIA,
    };
    if !node_ok {
        return None;
    }
    va.gst_h264_factory
        .as_deref()
        .or(va.gst_h264_lp_factory.as_deref())
}

/// `x264enc pass=qual quantizer=18 speed-preset=veryfast tune=zerolatency`
/// (without `pass=qual` x264enc records 2 Mbit/s CBR, A§8.11).
pub fn x264() -> VideoEncoder {
    VideoEncoder::X264 {
        pass: "qual".to_owned(),
        quantizer: RECORD_QP,
        speed_preset: "veryfast".to_owned(),
        tune: "zerolatency".to_owned(),
    }
}

/// `openh264enc rate-control=quality qp-min=qp-max=22 bitrate=100000000
/// complexity=low` on four threads ([`OPENH264_QP`] has the measurement).
pub fn openh264() -> VideoEncoder {
    VideoEncoder::OpenH264 {
        rate_control: OPENH264_RATE_CONTROL.to_owned(),
        qp: OPENH264_QP,
        bitrate: OPENH264_BITRATE,
        complexity: OPENH264_COMPLEXITY.to_owned(),
        threads: OPENH264_THREADS,
    }
}

/// The camera mode to negotiate (A§8.11): the closest mode at or above
/// `fps`, preferring the largest raw/MJPEG size.
///
/// - Raw and MJPEG (`image/jpeg`) modes are preferred; other compressed
///   formats (H.264, …) are used only when the camera offers nothing else.
/// - A rate counts as "at or above" within the NTSC factor 1000/1001, so a
///   29.97 fps mode serves a 30 fps canvas.
/// - Among the modes at the lowest qualifying rate the largest frame wins
///   (area, then width), raw before MJPEG on a tie. Without a qualifying
///   rate the fastest modes are used instead.
pub fn choose_camera_mode(modes: &[CameraMode], fps: Fps) -> Option<CameraMode> {
    let preferred: Vec<&CameraMode> = modes.iter().filter(|m| !is_other_compressed(m)).collect();
    let pool: Vec<&CameraMode> = if preferred.is_empty() {
        modes.iter().collect()
    } else {
        preferred
    };
    let qualifying: Vec<&CameraMode> = pool
        .iter()
        .copied()
        .filter(|m| at_least_ntsc(m.fps, fps))
        .collect();
    let rate = if qualifying.is_empty() {
        pool.iter().map(|m| m.fps).max_by(|a, b| cmp_fps(*a, *b))?
    } else {
        qualifying
            .iter()
            .map(|m| m.fps)
            .min_by(|a, b| cmp_fps(*a, *b))?
    };
    pool.into_iter()
        .filter(|m| cmp_fps(m.fps, rate).is_eq())
        .max_by(|a, b| {
            let area = |m: &CameraMode| u64::from(m.width) * u64::from(m.height);
            area(a)
                .cmp(&area(b))
                .then(a.width.cmp(&b.width))
                .then(is_mjpeg(b).cmp(&is_mjpeg(a)))
                .then(b.format.cmp(&a.format))
                .then(b.fps.den.cmp(&a.fps.den))
        })
        .cloned()
}

fn cmp_fps(a: Fps, b: Fps) -> std::cmp::Ordering {
    (u64::from(a.num) * u64::from(b.den)).cmp(&(u64::from(b.num) * u64::from(a.den)))
}

/// `rate ≥ target · 1000/1001`.
fn at_least_ntsc(rate: Fps, target: Fps) -> bool {
    u128::from(rate.num) * u128::from(target.den) * 1001
        >= u128::from(target.num) * u128::from(rate.den) * 1000
}

fn is_mjpeg(mode: &CameraMode) -> bool {
    let f = mode.format.to_ascii_lowercase();
    f == "image/jpeg" || f == "mjpg" || f == "mjpeg" || f == "jpeg"
}

/// Compressed formats other than MJPEG (they need a decoder the record bins don't have).
fn is_other_compressed(mode: &CameraMode) -> bool {
    if is_mjpeg(mode) {
        return false;
    }
    let f = mode.format.to_ascii_lowercase();
    ["h264", "h265", "hevc", "vp8", "vp9", "av1", "mpeg"]
        .iter()
        .any(|codec| f.contains(codec))
        || (f.starts_with("video/x-") && !f.starts_with("video/x-raw"))
        || f.starts_with("image/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{CameraInfo, ScreenInfo};
    use crate::model::Fps;
    use crate::state::{GstH264Caps, VaapiCaps};
    use crate::testkit::{self, ProjectBuilder};

    fn mode(width: u32, height: u32, fps: Fps, format: &str) -> CameraMode {
        CameraMode {
            width,
            height,
            fps,
            format: format.to_owned(),
        }
    }

    fn c920_modes() -> Vec<CameraMode> {
        vec![
            mode(640, 480, Fps::new(30, 1), "YUY2"),
            mode(1280, 720, Fps::new(10, 1), "YUY2"),
            mode(1920, 1080, Fps::new(5, 1), "YUY2"),
            mode(1280, 720, Fps::new(30, 1), "image/jpeg"),
            mode(1920, 1080, Fps::new(30, 1), "image/jpeg"),
            mode(1280, 720, Fps::new(60, 1), "image/jpeg"),
            mode(1920, 1080, Fps::new(30, 1), "video/x-h264"),
            mode(3840, 2160, Fps::new(30, 1), "video/x-h264"),
        ]
    }

    fn va_caps(device: &str, vendor: u16, factory: Option<&str>) -> Capabilities {
        let mut caps = testkit::capabilities();
        caps.vaapi = Some(VaapiCaps {
            device: device.to_owned(),
            vendor,
            ffmpeg_h264: true,
            gst_h264_factory: factory.map(str::to_owned),
            gst_h264_lp_factory: None,
        });
        caps
    }

    /// `caps` with GStreamer's software encoders known.
    fn with_gst(mut caps: Capabilities, x264: bool, openh264: bool) -> Capabilities {
        caps.gst_h264 = Some(GstH264Caps { x264, openh264 });
        caps
    }

    /// `caps` whose VA node has a low-power encoder too (or only, without
    /// a full one).
    fn with_lp(mut caps: Capabilities, factory: &str) -> Capabilities {
        caps.vaapi.as_mut().unwrap().gst_h264_lp_factory = Some(factory.to_owned());
        caps
    }

    fn full_scene() -> Project {
        ProjectBuilder::new()
            .screen_layer("L1", "DP-4")
            .camera_layer("L2", "/dev/video0")
            .mic("A1", "alsa_input.usb-Blue_Yeti")
            .system("A2", "alsa_output.pci.analog-stereo.monitor")
            .build()
    }

    fn camera_facts() -> Facts {
        Facts {
            cameras: vec![CameraInfo {
                device: "/dev/video0".into(),
                label: "Logitech C920".into(),
                modes: c920_modes(),
            }],
            ..Facts::default()
        }
    }

    #[test]
    fn camera_mode_picks_the_closest_rate_then_the_largest_size() {
        let modes = c920_modes();
        let at30 = choose_camera_mode(&modes, Fps::new(30, 1)).unwrap();
        assert_eq!(at30, mode(1920, 1080, Fps::new(30, 1), "image/jpeg"));
        let at60 = choose_camera_mode(&modes, Fps::new(60, 1)).unwrap();
        assert_eq!(at60, mode(1280, 720, Fps::new(60, 1), "image/jpeg"));
        let at25 = choose_camera_mode(&modes, Fps::new(25, 1)).unwrap();
        assert_eq!(at25.fps, Fps::new(30, 1), "the closest rate at or above 25");
        assert_eq!((at25.width, at25.height), (1920, 1080));
        // Nothing reaches 120 fps: the fastest modes are used.
        let at120 = choose_camera_mode(&modes, Fps::new(120, 1)).unwrap();
        assert_eq!(at120, mode(1280, 720, Fps::new(60, 1), "image/jpeg"));
        assert_eq!(choose_camera_mode(&[], Fps::new(30, 1)), None);
    }

    #[test]
    fn camera_mode_accepts_ntsc_rates_and_prefers_raw_on_ties() {
        let modes = vec![
            mode(1280, 720, Fps::new(30000, 1001), "NV12"),
            mode(640, 480, Fps::new(60, 1), "NV12"),
        ];
        assert_eq!(
            choose_camera_mode(&modes, Fps::new(30, 1)).unwrap(),
            modes[0],
            "29.97 serves a 30 fps canvas"
        );
        let tie = vec![
            mode(1280, 720, Fps::new(30, 1), "image/jpeg"),
            mode(1280, 720, Fps::new(30, 1), "YUY2"),
        ];
        assert_eq!(
            choose_camera_mode(&tie, Fps::new(30, 1)).unwrap().format,
            "YUY2"
        );
        // Only compressed modes: still a choice.
        let h264 = vec![mode(1920, 1080, Fps::new(30, 1), "video/x-h264")];
        assert_eq!(choose_camera_mode(&h264, Fps::new(30, 1)).unwrap(), h264[0]);
    }

    #[test]
    fn file_names_follow_the_spec_and_are_sanitized() {
        let project = full_scene();
        let names: Vec<String> = project.scene.layers.iter().map(video_file_name).collect();
        assert_eq!(names, ["L1-screen-DP-4.mkv", "L2-camera-0.mkv"]);
        let names: Vec<String> = project.scene.audio.iter().map(audio_file_name).collect();
        assert_eq!(names, ["A1-mic.mka", "A2-system.mka"]);

        let odd = ProjectBuilder::new()
            .screen_layer("L3", "HDMI A/1 ü")
            .camera_layer("L4", "/dev/v4l/by-id/usb-Cam_12-video-index17")
            .camera_layer("L5", "pipewire:Integrated Camera")
            .build();
        let names: Vec<String> = odd.scene.layers.iter().map(video_file_name).collect();
        assert_eq!(
            names,
            [
                "L3-screen-HDMI_A_1__.mkv",
                "L4-camera-17.mkv",
                "L5-camera-0.mkv"
            ]
        );
        let long = ProjectBuilder::new()
            .screen_layer("L1", &"X".repeat(300))
            .build();
        let name = video_file_name(&long.scene.layers[0]);
        assert!(name.ends_with(".mkv") && name.len() <= 120, "{name}");
    }

    #[test]
    fn encoder_choice() {
        let project = full_scene();
        let facts = Facts::default();
        let none = testkit::capabilities();
        assert_eq!(choose_encoder(&project, &none, &facts), x264(), "no VA-API");
        let intel = va_caps("/dev/dri/renderD129", 0x8086, Some("vah264enc"));
        assert_eq!(
            choose_encoder(&project, &intel, &facts),
            VideoEncoder::Va {
                factory: "vah264enc".into(),
                qp: 18
            }
        );
        let no_factory = va_caps("/dev/dri/renderD129", 0x8086, None);
        assert_eq!(choose_encoder(&project, &no_factory, &facts), x264());

        // The project's x264 wins over everything.
        let mut forced = project.clone();
        forced.recording.encoder = EncoderPref::X264;
        assert_eq!(choose_encoder(&forced, &intel, &facts), x264());
        // `auto` defers to the config.
        let mut cfg = Facts::default();
        cfg.config.encoder.preference = EncoderPref::X264;
        assert_eq!(choose_encoder(&project, &intel, &cfg), x264());
        // The project's vaapi beats a config x264.
        let mut va = project.clone();
        va.recording.encoder = EncoderPref::Vaapi;
        assert!(matches!(
            choose_encoder(&va, &intel, &cfg),
            VideoEncoder::Va { .. }
        ));

        // A configured node must be the node the capabilities describe.
        let mut other_node = project.clone();
        other_node.recording.vaapi_device = Some("/dev/dri/renderD130".into());
        assert_eq!(choose_encoder(&other_node, &intel, &facts), x264());
        let mut cfg_node = Facts::default();
        cfg_node.config.encoder.vaapi_device = Some("/dev/dri/renderD129".into());
        assert!(matches!(
            choose_encoder(&project, &intel, &cfg_node),
            VideoEncoder::Va { .. }
        ));

        // NVIDIA only when configured explicitly.
        let nvidia = va_caps("/dev/dri/renderD128", 0x10de, Some("vah264enc"));
        assert_eq!(choose_encoder(&project, &nvidia, &facts), x264());
        let mut pinned = project.clone();
        pinned.recording.vaapi_device = Some("/dev/dri/renderD128".into());
        assert!(matches!(
            choose_encoder(&pinned, &nvidia, &facts),
            VideoEncoder::Va { .. }
        ));

        // The node's full encoder before its low-power one, which records
        // when it is the only one (Intel Arc, Meteor Lake and newer).
        let va = |factory: &str| VideoEncoder::Va {
            factory: factory.into(),
            qp: 18,
        };
        let both = with_lp(intel.clone(), "vah264lpenc");
        assert_eq!(choose_encoder(&project, &both, &facts), va("vah264enc"));
        let lp_only = with_lp(no_factory.clone(), "vah264lpenc");
        assert_eq!(
            choose_encoder(&project, &lp_only, &facts),
            va("vah264lpenc")
        );
        // Under the same node rules: not on an unconfigured NVIDIA node.
        let nvidia_lp = with_lp(va_caps("/dev/dri/renderD128", 0x10de, None), "vah264lpenc");
        assert_eq!(choose_encoder(&project, &nvidia_lp, &facts), x264());
        assert_eq!(choose_encoder(&forced, &lp_only, &facts), x264());

        // Without VA: x264enc, else openh264enc, as GStreamer has them.
        let software = |x264, openh264| {
            choose_encoder(
                &project,
                &with_gst(no_factory.clone(), x264, openh264),
                &facts,
            )
        };
        assert_eq!(software(true, true), x264());
        assert_eq!(software(true, false), x264());
        assert_eq!(software(false, true), openh264());
        // Neither: x264, which the engine names as missing.
        assert_eq!(software(false, false), x264());
        // VA still comes first, and an explicit x264 stays x264.
        let fedora = with_gst(intel.clone(), false, true);
        assert_eq!(choose_encoder(&project, &fedora, &facts), va("vah264enc"));
        assert_eq!(choose_encoder(&forced, &fedora, &facts), x264());
        // The project's vaapi on a machine without VA: OpenH264 then.
        assert_eq!(
            choose_encoder(&va_pref(&project), &with_gst(none, false, true), &facts),
            openh264()
        );
    }

    fn va_pref(project: &Project) -> Project {
        let mut va = project.clone();
        va.recording.encoder = EncoderPref::Vaapi;
        va
    }

    #[test]
    fn keyframe_interval_is_one_second() {
        assert_eq!(key_int_max(Fps::new(30, 1)), 30);
        assert_eq!(key_int_max(Fps::new(30000, 1001)), 30);
        assert_eq!(key_int_max(Fps::new(24000, 1001)), 24);
        assert_eq!(key_int_max(Fps::new(60000, 1001)), 60);
        assert_eq!(key_int_max(Fps::new(25, 2)), 13);
        assert_eq!(key_int_max(Fps::new(1, 2)), 1);
    }

    #[test]
    fn empty_scene_is_invalid() {
        let project = ProjectBuilder::new().build();
        let err = plan_capture(
            &project,
            &TakeId::numbered(1),
            &testkit::capabilities(),
            &Facts::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Invalid);
    }

    #[test]
    fn planned_streams_mirror_the_plan() {
        let project = full_scene();
        let plan = plan_capture(
            &project,
            &TakeId::numbered(4),
            &testkit::capabilities(),
            &camera_facts(),
        )
        .unwrap();
        let streams = planned_streams(&plan);
        let ids: Vec<&str> = streams.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["L1", "L2", "A1", "A2"]);
        assert!(streams.iter().all(|s| s.width.is_none()
            && s.sample_rate.is_none()
            && s.frames.is_none()
            && s.start_offset_ns == 0));
        assert_eq!(
            plan_files(&plan),
            streams.iter().map(|s| s.file.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn snapshot_full_scene_x264() {
        let mut project = full_scene();
        // Hidden layers are recorded too.
        project.scene.layers[1].visible = false;
        let plan = plan_capture(
            &project,
            &TakeId::numbered(4),
            &testkit::capabilities(),
            &camera_facts(),
        )
        .unwrap();
        insta::assert_json_snapshot!(plan);
    }

    #[test]
    fn snapshot_vaapi_ntsc() {
        let project = ProjectBuilder::new()
            .canvas(2560, 1440, Fps::new(30000, 1001))
            .screen_layer("L1", "DP-4")
            .screen_layer("L2", "eDP-1")
            .camera_layer("L3", "/dev/video2")
            .mic("A1", "alsa_input.usb-Blue_Yeti")
            .build();
        let caps = va_caps("/dev/dri/renderD129", 0x8086, Some("varenderD129h264enc"));
        let mut facts = camera_facts();
        facts.cameras[0].device = "/dev/video2".into();
        let plan = plan_capture(&project, &TakeId::numbered(12), &caps, &facts).unwrap();
        insta::assert_json_snapshot!(plan);
    }

    #[test]
    fn snapshot_unknown_camera_and_system_audio() {
        // A camera missing from the facts negotiates its own mode.
        let project = ProjectBuilder::new()
            .camera_layer("L7", "/dev/video9")
            .system("A3", "alsa_output.monitor")
            .build();
        let plan = plan_capture(
            &project,
            &TakeId::numbered(10000),
            &testkit::capabilities(),
            &Facts::default(),
        )
        .unwrap();
        insta::assert_json_snapshot!(plan);
    }

    /// The encoder chain (A§9.5) on an Intel node: its full VA encoder
    /// (over its low-power one), its low-power one alone, x264enc alone,
    /// openh264enc alone (stock Fedora) and nothing, which plans x264 for
    /// the engine to name as missing.
    #[test]
    fn snapshot_encoder_chain() {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-4")
            .mic("A1", "alsa_input.usb-Blue_Yeti")
            .build();
        let intel = |factory: Option<&str>| va_caps("/dev/dri/renderD129", 0x8086, factory);
        let full = with_lp(intel(Some("varenderD129h264enc")), "varenderD129h264lpenc");
        let cases = [
            ("va_full", with_gst(full, true, true)),
            (
                "va_lp_only",
                with_gst(with_lp(intel(None), "vah264lpenc"), false, true),
            ),
            ("x264_only", with_gst(intel(None), true, false)),
            ("openh264_only", with_gst(intel(None), false, true)),
            ("none", with_gst(intel(None), false, false)),
        ];
        for (name, caps) in cases {
            let plan = plan_capture(&project, &TakeId::numbered(3), &caps, &Facts::default());
            insta::assert_json_snapshot!(format!("snapshot_encoder_{name}"), plan.unwrap());
        }
    }

    /// OpenH264 encodes at most 9,437,184 pixels a frame, whatever the
    /// shape: a take it would record a larger screen or camera mode with
    /// is refused up front (a missing x264enc), and one whose size isn't
    /// known goes ahead. Another encoder records any size.
    #[test]
    fn openh264_refuses_frames_larger_than_it_encodes() {
        let project = full_scene();
        let take = TakeId::numbered(3);
        let fedora = with_gst(testkit::capabilities(), false, true);
        let facts = |screen: (u32, u32), camera: (u32, u32)| {
            let mut facts = camera_facts();
            facts.screens = vec![ScreenInfo {
                output: "DP-4".into(),
                description: "Dell UP2715K".into(),
                x: 0,
                y: 0,
                width: screen.0,
                height: screen.1,
                scale: 2.0,
            }];
            facts.cameras[0].modes = vec![mode(camera.0, camera.1, Fps::new(30, 1), "NV12")];
            facts
        };
        let refused = |facts: &Facts| plan_capture(&project, &take, &fedora, facts).err();

        // 4096×2304 is the largest 16:9 frame; any shape up to that area passes.
        for size in [(4096, 2304), (2304, 4096), (5120, 1440), (3840, 2160)] {
            assert_eq!(refused(&facts(size, (1920, 1080))), None, "{size:?}");
        }
        // Nothing known about the sizes: the take goes ahead.
        assert_eq!(refused(&Facts::default()), None);

        let err = refused(&facts((5120, 2880), (1920, 1080))).unwrap();
        assert_eq!(err.code, ErrorCode::MissingDependency);
        assert_eq!(
            err.message,
            "can't record L1 (DP-4): its frames are 5120×2880, and OpenH264, which records \
             here, encodes at most 9437184 pixels a frame (4096×2304)"
        );
        let hint = err.hint.unwrap();
        assert!(
            hint.starts_with("install x264enc (gst-plugins-ugly)"),
            "{hint}"
        );
        // One pixel row over is too much; a camera mode counts the same.
        assert!(refused(&facts((4096, 2305), (1920, 1080))).is_some());
        let err = refused(&facts((2560, 1440), (4656, 3496))).unwrap();
        assert!(
            err.message
                .starts_with("can't record L2 (/dev/video0): its frames are 4656×3496"),
            "{}",
            err.message
        );

        // x264enc or a VA encoder records it.
        let huge = facts((7680, 4320), (4656, 3496));
        for caps in [
            with_gst(testkit::capabilities(), true, true),
            with_gst(
                va_caps("/dev/dri/renderD129", 0x8086, Some("vah264enc")),
                false,
                true,
            ),
        ] {
            assert!(plan_capture(&project, &take, &caps, &huge).is_ok());
        }
    }
}
