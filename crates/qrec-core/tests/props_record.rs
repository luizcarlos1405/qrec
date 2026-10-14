//! Properties of the recording state machine (A§5.3, A§5.5) and the
//! capture planner (A§9.5, A§8.11).
//!
//! The state machine is fuzzed against a fake shell that answers every
//! effect, but late and in any order (cancelled timers still fire, a
//! capture start may arrive after its stop, …). After every step the
//! project invariants hold; at the end the machine settles to Idle without
//! waiting for anything the shell won't send, and every request got
//! exactly one final reply.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use proptest::sample::Index;
use qrec_core::capture_plan::{choose_camera_mode, plan_capture, plan_files};
use qrec_core::ctx::{CameraMode, Ctx, Facts};
use qrec_core::ids::{ClipId, LayerId, ReqId, TakeId, TimerId, TrackId};
use qrec_core::model::{
    AudioRole, AudioTrack, Crop, Fit, Fps, Layer, Rect, Stream, StreamKind, TakeStatus, VideoSource,
};
use qrec_core::plans::LiveScene;
use qrec_core::protocol::command::{Command, RecordModeArg};
use qrec_core::protocol::effect::{Effect, ProbePurpose};
use qrec_core::protocol::event::{Event, MediaInfo, ProbeResult, RecordedStream};
use qrec_core::state::{CanvasMode, HostKind, RecordPhase, State};
use qrec_core::testkit::{self, strategies::arb_fps, strategies::arb_project};
use qrec_core::time::frames_to_ns;
use qrec_core::{Outcome, apply, on_event};

const NOW: &str = "2026-10-14T12:30:00Z";

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

/// Answers every effect (late): the default answers succeed with `frames`.
fn answers(state: &State, effect: &Effect, frames: u64) -> Vec<Event> {
    let fps = state.project.canvas.fps;
    match effect {
        Effect::StartTimer { timer, .. } => vec![Event::TimerFired { timer: *timer }],
        Effect::StartCapture { plan } => vec![Event::CaptureStarted {
            take: plan.take.clone(),
            t0_ns: 0,
            files: plan_files(plan),
        }],
        Effect::StopCapture { take, .. } => vec![Event::CaptureStopped {
            take: take.clone(),
            streams: Vec::new(),
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
                    info: Ok(media(j.kind, frames, fps)),
                })
                .collect(),
        }],
        _ => Vec::new(),
    }
}

// ----------------------------------------------------------- the harness

#[derive(Debug, Clone)]
enum Step {
    Start {
        over: Option<(Index, Index)>,
        countdown: u32,
        wait: bool,
    },
    Stop,
    Cancel,
    Toggle {
        wait: bool,
    },
    Discard,
    Deliver {
        pick: Index,
        variant: u8,
    },
    Clock {
        ms: u64,
    },
    Lost {
        pick: Index,
    },
    /// A failure of the whole pipeline (`CaptureFailed` without a take).
    PipelineFailed,
    Wedged,
    Stale {
        kind: u8,
    },
}

fn arb_step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (
            proptest::option::of((any::<Index>(), any::<Index>())),
            0u32..3,
            any::<bool>()
        )
            .prop_map(|(over, countdown, wait)| Step::Start { over, countdown, wait }),
        2 => Just(Step::Stop),
        1 => Just(Step::Cancel),
        2 => any::<bool>().prop_map(|wait| Step::Toggle { wait }),
        2 => Just(Step::Discard),
        10 => (any::<Index>(), any::<u8>()).prop_map(|(pick, variant)| Step::Deliver { pick, variant }),
        2 => (0u64..3_000).prop_map(|ms| Step::Clock { ms }),
        1 => any::<Index>().prop_map(|pick| Step::Lost { pick }),
        // Wedges and pipeline failures are rare: they end everything.
        2 => any::<u8>().prop_map(|kind| match kind {
            0..32 => Step::Wedged,
            32..64 => Step::PipelineFailed,
            _ => Step::Stale { kind },
        }),
    ]
}

struct Harness {
    state: State,
    /// Shell answers not delivered yet.
    pending: Vec<Event>,
    /// Final replies per request.
    finals: BTreeMap<ReqId, usize>,
    /// Requests whose reply comes later.
    open: BTreeSet<ReqId>,
    /// The live pipeline the engine runs: the last `SetLive`.
    live: Option<LiveScene>,
    requests: u32,
    entropy: u64,
}

/// What the engine should run (A§8.11): a pipeline while the GUI shows
/// Live (with preview) or while `record != Idle` (countdown included).
fn wanted_live(state: &State) -> Option<LiveScene> {
    let preview = state.runtime.host == HostKind::Gui && state.ui.canvas_mode == CanvasMode::Live;
    (preview || !state.runtime.record.is_idle()).then(|| LiveScene {
        canvas: state.project.canvas.clone(),
        scene: state.project.scene.clone(),
        preview,
    })
}

impl Harness {
    fn new(state: State) -> Self {
        Self {
            live: wanted_live(&state),
            state,
            pending: Vec::new(),
            finals: BTreeMap::new(),
            open: BTreeSet::new(),
            requests: 0,
            entropy: 1,
        }
    }

    fn ctx(&mut self, req: Option<ReqId>) -> Ctx {
        let mut ctx = testkit::ctx(NOW, self.entropy);
        self.entropy += 1;
        ctx.req = req;
        ctx
    }

    fn command(&mut self, command: Command) -> Result<(), TestCaseError> {
        self.requests += 1;
        let req = ReqId(format!("q{}", self.requests));
        let ctx = self.ctx(Some(req.clone()));
        match apply(&self.state, &ctx, command) {
            Err(_) => self.answered(&req)?,
            Ok(out) => {
                if out.reply.is_some() {
                    self.answered(&req)?;
                } else {
                    self.open.insert(req);
                }
                self.absorb(out)?;
            }
        }
        Ok(())
    }

    fn event(&mut self, ev: Event) -> Result<(), TestCaseError> {
        let ctx = self.ctx(None);
        let out = on_event(&self.state, &ctx, ev);
        prop_assert!(out.reply.is_none(), "events never reply directly");
        self.absorb(out)
    }

    fn absorb(&mut self, out: Outcome) -> Result<(), TestCaseError> {
        self.state = out.state;
        for effect in &out.effects {
            match effect {
                Effect::Reply { req, .. } => self.answered(req)?,
                Effect::SetLive(scene) => {
                    prop_assert_ne!(&self.live, scene, "a SetLive that changes nothing");
                    self.live = scene.clone();
                }
                _ => {}
            }
            let answers = answers(&self.state, effect, 90);
            self.pending.extend(answers);
        }
        prop_assert_eq!(
            &self.live,
            &wanted_live(&self.state),
            "the engine runs another pipeline than the state wants"
        );
        Ok(())
    }

    fn answered(&mut self, req: &ReqId) -> Result<(), TestCaseError> {
        let count = self.finals.entry(req.clone()).or_default();
        *count += 1;
        prop_assert_eq!(*count, 1, "{} was answered twice", req);
        self.open.remove(req);
        Ok(())
    }

    /// Delivers one pending answer, sometimes as a failure.
    fn deliver(&mut self, pick: Index, variant: u8) -> Result<(), TestCaseError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let ev = self.pending.remove(pick.index(self.pending.len()));
        let ev = self.vary(ev, variant);
        self.event(ev)
    }

    fn vary(&self, ev: Event, variant: u8) -> Event {
        let fps = self.state.project.canvas.fps;
        match ev {
            Event::CaptureStarted { take, .. } if variant % 6 == 0 => Event::CaptureFailed {
                take: Some(take),
                source: Some("L1".into()),
                message: "device busy".into(),
            },
            Event::CaptureStopped { take, .. } => Event::CaptureStopped {
                streams: recorded(&self.state, &take),
                timed_out: variant % 8 == 0,
                take,
            },
            Event::TakeProbed {
                take,
                purpose,
                results,
            } => {
                let results = results
                    .into_iter()
                    .map(|mut r| {
                        match variant % 5 {
                            0 => {
                                if let Ok(info) = &r.info {
                                    r.info = Ok(media(info.kind, 4, fps));
                                }
                            }
                            1 => r.info = Err("Invalid data found when processing input".into()),
                            2 if r.id.starts_with('A') => r.info = Err("truncated".into()),
                            _ => {}
                        }
                        r
                    })
                    .collect();
                Event::TakeProbed {
                    take,
                    purpose,
                    results,
                }
            }
            other => other,
        }
    }

    fn step(&mut self, step: &Step) -> Result<(), TestCaseError> {
        let in_flight = self.state.runtime.record.take().cloned();
        let stale = TakeId::numbered(9_999);
        match step {
            Step::Start {
                over,
                countdown,
                wait,
            } => {
                let clips = &self.state.project.timeline.clips;
                let mode = match over {
                    Some((a, n)) if !clips.is_empty() => {
                        let start = a.index(clips.len());
                        let len = 1 + n.index(clips.len() - start);
                        RecordModeArg::Over(
                            clips[start..start + len]
                                .iter()
                                .map(|c| c.id.clone())
                                .collect(),
                        )
                    }
                    _ => RecordModeArg::Append,
                };
                self.command(Command::RecordStart {
                    mode,
                    countdown: Some(*countdown),
                    wait: *wait,
                })
            }
            Step::Stop => self.command(Command::RecordStop),
            Step::Cancel => self.command(Command::RecordCancel),
            Step::Toggle { wait } => self.command(Command::RecordToggle { wait: *wait }),
            Step::Discard => self.command(Command::TakeDiscardLast),
            Step::Deliver { pick, variant } => self.deliver(*pick, *variant),
            Step::Clock { ms } => self.event(Event::RecordClock {
                take: in_flight.unwrap_or(stale),
                elapsed_ns: ms * 1_000_000,
            }),
            Step::Lost { pick } => {
                let streams: Vec<String> = in_flight
                    .as_ref()
                    .and_then(|t| self.state.project.take(t))
                    .map(|t| t.streams.iter().map(|s| s.id.clone()).collect())
                    .unwrap_or_default();
                if streams.is_empty() {
                    return Ok(());
                }
                self.event(Event::StreamLost {
                    take: in_flight.unwrap_or(stale),
                    stream: streams[pick.index(streams.len())].clone(),
                    message: "unplugged".into(),
                })
            }
            Step::PipelineFailed => self.event(Event::CaptureFailed {
                take: None,
                source: Some("L1".into()),
                message: "pipeline error".into(),
            }),
            Step::Wedged => self.event(Event::EngineWedged {
                message: "teardown timed out".into(),
            }),
            Step::Stale { kind } => self.event(match kind % 4 {
                0 => Event::TimerFired {
                    timer: TimerId(99_999),
                },
                1 => Event::CaptureStarted {
                    take: stale,
                    t0_ns: 0,
                    files: vec![],
                },
                2 => Event::CaptureStopped {
                    take: stale,
                    streams: vec![],
                    timed_out: false,
                },
                _ => Event::TakeProbed {
                    take: stale,
                    purpose: ProbePurpose::Finalize,
                    results: vec![],
                },
            }),
        }
    }

    /// Delivers everything (successfully) and stops a running take until
    /// the machine is Idle with nothing pending.
    fn settle(&mut self) -> Result<(), TestCaseError> {
        for _ in 0..500 {
            if !self.pending.is_empty() {
                let ev = self.pending.remove(0);
                let ev = self.vary(ev, 3);
                self.event(ev)?;
                continue;
            }
            match &self.state.runtime.record {
                RecordPhase::Idle => return Ok(()),
                RecordPhase::Recording { .. } => self.command(Command::RecordStop)?,
                phase => {
                    return Err(TestCaseError::fail(format!(
                        "stuck in {phase:?} with nothing pending"
                    )));
                }
            }
        }
        Err(TestCaseError::fail("the machine never settled"))
    }
}

fn has_media(s: &Stream) -> bool {
    match s.kind {
        StreamKind::Video => s.width.is_some() && s.height.is_some() && s.frames.is_some(),
        StreamKind::Audio => s.sample_rate.is_some() && s.channels.is_some(),
    }
}

/// The invariants every reachable state keeps.
fn check(state: &State) -> Result<(), TestCaseError> {
    let p = &state.project;
    let in_flight = state.runtime.record.take();
    if let Some(t) = in_flight {
        prop_assert_eq!(p.take(t).map(|t| t.status), Some(TakeStatus::Recording));
    }
    if let RecordPhase::Countdown { remaining, .. } = &state.runtime.record {
        prop_assert!(*remaining >= 1);
    }
    for (id, take) in &p.takes {
        prop_assert!(p.next_take_number > id.number(), "{} ≥ nextTakeNumber", id);
        if take.status == TakeStatus::Recording {
            prop_assert_eq!(Some(id), in_flight, "a leftover recording take");
            continue;
        }
        for s in &take.streams {
            prop_assert!(has_media(s), "{}/{} has no media fields", id, s.id);
        }
        let no_stream = |entry: &str| take.stream(entry).is_none();
        for layer in &take.scene.layers {
            prop_assert!(!no_stream(layer.id.as_str()) || layer.missing);
        }
        for track in &take.scene.audio {
            prop_assert!(!no_stream(track.id.as_str()) || track.missing);
        }
        if take.placed {
            prop_assert_eq!(take.status, TakeStatus::Ok);
        }
        prop_assert_eq!(take.status == TakeStatus::Broken, take.error.is_some());
    }
    let mut ids = BTreeSet::new();
    for clip in &p.timeline.clips {
        prop_assert!(ids.insert(clip.id.clone()), "duplicate clip {}", clip.id);
        let take = p.take(&clip.take);
        prop_assert!(take.is_some(), "{} references a missing take", clip.id);
        let take = take.unwrap();
        prop_assert_eq!(take.status, TakeStatus::Ok);
        prop_assert!(clip.in_ < clip.out && clip.out <= take.duration_frames);
    }
    let seqs: Vec<u64> = state.history.entries.iter().map(|e| e.seq).collect();
    prop_assert!(seqs.windows(2).all(|w| w[0] < w[1]));
    prop_assert_eq!(
        state.history.cursor,
        seqs.last().copied().unwrap_or(state.history.base_seq)
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Any interleaving of record commands and (late, failing, stale) shell
    /// answers keeps the invariants, settles to Idle and answers every
    /// request exactly once; the engine always runs the live pipeline the
    /// state wants.
    #[test]
    fn the_record_machine_is_safe_and_live(
        project in arb_project(),
        host in prop_oneof![Just(HostKind::Gui), Just(HostKind::Headless), Just(HostKind::Cli)],
        program in any::<bool>(),
        steps in prop::collection::vec(arb_step(), 1..40),
    ) {
        let mut state = testkit::state(project);
        state.runtime.host = host;
        if program {
            state.ui.canvas_mode = CanvasMode::Program;
        }
        let mut h = Harness::new(state);
        check(&h.state)?;
        for step in &steps {
            h.step(step)?;
            check(&h.state)?;
        }
        h.settle()?;
        check(&h.state)?;
        prop_assert_eq!(&h.state.runtime.record, &RecordPhase::Idle);
        prop_assert!(h.open.is_empty(), "never answered: {:?}", h.open);
    }

    /// Recording a take (append or over a contiguous run) and discarding it
    /// gives the timeline back (A§5.5).
    #[test]
    fn record_then_discard_restores_the_timeline(
        project in arb_project(),
        over in proptest::option::of((any::<Index>(), any::<Index>())),
        frames in 10u64..400,
    ) {
        let state = testkit::state(project);
        let original = state.project.timeline.clone();
        let clips = &original.clips;
        let mode = match over {
            Some((a, n)) if !clips.is_empty() => {
                let start = a.index(clips.len());
                let len = 1 + n.index(clips.len() - start);
                RecordModeArg::Over(clips[start..start + len].iter().map(|c| c.id.clone()).collect())
            }
            _ => RecordModeArg::Append,
        };
        let over_ids: Vec<ClipId> = match &mode {
            RecordModeArg::Over(ids) => ids.clone(),
            _ => vec![],
        };
        let ctx = testkit::ctx(NOW, 5);
        let shell = |s: &State, e: &Effect| {
            let mut events = answers(s, e, frames);
            for ev in &mut events {
                if let Event::CaptureStopped { take, streams, .. } = ev {
                    *streams = recorded(s, take);
                }
            }
            events
        };
        let commands = [
            Command::RecordStart { mode, countdown: Some(0), wait: false },
            Command::RecordStop,
        ];
        let recorded_state = testkit::drive(&state, &ctx, &commands, shell).unwrap();
        prop_assert_eq!(&recorded_state.runtime.record, &RecordPhase::Idle);
        let (id, take) = recorded_state.project.takes.iter().next_back().unwrap();
        prop_assert!(take.placed);
        prop_assert_eq!(take.duration_frames, frames);
        let replaced: Vec<ClipId> = take.replaced.iter().map(|c| c.id.clone()).collect();
        prop_assert_eq!(replaced, over_ids);
        let theirs: Vec<_> = recorded_state.project.timeline.clips.iter().filter(|c| &c.take == id).collect();
        prop_assert_eq!(theirs.len(), 1);
        prop_assert_eq!((theirs[0].in_, theirs[0].out), (0, frames));

        let discarded = testkit::drive(&recorded_state, &ctx, &[Command::TakeDiscardLast], testkit::no_shell).unwrap();
        prop_assert_eq!(&discarded.project.timeline, &original);
        prop_assert_eq!(discarded.history.entries.len(), 2);
    }

    /// Capture file names stay inside the take folder, match the purge-safe
    /// pattern (A§2.3) and never collide, whatever the output and device
    /// names are.
    #[test]
    fn capture_files_are_safe_and_unique(
        project in arb_project(),
        number in 1u32..20_000,
        output in "\\PC{0,40}",
        device in "\\PC{0,40}",
    ) {
        let mut project = project;
        let layer = |id: &str, source: VideoSource| Layer {
            id: id.parse::<LayerId>().unwrap(),
            name: "x".into(),
            visible: false,
            opacity: 1.0,
            source,
            rect: Rect::new(0, 0, 640, 360),
            crop: Crop::default(),
            fit: Fit::Contain,
            locked: false,
            missing: false,
        };
        project.scene.layers.push(layer("L90", VideoSource::Screen { output: output.clone(), label: output }));
        project.scene.layers.push(layer("L91", VideoSource::Camera { device: device.clone(), label: device }));
        project.scene.audio.push(AudioTrack {
            id: "A90".parse::<TrackId>().unwrap(),
            role: AudioRole::System,
            source: "alsa_output.monitor".into(),
            label: "x".into(),
            gain_db: 0.0,
            muted: false,
            sync_offset_ms: 0,
            missing: false,
        });
        let take = TakeId::numbered(number);
        let plan = plan_capture(&project, &take, &testkit::capabilities(), &Facts::default()).unwrap();
        prop_assert_eq!(plan.video.len(), project.scene.layers.len());
        prop_assert_eq!(plan.audio.len(), project.scene.audio.len());
        let files = plan_files(&plan);
        let unique: BTreeSet<&String> = files.iter().collect();
        prop_assert_eq!(unique.len(), files.len());
        let dir = format!("takes/{take}/");
        for file in &files {
            let name = file.strip_prefix(&dir);
            prop_assert!(name.is_some(), "{} outside {}", file, dir);
            let name = name.unwrap();
            prop_assert!(!name.is_empty() && name.len() <= 120);
            prop_assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)), "{}", name);
        }
    }

    /// The camera mode is the closest rate at or above the canvas (within
    /// 1000/1001), then the largest frame; raw/MJPEG over other formats.
    #[test]
    fn camera_mode_rule(modes in prop::collection::vec(arb_mode(), 0..12), fps in arb_fps()) {
        let chosen = choose_camera_mode(&modes, fps);
        if modes.is_empty() {
            prop_assert!(chosen.is_none());
            return Ok(());
        }
        let chosen = chosen.unwrap();
        prop_assert!(modes.contains(&chosen));
        let preferred: Vec<&CameraMode> = modes.iter().filter(|m| m.format != "video/x-h264").collect();
        let pool: Vec<&CameraMode> = if preferred.is_empty() { modes.iter().collect() } else { preferred };
        prop_assert!(pool.contains(&&chosen));
        let rate = |m: &CameraMode| (u128::from(m.fps.num), u128::from(m.fps.den));
        let cmp = |a: &CameraMode, b: &CameraMode| {
            let ((an, ad), (bn, bd)) = (rate(a), rate(b));
            (an * bd).cmp(&(bn * ad))
        };
        let qualifies = |m: &CameraMode| {
            let (n, d) = rate(m);
            n * u128::from(fps.den) * 1001 >= u128::from(fps.num) * d * 1000
        };
        let area = |m: &CameraMode| u64::from(m.width) * u64::from(m.height);
        let any_qualifies = pool.iter().any(|m| qualifies(m));
        prop_assert_eq!(qualifies(&chosen), any_qualifies);
        for m in &pool {
            if any_qualifies && !qualifies(m) {
                continue;
            }
            let order = cmp(m, &chosen);
            if any_qualifies {
                prop_assert!(order.is_ge(), "{:?} is closer than {:?}", m, chosen);
            } else {
                prop_assert!(order.is_le(), "{:?} is faster than {:?}", m, chosen);
            }
            if order.is_eq() {
                prop_assert!(area(m) <= area(&chosen), "{:?} is larger than {:?}", m, chosen);
            }
        }
    }
}

fn arb_mode() -> impl Strategy<Value = CameraMode> {
    (
        prop_oneof![
            Just((640u32, 480u32)),
            Just((1280, 720)),
            Just((1920, 1080)),
            Just((3840, 2160)),
            (16u32..4000, 16u32..3000),
        ],
        prop_oneof![
            arb_fps(),
            Just(Fps::new(5, 1)),
            Just(Fps::new(15, 1)),
            Just(Fps::new(120, 1)),
        ],
        prop_oneof![
            Just("YUY2"),
            Just("NV12"),
            Just("image/jpeg"),
            Just("video/x-h264")
        ],
    )
        .prop_map(|((width, height), fps, format)| CameraMode {
            width,
            height,
            fps,
            format: format.to_owned(),
        })
}
