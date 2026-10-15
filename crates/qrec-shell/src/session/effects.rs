//! Running an `Outcome` (A§7.6): take folders, then the commit (history →
//! `qrec.json` → `history.head.json`), then the effects that touch the
//! world in the order the core gave them, then the reply.
//!
//! - Before anything is written, `qrec.json` is compared with what this
//!   host last wrote (A§8.9): an external edit is merged first and the
//!   caller runs the request again against the merged state.
//! - A `qrec.json` that failed to load is never overwritten silently
//!   (A§7.7): a restore copies it to `.qrec/qrec.corrupt-<ts>.json`, a
//!   write of facts moves it to `.qrec/rejected/`. Both key on whether the
//!   commit writes `qrec.json` at all (history ops or `WriteProject`).
//! - Recovery mode writes nothing until a request saves: events' changes
//!   stay in memory for the restore's commit, and while `qrec.json` is
//!   unusable only the restore that replaces it saves.
//! - Effects that need the commit to be durable (`StartCapture`,
//!   `DeleteTakeDirs`) don't run when it failed; the core gets the failure
//!   as an event instead. A failed `CreateTakeDir` fails that take's
//!   capture the same way.
//! - A command whose change can't be saved didn't happen (A§7.6: the head
//!   is the commit point): the state stays as it was, none of its effects
//!   run and its requester gets an `io` error saying nothing changed. A
//!   CLI host exits right after its reply, so the change can't wait in
//!   memory for a later save. The exception is a history `Rewrite`
//!   (purge, clear, compaction), whose base file can't be put back: it is
//!   kept like an event's facts.
//! - An event's outcome carries facts (a finished take, a recovery), so it
//!   is kept in memory and the store saves it with the next commit; replies
//!   it sends say the change isn't saved yet.

use std::collections::BTreeMap;
use std::time::Instant;

use chrono::Duration;
use qrec_core::apply::Outcome;
use qrec_core::apply::project::is_initialized;
use qrec_core::protocol::{Effect, HistoryOp, LogLevel, Toast, ToastLevel, UiAction};
use qrec_core::{CoreError, Event, ReqId, State, TakeId};
use serde_json::Value;

use super::{Session, with_current};
use crate::msg::Responder;
use crate::store::{CommitError, CommitMark};

/// What became of an outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ran {
    /// Committed (or failed to) and run.
    Done,
    /// An external edit of `qrec.json` was merged first; nothing of the
    /// outcome ran. The caller runs the request or event again.
    ExternalFirst,
}

/// Where an outcome comes from, which decides what a failed commit does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    /// `apply` of a command: an unsaved change is dropped.
    Command,
    /// `on_event`: facts are kept and saved with the next commit.
    Event,
}

impl Session {
    /// Runs `outcome`; `req`/`to` are the request it answers (if any).
    /// `force` skips the external-edit check (after repeated re-dispatches).
    pub(super) fn run_outcome(
        &mut self,
        outcome: Outcome,
        origin: Origin,
        req: Option<&ReqId>,
        to: &mut Option<Box<dyn Responder>>,
        force: bool,
    ) -> Ran {
        let Outcome {
            state: next,
            effects,
            reply,
        } = outcome;
        let ops: Vec<&HistoryOp> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::History(op) => Some(op),
                _ => None,
            })
            .collect();
        let write_project = effects.contains(&Effect::WriteProject);
        // What `Store::commit` writes: history ops always rewrite qrec.json.
        let saves = write_project || !ops.is_empty();
        let writable = self.store.writable();

        if writable && saves && !force && is_initialized(&self.state.project) {
            if let Some(file) = self.store.disk_changed(self.check) {
                tracing::info!("qrec.json changed on disk: merging it before this change");
                self.store.accept_disk(&file);
                self.run_event(Event::ProjectChangedOnDisk {
                    project: file.project,
                });
                return Ran::ExternalFirst;
            }
        }

        // A§7.7: recovery mode (`qrec history restore`) leaves the files
        // alone until a request saves. An event's change (a merged hand
        // edit, a recovered take) is kept in memory for the restore to
        // save; while qrec.json is unusable, only the request that replaces
        // it (the restore) may save at all.
        if writable && saves && self.opts.recovery {
            if req.is_none() && to.is_none() {
                return self.keep_in_memory(next, effects);
            }
            if let (Some(why), Some(_)) =
                (&self.state.runtime.disk_invalid, &next.runtime.disk_invalid)
            {
                let err = CoreError::invalid(format!("qrec.json on disk is unusable: {why}"))
                    .with_hint(format!(
                        "restore it first with `qrec history restore {} --yes`",
                        self.state.history.cursor
                    ));
                self.reply_now(req, to, Err(err));
                return Ran::Done;
            }
        }

        // Take folders come first: a take whose folder can't be made is
        // still saved (as recording), then fails instead of capturing.
        let mut failed_takes: BTreeMap<TakeId, String> = BTreeMap::new();
        for effect in &effects {
            if let Effect::CreateTakeDir { path } = effect
                && let Err(why) = self.store.create_take_dir(path)
            {
                tracing::error!("{why}");
                let take = path
                    .rsplit('/')
                    .next()
                    .and_then(|name| name.parse::<TakeId>().ok());
                if let Some(take) = take {
                    failed_takes.insert(take, why);
                }
            }
        }

        // A§7.7: a qrec.json that failed to load is kept, never overwritten.
        let mut reclaimed = false;
        if writable && saves && self.state.runtime.disk_invalid.is_some() {
            let stamp = self.stamp();
            if next.runtime.disk_invalid.is_none() {
                match self.store.copy_disk_file_aside(&stamp) {
                    Ok(Some(path)) => self.notifier.notify(
                        &Toast::new(ToastLevel::Info, "Kept the broken qrec.json")
                            .with_body(format!(
                                "It was copied to {} before the restore.",
                                path.display()
                            ))
                            .with_action(
                                "Reveal",
                                UiAction::RevealPath(path.to_string_lossy().into_owned()),
                            ),
                    ),
                    Ok(None) => {}
                    Err(e) => self.notify_error(
                        "Couldn't keep a copy of the broken qrec.json",
                        &e.to_string(),
                    ),
                }
            } else {
                match self.store.reject_disk_file(&stamp) {
                    Ok(Some(path)) => {
                        reclaimed = true;
                        self.notifier.notify(
                            &Toast::new(ToastLevel::Warning, "Moved the invalid qrec.json aside")
                                .with_body(format!(
                                    "Qrec had to save the take's facts, so the invalid file is now {}.",
                                    path.display()
                                ))
                                .with_action("Reveal", UiAction::RevealPath(path.to_string_lossy().into_owned())),
                        );
                    }
                    Ok(None) => reclaimed = true,
                    Err(e) => self
                        .notify_error("Couldn't move the invalid qrec.json aside", &e.to_string()),
                }
            }
        }

        let was_initialized = is_initialized(&self.state.project);
        let droppable = origin == Origin::Command
            && saves
            && !reclaimed
            && !ops.iter().any(|op| matches!(op, HistoryOp::Rewrite { .. }));
        let mark = droppable.then(|| self.store.mark());
        let commit = self.store.commit(&self.state, &next, &ops, write_project);
        #[cfg(test)]
        if let Err(CommitError::Crashed(_)) = &commit {
            // The "process" died here: nothing else runs, nobody hears back.
            self.state = next;
            self.crashed = true;
            return Ran::Done;
        }
        if let Err(e) = &commit
            && let Some(mark) = mark
            && self.drop_unsaved(mark, e)
        {
            // The state stays as it was and none of the effects run.
            let err = CoreError::io(format!("couldn't save the project, so nothing changed: {e}"))
                .with_hint("free disk space or fix the project folder's permissions, then run the command again");
            self.reply_now(req, to, Err(err));
            return Ran::Done;
        }
        self.state = next;
        let unsaved = match &commit {
            Ok(()) => None,
            Err(e) => {
                self.notify_error("Couldn't save the project", &e.to_string());
                // Kept in memory: replies say so instead of "ok".
                saves.then(|| {
                    CoreError::io(format!("the change was made but couldn't be saved: {e}"))
                        .with_hint("free disk space or fix the project folder's permissions; this Qrec process saves it with its next change, and loses it if it exits before then")
                })
            }
        };
        let committed = commit.is_ok();
        if committed && !was_initialized && is_initialized(&self.state.project) {
            // A§8.4: `takes/` is made right after the first commit.
            if let Err(e) = self.store.ensure_takes_dir() {
                tracing::warn!("can't create takes/: {e}");
            }
        }
        if committed && reclaimed {
            // qrec.json is this host's again: the core clears `disk_invalid`.
            let project = Ok(Box::new(self.state.project.clone()));
            self.run_event(Event::ProjectChangedOnDisk { project });
        }

        // The reply waits for the commit (A§7.6 step 4). The requester is
        // registered first, so progress and late replies among the effects
        // reach it.
        let reply = match (reply, &unsaved) {
            (Some(Ok(_)), Some(err)) => Some(Err(err.clone())),
            (reply, _) => reply,
        };
        if let Some(id) = req
            && let Some(responder) = to.take()
        {
            self.pending.insert(id.clone(), responder);
        }
        for effect in effects {
            self.run_world_effect(effect, committed, unsaved.as_ref(), &failed_takes);
        }
        if let Some(reply) = reply {
            let reply = reply.map(|result| serde_json::to_value(result).unwrap_or(Value::Null));
            let mut responder = match req {
                Some(id) => self.pending.remove(id),
                None => to.take(),
            };
            self.reply_now(req, &mut responder, reply);
        }
        Ran::Done
    }

    /// After a failed commit of a command: puts back what it wrote
    /// (`Store::drop_commit`). `false` when `qrec.json` already holds the
    /// change and can't be put back: then the change is kept instead, as
    /// after a crash between the project and the head writes (A§7.6).
    fn drop_unsaved(&mut self, mark: CommitMark, failed: &CommitError) -> bool {
        match self.store.drop_commit(mark, &self.state, failed) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("can't put qrec.json back after a failed save: {e}");
                false
            }
        }
    }

    /// Recovery mode: an event's change stays in memory (the store is
    /// dirty, so the restore's commit writes it). Effects that need a
    /// durable commit don't run, as after a failed one; `ui.json` isn't
    /// written either, and toasts only go to the log: nothing they announce
    /// is saved unless the restore runs (A§7.6 step 4).
    fn keep_in_memory(&mut self, next: State, effects: Vec<Effect>) -> Ran {
        tracing::info!("recovery mode: a change is kept in memory until the restore saves it");
        self.store.mark_dirty();
        self.state = next;
        let no_failed_takes = BTreeMap::new();
        for effect in effects {
            match effect {
                Effect::WriteUi => {}
                Effect::Notify(toast) => tracing::info!(
                    "{} {}",
                    toast.title,
                    toast.body.as_deref().unwrap_or_default()
                ),
                effect => self.run_world_effect(effect, false, None, &no_failed_takes),
            }
        }
        Ran::Done
    }

    /// One effect after the commit. `unsaved` is the error replies get
    /// instead of "ok" when the commit failed.
    fn run_world_effect(
        &mut self,
        effect: Effect,
        committed: bool,
        unsaved: Option<&CoreError>,
        failed_takes: &BTreeMap<TakeId, String>,
    ) {
        let inbox = self.inbox.clone();
        match effect {
            Effect::WriteProject | Effect::History(_) | Effect::CreateTakeDir { .. } => {}
            Effect::WriteUi => {
                if let Err(e) = self.store.want_ui_write(&self.state.ui, Instant::now()) {
                    tracing::warn!("can't write .qrec/ui.json: {e}");
                }
            }
            Effect::DeleteTakeDirs { job, takes } => {
                if committed {
                    self.deps.media.delete_take_dirs(job, takes, inbox);
                } else {
                    inbox.event(Event::TakeDirsDeleted {
                        job,
                        deleted: Vec::new(),
                        errors: vec![
                            "nothing was deleted: the project couldn't be saved first".to_owned(),
                        ],
                    });
                }
            }
            Effect::SetLive(live) => self.deps.capture.set_live(live, inbox),
            Effect::StartCapture { plan } => {
                let failure = match failed_takes.get(&plan.take) {
                    Some(why) => Some(why.clone()),
                    None if !committed => {
                        Some("the project couldn't be saved, so the take didn't start".to_owned())
                    }
                    None => None,
                };
                match failure {
                    Some(message) => {
                        inbox.event(Event::CaptureFailed {
                            take: Some(plan.take),
                            source: None,
                            message,
                            missing: None,
                        });
                    }
                    None => self.deps.capture.start(plan, inbox),
                }
            }
            Effect::StopCapture { take, cancel } => self.deps.capture.stop(&take, cancel),
            Effect::Probe {
                take,
                purpose,
                jobs,
            } => self.deps.media.probe(take, purpose, jobs, inbox),
            Effect::DetectSilence { job, key, argv } => {
                self.deps.media.detect_silence(job, key, argv, inbox)
            }
            Effect::RenderExport { job, plan } => self.deps.media.render_export(job, plan, inbox),
            Effect::SaveFrame { job, plan } => self.deps.media.save_frame(job, plan, inbox),
            Effect::CancelJob { job } => self.deps.media.cancel(job),
            Effect::StartTimer { timer, after_ms } => self.timers.start(timer, after_ms),
            Effect::CancelTimer { timer } => self.timers.cancel(timer),
            Effect::Notify(toast) => self.notifier.notify(&toast),
            Effect::Progress { req, progress } => match self.pending.get_mut(&req) {
                Some(to) => with_current(&req, || to.progress(&progress)),
                None => tracing::debug!("progress for {req}, which nobody waits for"),
            },
            Effect::Reply { req, reply } => match self.pending.remove(&req) {
                Some(mut to) => {
                    let reply = match (reply, unsaved) {
                        (Ok(_), Some(err)) => Err(err.clone()),
                        (reply, _) => {
                            reply.map(|result| serde_json::to_value(result).unwrap_or(Value::Null))
                        }
                    };
                    with_current(&req, || to.reply(reply));
                }
                None => tracing::debug!("a reply for {req}, which nobody waits for"),
            },
            Effect::Log { level, message } => match level {
                LogLevel::Debug => tracing::debug!("{message}"),
                LogLevel::Info => tracing::info!("{message}"),
                LogLevel::Warn => tracing::warn!("{message}"),
                LogLevel::Error => tracing::error!("{message}"),
            },
        }
    }

    /// `YYYYMMDD-HHMMSS` in local time, for files named after a moment.
    fn stamp(&mut self) -> String {
        let (now, offset) = (self.deps.clock)();
        let local = now + Duration::seconds(i64::from(offset));
        local.format("%Y%m%d-%H%M%S").to_string()
    }
}
