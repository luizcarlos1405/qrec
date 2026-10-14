//! Timeline edits (A§5.2, SPEC §7.1). Owner: timeline.
//!
//! Each handler builds the next state with `crate::timeline` and finishes
//! with `history::commit` (label e.g. "Split clip c7f3 at 00:00:13:20",
//! `cmd` = the command's JSON). New clip ids come from
//! `IdGen::for_state(state, ctx.entropy)`. An edit that changes nothing
//! succeeds without a history entry (`seq: None`).

use std::collections::BTreeSet;

use serde_json::json;

use crate::apply::Outcome;
use crate::ctx::{Ctx, IdGen};
use crate::error::CoreError;
use crate::history::commit;
use crate::ids::{ClipId, TakeId};
use crate::model::{Clip, Project, TakeStatus};
use crate::protocol::command::{Command, InsertAt, MoveTo};
use crate::protocol::reply::{ClipRow, CmdResult};
use crate::state::State;
use crate::time::{TimeArg, timecode};
use crate::timeline::{self, Edge};

/// `qrec clip add <take> [--at N | --after <clip>]`: a clip for the whole take.
pub fn clip_add(
    state: &State,
    ctx: &Ctx,
    take: TakeId,
    at: InsertAt,
) -> Result<Outcome, CoreError> {
    let cmd = Command::ClipAdd {
        take: take.clone(),
        at: at.clone(),
    }
    .to_json();
    let project = &state.project;
    let clips = &project.timeline.clips;
    let source = project.take(&take).ok_or_else(|| {
        CoreError::not_found(format!("no take {take}"))
            .with_hint("list the takes with qrec take list")
    })?;
    if source.status != TakeStatus::Ok {
        let status = match source.status {
            TakeStatus::Recording => "still recording",
            TakeStatus::Broken => "broken",
            TakeStatus::TooShort => "too short",
            TakeStatus::Ok => "ok",
        };
        return Err(CoreError::invalid(format!(
            "take {} is {status}; only ok takes can go on the timeline",
            take.number()
        )));
    }
    if source.duration_frames == 0 {
        return Err(CoreError::invalid(format!(
            "take {} has no frames",
            take.number()
        )));
    }
    let (index, place) = match &at {
        InsertAt::End => (clips.len(), "at the end".to_owned()),
        InsertAt::Index(i) if *i >= clips.len() => (clips.len(), "at the end".to_owned()),
        InsertAt::Index(i) => (*i, format!("at index {i}")),
        InsertAt::After(anchor) => (find(project, anchor)? + 1, format!("after {anchor}")),
        InsertAt::AfterSelection => {
            let last = last_selected(state)?;
            (last + 1, format!("after {}", clips[last].id))
        }
    };
    let id = IdGen::for_state(state, ctx.entropy).next_clip_id();
    let clip = Clip {
        id: id.clone(),
        take: take.clone(),
        in_: 0,
        out: source.duration_frames,
    };
    let mut next = state.clone();
    next.project.timeline.clips = timeline::insert(clips, index, vec![clip]);
    let row = timeline::clip_row(&next.project.timeline.clips, index);
    let label = format!("Insert take {} {place}", take.number());
    let message = format!(
        "Inserted take {} as clip {id} at {}",
        take.number(),
        timecode(row.start, project.canvas.fps)
    );
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(
        committed,
        CmdResult::new(message)
            .with_created([&id])
            .with_result(&row),
    ))
}

/// `qrec clip move`: the clips move as one block in their original order.
pub fn clip_move(
    state: &State,
    ctx: &Ctx,
    clips: Vec<ClipId>,
    to: MoveTo,
) -> Result<Outcome, CoreError> {
    let cmd = Command::ClipMove {
        clips: clips.clone(),
        to: to.clone(),
    }
    .to_json();
    if clips.is_empty() {
        return Err(CoreError::usage("no clips to move")
            .with_hint("qrec clip move <clip>… (--to N | --by ±N | --before C | --after C)"));
    }
    let project = &state.project;
    let before = &project.timeline.clips;
    let block = in_timeline_order(project, &clips)?;
    let after = timeline::move_block(before, &clips, &to)?;
    let what = describe(&block);
    let label = format!("Move {what} {}", describe_move(&to));
    let fps = project.canvas.fps;
    let rows: Vec<ClipRow> = block
        .iter()
        .filter_map(|id| timeline::index_of(&after, id))
        .map(|i| timeline::clip_row(&after, i))
        .collect();
    let message = if &after == before {
        format!("{} already there", capitalize(&what))
    } else {
        let start = rows.first().map_or(0, |r| r.start);
        format!("Moved {what} to {}", timecode(start, fps))
    };
    let mut next = state.clone();
    next.project.timeline.clips = after;
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(
        committed,
        CmdResult::new(message).with_result(&rows),
    ))
}

/// `qrec clip trim`: clip-relative times unless `timeline`; values outside
/// the take are clamped and reported.
pub fn clip_trim(
    state: &State,
    ctx: &Ctx,
    clip: ClipId,
    start: Option<TimeArg>,
    end: Option<TimeArg>,
    timeline: bool,
) -> Result<Outcome, CoreError> {
    let cmd = Command::ClipTrim {
        clip: clip.clone(),
        start,
        end,
        timeline,
    }
    .to_json();
    if start.is_none() && end.is_none() {
        return Err(CoreError::usage(format!("nothing to trim on clip {clip}"))
            .with_hint("give --start and/or --end"));
    }
    let project = &state.project;
    let fps = project.canvas.fps;
    let clips = &project.timeline.clips;
    let i = find(project, &clip)?;
    let current = &clips[i];
    let take = project.take(&current.take).ok_or_else(|| {
        CoreError::invalid(format!(
            "clip {clip} uses take {}, which isn't in the project",
            current.take
        ))
    })?;
    let edge = |e: Edge, arg: Option<TimeArg>, keep: u64| match arg {
        Some(arg) => timeline::trim_edge(clips, i, e, &arg, timeline, fps),
        None => Ok(i64::try_from(keep).unwrap_or(i64::MAX)),
    };
    let new_in = edge(Edge::Start, start, current.in_)?;
    let new_out = edge(Edge::End, end, current.out)?;
    let trimmed = timeline::trim(clips, i, new_in, new_out, take.duration_frames)?;
    let row = timeline::clip_row(&trimmed.clips, i);
    let range = format!(
        "take {} frames [{}, {}) ({} long)",
        current.take.number(),
        row.in_,
        row.out,
        timecode(row.duration, fps)
    );
    let mut message = if (row.in_, row.out) == (current.in_, current.out) {
        format!("Clip {clip} already spans {range}")
    } else {
        format!("Trimmed clip {clip} to {range}")
    };
    if trimmed.clamped {
        message.push_str(&format!(
            "; clamped to the take's frames [0, {})",
            take.duration_frames
        ));
    }
    let label = format!("Trim clip {clip}");
    let mut next = state.clone();
    next.project.timeline.clips = trimmed.clips;
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(
        committed,
        CmdResult::new(message).with_result(&json!({"clip": row, "clamped": trimmed.clamped})),
    ))
}

/// `qrec clip split`: `[in, p)` keeps the id, `[p, out)` gets a new one.
/// `clip: None` (with `timeline`) splits the clip under `at`.
pub fn clip_split(
    state: &State,
    ctx: &Ctx,
    clip: Option<ClipId>,
    at: TimeArg,
    timeline: bool,
) -> Result<Outcome, CoreError> {
    let cmd = Command::ClipSplit {
        clip: clip.clone(),
        at,
        timeline,
    }
    .to_json();
    let project = &state.project;
    let fps = project.canvas.fps;
    let clips = &project.timeline.clips;
    let (i, cut) = match &clip {
        Some(id) => {
            let i = find(project, id)?;
            (i, timeline::split_point(clips, i, &at, timeline, fps)?)
        }
        None if timeline => {
            let t = timeline::absolute_frame(&at, fps, "clip split --at")?;
            let total = timeline::total_frames(clips);
            let (i, offset) = timeline::clip_at(clips, t).ok_or_else(|| {
                CoreError::invalid(format!(
                    "{} is past the end of the timeline ({})",
                    timecode(t, fps),
                    timecode(total, fps)
                ))
            })?;
            if offset == 0 {
                let what = match i.checked_sub(1) {
                    Some(prev) => format!("the cut between {} and {}", clips[prev].id, clips[i].id),
                    None => "the start of the timeline".to_owned(),
                };
                return Err(CoreError::invalid(format!(
                    "{} is already {what}; nothing to split",
                    timecode(t, fps)
                )));
            }
            (i, clips[i].in_ + offset)
        }
        None => {
            return Err(CoreError::usage("no clip to split")
                .with_hint("give a clip id, or --timeline to split the clip under --at"));
        }
    };
    let original = &clips[i];
    let new_id = IdGen::for_state(state, ctx.entropy).next_clip_id();
    let split = timeline::split(clips, i, cut, new_id.clone())?;
    let at_timeline = timeline::position_of(clips, i) + (cut - original.in_);
    let label = format!(
        "Split clip {} at {}",
        original.id,
        timecode(at_timeline, fps)
    );
    let message = format!("{label} into {} and {new_id}", original.id);
    let rows = [
        timeline::clip_row(&split, i),
        timeline::clip_row(&split, i + 1),
    ];
    let mut next = state.clone();
    next.project.timeline.clips = split;
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(
        committed,
        CmdResult::new(message)
            .with_created([&new_id])
            .with_result(&rows),
    ))
}

/// `qrec clip remove`: takes and files stay.
pub fn clip_remove(state: &State, ctx: &Ctx, clips: Vec<ClipId>) -> Result<Outcome, CoreError> {
    let cmd = Command::ClipRemove {
        clips: clips.clone(),
    }
    .to_json();
    if clips.is_empty() {
        return Err(CoreError::usage("no clips to remove").with_hint("qrec clip remove <clip>…"));
    }
    let project = &state.project;
    let gone = in_timeline_order(project, &clips)?;
    let after = timeline::remove(&project.timeline.clips, &gone);
    let used: BTreeSet<&TakeId> = after.iter().map(|c| &c.take).collect();
    let mut binned: Vec<&TakeId> = project
        .timeline
        .clips
        .iter()
        .filter(|c| gone.contains(&c.id) && !used.contains(&c.take))
        .map(|c| &c.take)
        .collect();
    binned.sort();
    binned.dedup();
    let what = describe(&gone);
    let mut message = format!("Removed {what}");
    match binned.as_slice() {
        [] => {}
        [take] => message.push_str(&format!("; take {} is in the bin now", take.number())),
        takes => {
            let numbers: Vec<String> = takes.iter().map(|t| t.number().to_string()).collect();
            message.push_str(&format!(
                "; takes {} are in the bin now",
                join_and(&numbers)
            ));
        }
    }
    let label = format!("Remove {what}");
    let mut next = state.clone();
    next.project.timeline.clips = after;
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(
        committed,
        CmdResult::new(message)
            .with_removed(&gone)
            .with_result(&json!({ "binned": binned })),
    ))
}

// ---------------------------------------------------------------- helpers

/// The index of clip `id`, or `NotFound`.
fn find(project: &Project, id: &ClipId) -> Result<usize, CoreError> {
    timeline::index_of(&project.timeline.clips, id).ok_or_else(|| timeline::unknown_clip(id))
}

/// `ids` as they sit on the timeline: each once, in timeline order.
/// `NotFound` for the first id that isn't on the timeline.
fn in_timeline_order(project: &Project, ids: &[ClipId]) -> Result<Vec<ClipId>, CoreError> {
    let mut indices = Vec::with_capacity(ids.len());
    for id in ids {
        indices.push(find(project, id)?);
    }
    indices.sort_unstable();
    indices.dedup();
    Ok(indices
        .into_iter()
        .map(|i| project.timeline.clips[i].id.clone())
        .collect())
}

/// The index of the last selected clip on the timeline; `Unavailable`
/// without a selection.
fn last_selected(state: &State) -> Result<usize, CoreError> {
    state
        .ui
        .selection
        .iter()
        .filter_map(|id| timeline::index_of(&state.project.timeline.clips, id))
        .max()
        .ok_or_else(|| {
            CoreError::unavailable("nothing is selected")
                .with_hint("select a clip first, or insert with --at N or --after <clip>")
        })
}

/// "clip c7f3", "clips c7f3 and c8a1", "clips c1, c2 and c3", "5 clips".
pub(crate) fn describe(ids: &[ClipId]) -> String {
    match ids {
        [one] => format!("clip {one}"),
        [_, _] | [_, _, _] => {
            let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
            format!("clips {}", join_and(&names))
        }
        many => format!("{} clips", many.len()),
    }
}

fn describe_move(to: &MoveTo) -> String {
    match to {
        MoveTo::Index(i) => format!("to index {i}"),
        MoveTo::By(n) if *n < 0 => format!("left by {}", n.unsigned_abs()),
        MoveTo::By(n) => format!("right by {n}"),
        MoveTo::Before(anchor) => format!("before {anchor}"),
        MoveTo::After(anchor) => format!("after {anchor}"),
    }
}

/// "a", "a and b", "a, b and c".
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::ErrorCode;
    use crate::apply::apply;
    use crate::protocol::effect::{Effect, HistoryOp};
    use crate::state::CanvasMode;
    use crate::testkit::{self, ProjectBuilder};

    const NOW: &str = "2026-10-14T12:30:00Z";

    /// Takes 1 (300 frames), 2 (200) and 3 (tooShort); clips
    /// c001 = take 1 [0, 100), c002 = take 1 [150, 300), c003 = take 2 [0, 200).
    fn state() -> State {
        testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-1")
                .mic("A1", "alsa_input.mic")
                .take("take-0001", 300)
                .take("take-0002", 200)
                .take("take-0003", 5)
                .with_take("take-0003", |t| t.status = TakeStatus::TooShort)
                .clip("c001", "take-0001", 0, 100)
                .clip("c002", "take-0001", 150, 300)
                .clip("c003", "take-0002", 0, 200)
                .build(),
        )
    }

    fn run(state: &State, cmd: Command) -> Result<Outcome, CoreError> {
        apply(state, &testkit::ctx(NOW, 7), cmd)
    }

    fn cid(s: &str) -> ClipId {
        s.parse().unwrap()
    }

    fn t(s: &str) -> TimeArg {
        s.parse().unwrap()
    }

    fn timeline_of(state: &State) -> Vec<String> {
        state
            .project
            .timeline
            .clips
            .iter()
            .map(|c| format!("{} {} [{}, {})", c.id, c.take, c.in_, c.out))
            .collect()
    }

    /// What a snapshot pins: the reply, the history entry (label and cmd;
    /// the patches belong to the history owner's diff), the effects, the
    /// timeline and the UI.
    fn summary(out: &Outcome) -> Value {
        let mut entry = Value::Null;
        let effects: Vec<Value> = out
            .effects
            .iter()
            .map(|e| match e {
                Effect::History(HistoryOp::Append(h)) => {
                    entry = json!({"seq": h.seq, "label": h.label, "cmd": h.cmd});
                    json!(format!("history.append#{}", h.seq))
                }
                other => serde_json::to_value(other).unwrap(),
            })
            .collect();
        json!({
            "reply": out.reply,
            "entry": entry,
            "effects": effects,
            "timeline": timeline_of(&out.state),
            "ui": out.state.ui,
        })
    }

    fn err(state: &State, cmd: Command) -> CoreError {
        run(state, cmd).unwrap_err()
    }

    #[test]
    fn add_inserts_a_whole_take() {
        let s = state();
        let end = run(
            &s,
            Command::ClipAdd {
                take: TakeId::numbered(2),
                at: InsertAt::End,
            },
        )
        .unwrap();
        insta::assert_json_snapshot!("add_at_end", summary(&end));

        let at = |at: InsertAt| {
            let out = run(
                &s,
                Command::ClipAdd {
                    take: TakeId::numbered(1),
                    at,
                },
            )
            .unwrap();
            let entry = out.state.history.entries.last().unwrap().label.clone();
            (
                timeline_of(&out.state)
                    .into_iter()
                    .map(|c| c[..4].to_owned())
                    .collect::<Vec<_>>()
                    .join(" "),
                entry,
            )
        };
        let (order, label) = at(InsertAt::Index(1));
        assert_eq!(label, "Insert take 1 at index 1");
        assert!(
            order.starts_with("c001 c") && order.ends_with("c002 c003"),
            "{order}"
        );
        let (order, label) = at(InsertAt::Index(99));
        assert_eq!(label, "Insert take 1 at the end");
        assert!(order.starts_with("c001 c002 c003 "), "{order}");
        let (order, label) = at(InsertAt::After(cid("c002")));
        assert_eq!(label, "Insert take 1 after c002");
        assert!(
            order.starts_with("c001 c002 c") && order.ends_with(" c003"),
            "{order}"
        );

        let mut selected = s.clone();
        selected.ui.selection = vec![cid("c001")];
        let out = run(
            &selected,
            Command::ClipAdd {
                take: TakeId::numbered(2),
                at: InsertAt::AfterSelection,
            },
        )
        .unwrap();
        insta::assert_json_snapshot!("add_after_selection", summary(&out));
    }

    #[test]
    fn add_refuses_bad_takes_and_anchors() {
        let s = state();
        let add = |take: u32, at: InsertAt| {
            err(
                &s,
                Command::ClipAdd {
                    take: TakeId::numbered(take),
                    at,
                },
            )
        };
        let e = add(9, InsertAt::End);
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrorCode::NotFound, "no take take-0009")
        );
        let e = add(3, InsertAt::End);
        assert_eq!(e.code, ErrorCode::Invalid);
        assert_eq!(
            e.message,
            "take 3 is too short; only ok takes can go on the timeline"
        );
        assert_eq!(
            add(1, InsertAt::After(cid("c999"))).code,
            ErrorCode::NotFound
        );
        let e = add(1, InsertAt::AfterSelection);
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrorCode::Unavailable, "nothing is selected")
        );
    }

    #[test]
    fn new_ids_avoid_every_id_in_use() {
        let s = state();
        let a = run(
            &s,
            Command::ClipAdd {
                take: TakeId::numbered(1),
                at: InsertAt::End,
            },
        )
        .unwrap();
        let b = run(
            &s,
            Command::ClipAdd {
                take: TakeId::numbered(1),
                at: InsertAt::End,
            },
        )
        .unwrap();
        assert_eq!(
            a.state.project.timeline.clips, b.state.project.timeline.clips,
            "same state + entropy ⇒ same id"
        );
        let id = &a.state.project.timeline.clips[3].id;
        // The same entropy against a state that has seen that id picks another.
        let mut seen = s.clone();
        seen.history.remember_clip_ids([id]);
        let c = run(
            &seen,
            Command::ClipAdd {
                take: TakeId::numbered(1),
                at: InsertAt::End,
            },
        )
        .unwrap();
        assert_ne!(&c.state.project.timeline.clips[3].id, id);
    }

    #[test]
    fn move_moves_a_block() {
        let s = state();
        let out = run(
            &s,
            Command::ClipMove {
                clips: vec![cid("c003"), cid("c001")],
                to: MoveTo::By(1),
            },
        )
        .unwrap();
        // The block [c001, c003] is gathered at c001's slot shifted by one: after c002.
        insta::assert_json_snapshot!("move_by_one", summary(&out));

        let moved = |to: MoveTo| {
            let out = run(
                &s,
                Command::ClipMove {
                    clips: vec![cid("c001")],
                    to,
                },
            )
            .unwrap();
            (
                timeline_of(&out.state)
                    .iter()
                    .map(|c| c[..4].to_owned())
                    .collect::<Vec<_>>()
                    .join(" "),
                out.state.history.entries.last().map(|e| e.label.clone()),
            )
        };
        assert_eq!(
            moved(MoveTo::Index(2)),
            (
                "c002 c003 c001".into(),
                Some("Move clip c001 to index 2".into())
            )
        );
        assert_eq!(
            moved(MoveTo::After(cid("c002"))),
            (
                "c002 c001 c003".into(),
                Some("Move clip c001 after c002".into())
            )
        );
        assert_eq!(
            moved(MoveTo::Before(cid("c003"))),
            (
                "c002 c001 c003".into(),
                Some("Move clip c001 before c003".into())
            )
        );
        assert_eq!(
            moved(MoveTo::By(-1)),
            ("c001 c002 c003".into(), None),
            "already first: no entry"
        );

        let out = run(
            &s,
            Command::ClipMove {
                clips: vec![cid("c001")],
                to: MoveTo::By(-1),
            },
        )
        .unwrap();
        let reply = out.reply.unwrap().unwrap();
        assert_eq!(
            (reply.seq, reply.message.as_str()),
            (None, "Clip c001 already there")
        );
        assert!(out.effects.is_empty());
    }

    #[test]
    fn move_refuses_bad_ids() {
        let s = state();
        let mv = |clips: &[&str], to: MoveTo| {
            err(
                &s,
                Command::ClipMove {
                    clips: clips.iter().map(|c| cid(c)).collect(),
                    to,
                },
            )
        };
        assert_eq!(mv(&[], MoveTo::By(1)).code, ErrorCode::Usage);
        assert_eq!(
            mv(&["c001", "c404"], MoveTo::By(1)).message,
            "no clip c404 on the timeline"
        );
        assert_eq!(
            mv(&["c001"], MoveTo::After(cid("c404"))).code,
            ErrorCode::NotFound
        );
        assert_eq!(
            mv(&["c001", "c002"], MoveTo::Before(cid("c002"))).code,
            ErrorCode::Invalid
        );
    }

    #[test]
    fn trim_moves_edges_and_reports_clamping() {
        let s = state();
        // c002 is take 1 [150, 300) at timeline 100..250.
        let trim = |start: Option<&str>, end: Option<&str>, timeline: bool| {
            run(
                &s,
                Command::ClipTrim {
                    clip: cid("c002"),
                    start: start.map(t),
                    end: end.map(t),
                    timeline,
                },
            )
        };
        let out = trim(Some("+10f"), Some("-1s"), false).unwrap();
        insta::assert_json_snapshot!("trim_both_edges", summary(&out));

        let range = |out: &Outcome| {
            let c = &out.state.project.timeline.clips[1];
            (c.in_, c.out)
        };
        assert_eq!(range(&trim(Some("5f"), None, false).unwrap()), (155, 300));
        assert_eq!(range(&trim(None, Some("20f"), false).unwrap()), (150, 170));
        assert_eq!(
            range(&trim(Some("110f"), Some("200f"), true).unwrap()),
            (160, 250)
        );
        // Extending past the take clamps and says so.
        let out = trim(Some("-200f"), Some("+1s"), false).unwrap();
        assert_eq!(range(&out), (0, 300));
        insta::assert_json_snapshot!("trim_clamped", summary(&out));

        assert_eq!(trim(None, None, false).unwrap_err().code, ErrorCode::Usage);
        assert_eq!(
            trim(Some("200f"), None, false).unwrap_err().code,
            ErrorCode::Invalid
        );
        assert_eq!(
            trim(Some("99f"), None, true).unwrap_err().code,
            ErrorCode::Invalid,
            "before the clip"
        );
        assert_eq!(
            trim(Some("00:00:00:31"), None, false).unwrap_err().code,
            ErrorCode::Usage
        );
        let e = err(
            &s,
            Command::ClipTrim {
                clip: cid("c404"),
                start: Some(t("1f")),
                end: None,
                timeline: false,
            },
        );
        assert_eq!(e.code, ErrorCode::NotFound);
        // Trimming to the current range is not an edit.
        let out = trim(Some("0f"), Some("150f"), false).unwrap();
        let reply = out.reply.unwrap().unwrap();
        assert_eq!(reply.seq, None);
        assert_eq!(
            reply.message,
            "Clip c002 already spans take 1 frames [150, 300) (00:00:05:00 long)"
        );
    }

    #[test]
    fn split_at_a_clip_time_or_a_timeline_position() {
        let s = state();
        // c002 is take 1 [150, 300) at timeline 100..250; 13f into it = timeline 113.
        let out = run(
            &s,
            Command::ClipSplit {
                clip: Some(cid("c002")),
                at: t("13f"),
                timeline: false,
            },
        )
        .unwrap();
        insta::assert_json_snapshot!("split_clip_relative", summary(&out));
        let same = run(
            &s,
            Command::ClipSplit {
                clip: Some(cid("c002")),
                at: t("113f"),
                timeline: true,
            },
        )
        .unwrap();
        assert_eq!(same.state.project, out.state.project);
        let under = run(
            &s,
            Command::ClipSplit {
                clip: None,
                at: t("00:00:03:23"),
                timeline: true,
            },
        )
        .unwrap();
        assert_eq!(
            under.state.project, out.state.project,
            "the clip under timeline frame 113"
        );
        assert_eq!(
            under.state.history.entries[0].label,
            "Split clip c002 at 00:00:03:23"
        );

        // The split keeps what every timeline frame shows.
        let (before, after) = (&s.project.timeline.clips, &out.state.project.timeline.clips);
        for f in 0..timeline::total_frames(before) {
            assert_eq!(
                timeline::source_at(after, f),
                timeline::source_at(before, f)
            );
        }
    }

    #[test]
    fn split_refuses_cuts_on_edges() {
        let s = state();
        let split = |clip: Option<&str>, at: &str, timeline: bool| {
            err(
                &s,
                Command::ClipSplit {
                    clip: clip.map(cid),
                    at: t(at),
                    timeline,
                },
            )
        };
        let e = split(None, "100f", true);
        assert_eq!(e.code, ErrorCode::Invalid);
        assert_eq!(
            e.message,
            "00:00:03:10 is already the cut between c001 and c002; nothing to split"
        );
        assert_eq!(
            split(None, "0f", true).message,
            "00:00:00:00 is already the start of the timeline; nothing to split"
        );
        assert_eq!(
            split(None, "450f", true).message,
            "00:00:15:00 is past the end of the timeline (00:00:15:00)"
        );
        assert_eq!(split(None, "5f", false).code, ErrorCode::Usage);
        assert_eq!(split(Some("c001"), "+5f", false).code, ErrorCode::Usage);
        assert_eq!(split(Some("c001"), "100f", false).code, ErrorCode::Invalid);
        assert_eq!(split(Some("c404"), "5f", false).code, ErrorCode::NotFound);
    }

    #[test]
    fn split_keeps_a_selection_on_both_halves() {
        let mut s = state();
        s.ui.selection = vec![cid("c002")];
        s.ui.canvas_mode = CanvasMode::Program;
        let out = run(
            &s,
            Command::ClipSplit {
                clip: Some(cid("c002")),
                at: t("13f"),
                timeline: false,
            },
        )
        .unwrap();
        let new = out.reply.as_ref().unwrap().as_ref().unwrap().created[0].clone();
        assert_eq!(out.state.ui.selection, vec![cid("c002"), cid(&new)]);
        assert!(out.effects.contains(&Effect::WriteUi));
    }

    #[test]
    fn remove_reports_takes_moving_to_the_bin() {
        let s = state();
        let out = run(
            &s,
            Command::ClipRemove {
                clips: vec![cid("c003"), cid("c001"), cid("c003")],
            },
        )
        .unwrap();
        insta::assert_json_snapshot!("remove_two", summary(&out));
        let out = run(
            &s,
            Command::ClipRemove {
                clips: vec![cid("c001"), cid("c002"), cid("c003")],
            },
        )
        .unwrap();
        let reply = out.reply.unwrap().unwrap();
        assert_eq!(
            reply.message,
            "Removed clips c001, c002 and c003; takes 1 and 2 are in the bin now"
        );
        assert_eq!(
            out.state.history.entries[0].label,
            "Remove clips c001, c002 and c003"
        );

        assert_eq!(
            err(&s, Command::ClipRemove { clips: vec![] }).code,
            ErrorCode::Usage
        );
        assert_eq!(
            err(
                &s,
                Command::ClipRemove {
                    clips: vec![cid("c001"), cid("c404")]
                }
            )
            .code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn edits_drop_removed_clips_from_the_selection() {
        let mut s = state();
        s.ui.selection = vec![cid("c001"), cid("c003")];
        let out = run(
            &s,
            Command::ClipRemove {
                clips: vec![cid("c003")],
            },
        )
        .unwrap();
        assert_eq!(out.state.ui.selection, vec![cid("c001")]);
        assert_eq!(
            out.effects[1..],
            [Effect::WriteProject, Effect::WriteUi],
            "history first, then the project, then the UI"
        );
    }

    #[test]
    fn labels_name_many_clips_by_count() {
        let ids: Vec<ClipId> = ["c1", "c2", "c3", "c4"].iter().map(|s| cid(s)).collect();
        assert_eq!(describe(&ids[..1]), "clip c1");
        assert_eq!(describe(&ids[..2]), "clips c1 and c2");
        assert_eq!(describe(&ids[..3]), "clips c1, c2 and c3");
        assert_eq!(describe(&ids), "4 clips");
        assert_eq!(describe_move(&MoveTo::By(-2)), "left by 2");
        assert_eq!(
            describe_move(&MoveTo::By(i64::MIN)),
            "left by 9223372036854775808"
        );
        assert_eq!(describe_move(&MoveTo::By(3)), "right by 3");
    }
}
