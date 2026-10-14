//! Keeps `UiState` consistent with the project after every change (A§5.10).
//! Owner: timeline.
//!
//! [`ui_normalize`] runs in every `commit`/`persist`:
//! - the selection keeps only clips on the timeline, in timeline order, each
//!   once;
//! - a run of removed clips that held a selected clip passes the selection
//!   to the new clips that took its place (record over; discard putting the
//!   replaced clips back);
//! - a selected clip that shrank and is followed by new clips carved out of
//!   its old range passes the selection to them (autocut fragments, the
//!   right part of a split): the selection keeps covering the same material;
//! - on an fps change the playhead and the zoom are converted through
//!   nanoseconds (same instant, same pixels per second);
//! - the playhead stays within `[0, total_frames]`;
//! - `selected_layer` and `solo` are cleared when they don't exist in the
//!   scene the canvas targets ([`canvas_target`]);
//! - values a hand-edited `ui.json` can break (zoom, scroll, panel sizes)
//!   fall back to their defaults.

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{ClipId, Frame};
use crate::model::{Clip, Fps, Project, Scene};
use crate::protocol::command::SceneTarget;
use crate::state::{CanvasMode, UiState};
use crate::time::{NS_PER_SEC, frame_ns, ns_to_frame_round};
use crate::timeline;

/// Runs in every `commit`/`persist`: `ui` (the UI the step wants) made
/// consistent with `next`, knowing the project was `prev` before the step.
pub fn ui_normalize(prev: &Project, next: &Project, ui: &UiState) -> UiState {
    let mut out = ui.clone();
    out.selection = selection(prev, next, &ui.selection);

    let (old_fps, new_fps) = (prev.canvas.fps, next.canvas.fps);
    if old_fps != new_fps {
        let playhead = ui
            .playhead
            .min(timeline::total_frames(&prev.timeline.clips));
        out.playhead = convert_frame(playhead, old_fps, new_fps);
        out.zoom = ui.zoom.map(|z| convert_zoom(z, old_fps, new_fps));
    }
    out.playhead = out
        .playhead
        .min(timeline::total_frames(&next.timeline.clips));

    if out.zoom.is_some_and(|z| !(z.is_finite() && z > 0.0)) {
        out.zoom = None;
    }
    if !out.scroll_px.is_finite() || out.scroll_px < 0.0 {
        out.scroll_px = 0.0;
    }
    out.panels.retain(|_, size| size.is_finite());

    let scene = canvas_scene(next, &out);
    if out
        .selected_layer
        .as_ref()
        .is_some_and(|l| scene.layer(l).is_none())
    {
        out.selected_layer = None;
    }
    if out.solo.as_ref().is_some_and(|t| scene.track(t).is_none()) {
        out.solo = None;
    }
    out
}

/// The scene the canvas shows and edits (SPEC §12): the project scene in
/// Live mode. In Program mode, the take of the first selected clip; with
/// nothing selected, the take under the playhead (the last clip's take when
/// the playhead is at the end); on an empty timeline, the project scene.
pub fn canvas_target(project: &Project, ui: &UiState) -> SceneTarget {
    if ui.canvas_mode == CanvasMode::Live {
        return SceneTarget::Project;
    }
    let clips = &project.timeline.clips;
    let clip = ui
        .selection
        .iter()
        .filter_map(|id| timeline::index_of(clips, id))
        .min()
        .or_else(|| timeline::clip_at(clips, ui.playhead).map(|(i, _)| i))
        .or_else(|| clips.len().checked_sub(1))
        .map(|i| &clips[i]);
    match clip {
        Some(c) if project.takes.contains_key(&c.take) => SceneTarget::Take(c.take.clone()),
        _ => SceneTarget::Project,
    }
}

/// The scene of [`canvas_target`].
pub fn canvas_scene<'a>(project: &'a Project, ui: &UiState) -> &'a Scene {
    match canvas_target(project, ui) {
        SceneTarget::Project => &project.scene,
        SceneTarget::Take(take) => project.take(&take).map_or(&project.scene, |t| &t.scene),
    }
}

/// The selection rules of A§5.10 (see the module docs), in timeline order.
fn selection(prev: &Project, next: &Project, selection: &[ClipId]) -> Vec<ClipId> {
    let selected: BTreeSet<&ClipId> = selection.iter().collect();
    if selected.is_empty() {
        return Vec::new();
    }
    let before = &prev.timeline.clips;
    let after = &next.timeline.clips;
    let mut keep: BTreeSet<&ClipId> = BTreeSet::new();
    let mut in_after: BTreeMap<&ClipId, usize> = BTreeMap::new();
    for (i, clip) in after.iter().enumerate() {
        in_after.entry(&clip.id).or_insert(i);
        if selected.contains(&clip.id) {
            keep.insert(&clip.id);
        }
    }

    if before != after {
        let mut in_before: BTreeMap<&ClipId, usize> = BTreeMap::new();
        for (i, clip) in before.iter().enumerate() {
            in_before.entry(&clip.id).or_insert(i);
        }
        let is_new = |c: &Clip| !in_before.contains_key(&c.id);

        // Runs of removed clips holding a selected clip: select the new
        // clips between the run's surviving neighbours.
        let mut i = 0;
        while i < before.len() {
            if in_after.contains_key(&before[i].id) {
                i += 1;
                continue;
            }
            let start = i;
            while i < before.len() && !in_after.contains_key(&before[i].id) {
                i += 1;
            }
            if !before[start..i].iter().any(|c| selected.contains(&c.id)) {
                continue;
            }
            let lo = start
                .checked_sub(1)
                .map_or(0, |k| in_after[&before[k].id] + 1);
            let hi = before.get(i).map_or(after.len(), |c| in_after[&c.id]);
            if lo <= hi {
                keep.extend(after[lo..hi].iter().filter(|c| is_new(c)).map(|c| &c.id));
            }
        }

        // Selected clips that shrank: select the new clips right after them
        // that lie inside their old range of the same take.
        for (j, clip) in after.iter().enumerate() {
            if !selected.contains(&clip.id) {
                continue;
            }
            let Some(old) = in_before.get(&clip.id).map(|&k| &before[k]) else {
                continue;
            };
            let inside = |c: &Clip| c.take == old.take && old.in_ <= c.in_ && c.out <= old.out;
            if (clip.in_, clip.out) == (old.in_, old.out) || !inside(clip) {
                continue;
            }
            for fragment in &after[j + 1..] {
                if !is_new(fragment) || !inside(fragment) {
                    break;
                }
                keep.insert(&fragment.id);
            }
        }
    }

    let mut seen = BTreeSet::new();
    after
        .iter()
        .filter(|c| keep.contains(&c.id) && seen.insert(&c.id))
        .map(|c| c.id.clone())
        .collect()
}

/// Frame `f` at `old` as the nearest frame at `new` (through nanoseconds,
/// like clip boundaries are re-quantized). Values too large for
/// nanoseconds are left alone (the caller clamps them).
fn convert_frame(f: Frame, old: Fps, new: Fps) -> Frame {
    let ns =
        i128::from(f) * i128::from(NS_PER_SEC) * i128::from(old.den) / i128::from(old.num.max(1));
    match (i64::try_from(ns), i64::try_from(f)) {
        (Ok(_), Ok(f)) => ns_to_frame_round(frame_ns(f, old), new).max(0) as Frame,
        _ => f,
    }
}

/// Pixels per frame at `old` → pixels per frame at `new`, keeping pixels per second.
fn convert_zoom(zoom: f64, old: Fps, new: Fps) -> f64 {
    zoom * (f64::from(old.num) * f64::from(new.den)) / (f64::from(old.den) * f64::from(new.num))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{LayerId, TakeId, TrackId};
    use crate::testkit::ProjectBuilder;

    fn cid(id: &str) -> ClipId {
        ClipId::from_raw(id)
    }

    fn ids(v: &[&str]) -> Vec<ClipId> {
        v.iter().map(|s| cid(s)).collect()
    }

    fn sel(ui: &UiState) -> Vec<&str> {
        ui.selection.iter().map(ClipId::as_str).collect()
    }

    fn clip(id: &str, take: u32, in_: Frame, out: Frame) -> Clip {
        Clip {
            id: cid(id),
            take: TakeId::numbered(take),
            in_,
            out,
        }
    }

    /// Layers L1 (screen) and L2 (camera), tracks A1 (mic) and A2 (system);
    /// take 1 (300 frames) and take 2 (200 frames, recorded without L2 and A2).
    fn project(clips: &[Clip]) -> Project {
        let mut p = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .camera_layer("L2", "/dev/video0")
            .mic("A1", "alsa_input.mic")
            .system("A2", "alsa_output.monitor")
            .take("take-0001", 300)
            .take("take-0002", 200)
            .with_take("take-0002", |t| {
                t.scene.layers.retain(|l| l.id.as_str() == "L1");
                t.scene.audio.retain(|a| a.id.as_str() == "A1");
            })
            .build();
        p.timeline.clips = clips.to_vec();
        p
    }

    fn ui_with(selection: &[&str]) -> UiState {
        UiState {
            selection: ids(selection),
            ..UiState::default()
        }
    }

    #[test]
    fn selection_is_filtered_sorted_and_deduplicated() {
        let p = project(&[
            clip("c1", 1, 0, 10),
            clip("c2", 1, 10, 20),
            clip("c3", 2, 0, 10),
        ]);
        let out = ui_normalize(&p, &p, &ui_with(&["c3", "c9", "c1", "c3"]));
        assert_eq!(sel(&out), ["c1", "c3"]);
        let out = ui_normalize(&p, &p, &ui_with(&[]));
        assert!(out.selection.is_empty());
    }

    #[test]
    fn record_over_selects_the_new_clip() {
        let prev = project(&[
            clip("c1", 1, 0, 10),
            clip("c2", 1, 10, 20),
            clip("c3", 1, 20, 30),
            clip("c4", 1, 30, 40),
        ]);
        let next = project(&[
            clip("c1", 1, 0, 10),
            clip("cx", 2, 0, 200),
            clip("c4", 1, 30, 40),
        ]);
        let out = ui_normalize(&prev, &next, &ui_with(&["c2", "c3"]));
        assert_eq!(sel(&out), ["cx"]);
        // At the start and at the end of the timeline too.
        let next = project(&[
            clip("cx", 2, 0, 200),
            clip("c3", 1, 20, 30),
            clip("c4", 1, 30, 40),
        ]);
        assert_eq!(sel(&ui_normalize(&prev, &next, &ui_with(&["c1"]))), ["cx"]);
        let next = project(&[
            clip("c1", 1, 0, 10),
            clip("c2", 1, 10, 20),
            clip("c3", 1, 20, 30),
            clip("cx", 2, 0, 200),
        ]);
        assert_eq!(
            sel(&ui_normalize(&prev, &next, &ui_with(&["c4", "c1"]))),
            ["c1", "cx"]
        );
        // A record over clips that weren't selected leaves the selection alone.
        let next = project(&[
            clip("c1", 1, 0, 10),
            clip("cx", 2, 0, 200),
            clip("c4", 1, 30, 40),
        ]);
        assert_eq!(sel(&ui_normalize(&prev, &next, &ui_with(&["c4"]))), ["c4"]);
        // Removing selected clips selects nothing new.
        let next = project(&[clip("c1", 1, 0, 10), clip("c4", 1, 30, 40)]);
        assert!(
            ui_normalize(&prev, &next, &ui_with(&["c2"]))
                .selection
                .is_empty()
        );
    }

    #[test]
    fn discard_after_record_over_selects_the_restored_clips() {
        let prev = project(&[
            clip("c1", 1, 0, 10),
            clip("cx", 2, 0, 200),
            clip("c4", 1, 30, 40),
        ]);
        let next = project(&[
            clip("c1", 1, 0, 10),
            clip("c2", 1, 10, 20),
            clip("c3", 1, 20, 30),
            clip("c4", 1, 30, 40),
        ]);
        assert_eq!(
            sel(&ui_normalize(&prev, &next, &ui_with(&["cx"]))),
            ["c2", "c3"]
        );
    }

    #[test]
    fn fragments_of_a_selected_clip_are_selected() {
        // Autocut: c2 [0, 300) becomes c2 [10, 80), ca [120, 200), cb [240, 290).
        let prev = project(&[
            clip("c1", 2, 0, 50),
            clip("c2", 1, 0, 300),
            clip("c3", 2, 50, 60),
        ]);
        let next = project(&[
            clip("c1", 2, 0, 50),
            clip("c2", 1, 10, 80),
            clip("ca", 1, 120, 200),
            clip("cb", 1, 240, 290),
            clip("c3", 2, 50, 60),
        ]);
        assert_eq!(
            sel(&ui_normalize(&prev, &next, &ui_with(&["c2"]))),
            ["c2", "ca", "cb"]
        );
        assert_eq!(sel(&ui_normalize(&prev, &next, &ui_with(&["c1"]))), ["c1"]);
        // A split is the same shape: both halves stay selected.
        let next = project(&[
            clip("c1", 2, 0, 50),
            clip("c2", 1, 0, 140),
            clip("cs", 1, 140, 300),
            clip("c3", 2, 50, 60),
        ]);
        assert_eq!(
            sel(&ui_normalize(&prev, &next, &ui_with(&["c3", "c2"]))),
            ["c2", "cs", "c3"]
        );
        // A clip inserted after an unchanged selected clip isn't a fragment.
        let next = project(&[
            clip("c1", 2, 0, 50),
            clip("c2", 1, 0, 300),
            clip("cn", 1, 0, 300),
            clip("c3", 2, 50, 60),
        ]);
        assert_eq!(sel(&ui_normalize(&prev, &next, &ui_with(&["c2"]))), ["c2"]);
        // Nor is a new clip of another take, or one outside the old range.
        let next = project(&[
            clip("c1", 2, 0, 50),
            clip("c2", 1, 0, 100),
            clip("cn", 2, 0, 10),
            clip("c3", 2, 50, 60),
        ]);
        assert_eq!(sel(&ui_normalize(&prev, &next, &ui_with(&["c2"]))), ["c2"]);
    }

    #[test]
    fn playhead_is_clamped_and_converted_on_fps_changes() {
        let p = project(&[clip("c1", 1, 0, 300)]);
        let ui = UiState {
            playhead: 999,
            zoom: Some(2.0),
            ..UiState::default()
        };
        let out = ui_normalize(&p, &p, &ui);
        assert_eq!(out.playhead, 300);
        assert_eq!(out.zoom, Some(2.0));

        // 30 → 60 fps: frame 150 (5 s) is frame 300; px per second stays 60.
        let mut next = p.clone();
        next.canvas.fps = Fps::new(60, 1);
        next.timeline.clips[0].out = 600;
        let ui = UiState {
            playhead: 150,
            zoom: Some(2.0),
            ..UiState::default()
        };
        let out = ui_normalize(&p, &next, &ui);
        assert_eq!(out.playhead, 300);
        assert_eq!(out.zoom, Some(1.0));
        // 30 → 29.97: frame 10 starts at 333.33 ms, nearest to frame 10 (333.67 ms).
        next.canvas.fps = Fps::new(30000, 1001);
        let ui = UiState {
            playhead: 10,
            ..UiState::default()
        };
        assert_eq!(ui_normalize(&p, &next, &ui).playhead, 10);
        // A shorter timeline at the new rate clamps.
        next.timeline.clips[0].out = 100;
        let ui = UiState {
            playhead: 290,
            ..UiState::default()
        };
        assert_eq!(ui_normalize(&p, &next, &ui).playhead, 100);
        assert_eq!(
            convert_frame(u64::MAX, Fps::new(30, 1), Fps::new(60, 1)),
            u64::MAX
        );
    }

    #[test]
    fn broken_view_values_fall_back() {
        let p = project(&[]);
        let mut ui = UiState {
            zoom: Some(f64::NAN),
            scroll_px: -4.0,
            ..UiState::default()
        };
        ui.panels.insert("bin".into(), f64::INFINITY);
        ui.panels.insert("layers".into(), 352.0);
        let out = ui_normalize(&p, &p, &ui);
        assert_eq!(out.zoom, None);
        assert_eq!(out.scroll_px, 0.0);
        assert_eq!(out.panels.keys().collect::<Vec<_>>(), ["layers"]);
        let ui = UiState {
            zoom: Some(0.0),
            scroll_px: f64::NAN,
            ..UiState::default()
        };
        let out = ui_normalize(&p, &p, &ui);
        assert_eq!((out.zoom, out.scroll_px), (None, 0.0));
    }

    #[test]
    fn layer_and_solo_follow_the_canvas_scene() {
        let p = project(&[clip("c1", 1, 0, 100), clip("c2", 2, 0, 100)]);
        let mut ui = UiState {
            selected_layer: Some(LayerId::numbered(2)),
            solo: Some(TrackId::numbered(2)),
            ..UiState::default()
        };
        // Live: the project scene has L2 and A2.
        assert_eq!(ui_normalize(&p, &p, &ui), ui);
        assert_eq!(canvas_target(&p, &ui), SceneTarget::Project);
        // Program, c1 selected: take 1 has them too.
        ui.canvas_mode = CanvasMode::Program;
        ui.selection = ids(&["c1"]);
        assert_eq!(
            canvas_target(&p, &ui),
            SceneTarget::Take(TakeId::numbered(1))
        );
        assert_eq!(ui_normalize(&p, &p, &ui), ui);
        // Program, c2 selected: take 2 was recorded without L2 and A2.
        ui.selection = ids(&["c2"]);
        let out = ui_normalize(&p, &p, &ui);
        assert_eq!((out.selected_layer, out.solo), (None, None));
        // Program without a selection: the take under the playhead, the
        // last clip's take at the end, the project scene when empty.
        ui.selection.clear();
        ui.playhead = 50;
        assert_eq!(
            canvas_target(&p, &ui),
            SceneTarget::Take(TakeId::numbered(1))
        );
        ui.playhead = 200;
        assert_eq!(
            canvas_target(&p, &ui),
            SceneTarget::Take(TakeId::numbered(2))
        );
        assert_eq!(canvas_target(&project(&[]), &ui), SceneTarget::Project);
        // Layers removed from the project scene are cleared in Live mode.
        let mut q = p.clone();
        q.scene.layers.retain(|l| l.id.as_str() == "L1");
        ui.canvas_mode = CanvasMode::Live;
        let out = ui_normalize(&p, &q, &ui);
        assert_eq!(out.selected_layer, None);
        assert_eq!(out.solo, Some(TrackId::numbered(2)));
    }

    #[test]
    fn normalizing_twice_changes_nothing() {
        let prev = project(&[clip("c1", 1, 0, 10), clip("c2", 1, 10, 20)]);
        let next = project(&[clip("cx", 2, 0, 200), clip("c2", 1, 10, 20)]);
        let mut ui = ui_with(&["c2", "c1", "c7"]);
        ui.canvas_mode = CanvasMode::Program;
        ui.selected_layer = Some(LayerId::numbered(2));
        ui.playhead = 5000;
        let once = ui_normalize(&prev, &next, &ui);
        assert_eq!(sel(&once), ["cx", "c2"]);
        assert_eq!(once.selected_layer, None, "take 2 has no L2");
        assert_eq!(ui_normalize(&next, &next, &once), once);
    }
}
