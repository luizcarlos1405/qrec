//! The window's text setup (docs/GUI.md G§3.14, WP10's text-rendering
//! experiment): GTK's manual font rendering with hinted metrics, what that
//! does to the bundled fonts' advances, and why it stays: GTK's other setups
//! bring back the header title's stray mark on GSK's GL renderer.
//!
//! `cargo test -p qrec --test gui_text -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon). Screenshots and
//! logs go to `tmp/gui-shots/text/`.
//!
//! The GL case runs its windows on GSK's GL renderer in software (Mesa's
//! llvmpipe, [`SOFTWARE_GL`]) under any compositor renderer, pixman too: the
//! mark only ever showed on GL, and llvmpipe opens no GPU node (the WP10
//! experiment traced every open of its runs: none under `/dev/dri` but
//! Qrec's own listing of the folder).

mod gui_support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use gui_support::{FONT_RENDERING, Launch, Nested, Options, wait_for_gui_host};
use qrec_shell::testkit::design::{DesignVariant, design_fixture};
use qrec_shell::testkit::run_tool;
use serde_json::{Value, json};

const OPEN: Duration = Duration::from_secs(60);
const AREA: &str = "text";

/// Qrec's own text setup (an empty `QREC_GUI_TEST_FONT_RENDERING`), also
/// when the test run names another for every window.
const QREC: &str = "";
/// GTK 4.22's automatic mode, whose stray mark manual mode removed.
const AUTOMATIC: &str = "mode=automatic";

/// The window on GSK's GL renderer in software: Mesa's llvmpipe on two
/// threads (GTK skips llvmpipe unless `GSK_RENDERER` names GL), Vulkan off.
const SOFTWARE_GL: [(&str, &str); 5] = [
    ("GDK_DISABLE", "vulkan"),
    ("GSK_RENDERER", "gl"),
    ("LIBGL_ALWAYS_SOFTWARE", "1"),
    ("GALLIUM_DRIVER", "llvmpipe"),
    ("LP_NUM_THREADS", "2"),
];

/// A folder whose name, the header's title while the project opens, has
/// accented capitals: the tallest ink the title draws. GL draws a hinted
/// font's glyphs on a whole-pixel baseline, while the bounds that limit a
/// partial redraw are the unhinted ink at the exact baseline; with
/// fractional metrics the accents' tops fall a row above them (12 px in
/// GTK's automatic mode). gui_shell's all-ascender `hhhh…` shows nothing
/// since the header's WP6 layout, in any setup but unhinted fractional
/// metrics (4 px).
const TALL_TITLE: &str = "ÅÉÂÊÔÛÅÉÂÊÔÛ";

/// `JetBrains Mono` 500 digits at `size` px: ten advances.
fn digits(size: f64) -> Value {
    json!({"family": "JetBrains Mono", "sizePx": size, "weight": 500, "text": "0123456789"})
}

/// The design fixture's window with the text setup `spec`: the `fonts`
/// reply, with the mono roles' digits measured (11, 12 and 13 px).
fn fonts(case: &str, spec: &str) -> Value {
    let mut nested = Nested::start(AREA, case, Options::default());
    let project = nested.env.empty_dir("rust-traits-talk");
    design_fixture(&project, DesignVariant::Base);
    nested.launch(&project, Launch::default().with_env(FONT_RENDERING, spec));
    wait_for_gui_host(&nested, &project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
    let reply = gui.ctl(json!({
        "op": "fonts",
        "measure": [digits(11.0), digits(12.0), digits(13.0)],
    }));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_clean(gui);
    reply
}

/// No panic caught and no GLib or GTK critical (the nested suites' rule).
fn assert_clean(gui: &gui_support::Gui) {
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    gui.assert_no_criticals();
}

/// Each measured text's advance: its logical width over its characters.
fn advances(reply: &Value) -> Vec<f64> {
    reply["widths"]
        .as_array()
        .expect("widths")
        .iter()
        .map(|w| {
            let chars = w["text"].as_str().unwrap_or_default().chars().count() as f64;
            w["logical"].as_f64().unwrap_or_default() / chars
        })
        .collect()
}

fn assert_advances(got: &[f64], want: [f64; 3], what: &str) {
    assert!(
        got.len() == 3 && got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.01),
        "{what}: JetBrains Mono advances {got:?} px at 11, 12, 13 px, not {want:?}"
    );
}

/// G§3.14: Qrec's text setup is GTK's manual mode with hinted metrics, so
/// every glyph advances a whole pixel (the mono roles' 7, 7 and 8 px where
/// the frames advance 6.6, 7.2 and 7.8: the experiment kept it). The test
/// hook names the others: metrics off (the frames' fractional advances)
/// and GTK's automatic mode.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_text_setup_is_manual_with_whole_pixel_advances() {
    let qrec = fonts("setup-qrec", QREC);
    let rendering = &qrec["rendering"];
    assert_eq!(rendering["mode"], "manual", "{qrec}");
    assert_eq!(rendering["hintMetrics"], true, "{qrec}");
    assert_advances(&advances(&qrec), [7.0, 7.0, 8.0], "Qrec's setup");

    let fractional = fonts("setup-fractional", "metrics=off,hintstyle=slight");
    let rendering = &fractional["rendering"];
    assert_eq!(rendering["mode"], "manual", "{fractional}");
    assert_eq!(rendering["hintMetrics"], false, "{fractional}");
    assert_eq!(rendering["hintStyle"], "hintslight", "{fractional}");
    assert_advances(&advances(&fractional), [6.6, 7.2, 7.8], "metrics off");

    let automatic = fonts("setup-automatic", AUTOMATIC);
    assert_eq!(automatic["rendering"]["mode"], "automatic", "{automatic}");
    assert_advances(&advances(&automatic), [6.6, 7.2, 7.8], "automatic");
}

/// The header title after a partial redraw (the folder's name while the
/// project opens, then the project's) against a full redraw's
/// (`GSK_DEBUG=full-redraw`), on software GL with the text setup `spec`:
/// the differing pixels of the title's crop, the shots and each window's
/// `fonts` reply.
fn title_trace(case: &str, spec: &str) -> (usize, Vec<PathBuf>, Vec<Value>) {
    let crop = |shot: &Path| -> Vec<u8> {
        let out = run_tool(
            Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(shot)
                .args([
                    "-vf",
                    "crop=480:48:560:0,format=rgb24",
                    "-f",
                    "rawvideo",
                    "-",
                ]),
            Duration::from_secs(30),
        );
        out.stdout
    };
    let mut shots = Vec::new();
    let mut fonts = Vec::new();
    for (n, debug) in [(1, None), (2, Some("full-redraw"))] {
        let mut nested = Nested::start(AREA, &format!("{case}-{n}"), Options::default());
        let project = nested.env.empty_dir(TALL_TITLE);
        design_fixture(&project, DesignVariant::Base);
        let mut launch = Launch::default().with_env(FONT_RENDERING, spec);
        for (key, value) in SOFTWARE_GL {
            launch = launch.with_env(key, value);
        }
        if let Some(debug) = debug {
            launch = launch.with_env("GSK_DEBUG", debug);
        }
        nested.launch(&project, launch);
        wait_for_gui_host(&nested, &project, OPEN);
        let gui = nested.window(0);
        gui.wait_phase("open", OPEN);
        gui.wait_idle();
        fonts.push(gui.ctl(json!({"op": "fonts"})));
        let split = nested.cli_json(&project, &["clip", "split", "--timeline", "--at", "2437f"]);
        assert_eq!(split["ok"], true, "{split}");
        nested.window(0).wait_idle();
        shots.push(nested.screenshot("header"));
        assert_clean(nested.window(0));
    }
    let (partial, full) = (crop(&shots[0]), crop(&shots[1]));
    assert_eq!(partial.len(), 480 * 48 * 3);
    let differing = partial
        .chunks_exact(3)
        .zip(full.chunks_exact(3))
        .filter(|(a, b)| a != b)
        .count();
    (differing, shots, fonts)
}

/// G§3.14's stray mark, on GSK's GL renderer (in software, so on any machine
/// and under pixman): with Qrec's text setup the header title after a
/// partial redraw equals a full redraw's. GTK's automatic mode, the
/// control, leaves the old title's accents above the new one, so the case
/// can see a mark (a GTK whose automatic mode stops leaving one fails it:
/// then the setup can be reconsidered).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_header_title_leaves_no_trace_on_gl() {
    let (differing, shots, fonts) = title_trace("title-qrec", QREC);
    for reply in &fonts {
        assert_eq!(reply["renderer"], "GskGLRenderer", "{reply}");
        assert_eq!(reply["rendering"]["mode"], "manual", "{reply}");
    }
    assert_eq!(
        differing,
        0,
        "the title differs from a full redraw's in {differing} px: {} vs {}",
        shots[0].display(),
        shots[1].display()
    );
    let (control, shots, fonts) = title_trace("title-automatic", AUTOMATIC);
    for reply in &fonts {
        assert_eq!(reply["renderer"], "GskGLRenderer", "{reply}");
    }
    assert!(
        control > 0,
        "GTK's automatic mode left no mark, so the case can't see one: {} vs {}",
        shots[0].display(),
        shots[1].display()
    );
}
