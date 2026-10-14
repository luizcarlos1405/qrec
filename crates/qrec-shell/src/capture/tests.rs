//! Engine tests with the fake backend: real GStreamer pipelines fed by
//! `videotestsrc`/`audiotestsrc`, files checked with ffprobe. Never touches
//! the portal, cameras, PulseAudio or a GPU (x264 only).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use qrec_core::ctx::CameraMode;
use qrec_core::model::{
    AudioRole, AudioTrack, Canvas, Crop, Fit, Layer, Rect, Scene, VideoSource,
};
use qrec_core::plans::{AudioRecord, CapturePlan, LiveScene, VideoEncoder, VideoRecord};
use qrec_core::protocol::RecordedStream;
use qrec_core::{Event, Fps, TakeId};
use serde_json::Value;

use super::fake::{FAKE_CAMERA, FAKE_MIC, FAKE_MONITOR};
use super::{Backend, EngineOptions, FakeFailure, GstCaptureEngine, SideEvent};
use crate::deps::CaptureEngine;
use crate::msg::{Inbox, Msg};
use crate::paths::UserDirs;
use crate::testkit::{TestEnv, ffprobe, run_tool};

/// Recording tests run one at a time: they're CPU-bound (x264) and their
/// timing assertions shouldn't fight each other.
static SERIAL: Mutex<()> = Mutex::new(());

const WAIT: Duration = Duration::from_secs(20);

struct Harness {
    _env: TestEnv,
    project: PathBuf,
    engine: GstCaptureEngine,
    inbox: Inbox,
    rx: Receiver<Msg>,
}

impl Harness {
    fn new() -> Self {
        Self::with_side(None)
    }

    fn with_side(side: Option<crossbeam_channel::Sender<SideEvent>>) -> Self {
        static DIAG: std::sync::Once = std::sync::Once::new();
        DIAG.call_once(|| {
            let printed = std::sync::atomic::AtomicBool::new(false);
            gst::glib::log_set_handler(
                Some("GStreamer"),
                gst::glib::LogLevels::LEVEL_CRITICAL,
                false,
                false,
                move |_, _, msg| {
                    if !printed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        eprintln!(
                            "DIAG CRITICAL {msg} on {:?}\n{}",
                            std::thread::current().name(),
                            std::backtrace::Backtrace::force_capture()
                        );
                    }
                },
            );
        });
        let env = TestEnv::new();
        let project = env.empty_dir("swamp");
        fs::create_dir(project.join("takes")).unwrap();
        let dirs = UserDirs::resolve(&env.paths_env()).unwrap();
        let mut opts = EngineOptions::new(&project, Backend::Fake, &dirs);
        opts.register_app_id = false;
        opts.side = side;
        let engine = GstCaptureEngine::spawn(opts).unwrap();
        let (inbox, rx) = Inbox::channel();
        Self {
            _env: env,
            project,
            engine,
            inbox,
            rx,
        }
    }

    /// `takes/take-NNNN/` (the core's `CreateTakeDir`).
    fn take_dir(&self, n: u32) -> String {
        let rel = format!("takes/{}", TakeId::numbered(n));
        fs::create_dir(self.project.join(&rel)).unwrap();
        rel
    }

    fn start(&mut self, plan: &CapturePlan) {
        self.engine.start(plan.clone(), self.inbox.clone());
    }

    /// The next event matching `pred`, skipping others (returned in order).
    fn wait_for(&self, what: &str, pred: impl Fn(&Event) -> bool) -> (Event, Vec<Event>) {
        let deadline = Instant::now() + WAIT;
        let mut skipped = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(Msg::Event(ev)) if pred(&ev) => return (ev, skipped),
                Ok(Msg::Event(ev)) => skipped.push(ev),
                Ok(other) => panic!("unexpected message {other:?}"),
                Err(_) => panic!("no {what} within {WAIT:?}; got {skipped:?}"),
            }
        }
    }

    fn started(&self) -> Event {
        let (ev, skipped) = self.wait_for("CaptureStarted", |e| {
            matches!(e, Event::CaptureStarted { .. } | Event::CaptureFailed { .. })
        });
        assert!(
            matches!(ev, Event::CaptureStarted { .. }),
            "{ev:?} (before: {skipped:?})"
        );
        ev
    }

    fn stopped(&self) -> (Vec<RecordedStream>, bool, Vec<Event>) {
        let (ev, skipped) = self.wait_for("CaptureStopped", |e| {
            matches!(e, Event::CaptureStopped { .. })
        });
        match ev {
            Event::CaptureStopped {
                streams, timed_out, ..
            } => (streams, timed_out, skipped),
            _ => unreachable!(),
        }
    }

    fn file(&self, rel: &str) -> PathBuf {
        self.project.join(rel)
    }
}

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
        id: id.parse().unwrap(),
        source: VideoSource::Screen {
            output: output.into(),
            label: format!("Fake {output}"),
        },
        file: format!("{id}-screen-{output}.mkv"),
        encoder: x264(),
        key_int_max: 30,
        camera_mode: None,
    }
}

fn camera(id: &str, device: &str) -> VideoRecord {
    VideoRecord {
        id: id.parse().unwrap(),
        source: VideoSource::Camera {
            device: device.into(),
            label: "Fake camera".into(),
        },
        file: format!("{id}-camera-0.mkv"),
        encoder: x264(),
        key_int_max: 30,
        camera_mode: Some(CameraMode {
            width: 640,
            height: 480,
            fps: Fps::new(30, 1),
            format: "YUY2".into(),
        }),
    }
}

fn audio(id: &str, device: &str, role: &str) -> AudioRecord {
    AudioRecord {
        id: id.parse().unwrap(),
        device: device.into(),
        file: format!("{id}-{role}.mka"),
        sample_rate: 48_000,
    }
}

fn plan(n: u32, dir: &str, video: Vec<VideoRecord>, audio: Vec<AudioRecord>) -> CapturePlan {
    CapturePlan {
        take: TakeId::numbered(n),
        dir: dir.into(),
        log: format!("{dir}/recorder.log"),
        fps: Fps::new(30, 1),
        health_check_ms: 500,
        first_buffer_ms: 3000,
        eos_timeout_ms: 10_000,
        video,
        audio,
    }
}

fn stream_of(probe: &Value) -> &Value {
    &probe["streams"][0]
}

fn secs(v: &Value) -> f64 {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or(f64::NAN)
}

/// Video packet timestamps (s) of a file.
fn packet_times(file: &Path) -> Vec<f64> {
    let out = run_tool(
        Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time"])
            .args(["-of", "csv=p=0"])
            .arg(format!("file:{}", file.display())),
        Duration::from_secs(30),
    );
    assert!(out.status.success(), "{}", out.stderr);
    let mut times: Vec<f64> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').parse().ok())
        .collect();
    times.sort_by(f64::total_cmp);
    times
}

/// Checks a recorded video file: H.264, CFR at 30 fps, BT.709 limited
/// range, starting at 0; returns (frames, duration in s).
fn check_video(file: &Path, width: u64, height: u64) -> (u64, f64) {
    let probe = ffprobe(file);
    let v = stream_of(&probe);
    assert_eq!(v["codec_name"], "h264", "{probe}");
    assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(width), Some(height)));
    assert_eq!(v["r_frame_rate"], "30/1", "{probe}");
    assert_eq!(v["color_space"], "bt709", "{probe}");
    assert_eq!(v["color_range"], "tv", "{probe}");
    let start = secs(&probe["format"]["start_time"]);
    assert!(start.abs() < 0.001, "start_time {start}");
    let frames: u64 = v["nb_read_packets"].as_str().unwrap().parse().unwrap();
    let times = packet_times(file);
    assert_eq!(times.len() as u64, frames);
    assert!(times[0].abs() < 0.001, "first pts {}", times[0]);
    // Constant frame rate: every step is one frame (MKV stores ms).
    for pair in times.windows(2) {
        let step = pair[1] - pair[0];
        assert!((step - 1.0 / 30.0).abs() < 0.0015, "VFR step {step} in {file:?}");
    }
    let duration = secs(&probe["format"]["duration"]);
    (frames, duration)
}

/// Checks a recorded audio file: FLAC at 48 kHz starting at 0.
fn check_audio(file: &Path, channels: u64) -> f64 {
    let probe = ffprobe(file);
    let a = stream_of(&probe);
    assert_eq!(a["codec_name"], "flac", "{probe}");
    assert_eq!(a["sample_rate"], "48000");
    assert_eq!(a["channels"].as_u64(), Some(channels));
    let start = secs(&probe["format"]["start_time"]);
    assert!(start.abs() < 0.001, "start_time {start} of {}: {probe}", file.display());
    secs(&probe["format"]["duration"])
}

#[test]
fn records_screens_cameras_and_audio_in_sync() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut h = Harness::new();
    let dir = h.take_dir(1);
    let plan = plan(
        1,
        &dir,
        vec![screen("L1", "FAKE-1"), camera("L2", FAKE_CAMERA)],
        vec![audio("A1", FAKE_MIC, "mic"), audio("A2", FAKE_MONITOR, "system")],
    );
    let started_at = Instant::now();
    h.start(&plan);
    let Event::CaptureStarted { take, files, .. } = h.started() else {
        unreachable!()
    };
    // The health window passed before CaptureStarted.
    assert!(started_at.elapsed() >= Duration::from_millis(500));
    assert_eq!(take, TakeId::numbered(1));
    assert_eq!(
        files,
        [
            "takes/take-0001/L1-screen-FAKE-1.mkv",
            "takes/take-0001/L2-camera-0.mkv",
            "takes/take-0001/A1-mic.mka",
            "takes/take-0001/A2-system.mka",
        ]
    );

    std::thread::sleep(Duration::from_millis(2500));
    h.engine.stop(&take, false);
    let (streams, timed_out, skipped) = h.stopped();
    assert!(!timed_out);

    // RecordClock ~4 Hz from the pipeline clock, increasing.
    let clocks: Vec<u64> = skipped
        .iter()
        .filter_map(|e| match e {
            Event::RecordClock { elapsed_ns, .. } => Some(*elapsed_ns),
            _ => None,
        })
        .collect();
    assert!(clocks.len() >= 8, "{clocks:?}");
    assert!(clocks.windows(2).all(|w| w[0] <= w[1]), "{clocks:?}");
    assert!(!skipped.iter().any(|e| matches!(e, Event::StreamLost { .. })), "{skipped:?}");

    let ids: Vec<&str> = streams.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["L1", "L2", "A1", "A2"]);
    for s in &streams {
        assert!(s.eos, "{s:?}");
        // The first buffer after T0: within a frame or an audio buffer.
        assert!((0..100_000_000).contains(&s.start_offset_ns), "{s:?}");
        assert_eq!(s.file, format!("takes/take-0001/{}", s.file.rsplit('/').next().unwrap()));
    }

    let (screen_frames, screen_dur) = check_video(&h.file(&streams[0].file), 1280, 720);
    let (camera_frames, camera_dur) = check_video(&h.file(&streams[1].file), 640, 480);
    let mic_dur = check_audio(&h.file(&streams[2].file), 1);
    let system_dur = check_audio(&h.file(&streams[3].file), 2);
    let last_clock = *clocks.last().unwrap() as f64 / 1e9;
    for (frames, duration) in [(screen_frames, screen_dur), (camera_frames, camera_dur)] {
        // CFR: frames ≈ duration × fps, and the take is as long as recorded.
        assert!(
            (frames as f64 - duration * 30.0).abs() <= 2.0,
            "{frames} frames in {duration} s"
        );
        assert!(duration >= last_clock - 0.1 && duration < last_clock + 1.0, "{duration} vs {last_clock}");
    }
    // Every stream covers the same span (one clock, filled to T1).
    for duration in [camera_dur, mic_dur, system_dur] {
        assert!((duration - screen_dur).abs() < 0.12, "{duration} vs {screen_dur}");
    }
    let offsets: Vec<i64> = streams.iter().map(|s| s.start_offset_ns).collect();
    let spread = offsets.iter().max().unwrap() - offsets.iter().min().unwrap();
    assert!(spread < 70_000_000, "start offsets {offsets:?}");

    let log = fs::read_to_string(h.file("takes/take-0001/recorder.log")).unwrap();
    for needle in ["attached at T0", "recording", "EOS written", "startOffsetNs", "stopped"] {
        assert!(log.contains(needle), "{needle:?} missing from:\n{log}");
    }
}

#[test]
fn fake_mic_has_periodic_silence() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut h = Harness::new();
    let dir = h.take_dir(1);
    let plan = plan(1, &dir, vec![], vec![audio("A1", FAKE_MIC, "mic")]);
    h.start(&plan);
    h.started();
    std::thread::sleep(Duration::from_millis(3200));
    h.engine.stop(&TakeId::numbered(1), false);
    let (streams, timed_out, _) = h.stopped();
    assert!(!timed_out && streams[0].eos);
    let file = h.file(&streams[0].file);
    let silences = crate::testkit::detect_silences(&file, -35.0, 400);
    // 0.5 s tone + 1 s silence, repeated: ≥ 2 silences in ~3 s.
    assert!(silences.len() >= 2, "{silences:?}");
    for (start, end) in &silences[..silences.len() - 1] {
        assert!((end - start - 1.0).abs() < 0.1, "{silences:?}");
    }
}

#[test]
fn stop_and_cancel_while_starting() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut h = Harness::new();
    let dir = h.take_dir(1);
    let first = plan(1, &dir, vec![screen("L1", "FAKE-2")], vec![audio("A1", FAKE_MIC, "mic")]);
    h.start(&first);
    // Cancel right away: no CaptureStarted, but always CaptureStopped.
    h.engine.stop(&first.take, true);
    let (streams, timed_out, skipped) = h.stopped();
    assert!(!timed_out, "{skipped:?}");
    assert!(
        !skipped.iter().any(|e| matches!(e, Event::CaptureFailed { .. })),
        "{skipped:?}"
    );
    assert_eq!(streams.len(), 2);
    assert!(streams.iter().all(|s| s.eos), "{streams:?}");

    // The engine records the next take normally.
    let dir = h.take_dir(2);
    let second = plan(2, &dir, vec![screen("L1", "FAKE-2")], vec![]);
    h.start(&second);
    h.started();
    std::thread::sleep(Duration::from_millis(1200));
    h.engine.stop(&second.take, false);
    let (streams, timed_out, _) = h.stopped();
    assert!(!timed_out && streams[0].eos);
    let (frames, duration) = check_video(&h.file(&streams[0].file), 1280, 720);
    assert!(frames >= 30, "{frames} frames in {duration} s");
}

#[test]
fn stop_of_an_unknown_take_still_answers() {
    let mut h = Harness::new();
    h.engine.set_live(None, h.inbox.clone());
    h.engine.stop(&TakeId::numbered(99), false);
    let (streams, timed_out, _) = h.stopped();
    assert!(streams.is_empty() && !timed_out);
}

#[test]
fn start_failures_name_the_source() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut h = Harness::new();

    // A device the fake backend doesn't have.
    let dir = h.take_dir(1);
    let plan1 = plan(1, &dir, vec![camera("L2", "/dev/video0")], vec![]);
    h.start(&plan1);
    let (ev, _) = h.wait_for("CaptureFailed", |e| matches!(e, Event::CaptureFailed { .. }));
    let Event::CaptureFailed { take, source, message } = ev else { unreachable!() };
    assert_eq!(take, Some(TakeId::numbered(1)));
    assert!(source.as_deref().unwrap().contains("/dev/video0"), "{source:?}");
    assert!(message.contains("no fake camera"), "{message}");

    // Never overwrite: an existing target file fails the start untouched.
    let dir = h.take_dir(2);
    fs::write(h.file(&format!("{dir}/A1-mic.mka")), b"precious").unwrap();
    let plan2 = plan(2, &dir, vec![], vec![audio("A1", FAKE_MIC, "mic")]);
    h.start(&plan2);
    let (ev, _) = h.wait_for("CaptureFailed", |e| matches!(e, Event::CaptureFailed { .. }));
    let Event::CaptureFailed { source, message, .. } = ev else { unreachable!() };
    assert!(message.contains("already exists"), "{message}");
    assert_eq!(source.as_deref(), Some("A1 audio alsa_input.fake"));
    assert_eq!(fs::read(h.file(&format!("{dir}/A1-mic.mka"))).unwrap(), b"precious");

    // A missing take folder.
    let plan3 = plan(3, "takes/take-0003", vec![], vec![audio("A1", FAKE_MIC, "mic")]);
    h.start(&plan3);
    let (ev, _) = h.wait_for("CaptureFailed", |e| matches!(e, Event::CaptureFailed { .. }));
    assert!(format!("{ev:?}").contains("is missing"), "{ev:?}");

    // A plan with an encoder setting GStreamer rejects.
    let dir = h.take_dir(4);
    let mut bad = screen("L1", "FAKE-1");
    bad.encoder = VideoEncoder::X264 {
        pass: "qual".into(),
        quantizer: 18,
        speed_preset: "ludicrous".into(),
        tune: "zerolatency".into(),
    };
    let plan4 = plan(4, &dir, vec![bad], vec![]);
    h.start(&plan4);
    let (ev, _) = h.wait_for("CaptureFailed", |e| matches!(e, Event::CaptureFailed { .. }));
    let Event::CaptureFailed { source, message, .. } = ev else { unreachable!() };
    assert!(message.contains("speed-preset"), "{message}");
    assert!(source.unwrap().starts_with("L1 screen FAKE-1"));

    // The engine still works afterwards, and a late stop of a failed take
    // gets an answer.
    h.engine.stop(&TakeId::numbered(1), false);
    let (streams, _, _) = h.stopped();
    assert!(streams.is_empty());
    let dir = h.take_dir(5);
    let plan5 = plan(5, &dir, vec![], vec![audio("A1", FAKE_MIC, "mic")]);
    h.start(&plan5);
    h.started();
    h.engine.stop(&plan5.take, false);
    let (streams, timed_out, _) = h.stopped();
    assert!(!timed_out && streams[0].eos);
}

#[test]
fn a_lost_source_is_stream_lost_and_the_rest_keeps_recording() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut h = Harness::new();
    let dir = h.take_dir(1);
    let plan = plan(
        1,
        &dir,
        vec![screen("L1", "FAKE-1"), camera("L2", FAKE_CAMERA)],
        vec![audio("A1", FAKE_MIC, "mic"), audio("A2", FAKE_MONITOR, "system")],
    );
    h.start(&plan);
    h.started();
    std::thread::sleep(Duration::from_millis(800));

    // The camera errors (unplugged).
    h.engine.inject_fake_failure(FAKE_CAMERA, FakeFailure::Error);
    let (ev, _) = h.wait_for("StreamLost", |e| matches!(e, Event::StreamLost { .. }));
    let Event::StreamLost { take, stream, message } = ev else { unreachable!() };
    assert_eq!((take, stream.as_str()), (TakeId::numbered(1), "L2"));
    assert!(message.contains("unplugged"), "{message}");

    // The monitor's stream ends (EOS).
    h.engine.inject_fake_failure(FAKE_MONITOR, FakeFailure::Eos);
    let (ev, _) = h.wait_for("StreamLost", |e| matches!(e, Event::StreamLost { .. }));
    let Event::StreamLost { stream, message, .. } = ev else { unreachable!() };
    assert_eq!(stream, "A2");
    assert!(message.contains("stopped sending data"), "{message}");

    std::thread::sleep(Duration::from_millis(800));
    h.engine.stop(&plan.take, false);
    let (streams, timed_out, skipped) = h.stopped();
    assert!(!timed_out, "{skipped:?}");
    assert!(
        !skipped.iter().any(|e| matches!(e, Event::StreamLost { .. })),
        "each stream is lost once: {skipped:?}"
    );
    assert!(streams.iter().all(|s| s.eos), "{streams:?}");
    // The lost streams stopped early; the others ran to the end.
    let (_, screen_dur) = check_video(&h.file(&streams[0].file), 1280, 720);
    let (_, camera_dur) = check_video(&h.file(&streams[1].file), 640, 480);
    let mic_dur = check_audio(&h.file(&streams[2].file), 1);
    let system_dur = check_audio(&h.file(&streams[3].file), 2);
    assert!(screen_dur > camera_dur + 0.4, "{screen_dur} vs {camera_dur}");
    assert!(mic_dur > system_dur + 0.4, "{mic_dur} vs {system_dur}");
    assert!((screen_dur - mic_dur).abs() < 0.12);
}

#[test]
fn live_pipeline_previews_and_records() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (side_tx, side_rx) = crossbeam_channel::unbounded();
    let mut h = Harness::with_side(Some(side_tx));
    let layer = Layer {
        id: "L1".parse().unwrap(),
        name: "Screen FAKE-1".into(),
        visible: true,
        opacity: 1.0,
        source: VideoSource::Screen {
            output: "FAKE-1".into(),
            label: "Fake screen 1".into(),
        },
        rect: Rect::new(0, 0, 320, 180),
        crop: Crop::default(),
        fit: Fit::Contain,
        locked: false,
        missing: false,
    };
    let track = AudioTrack {
        id: "A1".parse().unwrap(),
        role: AudioRole::Mic,
        source: FAKE_MIC.into(),
        label: "Fake mic".into(),
        gain_db: 0.0,
        muted: false,
        sync_offset_ms: 0,
        missing: false,
    };
    let scene = LiveScene {
        canvas: Canvas {
            width: 320,
            height: 180,
            fps: Fps::new(30, 1),
            background: "#12170F".parse().unwrap(),
        },
        scene: Scene {
            layers: vec![layer.clone()],
            audio: vec![track],
        },
        preview: true,
    };
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());

    // Preview frames at canvas size and level readings arrive.
    let deadline = Instant::now() + WAIT;
    let (mut frame, mut level) = (None, None);
    while (frame.is_none() || level.is_none()) && Instant::now() < deadline {
        match side_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(SideEvent::Preview(f)) => frame = Some(f),
            Ok(SideEvent::Level(l)) => level = Some(l),
            Ok(SideEvent::LiveError(e)) => panic!("live error: {e}"),
            Err(_) => {}
        }
    }
    let frame = frame.expect("a preview frame");
    assert_eq!((frame.width, frame.height), (320, 180));
    assert_eq!(frame.rgba.len(), (frame.stride * frame.height) as usize);
    let level = level.expect("a level reading");
    assert_eq!(level.device, FAKE_MIC);
    assert_eq!(level.rms_db.len(), 1);

    // A layout change applies without errors.
    let mut moved = scene.clone();
    moved.scene.layers[0].rect = Rect::new(160, 90, 160, 90);
    moved.scene.layers[0].opacity = 0.5;
    h.engine.set_live(Some(moved), h.inbox.clone());

    // Recording uses the running pipeline.
    let dir = h.take_dir(1);
    let plan = plan(1, &dir, vec![screen("L1", "FAKE-1")], vec![audio("A1", FAKE_MIC, "mic")]);
    h.start(&plan);
    h.started();
    std::thread::sleep(Duration::from_millis(1500));
    h.engine.stop(&plan.take, false);
    let (streams, timed_out, _) = h.stopped();
    assert!(!timed_out && streams.iter().all(|s| s.eos));
    let (frames, _) = check_video(&h.file(&streams[0].file), 1280, 720);
    // skip-to-first: no frozen frames from the live time before the take.
    assert!((40..=75).contains(&frames), "{frames}");

    // The preview keeps running after the take.
    while side_rx.try_recv().is_ok() {}
    let after = side_rx.recv_timeout(Duration::from_secs(5));
    assert!(after.is_ok(), "the live pipeline stopped after the take");

    h.engine.set_live(None, h.inbox.clone());
}

#[test]
fn shutdown_finalizes_a_running_take() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut h = Harness::new();
    let dir = h.take_dir(1);
    let plan = plan(1, &dir, vec![screen("L1", "FAKE-1")], vec![audio("A1", FAKE_MIC, "mic")]);
    h.start(&plan);
    h.started();
    std::thread::sleep(Duration::from_millis(700));
    let began = Instant::now();
    h.engine.shutdown();
    assert!(began.elapsed() < Duration::from_secs(7));
    let (streams, timed_out, _) = h.stopped();
    assert!(!timed_out && streams.iter().all(|s| s.eos), "{streams:?}");
    check_video(&h.file(&streams[0].file), 1280, 720);
    // After shutdown, starts fail at once.
    let dir = h.take_dir(2);
    h.start(&plan_with_take(2, &dir));
    let (ev, _) = h.wait_for("CaptureFailed", |e| matches!(e, Event::CaptureFailed { .. }));
    assert!(format!("{ev:?}").contains("isn't running"), "{ev:?}");
}

fn plan_with_take(n: u32, dir: &str) -> CapturePlan {
    plan(n, dir, vec![], vec![audio("A1", FAKE_MIC, "mic")])
}

fn screen_layer(id: &str, output: &str, rect: Rect) -> Layer {
    Layer {
        id: id.parse().unwrap(),
        name: format!("Screen {output}"),
        visible: true,
        opacity: 1.0,
        source: VideoSource::Screen {
            output: output.into(),
            label: format!("Fake {output}"),
        },
        rect,
        crop: Crop::default(),
        fit: Fit::Contain,
        locked: false,
        missing: false,
    }
}

fn audio_track(id: &str, device: &str) -> AudioTrack {
    AudioTrack {
        id: id.parse().unwrap(),
        role: if device.ends_with(".monitor") { AudioRole::System } else { AudioRole::Mic },
        source: device.into(),
        label: device.into(),
        gain_db: 0.0,
        muted: false,
        sync_offset_ms: 0,
        missing: false,
    }
}

fn live_scene(layers: Vec<Layer>, audio: Vec<AudioTrack>, preview: bool) -> LiveScene {
    LiveScene {
        canvas: Canvas {
            width: 320,
            height: 180,
            fps: Fps::new(30, 1),
            background: "#000000".parse().unwrap(),
        },
        scene: Scene { layers, audio },
        preview,
    }
}

/// The core sends `SetLive` whenever the wanted scene changes, before
/// `StartCapture` (A§8.11): the engine builds the pipeline then, keeps it
/// for equal scenes and layout changes, and records from it.
#[test]
fn set_live_is_idempotent_and_the_take_reuses_its_pipeline() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let mut h = Harness::new();
    assert_eq!(h.engine.inspect().generation, None);

    // Headless recording: the scene without preview.
    let full = Rect::new(0, 0, 320, 180);
    let scene = live_scene(
        vec![screen_layer("L1", "FAKE-1", full)],
        vec![audio_track("A1", FAKE_MIC)],
        false,
    );
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    let built = h.engine.inspect();
    let generation = built.generation.expect("SetLive builds the pipeline");
    assert!(!built.preview);
    assert_eq!(built.sources.len(), 2, "{built:?}");

    // The same scene again, or only a new layout: nothing is rebuilt.
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    let mut moved = scene.clone();
    moved.scene.layers[0].rect = Rect::new(10, 10, 100, 60);
    h.engine.set_live(Some(moved.clone()), h.inbox.clone());
    assert_eq!(h.engine.inspect().generation, Some(generation));

    // The take attaches to the running pipeline.
    let dir = h.take_dir(1);
    let take = plan(1, &dir, vec![screen("L1", "FAKE-1")], vec![audio("A1", FAKE_MIC, "mic")]);
    h.start(&take);
    h.started();
    assert_eq!(h.engine.inspect().generation, Some(generation));
    // While recording, an equal scene is still a no-op.
    h.engine.set_live(Some(moved.clone()), h.inbox.clone());
    std::thread::sleep(Duration::from_millis(600));
    h.engine.stop(&take.take, false);
    let (streams, timed_out, skipped) = h.stopped();
    assert!(!timed_out && streams.iter().all(|s| s.eos), "{streams:?} {skipped:?}");
    assert_eq!(h.engine.inspect().generation, Some(generation), "kept between takes");

    // New sources rebuild; no scene tears it down.
    let mut more = moved.clone();
    more.scene.audio.push(audio_track("A2", FAKE_MONITOR));
    h.engine.set_live(Some(more), h.inbox.clone());
    let rebuilt = h.engine.inspect();
    assert!(rebuilt.generation > Some(generation));
    assert_eq!(rebuilt.sources.len(), 3);
    h.engine.set_live(None, h.inbox.clone());
    assert_eq!(h.engine.inspect().generation, None);
    h.engine.set_live(None, h.inbox.clone());
    assert_eq!(h.engine.inspect().generation, None);
}
