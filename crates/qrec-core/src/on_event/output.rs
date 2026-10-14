//! Export and frame-save job events (A§5.8, A§5.9), and requesters going
//! away. Owner: output.
//!
//! Events whose job isn't in the job table (or is another kind of job) are
//! ignored with a debug log. A finished, failed or cancelled job leaves the
//! table, its requester gets the reply and the person a toast.

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::ids::{JobId, ReqId};
use crate::protocol::effect::{Effect, LogLevel};
use crate::protocol::progress::{Progress, ProgressPhase};
use crate::protocol::reply::{CmdResult, ExportDone, FrameSavedView};
use crate::protocol::toast::{Toast, ToastLevel, UiAction};
use crate::state::{HostKind, Job, JobKind, State};

/// The toast of a finished export (A§5.9).
pub const EXPORT_DONE_TITLE: &str = "Layers! Like an onion. Your video is ready.";

/// How much ffmpeg stderr a failure shows (SPEC §13.1).
pub const STDERR_TAIL_LINES: usize = 30;

/// The progress of an export job: `0.9·Σseg_done/total + 0.1·stitch` in
/// frames of `total_frames`, ETA from the time since it started, label
/// "Rendering clip 4/12" or "Stitching". `None` for other jobs.
pub(crate) fn export_progress_view(job: &Job, now: DateTime<Utc>) -> Option<Progress> {
    let JobKind::Export {
        total_frames,
        seg_frames,
        seg_done,
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
        let finished = seg_done
            .iter()
            .zip(seg_frames)
            .filter(|(done, frames)| done >= frames)
            .count();
        let count = seg_frames.len();
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
pub fn export_finished(state: &State, ctx: &Ctx, job: JobId, path: String, bytes: u64) -> Outcome {
    let _ = ctx;
    let Some(entry) = job_of(state, job, |k| matches!(k, JobKind::Export { .. })) else {
        return ignore(
            state,
            format!("ExportFinished for {job}, which isn't a running export"),
        );
    };
    let frames = match entry.kind {
        JobKind::Export { total_frames, .. } => total_frames,
        _ => 0,
    };
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
            .with_body(path.clone())
            .with_action("Open file", UiAction::OpenPath(path.clone()))
            .with_action("Open folder", UiAction::RevealPath(path.clone()))
            .with_action("Copy path", UiAction::CopyText(path.clone())),
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
    let mut next = state.clone();
    next.runtime.jobs.remove(&job);
    let mut effects = Vec::new();
    if let Some(req) = &entry.req {
        let error = CoreError::io(format!("export failed: {message}"))
            .with_hint(format!("the full ffmpeg log is {log_path}"))
            .with_details(json!({ "log": log_path, "stderrTail": tail }));
        effects.push(Effect::Reply {
            req: req.clone(),
            reply: Err(error),
        });
    }
    effects.push(Effect::Notify(
        Toast::new(ToastLevel::Error, "Export failed")
            .with_body(failure_body(&message, &tail))
            .with_action("Open log", UiAction::OpenPath(log_path.clone())),
    ));
    effects.push(log(
        LogLevel::Error,
        format!("{job}: export failed: {message} (log: {log_path})"),
    ));
    Outcome::new(next, effects, None)
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
    let mut next = state.clone();
    next.runtime.jobs.remove(&job);
    let mut effects = Vec::new();
    if let Some(req) = &entry.req {
        let error = CoreError::io(format!("saving the frame failed: {message}"))
            .with_hint(format!("the full ffmpeg log is {log_path}"))
            .with_details(json!({ "log": log_path }));
        effects.push(Effect::Reply {
            req: req.clone(),
            reply: Err(error),
        });
    }
    effects.push(Effect::Notify(
        Toast::new(ToastLevel::Error, "Couldn't save the frame")
            .with_body(message.clone())
            .with_action("Open log", UiAction::OpenPath(log_path.clone())),
    ));
    effects.push(log(
        LogLevel::Error,
        format!("{job}: frame save failed: {message} (log: {log_path})"),
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
    let cancel = state.runtime.host != HostKind::Gui;
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
    use crate::state::AutocutParams;
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
}
