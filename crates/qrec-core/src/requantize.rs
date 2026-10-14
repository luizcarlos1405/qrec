//! Frame-rate changes: clip re-quantization (A§5.4) and take durations
//! (A§2.2). Owner: scene.
//!
//! Clip bounds are integer frames at the canvas rate, so a new canvas fps
//! maps every bound through nanoseconds (`frame_ns` at the old rate, then a
//! frame at the new rate):
//! - a boundary two continuous clips share (same take, `out == next.in`,
//!   e.g. a split) is mapped once, to the nearest frame, so the pair stays
//!   continuous and renders as one unit;
//! - every other `in` rounds down and every other `out` rounds up, so a clip
//!   never loses media at a free edge;
//! - bounds are clamped to the take's `durationFrames` at the new rate, and
//!   clips that end up empty are removed (and reported separately).
//!
//! Take durations are re-derived from the streams (A§2.2). A take whose
//! streams carry no media info (hand-written projects such as the SPEC §6.3
//! example) can't be derived: its duration is converted through nanoseconds
//! instead of collapsing to 0, which would empty every clip of it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{ClipId, Frame, TakeId};
use crate::model::{Clip, Fps, Project, StreamKind, Take, TakeStatus};
use crate::time::{frame_ns, ns_to_frame_ceil, ns_to_frame_floor, ns_to_frame_round};

/// What changing the canvas fps does to a project (A§5.4): the plan the
/// confirmation lists and [`apply_requantized`] carries out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Requantized {
    /// The current canvas fps.
    pub from: Fps,
    /// The new canvas fps.
    pub to: Fps,
    /// The new timeline: the surviving clips, in their original order.
    pub clips: Vec<Clip>,
    /// Every surviving clip with its bounds before and after. An fps change
    /// re-times all of them, so the confirmation lists them all (SPEC §11).
    pub mapped: Vec<ClipChange>,
    /// Clips that ended up empty; `after` is the empty range they collapsed
    /// to. They are removed in the same history entry and listed separately.
    pub removed: Vec<ClipChange>,
    /// Every take's `durationFrames` at the new rate; clips are clamped to it.
    pub durations: BTreeMap<TakeId, Frame>,
}

/// One clip's bounds before and after re-quantization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipChange {
    pub id: ClipId,
    pub take: TakeId,
    /// `[in, out)` at the old rate.
    pub before: (Frame, Frame),
    /// `[in, out)` at the new rate.
    pub after: (Frame, Frame),
}

/// Plans the change of `project`'s canvas fps to `new_fps` (A§5.4). Pure:
/// the project is not touched; see [`apply_requantized`].
pub fn requantize(project: &Project, new_fps: Fps) -> Requantized {
    let from = project.canvas.fps;
    let durations = durations_at(project, from, new_fps);
    let (clips, mapped, removed) =
        requantize_clips(&project.timeline.clips, from, new_fps, |take| {
            durations.get(take).copied()
        });
    Requantized {
        from,
        to: new_fps,
        clips,
        mapped,
        removed,
        durations,
    }
}

/// Carries out `plan` on `project` (whose canvas is still at `plan.from`):
/// the canvas fps, the timeline, every take's `durationFrames`, and the
/// clips a record-over keeps in `Take.replaced` (re-quantized the same way,
/// so discarding the take puts back clips that fit).
pub fn apply_requantized(project: &mut Project, plan: &Requantized) {
    project.canvas.fps = plan.to;
    project.timeline.clips = plan.clips.clone();
    retime_take_facts(project, plan.from, plan.to, &plan.durations);
}

/// Maps a list of clips from `from` to `to` fps with the A§5.4 rules.
/// `duration` gives each take's length at `to` (`None`: don't clamp).
/// Returns the surviving clips, their changes, and the removed ones.
pub fn requantize_clips(
    clips: &[Clip],
    from: Fps,
    to: Fps,
    duration: impl Fn(&TakeId) -> Option<Frame>,
) -> (Vec<Clip>, Vec<ClipChange>, Vec<ClipChange>) {
    let continuous = |a: &Clip, b: &Clip| a.take == b.take && a.out == b.in_;
    let mut kept = Vec::with_capacity(clips.len());
    let mut mapped = Vec::with_capacity(clips.len());
    let mut removed = Vec::new();
    for (i, clip) in clips.iter().enumerate() {
        let shared_in = i > 0 && continuous(&clips[i - 1], clip);
        let shared_out = clips.get(i + 1).is_some_and(|next| continuous(clip, next));
        let mut in_ = map_frame(
            clip.in_,
            from,
            to,
            if shared_in { Edge::Nearest } else { Edge::Down },
        );
        let mut out = map_frame(
            clip.out,
            from,
            to,
            if shared_out { Edge::Nearest } else { Edge::Up },
        );
        if let Some(limit) = duration(&clip.take) {
            in_ = in_.min(limit);
            out = out.min(limit);
        }
        let change = ClipChange {
            id: clip.id.clone(),
            take: clip.take.clone(),
            before: (clip.in_, clip.out),
            after: (in_, out),
        };
        if in_ < out {
            kept.push(Clip {
                in_,
                out,
                ..clip.clone()
            });
            mapped.push(change);
        } else {
            removed.push(change);
        }
    }
    (kept, mapped, removed)
}

#[derive(Clone, Copy)]
enum Edge {
    /// `ns_to_frame_floor`: the frame containing the old instant.
    Down,
    /// `ns_to_frame_ceil`: the first frame starting at or after it.
    Up,
    /// `ns_to_frame_round`: the frame starting nearest to it.
    Nearest,
}

/// Frame `f` at `from` as a frame at `to` (the identity when they're equal).
fn map_frame(f: Frame, from: Fps, to: Fps, edge: Edge) -> Frame {
    if from == to {
        return f;
    }
    let ns = frame_ns(f as i64, from);
    let mapped = match edge {
        Edge::Down => ns_to_frame_floor(ns, to),
        Edge::Up => ns_to_frame_ceil(ns, to),
        Edge::Nearest => ns_to_frame_round(ns, to),
    };
    mapped.max(0) as Frame
}

/// After an undo, redo or external edit changed the canvas fps from
/// `previous_fps` to `project.canvas.fps` (A§2.2, A§7.4): re-derives every
/// take's `durationFrames` (converting the ones without media info through
/// nanoseconds) and re-quantizes `Take.replaced`. Clip bounds are not
/// touched: the patch restores them. A no-op when the fps didn't change.
pub fn retime_takes(project: &mut Project, previous_fps: Fps) {
    let to = project.canvas.fps;
    if previous_fps == to {
        return;
    }
    let durations = durations_at(project, previous_fps, to);
    retime_take_facts(project, previous_fps, to, &durations);
}

/// Sets the take durations and re-quantizes `Take.replaced` (whose clips
/// usually belong to other takes, hence the shared `durations`).
fn retime_take_facts(
    project: &mut Project,
    from: Fps,
    to: Fps,
    durations: &BTreeMap<TakeId, Frame>,
) {
    for (id, take) in project.takes.iter_mut() {
        if let Some(&frames) = durations.get(id) {
            take.duration_frames = frames;
        }
        if !take.replaced.is_empty() {
            let (kept, _, _) =
                requantize_clips(&take.replaced, from, to, |t| durations.get(t).copied());
            take.replaced = kept;
        }
    }
}

/// Every take's `durationFrames` at `to`, for a project whose clips are at `from`.
fn durations_at(project: &Project, from: Fps, to: Fps) -> BTreeMap<TakeId, Frame> {
    project
        .takes
        .iter()
        .map(|(id, take)| (id.clone(), duration_at(take, from, to)))
        .collect()
}

/// A take's length at `to`: derived from its streams when they say
/// (A§2.2), else its current length converted from `from`. A take that is
/// still recording keeps its value (0 until it is finalized).
fn duration_at(take: &Take, from: Fps, to: Fps) -> Frame {
    if take.status == TakeStatus::Recording {
        return take.duration_frames;
    }
    derived_duration(take, to).unwrap_or_else(|| rescale_duration(take.duration_frames, from, to))
}

/// `frames` at `from` as a length at `to`, rounded like a recorded stream
/// at another rate (A§2.2: `floor(end_ns + frame_ns(1)/2)`).
fn rescale_duration(frames: Frame, from: Fps, to: Fps) -> Frame {
    if from == to {
        return frames;
    }
    let end = frame_ns(frames as i64, from);
    ns_to_frame_floor(end + frame_ns(1, to) / 2, to).max(0) as Frame
}

/// A take's `durationFrames` at `canvas_fps`, from its streams (A§2.2).
///
/// Video streams count unless their layer is marked `missing` in the take
/// scene or they have no probed `frames`; with `off =
/// ns_to_frame_round(startOffsetNs)`, a stream at the canvas rate ends at
/// `off + frames`, otherwise at `ns_to_frame_floor(startOffsetNs +
/// frame_ns_rec(frames) + frame_ns(1)/2)`. The duration is the minimum over
/// those streams. Without video it is `ns_to_frame_floor(startOffsetNs +
/// durationNs)` of the shortest audio stream; 0 without media info.
pub fn derive_duration_frames(take: &Take, canvas_fps: Fps) -> Frame {
    derived_duration(take, canvas_fps).unwrap_or(0)
}

/// [`derive_duration_frames`], or `None` when no stream carries the media
/// info to derive it from (no probed video `frames`, no audio `durationNs`).
pub fn derived_duration(take: &Take, canvas_fps: Fps) -> Option<Frame> {
    let missing = |id: &str| {
        take.scene
            .layers
            .iter()
            .any(|l| l.missing && l.id.as_str() == id)
            || take
                .scene
                .audio
                .iter()
                .any(|t| t.missing && t.id.as_str() == id)
    };
    let video = take
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Video && !missing(&s.id))
        .filter_map(|s| {
            let frames = s.frames? as i64;
            let rec = s.fps.unwrap_or(canvas_fps);
            let end = if rec == canvas_fps {
                ns_to_frame_round(s.start_offset_ns, canvas_fps) + frames
            } else {
                let ns = s.start_offset_ns + frame_ns(frames, rec) + frame_ns(1, canvas_fps) / 2;
                ns_to_frame_floor(ns, canvas_fps)
            };
            Some(end.max(0) as Frame)
        })
        .min();
    if video.is_some() {
        return video;
    }
    take.streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio && !missing(&s.id))
        .filter_map(|s| {
            Some(ns_to_frame_floor(s.start_offset_ns + s.duration_ns?, canvas_fps).max(0) as Frame)
        })
        .min()
}

/// Re-derives `durationFrames` of every take that isn't recording from its
/// streams at the current canvas fps (A§2.2). Takes whose streams carry no
/// media info keep their value; when the previous fps is known, prefer
/// [`retime_takes`], which converts those too.
pub fn rederive_durations(project: &mut Project) {
    let fps = project.canvas.fps;
    for take in project
        .takes
        .values_mut()
        .filter(|t| t.status != TakeStatus::Recording)
    {
        if let Some(frames) = derived_duration(take, fps) {
            take.duration_frames = frames;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::ProjectBuilder;

    const FPS30: Fps = Fps::new(30, 1);
    const FPS60: Fps = Fps::new(60, 1);
    const NTSC: Fps = Fps::new(30000, 1001);

    fn clip(id: &str, take: &str, in_: Frame, out: Frame) -> Clip {
        Clip {
            id: ClipId::from_raw(id),
            take: take.parse().unwrap(),
            in_,
            out,
        }
    }

    fn bounds(clips: &[Clip]) -> Vec<(&str, Frame, Frame)> {
        clips
            .iter()
            .map(|c| (c.id.as_str(), c.in_, c.out))
            .collect()
    }

    #[test]
    fn durations_from_streams() {
        let fps = Fps::new(30, 1);
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .mic("A1", "mic")
            .take("take-0001", 300)
            .build();
        let mut take = project.takes.values().next().unwrap().clone();
        assert_eq!(derive_duration_frames(&take, fps), 300);

        // A late camera ends earlier: the shortest stream wins.
        let mut camera = take.streams[0].clone();
        camera.id = "L2".into();
        camera.start_offset_ns = 41_000_000; // rounds to frame 1
        camera.frames = Some(250);
        take.streams.push(camera);
        assert_eq!(derive_duration_frames(&take, fps), 251);

        // A recorded rate different from the canvas.
        take.streams[2].fps = Some(Fps::new(60, 1));
        take.streams[2].frames = Some(300); // 5 s
        assert_eq!(derive_duration_frames(&take, fps), 151);

        // Missing layers don't count; audio-only falls back to durations.
        take.scene.layers[0].missing = true;
        take.streams.retain(|s| s.id != "L2");
        assert_eq!(
            derive_duration_frames(&take, fps),
            300,
            "audio: 10 s at 30 fps"
        );
        take.streams.retain(|s| s.kind == StreamKind::Audio);
        take.streams[0].duration_ns = Some(1_000_000_000);
        take.streams[0].start_offset_ns = 3_000_000;
        assert_eq!(derive_duration_frames(&take, fps), 30);
        take.streams[0].duration_ns = None;
        assert_eq!(derive_duration_frames(&take, fps), 0);
        assert_eq!(derived_duration(&take, fps), None);
    }

    #[test]
    fn same_rate_is_the_identity() {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 300)
            .clip("c001", "take-0001", 0, 100)
            .clip("c002", "take-0001", 100, 300)
            .clip("c003", "take-0001", 7, 9)
            .build();
        let plan = requantize(&project, FPS30);
        assert_eq!(plan.clips, project.timeline.clips);
        assert!(plan.removed.is_empty());
        assert!(plan.mapped.iter().all(|c| c.before == c.after));
    }

    #[test]
    fn doubling_the_rate_doubles_every_bound() {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 300)
            .clip("c001", "take-0001", 0, 100)
            .clip("c002", "take-0001", 100, 250)
            .clip("c003", "take-0001", 30, 31)
            .build();
        let plan = requantize(&project, FPS60);
        assert_eq!(
            bounds(&plan.clips),
            [("c001", 0, 200), ("c002", 200, 500), ("c003", 60, 62)]
        );
        assert_eq!(plan.durations[&"take-0001".parse::<TakeId>().unwrap()], 600);
        assert_eq!(plan.mapped.len(), 3);
    }

    #[test]
    fn free_edges_widen_and_shared_ones_round() {
        // 60 → 30 fps: free edges floor/ceil, the split at 11 rounds to 6
        // (half up), so a 1-frame clip between two splits can vanish.
        let project = ProjectBuilder::new()
            .canvas(1920, 1080, FPS60)
            .screen_layer("L1", "DP-1")
            .take("take-0001", 600)
            .take("take-0002", 600)
            .clip("c001", "take-0001", 3, 11)
            .clip("c002", "take-0001", 11, 12)
            .clip("c003", "take-0001", 12, 41)
            .clip("c004", "take-0002", 5, 6)
            .build();
        let plan = requantize(&project, FPS30);
        assert_eq!(
            bounds(&plan.clips),
            [("c001", 1, 6), ("c003", 6, 21), ("c004", 2, 3)]
        );
        assert_eq!(plan.removed.len(), 1);
        assert_eq!(plan.removed[0].id.as_str(), "c002");
        assert_eq!(plan.removed[0].before, (11, 12));
        assert_eq!(plan.removed[0].after, (6, 6));
        // The split pair c001|c003 stays continuous once c002 is gone.
        assert_eq!(plan.clips[0].out, plan.clips[1].in_);
    }

    #[test]
    fn other_rates_map_through_nanoseconds() {
        // 30 → 29.97: 300 frames recorded at 30 fps last 10 s, which A§2.2
        // rounds to 300 frames at 29.97 (299.7 plus half a frame). The split
        // at 5 s rounds to frame 150; the free end at 10 s ceils to 300.
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 300)
            .clip("c001", "take-0001", 0, 150)
            .clip("c002", "take-0001", 150, 300)
            .build();
        let plan = requantize(&project, NTSC);
        let take: TakeId = "take-0001".parse().unwrap();
        assert_eq!(plan.durations[&take], 300);
        assert_eq!(bounds(&plan.clips), [("c001", 0, 150), ("c002", 150, 300)]);

        // The stream says it was recorded at 30 fps: other rates re-derive
        // its length exactly.
        assert_eq!(project.takes[&take].streams[0].fps, Some(FPS30));
        let plan = requantize(&project, FPS60);
        assert_eq!(plan.durations[&take], 600);
        let plan = requantize(&project, Fps::new(24, 1));
        assert_eq!(plan.durations[&take], 240);
        assert_eq!(bounds(&plan.clips), [("c001", 0, 120), ("c002", 120, 240)]);
    }

    #[test]
    fn clips_past_a_shorter_take_are_removed() {
        let mut project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 300)
            .clip("c001", "take-0001", 0, 280)
            .clip("c002", "take-0001", 290, 300)
            .build();
        // The stream really holds 9 s (135 frames at 15 fps): at 60 fps the
        // take is 540 frames, shorter than the clips' 600-frame bounds.
        let take: TakeId = "take-0001".parse().unwrap();
        project.takes.get_mut(&take).unwrap().streams[0].frames = Some(135);
        project.takes.get_mut(&take).unwrap().streams[0].fps = Some(Fps::new(15, 1));
        let plan = requantize(&project, FPS60);
        assert_eq!(plan.durations[&take], 540);
        assert_eq!(bounds(&plan.clips), [("c001", 0, 540)]);
        assert_eq!(plan.removed[0].after, (540, 540));
    }

    #[test]
    fn takes_without_media_info_are_converted_not_zeroed() {
        let mut project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 912)
            .clip("c7f3", "take-0001", 0, 410)
            .clip("c8a1", "take-0001", 455, 912)
            .build();
        let take: TakeId = "take-0001".parse().unwrap();
        // Like the SPEC §6.3 example: streams without frames or durations.
        for s in &mut project.takes.get_mut(&take).unwrap().streams {
            s.frames = None;
            s.duration_ns = None;
            s.fps = None;
        }
        let plan = requantize(&project, FPS60);
        assert_eq!(plan.durations[&take], 1824);
        assert_eq!(bounds(&plan.clips), [("c7f3", 0, 820), ("c8a1", 910, 1824)]);

        // rederive keeps what it can't derive; retime converts it.
        let mut p = project.clone();
        p.canvas.fps = FPS60;
        rederive_durations(&mut p);
        assert_eq!(p.takes[&take].duration_frames, 912);
        retime_takes(&mut p, FPS30);
        assert_eq!(p.takes[&take].duration_frames, 1824);
        retime_takes(&mut p, FPS60);
        assert_eq!(p.takes[&take].duration_frames, 1824, "same fps: no-op");
    }

    #[test]
    fn applying_a_plan_retimes_the_takes() {
        let mut project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 300)
            .take("take-0002", 90)
            .clip("c001", "take-0002", 0, 90)
            .build();
        // take-0002 recorded over c00a|c00b of take-0001.
        project
            .takes
            .get_mut(&"take-0002".parse::<TakeId>().unwrap())
            .unwrap()
            .replaced = vec![
            clip("c00a", "take-0001", 10, 20),
            clip("c00b", "take-0001", 20, 299),
        ];
        let plan = requantize(&project, FPS60);
        apply_requantized(&mut project, &plan);
        assert_eq!(project.canvas.fps, FPS60);
        assert_eq!(bounds(&project.timeline.clips), [("c001", 0, 180)]);
        let t1 = &project.takes[&"take-0001".parse::<TakeId>().unwrap()];
        let t2 = &project.takes[&"take-0002".parse::<TakeId>().unwrap()];
        assert_eq!((t1.duration_frames, t2.duration_frames), (600, 180));
        assert_eq!(bounds(&t2.replaced), [("c00a", 20, 40), ("c00b", 40, 598)]);

        // Undo (the patch restores 30 fps and the clips) then retime.
        project.canvas.fps = FPS30;
        retime_takes(&mut project, FPS60);
        let t2 = &project.takes[&"take-0002".parse::<TakeId>().unwrap()];
        assert_eq!(t2.duration_frames, 90);
        assert_eq!(bounds(&t2.replaced), [("c00a", 10, 20), ("c00b", 20, 299)]);
    }

    #[test]
    fn recording_takes_keep_their_duration() {
        let mut project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 300)
            .build();
        let take: TakeId = "take-0001".parse().unwrap();
        let t = project.takes.get_mut(&take).unwrap();
        t.status = TakeStatus::Recording;
        t.duration_frames = 0;
        assert_eq!(requantize(&project, FPS60).durations[&take], 0);
        project.canvas.fps = FPS60;
        rederive_durations(&mut project);
        assert_eq!(project.takes[&take].duration_frames, 0);
    }

    #[test]
    fn plan_snapshot() {
        let project = ProjectBuilder::new()
            .canvas(1920, 1080, FPS60)
            .screen_layer("L1", "DP-1")
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 600)
            .take("take-0002", 125)
            .clip("c001", "take-0001", 0, 11)
            .clip("c002", "take-0001", 11, 12)
            .clip("c003", "take-0001", 12, 400)
            .clip("c004", "take-0002", 3, 125)
            .clip("c005", "take-0001", 401, 600)
            .build();
        insta::assert_yaml_snapshot!("plan_60_to_29_97", requantize(&project, NTSC));
    }
}
