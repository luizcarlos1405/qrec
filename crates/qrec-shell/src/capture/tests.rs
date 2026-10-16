//! Engine tests with the fake backend: real GStreamer pipelines fed by
//! `videotestsrc`/`audiotestsrc`, files checked with ffprobe. Never touches
//! the portal, cameras, PulseAudio or a GPU (x264 only). The GUI's parts
//! (docs/GUI.md G§3.9) too: an appsink stands in for its paintable sink.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use gst::prelude::*;
use qrec_core::ctx::CameraMode;
use qrec_core::media_argv::probe_argv;
use qrec_core::model::{
    AudioRole, AudioTrack, Canvas, Crop, Fit, Layer, Rect, Scene, StreamKind, VideoSource,
};
use qrec_core::parse::ffprobe::parse_ffprobe;
use qrec_core::plans::{AudioRecord, CapturePlan, LiveScene, VideoEncoder, VideoRecord};
use qrec_core::protocol::{MediaInfo, RecordedStream};
use qrec_core::{Event, Fps, TakeId};
use serde_json::Value;

use super::fake::{FAKE_CAMERA, FAKE_MIC, FAKE_MONITOR};
use super::meters::MeterMonitor;
use super::spec::SourceSpec;
use super::{
    Backend, EngineOptions, FakeFailure, FakeHooks, FakePortal, GstCaptureEngine, LiveProgress,
    LiveSourceState, LiveSourceStatus, PreviewOutput, STALL_LIMIT, SideEvent,
};
use crate::deps::CaptureEngine;
use crate::gst_init::testing::{assert_quiet_since, problems, serial};
use crate::msg::{Inbox, Msg};
use crate::paths::UserDirs;
use crate::testkit::{TestEnv, ffprobe, run_tool};

const WAIT: Duration = Duration::from_secs(20);

/// An engine with the fake backend in a temp project. Tests that record
/// take [`serial`] first; dropping the harness shuts the engine down and
/// fails the test if GStreamer printed a critical or a warning meanwhile.
struct Harness {
    _env: TestEnv,
    project: PathBuf,
    engine: GstCaptureEngine,
    inbox: Inbox,
    rx: Receiver<Msg>,
    problems_before: u64,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.engine.shutdown();
        if !std::thread::panicking() {
            assert_quiet_since(self.problems_before);
        }
    }
}

impl Harness {
    fn new() -> Self {
        Self::with_side(None)
    }

    fn with_side(side: Option<Sender<SideEvent>>) -> Self {
        Self::custom(side, |_| {})
    }

    /// An engine with `side` and whatever else `configure` sets (the GUI's
    /// preview sink, a cast grace, fake hooks).
    fn custom(side: Option<Sender<SideEvent>>, configure: impl FnOnce(&mut EngineOptions)) -> Self {
        let env = TestEnv::new();
        let project = env.empty_dir("swamp");
        fs::create_dir(project.join("takes")).unwrap();
        let dirs = UserDirs::resolve(&env.paths_env()).unwrap();
        let mut opts = EngineOptions::new(&project, Backend::Fake, &dirs);
        opts.register_app_id = false;
        opts.side = side;
        configure(&mut opts);
        let engine = GstCaptureEngine::spawn(opts).unwrap();
        let (inbox, rx) = Inbox::channel();
        Self {
            _env: env,
            project,
            engine,
            inbox,
            rx,
            problems_before: problems(),
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
            matches!(
                e,
                Event::CaptureStarted { .. } | Event::CaptureFailed { .. }
            )
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
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "packet=pts_time",
            ])
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
    assert_eq!(
        (v["width"].as_u64(), v["height"].as_u64()),
        (Some(width), Some(height))
    );
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
        assert!(
            (step - 1.0 / 30.0).abs() < 0.0015,
            "VFR step {step} in {file:?}"
        );
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
    assert!(
        start.abs() < 0.001,
        "start_time {start} of {}: {probe}",
        file.display()
    );
    secs(&probe["format"]["duration"])
}

#[test]
fn records_screens_cameras_and_audio_in_sync() {
    let _serial = serial();
    let mut h = Harness::new();
    let dir = h.take_dir(1);
    let plan = plan(
        1,
        &dir,
        vec![screen("L1", "FAKE-1"), camera("L2", FAKE_CAMERA)],
        vec![
            audio("A1", FAKE_MIC, "mic"),
            audio("A2", FAKE_MONITOR, "system"),
        ],
    );
    let started_at = Instant::now();
    h.start(&plan);
    let Event::CaptureStarted {
        take,
        files,
        start_offset_ns,
        ..
    } = h.started()
    else {
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
    assert!(
        !skipped
            .iter()
            .any(|e| matches!(e, Event::StreamLost { .. })),
        "{skipped:?}"
    );

    let ids: Vec<&str> = streams.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["L1", "L2", "A1", "A2"]);
    for s in &streams {
        assert!(s.eos, "{s:?}");
        // The first buffer after T0: within a frame or an audio buffer.
        assert!((0..100_000_000).contains(&s.start_offset_ns), "{s:?}");
        assert_eq!(
            s.file,
            format!("takes/take-0001/{}", s.file.rsplit('/').next().unwrap())
        );
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
        assert!(
            duration >= last_clock - 0.1 && duration < last_clock + 1.0,
            "{duration} vs {last_clock}"
        );
    }
    // Every stream covers the same span (one clock, filled to T1).
    for duration in [camera_dur, mic_dur, system_dur] {
        assert!(
            (duration - screen_dur).abs() < 0.12,
            "{duration} vs {screen_dur}"
        );
    }
    let offsets: Vec<i64> = streams.iter().map(|s| s.start_offset_ns).collect();
    let spread = offsets.iter().max().unwrap() - offsets.iter().min().unwrap();
    assert!(spread < 70_000_000, "start offsets {offsets:?}");
    // The offsets were already known (and reported) when the take started,
    // so a take recovered after a crash keeps them.
    let at_stop: std::collections::BTreeMap<String, i64> = streams
        .iter()
        .map(|s| (s.id.clone(), s.start_offset_ns))
        .collect();
    assert_eq!(start_offset_ns, at_stop);

    let log = fs::read_to_string(h.file("takes/take-0001/recorder.log")).unwrap();
    for needle in [
        "attached at T0",
        "recording",
        "EOS written",
        "startOffsetNs",
        "stopped",
    ] {
        assert!(log.contains(needle), "{needle:?} missing from:\n{log}");
    }
    // FAKE-1 is still: it sent one frame before T0, and the screen bridge's
    // keepalive sent all the others, up to T1 (A§8.11), so closing its bin
    // filled no more than a frame or two.
    let closed = log
        .lines()
        .find(|l| l.contains("L1: closed after"))
        .unwrap_or_else(|| panic!("no close line for L1 in:\n{log}"));
    let filled: u64 = closed
        .split(", ")
        .find_map(|part| part.strip_suffix(" fill frames"))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{closed}"));
    assert!(filled <= 2, "{closed}");
}

#[test]
fn fake_mic_has_periodic_silence() {
    let _serial = serial();
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

/// Probes a take file the way finalization does: the core's argv and parser.
fn probe(h: &Harness, file: &str, kind: StreamKind) -> Result<MediaInfo, String> {
    let argv = probe_argv(file, kind);
    let out = run_tool(
        Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&h.project),
        Duration::from_secs(30),
    );
    if !out.status.success() {
        return Err(format!("ffprobe failed: {}", out.stderr));
    }
    parse_ffprobe(&String::from_utf8_lossy(&out.stdout), kind)
}

#[test]
fn stop_and_cancel_while_starting() {
    // A quick double-tap (or Esc) stops the take right after its bins are
    // attached, before any buffer reached them: every file must still be
    // readable, so the take ends up tooShort (or cancelled), never broken.
    let _serial = serial();
    let mut h = Harness::new();
    for (n, cancel) in [(1u32, true), (2, false)] {
        let dir = h.take_dir(n);
        let take = plan(
            n,
            &dir,
            vec![screen("L1", "FAKE-2"), camera("L2", FAKE_CAMERA)],
            vec![audio("A1", FAKE_MIC, "mic")],
        );
        h.start(&take);
        // No CaptureStarted, but always CaptureStopped.
        h.engine.stop(&take.take, cancel);
        let (streams, timed_out, skipped) = h.stopped();
        assert!(!timed_out, "take {n}: {skipped:?}");
        assert!(
            !skipped.iter().any(|e| matches!(
                e,
                Event::CaptureFailed { .. } | Event::CaptureStarted { .. }
            )),
            "take {n}: {skipped:?}"
        );
        let ids: Vec<&str> = streams.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["L1", "L2", "A1"], "take {n}");
        for s in &streams {
            assert!(s.eos, "take {n}: {s:?}");
            assert!((0..100_000_000).contains(&s.start_offset_ns), "{s:?}");
            let kind = if s.id.starts_with('A') {
                StreamKind::Audio
            } else {
                StreamKind::Video
            };
            let info = probe(&h, &s.file, kind)
                .unwrap_or_else(|e| panic!("take {n}: {} is unreadable: {e}", s.file));
            match kind {
                // One frame: the take is too short, not broken.
                StreamKind::Video => assert!(info.frames >= Some(1), "{info:?}"),
                // One buffer, padded to one FLAC block.
                StreamKind::Audio => {
                    assert!(info.duration_ns >= Some(96_000_000), "{info:?}")
                }
            }
        }
        let log = fs::read_to_string(h.file(&format!("{dir}/recorder.log"))).unwrap();
        assert!(!log.contains("closed after 0 buffers"), "{log}");
    }

    // The engine records the next take normally.
    let dir = h.take_dir(3);
    let third = plan(3, &dir, vec![screen("L1", "FAKE-2")], vec![]);
    h.start(&third);
    h.started();
    std::thread::sleep(Duration::from_millis(1200));
    h.engine.stop(&third.take, false);
    let (streams, timed_out, _) = h.stopped();
    assert!(!timed_out && streams[0].eos);
    let (frames, duration) = check_video(&h.file(&streams[0].file), 1280, 720);
    assert!(frames >= 30, "{frames} frames in {duration} s");
}

#[test]
fn stopping_within_the_first_audio_block_is_quiet() {
    // flacenc sets its output caps with its first block (4608 samples at
    // 48 kHz, 96 ms): a stream that ended sooner made GStreamer warn
    // ("got 'segment' before 'caps'"). Stop at several moments of that window.
    let _serial = serial();
    let mut h = Harness::new();
    for (n, delay_ms) in [(1u32, 15u64), (2, 35), (3, 55), (4, 75), (5, 95)] {
        let dir = h.take_dir(n);
        let take = plan(n, &dir, vec![], vec![audio("A1", FAKE_MIC, "mic")]);
        h.start(&take);
        std::thread::sleep(Duration::from_millis(delay_ms));
        h.engine.stop(&take.take, false);
        let (streams, timed_out, _) = h.stopped();
        assert!(!timed_out, "take {n}");
        assert!(streams.iter().all(|s| s.eos), "take {n}: {streams:?}");
    }
}

#[test]
fn stop_of_an_unknown_take_still_answers() {
    let _serial = serial();
    let mut h = Harness::new();
    h.engine.set_live(None, h.inbox.clone());
    h.engine.stop(&TakeId::numbered(99), false);
    let (streams, timed_out, _) = h.stopped();
    assert!(streams.is_empty() && !timed_out);
}

#[test]
fn start_failures_name_the_source() {
    let _serial = serial();
    let mut h = Harness::new();

    // A device the fake backend doesn't have.
    let dir = h.take_dir(1);
    let plan1 = plan(1, &dir, vec![camera("L2", "/dev/video0")], vec![]);
    h.start(&plan1);
    let (ev, _) = h.wait_for("CaptureFailed", |e| {
        matches!(e, Event::CaptureFailed { .. })
    });
    let Event::CaptureFailed {
        take,
        source,
        message,
        missing,
    } = ev
    else {
        unreachable!()
    };
    assert_eq!(take, Some(TakeId::numbered(1)));
    assert!(
        source.as_deref().unwrap().contains("/dev/video0"),
        "{source:?}"
    );
    assert!(message.contains("no fake camera"), "{message}");
    assert_eq!(missing, None, "nothing is missing from GStreamer");

    // Never overwrite: an existing target file fails the start untouched.
    let dir = h.take_dir(2);
    fs::write(h.file(&format!("{dir}/A1-mic.mka")), b"precious").unwrap();
    let plan2 = plan(2, &dir, vec![], vec![audio("A1", FAKE_MIC, "mic")]);
    h.start(&plan2);
    let (ev, _) = h.wait_for("CaptureFailed", |e| {
        matches!(e, Event::CaptureFailed { .. })
    });
    let Event::CaptureFailed {
        source, message, ..
    } = ev
    else {
        unreachable!()
    };
    assert!(message.contains("already exists"), "{message}");
    assert_eq!(source.as_deref(), Some("A1 audio alsa_input.fake"));
    assert_eq!(
        fs::read(h.file(&format!("{dir}/A1-mic.mka"))).unwrap(),
        b"precious"
    );

    // A missing take folder.
    let plan3 = plan(
        3,
        "takes/take-0003",
        vec![],
        vec![audio("A1", FAKE_MIC, "mic")],
    );
    h.start(&plan3);
    let (ev, _) = h.wait_for("CaptureFailed", |e| {
        matches!(e, Event::CaptureFailed { .. })
    });
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
    let (ev, _) = h.wait_for("CaptureFailed", |e| {
        matches!(e, Event::CaptureFailed { .. })
    });
    let Event::CaptureFailed {
        source, message, ..
    } = ev
    else {
        unreachable!()
    };
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

    // An element that isn't installed is named, so the core answers
    // missing_dependency (exit 5) instead of capture_failed.
    let dir = h.take_dir(6);
    let mut va = screen("L1", "FAKE-1");
    va.encoder = VideoEncoder::Va {
        factory: "qrecnosuchh264enc".into(),
        qp: 18,
    };
    h.start(&plan(6, &dir, vec![va], vec![]));
    let (ev, _) = h.wait_for("CaptureFailed", |e| {
        matches!(e, Event::CaptureFailed { .. })
    });
    let Event::CaptureFailed {
        message, missing, ..
    } = ev
    else {
        unreachable!()
    };
    assert!(
        message.contains("qrecnosuchh264enc is missing"),
        "{message}"
    );
    assert_eq!(missing.as_deref(), Some("qrecnosuchh264enc"));
}

#[test]
fn a_lost_source_is_stream_lost_and_the_rest_keeps_recording() {
    let _serial = serial();
    let mut h = Harness::new();
    let dir = h.take_dir(1);
    let plan = plan(
        1,
        &dir,
        vec![screen("L1", "FAKE-1"), camera("L2", FAKE_CAMERA)],
        vec![
            audio("A1", FAKE_MIC, "mic"),
            audio("A2", FAKE_MONITOR, "system"),
        ],
    );
    h.start(&plan);
    h.started();
    std::thread::sleep(Duration::from_millis(800));

    // The camera errors (unplugged).
    h.engine
        .inject_fake_failure(FAKE_CAMERA, FakeFailure::Error);
    let (ev, _) = h.wait_for("StreamLost", |e| matches!(e, Event::StreamLost { .. }));
    let Event::StreamLost {
        take,
        stream,
        message,
    } = ev
    else {
        unreachable!()
    };
    assert_eq!((take, stream.as_str()), (TakeId::numbered(1), "L2"));
    assert!(message.contains("unplugged"), "{message}");

    // The monitor's stream ends (EOS).
    h.engine.inject_fake_failure(FAKE_MONITOR, FakeFailure::Eos);
    let (ev, _) = h.wait_for("StreamLost", |e| matches!(e, Event::StreamLost { .. }));
    let Event::StreamLost {
        stream, message, ..
    } = ev
    else {
        unreachable!()
    };
    assert_eq!(stream, "A2");
    assert!(message.contains("stopped sending data"), "{message}");

    std::thread::sleep(Duration::from_millis(800));
    h.engine.stop(&plan.take, false);
    let (streams, timed_out, skipped) = h.stopped();
    assert!(!timed_out, "{skipped:?}");
    assert!(
        !skipped
            .iter()
            .any(|e| matches!(e, Event::StreamLost { .. })),
        "each stream is lost once: {skipped:?}"
    );
    assert!(streams.iter().all(|s| s.eos), "{streams:?}");
    // The lost streams stopped early; the others ran to the end.
    let (_, screen_dur) = check_video(&h.file(&streams[0].file), 1280, 720);
    let (_, camera_dur) = check_video(&h.file(&streams[1].file), 640, 480);
    let mic_dur = check_audio(&h.file(&streams[2].file), 1);
    let system_dur = check_audio(&h.file(&streams[3].file), 2);
    assert!(
        screen_dur > camera_dur + 0.4,
        "{screen_dur} vs {camera_dur}"
    );
    assert!(mic_dur > system_dur + 0.4, "{mic_dur} vs {system_dur}");
    assert!((screen_dur - mic_dur).abs() < 0.12);
}

#[test]
fn a_camera_that_hangs_is_lost_instead_of_filled_at_the_stop() {
    let _serial = serial();
    let mut h = Harness::new();
    let dir = h.take_dir(1);
    let plan = plan(
        1,
        &dir,
        vec![screen("L1", "FAKE-1"), camera("L2", FAKE_CAMERA)],
        vec![audio("A1", FAKE_MIC, "mic")],
    );
    h.start(&plan);
    h.started();
    std::thread::sleep(Duration::from_millis(500));

    // The camera hangs: no error, no EOS, just no frames.
    let hung = Instant::now();
    h.engine
        .inject_fake_failure(FAKE_CAMERA, FakeFailure::Stall);
    let (ev, _) = h.wait_for("StreamLost", |e| matches!(e, Event::StreamLost { .. }));
    let after = hung.elapsed();
    let Event::StreamLost {
        stream, message, ..
    } = ev
    else {
        unreachable!()
    };
    assert_eq!(stream, "L2");
    assert!(message.contains("stopped sending frames"), "{message}");
    // Lost once it was silent for STALL_LIMIT, counted from its last frame
    // (up to a frame and the pipeline latency before the hang) and checked
    // at the clock ticks.
    assert!(
        after >= STALL_LIMIT - Duration::from_millis(200)
            && after < STALL_LIMIT + Duration::from_secs(2),
        "{after:?}"
    );

    std::thread::sleep(Duration::from_millis(500));
    h.engine.stop(&plan.take, false);
    let (streams, timed_out, skipped) = h.stopped();
    assert!(!timed_out, "{skipped:?}");
    // The still screen (only the keepalive's frames) was never stalled.
    assert!(
        !skipped
            .iter()
            .any(|e| matches!(e, Event::StreamLost { .. })),
        "{skipped:?}"
    );
    assert!(streams.iter().all(|s| s.eos), "{streams:?}");
    // The camera's file ends where it hung, unfilled; the others ran on.
    let (_, screen_dur) = check_video(&h.file(&streams[0].file), 1280, 720);
    let (_, camera_dur) = check_video(&h.file(&streams[1].file), 640, 480);
    let mic_dur = check_audio(&h.file(&streams[2].file), 1);
    assert!(
        screen_dur > camera_dur + 3.0,
        "{screen_dur} vs {camera_dur}"
    );
    assert!(
        (screen_dur - mic_dur).abs() < 0.12,
        "{screen_dur} vs {mic_dur}"
    );
    let log = fs::read_to_string(h.file("takes/take-0001/recorder.log")).unwrap();
    assert!(log.contains("L2 lost: no frames from"), "{log}");
}

#[test]
fn live_pipeline_previews_and_records() {
    let _serial = serial();
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
            Ok(SideEvent::LiveStatus(_)) | Err(_) => {}
        }
    }
    let frame = frame.expect("a preview frame");
    assert_eq!((frame.width, frame.height), (320, 180));
    assert_eq!(frame.rgba.len(), (frame.stride * frame.height) as usize);
    let level = level.expect("a level reading");
    assert_eq!(level.device, FAKE_MIC);
    assert_eq!(level.rms_db.len(), 1);
    // Without a sink of the GUI's: the RGBA appsink, at the canvas size.
    let info = h.engine.inspect();
    assert!(info.preview && !info.preview_sink, "{info:?}");
    assert_eq!(info.preview_size, Some((320, 180)));
    assert_eq!(info.layers.len(), 1);
    assert_eq!(info.layers[0].rect, (0, 0, 320, 180));

    // A layout change applies without errors.
    let mut moved = scene.clone();
    moved.scene.layers[0].rect = Rect::new(160, 90, 160, 90);
    moved.scene.layers[0].opacity = 0.5;
    h.engine.set_live(Some(moved), h.inbox.clone());

    // Recording uses the running pipeline.
    let dir = h.take_dir(1);
    let plan = plan(
        1,
        &dir,
        vec![screen("L1", "FAKE-1")],
        vec![audio("A1", FAKE_MIC, "mic")],
    );
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
    let _serial = serial();
    let mut h = Harness::new();
    let dir = h.take_dir(1);
    let plan = plan(
        1,
        &dir,
        vec![screen("L1", "FAKE-1")],
        vec![audio("A1", FAKE_MIC, "mic")],
    );
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
    let (ev, _) = h.wait_for("CaptureFailed", |e| {
        matches!(e, Event::CaptureFailed { .. })
    });
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

fn camera_layer(id: &str, device: &str, rect: Rect) -> Layer {
    Layer {
        id: id.parse().unwrap(),
        name: "Camera".into(),
        visible: true,
        opacity: 1.0,
        source: VideoSource::Camera {
            device: device.into(),
            label: "Fake camera".into(),
        },
        rect,
        crop: Crop::default(),
        fit: Fit::Cover,
        locked: false,
        missing: false,
    }
}

fn audio_track(id: &str, device: &str) -> AudioTrack {
    AudioTrack {
        id: id.parse().unwrap(),
        role: if device.ends_with(".monitor") {
            AudioRole::System
        } else {
            AudioRole::Mic
        },
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
    let _serial = serial();
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
    let take = plan(
        1,
        &dir,
        vec![screen("L1", "FAKE-1")],
        vec![audio("A1", FAKE_MIC, "mic")],
    );
    h.start(&take);
    h.started();
    assert_eq!(h.engine.inspect().generation, Some(generation));
    // While recording, an equal scene is still a no-op.
    h.engine.set_live(Some(moved.clone()), h.inbox.clone());
    std::thread::sleep(Duration::from_millis(600));
    h.engine.stop(&take.take, false);
    let (streams, timed_out, skipped) = h.stopped();
    assert!(
        !timed_out && streams.iter().all(|s| s.eos),
        "{streams:?} {skipped:?}"
    );
    assert_eq!(
        h.engine.inspect().generation,
        Some(generation),
        "kept between takes"
    );

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

/// A kept live pipeline (the GUI's) whose camera hung is rebuilt after the
/// take that lost it, so the next take doesn't attach to the silent branch.
#[test]
fn a_hung_camera_is_rebuilt_for_the_next_take() {
    let _serial = serial();
    let mut h = Harness::new();
    let full = Rect::new(0, 0, 320, 180);
    let scene = live_scene(
        vec![
            screen_layer("L1", "FAKE-1", full),
            camera_layer("L2", FAKE_CAMERA, full),
        ],
        vec![audio_track("A1", FAKE_MIC)],
        false,
    );
    h.engine.set_live(Some(scene), h.inbox.clone());
    let generation = h.engine.inspect().generation.expect("SetLive builds it");
    // Any mode: the take records from the live camera branch.
    let take = |n: u32, h: &mut Harness| {
        let dir = h.take_dir(n);
        let camera = VideoRecord {
            camera_mode: None,
            ..camera("L2", FAKE_CAMERA)
        };
        plan(
            n,
            &dir,
            vec![screen("L1", "FAKE-1"), camera],
            vec![audio("A1", FAKE_MIC, "mic")],
        )
    };

    let first = take(1, &mut h);
    h.start(&first);
    h.started();
    assert_eq!(h.engine.inspect().generation, Some(generation));
    h.engine
        .inject_fake_failure(FAKE_CAMERA, FakeFailure::Stall);
    let (ev, _) = h.wait_for("StreamLost", |e| matches!(e, Event::StreamLost { .. }));
    assert!(
        matches!(&ev, Event::StreamLost { stream, .. } if stream == "L2"),
        "{ev:?}"
    );
    h.engine.stop(&first.take, false);
    let (_, timed_out, skipped) = h.stopped();
    assert!(!timed_out, "{skipped:?}");
    let rebuilt = h.engine.inspect().generation;
    assert!(rebuilt > Some(generation), "{rebuilt:?}");

    // The camera records again.
    let second = take(2, &mut h);
    h.start(&second);
    let started = h.started();
    assert!(
        matches!(started, Event::CaptureStarted { .. }),
        "{started:?}"
    );
    std::thread::sleep(Duration::from_millis(500));
    h.engine.stop(&second.take, false);
    let (streams, timed_out, skipped) = h.stopped();
    assert!(
        !timed_out && streams.iter().all(|s| s.eos),
        "{streams:?} {skipped:?}"
    );
}

// ------------------------------------------------- the GUI's parts (G§3.9)

/// An appsink standing in for the GUI's `gtk4paintablesink`: it records
/// what it gets and keeps the last frame.
struct PreviewTap {
    sink: gst::Element,
    seen: Arc<Mutex<TapFrames>>,
}

#[derive(Debug, Default, Clone)]
struct TapFrames {
    count: u64,
    format: String,
    size: (u32, u32),
    stride: usize,
    last: Vec<u8>,
}

impl TapFrames {
    /// (B, G, R) of the last BGRx frame at (x, y).
    fn bgr(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let i = y * self.stride + x * 4;
        (self.last[i], self.last[i + 1], self.last[i + 2])
    }

    /// Whether the last frame shows [`BACKGROUND`] at (x, y).
    fn background_at(&self, x: usize, y: usize) -> bool {
        let (b, g, r) = self.bgr(x, y);
        let near = |have: u8, want: u8| have.abs_diff(want) <= 12;
        near(r, 0x12) && near(g, 0x34) && near(b, 0x56)
    }
}

/// The canvas background of the GUI tests: no SMPTE bar is near it.
const BACKGROUND: &str = "#123456";

impl PreviewTap {
    fn new() -> Self {
        crate::gst_init::init().expect("GStreamer");
        let seen = Arc::new(Mutex::new(TapFrames::default()));
        let record = Arc::clone(&seen);
        let sink = gst_app::AppSink::builder()
            .name("gui-preview-sink")
            .max_buffers(1)
            .drop(true)
            .callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let info = sample
                            .caps()
                            .and_then(|caps| gst_video::VideoInfo::from_caps(caps).ok());
                        if let (Some(info), Some(buffer)) = (info, sample.buffer())
                            && let Ok(map) = buffer.map_readable()
                        {
                            let mut seen = record.lock().unwrap();
                            seen.count += 1;
                            seen.format = info.format().to_str().to_string();
                            seen.size = (info.width(), info.height());
                            seen.stride = info.stride()[0] as usize;
                            seen.last = map.as_slice().to_vec();
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            )
            .build();
        Self {
            sink: sink.upcast(),
            seen,
        }
    }

    fn frames(&self) -> TapFrames {
        self.seen.lock().unwrap().clone()
    }

    /// Polls the frames until `done` holds for them.
    fn wait(&self, what: &str, done: impl Fn(&TapFrames) -> bool) -> TapFrames {
        let deadline = Instant::now() + WAIT;
        loop {
            let frames = self.frames();
            if frames.count > 0 && done(&frames) {
                return frames;
            }
            assert!(Instant::now() < deadline, "{what}: not within {WAIT:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn output(&self, max_size: (u32, u32)) -> PreviewOutput {
        PreviewOutput {
            sink: self.sink.clone(),
            max_size,
        }
    }
}

/// A 1280×720 scene with the GUI tests' background.
fn wide_scene(layers: Vec<Layer>, audio: Vec<AudioTrack>) -> LiveScene {
    LiveScene {
        canvas: Canvas {
            width: 1280,
            height: 720,
            fps: Fps::new(30, 1),
            background: BACKGROUND.parse().unwrap(),
        },
        scene: Scene { layers, audio },
        preview: true,
    }
}

/// Polls the engine until `done` holds for what it shows.
fn inspect_until(
    h: &Harness,
    what: &str,
    done: impl Fn(&super::engine::LiveInfo) -> bool,
) -> super::engine::LiveInfo {
    let deadline = Instant::now() + WAIT;
    loop {
        let info = h.engine.inspect();
        if done(&info) {
            return info;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: not within {WAIT:?}; {info:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// H-1: the preview goes to the GUI's sink, BGRx at the canvas fitted into
/// its max size, and the same element serves every rebuilt pipeline; no
/// RGBA frames then.
#[test]
fn the_preview_goes_to_the_given_sink_through_rebuilds() {
    let _serial = serial();
    let tap = PreviewTap::new();
    let (side_tx, side_rx) = crossbeam_channel::unbounded();
    let mut h = Harness::custom(Some(side_tx), |opts| {
        opts.preview = Some(tap.output((320, 180)));
    });
    let quarter = Rect::new(0, 0, 640, 360);
    let mut scene = wide_scene(vec![screen_layer("L1", "FAKE-1", quarter)], Vec::new());
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    let mut info = h.engine.inspect();
    assert!(info.preview_sink, "{info:?}");
    assert_eq!(info.preview_size, Some((320, 180)));
    // The layer's rect × 320 / 1280.
    assert_eq!(info.layers[0].rect, (0, 0, 160, 90));
    let frames = tap.wait("BGRx frames", |f| f.count >= 5);
    assert_eq!((frames.format.as_str(), frames.size), ("BGRx", (320, 180)));
    assert!(frames.background_at(240, 135), "outside the layer");
    tap.wait("the test card in the layer", |f| !f.background_at(40, 45));

    // Two rebuilds (new sources): the same sink, in the new pipeline.
    let more_audio = {
        let mut next = scene.clone();
        next.scene.audio.push(audio_track("A1", FAKE_MIC));
        next
    };
    let more_video = {
        let mut next = more_audio.clone();
        next.scene
            .layers
            .push(screen_layer("L2", "FAKE-2", Rect::new(640, 360, 640, 360)));
        next
    };
    for next in [more_audio, more_video] {
        let before = info.generation;
        h.engine.set_live(Some(next.clone()), h.inbox.clone());
        info = h.engine.inspect();
        assert!(info.generation > before, "{info:?}");
        assert!(info.preview_sink, "{info:?}");
        let parent = tap.sink.parent().expect("in the live pipeline");
        assert_eq!(
            parent.name().as_str(),
            format!("qrec-live-{}", info.generation.unwrap())
        );
        let count = tap.frames().count;
        let frames = tap.wait("frames of the rebuilt pipeline", |f| f.count >= count + 5);
        assert_eq!((frames.format.as_str(), frames.size), ("BGRx", (320, 180)));
        scene = next;
    }
    assert_eq!(info.layers[1].rect, (160, 90, 160, 90));
    // Only the sink got the preview.
    assert!(
        !side_rx
            .try_iter()
            .any(|e| matches!(e, SideEvent::Preview(_))),
        "RGBA frames with a sink given"
    );

    // No live scene: the pipeline is gone and the sink out of it, ready for
    // the next one.
    h.engine.set_live(None, h.inbox.clone());
    assert_eq!(h.engine.inspect().generation, None);
    assert!(tap.sink.parent().is_none());
    h.engine.set_live(Some(scene), h.inbox.clone());
    assert!(h.engine.inspect().preview_sink);
    let count = tap.frames().count;
    tap.wait("frames again", |f| f.count >= count + 5);
}

/// The preview's size is the canvas fitted into the sink's max size, never
/// larger than the canvas.
#[test]
fn the_preview_size_fits_the_canvas_into_the_max_size() {
    let _serial = serial();
    let tap = PreviewTap::new();
    let mut h = Harness::custom(None, |opts| opts.preview = Some(tap.output((960, 540))));
    let mut square = wide_scene(
        vec![screen_layer("L1", "FAKE-1", Rect::new(0, 0, 1080, 1080))],
        Vec::new(),
    );
    square.canvas.width = 1080;
    square.canvas.height = 1080;
    h.engine.set_live(Some(square.clone()), h.inbox.clone());
    assert_eq!(h.engine.inspect().preview_size, Some((540, 540)));
    tap.wait("a 540×540 frame", |f| f.size == (540, 540));
    // A small canvas isn't scaled up.
    let mut small = square;
    small.canvas.width = 320;
    small.canvas.height = 180;
    small.scene.layers[0].rect = Rect::new(0, 0, 320, 180);
    h.engine.set_live(Some(small), h.inbox.clone());
    assert_eq!(h.engine.inspect().preview_size, Some((320, 180)));
    tap.wait("a 320×180 frame", |f| f.size == (320, 180));
}

/// D-32, D-33: `LiveControl::transient_layout` moves preview layers without
/// a rebuild, until the next `SetLive` (any) or a `None`.
#[test]
fn a_transient_layout_moves_the_preview_until_the_next_set_live() {
    let _serial = serial();
    let tap = PreviewTap::new();
    let mut h = Harness::custom(None, |opts| opts.preview = Some(tap.output((320, 180))));
    let top_left = Rect::new(0, 0, 640, 360);
    let scene = wide_scene(vec![screen_layer("L1", "FAKE-1", top_left)], Vec::new());
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    let generation = h.engine.inspect().generation;
    assert!(generation.is_some());
    let at_top_left = |f: &TapFrames| !f.background_at(40, 45) && f.background_at(240, 135);
    let at_bottom_right = |f: &TapFrames| f.background_at(40, 45) && !f.background_at(240, 135);
    tap.wait("the layer at the top left", at_top_left);

    let control = h.engine.control();
    let mut moved = scene.scene.clone();
    moved.layers[0].rect = Rect::new(640, 360, 640, 360);
    moved.layers[0].opacity = 0.5;
    control.transient_layout(Some(moved.clone()));
    let info = h.engine.inspect();
    assert_eq!(info.generation, generation, "no rebuild");
    assert!(info.transient);
    assert_eq!(info.layers[0].rect, (160, 90, 160, 90));
    assert_eq!(info.layers[0].alpha, 0.5);
    tap.wait("the layer at the bottom right", at_bottom_right);

    // The next SetLive, even of the same scene, ends it.
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    let info = h.engine.inspect();
    assert_eq!(info.generation, generation);
    assert!(!info.transient);
    assert_eq!(info.layers[0].rect, (0, 0, 160, 90));
    assert_eq!(info.layers[0].alpha, 1.0);
    tap.wait("the layer back at the top left", at_top_left);

    // `None` ends it too.
    control.transient_layout(Some(moved.clone()));
    assert_eq!(h.engine.inspect().layers[0].rect, (160, 90, 160, 90));
    control.transient_layout(None);
    let info = h.engine.inspect();
    assert!(!info.transient);
    assert_eq!(info.layers[0].rect, (0, 0, 160, 90));

    // A SetLive with another layout (no rebuild) ends it and shows its own.
    control.transient_layout(Some(moved));
    let mut lower = scene.clone();
    lower.scene.layers[0].rect = Rect::new(0, 360, 640, 360);
    h.engine.set_live(Some(lower), h.inbox.clone());
    let info = h.engine.inspect();
    assert_eq!(info.generation, generation);
    assert!(!info.transient);
    assert_eq!(info.layers[0].rect, (0, 90, 160, 90));
}

/// A compact form of a status list: each source's name and state.
fn brief(statuses: &[LiveSourceStatus]) -> Vec<(String, &'static str)> {
    statuses
        .iter()
        .map(|s| {
            let state = match &s.state {
                LiveSourceState::AwaitingPortal => "awaiting",
                LiveSourceState::Live => "live",
                LiveSourceState::Refused {
                    cancelled: true, ..
                } => "cancelled",
                LiveSourceState::Refused { .. } => "refused",
                LiveSourceState::Dead { .. } => "dead",
            };
            (s.source.name().to_owned(), state)
        })
        .collect()
}

fn names(list: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    list.iter().map(|(n, s)| ((*n).to_owned(), *s)).collect()
}

/// The `LiveStatus` events up to the first that `done` accepts (it last).
fn statuses_until(
    side: &Receiver<SideEvent>,
    what: &str,
    done: impl Fn(&[LiveSourceStatus]) -> bool,
) -> Vec<Vec<LiveSourceStatus>> {
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    loop {
        match side.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(SideEvent::LiveStatus(status)) => {
                let finished = done(&status);
                seen.push(status);
                if finished {
                    return seen;
                }
            }
            Ok(_) => {}
            Err(_) => panic!("no {what} within {WAIT:?}; got {seen:#?}"),
        }
    }
}

/// G§3.9 `LiveStatus` for a build: every source live, in the scene's
/// order, then with the sizes they negotiated; nothing awaits a portal. A
/// rebuilt pipeline reports the sizes at once; no live scene, no sources.
#[test]
fn live_status_follows_a_build() {
    let _serial = serial();
    let (side_tx, side_rx) = crossbeam_channel::unbounded();
    let mut h = Harness::with_side(Some(side_tx));
    let full = Rect::new(0, 0, 320, 180);
    let scene = live_scene(
        vec![
            camera_layer("L2", FAKE_CAMERA, full),
            screen_layer("L1", "FAKE-1", full),
        ],
        vec![audio_track("A1", FAKE_MIC)],
        true,
    );
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    let seen = statuses_until(&side_rx, "the sizes", |s| {
        s.len() == 3 && s[..2].iter().all(|s| s.size.is_some())
    });
    let all_live = names(&[
        (FAKE_CAMERA, "live"),
        ("FAKE-1", "live"),
        (FAKE_MIC, "live"),
    ]);
    for status in &seen {
        assert_eq!(brief(status), all_live, "{seen:#?}");
    }
    let last = seen.last().unwrap().clone();
    assert_eq!(
        last[0].size,
        Some((1280, 720)),
        "the fake camera's 1280×720 mode"
    );
    assert_eq!(last[1].size, Some((1280, 720)));
    assert_eq!(last[2].size, None, "audio has no size");
    // Nothing changes: nothing is sent.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !side_rx
            .try_iter()
            .any(|e| matches!(e, SideEvent::LiveStatus(_))),
        "a status without a change"
    );

    // No live scene: no sources. Back: live, with the sizes known already.
    h.engine.set_live(None, h.inbox.clone());
    let seen = statuses_until(&side_rx, "an empty status", <[_]>::is_empty);
    assert_eq!(seen.len(), 1, "{seen:#?}");
    h.engine.set_live(Some(scene), h.inbox.clone());
    let seen = statuses_until(&side_rx, "a status", |_| true);
    assert_eq!(seen[0], last);
}

/// `LiveControl::progress` (the GUI's `waitIdle`, G§3.18): a `set_live` is
/// unsettled from the moment it returns until the engine handled it and
/// every video source of the pipeline it built negotiated there, also for
/// the rebuilds S01b's audio tracks make, which keep both video sources'
/// states and sizes and so send no status that could tell a GUI (the
/// S01b shot caught the new pipeline's black frames). A layout change
/// settles in the same pipeline; no live scene settles with none.
#[test]
fn the_live_progress_waits_for_each_rebuild_to_negotiate() {
    let _serial = serial();
    let (side_tx, side_rx) = crossbeam_channel::unbounded();
    let mut h = Harness::with_side(Some(side_tx));
    let control = h.engine.control();
    assert_eq!(
        control.progress(),
        Some(LiveProgress {
            settled: true,
            generation: 0
        })
    );
    let settled = |what: &str| -> LiveProgress {
        let deadline = Instant::now() + WAIT;
        loop {
            let progress = control.progress().expect("an engine's control");
            if progress.settled {
                return progress;
            }
            assert!(Instant::now() < deadline, "{what}: not settled in {WAIT:?}");
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    let full = Rect::new(0, 0, 320, 180);
    let video = vec![
        screen_layer("L1", "FAKE-1", full),
        camera_layer("L2", FAKE_CAMERA, full),
    ];
    let mut generation = 0;
    for (i, audio) in [
        vec![],
        vec![audio_track("A1", FAKE_MIC)],
        vec![audio_track("A1", FAKE_MIC), audio_track("A2", FAKE_MONITOR)],
    ]
    .into_iter()
    .enumerate()
    {
        h.engine.set_live(
            Some(live_scene(video.clone(), audio, true)),
            h.inbox.clone(),
        );
        assert!(
            !control.progress().unwrap().settled,
            "scene {i}: settled before the engine saw it"
        );
        let progress = settled(&format!("scene {i}"));
        assert!(
            progress.generation > generation,
            "scene {i}: a new pipeline"
        );
        generation = progress.generation;
        let info = h.engine.inspect();
        assert_eq!(info.generation, Some(generation), "scene {i}");
        let video: Vec<bool> = info
            .sources
            .iter()
            .zip(&info.negotiated)
            .filter(|(spec, _)| spec.is_video())
            .map(|(_, negotiated)| *negotiated)
            .collect();
        assert_eq!(video, [true, true], "scene {i}: {info:?}");
    }
    // The audio tracks' rebuilds told nothing of the video sources: their
    // sizes were there in every status (the cached ones at once).
    let statuses: Vec<Vec<LiveSourceStatus>> = side_rx
        .try_iter()
        .filter_map(|e| match e {
            SideEvent::LiveStatus(s) => Some(s),
            _ => None,
        })
        .collect();
    let with_audio: Vec<_> = statuses.iter().filter(|s| s.len() > 2).collect();
    assert!(!with_audio.is_empty(), "{statuses:#?}");
    assert!(
        with_audio.iter().all(|s| s[..2]
            .iter()
            .all(|v| v.state == LiveSourceState::Live && v.size.is_some())),
        "{statuses:#?}"
    );

    // A layout change: no rebuild, settled in the same pipeline.
    let mut moved = video.clone();
    moved[1].rect = Rect::new(160, 90, 160, 90);
    h.engine.set_live(
        Some(live_scene(
            moved,
            vec![audio_track("A1", FAKE_MIC), audio_track("A2", FAKE_MONITOR)],
            true,
        )),
        h.inbox.clone(),
    );
    assert_eq!(settled("the layout").generation, generation);
    // No live scene: settled without a pipeline.
    h.engine.set_live(None, h.inbox.clone());
    assert_eq!(settled("no live scene").generation, 0);
}

/// G§3.9 `LiveStatus` for a refusal (`QREC_FAKE_PORTAL=deny` and
/// `cancel`): the screen awaits the portal, then is refused (cancelled for
/// a closed picker) while the rest goes live. A layout change, or Program
/// mode and back, doesn't ask again; `retry_live` does (D-50).
#[test]
fn live_status_follows_a_refusal() {
    let _serial = serial();
    for (portal, refused) in [
        (FakePortal::Deny, "refused"),
        (FakePortal::Cancel, "cancelled"),
    ] {
        let (side_tx, side_rx) = crossbeam_channel::unbounded();
        let mut h = Harness::custom(Some(side_tx), |opts| {
            opts.fake = FakeHooks {
                portal: Some(portal),
                ..FakeHooks::default()
            };
        });
        let full = Rect::new(0, 0, 320, 180);
        let scene = live_scene(
            vec![screen_layer("L1", "FAKE-1", full)],
            vec![audio_track("A1", FAKE_MIC)],
            true,
        );
        h.engine.set_live(Some(scene.clone()), h.inbox.clone());
        let answered = names(&[("FAKE-1", refused), (FAKE_MIC, "live")]);
        let seen = statuses_until(&side_rx, "the refusal", |s| brief(s) == answered);
        let briefs: Vec<_> = seen.iter().map(|s| brief(s)).collect();
        assert_eq!(
            briefs,
            [names(&[("FAKE-1", "awaiting")]), answered.clone()],
            "{portal:?}"
        );
        let LiveSourceState::Refused { message, .. } = &seen[1][0].state else {
            unreachable!()
        };
        assert!(message.contains("FAKE-1"), "{message}");
        assert_eq!(seen[1][0].size, None);
        let info = h.engine.inspect();
        assert_eq!(info.portal_asks, 1);
        assert_eq!(
            info.sources,
            vec![SourceSpec::Audio {
                device: FAKE_MIC.into()
            }]
        );

        // Moving the layer keeps the refusal: no dialog again.
        let mut moved = scene.clone();
        moved.scene.layers[0].rect = Rect::new(10, 10, 160, 90);
        h.engine.set_live(Some(moved.clone()), h.inbox.clone());
        assert_eq!(h.engine.inspect().portal_asks, 1, "{portal:?}");
        // Program mode and back keeps it too.
        h.engine.set_live(None, h.inbox.clone());
        h.engine.set_live(Some(moved), h.inbox.clone());
        assert_eq!(h.engine.inspect().portal_asks, 1, "{portal:?}");
        statuses_until(&side_rx, "the refusal again", |s| brief(s) == answered);

        // The GUI's retry asks again.
        h.engine.retry_live(h.inbox.clone());
        let seen = statuses_until(&side_rx, "the second refusal", |s| brief(s) == answered);
        assert_eq!(
            brief(&seen[0]),
            names(&[("FAKE-1", "awaiting"), (FAKE_MIC, "live")]),
            "{portal:?}: {seen:#?}"
        );
        assert_eq!(h.engine.inspect().portal_asks, 2);
    }
}

/// G§3.9 `LiveStatus` for a source's death (an error, a stream that ends)
/// and for a source that can't start: dead, while the rest stays live; a
/// retry brings them back.
#[test]
fn live_status_follows_a_death() {
    let _serial = serial();
    let (side_tx, side_rx) = crossbeam_channel::unbounded();
    let mut h = Harness::with_side(Some(side_tx));
    let full = Rect::new(0, 0, 320, 180);
    let scene = live_scene(
        vec![
            screen_layer("L1", "FAKE-1", full),
            camera_layer("L2", FAKE_CAMERA, full),
        ],
        vec![audio_track("A1", FAKE_MIC)],
        true,
    );
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    let all_live = names(&[
        ("FAKE-1", "live"),
        (FAKE_CAMERA, "live"),
        (FAKE_MIC, "live"),
    ]);
    statuses_until(&side_rx, "a build", |s| {
        brief(s) == all_live && s[1].size.is_some()
    });
    let generation = h.engine.inspect().generation;

    // The camera is unplugged.
    h.engine
        .inject_fake_failure(FAKE_CAMERA, FakeFailure::Error);
    let camera_dead = names(&[
        ("FAKE-1", "live"),
        (FAKE_CAMERA, "dead"),
        (FAKE_MIC, "live"),
    ]);
    let seen = statuses_until(&side_rx, "the camera's death", |s| brief(s) == camera_dead);
    let last = seen.last().unwrap();
    let LiveSourceState::Dead { message } = &last[1].state else {
        unreachable!()
    };
    assert!(message.contains("unplugged"), "{message}");
    // Its last size stays known (the layer box).
    assert_eq!(last[1].size, Some((1280, 720)));
    // The mic's stream ends.
    h.engine.inject_fake_failure(FAKE_MIC, FakeFailure::Eos);
    let both_dead = names(&[
        ("FAKE-1", "live"),
        (FAKE_CAMERA, "dead"),
        (FAKE_MIC, "dead"),
    ]);
    let seen = statuses_until(&side_rx, "the mic's end", |s| brief(s) == both_dead);
    let LiveSourceState::Dead { message } = &seen.last().unwrap()[2].state else {
        unreachable!()
    };
    assert!(message.contains("stopped sending data"), "{message}");
    assert_eq!(h.engine.inspect().generation, generation, "no rebuild yet");

    // The GUI's retry rebuilds: everything is live again.
    h.engine.retry_live(h.inbox.clone());
    statuses_until(&side_rx, "the retry", |s| brief(s) == all_live);
    let retried = h.engine.inspect().generation;
    assert!(retried > generation);
    // A retry with nothing wrong rebuilds too.
    h.engine.retry_live(h.inbox.clone());
    assert!(h.engine.inspect().generation > retried);

    // A source that can't start is dead; the rest runs without it.
    let mut broken = scene;
    broken.scene.layers[1] = camera_layer("L2", "/dev/video0", full);
    h.engine.set_live(Some(broken), h.inbox.clone());
    let failed = names(&[
        ("FAKE-1", "live"),
        ("/dev/video0", "dead"),
        (FAKE_MIC, "live"),
    ]);
    let seen = statuses_until(&side_rx, "a failed start", |s| brief(s) == failed);
    let LiveSourceState::Dead { message } = &seen.last().unwrap()[1].state else {
        unreachable!()
    };
    assert!(message.contains("no fake camera"), "{message}");
    let info = h.engine.inspect();
    assert_eq!(info.sources.len(), 2, "{info:?}");
    assert!(info.preview);
}

/// A preview rebuilt with its sources' sizes known (Program mode and back,
/// or a take that adds a source) keeps a camera its layer crops (`cover`
/// in a box of another aspect), with the GUI's sink and with the RGBA
/// appsink. The rebuilt pipeline used to be laid out only once it ran: the
/// crop then changed while the camera's first caps query crossed its
/// `videocrop` (two caps transforms with different crops, no common caps),
/// and the camera died "not-negotiated" in about half the round trips of
/// the GUI's Live mode, and broke a take whose pipeline it rebuilt.
#[test]
fn a_rebuilt_preview_keeps_a_cropped_camera() {
    const ROUND_TRIPS: usize = 12;
    let _serial = serial();
    for with_sink in [true, false] {
        let tap = PreviewTap::new();
        let (side_tx, side_rx) = crossbeam_channel::unbounded();
        let mut h = Harness::custom(Some(side_tx), |opts| {
            if with_sink {
                opts.preview = Some(tap.output((960, 540)));
            }
        });
        // A 4:3 box: `cover` crops the fake camera's 1280×720 to 960×720.
        let scene = wide_scene(
            vec![
                camera_layer("L2", FAKE_CAMERA, Rect::new(960, 480, 320, 240)),
                screen_layer("L1", "FAKE-1", Rect::new(0, 0, 1280, 720)),
            ],
            Vec::new(),
        );
        let all_live = names(&[(FAKE_CAMERA, "live"), ("FAKE-1", "live")]);
        h.engine.set_live(Some(scene.clone()), h.inbox.clone());
        statuses_until(&side_rx, "the sizes", |s| {
            brief(s) == all_live && s.iter().all(|s| s.size.is_some())
        });
        for round in 1..=ROUND_TRIPS {
            h.engine.set_live(None, h.inbox.clone());
            statuses_until(&side_rx, "no sources", <[_]>::is_empty);
            h.engine.set_live(Some(scene.clone()), h.inbox.clone());
            // The rebuild's first status, however long the build takes (on
            // a loaded machine the RGBA path's start took over 200 ms, and
            // a fixed 200 ms saw nothing yet), then 200 ms in which the
            // sources negotiate (within milliseconds of the start): no
            // status may lose a source, and no live error.
            let deadline = Instant::now() + WAIT;
            let mut seen: Vec<SideEvent> = Vec::new();
            while seen.is_empty() {
                match side_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(e @ (SideEvent::LiveStatus(_) | SideEvent::LiveError(_))) => seen.push(e),
                    Ok(_) => {}
                    Err(_) => panic!(
                        "round trip {round} (GUI sink: {with_sink}): no status within {WAIT:?}"
                    ),
                }
            }
            std::thread::sleep(Duration::from_millis(200));
            seen.extend(
                side_rx
                    .try_iter()
                    .filter(|e| matches!(e, SideEvent::LiveStatus(_) | SideEvent::LiveError(_))),
            );
            let all_fine = seen
                .iter()
                .all(|e| matches!(e, SideEvent::LiveStatus(s) if brief(s) == all_live));
            assert!(
                all_fine,
                "round trip {round} (GUI sink: {with_sink}): {seen:#?}"
            );
        }
        // Still laid out with the crop: the camera's pad is the box × 3/4.
        let info = h.engine.inspect();
        assert_eq!(info.preview_sink, with_sink, "{info:?}");
        let scale = |v: i32| if with_sink { v * 3 / 4 } else { v };
        assert_eq!(
            info.layers[0].rect,
            (scale(960), scale(480), scale(320), scale(240)),
            "{info:?}"
        );
    }
}

/// A camera whose mode changes (a new canvas rate: the fake camera is
/// 1280×720 at 30 fps and 640×480 at 60) starts laid out at its new mode's
/// size. The size was cached by device: the rebuilt camera got the crop of
/// a `cover` 9:16 box at 1280×720 (436 + 438 px), wider than its 640 px
/// frames, so it had no caps and died "not-negotiated", and again on every
/// retry and take, since a source that never negotiates never reports a
/// new size.
#[test]
fn a_new_camera_mode_is_laid_out_at_its_own_size() {
    let _serial = serial();
    let (side_tx, side_rx) = crossbeam_channel::unbounded();
    let mut h = Harness::with_side(Some(side_tx));
    let scene = wide_scene(
        vec![camera_layer("L2", FAKE_CAMERA, Rect::new(0, 0, 180, 320))],
        Vec::new(),
    );
    let live_at = |size: (u32, u32)| {
        move |s: &[LiveSourceStatus]| {
            brief(s) == names(&[(FAKE_CAMERA, "live")]) && s[0].size == Some(size)
        }
    };
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    statuses_until(&side_rx, "the 1280×720 mode", live_at((1280, 720)));
    let mut fast = scene;
    fast.canvas.fps = Fps::new(60, 1);
    // Still live, at 640×480, after the rebuild and after a retry (which
    // rebuilds without a change to report).
    let stays_live = |side: &Receiver<SideEvent>, what: &str| {
        std::thread::sleep(Duration::from_millis(500));
        let later: Vec<SideEvent> = side
            .try_iter()
            .filter(|e| matches!(e, SideEvent::LiveStatus(_) | SideEvent::LiveError(_)))
            .collect();
        assert!(
            later
                .iter()
                .all(|e| matches!(e, SideEvent::LiveStatus(s) if live_at((640, 480))(s))),
            "{what}: {later:#?}"
        );
    };
    h.engine.set_live(Some(fast), h.inbox.clone());
    statuses_until(&side_rx, "the 640×480 mode", live_at((640, 480)));
    stays_live(&side_rx, "the 640×480 mode");
    let generation = h.engine.inspect().generation;
    h.engine.retry_live(h.inbox.clone());
    inspect_until(&h, "the retry's rebuild", |i| i.generation > generation);
    stays_live(&side_rx, "the retry");
}

/// A screen asked of the portal again is `AwaitingPortal` while its dialog
/// is open, not its last state: a dead screen the GUI retries (its dead
/// branch stays in the pipeline until the answer), and a refused screen a
/// take asks for again.
#[test]
fn a_screen_asked_again_awaits_the_portal() {
    let _serial = serial();
    let full = Rect::new(0, 0, 320, 180);
    {
        let (side_tx, side_rx) = crossbeam_channel::unbounded();
        let mut h = Harness::with_side(Some(side_tx));
        h.engine.share_fake_screens();
        let scene = live_scene(vec![screen_layer("L1", "FAKE-1", full)], Vec::new(), true);
        h.engine.set_live(Some(scene), h.inbox.clone());
        let live = names(&[("FAKE-1", "live")]);
        statuses_until(&side_rx, "the screen", |s| brief(s) == live);
        h.engine.inject_fake_failure("FAKE-1", FakeFailure::Error);
        statuses_until(&side_rx, "the screen's death", |s| {
            brief(s) == names(&[("FAKE-1", "dead")])
        });
        h.engine.retry_live(h.inbox.clone());
        let seen = statuses_until(&side_rx, "the retry", |s| brief(s) == live);
        let briefs: Vec<_> = seen.iter().map(|s| brief(s)).collect();
        assert_eq!(briefs, [names(&[("FAKE-1", "awaiting")]), live]);
        assert_eq!(h.engine.inspect().portal_asks, 2);
    }
    let (side_tx, side_rx) = crossbeam_channel::unbounded();
    let mut h = Harness::custom(Some(side_tx), |opts| {
        opts.fake = FakeHooks {
            portal: Some(FakePortal::Deny),
            ..FakeHooks::default()
        };
    });
    let scene = live_scene(
        vec![screen_layer("L1", "FAKE-1", full)],
        vec![audio_track("A1", FAKE_MIC)],
        true,
    );
    h.engine.set_live(Some(scene), h.inbox.clone());
    let refused = names(&[("FAKE-1", "refused"), (FAKE_MIC, "live")]);
    statuses_until(&side_rx, "the refusal", |s| brief(s) == refused);
    let dir = h.take_dir(1);
    h.start(&plan(
        1,
        &dir,
        vec![screen("L1", "FAKE-1")],
        vec![audio("A1", FAKE_MIC, "mic")],
    ));
    let seen = statuses_until(&side_rx, "the take's refusal", |s| brief(s) == refused);
    let briefs: Vec<_> = seen.iter().map(|s| brief(s)).collect();
    assert_eq!(
        briefs,
        [
            names(&[("FAKE-1", "awaiting"), (FAKE_MIC, "live")]),
            refused
        ]
    );
    h.wait_for("CaptureFailed", |e| {
        matches!(e, Event::CaptureFailed { .. })
    });
}

/// D-32, D-33: a Live layer drag changes a `cover` camera's crop many times
/// a second through transient layouts, from the engine thread, while the
/// camera flows and while a rebuilt pipeline's sources negotiate. A crop
/// that changed between the two caps transforms of a query crossing the
/// camera's `videocrop` left no common caps and the camera died
/// "not-negotiated" (d0c4189). Now crops land on the streaming thread
/// between frames and a `videoscale` before each `videocrop` answers
/// upstream queries for any size, so no interleaving loses the source:
/// layouts hammer the crop from another thread (a new aspect, so a new
/// cover crop, every millisecond) through rebuild after rebuild, and every
/// source stays live with no live error.
#[test]
fn transient_crops_never_lose_a_source() {
    use std::sync::atomic::{AtomicBool, Ordering};
    const ROUNDS: usize = 8;
    let _serial = serial();
    for with_sink in [true, false] {
        let tap = PreviewTap::new();
        let (side_tx, side_rx) = crossbeam_channel::unbounded();
        let mut h = Harness::custom(Some(side_tx), |opts| {
            if with_sink {
                opts.preview = Some(tap.output((960, 540)));
            }
        });
        let scene = wide_scene(
            vec![
                camera_layer("L2", FAKE_CAMERA, Rect::new(960, 480, 320, 240)),
                screen_layer("L1", "FAKE-1", Rect::new(0, 0, 1280, 720)),
            ],
            Vec::new(),
        );
        let all_live = names(&[(FAKE_CAMERA, "live"), ("FAKE-1", "live")]);
        h.engine.set_live(Some(scene.clone()), h.inbox.clone());
        statuses_until(&side_rx, "the sizes", |s| {
            brief(s) == all_live && s.iter().all(|s| s.size.is_some())
        });
        let stop = Arc::new(AtomicBool::new(false));
        let hammer = {
            let (stop, control, base) =
                (Arc::clone(&stop), h.engine.control(), scene.scene.clone());
            std::thread::spawn(move || {
                // 4:3, 1:1, 16:9, 3:4: a new cover crop every time.
                const BOXES: [(u32, u32); 4] = [(320, 240), (240, 240), (320, 180), (180, 240)];
                let mut sent = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let mut moved = base.clone();
                    let (w, h) = BOXES[sent % BOXES.len()];
                    moved.layers[0].rect = Rect::new(960 - (sent % 64) as i32, 480, w, h);
                    control.transient_layout(Some(moved));
                    sent += 1;
                    std::thread::sleep(Duration::from_millis(1));
                }
                sent
            })
        };
        let mut problems = Vec::new();
        for round in 1..=ROUNDS {
            // Steady flow first, then a rebuild (its sources negotiating
            // under the hammer).
            std::thread::sleep(Duration::from_millis(400));
            h.engine.set_live(None, h.inbox.clone());
            statuses_until(&side_rx, "no sources", <[_]>::is_empty);
            h.engine.set_live(Some(scene.clone()), h.inbox.clone());
            std::thread::sleep(Duration::from_millis(400));
            for event in side_rx.try_iter() {
                match &event {
                    SideEvent::LiveStatus(s) if brief(s) == all_live => {}
                    SideEvent::LiveStatus(_) | SideEvent::LiveError(_) => {
                        problems.push(format!("round {round}: {event:?}"));
                    }
                    _ => {}
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        let sent = hammer.join().expect("the hammer");
        assert!(sent > 2000, "{sent} layouts sent");
        assert!(
            problems.is_empty(),
            "GUI sink: {with_sink}; {sent} layouts: {problems:#?}"
        );
        // Still live: every source, and frames keep coming.
        h.engine.set_live(Some(scene.clone()), h.inbox.clone());
        let info = h.engine.inspect();
        assert_eq!(info.layers.len(), 2, "{info:?}");
        if with_sink {
            let before = tap.frames().count;
            tap.wait("more frames", |f| f.count > before + 5);
        }
    }
}

/// The two halves of the crop race together (A§8.11): takes start and stop
/// on the live pipeline while a Live drag hammers a `cover` camera's crop.
/// Each take's record bins join their tees without making the sources
/// negotiate again, and the crops land on the streaming thread, so every
/// take records both streams to their end, from the same pipeline, and no
/// source dies or reports a live error.
#[test]
fn takes_keep_every_stream_under_transient_crops() {
    use std::sync::atomic::{AtomicBool, Ordering};
    const TAKES: u32 = 4;
    let _serial = serial();
    let tap = PreviewTap::new();
    let (side_tx, side_rx) = crossbeam_channel::unbounded();
    let mut h = Harness::custom(Some(side_tx), |opts| {
        opts.preview = Some(tap.output((960, 540)));
    });
    let scene = wide_scene(
        vec![
            camera_layer("L2", FAKE_CAMERA, Rect::new(960, 480, 320, 240)),
            screen_layer("L1", "FAKE-1", Rect::new(0, 0, 1280, 720)),
        ],
        Vec::new(),
    );
    let all_live = names(&[(FAKE_CAMERA, "live"), ("FAKE-1", "live")]);
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    statuses_until(&side_rx, "the sizes", |s| {
        brief(s) == all_live && s.iter().all(|s| s.size.is_some())
    });
    let generation = h.engine.inspect().generation;
    let stop = Arc::new(AtomicBool::new(false));
    let hammer = {
        let (stop, control, base) = (Arc::clone(&stop), h.engine.control(), scene.scene.clone());
        std::thread::spawn(move || {
            // 4:3, 1:1, 16:9, 3:4: a new cover crop every time.
            const BOXES: [(u32, u32); 4] = [(320, 240), (240, 240), (320, 180), (180, 240)];
            let mut sent = 0usize;
            while !stop.load(Ordering::Relaxed) {
                let mut moved = base.clone();
                let (w, h) = BOXES[sent % BOXES.len()];
                moved.layers[0].rect = Rect::new(960 - (sent % 64) as i32, 480, w, h);
                control.transient_layout(Some(moved));
                sent += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
            sent
        })
    };
    let mut problems = Vec::new();
    for n in 1..=TAKES {
        let dir = h.take_dir(n);
        // The live branch's camera (no mode), so the take attaches to it.
        let mut cam = camera("L2", FAKE_CAMERA);
        cam.camera_mode = None;
        let take = plan(n, &dir, vec![screen("L1", "FAKE-1"), cam], Vec::new());
        h.start(&take);
        h.started();
        std::thread::sleep(Duration::from_millis(600));
        h.engine.stop(&take.take, false);
        let (streams, timed_out, skipped) = h.stopped();
        if timed_out || streams.len() != 2 || !streams.iter().all(|s| s.eos) {
            problems.push(format!("take {n}: {streams:?} (before: {skipped:?})"));
        }
        for event in side_rx.try_iter() {
            match &event {
                SideEvent::LiveStatus(s) if brief(s) == all_live => {}
                SideEvent::LiveStatus(_) | SideEvent::LiveError(_) => {
                    problems.push(format!("take {n}: {event:?}"));
                }
                _ => {}
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    let sent = hammer.join().expect("the hammer");
    assert!(sent > 1000, "{sent} layouts sent");
    assert!(problems.is_empty(), "{sent} layouts: {problems:#?}");
    assert_eq!(
        h.engine.inspect().generation,
        generation,
        "the takes recorded from the live pipeline"
    );
    let before = tap.frames().count;
    tap.wait("more frames", |f| f.count > before + 5);
}

/// H-4: the screen casts outlive the live pipeline by `cast_grace` (a fake
/// portal that shares: its call count), then close; without a grace they
/// close at once.
#[test]
fn screen_casts_stay_open_for_the_cast_grace() {
    let _serial = serial();
    let grace = Duration::from_millis(1500);
    let mut h = Harness::custom(None, |opts| opts.cast_grace = grace);
    h.engine.share_fake_screens();
    let full = Rect::new(0, 0, 320, 180);
    let scene = live_scene(vec![screen_layer("L1", "FAKE-1", full)], Vec::new(), true);
    let shown = |h: &Harness| inspect_until(h, "the live pipeline", |i| i.generation.is_some());
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    let info = shown(&h);
    assert_eq!(info.portal_asks, 1);
    assert_eq!(info.casts, ["FAKE-1"]);

    // Program mode: the pipeline stops, the cast stays.
    h.engine.set_live(None, h.inbox.clone());
    let info = h.engine.inspect();
    assert_eq!(info.generation, None);
    assert_eq!(info.casts, ["FAKE-1"]);
    // Live again within the grace: no new dialog.
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    assert_eq!(shown(&h).portal_asks, 1);

    // Past the grace the cast is closed, and Live asks again.
    h.engine.set_live(None, h.inbox.clone());
    std::thread::sleep(grace + Duration::from_millis(500));
    assert!(h.engine.inspect().casts.is_empty());
    h.engine.set_live(Some(scene.clone()), h.inbox.clone());
    let info = shown(&h);
    assert_eq!((info.portal_asks, info.casts.len()), (2, 1));
    drop(h);

    // Hosts without a grace close their casts with the pipeline.
    let mut h = Harness::new();
    h.engine.share_fake_screens();
    h.engine.set_live(Some(scene), h.inbox.clone());
    assert_eq!(shown(&h).casts.len(), 1);
    h.engine.set_live(None, h.inbox.clone());
    assert!(h.engine.inspect().casts.is_empty());
}

/// D-03: the meter monitor reads the fake tone about every 50 ms per device
/// (once per device; one it doesn't have stays idle), and stops within 5 s.
#[test]
fn the_meter_monitor_reads_the_fake_tone() {
    let _serial = serial();
    let problems_before = problems();
    let (tx, rx) = crossbeam_channel::unbounded();
    let devices = [FAKE_MIC, FAKE_MONITOR, FAKE_MIC, "alsa_input.not-fake"];
    let monitor = MeterMonitor::start(
        devices.iter().map(|d| (*d).to_owned()).collect(),
        Backend::Fake,
        tx,
    )
    .expect("starts");
    let mut readings: BTreeMap<String, Vec<(Instant, f64)>> = BTreeMap::new();
    let window = Duration::from_millis(3200);
    let began = Instant::now();
    while began.elapsed() < window {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(SideEvent::Level(level)) => readings
                .entry(level.device.clone())
                .or_default()
                .push((Instant::now(), level.rms_db[0])),
            Ok(other) => panic!("{other:?}"),
            Err(_) => {}
        }
    }
    assert_eq!(
        readings.keys().map(String::as_str).collect::<Vec<_>>(),
        [FAKE_MIC, FAKE_MONITOR]
    );
    for (device, seen) in &readings {
        // About 20 a second, each ~50 ms after the one before.
        assert!(
            (50..=75).contains(&seen.len()),
            "{device}: {} readings",
            seen.len()
        );
        let mut gaps: Vec<Duration> = seen.windows(2).map(|w| w[1].0 - w[0].0).collect();
        gaps.sort();
        let median = gaps[gaps.len() / 2];
        assert!(
            (Duration::from_millis(35)..=Duration::from_millis(65)).contains(&median),
            "{device}: median gap {median:?}"
        );
        // The tone and its silences.
        assert!(seen.iter().any(|(_, db)| *db > -20.0), "{device}: {seen:?}");
        assert!(seen.iter().any(|(_, db)| *db < -60.0), "{device}: {seen:?}");
    }

    let stopping = Instant::now();
    monitor.stop();
    while !monitor.is_finished() {
        assert!(stopping.elapsed() < Duration::from_secs(5), "not stopped");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Nothing after it stopped.
    rx.try_iter().for_each(drop);
    std::thread::sleep(Duration::from_millis(200));
    assert!(rx.try_recv().is_err(), "a reading after the stop");
    drop(monitor);
    assert_quiet_since(problems_before);
}
