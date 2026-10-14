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
use crate::autocut::{
    ClipCut, db_label, dry_run_label, nothing_kept_reason, plan_clip, plural, run_label,
};
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
        let (failed, rest): (Vec<AutocutTarget>, Vec<AutocutTarget>) = std::mem::take(targets)
            .into_iter()
            .partition(|t| t.key == key);
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
        let toast =
            Toast::new(ToastLevel::Error, "Autocut wasn't applied").with_body(err.message.clone());
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
    let fps = project.canvas.fps;
    let cut = plan_clip(clip, stream, track, silences, params, fps);
    if cut.keeps_nothing() {
        return Err(nothing_kept_reason(
            clip, stream, track, silences, params, fps,
        ));
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
    let eta_secs =
        (done > 0 && done < total && elapsed > 0).then(|| elapsed * (total - done) / done);
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

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::apply::autocut::fixtures::{self, MIC1, MIC2};
    use crate::error::ErrorCode;
    use crate::ids::TakeId;
    use crate::protocol::command::{AutocutOverrides, ClipScope, Command};
    use crate::protocol::effect::HistoryOp;
    use crate::state::{ClipRef, RecordMode, RecordPhase};
    use crate::testkit;

    /// A state with job 1 analyzing both mic files (`qrec autocut`, request r1).
    fn analyzing(dry_run: bool) -> (State, Ctx) {
        let project = fixtures::project();
        let ctx = fixtures::ctx(&project);
        let state = testkit::state(project);
        let cmd = Command::Autocut {
            scope: ClipScope::All,
            overrides: AutocutOverrides::default(),
            dry_run,
        };
        let out = crate::apply::apply(&state, &ctx, cmd).unwrap();
        assert_eq!(out.reply, None);
        (out.state, ctx)
    }

    fn detected(state: &State, ctx: &Ctx, file: &str) -> Outcome {
        let key = fixtures::key(ctx, file, &fixtures::params());
        silence_detected(state, ctx, JobId(1), key, fixtures::silences(file))
    }

    fn failed(state: &State, ctx: &Ctx, file: &str, message: &str) -> Outcome {
        let key = fixtures::key(ctx, file, &fixtures::params());
        silence_failed(state, ctx, JobId(1), key, message.to_owned())
    }

    fn recording(state: &mut State) {
        state.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(3),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
    }

    fn reply_of(out: &Outcome) -> Option<(&ReqId, &Result<CmdResult, CoreError>)> {
        out.effects.iter().find_map(|e| match e {
            Effect::Reply { req, reply } => Some((req, reply)),
            _ => None,
        })
    }

    fn progress_labels(out: &Outcome) -> Vec<String> {
        out.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Progress { progress, .. } => Some(format!(
                    "{} ({}/{})",
                    progress.label, progress.done, progress.total
                )),
                _ => None,
            })
            .collect()
    }

    fn view(result: &CmdResult) -> AutocutPlanView {
        serde_json::from_value(result.result.clone()).unwrap()
    }

    #[test]
    fn the_last_result_applies_the_run() {
        let (state, ctx) = analyzing(false);
        let first = detected(&state, &ctx, MIC1);
        assert!(reply_of(&first).is_none(), "one key is still pending");
        assert_eq!(progress_labels(&first), ["Analyzing audio 1/2 (1/2)"]);
        assert_eq!(first.state.project, state.project);
        let key1 = fixtures::key(&ctx, MIC1, &fixtures::params());
        assert_eq!(first.state.runtime.silence[&key1], fixtures::silences(MIC1));

        let done = detected(&first.state, &ctx, MIC2);
        let (req, reply) = reply_of(&done).expect("the requester gets the reply");
        assert_eq!(req.0, "r1");
        let result = reply.as_ref().unwrap();
        assert_eq!(
            result.message,
            "Autocut 2 takes: removed 4 silences (4.3 s)"
        );
        assert_eq!(result.seq, Some(1));
        assert_eq!(result.created.len(), 2);
        assert_eq!(view(result).clips.len(), 2);
        assert_eq!(done.reply, None, "events answer through Effect::Reply");
        assert!(done.state.runtime.jobs.is_empty());
        assert_eq!(done.state.runtime.silence.len(), 2, "results stay cached");
        assert_eq!(done.state.project.timeline.clips.len(), 4);
        // Persistence first, the reply last (A§7.6).
        assert!(matches!(
            done.effects.as_slice(),
            [
                Effect::Progress { .. },
                Effect::History(HistoryOp::Append(_)),
                Effect::WriteProject,
                Effect::Reply { .. }
            ]
        ));
        let entry = &done.state.history.entries[0];
        assert_eq!(
            entry.cmd,
            json!({"cmd": "autocut", "scope": "all", "overrides": {}, "dryRun": false})
        );
    }

    #[test]
    fn a_dry_run_replies_with_the_plan() {
        let (state, ctx) = analyzing(true);
        let first = detected(&state, &ctx, MIC1);
        let done = detected(&first.state, &ctx, MIC2);
        let (_, reply) = reply_of(&done).unwrap();
        let result = reply.as_ref().unwrap();
        assert_eq!(result.seq, None);
        assert_eq!(
            result.message,
            "Dry run: autocut 2 takes would remove 4 silences (4.3 s)"
        );
        assert_eq!(view(result).clips[0].kept, vec![[20, 94], [131, 244]]);
        assert_eq!(done.state.project, state.project);
        assert!(done.state.history.entries.is_empty());
    }

    #[test]
    fn a_recording_defers_the_run_until_placement() {
        let (mut state, ctx) = analyzing(false);
        recording(&mut state);
        let first = detected(&state, &ctx, MIC1);
        let ready = detected(&first.state, &ctx, MIC2);
        assert!(reply_of(&ready).is_none());
        assert_eq!(
            ready.state.project, state.project,
            "nothing applied while recording"
        );
        assert_eq!(
            progress_labels(&ready),
            ["Waiting for the recording to finish (2/2)"]
        );
        assert!(matches!(
            ready.state.runtime.jobs[&JobId(1)].kind,
            JobKind::Autocut { ready: true, .. }
        ));

        // The take was appended and placed: the record owner applies the job
        // (whatever the phase still says).
        let mut placed = ready.state.clone();
        placed.project.timeline.clips.push(Clip {
            id: ClipId::from_raw("c003"),
            take: TakeId::numbered(2),
            in_: 0,
            out: 150,
        });
        let applied = apply_ready_autocuts(&placed, &ctx);
        let (_, reply) = reply_of(&applied).unwrap();
        assert_eq!(reply.as_ref().unwrap().seq, Some(1));
        assert!(applied.state.runtime.jobs.is_empty());
        assert_eq!(applied.state.project.timeline.clips.len(), 5);
        assert_eq!(
            applied.state.project.timeline.clips[4].id.as_str(),
            "c003",
            "the new clip isn't a target"
        );
        // Nothing else is ready: a no-op.
        assert_eq!(
            apply_ready_autocuts(&applied.state, &ctx),
            Outcome::unchanged(&applied.state)
        );
    }

    #[test]
    fn a_record_over_the_target_skips_it() {
        let (mut state, ctx) = analyzing(false);
        recording(&mut state);
        let ready = detected(&detected(&state, &ctx, MIC1).state, &ctx, MIC2).state;
        // Record over c002: the take replaces it.
        let mut placed = ready.clone();
        placed.project.timeline.clips[1] = Clip {
            id: ClipId::from_raw("c003"),
            take: TakeId::numbered(2),
            in_: 0,
            out: 150,
        };
        placed.runtime.record = RecordPhase::Idle;
        let applied = apply_ready_autocuts(&placed, &ctx);
        let (_, reply) = reply_of(&applied).unwrap();
        let result = reply.as_ref().unwrap();
        assert_eq!(
            view(result).skipped,
            vec![AutocutSkipView {
                clip: ClipId::from_raw("c002"),
                reason: CHANGED_WHILE_ANALYZING.to_owned()
            }]
        );
        assert_eq!(
            result.message,
            "Autocut take 1: removed 3 silences (3.8 s); 1 clip skipped \
             (c002: changed while analyzing)"
        );
    }

    #[test]
    fn a_dry_run_completes_even_while_recording() {
        let (mut state, ctx) = analyzing(true);
        recording(&mut state);
        let done = detected(&detected(&state, &ctx, MIC1).state, &ctx, MIC2);
        assert!(reply_of(&done).is_some());
        assert!(done.state.runtime.jobs.is_empty());
    }

    #[test]
    fn clips_edited_during_the_analysis_are_skipped() {
        let (mut state, ctx) = analyzing(false);
        state.project.timeline.clips[0].out = 200; // trimmed meanwhile
        let done = detected(&detected(&state, &ctx, MIC1).state, &ctx, MIC2);
        let (_, reply) = reply_of(&done).unwrap();
        let result = reply.as_ref().unwrap();
        let plan = view(result);
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].clip.as_str(), "c001");
        assert_eq!(plan.skipped[0].reason, "changed while analyzing");
        assert_eq!(
            result.message.split(';').next(),
            Some("Autocut take 2: removed 1 silence (0.5 s)")
        );
        assert_eq!(
            done.state.project.timeline.clips[0].out, 200,
            "left as edited"
        );

        // A clip removed meanwhile is skipped the same way.
        let (mut state, ctx) = analyzing(false);
        state.project.timeline.clips.remove(1);
        let done = detected(&detected(&state, &ctx, MIC1).state, &ctx, MIC2);
        let (_, reply) = reply_of(&done).unwrap();
        let plan = view(reply.as_ref().unwrap());
        assert_eq!(plan.skipped[0].clip.as_str(), "c002");
    }

    #[test]
    fn a_failed_analysis_skips_its_clips() {
        let (state, ctx) = analyzing(false);
        let first = failed(
            &state,
            &ctx,
            MIC1,
            "ffmpeg exited with status 1 (see .qrec/logs/x.log)",
        );
        assert!(matches!(
            first.effects.first(),
            Some(Effect::Log {
                level: LogLevel::Warn,
                ..
            })
        ));
        assert_eq!(progress_labels(&first), ["Analyzing audio 1/2 (1/2)"]);
        let JobKind::Autocut { targets, cmd, .. } = &first.state.runtime.jobs[&JobId(1)].kind
        else {
            panic!("still running");
        };
        assert_eq!(targets.len(), 1);
        assert_eq!(cmd["skipped"][0]["clip"], "c001");

        let done = detected(&first.state, &ctx, MIC2);
        let (_, reply) = reply_of(&done).unwrap();
        let result = reply.as_ref().unwrap();
        assert_eq!(
            view(result).skipped,
            vec![AutocutSkipView {
                clip: ClipId::from_raw("c001"),
                reason:
                    "silence analysis failed: ffmpeg exited with status 1 (see .qrec/logs/x.log)"
                        .to_owned()
            }]
        );
        assert_eq!(result.seq, Some(1));
        let entry = &done.state.history.entries[0];
        assert_eq!(entry.label, "Autocut take 2: removed 1 silence (0.5 s)");
        assert!(
            entry.cmd.get("skipped").is_none(),
            "the stash never reaches history"
        );
    }

    #[test]
    fn when_every_analysis_fails_the_run_fails() {
        let (state, ctx) = analyzing(false);
        let first = failed(&state, &ctx, MIC1, "boom");
        let done = failed(&first.state, &ctx, MIC2, "boom 2");
        let (_, reply) = reply_of(&done).unwrap();
        let err = reply.as_ref().unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(err.message, "silence analysis failed: boom");
        let details = err.details.as_ref().unwrap();
        assert_eq!(details["skipped"].as_array().unwrap().len(), 2);
        assert_eq!(done.state.project, state.project);
        assert!(done.state.runtime.jobs.is_empty());
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let (state, ctx) = analyzing(false);
        let key = fixtures::key(&ctx, MIC1, &fixtures::params());
        let other = SilenceKey("0".repeat(64));
        for out in [
            silence_detected(&state, &ctx, JobId(7), key.clone(), vec![]),
            silence_detected(&state, &ctx, JobId(1), other.clone(), vec![]),
            silence_failed(&state, &ctx, JobId(7), key.clone(), "x".into()),
            silence_failed(&state, &ctx, JobId(1), other, "x".into()),
        ] {
            assert_eq!(out.state, state);
            assert!(matches!(
                out.effects.as_slice(),
                [Effect::Log {
                    level: LogLevel::Debug,
                    ..
                }]
            ));
        }
        // A duplicate result after the key resolved is ignored too.
        let first = detected(&state, &ctx, MIC1);
        let again = detected(&first.state, &ctx, MIC1);
        assert_eq!(again.state, first.state);
    }

    #[test]
    fn nobody_waiting_means_a_toast() {
        let (mut state, ctx) = analyzing(false);
        if let Some(job) = state.runtime.jobs.get_mut(&JobId(1)) {
            job.req = None; // the requester went away
        }
        let first = detected(&state, &ctx, MIC1);
        assert!(progress_labels(&first).is_empty());
        let done = detected(&first.state, &ctx, MIC2);
        assert!(reply_of(&done).is_none());
        let Some(Effect::Notify(toast)) = done.effects.last() else {
            panic!("a toast: {:?}", done.effects);
        };
        assert_eq!(toast.level, ToastLevel::Success);
        assert_eq!(toast.title, "Autocut 2 takes: removed 4 silences (4.3 s)");

        let failing = failed(
            &failed(&state, &ctx, MIC1, "boom").state,
            &ctx,
            MIC2,
            "boom",
        );
        let Some(Effect::Notify(toast)) = failing.effects.last() else {
            panic!("a toast");
        };
        assert_eq!(toast.level, ToastLevel::Error);
        assert_eq!(toast.title, "Autocut failed");
        assert_eq!(toast.body.as_deref(), Some("silence analysis failed: boom"));
    }

    #[test]
    fn an_invalid_file_on_disk_blocks_the_edit() {
        let (mut state, ctx) = analyzing(false);
        state.runtime.disk_invalid = Some("expected value at line 3".into());
        let done = detected(&detected(&state, &ctx, MIC1).state, &ctx, MIC2);
        let (_, reply) = reply_of(&done).unwrap();
        let err = reply.as_ref().unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert!(
            err.message
                .starts_with("qrec.json on disk is invalid: expected value at line 3;")
        );
        assert_eq!(done.state.project, state.project);
        assert!(done.state.history.entries.is_empty());
        assert!(done.state.runtime.jobs.is_empty());
    }

    #[test]
    fn progress_estimates_the_time_left() {
        let (mut state, _) = analyzing(false);
        let later = testkit::ctx("2026-10-14T12:30:10Z", 8);
        let mut ctx = fixtures::ctx(&state.project);
        ctx.now = later.now;
        if let Some(job) = state.runtime.jobs.get_mut(&JobId(1)) {
            job.started = testkit::ctx("2026-10-14T12:30:00Z", 0).now;
        }
        let first = detected(&state, &ctx, MIC1);
        let Some(Effect::Progress { progress, .. }) = first
            .effects
            .iter()
            .find(|e| matches!(e, Effect::Progress { .. }))
        else {
            panic!("progress");
        };
        assert_eq!(progress.eta_secs, Some(10), "10 s for one file, one left");
        assert_eq!(progress.phase, ProgressPhase::Analyze);
    }

    #[test]
    fn stale_ready_jobs_go_first_when_idle() {
        let (state, ctx) = analyzing(false);
        let p = fixtures::params();
        // Job 9 became ready during a recording that ended without a take.
        let mut state = state;
        let target = AutocutTarget {
            clip: ClipRef::of(&state.project.timeline.clips[1]),
            track: "A1".parse().unwrap(),
            key: fixtures::key(&ctx, MIC2, &p),
        };
        state.runtime.jobs.insert(
            JobId(9),
            Job {
                id: JobId(9),
                kind: JobKind::Autocut {
                    cmd: json!({"cmd": "autocut", "scope": {"clips": ["c002"]}, "overrides": {}, "dryRun": false}),
                    targets: vec![target],
                    params: p.clone(),
                    dry_run: false,
                    pending: BTreeSet::new(),
                    ready: true,
                },
                req: None,
                started: ctx.now,
            },
        );
        state
            .runtime
            .silence
            .insert(fixtures::key(&ctx, MIC2, &p), fixtures::silences(MIC2));
        let first = detected(&state, &ctx, MIC1);
        let done = detected(&first.state, &ctx, MIC2);
        let labels: Vec<&str> = done
            .state
            .history
            .entries
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Autocut take 2: removed 1 silence (0.5 s)",
                "Autocut take 1: removed 3 silences (3.8 s)"
            ]
        );
        let (_, reply) = reply_of(&done).unwrap();
        let result = reply.as_ref().unwrap();
        assert_eq!(
            view(result).skipped,
            vec![AutocutSkipView {
                clip: ClipId::from_raw("c002"),
                reason: CHANGED_WHILE_ANALYZING.to_owned()
            }],
            "job 9 already cut c002"
        );
        assert!(done.state.runtime.jobs.is_empty());
    }

    #[test]
    fn real_ffmpeg_output_through_the_whole_flow() {
        // A 7 s take whose mic recorded the speech sample; the shell parses
        // ffmpeg's stderr and posts the intervals.
        let project = testkit::ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 210)
            .clip("c001", "take-0001", 0, 210)
            .build();
        let ctx = fixtures::ctx(&project);
        let state = testkit::state(project);
        let cmd = Command::Autocut {
            scope: ClipScope::All,
            overrides: AutocutOverrides::default(),
            dry_run: false,
        };
        let started = crate::apply::apply(&state, &ctx, cmd).unwrap();
        let Some(Effect::DetectSilence { job, key, argv }) = started.effects.first() else {
            panic!("an analysis: {:?}", started.effects);
        };
        assert_eq!(
            argv.join(" "),
            "ffmpeg -hide_banner -nostdin -nostats -loglevel info -y \
             -i file:takes/take-0001/A1-mic.mka -af silencedetect=noise=-35dB:d=0.4 -vn -f null -"
        );
        let stderr = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/silencedetect/speech-trailing.stderr"
        ));
        let silences = crate::parse::silencedetect::parse_silencedetect(stderr).unwrap();
        let done = silence_detected(&started.state, &ctx, *job, key.clone(), silences);
        let (_, reply) = reply_of(&done).unwrap();
        assert_eq!(
            reply.as_ref().unwrap().message,
            "Autocut take 1: removed 4 silences (3.1 s)"
        );
        let kept: Vec<(u64, u64)> = done
            .state
            .project
            .timeline
            .clips
            .iter()
            .map(|c| (c.in_, c.out))
            .collect();
        assert_eq!(kept, [(11, 64), (86, 112), (146, 185)]);
    }

    #[test]
    fn the_stash_round_trips() {
        let cmd = json!({"cmd": "autocut", "scope": "all", "overrides": {}, "dryRun": false});
        assert_eq!(
            with_skipped(cmd.clone(), &[]),
            cmd,
            "no member without skips"
        );
        let skips = vec![AutocutSkipView {
            clip: ClipId::from_raw("c001"),
            reason: "take 1 has no audio track".into(),
        }];
        let stashed = with_skipped(cmd.clone(), &skips);
        assert_eq!(stashed["skipped"][0]["reason"], "take 1 has no audio track");
        assert_eq!(split_skipped(&stashed), (cmd.clone(), skips));
        assert_eq!(split_skipped(&cmd), (cmd, vec![]));
    }

    #[test]
    fn events_snapshot() {
        let (state, ctx) = analyzing(false);
        let first = detected(&state, &ctx, MIC1);
        let done = detected(&first.state, &ctx, MIC2);
        let (mut deferred_state, _) = analyzing(false);
        recording(&mut deferred_state);
        let failed_first = failed(&deferred_state, &ctx, MIC1, "ffmpeg exited with status 1");
        let deferred = detected(&failed_first.state, &ctx, MIC2);
        let mut placed = deferred.state.clone();
        placed.runtime.record = RecordPhase::Idle;
        let flushed = apply_ready_autocuts(&placed, &ctx);
        insta::assert_yaml_snapshot!(json!({
            "firstResult": fixtures::digest(&first),
            "lastResult": fixtures::digest(&done),
            "failedWhileRecording": fixtures::digest(&failed_first),
            "readyWhileRecording": fixtures::digest(&deferred),
            "appliedAfterPlacement": fixtures::digest(&flushed),
        }));
    }
}
