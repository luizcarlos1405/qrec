//! End-to-end tests of the media runner: fixture projects (`crate::testkit`)
//! planned by the core (`export_plan`, `frame_plan`, `media_argv`), run with
//! real ffmpeg through `FfmpegRunner`, and checked frame by frame and sample
//! by sample. Every test works in a `TestEnv` temp project. A few tests run
//! hand-written command lines to reach failure paths a planner never takes.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use qrec_core::export_plan::{ExportOptions, plan_export};
use qrec_core::frame_plan::plan_frame;
use qrec_core::hash::sha256_hex;
use qrec_core::media_argv::{
    ffmpeg_prelude, file_arg, probe_argv, segment_check_argv, silence_argv,
};
use qrec_core::model::{Clip, Project, StreamKind};
use qrec_core::parse::ffprobe::parse_segment_check;
use qrec_core::plans::{ExportPlan, FramePlan, SegmentPlan, StitchPlan};
use qrec_core::protocol::{ExportFormat, Interval, ProbeJob, ProbePurpose, Quality, VideoCodec};
use qrec_core::state::{Capabilities, FfmpegCaps, SilenceKey, VaapiCaps};
use qrec_core::time::NS_PER_SEC;
use qrec_core::{ClipId, Event, JobId, LayerId, TakeId};
use serde_json::{Value, json};

use super::{FfmpegRunner, MediaOptions, PART_MARKER, housekeeping};
use crate::deps::MediaRunner;
use crate::msg::{Inbox, Msg};
use crate::testkit::{MediaSpec, TestEnv, ffprobe, fixture_project, run_tool, write_project};

const SEGMENTS: &str = ".qrec/cache/segments";
const WAIT: Duration = Duration::from_secs(180);

fn options() -> MediaOptions {
    MediaOptions {
        max_parallel: 3,
        kill_grace: Duration::from_millis(500),
        ..MediaOptions::default()
    }
}

fn runner(dir: &Path) -> FfmpegRunner {
    FfmpegRunner::with_options(dir, options())
}

/// Events until `last` matches one (inclusive).
fn events_until(
    rx: &Receiver<Msg>,
    timeout: Duration,
    last: impl Fn(&Event) -> bool,
) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Msg::Event(ev)) => {
                let done = last(&ev);
                events.push(ev);
                if done {
                    return events;
                }
            }
            Ok(other) => panic!("unexpected message {other:?}"),
            Err(_) => panic!("no final event within {timeout:?}; got {events:#?}"),
        }
    }
}

fn export_ended(ev: &Event) -> bool {
    matches!(
        ev,
        Event::ExportFinished { .. }
            | Event::ExportFailed { .. }
            | Event::ExportCancelled { .. }
            | Event::OutputAppeared { .. }
    )
}

fn frame_ended(ev: &Event) -> bool {
    matches!(
        ev,
        Event::FrameSaved { .. } | Event::FrameFailed { .. } | Event::OutputAppeared { .. }
    )
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

fn temps_in(dir: &Path) -> Vec<String> {
    names_in(dir)
        .into_iter()
        .filter(|n| n.contains(PART_MARKER))
        .collect()
}

fn logs_matching(dir: &Path, part: &str) -> Vec<String> {
    names_in(&dir.join(".qrec/logs"))
        .into_iter()
        .filter(|n| n.contains(part))
        .collect()
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

fn samples_of(frames: u64, num: u32, den: u32) -> u64 {
    let n = u128::from(frames) * 48_000 * u128::from(den);
    let d = u128::from(num);
    ((2 * n + d) / (2 * d)) as u64
}

/// Runs a tool and returns its stdout.
fn tool(argv: &[String]) -> String {
    let out = run_tool(
        std::process::Command::new(&argv[0]).args(&argv[1..]),
        Duration::from_secs(60),
    );
    assert!(out.status.success(), "{argv:?}: {}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

/// A segment's video packets and PCM samples, checked like the runner does.
fn segment_counts(file: &Path) -> (u64, u64) {
    let counts = parse_segment_check(&tool(&segment_check_argv(&file.to_string_lossy()))).unwrap();
    (counts.video_packets, counts.audio_samples)
}

/// An export's video packets and decoded audio samples.
fn output_counts(file: &Path) -> (u64, u64) {
    let probe = ffprobe(file);
    let video = probe["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == "video")
        .cloned()
        .unwrap_or(Value::Null);
    let packets = video["nb_read_packets"].as_str().unwrap().parse().unwrap();
    let samples = tool(&strings(&[
        "ffprobe",
        "-v",
        "error",
        "-select_streams",
        "a:0",
        "-show_entries",
        "frame=nb_samples",
        "-of",
        "csv=p=0",
        &file_arg(&file.to_string_lossy()),
    ]))
    .lines()
    .filter_map(|l| l.trim().trim_end_matches(',').parse::<u64>().ok())
    .sum();
    (packets, samples)
}

fn duration_secs(file: &Path) -> f64 {
    ffprobe(file)["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

// ------------------------------------------------------------ planning

/// The ffmpeg on PATH, canonicalized: what capability detection reports.
fn ffmpeg_identity() -> String {
    let found = which::which("ffmpeg").expect("ffmpeg on PATH");
    fs::canonicalize(&found)
        .unwrap_or(found)
        .to_string_lossy()
        .into_owned()
}

/// ffmpeg detected (no encoder list: the planner doesn't check it), and a
/// VA-API node if given.
fn caps(vaapi_device: Option<&str>) -> Capabilities {
    Capabilities {
        ffmpeg: Some(FfmpegCaps {
            version: "8.1.2".to_owned(),
            identity: ffmpeg_identity(),
            encoders: BTreeSet::new(),
        }),
        ffprobe: Some("ffprobe".to_owned()),
        vaapi: vaapi_device.map(|device| VaapiCaps {
            device: device.to_owned(),
            vendor: 0x8086,
            ffmpeg_h264: true,
            gst_h264_factory: None,
        }),
        cores: 4,
    }
}

/// A draft MP4 export plan from the core.
fn export_plan(project: &Project, out: &str, vaapi_device: Option<&str>) -> ExportPlan {
    let opts = ExportOptions {
        format: ExportFormat::Mp4,
        quality: Quality::Draft,
        codec: VideoCodec::H264,
        out: out.to_owned(),
        allow_vaapi: vaapi_device.is_some(),
    };
    plan_export(project, &caps(vaapi_device), &opts).expect("a plan")
}

fn load(dir: &Path) -> Project {
    serde_json::from_str(&fs::read_to_string(dir.join("qrec.json")).unwrap()).unwrap()
}

/// Three takes: video + mono mic; system audio only; video + stereo audio
/// that started 100 ms and 50 ms after T0. Timeline: take 1 split in two
/// continuous clips, take 2, a trimmed take 3, take 2 again.
fn three_take_project(dir: &Path) -> Project {
    let specs = [
        MediaSpec::video(320, 180, 30, 60).with_audio(48_000, 1, &[(0, 500), (1500, 2000)]),
        MediaSpec::audio(48_000, 2, 1500, &[(100, 900)]),
        MediaSpec::video(320, 180, 30, 45).with_audio(48_000, 2, &[(200, 1200)]),
    ];
    let mut json = fixture_project(dir, &specs);
    let take3 = &mut json["takes"]["take-0003"];
    take3["streams"][0]["startOffsetNs"] = json!(100_000_000);
    take3["streams"][1]["startOffsetNs"] = json!(50_000_000);
    take3["durationFrames"] = json!(48); // 3 frames of offset + 45 (A§2.2)
    json["timeline"]["clips"] = json!([
        { "id": "c001", "take": "take-0001", "in": 0, "out": 20 },
        { "id": "c004", "take": "take-0001", "in": 20, "out": 60 },
        { "id": "c002", "take": "take-0002", "in": 0, "out": 45 },
        { "id": "c003", "take": "take-0003", "in": 10, "out": 40 },
        { "id": "c005", "take": "take-0002", "in": 0, "out": 45 },
    ]);
    write_project(dir, &json);
    serde_json::from_value(json).expect("a valid project")
}

// ---------------------------------------------------------------- export

#[test]
fn exports_three_takes_with_splits_offsets_and_a_repeat() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let project = three_take_project(&dir);
    let plan = export_plan(&project, "exports/swamp-20261014-120000.mp4", None);
    assert_eq!(plan.segments.len(), 4, "the split clips render as one unit");
    assert_eq!(plan.total_frames, 60 + 45 + 30 + 45);
    assert_eq!(plan.segments[1].key, plan.segments[3].key, "take 2 twice");

    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.render_export(JobId(1), plan.clone(), inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    let Some(Event::ExportFinished { job, path, bytes }) = events.last() else {
        panic!("{events:#?}");
    };
    assert_eq!(*job, JobId(1));
    let out = dir.join("exports/swamp-20261014-120000.mp4");
    assert_eq!(Path::new(path), out);
    assert_eq!(*bytes, fs::metadata(&out).unwrap().len());
    assert!(!runner.is_running(JobId(1)));

    // Frame-exact output: every unit's frames, 6 s of audio (the AAC
    // encoder's padding is at most one frame) and video.
    let (frames, samples) = output_counts(&out);
    assert_eq!(frames, 180);
    assert!(samples.abs_diff(288_000) <= 1024, "{samples} samples");
    let duration = duration_secs(&out);
    assert!((duration - 6.0).abs() < 0.05, "{duration}");

    // Every segment reported its full frame count, then the stitch ran.
    for (i, segment) in plan.segments.iter().enumerate() {
        let last = events.iter().rev().find_map(|e| match e {
            Event::ExportProgress {
                segment: s,
                frames_done,
                ..
            } if *s == i => Some(*frames_done),
            _ => None,
        });
        assert_eq!(last, Some(segment.frames), "segment {i}");
    }
    let first_stitch = events
        .iter()
        .position(|e| matches!(e, Event::ExportStitchProgress { .. }))
        .expect("stitch progress");
    assert!(
        events[first_stitch..]
            .iter()
            .all(|e| !matches!(e, Event::ExportProgress { .. })),
        "segment progress after the stitch started"
    );

    // Three unique segments cached with exactly their frames and samples;
    // nothing temporary left anywhere.
    let cached = names_in(&dir.join(SEGMENTS));
    assert_eq!(cached.len(), 3, "{cached:?}");
    for segment in &plan.segments {
        assert_eq!(
            segment_counts(&dir.join(&segment.path)),
            (segment.frames, segment.samples),
            "{}",
            segment.path
        );
    }
    assert_eq!(plan.segments[2].samples, 48_000, "30 frames at 30 fps");
    let seg_logs = logs_matching(&dir, "-j1-seg");
    assert_eq!(
        seg_logs.len(),
        3,
        "the repeated unit renders once: {seg_logs:?}"
    );
    for log in &seg_logs {
        let text = fs::read_to_string(dir.join(".qrec/logs").join(log)).unwrap();
        assert!(text.contains("# qrec: verify:"), "{log}");
    }
    assert!(temps_in(&dir.join("exports")).is_empty());
    assert_eq!(
        names_in(&dir.join("exports")),
        ["swamp-20261014-120000.mp4"]
    );

    // Re-export: every segment comes from the cache; never overwrite.
    runner.render_export(JobId(2), plan.clone(), inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    let Some(Event::ExportFinished { path, .. }) = events.last() else {
        panic!("{events:#?}");
    };
    assert_eq!(
        Path::new(path),
        dir.join("exports/swamp-20261014-120000-2.mp4")
    );
    assert!(
        logs_matching(&dir, "-j2-seg").is_empty(),
        "nothing re-rendered"
    );
    assert_eq!(output_counts(Path::new(path)).0, 180);

    // Losing one cache file re-renders only that unit.
    fs::remove_file(dir.join(&plan.segments[2].path)).unwrap();
    runner.render_export(JobId(3), plan.clone(), inbox);
    let events = events_until(&rx, WAIT, export_ended);
    assert!(
        matches!(events.last(), Some(Event::ExportFinished { .. })),
        "{events:#?}"
    );
    assert_eq!(
        logs_matching(&dir, "-j3-seg"),
        [logs_matching(&dir, "-j3-seg2")[0].clone()]
    );
    assert_eq!(
        names_in(&dir.join("exports")),
        [
            "swamp-20261014-120000-2.mp4",
            "swamp-20261014-120000-3.mp4",
            "swamp-20261014-120000.mp4"
        ]
    );
}

/// The core's own flow: `apply(Export)` → `RenderExport` → this runner →
/// `on_event` for every event → the requester's `ExportDone` reply.
#[test]
fn the_core_export_and_frame_flows_finish_through_the_runner() {
    use qrec_core::ctx::{Ctx, Facts};
    use qrec_core::protocol::{Command, Effect, ExportDone, FrameSavedView};
    use qrec_core::state::{History, HostKind, State, UiState};
    use qrec_core::{ReqId, TimeArg, apply, on_event};

    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let project = three_take_project(&dir);
    let mut state = State::new(project, History::new(), UiState::default(), HostKind::Cli);
    state.runtime.capabilities = caps(None);
    let ctx = Ctx {
        now: "2026-10-14T12:00:00Z".parse().unwrap(),
        utc_offset_secs: 0,
        entropy: 7,
        confirmed: false,
        req: Some(ReqId("r1".to_owned())),
        facts: Facts::default(),
    };
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();

    // Runs one job's effects through the runner and its events through the core.
    let mut drive = |state: &mut State, cmd: Command| -> (serde_json::Value, usize) {
        let outcome = apply(state, &ctx, cmd).expect("accepted");
        *state = outcome.state;
        for effect in outcome.effects {
            match effect {
                Effect::RenderExport { job, plan } => {
                    runner.render_export(job, plan, inbox.clone())
                }
                Effect::SaveFrame { job, plan } => runner.save_frame(job, plan, inbox.clone()),
                _ => {}
            }
        }
        let mut progress = 0;
        loop {
            let Ok(Msg::Event(ev)) = rx.recv_timeout(WAIT) else {
                panic!("no event");
            };
            let out = on_event(state, &ctx, ev);
            *state = out.state;
            for effect in out.effects {
                match effect {
                    Effect::Progress { .. } => progress += 1,
                    Effect::Reply { reply, .. } => {
                        return (reply.expect("finished").result, progress);
                    }
                    _ => {}
                }
            }
        }
    };

    let export = Command::Export {
        format: ExportFormat::Mp4,
        quality: Quality::Draft,
        codec: None,
        out: None,
    };
    let (result, progress) = drive(&mut state, export);
    let done: ExportDone = serde_json::from_value(result).unwrap();
    assert_eq!(
        Path::new(&done.path),
        dir.join("exports/swamp-20261014-120000.mp4")
    );
    assert_eq!(done.frames, 180);
    assert_eq!(done.bytes, fs::metadata(&done.path).unwrap().len());
    assert!(progress > 0, "the requester saw progress");
    assert!(state.runtime.jobs.is_empty());

    // Timeline frame 70 (00:00:02:10) is take 2's frame 10.
    let frame = Command::FrameSave {
        at: TimeArg::frames(70),
        layer: None,
        out: None,
    };
    let (result, _) = drive(&mut state, frame);
    let saved: FrameSavedView = serde_json::from_value(result).unwrap();
    assert_eq!(
        Path::new(&saved.path),
        dir.join("frames/swamp-take-0002-00000210.png")
    );
    assert_eq!(
        ffprobe(Path::new(&saved.path))["streams"][0]["codec_name"],
        "png"
    );
    assert!(state.runtime.jobs.is_empty());
}

#[test]
fn a_split_renders_nothing_new() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let project = three_take_project(&dir);
    let plan = export_plan(&project, "exports/a.mp4", None);
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.render_export(JobId(1), plan.clone(), inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    assert!(
        matches!(events.last(), Some(Event::ExportFinished { .. })),
        "{events:#?}"
    );

    // Split c004 at take frame 40: the same render units, the same keys.
    let mut split = project.clone();
    let clip = split.timeline.clips[1].clone();
    split.timeline.clips[1].out = 40;
    split.timeline.clips.insert(
        2,
        Clip {
            id: ClipId::from_raw("c0ff"),
            in_: 40,
            ..clip
        },
    );
    let split_plan = export_plan(&split, "exports/a.mp4", None);
    let keys = |p: &ExportPlan| p.segments.iter().map(|s| s.key.clone()).collect::<Vec<_>>();
    assert_eq!(keys(&split_plan), keys(&plan));
    runner.render_export(JobId(2), split_plan, inbox);
    let events = events_until(&rx, WAIT, export_ended);
    let Some(Event::ExportFinished { path, .. }) = events.last() else {
        panic!("{events:#?}");
    };
    assert!(logs_matching(&dir, "-j2-seg").is_empty());
    assert_eq!(Path::new(path), dir.join("exports/a-2.mp4"));
}

#[test]
fn a_failed_va_segment_falls_back_to_libx264() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    fixture_project(
        &dir,
        &[MediaSpec::video(320, 180, 30, 30).with_audio(48_000, 1, &[(0, 1000)])],
    );
    // A node that doesn't exist: the VA chain fails at once and no real
    // device is ever opened.
    let device = "/dev/dri/qrec-no-such-node";
    let plan = export_plan(&load(&dir), "exports/va.mp4", Some(device));
    assert!(
        plan.segments[0]
            .argv
            .contains(&format!("vaapi=va:{device}")),
        "a VA plan"
    );
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.render_export(JobId(4), plan.clone(), inbox);
    let events = events_until(&rx, WAIT, export_ended);
    let Some(Event::ExportFinished { path, .. }) = events.last() else {
        panic!("{events:#?}");
    };
    assert_eq!(Path::new(path), dir.join("exports/va.mp4"));
    assert_eq!(output_counts(Path::new(path)).0, 30);
    // The VA render failed and was thrown away; libx264 rendered it again
    // under another key. Nothing VA-made was cached.
    let cached = names_in(&dir.join(SEGMENTS));
    assert_eq!(cached.len(), 1, "{cached:?}");
    assert_ne!(cached[0], format!("{}.mov", plan.segments[0].key));
    assert_eq!(logs_matching(&dir, "-j4-seg0").len(), 2, "VA, then libx264");
    assert_eq!(logs_matching(&dir, "-j4-export").len(), 2);
    assert!(temps_in(&dir.join(SEGMENTS)).is_empty());
    assert!(temps_in(&dir.join("exports")).is_empty());
}

/// A hand-picked output is replaced only when the plan says the person
/// confirmed it (`replace`). A file that appears at an unconfirmed path
/// while the job runs is kept, and so is the result, next to it:
/// `OutputAppeared` names both (the core makes it `confirmation_required`,
/// A§8.8). Whether a file was there when the job started doesn't matter.
#[test]
fn hand_picked_outputs_are_replaced_only_when_confirmed() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    fixture_project(
        &dir,
        &[MediaSpec::video(320, 180, 30, 30).with_audio(48_000, 1, &[])],
    );
    let out = env.root().join("mine.mp4");
    let plan = export_plan(&load(&dir), out.to_str().unwrap(), None);
    assert!(!plan.replace);
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();

    runner.render_export(JobId(1), plan.clone(), inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    assert!(
        matches!(events.last(), Some(Event::ExportFinished { path, .. }) if Path::new(path) == out),
        "{events:#?}"
    );
    // Someone else's file at the unconfirmed path: kept, and so is ours.
    fs::write(&out, "someone else's").unwrap();
    runner.render_export(JobId(2), plan.clone(), inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    let Some(Event::OutputAppeared {
        job,
        out: appeared,
        bytes,
        kept,
    }) = events.last()
    else {
        panic!("{events:#?}");
    };
    assert_eq!(
        (*job, Path::new(appeared), *bytes),
        (JobId(2), out.as_path(), 14)
    );
    assert_eq!(fs::read_to_string(&out).unwrap(), "someone else's");
    let kept = PathBuf::from(kept);
    assert_eq!(kept.parent(), Some(env.root()));
    assert!(
        kept.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".mine.qrec-part-"),
        "{}",
        kept.display()
    );
    assert_eq!(output_counts(&kept).0, 30, "the export was kept whole");
    assert_eq!(
        temps_in(env.root()),
        [kept.file_name().unwrap().to_string_lossy()]
    );
    fs::remove_file(&kept).unwrap();

    // Confirmed (`replace`): the file is replaced.
    let confirmed = ExportPlan {
        replace: true,
        ..plan
    };
    runner.render_export(JobId(3), confirmed, inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    assert!(
        matches!(events.last(), Some(Event::ExportFinished { path, .. }) if Path::new(path) == out),
        "{events:#?}"
    );
    assert_eq!(output_counts(&out).0, 30);
    assert!(temps_in(env.root()).is_empty());

    // Frame saves follow the same rule.
    let png = env.root().join("mine.png");
    fs::write(&png, "a drawing").unwrap();
    let frame = plan_frame(&load(&dir), 1, None, png.to_str().unwrap()).unwrap();
    runner.save_frame(JobId(4), frame.clone(), inbox.clone());
    let events = events_until(&rx, WAIT, frame_ended);
    let Some(Event::OutputAppeared {
        out: appeared,
        bytes: 9,
        kept,
        ..
    }) = events.last()
    else {
        panic!("{events:#?}");
    };
    assert_eq!(Path::new(appeared), png);
    assert_eq!(fs::read_to_string(&png).unwrap(), "a drawing");
    assert_eq!(&fs::read(kept).unwrap()[..4], b"\x89PNG");
    fs::remove_file(kept).unwrap();
    runner.save_frame(
        JobId(5),
        FramePlan {
            replace: true,
            ..frame
        },
        inbox,
    );
    let events = events_until(&rx, WAIT, frame_ended);
    assert!(
        matches!(events.last(), Some(Event::FrameSaved { path, .. }) if Path::new(path) == png),
        "{events:#?}"
    );
    assert_eq!(&fs::read(&png).unwrap()[..4], b"\x89PNG");
    assert!(temps_in(env.root()).is_empty());
}

/// A segment from a raw ffmpeg command line (no planner involved).
fn raw_segment(body: &[&str], frames: u64) -> SegmentPlan {
    let mut argv = ffmpeg_prelude(true);
    argv.extend(strings(body));
    let key = sha256_hex(argv.join("\0").as_bytes());
    let path = format!("{SEGMENTS}/{key}.mov");
    argv.extend(["-f".to_owned(), "mov".to_owned(), file_arg(&path)]);
    SegmentPlan {
        clips: vec![ClipId::from_raw("c001")],
        take: TakeId::numbered(1),
        in_: 0,
        out: frames,
        frames,
        samples: samples_of(frames, 30, 1),
        key,
        path,
        argv,
    }
}

fn raw_plan(segment: SegmentPlan, out: &str) -> ExportPlan {
    let mut list_content = "ffconcat version 1.0\n".to_owned();
    list_content.push_str(&format!("file '{}.mov'\n", segment.key));
    let list_path = format!(
        "{SEGMENTS}/stitch-{}.ffconcat",
        &sha256_hex(list_content.as_bytes())[..16]
    );
    let mut argv = ffmpeg_prelude(true);
    argv.extend(strings(&["-f", "concat", "-i"]));
    argv.push(file_arg(&list_path));
    argv.extend(strings(&["-c", "copy", "-f", "mp4"]));
    argv.push(file_arg(out));
    ExportPlan {
        out: out.to_owned(),
        replace: false,
        tmp_ext: "mp4".to_owned(),
        total_frames: segment.frames,
        files: BTreeSet::new(),
        segments: vec![segment],
        stitch: StitchPlan {
            list_path,
            list_content,
            argv,
        },
    }
}

#[test]
fn cancel_stops_ffmpeg_and_deletes_temp_files() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    fs::create_dir_all(dir.join("exports")).unwrap();
    // An endless, real-time render: only a cancel ends it.
    let segment = raw_segment(
        &[
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=30",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=stereo",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "pcm_s16le",
        ],
        300,
    );
    let plan = raw_plan(segment.clone(), "exports/never.mp4");
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.render_export(JobId(7), plan, inbox);

    // Wait until ffmpeg reports progress (it writes its temp segment), then cancel.
    let first = events_until(&rx, WAIT, |e| {
        matches!(e, Event::ExportProgress { .. }) || export_ended(e)
    });
    assert!(
        matches!(first.last(), Some(Event::ExportProgress { segment: 0, .. })),
        "{first:#?}"
    );
    let segments = dir.join(SEGMENTS);
    assert_eq!(temps_in(&segments).len(), 1, "{:?}", names_in(&segments));
    assert!(runner.is_running(JobId(7)));
    let cancelled_at = Instant::now();
    runner.cancel(JobId(7));
    let events = events_until(&rx, Duration::from_secs(20), export_ended);
    assert_eq!(
        events.last(),
        Some(&Event::ExportCancelled { job: JobId(7) })
    );
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(5),
        "{:?}",
        cancelled_at.elapsed()
    );
    assert!(!runner.is_running(JobId(7)));
    assert!(temps_in(&segments).is_empty(), "{:?}", names_in(&segments));
    assert!(
        !dir.join(&segment.path).exists(),
        "an unfinished segment was cached"
    );
    assert!(names_in(&dir.join("exports")).is_empty());
    // Cancelling again (or an unknown job) is harmless.
    runner.cancel(JobId(7));
    runner.cancel(JobId(99));
}

#[test]
fn a_failing_segment_reports_the_ffmpeg_error_and_its_log() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let segment = raw_segment(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=duration=1",
            "-vf",
            "qrecnosuchfilter",
        ],
        30,
    );
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.render_export(JobId(3), raw_plan(segment, "exports/x.mp4"), inbox);
    let events = events_until(&rx, WAIT, export_ended);
    let Some(Event::ExportFailed {
        message,
        stderr_tail,
        log,
        ..
    }) = events.last()
    else {
        panic!("{events:#?}");
    };
    assert!(
        message.starts_with("segment 1/1 (take-0001, frames 0–30): ffmpeg exited with status"),
        "{message}"
    );
    assert!(
        stderr_tail.iter().any(|l| l.contains("qrecnosuchfilter")),
        "{stderr_tail:#?}"
    );
    assert!(stderr_tail.len() <= 30);
    assert!(
        log.contains("/.qrec/logs/") && log.ends_with("-j3-seg0.log"),
        "{log}"
    );
    assert!(Path::new(log).is_file());
    assert!(temps_in(&dir.join(SEGMENTS)).is_empty());
    assert!(temps_in(&dir.join("exports")).is_empty());
}

#[test]
fn a_segment_with_the_wrong_counts_fails_the_export() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    // 30 frames rendered where the plan expects 31.
    let mut segment = raw_segment(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x36:rate=30:duration=1",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=stereo",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-t",
            "1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "pcm_s16le",
        ],
        30,
    );
    segment.frames = 31;
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.render_export(JobId(5), raw_plan(segment.clone(), "exports/x.mp4"), inbox);
    let events = events_until(&rx, WAIT, export_ended);
    let Some(Event::ExportFailed { message, .. }) = events.last() else {
        panic!("{events:#?}");
    };
    assert!(
        message.contains("30 video frames instead of 31"),
        "{message}"
    );
    assert!(
        !dir.join(&segment.path).exists(),
        "a bad segment was cached"
    );
    assert!(temps_in(&dir.join(SEGMENTS)).is_empty());
}

#[test]
fn missing_sources_and_programs_fail_cleanly() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();

    let segment = raw_segment(&["-i", "file:takes/take-0001/gone.mkv"], 30);
    runner.render_export(JobId(1), raw_plan(segment, "exports/x.mp4"), inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    assert!(
        matches!(events.last(), Some(Event::ExportFailed { message, .. })
            if message.contains("takes/take-0001/gone.mkv is missing")),
        "{events:#?}"
    );

    let mut segment = raw_segment(&["-f", "lavfi", "-i", "testsrc2"], 30);
    segment.argv[0] = "qrec-no-such-ffmpeg".to_owned();
    runner.render_export(JobId(2), raw_plan(segment, "exports/x.mp4"), inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    assert!(
        matches!(events.last(), Some(Event::ExportFailed { message, .. })
            if message.contains("qrec-no-such-ffmpeg wasn't found")),
        "{events:#?}"
    );

    // A plan that would write into a folder that doesn't exist.
    let segment = raw_segment(&["-f", "lavfi", "-i", "testsrc2"], 30);
    runner.render_export(
        JobId(3),
        raw_plan(segment, "/qrec-no-such-dir/x.mp4"),
        inbox,
    );
    let events = events_until(&rx, WAIT, export_ended);
    assert!(
        matches!(events.last(), Some(Event::ExportFailed { message, .. })
            if message.contains("/qrec-no-such-dir doesn't exist")),
        "{events:#?}"
    );
}

// ----------------------------------------------------------------- frames

#[test]
fn frames_get_new_names_and_are_pngs() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    fixture_project(&dir, &[MediaSpec::video(320, 180, 30, 30)]);
    let project = load(&dir);
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    let out = "frames/swamp-take-0001-00000001.png";
    for (job, expected) in [
        (1, "swamp-take-0001-00000001.png"),
        (2, "swamp-take-0001-00000001-2.png"),
    ] {
        let plan = plan_frame(&project, 1, None, out).unwrap();
        runner.save_frame(JobId(job), plan, inbox.clone());
        let events = events_until(&rx, WAIT, frame_ended);
        assert_eq!(
            events.last(),
            Some(&Event::FrameSaved {
                job: JobId(job),
                path: dir
                    .join("frames")
                    .join(expected)
                    .to_string_lossy()
                    .into_owned(),
            })
        );
    }
    // The raw frame of one layer, to a hand-picked path.
    let raw = env.root().join("raw.png");
    let plan = plan_frame(
        &project,
        1,
        Some(&LayerId::numbered(1)),
        raw.to_str().unwrap(),
    )
    .unwrap();
    runner.save_frame(JobId(3), plan, inbox);
    let events = events_until(&rx, WAIT, frame_ended);
    assert!(
        matches!(events.last(), Some(Event::FrameSaved { path, .. }) if Path::new(path) == raw),
        "{events:#?}"
    );
    for png in [dir.join("frames").join("swamp-take-0001-00000001.png"), raw] {
        let probe = ffprobe(&png);
        assert_eq!(probe["streams"][0]["codec_name"], "png");
        assert_eq!(
            (
                probe["streams"][0]["width"].as_u64(),
                probe["streams"][0]["height"].as_u64()
            ),
            (Some(320), Some(180))
        );
    }
    assert!(temps_in(&dir.join("frames")).is_empty());
    assert!(temps_in(env.root()).is_empty());
}

#[test]
fn a_failed_frame_save_reports_its_log() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let mut argv = ffmpeg_prelude(true);
    argv.extend(strings(&[
        "-i",
        "file:takes/take-0001/L1-screen-FAKE-1.mkv",
        "-frames:v",
        "1",
        "-f",
        "image2",
        "-update",
        "1",
    ]));
    argv.push(file_arg("frames/x.png"));
    let plan = FramePlan {
        out: "frames/x.png".to_owned(),
        replace: false,
        tmp_ext: "png".to_owned(),
        argv,
        files: BTreeSet::new(),
    };
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.save_frame(JobId(4), plan, inbox);
    let events = events_until(&rx, WAIT, frame_ended);
    let Some(Event::FrameFailed { message, log, .. }) = events.last() else {
        panic!("{events:#?}");
    };
    assert!(
        message.contains("L1-screen-FAKE-1.mkv is missing"),
        "{message}"
    );
    assert!(log.ends_with("-j4-frame.log"), "{log}");
}

// ----------------------------------------------------------------- probes

#[test]
fn probing_a_missing_file_is_an_error_result() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    let file = "takes/take-0001/L1-screen-DP-4.mkv";
    let jobs = vec![ProbeJob {
        id: "L1".into(),
        file: file.into(),
        kind: StreamKind::Video,
        argv: probe_argv(file, StreamKind::Video),
    }];
    let take = TakeId::numbered(1);
    runner.probe(take.clone(), ProbePurpose::Recover, jobs, inbox);
    let events = events_until(&rx, WAIT, |e| matches!(e, Event::TakeProbed { .. }));
    let Some(Event::TakeProbed {
        take: t,
        purpose,
        results,
    }) = events.last()
    else {
        panic!("{events:#?}");
    };
    assert_eq!((t, *purpose), (&take, ProbePurpose::Recover));
    assert_eq!(results.len(), 1);
    assert_eq!(
        (results[0].id.as_str(), results[0].file.as_str()),
        ("L1", file)
    );
    assert_eq!(results[0].info, Err(format!("{file} is missing")));
}

#[test]
fn probes_fixture_media() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    fixture_project(
        &dir,
        &[MediaSpec::video(320, 180, 30, 60).with_audio(48_000, 1, &[(0, 500)])],
    );
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    let job = |id: &str, file: &str, kind| ProbeJob {
        id: id.into(),
        file: file.into(),
        kind,
        argv: probe_argv(file, kind),
    };
    let jobs = vec![
        job(
            "L1",
            "takes/take-0001/L1-screen-FAKE-1.mkv",
            StreamKind::Video,
        ),
        job("A1", "takes/take-0001/A1-mic.mka", StreamKind::Audio),
        job("L2", "takes/take-0001/L2-camera-0.mkv", StreamKind::Video),
    ];
    runner.probe(TakeId::numbered(1), ProbePurpose::Finalize, jobs, inbox);
    let events = events_until(&rx, WAIT, |e| matches!(e, Event::TakeProbed { .. }));
    let Some(Event::TakeProbed { results, .. }) = events.last() else {
        panic!("{events:#?}");
    };
    let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["L1", "A1", "L2"], "in the jobs' order");
    let video = results[0].info.as_ref().unwrap();
    assert_eq!(
        (video.frames, video.width, video.height),
        (Some(60), Some(320), Some(180))
    );
    let audio = results[1].info.as_ref().unwrap();
    assert_eq!((audio.sample_rate, audio.channels), (Some(48_000), Some(1)));
    assert!(results[2].info.is_err());
    assert_eq!(logs_matching(&dir, "-probe-take-0001-").len(), 2);
}

// ---------------------------------------------------------------- silence

fn silence_job(dir: &Path, threshold_db: f64, min_ms: u32) -> (SilenceKey, Vec<String>) {
    let file = "takes/take-0001/A1-mic.mka";
    let meta = fs::metadata(dir.join(file)).unwrap();
    let mtime = {
        use std::os::unix::fs::MetadataExt;
        meta.mtime() * NS_PER_SEC + meta.mtime_nsec()
    };
    (
        SilenceKey::compute(file, meta.len(), mtime, threshold_db, min_ms),
        silence_argv(file, threshold_db, min_ms),
    )
}

#[test]
fn cached_silences_are_reused_without_ffmpeg() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let key = SilenceKey("ab".repeat(32));
    let cached = vec![Interval {
        start_ns: 500_000_000,
        end_ns: 1_500_000_000,
    }];
    let cache_dir = dir.join(".qrec/cache/silence");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(
        cache_dir.join(format!("{}.json", key.0)),
        serde_json::to_vec(&json!({ "version": 1, "key": key.0, "silences": cached })).unwrap(),
    )
    .unwrap();
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    // The argv would fail: the input doesn't exist. The cache answers first.
    runner.detect_silence(
        JobId(5),
        key.clone(),
        silence_argv("takes/gone.mka", -35.0, 400),
        inbox.clone(),
    );
    let events = events_until(&rx, WAIT, |_| true);
    assert_eq!(
        events,
        [Event::SilenceDetected {
            job: JobId(5),
            key: key.clone(),
            silences: cached,
        }]
    );
    assert!(logs_matching(&dir, "-j5-").is_empty(), "ffmpeg didn't run");

    // A corrupt cache file (or another key's) is ignored: ffmpeg runs and fails.
    let other = SilenceKey("cd".repeat(32));
    fs::write(cache_dir.join(format!("{}.json", other.0)), "{oops").unwrap();
    runner.detect_silence(
        JobId(6),
        other.clone(),
        silence_argv("takes/gone.mka", -35.0, 400),
        inbox,
    );
    let events = events_until(&rx, WAIT, |_| true);
    let [Event::SilenceFailed { job, key, message }] = events.as_slice() else {
        panic!("{events:#?}");
    };
    assert_eq!((*job, key), (JobId(6), &other));
    assert!(message.contains("ffmpeg exited with status"), "{message}");
    assert!(message.contains("-j6-silence.log"), "{message}");
}

#[test]
fn silence_detection_finds_the_fixture_gaps_and_caches_them() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    // Speech 0–0.5 s and 1.5–2 s of 2.5 s: a gap, then a trailing silence.
    fixture_project(
        &dir,
        &[MediaSpec::audio(48_000, 1, 2500, &[(0, 500), (1500, 2000)])],
    );
    let (key, argv) = silence_job(&dir, -35.0, 400);
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.detect_silence(JobId(1), key.clone(), argv.clone(), inbox.clone());
    let events = events_until(&rx, WAIT, |_| true);
    let [Event::SilenceDetected { silences, .. }] = events.as_slice() else {
        panic!("{events:#?}");
    };
    let ms = |ns: i64| (ns as f64 / 1e6).round() as i64;
    let found: Vec<(i64, i64)> = silences
        .iter()
        .map(|i| (ms(i.start_ns), ms(i.end_ns)))
        .collect();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        (found[0].0 - 500).abs() <= 10 && (found[0].1 - 1500).abs() <= 10,
        "{found:?}"
    );
    // A trailing silence ends at the file's end.
    assert!(
        (found[1].0 - 2000).abs() <= 10 && (found[1].1 - 2500).abs() <= 10,
        "{found:?}"
    );
    assert!(
        dir.join(format!(".qrec/cache/silence/{}.json", key.0))
            .is_file()
    );

    // Again (another job): straight from the cache.
    runner.detect_silence(JobId(2), key, argv, inbox);
    let again = events_until(&rx, WAIT, |_| true);
    assert!(matches!(&again[0], Event::SilenceDetected { silences: s, .. } if s == silences));
    assert!(logs_matching(&dir, "-j2-").is_empty());
}

#[test]
fn a_cancelled_analysis_reports_cancelled() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    // An endless analysis: only a cancel ends it.
    let mut argv = ffmpeg_prelude(false);
    argv.extend(strings(&[
        "-re",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440",
        "-af",
        "silencedetect=noise=-35dB:d=0.4",
        "-f",
        "null",
        "-",
    ]));
    let key = SilenceKey("ef".repeat(32));
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.detect_silence(JobId(8), key.clone(), argv, inbox);
    std::thread::sleep(Duration::from_millis(300));
    assert!(runner.is_running(JobId(8)));
    runner.cancel(JobId(8));
    let events = events_until(&rx, Duration::from_secs(20), |_| true);
    assert_eq!(
        events,
        [Event::SilenceFailed {
            job: JobId(8),
            key,
            message: "cancelled".to_owned(),
        }]
    );
    assert!(!runner.is_running(JobId(8)));
}

// ------------------------------------------------------------ purge, misc

#[test]
fn purge_reports_through_the_inbox() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    fixture_project(
        &dir,
        &[
            MediaSpec::audio(48_000, 1, 500, &[]),
            MediaSpec::audio(48_000, 1, 500, &[]),
        ],
    );
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.delete_take_dirs(JobId(9), vec![TakeId::numbered(2)], inbox);
    let events = events_until(&rx, WAIT, |_| true);
    assert_eq!(
        events,
        [Event::TakeDirsDeleted {
            job: JobId(9),
            deleted: vec![
                "takes/take-0002/A1-mic.mka".to_owned(),
                "takes/take-0002/".to_owned()
            ],
            errors: vec![],
        }]
    );
    assert!(dir.join("takes/take-0001/A1-mic.mka").is_file());
    assert!(!dir.join("takes/take-0002").exists());
}

#[test]
fn housekeeping_sweeps_old_temps_and_prunes_logs() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let two_hours_ago = std::time::SystemTime::now() - Duration::from_secs(7200);
    let mut old = Vec::new();
    for sub in ["exports", "frames", SEGMENTS, ".qrec/cache/silence"] {
        let d = dir.join(sub);
        fs::create_dir_all(&d).unwrap();
        let stale: PathBuf = d.join(format!(".x{PART_MARKER}1-1.mp4"));
        fs::write(&stale, "").unwrap();
        fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(two_hours_ago)
            .unwrap();
        old.push(stale);
        fs::write(d.join(format!(".y{PART_MARKER}1-2.mp4")), "").unwrap(); // fresh: kept
        fs::write(d.join("keep.mp4"), "").unwrap();
    }
    let logs = dir.join(".qrec/logs");
    fs::create_dir_all(&logs).unwrap();
    for i in 0..205 {
        fs::write(logs.join(format!("20261014-{i:06}-1-x.log")), "").unwrap();
    }
    let report = housekeeping(&dir);
    assert_eq!(report.logs_pruned, 5);
    let mut removed = report.temps_removed.clone();
    removed.sort();
    old.sort();
    assert_eq!(removed, old);
    for sub in ["exports", "frames", SEGMENTS] {
        assert_eq!(temps_in(&dir.join(sub)).len(), 1, "{sub}");
        assert!(dir.join(sub).join("keep.mp4").exists());
    }
    assert_eq!(names_in(&logs).len(), 200);
    assert_eq!(names_in(&logs)[0], "20261014-000005-1-x.log");
}
