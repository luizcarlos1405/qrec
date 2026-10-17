//! DESIGN's motion table under reduced motion (D-30; WP10's audit): every
//! moment it lists holds still (instant or static, or the fade DESIGN asks
//! for in place of a movement), row by row, through the widgets' own
//! decisions, GTK-free. The rows are DESIGN.md's (a row this table doesn't
//! know fails), and every animation the crate drives is one of them or
//! says why it isn't motion (a new one fails until it is classified). The
//! Narrow sheet's slide is libadwaita's: `gui_audits`' nested case (GTK's
//! own animations on) checks it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use qrec_core::ClipId;

use crate::bin_drawer::slide;
use crate::panel::page_transition;
use crate::stage::{countdown, overlays};
use crate::timeline::control::{GapEase, ViewModel};
use crate::timeline::draw::saving_phase;
use crate::toasts;
use crate::widgets::mud_bar::{self, MudSim, MudState};

/// A DESIGN Motion row and the check of its reduced-motion fallback.
struct Row {
    /// DESIGN.md's "Moment" cell, verbatim.
    moment: &'static str,
    /// Its reduced-motion column, for the failure message.
    reduced: &'static str,
    check: fn(),
}

const ROWS: [Row; 12] = [
    Row {
        moment: "Hover, press, toggle",
        reduced: "same (color only)",
        check: css_transitions_are_colors_within_120_ms,
    },
    Row {
        moment: "State change (selection, segment, row)",
        reduced: "120 ms opacity/color only",
        check: css_transitions_are_colors_within_120_ms,
    },
    Row {
        moment: "Popover, menu, palette in / out",
        reduced: "fade 120 ms, no scale",
        check: overlays_open_at_once,
    },
    Row {
        moment: "Bin drawer and panel overlay",
        reduced: "crossfade 120 ms",
        check: the_bin_drawer_crossfades,
    },
    Row {
        moment: "Toast in / out",
        reduced: "fade only",
        check: toasts_fade_without_rising,
    },
    Row {
        moment: "Timeline zoom (`=`, `\u{2212}`, Ctrl+wheel, `\\`, `z`)",
        reduced: "instant",
        check: the_zoom_is_instant,
    },
    Row {
        moment: "Drag-move neighbors closing the gap",
        reduced: "instant",
        check: the_gap_closes_at_once,
    },
    Row {
        moment: "REC torch flicker",
        reduced: "static",
        check: the_torch_is_static,
    },
    Row {
        moment: "Countdown digit / ring",
        reduced: "swap / static ring",
        check: the_countdown_swaps_on_a_full_ring,
    },
    Row {
        moment: "Mud level / bubbles / wobble",
        reduced: "jump / none / none",
        check: the_mud_jumps_and_lies_flat,
    },
    Row {
        moment: "Playhead during playback",
        reduced: "same",
        check: conveys_state,
    },
    Row {
        moment: "Live meters",
        reduced: "same (they convey state)",
        check: conveys_state,
    },
];

/// Every row of DESIGN.md's motion table has its check here, and each
/// check holds (the table's own rows without DESIGN.md: a Nix build's
/// source has none).
#[test]
fn every_moment_of_designs_table_holds_still_under_reduced_motion() {
    if let Some(design) = design_rows() {
        let known: Vec<&str> = ROWS.iter().map(|r| r.moment).collect();
        for (moment, reduced) in &design {
            assert!(
                known.contains(&moment.as_str()),
                "DESIGN's motion row {moment:?} (reduced: {reduced:?}) has no check here"
            );
        }
        for row in &ROWS {
            let in_design = design.iter().find(|(m, _)| m == row.moment);
            let (_, reduced) = in_design
                .unwrap_or_else(|| panic!("{:?} isn't in DESIGN's motion table", row.moment));
            assert_eq!(reduced, row.reduced, "{}", row.moment);
        }
    }
    for row in &ROWS {
        (row.check)();
    }
}

/// The motion table of DESIGN.md (`### Motion system`): each row's moment
/// and its reduced-motion cell; `None` without the file.
fn design_rows() -> Option<Vec<(String, String)>> {
    let design = repo().join("DESIGN.md");
    let text = std::fs::read_to_string(design).ok()?;
    let start = text.find("### Motion system")?;
    let rows = text[start..]
        .lines()
        .skip_while(|l| !l.starts_with("| Moment"))
        .skip(2)
        .take_while(|l| l.starts_with('|'))
        .map(|l| {
            let cells: Vec<&str> = l.trim_matches('|').split(" | ").map(str::trim).collect();
            (cells[0].to_owned(), cells[cells.len() - 1].to_owned())
        })
        .collect();
    Some(rows)
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// ------------------------------------------------------------- the checks

/// Hover, press, toggle and state changes: the stylesheets transition
/// colors and opacity only, within 120 ms (nothing moves or scales), and
/// no CSS animation runs (G§3.14: every animation is driven from code).
fn css_transitions_are_colors_within_120_ms() {
    const COLORS: [&str; 6] = [
        "background-color",
        "background-image",
        "color",
        "border-color",
        "opacity",
        "outline-color",
    ];
    let mut seen = 0;
    for (path, css) in stylesheets() {
        let name = path.display();
        assert!(
            !css.contains("@keyframes") && !css.contains("animation:"),
            "{name}: a CSS animation (G§3.14: code drives every animation)"
        );
        let mut rest = css.as_str();
        while let Some(at) = rest.find("transition") {
            let decl = &rest[at..];
            let end = decl.find(';').unwrap_or(decl.len());
            let value = decl[..end]
                .split_once(':')
                .map_or("", |(_, v)| v)
                .to_owned();
            rest = &decl[end..];
            // `cubic-bezier(…)`'s commas aren't the list's.
            let mut flat = String::new();
            let mut depth = 0;
            for c in value.chars() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ if depth == 0 => flat.push(c),
                    _ => {}
                }
            }
            for item in flat.split(',').map(str::trim).filter(|i| !i.is_empty()) {
                let mut words = item.split_whitespace();
                let property = words.next().unwrap_or_default();
                let duration = words.next().unwrap_or_default();
                assert!(
                    COLORS.contains(&property),
                    "{name}: `transition: {item}` moves something ({property})"
                );
                let ms: f64 = duration
                    .strip_suffix("ms")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| panic!("{name}: `transition: {item}`: no ms duration"));
                assert!(ms <= 120.0, "{name}: `transition: {item}` over 120 ms");
                seen += 1;
            }
        }
    }
    assert!(seen > 0, "the hover transitions (common.css, panel.css)");
}

/// Every stylesheet of the GResource's theme (path, text).
fn stylesheets() -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut dirs = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/theme")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("the theme folder").flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "css") {
                let text = std::fs::read_to_string(&path).expect("a stylesheet");
                out.push((path, text));
            }
        }
    }
    out
}

/// Popovers, menus and Qrec's own overlays (the export panel, the modal,
/// Settings; the palette is M5's) have no animation of their own: GTK 4's
/// popovers appear at once and Qrec animates none of them, so under
/// reduced motion too they appear without a scale (the inventory below:
/// no animation site in their modules).
fn overlays_open_at_once() {
    let moves = |site: &Site| {
        !KNOWN.iter().any(|(file, what, moment)| {
            *file == site.file && *what == site.what && moment.starts_with("not motion")
        })
    };
    for site in animation_sites().into_iter().filter(moves) {
        for module in [
            "export/",
            "modal.rs",
            "settings/",
            "palette.rs",
            "panel/menus.rs",
        ] {
            assert!(
                !site.file.starts_with(module),
                "an animation in an overlay ({site:?}): DESIGN's reduced fade 120 ms, no scale"
            );
        }
    }
}

/// The bin drawer crossfades in place (120 ms, linear) instead of sliding
/// (240 ms expo-out in, 160 ms ease-in out).
fn the_bin_drawer_crossfades() {
    let open = slide::plan(true, true, 0.0, 1.0);
    assert_eq!(
        (open.fade, open.from, open.to, open.ms, open.easing),
        (true, 0.0, 1.0, slide::FADE_MS, adw::Easing::Linear)
    );
    let close = slide::plan(false, true, 1.0, 0.8);
    assert_eq!(
        (close.fade, close.from, close.to, close.ms),
        (true, 0.8, 0.0, 120)
    );
    assert_eq!(slide::FADE_MS, 120);
    // With motion allowed, it slides.
    let slides = slide::plan(true, false, 0.25, 1.0);
    assert_eq!(
        (
            slides.fade,
            slides.from,
            slides.to,
            slides.ms,
            slides.easing
        ),
        (false, 0.25, 1.0, 240, adw::Easing::EaseOutExpo)
    );
    let closes = slide::plan(false, false, 1.0, 1.0);
    assert_eq!((closes.fade, closes.ms), (false, 160));
}

/// A toast fades in where it rests (it rises 12 px with motion allowed);
/// its way out is a fade in both.
fn toasts_fade_without_rising() {
    for eased in [0.0, 0.3, 1.0] {
        assert_eq!(toasts::lift(eased, true), 12, "reduced, at {eased}");
    }
    assert_eq!(toasts::lift(0.0, false), 0);
    assert_eq!(toasts::lift(0.5, false), 6);
    assert_eq!(toasts::lift(1.0, false), 12);
}

/// A zoom step lands at once (it eases over 200 ms with motion allowed).
fn the_zoom_is_instant() {
    let state = qrec_core::testkit::design_state();
    for (reduced, animating) in [(true, false), (false, true)] {
        let mut vm = ViewModel::new(reduced);
        vm.set_width(1040.0);
        vm.on_state(&state, 7391, ms(0));
        vm.zoom_step(true, 2437, ms(1000));
        assert_eq!(vm.animating(ms(1050)), animating, "reduced {reduced}");
        if reduced {
            assert_eq!(vm.shown(ms(1000)), vm.target_view());
        }
    }
}

/// A move drag's neighbours are at their places at once (they close the
/// gap over 120 ms with motion allowed).
fn the_gap_closes_at_once() {
    let clip = ClipId::from_raw("c1b2");
    for (instant, offset) in [(true, 0.0), (false, 40.0)] {
        let mut gap = GapEase::default();
        gap.retarget(
            &[(clip.clone(), 100.0)],
            || vec![(clip.clone(), 140.0)],
            ms(0),
            instant,
        );
        let drawn = gap.offset(&clip, ms(0));
        assert!((drawn - offset).abs() < 1e-6, "instant {instant}: {drawn}");
    }
}

/// The REC badge holds still without its glow (it flickers on a 1600 ms
/// loop with its `record-glow` otherwise).
fn the_torch_is_static() {
    for t in (0..1600).step_by(40) {
        assert_eq!(overlays::torch_look(true, ms(t)), (1.0, false), "at {t} ms");
    }
    let (opacity, glow) = overlays::torch_look(false, ms(144));
    assert!(
        (opacity - 0.82).abs() < 1e-9 && glow,
        "9 % into the loop: 0.82"
    );
}

/// The countdown's ring stays full and the digit swaps in at its size and
/// full ink (the ring empties over each second and the digit settles from
/// 1.12× over 160 ms otherwise).
fn the_countdown_swaps_on_a_full_ring() {
    for t in [0, 250, 999, 1000] {
        assert_eq!(countdown::sweep(true, ms(t)), 1.0, "the ring at {t} ms");
        assert_eq!(countdown::settle(true, false, ms(t)), 1.0);
    }
    assert!((countdown::digit_zoom(1.0) - 1.0).abs() < 1e-9);
    assert!((countdown::sweep(false, ms(500)) - 0.5).abs() < 1e-9);
    assert_eq!(countdown::settle(false, false, ms(0)), 0.0);
    assert!((countdown::digit_zoom(0.0) - 1.12).abs() < 1e-9);
}

/// The mud bar: a level jumps, the fill lies flat (no bubbles, no wobble)
/// and nothing ticks (with motion allowed it eases over 400 ms, bubbles
/// and wobbles on the frame clock).
fn the_mud_jumps_and_lies_flat() {
    let running = MudState::Running(0.42);
    assert!(!mud_bar::animates(running, true, false));
    assert!(mud_bar::animates(running, false, false));
    let mut sim = MudSim::new(7);
    sim.set_level(0.2, 0.0, true);
    for t in [0.0, 700.0, 1900.0, 5000.0] {
        let (level, bubbles, wobble) = mud_bar::frame_look(&mut sim, running, true, false, t, 1.0);
        assert_eq!(level, 0.42, "at {t} ms: the state's level, at once");
        assert!(bubbles.is_empty() && wobble == 0.0, "at {t} ms: flat");
    }
    let mut sim = MudSim::new(7);
    sim.set_level(0.42, 0.0, true);
    let moving = (0..40).any(|i| {
        let (_, bubbles, wobble) =
            mud_bar::frame_look(&mut sim, running, false, false, f64::from(i) * 100.0, 1.0);
        !bubbles.is_empty() || wobble != 0.0
    });
    assert!(moving, "with motion allowed it bubbles and wobbles");
}

/// The playhead and the meters convey state: they move under reduced
/// motion too (DESIGN: "same"). So do the spinners (DESIGN: they keep
/// spinning, essential feedback).
fn conveys_state() {}

/// Beyond the table (DESIGN Progress, C13): the saving ghost's stripes
/// hold still (they move a stripe every 1.2 s otherwise), and the panel's
/// pages switch at once (they slide otherwise).
#[test]
fn the_saving_ghost_and_the_panel_pages_hold_still() {
    for t in [0, 300, 600, 1199] {
        assert_eq!(saving_phase(t, true), 0.0, "at {t} ms");
    }
    assert!(saving_phase(600, false) > 0.0);
    assert_eq!(page_transition(true), gtk::StackTransitionType::None);
    assert_eq!(
        page_transition(false),
        gtk::StackTransitionType::SlideLeftRight
    );
}

// ---------------------------------------------------------- the inventory

/// An animation site in the crate's source: a tick callback (its `what`),
/// a timed animation, a stack transition, the Narrow sheet's show, or a
/// read of the animation clock.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    /// Under `src/`.
    file: String,
    what: String,
}

/// Every animation the crate drives, with the DESIGN moment it is (or why
/// it isn't motion).
const KNOWN: [(&str, &str, &str); 19] = [
    (
        "stage/countdown.rs",
        "countdown: tick",
        "Countdown digit / ring",
    ),
    (
        "stage/countdown.rs",
        "motion::now",
        "Countdown digit / ring",
    ),
    (
        "stage/mod.rs",
        "motion::now",
        "Countdown digit / ring (its digit's start)",
    ),
    (
        "stage/overlays.rs",
        "stage: the REC cluster",
        "REC torch flicker (and the timer, state)",
    ),
    (
        "stage/overlays.rs",
        "motion::now",
        "REC torch flicker (and the timer, state)",
    ),
    (
        "stage/transport.rs",
        "transport: the timecode",
        "Playhead during playback (state)",
    ),
    (
        "widgets/mud_bar.rs",
        "mud bar: tick",
        "Mud level / bubbles / wobble",
    ),
    (
        "widgets/mud_bar.rs",
        "motion::now",
        "Mud level / bubbles / wobble",
    ),
    (
        "widgets/spinner.rs",
        "spinner: tick",
        "spinners: essential, they keep spinning",
    ),
    (
        "widgets/spinner.rs",
        "motion::now",
        "spinners: essential, they keep spinning",
    ),
    ("widgets/meter.rs", "motion::now", "Live meters (state)"),
    (
        "timeline/mod.rs",
        "timeline: a frame",
        "Timeline zoom, the gap, the playhead, the ghosts",
    ),
    (
        "timeline/mod.rs",
        "timeline: the probe",
        "not motion: G§8.6's scripted frame-time probe",
    ),
    (
        "timeline/mod.rs",
        "motion::now",
        "the ghosts' growth and stripes, the spinners",
    ),
    (
        "export/mod.rs",
        "export: the stderr's end",
        "not motion: the log box kept at its end",
    ),
    (
        "bin_drawer/slide.rs",
        "TimedAnimation",
        "Bin drawer and panel overlay",
    ),
    ("toasts.rs", "TimedAnimation", "Toast in / out"),
    (
        "panel/mod.rs",
        "set_transition_type",
        "C13's page switch (instant under reduced motion)",
    ),
    (
        "window/mod.rs",
        "set_show_sidebar",
        "Bin drawer and panel overlay (the Narrow sheet)",
    ),
];

/// The animation sites of `src/` (this table's file left out).
fn animation_sites() -> Vec<Site> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut dirs = vec![src.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("src").flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    let mut sites = Vec::new();
    for path in files {
        let file = path
            .strip_prefix(&src)
            .expect("under src")
            .to_string_lossy()
            .into_owned();
        if file == "theme/motion_table.rs" || file == "theme/motion.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a source file");
        let code = without_comments(&text);
        let mut add = |what: String| {
            let site = Site {
                file: file.clone(),
                what,
            };
            if !sites.contains(&site) {
                sites.push(site);
            }
        };
        for start in ["guard::tick(", ".tick("] {
            let mut rest = code.as_str();
            while let Some(at) = rest.find(start) {
                let args = call_args(&rest[at + start.len() - 1..]);
                rest = &rest[at + start.len()..];
                // A tick callback names itself: its first string literal.
                if let Some(what) = first_literal(args) {
                    add(what);
                }
            }
        }
        for (needle, what) in [
            ("TimedAnimation::new(", "TimedAnimation"),
            ("SpringAnimation::new(", "SpringAnimation"),
            ("set_transition_type(", "set_transition_type"),
            ("set_show_sidebar(", "set_show_sidebar"),
            ("motion::now()", "motion::now"),
        ] {
            if code.contains(needle) {
                add(what.to_owned());
            }
        }
    }
    sites
}

/// `text` with its `//` comments blanked (doc comments name sites too).
fn without_comments(text: &str) -> String {
    text.lines()
        .map(|line| {
            let mut in_str = false;
            let bytes = line.as_bytes();
            for i in 0..bytes.len() {
                match bytes[i] {
                    b'"' if i == 0 || bytes[i - 1] != b'\\' => in_str = !in_str,
                    b'/' if !in_str && bytes.get(i + 1) == Some(&b'/') => return &line[..i],
                    _ => {}
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The argument list of a call whose `(` starts `text`.
fn call_args(text: &str) -> &str {
    let mut depth = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[..=i];
                }
            }
            _ => {}
        }
    }
    text
}

/// The first string literal in `args`, before any closure starts.
fn first_literal(args: &str) -> Option<String> {
    let before = args.split(['|']).next().unwrap_or(args);
    let start = before.find('"')? + 1;
    let end = before[start..].find('"')? + start;
    Some(before[start..end].to_owned())
}

/// Every animation in the source is a known one, and every known one is
/// still there (the list follows the code both ways).
#[test]
fn every_animation_is_a_moment_of_the_table() {
    let sites = animation_sites();
    let known = |site: &Site| {
        KNOWN
            .iter()
            .any(|(file, what, _)| *file == site.file && *what == site.what)
    };
    let unknown: Vec<&Site> = sites.iter().filter(|s| !known(s)).collect();
    assert!(
        unknown.is_empty(),
        "animations with no reduced-motion row (classify each in KNOWN): {unknown:#?}"
    );
    for (file, what, moment) in KNOWN {
        assert!(
            sites.iter().any(|s| s.file == file && s.what == what),
            "{file}'s {what} ({moment}) is gone: drop it from KNOWN"
        );
    }
}
