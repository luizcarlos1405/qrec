//! The history rewrite that goes with a take purge (A§7.5). Owner: history.
//!
//! States `S_0` (base) … `S_n`, `S_i` after entry `e_i`; `c` is the cursor's
//! position. With `k = max{i ≤ c : S_i references T}` (timeline only),
//! `e_1 … e_{k+1}` are dropped and the new base is `S_{k+1}` with `T` removed
//! from its pool (it can't reference `T`: the current state doesn't). In the
//! redo tail, with `j = min{i > c : S_i references T}`, `e_j … e_n` are
//! dropped. Kept entries lose their ops under `/takes/T/…` (with their
//! guarding tests); entries left empty are dropped; `after_hash` is
//! recomputed. If no state references `T`, only the `/takes/T` ops go.
//!
//! The states are found by walking the patches from the current project:
//! inverses back from the cursor, forwards into the redo tail. An entry
//! whose patch no longer applies is dropped with everything beyond it (it
//! could not be undone or redone anyway).

use std::collections::BTreeSet;

use serde_json::Value;

use crate::error::CoreError;
use crate::hash::json_hash;
use crate::history::HistoryEntry;
use crate::history::ops::{cursor_position, rewritten};
use crate::history::view::{project_from_value, project_value, undoable_view};
use crate::ids::TakeId;
use crate::model::Project;
use crate::protocol::effect::HistoryOp;
use crate::requantize::rederive_durations;
use crate::state::{History, State};

/// What a purge does to the history.
#[derive(Debug, Clone, PartialEq)]
pub struct PurgeRewrite {
    /// The history after the rewrite.
    pub history: History,
    /// `HistoryOp::Rewrite` for the shell.
    pub op: HistoryOp,
    /// Entries dropped (for the confirmation `details`).
    pub entries_lost: usize,
    /// Label of the oldest surviving entry.
    pub oldest_kept: Option<String>,
}

/// Drops the entries that could bring `takes` back and strips `/takes/<id>`
/// ops from the rest (A§7.5). `state.project` is the current project, still
/// holding the takes.
pub fn purge_rewrite(state: &State, takes: &[TakeId]) -> Result<PurgeRewrite, CoreError> {
    rewrite(&state.history, &state.project, takes)
}

/// [`purge_rewrite`] over a history and the project at its cursor (the
/// takes still in the pool). `Invalid` if a clip of the project uses one of
/// the takes.
pub fn rewrite(
    history: &History,
    project: &Project,
    takes: &[TakeId],
) -> Result<PurgeRewrite, CoreError> {
    let purged: BTreeSet<&str> = takes.iter().map(TakeId::as_str).collect();
    let entries = &history.entries;
    let n = entries.len();
    let c = cursor_position(history)?;
    let current = project_value(project);
    if let Some(clip) = clip_using(&current, &purged) {
        return Err(CoreError::invalid(format!(
            "can't rewrite the history for a purge: clip {clip} still uses the take"
        )));
    }

    // Front: walk back from the cursor; the first state that uses a purged
    // take (or can't be reached) ends the reachable past.
    let mut value = current.clone();
    let mut front = 0;
    for (p, entry) in entries[..c].iter().enumerate().rev() {
        // Undoing entries[p] gives S_p.
        let reached = json_patch::patch(&mut value, &entry.inverse).is_ok();
        if !reached || clip_using(&value, &purged).is_some() {
            front = p + 1;
            break;
        }
    }
    // Redo tail: walk forward; the first state that uses a purged take (or
    // can't be reached) ends the reachable future.
    let mut value = current.clone();
    let mut back = n;
    for (q, entry) in entries.iter().enumerate().skip(c) {
        // Redoing entries[q] gives S_{q+1}.
        let reached = json_patch::patch(&mut value, &entry.forward).is_ok();
        if !reached || clip_using(&value, &purged).is_some() {
            back = q;
            break;
        }
    }

    // The new base: S_front, found by walking back again.
    let mut value = current;
    for entry in entries[front..c].iter().rev() {
        json_patch::patch(&mut value, &entry.inverse)
            .map_err(|e| CoreError::internal(format!("purge: entry {} stopped undoing: {e}", entry.seq)))?;
    }
    let mut base = project_from_value(value)?;
    if base.canvas.fps != project.canvas.fps {
        rederive_durations(&mut base);
    }
    remove_takes(&mut base, takes);

    // Kept entries without the purged takes' ops; empty ones go too.
    let mut kept: Vec<(usize, HistoryEntry)> = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(back).skip(front) {
        let mut entry = entry.clone();
        entry.forward.0.retain(|op| !under_purged(op.path(), &purged));
        entry.inverse.0.retain(|op| !under_purged(op.path(), &purged));
        if !entry.forward.0.is_empty() || !entry.inverse.0.is_empty() {
            kept.push((i, entry));
        }
    }
    let base_seq = match front {
        0 => history.base_seq,
        f => entries[f - 1].seq,
    };
    let cursor = kept
        .iter()
        .rev()
        .find(|(i, _)| *i < c)
        .map_or(base_seq, |(_, e)| e.seq);
    let mut kept: Vec<HistoryEntry> = kept.into_iter().map(|(_, e)| e).collect();

    // Recompute the hashes: the view after each entry, now without the takes.
    let mut view = undoable_view(&base);
    for entry in &mut kept {
        json_patch::patch(&mut view, &entry.forward).map_err(|e| {
            CoreError::internal(format!("purge: entry {} stopped redoing: {e}", entry.seq))
        })?;
        entry.after_hash = json_hash(&view);
    }

    let entries_lost = n - kept.len();
    let oldest_kept = kept.first().map(|e| e.label.clone());
    let (history, op) = rewritten(history, base, base_seq, kept, cursor);
    Ok(PurgeRewrite {
        history,
        op,
        entries_lost,
        oldest_kept,
    })
}

/// Removes `takes` from the pool, and the clips of those takes from every
/// other take's `replaced` list (a later discard must not put them back).
pub fn remove_takes(project: &mut Project, takes: &[TakeId]) {
    let purged: BTreeSet<&TakeId> = takes.iter().collect();
    project.takes.retain(|id, _| !purged.contains(id));
    for take in project.takes.values_mut() {
        take.replaced.retain(|clip| !purged.contains(&clip.take));
    }
}

/// The id of a timeline clip that uses one of `takes`, in a project value.
fn clip_using(value: &Value, takes: &BTreeSet<&str>) -> Option<String> {
    let clips = value.pointer("/timeline/clips")?.as_array()?;
    clips
        .iter()
        .find(|clip| {
            clip.get("take")
                .and_then(Value::as_str)
                .is_some_and(|take| takes.contains(take))
        })
        .map(|clip| clip.get("id").and_then(Value::as_str).unwrap_or("?").to_owned())
}

/// The path is `/takes/<purged take>` or below it.
fn under_purged(path: &json_patch::jsonptr::Pointer, takes: &BTreeSet<&str>) -> bool {
    let mut tokens = path.tokens();
    match (tokens.next(), tokens.next()) {
        (Some(first), Some(take)) => first.decoded() == "takes" && takes.contains(take.decoded().as_ref()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::ctx::Ctx;
    use crate::history::commit;
    use crate::history::ops::{redo_with, undo_with, unchecked};
    use crate::model::Clip;
    use crate::testkit::{self, ProjectBuilder};

    fn ctx() -> Ctx {
        testkit::ctx("2026-10-14T12:30:00Z", 1)
    }

    fn take(n: u32) -> TakeId {
        TakeId::numbered(n)
    }

    fn clip(id: &str, take: u32, in_: u64, out: u64) -> Clip {
        Clip {
            id: id.parse().unwrap(),
            take: TakeId::numbered(take),
            in_,
            out,
        }
    }

    fn edit(state: &State, label: &str, edit: impl FnOnce(&mut Project)) -> State {
        let mut next = state.clone();
        edit(&mut next.project);
        commit(state, &ctx(), label.into(), Value::Null, next).state
    }

    fn start() -> State {
        testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-1")
                .mic("A1", "alsa_input.mic")
                .take("take-0001", 300)
                .take("take-0002", 200)
                .take("take-0003", 100)
                .clip("c001", "take-0001", 0, 300)
                .build(),
        )
    }

    /// Every state reachable from `state` by undo and redo, oldest first.
    fn reachable(state: &State) -> Vec<Project> {
        let pos = crate::history::ops::cursor_position(&state.history).unwrap();
        let bottom = match pos {
            0 => state.clone(),
            p => undo_with(state, p as u32, unchecked).unwrap(),
        };
        let mut out = vec![bottom.project.clone()];
        let mut s = bottom;
        while s.history.entries.len() > crate::history::ops::cursor_position(&s.history).unwrap() {
            s = redo_with(&s, 1, unchecked).unwrap();
            out.push(s.project.clone());
        }
        out
    }

    fn after_purge(state: &State, takes: &[TakeId]) -> (State, PurgeRewrite) {
        let rewrite = purge_rewrite(state, takes).unwrap();
        let mut next = state.clone();
        next.history = rewrite.history.clone();
        remove_takes(&mut next.project, takes);
        (next, rewrite)
    }

    /// e1 adds a clip of take 2, e2 edits take 2's scene, e3 removes the
    /// clip, e4 edits take 3's scene, e5 renames.
    fn history_with_take_2() -> State {
        let s = start();
        let s = edit(&s, "Add take 2", |p| p.timeline.clips.push(clip("c002", 2, 0, 200)));
        let s = edit(&s, "Take 2 layout", |p| {
            p.takes.get_mut(&take(2)).unwrap().scene.layers[0].opacity = 0.5;
        });
        let s = edit(&s, "Remove c002", |p| {
            p.timeline.clips.pop();
        });
        let s = edit(&s, "Take 3 layout", |p| {
            p.takes.get_mut(&take(3)).unwrap().scene.layers[0].opacity = 0.25;
        });
        edit(&s, "Rename", |p| p.name = "talk".into())
    }

    #[test]
    fn the_past_that_used_the_take_is_dropped() {
        let state = history_with_take_2();
        let (next, rewrite) = after_purge(&state, &[take(2)]);
        // S_2 (after the scene edit) is the last state using take 2, so
        // e1..e3 go and the new base is S_3.
        assert_eq!(rewrite.entries_lost, 3);
        assert_eq!(rewrite.oldest_kept.as_deref(), Some("Take 3 layout"));
        let h = &next.history;
        assert_eq!(h.base_seq, 3);
        assert_eq!(h.cursor, 5);
        assert_eq!(h.next_seq, 6);
        assert_eq!(h.entries.iter().map(|e| e.seq).collect::<Vec<_>>(), [4, 5]);
        let HistoryOp::Rewrite { base, base_seq, cursor, next_seq, entries } = &rewrite.op else {
            panic!("a purge rewrites the history")
        };
        assert_eq!((*base_seq, *cursor, *next_seq), (3, 5, 6));
        assert_eq!(entries, &*h.entries);
        assert!(!base.takes.contains_key(&take(2)));
        assert_eq!(base.timeline.clips, [clip("c001", 1, 0, 300)]);
        // Every reachable state is free of take 2 and the patches apply.
        let states = reachable(&next);
        assert_eq!(states.len(), 3);
        assert_eq!(&states[0], &**base);
        assert_eq!(states[2], next.project);
        for p in &states {
            assert!(p.timeline.clips.iter().all(|c| c.take != take(2)));
            assert!(!p.takes.contains_key(&take(2)));
        }
    }

    #[test]
    fn the_future_that_uses_the_take_is_dropped() {
        let s = start();
        let s1 = edit(&s, "Rename", |p| p.name = "a".into());
        let s2 = edit(&s1, "Take 2 layout", |p| {
            p.takes.get_mut(&take(2)).unwrap().scene.layers[0].visible = false;
        });
        let s3 = edit(&s2, "Add take 2", |p| p.timeline.clips.push(clip("c002", 2, 0, 50)));
        let s4 = edit(&s3, "Rename again", |p| p.name = "b".into());
        // Back to S_1: S_3 and S_4 use take 2.
        let state = undo_with(&s4, 3, unchecked).unwrap();
        let (next, rewrite) = after_purge(&state, &[take(2)]);
        // e3, e4 go (they lead to states using take 2); e2 only touched
        // take 2's scene, so it ends up empty and goes too.
        assert_eq!(rewrite.entries_lost, 3);
        assert_eq!(next.history.entries.iter().map(|e| e.seq).collect::<Vec<_>>(), [1]);
        assert_eq!(next.history.cursor, 1);
        assert_eq!(next.history.base_seq, 0);
        assert_eq!(reachable(&next).len(), 2);
    }

    #[test]
    fn untouched_histories_only_lose_the_take_ops() {
        let s = start();
        let s1 = edit(&s, "Both layouts", |p| {
            p.takes.get_mut(&take(2)).unwrap().scene.layers[0].opacity = 0.5;
            p.takes.get_mut(&take(3)).unwrap().scene.layers[0].opacity = 0.5;
        });
        let s2 = edit(&s1, "Rename", |p| p.name = "x".into());
        let (next, rewrite) = after_purge(&s2, &[take(2)]);
        assert_eq!(rewrite.entries_lost, 0);
        assert_eq!(next.history.base_seq, 0);
        let first = &next.history.entries[0];
        assert!(first.forward.0.iter().chain(first.inverse.0.iter()).all(|op| {
            !op.path().as_str().starts_with("/takes/take-0002")
        }));
        assert_eq!(first.forward.0.len(), 2, "test + replace for take 3");
        // Hashes are those of the views without take 2.
        let mut expected = start().project;
        remove_takes(&mut expected, &[take(2)]);
        let mut view = undoable_view(&expected);
        json_patch::patch(&mut view, &first.forward).unwrap();
        assert_eq!(first.after_hash, json_hash(&view));
        assert_eq!(next.history.entries[1].after_hash, json_hash(&undoable_view(&next.project)));
        let states = reachable(&next);
        assert_eq!(states.len(), 3);
        assert_eq!(states[0].takes[&take(3)].scene.layers[0].opacity, 1.0);
    }

    #[test]
    fn an_empty_cursor_entry_moves_the_cursor_back() {
        let s = start();
        let s1 = edit(&s, "Rename", |p| p.name = "x".into());
        let s2 = edit(&s1, "Take 2 layout", |p| {
            p.takes.get_mut(&take(2)).unwrap().scene.layers[0].opacity = 0.5;
        });
        let (next, rewrite) = after_purge(&s2, &[take(2)]);
        assert_eq!(rewrite.entries_lost, 1);
        assert_eq!(next.history.cursor, 1, "the cursor entry went empty");
        assert_eq!(reachable(&next).last().unwrap(), &next.project);
    }

    #[test]
    fn several_takes_at_once() {
        let state = history_with_take_2();
        let state = edit(&state, "Add take 3", |p| p.timeline.clips.push(clip("c003", 3, 0, 100)));
        let state = edit(&state, "Remove c003", |p| {
            p.timeline.clips.pop();
        });
        let (next, rewrite) = after_purge(&state, &[take(2), take(3)]);
        assert_eq!(rewrite.entries_lost, 7, "everything up to the removal of c003");
        assert!(next.history.entries.is_empty());
        assert_eq!(next.history.cursor, 7);
        assert_eq!(next.history.base_seq, 7);
        let HistoryOp::Rewrite { base, .. } = &rewrite.op else { panic!() };
        assert_eq!(**base, next.project);
    }

    #[test]
    fn unreachable_entries_are_dropped() {
        let mut state = history_with_take_2();
        // Corrupt e4's inverse: undo can't get past it any more.
        let entries = std::sync::Arc::make_mut(&mut state.history.entries);
        if let json_patch::PatchOperation::Test(t) = &mut entries[3].inverse.0[0] {
            t.value = serde_json::json!(0.75);
        }
        let (next, rewrite) = after_purge(&state, &[take(3)]);
        assert_eq!(rewrite.entries_lost, 4, "e1..e4 can't be reached");
        assert_eq!(next.history.base_seq, 4);
        assert_eq!(next.history.entries.len(), 1);
    }

    #[test]
    fn a_take_on_the_timeline_is_refused() {
        let state = start();
        let err = purge_rewrite(&state, &[take(1)]).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Invalid);
        assert!(err.message.contains("clip c001"), "{}", err.message);
    }

    #[test]
    fn replaced_clips_of_purged_takes_are_forgotten() {
        let mut project = start().project;
        project.takes.get_mut(&take(3)).unwrap().replaced = vec![clip("c0aa", 2, 0, 10), clip("c0bb", 1, 0, 10)];
        remove_takes(&mut project, &[take(2)]);
        assert!(!project.takes.contains_key(&take(2)));
        assert_eq!(project.takes[&take(3)].replaced, [clip("c0bb", 1, 0, 10)]);
    }
}
