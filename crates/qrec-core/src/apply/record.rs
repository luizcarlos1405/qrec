//! Recording commands (A§5.3 state machine, A§5.5 discard). Owner: record.
//!
//! `apply` has already rejected non-R commands while not `Idle`; these
//! handlers implement the command rows of the A§5.3 table. The helpers the
//! event side (`on_event::record`) shares live here too: beginning a take,
//! stopping the capture, ending a countdown, discarding the last take, the
//! autocut hooks, reply results and the A§7.6 effect order.
//!
//! Every step that changes the record phase, facts or the UI finishes
//! through `history::persist` (or `commit` for edits), which also emits the
//! `SetLive` the change implies (`apply::ui::live_scene`: the pipeline runs
//! while the GUI shows Live or while `record != Idle`, countdown included).
//! `StartCapture`/`StopCapture` come after those effects, so the engine
//! gets the pipeline before the record bins.
//!
//! Begin take: `ctx::next_take_number`, insert the take (`status:
//! recording`, scene snapshot, planned streams) with `persist`, then
//! `CreateTakeDir`, `WriteProject`, `SetLive` (if needed) and
//! `StartCapture`.

use std::collections::BTreeSet;

use crate::apply::Outcome;
use crate::capture_plan::{self, plan_capture, planned_streams};
use crate::ctx::{Ctx, next_take_number};
use crate::error::CoreError;
use crate::history::{self, Committed};
use crate::ids::{ClipId, ReqId, TakeId};
use crate::model::{Clip, EncoderPref, Project, Take, TakeStatus};
use crate::plans::{CapturePlan, VideoEncoder};
use crate::protocol::command::{Command, RecordModeArg};
use crate::protocol::effect::{Effect, LogLevel};
use crate::protocol::progress::{Progress, ProgressPhase};
use crate::protocol::reply::{CmdResult, RecordStartedView};
use crate::state::{ClipRef, DiscardRequest, JobKind, RecordMode, RecordPhase, State};
use crate::timeline;

/// `recording.countdownSecs` and `--countdown` are 0–10 (SPEC §5.5).
pub const MAX_COUNTDOWN_SECS: u32 = 10;
/// One countdown tick.
pub const COUNTDOWN_TICK_MS: u64 = 1_000;
/// Takes shorter than this become `tooShort` (SPEC §5.3).
pub const MIN_TAKE_FRAMES: u64 = 10;
/// Why a take breaks once the engine missed a teardown (A§5.3).
pub const ENGINE_STUCK: &str = "the capture engine is stuck (GPU driver?); restart Qrec";

/// `qrec record start`: countdown > 0 ⇒ Countdown + `StartTimer`; else begin take.
///
/// `Busy` while an export runs; `Invalid` for an empty scene; record-over
/// needs contiguous existing clips (`OverSelection` without a selection is
/// `Unavailable`). The start request is answered later: `RecordStartedView`
/// once capture runs, or with `wait` the `TakeFinalizedView`.
pub fn record_start(
    state: &State,
    ctx: &Ctx,
    mode: RecordModeArg,
    countdown: Option<u32>,
    wait: bool,
) -> Result<Outcome, CoreError> {
    if !state.runtime.record.is_idle() {
        return Err(busy_phase(&state.runtime.record));
    }
    let secs = match countdown {
        Some(secs) if secs > MAX_COUNTDOWN_SECS => {
            return Err(CoreError::usage(format!(
                "the countdown must be 0–{MAX_COUNTDOWN_SECS} seconds, not {secs}"
            )));
        }
        Some(secs) => secs,
        None => project_countdown(&state.project),
    };
    check_can_start(state)?;
    let mode = resolve_mode(state, mode)?;
    start(state, ctx, mode, secs, wait)
}

/// `qrec record stop`: finalize (the caller waits for `TakeFinalizedView`).
/// During a countdown it cancels the countdown; while finalizing the caller
/// joins the stop requests.
pub fn record_stop(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    match &state.runtime.record {
        RecordPhase::Idle => Err(not_recording()),
        RecordPhase::Countdown { .. } => Ok(cancel_countdown(state, ctx)),
        RecordPhase::Starting { .. } | RecordPhase::Recording { .. } => {
            Ok(stop_capture(state, ctx.req.clone(), false))
        }
        RecordPhase::Finalizing { .. } => Ok(join_stop(state, ctx)),
    }
}

/// `qrec record cancel`: files kept, take goes to the bin, no clip.
pub fn record_cancel(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    match &state.runtime.record {
        RecordPhase::Idle => Err(not_recording()),
        RecordPhase::Countdown { .. } => Ok(cancel_countdown(state, ctx)),
        RecordPhase::Starting { .. } | RecordPhase::Recording { .. } => {
            Ok(stop_capture(state, ctx.req.clone(), true))
        }
        phase @ RecordPhase::Finalizing { .. } => Err(busy_phase(phase)),
    }
}

/// `qrec record toggle`: start when idle (append, the project's countdown),
/// stop otherwise; `Busy` while finalizing.
pub fn record_toggle(state: &State, ctx: &Ctx, wait: bool) -> Result<Outcome, CoreError> {
    match &state.runtime.record {
        RecordPhase::Idle => {
            check_can_start(state)?;
            let secs = project_countdown(&state.project);
            start(state, ctx, RecordMode::Append, secs, wait)
        }
        RecordPhase::Countdown { .. } => Ok(cancel_countdown(state, ctx)),
        RecordPhase::Starting { .. } | RecordPhase::Recording { .. } => {
            Ok(stop_capture(state, ctx.req.clone(), false))
        }
        phase @ RecordPhase::Finalizing { .. } => Err(busy_phase(phase)),
    }
}

/// `qrec take discard-last` (A§5.5); while finalizing it is remembered and
/// applied after placement (the requester is answered then).
pub fn take_discard_last(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    match &state.runtime.record {
        RecordPhase::Idle => {
            let discarded = discard_last(state, ctx)?;
            Ok(Outcome::done(discarded.committed, discarded.result))
        }
        RecordPhase::Finalizing { take, discard, .. } => {
            if discard.is_some() {
                return Err(CoreError::busy(format!(
                    "take {} is being finalized and its discard is already queued",
                    take.number()
                )));
            }
            let mut next = state.clone();
            if let RecordPhase::Finalizing { discard, .. } = &mut next.runtime.record {
                *discard = Some(DiscardRequest {
                    req: ctx.req.clone(),
                });
            }
            Ok(Outcome::new(next, Vec::new(), None))
        }
        phase => Err(busy_phase(phase)),
    }
}

// ------------------------------------------------------------- starting

/// The project's countdown; a hand-edited value above the limit counts as
/// the limit (only an explicit `--countdown` is a usage error).
fn project_countdown(project: &Project) -> u32 {
    project.recording.countdown_secs.min(MAX_COUNTDOWN_SECS)
}

/// What an Idle `Start`/`Toggle` needs: no export running and a scene with
/// something to record.
pub(crate) fn check_can_start(state: &State) -> Result<(), CoreError> {
    let export = state
        .runtime
        .jobs
        .values()
        .find(|j| matches!(j.kind, JobKind::Export { .. }));
    if let Some(job) = export {
        return Err(
            CoreError::busy(format!("an export is running ({})", job.id))
                .with_hint("wait for it to finish, or cancel it with qrec export cancel"),
        );
    }
    if state.project.scene.is_empty() {
        return Err(capture_plan::empty_scene());
    }
    Ok(())
}

/// Resolves `--over`/`--over-selection` into the `ClipRef`s the take will
/// replace (timeline order, each once).
fn resolve_mode(state: &State, mode: RecordModeArg) -> Result<RecordMode, CoreError> {
    match mode {
        RecordModeArg::Append => Ok(RecordMode::Append),
        RecordModeArg::Over(ids) => over_refs(state, &ids),
        RecordModeArg::OverSelection => {
            if state.ui.selection.is_empty() {
                return Err(
                    CoreError::unavailable("no clips are selected to record over")
                        .with_hint("select clips on the timeline, or pass --over <clip>… instead"),
                );
            }
            over_refs(state, &state.ui.selection)
        }
    }
}

/// Record over needs existing clips that sit next to each other.
fn over_refs(state: &State, ids: &[ClipId]) -> Result<RecordMode, CoreError> {
    if ids.is_empty() {
        return Err(CoreError::usage("record over needs at least one clip"));
    }
    let clips = &state.project.timeline.clips;
    if let Some(id) = ids
        .iter()
        .find(|id| timeline::index_of(clips, id).is_none())
    {
        return Err(timeline::unknown_clip(id));
    }
    if !timeline::is_contiguous(clips, ids) {
        return Err(CoreError::invalid("record over needs contiguous clips")
            .with_hint("pick clips that sit next to each other on the timeline"));
    }
    let wanted: BTreeSet<&ClipId> = ids.iter().collect();
    Ok(RecordMode::Over {
        clips: clips
            .iter()
            .filter(|c| wanted.contains(&c.id))
            .map(ClipRef::of)
            .collect(),
    })
}

/// Countdown when `secs > 0`, else the take begins right away. Either way
/// playback stops.
fn start(
    state: &State,
    ctx: &Ctx,
    mode: RecordMode,
    secs: u32,
    wait: bool,
) -> Result<Outcome, CoreError> {
    if secs == 0 {
        let (next, effects) = begin_take(state, ctx, mode, ctx.req.clone(), wait)?;
        return Ok(Outcome::new(next, effects, None));
    }
    let mut next = state.clone();
    next.runtime.playing = false;
    let timer = next.runtime.alloc_timer_id();
    next.runtime.record = RecordPhase::Countdown {
        remaining: secs,
        mode,
        timer,
        start_req: ctx.req.clone(),
        wait,
    };
    let persisted = history::persist(state, next);
    let mut effects = persisted.effects;
    effects.push(Effect::StartTimer {
        timer,
        after_ms: COUNTDOWN_TICK_MS,
    });
    effects.extend(ctx.req.iter().map(|req| Effect::Progress {
        req: req.clone(),
        progress: countdown_progress(secs),
    }));
    Ok(Outcome::new(persisted.state, ordered(effects), None))
}

/// Begin take (A§5.3): allocate the take number, insert the take
/// (`recording`, scene snapshot, planned streams) with `persist`, then
/// `CreateTakeDir`, `WriteProject`, the `SetLive` the phase change implies
/// (none after a countdown, which already wanted the pipeline) and
/// `StartCapture`. The phase becomes `Starting`; playback stops.
pub(crate) fn begin_take(
    state: &State,
    ctx: &Ctx,
    mode: RecordMode,
    start_req: Option<ReqId>,
    wait: bool,
) -> Result<(State, Vec<Effect>), CoreError> {
    check_can_start(state)?;
    let number = next_take_number(&state.project, &ctx.facts);
    let take = TakeId::numbered(number);
    if state.project.takes.contains_key(&take) {
        return Err(CoreError::internal(format!("{take} already exists")));
    }
    let plan = plan_capture(
        &state.project,
        &take,
        &state.runtime.capabilities,
        &ctx.facts,
    )?;
    let mut next = state.clone();
    next.project.takes.insert(
        take.clone(),
        Take {
            status: TakeStatus::Recording,
            recorded_at: ctx.now,
            duration_frames: 0,
            scene: state.project.scene.clone(),
            streams: planned_streams(&plan),
            error: None,
            placed: false,
            replaced: Vec::new(),
        },
    );
    next.project.next_take_number = number.saturating_add(1);
    next.runtime.playing = false;
    next.runtime.record = RecordPhase::Starting {
        take,
        mode,
        start_req,
        wait,
    };
    let persisted = history::persist(state, next);
    let mut effects = vec![Effect::CreateTakeDir {
        path: plan.dir.clone(),
    }];
    effects.extend(persisted.effects);
    if let Some(message) = encoder_warning(state, ctx, &plan) {
        effects.push(Effect::Log {
            level: LogLevel::Warn,
            message,
        });
    }
    effects.push(Effect::StartCapture { plan });
    Ok((persisted.state, ordered(effects)))
}

/// VA-API was asked for explicitly but the plan records with x264.
fn encoder_warning(state: &State, ctx: &Ctx, plan: &CapturePlan) -> Option<String> {
    let pref = match state.project.recording.encoder {
        EncoderPref::Auto => ctx.facts.config.encoder.preference,
        explicit => explicit,
    };
    let x264 = plan
        .video
        .first()
        .is_some_and(|v| matches!(v.encoder, VideoEncoder::X264 { .. }));
    (pref == EncoderPref::Vaapi && x264).then(|| {
        "VA-API recording is configured, but no VA H.264 encoder was found for the render node; recording with x264"
            .to_owned()
    })
}

// ------------------------------------------------------------- stopping

/// Countdown + Stop/Toggle/Cancel: `CancelTimer` ⇒ Idle; the start request
/// gets `Invalid` "countdown cancelled", the caller gets Ok.
fn cancel_countdown(state: &State, ctx: &Ctx) -> Outcome {
    let (next, effects) =
        end_countdown(state, ctx, true, CoreError::invalid("countdown cancelled"));
    Outcome::new(
        next,
        effects,
        Some(Ok(CmdResult::new("Countdown cancelled"))),
    )
}

/// Ends a countdown ⇒ Idle: `CancelTimer` (unless the timer just fired),
/// the start request gets `err`, and the autocut jobs that finished meanwhile
/// are applied (A§5.7). Not in a countdown: nothing changes.
pub(crate) fn end_countdown(
    state: &State,
    ctx: &Ctx,
    cancel_timer: bool,
    err: CoreError,
) -> (State, Vec<Effect>) {
    let RecordPhase::Countdown {
        timer, start_req, ..
    } = &state.runtime.record
    else {
        return (state.clone(), Vec::new());
    };
    let mut next = state.clone();
    next.runtime.record = RecordPhase::Idle;
    let persisted = history::persist(state, next);
    let mut effects = persisted.effects;
    if cancel_timer {
        effects.push(Effect::CancelTimer { timer: *timer });
    }
    if let Some(req) = start_req {
        effects.push(Effect::Reply {
            req: req.clone(),
            reply: Err(err),
        });
    }
    let ready = autocut_hooks::apply_ready_autocuts(&persisted.state, ctx);
    effects.extend(ready.effects);
    (ready.state, ordered(effects))
}

/// Starting/Recording + Stop (`cancel: false`) or Cancel: `StopCapture` ⇒
/// Finalizing. `stopper` waits for the finalized reply. Leaving Starting
/// answers the start request now (its started reply) unless it waits for
/// the whole take, in which case it joins the waiters.
pub(crate) fn stop_capture(state: &State, stopper: Option<ReqId>, cancel: bool) -> Outcome {
    let (take, mode, started, waiters) = match &state.runtime.record {
        RecordPhase::Starting {
            take,
            mode,
            start_req,
            wait,
        } => {
            let (started, waiters) = match start_req {
                Some(req) if *wait => (None, vec![req.clone()]),
                Some(req) => {
                    let files = state
                        .project
                        .take(take)
                        .map(|t| t.streams.iter().map(|s| s.file.clone()).collect())
                        .unwrap_or_default();
                    let reply = Effect::Reply {
                        req: req.clone(),
                        reply: Ok(started_result(take, mode, files)),
                    };
                    (Some(reply), Vec::new())
                }
                None => (None, Vec::new()),
            };
            (take.clone(), mode.clone(), started, waiters)
        }
        RecordPhase::Recording {
            take,
            mode,
            waiters,
            ..
        } => (take.clone(), mode.clone(), None, waiters.clone()),
        _ => return Outcome::unchanged(state),
    };
    let stop_reqs: Vec<ReqId> = stopper.into_iter().collect();
    let label = if cancel {
        format!("Cancelling take {}", take.number())
    } else {
        format!("Stopping take {}", take.number())
    };
    let notify = unique(stop_reqs.iter().chain(&waiters));
    let mut next = state.clone();
    next.runtime.record = RecordPhase::Finalizing {
        take: take.clone(),
        mode,
        cancelled: cancel,
        stop_reqs,
        waiters,
        discard: None,
    };
    let persisted = history::persist(state, next);
    let mut effects = persisted.effects;
    effects.push(Effect::StopCapture {
        take: take.clone(),
        cancel,
    });
    effects.extend(started);
    for req in notify {
        effects.push(Effect::Progress {
            req,
            progress: finalize_progress(&take, label.clone()),
        });
    }
    Outcome::new(persisted.state, ordered(effects), None)
}

/// Finalizing + Stop: the caller joins the stop requests.
fn join_stop(state: &State, ctx: &Ctx) -> Outcome {
    let mut next = state.clone();
    if let (RecordPhase::Finalizing { stop_reqs, .. }, Some(req)) =
        (&mut next.runtime.record, &ctx.req)
    {
        if !stop_reqs.contains(req) {
            stop_reqs.push(req.clone());
        }
    }
    Outcome::new(next, Vec::new(), None)
}

// ------------------------------------------------------------- discard

/// A committed discard and its reply.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Discarded {
    pub committed: Committed,
    pub result: CmdResult,
}

/// Discard last (A§5.5): the highest-numbered take with `placed`; its clips
/// leave the timeline in one entry "Discard take N". After a record-over
/// whose replaced clips are all gone from the timeline, those clips are put
/// back where the first removed clip was, clamped to their takes' current
/// lengths. No clip left ⇒ `NotFound` "Take N is already in the bin" (`d`
/// never reaches older takes).
pub(crate) fn discard_last(state: &State, ctx: &Ctx) -> Result<Discarded, CoreError> {
    let project = &state.project;
    let (take_id, take) = project
        .takes
        .iter()
        .filter(|(_, t)| t.placed)
        .max_by_key(|(id, _)| id.number())
        .ok_or_else(|| {
            CoreError::not_found("there is no recorded take to discard")
                .with_hint("record a take first (r, or qrec record toggle)")
        })?;
    let n = take_id.number();
    let clips = &project.timeline.clips;
    let Some(first) = clips.iter().position(|c| &c.take == take_id) else {
        return Err(
            CoreError::not_found(format!("Take {n} is already in the bin")).with_hint(
                "d only discards the newest take; remove other clips with qrec clip remove",
            ),
        );
    };
    let removed: Vec<ClipId> = clips
        .iter()
        .filter(|c| &c.take == take_id)
        .map(|c| c.id.clone())
        .collect();
    let restore: Vec<Clip> = if !take.replaced.is_empty()
        && take.replaced.iter().all(|c| project.clip(&c.id).is_none())
    {
        take.replaced
            .iter()
            .filter_map(|c| restorable(project, c))
            .collect()
    } else {
        Vec::new()
    };
    let mut next = state.clone();
    let kept = timeline::remove(clips, &removed);
    next.project.timeline.clips = timeline::insert(&kept, first, restore.clone());
    let committed = history::commit(
        state,
        ctx,
        format!("Discard take {n}"),
        Command::TakeDiscardLast.to_json(),
        next,
    );
    let restored: Vec<ClipId> = restore.into_iter().map(|c| c.id).collect();
    let mut message = format!("Discarded take {n}; it is in the bin");
    if !restored.is_empty() {
        message.push_str(&format!("; put back {}", join_ids(&restored)));
    }
    let result = CmdResult::new(message)
        .with_removed(&removed)
        .with_created(&restored);
    Ok(Discarded { committed, result })
}

/// A replaced clip as it can go back: its take still exists and is `ok`, and
/// the range is clamped to the take's current `durationFrames` (an fps
/// change may have shortened it). `None` when nothing of it is left.
fn restorable(project: &Project, clip: &Clip) -> Option<Clip> {
    let take = project.take(&clip.take)?;
    if take.status != TakeStatus::Ok {
        return None;
    }
    let out = clip.out.min(take.duration_frames);
    let in_ = clip.in_.min(out);
    (in_ < out).then(|| Clip {
        in_,
        out,
        ..clip.clone()
    })
}

// -------------------------------------------------------- autocut hooks

/// The autocut owner's entry points for the record flow (A§5.7, SPEC §5.3
/// step 6): `apply_ready_autocuts` applies the jobs that finished analyzing
/// while a recording ran, and `start_auto_run` is `autoRunAfterTake` for a
/// freshly placed clip. Their home is `apply::autocut`; this module has the
/// same signatures and reaches the same result through the autocut entry
/// points every base has, so switching to the owner's functions is one
/// `use` line (`pub(crate) use crate::apply::autocut::{apply_ready_autocuts,
/// start_auto_run};`).
pub(crate) mod autocut_hooks {
    use crate::apply::Outcome;
    use crate::ctx::Ctx;
    use crate::error::CoreError;
    use crate::ids::{ClipId, JobId};
    use crate::protocol::command::{AutocutOverrides, ClipScope};
    use crate::protocol::effect::{Effect, LogLevel};
    use crate::protocol::toast::{Toast, ToastLevel};
    use crate::state::{JobKind, State};

    /// Applies every autocut job marked `ready` (its analysis finished while
    /// not Idle), oldest first: the job's last analysis is delivered again
    /// now that the machine is Idle, which completes it. A no-op when no job
    /// is ready.
    pub(crate) fn apply_ready_autocuts(state: &State, ctx: &Ctx) -> Outcome {
        let ready: Vec<JobId> = state
            .runtime
            .jobs
            .values()
            .filter(|job| {
                matches!(&job.kind, JobKind::Autocut { ready: true, pending, .. } if pending.is_empty())
            })
            .map(|job| job.id)
            .collect();
        let mut current = state.clone();
        let mut effects = Vec::new();
        for job in ready {
            let key = match current.runtime.jobs.get(&job).map(|j| &j.kind) {
                Some(JobKind::Autocut { targets, .. }) => targets
                    .iter()
                    .map(|t| &t.key)
                    .find(|key| current.runtime.silence.contains_key(*key))
                    .cloned(),
                _ => None,
            };
            let Some(key) = key else {
                effects.push(Effect::Log {
                    level: LogLevel::Warn,
                    message: format!(
                        "{job}: ready, but none of its analyses is known; left for the next autocut"
                    ),
                });
                continue;
            };
            let silences = current.runtime.silence[&key].clone();
            if let Some(JobKind::Autocut { pending, ready, .. }) =
                current.runtime.jobs.get_mut(&job).map(|j| &mut j.kind)
            {
                pending.insert(key.clone());
                *ready = false;
            }
            let out = crate::on_event::autocut::silence_detected(&current, ctx, job, key, silences);
            current = out.state;
            effects.extend(out.effects);
        }
        Outcome::new(current, effects, None)
    }

    /// `autocut.autoRunAfterTake`: autocut on `clip` as its own job (and its
    /// own history entry), with no requester; problems are told with a
    /// toast. A no-op unless the setting is on.
    pub(crate) fn start_auto_run(state: &State, ctx: &Ctx, clip: &ClipId) -> Outcome {
        if !state.project.autocut.auto_run_after_take {
            return Outcome::unchanged(state);
        }
        if state.project.clip(clip).is_none() {
            return Outcome::new(
                state.clone(),
                vec![Effect::Log {
                    level: LogLevel::Debug,
                    message: format!("autocut after the take: {clip} is not on the timeline"),
                }],
                None,
            );
        }
        let mut auto = ctx.clone();
        auto.req = None;
        let scope = ClipScope::Clips(vec![clip.clone()]);
        match crate::apply::autocut::autocut(
            state,
            &auto,
            scope,
            AutocutOverrides::default(),
            false,
        ) {
            Ok(out) => {
                let mut effects = out.effects;
                if let Some(Err(err)) = &out.reply {
                    effects.push(warning(err));
                }
                Outcome::new(out.state, effects, None)
            }
            Err(err) => Outcome::new(
                state.clone(),
                vec![
                    Effect::Log {
                        level: LogLevel::Warn,
                        message: format!("autocut after the take didn't run: {err}"),
                    },
                    warning(&err),
                ],
                None,
            ),
        }
    }

    fn warning(err: &CoreError) -> Effect {
        Effect::Notify(
            Toast::new(ToastLevel::Warning, "Autocut didn't run").with_body(err.message.clone()),
        )
    }
}

// ------------------------------------------------------- shared helpers

/// The same message `apply`'s recording gate uses (A§5.3).
pub(crate) fn busy_phase(phase: &RecordPhase) -> CoreError {
    let what = match phase {
        RecordPhase::Idle => "not recording".to_owned(),
        RecordPhase::Countdown { .. } => "a recording countdown is running".to_owned(),
        RecordPhase::Starting { take, .. } | RecordPhase::Recording { take, .. } => {
            format!("recording take {}", take.number())
        }
        RecordPhase::Finalizing { take, .. } => format!("finalizing take {}", take.number()),
    };
    CoreError::busy(what)
}

fn not_recording() -> CoreError {
    CoreError::invalid("not recording").with_hint("start a take with qrec record start")
}

/// The reply to a start request once capture runs (`RecordStartedView`).
pub(crate) fn started_result(take: &TakeId, mode: &RecordMode, files: Vec<String>) -> CmdResult {
    CmdResult::new(format!("Recording take {}", take.number()))
        .with_created([take])
        .with_result(&RecordStartedView {
            take: take.clone(),
            mode: mode.clone(),
            files,
        })
}

/// `Recording in 3…`, sent to the start request at every countdown tick.
pub(crate) fn countdown_progress(remaining: u32) -> Progress {
    Progress {
        phase: ProgressPhase::Record,
        done: 0,
        total: 0,
        eta_secs: Some(u64::from(remaining)),
        label: format!("Recording in {remaining}…"),
        take: None,
    }
}

/// `Stopping take 3` / `Finalizing take 3`.
pub(crate) fn finalize_progress(take: &TakeId, label: String) -> Progress {
    Progress {
        phase: ProgressPhase::Finalize,
        done: 0,
        total: 0,
        eta_secs: None,
        label,
        take: Some(take.clone()),
    }
}

/// The requests in order, each once.
pub(crate) fn unique<'a>(reqs: impl IntoIterator<Item = &'a ReqId>) -> Vec<ReqId> {
    let mut out: Vec<ReqId> = Vec::new();
    for req in reqs {
        if !out.contains(req) {
            out.push(req.clone());
        }
    }
    out
}

/// `c7f3, c8a1`.
pub(crate) fn join_ids<T: std::fmt::Display>(ids: &[T]) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Puts one outcome's effects in the A§7.6 commit order: history ops (in
/// order), take directories, one `WriteProject`, one `WriteUi`, then the
/// effects that touch the world (`SetLive`, `StartCapture`, replies, …) in
/// their original order. An outcome that commits and persists several times
/// (finalization) thus writes the files once, before anything else happens.
pub(crate) fn ordered(effects: Vec<Effect>) -> Vec<Effect> {
    let mut history = Vec::new();
    let mut dirs = Vec::new();
    let mut world = Vec::new();
    let (mut project, mut ui) = (false, false);
    for effect in effects {
        match effect {
            Effect::History(_) => history.push(effect),
            Effect::CreateTakeDir { .. } => dirs.push(effect),
            Effect::WriteProject => project = true,
            Effect::WriteUi => ui = true,
            other => world.push(other),
        }
    }
    let mut out = history;
    out.append(&mut dirs);
    if project {
        out.push(Effect::WriteProject);
    }
    if ui {
        out.push(Effect::WriteUi);
    }
    out.append(&mut world);
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ErrorCode;
    use crate::ctx::Facts;
    use crate::history::HistoryEntry;
    use crate::ids::JobId;
    use crate::model::{Fps, Project};
    use crate::protocol::effect::HistoryOp;
    use crate::state::{AutocutParams, AutocutTarget, ClipRef, Job, SilenceKey};
    use crate::testkit::{self, ProjectBuilder};

    const NOW: &str = "2026-10-14T12:30:00Z";

    fn project() -> Project {
        ProjectBuilder::new()
            .screen_layer("L1", "DP-4")
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 300)
            .take("take-0002", 200)
            .clip("c001", "take-0001", 0, 100)
            .clip("c002", "take-0001", 100, 300)
            .clip("c003", "take-0002", 0, 200)
            .build()
    }

    fn ctx() -> Ctx {
        testkit::ctx(NOW, 7)
    }

    fn req(id: &str) -> Option<ReqId> {
        Some(ReqId(id.to_owned()))
    }

    fn ids(xs: &[&str]) -> Vec<ClipId> {
        xs.iter().map(|x| ClipId::from_raw(*x)).collect()
    }

    fn clip(id: &str, take: u32, in_: u64, out: u64) -> Clip {
        Clip {
            id: ClipId::from_raw(id),
            take: TakeId::numbered(take),
            in_,
            out,
        }
    }

    #[test]
    fn over_needs_contiguous_existing_clips() {
        let state = testkit::state(project());
        let mode =
            resolve_mode(&state, RecordModeArg::Over(ids(&["c002", "c001", "c002"]))).unwrap();
        let RecordMode::Over { clips } = mode else {
            panic!("over")
        };
        assert_eq!(
            clips,
            vec![
                ClipRef::of(&state.project.timeline.clips[0]),
                ClipRef::of(&state.project.timeline.clips[1])
            ],
            "timeline order, each once"
        );
        let err = resolve_mode(&state, RecordModeArg::Over(ids(&["c001", "c003"]))).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        let err = resolve_mode(&state, RecordModeArg::Over(ids(&["c009"]))).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let err = resolve_mode(&state, RecordModeArg::Over(vec![])).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        let err = resolve_mode(&state, RecordModeArg::OverSelection).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unavailable);
        let mut selected = state.clone();
        selected.ui.selection = ids(&["c003"]);
        assert!(matches!(
            resolve_mode(&selected, RecordModeArg::OverSelection).unwrap(),
            RecordMode::Over { clips } if clips.len() == 1 && clips[0].id.as_str() == "c003"
        ));
    }

    #[test]
    fn the_countdown_comes_from_the_command_or_the_project() {
        let mut state = testkit::state(project());
        state.project.recording.countdown_secs = 3;
        state.runtime.playing = true;
        let out = record_start(&state, &ctx(), RecordModeArg::Append, None, false).unwrap();
        assert!(matches!(
            out.state.runtime.record,
            RecordPhase::Countdown { remaining: 3, .. }
        ));
        assert!(!out.state.runtime.playing, "a countdown stops playback");
        let out = record_start(&state, &ctx(), RecordModeArg::Append, Some(0), false).unwrap();
        assert!(matches!(
            out.state.runtime.record,
            RecordPhase::Starting { .. }
        ));
        assert!(!out.state.runtime.playing, "a take stops playback");
        // A hand-edited countdown above the limit counts as the limit.
        state.project.recording.countdown_secs = 30;
        let out = record_toggle(&state, &ctx(), false).unwrap();
        assert!(matches!(
            out.state.runtime.record,
            RecordPhase::Countdown { remaining: 10, .. }
        ));
        let err = record_start(&state, &ctx(), RecordModeArg::Append, Some(11), false).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
    }

    #[test]
    fn effects_follow_the_commit_order() {
        let entry = |seq| HistoryEntry {
            seq,
            ts: testkit::ctx(NOW, 1).now,
            label: "x".into(),
            cmd: serde_json::Value::Null,
            forward: json_patch::Patch(vec![]),
            inverse: json_patch::Patch(vec![]),
            after_hash: String::new(),
        };
        let effects = vec![
            Effect::WriteProject,
            Effect::Notify(crate::protocol::toast::Toast::new(
                crate::protocol::toast::ToastLevel::Info,
                "hi",
            )),
            Effect::History(HistoryOp::Append(entry(1))),
            Effect::WriteUi,
            Effect::CreateTakeDir {
                path: "takes/take-0003".into(),
            },
            Effect::SetLive(None),
            Effect::WriteProject,
            Effect::History(HistoryOp::Append(entry(2))),
            Effect::StartCapture {
                plan: plan_capture(
                    &project(),
                    &TakeId::numbered(3),
                    &testkit::capabilities(),
                    &Facts::default(),
                )
                .unwrap(),
            },
        ];
        let out = ordered(effects);
        let names: Vec<&str> = out
            .iter()
            .map(|e| match e {
                Effect::History(HistoryOp::Append(e)) if e.seq == 1 => "append1",
                Effect::History(_) => "append2",
                Effect::CreateTakeDir { .. } => "mkdir",
                Effect::WriteProject => "project",
                Effect::WriteUi => "ui",
                Effect::Notify(_) => "notify",
                Effect::SetLive(_) => "live",
                Effect::StartCapture { .. } => "capture",
                _ => "other",
            })
            .collect();
        assert_eq!(
            names,
            [
                "append1", "append2", "mkdir", "project", "ui", "notify", "live", "capture"
            ]
        );
    }

    #[test]
    fn discard_targets_the_newest_placed_take() {
        let mut p = project();
        p.takes.get_mut(&TakeId::numbered(1)).unwrap().placed = true;
        p.takes.get_mut(&TakeId::numbered(2)).unwrap().placed = true;
        let state = testkit::state(p);
        let d = discard_last(&state, &ctx()).unwrap();
        let ids: Vec<&str> = d
            .committed
            .state
            .project
            .timeline
            .clips
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["c001", "c002"]);
        assert_eq!(d.result.removed, ["c003"]);
        assert_eq!(d.committed.seq, Some(1));
        let entry = &d.committed.state.history.entries[0];
        assert_eq!(entry.label, "Discard take 2");
        assert_eq!(entry.cmd, json!({"cmd": "takeDiscardLast"}));

        // Again: take 2 is the newest placed take and it is in the bin now.
        let err = discard_last(&d.committed.state, &ctx()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.message, "Take 2 is already in the bin");

        let none = testkit::state(project());
        let err = discard_last(&none, &ctx()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn discard_orders_takes_by_number() {
        // take-10000 sorts before take-9999 as a string; numbers decide.
        let p = ProjectBuilder::new()
            .screen_layer("L1", "DP-4")
            .take("take-9999", 50)
            .take("take-10000", 50)
            .with_take("take-9999", |t| t.placed = true)
            .with_take("take-10000", |t| t.placed = true)
            .clip("c001", "take-9999", 0, 50)
            .clip("c002", "take-10000", 0, 50)
            .build();
        let d = discard_last(&testkit::state(p), &ctx()).unwrap();
        assert_eq!(d.result.removed, ["c002"]);
    }

    /// take-0003 recorded over c001 + c002 (both of take-0001).
    fn after_record_over() -> (State, Vec<Clip>) {
        let replaced = vec![clip("c001", 1, 0, 100), clip("c002", 1, 100, 300)];
        let p = ProjectBuilder::new()
            .screen_layer("L1", "DP-4")
            .take("take-0001", 300)
            .take("take-0002", 200)
            .take("take-0003", 150)
            .with_take("take-0003", |t| {
                t.placed = true;
                t.replaced = replaced.clone();
            })
            .clip("c003", "take-0002", 0, 200)
            .clip("c010", "take-0003", 0, 150)
            .build();
        (testkit::state(p), replaced)
    }

    #[test]
    fn discard_puts_replaced_clips_back() {
        let (state, replaced) = after_record_over();
        let d = discard_last(&state, &ctx()).unwrap();
        let clips = &d.committed.state.project.timeline.clips;
        assert_eq!(clips[0].id.as_str(), "c003");
        assert_eq!(&clips[1..], &replaced[..], "back where take 3's clip was");
        assert_eq!(d.result.created, ["c001", "c002"]);
        assert_eq!(d.result.removed, ["c010"]);
        assert!(d.result.message.contains("put back c001, c002"));

        // One of the replaced clips is on the timeline again: nothing comes back.
        let mut moved = state.clone();
        moved.project.timeline.clips.insert(0, replaced[0].clone());
        let d = discard_last(&moved, &ctx()).unwrap();
        let ids: Vec<&str> = d
            .committed
            .state
            .project
            .timeline
            .clips
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["c001", "c003"]);
        assert!(d.result.created.is_empty());
    }

    #[test]
    fn discard_clamps_replaced_clips_to_their_takes() {
        // take-0001 got shorter (an fps change re-derived it): c002 is cut to
        // the new length; a clip past the end, or of a take gone from the
        // pool, doesn't come back.
        let (mut state, _) = after_record_over();
        state
            .project
            .takes
            .get_mut(&TakeId::numbered(1))
            .unwrap()
            .duration_frames = 250;
        state
            .project
            .takes
            .get_mut(&TakeId::numbered(3))
            .unwrap()
            .replaced
            .extend([clip("c004", 1, 260, 280), clip("c005", 8, 0, 10)]);
        let d = discard_last(&state, &ctx()).unwrap();
        let clips = &d.committed.state.project.timeline.clips;
        assert_eq!(
            clips[1..],
            [clip("c001", 1, 0, 100), clip("c002", 1, 100, 250)]
        );
        assert_eq!(d.result.created, ["c001", "c002"]);
    }

    #[test]
    fn discard_while_finalizing_is_queued_once() {
        let mut state = testkit::state(project());
        state.runtime.record = RecordPhase::Finalizing {
            take: TakeId::numbered(3),
            mode: RecordMode::Append,
            cancelled: false,
            stop_reqs: vec![],
            waiters: vec![],
            discard: None,
        };
        let mut c = ctx();
        c.req = req("d1");
        let out = take_discard_last(&state, &c).unwrap();
        assert_eq!(out.reply, None, "answered after placement");
        assert!(out.effects.is_empty());
        assert!(matches!(
            &out.state.runtime.record,
            RecordPhase::Finalizing { discard: Some(DiscardRequest { req: queued }), .. } if *queued == req("d1")
        ));
        let err = take_discard_last(&out.state, &c).unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
    }

    #[test]
    fn the_engine_gets_the_pipeline_before_the_record_bins() {
        // A headless host has no pipeline until the take begins.
        let mut state = testkit::state(project());
        state.runtime.host = crate::state::HostKind::Headless;
        let out = record_start(&state, &ctx(), RecordModeArg::Append, Some(0), false).unwrap();
        let live = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::SetLive(Some(_))))
            .unwrap();
        let capture = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::StartCapture { .. }))
            .unwrap();
        assert!(live < capture);
    }

    fn autocut_job(ready: bool, key: &str) -> Job {
        Job {
            id: JobId(3),
            kind: JobKind::Autocut {
                cmd: json!({"cmd": "autocut"}),
                targets: vec![AutocutTarget {
                    clip: ClipRef::of(&clip("c001", 1, 0, 100)),
                    track: "A1".parse().unwrap(),
                    key: SilenceKey(key.to_owned()),
                }],
                params: AutocutParams {
                    threshold_db: -35.0,
                    min_silence_ms: 400,
                    padding_ms: 120,
                    min_keep_ms: 200,
                },
                dry_run: false,
                pending: Default::default(),
                ready,
            },
            req: None,
            started: ctx().now,
        }
    }

    #[test]
    fn autocut_hooks_do_nothing_without_work() {
        let state = testkit::state(project());
        let out = autocut_hooks::apply_ready_autocuts(&state, &ctx());
        assert_eq!(out, Outcome::unchanged(&state));
        // A job still analyzing is not ready; one whose analysis is unknown
        // is left for the next autocut.
        let mut busy = state.clone();
        busy.runtime.jobs.insert(JobId(3), autocut_job(false, "ab"));
        assert_eq!(
            autocut_hooks::apply_ready_autocuts(&busy, &ctx()),
            Outcome::unchanged(&busy)
        );
        let mut unknown = state.clone();
        unknown
            .runtime
            .jobs
            .insert(JobId(3), autocut_job(true, "ab"));
        let out = autocut_hooks::apply_ready_autocuts(&unknown, &ctx());
        assert_eq!(out.state, unknown);
        assert!(matches!(
            out.effects.as_slice(),
            [Effect::Log {
                level: LogLevel::Warn,
                ..
            }]
        ));
        // autoRunAfterTake is off by default.
        let id = ClipId::from_raw("c001");
        assert_eq!(
            autocut_hooks::start_auto_run(&state, &ctx(), &id),
            Outcome::unchanged(&state)
        );
        let mut on = state.clone();
        on.project.autocut.auto_run_after_take = true;
        let out = autocut_hooks::start_auto_run(&on, &ctx(), &ClipId::from_raw("c404"));
        assert_eq!(out.state, on);
        assert!(matches!(
            out.effects.as_slice(),
            [Effect::Log {
                level: LogLevel::Debug,
                ..
            }]
        ));
    }

    #[test]
    fn a_take_records_the_scene_at_the_canvas_rate() {
        let state = testkit::state(
            ProjectBuilder::new()
                .canvas(1280, 720, Fps::new(60, 1))
                .screen_layer("L1", "DP-4")
                .build(),
        );
        let (next, effects) = begin_take(&state, &ctx(), RecordMode::Append, None, false).unwrap();
        let Some(Effect::StartCapture { plan }) = effects.last() else {
            panic!("{effects:?}")
        };
        assert_eq!(plan.fps, Fps::new(60, 1));
        assert_eq!(plan.video[0].key_int_max, 60);
        assert_eq!(next.project.takes[&TakeId::numbered(1)].streams.len(), 1);
    }
}
