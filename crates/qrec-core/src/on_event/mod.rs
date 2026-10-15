//! `on_event`: events in, the next state and effects out (A§4.1, foundation).
//!
//! `on_event` can't fail: failures become state plus `Notify` and `Reply`
//! errors. Handlers ignore (with a debug `Effect::Log`) any event whose
//! take, timer or job doesn't match the current phase or job table.
//!
//! | file | owner | events |
//! | --- | --- | --- |
//! | `record.rs` | record | capture, clock, probe and countdown timer events |
//! | `autocut.rs` | autocut | `SilenceDetected`, `SilenceFailed` |
//! | `output.rs` | output | export and frame events, `RequesterGone` |
//! | `disk.rs` | history | `ProjectChangedOnDisk`, `TakeDirsDeleted` |
//!
//! `Capabilities` is handled here.

pub mod autocut;
pub mod disk;
pub mod output;
pub mod record;

use crate::apply::{Outcome, begin_take_facts};
use crate::ctx::{Ctx, FactsNeeded};
use crate::protocol::effect::ProbePurpose;
use crate::protocol::event::Event;
use crate::state::{Capabilities, RecordPhase, State};

/// Feeds one event to the core (pure).
pub fn on_event(state: &State, ctx: &Ctx, ev: Event) -> Outcome {
    match ev {
        Event::Capabilities(caps) => capabilities(state, caps),
        Event::CaptureStarted {
            take,
            t0_ns,
            files,
            start_offset_ns,
        } => record::capture_started(state, ctx, take, t0_ns, files, start_offset_ns),
        Event::CaptureFailed {
            take,
            source,
            message,
            missing,
        } => record::capture_failed(state, ctx, take, source, message, missing),
        Event::StreamLost {
            take,
            stream,
            message,
        } => record::stream_lost(state, ctx, take, stream, message),
        Event::RecordClock { take, elapsed_ns } => {
            record::record_clock(state, ctx, take, elapsed_ns)
        }
        Event::CaptureStopped {
            take,
            streams,
            timed_out,
        } => record::capture_stopped(state, ctx, take, streams, timed_out),
        Event::EngineWedged { message } => record::engine_wedged(state, ctx, message),
        Event::TakeProbed {
            take,
            purpose,
            results,
        } => record::take_probed(state, ctx, take, purpose, results),
        Event::TimerFired { timer } => record::timer_fired(state, ctx, timer),
        Event::SilenceDetected { job, key, silences } => {
            autocut::silence_detected(state, ctx, job, key, silences)
        }
        Event::SilenceFailed { job, key, message } => {
            autocut::silence_failed(state, ctx, job, key, message)
        }
        Event::ExportProgress {
            job,
            segment,
            frames_done,
        } => output::export_progress(state, ctx, job, segment, frames_done),
        Event::ExportStitchProgress { job, out_time_ns } => {
            output::export_stitch_progress(state, ctx, job, out_time_ns)
        }
        Event::ExportFinished { job, path, bytes } => {
            output::export_finished(state, ctx, job, path, bytes)
        }
        Event::ExportFailed {
            job,
            message,
            stderr_tail,
            log,
        } => output::export_failed(state, ctx, job, message, stderr_tail, log),
        Event::ExportCancelled { job } => output::export_cancelled(state, ctx, job),
        Event::FrameSaved { job, path } => output::frame_saved(state, ctx, job, path),
        Event::FrameFailed { job, message, log } => {
            output::frame_failed(state, ctx, job, message, log)
        }
        Event::OutputAppeared {
            job,
            out,
            bytes,
            kept,
        } => output::output_appeared(state, ctx, job, out, bytes, kept),
        Event::TakeDirsDeleted {
            job,
            deleted,
            errors,
        } => disk::take_dirs_deleted(state, ctx, job, deleted, errors),
        Event::ProjectChangedOnDisk { project } => {
            disk::project_changed_on_disk(state, ctx, project)
        }
        Event::RequesterGone { req } => output::requester_gone(state, ctx, req),
    }
}

/// The shell detected (or re-detected) what the machine can do. Runtime only.
fn capabilities(state: &State, caps: Capabilities) -> Outcome {
    let mut next = state.clone();
    next.runtime.capabilities = caps;
    Outcome::new(next, Vec::new(), None)
}

/// The facts an event needs. Like `facts_needed` for commands: the last
/// countdown tick begins a take (take number, camera modes), and a
/// finalization probe may start autocut (file facts for silence keys).
pub fn facts_needed_for_event(state: &State, ev: &Event) -> FactsNeeded {
    let mut need = FactsNeeded::default();
    match (ev, &state.runtime.record) {
        (
            Event::TimerFired { timer },
            RecordPhase::Countdown {
                timer: current,
                remaining,
                ..
            },
        ) if timer == current && *remaining <= 1 => {
            begin_take_facts(&state.project, &mut need);
        }
        (
            Event::TakeProbed {
                purpose: ProbePurpose::Finalize,
                results,
                ..
            },
            _,
        ) => {
            need.files = results.iter().map(|r| r.file.clone()).collect();
        }
        _ => {}
    }
    need
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TimerId;
    use crate::state::{FfmpegCaps, RecordMode};
    use crate::testkit::{self, ProjectBuilder};

    #[test]
    fn capabilities_replace_the_runtime_copy() {
        let state = testkit::state(ProjectBuilder::new().build());
        let ctx = testkit::ctx("2026-10-14T12:00:00Z", 1);
        let caps = Capabilities {
            ffmpeg: Some(FfmpegCaps {
                version: "8.1.2".into(),
                identity: "/bin/ffmpeg".into(),
                encoders: Default::default(),
            }),
            ffprobe: None,
            vaapi: None,
            cores: 16,
        };
        let out = on_event(&state, &ctx, Event::Capabilities(caps.clone()));
        assert_eq!(out.state.runtime.capabilities, caps);
        assert!(out.effects.is_empty() && out.reply.is_none());
    }

    #[test]
    fn last_countdown_tick_needs_take_facts() {
        let mut state = testkit::state(ProjectBuilder::new().screen_layer("L1", "DP-1").build());
        state.runtime.record = RecordPhase::Countdown {
            remaining: 1,
            mode: RecordMode::Append,
            timer: TimerId(4),
            start_req: None,
            wait: false,
        };
        assert!(facts_needed_for_event(&state, &Event::TimerFired { timer: TimerId(4) }).take_dirs);
        assert!(
            facts_needed_for_event(&state, &Event::TimerFired { timer: TimerId(5) }).is_empty()
        );
    }
}
