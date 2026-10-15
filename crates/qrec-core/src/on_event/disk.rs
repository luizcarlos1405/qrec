//! Disk events: external edits of `qrec.json` (A§7.4, A§7.7) and finished
//! purges (A§5.6). Owner: history.

use serde_json::json;

use crate::apply::Outcome;
use crate::apply::history::take_names;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::history::commit;
use crate::history::external::merge_external;
use crate::history::ops::{self, Check};
use crate::ids::JobId;
use crate::model::Project;
use crate::protocol::command::Command;
use crate::protocol::effect::{Effect, LogLevel};
use crate::protocol::reply::CmdResult;
use crate::protocol::toast::{Toast, ToastLevel, UiAction};
use crate::state::{JobKind, State};

/// `Ok`: merge (`history::external`), validate and commit an "External
/// edit" entry; `Err` (or a merge that doesn't validate): set
/// `runtime.disk_invalid` and keep the last good state. A later valid read
/// clears it.
pub fn project_changed_on_disk(
    state: &State,
    ctx: &Ctx,
    project: Result<Box<Project>, String>,
) -> Outcome {
    changed_on_disk(state, ctx, project, ops::validated)
}

/// [`project_changed_on_disk`] with another check of the merged project.
pub fn changed_on_disk(
    state: &State,
    ctx: &Ctx,
    project: Result<Box<Project>, String>,
    check: Check,
) -> Outcome {
    let file = match project {
        Ok(file) => *file,
        Err(msg) => return invalid_on_disk(state, msg),
    };
    if file == state.project {
        // The file says what we hold (e.g. a bad edit was reverted).
        let mut next = state.clone();
        next.runtime.disk_invalid = None;
        return Outcome::new(next, Vec::new(), None);
    }
    let merge = match merge_external(state, file.clone()) {
        Ok(merge) => merge,
        Err(e) => return invalid_on_disk(state, e.message),
    };
    if let Err(e) = check(&merge.project) {
        return invalid_on_disk(state, e.message);
    }

    let mut next = state.clone();
    next.project = merge.project;
    next.runtime.disk_invalid = None;
    let committed = commit(
        state,
        ctx,
        "External edit".to_owned(),
        json!({ "external": true }),
        next,
    );
    let mut effects = committed.effects;
    // Facts the file got wrong are restored on disk too.
    if !effects.contains(&Effect::WriteProject) && committed.state.project != file {
        effects.insert(0, Effect::WriteProject);
    }
    let warnings = merge.warnings.join("\n");
    match committed.seq {
        Some(seq) => {
            let level = if warnings.is_empty() {
                ToastLevel::Info
            } else {
                ToastLevel::Warning
            };
            let mut toast = Toast::new(level, "Merged an external edit of qrec.json").with_action(
                "Undo",
                UiAction::Dispatch(Box::new(Command::Undo { steps: 1 })),
            );
            if !warnings.is_empty() {
                toast = toast.with_body(warnings.clone());
            }
            effects.push(Effect::Notify(toast));
            effects.push(log(
                LogLevel::Info,
                format!("external edit of qrec.json merged as entry {seq}"),
            ));
        }
        None if !warnings.is_empty() => {
            effects.push(Effect::Notify(
                Toast::new(ToastLevel::Warning, "Ignored an external edit of qrec.json")
                    .with_body(warnings.clone()),
            ));
            effects.push(log(
                LogLevel::Info,
                "external edit of qrec.json changed nothing undoable",
            ));
        }
        None => effects.push(log(
            LogLevel::Debug,
            "external edit of qrec.json changed nothing undoable",
        )),
    }
    if !warnings.is_empty() {
        effects.push(log(
            LogLevel::Warn,
            format!("external edit: {}", merge.warnings.join("; ")),
        ));
    }
    Outcome::new(committed.state, effects, None)
}

/// A§7.7: remember why the file is bad; user edits are refused until it is
/// fixed or restored. Nothing is written (a bad file is never overwritten).
fn invalid_on_disk(state: &State, msg: String) -> Outcome {
    let mut next = state.clone();
    let new = state.runtime.disk_invalid.as_deref() != Some(msg.as_str());
    next.runtime.disk_invalid = Some(msg.clone());
    let mut effects = vec![log(
        LogLevel::Warn,
        format!("qrec.json on disk is invalid: {msg}"),
    )];
    if new {
        effects.push(Effect::Notify(
            Toast::new(ToastLevel::Error, "qrec.json on disk is invalid").with_body(format!(
                "{msg}\nQrec keeps the last good state. Fix the file, or run \
                 `qrec history restore {} --yes` to rewrite it from the history.",
                state.history.cursor
            )),
        ));
    }
    Outcome::new(next, effects, None)
}

/// The purge job finished: drop the job, reply to its requester.
pub fn take_dirs_deleted(
    state: &State,
    _ctx: &Ctx,
    job: JobId,
    deleted: Vec<String>,
    errors: Vec<String>,
) -> Outcome {
    let takes = match state.runtime.jobs.get(&job).map(|j| &j.kind) {
        Some(JobKind::Purge { takes }) => takes.clone(),
        _ => {
            let mut out = Outcome::unchanged(state);
            out.effects.push(log(
                LogLevel::Debug,
                format!("ignored TakeDirsDeleted for {job}: no such purge"),
            ));
            return out;
        }
    };
    let mut next = state.clone();
    let finished = next
        .runtime
        .jobs
        .remove(&job)
        .expect("the job was just found");
    let names = take_names(&takes);
    let reply = if errors.is_empty() {
        Ok(CmdResult::new(format!("Purged {names} from disk")).with_removed(&takes))
    } else {
        Err(CoreError::io(format!(
            "purged {names} from the project, but some files could not be deleted: {}",
            errors.join("; ")
        ))
        .with_hint("delete the leftover files by hand")
        .with_details(json!({ "deleted": deleted, "errors": errors })))
    };
    let mut effects = vec![log(
        LogLevel::Info,
        format!(
            "purge {job} done: {} deleted, {} errors",
            deleted.len(),
            errors.len()
        ),
    )];
    match (finished.req, reply) {
        (Some(req), reply) => effects.push(Effect::Reply { req, reply }),
        (None, Ok(result)) => effects.push(Effect::Notify(Toast::new(
            ToastLevel::Success,
            format!("{}.", result.message),
        ))),
        (None, Err(e)) => effects.push(Effect::Notify(
            Toast::new(
                ToastLevel::Warning,
                format!("Couldn't delete every file of {names}"),
            )
            .with_body(e.message),
        )),
    }
    Outcome::new(next, effects, None)
}

/// The purge's commit failed and its files couldn't be put back, so the
/// shell deleted nothing (A§5.6) and the purge lives only in memory until
/// a later commit saves it (A§7.6). The reply says nothing was deleted and
/// how to finish either way; it never says the take was purged.
pub fn purge_failed(state: &State, _ctx: &Ctx, job: JobId, message: String) -> Outcome {
    let takes = match state.runtime.jobs.get(&job).map(|j| &j.kind) {
        Some(JobKind::Purge { takes }) => takes.clone(),
        _ => {
            let mut out = Outcome::unchanged(state);
            out.effects.push(log(
                LogLevel::Debug,
                format!("ignored PurgeFailed for {job}: no such purge"),
            ));
            return out;
        }
    };
    let mut next = state.clone();
    let finished = next
        .runtime
        .jobs
        .remove(&job)
        .expect("the job was just found");
    let names = take_names(&takes);
    let folders: Vec<String> = takes.iter().map(|t| format!("takes/{t}/")).collect();
    let err = CoreError::io(format!(
        "the purge of {names} couldn't be saved, so no file was deleted: {message}"
    ))
    .with_hint(format!(
        "free disk space or fix the project folder's permissions, then check `qrec take list`: \
         purge what is still listed again; delete {} by hand only if it is no longer listed",
        folders.join(", ")
    ))
    .with_details(json!({ "takes": takes, "folders": folders }));
    let mut effects = vec![log(
        LogLevel::Warn,
        format!("purge {job} not saved: nothing deleted ({message})"),
    )];
    match finished.req {
        Some(req) => effects.push(Effect::Reply {
            req,
            reply: Err(err),
        }),
        None => effects.push(Effect::Notify(
            Toast::new(ToastLevel::Warning, format!("Couldn't purge {names}"))
                .with_body(err.message),
        )),
    }
    Outcome::new(next, effects, None)
}

fn log(level: LogLevel, message: impl Into<String>) -> Effect {
    Effect::Log {
        level,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryEntry;
    use crate::history::ops::{unchecked, undo_with};
    use crate::ids::TakeId;
    use crate::model::TakeStatus;
    use crate::protocol::effect::HistoryOp;
    use crate::state::{Job, RecordMode, RecordPhase};
    use crate::testkit::{self, ProjectBuilder};

    fn ctx() -> Ctx {
        testkit::ctx("2026-10-14T12:30:00Z", 7)
    }

    fn state() -> State {
        testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-1")
                .mic("A1", "alsa_input.mic")
                .take("take-0001", 300)
                .take("take-0002", 200)
                .clip("c001", "take-0001", 0, 300)
                .build(),
        )
    }

    fn changed(state: &State, file: Project) -> Outcome {
        changed_on_disk(state, &ctx(), Ok(Box::new(file)), unchecked)
    }

    fn toasts(outcome: &Outcome) -> Vec<&Toast> {
        outcome
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::Notify(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_edit_becomes_an_undoable_entry() {
        let state = state();
        let mut file = state.project.clone();
        file.name = "renamed".into();
        file.timeline.clips[0].out = 200;
        let out = changed(&state, file.clone());
        assert_eq!(out.state.project, file);
        let entry: &HistoryEntry = &out.state.history.entries[0];
        assert_eq!((entry.seq, entry.label.as_str()), (1, "External edit"));
        assert_eq!(entry.cmd, json!({"external": true}));
        assert_eq!(entry.ts, ctx().now);
        assert_eq!(
            out.effects[0],
            Effect::History(HistoryOp::Append(entry.clone()))
        );
        assert_eq!(out.effects[1], Effect::WriteProject);
        let toast = toasts(&out)[0];
        assert_eq!(toast.level, ToastLevel::Info);
        assert_eq!(
            toast.actions[0].action,
            UiAction::Dispatch(Box::new(Command::Undo { steps: 1 }))
        );
        assert!(out.reply.is_none());
        // Undo brings back what was there before the edit.
        let undone = undo_with(&out.state, 1, unchecked).unwrap();
        assert_eq!(undone.project, state.project);
    }

    #[test]
    fn ignored_facts_are_written_back() {
        let state = state();
        let mut file = state.project.clone();
        file.takes
            .get_mut(&TakeId::numbered(1))
            .unwrap()
            .duration_frames = 5;
        let out = changed(&state, file);
        assert!(
            out.state.history.entries.is_empty(),
            "nothing undoable changed"
        );
        assert_eq!(out.state.project, state.project);
        assert_eq!(
            out.effects[0],
            Effect::WriteProject,
            "the file is put right"
        );
        let toast = toasts(&out)[0];
        assert_eq!(toast.level, ToastLevel::Warning);
        assert_eq!(
            toast.body.as_deref(),
            Some("facts can't be edited by hand: ignored /takes/take-0001/durationFrames")
        );
    }

    #[test]
    fn our_own_state_changes_nothing() {
        let mut state = state();
        state.runtime.disk_invalid = Some("bad".into());
        let out = changed(&state, state.project.clone());
        assert_eq!(
            out.state.runtime.disk_invalid, None,
            "a valid read clears it"
        );
        assert!(out.effects.is_empty());
        assert_eq!(out.state.history, state.history);
    }

    #[test]
    fn an_unreadable_file_marks_the_disk_invalid() {
        let state = state();
        let out = project_changed_on_disk(&state, &ctx(), Err("expected value at line 3".into()));
        assert_eq!(
            out.state.runtime.disk_invalid.as_deref(),
            Some("expected value at line 3")
        );
        assert_eq!(out.state.project, state.project);
        assert!(!out.effects.contains(&Effect::WriteProject));
        let toast = toasts(&out)[0];
        assert_eq!(toast.level, ToastLevel::Error);
        assert!(
            toast
                .body
                .as_deref()
                .unwrap()
                .contains("qrec history restore 0 --yes")
        );
        // The same message again doesn't toast twice.
        let again =
            project_changed_on_disk(&out.state, &ctx(), Err("expected value at line 3".into()));
        assert!(toasts(&again).is_empty());
        // A valid edit afterwards clears it and merges.
        let mut file = state.project.clone();
        file.name = "fixed".into();
        let fixed = changed(&again.state, file);
        assert_eq!(fixed.state.runtime.disk_invalid, None);
        assert_eq!(fixed.state.project.name, "fixed");
    }

    #[test]
    fn a_merge_that_fails_the_check_keeps_the_last_good_state() {
        let state = state();
        let mut file = state.project.clone();
        file.timeline.clips[0].out = 999;
        fn bounds(p: &Project) -> Result<(), CoreError> {
            for c in &p.timeline.clips {
                if c.out > p.takes[&c.take].duration_frames {
                    return Err(CoreError::invalid(format!(
                        "clip {} ends after its take",
                        c.id
                    )));
                }
            }
            Ok(())
        }
        let out = changed_on_disk(&state, &ctx(), Ok(Box::new(file)), bounds);
        assert_eq!(out.state.project, state.project);
        assert_eq!(
            out.state.runtime.disk_invalid.as_deref(),
            Some("clip c001 ends after its take")
        );
        assert!(out.state.history.entries.is_empty());
    }

    #[test]
    fn merges_while_recording_keep_the_take() {
        let mut state = state();
        state
            .project
            .takes
            .get_mut(&TakeId::numbered(2))
            .unwrap()
            .status = TakeStatus::Recording;
        state.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(2),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        let mut file = state.project.clone();
        file.takes.remove(&TakeId::numbered(2));
        file.name = "x".into();
        let out = changed(&state, file);
        assert_eq!(out.state.project.takes, state.project.takes);
        assert_eq!(out.state.project.name, "x");
    }

    fn purging(req: Option<crate::ids::ReqId>) -> State {
        let mut state = state();
        state.runtime.jobs.insert(
            JobId(3),
            Job {
                id: JobId(3),
                kind: JobKind::Purge {
                    takes: vec![TakeId::numbered(2)],
                },
                req,
                started: ctx().now,
            },
        );
        state
    }

    #[test]
    fn a_finished_purge_replies() {
        let req = ctx().req;
        let state = purging(req.clone());
        let out = take_dirs_deleted(
            &state,
            &ctx(),
            JobId(3),
            vec!["takes/take-0002".into()],
            vec![],
        );
        assert!(out.state.runtime.jobs.is_empty());
        let reply = Effect::Reply {
            req: req.clone().unwrap(),
            reply: Ok(CmdResult::new("Purged take 2 from disk").with_removed(["take-0002"])),
        };
        assert!(out.effects.contains(&reply), "{:?}", out.effects);
        assert!(toasts(&out).is_empty(), "the requester gets the news");

        let out = take_dirs_deleted(
            &state,
            &ctx(),
            JobId(3),
            vec![],
            vec!["EACCES: L1.mkv".into()],
        );
        let Some(Effect::Reply { reply: Err(e), .. }) = out
            .effects
            .iter()
            .find(|e| matches!(e, Effect::Reply { .. }))
        else {
            panic!("an error reply")
        };
        assert_eq!(e.code, crate::ErrorCode::Io);
        assert_eq!(
            e.message,
            "purged take 2 from the project, but some files could not be deleted: EACCES: L1.mkv"
        );
    }

    #[test]
    fn a_purge_without_requester_toasts() {
        let state = purging(None);
        let out = take_dirs_deleted(
            &state,
            &ctx(),
            JobId(3),
            vec!["takes/take-0002".into()],
            vec![],
        );
        assert_eq!(toasts(&out)[0].title, "Purged take 2 from disk.");
        let out = take_dirs_deleted(&state, &ctx(), JobId(3), vec![], vec!["EIO".into()]);
        assert_eq!(toasts(&out)[0].level, ToastLevel::Warning);
    }

    /// A purge that couldn't be saved never says the take was purged or
    /// that its files are leftovers to delete: nothing was deleted.
    #[test]
    fn an_unsaved_purge_says_nothing_was_deleted() {
        let req = ctx().req;
        let state = purging(req.clone());
        let out = purge_failed(
            &state,
            &ctx(),
            JobId(3),
            "write .qrec/history.base.json: Permission denied".into(),
        );
        assert!(out.state.runtime.jobs.is_empty(), "the purge job is over");
        let Some(Effect::Reply {
            req: replied,
            reply: Err(e),
        }) = out
            .effects
            .iter()
            .find(|e| matches!(e, Effect::Reply { .. }))
        else {
            panic!("an error reply: {:?}", out.effects)
        };
        assert_eq!(Some(replied), req.as_ref());
        assert_eq!(e.code, crate::ErrorCode::Io);
        assert_eq!(
            e.message,
            "the purge of take 2 couldn't be saved, so no file was deleted: \
             write .qrec/history.base.json: Permission denied"
        );
        assert!(!e.message.contains("purged take"), "{e:?}");
        let hint = e.hint.as_deref().unwrap();
        assert!(hint.contains("qrec take list"), "{hint}");
        assert!(hint.contains("takes/take-0002/"), "{hint}");
        assert!(!hint.starts_with("delete the leftover files"), "{hint}");
        assert!(toasts(&out).is_empty(), "the requester gets the news");

        // Nobody waits: a warning toast says the same.
        let out = purge_failed(&purging(None), &ctx(), JobId(3), "EIO".into());
        let toast = toasts(&out)[0];
        assert_eq!(toast.level, ToastLevel::Warning);
        assert_eq!(toast.title, "Couldn't purge take 2");
        // Not a purge: ignored.
        let out = purge_failed(&state, &ctx(), JobId(9), "EIO".into());
        assert_eq!(out.state, state);
    }

    #[test]
    fn unknown_jobs_are_ignored() {
        let state = state();
        let out = take_dirs_deleted(&state, &ctx(), JobId(9), vec![], vec![]);
        assert_eq!(out.state, state);
        assert!(matches!(
            out.effects.as_slice(),
            [Effect::Log {
                level: LogLevel::Debug,
                ..
            }]
        ));
    }
}
