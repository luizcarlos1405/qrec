//! The commit helper (A§5.1, foundation): every handler builds the next
//! `State` and finishes through [`commit`] (undoable edits) or [`persist`]
//! (facts and UI). Handlers never emit `WriteProject` or `History` by hand.
//!
//! **Facts first:** a step that changes facts *and* makes an edit (take
//! finalization, recovery) first applies the facts with `persist`, then the
//! edit with `commit`, so a patch never contains facts.

use serde_json::Value;

use crate::ctx::Ctx;
use crate::hash::json_hash;
use crate::history::{HistoryEntry, diff};
use crate::protocol::effect::{Effect, HistoryOp};
use crate::state::State;
use crate::ui_normalize::ui_normalize;

/// The result of [`commit`] or [`persist`].
#[derive(Debug, Clone, PartialEq)]
pub struct Committed {
    pub state: State,
    /// `History(Append)` (if an entry was appended), then `WriteProject` /
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
    let mut effects = vec![
        Effect::History(HistoryOp::Append(entry)),
        Effect::WriteProject,
    ];
    if next.ui != prev.ui {
        effects.push(Effect::WriteUi);
    }
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

fn writes(prev: &State, next: &State) -> Vec<Effect> {
    let mut effects = Vec::new();
    if next.project != prev.project {
        effects.push(Effect::WriteProject);
    }
    if next.ui != prev.ui {
        effects.push(Effect::WriteUi);
    }
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
        assert!(
            c.state
                .history
                .seen_clip_ids
                .iter()
                .any(|id| id.as_str() == "c002")
        );

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
}
