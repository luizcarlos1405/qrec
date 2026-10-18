//! WP10's theme proofs (docs/GUI.md WP10, BRIEF §7) in a nested headless
//! sway: S02 and S06 in all four themes, S18 and S25's toasts in Swamp Day
//! next to Swamp Night (the approved slice's theme), and the narrow
//! layout's sheet, whose shades came from libadwaita until WP10. Each
//! window starts with its theme in `config.toml` (G§3.14: loaded before
//! the first frame).
//!
//! Each shot is checked where the theme shows: BRIEF's regions, the a11y
//! walk, no panic and no critical, `local`'s theme, and the grounds and
//! marks the frames draw probed in the shot against the theme's tokens
//! (the generated `resources/theme/tokens/<theme>.css` the window loads;
//! alpha tokens composited as the screen shows them, 8-bit sRGB). Probes
//! are in logical px, read in device px (× the output's scale).
//!
//! The shots go to `tmp/gui-shots/themes/<case>-<theme>-<name>.png`, with
//! one board per case, `<case>-board.png`: the themes side by side in BRIEF
//! §7's column order (Swamp Night, Swamp Day, Dragon's Keep, Ogre Mode;
//! S18 and S25 Swamp Night and Swamp Day), S25's four toasts stacked in
//! each column (info, warning, success, error). S25's frame is a draft
//! (G§0.2): the toasts are shot as built, with the core's copy.
//!
//! `cargo test -p qrec --test gui_themes -- --ignored --test-threads=1`
//! (inside the dev shell: sway, grim, wtype, dbus-daemon, ffmpeg; pixman,
//! or GL with `QREC_TEST_WLR=gles2`).

mod gui_support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use gui_support::{Launch, Nested, Options, assert_regions, wait_for_gui_host};
use qrec_core::state::{CanvasMode, UiState};
use qrec_core::{ClipId, LayerId};
use qrec_shell::testkit::design::{DesignVariant, design_fixture, write_ui};
use qrec_shell::testkit::run_tool;
use serde_json::{Value, json};

const OPEN: Duration = Duration::from_secs(60);
const EXIT: Duration = Duration::from_secs(20);
const AREA: &str = "themes";

/// The themes in BRIEF §7's column order (`config.toml`'s ids).
const THEMES: [&str; 4] = ["swamp-night", "swamp-day", "dragons-keep", "ogre-mode"];
/// The themes BRIEF §7 draws S18 and S25 in: Swamp Day, the only light
/// theme, under Swamp Night.
const NIGHT_AND_DAY: [&str; 2] = ["swamp-night", "swamp-day"];
/// BRIEF's meter readings, pinned (G§8.4 `QREC_GUI_TEST_LEVELS`).
const DESIGN_LEVELS: &str = "alsa_input.fake=-22/-16,alsa_output.fake.monitor=-52/-48";

// ---------------------------------------------------------------- tokens

/// A color token: its RGB and alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgba([u8; 3], u8);

impl Rgba {
    /// `self` over the opaque `below`, as the screen shows it (sRGB, each
    /// channel rounded).
    fn over(self, below: [u8; 3]) -> [u8; 3] {
        let a = f64::from(self.1) / 255.0;
        let mut out = [0; 3];
        for (i, channel) in out.iter_mut().enumerate() {
            let mixed = f64::from(self.0[i]) * a + f64::from(below[i]) * (1.0 - a);
            *channel = mixed.round().clamp(0.0, 255.0) as u8;
        }
        out
    }
}

/// A theme's color tokens, from its generated CSS (what the window loads).
struct Tokens {
    theme: &'static str,
    colors: BTreeMap<String, Rgba>,
}

impl Tokens {
    fn of(theme: &'static str) -> Tokens {
        let path = gui_support::repo()
            .join("crates/qrec-gui/resources/theme/tokens")
            .join(format!("{theme}.css"));
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut colors = BTreeMap::new();
        for line in text.lines() {
            let Some((name, value)) = line
                .trim()
                .strip_prefix("--qrec-")
                .and_then(|rest| rest.split_once(':'))
            else {
                continue;
            };
            let Some(hex) = value.trim().trim_end_matches(';').strip_prefix('#') else {
                continue;
            };
            let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).expect("a hex color");
            let alpha = if hex.len() == 8 { byte(6) } else { 255 };
            colors.insert(name.to_owned(), Rgba([byte(0), byte(2), byte(4)], alpha));
        }
        Tokens { theme, colors }
    }

    fn get(&self, name: &str) -> Rgba {
        *self
            .colors
            .get(name)
            .unwrap_or_else(|| panic!("{}: no token {name}", self.theme))
    }

    /// An opaque token's RGB.
    fn rgb(&self, name: &str) -> [u8; 3] {
        let c = self.get(name);
        assert_eq!(c.1, 255, "{}: {name} has an alpha", self.theme);
        c.0
    }

    /// Alpha token `top` over opaque token `base`.
    fn over(&self, top: &str, base: &str) -> [u8; 3] {
        self.get(top).over(self.rgb(base))
    }
}

fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
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

/// `theme = "<theme>"` in the user's `config.toml`.
fn use_theme(nested: &Nested, theme: &str) {
    let config = nested.env.config_home().join("qrec");
    fs::create_dir_all(&config).expect("the config folder");
    fs::write(config.join("config.toml"), format!("theme = \"{theme}\"\n")).expect("config.toml");
}

/// A window on `project` in `theme`, open and idle, wearing it.
fn open(nested: &mut Nested, project: &Path, theme: &str, launch: Launch) {
    use_theme(nested, theme);
    nested.launch(project, launch);
    wait_for_gui_host(nested, project, OPEN);
    let gui = nested.window(0);
    gui.wait_phase("open", OPEN);
    gui.wait_idle();
    let local = gui.local();
    assert_eq!(local["theme"], theme, "{local}");
}

/// The launch every case uses: the a11y walk on, the meters pinned.
fn launch() -> Launch {
    Launch::a11y().with_env("QREC_GUI_TEST_LEVELS", DESIGN_LEVELS)
}

/// The a11y walk is empty, no panic was caught and no critical printed.
fn assert_clean(nested: &mut Nested) {
    let gui = nested.window(0);
    let reply = gui.ctl(json!({"op": "a11y"}));
    assert_eq!(reply["ok"], true, "a11y: {reply}");
    let local = gui.local();
    assert_eq!(local["panics"], 0, "{local}");
    gui.assert_no_criticals();
}

fn close(nested: &mut Nested) {
    nested.close_window();
    assert_eq!(nested.window(0).wait_exit(EXIT), Some(0));
}

/// The bounds of the widget named `name` (it must show).
fn rect(nested: &mut Nested, name: &str) -> [f64; 4] {
    let regions = nested.window(0).regions(&[name]);
    *regions
        .get(name)
        .unwrap_or_else(|| panic!("no {name}: {regions:?}"))
}

/// The center of the widget named `name`.
fn center(nested: &mut Nested, name: &str) -> (f64, f64) {
    let [x, y, w, h] = rect(nested, name);
    (x + w / 2.0, y + h / 2.0)
}

/// `[x, y, w, h]` in logical px as device px at output scale `scale`.
fn device(scale: f64, [x, y, w, h]: [f64; 4]) -> [u32; 4] {
    let at = |v: f64| (v * scale).round().max(0.0) as u32;
    [at(x), at(y), at(w).max(1), at(h).max(1)]
}

/// The pixel at (`x`, `y`) (logical px) of a shot.
fn pixel(nested: &Nested, png: &Path, x: f64, y: f64) -> [u8; 3] {
    let [x, y, _, _] = device(nested.opts.scale, [x, y, 1.0, 1.0]);
    gui_support::rgb_pixels(png, [x, y, 1, 1])[0]
}

/// A spot of a shot that must read one color.
struct Probe {
    what: &'static str,
    at: (f64, f64),
    want: [u8; 3],
}

fn probe(what: &'static str, x: f64, y: f64, want: [u8; 3]) -> Probe {
    Probe {
        what,
        at: (x, y),
        want,
    }
}

/// Every probe reads its color (±`tolerance` a channel); the failure
/// lists each one that doesn't, with what it read.
fn assert_probes(nested: &Nested, png: &Path, theme: &str, probes: &[Probe], tolerance: u8) {
    let off: Vec<String> = probes
        .iter()
        .filter_map(|p| {
            let got = pixel(nested, png, p.at.0, p.at.1);
            let near = got
                .iter()
                .zip(p.want)
                .all(|(g, w)| g.abs_diff(w) <= tolerance);
            (!near).then(|| {
                format!(
                    "{} at {:?}: {}, want {}",
                    p.what,
                    p.at,
                    hex(got),
                    hex(p.want)
                )
            })
        })
        .collect();
    assert!(
        off.is_empty(),
        "{theme}, {}:\n  {}",
        png.display(),
        off.join("\n  ")
    );
}

/// How many pixels of `rect` (logical px) read `color` within `tolerance`
/// a channel: a glyph or an icon drawn in it.
fn count_near(nested: &Nested, png: &Path, rect: [f64; 4], color: [u8; 3], tolerance: u8) -> usize {
    gui_support::count_near(png, device(nested.opts.scale, rect), color, tolerance)
}

/// The sum of the channels (a pixel's brightness, for the shadows).
fn brightness(c: [u8; 3]) -> u32 {
    c.iter().map(|&v| u32::from(v)).sum()
}

/// The header's app icon (24 px at x 12, y 12): the pen's onion in its
/// own colors in every theme (G§3.14's art; artwork.html's "Art · App
/// icon" has no themed fill), so it reads alike in both, as video does.
const APP_ICON: [f64; 4] = [12.0, 12.0, 24.0, 24.0];

/// Every color outside `video` (logical px: the preview, the filmstrips,
/// what is drawn over video) and the app icon follows the theme: Swamp
/// Night and Swamp Day share no color token, so a spot that reads the same
/// in both shots of a case is drawn in a color no token gives: one
/// hard-coded in custom drawing (`gdk::RGBA::BLACK`) or in CSS (`white`),
/// which the tokens test's hex grep doesn't see. Spots, not single
/// pixels: 8×8 cells with 4 or more pixels alike (an antialiased edge may
/// match by chance; none did in any shot). A hard-coded hairline at a
/// fractional x mixes with each theme's ground and reads apart: the
/// probes are for those.
fn assert_follows_the_theme(opts: &Options, night: &Path, day: &Path, video: &[[f64; 4]]) {
    let (w, h) = opts.logical();
    let full = device(opts.scale, [0.0, 0.0, w, h]);
    let (a, b) = (
        gui_support::rgb_pixels(night, full),
        gui_support::rgb_pixels(day, full),
    );
    let width = full[2] as usize;
    let out: Vec<[u32; 4]> = video
        .iter()
        .chain([&APP_ICON])
        .map(|r| device(opts.scale, *r))
        .collect();
    let mut cells: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (i, (pa, pb)) in a.iter().zip(&b).enumerate() {
        if pa != pb {
            continue;
        }
        let (x, y) = ((i % width) as u32, (i / width) as u32);
        if out
            .iter()
            .any(|[rx, ry, rw, rh]| (*rx..rx + rw).contains(&x) && (*ry..ry + rh).contains(&y))
        {
            continue;
        }
        *cells
            .entry((x as usize / 8 * 8, y as usize / 8 * 8))
            .or_default() += 1;
    }
    let spots: Vec<String> = cells
        .iter()
        .filter(|(_, n)| **n >= 4)
        .map(|((x, y), n)| format!("({x}, {y}): {n} px"))
        .collect();
    assert!(
        spots.is_empty(),
        "colors that don't follow the theme (alike in {} and {}), device px:\n  {}",
        night.display(),
        day.display(),
        spots.join("\n  ")
    );
}

/// The suite's shots: `<repo>/tmp/gui-shots/themes/`, or
/// `tmp/gui-shots/scale/<s>/themes/` in a suite-wide scale run
/// (`gui_support::shots_dir`; `Nested::start` makes it).
fn shots_dir() -> PathBuf {
    gui_support::shots_dir(AREA)
}

/// `<case>-board.png`: one column per theme, each its shots top to
/// bottom, cropped to `crop` (logical px at output scale `scale`) when
/// given.
fn board(scale: f64, case: &str, columns: &[Vec<PathBuf>], crop: Option<[f64; 4]>) -> PathBuf {
    let out = shots_dir().join(format!("{case}-board.png"));
    let mut command = Command::new("ffmpeg");
    command.args(["-v", "error", "-y"]);
    let mut filter = String::new();
    let mut n = 0;
    let mut stacks = Vec::new();
    for (c, column) in columns.iter().enumerate() {
        let mut cells = String::new();
        for shot in column {
            command.arg("-i").arg(shot);
            match crop {
                Some(rect) => {
                    let [x, y, w, h] = device(scale, rect);
                    filter.push_str(&format!("[{n}]crop={w}:{h}:{x}:{y}[s{n}];"));
                }
                None => filter.push_str(&format!("[{n}]null[s{n}];")),
            }
            cells.push_str(&format!("[s{n}]"));
            n += 1;
        }
        if column.len() > 1 {
            filter.push_str(&format!("{cells}vstack=inputs={}[c{c}];", column.len()));
        } else {
            filter.push_str(&format!("{cells}null[c{c}];"));
        }
        stacks.push(format!("[c{c}]"));
    }
    if stacks.len() > 1 {
        filter.push_str(&format!(
            "{}hstack=inputs={}",
            stacks.join(""),
            stacks.len()
        ));
    } else {
        filter.push_str(&format!("{}null", stacks.join("")));
    }
    command.args(["-filter_complex", &filter]).arg(&out);
    let run = run_tool(&mut command, gui_support::TOOL_TIMEOUT);
    assert!(
        run.status.success(),
        "the board {}: {}",
        out.display(),
        run.stderr
    );
    out
}

/// How far a composited probe may read from the 8-bit model: the renderer
/// rounds its blends its own way (pixman read #102B22 for #112B23; GL
/// blends in floats, given more room unmeasured here).
fn blend(opts: &Options) -> u8 {
    match opts.renderer {
        gui_support::Renderer::Pixman => 2,
        gui_support::Renderer::Gles2 => 4,
    }
}

/// The x of frame `frame` on the 7,391-frame design timeline at fit zoom
/// (G§4.5: (1040 − 8) / 7391 px a frame from the time area's x 176).
fn frame_x(frame: u64) -> f64 {
    176.0 + frame as f64 * (1040.0 - qrec_core::view::timeline::FIT_INSET_PX) / 7391.0
}

/// What S02 and S06 draw from video at 1600×1000: the preview and the
/// video row's filmstrips (with their take label plates).
const VIDEO: [[f64; 4]; 2] = [[128.0, 64.0, 960.0, 540.0], [176.0, 720.0, 1040.0, 76.0]];

/// The opaque grounds and marks every main-window case shows at 1600×1000
/// (BRIEF §2.1, DESIGN Layout): the header, the stage, the panel, the
/// timeline's track headers and lanes, the hint bar, Record's fill and
/// the playhead at frame `playhead` (its 2 px line over lane A2).
fn grounds(t: &Tokens, playhead: u64) -> Vec<Probe> {
    vec![
        probe("the header's surface", 420.0, 4.0, t.rgb("surface")),
        probe("the stage's bg", 40.0, 300.0, t.rgb("bg")),
        probe("the panel's surface", 1592.0, 940.0, t.rgb("surface")),
        probe("the track headers' surface", 90.0, 940.0, t.rgb("surface")),
        probe("the lanes' bg", 600.0, 950.0, t.rgb("bg")),
        probe(
            "the playhead over A2",
            frame_x(playhead),
            900.0,
            t.rgb("playhead"),
        ),
        probe("Record's fill", 1244.0, 24.0, t.rgb("record")),
        probe("the hint bar's surface", 1596.0, 976.0, t.rgb("surface")),
    ]
}

// ------------------------------------------------------------------- S02

/// S02's preset: Live, the playhead at the end.
fn s02_ui() -> UiState {
    UiState {
        playhead: 7391,
        ..UiState::default()
    }
}

/// S02 in every theme: Live idle on the design fixture, BRIEF's regions,
/// the live picture, the grounds and marks in the theme's tokens, and no
/// color outside video alike in Swamp Night and Swamp Day.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s02_live_idle_in_every_theme() {
    let opts = Options::default();
    let mut shots = Vec::new();
    for theme in THEMES {
        let mut nested = Nested::start(AREA, &format!("S02-{theme}"), opts.clone());
        let project = fixture(&nested, DesignVariant::Base, &s02_ui());
        open(&mut nested, &project, theme, launch());
        let regions = nested.window(0).regions(&[]);
        assert_regions(
            &regions,
            &gui_support::BRIEF_REGIONS,
            &gui_support::MERGED_REGIONS,
            opts.scale,
        );
        assert_clean(&mut nested);
        let shot = nested.screenshot("live");
        let grey = gui_support::mean_grey(&shot, device(opts.scale, VIDEO[0]));
        assert!(
            grey > 40.0,
            "{theme}: the live picture is black ({grey:.1})"
        );
        let t = Tokens::of(theme);
        let mut probes = grounds(&t, 7391);
        probes.extend([
            probe("Live, the selected segment", 118.0, 24.0, t.rgb("raised")),
            probe("the mode switch's field well", 280.0, 24.0, t.rgb("field")),
            probe("A1's meter track (field)", 1500.0, 341.0, t.rgb("field")),
        ]);
        assert_probes(&nested, &shot, theme, &probes, 0);
        assert_probes(
            &nested,
            &shot,
            theme,
            &[probe(
                "A2's take 5 tint under c2d0",
                520.0,
                870.0,
                t.over("take-lily-tint", "bg"),
            )],
            blend(&nested.opts),
        );
        shots.push(vec![shot]);
        close(&mut nested);
    }
    board(opts.scale, "S02", &shots, None);
    assert_follows_the_theme(&opts, &shots[0][0], &shots[1][0], &VIDEO);
}

// ------------------------------------------------------------------- S06

/// S06's preset: Program, c2d0 selected, the playhead at 2437, Face (L2)
/// selected (the inspector the frame draws), as gui_panel's S06.
fn s06_ui() -> UiState {
    UiState {
        canvas_mode: CanvasMode::Program,
        selection: vec![ClipId::from_raw("c2d0")],
        playhead: 2437,
        selected_layer: Some(LayerId::numbered(2)),
        ..UiState::default()
    }
}

/// The x where `clip` starts on the video row at fit zoom (window px), from
/// the clip list.
fn clip_start_x(nested: &Nested, project: &Path, clip: &str) -> f64 {
    let list = nested.cli_json(project, &["clip", "list"]);
    let clips = list["data"].as_array().cloned().unwrap_or_default();
    let frames = |c: &Value, key: &str| c[key].as_u64().unwrap_or(0) as f64;
    let total = clips
        .iter()
        .map(|c| frames(c, "start") + frames(c, "duration"))
        .fold(0.0, f64::max);
    let c = clips
        .iter()
        .find(|c| c["id"] == clip)
        .unwrap_or_else(|| panic!("no {clip}: {list}"));
    176.0 + frames(c, "start") * (1040.0 - qrec_core::view::timeline::FIT_INSET_PX) / total
}

/// S06 in every theme: Program with c2d0 selected and the pointer over
/// c2d1 for its tooltip, as the frame: the selection (the clip's accent
/// handle, the layer row's R1 tint), the playhead, the take tints and the
/// tooltip in each theme's tokens; no color outside video alike in Swamp
/// Night and Swamp Day.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s06_program_with_selection_in_every_theme() {
    let opts = Options::default();
    let mut shots = Vec::new();
    for theme in THEMES {
        let mut nested = Nested::start(AREA, &format!("S06-{theme}"), opts.clone());
        let project = fixture(&nested, DesignVariant::Base, &s06_ui());
        open(&mut nested, &project, theme, launch());
        let regions = nested.window(0).regions(&[]);
        assert_regions(
            &regions,
            &gui_support::BRIEF_REGIONS,
            &gui_support::MERGED_REGIONS,
            opts.scale,
        );
        // Where S06's tooltip puts the pointer (gui_panel's S06).
        nested.pointer().move_to(692.0, 756.0);
        gui_support::poll(Duration::from_secs(10), "c2d1's tooltip", || {
            let tip = nested.window(0).labels("timeline-tooltip");
            tip.iter().any(|l| l == "Take 5 · c2d1").then_some(())
        });
        nested.window(0).wait_idle();
        assert_clean(&mut nested);
        let tooltip = rect(&mut nested, "timeline-tooltip");
        let shot = nested.screenshot("program");
        let t = Tokens::of(theme);
        let mut probes = grounds(&t, 2437);
        probes.extend([
            probe(
                "Live, unselected: the field well",
                116.0,
                24.0,
                t.rgb("field"),
            ),
            probe(
                "Program, the selected segment",
                280.0,
                24.0,
                t.rgb("raised"),
            ),
            probe("A1's meter track (field)", 1500.0, 389.0, t.rgb("field")),
            probe(
                "the tooltip's raised",
                tooltip[0] + 6.0,
                tooltip[1] + tooltip[3] - 6.0,
                t.rgb("raised"),
            ),
        ]);
        assert_probes(&nested, &shot, theme, &probes, 0);
        // A selected clip's lane segments wear the `selected` tint over
        // their take's (DESIGN Timeline); the selected layer row R1's.
        let tint = t.over("take-lily-tint", "bg");
        let handle_x = clip_start_x(&nested, &project, "c2d0") + 2.0;
        assert_probes(
            &nested,
            &shot,
            theme,
            &[
                probe(
                    "A2 under the selected c2d0",
                    520.0,
                    870.0,
                    t.get("selected").over(tint),
                ),
                probe(
                    "Face's selected row (R1)",
                    1240.0,
                    196.0,
                    t.over("selected", "surface"),
                ),
                // Its 4 px trim handle at its start, over the filmstrip.
                probe(
                    "c2d0's accent trim handle",
                    handle_x,
                    758.0,
                    t.rgb("accent"),
                ),
            ],
            blend(&nested.opts),
        );
        shots.push(vec![shot]);
        close(&mut nested);
    }
    board(opts.scale, "S06", &shots, None);
    assert_follows_the_theme(&opts, &shots[0][0], &shots[1][0], &VIDEO);
}

// ------------------------------------------------------------------- S18

/// S18 in Swamp Day, next to Swamp Night: the bin open, take 2 clicked,
/// take 4 Ctrl+clicked, Purge from disk. The modal over the theme's scrim
/// (Swamp Day's is `text` at 45 %, the others `bg` at 80 %) to y 968, the
/// hint bar outside it, the `raised` modal with its `field` loss list, the
/// destructive button's `record` fill under its `text-on-record` label,
/// the Fraunces title; no color outside video alike in both themes.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s18_the_purge_modal_in_swamp_day() {
    let opts = Options::default()
        .exclusive("Ctrl held by a wtype for a fixed window, the click timed into it");
    let mut shots = Vec::new();
    for theme in NIGHT_AND_DAY {
        let mut nested = Nested::start(AREA, &format!("S18-{theme}"), opts.clone());
        let ui = UiState {
            bin_open: true,
            ..s06_ui()
        };
        let project = fixture(&nested, DesignVariant::Base, &ui);
        open(&mut nested, &project, theme, launch());
        let (x, y) = center(&mut nested, "bin-take-2");
        nested.pointer().click(x, y);
        nested.window(0).wait_idle();
        // Ctrl held from 400 ms to 1600 ms into the run; the click lands in
        // between (as gui_bin_export's S18).
        let (x, y) = center(&mut nested, "bin-take-4");
        let held = nested.keys_held(&["-M", "ctrl", "-s", "1200", "-m", "ctrl"], 200);
        std::thread::sleep(Duration::from_millis(900));
        nested.pointer().click(x, y);
        held.wait();
        nested.window(0).wait_idle();
        assert_eq!(
            nested.window(0).local()["binSelection"],
            json!(["take-0004", "take-0002"])
        );
        let (x, y) = center(&mut nested, "bin-purge-selected");
        nested.pointer().click(x, y);
        nested.window(0).wait_idle();
        // Off the button that opened it: GTK picks the scrim again.
        nested.pointer().move_to(x + 1.0, y);
        nested.window(0).wait_idle();
        let labels = nested.window(0).labels("modal");
        for want in [
            "Get out of my swamp?",
            "Purge take 2 and take 4 from disk?",
            "Cancel",
            "Purge 2 takes",
        ] {
            assert!(labels.iter().any(|l| l == want), "{want:?} in {labels:?}");
        }
        let regions =
            nested
                .window(0)
                .regions(&["modal", "scrim-modal", "modal-confirm", "modal-list"]);
        assert_regions(
            &regions,
            &[("scrim-modal", [0.0, 0.0, 1600.0, 968.0])],
            &["scrim-modal", "modal", "modal-confirm", "modal-list"],
            nested.opts.scale,
        );
        let [mx, my, mw, mh] = regions["modal"];
        assert_eq!(mw, 520.0, "{regions:?}");
        assert!((mx + mw / 2.0 - 800.0).abs() <= 1.0, "{regions:?}");
        assert_clean(&mut nested);
        let shot = nested.screenshot("modal");
        let t = Tokens::of(theme);
        let [cx, cy, _, ch] = regions["modal-confirm"];
        let [lx, ly, _, lh] = regions["modal-list"];
        assert_probes(
            &nested,
            &shot,
            theme,
            &[
                probe(
                    "the hint bar, outside the scrim",
                    1596.0,
                    976.0,
                    t.rgb("surface"),
                ),
                probe(
                    "the modal's raised",
                    mx + 10.0,
                    my + mh / 2.0,
                    t.rgb("raised"),
                ),
                probe(
                    "the loss list's field",
                    lx + 3.0,
                    ly + lh / 2.0,
                    t.rgb("field"),
                ),
                probe(
                    "Purge 2 takes' record fill",
                    cx + 6.0,
                    cy + ch / 2.0,
                    t.rgb("record"),
                ),
            ],
            0,
        );
        assert_probes(
            &nested,
            &shot,
            theme,
            &[
                probe(
                    "the header under the scrim",
                    420.0,
                    4.0,
                    t.over("scrim", "surface"),
                ),
                probe(
                    "the stage under the scrim",
                    1208.0,
                    300.0,
                    t.over("scrim", "bg"),
                ),
                probe(
                    "the panel under the scrim",
                    1592.0,
                    940.0,
                    t.over("scrim", "surface"),
                ),
            ],
            blend(&nested.opts),
        );
        // The label: `text-on-record` glyphs on the fill.
        let confirm = regions["modal-confirm"];
        let label = count_near(&nested, &shot, confirm, t.rgb("text-on-record"), 24);
        assert!(
            label >= 40,
            "{theme}: {label} px of text-on-record in Purge 2 takes"
        );
        shots.push(vec![shot]);
        close(&mut nested);
    }
    board(opts.scale, "S18", &shots, None);
    // Video with the bin open: the pushed preview, the filmstrips and the
    // bin rows' thumbnails (under the scrim, each theme's).
    let video = [
        [368.0, 100.0, 832.0, 468.0],
        VIDEO[1],
        [12.0, 96.0, 96.0, 450.0],
    ];
    assert_follows_the_theme(&opts, &shots[0][0], &shots[1][0], &video);
}

// ------------------------------------------------------------------- S25

/// Waits until the toast host shows `title`, then for the window to settle.
fn wait_toast(nested: &mut Nested, title: &str) {
    gui_support::poll(Duration::from_secs(30), title, || {
        let labels = nested.window(0).labels("toast-host");
        labels.iter().any(|l| l == title).then_some(())
    });
    nested.window(0).wait_idle();
}

/// Dismisses the toast shown (its close button).
fn dismiss(nested: &mut Nested) {
    let (x, y) = center(nested, "toast-dismiss");
    nested.pointer().click(x, y);
    nested.window(0).wait_idle();
    gui_support::poll(Duration::from_secs(10), "the toast gone", || {
        nested
            .window(0)
            .labels("toast-host")
            .is_empty()
            .then_some(())
    });
}

/// Rewrites the project's `qrec.json` through `edit` (an external edit:
/// the host merges it).
fn edit_project(project: &Path, edit: impl FnOnce(&mut Value)) {
    let file = project.join("qrec.json");
    let mut json: Value =
        serde_json::from_str(&fs::read_to_string(&file).expect("qrec.json")).expect("JSON");
    edit(&mut json);
    fs::write(&file, serde_json::to_string_pretty(&json).expect("JSON")).expect("qrec.json");
}

/// One toast's checks in `theme`: placed (bottom 12 px above the hint bar,
/// centered, at least 360 wide), `raised` with its `border` hairline, its
/// level icon in its token, its action (if any) in `accent-text`, and its
/// e2 shadow on the lanes' `bg` under it. Returns the shot.
///
/// At most DESIGN's 560 too, its body in 2 lines at most (WP9: the error
/// toast measured 607 wide with 3; C23's labels counted characters, and
/// the body's line break let it run past its 2 lines).
fn shoot_toast(
    nested: &mut Nested,
    t: &Tokens,
    level: &str,
    icon_token: &str,
    action: bool,
) -> PathBuf {
    let [x, y, w, h] = rect(nested, "toast-host");
    assert!(
        (y + h - 956.0).abs() <= 1.0,
        "{level}: the bottom at {}",
        y + h
    );
    assert!(
        (x + w / 2.0 - 800.0).abs() <= 1.0,
        "{level}: centered: {x} + {w}"
    );
    assert!((360.0..=561.0).contains(&w), "{level}: {w} wide (360–560)");
    if let Some(body) = nested.window(0).regions(&["toast-body"]).get("toast-body") {
        // `body-sm` is 13/18.
        assert!(body[3] <= 37.0, "{level}: the body is {} high", body[3]);
    }
    let action_rect = action.then(|| rect(nested, "toast-action-0"));
    let shot = nested.screenshot(level);
    assert_probes(
        nested,
        &shot,
        t.theme,
        &[
            probe("the toast's raised", x + 6.0, y + h / 2.0, t.rgb("raised")),
            probe("the toast's border", x, y + h / 2.0, t.rgb("border")),
        ],
        0,
    );
    // The 20 px icon, 16 px in and 12 down (C23's padding).
    let icon = count_near(
        nested,
        &shot,
        [x + 16.0, y + 12.0, 20.0, 20.0],
        t.rgb(icon_token),
        24,
    );
    assert!(
        icon >= 20,
        "{}: {level}'s icon has {icon} px of {icon_token}",
        t.theme
    );
    if let Some(r) = action_rect {
        let label = count_near(nested, &shot, r, t.rgb("accent-text"), 24);
        assert!(
            label >= 15,
            "{}: {level}'s action has {label} px of accent-text",
            t.theme
        );
    }
    // The e2 shadow falls on the lanes' bg under the toast (2 px down).
    let under = pixel(nested, &shot, x + w / 2.0, y + h + 2.0);
    let bg = t.rgb("bg");
    assert!(
        darker(under, bg),
        "{}: no shadow under the {level} toast: {} on bg {}",
        t.theme,
        hex(under),
        hex(bg)
    );
    shot
}

/// `shaded` is visibly darker than `ground`: 6 % of its brightness, and at
/// least 6 levels (a shadow on Swamp Night's near-black is a few levels).
fn darker(shaded: [u8; 3], ground: [u8; 3]) -> bool {
    let (s, g) = (brightness(shaded), brightness(ground));
    s + 6 <= g && s * 100 <= g * 94
}

/// S25's toasts in Swamp Day, next to Swamp Night: one toast of each level
/// the core has, each triggered for real and shot alone (DESIGN Lists: the
/// info icon in `text`, success in `success`, warning in `warning-text`,
/// error in `record-text`; actions in `accent-text`; e2 on `raised`):
/// info, an external edit of `qrec.json` merged (with Undo); warning, an
/// edit of a fact by hand, ignored; success, `p` (Frame saved, the path in
/// the mono face, Copy image and Open folder); error, `qrec.json` made
/// invalid on disk (the core keeps the last good state; the toast stays).
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn s25_toasts_in_swamp_day() {
    let opts = Options::default();
    let mut columns: Vec<Vec<PathBuf>> = Vec::new();
    for theme in NIGHT_AND_DAY {
        let mut nested = Nested::start(AREA, &format!("S25-{theme}"), opts.clone());
        let project = fixture(&nested, DesignVariant::Base, &s06_ui());
        open(&mut nested, &project, theme, launch());
        let t = Tokens::of(theme);
        let mut column = Vec::new();

        edit_project(&project, |json| json["name"] = json!("rust-traits-talk-v2"));
        wait_toast(&mut nested, "Merged an external edit of qrec.json");
        column.push(shoot_toast(&mut nested, &t, "info", "text", true));
        dismiss(&mut nested);

        edit_project(&project, |json| {
            json["createdAt"] = json!("2026-10-01T09:00:00Z");
        });
        wait_toast(&mut nested, "Ignored an external edit of qrec.json");
        column.push(shoot_toast(
            &mut nested,
            &t,
            "warning",
            "warning-text",
            false,
        ));
        dismiss(&mut nested);

        nested.keys(&["-k", "p"]);
        wait_toast(&mut nested, "Frame saved");
        column.push(shoot_toast(&mut nested, &t, "success", "success", true));
        dismiss(&mut nested);

        fs::write(project.join("qrec.json"), "{ \"version\": 1, \"name\": ").expect("qrec.json");
        wait_toast(&mut nested, "qrec.json changed on disk but isn't valid");
        // Its body keeps the file name's case, as the title does (M5: it
        // read "Qrec.json is not valid JSON: …", `format::capitalized`).
        let labels = nested.window(0).labels("toast-host");
        assert!(
            labels
                .iter()
                .any(|l| l.starts_with("qrec.json is not valid JSON: ")),
            "{}: the error toast's body: {labels:?}",
            t.theme
        );
        column.push(shoot_toast(&mut nested, &t, "error", "record-text", false));
        assert_clean(&mut nested);
        columns.push(column);
        close(&mut nested);
    }
    board(
        opts.scale,
        "S25",
        &columns,
        Some([420.0, 776.0, 760.0, 200.0]),
    );
    for (night, day) in columns[0].iter().zip(&columns[1]) {
        assert_follows_the_theme(&opts, night, day, &VIDEO);
    }
}

// ----------------------------------------------------- the narrow sheet

/// The narrow layout's sheet (1000 wide: the panel over the content,
/// G§3.15) in Swamp Day and Swamp Night: the split view dims the content
/// and shades the sheet's edge with the theme's shadow (`shadow-e2-contact`
/// through the libadwaita bridge's `--shade-color`), not libadwaita's own
/// neutral `rgb(0 0 6 / 7%)` (25 % dark): WP10's light-theme fix. The
/// header and the hint bar are outside the split view: not dimmed.
#[test]
#[ignore = "needs a nested compositor (sway, grim, wtype): G§8.4"]
fn the_narrow_sheet_shades_in_the_themes_tokens() {
    let opts = Options::size(1000, 1000);
    let mut shots = Vec::new();
    for theme in NIGHT_AND_DAY {
        let mut nested = Nested::start(AREA, &format!("sheet-{theme}"), opts.clone());
        let project = fixture(&nested, DesignVariant::Base, &s06_ui());
        open(&mut nested, &project, theme, launch());
        assert_eq!(nested.window(0).local()["layout"]["width"], "Narrow");
        let (x, y) = center(&mut nested, "layers-button");
        nested.pointer().click(x, y);
        let panel = gui_support::poll(Duration::from_secs(10), "the sheet", || {
            let gui = nested.window(0);
            gui.wait_idle();
            gui.regions(&[]).get("region-panel").copied()
        });
        assert_regions(
            &BTreeMap::from([("region-panel".to_owned(), panel)]),
            &[("region-panel", [616.0, 48.0, 384.0, 920.0])],
            &["region-panel"],
            nested.opts.scale,
        );
        // Off the button, over the dimmed content.
        nested.pointer().move_to(300.0, 300.0);
        nested.window(0).wait_idle();
        assert_clean(&mut nested);
        let shot = nested.screenshot("sheet");
        let t = Tokens::of(theme);
        let shade = "shadow-e2-contact";
        assert_probes(
            &nested,
            &shot,
            theme,
            &[
                probe("the header, not dimmed", 420.0, 4.0, t.rgb("surface")),
                probe("the sheet's surface", 990.0, 940.0, t.rgb("surface")),
                probe("the hint bar, not dimmed", 996.0, 976.0, t.rgb("surface")),
            ],
            0,
        );
        assert_probes(
            &nested,
            &shot,
            theme,
            &[
                probe("the stage, dimmed", 10.0, 300.0, t.over(shade, "bg")),
                probe("the lanes, dimmed", 300.0, 950.0, t.over(shade, "bg")),
                probe(
                    "the track headers, dimmed",
                    90.0,
                    950.0,
                    t.over(shade, "surface"),
                ),
            ],
            blend(&nested.opts),
        );
        // The edge shadow darkens the dimmed lanes toward the sheet.
        let edge = pixel(&nested, &shot, 614.0, 950.0);
        let dimmed = t.over(shade, "bg");
        assert!(
            darker(edge, dimmed),
            "{theme}: no shadow at the sheet's edge: {} on {}",
            hex(edge),
            hex(dimmed)
        );
        shots.push(vec![shot]);
        close(&mut nested);
    }
    board(opts.scale, "sheet", &shots, None);
    // At 1000 wide the preview fits the 1000×608 stage (G§3.15) and the
    // filmstrips run 824 px; the sheet covers what is past x 616.
    let video = [[20.0, 64.0, 960.0, 540.0], [176.0, 720.0, 824.0, 76.0]];
    assert_follows_the_theme(&opts, &shots[0][0], &shots[1][0], &video);
}
