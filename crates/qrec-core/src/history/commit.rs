//! The commit helper (A§5.1, foundation): every handler builds the next
//! `State` and finishes through [`commit`] (undoable edits) or [`persist`]
//! (facts and UI). Handlers never emit `WriteProject` or `History` by hand.
//!
//! **Facts first:** a step that changes facts *and* makes an edit (take
//! finalization, recovery) first applies the facts with `persist`, then the
//! edit with `commit`, so a patch never contains facts.

use serde_json::Value;

use crate::apply::ui::live_effect;
use crate::ctx::Ctx;
use crate::hash::json_hash;
use crate::history::{HistoryEntry, diff, ops};
use crate::protocol::effect::{Effect, HistoryOp};
use crate::state::State;
use crate::ui_normalize::ui_normalize;

/// The result of [`commit`] or [`persist`].
#[derive(Debug, Clone, PartialEq)]
pub struct Committed {
    pub state: State,
    /// `History(Append)` (if an entry was appended; `History(Rewrite)` when
    /// the append also compacted the history), then `WriteProject` /
    /// `WriteUi` for what changed, in the A§7.6 order.
    pub effects: Vec<Effect>,
    /// The new entry's seq; `None` when nothing undoable changed.
    pub seq: Option<u64>,
}

/// Finishes an undoable edit: normalizes the UI, diffs the undoable views of
/// `prev` and `next` and, if they differ, appends a history entry
/// (`seq = next_seq`) and emits `History(Append)` + `WriteProject` (+
/// `WriteUi` when the UI changed). Equal views append nothing and behave
/// like [`persist`].
///
/// When the append takes the history above 5,000 entries it is compacted
/// (A§7.3) and the one history effect is the `Rewrite` (which holds the
/// new entry) instead of the `Append`.
///
/// `cmd` is the entry's `cmd`: `Command::to_json()`, `{"event":…}` or
/// `{"external":true}`.
pub fn commit(prev: &State, ctx: &Ctx, label: String, cmd: Value, mut next: State) -> Committed {
    next.ui = ui_normalize(&prev.project, &next.project, &next.ui);
    let (before, after) = diff::aligned_views(&prev.project, &next.project);
    if before == after {
        let effects = writes(prev, &next);
        return Committed {
            state: next,
            effects,
            seq: None,
        };
    }
    let seq = next.history.next_seq;
    let entry = HistoryEntry {
        seq,
        ts: ctx.now,
        label,
        cmd,
        forward: diff::diff(&before, &after),
        inverse: diff::diff(&after, &before),
        after_hash: json_hash(&after),
    };
    next.history.append(entry.clone());
    let history_op = match ops::compact(&next.history, &next.project) {
        Some((compacted, rewrite)) => {
            next.history = compacted;
            rewrite
        }
        None => HistoryOp::Append(entry),
    };
    let mut effects = vec![Effect::History(history_op), Effect::WriteProject];
    if next.ui != prev.ui {
        effects.push(Effect::WriteUi);
    }
    effects.extend(live_effect(prev, &next));
    Committed {
        state: next,
        effects,
        seq: Some(seq),
    }
}

/// Finishes a non-undoable change (facts, UI): normalizes the UI and emits
/// `WriteProject` / `WriteUi` for whatever changed. No history entry.
pub fn persist(prev: &State, mut next: State) -> Committed {
    next.ui = ui_normalize(&prev.project, &next.project, &next.ui);
    let effects = writes(prev, &next);
    Committed {
        state: next,
        effects,
        seq: None,
    }
}

/// The writes for what changed, then the `SetLive` the change implies.
/// `SetLive` is emitted here and only here, so the live pipeline follows
/// scene edits, undo/redo, canvas-mode switches and record phases alike;
/// handlers add `StartCapture`/`StopCapture` after it.
fn writes(prev: &State, next: &State) -> Vec<Effect> {
    let mut effects = Vec::new();
    if next.project != prev.project {
        effects.push(Effect::WriteProject);
    }
    if next.ui != prev.ui {
        effects.push(Effect::WriteUi);
    }
    effects.extend(live_effect(prev, next));
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::view::undoable_view;
    use crate::model::TakeStatus;
    use crate::testkit::{self, ProjectBuilder};

    fn two_clips() -> State {
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

    #[test]
    fn an_edit_appends_an_entry() {
        let prev = two_clips();
        let ctx = testkit::ctx("2026-10-14T12:30:00Z", 1);
        let mut next = prev.clone();
        next.project.timeline.clips[1].in_ = 160;
        let cmd = serde_json::json!({"cmd": "clipTrim"});
        let c = commit(&prev, &ctx, "Trim clip c002".into(), cmd.clone(), next);

        assert_eq!(c.seq, Some(1));
        assert_eq!(c.state.history.cursor, 1);
        assert_eq!(c.state.history.next_seq, 2);
        assert_eq!(c.state.history.entries.len(), 1);
        let entry = &c.state.history.entries[0];
        assert_eq!(entry.seq, prev.history.next_seq);
        assert_eq!(entry.label, "Trim clip c002");
        assert_eq!(entry.cmd, cmd);
        assert_eq!(entry.ts, ctx.now);
        assert_eq!(
            c.effects,
            vec![
                Effect::History(HistoryOp::Append(entry.clone())),
                Effect::WriteProject
            ]
        );
        // A trim patches fields only; the clip is still on the timeline.
        assert!(c.state.history.seen_clip_ids.is_empty());

        // The patches move between the two views.
        let mut doc = undoable_view(&prev.project);
        json_patch::patch(&mut doc, &entry.forward).unwrap();
        assert_eq!(doc, undoable_view(&c.state.project));
        assert_eq!(entry.after_hash, json_hash(&doc));
        json_patch::patch(&mut doc, &entry.inverse).unwrap();
        assert_eq!(doc, undoable_view(&prev.project));

        // A second edit gets the next seq.
        let mut next = c.state.clone();
        next.project.timeline.clips.remove(0);
        let c2 = commit(&c.state, &ctx, "Remove clip c001".into(), Value::Null, next);
        assert_eq!(c2.seq, Some(2));
        assert_eq!(c2.state.history.entries.len(), 2);
        // A removed clip could come back with undo: its id is never reused.
        let seen: Vec<&str> = c2
            .state
            .history
            .seen_clip_ids
            .iter()
            .map(|id| id.as_str())
            .collect();
        assert_eq!(seen, ["c001"]);
    }

    #[test]
    fn an_unchanged_state_appends_nothing() {
        let prev = two_clips();
        let ctx = testkit::ctx("2026-10-14T12:30:00Z", 1);
        let c = commit(&prev, &ctx, "Nothing".into(), Value::Null, prev.clone());
        assert_eq!(c.seq, None);
        assert!(c.effects.is_empty());
        assert_eq!(c.state, prev);
    }

    #[test]
    fn facts_and_ui_are_persisted_without_an_entry() {
        let prev = two_clips();
        let ctx = testkit::ctx("2026-10-14T12:30:00Z", 1);
        let mut next = prev.clone();
        let take = next.project.takes.values_mut().next().unwrap();
        take.status = TakeStatus::Broken;
        next.ui.bin_open = true;
        let c = commit(&prev, &ctx, "Facts".into(), Value::Null, next.clone());
        assert_eq!(c.seq, None, "takes' facts are not undoable");
        assert_eq!(c.effects, vec![Effect::WriteProject, Effect::WriteUi]);
        assert_eq!(
            persist(&prev, next).effects,
            vec![Effect::WriteProject, Effect::WriteUi]
        );
    }

    #[test]
    fn appending_drops_the_redo_tail() {
        let prev = two_clips();
        let ctx = testkit::ctx("2026-10-14T12:30:00Z", 1);
        let mut state = prev.clone();
        for out in [90, 80, 70] {
            let mut next = state.clone();
            next.project.timeline.clips[0].out = out;
            state = commit(&state, &ctx, format!("Trim to {out}"), Value::Null, next).state;
        }
        assert_eq!(state.history.entries.len(), 3);
        // Pretend two steps were undone (the history owner's ops do this for real).
        state.history.cursor = 1;
        let mut next = state.clone();
        next.project.timeline.clips[0].out = 60;
        let c = commit(&state, &ctx, "Trim to 60".into(), Value::Null, next);
        assert_eq!(c.seq, Some(4), "seqs are never reused");
        let seqs: Vec<u64> = c.state.history.entries.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 4]);
        assert_eq!(c.state.history.cursor, 4);
    }

    #[test]
    fn patches_are_guarded_by_tests() {
        let prev = two_clips();
        let ctx = testkit::ctx("2026-10-14T12:30:00Z", 1);
        let mut next = prev.clone();
        next.project.timeline.clips.remove(0);
        next.project.timeline.clips[0].out = 290;
        let c = commit(&prev, &ctx, "Edit".into(), Value::Null, next);
        let entry = &c.state.history.entries[0];
        let ops: Vec<String> = entry.forward.0.iter().map(|op| op.to_string()).collect();
        assert_eq!(
            ops,
            [
                r#"{"op":"test","path":"/timeline/clips/0","value":{"id":"c001","in":0,"out":100,"take":"take-0001"}}"#,
                r#"{"op":"remove","path":"/timeline/clips/0"}"#,
                r#"{"op":"test","path":"/timeline/clips/0/out","value":300}"#,
                r#"{"op":"replace","path":"/timeline/clips/0/out","value":290}"#,
            ]
        );
    }

    /// 5,000 renames, entry `i` turning `n{i-1}` into `n{i}`.
    fn long_history() -> State {
        use json_patch::Patch;
        let mut state = two_clips();
        let ts = state.project.created_at;
        let mut name = "n0".to_owned();
        state.project.name = name.clone();
        let rename = |from: &str, to: &str| -> Patch {
            serde_json::from_value(serde_json::json!([
                {"op": "test", "path": "/name", "value": from},
                {"op": "replace", "path": "/name", "value": to}
            ]))
            .unwrap()
        };
        let entries = std::sync::Arc::make_mut(&mut state.history.entries);
        for seq in 1..=5_000u64 {
            let new = format!("n{seq}");
            entries.push(HistoryEntry {
                seq,
                ts,
                label: format!("Rename {seq}"),
                cmd: Value::Null,
                forward: rename(&name, &new),
                inverse: rename(&new, &name),
                after_hash: String::new(),
            });
            name = new;
        }
        state.history.cursor = 5_000;
        state.history.next_seq = 5_001;
        state.project.name = name;
        state
    }

    #[test]
    fn appending_past_the_limit_compacts() {
        let state = long_history();
        let ctx = testkit::ctx("2026-10-14T12:30:00Z", 1);
        let mut next = state.clone();
        next.project.name = "final".into();
        let c = commit(&state, &ctx, "Rename final".into(), Value::Null, next);
        assert_eq!(c.seq, Some(5_001));
        let h = &c.state.history;
        assert_eq!(h.entries.len(), 4_000);
        assert_eq!((h.base_seq, h.cursor, h.next_seq), (1_001, 5_001, 5_002));
        assert_eq!(h.entries.last().unwrap().label, "Rename final");
        match c.effects.as_slice() {
            [
                Effect::History(HistoryOp::Rewrite {
                    entries,
                    base,
                    base_seq,
                    cursor,
                    next_seq,
                }),
                Effect::WriteProject,
            ] => {
                assert_eq!(entries, &*h.entries);
                assert_eq!(base.name, "n1001", "the state before the oldest kept entry");
                assert_eq!((*base_seq, *cursor, *next_seq), (1_001, 5_001, 5_002));
            }
            other => panic!("expected a rewrite: {other:?}"),
        }
        // One more entry appends normally: 4,001 is below the limit.
        let mut next = c.state.clone();
        next.project.name = "again".into();
        let c2 = commit(&c.state, &ctx, "Rename again".into(), Value::Null, next);
        assert!(matches!(
            c2.effects[0],
            Effect::History(HistoryOp::Append(_))
        ));
    }
}
