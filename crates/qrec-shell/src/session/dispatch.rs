//! Requests, events, disk changes and interrupts (A§8.1): a `ReqId` per
//! request, `facts_needed` → gather → `apply`/`on_event`, then the outcome
//! through `effects::run_outcome`. An empty folder is initialized lazily
//! before its first command (A§8.4).

use std::sync::{Arc, Mutex};

use chrono::SubsecRound;
use qrec_core::apply::project::is_initialized;
use qrec_core::ctx::{Ctx, Facts, FactsNeeded};
use qrec_core::protocol::{Command, HelloInfo, Query, Toast, ToastLevel, WireBody};
use qrec_core::state::JobKind;
use qrec_core::{
    CoreError, ErrorCode, Event, ReqId, apply, facts_needed, facts_needed_for_event, on_event,
};
use serde_json::{Value, json};

use super::effects::Ran;
use super::open::{Folder, classify};
use super::{Session, with_current};
use crate::msg::{Request, Responder};
use crate::proc;

/// An outcome is re-dispatched at most this often because `qrec.json`
/// kept changing under it; then it is saved over the change.
const MAX_REDISPATCH: u32 = 3;

/// Commands that create the project in an empty folder before they run
/// (A§8.4): edits and recordings. UI, playback, output, undo and history
/// commands don't: in an empty folder they get `not_project`.
pub(super) fn creates_the_project(cmd: &Command) -> bool {
    !matches!(cmd, Command::Init { .. })
        && (cmd.edits_project()
            || matches!(
                cmd,
                Command::RecordStart { .. } | Command::RecordToggle { .. }
            ))
        && !matches!(
            cmd,
            Command::Undo { .. } | Command::Redo { .. } | Command::HistoryClear
        )
}

/// Keeps the reply of an internal request.
struct Capture(Arc<Mutex<Option<Result<Value, CoreError>>>>);

impl Responder for Capture {
    fn accepted(&mut self) {}

    fn progress(&mut self, _: &qrec_core::protocol::Progress) {}

    fn reply(&mut self, r: Result<Value, CoreError>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(r);
        }
    }
}

impl Session {
    pub(super) fn handle_request(&mut self, req: Request, mut to: Box<dyn Responder>) {
        let id = self.alloc_req();
        with_current(&id, || to.accepted());
        match req.body {
            WireBody::Hello => {
                let hello = self.hello();
                with_current(&id, || to.reply(Ok(hello)));
            }
            WireBody::Shutdown => {
                self.shutdown = true;
                with_current(&id, || to.reply(Ok(json!({ "shuttingDown": true }))));
            }
            WireBody::Query(query) => {
                let answer = self.answer_query(&id, &query);
                with_current(&id, || to.reply(answer));
            }
            WireBody::Command(cmd) => self.dispatch(Some(id), cmd, req.confirmed, Some(to)),
        }
    }

    /// `hello` (A§4.9).
    fn hello(&self) -> Value {
        let info = HelloInfo {
            pid: std::process::id(),
            project_dir: self.dir.to_string_lossy().into_owned(),
            host: self.state.runtime.host,
            record: self.state.runtime.record.clone(),
            jobs: self.state.runtime.jobs.values().cloned().collect(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        serde_json::to_value(info).unwrap_or(Value::Null)
    }

    fn answer_query(&mut self, id: &ReqId, query: &Query) -> Result<Value, CoreError> {
        let (config, _) = self.facts.config();
        let keys_toml = if matches!(query, Query::Keys) {
            self.facts.keys_toml()
        } else {
            String::new()
        };
        let ctx = self.ctx(
            Some(id.clone()),
            false,
            Facts {
                config,
                keys_toml,
                ..Facts::default()
            },
        );
        let mut value = qrec_core::query(&self.state, &ctx, query)?;
        if matches!(query, Query::Status)
            && let Value::Object(view) = &mut value
        {
            // The shell's part of `StatusView`.
            view.insert(
                "path".into(),
                Value::from(self.dir.to_string_lossy().into_owned()),
            );
            if self.is_host() {
                view.insert("pid".into(), Value::from(std::process::id()));
            }
        }
        Ok(value)
    }

    /// Runs `cmd` for a requester (`to`) or for the session itself.
    pub(super) fn dispatch(
        &mut self,
        id: Option<ReqId>,
        cmd: Command,
        confirmed: bool,
        mut to: Option<Box<dyn Responder>>,
    ) {
        if !self.store.writable() {
            let err = CoreError::internal(format!(
                "{} needs the project lock, but this session was opened read-only",
                cmd.name()
            ));
            return self.reply_now(id.as_ref(), &mut to, Err(err));
        }
        let initialized = is_initialized(&self.state.project);
        if !initialized && creates_the_project(&cmd) {
            // A§8.4: the first mutating command in an empty folder creates
            // the project (other commands get the core's `not_project`).
            if let Err(e) = self.lazy_init() {
                return self.reply_now(id.as_ref(), &mut to, Err(e));
            }
        }
        if !initialized && matches!(cmd, Command::Init { .. }) {
            // The folder must still be empty when the project is written.
            let refusal = match classify(&self.paths) {
                Ok(Folder::Empty) => None,
                Ok(Folder::Project | Folder::MissingFile) => Some(
                    CoreError::invalid(
                        "a qrec.json appeared in this folder while Qrec had it open",
                    )
                    .with_hint("open the project again"),
                ),
                Err(e) => Some(e.into()),
            };
            if let Some(err) = refusal {
                return self.reply_now(id.as_ref(), &mut to, Err(err));
            }
        }
        let mut attempts = 0;
        loop {
            attempts += 1;
            let need = facts_needed(&self.state, &cmd);
            let facts = self.gather(&need);
            let ctx = self.ctx(id.clone(), confirmed, facts);
            let outcome = match apply(&self.state, &ctx, cmd.clone()) {
                Ok(outcome) => outcome,
                Err(e) => return self.reply_now(id.as_ref(), &mut to, Err(e)),
            };
            match self.run_outcome(outcome, id.as_ref(), &mut to, attempts > MAX_REDISPATCH) {
                Ran::Done => return,
                // An external edit was merged first: run the command again
                // against the merged state (nothing of the old outcome ran).
                Ran::ExternalFirst => continue,
            }
        }
    }

    /// `Command::Init` before the first command of an empty folder.
    fn lazy_init(&mut self) -> Result<(), CoreError> {
        let slot = Arc::new(Mutex::new(None));
        let init = Command::Init {
            canvas: None,
            name: None,
        };
        self.dispatch(
            None,
            init,
            false,
            Some(Box::new(Capture(Arc::clone(&slot)))),
        );
        let reply = slot.lock().ok().and_then(|mut s| s.take());
        match reply {
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(e),
            None => Err(CoreError::internal(
                "initializing the project gave no answer",
            )),
        }
    }

    pub(super) fn handle_event(&mut self, ev: Event) {
        if let Event::RequesterGone { req } = &ev {
            // Its connection is gone: nothing more can be sent to it.
            self.pending.remove(req);
        }
        self.run_event(ev);
    }

    /// Feeds one event to the core and runs its outcome.
    pub(super) fn run_event(&mut self, ev: Event) {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let need = facts_needed_for_event(&self.state, &ev);
            let facts = self.gather(&need);
            let ctx = self.ctx(None, false, facts);
            let outcome = on_event(&self.state, &ctx, ev.clone());
            match self.run_outcome(outcome, None, &mut None, attempts > MAX_REDISPATCH) {
                Ran::Done => return,
                Ran::ExternalFirst => continue,
            }
        }
    }

    /// The watcher saw `qrec.json` change (A§8.9): unless it is this host's
    /// own write, parse + migrate + check it and let the core merge it.
    pub(super) fn handle_disk_changed(&mut self) {
        if !self.store.writable() || self.opts.recovery || !is_initialized(&self.state.project) {
            return;
        }
        if let Some(file) = self.store.disk_changed(self.check) {
            self.store.accept_disk(&file);
            self.run_event(Event::ProjectChangedOnDisk {
                project: file.project,
            });
        }
    }

    /// Ctrl+C / SIGTERM (A§8.6). The first finalizes a recording (stop,
    /// not cancel) and cancels the jobs; the second SIGKILLs every child's
    /// process group (the host exits right after).
    pub(super) fn handle_interrupt(&mut self) {
        self.interrupts += 1;
        if self.interrupts > 1 {
            let killed = proc::signal_all(true);
            tracing::warn!("second interrupt: killed {killed} child process group(s)");
            return;
        }
        if !self.state.runtime.record.is_idle() && self.store.writable() {
            self.dispatch(None, Command::RecordStop, false, None);
        }
        let jobs: Vec<_> = self
            .state
            .runtime
            .jobs
            .values()
            .filter(|job| !matches!(job.kind, JobKind::Purge { .. }))
            .map(|job| job.id)
            .collect();
        for job in jobs {
            self.deps.media.cancel(job);
        }
    }

    /// The inputs of one `apply`/`on_event`: now (whole seconds), entropy,
    /// confirmation, the request and the facts.
    pub(super) fn ctx(&mut self, req: Option<ReqId>, confirmed: bool, facts: Facts) -> Ctx {
        let (now, utc_offset_secs) = (self.deps.clock)();
        Ctx {
            now: now.trunc_subsecs(0),
            utc_offset_secs,
            entropy: (self.deps.entropy)(),
            confirmed,
            req,
            facts,
        }
    }

    /// Gathers facts; problems go to the log (a broken config also to a toast, once).
    pub(super) fn gather(&mut self, need: &FactsNeeded) -> Facts {
        let gathered = self.facts.gather(need);
        for warning in &gathered.warnings {
            tracing::warn!("facts: {warning}");
        }
        if let Some(problem) = gathered.config_problem {
            self.notifier.notify(
                &Toast::new(ToastLevel::Warning, "config.toml is invalid").with_body(problem),
            );
        }
        gathered.facts
    }

    pub(super) fn reply_now(
        &mut self,
        id: Option<&ReqId>,
        to: &mut Option<Box<dyn Responder>>,
        reply: Result<Value, CoreError>,
    ) {
        if let Some(mut to) = to.take() {
            match id {
                Some(id) => with_current(id, || to.reply(reply)),
                None => to.reply(reply),
            }
        } else if let Err(e) = reply
            && e.code != ErrorCode::ConfirmationRequired
        {
            tracing::warn!("{e}");
        }
    }
}
