//! The GUI's Session (G§3.4.2): what `GuiHost::open` opens on the
//! `qrec-session` thread (startup step 7).
//!
//! - The capture engine records and shows the live preview: its side
//!   channel (preview frames, levels, live statuses) goes to the media hub's
//!   pump; its [`LiveControl`] goes to the media hub through the queue
//!   before the engine is boxed. Once the engine composites into the GUI's
//!   sink (`capture::PREVIEW_SINK_SUPPORTED`, WP1), it gets the live
//!   `gtk4paintablesink` made on the GTK thread; until then its RGBA appsink
//!   feeds `SideEvent::Preview` and the hub shows those frames.
//! - ffmpeg for media, the system clock, OS entropy.
//! - `SessionConfig`: the share of the lock, the environment with the
//!   `config.toml` read at step 1 (a broken one is reported once, A§4.2),
//!   the port as notifier and observer (H-6), and the shared source cache
//!   (H-5), so device facts never block the session thread.

use std::path::Path;
use std::sync::Arc;

use crossbeam_channel::Sender;
use qrec_core::state::HostKind;
use qrec_shell::caps;
use qrec_shell::capture::{
    self, Backend, EngineOptions, GUI_CAST_GRACE, GstCaptureEngine, LiveControl, PreviewOutput,
    SideEvent,
};
use qrec_shell::deps::{CaptureEngine, Deps, NoCapture, OpenOpts, os_entropy, system_clock};
use qrec_shell::error::ShellError;
use qrec_shell::facts::{ConfigFile, SharedSources};
use qrec_shell::host::gui::{PortNotifier, PortObserver};
use qrec_shell::host::{GuiPort, HostSession, Opener, SessionLock};
use qrec_shell::media::FfmpegRunner;
use qrec_shell::session::{Session, SessionConfig, SessionEnv};
use qrec_shell::sources::LiveDevices;

/// The live preview's size cap before the window's scale factor (G§3.4.2).
pub const PREVIEW_MAX: (u32, u32) = (960, 540);

/// Opens the GUI's Session (G§3.4.2). Made on the GTK thread at step 7,
/// used once on the `qrec-session` thread.
pub struct GuiOpener {
    /// The window's port: the Session's observer and notifier.
    pub port: Arc<dyn GuiPort>,
    /// `config.toml` as startup step 1 read it.
    pub config: ConfigFile,
    /// The device facts cache the window refreshes on `qrec-gui-io`.
    pub sources: SharedSources,
    /// The media hub's side channel.
    pub side: Sender<SideEvent>,
    /// The live `gtk4paintablesink` (step 5), if one could be made.
    pub live_sink: Option<gst::Element>,
    /// [`PREVIEW_MAX`] × the window's scale factor at step 5.
    pub max_size: (u32, u32),
    /// Hands the engine's [`LiveControl`] to the media hub (through the
    /// queue), before the engine is boxed.
    pub on_control: Box<dyn Fn(LiveControl) + Send>,
}

impl std::fmt::Debug for GuiOpener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuiOpener")
            .field("config", &self.config.path())
            .field("live_sink", &self.live_sink.is_some())
            .field("max_size", &self.max_size)
            .finish_non_exhaustive()
    }
}

impl GuiOpener {
    /// The capture engine with the GUI's options: the side channel, live
    /// device facts, the preview sink once supported, the cast grace (H-4).
    fn engine(&self, dir: &Path, env: &SessionEnv) -> Box<dyn CaptureEngine> {
        let mut opts = EngineOptions::new(dir, Backend::from_env(), &env.dirs);
        opts.side = Some(self.side.clone());
        opts.devices = Some(Arc::new(LiveDevices));
        opts.cast_grace = GUI_CAST_GRACE;
        opts.preview = capture::PREVIEW_SINK_SUPPORTED
            .then(|| self.live_sink.clone())
            .flatten()
            .map(|sink| PreviewOutput {
                sink,
                max_size: self.max_size,
            });
        match GstCaptureEngine::spawn(opts) {
            Ok(engine) => {
                (self.on_control)(engine.control());
                Box::new(engine)
            }
            Err(e) => {
                tracing::error!("can't start the capture engine thread: {e}");
                Box::new(NoCapture)
            }
        }
    }
}

impl Opener for GuiOpener {
    fn open(
        &mut self,
        dir: &Path,
        opts: OpenOpts,
        lock: Option<SessionLock>,
    ) -> Result<Box<dyn HostSession>, ShellError> {
        let mut env = SessionEnv::from_process()?;
        // The file startup step 1 read, so a broken one is reported once.
        if self.config.path() == env.config.path() {
            env.config = self.config.clone();
        }
        // The VA-API encoder is found in a probe child; never under the
        // fake backend, which records without a GPU.
        if !env.fake_backend {
            env.caps.gst_factory = Some(caps::gst_factory_probe);
        }
        let capture = if opts.mutating && opts.host == HostKind::Gui {
            self.engine(dir, &env)
        } else {
            Box::new(NoCapture)
        };
        let deps = Deps {
            capture,
            media: Box::new(FfmpegRunner::new(dir)),
            clock: Box::new(system_clock),
            entropy: Box::new(os_entropy),
        };
        let cfg = SessionConfig {
            lock,
            env: Some(env),
            notifier: Some(Box::new(PortNotifier(Arc::clone(&self.port)))),
            observer: Some(Box::new(PortObserver(Arc::clone(&self.port)))),
            sources: Some(Box::new(self.sources.clone())),
            ..SessionConfig::default()
        };
        Ok(Box::new(Session::open_with(dir, opts, deps, cfg)?))
    }
}
