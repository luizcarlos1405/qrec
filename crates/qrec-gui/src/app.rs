//! The application (G§3.2, G§3.3, D-12): `qrec_gui::run`, the libadwaita
//! application, `activate`, the queue's drain, the close sequence and the
//! exit code.
//!
//! - `run`: the signal handlers, startup step 1, then steps 3 and 4 at once
//!   when a Wayland display is there (the lock and the socket within
//!   milliseconds of launch; their results wait in the queue), then
//!   `adw::Application` (`com.rsensato.qrec`, `NON_UNIQUE`) on this thread;
//!   returns the exit code the app stored before quitting. GDK only uses
//!   its Wayland backend (no X11 fallback) unless `GDK_BACKEND` says
//!   otherwise; without a display `run` fails before touching the folder.
//! - `activate`: the store, the media hub, the key router, the theme and the
//!   window; the X2 or unreadable-folder page; the first drain.
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
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use chrono::Utc;
use gtk::{gio, glib};
use qrec_core::model::Project;
use qrec_core::on_event::output::export_readout;
use qrec_core::protocol::ToastLevel;
use qrec_core::state::{HostKind, JobKind, RecordPhase, State};
use qrec_core::view::notices::{Notice, unavailable_notice};
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

/// `qrec_gui::run`.
pub fn run(args: GuiArgs) -> Result<u8, CoreError> {
    log_panics();
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
    // Steps 3 and 4 now, while GTK starts: the lock and the socket come
    // first. Not without a Wayland display: nothing is touched then.
    let gdk_backend = std::env::var_os("GDK_BACKEND");
    let early = wanted && display_reachable(&DisplayEnv::from_process());
    if early {
        startup.start_workers();
    }
    // A Wayland app: GDK never falls back to X11 (an X server of the
    // session, say), unless GDK_BACKEND asks for another backend.
    if gdk_backend.is_none() {
        gtk::gdk::set_allowed_backends("wayland");
    }
    if let Err(e) = gtk::init() {
        return Err(CoreError::unavailable(format!(
            "can't open the window: {e} (is there a Wayland display?)"
        )));
    }
    if let Err(e) = adw::init() {
        return Err(CoreError::unavailable(format!("libadwaita: {e}")));
    }
    if wanted && !early {
        startup.start_workers();
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
    let the_app = Rc::new(App {
        app: app.clone(),
        ctx: ctx.clone(),
        window,
        startup,
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
    // The main-loop probe (G§8.6, `QREC_GUI_PROBE=1`), from the first frame
    // to the quit.
    if crate::probe::requested() {
        crate::probe::start();
    }
    if testctl::mode().on {
        log_regions_when_mapped(&the_app);
    }
    the_app.startup.watch_signals(&the_app);
    // What the workers reported while the window was being built.
    queue.kick();
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

/// Where the custom symbolic icons are in the GResource.
pub const ICONS: &str = "/com/rsensato/qrec/icons";

/// Registers the compiled `resources/` (build.rs).
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
            Inbound::Test(request) => testctl::handle(self, request),
        }
    }

    /// The GUI actions the app owns: quit, the "Not built yet" keys (G§1.1),
    /// the blocking pages' buttons and the D-12 modal's.
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
        for id in ["palette.open", "keys.help"] {
            let store = self.ctx.store.clone();
            actions.register(
                id,
                Rc::new(move || {
                    if let Some(notice) = unavailable_notice(id, "", &store.state()) {
                        store.notify(notice);
                    }
                }),
            );
        }
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
        crate::probe::stop();
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

/// What says where the Wayland display is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisplayEnv {
    pub gdk_backend: Option<std::ffi::OsString>,
    pub wayland_display: Option<std::ffi::OsString>,
    pub wayland_socket: Option<std::ffi::OsString>,
    pub runtime_dir: Option<std::path::PathBuf>,
}

impl DisplayEnv {
    pub fn from_process() -> Self {
        Self {
            gdk_backend: std::env::var_os("GDK_BACKEND"),
            wayland_display: std::env::var_os("WAYLAND_DISPLAY"),
            wayland_socket: std::env::var_os("WAYLAND_SOCKET"),
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(Into::into),
        }
    }
}

/// Whether a Wayland display looks reachable (its socket exists), so the
/// startup workers may take the lock before GTK connects. A chosen
/// `GDK_BACKEND` or a passed `WAYLAND_SOCKET` counts as yes: GTK decides.
pub fn display_reachable(env: &DisplayEnv) -> bool {
    use std::os::unix::fs::FileTypeExt;
    if env.gdk_backend.is_some() || env.wayland_socket.is_some() {
        return true;
    }
    let name = env
        .wayland_display
        .clone()
        .unwrap_or_else(|| "wayland-0".into());
    let path = std::path::PathBuf::from(&name);
    let path = if path.is_absolute() {
        path
    } else {
        match &env.runtime_dir {
            Some(dir) => dir.join(path),
            None => return false,
        }
    };
    std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
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

    #[test]
    fn the_display_is_reachable_only_with_its_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(tmp.path().join("wayland-7")).unwrap();
        std::fs::write(tmp.path().join("wayland-file"), "not a socket").unwrap();
        let env = |display: Option<&str>| DisplayEnv {
            wayland_display: display.map(Into::into),
            runtime_dir: Some(tmp.path().to_path_buf()),
            ..DisplayEnv::default()
        };
        assert!(display_reachable(&env(Some("wayland-7"))));
        let absolute = tmp.path().join("wayland-7");
        assert!(display_reachable(&env(Some(absolute.to_str().unwrap()))));
        assert!(
            !display_reachable(&env(Some("wayland-1"))),
            "no such socket"
        );
        assert!(
            !display_reachable(&env(Some("wayland-file"))),
            "not a socket"
        );
        assert!(!display_reachable(&env(None)), "wayland-0 isn't there");
        assert!(!display_reachable(&DisplayEnv {
            wayland_display: Some("wayland-7".into()),
            ..DisplayEnv::default()
        }));
        // GTK decides when GDK_BACKEND or WAYLAND_SOCKET say where to go.
        assert!(display_reachable(&DisplayEnv {
            gdk_backend: Some("x11".into()),
            ..DisplayEnv::default()
        }));
        assert!(display_reachable(&DisplayEnv {
            wayland_socket: Some("5".into()),
            ..DisplayEnv::default()
        }));
        drop(listener);
    }
}
