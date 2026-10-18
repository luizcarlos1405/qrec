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
//! `QREC_GUI_TEST_SEND_DELAY_MS` holds every command that long before it
//! goes to the host, in order (a slow Session: a reply comes after the
//! next gesture, R22, R23), and the control channel listens. Flags,
//! comma-separated: `stuck-gst` makes startup report GStreamer stuck
//! without initializing it (the stuck page, exit 5); `panic-activate`
//! panics in `activate` (the app quits with 1); `animations` leaves GTK's
//! own animations on (libadwaita's, the stacks', CSS transitions; Qrec's
//! own still jump to their end): WP10's reduced-motion audit sees
//! libadwaita's sheet slide, or not; `system-motion` (with `animations`)
//! lets the system's reduced-motion settings count as outside test mode
//! (D-30: GTK's `gtk-interface-reduced-motion` and animations), so a case
//! with a stand-in desktop sees the window follow it.
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
//! | `sources` | the source discovery's last answer (`LocalUi.sources`, `inventory`: `null` before one); with `inventory` (a `SourceInventory`), that answer instead, through the queue as discovery's own (the first run's rows follow it, R25) |
//! | `hints` | the hint bar's items and status (`hintbar::current_items`), the labels of the items it draws whole (`shown`: a narrow window leaves the trailing ones out) and whether a drawn one runs past the bar (`cut`, never) |
//! | `local` | a summary of `LocalUi` (with the panel's page and the autocut page's preview, `panelPage`, `autocutPreview`; whether the player plays, the program or a take, `playing`, and the program, `programPlaying`), the theme (and the window's `.reduced-motion`, and `panelTransition`, the panel pages' transition that follows it, and GTK's own animation setting, `gtkAnimations`: off in test mode but with its `animations` flag, and while the Narrow sheet is open under reduced motion; libadwaita's color scheme, `colorScheme`: `scheme`, `dark`), whether GTK's fundamental types were registered before startup's workers (`fundamentalsFirst`), the settings changes the follower applied and whether one is pending (`settingsApplied`, `settingsPending`, WP10), the live controls that show a binding's keys (`keyedControls`: a keymap reload makes none), the caught panics, the focused widget's path and its bounds (`focusBounds`), whether the window shows focus (`focusVisible`), what it announced to screen readers (`announcements`: `message`, `priority`, and `sent`, false without a context that announces: WP10), the window's pid, the Program player's position (`playerNs`, `null` without a player), the bin rows still alive (`binRowsAlive`: a rebuild's dropped rows go, G§3.12 R11) and the panel's (`layerRowsAlive`, `audioRowsAlive`), the bin drawer's last slide as asked (`binSlide`: `open`, `animate`; nothing animates on start), the bin preview (`takePreview`: `take`, `playing`; `null` without one), the theme's subscribers (`themeSubscribers`: a dropped bin thumbnail's goes with it), the mud bars ticking (`mudBarsTicking`: none under the frozen clock), the meter bars on their refresh timer (`metersTicking`) and whether the export panel's Running clock runs (`exportClockTicking`: both come back with the next change after a caught panic, R65), how many times the timeline's canvas re-rendered (`timelinePaints`: not while only the playhead moves), and how many columns of the ghost's waveform its last paint with one worked out (`timelineGhostColumns`: those in view, R01) |
//! | `a11y` | every visible icon-only button, focusable region or custom widget without an accessible label (expected empty; needs `GTK_A11Y=test`) |
//! | `fonts` | the family Pango loads for each bundled font; GTK's text settings as they are (`rendering`: `mode`, `hintMetrics`, `hinting`, `hintStyle`, `antialias`; `QREC_GUI_TEST_FONT_RENDERING` names another setup, `theme::fonts::Rendering`) and the window's GSK renderer (`renderer`, `GskGLRenderer`, …); with `measure` (`[{"family", "sizePx", "weight", "text"}]`), each text's `logical` and `ink` widths in px laid out in the window's Pango context (`widths`) |
//! | `icons` | every icon of the GResource (`count`) through the window's icon theme: found, its own resource `file`, and drawn at 16 px with a probe color in each of GTK's four symbolic slots, every pixel in the foreground's (`recolored`: symbolic, recolored in full, no status color) |
//! | `art` | each illustration through `gtk::Svg`: its size, drawn pixels and SVG errors (none for `ok`), and a known-bad probe whose errors must be counted; with `compare` (a folder of the pen's PNGs, `design/export/art/`), each drawn at 1× and 2× and the REC glyph through the icon theme, against the pen's PNG within antialiasing (`compared`: the worst excess and the share over 8 levels, over `bg` and over white) |
//! | `probe` | the main-loop probe's report (`probe::report`, G§8.6): `on`, its `ticks`, `stalls` (over 50 ms), `longestMs` and the newest stalls `kept` (`atMs`, `driftMs`, `ran`: the dispatch that held the loop); `on: false` without `QREC_GUI_PROBE=1` |
//!
//! Dispatches go through the Store (R8), a pushed inventory through the
//! queue as discovery's answer; the other ops only read.

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
    /// `animations`: GTK's own animations stay on (test mode turns them
    /// off otherwise, G§8.4).
    pub animations: bool,
    /// `system-motion`: the system's reduced-motion settings count (test
    /// mode takes reduced motion from the config only otherwise, R29).
    pub system_motion: bool,
    /// `QREC_GUI_TEST_PANIC_ONCE`: the guarded callbacks (by their `what`)
    /// whose first run panics (caught: what they drive must come back,
    /// G§3.12 R10).
    pub panic_once: Vec<String>,
    /// `QREC_GUI_TEST_SEND_DELAY_MS`: how long each command waits before
    /// it goes to the host (in order; `Store::dispatch_with`).
    pub send_delay: Option<Duration>,
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
    pub send_delay_ms: Option<&'a str>,
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
            send_delay_ms,
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
            animations: on && flags.contains(&"animations"),
            system_motion: on && flags.contains(&"system-motion"),
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
            send_delay: send_delay_ms
                .filter(|_| on)
                .and_then(|ms| ms.trim().parse().ok())
                .filter(|ms| *ms > 0)
                .map(Duration::from_millis),
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
        let delay = var("QREC_GUI_TEST_SEND_DELAY_MS");
        Self::parse_all(
            var("QREC_GUI_TEST").as_deref(),
            TestVars {
                t_ms: t_ms.as_deref(),
                now: now.as_deref(),
                panic_once: panic_once.as_deref(),
                send_delay_ms: delay.as_deref(),
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
        "sources" => sources(app, &request),
        "hints" => hints(app),
        "local" => local(app),
        "a11y" => a11y(app),
        "fonts" => {
            let resolved = theme::fonts::resolve();
            let rendering = gtk::Settings::default().map(|s| theme::fonts::settings_now(&s));
            let renderer = app
                .window
                .window
                .renderer()
                .map(|r| r.type_().name().to_string());
            json!({
                "ok": theme::fonts::all_resolved(&resolved),
                "fonts": resolved.into_iter().collect::<BTreeMap<_, _>>(),
                "rendering": rendering,
                "renderer": renderer,
                "widths": measure_texts(app, &request),
            })
        }
        "icons" => icons(app),
        "art" => art(app, &request),
        "probe" => crate::probe::report(),
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

/// `sources`: the discovery's last answer, or a new one in its place.
fn sources(app: &App, request: &Value) -> Value {
    let wanted = &request["inventory"];
    if wanted.is_null() {
        let local = app.ctx.store.local();
        return json!({"ok": true, "inventory": local.sources.as_deref()});
    }
    match serde_json::from_value::<qrec_shell::sources::SourceInventory>(wanted.clone()) {
        Ok(inventory) => {
            app.ctx.store.queue().push(Inbound::Sources(inventory));
            json!({"ok": true})
        }
        Err(e) => json!({"ok": false, "error": format!("not an inventory: {e}")}),
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
    let mut summary = json!({
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
        "timelineGhostColumns": crate::timeline::ghost_columns(),
        "keymapProblems": local.keymap_problems,
    });
    // The rest in a second `json!`: one with every key passes serde_json's
    // macro recursion limit (WP10 integration).
    let rest = json!({
        "theme": theme::css_name(app.ctx.theme.id()),
        "scale": app.ctx.theme.scale(),
        "reducedMotion": app.ctx.theme.motion().reduced,
        "reducedMotionClass": app.window.window.has_css_class("reduced-motion"),
        "colorScheme": color_scheme(),
        "settingsApplied": app.follower.applied(),
        "settingsPending": app.follower.pending(),
        "keyedControls": crate::widgets::button::keyed(),
        "gtkAnimations": gtk::Settings::default().is_some_and(|s| s.is_gtk_enable_animations()),
        "fundamentalsFirst": crate::app::fundamentals_first(),
        "panelTransition": panel_transition(app),
        "rev": app.ctx.store.rev(),
        "panics": guard::panics(),
        "focusPath": focus_path(app),
        "focusBounds": focus_bounds(app),
        "focusType": gtk::prelude::GtkWindowExt::focus(&app.window.window)
            .map(|f| f.type_().name().to_owned()),
        "focusVisible": app.window.window.gets_focus_visible(),
        "focusInTopLayer": focus_in_top_layer(app),
        "pid": std::process::id(),
        "playerNs": app.ctx.media.position_ns(),
        "announcements": crate::window::announce::announced(),
    });
    if let (Some(summary), Value::Object(rest)) = (summary.as_object_mut(), rest) {
        summary.extend(rest);
    }
    summary
}

/// libadwaita's color scheme as set (`"force-light"`, `"force-dark"`, …)
/// and whether it draws dark (WP10: Swamp Day forces light, the others
/// dark).
fn color_scheme() -> Value {
    let manager = adw::StyleManager::default();
    let scheme = match manager.color_scheme() {
        adw::ColorScheme::ForceLight => "force-light",
        adw::ColorScheme::ForceDark => "force-dark",
        adw::ColorScheme::PreferLight => "prefer-light",
        adw::ColorScheme::PreferDark => "prefer-dark",
        _ => "default",
    };
    json!({"scheme": scheme, "dark": manager.is_dark()})
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

/// The focused widget's bounds in window coordinates (logical px; WP10's
/// focus ring probes); `None` without a focus.
fn focus_bounds(app: &App) -> Option<[f32; 4]> {
    let window = &app.window.window;
    let focus = gtk::prelude::GtkWindowExt::focus(window)?;
    let b = focus.compute_bounds(window)?;
    Some([b.x(), b.y(), b.width(), b.height()])
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
/// And (WP10) every widget the focus lands on that a screen reader isn't
/// told of, can't name (a text box by its label alone: its text is its
/// value) or can't say what it is; each item says what it `lacks`.
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
        let lacks = if (is_icon_only_button(w) || custom || region) && !has_label(w) {
            Some("label")
        } else if lands_focus(w) {
            // WP10: what the keyboard lands on reaches a screen reader (a
            // widget GTK skips belongs to an editable it shows instead),
            // named by a label or its own text (a button's, a row's; a text
            // box's text is its value, not its name), and says what it is.
            match exposed(w) {
                None => Some("role"),
                Some(shown)
                    if !has_label(&shown)
                        && (is_text_box(shown.accessible_role()) || !has_text(&shown)) =>
                {
                    Some("name")
                }
                Some(shown) if shown != *w && is_generic(shown.accessible_role()) => Some("role"),
                Some(_) => None,
            }
        } else {
            None
        };
        let lacks = lacks.or_else(|| {
            // WP10: and a custom widget says what it is.
            (custom && is_generic(w.accessible_role())).then_some("role")
        });
        if let Some(lacks) = lacks {
            let bounds = w
                .compute_bounds(&app.window.window)
                .map(|b| [b.x(), b.y(), b.width(), b.height()]);
            missing.push(json!({
                "type": type_name,
                "name": name,
                "bounds": bounds,
                "lacks": lacks,
            }));
        }
        true
    });
    json!({"ok": missing.is_empty(), "missing": missing})
}

/// A widget the keyboard focus lands on: focusable and sensitive, and not
/// a container that takes the focus only when nothing in it can (a
/// scrolled window, a list: its rows are what lands).
fn lands_focus(w: &gtk::Widget) -> bool {
    w.is_focusable()
        && w.is_sensitive()
        && !(w.is::<gtk::ScrolledWindow>()
            || w.is::<gtk::Viewport>()
            || w.is::<gtk::ListBox>()
            || w.is::<gtk::ListView>()
            || w.is::<gtk::GridView>())
}

/// What a screen reader is told of the focus on `w`: `w`, or, for a widget
/// GTK leaves out of the tree (`GtkText`'s role is `None`: an editable's
/// delegate), the editable it belongs to (an entry, a spin button, an
/// editable label); `None` when it belongs to none: a focus no screen
/// reader hears of.
fn exposed(w: &gtk::Widget) -> Option<gtk::Widget> {
    let skipped = matches!(
        w.accessible_role(),
        gtk::AccessibleRole::None | gtk::AccessibleRole::Presentation
    );
    if !skipped {
        return Some(w.clone());
    }
    let mut at = w.parent();
    for _ in 0..3 {
        let p = at?;
        if p.is::<gtk::Entry>()
            || p.is::<gtk::SpinButton>()
            || p.is::<gtk::SearchEntry>()
            || p.is::<gtk::PasswordEntry>()
            || p.is::<gtk::EditableLabel>()
        {
            return Some(p);
        }
        at = p.parent();
    }
    None
}

/// A role whose text is the value typed, which names nothing.
fn is_text_box(role: gtk::AccessibleRole) -> bool {
    matches!(
        role,
        gtk::AccessibleRole::TextBox
            | gtk::AccessibleRole::SearchBox
            | gtk::AccessibleRole::SpinButton
    )
}

/// A role that says nothing of what the widget is.
fn is_generic(role: gtk::AccessibleRole) -> bool {
    matches!(
        role,
        gtk::AccessibleRole::Generic
            | gtk::AccessibleRole::Widget
            | gtk::AccessibleRole::None
            | gtk::AccessibleRole::Presentation
    )
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

/// Where the symbolic icons are in the GResource (the icon theme's resource
/// path is its parent's parent: `app::ICONS`, G§5.2).
const ICON_RESOURCES: &str = "/com/rsensato/qrec/icons/scalable/actions";

/// The GResource's icons, by name (`qrec-<icon>-symbolic`).
pub fn resource_icons() -> Vec<String> {
    let mut names: Vec<String> =
        gtk::gio::resources_enumerate_children(ICON_RESOURCES, gtk::gio::ResourceLookupFlags::NONE)
            .map(|files| {
                files
                    .iter()
                    .filter_map(|f| f.strip_suffix(".svg").map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
    names.sort();
    names
}

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

/// Every icon of the GResource through the window's icon theme (G§5.2): it
/// resolves to its own resource file (not another theme's icon of that
/// name, nor `image-missing`), and drawn at 16 px with a probe color in
/// each of GTK's four symbolic slots (foreground magenta, success green,
/// warning yellow, error cyan), every pixel it covers takes the
/// foreground's: it is symbolic and recolors in full from the widget's CSS
/// `color` (a fill GTK doesn't recolor stays black, or its own color) and
/// no part of it takes a status color.
fn icons(app: &App) -> Value {
    let display = WidgetExt::display(&app.window.window);
    let icon_theme = gtk::IconTheme::for_display(&display);
    let probes = [
        gdk::RGBA::new(1.0, 0.0, 1.0, 1.0),
        gdk::RGBA::new(0.0, 1.0, 0.0, 1.0),
        gdk::RGBA::new(1.0, 1.0, 0.0, 1.0),
        gdk::RGBA::new(0.0, 1.0, 1.0, 1.0),
    ];
    let names = resource_icons();
    let mut all = !names.is_empty();
    let mut out = Vec::new();
    for name in &names {
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
        let file = paintable.file().map(|f| f.uri().to_string());
        let own = file.as_deref() == Some(&format!("resource://{ICON_RESOURCES}/{name}.svg"));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot_symbolic(&snapshot, 16.0, 16.0, &probes);
        let pixels = snapshot
            .to_node()
            .and_then(|node| render(app, &node, 16.0, 16.0))
            .unwrap_or_default();
        let (mut drawn, mut off_color) = (0, 0);
        for px in pixels.chunks_exact(4) {
            if px[3] > 128 {
                drawn += 1;
                // The magenta probe, unpremultiplied: full red and blue, no
                // green (black, an SVG's own fill or a status probe isn't).
                if !(px[0] > 200 && px[2] > 200 && px[1] < 40) {
                    off_color += 1;
                }
            }
        }
        // A chevron covers 8 px at 16 (its 1.3 px stroke).
        let recolored = drawn >= 4 && off_color == 0;
        all &= own && recolored;
        out.push(json!({
            "name": name,
            "found": true,
            "file": file,
            "pixels": drawn,
            "offColor": off_color,
            "recolored": recolored,
        }));
    }
    json!({"ok": all, "count": names.len(), "icons": out})
}

/// Each illustration through `gtk::Svg` in the theme's colors: its size and
/// how many pixels it draws.
///
/// With `compare` (a folder of the pen's PNGs: `design/export/art/`), each
/// is also drawn at 1× and 2× and compared with `<name>@<n>x.png` within
/// antialiasing ([`aa_excess`]), and so is the REC glyph
/// (`qrec-record-symbolic` through the icon theme, in `text-on-record`,
/// against `rec-glyph@<n>x.png`): `compared`.
fn art(app: &App, request: &Value) -> Value {
    let mut all = true;
    let mut out = Vec::new();
    let mut compared = Vec::new();
    let palette = app.ctx.theme.palette();
    let pens = request["compare"].as_str().map(PathBuf::from);
    for which in theme::art::Art::ALL {
        let Some(svg) = theme::art::paintable_checked(which, &palette) else {
            all = false;
            out.push(json!({"name": which.name(), "loaded": false}));
            continue;
        };
        let (w, h) = (svg.svg.intrinsic_width(), svg.svg.intrinsic_height());
        let draw = |scale: i32| {
            let snapshot = gtk::Snapshot::new();
            svg.svg
                .snapshot(&snapshot, f64::from(w * scale), f64::from(h * scale));
            snapshot
                .to_node()
                .and_then(|node| render(app, &node, (w * scale) as f32, (h * scale) as f32))
                .unwrap_or_default()
        };
        let pixels = draw(1);
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
        if let Some(dir) = &pens {
            for scale in [1, 2] {
                let pen = dir.join(format!("{}@{scale}x.png", which.name()));
                let result = compare_with(&draw(scale), &pen, (w * scale, h * scale));
                all &= result["ok"] == true;
                compared.push(result);
            }
        }
    }
    if let Some(dir) = &pens {
        let display = WidgetExt::display(&app.window.window);
        let icon_theme = gtk::IconTheme::for_display(&display);
        for scale in [1, 2] {
            let size = 16 * scale;
            let paintable = icon_theme.lookup_icon(
                "qrec-record-symbolic",
                &[],
                size,
                1,
                gtk::TextDirection::Ltr,
                gtk::IconLookupFlags::empty(),
            );
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot_symbolic(
                &snapshot,
                f64::from(size),
                f64::from(size),
                &[palette.text_on_record],
            );
            let pixels = snapshot
                .to_node()
                .and_then(|node| render(app, &node, size as f32, size as f32))
                .unwrap_or_default();
            let pen = dir.join(format!("rec-glyph@{scale}x.png"));
            let result = compare_with(&pixels, &pen, (size, size));
            all &= result["ok"] == true;
            compared.push(result);
        }
    }
    // What GtkSvg drops while it parses is counted (an unsupported element
    // here): the art's zero means something.
    let probe = theme::art::load("probe", PROBE_SVG).errors();
    all &= probe > 0;
    json!({"ok": all, "art": out, "probeErrors": probe, "compared": compared})
}

/// How far outside antialiasing a pixel of a render may be (levels), and
/// the share of its pixels allowed more than 8 levels off (`tests/art.rs`
/// holds rsvg's renders to the same).
const ART_MAX_EXCESS: f64 = 32.0;
const ART_MAX_OFF_SHARE: f64 = 0.01;

/// `ours` (RGBA, `w`×`h`) against the PNG `pen`: the worst excess over
/// antialiasing and the share over 8 levels, over Swamp Night's `bg` and
/// over white.
fn compare_with(ours: &[u8], pen: &std::path::Path, (w, h): (i32, i32)) -> Value {
    let name = pen.file_name().map(|n| n.to_string_lossy().into_owned());
    let texture = match gdk::Texture::from_filename(pen) {
        Ok(texture) => texture,
        Err(e) => return json!({"pen": name, "ok": false, "error": e.to_string()}),
    };
    if (texture.width(), texture.height()) != (w, h) || ours.len() != (w * h * 4) as usize {
        return json!({
            "pen": name,
            "ok": false,
            "error": format!("{}×{} for {w}×{h}", texture.width(), texture.height()),
        });
    }
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let row = (w * 4) as usize;
    let theirs: Vec<u8> = bytes
        .chunks(stride)
        .take(h as usize)
        .flat_map(|line| line[..row].to_vec())
        .collect();
    let mut ok = true;
    let mut grounds = Vec::new();
    for ground in [[18.0, 23.0, 15.0], [255.0, 255.0, 255.0]] {
        let (worst, share) = aa_excess(ours, &theirs, (w as usize, h as usize), ground);
        ok &= worst <= ART_MAX_EXCESS && share <= ART_MAX_OFF_SHARE;
        grounds.push(json!({"ground": ground, "worst": worst, "share": share}));
    }
    json!({"pen": name, "ok": ok, "grounds": grounds})
}

/// The worst excess over antialiasing of `ours` against `pen` (RGBA, both
/// over `ground`): each pixel against the range of `pen`'s 3×3 window
/// around it; and the share of pixels more than 8 levels off.
fn aa_excess(ours: &[u8], pen: &[u8], (w, h): (usize, usize), ground: [f64; 3]) -> (f64, f64) {
    let at = |data: &[u8], x: usize, y: usize| {
        let px = &data[(y * w + x) * 4..][..4];
        let a = f64::from(px[3]) / 255.0;
        [0, 1, 2].map(|k| f64::from(px[k]) * a + ground[k] * (1.0 - a))
    };
    let (mut worst, mut off) = (0.0f64, 0usize);
    for y in 0..h {
        for x in 0..w {
            let ours = at(ours, x, y);
            let (mut lo, mut hi) = ([255.0f64; 3], [0.0f64; 3]);
            for yy in y.saturating_sub(1)..(y + 2).min(h) {
                for xx in x.saturating_sub(1)..(x + 2).min(w) {
                    let p = at(pen, xx, yy);
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
            }
            let d = (0..3)
                .map(|k| (lo[k] - ours[k]).max(ours[k] - hi[k]).max(0.0))
                .fold(0.0, f64::max);
            worst = worst.max(d);
            if d > 8.0 {
                off += 1;
            }
        }
    }
    (worst, off as f64 / (w * h).max(1) as f64)
}

/// An SVG with an element GtkSvg doesn't support (`<foreignObject>`).
const PROBE_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\">\
    <foreignObject width=\"8\" height=\"8\"/><rect width=\"8\" height=\"8\" fill=\"black\"/></svg>";

/// `fonts`' `measure`: each text laid out in the window's Pango context
/// (GTK's font rendering as set up), in the family, absolute size (px) and
/// weight asked (JetBrains Mono, 12, 400 by default): its logical and ink
/// widths in px.
fn measure_texts(app: &App, request: &Value) -> Vec<Value> {
    let Some(items) = request["measure"].as_array() else {
        return Vec::new();
    };
    let px = |units: i32| f64::from(units) / f64::from(gtk::pango::SCALE);
    items
        .iter()
        .map(|item| {
            let text = item["text"].as_str().unwrap_or_default();
            let layout = app.window.window.create_pango_layout(Some(text));
            let mut desc = gtk::pango::FontDescription::new();
            desc.set_family(item["family"].as_str().unwrap_or(theme::fonts::FAMILIES[2]));
            let size = item["sizePx"].as_f64().unwrap_or(12.0);
            desc.set_absolute_size(size * f64::from(gtk::pango::SCALE));
            desc.set_weight(match item["weight"].as_u64().unwrap_or(400) {
                500 => gtk::pango::Weight::Medium,
                600 => gtk::pango::Weight::Semibold,
                700 => gtk::pango::Weight::Bold,
                _ => gtk::pango::Weight::Normal,
            });
            layout.set_font_description(Some(&desc));
            let (ink, logical) = layout.extents();
            json!({
                "text": text,
                "sizePx": size,
                "logical": px(logical.width()),
                "ink": px(ink.width()),
            })
        })
        .collect()
}

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
        // GTK's own animations stay on only when asked.
        assert!(!on.animations);
        let animated = TestMode::parse(Some("1,animations"), None, None);
        assert!(animated.on && animated.animations && !animated.stuck_gst);
        // The system's motion settings count only when asked.
        assert!(!animated.system_motion);
        let system = TestMode::parse(Some("1,animations,system-motion"), None, None);
        assert!(system.animations && system.system_motion);
        let vars = TestVars {
            panic_once: Some("timeline: a frame| meter: refresh |"),
            send_delay_ms: Some("225"),
            ..TestVars::default()
        };
        let faults = TestMode::parse_all(Some("1"), vars);
        assert_eq!(faults.panic_once, ["timeline: a frame", "meter: refresh"]);
        assert_eq!(faults.send_delay, Some(Duration::from_millis(225)));
        assert!(!faults.panic_activate && on.panic_once.is_empty());
        // Outside test mode, none of it.
        let off = TestMode::parse_all(None, vars);
        assert!(off.panic_once.is_empty() && off.send_delay.is_none());
        let zero = TestVars {
            send_delay_ms: Some("0"),
            ..TestVars::default()
        };
        assert_eq!(TestMode::parse_all(Some("1"), zero).send_delay, None);
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
