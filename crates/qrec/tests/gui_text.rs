//! The window's text setup (docs/GUI.md G§3.14, WP10's text-rendering
//! experiment): GTK's manual font rendering with hinted metrics, and what
//! that does to the bundled fonts' advances.
//!
//! `cargo test -p qrec --test gui_text -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon). Screenshots and
//! logs go to `tmp/gui-shots/text/`.

mod gui_support;

use std::time::Duration;

use gui_support::{FONT_RENDERING, Launch, Nested, Options, wait_for_gui_host};
use qrec_shell::testkit::design::{DesignVariant, design_fixture};
use serde_json::{Value, json};

const OPEN: Duration = Duration::from_secs(60);
const AREA: &str = "text";

/// Qrec's own text setup (an empty `QREC_GUI_TEST_FONT_RENDERING`), also
/// when the test run names another for every window.
const QREC: &str = "";
/// GTK 4.22's automatic mode, whose stray mark manual mode removed.
const AUTOMATIC: &str = "mode=automatic";

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
    reply
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
