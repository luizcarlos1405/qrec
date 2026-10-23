//! The application (G§3.2, G§3.3, D-12): `qrec_gui::run`, the libadwaita
//! application, `activate`, the queue's drain, the close sequence and the
//! exit code.
//!
//! - `run`: the program's name (the app id: X11's `WM_CLASS`), the signal
//!   handlers, startup step 1, then steps 3 and 4 at once when the
//!   window opens on Wayland (the lock and the socket within
//!   milliseconds of launch; their results wait in the queue), else once
//!   GTK connected (an X11 display can't be checked without connecting),
//!   then `adw::Application` (`com.rsensato.qrec`, `NON_UNIQUE`) on this
//!   thread; returns the exit code the app stored before quitting. GDK uses
//!   exactly the backend [`display_target`] picks: Wayland when something
//!   listens on its socket, else X11 when `DISPLAY` names one (a
//!   `GDK_BACKEND` counts when its backend's display is there;
//!   [`prepare_display`], called while `main` is still single-threaded,
//!   after the GPU environment, drops one that names none). Without a
//!   display `run` fails before touching the folder.
//! - `activate`: the store, the media hub, the key router, the theme, the
//!   window's icon (the app icon) and the window, sized to the monitors
//!   (`window::first_size`); the settings follower (`config.toml` and
//!   `keys.toml` live, WP10); the X2 or unreadable-folder page; the first
//!   drain.
//! - The drain: everything the queue holds, in order, on the main thread
//!   (a thread-local holds the app; before it is up the items wait).
//! - Closing (window close, `app.quit`, D-12): a countdown is cancelled; a
//!   take being recorded is stopped and the window stays until it is
//!   finalized; a running export asks first ("Quit while exporting?");
//!   then the host closes (it waits until quiet, ≤ 60 s) and the app is
//!   held until it stopped; then the window goes and the main loop runs on
//!   until the player and the meter monitors reached NULL (each within
//!   5 s; at most [`MEDIA_STOP_WAIT`]: R4). The exit code is the blocking
//!   page's when one is up then (a session crash's: 1), else the close's.
//! - A panic in `activate` (caught, G§3.12) would leave a window that
//!   never shows holding the project: the app quits with 1 instead.

use std::cell::{Cell, RefCell};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use adw::prelude::*;
use chrono::Utc;
use gtk::{gio, glib};
use qrec_core::model::Project;
use qrec_core::on_event::output::export_readout;
use qrec_core::protocol::ToastLevel;
use qrec_core::state::{HostKind, JobKind, RecordPhase, State};
use qrec_core::view::notices::Notice;
use qrec_core::{Command, CoreError, History, UiState};
use qrec_shell::host::HostEnd;
use qrec_shell::host::signals::{self, HOST_SIGNALS, Interrupts};

use crate::host::queue::{Inbound, Queue};
use crate::keys::KeyRouter;
use crate::keys::actions::ActionRouter;
use crate::media::MediaHub;
use crate::startup::{self, Startup};
use crate::store::Store;
use crate::store::local::{LocalUi, Overlay, StartupPhase};
use crate::theme::{MotionPolicy, Theme};
use crate::window::MainWindow;
use crate::{Ctx, GuiArgs, blocking, guard, guarded, testctl};

/// The application id (SPEC §15, the portal's app id).
pub const APP_ID: &str = "com.rsensato.qrec";

/// The longest the quit waits for the player and the meter monitors to
/// reach NULL (they bound themselves at 5 s each, R4, R6).
pub const MEDIA_STOP_WAIT: Duration = Duration::from_secs(6);

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
    static QUEUE: RefCell<Option<Queue>> = const { RefCell::new(None) };
    static EXIT_CODE: Cell<u8> = const { Cell::new(0) };
}

/// Where the close sequence is (D-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closing {
    No,
    /// A take is being stopped: close once the phase is Idle.
    WaitingForIdle,
    /// The host was asked to close: quit once it stopped.
    WaitingForHost,
}

/// The running window.
pub struct App {
    pub app: adw::Application,
    pub ctx: Ctx,
    pub window: MainWindow,
    pub startup: Startup,
    /// The user's settings, followed live (WP10).
    pub follower: crate::follow::Follower,
    closing: Cell<Closing>,
    /// The exit code once the host stopped (a page's, or 0).
    exit_after_host: Cell<u8>,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    quitting: Cell<bool>,
    /// The test control channel (test mode only; the socket goes with it).
    channel: RefCell<Option<testctl::Channel>>,
}

/// Registers GTK's fundamental types (`GdkEvent`, `GskRenderNode`,
/// `GtkExpression`) on the main thread before any other thread can register
/// one. GLib hands out a fundamental type's ID (`g_type_fundamental_next`)
/// and registers it in a second step, so two threads registering at once
/// can take the same ID: GStreamer's init does on `qrec-gst` (`GstFlagSet`,
/// `GstFraction`, … for its values) while `gtk::init` runs here, and once
/// (WP10's scale runs, under load) "cannot register existing fundamental
/// type 'GstFlagSet' (as 'GdkEvent')": GTK had no event type, and the
/// window never opened. Registering a type needs no display.
pub fn register_fundamentals() {
    let _ = gtk::gdk::Event::static_type();
    let _ = gtk::gsk::RenderNode::static_type();
    let _ = gtk::Expression::static_type();
}

/// GTK's fundamental types, by name (the ones [`register_fundamentals`]
/// registers).
const FUNDAMENTALS: [&str; 3] = ["GdkEvent", "GskRenderNode", "GtkExpression"];

thread_local! {
    /// Whether GTK's fundamental types were all registered when the workers
    /// started (the test channel's `local`: `fundamentalsFirst`), `None`
    /// before they start.
    static FUNDAMENTALS_FIRST: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Starts startup's workers (steps 3 and 4: GStreamer's init on
/// `qrec-gst`), noting whether GTK's fundamental types were registered
/// before them (WP10's code audit: no test held `run` to registering them
/// first).
fn start_workers(startup: &Startup) {
    let registered = FUNDAMENTALS
        .iter()
        .all(|name| glib::Type::from_name(*name).is_some());
    FUNDAMENTALS_FIRST.with(|f| f.set(Some(registered)));
    startup.start_workers();
}

/// Whether GTK's fundamental types were registered before the workers
/// started (`None`: they haven't).
pub fn fundamentals_first() -> Option<bool> {
    FUNDAMENTALS_FIRST.with(Cell::get)
}

/// `qrec_gui::run`.
pub fn run(args: GuiArgs) -> Result<u8, CoreError> {
    log_panics();
    // The program's name is the app id, before GTK and the workers read
    // it: GTK's X11 backend makes both parts of the window's WM_CLASS
    // GLib's prgname, which was the binary's name ("qrec", "qrec"): now
    // it is the desktop entry's `StartupWMClass`, so docks and taskbars
    // find the entry and its icon (GUI-08). `run_with_args(&[])` gives
    // GApplication no argv[0] to take one from. Wayland matches the window
    // by its app_id, the application's id, which this doesn't change.
    glib::set_prgname(Some(APP_ID));
    let interrupts = signals::install(&HOST_SIGNALS).unwrap_or_else(|e| {
        tracing::warn!("signals won't be handled gracefully: {e}");
        Interrupts::new()
    });
    // Step 1, before GTK.
    let prepared = startup::prepare(&args, interrupts);
    tracing::info!(
        "the window for {} ({:?})",
        prepared.dir.display(),
        prepared.folder
    );
    // The queue's drain runs on this thread's main loop; items wait until
    // the window is up (`activate` kicks the first drain).
    let queue = Queue::new(|| {
        guard::idle(glib::Priority::DEFAULT, "the queue's drain", || {
            drain();
            glib::ControlFlow::Break
        });
    });
    QUEUE.with(|q| *q.borrow_mut() = Some(queue.clone()));
    let startup = Startup::new(prepared, queue);
    let wanted = startup.prepared().folder.is_ok();
    // Before any worker: step 3's `gst::init()` registers GStreamer's
    // fundamental types on its thread while GTK registers its own here.
    register_fundamentals();
    // Where the window opens (Linux: Wayland or X11, nothing else; other
    // systems have GTK's own backend). No display: nothing is touched.
    let display = display_choice();
    let target = if cfg!(target_os = "linux") {
        display.log();
        match display.target {
            Some(target) => Some(target),
            None => return Err(CoreError::unavailable(NO_DISPLAY)),
        }
    } else {
        None
    };
    // Steps 3 and 4 now, while GTK starts: the lock and the socket come
    // first. Only on Wayland, whose socket answered: the display is there
    // (should GTK still fail on it, the lock was taken all the same).
    let early = wanted && target == Some(GdkTarget::Wayland);
    if early {
        start_workers(&startup);
    }
    // Exactly that backend: a Wayland session never opens the window on
    // its X server, an X11 one never tries Wayland.
    if let Some(target) = target {
        gtk::gdk::set_allowed_backends(target.backend());
    }
    if let Err(e) = gtk::init() {
        return Err(CoreError::unavailable(open_failed(
            target,
            &display.env,
            &e.to_string(),
        )));
    }
    if let Some(opened) = gtk::gdk::Display::default() {
        tracing::info!(
            "the window's display: {} {}",
            opened.type_().name(),
            opened.name()
        );
    }
    if let Err(e) = adw::init() {
        return Err(CoreError::unavailable(format!("libadwaita: {e}")));
    }
    if wanted && !early {
        start_workers(&startup);
    }
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let startup = RefCell::new(Some(startup));
    app.connect_startup(guarded!("startup", move |_| on_startup()));
    app.connect_activate(guarded!(
        "activate",
        after_panic = activate_failed(app),
        move |app| {
            let startup = startup.borrow_mut().take();
            if let Some(startup) = startup {
                activate(app, startup);
            }
        }
    ));
    app.run_with_args::<&str>(&[]);
    APP.with(|a| a.borrow_mut().take());
    QUEUE.with(|q| q.borrow_mut().take());
    Ok(EXIT_CODE.with(Cell::get))
}

/// GTK `startup`: GTK's settings. Font rendering is GTK's manual mode
/// (`theme::fonts::configure_rendering`); test mode turns animations (but
/// with its `animations` flag) and the cursor blink off (G§8.4). The fonts,
/// the theme providers and the icon theme come with the window
/// (`activate`).
fn on_startup() {
    let Some(settings) = gtk::Settings::default() else {
        return;
    };
    crate::theme::fonts::configure_rendering(&settings);
    let mode = testctl::mode();
    if mode.on {
        if !mode.animations {
            settings.set_gtk_enable_animations(false);
        }
        settings.set_gtk_cursor_blink(false);
    }
}

/// `activate` panicked (caught, G§3.12): nothing would ever show, and a
/// host started at step 4 holds the project behind a window nobody sees.
/// Quit with 1: through the close (the host closes first) when the app is
/// up, else at once (`run_with_args` returns; a window that never showed
/// isn't destroyed, see [`App::quit`]).
fn activate_failed(app: &adw::Application) {
    tracing::error!("the window couldn't be built; quitting with 1");
    match App::current() {
        Some(the_app) => the_app.close_host_then_quit(1),
        None => {
            EXIT_CODE.with(|c| c.set(1));
            app.quit();
        }
    }
}

/// GTK `activate` (G§3.3 step 2).
fn activate(app: &adw::Application, startup: Startup) {
    let queue = startup.queue().clone();
    let prepared = startup.prepared();
    let name = prepared.name.clone();
    let placeholder = placeholder_state(prepared);
    let local = LocalUi {
        keymap_problems: prepared.keymap_problems.clone(),
        startup: match &prepared.folder {
            Ok(_) => StartupPhase::Opening { slow: false },
            Err(page) => StartupPhase::Blocked(page.clone()),
        },
        ..LocalUi::default()
    };
    let store = Store::new(
        queue.clone(),
        prepared.dir.clone(),
        Arc::new(placeholder),
        prepared.keymap.clone(),
        local,
    );
    let media = MediaHub::new(queue.clone(), store.clone());
    let actions = ActionRouter::new();
    let keys = KeyRouter::new(store.clone(), actions.clone(), media.clone());
    let ui = &prepared.config.ui;
    let theme = Theme::new(
        prepared.config.theme,
        ui.scale,
        MotionPolicy::from_settings(ui.reduced_motion),
    );
    register_resources();
    // The fonts first (before any widget the window shows), then the
    // theme's providers and the icons (G§3.3 step 2, G§3.14).
    if let Some(dir) = crate::theme::fonts::font_dir() {
        for warning in crate::theme::fonts::register(&dir) {
            tracing::warn!("{warning}");
        }
    }
    let resolved = crate::theme::fonts::resolve();
    if crate::theme::fonts::all_resolved(&resolved) {
        tracing::info!("fonts: the three bundled families resolve");
    } else {
        tracing::warn!("fonts: not every bundled family resolves: {resolved:?}");
    }
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_resource_path(ICONS);
        // The window's icon (X11's `_NET_WM_ICON`, a Wayland compositor's
        // xdg-toplevel-icon) is the app icon, whose hicolor sizes are in
        // the GResource too (build.rs): it resolves when Qrec isn't
        // installed (GUI-08).
        gtk::Window::set_default_icon_name(APP_ID);
        theme.install(&display);
        tracing::info!(
            "theme {} at scale {} before the first frame",
            crate::theme::css_name(theme.id()),
            theme.scale()
        );
    }
    let window = MainWindow::new(app, &name);
    // `.reduced-motion` follows the motion policy, also when it changes
    // live (WP10's switch, G§3.14).
    let motion_target = window.window.downgrade();
    theme
        .follow_motion(move |motion| {
            if let Some(window) = motion_target.upgrade() {
                if motion.reduced {
                    window.add_css_class("reduced-motion");
                } else {
                    window.remove_css_class("reduced-motion");
                }
            }
        })
        .detach();
    let ctx = Ctx {
        store: store.clone(),
        keys: keys.clone(),
        actions: actions.clone(),
        media,
        theme,
        window: window.layers.clone(),
    };
    let blocked = prepared.folder.is_err();
    // `config.toml` and `keys.toml` from now on (G§3.14, WP10).
    let follower = crate::follow::Follower::start(
        &ctx,
        &queue,
        prepared.dirs.as_ref(),
        prepared.config_file.as_ref(),
        prepared.config.ui.reduced_motion,
    );
    let the_app = Rc::new(App {
        app: app.clone(),
        ctx: ctx.clone(),
        window,
        startup,
        follower,
        closing: Cell::new(Closing::No),
        exit_after_host: Cell::new(0),
        hold: RefCell::new(None),
        quitting: Cell::new(false),
        channel: RefCell::new(None),
    });
    APP.with(|a| *a.borrow_mut() = Some(Rc::clone(&the_app)));
    // A caught panic (G§3.12) is told once as an error toast.
    let notices = store.downgrade();
    guard::set_reporter(move |what, message| {
        if let Some(store) = notices.upgrade() {
            store.notify(
                Notice::new(ToastLevel::Error, "Something went wrong in the window").with_body(
                    format!(
                        "{what}: {message}. It's in the log; if the window misbehaves, close it \
                         and start Qrec again."
                    ),
                ),
            );
        }
    });

    if testctl::mode().panic_activate {
        panic!("a test panic in activate (QREC_GUI_TEST=panic-activate)");
    }
    the_app.window.populate(&ctx, &name);
    keys.attach(&the_app.window.window);
    the_app.register_actions();
    let weak = Rc::downgrade(&the_app);
    the_app.window.window.connect_close_request(guarded!(
        "close request",
        after_panic = glib::Propagation::Stop,
        move |_| {
            if let Some(app) = weak.upgrade() {
                app.request_close();
            }
            glib::Propagation::Stop
        }
    ));
    // The keymap's problems, one warning each (shown once the window maps).
    let problems = the_app.ctx.store.local().keymap_problems.clone();
    for problem in problems {
        store
            .notify(Notice::new(ToastLevel::Warning, "keys.toml has a problem").with_body(problem));
    }
    *the_app.channel.borrow_mut() = testctl::listen(queue.clone());
    let weak = Rc::downgrade(&the_app);
    store
        .subscribe(move |change| {
            if let Some(app) = weak.upgrade() {
                app.after_snapshot(&change.next);
            }
        })
        .detach();
    the_app.window.window.present();
    tracing::info!("the window is presented");
    // The GL renderer's programs, after the first frame and while the
    // project opens, not on a take's first frames (G§8.6).
    crate::gpu_warmup::start(the_app.window.window.upcast_ref());
    if testctl::mode().on {
        log_regions_when_mapped(&the_app);
    }
    the_app.startup.watch_signals(&the_app);
    // What the workers reported while the window was being built.
    queue.kick();
    // The main-loop probe (`QREC_GUI_PROBE=1`, G§8.6), now the window is
    // built and presented: from its first frame to the quit.
    crate::probe::start_if_wanted();
    if blocked {
        tracing::info!(
            "startup stops at a page: {:?}",
            the_app.ctx.store.local().startup
        );
        return;
    }
    // Warm the source cache before the Session needs it (H-5).
    let sources = the_app.startup.sources().clone();
    the_app.ctx.media.run_io(move || {
        let found = sources.refresh();
        queue.push(Inbound::Sources((*found).clone()));
    });
    the_app.startup.begin(&the_app);
}

/// The icon theme's resource path: the custom symbolic icons
/// (`scalable/actions`) and the app icon's hicolor sizes (`<size>/apps`,
/// from `data/icons/hicolor`: build.rs).
pub const ICONS: &str = "/com/rsensato/qrec/icons";

/// Registers the compiled `resources/` and app icon (build.rs).
fn register_resources() {
    let bytes =
        glib::Bytes::from_static(include_bytes!(concat!(env!("OUT_DIR"), "/qrec.gresource")));
    match gio::Resource::from_data(&bytes) {
        Ok(resource) => gio::resources_register(&resource),
        Err(e) => tracing::error!("the window's resources are broken: {e}"),
    }
}

/// The state shown before the Session opens: an empty project named after
/// the folder, with the user's defaults.
fn placeholder_state(prepared: &startup::Prepared) -> State {
    let project = Project::new(
        prepared.name.clone(),
        Utc::now(),
        &prepared.config.project_defaults(),
    );
    State::new(project, History::new(), UiState::default(), HostKind::Gui)
}

/// The queue's drain, on the main thread. Before the window is up the
/// items wait (`activate` kicks a drain once it is).
fn drain() {
    let Some(app) = APP.with(|a| a.borrow().clone()) else {
        QUEUE.with(|q| {
            if let Some(queue) = &*q.borrow() {
                queue.clear_pending();
            }
        });
        return;
    };
    // One item at a time: a panic in one (caught, G§3.12) leaves the others
    // to be handled.
    for item in app.ctx.store.queue().take_all() {
        guard::run("an inbound item", || app.handle(item));
    }
}

impl App {
    /// The running app (main thread).
    pub fn current() -> Option<Rc<App>> {
        APP.with(|a| a.borrow().clone())
    }

    fn handle(self: &Rc<Self>, item: Inbound) {
        let store = &self.ctx.store;
        match item {
            Inbound::Snapshot(rev, state) => store.on_snapshot(rev, state),
            Inbound::Accepted(ticket) => store.on_progress(ticket, None),
            Inbound::Progress(ticket, progress) => store.on_progress(ticket, Some(&progress)),
            Inbound::Reply {
                ticket,
                result,
                late,
            } => store.on_reply(ticket, result, late),
            Inbound::Toast { toast, recovery } => store.on_toast(toast, recovery),
            Inbound::Host(event) => self.startup.on_host(self, event),
            Inbound::Startup(event) => self.startup.on_event(self, event),
            Inbound::Player(event) => {
                // The hub logs it and notes Stopped (R4); the sync acts on it.
                self.ctx.media.on_player(&event);
                crate::media::program::on_player_event(&event);
            }
            Inbound::Preview(data) => self.ctx.media.on_preview(&data),
            Inbound::Sources(inventory) => {
                let inventory = Arc::new(inventory);
                store.update_local(move |l| l.sources = Some(inventory));
            }
            Inbound::Io(done) => self.ctx.media.on_io(done),
            Inbound::Settings(event) => self.follower.on_event(event),
            Inbound::Test(request) => testctl::handle(self, request),
        }
    }

    /// The GUI actions the app owns: quit, the blocking pages' buttons and
    /// the D-12 modal's. (`palette.open` and `keys.help` are the palette's
    /// and the cheat sheet's, WP9.)
    fn register_actions(self: &Rc<Self>) {
        let actions = &self.ctx.actions;
        let weak = Rc::downgrade(self);
        actions.register(
            "app.quit",
            Rc::new(move || {
                if let Some(app) = weak.upgrade() {
                    app.request_close();
                }
            }),
        );
        for action in [
            qrec_core::view::blocking::BlockingAction::CheckAgain,
            qrec_core::view::blocking::BlockingAction::StopAndOpen,
            qrec_core::view::blocking::BlockingAction::Wait,
            qrec_core::view::blocking::BlockingAction::Quit,
        ] {
            let weak = Rc::downgrade(self);
            actions.register(
                blocking::action_id(action),
                Rc::new(move || {
                    if let Some(app) = weak.upgrade() {
                        app.startup.on_page_action(&app, action);
                    }
                }),
            );
        }
        let weak = Rc::downgrade(self);
        actions.register(
            "app.cancel-export-and-quit",
            Rc::new(move || {
                if let Some(app) = weak.upgrade() {
                    app.ctx.store.dispatch(Command::ExportCancel { job: None });
                    app.finish_close();
                }
            }),
        );
    }

    /// The window's scale factor (the preview's size cap, step 5).
    pub fn scale_factor(&self) -> i32 {
        self.window.window.scale_factor()
    }

    /// The Session opened (step 8 is the media hub's).
    pub fn on_opened(self: &Rc<Self>) {
        tracing::info!("the project is open");
        if testctl::mode().on {
            log_regions_when_mapped(self);
        }
    }

    /// After every snapshot: the close sequence waits for Idle.
    fn after_snapshot(&self, state: &State) {
        if self.closing.get() == Closing::WaitingForIdle && state.runtime.record.is_idle() {
            self.finish_close();
        }
    }

    /// Window close or `app.quit` (D-12).
    pub fn request_close(&self) {
        let local_startup = self.ctx.store.local().startup.clone();
        match local_startup {
            StartupPhase::Blocked(_) => return self.quit_from_page(),
            StartupPhase::Closing => {
                tracing::debug!("already closing");
                return;
            }
            StartupPhase::Opening { .. } => return self.finish_close(),
            StartupPhase::Open => {}
        }
        let state = self.ctx.store.state();
        match &state.runtime.record {
            RecordPhase::Countdown { .. } => {
                self.ctx.store.dispatch(Command::RecordCancel);
                self.wait_for_idle();
            }
            RecordPhase::Starting { .. } | RecordPhase::Recording { .. } => {
                self.ctx.store.dispatch(Command::RecordStop);
                self.wait_for_idle();
            }
            RecordPhase::Finalizing { .. } => self.wait_for_idle(),
            RecordPhase::Idle => match export_percent(&state) {
                Some(percent) => self.ctx.store.update_local(move |l| {
                    if !l
                        .overlays
                        .iter()
                        .any(|o| matches!(o, Overlay::QuitWhileExporting { .. }))
                    {
                        l.overlays.push(Overlay::QuitWhileExporting { percent });
                    }
                }),
                None => self.finish_close(),
            },
        }
    }

    fn wait_for_idle(&self) {
        tracing::info!("closing once the take is finalized");
        self.closing.set(Closing::WaitingForIdle);
        self.ctx
            .store
            .update_local(|l| l.startup = StartupPhase::Closing);
    }

    /// The host closes; the app holds until it stopped (R4).
    fn finish_close(&self) {
        self.close_host_then_quit(0);
    }

    /// Closes the host (it stops once quiet, within 60 s) and quits with
    /// `code` once it stopped; at once without a host. The media pipelines
    /// start going to NULL meanwhile, on their own threads (R4).
    fn close_host_then_quit(&self, code: u8) {
        if self.closing.get() == Closing::WaitingForHost {
            return;
        }
        if !self.startup.host_alive() {
            return self.quit(code);
        }
        tracing::info!("closing: the host stops once quiet");
        self.exit_after_host.set(code);
        self.closing.set(Closing::WaitingForHost);
        if !matches!(self.ctx.store.local().startup, StartupPhase::Blocked(_)) {
            self.ctx
                .store
                .update_local(|l| l.startup = StartupPhase::Closing);
        }
        *self.hold.borrow_mut() = Some(self.app.hold());
        // What the regions debounced goes ahead of the close (G§3.4.1),
        // the playing program's playhead too: before the player stops.
        self.ctx.store.before_close();
        self.ctx.media.shutdown();
        self.startup.close_host();
        self.window.window.set_visible(false);
    }

    /// The host stopped (`HostEvent::Stopped`).
    pub fn on_host_stopped(&self, end: HostEnd) {
        tracing::info!("the host stopped: {end:?}");
        if end.signalled() {
            return self.quit(130);
        }
        match self.closing.get() {
            Closing::WaitingForHost | Closing::WaitingForIdle => {
                let code =
                    exit_after_close(&self.ctx.store.local().startup, self.exit_after_host.get());
                self.quit(code);
            }
            Closing::No => {
                if matches!(self.ctx.store.local().startup, StartupPhase::Blocked(_)) {
                    // The page stays until the person quits.
                    return;
                }
                tracing::warn!("the host stopped on its own ({end:?})");
                self.quit(1);
            }
        }
    }

    /// Quit from a blocking page: its exit code (G§3.2).
    pub fn quit_from_page(&self) {
        let code = match &self.ctx.store.local().startup {
            StartupPhase::Blocked(kind) => kind.exit_code(),
            _ => 0,
        };
        self.startup.choose(startup::Choice::Quit);
        // A host still serving (a page shown after it started) lets go of
        // the project first.
        self.close_host_then_quit(code);
    }

    /// Stops the media pipelines and quits the main loop with `code` (the
    /// first quit wins) once the player and the meter monitors reached NULL
    /// (R4: the main loop runs meanwhile, for `gtk4paintablesink`'s calls
    /// into it), at most [`MEDIA_STOP_WAIT`] later.
    pub fn quit(&self, code: u8) {
        if self.quitting.replace(true) {
            return;
        }
        EXIT_CODE.with(|c| c.set(code));
        tracing::info!("quitting with {code}");
        crate::probe::finish();
        self.ctx.media.shutdown();
        // The window goes now; the application lives on until NULL. One
        // that never showed (a panic in `activate`) stays: GTK 4.22
        // crashes taking an unrealized window out of its Wayland session
        // (`gdk_wayland_toplevel_remove_from_session`), and quitting ends it.
        let hold = self.app.hold();
        self.hold.borrow_mut().take();
        if self.window.window.is_realized() {
            self.window.window.destroy();
        }
        let (media, app, asked) = (self.ctx.media.clone(), self.app.clone(), Instant::now());
        let done = move || {
            let waited = asked.elapsed();
            if media.stopped() {
                tracing::info!(
                    "the player and the meters reached NULL ({} ms after the quit)",
                    waited.as_millis()
                );
            } else {
                tracing::warn!(
                    "the player or the meters didn't reach NULL within {} s; quitting anyway",
                    MEDIA_STOP_WAIT.as_secs()
                );
            }
            app.quit();
        };
        if self.ctx.media.stopped() {
            drop(hold);
            return done();
        }
        let media = self.ctx.media.clone();
        let mut hold = Some(hold);
        let mut done = Some(done);
        guard::timeout_local(
            Duration::from_millis(50),
            "quit: the media's NULL",
            move || {
                if !media.stopped() && asked.elapsed() < MEDIA_STOP_WAIT {
                    return glib::ControlFlow::Continue;
                }
                hold.take();
                if let Some(done) = done.take() {
                    done();
                }
                glib::ControlFlow::Break
            },
        );
    }
}

/// The exit code once the host stopped during a close: the blocking page's
/// when one is up (a session that crashed meanwhile blocks with
/// `HostFailed`, 1; GStreamer stuck or a missing dependency, 5), else the
/// one the close asked for (G§3.2).
fn exit_after_close(startup: &StartupPhase, asked: u8) -> u8 {
    match startup {
        StartupPhase::Blocked(kind) => kind.exit_code(),
        _ => asked,
    }
}

/// Panics reach the GUI log too (with where they happened), not only
/// stderr; the default hook still prints them. GLib callbacks catch theirs
/// (`guard`), so the window carries on.
fn log_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            let message = guard::message(info.payload());
            let place = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_default();
            tracing::error!(
                "panic on thread {} at {place}: {message}",
                thread.name().unwrap_or("?")
            );
            default(info);
        }));
    });
}

/// The backend GDK opens the window with (Linux).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GdkTarget {
    Wayland,
    X11,
}

impl GdkTarget {
    /// Wayland first, as in GDK's own order.
    const ALL: [Self; 2] = [Self::Wayland, Self::X11];

    /// GDK's name for it (`GDK_BACKEND`, `gdk::set_allowed_backends`).
    pub const fn backend(self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
            Self::X11 => "x11",
        }
    }
}

/// `run`'s error without a display (exit 1).
const NO_DISPLAY: &str =
    "can't open the window: no Wayland (WAYLAND_DISPLAY) or X11 (DISPLAY) display";

/// What says where the displays are: the process's variables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisplayEnv {
    pub gdk_backend: Option<OsString>,
    pub wayland_display: Option<OsString>,
    pub wayland_socket: Option<OsString>,
    pub runtime_dir: Option<PathBuf>,
    pub display: Option<OsString>,
}

impl DisplayEnv {
    pub fn from_process() -> Self {
        Self {
            gdk_backend: std::env::var_os("GDK_BACKEND"),
            wayland_display: std::env::var_os("WAYLAND_DISPLAY"),
            wayland_socket: std::env::var_os("WAYLAND_SOCKET"),
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(Into::into),
            display: std::env::var_os("DISPLAY"),
        }
    }

    /// The Wayland display's name: `WAYLAND_DISPLAY`, else libwayland's
    /// `wayland-0`.
    fn wayland_name(&self) -> &OsStr {
        self.wayland_display
            .as_deref()
            .unwrap_or(OsStr::new("wayland-0"))
    }

    /// Whether a Wayland display is reachable where libwayland looks for
    /// it: a passed `WAYLAND_SOCKET`, or something listens on the display's
    /// socket (an absolute path, else in `XDG_RUNTIME_DIR`). Only
    /// connecting tells: the socket file a compositor that died left
    /// behind refuses (`ECONNREFUSED`), and GTK would fail on it after the
    /// lock was taken. The connection closes at once, before a byte of the
    /// protocol, and never blocks (a full accept backlog still means a
    /// listener).
    pub fn wayland_reachable(&self) -> bool {
        if self.wayland_socket.is_some() {
            return true;
        }
        let name = Path::new(self.wayland_name());
        let path = if name.is_absolute() {
            name.to_path_buf()
        } else {
            match &self.runtime_dir {
                Some(dir) => dir.join(name),
                None => return false,
            }
        };
        match qrec_shell::ipc::peer::connect_nonblocking(&path) {
            Ok(_connected) => true,
            Err(e) => e.kind() == std::io::ErrorKind::WouldBlock,
        }
    }

    /// Whether `DISPLAY` names an X11 display. Only connecting tells
    /// whether a server answers there (its socket may be an abstract one).
    pub fn x11_named(&self) -> bool {
        self.display.as_ref().is_some_and(|d| !d.is_empty())
    }

    /// Where `target`'s display is, for the log and the errors.
    fn describe(&self, target: GdkTarget) -> String {
        match target {
            GdkTarget::Wayland if self.wayland_socket.is_some() => {
                "the Wayland socket passed in WAYLAND_SOCKET".to_owned()
            }
            GdkTarget::Wayland => format!(
                "the Wayland display {:?} (WAYLAND_DISPLAY)",
                self.wayland_name().to_string_lossy()
            ),
            GdkTarget::X11 => format!(
                "the X11 display {:?} (DISPLAY)",
                self.display
                    .as_deref()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
        }
    }
}

/// The backend the window opens with: the first one `GDK_BACKEND` names
/// (GDK's comma-separated list; `*` is any) whose display is there, else
/// Wayland when something listens on its socket
/// ([`DisplayEnv::wayland_reachable`]), else X11 when `DISPLAY` names a
/// display; `None` without either. So `GDK_BACKEND=x11` in a Wayland
/// session with XWayland opens the window on X11, and a
/// `GDK_BACKEND=wayland` exported on Xorg, or a dead compositor's socket
/// left in `XDG_RUNTIME_DIR`, doesn't keep it from opening on X11.
pub fn display_target(env: &DisplayEnv) -> Option<GdkTarget> {
    // One connection at most, however often `pick_target` asks.
    let wayland = std::cell::OnceCell::new();
    pick_target(env.gdk_backend.as_deref(), |target| match target {
        GdkTarget::Wayland => *wayland.get_or_init(|| env.wayland_reachable()),
        GdkTarget::X11 => env.x11_named(),
    })
}

/// [`display_target`], given whether each backend's display is there.
fn pick_target(
    gdk_backend: Option<&OsStr>,
    there: impl Fn(GdkTarget) -> bool,
) -> Option<GdkTarget> {
    let first_there = || GdkTarget::ALL.into_iter().find(|&target| there(target));
    for name in backend_names(gdk_backend) {
        if name == b"*" {
            return first_there();
        }
        let named = GdkTarget::ALL
            .into_iter()
            .find(|&target| target.backend().as_bytes() == name && there(target));
        if named.is_some() {
            return named;
        }
    }
    first_there()
}

/// `GDK_BACKEND`'s names as GDK splits them: at commas, untrimmed.
fn backend_names(gdk_backend: Option<&OsStr>) -> impl Iterator<Item = &[u8]> {
    gdk_backend
        .into_iter()
        .flat_map(|list| list.as_encoded_bytes().split(|&b| b == b','))
}

/// Whether GDK, allowed only `target`, still opens it with this
/// `GDK_BACKEND`: GDK tries only what the variable names, skipping what
/// isn't allowed, so it does when the list names `target` or has `*`.
fn gdk_backend_opens(gdk_backend: &OsStr, target: GdkTarget) -> bool {
    backend_names(Some(gdk_backend)).any(|name| name == b"*" || name == target.backend().as_bytes())
}

/// The window's display, as [`prepare_display`] found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayChoice {
    /// The variables as the process got them.
    pub env: DisplayEnv,
    /// [`display_target`] of them.
    pub target: Option<GdkTarget>,
    /// `GDK_BACKEND` names no backend whose display is here, so the window
    /// opens on `target` without it: `Ok` once it's out of the process's
    /// environment (GDK reads it, and would only try what it names), `Err`
    /// why it couldn't go.
    pub ignored_backend: Option<Result<(), String>>,
}

impl DisplayChoice {
    /// The choice for `env`, before anything leaves the environment.
    fn of(env: DisplayEnv) -> Self {
        let target = display_target(&env);
        let ignored = match (&env.gdk_backend, target) {
            (Some(var), Some(target)) => !gdk_backend_opens(var, target),
            _ => false,
        };
        Self {
            env,
            target,
            ignored_backend: ignored.then(|| Err("prepare_display didn't run".to_owned())),
        }
    }

    /// Says in the log where the window opens.
    fn log(&self) {
        let Some(target) = self.target else {
            return;
        };
        let place = self.env.describe(target);
        let asked = self.env.gdk_backend.as_deref().map(OsStr::to_string_lossy);
        match (&self.ignored_backend, asked) {
            (Some(Ok(())), Some(asked)) => tracing::info!(
                "GDK_BACKEND={asked:?} names no backend whose display is here: the window \
                 opens on {place}"
            ),
            (Some(Err(why)), Some(asked)) => tracing::warn!(
                "GDK_BACKEND={asked:?} names no backend whose display is here but stays set \
                 ({why}): GTK won't open the window on {place}"
            ),
            (_, Some(asked)) => {
                tracing::info!("the window opens on {place}, as GDK_BACKEND={asked:?} says");
            }
            (_, None) => tracing::info!("the window opens on {place}"),
        }
    }
}

/// [`prepare_display`]'s choice.
static PREPARED: OnceLock<DisplayChoice> = OnceLock::new();

/// Works out the window's display ([`display_target`]) and, when an
/// inherited `GDK_BACKEND` names no backend whose display is here, takes
/// it out of the environment: GDK reads it as GTK starts and would only
/// try what it names. Changing the environment is only sound while the
/// process has a single thread, so `main` calls this while it still has
/// one, right after it set the GPU variables (A§8.10); once another thread
/// runs it leaves the variable and `run` says so in the log. Linux only;
/// once per process.
pub fn prepare_display() {
    if cfg!(target_os = "linux") {
        PREPARED.get_or_init(|| {
            let mut choice = DisplayChoice::of(DisplayEnv::from_process());
            if choice.ignored_backend.is_some() {
                choice.ignored_backend = Some(remove_gdk_backend());
            }
            choice
        });
    }
}

/// [`prepare_display`]'s choice, else (it didn't run) the process's now.
fn display_choice() -> DisplayChoice {
    PREPARED
        .get()
        .cloned()
        .unwrap_or_else(|| DisplayChoice::of(DisplayEnv::from_process()))
}

/// Removes `GDK_BACKEND` from the environment while the process has one
/// thread (`/proc/self/task`).
fn remove_gdk_backend() -> Result<(), String> {
    match std::fs::read_dir("/proc/self/task").map(Iterator::count) {
        Ok(1) => {
            // SAFETY: the process has exactly one thread (just counted), so
            // no other thread reads or writes the environment meanwhile.
            unsafe { std::env::remove_var("GDK_BACKEND") };
            Ok(())
        }
        Ok(threads) => Err(format!("the process already runs {threads} threads")),
        Err(e) => Err(format!("can't count the process's threads: {e}")),
    }
}

/// `run`'s error when GTK can't connect to `target`'s display: where it
/// tried, and the other display.
fn open_failed(target: Option<GdkTarget>, env: &DisplayEnv, error: &str) -> String {
    let Some(target) = target else {
        return format!("can't open the window: {error}");
    };
    let other = match target {
        GdkTarget::Wayland if env.x11_named() => format!(
            "GDK_BACKEND=x11 would open it on {}",
            env.describe(GdkTarget::X11)
        ),
        GdkTarget::Wayland => "there's no X11 display (DISPLAY) either".to_owned(),
        GdkTarget::X11 if env.wayland_reachable() => format!(
            "GDK_BACKEND chose it over {}",
            env.describe(GdkTarget::Wayland)
        ),
        GdkTarget::X11 => "there's no Wayland display (WAYLAND_DISPLAY) either".to_owned(),
    };
    format!(
        "can't open the window on {}: {error}; {other}",
        env.describe(target)
    )
}

/// The running export's percent, if one runs.
fn export_percent(state: &State) -> Option<u8> {
    let job = state
        .runtime
        .jobs
        .values()
        .find(|job| matches!(job.kind, JobKind::Export { .. }))?;
    Some(export_readout(job, Utc::now()).map_or(0, |r| r.percent))
}

/// Test mode: the regions' bounds in the log once the window is drawn
/// (until the control channel's `regions` op).
fn log_regions_when_mapped(app: &Rc<App>) {
    let weak = Rc::downgrade(app);
    guard::timeout_local_once(
        std::time::Duration::from_millis(1500),
        "the regions' log",
        move || {
            if let Some(app) = weak.upgrade() {
                for (name, [x, y, w, h]) in app.window.regions() {
                    tracing::info!("region {name}: ({x}, {y}, {w}, {h})");
                }
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use qrec_core::CoreError;
    use qrec_core::view::blocking::BlockKind;

    use super::*;

    /// A session that crashes during the D-12 close (its `HostFailed` page
    /// up when the host stops) exits 1, like the crash page's Quit; a page
    /// the close came from keeps its code; a plain close exits with the
    /// close's code. (The close's 0 was used whatever ended it.)
    #[test]
    fn a_crash_during_the_close_exits_with_its_page() {
        let crashed = StartupPhase::Blocked(BlockKind::HostFailed {
            error: CoreError::internal("the Qrec session crashed: boom"),
        });
        assert_eq!(exit_after_close(&crashed, 0), 1);
        assert_eq!(
            exit_after_close(&StartupPhase::Blocked(BlockKind::GstStuck), 5),
            5
        );
        assert_eq!(exit_after_close(&StartupPhase::Closing, 0), 0);
        assert_eq!(exit_after_close(&StartupPhase::Open, 1), 1);
    }

    /// A runtime dir with the Wayland socket `wayland-7` (and libwayland's
    /// default `wayland-0` when `default` says so), `wayland-stale`, the
    /// socket file a compositor that died leaves (nobody listens), and a
    /// plain file `wayland-file`; the listeners keep their sockets.
    fn runtime_dir(default: bool) -> (tempfile::TempDir, Vec<std::os::unix::net::UnixListener>) {
        let tmp = tempfile::tempdir().unwrap();
        let bind =
            |name: &str| std::os::unix::net::UnixListener::bind(tmp.path().join(name)).unwrap();
        // Dropping a listener leaves its file.
        drop(bind("wayland-stale"));
        let mut listeners = vec![bind("wayland-7")];
        if default {
            listeners.push(bind("wayland-0"));
        }
        std::fs::write(tmp.path().join("wayland-file"), "not a socket").unwrap();
        (tmp, listeners)
    }

    /// Where the window opens: Wayland when something listens on its
    /// socket, else X11 when `DISPLAY` names a display, else nowhere (the
    /// folder is never touched then). A `GDK_BACKEND` counts only when its
    /// backend's display is there; one GDK couldn't open the chosen
    /// backend with is ignored (taken out of the environment).
    /// `GDK_BACKEND` alone no longer counts as a display (GUI-19:
    /// `GDK_BACKEND=x11` without `DISPLAY` took the lock before a doomed
    /// `gtk::init`), nor does a dead compositor's socket file.
    #[test]
    fn the_window_opens_where_a_display_is() {
        use GdkTarget::{Wayland, X11};
        let (tmp, _listeners) = runtime_dir(false);
        let (with_default, _default_listeners) = runtime_dir(true);
        let absolute = tmp.path().join("wayland-7");
        let absolute = absolute.to_str().unwrap();
        let env = |gdk: Option<&str>, wayland: Option<&str>, display: Option<&str>| DisplayEnv {
            gdk_backend: gdk.map(Into::into),
            wayland_display: wayland.map(Into::into),
            runtime_dir: Some(tmp.path().to_path_buf()),
            display: display.map(Into::into),
            ..DisplayEnv::default()
        };
        let w7 = Some("wayland-7");
        let x0 = Some(":0");
        let no_runtime_dir = DisplayEnv {
            runtime_dir: None,
            ..env(None, w7, None)
        };
        let passed = DisplayEnv {
            wayland_socket: Some("5".into()),
            ..DisplayEnv::default()
        };
        let default_socket = DisplayEnv {
            runtime_dir: Some(with_default.path().to_path_buf()),
            ..DisplayEnv::default()
        };
        let stale = Some("wayland-stale");
        // What, the env, where the window opens, GDK_BACKEND ignored.
        #[rustfmt::skip]
        let rows: Vec<(&str, DisplayEnv, Option<GdkTarget>, bool)> = vec![
            ("WAYLAND_DISPLAY's socket", env(None, w7, None), Some(Wayland), false),
            ("an absolute WAYLAND_DISPLAY", env(None, Some(absolute), None), Some(Wayland), false),
            ("no such socket", env(None, Some("wayland-1"), None), None, false),
            ("not a socket", env(None, Some("wayland-file"), None), None, false),
            ("a socket nobody listens on", env(None, stale, None), None, false),
            ("wayland-0 by default", default_socket, Some(Wayland), false),
            ("wayland-0 isn't there", env(None, None, None), None, false),
            ("no XDG_RUNTIME_DIR", no_runtime_dir, None, false),
            ("WAYLAND_SOCKET passed", passed, Some(Wayland), false),
            ("Xorg: DISPLAY", env(None, None, x0), Some(X11), false),
            ("Xorg, a stale WAYLAND_DISPLAY", env(None, Some("wayland-1"), x0), Some(X11), false),
            ("Xorg, a dead compositor's socket", env(None, stale, x0), Some(X11), false),
            ("wayland,x11, a dead socket", env(Some("wayland,x11"), stale, x0), Some(X11), false),
            ("an empty DISPLAY names none", env(None, None, Some("")), None, false),
            ("Wayland with XWayland", env(None, w7, x0), Some(Wayland), false),
            ("GDK_BACKEND=x11 with XWayland", env(Some("x11"), w7, x0), Some(X11), false),
            ("GDK_BACKEND=x11 without DISPLAY", env(Some("x11"), w7, None), Some(Wayland), true),
            ("GDK_BACKEND=wayland on Wayland", env(Some("wayland"), w7, x0), Some(Wayland), false),
            ("GDK_BACKEND=wayland on Xorg", env(Some("wayland"), None, x0), Some(X11), true),
            ("GDK_BACKEND=x11, no display", env(Some("x11"), None, None), None, false),
            ("GDK_BACKEND=wayland, no display", env(Some("wayland"), None, None), None, false),
            ("x11,wayland, no DISPLAY", env(Some("x11,wayland"), w7, None), Some(Wayland), false),
            ("x11,wayland with both", env(Some("x11,wayland"), w7, x0), Some(X11), false),
            ("* is any: Wayland first", env(Some("*"), w7, x0), Some(Wayland), false),
            ("x11,* without DISPLAY", env(Some("x11,*"), w7, None), Some(Wayland), false),
            ("a backend Qrec doesn't open", env(Some("broadway"), w7, x0), Some(Wayland), true),
            ("GDK doesn't trim names", env(Some("x11, wayland"), w7, None), Some(Wayland), true),
            ("an empty GDK_BACKEND", env(Some(""), None, x0), Some(X11), true),
        ];
        for (what, env, target, ignored) in rows {
            let choice = DisplayChoice::of(env);
            assert_eq!(choice.target, target, "{what}");
            assert_eq!(choice.ignored_backend.is_some(), ignored, "{what}");
        }
    }

    /// Whether GDK, allowed only `allowed`, opens it with this
    /// `GDK_BACKEND`, as `gdk_display_manager_open_display` goes: without
    /// one it tries what's allowed, else the names in order (`*` for any),
    /// skipping what isn't allowed.
    fn gdk_opens(gdk_backend: Option<&str>, allowed: &str) -> bool {
        gdk_backend.is_none_or(|list| {
            list.split(',')
                .filter(|name| !name.is_empty())
                .any(|name| name == "*" || name == allowed)
        })
    }

    proptest::proptest! {
        /// Whatever the variables say: a reachable Wayland socket (one
        /// something listens on) opens the window on Wayland, unless
        /// `GDK_BACKEND` names `x11` (before any `wayland` or `*`) and
        /// `DISPLAY` is set; without `WAYLAND_*` and `DISPLAY` it opens on
        /// `wayland-0` when that is reachable, else nowhere; it never opens
        /// on a backend whose display is missing, and GDK (told only that
        /// backend, and the `GDK_BACKEND` left in the environment) opens
        /// exactly it.
        #[test]
        fn a_reachable_wayland_socket_opens_on_wayland(
            names in proptest::collection::vec(
                proptest::sample::select(vec!["wayland", "x11", "*", "broadway", "", " x11"]),
                0..4,
            ),
            gdk_set in proptest::bool::ANY,
            wayland in proptest::option::of(proptest::sample::select(vec![
                "wayland-7", "absolute", "wayland-1", "wayland-stale", "wayland-file", "",
            ])),
            wayland_socket in proptest::bool::ANY,
            runtime in proptest::bool::ANY,
            default_there in proptest::bool::ANY,
            display in proptest::option::of(proptest::sample::select(vec![":0", ":99", ""])),
        ) {
            use GdkTarget::{Wayland, X11};
            let (tmp, _listeners) = runtime_dir(default_there);
            let absolute = tmp.path().join("wayland-7");
            let gdk = gdk_set.then(|| names.join(","));
            let env = DisplayEnv {
                gdk_backend: gdk.clone().map(Into::into),
                wayland_display: wayland.map(|name| match name {
                    "absolute" => absolute.clone().into_os_string(),
                    name => name.into(),
                }),
                wayland_socket: wayland_socket.then(|| "5".into()),
                runtime_dir: runtime.then(|| tmp.path().to_path_buf()),
                display: display.map(Into::into),
            };
            let default_reachable = runtime && default_there;
            let wayland_there = wayland_socket
                || wayland == Some("absolute")
                || (runtime && wayland == Some("wayland-7"))
                || (wayland.is_none() && default_reachable);
            let x11_there = display.is_some_and(|d| !d.is_empty());
            let choice = DisplayChoice::of(env);
            match choice.target {
                Some(Wayland) => proptest::prop_assert!(wayland_there),
                Some(X11) => proptest::prop_assert!(x11_there),
                None => proptest::prop_assert!(!wayland_there && !x11_there),
            }
            if wayland_there {
                let position = |wanted: &[&str]| names.iter().position(|n| wanted.contains(n));
                let before = |x: usize| position(&["wayland", "*"]).is_none_or(|w| x < w);
                let x11_first = gdk_set && position(&["x11"]).is_some_and(before);
                let expected = if x11_first && x11_there { X11 } else { Wayland };
                proptest::prop_assert_eq!(choice.target, Some(expected));
            }
            if !wayland_socket && wayland.is_none() && display.is_none() {
                proptest::prop_assert_eq!(choice.target, default_reachable.then_some(Wayland));
            }
            if let Some(target) = choice.target {
                // What GDK reads: the variable unless it was ignored (and
                // only one GDK couldn't open the target with is).
                let ignored = choice.ignored_backend.is_some();
                let left = if ignored { None } else { gdk.as_deref() };
                proptest::prop_assert!(gdk_opens(left, target.backend()));
                proptest::prop_assert_eq!(ignored, !gdk_opens(gdk.as_deref(), target.backend()));
            } else {
                proptest::prop_assert!(choice.ignored_backend.is_none());
            }
        }
    }

    /// GTK couldn't connect: the error says where it tried and what else
    /// there is, naming both variables when neither works.
    #[test]
    fn a_failed_open_says_where_and_what_else() {
        use GdkTarget::{Wayland, X11};
        let (tmp, _listeners) = runtime_dir(false);
        let env = |gdk: Option<&str>, wayland: Option<&str>, display: Option<&str>| DisplayEnv {
            gdk_backend: gdk.map(Into::into),
            wayland_display: wayland.map(Into::into),
            runtime_dir: Some(tmp.path().to_path_buf()),
            display: display.map(Into::into),
            ..DisplayEnv::default()
        };
        let failed = "Failed to initialize GTK";
        assert_eq!(
            open_failed(Some(X11), &env(None, None, Some(":4000")), failed),
            "can't open the window on the X11 display \":4000\" (DISPLAY): Failed to initialize \
             GTK; there's no Wayland display (WAYLAND_DISPLAY) either"
        );
        assert_eq!(
            open_failed(
                Some(X11),
                &env(Some("x11"), Some("wayland-7"), Some(":0")),
                failed
            ),
            "can't open the window on the X11 display \":0\" (DISPLAY): Failed to initialize GTK; \
             GDK_BACKEND chose it over the Wayland display \"wayland-7\" (WAYLAND_DISPLAY)"
        );
        assert_eq!(
            open_failed(Some(Wayland), &env(None, Some("wayland-7"), None), failed),
            "can't open the window on the Wayland display \"wayland-7\" (WAYLAND_DISPLAY): Failed \
             to initialize GTK; there's no X11 display (DISPLAY) either"
        );
        assert_eq!(
            open_failed(
                Some(Wayland),
                &env(None, Some("wayland-7"), Some(":0")),
                failed
            ),
            "can't open the window on the Wayland display \"wayland-7\" (WAYLAND_DISPLAY): Failed \
             to initialize GTK; GDK_BACKEND=x11 would open it on the X11 display \":0\" (DISPLAY)"
        );
        let passed = DisplayEnv {
            wayland_socket: Some("5".into()),
            ..DisplayEnv::default()
        };
        assert!(
            open_failed(Some(Wayland), &passed, failed).starts_with(
                "can't open the window on the Wayland socket passed in WAYLAND_SOCKET:"
            )
        );
        assert_eq!(
            open_failed(None, &DisplayEnv::default(), failed),
            "can't open the window: Failed to initialize GTK"
        );
        assert!(NO_DISPLAY.contains("WAYLAND_DISPLAY") && NO_DISPLAY.contains("(DISPLAY)"));
    }
}
