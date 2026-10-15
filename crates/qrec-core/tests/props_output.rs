//! Properties of the output planners (A§9.3, A§9.6, A§11): a split never
//! changes the export or GES plan, every segment has exactly the frames and
//! samples of its unit, units are maximal runs, GES clips stay inside their
//! unit and their media, the libx264 fallback of a VA plan equals planning
//! without VA, and the frame selection's lead counts canvas frames.
//!
//! `crates/qrec-shell/tests/plans_ffmpeg.rs` runs the plans through real
//! ffmpeg.

use proptest::prelude::*;
use proptest::sample::Index;

use qrec_core::export_plan::{
    ExportOptions, RenderUnit, plan_export, render_units, select_frames, without_vaapi,
};
use qrec_core::ges_plan::plan_ges;
use qrec_core::model::{Clip, Fps, Project, StreamKind};
use qrec_core::plans::ExportPlan;
use qrec_core::protocol::command::{ExportFormat, Quality, VideoCodec};
use qrec_core::requantize::derive_duration_frames;
use qrec_core::state::{Capabilities, VaapiCaps};
use qrec_core::testkit::{self, strategies::arb_project};
use qrec_core::time::frame_ns;
use qrec_core::timeline::total_frames;
use qrec_core::{ClipId, Frame};

/// Random media facts and layout on top of `arb_project`: stream offsets
/// (−60…+150 ms), other recorded rates, sync offsets, gains, mutes,
/// hidden or translucent layers. Durations are re-derived and clips clamped
/// to them, so the project stays valid.
#[derive(Debug, Clone)]
struct Tweak {
    offset_ms: i64,
    rate: u8,
    flag: u8,
    sync_ms: i32,
}

fn arb_tweaks() -> impl Strategy<Value = Vec<Tweak>> {
    prop::collection::vec(
        (-60i64..150, 0u8..6, 0u8..8, -100i32..100).prop_map(|(offset_ms, rate, flag, sync_ms)| {
            Tweak {
                offset_ms,
                rate,
                flag,
                sync_ms,
            }
        }),
        16,
    )
}

fn tweaked(mut project: Project, tweaks: &[Tweak]) -> Project {
    let canvas_fps = project.canvas.fps;
    let mut n = 0;
    let mut next = || {
        n += 1;
        &tweaks[n % tweaks.len()]
    };
    for take in project.takes.values_mut() {
        for stream in &mut take.streams {
            let t = next();
            stream.start_offset_ns = t.offset_ms * 1_000_000 + i64::from(t.sync_ms) * 1000;
            if stream.kind == StreamKind::Video {
                stream.fps = Some(match t.rate {
                    0 => Fps::new(60, 1),
                    1 => Fps::new(24, 1),
                    _ => canvas_fps,
                });
            }
        }
        for layer in &mut take.scene.layers {
            match next().flag {
                0 => layer.visible = false,
                1 => layer.opacity = 0.5,
                2 => layer.opacity = 0.0,
                _ => {}
            }
        }
        for track in &mut take.scene.audio {
            let t = next();
            track.sync_offset_ms = t.sync_ms;
            track.muted = t.flag == 0;
            track.gain_db = f64::from(t.flag) - 3.0;
        }
        take.duration_frames = derive_duration_frames(take, canvas_fps).max(1);
    }
    let takes = project.takes.clone();
    project.timeline.clips.retain_mut(|clip| {
        let duration = takes[&clip.take].duration_frames;
        clip.out = clip.out.min(duration);
        clip.in_ = clip.in_.min(clip.out.saturating_sub(1));
        clip.in_ < clip.out
    });
    project
}

/// Splits the `which`-th clip that is at least 2 frames long at a point
/// inside it (like `timeline::split`: the left part keeps the id).
fn split(project: &Project, which: Index, at: Index) -> Option<(Project, ClipId)> {
    let candidates: Vec<usize> = (0..project.timeline.clips.len())
        .filter(|&i| project.timeline.clips[i].len() >= 2)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let i = candidates[which.index(candidates.len())];
    let clip = project.timeline.clips[i].clone();
    let p = clip.in_ + 1 + at.index((clip.len() - 1) as usize) as Frame;
    let new_id = ClipId::from_raw("split-right");
    let mut split = project.clone();
    split.timeline.clips[i].out = p;
    split.timeline.clips.insert(
        i + 1,
        Clip {
            id: new_id.clone(),
            take: clip.take,
            in_: p,
            out: clip.out,
        },
    );
    Some((split, new_id))
}

fn opts(
    format: ExportFormat,
    codec: VideoCodec,
    quality: Quality,
    allow_vaapi: bool,
) -> ExportOptions {
    ExportOptions {
        format,
        quality,
        codec,
        out: "exports/test-20261014-120000.mp4".to_owned(),
        allow_vaapi,
    }
}

fn arb_opts() -> impl Strategy<Value = ExportOptions> {
    (0u8..4, 0u8..3, any::<bool>()).prop_map(|(codec, quality, va)| {
        let quality = [Quality::Draft, Quality::Standard, Quality::High][quality as usize];
        match codec {
            0 | 1 => opts(ExportFormat::Mp4, VideoCodec::H264, quality, va),
            2 => opts(ExportFormat::Webm, VideoCodec::Vp9, quality, va),
            _ => opts(ExportFormat::Webm, VideoCodec::Av1, quality, va),
        }
    })
}

fn va_caps() -> Capabilities {
    let mut caps = testkit::capabilities();
    caps.vaapi = Some(VaapiCaps {
        device: "/dev/dri/renderD129".to_owned(),
        vendor: 0x8086,
        ffmpeg_h264: true,
        gst_h264_factory: None,
    });
    caps
}

/// The plan without the clip ids its segments list.
fn without_clip_ids(mut plan: ExportPlan) -> ExportPlan {
    for segment in &mut plan.segments {
        segment.clips.clear();
    }
    plan
}

fn graph(argv: &[String]) -> &str {
    let at = argv
        .iter()
        .position(|a| a == "-filter_complex")
        .expect("a filter graph");
    &argv[at + 1]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn a_split_never_changes_the_export(
        project in arb_project(),
        tweaks in arb_tweaks(),
        which in any::<Index>(),
        at in any::<Index>(),
        opts in arb_opts(),
    ) {
        let project = tweaked(project, &tweaks);
        // Nothing to split: nothing to check (returning instead of
        // `prop_assume!` keeps large PROPTEST_CASES runs under the reject limit).
        let Some((split, new_id)) = split(&project, which, at) else { return Ok(()); };
        let caps = va_caps();
        let before = plan_export(&project, &caps, &opts).unwrap();
        let after = plan_export(&split, &caps, &opts).unwrap();
        // The new clip joins its unit: the clip lists differ by exactly that id.
        for (b, a) in before.segments.iter().zip(&after.segments) {
            let mut ids = a.clips.clone();
            ids.retain(|id| id != &new_id);
            prop_assert_eq!(&ids, &b.clips);
        }
        prop_assert_eq!(without_clip_ids(after), without_clip_ids(before));
    }

    #[test]
    fn a_split_never_changes_the_ges_plan(
        project in arb_project(),
        tweaks in arb_tweaks(),
        which in any::<Index>(),
        at in any::<Index>(),
    ) {
        let project = tweaked(project, &tweaks);
        let Some((split, _)) = split(&project, which, at) else { return Ok(()); };
        prop_assert_eq!(plan_ges(&split), plan_ges(&project));
    }

    #[test]
    fn segments_have_exactly_their_frames_and_samples(
        project in arb_project(),
        tweaks in arb_tweaks(),
        opts in arb_opts(),
    ) {
        let project = tweaked(project, &tweaks);
        let fps = project.canvas.fps;
        let total = total_frames(&project.timeline.clips);
        let plan = match plan_export(&project, &va_caps(), &opts) {
            Ok(plan) => plan,
            Err(e) => {
                prop_assert_eq!(total, 0, "{}", e.message);
                return Ok(());
            }
        };
        prop_assert_eq!(plan.total_frames, total);
        prop_assert_eq!(plan.segments.iter().map(|s| s.frames).sum::<u64>(), total);
        for s in &plan.segments {
            let n = s.out - s.in_;
            prop_assert_eq!(s.frames, n);
            let exact = u128::from(n) * 48_000 * u128::from(fps.den);
            let rounded = ((2 * exact + u128::from(fps.num)) / (2 * u128::from(fps.num))) as u64;
            prop_assert_eq!(s.samples, rounded);
            let g = graph(&s.argv);
            let trim = format!("trim=end_frame={n},");
            let atrim = format!("atrim=end_sample={},", s.samples);
            let atrim_last = format!("atrim=end_sample={}[a]", s.samples);
            prop_assert!(g.contains(&trim), "{}", g);
            prop_assert!(g.contains(&atrim) || g.contains(&atrim_last), "{}", g);
            // Lengths live in the graph, never in output options.
            prop_assert!(!s.argv.iter().any(|a| a == "-t" || a == "-frames:v" || a == "-shortest"));
            prop_assert_eq!(s.argv.last().unwrap(), &format!("file:{}", s.path));
            let (mov, mkv) = (format!("{}.mov", s.key), format!("{}.mkv", s.key));
            prop_assert!(s.path.ends_with(&mov) || s.path.ends_with(&mkv), "{}", s.path);
            prop_assert_eq!(&s.argv[0], "ffmpeg");
            // Every input path is a project file the job lists.
            for arg in s.argv.iter().filter(|a| a.starts_with("file:takes/")) {
                prop_assert!(plan.files.contains(&arg["file:".len()..]), "{}", arg);
            }
        }
        // The concat list: a header, then each segment's file and exact
        // duration; every segment starts within 0.5 µs of its timeline time
        // (the durations never add up their rounding).
        let lines: Vec<&str> = plan.stitch.list_content.lines().collect();
        prop_assert_eq!(lines.len(), 2 * plan.segments.len() + 1);
        let (num, den) = (i128::from(fps.num), i128::from(fps.den));
        let (mut start_us, mut position) = (0i128, 0i128);
        for (s, pair) in plan.segments.iter().zip(lines[1..].chunks(2)) {
            let name = s.path.rsplit('/').next().unwrap();
            prop_assert_eq!(pair[0], format!("file '{name}'"));
            let error = start_us * num - position * 1_000_000 * den;
            prop_assert!(2 * error.abs() <= num, "starts at {} µs", start_us);
            let duration = pair[1].strip_prefix("duration ").unwrap();
            let (secs, micros) = duration.split_once('.').unwrap();
            prop_assert_eq!(micros.len(), 6);
            start_us += secs.parse::<i128>().unwrap() * 1_000_000 + micros.parse::<i128>().unwrap();
            position += i128::from(s.frames);
        }
        let error = start_us * num - position * 1_000_000 * den;
        prop_assert!(2 * error.abs() <= num, "ends at {} µs", start_us);
        prop_assert_eq!(plan.stitch.argv.last().unwrap(), &format!("file:{}", plan.out));
    }

    #[test]
    fn units_are_maximal_runs(project in arb_project(), tweaks in arb_tweaks()) {
        let project = tweaked(project, &tweaks);
        let units = render_units(&project);
        let ids: Vec<&ClipId> = units.iter().flat_map(|u| &u.clips).collect();
        let timeline: Vec<&ClipId> = project.timeline.clips.iter().map(|c| &c.id).collect();
        prop_assert_eq!(ids, timeline);
        let mut position = 0;
        for (i, unit) in units.iter().enumerate() {
            prop_assert_eq!(unit.position, position);
            position += unit.frames();
            if let Some(next) = units.get(i + 1) {
                let continuous = |a: &RenderUnit, b: &RenderUnit| a.take == b.take && a.out == b.in_;
                prop_assert!(!continuous(unit, next), "{:?} {:?}", unit, next);
            }
        }
        prop_assert_eq!(position, total_frames(&project.timeline.clips));
    }

    #[test]
    fn the_x264_fallback_equals_planning_without_vaapi(
        project in arb_project(),
        tweaks in arb_tweaks(),
        quality in 0usize..3,
    ) {
        let project = tweaked(project, &tweaks);
        let quality = [Quality::Draft, Quality::Standard, Quality::High][quality];
        let caps = va_caps();
        let Ok(va) = plan_export(&project, &caps, &opts(ExportFormat::Mp4, VideoCodec::H264, quality, true)) else {
            prop_assert!(project.timeline.clips.is_empty(), "only an empty timeline has nothing to export");
            return Ok(());
        };
        let x264 = plan_export(&project, &caps, &opts(ExportFormat::Mp4, VideoCodec::H264, quality, false)).unwrap();
        prop_assert_eq!(without_vaapi(&va, &caps), Some(x264.clone()));
        prop_assert_eq!(without_vaapi(&x264, &caps), None);
    }

    #[test]
    fn ges_clips_stay_inside_their_unit_and_their_media(
        project in arb_project(),
        tweaks in arb_tweaks(),
    ) {
        let project = tweaked(project, &tweaks);
        let fps = project.canvas.fps;
        let ges = plan_ges(&project);
        let total = total_frames(&project.timeline.clips);
        prop_assert_eq!(ges.duration_ns as i64, frame_ns(total as i64, fps));
        let units: Vec<(i64, i64)> = render_units(&project)
            .iter()
            .map(|u| (frame_ns(u.position as i64, fps), frame_ns((u.position + u.frames()) as i64, fps)))
            .collect();
        let inside = |start: u64, duration: u64| {
            let (start, end) = (start as i64, (start + duration) as i64);
            units.iter().any(|&(a, b)| a <= start && end <= b)
        };
        let stream = |file: &str| {
            project
                .takes
                .values()
                .flat_map(|t| &t.streams)
                .find(|s| s.file == file)
                .expect("a clip plays a stream of the project")
        };
        for clip in &ges.video {
            prop_assert!(clip.duration_ns > 0);
            prop_assert!(inside(clip.start_ns, clip.duration_ns), "{:?}", clip);
            let s = stream(&clip.file);
            if let Some(frames) = s.frames {
                let end = frame_ns(frames as i64, s.fps.unwrap_or(fps));
                prop_assert!((clip.inpoint_ns + clip.duration_ns) as i64 <= end, "{:?} {:?}", clip, s);
            }
            prop_assert!(clip.alpha > 0.0 && clip.alpha <= 1.0);
        }
        for clip in &ges.audio {
            prop_assert!(clip.duration_ns > 0);
            prop_assert!(inside(clip.start_ns, clip.duration_ns), "{:?}", clip);
            let s = stream(&clip.file);
            if let Some(duration) = s.duration_ns {
                prop_assert!((clip.inpoint_ns + clip.duration_ns) as i64 <= duration, "{:?} {:?}", clip, s);
            }
            prop_assert!(clip.fade_in_samples <= 240 && clip.fade_out_samples <= 240);
        }
    }

    #[test]
    fn lead_counts_canvas_frames_without_a_recorded_frame(
        project in arb_project(),
        tweaks in arb_tweaks(),
        in_ in 0u64..40,
    ) {
        let project = tweaked(project, &tweaks);
        let canvas = project.canvas.fps;
        for stream in project.takes.values().flat_map(|t| &t.streams).filter(|s| s.kind == StreamKind::Video) {
            let recorded = stream.fps.unwrap_or(canvas);
            let sel = select_frames(stream, canvas, in_);
            let nearest = |t: u64| nearest_recorded(recorded, stream.start_offset_ns, canvas, t);
            let lead = (in_..in_ + 10_000).take_while(|&t| nearest(t) < 0).count() as u64;
            prop_assert_eq!(sel.lead, lead);
            prop_assert_eq!(sel.k0 as i64, nearest(in_ + lead));
            // At another rate, every recorded frame starts where the rule
            // first shows it, counted from the unit's start (`k0` may have
            // started before the unit: it shows from the unit's first frame).
            if let Some(retime) = sel.retime {
                for j in sel.k0 as i64..sel.k0 as i64 + 20 {
                    let start = (in_..in_ + 10_000).find(|&t| nearest(t) >= j).unwrap();
                    prop_assert_eq!(retime.slot(j).max(0), start as i64 - in_ as i64);
                }
            }
        }
    }
}

/// The recorded frame nearest to take frame `t`'s time in the file, the
/// earlier one on a tie (A§9.3), by comparing distances in whole numbers
/// (times scaled by R·C·10⁹).
fn nearest_recorded(recorded: Fps, offset_ns: i64, canvas: Fps, t: u64) -> i64 {
    let (cn, cd) = (i128::from(canvas.num), i128::from(canvas.den));
    let (rn, rd) = (i128::from(recorded.num), i128::from(recorded.den));
    let time = (i128::from(t) * cd * 1_000_000_000 - i128::from(offset_ns) * cn) * rn;
    let step = rd * cn * 1_000_000_000;
    let before = time.div_euclid(step);
    let after = before + 1;
    (if after * step - time < time - before * step {
        after
    } else {
        before
    }) as i64
}
