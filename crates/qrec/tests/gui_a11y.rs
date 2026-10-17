//! WP10's accessibility cases (docs/GUI.md G§7 WP10, G§8.4; PRODUCT
//! Accessibility; BRIEF §6), in a nested headless sway on the design
//! fixture:
//!
//! - **Announcements** (`gtk::Accessible::announce`): a take starting to
//!   record, the take landing, the take's other ends and the export's end,
//!   in the toasts' words, polite, assertive for a failure. The window
//!   keeps what it announced for the test channel (`local`'s
//!   `announcements`: message, priority, and whether GTK had a context
//!   that announces) and logs it (`announce (polite): …`).
//! - **Focus order** (BRIEF §6 by D-10): from the window's start, `F6`
//!   goes header → transport → timeline toolbar → timeline → panel and
//!   around, `Shift+F6` back; GTK's own chain, walked with `Shift+Tab`
//!   (`Tab` switches Live and Program in the main window, D-10), visits
//!   every focusable widget of S02 and S06 region by region in the reverse
//!   of that order, the stage canvas between the header and the transport,
//!   and never the hint bar. Each widget it lands on draws the 2 px focus
//!   ring (a shot while the key's keyboard holds the window active: G§8.4).
//! - The a11y walk stays empty in each of these states.
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
    CLI_TIMEOUT, Gui, Launch, Nested, Options, poll, reap_within, wait_for_gui_host,
};
use qrec_core::state::CanvasMode;
use qrec_core::{ClipId, UiState};
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
/// politeness: 1 polite, 2 assertive), which a screen reader hears.
mod atspi {
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use futures_lite::StreamExt;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Type};
    use zbus::{MatchRule, MessageStream, interface};

    const REGISTRY: &str = "org.a11y.atspi.Registry";
    const ROOT: &str = "/org/a11y/atspi/accessible/root";

    /// An object reference, `(so)`.
    #[derive(serde::Serialize, Type)]
    struct Reference(String, OwnedObjectPath);

    struct Socket;

    #[interface(name = "org.a11y.atspi.Socket")]
    impl Socket {
        /// The application's root is embedded in the desktop: here. One
        /// out argument, the reference (`((so))`; a bare struct would be
        /// its two fields).
        fn embed(&self, _plug: (String, OwnedObjectPath)) -> (Reference,) {
            (Reference(
                REGISTRY.to_owned(),
                OwnedObjectPath::try_from(ROOT).expect("a path"),
            ),)
        }
    }

    struct Registry;

    #[interface(name = "org.a11y.atspi.Registry")]
    impl Registry {
        /// No screen reader listens to events (announcements go out
        /// anyway).
        fn get_registered_events(&self) -> Vec<(String, String)> {
            Vec::new()
        }
    }

    /// What was announced on the bus: the politeness and the message.
    pub type Heard = Arc<Mutex<Vec<(i32, String)>>>;

    /// Serves the registry on the bus at `address` and records the
    /// announcements until the bus goes (the case's end).
    pub fn serve(address: &str) -> Heard {
        let heard: Heard = Arc::default();
        let (ready_tx, ready) = mpsc::channel();
        let (address, log) = (address.to_owned(), Arc::clone(&heard));
        thread::spawn(move || {
            zbus::block_on(async move {
                let built = async {
                    let conn = zbus::connection::Builder::address(address.as_str())?
                        .name(REGISTRY)?
                        .serve_at(ROOT, Socket)?
                        .serve_at("/org/a11y/atspi/registry", Registry)?
                        .build()
                        .await?;
                    let rule = MatchRule::builder()
                        .msg_type(zbus::message::Type::Signal)
                        .interface("org.a11y.atspi.Event.Object")?
                        .member("Announcement")?
                        .build();
                    let stream = MessageStream::for_match_rule(rule, &conn, None).await?;
                    Ok::<_, zbus::Error>((conn, stream))
                }
                .await;
                let (_conn, mut stream) = match built {
                    Ok(both) => {
                        let _ = ready_tx.send(Ok(()));
                        both
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                // Until the bus goes with the case.
                while let Some(Ok(msg)) = stream.next().await {
                    type Body = (String, i32, i32, OwnedValue, HashMap<String, OwnedValue>);
                    match msg.body().deserialize::<Body>() {
                        Ok((_, live, _, value, _)) => {
                            let text = String::try_from(value).unwrap_or_default();
                            log.lock().expect("the log").push((live, text));
                        }
                        Err(e) => eprintln!("atspi: an announcement that isn't one: {e}"),
                    }
                }
            });
        });
        match ready.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => heard,
            Ok(Err(e)) => panic!("the stand-in AT-SPI registry: {e}"),
            Err(e) => panic!("the stand-in AT-SPI registry never started: {e}"),
        }
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
