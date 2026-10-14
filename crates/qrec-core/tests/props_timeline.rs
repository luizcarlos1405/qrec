//! Timeline properties (A§5.2, A§5.10, A§11): split identity, trim bounds,
//! move permutation, selection normalization, and clip commands that keep a
//! project valid.

use std::collections::BTreeSet;

use proptest::prelude::*;
use proptest::sample::Index;
use qrec_core::apply::{Outcome, apply};
use qrec_core::protocol::command::{Command, InsertAt, MoveTo, SeekTarget, SelectOp};
use qrec_core::protocol::effect::{Effect, HistoryOp};
use qrec_core::state::{CanvasMode, State, UiState};
use qrec_core::testkit::{self, strategies};
use qrec_core::time::{Sign, TimeArg, TimeValue};
use qrec_core::timeline;
use qrec_core::ui_normalize::{canvas_scene, ui_normalize};
use qrec_core::{
    Clip, ClipId, ErrorCode, Fps, Frame, LayerId, Project, TakeId, TakeStatus, TrackId,
};

/// Maximal runs of continuous clips (same take, `out == next.in`) as
/// `(take, in, out)`: what the export and GES plans render (A§9.3).
fn units(clips: &[Clip]) -> Vec<(TakeId, Frame, Frame)> {
    let mut out: Vec<(TakeId, Frame, Frame)> = Vec::new();
    for clip in clips {
        match out.last_mut() {
            Some((take, _, end)) if *take == clip.take && *end == clip.in_ => *end = clip.out,
            _ => out.push((clip.take.clone(), clip.in_, clip.out)),
        }
    }
    out
}

/// Every timeline frame as (take, take frame).
fn frames(clips: &[Clip]) -> Vec<(TakeId, Frame)> {
    (0..timeline::total_frames(clips))
        .map(|f| {
            let (take, frame) = timeline::source_at(clips, f).expect("inside the timeline");
            (take.clone(), frame)
        })
        .collect()
}

/// A clip that can be split and a cut strictly inside it.
fn pick_cut(clips: &[Clip], which: Index, cut: Index) -> Option<(usize, Frame)> {
    let splittable: Vec<usize> = (0..clips.len()).filter(|&i| clips[i].len() >= 2).collect();
    if splittable.is_empty() {
        return None;
    }
    let i = splittable[which.index(splittable.len())];
    let clip = &clips[i];
    Some((
        i,
        clip.in_ + 1 + cut.index((clip.len() - 1) as usize) as Frame,
    ))
}

proptest! {
    #[test]
    fn split_keeps_what_every_frame_shows(p in strategies::arb_project(), which in any::<Index>(), cut in any::<Index>()) {
        let clips = &p.timeline.clips;
        let Some((i, at)) = pick_cut(clips, which, cut) else { return Ok(()); };
        let original = &clips[i];
        let new_id = ClipId::from_raw("cnew");
        let out = timeline::split(clips, i, at, new_id.clone()).unwrap();
        prop_assert_eq!(out.len(), clips.len() + 1);
        prop_assert_eq!(&out[i].id, &original.id, "the left part keeps the id");
        prop_assert_eq!(&out[i + 1].id, &new_id);
        prop_assert_eq!((out[i].in_, out[i].out), (original.in_, at));
        prop_assert_eq!((out[i + 1].in_, out[i + 1].out), (at, original.out));
        prop_assert_eq!(timeline::total_frames(&out), timeline::total_frames(clips));
        prop_assert_eq!(frames(&out), frames(clips));
        prop_assert_eq!(units(&out), units(clips), "render units (and so the plans) don't change");
        let mut points = timeline::edit_points(clips);
        points.push(timeline::position_of(clips, i) + (at - original.in_));
        points.sort_unstable();
        prop_assert_eq!(timeline::edit_points(&out), points, "the cut is the only new edit point");

        // The command does the same, with a fresh id.
        let state = testkit::state(p.clone());
        let cmd = Command::ClipSplit {
            clip: Some(original.id.clone()),
            at: TimeArg::frames(at - original.in_),
            timeline: false,
        };
        let done = apply(&state, &testkit::ctx(testkit::CREATED_AT, 3), cmd).unwrap();
        let after = &done.state.project.timeline.clips;
        prop_assert_eq!(frames(after), frames(clips));
        prop_assert_eq!(units(after), units(clips));
        let reply = done.reply.unwrap().unwrap();
        prop_assert_eq!(reply.created.len(), 1);
        prop_assert_eq!(after[i + 1].id.as_str(), reply.created[0].as_str());
        prop_assert!(!clips.iter().any(|c| c.id == after[i + 1].id), "the new id is fresh");
    }

    #[test]
    fn trim_keeps_clips_inside_their_take(
        p in strategies::arb_project(),
        which in any::<Index>(),
        a in -700i64..1400,
        b in -700i64..1400,
    ) {
        let clips = &p.timeline.clips;
        if clips.is_empty() { return Ok(()); }
        let i = which.index(clips.len());
        let duration = p.take(&clips[i].take).unwrap().duration_frames;
        let max = duration as i64;
        let (want_in, want_out) = (a.clamp(0, max), b.clamp(0, max));
        match timeline::trim(clips, i, a, b, duration) {
            Ok(trimmed) => {
                let clip = &trimmed.clips[i];
                prop_assert!(clip.in_ < clip.out && clip.out <= duration);
                prop_assert_eq!((clip.in_ as i64, clip.out as i64), (want_in, want_out));
                prop_assert_eq!(trimmed.clamped, (want_in, want_out) != (a, b));
                prop_assert_eq!(&clip.id, &clips[i].id);
                prop_assert_eq!(&clip.take, &clips[i].take);
                for (j, (x, y)) in trimmed.clips.iter().zip(clips).enumerate() {
                    if j != i { prop_assert_eq!(x, y); }
                }
            }
            Err(e) => {
                prop_assert_eq!(e.code, ErrorCode::Invalid);
                prop_assert!(want_in >= want_out, "only an empty result fails");
            }
        }
    }

    #[test]
    fn move_block_is_an_ordered_permutation(
        p in strategies::arb_project(),
        mask in any::<u16>(),
        kind in 0u8..4,
        n in -12i64..12,
        anchor in any::<Index>(),
    ) {
        let clips = &p.timeline.clips;
        let mut ids: Vec<ClipId> = clips
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, c)| c.id.clone())
            .collect();
        if mask & 0x8000 != 0 {
            ids.reverse(); // the block keeps timeline order whatever order the ids come in
        }
        if ids.is_empty() {
            prop_assert_eq!(timeline::move_block(clips, &ids, &MoveTo::By(n)).unwrap_err().code, ErrorCode::Usage);
            return Ok(());
        }
        let anchor = clips[anchor.index(clips.len())].id.clone();
        let to = match kind {
            0 => MoveTo::Index(n.unsigned_abs() as usize),
            1 => MoveTo::By(n),
            2 => MoveTo::Before(anchor),
            _ => MoveTo::After(anchor),
        };
        let result = timeline::move_block(clips, &ids, &to);
        if let MoveTo::Before(c) | MoveTo::After(c) = &to {
            if ids.contains(c) {
                prop_assert_eq!(result.unwrap_err().code, ErrorCode::Invalid);
                return Ok(());
            }
        }
        let out = result.unwrap();

        let mut sorted_in = clips.clone();
        let mut sorted_out = out.clone();
        sorted_in.sort_by(|x, y| x.id.cmp(&y.id));
        sorted_out.sort_by(|x, y| x.id.cmp(&y.id));
        prop_assert_eq!(sorted_out, sorted_in, "a permutation");

        let in_block = |c: &&Clip| ids.contains(&c.id);
        let block: Vec<&Clip> = clips.iter().filter(in_block).collect();
        let rest: Vec<&Clip> = clips.iter().filter(|c| !in_block(c)).collect();
        let start = out.iter().position(|c| ids.contains(&c.id)).unwrap();
        let landed: Vec<&Clip> = out[start..start + block.len()].iter().collect();
        prop_assert_eq!(landed, block, "contiguous, in timeline order");
        let rest_after: Vec<&Clip> = out.iter().filter(|c| !in_block(c)).collect();
        prop_assert_eq!(rest_after, rest.clone(), "the other clips keep their order");

        let first = clips.iter().position(|c| ids.contains(&c.id)).unwrap();
        let expected = match &to {
            MoveTo::Index(i) => (*i).min(rest.len()),
            MoveTo::By(n) => (first as i64 + n).clamp(0, rest.len() as i64) as usize,
            MoveTo::Before(c) => rest.iter().position(|x| &x.id == c).unwrap(),
            MoveTo::After(c) => rest.iter().position(|x| &x.id == c).unwrap() + 1,
        };
        prop_assert_eq!(start, expected);
    }

    #[test]
    fn edit_points_and_positions_agree(p in strategies::arb_project()) {
        let clips = &p.timeline.clips;
        let total = timeline::total_frames(clips);
        let points = timeline::edit_points(clips);
        prop_assert_eq!(points.first().copied(), Some(0));
        prop_assert_eq!(points.last().copied(), Some(total));
        prop_assert!(points.windows(2).all(|w| w[0] < w[1]), "ascending, no duplicates");
        for i in 0..clips.len() {
            prop_assert!(points.contains(&timeline::position_of(clips, i)));
        }
        for f in 0..total {
            let (i, offset) = timeline::clip_at(clips, f).unwrap();
            prop_assert!(offset < clips[i].len());
            prop_assert_eq!(timeline::position_of(clips, i) + offset, f);
        }
        prop_assert_eq!(timeline::clip_at(clips, total), None);
    }
}

// ------------------------------------------------------------ ui_normalize

/// One change to a timeline, as a step can make it.
#[derive(Debug, Clone)]
enum Edit {
    Remove(Index),
    Split(Index, Index),
    Insert(Index, Index),
    Move(Index, i64),
    Trim(Index, Index, Index),
    Fps(Fps),
}

fn arb_edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        any::<Index>().prop_map(Edit::Remove),
        (any::<Index>(), any::<Index>()).prop_map(|(a, b)| Edit::Split(a, b)),
        (any::<Index>(), any::<Index>()).prop_map(|(a, b)| Edit::Insert(a, b)),
        (any::<Index>(), -3i64..4).prop_map(|(a, n)| Edit::Move(a, n)),
        (any::<Index>(), any::<Index>(), any::<Index>()).prop_map(|(a, b, c)| Edit::Trim(a, b, c)),
        strategies::arb_fps().prop_map(Edit::Fps),
    ]
}

/// `p` after `edits`; new clips are `n1`, `n2`, …
fn edited(p: &Project, edits: &[Edit]) -> Project {
    let mut next = p.clone();
    let takes: Vec<TakeId> = p.takes.keys().cloned().collect();
    for (k, edit) in edits.iter().enumerate() {
        let clips = &next.timeline.clips;
        let new_id = ClipId::from_raw(format!("n{k}"));
        let clips = match edit {
            Edit::Remove(i) if !clips.is_empty() => {
                let id = clips[i.index(clips.len())].id.clone();
                timeline::remove(clips, &[id])
            }
            Edit::Split(which, cut) => match pick_cut(clips, *which, *cut) {
                Some((i, at)) => timeline::split(clips, i, at, new_id).unwrap(),
                None => continue,
            },
            Edit::Insert(take, at) => {
                let take = &takes[take.index(takes.len())];
                let clip = Clip {
                    id: new_id,
                    take: take.clone(),
                    in_: 0,
                    out: p.takes[take].duration_frames,
                };
                timeline::insert(clips, at.index(clips.len() + 1), vec![clip])
            }
            Edit::Move(i, n) if !clips.is_empty() => {
                let id = clips[i.index(clips.len())].id.clone();
                timeline::move_block(clips, &[id], &MoveTo::By(*n)).unwrap()
            }
            Edit::Trim(i, a, b) if !clips.is_empty() => {
                let i = i.index(clips.len());
                let c = &clips[i];
                let new_in = c.in_ + a.index(c.len() as usize) as Frame;
                let new_out = new_in + 1 + b.index((c.out - new_in) as usize) as Frame;
                let duration = p.takes[&c.take].duration_frames;
                timeline::trim(clips, i, new_in as i64, new_out as i64, duration)
                    .unwrap()
                    .clips
            }
            Edit::Fps(fps) => {
                next.canvas.fps = *fps;
                continue;
            }
            _ => continue,
        };
        next.timeline.clips = clips;
    }
    next
}

/// A UI to normalize. The selection is drawn from the old timeline (0),
/// the new one (1) and an id that exists nowhere (2).
#[derive(Debug, Clone)]
struct UiSpec {
    picks: Vec<(u8, Index)>,
    playhead: Frame,
    program: bool,
    layer: Option<u32>,
    track: Option<u32>,
    zoom: Option<f64>,
}

fn arb_ui() -> impl Strategy<Value = UiSpec> {
    (
        prop::collection::vec((0u8..3, any::<Index>()), 0..6),
        0u64..6000,
        any::<bool>(),
        prop::option::of(1u32..5),
        prop::option::of(1u32..4),
        prop::option::of(0.01f64..100.0),
    )
        .prop_map(|(picks, playhead, program, layer, track, zoom)| UiSpec {
            picks,
            playhead,
            program,
            layer,
            track,
            zoom,
        })
}

proptest! {
    #[test]
    fn ui_normalize_keeps_the_selection_valid_and_ordered(
        prev in strategies::arb_project(),
        edits in prop::collection::vec(arb_edit(), 0..6),
        spec in arb_ui(),
    ) {
        let UiSpec { picks, playhead, program, layer, track, zoom } = spec;
        let next = edited(&prev, &edits);
        let mut ui = UiState {
            playhead,
            canvas_mode: if program { CanvasMode::Program } else { CanvasMode::Live },
            selected_layer: layer.map(LayerId::numbered),
            solo: track.map(TrackId::numbered),
            zoom,
            ..UiState::default()
        };
        for (source, pick) in picks {
            let from = match source {
                0 => &prev.timeline.clips,
                1 => &next.timeline.clips,
                _ => {
                    ui.selection.push(ClipId::from_raw("zz9"));
                    continue;
                }
            };
            if !from.is_empty() {
                ui.selection.push(from[pick.index(from.len())].id.clone());
            }
        }

        let out = ui_normalize(&prev, &next, &ui);
        let clips = &next.timeline.clips;
        let indices: Vec<usize> = out
            .selection
            .iter()
            .map(|id| timeline::index_of(clips, id))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| TestCaseError::fail("a selected clip isn't on the timeline"))?;
        prop_assert!(indices.windows(2).all(|w| w[0] < w[1]), "timeline order, each id once");
        let kept: BTreeSet<&ClipId> = out.selection.iter().collect();
        for id in &ui.selection {
            if timeline::index_of(clips, id).is_some() {
                prop_assert!(kept.contains(id), "{} is on the timeline and stays selected", id);
            }
        }
        prop_assert!(out.playhead <= timeline::total_frames(clips));
        let scene = canvas_scene(&next, &out);
        if let Some(layer) = &out.selected_layer {
            prop_assert!(scene.layer(layer).is_some());
        }
        if let Some(track) = &out.solo {
            prop_assert!(scene.track(track).is_some());
        }
        prop_assert_eq!(out.zoom.is_some(), zoom.is_some());
        prop_assert!(out.zoom.is_none_or(|z| z.is_finite() && z > 0.0));
        prop_assert_eq!(&ui_normalize(&next, &next, &out), &out, "idempotent");
    }
}

// ------------------------------------------------------- clip commands

/// A clip or UI command whose targets are picked from the state it runs on.
#[derive(Debug, Clone)]
enum Op {
    Add {
        take: Index,
        at: u8,
        pos: Index,
    },
    Move {
        pick: u16,
        to: u8,
        n: i64,
        anchor: Index,
    },
    Trim {
        clip: Index,
        start: Option<At>,
        end: Option<At>,
        timeline: bool,
    },
    Split {
        clip: Index,
        at: At,
        timeline: bool,
        anywhere: bool,
    },
    Remove {
        pick: u16,
    },
    Select {
        op: u8,
        clip: Index,
        pick: u16,
    },
    Seek {
        kind: u8,
        n: u16,
    },
}

/// A time argument: any value in frames (absolute, `+` or `-`), or an
/// absolute one inside the target clip (its end included).
#[derive(Debug, Clone, Copy)]
enum At {
    Raw(u8, u16),
    Inside(Index),
}

fn arb_at() -> impl Strategy<Value = At> {
    prop_oneof![
        (0u8..3, 0u16..700).prop_map(|(sign, n)| At::Raw(sign, n)),
        any::<Index>().prop_map(At::Inside),
    ]
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (any::<Index>(), 0u8..4, any::<Index>()).prop_map(|(take, at, pos)| Op::Add {
            take,
            at,
            pos
        }),
        (any::<u16>(), 0u8..4, -5i64..6, any::<Index>()).prop_map(|(pick, to, n, anchor)| {
            Op::Move {
                pick,
                to,
                n,
                anchor,
            }
        }),
        (
            any::<Index>(),
            prop::option::of(arb_at()),
            prop::option::of(arb_at()),
            any::<bool>()
        )
            .prop_map(|(clip, start, end, timeline)| Op::Trim {
                clip,
                start,
                end,
                timeline
            }),
        (any::<Index>(), arb_at(), any::<bool>(), any::<bool>()).prop_map(
            |(clip, at, timeline, anywhere)| Op::Split {
                clip,
                at,
                timeline,
                anywhere
            }
        ),
        any::<u16>().prop_map(|pick| Op::Remove { pick }),
        (0u8..7, any::<Index>(), any::<u16>()).prop_map(|(op, clip, pick)| Op::Select {
            op,
            clip,
            pick
        }),
        (0u8..6, 0u16..700).prop_map(|(kind, n)| Op::Seek { kind, n }),
    ]
}

/// `At` as a time argument for the clip `clip` picks (clip-relative, or a
/// timeline position with `timeline`).
fn resolve(clips: &[Clip], clip: &Index, at: At, timeline: bool) -> TimeArg {
    match at {
        At::Raw(sign, n) => time((sign, n)),
        At::Inside(_) if clips.is_empty() => TimeArg::frames(0),
        At::Inside(offset) => {
            let i = clip.index(clips.len());
            let offset = offset.index(clips[i].len() as usize + 1) as Frame;
            let base = if timeline {
                timeline::position_of(clips, i)
            } else {
                0
            };
            TimeArg::frames(base + offset)
        }
    }
}

fn time((sign, n): (u8, u16)) -> TimeArg {
    let sign = [Sign::Absolute, Sign::Plus, Sign::Minus][usize::from(sign % 3)];
    TimeArg {
        sign,
        value: TimeValue::Frames(u64::from(n)),
    }
}

fn command(state: &State, op: &Op) -> Command {
    let clips = &state.project.timeline.clips;
    let some_clip = |i: &Index| -> ClipId {
        if clips.is_empty() {
            ClipId::from_raw("c404")
        } else {
            clips[i.index(clips.len())].id.clone()
        }
    };
    let picked = |mask: u16| -> Vec<ClipId> {
        clips
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << (i % 16)) != 0)
            .map(|(_, c)| c.id.clone())
            .collect()
    };
    match op {
        Op::Add { take, at, pos } => {
            let takes: Vec<&TakeId> = state.project.takes.keys().collect();
            let at = match at {
                0 => InsertAt::End,
                1 => InsertAt::Index(pos.index(clips.len() + 2)),
                2 => InsertAt::After(some_clip(pos)),
                _ => InsertAt::AfterSelection,
            };
            Command::ClipAdd {
                take: takes[take.index(takes.len())].clone(),
                at,
            }
        }
        Op::Move {
            pick,
            to,
            n,
            anchor,
        } => {
            let to = match to {
                0 => MoveTo::Index(n.unsigned_abs() as usize),
                1 => MoveTo::By(*n),
                2 => MoveTo::Before(some_clip(anchor)),
                _ => MoveTo::After(some_clip(anchor)),
            };
            Command::ClipMove {
                clips: picked(*pick),
                to,
            }
        }
        Op::Trim {
            clip,
            start,
            end,
            timeline,
        } => Command::ClipTrim {
            clip: some_clip(clip),
            start: start.map(|at| resolve(clips, clip, at, *timeline)),
            end: end.map(|at| resolve(clips, clip, at, *timeline)),
            timeline: *timeline,
        },
        Op::Split {
            clip,
            at,
            timeline,
            anywhere,
        } => Command::ClipSplit {
            clip: if *anywhere && *timeline {
                None
            } else {
                Some(some_clip(clip))
            },
            at: resolve(clips, clip, *at, *timeline),
            timeline: *timeline,
        },
        Op::Remove { pick } => Command::ClipRemove {
            clips: picked(*pick),
        },
        Op::Select { op, clip, pick } => Command::Select {
            op: match op {
                0 => SelectOp::Set(picked(*pick)),
                1 => SelectOp::Toggle(some_clip(clip)),
                2 => SelectOp::RangeTo(some_clip(clip)),
                3 => SelectOp::Prev,
                4 => SelectOp::Next,
                5 => SelectOp::All,
                _ => SelectOp::Clear,
            },
        },
        Op::Seek { kind, n } => Command::Seek {
            to: match kind {
                0 => SeekTarget::To(TimeArg::frames(u64::from(*n))),
                1 => SeekTarget::By(time((1, *n))),
                2 => SeekTarget::By(time((2, *n))),
                3 => SeekTarget::EditPrev,
                4 => SeekTarget::EditNext,
                _ => SeekTarget::End,
            },
        },
    }
}

/// The invariants of SPEC §6.3 for the timeline, plus a valid UI.
fn check(state: &State) -> Result<(), TestCaseError> {
    let project = &state.project;
    let mut ids = BTreeSet::new();
    for clip in &project.timeline.clips {
        prop_assert!(ids.insert(&clip.id), "clip ids are unique: {}", clip.id);
        let take = project.take(&clip.take);
        prop_assert!(
            take.is_some_and(|t| t.status == TakeStatus::Ok),
            "{} uses an ok take",
            clip.id
        );
        let take = take.unwrap();
        prop_assert!(
            clip.in_ < clip.out && clip.out <= take.duration_frames,
            "{:?} inside its take",
            clip
        );
    }
    let clips = &project.timeline.clips;
    let indices: Vec<Option<usize>> = state
        .ui
        .selection
        .iter()
        .map(|id| timeline::index_of(clips, id))
        .collect();
    prop_assert!(
        indices.iter().all(Option::is_some),
        "the selection is on the timeline"
    );
    prop_assert!(
        indices.windows(2).all(|w| w[0] < w[1]),
        "the selection is in timeline order"
    );
    prop_assert!(state.ui.playhead <= timeline::total_frames(clips));
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn clip_commands_keep_the_project_valid(
        p in strategies::arb_project(),
        ops in prop::collection::vec(arb_op(), 1..12),
    ) {
        let mut state = testkit::state(p);
        for (step, op) in ops.iter().enumerate() {
            let cmd = command(&state, op);
            let ctx = testkit::ctx("2026-10-14T12:30:00Z", step as u64);
            match apply(&state, &ctx, cmd.clone()) {
                Ok(Outcome { state: next, effects, reply }) => {
                    check(&next)?;
                    let reply = reply.expect("clip and UI commands reply now").expect("an Ok reply");
                    let appended = effects.iter().find_map(|e| match e {
                        Effect::History(HistoryOp::Append(entry)) => Some(entry),
                        _ => None,
                    });
                    match (reply.seq, appended) {
                        (Some(seq), Some(entry)) => {
                            prop_assert!(cmd.is_undoable());
                            prop_assert_eq!(entry.seq, seq);
                            prop_assert_eq!(&entry.cmd, &cmd.to_json());
                            prop_assert!(!entry.label.is_empty());
                            prop_assert_eq!(next.history.cursor, seq);
                            prop_assert_eq!(&effects[1], &Effect::WriteProject);
                        }
                        (None, None) => {
                            prop_assert_eq!(&next.project, &state.project, "no entry, no edit: {:?}", cmd);
                            prop_assert!(!effects.contains(&Effect::WriteProject));
                        }
                        (seq, entry) => prop_assert!(false, "seq {:?} vs entry {:?}", seq, entry.map(|e| e.seq)),
                    }
                    prop_assert_eq!(effects.contains(&Effect::WriteUi), next.ui != state.ui);
                    state = next;
                }
                Err(e) => prop_assert!(
                    matches!(e.code, ErrorCode::Usage | ErrorCode::NotFound | ErrorCode::Invalid | ErrorCode::Unavailable),
                    "{:?} failed with {:?}", cmd, e
                ),
            }
        }
    }
}

// ------------------------------------- plans (needs the output owner's code)

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// A§5.2 / A§9.3: `plan_export(split(p)) == plan_export(p)` and the same
    /// for GES, up to the clip ids a segment lists.
    #[test]
    #[ignore = "needs export_plan::plan_export, ges_plan::plan_ges and render_units (output owner); run with --ignored once they exist"]
    fn split_leaves_the_export_and_ges_plans_unchanged(
        p in strategies::arb_project(),
        which in any::<Index>(),
        cut in any::<Index>(),
    ) {
        use qrec_core::export_plan::{ExportOptions, plan_export, render_units};
        use qrec_core::ges_plan::plan_ges;
        use qrec_core::plans::ExportPlan;
        use qrec_core::protocol::command::{ExportFormat, Quality, VideoCodec};

        let Some((i, at)) = pick_cut(&p.timeline.clips, which, cut) else { return Ok(()); };
        let mut split = p.clone();
        split.timeline.clips = timeline::split(&p.timeline.clips, i, at, ClipId::from_raw("cnew")).unwrap();

        let without_ids = |plan: ExportPlan| {
            let mut plan = plan;
            for segment in &mut plan.segments {
                segment.clips.clear();
            }
            plan
        };
        let caps = testkit::capabilities();
        let opts = ExportOptions {
            format: ExportFormat::Mp4,
            quality: Quality::Standard,
            codec: VideoCodec::H264,
            out: "/tmp/qrec-props/out.mp4".to_owned(),
            allow_vaapi: false,
        };
        match (plan_export(&p, &caps, &opts), plan_export(&split, &caps, &opts)) {
            (Ok(a), Ok(b)) => prop_assert_eq!(without_ids(a), without_ids(b)),
            (a, b) => prop_assert_eq!(a.map(|_| ()), b.map(|_| ())),
        }
        prop_assert_eq!(plan_ges(&p), plan_ges(&split));
        let unit_ranges = |p: &Project| -> Vec<(TakeId, Frame, Frame, Frame)> {
            render_units(p).into_iter().map(|u| (u.take, u.in_, u.out, u.position)).collect()
        };
        prop_assert_eq!(unit_ranges(&p), unit_ranges(&split));
    }
}
