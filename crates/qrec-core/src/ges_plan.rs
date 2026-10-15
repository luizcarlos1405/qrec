//! The GES timeline planner for preview playback (A§9.6). Owner: output.
//!
//! Same render units, layer inputs, frame selection and lead as the export
//! plan (A§9.3), so preview and export show the same frames:
//! `inpoint_ns = frame_ns_rec(k0)`, `start_ns = frame_ns(pos + lead)`,
//! `duration_ns = frame_ns(pos + n) − start_ns`. A layer recorded at another
//! rate instead starts at the file time of its first canvas frame, so its
//! frames line up with the take's time wherever the unit starts (from `k0`
//! they would be off by `k0`'s rounding, up to half a recorded frame): GES
//! shows the last frame started, so the in-point is half a recorded frame
//! later to make that the nearest one, and 3 ms earlier to undo the
//! renderer's 2 ms Matroska nudge and keep a stamp truncated to the
//! millisecond from winning a tie that the export gives to the earlier
//! frame (`mixed_rate_inpoint`). Video clips carry their
//! placement (the renderer adds a `videocrop` from `placement.src` and sets
//! `posx/posy/width/height` from `placement.dst`); `layer_priority` 0 is the
//! top of the take scene. Audio clips start where their file time is 0 when
//! the track starts after the unit does, and fade over 240 samples at cuts,
//! like the export's `afade` on the unit's mix.
//!
//! GES refuses a clip whose `inpoint + duration` passes the end of its
//! media, so a clip is cut short at the stream's known end (`frames` for
//! video, `durationNs` for audio) and left out when nothing of it remains.
//! The export shows the same thing there: the background (`eof_action=pass`)
//! and silence (`apad`).
//!
//! For the GUI's Program player (docs/GUI.md G§3.10): [`plan_ges_take`]
//! plays one take whole (the trim scrub, the bin preview), [`without_tracks`]
//! leaves out the lanes muted or not soloed for playback (D-08), and
//! [`scaled`] composites at a smaller size than the canvas (H-9), with
//! [`fit_size`] choosing that size.

use std::collections::BTreeSet;

use crate::export_plan::{
    FADE_SAMPLES, LayerInput, RenderUnit, SEEK_BACKOFF_NS, layer_inputs, render_units,
    track_inputs, unit_samples,
};
use crate::ids::{Frame, TakeId, TrackId};
use crate::model::{Fps, Project, Rect};
use crate::plans::{GesAudioClip, GesTimeline, GesVideoClip};
use crate::time::{NS_PER_MS, frame_ns};
use crate::timeline::total_frames;

/// The timeline as GES clips: same render units, frame selection and lead
/// as the export plan (A§9.3).
pub fn plan_ges(project: &Project) -> GesTimeline {
    let total = total_frames(&project.timeline.clips);
    plan_units(project, &render_units(project), total)
}

/// One take played whole, as if it were the only clip on the timeline,
/// `[0, durationFrames)`, in its take scene (GUI G§3.10: the trim scrub of
/// S10 and the bin preview of D-17). An unknown take, or one without
/// frames, plans an empty timeline.
pub fn plan_ges_take(project: &Project, take: &TakeId) -> GesTimeline {
    let frames = project.take(take).map_or(0, |t| t.duration_frames);
    let units: Vec<RenderUnit> = (frames > 0)
        .then(|| RenderUnit {
            clips: Vec::new(),
            take: take.clone(),
            in_: 0,
            out: frames,
            position: 0,
        })
        .into_iter()
        .collect();
    plan_units(project, &units, frames)
}

/// `plan` without the sound of the lanes muted for playback, and with only
/// the soloed lane's when one is soloed (GUI D-08, `ui.laneMute` and
/// `ui.solo`): a muted solo lane is silent too. Video is untouched, and the
/// export never sees this (it plans every unmuted track of the scenes).
pub fn without_tracks(
    mut plan: GesTimeline,
    solo: Option<&TrackId>,
    muted: &BTreeSet<TrackId>,
) -> GesTimeline {
    plan.audio
        .retain(|clip| !muted.contains(&clip.track) && solo.is_none_or(|s| *s == clip.track));
    plan
}

/// `plan` composited at `width`×`height` instead of the canvas size (GUI
/// H-9): the output and every video clip's destination rect are scaled by
/// `width / canvas width` and `height / canvas height`, each edge rounded to
/// an even pixel like [`place`](crate::layout::place)'s, so 4:2:0 frames
/// stay aligned and layers that touch keep touching. Source crops stay in
/// the recorded file's pixels (GES crops the file before it scales), and
/// timing and audio don't change. At the canvas size it is `plan`.
pub fn scaled(mut plan: GesTimeline, width: u32, height: u32) -> GesTimeline {
    let (from_w, from_h) = (plan.canvas.width, plan.canvas.height);
    if from_w > 0 && from_h > 0 {
        for clip in &mut plan.video {
            let dst = clip.placement.dst;
            let (x, y) = (i64::from(dst.x), i64::from(dst.y));
            let left = scale_even(x, width, from_w);
            let right = scale_even(x + i64::from(dst.w), width, from_w);
            let top = scale_even(y, height, from_h);
            let bottom = scale_even(y + i64::from(dst.h), height, from_h);
            clip.placement.dst = Rect::new(
                clamp_i32(left),
                clamp_i32(top),
                size(right - left),
                size(bottom - top),
            );
        }
    }
    plan.canvas.width = width;
    plan.canvas.height = height;
    plan
}

/// The size to composite a `width`×`height` canvas at inside `max` (GUI
/// H-9, and the live preview's size, G§3.9): the canvas itself when it
/// fits, else scaled down to touch `max` with its aspect kept, each side
/// rounded down to even and at least 2. Never larger than the canvas.
pub fn fit_size(width: u32, height: u32, max: (u32, u32)) -> (u32, u32) {
    let (max_w, max_h) = (u64::from(max.0.max(2)), u64::from(max.1.max(2)));
    let (w, h) = (u64::from(width.max(1)), u64::from(height.max(1)));
    if w <= max_w && h <= max_h {
        return (width, height);
    }
    let (fit_w, fit_h) = if w * max_h >= h * max_w {
        // Width-limited: round(h · max_w / w).
        (max_w, (2 * h * max_w + w) / (2 * w))
    } else {
        ((2 * w * max_h + h) / (2 * h), max_h)
    };
    let even = |v: u64| (v & !1).max(2) as u32;
    (even(fit_w), even(fit_h))
}

/// `v · to / from` rounded to the nearest even number (halves up), in
/// exact integer arithmetic: an even `v` maps to itself when `to == from`.
fn scale_even(v: i64, to: u32, from: u32) -> i64 {
    let num = i128::from(v) * i128::from(to);
    let den = 2 * i128::from(from);
    // round(num / den) = ⌊(2·num + den) / 2·den⌋
    let half = (2 * num + den).div_euclid(2 * den);
    (2 * half).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// A scaled side: at least 2 pixels, like [`place`](crate::layout::place)'s.
fn size(v: i64) -> u32 {
    v.clamp(2, i64::from(u32::MAX)) as u32
}

/// The GES clips of `units`, on a timeline `total` frames long.
fn plan_units(project: &Project, units: &[RenderUnit], total: Frame) -> GesTimeline {
    let canvas = &project.canvas;
    let fps = canvas.fps;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for unit in units {
        let Some(take) = project.take(&unit.take) else {
            continue;
        };
        let n = unit.frames();
        let unit_start = frame_ns(unit.position as i64, fps);
        let unit_end = frame_ns((unit.position + n) as i64, fps);
        let top = take.scene.layers.len().saturating_sub(1);
        for input in layer_inputs(take, canvas, unit.in_, n) {
            let start = frame_ns((unit.position + input.selection.lead) as i64, fps);
            let inpoint = match input.selection.retime {
                None => frame_ns(input.selection.k0 as i64, input.recorded),
                Some(_) => mixed_rate_inpoint(&input, unit.in_, fps),
            };
            let media_end = input
                .stream
                .frames
                .map(|frames| frame_ns(frames as i64, input.recorded));
            let Some(duration) = clamp(unit_end - start, inpoint, media_end) else {
                continue;
            };
            video.push(GesVideoClip {
                take: unit.take.clone(),
                layer: input.layer.id.clone(),
                file: input.stream.file.clone(),
                layer_priority: (top - input.index) as u32,
                start_ns: start as u64,
                inpoint_ns: inpoint as u64,
                duration_ns: duration as u64,
                placement: input.placement,
                alpha: input.layer.opacity,
            });
        }
        let fade = FADE_SAMPLES.min(unit_samples(n, fps)) as u32;
        for input in track_inputs(take, fps, unit.in_) {
            let (start, inpoint) = if input.file_ns >= 0 {
                (unit_start, input.file_ns)
            } else {
                (unit_start - input.file_ns, 0)
            };
            let Some(duration) = clamp(unit_end - start, inpoint, input.stream.duration_ns) else {
                continue;
            };
            // The export fades the unit's mix: a track that starts later than
            // the unit, or ends before it, isn't playing at the fade.
            let fade_in = unit.in_ != 0 && start == unit_start;
            let fade_out = unit.out < take.duration_frames && start + duration == unit_end;
            audio.push(GesAudioClip {
                take: unit.take.clone(),
                track: input.track.id.clone(),
                file: input.stream.file.clone(),
                start_ns: start as u64,
                inpoint_ns: inpoint as u64,
                duration_ns: duration as u64,
                volume: 10f64.powf(input.track.gain_db / 20.0),
                fade_in_samples: if fade_in { fade } else { 0 },
                fade_out_samples: if fade_out { fade } else { 0 },
            });
        }
    }
    GesTimeline {
        canvas: canvas.clone(),
        duration_ns: frame_ns(total as i64, fps) as u64,
        video,
        audio,
    }
}

/// How much earlier than "nearest" a layer at another rate switches frames
/// in GES: the renderer's 2 ms Matroska in-point nudge (like the export's
/// seek back-off) and 1 ms for stamps truncated to the millisecond.
const MIXED_RATE_MARGIN_NS: i64 = SEEK_BACKOFF_NS + NS_PER_MS;

/// The in-point of a layer recorded at another rate: the file time of the
/// clip's first canvas frame (`in + lead`) plus half a recorded frame, less
/// [`MIXED_RATE_MARGIN_NS`] (module docs). Never before the file's start.
fn mixed_rate_inpoint(input: &LayerInput<'_>, in_: Frame, fps: Fps) -> i64 {
    let first = frame_ns((in_ + input.selection.lead) as i64, fps);
    let half_frame = frame_ns(1, input.recorded) / 2;
    (first - input.stream.start_offset_ns + half_frame - MIXED_RATE_MARGIN_NS).max(0)
}

/// The duration a clip can have: `wanted`, cut at the media's end when it
/// is known. `None` when nothing is left.
fn clamp(wanted: i64, inpoint: i64, media_end: Option<i64>) -> Option<i64> {
    let available = media_end.map_or(wanted, |end| end - inpoint);
    Some(wanted.min(available)).filter(|&d| d > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::ProjectBuilder;

    const F30: Fps = Fps::new(30, 1);

    /// Two takes on four clips: take 1's camera starts 41 ms late, its mic
    /// 3 ms + 20 ms (sync) late, its system audio 20 ms early at −6 dB;
    /// take 2's mic is muted in its scene.
    fn spec_like_project() -> Project {
        ProjectBuilder::new()
            .screen_layer("L1", "DP-4")
            .camera_layer("L2", "/dev/video0")
            .mic("A1", "mic")
            .system("A2", "monitor")
            .take("take-0001", 912)
            .take("take-0002", 300)
            .with_take("take-0001", |take| {
                take.scene.layers[1].opacity = 0.8;
                take.scene.audio[0].sync_offset_ms = 20;
                take.scene.audio[1].gain_db = -6.0;
                for s in &mut take.streams {
                    s.start_offset_ns = match s.id.as_str() {
                        "L2" => 41_000_000,
                        "A1" => 3_000_000,
                        "A2" => -20_000_000,
                        _ => 0,
                    };
                }
            })
            .with_take("take-0002", |take| take.scene.audio[0].muted = true)
            .clip("c7f3", "take-0001", 0, 410)
            .clip("c8a1", "take-0001", 455, 912)
            .clip("c9b2", "take-0002", 100, 200)
            .clip("c9b3", "take-0002", 200, 210)
            .build()
    }

    #[test]
    fn spec_like_timeline() {
        let project = spec_like_project();
        let ges = plan_ges(&project);
        assert_eq!(ges.duration_ns, frame_ns(410 + 457 + 110, F30) as u64);
        // Video: the camera of the first unit starts one frame late.
        let camera = &ges.video[1];
        assert_eq!(
            (camera.take.as_str(), camera.layer.as_str()),
            ("take-0001", "L2"),
            "clips name their take and layer"
        );
        assert_eq!(camera.layer_priority, 0, "the camera is on top");
        assert_eq!((camera.start_ns, camera.inpoint_ns), (33_333_333, 0));
        assert_eq!(
            camera.start_ns + camera.duration_ns,
            frame_ns(410, F30) as u64
        );
        // Second unit: take frame 455, camera frame 454.
        assert_eq!(ges.video[3].inpoint_ns, frame_ns(454, F30) as u64);
        // Audio: the mic is 3 ms + 20 ms late at the take start.
        let mic = &ges.audio[0];
        assert_eq!((mic.take.as_str(), mic.track.as_str()), ("take-0001", "A1"));
        assert_eq!((mic.start_ns, mic.inpoint_ns), (23_000_000, 0));
        let tracks: Vec<(&str, &str)> = ges
            .audio
            .iter()
            .map(|a| (a.take.as_str(), a.track.as_str()))
            .collect();
        assert_eq!(
            tracks,
            [
                ("take-0001", "A1"),
                ("take-0001", "A2"),
                ("take-0001", "A1"),
                ("take-0001", "A2"),
                ("take-0002", "A2"),
            ],
            "take 2's muted mic isn't played"
        );
        assert_eq!((mic.fade_in_samples, mic.fade_out_samples), (0, 240));
        assert!((ges.audio[1].volume - 0.501_187).abs() < 1e-6);
        assert_eq!(
            ges.audio[1].inpoint_ns, 20_000_000,
            "started 20 ms before T0"
        );
        // The continuous clips of take 2 are one unit; its mic is muted.
        assert_eq!(ges.video.len(), 6);
        assert_eq!(ges.audio.len(), 5);
        insta::assert_json_snapshot!("ges_spec_like", ges);
    }

    #[test]
    fn fades_follow_the_export() {
        let project = ProjectBuilder::new()
            .mic("A1", "mic")
            .system("A2", "monitor")
            .take("take-0001", 300)
            .with_take("take-0001", |take| {
                // A2 starts 100 ms (3 frames) after T0.
                take.streams[1].start_offset_ns = 100_000_000;
            })
            .clip("c1", "take-0001", 0, 100)
            .clip("c2", "take-0001", 2, 50)
            .build();
        let ges = plan_ges(&project);
        let fades: Vec<(u64, u32, u32)> = ges
            .audio
            .iter()
            .map(|a| (a.start_ns, a.fade_in_samples, a.fade_out_samples))
            .collect();
        let unit2 = frame_ns(100, F30) as u64;
        assert_eq!(
            fades,
            vec![
                (0, 0, 240),
                (100_000_000, 0, 240),
                (unit2, 240, 240),
                // Delayed past the unit's fade-in: only the fade-out.
                (unit2 + 33_333_334, 0, 240),
            ]
        );
    }

    #[test]
    fn clips_stop_at_the_end_of_their_media() {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .mic("A1", "mic")
            .take("take-0001", 300)
            .with_take("take-0001", |take| {
                // The mic recorded only 5 s; the screen has a frame count.
                take.streams[1].duration_ns = Some(5_000_000_000);
                take.streams[0].frames = Some(300);
            })
            .clip("c1", "take-0001", 120, 200)
            .clip("c2", "take-0001", 280, 300)
            .build();
        let ges = plan_ges(&project);
        // [4 s, 6.67 s) of a 5 s mic: 1 s remains, without a fade-out.
        let mic = &ges.audio[0];
        assert_eq!(
            (mic.inpoint_ns, mic.duration_ns),
            (4_000_000_000, 1_000_000_000)
        );
        assert_eq!((mic.fade_in_samples, mic.fade_out_samples), (240, 0));
        // Nothing of the mic remains in the second unit; the screen does.
        assert_eq!(ges.audio.len(), 1);
        assert_eq!(ges.video.len(), 2);
        assert_eq!(
            ges.video[1].duration_ns,
            (frame_ns(100, F30) - frame_ns(80, F30)) as u64
        );
        assert_eq!(clamp(10, 5, Some(12)), Some(7));
        assert_eq!(clamp(10, 12, Some(12)), None);
        assert_eq!(clamp(10, 0, None), Some(10));
    }

    /// A layer recorded at another rate follows the take's time in every
    /// unit, wherever the unit starts. Modelling the renderer (in-point
    /// nudged 2 ms later, the last frame stamped at or before the output
    /// frame's time, stamps truncated to the millisecond like the recorder's
    /// Matroska), each canvas frame shows the export's frame (`shown_frame`).
    #[test]
    fn layers_at_another_rate_follow_the_take_time() {
        use crate::export_plan::shown_frame;
        for (recorded, canvas) in [
            (Fps::new(25, 1), F30),
            (F30, Fps::new(60, 1)),
            (Fps::new(50, 1), F30),
            (Fps::new(60, 1), F30),
        ] {
            let project = ProjectBuilder::new()
                .canvas(320, 180, canvas)
                .screen_layer("L1", "DP-1")
                .take("take-0001", 400)
                .with_take("take-0001", |take| {
                    take.streams[0].fps = Some(recorded);
                    take.streams[0].frames = None;
                })
                .clip("c1", "take-0001", 0, 37)
                .clip("c2", "take-0001", 101, 160)
                .clip("c3", "take-0001", 201, 202)
                .clip("c4", "take-0001", 203, 260)
                .clip("c5", "take-0001", 102, 103)
                .build();
            let ges = plan_ges(&project);
            assert_eq!(ges.video.len(), 5);
            let stamp = |j: i64| frame_ns(j, recorded) / NS_PER_MS * NS_PER_MS;
            let mut position = 0u64;
            for (clip, unit) in ges.video.iter().zip(&project.timeline.clips) {
                for t in 0..unit.len() {
                    let at = frame_ns((position + t) as i64, canvas);
                    let file = clip.inpoint_ns as i64 + SEEK_BACKOFF_NS + at - clip.start_ns as i64;
                    let shows = (0..).take_while(|&j| stamp(j) <= file).last().unwrap();
                    assert_eq!(
                        shows,
                        shown_frame(recorded, 0, canvas, (unit.in_ + t) as i64),
                        "{recorded} on {canvas}: take frame {}",
                        unit.in_ + t
                    );
                }
                position += unit.len();
            }
        }
    }

    #[test]
    fn empty_timeline() {
        let ges = plan_ges(&ProjectBuilder::new().screen_layer("L1", "DP-1").build());
        assert_eq!(ges.duration_ns, 0);
        assert!(ges.video.is_empty() && ges.audio.is_empty());
    }

    /// `project` with one clip on its timeline: take `take`, `[0, frames)`.
    fn only_take(project: &Project, take: &TakeId) -> Project {
        let mut whole = project.clone();
        let frames = project.take(take).map_or(0, |t| t.duration_frames);
        whole.timeline.clips = vec![crate::model::Clip {
            id: crate::ids::ClipId::from_raw("cfff"),
            take: take.clone(),
            in_: 0,
            out: frames,
        }];
        whole
    }

    #[test]
    fn a_take_plays_whole_in_its_scene() {
        let project = spec_like_project();
        let take1 = TakeId::numbered(1);
        let ges = plan_ges_take(&project, &take1);
        assert_eq!(ges.duration_ns, frame_ns(912, F30) as u64);
        assert_eq!(ges, plan_ges(&only_take(&project, &take1)));
        let video: Vec<(&str, u64, u64, u64)> = ges
            .video
            .iter()
            .map(|c| (c.layer.as_str(), c.start_ns, c.inpoint_ns, c.duration_ns))
            .collect();
        assert_eq!(
            video,
            [
                ("L1", 0, 0, frame_ns(912, F30) as u64),
                // 41 ms late: from the take's frame 1.
                (
                    "L2",
                    33_333_333,
                    0,
                    (frame_ns(912, F30) - 33_333_333) as u64
                ),
            ]
        );
        // Mic 23 ms late, system audio from 20 ms into its file; one unit
        // from the take's first frame to its last: no fades.
        let audio: Vec<(&str, u64, u64, u32, u32)> = ges
            .audio
            .iter()
            .map(|c| {
                (
                    c.track.as_str(),
                    c.start_ns,
                    c.inpoint_ns,
                    c.fade_in_samples,
                    c.fade_out_samples,
                )
            })
            .collect();
        assert_eq!(
            audio,
            [("A1", 23_000_000, 0, 0, 0), ("A2", 0, 20_000_000, 0, 0)]
        );
        insta::assert_json_snapshot!("ges_take", ges);

        // Take 2's scene has its mic muted; it isn't on the timeline whole.
        let take2 = plan_ges_take(&project, &TakeId::numbered(2));
        assert_eq!(take2.duration_ns, frame_ns(300, F30) as u64);
        let tracks: Vec<&str> = take2.audio.iter().map(|c| c.track.as_str()).collect();
        assert_eq!(tracks, ["A2"]);
        // An unknown take plans nothing.
        let none = plan_ges_take(&project, &TakeId::numbered(9));
        assert_eq!(
            (none.duration_ns, none.video.len(), none.audio.len()),
            (0, 0, 0)
        );
    }

    #[test]
    fn lanes_muted_for_playback_drop_their_sound_only() {
        let plan = plan_ges(&spec_like_project());
        let (a1, a2) = (TrackId::numbered(1), TrackId::numbered(2));
        let tracks = |p: &GesTimeline| -> Vec<String> {
            p.audio.iter().map(|c| c.track.to_string()).collect()
        };
        assert_eq!(tracks(&plan), ["A1", "A2", "A1", "A2", "A2"]);
        let none = BTreeSet::new();
        assert_eq!(without_tracks(plan.clone(), None, &none), plan);

        let muted_a1 = without_tracks(plan.clone(), None, &BTreeSet::from([a1.clone()]));
        assert_eq!(tracks(&muted_a1), ["A2", "A2", "A2"]);
        assert_eq!(muted_a1.video, plan.video, "video is untouched");
        assert_eq!(muted_a1.duration_ns, plan.duration_ns);

        let solo_a1 = without_tracks(plan.clone(), Some(&a1), &none);
        assert_eq!(tracks(&solo_a1), ["A1", "A1"]);
        let solo_a2_muted_a1 =
            without_tracks(plan.clone(), Some(&a2), &BTreeSet::from([a1.clone()]));
        assert_eq!(tracks(&solo_a2_muted_a1), ["A2", "A2", "A2"]);
        // A muted solo lane is silent; so is everything else.
        let silent = without_tracks(plan.clone(), Some(&a1), &BTreeSet::from([a1.clone()]));
        assert!(silent.audio.is_empty());
        assert_eq!(silent.video, plan.video);
    }

    #[test]
    fn scaled_to_the_preview_size() {
        let plan = plan_ges(&spec_like_project());
        let small = scaled(plan.clone(), 960, 540);
        assert_eq!((small.canvas.width, small.canvas.height), (960, 540));
        assert_eq!(small.canvas.fps, plan.canvas.fps);
        let rects: Vec<(&str, Rect)> = small
            .video
            .iter()
            .map(|c| (c.layer.as_str(), c.placement.dst))
            .collect();
        // The camera at (1498, 754, 384, 288): its edges at half, rounded to
        // even pixels (749 → 750, 1882 → 942, 754 → 378, 1042 → 522).
        assert_eq!(
            rects[..2],
            [
                ("L1", Rect::new(0, 0, 960, 540)),
                ("L2", Rect::new(750, 378, 192, 144))
            ]
        );
        for (a, b) in small.video.iter().zip(&plan.video) {
            assert_eq!(
                a.placement.src, b.placement.src,
                "crops stay in file pixels"
            );
            assert_eq!(
                (a.start_ns, a.inpoint_ns, a.duration_ns, a.alpha),
                (b.start_ns, b.inpoint_ns, b.duration_ns, b.alpha)
            );
        }
        assert_eq!(small.audio, plan.audio);
        assert_eq!(scaled(plan.clone(), 1920, 1080), plan);
        insta::assert_json_snapshot!("ges_scaled", small);

        // Off-canvas and odd scales: edges round to even pixels, sides stay ≥ 2.
        let mut odd = plan.clone();
        odd.video[1].placement.dst = Rect::new(-100, 1074, 6, 6);
        let small = scaled(odd, 1280, 720);
        assert_eq!(small.video[1].placement.dst, Rect::new(-66, 716, 4, 4));
        let tiny = scaled(plan, 4, 2);
        assert!(
            tiny.video
                .iter()
                .all(|c| c.placement.dst.w >= 2 && c.placement.dst.h >= 2)
        );
    }

    #[test]
    fn fit_sizes() {
        let max = (960, 540);
        assert_eq!(fit_size(1920, 1080, max), (960, 540));
        assert_eq!(fit_size(1280, 720, max), (960, 540));
        assert_eq!(fit_size(960, 540, max), (960, 540));
        assert_eq!(
            fit_size(640, 360, max),
            (640, 360),
            "never larger than the canvas"
        );
        assert_eq!(fit_size(1440, 1080, max), (720, 540), "4:3");
        assert_eq!(fit_size(1080, 1920, max), (304, 540), "portrait");
        assert_eq!(fit_size(2560, 1080, max), (960, 404), "ultrawide");
        assert_eq!(fit_size(3840, 2160, (1920, 1080)), (1920, 1080), "scale 2");
        assert_eq!(fit_size(1920, 1080, (1, 1)), (2, 2));
    }

    mod properties {
        use proptest::prelude::*;

        use super::*;
        use crate::testkit::strategies::arb_project;

        fn arb_lanes() -> impl Strategy<Value = (Option<TrackId>, BTreeSet<TrackId>)> {
            let id = (1u32..=3).prop_map(TrackId::numbered);
            (
                proptest::option::of(id.clone()),
                proptest::collection::btree_set(id, 0..=3),
            )
        }

        proptest! {
            /// Nothing muted, nothing soloed: the plan itself.
            #[test]
            fn no_lane_left_out_is_the_plan(project in arb_project()) {
                let plan = plan_ges(&project);
                prop_assert_eq!(without_tracks(plan.clone(), None, &BTreeSet::new()), plan);
                for take in project.takes.keys() {
                    let plan = plan_ges_take(&project, take);
                    prop_assert_eq!(without_tracks(plan.clone(), None, &BTreeSet::new()), plan);
                }
            }

            /// At the canvas size, scaling changes nothing.
            #[test]
            fn scaled_to_the_canvas_is_the_plan(project in arb_project()) {
                let (w, h) = (project.canvas.width, project.canvas.height);
                let plan = plan_ges(&project);
                prop_assert_eq!(scaled(plan.clone(), w, h), plan);
                for take in project.takes.keys() {
                    let plan = plan_ges_take(&project, take);
                    prop_assert_eq!(scaled(plan.clone(), w, h), plan);
                }
            }

            /// Leaving lanes out keeps the video and exactly the sound of
            /// the lanes that play, in order.
            #[test]
            fn left_out_lanes_are_silent(project in arb_project(), (solo, muted) in arb_lanes()) {
                let plan = plan_ges(&project);
                let played = without_tracks(plan.clone(), solo.as_ref(), &muted);
                prop_assert_eq!(&played.video, &plan.video);
                prop_assert_eq!(played.duration_ns, plan.duration_ns);
                let plays = |t: &TrackId| !muted.contains(t) && solo.as_ref().is_none_or(|s| s == t);
                let expected: Vec<&GesAudioClip> = plan.audio.iter().filter(|c| plays(&c.track)).collect();
                prop_assert_eq!(played.audio.iter().collect::<Vec<_>>(), expected);
            }

            /// A scaled clip keeps its timing and source and lands within a
            /// pixel or two of its exactly scaled rect.
            #[test]
            fn scaled_rects_follow_the_scale(
                project in arb_project(),
                w in 2u32..4000,
                h in 2u32..4000,
            ) {
                let plan = plan_ges(&project);
                let small = scaled(plan.clone(), w, h);
                prop_assert_eq!((small.canvas.width, small.canvas.height), (w, h));
                prop_assert_eq!(small.video.len(), plan.video.len());
                let (sx, sy) = (
                    f64::from(w) / f64::from(plan.canvas.width),
                    f64::from(h) / f64::from(plan.canvas.height),
                );
                for (a, b) in small.video.iter().zip(&plan.video) {
                    prop_assert_eq!(a.placement.src, b.placement.src);
                    prop_assert_eq!((a.start_ns, a.duration_ns), (b.start_ns, b.duration_ns));
                    let (da, db) = (a.placement.dst, b.placement.dst);
                    prop_assert!(da.x % 2 == 0 && da.y % 2 == 0 && da.w % 2 == 0 && da.h % 2 == 0);
                    let near = |got: i64, exact: f64| (got as f64 - exact).abs() <= 2.0;
                    prop_assert!(near(i64::from(da.x), f64::from(db.x) * sx));
                    prop_assert!(near(i64::from(da.y), f64::from(db.y) * sy));
                    prop_assert!(near(i64::from(da.x) + i64::from(da.w), (f64::from(db.x) + f64::from(db.w)) * sx) || da.w == 2);
                    prop_assert!(near(i64::from(da.y) + i64::from(da.h), (f64::from(db.y) + f64::from(db.h)) * sy) || da.h == 2);
                }
                prop_assert_eq!(&small.audio, &plan.audio);
            }

            /// A take's plan is the program plan of a timeline holding only
            /// that take, whole.
            #[test]
            fn a_take_plan_is_the_take_alone_on_the_timeline(project in arb_project()) {
                for take in project.takes.keys() {
                    prop_assert_eq!(plan_ges_take(&project, take), plan_ges(&only_take(&project, take)));
                }
            }

            /// The fitted size fits, keeps the aspect within a pixel or two,
            /// and is the canvas when the canvas fits.
            #[test]
            fn fitted_sizes_fit(w in 2u32..8000, h in 2u32..8000, mw in 2u32..4000, mh in 2u32..4000) {
                let (fw, fh) = fit_size(w, h, (mw, mh));
                if w <= mw && h <= mh {
                    prop_assert_eq!((fw, fh), (w, h));
                } else {
                    prop_assert!(fw <= mw.max(2) && fh <= mh.max(2));
                    prop_assert!(fw % 2 == 0 && fh % 2 == 0 && fw >= 2 && fh >= 2);
                    prop_assert!(fw == mw & !1 || fh == mh & !1 || fw == 2 || fh == 2);
                    // Aspect: within the rounding of the shorter side.
                    let exact_h = f64::from(fw) * f64::from(h) / f64::from(w);
                    let exact_w = f64::from(fh) * f64::from(w) / f64::from(h);
                    prop_assert!((f64::from(fh) - exact_h).abs() <= 3.0 || (f64::from(fw) - exact_w).abs() <= 3.0);
                }
            }
        }
    }
}
