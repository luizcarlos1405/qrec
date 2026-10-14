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
//! Hosts build the engine with [`new_engine`]:
//!
//! ```ignore
//! let dirs = UserDirs::resolve(&Env::from_process())?;
//! let opts = EngineOptions::new(project_dir, Backend::from_env(), &dirs);
//! let deps = Deps { capture: capture::new_engine(opts), .. };
//! ```
//!
//! Without the `gst` feature the engine fails every start (`NoCapture`).

pub mod fake;
pub mod spec;
pub mod tokens;

#[cfg(feature = "gst")]
mod engine;
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
use std::time::Duration;

use qrec_core::ctx::{CameraMode, ScreenInfo};

use crate::deps::CaptureEngine;
use crate::paths::UserDirs;

#[cfg(feature = "gst")]
pub use engine::{FakeFailure, GstCaptureEngine};
pub use fake::{FakeInventory, fake_inventory};
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

/// How long a new live pipeline may take to reach PLAYING.
pub const PIPELINE_START_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the user has to answer a screen-share dialog.
pub const PORTAL_TIMEOUT: Duration = Duration::from_secs(120);

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
    /// Preview frames and level meters (GUI).
    pub side: Option<crossbeam_channel::Sender<SideEvent>>,
    pub portal_timeout: Duration,
    /// Register `com.rsensato.qrec` with the host portal Registry first.
    pub register_app_id: bool,
}

impl EngineOptions {
    pub fn new(project_dir: impl Into<PathBuf>, backend: Backend, dirs: &UserDirs) -> Self {
        Self {
            project_dir: project_dir.into(),
            backend,
            portal_tokens: dirs.portal_tokens(),
            devices: None,
            side: None,
            portal_timeout: PORTAL_TIMEOUT,
            register_app_id: true,
        }
    }
}

impl std::fmt::Debug for EngineOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineOptions")
            .field("project_dir", &self.project_dir)
            .field("backend", &self.backend)
            .field("portal_tokens", &self.portal_tokens)
            .field("devices", &self.devices.is_some())
            .field("side", &self.side.is_some())
            .field("portal_timeout", &self.portal_timeout)
            .field("register_app_id", &self.register_app_id)
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
        assert_eq!(opts.portal_tokens, env.state_home().join("qrec/portal-tokens.json"));
        assert!(opts.register_app_id);
        assert!(format!("{opts:?}").contains("Fake"));
    }
}
