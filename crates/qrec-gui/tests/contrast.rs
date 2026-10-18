//! WCAG 2.2 AA for the theme tokens (DESIGN.md → Colors → Contrast audit
//! and Decisions; WP10): every text and indicator token on every background
//! DESIGN says it is drawn on, in all four themes, read from the generated
//! token files the window loads (`resources/theme/tokens/<theme>.css`, so
//! this runs without `design/` and without GTK).
//!
//! The rule (the PO's decision of 2026-10-15): text 4.5:1 (SC 1.4.3, in
//! every state it is drawn in: hover included) and indicators, focus,
//! control outlines and graphical objects 3:1 (SC 1.4.11), each with a 0.05
//! rounding margin: text forms reach 4.55, indicators 3.05. No token is
//! drawn only at a large size (the display titles and the export
//! percentage are `text`, or `record-text` once it failed; the REC timer
//! is `video-text`: each is drawn small elsewhere too), so the 3:1
//! large-text floor never binds a pair.
//!
//! An alpha token (`hover`, `selected`, `warning-20`, `video-scrim`, …)
//! composites over its background as the screen shows it: in sRGB, each
//! channel rounded to 8 bits. DESIGN's audit reads that way to its two
//! decimals ([`the_decided_values_bind_where_design_says`]); float
//! compositing reads up to 0.03 lower (Ogre Mode's P6 pair at 4.52).
//!
//! The one waiver is the PO's P4: Swamp Day's `text-on-record` on `record`
//! is 4.53 ("passes AA; not darkened further, which would shrink the
//! deutan gap to `accent`"): it must pass 4.5, and the waiver goes once it
//! isn't needed.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const THEMES: [&str; 4] = ["swamp-night", "swamp-day", "dragons-keep", "ogre-mode"];

/// A color with its alpha, each channel 0–255.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rgba {
    rgb: [u8; 3],
    alpha: u8,
}

impl Rgba {
    const WHITE: Rgba = Rgba {
        rgb: [255, 255, 255],
        alpha: 255,
    };

    fn parse(hex: &str) -> Option<Rgba> {
        let hex = hex.strip_prefix('#')?;
        if !matches!(hex.len(), 6 | 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgba {
            rgb: [byte(0)?, byte(2)?, byte(4)?],
            alpha: if hex.len() == 8 { byte(6)? } else { 255 },
        })
    }

    /// `self` over the opaque `below`, as the screen shows it: sRGB, each
    /// channel rounded to 8 bits.
    fn over(self, below: Rgba) -> Rgba {
        let a = f64::from(self.alpha) / 255.0;
        let mut rgb = [0; 3];
        for (i, out) in rgb.iter_mut().enumerate() {
            let mixed = f64::from(self.rgb[i]) * a + f64::from(below.rgb[i]) * (1.0 - a);
            *out = mixed.round().clamp(0.0, 255.0) as u8;
        }
        Rgba { rgb, alpha: 255 }
    }

    /// WCAG 2.2's relative luminance (of the color as if opaque).
    fn luminance(self) -> f64 {
        let channel = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let [r, g, b] = self.rgb.map(channel);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }
}

/// WCAG 2.2's contrast ratio of two opaque colors.
fn ratio(a: Rgba, b: Rgba) -> f64 {
    let (la, lb) = (a.luminance(), b.luminance());
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// One theme's color tokens, by name (without `--qrec-`).
type Tokens = BTreeMap<String, Rgba>;

fn tokens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/theme/tokens")
}

/// The color tokens of `theme`'s generated CSS (`  --qrec-<name>: #…;`).
fn read_tokens(theme: &str) -> Tokens {
    let path = tokens_dir().join(format!("{theme}.css"));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut tokens = Tokens::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("--qrec-") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        if let Some(color) = Rgba::parse(value.trim().trim_end_matches(';')) {
            tokens.insert(name.to_owned(), color);
        }
    }
    assert!(
        tokens.len() >= 75,
        "{}: {} colors, the generator writes 75 (G§3.14)",
        path.display(),
        tokens.len()
    );
    tokens
}

/// What a pair has to reach (before the margin).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Need {
    /// Text and icons read as text: 4.5:1 (SC 1.4.3).
    Text,
    /// Indicators, focus, control outlines, graphical objects: 3:1 (SC
    /// 1.4.11).
    Indicator,
}

impl Need {
    fn floor(self) -> f64 {
        match self {
            Need::Text => 4.5,
            Need::Indicator => 3.0,
        }
    }
}

/// The PO's margin over each floor (DESIGN → Decisions).
const MARGIN: f64 = 0.05;

/// A background: a token, or alpha tokens composited, top last, over an
/// opaque base (a token, or white video for the overlays' worst case).
#[derive(Debug, Clone, Copy)]
enum Ground {
    Token(&'static str),
    /// `top` over the token `base`.
    Over(&'static str, &'static str),
    /// `top` over white video (theme-independent content, R5).
    OverWhite(&'static str),
    /// The countdown's dim over white video: `video-scrim` at 65 % (black
    /// at 40 %, G§1.3), its worst case (C07).
    DimmedWhite,
    /// `top` over the countdown's dim over white video: the digit's plate.
    OverDimmedWhite(&'static str),
}

/// The countdown dims the preview with `video-scrim` at this opacity
/// (`stage::countdown`, G§1.3).
const COUNTDOWN_DIM: f64 = 0.65;

impl Rgba {
    /// `self` at `opacity` (its alpha × `opacity`, rounded to 8 bits).
    fn at(self, opacity: f64) -> Rgba {
        Rgba {
            rgb: self.rgb,
            alpha: (f64::from(self.alpha) * opacity).round() as u8,
        }
    }
}

impl Ground {
    fn color(self, t: &Tokens) -> Rgba {
        let dimmed_white = || color(t, "video-scrim").at(COUNTDOWN_DIM).over(Rgba::WHITE);
        match self {
            Ground::Token(name) => opaque(t, name),
            Ground::Over(top, base) => color(t, top).over(opaque(t, base)),
            Ground::OverWhite(top) => color(t, top).over(Rgba::WHITE),
            Ground::DimmedWhite => dimmed_white(),
            Ground::OverDimmedWhite(top) => color(t, top).over(dimmed_white()),
        }
    }

    fn describe(self) -> String {
        match self {
            Ground::Token(name) => name.to_owned(),
            Ground::Over(top, base) => format!("{top} over {base}"),
            Ground::OverWhite(top) => format!("{top} over white video"),
            Ground::DimmedWhite => "the countdown's dim over white video".to_owned(),
            Ground::OverDimmedWhite(top) => {
                format!("{top} over the countdown's dim over white video")
            }
        }
    }

    /// The tokens it is made of.
    fn tokens(self) -> Vec<&'static str> {
        match self {
            Ground::Token(name) => vec![name],
            Ground::Over(top, base) => vec![top, base],
            Ground::OverWhite(top) => vec![top],
            Ground::DimmedWhite => vec!["video-scrim"],
            Ground::OverDimmedWhite(top) => vec![top, "video-scrim"],
        }
    }
}

fn color(t: &Tokens, name: &str) -> Rgba {
    *t.get(name)
        .unwrap_or_else(|| panic!("no token {name} in the generated CSS"))
}

/// A token every pair draws opaque (foregrounds, bases).
fn opaque(t: &Tokens, name: &str) -> Rgba {
    let c = color(t, name);
    assert_eq!(c.alpha, 255, "{name} is drawn opaque but has an alpha");
    c
}

/// One pair: `fg` on `ground`, where DESIGN draws it (`what`).
#[derive(Debug, Clone, Copy)]
struct Pair {
    fg: &'static str,
    ground: Ground,
    need: Need,
    what: &'static str,
}

const fn text(fg: &'static str, ground: Ground, what: &'static str) -> Pair {
    Pair {
        fg,
        ground,
        need: Need::Text,
        what,
    }
}

const fn indicator(fg: &'static str, ground: Ground, what: &'static str) -> Pair {
    Pair {
        fg,
        ground,
        need: Need::Indicator,
        what,
    }
}

use Ground::{Over, OverDimmedWhite, OverWhite, Token};

/// Every pair: DESIGN's Contrast audit, row by row (its "min of" rows one
/// pair per background), then the pairs DESIGN's Components draw that the
/// audit doesn't list. R1 and R2 keep the rest out: a selected row's text
/// turns `text` (`muted` and the text forms fail on the tint), and rows on
/// `raised` highlight with R1, not the `hover` overlay, so `muted` and
/// `warning-text` are never drawn under `hover` over `raised`.
const PAIRS: &[Pair] = &[
    // ------------------------------------------------- the audit: text
    text("text", Token("bg"), "primary text on the window ground"),
    text("text", Token("surface"), "primary text on the chrome"),
    text(
        "text",
        Token("raised"),
        "primary text on popovers, toasts, modals",
    ),
    text("muted", Token("bg"), "secondary text on the window ground"),
    text("muted", Token("surface"), "secondary text, ruler labels"),
    text(
        "muted",
        Token("raised"),
        "secondary text on raised (toast bodies)",
    ),
    text(
        "muted",
        Over("hover", "surface"),
        "rows on surface keep muted under hover (R2)",
    ),
    text(
        "text",
        Over("selected", "surface"),
        "a selected row's text (R1)",
    ),
    text(
        "text",
        Over("selected", "raised"),
        "a selected row on raised (R1, R2)",
    ),
    text("accent-text", Token("bg"), "links, selected labels"),
    text("accent-text", Token("surface"), "links, selected labels"),
    text("accent-text", Token("raised"), "toast actions, links"),
    text(
        "accent-text",
        Over("hover", "surface"),
        "flat buttons under hover (R6)",
    ),
    text(
        "accent-text",
        Over("hover", "raised"),
        "flat buttons under hover (R6)",
    ),
    text("record-text", Token("bg"), "errors, Cancel countdown"),
    text("record-text", Token("surface"), "Purge from disk, errors"),
    text(
        "record-text",
        Token("raised"),
        "error icons in toasts, Failed at",
    ),
    text(
        "record-text",
        Over("hover", "surface"),
        "destructive flat buttons under hover (R6)",
    ),
    text(
        "record-text",
        Over("hover", "raised"),
        "flat destructive buttons in dialogs (P6)",
    ),
    text("warning-text", Token("bg"), "conflicts, the too-short chip"),
    text("warning-text", Token("surface"), "conflicts, warning icons"),
    text("warning-text", Token("raised"), "warning icons in toasts"),
    text(
        "warning-text",
        Over("hover", "surface"),
        "warning rows on surface under hover",
    ),
    text(
        "text-on-accent",
        Token("accent"),
        "the primary button's label",
    ),
    text(
        "text-on-record",
        Token("record"),
        "REC badge, Record, destructive labels",
    ),
    text("text", Token("keycap"), "key-cap legends"),
    text("muted", Token("field"), "placeholders"),
    text(
        "video-text",
        OverWhite("video-scrim"),
        "badges and plates over video (R5)",
    ),
    // --------------------------------------------- the audit: indicators
    indicator("focus-ring", Token("bg"), "the focus ring"),
    indicator("focus-ring", Token("surface"), "the focus ring"),
    indicator("focus-ring", Token("raised"), "the focus ring"),
    indicator(
        "border-strong",
        Token("field"),
        "field, toggle and checkbox outlines",
    ),
    indicator(
        "border-strong",
        Token("surface"),
        "control outlines on the chrome",
    ),
    indicator(
        "border-strong",
        Token("raised"),
        "control outlines on raised",
    ),
    indicator("playhead", Token("bg"), "the playhead over the lanes"),
    indicator(
        "playhead",
        Token("surface"),
        "the playhead's head on the ruler (P5)",
    ),
    indicator(
        "accent",
        Over("selected", "surface"),
        "a selected row's stroke (R1)",
    ),
    indicator(
        "accent",
        Over("selected", "raised"),
        "a selected row's stroke on raised",
    ),
    indicator(
        "record",
        Token("bg"),
        "the ghost clip outline, the meter's clip zone",
    ),
    indicator("mud", Token("bg"), "the export bar's fill"),
    indicator("warning", Token("bg"), "the silence hatch's edge"),
    indicator("wave", Token("bg"), "waveforms"),
    indicator("meter-green", Token("bg"), "the meter, −60 to −18 dBFS"),
    indicator("meter-yellow", Token("bg"), "the meter, −18 to −6 dBFS"),
    indicator("meter-red", Token("bg"), "the meter, −6 to 0 dBFS"),
    indicator("take-moss", Token("bg"), "take 1's stripe and tint"),
    indicator("take-donkey", Token("bg"), "take 2's stripe and tint"),
    indicator("take-onion", Token("bg"), "take 3's stripe and tint"),
    indicator("take-mud", Token("bg"), "take 4's stripe and tint"),
    indicator("take-lily", Token("bg"), "take 5's stripe and tint"),
    indicator("take-ginger", Token("bg"), "take 6's stripe and tint"),
    indicator("take-dragon", Token("bg"), "take 7's stripe and tint"),
    indicator("take-fairy", Token("bg"), "take 8's stripe and tint"),
    // ------------------------------- DESIGN's Components, past the audit
    text(
        "text",
        Token("field"),
        "field values (Inputs: `mono` in a `field`)",
    ),
    text(
        "text",
        Over("warning-20", "surface"),
        "M on: its letter stays text (Timeline)",
    ),
    text(
        "text",
        Over("accent-2-20", "surface"),
        "S on: its letter stays text (Timeline)",
    ),
    indicator("warning", Token("surface"), "M on: its 1 px warning stroke"),
    indicator(
        "accent-2",
        Token("surface"),
        "S on: its 1 px accent-2 stroke",
    ),
    indicator(
        "success",
        Token("raised"),
        "the success toast's check (Lists)",
    ),
    indicator(
        "meter-green",
        Token("field"),
        "the meter on its field track (panel)",
    ),
    indicator(
        "meter-yellow",
        Token("field"),
        "the meter on its field track (panel)",
    ),
    indicator(
        "meter-red",
        Token("field"),
        "the meter on its field track (panel)",
    ),
    indicator("playhead", Token("raised"), "the scrub flag's stroke (R4)"),
    indicator("caret", Token("bg"), "the insertion caret across the lanes"),
    indicator(
        "record-text",
        Token("field"),
        "an invalid field's stroke (Inputs)",
    ),
    // ------------- C01's pressed state (G§0's M4 decision: AA + 0.05 in
    // every state, pressed included). A pressed button adds `pressed` over
    // its fill or its ground; a colored label turns `text` while pressed
    // (common.css, chrome.css: `accent-text` and `record-text` fail under
    // `pressed`, [`colored_labels_fail_under_pressed`]), and destructive
    // flat buttons press to a `ghost-fill` ground (README known gap).
    text(
        "text",
        Over("pressed", "bg"),
        "a pressed button on the window ground (the transport)",
    ),
    text(
        "text",
        Over("pressed", "surface"),
        "a pressed button on the chrome; Cancel's and the bin's labels, pressed",
    ),
    text(
        "text",
        Over("pressed", "raised"),
        "a pressed button on raised; a toast action's label, pressed",
    ),
    text(
        "text-on-accent",
        Over("pressed", "accent"),
        "a pressed primary button",
    ),
    text(
        "text-on-record",
        Over("pressed", "record"),
        "a pressed Record or destructive button",
    ),
    text(
        "text",
        Over("ghost-fill", "surface"),
        "a pressed destructive-flat button (Purge from disk, the bin)",
    ),
    text(
        "text",
        Over("ghost-fill", "raised"),
        "a pressed destructive-flat button on raised",
    ),
    // --------------------------------- C07's digit (G§0's M4 decision)
    text(
        "video-text",
        OverDimmedWhite("video-scrim"),
        "the countdown's digit and its caption on their video-scrim plates over a white screen",
    ),
];

/// A pair the PO accepted under the margin (it must still pass AA).
struct Waiver {
    theme: &'static str,
    fg: &'static str,
    ground: &'static str,
    why: &'static str,
}

const WAIVERS: &[Waiver] = &[Waiver {
    theme: "swamp-day",
    fg: "text-on-record",
    ground: "record",
    why: "P4: label on fill 4.53 passes AA; not darkened further, which would shrink the deutan \
          gap to accent",
}];

fn waiver(theme: &str, pair: &Pair) -> Option<&'static Waiver> {
    WAIVERS
        .iter()
        .find(|w| w.theme == theme && w.fg == pair.fg && w.ground == pair.ground.describe())
}

/// The ratio of `pair` in `theme`'s tokens.
fn pair_ratio(tokens: &Tokens, pair: &Pair) -> f64 {
    ratio(opaque(tokens, pair.fg), pair.ground.color(tokens))
}

#[test]
fn every_pair_meets_aa_with_the_pos_margin_in_every_theme() {
    let mut failures = Vec::new();
    let mut waived = Vec::new();
    for theme in THEMES {
        let tokens = read_tokens(theme);
        for pair in PAIRS {
            let got = pair_ratio(&tokens, pair);
            let floor = pair.need.floor();
            let line = format!(
                "{theme}: {} on {} is {got:.2}:1 ({})",
                pair.fg,
                pair.ground.describe(),
                pair.what
            );
            match waiver(theme, pair) {
                Some(w) => {
                    if got < floor {
                        failures.push(format!("{line}, under AA's {floor} even with {}", w.why));
                    } else if got >= floor + MARGIN {
                        failures.push(format!(
                            "{line}: the waiver isn't needed any more ({}); remove it",
                            w.why
                        ));
                    }
                    waived.push(line);
                }
                None if got < floor + MARGIN => failures.push(format!(
                    "{line}, needs {:.2} ({floor} + the PO's {MARGIN} margin)",
                    floor + MARGIN
                )),
                None => {}
            }
        }
    }
    assert!(
        failures.is_empty(),
        "contrast below the rule (DESIGN.md → Colors → Decisions):\n{}",
        failures.join("\n")
    );
    assert_eq!(
        waived.len(),
        WAIVERS.len(),
        "every waiver names a pair: {waived:?}"
    );
}

/// Why a pressed button's colored label turns `text` (WP10's contrast
/// audit, G§0's M4 decision): `accent-text` and `record-text` under
/// `pressed` (a toast's actions on `raised`, the countdown's Cancel on
/// `surface`) and `record-text` on a destructive flat button's pressed
/// `ghost-fill` fall below AA + 0.05 in some theme. When the tokens change
/// so that every one of them passes, this fails: the labels can keep their
/// color pressed (drop the `:active` rules [`pressed_colored_labels_turn_text`]
/// holds).
#[test]
fn colored_labels_fail_under_pressed() {
    let cases = [
        text("accent-text", Over("pressed", "raised"), "a toast's action"),
        text(
            "record-text",
            Over("pressed", "surface"),
            "the countdown's Cancel",
        ),
        text(
            "record-text",
            Over("ghost-fill", "surface"),
            "Purge from disk (the bin)",
        ),
    ];
    for pair in cases {
        let ratios: Vec<(&str, f64)> = THEMES
            .iter()
            .map(|theme| (*theme, pair_ratio(&read_tokens(theme), &pair)))
            .collect();
        assert!(
            ratios.iter().any(|(_, r)| *r < Need::Text.floor() + MARGIN),
            "{} on {} ({}) passes in every theme now: {ratios:?}",
            pair.fg,
            pair.ground.describe(),
            pair.what
        );
    }
}

/// The rules of `files` (comments dropped): each selector of a rule with
/// its declarations.
fn css_rules(files: &[&str]) -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/theme/components");
    let mut rules = Vec::new();
    for file in files {
        let mut text = fs::read_to_string(dir.join(file)).expect("a stylesheet");
        while let Some(start) = text.find("/*") {
            let end = text[start..]
                .find("*/")
                .map_or(text.len(), |e| start + e + 2);
            text.replace_range(start..end, "");
        }
        for block in text.split('}') {
            let Some((selectors, body)) = block.split_once('{') else {
                continue;
            };
            for selector in selectors.split(',') {
                rules.push((selector.trim().to_owned(), body.trim().to_owned()));
            }
        }
    }
    rules
}

/// Pressed, the colored labels the audit can't pass turn `text`.
#[test]
fn pressed_colored_labels_turn_text() {
    let rules = css_rules(&["common.css", "chrome.css"]);
    for selector in [
        ".qrec-toast button.qrec-btn.toast-action:active",
        "button.qrec-btn.record.cancel:active",
        "button.qrec-btn.destructive-flat:active",
    ] {
        let colors: Vec<&str> = rules
            .iter()
            .filter(|(s, _)| s == selector)
            .flat_map(|(_, body)| body.split(';'))
            .filter_map(|decl| decl.trim().strip_prefix("color:"))
            .map(str::trim)
            .collect();
        assert_eq!(
            colors,
            ["var(--qrec-text)"],
            "{selector} draws its label in `text`"
        );
    }
}

/// Why the countdown's digit has a plate (G§0's M4 decision): over the
/// dim alone it reads 2.56:1 over a white screen (`video-text` on black at
/// 40 % over white; WP10 read 2.53 at an exact 40 %). When it passes, the
/// plate can go.
#[test]
fn the_countdown_digit_needs_its_plate() {
    let tokens = read_tokens("swamp-night");
    let alone = pair_ratio(
        &tokens,
        &text("video-text", Ground::DimmedWhite, "the digit on the dim"),
    );
    assert!(alone < 3.0, "the digit on the dim alone reads {alone:.2}");
    let plated = pair_ratio(
        &tokens,
        &text("video-text", OverDimmedWhite("video-scrim"), "the digit"),
    );
    assert!(plated > 10.0, "on its plate: {plated:.2}");
}

/// The method reads DESIGN's audit: the Decisions table's binding pairs, to
/// their two decimals (P6's "4.55 in all four" is the audit's min of
/// `surface` and `raised`: Swamp Day's binds over `surface`).
#[test]
fn the_decided_values_bind_where_design_says() {
    let cases: [(&str, Pair, f64); 13] = [
        (
            "swamp-day",
            text("muted", Over("hover", "surface"), "P1"),
            4.56,
        ),
        ("ogre-mode", text("muted", Token("raised"), "P2"), 4.58),
        (
            "swamp-day",
            text("accent-text", Over("hover", "surface"), "P3"),
            4.56,
        ),
        (
            "swamp-day",
            text("text-on-record", Token("record"), "P4"),
            4.53,
        ),
        (
            "swamp-day",
            indicator("playhead", Token("surface"), "P5"),
            3.06,
        ),
        (
            "swamp-night",
            text("record-text", Over("hover", "raised"), "P6"),
            4.55,
        ),
        (
            "swamp-day",
            text("record-text", Over("hover", "surface"), "P6"),
            4.55,
        ),
        (
            "dragons-keep",
            text("record-text", Over("hover", "raised"), "P6"),
            4.55,
        ),
        (
            "ogre-mode",
            text("record-text", Over("hover", "raised"), "P6"),
            4.55,
        ),
        (
            "dragons-keep",
            text("accent-text", Over("hover", "raised"), "P7"),
            4.57,
        ),
        (
            "swamp-day",
            text("warning-text", Over("hover", "surface"), "P8"),
            4.57,
        ),
        (
            "ogre-mode",
            text("warning-text", Token("raised"), "P8"),
            4.55,
        ),
        (
            "ogre-mode",
            text("accent-text", Over("hover", "raised"), "—"),
            4.56,
        ),
    ];
    let mut off = Vec::new();
    for (theme, pair, design) in cases {
        let got = pair_ratio(&read_tokens(theme), &pair);
        if ((got * 100.0).round() / 100.0 - design).abs() > 1e-9 {
            off.push(format!(
                "{theme} {}: {} on {} reads {got:.4}, DESIGN says {design}",
                pair.what,
                pair.fg,
                pair.ground.describe()
            ));
        }
    }
    assert!(off.is_empty(), "{}", off.join("\n"));
}

/// WCAG's own anchors, and the 8-bit compositing.
#[test]
fn the_ratio_and_the_compositing_are_wcags() {
    let black = Rgba::parse("#000000").unwrap();
    let white = Rgba::parse("#FFFFFF").unwrap();
    assert!((ratio(black, white) - 21.0).abs() < 1e-9);
    assert!((ratio(white, white) - 1.0).abs() < 1e-9);
    // #767676 on white is the classic 4.54:1.
    let grey = Rgba::parse("#767676").unwrap();
    assert!((ratio(grey, white) - 4.54).abs() < 0.005);
    // Black at 50 % (0x80) over white: 127.5 → 127 or 128, rounded.
    let half = Rgba::parse("#00000080").unwrap().over(white);
    assert_eq!(half.rgb, [127, 127, 127]);
    assert_eq!(half.alpha, 255);
    assert_eq!(Rgba::parse("#12"), None);
    assert_eq!(
        Rgba::parse("#E5483B73"),
        Some(Rgba {
            rgb: [0xE5, 0x48, 0x3B],
            alpha: 0x73
        })
    );
}

/// Every pair names tokens the generator writes, opaque where drawn
/// opaque, in every theme (a renamed token fails here, not as a panic in
/// the middle of the audit).
#[test]
fn every_pair_names_real_tokens() {
    for theme in THEMES {
        let tokens = read_tokens(theme);
        for pair in PAIRS {
            assert!(tokens.contains_key(pair.fg), "{theme}: no {}", pair.fg);
            for name in pair.ground.tokens() {
                assert!(tokens.contains_key(name), "{theme}: no {name}");
            }
        }
    }
}
