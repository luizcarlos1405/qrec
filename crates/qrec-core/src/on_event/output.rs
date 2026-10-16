//! Export and frame-save job events (A§5.8, A§5.9), and requesters going
//! away. Owner: output.
//!
//! Events whose job isn't in the job table (or is another kind of job) are
//! ignored with a debug log. A finished, failed or cancelled job, or one
//! whose hand-picked output appeared meanwhile (`OutputAppeared`), leaves
//! the table, its requester gets the reply and the person a toast.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::ids::{JobId, ReqId};
use crate::protocol::effect::{Effect, LogLevel};
use crate::protocol::progress::{Progress, ProgressPhase};
use crate::protocol::reply::{CmdResult, ExportDone, FrameSavedView};
use crate::protocol::toast::{Toast, ToastLevel, UiAction};
use crate::state::{Job, JobKind, State};

/// The toast of a finished export (A§5.9).
pub const EXPORT_DONE_TITLE: &str = "Layers! Like an onion. Your video is ready.";

/// The toast of a failed export (A§5.9).
pub const EXPORT_FAILED_TITLE: &str = "Export failed";

/// How much ffmpeg stderr a failure shows (SPEC §13.1).
pub const STDERR_TAIL_LINES: usize = 30;

/// The progress of an export job: `0.9·Σseg_done/total + 0.1·stitch` in
/// frames of `total_frames`, ETA from the time since it started, label
/// "Rendering clip 4/12" or "Stitching". The label counts the timeline's
/// clips the export renders ([`rendered_clips`]): the clips rendered whole,
/// plus one. `None` for other jobs. A GUI shows a running export from any
/// host with it (`hello.jobs`, the export slot, the export panel's
/// [`export_readout`]).
pub fn export_progress_view(job: &Job, now: DateTime<Utc>) -> Option<Progress> {
    let JobKind::Export {
        total_frames,
        seg_frames,
        seg_done,
        seg_clips,
        stitch_total_ns,
        stitch_done_ns,
        stitching,
        ..
    } = &job.kind
    else {
        return None;
    };
    let total = *total_frames;
    let rendered: u64 = seg_done
        .iter()
        .zip(seg_frames)
        .map(|(done, frames)| (*done).min(*frames))
        .sum();
    let stitch_total = (*stitch_total_ns).max(1);
    let stitched = (*stitch_done_ns).min(stitch_total);
    let done = (9 * u128::from(rendered) * u128::from(stitch_total)
        + u128::from(total) * u128::from(stitched))
        / (10 * u128::from(stitch_total));
    let done = (done as u64).min(total);
    let (phase, label) = if *stitching {
        (ProgressPhase::Stitch, "Stitching".to_owned())
    } else {
        let (finished, count) = rendered_clips(seg_frames, seg_done, seg_clips);
        (
            ProgressPhase::Render,
            format!("Rendering clip {}/{count}", (finished + 1).min(count)),
        )
    };
    let elapsed = (now - job.started).num_seconds().max(0) as u128;
    let eta_secs =
        (done > 0).then(|| (elapsed * u128::from(total - done) / u128::from(done)) as u64);
    Some(Progress {
        phase,
        done,
        total,
        eta_secs,
        label,
        take: None,
    })
}

/// How many of the clips an export renders are rendered whole, and how many
/// it renders: the timeline's clips (`seg_clips`, each segment's clips as
/// their frame counts: a render unit of continuous clips counts each,
/// A§9.3), or the segments for a job without them.
pub fn rendered_clips(
    seg_frames: &[u64],
    seg_done: &[u64],
    seg_clips: &[Vec<u64>],
) -> (usize, usize) {
    if seg_clips.is_empty() || seg_clips.len() != seg_frames.len() {
        let finished = seg_done
            .iter()
            .zip(seg_frames)
            .filter(|(done, frames)| done >= frames)
            .count();
        return (finished, seg_frames.len());
    }
    let count = seg_clips.iter().map(Vec::len).sum();
    let finished = seg_clips
        .iter()
        .zip(seg_done)
        .map(|(clips, done)| clips_done(clips, *done))
        .sum();
    (finished, count)
}

/// How many of a render unit's clips (their frame counts, in timeline
/// order) its first `done` frames cover whole.
pub fn clips_done(clips: &[u64], done: u64) -> usize {
    let mut end = 0u64;
    clips
        .iter()
        .take_while(|frames| {
            end += **frames;
            end <= done
        })
        .count()
}

/// What the export panel and the header's export slot read while an
/// export runs (S15b, G§4.8): "42%", "3,449 / 7,391 frames", "Rendering
/// clip 6/10", "Elapsed 00:00:38 · About 00:00:52 left".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportReadout {
    /// [`export_progress_view`]'s `done` (rendering weighs 90 %, the stitch
    /// 10 %) as a whole percent of `total`, rounded to the nearest; 100
    /// only once everything is done.
    pub percent: u8,
    /// Frames rendered so far, `Σ min(seg_done, seg_frames)`: the frames
    /// line (the stitch isn't counted).
    pub rendered: u64,
    pub total: u64,
    /// "Rendering clip 6/10" or "Stitching".
    pub label: String,
    pub elapsed_secs: u64,
    /// `None` until something is done.
    pub eta_secs: Option<u64>,
}

/// The readout of a running export at `now`; `None` for other jobs.
pub fn export_readout(job: &Job, now: DateTime<Utc>) -> Option<ExportReadout> {
    let progress = export_progress_view(job, now)?;
    let JobKind::Export {
        seg_frames,
        seg_done,
        ..
    } = &job.kind
    else {
        return None;
    };
    let rendered = seg_done
        .iter()
        .zip(seg_frames)
        .map(|(done, frames)| (*done).min(*frames))
        .sum();
    let (done, total) = (u128::from(progress.done), u128::from(progress.total));
    let percent = if total == 0 {
        0
    } else if done >= total {
        100
    } else {
        // Nearest whole percent, half up; never 100 before the end.
        ((200 * done + total) / (2 * total)).min(99) as u8
    };
    Some(ExportReadout {
        percent,
        rendered,
        total: progress.total,
        label: progress.label,
        elapsed_secs: (now - job.started).num_seconds().max(0) as u64,
        eta_secs: progress.eta_secs,
    })
}

/// Progress = `0.9·Σseg_done/total + 0.1·stitch`; ETA from `ctx.now − started`.
pub fn export_progress(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    segment: usize,
    frames_done: u64,
) -> Outcome {
    let mut next = state.clone();
    let Some(entry) = next.runtime.jobs.get_mut(&job) else {
        return ignore(
            state,
            format!("ExportProgress for {job}, which isn't running"),
        );
    };
    let JobKind::Export {
        seg_frames,
        seg_done,
        ..
    } = &mut entry.kind
    else {
        return ignore(
            state,
            format!("ExportProgress for {job}, which isn't an export"),
        );
    };
    let (Some(frames), Some(done)) = (seg_frames.get(segment), seg_done.get_mut(segment)) else {
        return ignore(
            state,
            format!(
                "ExportProgress for segment {segment} of {job}, which has {}",
                seg_frames.len()
            ),
        );
    };
    *done = frames_done.min(*frames);
    let effects = progress_effect(entry, ctx.now).into_iter().collect();
    Outcome::new(next, effects, None)
}

pub fn export_stitch_progress(state: &State, ctx: &Ctx, job: JobId, out_time_ns: u64) -> Outcome {
    let mut next = state.clone();
    let Some(entry) = next.runtime.jobs.get_mut(&job) else {
        return ignore(
            state,
            format!("ExportStitchProgress for {job}, which isn't running"),
        );
    };
    let JobKind::Export {
        seg_frames,
        seg_done,
        stitch_total_ns,
        stitch_done_ns,
        stitching,
        ..
    } = &mut entry.kind
    else {
        return ignore(
            state,
            format!("ExportStitchProgress for {job}, which isn't an export"),
        );
    };
    // The stitch starts once every segment is rendered and verified.
    seg_done.clone_from(seg_frames);
    *stitching = true;
    *stitch_done_ns = out_time_ns.min(*stitch_total_ns);
    let effects = progress_effect(entry, ctx.now).into_iter().collect();
    Outcome::new(next, effects, None)
}

/// Reply `ExportDone`; toast "Layers! Like an onion. Your video is ready."
/// with the body "<path> · 88.4 MB" and the actions Open file and Open
/// folder (S16b; Copy path stays in the GUI's export panel). The toast's
/// path is project-relative for an auto name, as the job named it
/// ([`shown_output`]); the reply keeps the shell's.
pub fn export_finished(state: &State, ctx: &Ctx, job: JobId, path: String, bytes: u64) -> Outcome {
    let _ = ctx;
    let Some(entry) = job_of(state, job, |k| matches!(k, JobKind::Export { .. })) else {
        return ignore(
            state,
            format!("ExportFinished for {job}, which isn't a running export"),
        );
    };
    let (frames, out) = match &entry.kind {
        JobKind::Export {
            total_frames, out, ..
        } => (*total_frames, out.as_str()),
        _ => (0, ""),
    };
    let shown = shown_output(out, &path);
    let mut next = state.clone();
    next.runtime.jobs.remove(&job);
    let mut effects = Vec::new();
    if let Some(req) = &entry.req {
        let done = ExportDone {
            path: path.clone(),
            bytes,
            frames,
        };
        effects.push(Effect::Reply {
            req: req.clone(),
            reply: Ok(CmdResult::new(format!("Exported {path}")).with_result(&done)),
        });
    }
    effects.push(Effect::Notify(
        Toast::new(ToastLevel::Success, EXPORT_DONE_TITLE)
            .with_body(format!(
                "{shown} \u{b7} {}",
                crate::view::format::size(bytes)
            ))
            .with_action("Open file", UiAction::OpenPath(shown.clone()))
            .with_action("Open folder", UiAction::RevealPath(shown)),
    ));
    effects.push(log(
        LogLevel::Info,
        format!("{job}: exported {path} ({bytes} bytes, {frames} frames)"),
    ));
    Outcome::new(next, effects, None)
}

pub fn export_failed(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    message: String,
    stderr_tail: Vec<String>,
    log_path: String,
) -> Outcome {
    let _ = ctx;
    let Some(entry) = job_of(state, job, |k| matches!(k, JobKind::Export { .. })) else {
        return ignore(
            state,
            format!("ExportFailed for {job}, which isn't a running export"),
        );
    };
    let tail = last_lines(stderr_tail);
    let log_path = written_log(log_path);
    let mut next = state.clone();
    next.runtime.jobs.remove(&job);
    let mut effects = Vec::new();
    if let Some(req) = &entry.req {
        let error = with_log(
            CoreError::io(format!("export failed: {message}")),
            log_path.as_deref(),
            json!({ "log": log_path, "stderrTail": tail }),
        );
        effects.push(Effect::Reply {
            req: req.clone(),
            reply: Err(error),
        });
    }
    effects.push(Effect::Notify(open_log_action(
        Toast::new(ToastLevel::Error, EXPORT_FAILED_TITLE).with_body(failure_body(&message, &tail)),
        log_path.as_deref(),
    )));
    effects.push(log(
        LogLevel::Error,
        format!(
            "{job}: export failed: {message}{}",
            log_note(log_path.as_deref())
        ),
    ));
    Outcome::new(next, effects, None)
}

/// The output of a finished export as people read it: an auto name stays
/// project-relative, as the job named it (`exports/…`, A§1.3), with the file
/// name the shell gave it (a numbered one when the name was taken); a
/// hand-picked path is the shell's `path`.
pub fn shown_output(out: &str, path: &str) -> String {
    if out.is_empty() || out.starts_with('/') {
        return path.to_owned();
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    match out.rsplit_once('/') {
        Some((folder, _)) => format!("{folder}/{name}"),
        None => name.to_owned(),
    }
}

/// The log a failed job wrote; the shell sends an empty path when it
/// couldn't create one (a read-only project folder, a worker that died):
/// then no hint, detail or action points at it.
fn written_log(log_path: String) -> Option<String> {
    (!log_path.is_empty()).then_some(log_path)
}

/// `error` with "the full ffmpeg log is …" and `details` when there is a log.
fn with_log(error: CoreError, log_path: Option<&str>, details: Value) -> CoreError {
    let error = error.with_details(details);
    match log_path {
        Some(path) => error.with_hint(format!("the full ffmpeg log is {path}")),
        None => error,
    }
}

/// The toast's "Open log" action, when there is a log to open.
fn open_log_action(toast: Toast, log_path: Option<&str>) -> Toast {
    match log_path {
        Some(path) => toast.with_action("Open log", UiAction::OpenPath(path.to_owned())),
        None => toast,
    }
}

fn log_note(log_path: Option<&str>) -> String {
    log_path.map_or_else(String::new, |path| format!(" (log: {path})"))
}

pub fn export_cancelled(state: &State, ctx: &Ctx, job: JobId) -> Outcome {
    let _ = ctx;
    let Some(entry) = job_of(state, job, |k| matches!(k, JobKind::Export { .. })) else {
        return ignore(
            state,
            format!("ExportCancelled for {job}, which isn't a running export"),
        );
    };
    let mut next = state.clone();
    next.runtime.jobs.remove(&job);
    let mut effects = Vec::new();
    if let Some(req) = &entry.req {
        effects.push(Effect::Reply {
            req: req.clone(),
            reply: Err(CoreError::invalid("export cancelled")),
        });
    }
    effects.push(Effect::Notify(Toast::new(
        ToastLevel::Info,
        "Export cancelled",
    )));
    effects.push(log(LogLevel::Info, format!("{job}: export cancelled")));
    Outcome::new(next, effects, None)
}

/// Reply `FrameSavedView`; toast with Copy image / Open folder.
pub fn frame_saved(state: &State, ctx: &Ctx, job: JobId, path: String) -> Outcome {
    let _ = ctx;
    let Some(entry) = job_of(state, job, |k| matches!(k, JobKind::FrameSave { .. })) else {
        return ignore(
            state,
            format!("FrameSaved for {job}, which isn't a running frame save"),
        );
    };
    let mut next = state.clone();
    next.runtime.jobs.remove(&job);
    let mut effects = Vec::new();
    if let Some(req) = &entry.req {
        let view = FrameSavedView { path: path.clone() };
        effects.push(Effect::Reply {
            req: req.clone(),
            reply: Ok(CmdResult::new(format!("Saved {path}")).with_result(&view)),
        });
    }
    effects.push(Effect::Notify(
        Toast::new(ToastLevel::Success, "Frame saved")
            .with_body(path.clone())
            .with_action("Copy image", UiAction::CopyImage(path.clone()))
            .with_action("Open folder", UiAction::RevealPath(path.clone())),
    ));
    Outcome::new(next, effects, None)
}

pub fn frame_failed(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    message: String,
    log_path: String,
) -> Outcome {
    let _ = ctx;
    let Some(entry) = job_of(state, job, |k| matches!(k, JobKind::FrameSave { .. })) else {
        return ignore(
            state,
            format!("FrameFailed for {job}, which isn't a running frame save"),
        );
    };
    let log_path = written_log(log_path);
    let mut next = state.clone();
    next.runtime.jobs.remove(&job);
    let mut effects = Vec::new();
    if let Some(req) = &entry.req {
        let error = with_log(
            CoreError::io(format!("saving the frame failed: {message}")),
            log_path.as_deref(),
            json!({ "log": log_path }),
        );
        effects.push(Effect::Reply {
            req: req.clone(),
            reply: Err(error),
        });
    }
    effects.push(Effect::Notify(open_log_action(
        Toast::new(ToastLevel::Error, "Couldn't save the frame").with_body(message.clone()),
        log_path.as_deref(),
    )));
    effects.push(log(
        LogLevel::Error,
        format!(
            "{job}: frame save failed: {message}{}",
            log_note(log_path.as_deref())
        ),
    ));
    Outcome::new(next, effects, None)
}

/// A hand-picked output that appeared while an export or a frame save ran
/// (A§8.8): nothing was replaced and the result was kept next to it. The
/// job ends; its requester gets `confirmation_required` (exit 4) naming
/// both paths, with the file that appeared in `details.files` like the
/// confirmation asked before a job starts; the person gets a warning toast.
pub fn output_appeared(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    out: String,
    bytes: u64,
    kept: String,
) -> Outcome {
    let _ = ctx;
    let Some(entry) = job_of(state, job, |k| {
        matches!(k, JobKind::Export { .. } | JobKind::FrameSave { .. })
    }) else {
        return ignore(
            state,
            format!("OutputAppeared for {job}, which isn't a running export or frame save"),
        );
    };
    let (what, title) = match entry.kind {
        JobKind::Export { .. } => ("export", "Export kept aside"),
        _ => ("frame", "Frame kept aside"),
    };
    let mut next = state.clone();
    next.runtime.jobs.remove(&job);
    let mut effects = Vec::new();
    if let Some(req) = &entry.req {
        let error = CoreError::confirmation_required(format!(
            "{out} appeared while Qrec was writing it; the new {what} was kept as {kept}"
        ))
        .with_hint(format!(
            "rename {kept}, or run the command again with --yes to replace {out}"
        ))
        .with_details(json!({
            "files": [{ "path": out, "bytes": bytes }],
            "kept": kept,
        }));
        effects.push(Effect::Reply {
            req: req.clone(),
            reply: Err(error),
        });
    }
    effects.push(Effect::Notify(
        Toast::new(ToastLevel::Warning, title)
            .with_body(format!(
                "{out} appeared while Qrec was writing it, so it wasn't replaced. The new {what} is {kept}."
            ))
            .with_action("Open folder", UiAction::RevealPath(kept.clone()))
            .with_action("Copy path", UiAction::CopyText(kept.clone())),
    ));
    effects.push(log(
        LogLevel::Warn,
        format!("{job}: {out} appeared meanwhile; the {what} was kept as {kept}"),
    ));
    Outcome::new(next, effects, None)
}

/// An IPC client disconnected (A§5.8): clear `job.req` for every job (export,
/// frame, autocut, purge); in Headless/Cli hosts cancel the jobs it started
/// that nobody else waits for. Never stops a recording.
///
/// Exports and frame saves stay in the table until the shell reports their
/// end (`ExportCancelled`, `FrameFailed`), so purge keeps treating their files
/// as in use. Autocut jobs leave at once (their late analysis results are
/// ignored); a purge has already committed and finishes on its own.
pub fn requester_gone(state: &State, ctx: &Ctx, req: ReqId) -> Outcome {
    let _ = ctx;
    let mut next = state.clone();
    let cancel = !state.runtime.host.keeps_orphaned_jobs();
    let mut effects = Vec::new();
    let mut cancelled = Vec::new();
    let mut dropped = Vec::new();
    for (id, job) in next.runtime.jobs.iter_mut() {
        if job.req.as_ref() != Some(&req) {
            continue;
        }
        job.req = None;
        if !cancel {
            continue;
        }
        match job.kind {
            JobKind::Export { .. } | JobKind::FrameSave { .. } => cancelled.push(*id),
            JobKind::Autocut { .. } => dropped.push(*id),
            JobKind::Purge { .. } => {}
        }
    }
    for id in &dropped {
        next.runtime.jobs.remove(id);
    }
    for id in cancelled.iter().chain(&dropped) {
        effects.push(Effect::CancelJob { job: *id });
    }
    let what = if cancelled.is_empty() && dropped.is_empty() {
        "nothing to cancel".to_owned()
    } else {
        let ids: Vec<String> = cancelled
            .iter()
            .chain(&dropped)
            .map(JobId::to_string)
            .collect();
        format!("cancelled {}", ids.join(", "))
    };
    effects.push(log(
        LogLevel::Debug,
        format!("requester {req} went away: {what}"),
    ));
    Outcome::new(next, effects, None)
}

/// The progress effect for the job's requester, if any.
fn progress_effect(job: &Job, now: DateTime<Utc>) -> Option<Effect> {
    let req = job.req.clone()?;
    let progress = export_progress_view(job, now)?;
    Some(Effect::Progress { req, progress })
}

fn job_of(state: &State, job: JobId, kind: impl Fn(&JobKind) -> bool) -> Option<&Job> {
    state.runtime.jobs.get(&job).filter(|j| kind(&j.kind))
}

fn ignore(state: &State, message: String) -> Outcome {
    Outcome::new(
        state.clone(),
        vec![log(LogLevel::Debug, format!("ignored: {message}"))],
        None,
    )
}

fn log(level: LogLevel, message: String) -> Effect {
    Effect::Log { level, message }
}

fn last_lines(mut lines: Vec<String>) -> Vec<String> {
    let cut = lines.len().saturating_sub(STDERR_TAIL_LINES);
    lines.drain(..cut);
    lines
}

fn failure_body(message: &str, tail: &[String]) -> String {
    if tail.is_empty() {
        message.to_owned()
    } else {
        format!("{message}\n\n{}", tail.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TakeId;
    use crate::protocol::command::{ClipScope, Command, ExportFormat, Quality};
    use crate::protocol::event::Event;
    use crate::state::{AutocutParams, HostKind};
    use crate::testkit::{self, ProjectBuilder};
    use crate::{ErrorCode, apply, on_event};

    const START: &str = "2026-10-14T12:00:00Z";

    fn exporting(host: HostKind) -> State {
        let mut state = testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-1")
                .take("take-0001", 300)
                .clip("c1", "take-0001", 0, 100)
                .clip("c2", "take-0001", 200, 300)
                .build(),
        );
        state.runtime.host = host;
        let cmd = Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Draft,
            codec: None,
            out: None,
        };
        apply(&state, &testkit::ctx(START, 1), cmd).unwrap().state
    }

    fn progress_of(out: &Outcome) -> Progress {
        match out.effects.as_slice() {
            [Effect::Progress { req, progress }] => {
                assert_eq!(req, &ReqId("r1".into()));
                progress.clone()
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn progress_is_weighted_and_has_an_eta() {
        let state = exporting(HostKind::Cli);
        let ctx = testkit::ctx("2026-10-14T12:00:10Z", 2);
        let event = |segment, frames_done| Event::ExportProgress {
            job: JobId(1),
            segment,
            frames_done,
        };
        let out = on_event(&state, &ctx, event(0, 50));
        let p = progress_of(&out);
        // 0.9 · 50/200 of 200 frames = 45; 10 s for 45 of 200 ⇒ 34 s left.
        assert_eq!((p.done, p.total, p.eta_secs), (45, 200, Some(34)));
        assert_eq!(
            (p.phase, p.label.as_str()),
            (ProgressPhase::Render, "Rendering clip 1/2")
        );
        let out = on_event(&out.state, &ctx, event(0, 100));
        assert_eq!(progress_of(&out).label, "Rendering clip 2/2");
        let out = on_event(&out.state, &ctx, event(1, 1000));
        let p = progress_of(&out);
        assert_eq!(
            (p.done, p.label.as_str()),
            (180, "Rendering clip 2/2"),
            "clamped to the segment"
        );
        // Stitching: 0.9 + 0.1 · half.
        let half = frame_ns_of(&out.state) / 2;
        let out = on_event(
            &out.state,
            &ctx,
            Event::ExportStitchProgress {
                job: JobId(1),
                out_time_ns: half,
            },
        );
        let p = progress_of(&out);
        assert_eq!(
            (p.phase, p.label.as_str(), p.done),
            (ProgressPhase::Stitch, "Stitching", 190)
        );
        assert_eq!(p.eta_secs, Some(0));
        let out = on_event(
            &out.state,
            &ctx,
            Event::ExportStitchProgress {
                job: JobId(1),
                out_time_ns: u64::MAX,
            },
        );
        assert_eq!(progress_of(&out).done, 200);

        // Nobody waiting: the state still tracks it, but no progress effect.
        let mut quiet = state.clone();
        quiet.runtime.jobs.get_mut(&JobId(1)).unwrap().req = None;
        let out = on_event(&quiet, &ctx, event(0, 10));
        assert!(out.effects.is_empty());
        assert!(matches!(
            &out.state.runtime.jobs[&JobId(1)].kind,
            JobKind::Export { seg_done, .. } if seg_done == &vec![10, 0]
        ));
    }

    fn at(time: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(time)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn readouts_match_the_export_panel() {
        // BRIEF S15b: "42%", "Rendering clip 6/10", "3,449 / 7,391 frames",
        // "Elapsed 00:00:38 · About 00:00:52 left" (one segment per clip,
        // rendered in parallel: five finished, five under way).
        let seg_frames = vec![410, 457, 268, 307, 238, 249, 155, 1092, 1110, 3105];
        let stitch_total_ns = 246_366_666_666;
        let mut job = Job {
            id: JobId(3),
            kind: JobKind::Export {
                out: "exports/rust-traits-talk-20261014-153012.mp4".into(),
                files: Default::default(),
                total_frames: 7391,
                seg_frames: seg_frames.clone(),
                seg_done: vec![410, 457, 268, 307, 238, 200, 100, 900, 400, 169],
                seg_clips: Vec::new(),
                stitch_total_ns,
                stitch_done_ns: 0,
                stitching: false,
            },
            req: None,
            started: at("2026-10-14T15:30:12Z"),
        };
        let now = at("2026-10-14T15:30:50Z");
        assert_eq!(
            export_readout(&job, now),
            Some(ExportReadout {
                percent: 42,
                rendered: 3449,
                total: 7391,
                label: "Rendering clip 6/10".into(),
                elapsed_secs: 38,
                eta_secs: Some(52),
            })
        );
        assert_eq!(
            export_progress_view(&job, now).map(|p| p.done),
            Some(3104),
            "rendering weighs 90 %: 41.997 % reads 42 %"
        );
        let mut stitch = |done_ns: u64| {
            if let JobKind::Export {
                seg_done,
                stitching,
                stitch_done_ns,
                ..
            } = &mut job.kind
            {
                seg_done.clone_from(&seg_frames);
                *stitching = true;
                *stitch_done_ns = done_ns;
            }
            export_readout(&job, now).unwrap()
        };
        let half = stitch(stitch_total_ns / 2);
        assert_eq!(
            (half.label.as_str(), half.rendered, half.percent),
            ("Stitching", 7391, 95)
        );
        assert_eq!(stitch(stitch_total_ns - 1).percent, 99, "never 100 early");
        assert_eq!(stitch(stitch_total_ns).percent, 100);
        // Nothing done yet: no ETA.
        let fresh = Job {
            kind: JobKind::Export {
                out: "exports/a.mp4".into(),
                files: Default::default(),
                total_frames: 90,
                seg_frames: vec![90],
                seg_done: vec![0],
                seg_clips: vec![vec![90]],
                stitch_total_ns: 3_000_000_000,
                stitch_done_ns: 0,
                stitching: false,
            },
            ..job.clone()
        };
        let readout = export_readout(&fresh, now).unwrap();
        assert_eq!(
            (readout.percent, readout.eta_secs, readout.label.as_str()),
            (0, None, "Rendering clip 1/1")
        );
        // Other jobs have no readout.
        let frame = Job {
            kind: JobKind::FrameSave {
                out: "frames/a.png".into(),
                files: Default::default(),
            },
            ..job
        };
        assert_eq!(export_readout(&frame, now), None);
    }

    #[test]
    fn the_design_export_reads_like_s15b() {
        // The design timeline renders in 9 units: c2d0 and c2d1 (a split)
        // are one run of take 5. The label counts the timeline's 10 clips
        // (BRIEF's "Rendering clip 6/10"): the job knows each unit's clips.
        let state = testkit::design_state();
        let ctx = testkit::ctx("2026-10-14T15:30:12Z", 1);
        let cmd = Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Standard,
            codec: None,
            out: None,
        };
        let mut state = apply(&state, &ctx, cmd).unwrap().state;
        let job = *state.runtime.jobs.keys().next().unwrap();
        let JobKind::Export {
            seg_frames,
            seg_clips,
            ..
        } = &state.runtime.jobs[&job].kind
        else {
            panic!("not an export");
        };
        assert_eq!(seg_frames, &[410, 457, 268, 307, 238, 249, 155, 2202, 3105]);
        assert_eq!(
            seg_clips,
            &[
                vec![410],
                vec![457],
                vec![268],
                vec![307],
                vec![238],
                vec![249],
                vec![155],
                vec![1092, 1110],
                vec![3105]
            ]
        );
        // Five clips rendered, c2d0 not yet: 3,449 frames in all.
        for (segment, frames_done) in [
            (0, 410),
            (1, 457),
            (2, 268),
            (3, 307),
            (4, 238),
            (5, 100),
            (6, 50),
            (7, 1000),
            (8, 619),
        ] {
            let event = Event::ExportProgress {
                job,
                segment,
                frames_done,
            };
            state = on_event(&state, &ctx, event).state;
        }
        let readout =
            export_readout(&state.runtime.jobs[&job], at("2026-10-14T15:30:50Z")).unwrap();
        assert_eq!(
            (
                readout.percent,
                readout.rendered,
                readout.total,
                readout.label.as_str(),
                readout.elapsed_secs,
                readout.eta_secs
            ),
            (42, 3449, 7391, "Rendering clip 6/10", 38, Some(52))
        );
        // Past c2d0's 1,092 frames its unit has rendered one clip whole.
        let event = Event::ExportProgress {
            job,
            segment: 7,
            frames_done: 1100,
        };
        let state = on_event(&state, &ctx, event).state;
        let readout =
            export_readout(&state.runtime.jobs[&job], at("2026-10-14T15:30:50Z")).unwrap();
        assert_eq!(readout.label, "Rendering clip 7/10");
    }

    /// The clips counted whole: each unit's clips its frames cover, or the
    /// segments for a job that doesn't name its clips.
    #[test]
    fn rendered_clips_count_the_timelines_clips() {
        let frames = [410, 2202, 3105];
        let clips = [vec![410], vec![1092, 1110], vec![3105]];
        assert_eq!(rendered_clips(&frames, &[0, 0, 0], &clips), (0, 4));
        assert_eq!(rendered_clips(&frames, &[410, 1091, 0], &clips), (1, 4));
        assert_eq!(rendered_clips(&frames, &[410, 1092, 0], &clips), (2, 4));
        assert_eq!(rendered_clips(&frames, &[410, 2202, 3105], &clips), (4, 4));
        // Without the clips (or not one list per segment): the segments.
        assert_eq!(rendered_clips(&frames, &[410, 1092, 0], &[]), (1, 3));
        assert_eq!(
            rendered_clips(&frames, &[410, 2202, 0], &clips[..2]),
            (2, 3)
        );
        assert_eq!(clips_done(&[1092, 1110], 0), 0);
        assert_eq!(clips_done(&[1092, 1110], 1092), 1);
        assert_eq!(clips_done(&[1092, 1110], 5000), 2);
        assert_eq!(clips_done(&[], 10), 0);
    }

    /// S16b: the done toast reads "<path> · 88.4 MB" with Open file and
    /// Open folder (Copy path is the export panel's); an auto name stays
    /// project-relative, with the name the shell gave the file.
    #[test]
    fn the_done_toast_is_s16b() {
        let state = testkit::design_state();
        let ctx = testkit::ctx("2026-10-14T15:30:12Z", 1);
        let cmd = Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Standard,
            codec: None,
            out: None,
        };
        let state = apply(&state, &ctx, cmd).unwrap().state;
        let job = *state.runtime.jobs.keys().next().unwrap();
        let done = |path: &str| {
            let out = on_event(
                &state,
                &ctx,
                Event::ExportFinished {
                    job,
                    path: path.into(),
                    bytes: 88_400_000,
                },
            );
            out.effects
                .into_iter()
                .find_map(|e| match e {
                    Effect::Notify(t) => Some(t),
                    _ => None,
                })
                .expect("a toast")
        };
        let toast =
            done("/home/me/videos/rust-traits-talk/exports/rust-traits-talk-20261014-153012.mp4");
        assert_eq!(toast.title, EXPORT_DONE_TITLE);
        let shown = "exports/rust-traits-talk-20261014-153012.mp4";
        assert_eq!(
            toast.body.as_deref(),
            Some("exports/rust-traits-talk-20261014-153012.mp4 \u{b7} 88.4 MB")
        );
        let actions: Vec<(&str, &UiAction)> = toast
            .actions
            .iter()
            .map(|a| (a.label.as_str(), &a.action))
            .collect();
        assert_eq!(
            actions,
            [
                ("Open file", &UiAction::OpenPath(shown.into())),
                ("Open folder", &UiAction::RevealPath(shown.into())),
            ]
        );
        // The name was taken: the shell numbered it.
        let numbered = done("/p/exports/rust-traits-talk-20261014-153012-2.mp4");
        assert!(
            numbered
                .body
                .as_deref()
                .unwrap()
                .starts_with("exports/rust-traits-talk-20261014-153012-2.mp4 \u{b7} ")
        );
        assert_eq!(
            shown_output("/home/me/talk-final.mp4", "/home/me/talk-final.mp4"),
            "/home/me/talk-final.mp4",
            "a hand-picked path as the shell reports it"
        );
        assert_eq!(shown_output("", "/p/x.mp4"), "/p/x.mp4");
    }

    fn frame_ns_of(state: &State) -> u64 {
        match &state.runtime.jobs[&JobId(1)].kind {
            JobKind::Export {
                stitch_total_ns, ..
            } => *stitch_total_ns,
            _ => unreachable!(),
        }
    }

    #[test]
    fn stale_events_are_ignored() {
        let state = exporting(HostKind::Cli);
        let ctx = testkit::ctx(START, 2);
        for event in [
            Event::ExportProgress {
                job: JobId(9),
                segment: 0,
                frames_done: 1,
            },
            Event::ExportProgress {
                job: JobId(1),
                segment: 5,
                frames_done: 1,
            },
            Event::ExportStitchProgress {
                job: JobId(9),
                out_time_ns: 1,
            },
            Event::ExportFinished {
                job: JobId(9),
                path: "x".into(),
                bytes: 1,
            },
            Event::ExportFailed {
                job: JobId(9),
                message: "x".into(),
                stderr_tail: vec![],
                log: "l".into(),
            },
            Event::ExportCancelled { job: JobId(9) },
            Event::FrameSaved {
                job: JobId(1),
                path: "x".into(),
            },
            Event::FrameFailed {
                job: JobId(1),
                message: "x".into(),
                log: "l".into(),
            },
        ] {
            let out = on_event(&state, &ctx, event.clone());
            assert_eq!(out.state, state, "{event:?}");
            assert!(out.reply.is_none());
            assert!(
                matches!(
                    out.effects.as_slice(),
                    [Effect::Log {
                        level: LogLevel::Debug,
                        ..
                    }]
                ),
                "{event:?}: {:?}",
                out.effects
            );
        }
    }

    #[test]
    fn finished_failed_cancelled() {
        let state = exporting(HostKind::Cli);
        let ctx = testkit::ctx(START, 2);
        let out = on_event(
            &state,
            &ctx,
            Event::ExportFinished {
                job: JobId(1),
                path: "/p/exports/test-20261014-120000.mp4".into(),
                bytes: 4096,
            },
        );
        assert!(out.state.runtime.jobs.is_empty());
        insta::assert_debug_snapshot!("export_finished_effects", out.effects);

        let tail: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
        let out = on_event(
            &state,
            &ctx,
            Event::ExportFailed {
                job: JobId(1),
                message: "segment 2: ffmpeg exited with 1".into(),
                stderr_tail: tail,
                log: "/p/.qrec/logs/x.log".into(),
            },
        );
        assert!(out.state.runtime.jobs.is_empty());
        let Effect::Reply {
            reply: Err(err), ..
        } = &out.effects[0]
        else {
            panic!("{:?}", out.effects)
        };
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(
            err.message,
            "export failed: segment 2: ffmpeg exited with 1"
        );
        let details = err.details.as_ref().unwrap();
        assert_eq!(details["stderrTail"].as_array().unwrap().len(), 30);
        assert_eq!(details["stderrTail"][0], "line 11");
        let Effect::Notify(toast) = &out.effects[1] else {
            panic!()
        };
        assert_eq!(toast.level, ToastLevel::Error);
        assert!(toast.body.as_ref().unwrap().ends_with("line 40"));

        let out = on_event(&state, &ctx, Event::ExportCancelled { job: JobId(1) });
        assert!(out.state.runtime.jobs.is_empty());
        assert!(matches!(
            &out.effects[0],
            Effect::Reply { reply: Err(e), .. } if e.message == "export cancelled"
        ));
        assert!(matches!(&out.effects[1], Effect::Notify(t) if t.title == "Export cancelled"));
    }

    #[test]
    fn frame_events() {
        let state = exporting(HostKind::Gui);
        let ctx = testkit::ctx(START, 2);
        let cmd = Command::FrameSave {
            at: "5f".parse().unwrap(),
            layer: None,
            out: None,
        };
        let saving = apply(&state, &ctx, cmd).unwrap().state;
        assert!(saving.runtime.jobs.contains_key(&JobId(2)));
        let out = on_event(
            &saving,
            &ctx,
            Event::FrameSaved {
                job: JobId(2),
                path: "/p/frames/a.png".into(),
            },
        );
        assert!(!out.state.runtime.jobs.contains_key(&JobId(2)));
        assert!(
            out.state.runtime.jobs.contains_key(&JobId(1)),
            "the export goes on"
        );
        insta::assert_debug_snapshot!("frame_saved_effects", out.effects);
        let out = on_event(
            &saving,
            &ctx,
            Event::FrameFailed {
                job: JobId(2),
                message: "boom".into(),
                log: "l.log".into(),
            },
        );
        assert!(matches!(
            &out.effects[0],
            Effect::Reply { reply: Err(e), .. } if e.code == ErrorCode::Io && e.message == "saving the frame failed: boom"
        ));
    }

    /// A hand-picked output that appeared while the job ran is a
    /// confirmation (exit 4) naming both paths, for exports and frame saves
    /// alike, never a generic I/O failure (A§8.8).
    #[test]
    fn an_output_that_appeared_meanwhile_needs_confirmation() {
        let state = exporting(HostKind::Cli);
        let ctx = testkit::ctx(START, 2);
        let frame = Command::FrameSave {
            at: "5f".parse().unwrap(),
            layer: None,
            out: None,
        };
        let state = apply(&state, &ctx, frame).unwrap().state;
        for (job, what, title) in [
            (JobId(1), "export", "Export kept aside"),
            (JobId(2), "frame", "Frame kept aside"),
        ] {
            let out = on_event(
                &state,
                &ctx,
                Event::OutputAppeared {
                    job,
                    out: "/home/me/race.webm".into(),
                    bytes: 20,
                    kept: "/home/me/.race.qrec-part-7-1.webm".into(),
                },
            );
            assert!(!out.state.runtime.jobs.contains_key(&job), "{what}");
            assert_eq!(out.state.runtime.jobs.len(), 1, "the other job goes on");
            let [
                Effect::Reply {
                    reply: Err(err), ..
                },
                Effect::Notify(toast),
                Effect::Log {
                    level: LogLevel::Warn,
                    ..
                },
            ] = &out.effects[..]
            else {
                panic!("{:?}", out.effects)
            };
            assert_eq!(err.code, ErrorCode::ConfirmationRequired);
            assert_eq!(err.exit_code(), 4);
            assert_eq!(
                err.message,
                format!(
                    "confirmation required: /home/me/race.webm appeared while Qrec was \
                     writing it; the new {what} was kept as /home/me/.race.qrec-part-7-1.webm"
                )
            );
            assert_eq!(
                err.hint.as_deref(),
                Some(
                    "rename /home/me/.race.qrec-part-7-1.webm, or run the command again \
                     with --yes to replace /home/me/race.webm"
                )
            );
            assert_eq!(
                err.details,
                Some(json!({
                    "files": [{ "path": "/home/me/race.webm", "bytes": 20 }],
                    "kept": "/home/me/.race.qrec-part-7-1.webm",
                }))
            );
            assert_eq!(
                (toast.level, toast.title.as_str()),
                (ToastLevel::Warning, title)
            );
            assert!(matches!(
                &toast.actions[0].action,
                UiAction::RevealPath(p) if p == "/home/me/.race.qrec-part-7-1.webm"
            ));
        }
        // Another job, or none: ignored.
        let out = on_event(
            &state,
            &ctx,
            Event::OutputAppeared {
                job: JobId(9),
                out: "/x".into(),
                bytes: 0,
                kept: "/y".into(),
            },
        );
        assert_eq!(out.state, state);
        assert!(matches!(
            out.effects.as_slice(),
            [Effect::Log {
                level: LogLevel::Debug,
                ..
            }]
        ));
    }

    /// A job that couldn't create its log (a read-only project folder) sends
    /// an empty path: nothing may point at a log that doesn't exist.
    #[test]
    fn failures_without_a_log_point_at_none() {
        let ctx = testkit::ctx(START, 2);
        let state = exporting(HostKind::Cli);
        let no_log = |out: &Outcome| {
            let Effect::Reply { reply: Err(e), .. } = &out.effects[0] else {
                panic!("{:?}", out.effects)
            };
            assert_eq!(e.hint, None, "{e:?}");
            assert!(e.details.as_ref().unwrap()["log"].is_null(), "{e:?}");
            let Effect::Notify(toast) = &out.effects[1] else {
                panic!("{:?}", out.effects)
            };
            assert!(toast.actions.is_empty(), "{toast:?}");
            let Effect::Log { message, .. } = &out.effects[2] else {
                panic!("{:?}", out.effects)
            };
            assert!(!message.contains("(log: )"), "{message}");
        };
        let message = "can't create a log in /p/.qrec/logs: Permission denied (os error 13)";
        let out = on_event(
            &state,
            &ctx,
            Event::ExportFailed {
                job: JobId(1),
                message: message.into(),
                stderr_tail: vec![],
                log: String::new(),
            },
        );
        no_log(&out);
        let cmd = Command::FrameSave {
            at: "5f".parse().unwrap(),
            layer: None,
            out: None,
        };
        let saving = apply(&exporting(HostKind::Gui), &ctx, cmd).unwrap().state;
        let out = on_event(
            &saving,
            &ctx,
            Event::FrameFailed {
                job: JobId(2),
                message: message.into(),
                log: String::new(),
            },
        );
        no_log(&out);
        // With a log, the hint, the detail and the action name it.
        let out = on_event(
            &state,
            &ctx,
            Event::ExportFailed {
                job: JobId(1),
                message: "boom".into(),
                stderr_tail: vec![],
                log: "/p/.qrec/logs/x.log".into(),
            },
        );
        let Effect::Reply { reply: Err(e), .. } = &out.effects[0] else {
            panic!()
        };
        assert_eq!(
            e.hint.as_deref(),
            Some("the full ffmpeg log is /p/.qrec/logs/x.log")
        );
        assert_eq!(e.details.as_ref().unwrap()["log"], "/p/.qrec/logs/x.log");
        let Effect::Notify(toast) = &out.effects[1] else {
            panic!()
        };
        assert_eq!(toast.actions[0].label, "Open log");
    }

    #[test]
    fn requesters_going_away() {
        let ctx = testkit::ctx(START, 2);
        let gone = ReqId("r1".into());
        // A GUI host keeps everything running; only the requester is forgotten.
        let gui = exporting(HostKind::Gui);
        let out = on_event(&gui, &ctx, Event::RequesterGone { req: gone.clone() });
        assert_eq!(out.state.runtime.jobs[&JobId(1)].req, None);
        assert!(
            !out.effects
                .iter()
                .any(|e| matches!(e, Effect::CancelJob { .. }))
        );

        // A CLI host cancels what that requester started.
        let mut state = exporting(HostKind::Headless);
        let mut add = |id: u64, kind: JobKind, req: &str| {
            state.runtime.jobs.insert(
                JobId(id),
                Job {
                    id: JobId(id),
                    kind,
                    req: Some(ReqId(req.into())),
                    started: ctx.now,
                },
            );
        };
        add(
            2,
            JobKind::FrameSave {
                out: "f.png".into(),
                files: Default::default(),
            },
            "r1",
        );
        add(
            3,
            JobKind::Autocut {
                cmd: Command::Autocut {
                    scope: ClipScope::All,
                    overrides: Default::default(),
                    dry_run: false,
                }
                .to_json(),
                targets: vec![],
                params: AutocutParams {
                    threshold_db: -35.0,
                    min_silence_ms: 400,
                    padding_ms: 120,
                    min_keep_ms: 200,
                },
                dry_run: false,
                pending: Default::default(),
                ready: false,
            },
            "r1",
        );
        add(
            4,
            JobKind::Purge {
                takes: vec![TakeId::numbered(1)],
            },
            "r1",
        );
        add(
            5,
            JobKind::FrameSave {
                out: "g.png".into(),
                files: Default::default(),
            },
            "r2",
        );
        let out = on_event(&state, &ctx, Event::RequesterGone { req: gone });
        let cancelled: Vec<JobId> = out
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::CancelJob { job } => Some(*job),
                _ => None,
            })
            .collect();
        assert_eq!(cancelled, vec![JobId(1), JobId(2), JobId(3)]);
        let jobs = &out.state.runtime.jobs;
        assert!(!jobs.contains_key(&JobId(3)), "autocut jobs leave at once");
        assert!(
            jobs.contains_key(&JobId(1)) && jobs.contains_key(&JobId(2)),
            "until the shell reports the end"
        );
        assert!(
            jobs.values()
                .filter(|j| j.id != JobId(5))
                .all(|j| j.req.is_none())
        );
        assert_eq!(jobs[&JobId(5)].req, Some(ReqId("r2".into())));
        assert!(matches!(
            out.effects.last(),
            Some(Effect::Log { level: LogLevel::Debug, message }) if message == "requester r1 went away: cancelled job-1, job-2, job-3"
        ));
    }

    /// What the CLI tells an interrupted person (A§8.6) follows from the
    /// same rules: the commands whose jobs `requester_gone` stops (export,
    /// frame save, autocut; never a purge) in the hosts that stop them.
    #[test]
    fn the_cli_hint_and_requester_gone_agree() {
        let ends = |cmd: Command| cmd.job_ends_with_requester();
        assert!(ends(Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Draft,
            codec: None,
            out: None,
        }));
        assert!(ends(Command::FrameSave {
            at: "1f".parse().unwrap(),
            layer: None,
            out: None,
        }));
        assert!(ends(Command::Autocut {
            scope: ClipScope::All,
            overrides: Default::default(),
            dry_run: false,
        }));
        assert!(!ends(Command::TakePurge { takes: vec![] }));
        assert!(!ends(Command::ClipRemove { clips: vec![] }));
        let ctx = testkit::ctx(START, 2);
        for host in [HostKind::Gui, HostKind::Headless, HostKind::Cli] {
            let state = exporting(host);
            let gone = Event::RequesterGone {
                req: ReqId("r1".into()),
            };
            let out = on_event(&state, &ctx, gone);
            let cancelled = out
                .effects
                .iter()
                .any(|e| matches!(e, Effect::CancelJob { .. }));
            assert_eq!(cancelled, !host.keeps_orphaned_jobs(), "{host:?}");
        }
    }
}
