//! Recording events (A§5.3). Owner: record. Stub: signatures only.
//!
//! Finalization order (A§5.3): fill media info and mark unreadable streams
//! `missing` with `persist` (facts first), then place the take with
//! `commit` ("Record take N", `cmd: {"event":"takeFinalized","take":…}`),
//! then the pending discard, ready autocut jobs and auto-run autocut.
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::ids::{TakeId, TimerId};
use crate::protocol::effect::ProbePurpose;
use crate::protocol::event::{ProbeResult, RecordedStream};
use crate::state::State;

/// Starting ⇒ Recording; reply `RecordStartedView` to the start request (unless `wait`).
pub fn capture_started(
    state: &State,
    ctx: &Ctx,
    take: TakeId,
    t0_ns: u64,
    files: Vec<String>,
) -> Outcome {
    todo!("record: CaptureStarted")
}

/// Start/health-window failure: take `broken` ⇒ Idle; `capture_failed` reply.
pub fn capture_failed(
    state: &State,
    ctx: &Ctx,
    take: Option<TakeId>,
    source: Option<String>,
    message: String,
) -> Outcome {
    todo!("record: CaptureFailed")
}

/// Mark the layer/track `missing` (persist); the last stream lost acts as Stop.
pub fn stream_lost(
    state: &State,
    ctx: &Ctx,
    take: TakeId,
    stream: String,
    message: String,
) -> Outcome {
    todo!("record: StreamLost")
}

/// Update `elapsed_ns`; `Progress{phase: Record}` to `wait` requesters (≥ 500 ms apart).
pub fn record_clock(state: &State, ctx: &Ctx, take: TakeId, elapsed_ns: u64) -> Outcome {
    todo!("record: RecordClock")
}

/// `timed_out` ⇒ `broken`; else record offsets and `Probe{Finalize}`.
pub fn capture_stopped(
    state: &State,
    ctx: &Ctx,
    take: TakeId,
    streams: Vec<RecordedStream>,
    timed_out: bool,
) -> Outcome {
    todo!("record: CaptureStopped")
}

/// The take is `broken`; later starts fail with "the capture engine is stuck
/// (GPU driver?); restart Qrec".
pub fn engine_wedged(state: &State, ctx: &Ctx, message: String) -> Outcome {
    todo!("record: EngineWedged")
}

/// Finalize (A§5.3) or Recover ("Recovered take 7 from the bog.").
pub fn take_probed(
    state: &State,
    ctx: &Ctx,
    take: TakeId,
    purpose: ProbePurpose,
    results: Vec<ProbeResult>,
) -> Outcome {
    todo!("record: TakeProbed")
}

/// Countdown ticks; the last one begins the take. Timers are only used by
/// the countdown so far.
pub fn timer_fired(state: &State, ctx: &Ctx, timer: TimerId) -> Outcome {
    todo!("record: TimerFired")
}
