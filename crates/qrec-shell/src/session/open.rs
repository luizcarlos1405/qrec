//! Opening a project: the folder rule and init order (A§8.4), the lock
//! (A§8.3), loading and reconciling the files (A§7.6, A§7.7), capabilities
//! before the first dispatch (A§8.10), recovery probes (A§5.3) and the
//! watcher (A§8.9).
//!
//! Opening writes nothing a reader or an unconfirmed command could lose:
//! a read-only open writes nothing at all, a recovery open (`qrec history
//! restore`) keeps every change in memory until the restore saves, and a
//! host's open only repairs what the load found (and merges a valid hand
//! edit as an "External edit" entry).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;
use qrec_core::apply::project::{is_initialized, uninitialized_state};
use qrec_core::apply::ui::live_scene;
use qrec_core::history::external::merged_preview;
use qrec_core::model::Project;
use qrec_core::on_event::record::start_recovery;
use qrec_core::protocol::{Toast, ToastLevel};
use qrec_core::{CoreError, ErrorCode, Event, State, TakeId};

use super::Session;
use super::config::{LogNotifier, SessionConfig, SessionEnv};
use super::effects::Origin;
use crate::caps::{self, VaConfig};
use crate::deps::{Deps, OpenOpts};
use crate::error::ShellError;
use crate::facts::{FactsGatherer, FakeSources, Sources, SystemSources};
use crate::host::lockfile;
use crate::lock::ProjectLock;
use crate::msg::Inbox;
use crate::paths::ProjectPaths;
use crate::proc;
use crate::store::{self, Check, LoadOpts, Loaded, Store};
use crate::timers::Timers;
use crate::watcher;

/// How long opening a project waits for the recovery probes (A§5.3)
/// before the first request runs. Probing reads every packet of the files
/// (`-count_packets`), a few seconds even for long takes; the runner's own
/// limit is 20 s plus 30 s per GiB.
pub const RECOVERY_WAIT: Duration = Duration::from_secs(60);

/// What a folder holds (A§8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Folder {
    /// `qrec.json` is there.
    Project,
    /// Strictly empty, or only a `.qrec/` without a history (an init that
    /// stopped half-way): a new project goes here.
    Empty,
    /// `.qrec/` holds a history but `qrec.json` is gone (restore it).
    MissingFile,
}

/// The folder rule. Anything else is "Not my swamp" (exit 3).
pub(super) fn classify(paths: &ProjectPaths) -> Result<Folder, ShellError> {
    if store::exists(&paths.project_file()) {
        return Ok(Folder::Project);
    }
    // A project that lost its qrec.json (its takes/ and exports/ are
    // still there): restore it rather than call it someone else's folder.
    let history = [
        paths.history_head(),
        paths.history_file(),
        paths.history_base(),
    ];
    if paths.qrec_dir().is_dir() && history.iter().any(|p| store::exists(p)) {
        return Ok(Folder::MissingFile);
    }
    let root = paths.root();
    let entries =
        fs::read_dir(root).map_err(|e| ShellError::io(format!("read {}", root.display()), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| ShellError::io(format!("read {}", root.display()), e))?;
        let qrec = entry.file_name() == ".qrec" && entry.file_type().is_ok_and(|t| t.is_dir());
        if !qrec {
            return Err(CoreError::not_project().into());
        }
    }
    Ok(Folder::Empty)
}

fn canonical_dir(dir: &Path) -> Result<PathBuf, ShellError> {
    let canonical = fs::canonicalize(dir).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => ShellError::Core(
            CoreError::new(
                ErrorCode::NotProject,
                format!("{} doesn't exist", dir.display()),
            )
            .with_hint("pass --project with an existing folder"),
        ),
        _ => ShellError::io(format!("open {}", dir.display()), e),
    })?;
    if !canonical.is_dir() {
        return Err(CoreError::new(
            ErrorCode::NotProject,
            format!("{} is not a folder", dir.display()),
        )
        .into());
    }
    Ok(canonical)
}

/// `Busy` naming the host when its record says who it is (and it still runs).
fn lock_held(dir: &Path) -> ShellError {
    let who = match lockfile::read_live(dir) {
        Ok(Some(record)) => format!("a {} host (pid {})", record.kind.as_str(), record.pid),
        _ => "another Qrec process".to_owned(),
    };
    CoreError::busy(format!("the project is open in {who}"))
        .with_hint(
            "send the command to it (the CLI routes there by itself), or wait until it exits",
        )
        .into()
}

pub(super) fn open(
    dir: &Path,
    opts: OpenOpts,
    deps: Deps,
    cfg: SessionConfig,
) -> Result<Session, ShellError> {
    let dir = canonical_dir(dir)?;
    let paths = ProjectPaths::new(&dir);
    let env = match cfg.env {
        Some(env) => env,
        None => SessionEnv::from_process()?,
    };
    #[cfg(test)]
    let check: Check = cfg.check.unwrap_or(store::validated);
    #[cfg(not(test))]
    let check: Check = store::validated;

    let mut folder = classify(&paths)?;
    if let Some(lock) = &cfg.lock {
        let same = fs::canonicalize(lock.path()).ok() == fs::canonicalize(paths.lock_file()).ok();
        if !same {
            return Err(ShellError::Internal(format!(
                "the lock {} doesn't belong to {}",
                lock.path().display(),
                paths.root().display()
            )));
        }
    }
    let lock = match cfg.lock {
        Some(lock) => Some(lock),
        None if opts.mutating => {
            Some(ProjectLock::try_acquire(&dir)?.ok_or_else(|| lock_held(&dir))?)
        }
        None => None,
    };
    if lock.is_some() {
        // A§8.4: another process may have initialized the folder meanwhile.
        folder = classify(&paths)?;
    }
    let writable = lock.is_some();

    let mut store = Store::new(paths.clone(), writable);
    let (state, loaded) = match folder {
        Folder::Empty => (uninitialized_state(opts.host), None),
        Folder::Project | Folder::MissingFile => {
            let loaded = store::load(
                &paths,
                &LoadOpts {
                    recovery: opts.recovery,
                    check,
                },
            )?;
            store.adopt(&loaded);
            let mut state = State::new(
                loaded.project.clone(),
                loaded.history.clone(),
                loaded.ui.clone(),
                opts.host,
            );
            state.runtime.disk_invalid = loaded.disk_invalid.clone();
            (state, Some(loaded))
        }
    };

    let (inbox, rx) = cfg.channel.unwrap_or_else(Inbox::channel);
    let sources: Box<dyn Sources> = match cfg.sources {
        Some(sources) => sources,
        None if env.fake_backend => Box::new(FakeSources),
        None => {
            let logs = if writable {
                paths.logs_dir()
            } else {
                env.dirs.state.join("logs")
            };
            Box::new(SystemSources::new(Some(logs)))
        }
    };
    let facts = FactsGatherer::new(paths.clone(), env.config_file(), sources);
    let timers = Timers::new(inbox.clone());
    let mut session = Session {
        dir,
        paths,
        state,
        deps,
        inbox,
        rx,
        store,
        opts,
        check,
        facts,
        timers,
        watcher: None,
        pending: BTreeMap::new(),
        parked: Vec::new(),
        next_req: 0,
        notifier: cfg.notifier.unwrap_or_else(|| Box::new(LogNotifier)),
        shutdown: false,
        interrupts: 0,
        #[cfg(test)]
        crashed: false,
        lock,
    };

    if let Some(loaded) = &loaded {
        session.report_load(loaded);
    }
    if writable {
        let logs = proc::prune_logs(&session.paths.logs_dir(), proc::KEEP_LOGS);
        let parts = proc::prune_temp_outputs(&session.paths, proc::TEMP_MAX_AGE);
        if logs + parts > 0 {
            tracing::debug!("pruned {logs} old logs and {parts} stale temp outputs");
        }
    }

    // Writes nothing: an outcome without history ops or `WriteProject`
    // never saves, even when the load found files to repair.
    session.detect_capabilities(&env);

    // An external edit (or a crash between writing qrec.json and the
    // commit record) is merged before anything else (A§7.6). Its commit
    // also rewrites whatever the load found to repair. A reader can't
    // record history (no lock): it shows the merged project and leaves the
    // "External edit" entry to the next host (A§8.4).
    if let Some(external) = loaded.as_ref().and_then(|l| l.external.clone()) {
        if writable {
            session.run_event(Event::ProjectChangedOnDisk { project: external });
        } else {
            session.preview_external(external);
        }
    }
    // A consistent commit record from the start. Recovery mode leaves the
    // files alone until its first write (the restore) rewrites them all.
    let initialized = is_initialized(&session.state.project);
    if writable && initialized && !opts.recovery {
        if session.store.is_dirty()
            && let Err(e) = session.store.write_all(&session.state)
        {
            session.notify_error("Couldn't repair the project files", &e.to_string());
        }
        session.store.sync_cursor_file(session.state.history.cursor);
    }
    if writable && is_initialized(&session.state.project) {
        session.start_recovery_probes();
        // A§5.3: a take left `recording` is finalized before the first
        // request runs, so even a one-shot CLI command recovers it (and
        // sees it recovered). Requests that arrive meanwhile queue.
        session.finish_recovery(cfg.recovery_wait.unwrap_or(RECOVERY_WAIT));
    }
    if writable && !opts.recovery {
        match watcher::watch(&session.dir, session.inbox.clone()) {
            Ok(w) => session.watcher = Some(w),
            Err(e) => tracing::warn!("external edits of qrec.json won't be noticed: {e}"),
        }
    }
    // What the live pipeline should show from the start (a GUI in Live mode).
    if let Some(live) = live_scene(&session.state) {
        session
            .deps
            .capture
            .set_live(Some(live), session.inbox.clone());
    }
    Ok(session)
}

impl Session {
    fn report_load(&mut self, loaded: &Loaded) {
        for note in &loaded.notes {
            tracing::warn!("{}: {note}", self.dir.display());
        }
        if !loaded.notes.is_empty() && self.store.writable() {
            // Recovery mode repairs nothing until the restore saves (A§7.7).
            let (title, then) = if self.opts.recovery {
                (
                    "The project's files need repair",
                    "\nA confirmed restore rewrites them.",
                )
            } else {
                ("Qrec repaired the project's files", "")
            };
            let toast = Toast::new(ToastLevel::Warning, title)
                .with_body(format!("{}{then}", loaded.notes.join("\n")));
            self.notifier.notify(&toast);
        }
    }

    pub(super) fn notify_error(&mut self, title: &str, body: &str) {
        tracing::error!("{title}: {body}");
        self.notifier
            .notify(&Toast::new(ToastLevel::Error, title).with_body(body));
    }

    /// `Event::Capabilities` before the first dispatch (A§3).
    fn detect_capabilities(&mut self, env: &SessionEnv) {
        let (config, _) = self.facts.config();
        let va = VaConfig {
            project: self.state.project.recording.vaapi_device.clone(),
            config: config.encoder.vaapi_device.clone(),
        };
        let detected = caps::detect(&env.caps, &va);
        for note in &detected.notes {
            tracing::info!("capabilities: {note}");
        }
        self.run_event(Event::Capabilities(detected.caps));
    }

    /// One `Probe{Recover}` per take still `recording` (A§5.3 "Recovery",
    /// the record owner's `start_recovery`). Each take stays in
    /// `runtime.recovering` until its result is applied, so the host keeps
    /// running until then (A§8.6), an in-process command too.
    fn start_recovery_probes(&mut self) {
        let outcome = start_recovery(&self.state);
        if !outcome.effects.is_empty() {
            self.run_outcome(outcome, Origin::Event, None, &mut None, true);
        }
    }

    /// A read-only open found a valid hand edit (A§8.4): show the project a
    /// host would merge, with the history as it is on disk. No entry is
    /// made up (nothing can be saved here), so the cursor and seqs agree
    /// with `qrec history`; the next host records the edit.
    fn preview_external(&mut self, file: Result<Box<Project>, String>) {
        let preview = file.and_then(|file| merged_preview(&self.state, *file, self.check));
        match preview {
            Ok(project) if project != self.state.project => {
                tracing::info!(
                    "qrec.json was edited by hand; the next change records it as an External edit"
                );
                self.state.project = project;
            }
            Ok(_) => {}
            Err(why) => self.state.runtime.disk_invalid = Some(why),
        }
    }

    /// Handles what arrives until every recovery probe is answered, for at
    /// most `wait`. A probe still running then (a huge file) is answered
    /// later: [`Session::run_until`] waits for it.
    fn finish_recovery(&mut self, wait: Duration) {
        let deadline = Instant::now() + wait;
        while !self.state.runtime.recovering.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                tracing::warn!(
                    "still recovering {} after {} s; the project opens meanwhile",
                    self.state.runtime.recovering
                        .iter()
                        .map(TakeId::as_str)
                        .collect::<Vec<_>>()
                        .join(", "),
                    wait.as_secs()
                );
                return;
            }
            match self.rx.recv_timeout(left) {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}
