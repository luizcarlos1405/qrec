//! Properties of the scene owner's code (A§5.4, A§4.4):
//! - fps re-quantization keeps split pairs continuous, never leaves an
//!   empty clip (it removes it instead), never cuts media at a free edge,
//!   and is what a confirmed `ProjectSet` applies;
//! - `SceneApply` never changes a source or a `missing` flag, never removes
//!   anything, and only adds back what a take has a stream for.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use proptest::sample::Index;
use qrec_core::apply::project::project_set;
use qrec_core::apply::scene::{audio_remove, layer_remove, scene_apply};
use qrec_core::protocol::command::{CanvasSpec, Nullable, SceneDest, SceneTarget};
use qrec_core::requantize::requantize;
use qrec_core::testkit;
use qrec_core::testkit::strategies::{arb_fps, arb_project};
use qrec_core::time::frame_ns;
use qrec_core::{
    Clip, ClipId, Ctx, ErrorCode, Fps, Layer, LayerId, Project, Scene, Stream, StreamKind, Take,
    TakeId, VideoSource,
};

fn ctx() -> Ctx {
    testkit::ctx("2026-10-14T12:30:00Z", 11)
}

fn continuous(a: &Clip, b: &Clip) -> bool {
    a.take == b.take && a.out == b.in_
}

fn ns(frame: u64, fps: Fps) -> i64 {
    frame_ns(frame as i64, fps)
}

// ------------------------------------------------------------ requantize

/// Arbitrary projects whose clips are split again and again at random
/// points, so the timeline has continuous runs and 1-frame pieces; the
/// first take may lose its media info (a hand-written project).
fn arb_split_project() -> impl Strategy<Value = Project> {
    (
        arb_project(),
        prop::collection::vec((any::<Index>(), 0u64..40), 0..12),
        any::<bool>(),
    )
        .prop_map(|(mut project, splits, strip)| {
            for (k, (index, offset)) in splits.into_iter().enumerate() {
                let clips = &mut project.timeline.clips;
                if clips.is_empty() {
                    break;
                }
                let i = index.index(clips.len());
                let clip = clips[i].clone();
                if clip.len() < 2 {
                    continue;
                }
                let at = clip.in_ + 1 + offset % (clip.len() - 1);
                clips[i].out = at;
                let right = Clip {
                    id: ClipId::from_raw(format!("d{k:03}")),
                    in_: at,
                    ..clip
                };
                clips.insert(i + 1, right);
            }
            if strip && let Some(take) = project.takes.values_mut().next() {
                for stream in &mut take.streams {
                    stream.frames = None;
                    stream.duration_ns = None;
                    stream.fps = None;
                }
            }
            project
        })
}

proptest! {
    // Removals need a lower rate and 1-frame pieces at split points: about
    // one draw in thirteen, so draw more than the default 256.
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn requantize_keeps_split_pairs_and_never_leaves_empty_clips(
        project in arb_split_project(),
        to in arb_fps(),
    ) {
        let from = project.canvas.fps;
        let input = &project.timeline.clips;
        let plan = requantize(&project, to);

        // The output is the input minus the removed clips, in order.
        let removed: BTreeSet<&ClipId> = plan.removed.iter().map(|c| &c.id).collect();
        let expected: Vec<&ClipId> = input.iter().map(|c| &c.id).filter(|id| !removed.contains(id)).collect();
        let kept: Vec<&ClipId> = plan.clips.iter().map(|c| &c.id).collect();
        prop_assert_eq!(kept, expected);
        prop_assert_eq!(plan.mapped.len() + plan.removed.len(), input.len());

        // Never an empty clip, never past the take's new length.
        for clip in &plan.clips {
            prop_assert!(clip.in_ < clip.out, "empty clip {:?}", clip);
            prop_assert!(clip.out <= plan.durations[&clip.take], "{:?} past the take", clip);
        }
        for change in &plan.removed {
            prop_assert!(change.after.0 >= change.after.1, "removed but not empty: {:?}", change);
        }

        let after: BTreeMap<&ClipId, (usize, &Clip)> =
            plan.clips.iter().enumerate().map(|(i, c)| (&c.id, (i, c))).collect();
        let half = frame_ns(1, to) / 2 + 1;
        let mut run: Option<(usize, &Clip)> = None; // last survivor of the current run
        for (i, clip) in input.iter().enumerate() {
            let shared_in = i > 0 && continuous(&input[i - 1], clip);
            let shared_out = input.get(i + 1).is_some_and(|next| continuous(clip, next));
            if !shared_in {
                run = None;
            }
            let Some(&(pos, new)) = after.get(&clip.id) else { continue };
            // Split pairs stay next to each other and continuous.
            if let Some((prev_pos, prev)) = run {
                prop_assert_eq!(prev_pos + 1, pos, "{} | {} are no longer adjacent", &prev.id, &new.id);
                prop_assert_eq!(prev.out, new.in_, "split pair {} | {} drifted apart", &prev.id, &new.id);
            }
            run = Some((pos, new));

            let limit = plan.durations[&clip.take];
            // Free edges never lose media; shared ones move by at most half a frame.
            if shared_in {
                prop_assert!((ns(new.in_, to) - ns(clip.in_, from)).abs() <= half, "{:?} → {:?}", clip, new);
            } else {
                prop_assert!(ns(new.in_, to) <= ns(clip.in_, from), "{:?} → {:?}", clip, new);
            }
            if new.out < limit {
                if shared_out {
                    prop_assert!((ns(new.out, to) - ns(clip.out, from)).abs() <= half, "{:?} → {:?}", clip, new);
                } else {
                    prop_assert!(ns(new.out, to) >= ns(clip.out, from), "{:?} → {:?}", clip, new);
                }
            }
        }
    }

    #[test]
    fn requantize_to_the_same_rate_changes_nothing(project in arb_split_project()) {
        let plan = requantize(&project, project.canvas.fps);
        prop_assert_eq!(&plan.clips, &project.timeline.clips);
        prop_assert!(plan.removed.is_empty());
        for (id, take) in &project.takes {
            prop_assert_eq!(plan.durations[id], take.duration_frames);
        }
    }

    #[test]
    fn project_set_applies_the_plan_after_confirmation(
        project in arb_split_project(),
        to in arb_fps(),
    ) {
        let state = testkit::state(project.clone());
        let spec = CanvasSpec { width: None, height: None, fps: Some(to) };
        let changes = to != project.canvas.fps;

        let unconfirmed = project_set(&state, &ctx(), None, Some(spec), None, None, None, Nullable::Keep);
        if changes {
            let err = unconfirmed.unwrap_err();
            prop_assert_eq!(err.code, ErrorCode::ConfirmationRequired);
            let details = err.details.unwrap();
            let listed = details["clips"].as_array().unwrap().len()
                + details["removedClips"].as_array().unwrap().len();
            prop_assert_eq!(listed, project.timeline.clips.len(), "every clip is listed");
        } else {
            prop_assert!(unconfirmed.is_ok());
        }

        let mut confirmed = ctx();
        confirmed.confirmed = true;
        let out = project_set(&state, &confirmed, None, Some(spec), None, None, None, Nullable::Keep).unwrap();
        let plan = requantize(&project, to);
        let p = &out.state.project;
        prop_assert_eq!(p.canvas.fps, to);
        prop_assert_eq!(&p.timeline.clips, &plan.clips);
        for (id, take) in &p.takes {
            prop_assert_eq!(take.duration_frames, plan.durations[id]);
        }
        for clip in &p.timeline.clips {
            prop_assert!(clip.in_ < clip.out && clip.out <= p.takes[&clip.take].duration_frames);
        }
        let reply = out.reply.unwrap().unwrap();
        let removed: Vec<String> = plan.removed.iter().map(|c| c.id.to_string()).collect();
        prop_assert_eq!(reply.removed, removed);
        prop_assert_eq!(out.state.history.entries.len(), usize::from(changes), "one entry for it all");
    }
}

// ----------------------------------------------------------- scene apply

/// Arbitrary projects whose take scenes drifted from the project scene:
/// entries removed, layouts, names and sources changed, `missing` flags
/// flipped, layers reordered, streams without media, and a project layer
/// that no take recorded.
fn arb_scene_project() -> impl Strategy<Value = Project> {
    (
        arb_project(),
        prop::collection::vec((any::<Index>(), any::<Index>(), 0u8..9, any::<u8>()), 0..20),
    )
        .prop_map(|(mut project, edits)| {
            let ids: Vec<TakeId> = project.takes.keys().cloned().collect();
            for (t, item, op, v) in edits {
                if op == 8 {
                    add_unrecorded_layer(&mut project);
                    continue;
                }
                let take = project.takes.get_mut(&ids[t.index(ids.len())]).unwrap();
                edit_take(take, item, op, v);
            }
            project
        })
}

fn edit_take(take: &mut Take, item: Index, op: u8, v: u8) {
    let scene = &mut take.scene;
    let layers = scene.layers.len();
    let tracks = scene.audio.len();
    match op {
        0 if layers > 0 => {
            scene.layers.remove(item.index(layers));
        }
        1 if tracks > 0 => {
            scene.audio.remove(item.index(tracks));
        }
        2 if layers > 0 => {
            let layer = &mut scene.layers[item.index(layers)];
            layer.missing = !layer.missing;
        }
        3 if tracks > 0 => {
            let track = &mut scene.audio[item.index(tracks)];
            track.missing = !track.missing;
        }
        4 if layers > 0 => {
            let layer = &mut scene.layers[item.index(layers)];
            layer.rect.x += i32::from(v);
            layer.rect.w = 1 + u32::from(v);
            layer.crop.left = u32::from(v);
            layer.opacity = f64::from(v) / 255.0;
            layer.visible = v.is_multiple_of(2);
            layer.fit = if v.is_multiple_of(3) {
                qrec_core::Fit::Cover
            } else {
                qrec_core::Fit::Contain
            };
            layer.locked = !layer.locked;
            layer.name = format!("renamed {v}");
            let (VideoSource::Screen { label, .. } | VideoSource::Camera { label, .. }) =
                &mut layer.source;
            *label = format!("old label {v}");
        }
        5 if tracks > 0 => {
            let track = &mut scene.audio[item.index(tracks)];
            track.gain_db = -f64::from(v % 60);
            track.muted = v % 2 == 1;
            track.sync_offset_ms = i32::from(v) * 7;
            track.source = format!("{}.then", track.source);
            track.label = format!("old {v}");
        }
        6 if !take.streams.is_empty() => {
            let index = item.index(take.streams.len());
            let stream = &mut take.streams[index];
            stream.width = None;
            stream.height = None;
            stream.sample_rate = None;
        }
        7 if layers > 1 => scene.layers.rotate_left(1 + item.index(layers - 1)),
        _ => {}
    }
}

/// A project layer added after every take was recorded (no stream anywhere).
fn add_unrecorded_layer(project: &mut Project) {
    let id: LayerId = "L9".parse().unwrap();
    if project.scene.layer(&id).is_some() {
        return;
    }
    if let Some(mut layer) = project.scene.layers.first().cloned() {
        layer.id = id;
        layer.source = VideoSource::Screen {
            output: "LATE-1".into(),
            label: "Late".into(),
        };
        project.scene.layers.insert(0, layer);
    }
}

fn arb_apply() -> impl Strategy<Value = (Project, Index, u8, u8)> {
    (arb_scene_project(), any::<Index>(), 0u8..3, any::<u8>())
}

/// The command for a strategy draw: `from` is the project or a take; `to`
/// the project, all takes or a subset of them (a bit mask).
fn command(project: &Project, from: Index, to: u8, mask: u8) -> (SceneTarget, SceneDest) {
    let takes: Vec<&TakeId> = project.takes.keys().collect();
    let from = match from.index(takes.len() + 1) {
        0 => SceneTarget::Project,
        n => SceneTarget::Take(takes[n - 1].clone()),
    };
    let to = match to {
        0 => SceneDest::Project,
        1 => SceneDest::AllTakes,
        _ => SceneDest::Takes(
            takes
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, t)| (*t).clone())
                .collect(),
        ),
    };
    (from, to)
}

fn scenes(project: &Project) -> BTreeMap<String, &Scene> {
    let mut scenes: BTreeMap<String, &Scene> = project
        .takes
        .iter()
        .map(|(id, take)| (id.to_string(), &take.scene))
        .collect();
    scenes.insert("project".to_owned(), &project.scene);
    scenes
}

fn has_media(stream: &Stream) -> bool {
    match stream.kind {
        StreamKind::Video => stream.width.is_some() && stream.height.is_some(),
        StreamKind::Audio => stream.sample_rate.is_some(),
    }
}

/// The ids of `scene` that are also in `from`, in `scene`'s order.
fn shared_order<'a>(scene: &'a Scene, from: &Scene) -> Vec<&'a LayerId> {
    scene
        .layers
        .iter()
        .map(|l| &l.id)
        .filter(|id| from.layer(id).is_some())
        .collect()
}

fn layout(layer: &Layer) -> String {
    format!(
        "{:?} {:?} {} {} {:?}",
        layer.rect, layer.crop, layer.opacity, layer.visible, layer.fit
    )
}

proptest! {
    #[test]
    fn scene_apply_never_changes_sources_or_missing_flags(
        (project, from, to, mask) in arb_apply(),
    ) {
        let state = testkit::state(project.clone());
        let (from, to) = command(&project, from, to, mask);
        let out = match scene_apply(&state, &ctx(), from.clone(), to.clone()) {
            Ok(out) => out,
            Err(err) => {
                // Only applying a scene to itself (or to nothing) is refused.
                prop_assert_eq!(err.code, ErrorCode::Usage, "{:?}", err);
                let to_self = match (&from, &to) {
                    (SceneTarget::Project, SceneDest::Project) => true,
                    (_, SceneDest::Takes(ids)) => ids.iter().all(|t| SceneTarget::Take(t.clone()) == from),
                    _ => false,
                };
                prop_assert!(to_self, "{:?} → {:?}: {:?}", from, to, err);
                return Ok(());
            }
        };
        let next = &out.state.project;
        let source = match &from {
            SceneTarget::Project => &project.scene,
            SceneTarget::Take(t) => &project.takes[t].scene,
        };
        let dests: BTreeSet<String> = match &to {
            SceneDest::Project => ["project".to_owned()].into(),
            SceneDest::AllTakes => project.takes.keys().map(ToString::to_string).collect(),
            SceneDest::Takes(ids) => ids.iter().map(ToString::to_string).collect(),
        };
        let from_name = from.to_string();

        // Everything but the scenes is untouched.
        prop_assert_eq!(&next.timeline, &project.timeline);
        prop_assert_eq!(&next.canvas, &project.canvas);
        prop_assert_eq!(&next.autocut, &project.autocut);
        for (id, take) in &project.takes {
            let mut scene_less = next.takes[id].clone();
            scene_less.scene = take.scene.clone();
            prop_assert_eq!(&scene_less, take, "only the scene of {} may change", id);
        }

        let before = scenes(&project);
        let after = scenes(next);
        for (name, old) in &before {
            let new = after[name];
            let is_dest = dests.contains(name) && *name != from_name;
            if !is_dest {
                prop_assert_eq!(new, *old, "{} isn't a destination", name);
                continue;
            }
            // Nothing is removed; sources, missing flags, names, labels and locks stay.
            for layer in &old.layers {
                let now = new.layer(&layer.id);
                prop_assert!(now.is_some(), "{} lost layer {}", name, &layer.id);
                let now = now.unwrap();
                prop_assert_eq!(&now.source, &layer.source);
                prop_assert_eq!(now.missing, layer.missing);
                prop_assert_eq!((&now.name, now.locked), (&layer.name, layer.locked));
            }
            for track in &old.audio {
                let now = new.track(&track.id);
                prop_assert!(now.is_some(), "{} lost track {}", name, &track.id);
                let now = now.unwrap();
                prop_assert_eq!(&now.source, &track.source);
                prop_assert_eq!(now.missing, track.missing);
                prop_assert_eq!((&now.label, now.role), (&track.label, track.role));
            }
            // Matched entries carry the source's layout, layers in its z-order.
            for layer in &new.layers {
                if let Some(src) = source.layer(&layer.id) {
                    prop_assert_eq!(layout(layer), layout(src));
                }
            }
            let order = shared_order(new, source);
            let expected: Vec<&LayerId> = source.layers.iter().map(|l| &l.id).filter(|id| new.layer(id).is_some()).collect();
            prop_assert_eq!(order, expected, "z-order of {}", name);
            for track in &new.audio {
                if let Some(src) = source.track(&track.id) {
                    prop_assert_eq!(
                        (track.gain_db, track.muted, track.sync_offset_ms),
                        (src.gain_db, src.muted, src.sync_offset_ms)
                    );
                }
            }
            // Added entries: only in takes, only with a stream, never with a
            // borrowed `missing` flag.
            let added_layers = new.layers.iter().filter(|l| old.layer(&l.id).is_none());
            let added_tracks = new.audio.iter().filter(|t| old.track(&t.id).is_none());
            let take = next.takes.get(&name.parse::<TakeId>().unwrap_or_else(|_| TakeId::numbered(0)));
            for layer in added_layers {
                prop_assert!(take.is_some(), "{} gained layer {}", name, &layer.id);
                let stream = take.unwrap().stream(layer.id.as_str());
                prop_assert!(stream.is_some_and(|s| s.kind == StreamKind::Video), "{} has no stream", &layer.id);
                prop_assert_eq!(layer.missing, !has_media(stream.unwrap()));
                prop_assert_eq!(&layer.source, &source.layer(&layer.id).unwrap().source);
            }
            for track in added_tracks {
                prop_assert!(take.is_some(), "{} gained track {}", name, &track.id);
                let stream = take.unwrap().stream(track.id.as_str());
                prop_assert!(stream.is_some_and(|s| s.kind == StreamKind::Audio), "{} has no stream", &track.id);
                prop_assert_eq!(track.missing, !has_media(stream.unwrap()));
            }
        }
    }

    #[test]
    fn removing_from_a_take_then_resetting_it_restores_the_snapshot(
        project in arb_project(),
        take_index in any::<Index>(),
        item in any::<Index>(),
        track_too in any::<bool>(),
    ) {
        // A take scene of an arbitrary project is a pure snapshot of the
        // project scene: removing an entry and applying the project layout
        // brings back exactly what was there.
        let state = testkit::state(project.clone());
        let ids: Vec<&TakeId> = project.takes.keys().collect();
        let take = ids[take_index.index(ids.len())].clone();
        let target = SceneTarget::Take(take.clone());
        let mut state = state;
        let scene = &project.takes[&take].scene;
        if !scene.layers.is_empty() {
            let layer = scene.layers[item.index(scene.layers.len())].id.clone();
            state = layer_remove(&state, &ctx(), target.clone(), layer).unwrap().state;
        }
        if track_too && !scene.audio.is_empty() {
            let track = scene.audio[item.index(scene.audio.len())].id.clone();
            state = audio_remove(&state, &ctx(), target.clone(), track).unwrap().state;
        }
        let out = scene_apply(&state, &ctx(), SceneTarget::Project, SceneDest::Takes(vec![take.clone()])).unwrap();
        prop_assert_eq!(&out.state.project.takes[&take].scene, &project.scene);
        prop_assert_eq!(&out.state.project.takes[&take], &project.takes[&take]);
    }
}
