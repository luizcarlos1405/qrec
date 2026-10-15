//! Opening a project's files (A§7.6 "On open", A§7.7). Owner: store.
//!
//! With the commit record (`history.head.json`):
//! - history lines with `seq ≥ nextSeq` (written, never committed) and a
//!   torn last line are dropped; a bad line elsewhere is `Invalid` naming
//!   its line number;
//! - `sha256(qrec.json) == head.projectSha256` ⇒ clean; otherwise a valid
//!   file is an external edit of `head.project` (this also covers a crash
//!   between writing `qrec.json` and the head) for the core to merge, and
//!   an invalid one fails the open with the restore hint.
//!
//! Without it (a project written by hand or by tests, or a lost head): the
//! file is the project, the history lines are kept and the cursor is the
//! entry whose `afterHash` matches the file.
//!
//! Recovery mode (`qrec history`, `qrec history restore`) never loads an
//! unusable `qrec.json`: it uses `head.project`, else a valid `qrec.json`,
//! else the base replayed with the forward patches, with the takes found
//! under `takes/` added back for the Recover probe. A missing or invalid
//! file only sets `disk_invalid`; a valid one that differs from the head
//! is an external edit like on any open (SPEC §8.1: it becomes an
//! "External edit" entry first, so a restore can be undone back to it).

use std::fs;
use std::io;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use qrec_core::hash::json_hash;
use qrec_core::history::HistoryEntry;
use qrec_core::history::ops::{apply_patch_with, unchecked};
use qrec_core::history::view::undoable_view;
use qrec_core::model::{Project, Stream, StreamKind, Take, TakeStatus};
use qrec_core::state::{History, UiState};
use qrec_core::{CoreError, ErrorCode, TakeId};
use serde_json::Value;

use super::Check;
use super::format::{self, Head, ParsedHistory};
use crate::error::ShellError;
use crate::paths::ProjectPaths;

/// How to open.
#[derive(Debug, Clone, Copy)]
pub struct LoadOpts {
    /// Use the head and the history; never load `qrec.json` as the project.
    pub recovery: bool,
    /// How a project from disk is checked (A§2.3).
    pub check: Check,
}

/// What was found.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    /// The state's project: the file's, the head's (when the file is an
    /// edit to merge) or a rebuilt one.
    pub project: Project,
    pub history: History,
    pub ui: UiState,
    /// The file differs from the head: the session feeds
    /// `ProjectChangedOnDisk(external)` to the core right after opening.
    pub external: Option<Result<Box<Project>, String>>,
    /// `runtime.disk_invalid` to start with (recovery mode).
    pub disk_invalid: Option<String>,
    /// The `qrec.json` hash the store starts from (later changes are edits).
    pub accepted_sha: Option<String>,
    /// The file was an older schema: version and original bytes to back up.
    pub migrated_from: Option<(u32, Vec<u8>)>,
    /// The files don't match this state: the first write rewrites them all.
    pub needs_repair: bool,
    /// A base the first write must store (`history.base.json`).
    pub base_to_write: Option<(u64, Project)>,
    /// Takes found under `takes/` and added back (`recording`) to a rebuilt project.
    pub readded: Vec<TakeId>,
    /// What was repaired or assumed, for the log.
    pub notes: Vec<String>,
}

/// Loads the project in `paths` (which has a `qrec.json` or, in recovery
/// mode, at least a history to rebuild from).
pub fn load(paths: &ProjectPaths, opts: &LoadOpts) -> Result<Loaded, ShellError> {
    let mut notes = Vec::new();
    let head = read_head(paths, &mut notes)?;
    let base = read_base(paths, &mut notes)?;
    let parsed = read_history(paths)?;
    let ui = read_ui(paths, &mut notes);
    let file = read_optional(&paths.project_file())
        .map_err(|e| ShellError::io(format!("read {}", paths.project_file().display()), e))?;
    let file_sha = file.as_deref().map(format::sha);

    if opts.recovery {
        if let Some(head) = head {
            let clean = file_sha.as_deref() == Some(head.project_sha256.as_str());
            let mut loaded = with_head(head, base, parsed, ui, notes);
            match file.as_deref() {
                None => loaded.disk_invalid = Some("qrec.json is missing".to_owned()),
                Some(_) if clean => {}
                Some(bytes) => match parse_file(bytes, opts.check) {
                    // A valid hand edit is merged first, as on any open.
                    Ok(migrated) => {
                        loaded.migrated_from = migrated_original(&migrated, Some(bytes));
                        if migrated.project != loaded.project {
                            loaded.external = Some(Ok(Box::new(migrated.project)));
                        }
                    }
                    Err(e) => loaded.disk_invalid = Some(file_problem(&e)),
                },
            }
            loaded.accepted_sha = file_sha;
            return Ok(loaded);
        }
        let why = match file.as_deref().map(|bytes| parse_file(bytes, opts.check)) {
            Some(Ok(migrated)) => {
                let original = migrated_original(&migrated, file.as_deref());
                return Ok(without_head(
                    migrated.project,
                    original,
                    base,
                    parsed,
                    ui,
                    notes,
                    file_sha,
                ));
            }
            Some(Err(e)) => file_problem(&e),
            None => "qrec.json is missing".to_owned(),
        };
        let mut loaded = rebuild(paths, base, parsed, ui, notes)?;
        loaded.disk_invalid = Some(why);
        loaded.accepted_sha = file_sha;
        return Ok(loaded);
    }

    let restore_seq = head.as_ref().map(|h| h.cursor).or_else(|| {
        let base_seq = base.as_ref().map(|(seq, _)| seq.unwrap_or(0));
        parsed.entries.last().map(|e| e.seq).or(base_seq)
    });
    let Some(bytes) = file else {
        return Err(missing_file(restore_seq).into());
    };
    let migrated = parse_file(&bytes, opts.check).map_err(|e| invalid_file(&e, restore_seq))?;
    let original = migrated_original(&migrated, Some(&bytes));
    let Some(head) = head else {
        return Ok(without_head(
            migrated.project,
            original,
            base,
            parsed,
            ui,
            notes,
            file_sha,
        ));
    };
    let file_project = migrated.project;
    let clean =
        file_sha.as_deref() == Some(head.project_sha256.as_str()) || file_project == head.project;
    let mut loaded = with_head(head, base, parsed, ui, notes);
    loaded.external = (!clean).then(|| Ok(Box::new(file_project)));
    loaded.accepted_sha = file_sha;
    loaded.migrated_from = original;
    Ok(loaded)
}

/// `seen_clip_ids` from the base snapshot (or the project when there is none).
fn with_seen_ids(mut history: History, base: Option<&Project>, project: &Project) -> History {
    history.rebuild_seen_clip_ids(base.unwrap_or(project));
    history
}

fn missing_file(restore_seq: Option<u64>) -> CoreError {
    let hint = match restore_seq {
        Some(seq) => format!("rebuild it from the history with `qrec history restore {seq} --yes`"),
        None => "restore it from a backup".to_owned(),
    };
    CoreError::invalid("qrec.json is missing").with_hint(hint)
}

fn invalid_file(e: &CoreError, restore_seq: Option<u64>) -> CoreError {
    let hint = match restore_seq {
        Some(seq) => format!(
            "fix it, or rebuild it from the history with `qrec history restore {seq} --yes`"
        ),
        None => "fix it; there is no history to rebuild it from".to_owned(),
    };
    let mut err = CoreError::new(ErrorCode::Invalid, file_problem(e)).with_hint(hint);
    err.details = e.details.clone();
    err
}

/// Why `qrec.json` can't be loaded, starting with `qrec.json`.
fn file_problem(e: &CoreError) -> String {
    e.message.strip_prefix("qrec.json ").map_or_else(
        || format!("qrec.json is invalid: {}", e.message),
        |rest| format!("qrec.json {rest}"),
    )
}

fn parse_file(bytes: &[u8], check: Check) -> Result<qrec_core::migrate::Migrated, CoreError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| CoreError::invalid(format!("qrec.json is not UTF-8: {e}")))?;
    let migrated = format::parse_project_text(text)?;
    check(&migrated.project)?;
    Ok(migrated)
}

fn migrated_original(
    migrated: &qrec_core::migrate::Migrated,
    bytes: Option<&[u8]>,
) -> Option<(u32, Vec<u8>)> {
    (migrated.from_version < qrec_core::model::CURRENT_VERSION)
        .then(|| (migrated.from_version, bytes.unwrap_or_default().to_vec()))
}

// ------------------------------------------------------------ with a head

fn with_head(
    head: Head,
    base: Option<(Option<u64>, Project)>,
    parsed: ParsedHistory,
    ui: UiState,
    mut notes: Vec<String>,
) -> Loaded {
    let base_seq = base
        .as_ref()
        .and_then(|(seq, _)| *seq)
        .unwrap_or(head.base_seq);
    if base_seq != head.base_seq {
        notes.push(format!(
            "history base {base_seq} differs from the commit record's {}; a history rewrite didn't finish",
            head.base_seq
        ));
    }
    let mut needs_repair = parsed.dropped_torn;
    if parsed.dropped_torn {
        notes.push("dropped a torn last line of .qrec/history.jsonl".to_owned());
    }
    let mut entries = parsed.entries;
    let before = entries.len();
    entries.retain(|e| e.seq < head.next_seq && e.seq > base_seq);
    if entries.len() != before {
        needs_repair = true;
        notes.push(format!(
            "dropped {} history entries that were never committed (or are folded into the base)",
            before - entries.len()
        ));
    }
    let next_seq = head
        .next_seq
        .max(entries.last().map_or(0, |e| e.seq + 1))
        .max(base_seq + 1);
    let mut history = History {
        entries: Arc::new(entries),
        cursor: head.cursor,
        base_seq,
        next_seq,
        seen_clip_ids: Arc::default(),
    };
    let mut base_to_write = None;
    let cursor_known = history.cursor == history.base_seq
        || history.entries.iter().any(|e| e.seq == history.cursor);
    if !cursor_known {
        notes.push(format!(
            "the undo history lost its current entry {}; it starts over from the current project",
            history.cursor
        ));
        history.entries = Arc::default();
        history.base_seq = history.cursor;
        base_to_write = Some((history.cursor, head.project.clone()));
        needs_repair = true;
    }
    let base_project = match &base_to_write {
        Some((_, project)) => Some(project),
        None => base.as_ref().map(|(_, project)| project),
    };
    let history = with_seen_ids(history, base_project, &head.project);
    Loaded {
        project: head.project,
        history,
        ui,
        external: None,
        disk_invalid: None,
        accepted_sha: None,
        migrated_from: None,
        needs_repair,
        base_to_write,
        readded: Vec::new(),
        notes,
    }
}

// ---------------------------------------------------------- without a head

fn without_head(
    project: Project,
    migrated_from: Option<(u32, Vec<u8>)>,
    base: Option<(Option<u64>, Project)>,
    parsed: ParsedHistory,
    ui: UiState,
    mut notes: Vec<String>,
    file_sha: Option<String>,
) -> Loaded {
    let (base_seq, base_project) = match base {
        Some((seq, project)) => (seq.unwrap_or(0), Some(project)),
        None => (0, None),
    };
    if parsed.dropped_torn {
        notes.push("dropped a torn last line of .qrec/history.jsonl".to_owned());
    }
    let mut entries = parsed.entries;
    entries.retain(|e| e.seq > base_seq);
    let next_seq = entries.last().map_or(0, |e| e.seq + 1).max(base_seq + 1);
    let view_hash = json_hash(&undoable_view(&project));
    let cursor = if entries.is_empty() {
        base_seq
    } else if let Some(entry) = entries.iter().rev().find(|e| e.after_hash == view_hash) {
        entry.seq
    } else if base_project
        .as_ref()
        .is_some_and(|b| json_hash(&undoable_view(b)) == view_hash)
    {
        base_seq
    } else {
        let last = entries.last().map_or(base_seq, |e| e.seq);
        notes.push(format!(
            "no commit record and qrec.json matches no history entry; assuming entry {last} (undo refuses if they differ)"
        ));
        last
    };
    if !entries.is_empty() || base_project.is_some() {
        notes.push(
            "no .qrec/history.head.json: rebuilt the commit record from qrec.json".to_owned(),
        );
    }
    let base_to_write = match (&base_project, entries.is_empty()) {
        (None, true) => Some((base_seq, project.clone())),
        _ => None,
    };
    if base_project.is_none() && !entries.is_empty() {
        notes.push(
            "no .qrec/history.base.json: the project can't be rebuilt from the history".to_owned(),
        );
    }
    let history = History {
        entries: Arc::new(entries),
        cursor,
        base_seq,
        next_seq,
        seen_clip_ids: Arc::default(),
    };
    let history = with_seen_ids(history, base_project.as_ref(), &project);
    Loaded {
        project,
        history,
        ui,
        external: None,
        disk_invalid: None,
        accepted_sha: file_sha,
        migrated_from,
        needs_repair: true,
        base_to_write,
        readded: Vec::new(),
        notes,
    }
}

// ----------------------------------------------------------------- rebuild

/// No head and no usable `qrec.json`: the base replayed with every forward
/// patch that still applies, plus the takes found under `takes/`.
///
/// Take facts aren't in the patches (A§7.2), and the base usually predates
/// the takes, so each take on disk joins the pool right before the first
/// entry that uses it (its clip, or its scene), with the scene the project
/// had then: what `begin_take` copied when it was recorded, so later
/// patches of that scene still apply. It comes back `recording`, without
/// media facts: the Recover probe finalizes it (A§5.3), and until then the
/// project doesn't validate (a restore waits for the probes, A§7.7).
fn rebuild(
    paths: &ProjectPaths,
    base: Option<(Option<u64>, Project)>,
    parsed: ParsedHistory,
    ui: UiState,
    mut notes: Vec<String>,
) -> Result<Loaded, ShellError> {
    let Some((base_seq, base_project)) = base else {
        return Err(CoreError::invalid(
            "nothing to recover from: qrec.json is unusable and there is no .qrec/history.head.json or .qrec/history.base.json",
        )
        .with_hint("restore qrec.json from a backup")
        .into());
    };
    let base_seq = base_seq.unwrap_or(0);
    let mut project = base_project.clone();
    let mut on_disk = takes_on_disk(paths, &project);
    let mut readded: Vec<TakeId> = Vec::new();
    let mut kept: Vec<HistoryEntry> = Vec::new();
    for entry in parsed.entries.into_iter().filter(|e| e.seq > base_seq) {
        let (used, later): (Vec<FoundTake>, Vec<FoundTake>) = on_disk
            .into_iter()
            .partition(|take| mentions(&entry, &take.id));
        on_disk = later;
        for take in &used {
            readd(&mut project, take);
            readded.push(take.id.clone());
        }
        match apply_patch_with(&project, &entry.forward, unchecked) {
            Ok(mut next) => {
                // The entry that first puts a take's clip on the timeline is
                // its placement (discard-last finds it by `placed`, A§5.5).
                for take in &used {
                    let placed = next.timeline.clips.iter().any(|c| c.take == take.id);
                    if let Some(t) = next.takes.get_mut(&take.id) {
                        t.placed = placed;
                    }
                }
                project = next;
                kept.push(entry);
            }
            Err(e) => {
                notes.push(format!(
                    "rebuilding stopped at history entry {} (\"{}\"): {}",
                    entry.seq, entry.label, e.message
                ));
                break;
            }
        }
    }
    // Takes no kept entry uses: in the bin, with the latest scene.
    for take in &on_disk {
        readd(&mut project, take);
        readded.push(take.id.clone());
    }
    readded.sort();
    if !readded.is_empty() {
        notes.push(format!(
            "added back {} take(s) found under takes/: {}",
            readded.len(),
            readded
                .iter()
                .map(TakeId::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let cursor = kept.last().map_or(base_seq, |e| e.seq);
    let next_seq = (cursor + 1).max(base_seq + 1);
    notes.push(format!(
        "rebuilt the project from the history (entry {cursor})"
    ));
    let history = History {
        entries: Arc::new(kept),
        cursor,
        base_seq,
        next_seq,
        seen_clip_ids: Arc::default(),
    };
    let history = with_seen_ids(history, Some(&base_project), &project);
    Ok(Loaded {
        project,
        history,
        ui,
        external: None,
        disk_invalid: None,
        accepted_sha: None,
        migrated_from: None,
        needs_repair: true,
        base_to_write: None,
        readded,
        notes,
    })
}

/// A `takes/take-N` folder the project doesn't know.
struct FoundTake {
    id: TakeId,
    /// One per media file (`L1-….mkv` is video, `A1-….mka` audio), by id.
    streams: Vec<Stream>,
    recorded_at: Option<DateTime<Utc>>,
}

/// The take folders `project` doesn't have, with their media files.
fn takes_on_disk(paths: &ProjectPaths, project: &Project) -> Vec<FoundTake> {
    super::take_dirs(paths)
        .into_iter()
        .filter(|id| !project.takes.contains_key(id))
        .map(|id| {
            let dir = paths.take_dir(&id);
            let mut streams: Vec<Stream> = fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let kind = match name.rsplit_once('.')?.1 {
                        "mkv" => StreamKind::Video,
                        "mka" => StreamKind::Audio,
                        _ => return None,
                    };
                    let stream_id = name.split('-').next()?.to_owned();
                    Some(Stream {
                        id: stream_id,
                        kind,
                        file: format!("takes/{id}/{name}"),
                        width: None,
                        height: None,
                        sample_rate: None,
                        channels: None,
                        start_offset_ns: 0,
                        duration_ns: None,
                        frames: None,
                        fps: None,
                    })
                })
                .collect();
            streams.sort_by(|a, b| a.id.cmp(&b.id));
            let recorded_at = fs::metadata(&dir)
                .and_then(|m| m.modified())
                .map(DateTime::<Utc>::from)
                .ok();
            FoundTake {
                id,
                streams,
                recorded_at,
            }
        })
        .collect()
}

/// Adds `found` to the pool as a take still `recording` (for the Recover
/// probe to finalize), with the project's scene as it is now, like
/// `begin_take` copies it.
fn readd(project: &mut Project, found: &FoundTake) {
    project.next_take_number = project.next_take_number.max(found.id.number() + 1);
    project.takes.insert(
        found.id.clone(),
        Take {
            status: TakeStatus::Recording,
            recorded_at: found.recorded_at.unwrap_or(project.created_at),
            duration_frames: 0,
            scene: project.scene.clone(),
            streams: found.streams.clone(),
            error: None,
            placed: false,
            replaced: Vec::new(),
        },
    );
}

/// Whether redoing `entry` uses `take`: an op under `/takes/<take>`, or a
/// value naming it (a clip placed on it). Patches never hold take facts
/// (A§7.2), so this is how the first entry that needs the take is found.
fn mentions(entry: &HistoryEntry, take: &TakeId) -> bool {
    fn walk(value: &Value, id: &str, prefix: &str) -> bool {
        match value {
            Value::String(s) => {
                s == id
                    || s.strip_prefix(prefix)
                        .is_some_and(|rest| rest == id || rest.starts_with(&format!("{id}/")))
            }
            Value::Array(items) => items.iter().any(|v| walk(v, id, prefix)),
            Value::Object(members) => members.values().any(|v| walk(v, id, prefix)),
            _ => false,
        }
    }
    serde_json::to_value(&entry.forward).is_ok_and(|v| walk(&v, take.as_str(), "/takes/"))
}

// ------------------------------------------------------------------- files

fn read_optional(path: &std::path::Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn read_head(paths: &ProjectPaths, notes: &mut Vec<String>) -> Result<Option<Head>, ShellError> {
    let path = paths.history_head();
    let Some(bytes) =
        read_optional(&path).map_err(|e| ShellError::io(format!("read {}", path.display()), e))?
    else {
        return Ok(None);
    };
    match serde_json::from_slice::<Head>(&bytes) {
        Ok(head) => Ok(Some(head)),
        Err(e) => {
            notes.push(format!(
                "ignored an unreadable .qrec/history.head.json: {e}"
            ));
            Ok(None)
        }
    }
}

fn read_base(
    paths: &ProjectPaths,
    notes: &mut Vec<String>,
) -> Result<Option<(Option<u64>, Project)>, ShellError> {
    let path = paths.history_base();
    let Some(bytes) =
        read_optional(&path).map_err(|e| ShellError::io(format!("read {}", path.display()), e))?
    else {
        return Ok(None);
    };
    let base = format::parse_base(&bytes);
    if base.is_none() {
        notes.push("ignored an unreadable .qrec/history.base.json".to_owned());
    }
    Ok(base)
}

fn read_history(paths: &ProjectPaths) -> Result<ParsedHistory, ShellError> {
    let path = paths.history_file();
    let bytes = read_optional(&path)
        .map_err(|e| ShellError::io(format!("read {}", path.display()), e))?
        .unwrap_or_default();
    Ok(format::parse_history(&bytes)?)
}

fn read_ui(paths: &ProjectPaths, notes: &mut Vec<String>) -> UiState {
    match read_optional(&paths.ui_file()) {
        Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            notes.push(format!("ignored an unreadable .qrec/ui.json: {e}"));
            UiState::default()
        }),
        _ => UiState::default(),
    }
}
