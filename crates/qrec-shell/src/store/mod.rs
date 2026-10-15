//! The project store (A§7.6, A§7.7). Owner: store.
//!
//! Reads and writes `qrec.json` (tmp → fsync → rename → fsync(dir)),
//! `.qrec/ui.json` (coalesced, no fsync), `.qrec/history.jsonl` (one line
//! per `Append` via `O_APPEND` + `fdatasync`; tmp + rename for redo-tail
//! drops and `Rewrite`), `history.base.json` and the commit record
//! `history.head.json` (`{cursor, baseSeq, nextSeq, projectSha256,
//! project}`). On open ([`load`]): drop history lines with `seq ≥
//! nextSeq`, drop a torn last line, detect external edits by hash,
//! recovery mode, migration backups (`.qrec/qrec.v{N}.json.bak`, never
//! overwritten) and rejected files (`.qrec/rejected/`).
//!
//! **Commit order** ([`Store::commit`]), per outcome:
//! 1. history: the line (or the whole file, and the base on `Rewrite`);
//! 2. `qrec.json`;
//! 3. `history.head.json`, the commit point.
//!
//! The session runs the effects that touch the world only after that.
//! A crash between any two steps leaves something [`load`] turns back into
//! a consistent state: an uncommitted history line has `seq ≥ nextSeq`
//! and is dropped; a `qrec.json` newer than the head is an external edit
//! of `head.project` that the core merges ("External edit").

pub mod format;
pub mod fsx;
pub mod load;
#[cfg(test)]
pub(crate) mod testdata;
#[cfg(test)]
mod tests;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qrec_core::apply::project::is_initialized;
use qrec_core::model::Project;
use qrec_core::protocol::HistoryOp;
use qrec_core::state::{History, State, UiState};
use qrec_core::{CoreError, TakeId};

use crate::paths::ProjectPaths;
pub use format::{BaseFile, Head, RESTORE_HINT};
pub use load::{LoadOpts, Loaded, load};

/// How a project coming from disk or history is checked before it becomes
/// the state: `qrec_core::validate::ensure_valid` (A§2.3) everywhere but
/// in tests that replay projects they made themselves.
pub type Check = fn(&Project) -> Result<(), CoreError>;

/// The production check (A§2.3).
pub fn validated(project: &Project) -> Result<(), CoreError> {
    qrec_core::validate::ensure_valid(project)
}

/// `ui.json` is written at most this often (A§4.6).
pub const UI_WRITE_INTERVAL: Duration = Duration::from_millis(500);

/// A step of the commit (A§7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    History,
    Project,
    Head,
}

/// A commit that didn't complete.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error("{what}: {source}")]
    Io {
        step: Step,
        what: String,
        #[source]
        source: io::Error,
    },
    /// Tests: the process "died" after this step (nothing later ran).
    #[cfg(test)]
    #[error("simulated crash after the {0:?} step")]
    Crashed(Step),
}

impl CommitError {
    pub fn to_core(&self) -> CoreError {
        CoreError::io(format!("couldn't save the project: {self}"))
            .with_hint("check free disk space and the project folder's permissions")
    }
}

/// What `qrec.json` holds right now, compared with what the host wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskFile {
    /// sha256 of the bytes; `None` when the file is missing.
    pub sha: Option<String>,
    /// Parsed, migrated and checked; `Err` says why it is unusable.
    pub project: Result<Box<Project>, String>,
    /// The file was an older schema version: the original bytes, backed up
    /// before the first save.
    pub migrated_from: Option<(u32, Vec<u8>)>,
}

/// Files of one project, and what this process knows about them.
pub struct Store {
    paths: ProjectPaths,
    /// Holding the project lock: allowed to write.
    writable: bool,
    /// sha256 of the `qrec.json` bytes this host last wrote or accepted.
    last_sha: Option<String>,
    /// `history.base.json` exists.
    has_base: bool,
    /// A base to write with the next history step.
    pending_base: Option<(u64, Project)>,
    /// A migrated file's original, saved before the next project write.
    pending_backup: Option<(u32, Vec<u8>)>,
    /// A commit failed (or the files need repair): the next one rewrites
    /// the history file, `qrec.json` and the head whatever it changed.
    dirty: bool,
    ui_last: Option<Instant>,
    ui_pending: bool,
    #[cfg(test)]
    pub(crate) crash_after: Option<Step>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.paths.root())
            .field("writable", &self.writable)
            .field("last_sha", &self.last_sha)
            .field("dirty", &self.dirty)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// A store for `paths`; `writable` when this process holds the lock.
    pub fn new(paths: ProjectPaths, writable: bool) -> Self {
        let has_base = paths.history_base().is_file();
        Self {
            paths,
            writable,
            last_sha: None,
            has_base,
            pending_base: None,
            pending_backup: None,
            dirty: false,
            ui_last: None,
            ui_pending: false,
            #[cfg(test)]
            crash_after: None,
        }
    }

    pub fn paths(&self) -> &ProjectPaths {
        &self.paths
    }

    pub fn writable(&self) -> bool {
        self.writable
    }

    /// sha256 of what this host last wrote to (or accepted from) `qrec.json`.
    pub fn last_sha(&self) -> Option<&str> {
        self.last_sha.as_deref()
    }

    /// Something wasn't saved; the next commit writes everything.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Adopts what [`load`] found: the accepted hash, a pending migration
    /// backup, and whether the files need rewriting.
    pub fn adopt(&mut self, loaded: &Loaded) {
        self.last_sha = loaded.accepted_sha.clone();
        self.pending_backup = loaded.migrated_from.clone();
        self.pending_base = loaded.base_to_write.clone();
        self.has_base = self.paths.history_base().is_file();
        self.dirty =
            loaded.needs_repair || self.pending_base.is_some() || self.pending_backup.is_some();
    }

    // ------------------------------------------------------------ commit

    /// Persists one outcome in the A§7.6 order: history `ops` (with the
    /// base on `Rewrite`), `qrec.json`, then the head. `project` says the
    /// outcome had `WriteProject`; history ops always rewrite `qrec.json`
    /// too, so the head never describes a file that wasn't written. Does
    /// nothing in a read-only store.
    pub fn commit(
        &mut self,
        prev: &State,
        next: &State,
        ops: &[&HistoryOp],
        project: bool,
    ) -> Result<(), CommitError> {
        if !self.writable || (ops.is_empty() && !project && !self.dirty) {
            return Ok(());
        }
        let result = self.commit_steps(prev, next, ops);
        self.dirty = result.is_err();
        result
    }

    fn commit_steps(
        &mut self,
        prev: &State,
        next: &State,
        ops: &[&HistoryOp],
    ) -> Result<(), CommitError> {
        self.write_history(prev, next, ops)?;
        self.crash_point(Step::History)?;
        self.write_project(&next.project)?;
        self.crash_point(Step::Project)?;
        self.write_head(&next.history, &next.project)?;
        self.crash_point(Step::Head)?;
        Ok(())
    }

    #[cfg(test)]
    fn crash_point(&self, step: Step) -> Result<(), CommitError> {
        match self.crash_after {
            Some(at) if at == step => Err(CommitError::Crashed(step)),
            _ => Ok(()),
        }
    }

    #[cfg(not(test))]
    fn crash_point(&self, _step: Step) -> Result<(), CommitError> {
        Ok(())
    }

    /// Step 1: the base (on `Rewrite`, or the first time a project gets a
    /// history), then one appended line or the whole file.
    fn write_history(
        &mut self,
        prev: &State,
        next: &State,
        ops: &[&HistoryOp],
    ) -> Result<(), CommitError> {
        let io_err = |what: String| {
            move |source| CommitError::Io {
                step: Step::History,
                what,
                source,
            }
        };
        for op in ops {
            if let HistoryOp::Rewrite { base, base_seq, .. } = op {
                self.pending_base = Some((*base_seq, (**base).clone()));
            }
        }
        if self.pending_base.is_none() && !self.has_base {
            // A project's first history: its base is the project before
            // the first entry (A§7.7 rebuilds from it).
            if next.history.entries.is_empty() {
                self.pending_base = Some((next.history.base_seq, next.project.clone()));
            } else if prev.history.entries.is_empty() && is_initialized(&prev.project) {
                self.pending_base = Some((prev.history.base_seq, prev.project.clone()));
            }
        }
        if let Some((base_seq, project)) = &self.pending_base {
            let path = self.paths.history_base();
            // On failure it stays pending for the next attempt.
            fsx::replace(
                &path,
                &format::base_bytes(*base_seq, project),
                fsx::Sync::File,
            )
            .map_err(io_err(format!("write {}", path.display())))?;
            self.pending_base = None;
            self.has_base = true;
        }
        let path = self.paths.history_file();
        let append = match ops {
            [HistoryOp::Append(entry)] if !self.dirty && !has_redo_tail(&prev.history) => {
                Some(entry)
            }
            _ => None,
        };
        match append {
            Some(entry) => fsx::append_durable(&path, &format::entry_line(entry))
                .map_err(io_err(format!("append to {}", path.display()))),
            None if !ops.is_empty() || self.dirty => fsx::replace(
                &path,
                &format::history_bytes(next.history.entries.iter()),
                fsx::Sync::File,
            )
            .map_err(io_err(format!("write {}", path.display()))),
            None => Ok(()),
        }
    }

    /// Step 2: `qrec.json` (after a pending migration backup).
    fn write_project(&mut self, project: &Project) -> Result<(), CommitError> {
        let io_err = |what: String| {
            move |source| CommitError::Io {
                step: Step::Project,
                what,
                source,
            }
        };
        if let Some((from, original)) = self.pending_backup.take() {
            let path = self.paths.migration_backup(from);
            if let Err(e) = fsx::write_new(&path, &original, ".json") {
                self.pending_backup = Some((from, original));
                return Err(io_err(format!("back up {}", path.display()))(e));
            }
        }
        let bytes = format::project_bytes(project);
        let path = self.paths.project_file();
        // Through `.qrec/qrec.json.tmp`: a folder being initialized shows
        // only `.qrec/` to other commands' folder rule (A§8.4).
        fsx::replace_via(
            &path,
            &self.paths.project_tmp(),
            &bytes,
            fsx::Sync::FileAndDir,
        )
        .map_err(io_err(format!("write {}", path.display())))?;
        self.last_sha = Some(format::sha(&bytes));
        Ok(())
    }

    /// Step 3: the commit record.
    fn write_head(&mut self, history: &History, project: &Project) -> Result<(), CommitError> {
        let bytes = format::head_bytes(
            history.cursor,
            history.base_seq,
            history.next_seq,
            self.last_sha.as_deref().unwrap_or_default(),
            project,
        );
        let path = self.paths.history_head();
        fsx::replace(&path, &bytes, fsx::Sync::File)
            .and_then(|()| fsx::sync_dir(&self.paths.qrec_dir()))
            .map_err(|source| CommitError::Io {
                step: Step::Head,
                what: format!("write {}", path.display()),
                source,
            })
    }

    /// Writes the history file, base, `qrec.json` and head from `state`
    /// (repairing what [`load`] found, or committing an `Init`).
    pub fn write_all(&mut self, state: &State) -> Result<(), CommitError> {
        if !self.writable {
            return Ok(());
        }
        self.dirty = true;
        let result = self.commit_steps(state, state, &[]);
        self.dirty = result.is_err();
        result
    }

    // ------------------------------------------------------------- disk

    /// Reads `qrec.json` as it is now: parsed, migrated and checked.
    pub fn read_disk(&self, check: Check) -> DiskFile {
        let path = self.paths.project_file();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return DiskFile {
                    sha: None,
                    project: Err("qrec.json was deleted".to_owned()),
                    migrated_from: None,
                };
            }
            Err(e) => {
                return DiskFile {
                    sha: Some(String::new()),
                    project: Err(format!("can't read qrec.json: {e}")),
                    migrated_from: None,
                };
            }
        };
        let sha = Some(format::sha(&bytes));
        let parsed = std::str::from_utf8(&bytes)
            .map_err(|e| CoreError::invalid(format!("qrec.json is not UTF-8: {e}")))
            .and_then(format::parse_project_text)
            .and_then(|m| check(&m.project).map(|()| m));
        match parsed {
            Ok(m) => DiskFile {
                sha,
                migrated_from: (m.from_version < qrec_core::model::CURRENT_VERSION)
                    .then(|| (m.from_version, bytes.clone())),
                project: Ok(Box::new(m.project)),
            },
            Err(e) => DiskFile {
                sha,
                project: Err(e.message),
                migrated_from: None,
            },
        }
    }

    /// `Some` when `qrec.json` isn't what this host last wrote or accepted
    /// (A§8.9). A missing file is a change only if the host had one.
    pub fn disk_changed(&self, check: Check) -> Option<DiskFile> {
        let path = self.paths.project_file();
        let current = match fs::read(&path) {
            Ok(bytes) => Some(format::sha(&bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(_) => Some(String::new()),
        };
        if current == self.last_sha {
            return None;
        }
        let file = self.read_disk(check);
        // The file may have changed again between the two reads; what was
        // parsed is what counts.
        (file.sha != self.last_sha).then_some(file)
    }

    /// The host has seen `file`: later checks compare against it.
    pub fn accept_disk(&mut self, file: &DiskFile) {
        self.last_sha = file.sha.clone();
        if let Some(backup) = &file.migrated_from {
            self.pending_backup = Some(backup.clone());
        }
    }

    /// Moves an invalid `qrec.json` to `.qrec/rejected/qrec-<stamp>.json`
    /// (never overwriting) before the host saves facts over it (A§7.7).
    pub fn reject_disk_file(&mut self, stamp: &str) -> io::Result<Option<PathBuf>> {
        let from = self.paths.project_file();
        if fs::symlink_metadata(&from).is_err() {
            return Ok(None);
        }
        fs::create_dir_all(self.paths.rejected_dir())?;
        let moved = fsx::move_new(&from, &self.paths.rejected_file(stamp), ".json")?;
        fsx::sync_dir(self.paths.root())?;
        self.last_sha = None;
        Ok(Some(moved))
    }

    /// Copies `qrec.json` to `.qrec/qrec.corrupt-<stamp>.json` before a
    /// restore rewrites it (A§7.7). `None` when there is no file.
    pub fn copy_disk_file_aside(&self, stamp: &str) -> io::Result<Option<PathBuf>> {
        let bytes = match fs::read(self.paths.project_file()) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        fsx::write_new(&self.paths.corrupt_copy(stamp), &bytes, ".json").map(Some)
    }

    // ------------------------------------------------------- directories

    /// `Effect::CreateTakeDir`: `mkdir` (not `-p`) of `takes/<take>`; an
    /// existing folder is an error (it would mix two takes' files). Creates
    /// `takes/` itself when missing.
    pub fn create_take_dir(&self, relative: &str) -> Result<PathBuf, String> {
        let path = self.paths.resolve(relative).map_err(|e| e.message)?;
        if path.parent() != Some(self.paths.takes_dir().as_path()) {
            return Err(format!("{relative} is not a folder in takes/"));
        }
        if !self.writable {
            return Err("the project is open read-only".to_owned());
        }
        self.ensure_takes_dir()
            .map_err(|e| format!("can't create takes/: {e}"))?;
        match fs::create_dir(&path) {
            Ok(()) => Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(format!(
                "{relative} already exists; a take never reuses a folder"
            )),
            Err(e) => Err(format!("can't create {relative}: {e}")),
        }
    }

    /// `takes/` (one level).
    pub fn ensure_takes_dir(&self) -> io::Result<()> {
        match fs::create_dir(self.paths.takes_dir()) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
            _ => Ok(()),
        }
    }

    // --------------------------------------------------------------- ui

    /// `Effect::WriteUi`: writes now, or marks the UI pending when the last
    /// write was less than [`UI_WRITE_INTERVAL`] ago.
    pub fn want_ui_write(&mut self, ui: &UiState, now: Instant) -> io::Result<()> {
        if !self.writable {
            return Ok(());
        }
        match self.ui_last {
            Some(last) if now.saturating_duration_since(last) < UI_WRITE_INTERVAL => {
                self.ui_pending = true;
                Ok(())
            }
            _ => self.write_ui(ui, now),
        }
    }

    /// When a pending UI write is due.
    pub fn ui_deadline(&self) -> Option<Instant> {
        self.ui_pending.then(|| {
            self.ui_last
                .map_or_else(Instant::now, |t| t + UI_WRITE_INTERVAL)
        })
    }

    /// Writes a pending UI write if it is due (or `force`).
    pub fn flush_ui(&mut self, ui: &UiState, now: Instant, force: bool) -> io::Result<()> {
        match self.ui_deadline() {
            Some(due) if force || now >= due => self.write_ui(ui, now),
            _ => Ok(()),
        }
    }

    fn write_ui(&mut self, ui: &UiState, now: Instant) -> io::Result<()> {
        self.ui_pending = false;
        self.ui_last = Some(now);
        let mut bytes = serde_json::to_vec(ui).map_err(io::Error::other)?;
        bytes.push(b'\n');
        fsx::replace(&self.paths.ui_file(), &bytes, fsx::Sync::None)
    }
}

/// The history has entries after its cursor (a redo tail) on disk.
fn has_redo_tail(history: &History) -> bool {
    history
        .entries
        .last()
        .is_some_and(|e| e.seq > history.cursor)
}

/// The takes on disk: every `takes/<take>` folder name that parses as a take id.
pub fn take_dirs(paths: &ProjectPaths) -> Vec<TakeId> {
    let Ok(entries) = fs::read_dir(paths.takes_dir()) else {
        return Vec::new();
    };
    let mut takes: Vec<TakeId> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect();
    takes.sort();
    takes
}

/// Whether `path` exists without following a final symlink.
pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}
