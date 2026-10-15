//! Pure pieces of the capture engine: which sources a scene or plan needs,
//! element settings derived from the plan (A§8.11), caps strings and file
//! paths. No GStreamer here, so all of it is unit-tested directly.

use qrec_core::Fps;
use qrec_core::ctx::CameraMode;
use qrec_core::model::{Scene, VideoSource};
use qrec_core::plans::{AudioRecord, CapturePlan, VideoEncoder, VideoRecord};
use qrec_core::time::frame_ns;

use super::fake::FakeTone;

/// One capture device of the live pipeline: everything that needs its own
/// source element. Layers sharing a screen share its source (and tee).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceSpec {
    /// A monitor through the ScreenCast portal (`pipewiresrc`).
    Screen { output: String },
    /// A V4L2 camera; `mode` is the negotiated mode when one was chosen.
    Camera {
        device: String,
        mode: Option<CameraKey>,
    },
    /// A PulseAudio/PipeWire source (`pulsesrc`).
    Audio { device: String },
}

/// A camera mode in a hashable, ordered form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CameraKey {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub format: String,
}

impl From<&CameraMode> for CameraKey {
    fn from(mode: &CameraMode) -> Self {
        Self {
            width: mode.width,
            height: mode.height,
            fps_num: mode.fps.num,
            fps_den: mode.fps.den,
            format: mode.format.clone(),
        }
    }
}

impl CameraKey {
    pub fn fps(&self) -> Fps {
        Fps::new(self.fps_num, self.fps_den)
    }

    /// `image/jpeg,…` for MJPEG modes, `video/x-raw,format=…,…` otherwise.
    pub fn caps_string(&self) -> String {
        let size = format!(
            "width={},height={},framerate={}/{}",
            self.width, self.height, self.fps_num, self.fps_den
        );
        if self.is_mjpeg() {
            format!("image/jpeg,{size}")
        } else if is_video_format_name(&self.format) {
            format!("video/x-raw,format={},{size}", self.format)
        } else {
            format!("video/x-raw,{size}")
        }
    }

    pub fn is_mjpeg(&self) -> bool {
        let f = self.format.to_ascii_lowercase();
        f == "image/jpeg" || f == "mjpg" || f == "mjpeg" || f == "jpeg"
    }
}

/// A plausible GStreamer raw format name (`YUY2`, `NV12`, `I420`, `BGRx`).
fn is_video_format_name(format: &str) -> bool {
    !format.is_empty()
        && format.len() <= 16
        && format
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl SourceSpec {
    /// The same device, ignoring the camera mode.
    pub fn same_device(&self, other: &SourceSpec) -> bool {
        match (self, other) {
            (SourceSpec::Screen { output: a }, SourceSpec::Screen { output: b }) => a == b,
            (SourceSpec::Camera { device: a, .. }, SourceSpec::Camera { device: b, .. }) => a == b,
            (SourceSpec::Audio { device: a }, SourceSpec::Audio { device: b }) => a == b,
            _ => false,
        }
    }

    /// Whether a source built as `self` can serve `wanted` (same device and,
    /// when `wanted` names a camera mode, that mode).
    pub fn serves(&self, wanted: &SourceSpec) -> bool {
        match (self, wanted) {
            (
                SourceSpec::Camera {
                    device: a,
                    mode: have,
                },
                SourceSpec::Camera {
                    device: b,
                    mode: want,
                },
            ) => a == b && (want.is_none() || have == want),
            _ => self == wanted,
        }
    }

    pub fn is_video(&self) -> bool {
        !matches!(self, SourceSpec::Audio { .. })
    }

    /// `screen DP-4`, `camera /dev/video0`, `audio alsa_input.…`.
    pub fn describe(&self) -> String {
        match self {
            SourceSpec::Screen { output } => format!("screen {output}"),
            SourceSpec::Camera { device, .. } => format!("camera {device}"),
            SourceSpec::Audio { device } => format!("audio source {device}"),
        }
    }

    /// The output or device name.
    pub fn device_name(&self) -> &str {
        match self {
            SourceSpec::Screen { output } => output,
            SourceSpec::Camera { device, .. } | SourceSpec::Audio { device } => device,
        }
    }

    pub fn of_video(source: &VideoSource, mode: Option<&CameraMode>) -> Self {
        match source {
            VideoSource::Screen { output, .. } => SourceSpec::Screen {
                output: output.clone(),
            },
            VideoSource::Camera { device, .. } => SourceSpec::Camera {
                device: device.clone(),
                mode: mode.map(CameraKey::from),
            },
        }
    }

    pub fn of_record(rec: &VideoRecord) -> Self {
        Self::of_video(&rec.source, rec.camera_mode.as_ref())
    }

    pub fn of_audio(rec: &AudioRecord) -> Self {
        SourceSpec::Audio {
            device: rec.device.clone(),
        }
    }
}

/// The sources a live scene shows (cameras without a mode yet), deduplicated.
pub fn scene_sources(scene: &Scene) -> Vec<SourceSpec> {
    let mut out: Vec<SourceSpec> = Vec::new();
    let mut push = |spec: SourceSpec| {
        if !out.iter().any(|s| s.same_device(&spec)) {
            out.push(spec);
        }
    };
    for layer in &scene.layers {
        push(SourceSpec::of_video(&layer.source, None));
    }
    for track in &scene.audio {
        push(SourceSpec::Audio {
            device: track.source.clone(),
        });
    }
    out
}

/// The sources a capture plan records from, deduplicated, with camera modes.
pub fn plan_sources(plan: &CapturePlan) -> Vec<SourceSpec> {
    let mut out: Vec<SourceSpec> = Vec::new();
    let specs = plan
        .video
        .iter()
        .map(SourceSpec::of_record)
        .chain(plan.audio.iter().map(SourceSpec::of_audio));
    for spec in specs {
        if !out.contains(&spec) {
            out.push(spec);
        }
    }
    out
}

/// A human name for a recorded stream, for `CaptureFailed.source` and logs:
/// `L1 screen DP-4 (GIGA-BYTE GS27QA)`, `A1 mic alsa_input.usb-…`.
pub fn video_label(rec: &VideoRecord) -> String {
    let (what, name, label) = match &rec.source {
        VideoSource::Screen { output, label } => ("screen", output, label),
        VideoSource::Camera { device, label } => ("camera", device, label),
    };
    if label.is_empty() || label == name {
        format!("{} {what} {name}", rec.id)
    } else {
        format!("{} {what} {name} ({label})", rec.id)
    }
}

pub fn audio_label(rec: &AudioRecord) -> String {
    format!("{} audio {}", rec.id, rec.device)
}

/// One frame at `fps`, in ns (`frame_ns(1)`).
pub fn frame_duration_ns(fps: Fps) -> u64 {
    frame_ns(1, fps).max(1) as u64
}

/// The project-relative path of a record file: `file` as is when it is
/// already a path (`takes/take-0001/L1-screen-DP-4.mkv`), else joined with
/// the plan's take dir.
pub fn record_file_rel(plan_dir: &str, file: &str) -> String {
    if file.contains('/') {
        file.to_owned()
    } else {
        format!("{}/{file}", plan_dir.trim_end_matches('/'))
    }
}

/// The encoder factory and its properties as `(name, value)` strings
/// (A§8.11): x264 `pass=qual quantizer=18 speed-preset=veryfast
/// tune=zerolatency key-int-max=<fps>`; VA `rate-control=cqp qpi=qpp=qpb=<qp>
/// key-int-max=<fps>`.
pub fn encoder_settings(rec: &VideoRecord) -> (String, Vec<(&'static str, String)>) {
    let key_int = rec.key_int_max.max(1).to_string();
    match &rec.encoder {
        VideoEncoder::X264 {
            pass,
            quantizer,
            speed_preset,
            tune,
        } => (
            "x264enc".to_owned(),
            vec![
                ("pass", pass.clone()),
                ("quantizer", quantizer.to_string()),
                ("speed-preset", speed_preset.clone()),
                ("tune", tune.clone()),
                ("key-int-max", key_int),
            ],
        ),
        VideoEncoder::Va { factory, qp } => (
            factory.clone(),
            vec![
                ("rate-control", "cqp".to_owned()),
                ("qpi", qp.to_string()),
                ("qpp", qp.to_string()),
                ("qpb", qp.to_string()),
                ("key-int-max", key_int),
            ],
        ),
    }
}

/// The raw format the encoder takes: NV12 for VA, I420 for x264.
pub fn encoder_input_format(encoder: &VideoEncoder) -> &'static str {
    match encoder {
        VideoEncoder::Va { .. } => "NV12",
        VideoEncoder::X264 { .. } => "I420",
    }
}

// `recorder.log` lines a rebuild reads back (A§7.7): take facts aren't in
// the history, so a take rebuilt from its files gets its streams' ids and
// offsets, and which of them were lost, from its log. Every line the
// engine writes starts with a UTC timestamp (`2026-10-15T13:25:00.871Z`).

/// One stream's `startOffsetNs`: `<id>: <file> startOffsetNs=<n>`. The
/// engine logs it for every stream once recording starts (the offsets are
/// known then) and again at the stop (with ` eos=…`).
pub fn start_offset_line(id: &str, file: &str, start_offset_ns: i64) -> String {
    format!("{id}: {file} {START_OFFSET_KEY}{start_offset_ns}")
}

const START_OFFSET_KEY: &str = "startOffsetNs=";

/// A stream lost while recording or stopping: `<id> lost: <message>`.
pub fn stream_lost_line(id: &str, message: &str) -> String {
    format!("{id} {LOST_WORD} {message}")
}

const LOST_WORD: &str = "lost:";

/// The stop: `stop at T1 = <ns> ns (<ms> ms after T0)`, or `cancel at …`.
/// Streams lost after it count as recorded (the core never hears of them).
pub fn stop_line(cancel: bool, t1_ns: u64, after_t0_ms: u64) -> String {
    let what = if cancel { "cancel" } else { "stop" };
    format!("{what} {T1_WORDS} {t1_ns} ns ({after_t0_ms} ms after T0)")
}

const T1_WORDS: &str = "at T1 =";

/// What a take's `recorder.log` tells a rebuild.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LoggedTake {
    /// Each stream's id and `startOffsetNs`, by its file
    /// (`takes/take-0001/L1-screen-DP-4.mkv`); the last line wins.
    pub streams: std::collections::BTreeMap<String, LoggedStream>,
    /// The ids of the streams lost before the stop (the core marked their
    /// layers or tracks missing in the take's scene).
    pub lost: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedStream {
    pub id: String,
    pub start_offset_ns: i64,
}

/// Reads the lines [`start_offset_line`], [`stream_lost_line`] and
/// [`stop_line`] write; everything else (and offsets that aren't a count of
/// ns ≥ 0) is skipped.
pub fn read_take_log(log: &str) -> LoggedTake {
    let mut take = LoggedTake::default();
    let mut stopped = false;
    for line in log.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        // Without the timestamp.
        let words = match words.first() {
            Some(first) if first.ends_with('Z') && first.contains('T') => &words[1..],
            _ => &words[..],
        };
        match words {
            [id, file, value, ..] if id.ends_with(':') => {
                let Some(value) = value.strip_prefix(START_OFFSET_KEY) else {
                    continue;
                };
                if let Ok(ns) = value.parse::<i64>()
                    && ns >= 0
                {
                    let stream = LoggedStream {
                        id: id.trim_end_matches(':').to_owned(),
                        start_offset_ns: ns,
                    };
                    take.streams.insert((*file).to_owned(), stream);
                }
            }
            [id, word, ..] if *word == LOST_WORD => {
                if !stopped {
                    take.lost.insert((*id).to_owned());
                }
            }
            [what, rest @ ..]
                if matches!(*what, "stop" | "cancel") && rest.join(" ").starts_with(T1_WORDS) =>
            {
                stopped = true;
            }
            _ => {}
        }
    }
    take
}

/// `audiotestsrc wave=ticks` settings for a fake tone: sine periods per tick
/// and the tick interval in ns.
pub fn tick_settings(tone: &FakeTone) -> (u32, u64) {
    let periods = (u64::from(tone.freq_hz) * u64::from(tone.tone_ms) / 1000).max(1);
    (
        periods.min(u64::from(u32::MAX)) as u32,
        u64::from(tone.period_ms.max(1)) * 1_000_000,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrec_core::ids::{LayerId, TakeId, TrackId};
    use qrec_core::model::{AudioRole, AudioTrack, Crop, Fit, Layer, Rect};

    fn x264() -> VideoEncoder {
        VideoEncoder::X264 {
            pass: "qual".into(),
            quantizer: 18,
            speed_preset: "veryfast".into(),
            tune: "zerolatency".into(),
        }
    }

    fn screen(id: &str, output: &str) -> VideoRecord {
        VideoRecord {
            id: id.parse::<LayerId>().unwrap(),
            source: VideoSource::Screen {
                output: output.into(),
                label: "GIGA-BYTE GS27QA".into(),
            },
            file: format!("{id}-screen-{output}.mkv"),
            encoder: x264(),
            key_int_max: 30,
            camera_mode: None,
        }
    }

    fn mode(w: u32, h: u32, fps: u32, format: &str) -> CameraMode {
        CameraMode {
            width: w,
            height: h,
            fps: Fps::new(fps, 1),
            format: format.into(),
        }
    }

    #[test]
    fn camera_caps_strings() {
        assert_eq!(
            CameraKey::from(&mode(1280, 720, 30, "image/jpeg")).caps_string(),
            "image/jpeg,width=1280,height=720,framerate=30/1"
        );
        assert_eq!(
            CameraKey::from(&mode(640, 480, 60, "YUY2")).caps_string(),
            "video/x-raw,format=YUY2,width=640,height=480,framerate=60/1"
        );
        assert!(CameraKey::from(&mode(1, 1, 1, "MJPG")).is_mjpeg());
        // A format that isn't a format name is left to negotiation.
        assert_eq!(
            CameraKey::from(&mode(640, 480, 30, "weird, value")).caps_string(),
            "video/x-raw,width=640,height=480,framerate=30/1"
        );
    }

    #[test]
    fn sources_of_scenes_and_plans() {
        let layer = |id: &str, source: VideoSource| Layer {
            id: id.parse().unwrap(),
            name: id.into(),
            visible: true,
            opacity: 1.0,
            source,
            rect: Rect::new(0, 0, 100, 100),
            crop: Crop::default(),
            fit: Fit::Contain,
            locked: false,
            missing: false,
        };
        let scene = Scene {
            layers: vec![
                layer(
                    "L1",
                    VideoSource::Screen {
                        output: "DP-4".into(),
                        label: "a".into(),
                    },
                ),
                layer(
                    "L2",
                    VideoSource::Screen {
                        output: "DP-4".into(),
                        label: "a".into(),
                    },
                ),
                layer(
                    "L3",
                    VideoSource::Camera {
                        device: "/dev/video0".into(),
                        label: "c".into(),
                    },
                ),
            ],
            audio: vec![AudioTrack {
                id: "A1".parse().unwrap(),
                role: AudioRole::Mic,
                source: "alsa_input.x".into(),
                label: "Mic".into(),
                gain_db: 0.0,
                muted: false,
                sync_offset_ms: 0,
                missing: false,
            }],
        };
        assert_eq!(
            scene_sources(&scene),
            vec![
                SourceSpec::Screen {
                    output: "DP-4".into()
                },
                SourceSpec::Camera {
                    device: "/dev/video0".into(),
                    mode: None
                },
                SourceSpec::Audio {
                    device: "alsa_input.x".into()
                },
            ]
        );

        let mut cam = screen("L3", "x");
        cam.source = VideoSource::Camera {
            device: "/dev/video0".into(),
            label: "C920".into(),
        };
        cam.camera_mode = Some(mode(1280, 720, 30, "image/jpeg"));
        let plan = CapturePlan {
            take: TakeId::numbered(1),
            dir: "takes/take-0001".into(),
            log: "takes/take-0001/recorder.log".into(),
            fps: Fps::new(30, 1),
            health_check_ms: 500,
            first_buffer_ms: 3000,
            eos_timeout_ms: 10_000,
            video: vec![screen("L1", "DP-4"), screen("L2", "DP-4"), cam],
            audio: vec![AudioRecord {
                id: "A1".parse::<TrackId>().unwrap(),
                device: "alsa_input.x".into(),
                file: "A1-mic.mka".into(),
                sample_rate: 48_000,
            }],
        };
        let sources = plan_sources(&plan);
        assert_eq!(sources.len(), 3);
        let live_cam = SourceSpec::Camera {
            device: "/dev/video0".into(),
            mode: None,
        };
        assert!(live_cam.same_device(&sources[1]));
        assert!(
            !live_cam.serves(&sources[1]),
            "the plan wants a specific mode"
        );
        assert!(sources[1].serves(&live_cam));
        assert!(sources[1].serves(&sources[1]));
        assert!(!sources[0].same_device(&sources[2]));
        assert_eq!(sources[2].describe(), "audio source alsa_input.x");
        assert!(sources[0].is_video() && !sources[2].is_video());
    }

    #[test]
    fn labels_and_paths() {
        assert_eq!(
            video_label(&screen("L1", "DP-4")),
            "L1 screen DP-4 (GIGA-BYTE GS27QA)"
        );
        let rec = AudioRecord {
            id: "A2".parse().unwrap(),
            device: "alsa_output.x.monitor".into(),
            file: "A2-system.mka".into(),
            sample_rate: 48_000,
        };
        assert_eq!(audio_label(&rec), "A2 audio alsa_output.x.monitor");
        assert_eq!(
            record_file_rel("takes/take-0001", "L1-screen-DP-4.mkv"),
            "takes/take-0001/L1-screen-DP-4.mkv"
        );
        assert_eq!(
            record_file_rel("takes/take-0001/", "takes/take-0001/A1-mic.mka"),
            "takes/take-0001/A1-mic.mka"
        );
    }

    #[test]
    fn a_rebuild_reads_streams_and_losses_from_the_recorder_log() {
        let l1 = "takes/take-0001/L1-screen-DP-4.mkv";
        let cam = "takes/take-0001/cam-a-camera-0.mkv";
        let a1 = "takes/take-0001/A1-mic.mka";
        assert_eq!(
            start_offset_line("L1", l1, 379_829),
            format!("L1: {l1} startOffsetNs=379829")
        );
        assert_eq!(stream_lost_line("L2", "gone"), "L2 lost: gone");
        assert_eq!(
            stop_line(false, 25_000_000_000, 24_950),
            "stop at T1 = 25000000000 ns (24950 ms after T0)"
        );
        let at = |line: String| format!("2026-10-15T13:25:01.402Z {line}");
        // A log as the engine writes it: the lines at the start, other
        // lines, the summary at the stop (same values, plus `eos`).
        let log = [
            at(format!("attached at T0 = 67619293 ns: {l1}, {cam}, {a1}")),
            at("L1: first buffer at T0 + 11.623 ms".into()),
            at("recording".into()),
            at(start_offset_line("L1", l1, 379_829)),
            at(start_offset_line("cam-a", cam, 532_318_194)),
            at(start_offset_line("A1", a1, 2_380_707)),
            at(stream_lost_line(
                "cam-a",
                "no frames from cam-a camera for 3.0 s: the camera stopped",
            )),
            at(stop_line(false, 25_000_000_000, 24_950)),
            // Lost while stopping: the core never hears of it.
            at(stream_lost_line("A1", "the device was unplugged")),
            at("L1: closed after 1190 buffers (last at T0 + 24634.735 ms), 0 fill frames, EOS sent".into()),
            at(format!("{} eos=true", start_offset_line("L1", l1, 379_829))),
            // Not offsets: skipped.
            at("A2: takes/take-0001/A2.mka startOffsetNs=-5".into()),
            at("A3: takes/take-0001/A3.mka startOffsetNs=lots".into()),
            at("stopped".into()),
            "garbage".to_owned(),
            String::new(),
        ]
        .join("\n");
        let take = read_take_log(&log);
        let stream = |id: &str, start_offset_ns| LoggedStream {
            id: id.into(),
            start_offset_ns,
        };
        assert_eq!(
            take.streams.into_iter().collect::<Vec<_>>(),
            [
                (a1.to_owned(), stream("A1", 2_380_707)),
                (l1.to_owned(), stream("L1", 379_829)),
                (cam.to_owned(), stream("cam-a", 532_318_194)),
            ]
        );
        assert_eq!(take.lost.into_iter().collect::<Vec<_>>(), ["cam-a"]);
        assert_eq!(read_take_log(""), LoggedTake::default());
    }

    #[test]
    fn element_settings_follow_the_architecture() {
        assert_eq!(frame_duration_ns(Fps::new(30, 1)), 33_333_333);

        let (factory, props) = encoder_settings(&screen("L1", "DP-4"));
        assert_eq!(factory, "x264enc");
        assert_eq!(
            props,
            vec![
                ("pass", "qual".to_owned()),
                ("quantizer", "18".to_owned()),
                ("speed-preset", "veryfast".to_owned()),
                ("tune", "zerolatency".to_owned()),
                ("key-int-max", "30".to_owned()),
            ]
        );
        let mut va = screen("L1", "DP-4");
        va.encoder = VideoEncoder::Va {
            factory: "varenderD129h264enc".into(),
            qp: 18,
        };
        let (factory, props) = encoder_settings(&va);
        assert_eq!(factory, "varenderD129h264enc");
        assert_eq!(props[0], ("rate-control", "cqp".to_owned()));
        assert_eq!(props[3], ("qpb", "18".to_owned()));
        assert_eq!(encoder_input_format(&va.encoder), "NV12");
        assert_eq!(encoder_input_format(&x264()), "I420");

        let tone = FakeTone {
            channels: 1,
            freq_hz: 440,
            tone_ms: 500,
            period_ms: 1500,
        };
        assert_eq!(tick_settings(&tone), (220, 1_500_000_000));
    }
}
