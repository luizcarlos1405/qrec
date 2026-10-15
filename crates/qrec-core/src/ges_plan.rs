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

use crate::export_plan::{
    FADE_SAMPLES, LayerInput, SEEK_BACKOFF_NS, layer_inputs, render_units, track_inputs,
    unit_samples,
};
use crate::ids::Frame;
use crate::model::{Fps, Project};
use crate::plans::{GesAudioClip, GesTimeline, GesVideoClip};
use crate::time::{NS_PER_MS, frame_ns};
use crate::timeline::total_frames;

/// The timeline as GES clips: same render units, frame selection and lead
/// as the export plan (A§9.3).
pub fn plan_ges(project: &Project) -> GesTimeline {
    let canvas = &project.canvas;
    let fps = canvas.fps;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for unit in render_units(project) {
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
        duration_ns: frame_ns(total_frames(&project.timeline.clips) as i64, fps) as u64,
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

    #[test]
    fn spec_like_timeline() {
        let project = ProjectBuilder::new()
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
            .build();
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
}
