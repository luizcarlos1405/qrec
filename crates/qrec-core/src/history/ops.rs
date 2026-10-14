//! Undo, redo, restore and compaction over `History` (A§7.1, A§7.3).
//! Owner: history.
//!
//! Positions: the cursor's *position* is the number of applied entries (0 =
//! the base snapshot), so state `S_p` is the base with `entries[..p]`
//! applied. Undo and redo move by position; `Undo N` / `Redo N` compute
//! every step first and change nothing when one fails (`Invalid` naming
//! its seq).
//!
//! Patches are applied to `view::project_value`, the result is
//! deserialized, take durations are re-derived when the canvas fps changed
//! (A§2.2) and the project is checked. The command handlers check with
//! [`validated`] (A§2.3); the `*_with` variants take the check as an
//! argument for callers that replay states validated when they were made.

use std::sync::Arc;

use json_patch::{Patch, PatchError};
use serde_json::{Value, json};

use crate::error::CoreError;
use crate::history::HistoryEntry;
use crate::history::view::{project_from_value, project_value};
use crate::model::Project;
use crate::protocol::effect::HistoryOp;
use crate::requantize::rederive_durations;
use crate::state::{History, State};

/// Compaction starts above this many entries (A§7.3)…
pub const COMPACT_ABOVE: usize = 5_000;
/// …and keeps this many.
pub const COMPACT_TO: usize = 4_000;

/// How a project produced by history is checked before it becomes the state.
pub type Check = fn(&Project) -> Result<(), CoreError>;

/// The A§2.3 validation: every error makes the step fail.
pub fn validated(project: &Project) -> Result<(), CoreError> {
    crate::validate::ensure_valid(project)
}

/// Accepts every project that deserializes (replaying states that were
/// validated when they were made, tests).
pub fn unchecked(_: &Project) -> Result<(), CoreError> {
    Ok(())
}

/// Which way a step goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Applies `inverse`.
    Undo,
    /// Applies `forward`.
    Redo,
}

impl Direction {
    fn verb(self) -> &'static str {
        match self {
            Direction::Undo => "undo",
            Direction::Redo => "redo",
        }
    }

    fn patch(self, entry: &HistoryEntry) -> &Patch {
        match self {
            Direction::Undo => &entry.inverse,
            Direction::Redo => &entry.forward,
        }
    }
}

// ------------------------------------------------------------- positions

/// The cursor's position: how many entries are applied (0 = at the base).
/// A cursor that is neither the base nor an entry is `Internal`.
pub fn cursor_position(history: &History) -> Result<usize, CoreError> {
    position_of(history, history.cursor).ok_or_else(|| {
        CoreError::internal(format!(
            "the history cursor {} is neither the base ({}) nor an entry",
            history.cursor, history.base_seq
        ))
    })
}

/// The position of `seq`: 0 for the base, `i + 1` for `entries[i]`.
pub fn position_of(history: &History, seq: u64) -> Option<usize> {
    if seq == history.base_seq {
        return Some(0);
    }
    history
        .entries
        .iter()
        .position(|e| e.seq == seq)
        .map(|i| i + 1)
}

/// The seq of position `pos` (the base seq for 0).
pub fn seq_at(history: &History, pos: usize) -> u64 {
    match pos {
        0 => history.base_seq,
        p => history.entries[p - 1].seq,
    }
}

// --------------------------------------------------------------- patches

/// Applies a patch to `view::project_value(project)`, deserializes and
/// validates the result, and re-derives take durations when the canvas fps
/// changed. A failing `test` op means the state drifted: `Invalid`.
pub fn apply_patch(project: &Project, patch: &Patch) -> Result<Project, CoreError> {
    apply_patch_with(project, patch, validated)
}

/// [`apply_patch`] with another check.
pub fn apply_patch_with(
    project: &Project,
    patch: &Patch,
    check: Check,
) -> Result<Project, CoreError> {
    let mut value = project_value(project);
    json_patch::patch(&mut value, patch).map_err(|e| {
        CoreError::invalid(format!("the patch doesn't apply: {}", patch_failure(&e)))
            .with_details(json!({ "path": e.path.as_str() }))
    })?;
    finish(project, value, check)
}

/// The project the base snapshot `base` becomes after `entries` are redone
/// in order (rebuilding a project from `history.base.json`, A§7.7).
pub fn replay(
    base: &Project,
    entries: &[HistoryEntry],
    check: Check,
) -> Result<Project, CoreError> {
    let mut value = project_value(base);
    for entry in entries {
        step(&mut value, entry, Direction::Redo)?;
    }
    match entries.last() {
        None => Ok(base.clone()),
        Some(last) => finish(base, value, check).map_err(|e| step_failed(last, Direction::Redo, e)),
    }
}

/// Deserializes a patched value, re-derives durations after an fps change
/// and runs `check`.
fn finish(project: &Project, value: Value, check: Check) -> Result<Project, CoreError> {
    let mut next = project_from_value(value)?;
    if next.canvas.fps != project.canvas.fps {
        rederive_durations(&mut next);
    }
    check(&next)?;
    Ok(next)
}

/// Applies one entry's patch to `value`.
fn step(value: &mut Value, entry: &HistoryEntry, dir: Direction) -> Result<(), CoreError> {
    json_patch::patch(value, dir.patch(entry)).map_err(|e| drifted(entry, dir, &e))
}

fn patch_failure(e: &PatchError) -> String {
    let path = match e.path.as_str() {
        "" => "/",
        path => path,
    };
    format!("{} at {path}", e.kind)
}

/// A `test` op (or a path) didn't match: the project changed outside the history.
fn drifted(entry: &HistoryEntry, dir: Direction, e: &PatchError) -> CoreError {
    CoreError::invalid(format!(
        "can't {} entry {} (\"{}\"): the project no longer matches the history ({})",
        dir.verb(),
        entry.seq,
        entry.label,
        patch_failure(e)
    ))
    .with_hint("qrec.json was changed outside Qrec's history; `qrec history` lists the entries")
    .with_details(json!({ "seq": entry.seq, "label": entry.label, "path": e.path.as_str() }))
}

/// The patched project doesn't parse or doesn't validate.
fn step_failed(entry: &HistoryEntry, dir: Direction, e: CoreError) -> CoreError {
    let details = json!({ "seq": entry.seq, "label": entry.label, "cause": e });
    let mut err = CoreError::invalid(format!(
        "can't {} entry {} (\"{}\"): {}",
        dir.verb(),
        entry.seq,
        entry.label,
        e.message
    ))
    .with_details(details);
    err.hint = e.hint;
    err
}

// ------------------------------------------------------- undo, redo, restore

/// Undoes `steps` entries (by position), all or nothing, and validates the
/// result. Returns the new state; its `history.cursor` is the new cursor.
/// A failing step is `Invalid` naming its seq.
pub fn undo(state: &State, steps: u32) -> Result<State, CoreError> {
    undo_with(state, steps, validated)
}

/// [`undo`] with another check.
pub fn undo_with(state: &State, steps: u32, check: Check) -> Result<State, CoreError> {
    let pos = cursor_position(&state.history)?;
    let steps = usize::try_from(steps).unwrap_or(usize::MAX);
    if steps == 0 {
        return Err(CoreError::usage("undo needs at least 1 step"));
    }
    if pos == 0 {
        return Err(CoreError::invalid("nothing to undo")
            .with_hint("`qrec history` lists the entries; the cursor is at the oldest one"));
    }
    if steps > pos {
        return Err(
            CoreError::invalid(format!("can't undo {steps} steps: only {pos} to undo"))
                .with_details(json!({ "available": pos })),
        );
    }
    move_to(state, pos - steps, check)
}

/// Redoes `steps` entries, all or nothing (like [`undo`]).
pub fn redo(state: &State, steps: u32) -> Result<State, CoreError> {
    redo_with(state, steps, validated)
}

/// [`redo`] with another check.
pub fn redo_with(state: &State, steps: u32, check: Check) -> Result<State, CoreError> {
    let pos = cursor_position(&state.history)?;
    let available = state.history.entries.len() - pos;
    let steps = usize::try_from(steps).unwrap_or(usize::MAX);
    if steps == 0 {
        return Err(CoreError::usage("redo needs at least 1 step"));
    }
    if available == 0 {
        return Err(
            CoreError::invalid("nothing to redo").with_hint("the cursor is at the newest entry")
        );
    }
    if steps > available {
        return Err(CoreError::invalid(format!(
            "can't redo {steps} steps: only {available} to redo"
        ))
        .with_details(json!({ "available": available })));
    }
    move_to(state, pos + steps, check)
}

/// Undoes or redoes to `seq` (`base_seq` or an existing seq, else `NotFound`).
pub fn restore(state: &State, seq: u64) -> Result<State, CoreError> {
    restore_with(state, seq, validated)
}

/// [`restore`] with another check.
pub fn restore_with(state: &State, seq: u64, check: Check) -> Result<State, CoreError> {
    let target =
        position_of(&state.history, seq).ok_or_else(|| unknown_seq(&state.history, seq))?;
    move_to(state, target, check)
}

/// `NotFound` for a seq that is neither the base nor an entry.
pub fn unknown_seq(history: &History, seq: u64) -> CoreError {
    let newest = history.entries.last().map_or(history.base_seq, |e| e.seq);
    CoreError::not_found(format!("no history entry {seq}")).with_hint(format!(
        "`qrec history` lists the entries; restorable seqs go from {} (the oldest state) to {newest}",
        history.base_seq
    ))
}

/// The state with the cursor at position `target`: the entries between
/// the cursor and `target` are undone or redone in order, all or nothing.
pub fn move_to(state: &State, target: usize, check: Check) -> Result<State, CoreError> {
    let history = &state.history;
    let pos = cursor_position(history)?;
    if target > history.entries.len() {
        return Err(CoreError::internal(format!(
            "history position {target} is past the newest entry"
        )));
    }
    let (dir, entries): (Direction, Vec<&HistoryEntry>) = if target < pos {
        (
            Direction::Undo,
            history.entries[target..pos].iter().rev().collect(),
        )
    } else {
        (
            Direction::Redo,
            history.entries[pos..target].iter().collect(),
        )
    };
    let mut next = state.clone();
    next.history.cursor = seq_at(history, target);
    let Some(last) = entries.last() else {
        return Ok(next);
    };
    let mut value = project_value(&state.project);
    for entry in &entries {
        step(&mut value, entry, dir)?;
    }
    next.project = finish(&state.project, value, check).map_err(|e| step_failed(last, dir, e))?;
    Ok(next)
}

// ------------------------------------------------------------ compaction

/// Above 5,000 entries: drops the oldest down to 4,000 and returns the new
/// history with the `Rewrite` op (new base = the project before the oldest
/// kept entry). `None` when no compaction is due.
pub fn compact(history: &History, current: &Project) -> Option<(History, HistoryOp)> {
    compact_to(history, current, COMPACT_ABOVE, COMPACT_TO)
}

/// [`compact`] with other thresholds: above `above` entries keep the newest
/// `keep`. The base is found by walking inverses back from `current` (the
/// state at the cursor); entries after the cursor are never dropped, and an
/// entry whose inverse no longer applies is dropped with everything older
/// (it could not be undone anyway).
pub fn compact_to(
    history: &History,
    current: &Project,
    above: usize,
    keep: usize,
) -> Option<(History, HistoryOp)> {
    let len = history.entries.len();
    if len <= above {
        return None;
    }
    let pos = cursor_position(history).ok()?;
    let mut dropped = len.saturating_sub(keep).min(pos);
    if dropped == 0 {
        return None;
    }
    let mut value = project_value(current);
    for (i, entry) in history.entries[..pos]
        .iter()
        .enumerate()
        .skip(dropped)
        .rev()
    {
        if json_patch::patch(&mut value, &entry.inverse).is_err() {
            dropped = i + 1;
            break;
        }
    }
    let mut base = project_from_value(value).ok()?;
    if base.canvas.fps != current.canvas.fps {
        rederive_durations(&mut base);
    }
    let base_seq = history.entries[dropped - 1].seq;
    let entries = history.entries[dropped..].to_vec();
    Some(rewritten(history, base, base_seq, entries, history.cursor))
}

/// A history with these entries and base, and the `Rewrite` op that
/// persists it. `seen_clip_ids` is rebuilt; `next_seq` never goes back.
pub fn rewritten(
    old: &History,
    base: Project,
    base_seq: u64,
    entries: Vec<HistoryEntry>,
    cursor: u64,
) -> (History, HistoryOp) {
    let mut history = History {
        entries: Arc::new(entries.clone()),
        cursor,
        base_seq,
        next_seq: old.next_seq,
        seen_clip_ids: Arc::default(),
    };
    history.rebuild_seen_clip_ids(&base);
    let op = HistoryOp::Rewrite {
        entries,
        base: Box::new(base),
        base_seq,
        cursor,
        next_seq: old.next_seq,
    };
    (history, op)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::ErrorCode;
    use crate::ctx::Ctx;
    use crate::history::commit;
    use crate::history::view::undoable_view;
    use crate::model::{Fps, Stream};
    use crate::testkit::{self, ProjectBuilder};

    fn ctx() -> Ctx {
        testkit::ctx("2026-10-14T12:30:00Z", 1)
    }

    fn start() -> State {
        testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-1")
                .mic("A1", "alsa_input.mic")
                .take("take-0001", 300)
                .clip("c001", "take-0001", 0, 100)
                .clip("c002", "take-0001", 150, 300)
                .build(),
        )
    }

    /// Commits `edit` applied to a copy of `state`.
    fn edit(state: &State, label: &str, edit: impl FnOnce(&mut Project)) -> State {
        let mut next = state.clone();
        edit(&mut next.project);
        let c = commit(state, &ctx(), label.into(), Value::Null, next);
        assert!(c.seq.is_some(), "{label} changed nothing");
        c.state
    }

    /// Three entries: trim c002, rename, remove c001.
    fn three_edits() -> Vec<State> {
        let s0 = start();
        let s1 = edit(&s0, "Trim c002", |p| p.timeline.clips[1].in_ = 160);
        let s2 = edit(&s1, "Rename", |p| p.name = "talk".into());
        let s3 = edit(&s2, "Remove c001", |p| {
            p.timeline.clips.remove(0);
        });
        vec![s0, s1, s2, s3]
    }

    #[test]
    fn undo_and_redo_walk_the_states() {
        let states = three_edits();
        let last = states[3].clone();
        assert_eq!(last.history.cursor, 3);

        let one = undo_with(&last, 1, unchecked).unwrap();
        assert_eq!(one.project, states[2].project);
        assert_eq!(one.history.cursor, 2);
        assert_eq!(
            one.history.entries, last.history.entries,
            "undo keeps the entries"
        );

        let all = undo_with(&last, 3, unchecked).unwrap();
        assert_eq!(all.project, states[0].project);
        assert_eq!(all.history.cursor, 0);

        let again = redo_with(&all, 2, unchecked).unwrap();
        assert_eq!(again.project, states[2].project);
        assert_eq!(again.history.cursor, 2);
        let back = redo_with(&again, 1, unchecked).unwrap();
        assert_eq!(back.project, last.project);
        assert_eq!(back.history, last.history);

        for (seq, state) in states.iter().enumerate() {
            let restored = restore_with(&last, seq as u64, unchecked).unwrap();
            assert_eq!(restored.project, state.project, "restore {seq}");
            assert_eq!(restored.history.cursor, seq as u64);
        }
    }

    #[test]
    fn undo_and_redo_are_all_or_nothing() {
        let last = three_edits().pop().unwrap();
        let err = undo_with(&last, 4, unchecked).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(err.message, "can't undo 4 steps: only 3 to undo");
        let err = redo_with(&last, 1, unchecked).unwrap_err();
        assert_eq!(err.message, "nothing to redo");
        let bottom = undo_with(&last, 3, unchecked).unwrap();
        assert_eq!(
            undo_with(&bottom, 1, unchecked).unwrap_err().message,
            "nothing to undo"
        );
        assert_eq!(
            redo_with(&bottom, 5, unchecked).unwrap_err().message,
            "can't redo 5 steps: only 3 to redo"
        );
        assert_eq!(
            undo_with(&last, 0, unchecked).unwrap_err().code,
            ErrorCode::Usage
        );
        assert_eq!(
            redo_with(&bottom, 0, unchecked).unwrap_err().code,
            ErrorCode::Usage
        );
        let err = restore_with(&last, 9, unchecked).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.message, "no history entry 9");
    }

    #[test]
    fn a_drifted_state_is_refused_naming_the_seq() {
        let mut last = three_edits().pop().unwrap();
        // Someone changed c002's start behind the history's back.
        last.project.timeline.clips[0].in_ = 170;
        // Undoing "Remove c001" still works (it doesn't touch c002)…
        let one = undo_with(&last, 1, unchecked).unwrap();
        // …but undoing the trim of c002 needs c002 to start at 160.
        let err = undo_with(&one, 2, unchecked).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert!(
            err.message.starts_with(
                "can't undo entry 1 (\"Trim c002\"): the project no longer matches the history"
            ),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("/timeline/clips/1/in"),
            "{}",
            err.message
        );
        assert_eq!(err.details.as_ref().unwrap()["seq"], 1);
        // Nothing changed: the whole multi-step undo is refused.
        let err = undo_with(&last, 3, unchecked).unwrap_err();
        assert!(err.message.contains("entry 1"), "{}", err.message);
    }

    #[test]
    fn the_check_runs_on_the_result() {
        let last = three_edits().pop().unwrap();
        fn no_c001(p: &Project) -> Result<(), CoreError> {
            match p.timeline.clips.iter().any(|c| c.id.as_str() == "c001") {
                true => Err(CoreError::invalid("c001 is not allowed").with_hint("remove it")),
                false => Ok(()),
            }
        }
        let err = undo_with(&last, 1, no_c001).unwrap_err();
        assert_eq!(
            err.message,
            "can't undo entry 3 (\"Remove c001\"): c001 is not allowed"
        );
        assert_eq!(err.hint.as_deref(), Some("remove it"));
        assert_eq!(
            err.details.as_ref().unwrap()["cause"]["message"],
            "c001 is not allowed"
        );
        let bottom = undo_with(&last, 3, unchecked).unwrap();
        // Redo 3 ends without c001: fine, even though intermediate states had it.
        assert!(redo_with(&bottom, 3, no_c001).is_ok());
    }

    #[test]
    fn an_fps_change_rederives_durations_both_ways() {
        let s0 = start();
        let s1 = edit(&s0, "Canvas at 60 fps", |p| {
            p.canvas.fps = Fps::new(60, 1);
            rederive_durations(p);
            for clip in &mut p.timeline.clips {
                clip.in_ *= 2;
                clip.out *= 2;
            }
        });
        assert_eq!(
            s1.project.takes.values().next().unwrap().duration_frames,
            600
        );
        let undone = undo_with(&s1, 1, unchecked).unwrap();
        assert_eq!(undone.project, s0.project, "durations back at 30 fps");
        let redone = redo_with(&undone, 1, unchecked).unwrap();
        assert_eq!(redone.project, s1.project);
        // A single patch applied by hand does the same.
        let entry = &s1.history.entries[0];
        assert_eq!(
            apply_patch_with(&s1.project, &entry.inverse, unchecked).unwrap(),
            s0.project
        );
    }

    #[test]
    fn facts_survive_undo() {
        let s0 = start();
        let s1 = edit(&s0, "Trim", |p| p.timeline.clips[0].out = 90);
        // A take recorded after the entry (facts are persisted, not undoable).
        let mut s2 = s1.clone();
        let mut take = s2.project.takes.values().next().unwrap().clone();
        take.streams
            .iter_mut()
            .for_each(|s: &mut Stream| s.file = s.file.replace("take-0001", "take-0002"));
        s2.project.takes.insert("take-0002".parse().unwrap(), take);
        s2.project.next_take_number = 3;
        let undone = undo_with(&s2, 1, unchecked).unwrap();
        assert_eq!(undone.project.takes, s2.project.takes, "takes stay");
        assert_eq!(undone.project.next_take_number, 3);
        assert_eq!(undone.project.timeline, s0.project.timeline);
    }

    #[test]
    fn replay_rebuilds_from_the_base() {
        let states = three_edits();
        let last = &states[3];
        let rebuilt = replay(&states[0].project, &last.history.entries, unchecked).unwrap();
        assert_eq!(rebuilt, last.project);
        assert_eq!(
            replay(&states[0].project, &[], unchecked).unwrap(),
            states[0].project
        );
        // Replaying onto the wrong base fails at the first entry that doesn't fit.
        let err = replay(&last.project, &last.history.entries, unchecked).unwrap_err();
        assert!(
            err.message.starts_with("can't redo entry 1"),
            "{}",
            err.message
        );
    }

    #[test]
    fn apply_patch_reports_bad_patches() {
        let s = start();
        let patch: Patch = serde_json::from_value(json!([
            {"op": "test", "path": "/name", "value": "other"},
            {"op": "replace", "path": "/name", "value": "x"}
        ]))
        .unwrap();
        let err = apply_patch_with(&s.project, &patch, unchecked).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(
            err.message,
            "the patch doesn't apply: value did not match at /name"
        );
        let patch: Patch = serde_json::from_value(
            json!([{"op": "replace", "path": "/canvas/fps", "value": "fast"}]),
        )
        .unwrap();
        let err = apply_patch_with(&s.project, &patch, unchecked).unwrap_err();
        assert!(
            err.message.starts_with("the patched project doesn't parse"),
            "{}",
            err.message
        );
    }

    /// `n` renames on top of [`start`].
    fn renames(n: usize) -> State {
        let mut state = start();
        for i in 0..n {
            state = edit(&state, &format!("Rename {i}"), |p| {
                p.name = format!("name {i}")
            });
        }
        state
    }

    #[test]
    fn compaction_keeps_the_newest_entries() {
        let state = renames(12);
        assert!(
            compact_to(&state.history, &state.project, 12, 8).is_none(),
            "not above the limit"
        );
        let (history, op) = compact_to(&state.history, &state.project, 10, 8).unwrap();
        let seqs: Vec<u64> = history.entries.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, (5..=12).collect::<Vec<_>>());
        assert_eq!(
            (history.base_seq, history.cursor, history.next_seq),
            (4, 12, 13)
        );
        let HistoryOp::Rewrite {
            entries,
            base,
            base_seq,
            cursor,
            next_seq,
        } = op
        else {
            panic!("compaction rewrites")
        };
        assert_eq!(entries, *history.entries);
        assert_eq!((base_seq, cursor, next_seq), (4, 12, 13));
        assert_eq!(
            base.name, "name 3",
            "the state after the last dropped entry"
        );
        // The current state is still reachable from the new base, and undo
        // goes back exactly to it.
        assert_eq!(replay(&base, &entries, unchecked).unwrap(), state.project);
        let mut compacted = state.clone();
        compacted.history = history;
        let bottom = undo_with(&compacted, 8, unchecked).unwrap();
        assert_eq!(bottom.project, *base);
        assert_eq!(bottom.history.cursor, 4);
        assert_eq!(
            undo_with(&bottom, 1, unchecked).unwrap_err().message,
            "nothing to undo"
        );
    }

    #[test]
    fn compaction_never_drops_past_the_cursor() {
        let state = renames(12);
        let undone = undo_with(&state, 10, unchecked).unwrap();
        let (history, _) = compact_to(&undone.history, &undone.project, 10, 8).unwrap();
        assert_eq!(
            history.base_seq, 2,
            "only the 2 entries before the cursor go"
        );
        assert_eq!(history.cursor, 2);
        assert_eq!(history.entries.len(), 10);
        let mut compacted = undone.clone();
        compacted.history = history;
        assert_eq!(
            redo_with(&compacted, 10, unchecked).unwrap().project,
            state.project
        );
        // At the very bottom nothing can be dropped.
        let bottom = undo_with(&state, 12, unchecked).unwrap();
        assert!(compact_to(&bottom.history, &bottom.project, 10, 8).is_none());
    }

    #[test]
    fn compaction_drops_entries_that_no_longer_undo() {
        let mut state = renames(12);
        // Entry 6's inverse expects "name 5" before it: break it.
        let entries = Arc::make_mut(&mut state.history.entries);
        if let json_patch::PatchOperation::Test(t) = &mut entries[5].inverse.0[0] {
            t.value = json!("tampered");
        }
        let (history, op) = compact_to(&state.history, &state.project, 10, 10).unwrap();
        assert_eq!(
            history.base_seq, 6,
            "entries up to the broken one are dropped"
        );
        assert_eq!(history.entries.len(), 6);
        let HistoryOp::Rewrite { base, .. } = op else {
            panic!()
        };
        assert_eq!(base.name, "name 5");
    }

    #[test]
    fn compaction_rederives_durations_of_the_base() {
        let mut state = edit(&start(), "Rename", |p| p.name = "first".into());
        state = edit(&state, "60 fps", |p| {
            p.canvas.fps = Fps::new(60, 1);
            rederive_durations(p);
        });
        for i in 0..3 {
            state = edit(&state, "Rename", |p| p.name = format!("n{i}"));
        }
        assert_eq!(
            state.project.takes.values().next().unwrap().duration_frames,
            600
        );
        // Drops the first rename: the base is before the fps change.
        let (_, op) = compact_to(&state.history, &state.project, 4, 4).unwrap();
        let HistoryOp::Rewrite { base, base_seq, .. } = op else {
            panic!()
        };
        assert_eq!(base_seq, 1);
        assert_eq!(base.canvas.fps, Fps::new(30, 1));
        assert_eq!(base.takes.values().next().unwrap().duration_frames, 300);
    }

    #[test]
    fn rewritten_histories_rebuild_seen_ids() {
        let states = three_edits();
        let last = &states[3];
        assert!(
            last.history
                .seen_clip_ids
                .iter()
                .any(|id| id.as_str() == "c001")
        );
        let (history, _) = rewritten(&last.history, last.project.clone(), 3, Vec::new(), 3);
        let seen: Vec<&str> = history.seen_clip_ids.iter().map(|id| id.as_str()).collect();
        assert_eq!(seen, ["c002"], "only the base's clips are left");
        assert_eq!(undoable_view(&last.project)["name"], "talk");
    }
}
