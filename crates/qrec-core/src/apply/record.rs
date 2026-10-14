//! Recording commands (A§5.3 state machine, A§5.5 discard, A§5.6 purge).
//! Owner: record.
//!
//! `apply` has already rejected non-R commands while not `Idle`; these
//! handlers implement the command rows of the A§5.3 table. The helpers the
//! event side (`on_event::record`) shares live here too: beginning a take,
//! stopping the capture, discarding the last take, reply results and the
//! A§7.6 effect order.
//!
//! Begin take: `ctx::next_take_number`, insert the take (`status:
//! recording`, scene snapshot, planned streams) with `persist`, then
//! `CreateTakeDir`, `WriteProject`, `SetLive` (if needed) and
//! `StartCapture`.
//!
//! The live pipeline follows one rule ([`wanted_live`]): it runs while the
//! GUI shows Live or while `record != Idle` (countdown included), and every
//! transition that changes what is wanted emits `SetLive`.

use std::collections::BTreeSet;

use serde_json::json;

use crate::apply::Outcome;
use crate::capture_plan::{self, plan_capture, planned_streams};
use crate::ctx::{Ctx, FileFact, next_take_number};
use crate::error::CoreError;
use crate::history::purge::{PurgeRewrite, purge_rewrite};
use crate::history::{self, Committed};
use crate::ids::{ClipId, ReqId, TakeId, TimerId};
use crate::model::{Clip, EncoderPref, Take, TakeStatus};
use crate::on_event::record::run_ready_autocut;
use crate::plans::{CapturePlan, LiveScene, VideoEncoder};
use crate::protocol::command::{Command, RecordModeArg};
use crate::protocol::effect::{Effect, LogLevel};
use crate::protocol::progress::{Progress, ProgressPhase};
use crate::protocol::reply::{CmdResult, RecordStartedView};
use crate::state::{
    CanvasMode, ClipRef, DiscardRequest, HostKind, Job, JobKind, RecordMode, RecordPhase, State,
};

/// `recording.countdownSecs` and `--countdown` are 0–10 (SPEC §5.5).
pub const MAX_COUNTDOWN_SECS: u32 = 10;
/// One countdown tick.
pub const COUNTDOWN_TICK_MS: u64 = 1_000;
/// Takes shorter than this become `tooShort` (SPEC §5.3).
pub const MIN_TAKE_FRAMES: u64 = 10;
/// Why every take and start fails once the engine missed a teardown (A§5.3).
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
    let secs = countdown.unwrap_or(state.project.recording.countdown_secs);
    if secs > MAX_COUNTDOWN_SECS {
        return Err(CoreError::usage(format!(
            "the countdown must be 0–{MAX_COUNTDOWN_SECS} seconds, not {secs}"
        )));
    }
    check_can_start(state)?;
    let mode = resolve_mode(state, mode)?;
    start(state, ctx, mode, secs, wait)
}

/// `qrec record stop`: finalize (the caller waits for `TakeFinalizedView`).
pub fn record_stop(state: &State, ctx: &Ctx) -> Result<Outcome, CoreError> {
    match &state.runtime.record {
        RecordPhase::Idle => Err(not_recording()),
        RecordPhase::Countdown {
            timer, start_req, ..
        } => Ok(cancel_countdown(state, ctx, *timer, start_req.as_ref())),
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
        RecordPhase::Countdown {
            timer, start_req, ..
        } => Ok(cancel_countdown(state, ctx, *timer, start_req.as_ref())),
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
            let secs = state
                .project
                .recording
                .countdown_secs
                .min(MAX_COUNTDOWN_SECS);
            start(state, ctx, RecordMode::Append, secs, wait)
        }
        RecordPhase::Countdown {
            timer, start_req, ..
        } => Ok(cancel_countdown(state, ctx, *timer, start_req.as_ref())),
        RecordPhase::Starting { .. } | RecordPhase::Recording { .. } => {
            Ok(stop_capture(state, ctx.req.clone(), false))
        }
        phase @ RecordPhase::Finalizing { .. } => Err(busy_phase(phase)),
    }
}

/// `qrec take discard-last` (A§5.5); while finalizing it is remembered and
/// applied after placement.
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

/// `qrec take purge <take>… --yes` (A§5.6, A§7.5). Not undoable.
///
/// Only takes no clip references and that aren't recording; `Busy` while a
/// job reads one of their files. Unconfirmed, it fails with
/// `confirmation_required` and `details {takes, files:[{path,bytes}],
/// historyEntriesLost, oldestKept}`. Confirmed: `History(Rewrite)`,
/// `WriteProject`, then `DeleteTakeDirs`; the reply comes with
/// `TakeDirsDeleted` (`on_event::record::take_dirs_deleted`).
pub fn take_purge(state: &State, ctx: &Ctx, takes: Vec<TakeId>) -> Result<Outcome, CoreError> {
    let mut seen = BTreeSet::new();
    let takes: Vec<TakeId> = takes
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect();
    if takes.is_empty() {
        return Err(CoreError::usage("name at least one take to purge")
            .with_hint("qrec take list --unused shows the takes in the bin"));
    }
    for take in &takes {
        check_purgeable(state, take)?;
    }
    let rewrite = purge_rewrite(state, &takes)?;
    if !ctx.confirmed {
        return Err(purge_confirmation(ctx, &takes, &rewrite));
    }
    Ok(purge(state, ctx, takes, rewrite))
}

// ------------------------------------------------------------- starting

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
/// replace (timeline order).
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

fn over_refs(state: &State, ids: &[ClipId]) -> Result<RecordMode, CoreError> {
    if ids.is_empty() {
        return Err(CoreError::usage("record over needs at least one clip"));
    }
    let mut indices = Vec::with_capacity(ids.len());
    for id in ids {
        let index = state.project.clip_index(id).ok_or_else(|| {
            CoreError::not_found(format!("no clip {id}"))
                .with_hint("qrec clip list shows the clips on the timeline")
        })?;
        if !indices.contains(&index) {
            indices.push(index);
        }
    }
    indices.sort_unstable();
    if indices.windows(2).any(|w| w[1] != w[0] + 1) {
        return Err(CoreError::invalid("record over needs contiguous clips")
            .with_hint("pick clips that sit next to each other on the timeline"));
    }
    let clips = &state.project.timeline.clips;
    Ok(RecordMode::Over {
        clips: indices.iter().map(|&i| ClipRef::of(&clips[i])).collect(),
    })
}

/// Countdown when `secs > 0`, else the take begins right away.
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
    let timer = next.runtime.alloc_timer_id();
    next.runtime.record = RecordPhase::Countdown {
        remaining: secs,
        mode,
        timer,
        start_req: ctx.req.clone(),
        wait,
    };
    let mut effects: Vec<Effect> = live_change(state, &next).into_iter().collect();
    effects.push(Effect::StartTimer {
        timer,
        after_ms: COUNTDOWN_TICK_MS,
    });
    effects.extend(ctx.req.iter().map(|req| Effect::Progress {
        req: req.clone(),
        progress: countdown_progress(secs),
    }));
    Ok(Outcome::new(next, effects, None))
}

/// Begin take (A§5.3): allocate the take number, insert the take
/// (`recording`, scene snapshot, planned streams) with `persist`, then
/// `CreateTakeDir`, `WriteProject`, `SetLive` when the wanted pipeline
/// changes (not after a countdown, which already wanted it), and
/// `StartCapture`. The phase becomes `Starting`.
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
    let caps = &state.runtime.capabilities;
    let plan = plan_capture(&state.project, &take, caps, &ctx.facts)?;
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
    effects.extend(live_change(state, &persisted.state));
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

/// The live pipeline `state` wants (A§8.11): the project scene while the
/// GUI shows the Live canvas (with the preview branch), or while a take is
/// counted down, started, recorded or finalized in any host (record bins
/// attach to it; no preview unless the GUI shows Live). `None` otherwise.
/// Counting down already wants it, so sources start during the countdown.
///
/// The same rule as the timeline owner's `apply::ui::live_scene`, so the
/// UI commands and the recording agree on what the engine runs.
pub(crate) fn wanted_live(state: &State) -> Option<LiveScene> {
    let preview = state.runtime.host == HostKind::Gui && state.ui.canvas_mode == CanvasMode::Live;
    let recording = !state.runtime.record.is_idle();
    (preview || recording).then(|| LiveScene {
        canvas: state.project.canvas.clone(),
        scene: state.project.scene.clone(),
        preview,
    })
}

/// `SetLive` with what `next` wants, when that differs from what `prev`
/// wanted ("SetLive (if needed)", A§5.3).
pub(crate) fn live_change(prev: &State, next: &State) -> Option<Effect> {
    let wanted = wanted_live(next);
    (wanted_live(prev) != wanted).then_some(Effect::SetLive(wanted))
}

// ------------------------------------------------------------- stopping

/// Countdown + Stop/Toggle/Cancel: `CancelTimer` ⇒ Idle; the start request
/// gets `Invalid` "countdown cancelled", the caller gets Ok. Back in Idle,
/// autocut jobs that finished meanwhile are applied.
fn cancel_countdown(
    state: &State,
    ctx: &Ctx,
    timer: TimerId,
    start_req: Option<&ReqId>,
) -> Outcome {
    let mut next = state.clone();
    next.runtime.record = RecordPhase::Idle;
    let mut effects = vec![Effect::CancelTimer { timer }];
    effects.extend(live_change(state, &next));
    if let Some(req) = start_req {
        effects.push(Effect::Reply {
            req: req.clone(),
            reply: Err(CoreError::invalid("countdown cancelled")),
        });
    }
    let (next, jobs) = run_ready_autocut(next, ctx);
    effects.extend(jobs);
    Outcome::new(
        next,
        ordered(effects),
        Some(Ok(CmdResult::new("Countdown cancelled"))),
    )
}

/// Starting/Recording + Stop (`cancel: false`) or Cancel: `StopCapture` ⇒
/// Finalizing. `stopper` waits for the finalized reply. Leaving Starting
/// answers the start request now (its started reply) unless it waits for
/// the whole take, in which case it joins the waiters.
pub(crate) fn stop_capture(state: &State, stopper: Option<ReqId>, cancel: bool) -> Outcome {
    let mut effects = Vec::new();
    let (take, mode, waiters) = match &state.runtime.record {
        RecordPhase::Starting {
            take,
            mode,
            start_req,
            wait,
        } => {
            let mut waiters = Vec::new();
            if let Some(req) = start_req {
                if *wait {
                    waiters.push(req.clone());
                } else {
                    let files = state
                        .project
                        .take(take)
                        .map(|t| t.streams.iter().map(|s| s.file.clone()).collect())
                        .unwrap_or_default();
                    effects.push(Effect::Reply {
                        req: req.clone(),
                        reply: Ok(started_result(take, mode, files)),
                    });
                }
            }
            (take.clone(), mode.clone(), waiters)
        }
        RecordPhase::Recording {
            take,
            mode,
            waiters,
            ..
        } => (take.clone(), mode.clone(), waiters.clone()),
        _ => return Outcome::unchanged(state),
    };
    let stop_reqs: Vec<ReqId> = stopper.into_iter().collect();
    let label = if cancel {
        format!("Cancelling take {}", take.number())
    } else {
        format!("Stopping take {}", take.number())
    };
    let mut all = vec![Effect::StopCapture {
        take: take.clone(),
        cancel,
    }];
    all.append(&mut effects);
    for req in unique(stop_reqs.iter().chain(&waiters)) {
        all.push(Effect::Progress {
            req,
            progress: finalize_progress(&take, label.clone()),
        });
    }
    let mut next = state.clone();
    next.runtime.record = RecordPhase::Finalizing {
        take,
        mode,
        cancelled: cancel,
        stop_reqs,
        waiters,
        discard: None,
    };
    Outcome::new(next, all, None)
}

/// Finalizing + Stop: the caller joins the stop waiters.
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
/// back where the first removed clip was. No clip left ⇒ `NotFound` "Take N
/// is already in the bin" (`d` never reaches older takes).
pub(crate) fn discard_last(state: &State, ctx: &Ctx) -> Result<Discarded, CoreError> {
    let project = &state.project;
    let (take_id, take) = project
        .takes
        .iter()
        .filter(|(_, t)| t.placed)
        .max_by_key(|(id, _)| id.number())
        .ok_or_else(|| CoreError::not_found("there is no recorded take to discard"))?;
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
            .filter(|c| restorable(state, c))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let mut next = state.clone();
    let timeline = &mut next.project.timeline.clips;
    timeline.retain(|c| &c.take != take_id);
    timeline.splice(first..first, restore.iter().cloned());
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

/// A replaced clip can go back when its take still exists, is `ok` and
/// covers the range.
fn restorable(state: &State, clip: &Clip) -> bool {
    state.project.take(&clip.take).is_some_and(|t| {
        t.status == TakeStatus::Ok && clip.in_ < clip.out && clip.out <= t.duration_frames
    })
}

// --------------------------------------------------------------- purge

fn check_purgeable(state: &State, take: &TakeId) -> Result<(), CoreError> {
    let Some(t) = state.project.take(take) else {
        return Err(CoreError::not_found(format!("no take {take}"))
            .with_hint("qrec take list shows the takes"));
    };
    let used: Vec<ClipId> = state
        .project
        .timeline
        .clips
        .iter()
        .filter(|c| &c.take == take)
        .map(|c| c.id.clone())
        .collect();
    if !used.is_empty() {
        return Err(CoreError::invalid(format!(
            "{take} is on the timeline (clips {})",
            join_ids(&used)
        ))
        .with_hint("only takes in the bin can be purged: remove their clips first"));
    }
    if t.status == TakeStatus::Recording || state.runtime.record.take() == Some(take) {
        return Err(CoreError::busy(format!("{take} is still recording")));
    }
    let prefix = format!("takes/{take}/");
    let reads = |files: &BTreeSet<String>| {
        files
            .iter()
            .any(|f| f.starts_with(&prefix) || t.streams.iter().any(|s| &s.file == f))
    };
    let busy = state.runtime.jobs.values().find(|job| match &job.kind {
        JobKind::Export { files, .. } | JobKind::FrameSave { files, .. } => reads(files),
        JobKind::Autocut { targets, .. } => targets.iter().any(|x| &x.clip.take == take),
        JobKind::Purge { takes } => takes.contains(take),
    });
    if let Some(job) = busy {
        return Err(CoreError::busy(format!(
            "{take} is in use by {} ({})",
            job.id,
            job_kind_name(&job.kind)
        ))
        .with_hint("wait for the job to finish, then purge again"));
    }
    Ok(())
}

fn job_kind_name(kind: &JobKind) -> &'static str {
    match kind {
        JobKind::Autocut { .. } => "autocut",
        JobKind::Export { .. } => "export",
        JobKind::FrameSave { .. } => "frame save",
        JobKind::Purge { .. } => "purge",
    }
}

/// `confirmation required: purge take-0003 (2 files, 412 MB)` with the
/// files, the history entries lost and the oldest surviving label.
fn purge_confirmation(ctx: &Ctx, takes: &[TakeId], rewrite: &PurgeRewrite) -> CoreError {
    let files: Vec<&FileFact> = takes
        .iter()
        .filter_map(|t| ctx.facts.take_files.get(t))
        .flatten()
        .collect();
    let bytes: u64 = files.iter().map(|f| f.bytes).sum();
    let count = match files.len() {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    };
    let mut what = format!(
        "purge {} ({count}, {})",
        join_ids(takes),
        human_bytes(bytes)
    );
    match rewrite.entries_lost {
        0 => {}
        1 => what.push_str(" and drop 1 history entry"),
        n => what.push_str(&format!(" and drop {n} history entries")),
    }
    CoreError::confirmation_required(what).with_details(json!({
        "takes": takes,
        "files": files
            .iter()
            .map(|f| json!({"path": f.path, "bytes": f.bytes}))
            .collect::<Vec<_>>(),
        "historyEntriesLost": rewrite.entries_lost,
        "oldestKept": rewrite.oldest_kept,
    }))
}

/// The confirmed purge: the takes leave the pool (and every `replaced` list),
/// the history is rewritten, a purge job deletes the directories after the
/// commit is durable.
fn purge(state: &State, ctx: &Ctx, takes: Vec<TakeId>, rewrite: PurgeRewrite) -> Outcome {
    let mut next = state.clone();
    next.history = rewrite.history;
    for take in &takes {
        next.project.takes.remove(take);
    }
    for take in next.project.takes.values_mut() {
        take.replaced.retain(|c| !takes.contains(&c.take));
    }
    let job = next.runtime.alloc_job_id();
    next.runtime.jobs.insert(
        job,
        Job {
            id: job,
            kind: JobKind::Purge {
                takes: takes.clone(),
            },
            req: ctx.req.clone(),
            started: ctx.now,
        },
    );
    let persisted = history::persist(state, next);
    let mut effects = vec![Effect::History(rewrite.op)];
    effects.extend(persisted.effects);
    effects.push(Effect::DeleteTakeDirs { job, takes });
    Outcome::new(persisted.state, ordered(effects), None)
}

/// `412 MB`, `1.2 GB`, `900 B` (decimal units).
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = "B";
    for u in UNITS {
        value /= 1000.0;
        unit = u;
        if value < 999.95 {
            break;
        }
    }
    if value < 9.95 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
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
/// effects that touch the world, in their original order.
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
    use super::*;
    use crate::ctx::Facts;
    use crate::history::HistoryEntry;
    use crate::model::Project;
    use crate::protocol::effect::HistoryOp;
    use crate::state::ClipRef;
    use crate::testkit::{self, ProjectBuilder};
    use crate::{ErrorCode, JobId};

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

    #[test]
    fn over_needs_contiguous_existing_clips() {
        let state = testkit::state(project());
        let ids = |xs: &[&str]| xs.iter().map(|x| ClipId::from_raw(*x)).collect::<Vec<_>>();
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
                Effect::StartCapture { .. } => "capture",
                _ => "other",
            })
            .collect();
        assert_eq!(
            names,
            [
                "append1", "append2", "mkdir", "project", "ui", "notify", "capture"
            ]
        );
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1000), "1.0 kB");
        assert_eq!(human_bytes(412_000_000), "412 MB");
        assert_eq!(human_bytes(1_234_000_000), "1.2 GB");
        assert_eq!(human_bytes(999_960_000), "1.0 GB");
        assert_eq!(human_bytes(9_960_000), "10 MB");
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
    fn discard_puts_replaced_clips_back() {
        // take-0003 recorded over c001 + c002 (both of take-0001).
        let replaced = vec![
            Clip {
                id: ClipId::from_raw("c001"),
                take: TakeId::numbered(1),
                in_: 0,
                out: 100,
            },
            Clip {
                id: ClipId::from_raw("c002"),
                take: TakeId::numbered(1),
                in_: 100,
                out: 300,
            },
        ];
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
        let state = testkit::state(p);
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
    fn purge_checks_its_takes() {
        let mut state = testkit::state(project());
        let c = ctx();
        let err = take_purge(&state, &c, vec![]).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        let err = take_purge(&state, &c, vec![TakeId::numbered(9)]).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let err = take_purge(&state, &c, vec![TakeId::numbered(2)]).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(err.message, "take-0002 is on the timeline (clips c003)");

        // A bin take that a job reads, or that is still recording.
        state.project.timeline.clips.pop();
        let job = |kind| Job {
            id: JobId(5),
            kind,
            req: None,
            started: c.now,
        };
        let mut exporting = state.clone();
        exporting.runtime.jobs.insert(
            JobId(5),
            job(JobKind::FrameSave {
                out: "/tmp/x.png".into(),
                files: ["takes/take-0002/L1-screen-DP-4.mkv".to_owned()].into(),
            }),
        );
        let err = check_purgeable(&exporting, &TakeId::numbered(2)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        assert_eq!(err.message, "take-0002 is in use by job-5 (frame save)");
        let mut purging = state.clone();
        purging.runtime.jobs.insert(
            JobId(5),
            job(JobKind::Purge {
                takes: vec![TakeId::numbered(2)],
            }),
        );
        assert_eq!(
            check_purgeable(&purging, &TakeId::numbered(2))
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
        let mut recording = state.clone();
        recording
            .project
            .takes
            .get_mut(&TakeId::numbered(2))
            .unwrap()
            .status = TakeStatus::Recording;
        assert_eq!(
            check_purgeable(&recording, &TakeId::numbered(2))
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
        assert_eq!(check_purgeable(&state, &TakeId::numbered(2)), Ok(()));
    }

    fn rewrite_for(state: &State, lost: usize) -> PurgeRewrite {
        let mut history = state.history.clone();
        history.cursor = 3;
        history.next_seq = 9;
        PurgeRewrite {
            op: HistoryOp::Rewrite {
                entries: vec![],
                base: Box::new(state.project.clone()),
                base_seq: 3,
                cursor: 3,
                next_seq: 9,
            },
            history,
            entries_lost: lost,
            oldest_kept: Some("Split clip c001 at 00:00:01:00".into()),
        }
    }

    #[test]
    fn purge_confirmation_details() {
        let mut state = testkit::state(project());
        state.project.timeline.clips.pop();
        let mut c = ctx();
        c.facts.take_files.insert(
            TakeId::numbered(2),
            vec![
                FileFact {
                    path: "takes/take-0002/L1-screen-DP-4.mkv".into(),
                    bytes: 400_000_000,
                    mtime_ns: 1,
                },
                FileFact {
                    path: "takes/take-0002/A1-mic.mka".into(),
                    bytes: 12_000_000,
                    mtime_ns: 2,
                },
            ],
        );
        let err = purge_confirmation(&c, &[TakeId::numbered(2)], &rewrite_for(&state, 3));
        assert_eq!(err.code, ErrorCode::ConfirmationRequired);
        assert_eq!(
            err.message,
            "confirmation required: purge take-0002 (2 files, 412 MB) and drop 3 history entries"
        );
        assert_eq!(
            err.details.unwrap(),
            json!({
                "takes": ["take-0002"],
                "files": [
                    {"path": "takes/take-0002/L1-screen-DP-4.mkv", "bytes": 400_000_000},
                    {"path": "takes/take-0002/A1-mic.mka", "bytes": 12_000_000}
                ],
                "historyEntriesLost": 3,
                "oldestKept": "Split clip c001 at 00:00:01:00"
            })
        );
        let err = purge_confirmation(&c, &[TakeId::numbered(2)], &rewrite_for(&state, 0));
        assert_eq!(
            err.message,
            "confirmation required: purge take-0002 (2 files, 412 MB)"
        );
    }

    #[test]
    fn confirmed_purge_rewrites_writes_then_deletes() {
        let mut p = project();
        p.timeline.clips.pop();
        // take-0003 replaced a clip of take-0002 earlier.
        p.takes.insert(
            TakeId::numbered(3),
            Take {
                replaced: vec![Clip {
                    id: ClipId::from_raw("c003"),
                    take: TakeId::numbered(2),
                    in_: 0,
                    out: 200,
                }],
                ..p.takes[&TakeId::numbered(1)].clone()
            },
        );
        let state = testkit::state(p);
        let mut c = ctx();
        c.confirmed = true;
        c.req = req("p1");
        let rewrite = rewrite_for(&state, 1);
        let out = purge(&state, &c, vec![TakeId::numbered(2)], rewrite.clone());
        assert_eq!(out.reply, None, "replied when the directories are gone");
        assert!(!out.state.project.takes.contains_key(&TakeId::numbered(2)));
        assert!(
            out.state.project.takes[&TakeId::numbered(3)]
                .replaced
                .is_empty()
        );
        assert_eq!(out.state.history, rewrite.history);
        let job = JobId(1);
        assert_eq!(
            out.state.runtime.jobs[&job].kind,
            JobKind::Purge {
                takes: vec![TakeId::numbered(2)]
            }
        );
        assert_eq!(out.state.runtime.jobs[&job].req, req("p1"));
        assert_eq!(
            out.effects,
            vec![
                Effect::History(rewrite.op),
                Effect::WriteProject,
                Effect::DeleteTakeDirs {
                    job,
                    takes: vec![TakeId::numbered(2)]
                }
            ]
        );
    }
}
