//! The illustrations (G§3.14), from the pen file's art (`w7Qg8`) as
//! `design/export/html/artwork.html` draws it: each "Art · …" component is a
//! box of absolutely placed pieces (an `<svg>` per path, its `viewBox`
//! stretched over its box, `preserveAspectRatio="none"`; a `<div>` with
//! `border-radius: 50%` per ellipse, some rotated about their top left),
//! in Swamp Night's resolved colors.
//!
//! **The generator.** This test writes each component as one SVG, every
//! piece moved into the component's px (each point mapped from its
//! `viewBox` to its box; a rotated ellipse as two arcs), in the export's
//! order (its z-order):
//! - `resources/art/app-icon-24.svg` (`uH1BD`, the header's icon): fixed
//!   colors;
//! - `resources/art/quiet-swamp.svg` (`rfUfx`) and `resources/art/signpost.svg`
//!   (`W1Ia5`), whose `muted` and `border` fills are the theme's
//!   (design/export/INDEX.md): `{{muted}}` and `{{border}}`, which
//!   `theme::art` substitutes per theme;
//! - `resources/icons/scalable/actions/qrec-record-symbolic.svg`, the REC
//!   glyph (`w8imU`, all `text-on-record`): a symbolic icon (no fills: GTK
//!   colors it from the CSS, `tests/icons.rs`' markup).
//!
//! `QREC_UPDATE_ART=1` writes the files; the test fails when a committed
//! one differs, and is skipped when `design/` is absent.
//!
//! **The renders.** Each SVG, rendered by rsvg-convert at the pen PNGs'
//! sizes (1× and 2×), matches the PNG
//! within antialiasing: composited over Swamp Night's `bg` and over white,
//! no pixel lies more than [`MAX_EXCESS`] levels outside the range of the
//! PNG's pixels around it (3×3), and at most [`MAX_OFF_SHARE`] of them more
//! than 8.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use qrec_shell::testkit::run_tool;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo() -> PathBuf {
    crate_dir().join("../..")
}

fn export_dir() -> Option<PathBuf> {
    let dir = repo().join("design/export");
    dir.join("html/artwork.html").is_file().then_some(dir)
}

// ------------------------------------------------------------- the HTML

/// An element of the export: its tag, attributes and children.
#[derive(Debug, Default)]
struct Element {
    tag: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Element>,
}

impl Element {
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).map(String::as_str)
    }

    /// The `style` declarations.
    fn style(&self) -> BTreeMap<String, String> {
        self.attr("style")
            .unwrap_or_default()
            .split(';')
            .filter_map(|decl| decl.split_once(':'))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect()
    }

    /// The first element (depth first) named `name` (`data-pencil-name`).
    fn find(&self, name: &str) -> Option<&Element> {
        if self.attr("data-pencil-name") == Some(name) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(name))
    }
}

/// The export's markup as a tree: start and end tags with `name="value"`
/// or `name='value'` attributes; text and comments skipped.
fn parse_html(text: &str) -> Element {
    let mut stack = vec![Element::default()];
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(lt) = text[i..].find('<') {
        i += lt;
        if text[i..].starts_with("<!--") {
            i += text[i..].find("-->").expect("a closed comment") + 3;
            continue;
        }
        if text[i..].starts_with("</") {
            let end = i + text[i..].find('>').expect("a closed tag");
            let tag = text[i + 2..end].trim();
            let element = stack.pop().expect("an open element");
            assert_eq!(element.tag, tag, "an end tag closes what is open");
            stack.last_mut().expect("a parent").children.push(element);
            i = end + 1;
            continue;
        }
        // A start tag: its name, then attributes up to `>` or `/>`.
        let mut j = i + 1;
        while j < bytes.len()
            && !bytes[j].is_ascii_whitespace()
            && bytes[j] != b'>'
            && bytes[j] != b'/'
        {
            j += 1;
        }
        let mut element = Element {
            tag: text[i + 1..j].to_owned(),
            ..Element::default()
        };
        loop {
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            match bytes.get(j) {
                Some(b'>') => {
                    j += 1;
                    stack.push(element);
                    break;
                }
                Some(b'/') => {
                    assert_eq!(bytes.get(j + 1), Some(&b'>'), "a self-closing tag");
                    j += 2;
                    stack.last_mut().expect("a parent").children.push(element);
                    break;
                }
                Some(_) => {
                    let eq = j + text[j..].find('=').expect("name=value");
                    let key = text[j..eq].trim().to_owned();
                    let quote = bytes[eq + 1];
                    assert!(matches!(quote, b'"' | b'\''), "a quoted value for {key}");
                    let close =
                        eq + 2 + text[eq + 2..].find(quote as char).expect("a closed value");
                    let value = text[eq + 2..close]
                        .replace("&quot;", "\"")
                        .replace("&amp;", "&");
                    element.attrs.insert(key, value);
                    j = close + 1;
                }
                None => panic!("an unclosed tag"),
            }
        }
        i = j;
    }
    assert_eq!(stack.len(), 1, "every element closed");
    stack.pop().unwrap()
}

// ------------------------------------------------------------ path data

/// Numbers with at most 4 decimals, no trailing zeros (`-0` is `0`).
fn number(v: f64) -> String {
    let text = format!("{v:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match text {
        "-0" | "" => "0".to_owned(),
        t => t.to_owned(),
    }
}

fn round4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

/// A path's tokens: commands and numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Command(char),
    Number(f64),
}

fn tokenize(d: &str) -> Vec<Token> {
    let bytes = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            out.push(Token::Command(c));
            i += 1;
        } else if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            let mut dot = c == '.';
            while i < bytes.len() {
                let b = bytes[i] as char;
                if b.is_ascii_digit() {
                    i += 1;
                } else if b == '.' && !dot {
                    dot = true;
                    i += 1;
                } else if matches!(b, 'e' | 'E') {
                    i += 1;
                    if matches!(bytes.get(i), Some(b'-' | b'+')) {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            out.push(Token::Number(
                d[start..i]
                    .parse()
                    .unwrap_or_else(|e| panic!("{:?} in a path: {e}", &d[start..i])),
            ));
        } else {
            i += 1;
        }
    }
    out
}

/// A piece's path moved into the component's px: absolute points `p` map
/// to `origin + (p − view) × scale`, relative steps to `step × scale`
/// (the export's paths use `M`, `c`, `l` and `z` only). Each point is
/// rounded where it lands; a relative step is the difference of rounded
/// points, so rounding doesn't add up along the path.
fn move_path(d: &str, origin: (f64, f64), view: (f64, f64), scale: (f64, f64)) -> String {
    let tokens = tokenize(d);
    let map_abs = |x: f64, y: f64| {
        (
            origin.0 + (x - view.0) * scale.0,
            origin.1 + (y - view.1) * scale.1,
        )
    };
    let mut out = String::new();
    let (mut exact, mut written) = ((0.0, 0.0), (0.0, 0.0));
    let (mut exact_start, mut written_start) = ((0.0, 0.0), (0.0, 0.0));
    let mut cmd = 'M';
    let mut i = 0;
    let mut after_number = false;
    let push = |out: &mut String, v: f64, after_number: &mut bool| {
        let n = number(v);
        if *after_number && !n.starts_with('-') {
            out.push(' ');
        }
        out.push_str(&n);
        *after_number = true;
    };
    while i < tokens.len() {
        match tokens[i] {
            Token::Command(c) => {
                assert!("MmLlCcZz".contains(c), "{c:?} in an art path: {d}");
                out.push(c);
                after_number = false;
                cmd = c;
                i += 1;
                if matches!(c, 'Z' | 'z') {
                    (exact, written) = (exact_start, written_start);
                }
                continue;
            }
            Token::Number(_) => {}
        }
        let pairs = if matches!(cmd, 'C' | 'c') { 3 } else { 1 };
        let base = exact;
        let mut end = (exact, written);
        for _ in 0..pairs {
            let value = |t: Token| match t {
                Token::Number(v) => v,
                Token::Command(c) => panic!("{c:?} where a number goes in {d}"),
            };
            let (x, y) = (value(tokens[i]), value(tokens[i + 1]));
            i += 2;
            let point = if cmd.is_ascii_lowercase() {
                (base.0 + x * scale.0, base.1 + y * scale.1)
            } else {
                map_abs(x, y)
            };
            let landed = (round4(point.0), round4(point.1));
            let shown = if cmd.is_ascii_lowercase() {
                (landed.0 - written.0, landed.1 - written.1)
            } else {
                landed
            };
            push(&mut out, shown.0, &mut after_number);
            push(&mut out, shown.1, &mut after_number);
            end = (point, landed);
        }
        (exact, written) = end;
        if matches!(cmd, 'M' | 'm') {
            (exact_start, written_start) = (exact, written);
            cmd = if cmd == 'M' { 'L' } else { 'l' };
        }
    }
    out
}

// ------------------------------------------------------------- the art

/// How a component's colors are written.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Colors {
    /// As the pen resolves them.
    Fixed,
    /// `muted` and `border` (Swamp Night's) as `{{muted}}` and `{{border}}`.
    Themed,
    /// None: a symbolic icon, colored by GTK.
    Symbolic,
}

/// Swamp Night's `muted` and `border`, the art's token fills.
const THEMED: [(&str, &str); 2] = [("#9BA582", "{{muted}}"), ("#3A4A2C", "{{border}}")];

fn px(styles: &BTreeMap<String, String>, key: &str) -> f64 {
    let value = styles
        .get(key)
        .unwrap_or_else(|| panic!("no {key} in {styles:?}"));
    value
        .strip_suffix("px")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{key}: {value} isn't px"))
}

/// `#RRGGBB` and its alpha (0–1) from `#rrggbb` or `#rrggbbaa`.
fn color(hex: &str) -> (String, f64) {
    let h = hex.trim().trim_start_matches('#');
    assert!(matches!(h.len(), 6 | 8), "a color: {hex}");
    let alpha = if h.len() == 8 {
        f64::from(u8::from_str_radix(&h[6..8], 16).expect("hex")) / 255.0
    } else {
        1.0
    };
    (format!("#{}", h[..6].to_ascii_uppercase()), alpha)
}

/// A shape's paint attributes.
fn paint(fill: &str, alpha: f64, colors: Colors) -> String {
    match colors {
        Colors::Symbolic => String::new(),
        Colors::Fixed | Colors::Themed => {
            let fill = THEMED
                .iter()
                .find(|(hex, _)| colors == Colors::Themed && *hex == fill)
                .map_or(fill, |(_, token)| token);
            let mut out = format!(" fill=\"{fill}\"");
            if alpha < 1.0 {
                write!(out, " fill-opacity=\"{}\"", number(alpha)).unwrap();
            }
            out
        }
    }
}

/// The component named `Art · <name>` as one SVG's shapes (each a line),
/// and its size.
fn shapes(art: &Element, colors: Colors) -> (Vec<String>, f64, f64) {
    let styles = art.style();
    let (w, h) = (px(&styles, "width"), px(&styles, "height"));
    let mut out = Vec::new();
    for piece in &art.children {
        let s = piece.style();
        let (left, top) = (px(&s, "left"), px(&s, "top"));
        let (pw, ph) = (px(&s, "width"), px(&s, "height"));
        let opacity: f64 = s
            .get("opacity")
            .map_or(1.0, |o| o.parse().expect("opacity"));
        match piece.tag.as_str() {
            "svg" => {
                assert_eq!(piece.attr("preserveAspectRatio"), Some("none"));
                assert!(!s.contains_key("transform"), "a transformed svg piece");
                let view: Vec<f64> = piece
                    .attr("viewBox")
                    .expect("a viewBox")
                    .split_whitespace()
                    .map(|v| v.parse().expect("a viewBox number"))
                    .collect();
                let scale = (pw / view[2], ph / view[3]);
                for path in &piece.children {
                    assert_eq!(path.tag, "path");
                    let d = move_path(
                        path.attr("d").expect("path data"),
                        (left, top),
                        (view[0], view[1]),
                        scale,
                    );
                    match path.attr("fill") {
                        Some(fill) if fill != "none" => {
                            let (fill, alpha) = color(fill);
                            out.push(format!(
                                "<path d=\"{d}\"{}/>",
                                paint(&fill, alpha * opacity, colors)
                            ));
                        }
                        _ => {
                            // A stroke: pen draws its 1 px `non-scaling-stroke`
                            // inside the open path's region, so half of it
                            // shows (the PNGs' ripples are half a px wide).
                            assert_eq!(path.attr("vector-effect"), Some("non-scaling-stroke"));
                            let (stroke, alpha) = color(path.attr("stroke").expect("a stroke"));
                            let width: f64 = path
                                .attr("stroke-width")
                                .unwrap_or("1")
                                .parse()
                                .expect("a stroke width");
                            let stroke = THEMED
                                .iter()
                                .find(|(hex, _)| colors == Colors::Themed && *hex == stroke)
                                .map_or(stroke.as_str(), |(_, token)| token)
                                .to_owned();
                            let mut line = format!(
                                "<path d=\"{d}\" fill=\"none\" stroke=\"{stroke}\" stroke-width=\"{}\" \
                                 stroke-linecap=\"{}\"",
                                number(width / 2.0),
                                path.attr("stroke-linecap").unwrap_or("butt")
                            );
                            if alpha * opacity < 1.0 {
                                write!(line, " stroke-opacity=\"{}\"", number(alpha * opacity))
                                    .unwrap();
                            }
                            line.push_str("/>");
                            out.push(line);
                        }
                    }
                }
            }
            "div" => {
                assert_eq!(s.get("border-radius").map(String::as_str), Some("50%"));
                let (fill, alpha) = color(s.get("background-color").expect("a fill"));
                let paint = paint(&fill, alpha * opacity, colors);
                let (rx, ry) = (pw / 2.0, ph / 2.0);
                match s.get("transform") {
                    None => {
                        let (cx, cy) = (left + rx, top + ry);
                        if (rx - ry).abs() < 1e-9 {
                            out.push(format!(
                                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\"{paint}/>",
                                number(cx),
                                number(cy),
                                number(rx)
                            ));
                        } else {
                            out.push(format!(
                                "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\"{paint}/>",
                                number(cx),
                                number(cy),
                                number(rx),
                                number(ry)
                            ));
                        }
                    }
                    Some(transform) => {
                        // rotate(θ) about the box's top left: the center
                        // turns with it; the ellipse is two arcs.
                        assert_eq!(
                            s.get("transform-origin").map(String::as_str),
                            Some("top left")
                        );
                        let degrees: f64 = transform
                            .strip_prefix("rotate(")
                            .and_then(|t| t.strip_suffix("deg)"))
                            .and_then(|t| t.parse().ok())
                            .unwrap_or_else(|| panic!("a rotation: {transform}"));
                        let t = degrees.to_radians();
                        let (c, sn) = (t.cos(), t.sin());
                        let (cx, cy) = (left + rx * c - ry * sn, top + rx * sn + ry * c);
                        let (x0, y0) = (cx + rx * c, cy + rx * sn);
                        let (x1, y1) = (cx - rx * c, cy - rx * sn);
                        let arc = |x: f64, y: f64| {
                            format!(
                                "A{} {} {} 1 1 {} {}",
                                number(rx),
                                number(ry),
                                number(degrees),
                                number(x),
                                number(y)
                            )
                        };
                        out.push(format!(
                            "<path d=\"M{} {}{}{}Z\"{paint}/>",
                            number(x0),
                            number(y0),
                            arc(x1, y1),
                            arc(x0, y0)
                        ));
                    }
                }
            }
            other => panic!("a {other} piece"),
        }
    }
    (out, w, h)
}

const GENERATED: &str = "generated by crates/qrec-gui/tests/art.rs (QREC_UPDATE_ART=1 writes \
it) from design/export/html/artwork.html";

/// One generated SVG file.
fn svg_file(comment: &str, shapes: &[String], w: f64, h: f64) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    writeln!(out, "<!-- {comment}: {GENERATED}. -->").unwrap();
    writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">",
        w = number(w),
        h = number(h)
    )
    .unwrap();
    for shape in shapes {
        writeln!(out, "  {shape}").unwrap();
    }
    out.push_str("</svg>\n");
    out
}

/// What the generator writes: a component, the file, its colors and its
/// pen PNGs (`(file, size)` in design/export/art/).
struct Output {
    component: &'static str,
    file: &'static str,
    colors: Colors,
    comment: &'static str,
    pngs: &'static [(&'static str, u32)],
}

const OUTPUTS: [Output; 4] = [
    Output {
        component: "Art · App icon 24",
        file: "crates/qrec-gui/resources/art/app-icon-24.svg",
        colors: Colors::Fixed,
        comment: "The 24 px app icon (pen uH1BD), the header's",
        pngs: &[("app-icon-24@1x.png", 24), ("app-icon-24@2x.png", 48)],
    },
    Output {
        component: "Art · Quiet swamp",
        file: "crates/qrec-gui/resources/art/quiet-swamp.svg",
        colors: Colors::Themed,
        comment: "The quiet swamp (pen rfUfx), the empty timeline's; its muted and border \
                  fills are the theme's ({{muted}}, {{border}})",
        pngs: &[("quiet-swamp@1x.png", 200), ("quiet-swamp@2x.png", 400)],
    },
    Output {
        component: "Art · Signpost",
        file: "crates/qrec-gui/resources/art/signpost.svg",
        colors: Colors::Themed,
        comment: "The signpost (pen W1Ia5), the blocking pages'; its muted and border fills \
                  are the theme's ({{muted}}, {{border}})",
        pngs: &[("signpost@1x.png", 160), ("signpost@2x.png", 320)],
    },
    Output {
        component: "Art · REC glyph",
        file: "crates/qrec-gui/resources/icons/scalable/actions/qrec-record-symbolic.svg",
        colors: Colors::Symbolic,
        comment: "The REC glyph (pen w8imU): the REC badge, the idle Record button and the \
                  app's symbolic icon, a symbolic icon GTK colors (text-on-record)",
        pngs: &[("rec-glyph@1x.png", 16), ("rec-glyph@2x.png", 32)],
    },
];

/// Every generated file's content, by its path under the repo.
fn generated(export: &Path) -> BTreeMap<String, Vec<u8>> {
    let html = fs::read_to_string(export.join("html/artwork.html")).expect("artwork.html");
    let tree = parse_html(&html);
    let mut out = BTreeMap::new();
    for output in &OUTPUTS {
        let art = tree
            .find(output.component)
            .unwrap_or_else(|| panic!("no {:?} in artwork.html", output.component));
        let (shapes, w, h) = shapes(art, output.colors);
        out.insert(
            output.file.to_owned(),
            svg_file(output.comment, &shapes, w, h).into_bytes(),
        );
    }
    out
}

fn updating() -> bool {
    std::env::var("QREC_UPDATE_ART").is_ok_and(|v| v == "1")
}

#[test]
fn the_art_is_the_pens() {
    let Some(export) = export_dir() else {
        eprintln!("design/ is absent: the art isn't checked against the export");
        return;
    };
    let mut stale = Vec::new();
    for (file, content) in generated(&export) {
        let path = repo().join(&file);
        if fs::read(&path).ok().as_deref() == Some(content.as_slice()) {
            continue;
        }
        if updating() {
            fs::create_dir_all(path.parent().unwrap()).expect("create the folder");
            fs::write(&path, &content).unwrap_or_else(|e| panic!("write {file}: {e}"));
            eprintln!("wrote {file}");
        } else {
            stale.push(file);
        }
    }
    assert!(
        stale.is_empty(),
        "art out of date (QREC_UPDATE_ART=1 cargo test -p qrec-gui --test art rewrites it): \
         {stale:#?}"
    );
}

// ------------------------------------------------------------ renders

/// How far outside antialiasing a pixel may be, in levels (0–255), and the
/// share of pixels allowed more than 8 levels off.
const MAX_EXCESS: f64 = 32.0;
const MAX_OFF_SHARE: f64 = 0.01;

/// A PNG's (or an SVG's, rendered by rsvg-convert at `size` wide) RGBA
/// pixels, decoded by ffmpeg.
fn rgba(file: &Path, width: u32, height: u32) -> Vec<u8> {
    let out = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(file)
            .args(["-pix_fmt", "rgba", "-f", "rawvideo", "-"]),
        Duration::from_secs(60),
    );
    assert_eq!(
        out.stdout.len(),
        (width * height * 4) as usize,
        "{width}×{height} pixels of {}: {}",
        file.display(),
        out.stderr
    );
    out.stdout
}

fn render(svg: &Path, width: u32, height: u32, png: &Path) {
    let out = run_tool(
        Command::new("rsvg-convert")
            .arg("--width")
            .arg(width.to_string())
            .arg("--height")
            .arg(height.to_string())
            .arg("--output")
            .arg(png)
            .arg(svg),
        Duration::from_secs(60),
    );
    assert!(
        out.status.success(),
        "rsvg-convert {}: {}",
        svg.display(),
        out.stderr
    );
}

/// `px` (RGBA) over the opaque `ground`.
fn over(px: &[u8], ground: [f64; 3]) -> [f64; 3] {
    let a = f64::from(px[3]) / 255.0;
    [0, 1, 2].map(|k| f64::from(px[k]) * a + ground[k] * (1.0 - a))
}

/// The worst excess over antialiasing and the share of pixels more than 8
/// levels off: each pixel of `ours` against the range of `pen`'s 3×3 window
/// around it, both over `ground`.
fn excess(ours: &[u8], pen: &[u8], width: u32, height: u32, ground: [f64; 3]) -> (f64, f64) {
    let (w, h) = (width as usize, height as usize);
    let at = |data: &[u8], x: usize, y: usize| over(&data[(y * w + x) * 4..][..4], ground);
    let (mut worst, mut off) = (0.0f64, 0usize);
    for y in 0..h {
        for x in 0..w {
            let ours = at(ours, x, y);
            let (mut lo, mut hi) = ([255.0f64; 3], [0.0f64; 3]);
            for yy in y.saturating_sub(1)..(y + 2).min(h) {
                for xx in x.saturating_sub(1)..(x + 2).min(w) {
                    let p = at(pen, xx, yy);
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
            }
            let d = (0..3)
                .map(|k| (lo[k] - ours[k]).max(ours[k] - hi[k]).max(0.0))
                .fold(0.0, f64::max);
            worst = worst.max(d);
            if d > 8.0 {
                off += 1;
            }
        }
    }
    (worst, off as f64 / (w * h) as f64)
}

/// Swamp Night's `bg`, the art's ground in the window, and white.
const GROUNDS: [[f64; 3]; 2] = [[18.0, 23.0, 15.0], [255.0, 255.0, 255.0]];

/// `svg` with Swamp Night's `muted` and `border` for its placeholders and,
/// a symbolic icon, its shapes in `text-on-record` (as the REC badge draws
/// it), into `dir`.
fn as_drawn(svg: &str, colors: Colors) -> String {
    match colors {
        Colors::Symbolic => svg
            .replace("<path ", "<path fill=\"#12170F\" ")
            .replace("<circle ", "<circle fill=\"#12170F\" "),
        _ => THEMED
            .iter()
            .fold(svg.to_owned(), |s, (hex, token)| s.replace(token, hex)),
    }
}

#[test]
fn the_art_draws_as_the_pen_does() {
    let Some(export) = export_dir() else {
        eprintln!("design/ is absent: the art isn't compared with the pen's PNGs");
        return;
    };
    if updating() {
        return;
    }
    let dir = tempfile::tempdir().expect("a scratch dir");
    let mut report = Vec::new();
    let mut wrong = Vec::new();
    for output in &OUTPUTS {
        let svg = fs::read_to_string(repo().join(output.file))
            .unwrap_or_else(|e| panic!("{}: {e}", output.file));
        let source = dir.path().join("art.svg");
        fs::write(&source, as_drawn(&svg, output.colors)).expect("write the scratch SVG");
        for (png, size) in output.pngs {
            let pen = export.join("art").join(png);
            let (w, h) = png_size(&pen);
            assert_eq!(w, *size, "{png}");
            let ours = dir.path().join(format!("ours-{png}"));
            render(&source, w, h, &ours);
            let (a, b) = (rgba(&ours, w, h), rgba(&pen, w, h));
            for ground in GROUNDS {
                let (worst, share) = excess(&a, &b, w, h, ground);
                report.push(format!(
                    "{png} over {ground:?}: worst {worst:.1}, {:.2} % over 8",
                    share * 100.0
                ));
                if worst > MAX_EXCESS || share > MAX_OFF_SHARE {
                    wrong.push(report.last().unwrap().clone());
                }
            }
        }
    }
    eprintln!("{}", report.join("\n"));
    assert!(
        wrong.is_empty(),
        "the art drawn by rsvg differs from the pen's PNGs past antialiasing: {wrong:#?}"
    );
}

/// A PNG's width and height (its IHDR).
fn png_size(file: &Path) -> (u32, u32) {
    let bytes = fs::read(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    assert!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() > 24,
        "{} isn't a PNG",
        file.display()
    );
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    (be(16), be(20))
}

/// The comparison catches what isn't antialiasing: a piece left out.
#[test]
fn the_comparison_catches_a_missing_piece() {
    let Some(export) = export_dir() else {
        return;
    };
    let svg = fs::read_to_string(repo().join(OUTPUTS[2].file)).expect("the signpost");
    // Drop the post (the signpost's first piece).
    let first = svg.find("<path ").expect("a path");
    let end = first + svg[first..].find("/>").expect("its end") + 2;
    let broken = format!("{}{}", &svg[..first], &svg[end..]);
    let dir = tempfile::tempdir().expect("a scratch dir");
    let source = dir.path().join("broken.svg");
    fs::write(&source, as_drawn(&broken, Colors::Themed)).expect("write");
    let ours = dir.path().join("broken.png");
    render(&source, 160, 160, &ours);
    let pen = export.join("art/signpost@1x.png");
    let (worst, share) = excess(
        &rgba(&ours, 160, 160),
        &rgba(&pen, 160, 160),
        160,
        160,
        GROUNDS[0],
    );
    assert!(
        worst > MAX_EXCESS && share > MAX_OFF_SHARE,
        "{worst} {share}"
    );
}

#[test]
fn the_html_parser_reads_the_exports_markup() {
    let tree = parse_html(
        "<div data-pencil-name=\"Art · X\" style='a: 1px; b: \"q\"'>\n  <svg viewBox=\"0 0 2 2\">\
         <path d=\"M0 0z\" fill=\"#123456\"></path></svg>\n  <div style=\"c: 2px\"></div>\n</div>",
    );
    let art = tree.find("Art · X").expect("the component");
    assert_eq!(art.style()["b"], "\"q\"");
    assert_eq!(art.children.len(), 2);
    assert_eq!(art.children[0].children[0].attr("fill"), Some("#123456"));
    assert_eq!(
        move_path(
            "M1 1c1 0 2 0 2 1l-1 0z",
            (10.0, 20.0),
            (1.0, 1.0),
            (2.0, 3.0)
        ),
        "M10 20c2 0 4 0 4 3l-2 0z"
    );
}
