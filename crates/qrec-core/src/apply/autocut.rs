//! Autocut commands (A§5.7). Owner: autocut.
//!
//! `Autocut` snapshots its parameters and targets (`AutocutTarget`: the
//! clip as a `ClipRef`, the track, the `SilenceKey` of its audio file from
//! `ctx.facts.files`) into a `JobKind::Autocut` job and emits one
//! `DetectSilence` per key missing from `runtime.silence`. The job completes
//! in `on_event::autocut` — or right here when every key is already known.
//! Clips that can't be analyzed (no such track, missing media, no file) are
//! skipped with a reason; until the shared job type has a field for them,
//! they ride in the job's `cmd` (see `on_event::autocut::with_skipped`).
//!
//! `start_auto_run` is the same flow for `autocut.autoRunAfterTake`, called
//! by the record owner right after a take is placed.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::apply::Outcome;
use crate::autocut::{analysis_source, db_label};
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::history::commit;
use crate::ids::{ClipId, ReqId, TrackId};
use crate::media_argv::silence_argv;
use crate::model::{AutocutSettings, Clip, Project, Take};
use crate::on_event::autocut::{self as events, Snapshot};
use crate::protocol::command::{AutocutOverrides, AutocutPatch, ClipScope, Command, Nullable};
use crate::protocol::effect::{Effect, LogLevel};
use crate::protocol::reply::{AutocutSkipView, CmdResult};
use crate::state::{AutocutParams, AutocutTarget, ClipRef, Job, JobKind, SilenceKey, State};

pub use crate::on_event::autocut::apply_ready_autocuts;

/// Thresholds outside −90…0 dB are refused (A§2.3).
pub const THRESHOLD_DB_RANGE: std::ops::RangeInclusive<f64> = -90.0..=0.0;
/// `minSilenceMs`, `paddingMs` and `minKeepMs` are at most a minute (A§2.3).
pub const MAX_MS: u32 = 60_000;

/// `qrec autocut [--clips …|--all] [--dry-run] …`.
pub fn autocut(
    state: &State,
    ctx: &Ctx,
    scope: ClipScope,
    overrides: AutocutOverrides,
    dry_run: bool,
) -> Result<Outcome, CoreError> {
    let params = effective_params(&state.project.autocut, &overrides)?;
    // A job that finished analyzing while a recording ran and was never
    // applied (the recording ended without placing a take) goes first.
    let (base, mut effects) = if dry_run {
        (state.clone(), Vec::new())
    } else {
        events::flush_ready(state, ctx)
    };
    let clips = clips_in_scope(&base.project, &scope)?;
    if let Some(track) = &overrides.track {
        let takes = clips.iter().filter_map(|c| base.project.take(&c.take));
        ensure_track_known(&base.project, track, takes)?;
    }
    let cmd = Command::Autocut {
        scope,
        overrides: overrides.clone(),
        dry_run,
    }
    .to_json();
    let request = Request {
        cmd,
        clips,
        params,
        track: overrides.track,
        dry_run,
        req: ctx.req.clone(),
    };
    Ok(match start(&base, ctx, request) {
        Started::Done(done) => {
            effects.extend(done.effects);
            Outcome::new(done.state, effects, Some(done.reply))
        }
        Started::Pending { state, effects: more } => {
            effects.extend(more);
            Outcome::new(state, effects, None)
        }
    })
}

/// Starts autocut on `clip` for `autocut.autoRunAfterTake` (SPEC §5.3 step
/// 6): the record owner calls it right after placing a take. A no-op unless
/// the setting is on. It becomes its own history entry (cmd
/// `{"event":"autoRunAfterTake",…}`) when the analysis completes; there is
/// no requester, so the outcome is told with a toast.
pub fn start_auto_run(state: &State, ctx: &Ctx, clip: &ClipId) -> Outcome {
    let settings = &state.project.autocut;
    if !settings.auto_run_after_take {
        return Outcome::unchanged(state);
    }
    let Some(target) = state.project.clip(clip).cloned() else {
        return Outcome::new(
            state.clone(),
            vec![log(
                LogLevel::Debug,
                format!("autocut after take: clip {clip} is not on the timeline"),
            )],
            None,
        );
    };
    let params = match effective_params(settings, &AutocutOverrides::default()) {
        Ok(params) => params,
        Err(err) => {
            let toast = events::failure_toast(&err);
            return Outcome::new(state.clone(), vec![Effect::Notify(toast)], None);
        }
    };
    let request = Request {
        cmd: json!({"event": "autoRunAfterTake", "take": target.take, "clip": target.id}),
        clips: vec![target],
        params,
        track: None,
        dry_run: false,
        req: None,
    };
    match start(state, ctx, request) {
        Started::Done(done) => {
            let (state, effects) = done.deliver(None);
            Outcome::new(state, effects, None)
        }
        Started::Pending { state, effects } => Outcome::new(state, effects, None),
    }
}

/// `qrec autocut settings …`: edits `project.autocut` (undoable).
pub fn autocut_settings(
    state: &State,
    ctx: &Ctx,
    patch: AutocutPatch,
) -> Result<Outcome, CoreError> {
    let empty = patch.auto_run_after_take.is_none()
        && patch.track.is_keep()
        && patch.threshold_db.is_none()
        && patch.min_silence_ms.is_none()
        && patch.padding_ms.is_none()
        && patch.min_keep_ms.is_none();
    if empty {
        return Err(CoreError::usage("nothing to change in the autocut settings").with_hint(
            "pass --auto-run, --track, --threshold-db, --min-silence-ms, --padding-ms or --min-keep-ms",
        ));
    }
    if let Nullable::Set(track) = &patch.track {
        // Any take may use the setting later: the track must exist somewhere.
        ensure_track_known(&state.project, track, state.project.takes.values())?;
    }
    let mut next = state.clone();
    let settings = &mut next.project.autocut;
    if let Some(on) = patch.auto_run_after_take {
        settings.auto_run_after_take = on;
    }
    settings.track = patch.track.clone().apply(settings.track.take());
    if let Some(db) = patch.threshold_db {
        settings.threshold_db = db;
    }
    if let Some(ms) = patch.min_silence_ms {
        settings.min_silence_ms = ms;
    }
    if let Some(ms) = patch.padding_ms {
        settings.padding_ms = ms;
    }
    if let Some(ms) = patch.min_keep_ms {
        settings.min_keep_ms = ms;
    }
    check_params(&params_of(settings))?;

    let changes = settings_changes(&state.project.autocut, &next.project.autocut);
    let label = format!("Autocut settings: {}", changes.join(", "));
    let view = next.project.autocut.clone();
    let committed = commit(
        state,
        ctx,
        label.clone(),
        Command::AutocutSettings { patch }.to_json(),
        next,
    );
    let message = if committed.seq.is_some() {
        label
    } else {
        "Autocut settings unchanged".to_owned()
    };
    Ok(Outcome::done(committed, CmdResult::new(message).with_result(&view)))
}

// ------------------------------------------------------------- the job

/// What a run analyzes, before the targets are resolved.
struct Request {
    /// The history entry's `cmd`.
    cmd: Value,
    /// In timeline order.
    clips: Vec<Clip>,
    params: AutocutParams,
    /// The override track, if any.
    track: Option<TrackId>,
    dry_run: bool,
    req: Option<ReqId>,
}

enum Started {
    /// Every key was known: the run completed (or nothing was analyzable).
    Done(events::Finished),
    /// A job waits for `DetectSilence` results.
    Pending { state: State, effects: Vec<Effect> },
}

/// Resolves targets, then completes at once or creates the job.
fn start(state: &State, ctx: &Ctx, request: Request) -> Started {
    let project = &state.project;
    let params = request.params;
    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    // Keys in first-use order, with the file each one analyzes.
    let mut files: Vec<(SilenceKey, String)> = Vec::new();
    for clip in &request.clips {
        let source = project
            .take(&clip.take)
            .ok_or_else(|| format!("take {} doesn't exist", clip.take))
            .and_then(|take| analysis_source(project, &clip.take, take, request.track.as_ref()));
        let (track, stream) = match source {
            Ok(found) => found,
            Err(reason) => {
                skipped.push(AutocutSkipView {
                    clip: clip.id.clone(),
                    reason,
                });
                continue;
            }
        };
        let Some(fact) = ctx.facts.files.get(&stream.file) else {
            skipped.push(AutocutSkipView {
                clip: clip.id.clone(),
                reason: format!("audio file {} not found", stream.file),
            });
            continue;
        };
        let key = SilenceKey::compute(
            &stream.file,
            fact.bytes,
            fact.mtime_ns,
            params.threshold_db,
            params.min_silence_ms,
        );
        if !files.iter().any(|(k, _)| *k == key) {
            files.push((key.clone(), stream.file.clone()));
        }
        targets.push(AutocutTarget {
            clip: ClipRef::of(clip),
            track: track.id.clone(),
            key,
        });
    }

    let missing: Vec<&(SilenceKey, String)> = files
        .iter()
        .filter(|(key, _)| !state.runtime.silence.contains_key(key))
        .collect();
    if missing.is_empty() {
        let snapshot = Snapshot {
            cmd: request.cmd,
            targets,
            params,
            dry_run: request.dry_run,
            skipped,
        };
        return Started::Done(events::finish(state, ctx, snapshot));
    }

    let mut next = state.clone();
    let job = next.runtime.alloc_job_id();
    let mut effects: Vec<Effect> = missing
        .iter()
        .map(|(key, file)| Effect::DetectSilence {
            job,
            key: key.clone(),
            argv: silence_argv(file, params.threshold_db, params.min_silence_ms),
        })
        .collect();
    let pending: BTreeSet<SilenceKey> = missing.iter().map(|(key, _)| key.clone()).collect();
    let kind = JobKind::Autocut {
        cmd: events::with_skipped(request.cmd, &skipped),
        targets,
        params,
        dry_run: request.dry_run,
        pending,
        ready: false,
    };
    let entry = Job {
        id: job,
        kind,
        req: request.req,
        started: ctx.now,
    };
    if let Some(progress) = events::progress_of(&entry, ctx) {
        effects.push(progress);
    }
    next.runtime.jobs.insert(job, entry);
    Started::Pending {
        state: next,
        effects,
    }
}

// ------------------------------------------------------------ helpers

/// The project settings with the command's overrides, checked against A§2.3.
fn effective_params(
    settings: &AutocutSettings,
    overrides: &AutocutOverrides,
) -> Result<AutocutParams, CoreError> {
    let base = params_of(settings);
    let params = AutocutParams {
        threshold_db: overrides.threshold_db.unwrap_or(base.threshold_db),
        min_silence_ms: overrides.min_silence_ms.unwrap_or(base.min_silence_ms),
        padding_ms: overrides.padding_ms.unwrap_or(base.padding_ms),
        min_keep_ms: overrides.min_keep_ms.unwrap_or(base.min_keep_ms),
    };
    check_params(&params)?;
    Ok(params)
}

fn params_of(settings: &AutocutSettings) -> AutocutParams {
    AutocutParams {
        threshold_db: settings.threshold_db,
        min_silence_ms: settings.min_silence_ms,
        padding_ms: settings.padding_ms,
        min_keep_ms: settings.min_keep_ms,
    }
}

fn check_params(params: &AutocutParams) -> Result<(), CoreError> {
    if !THRESHOLD_DB_RANGE.contains(&params.threshold_db) {
        return Err(CoreError::usage(format!(
            "the autocut threshold must be between \u{2212}90 and 0 dB (got {})",
            db_label(params.threshold_db)
        ))
        .with_hint("speech usually sits well above \u{2212}35 dB; try --threshold-db -35"));
    }
    for (what, ms) in [
        ("minimum silence", params.min_silence_ms),
        ("padding", params.padding_ms),
        ("minimum kept fragment", params.min_keep_ms),
    ] {
        if ms > MAX_MS {
            return Err(CoreError::usage(format!(
                "the autocut {what} must be at most {MAX_MS} ms (got {ms} ms)"
            )));
        }
    }
    Ok(())
}

/// The clips `scope` names, in timeline order and without duplicates.
fn clips_in_scope(project: &Project, scope: &ClipScope) -> Result<Vec<Clip>, CoreError> {
    match scope {
        ClipScope::All => {
            if project.timeline.clips.is_empty() {
                return Err(CoreError::invalid("nothing to autocut: the timeline is empty")
                    .with_hint("record a take first (qrec record start)"));
            }
            Ok(project.timeline.clips.clone())
        }
        ClipScope::Clips(ids) => {
            if ids.is_empty() {
                return Err(CoreError::usage("no clips to autocut")
                    .with_hint("pass clip ids, or --all for the whole timeline"));
            }
            if let Some(unknown) = ids.iter().find(|id| project.clip(id).is_none()) {
                return Err(CoreError::not_found(format!("no clip {unknown} on the timeline"))
                    .with_hint("list the clips with qrec clip list"));
            }
            let wanted: BTreeSet<&ClipId> = ids.iter().collect();
            Ok(project
                .timeline
                .clips
                .iter()
                .filter(|c| wanted.contains(&c.id))
                .cloned()
                .collect())
        }
    }
}

/// A track id must exist somewhere: in the project scene or in the scene of
/// one of `takes`. Takes that lack it are skipped later, with a reason.
fn ensure_track_known<'a>(
    project: &Project,
    track: &TrackId,
    mut takes: impl Iterator<Item = &'a Take>,
) -> Result<(), CoreError> {
    let known =
        project.scene.track(track).is_some() || takes.any(|take| take.scene.track(track).is_some());
    if known {
        Ok(())
    } else {
        Err(CoreError::not_found(format!("no audio track {track}"))
            .with_hint("use the id of an audio track in the scene, such as A1"))
    }
}

/// "auto-run on", "track A2", "threshold −30 dB", … for the history label.
fn settings_changes(old: &AutocutSettings, new: &AutocutSettings) -> Vec<String> {
    let mut changes = Vec::new();
    if old.auto_run_after_take != new.auto_run_after_take {
        let on = if new.auto_run_after_take { "on" } else { "off" };
        changes.push(format!("auto-run {on}"));
    }
    if old.track != new.track {
        changes.push(match &new.track {
            Some(track) => format!("track {track}"),
            None => "track: first mic".to_owned(),
        });
    }
    if old.threshold_db.to_bits() != new.threshold_db.to_bits() {
        changes.push(format!("threshold {}", db_label(new.threshold_db)));
    }
    let ms = [
        ("min silence", old.min_silence_ms, new.min_silence_ms),
        ("padding", old.padding_ms, new.padding_ms),
        ("min keep", old.min_keep_ms, new.min_keep_ms),
    ];
    for (what, before, after) in ms {
        if before != after {
            changes.push(format!("{what} {after} ms"));
        }
    }
    if changes.is_empty() {
        changes.push("unchanged".to_owned());
    }
    changes
}

fn log(level: LogLevel, message: String) -> Effect {
    Effect::Log { level, message }
}
