//! Frame-rate changes: clip re-quantization (A§5.4) and take durations
//! (A§2.2). Owner: scene.
//!
//! `derive_duration_frames` and `rederive_durations` are implemented by the
//! foundation (record, scene and history all need them); `requantize` is a
//! signature only.
#![allow(clippy::todo, unused_variables)]

use crate::ids::{ClipId, Frame};
use crate::model::{Clip, Fps, Project, StreamKind, Take, TakeStatus};
use crate::time::{frame_ns, ns_to_frame_floor, ns_to_frame_round};

/// The clips after an fps change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requantized {
    pub clips: Vec<Clip>,
    /// Clips whose bounds changed (for the confirmation `details`).
    pub changed: Vec<ClipChange>,
    /// Clips that ended up empty and are removed in the same entry.
    pub removed: Vec<ClipId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipChange {
    pub id: ClipId,
    pub before: (Frame, Frame),
    pub after: (Frame, Frame),
}

/// Maps every clip of `project` (at its current fps) to `new_fps` (A§5.4):
/// shared boundaries of continuous clips round to nearest, other `in`s
/// floor, other `out`s ceil, clamped to the new `durationFrames`.
pub fn requantize(project: &Project, new_fps: Fps) -> Requantized {
    todo!("scene: requantize")
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
    if let Some(frames) = video {
        return frames;
    }
    take.streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio && !missing(&s.id))
        .filter_map(|s| {
            Some(ns_to_frame_floor(s.start_offset_ns + s.duration_ns?, canvas_fps).max(0) as Frame)
        })
        .min()
        .unwrap_or(0)
}

/// Re-derives `durationFrames` of every take that isn't recording (after
/// an fps change, including undo/redo of one).
pub fn rederive_durations(project: &mut Project) {
    let fps = project.canvas.fps;
    for take in project
        .takes
        .values_mut()
        .filter(|t| t.status != TakeStatus::Recording)
    {
        take.duration_frames = derive_duration_frames(take, fps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::ProjectBuilder;

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
    }
}
