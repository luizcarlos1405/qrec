//! Test mode and the test control channel (G§3.18, G§8.4).
//!
//! **Test mode** (`QREC_GUI_TEST`, read once): `1` (or any value but `0`)
//! turns it on: GTK's animations and the cursor blink are off, the
//! animation clock is frozen at `QREC_GUI_TEST_T_MS` (default 0), one-shot
//! animations jump to their end, reduced motion comes from the config only
//! (D-30), `QREC_GUI_TEST_NOW` (RFC 3339) is the local time of the export's
//! auto name, `QREC_GUI_TEST_LEVELS` (`<device>=<rms>/<peak>,…`, dBFS) pins
//! the meters' readings (`widgets::meter`), `QREC_GUI_TEST_SAVE_AS`
//! (`|`-separated paths, one per press) stands in for the export panel's
//! save dialog (no portal in the tests, G§9 R7),
//! `QREC_GUI_TEST_PANIC_ONCE` (`|`-separated) names guarded callbacks
//! (their `what`: `timeline: a frame`, `meter: refresh`) whose first run
//! panics, caught as any other (G§3.12: what a slot drives comes back),
//! and the control channel listens. Flags,
//! comma-separated: `stuck-gst` makes startup report GStreamer stuck
//! without initializing it (the stuck page, exit 5); `panic-activate`
//! panics in `activate` (the app quits with 1).
//!
//! **The control channel**: a socket `$XDG_RUNTIME_DIR/qrec-gui-test-<pid>.sock`
//! (0600, the test's private runtime dir; one per window, so a test can
//! drive two at once), served by thread `qrec-testctl`: one JSON request
//! line, one JSON reply line. Requests reach the main thread as
//! `Inbound::Test`; the socket thread waits for the answer up to the
//! request's `timeoutMs` (default 10 000, at most 60 000).
//!
//! | op | reply |
//! | --- | --- |
//! | `waitIdle` | `{"ok":true,"rev":R}` once the previews, the player and the io worker are idle (checked before the queue: what they send is queued before they say so), the queue is empty, the window's requests are answered (its own export once its job runs: `Store::unsettled_requests`), no source awaits the portal, the window isn't opening, the stage is idle (no program reload due; in Live, the engine settled its live scenes and the picture drew its live sources, also after a rebuild that sent no status: `media::live`), and a frame was drawn after that |
//! | `regions` | `{"regions":{"<name>":[x,y,w,h],…}}`: every `region-*` widget and named part (G§8.4), window coordinates; `names` adds widgets by name |
//! | `labels` | the text of every visible label under the widget `name` (default: the window), and `entries`: each visible text field's value by its nearest given name (`{"inspector-x":"1280"}`) |
//! | `dispatch` | `Store::dispatch` of `command` (`confirmed`: as confirmed), `{"ok":true}` at once |
//! | `action` | runs binding or GUI action `id` as its key would |
//! | `hints` | the hint bar's items and status (`hintbar::current_items`), the labels of the items it draws whole (`shown`: a narrow window leaves the trailing ones out) and whether a drawn one runs past the bar (`cut`, never) |
//! | `local` | a summary of `LocalUi` (with the panel's page and the autocut page's preview, `panelPage`, `autocutPreview`; whether the player plays, the program or a take, `playing`, and the program, `programPlaying`), the theme (and the window's `.reduced-motion`, and `panelTransition`, the panel pages' transition that follows it), the caught panics, the focused widget's path, the window's pid, the Program player's position (`playerNs`, `null` without a player), the bin rows still alive (`binRowsAlive`: a rebuild's dropped rows go, G§3.12 R11) and the panel's (`layerRowsAlive`, `audioRowsAlive`), the bin drawer's last slide as asked (`binSlide`: `open`, `animate`; nothing animates on start), the bin preview (`takePreview`: `take`, `playing`; `null` without one), the theme's subscribers (`themeSubscribers`: a dropped bin thumbnail's goes with it), the mud bars ticking (`mudBarsTicking`: none under the frozen clock), the meter bars on their refresh timer (`metersTicking`) and whether the export panel's Running clock runs (`exportClockTicking`: both come back with the next change after a caught panic, R65), and how many times the timeline's canvas re-rendered (`timelinePaints`: not while only the playhead moves) |
//! | `a11y` | every visible icon-only button, focusable region or custom widget without an accessible label (expected empty; needs `GTK_A11Y=test`) |
//! | `fonts` | the family Pango loads for each bundled font |
//! | `icons` | each custom symbolic icon drawn in a probe color: found, pixels, recolored |
//! | `art` | each illustration through `gtk::Svg`: its size, drawn pixels and SVG errors (none for `ok`), and a known-bad probe whose errors must be counted |
//!
//! Dispatches go through the Store (R8); the other ops only read.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDateTime};
use gtk::prelude::*;
use gtk::{gdk, glib, graphene};
use qrec_core::Command;
use qrec_shell::capture::LiveSourceState;
use serde_json::{Value, json};

use crate::app::App;
use crate::host::queue::{Inbound, Queue};
use crate::store::local::StartupPhase;
use crate::{guard, guarded, hintbar, theme};

/// `QREC_GUI_TEST` and its companions, as read at startup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestMode {
    /// Test mode is on.
    pub on: bool,
    /// `stuck-gst`: report GStreamer stuck without initializing it.
    pub stuck_gst: bool,
    /// `panic-activate`: panic in `activate`, after the app is up.
    pub panic_activate: bool,
    /// `QREC_GUI_TEST_PANIC_ONCE`: the guarded callbacks (by their `what`)
    /// whose first run panics (caught: what they drive must come back,
    /// G§3.12 R10).
    pub panic_once: Vec<String>,
    /// The frozen animation clock (`QREC_GUI_TEST_T_MS`).
    pub t_ms: u64,
    /// The local time of the export's auto name (`QREC_GUI_TEST_NOW`: RFC
    /// 3339, its offset ignored, or `YYYY-MM-DDTHH:MM:SS`).
    pub now: Option<NaiveDateTime>,
}

/// The test variables besides `QREC_GUI_TEST` itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct TestVars<'a> {
    pub t_ms: Option<&'a str>,
    pub now: Option<&'a str>,
    pub panic_once: Option<&'a str>,
}

impl TestMode {
    /// From the variables' values.
    pub fn parse(test: Option<&str>, t_ms: Option<&str>, now: Option<&str>) -> Self {
        Self::parse_all(
            test,
            TestVars {
                t_ms,
                now,
                ..TestVars::default()
            },
        )
    }

    /// From every variable's value.
    pub fn parse_all(test: Option<&str>, vars: TestVars<'_>) -> Self {
        let TestVars {
            t_ms,
            now,
            panic_once,
        } = vars;
        let flags: Vec<&str> = test
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .collect();
        let on = !flags.is_empty() && flags != ["0"];
        Self {
            on,
            stuck_gst: on && flags.contains(&"stuck-gst"),
            panic_activate: on && flags.contains(&"panic-activate"),
            panic_once: panic_once
                .filter(|_| on)
                .map(|names| {
                    names
                        .split('|')
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            t_ms: t_ms.and_then(|t| t.trim().parse().ok()).unwrap_or(0),
            now: now.and_then(|n| {
                let n = n.trim();
                DateTime::parse_from_rfc3339(n)
                    .map(|t| t.naive_local())
                    .or_else(|_| NaiveDateTime::parse_from_str(n, "%Y-%m-%dT%H:%M:%S"))
                    .ok()
            }),
        }
    }

    fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        let (t_ms, now) = (var("QREC_GUI_TEST_T_MS"), var("QREC_GUI_TEST_NOW"));
        let panic_once = var("QREC_GUI_TEST_PANIC_ONCE");
        Self::parse_all(
            var("QREC_GUI_TEST").as_deref(),
            TestVars {
                t_ms: t_ms.as_deref(),
                now: now.as_deref(),
                panic_once: panic_once.as_deref(),
            },
        )
    }
}

/// The process's test mode (read on first use).
pub fn mode() -> &'static TestMode {
    static MODE: OnceLock<TestMode> = OnceLock::new();
    MODE.get_or_init(TestMode::from_env)
}

/// One control request on its way to the main thread, with where its reply
/// goes.
pub struct TestRequest {
    /// The request line, parsed.
    pub request: Value,
    /// Called once with the reply line's JSON.
    pub reply: Box<dyn FnOnce(Value) + Send>,
    /// When the socket thread stops waiting.
    pub deadline: Instant,
}

impl std::fmt::Debug for TestRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestRequest")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

// ------------------------------------------------------------- the socket

/// The socket's file name in `$XDG_RUNTIME_DIR` for the window `pid`.
pub fn socket_name(pid: u32) -> String {
    format!("qrec-gui-test-{pid}.sock")
}
/// A request's default wait (`timeoutMs`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest wait a request may ask for (R6).
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest request line.
const MAX_LINE: u64 = 1 << 20;

/// Where this window's socket is: `$XDG_RUNTIME_DIR/qrec-gui-test-<pid>.sock`.
pub fn socket_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(|dir| PathBuf::from(dir).join(socket_name(std::process::id())))
}

/// The listening channel; the socket goes when it drops.
#[derive(Debug)]
pub struct Channel {
    path: PathBuf,
}

impl Drop for Channel {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Listens in test mode (thread `qrec-testctl`) on this window's own
/// socket; `None` outside test mode.
pub fn listen(queue: Queue) -> Option<Channel> {
    if !mode().on {
        return None;
    }
    let path = socket_path()?;
    // A dead process's socket of the same pid, if any.
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::warn!("test channel: can't listen on {}: {e}", path.display());
            return None;
        }
    };
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!("test channel: chmod {}: {e}", path.display());
    }
    let spawned = thread::Builder::new()
        .name("qrec-testctl".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let queue = queue.clone();
                let _ = thread::Builder::new()
                    .name("qrec-testctl".into())
                    .spawn(move || serve(stream, &queue));
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("test channel: can't start its thread: {e}");
        return None;
    }
    tracing::info!("test channel on {}", path.display());
    Some(Channel { path })
}

/// One connection: a request line in, its reply line out.
fn serve(stream: UnixStream, queue: &Queue) {
    let _ = stream.set_read_timeout(Some(DEFAULT_TIMEOUT));
    let mut line = String::new();
    let read = BufReader::new(&stream).take(MAX_LINE).read_line(&mut line);
    let reply = match read {
        Err(e) => json!({"ok": false, "error": format!("can't read the request: {e}")}),
        Ok(_) => match serde_json::from_str::<Value>(line.trim()) {
            Err(e) => json!({"ok": false, "error": format!("not JSON: {e}")}),
            Ok(request) => {
                let wait = request["timeoutMs"]
                    .as_u64()
                    .map_or(DEFAULT_TIMEOUT, Duration::from_millis)
                    .min(MAX_TIMEOUT);
                let (tx, rx) = crossbeam_channel::bounded(1);
                queue.push(Inbound::Test(TestRequest {
                    request,
                    reply: Box::new(move |value| {
                        let _ = tx.send(value);
                    }),
                    deadline: Instant::now() + wait,
                }));
                rx.recv_timeout(wait + Duration::from_millis(200))
                    .unwrap_or_else(|_| json!({"ok": false, "error": "timed out"}))
            }
        },
    };
    let mut stream = stream;
    let _ = writeln!(stream, "{reply}");
}

// ------------------------------------------------------------- the ops

/// A reply that may be sent later (once).
type Reply = Rc<RefCell<Option<Box<dyn FnOnce(Value) + Send>>>>;

fn answer(reply: &Reply, value: Value) {
    if let Some(send) = reply.borrow_mut().take() {
        send(value);
    }
}

/// A request on the main thread.
pub fn handle(app: &Rc<App>, request: TestRequest) {
    let TestRequest {
        request,
        reply,
        deadline,
    } = request;
    let reply: Reply = Rc::new(RefCell::new(Some(reply)));
    let op = request["op"].as_str().unwrap_or_default().to_owned();
    tracing::debug!("test channel: {op}");
    let value = match op.as_str() {
        "waitIdle" => return wait_idle(app, deadline, reply),
        "regions" => regions(app, &request),
        "labels" => labels(app, &request),
        "dispatch" => dispatch(app, &request),
        "action" => action(app, &request),
        "hints" => hints(app),
        "local" => local(app),
        "a11y" => a11y(app),
        "fonts" => {
            let resolved = theme::fonts::resolve();
            json!({
                "ok": theme::fonts::all_resolved(&resolved),
                "fonts": resolved.into_iter().collect::<BTreeMap<_, _>>(),
            })
        }
        "icons" => icons(app),
        "art" => art(app),
        other => json!({"ok": false, "error": format!("no op {other:?}")}),
    };
    answer(&reply, value);
}

/// Why the window isn't idle (empty: idle).
fn busy(app: &App) -> Vec<String> {
    let store = &app.ctx.store;
    let mut why = Vec::new();
    // The threads that feed the queue before the queue: the preview cache's
    // answers and the player's events are in the queue before the cache or
    // the player says it is idle, so none slips between the checks.
    if !app.ctx.media.previews_idle() {
        why.push("previews".to_owned());
    }
    if app.ctx.media.player().is_some_and(|p| !p.idle()) {
        why.push("the player".to_owned());
    }
    if !app.ctx.media.io_idle() {
        why.push("io".to_owned());
    }
    if !store.queue().is_empty() {
        why.push("the inbound queue".to_owned());
    }
    // The window's own export counts until its job runs: then its progress
    // comes as snapshots and its answer only when it ends (G§3.18).
    let pending = store.unsettled_requests();
    if pending > 0 {
        why.push(format!("{pending} request(s) unanswered"));
    }
    let local = store.local();
    if matches!(local.startup, StartupPhase::Opening { .. }) {
        why.push("opening".to_owned());
    }
    if local
        .live_status
        .iter()
        .any(|s| matches!(s.state, LiveSourceState::AwaitingPortal))
    {
        why.push("the portal".to_owned());
    }
    // The stage's own (WP4): the live picture's first frames in Live, a
    // program reload still due.
    why.extend(crate::stage::busy());
    why
}

/// `waitIdle`: polls every 10 ms until idle, then answers after the next
/// frame is drawn (and the window is still idle then).
fn wait_idle(app: &Rc<App>, deadline: Instant, reply: Reply) {
    let weak = Rc::downgrade(app);
    guard::timeout_local(
        Duration::from_millis(10),
        "test channel: waitIdle",
        move || {
            let Some(app) = weak.upgrade() else {
                answer(&reply, json!({"ok": false, "error": "the window is gone"}));
                return glib::ControlFlow::Break;
            };
            let why = busy(&app);
            if why.is_empty() {
                after_paint(&app, deadline, Rc::clone(&reply));
                return glib::ControlFlow::Break;
            }
            if Instant::now() >= deadline {
                answer(
                    &reply,
                    json!({"ok": false, "error": format!("not idle: {}", why.join(", "))}),
                );
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        },
    );
}

/// Answers `waitIdle` after the window's next `after-paint`.
fn after_paint(app: &Rc<App>, deadline: Instant, reply: Reply) {
    let window = app.window.window.clone();
    let Some(clock) = window.frame_clock() else {
        answer(&reply, json!({"ok": true, "rev": app.ctx.store.rev()}));
        return;
    };
    let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
    let weak = Rc::downgrade(app);
    let slot = Rc::clone(&handler);
    let id = clock.connect_after_paint(guarded!("test channel: after-paint", move |clock| {
        if let Some(id) = slot.borrow_mut().take() {
            clock.disconnect(id);
        }
        let Some(app) = weak.upgrade() else {
            answer(&reply, json!({"ok": false, "error": "the window is gone"}));
            return;
        };
        if busy(&app).is_empty() {
            answer(&reply, json!({"ok": true, "rev": app.ctx.store.rev()}));
        } else {
            wait_idle(&app, deadline, Rc::clone(&reply));
        }
    }));
    *handler.borrow_mut() = Some(id);
    window.queue_draw();
}

/// `regions`: every `region-*` widget and named part, plus the widgets
/// named in `names` (a button a case clicks, say).
fn regions(app: &App, request: &Value) -> Value {
    let mut regions = app.window.regions();
    let names: Vec<&str> = request["names"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let root: gtk::Widget = app.window.window.clone().upcast();
    walk_visible(&root, &mut |w| {
        let name = w.widget_name();
        if names.contains(&name.as_str())
            && let Some(b) = w.compute_bounds(&app.window.window)
        {
            regions.insert(
                name.to_string(),
                [
                    f64::from(b.x()),
                    f64::from(b.y()),
                    f64::from(b.width()),
                    f64::from(b.height()),
                ],
            );
        }
        true
    });
    json!({"regions": regions})
}

/// `labels`: the text of every visible label under the widget named
/// `name` (default: the window), in tree order.
fn labels(app: &App, request: &Value) -> Value {
    let root: gtk::Widget = app.window.window.clone().upcast();
    let start = match request["name"].as_str() {
        Some(name) => match crate::window::find_named(&root, name) {
            Some(w) => w,
            None => return json!({"ok": false, "error": format!("no widget named {name}")}),
        },
        None => root,
    };
    let mut texts = Vec::new();
    let mut entries = serde_json::Map::new();
    walk_visible(&start, &mut |w| {
        if let Some(label) = w.downcast_ref::<gtk::Label>()
            && !label.text().is_empty()
        {
            texts.push(label.text().to_string());
        }
        // A text field's value under its nearest given name, its own or an
        // ancestor's (a `NumericField`'s box: `inspector-x`; GTK names an
        // unnamed widget after its type).
        if let Some(text) = w.downcast_ref::<gtk::Text>() {
            let mut at = Some(w.clone());
            while let Some(n) = at.as_ref().filter(|n| n.widget_name() == n.type_().name()) {
                at = n.parent();
            }
            if let Some(n) = at {
                entries.insert(n.widget_name().to_string(), json!(text.text().to_string()));
            }
        }
        true
    });
    json!({"ok": true, "labels": texts, "entries": entries})
}

fn dispatch(app: &App, request: &Value) -> Value {
    let cmd: Command = match serde_json::from_value(request["command"].clone()) {
        Ok(cmd) => cmd,
        Err(e) => return json!({"ok": false, "error": format!("not a command: {e}")}),
    };
    if request["confirmed"].as_bool().unwrap_or(false) {
        app.ctx.store.dispatch_confirmed(cmd);
    } else {
        app.ctx.store.dispatch(cmd);
    }
    json!({"ok": true})
}

fn action(app: &App, request: &Value) -> Value {
    let Some(id) = request["id"].as_str() else {
        return json!({"ok": false, "error": "no id"});
    };
    if app.ctx.store.keymap().binding(id).is_some() {
        app.ctx.keys.run_binding(id);
        json!({"ok": true})
    } else if app.ctx.actions.run(id) {
        json!({"ok": true})
    } else {
        json!({"ok": false, "error": format!("no binding or GUI action {id}")})
    }
}

fn hints(app: &App) -> Value {
    let (items, status) = hintbar::current_items(&app.ctx);
    let root: gtk::Widget = app.window.window.clone().upcast();
    let (shown, cut) = hintbar::shown(&root);
    json!({"items": items, "status": status, "shown": shown, "cut": cut})
}

fn local(app: &App) -> Value {
    let local = app.ctx.store.local();
    let startup = match &local.startup {
        StartupPhase::Opening { slow } => json!({"phase": "opening", "slow": slow}),
        StartupPhase::Open => json!({"phase": "open"}),
        StartupPhase::Blocked(kind) => json!({"phase": "blocked", "page": kind}),
        StartupPhase::Closing => json!({"phase": "closing"}),
    };
    json!({
        "startup": startup,
        "focus": local.focus,
        "overlays": local.overlays.iter().map(|o| format!("{o:?}")).collect::<Vec<_>>(),
        "drag": local.drag,
        "layout": {
            "width": format!("{:?}", local.layout.width),
            "compact": local.layout.compact,
        },
        "playing": local.playing,
        "programPlaying": local.program_playing,
        "playingRate": local.playing_rate,
        "panelPage": local.panel_page,
        "autocutPreview": local.autocut_preview.as_deref(),
        "binSelection": local.bin_selection.iter().map(|t| t.as_str().to_owned()).collect::<Vec<_>>(),
        "takePreview": local.take_preview.as_ref().map(|p| json!({"take": p.take.as_str(), "playing": p.playing})),
        "binRowsAlive": crate::bin_drawer::rows_alive(),
        "layerRowsAlive": crate::panel::rows::rows_alive(),
        "audioRowsAlive": crate::panel::audio::rows_alive(),
        "binSlide": crate::bin_drawer::slide_shown()
            .map(|(open, animate)| json!({"open": open, "animate": animate})),
        "themeSubscribers": app.ctx.theme.subscribers(),
        "mudBarsTicking": crate::widgets::mud_bar::ticking(),
        "metersTicking": crate::widgets::meter::ticking(),
        "exportClockTicking": crate::export::clock_ticking(),
        "timelinePaints": crate::timeline::canvas_paints(),
        "keymapProblems": local.keymap_problems,
        "theme": theme::css_name(app.ctx.theme.id()),
        "scale": app.ctx.theme.scale(),
        "reducedMotion": app.ctx.theme.motion().reduced,
        "reducedMotionClass": app.window.window.has_css_class("reduced-motion"),
        "panelTransition": panel_transition(app),
        "rev": app.ctx.store.rev(),
        "panics": guard::panics(),
        "focusPath": focus_path(app),
        "focusInTopLayer": focus_in_top_layer(app),
        "pid": std::process::id(),
        "playerNs": app.ctx.media.position_ns(),
    })
}

/// How the panel's pages switch (`panel::PAGES`): `"slide-left-right"`, or
/// `"none"` under reduced motion (test mode turns GTK's animations off, so
/// the stack's transition is what a case can see); `None` without a panel.
fn panel_transition(app: &App) -> Option<&'static str> {
    let root: gtk::Widget = app.window.window.clone().upcast();
    let stack = crate::window::find_named(&root, crate::panel::PAGES)?
        .downcast::<gtk::Stack>()
        .ok()?;
    Some(match stack.transition_type() {
        gtk::StackTransitionType::None => "none",
        gtk::StackTransitionType::SlideLeftRight => "slide-left-right",
        _ => "other",
    })
}

/// Whether the focused widget is inside the newest open layer (a modal,
/// Settings…); `None` without an open layer.
fn focus_in_top_layer(app: &App) -> Option<bool> {
    let top = app.ctx.window.top_widget()?;
    let focus = gtk::prelude::GtkWindowExt::focus(&app.window.window);
    Some(focus.is_some_and(|f| f == top || f.is_ancestor(&top)))
}

/// The names (or types) of the focused widget and its ancestors, the
/// focused one first; empty without a focus.
fn focus_path(app: &App) -> Vec<String> {
    let mut path = Vec::new();
    let mut current = gtk::prelude::GtkWindowExt::focus(&app.window.window);
    while let Some(widget) = current {
        let name = widget.widget_name();
        path.push(if name.is_empty() {
            widget.type_().name().to_owned()
        } else {
            name.to_string()
        });
        current = widget.parent();
    }
    path
}

// ----------------------------------------------------------- a11y walk

/// Every visible icon-only button, focusable region and focusable custom
/// widget (ours: `Qrec…` types) without an accessible label or a
/// labelled-by relation. Window controls are GTK's own (labeled by it).
fn a11y(app: &App) -> Value {
    if std::env::var("GTK_A11Y").as_deref() != Ok("test") {
        return json!({"ok": false, "error": "the a11y walk needs GTK_A11Y=test"});
    }
    let mut missing = Vec::new();
    let root: gtk::Widget = app.window.window.clone().upcast();
    walk_visible(&root, &mut |w| {
        let type_name = w.type_().name().to_owned();
        if type_name == "GtkWindowControls" {
            return false;
        }
        let name = w.widget_name().to_string();
        let custom = type_name.starts_with("Qrec") && w.is_focusable();
        let region = name.starts_with("region-") && w.is_focusable();
        if (is_icon_only_button(w) || custom || region) && !has_label(w) {
            let bounds = w
                .compute_bounds(&app.window.window)
                .map(|b| [b.x(), b.y(), b.width(), b.height()]);
            missing.push(json!({"type": type_name, "name": name, "bounds": bounds}));
        }
        true
    });
    json!({"ok": missing.is_empty(), "missing": missing})
}

/// Visits the visible, mapped widgets under `root`; `f` returns whether to
/// go into the widget's children.
fn walk_visible(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget) -> bool) {
    if !root.is_visible() || !root.is_mapped() {
        return;
    }
    if !f(root) {
        return;
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        walk_visible(&c, f);
        child = c.next_sibling();
    }
}

fn is_icon_only_button(w: &gtk::Widget) -> bool {
    let button = w.is::<gtk::Button>() || w.is::<gtk::MenuButton>();
    button && !has_text(w)
}

fn has_text(w: &gtk::Widget) -> bool {
    if let Some(label) = w.downcast_ref::<gtk::Label>()
        && !label.text().is_empty()
    {
        return true;
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if c.is_visible() && has_text(&c) {
            return true;
        }
        child = c.next_sibling();
    }
    false
}

fn has_label(w: &gtk::Widget) -> bool {
    gtk::test_accessible_has_property(w, gtk::AccessibleProperty::Label)
        || gtk::test_accessible_has_relation(w, gtk::AccessibleRelation::LabelledBy)
}

// --------------------------------------------------------- icons and art

/// The custom symbolic icons (G§5.2).
pub const CUSTOM_ICONS: [&str; 10] = [
    "qrec-record-symbolic",
    "qrec-success-symbolic",
    "qrec-magnet-symbolic",
    "qrec-monitor-x-symbolic",
    "qrec-package-x-symbolic",
    "qrec-life-buoy-symbolic",
    "qrec-timer-off-symbolic",
    "qrec-gauge-symbolic",
    "qrec-layers-symbolic",
    "qrec-audio-lines-symbolic",
];

/// The Adwaita symbolic names G§5.2 maps the pen's icons to (checked
/// against adwaita-icon-theme 50.0).
pub const ADWAITA_ICONS: [&str; 44] = [
    "media-playback-stop-symbolic",
    "media-playback-start-symbolic",
    "media-playback-pause-symbolic",
    "go-previous-symbolic",
    "go-next-symbolic",
    "media-skip-backward-symbolic",
    "media-skip-forward-symbolic",
    "camera-photo-symbolic",
    "document-save-symbolic",
    "edit-find-symbolic",
    // Settings: the gear as drawn (preferences-system-symbolic is tools in
    // Adwaita 50).
    "applications-system-symbolic",
    "camera-video-symbolic",
    "video-x-generic-symbolic",
    "view-reveal-symbolic",
    "view-conceal-symbolic",
    "changes-prevent-symbolic",
    "changes-allow-symbolic",
    "list-drag-handle-symbolic",
    "list-add-symbolic",
    "video-display-symbolic",
    "camera-web-symbolic",
    "audio-input-microphone-symbolic",
    "audio-speakers-symbolic",
    "package-x-generic-symbolic",
    "edit-cut-symbolic",
    "preferences-other-symbolic",
    "zoom-in-symbolic",
    "zoom-out-symbolic",
    "zoom-fit-best-symbolic",
    "user-trash-symbolic",
    "folder-open-symbolic",
    "folder-symbolic",
    "edit-copy-symbolic",
    "adw-external-link-symbolic",
    "dialog-information-symbolic",
    "dialog-warning-symbolic",
    "dialog-error-symbolic",
    "object-select-symbolic",
    "pan-down-symbolic",
    "view-more-symbolic",
    "window-close-symbolic",
    "insert-link-symbolic",
    "document-edit-symbolic",
    "edit-undo-symbolic",
];

/// Renders `node` (`width`×`height`) with the window's renderer; RGBA8.
fn render(app: &App, node: &gtk::gsk::RenderNode, width: f32, height: f32) -> Option<Vec<u8>> {
    let renderer = app.window.window.renderer()?;
    let texture =
        renderer.render_texture(node, Some(&graphene::Rect::new(0.0, 0.0, width, height)));
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, _stride) = downloader.download_bytes();
    Some(bytes.to_vec())
}

/// Each custom icon drawn at 16 px in a probe color: it recolors when every
/// pixel it covers takes the probe color (a stroke that doesn't recolor
/// would stay black).
fn icons(app: &App) -> Value {
    let display = WidgetExt::display(&app.window.window);
    let icon_theme = gtk::IconTheme::for_display(&display);
    let probe = gdk::RGBA::new(1.0, 0.0, 1.0, 1.0);
    let mut all = true;
    let mut out = Vec::new();
    for name in CUSTOM_ICONS {
        if !icon_theme.has_icon(name) {
            all = false;
            out.push(json!({"name": name, "found": false}));
            continue;
        }
        let paintable = icon_theme.lookup_icon(
            name,
            &[],
            16,
            1,
            gtk::TextDirection::Ltr,
            gtk::IconLookupFlags::empty(),
        );
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot_symbolic(&snapshot, 16.0, 16.0, &[probe, probe, probe, probe]);
        let pixels = snapshot
            .to_node()
            .and_then(|node| render(app, &node, 16.0, 16.0))
            .unwrap_or_default();
        let (mut drawn, mut off_color) = (0, 0);
        for px in pixels.chunks_exact(4) {
            if px[3] > 128 {
                drawn += 1;
                // Premultiplied or not, magenta has no green and red = blue.
                if px[1] > 40 || px[0].abs_diff(px[2]) > 40 {
                    off_color += 1;
                }
            }
        }
        let recolored = drawn > 8 && off_color == 0;
        all &= recolored;
        out.push(json!({
            "name": name,
            "found": true,
            "pixels": drawn,
            "offColor": off_color,
            "recolored": recolored,
        }));
    }
    let missing: Vec<&str> = ADWAITA_ICONS
        .iter()
        .copied()
        .filter(|name| !icon_theme.has_icon(name))
        .collect();
    all &= missing.is_empty();
    json!({"ok": all, "icons": out, "adwaitaMissing": missing})
}

/// Each illustration through `gtk::Svg` in the theme's colors: its size and
/// how many pixels it draws.
fn art(app: &App) -> Value {
    let mut all = true;
    let mut out = Vec::new();
    let palette = app.ctx.theme.palette();
    for which in theme::art::Art::ALL {
        let Some(svg) = theme::art::paintable_checked(which, &palette) else {
            all = false;
            out.push(json!({"name": which.name(), "loaded": false}));
            continue;
        };
        let (w, h) = (svg.svg.intrinsic_width(), svg.svg.intrinsic_height());
        let snapshot = gtk::Snapshot::new();
        svg.svg.snapshot(&snapshot, f64::from(w), f64::from(h));
        let pixels = snapshot
            .to_node()
            .and_then(|node| render(app, &node, w as f32, h as f32))
            .unwrap_or_default();
        let drawn = pixels.chunks_exact(4).filter(|px| px[3] > 0).count();
        let errors = svg.errors();
        all &= w > 0 && h > 0 && drawn > 0 && errors == 0;
        out.push(json!({
            "name": which.name(),
            "loaded": true,
            "size": [w, h],
            "pixels": drawn,
            "errors": errors,
        }));
    }
    // What GtkSvg drops while it parses is counted (an unsupported element
    // here): the art's zero means something.
    let probe = theme::art::load("probe", PROBE_SVG).errors();
    all &= probe > 0;
    json!({"ok": all, "art": out, "probeErrors": probe})
}

/// An SVG with an element GtkSvg doesn't support (`<foreignObject>`).
const PROBE_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\">\
    <foreignObject width=\"8\" height=\"8\"/><rect width=\"8\" height=\"8\" fill=\"black\"/></svg>";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_parses_its_flags() {
        assert_eq!(TestMode::parse(None, None, None), TestMode::default());
        assert!(!TestMode::parse(Some(""), None, None).on);
        assert!(!TestMode::parse(Some("0"), None, None).on);
        let on = TestMode::parse(Some("1"), Some("250"), Some("2026-10-14T15:30:12+00:00"));
        assert!(on.on && !on.stuck_gst);
        assert_eq!(on.t_ms, 250);
        assert_eq!(on.now.unwrap().to_string(), "2026-10-14 15:30:12");
        let naive = TestMode::parse(Some("1"), None, Some("2026-10-14T15:30:12"));
        assert_eq!(naive.now, on.now);
        let stuck = TestMode::parse(Some("stuck-gst"), Some("x"), Some("yesterday"));
        assert!(stuck.on && stuck.stuck_gst);
        assert_eq!((stuck.t_ms, stuck.now), (0, None));
        assert!(TestMode::parse(Some("1,stuck-gst"), None, None).stuck_gst);
        let vars = TestVars {
            panic_once: Some("timeline: a frame| meter: refresh |"),
            ..TestVars::default()
        };
        let faults = TestMode::parse_all(Some("1"), vars);
        assert_eq!(faults.panic_once, ["timeline: a frame", "meter: refresh"]);
        assert!(!faults.panic_activate && on.panic_once.is_empty());
        // Outside test mode, none of it.
        assert!(TestMode::parse_all(None, vars).panic_once.is_empty());
    }

    /// The socket thread's half: a request line reaches the queue with a
    /// way back, and its reply comes out as one line.
    #[test]
    fn a_request_line_gets_its_reply_line() {
        let queue = Queue::new(|| {});
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = {
            let queue = queue.clone();
            thread::spawn(move || serve(server, &queue))
        };
        writeln!(client, r#"{{"op":"regions","timeoutMs":5000}}"#).unwrap();
        let item = loop {
            if let Some(item) = queue.take_all().into_iter().next() {
                break item;
            }
            thread::sleep(Duration::from_millis(5));
        };
        let Inbound::Test(request) = item else {
            panic!("{item:?}")
        };
        assert_eq!(request.request["op"], "regions");
        assert!(request.deadline > Instant::now() + Duration::from_secs(4));
        (request.reply)(json!({"regions": {}}));
        worker.join().unwrap();
        let mut line = String::new();
        BufReader::new(&client).read_line(&mut line).unwrap();
        assert_eq!(line.trim(), r#"{"regions":{}}"#);
    }

    #[test]
    fn garbage_gets_an_error_line() {
        let queue = Queue::new(|| {});
        let (mut client, server) = UnixStream::pair().unwrap();
        writeln!(client, "not json").unwrap();
        serve(server, &queue);
        let mut line = String::new();
        BufReader::new(&client).read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["ok"], false);
        assert!(queue.is_empty());
    }
}
