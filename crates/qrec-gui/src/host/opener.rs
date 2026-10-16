//! The GUI's Session (G§3.4.2): what `GuiHost::open` opens on the
//! `qrec-session` thread (startup step 7).
//!
//! - The capture engine records and shows the live preview: its side
//!   channel (preview frames, levels, live statuses) goes to the media hub's
//!   pump; its [`LiveControl`] goes to the media hub through the queue
//!   before the engine is boxed. Once the engine composites into the GUI's
//!   sink (`capture::PREVIEW_SINK_SUPPORTED`, WP1), it gets the live
//!   `gtk4paintablesink` made on the GTK thread; until then its RGBA appsink
//!   feeds `SideEvent::Preview` and the hub shows those frames. Under the
//!   fake backend it takes the process's test hooks
//!   ([`FakeHooks::from_env`], G§8.4): `EngineOptions::new` sets none.
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
    self, Backend, EngineOptions, FakeHooks, GUI_CAST_GRACE, GstCaptureEngine, LiveControl,
    PreviewOutput, SideEvent,
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
    /// The GUI's engine options: the side channel, live device facts, the
    /// preview sink once supported, the cast grace (H-4) and, under the
    /// fake backend, this process's test hooks (G§8.4).
    fn engine_options(&self, dir: &Path, env: &SessionEnv) -> EngineOptions {
        let backend = Backend::from_env();
        let mut opts = EngineOptions::new(dir, backend, &env.dirs);
        opts.side = Some(self.side.clone());
        opts.devices = Some(Arc::new(LiveDevices));
        opts.cast_grace = GUI_CAST_GRACE;
        // `EngineOptions::new` takes no hooks (an engine a test builds in
        // its own process never sees exported ones, A§11): the window runs
        // as the process, so it reads them like the binary's hosts do.
        if backend == Backend::Fake {
            opts.fake = FakeHooks::from_env();
        }
        opts.preview = capture::PREVIEW_SINK_SUPPORTED
            .then(|| self.live_sink.clone())
            .flatten()
            .map(|sink| PreviewOutput {
                sink,
                max_size: self.max_size,
            });
        opts
    }

    /// The capture engine with [`engine_options`](Self::engine_options).
    fn engine(&self, dir: &Path, env: &SessionEnv) -> Box<dyn CaptureEngine> {
        match GstCaptureEngine::spawn(self.engine_options(dir, env)) {
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

#[cfg(test)]
mod tests {
    use qrec_shell::capture::BACKEND_ENV;
    use qrec_shell::capture::fake::{
        FAKE_EOS_DELAY_ENV, FAKE_PORTAL_ENV, FAKE_RECORD_OFFSET_ENV, FakePortal,
    };
    use qrec_shell::paths::UserDirs;
    use qrec_shell::testkit::TestEnv;

    use super::*;
    use crate::host::Port;
    use crate::host::queue::Queue;

    fn opener() -> GuiOpener {
        GuiOpener {
            port: Arc::new(Port::new(Queue::new(|| {}))),
            config: ConfigFile::new("/nonexistent/config.toml".into()),
            sources: SharedSources::new(),
            side: crossbeam_channel::unbounded().0,
            live_sink: None,
            max_size: PREVIEW_MAX,
            on_control: Box::new(|_| {}),
        }
    }

    /// G§3.4.2 (WP0a review): `EngineOptions::new` takes no test hooks, so
    /// the window, which runs as the process, sets the ones exported to it
    /// under the fake backend, like the binary's hosts. Checked in a child
    /// of this test binary with the hooks exported (A§11: this process's
    /// own engines never see them).
    #[test]
    fn the_engine_takes_the_processs_fake_hooks() {
        const CHILD: &str = "QREC_GUI_TEST_HOOKS_CHILD";
        const NAME: &str = "host::opener::tests::the_engine_takes_the_processs_fake_hooks";
        if let Some(backend) = std::env::var_os(CHILD) {
            let env = TestEnv::new();
            let dirs = UserDirs::resolve(&env.paths_env()).unwrap();
            let session = SessionEnv::for_dirs(dirs, backend == "fake");
            let opts = opener().engine_options(Path::new("/p"), &session);
            assert_eq!(opts.cast_grace, GUI_CAST_GRACE);
            assert!(opts.side.is_some() && opts.devices.is_some());
            if backend == "fake" {
                assert_eq!(opts.backend, Backend::Fake);
                assert_eq!(opts.fake.portal, Some(FakePortal::Deny));
                assert_eq!(opts.fake.record_offset_frames, 4842);
                assert_eq!(opts.fake, FakeHooks::from_env());
            } else {
                // The real backend never takes them.
                assert_eq!(opts.backend, Backend::Real);
                assert_eq!(opts.fake, FakeHooks::default());
            }
            return;
        }
        for backend in ["fake", "real"] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(CHILD, backend)
                .env(FAKE_PORTAL_ENV, "deny")
                .env(FAKE_EOS_DELAY_ENV, "5000")
                .env(FAKE_RECORD_OFFSET_ENV, "4842");
            if backend == "fake" {
                child.env(BACKEND_ENV, "fake");
            } else {
                child.env_remove(BACKEND_ENV);
            }
            let out = child.output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.contains("1 passed"),
                "{backend}: {stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
