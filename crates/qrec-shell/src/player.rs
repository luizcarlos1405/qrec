//! The Program player (GUI H-2, docs/GUI.md G§3.10): one GES timeline and
//! pipeline per session on thread `qrec-player`, rebuilt in place by
//! [`ProgramPlayer::load`] and played into the GUI's Program
//! `gtk4paintablesink`. Every call returns at once; the thread does the work
//! in order, and loads and seeks that pile up while it is busy coalesce
//! (the newest wins). Owner: capture.
//!
//! - **Load**: pause if playing, [`ges_render::rebuild_into`], commit (a
//!   stopped pipeline prerolls instead), an accurate seek to `at_ns`, then
//!   [`PlayerEvent::Loaded`] once that frame reached the video sink, and
//!   play again if it was playing. An empty plan stops the pipeline (no
//!   picture; `Loaded` with a zero duration).
//! - **Seeks** are `FLUSH | ACCURATE`, one in flight: a seek is done when
//!   the first frame after its flush, at its time, reaches the video sink (a
//!   pad probe), or the sink gets EOS. A seek past the last frame shows the
//!   last frame (the position still says where the playhead is).
//! - **Rates**: forward rates are seeks with that rate where playback is;
//!   reverse is stepping (GUI G§3.10, R4): an accurate seek at most 15
//!   times a second, to where `|rate|` times the time since playing started
//!   puts it, frame-aligned, video only (the pipeline stays paused).
//!   `Ended { reverse: true }` at 0.
//! - **Scrub**: while paused, a seek with a stop 60 ms later and the
//!   pipeline playing until that end, then paused and back on the scrubbed
//!   frame; while playing, a plain seek.
//! - **Late frames** (H-8): QoS drops are counted per element and reported
//!   as [`PlayerEvent::Late`] at most once a second, and logged.
//! - **`out_size`** (H-9): the plan is composited at the canvas fitted into
//!   that size ([`fit_size`], never larger than the canvas), through
//!   [`scaled`].
//!
//! Every wait has a deadline: a load or seek [`SEEK_TIMEOUT`], stopping
//! [`STOP_TIMEOUT`], opening the sound output [`SOUND_OPEN_TIMEOUT`].
//! GStreamer is initialized on the player thread (through
//! [`ges_render::init`], which keeps GES in software); its bus has a sync
//! handler that forwards to the thread, never a watch on a main context
//! (GUI R5).

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use ges::prelude::*;
use qrec_core::Fps;
use qrec_core::ges_plan::{fit_size, scaled};
use qrec_core::model::Canvas;
use qrec_core::plans::GesTimeline;
use qrec_core::time::{frame_ns, ns_to_frame_ceil, ns_to_frame_floor};

use crate::ges_render::{self, Tracks};

/// How long a load (its commit or preroll) or a seek may take before the
/// player reports an error.
pub const SEEK_TIMEOUT: Duration = Duration::from_secs(10);

/// How long [`ProgramPlayer::shutdown`] lets the pipeline take to stop.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the system sound output may take to open before the player
/// plays silently instead.
pub const SOUND_OPEN_TIMEOUT: Duration = Duration::from_secs(3);

/// How much sound a scrub plays.
pub const SCRUB_NS: u64 = 60_000_000;

/// The position is read this often while playing (125 Hz).
const POSITION_PERIOD: Duration = Duration::from_millis(8);

/// Reverse stepping seeks at most 15 times a second (GUI G§9 R4).
const MIN_STEP_PERIOD: Duration = Duration::from_millis(67);

/// A scrub's sound ends at its stop; this is the latest it may end.
const SCRUB_SOUND_TIMEOUT: Duration = Duration::from_millis(500);

/// Late frames are reported at most this often.
const LATE_PERIOD: Duration = Duration::from_secs(1);

/// The fastest rate either way.
const MAX_RATE: f64 = 16.0;

/// How to build a player.
pub struct PlayerOptions {
    /// Plan paths are joined with it.
    pub project_dir: PathBuf,
    /// The Program `gtk4paintablesink`, made on the GTK thread.
    pub video_sink: gst::Element,
    pub audio: AudioOut,
    /// `None`: composite at the canvas size; `Some`: at the canvas fitted
    /// into this size (never larger than the canvas), for a machine that
    /// can't play at canvas size (GUI H-9).
    pub out_size: Option<(u32, u32)>,
    /// Called on the player thread; must return at once.
    pub events: Box<dyn Fn(PlayerEvent) + Send + Sync>,
}

impl std::fmt::Debug for PlayerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayerOptions")
            .field("project_dir", &self.project_dir)
            .field("audio", &self.audio)
            .field("out_size", &self.out_size)
            .finish_non_exhaustive()
    }
}

/// Where the player's sound goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOut {
    /// `pulsesink client-name=Qrec`; a `fakesink` and a warning when it
    /// can't start.
    System,
    /// `fakesink sync=true`: the fake backend and tests.
    Silent,
}

/// What the player reports (through [`PlayerOptions::events`]).
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// A plan is in place and its frame at the load's position reached the
    /// video sink; its length.
    Loaded { duration_ns: u64 },
    /// Playback reached the end (the start, playing in reverse).
    Ended { reverse: bool },
    /// Playback stopped on an error; the player stays paused.
    Error { message: String },
    /// Frames dropped as late since the last report (QoS).
    Late { dropped: u64 },
    /// After [`ProgramPlayer::shutdown`]: the pipeline reached NULL.
    Stopped,
}

/// Plays the program timeline (or one take) for the Program picture.
#[derive(Debug)]
pub struct ProgramPlayer {
    tx: Sender<Input>,
    shared: Arc<Shared>,
}

/// What the handle reads without asking the thread.
#[derive(Debug, Default)]
struct Shared {
    position: AtomicU64,
    /// Commands sent and not handled yet.
    queued: AtomicU64,
    /// A load, seek or scrub is waiting or running.
    busy: AtomicBool,
    /// [`ProgramPlayer::shutdown`] was called.
    stopping: AtomicBool,
}

impl ProgramPlayer {
    /// Starts the player thread `qrec-player`, which initializes GStreamer
    /// and builds the pipeline at once (an error is reported as
    /// [`PlayerEvent::Error`]).
    pub fn spawn(opts: PlayerOptions) -> std::io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared::default());
        let (worker_tx, worker_shared) = (tx.clone(), Arc::clone(&shared));
        // GES timelines aren't `Send`: everything GES lives on this thread.
        thread::Builder::new()
            .name("qrec-player".into())
            .spawn(move || Worker::new(opts, worker_tx, rx, worker_shared).run())?;
        Ok(Self { tx, shared })
    }

    /// Shows `plan` from `at_ns`, playing or paused as before (the newest
    /// load wins).
    pub fn load(&self, plan: GesTimeline, at_ns: u64) {
        self.send(Cmd::Load(Box::new(plan), at_ns));
    }

    /// An accurate seek; one at a time, the newest wins.
    pub fn seek(&self, ns: u64) {
        self.send(Cmd::Seek(ns));
    }

    /// Plays at `rate` (1, 2, 4; negative steps backwards), also changing
    /// the rate while playing.
    pub fn play(&self, rate: f64) {
        self.send(Cmd::Play(rate));
    }

    pub fn pause(&self) {
        self.send(Cmd::Pause);
    }

    /// A seek plus 60 ms of sound (ruler scrubbing).
    pub fn scrub(&self, ns: u64) {
        self.send(Cmd::Scrub(ns));
    }

    /// Where playback is, updated at least 60 times a second while playing.
    pub fn position_ns(&self) -> u64 {
        self.shared.position.load(Ordering::SeqCst)
    }

    /// Nothing to load, seek or scrub, queued or in flight: the picture
    /// shows the last load's or seek's frame (for the GUI's `waitIdle`,
    /// G§3.18). Playing doesn't count.
    pub fn idle(&self) -> bool {
        self.shared.queued.load(Ordering::SeqCst) == 0 && !self.shared.busy.load(Ordering::SeqCst)
    }

    /// Stops the pipeline; [`PlayerEvent::Stopped`] once it reached NULL
    /// (within 5 s). Later calls do nothing.
    pub fn shutdown(&self) {
        if !self.shared.stopping.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(Input::Cmd(Cmd::Shutdown));
        }
    }

    fn send(&self, cmd: Cmd) {
        if self.shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        self.shared.queued.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(Input::Cmd(cmd)).is_err() {
            self.shared.queued.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for ProgramPlayer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ------------------------------------------------------------- the thread

enum Input {
    Cmd(Cmd),
    Bus(gst::Message),
    /// The video sink got the first frame of seek `id` (or its EOS).
    Shown(u64),
}

enum Cmd {
    Load(Box<GesTimeline>, u64),
    Seek(u64),
    Play(f64),
    Pause,
    Scrub(u64),
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeekKind {
    Plain,
    /// Plays [`SCRUB_NS`] of sound, then comes back.
    Scrub,
}

/// What follows a seek once its frame shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    Nothing,
    /// A load's seek: [`PlayerEvent::Loaded`].
    Loaded,
    /// A scrub's seek: its sound plays, then the picture comes back to `at`.
    ScrubSound {
        at: u64,
    },
}

/// What the thread waits for before it starts the next load or seek.
#[derive(Debug, Clone, Copy)]
enum Flight {
    Seek {
        id: u64,
        deadline: Instant,
        after: After,
        what: &'static str,
    },
    /// A scrub's sound plays until EOS (at the latest `until`).
    ScrubSound { at: u64, until: Instant },
}

/// The seek the video sink's pad probe waits for.
#[derive(Debug)]
struct Awaited {
    id: u64,
    target_ns: u64,
    tolerance_ns: u64,
    /// A flush passed since the seek was sent: frames now are the seek's.
    flushed: bool,
}

type AwaitedSlot = Arc<Mutex<Option<Awaited>>>;

fn lock(slot: &AwaitedSlot) -> MutexGuard<'_, Option<Awaited>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The GES side, made once.
struct Pipeline {
    timeline: ges::Timeline,
    pipeline: ges::Pipeline,
    awaited: AwaitedSlot,
    /// Frames shorter than this never reach the video sink (see
    /// [`watch_frames`]): half a frame of the loaded plan.
    sliver_ns: Arc<AtomicU64>,
    probe: Option<(gst::Pad, gst::PadProbeId)>,
    /// At least PAUSED (prerolled once).
    running: bool,
    /// Its target state is PLAYING.
    playing: bool,
    /// The rate of the last seek.
    rate: f64,
    /// It reached the end and no seek happened since.
    at_eos: bool,
}

/// Reverse playback in progress.
#[derive(Debug, Clone, Copy)]
struct Stepping {
    origin_ns: u64,
    origin: Instant,
    next: Instant,
}

struct Worker {
    project_dir: PathBuf,
    video_sink: gst::Element,
    audio: AudioOut,
    out_size: Option<(u32, u32)>,
    events: Box<dyn Fn(PlayerEvent) + Send + Sync>,
    tx: Sender<Input>,
    rx: Receiver<Input>,
    /// Inputs read while looking for something else, handled first.
    deferred: VecDeque<Input>,
    shared: Arc<Shared>,
    gst: Option<Pipeline>,
    /// GStreamer or GES couldn't start; every load says so.
    broken: Option<String>,
    /// The loaded plan: its length and frame rate.
    duration_ns: u64,
    fps: Fps,
    position: u64,
    /// What the caller wants.
    playing: bool,
    rate: f64,
    /// Waiting for the current flight (newest wins).
    load: Option<GesTimeline>,
    seek: Option<(u64, SeekKind)>,
    flight: Option<Flight>,
    stepping: Option<Stepping>,
    /// An error was reported and nothing worked since: later bus errors of
    /// the same failure are only logged.
    failed: bool,
    late: Late,
    next_id: u64,
    exiting: bool,
}

impl Worker {
    fn new(
        opts: PlayerOptions,
        tx: Sender<Input>,
        rx: Receiver<Input>,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            project_dir: opts.project_dir,
            video_sink: opts.video_sink,
            audio: opts.audio,
            out_size: opts.out_size,
            events: opts.events,
            tx,
            rx,
            deferred: VecDeque::new(),
            shared,
            gst: None,
            broken: None,
            duration_ns: 0,
            fps: Canvas::default().fps,
            position: 0,
            playing: false,
            rate: 1.0,
            load: None,
            seek: None,
            flight: None,
            stepping: None,
            failed: false,
            late: Late::default(),
            next_id: 0,
            exiting: false,
        }
    }

    fn run(mut self) {
        if let Err(message) = self.ensure_pipeline() {
            self.report_error(message);
        }
        while !self.exiting {
            let mut handled = 0;
            if let Some(input) = self.next_input() {
                handled += self.handle(input);
            }
            // Everything already queued first, so loads and seeks coalesce.
            while !self.exiting {
                let Some(input) = self
                    .deferred
                    .pop_front()
                    .or_else(|| self.rx.try_recv().ok())
                else {
                    break;
                };
                handled += self.handle(input);
            }
            if !self.exiting {
                self.tick(Instant::now());
            }
            self.publish();
            // After `busy` is published, so `idle()` is never true early.
            self.shared.queued.fetch_sub(handled, Ordering::SeqCst);
        }
        self.stop();
    }

    /// The next input, waiting until the next thing to do.
    fn next_input(&mut self) -> Option<Input> {
        if let Some(input) = self.deferred.pop_front() {
            return Some(input);
        }
        let received = match self.wakeup() {
            Some(at) => self.rx.recv_deadline(at),
            None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(input) => Some(input),
            Err(RecvTimeoutError::Timeout) => None,
            // Unreachable while `self.tx` lives; stop cleanly anyway.
            Err(RecvTimeoutError::Disconnected) => {
                self.exiting = true;
                None
            }
        }
    }

    /// When the thread has something to do without an input.
    fn wakeup(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut at: Option<Instant> = None;
        let mut sooner = |t: Instant| at = Some(at.map_or(t, |a| a.min(t)));
        match self.flight {
            Some(Flight::Seek { deadline, .. }) => sooner(deadline),
            Some(Flight::ScrubSound { until, .. }) => sooner(until),
            None => {
                if self.load.is_some() || self.seek.is_some() {
                    sooner(now);
                }
            }
        }
        if self.playing_forward() {
            sooner(now + POSITION_PERIOD);
        }
        // The next step once the one in flight showed.
        if self.playing && self.rate < 0.0 && self.flight.is_none() {
            sooner(self.stepping.map_or(now, |s| s.next));
        }
        if let Some(due) = self.late.due() {
            sooner(due);
        }
        at
    }

    /// Handles one input; 1 for a counted command.
    fn handle(&mut self, input: Input) -> u64 {
        match input {
            Input::Cmd(Cmd::Shutdown) => {
                self.exiting = true;
                0
            }
            Input::Cmd(cmd) => {
                self.command(cmd);
                1
            }
            Input::Bus(msg) => {
                self.on_bus(&msg);
                0
            }
            Input::Shown(id) => {
                self.on_shown(id);
                0
            }
        }
    }

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Load(plan, at) => {
                self.load = Some(*plan);
                self.seek = Some((at, SeekKind::Plain));
                self.restart_stepping(at);
            }
            Cmd::Seek(at) => {
                self.seek = Some((at, SeekKind::Plain));
                self.restart_stepping(at);
            }
            Cmd::Scrub(at) => {
                let kind = if self.playing {
                    SeekKind::Plain
                } else {
                    SeekKind::Scrub
                };
                self.seek = Some((at, kind));
                self.restart_stepping(at);
            }
            Cmd::Play(rate) if rate.is_finite() && rate != 0.0 => {
                let rate = rate.clamp(-MAX_RATE, MAX_RATE);
                if !self.playing || self.rate != rate {
                    self.stepping = None;
                }
                self.playing = true;
                self.rate = rate;
            }
            Cmd::Play(_) | Cmd::Pause => {
                self.playing = false;
                self.stepping = None;
            }
            Cmd::Shutdown => self.exiting = true,
        }
    }

    /// Reverse playback goes on from `at` after a seek or load.
    fn restart_stepping(&mut self, at: u64) {
        let period = self.step_period();
        if let Some(stepping) = &mut self.stepping {
            let now = Instant::now();
            *stepping = Stepping {
                origin_ns: at,
                origin: now,
                next: now + period,
            };
        }
    }

    fn playing_forward(&self) -> bool {
        self.playing && self.rate > 0.0 && self.gst.as_ref().is_some_and(|p| p.playing)
    }

    /// Starts what is due: a flight past its deadline fails, the next load
    /// or seek starts when none is in flight, then the pipeline follows
    /// play and pause.
    fn tick(&mut self, now: Instant) {
        match self.flight {
            Some(Flight::Seek { deadline, what, .. }) if now >= deadline => {
                self.flight = None;
                self.clear_awaited();
                self.fail(format!(
                    "{what} didn't finish within {} s",
                    SEEK_TIMEOUT.as_secs()
                ));
            }
            Some(Flight::ScrubSound { at, until }) if now >= until => {
                self.flight = None;
                self.end_scrub(at);
            }
            _ => {}
        }
        if self.flight.is_none() {
            if let Some(plan) = self.load.take() {
                self.do_load(plan);
            } else if let Some((at, kind)) = self.seek.take() {
                self.do_seek(at, kind, After::Nothing, "seeking");
            }
        }
        if self.flight.is_none() && self.load.is_none() && self.seek.is_none() {
            self.follow_play_state(now);
        }
        if self.playing_forward() && self.flight.is_none() {
            self.poll_position();
        }
        if let Some(dropped) = self.late.report(now) {
            self.emit(PlayerEvent::Late { dropped });
        }
    }

    fn publish(&self) {
        self.shared.position.store(self.position, Ordering::SeqCst);
        let busy = self.flight.is_some() || self.load.is_some() || self.seek.is_some();
        self.shared.busy.store(busy, Ordering::SeqCst);
    }

    fn emit(&self, event: PlayerEvent) {
        tracing::debug!("player: {event:?}");
        (self.events)(event);
    }

    // ------------------------------------------------------- the pipeline

    /// Initializes GES and makes the pipeline, once.
    fn ensure_pipeline(&mut self) -> Result<(), String> {
        if self.gst.is_some() {
            return Ok(());
        }
        if let Some(why) = &self.broken {
            return Err(why.clone());
        }
        match self.make_pipeline() {
            Ok(pipeline) => {
                self.gst = Some(pipeline);
                Ok(())
            }
            Err(why) => {
                self.broken = Some(why.clone());
                Err(why)
            }
        }
    }

    fn make_pipeline(&self) -> Result<Pipeline, String> {
        ges_render::init().map_err(|e| e.to_string())?;
        let empty = GesTimeline {
            canvas: Canvas::default(),
            duration_ns: 0,
            video: Vec::new(),
            audio: Vec::new(),
        };
        let timeline =
            ges_render::build(&empty, &self.project_dir, Tracks::All).map_err(|e| e.to_string())?;
        let pipeline = ges_render::preview_pipeline(&timeline, ges::PipelineFlags::FULL_PREVIEW)
            .map_err(|e| e.to_string())?;
        pipeline.preview_set_video_sink(Some(&self.video_sink));
        pipeline.preview_set_audio_sink(Some(&sound_sink(self.audio)?));
        let bus = pipeline
            .bus()
            .ok_or_else(|| "the player's pipeline has no bus".to_owned())?;
        let tx = self.tx.clone();
        bus.set_sync_handler(move |_, msg| {
            use gst::MessageView as M;
            if matches!(
                msg.view(),
                M::Eos(_) | M::Error(_) | M::Warning(_) | M::Qos(_) | M::Latency(_)
            ) {
                let _ = tx.send(Input::Bus(msg.clone()));
            }
            gst::BusSyncReply::Drop
        });
        let awaited: AwaitedSlot = Arc::new(Mutex::new(None));
        let sliver_ns = Arc::new(AtomicU64::new(0));
        let probe = watch_frames(
            &self.video_sink,
            Arc::clone(&awaited),
            Arc::clone(&sliver_ns),
            self.tx.clone(),
        );
        if probe.is_none() {
            tracing::warn!("player: the video sink has no sink pad; seeks aren't confirmed");
        }
        Ok(Pipeline {
            timeline,
            pipeline,
            awaited,
            sliver_ns,
            probe,
            running: false,
            playing: false,
            rate: 1.0,
            at_eos: false,
        })
    }

    /// `plan` at the size it is composited at (H-9).
    fn fitted(&self, plan: GesTimeline) -> GesTimeline {
        let Some(max) = self.out_size else {
            return plan;
        };
        let canvas = (plan.canvas.width, plan.canvas.height);
        let (w, h) = fit_size(canvas.0, canvas.1, max);
        if (w, h) == canvas {
            plan
        } else {
            scaled(plan, w, h)
        }
    }

    fn do_load(&mut self, plan: GesTimeline) {
        if let Err(message) = self.ensure_pipeline() {
            self.seek = None;
            self.fail(message);
            return;
        }
        let plan = self.fitted(plan);
        let Some(p) = self.gst.as_mut() else {
            return;
        };
        if p.playing {
            let _ = p.pipeline.set_state(gst::State::Paused);
            p.playing = false;
        }
        let built = ges_render::rebuild_into(&p.timeline, &plan, &self.project_dir);
        self.duration_ns = 0;
        if let Err(e) = built {
            self.seek = None;
            self.fail(e.to_string());
            return;
        }
        self.duration_ns = plan.duration_ns;
        self.fps = plan.canvas.fps;
        p.sliver_ns
            .store((frame_ns(1, self.fps) / 2).max(0) as u64, Ordering::SeqCst);
        if self.duration_ns == 0 {
            // Nothing to show: the pipeline stops (no picture).
            let _ = p.timeline.commit();
            self.halt();
            self.seek = None;
            self.position = 0;
            self.failed = false;
            self.emit(PlayerEvent::Loaded { duration_ns: 0 });
            return;
        }
        let ready = if p.running {
            commit_and_wait(&p.timeline, SEEK_TIMEOUT, &self.shared.stopping)
        } else {
            // A stopped pipeline applies the commit as it starts.
            let _ = p.timeline.commit();
            self.start()
        };
        if let Err(message) = ready {
            self.seek = None;
            self.fail(message);
            return;
        }
        let (at, kind) = self.seek.take().unwrap_or((self.position, SeekKind::Plain));
        self.do_seek(at, kind, After::Loaded, "loading");
    }

    /// Prerolls a stopped pipeline (PAUSED), waiting at most
    /// [`SEEK_TIMEOUT`] (less when a shutdown comes).
    fn start(&mut self) -> Result<(), String> {
        let Some(p) = self.gst.as_mut() else {
            return Err("the player has no pipeline".into());
        };
        let changed = p.pipeline.set_state(gst::State::Paused);
        let deadline = Instant::now() + SEEK_TIMEOUT;
        let result = loop {
            let (result, _, _) = p.pipeline.state(gst::ClockTime::from_mseconds(100));
            if !matches!(result, Ok(gst::StateChangeSuccess::Async))
                || Instant::now() >= deadline
                || self.shared.stopping.load(Ordering::SeqCst)
            {
                break result;
            }
        };
        match (changed, result) {
            (Ok(_), Ok(gst::StateChangeSuccess::Async)) => {
                let _ = p.pipeline.set_state(gst::State::Ready);
                Err(format!(
                    "the picture didn't start within {} s",
                    SEEK_TIMEOUT.as_secs()
                ))
            }
            (Ok(_), Ok(_)) => {
                p.running = true;
                p.at_eos = false;
                Ok(())
            }
            _ => {
                let _ = p.pipeline.set_state(gst::State::Ready);
                Err(self.bus_error("the pipeline failed to start"))
            }
        }
    }

    /// The first error the bus already posted, or `fallback`; other inputs
    /// read meanwhile are kept for later.
    fn bus_error(&mut self, fallback: &str) -> String {
        let mut found = None;
        while let Ok(input) = self.rx.try_recv() {
            match input {
                Input::Bus(msg) if found.is_none() => match msg.view() {
                    gst::MessageView::Error(err) => {
                        tracing::debug!("player: {:?}", err.debug());
                        found = Some(format!("{fallback}: {}", err.error()));
                    }
                    _ => self.deferred.push_back(Input::Bus(msg)),
                },
                other => self.deferred.push_back(other),
            }
        }
        found.unwrap_or_else(|| fallback.to_owned())
    }

    /// Stops the pipeline (READY): no picture, nothing to play.
    fn halt(&mut self) {
        self.clear_awaited();
        if let Some(p) = self.gst.as_mut() {
            let _ = p.pipeline.set_state(gst::State::Ready);
            p.running = false;
            p.playing = false;
            p.at_eos = false;
        }
    }

    /// The start of the plan's last frame.
    fn last_frame_ns(&self) -> u64 {
        let frames = ns_to_frame_ceil(
            i64::try_from(self.duration_ns).unwrap_or(i64::MAX),
            self.fps,
        );
        frame_ns((frames - 1).max(0), self.fps).max(0) as u64
    }

    fn frame_period(&self) -> Duration {
        Duration::from_nanos(frame_ns(1, self.fps).max(1) as u64)
    }

    fn step_period(&self) -> Duration {
        self.frame_period().max(MIN_STEP_PERIOD)
    }

    /// Sends an accurate flushing seek to `at` and waits (in flight) for its
    /// frame.
    fn do_seek(&mut self, at: u64, kind: SeekKind, after: After, what: &'static str) {
        if self.duration_ns == 0 {
            return;
        }
        if !self.gst.as_ref().is_some_and(|p| p.running)
            && let Err(message) = self.start()
        {
            self.fail(message);
            return;
        }
        // The frame showing at `at` (the last one past the end): frame
        // starts keep the compositor's output on the canvas frames.
        let at = at.min(self.duration_ns);
        let frame = ns_to_frame_floor(i64::try_from(at).unwrap_or(i64::MAX), self.fps);
        let target = (frame_ns(frame, self.fps).max(0) as u64).min(self.last_frame_ns());
        let tolerance = (frame_ns(1, self.fps) / 2).max(1) as u64;
        let rate = if self.playing && self.rate > 0.0 {
            self.rate
        } else {
            1.0
        };
        let id = self.next_id;
        self.next_id += 1;
        let Some(p) = self.gst.as_mut() else {
            return;
        };
        let stop = match kind {
            SeekKind::Scrub => Some(gst::ClockTime::from_nseconds(
                (target + SCRUB_NS).min(self.duration_ns),
            )),
            SeekKind::Plain => None,
        };
        let confirmed = p.probe.is_some();
        if confirmed {
            *lock(&p.awaited) = Some(Awaited {
                id,
                target_ns: target,
                tolerance_ns: tolerance,
                flushed: false,
            });
        }
        // The stop is always set (none = the end): a seek that leaves it
        // keeps nlecomposition's segment stop, the end of the part of the
        // timeline it played last, and a later start fails.
        let event = gst::event::Seek::new(
            rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            gst::ClockTime::from_nseconds(target),
            gst::SeekType::Set,
            stop,
        );
        if !p.pipeline.send_event(event) {
            *lock(&p.awaited) = None;
            tracing::warn!("player: {what}: the pipeline refused the seek to {target} ns");
            if after == After::Loaded {
                self.emit(PlayerEvent::Loaded {
                    duration_ns: self.duration_ns,
                });
            }
            return;
        }
        p.rate = rate;
        p.at_eos = false;
        self.position = at;
        let after = match kind {
            SeekKind::Scrub => {
                let _ = p.pipeline.set_state(gst::State::Playing);
                p.playing = true;
                After::ScrubSound { at: target }
            }
            SeekKind::Plain => after,
        };
        self.flight = Some(Flight::Seek {
            id,
            deadline: Instant::now() + SEEK_TIMEOUT,
            after,
            what,
        });
        if !confirmed {
            self.on_shown(id);
        }
    }

    fn on_shown(&mut self, id: u64) {
        let Some(Flight::Seek {
            id: current, after, ..
        }) = self.flight
        else {
            return;
        };
        if current != id {
            return;
        }
        self.flight = None;
        self.failed = false;
        match after {
            After::Nothing => {}
            After::Loaded => self.emit(PlayerEvent::Loaded {
                duration_ns: self.duration_ns,
            }),
            After::ScrubSound { at } => {
                self.flight = Some(Flight::ScrubSound {
                    at,
                    until: Instant::now() + SCRUB_SOUND_TIMEOUT,
                });
            }
        }
    }

    /// A scrub's sound ended: pause and show the scrubbed frame again (a
    /// newer seek replaces that).
    fn end_scrub(&mut self, at: u64) {
        self.pause_pipeline();
        if self.seek.is_none() {
            self.seek = Some((at, SeekKind::Plain));
        }
    }

    fn pause_pipeline(&mut self) {
        let Some(p) = self.gst.as_mut() else {
            return;
        };
        if !p.playing {
            return;
        }
        let _ = p.pipeline.set_state(gst::State::Paused);
        p.playing = false;
        if let Some(pos) = p.pipeline.query_position::<gst::ClockTime>() {
            self.position = pos.nseconds().min(self.duration_ns);
        }
        if let Some(total) = self.late.take_total() {
            tracing::info!("player: {total} frame(s) dropped as late while playing (QoS)");
        }
    }

    fn poll_position(&mut self) {
        if let Some(pos) = self
            .gst
            .as_ref()
            .and_then(|p| p.pipeline.query_position::<gst::ClockTime>())
        {
            self.position = pos.nseconds().min(self.duration_ns);
        }
    }

    /// Makes the pipeline play, step or pause as asked.
    fn follow_play_state(&mut self, now: Instant) {
        let Some((running, at_eos, pipeline_rate)) =
            self.gst.as_ref().map(|p| (p.running, p.at_eos, p.rate))
        else {
            self.playing = false;
            return;
        };
        if !self.playing {
            self.pause_pipeline();
            return;
        }
        if self.duration_ns == 0 {
            // Nothing to play: it ended at once.
            self.playing = false;
            self.emit(PlayerEvent::Ended {
                reverse: self.rate < 0.0,
            });
            return;
        }
        if self.rate > 0.0 {
            self.stepping = None;
            if at_eos && running {
                self.playing = false;
                self.emit(PlayerEvent::Ended { reverse: false });
                return;
            }
            if !running || pipeline_rate != self.rate {
                // Not started, or another rate: a seek where playback is.
                let at = self.position;
                self.do_seek(at, SeekKind::Plain, After::Nothing, "changing the rate");
            }
            if let Some(p) = self.gst.as_mut()
                && p.running
                && !p.playing
            {
                let _ = p.pipeline.set_state(gst::State::Playing);
                p.playing = true;
            }
            return;
        }
        // Reverse: stepping with the pipeline paused.
        self.pause_pipeline();
        let period = self.step_period();
        let stepping = *self.stepping.get_or_insert(Stepping {
            origin_ns: self.position,
            origin: now,
            next: now,
        });
        if now < stepping.next {
            return;
        }
        if self.position == 0 {
            self.playing = false;
            self.stepping = None;
            self.emit(PlayerEvent::Ended { reverse: true });
            return;
        }
        let back = (now - stepping.origin).as_nanos() as f64 * -self.rate;
        let wanted = stepping.origin_ns.saturating_sub(back as u64);
        let frame = ns_to_frame_floor(i64::try_from(wanted).unwrap_or(i64::MAX), self.fps);
        let target = frame_ns(frame.max(0), self.fps).max(0) as u64;
        if let Some(stepping) = &mut self.stepping {
            stepping.next = now + period;
        }
        if target < self.position {
            self.do_seek(target, SeekKind::Plain, After::Nothing, "stepping back");
        }
    }

    // ----------------------------------------------------------- messages

    fn on_bus(&mut self, msg: &gst::Message) {
        use gst::MessageView as M;
        match msg.view() {
            M::Eos(_) => self.on_eos(),
            M::Error(err) => {
                let source = msg
                    .src()
                    .map(|s| s.path_string().to_string())
                    .unwrap_or_default();
                tracing::debug!("player: error from {source}: {:?}", err.debug());
                let message = err.error().to_string();
                if self.failed {
                    tracing::warn!("player: {message}");
                } else {
                    self.fail(message);
                }
            }
            M::Warning(w) => tracing::warn!("player: {} ({:?})", w.error(), w.debug()),
            M::Qos(qos) => self.late.count(qos),
            M::Latency(_) => {
                if let Some(p) = &self.gst
                    && p.pipeline.recalculate_latency().is_err()
                {
                    tracing::debug!("player: latency recalculation failed");
                }
            }
            _ => {}
        }
    }

    fn on_eos(&mut self) {
        match self.flight {
            Some(Flight::ScrubSound { at, .. })
            | Some(Flight::Seek {
                after: After::ScrubSound { at },
                ..
            }) => {
                self.flight = None;
                self.clear_awaited();
                self.end_scrub(at);
            }
            // From before the seek in flight, which flushes it.
            Some(Flight::Seek { .. }) => {}
            None => {
                let playing = self.playing_forward();
                if let Some(p) = self.gst.as_mut() {
                    p.at_eos = true;
                }
                if playing {
                    self.playing = false;
                    self.pause_pipeline();
                    self.position = self.duration_ns;
                    self.emit(PlayerEvent::Ended { reverse: false });
                }
            }
        }
    }

    fn clear_awaited(&self) {
        if let Some(p) = &self.gst {
            *lock(&p.awaited) = None;
        }
    }

    /// Reports `message` once and stays paused: the pipeline stops, waiting
    /// work is dropped; the next load (or a seek) starts it again.
    fn fail(&mut self, message: String) {
        self.playing = false;
        self.stepping = None;
        self.flight = None;
        self.halt();
        self.report_error(message);
    }

    fn report_error(&mut self, message: String) {
        if self.shared.stopping.load(Ordering::SeqCst) {
            // Cut short by `shutdown`: not an error anyone sees.
            tracing::debug!("player: while stopping: {message}");
            return;
        }
        tracing::warn!("player: {message}");
        self.failed = true;
        self.emit(PlayerEvent::Error { message });
    }

    fn stop(mut self) {
        if let Some(p) = self.gst.take() {
            if let Some(bus) = p.pipeline.bus() {
                bus.unset_sync_handler();
            }
            if let Some((pad, id)) = p.probe {
                pad.remove_probe(id);
            }
            if !ges_render::set_null_with_deadline(p.pipeline.clone().upcast(), STOP_TIMEOUT) {
                tracing::warn!(
                    "player: the pipeline didn't stop within {} s; abandoned",
                    STOP_TIMEOUT.as_secs()
                );
            }
        }
        if let Some(total) = self.late.take_total() {
            tracing::info!("player: {total} frame(s) dropped as late while playing (QoS)");
        }
        self.emit(PlayerEvent::Stopped);
    }
}

/// Watches what reaches `sink`: the first frame (or EOS) after the flush of
/// the awaited seek, at its time, completes it.
///
/// Frames shorter than `sliver_ns` are dropped. GES plays video in-points
/// 2 ms late ([`ges_render::MKV_INPOINT_NUDGE_NS`]), so a clip that plays
/// its file to the end runs out of frames 2 ms before the clip does, and
/// the compositor fills those 2 ms with a frame of what is below (the
/// canvas background): a flash at a cut, and the picture left after the
/// end of the timeline. Without it the sink holds the clip's last frame
/// there, as the export does.
fn watch_frames(
    sink: &gst::Element,
    awaited: AwaitedSlot,
    sliver_ns: Arc<AtomicU64>,
    tx: Sender<Input>,
) -> Option<(gst::Pad, gst::PadProbeId)> {
    let pad = sink.static_pad("sink")?;
    let mask = gst::PadProbeType::BUFFER
        | gst::PadProbeType::BUFFER_LIST
        | gst::PadProbeType::EVENT_DOWNSTREAM
        | gst::PadProbeType::EVENT_FLUSH;
    let id = pad.add_probe(mask, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = &info.data
            && buffer
                .duration()
                .is_some_and(|d| d.nseconds() < sliver_ns.load(Ordering::Relaxed))
        {
            tracing::trace!(
                "player: dropped a {:?} frame at {:?}",
                buffer.duration(),
                buffer.pts()
            );
            return gst::PadProbeReturn::Drop;
        }
        let mut slot = lock(&awaited);
        let Some(seek) = slot.as_mut() else {
            return gst::PadProbeReturn::Ok;
        };
        let at = |pts: Option<gst::ClockTime>| {
            pts.is_none_or(|pts| pts.nseconds().abs_diff(seek.target_ns) <= seek.tolerance_ns)
        };
        let done = match &info.data {
            Some(gst::PadProbeData::Event(event)) => match event.type_() {
                gst::EventType::FlushStop => {
                    seek.flushed = true;
                    false
                }
                gst::EventType::Eos => seek.flushed,
                _ => false,
            },
            Some(gst::PadProbeData::Buffer(buffer)) => seek.flushed && at(buffer.pts()),
            Some(gst::PadProbeData::BufferList(list)) => {
                seek.flushed && list.get(0).is_some_and(|buffer| at(buffer.pts()))
            }
            _ => false,
        };
        if done {
            let _ = tx.send(Input::Shown(seek.id));
            *slot = None;
        }
        gst::PadProbeReturn::Ok
    })?;
    Some((pad, id))
}

/// Commits `timeline` in a running pipeline and waits for GES to apply it,
/// at most `timeout` (less once `stopping`).
fn commit_and_wait(
    timeline: &ges::Timeline,
    timeout: Duration,
    stopping: &AtomicBool,
) -> Result<(), String> {
    let (tx, rx) = mpsc::sync_channel::<()>(1);
    let handler = timeline.connect_commited(move |_| {
        let _ = tx.try_send(());
    });
    let mut result = Ok(());
    if timeline.commit() {
        let deadline = Instant::now() + timeout;
        result = loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(()) => break Ok(()),
                Err(_) if Instant::now() < deadline && !stopping.load(Ordering::SeqCst) => {}
                Err(_) => {
                    break Err(format!(
                        "the new timeline didn't apply within {} s",
                        timeout.as_secs()
                    ));
                }
            }
        };
    }
    timeline.disconnect(handler);
    result
}

/// The audio sink for `out`.
fn sound_sink(out: AudioOut) -> Result<gst::Element, String> {
    match out {
        AudioOut::Silent => silent_sink(),
        AudioOut::System => system_sink("pulsesink").or_else(|why| {
            tracing::warn!("player: {why}; playing without sound");
            silent_sink()
        }),
    }
}

/// `fakesink sync=true`: plays in time, silently.
fn silent_sink() -> Result<gst::Element, String> {
    gst::ElementFactory::make("fakesink")
        .name("qrec-player-silence")
        .property("sync", true)
        .build()
        .map_err(|_| "the GStreamer element fakesink is missing".to_owned())
}

/// `factory` (`pulsesink`) as client `Qrec`, once it opened the sound
/// server: on a helper thread, within [`SOUND_OPEN_TIMEOUT`].
fn system_sink(factory: &str) -> Result<gst::Element, String> {
    let sink = gst::ElementFactory::make(factory)
        .name("qrec-player-sound")
        .build()
        .map_err(|_| format!("the GStreamer element {factory} is missing"))?;
    if sink.find_property("client-name").is_some() {
        sink.set_property("client-name", "Qrec");
    }
    let opening = sink.clone();
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("qrec-player-sound".into())
        .spawn(move || {
            let opened = opening.set_state(gst::State::Ready).is_ok();
            let _ = opening.set_state(gst::State::Null);
            let _ = tx.send(opened);
        })
        .map_err(|e| format!("can't open the sound output: {e}"))?;
    match rx.recv_timeout(SOUND_OPEN_TIMEOUT) {
        Ok(true) => Ok(sink),
        Ok(false) => Err(format!("{factory} can't open the sound output")),
        Err(_) => Err(format!(
            "the sound output didn't open within {} s",
            SOUND_OPEN_TIMEOUT.as_secs()
        )),
    }
}

/// Frames dropped as late (QoS, H-8), from the counts each element posts.
#[derive(Debug, Default)]
struct Late {
    /// Per element: the last cumulative count it reported.
    seen: BTreeMap<String, u64>,
    /// Not reported yet.
    pending: u64,
    /// Since playback started (for the log).
    total: u64,
    last_report: Option<Instant>,
}

impl Late {
    fn count(&mut self, qos: &gst::message::Qos) {
        let (_, dropped) = qos.stats();
        let gst::GenericFormattedValue::Buffers(Some(dropped)) = dropped else {
            return;
        };
        let dropped = *dropped;
        let source = qos
            .src()
            .map(|s| s.path_string().to_string())
            .unwrap_or_default();
        let before = self.seen.insert(source, dropped).unwrap_or(0);
        // A count that went down was reset (a flush): it counts from 0.
        let new = if dropped >= before {
            dropped - before
        } else {
            dropped
        };
        self.pending += new;
        self.total += new;
    }

    /// When the pending drops may be reported.
    fn due(&self) -> Option<Instant> {
        (self.pending > 0).then(|| {
            self.last_report
                .map_or_else(Instant::now, |at| at + LATE_PERIOD)
        })
    }

    /// The drops to report now, if any.
    fn report(&mut self, now: Instant) -> Option<u64> {
        if self.pending == 0 || self.last_report.is_some_and(|at| now < at + LATE_PERIOD) {
            return None;
        }
        self.last_report = Some(now);
        tracing::debug!("player: {} frame(s) dropped as late (QoS)", self.pending);
        Some(std::mem::take(&mut self.pending))
    }

    /// The drops since playback started, once.
    fn take_total(&mut self) -> Option<u64> {
        Some(std::mem::take(&mut self.total)).filter(|&n| n > 0)
    }
}

#[cfg(test)]
#[path = "player_tests.rs"]
mod tests;
