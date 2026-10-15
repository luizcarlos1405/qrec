//! H-7 (docs/GUI.md): a GUI only logs an error reply that comes late
//! (`Effect::Reply` from `on_event`), because the core tells people itself.
//! This holds the core to that: an `on_event` outcome that replies `Err` to
//! a requester also has an `Effect::Notify`, whatever arrives in whatever
//! order.
//!
//! Requests are made with `apply`, each with its own requester; a fake shell
//! answers every effect late and out of order, sometimes as a failure
//! (capture, probe, analysis, export, frame and purge failures, timeouts,
//! outputs that appeared); requesters go away, the engine wedges, the
//! pipeline fails, `qrec.json` changes on disk and ffmpeg disappears in
//! between. A table of every known late-error path checks the same with
//! named cases, so the property doesn't rely on the generator alone.

use proptest::prelude::*;
use proptest::sample::Index;
use qrec_core::capture_plan::plan_files;
use qrec_core::ctx::{Ctx, FileFact};
use qrec_core::ids::{ClipId, JobId, ReqId, TakeId, TimerId};
use qrec_core::model::{Fps, StreamKind};
use qrec_core::protocol::command::{
    AutocutOverrides, ClipScope, Command, ExportFormat, Quality, RecordModeArg,
};
use qrec_core::protocol::effect::Effect;
use qrec_core::protocol::event::{
    Event, Interval, MediaInfo, ProbeError, ProbeResult, RecordedStream,
};
use qrec_core::state::{HostKind, JobKind, RecordPhase, State};
use qrec_core::testkit;
use qrec_core::time::{TimeArg, frames_to_ns};
use qrec_core::{Outcome, apply, facts_needed, on_event};

const NOW: &str = "2026-10-14T15:30:12Z";

// ------------------------------------------------------------- the check

/// The late errors an outcome sends, by requester.
fn late_errors(out: &Outcome) -> Vec<&ReqId> {
    out.effects
        .iter()
        .filter_map(|e| match e {
            Effect::Reply { req, reply: Err(_) } => Some(req),
            _ => None,
        })
        .collect()
}

fn notifies(out: &Outcome) -> bool {
    out.effects.iter().any(|e| matches!(e, Effect::Notify(_)))
}

/// H-7 for one `on_event` outcome.
fn heard(out: &Outcome, what: &str) -> Result<(), TestCaseError> {
    let late = late_errors(out);
    prop_assert!(
        late.is_empty() || notifies(out),
        "{}: a late error to {:?} without a toast: {:?}",
        what,
        late,
        out.effects
    );
    Ok(())
}

// ------------------------------------------------------------ the shell

fn media(kind: StreamKind, frames: u64, fps: Fps) -> MediaInfo {
    MediaInfo {
        kind,
        frames: (kind == StreamKind::Video).then_some(frames),
        fps: (kind == StreamKind::Video).then_some(fps),
        width: (kind == StreamKind::Video).then_some(1920),
        height: (kind == StreamKind::Video).then_some(1080),
        sample_rate: (kind == StreamKind::Audio).then_some(48_000),
        channels: (kind == StreamKind::Audio).then_some(2),
        duration_ns: Some(frames_to_ns(frames, fps)),
        start_time_ns: 0,
        color_range: None,
        color_space: None,
    }
}

fn recorded(state: &State, take: &TakeId) -> Vec<RecordedStream> {
    state
        .project
        .take(take)
        .map(|t| {
            t.streams
                .iter()
                .map(|s| RecordedStream {
                    id: s.id.clone(),
                    file: s.file.clone(),
                    start_offset_ns: 0,
                    eos: true,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The shell's answers to `effect`, all successful.
fn answers(state: &State, effect: &Effect) -> Vec<Event> {
    let fps = state.project.canvas.fps;
    match effect {
        Effect::StartTimer { timer, .. } => vec![Event::TimerFired { timer: *timer }],
        Effect::StartCapture { plan } => vec![Event::CaptureStarted {
            take: plan.take.clone(),
            t0_ns: 0,
            files: plan_files(plan),
            start_offset_ns: Default::default(),
        }],
        Effect::StopCapture { take, .. } => vec![Event::CaptureStopped {
            take: take.clone(),
            streams: recorded(state, take),
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
                    info: Ok(media(j.kind, 90, fps)),
                })
                .collect(),
        }],
        Effect::DetectSilence { job, key, .. } => vec![Event::SilenceDetected {
            job: *job,
            key: key.clone(),
            silences: vec![Interval {
                start_ns: 1_000_000_000,
                end_ns: 2_000_000_000,
            }],
        }],
        Effect::RenderExport { job, plan } => {
            let mut events: Vec<Event> = plan
                .segments
                .iter()
                .enumerate()
                .map(|(segment, s)| Event::ExportProgress {
                    job: *job,
                    segment,
                    frames_done: s.frames,
                })
                .collect();
            events.push(Event::ExportStitchProgress {
                job: *job,
                out_time_ns: u64::MAX,
            });
            events.push(Event::ExportFinished {
                job: *job,
                path: plan.out.clone(),
                bytes: 88_400_000,
            });
            events
        }
        Effect::SaveFrame { job, plan } => vec![Event::FrameSaved {
            job: *job,
            path: plan.out.clone(),
        }],
        Effect::DeleteTakeDirs { job, takes } => vec![Event::TakeDirsDeleted {
            job: *job,
            deleted: takes.iter().map(|t| format!("takes/{t}")).collect(),
            errors: Vec::new(),
        }],
        Effect::CancelJob { job } => match state.runtime.jobs.get(job).map(|j| &j.kind) {
            Some(JobKind::Export { .. }) => vec![Event::ExportCancelled { job: *job }],
            Some(JobKind::FrameSave { .. }) => vec![Event::FrameFailed {
                job: *job,
                message: "cancelled".into(),
                log: String::new(),
            }],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// `ev` turned into one of its failures, depending on `variant`.
fn failed(state: &State, ev: Event, variant: u8) -> Event {
    let fps = state.project.canvas.fps;
    match ev {
        Event::CaptureStarted { take, .. } => Event::CaptureFailed {
            take: Some(take),
            source: Some("L1".into()),
            message: "access denied".into(),
            missing: (variant % 2 == 0).then(|| "pipewiresrc".into()),
        },
        Event::CaptureStopped { take, streams, .. } => Event::CaptureStopped {
            take,
            streams,
            timed_out: true,
        },
        Event::TakeProbed {
            take,
            purpose,
            results,
        } => {
            let results = results
                .into_iter()
                .map(|mut r| {
                    r.info = match variant % 4 {
                        0 => Err(ProbeError::media(
                            "Invalid data found when processing input",
                        )),
                        1 => Err(ProbeError::tool("ffprobe timed out")),
                        2 => Err(ProbeError::missing_tool("ffprobe not found")),
                        _ => r.info.map(|info| media(info.kind, 4, fps)),
                    };
                    r
                })
                .collect();
            Event::TakeProbed {
                take,
                purpose,
                results,
            }
        }
        Event::SilenceDetected { job, key, .. } => Event::SilenceFailed {
            job,
            key,
            message: "ffmpeg exited with status 1".into(),
        },
        Event::ExportFinished { job, path, bytes } => match variant % 3 {
            0 => Event::ExportFailed {
                job,
                message: "ffmpeg exited with status 1".into(),
                stderr_tail: vec!["No space left on device".into()],
                log: if variant % 2 == 0 {
                    ".qrec/logs/20261014-153050-1-export.log".into()
                } else {
                    String::new()
                },
            },
            1 => Event::ExportCancelled { job },
            _ => Event::OutputAppeared {
                job,
                out: path.clone(),
                bytes,
                kept: format!("{path}.kept"),
            },
        },
        Event::FrameSaved { job, path } => match variant % 2 {
            0 => Event::FrameFailed {
                job,
                message: "ffmpeg exited with status 1".into(),
                log: String::new(),
            },
            _ => Event::OutputAppeared {
                job,
                out: path.clone(),
                bytes: 10,
                kept: format!("{path}.kept"),
            },
        },
        Event::TakeDirsDeleted { job, deleted, .. } => match variant % 2 {
            0 => Event::TakeDirsDeleted {
                job,
                deleted,
                errors: vec!["EACCES: takes/take-0002/L1-screen-DP-4.mkv".into()],
            },
            _ => Event::PurgeFailed {
                job,
                message: "write .qrec/history.base.json: No space left on device".into(),
            },
        },
        other => other,
    }
}

// ----------------------------------------------------------- the harness

#[derive(Debug, Clone)]
enum Step {
    Start {
        countdown: u32,
        wait: bool,
        over: Option<Index>,
    },
    Stop,
    Cancel,
    Toggle {
        wait: bool,
    },
    Discard,
    Export,
    Frame {
        at: Index,
    },
    Autocut {
        clip: Option<Index>,
        dry_run: bool,
    },
    Purge {
        pick: Index,
    },
    /// Delivers one pending answer, failing when `fail`.
    Deliver {
        pick: Index,
        fail: bool,
        variant: u8,
    },
    Gone {
        pick: Index,
    },
    Wedged,
    PipelineFailed,
    Lost {
        pick: Index,
    },
    Clock {
        ms: u64,
    },
    Disk {
        valid: bool,
    },
    NoFfmpeg,
}

fn arb_step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (0u32..3, any::<bool>(), proptest::option::of(any::<Index>()))
            .prop_map(|(countdown, wait, over)| Step::Start { countdown, wait, over }),
        2 => Just(Step::Stop),
        1 => Just(Step::Cancel),
        1 => any::<bool>().prop_map(|wait| Step::Toggle { wait }),
        1 => Just(Step::Discard),
        2 => Just(Step::Export),
        1 => any::<Index>().prop_map(|at| Step::Frame { at }),
        2 => (proptest::option::of(any::<Index>()), any::<bool>())
            .prop_map(|(clip, dry_run)| Step::Autocut { clip, dry_run }),
        1 => any::<Index>().prop_map(|pick| Step::Purge { pick }),
        14 => (any::<Index>(), prop::bool::weighted(0.35), any::<u8>())
            .prop_map(|(pick, fail, variant)| Step::Deliver { pick, fail, variant }),
        1 => any::<Index>().prop_map(|pick| Step::Gone { pick }),
        1 => any::<u8>().prop_map(|k| match k % 4 {
            0 => Step::Wedged,
            1 => Step::PipelineFailed,
            2 => Step::NoFfmpeg,
            _ => Step::Disk { valid: k % 8 < 4 },
        }),
        1 => any::<Index>().prop_map(|pick| Step::Lost { pick }),
        1 => (0u64..3_000).prop_map(|ms| Step::Clock { ms }),
    ]
}

struct Harness {
    state: State,
    /// Shell answers not delivered yet.
    pending: Vec<Event>,
    /// Requests still waiting for their reply.
    open: Vec<ReqId>,
    requests: u32,
    entropy: u64,
    /// Late errors seen (the property's own coverage).
    late_errors: usize,
}

impl Harness {
    fn new(state: State) -> Self {
        Self {
            state,
            pending: Vec::new(),
            open: Vec::new(),
            requests: 0,
            entropy: 1,
            late_errors: 0,
        }
    }

    fn ctx(&mut self) -> Ctx {
        let ctx = testkit::ctx(NOW, self.entropy);
        self.entropy += 1;
        ctx
    }

    /// Runs `cmd` with its own requester and the facts it asks for.
    fn command(&mut self, cmd: Command, confirmed: bool) -> Result<(), TestCaseError> {
        self.requests += 1;
        let req = ReqId(format!("q{}", self.requests));
        let mut ctx = self.ctx();
        ctx.req = Some(req.clone());
        ctx.confirmed = confirmed;
        let need = facts_needed(&self.state, &cmd);
        for path in need.files {
            ctx.facts.files.insert(
                path.clone(),
                FileFact {
                    path,
                    bytes: 1_000_000,
                    mtime_ns: 1,
                },
            );
        }
        for take in need.take_files {
            let path = format!("takes/{take}/A1-mic.mka");
            ctx.facts.take_files.insert(
                take,
                vec![FileFact {
                    path,
                    bytes: 1_000,
                    mtime_ns: 1,
                }],
            );
        }
        if let Ok(out) = apply(&self.state, &ctx, cmd) {
            if out.reply.is_none() {
                self.open.push(req);
            }
            self.absorb(out);
        }
        Ok(())
    }

    fn event(&mut self, ev: Event) -> Result<(), TestCaseError> {
        let what = format!("{ev:?}");
        let ctx = self.ctx();
        let out = on_event(&self.state, &ctx, ev);
        heard(&out, &what)?;
        self.late_errors += late_errors(&out).len();
        self.absorb(out);
        Ok(())
    }

    fn absorb(&mut self, out: Outcome) {
        self.state = out.state;
        for effect in &out.effects {
            if let Effect::Reply { req, .. } = effect {
                self.open.retain(|r| r != req);
            }
            let more = answers(&self.state, effect);
            self.pending.extend(more);
        }
    }

    fn step(&mut self, step: &Step) -> Result<(), TestCaseError> {
        let clips: Vec<ClipId> = self
            .state
            .project
            .timeline
            .clips
            .iter()
            .map(|c| c.id.clone())
            .collect();
        let in_flight = self.state.runtime.record.take().cloned();
        match step {
            Step::Start {
                countdown,
                wait,
                over,
            } => {
                let mode = match over {
                    Some(pick) if !clips.is_empty() => {
                        RecordModeArg::Over(vec![clips[pick.index(clips.len())].clone()])
                    }
                    _ => RecordModeArg::Append,
                };
                self.command(
                    Command::RecordStart {
                        mode,
                        countdown: Some(*countdown),
                        wait: *wait,
                    },
                    false,
                )
            }
            Step::Stop => self.command(Command::RecordStop, false),
            Step::Cancel => self.command(Command::RecordCancel, false),
            Step::Toggle { wait } => self.command(Command::RecordToggle { wait: *wait }, false),
            Step::Discard => self.command(Command::TakeDiscardLast, false),
            Step::Export => self.command(
                Command::Export {
                    format: ExportFormat::Mp4,
                    quality: Quality::Draft,
                    codec: None,
                    out: None,
                },
                false,
            ),
            Step::Frame { at } => {
                let total = qrec_core::timeline::total_frames(&self.state.project.timeline.clips);
                let at = at.index(total.max(1) as usize) as u64;
                self.command(
                    Command::FrameSave {
                        at: TimeArg::frames(at),
                        layer: None,
                        out: None,
                    },
                    false,
                )
            }
            Step::Autocut { clip, dry_run } => {
                let scope = match clip {
                    Some(pick) if !clips.is_empty() => {
                        ClipScope::Clips(vec![clips[pick.index(clips.len())].clone()])
                    }
                    _ => ClipScope::All,
                };
                self.command(
                    Command::Autocut {
                        scope,
                        overrides: AutocutOverrides::default(),
                        dry_run: *dry_run,
                    },
                    false,
                )
            }
            Step::Purge { pick } => {
                let bin: Vec<TakeId> = self
                    .state
                    .project
                    .takes
                    .keys()
                    .filter(|t| {
                        !self
                            .state
                            .project
                            .timeline
                            .clips
                            .iter()
                            .any(|c| &c.take == *t)
                    })
                    .cloned()
                    .collect();
                if bin.is_empty() {
                    return Ok(());
                }
                let take = bin[pick.index(bin.len())].clone();
                self.command(Command::TakePurge { takes: vec![take] }, true)
            }
            Step::Deliver {
                pick,
                fail,
                variant,
            } => {
                if self.pending.is_empty() {
                    return Ok(());
                }
                let ev = self.pending.remove(pick.index(self.pending.len()));
                let ev = if *fail {
                    failed(&self.state, ev, *variant)
                } else {
                    ev
                };
                self.event(ev)
            }
            Step::Gone { pick } => {
                if self.open.is_empty() {
                    return Ok(());
                }
                let req = self.open[pick.index(self.open.len())].clone();
                self.event(Event::RequesterGone { req })
            }
            Step::Wedged => self.event(Event::EngineWedged {
                message: "teardown timed out".into(),
            }),
            Step::PipelineFailed => self.event(Event::CaptureFailed {
                take: None,
                source: Some("L1".into()),
                message: "pipeline error".into(),
                missing: None,
            }),
            Step::Lost { pick } => {
                let Some(take) = in_flight else {
                    return Ok(());
                };
                let streams: Vec<String> = self
                    .state
                    .project
                    .take(&take)
                    .map(|t| t.streams.iter().map(|s| s.id.clone()).collect())
                    .unwrap_or_default();
                if streams.is_empty() {
                    return Ok(());
                }
                self.event(Event::StreamLost {
                    take,
                    stream: streams[pick.index(streams.len())].clone(),
                    message: "unplugged".into(),
                })
            }
            Step::Clock { ms } => {
                let Some(take) = in_flight else {
                    return Ok(());
                };
                self.event(Event::RecordClock {
                    take,
                    elapsed_ns: ms * 1_000_000,
                })
            }
            Step::Disk { valid } => {
                let project = if *valid {
                    let mut file = self.state.project.clone();
                    file.name = "renamed by hand".into();
                    Ok(Box::new(file))
                } else {
                    Err("expected value at line 1 column 1".to_owned())
                };
                self.event(Event::ProjectChangedOnDisk { project })
            }
            Step::NoFfmpeg => {
                let mut caps = self.state.runtime.capabilities.clone();
                caps.ffmpeg = None;
                self.event(Event::Capabilities(caps))
            }
        }
    }

    /// Delivers everything left, successfully, and stops a running take.
    fn settle(&mut self) -> Result<(), TestCaseError> {
        for _ in 0..1_000 {
            if !self.pending.is_empty() {
                let ev = self.pending.remove(0);
                self.event(ev)?;
                continue;
            }
            match &self.state.runtime.record {
                RecordPhase::Recording { .. } => self.command(Command::RecordStop, false)?,
                _ => return Ok(()),
            }
        }
        Err(TestCaseError::fail("the shell never settled"))
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Whatever arrives, a late error reply comes with a toast (H-7).
    #[test]
    fn late_errors_are_always_told(
        host in prop_oneof![Just(HostKind::Gui), Just(HostKind::Headless), Just(HostKind::Cli)],
        steps in prop::collection::vec(arb_step(), 1..48),
    ) {
        let mut state = testkit::design_state();
        state.runtime.host = host;
        let mut h = Harness::new(state);
        for step in &steps {
            h.step(step)?;
        }
        h.settle()?;
    }
}

// ------------------------------------------------- every known path, named

/// A state after `cmds` (each with requester `r1`), with the effects'
/// answers still to deliver.
fn after(state: &State, cmds: &[Command]) -> (State, Vec<Event>) {
    let mut h = Harness::new(state.clone());
    for cmd in cmds {
        let confirmed = matches!(cmd, Command::TakePurge { .. });
        h.command(cmd.clone(), confirmed).unwrap();
    }
    (h.state, h.pending)
}

/// Delivers answers in order, turning the first one `which` matches into
/// its failure, and returns that failure's outcome.
fn failing(
    state: &State,
    cmds: &[Command],
    which: impl Fn(&Event) -> bool,
    variant: u8,
) -> Outcome {
    let (mut state, mut pending) = after(state, cmds);
    for _ in 0..200 {
        assert!(!pending.is_empty(), "the answer never came");
        let ev = pending.remove(0);
        let ctx = testkit::ctx(NOW, 9);
        if which(&ev) {
            let ev = failed(&state, ev, variant);
            return on_event(&state, &ctx, ev);
        }
        let out = on_event(&state, &ctx, ev);
        for effect in &out.effects {
            pending.extend(answers(&out.state, effect));
        }
        state = out.state;
    }
    panic!("the answer never came");
}

#[test]
fn every_known_late_error_is_told() {
    let base = testkit::design_state();
    let export = Command::Export {
        format: ExportFormat::Mp4,
        quality: Quality::Draft,
        codec: None,
        out: None,
    };
    let frame = Command::FrameSave {
        at: TimeArg::frames(2437),
        layer: None,
        out: None,
    };
    let autocut = |dry_run| Command::Autocut {
        scope: ClipScope::Clips(vec![ClipId::from_raw("c3e9")]),
        overrides: AutocutOverrides::default(),
        dry_run,
    };
    let purge = Command::TakePurge {
        takes: vec![TakeId::numbered(2)],
    };
    let record = |countdown| Command::RecordStart {
        mode: RecordModeArg::Append,
        countdown: Some(countdown),
        wait: true,
    };
    let is_finished = |ev: &Event| matches!(ev, Event::ExportFinished { .. });
    let is_saved = |ev: &Event| matches!(ev, Event::FrameSaved { .. });
    let is_silence = |ev: &Event| matches!(ev, Event::SilenceDetected { .. });
    let is_deleted = |ev: &Event| matches!(ev, Event::TakeDirsDeleted { .. });
    let is_started = |ev: &Event| matches!(ev, Event::CaptureStarted { .. });
    let mut cases: Vec<(&str, Outcome)> = vec![
        (
            "export failed",
            failing(&base, std::slice::from_ref(&export), is_finished, 0),
        ),
        (
            "export cancelled",
            failing(&base, std::slice::from_ref(&export), is_finished, 1),
        ),
        (
            "export output appeared",
            failing(&base, std::slice::from_ref(&export), is_finished, 2),
        ),
        (
            "frame failed",
            failing(&base, std::slice::from_ref(&frame), is_saved, 0),
        ),
        (
            "frame output appeared",
            failing(&base, &[frame], is_saved, 1),
        ),
        (
            "autocut analysis failed",
            failing(&base, &[autocut(false)], is_silence, 0),
        ),
        (
            "autocut dry run analysis failed",
            failing(&base, &[autocut(true)], is_silence, 0),
        ),
        (
            "purge left files",
            failing(&base, std::slice::from_ref(&purge), is_deleted, 0),
        ),
        ("purge not saved", failing(&base, &[purge], is_deleted, 1)),
        (
            "capture failed while starting",
            failing(&base, &[record(0)], is_started, 1),
        ),
        (
            "a missing element",
            failing(&base, &[record(0)], is_started, 0),
        ),
    ];

    // A take that won't start after the countdown (no source left).
    let (counting, _) = after(&base, &[record(1)]);
    let RecordPhase::Countdown { timer, .. } = counting.runtime.record.clone() else {
        panic!("a countdown: {:?}", counting.runtime.record);
    };
    let mut empty_scene = counting.clone();
    empty_scene.project.scene.layers.clear();
    empty_scene.project.scene.audio.clear();
    let ctx = testkit::ctx(NOW, 3);
    cases.push((
        "the take can't begin",
        on_event(&empty_scene, &ctx, Event::TimerFired { timer }),
    ));
    cases.push((
        "the pipeline fails during the countdown",
        on_event(
            &counting,
            &ctx,
            Event::CaptureFailed {
                take: None,
                source: None,
                message: "pipeline error".into(),
                missing: None,
            },
        ),
    ));
    cases.push((
        "the engine wedges during the countdown",
        on_event(
            &counting,
            &ctx,
            Event::EngineWedged {
                message: "stuck".into(),
            },
        ),
    ));
    let stale = TimerId(9_999);
    cases.push((
        "a stale timer",
        on_event(&counting, &ctx, Event::TimerFired { timer: stale }),
    ));

    // While recording with a waiter: the capture fails, the engine wedges.
    let (mut recording, mut pending) = after(&base, &[record(0)]);
    let started = pending.remove(pending.iter().position(is_started).unwrap());
    recording = on_event(&recording, &ctx, started).state;
    let take = recording.runtime.record.take().cloned().unwrap();
    cases.push((
        "the capture fails while recording",
        on_event(
            &recording,
            &ctx,
            Event::CaptureFailed {
                take: Some(take.clone()),
                source: Some("A1".into()),
                message: "device gone".into(),
                missing: None,
            },
        ),
    ));
    cases.push((
        "the engine wedges while recording",
        on_event(
            &recording,
            &ctx,
            Event::EngineWedged {
                message: "stuck".into(),
            },
        ),
    ));

    // Finalizing: the probe fails on its own, or the EOS times out.
    let mut h = Harness::new(recording.clone());
    h.command(Command::RecordStop, false).unwrap();
    let stopped = h.pending.remove(
        h.pending
            .iter()
            .position(|e| matches!(e, Event::CaptureStopped { .. }))
            .unwrap(),
    );
    let finalizing = h.state.clone();
    cases.push((
        "the files didn't close",
        on_event(&finalizing, &ctx, failed(&finalizing, stopped.clone(), 0)),
    ));
    let probing = on_event(&finalizing, &ctx, stopped);
    let probe = probing
        .effects
        .iter()
        .flat_map(|e| answers(&probing.state, e))
        .find(|e| matches!(e, Event::TakeProbed { .. }))
        .unwrap();
    for (variant, name) in [
        (0, "the take's files are unreadable"),
        (1, "the probe timed out"),
        (2, "ffprobe is missing"),
        (3, "the take is too short"),
    ] {
        cases.push((
            name,
            on_event(
                &probing.state,
                &ctx,
                failed(&probing.state, probe.clone(), variant),
            ),
        ));
    }

    let mut told = 0;
    for (name, out) in &cases {
        heard(out, name).unwrap();
        if !late_errors(out).is_empty() {
            told += 1;
        }
    }
    // The table reaches late errors on most paths (the rest reply `Ok`
    // with the bad news or have nobody waiting).
    assert!(told >= 15, "only {told} cases sent a late error");
}

#[test]
fn the_harness_reaches_late_errors() {
    // A fixed run through failures: the property is exercised, not vacuous.
    let mut h = Harness::new(testkit::design_state());
    h.command(
        Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Draft,
            codec: None,
            out: None,
        },
        false,
    )
    .unwrap();
    h.command(
        Command::Autocut {
            scope: ClipScope::All,
            overrides: AutocutOverrides::default(),
            dry_run: false,
        },
        false,
    )
    .unwrap();
    h.command(
        Command::TakePurge {
            takes: vec![TakeId::numbered(6)],
        },
        true,
    )
    .unwrap();
    for _ in 0..500 {
        if h.pending.is_empty() {
            break;
        }
        let ev = h.pending.remove(0);
        let ev = failed(&h.state, ev, 0);
        h.event(ev).unwrap();
    }
    assert!(h.pending.is_empty());
    assert!(h.late_errors >= 3, "late errors seen: {}", h.late_errors);
}

#[test]
fn job_ids_in_answers_are_the_jobs() {
    // The fake shell answers each job by its own id.
    let (state, pending) = after(
        &testkit::design_state(),
        &[Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Draft,
            codec: None,
            out: None,
        }],
    );
    let jobs: Vec<JobId> = state.runtime.jobs.keys().copied().collect();
    assert_eq!(jobs.len(), 1);
    assert!(pending.iter().all(|e| match e {
        Event::ExportProgress { job, .. }
        | Event::ExportStitchProgress { job, .. }
        | Event::ExportFinished { job, .. } => *job == jobs[0],
        _ => true,
    }));
}
