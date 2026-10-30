//! The X11 smoke (docs/GUI.md G§8.4, issue #9): `qrec`'s window on a
//! private Xvfb with no window manager (`gui_support::x11`), in test mode
//! with the fake capture backend and the design inventory, as the sway
//! suites run it. It holds what only X11 shows: GDK's backend picked from
//! `DISPLAY` alone, the window drawn by Cairo and by GL on llvmpipe, its
//! WM_CLASS and icon, a held key's auto-repeat, Copy image through the X
//! clipboard, the quit, and the first window's size on a small screen.
//!
//! Its cases start an X server whatever `QREC_TEST_DISPLAY` says
//! (`Options::x11`). `QREC_TEST_WLR=gles2` draws their windows with GL on
//! llvmpipe instead of Cairo, `QREC_TEST_SCALE=2` at `GDK_SCALE=2` (there is
//! no 1.5 run: GTK's X11 backend scales by whole numbers).
//!
//! `cargo nextest run -p qrec --run-ignored only --test gui_x11` (inside the
//! dev shell: Xvfb, xdotool, xclip, xprop, ffmpeg's x11grab, dbus-daemon).
//! Shots and logs go to `tmp/gui-shots/x11/`.

mod gui_support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use gui_support::{
    BRIEF_REGIONS, Launch, Nested, Options, WP0B_REGIONS, assert_regions, poll, rgb_pixels,
    token_rgb, wait_for_gui_host,
};
use qrec_core::state::CanvasMode;
use qrec_core::{ClipId, UiState};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use serde_json::{Value, json};

const AREA: &str = "x11";
const OPEN: Duration = Duration::from_secs(60);
const EXIT: Duration = Duration::from_secs(20);

/// A common monitor whose first window is the default 1600×1000 (90 % of
/// it is more than that): at 0,0, the editor inside GTK's frame.
const SCREEN: (u32, u32) = (1920, 1200);

fn options() -> Options {
    Options::size(SCREEN.0, SCREEN.1).x11()
}

// ---------------------------------------------------------------- helpers

/// The design fixture as `variant` in a folder named `rust-traits-talk`,
/// with `ui` as its `ui.json`.
fn fixture(nested: &Nested, variant: DesignVariant, ui: &UiState) -> PathBuf {
    let dir = nested.env.empty_dir("rust-traits-talk");
    design_fixture(&dir, variant);
    write_ui(&dir, ui);
    dir
}

/// Live, the playhead at 7391 (S02's).
fn live_ui() -> UiState {
    UiState {
        playhead: 7391,
        ..UiState::default()
    }
}

/// S06's: Program, c2d0 selected, the playhead at 2437.
fn program_ui() -> UiState {
    UiState {
        canvas_mode: CanvasMode::Program,
        selection: vec![ClipId::from_raw("c2d0")],
        playhead: 2437,
        ..UiState::default()
    }
}

/// Launches window `n` (the case's `n`th) on `project` and waits until it
/// is open and idle (`within`: GL on llvmpipe warms up for seconds).
fn open_within(nested: &mut Nested, n: usize, project: &Path, launch: Launch, within: Duration) {
    let pid = nested.launch(project, launch).pid();
    assert_eq!(nested.gui(n).pid(), pid, "window {n} is the one launched");
    wait_for_gui_host(nested, project, OPEN);
    let gui = nested.window(n);
    gui.wait_phase("open", OPEN);
    gui.wait_idle_within(within);
}

/// Launches the case's window on `project` and waits until it is open
/// and idle.
fn open(nested: &mut Nested, project: &Path, launch: Launch) {
    open_within(nested, 0, project, launch, Duration::from_secs(30));
}

/// The objective checks a case ends with on window `n`: the a11y walk
/// (launched with `Launch::a11y`), no panic caught, no GLib or GTK
/// critical, no ERROR line.
fn assert_clean(nested: &mut Nested, n: usize) {
    let gui = nested.window(n);
    let a11y = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(a11y["ok"], true, "a11y: {a11y}");
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    gui.assert_no_criticals();
    let log = gui.gui_log();
    let errors: Vec<&str> = log.lines().filter(|l| l.contains(" ERROR ")).collect();
    assert!(errors.is_empty(), "ERROR lines in the GUI log: {errors:#?}");
}

/// Closes window `n` as a window manager would (`WM_DELETE_WINDOW`): it
/// quits with 0.
fn close(nested: &mut Nested, n: usize) {
    nested.close_window();
    let gui = nested.window(n);
    assert_eq!(gui.wait_exit(EXIT), Some(0), "log {}", gui.log.display());
}

/// `qrec status --json`'s data.
fn status(nested: &Nested, project: &Path) -> Value {
    nested.cli_json(project, &["status"])["data"].clone()
}

/// The record phase.
fn phase(nested: &Nested, project: &Path) -> String {
    status(nested, project)["runtime"]["record"]["phase"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn wait_phase(nested: &Nested, project: &Path, want: &str, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let now = phase(nested, project);
        if now == want {
            return;
        }
        assert!(Instant::now() < deadline, "never {want} (is {now})");
        thread::sleep(Duration::from_millis(100));
    }
}

/// The takes' ids.
fn takes(nested: &Nested, project: &Path) -> Vec<String> {
    let list = nested.cli_json(project, &["take", "list"]);
    list["data"]
        .as_array()
        .unwrap_or_else(|| panic!("take list: {list}"))
        .iter()
        .filter_map(|t| t["id"].as_str().map(str::to_owned))
        .collect()
}

/// The variables of a running process (`/proc/<pid>/environ`).
fn environ(pid: u32) -> BTreeMap<String, String> {
    let bytes = fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    bytes
        .split(|&b| b == 0)
        .filter_map(|var| {
            let var = String::from_utf8_lossy(var);
            let (key, value) = var.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// BRIEF §2.1's top-level layout for an editor of `width`×`height`
/// logical px: the header and the hint bar across, the panel on the right,
/// the timeline under the stage. At 1600×1000 it is [`BRIEF_REGIONS`]'s.
fn brief_layout(width: f64, height: f64) -> [(&'static str, [f64; 4]); 5] {
    let brief = |name: &str| {
        BRIEF_REGIONS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("BRIEF has no {name}"))
    };
    let header = brief("region-header")[3];
    let hintbar = brief("region-hintbar")[3];
    let panel = brief("region-panel")[2];
    let timeline = brief("region-timeline")[3];
    let left = width - panel;
    [
        ("region-header", [0.0, 0.0, width, header]),
        (
            "region-stage",
            [0.0, header, left, height - header - timeline - hintbar],
        ),
        (
            "region-panel",
            [left, header, panel, height - header - hintbar],
        ),
        (
            "region-timeline",
            [0.0, height - hintbar - timeline, left, timeline],
        ),
        ("region-hintbar", [0.0, height - hintbar, width, hintbar]),
    ]
}

/// The top-level regions tile the `width`×`height` editor whatever its
/// layout (G§3.15): the header and the hint bar across it, the panel down
/// its right edge between them, the stage over the timeline left of it.
fn assert_tiles(regions: &BTreeMap<String, [f64; 4]>, width: f64, height: f64) {
    let get = |name: &str| {
        regions
            .get(name)
            .copied()
            .unwrap_or_else(|| panic!("no {name}: {regions:#?}"))
    };
    let [hx, hy, hw, hh] = get("region-header");
    let [bx, by, bw, bh] = get("region-hintbar");
    let [px, py, pw, ph] = get("region-panel");
    let [sx, sy, sw, sh] = get("region-stage");
    let [tx, ty, tw, th] = get("region-timeline");
    let near = |a: f64, b: f64| (a - b).abs() <= 1.0;
    let rules = [
        (
            "the header across the top",
            near(hx, 0.0) && near(hy, 0.0) && near(hw, width),
        ),
        (
            "the hint bar across the bottom",
            near(bx, 0.0) && near(by + bh, height) && near(bw, width),
        ),
        (
            "the panel down the right edge",
            near(px + pw, width) && near(py, hh) && near(py + ph, by),
        ),
        (
            "the stage under the header",
            near(sx, 0.0) && near(sy, hh) && near(sw, px),
        ),
        (
            "the timeline under the stage",
            near(tx, 0.0) && near(ty, sy + sh) && near(tw, px) && near(ty + th, by),
        ),
    ];
    let broken: Vec<&str> = rules
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(rule, _)| *rule)
        .collect();
    assert!(
        broken.is_empty(),
        "the regions don't tile the {width}×{height} editor: {broken:?}\n{regions:#?}"
    );
}

/// The editor's size inside its window: the window's own bounds (its
/// test channel's `qrec`, whose (0, 0) is the editor's corner) less the
/// frame GTK draws around it with no compositing manager, as wide on each
/// side.
fn editor_size(nested: &Nested, n: usize) -> (f64, f64) {
    let [x, y, w, h] = nested.gui(n).regions(&["qrec"])["qrec"];
    (w + 2.0 * x, h + 2.0 * y)
}

/// A region's pixels in a shot: its window rect (logical px), `inset` px in
/// from each edge, on the screen from the window's `origin` (device px).
fn shot_rect(rect: [f64; 4], origin: (f64, f64), scale: f64, inset: f64) -> [u32; 4] {
    let px = |v: f64| v.round().max(0.0) as u32;
    [
        px(origin.0 + (rect[0] + inset) * scale),
        px(origin.1 + (rect[1] + inset) * scale),
        px((rect[2] - 2.0 * inset) * scale),
        px((rect[3] - 2.0 * inset) * scale),
    ]
}

/// The mean color of `pixels` and the share of them within `tolerance` of
/// `color` on every channel.
fn mean_and_share(pixels: &[[u8; 3]], color: [u8; 3], tolerance: u8) -> ([f64; 3], f64) {
    let mut sum = [0.0; 3];
    let mut near = 0usize;
    for p in pixels {
        for (s, v) in sum.iter_mut().zip(p) {
            *s += f64::from(*v);
        }
        if p.iter().zip(color).all(|(a, b)| a.abs_diff(b) <= tolerance) {
            near += 1;
        }
    }
    let n = pixels.len().max(1) as f64;
    (sum.map(|s| s / n), near as f64 / n)
}

// ------------------------------------------------------------------ cases

/// Only `DISPLAY` (with its cookie) names a display: no `GDK_BACKEND`, no
/// Wayland display. The window picks X11 itself, opens on the private
/// server (its log names GdkX11Display and the display) at 0,0 at its
/// default size, and closes as a window manager closes it, with 0.
#[test]
#[ignore = "needs a private Xvfb (Xvfb, xdotool, xclip): G§8.4"]
fn the_window_opens_on_x11_with_only_display_set() {
    let mut nested = Nested::start(AREA, "only-display", options());
    let project = nested.env.empty_dir("rust-traits-talk");
    open(&mut nested, &project, Launch::a11y().session_backend());
    let display = nested.display.clone();
    let vars = environ(nested.gui(0).pid());
    assert_eq!(vars.get("DISPLAY"), Some(&display), "{vars:#?}");
    for key in ["GDK_BACKEND", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"] {
        assert_eq!(vars.get(key), None, "{key} is set");
    }
    let log = nested.gui(0).gui_log();
    let picked = format!("the window opens on the X11 display \"{display}\" (DISPLAY)");
    assert!(
        log.lines().any(|l| l.ends_with(&picked)),
        "{picked:?} in the GUI log:\n{log}"
    );
    let opened = format!("the window's display: GdkX11Display {display}");
    assert!(log.contains(&opened), "{opened:?} in the GUI log:\n{log}");
    let (width, height) = qrec_gui::window::DEFAULT_SIZE;
    assert_eq!(
        nested.window_rect(),
        Some([0.0, 0.0, f64::from(width), f64::from(height)]),
        "the default size at 0,0"
    );
    nested.screenshot("open");
    assert_clean(&mut nested, 0);
    close(&mut nested, 0);
}

/// Cairo and GL on llvmpipe both draw the empty state (an empty folder:
/// S01a's first run, the timeline's "This swamp is quiet."): the editor's
/// regions are BRIEF's layout for its size inside GTK's frame, the same
/// for both, and each region of the two shots is its theme ground (most of
/// its pixels within 2 levels) with the same mean color within 3 levels.
#[test]
#[ignore = "needs a private Xvfb (Xvfb, xdotool, xclip): G§8.4"]
fn cairo_and_llvmpipe_gl_draw_the_empty_state() {
    let mut nested = Nested::start(AREA, "renderers", options());
    let project = nested.env.empty_dir("rust-traits-talk");
    let scale = nested.opts.scale;
    let mut drawn = Vec::new();
    for (n, (name, gl)) in [("cairo", false), ("gl", true)].into_iter().enumerate() {
        let mut launch = Launch::a11y();
        if gl {
            launch.unset.push("GDK_DISABLE".to_owned());
            launch = launch.with_env("LIBGL_ALWAYS_SOFTWARE", "1");
        } else {
            launch = launch.with_env("GDK_DISABLE", "gl,vulkan");
        }
        open_within(&mut nested, n, &project, launch, Duration::from_secs(60));
        let gui = nested.window(n);
        let renderer = gui.ctl(json!({"op": "fonts"}))["renderer"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let want = if gl {
            "GskGLRenderer"
        } else {
            "GskCairoRenderer"
        };
        assert_eq!(renderer, want, "{name}");
        let stage = gui.labels("region-stage");
        assert!(
            stage.iter().any(|l| l == "What should Qrec record?"),
            "{name}: the first run: {stage:?}"
        );
        let timeline = gui.labels("region-timeline");
        assert!(
            timeline.iter().any(|l| l == "This swamp is quiet."),
            "{name}: the empty timeline: {timeline:?}"
        );
        let regions = gui.regions(&[]);
        let (width, height) = editor_size(&nested, n);
        assert_regions(&regions, &brief_layout(width, height), &WP0B_REGIONS, scale);
        let origin = nested.x11().origin(nested.gui(n));
        let shot = nested.screenshot(name);
        assert_clean(&mut nested, n);
        close(&mut nested, n);
        drawn.push((regions, origin, shot));
    }
    let (cairo, gl) = (&drawn[0], &drawn[1]);
    assert_eq!(cairo.0, gl.0, "the same regions");
    assert_eq!(cairo.1, gl.1, "the same origin");
    let grounds = [
        ("region-header", "surface"),
        ("region-stage", "bg"),
        ("region-panel", "surface"),
        ("region-timeline", "bg"),
        ("region-hintbar", "surface"),
    ];
    for (region, ground) in grounds {
        let rect = shot_rect(cairo.0[region], cairo.1, scale, 2.0);
        let color = token_rgb("swamp-night", ground);
        let (cairo_mean, cairo_share) = mean_and_share(&rgb_pixels(&cairo.2, rect), color, 2);
        let (gl_mean, gl_share) = mean_and_share(&rgb_pixels(&gl.2, rect), color, 2);
        for (renderer, share) in [("cairo", cairo_share), ("gl", gl_share)] {
            assert!(
                share > 0.5,
                "{renderer}: {region}'s ground is {ground} on {:.0} % of it",
                share * 100.0
            );
        }
        let apart = cairo_mean
            .iter()
            .zip(gl_mean)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        assert!(
            apart <= 3.0,
            "{region}: Cairo's mean {cairo_mean:?}, GL's {gl_mean:?} ({apart:.1} apart): {} {}",
            cairo.2.display(),
            gl.2.display()
        );
    }
}

/// GUI-08: the window's X11 class is the desktop entry's `StartupWMClass`
/// and its `_NET_WM_ICON` the app icon at the hicolor sizes, so docks and
/// taskbars find the entry and draw the icon.
#[test]
#[ignore = "needs a private Xvfb (Xvfb, xdotool, xclip): G§8.4"]
fn the_window_has_its_class_and_icon() {
    let mut nested = Nested::start(AREA, "class-icon", options());
    let project = nested.env.empty_dir("rust-traits-talk");
    open(&mut nested, &project, Launch::a11y());
    let pid = nested.gui(0).pid();
    let x11 = nested.x11();
    let (id, _) = x11.toplevel(pid).expect("the window's X window");
    let props = x11.xprop(id, &["WM_CLASS", "_NET_WM_PID", "_NET_WM_ICON"]);
    let id_class = qrec_gui::app::APP_ID;
    let class = format!("WM_CLASS(STRING) = \"{id_class}\", \"{id_class}\"");
    assert!(props.contains(&class), "{class:?} in:\n{props}");
    assert!(
        props.contains(&format!("_NET_WM_PID(CARDINAL) = {pid}")),
        "{props}"
    );
    // `Icon (16 x 16):` before each one's picture, the first on the
    // property's line.
    let sizes: BTreeSet<(u32, u32)> = props
        .lines()
        .filter_map(|l| {
            let rest = &l[l.find("Icon (")? + "Icon (".len()..];
            let (w, rest) = rest.split_once(" x ")?;
            let (h, _) = rest.split_once(')')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .collect();
    for size in [16, 32, 48, 64, 128] {
        assert!(
            sizes.contains(&(size, size)),
            "a {size} px icon in {sizes:?}"
        );
    }
    assert_clean(&mut nested, 0);
    close(&mut nested, 0);
}

/// R30 on X11: holding `R` past the server's repeat delay (Xvfb's 660 ms,
/// 25 a second) starts one take and leaves it recording. The server sends
/// the repeats as presses with no release between (XKB's detectable
/// auto-repeat, which GTK asks for), and the key router takes them for
/// repeats: the GUI log has one press of `r` and the repeats after it.
#[test]
#[ignore = "needs a private Xvfb (Xvfb, xdotool, xclip): G§8.4"]
fn holding_r_records_one_take() {
    let mut nested = Nested::start(AREA, "held-r", options());
    let project = fixture(&nested, DesignVariant::Base, &live_ui());
    open(&mut nested, &project, Launch::a11y());
    let before = takes(&nested, &project).len();
    nested.keys(&["-P", "r", "-s", "1500", "-p", "r"]);
    nested.window(0).wait_idle();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let now = phase(&nested, &project);
        if now != "starting" {
            assert_eq!(now, "recording", "{}", status(&nested, &project));
            break;
        }
        assert!(Instant::now() < deadline, "still starting");
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(takes(&nested, &project).len(), before + 1, "one take");
    let toasts = nested.window(0).labels("toast-host");
    assert!(toasts.is_empty(), "{toasts:?}");
    let log = nested.gui(0).gui_log();
    let r: Vec<&str> = log.lines().filter(|l| l.contains(" key r (")).collect();
    let repeats = r.iter().filter(|l| l.contains("repeat true")).count();
    assert_eq!(r.len() - repeats, 1, "one press of r: {r:#?}");
    assert!(
        repeats >= 5,
        "the held r repeated ({repeats} repeats): {r:#?}"
    );
    let stop = nested.cli_json(&project, &["record", "stop"]);
    assert_eq!(stop["ok"], true, "{stop}");
    wait_phase(&nested, &project, "idle", Duration::from_secs(30));
    assert_eq!(takes(&nested, &project).len(), before + 1, "it landed");
    nested.window(0).wait_idle();
    assert_clean(&mut nested, 0);
    close(&mut nested, 0);
}

/// GUI-04 on X11 (Copy image through GDK's clipboard, no `wl-copy`): `p`
/// saves the frame at the playhead, and its toast's Copy image puts that
/// PNG on the X clipboard, read back with `xclip`: its exact bytes as
/// image/png, beside a URI list. With no clipboard manager to take it at
/// the quit, the window still quits with 0 (the hand-off waits 1 s at
/// most).
#[test]
#[ignore = "needs a private Xvfb (Xvfb, xdotool, xclip): G§8.4"]
fn copy_image_reaches_the_x_clipboard() {
    let mut nested = Nested::start(AREA, "copy-image", options());
    let project = fixture(&nested, DesignVariant::Base, &program_ui());
    open(&mut nested, &project, Launch::a11y());
    nested.keys(&["-k", "p"]);
    let button = poll(Duration::from_secs(30), "the frame's toast", || {
        (0..2)
            .map(|n| format!("toast-action-{n}"))
            .find(|name| nested.gui(0).labels(name).iter().any(|l| l == "Copy image"))
    });
    // Settled: the toast is laid out where its region says (a click as its
    // label first showed hit the toast before it had a place).
    nested.window(0).wait_idle();
    let frames: Vec<PathBuf> = fs::read_dir(project.join("frames"))
        .expect("frames/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "png"))
        .collect();
    assert_eq!(frames.len(), 1, "{frames:?}");
    nested.x11().click(nested.gui(0), &button);
    nested.window(0).wait_idle();
    let png = poll(Duration::from_secs(15), "the X clipboard's PNG", || {
        nested
            .x11()
            .clipboard("image/png")
            .filter(|bytes| bytes.starts_with(b"\x89PNG"))
    });
    let saved = fs::read(&frames[0]).expect("the frame");
    assert!(
        png == saved,
        "the clipboard isn't the saved frame ({} bytes for {})",
        png.len(),
        saved.len()
    );
    let targets = nested.x11().clipboard("TARGETS").unwrap_or_default();
    let targets = String::from_utf8_lossy(&targets);
    for target in ["image/png", "text/uri-list"] {
        assert!(
            targets.lines().any(|t| t == target),
            "{target} in {targets}"
        );
    }
    nested.window(0).wait_idle();
    let toasts = nested.window(0).labels("toast-host");
    assert!(
        !toasts.iter().any(|l| l == "Couldn't copy the image"),
        "{toasts:?}"
    );
    assert_clean(&mut nested, 0);
    close(&mut nested, 0);
}

/// `Ctrl+Q` quits cleanly on X11: 0 within the deadline, the lock free,
/// no ERROR line, no panic, no critical.
#[test]
#[ignore = "needs a private Xvfb (Xvfb, xdotool, xclip): G§8.4"]
fn ctrl_q_quits_cleanly() {
    let mut nested = Nested::start(AREA, "ctrl-q", options());
    let project = fixture(&nested, DesignVariant::Base, &live_ui());
    open(&mut nested, &project, Launch::a11y());
    assert_clean(&mut nested, 0);
    nested.keys(&["-M", "ctrl", "-k", "q", "-m", "ctrl"]);
    let gui = nested.window(0);
    assert_eq!(gui.wait_exit(EXIT), Some(0), "log {}", gui.log.display());
    let log = gui.gui_log();
    assert!(log.contains("quitting with 0"), "the quit line:\n{log}");
    assert!(
        !log.contains("panicked") && !gui.output().contains("panicked"),
        "a panic: {}",
        gui.log.display()
    );
    let errors: Vec<&str> = log.lines().filter(|l| l.contains(" ERROR ")).collect();
    assert!(errors.is_empty(), "ERROR lines in the GUI log: {errors:#?}");
    gui.assert_no_criticals();
    let after = nested.cli_json(&project, &["status"]);
    assert_eq!(
        after["data"]["host"],
        Value::Null,
        "the lock is free: {after}"
    );
}

/// GUI-16 on X11: on a 1366×768 screen the first window asks for 90 % of
/// it (1229×691, `window::first_size`), opens at 0,0 within the screen,
/// and the editor fills it inside GTK's frame, in the layout for its size.
#[test]
#[ignore = "needs a private Xvfb (Xvfb, xdotool, xclip): G§8.4"]
fn the_first_window_fits_a_1366_by_768_screen() {
    let mut nested = Nested::start(AREA, "small-screen", Options::size(1366, 768).x11());
    let project = nested.env.empty_dir("rust-traits-talk");
    open(&mut nested, &project, Launch::a11y());
    let rect = nested.window_rect().expect("the window's X window");
    assert_eq!(rect, [0.0, 0.0, 1229.0, 691.0], "90 % of the screen");
    let (screen_w, screen_h) = nested.opts.logical();
    assert!(
        rect[0] + rect[2] <= screen_w && rect[1] + rect[3] <= screen_h,
        "{rect:?} within {screen_w}×{screen_h}"
    );
    let log = nested.gui(0).gui_log();
    let asked =
        "the window asks for 1229×691 (its default size), to fit the monitors [(1366, 768)]";
    assert!(log.contains(asked), "{asked:?} in the GUI log:\n{log}");
    let (width, height) = editor_size(&nested, 0);
    assert!(
        width < rect[2] && height < rect[3],
        "the editor {width}×{height} inside its window's frame"
    );
    assert_tiles(&nested.gui(0).regions(&[]), width, height);
    nested.screenshot("open");
    assert_clean(&mut nested, 0);
    close(&mut nested, 0);
}
