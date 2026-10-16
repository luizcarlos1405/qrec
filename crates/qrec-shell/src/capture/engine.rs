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
//!
//! For the GUI (docs/GUI.md G§3.9): the preview goes to its sink when it
//! gives one (`EngineOptions::preview`, the same element in every rebuilt
//! pipeline); [`LiveControl`] lays the preview out during a drag until the
//! next `SetLive`; `retry_live` forgets what was refused or failed and
//! rebuilds; screen casts outlive the pipeline by `cast_grace`; and every
//! change to a live source's state goes out as `SideEvent::LiveStatus`.
//! A source that fails to start (or a screen the portal refused) stays out
//! of the live pipeline, which runs with the rest, until the scene's
//! sources change or a retry.

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
use qrec_core::model::{Layer, Scene, StreamKind, VideoSource};
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
    Backend, CLOCK_INTERVAL, EngineOptions, FILL_GRACE, LiveControl, LiveSource, LiveSourceState,
    LiveSourceStatus, PIPELINE_START_TIMEOUT, STALL_LIMIT, SideEvent, TEARDOWN_TIMEOUT,
};
use crate::deps::CaptureEngine;
use crate::gst_init::{self, InitError};
use crate::msg::Inbox;
use crate::paths::ProjectPaths;

/// How long `shutdown()` waits for the engine thread: a running take is
/// finalized first (EOS within the default 10 s, then teardown within 5 s).
const SHUTDOWN_WAIT: Duration = Duration::from_secs(16);

/// How the portal words a closed screen-share dialog (`capture::portal`,
/// and the fake portal's `cancel`): `LiveSourceState::Refused { cancelled }`.
const CANCELLED_IN_THE_DIALOG: &str = "cancelled in the dialog";

/// Why a live source that ended is dead.
const STOPPED_SENDING: &str = "it stopped sending data";

/// Messages to the engine thread.
pub(super) enum Cmd {
    SetLive(Option<LiveScene>, Inbox),
    Start(Box<CapturePlan>, Inbox),
    Stop {
        take: TakeId,
        cancel: bool,
    },
    Shutdown(Sender<()>),
    /// The GUI's retry (D-50): forget refused, failed and dead sources and
    /// rebuild.
    RetryLive(Inbox),
    /// [`LiveControl::transient_layout`]: the preview's layers laid out as
    /// this scene's until the next `SetLive` (`None`: as the live scene's).
    Transient(Option<Scene>),
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
    /// A source of the live pipeline with this generation ended (EOS).
    SourceEos {
        generation: u64,
        source: usize,
    },
    /// The portal worker finished an open request.
    PortalDone {
        id: u64,
        results: Vec<(String, Result<ScreenShare, String>)>,
    },
    /// The fake backend's portal answered: a refusal (`QREC_FAKE_PORTAL`)
    /// or, in tests, a share without a session.
    FakePortalDone {
        id: u64,
        results: Vec<(String, Result<(), String>)>,
    },
    /// Test hook (fake backend only): make a source fail.
    Inject {
        source: String,
        failure: FakeFailure,
    },
    /// Test hook: the fake screens go through a fake portal that shares
    /// them (like a real one would), so casts and their grace can be seen.
    #[cfg(test)]
    ShareFakeScreens,
    /// Test hook: what the live pipeline is right now.
    #[cfg(test)]
    Inspect(Sender<LiveInfo>),
}

/// The live pipeline as tests see it.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(super) struct LiveInfo {
    /// Bumped by every rebuild; `None` without a pipeline.
    pub generation: Option<u64>,
    pub sources: Vec<SourceSpec>,
    pub preview: bool,
    /// The preview goes to the GUI's sink (else the RGBA appsink).
    pub preview_sink: bool,
    /// What the preview compositor outputs.
    pub preview_size: Option<(u32, u32)>,
    /// Each preview layer's compositor pad.
    pub layers: Vec<PadInfo>,
    /// Outputs with an open screen share.
    pub casts: Vec<String>,
    /// Screens asked of the portal so far (one per screen per request).
    pub portal_asks: u64,
    /// A transient layout is in place.
    pub transient: bool,
}

/// A preview layer's compositor pad, in output pixels.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PadInfo {
    pub layer: String,
    pub rect: (i32, i32, i32, i32),
    pub alpha: f64,
    pub zorder: u32,
}

/// An open screen share: the portal session of a real screen, or none for
/// a fake screen shared by the tests' fake portal. Kept while a live
/// pipeline (or a take) uses it, and for the cast grace after.
pub(super) struct OpenCast {
    pub share: Option<ScreenShare>,
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

    /// Test hook (fake backend): from now on the fake screens are shared
    /// through a fake portal, answered through the queue like the real
    /// one's dialog, so the engine holds casts for them.
    #[cfg(test)]
    pub(super) fn share_fake_screens(&self) {
        let _ = self.tx.send(Cmd::ShareFakeScreens);
    }

    /// Moves the live preview's layers during a drag or nudge (docs/GUI.md
    /// G§3.9, D-32, D-33): each layout goes to the engine thread, which sets
    /// the preview compositor's pads without a rebuild until the next
    /// `SetLive` or a `None`. Never reaches a take or State.
    pub fn control(&self) -> LiveControl {
        let tx = self.tx.clone();
        LiveControl::new(move |scene| {
            let _ = tx.send(Cmd::Transient(scene));
        })
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

    fn retry_live(&mut self, out: Inbox) {
        self.remember(&out);
        let _ = self.tx.send(Cmd::RetryLive(out));
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
    casts: BTreeMap<String, OpenCast>,
    /// When the casts kept after the live pipeline stopped close (GUI H-4).
    casts_close_at: Option<Instant>,
    portal_seq: u64,
    /// Screens asked of the portal so far, one per screen per request.
    portal_asks: u64,
    /// Outputs asked of the portal and not answered yet.
    in_flight: BTreeSet<String>,
    /// Outputs the portal refused for the live scene's sources (left out of
    /// the live pipeline until they change or a retry, D-50).
    refused: BTreeMap<String, String>,
    /// Sources of the live scene that failed to start, by device, and why:
    /// left out like refused screens.
    failed: BTreeMap<String, String>,
    /// Why the last live pipeline couldn't start at all (no source to blame).
    broken: Option<String>,
    /// The sources `refused` and `failed` are about.
    known_sources: BTreeSet<SourceSpec>,
    /// The size each video source delivered last, by its whole spec (a
    /// camera's mode too: another mode misses it): a rebuilt branch starts
    /// laid out (and reported) with it ([`Self::expected_size`]). A branch
    /// that dies before it negotiated forgets it: a crop sized for frames
    /// larger than the new ones leaves the source no caps.
    sizes: BTreeMap<SourceSpec, (u32, u32)>,
    /// [`LiveControl::transient_layout`]'s layers, until the next `SetLive`.
    transient: Option<Scene>,
    /// The last `SideEvent::LiveStatus` sent.
    status: Vec<LiveSourceStatus>,
    /// Test hook: the fake screens go through a sharing fake portal.
    #[cfg(test)]
    share_fake_screens: bool,
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
            casts_close_at: None,
            portal_seq: 0,
            portal_asks: 0,
            in_flight: BTreeSet::new(),
            refused: BTreeMap::new(),
            failed: BTreeMap::new(),
            broken: None,
            known_sources: BTreeSet::new(),
            sizes: BTreeMap::new(),
            transient: None,
            status: Vec::new(),
            #[cfg(test)]
            share_fake_screens: false,
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
            self.report_status();
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
            Cmd::RetryLive(out) => {
                self.set_out(out);
                self.retry_live();
            }
            Cmd::Transient(scene) => self.set_transient(scene),
            Cmd::Bus(generation, msg) => self.on_bus(generation, &msg),
            Cmd::Probe(tag, event) => self.on_probe(tag, event),
            Cmd::SourceSize {
                generation,
                source,
                width,
                height,
            } => self.on_source_size(generation, source, width, height),
            Cmd::SourceEos { generation, source } => self.on_source_eos(generation, source),
            Cmd::PortalDone { id, results } => {
                tracing::debug!("capture: portal answer {id}");
                let results = results
                    .into_iter()
                    .map(|(output, result)| {
                        (output, result.map(|share| OpenCast { share: Some(share) }))
                    })
                    .collect();
                self.on_portal(results);
            }
            Cmd::FakePortalDone { id, results } => {
                tracing::debug!("capture: fake portal answer {id}");
                let results = results
                    .into_iter()
                    .map(|(output, result)| (output, result.map(|()| OpenCast { share: None })))
                    .collect();
                self.on_portal(results);
            }
            Cmd::Inject { source, failure } => self.inject(&source, failure),
            #[cfg(test)]
            Cmd::ShareFakeScreens => self.share_fake_screens = true,
            #[cfg(test)]
            Cmd::Inspect(reply) => {
                let _ = reply.send(self.inspect());
            }
        }
    }

    /// What tests see of the live pipeline (`Cmd::Inspect`).
    #[cfg(test)]
    fn inspect(&self) -> LiveInfo {
        let live = self.live.as_ref();
        let preview = live.and_then(|l| l.preview.as_ref());
        let layers = preview
            .map(|p| {
                p.layers
                    .iter()
                    .map(|pl| PadInfo {
                        layer: pl.layer.id.to_string(),
                        rect: (
                            pl.pad.property::<i32>("xpos"),
                            pl.pad.property::<i32>("ypos"),
                            pl.pad.property::<i32>("width"),
                            pl.pad.property::<i32>("height"),
                        ),
                        alpha: pl.pad.property::<f64>("alpha"),
                        zorder: pl.pad.property::<u32>("zorder"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        LiveInfo {
            generation: live.map(|l| l.generation),
            sources: live
                .map(|l| l.sources.iter().map(|s| s.spec.clone()).collect())
                .unwrap_or_default(),
            preview: preview.is_some(),
            preview_sink: match (&self.opts.preview, live) {
                (Some(out), Some(live)) => {
                    out.sink.parent().as_ref() == Some(live.pipeline.upcast_ref::<gst::Object>())
                }
                _ => false,
            },
            preview_size: preview.map(|p| p.scale.out),
            layers,
            casts: self.casts.keys().cloned().collect(),
            portal_asks: self.portal_asks,
            transient: self.transient.is_some(),
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
    /// an equal scene with a healthy pipeline changes nothing. Any `SetLive`
    /// ends a transient layout.
    fn set_live(&mut self, scene: Option<LiveScene>) {
        let transient = self.transient.take().is_some();
        if let Some(next) = &scene {
            let sources: BTreeSet<SourceSpec> = scene_sources(&next.scene).into_iter().collect();
            if sources != self.known_sources {
                // New sources may be picked differently in the dialog, or
                // be plugged in now: try them again. (A layout change, or
                // Program mode and back, keeps what was refused: the GUI's
                // retry asks again, D-50.)
                self.refused.clear();
                self.failed.clear();
                self.known_sources = sources;
            }
        }
        if scene == self.desired && self.live_is_healthy() {
            if transient {
                self.apply_layout();
            }
            return;
        }
        self.desired = scene;
        if self.take.is_some() {
            // Layout changes show now; source changes wait for the take's end.
            self.apply_layout();
            return;
        }
        self.apply_desired(false);
    }

    /// Lays the live preview out as the live scene, or as the transient
    /// layout while one is in place.
    fn apply_layout(&self) {
        let (Some(live), Some(scene)) = (&self.live, &self.desired) else {
            return;
        };
        let layers: &[Layer] = match &self.transient {
            Some(transient) => &transient.layers,
            None => &scene.scene.layers,
        };
        live.apply_layout(layers);
    }

    /// `LiveControl::transient_layout` (GUI D-32, D-33): the preview is laid
    /// out again (pads, and crops that change), without a rebuild; `None`
    /// goes back to the live scene's.
    fn set_transient(&mut self, scene: Option<Scene>) {
        if scene.is_none() && self.transient.is_none() {
            return;
        }
        self.transient = scene;
        self.apply_layout();
    }

    /// The GUI's "Try again" (D-50, `Msg::RetryLive`): forget the screens the
    /// portal refused, the sources that failed to start and the dead ones,
    /// and rebuild the live pipeline (asking the portal again). Screens that
    /// failed or died lose their cast, which is stale. During a take the
    /// rebuild waits for its end, like every source change.
    fn retry_live(&mut self) {
        let dead: Vec<String> = self
            .live
            .iter()
            .flat_map(|live| &live.sources)
            .filter(|s| s.dead.is_some())
            .map(|s| s.spec.device_name().to_owned())
            .collect();
        tracing::info!(
            "capture: retrying the live preview: {} refused, {} failed, {} dead",
            self.refused.len(),
            self.failed.len(),
            dead.len()
        );
        let stale: Vec<OpenCast> = dead
            .iter()
            .chain(self.failed.keys())
            .filter_map(|name| self.casts.remove(name))
            .collect();
        self.close_casts(stale);
        // A size a source failed with may be why it failed (a crop for
        // larger frames): it starts uncropped and negotiates again.
        self.sizes
            .retain(|spec, _| !self.failed.contains_key(spec.device_name()));
        self.refused.clear();
        self.failed.clear();
        self.broken = None;
        if self.take.is_none() && self.desired.is_some() && !self.exiting {
            self.apply_desired(true);
        }
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

    /// Makes the live pipeline match `desired` (no take running); `force`
    /// rebuilds it even when it matches (a retry).
    fn apply_desired(&mut self, force: bool) {
        let Some(scene) = self.desired.clone() else {
            self.teardown_live();
            self.release_casts();
            return;
        };
        // A live scene again: the casts it shows stay, the others close
        // once it runs.
        self.casts_close_at = None;
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
        // Screens the portal refused and sources that failed to start stay
        // out of the preview until the scene's sources change or a retry;
        // the rest still shows.
        let specs: Vec<SourceSpec> = self
            .scene_specs(&scene)
            .into_iter()
            .filter(|s| !self.left_out(s))
            .collect();
        let key = LiveKey::of(specs.clone(), Some(&scene));
        if !force
            && let Some(live) = &self.live
            && live.key == key
            && live.sources.iter().all(|s| s.dead.is_none())
        {
            self.apply_layout();
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
                return self.apply_desired(force);
            }
        }
        if let Err(e) = self.rebuild_live(&specs, Some(&scene)) {
            let message = match &e.source {
                Some(spec) => format!("{}: {}", spec.describe(), e.message),
                None => e.message.clone(),
            };
            tracing::warn!("capture: the live pipeline can't start: {message}");
            self.side(SideEvent::LiveError(message.clone()));
            match e.source {
                // Without it, the rest starts (each round leaves one
                // more source out, so this ends). The size it was laid
                // out with may be what failed it: the next try is
                // uncropped.
                Some(spec) => {
                    self.failed
                        .insert(spec.device_name().to_owned(), e.message.clone());
                    self.sizes.remove(&spec);
                    return self.apply_desired(false);
                }
                None => self.broken = Some(e.message),
            }
        }
        self.close_unused_casts(&specs);
    }

    /// Whether the live pipeline leaves `spec` out: a screen the portal
    /// refused, or a source that failed to start.
    fn left_out(&self, spec: &SourceSpec) -> bool {
        let name = spec.device_name();
        self.failed.contains_key(name)
            || matches!(spec, SourceSpec::Screen { .. }) && self.refused.contains_key(name)
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

    /// Sends `SideEvent::LiveStatus` when a live source's state or size
    /// changed: after builds and rebuilds, portal answers, deaths, sizes
    /// negotiated, a retry, and a scene that drops or adds sources.
    fn report_status(&mut self) {
        if self.opts.side.is_none() {
            return;
        }
        let status = self.live_status();
        if status != self.status {
            tracing::debug!("capture: live status {status:?}");
            self.side(SideEvent::LiveStatus(status.clone()));
            self.status = status;
        }
    }

    /// Every source of the live scene, in the scene's order (its layers in
    /// array order, bottom first and the last on top, then its audio
    /// tracks; one entry per device; consumers look a source up by name,
    /// `live_sources_view`), that has a state: waiting for the portal
    /// (a screen asked again, a retry or a take's dialog: before its old
    /// branch or refusal), in the live pipeline (`Live`, or `Dead` once it
    /// failed or ended), refused by the portal, or failed to start
    /// (`Dead`). A source with none isn't started yet: its pipeline waits
    /// for the portal's answer about another screen, or for the take to
    /// end.
    fn live_status(&self) -> Vec<LiveSourceStatus> {
        let Some(scene) = &self.desired else {
            return Vec::new();
        };
        let live = self.live.as_ref();
        // The scene's specs as they are (no camera mode: picking one asks
        // the devices, and this runs after every command).
        scene_sources(&scene.scene)
            .into_iter()
            .filter_map(|spec| {
                let name = spec.device_name();
                let screen = matches!(spec, SourceSpec::Screen { .. });
                let branch =
                    live.and_then(|l| l.sources.iter().find(|b| b.spec.same_device(&spec)));
                // A branch's size (expected at its build, then negotiated);
                // without one, what the device delivered last in any mode.
                let known = branch.and_then(|b| b.size).or_else(|| {
                    self.sizes
                        .iter()
                        .find(|(cached, _)| cached.same_device(&spec))
                        .map(|(_, size)| *size)
                });
                // A screen asked of the portal has no cast, so a branch of
                // it is a dead one's and a refusal is the last answer.
                let (state, size) = if screen && self.in_flight.contains(name) {
                    (LiveSourceState::AwaitingPortal, None)
                } else if let Some(branch) = branch {
                    let state = match &branch.dead {
                        Some(message) => LiveSourceState::Dead {
                            message: message.clone(),
                        },
                        None => LiveSourceState::Live,
                    };
                    (state, known)
                } else if let Some(message) = self.refused.get(name).filter(|_| screen) {
                    let state = LiveSourceState::Refused {
                        cancelled: message.contains(CANCELLED_IN_THE_DIALOG),
                        message: message.clone(),
                    };
                    (state, None)
                } else if let Some(message) = self.failed.get(name).or(self.broken.as_ref()) {
                    let state = LiveSourceState::Dead {
                        message: message.clone(),
                    };
                    (state, known)
                } else {
                    return None;
                };
                Some(LiveSourceStatus {
                    source: live_source(&spec),
                    state,
                    size: size.filter(|_| spec.is_video()),
                })
            })
            .collect()
    }

    /// Replaces the live pipeline with one for `specs`, laid out (the
    /// preview, as [`Self::apply_layout`] does) before it starts.
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
            preview: self.opts.preview.as_ref(),
        };
        let mut live = Live::build(specs, scene, &ctx)?;
        // Laid out (and reported) at the size each source will deliver,
        // until it negotiates.
        for branch in live.sources.iter_mut().filter(|b| b.spec.is_video()) {
            branch.size = self.expected_size(&branch.spec);
        }
        let generation = live.generation;
        let pipeline = live.pipeline.clone();
        self.live = Some(live);
        // Before it starts: a crop that changes while a source's first caps
        // query crosses its `videocrop` (two caps transforms, each with
        // another crop) leaves no common caps, and the source dies
        // "not-negotiated". The GUI's sink (`async=false`) lets the
        // pipeline reach PLAYING before its sources negotiated.
        self.apply_layout();
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
        // What started isn't failing any more (a take may start a source
        // the preview had left out).
        for spec in specs {
            self.failed.remove(spec.device_name());
        }
        self.broken = None;
        Ok(())
    }

    /// The size a branch for `spec` will deliver, before it negotiates: a
    /// camera mode's own (its capsfilter, and `jpegdec` for MJPEG, force
    /// it), or what that source delivered last. Never a size of another
    /// mode or device: a crop sized for larger frames than the new ones
    /// (a canvas rate that picks another mode: the fake camera's 1280×720 at
    /// 30 fps, 640×480 at 60) leaves the source no caps, and it dies
    /// "not-negotiated".
    fn expected_size(&self, spec: &SourceSpec) -> Option<(u32, u32)> {
        match spec {
            SourceSpec::Camera {
                mode: Some(mode), ..
            } => Some((mode.width, mode.height)),
            _ => self.sizes.get(spec).copied(),
        }
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
            let ok = set_null_with_deadline(vec![live.pipeline.clone().upcast()], TEARDOWN_TIMEOUT);
            if !ok {
                self.wedge(
                    "the capture engine is stuck stopping the live pipeline (GPU driver?); restart Qrec"
                        .into(),
                );
                return;
            }
            // At NULL: the GUI's sink leaves with nothing in flight, for the
            // next pipeline (GUI H-1).
            if let Some(out) = &self.opts.preview
                && out.sink.parent().as_ref() == Some(live.pipeline.upcast_ref::<gst::Object>())
                && let Err(e) = live.pipeline.remove(&out.sink)
            {
                tracing::warn!(
                    "capture: can't take the preview sink out of the live pipeline: {e}"
                );
            }
        }
    }

    /// Whether `src` (a bus message's) is the GUI's preview sink or inside it.
    fn is_preview_sink(&self, src: Option<&gst::Object>) -> bool {
        let Some(out) = &self.opts.preview else {
            return false;
        };
        let sink = out.sink.upcast_ref::<gst::Object>();
        let mut current = src.cloned();
        while let Some(object) = current {
            if &object == sink {
                return true;
            }
            current = object.parent();
        }
        false
    }

    // ------------------------------------------------------------ portal

    /// Screens of `specs` that need a portal session (real backend; the fake
    /// one's screens only with `QREC_FAKE_PORTAL`, whose portal refuses them,
    /// or a test's sharing fake portal).
    fn missing_casts(&self, specs: &[SourceSpec]) -> Vec<String> {
        let portal = match self.opts.backend {
            Backend::Real => true,
            Backend::Fake => self.opts.fake.portal.is_some() || self.fake_portal_shares(),
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

    /// Test hook: the fake screens go through a fake portal that shares them.
    fn fake_portal_shares(&self) -> bool {
        #[cfg(test)]
        {
            self.share_fake_screens
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    fn request_casts(&mut self, outputs: Vec<String>) -> Result<(), String> {
        self.portal_asks += outputs.len() as u64;
        if self.opts.backend == Backend::Fake {
            // QREC_FAKE_PORTAL: refused (a test's fake portal: shared),
            // answered later like the real portal's dialog (through the
            // queue, as its worker does).
            self.portal_seq += 1;
            let refusal = self.opts.fake.portal;
            let results = outputs
                .iter()
                .map(|output| {
                    let answer = match refusal {
                        Some(portal) => Err(portal.refusal(output)),
                        None => Ok(()),
                    };
                    (output.clone(), answer)
                })
                .collect();
            let _ = self.tx.send(Cmd::FakePortalDone {
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

    fn on_portal(&mut self, results: Vec<(String, Result<OpenCast, String>)>) {
        let mut failures = Vec::new();
        for (output, result) in results {
            self.in_flight.remove(&output);
            match result {
                Ok(cast) => {
                    self.refused.remove(&output);
                    if let Some(old) = self.casts.insert(output, cast) {
                        self.close_casts(vec![old]);
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
                self.apply_desired(false);
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
        let casts: Vec<OpenCast> = unused
            .iter()
            .filter_map(|output| self.casts.remove(output))
            .collect();
        self.close_casts(casts);
        if self.casts.is_empty() {
            self.casts_close_at = None;
        }
    }

    /// Ends these screen shares' portal sessions (on the portal thread).
    fn close_casts(&self, casts: Vec<OpenCast>) {
        let shares: Vec<ScreenShare> = casts.into_iter().filter_map(|c| c.share).collect();
        if let Some(worker) = &self.portal {
            worker.close(shares);
        }
    }

    /// The live pipeline stopped (no live scene is wanted): its screen
    /// casts close now, or once `cast_grace` passed without a live scene,
    /// so the GUI's `Tab` back to Live doesn't ask the portal again (H-4).
    fn release_casts(&mut self) {
        if self.casts.is_empty() || self.take.is_some() {
            return;
        }
        let grace = self.opts.cast_grace;
        if grace.is_zero() || self.exiting {
            return self.close_unused_casts(&[]);
        }
        if self.casts_close_at.is_none() {
            tracing::debug!(
                "capture: keeping {} screen share(s) open for {} s",
                self.casts.len(),
                grace.as_secs_f64()
            );
            self.casts_close_at = Some(Instant::now() + grace);
        }
    }

    /// The cast grace is over: the casts no wanted scene shows close.
    fn end_cast_grace(&mut self) {
        self.casts_close_at = None;
        let keep = self
            .desired
            .clone()
            .map(|scene| self.scene_specs(&scene))
            .unwrap_or_default();
        tracing::debug!("capture: the cast grace is over");
        self.close_unused_casts(&keep);
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
            Ok(()) => self.attach_take(),
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
                if self.is_preview_sink(msg.src()) {
                    // The GUI's sink: a preview problem, never a source's.
                    self.side(SideEvent::LiveError(format!("preview: {message}")));
                    return;
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
                    let branch = &mut live.sources[source];
                    branch.dead = Some(message.clone());
                    if !branch.negotiated {
                        // The size it was laid out with may be what
                        // killed it: the next build starts uncropped.
                        self.sizes.remove(&branch.spec);
                    }
                    branch.spec.clone()
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
                if !source.negotiated {
                    self.sizes.remove(&source.spec);
                }
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
        self.apply_desired(false);
    }

    fn on_source_size(&mut self, generation: u64, source: usize, width: u32, height: u32) {
        let Some(live) = &mut self.live else { return };
        if live.generation != generation || source >= live.sources.len() {
            return;
        }
        let branch = &mut live.sources[source];
        branch.negotiated = true;
        self.sizes.insert(branch.spec.clone(), (width, height));
        if branch.size == Some((width, height)) {
            return;
        }
        branch.size = Some((width, height));
        self.apply_layout();
    }

    /// A live source's stream ended (EOS into its tee): it is dead, so the
    /// next live scene, take or retry rebuilds it. A take that records it
    /// loses those streams through its bins' EOS (`StreamLost`).
    fn on_source_eos(&mut self, generation: u64, source: usize) {
        let Some(live) = &mut self.live else { return };
        if live.generation != generation {
            return;
        }
        let Some(branch) = live.sources.get_mut(source) else {
            return;
        };
        if branch.dead.is_some() {
            return;
        }
        branch.dead = Some(STOPPED_SENDING.to_owned());
        if !branch.negotiated {
            self.sizes.remove(&branch.spec);
        }
        let text = format!("{}: {STOPPED_SENDING}", branch.spec.describe());
        tracing::warn!("capture: {text}");
        let recorded = self
            .take
            .as_ref()
            .is_some_and(|run| run.bins.iter().any(|b| b.source == source));
        if !recorded {
            self.side(SideEvent::LiveError(text));
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
        match (self.take_deadline(), self.casts_close_at) {
            (Some(take), Some(casts)) => Some(take.min(casts)),
            (take, casts) => take.or(casts),
        }
    }

    fn take_deadline(&self) -> Option<Instant> {
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
        if self.casts_close_at.is_some_and(|at| now >= at) {
            self.end_cast_grace();
        }
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
        let casts: Vec<OpenCast> = std::mem::take(&mut self.casts).into_values().collect();
        self.close_casts(casts);
        self.casts_close_at = None;
        if let Some(ack) = self.shutdown_ack.take() {
            let _ = ack.send(());
        }
        self.done = true;
    }
}

/// The live source a [`SourceSpec`] is, for `SideEvent::LiveStatus`.
fn live_source(spec: &SourceSpec) -> LiveSource {
    match spec {
        SourceSpec::Screen { output } => LiveSource::Screen {
            output: output.clone(),
        },
        SourceSpec::Camera { device, .. } => LiveSource::Camera {
            device: device.clone(),
        },
        SourceSpec::Audio { device } => LiveSource::Audio {
            device: device.clone(),
        },
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
    use crate::capture::FakePortal;

    /// `LiveSourceState::Refused { cancelled }` comes from the portal's
    /// words: a closed dialog (the real portal's `ResponseError::Cancelled`
    /// and the fake portal's `cancel`) reads "cancelled in the dialog".
    #[test]
    fn a_closed_dialog_is_told_from_a_denial() {
        let cancelled = FakePortal::Cancel.refusal("FAKE-1");
        let denied = FakePortal::Deny.refusal("FAKE-1");
        assert!(cancelled.contains(CANCELLED_IN_THE_DIALOG), "{cancelled}");
        assert!(!denied.contains(CANCELLED_IN_THE_DIALOG), "{denied}");
    }

    #[test]
    fn statuses_name_their_source() {
        let camera = SourceSpec::Camera {
            device: "/dev/video0".into(),
            mode: None,
        };
        assert_eq!(
            live_source(&camera),
            LiveSource::Camera {
                device: "/dev/video0".into()
            }
        );
        assert_eq!(
            live_source(&SourceSpec::Screen {
                output: "DP-4".into()
            })
            .name(),
            "DP-4"
        );
    }

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
