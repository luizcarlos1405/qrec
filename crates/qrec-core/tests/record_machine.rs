//! The recording state machine (A§5.3), driven row by row through `apply`
//! and `on_event` with fake shell events, plus discard (A§5.5) and recovery.

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use qrec_core::apply::record::ENGINE_STUCK;
use qrec_core::capture_plan::plan_files;
use qrec_core::ctx::Ctx;
use qrec_core::ids::{ClipId, JobId, ReqId, TakeId, TimerId};
use qrec_core::model::{Clip, Fps, Project, StreamKind, TakeStatus};
use qrec_core::on_event::record::{EOS_TIMEOUT_REASON, recovery_probes, start_recovery};
use qrec_core::parse::ffprobe::parse_ffprobe;
use qrec_core::protocol::command::{Command, InsertAt, RecordModeArg};
use qrec_core::protocol::effect::{Effect, LogLevel, ProbePurpose};
use qrec_core::protocol::event::{Event, MediaInfo, ProbeError, ProbeResult, RecordedStream};
use qrec_core::protocol::progress::ProgressPhase;
use qrec_core::protocol::reply::{CmdResult, RecordStartedView, TakeFinalizedView};
use qrec_core::protocol::toast::{Toast, ToastLevel};
use qrec_core::state::{
    CanvasMode, ClipRef, DiscardRequest, HostKind, Job, JobKind, RecordMode, RecordPhase, State,
};
use qrec_core::testkit::{self, ProjectBuilder};
use qrec_core::time::frames_to_ns;
use qrec_core::{
    CoreError, ErrorCode, Outcome, apply, facts_needed, facts_needed_for_event, on_event,
};
use serde_json::json;

const NOW: &str = "2026-10-14T12:30:00Z";

// ------------------------------------------------------------------ kit

fn req(id: &str) -> ReqId {
    ReqId(id.to_owned())
}

fn ctx_for(by: Option<&str>) -> Ctx {
    let mut ctx = testkit::ctx(NOW, 42);
    ctx.req = by.map(req);
    ctx
}

/// A screen and a mic; take 1 is on the timeline as c001 + c002.
fn project() -> Project {
    ProjectBuilder::new()
        .screen_layer("L1", "DP-4")
        .mic("A1", "alsa_input.mic")
        .take("take-0001", 300)
        .clip("c001", "take-0001", 0, 100)
        .clip("c002", "take-0001", 100, 300)
        .build()
}

fn idle() -> State {
    testkit::state(project())
}

fn take2() -> TakeId {
    TakeId::numbered(2)
}

fn cmd(state: &State, by: Option<&str>, command: Command) -> Outcome {
    apply(state, &ctx_for(by), command).unwrap()
}

fn fail(state: &State, by: Option<&str>, command: Command) -> CoreError {
    apply(state, &ctx_for(by), command).unwrap_err()
}

fn event(state: &State, ev: Event) -> Outcome {
    on_event(state, &ctx_for(None), ev)
}

fn start(mode: RecordModeArg, countdown: Option<u32>, wait: bool) -> Command {
    Command::RecordStart {
        mode,
        countdown,
        wait,
    }
}

fn append() -> Command {
    start(RecordModeArg::Append, None, false)
}

fn over(ids: &[&str]) -> Command {
    start(
        RecordModeArg::Over(ids.iter().map(|id| ClipId::from_raw(*id)).collect()),
        None,
        false,
    )
}

fn replies(out: &Outcome) -> Vec<(ReqId, Result<CmdResult, CoreError>)> {
    out.effects
        .iter()
        .filter_map(|e| match e {
            Effect::Reply { req, reply } => Some((req.clone(), reply.clone())),
            _ => None,
        })
        .collect()
}

/// The single reply `id` gets from `out`.
fn reply_to(out: &Outcome, id: &str) -> Result<CmdResult, CoreError> {
    let mut all: Vec<_> = replies(out)
        .into_iter()
        .filter(|(r, _)| r.0 == id)
        .collect();
    assert_eq!(all.len(), 1, "one reply to {id}: {:#?}", out.effects);
    all.remove(0).1
}

fn toasts(out: &Outcome) -> Vec<&Toast> {
    out.effects
        .iter()
        .filter_map(|e| match e {
            Effect::Notify(t) => Some(t),
            _ => None,
        })
        .collect()
}

fn finalized_view(result: &CmdResult) -> TakeFinalizedView {
    serde_json::from_value(result.result.clone()).unwrap()
}

/// The event changes nothing but a debug log.
fn assert_ignored(state: &State, ev: Event) {
    let out = event(state, ev.clone());
    assert_eq!(out.state, *state, "{ev:?} changed the state");
    assert_eq!(out.reply, None);
    assert!(
        matches!(
            out.effects.as_slice(),
            [Effect::Log {
                level: LogLevel::Debug,
                ..
            }]
        ),
        "{ev:?}: {:?}",
        out.effects
    );
}

fn files(state: &State) -> Vec<String> {
    state.project.takes[&take2()]
        .streams
        .iter()
        .map(|s| s.file.clone())
        .collect()
}

/// Starting take 2 for request `s1`.
fn starting(wait: bool) -> State {
    cmd(
        &idle(),
        Some("s1"),
        start(RecordModeArg::Append, None, wait),
    )
    .state
}

/// Recording take 2 (started by `s1`).
fn recording(wait: bool) -> State {
    let s = starting(wait);
    let files = files(&s);
    event(
        &s,
        Event::CaptureStarted {
            take: take2(),
            t0_ns: 5,
            files,
            start_offset_ns: Default::default(),
        },
    )
    .state
}

/// Finalizing take 2, stopped by `t1`.
fn finalizing_from(state: &State) -> State {
    cmd(state, Some("t1"), Command::RecordStop).state
}

fn recorded(state: &State) -> Vec<RecordedStream> {
    state.project.takes[&take2()]
        .streams
        .iter()
        .map(|s| RecordedStream {
            id: s.id.clone(),
            file: s.file.clone(),
            start_offset_ns: if s.kind == StreamKind::Audio {
                3_000_000
            } else {
                0
            },
            eos: true,
        })
        .collect()
}

/// After `CaptureStopped`: waiting for the probe.
fn probing(state: &State) -> State {
    event(
        state,
        Event::CaptureStopped {
            take: take2(),
            streams: recorded(state),
            timed_out: false,
        },
    )
    .state
}

fn video(frames: u64) -> MediaInfo {
    MediaInfo {
        kind: StreamKind::Video,
        frames: Some(frames),
        fps: Some(Fps::new(30, 1)),
        width: Some(2560),
        height: Some(1440),
        sample_rate: None,
        channels: None,
        duration_ns: Some(frames_to_ns(frames, Fps::new(30, 1))),
        start_time_ns: 0,
        color_range: Some("tv".into()),
        color_space: Some("bt709".into()),
    }
}

fn audio(duration_ns: i64) -> MediaInfo {
    MediaInfo {
        kind: StreamKind::Audio,
        frames: None,
        fps: None,
        width: None,
        height: None,
        sample_rate: Some(48_000),
        channels: Some(1),
        duration_ns: Some(duration_ns),
        start_time_ns: 0,
        color_range: None,
        color_space: None,
    }
}

/// Every stream of `take` readable, `frames` long.
fn probed(state: &State, take: &TakeId, frames: u64) -> Vec<ProbeResult> {
    state.project.takes[take]
        .streams
        .iter()
        .map(|s| ProbeResult {
            id: s.id.clone(),
            file: s.file.clone(),
            info: Ok(match s.kind {
                StreamKind::Video => video(frames),
                StreamKind::Audio => audio(frames_to_ns(frames, Fps::new(30, 1))),
            }),
        })
        .collect()
}

fn finalize(state: &State, frames: u64) -> Outcome {
    event(
        state,
        Event::TakeProbed {
            take: take2(),
            purpose: ProbePurpose::Finalize,
            results: probed(state, &take2(), frames),
        },
    )
}

fn clip_ids(state: &State) -> Vec<String> {
    state
        .project
        .timeline
        .clips
        .iter()
        .map(|c| c.id.to_string())
        .collect()
}

// ------------------------------------------------------- Idle + Start

#[test]
fn idle_start_begins_a_take() {
    let state = idle();
    let command = append();
    let need = facts_needed(&state, &command);
    assert!(need.take_dirs, "the take number scans takes/");
    let out = cmd(&state, Some("s1"), command);
    assert_eq!(out.reply, None, "answered once capture runs");

    let take = &out.state.project.takes[&take2()];
    assert_eq!(take.status, TakeStatus::Recording);
    assert_eq!(take.recorded_at, ctx_for(None).now);
    assert_eq!(take.duration_frames, 0);
    assert_eq!(take.scene, state.project.scene, "the scene is snapshotted");
    let streams: Vec<(&str, &str)> = take
        .streams
        .iter()
        .map(|s| (s.id.as_str(), s.file.as_str()))
        .collect();
    assert_eq!(
        streams,
        [
            ("L1", "takes/take-0002/L1-screen-DP-4.mkv"),
            ("A1", "takes/take-0002/A1-mic.mka")
        ]
    );
    assert!(
        take.streams
            .iter()
            .all(|s| s.width.is_none() && s.sample_rate.is_none())
    );
    assert_eq!(out.state.project.next_take_number, 3);
    assert_eq!(
        out.state.runtime.record,
        RecordPhase::Starting {
            take: take2(),
            mode: RecordMode::Append,
            start_req: Some(req("s1")),
            wait: false
        }
    );
    assert!(
        out.state.history.entries.is_empty(),
        "facts are not undoable"
    );

    // CreateTakeDir, WriteProject, then StartCapture; the GUI already shows Live.
    let [
        Effect::CreateTakeDir { path },
        Effect::WriteProject,
        Effect::StartCapture { plan },
    ] = out.effects.as_slice()
    else {
        panic!("{:#?}", out.effects)
    };
    assert_eq!(path, "takes/take-0002");
    assert_eq!(plan.take, take2());
    assert_eq!(plan_files(plan), files(&out.state));
}

/// A CLI command's own host never records (A§8.5): a recording there
/// would run inside that command's process, hold its result back and end
/// with its terminal. Start and toggle are `Busy` and change nothing.
#[test]
fn a_cli_host_never_records() {
    let mut state = idle();
    state.runtime.host = HostKind::Cli;
    for command in [
        append(),
        start(RecordModeArg::Append, Some(3), true),
        over(&["c001"]),
        Command::RecordToggle { wait: false },
        Command::RecordToggle { wait: true },
    ] {
        let err = fail(&state, Some("s1"), command.clone());
        assert_eq!(err.code, ErrorCode::Busy, "{command:?}");
        assert_eq!(
            err.message,
            qrec_core::apply::record::CLI_HOST_BUSY,
            "{command:?}"
        );
        assert!(
            err.hint
                .is_some_and(|h| h.contains("once that command finishes"))
        );
    }
    // Stopping finds nothing to stop, as anywhere nothing records.
    assert_eq!(
        fail(&state, Some("s2"), Command::RecordStop).code,
        ErrorCode::Invalid
    );
    // The other hosts record the same scene.
    for host in [HostKind::Gui, HostKind::Headless] {
        let mut other = idle();
        other.runtime.host = host;
        let out = cmd(&other, Some("s1"), append());
        assert!(
            matches!(out.state.runtime.record, RecordPhase::Starting { .. }),
            "{host:?}"
        );
    }
}

fn export_job(id: u64) -> Job {
    Job {
        id: JobId(id),
        kind: JobKind::Export {
            out: "/tmp/x.mp4".into(),
            files: Default::default(),
            total_frames: 10,
            seg_frames: vec![],
            seg_done: vec![],
            seg_clips: Vec::new(),
            stitch_total_ns: 0,
            stitch_done_ns: 0,
            stitching: false,
        },
        req: None,
        started: ctx_for(None).now,
    }
}

/// Recording is blocked during an export (SPEC §13.1) with one answer,
/// whichever host runs the export: a CLI command's own host running one
/// says "an export is running", like a GUI or headless host, not that it
/// doesn't record (A§5.3). The shell answers a recording from that host's
/// `hello` with the same helper (A§8.5), so a key press during an
/// in-process export is refused at once instead of waiting for it.
#[test]
fn an_export_blocks_recording_the_same_way_in_every_host() {
    for host in [HostKind::Cli, HostKind::Gui, HostKind::Headless] {
        let mut state = idle();
        state.runtime.host = host;
        state.runtime.jobs.insert(JobId(4), export_job(4));
        for command in [append(), Command::RecordToggle { wait: false }] {
            let err = fail(&state, Some("s1"), command.clone());
            assert_eq!(err.code, ErrorCode::Busy, "{host:?} {command:?}");
            assert_eq!(
                err.message, "an export is running (job-4)",
                "{host:?} {command:?}"
            );
            assert_eq!(
                err.hint.as_deref(),
                Some("wait for it to finish, or cancel it with qrec export cancel")
            );
            assert_eq!(
                Some(err),
                qrec_core::apply::record::export_blocks_recording(state.runtime.jobs.values()),
                "the shell's answer is the core's"
            );
        }
    }
    // Other jobs don't block a recording; a CLI host still doesn't record.
    let purge = Job {
        id: JobId(5),
        kind: JobKind::Purge { takes: vec![] },
        req: None,
        started: ctx_for(None).now,
    };
    let frame = Job {
        id: JobId(6),
        kind: JobKind::FrameSave {
            out: "/tmp/f.png".into(),
            files: Default::default(),
        },
        req: None,
        started: ctx_for(None).now,
    };
    assert_eq!(
        qrec_core::apply::record::export_blocks_recording([&purge, &frame]),
        None
    );
    let mut cli = idle();
    cli.runtime.host = HostKind::Cli;
    cli.runtime.jobs.insert(JobId(5), purge);
    assert_eq!(
        fail(&cli, Some("s1"), append()).message,
        qrec_core::apply::record::CLI_HOST_BUSY
    );
}

#[test]
fn a_host_without_live_mode_starts_the_live_pipeline_first() {
    for (host, mode) in [
        (HostKind::Headless, CanvasMode::Live),
        (HostKind::Headless, CanvasMode::Program),
        (HostKind::Gui, CanvasMode::Program),
    ] {
        let mut state = idle();
        state.runtime.host = host;
        state.ui.canvas_mode = mode;
        let out = cmd(&state, Some("s1"), append());
        let names: Vec<&str> = out
            .effects
            .iter()
            .map(|e| match e {
                Effect::CreateTakeDir { .. } => "mkdir",
                Effect::WriteProject => "project",
                Effect::SetLive(Some(live)) => {
                    assert!(!live.preview);
                    assert_eq!(live.scene, state.project.scene);
                    "live"
                }
                Effect::StartCapture { .. } => "capture",
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(names, ["mkdir", "project", "live", "capture"], "{host:?}");
    }
}

/// The `SetLive` effects of an outcome.
fn live(out: &Outcome) -> Vec<Option<bool>> {
    out.effects
        .iter()
        .filter_map(|e| match e {
            Effect::SetLive(scene) => Some(scene.as_ref().map(|s| s.preview)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_live_pipeline_follows_the_phase() {
    // Headless with a countdown: the sources start with the countdown and
    // stop once the take is finalized.
    let mut state = idle();
    state.runtime.host = HostKind::Headless;
    let out = cmd(
        &state,
        Some("s1"),
        start(RecordModeArg::Append, Some(1), false),
    );
    assert_eq!(live(&out), [Some(false)], "no preview branch");
    let RecordPhase::Countdown { timer, .. } = out.state.runtime.record else {
        panic!()
    };
    let out = event(&out.state, Event::TimerFired { timer });
    assert!(matches!(
        out.state.runtime.record,
        RecordPhase::Starting { .. }
    ));
    assert_eq!(live(&out), [], "already running");
    let files = files(&out.state);
    let out = event(
        &out.state,
        Event::CaptureStarted {
            take: take2(),
            t0_ns: 1,
            files,
            start_offset_ns: Default::default(),
        },
    );
    assert_eq!(live(&out), []);
    let stopping = cmd(&out.state, Some("t1"), Command::RecordStop);
    assert_eq!(live(&stopping), []);
    let stopped = event(
        &stopping.state,
        Event::CaptureStopped {
            take: take2(),
            streams: recorded(&stopping.state),
            timed_out: false,
        },
    );
    assert_eq!(live(&stopped), [], "until the take is finalized");
    let done = finalize(&stopped.state, 90);
    assert_eq!(live(&done), [None]);

    // A cancelled countdown stops what it started.
    let counting = cmd(
        &state,
        Some("s1"),
        start(RecordModeArg::Append, Some(3), false),
    )
    .state;
    let out = cmd(&counting, Some("c1"), Command::RecordCancel);
    assert_eq!(live(&out), [None]);

    // The GUI in Program mode: a pipeline without preview, stopped after the take.
    let mut program = idle();
    program.ui.canvas_mode = CanvasMode::Program;
    let out = cmd(&program, Some("s1"), append());
    assert_eq!(live(&out), [Some(false)]);
    // Switching to Live while recording (the UI owner's SetCanvasMode) keeps
    // the pipeline once the take is done.
    let mut switched = finalizing_from(
        &event(
            &out.state,
            Event::CaptureStarted {
                take: take2(),
                t0_ns: 1,
                files: vec![],
                start_offset_ns: Default::default(),
            },
        )
        .state,
    );
    switched.ui.canvas_mode = CanvasMode::Live;
    let done = finalize(&probing(&switched), 90);
    assert_eq!(live(&done), []);

    // The GUI in Live mode never starts or stops anything.
    let out = cmd(
        &idle(),
        Some("s1"),
        start(RecordModeArg::Append, Some(1), false),
    );
    assert_eq!(live(&out), []);
}

#[test]
fn the_take_number_never_reuses_a_folder() {
    let state = idle();
    let mut ctx = ctx_for(Some("s1"));
    ctx.facts.min_take_number = 7;
    let out = apply(&state, &ctx, append()).unwrap();
    assert!(out.state.project.takes.contains_key(&TakeId::numbered(7)));
    assert_eq!(out.state.project.next_take_number, 8);
}

#[test]
fn idle_start_rejections() {
    let state = idle();
    // An export runs.
    let mut exporting = state.clone();
    exporting.runtime.jobs.insert(JobId(3), export_job(3));
    for command in [append(), Command::RecordToggle { wait: false }] {
        let err = fail(&exporting, Some("s1"), command);
        assert_eq!(err.code, ErrorCode::Busy);
        assert_eq!(err.message, "an export is running (job-3)");
    }
    // An empty scene.
    let empty = testkit::state(ProjectBuilder::new().build());
    for command in [append(), Command::RecordToggle { wait: false }] {
        assert_eq!(fail(&empty, None, command).code, ErrorCode::Invalid);
    }
    // Record over needs contiguous existing clips.
    let three = testkit::state(
        ProjectBuilder::new()
            .screen_layer("L1", "DP-4")
            .take("take-0001", 300)
            .clip("c001", "take-0001", 0, 100)
            .clip("c002", "take-0001", 100, 200)
            .clip("c003", "take-0001", 200, 300)
            .build(),
    );
    assert_eq!(
        fail(&three, None, over(&["c001", "c003"])).code,
        ErrorCode::Invalid
    );
    assert_eq!(
        fail(&three, None, over(&["c404"])).code,
        ErrorCode::NotFound
    );
    let err = fail(
        &three,
        None,
        start(RecordModeArg::OverSelection, None, false),
    );
    assert_eq!(err.code, ErrorCode::Unavailable);
    // The countdown is 0–10 s.
    let err = fail(&state, None, start(RecordModeArg::Append, Some(11), false));
    assert_eq!(err.code, ErrorCode::Usage);
}

#[test]
fn over_is_stored_as_clip_refs() {
    let mut state = idle();
    state.ui.selection = vec![ClipId::from_raw("c002"), ClipId::from_raw("c001")];
    let out = cmd(
        &state,
        Some("s1"),
        start(RecordModeArg::OverSelection, None, false),
    );
    let RecordPhase::Starting { mode, .. } = &out.state.runtime.record else {
        panic!("{:?}", out.state.runtime.record)
    };
    assert_eq!(
        *mode,
        RecordMode::Over {
            clips: state
                .project
                .timeline
                .clips
                .iter()
                .map(ClipRef::of)
                .collect()
        }
    );
}

#[test]
fn idle_stop_and_cancel_are_invalid() {
    for command in [Command::RecordStop, Command::RecordCancel] {
        let err = fail(&idle(), Some("t1"), command);
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(err.message, "not recording");
    }
}

// ----------------------------------------------------------- Countdown

#[test]
fn countdown_ticks_then_begins_the_take() {
    let state = idle();
    let command = start(RecordModeArg::Append, Some(3), true);
    assert!(
        facts_needed(&state, &command).is_empty(),
        "the last tick asks for the take facts"
    );
    let out = cmd(&state, Some("s1"), command);
    assert_eq!(out.reply, None);
    assert!(!out.state.project.takes.contains_key(&take2()));
    let RecordPhase::Countdown {
        remaining: 3,
        timer,
        wait: true,
        ..
    } = out.state.runtime.record.clone()
    else {
        panic!("{:?}", out.state.runtime.record)
    };
    assert_eq!(
        out.effects[0],
        Effect::StartTimer {
            timer,
            after_ms: 1000
        }
    );
    let Effect::Progress { req: to, progress } = &out.effects[1] else {
        panic!("{:?}", out.effects)
    };
    assert_eq!(
        (to.0.as_str(), progress.label.as_str()),
        ("s1", "Recording in 3…")
    );
    assert_eq!(progress.eta_secs, Some(3));

    // A stale timer changes nothing.
    assert_ignored(
        &out.state,
        Event::TimerFired {
            timer: TimerId(999),
        },
    );

    let mut state = out.state;
    let mut current = timer;
    for left in [2, 1] {
        let fired = Event::TimerFired { timer: current };
        assert!(facts_needed_for_event(&state, &fired).is_empty());
        let out = event(&state, fired);
        let RecordPhase::Countdown {
            remaining, timer, ..
        } = out.state.runtime.record.clone()
        else {
            panic!()
        };
        assert_eq!(remaining, left);
        assert_ne!(timer, current, "every tick has its own timer");
        assert!(out.effects.contains(&Effect::StartTimer {
            timer,
            after_ms: 1000
        }));
        assert_ignored(&out.state, Event::TimerFired { timer: current });
        current = timer;
        state = out.state;
    }
    let last = Event::TimerFired { timer: current };
    assert!(facts_needed_for_event(&state, &last).take_dirs);
    let out = event(&state, last);
    assert_eq!(
        out.state.runtime.record,
        RecordPhase::Starting {
            take: take2(),
            mode: RecordMode::Append,
            start_req: Some(req("s1")),
            wait: true
        }
    );
    assert!(matches!(
        out.effects.last(),
        Some(Effect::StartCapture { .. })
    ));
    assert_eq!(
        out.state.project.takes[&take2()].status,
        TakeStatus::Recording
    );
}

#[test]
fn countdown_stop_toggle_cancel() {
    let counting = cmd(
        &idle(),
        Some("s1"),
        start(RecordModeArg::Append, Some(2), false),
    )
    .state;
    let RecordPhase::Countdown { timer, .. } = counting.runtime.record else {
        panic!()
    };
    for command in [
        Command::RecordStop,
        Command::RecordToggle { wait: false },
        Command::RecordCancel,
    ] {
        let out = cmd(&counting, Some("c1"), command);
        assert_eq!(out.state.runtime.record, RecordPhase::Idle);
        assert_eq!(out.state.project, counting.project);
        assert_eq!(out.reply, Some(Ok(CmdResult::new("Countdown cancelled"))));
        assert_eq!(out.effects[0], Effect::CancelTimer { timer });
        let err = reply_to(&out, "s1").unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::Invalid, "countdown cancelled")
        );
        // The cancelled timer may still fire.
        assert_ignored(&out.state, Event::TimerFired { timer });
    }
    // Everything else is Busy during a countdown.
    let err = fail(&counting, None, append());
    assert_eq!(err.code, ErrorCode::Busy);
    assert_eq!(err.message, "a recording countdown is running");
}

#[test]
fn a_countdown_that_cannot_begin_fails_the_start_request() {
    let counting = cmd(
        &idle(),
        Some("s1"),
        start(RecordModeArg::Append, Some(1), false),
    )
    .state;
    let RecordPhase::Countdown { timer, .. } = counting.runtime.record else {
        panic!()
    };
    // The scene was emptied meanwhile (e.g. by an external edit).
    let mut emptied = counting.clone();
    emptied.project.scene.layers.clear();
    emptied.project.scene.audio.clear();
    let out = event(&emptied, Event::TimerFired { timer });
    assert_eq!(out.state.runtime.record, RecordPhase::Idle);
    assert_eq!(reply_to(&out, "s1").unwrap_err().code, ErrorCode::Invalid);
    assert_eq!(toasts(&out)[0].title, "Recording didn't start");
}

#[test]
fn a_pipeline_failure_ends_the_countdown() {
    // The sources start with the countdown; if the pipeline fails, the
    // recording can't start: the countdown ends and the start request fails.
    let mut headless = idle();
    headless.runtime.host = HostKind::Headless;
    let counting = cmd(
        &headless,
        Some("s1"),
        start(RecordModeArg::Append, Some(3), true),
    )
    .state;
    let RecordPhase::Countdown { timer, .. } = counting.runtime.record else {
        panic!()
    };
    let out = event(
        &counting,
        Event::CaptureFailed {
            take: None,
            source: Some("L1".into()),
            message: "portal request denied".into(),
            missing: None,
        },
    );
    assert_eq!(out.state.runtime.record, RecordPhase::Idle);
    assert_eq!(out.state.project, counting.project, "no take was created");
    assert!(out.effects.contains(&Effect::CancelTimer { timer }));
    assert!(out.effects.contains(&Effect::SetLive(None)));
    let err = reply_to(&out, "s1").unwrap_err();
    assert_eq!(err.code, ErrorCode::CaptureFailed);
    assert_eq!(
        err.message,
        "capture failed: L1 (Monitor DP-4): portal request denied"
    );
    assert_eq!(toasts(&out)[0].title, "Capture failed");
    // The cancelled timer may still fire.
    assert_ignored(&out.state, Event::TimerFired { timer });
    // A take's failure during a countdown is stale (there is no take yet).
    assert_ignored(
        &counting,
        Event::CaptureFailed {
            take: Some(TakeId::numbered(1)),
            source: None,
            message: "late".into(),
            missing: None,
        },
    );
}

// ------------------------------------------------------------ Starting

#[test]
fn starting_capture_started() {
    let state = starting(false);
    let files = files(&state);
    let out = event(
        &state,
        Event::CaptureStarted {
            take: take2(),
            t0_ns: 77,
            files: files.clone(),
            start_offset_ns: Default::default(),
        },
    );
    assert_eq!(
        out.state.runtime.record,
        RecordPhase::Recording {
            take: take2(),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![]
        }
    );
    let result = reply_to(&out, "s1").unwrap();
    assert_eq!(result.message, "Recording take 2");
    assert_eq!(result.created, ["take-0002"]);
    assert_eq!(
        serde_json::from_value::<RecordStartedView>(result.result).unwrap(),
        RecordStartedView {
            take: take2(),
            mode: RecordMode::Append,
            files
        }
    );

    // With --wait the start request waits for the whole take; its progress
    // starts at 00:00.
    let state = starting(true);
    let out = event(
        &state,
        Event::CaptureStarted {
            take: take2(),
            t0_ns: 77,
            files: vec![],
            start_offset_ns: Default::default(),
        },
    );
    assert!(replies(&out).is_empty());
    assert!(matches!(
        &out.state.runtime.record,
        RecordPhase::Recording { waiters, .. } if *waiters == [req("s1")]
    ));
    let progress: Vec<(&str, &str)> = out
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::Progress { req, progress } => Some((req.0.as_str(), progress.label.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(progress, [("s1", "Recording take 2: 00:00")]);

    // Stale: another take, or not starting.
    assert_ignored(
        &starting(false),
        Event::CaptureStarted {
            take: TakeId::numbered(9),
            t0_ns: 1,
            files: vec![],
            start_offset_ns: Default::default(),
        },
    );
    assert_ignored(
        &idle(),
        Event::CaptureStarted {
            take: take2(),
            t0_ns: 1,
            files: vec![],
            start_offset_ns: Default::default(),
        },
    );
    assert_ignored(
        &recording(false),
        Event::CaptureStarted {
            take: take2(),
            t0_ns: 1,
            files: vec![],
            start_offset_ns: Default::default(),
        },
    );
}

#[test]
fn starting_capture_failed_breaks_the_take() {
    let state = starting(false);
    let out = event(
        &state,
        Event::CaptureFailed {
            take: Some(take2()),
            source: Some("L1".into()),
            message: "portal request denied".into(),
            missing: None,
        },
    );
    assert_eq!(out.state.runtime.record, RecordPhase::Idle);
    let take = &out.state.project.takes[&take2()];
    assert_eq!(take.status, TakeStatus::Broken);
    assert_eq!(
        take.error.as_deref(),
        Some("L1 (Monitor DP-4): portal request denied")
    );
    assert!(take.streams.is_empty(), "nothing was probed");
    assert!(take.scene.layers.iter().all(|l| l.missing));
    assert!(take.scene.audio.iter().all(|t| t.missing));
    assert!(!take.placed);
    let err = reply_to(&out, "s1").unwrap_err();
    assert_eq!(err.code, ErrorCode::CaptureFailed);
    assert_eq!(
        err.message,
        "capture failed: L1 (Monitor DP-4): portal request denied"
    );
    assert!(err.hint.unwrap().contains("L1 (Monitor DP-4)"));
    assert!(out.effects.contains(&Effect::WriteProject));
    assert_eq!(toasts(&out)[0].level, ToastLevel::Error);
    assert!(out.state.history.entries.is_empty());
    assert!(
        !out.effects.iter().any(|e| matches!(e, Effect::SetLive(_))),
        "the GUI keeps its Live view"
    );

    // A headless host stops the pipeline it started for the take.
    let mut headless = idle();
    headless.runtime.host = HostKind::Headless;
    let state = cmd(&headless, Some("s1"), append()).state;
    let out = event(
        &state,
        Event::CaptureFailed {
            take: None,
            source: None,
            message: "pipeline error".into(),
            missing: None,
        },
    );
    assert_eq!(out.state.project.takes[&take2()].status, TakeStatus::Broken);
    assert!(out.effects.contains(&Effect::SetLive(None)));
    assert_eq!(
        reply_to(&out, "s1").unwrap_err().code,
        ErrorCode::CaptureFailed
    );

    // Another take's failure is stale.
    assert_ignored(
        &starting(false),
        Event::CaptureFailed {
            take: Some(TakeId::numbered(9)),
            source: None,
            message: "x".into(),
            missing: None,
        },
    );
    // A live-pipeline failure while idle only tells people.
    let out = event(
        &idle(),
        Event::CaptureFailed {
            take: None,
            source: Some("L1".into()),
            message: "portal closed".into(),
            missing: None,
        },
    );
    assert_eq!(out.state, idle());
    assert_eq!(
        toasts(&out)[0].body.as_deref(),
        Some("L1 (Monitor DP-4): portal closed")
    );
}

/// A GStreamer element that isn't installed is a missing dependency (exit
/// 5, A§4.8) whose hint names it, not `capture_failed`; the take is broken
/// as for any start failure (SPEC §5.3 writes it before capture starts).
#[test]
fn a_missing_element_is_a_missing_dependency() {
    let missing = || Event::CaptureFailed {
        take: Some(take2()),
        source: Some("L1".into()),
        message: "the GStreamer element videotestsrc is missing (see `qrec doctor`)".into(),
        missing: Some("videotestsrc".into()),
    };
    let out = event(&starting(false), missing());
    assert_eq!(out.state.runtime.record, RecordPhase::Idle);
    assert_eq!(out.state.project.takes[&take2()].status, TakeStatus::Broken);
    let err = reply_to(&out, "s1").unwrap_err();
    assert_eq!(
        (err.code, err.exit_code()),
        (ErrorCode::MissingDependency, 5)
    );
    assert_eq!(
        err.message,
        "capture failed: L1 (Monitor DP-4): the GStreamer element videotestsrc is missing (see `qrec doctor`)"
    );
    let hint = err.hint.unwrap();
    assert!(
        hint.contains("videotestsrc") && hint.contains("qrec doctor"),
        "{hint}"
    );

    // The same while a countdown runs (the live pipeline failed).
    let mut headless = idle();
    headless.runtime.host = HostKind::Headless;
    let counting = cmd(
        &headless,
        Some("s1"),
        start(RecordModeArg::Append, Some(3), true),
    )
    .state;
    let out = event(
        &counting,
        Event::CaptureFailed {
            take: None,
            source: Some("L1".into()),
            message: "the GStreamer element pipewiresrc is missing (see `qrec doctor`)".into(),
            missing: Some("pipewiresrc".into()),
        },
    );
    assert_eq!(
        reply_to(&out, "s1").unwrap_err().code,
        ErrorCode::MissingDependency
    );
}

#[test]
fn starting_stop_and_toggle_answer_the_start_request() {
    for command in [Command::RecordStop, Command::RecordToggle { wait: true }] {
        let state = starting(false);
        let out = cmd(&state, Some("t1"), command);
        assert_eq!(out.reply, None, "the stopper waits for the finalized take");
        assert_eq!(
            out.effects[0],
            Effect::StopCapture {
                take: take2(),
                cancel: false
            }
        );
        let started = reply_to(&out, "s1").unwrap();
        assert_eq!(started.message, "Recording take 2");
        let view: RecordStartedView = serde_json::from_value(started.result).unwrap();
        assert_eq!(view.files, files(&state));
        assert_eq!(
            out.state.runtime.record,
            RecordPhase::Finalizing {
                take: take2(),
                mode: RecordMode::Append,
                cancelled: false,
                stop_reqs: vec![req("t1")],
                waiters: vec![],
                discard: None
            }
        );
        let progress: Vec<_> = out
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::Progress { req, progress } => Some((req.0.clone(), progress.label.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(progress, [("t1".to_owned(), "Stopping take 2".to_owned())]);
    }
    // A --wait start request joins the waiters instead.
    let out = cmd(&starting(true), Some("t1"), Command::RecordStop);
    assert!(replies(&out).is_empty());
    assert!(matches!(
        &out.state.runtime.record,
        RecordPhase::Finalizing { waiters, .. } if *waiters == [req("s1")]
    ));
}

#[test]
fn cancel_while_starting_or_recording() {
    for state in [starting(false), recording(false)] {
        let out = cmd(&state, Some("x1"), Command::RecordCancel);
        assert_eq!(
            out.effects[0],
            Effect::StopCapture {
                take: take2(),
                cancel: true
            }
        );
        assert!(matches!(
            &out.state.runtime.record,
            RecordPhase::Finalizing { cancelled: true, stop_reqs, .. } if *stop_reqs == [req("x1")]
        ));
    }
}

// ----------------------------------------------------------- Recording

#[test]
fn recording_stop_and_toggle() {
    for command in [Command::RecordStop, Command::RecordToggle { wait: false }] {
        let state = recording(true);
        let out = cmd(&state, Some("t1"), command);
        assert_eq!(out.reply, None);
        assert_eq!(
            out.effects[0],
            Effect::StopCapture {
                take: take2(),
                cancel: false
            }
        );
        assert!(replies(&out).is_empty());
        assert_eq!(
            out.state.runtime.record,
            RecordPhase::Finalizing {
                take: take2(),
                mode: RecordMode::Append,
                cancelled: false,
                stop_reqs: vec![req("t1")],
                waiters: vec![req("s1")],
                discard: None
            }
        );
    }
    // Everything that isn't R is Busy.
    let err = fail(&recording(false), None, Command::Undo { steps: 1 });
    assert_eq!(
        (err.code, err.message.as_str()),
        (ErrorCode::Busy, "recording take 2")
    );
    assert_eq!(
        fail(&recording(false), None, Command::TakeDiscardLast).code,
        ErrorCode::Busy
    );
}

#[test]
fn record_clock_updates_and_reports_each_new_second() {
    let mut state = recording(true);
    let mut sent = Vec::new();
    // ~4 Hz with jitter around the half-second edges.
    for ms in [
        249u64, 501, 749, 999, 1_001, 1_249, 1_499, 1_501, 1_999, 2_001, 2_250,
    ] {
        let out = event(
            &state,
            Event::RecordClock {
                take: take2(),
                elapsed_ns: ms * 1_000_000,
            },
        );
        assert!(matches!(
            out.state.runtime.record,
            RecordPhase::Recording { elapsed_ns, .. } if elapsed_ns == ms * 1_000_000
        ));
        assert!(out.state.project == state.project, "runtime only");
        for e in &out.effects {
            let Effect::Progress { req: to, progress } = e else {
                panic!("{e:?}")
            };
            assert_eq!(to.0, "s1");
            assert_eq!(progress.phase, ProgressPhase::Record);
            assert_eq!(progress.take, Some(take2()));
            sent.push(progress.done);
        }
        state = out.state;
    }
    assert_eq!(sent, [1_001, 2_001], "once per second, > 500 ms apart");
    // A late clock never rewinds the timer (or re-announces a second).
    let late = event(
        &state,
        Event::RecordClock {
            take: take2(),
            elapsed_ns: 1_000_000_000,
        },
    );
    assert!(late.effects.is_empty());
    assert!(matches!(
        late.state.runtime.record,
        RecordPhase::Recording {
            elapsed_ns: 2_250_000_000,
            ..
        }
    ));
    let out = event(
        &state,
        Event::RecordClock {
            take: take2(),
            elapsed_ns: 65_000_000_000,
        },
    );
    let Effect::Progress { progress, .. } = &out.effects[0] else {
        panic!()
    };
    assert_eq!(progress.label, "Recording take 2: 01:05");

    // No waiter, no progress.
    let quiet = event(
        &recording(false),
        Event::RecordClock {
            take: take2(),
            elapsed_ns: 2_000_000_000,
        },
    );
    assert!(quiet.effects.is_empty());
    // Stale clocks.
    assert_ignored(
        &recording(false),
        Event::RecordClock {
            take: TakeId::numbered(9),
            elapsed_ns: 1,
        },
    );
    assert_ignored(
        &starting(false),
        Event::RecordClock {
            take: take2(),
            elapsed_ns: 1,
        },
    );
}

#[test]
fn stream_lost_marks_missing_and_the_last_one_stops() {
    let state = recording(true);
    let lost = |s: &State, id: &str| {
        event(
            s,
            Event::StreamLost {
                take: take2(),
                stream: id.into(),
                message: "device unplugged".into(),
            },
        )
    };
    let out = lost(&state, "A1");
    let take = &out.state.project.takes[&take2()];
    assert!(take.scene.audio[0].missing);
    assert!(!take.scene.layers[0].missing);
    assert!(out.effects.contains(&Effect::WriteProject));
    assert_eq!(toasts(&out)[0].title, "Lost A1 (Mic)");
    assert!(matches!(
        out.state.runtime.record,
        RecordPhase::Recording { .. }
    ));
    assert!(out.state.history.entries.is_empty(), "a fact");
    // Twice: nothing new.
    assert_ignored(
        &out.state,
        Event::StreamLost {
            take: take2(),
            stream: "A1".into(),
            message: "again".into(),
        },
    );

    // The last stream: act as Stop (the waiter keeps waiting).
    let out = lost(&out.state, "L1");
    assert!(out.effects.contains(&Effect::StopCapture {
        take: take2(),
        cancel: false
    }));
    assert_eq!(
        out.state.runtime.record,
        RecordPhase::Finalizing {
            take: take2(),
            mode: RecordMode::Append,
            cancelled: false,
            stop_reqs: vec![],
            waiters: vec![req("s1")],
            discard: None
        }
    );
    // Unknown stream or take.
    assert_ignored(
        &state,
        Event::StreamLost {
            take: take2(),
            stream: "L9".into(),
            message: "x".into(),
        },
    );
    assert_ignored(
        &idle(),
        Event::StreamLost {
            take: TakeId::numbered(1),
            stream: "L1".into(),
            message: "x".into(),
        },
    );
}

#[test]
fn a_take_whose_every_source_was_lost_is_broken() {
    // Both sources get unplugged: the last loss stops the take; the files are
    // readable, but nothing of the take is usable.
    let mut current = recording(true);
    for id in ["A1", "L1"] {
        current = event(
            &current,
            Event::StreamLost {
                take: take2(),
                stream: id.into(),
                message: "device unplugged".into(),
            },
        )
        .state;
    }
    assert!(matches!(
        current.runtime.record,
        RecordPhase::Finalizing { .. }
    ));
    let out = finalize(&probing(&current), 90);
    assert_eq!(out.state.runtime.record, RecordPhase::Idle);
    let take = &out.state.project.takes[&take2()];
    assert_eq!(take.status, TakeStatus::Broken);
    assert_eq!(
        take.error.as_deref(),
        Some("every source was lost during the take")
    );
    assert_eq!(take.streams.len(), 2, "the probed files stay listed");
    assert!(take.scene.layers[0].missing && take.scene.audio[0].missing);
    assert_eq!(out.state.project.timeline, idle().project.timeline);
    let view = finalized_view(&reply_to(&out, "s1").unwrap());
    assert_eq!((view.status, view.placed), (TakeStatus::Broken, false));
}

// ---------------------------------------------------------- Finalizing

#[test]
fn finalizing_stop_joins_and_the_rest_is_busy() {
    let state = finalizing_from(&recording(false));
    let out = cmd(&state, Some("t2"), Command::RecordStop);
    assert_eq!(out.reply, None);
    assert!(out.effects.is_empty());
    assert!(matches!(
        &out.state.runtime.record,
        RecordPhase::Finalizing { stop_reqs, .. } if *stop_reqs == [req("t1"), req("t2")]
    ));
    for command in [
        append(),
        Command::RecordToggle { wait: false },
        Command::RecordCancel,
    ] {
        let err = fail(&state, None, command);
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::Busy, "finalizing take 2")
        );
    }
}

#[test]
fn capture_stopped_records_offsets_and_probes() {
    let state = finalizing_from(&recording(true));
    let out = event(
        &state,
        Event::CaptureStopped {
            take: take2(),
            streams: recorded(&state),
            timed_out: false,
        },
    );
    let take = &out.state.project.takes[&take2()];
    assert_eq!(take.stream("A1").unwrap().start_offset_ns, 3_000_000);
    assert_eq!(take.status, TakeStatus::Recording, "until probed");
    let Some(Effect::Probe {
        take: probe_take,
        purpose: ProbePurpose::Finalize,
        jobs,
    }) = out
        .effects
        .iter()
        .find(|e| matches!(e, Effect::Probe { .. }))
    else {
        panic!("{:?}", out.effects)
    };
    assert_eq!(*probe_take, take2());
    let jobs: Vec<(&str, StreamKind, &str, &str)> = jobs
        .iter()
        .map(|j| {
            (
                j.id.as_str(),
                j.kind,
                j.argv[0].as_str(),
                j.argv.last().unwrap().as_str(),
            )
        })
        .collect();
    assert_eq!(
        jobs,
        [
            (
                "L1",
                StreamKind::Video,
                "ffprobe",
                "file:takes/take-0002/L1-screen-DP-4.mkv"
            ),
            (
                "A1",
                StreamKind::Audio,
                "ffprobe",
                "file:takes/take-0002/A1-mic.mka"
            )
        ]
    );
    assert_eq!(out.effects[0], Effect::WriteProject);
    let finalizing: Vec<String> = out
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::Progress { req, progress } => {
                assert_eq!(progress.phase, ProgressPhase::Finalize);
                Some(req.0.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(finalizing, ["t1", "s1"]);

    // Stale: not finalizing, or another take.
    assert_ignored(
        &recording(false),
        Event::CaptureStopped {
            take: take2(),
            streams: vec![],
            timed_out: false,
        },
    );
    assert_ignored(
        &state,
        Event::CaptureStopped {
            take: TakeId::numbered(9),
            streams: vec![],
            timed_out: false,
        },
    );
}

#[test]
fn capture_stopped_timeout_breaks_the_take() {
    let mut state = finalizing_from(&recording(true));
    state.runtime.host = HostKind::Headless;
    let out = event(
        &state,
        Event::CaptureStopped {
            take: take2(),
            streams: vec![],
            timed_out: true,
        },
    );
    assert_eq!(out.state.runtime.record, RecordPhase::Idle);
    let take = &out.state.project.takes[&take2()];
    assert_eq!(take.status, TakeStatus::Broken);
    assert_eq!(take.error.as_deref(), Some(EOS_TIMEOUT_REASON));
    assert_eq!(EOS_TIMEOUT_REASON, "the recording didn't stop within 10 s");
    for who in ["t1", "s1"] {
        let result = reply_to(&out, who).unwrap();
        assert_eq!(
            finalized_view(&result),
            TakeFinalizedView {
                take: take2(),
                status: TakeStatus::Broken,
                duration_frames: 0,
                placed: false,
                clips: vec![]
            }
        );
        assert_eq!(
            result.message,
            "Take 2 is broken: the recording didn't stop within 10 s"
        );
    }
    assert!(out.effects.contains(&Effect::SetLive(None)));
    assert!(
        !out.effects
            .iter()
            .any(|e| matches!(e, Effect::Probe { .. }))
    );
}

#[test]
fn finalize_appends_the_take_with_facts_first() {
    let state = probing(&finalizing_from(&recording(true)));
    let out = finalize(&state, 90);
    assert_eq!(out.state.runtime.record, RecordPhase::Idle);
    let take = &out.state.project.takes[&take2()];
    assert_eq!(take.status, TakeStatus::Ok);
    assert_eq!(take.duration_frames, 90);
    assert!(take.placed);
    assert!(take.replaced.is_empty());
    let l1 = take.stream("L1").unwrap();
    assert_eq!(
        (l1.width, l1.height, l1.frames, l1.fps),
        (Some(2560), Some(1440), Some(90), Some(Fps::new(30, 1)))
    );
    let a1 = take.stream("A1").unwrap();
    assert_eq!((a1.sample_rate, a1.channels), (Some(48_000), Some(1)));
    assert_eq!(a1.start_offset_ns, 3_000_000);

    // One clip [0, 90) at the end, one history entry without facts.
    let clips = &out.state.project.timeline.clips;
    assert_eq!(clips.len(), 3);
    let new = clips.last().unwrap();
    assert_eq!((new.take.clone(), new.in_, new.out), (take2(), 0, 90));
    let entries = &out.state.history.entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].label, "Record take 2");
    assert_eq!(
        entries[0].cmd,
        json!({"event": "takeFinalized", "take": "take-0002"})
    );
    let patch = serde_json::to_string(&entries[0].forward).unwrap();
    assert!(patch.contains("/timeline/clips"), "{patch}");
    for fact in ["streams", "status", "missing", "placed", "durationFrames"] {
        assert!(!patch.contains(fact), "{fact} in {patch}");
    }
    assert!(matches!(
        out.effects.as_slice(),
        [Effect::History(_), Effect::WriteProject, ..]
    ));

    // Both the stopper and the waiter get the finalized view.
    for who in ["t1", "s1"] {
        let result = reply_to(&out, who).unwrap();
        assert_eq!(result.seq, Some(1));
        assert_eq!(result.created, [new.id.to_string()]);
        assert_eq!(result.message, "Recorded take 2 (3.0 s)");
        assert_eq!(
            finalized_view(&result),
            TakeFinalizedView {
                take: take2(),
                status: TakeStatus::Ok,
                duration_frames: 90,
                placed: true,
                clips: vec![new.id.clone()]
            }
        );
    }
    assert!(toasts(&out).is_empty());
    // A late probe of the same take is stale.
    assert_ignored(
        &out.state,
        Event::TakeProbed {
            take: take2(),
            purpose: ProbePurpose::Finalize,
            results: vec![],
        },
    );
}

#[test]
fn finalize_too_short_cancelled_and_broken() {
    let base = probing(&finalizing_from(&recording(false)));

    let short = finalize(&base, 9);
    let take = &short.state.project.takes[&take2()];
    assert_eq!(
        (take.status, take.duration_frames, take.placed),
        (TakeStatus::TooShort, 9, false)
    );
    assert_eq!(
        short.state.project.timeline, base.project.timeline,
        "the bin, no clip"
    );
    assert!(short.state.history.entries.is_empty());
    assert_eq!(
        toasts(&short)[0].title,
        "That was shorter than Lord Farquaad. Take moved to the bin."
    );
    let view = finalized_view(&reply_to(&short, "t1").unwrap());
    assert_eq!(
        (view.status, view.placed, view.clips.len()),
        (TakeStatus::TooShort, false, 0)
    );

    let cancelled = cmd(&recording(false), Some("x1"), Command::RecordCancel).state;
    let cancelled = finalize(&probing(&cancelled), 90);
    let take = &cancelled.state.project.takes[&take2()];
    assert_eq!((take.status, take.placed), (TakeStatus::Ok, false));
    assert_eq!(cancelled.state.project.timeline, base.project.timeline);
    let result = reply_to(&cancelled, "x1").unwrap();
    assert_eq!(result.message, "Cancelled take 2 (3.0 s); it is in the bin");
    assert!(!finalized_view(&result).placed);

    // Nothing readable.
    let out = event(
        &base,
        Event::TakeProbed {
            take: take2(),
            purpose: ProbePurpose::Finalize,
            results: vec![
                ProbeResult {
                    id: "L1".into(),
                    file: "takes/take-0002/L1-screen-DP-4.mkv".into(),
                    info: Err("Invalid data found when processing input".into()),
                },
                ProbeResult {
                    id: "A1".into(),
                    file: "takes/take-0002/A1-mic.mka".into(),
                    info: Ok(video(10)),
                },
            ],
        },
    );
    let take = &out.state.project.takes[&take2()];
    assert_eq!(take.status, TakeStatus::Broken);
    assert!(take.streams.is_empty());
    assert!(take.scene.layers[0].missing && take.scene.audio[0].missing);
    assert_eq!(out.state.runtime.record, RecordPhase::Idle);
    assert_eq!(
        finalized_view(&reply_to(&out, "t1").unwrap()).status,
        TakeStatus::Broken
    );
}

#[test]
fn finalize_drops_unreadable_streams() {
    let state = probing(&finalizing_from(&recording(false)));
    let mut results = probed(&state, &take2(), 120);
    results[1].info = Err("moov atom not found".into());
    let out = event(
        &state,
        Event::TakeProbed {
            take: take2(),
            purpose: ProbePurpose::Finalize,
            results,
        },
    );
    let take = &out.state.project.takes[&take2()];
    assert_eq!(take.status, TakeStatus::Ok);
    assert_eq!(take.duration_frames, 120);
    let ids: Vec<&str> = take.streams.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["L1"], "a stream without media leaves the take");
    assert!(
        take.scene.audio[0].missing,
        "…and its track is missing media"
    );
    assert!(take.placed);
}

#[test]
fn record_over_replaces_the_run() {
    let state = cmd(&idle(), Some("s1"), over(&["c001"])).state;
    let state = event(
        &state,
        Event::CaptureStarted {
            take: take2(),
            t0_ns: 1,
            files: vec![],
            start_offset_ns: Default::default(),
        },
    )
    .state;
    let state = probing(&finalizing_from(&state));
    let out = finalize(&state, 60);
    let clips = &out.state.project.timeline.clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(
        (clips[0].take.clone(), clips[0].in_, clips[0].out),
        (take2(), 0, 60)
    );
    assert_eq!(clips[1].id.as_str(), "c002");
    let take = &out.state.project.takes[&take2()];
    assert_eq!(
        take.replaced,
        vec![idle().project.timeline.clips[0].clone()]
    );
    assert_eq!(
        out.state.ui.selection,
        vec![clips[0].id.clone()],
        "the new clip is selected"
    );
    assert!(out.effects.contains(&Effect::WriteUi));
    let result = reply_to(&out, "t1").unwrap();
    assert_eq!(result.removed, ["c001"]);
    assert_eq!(result.message, "Recorded take 2 (2.0 s) over c001");
    assert!(toasts(&out).is_empty());
}

#[test]
fn record_over_a_changed_target_appends() {
    let state = cmd(&idle(), Some("s1"), over(&["c001", "c002"])).state;
    let mut state = probing(&finalizing_from(
        &event(
            &state,
            Event::CaptureStarted {
                take: take2(),
                t0_ns: 1,
                files: vec![],
                start_offset_ns: Default::default(),
            },
        )
        .state,
    ));
    // c002 changed meanwhile (an external edit).
    state.project.timeline.clips[1].out = 250;
    let out = finalize(&state, 60);
    let ids = clip_ids(&out.state);
    assert_eq!(ids.len(), 3);
    assert_eq!(&ids[..2], ["c001", "c002"]);
    assert!(out.state.project.takes[&take2()].replaced.is_empty());
    assert_eq!(
        toasts(&out)[0].title,
        "record-over target changed; take appended at the end"
    );
    assert!(reply_to(&out, "t1").unwrap().removed.is_empty());
}

// ------------------------------------------------------------- discard

#[test]
fn discard_while_finalizing_runs_after_placement() {
    let state = finalizing_from(&recording(false));
    let queued = cmd(&state, Some("d1"), Command::TakeDiscardLast);
    assert_eq!(queued.reply, None);
    assert!(matches!(
        &queued.state.runtime.record,
        RecordPhase::Finalizing { discard: Some(DiscardRequest { req: Some(r) }), .. } if r.0 == "d1"
    ));
    let out = finalize(&probing(&queued.state), 90);
    let labels: Vec<&str> = out
        .state
        .history
        .entries
        .iter()
        .map(|e| e.label.as_str())
        .collect();
    assert_eq!(labels, ["Record take 2", "Discard take 2"], "both undoable");
    assert_eq!(out.state.project.timeline, idle().project.timeline);
    let discarded = reply_to(&out, "d1").unwrap();
    assert_eq!(discarded.seq, Some(2));
    assert_eq!(discarded.message, "Discarded take 2; it is in the bin");
    let stopped = reply_to(&out, "t1").unwrap();
    assert_eq!(stopped.seq, Some(1));
    assert!(finalized_view(&stopped).placed);

    // Not placed (cancelled): the discard just says so.
    let cancelled = cmd(&recording(false), Some("x1"), Command::RecordCancel).state;
    let queued = cmd(&cancelled, Some("d1"), Command::TakeDiscardLast).state;
    let out = finalize(&probing(&queued), 90);
    assert_eq!(
        reply_to(&out, "d1").unwrap().message,
        "take 2 was not placed"
    );
    assert!(out.state.history.entries.is_empty());
}

#[test]
fn discard_after_record_over_puts_the_clips_back() {
    let state = cmd(&idle(), Some("s1"), over(&["c001", "c002"])).state;
    let state = event(
        &state,
        Event::CaptureStarted {
            take: take2(),
            t0_ns: 1,
            files: vec![],
            start_offset_ns: Default::default(),
        },
    )
    .state;
    let placed = finalize(&probing(&finalizing_from(&state)), 45).state;
    assert_eq!(placed.project.timeline.clips.len(), 1);
    let out = cmd(&placed, Some("d1"), Command::TakeDiscardLast);
    let Some(Ok(result)) = &out.reply else {
        panic!("{:?}", out.reply)
    };
    assert_eq!(out.state.project.timeline, idle().project.timeline);
    assert_eq!(result.created, ["c001", "c002"]);
    assert_eq!(result.seq, Some(2));
    // `d` again: take 2 is the newest placed take and already in the bin.
    let err = fail(&out.state, None, Command::TakeDiscardLast);
    assert_eq!(
        (err.code, err.message.as_str()),
        (ErrorCode::NotFound, "Take 2 is already in the bin")
    );
}

#[test]
fn discard_skips_takes_that_were_never_placed() {
    // Take 2 placed, take 3 too short: d discards take 2.
    let placed = finalize(&probing(&finalizing_from(&recording(false))), 90).state;
    let mut state = cmd(&placed, Some("s2"), append()).state;
    let take3 = TakeId::numbered(3);
    state = event(
        &state,
        Event::CaptureStarted {
            take: take3.clone(),
            t0_ns: 1,
            files: vec![],
            start_offset_ns: Default::default(),
        },
    )
    .state;
    state = cmd(&state, Some("t2"), Command::RecordStop).state;
    let streams: Vec<RecordedStream> = vec![];
    state = event(
        &state,
        Event::CaptureStopped {
            take: take3.clone(),
            streams,
            timed_out: false,
        },
    )
    .state;
    let results = probed(&state, &take3, 4);
    state = event(
        &state,
        Event::TakeProbed {
            take: take3.clone(),
            purpose: ProbePurpose::Finalize,
            results,
        },
    )
    .state;
    assert_eq!(state.project.takes[&take3].status, TakeStatus::TooShort);
    let out = cmd(&state, None, Command::TakeDiscardLast);
    assert_eq!(out.state.project.timeline, idle().project.timeline);
    assert!(
        out.reply
            .unwrap()
            .unwrap()
            .message
            .starts_with("Discarded take 2")
    );
}

// ---------------------------------------------------------- EngineWedged

#[test]
fn engine_wedged_in_every_phase() {
    let wedged = |s: &State| {
        event(
            s,
            Event::EngineWedged {
                message: "teardown timed out".into(),
            },
        )
    };
    // Idle: people are told, and new takes are refused until a restart.
    let out = wedged(&idle());
    let mut expected = idle();
    expected.runtime.engine_stuck = Some("teardown timed out".into());
    assert_eq!(out.state, expected);
    assert_eq!(toasts(&out)[0].title, "The capture engine is stuck");

    // Countdown: cancelled, the start request fails.
    let counting = cmd(
        &idle(),
        Some("s1"),
        start(RecordModeArg::Append, Some(3), false),
    )
    .state;
    let RecordPhase::Countdown { timer, .. } = counting.runtime.record else {
        panic!()
    };
    let out = wedged(&counting);
    assert_eq!(out.state.runtime.record, RecordPhase::Idle);
    assert!(out.effects.contains(&Effect::CancelTimer { timer }));
    let err = reply_to(&out, "s1").unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (ErrorCode::CaptureFailed, ENGINE_STUCK)
    );

    // Capturing: the take is broken, requesters learn why.
    for (state, who, finalizing) in [
        (starting(false), "s1", false),
        (recording(true), "s1", false),
        (finalizing_from(&recording(false)), "t1", true),
    ] {
        let out = wedged(&state);
        assert_eq!(out.state.runtime.record, RecordPhase::Idle);
        let take = &out.state.project.takes[&take2()];
        assert_eq!(take.status, TakeStatus::Broken);
        assert!(take.error.as_deref().unwrap().starts_with(ENGINE_STUCK));
        let reply = reply_to(&out, who);
        if finalizing {
            assert_eq!(finalized_view(&reply.unwrap()).status, TakeStatus::Broken);
        } else {
            assert_eq!(reply.unwrap_err().code, ErrorCode::CaptureFailed);
        }
        assert!(
            !out.effects
                .iter()
                .any(|e| matches!(e, Effect::SetLive(_) | Effect::StopCapture { .. })),
            "nothing more is asked of a stuck engine"
        );
    }
}

// ------------------------------------------------------------- recovery

#[test]
fn recovery_probes_leftover_takes() {
    let mut state = idle();
    let take3 = TakeId::numbered(3);
    let mut leftover = state.project.takes[&TakeId::numbered(1)].clone();
    leftover.status = TakeStatus::Recording;
    leftover.duration_frames = 0;
    for s in &mut leftover.streams {
        s.file = s.file.replace("take-0001", "take-0003");
        s.width = None;
        s.height = None;
        s.frames = None;
        s.fps = None;
        s.sample_rate = None;
        s.channels = None;
        s.duration_ns = None;
    }
    state.project.takes.insert(take3.clone(), leftover);
    state.project.next_take_number = 4;

    let probes = recovery_probes(&state);
    let [
        Effect::Probe {
            take,
            purpose: ProbePurpose::Recover,
            jobs,
        },
    ] = probes.as_slice()
    else {
        panic!("{probes:?}")
    };
    assert_eq!(*take, take3);
    assert_eq!(jobs.len(), 2);

    let recover = |s: &State, frames: u64| {
        let results = probed(s, &take3, frames);
        event(
            s,
            Event::TakeProbed {
                take: take3.clone(),
                purpose: ProbePurpose::Recover,
                results,
            },
        )
    };
    let out = recover(&state, 300);
    let t = &out.state.project.takes[&take3];
    assert_eq!(
        (t.status, t.duration_frames, t.placed, t.recovered),
        (TakeStatus::Ok, 300, false, true)
    );
    assert!(
        !out.state.project.takes[&TakeId::numbered(1)].recovered,
        "only the recovered take"
    );
    let saved: serde_json::Value = serde_json::to_value(&out.state.project).unwrap();
    assert_eq!(saved["takes"]["take-0003"]["recovered"], true);
    assert!(
        saved["takes"]["take-0001"].get("recovered").is_none(),
        "written only when set"
    );
    assert_eq!(
        out.state.project.timeline, state.project.timeline,
        "the bin"
    );
    assert!(out.state.history.entries.is_empty());
    assert_eq!(toasts(&out)[0].title, "Recovered take 3 from the bog.");
    assert!(out.effects.contains(&Effect::WriteProject));
    // Once recovered, a second answer is stale.
    assert_ignored(
        &out.state,
        Event::TakeProbed {
            take: take3.clone(),
            purpose: ProbePurpose::Recover,
            results: vec![],
        },
    );
    assert!(recovery_probes(&out.state).is_empty());

    let short = recover(&state, 3);
    assert_eq!(
        short.state.project.takes[&take3].status,
        TakeStatus::TooShort
    );
    assert!(short.state.project.takes[&take3].recovered);

    let unreadable = event(
        &state,
        Event::TakeProbed {
            take: take3.clone(),
            purpose: ProbePurpose::Recover,
            results: vec![],
        },
    );
    let t = &unreadable.state.project.takes[&take3];
    assert_eq!(t.status, TakeStatus::Broken);
    assert!(!t.recovered, "a broken take wasn't recovered");
    assert!(t.streams.is_empty());
    assert_eq!(toasts(&unreadable)[0].title, "Take 3 couldn't be recovered");

    // The take being captured is never "recovered".
    let busy = recording(false);
    assert!(recovery_probes(&busy).is_empty());
    assert_ignored(
        &busy,
        Event::TakeProbed {
            take: take2(),
            purpose: ProbePurpose::Recover,
            results: vec![],
        },
    );
}

/// Take 3 left `recording` by a crash, its streams not probed yet.
fn crashed() -> (State, TakeId) {
    let mut state = idle();
    let take3 = TakeId::numbered(3);
    let mut leftover = state.project.takes[&TakeId::numbered(1)].clone();
    leftover.status = TakeStatus::Recording;
    leftover.duration_frames = 0;
    for s in &mut leftover.streams {
        s.file = s.file.replace("take-0001", "take-0003");
        s.width = None;
        s.height = None;
        s.frames = None;
        s.fps = None;
        s.sample_rate = None;
        s.channels = None;
        s.duration_ns = None;
    }
    state.project.takes.insert(take3.clone(), leftover);
    state.project.next_take_number = 4;
    (state, take3)
}

/// A host that started recovery is busy until every probe came back, so an
/// in-process command doesn't exit before the take is recovered (A§8.6).
#[test]
fn recovery_in_flight_keeps_the_host_busy_until_its_probe_returns() {
    let (state, take3) = crashed();
    assert!(!state.runtime.is_busy());
    let started = start_recovery(&state);
    assert_eq!(
        started.state.runtime.recovering,
        [take3.clone()].into_iter().collect()
    );
    assert!(started.state.runtime.is_busy());
    assert_eq!(started.state.project, state.project, "nothing to save yet");
    assert!(!started.effects.contains(&Effect::WriteProject));
    let probes: Vec<_> = started
        .effects
        .iter()
        .filter(|e| matches!(e, Effect::Probe { .. }))
        .collect();
    assert!(
        matches!(probes.as_slice(), [Effect::Probe { take, purpose: ProbePurpose::Recover, .. }] if *take == take3),
        "{:?}",
        started.effects
    );
    // Never probed twice while one is out.
    assert!(recovery_probes(&started.state).is_empty());
    assert!(start_recovery(&started.state).effects.is_empty());

    // Whatever comes back ends the recovery: readable…
    let back = |results: Vec<ProbeResult>| {
        event(
            &started.state,
            Event::TakeProbed {
                take: take3.clone(),
                purpose: ProbePurpose::Recover,
                results,
            },
        )
    };
    let ok = back(probed(&started.state, &take3, 300));
    assert!(ok.state.runtime.recovering.is_empty());
    assert!(!ok.state.runtime.is_busy());
    assert_eq!(ok.state.project.takes[&take3].status, TakeStatus::Ok);
    // …unreadable…
    let broken = back(vec![]);
    assert!(broken.state.runtime.recovering.is_empty());
    assert_eq!(
        broken.state.project.takes[&take3].status,
        TakeStatus::Broken
    );
    // …a tool failure…
    let missing = back(
        started.state.project.takes[&take3]
            .streams
            .iter()
            .map(|s| ProbeResult {
                id: s.id.clone(),
                file: s.file.clone(),
                info: Err(ProbeError::missing_tool("ffprobe wasn't found")),
            })
            .collect(),
    );
    assert!(missing.state.runtime.recovering.is_empty());
    // …or a stale answer (the take was recovered meanwhile).
    let mut recovered = started.state.clone();
    recovered.project.takes.get_mut(&take3).unwrap().status = TakeStatus::Ok;
    let stale = event(
        &recovered,
        Event::TakeProbed {
            take: take3.clone(),
            purpose: ProbePurpose::Recover,
            results: vec![],
        },
    );
    assert!(stale.state.runtime.recovering.is_empty());
    assert_eq!(stale.state.project, recovered.project);
}

fn tool_failed(state: &State, take: &TakeId, error: &ProbeError) -> Vec<ProbeResult> {
    state.project.takes[take]
        .streams
        .iter()
        .map(|s| ProbeResult {
            id: s.id.clone(),
            file: s.file.clone(),
            info: Err(error.clone()),
        })
        .collect()
}

/// A probe that failed on its own (ffprobe missing or hung) says nothing
/// about the files: the take keeps its facts and stays `recording` for the
/// next host start, and stop requests hear `missing_dependency` (exit 5).
#[test]
fn a_probe_tool_failure_never_breaks_a_take() {
    let waiting = probing(&finalizing_from(&recording(false)));
    let before = waiting.project.takes[&take2()].clone();
    let missing = ProbeError::missing_tool("ffprobe wasn't found (is it installed and on PATH?)");
    let out = event(
        &waiting,
        Event::TakeProbed {
            take: take2(),
            purpose: ProbePurpose::Finalize,
            results: tool_failed(&waiting, &take2(), &missing),
        },
    );
    let t = &out.state.project.takes[&take2()];
    assert_eq!(*t, before, "no fact changed");
    assert_eq!(t.status, TakeStatus::Recording);
    assert!(t.scene.layers.iter().all(|l| !l.missing));
    assert!(t.scene.audio.iter().all(|a| !a.missing));
    assert_eq!(out.state.runtime.record, RecordPhase::Idle);
    assert_eq!(
        out.state.project.timeline, waiting.project.timeline,
        "nothing placed"
    );
    let err = reply_to(&out, "t1").unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingDependency);
    assert_eq!(err.exit_code(), 5);
    assert!(err.message.contains("ffprobe wasn't found"), "{err:?}");
    assert!(
        err.hint.as_deref().unwrap().contains("qrec doctor"),
        "{err:?}"
    );
    assert_eq!(toasts(&out)[0].title, "Take 2 wasn't finalized");
    // The next host start recovers it.
    assert_eq!(recovery_probes(&out.state).len(), 1);

    // A hung ffprobe (timed out): the same, as an io error.
    let hung = ProbeError::tool("ffprobe timed out after 20 s");
    let mut results = probed(&waiting, &take2(), 90);
    results[0].info = Err(hung);
    let out = event(
        &waiting,
        Event::TakeProbed {
            take: take2(),
            purpose: ProbePurpose::Finalize,
            results,
        },
    );
    assert_eq!(out.state.project.takes[&take2()], before);
    assert_eq!(reply_to(&out, "t1").unwrap_err().code, ErrorCode::Io);

    // A file ffprobe could read but not use is a fact: broken, with why.
    let mut results = probed(&waiting, &take2(), 90);
    for r in &mut results {
        r.info = Err("Invalid data found when processing input".into());
    }
    let out = event(
        &waiting,
        Event::TakeProbed {
            take: take2(),
            purpose: ProbePurpose::Finalize,
            results,
        },
    );
    let t = &out.state.project.takes[&take2()];
    assert_eq!(t.status, TakeStatus::Broken);
    assert!(
        t.error
            .as_deref()
            .unwrap()
            .contains("L1: Invalid data found when processing input"),
        "{:?}",
        t.error
    );

    // Recovery postpones the same way.
    let (state, take3) = crashed();
    let recovering = start_recovery(&state).state;
    let out = event(
        &recovering,
        Event::TakeProbed {
            take: take3.clone(),
            purpose: ProbePurpose::Recover,
            results: tool_failed(&recovering, &take3, &missing),
        },
    );
    assert_eq!(out.state.project, state.project, "no fact changed");
    assert!(!out.effects.contains(&Effect::WriteProject));
    assert!(out.state.runtime.recovering.is_empty());
    assert_eq!(toasts(&out)[0].title, "Take 3 isn't recovered yet");
    assert_eq!(
        recovery_probes(&out.state).len(),
        1,
        "tried again next time"
    );
}

// ------------------------------------------------------- stale events

#[test]
fn events_for_other_phases_are_ignored() {
    let phases = [
        ("idle", idle()),
        ("starting", starting(false)),
        ("recording", recording(false)),
        ("finalizing", finalizing_from(&recording(false))),
    ];
    for (name, state) in &phases {
        let stale_take = TakeId::numbered(42);
        let mut events = vec![
            Event::TimerFired {
                timer: TimerId(4242),
            },
            Event::RecordClock {
                take: stale_take.clone(),
                elapsed_ns: 1,
            },
            Event::CaptureStarted {
                take: stale_take.clone(),
                t0_ns: 0,
                files: vec![],
                start_offset_ns: Default::default(),
            },
            Event::CaptureStopped {
                take: stale_take.clone(),
                streams: vec![],
                timed_out: false,
            },
            Event::TakeProbed {
                take: stale_take.clone(),
                purpose: ProbePurpose::Finalize,
                results: vec![],
            },
            Event::TakeProbed {
                take: stale_take.clone(),
                purpose: ProbePurpose::Recover,
                results: vec![],
            },
            Event::StreamLost {
                take: stale_take.clone(),
                stream: "L1".into(),
                message: "x".into(),
            },
            Event::CaptureFailed {
                take: Some(stale_take.clone()),
                source: None,
                message: "x".into(),
                missing: None,
            },
        ];
        if *name != "finalizing" {
            events.push(Event::TakeProbed {
                take: take2(),
                purpose: ProbePurpose::Finalize,
                results: vec![],
            });
            events.push(Event::CaptureStopped {
                take: take2(),
                streams: vec![],
                timed_out: false,
            });
        }
        for ev in events {
            assert_ignored(state, ev);
        }
    }
}

// -------------------------------------------------------- end to end

/// A fake shell: capture starts, stops and probes succeed (90 frames).
fn shell(state: &State, effect: &Effect) -> Vec<Event> {
    match effect {
        Effect::StartCapture { plan } => vec![Event::CaptureStarted {
            take: plan.take.clone(),
            t0_ns: 1_000,
            files: plan_files(plan),
            start_offset_ns: Default::default(),
        }],
        Effect::StopCapture { take, .. } => vec![Event::CaptureStopped {
            take: take.clone(),
            streams: state.project.takes[take]
                .streams
                .iter()
                .map(|s| RecordedStream {
                    id: s.id.clone(),
                    file: s.file.clone(),
                    start_offset_ns: 0,
                    eos: true,
                })
                .collect(),
            timed_out: false,
        }],
        Effect::Probe {
            take,
            purpose,
            jobs,
        } => vec![Event::TakeProbed {
            take: take.clone(),
            purpose: *purpose,
            results: jobs
                .iter()
                .map(|j| ProbeResult {
                    id: j.id.clone(),
                    file: j.file.clone(),
                    info: Ok(match j.kind {
                        StreamKind::Video => video(90),
                        StreamKind::Audio => audio(3_000_000_000),
                    }),
                })
                .collect(),
        }],
        _ => vec![],
    }
}

#[test]
fn record_loop_end_to_end() {
    let state = idle();
    let ctx = ctx_for(Some("r1"));
    // r, r: append take 2. o over c001, r: take 3 replaces c001. d: undone by discard.
    let after = testkit::drive(
        &state,
        &ctx,
        &[
            Command::RecordToggle { wait: false },
            Command::RecordToggle { wait: false },
            over(&["c001"]),
            Command::RecordStop,
        ],
        shell,
    )
    .unwrap();
    assert_eq!(after.runtime.record, RecordPhase::Idle);
    let takes: Vec<(u32, TakeStatus, bool)> = after
        .project
        .takes
        .iter()
        .map(|(id, t)| (id.number(), t.status, t.placed))
        .collect();
    assert_eq!(
        takes,
        [
            (1, TakeStatus::Ok, false),
            (2, TakeStatus::Ok, true),
            (3, TakeStatus::Ok, true)
        ]
    );
    let clips: Vec<(u32, u64, u64)> = after
        .project
        .timeline
        .clips
        .iter()
        .map(|c| (c.take.number(), c.in_, c.out))
        .collect();
    assert_eq!(clips, [(3, 0, 90), (1, 100, 300), (2, 0, 90)]);
    let labels: Vec<&str> = after
        .history
        .entries
        .iter()
        .map(|e| e.label.as_str())
        .collect();
    assert_eq!(labels, ["Record take 2", "Record take 3"]);

    let discarded = testkit::drive(&after, &ctx, &[Command::TakeDiscardLast], shell).unwrap();
    let clips: Vec<(u32, u64, u64)> = discarded
        .project
        .timeline
        .clips
        .iter()
        .map(|c| (c.take.number(), c.in_, c.out))
        .collect();
    assert_eq!(
        clips,
        [(1, 0, 100), (1, 100, 300), (2, 0, 90)],
        "c001 is back"
    );
    let restored: &Clip = &discarded.project.timeline.clips[0];
    assert_eq!(restored.id.as_str(), "c001");
}

#[test]
fn start_offsets_are_facts_from_the_start_and_survive_a_crash() {
    let state = starting(false);
    let files = files(&state);
    let out = event(
        &state,
        Event::CaptureStarted {
            take: take2(),
            t0_ns: 77,
            files,
            start_offset_ns: BTreeMap::from([
                ("L1".to_owned(), 15_261_442),
                ("A1".to_owned(), 3_906_126),
                ("L9".to_owned(), 1),
            ]),
        },
    );
    let offsets = |s: &State| -> Vec<(String, i64)> {
        s.project.takes[&take2()]
            .streams
            .iter()
            .map(|st| (st.id.clone(), st.start_offset_ns))
            .collect()
    };
    let measured = [("L1".to_owned(), 15_261_442), ("A1".to_owned(), 3_906_126)];
    assert_eq!(offsets(&out.state), measured);
    assert!(
        out.effects.contains(&Effect::WriteProject),
        "persisted at once: {:?}",
        out.effects
    );
    assert!(
        out.effects.iter().any(|e| matches!(
            e,
            Effect::Log { level: LogLevel::Warn, message } if message.contains("unknown stream L9")
        )),
        "{:?}",
        out.effects
    );

    // The host dies while recording. The next host opens what was written
    // and recovers the take: the measured offsets stay.
    let reopened = testkit::state(out.state.project.clone());
    assert_eq!(recovery_probes(&reopened).len(), 1);
    let recovered = event(
        &reopened,
        Event::TakeProbed {
            take: take2(),
            purpose: ProbePurpose::Recover,
            results: probed(&reopened, &take2(), 90),
        },
    );
    assert_eq!(
        recovered.state.project.takes[&take2()].status,
        TakeStatus::Ok
    );
    assert_eq!(offsets(&recovered.state), measured);
}

/// What ffprobe 8.1.2 (`probe_argv`) prints for the mic of an audio-only
/// take whose host was SIGKILLed after ~3.4 s: matroskamux never wrote a
/// duration (no stream, tag or format duration), 36 FLAC frames.
const CRASHED_MIC: &str = r#"{"programs":[],"stream_groups":[],"streams":[{"codec_name":"flac",
    "sample_rate":"48000","channels":1,"r_frame_rate":"0/0","nb_read_packets":"36","tags":{}}],
    "format":{"start_time":"0.000000"}}"#;

/// An audio-only project whose take 1 was recording when its host died
/// (its offset was measured when the capture started).
fn crashed_audio_only() -> State {
    let mut state = testkit::state(
        ProjectBuilder::new()
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 0)
            .build(),
    );
    let take = state.project.takes.get_mut(&TakeId::numbered(1)).unwrap();
    take.status = TakeStatus::Recording;
    for s in &mut take.streams {
        s.sample_rate = None;
        s.channels = None;
        s.duration_ns = None;
        s.start_offset_ns = 4_000_000;
    }
    state
}

/// The `TakeProbed{Recover}` of `state`'s leftover take, every file
/// answering `json`.
fn recovered_from(state: &State, json: &str) -> Outcome {
    let probes = recovery_probes(state);
    let [Effect::Probe { take, jobs, .. }] = probes.as_slice() else {
        panic!("{probes:?}")
    };
    let results = jobs
        .iter()
        .map(|j| ProbeResult {
            id: j.id.clone(),
            file: j.file.clone(),
            info: parse_ffprobe(json, j.kind).map_err(ProbeError::media),
        })
        .collect();
    event(
        state,
        Event::TakeProbed {
            take: take.clone(),
            purpose: ProbePurpose::Recover,
            results,
        },
    )
}

#[test]
fn an_audio_only_take_cut_short_by_a_crash_is_recovered_whole() {
    let state = crashed_audio_only();
    let take1 = TakeId::numbered(1);
    let out = recovered_from(&state, CRASHED_MIC);
    let t = &out.state.project.takes[&take1];
    // 36 FLAC frames × 4608 samples = 3.456 s, after the 4 ms offset: 103
    // frames at 30 fps, not a 0-frame tooShort.
    assert_eq!(t.streams[0].duration_ns, Some(3_456_000_000));
    assert_eq!(t.streams[0].start_offset_ns, 4_000_000);
    assert_eq!(
        (t.status, t.duration_frames, t.error.as_deref()),
        (TakeStatus::Ok, 103, None)
    );
    assert_eq!(toasts(&out)[0].title, "Recovered take 1 from the bog.");
    // A usable take: it can go on the timeline.
    let added = cmd(
        &out.state,
        Some("a1"),
        Command::ClipAdd {
            take: take1.clone(),
            at: InsertAt::End,
        },
    );
    let clips = &added.state.project.timeline.clips;
    assert_eq!(
        (
            clips.len(),
            clips[0].take.clone(),
            clips[0].in_,
            clips[0].out
        ),
        (1, take1, 0, 103)
    );
}

#[test]
fn a_take_whose_length_is_unknown_is_broken_not_too_short() {
    // An audio stream that says nothing about its length (no duration, no
    // FLAC packets to count) is readable but can't be placed: the take is
    // broken with a reason, never a silent 0-frame tooShort.
    let unknown =
        r#"{"streams":[{"codec_name":"opus","sample_rate":"48000","channels":1}],"format":{}}"#;
    let out = recovered_from(&crashed_audio_only(), unknown);
    let t = &out.state.project.takes[&TakeId::numbered(1)];
    assert_eq!((t.status, t.duration_frames), (TakeStatus::Broken, 0));
    assert_eq!(
        t.error.as_deref(),
        Some("recovery failed: the recorded files don't tell how long the take is")
    );
    assert_eq!(toasts(&out)[0].title, "Take 1 couldn't be recovered");

    // The same when finalizing a take that stopped normally.
    let project = ProjectBuilder::new()
        .mic("A1", "alsa_input.mic")
        .take("take-0001", 300)
        .build();
    let state = testkit::state(project);
    let started = cmd(&state, Some("s1"), append()).state;
    let files = files(&started);
    let recording = event(
        &started,
        Event::CaptureStarted {
            take: take2(),
            t0_ns: 5,
            files,
            start_offset_ns: Default::default(),
        },
    )
    .state;
    let stopped = probing(&finalizing_from(&recording));
    let results = stopped.project.takes[&take2()]
        .streams
        .iter()
        .map(|s| ProbeResult {
            id: s.id.clone(),
            file: s.file.clone(),
            info: parse_ffprobe(unknown, s.kind).map_err(ProbeError::media),
        })
        .collect();
    let out = event(
        &stopped,
        Event::TakeProbed {
            take: take2(),
            purpose: ProbePurpose::Finalize,
            results,
        },
    );
    let t = &out.state.project.takes[&take2()];
    assert_eq!(t.status, TakeStatus::Broken);
    assert_eq!(
        t.error.as_deref(),
        Some("the recorded files don't tell how long the take is")
    );
    let view = finalized_view(&reply_to(&out, "t1").unwrap());
    assert_eq!(view.status, TakeStatus::Broken);
    assert!(out.state.runtime.record.is_idle());
}
