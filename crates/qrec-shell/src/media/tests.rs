//! End-to-end tests of the media runner with real ffmpeg and fixture media
//! (`crate::testkit`). Every test works in a `TestEnv` temp project.
//!
//! The core's planners and parsers (`export_plan`, `frame_plan`,
//! `parse::{ffprobe, progress, silencedetect}`) are being finished by their
//! owners; tests that need them are `#[ignore]`d with the reason. Until then
//! the export tests use plans built here by hand from the A§9.3 rules.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use qrec_core::hash::sha256_hex;
use qrec_core::layout::place;
use qrec_core::media_argv::{ffmpeg_prelude, file_arg, probe_argv, silence_argv};
use qrec_core::model::{Project, StreamKind};
use qrec_core::plans::{ExportPlan, FramePlan, SegmentPlan, StitchPlan};
use qrec_core::protocol::{Interval, ProbeJob, ProbePurpose};
use qrec_core::state::SilenceKey;
use qrec_core::time::{NS_PER_MS, NS_PER_SEC, frame_ns, ns_to_frame_round};
use qrec_core::{ClipId, Event, JobId, TakeId};
use serde_json::{Value, json};

use super::{FfmpegRunner, MediaOptions, PART_MARKER, housekeeping};
use crate::deps::MediaRunner;
use crate::msg::{Inbox, Msg};
use crate::testkit::{MediaSpec, TestEnv, ffprobe, fixture_project, write_project};

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
fn events_until(rx: &Receiver<Msg>, timeout: Duration, last: impl Fn(&Event) -> bool) -> Vec<Event> {
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
        Event::ExportFinished { .. } | Event::ExportFailed { .. } | Event::ExportCancelled { .. }
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

/// `1.233333` (µs precision, like ffmpeg's time options).
fn seconds(ns: i64) -> String {
    let us = (ns + 500) / 1000;
    format!("{}.{:06}", us / 1_000_000, us % 1_000_000)
}

fn samples_of(frames: u64, num: u32, den: u32) -> u64 {
    let n = u128::from(frames) * 48_000 * u128::from(den);
    let d = u128::from(num);
    ((2 * n + d) / (2 * d)) as u64
}

// ------------------------------------------------------- a hand planner

/// A maximal run of continuous clips (same take, `out == next.in`).
struct Unit {
    clips: Vec<ClipId>,
    take: TakeId,
    in_: u64,
    out: u64,
}

fn units(project: &Project) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    for clip in &project.timeline.clips {
        match units.last_mut() {
            Some(u) if u.take == clip.take && u.out == clip.in_ => {
                u.out = clip.out;
                u.clips.push(clip.id.clone());
            }
            _ => units.push(Unit {
                clips: vec![clip.id.clone()],
                take: clip.take.clone(),
                in_: clip.in_,
                out: clip.out,
            }),
        }
    }
    units
}

/// The A§9.3 export plan for projects whose videos run at the canvas rate
/// (libx264 draft, MP4), built by hand while `export_plan` is a stub.
fn hand_plan(project: &Project, out: &str) -> ExportPlan {
    let canvas = &project.canvas;
    let (num, den) = (canvas.fps.num, canvas.fps.den);
    let mut segments = Vec::new();
    let mut files = BTreeSet::new();
    for unit in units(project) {
        let take = project.take(&unit.take).expect("the clip's take");
        let n = unit.out - unit.in_;
        let samples = samples_of(n, num, den);
        let mut argv = ffmpeg_prelude(true);
        let mut chains = vec![format!(
            "color=c={}:s={}x{}:r={num}/{den}[base]",
            canvas.background.ffmpeg_hex(),
            canvas.width,
            canvas.height
        )];
        let mut label = "base".to_owned();
        let mut input = 0;
        for layer in take.scene.layers.iter().filter(|l| l.visible && !l.missing) {
            let Some(stream) = take.stream(layer.id.as_str()) else {
                continue;
            };
            let p = place(
                stream.width.unwrap(),
                stream.height.unwrap(),
                layer.crop,
                layer.rect,
                layer.fit,
            )
            .unwrap();
            let k = unit.in_ as i64 - ns_to_frame_round(stream.start_offset_ns, canvas.fps);
            let (k0, lead) = (k.max(0), (-k).max(0));
            let seek = frame_ns(k0, canvas.fps) - 2 * NS_PER_MS;
            if seek > 0 {
                argv.extend(["-ss".to_owned(), seconds(seek)]);
            }
            argv.extend(["-i".to_owned(), file_arg(&stream.file)]);
            files.insert(stream.file.clone());
            let lead = if lead > 0 {
                format!("+{lead}")
            } else {
                String::new()
            };
            chains.push(format!(
                "[{input}:v]fps={num}/{den},setpts=PTS-STARTPTS{lead},crop={}:{}:{}:{},scale={}:{}:flags=bilinear,format=yuva420p[l{input}]",
                p.src.w, p.src.h, p.src.x, p.src.y, p.dst.w, p.dst.h
            ));
            chains.push(format!(
                "[{label}][l{input}]overlay=x={}:y={}:eof_action=pass[o{input}]",
                p.dst.x, p.dst.y
            ));
            label = format!("o{input}");
            input += 1;
        }
        chains.push(format!("[{label}]trim=end_frame={n},setsar=1,format=yuv420p[v]"));
        let tracks: Vec<_> = take
            .scene
            .audio
            .iter()
            .filter(|t| !t.muted && !t.missing)
            .filter_map(|t| take.stream(t.id.as_str()).map(|s| (t, s)))
            .collect();
        if tracks.is_empty() {
            chains.push(format!("anullsrc=r=48000:cl=stereo,atrim=end_sample={samples}[a]"));
        } else {
            let mut mix = String::new();
            for (j, (track, stream)) in tracks.iter().enumerate() {
                let t = frame_ns(unit.in_ as i64, canvas.fps)
                    - stream.start_offset_ns
                    - i64::from(track.sync_offset_ms) * NS_PER_MS;
                if t > 0 {
                    argv.extend(["-ss".to_owned(), seconds(t)]);
                }
                argv.extend(["-i".to_owned(), file_arg(&stream.file)]);
                files.insert(stream.file.clone());
                let up = if stream.channels == Some(1) {
                    "pan=stereo|c0=c0|c1=c0"
                } else {
                    "aformat=channel_layouts=stereo"
                };
                let mut chain = format!("[{input}:a]aresample=48000,{up},volume={}dB", track.gain_db);
                if t < 0 {
                    let delay = ((-t) as u128 * 48_000 + NS_PER_SEC as u128 / 2) / NS_PER_SEC as u128;
                    chain.push_str(&format!(",adelay={delay}S:all=1"));
                }
                chain.push_str(&format!("[a{j}]"));
                chains.push(chain);
                mix.push_str(&format!("[a{j}]"));
                input += 1;
            }
            if tracks.len() >= 2 {
                mix.push_str(&format!(
                    "amix=inputs={}:normalize=0:duration=longest,",
                    tracks.len()
                ));
            }
            mix.push_str(&format!("apad,atrim=end_sample={samples}"));
            if unit.in_ != 0 {
                mix.push_str(",afade=t=in:ns=240");
            }
            if unit.out < take.duration_frames {
                mix.push_str(&format!(",afade=t=out:ss={}:ns=240", samples - 240));
            }
            mix.push_str("[a]");
            chains.push(mix);
        }
        argv.extend([
            "-filter_complex".to_owned(),
            chains.join(";"),
            "-map".to_owned(),
            "[v]".to_owned(),
            "-map".to_owned(),
            "[a]".to_owned(),
        ]);
        argv.extend(strings(&[
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
            "-profile:v", "high", "-color_range", "tv", "-colorspace", "bt709",
            "-color_primaries", "bt709", "-color_trc", "bt709", "-c:a", "pcm_s16le", "-f", "mov",
        ]));
        let key = sha256_hex(format!("media-test\0{}", argv.join("\0")).as_bytes());
        let path = format!("{SEGMENTS}/{key}.mov");
        argv.push(file_arg(&path));
        segments.push(SegmentPlan {
            clips: unit.clips,
            take: unit.take,
            in_: unit.in_,
            out: unit.out,
            frames: n,
            samples,
            key,
            path,
            argv,
        });
    }
    let total_frames = segments.iter().map(|s| s.frames).sum();
    let stitch = stitch_plan(&segments, out);
    ExportPlan {
        out: out.to_owned(),
        tmp_ext: "mp4".to_owned(),
        segments,
        stitch,
        total_frames,
        files,
    }
}

fn stitch_plan(segments: &[SegmentPlan], out: &str) -> StitchPlan {
    let mut list_content = "ffconcat version 1.0\n".to_owned();
    for s in segments {
        list_content.push_str(&format!("file '{}.mov'\n", s.key));
    }
    let list_path = format!(
        "{SEGMENTS}/stitch-{}.ffconcat",
        &sha256_hex(list_content.as_bytes())[..16]
    );
    let mut argv = ffmpeg_prelude(true);
    argv.extend(strings(&["-f", "concat", "-i"]));
    argv.push(file_arg(&list_path));
    argv.extend(strings(&[
        "-map", "0:v:0", "-map", "0:a:0", "-c:v", "copy", "-c:a", "aac", "-b:a", "192k",
        "-movflags", "+faststart", "-f", "mp4",
    ]));
    argv.push(file_arg(out));
    StitchPlan {
        list_path,
        list_content,
        argv,
    }
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
    let stitch = stitch_plan(std::slice::from_ref(&segment), out);
    ExportPlan {
        out: out.to_owned(),
        tmp_ext: "mp4".to_owned(),
        total_frames: segment.frames,
        files: BTreeSet::new(),
        segments: vec![segment],
        stitch,
    }
}

// ------------------------------------------------------------- fixtures

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

fn video_packets(file: &Path) -> u64 {
    let probe = ffprobe(file);
    let video = probe["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == "video")
        .cloned()
        .unwrap_or(Value::Null);
    video["nb_read_packets"].as_str().unwrap().parse().unwrap()
}

fn duration_secs(file: &Path) -> f64 {
    ffprobe(file)["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

// ---------------------------------------------------------------- export

#[test]
#[ignore = "needs core output (parse::progress)"]
fn exports_three_takes_with_splits_offsets_and_a_repeat() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let project = three_take_project(&dir);
    let plan = hand_plan(&project, "exports/swamp-20261014-120000.mp4");
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

    // Frame-exact output: every unit's frames, about 6 s of audio and video.
    assert_eq!(video_packets(&out), 180);
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

    // Three unique segments cached, verified, nothing temporary left.
    let cached = names_in(&dir.join(SEGMENTS));
    assert_eq!(cached.len(), 3, "{cached:?}");
    for segment in &plan.segments {
        assert!(cached.contains(&format!("{}.mov", segment.key)));
    }
    let seg_logs = logs_matching(&dir, "-j1-seg");
    assert_eq!(seg_logs.len(), 3, "the repeated unit renders once: {seg_logs:?}");
    for log in &seg_logs {
        let text = fs::read_to_string(dir.join(".qrec/logs").join(log)).unwrap();
        assert!(text.contains("# qrec: verify:"), "{log}");
    }
    assert!(temps_in(&dir.join("exports")).is_empty());
    assert_eq!(names_in(&dir.join("exports")), ["swamp-20261014-120000.mp4"]);

    // Re-export: every segment comes from the cache; never overwrite.
    runner.render_export(JobId(2), plan.clone(), inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    let Some(Event::ExportFinished { path, .. }) = events.last() else {
        panic!("{events:#?}");
    };
    assert_eq!(Path::new(path), dir.join("exports/swamp-20261014-120000-2.mp4"));
    assert!(logs_matching(&dir, "-j2-seg").is_empty(), "nothing re-rendered");
    assert_eq!(video_packets(Path::new(path)), 180);

    // Losing one cache file re-renders only that unit.
    fs::remove_file(dir.join(&plan.segments[2].path)).unwrap();
    runner.render_export(JobId(3), plan.clone(), inbox);
    let events = events_until(&rx, WAIT, export_ended);
    assert!(matches!(events.last(), Some(Event::ExportFinished { .. })), "{events:#?}");
    assert_eq!(logs_matching(&dir, "-j3-seg"), [logs_matching(&dir, "-j3-seg2")[0].clone()]);
    assert_eq!(names_in(&dir.join("exports")).len(), 3);
}

#[test]
#[ignore = "needs core output (parse::progress)"]
fn hand_picked_outputs_are_replaced_only_when_they_existed() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    fixture_project(&dir, &[MediaSpec::video(320, 180, 30, 30).with_audio(48_000, 1, &[])]);
    let project: Project =
        serde_json::from_str(&fs::read_to_string(dir.join("qrec.json")).unwrap()).unwrap();
    let out = env.root().join("mine.mp4");
    let plan = hand_plan(&project, out.to_str().unwrap());
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();

    runner.render_export(JobId(1), plan.clone(), inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    assert!(
        matches!(events.last(), Some(Event::ExportFinished { path, .. }) if Path::new(path) == out),
        "{events:#?}"
    );
    // It exists now, so a second export (confirmed by the core) replaces it.
    fs::write(&out, "old").unwrap();
    runner.render_export(JobId(2), plan, inbox);
    let events = events_until(&rx, WAIT, export_ended);
    assert!(
        matches!(events.last(), Some(Event::ExportFinished { path, .. }) if Path::new(path) == out),
        "{events:#?}"
    );
    assert_eq!(video_packets(&out), 30);
    assert!(temps_in(env.root()).is_empty());
}

#[test]
fn cancel_stops_ffmpeg_and_deletes_temp_files() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    fs::create_dir_all(dir.join("exports")).unwrap();
    // An endless, real-time render: only a cancel ends it.
    let segment = raw_segment(
        &[
            "-re", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30", "-f", "lavfi", "-i",
            "anullsrc=r=48000:cl=stereo", "-map", "0:v", "-map", "1:a", "-c:v", "libx264",
            "-preset", "ultrafast", "-c:a", "pcm_s16le",
        ],
        300,
    );
    let plan = raw_plan(segment.clone(), "exports/never.mp4");
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.render_export(JobId(7), plan, inbox);

    // Wait for ffmpeg to write its temp segment, then cancel.
    let segments = dir.join(SEGMENTS);
    let started = Instant::now();
    while temps_in(&segments).is_empty() {
        assert!(started.elapsed() < Duration::from_secs(30), "no temp segment appeared");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(runner.is_running(JobId(7)));
    let cancelled_at = Instant::now();
    runner.cancel(JobId(7));
    let events = events_until(&rx, Duration::from_secs(20), export_ended);
    assert_eq!(events.last(), Some(&Event::ExportCancelled { job: JobId(7) }));
    assert!(cancelled_at.elapsed() < Duration::from_secs(5), "{:?}", cancelled_at.elapsed());
    assert!(!runner.is_running(JobId(7)));
    assert!(temps_in(&segments).is_empty(), "{:?}", names_in(&segments));
    assert!(!dir.join(&segment.path).exists(), "an unfinished segment was cached");
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
        &["-f", "lavfi", "-i", "testsrc2=duration=1", "-vf", "qrecnosuchfilter"],
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
    assert!(message.starts_with("segment 1/1 (take-0001, frames 0–30): ffmpeg exited with status"), "{message}");
    assert!(
        stderr_tail.iter().any(|l| l.contains("qrecnosuchfilter")),
        "{stderr_tail:#?}"
    );
    assert!(stderr_tail.len() <= 30);
    assert!(log.contains("/.qrec/logs/") && log.ends_with("-j3-seg0.log"), "{log}");
    assert!(Path::new(log).is_file());
    assert!(temps_in(&dir.join(SEGMENTS)).is_empty());
    assert!(temps_in(&dir.join("exports")).is_empty());
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

    // A plan that would write outside an existing folder.
    let segment = raw_segment(&["-f", "lavfi", "-i", "testsrc2"], 30);
    runner.render_export(JobId(3), raw_plan(segment, "/qrec-no-such-dir/x.mp4"), inbox);
    let events = events_until(&rx, WAIT, export_ended);
    assert!(
        matches!(events.last(), Some(Event::ExportFailed { message, .. })
            if message.contains("/qrec-no-such-dir doesn't exist")),
        "{events:#?}"
    );
}

#[test]
#[ignore = "needs core output (export_plan, frame_plan, parse::progress)"]
fn exports_with_the_core_planner_and_split_identity() {
    use qrec_core::export_plan::{ExportOptions, plan_export};
    use qrec_core::protocol::{ExportFormat, Quality, VideoCodec};
    use qrec_core::state::{Capabilities, FfmpegCaps};

    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let project = three_take_project(&dir);
    let encoders: BTreeSet<String> = ["libx264", "aac", "pcm_s16le", "libopus", "libvpx-vp9"]
        .map(String::from)
        .into();
    let caps = Capabilities {
        ffmpeg: Some(FfmpegCaps {
            version: "8.1.2".into(),
            identity: "ffmpeg".into(),
            encoders,
        }),
        vaapi: None,
        cores: 4,
    };
    let opts = ExportOptions {
        format: ExportFormat::Mp4,
        quality: Quality::Draft,
        codec: VideoCodec::H264,
        out: "exports/core.mp4".to_owned(),
        allow_vaapi: false,
    };
    let plan = plan_export(&project, &caps, &opts).unwrap();
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.render_export(JobId(1), plan.clone(), inbox.clone());
    let events = events_until(&rx, WAIT, export_ended);
    let Some(Event::ExportFinished { path, .. }) = events.last() else {
        panic!("{events:#?}");
    };
    assert_eq!(video_packets(Path::new(path)), plan.total_frames);

    // Splitting a clip changes no segment: re-exporting renders nothing.
    let mut split = project.clone();
    let clip = split.timeline.clips[1].clone();
    split.timeline.clips[1].out = 40;
    split.timeline.clips.insert(
        2,
        qrec_core::model::Clip {
            id: ClipId::from_raw("c0ff"),
            in_: 40,
            ..clip
        },
    );
    let split_plan = plan_export(&split, &caps, &opts).unwrap();
    let keys = |p: &ExportPlan| p.segments.iter().map(|s| s.key.clone()).collect::<Vec<_>>();
    assert_eq!(keys(&split_plan), keys(&plan));
    runner.render_export(JobId(2), split_plan, inbox);
    let events = events_until(&rx, WAIT, export_ended);
    assert!(matches!(events.last(), Some(Event::ExportFinished { .. })), "{events:#?}");
    assert!(logs_matching(&dir, "-j2-seg").is_empty());
}

// ----------------------------------------------------------------- frames

fn frame_plan(out: &str) -> FramePlan {
    let mut argv = ffmpeg_prelude(true);
    argv.extend(strings(&[
        "-ss",
        "0.031333",
        "-i",
        "file:takes/take-0001/L1-screen-FAKE-1.mkv",
        "-filter_complex",
        "[0:v]scale=out_color_matrix=bt709:out_range=pc,format=rgb24[v]",
        "-map",
        "[v]",
        "-frames:v",
        "1",
        "-f",
        "image2",
        "-update",
        "1",
    ]));
    argv.push(file_arg(out));
    FramePlan {
        out: out.to_owned(),
        tmp_ext: "png".to_owned(),
        argv,
        files: ["takes/take-0001/L1-screen-FAKE-1.mkv".to_owned()].into(),
    }
}

#[test]
#[ignore = "needs core output (parse::progress)"]
fn frames_get_new_names_and_are_pngs() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    fixture_project(&dir, &[MediaSpec::video(320, 180, 30, 30)]);
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    let done = |e: &Event| matches!(e, Event::FrameSaved { .. } | Event::FrameFailed { .. });
    let out = "frames/swamp-take-0001-00000001.png";
    for (job, expected) in [(1, "swamp-take-0001-00000001.png"), (2, "swamp-take-0001-00000001-2.png")] {
        runner.save_frame(JobId(job), frame_plan(out), inbox.clone());
        let events = events_until(&rx, WAIT, done);
        assert_eq!(
            events.last(),
            Some(&Event::FrameSaved {
                job: JobId(job),
                path: dir.join("frames").join(expected).to_string_lossy().into_owned(),
            })
        );
    }
    let probe = ffprobe(&dir.join("frames/swamp-take-0001-00000001.png"));
    assert_eq!(probe["streams"][0]["codec_name"], "png");
    assert_eq!(probe["streams"][0]["width"], 320);
    assert!(temps_in(&dir.join("frames")).is_empty());
}

#[test]
fn a_failed_frame_save_reports_its_log() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    let mut runner = runner(&dir);
    let (inbox, rx) = Inbox::channel();
    runner.save_frame(JobId(4), frame_plan("frames/x.png"), inbox);
    let events = events_until(&rx, WAIT, |e| matches!(e, Event::FrameFailed { .. }));
    let Some(Event::FrameFailed { message, log, .. }) = events.last() else {
        panic!("{events:#?}");
    };
    assert!(message.contains("L1-screen-FAKE-1.mkv is missing"), "{message}");
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
    assert_eq!((results[0].id.as_str(), results[0].file.as_str()), ("L1", file));
    assert_eq!(results[0].info, Err(format!("{file} is missing")));
}

#[test]
#[ignore = "needs core output (parse::ffprobe)"]
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
        job("L1", "takes/take-0001/L1-screen-FAKE-1.mkv", StreamKind::Video),
        job("A1", "takes/take-0001/A1-mic.mka", StreamKind::Audio),
        job("L2", "takes/take-0001/L2-camera-0.mkv", StreamKind::Video),
    ];
    runner.probe(TakeId::numbered(1), ProbePurpose::Finalize, jobs, inbox);
    let events = events_until(&rx, WAIT, |e| matches!(e, Event::TakeProbed { .. }));
    let Some(Event::TakeProbed { results, .. }) = events.last() else {
        panic!("{events:#?}");
    };
    let video = results[0].info.as_ref().unwrap();
    assert_eq!((video.frames, video.width, video.height), (Some(60), Some(320), Some(180)));
    let audio = results[1].info.as_ref().unwrap();
    assert_eq!((audio.sample_rate, audio.channels), (Some(48_000), Some(1)));
    assert!(results[2].info.is_err());
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
    runner.detect_silence(JobId(5), key.clone(), silence_argv("takes/gone.mka", -35.0, 400), inbox.clone());
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

    // A cache file for another key (or corrupt) is ignored: ffmpeg runs and fails.
    fs::write(cache_dir.join(format!("{}.json", "cd".repeat(32))), "{oops").unwrap();
    let other = SilenceKey("cd".repeat(32));
    runner.detect_silence(JobId(6), other.clone(), silence_argv("takes/gone.mka", -35.0, 400), inbox);
    let events = events_until(&rx, WAIT, |_| true);
    let [Event::SilenceFailed { job, key, message }] = events.as_slice() else {
        panic!("{events:#?}");
    };
    assert_eq!((*job, key), (JobId(6), &other));
    assert!(message.contains("ffmpeg exited with status"), "{message}");
    assert!(message.contains("-j6-silence.log"), "{message}");
}

#[test]
#[ignore = "needs core autocut (parse::silencedetect)"]
fn silence_detection_finds_the_fixture_gaps_and_caches_them() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    // Speech 0–0.5 s and 1.5–2 s: one silence in between.
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
    let found: Vec<(i64, i64)> = silences.iter().map(|i| (ms(i.start_ns), ms(i.end_ns))).collect();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!((found[0].0 - 500).abs() <= 10 && (found[0].1 - 1500).abs() <= 10, "{found:?}");
    // A trailing silence ends at the file's end.
    assert!((found[1].0 - 2000).abs() <= 10 && (found[1].1 - 2500).abs() <= 10, "{found:?}");
    assert!(dir.join(format!(".qrec/cache/silence/{}.json", key.0)).is_file());

    // Again (another job): straight from the cache.
    runner.detect_silence(JobId(2), key, argv, inbox);
    let again = events_until(&rx, WAIT, |_| true);
    assert!(matches!(&again[0], Event::SilenceDetected { silences: s, .. } if s == silences));
    assert!(logs_matching(&dir, "-j2-").is_empty());
}

// ------------------------------------------------------------ purge, misc

#[test]
fn purge_reports_through_the_inbox() {
    let env = TestEnv::new();
    let dir = env.empty_dir("swamp");
    fixture_project(&dir, &[MediaSpec::audio(48_000, 1, 500, &[]), MediaSpec::audio(48_000, 1, 500, &[])]);
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
