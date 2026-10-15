//! The capture engine thread (A§8.11).
//!
//! [`GstCaptureEngine`] is the `CaptureEngine` the Session holds: every
//! method only queues a [`Cmd`] for the engine thread, so the session
//! thread never waits on GStreamer. The engine thread owns every GStreamer
//! object; streaming threads (pad probes, the bus sync handler, caps
//! notifications) and the portal worker only send it `Cmd`s.
//!
//! A take moves through
//! `AwaitingPortal → Starting → Recording → Stopping → (CaptureStopped)`:
//!
//! - **Starting**: record bins are attached at T0 (the running time after
//!   attaching). `CaptureStarted` is sent once every filesink wrote its
//!   first buffer and the health window (`health_check_ms`) passed without
//!   errors, with each stream's `startOffsetNs` (known from its first
//!   buffer, so a crash later doesn't lose them); no data within
//!   `first_buffer_ms`, or any error, is `CaptureFailed` naming the source.
//! - **Recording**: `RecordClock` every 250 ms from the pipeline clock; an
//!   error attributable to one branch (its source or anything in its bin)
//!   EOSes that branch and sends `StreamLost`; a pipeline-wide error loses
//!   every stream (the core then stops).
//! - **Stopping**: T1 = the running time of the stop. Each bin closes when
//!   its source delivers a buffer at or after T1 (or after 500 ms):
//!   unlinked from the tee, filled to T1, EOS. A bin stopped before its
//!   first buffer (a stop while Starting) still passes that one buffer, and
//!   waits for it until the first-buffer deadline: closed empty, it would
//!   write a file nothing can read. Every filesink must see EOS
//!   within `eos_timeout_ms` (else `timed_out`), then the bins go to NULL
//!   within 5 s (else `EngineWedged`) and `CaptureStopped` reports each
//!   stream's `startOffsetNs`.
//!
//! The live pipeline stays up between takes while a live scene is wanted
//! (`set_live`); `start` builds it from the plan when needed.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use gst::prelude::*;
use qrec_core::ctx::CameraMode;
use qrec_core::model::{StreamKind, VideoSource};
use qrec_core::plans::{CapturePlan, LiveScene};
use qrec_core::protocol::RecordedStream;
use qrec_core::{Event, Fps, TakeId};

use super::fake::{fake_camera_modes, is_fake_audio, is_fake_camera, is_fake_screen};
use super::live::{BuildCtx, BuildError, Live, LiveKey, level_reading};
use super::portal::{OpenRequest, PortalWorker, ScreenShare};
use super::record::{RecordBin, attach_audio, attach_video};
use super::spec::{
    CameraKey, SourceSpec, audio_label, plan_sources, record_file_rel, scene_sources,
    start_offset_line, stop_line, stream_lost_line, video_label,
};
use super::util::{error_text, missing_element, object_path, running_time, set_null_with_deadline};
use super::{
    Backend, CLOCK_INTERVAL, EngineOptions, FILL_GRACE, LiveControl, PIPELINE_START_TIMEOUT,
    STALL_LIMIT, SideEvent, TEARDOWN_TIMEOUT,
};
use crate::deps::CaptureEngine;
use crate::gst_init::{self, InitError};
use crate::msg::Inbox;
use crate::paths::ProjectPaths;

/// How long `shutdown()` waits for the engine thread: a running take is
/// finalized first (EOS within the default 10 s, then teardown within 5 s).
const SHUTDOWN_WAIT: Duration = Duration::from_secs(16);

/// Messages to the engine thread.
pub(super) enum Cmd {
    SetLive(Option<LiveScene>, Inbox),
    Start(Box<CapturePlan>, Inbox),
    Stop {
        take: TakeId,
        cancel: bool,
    },
    Shutdown(Sender<()>),
    /// A bus message of the live pipeline with this generation.
    Bus(u64, gst::Message),
    /// A record-bin probe of the take with this tag.
    Probe(u64, ProbeEvent),
    /// A video source negotiated its size.
    SourceSize {
        generation: u64,
        source: usize,
        width: u32,
        height: u32,
    },
    /// The portal worker finished an open request.
    PortalDone {
        id: u64,
        results: Vec<(String, Result<ScreenShare, String>)>,
    },
    /// Test hook (fake backend only): make a source fail.
    Inject {
        source: String,
        failure: FakeFailure,
    },
    /// Test hook: what the live pipeline is right now.
    #[cfg(test)]
    Inspect(Sender<LiveInfo>),
}

/// The live pipeline as tests see it.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveInfo {
    /// Bumped by every rebuild; `None` without a pipeline.
    pub generation: Option<u64>,
    pub sources: Vec<SourceSpec>,
    pub preview: bool,
}

/// What a record bin's probes saw (index = position in the take's bins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProbeEvent {
    /// The first buffer at or after T0 passed.
    FirstPass(usize),
    /// The filesink got its first buffer.
    FirstWrite(usize),
    /// A buffer at or after T1 arrived (stop): time to close.
    ReachedEnd(usize),
    /// The bin is unlinked from its tee.
    Unlinked(usize),
    /// EOS reached the filesink.
    SinkEos(usize),
}

/// How a fake source fails (tests of `StreamLost`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeFailure {
    /// The source posts an error, like an unplugged camera.
    Error,
    /// The source ends its stream (EOS), like a closed screencast.
    Eos,
    /// The source goes silent without an error, like a camera that hangs.
    Stall,
}

/// The GStreamer capture engine. Cheap to call from the session thread.
pub struct GstCaptureEngine {
    tx: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
    /// The last inbox, for reporting when the engine thread is gone.
    out: Arc<Mutex<Option<Inbox>>>,
}

impl GstCaptureEngine {
    /// Starts the engine thread. GStreamer itself is initialized lazily, on
    /// the engine thread, by the first `set_live`/`start`.
    pub fn spawn(opts: EngineOptions) -> std::io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let out = Arc::new(Mutex::new(None));
        let engine = Engine::new(opts, tx.clone(), rx, out.clone());
        let shared = out.clone();
        let thread = thread::Builder::new()
            .name("qrec-capture".into())
            .spawn(move || {
                if let Err(panic) = catch_unwind(AssertUnwindSafe(move || engine.run())) {
                    let message = format!("the capture engine crashed: {}", panic_text(&panic));
                    tracing::error!("{message}");
                    let out = shared
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    if let Some(out) = out {
                        out.event(Event::EngineWedged { message });
                    }
                }
            })?;
        Ok(Self {
            tx,
            thread: Some(thread),
            out,
        })
    }

    /// Test hook: the live pipeline once every command sent so far is handled.
    #[cfg(test)]
    pub(super) fn inspect(&self) -> LiveInfo {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.tx.send(Cmd::Inspect(tx)).expect("the engine runs");
        rx.recv_timeout(Duration::from_secs(20))
            .expect("the engine answers")
    }

    /// Moves the live preview's layers during a drag (docs/GUI.md G§3.9).
    /// Not connected yet: the control it returns does nothing.
    pub fn control(&self) -> LiveControl {
        LiveControl::noop()
    }

    /// Fake backend only: makes the source `source` (an output, camera
    /// device or audio source name) fail while it runs.
    pub fn inject_fake_failure(&self, source: &str, failure: FakeFailure) {
        let _ = self.tx.send(Cmd::Inject {
            source: source.to_owned(),
            failure,
        });
    }

    fn remember(&self, out: &Inbox) {
        *self.out.lock().unwrap_or_else(PoisonError::into_inner) = Some(out.clone());
    }

    fn gone(&self) -> Option<Inbox> {
        self.out
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl CaptureEngine for GstCaptureEngine {
    fn set_live(&mut self, live: Option<LiveScene>, out: Inbox) {
        self.remember(&out);
        let _ = self.tx.send(Cmd::SetLive(live, out));
    }

    fn start(&mut self, plan: CapturePlan, out: Inbox) {
        self.remember(&out);
        let take = plan.take.clone();
        if self
            .tx
            .send(Cmd::Start(Box::new(plan), out.clone()))
            .is_err()
        {
            out.event(Event::CaptureFailed {
                take: Some(take),
                source: None,
                message: "the capture engine isn't running".into(),
                missing: None,
            });
        }
    }

    fn stop(&mut self, take: &TakeId, cancel: bool) {
        let sent = self.tx.send(Cmd::Stop {
            take: take.clone(),
            cancel,
        });
        if sent.is_err()
            && let Some(out) = self.gone()
        {
            out.event(Event::CaptureStopped {
                take: take.clone(),
                streams: Vec::new(),
                timed_out: true,
            });
        }
    }

    fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        let acked = self.tx.send(Cmd::Shutdown(ack_tx)).is_ok()
            && ack_rx.recv_timeout(SHUTDOWN_WAIT).is_ok();
        if acked || thread.is_finished() {
            let _ = thread.join();
        } else {
            tracing::warn!("the capture engine didn't stop in time; leaving it behind");
        }
    }
}

impl Drop for GstCaptureEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Stopping: when a bin whose source hasn't reached T1 closes anyway. One
/// that passed a buffer closes at `fill_by`; one that hasn't (stopped while
/// Starting) waits for its first buffer until `first_by` too, since a bin
/// closed empty writes a file nothing can read.
fn close_deadline(bin: &RecordBin, fill_by: Instant, first_by: Instant) -> Instant {
    if bin.first_running().is_some() {
        fill_by
    } else {
        fill_by.max(first_by)
    }
}

fn panic_text(panic: &Box<dyn Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

/// Where the take is.
enum Phase {
    /// Accepted, nothing attached yet (left within the same command).
    New,
    /// Waiting for the portal to share screens.
    AwaitingPortal {
        deadline: Instant,
    },
    /// Bins attached at T0; waiting for first writes and the health window.
    Starting {
        health_until: Instant,
        first_buffer_by: Instant,
    },
    Recording {
        next_clock: Instant,
    },
    Stopping {
        /// Bins whose source didn't reach T1 by then close anyway.
        fill_by: Instant,
        /// A bin that hasn't passed its first buffer (stopped while
        /// Starting) waits for it until then: a bin closed empty writes a
        /// file nothing can read.
        first_by: Instant,
        eos_by: Instant,
    },
}

/// The `recorder.log` of a take.
struct TakeLog {
    file: Option<File>,
}

impl TakeLog {
    fn open(path: Option<PathBuf>) -> Self {
        let file = path.and_then(|path| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| tracing::warn!("can't open {}: {e}", path.display()))
                .ok()
        });
        Self { file }
    }

    fn line(&mut self, text: impl AsRef<str>) {
        tracing::debug!("capture: {}", text.as_ref());
        if let Some(file) = &mut self.file {
            let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
            let _ = writeln!(file, "{now} {}", text.as_ref());
        }
    }
}

/// The take being recorded.
struct TakeRun {
    plan: CapturePlan,
    /// Probe messages carry it; stale ones are ignored.
    tag: u64,
    log: TakeLog,
    phase: Phase,
    t0: u64,
    t1: Option<u64>,
    bins: Vec<RecordBin>,
    cancel: bool,
    /// `QREC_FAKE_EOS_DELAY_MS`: a stopped take ends no sooner than this.
    hold_until: Option<Instant>,
}

struct Engine {
    opts: EngineOptions,
    project: ProjectPaths,
    tx: Sender<Cmd>,
    rx: Receiver<Cmd>,
    /// Commands put aside while a pipeline started (handled next, in order).
    deferred: VecDeque<Cmd>,
    out: Option<Inbox>,
    shared_out: Arc<Mutex<Option<Inbox>>>,
    gst: Option<Result<(), InitError>>,
    wedged: Option<String>,
    desired: Option<LiveScene>,
    live: Option<Live>,
    generation: u64,
    take: Option<TakeRun>,
    tags: u64,
    portal: Option<PortalWorker>,
    casts: BTreeMap<String, ScreenShare>,
    portal_seq: u64,
    /// Outputs asked of the portal and not answered yet.
    in_flight: BTreeSet<String>,
    /// Outputs the portal refused for the current live scene (not asked
    /// again until the scene changes).
    refused: BTreeMap<String, String>,
    shutdown_ack: Option<Sender<()>>,
    exiting: bool,
    done: bool,
}

impl Engine {
    fn new(
        opts: EngineOptions,
        tx: Sender<Cmd>,
        rx: Receiver<Cmd>,
        shared_out: Arc<Mutex<Option<Inbox>>>,
    ) -> Self {
        Self {
            project: ProjectPaths::new(&opts.project_dir),
            opts,
            tx,
            rx,
            deferred: VecDeque::new(),
            out: None,
            shared_out,
            gst: None,
            wedged: None,
            desired: None,
            live: None,
            generation: 0,
            take: None,
            tags: 0,
            portal: None,
            casts: BTreeMap::new(),
            portal_seq: 0,
            in_flight: BTreeSet::new(),
            refused: BTreeMap::new(),
            shutdown_ack: None,
            exiting: false,
            done: false,
        }
    }

    fn run(mut self) {
        while !self.done {
            let cmd = match self.deferred.pop_front() {
                Some(cmd) => Some(cmd),
                None => {
                    let received = match self.next_deadline() {
                        Some(at) => self
                            .rx
                            .recv_timeout(at.saturating_duration_since(Instant::now())),
                        None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                    };
                    match received {
                        Ok(cmd) => Some(cmd),
                        Err(RecvTimeoutError::Timeout) => None,
                        // Unreachable while `self.tx` lives; stop cleanly anyway.
                        Err(RecvTimeoutError::Disconnected) => {
                            self.exiting = true;
                            None
                        }
                    }
                }
            };
            if let Some(cmd) = cmd {
                self.handle(cmd);
            }
            self.tick();
            self.maybe_finish_shutdown();
        }
    }

    fn emit(&self, event: Event) {
        if let Some(out) = &self.out {
            out.event(event);
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::SetLive(scene, out) => {
                self.set_out(out);
                self.set_live(scene);
            }
            Cmd::Start(plan, out) => {
                self.set_out(out);
                self.start(*plan);
            }
            Cmd::Stop { take, cancel } => self.stop(take, cancel),
            Cmd::Shutdown(ack) => self.begin_shutdown(ack),
            Cmd::Bus(generation, msg) => self.on_bus(generation, &msg),
            Cmd::Probe(tag, event) => self.on_probe(tag, event),
            Cmd::SourceSize {
                generation,
                source,
                width,
                height,
            } => self.on_source_size(generation, source, width, height),
            Cmd::PortalDone { id, results } => {
                tracing::debug!("capture: portal answer {id}");
                self.on_portal(results);
            }
            Cmd::Inject { source, failure } => self.inject(&source, failure),
            #[cfg(test)]
            Cmd::Inspect(reply) => {
                let info = LiveInfo {
                    generation: self.live.as_ref().map(|l| l.generation),
                    sources: self
                        .live
                        .as_ref()
                        .map(|l| l.sources.iter().map(|s| s.spec.clone()).collect())
                        .unwrap_or_default(),
                    preview: self.live.as_ref().is_some_and(|l| l.preview.is_some()),
                };
                let _ = reply.send(info);
            }
        }
    }

    fn set_out(&mut self, out: Inbox) {
        *self
            .shared_out
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(out.clone());
        self.out = Some(out);
    }

    fn ensure_gst(&mut self) -> Result<(), InitError> {
        if let Some(result) = &self.gst {
            return result.clone();
        }
        let result = gst_init::init();
        self.gst = Some(result.clone());
        result
    }

    fn wedge(&mut self, message: String) {
        if self.wedged.is_none() {
            tracing::error!("capture: {message}");
            self.wedged = Some(message.clone());
            self.emit(Event::EngineWedged { message });
        }
    }

    // ------------------------------------------------------------ live

    /// The core sends the live scene it wants whenever that changes (A§8.11);
    /// an equal scene with a healthy pipeline changes nothing.
    fn set_live(&mut self, scene: Option<LiveScene>) {
        if scene == self.desired && self.live_is_healthy() {
            return;
        }
        if scene.as_ref().map(|s| &s.scene) != self.desired.as_ref().map(|s| &s.scene) {
            // New sources may be picked differently in the dialog: ask again.
            self.refused.clear();
        }
        self.desired = scene;
        if self.take.is_some() {
            // Layout changes show now; source changes wait for the take's end.
            if let (Some(live), Some(scene)) = (&self.live, &self.desired) {
                live.apply_layout(scene);
            }
            return;
        }
        self.apply_desired();
    }

    /// The pipeline matches `desired`: none wanted and none running, or one
    /// running with every source alive.
    fn live_is_healthy(&self) -> bool {
        match (&self.desired, &self.live) {
            (None, None) => true,
            (Some(_), Some(live)) => live.sources.iter().all(|s| s.dead.is_none()),
            _ => false,
        }
    }

    /// Makes the live pipeline match `desired` (no take running).
    fn apply_desired(&mut self) {
        let Some(scene) = self.desired.clone() else {
            self.teardown_live();
            self.close_unused_casts(&[]);
            return;
        };
        if self.wedged.is_some() {
            return;
        }
        if let Err(e) = self.ensure_gst() {
            self.side(SideEvent::LiveError(e.to_string()));
            if e == InitError::Stuck {
                self.wedge(e.to_string());
            }
            return;
        }
        // Screens the portal refused stay out of the preview until the
        // scene changes; the rest still shows.
        let specs: Vec<SourceSpec> = self
            .scene_specs(&scene)
            .into_iter()
            .filter(|s| !matches!(s, SourceSpec::Screen { output } if self.refused.contains_key(output)))
            .collect();
        let key = LiveKey::of(specs.clone(), Some(&scene));
        if let Some(live) = &self.live
            && live.key == key
            && live.sources.iter().all(|s| s.dead.is_none())
        {
            live.apply_layout(&scene);
            return;
        }
        match self.await_casts(&specs) {
            Ok(false) => {}
            // The portal answers later (`on_portal` comes back here).
            Ok(true) => return,
            Err(message) => {
                for output in self.missing_casts(&specs) {
                    self.refused.insert(output, message.clone());
                }
                self.side(SideEvent::LiveError(message));
                return self.apply_desired();
            }
        }
        match self.rebuild_live(&specs, Some(&scene)) {
            Ok(()) => {
                if let Some(live) = &self.live {
                    live.apply_layout(&scene);
                }
            }
            Err(e) => {
                let message = match &e.source {
                    Some(spec) => format!("{}: {}", spec.describe(), e.message),
                    None => e.message.clone(),
                };
                tracing::warn!("capture: the live pipeline can't start: {message}");
                self.side(SideEvent::LiveError(message));
            }
        }
        self.close_unused_casts(&specs);
    }

    /// The scene's sources, cameras with the mode the engine negotiates.
    fn scene_specs(&self, scene: &LiveScene) -> Vec<SourceSpec> {
        scene_sources(&scene.scene)
            .into_iter()
            .map(|spec| match spec {
                SourceSpec::Camera { device, mode: None } => {
                    let mode = self.camera_mode(&device, scene.canvas.fps);
                    SourceSpec::Camera { device, mode }
                }
                other => other,
            })
            .collect()
    }

    /// The camera mode rule of `capture_plan` applied to the device's modes.
    fn camera_mode(&self, device: &str, fps: Fps) -> Option<CameraKey> {
        let modes: Vec<CameraMode> = match self.opts.backend {
            Backend::Fake => fake_camera_modes(),
            Backend::Real => self
                .opts
                .devices
                .as_ref()
                .map(|d| d.camera_modes(device))
                .unwrap_or_default(),
        };
        if modes.is_empty() {
            return None;
        }
        // The rule lives in the core; never let a panic there (a stub, a
        // bug) take the engine down, and don't retry one that panicked.
        static BROKEN: AtomicBool = AtomicBool::new(false);
        if BROKEN.load(Ordering::Relaxed) {
            return None;
        }
        match catch_unwind(|| qrec_core::capture_plan::choose_camera_mode(&modes, fps)) {
            Ok(mode) => mode.as_ref().map(CameraKey::from),
            Err(_) => {
                BROKEN.store(true, Ordering::Relaxed);
                tracing::warn!("capture: the camera mode rule failed; cameras pick their own mode");
                None
            }
        }
    }

    fn side(&self, event: SideEvent) {
        if let Some(side) = &self.opts.side {
            let _ = side.send(event);
        }
    }

    /// Replaces the live pipeline with one for `specs` and starts it.
    fn rebuild_live(
        &mut self,
        specs: &[SourceSpec],
        scene: Option<&LiveScene>,
    ) -> Result<(), BuildError> {
        self.teardown_live();
        if self.wedged.is_some() {
            return Err(BuildError {
                source: None,
                message: self.wedged.clone().unwrap_or_default(),
            });
        }
        if let Some(spec) = specs.iter().find(|s| !self.backend_has(s)) {
            return Err(BuildError {
                source: Some(spec.clone()),
                message: format!("there is no fake {}", spec.describe()),
            });
        }
        self.generation += 1;
        let ctx = BuildCtx {
            backend: self.opts.backend,
            generation: self.generation,
            tx: &self.tx,
            side: self.opts.side.as_ref(),
            casts: &self.casts,
        };
        let live = Live::build(specs, scene, &ctx)?;
        let generation = live.generation;
        let pipeline = live.pipeline.clone();
        self.live = Some(live);
        if let Err(message) = self.start_pipeline(&pipeline, generation) {
            let source = self.collect_start_error(generation);
            self.teardown_live();
            return Err(match source {
                Some((spec, detail)) => BuildError {
                    source: spec,
                    message: detail,
                },
                None => BuildError {
                    source: None,
                    message,
                },
            });
        }
        Ok(())
    }

    /// Whether the fake backend has this device (the real one tries anything).
    fn backend_has(&self, spec: &SourceSpec) -> bool {
        match (self.opts.backend, spec) {
            (Backend::Real, _) => true,
            (Backend::Fake, SourceSpec::Screen { output }) => is_fake_screen(output),
            (Backend::Fake, SourceSpec::Camera { device, .. }) => is_fake_camera(device),
            (Backend::Fake, SourceSpec::Audio { device }) => is_fake_audio(device),
        }
    }

    fn start_pipeline(&self, pipeline: &gst::Pipeline, generation: u64) -> Result<(), String> {
        let _ = generation;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| "the capture pipeline failed to start".to_owned())?;
        let (result, current, _) = pipeline.state(gst::ClockTime::from_nseconds(
            PIPELINE_START_TIMEOUT.as_nanos() as u64,
        ));
        match result {
            Err(_) => Err("the capture pipeline failed to start".into()),
            Ok(_) if current == gst::State::Playing => Ok(()),
            Ok(_) => Err(format!(
                "the capture pipeline didn't start within {} s",
                PIPELINE_START_TIMEOUT.as_secs()
            )),
        }
    }

    /// After a failed start: the first error the new pipeline posted, and
    /// the source it came from. Other commands are kept for later.
    fn collect_start_error(&mut self, generation: u64) -> Option<(Option<SourceSpec>, String)> {
        let deadline = Instant::now() + Duration::from_millis(200);
        let mut found = None;
        loop {
            let cmd = match self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(cmd) => cmd,
                Err(_) => break,
            };
            match cmd {
                Cmd::Bus(g, msg) if g == generation && found.is_none() => {
                    if let gst::MessageView::Error(err) = msg.view() {
                        let (message, detailed) = error_text(&err.error(), err.debug().as_deref());
                        tracing::warn!("capture: start error: {detailed}");
                        let spec = msg.src().and_then(|src| {
                            let live = self.live.as_ref()?;
                            let index = live.source_of(&object_path(src))?;
                            Some(live.sources[index].spec.clone())
                        });
                        found = Some((spec, message));
                    }
                }
                Cmd::Bus(..) => {}
                other => self.deferred.push_back(other),
            }
            if found.is_some() {
                break;
            }
        }
        found
    }

    fn teardown_live(&mut self) {
        if let Some(live) = self.live.take() {
            let ok = set_null_with_deadline(vec![live.pipeline.upcast()], TEARDOWN_TIMEOUT);
            if !ok {
                self.wedge(
                    "the capture engine is stuck stopping the live pipeline (GPU driver?); restart Qrec"
                        .into(),
                );
            }
        }
    }

    // ------------------------------------------------------------ portal

    /// Screens of `specs` that need a portal session (real backend; the fake
    /// one's screens only with `QREC_FAKE_PORTAL`, whose portal refuses them).
    fn missing_casts(&self, specs: &[SourceSpec]) -> Vec<String> {
        let portal = match self.opts.backend {
            Backend::Real => true,
            Backend::Fake => self.opts.fake.portal.is_some(),
        };
        if !portal {
            return Vec::new();
        }
        specs
            .iter()
            .filter_map(|s| match s {
                SourceSpec::Screen { output } if !self.casts.contains_key(output) => {
                    Some(output.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// Asks the portal for the screens of `specs` that have no session and
    /// aren't asked for already. `Ok(true)` = an answer is pending; `Err` =
    /// the portal can't be asked.
    fn await_casts(&mut self, specs: &[SourceSpec]) -> Result<bool, String> {
        let missing = self.missing_casts(specs);
        if missing.is_empty() {
            return Ok(false);
        }
        let new: Vec<String> = missing
            .into_iter()
            .filter(|output| !self.in_flight.contains(output))
            .collect();
        if !new.is_empty() {
            self.request_casts(new)?;
        }
        Ok(true)
    }

    fn request_casts(&mut self, outputs: Vec<String>) -> Result<(), String> {
        if let (Backend::Fake, Some(portal)) = (self.opts.backend, self.opts.fake.portal) {
            // QREC_FAKE_PORTAL: refused, answered later like the real
            // portal's dialog (through the queue, as its worker does).
            self.portal_seq += 1;
            let results = outputs
                .iter()
                .map(|output| (output.clone(), Err(portal.refusal(output))))
                .collect();
            let _ = self.tx.send(Cmd::PortalDone {
                id: self.portal_seq,
                results,
            });
            self.in_flight.extend(outputs);
            return Ok(());
        }
        if self.portal.is_none() {
            let worker = PortalWorker::spawn().map_err(|e| {
                tracing::error!("capture: can't start the portal thread: {e}");
                "can't reach the screen-share portal".to_owned()
            })?;
            self.portal = Some(worker);
        }
        self.portal_seq += 1;
        let screens = self
            .opts
            .devices
            .as_ref()
            .map(|d| d.screens())
            .unwrap_or_default();
        let request = OpenRequest {
            id: self.portal_seq,
            outputs: outputs.clone(),
            screens,
            tokens: self.opts.portal_tokens.clone(),
            timeout: self.opts.portal_timeout,
            register_app_id: self.opts.register_app_id,
        };
        let sent = self
            .portal
            .as_ref()
            .is_some_and(|worker| worker.open(request, self.tx.clone()));
        if !sent {
            return Err("can't reach the screen-share portal".to_owned());
        }
        self.in_flight.extend(outputs);
        Ok(())
    }

    fn on_portal(&mut self, results: Vec<(String, Result<ScreenShare, String>)>) {
        let mut failures = Vec::new();
        for (output, result) in results {
            self.in_flight.remove(&output);
            match result {
                Ok(cast) => {
                    self.refused.remove(&output);
                    if let Some(old) = self.casts.insert(output, cast)
                        && let Some(worker) = &self.portal
                    {
                        worker.close(vec![old]);
                    }
                }
                Err(message) => failures.push((output, message)),
            }
        }
        let awaiting = matches!(
            self.take.as_ref().map(|t| &t.phase),
            Some(Phase::AwaitingPortal { .. })
        );
        if awaiting {
            let needed = |output: &str| {
                self.take.as_ref().and_then(|run| {
                    run.plan.video.iter().find_map(|rec| match &rec.source {
                        VideoSource::Screen { output: o, .. } if o == output => {
                            Some(video_label(rec))
                        }
                        _ => None,
                    })
                })
            };
            let failed = failures
                .iter()
                .find_map(|(output, message)| needed(output).map(|label| (label, message.clone())));
            for (output, message) in &failures {
                self.refused.insert(output.clone(), message.clone());
            }
            match failed {
                Some((label, message)) => self.fail_take(Some(label), message),
                // Every screen it needs may be shared now (or still pending).
                None => self.prepare_take(),
            }
        } else {
            for (output, message) in failures {
                self.side(SideEvent::LiveError(format!("screen {output}: {message}")));
                self.refused.insert(output, message);
            }
            if self.take.is_none() {
                self.apply_desired();
            }
        }
    }

    /// Closes portal sessions no source in `keep` uses (none while recording).
    fn close_unused_casts(&mut self, keep: &[SourceSpec]) {
        if self.take.is_some() || self.casts.is_empty() {
            return;
        }
        let unused: Vec<String> = self
            .casts
            .keys()
            .filter(|output| {
                !keep
                    .iter()
                    .any(|s| matches!(s, SourceSpec::Screen { output: o } if o == *output))
            })
            .cloned()
            .collect();
        let casts: Vec<ScreenShare> = unused
            .iter()
            .filter_map(|output| self.casts.remove(output))
            .collect();
        if let Some(worker) = &self.portal {
            worker.close(casts);
        }
    }

    // ------------------------------------------------------------ takes

    fn start(&mut self, plan: CapturePlan) {
        let take = plan.take.clone();
        let fail = |engine: &mut Engine, source: Option<String>, message: String| {
            engine.emit(Event::CaptureFailed {
                take: Some(take.clone()),
                source,
                missing: missing_element(&message),
                message,
            });
        };
        if let Some(message) = self.wedged.clone() {
            return fail(self, None, message);
        }
        if let Some(run) = &self.take {
            if run.plan.take == take {
                tracing::warn!("capture: ignoring a second start of {take}");
                return;
            }
            let busy = format!("{} is still being recorded or finalized", run.plan.take);
            return fail(self, None, busy);
        }
        if self.exiting {
            return fail(self, None, "the capture engine is shutting down".into());
        }
        if plan.video.is_empty() && plan.audio.is_empty() {
            return fail(self, None, "the scene has nothing to record".into());
        }
        if let Err(e) = self.ensure_gst() {
            let stuck = e == InitError::Stuck;
            fail(self, None, e.to_string());
            if stuck {
                self.wedge(e.to_string());
            }
            return;
        }
        match self.project.resolve(&plan.dir) {
            Ok(dir) if dir.is_dir() => {}
            Ok(dir) => {
                return fail(
                    self,
                    None,
                    format!("the take folder {} is missing", dir.display()),
                );
            }
            Err(e) => return fail(self, None, e.message),
        }
        let files = plan
            .video
            .iter()
            .map(|r| (r.file.as_str(), video_label(r)))
            .chain(plan.audio.iter().map(|r| (r.file.as_str(), audio_label(r))));
        for (file, label) in files {
            let rel = record_file_rel(&plan.dir, file);
            match self.project.resolve(&rel) {
                Ok(path) if path.exists() || path.is_symlink() => {
                    return fail(
                        self,
                        Some(label),
                        format!("{rel} already exists; recordings never overwrite files"),
                    );
                }
                Ok(_) => {}
                Err(e) => return fail(self, Some(label), e.message),
            }
        }
        let log = TakeLog::open(self.project.resolve(&plan.log).ok());
        self.tags += 1;
        let mut run = TakeRun {
            plan,
            tag: self.tags,
            log,
            phase: Phase::New,
            t0: 0,
            t1: None,
            bins: Vec::new(),
            cancel: false,
            hold_until: None,
        };
        run.log.line(format!(
            "start {} ({:?} backend): {}",
            run.plan.take,
            self.opts.backend,
            serde_json::to_string(&run.plan).unwrap_or_default()
        ));
        self.take = Some(run);
        self.prepare_take();
    }

    /// Makes sure the live pipeline has every source of the plan (asking
    /// the portal first if needed), then attaches the record bins.
    fn prepare_take(&mut self) {
        let Some(run) = &self.take else { return };
        let wanted = plan_sources(&run.plan);
        if let Some(live) = &self.live
            && live.serves_all(&wanted)
        {
            return self.attach_take();
        }
        // The plan's screens must be shared first (one dialog per screen,
        // never twice for the same one).
        match self.await_casts(&wanted) {
            Ok(false) => {}
            Ok(true) => {
                let count = self.missing_casts(&wanted).len().max(1) as u32;
                let wait = self.opts.portal_timeout * count + Duration::from_secs(10);
                if let Some(run) = &mut self.take
                    && !matches!(run.phase, Phase::AwaitingPortal { .. })
                {
                    run.log.line("waiting for the screen-share portal");
                    run.phase = Phase::AwaitingPortal {
                        deadline: Instant::now() + wait,
                    };
                }
                return;
            }
            Err(message) => return self.fail_take(None, message),
        }
        // Rebuild with the plan's sources plus what the live scene shows
        // (its screens only if they're shared already).
        let mut specs = wanted.clone();
        if let Some(scene) = self.desired.clone() {
            for spec in self.scene_specs(&scene) {
                let unshared = !self.missing_casts(std::slice::from_ref(&spec)).is_empty();
                if !unshared && !specs.iter().any(|s| s.same_device(&spec)) {
                    specs.push(spec);
                }
            }
        }
        let scene = self.desired.clone();
        match self.rebuild_live(&specs, scene.as_ref()) {
            Ok(()) => {
                if let (Some(live), Some(scene)) = (&self.live, &scene) {
                    live.apply_layout(scene);
                }
                self.attach_take();
            }
            Err(e) => {
                let source = e.source.as_ref().map(|spec| self.label_for(spec));
                self.fail_take(source, e.message);
            }
        }
    }

    /// The recorded stream(s) a source feeds, as a label.
    fn label_for(&self, spec: &SourceSpec) -> String {
        let Some(run) = &self.take else {
            return spec.describe();
        };
        let video = run
            .plan
            .video
            .iter()
            .filter(|r| SourceSpec::of_record(r).same_device(spec))
            .map(video_label);
        let audio = run
            .plan
            .audio
            .iter()
            .filter(|r| SourceSpec::of_audio(r).same_device(spec))
            .map(audio_label);
        let labels: Vec<String> = video.chain(audio).collect();
        if labels.is_empty() {
            spec.describe()
        } else {
            labels.join(", ")
        }
    }

    fn attach_take(&mut self) {
        let (Some(live), Some(run)) = (&self.live, &mut self.take) else {
            return;
        };
        let mut failure: Option<(String, String)> = None;
        let plan = run.plan.clone();
        let video = plan.video.iter().map(|rec| {
            (
                SourceSpec::of_record(rec),
                video_label(rec),
                record_file_rel(&plan.dir, &rec.file),
                Some(rec),
                None,
            )
        });
        let audio = plan.audio.iter().map(|rec| {
            (
                SourceSpec::of_audio(rec),
                audio_label(rec),
                record_file_rel(&plan.dir, &rec.file),
                None,
                Some(rec),
            )
        });
        for (spec, label, rel, vrec, arec) in video.chain(audio) {
            let index = run.bins.len();
            let Some(source) = live.find(&spec) else {
                failure = Some((label, format!("{} isn't available", spec.describe())));
                break;
            };
            let location = match self.project.resolve(&rel) {
                Ok(path) => path,
                Err(e) => {
                    failure = Some((label, e.message));
                    break;
                }
            };
            let tee = &live.sources[source].tee;
            let attached = match (vrec, arec) {
                (Some(rec), _) => attach_video(
                    &live.pipeline,
                    tee,
                    source,
                    index,
                    rec,
                    plan.fps,
                    &location,
                    rel,
                    label.clone(),
                    &self.tx,
                    run.tag,
                ),
                (_, Some(rec)) => attach_audio(
                    &live.pipeline,
                    tee,
                    source,
                    index,
                    rec,
                    &location,
                    rel,
                    label.clone(),
                    &self.tx,
                    run.tag,
                ),
                _ => unreachable!("every stream is video or audio"),
            };
            match attached {
                Ok(bin) => run.bins.push(bin),
                Err(message) => {
                    failure = Some((label, message));
                    break;
                }
            }
        }
        if let Some((label, message)) = failure {
            return self.fail_take(Some(label), message);
        }
        let Some(t0) = running_time(&live.pipeline) else {
            return self.fail_take(None, "the capture pipeline has no clock".into());
        };
        for bin in &run.bins {
            bin.probe.t0.store(t0, Ordering::SeqCst);
        }
        run.t0 = t0;
        let now = Instant::now();
        run.phase = Phase::Starting {
            health_until: now + Duration::from_millis(u64::from(plan.health_check_ms)),
            first_buffer_by: now + Duration::from_millis(u64::from(plan.first_buffer_ms)),
        };
        let files: Vec<&str> = run.bins.iter().map(|b| b.file.as_str()).collect();
        run.log
            .line(format!("attached at T0 = {t0} ns: {}", files.join(", ")));
        // A still screen sends nothing until it changes: its bridge sends
        // its last frame now, so its bins start at T0 (A§8.11).
        kick_screens(live, &run.bins);
    }

    /// Fails the take being started: tears its bins down, `CaptureFailed`.
    fn fail_take(&mut self, source: Option<String>, message: String) {
        let Some(mut run) = self.take.take() else {
            return;
        };
        run.log.line(format!(
            "failed: {}{message}",
            source
                .as_deref()
                .map(|s| format!("{s}: "))
                .unwrap_or_default()
        ));
        let ok = self.teardown_bins(&mut run.bins, false);
        self.emit(Event::CaptureFailed {
            take: Some(run.plan.take.clone()),
            source,
            missing: missing_element(&message),
            message,
        });
        if !ok {
            self.wedge(
                "the capture engine is stuck tearing down a take (GPU driver?); restart Qrec"
                    .into(),
            );
        }
        self.after_take();
    }

    fn stop(&mut self, take: TakeId, cancel: bool) {
        let hold = self.fake_eos_delay();
        let matches = self.take.as_ref().is_some_and(|run| run.plan.take == take);
        if !matches {
            // A take that already failed or ended (or was never seen): the
            // core waits for `CaptureStopped`, so always answer.
            tracing::debug!("capture: stop of {take}, which isn't recording");
            self.emit(Event::CaptureStopped {
                take,
                streams: Vec::new(),
                timed_out: false,
            });
            return;
        }
        let Some(run) = &mut self.take else { return };
        run.cancel = cancel;
        match run.phase {
            Phase::New | Phase::AwaitingPortal { .. } => {
                run.log.line("stopped before the portal answered");
                let take = run.plan.take.clone();
                self.take = None;
                self.emit(Event::CaptureStopped {
                    take,
                    streams: Vec::new(),
                    timed_out: false,
                });
                self.after_take();
            }
            Phase::Stopping { .. } => {}
            Phase::Starting { .. } | Phase::Recording { .. } => {
                // A still screen's bins reach T1 at once: a repeat stamped
                // before it passes, one after it closes them (A§8.11).
                if let Some(live) = &self.live {
                    kick_screens(live, &run.bins);
                }
                let pipeline = self.live.as_ref().map(|l| l.pipeline.clone());
                let t1 = pipeline
                    .as_ref()
                    .and_then(running_time)
                    .unwrap_or(run.t0)
                    .max(run.t0);
                run.t1 = Some(t1);
                for bin in &run.bins {
                    bin.probe.t1.store(t1, Ordering::SeqCst);
                }
                if let Some(live) = &self.live {
                    kick_screens(live, &run.bins);
                }
                let now = Instant::now();
                // Stopped while Starting: a source may not have delivered its
                // first buffer yet; it gets the start's first-buffer window.
                let first_by = match run.phase {
                    Phase::Starting {
                        first_buffer_by, ..
                    } => first_buffer_by,
                    _ => now,
                };
                run.phase = Phase::Stopping {
                    fill_by: now + FILL_GRACE,
                    first_by,
                    eos_by: now + Duration::from_millis(u64::from(run.plan.eos_timeout_ms)),
                };
                run.log
                    .line(stop_line(cancel, t1, (t1 - run.t0) / 1_000_000));
                if let Some(delay) = hold {
                    run.hold_until = Some(now + delay);
                    run.log.line(format!(
                        "QREC_FAKE_EOS_DELAY_MS: the files close {} ms late",
                        delay.as_millis()
                    ));
                }
                self.close_ready_bins();
            }
        }
    }

    /// `QREC_FAKE_EOS_DELAY_MS` (fake backend only).
    fn fake_eos_delay(&self) -> Option<Duration> {
        match self.opts.backend {
            Backend::Fake => self.opts.fake.eos_delay,
            Backend::Real => None,
        }
    }

    /// Closes the bins whose source already reached T1 (and lost ones).
    fn close_ready_bins(&mut self) {
        let Some(run) = &mut self.take else { return };
        let tag = run.tag;
        for bin in &mut run.bins {
            let reached = bin.probe.lock().reached_end;
            if reached || bin.lost.is_some() {
                bin.begin_close(&self.tx, tag);
            }
        }
        self.check_stopped();
    }

    fn on_probe(&mut self, tag: u64, event: ProbeEvent) {
        let Some(run) = &mut self.take else { return };
        if run.tag != tag {
            return;
        }
        match event {
            ProbeEvent::FirstPass(i) => {
                if let Some(bin) = run.bins.get(i) {
                    let first = bin.first_running().unwrap_or(run.t0);
                    run.log.line(format!(
                        "{}: first buffer at T0 + {:.3} ms",
                        bin.id,
                        first.saturating_sub(run.t0) as f64 / 1e6
                    ));
                }
            }
            ProbeEvent::FirstWrite(i) => {
                if let Some(bin) = run.bins.get(i) {
                    run.log.line(format!("{}: writing {}", bin.id, bin.file));
                }
            }
            ProbeEvent::ReachedEnd(i) => {
                if matches!(run.phase, Phase::Stopping { .. })
                    && let Some(bin) = run.bins.get_mut(i)
                {
                    bin.begin_close(&self.tx, tag);
                }
            }
            ProbeEvent::Unlinked(i) => {
                let fps = run.plan.fps;
                let stopping = matches!(run.phase, Phase::Stopping { .. });
                if let Some(bin) = run.bins.get_mut(i)
                    && !bin.closed
                {
                    bin.release_tee_pad();
                    let fill = stopping && bin.lost.is_none();
                    let filled = bin.finish_close(fps, fill);
                    let (passed, last) = {
                        let state = bin.probe.lock();
                        (state.passed, state.last)
                    };
                    run.log.line(format!(
                        "{}: closed after {passed} buffers (last at T0 + {:.3} ms), {filled} fill frames, EOS sent",
                        bin.id,
                        last.unwrap_or(run.t0).saturating_sub(run.t0) as f64 / 1e6
                    ));
                }
            }
            ProbeEvent::SinkEos(i) => {
                let stopping = matches!(run.phase, Phase::Stopping { .. });
                let Some(bin) = run.bins.get_mut(i) else {
                    return;
                };
                bin.eos = true;
                run.log.line(format!("{}: EOS written", bin.id));
                if !stopping && !bin.closing && bin.lost.is_none() {
                    let message = format!("{} stopped sending data", bin.label);
                    self.branch_lost(&[i], message);
                }
                self.check_stopped();
            }
        }
    }

    fn on_bus(&mut self, generation: u64, msg: &gst::Message) {
        let Some(live) = &self.live else { return };
        if live.generation != generation {
            return;
        }
        match msg.view() {
            gst::MessageView::Error(err) => {
                let (message, detailed) = error_text(&err.error(), err.debug().as_deref());
                let path = msg.src().map(object_path).unwrap_or_default();
                tracing::warn!("capture: error from {}: {detailed}", path.join("<"));
                if let Some(run) = &mut self.take {
                    run.log
                        .line(format!("error from {}: {detailed}", path.join(" < ")));
                }
                self.on_error(&path, message);
            }
            gst::MessageView::Warning(w) => {
                let (_, detailed) = error_text(&w.error(), w.debug().as_deref());
                let path = msg.src().map(object_path).unwrap_or_default();
                tracing::info!("capture: warning from {}: {detailed}", path.join("<"));
                if let Some(run) = &mut self.take {
                    run.log
                        .line(format!("warning from {}: {detailed}", path.join(" < ")));
                }
            }
            gst::MessageView::Latency(_) => {
                let _ = live.pipeline.recalculate_latency();
            }
            gst::MessageView::Element(e) => {
                if let Some(s) = e.structure()
                    && s.name() == "level"
                    && let Some(index) = msg.src().and_then(|src| live.source_of(&object_path(src)))
                    && let Some(reading) = level_reading(live.sources[index].spec.device_name(), s)
                {
                    self.side(SideEvent::Level(reading));
                }
            }
            gst::MessageView::Eos(_) => {
                tracing::info!("capture: every live source ended");
            }
            _ => {}
        }
    }

    /// Attributes an error to a record bin, a source, the preview or the
    /// whole pipeline.
    fn on_error(&mut self, path: &[String], message: String) {
        let bin_index = self.take.as_ref().and_then(|run| {
            run.bins
                .iter()
                .position(|b| path.iter().any(|name| *name == b.bin.name().as_str()))
        });
        if let Some(index) = bin_index {
            let label = self
                .take
                .as_ref()
                .map(|r| r.bins[index].label.clone())
                .unwrap_or_default();
            return self.branch_lost(&[index], format!("{label}: {message}"));
        }
        let source = self.live.as_ref().and_then(|live| live.source_of(path));
        if let Some(source) = source {
            let spec = match &mut self.live {
                Some(live) => {
                    live.sources[source].dead = Some(message.clone());
                    live.sources[source].spec.clone()
                }
                None => return,
            };
            let bins: Vec<usize> = self
                .take
                .as_ref()
                .map(|run| {
                    run.bins
                        .iter()
                        .enumerate()
                        .filter(|(_, b)| b.source == source)
                        .map(|(i, _)| i)
                        .collect()
                })
                .unwrap_or_default();
            let text = format!("{}: {message}", spec.describe());
            if bins.is_empty() {
                self.side(SideEvent::LiveError(text));
            } else {
                self.branch_lost(&bins, text);
            }
            return;
        }
        if path.iter().any(|name| name.starts_with("pv")) {
            self.side(SideEvent::LiveError(format!("preview: {message}")));
            return;
        }
        // Pipeline-wide.
        let all: Vec<usize> = self
            .take
            .as_ref()
            .map_or(Vec::new(), |run| (0..run.bins.len()).collect());
        if let Some(live) = &mut self.live {
            for source in &mut live.sources {
                source.dead.get_or_insert_with(|| message.clone());
            }
        }
        if all.is_empty() {
            self.side(SideEvent::LiveError(message));
        } else {
            self.branch_lost(&all, message);
        }
    }

    /// Some branches stopped working: before `CaptureStarted` that fails
    /// the take; afterwards each one is closed and reported `StreamLost`.
    fn branch_lost(&mut self, bins: &[usize], message: String) {
        let Some(run) = &mut self.take else { return };
        match run.phase {
            Phase::New | Phase::AwaitingPortal { .. } => {}
            Phase::Starting { .. } => {
                let label = bins
                    .first()
                    .and_then(|&i| run.bins.get(i))
                    .map(|b| b.label.clone());
                self.fail_take(label, message);
            }
            Phase::Recording { .. } | Phase::Stopping { .. } => {
                let tag = run.tag;
                let take = run.plan.take.clone();
                let mut lost = Vec::new();
                for &i in bins {
                    let Some(bin) = run.bins.get_mut(i) else {
                        continue;
                    };
                    if bin.lost.is_some() {
                        continue;
                    }
                    bin.lost = Some(message.clone());
                    bin.begin_close(&self.tx, tag);
                    lost.push(bin.id.clone());
                }
                for id in &lost {
                    run.log.line(stream_lost_line(id, &message));
                }
                if matches!(run.phase, Phase::Recording { .. }) {
                    for id in lost {
                        self.emit(Event::StreamLost {
                            take: take.clone(),
                            stream: id,
                            message: message.clone(),
                        });
                    }
                }
                self.check_stopped();
            }
        }
    }

    /// Stopping: done once every bin wrote EOS or was lost (and the fake EOS
    /// delay is over).
    fn check_stopped(&mut self) {
        let Some(run) = &self.take else { return };
        if !matches!(run.phase, Phase::Stopping { .. }) {
            return;
        }
        let held = run.hold_until.is_some_and(|until| Instant::now() < until);
        if !held && run.bins.iter().all(|b| b.eos || b.lost.is_some()) {
            self.finish_take(false);
        }
    }

    fn finish_take(&mut self, timed_out: bool) {
        let Some(mut run) = self.take.take() else {
            return;
        };
        let streams: Vec<RecordedStream> = run
            .bins
            .iter()
            .map(|b| RecordedStream {
                id: b.id.clone(),
                file: b.file.clone(),
                start_offset_ns: b.start_offset_ns(run.t0),
                eos: b.eos,
            })
            .collect();
        if timed_out {
            let waiting: Vec<&str> = run
                .bins
                .iter()
                .filter(|b| !b.eos && b.lost.is_none())
                .map(|b| b.id.as_str())
                .collect();
            run.log.line(format!(
                "EOS timed out after {} ms waiting for {}",
                run.plan.eos_timeout_ms,
                waiting.join(", ")
            ));
        }
        let ok = self.teardown_bins(&mut run.bins, true);
        for s in &streams {
            run.log.line(format!(
                "{} eos={}",
                start_offset_line(s.id.as_str(), &s.file, s.start_offset_ns),
                s.eos
            ));
        }
        run.log.line(format!(
            "stopped{}{}",
            if run.cancel { " (cancelled)" } else { "" },
            if timed_out { " (timed out)" } else { "" }
        ));
        self.emit(Event::CaptureStopped {
            take: run.plan.take.clone(),
            streams,
            timed_out,
        });
        if !ok {
            self.wedge(
                "the capture engine is stuck tearing down a take (GPU driver?); restart Qrec"
                    .into(),
            );
        }
        self.after_take();
    }

    /// Unlinks (if still linked), stops and removes record bins. `false` =
    /// the bins didn't reach NULL within the teardown deadline.
    fn teardown_bins(&mut self, bins: &mut [RecordBin], _graceful: bool) -> bool {
        for bin in bins.iter_mut() {
            if !bin.closing {
                bin.unlink_now();
            }
            bin.release_tee_pad();
        }
        let elements: Vec<gst::Element> = bins.iter().map(|b| b.bin.clone().upcast()).collect();
        let ok = set_null_with_deadline(elements, TEARDOWN_TIMEOUT);
        if ok && let Some(live) = &self.live {
            for bin in bins.iter() {
                let _ = live.pipeline.remove(&bin.bin);
            }
        }
        ok
    }

    /// After a take: keep the live pipeline only if a live scene is wanted.
    fn after_take(&mut self) {
        if self.exiting {
            return;
        }
        self.apply_desired();
    }

    fn on_source_size(&mut self, generation: u64, source: usize, width: u32, height: u32) {
        let Some(live) = &mut self.live else { return };
        if live.generation != generation || source >= live.sources.len() {
            return;
        }
        if live.sources[source].size == Some((width, height)) {
            return;
        }
        live.sources[source].size = Some((width, height));
        if let Some(scene) = &self.desired {
            live.apply_layout(scene);
        }
    }

    fn inject(&mut self, source: &str, failure: FakeFailure) {
        if self.opts.backend != Backend::Fake {
            tracing::warn!("capture: fake failures only work with the fake backend");
            return;
        }
        let Some(live) = &self.live else { return };
        let Some(branch) = live.sources.iter().find(|b| b.spec.device_name() == source) else {
            tracing::warn!("capture: no live source {source} to fail");
            return;
        };
        match failure {
            FakeFailure::Error => {
                let msg = gst::message::Error::builder(
                    gst::ResourceError::Read,
                    "the device was unplugged (simulated)",
                )
                .src(&branch.src)
                .build();
                let _ = branch.src.post_message(msg);
            }
            FakeFailure::Eos => {
                let _ = branch.src.send_event(gst::event::Eos::new());
            }
            FakeFailure::Stall => {
                if let Some(pad) = branch.src.static_pad("src") {
                    pad.add_probe(gst::PadProbeType::BUFFER, |_, _| gst::PadProbeReturn::Drop);
                }
            }
        }
    }

    // ------------------------------------------------------------ time

    fn next_deadline(&self) -> Option<Instant> {
        let run = self.take.as_ref()?;
        match &run.phase {
            Phase::New => None,
            Phase::AwaitingPortal { deadline, .. } => Some(*deadline),
            // Probe events wake the loop as files start; the deadline is the
            // end of the health window once all have, else the first-buffer limit.
            Phase::Starting {
                health_until,
                first_buffer_by,
            } => Some(if run.bins.iter().all(RecordBin::written) {
                *health_until
            } else {
                *first_buffer_by
            }),
            Phase::Recording { next_clock } => Some(*next_clock),
            Phase::Stopping {
                fill_by,
                first_by,
                eos_by,
            } => {
                let close_by = run
                    .bins
                    .iter()
                    .filter(|b| !b.closing && b.lost.is_none())
                    .map(|b| close_deadline(b, *fill_by, *first_by))
                    .min();
                let due = close_by.map_or(*eos_by, |at| at.min(*eos_by));
                // `tick` clears a hold once it is over.
                Some(run.hold_until.map_or(due, |hold| hold.min(due)))
            }
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let Some(run) = &mut self.take else { return };
        match run.phase {
            Phase::New => {}
            Phase::AwaitingPortal { deadline, .. } => {
                if now >= deadline {
                    self.fail_take(None, "the screen-share portal didn't answer".into());
                }
            }
            Phase::Starting {
                health_until,
                first_buffer_by,
            } => {
                let all_written = run.bins.iter().all(RecordBin::written);
                if all_written && now >= health_until {
                    run.phase = Phase::Recording {
                        next_clock: now + CLOCK_INTERVAL,
                    };
                    let files: Vec<String> = run.bins.iter().map(|b| b.file.clone()).collect();
                    // Every bin passed its first buffer (it wrote one): the
                    // offsets are known now, not only at the stop (a crash
                    // never gets there).
                    let start_offset_ns = run
                        .bins
                        .iter()
                        .map(|b| (b.id.clone(), b.start_offset_ns(run.t0)))
                        .collect();
                    run.log.line("recording");
                    // A rebuild reads them back (A§7.7).
                    for bin in &run.bins {
                        run.log.line(start_offset_line(
                            bin.id.as_str(),
                            &bin.file,
                            bin.start_offset_ns(run.t0),
                        ));
                    }
                    let take = run.plan.take.clone();
                    let t0 = run.t0;
                    self.emit(Event::CaptureStarted {
                        take: take.clone(),
                        t0_ns: t0,
                        files,
                        start_offset_ns,
                    });
                    self.emit_clock(&take, t0);
                } else if !all_written && now >= first_buffer_by {
                    let silent = run.bins.iter().find(|b| !b.written());
                    let label = silent.map(|b| b.label.clone());
                    let message = match silent {
                        Some(bin) => format!(
                            "no data from {} within {} ms: {}",
                            bin.label,
                            run.plan.first_buffer_ms,
                            silence_reason(&run.plan, bin.id.as_str())
                        ),
                        None => "the recording didn't start".into(),
                    };
                    self.fail_take(label, message);
                }
            }
            Phase::Recording { next_clock } => {
                if now >= next_clock {
                    let mut next = next_clock + CLOCK_INTERVAL;
                    if next <= now {
                        next = now + CLOCK_INTERVAL;
                    }
                    run.phase = Phase::Recording { next_clock: next };
                    let (take, t0) = (run.plan.take.clone(), run.t0);
                    self.emit_clock(&take, t0);
                    self.check_stalls();
                }
            }
            Phase::Stopping {
                fill_by,
                first_by,
                eos_by,
            } => {
                let held_until_now = run.hold_until.is_some_and(|until| now >= until);
                if held_until_now {
                    run.hold_until = None;
                }
                if now >= eos_by {
                    self.finish_take(true);
                } else if now >= fill_by {
                    // Sources that never reached T1 (stalled): close anyway,
                    // once each had its chance to pass a first buffer.
                    let tag = run.tag;
                    for bin in &mut run.bins {
                        if now >= close_deadline(bin, fill_by, first_by) {
                            bin.begin_close(&self.tx, tag);
                        }
                    }
                    self.check_stopped();
                } else if held_until_now {
                    self.check_stopped();
                }
            }
        }
    }

    /// Recording: a video stream that passed no frame for [`STALL_LIMIT`]
    /// is lost like an unplugged one (SPEC §16). A camera can hang without
    /// an error; its bin would otherwise be filled for as long as the hang
    /// lasted at the stop, more frames than the EOS timeout lets it encode
    /// (a still screen never gets here: the bridge's keepalive, A§8.11).
    fn check_stalls(&mut self) {
        let Some(now) = self.live.as_ref().and_then(|l| running_time(&l.pipeline)) else {
            return;
        };
        let Some(run) = &self.take else { return };
        let watched: Vec<(Option<u64>, bool)> = run
            .bins
            .iter()
            .map(|b| {
                let watch = b.kind == StreamKind::Video && b.lost.is_none() && !b.closing;
                (b.probe.lock().last, watch)
            })
            .collect();
        let lost: Vec<(usize, String)> = stalled(&watched, now, STALL_LIMIT)
            .into_iter()
            .map(|(i, silent)| {
                let bin = &run.bins[i];
                let message = format!(
                    "no frames from {} for {:.1} s: {}",
                    bin.label,
                    silent.as_secs_f64(),
                    stall_reason(&run.plan, bin.id.as_str())
                );
                (i, message)
            })
            .collect();
        for (i, message) in lost {
            // Its branch is rebuilt after the take: reopening the device
            // may revive it, and later takes don't attach to a hung tee.
            if let (Some(live), Some(run)) = (&mut self.live, &self.take)
                && let Some(source) = run.bins.get(i).and_then(|b| live.sources.get_mut(b.source))
            {
                source.dead.get_or_insert_with(|| message.clone());
            }
            self.branch_lost(&[i], message);
        }
    }

    fn emit_clock(&self, take: &TakeId, t0: u64) {
        let now = self.live.as_ref().and_then(|l| running_time(&l.pipeline));
        if let Some(now) = now {
            // QREC_FAKE_RECORD_OFFSET_FRAMES: a take that seems to have
            // started earlier (the GUI's timer and ghost; fake backend only).
            let offset = match (&self.take, self.opts.backend) {
                (Some(run), Backend::Fake) => self.opts.fake.record_offset_ns(run.plan.fps),
                _ => 0,
            };
            self.emit(Event::RecordClock {
                take: take.clone(),
                elapsed_ns: now.saturating_sub(t0).saturating_add(offset),
            });
        }
    }

    // ------------------------------------------------------------ shutdown

    fn begin_shutdown(&mut self, ack: Sender<()>) {
        self.exiting = true;
        self.shutdown_ack = Some(ack);
        let Some(run) = &self.take else { return };
        let take = run.plan.take.clone();
        match run.phase {
            Phase::Stopping { .. } => {}
            _ => self.stop(take, false),
        }
    }

    fn maybe_finish_shutdown(&mut self) {
        if !self.exiting || self.take.is_some() || self.done {
            return;
        }
        self.teardown_live();
        let casts: Vec<ScreenShare> = std::mem::take(&mut self.casts).into_values().collect();
        if let Some(worker) = &self.portal {
            worker.close(casts);
        }
        if let Some(ack) = self.shutdown_ack.take() {
            let _ = ack.send(());
        }
        self.done = true;
    }
}

/// Kicks the bridge keepalive of every screen that `bins` record from, so
/// a still one sends its last frame now (A§8.11).
fn kick_screens(live: &Live, bins: &[RecordBin]) {
    let mut sources: Vec<usize> = bins.iter().map(|b| b.source).collect();
    sources.sort_unstable();
    sources.dedup();
    for source in sources {
        if let Some(keepalive) = live.sources.get(source).and_then(|s| s.keepalive.as_ref()) {
            keepalive.kick();
        }
    }
}

/// The bins (by index) whose last frame is more than `limit` older than
/// the running time `now`, with how long they've been silent. Each bin is
/// `(running time of its last passed frame, watched)`; a bin that passed
/// none yet is the first-buffer check's business, not this.
fn stalled(bins: &[(Option<u64>, bool)], now: u64, limit: Duration) -> Vec<(usize, Duration)> {
    bins.iter()
        .enumerate()
        .filter_map(|(i, &(last, watched))| {
            let silent = Duration::from_nanos(now.saturating_sub(last?));
            (watched && silent > limit).then_some((i, silent))
        })
        .collect()
}

/// Why a video stream went silent mid-take, for its `StreamLost` message.
fn stall_reason(plan: &CapturePlan, id: &str) -> &'static str {
    match plan
        .video
        .iter()
        .find(|v| v.id.as_str() == id)
        .map(|v| &v.source)
    {
        Some(VideoSource::Screen { .. }) => "the screen share stopped sending frames",
        _ => {
            "the camera stopped sending frames; it may have been unplugged or hung (unplug and \
             replug it; `journalctl -k | grep uvcvideo` shows driver errors)"
        }
    }
}

/// What usually keeps a source silent through the start window (A§5.3), for
/// the `capture_failed` message.
fn silence_reason(plan: &CapturePlan, id: &str) -> &'static str {
    match plan
        .video
        .iter()
        .find(|v| v.id.as_str() == id)
        .map(|v| &v.source)
    {
        Some(VideoSource::Camera { .. }) => {
            "the camera sent no frames; it may be in use, unplugged or stuck (unplug and replug \
             it; `journalctl -k | grep uvcvideo` shows driver errors), or not support the chosen mode"
        }
        Some(VideoSource::Screen { .. }) => {
            "the screen share sent no frames; the portal may have stopped it or the output may be off"
        }
        None => "the audio device sent no samples; it may be unplugged or suspended",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_watched_bins_silent_past_the_limit_are_stalled() {
        let ms = |n: u64| n * 1_000_000;
        let limit = Duration::from_secs(3);
        let now = ms(10_000);
        let bins = [
            // Silent 3.5 s: stalled.
            (Some(ms(6_500)), true),
            // Silent exactly the limit: not yet.
            (Some(ms(7_000)), true),
            // A frame 10 ms ago.
            (Some(ms(9_990)), true),
            // Silent, but not watched (audio, lost or closing).
            (Some(ms(1_000)), false),
            // No frame yet: the first-buffer check's business.
            (None, true),
            // A frame "after" now (clock skew): not silent.
            (Some(ms(10_500)), true),
        ];
        assert_eq!(
            stalled(&bins, now, limit),
            [(0, Duration::from_millis(3_500))]
        );
    }
}
