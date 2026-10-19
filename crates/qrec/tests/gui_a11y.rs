//! WP10's accessibility cases (docs/GUI.md G§7 WP10, G§8.4; PRODUCT
//! Accessibility; BRIEF §6), in a nested headless sway on the design
//! fixture:
//!
//! - **Announcements** (`gtk::Accessible::announce`): a take starting to
//!   record, the take landing, the take's other ends and the export's end,
//!   in the toasts' words, polite, assertive for a failure. The window
//!   keeps what it announced for the test channel (`local`'s
//!   `announcements`: message, priority, and whether GTK had a context
//!   that announces) and logs it (`announce (polite): …`). A toast pushed
//!   again (the toast host makes it one) is one announcement.
//! - **Focus order** (BRIEF §6 by D-10): from the window's start, `F6`
//!   goes header → transport → timeline toolbar → timeline → panel and
//!   around, `Shift+F6` back; GTK's own chain, walked with `Shift+Tab`
//!   (`Tab` switches Live and Program in the main window, D-10), visits
//!   every focusable widget of S02 and S06 region by region in the reverse
//!   of that order, the stage canvas between the header and the transport,
//!   and never the hint bar. Each widget it lands on draws the 2 px focus
//!   ring (a shot while the key's keyboard holds the window active: G§8.4).
//! - The a11y walk stays empty in each of these states, and while a
//!   layer's and a track's names are renamed (the field a named text box).
//!
//! `cargo test -p qrec --test gui_a11y -- --ignored --test-threads=1`
//! (inside the dev shell). Shots and logs go to `tmp/gui-shots/a11y/`.

mod gui_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use gui_support::{
    CLI_TIMEOUT, Gui, Launch, Nested, Options, poll, reap_within, rgb_pixels, token_rgb,
    wait_for_gui_host,
};
use qrec_core::state::CanvasMode;
use qrec_core::{ClipId, LayerId, UiState};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use serde_json::{Value, json};

const OPEN: Duration = Duration::from_secs(60);
const EXIT: Duration = Duration::from_secs(20);
const AREA: &str = "a11y";

// ---------------------------------------------------------------- helpers

/// `design_fixture(variant)` in a folder named `rust-traits-talk`, with
/// `ui` as its `ui.json`.
fn fixture(nested: &Nested, variant: DesignVariant, ui: &UiState) -> PathBuf {
    let dir = nested.env.empty_dir("rust-traits-talk");
    design_fixture(&dir, variant);
    write_ui(&dir, ui);
    dir
}

/// The window on `project`, open and idle.
fn open(nested: &mut Nested, project: &Path, launch: Launch) {
    nested.launch(project, launch);
    wait_for_gui_host(nested, project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
}

/// S02's preset: Live, the playhead at the end.
fn s02_ui() -> UiState {
    UiState {
        canvas_mode: CanvasMode::Live,
        playhead: 7391,
        ..UiState::default()
    }
}

/// S06's preset: Program, c2d0 selected with the playhead in it, Face
/// selected (the frames draw its inspector).
fn s06_ui() -> UiState {
    UiState {
        canvas_mode: CanvasMode::Program,
        playhead: 2437,
        selection: vec![ClipId::from_raw("c2d0")],
        selected_layer: Some(LayerId::numbered(2)),
        ..UiState::default()
    }
}

/// The Smoke fixture's: Program, its middle clip selected (gui_bin_export's
/// S16 preset).
fn smoke_ui() -> UiState {
    UiState {
        canvas_mode: CanvasMode::Program,
        selection: vec![ClipId::from_raw("c1b2")],
        playhead: 45,
        ..UiState::default()
    }
}

/// BRIEF's meter readings, pinned (G§8.4 `QREC_GUI_TEST_LEVELS`): the
/// focus walks compare shots, and the fake tone's ticking meters would
/// differ between them.
const DESIGN_LEVELS: &str = "alsa_input.fake=-22/-16,alsa_output.fake.monitor=-52/-48";

/// The focus walks' launch: the a11y walk on, the meters pinned.
fn still() -> Launch {
    Launch::a11y().with_env("QREC_GUI_TEST_LEVELS", DESIGN_LEVELS)
}

/// The objective checks of a case: no panic caught, no GLib or GTK
/// critical, the a11y walk empty (launched with `Launch::a11y()`).
fn assert_clean(gui: &Gui) {
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    gui.assert_no_criticals();
    let a11y = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "a11y: {a11y}");
}

fn close(nested: &mut Nested) {
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// `qrec <args> --json` succeeded.
fn cli_ok(nested: &Nested, project: &Path, args: &[&str]) -> Value {
    let reply = nested.cli_json(project, args);
    assert_eq!(reply["ok"], true, "qrec {args:?}: {reply}");
    reply
}

/// The record phase (`qrec status`).
fn phase(nested: &Nested, project: &Path) -> String {
    nested.cli_json(project, &["status"])["data"]["runtime"]["record"]["phase"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn wait_phase(nested: &Nested, project: &Path, want: &str) {
    poll(Duration::from_secs(30), &format!("phase {want}"), || {
        (phase(nested, project) == want).then_some(())
    });
}

// ---------------------------------------------------------- announcements

/// One announcement the window made.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Spoken {
    message: String,
    priority: String,
    /// GTK had a context that announces (not the test one, G§8.4's
    /// `GTK_A11Y=test`).
    sent: bool,
}

fn announcements(gui: &Gui) -> Vec<Spoken> {
    gui.local()["announcements"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| Spoken {
            message: a["message"].as_str().unwrap_or_default().to_owned(),
            priority: a["priority"].as_str().unwrap_or_default().to_owned(),
            sent: a["sent"].as_bool().unwrap_or(true),
        })
        .collect()
}

/// Waits (30 s) until the window has announced `count` messages `like`
/// accepts, and returns them.
fn wait_announced(gui: &Gui, what: &str, count: usize, like: impl Fn(&str) -> bool) -> Vec<Spoken> {
    let found = poll(Duration::from_secs(30), what, || {
        let found: Vec<Spoken> = announcements(gui)
            .into_iter()
            .filter(|a| like(&a.message))
            .collect();
        (found.len() >= count).then_some(found)
    });
    assert_eq!(found.len(), count, "{what}: {found:#?}");
    found
}

/// The GUI log's announcement lines (`announce (polite): …`).
fn logged_announcements(gui: &Gui) -> Vec<String> {
    gui.gui_log()
        .lines()
        .filter_map(|l| l.split_once(" announce (").map(|(_, rest)| rest.to_owned()))
        .collect()
}

/// PRODUCT Accessibility: a take starting to record and landing on the
/// timeline is announced whoever starts and stops it (`O` and `R` here,
/// the CLI after), in the ghost's and the stop reply's words; a cancelled
/// take in its toast's. Polite, and not sent to GTK under the a11y walk's
/// test context (which has no `announce`: the call would crash the
/// window).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_record_loop_is_announced() {
    let mut nested = Nested::start(AREA, "record-loop", Options::default());
    let ui = UiState {
        selection: vec![ClipId::from_raw("c2d0")],
        ..s02_ui()
    };
    let project = fixture(&nested, DesignVariant::Base, &ui);
    open(&mut nested, &project, Launch::a11y());
    // Record over c2d0 with O, stop with R.
    nested.keys(&["-k", "o"]);
    wait_phase(&nested, &project, "recording");
    let started = wait_announced(nested.window(0), "take 10's start", 1, |m| {
        m.starts_with("Recording take 10")
    });
    assert_eq!(
        started[0].message, "Recording take 10 over c2d0 (take 5)",
        "the ghost's words"
    );
    // Let it run past the 10 frames a take needs.
    thread::sleep(Duration::from_millis(1500));
    nested.keys(&["-k", "r"]);
    wait_phase(&nested, &project, "idle");
    let landed = wait_announced(nested.window(0), "take 10 landing", 1, |m| {
        m.starts_with("Recorded take 10 (")
    });
    assert!(
        landed[0].message.ends_with(" s) over c2d0"),
        "the stop's reply's words: {landed:?}"
    );
    // The CLI's take: started, then cancelled (its toast's words).
    cli_ok(&nested, &project, &["record", "start"]);
    wait_phase(&nested, &project, "recording");
    wait_announced(nested.window(0), "take 11's start", 1, |m| {
        m == "Recording take 11"
    });
    cli_ok(&nested, &project, &["record", "cancel"]);
    wait_phase(&nested, &project, "idle");
    wait_announced(nested.window(0), "take 11 cancelled", 1, |m| {
        m == "Take 11 cancelled. It is in the bin; nothing was added to the timeline."
    });
    let gui = nested.window(0);
    gui.wait_idle();
    let all = announcements(gui);
    let record: Vec<&Spoken> = all
        .iter()
        .filter(|a| {
            ["Recording take ", "Recorded take ", "Take 11 cancelled"]
                .iter()
                .any(|p| a.message.starts_with(p))
        })
        .collect();
    assert_eq!(record.len(), 4, "once each: {all:#?}");
    for spoken in &record {
        assert_eq!(spoken.priority, "polite", "{spoken:?}");
        assert!(
            !spoken.sent,
            "the test context announces nothing: {spoken:?}"
        );
    }
    let logged = logged_announcements(gui);
    assert!(
        logged
            .iter()
            .any(|l| l.ends_with("Recording take 10 over c2d0 (take 5)")),
        "logged: {logged:#?}"
    );
    assert!(gui.running(), "the window is still up");
    assert_clean(gui);
    close(&mut nested);
}

/// The export's end is announced in its toast's words whether the toast
/// shows (a CLI export) or the export panel's Done page drops it (the
/// panel's own export): "Layers! Like an onion. Your video is ready.",
/// without the path.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_exports_end_is_announced() {
    let mut nested = Nested::start(AREA, "export-done", Options::default());
    let project = fixture(&nested, DesignVariant::Smoke, &smoke_ui());
    open(&mut nested, &project, Launch::a11y());
    const DONE: &str = "Layers! Like an onion. Your video is ready.";
    let export = nested.cli_background(&project, &["export", "--quality", "draft"]);
    let status = reap_within(export, CLI_TIMEOUT, "qrec export");
    assert!(status.success(), "qrec export: {status}");
    wait_announced(nested.window(0), "the CLI export's end", 1, |m| m == DONE);
    // The panel's own export: its Done page shows the end and drops the
    // core's toast; a screen reader still hears it.
    nested.keys(&["-M", "ctrl", "-k", "e", "-m", "ctrl"]);
    nested.window(0).wait_idle();
    let start = nested.window(0).regions(&["export-start"]);
    let [x, y, w, h] = *start
        .get("export-start")
        .unwrap_or_else(|| panic!("no Export button: {start:?}"));
    nested.pointer().click(x + w / 2.0, y + h / 2.0);
    poll(Duration::from_secs(120), "the Done page", || {
        let local = nested.window(0).local();
        (local["overlays"] == json!(["ExportPanel(Done)"])).then_some(())
    });
    let spoken = wait_announced(nested.window(0), "the panel's export end", 2, |m| m == DONE);
    for s in &spoken {
        assert_eq!(s.priority, "polite", "{s:?}");
    }
    let gui = nested.window(0);
    gui.wait_idle();
    assert_clean(gui);
    nested.keys(&["-k", "Escape"]);
    close(&mut nested);
}

/// The same toast thirty times (the toast host makes it one toast) is one
/// announcement: a screen reader isn't flooded by a held key or a button
/// pressed again; a new toast after it is announced.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_repeated_toast_is_announced_once() {
    let mut nested = Nested::start(AREA, "repeated-toast", Options::default());
    let project = fixture(&nested, DesignVariant::Base, &s02_ui());
    open(&mut nested, &project, Launch::a11y());
    let gui = nested.window(0);
    // A key that can't run (D-34), pressed 30 times through the channel
    // (WP9 built `keys.help`, whose "Not built yet" toast this was).
    for _ in 0..30 {
        let reply = gui.ctl(json!({"op": "action", "id": "history.redo"}));
        assert_eq!(reply["ok"], true, "{reply}");
    }
    gui.wait_idle();
    let reply = gui.ctl(json!({"op": "dispatch", "command": {"cmd": "undo"}}));
    assert_eq!(reply["ok"], true, "{reply}");
    let after = wait_announced(gui, "the undo's toast", 1, |m| {
        m.starts_with("Nothing to undo")
    });
    assert_eq!(after[0].priority, "polite", "a warning: {after:?}");
    // Its body's CLI-style backticks (`qrec history`) are shown, not said.
    let shown = gui.gui_log();
    let toast = shown
        .lines()
        .find(|l| l.contains("toast: Nothing to undo"))
        .unwrap_or_default();
    assert!(toast.contains("`qrec history`"), "the toast: {toast}");
    assert!(
        after[0].message.contains("qrec history") && !after[0].message.contains('`'),
        "said: {after:?}"
    );
    let spoken = announcements(gui);
    let repeated = spoken
        .iter()
        .filter(|s| s.message.starts_with("Nothing to redo"))
        .count();
    assert_eq!(repeated, 1, "announced once: {spoken:#?}");
    assert_clean(gui);
    close(&mut nested);
}

/// A rename's field (a layer's, a track's: the overflow's Rename) reaches
/// a screen reader while it is edited as a text box named for what it
/// renames ("Name of layer Face"): GTK gives an editable label no role and
/// leaves its text out of the tree, and a text box's text is its value.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_rename_field_is_a_named_text_box() {
    let mut nested = Nested::start(AREA, "rename-field", Options::default());
    let project = fixture(&nested, DesignVariant::Base, &s02_ui());
    open(&mut nested, &project, still());
    // The overflow, the row, the field's key in `entries` (its nearest
    // named widget).
    for (menu, row, field) in [
        ("layer-menu-L2", "layer-row-L2", "layer-row-L2"),
        ("audio-menu-A1", "audio-row-A1", "audio-name-A1"),
    ] {
        let found = nested.window(0).regions(&[menu]);
        let [x, y, w, h] = *found
            .get(menu)
            .unwrap_or_else(|| panic!("no {menu}: {found:?}"));
        nested.pointer().click(x + w / 2.0, y + h / 2.0);
        nested.window(0).wait_idle();
        // One `wtype` run from Rename (the menu's first item) to Esc: the
        // window deactivates between runs, which ends an edit.
        let held = nested.keys_held(&["-k", "Return", "-s", "2500", "-k", "Escape"], 500);
        poll(Duration::from_secs(10), "the name as a field", || {
            nested
                .window(0)
                .entries(row)
                .contains_key(field)
                .then_some(())
        });
        nested.window(0).wait_idle();
        nested.screenshot(&format!("{row}-renaming"));
        let a11y = nested.window(0).ctl(json!({"op": "a11y"}));
        assert_eq!(a11y["ok"], true, "a11y while renaming {row}: {a11y}");
        held.wait();
        nested.window(0).wait_idle();
        assert!(
            !nested.window(0).entries(row).contains_key(field),
            "Esc ends {row}'s rename"
        );
    }
    assert_clean(nested.window(0));
    close(&mut nested);
}

/// A `PATH` whose `ffmpeg` fails every render unit (`-progress pipe:1`) at
/// once, and is the real one for anything else (probes, thumbnails).
fn failing_ffmpeg(root: &Path) -> std::ffi::OsString {
    let bin = root.join("failing-ffmpeg");
    fs::create_dir_all(&bin).expect("the wrapper's folder");
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("ffmpeg"))
        .find(|p| p.is_file())
        .expect("ffmpeg on PATH");
    let script = format!(
        "#!/bin/sh\n\
         case \" $* \" in\n  \
         *' -progress '*) echo 'Error opening output files: Input/output error' >&2; exit 1 ;;\n\
         esac\n\
         exec '{}' \"$@\"\n",
        real.display()
    );
    let wrapper = bin.join("ffmpeg");
    fs::write(&wrapper, script).expect("the wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("chmod the wrapper");
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("a PATH")
}

/// A failed export is announced assertively: "Export failed." and the
/// first line of why, not the stderr tail.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_failed_export_is_announced_assertively() {
    let mut nested = Nested::start(AREA, "export-failed", Options::default());
    let project = fixture(&nested, DesignVariant::Smoke, &smoke_ui());
    let launch = Launch {
        path: Some(failing_ffmpeg(nested.env.root())),
        ..Launch::a11y()
    };
    open(&mut nested, &project, launch);
    let export = nested.cli_background(&project, &["export", "--quality", "draft"]);
    let status = reap_within(export, CLI_TIMEOUT, "qrec export");
    assert!(!status.success(), "the export fails: {status}");
    let spoken = wait_announced(nested.window(0), "the failure", 1, |m| {
        m.starts_with("Export failed. ")
    });
    assert_eq!(spoken[0].priority, "assertive", "{spoken:?}");
    assert!(
        !spoken[0].message.contains('\n'),
        "one line: {:?}",
        spoken[0].message
    );
    let gui = nested.window(0);
    gui.wait_idle();
    assert_clean(gui);
    close(&mut nested);
}

/// A stand-in AT-SPI registry on the nested bus, and an ear for what the
/// window announces there: GTK's AT-SPI backend embeds its root with the
/// registry (`org.a11y.atspi.Socket.Embed`; with no registry it prints a
/// critical) and asks which events are listened to; `announce` emits
/// `org.a11y.atspi.Event.Object.Announcement` (the message, and the
/// politeness: 1 polite, 2 assertive), which a screen reader hears. GIO's
/// D-Bus (the workspace's gtk), on a thread and main context of its own.
mod atspi {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use gtk::glib::variant::{ObjectPath, ToVariant};
    use gtk::{gio, glib};

    const REGISTRY: &str = "org.a11y.atspi.Registry";
    const ROOT: &str = "/org/a11y/atspi/accessible/root";
    const REGISTRY_PATH: &str = "/org/a11y/atspi/registry";
    /// What the stand-in serves: `Embed` at the root, the registry's event
    /// list at the registry.
    const NODE: &str = r#"<node>
  <interface name="org.a11y.atspi.Socket">
    <method name="Embed">
      <arg name="plug" type="(so)" direction="in"/>
      <arg name="socket" type="(so)" direction="out"/>
    </method>
  </interface>
  <interface name="org.a11y.atspi.Registry">
    <method name="GetRegisteredEvents">
      <arg name="events" type="a(ss)" direction="out"/>
    </method>
  </interface>
</node>"#;
    /// How long a call to the bus may take.
    const CALL_MS: i32 = 5000;

    /// What was announced on the bus: the politeness and the message.
    pub type Heard = Arc<Mutex<Vec<(i32, String)>>>;

    /// The stand-in, up: its connection, its ear, and whether the bus went.
    type Up = (
        gio::DBusConnection,
        gio::SignalSubscription,
        Arc<AtomicBool>,
    );

    /// Serves the registry on the bus at `address` and records the
    /// announcements until the bus goes (the case's end).
    pub fn serve(address: &str) -> Heard {
        let heard: Heard = Arc::default();
        let (ready_tx, ready) = mpsc::channel();
        let (address, log) = (address.to_owned(), Arc::clone(&heard));
        thread::spawn(move || {
            // The calls and the signals come on the main context the
            // objects were registered under: this thread's own.
            let context = glib::MainContext::new();
            let ran = context.with_thread_default(|| {
                let (_conn, _ear, gone) = match start(&address, log) {
                    Ok(up) => {
                        let _ = ready_tx.send(Ok(()));
                        up
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                // Until the bus goes with the case.
                while !gone.load(Ordering::SeqCst) {
                    context.iteration(true);
                }
            });
            if let Err(e) = ran {
                eprintln!("atspi: no main context: {e}");
            }
        });
        match ready.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => heard,
            Ok(Err(e)) => panic!("the stand-in AT-SPI registry: {e}"),
            Err(e) => panic!("the stand-in AT-SPI registry never started: {e}"),
        }
    }

    /// Connects, serves the two objects, listens to the announcements and
    /// takes the registry's name, last: GTK finds everything served.
    fn start(address: &str, log: Heard) -> Result<Up, glib::Error> {
        let conn = gio::DBusConnection::for_address_sync(
            address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            gio::Cancellable::NONE,
        )?;
        let gone = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&gone);
        conn.connect_closed(move |_, _, _| flag.store(true, Ordering::SeqCst));
        let node = gio::DBusNodeInfo::for_xml(NODE)?;
        for (path, name) in [(ROOT, "org.a11y.atspi.Socket"), (REGISTRY_PATH, REGISTRY)] {
            let info = node.lookup_interface(name).expect("the interface in NODE");
            conn.register_object(path, &info)
                .method_call(|_, _, _, _, method, _, invocation| match method {
                    // The application's root is embedded in the desktop:
                    // here (one out argument, the reference: `((so))`).
                    "Embed" => {
                        let root = ObjectPath::try_from(ROOT).expect("a path");
                        let socket = ((REGISTRY.to_owned(), root),).to_variant();
                        invocation.return_value(Some(&socket));
                    }
                    // No screen reader listens to events (announcements
                    // go out anyway).
                    "GetRegisteredEvents" => invocation
                        .return_value(Some(&(Vec::<(String, String)>::new(),).to_variant())),
                    other => invocation.return_dbus_error(
                        "org.freedesktop.DBus.Error.UnknownMethod",
                        &format!("the stand-in has no {other}"),
                    ),
                })
                .build()?;
        }
        let ear = conn.subscribe_to_signal(
            None,
            Some("org.a11y.atspi.Event.Object"),
            Some("Announcement"),
            None,
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                // `(siiva{sv})`: the event's kind, the politeness, 0, the
                // message, no properties.
                let body = signal.parameters;
                let live = body.try_child_value(1).and_then(|v| v.get::<i32>());
                let text = body
                    .try_child_value(3)
                    .and_then(|v| v.as_variant())
                    .and_then(|v| v.str().map(str::to_owned));
                match (live, text) {
                    (Some(live), Some(text)) => log.lock().expect("the log").push((live, text)),
                    _ => eprintln!("atspi: an announcement that isn't one: {body}"),
                }
            },
        );
        let reply = conn.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(REGISTRY, 0u32).to_variant()),
            Some(glib::VariantTy::new("(u)").expect("a type")),
            gio::DBusCallFlags::NONE,
            CALL_MS,
            gio::Cancellable::NONE,
        )?;
        // 1: the name's primary owner.
        if reply.child_value(0).get::<u32>() != Some(1) {
            let why = format!("the registry's name isn't ours: {reply}");
            return Err(glib::Error::new(gio::IOErrorEnum::Failed, &why));
        }
        Ok((conn, ear, gone))
    }
}

/// With a screen reader's bus (GTK's AT-SPI backend on the nested bus and
/// the stand-in registry), each announcement reaches it as GTK's
/// `Announcement` event, polite or assertive: a take's start and its
/// landing (polite), and a failed export (assertive). G§8.4's other cases
/// run GTK's test context, which has no `announce`: there the window only
/// logs them.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn announcements_reach_the_accessibility_bus() {
    let mut nested = Nested::start(AREA, "atspi", Options::default());
    let project = fixture(&nested, DesignVariant::Smoke, &smoke_ui());
    let bus = format!(
        "unix:path={}",
        nested.env.xdg_runtime_dir().join("bus").display()
    );
    let heard = atspi::serve(&bus);
    let launch = Launch {
        path: Some(failing_ffmpeg(nested.env.root())),
        ..Launch::default()
    }
    .with_env("GTK_A11Y", "atspi")
    .with_env("AT_SPI_BUS_ADDRESS", &bus);
    open(&mut nested, &project, launch);
    cli_ok(&nested, &project, &["record", "start"]);
    wait_phase(&nested, &project, "recording");
    thread::sleep(Duration::from_millis(1500));
    cli_ok(&nested, &project, &["record", "stop"]);
    wait_phase(&nested, &project, "idle");
    let export = nested.cli_background(&project, &["export", "--quality", "draft"]);
    let status = reap_within(export, CLI_TIMEOUT, "qrec export");
    assert!(!status.success(), "the export fails: {status}");
    let heard_by = |live: i32, like: &dyn Fn(&str) -> bool| {
        poll(
            Duration::from_secs(30),
            "an announcement on the bus",
            || {
                let heard = heard.lock().expect("the log").clone();
                heard
                    .iter()
                    .any(|(l, m)| *l == live && like(m))
                    .then_some(heard)
            },
        )
    };
    heard_by(1, &|m| m == "Recording take 10");
    heard_by(1, &|m| {
        m.starts_with("Recorded take 10 (") && m.ends_with(" s)")
    });
    let all = heard_by(2, &|m| m.starts_with("Export failed. "));
    let gui = nested.window(0);
    gui.wait_idle();
    let spoken = announcements(gui);
    assert!(
        spoken.iter().all(|s| s.sent),
        "every announcement went to GTK: {spoken:#?}"
    );
    let said: Vec<&str> = spoken.iter().map(|s| s.message.as_str()).collect();
    let on_bus: Vec<&str> = all.iter().map(|(_, m)| m.as_str()).collect();
    assert_eq!(on_bus, said, "the bus heard each once, in order");
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    gui.assert_no_criticals();
    close(&mut nested);
}

// ------------------------------------------------------------ focus order

/// The focus region a `focusPath` is in: the nearest region root or named
/// part that takes focus (`router::region_for_name`), the hint bar's too.
fn region_of(path: &[String]) -> &'static str {
    for name in path {
        let region = match name.as_str() {
            "region-header" => "header",
            "region-stage" => "stage",
            "transport" => "transport",
            "timeline-toolbar" => "timelineToolbar",
            "region-timeline" => "timeline",
            "region-panel" => "panel",
            "region-bin" => "bin",
            "region-hintbar" => "hintbar",
            "toast-host" => "toast",
            _ => continue,
        };
        // The stage's own part: `preview` holds the stage canvas.
        return region;
    }
    "other"
}

/// BRIEF §6's order (D-10's cycle, with the stage canvas between the
/// header and the transport).
const ORDER: [&str; 6] = [
    "header",
    "stage",
    "transport",
    "timelineToolbar",
    "timeline",
    "panel",
];

fn rank(region: &str) -> usize {
    ORDER
        .iter()
        .position(|r| *r == region)
        .unwrap_or_else(|| panic!("{region} isn't in the main window's order"))
}

/// Where the focus is: its path (the focused widget first), its bounds,
/// its widget type.
#[derive(Debug, Clone, PartialEq)]
struct Focus {
    path: Vec<String>,
    bounds: [f64; 4],
    kind: String,
}

fn focus(gui: &Gui) -> Option<Focus> {
    let local = gui.local();
    let path: Vec<String> = local["focusPath"]
        .as_array()?
        .iter()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .collect();
    let b = local["focusBounds"].as_array()?;
    let bounds = [0, 1, 2, 3].map(|i| b.get(i).and_then(Value::as_f64).unwrap_or(-1.0));
    let kind = local["focusType"].as_str().unwrap_or_default().to_owned();
    Some(Focus { path, bounds, kind })
}

/// One step of a keyboard walk: the focus it landed on and the shot taken
/// while the key's keyboard held the window active (its ring drawn).
struct Step {
    focus: Focus,
    shot: PathBuf,
}

/// Presses `keys` (one `wtype` run that holds its keyboard), waits for the
/// focus to move, and shoots while the window is active; then ends the
/// run (its keys were typed, only its hold is left). The run types once
/// the window has the seat's keyboard (`Nested::hold_keyboard`, G§8.4):
/// a busy window no longer misses a step's key, so none is typed twice.
fn step(nested: &mut Nested, keys: &[&str], name: &str) -> Step {
    let before = focus(nested.window(0));
    let held = nested.keys_held(keys, 5000);
    let focus = poll(
        Duration::from_secs(10),
        &format!("{keys:?} moving the focus"),
        || focus(nested.window(0)).filter(|f| Some(f) != before.as_ref()),
    );
    nested.window(0).wait_idle();
    let shot = nested.screenshot(name);
    drop(held);
    Step { focus, shot }
}

/// The focus-ring color pixels of `shot` in `rect` (the shot's device px)
/// that neither `others` shot has there: the ring drawn on the step's
/// widget only.
fn ring_pixels(shot: &Path, others: &[&Path], rect: [u32; 4], ring: [u8; 3]) -> usize {
    let near = |p: &[u8; 3]| p.iter().zip(ring).all(|(a, b)| a.abs_diff(b) <= 20);
    let mine = rgb_pixels(shot, rect);
    let theirs: Vec<Vec<[u8; 3]>> = others.iter().map(|o| rgb_pixels(o, rect)).collect();
    (0..mine.len())
        .filter(|&i| near(&mine[i]) && theirs.iter().all(|t| !near(&t[i])))
        .count()
}

/// `bounds` (logical px) grown by 6 px (the ring is 2 px wide, 2 px
/// outside; rows and the canvas draw it inside), clamped to the window, in
/// the shot's device px (× the output scale: WP10's scale runs).
fn ring_rect(opts: &Options, [x, y, w, h]: [f64; 4]) -> [u32; 4] {
    let (width, height) = opts.logical();
    let s = opts.scale;
    let x0 = ((x - 6.0).max(0.0) * s).floor();
    let y0 = ((y - 6.0).max(0.0) * s).floor();
    let x1 = ((x + w + 6.0).min(width) * s).ceil();
    let y1 = ((y + h + 6.0).min(height) * s).ceil();
    [x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32]
}

/// How many ring pixels a focused widget of `kind` and `bounds` shows at
/// least: half of a 2 px ring around its bounds (rows and the canvas draw
/// it inside, a field around its box), but a scale rings only its knob
/// (common.css): a small circle's worth there. A `stroked` widget (a
/// selected row) draws a 1 px accent stroke, the ring's color, in every
/// shot: under the ring's inner pixel, where the steps around it have it
/// too; half of the other pixel's worth there. At an output scale the ring
/// is `scale` times as wide and as long; the count wanted grows by the
/// scale once (at 1.5 the compositor blends a ring's edges, which then
/// read as neither color: the ring's area is the scale squared, its edges
/// a pixel less).
fn ring_wanted(kind: &str, [_, _, w, h]: [f64; 4], stroked: bool, scale: f64) -> usize {
    if kind == "GtkScale" {
        return (40.0 * scale) as usize;
    }
    // 2 px × the perimeter, halved.
    let half = (2.0 * (w + h)).clamp(40.0, 2000.0) * scale;
    if stroked {
        (half / 2.0) as usize
    } else {
        half as usize
    }
}

/// The rows a case's `ui` selects, which wear the accent stroke (C09's
/// selected state): the selected layer's.
fn stroked_rows(ui: &UiState) -> Vec<String> {
    ui.selected_layer
        .iter()
        .map(|layer| format!("layer-row-{layer}"))
        .collect()
}

/// Each step's widget draws its focus ring: enough ring-colored pixels
/// around it that the steps before and after don't have (the ring moved
/// with the focus). `baseline` is a shot with no ring at all. Each step's
/// measure goes to `<case>-rings.txt`; `stroked`, the widgets named so
/// draw the ring over a stroke of its color ([`ring_wanted`]).
fn assert_rings(nested: &Nested, steps: &[Step], baseline: &Path, stroked: &[String]) {
    let ring = token_rgb("swamp-night", "focus-ring");
    let mut missing = Vec::new();
    let mut report = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let prev = if i == 0 { baseline } else { &steps[i - 1].shot };
        let next = steps.get(i + 1).map_or(baseline, |s| &s.shot);
        let on_stroke = step
            .focus
            .path
            .first()
            .is_some_and(|name| stroked.contains(name));
        let want = ring_wanted(
            &step.focus.kind,
            step.focus.bounds,
            on_stroke,
            nested.opts.scale,
        );
        let got = ring_pixels(
            &step.shot,
            &[prev, next],
            ring_rect(&nested.opts, step.focus.bounds),
            ring,
        );
        let line = format!(
            "{i:3} {got:5} ring px (want {want:4}) {} {} {:?}",
            step.focus.kind,
            step.focus.path.first().map_or("", String::as_str),
            step.focus.bounds
        );
        if got < want {
            missing.push(format!("{line} in {}", step.shot.display()));
        }
        report.push(line);
    }
    fs::write(
        nested.shots.join(format!("{}-rings.txt", nested.case)),
        report.join("\n") + "\n",
    )
    .expect("the rings' report");
    assert!(missing.is_empty(), "no focus ring: {missing:#?}");
}

/// D-10's region keys from the window's start: `F6` header → transport →
/// timeline toolbar → timeline → panel → header, `Shift+F6` back; each
/// region's widget wears the ring, and the hint bar never has the focus.
fn region_cycle(case: &str, ui: UiState) {
    let mut nested = Nested::start(AREA, case, Options::default());
    let project = fixture(&nested, DesignVariant::Base, &ui);
    open(&mut nested, &project, still());
    let start = focus(nested.window(0)).expect("a focus at the start");
    assert_eq!(region_of(&start.path), "header", "the start: {start:?}");
    let baseline = nested.screenshot("baseline");
    let mut steps = Vec::new();
    let forward = [
        "transport",
        "timelineToolbar",
        "timeline",
        "panel",
        "header",
    ];
    for (i, want) in forward.iter().enumerate() {
        let step = step(&mut nested, &["-k", "F6"], &format!("f6-{i}"));
        assert_eq!(
            region_of(&step.focus.path),
            *want,
            "F6 {i}: {:?}",
            step.focus
        );
        steps.push(step);
    }
    let backward = [
        "panel",
        "timeline",
        "timelineToolbar",
        "transport",
        "header",
    ];
    for (i, want) in backward.iter().enumerate() {
        let step = step(
            &mut nested,
            &["-M", "shift", "-k", "F6", "-m", "shift"],
            &format!("shift-f6-{i}"),
        );
        assert_eq!(
            region_of(&step.focus.path),
            *want,
            "Shift+F6 {i}: {:?}",
            step.focus
        );
        steps.push(step);
    }
    // Shift+F6's walk lands where F6's did, region by region.
    for (back, fwd) in [(5, 3), (6, 2), (7, 1), (8, 0)] {
        assert_eq!(steps[back].focus, steps[fwd].focus, "the same widget");
    }
    assert_rings(&nested, &steps, &baseline, &stroked_rows(&ui));
    let gui = nested.window(0);
    gui.wait_idle();
    assert_clean(gui);
    close(&mut nested);
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn f6_cycles_the_regions_of_s02() {
    region_cycle("f6-s02", s02_ui());
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn f6_cycles_the_regions_of_s06() {
    region_cycle("f6-s06", s06_ui());
}

/// GTK's chain backwards from the window's start (`Shift+Tab`: the key
/// sends `ISO_Left_Tab`, as a keyboard does) until it comes round: every
/// focusable widget once, region by region in the reverse of BRIEF §6's
/// order (panel → timeline → timeline toolbar → transport → stage →
/// header), none in the hint bar, each with its ring; the window's start
/// is in it. Returns the regions visited.
fn tab_walk(case: &str, ui: UiState) -> Vec<String> {
    let mut nested = Nested::start(AREA, case, Options::default());
    let project = fixture(&nested, DesignVariant::Base, &ui);
    open(&mut nested, &project, still());
    let start = focus(nested.window(0)).expect("a focus at the start");
    let baseline = nested.screenshot("baseline");
    let mut steps: Vec<Step> = Vec::new();
    let mut again = None;
    for i in 0..150 {
        let step = step(
            &mut nested,
            &["-M", "shift", "-k", "ISO_Left_Tab", "-m", "shift"],
            &format!("step-{i:03}"),
        );
        if let Some(seen) = steps.iter().position(|s| s.focus == step.focus) {
            again = Some(seen);
            break;
        }
        steps.push(step);
    }
    let report: Vec<String> = steps
        .iter()
        .map(|s| format!("{} {:?}", s.focus.path.join(" < "), s.focus.bounds))
        .collect();
    fs::write(
        nested.shots.join(format!("{case}-walk.txt")),
        report.join("\n") + "\n",
    )
    .expect("the walk's report");
    assert_eq!(
        again,
        Some(0),
        "the walk comes round to its first widget: {report:#?}"
    );
    assert!(
        steps.iter().any(|s| s.focus == start),
        "the window's start {start:?} is in the chain: {report:#?}"
    );
    let regions: Vec<String> = steps
        .iter()
        .map(|s| region_of(&s.focus.path).to_owned())
        .collect();
    assert!(
        !regions.iter().any(|r| r == "hintbar" || r == "other"),
        "outside the regions: {report:#?}"
    );
    // Backwards: the ranks never go up (the first step wraps from the
    // header to the panel's last widget).
    for (i, pair) in regions.windows(2).enumerate() {
        assert!(
            rank(&pair[1]) <= rank(&pair[0]),
            "step {} goes from {} to {}: {report:#?}",
            i + 1,
            pair[0],
            pair[1]
        );
    }
    for region in ORDER {
        assert!(
            regions.iter().any(|r| r == region),
            "the walk never reached {region}: {report:#?}"
        );
    }
    assert_rings(&nested, &steps, &baseline, &stroked_rows(&ui));
    let gui = nested.window(0);
    gui.wait_idle();
    assert_clean(gui);
    close(&mut nested);
    regions
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn shift_tab_walks_every_focusable_widget_of_s02() {
    tab_walk("tab-s02", s02_ui());
}

#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn shift_tab_walks_every_focusable_widget_of_s06() {
    let regions = tab_walk("tab-s06", s06_ui());
    // Program: the transport's every button takes the focus.
    let transport = regions.iter().filter(|r| *r == "transport").count();
    assert!(transport >= 5, "the transport's buttons: {regions:?}");
}
