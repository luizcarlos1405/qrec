//! Requests, events, disk changes and interrupts (A§8.1): a `ReqId` per
//! request, `facts_needed` → gather → `apply`/`on_event`, then the outcome
//! through `effects::run_outcome`. An empty folder is initialized lazily
//! before its first command (A§8.4).

use std::os::unix::ffi::OsStrExt;
use std::sync::{Arc, Mutex};

use chrono::SubsecRound;
use qrec_core::apply::project::is_initialized;
use qrec_core::config::UserConfig;
use qrec_core::ctx::{Ctx, Facts, FactsNeeded};
use qrec_core::protocol::{Command, HelloInfo, ProbePurpose, Query, WireBody};
use qrec_core::state::JobKind;
use qrec_core::{
    CmdResult, CoreError, ErrorCode, Event, ReqId, apply, facts_needed, facts_needed_for_event,
    on_event,
};
use serde_json::{Value, json};

use super::effects::{Origin, Ran};
use super::open::{Folder, classify};
use super::{Session, with_current};
use crate::facts::config_problem_toast;
use crate::msg::{Request, Responder};
use crate::proc;

/// An outcome is re-dispatched at most this often because `qrec.json`
/// kept changing under it; then it is saved over the change.
const MAX_REDISPATCH: u32 = 3;

/// Commands an interrupted host still runs (A§8.6): the ones that stop
/// what runs. They never wait for a recovery either, and stop what waits
/// for it ([`aborts`]).
fn winds_down(cmd: &Command, state: &qrec_core::State) -> bool {
    match cmd {
        Command::RecordStop | Command::RecordCancel | Command::ExportCancel { .. } => true,
        Command::RecordToggle { .. } => !state.runtime.record.is_idle(),
        _ => false,
    }
}

/// Whether `stop`, a command that only stops something, aborts `parked`, a
/// command still waiting for the startup recovery (A§5.3). It would run
/// after the stop, so the stop acts on it like on a countdown: a stop,
/// cancel or toggle aborts a start (a toggle that waits is a start: it was
/// parked while nothing recorded), and `export cancel` without a job aborts
/// an export (a job id can only name one that runs).
fn aborts(stop: &Command, parked: &Command) -> bool {
    match stop {
        Command::RecordStop | Command::RecordCancel | Command::RecordToggle { .. } => {
            parked.may_start_recording()
        }
        Command::ExportCancel { job: None } => matches!(parked, Command::Export { .. }),
        _ => false,
    }
}

/// What a start aborted before it ran hears (like "countdown cancelled").
const START_ABORTED: &str = "recording cancelled before it started";

/// What the aborted command and the caller hear.
fn aborted(stop: &Command) -> (CoreError, CmdResult) {
    match stop {
        Command::ExportCancel { .. } => (
            CoreError::invalid("export cancelled"),
            CmdResult::new("Export cancelled before it started"),
        ),
        _ => (
            CoreError::invalid(START_ABORTED),
            CmdResult::new("Recording cancelled before it started"),
        ),
    }
}

/// The answer to a command an interrupted host doesn't run (A§8.6): it is
/// exiting, so nothing new starts, and a command that hadn't started when
/// the signal came never does.
fn stopping(cmd: &Command) -> CoreError {
    CoreError::busy(format!(
        "this Qrec host is stopping (it was interrupted), so {} didn't run",
        cmd.name()
    ))
    .with_hint("run the command again: it goes to the next host once this one has exited")
}

/// A command that waits for the takes being recovered (A§5.3): the first
/// request after a crash sees them recovered, and a restore of a project
/// rebuilt from the history (recovery mode, A§7.7) sees the facts their
/// Recover probes find, or its clips would use takes still `recording`.
/// Parked commands keep their order among themselves; commands that stop
/// something and UI-state ones (`Command::ui_only`, GUI D-47) never park.
pub(super) struct Parked {
    id: Option<ReqId>,
    cmd: Command,
    confirmed: bool,
    to: Option<Box<dyn Responder>>,
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
        // The exact bytes too when the path isn't UTF-8 (A§8.4).
        let (project_dir, project_dir_hex) = HelloInfo::dir_fields(self.dir.as_os_str().as_bytes());
        let info = HelloInfo {
            pid: std::process::id(),
            project_dir,
            project_dir_hex,
            host: self.state.runtime.host,
            record: self.state.runtime.record.clone(),
            jobs: self.state.runtime.jobs.values().cloned().collect(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        serde_json::to_value(info).unwrap_or(Value::Null)
    }

    fn answer_query(&mut self, id: &ReqId, query: &Query) -> Result<Value, CoreError> {
        let config = self.user_config();
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
        if self.interrupts > 0 && !winds_down(&cmd, &self.state) {
            return self.reply_now(id.as_ref(), &mut to, Err(stopping(&cmd)));
        }
        // A stop never waits for the recovery, so it acts on what waits too:
        // a start or export it would otherwise let run afterwards (A§5.3).
        // Commands wait only while the open's recovery holds them, before
        // any command ran: no take or export runs then, so aborting what
        // waits is all the stop does.
        if let Some(result) = self.abort_parked(&cmd) {
            let reply = serde_json::to_value(result).unwrap_or(Value::Null);
            return self.reply_now(id.as_ref(), &mut to, Ok(reply));
        }
        if self.holding_for_recovery() && !winds_down(&cmd, &self.state) && !cmd.ui_only() {
            // Resumed by `release_parked` once the Recover probes answered
            // (or, outside recovery mode, once the open's wait is over).
            // UI-state commands (selection, playhead, view, playback) never
            // wait (GUI D-47): they touch no take, and the parked commands
            // resolve the selection and playhead when they run, like every
            // command at its turn.
            tracing::info!("{} waits for the takes being recovered", cmd.name());
            self.parked.push(Parked {
                id,
                cmd,
                confirmed,
                to,
            });
            return;
        }
        let initialized = is_initialized(&self.state.project);
        if !initialized && cmd.creates_project() {
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
            match self.run_outcome(
                outcome,
                Origin::Command,
                id.as_ref(),
                &mut to,
                attempts > MAX_REDISPATCH,
            ) {
                Ran::Done => return,
                // An external edit was merged first: run the command again
                // against the merged state (nothing of the old outcome ran).
                Ran::ExternalFirst => continue,
            }
        }
    }

    /// `cmd` only stops something and commands it stops wait for the
    /// recovery ([`aborts`]): answers each of them (its requester hears it
    /// was cancelled before it started) and returns the caller's result.
    /// `None` when nothing waiting is stopped: `cmd` runs as usual.
    fn abort_parked(&mut self, cmd: &Command) -> Option<CmdResult> {
        if !self.parked.iter().any(|p| aborts(cmd, &p.cmd)) {
            return None;
        }
        let (err, result) = aborted(cmd);
        let (gone, kept): (Vec<Parked>, Vec<Parked>) = std::mem::take(&mut self.parked)
            .into_iter()
            .partition(|p| aborts(cmd, &p.cmd));
        self.parked = kept;
        for Parked {
            id,
            cmd: what,
            mut to,
            ..
        } in gone
        {
            tracing::info!(
                "{} cancelled {}, which waited for the takes being recovered",
                cmd.name(),
                what.name()
            );
            self.reply_now(id.as_ref(), &mut to, Err(err.clone()));
        }
        Some(result)
    }

    /// Runs a parked command now.
    pub(super) fn resume(&mut self, parked: Parked) {
        let Parked {
            id,
            cmd,
            confirmed,
            to,
        } = parked;
        self.dispatch(id, cmd, confirmed, to);
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
            // Its connection is gone: nothing more can be sent to it, and a
            // command it left waiting doesn't run.
            self.pending.remove(req);
            self.parked.retain(|p| p.id.as_ref() != Some(req));
        }
        self.run_event(ev);
    }

    /// Feeds one event to the core and runs its outcome.
    pub(super) fn run_event(&mut self, ev: Event) {
        // What the startup recovery finds is the host's own news, which an
        // in-process command still shows after its reply (A§8.6).
        let recovery = matches!(
            ev,
            Event::TakeProbed {
                purpose: ProbePurpose::Recover,
                ..
            }
        );
        let outer = self.recovery_news;
        self.recovery_news = outer || recovery;
        let mut attempts = 0;
        loop {
            attempts += 1;
            let need = facts_needed_for_event(&self.state, &ev);
            let facts = self.gather(&need);
            let ctx = self.ctx(None, false, facts);
            let outcome = on_event(&self.state, &ctx, ev.clone());
            match self.run_outcome(
                outcome,
                Origin::Event,
                None,
                &mut None,
                attempts > MAX_REDISPATCH,
            ) {
                Ran::Done => break,
                Ran::ExternalFirst => continue,
            }
        }
        self.recovery_news = outer;
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
    /// not cancel) and cancels the jobs; commands still waiting for a
    /// recovery never run, and from now on only commands that stop things
    /// do. The second SIGKILLs every child's process group (the host exits
    /// right after).
    pub(super) fn handle_interrupt(&mut self) {
        self.interrupts += 1;
        if self.interrupts > 1 {
            let killed = proc::signal_all(true);
            tracing::warn!("second interrupt: killed {killed} child process group(s)");
            return;
        }
        for parked in std::mem::take(&mut self.parked) {
            let Parked {
                id, cmd, mut to, ..
            } = parked;
            self.reply_now(id.as_ref(), &mut to, Err(stopping(&cmd)));
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

    /// Gathers facts; problems go to the log, a `config.toml` that can't be
    /// used to a toast ([`Session::report_config_problem`]).
    pub(super) fn gather(&mut self, need: &FactsNeeded) -> Facts {
        let gathered = self.facts.gather(need);
        for warning in &gathered.warnings {
            tracing::warn!("facts: {warning}");
        }
        self.report_config_problem();
        gathered.facts
    }

    /// The user config alone (a query, the capability check), reported
    /// like [`Session::gather`] does.
    pub(super) fn user_config(&mut self) -> UserConfig {
        let config = self.facts.config();
        self.report_config_problem();
        config
    }

    /// A `config.toml` that can't be used is ignored for the defaults
    /// (A§4.2), and people hear it: one warning toast per change of the
    /// file, whichever read saw it first (the open, a query or a dispatch;
    /// the Sessions of a process that share the [`ConfigFile`] say it once
    /// between them). Toasts reach the GUI, the CLI's stderr (`--json`:
    /// notice lines) or a headless host's log.
    ///
    /// [`ConfigFile`]: crate::facts::ConfigFile
    fn report_config_problem(&mut self) {
        if let Some(problem) = self.facts.take_config_problem() {
            self.notifier.notify(&config_problem_toast(problem));
        }
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
