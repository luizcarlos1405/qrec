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
