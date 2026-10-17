//! WP10's audits (docs/GUI.md G§8.6, DESIGN Motion) in a nested headless
//! sway: the GPU device nodes a window's start opens (`strace -f`, read by
//! `gui_support/scripts/gpu-opens.sh`), and reduced motion where GTK's own
//! animations are at stake (the Narrow sheet, libadwaita's), with test
//! mode's `animations` flag keeping them on. The rest of DESIGN's motion
//! table is GTK-free (`theme::motion`'s table test); the scale runs are
//! every suite's own cases at `QREC_TEST_SCALE` 1.5 and 2.
//!
//! `cargo test -p qrec --test gui_audits -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon; strace).
//! Screenshots and logs go to `tmp/gui-shots/audits/`.

mod gui_support;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use gui_support::{Launch, Nested, Options, TOOL_TIMEOUT, wait_for_gui_host};
use qrec_core::ClipId;
use qrec_core::state::{CanvasMode, UiState};
use qrec_shell::gpu::{VENDOR_AMD, VENDOR_INTEL, VENDOR_NVIDIA, render_nodes};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use qrec_shell::testkit::run_tool;
use serde_json::json;

const OPEN: Duration = Duration::from_secs(90);
const EXIT: Duration = Duration::from_secs(30);
const AREA: &str = "audits";

/// The design fixture in a folder named `rust-traits-talk` with `ui`.
fn fixture(nested: &Nested, ui: &UiState) -> PathBuf {
    let dir = nested.env.empty_dir("rust-traits-talk");
    design_fixture(&dir, DesignVariant::Base);
    write_ui(&dir, ui);
    dir
}

/// Launches window `n` on `project` and waits until it is open and idle.
fn open(nested: &mut Nested, n: usize, project: &Path, launch: Launch) {
    nested.launch(project, launch);
    wait_for_gui_host(nested, project, OPEN);
    let gui = nested.window(n);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
}

/// No caught panic, no GLib or GTK critical, and the a11y walk empty (the
/// cases launch with `Launch::a11y()`, G§8.4).
fn assert_clean(nested: &mut Nested, n: usize) {
    let gui = nested.window(n);
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    let a11y = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "{a11y}");
    gui.assert_no_criticals();
}

// ------------------------------------------------------- GPU device opens

/// The render node the window may open (G§3.13): the first Intel or AMD
/// one, by its PCI vendor (the GL renderer's under gles2 and the chosen VA
/// node, A§2.1); none without one.
fn chosen_node() -> Option<String> {
    render_nodes()
        .into_iter()
        .find(|n| matches!(n.vendor, Some(VENDOR_INTEL | VENDOR_AMD)))
        .map(|n| n.path)
}

/// NVIDIA's render nodes (never to be opened: renderD128 hangs on the dev
/// machine, A§8.10).
fn nvidia_nodes() -> BTreeSet<String> {
    render_nodes()
        .into_iter()
        .filter(|n| n.vendor == Some(VENDOR_NVIDIA))
        .map(|n| n.path)
        .collect()
}

/// The `/dev/dri` nodes a trace touched (every open but those that found
/// no such file, as `gpu-opens.sh` counts them).
fn dri_nodes(trace: &Path) -> BTreeSet<String> {
    let text = fs::read_to_string(trace).unwrap_or_default();
    text.lines()
        .filter(|l| !l.contains("ENOENT"))
        .filter_map(|l| {
            let at = l.find("\"/dev/dri/")? + 1;
            let end = l[at..].find('"')? + at;
            Some(l[at..end].to_owned())
        })
        .collect()
}

/// `gpu-opens.sh` on `trace`: its report and exit status (1: an NVIDIA
/// touch).
fn gpu_opens(trace: &Path) -> (String, Option<i32>) {
    let script = gui_support::repo().join("crates/qrec/tests/gui_support/scripts/gpu-opens.sh");
    let out = run_tool(Command::new("bash").arg(&script).arg(trace), TOOL_TIMEOUT);
    (
        format!("{}{}", String::from_utf8_lossy(&out.stdout), out.stderr),
        out.status.code(),
    )
}

/// G§8.6, G§3.13: a window's start opens only the chosen render node, the
/// first Intel or AMD one (under gles2 GTK renders on it; under pixman,
/// `GDK_DISABLE=gl,vulkan`, nothing renders on a GPU, and the fake backend
/// probes no VA device), never NVIDIA's, with every child it starts
/// (`strace -f`: GStreamer's plugin scanner, ffmpeg): twice, the second
/// with the Program played a moment (the player decodes). GStreamer's
/// registry is the dev shell's (`GST_REGISTRY_1_0`, which Qrec leaves as
/// the user set it, A§8.10), so no first-ever plugin scan runs here: that
/// scan's `va` plugin probes every render node it finds, NVIDIA's too, and
/// is never forced on the dev machine. Both traces and `gpu-opens.sh`'s
/// report stay in `logs/`.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn a_window_opens_only_the_chosen_render_node() {
    let mut nested = Nested::start(AREA, "gpu-opens", Options::default());
    let ui = UiState {
        canvas_mode: CanvasMode::Program,
        selection: vec![ClipId::from_raw("c2d0")],
        playhead: 2437,
        ..UiState::default()
    };
    let project = fixture(&nested, &ui);
    let logs = nested.shots.join("logs");
    let trace = |what: &str| logs.join(format!("gpu-opens-{what}.strace"));
    let (first, second) = (trace("first"), trace("second"));
    for (n, path) in [(0, &first), (1, &second)] {
        let _ = fs::remove_file(path);
        open(&mut nested, n, &project, Launch::a11y().traced(path));
        if n == 1 {
            // The Program plays a moment (the player decodes).
            let gui = nested.window(1);
            let play = gui.ctl(json!({"op": "dispatch", "command": {"cmd": "play"}}));
            assert_eq!(play["ok"], true, "{play}");
            std::thread::sleep(Duration::from_millis(1500));
            let pause = gui.ctl(json!({"op": "dispatch", "command": {"cmd": "pause"}}));
            assert_eq!(pause["ok"], true, "{pause}");
            gui.wait_idle();
        }
        assert_clean(&mut nested, n);
        nested.close_window();
        assert_eq!(
            nested.window(n).wait_exit(EXIT),
            Some(0),
            "window {n}'s exit"
        );
    }
    let mut report = String::new();
    for (what, path) in [("first", &first), ("second", &second)] {
        let (text, status) = gpu_opens(path);
        report.push_str(&format!(
            "== {what} start: gpu-opens.sh exit {status:?}\n{text}\n/dev/dri: {:?}\n\n",
            dri_nodes(path)
        ));
    }
    fs::write(logs.join("gpu-opens-report.txt"), &report).expect("the report");
    eprintln!("{report}");
    let chosen: BTreeSet<String> = chosen_node().into_iter().collect();
    for path in [&first, &second] {
        let opened = dri_nodes(path);
        assert!(
            opened.is_disjoint(&nvidia_nodes()),
            "{}: the window opened an NVIDIA node: {opened:?}\n{report}",
            path.display()
        );
        assert!(
            opened.is_subset(&chosen),
            "{}: the window opened {opened:?}, beyond the chosen node {chosen:?}\n{report}",
            path.display()
        );
        assert_eq!(gpu_opens(path).1, Some(0), "gpu-opens.sh: {report}");
    }
}

/// `gpu-opens.sh` counts the opens of NVIDIA's device nodes and driver
/// code, never a read of a node's vendor in `/sys` (Qrec's own
/// `gpu::render_nodes`: it made every traced start an NVIDIA touch, WP10's
/// audit), nor an open that found no file, nor the chosen node's.
#[test]
fn gpu_opens_counts_devices_and_drivers_not_vendor_reads() {
    let env = qrec_shell::testkit::TestEnv::new();
    let line = |path: &str, result: &str| {
        format!("7 openat(AT_FDCWD, \"{path}\", O_RDWR|O_CLOEXEC) = {result}\n")
    };
    let mut quiet = String::new();
    for node in render_nodes() {
        let name = Path::new(&node.path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        quiet.push_str(&line(&format!("/sys/class/drm/{name}/device/vendor"), "4"));
        quiet.push_str(&line(&node.path, "-1 ENOENT (No such file or directory)"));
    }
    if let Some(chosen) = chosen_node() {
        quiet.push_str(&line(&chosen, "5"));
    }
    quiet.push_str(&line(
        "/nix/store/x/lib/libGLX_nvidia.so.0",
        "-1 ENOENT (No such file or directory)",
    ));
    let mut touches = vec![
        line("/dev/nvidia0", "6"),
        line("/nix/store/x-nvidia/lib/libGLX_nvidia.so.0", "6"),
        line("/nix/store/x-nvidia/lib/libnvidia-glcore.so.580", "6"),
    ];
    touches.extend(nvidia_nodes().iter().map(|node| line(node, "6")));
    let trace = env.root().join("quiet.strace");
    fs::write(&trace, &quiet).expect("a trace");
    let (report, status) = gpu_opens(&trace);
    assert_eq!(status, Some(0), "no NVIDIA touch in:\n{quiet}\n{report}");
    for (i, touch) in touches.iter().enumerate() {
        let trace = env.root().join(format!("touch-{i}.strace"));
        fs::write(&trace, format!("{quiet}{touch}")).expect("a trace");
        let (report, status) = gpu_opens(&trace);
        assert_eq!(status, Some(1), "an NVIDIA touch: {touch}\n{report}");
    }
}

// --------------------------------------------------------- reduced motion

/// `ui.reduced_motion = true` in the user's `config.toml`.
fn reduced_motion(nested: &Nested) {
    let config = nested.env.config_home().join("qrec");
    fs::create_dir_all(&config).expect("the config folder");
    fs::write(config.join("config.toml"), "[ui]\nreduced_motion = true\n").expect("config.toml");
}

/// The Narrow sheet's x (window px) at each `regions` for `ms` (`None`:
/// not showing).
fn sample_sheet(nested: &mut Nested, ms: u64) -> Vec<Option<f64>> {
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut samples = Vec::new();
    while Instant::now() < deadline {
        let regions = nested.window(0).regions(&[]);
        samples.push(regions.get("region-panel").map(|r| r[0]));
    }
    samples
}

/// `samples` as runs of equal samples, each once with its count.
fn runs(samples: &[Option<f64>]) -> String {
    let mut runs: Vec<(Option<f64>, usize)> = Vec::new();
    for &s in samples {
        match runs.last_mut() {
            Some((last, n)) if *last == s => *n += 1,
            _ => runs.push((s, 1)),
        }
    }
    runs.iter()
        .map(|(s, n)| match s {
            Some(x) => format!("{x}×{n}"),
            None => format!("gone×{n}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `samples` caught the sheet between the window's edge (1000)
/// and its place (616): mid-slide.
fn slid(samples: &[Option<f64>]) -> bool {
    samples.iter().flatten().any(|&x| x > 617.0 && x < 999.0)
}

/// What [`the_sheet`] saw: the sheet's x through its opening, its close by
/// `Esc` and its own close (a click on the content it dims), and GTK's
/// animation setting while it was open and after each close.
struct Sheet {
    opening: Vec<Option<f64>>,
    closing: Vec<Option<f64>>,
    dismissed: Vec<Option<f64>>,
    animations: [bool; 3],
}

/// The Narrow sheet opened by the header's layers button, closed by `Esc`,
/// opened again and closed by a click on the content it dims (libadwaita's
/// own close), its x sampled through each; GTK's own animations on
/// (`QREC_GUI_TEST=1,animations`).
fn the_sheet(reduced: bool) -> Sheet {
    let case = if reduced {
        "sheet-reduced"
    } else {
        "sheet-animated"
    };
    let mut nested = Nested::start(AREA, case, Options::size(1000, 1000));
    if reduced {
        reduced_motion(&nested);
    }
    let ui = UiState {
        playhead: 7391,
        ..UiState::default()
    };
    let project = fixture(&nested, &ui);
    open(
        &mut nested,
        0,
        &project,
        Launch::a11y().with_env("QREC_GUI_TEST", "1,animations"),
    );
    let local = nested.window(0).local();
    assert_eq!(local["layout"]["width"], "Narrow", "{local}");
    assert_eq!(local["reducedMotion"], reduced, "{local}");
    assert_eq!(
        local["gtkAnimations"], true,
        "test mode left them on: {local}"
    );
    let button = nested.window(0).regions(&["layers-button"])["layers-button"];
    let press = |nested: &mut Nested| {
        let [x, y, w, h] = button;
        nested.pointer().click(x + w / 2.0, y + h / 2.0);
    };
    press(&mut nested);
    let opening = sample_sheet(&mut nested, 800);
    let local = nested.window(0).local();
    assert_eq!(local["overlays"], json!(["LayersSheet"]), "{local}");
    let animations = |nested: &mut Nested| nested.window(0).local()["gtkAnimations"] == true;
    let while_open = animations(&mut nested);
    nested.keys(&["-k", "Escape"]);
    let closing = sample_sheet(&mut nested, 800);
    let after_esc = animations(&mut nested);
    press(&mut nested);
    gui_support::poll(Duration::from_secs(10), "the sheet again", || {
        let regions = nested.window(0).regions(&[]);
        regions
            .get("region-panel")
            .is_some_and(|r| (r[0] - 616.0).abs() <= 1.0)
            .then_some(())
    });
    // The stage under the sheet's dim: libadwaita's own close.
    nested.pointer().click(300.0, 400.0);
    let dismissed = sample_sheet(&mut nested, 800);
    let local = nested.window(0).local();
    assert_eq!(local["overlays"], json!([]), "{local}");
    let after_dismissal = animations(&mut nested);
    assert_clean(&mut nested, 0);
    let record = nested.shots.join(format!("{case}-samples.txt"));
    let lines: Vec<String> = [
        ("opening", &opening),
        ("closing", &closing),
        ("dismissed", &dismissed),
    ]
    .iter()
    .map(|(what, s)| format!("{what}: {}", runs(s)))
    .collect();
    fs::write(record, lines.join("\n") + "\n").expect("the samples");
    Sheet {
        opening,
        closing,
        dismissed,
        animations: [while_open, after_esc, after_dismissal],
    }
}

/// DESIGN Motion's panel overlay (D-30): under reduced motion the Narrow
/// sheet neither slides in nor out, its own close included: from its first
/// sample it is at its place (x 616), and it goes at once. libadwaita
/// slides it otherwise (the same samples catch it mid-slide: the twin
/// case below), and reads no reduced motion itself: the window turns GTK's
/// animations off while the sheet is open, and on again once it goes.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn under_reduced_motion_the_narrow_sheet_neither_slides_in_nor_out() {
    let Sheet {
        opening,
        closing,
        dismissed,
        animations,
    } = the_sheet(true);
    for (what, samples) in [
        ("opening", &opening),
        ("closing", &closing),
        ("dismissed", &dismissed),
    ] {
        assert!(!slid(samples), "{what}: the sheet slid: {}", runs(samples));
        // At its place, or still off the window's edge before its first
        // allocation (a sample taken between the show and the layout).
        for x in samples.iter().flatten() {
            assert!(
                (x - 616.0).abs() <= 1.0 || *x >= 999.0,
                "{what}: the sheet at x {x}: {}",
                runs(samples)
            );
        }
    }
    assert!(
        opening.iter().any(Option::is_some),
        "it opened: {}",
        runs(&opening)
    );
    assert!(
        closing.last() == Some(&None),
        "it closed: {}",
        runs(&closing)
    );
    assert!(
        dismissed.last() == Some(&None),
        "it closed: {}",
        runs(&dismissed)
    );
    // Off while the sheet was open (libadwaita reads no reduced motion of
    // its own), on again after either close.
    assert_eq!(animations, [false, true, true], "GTK's animations");
}

/// The header's layers button (window px).
fn layers_button(nested: &mut Nested) -> [f64; 4] {
    nested.window(0).regions(&["layers-button"])["layers-button"]
}

/// A click on the layers button, then until the sheet is at its place.
fn open_the_sheet(nested: &mut Nested) {
    let [x, y, w, h] = layers_button(nested);
    nested.pointer().click(x + w / 2.0, y + h / 2.0);
    gui_support::poll(Duration::from_secs(10), "the sheet at its place", || {
        let regions = nested.window(0).regions(&[]);
        regions
            .get("region-panel")
            .is_some_and(|r| (r[0] - 616.0).abs() <= 1.0)
            .then_some(())
    });
}

/// GTK's `gtk-enable-animations`, as the window reads it.
fn gtk_animations(nested: &mut Nested) -> bool {
    nested.window(0).local()["gtkAnimations"] == true
}

/// D-30 with the live follower (WP10's code audit, D2): `ui.reduced_motion
/// = true` set while the Narrow sheet is open holds it still from then on,
/// so its close by `Esc` doesn't slide (the window held GTK's animations
/// off only from a sheet's opening: one opened before the change slid out);
/// and `false` set while it is held lets it slide again.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn reduced_motion_set_while_the_sheet_is_open_holds_it_still() {
    let mut nested = Nested::start(AREA, "sheet-live", Options::size(1000, 1000));
    let ui = UiState {
        playhead: 7391,
        ..UiState::default()
    };
    let project = fixture(&nested, &ui);
    open(
        &mut nested,
        0,
        &project,
        Launch::a11y().with_env("QREC_GUI_TEST", "1,animations"),
    );
    let config_set = |nested: &mut Nested, value: &str| {
        let out = nested.cli(&project, &["config", "set", "ui.reduced_motion", value]);
        assert!(out.status.success(), "qrec config set: {out:?}");
        gui_support::poll(Duration::from_secs(10), "the change applied", || {
            let local = nested.window(0).local();
            (local["reducedMotion"] == (value == "true") && local["settingsPending"] == false)
                .then_some(())
        });
        nested.window(0).wait_idle();
    };
    open_the_sheet(&mut nested);
    assert!(gtk_animations(&mut nested), "motion allowed: not held");
    config_set(&mut nested, "true");
    assert!(
        !gtk_animations(&mut nested),
        "reduced motion now: GTK's animations held off while the sheet is open"
    );
    nested.keys(&["-k", "Escape"]);
    let closing = sample_sheet(&mut nested, 800);
    assert!(
        closing.last() == Some(&None),
        "it closed: {}",
        runs(&closing)
    );
    assert!(!slid(&closing), "the sheet slid out: {}", runs(&closing));
    assert!(gtk_animations(&mut nested), "on again once it went");

    // Held still from its opening, then motion allowed again: it slides.
    open_the_sheet(&mut nested);
    assert!(!gtk_animations(&mut nested), "held from its opening");
    config_set(&mut nested, "false");
    assert!(
        gtk_animations(&mut nested),
        "motion allowed: no longer held"
    );
    nested.keys(&["-k", "Escape"]);
    let closing = sample_sheet(&mut nested, 800);
    assert!(slid(&closing), "no slide caught: {}", runs(&closing));
    assert_clean(&mut nested, 0);
    let record = nested.shots.join("sheet-live-samples.txt");
    fs::write(record, format!("closing: {}\n", runs(&closing))).expect("the samples");
}

/// D-30's system setting, followed live, and the Narrow sheet's hold on it
/// (WP10's code audit, D1): the window holds GTK's `gtk-enable-animations`
/// off while the sheet is open under reduced motion, reads that as its own
/// (not the desktop asking for less motion: `ui.reduced_motion = false`
/// while the sheet is held lets it go), and gives the setting back to the
/// desktop when the sheet goes (`gtk::Settings::reset_property`: a value
/// the window sets itself, `true` too, is the application's to GTK, which
/// then ignores every later desktop change). The desktop is a stand-in
/// Settings portal on the nested bus (GDK reads the setting from the
/// portal only, `GDK_DEBUG=portals` forcing it without a portal service
/// file; libadwaita left to its own settings, `ADW_DISABLE_PORTAL`), and
/// test mode lets the system's settings count (`system-motion`): the
/// desktop's animations off make the window's motion reduced, before the
/// sheet and after it.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_desktops_animations_reach_the_window_after_the_sheet() {
    let mut nested = Nested::start(AREA, "sheet-desktop", Options::size(1000, 1000));
    let ui = UiState {
        playhead: 7391,
        ..UiState::default()
    };
    let project = fixture(&nested, &ui);
    let bus = format!(
        "unix:path={}",
        nested.env.xdg_runtime_dir().join("bus").display()
    );
    let desktop = portal::serve(&bus, true);
    open(
        &mut nested,
        0,
        &project,
        Launch::a11y()
            .with_env("QREC_GUI_TEST", "1,animations,system-motion")
            .with_env("GDK_DEBUG", "portals")
            .with_env("ADW_DISABLE_PORTAL", "1"),
    );
    // GTK's setting and the window's motion policy, as the window reads them.
    let motion = |nested: &mut Nested| {
        let local = nested.window(0).local();
        (
            local["gtkAnimations"] == true,
            local["reducedMotion"] == true,
        )
    };
    let wait_motion = |nested: &mut Nested, want: (bool, bool), what: &str| {
        gui_support::poll(Duration::from_secs(10), what, || {
            (motion(nested) == want).then_some(())
        });
    };
    let config_set = |nested: &mut Nested, value: &str| {
        let out = nested.cli(&project, &["config", "set", "ui.reduced_motion", value]);
        assert!(out.status.success(), "qrec config set: {out:?}");
        gui_support::poll(Duration::from_secs(10), "the change applied", || {
            (nested.window(0).local()["settingsPending"] == false).then_some(())
        });
        nested.window(0).wait_idle();
    };
    assert_eq!(motion(&mut nested), (true, false), "the desktop's: on");
    // Before the sheet: the window follows the desktop (D-30's system).
    desktop.set_animations(false);
    wait_motion(&mut nested, (false, true), "the desktop's off: reduced");
    desktop.set_animations(true);
    wait_motion(&mut nested, (true, false), "the desktop's on again");
    // Reduced by the config: the sheet holds GTK's animations off while it
    // is open; motion allowed meanwhile gives them back at once.
    config_set(&mut nested, "true");
    assert_eq!(motion(&mut nested), (true, true), "reduced by the config");
    open_the_sheet(&mut nested);
    assert_eq!(motion(&mut nested), (false, true), "held off while open");
    config_set(&mut nested, "false");
    wait_motion(
        &mut nested,
        (true, false),
        "the window's own hold isn't the desktop's: motion allowed, given back",
    );
    config_set(&mut nested, "true");
    assert_eq!(motion(&mut nested), (false, true), "held again");
    nested.keys(&["-k", "Escape"]);
    gui_support::poll(Duration::from_secs(10), "the sheet gone", || {
        nested.window(0).local()["overlays"]
            .as_array()
            .is_some_and(Vec::is_empty)
            .then_some(())
    });
    assert_eq!(motion(&mut nested), (true, true), "given back once it went");
    config_set(&mut nested, "false");
    assert_eq!(motion(&mut nested), (true, false));
    // After the sheet: the desktop's changes still reach the window.
    desktop.set_animations(false);
    wait_motion(&mut nested, (false, true), "the desktop's off, after");
    desktop.set_animations(true);
    wait_motion(&mut nested, (true, false), "the desktop's on, after");
    assert_clean(&mut nested, 0);
}

/// A stand-in `org.freedesktop.portal.Settings` on the nested bus (the
/// desktop's settings as GDK reads them on Wayland: `ReadAll` once, then
/// `SettingChanged`), with `org.gnome.desktop.interface`'s
/// `enable-animations` only. GIO's D-Bus (the workspace's gtk, as
/// `gui_a11y`'s registry), on a thread and main context of its own; the
/// window's calls are answered while the case lasts.
mod portal {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    use gtk::glib::variant::ToVariant;
    use gtk::{gio, glib};

    const NAME: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const SETTINGS: &str = "org.freedesktop.portal.Settings";
    const NAMESPACE: &str = "org.gnome.desktop.interface";
    const KEY: &str = "enable-animations";
    const NODE: &str = r#"<node>
  <interface name="org.freedesktop.portal.Settings">
    <method name="ReadAll">
      <arg name="namespaces" type="as" direction="in"/>
      <arg name="value" type="a{sa{sv}}" direction="out"/>
    </method>
    <method name="Read">
      <arg name="namespace" type="s" direction="in"/>
      <arg name="key" type="s" direction="in"/>
      <arg name="value" type="v" direction="out"/>
    </method>
    <method name="ReadOne">
      <arg name="namespace" type="s" direction="in"/>
      <arg name="key" type="s" direction="in"/>
      <arg name="value" type="v" direction="out"/>
    </method>
    <signal name="SettingChanged">
      <arg name="namespace" type="s"/>
      <arg name="key" type="s"/>
      <arg name="value" type="v"/>
    </signal>
    <property name="version" type="u" access="read"/>
  </interface>
</node>"#;
    /// How long a call to the bus may take.
    const CALL_MS: i32 = 5000;

    /// The desktop, served: its animation setting and its connection.
    pub struct Desktop {
        animations: Arc<AtomicBool>,
        conn: gio::DBusConnection,
    }

    impl Desktop {
        /// The desktop turns its animations on or off: the setting, and
        /// `SettingChanged` for it.
        pub fn set_animations(&self, on: bool) {
            self.animations.store(on, Ordering::SeqCst);
            let value = on.to_variant();
            self.conn
                .emit_signal(
                    None,
                    PATH,
                    SETTINGS,
                    "SettingChanged",
                    Some(&(NAMESPACE, KEY, value).to_variant()),
                )
                .expect("SettingChanged");
            self.conn
                .flush_sync(gio::Cancellable::NONE)
                .expect("the signal sent");
        }
    }

    /// Serves the portal on the bus at `address`, its animations `on`,
    /// until the bus goes (the case's end).
    pub fn serve(address: &str, on: bool) -> Desktop {
        let animations = Arc::new(AtomicBool::new(on));
        let (ready_tx, ready) = mpsc::channel();
        let (address, setting) = (address.to_owned(), Arc::clone(&animations));
        thread::spawn(move || {
            // The calls come on the main context the object was registered
            // under: this thread's own.
            let context = glib::MainContext::new();
            let ran = context.with_thread_default(|| {
                let (conn, gone) = match start(&address, setting) {
                    Ok(up) => up,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(conn.clone()));
                // Until the bus goes with the case.
                while !gone.load(Ordering::SeqCst) {
                    context.iteration(true);
                }
            });
            if let Err(e) = ran {
                eprintln!("portal: no main context: {e}");
            }
        });
        match ready.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(conn)) => Desktop { animations, conn },
            Ok(Err(e)) => panic!("the stand-in Settings portal: {e}"),
            Err(e) => panic!("the stand-in Settings portal never started: {e}"),
        }
    }

    /// Connects, serves the object and takes the portal's name, last.
    fn start(
        address: &str,
        animations: Arc<AtomicBool>,
    ) -> Result<(gio::DBusConnection, Arc<AtomicBool>), glib::Error> {
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
        let info = node.lookup_interface(SETTINGS).expect("the interface");
        conn.register_object(PATH, &info)
            .method_call(move |_, _, _, _, method, params, invocation| {
                let on = animations.load(Ordering::SeqCst);
                let ours = |namespace: &str, key: &str| namespace == NAMESPACE && key == KEY;
                match method {
                    // `a{sa{sv}}`: each namespace, then each key's value.
                    "ReadAll" => {
                        let mut keys = std::collections::HashMap::new();
                        keys.insert(KEY.to_owned(), on.to_variant());
                        let mut all = std::collections::HashMap::new();
                        all.insert(NAMESPACE.to_owned(), keys);
                        invocation.return_value(Some(&(all,).to_variant()));
                    }
                    "Read" | "ReadOne" => {
                        let (namespace, key) = params.get::<(String, String)>().unwrap_or_default();
                        if ours(&namespace, &key) {
                            // `(v)`; `Read`, the older call, wraps the
                            // value once more.
                            let value = if method == "Read" {
                                glib::Variant::from_variant(&on.to_variant())
                            } else {
                                on.to_variant()
                            };
                            invocation.return_value(Some(&(value,).to_variant()));
                        } else {
                            invocation.return_dbus_error(
                                "org.freedesktop.portal.Error.NotFound",
                                &format!("no {namespace} {key} here"),
                            );
                        }
                    }
                    other => invocation.return_dbus_error(
                        "org.freedesktop.DBus.Error.UnknownMethod",
                        &format!("the stand-in has no {other}"),
                    ),
                }
            })
            // Its one property, `version`.
            .property(|_, _, _, _, _| 2u32.to_variant())
            .build()?;
        let reply = conn.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(NAME, 0u32).to_variant()),
            Some(glib::VariantTy::new("(u)").expect("a type")),
            gio::DBusCallFlags::NONE,
            CALL_MS,
            gio::Cancellable::NONE,
        )?;
        // 1: the name's primary owner.
        if reply.child_value(0).get::<u32>() != Some(1) {
            let why = format!("the portal's name isn't ours: {reply}");
            return Err(glib::Error::new(gio::IOErrorEnum::Failed, &why));
        }
        Ok((conn, gone))
    }
}

/// The twin: with motion allowed libadwaita slides the sheet in and out,
/// and the samples catch it (so the reduced case's would).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn with_motion_allowed_the_narrow_sheet_slides() {
    let Sheet {
        opening,
        closing,
        animations,
        ..
    } = the_sheet(false);
    assert_eq!(animations, [true; 3], "GTK's animations, never touched");
    assert!(
        slid(&opening),
        "no slide caught opening: {}",
        runs(&opening)
    );
    assert!(
        slid(&closing),
        "no slide caught closing: {}",
        runs(&closing)
    );
}
