//! Silence analysis results and autocut completion (A§5.7). Owner: autocut.
//!
//! `SilenceDetected` stores the intervals in `runtime.silence` and drops the
//! key from the job's `pending`; `SilenceFailed` moves the clips of that key
//! to the skipped list. When nothing is pending the job completes:
//! - while a recording runs (`record != Idle`) a real run is only marked
//!   `ready`; the record owner applies it right after placing the take with
//!   [`apply_ready_autocuts`] (dry runs apply nothing and complete at once);
//! - otherwise it is applied to targets whose clip still equals its
//!   `ClipRef` (the others are skipped, "changed while analyzing"): one
//!   history entry for the whole run, the first fragment keeping the clip
//!   id, and no entry when nothing changes.
//!
//! The requester gets the reply; a run nobody waits for (autocut after a
//! take, a requester gone) is told with a toast.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::apply::Outcome;
use crate::autocut::{ClipCut, db_label, dry_run_label, entirely_silent, plan_clip, plural, run_label};
use crate::ctx::{Ctx, IdGen};
use crate::error::CoreError;
use crate::history::commit;
use crate::ids::{ClipId, JobId, ReqId};
use crate::model::{Clip, Project, StreamKind};
use crate::protocol::effect::{Effect, LogLevel};
use crate::protocol::event::Interval;
use crate::protocol::progress::{Progress, ProgressPhase};
use crate::protocol::reply::{AutocutClipView, AutocutPlanView, AutocutSkipView, CmdResult};
use crate::protocol::toast::{Toast, ToastLevel};
use crate::state::{AutocutParams, AutocutTarget, Job, JobKind, SilenceKey, State};

pub use crate::apply::autocut::start_auto_run;

/// The skip reason of a target whose clip changed during the analysis.
pub const CHANGED_WHILE_ANALYZING: &str = "changed while analyzing";
/// Prefix of the skip reason of a clip whose analysis failed.
const ANALYSIS_FAILED: &str = "silence analysis failed: ";
/// The member of a job's `cmd` that carries the clips skipped so far: A§3's
/// `JobKind::Autocut` has no field for them. Never part of a history entry.
const SKIPPED_MEMBER: &str = "skipped";

/// ffmpeg found these silences (file time) for `key`.
pub fn silence_detected(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    key: SilenceKey,
    silences: Vec<Interval>,
) -> Outcome {
    if !is_pending(state, job, &key) {
        return ignored(state, "SilenceDetected", job, &key);
    }
    let mut next = state.clone();
    next.runtime.silence.insert(key.clone(), silences);
    resolve(next, ctx, job, &key, Vec::new())
}

/// The analysis of `key` failed: its clips are skipped with the message.
pub fn silence_failed(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    key: SilenceKey,
    message: String,
) -> Outcome {
    if !is_pending(state, job, &key) {
        return ignored(state, "SilenceFailed", job, &key);
    }
    let mut next = state.clone();
    let effects = vec![Effect::Log {
        level: LogLevel::Warn,
        message: format!("autocut {job}: silence analysis failed: {message}"),
    }];
    if let Some(Job {
        kind: JobKind::Autocut { cmd, targets, .. },
        ..
    }) = next.runtime.jobs.get_mut(&job)
    {
        let (failed, rest): (Vec<AutocutTarget>, Vec<AutocutTarget>) =
            std::mem::take(targets).into_iter().partition(|t| t.key == key);
        *targets = rest;
        let (clean, mut skipped) = split_skipped(cmd);
        skipped.extend(failed.into_iter().map(|t| AutocutSkipView {
            clip: t.clip.id,
            reason: format!("{ANALYSIS_FAILED}{message}"),
        }));
        *cmd = with_skipped(clean, &skipped);
    }
    resolve(next, ctx, job, &key, effects)
}

/// Applies every autocut job that finished analyzing while a recording ran
/// (`ready`), oldest first, each as its own history entry, and replies to
/// their requesters. The record owner calls it right after placing a take —
/// and whenever a recording ends without placing one — whatever
/// `runtime.record` still says; a no-op when no job is ready.
pub fn apply_ready_autocuts(state: &State, ctx: &Ctx) -> Outcome {
    let (state, effects) = flush_ready(state, ctx);
    Outcome::new(state, effects, None)
}

/// [`apply_ready_autocuts`] as state and effects.
pub(crate) fn flush_ready(state: &State, ctx: &Ctx) -> (State, Vec<Effect>) {
    let ready: Vec<JobId> = state
        .runtime
        .jobs
        .values()
        .filter(|job| matches!(job.kind, JobKind::Autocut { ready: true, .. }))
        .map(|job| job.id)
        .collect();
    let mut current = state.clone();
    let mut effects = Vec::new();
    for id in ready {
        let (next, more) = complete(&current, ctx, id);
        current = next;
        effects.extend(more);
    }
    (current, effects)
}

fn is_pending(state: &State, job: JobId, key: &SilenceKey) -> bool {
    matches!(
        state.runtime.jobs.get(&job),
        Some(Job { kind: JobKind::Autocut { pending, .. }, .. }) if pending.contains(key)
    )
}

fn ignored(state: &State, event: &str, job: JobId, key: &SilenceKey) -> Outcome {
    let message = format!(
        "ignoring {event} for {job}: no autocut job waits for key {}",
        key.0
    );
    Outcome::new(
        state.clone(),
        vec![Effect::Log {
            level: LogLevel::Debug,
            message,
        }],
        None,
    )
}

/// `key` is known (or failed) for `job`: report progress, then complete,
/// defer or keep waiting.
fn resolve(
    mut next: State,
    ctx: &Ctx,
    job_id: JobId,
    key: &SilenceKey,
    mut effects: Vec<Effect>,
) -> Outcome {
    let idle = next.runtime.record.is_idle();
    let Some(job) = next.runtime.jobs.get_mut(&job_id) else {
        return Outcome::new(next, effects, None);
    };
    let JobKind::Autocut {
        pending,
        dry_run,
        ready,
        ..
    } = &mut job.kind
    else {
        return Outcome::new(next, effects, None);
    };
    pending.remove(key);
    let analyzed = pending.is_empty();
    let completes = analyzed && (idle || *dry_run);
    if analyzed && !completes {
        // A recording runs: apply after the take is placed (A§5.7).
        *ready = true;
    }
    effects.extend(progress_of(job, ctx));
    if !completes {
        return Outcome::new(next, effects, None);
    }
    if idle {
        // Jobs left ready by a recording that placed nothing go first.
        let (flushed, more) = flush_ready(&next, ctx);
        next = flushed;
        effects.extend(more);
    }
    let (next, more) = complete(&next, ctx, job_id);
    effects.extend(more);
    Outcome::new(next, effects, None)
}

/// Removes the job, applies (or previews) it and delivers the reply.
fn complete(state: &State, ctx: &Ctx, job_id: JobId) -> (State, Vec<Effect>) {
    let Some(Job {
        kind:
            JobKind::Autocut {
                cmd,
                targets,
                params,
                dry_run,
                ..
            },
        req,
        ..
    }) = state.runtime.jobs.get(&job_id).cloned()
    else {
        return (state.clone(), Vec::new());
    };
    let mut next = state.clone();
    next.runtime.jobs.remove(&job_id);
    let (cmd, skipped) = split_skipped(&cmd);
    let snapshot = Snapshot {
        cmd,
        targets,
        params,
        dry_run,
        skipped,
    };
    finish(&next, ctx, snapshot).deliver(req.as_ref())
}

// ------------------------------------------------------------ completion

/// An autocut run whose silences are all known.
pub(crate) struct Snapshot {
    /// The history entry's `cmd` (without the skipped member).
    pub cmd: Value,
    pub targets: Vec<AutocutTarget>,
    pub params: AutocutParams,
    pub dry_run: bool,
    /// Clips skipped before completion, with their reasons.
    pub skipped: Vec<AutocutSkipView>,
}

/// A completed run: the next state, its persistence effects, the reply for
/// the requester and the toast for when nobody waits.
pub(crate) struct Finished {
    pub state: State,
    pub effects: Vec<Effect>,
    pub reply: Result<CmdResult, CoreError>,
    pub toast: Toast,
}

impl Finished {
    /// Replies to `req`, or shows the toast when nobody waits.
    pub(crate) fn deliver(self, req: Option<&ReqId>) -> (State, Vec<Effect>) {
        let mut effects = self.effects;
        effects.push(match req {
            Some(req) => Effect::Reply {
                req: req.clone(),
                reply: self.reply,
            },
            None => Effect::Notify(self.toast),
        });
        (self.state, effects)
    }
}

/// Plans every target against `state` (which no longer holds the job) and
/// applies the cuts as one history entry, or previews them for a dry run.
pub(crate) fn finish(state: &State, ctx: &Ctx, job: Snapshot) -> Finished {
    let project = &state.project;
    let fps = project.canvas.fps;
    let mut skipped = job.skipped;
    let mut clips = Vec::new();
    let mut cuts: BTreeMap<ClipId, ClipCut> = BTreeMap::new();
    for target in &job.targets {
        match plan_target(state, target, &job.params) {
            Ok((clip, cut)) => {
                clips.push(AutocutClipView {
                    clip: clip.id.clone(),
                    kept: cut.kept.clone(),
                    removed_frames: cut.removed_frames,
                    silences: cut.silences,
                });
                if cut.changes(clip) {
                    cuts.insert(clip.id.clone(), cut);
                }
            }
            Err(reason) => skipped.push(AutocutSkipView {
                clip: target.clip.id.clone(),
                reason,
            }),
        }
    }
    in_timeline_order(project, &mut skipped);
    let plan = AutocutPlanView { clips, skipped };
    let note = skipped_note(&plan.skipped);

    let failed = plan
        .skipped
        .iter()
        .find(|s| s.reason.starts_with(ANALYSIS_FAILED));
    if let (true, Some(first)) = (plan.clips.is_empty(), failed) {
        let err = CoreError::io(first.reason.clone())
            .with_hint("the ffmpeg log is in .qrec/logs/")
            .with_details(plan_json(&plan));
        return unchanged(state, Err(err.clone()), failure_toast(&err));
    }

    let mut takes = BTreeSet::new();
    let (mut silences, mut removed_ns) = (0, 0);
    for (id, cut) in &cuts {
        if let Some(clip) = project.clip(id) {
            takes.insert(clip.take.clone());
            silences += cut.silences;
            removed_ns += cut.removed_ns(clip, fps);
        }
    }

    if cuts.is_empty() {
        let why = nothing_to_cut(&plan, &job.params);
        let message = match job.dry_run {
            true => with_note(format!("Dry run: nothing to cut: {why}"), &note),
            false => with_note(format!("Nothing to cut: {why}"), &note),
        };
        let level = if plan.clips.is_empty() {
            ToastLevel::Warning
        } else {
            ToastLevel::Info
        };
        let toast = Toast::new(level, "Autocut found nothing to cut")
            .with_body(with_note(capitalized(&why), &note));
        return unchanged(state, Ok(CmdResult::new(message).with_result(&plan)), toast);
    }

    if job.dry_run {
        let message = with_note(dry_run_label(&takes, silences, removed_ns), &note);
        let toast = Toast::new(ToastLevel::Info, message.clone());
        return unchanged(state, Ok(CmdResult::new(message).with_result(&plan)), toast);
    }

    if let Some(why) = &state.runtime.disk_invalid {
        let err = CoreError::invalid(format!(
            "qrec.json on disk is invalid: {why}; fix it or run `qrec history restore {} --yes`",
            state.history.cursor
        ))
        .with_details(plan_json(&plan));
        let toast = Toast::new(ToastLevel::Error, "Autocut wasn't applied").with_body(err.message.clone());
        return unchanged(state, Err(err), toast);
    }

    let mut ids = IdGen::for_state(state, ctx.entropy);
    let mut created = Vec::new();
    let mut timeline = Vec::with_capacity(project.timeline.clips.len() + cuts.len());
    for clip in &project.timeline.clips {
        let Some(cut) = cuts.get(&clip.id) else {
            timeline.push(clip.clone());
            continue;
        };
        for (i, &[in_, out]) in cut.kept.iter().enumerate() {
            let id = if i == 0 {
                clip.id.clone()
            } else {
                let id = ids.next_clip_id();
                created.push(id.clone());
                id
            };
            timeline.push(Clip {
                id,
                take: clip.take.clone(),
                in_,
                out,
            });
        }
    }
    let mut next = state.clone();
    next.project.timeline.clips = timeline;
    let label = run_label(&takes, silences, removed_ns);
    let committed = commit(state, ctx, label.clone(), job.cmd, next);
    let message = with_note(label.clone(), &note);
    let mut toast = Toast::new(ToastLevel::Success, label);
    if let Some(note) = &note {
        toast = toast.with_body(capitalized(note));
    }
    let reply = CmdResult::new(message)
        .with_seq(committed.seq)
        .with_created(&created)
        .with_result(&plan);
    Finished {
        state: committed.state,
        effects: committed.effects,
        reply: Ok(reply),
        toast,
    }
}

/// The current clip, its take's track and stream, and the cut — or why the
/// target is skipped.
fn plan_target<'s>(
    state: &'s State,
    target: &AutocutTarget,
    params: &AutocutParams,
) -> Result<(&'s Clip, ClipCut), String> {
    let project = &state.project;
    let changed = || CHANGED_WHILE_ANALYZING.to_owned();
    let clip = project
        .clip(&target.clip.id)
        .filter(|clip| target.clip.matches(clip))
        .ok_or_else(changed)?;
    let take = project.take(&clip.take).ok_or_else(changed)?;
    let track = take.scene.track(&target.track).ok_or_else(changed)?;
    let stream = take
        .stream(target.track.as_str())
        .filter(|s| s.kind == StreamKind::Audio && !track.missing)
        .ok_or_else(|| {
            format!(
                "track {} of take {} is missing media",
                track.id,
                clip.take.number()
            )
        })?;
    let silences = state
        .runtime
        .silence
        .get(&target.key)
        .ok_or_else(|| format!("{ANALYSIS_FAILED}no result"))?;
    let cut = plan_clip(clip, stream, track, silences, params, project.canvas.fps);
    if cut.is_entirely_silent() {
        return Err(entirely_silent(params));
    }
    Ok((clip, cut))
}

/// A completion that leaves the project alone (dry runs, nothing to cut, errors).
fn unchanged(state: &State, reply: Result<CmdResult, CoreError>, toast: Toast) -> Finished {
    Finished {
        state: state.clone(),
        effects: Vec::new(),
        reply,
        toast,
    }
}

fn nothing_to_cut(plan: &AutocutPlanView, params: &AutocutParams) -> String {
    match plan.clips.len() {
        0 => "no clip could be analyzed".to_owned(),
        _ => format!(
            "no silence long enough to cut at {}",
            db_label(params.threshold_db)
        ),
    }
}

/// "1 clip skipped (c1a2: entirely silent at −35 dB)"; at most three reasons.
fn skipped_note(skipped: &[AutocutSkipView]) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let mut reasons: Vec<String> = skipped
        .iter()
        .take(3)
        .map(|s| format!("{}: {}", s.clip, s.reason))
        .collect();
    if skipped.len() > 3 {
        reasons.push("…".to_owned());
    }
    Some(format!(
        "{} {} skipped ({})",
        skipped.len(),
        plural(skipped.len(), "clip", "clips"),
        reasons.join(", ")
    ))
}

fn with_note(message: String, note: &Option<String>) -> String {
    match note {
        Some(note) => format!("{message}; {note}"),
        None => message,
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Skips sorted by where their clip sits on the timeline (gone clips last).
fn in_timeline_order(project: &Project, skipped: &mut [AutocutSkipView]) {
    skipped.sort_by_key(|s| project.clip_index(&s.clip).unwrap_or(usize::MAX));
}

fn plan_json(plan: &AutocutPlanView) -> Value {
    serde_json::to_value(plan).unwrap_or(Value::Null)
}

/// The toast for a run that failed and has nobody to reply to.
pub(crate) fn failure_toast(err: &CoreError) -> Toast {
    Toast::new(ToastLevel::Error, "Autocut failed").with_body(err.message.clone())
}

// --------------------------------------------------------------- progress

/// `Progress{phase: Analyze}` for the job's requester: clips whose audio is
/// analyzed (or failed) out of all, then "Waiting for the recording to
/// finish" once a recording holds the job back.
pub(crate) fn progress_of(job: &Job, ctx: &Ctx) -> Option<Effect> {
    let req = job.req.clone()?;
    let JobKind::Autocut {
        cmd,
        targets,
        pending,
        ready,
        ..
    } = &job.kind
    else {
        return None;
    };
    let failed = split_skipped(cmd)
        .1
        .iter()
        .filter(|s| s.reason.starts_with(ANALYSIS_FAILED))
        .count();
    let total = (targets.len() + failed) as u64;
    let done = (failed + targets.iter().filter(|t| !pending.contains(&t.key)).count()) as u64;
    let elapsed = u64::try_from((ctx.now - job.started).num_seconds()).unwrap_or(0);
    let eta_secs = (done > 0 && done < total).then(|| elapsed * (total - done) / done);
    let label = if *ready {
        "Waiting for the recording to finish".to_owned()
    } else {
        format!("Analyzing audio {done}/{total}")
    };
    Some(Effect::Progress {
        req,
        progress: Progress {
            phase: ProgressPhase::Analyze,
            done,
            total,
            eta_secs,
            label,
            take: None,
        },
    })
}

// ---------------------------------------------------- the skipped stash

/// `cmd` with `skipped` attached (as its `skipped` member) when there are any.
pub(crate) fn with_skipped(cmd: Value, skipped: &[AutocutSkipView]) -> Value {
    match cmd {
        Value::Object(mut map) if !skipped.is_empty() => {
            let list = serde_json::to_value(skipped).unwrap_or(Value::Null);
            map.insert(SKIPPED_MEMBER.to_owned(), list);
            Value::Object(map)
        }
        other => other,
    }
}

/// The job's `cmd` without the stash, and the clips skipped so far.
pub(crate) fn split_skipped(cmd: &Value) -> (Value, Vec<AutocutSkipView>) {
    let mut clean = cmd.clone();
    let skipped = match &mut clean {
        Value::Object(map) => map
            .remove(SKIPPED_MEMBER)
            .and_then(|list| serde_json::from_value(list).ok())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    (clean, skipped)
}
