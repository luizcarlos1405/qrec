//! The capture engine (A§8.11, `CaptureEngine`): the live pipeline, record
//! bins attached at T0 (buffer probes, `videorate skip-to-first`, x264/VA
//! encoders, matroskamux), first-buffer and health checks, EOS/teardown
//! timeouts (`EngineWedged`), the ScreenCast portal (ashpd under
//! `futures_lite::block_on`, restore tokens) and the fake backend
//! (`QREC_CAPTURE_BACKEND=fake`).
//!
//! All GStreamer calls happen on one engine thread ([`GstCaptureEngine`]);
//! the trait methods only queue commands, so the session thread never
//! blocks. Results come back as `Event`s through the `Inbox` given to
//! `set_live`/`start`; level meters and preview frames go to the optional
//! [`SideEvent`] channel, never through `on_event`.
//!
//! The contract with the core (A§5.3, A§8.11):
//! - `SetLive` arrives whenever the wanted live scene changes, before
//!   `StartCapture`: the pipeline is built then and the take attaches to it
//!   (a plan source the pipeline lacks rebuilds it first). An equal scene
//!   with a healthy pipeline changes nothing; a new layout only moves
//!   preview layers; `None` tears the pipeline down once no take runs.
//! - `StartCapture` ⇒ `CaptureStarted` (every file got data and the health
//!   window passed) or `CaptureFailed { source }`. Afterwards, a failure of
//!   one branch is `StreamLost` for each stream it fed; RecordClock ~4 Hz.
//! - A take that is stopped gets one `CaptureStopped` (a repeated stop is
//!   absorbed; a take that failed or was never seen gets an empty one),
//!   with `timed_out` when EOS missed its deadline. A teardown that misses
//!   its deadline is `EngineWedged`.
//!
//! Hosts build the engine with [`new_engine`]:
//!
//! ```ignore
//! let dirs = UserDirs::resolve(&Env::from_process())?;
//! let opts = EngineOptions::new(project_dir, Backend::from_env(), &dirs);
//! let deps = Deps { capture: capture::new_engine(opts), .. };
//! ```
//!
//! Without the `gst` feature the engine fails every start (`NoCapture`).
//!
//! The GUI also gets (docs/GUI.md G§3.9): the preview in its own sink
//! ([`PreviewOutput`], once [`PREVIEW_SINK_SUPPORTED`]), each live source's
//! state ([`SideEvent::LiveStatus`]), a [`LiveControl`] that moves preview
//! layers during a drag, screen casts kept open for [`GUI_CAST_GRACE`]
//! after the pipeline stops, and the meters of `meters::MeterMonitor`
//! while no capture pipeline runs.

pub mod fake;
#[cfg(feature = "gst")]
pub mod meters;
pub mod spec;
pub mod tokens;

#[cfg(feature = "gst")]
mod bridge;
#[cfg(all(test, feature = "gst"))]
mod cast_probe;
#[cfg(feature = "gst")]
mod engine;
#[cfg(all(test, feature = "gst"))]
mod fake_tests;
#[cfg(feature = "gst")]
mod live;
#[cfg(feature = "gst")]
mod portal;
#[cfg(feature = "gst")]
mod record;
#[cfg(all(test, feature = "gst"))]
mod tests;
#[cfg(feature = "gst")]
mod util;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use qrec_core::ctx::{CameraMode, ScreenInfo};
use qrec_core::model::Scene;
use qrec_core::view::{LiveSourceView, LiveSources};

use crate::deps::CaptureEngine;
use crate::paths::UserDirs;

#[cfg(feature = "gst")]
pub use engine::{FakeFailure, GstCaptureEngine};
pub use fake::{FakeHooks, FakeInventory, FakeInventoryKind, FakePortal, fake_inventory_of};
/// A live source's state: the view models' type (`qrec_core::view`), so the
/// GUI shows what the engine reports without converting it.
pub use qrec_core::view::LiveSourceState;
pub use tokens::{PortalTokens, TokenEntry};

/// Selects the backend: `fake` uses test sources, anything else the devices.
pub const BACKEND_ENV: &str = "QREC_CAPTURE_BACKEND";

/// The app id registered with the portal (SPEC header).
pub const APP_ID: &str = "com.rsensato.qrec";

/// Record bins and pipelines must reach NULL within this (else `EngineWedged`).
pub const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// `RecordClock` period (~4 Hz).
pub const CLOCK_INTERVAL: Duration = Duration::from_millis(250);

/// After a stop, how long a bin waits for its source to reach T1 before it
/// is closed anyway (a stalled source).
pub const FILL_GRACE: Duration = Duration::from_millis(500);

/// While recording, a video stream that passes no frame for this long is
/// lost (`StreamLost`, SPEC §16): a camera that hangs without an error.
/// It also bounds what a stop fills: never minutes of frames.
pub const STALL_LIMIT: Duration = Duration::from_secs(3);

/// How long a new live pipeline may take to reach PLAYING.
pub const PIPELINE_START_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the user has to answer a screen-share dialog.
pub const PORTAL_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a GUI host keeps its screen casts open after the live pipeline
/// stops (GUI H-4, [`EngineOptions::cast_grace`]): `Tab` back to Live
/// doesn't ask the portal again.
pub const GUI_CAST_GRACE: Duration = Duration::from_secs(60);

/// Whether the engine composites the live preview into
/// [`EngineOptions::preview`] (GUI H-1): BGRx at the canvas fitted into its
/// `max_size`, the same sink across rebuilds. Without a sink the preview is
/// the RGBA appsink's [`SideEvent::Preview`] frames.
pub const PREVIEW_SINK_SUPPORTED: bool = true;

/// Which devices the engine records from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The portal, `v4l2src` and `pulsesrc`.
    Real,
    /// `videotestsrc`/`audiotestsrc`; never touches the portal or devices.
    Fake,
}

impl Backend {
    /// `QREC_CAPTURE_BACKEND`: `fake` ⇒ [`Backend::Fake`], else real.
    pub fn from_env() -> Self {
        Self::parse(std::env::var(BACKEND_ENV).ok().as_deref())
    }

    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("fake") => Backend::Fake,
            None | Some("" | "real") => Backend::Real,
            Some(other) => {
                tracing::warn!("unknown {BACKEND_ENV}={other:?}; using the real devices");
                Backend::Real
            }
        }
    }
}

/// Device facts the real backend needs while it runs: screen geometry (to
/// match portal streams to outputs) and camera modes (to apply the
/// `capture_plan` camera rule to live-only cameras). Hosts pass the
/// `sources` discovery.
pub trait DeviceInfo: Send + Sync {
    fn screens(&self) -> Vec<ScreenInfo>;
    fn camera_modes(&self, device: &str) -> Vec<CameraMode>;
}

/// Out-of-band output for the GUI (A§8.11): never goes through `on_event`.
#[derive(Debug, Clone)]
pub enum SideEvent {
    /// A composited canvas frame of the live preview.
    Preview(PreviewFrame),
    /// Audio levels of a source (~20 Hz).
    Level(LevelReading),
    /// The live pipeline (not a take) has a problem.
    LiveError(String),
    /// Every live source's state, whenever it changes (after builds,
    /// portal answers, deaths and rebuilds that change a source's state or
    /// size), in the scene's order; look a source up by name
    /// ([`live_sources_view`]) (docs/GUI.md G§3.9).
    LiveStatus(Vec<LiveSourceStatus>),
}

/// One live source as the engine sees it ([`SideEvent::LiveStatus`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSourceStatus {
    pub source: LiveSource,
    pub state: LiveSourceState,
    /// The size it delivers, once known: the layer boxes in Live mode.
    pub size: Option<(u32, u32)>,
}

/// A capture device of the live pipeline.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LiveSource {
    /// By output name (`DP-4`).
    Screen { output: String },
    /// By device (`/dev/video0`).
    Camera { device: String },
    /// By PulseAudio/PipeWire source name.
    Audio { device: String },
}

impl LiveSource {
    /// The output, camera device or audio source name: how scenes name it.
    pub fn name(&self) -> &str {
        match self {
            LiveSource::Screen { output } => output,
            LiveSource::Camera { device } | LiveSource::Audio { device } => device,
        }
    }
}

impl LiveSourceStatus {
    /// What the view models take of it.
    pub fn view(&self) -> LiveSourceView {
        LiveSourceView {
            state: self.state.clone(),
            size: self.size,
        }
    }
}

/// The statuses keyed by source name, as the view models take them
/// (`qrec_core::view::LiveSources`). A name reported twice keeps the last.
pub fn live_sources_view(statuses: &[LiveSourceStatus]) -> LiveSources {
    statuses
        .iter()
        .map(|status| (status.source.name().to_owned(), status.view()))
        .collect()
}

/// Lays out the live preview's layers without a `SetLive`, for a layer
/// drag or nudge in Live mode (GUI D-32, D-33): the layout holds until the
/// next `SetLive` (any) or a `None`. It moves the preview compositor's pads
/// at once and leaves a changed `videocrop` crop to the layer's streaming
/// thread, which applies it with the pad before the next caps event or
/// buffer (a `videoscale` in front answers the source's caps queries for
/// any size, and only the crop sides that change are written; A§8.11), so
/// no crop can leave a source without caps. It never touches State; record
/// bins record each source to its own file and join their tee without a
/// renegotiation, so the layout never reaches a take.
/// Cheap to clone; every call returns at once.
///
/// It also tells where the live picture is ([`Self::progress`], for the
/// GUI's `waitIdle`, G§3.18).
///
/// From `GstCaptureEngine::control()`; one that isn't connected to an
/// engine does nothing.
#[derive(Clone, Default)]
pub struct LiveControl {
    apply: Option<Arc<dyn Fn(Option<Scene>) + Send + Sync>>,
    tracker: Option<Arc<LiveTracker>>,
}

impl LiveControl {
    /// Hands every layout to `apply` (the engine queues it).
    pub fn new(apply: impl Fn(Option<Scene>) + Send + Sync + 'static) -> Self {
        Self {
            apply: Some(Arc::new(apply)),
            tracker: None,
        }
    }

    /// Reports `tracker`'s [`progress`](Self::progress).
    pub fn with_tracker(mut self, tracker: Arc<LiveTracker>) -> Self {
        self.tracker = Some(tracker);
        self
    }

    /// A control that does nothing.
    pub fn noop() -> Self {
        Self::default()
    }

    /// Shows the preview's layers laid out as `scene` (the same layers and
    /// sources as the live scene); `None` goes back to the live scene's own.
    pub fn transient_layout(&self, scene: Option<Scene>) {
        if let Some(apply) = &self.apply {
            apply(scene);
        }
    }

    /// Where the engine is with the live scenes it was given; `None`
    /// without an engine.
    pub fn progress(&self) -> Option<LiveProgress> {
        self.tracker.as_ref().map(|t| t.progress())
    }
}

impl std::fmt::Debug for LiveControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveControl")
            .field("connected", &self.apply.is_some())
            .field("progress", &self.progress())
            .finish()
    }
}

/// [`LiveControl::progress`]: where the engine is with the live scenes it
/// was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveProgress {
    /// Every `set_live` and `retry_live` so far was handled, and each
    /// video source of the live pipeline negotiated its caps in it (or
    /// died); also with no live pipeline (Program, a portal dialog open,
    /// none wanted). A rebuild that keeps every source's state and size
    /// sends no `LiveStatus`: this is how a GUI knows it is in progress.
    pub settled: bool,
    /// The live pipeline the preview comes from (bumped by every build, 0
    /// before the first): a new one draws its own first frames.
    pub generation: u64,
}

/// The engine's side of [`LiveProgress`], shared by `GstCaptureEngine`,
/// its thread and its [`LiveControl`]s. `set_live` and `retry_live` count
/// themselves on the caller's thread before the engine thread can see
/// them, so a `waitIdle` that comes after a request was answered never
/// sees the last pipeline as settled; the engine thread publishes what it
/// handled after each command (negotiation first, then the count).
#[derive(Debug)]
pub struct LiveTracker {
    asked: AtomicU64,
    handled: AtomicU64,
    negotiated: AtomicBool,
    generation: AtomicU64,
}

impl Default for LiveTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveTracker {
    /// A tracker with nothing asked yet (settled).
    pub fn new() -> Self {
        Self {
            asked: AtomicU64::new(0),
            handled: AtomicU64::new(0),
            negotiated: AtomicBool::new(true),
            generation: AtomicU64::new(0),
        }
    }

    /// A `set_live` or `retry_live` is on its way to the engine thread.
    pub fn ask(&self) {
        self.asked.fetch_add(1, Ordering::SeqCst);
    }

    /// One that [`ask`](Self::ask) counted never reached the engine thread.
    pub fn unask(&self) {
        self.asked.fetch_sub(1, Ordering::SeqCst);
    }

    /// The engine thread, after a command: it handled `handled` asks in
    /// all, the live pipeline `generation` (0: none) shows, and its video
    /// sources negotiated (or died, or there is none) when `negotiated`.
    pub fn publish(&self, handled: u64, negotiated: bool, generation: u64) {
        self.generation.store(generation, Ordering::SeqCst);
        self.negotiated.store(negotiated, Ordering::SeqCst);
        self.handled.store(handled, Ordering::SeqCst);
    }

    pub fn progress(&self) -> LiveProgress {
        // `handled` first: a count that includes the last ask comes with
        // the negotiation state published with it, or a later one.
        let handled = self.handled.load(Ordering::SeqCst);
        let negotiated = self.negotiated.load(Ordering::SeqCst);
        let asked = self.asked.load(Ordering::SeqCst);
        LiveProgress {
            settled: handled >= asked && negotiated,
            generation: self.generation.load(Ordering::SeqCst),
        }
    }
}

/// Where the engine composites the live preview for the GUI (GUI H-1,
/// docs/GUI.md G§3.9): BGRx at the canvas fitted into `max_size`.
#[cfg(feature = "gst")]
#[derive(Debug, Clone)]
pub struct PreviewOutput {
    /// The GUI's `gtk4paintablesink`, made on the GTK thread, which holds a
    /// reference until exit. The engine reuses it across rebuilds.
    pub sink: gst::Element,
    /// (960, 540) × the window's scale factor.
    pub max_size: (u32, u32),
}

/// One RGBA frame of the preview.
#[derive(Clone)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row.
    pub stride: u32,
    pub rgba: Vec<u8>,
    pub pts_ns: Option<u64>,
}

impl std::fmt::Debug for PreviewFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("bytes", &self.rgba.len())
            .field("pts_ns", &self.pts_ns)
            .finish()
    }
}

/// Per-channel RMS and peak levels in dB of one audio source.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelReading {
    /// The PulseAudio/PipeWire source name.
    pub device: String,
    pub rms_db: Vec<f64>,
    pub peak_db: Vec<f64>,
}

/// How to build an engine.
#[derive(Clone)]
pub struct EngineOptions {
    /// Plan paths are joined with it.
    pub project_dir: PathBuf,
    pub backend: Backend,
    /// `<state>/portal-tokens.json`.
    pub portal_tokens: PathBuf,
    /// Screen and camera facts for the real backend.
    pub devices: Option<Arc<dyn DeviceInfo>>,
    /// Preview frames, level meters and live statuses (GUI).
    pub side: Option<crossbeam_channel::Sender<SideEvent>>,
    pub portal_timeout: Duration,
    /// Register `com.rsensato.qrec` with the host portal Registry first.
    pub register_app_id: bool,
    /// The GUI's preview sink (`None`: the RGBA appsink and
    /// [`SideEvent::Preview`]); used once [`PREVIEW_SINK_SUPPORTED`].
    #[cfg(feature = "gst")]
    pub preview: Option<PreviewOutput>,
    /// After `SetLive(None)` with no take, screen casts stay open this long,
    /// then close: [`GUI_CAST_GRACE`] for the GUI, zero for other hosts.
    pub cast_grace: Duration,
    /// The fake backend's test hooks ([`FakeHooks`]).
    pub fake: FakeHooks,
}

impl EngineOptions {
    /// Options with no test hooks, whatever the environment says: code that
    /// runs as the process (the binary's hosts, the GUI's opener) sets
    /// `fake` from [`FakeHooks::from_env`], so an engine a test builds in
    /// its own process sees only the hooks that test sets (A§11).
    pub fn new(project_dir: impl Into<PathBuf>, backend: Backend, dirs: &UserDirs) -> Self {
        Self {
            project_dir: project_dir.into(),
            backend,
            portal_tokens: dirs.portal_tokens(),
            devices: None,
            side: None,
            portal_timeout: PORTAL_TIMEOUT,
            register_app_id: true,
            #[cfg(feature = "gst")]
            preview: None,
            cast_grace: Duration::ZERO,
            fake: FakeHooks::default(),
        }
    }
}

impl std::fmt::Debug for EngineOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("EngineOptions");
        d.field("project_dir", &self.project_dir)
            .field("backend", &self.backend)
            .field("portal_tokens", &self.portal_tokens)
            .field("devices", &self.devices.is_some())
            .field("side", &self.side.is_some())
            .field("portal_timeout", &self.portal_timeout)
            .field("register_app_id", &self.register_app_id);
        #[cfg(feature = "gst")]
        d.field("preview", &self.preview.as_ref().map(|p| p.max_size));
        d.field("cast_grace", &self.cast_grace)
            .field("fake", &self.fake)
            .finish()
    }
}

/// The capture engine for a host: GStreamer when built with `gst` (falls
/// back to `NoCapture` if its thread can't start), else `NoCapture`.
pub fn new_engine(opts: EngineOptions) -> Box<dyn CaptureEngine> {
    #[cfg(feature = "gst")]
    {
        match GstCaptureEngine::spawn(opts) {
            Ok(engine) => Box::new(engine),
            Err(e) => {
                tracing::error!("can't start the capture engine thread: {e}");
                Box::new(crate::deps::NoCapture)
            }
        }
    }
    #[cfg(not(feature = "gst"))]
    {
        let _ = opts;
        Box::new(crate::deps::NoCapture)
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn backend_selection() {
        assert_eq!(Backend::parse(Some("fake")), Backend::Fake);
        assert_eq!(Backend::parse(Some(" fake ")), Backend::Fake);
        assert_eq!(Backend::parse(None), Backend::Real);
        assert_eq!(Backend::parse(Some("")), Backend::Real);
        assert_eq!(Backend::parse(Some("real")), Backend::Real);
        assert_eq!(Backend::parse(Some("bogus")), Backend::Real);
    }

    #[test]
    fn options_point_at_the_state_dir() {
        let env = crate::testkit::TestEnv::new();
        let dirs = UserDirs::resolve(&env.paths_env()).unwrap();
        let opts = EngineOptions::new("/p", Backend::Fake, &dirs);
        assert_eq!(
            opts.portal_tokens,
            env.state_home().join("qrec/portal-tokens.json")
        );
        assert!(opts.register_app_id);
        assert!(format!("{opts:?}").contains("Fake"));
        // Casts close at once unless a GUI asks for the grace.
        assert_eq!(opts.cast_grace, Duration::ZERO);
        assert_eq!(GUI_CAST_GRACE, Duration::from_secs(60));
        let real = EngineOptions::new("/p", Backend::Real, &dirs);
        assert_eq!(real.fake, FakeHooks::default(), "only the fake backend");
        #[cfg(feature = "gst")]
        assert!(real.preview.is_none());
    }

    /// While a developer has a fake hook exported (for the manual shots,
    /// G§8.4), the engines, Sessions and fake devices a test builds in its
    /// own process see none of it (A§11): only what runs as the process
    /// reads the hooks. Checked in a child of this test binary with every
    /// hook set. They used to take them from the environment, so an
    /// exported `QREC_FAKE_PORTAL=deny` refused every in-process recording.
    #[test]
    fn in_process_fakes_ignore_the_hooks_in_the_environment() {
        use crate::facts::{FakeSources, Sources};
        const CHILD: &str = "QREC_TEST_HOOKS_CHILD";
        const NAME: &str =
            "capture::unit_tests::in_process_fakes_ignore_the_hooks_in_the_environment";
        if std::env::var_os(CHILD).is_some() {
            let env = crate::testkit::TestEnv::new();
            let dirs = UserDirs::resolve(&env.paths_env()).unwrap();
            let opts = EngineOptions::new("/p", Backend::Fake, &dirs);
            assert_eq!(opts.fake, FakeHooks::default());
            let session = crate::session::SessionEnv::for_dirs(dirs, true);
            assert_eq!(session.fake_inventory, FakeInventoryKind::Plain);
            let mut sources = FakeSources(session.fake_inventory);
            assert_eq!(sources.screens().unwrap()[0].description, "Fake screen 1");
            assert_eq!(sources.screens().unwrap()[0].width, 1280);
            let plain = crate::sources::SourceInventory::fake();
            assert_eq!(plain.screens[1].description, "Fake screen 2");
            // What runs as the process still reads them.
            assert_eq!(FakeHooks::from_env().portal, Some(FakePortal::Deny));
            assert_eq!(FakeHooks::from_env().record_offset_frames, 4842);
            assert_eq!(FakeInventoryKind::from_env(), FakeInventoryKind::Design);
            return;
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env(fake::FAKE_PORTAL_ENV, "deny")
            .env(fake::FAKE_INVENTORY_ENV, "design")
            .env(fake::FAKE_EOS_DELAY_ENV, "5000")
            .env(fake::FAKE_RECORD_OFFSET_ENV, "4842")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn live_statuses_become_the_view_models_map() {
        let statuses = [
            LiveSourceStatus {
                source: LiveSource::Screen {
                    output: "FAKE-1".into(),
                },
                state: LiveSourceState::Refused {
                    cancelled: false,
                    message: "denied".into(),
                },
                size: None,
            },
            LiveSourceStatus {
                source: LiveSource::Camera {
                    device: "/dev/fake0".into(),
                },
                state: LiveSourceState::Live,
                size: Some((640, 480)),
            },
            LiveSourceStatus {
                source: LiveSource::Audio {
                    device: "alsa_input.fake".into(),
                },
                state: LiveSourceState::AwaitingPortal,
                size: None,
            },
        ];
        let view = live_sources_view(&statuses);
        assert_eq!(view.len(), 3);
        assert_eq!(
            view["/dev/fake0"],
            LiveSourceView {
                state: LiveSourceState::Live,
                size: Some((640, 480))
            }
        );
        assert!(matches!(
            view["FAKE-1"].state,
            LiveSourceState::Refused {
                cancelled: false,
                ..
            }
        ));
        assert_eq!(statuses[2].source.name(), "alsa_input.fake");
    }

    #[test]
    fn a_live_control_hands_layouts_on_or_does_nothing() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let control = {
            let seen = Arc::clone(&seen);
            LiveControl::new(move |scene: Option<Scene>| {
                seen.lock().unwrap().push(scene.map(|s| s.layers.len()));
            })
        };
        let copy = control.clone();
        copy.transient_layout(Some(Scene::default()));
        control.transient_layout(None);
        assert_eq!(*seen.lock().unwrap(), [Some(0), None]);
        assert!(format!("{control:?}").contains("connected: true"));
        // Not connected: nothing happens.
        LiveControl::noop().transient_layout(None);
        assert!(format!("{:?}", LiveControl::default()).contains("connected: false"));
    }

    /// The engine's side of `LiveControl::progress`: an ask counts from the
    /// caller's thread, so the last pipeline's state never settles it; the
    /// engine settles it once it handled it and the pipeline it shows has
    /// every video source negotiated.
    #[test]
    fn the_live_tracker_settles_an_ask_once_its_pipeline_negotiated() {
        let tracker = Arc::new(LiveTracker::new());
        let control = LiveControl::noop().with_tracker(Arc::clone(&tracker));
        let at = |settled, generation| {
            Some(LiveProgress {
                settled,
                generation,
            })
        };
        assert_eq!(control.progress(), at(true, 0), "nothing asked");
        tracker.publish(0, true, 1);
        assert_eq!(control.progress(), at(true, 1));
        tracker.ask();
        // Not handled yet: pipeline 1 negotiated, but that's the old one.
        assert_eq!(control.progress(), at(false, 1));
        // Handled: it built pipeline 2, whose sources negotiate.
        tracker.publish(1, false, 2);
        assert_eq!(control.progress(), at(false, 2));
        tracker.publish(1, true, 2);
        assert_eq!(control.progress(), at(true, 2));
        // One that never reached the engine thread.
        tracker.ask();
        tracker.unask();
        assert_eq!(control.progress(), at(true, 2));
        assert!(format!("{control:?}").contains("settled: true"));
        assert_eq!(LiveControl::noop().progress(), None);
    }
}
