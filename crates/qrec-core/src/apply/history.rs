//! History commands and take purge (A§7, A§5.6). Owner: history.
//!
//! Undo/redo/restore emit `History(SetCursor)` + `WriteProject` (+
//! `WriteUi`); clear emits `History(Rewrite)` + `WriteProject`; purge emits
//! `History(Rewrite)`, `WriteProject`, then `DeleteTakeDirs` (A§5.6) and
//! replies when `TakeDirsDeleted` arrives (`on_event::disk`).
//!
//! Projects reached through the history are validated (A§7.2); the
//! `*_checked` variants take the check as an argument.

use std::collections::BTreeSet;

use serde_json::json;

use crate::apply::Outcome;
use crate::ctx::{Ctx, FileFact};
use crate::error::CoreError;
use crate::history::ops::{self, Check};
use crate::history::{HistoryEntry, persist, purge};
use crate::ids::TakeId;
use crate::model::{Project, Take, TakeStatus};
use crate::protocol::effect::{Effect, HistoryOp};
use crate::protocol::reply::CmdResult;
use crate::state::{History, Job, JobKind, State};

/// `qrec undo [N]`: all or nothing; `CmdResult.seq` is the new cursor.
pub fn undo(state: &State, ctx: &Ctx, steps: u32) -> Result<Outcome, CoreError> {
    undo_checked(state, ctx, steps, ops::validated)
}

/// [`undo`] with another check of the resulting project.
pub fn undo_checked(
    state: &State,
    _ctx: &Ctx,
    steps: u32,
    check: Check,
) -> Result<Outcome, CoreError> {
    let next = ops::undo_with(state, steps, check)?;
    let undone = between(&state.history, next.history.cursor, state.history.cursor);
    let message = match undone.as_slice() {
        [one] => format!("Undid \"{}\"", one.label),
        [oldest, ..] => format!(
            "Undid {} steps (back to before \"{}\")",
            undone.len(),
            oldest.label
        ),
        [] => "Nothing undone".to_owned(),
    };
    Ok(moved(state, next, message))
}

/// `qrec redo [N]`.
pub fn redo(state: &State, ctx: &Ctx, steps: u32) -> Result<Outcome, CoreError> {
    redo_checked(state, ctx, steps, ops::validated)
}

/// [`redo`] with another check of the resulting project.
pub fn redo_checked(
    state: &State,
    _ctx: &Ctx,
    steps: u32,
    check: Check,
) -> Result<Outcome, CoreError> {
    let next = ops::redo_with(state, steps, check)?;
    let redone = between(&state.history, state.history.cursor, next.history.cursor);
    let message = match redone.as_slice() {
        [one] => format!("Redid \"{}\"", one.label),
        [.., newest] => format!("Redid {} steps (up to \"{}\")", redone.len(), newest.label),
        [] => "Nothing redone".to_owned(),
    };
    Ok(moved(state, next, message))
}

/// `qrec history restore <seq> --yes`: undo or redo to `seq` (needs
/// confirmation). Always rewrites `qrec.json`, so restoring the current
/// cursor is the way out of an invalid file on disk (A§7.7).
pub fn history_restore(state: &State, ctx: &Ctx, seq: u64) -> Result<Outcome, CoreError> {
    restore_checked(state, ctx, seq, ops::validated)
}

/// [`history_restore`] with another check of the resulting project.
pub fn restore_checked(
    state: &State,
    ctx: &Ctx,
    seq: u64,
    check: Check,
) -> Result<Outcome, CoreError> {
    let history = &state.history;
    let target = ops::position_of(history, seq).ok_or_else(|| ops::unknown_seq(history, seq))?;
    let pos = ops::cursor_position(history)?;
    let what = describe(history, seq);
    if !ctx.confirmed {
        let (undo, redo) = (pos.saturating_sub(target), target.saturating_sub(pos));
        let effect = match (undo, redo) {
            (0, 0) => "qrec.json is rewritten from it".to_owned(),
            (n, 0) => format!("undoes {}", entries(n)),
            (_, n) => format!("redoes {}", entries(n)),
        };
        return Err(
            CoreError::confirmation_required(format!("restore {what}: {effect}")).with_details(
                json!({ "seq": seq, "label": label_of(history, seq), "undo": undo, "redo": redo }),
            ),
        );
    }
    let mut next = ops::restore_with(state, seq, check)?;
    next.runtime.disk_invalid = None;
    Ok(moved(state, next, format!("Restored {what}")))
}

/// `qrec history clear --yes`: no entries, base = current project.
pub fn history_clear(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    let history = &state.history;
    let cursor = history.cursor;
    let lost = history.entries.len();
    if lost == 0 {
        let result = CmdResult::new("The history is already empty").with_seq(Some(cursor));
        return Ok(Outcome::new(state.clone(), Vec::new(), Some(Ok(result))));
    }
    if !ctx.confirmed {
        return Err(CoreError::confirmation_required(format!(
            "clear the undo history ({})",
            entries(lost)
        ))
        .with_details(json!({ "historyEntriesLost": lost })));
    }
    let (cleared, op) = ops::rewritten(history, state.project.clone(), cursor, Vec::new(), cursor);
    let mut next = state.clone();
    next.history = cleared;
    let result =
        CmdResult::new(format!("Cleared the history ({})", entries(lost))).with_seq(Some(cursor));
    Ok(Outcome::new(
        next,
        vec![Effect::History(op), Effect::WriteProject],
        Some(Ok(result)),
    ))
}

/// `qrec take purge <take>… --yes` (A§5.6, A§7.5). Not undoable. Replies
/// when the shell reports the folders deleted (`TakeDirsDeleted`).
pub fn take_purge(state: &State, ctx: &Ctx, takes: Vec<TakeId>) -> Result<Outcome, CoreError> {
    if takes.is_empty() {
        return Err(CoreError::usage("name at least one take to purge")
            .with_hint("`qrec take list --unused` shows the takes in the bin"));
    }
    let mut unique: Vec<TakeId> = Vec::new();
    for take in takes {
        if !unique.contains(&take) {
            unique.push(take);
        }
    }
    let takes = unique;
    for id in &takes {
        purgeable(state, id)?;
    }
    let rewrite = purge::purge_rewrite(state, &takes)?;
    let files: Vec<&FileFact> = takes
        .iter()
        .filter_map(|t| ctx.facts.take_files.get(t))
        .flatten()
        .collect();
    let bytes: u64 = files.iter().map(|f| f.bytes).sum();
    let names = take_names(&takes);
    if !ctx.confirmed {
        let mut what = format!(
            "purge {names} from disk ({} {}, {})",
            files.len(),
            if files.len() == 1 { "file" } else { "files" },
            byte_size(bytes)
        );
        if rewrite.entries_lost > 0 {
            what.push_str(&format!(" and {}", entries(rewrite.entries_lost)));
        }
        let files_json: Vec<_> = files
            .iter()
            .map(|f| json!({ "path": f.path, "bytes": f.bytes }))
            .collect();
        return Err(CoreError::confirmation_required(what).with_details(json!({
            "takes": takes,
            "files": files_json,
            "bytes": bytes,
            "historyEntriesLost": rewrite.entries_lost,
            "oldestKept": rewrite.oldest_kept,
        })));
    }

    let mut next = state.clone();
    next.history = rewrite.history;
    purge::remove_takes(&mut next.project, &takes);
    let job = next.runtime.alloc_job_id();
    next.runtime.jobs.insert(
        job,
        Job {
            id: job,
            kind: JobKind::Purge {
                takes: takes.clone(),
            },
            req: ctx.req.clone(),
            started: ctx.now,
        },
    );
    let committed = persist(state, next);
    let mut effects = vec![Effect::History(rewrite.op)];
    effects.extend(committed.effects);
    effects.push(Effect::DeleteTakeDirs { job, takes });
    Ok(Outcome::new(committed.state, effects, None))
}

/// A take can be purged: it exists, isn't recording, no clip uses it and
/// no job reads its files.
fn purgeable(state: &State, id: &TakeId) -> Result<(), CoreError> {
    let take = state.project.take(id).ok_or_else(|| {
        CoreError::not_found(format!("no take {id}")).with_hint("`qrec take list` shows the takes")
    })?;
    let n = id.number();
    if take.status == TakeStatus::Recording {
        return Err(CoreError::busy(format!(
            "take {n} is still being recorded or recovered"
        )));
    }
    let clips: Vec<&str> = state
        .project
        .timeline
        .clips
        .iter()
        .filter(|c| &c.take == id)
        .map(|c| c.id.as_str())
        .collect();
    if !clips.is_empty() {
        return Err(CoreError::invalid(format!(
            "take {n} is on the timeline (clips {})",
            clips.join(", ")
        ))
        .with_hint(
            "remove its clips first (qrec clip remove …): only takes in the bin can be purged",
        ));
    }
    if let Some(job) = job_reading(state, id, take) {
        return Err(CoreError::busy(format!("{job} is reading take {n}"))
            .with_hint("wait for it to finish or cancel it, then purge"));
    }
    Ok(())
}

/// The kind of a running job that reads the take's files.
fn job_reading(state: &State, id: &TakeId, take: &Take) -> Option<&'static str> {
    let prefix = format!("takes/{id}/");
    let reads = |files: &BTreeSet<String>| {
        files
            .iter()
            .any(|f| f.starts_with(&prefix) || take.streams.iter().any(|s| &s.file == f))
    };
    state.runtime.jobs.values().find_map(|job| match &job.kind {
        JobKind::Export { files, .. } => reads(files).then_some("an export"),
        JobKind::FrameSave { files, .. } => reads(files).then_some("a frame save"),
        JobKind::Autocut { targets, .. } => targets
            .iter()
            .any(|t| &t.clip.take == id)
            .then_some("an autocut analysis"),
        JobKind::Purge { takes } => takes.contains(id).then_some("a purge"),
    })
}

/// Undo/redo/restore: normalize the UI and emit `SetCursor` +
/// `WriteProject` (+ `WriteUi`). The reply's `seq` is the new cursor and
/// `created`/`removed` list the clip, layer and track ids that came back
/// or went away.
fn moved(prev: &State, next: State, message: String) -> Outcome {
    let cursor = next.history.cursor;
    let committed = persist(prev, next);
    let mut effects = vec![
        Effect::History(HistoryOp::SetCursor(cursor)),
        Effect::WriteProject,
    ];
    if committed.effects.contains(&Effect::WriteUi) {
        effects.push(Effect::WriteUi);
    }
    let (created, removed) = id_changes(&prev.project, &committed.state.project);
    let result = CmdResult::new(message)
        .with_seq(Some(cursor))
        .with_created(created)
        .with_removed(removed);
    Outcome::new(committed.state, effects, Some(Ok(result)))
}

/// Clip, project-layer and project-track ids only in `next` (created) and
/// only in `prev` (removed).
fn id_changes(prev: &Project, next: &Project) -> (Vec<String>, Vec<String>) {
    fn ids(p: &Project) -> Vec<String> {
        let clips = p.timeline.clips.iter().map(|c| c.id.to_string());
        let layers = p.scene.layers.iter().map(|l| l.id.to_string());
        let tracks = p.scene.audio.iter().map(|t| t.id.to_string());
        clips.chain(layers).chain(tracks).collect()
    }
    let (before, after) = (ids(prev), ids(next));
    let (in_before, in_after): (BTreeSet<&String>, BTreeSet<&String>) =
        (before.iter().collect(), after.iter().collect());
    let created = after
        .iter()
        .filter(|id| !in_before.contains(id))
        .cloned()
        .collect();
    let removed = before
        .iter()
        .filter(|id| !in_after.contains(id))
        .cloned()
        .collect();
    (created, removed)
}

/// The entries between two seqs (positions `from < to`), oldest first.
fn between(history: &History, from_seq: u64, to_seq: u64) -> Vec<&HistoryEntry> {
    match (
        ops::position_of(history, from_seq),
        ops::position_of(history, to_seq),
    ) {
        (Some(from), Some(to)) if from < to => history.entries[from..to].iter().collect(),
        _ => Vec::new(),
    }
}

fn label_of(history: &History, seq: u64) -> Option<&str> {
    history
        .entries
        .iter()
        .find(|e| e.seq == seq)
        .map(|e| e.label.as_str())
}

/// `entry 12 ("Split clip c7f3")`, or the base snapshot.
fn describe(history: &History, seq: u64) -> String {
    match label_of(history, seq) {
        Some(label) => format!("entry {seq} (\"{label}\")"),
        None => format!("the oldest state in the history ({seq})"),
    }
}

fn entries(n: usize) -> String {
    match n {
        1 => "1 history entry".to_owned(),
        n => format!("{n} history entries"),
    }
}

/// `take 3` or `takes 3, 4`.
pub(crate) fn take_names(takes: &[TakeId]) -> String {
    let numbers: Vec<String> = takes.iter().map(|t| t.number().to_string()).collect();
    match numbers.as_slice() {
        [one] => format!("take {one}"),
        many => format!("takes {}", many.join(", ")),
    }
}

/// `412.0 MB` (decimal units, one decimal).
pub(crate) fn byte_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::ErrorCode;
    use crate::history::commit;
    use crate::history::ops::unchecked;
    use crate::model::Clip;
    use crate::state::{AutocutParams, AutocutTarget, ClipRef, SilenceKey};
    use crate::testkit::{self, ProjectBuilder};

    fn ctx() -> Ctx {
        testkit::ctx("2026-10-14T12:30:00Z", 1)
    }

    fn confirmed() -> Ctx {
        let mut ctx = ctx();
        ctx.confirmed = true;
        ctx
    }

    fn take(n: u32) -> TakeId {
        TakeId::numbered(n)
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

    fn edit(state: &State, label: &str, edit: impl FnOnce(&mut Project)) -> State {
        let mut next = state.clone();
        edit(&mut next.project);
        commit(state, &ctx(), label.into(), Value::Null, next).state
    }

    /// e1 split c001, e2 remove c002 (the right part), e3 add a layer.
    fn edited() -> State {
        let s = start();
        let s = edit(&s, "Split clip c001", |p| {
            p.timeline.clips[0].out = 100;
            p.timeline.clips.push(Clip {
                id: "c002".parse().unwrap(),
                take: take(1),
                in_: 100,
                out: 300,
            });
        });
        let s = edit(&s, "Remove clip c002", |p| {
            p.timeline.clips.pop();
        });
        edit(&s, "Add layer L2", |p| {
            let mut layer = p.scene.layers[0].clone();
            layer.id = "L2".parse().unwrap();
            p.scene.layers.push(layer);
        })
    }

    fn reply(outcome: &Outcome) -> &CmdResult {
        outcome.reply.as_ref().unwrap().as_ref().unwrap()
    }

    #[test]
    fn undo_moves_the_cursor_and_writes() {
        let state = edited();
        let out = undo_checked(&state, &ctx(), 1, unchecked).unwrap();
        assert_eq!(out.state.history.cursor, 2);
        assert_eq!(
            out.effects,
            [
                Effect::History(HistoryOp::SetCursor(2)),
                Effect::WriteProject
            ]
        );
        let r = reply(&out);
        assert_eq!(r.seq, Some(2));
        assert_eq!(r.message, "Undid \"Add layer L2\"");
        assert_eq!(r.removed, ["L2"]);
        assert!(r.created.is_empty());

        let out = undo_checked(&out.state, &ctx(), 2, unchecked).unwrap();
        let r = reply(&out);
        assert_eq!(r.seq, Some(0));
        assert_eq!(
            r.message,
            "Undid 2 steps (back to before \"Split clip c001\")"
        );
        assert!(
            r.created.is_empty() && r.removed.is_empty(),
            "c002 came and went"
        );
        assert_eq!(out.state.project, start().project);
    }

    #[test]
    fn redo_reports_what_came_back() {
        let state = edited();
        let bottom = undo_checked(&state, &ctx(), 3, unchecked).unwrap().state;
        let out = redo_checked(&bottom, &ctx(), 1, unchecked).unwrap();
        let r = reply(&out);
        assert_eq!(r.message, "Redid \"Split clip c001\"");
        assert_eq!(r.created, ["c002"]);
        assert_eq!(r.seq, Some(1));
        let out = redo_checked(&out.state, &ctx(), 2, unchecked).unwrap();
        let r = reply(&out);
        assert_eq!(r.message, "Redid 2 steps (up to \"Add layer L2\")");
        assert_eq!(
            (r.created.clone(), r.removed.clone()),
            (vec!["L2".to_owned()], vec!["c002".to_owned()])
        );
        assert_eq!(out.state.project, state.project);
        assert_eq!(out.state.history, state.history);
        let err = redo_checked(&out.state, &ctx(), 1, unchecked).unwrap_err();
        assert_eq!(err.message, "nothing to redo");
    }

    #[test]
    fn restore_needs_confirmation_and_clears_disk_invalid() {
        let mut state = edited();
        let err = restore_checked(&state, &ctx(), 1, unchecked).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfirmationRequired);
        assert_eq!(
            err.message,
            "confirmation required: restore entry 1 (\"Split clip c001\"): undoes 2 history entries"
        );
        assert_eq!(
            err.details.unwrap(),
            json!({"seq": 1, "label": "Split clip c001", "undo": 2, "redo": 0})
        );
        let err = restore_checked(&state, &ctx(), 0, unchecked).unwrap_err();
        assert_eq!(
            err.message,
            "confirmation required: restore the oldest state in the history (0): undoes 3 history entries"
        );
        let err = restore_checked(&state, &ctx(), 3, unchecked).unwrap_err();
        assert!(
            err.message.ends_with("qrec.json is rewritten from it"),
            "{}",
            err.message
        );
        assert_eq!(
            restore_checked(&state, &ctx(), 42, unchecked)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );

        // The way out of an invalid qrec.json: restore the cursor itself.
        state.runtime.disk_invalid = Some("expected value at line 3".into());
        let out = restore_checked(&state, &confirmed(), 3, unchecked).unwrap();
        assert_eq!(out.state.runtime.disk_invalid, None);
        assert_eq!(out.state.project, state.project);
        assert_eq!(
            out.effects,
            [
                Effect::History(HistoryOp::SetCursor(3)),
                Effect::WriteProject
            ]
        );
        assert_eq!(reply(&out).message, "Restored entry 3 (\"Add layer L2\")");

        let out = restore_checked(&out.state, &confirmed(), 1, unchecked).unwrap();
        assert_eq!(out.state.history.cursor, 1);
        assert_eq!(reply(&out).seq, Some(1));
        let err = restore_checked(&out.state, &ctx(), 3, unchecked).unwrap_err();
        assert!(
            err.message.ends_with("redoes 2 history entries"),
            "{}",
            err.message
        );
    }

    #[test]
    fn clear_keeps_the_project_and_forgets_the_entries() {
        let state = edited();
        let err = history_clear(&state, &ctx()).unwrap_err();
        assert_eq!(
            err.message,
            "confirmation required: clear the undo history (3 history entries)"
        );
        assert_eq!(err.details.unwrap(), json!({"historyEntriesLost": 3}));
        let out = history_clear(&state, &confirmed()).unwrap();
        let h = &out.state.history;
        assert!(h.entries.is_empty());
        assert_eq!((h.cursor, h.base_seq, h.next_seq), (3, 3, 4));
        assert_eq!(out.state.project, state.project);
        assert_eq!(
            out.effects,
            [
                Effect::History(HistoryOp::Rewrite {
                    entries: vec![],
                    base: Box::new(state.project.clone()),
                    base_seq: 3,
                    cursor: 3,
                    next_seq: 4,
                }),
                Effect::WriteProject
            ]
        );
        assert_eq!(
            reply(&out).message,
            "Cleared the history (3 history entries)"
        );
        let seen: Vec<&str> = h.seen_clip_ids.iter().map(|id| id.as_str()).collect();
        assert_eq!(seen, ["c001"]);
        // Nothing left to clear (no confirmation needed), nothing to undo.
        let again = history_clear(&out.state, &ctx()).unwrap();
        assert!(again.effects.is_empty());
        let err = undo_checked(&out.state, &ctx(), 1, unchecked).unwrap_err();
        assert_eq!(err.message, "nothing to undo");
        // New entries continue the seqs.
        let next = edit(&out.state, "Rename", |p| p.name = "x".into());
        assert_eq!(next.history.cursor, 4);
        assert_eq!(
            undo_checked(&next, &ctx(), 1, unchecked)
                .unwrap()
                .state
                .project,
            state.project
        );
    }

    fn purge_ctx(confirmed: bool) -> Ctx {
        let mut ctx = ctx();
        ctx.confirmed = confirmed;
        ctx.facts.take_files.insert(
            take(2),
            vec![
                FileFact {
                    path: "takes/take-0002/L1-screen-DP-1.mkv".into(),
                    bytes: 412_000_000,
                    mtime_ns: 1,
                },
                FileFact {
                    path: "takes/take-0002/A1-mic.mka".into(),
                    bytes: 3_500_000,
                    mtime_ns: 1,
                },
            ],
        );
        ctx
    }

    /// Take 2 was on the timeline in e1 and removed in e2; e3 renames.
    fn with_take_2_history() -> State {
        let s = start();
        let s = edit(&s, "Add take 2", |p| {
            p.timeline.clips.push(Clip {
                id: "c002".parse().unwrap(),
                take: take(2),
                in_: 0,
                out: 200,
            })
        });
        let s = edit(&s, "Remove clip c002", |p| {
            p.timeline.clips.pop();
        });
        edit(&s, "Rename", |p| p.name = "talk".into())
    }

    #[test]
    fn purge_asks_first_with_the_cost() {
        let state = with_take_2_history();
        let err = take_purge(&state, &purge_ctx(false), vec![take(2), take(2)]).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfirmationRequired);
        assert_eq!(
            err.message,
            "confirmation required: purge take 2 from disk (2 files, 415.5 MB) and 2 history entries"
        );
        assert_eq!(
            err.details.unwrap(),
            json!({
                "takes": ["take-0002"],
                "files": [
                    {"path": "takes/take-0002/L1-screen-DP-1.mkv", "bytes": 412_000_000},
                    {"path": "takes/take-0002/A1-mic.mka", "bytes": 3_500_000}
                ],
                "bytes": 415_500_000,
                "historyEntriesLost": 2,
                "oldestKept": "Rename"
            })
        );
    }

    #[test]
    fn purge_rewrites_history_then_deletes() {
        let state = with_take_2_history();
        let out = take_purge(&state, &purge_ctx(true), vec![take(2)]).unwrap();
        assert!(out.reply.is_none(), "replies when the folders are gone");
        let next = &out.state;
        assert!(!next.project.takes.contains_key(&take(2)));
        assert_eq!(next.history.base_seq, 2);
        assert_eq!(next.history.entries.len(), 1);
        let job = crate::ids::JobId(1);
        assert_eq!(
            next.runtime.jobs[&job],
            Job {
                id: job,
                kind: JobKind::Purge {
                    takes: vec![take(2)]
                },
                req: ctx().req,
                started: ctx().now
            }
        );
        assert!(matches!(
            &out.effects[0],
            Effect::History(HistoryOp::Rewrite { base_seq: 2, .. })
        ));
        assert_eq!(
            out.effects[1..],
            [
                Effect::WriteProject,
                Effect::DeleteTakeDirs {
                    job,
                    takes: vec![take(2)]
                }
            ]
        );
        // The remaining history never brings take 2 back.
        let bottom = undo_checked(next, &ctx(), 1, unchecked).unwrap().state;
        assert!(
            bottom
                .project
                .timeline
                .clips
                .iter()
                .all(|c| c.take != take(2))
        );
        assert_eq!(
            undo_checked(&bottom, &ctx(), 1, unchecked)
                .unwrap_err()
                .message,
            "nothing to undo"
        );
    }

    #[test]
    fn purge_refuses_what_it_must_not_delete() {
        let mut state = start();
        let code =
            |s: &State, takes: Vec<TakeId>| take_purge(s, &purge_ctx(true), takes).unwrap_err();
        assert_eq!(code(&state, vec![]).code, ErrorCode::Usage);
        let err = code(&state, vec![take(9)]);
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::NotFound, "no take take-0009")
        );
        let err = code(&state, vec![take(2), take(1)]);
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(err.message, "take 1 is on the timeline (clips c001)");

        state.project.takes.get_mut(&take(2)).unwrap().status = TakeStatus::Recording;
        assert_eq!(code(&state, vec![take(2)]).code, ErrorCode::Busy);

        state.runtime.jobs.insert(
            crate::ids::JobId(5),
            Job {
                id: crate::ids::JobId(5),
                kind: JobKind::Export {
                    out: "/tmp/x.mp4".into(),
                    files: ["takes/take-0003/L1-screen-DP-1.mkv".to_owned()].into(),
                    total_frames: 1,
                    seg_frames: vec![],
                    seg_done: vec![],
                    stitch_total_ns: 0,
                    stitch_done_ns: 0,
                    stitching: false,
                },
                req: None,
                started: ctx().now,
            },
        );
        let err = code(&state, vec![take(3)]);
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::Busy, "an export is reading take 3")
        );

        state.runtime.jobs.clear();
        state.runtime.jobs.insert(
            crate::ids::JobId(6),
            Job {
                id: crate::ids::JobId(6),
                kind: JobKind::Autocut {
                    cmd: Value::Null,
                    targets: vec![AutocutTarget {
                        clip: ClipRef {
                            id: "c0ff".parse().unwrap(),
                            take: take(3),
                            in_: 0,
                            out: 10,
                        },
                        track: "A1".parse().unwrap(),
                        key: SilenceKey("k".into()),
                    }],
                    params: AutocutParams {
                        threshold_db: -35.0,
                        min_silence_ms: 400,
                        padding_ms: 120,
                        min_keep_ms: 200,
                    },
                    dry_run: false,
                    pending: Default::default(),
                    ready: false,
                },
                req: None,
                started: ctx().now,
            },
        );
        assert_eq!(
            code(&state, vec![take(3)]).message,
            "an autocut analysis is reading take 3"
        );
    }

    #[test]
    fn byte_sizes() {
        assert_eq!(byte_size(0), "0 B");
        assert_eq!(byte_size(999), "999 B");
        assert_eq!(byte_size(1_000), "1.0 kB");
        assert_eq!(byte_size(415_500_000), "415.5 MB");
        assert_eq!(byte_size(999_960_000), "1.0 GB");
        assert_eq!(byte_size(2_500_000_000_000_000), "2500.0 TB");
    }
}
