//! History properties (A§7.2, A§7.3, A§7.5, A§11):
//!
//! - undo∘apply = id and redo∘undo = id over random edit sequences, with
//!   every `test` op passing (and guarding every `replace`/`remove`);
//! - patch size grows with the number of changed clips, not with the
//!   timeline length;
//! - after a purge no reachable state references the take;
//! - compaction preserves the current state.
//!
//! Edits are applied through pure helpers below and finished with
//! `history::commit`, like the command handlers do; a "record" edit first
//! inserts a take as a fact (no entry), then places its clip (an entry).

use std::collections::{BTreeMap, BTreeSet};

use chrono::Duration;
use json_patch::{Patch, PatchOperation};
use proptest::prelude::*;
use proptest::sample::Index;
use qrec_core::ctx::{Facts, next_layer_id, next_take_number};
use qrec_core::history::diff::{aligned_views, diff};
use qrec_core::history::ops::{self, cursor_position, position_of};
use qrec_core::history::purge::{purge_rewrite, remove_takes};
use qrec_core::history::{commit, external};
use qrec_core::protocol::HistoryOp;
use qrec_core::requantize::rederive_durations;
use qrec_core::state::State;
use qrec_core::testkit::{self, ProjectBuilder, strategies};
use qrec_core::time::frames_to_ns;
use qrec_core::*;
use serde_json::{Value, json};

// ------------------------------------------------------------------ edits

#[derive(Debug, Clone)]
enum Edit {
    Split {
        clip: Index,
        at: Index,
    },
    Trim {
        clip: Index,
        a: Index,
        b: Index,
    },
    Move {
        clip: Index,
        to: Index,
    },
    Remove {
        clip: Index,
    },
    Add {
        take: Index,
        a: Index,
        b: Index,
        at: Index,
    },
    LayerRect {
        scene: Index,
        layer: Index,
        rect: (i32, i32, u32, u32),
    },
    LayerOpacity {
        scene: Index,
        layer: Index,
        percent: u8,
    },
    LayerToggle {
        scene: Index,
        layer: Index,
    },
    LayerOrder {
        scene: Index,
        layer: Index,
        to: Index,
    },
    LayerRemove {
        scene: Index,
        layer: Index,
    },
    LayerAdd,
    AudioGain {
        scene: Index,
        track: Index,
        gain: i8,
    },
    AudioMute {
        scene: Index,
        track: Index,
    },
    AudioRemove {
        scene: Index,
        track: Index,
    },
    Rename(u8),
    Background(u8),
    Fps(Fps),
    AutocutThreshold(u8),
    Countdown(u8),
    Record {
        frames: u64,
    },
    Nothing,
}

fn arb_edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        3 => (any::<Index>(), any::<Index>()).prop_map(|(clip, at)| Edit::Split { clip, at }),
        3 => (any::<Index>(), any::<Index>(), any::<Index>()).prop_map(|(clip, a, b)| Edit::Trim { clip, a, b }),
        2 => (any::<Index>(), any::<Index>()).prop_map(|(clip, to)| Edit::Move { clip, to }),
        2 => any::<Index>().prop_map(|clip| Edit::Remove { clip }),
        2 => (any::<Index>(), any::<Index>(), any::<Index>(), any::<Index>())
            .prop_map(|(take, a, b, at)| Edit::Add { take, a, b, at }),
        1 => (any::<Index>(), any::<Index>(), (-50i32..50, -50i32..50, 1u32..4000, 1u32..4000))
            .prop_map(|(scene, layer, rect)| Edit::LayerRect { scene, layer, rect }),
        1 => (any::<Index>(), any::<Index>(), 0u8..=100)
            .prop_map(|(scene, layer, percent)| Edit::LayerOpacity { scene, layer, percent }),
        1 => (any::<Index>(), any::<Index>()).prop_map(|(scene, layer)| Edit::LayerToggle { scene, layer }),
        1 => (any::<Index>(), any::<Index>(), any::<Index>())
            .prop_map(|(scene, layer, to)| Edit::LayerOrder { scene, layer, to }),
        1 => (any::<Index>(), any::<Index>()).prop_map(|(scene, layer)| Edit::LayerRemove { scene, layer }),
        1 => Just(Edit::LayerAdd),
        1 => (any::<Index>(), any::<Index>(), -60i8..=60)
            .prop_map(|(scene, track, gain)| Edit::AudioGain { scene, track, gain }),
        1 => (any::<Index>(), any::<Index>()).prop_map(|(scene, track)| Edit::AudioMute { scene, track }),
        1 => (any::<Index>(), any::<Index>()).prop_map(|(scene, track)| Edit::AudioRemove { scene, track }),
        1 => any::<u8>().prop_map(Edit::Rename),
        1 => any::<u8>().prop_map(Edit::Background),
        1 => strategies::arb_fps().prop_map(Edit::Fps),
        1 => (0u8..80).prop_map(Edit::AutocutThreshold),
        1 => (0u8..=10).prop_map(Edit::Countdown),
        2 => (10u64..400).prop_map(|frames| Edit::Record { frames }),
        1 => Just(Edit::Nothing),
    ]
}

/// Ids for new clips; never reused within a run.
struct Ids(u32);

impl Ids {
    fn clip(&mut self) -> ClipId {
        self.0 += 1;
        format!("n{}", self.0).parse().unwrap()
    }
}

fn scene_mut(p: &mut Project, scene: Index) -> &mut Scene {
    match scene.index(1 + p.takes.len()) {
        0 => &mut p.scene,
        i => &mut p.takes.values_mut().nth(i - 1).unwrap().scene,
    }
}

fn ok_takes(p: &Project) -> Vec<TakeId> {
    p.takes
        .iter()
        .filter(|(_, t)| t.status == TakeStatus::Ok && t.duration_frames > 0)
        .map(|(id, _)| id.clone())
        .collect()
}

/// A take recorded with the current project scene (a fact: no entry).
fn record_take(p: &mut Project, frames: u64) -> Option<TakeId> {
    if p.scene.is_empty() {
        return None;
    }
    let n = next_take_number(p, &Facts::default());
    let id = TakeId::numbered(n);
    let fps = p.canvas.fps;
    let duration_ns = Some(frames_to_ns(frames, fps));
    let scene = p.scene.clone();
    let video = scene.layers.iter().map(|l| Stream {
        id: l.id.to_string(),
        kind: StreamKind::Video,
        file: format!("takes/{id}/{}-video.mkv", l.id),
        width: Some(p.canvas.width),
        height: Some(p.canvas.height),
        sample_rate: None,
        channels: None,
        start_offset_ns: 0,
        duration_ns,
        frames: Some(frames),
        fps: Some(fps),
    });
    let audio = scene.audio.iter().map(|t| Stream {
        id: t.id.to_string(),
        kind: StreamKind::Audio,
        file: format!("takes/{id}/{}-audio.mka", t.id),
        width: None,
        height: None,
        sample_rate: Some(48_000),
        channels: Some(1),
        start_offset_ns: 0,
        duration_ns,
        frames: None,
        fps: None,
    });
    let streams = video.chain(audio).collect();
    let take = Take {
        status: TakeStatus::Ok,
        recorded_at: p.created_at + Duration::minutes(i64::from(n)),
        duration_frames: frames,
        scene,
        streams,
        error: None,
        placed: true,
        replaced: Vec::new(),
    };
    p.takes.insert(id.clone(), take);
    p.next_take_number = n + 1;
    Some(id)
}

/// Changes `p` like the command would (no-ops leave it unchanged).
fn apply_edit(p: &mut Project, edit: &Edit, ids: &mut Ids, state: &State) {
    let clips = &mut p.timeline.clips;
    match edit {
        Edit::Split { clip, at } => {
            if clips.is_empty() {
                return;
            }
            let i = clip.index(clips.len());
            let c = clips[i].clone();
            if c.len() < 2 {
                return;
            }
            let point = c.in_ + 1 + at.index((c.len() - 1) as usize) as u64;
            clips[i].out = point;
            let right = Clip {
                id: ids.clip(),
                take: c.take,
                in_: point,
                out: c.out,
            };
            clips.insert(i + 1, right);
        }
        Edit::Trim { clip, a, b } => {
            if clips.is_empty() {
                return;
            }
            let i = clip.index(clips.len());
            let duration = p.takes[&clips[i].take].duration_frames;
            let new_in = a.index(duration as usize) as u64;
            let new_out = new_in + 1 + b.index((duration - new_in) as usize) as u64;
            clips[i].in_ = new_in;
            clips[i].out = new_out;
        }
        Edit::Move { clip, to } => {
            if clips.is_empty() {
                return;
            }
            let c = clips.remove(clip.index(clips.len()));
            let at = to.index(clips.len() + 1);
            clips.insert(at, c);
        }
        Edit::Remove { clip } => {
            if !clips.is_empty() {
                clips.remove(clip.index(clips.len()));
            }
        }
        Edit::Add { take, a, b, at } => {
            let takes = ok_takes(p);
            if takes.is_empty() {
                return;
            }
            let take = takes[take.index(takes.len())].clone();
            let duration = p.takes[&take].duration_frames;
            let in_ = a.index(duration as usize) as u64;
            let out = in_ + 1 + b.index((duration - in_) as usize) as u64;
            let clips = &mut p.timeline.clips;
            let at = at.index(clips.len() + 1);
            clips.insert(
                at,
                Clip {
                    id: ids.clip(),
                    take,
                    in_,
                    out,
                },
            );
        }
        Edit::LayerRect { scene, layer, rect } => {
            let scene = scene_mut(p, *scene);
            if !scene.layers.is_empty() {
                let i = layer.index(scene.layers.len());
                scene.layers[i].rect = Rect::new(rect.0, rect.1, rect.2, rect.3);
            }
        }
        Edit::LayerOpacity {
            scene,
            layer,
            percent,
        } => {
            let scene = scene_mut(p, *scene);
            if !scene.layers.is_empty() {
                let i = layer.index(scene.layers.len());
                scene.layers[i].opacity = f64::from(*percent) / 100.0;
            }
        }
        Edit::LayerToggle { scene, layer } => {
            let scene = scene_mut(p, *scene);
            if !scene.layers.is_empty() {
                let i = layer.index(scene.layers.len());
                scene.layers[i].visible = !scene.layers[i].visible;
            }
        }
        Edit::LayerOrder { scene, layer, to } => {
            let scene = scene_mut(p, *scene);
            if !scene.layers.is_empty() {
                let l = scene.layers.remove(layer.index(scene.layers.len()));
                let at = to.index(scene.layers.len() + 1);
                scene.layers.insert(at, l);
            }
        }
        Edit::LayerRemove { scene, layer } => {
            let scene = scene_mut(p, *scene);
            if !scene.layers.is_empty() {
                scene.layers.remove(layer.index(scene.layers.len()));
            }
        }
        Edit::LayerAdd => {
            let id = next_layer_id(state);
            let n = id.number().unwrap_or(1);
            p.scene.layers.push(Layer {
                id,
                name: format!("Screen DP-{n}"),
                visible: true,
                opacity: 1.0,
                source: VideoSource::Screen {
                    output: format!("DP-{n}"),
                    label: "Monitor".into(),
                },
                rect: Rect::new(0, 0, p.canvas.width, p.canvas.height),
                crop: Crop::default(),
                fit: Fit::Contain,
                locked: false,
                missing: false,
            });
        }
        Edit::AudioGain { scene, track, gain } => {
            let scene = scene_mut(p, *scene);
            if !scene.audio.is_empty() {
                let i = track.index(scene.audio.len());
                scene.audio[i].gain_db = f64::from(*gain);
            }
        }
        Edit::AudioMute { scene, track } => {
            let scene = scene_mut(p, *scene);
            if !scene.audio.is_empty() {
                let i = track.index(scene.audio.len());
                scene.audio[i].muted = !scene.audio[i].muted;
            }
        }
        Edit::AudioRemove { scene, track } => {
            let is_project = scene.index(1 + p.takes.len()) == 0;
            let target = scene_mut(p, *scene);
            if !target.audio.is_empty() {
                let removed = target.audio.remove(track.index(target.audio.len()));
                if is_project && p.autocut.track.as_ref() == Some(&removed.id) {
                    p.autocut.track = None;
                }
            }
        }
        Edit::Rename(n) => p.name = format!("name {n}"),
        Edit::Background(n) => p.canvas.background = format!("#{n:02X}{n:02X}00").parse().unwrap(),
        Edit::Fps(fps) => change_fps(p, *fps),
        Edit::AutocutThreshold(t) => p.autocut.threshold_db = -f64::from(*t),
        Edit::Countdown(n) => p.recording.countdown_secs = u32::from(*n),
        Edit::Record { .. } | Edit::Nothing => {}
    }
}

/// A canvas fps change: durations re-derived, clips scaled and clamped,
/// empty ones removed (a stand-in for `requantize`).
fn change_fps(p: &mut Project, fps: Fps) {
    let old = p.canvas.fps;
    if old == fps {
        return;
    }
    p.canvas.fps = fps;
    rederive_durations(p);
    let scale = |f: u64, up: bool| {
        let num = u128::from(f) * u128::from(fps.num) * u128::from(old.den);
        let den = u128::from(fps.den) * u128::from(old.num);
        (if up { num.div_ceil(den) } else { num / den }) as u64
    };
    let takes = p.takes.clone();
    p.timeline.clips.retain_mut(|c| {
        let duration = takes[&c.take].duration_frames;
        c.out = scale(c.out, true).min(duration);
        c.in_ = scale(c.in_, false).min(c.out);
        c.in_ < c.out
    });
}

/// The structural invariants undo must keep (A§2.3, the parts history can
/// break): unique clip ids, clips inside `ok` takes, unique scene ids.
fn structural(p: &Project) -> Result<(), CoreError> {
    let mut clip_ids = BTreeSet::new();
    for c in &p.timeline.clips {
        if !clip_ids.insert(&c.id) {
            return Err(CoreError::invalid(format!("duplicate clip {}", c.id)));
        }
        let take = p
            .takes
            .get(&c.take)
            .ok_or_else(|| CoreError::invalid(format!("clip {} uses a missing take", c.id)))?;
        if take.status != TakeStatus::Ok || c.in_ >= c.out || c.out > take.duration_frames {
            return Err(CoreError::invalid(format!(
                "clip {} is out of bounds",
                c.id
            )));
        }
    }
    for scene in std::iter::once(&p.scene).chain(p.takes.values().map(|t| &t.scene)) {
        let layers: BTreeSet<_> = scene.layers.iter().map(|l| &l.id).collect();
        let tracks: BTreeSet<_> = scene.audio.iter().map(|t| &t.id).collect();
        if layers.len() != scene.layers.len() || tracks.len() != scene.audio.len() {
            return Err(CoreError::invalid("duplicate scene ids"));
        }
    }
    Ok(())
}

/// The states after each entry, by seq (0 = the start).
struct Run {
    state: State,
    states: BTreeMap<u64, Project>,
}

fn run(project: Project, edits: &[Edit]) -> Run {
    let ctx = testkit::ctx("2026-10-14T12:30:00Z", 1);
    let mut state = testkit::state(project);
    let mut states = BTreeMap::from([(0, state.project.clone())]);
    let mut ids = Ids(0);
    for (i, edit) in edits.iter().enumerate() {
        let mut before = state.clone();
        let mut next = state.clone();
        let label = format!("{i}: {edit:?}");
        if let Edit::Record { frames } = edit {
            // Facts first: the take enters the pool without an entry.
            if record_take(&mut before.project, *frames).is_none() {
                continue;
            }
            next = before.clone();
            let take = TakeId::numbered(before.project.next_take_number - 1);
            next.project.timeline.clips.push(Clip {
                id: ids.clip(),
                take,
                in_: 0,
                out: *frames,
            });
        } else {
            apply_edit(&mut next.project, edit, &mut ids, &state);
        }
        structural(&next.project).unwrap_or_else(|e| panic!("edit {label} broke the project: {e}"));
        let c = commit(&before, &ctx, label, Value::Null, next);
        if let Some(seq) = c.seq {
            states.insert(seq, c.state.project.clone());
        }
        state = c.state;
    }
    Run { state, states }
}

/// `actual` is `expected` up to facts that undo never touches: takes
/// recorded later stay in the pool and `nextTakeNumber` only grows.
fn assert_same(actual: &Project, expected: &Project, what: &str) {
    let mut actual = actual.clone();
    actual.takes.retain(|id, _| expected.takes.contains_key(id));
    actual.next_take_number = expected.next_take_number;
    assert_eq!(&actual, expected, "{what}");
}

fn assert_guarded(patch: &Patch, what: &str) {
    for (i, op) in patch.0.iter().enumerate() {
        if matches!(op, PatchOperation::Replace(_) | PatchOperation::Remove(_)) {
            let guard = i.checked_sub(1).map(|j| &patch.0[j]);
            assert!(
                matches!(guard, Some(PatchOperation::Test(t)) if t.path == *op.path()),
                "{what}: op {i} ({op}) isn't guarded by a test"
            );
        }
        if let PatchOperation::Add(add) = op {
            let tokens: Vec<_> = add
                .path
                .tokens()
                .map(|t| t.decoded().into_owned())
                .collect();
            assert!(
                tokens.last().is_some_and(|t| t.parse::<usize>().is_ok()),
                "{what}: add {} adds an object member",
                add.path
            );
        }
    }
}

fn arb_start() -> impl Strategy<Value = Project> {
    strategies::arb_project()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// undo∘apply = id, redo∘undo = id, one step at a time and all at once.
    #[test]
    fn undo_and_redo_walk_back_and_forth(
        start in arb_start(),
        edits in prop::collection::vec(arb_edit(), 1..30),
    ) {
        let Run { state, states } = run(start, &edits);
        let n = state.history.entries.len();
        prop_assert_eq!(cursor_position(&state.history).unwrap(), n);
        for entry in state.history.entries.iter() {
            assert_guarded(&entry.forward, &entry.label);
            assert_guarded(&entry.inverse, &entry.label);
            prop_assert!(!entry.forward.0.is_empty());
        }
        // Step by step down…
        let mut s = state.clone();
        for pos in (0..n).rev() {
            s = ops::undo_with(&s, 1, structural).unwrap();
            let seq = ops::seq_at(&s.history, pos);
            prop_assert_eq!(s.history.cursor, seq);
            assert_same(&s.project, &states[&seq], &format!("undo to {seq}"));
        }
        // …and up again.
        for pos in 1..=n {
            s = ops::redo_with(&s, 1, structural).unwrap();
            let seq = ops::seq_at(&s.history, pos);
            assert_same(&s.project, &states[&seq], &format!("redo to {seq}"));
        }
        prop_assert_eq!(&s.project, &state.project);
        prop_assert_eq!(&s.history, &state.history);
        // All at once.
        if n > 0 {
            let bottom = ops::undo_with(&state, n as u32, structural).unwrap();
            assert_same(&bottom.project, &states[&0], "undo all");
            let top = ops::redo_with(&bottom, n as u32, structural).unwrap();
            prop_assert_eq!(&top.project, &state.project);
        }
    }

    /// Restore reaches every recorded state from anywhere.
    #[test]
    fn restore_reaches_every_state(
        start in arb_start(),
        edits in prop::collection::vec(arb_edit(), 1..20),
        from in any::<Index>(),
    ) {
        let Run { state, states } = run(start, &edits);
        let seqs: Vec<u64> = states.keys().copied().collect();
        let origin = ops::restore_with(&state, seqs[from.index(seqs.len())], structural).unwrap();
        for (seq, expected) in &states {
            let s = ops::restore_with(&origin, *seq, structural).unwrap();
            prop_assert_eq!(s.history.cursor, *seq);
            assert_same(&s.project, expected, &format!("restore {seq}"));
        }
    }

    /// A diff turns one view into the other whatever the projects are.
    #[test]
    fn diffs_connect_any_two_projects(a in arb_start(), b in arb_start()) {
        let (before, after) = aligned_views(&a, &b);
        for (from, to) in [(&before, &after), (&after, &before)] {
            let patch = diff(from, to);
            assert_guarded(&patch, "diff");
            let mut doc = from.clone();
            json_patch::patch(&mut doc, &patch).unwrap();
            prop_assert_eq!(&doc, to);
        }
    }

    /// One changed clip costs the same patch on a timeline of 1 or 400 clips.
    #[test]
    fn patch_size_does_not_grow_with_the_timeline(
        n in 1usize..400,
        pick in any::<Index>(),
        kind in 0usize..6,
    ) {
        let mut b = ProjectBuilder::new().screen_layer("L1", "DP-1").take("take-0001", 100_000);
        for i in 0..n as u64 {
            b = b.clip(&format!("c{i:03x}"), "take-0001", i * 10, i * 10 + 10);
        }
        let prev = b.build();
        let k = pick.index(n);
        let mut next = prev.clone();
        let clips = &mut next.timeline.clips;
        let (forward_max, inverse_max) = match kind {
            0 => { clips[k].out -= 1; (2, 2) }
            1 => {
                let c = clips[k].clone();
                clips[k].out = c.in_ + 5;
                clips.insert(k + 1, Clip { id: "x1".parse().unwrap(), take: c.take, in_: c.in_ + 5, out: c.out });
                (3, 4)
            }
            2 => { clips.remove(k); (2, 1) }
            3 => { let c = clips.remove(k); clips.push(c); (3, 3) }
            4 => {
                clips.insert(k, Clip { id: "x2".parse().unwrap(), take: "take-0001".parse().unwrap(), in_: 0, out: 7 });
                (1, 2)
            }
            _ => { next.name = "renamed".into(); (2, 2) }
        };
        let (before, after) = aligned_views(&prev, &next);
        let forward = diff(&before, &after);
        let inverse = diff(&after, &before);
        prop_assert!(forward.0.len() <= forward_max, "forward {}: {}", forward.0.len(), forward);
        prop_assert!(inverse.0.len() <= inverse_max, "inverse {}: {}", inverse.0.len(), inverse);
    }

    /// Removing m clips anywhere costs exactly m tests + m removes.
    #[test]
    fn patch_size_grows_with_the_changed_clips(
        n in 1usize..200,
        picks in prop::collection::btree_set(any::<u16>(), 1..20),
    ) {
        let mut b = ProjectBuilder::new().screen_layer("L1", "DP-1").take("take-0001", 100_000);
        for i in 0..n as u64 {
            b = b.clip(&format!("c{i:03x}"), "take-0001", i * 10, i * 10 + 10);
        }
        let prev = b.build();
        let gone: BTreeSet<usize> = picks.iter().map(|p| usize::from(*p) % n).collect();
        let mut next = prev.clone();
        next.timeline.clips = prev
            .timeline
            .clips
            .iter()
            .enumerate()
            .filter(|(i, _)| !gone.contains(i))
            .map(|(_, c)| c.clone())
            .collect();
        let (before, after) = aligned_views(&prev, &next);
        prop_assert_eq!(diff(&before, &after).0.len(), 2 * gone.len());
        prop_assert_eq!(diff(&after, &before).0.len(), gone.len());
    }

    /// After a purge every reachable state is free of the take, every step
    /// still applies, and kept entries lead to the old states minus the take.
    #[test]
    fn purge_leaves_no_reachable_state_with_the_take(
        start in arb_start(),
        edits in prop::collection::vec(arb_edit(), 1..25),
        undo in any::<Index>(),
        victim in any::<Index>(),
        from_bin in any::<bool>(),
    ) {
        let Run { mut state, mut states } = run(start, &edits);
        let takes: Vec<TakeId> = state.project.takes.keys().cloned().collect();
        let bin: Vec<TakeId> = takes
            .iter()
            .filter(|t| state.project.timeline.clips.iter().all(|c| &c.take != *t))
            .cloned()
            .collect();
        let take = match (from_bin, bin.is_empty()) {
            (true, false) => bin[victim.index(bin.len())].clone(),
            _ => takes[victim.index(takes.len())].clone(),
        };
        // The take must be in the bin: remove its clips first (one entry).
        if state.project.timeline.clips.iter().any(|c| c.take == take) {
            let mut next = state.clone();
            next.project.timeline.clips.retain(|c| c.take != take);
            let ctx = testkit::ctx("2026-10-14T12:30:00Z", 1);
            let c = commit(&state, &ctx, "Remove the take's clips".into(), Value::Null, next);
            states.insert(c.seq.unwrap(), c.state.project.clone());
            state = c.state;
        }
        // Somewhere in the middle of the history, if that doesn't bring the take back.
        let n = state.history.entries.len();
        let back = undo.index(n + 1);
        if back > 0 {
            let undone = ops::undo_with(&state, back as u32, structural).unwrap();
            if undone.project.timeline.clips.iter().all(|c| c.take != take) {
                state = undone;
            }
        }

        let rewrite = purge_rewrite(&state, std::slice::from_ref(&take)).unwrap();
        let mut purged = state.clone();
        purged.history = rewrite.history.clone();
        remove_takes(&mut purged.project, std::slice::from_ref(&take));
        let h = &purged.history;
        prop_assert_eq!(rewrite.entries_lost + h.entries.len(), state.history.entries.len());
        prop_assert!(position_of(h, h.cursor).is_some());
        let take_ptr = format!("/takes/{take}");
        for entry in h.entries.iter() {
            assert_guarded(&entry.forward, &entry.label);
            for op in entry.forward.0.iter().chain(entry.inverse.0.iter()) {
                prop_assert!(!op.path().as_str().starts_with(&take_ptr), "{} touches {}", op, take);
                prop_assert!(!op.to_string().contains(&format!("\"take\":\"{take}\"")), "{} mentions {}", op, take);
            }
            // Kept entries are old entries; the state after each is the old one minus the take.
            let restored = ops::restore_with(&purged, entry.seq, structural).unwrap();
            let mut expected = states[&entry.seq].clone();
            remove_takes(&mut expected, std::slice::from_ref(&take));
            assert_same(&restored.project, &expected, &format!("restore {}", entry.seq));
        }
        let pos = cursor_position(h).unwrap();
        let mut s = purged.clone();
        for _ in 0..pos {
            s = ops::undo_with(&s, 1, structural).unwrap();
            prop_assert!(s.project.timeline.clips.iter().all(|c| c.take != take));
        }
        let mut expected_base = states[&h.base_seq].clone();
        remove_takes(&mut expected_base, std::slice::from_ref(&take));
        assert_same(&s.project, &expected_base, "the new base");
        for _ in 0..h.entries.len() {
            s = ops::redo_with(&s, 1, structural).unwrap();
            prop_assert!(s.project.timeline.clips.iter().all(|c| c.take != take));
        }
        // Back at the cursor: the current project minus the take.
        let at_cursor = ops::restore_with(&s, h.cursor, structural).unwrap();
        prop_assert_eq!(&at_cursor.project, &purged.project);
    }

    /// Compaction keeps the newest entries and the current state.
    #[test]
    fn compaction_preserves_the_current_state(
        start in arb_start(),
        edits in prop::collection::vec(arb_edit(), 5..30),
        keep in 1usize..6,
        undo in any::<Index>(),
    ) {
        let Run { state, states } = run(start, &edits);
        let n = state.history.entries.len();
        let state = match undo.index(n + 1) {
            0 => state,
            k => ops::undo_with(&state, k as u32, structural).unwrap(),
        };
        let Some((history, op)) = ops::compact_to(&state.history, &state.project, keep, keep) else {
            prop_assume!(false);
            unreachable!()
        };
        let pos = cursor_position(&state.history).unwrap();
        let HistoryOp::Rewrite { entries, base, base_seq, cursor, next_seq } = op else {
            panic!("compaction rewrites the history")
        };
        prop_assert_eq!(&entries, &*history.entries);
        prop_assert_eq!((cursor, next_seq), (state.history.cursor, state.history.next_seq));
        prop_assert_eq!(base_seq, history.base_seq);
        let dropped = n - entries.len();
        prop_assert!(dropped <= pos, "never past the cursor");
        prop_assert_eq!(&entries[..], &state.history.entries[dropped..]);
        assert_same(&base, &states[&base_seq], "the base is the old state");
        // The current state is rebuilt from the base, and reached again by undo/redo.
        let mut compacted = state.clone();
        compacted.history = history;
        let new_pos = cursor_position(&compacted.history).unwrap();
        let rebuilt = ops::replay(&base, &entries[..new_pos], structural).unwrap();
        prop_assert_eq!(&rebuilt, &state.project);
        if new_pos > 0 {
            let bottom = ops::undo_with(&compacted, new_pos as u32, structural).unwrap();
            prop_assert_eq!(&bottom.project, &*base);
            let top = ops::redo_with(&bottom, new_pos as u32, structural).unwrap();
            prop_assert_eq!(&top.project, &state.project);
        }
    }

    /// An external edit is merged as one entry that undo takes back.
    #[test]
    fn external_edits_undo_cleanly(
        start in arb_start(),
        edits in prop::collection::vec(arb_edit(), 1..8),
    ) {
        let begin = testkit::state(start.clone());
        let Run { state: edited, .. } = run(start, &edits);
        // The file holds the edited project; memory holds the start (plus
        // the takes the edits recorded, which a hand edit can't add as facts
        // here: keep them out of the file).
        let mut file = edited.project.clone();
        file.takes.retain(|id, _| begin.project.takes.contains_key(id));
        file.next_take_number = begin.project.next_take_number;
        file.timeline.clips.retain(|c| begin.project.takes.contains_key(&c.take));
        let merge = external::merge_external(&begin, file.clone()).unwrap();
        prop_assert!(merge.warnings.is_empty(), "{:?}", merge.warnings);
        prop_assert_eq!(&merge.project, &file);
        let ctx = testkit::ctx("2026-10-14T12:30:00Z", 1);
        let mut next = begin.clone();
        next.project = merge.project;
        let c = commit(&begin, &ctx, "External edit".into(), json!({"external": true}), next);
        if c.seq.is_some() {
            let undone = ops::undo_with(&c.state, 1, structural).unwrap();
            prop_assert_eq!(&undone.project, &begin.project);
            let redone = ops::redo_with(&undone, 1, structural).unwrap();
            prop_assert_eq!(&redone.project, &file);
        } else {
            prop_assert_eq!(&file, &begin.project);
        }
    }
}
