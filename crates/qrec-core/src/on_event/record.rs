//! Recording events (A§5.3). Owner: record.
//!
//! Every handler first checks that the event belongs to the current phase
//! (take, timer); anything else is ignored with a debug `Log` (A§4.1). Like
//! the commands, every step that changes facts or the record phase finishes
//! through `history::persist`/`commit`, which emit the `SetLive` the change
//! implies.
//!
//! Finalization order (A§5.3): fill media info and mark unreadable streams
//! `missing` with `persist` (facts first, the phase back to Idle), then
//! place the take with `commit` ("Record take N", `cmd:
//! {"event":"takeFinalized","take":…}`), then the pending discard, the ready
//! autocut jobs and `autoRunAfterTake`, then the `TakeFinalizedView` replies.
//!
//! Streams of a finished take always carry the media fields of their kind:
//! a stream that couldn't be probed (or was never probed, for a broken
//! take) leaves `streams` and its layer/track is marked `missing`, which
//! validation reports as "missing media".

use serde_json::json;

use crate::apply::Outcome;
use crate::apply::record::{
    COUNTDOWN_TICK_MS, ENGINE_STUCK, MIN_TAKE_FRAMES, autocut_hooks, begin_take,
    countdown_progress, discard_last, end_countdown, finalize_progress, join_ids, ordered,
    started_result, stop_capture, unique,
};
use crate::capture_plan::EOS_TIMEOUT_MS;
use crate::ctx::{Ctx, IdGen};
use crate::error::{CoreError, ErrorCode};
use crate::history;
use crate::ids::{ClipId, TakeId, TimerId};
use crate::media_argv::probe_argv;
use crate::model::{Clip, Scene, Stream, StreamKind, Take, TakeStatus};
use crate::protocol::effect::{Effect, LogLevel, ProbeJob, ProbePurpose};
use crate::protocol::event::{MediaInfo, ProbeError, ProbeFailure, ProbeResult, RecordedStream};
use crate::protocol::progress::{Progress, ProgressPhase};
use crate::protocol::reply::{CmdResult, TakeFinalizedView};
use crate::protocol::toast::{Toast, ToastLevel};
use crate::requantize::derive_duration_frames;
use crate::state::{DiscardRequest, RecordMode, RecordPhase, State};
use crate::time::{duration_label, frames_to_ns};
use crate::timeline;

/// Record progress goes to `wait` requesters when the pipeline clock enters
/// the first of these slots of a new second (see [`announces_second`]).
pub const RECORD_PROGRESS_NS: u64 = 500_000_000;

/// Starting ⇒ Recording; reply `RecordStartedView` to the start request
/// (unless `wait`: then it waits for the whole take, starting with a
/// `Recording take N: 00:00` progress).
pub fn capture_started(
    state: &State,
    _ctx: &Ctx,
    take: TakeId,
    t0_ns: u64,
    files: Vec<String>,
) -> Outcome {
    let RecordPhase::Starting {
        take: current,
        mode,
        start_req,
        wait,
    } = &state.runtime.record
    else {
        return ignore(state, format!("CaptureStarted for {take}"));
    };
    if *current != take {
        return ignore(state, format!("CaptureStarted for {take}"));
    }
    let mut next = state.clone();
    let mut waiters = Vec::new();
    let mut answers = Vec::new();
    match start_req {
        Some(req) if *wait => {
            waiters.push(req.clone());
            answers.push(Effect::Progress {
                req: req.clone(),
                progress: record_progress(&take, 0),
            });
        }
        Some(req) => answers.push(Effect::Reply {
            req: req.clone(),
            reply: Ok(started_result(&take, mode, files)),
        }),
        None => {}
    }
    next.runtime.record = RecordPhase::Recording {
        take: take.clone(),
        mode: mode.clone(),
        elapsed_ns: 0,
        waiters,
    };
    let persisted = history::persist(state, next);
    let mut effects = persisted.effects;
    effects.push(Effect::Log {
        level: LogLevel::Info,
        message: format!("{take}: capture started (T0 = {t0_ns} ns)"),
    });
    effects.extend(answers);
    Outcome::new(persisted.state, ordered(effects), None)
}

/// Start/health-window or pipeline-wide failure (A§5.3): the take is
/// `broken` (+`error`) ⇒ Idle; the start request and `wait` requesters get
/// `capture_failed` (the hint names the source), or `missing_dependency`
/// when a GStreamer element isn't installed (`missing`); stop requests get
/// the broken `TakeFinalizedView`. A pipeline failure during a countdown
/// ends the countdown the same way; one while Idle (the live preview) only
/// tells people.
pub fn capture_failed(
    state: &State,
    ctx: &Ctx,
    take: Option<TakeId>,
    source: Option<String>,
    message: String,
    missing: Option<String>,
) -> Outcome {
    let current = state.runtime.record.take().cloned();
    let target = match (&take, current) {
        (Some(t), Some(c)) if *t == c => c,
        (None, Some(c)) => c,
        (None, None) => return pipeline_failed(state, ctx, source, message, missing),
        (Some(t), _) => return ignore(state, format!("CaptureFailed for {t}")),
    };
    let label = source.as_deref().map(|s| source_label(state, &target, s));
    let reason = match &label {
        Some(l) => format!("{l}: {message}"),
        None => message,
    };
    let err = capture_error(&reason, label.as_deref(), missing.as_deref());
    end_broken(state, ctx, state.clone(), &target, &reason, &err)
}

/// `capture failed: <reason>`, with a hint naming the failing source; a
/// GStreamer element that isn't installed is a missing dependency (exit 5,
/// A§4.8) whose hint names it.
fn capture_error(reason: &str, source: Option<&str>, missing: Option<&str>) -> CoreError {
    let message = format!("capture failed: {reason}");
    if let Some(element) = missing {
        return CoreError::new(ErrorCode::MissingDependency, message).with_hint(format!(
            "install the GStreamer plugin that provides {element}; qrec doctor lists what is missing"
        ));
    }
    let hint = match source {
        Some(s) => format!("check {s}; qrec doctor lists capture problems"),
        None => "qrec doctor lists capture problems".to_owned(),
    };
    CoreError::capture_failed(message).with_hint(hint)
}

/// A failure of the live pipeline with no take in flight: a countdown can't
/// lead to a recording any more, so it ends and the start request fails;
/// otherwise people are told.
fn pipeline_failed(
    state: &State,
    ctx: &Ctx,
    source: Option<String>,
    message: String,
    missing: Option<String>,
) -> Outcome {
    // The live pipeline shows the project scene.
    let label = source.as_deref().map(|s| {
        if scene_has(&state.project.scene, s) {
            entry_label(&state.project.scene, s)
        } else {
            s.to_owned()
        }
    });
    let what = match &label {
        Some(l) => format!("{l}: {message}"),
        None => message,
    };
    let mut effects = vec![Effect::Log {
        level: LogLevel::Warn,
        message: format!("capture failed: {what}"),
    }];
    let mut next = state.clone();
    if matches!(state.runtime.record, RecordPhase::Countdown { .. }) {
        let err = capture_error(&what, label.as_deref(), missing.as_deref());
        let (ended, more) = end_countdown(state, ctx, true, err);
        next = ended;
        effects.extend(more);
    }
    effects.push(Effect::Notify(
        Toast::new(ToastLevel::Error, "Capture failed").with_body(what),
    ));
    Outcome::new(next, ordered(effects), None)
}

/// Marks the layer/track `missing` (persist); the last stream lost acts as
/// Stop (A§5.3): the waiters keep waiting for the finalized take.
pub fn stream_lost(
    state: &State,
    _ctx: &Ctx,
    take: TakeId,
    stream: String,
    message: String,
) -> Outcome {
    if state.runtime.record.take() != Some(&take) {
        return ignore(state, format!("StreamLost for {take}/{stream}"));
    }
    let Some(t) = state.project.take(&take) else {
        return ignore(state, format!("StreamLost for the unknown {take}"));
    };
    if !scene_has(&t.scene, &stream) {
        return ignore(
            state,
            format!("StreamLost for the unknown stream {take}/{stream}"),
        );
    }
    if is_missing(&t.scene, &stream) {
        return ignore(
            state,
            format!("StreamLost for {take}/{stream} (already missing)"),
        );
    }
    let n = take.number();
    let label = entry_label(&t.scene, &stream);
    let mut next = state.clone();
    if let Some(t) = next.project.takes.get_mut(&take) {
        set_missing(&mut t.scene, &stream);
    }
    let all_lost = next
        .project
        .take(&take)
        .is_some_and(|t| t.streams.iter().all(|s| is_missing(&t.scene, &s.id)));
    let persisted = history::persist(state, next);
    let mut effects = persisted.effects;
    effects.push(Effect::Log {
        level: LogLevel::Warn,
        message: format!("{take}: lost {stream}: {message}"),
    });
    let capturing = matches!(
        state.runtime.record,
        RecordPhase::Starting { .. } | RecordPhase::Recording { .. }
    );
    let body = match (capturing, all_lost) {
        (true, true) => format!("{message}. That was its last source, so take {n} stops."),
        (true, false) => format!("{message}. Take {n} continues without it."),
        (false, _) => format!("{message}."),
    };
    effects.push(Effect::Notify(
        Toast::new(ToastLevel::Warning, format!("Lost {label}")).with_body(body),
    ));
    let mut next = persisted.state;
    if all_lost && capturing {
        let stopped = stop_capture(&next, None, false);
        next = stopped.state;
        effects.extend(stopped.effects);
    }
    Outcome::new(next, ordered(effects), None)
}

/// Updates `elapsed_ns` (never backwards: the pipeline clock is monotonic
/// and a late event must not rewind it) and sends `Progress{phase: Record}`
/// to the `wait` requesters when [`announces_second`] says so.
pub fn record_clock(state: &State, _ctx: &Ctx, take: TakeId, elapsed_ns: u64) -> Outcome {
    let RecordPhase::Recording {
        take: current,
        elapsed_ns: previous,
        waiters,
        ..
    } = &state.runtime.record
    else {
        return ignore(state, format!("RecordClock for {take}"));
    };
    if *current != take {
        return ignore(state, format!("RecordClock for {take}"));
    }
    let elapsed = elapsed_ns.max(*previous);
    let mut effects = Vec::new();
    if announces_second(*previous, elapsed) {
        for req in waiters {
            effects.push(Effect::Progress {
                req: req.clone(),
                progress: record_progress(&take, elapsed),
            });
        }
    }
    let mut next = state.clone();
    if let RecordPhase::Recording { elapsed_ns: e, .. } = &mut next.runtime.record {
        *e = elapsed;
    }
    Outcome::new(next, effects, None)
}

/// Whether the clock moving from `previous` to `elapsed` announces a new
/// second: it entered a new 500 ms slot that is the first half of a second.
/// Two announcements are then always more than 500 ms apart (A§5.3) —
/// they sit in different seconds' first halves — whatever the clock's tick
/// rate or jitter, and each shows a new `mm:ss`.
pub fn announces_second(previous: u64, elapsed: u64) -> bool {
    let slot = elapsed / RECORD_PROGRESS_NS;
    slot > previous / RECORD_PROGRESS_NS && slot % 2 == 0
}

/// `timed_out` ⇒ `broken`; else record the offsets (persist) and
/// `Probe{Finalize}`. The live pipeline stays wanted until the take is
/// finalized (back in Idle).
pub fn capture_stopped(
    state: &State,
    ctx: &Ctx,
    take: TakeId,
    streams: Vec<RecordedStream>,
    timed_out: bool,
) -> Outcome {
    let RecordPhase::Finalizing {
        take: current,
        stop_reqs,
        waiters,
        ..
    } = &state.runtime.record
    else {
        return ignore(state, format!("CaptureStopped for {take}"));
    };
    if *current != take {
        return ignore(state, format!("CaptureStopped for {take}"));
    }
    if timed_out {
        let reason = format!(
            "the recording didn't stop within {} s",
            EOS_TIMEOUT_MS / 1000
        );
        let err = CoreError::capture_failed(reason.clone());
        return end_broken(state, ctx, state.clone(), &take, &reason, &err);
    }
    let mut next = state.clone();
    let Some(t) = next.project.takes.get_mut(&take) else {
        let reason = "the take is missing from the project".to_owned();
        let err = CoreError::internal(reason.clone());
        return end_broken(state, ctx, state.clone(), &take, &reason, &err);
    };
    let mut notes = Vec::new();
    for recorded in &streams {
        match t.stream_mut(&recorded.id) {
            Some(s) => s.start_offset_ns = recorded.start_offset_ns,
            None => notes.push(Effect::Log {
                level: LogLevel::Warn,
                message: format!(
                    "{take}: the engine reported an unknown stream {}",
                    recorded.id
                ),
            }),
        }
    }
    let jobs = probe_jobs(t);
    let no_eos: Vec<&str> = streams
        .iter()
        .filter(|s| !s.eos)
        .map(|s| s.id.as_str())
        .collect();
    if !no_eos.is_empty() {
        notes.push(Effect::Log {
            level: LogLevel::Warn,
            message: format!("{take}: no EOS from {}", no_eos.join(", ")),
        });
    }
    let progress = finalize_progress(&take, format!("Finalizing take {}", take.number()));
    let reqs = unique(stop_reqs.iter().chain(waiters));
    let persisted = history::persist(state, next);
    let mut effects = persisted.effects;
    effects.extend(notes);
    effects.push(Effect::Probe {
        take,
        purpose: ProbePurpose::Finalize,
        jobs,
    });
    for req in reqs {
        effects.push(Effect::Progress {
            req,
            progress: progress.clone(),
        });
    }
    Outcome::new(persisted.state, ordered(effects), None)
}

/// The engine missed a teardown (A§5.3, A§8.11): the in-flight take (if
/// any) is `broken` with the "stuck" error, a countdown ends, and people are
/// told to restart Qrec. No `StopCapture` is sent to the stuck engine.
///
/// A§5.3 also wants later starts to fail at once; that needs a runtime flag
/// A§3 doesn't define. Until it exists, a later start reaches the engine,
/// which answers `StartCapture` with `CaptureFailed` (the take breaks).
pub fn engine_wedged(state: &State, ctx: &Ctx, message: String) -> Outcome {
    let mut stuck = state.clone();
    stuck.runtime.engine_stuck = Some(if message.is_empty() {
        ENGINE_STUCK.to_owned()
    } else {
        message.clone()
    });
    let state = &stuck;
    let log = Effect::Log {
        level: LogLevel::Error,
        message: format!("capture engine wedged: {message}"),
    };
    let reason = if message.is_empty() {
        ENGINE_STUCK.to_owned()
    } else {
        format!("{ENGINE_STUCK} ({message})")
    };
    let err = CoreError::capture_failed(ENGINE_STUCK)
        .with_hint("restart Qrec; qrec doctor checks the GPU driver");
    let toast = Effect::Notify(
        Toast::new(ToastLevel::Error, "The capture engine is stuck").with_body(reason.clone()),
    );
    match &state.runtime.record {
        RecordPhase::Idle => Outcome::new(state.clone(), vec![log, toast], None),
        RecordPhase::Countdown { .. } => {
            let (next, more) = end_countdown(state, ctx, true, err);
            let mut effects = vec![log];
            effects.extend(more);
            effects.push(toast);
            Outcome::new(next, ordered(effects), None)
        }
        RecordPhase::Starting { take, .. }
        | RecordPhase::Recording { take, .. }
        | RecordPhase::Finalizing { take, .. } => {
            let mut out = end_broken(state, ctx, state.clone(), take, &reason, &err);
            out.effects.insert(0, log);
            out.effects = ordered(out.effects);
            out
        }
    }
}

/// Finalize (A§5.3) or Recover (A§5.3 "Recovery").
pub fn take_probed(
    state: &State,
    ctx: &Ctx,
    take: TakeId,
    purpose: ProbePurpose,
    results: Vec<ProbeResult>,
) -> Outcome {
    match purpose {
        ProbePurpose::Finalize => match &state.runtime.record {
            RecordPhase::Finalizing { take: current, .. } if *current == take => {
                finalize(state, ctx, &take, &results)
            }
            _ => ignore(state, format!("TakeProbed(finalize) for {take}")),
        },
        ProbePurpose::Recover => {
            // Its probe is back, whatever it says: nothing is in flight.
            let mut current = state.clone();
            current.runtime.recovering.remove(&take);
            let stale = state.project.take(&take).is_none_or(|t| {
                t.status != TakeStatus::Recording || state.runtime.record.take() == Some(&take)
            });
            if stale {
                return ignore(&current, format!("TakeProbed(recover) for {take}"));
            }
            recover(state, current, &take, &results)
        }
    }
}

/// Countdown ticks (`Recording in 3…` to the start request); the last one
/// begins the take. A take that can't begin ends the countdown and fails
/// the start request.
pub fn timer_fired(state: &State, ctx: &Ctx, timer: TimerId) -> Outcome {
    let RecordPhase::Countdown {
        remaining,
        mode,
        timer: current,
        start_req,
        wait,
    } = &state.runtime.record
    else {
        return ignore(state, format!("TimerFired for timer {}", timer.0));
    };
    if *current != timer {
        return ignore(state, format!("TimerFired for timer {}", timer.0));
    }
    if *remaining > 1 {
        let mut next = state.clone();
        let tick = next.runtime.alloc_timer_id();
        let left = remaining - 1;
        next.runtime.record = RecordPhase::Countdown {
            remaining: left,
            mode: mode.clone(),
            timer: tick,
            start_req: start_req.clone(),
            wait: *wait,
        };
        let mut effects = vec![Effect::StartTimer {
            timer: tick,
            after_ms: COUNTDOWN_TICK_MS,
        }];
        effects.extend(start_req.iter().map(|req| Effect::Progress {
            req: req.clone(),
            progress: countdown_progress(left),
        }));
        return Outcome::new(next, effects, None);
    }
    match begin_take(state, ctx, mode.clone(), start_req.clone(), *wait) {
        Ok((next, effects)) => Outcome::new(next, effects, None),
        Err(err) => {
            let mut effects = vec![Effect::Log {
                level: LogLevel::Warn,
                message: format!("the take didn't begin: {err}"),
            }];
            let (next, more) = end_countdown(state, ctx, false, err.clone());
            effects.extend(more);
            effects.push(Effect::Notify(
                Toast::new(ToastLevel::Error, "Recording didn't start").with_body(err.message),
            ));
            Outcome::new(next, ordered(effects), None)
        }
    }
}

/// The probes startup recovery runs (A§5.3 "Recovery"): one
/// `Probe{Recover}` per take still `recording` that isn't being captured
/// and isn't being recovered already.
pub fn recovery_probes(state: &State) -> Vec<Effect> {
    let in_flight = state.runtime.record.take();
    state
        .project
        .takes
        .iter()
        .filter(|(id, t)| {
            t.status == TakeStatus::Recording
                && in_flight != Some(*id)
                && !state.runtime.recovering.contains(*id)
        })
        .map(|(id, t)| Effect::Probe {
            take: id.clone(),
            purpose: ProbePurpose::Recover,
            jobs: probe_jobs(t),
        })
        .collect()
}

/// Startup recovery (A§5.3 "Recovery"), run by the shell once
/// `Session::open` holds the lock: the [`recovery_probes`], each take
/// marked in `runtime.recovering` until its `TakeProbed(Recover)` comes
/// back, so a host never exits with a recovery in flight (A§8.6) and an
/// in-process command finishes the recovery it started.
pub fn start_recovery(state: &State) -> Outcome {
    let mut next = state.clone();
    let mut effects = Vec::new();
    for probe in recovery_probes(state) {
        if let Effect::Probe { take, jobs, .. } = &probe {
            next.runtime.recovering.insert(take.clone());
            effects.push(Effect::Log {
                level: LogLevel::Info,
                message: format!("recovering {take}: probing {} file(s)", jobs.len()),
            });
        }
        effects.push(probe);
    }
    Outcome::new(next, effects, None)
}

/// One `ffprobe` per stream of `take` (`media_argv::probe_argv`).
pub fn probe_jobs(take: &Take) -> Vec<ProbeJob> {
    take.streams
        .iter()
        .map(|s| ProbeJob {
            id: s.id.clone(),
            file: s.file.clone(),
            kind: s.kind,
            argv: probe_argv(&s.file, s.kind),
        })
        .collect()
}

// ------------------------------------------------------------ finalizing

/// Where finalization puts the new clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    Append,
    /// Replace `len` clips from `start`: every `ClipRef` still matches its
    /// clip and the clips still sit together.
    Over {
        start: usize,
        len: usize,
    },
    /// The record-over target changed: append and say so.
    Fallback,
}

fn placement_for(clips: &[Clip], mode: &RecordMode) -> Placement {
    let RecordMode::Over { clips: refs } = mode else {
        return Placement::Append;
    };
    let ids: Vec<ClipId> = refs.iter().map(|r| r.id.clone()).collect();
    let all_match = refs.iter().all(|r| clips.iter().any(|c| r.matches(c)));
    if !all_match || !timeline::is_contiguous(clips, &ids) {
        return Placement::Fallback;
    }
    match ids
        .iter()
        .filter_map(|id| timeline::index_of(clips, id))
        .min()
    {
        Some(start) => Placement::Over {
            start,
            len: refs.len(),
        },
        None => Placement::Fallback,
    }
}

/// What finalization did with the take.
enum Verdict {
    TooShort,
    Cancelled,
    Placed(Placement),
}

/// TakeProbed (Finalize), in order (A§5.3).
fn finalize(state: &State, ctx: &Ctx, take: &TakeId, results: &[ProbeResult]) -> Outcome {
    let RecordPhase::Finalizing {
        mode,
        cancelled,
        stop_reqs,
        waiters,
        discard,
        ..
    } = state.runtime.record.clone()
    else {
        return ignore(state, format!("TakeProbed(finalize) for {take}"));
    };
    let n = take.number();
    let fps = state.project.canvas.fps;
    let mut next = state.clone();
    let placement = placement_for(&next.project.timeline.clips, &mode);
    let Some(t) = next.project.takes.get_mut(take) else {
        let reason = "the take is missing from the project".to_owned();
        let err = CoreError::internal(reason.clone());
        return end_broken(state, ctx, state.clone(), take, &reason, &err);
    };
    if let Some(failure) = tool_failure(results) {
        return end_unprobed(state, ctx, take, failure);
    }
    let lost = fill_media(t, results);
    if let Some(reason) = unusable(t) {
        let reason = explained(reason, &lost);
        let err = CoreError::capture_failed(format!("take {n} is broken: {reason}"));
        return end_broken(state, ctx, next, take, &reason, &err);
    }
    let frames = derive_duration_frames(t, fps);
    t.duration_frames = frames;
    t.status = TakeStatus::Ok;
    t.error = None;
    let verdict = if frames < MIN_TAKE_FRAMES {
        t.status = TakeStatus::TooShort;
        Verdict::TooShort
    } else if cancelled {
        Verdict::Cancelled
    } else {
        t.placed = true;
        t.replaced = match placement {
            Placement::Over { start, len } => {
                next.project.timeline.clips[start..start + len].to_vec()
            }
            Placement::Append | Placement::Fallback => Vec::new(),
        };
        Verdict::Placed(placement)
    };
    let status = next.project.takes[take].status;
    next.runtime.record = RecordPhase::Idle;

    // Facts first: media info, missing flags, status, placed/replaced, Idle.
    let facts = history::persist(state, next);
    let mut effects = facts.effects;
    effects.extend(lost_log(take, &lost));
    let mut current = facts.state;
    let mut notes = Vec::new();
    let length = duration_label(frames_to_ns(frames, fps));

    let mut placed_clips: Vec<ClipId> = Vec::new();
    let mut replaced_ids: Vec<ClipId> = Vec::new();
    let mut record_seq = None;
    let message = match verdict {
        Verdict::TooShort => {
            notes.push(Effect::Notify(Toast::new(
                ToastLevel::Warning,
                "That was shorter than Lord Farquaad. Take moved to the bin.",
            )));
            format!("Take {n} was too short ({frames} frames); it is in the bin")
        }
        Verdict::Cancelled => {
            notes.push(Effect::Notify(
                Toast::new(ToastLevel::Info, format!("Take {n} cancelled"))
                    .with_body("It is in the bin; nothing was added to the timeline."),
            ));
            format!("Cancelled take {n} ({length}); it is in the bin")
        }
        Verdict::Placed(placement) => {
            let id = IdGen::for_state(&current, ctx.entropy).next_clip_id();
            let clip = Clip {
                id: id.clone(),
                take: take.clone(),
                in_: 0,
                out: frames,
            };
            let clips = &current.project.timeline.clips;
            let mut edit = current.clone();
            let message = match placement {
                Placement::Over { start, len } => {
                    replaced_ids = clips[start..start + len]
                        .iter()
                        .map(|c| c.id.clone())
                        .collect();
                    edit.project.timeline.clips =
                        timeline::replace_run(clips, start, len, vec![clip]);
                    // A§5.10: after a record-over the new clip is selected.
                    edit.ui.selection = vec![id.clone()];
                    format!(
                        "Recorded take {n} ({length}) over {}",
                        join_ids(&replaced_ids)
                    )
                }
                Placement::Append => {
                    edit.project.timeline.clips = timeline::insert(clips, clips.len(), vec![clip]);
                    format!("Recorded take {n} ({length})")
                }
                Placement::Fallback => {
                    edit.project.timeline.clips = timeline::insert(clips, clips.len(), vec![clip]);
                    notes.push(Effect::Notify(Toast::new(
                        ToastLevel::Warning,
                        "record-over target changed; take appended at the end",
                    )));
                    format!(
                        "Recorded take {n} ({length}); the record-over target changed, so it was appended at the end"
                    )
                }
            };
            let committed = history::commit(
                &current,
                ctx,
                format!("Record take {n}"),
                json!({"event": "takeFinalized", "take": take}),
                edit,
            );
            record_seq = committed.seq;
            effects.extend(committed.effects);
            current = committed.state;
            placed_clips.push(id);
            message
        }
    };
    let placed = !placed_clips.is_empty();

    // The discard pressed while finalizing (A§5.5).
    if let Some(DiscardRequest { req }) = discard {
        let reply = if placed {
            discard_last(&current, ctx).map(|d| {
                effects.extend(d.committed.effects);
                current = d.committed.state;
                d.result.with_seq(d.committed.seq)
            })
        } else {
            Ok(CmdResult::new(format!("take {n} was not placed")))
        };
        if let Some(req) = req {
            effects.push(Effect::Reply { req, reply });
        }
    }

    // Autocut jobs that finished while recording (A§5.7).
    let ready = autocut_hooks::apply_ready_autocuts(&current, ctx);
    current = ready.state;
    effects.extend(ready.effects);

    // autoRunAfterTake on the new clip (SPEC §5.3 step 6).
    if let Some(id) = placed_clips.first() {
        let auto = autocut_hooks::start_auto_run(&current, ctx, id);
        current = auto.state;
        effects.extend(auto.effects);
    }

    let view = TakeFinalizedView {
        take: take.clone(),
        status,
        duration_frames: frames,
        placed,
        clips: placed_clips.clone(),
    };
    let result = CmdResult::new(message)
        .with_seq(record_seq)
        .with_created(&placed_clips)
        .with_removed(&replaced_ids)
        .with_result(&view);
    for req in unique(stop_reqs.iter().chain(&waiters)) {
        effects.push(Effect::Reply {
            req,
            reply: Ok(result.clone()),
        });
    }
    effects.extend(notes);
    Outcome::new(current, ordered(effects), None)
}

/// Recovery (A§5.3): readable ⇒ `ok` (or `tooShort` under 10 frames), bin,
/// "Recovered take 7 from the bog."; otherwise `broken`. Never places clips.
/// A probe that failed on its own (ffprobe missing, timed out) says nothing
/// about the files: the take stays `recording` for the next host start.
/// `current` is `state` with the take's recovery no longer in flight.
fn recover(state: &State, current: State, take: &TakeId, results: &[ProbeResult]) -> Outcome {
    let n = take.number();
    let fps = state.project.canvas.fps;
    if let Some(failure) = tool_failure(results) {
        let effects = vec![
            Effect::Log {
                level: LogLevel::Warn,
                message: format!(
                    "{take}: recovery postponed: {}; it stays `recording` until the probe works",
                    failure.message
                ),
            },
            Effect::Notify(
                Toast::new(ToastLevel::Warning, format!("Take {n} isn't recovered yet"))
                    .with_body(format!(
                        "{}. Its files are kept; Qrec tries again the next time it opens the project.",
                        failure.message
                    )),
            ),
        ];
        return Outcome::new(current, effects, None);
    }
    let mut next = current;
    let Some(mut t) = next.project.takes.get(take).cloned() else {
        return ignore(&next, format!("TakeProbed(recover) for the unknown {take}"));
    };
    let lost = fill_media(&mut t, results);
    let toast = if let Some(reason) = unusable(&t) {
        let reason = explained(reason, &lost);
        mark_broken(&mut t, &format!("recovery failed: {reason}"));
        Toast::new(ToastLevel::Error, format!("Take {n} couldn't be recovered"))
            .with_body(format!("It is marked broken: {reason}."))
    } else {
        let frames = derive_duration_frames(&t, fps);
        t.duration_frames = frames;
        t.error = None;
        let recovered = format!("Recovered take {n} from the bog.");
        if frames < MIN_TAKE_FRAMES {
            t.status = TakeStatus::TooShort;
            Toast::new(ToastLevel::Info, recovered)
                .with_body("It was shorter than Lord Farquaad, so it stays in the bin.")
        } else {
            t.status = TakeStatus::Ok;
            Toast::new(ToastLevel::Success, recovered).with_body("It is in the bin.")
        }
    };
    next.project.takes.insert(take.clone(), t);
    let persisted = history::persist(state, next);
    let mut effects = persisted.effects;
    effects.extend(lost_log(take, &lost));
    effects.push(Effect::Log {
        level: LogLevel::Info,
        message: format!("{take}: recovery: {}", toast.title),
    });
    effects.push(Effect::Notify(toast));
    Outcome::new(persisted.state, ordered(effects), None)
}

// ----------------------------------------------------------------- endings

/// Ends the in-flight phase with `take` broken ⇒ Idle, starting from `next`
/// (the state with whatever facts were gathered). Requests that asked to
/// record (the start request, `wait` requesters while capturing) get `err`;
/// once stopping (Finalizing) the stop requests and waiters get the broken
/// `TakeFinalizedView` and a pending discard is told the take wasn't
/// placed. Back in Idle the ready autocut jobs run.
fn end_broken(
    prev: &State,
    ctx: &Ctx,
    mut next: State,
    take: &TakeId,
    reason: &str,
    err: &CoreError,
) -> Outcome {
    let n = take.number();
    let phase = std::mem::take(&mut next.runtime.record);
    if let Some(t) = next.project.takes.get_mut(take) {
        mark_broken(t, reason);
    }
    let persisted = history::persist(prev, next);
    let mut effects = persisted.effects;
    effects.push(Effect::Log {
        level: LogLevel::Error,
        message: format!("{take} is broken: {reason}"),
    });
    match phase {
        RecordPhase::Starting { start_req, .. } => {
            effects.extend(start_req.map(|req| Effect::Reply {
                req,
                reply: Err(err.clone()),
            }));
        }
        RecordPhase::Recording { waiters, .. } => {
            effects.extend(waiters.into_iter().map(|req| Effect::Reply {
                req,
                reply: Err(err.clone()),
            }));
        }
        RecordPhase::Finalizing {
            stop_reqs,
            waiters,
            discard,
            ..
        } => {
            let view = TakeFinalizedView {
                take: take.clone(),
                status: TakeStatus::Broken,
                duration_frames: 0,
                placed: false,
                clips: Vec::new(),
            };
            let result = CmdResult::new(format!("Take {n} is broken: {reason}")).with_result(&view);
            for req in unique(stop_reqs.iter().chain(&waiters)) {
                effects.push(Effect::Reply {
                    req,
                    reply: Ok(result.clone()),
                });
            }
            if let Some(DiscardRequest { req: Some(req) }) = discard {
                effects.push(Effect::Reply {
                    req,
                    reply: Ok(CmdResult::new(format!("take {n} was not placed"))),
                });
            }
        }
        RecordPhase::Idle | RecordPhase::Countdown { .. } => {}
    }
    let ready = autocut_hooks::apply_ready_autocuts(&persisted.state, ctx);
    effects.extend(ready.effects);
    effects.push(Effect::Notify(
        Toast::new(ToastLevel::Error, format!("Take {n} is broken")).with_body(reason),
    ));
    Outcome::new(ready.state, ordered(effects), None)
}

/// The finalize probe failed on its own (ffprobe missing or timed out):
/// nothing is known about the files, so none of the take's facts change.
/// It stays `recording`, with its streams and offsets, for startup recovery
/// to finish once the probe works (A§5.3 "Recovery"); the phase goes back
/// to Idle. Stop requests and waiters get the error (`missing_dependency`
/// for a missing ffprobe), a pending discard hears the take wasn't placed,
/// and the ready autocut jobs run.
fn end_unprobed(prev: &State, ctx: &Ctx, take: &TakeId, failure: &ProbeError) -> Outcome {
    let n = take.number();
    let mut next = prev.clone();
    let phase = std::mem::take(&mut next.runtime.record);
    let persisted = history::persist(prev, next);
    let mut effects = persisted.effects;
    effects.push(Effect::Log {
        level: LogLevel::Error,
        message: format!(
            "{take} wasn't finalized: {}; it stays `recording` until the probe works",
            failure.message
        ),
    });
    if let RecordPhase::Finalizing {
        stop_reqs,
        waiters,
        discard,
        ..
    } = phase
    {
        let err = unprobed_error(n, failure);
        for req in unique(stop_reqs.iter().chain(&waiters)) {
            effects.push(Effect::Reply {
                req,
                reply: Err(err.clone()),
            });
        }
        if let Some(DiscardRequest { req: Some(req) }) = discard {
            effects.push(Effect::Reply {
                req,
                reply: Ok(CmdResult::new(format!("take {n} was not placed"))),
            });
        }
    }
    let ready = autocut_hooks::apply_ready_autocuts(&persisted.state, ctx);
    effects.extend(ready.effects);
    effects.push(Effect::Notify(
        Toast::new(ToastLevel::Error, format!("Take {n} wasn't finalized")).with_body(format!(
            "{}. Its files are kept; Qrec finalizes it (into the bin) the next time it opens the project.",
            failure.message
        )),
    ));
    Outcome::new(ready.state, ordered(effects), None)
}

/// What stop requests hear when the finalize probe failed on its own.
fn unprobed_error(n: u32, failure: &ProbeError) -> CoreError {
    let message = format!("take {n} wasn't finalized: {}", failure.message);
    let kept = "its files are kept, and Qrec finalizes it the next time it opens the project \
                (any command that changes the project)";
    match failure.kind {
        ProbeFailure::MissingTool => CoreError::new(ErrorCode::MissingDependency, message)
            .with_hint(format!(
                "install ffprobe (it comes with ffmpeg) and check with `qrec doctor`; {kept}"
            )),
        ProbeFailure::Media | ProbeFailure::Tool => CoreError::io(message).with_hint(kept),
    }
}

/// `broken` with `error`; streams without media leave the take and their
/// layers/tracks are marked `missing`. Probed streams keep their facts.
fn mark_broken(take: &mut Take, reason: &str) {
    take.status = TakeStatus::Broken;
    take.error = Some(reason.to_owned());
    take.duration_frames = 0;
    take.placed = false;
    take.replaced.clear();
    let (kept, dropped): (Vec<Stream>, Vec<Stream>) = std::mem::take(&mut take.streams)
        .into_iter()
        .partition(has_media);
    for stream in &dropped {
        set_missing(&mut take.scene, &stream.id);
    }
    take.streams = kept;
}

// ----------------------------------------------------------- media facts

/// The first probe that failed on its own rather than on its file (a
/// missing ffprobe first): such a result says nothing about the media, so
/// it must not change the take's facts.
fn tool_failure(results: &[ProbeResult]) -> Option<&ProbeError> {
    let mut failures = results
        .iter()
        .filter_map(|r| r.info.as_ref().err())
        .filter(|e| e.is_tool_failure());
    let first = failures.next()?;
    Some(
        std::iter::once(first)
            .chain(failures)
            .find(|e| e.kind == ProbeFailure::MissingTool)
            .unwrap_or(first),
    )
}

/// Fills each stream from its probe result; streams without a usable
/// result leave `streams` and are marked `missing`. Returns why each one
/// left (`L1: <what ffprobe said>`).
fn fill_media(take: &mut Take, results: &[ProbeResult]) -> Vec<String> {
    let mut kept = Vec::with_capacity(take.streams.len());
    let mut lost = Vec::new();
    for mut stream in std::mem::take(&mut take.streams) {
        let result = results.iter().find(|r| r.id == stream.id);
        let usable = match result.map(|r| &r.info) {
            Some(Ok(info)) if readable(info, stream.kind) => Ok(info),
            Some(Ok(_)) => Err(format!("no usable {} stream", kind_name(stream.kind))),
            Some(Err(e)) => Err(e.message.clone()),
            None => Err("it wasn't probed".to_owned()),
        };
        match usable {
            Ok(info) => {
                fill(&mut stream, info);
                kept.push(stream);
            }
            Err(why) => {
                set_missing(&mut take.scene, &stream.id);
                lost.push(format!("{}: {why}", stream.id));
            }
        }
    }
    take.streams = kept;
    lost
}

fn kind_name(kind: StreamKind) -> &'static str {
    match kind {
        StreamKind::Video => "video",
        StreamKind::Audio => "audio",
    }
}

/// A broken take's reason with what each probe said:
/// `none of the recorded files could be read (L1: …; A1: …)`.
fn explained(reason: String, lost: &[String]) -> String {
    if lost.is_empty() {
        reason
    } else {
        format!("{reason} ({})", lost.join("; "))
    }
}

/// A warning naming the streams that left the take and why.
fn lost_log(take: &TakeId, lost: &[String]) -> Option<Effect> {
    (!lost.is_empty()).then(|| Effect::Log {
        level: LogLevel::Warn,
        message: format!("{take}: unreadable streams: {}", lost.join("; ")),
    })
}

/// Why a probed take has nothing to use: none of its files could be read
/// ("zero readable streams ⇒ broken", A§5.3), or every readable one belongs
/// to a source lost during the take (missing streams don't count, A§2.2).
fn unusable(take: &Take) -> Option<String> {
    if take.streams.is_empty() {
        return Some("none of the recorded files could be read".to_owned());
    }
    if take.streams.iter().all(|s| is_missing(&take.scene, &s.id)) {
        return Some("every source was lost during the take".to_owned());
    }
    None
}

/// A video stream needs packets and a size; audio needs a rate and channels.
fn readable(info: &MediaInfo, kind: StreamKind) -> bool {
    info.kind == kind
        && match kind {
            StreamKind::Video => {
                info.frames.unwrap_or(0) > 0 && info.width.is_some() && info.height.is_some()
            }
            StreamKind::Audio => info.sample_rate.is_some() && info.channels.is_some(),
        }
}

fn fill(stream: &mut Stream, info: &MediaInfo) {
    stream.duration_ns = info.duration_ns;
    match stream.kind {
        StreamKind::Video => {
            stream.width = info.width;
            stream.height = info.height;
            stream.frames = info.frames;
            stream.fps = info.fps;
        }
        StreamKind::Audio => {
            stream.sample_rate = info.sample_rate;
            stream.channels = info.channels;
        }
    }
}

/// The media fields a finished take's stream must have.
fn has_media(stream: &Stream) -> bool {
    match stream.kind {
        StreamKind::Video => {
            stream.width.is_some() && stream.height.is_some() && stream.frames.is_some()
        }
        StreamKind::Audio => stream.sample_rate.is_some() && stream.channels.is_some(),
    }
}

// ----------------------------------------------------------------- scenes

fn scene_has(scene: &Scene, id: &str) -> bool {
    scene.layers.iter().any(|l| l.id.as_str() == id)
        || scene.audio.iter().any(|t| t.id.as_str() == id)
}

fn is_missing(scene: &Scene, id: &str) -> bool {
    scene
        .layers
        .iter()
        .any(|l| l.id.as_str() == id && l.missing)
        || scene.audio.iter().any(|t| t.id.as_str() == id && t.missing)
}

fn set_missing(scene: &mut Scene, id: &str) {
    for layer in scene.layers.iter_mut().filter(|l| l.id.as_str() == id) {
        layer.missing = true;
    }
    for track in scene.audio.iter_mut().filter(|t| t.id.as_str() == id) {
        track.missing = true;
    }
}

/// `L2 (Logitech C920)` for a scene entry, the id itself otherwise.
fn entry_label(scene: &Scene, id: &str) -> String {
    if let Some(layer) = scene.layers.iter().find(|l| l.id.as_str() == id) {
        return format!("{id} ({})", layer.source.label());
    }
    if let Some(track) = scene.audio.iter().find(|t| t.id.as_str() == id) {
        return format!("{id} ({})", track.label);
    }
    id.to_owned()
}

/// The failing source as people know it: a layer/track of the take, or the
/// engine's own words (a device name).
fn source_label(state: &State, take: &TakeId, source: &str) -> String {
    match state.project.take(take) {
        Some(t) if scene_has(&t.scene, source) => entry_label(&t.scene, source),
        _ => source.to_owned(),
    }
}

// ------------------------------------------------------------------ misc

/// Events that don't belong to the current phase or timer are ignored with
/// a debug log (A§4.1).
fn ignore(state: &State, what: String) -> Outcome {
    Outcome::new(
        state.clone(),
        vec![Effect::Log {
            level: LogLevel::Debug,
            message: format!(
                "ignored {what} (record phase: {})",
                phase_name(&state.runtime.record)
            ),
        }],
        None,
    )
}

fn phase_name(phase: &RecordPhase) -> String {
    match phase {
        RecordPhase::Idle => "idle".to_owned(),
        RecordPhase::Countdown { remaining, .. } => format!("countdown {remaining}"),
        RecordPhase::Starting { take, .. } => format!("starting {take}"),
        RecordPhase::Recording { take, .. } => format!("recording {take}"),
        RecordPhase::Finalizing { take, .. } => format!("finalizing {take}"),
    }
}

/// `Recording take 3: 01:05` (pipeline time); `done` is in milliseconds.
fn record_progress(take: &TakeId, elapsed_ns: u64) -> Progress {
    let secs = elapsed_ns / 1_000_000_000;
    let clock = if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    };
    Progress {
        phase: ProgressPhase::Record,
        done: elapsed_ns / 1_000_000,
        total: 0,
        eta_secs: None,
        label: format!("Recording take {}: {clock}", take.number()),
        take: Some(take.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Fps;
    use crate::state::ClipRef;
    use crate::testkit::ProjectBuilder;

    fn clip(id: &str, take: u32, in_: u64, out: u64) -> Clip {
        Clip {
            id: ClipId::from_raw(id),
            take: TakeId::numbered(take),
            in_,
            out,
        }
    }

    fn info(kind: StreamKind) -> MediaInfo {
        MediaInfo {
            kind,
            frames: (kind == StreamKind::Video).then_some(60),
            fps: (kind == StreamKind::Video).then_some(Fps::new(30, 1)),
            width: (kind == StreamKind::Video).then_some(1280),
            height: (kind == StreamKind::Video).then_some(720),
            sample_rate: (kind == StreamKind::Audio).then_some(48_000),
            channels: (kind == StreamKind::Audio).then_some(2),
            duration_ns: Some(2_000_000_000),
            start_time_ns: 0,
            color_range: None,
            color_space: None,
        }
    }

    /// Take 2 (screen, camera, mic) as it is while recording.
    fn recording_take() -> Take {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-4")
            .camera_layer("L2", "/dev/video0")
            .mic("A1", "alsa_input.mic")
            .take("take-0002", 10)
            .build();
        let mut take = project.takes[&TakeId::numbered(2)].clone();
        take.status = TakeStatus::Recording;
        for s in &mut take.streams {
            s.width = None;
            s.height = None;
            s.frames = None;
            s.fps = None;
            s.sample_rate = None;
            s.channels = None;
            s.duration_ns = None;
        }
        take
    }

    fn result(id: &str, info: Result<MediaInfo, ProbeError>) -> ProbeResult {
        ProbeResult {
            id: id.to_owned(),
            file: format!("takes/take-0002/{id}"),
            info,
        }
    }

    #[test]
    fn placement_checks_every_clip_ref() {
        let clips = vec![
            clip("c1", 1, 0, 10),
            clip("c2", 1, 10, 20),
            clip("c3", 1, 20, 30),
        ];
        assert_eq!(
            placement_for(&clips, &RecordMode::Append),
            Placement::Append
        );
        let over = |refs: &[&Clip]| RecordMode::Over {
            clips: refs.iter().map(|c| ClipRef::of(c)).collect(),
        };
        assert_eq!(
            placement_for(&clips, &over(&[&clips[1], &clips[2]])),
            Placement::Over { start: 1, len: 2 }
        );
        // The run was reordered but still sits together: still replaced.
        let swapped = vec![clips[0].clone(), clips[2].clone(), clips[1].clone()];
        assert_eq!(
            placement_for(&swapped, &over(&[&clips[1], &clips[2]])),
            Placement::Over { start: 1, len: 2 }
        );
        // A trimmed clip, a gone clip, a run that no longer sits together.
        assert_eq!(
            placement_for(&clips, &over(&[&clip("c2", 1, 10, 19)])),
            Placement::Fallback
        );
        assert_eq!(
            placement_for(&clips, &over(&[&clip("c9", 1, 0, 10)])),
            Placement::Fallback
        );
        assert_eq!(
            placement_for(&clips, &over(&[&clips[0], &clips[2]])),
            Placement::Fallback
        );
        assert_eq!(
            placement_for(&clips, &over(&[&clips[2], &clip("c4", 1, 30, 40)])),
            Placement::Fallback,
            "a clip of the run is gone"
        );
        assert_eq!(placement_for(&clips, &over(&[])), Placement::Fallback);
    }

    #[test]
    fn media_is_filled_and_unreadable_streams_leave() {
        let mut take = recording_take();
        let results = vec![
            result("L1", Ok(info(StreamKind::Video))),
            result("L2", Ok(info(StreamKind::Audio))),
            result("A1", Ok(info(StreamKind::Audio))),
            result("L9", Ok(info(StreamKind::Video))),
        ];
        fill_media(&mut take, &results);
        let ids: Vec<&str> = take.streams.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["L1", "A1"]);
        let l1 = take.stream("L1").unwrap();
        assert_eq!(
            (l1.width, l1.height, l1.frames, l1.fps, l1.duration_ns),
            (
                Some(1280),
                Some(720),
                Some(60),
                Some(Fps::new(30, 1)),
                Some(2_000_000_000)
            )
        );
        let a1 = take.stream("A1").unwrap();
        assert_eq!((a1.sample_rate, a1.channels), (Some(48_000), Some(2)));
        assert!(take.scene.layers[1].missing, "L2 probed as audio");
        assert!(!take.scene.layers[0].missing && !take.scene.audio[0].missing);
        assert_eq!(unusable(&take), None);

        // A video without packets is unreadable; so is a missing result.
        let mut take = recording_take();
        let mut empty = info(StreamKind::Video);
        empty.frames = Some(0);
        fill_media(&mut take, &[result("L1", Ok(empty))]);
        assert!(take.streams.is_empty());
        assert!(take.scene.layers.iter().all(|l| l.missing));
        assert!(take.scene.audio.iter().all(|t| t.missing));
        assert_eq!(
            unusable(&take).as_deref(),
            Some("none of the recorded files could be read")
        );
    }

    #[test]
    fn a_take_whose_every_source_was_lost_is_unusable() {
        let mut take = recording_take();
        for id in ["L1", "L2", "A1"] {
            set_missing(&mut take.scene, id);
        }
        let results: Vec<ProbeResult> = take
            .streams
            .iter()
            .map(|s| result(&s.id, Ok(info(s.kind))))
            .collect();
        fill_media(&mut take, &results);
        assert_eq!(take.streams.len(), 3, "the files are readable");
        assert_eq!(
            unusable(&take).as_deref(),
            Some("every source was lost during the take")
        );
    }

    #[test]
    fn broken_takes_keep_only_probed_streams() {
        let mut take = recording_take();
        fill_media(
            &mut take,
            &[
                result("L1", Ok(info(StreamKind::Video))),
                result("L2", Ok(info(StreamKind::Video))),
                result("A1", Ok(info(StreamKind::Audio))),
            ],
        );
        take.streams[1].frames = None;
        take.placed = true;
        take.replaced = vec![clip("c1", 1, 0, 10)];
        mark_broken(&mut take, "boom");
        assert_eq!(take.status, TakeStatus::Broken);
        assert_eq!(take.error.as_deref(), Some("boom"));
        assert_eq!(take.duration_frames, 0);
        assert!(!take.placed && take.replaced.is_empty());
        let ids: Vec<&str> = take.streams.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["L1", "A1"]);
        assert!(take.scene.layers[1].missing);
    }

    #[test]
    fn seconds_are_announced_more_than_half_a_second_apart() {
        // Ticks every ~250 ms with jitter right at the slot edges.
        let ticks = [
            249u64, 499, 501, 749, 999, 1001, 1249, 1499, 1501, 1999, 2001,
        ];
        let mut previous = 0;
        let mut sent = Vec::new();
        for ms in ticks {
            let now = ms * 1_000_000;
            if announces_second(previous, now) {
                sent.push(ms);
            }
            previous = now;
        }
        assert_eq!(sent, [1001, 2001]);
        // A slow clock that skips the first half of a second skips it.
        assert!(!announces_second(900_000_000, 1_600_000_000));
        assert!(announces_second(1_600_000_000, 2_100_000_000));
        // Never twice in one second, never backwards.
        assert!(!announces_second(1_000_000_000, 1_400_000_000));
        assert!(!announces_second(2_000_000_000, 1_000_000_000));
    }

    #[test]
    fn record_progress_labels() {
        let take = TakeId::numbered(3);
        assert_eq!(
            record_progress(&take, 65_400_000_000).label,
            "Recording take 3: 01:05"
        );
        let long = record_progress(&take, 3_725_000_000_000);
        assert_eq!(long.label, "Recording take 3: 1:02:05");
        assert_eq!(long.done, 3_725_000);
        assert_eq!(long.phase, ProgressPhase::Record);
    }

    #[test]
    fn probe_jobs_use_the_media_argv() {
        let take = recording_take();
        let jobs = probe_jobs(&take);
        assert_eq!(jobs.len(), 3);
        assert_eq!(
            jobs[0].argv,
            probe_argv(&take.streams[0].file, StreamKind::Video)
        );
        assert_eq!(jobs[2].kind, StreamKind::Audio);
        assert_eq!(jobs[2].file, "takes/take-0002/A1-mic.mka");
    }
}
