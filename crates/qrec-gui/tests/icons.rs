//! The symbolic icons (G§3.14, G§5.2): every Lucide icon the frames draw,
//! installed in the GResource as
//! `resources/icons/scalable/actions/qrec-<icon>-symbolic.svg`.
//!
//! **The generator.** pen.dev exports each icon the frames draw as
//! `design/export/icons/<icon>@<w>x<h>.svg`: one path, outlined to fills, on
//! a 14-unit grid (Lucide's 24-unit icon at 14/24, its 2-unit stroke
//! outlined), drawn into a `w`×`h` box (`xMidYMid meet`; the box is square,
//! so the grid fills it). Every size of an icon shares that path. This test
//! writes it on GTK's 16 px grid (each coordinate × 16/14), so a 16 px
//! request draws exactly what a 16 px frame draws and a 20 px one what a
//! 20 px frame draws. Two icons the frames draw but don't export as Lucide
//! icons come from the frames too: the header's Stop square (`qrec-stop`,
//! S04's "Stop square") and, not drawn anywhere (no frame shows playback
//! running), Lucide's own `pause` (DESIGN Icons: Play / Pause) outlined the
//! same way. The REC glyph (`qrec-record`) is the art's (`tests/art.rs`).
//! This test renders every file in memory and fails when a committed one
//! differs (`QREC_UPDATE_ICONS=1` writes them); it is skipped when
//! `design/` is absent (the Nix build has no `design/`). S04's HTML isn't
//! committed (`design/qrec.pen` is the source): the Stop square is checked
//! only after exporting S04 through the pen MCP to
//! `design/export/html/S04.html` (gitignored).
//!
//! **The markup** (checked without `design/`): GTK 4.22 loads a
//! `-symbolic.svg` from a resource as render nodes
//! (`gsk_render_node_new_from_resource_symbolic`, `gtk/gdktextureutils.c`):
//! a `<svg>` with `width` and `height` (a `viewBox` must be `0 0 width
//! height`), then only `<path>`, `<circle>` and `<rect>` (no transforms: a
//! `<g>` is skipped with its attributes); a shape without a `class` is
//! filled with the foreground color, the symbolic color GTK gives the
//! widget's CSS `color`; an attribute it doesn't know makes the whole file
//! fall back to the rsvg texture path. So each icon here is a 16×16 `<svg>`
//! of shapes with geometry only: no `class` (foreground: GTK recolors
//! every pixel), no `fill` or `stroke` (fill-only), no `transform`, no
//! `opacity`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Where the icons live in the GResource's sources.
fn icons_dir() -> PathBuf {
    crate_dir().join("resources/icons/scalable/actions")
}

fn design_dir() -> Option<PathBuf> {
    let dir = crate_dir().join("../../design/export");
    dir.join("icons").is_dir().then_some(dir)
}

/// The export's grid and GTK's.
const EXPORT_GRID: f64 = 14.0;
const GRID: f64 = 16.0;

/// Lucide's `pause` (lucide 1.0, ISC: `<rect x="14" y="3" width="5"
/// height="18" rx="1"/>` and the same at x 5), on its 24-unit grid with its
/// 2-unit stroke and round joins: the transport's Pause while Program plays,
/// which no frame draws.
const LUCIDE_PAUSE: [[f64; 5]; 2] = [[14.0, 3.0, 5.0, 18.0, 1.0], [5.0, 3.0, 5.0, 18.0, 1.0]];
const LUCIDE_GRID: f64 = 24.0;
const LUCIDE_STROKE: f64 = 2.0;

// ------------------------------------------------------------ path data

/// A path's commands and numbers, in order.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Command(char),
    Number(f64),
}

/// The path commands SVG has.
const COMMANDS: &str = "MmLlHhVvCcSsQqTtAaZz";

fn tokenize(d: &str) -> Vec<Token> {
    let bytes = d.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if COMMANDS.contains(c) {
            tokens.push(Token::Command(c));
            i += 1;
        } else if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() {
            let start = i;
            if c == '-' || c == '+' {
                i += 1;
            }
            let mut dot = false;
            while i < bytes.len() {
                let b = bytes[i] as char;
                if b.is_ascii_digit() {
                    i += 1;
                } else if b == '.' && !dot {
                    dot = true;
                    i += 1;
                } else if (b == 'e' || b == 'E')
                    && bytes
                        .get(i + 1)
                        .is_some_and(|n| n.is_ascii_digit() || *n == b'-' || *n == b'+')
                {
                    i += 2;
                    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                    break;
                } else {
                    break;
                }
            }
            let text = &d[start..i];
            let value: f64 = text
                .parse()
                .unwrap_or_else(|e| panic!("{text:?} in a path isn't a number: {e}"));
            tokens.push(Token::Number(value));
        } else if c == ',' || c.is_ascii_whitespace() {
            i += 1;
        } else {
            panic!("{c:?} in a path: {d}");
        }
    }
    tokens
}

/// `v` with at most 4 decimals, no trailing zeros (`-0` is `0`).
fn number(v: f64) -> String {
    let text = format!("{v:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match text {
        "-0" | "" => "0".to_owned(),
        t => t.to_owned(),
    }
}

/// Path data from tokens, compact: a command then its numbers, a space
/// between numbers but before a minus sign.
fn path_data(tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut after_number = false;
    for t in tokens {
        match t {
            Token::Command(c) => {
                out.push(*c);
                after_number = false;
            }
            Token::Number(v) => {
                let n = number(*v);
                if after_number && !n.starts_with('-') {
                    out.push(' ');
                }
                out.push_str(&n);
                after_number = true;
            }
        }
    }
    out
}

/// `v` rounded to the 4 decimals [`number`] writes.
fn round4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

/// `d` scaled by `k` about the origin, its relative coordinates kept
/// relative. Each point is rounded to 4 decimals where it lands, and a
/// relative step is the difference of rounded points, so rounding doesn't
/// add up along a path (each point within 0.00005 of its exact place). The
/// export's paths use `M`, `m`, `L`, `l`, `Q`, `q` and `Z`, `z` only.
fn scaled(d: &str, k: f64) -> String {
    let tokens = tokenize(d);
    let mut out = Vec::with_capacity(tokens.len());
    // The exact current point and the written (rounded) one, and their
    // subpath starts.
    let (mut exact, mut written) = ((0.0, 0.0), (0.0, 0.0));
    let (mut exact_start, mut written_start) = ((0.0, 0.0), (0.0, 0.0));
    let mut cmd = 'M';
    let mut i = 0;
    while i < tokens.len() {
        if let Token::Command(c) = tokens[i] {
            assert!("MmLlQqZz".contains(c), "{c:?} in an export path: {d}");
            out.push(tokens[i].clone());
            cmd = c;
            i += 1;
            if matches!(c, 'Z' | 'z') {
                (exact, written) = (exact_start, written_start);
            }
            continue;
        }
        let pairs = if matches!(cmd, 'Q' | 'q') { 2 } else { 1 };
        let relative = cmd.is_ascii_lowercase();
        let base = exact;
        let mut end = (exact, written);
        for _ in 0..pairs {
            let value = |t: &Token| match t {
                Token::Number(v) => *v,
                Token::Command(c) => panic!("{c:?} where a number goes in {d}"),
            };
            let (x, y) = (value(&tokens[i]) * k, value(&tokens[i + 1]) * k);
            i += 2;
            let point = if relative {
                (base.0 + x, base.1 + y)
            } else {
                (x, y)
            };
            let landed = (round4(point.0), round4(point.1));
            let (wx, wy) = if relative {
                (landed.0 - written.0, landed.1 - written.1)
            } else {
                landed
            };
            out.push(Token::Number(wx));
            out.push(Token::Number(wy));
            end = (point, landed);
        }
        (exact, written) = end;
        if matches!(cmd, 'M' | 'm') {
            (exact_start, written_start) = (exact, written);
            cmd = if cmd == 'M' { 'L' } else { 'l' };
        }
    }
    path_data(&out)
}

/// The end points of a path's segments (and its control points), absolute:
/// enough for a bounding box of the shapes this folder holds.
fn points(d: &str) -> Vec<(f64, f64)> {
    let tokens = tokenize(d);
    let mut out = Vec::new();
    let (mut x, mut y) = (0.0, 0.0);
    let (mut sx, mut sy) = (0.0, 0.0);
    let mut i = 0;
    let mut cmd = 'M';
    while i < tokens.len() {
        if let Token::Command(c) = tokens[i] {
            cmd = c;
            i += 1;
            if matches!(c, 'Z' | 'z') {
                (x, y) = (sx, sy);
            }
            continue;
        }
        let arity = match cmd {
            'M' | 'm' | 'L' | 'l' | 'T' | 't' => 2,
            'H' | 'h' | 'V' | 'v' => 1,
            'C' | 'c' => 6,
            'S' | 's' | 'Q' | 'q' => 4,
            'A' | 'a' => 7,
            other => panic!("numbers after {other:?} in {d}"),
        };
        let args: Vec<f64> = tokens[i..i + arity]
            .iter()
            .map(|t| match t {
                Token::Number(v) => *v,
                Token::Command(c) => panic!("{c:?} where a number goes in {d}"),
            })
            .collect();
        i += arity;
        let relative = cmd.is_ascii_lowercase();
        let (bx, by) = if relative { (x, y) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            'H' => x = bx + args[0],
            'V' => y = by + args[0],
            'A' => (x, y) = (bx + args[5], by + args[6]),
            _ => {
                for pair in args[..arity - 2].chunks(2) {
                    out.push((bx + pair[0], by + pair[1]));
                }
                (x, y) = (bx + args[arity - 2], by + args[arity - 1]);
            }
        }
        out.push((x, y));
        if matches!(cmd, 'M' | 'm') {
            (sx, sy) = (x, y);
            cmd = if cmd == 'M' { 'L' } else { 'l' };
        }
    }
    out
}

// ------------------------------------------------------------- the export

/// One icon of the export: its name and its path on the 14-unit grid.
#[derive(Debug)]
struct Exported {
    name: String,
    d: String,
    /// The files it came from (`archive@16x16.svg`), every size.
    files: Vec<String>,
}

/// `name="value"` of the first tag carrying `name` in `text`.
fn attribute<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = text.find(&key)? + key.len();
    let end = text[start..].find('"')? + start;
    Some(&text[start..end])
}

fn read_export(dir: &Path) -> Vec<Exported> {
    let mut by_name: BTreeMap<String, Exported> = BTreeMap::new();
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "svg"))
        .collect();
    files.sort();
    for file in files {
        let base = file.file_name().unwrap().to_string_lossy().into_owned();
        let (name, size) = base
            .trim_end_matches(".svg")
            .split_once('@')
            .unwrap_or_else(|| panic!("{base}: not <icon>@<w>x<h>.svg"));
        let (w, h) = size
            .split_once('x')
            .unwrap_or_else(|| panic!("{base}: not <icon>@<w>x<h>.svg"));
        assert_eq!(w, h, "{base}: drawn into a square box");
        let text = fs::read_to_string(&file).unwrap_or_else(|e| panic!("{base}: {e}"));
        assert_eq!(attribute(&text, "data-icon-name"), Some(name), "{base}");
        assert_eq!(attribute(&text, "data-icon-set"), Some("lucide"), "{base}");
        let view_box: Vec<f64> = attribute(&text, "viewBox")
            .unwrap_or_else(|| panic!("{base}: no viewBox"))
            .split_whitespace()
            .map(|v| v.parse().expect("a viewBox number"))
            .collect();
        assert_eq!(view_box.len(), 4, "{base}");
        assert!(
            view_box[0] == 0.0
                && view_box[1] == 0.0
                && (view_box[2] - EXPORT_GRID).abs() < 1e-3
                && view_box[3] == EXPORT_GRID,
            "{base}: on the 14-unit grid: {view_box:?}"
        );
        assert_eq!(
            attribute(&text, "preserveAspectRatio"),
            Some("xMidYMid meet"),
            "{base}"
        );
        assert_eq!(text.matches("<path").count(), 1, "{base}: one path");
        let d = attribute(&text, "d")
            .unwrap_or_else(|| panic!("{base}: no path data"))
            .to_owned();
        let entry = by_name.entry(name.to_owned()).or_insert_with(|| Exported {
            name: name.to_owned(),
            d: d.clone(),
            files: Vec::new(),
        });
        assert_eq!(
            entry.d, d,
            "{base}: every size of {name} shares one path (design/export/INDEX.md)"
        );
        entry.files.push(base);
    }
    by_name.into_values().collect()
}

// ---------------------------------------------------------- the generator

fn icon_file(name: &str) -> String {
    format!("qrec-{name}-symbolic.svg")
}

/// An icon's file: the shapes on the 16 px grid, after a comment saying
/// where they come from.
fn icon_svg(comment: &str, shapes: &[String]) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    writeln!(out, "<!-- {comment} -->").unwrap();
    out.push_str(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\" viewBox=\"0 0 16 16\">\n",
    );
    for shape in shapes {
        writeln!(out, "  {shape}").unwrap();
    }
    out.push_str("</svg>\n");
    out
}

/// Where a file comes from (no double hyphen: it goes in an XML comment).
const GENERATED: &str = "generated by crates/qrec-gui/tests/icons.rs (QREC_UPDATE_ICONS=1 \
writes it)";

fn lucide_icon(icon: &Exported) -> String {
    let comment = format!(
        "Lucide's {} as design/qrec.pen draws it (design/export/icons/{}), on GTK's \
         16 px grid: {GENERATED}. ISC license: ../../LICENSE-lucide.txt.",
        icon.name,
        icon.files.join(", ")
    );
    let d = scaled(&icon.d, GRID / EXPORT_GRID);
    icon_svg(&comment, &[format!("<path d=\"{d}\"/>")])
}

/// The style declarations of the element named `name` in a frame's HTML.
fn styles_of(html: &str, name: &str) -> BTreeMap<String, String> {
    let key = format!("data-pencil-name=\"{name}\"");
    let at = html
        .find(&key)
        .unwrap_or_else(|| panic!("no {name:?} in the frame"));
    let rest = &html[at..];
    let style = rest
        .find("style=\"")
        .map(|s| &rest[s + 7..])
        .and_then(|s| s.split('"').next())
        .unwrap_or_else(|| panic!("{name:?} has no style"));
    style
        .split(';')
        .filter_map(|decl| decl.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}

fn px(styles: &BTreeMap<String, String>, key: &str) -> f64 {
    let value = styles
        .get(key)
        .unwrap_or_else(|| panic!("no {key} in {styles:?}"));
    value
        .strip_suffix("px")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{key}: {value} isn't px"))
}

/// The header's Stop square as S04 draws it: a `border-radius` square
/// centered in the 16 px "Stop" box (the Record button while recording).
fn stop_icon(design: &Path) -> String {
    let html = fs::read_to_string(design.join("html/S04.html")).expect("S04's HTML");
    let boxed = styles_of(&html, "Stop");
    assert_eq!((px(&boxed, "width"), px(&boxed, "height")), (GRID, GRID));
    assert_eq!(
        boxed.get("justify-content").map(String::as_str),
        Some("center")
    );
    assert_eq!(boxed.get("align-items").map(String::as_str), Some("center"));
    let square = styles_of(&html, "Stop square");
    let (w, h) = (px(&square, "width"), px(&square, "height"));
    let r = px(&square, "border-radius");
    let (x, y) = ((GRID - w) / 2.0, (GRID - h) / 2.0);
    let comment = format!(
        "The Record button's Stop square as design/qrec.pen draws it (S04, \"Stop square\": \
         {w}x{h}, radius {r}, centered in 16 px): {GENERATED}."
    );
    icon_svg(
        &comment,
        &[format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{}\"/>",
            number(x),
            number(y),
            number(w),
            number(h),
            number(r)
        )],
    )
}

/// A rounded rectangle's outline as one closed subpath, clockwise on screen
/// (`r` 0: square corners).
fn rounded_rect(x: f64, y: f64, w: f64, h: f64, r: f64) -> Vec<Token> {
    use Token::{Command as C, Number as N};
    let (x1, y1) = (x + w, y + h);
    let mut t = vec![C('M'), N(x + r), N(y), C('H'), N(x1 - r)];
    let arc = |t: &mut Vec<Token>, ex: f64, ey: f64| {
        if r > 0.0 {
            t.extend([C('A'), N(r), N(r), N(0.0), N(0.0), N(1.0), N(ex), N(ey)]);
        }
    };
    arc(&mut t, x1, y + r);
    t.extend([C('V'), N(y1 - r)]);
    arc(&mut t, x1 - r, y1);
    t.extend([C('H'), N(x + r)]);
    arc(&mut t, x, y1 - r);
    t.extend([C('V'), N(y + r)]);
    arc(&mut t, x + r, y);
    t.push(C('Z'));
    t
}

/// A rectangle as one closed subpath, counter-clockwise on screen: a hole
/// in a clockwise one under the nonzero rule.
fn hole(x: f64, y: f64, w: f64, h: f64) -> Vec<Token> {
    use Token::{Command as C, Number as N};
    vec![
        C('M'),
        N(x),
        N(y),
        C('V'),
        N(y + h),
        C('H'),
        N(x + w),
        C('V'),
        N(y),
        C('Z'),
    ]
}

/// Lucide's `pause`, outlined as pen.dev outlines the frames' icons: each
/// bar's 2-unit stroke is a ring from 1 unit outside it (its corners `rx` +
/// 1, round joins) to 1 unit inside (square: `rx` − 1 is 0), scaled from
/// the 24-unit grid to the 16 px one.
fn pause_icon() -> String {
    let k = GRID / LUCIDE_GRID;
    let half = LUCIDE_STROKE / 2.0;
    let mut tokens = Vec::new();
    for [x, y, w, h, rx] in LUCIDE_PAUSE {
        tokens.extend(rounded_rect(
            (x - half) * k,
            (y - half) * k,
            (w + LUCIDE_STROKE) * k,
            (h + LUCIDE_STROKE) * k,
            (rx + half) * k,
        ));
        assert!(rx - half <= 0.0, "the inner corners are square");
        tokens.extend(hole(
            (x + half) * k,
            (y + half) * k,
            (w - LUCIDE_STROKE) * k,
            (h - LUCIDE_STROKE) * k,
        ));
    }
    let comment = format!(
        "Lucide's pause (lucide 1.0: two 5x18 bars, radius 1, stroke 2) outlined like the \
         frames' icons, on GTK's 16 px grid; no frame draws it (DESIGN Icons: Play / Pause): \
         {GENERATED}. ISC license: ../../LICENSE-lucide.txt."
    );
    icon_svg(&comment, &[format!("<path d=\"{}\"/>", path_data(&tokens))])
}

/// Every icon this test generates, by file name.
fn generated(design: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for icon in read_export(&design.join("icons")) {
        out.insert(icon_file(&icon.name), lucide_icon(&icon));
    }
    if design.join("html/S04.html").is_file() {
        out.insert(icon_file("stop"), stop_icon(design));
    }
    out.insert(icon_file("pause"), pause_icon());
    out
}

/// `QREC_UPDATE_ICONS=1`: the generator writes the files (the tests that
/// read them don't run meanwhile: they would race it).
fn updating() -> bool {
    std::env::var("QREC_UPDATE_ICONS").is_ok_and(|v| v == "1")
}

/// Writes `content` to `path` under `QREC_UPDATE_ICONS=1`, else notes that
/// the committed file differs.
fn check_or_write(path: &Path, content: &str, stale: &mut Vec<String>) {
    let update = updating();
    let current = fs::read_to_string(path).unwrap_or_default();
    if current == content {
        return;
    }
    if update {
        fs::write(path, content).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        eprintln!("wrote {}", path.display());
    } else {
        stale.push(path.display().to_string());
    }
}

#[test]
fn the_icons_are_the_frames() {
    let Some(design) = design_dir() else {
        eprintln!("design/ is absent: the icons aren't checked against the export");
        return;
    };
    let icons = generated(&design);
    // The frames' Lucide icons (design/export/INDEX.md), the pause and, with
    // S04's HTML, the Stop square.
    let stop = usize::from(design.join("html/S04.html").is_file());
    assert_eq!(icons.len(), 49 + 1 + stop, "{:?}", icons.keys());
    let mut stale = Vec::new();
    for (file, svg) in &icons {
        check_or_write(&icons_dir().join(file), svg, &mut stale);
    }
    assert!(
        stale.is_empty(),
        "icons out of date (QREC_UPDATE_ICONS=1 cargo test -p qrec-gui --test icons \
         rewrites them): {stale:#?}"
    );
    // Nothing else: a placeholder no frame uses goes (the REC glyph is the
    // art's; without S04's HTML the committed Stop square stays as it is).
    let extra: Vec<String> = installed()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .filter(|f| !icons.contains_key(f) && !GENERATED_ELSEWHERE.contains(&f.as_str()))
        .filter(|f| stop == 1 || *f != icon_file("stop"))
        .collect();
    assert!(
        extra.is_empty(),
        "icons no generator writes (a placeholder?): {extra:?}"
    );
}

/// The icons another generator writes into the folder: the REC glyph is the
/// art's (`tests/art.rs`).
const GENERATED_ELSEWHERE: [&str; 1] = ["qrec-record-symbolic.svg"];

// ------------------------------------------- the icons the GUI names

/// G§5.2's Adwaita stand-ins, as the GUI named them until the pen export,
/// and GTK's and libadwaita's own icons for widgets the frames draw (the
/// dropdown's `pan-down`, the check box's `object-select`).
const STAND_INS: [&str; 46] = [
    "media-playback-stop-symbolic",
    "media-playback-start-symbolic",
    "media-playback-pause-symbolic",
    "go-previous-symbolic",
    "go-next-symbolic",
    "media-skip-backward-symbolic",
    "media-skip-forward-symbolic",
    "camera-photo-symbolic",
    "document-save-symbolic",
    "edit-find-symbolic",
    "applications-system-symbolic",
    "preferences-system-symbolic",
    "camera-video-symbolic",
    "video-x-generic-symbolic",
    "view-reveal-symbolic",
    "view-conceal-symbolic",
    "changes-prevent-symbolic",
    "changes-allow-symbolic",
    "list-drag-handle-symbolic",
    "list-add-symbolic",
    "video-display-symbolic",
    "camera-web-symbolic",
    "audio-input-microphone-symbolic",
    "audio-speakers-symbolic",
    "package-x-generic-symbolic",
    "edit-cut-symbolic",
    "preferences-other-symbolic",
    "zoom-in-symbolic",
    "zoom-out-symbolic",
    "zoom-fit-best-symbolic",
    "user-trash-symbolic",
    "folder-open-symbolic",
    "folder-symbolic",
    "edit-copy-symbolic",
    "adw-external-link-symbolic",
    "dialog-information-symbolic",
    "dialog-warning-symbolic",
    "dialog-error-symbolic",
    "object-select-symbolic",
    "pan-down-symbolic",
    "view-more-symbolic",
    "window-close-symbolic",
    "insert-link-symbolic",
    "document-edit-symbolic",
    "edit-undo-symbolic",
    "emblem-ok-symbolic",
];

/// The system icons a frame really draws (none: every icon the frames draw
/// is a Lucide one, the REC glyph or the Stop square).
const SYSTEM_ICONS_THE_FRAMES_DRAW: [&str; 0] = [];

fn files_under(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

/// The icon names in `text`, each once, sorted: string literals ending in
/// `-symbolic` (Rust) and `-gtk-icontheme("…")` sources (CSS).
fn icon_names(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (end, _) in text.match_indices("-symbolic\"") {
        let head = &text[..end];
        let Some(start) = head.rfind('"') else {
            continue;
        };
        let name = &text[start + 1..end + "-symbolic".len()];
        if name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            names.insert(name.to_owned());
        }
    }
    for (at, _) in text.match_indices("-gtk-icontheme(") {
        let rest = &text[at + "-gtk-icontheme(".len()..];
        let quote = rest.chars().next().filter(|q| matches!(q, '"' | '\''));
        if let Some(q) = quote
            && let Some(end) = rest[1..].find(q)
        {
            names.insert(rest[1..1 + end].to_owned());
        }
    }
    names
}

/// Every icon name the GUI's code and stylesheets give, with where.
fn named_icons() -> Vec<(String, String)> {
    let mut files = Vec::new();
    files_under(&crate_dir().join("src"), "rs", &mut files);
    files_under(&crate_dir().join("resources/theme"), "css", &mut files);
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let place = file
            .strip_prefix(crate_dir())
            .unwrap_or(&file)
            .display()
            .to_string();
        for name in icon_names(&text) {
            out.push((name, place.clone()));
        }
    }
    out
}

/// Every icon the GUI names is one of the GResource's (`qrec-<icon>-symbolic`,
/// its file in `resources/icons/scalable/actions/`), and none of G§5.2's
/// stand-ins is left (but a system icon a frame really draws: none).
#[test]
fn every_icon_the_gui_names_is_in_the_gresource() {
    let named = named_icons();
    assert!(named.len() >= 60, "the scan found {} names", named.len());
    let mut wrong = Vec::new();
    for (name, place) in &named {
        if SYSTEM_ICONS_THE_FRAMES_DRAW.contains(&name.as_str()) {
            continue;
        }
        if STAND_INS.contains(&name.as_str()) {
            wrong.push(format!("{place}: {name} is a G§5.2 stand-in"));
        } else if !name.starts_with("qrec-") || !name.ends_with("-symbolic") {
            wrong.push(format!("{place}: {name} isn't a qrec-<icon>-symbolic"));
        } else if !icons_dir().join(format!("{name}.svg")).is_file() {
            wrong.push(format!("{place}: {name} isn't in the GResource"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn the_name_scan_finds_literals_and_css_sources() {
    let rust = r#"let a = "qrec-x-symbolic"; f("go-next-symbolic", 1); // "not a name-symbolic""#;
    assert_eq!(
        icon_names(rust).into_iter().collect::<Vec<_>>(),
        ["go-next-symbolic", "qrec-x-symbolic"],
        "a comment's quoted words with spaces aren't names"
    );
    let css = "a { -gtk-icon-source: -gtk-icontheme(\"qrec-check-symbolic\"); }\n\
               b { -gtk-icon-source: -gtk-icontheme('pan-down-symbolic'); }";
    assert_eq!(
        icon_names(css).into_iter().collect::<Vec<_>>(),
        ["pan-down-symbolic", "qrec-check-symbolic"]
    );
}

/// Each 16 px icon covers the 16 px box as the export's 14-unit path covers
/// its box: the same outline, × 16/14 (a 16 px request draws the frame's
/// 16 px icon, a 20 px one its 20 px icon).
#[test]
fn the_icons_are_on_the_16_px_grid() {
    if updating() {
        return;
    }
    let Some(design) = design_dir() else {
        eprintln!("design/ is absent: the grid isn't checked against the export");
        return;
    };
    for icon in read_export(&design.join("icons")) {
        let ours = fs::read_to_string(icons_dir().join(icon_file(&icon.name)))
            .unwrap_or_else(|e| panic!("{}: {e}", icon_file(&icon.name)));
        let d = attribute(&ours, "d").expect("a path");
        let (a, b) = (points(&icon.d), points(d));
        assert_eq!(a.len(), b.len(), "{}", icon.name);
        for ((x0, y0), (x1, y1)) in a.iter().zip(&b) {
            let k = GRID / EXPORT_GRID;
            assert!(
                (x0 * k - x1).abs() < 1e-4 && (y0 * k - y1).abs() < 1e-4,
                "{}: ({x0}, {y0}) × 16/14 isn't ({x1}, {y1})",
                icon.name
            );
        }
    }
}

// ------------------------------------------------------------ the markup

/// A start tag: its name and attributes, in order.
#[derive(Debug)]
struct Tag {
    name: String,
    attributes: Vec<(String, String)>,
}

/// The start tags of an SVG (self-closing ones too), skipping the XML
/// declaration, comments and end tags. Attributes are `name="value"`.
fn tags(svg: &str) -> Vec<Tag> {
    let mut out = Vec::new();
    let mut rest = svg;
    while let Some(start) = rest.find('<') {
        rest = &rest[start..];
        if let Some(after) = rest.strip_prefix("<!--") {
            let end = after.find("-->").expect("a closed comment");
            assert!(!after[..end].contains("--"), "a comment with --");
            rest = &after[end + 3..];
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("</") {
            rest = &rest[rest.find('>').expect("a closed tag") + 1..];
            continue;
        }
        let end = rest.find('>').expect("a closed tag");
        let inner = rest[1..end].trim_end_matches('/').trim();
        rest = &rest[end + 1..];
        let (name, mut attrs) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
        let mut attributes = Vec::new();
        loop {
            attrs = attrs.trim_start();
            if attrs.is_empty() {
                break;
            }
            let (key, after) = attrs.split_once("=\"").expect("name=\"value\"");
            let (value, after) = after.split_once('"').expect("a closed value");
            attributes.push((key.trim().to_owned(), value.to_owned()));
            attrs = after;
        }
        out.push(Tag {
            name: name.to_owned(),
            attributes,
        });
    }
    out
}

/// Whether `svg` is an icon GTK 4.22 loads as render nodes and recolors in
/// full (see the module's docs): `Err` says why not.
fn gtk_symbolic(svg: &str) -> Result<(), String> {
    let tags = tags(svg);
    let (root, shapes) = tags.split_first().ok_or("no tags")?;
    if root.name != "svg" {
        return Err(format!("the root is <{}>", root.name));
    }
    let get = |tag: &Tag, key: &str| {
        tag.attributes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    if get(root, "width").as_deref() != Some("16")
        || get(root, "height").as_deref() != Some("16")
        || get(root, "viewBox").as_deref() != Some("0 0 16 16")
    {
        return Err("not a 16×16 <svg> with viewBox \"0 0 16 16\"".into());
    }
    if shapes.is_empty() {
        return Err("no shapes".into());
    }
    for tag in shapes {
        let allowed: &[&str] = match tag.name.as_str() {
            "path" => &["d", "fill-rule"],
            "circle" => &["cx", "cy", "r"],
            "rect" => &["x", "y", "width", "height", "rx", "ry"],
            other => return Err(format!("<{other}>: GTK draws only path, circle and rect")),
        };
        for (key, value) in &tag.attributes {
            if !allowed.contains(&key.as_str()) {
                return Err(format!(
                    "<{} {key}=\"{value}\">: only geometry (no class, fill, stroke, \
                     transform or opacity)",
                    tag.name
                ));
            }
            if key == "fill-rule" && !matches!(value.as_str(), "nonzero" | "evenodd") {
                return Err(format!("fill-rule {value}"));
            }
        }
        let numbers = |keys: &[&str]| -> Result<Vec<f64>, String> {
            keys.iter()
                .map(|k| {
                    get(tag, k)
                        .ok_or_else(|| format!("<{}> without {k}", tag.name))?
                        .parse::<f64>()
                        .map_err(|e| format!("{k}: {e}"))
                })
                .collect()
        };
        let inside =
            |(x, y): (f64, f64)| (-1e-3..=16.001).contains(&x) && (-1e-3..=16.001).contains(&y);
        match tag.name.as_str() {
            "path" => {
                let d = get(tag, "d").ok_or("<path> without d")?;
                if let Some(p) = points(&d).into_iter().find(|p| !inside(*p)) {
                    return Err(format!("a point outside the 16 px box: {p:?}"));
                }
            }
            "circle" => {
                let [cx, cy, r] = numbers(&["cx", "cy", "r"])?[..] else {
                    unreachable!()
                };
                if !(inside((cx - r, cy - r)) && inside((cx + r, cy + r))) {
                    return Err("a circle outside the 16 px box".into());
                }
            }
            _ => {
                let [w, h] = numbers(&["width", "height"])?[..] else {
                    unreachable!()
                };
                let x = get(tag, "x")
                    .map_or(Ok(0.0), |v| v.parse::<f64>())
                    .map_err(|e| e.to_string())?;
                let y = get(tag, "y")
                    .map_or(Ok(0.0), |v| v.parse::<f64>())
                    .map_err(|e| e.to_string())?;
                if !(inside((x, y)) && inside((x + w, y + h))) {
                    return Err("a rect outside the 16 px box".into());
                }
            }
        }
    }
    Ok(())
}

fn installed() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(icons_dir())
        .expect("the icons folder")
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

#[test]
fn every_icon_is_a_symbolic_gtk_recolors_in_full() {
    if updating() {
        return;
    }
    let files = installed();
    assert!(files.len() >= 50, "{files:?}");
    let mut wrong = Vec::new();
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        if !(name.starts_with("qrec-") && name.ends_with("-symbolic.svg")) {
            wrong.push(format!("{name}: not qrec-<icon>-symbolic.svg"));
            continue;
        }
        let svg = fs::read_to_string(file).unwrap_or_else(|e| panic!("{name}: {e}"));
        if let Err(why) = gtk_symbolic(&svg) {
            wrong.push(format!("{name}: {why}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The rules catch what GTK's loader can't take or wouldn't recolor.
#[test]
fn the_markup_rules_catch_what_gtk_wouldnt_recolor() {
    let ok = icon_svg("x", &["<path d=\"M1 1h14v14H1z\"/>".to_owned()]);
    assert_eq!(gtk_symbolic(&ok), Ok(()));
    let with = |shape: &str| gtk_symbolic(&icon_svg("x", &[shape.to_owned()]));
    for bad in [
        "<path d=\"M1 1h14v14H1z\" fill=\"#E8E6C8\"/>",
        "<path d=\"M1 1h14v14H1z\" class=\"error\"/>",
        "<path d=\"M1 1h14v14H1z\" stroke=\"currentColor\"/>",
        "<path d=\"M1 1h14v14H1z\" transform=\"scale(2)\"/>",
        "<path d=\"M1 1h14v14H1z\" opacity=\"0.5\"/>",
        "<ellipse cx=\"8\" cy=\"8\" rx=\"4\" ry=\"2\"/>",
        "<g><path d=\"M1 1h14v14H1z\"/></g>",
        "<path d=\"M1 1h20v14H1z\"/>",
        "<circle cx=\"8\" cy=\"8\" r=\"9\"/>",
    ] {
        assert!(with(bad).is_err(), "{bad} passed");
    }
    let wrong_box = ok.replace("viewBox=\"0 0 16 16\"", "viewBox=\"0 0 14 14\"");
    assert!(gtk_symbolic(&wrong_box).is_err());
}

#[test]
fn path_data_round_trips() {
    let d = "M1.4697 1.2031q-0.2803 0.0581-0.5332 0.3076zm10.7803 2.0029l0 0.8682-10.5 0";
    assert_eq!(path_data(&tokenize(d)), d);
    assert_eq!(
        tokenize("M.5-1.5.5e1z"),
        [
            Token::Command('M'),
            Token::Number(0.5),
            Token::Number(-1.5),
            Token::Number(5.0),
            Token::Command('z')
        ]
    );
    assert_eq!(scaled("M7 0l-3.5 7z", 16.0 / 14.0), "M8 0l-4 8z");
    // Steps of 1/3 land on rounded points: three of them end at 1, not
    // 0.9999.
    assert_eq!(
        scaled("M0 0l1 0 1 0 1 0", 1.0 / 3.0),
        "M0 0l0.3333 0 0.3334 0 0.3333 0"
    );
    assert_eq!(number(-0.00001), "0");
    assert_eq!(number(2.50000), "2.5");
    assert_eq!(
        points("M1 1h2v3l1 1q1 1 2 2z m1 0"),
        [
            (1.0, 1.0),
            (3.0, 1.0),
            (3.0, 4.0),
            (4.0, 5.0),
            (5.0, 6.0),
            (6.0, 7.0),
            (2.0, 1.0)
        ]
    );
}
