//! Numbers, times, sizes and paths as the GUI writes them (G§4.2). Owner:
//! WP0a.
//!
//! Negative numbers use the true minus `−` (U+2212), sizes are SI (1 MB =
//! 1,000,000 bytes), `×` is U+00D7 and `·` U+00B7. Key caps come from
//! `keymap::KeyChord::cap_legends`.

use crate::ids::{Frame, TakeId};
use crate::model::{Canvas, Clip, Fps};
use crate::timeline::total_frames;

/// The true minus sign.
pub const MINUS: char = '\u{2212}';

/// The take colors in cycle order (DESIGN.md): take 1 is moss, take 9 moss
/// again. [`take_color`] indexes this.
pub const TAKE_COLORS: [&str; 8] = [
    "moss", "donkey", "onion", "mud", "lily", "ginger", "dragon", "fairy",
];

/// `HH:MM:SS:FF` at the nominal rate (`time::timecode`).
pub fn timecode(f: Frame, fps: Fps) -> String {
    crate::time::timecode(f, fps)
}

/// A file size in SI units: "12 KB" below 1 MB (whole kilobytes, at least
/// 1 for a non-empty file), then one decimal: "129.0 MB", "1.2 GB",
/// "3.4 TB".
pub fn size(bytes: u64) -> String {
    const KB: u128 = 1_000;
    const MB: u128 = 1_000_000;
    let b = u128::from(bytes);
    let kb = (b + KB / 2) / KB;
    if kb < 1000 {
        let kb = if bytes > 0 { kb.max(1) } else { 0 };
        return format!("{kb} KB");
    }
    let mut unit = MB;
    for name in ["MB", "GB"] {
        let tenths = (b * 10 + unit / 2) / unit;
        if tenths < 10_000 {
            return format!("{}.{} {name}", tenths / 10, tenths % 10);
        }
        unit *= 1000;
    }
    let tenths = (b * 10 + unit / 2) / unit;
    format!("{}.{} TB", tenths / 10, tenths % 10)
}

/// A gain: "0.0 dB", "−6.0 dB", "3.0 dB".
pub fn gain_db(v: f64) -> String {
    format!("{} dB", signed_tenths(v))
}

/// An audio sync offset: "+0 ms", "+80 ms", "−20 ms".
pub fn offset_ms(v: i32) -> String {
    if v < 0 {
        format!("{MINUS}{} ms", v.unsigned_abs())
    } else {
        format!("+{v} ms")
    }
}

/// A count with thousands separators: "3,449".
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A duration in whole seconds as `HH:MM:SS`: "00:00:38" (hours grow past
/// 99 rather than wrapping).
pub fn hms(secs: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

/// A frame rate: "30 fps", "29.97 fps", "23.976 fps" (up to three
/// decimals, trailing zeros dropped).
pub fn fps(f: Fps) -> String {
    let thousandths =
        (u128::from(f.num) * 1000 * 2 + u128::from(f.den.max(1))) / (2 * u128::from(f.den.max(1)));
    let (whole, frac) = (thousandths / 1000, thousandths % 1000);
    if frac == 0 {
        format!("{whole} fps")
    } else {
        let frac = format!("{frac:03}");
        format!("{whole}.{} fps", frac.trim_end_matches('0'))
    }
}

/// The canvas: "1920×1080 · 30 fps".
pub fn canvas(c: &Canvas) -> String {
    format!("{}\u{00d7}{} \u{00b7} {}", c.width, c.height, fps(c.fps))
}

/// The timeline summary in two parts, for a sans and a mono label:
/// `("10 clips · ", "00:04:06:11")`, `("1 clip · ", …)`. `None` on an
/// empty timeline: the GUI writes "No clips yet".
pub fn clips_summary(clips: &[Clip], fps: Fps) -> Option<(String, String)> {
    if clips.is_empty() {
        return None;
    }
    let n = clips.len() as u64;
    let noun = if n == 1 { "clip" } else { "clips" };
    Some((
        format!("{} {noun} \u{00b7} ", count(n)),
        timecode(total_frames(clips), fps),
    ))
}

/// What an empty timeline's summary says.
pub const NO_CLIPS: &str = "No clips yet";

/// The take's color: an index into [`TAKE_COLORS`] (and the `take-1` …
/// `take-8` CSS classes minus one), `(n − 1) mod 8`.
pub fn take_color(take: &TakeId) -> usize {
    (take.number() as usize + 7) % 8
}

/// A meter's peak reading in dBFS: "−12.4", "0.0"; "—" below −60.
pub fn peak(db: f64) -> String {
    if db.is_nan() || db < -60.0 {
        "\u{2014}".to_owned()
    } else {
        signed_tenths(db)
    }
}

/// A path for people: inside the project, relative to it
/// (`exports/talk.mp4`; a relative path already is); under `home`,
/// `~/…`; otherwise as given.
pub fn display_path(path: &str, project_dir: &str, home: Option<&str>) -> String {
    if !path.starts_with('/') {
        return path.to_owned();
    }
    let inside = |dir: &str| -> Option<String> {
        let dir = dir.trim_end_matches('/');
        if dir.is_empty() {
            return None;
        }
        if path == dir {
            return Some(String::new());
        }
        path.strip_prefix(dir)
            .and_then(|rest| rest.strip_prefix('/'))
            .map(str::to_owned)
    };
    if let Some(rel) = inside(project_dir).filter(|rel| !rel.is_empty()) {
        return rel;
    }
    match home.and_then(inside) {
        Some(rel) if rel.is_empty() => "~".to_owned(),
        Some(rel) => format!("~/{rel}"),
        None => path.to_owned(),
    }
}

/// A core message as a title or a sentence: its first letter uppercased
/// ("no clip c9 on the timeline" → "No clip c9 on the timeline").
pub fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `v` with one decimal and the true minus; never "−0.0".
fn signed_tenths(v: f64) -> String {
    let text = format!("{:.1}", v.abs());
    if v < 0.0 && text != "0.0" {
        format!("{MINUS}{text}")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    #[test]
    fn formats_match_the_designs() {
        let state = testkit::design_state();
        let p = &state.project;
        let summary = clips_summary(&p.timeline.clips, p.canvas.fps).unwrap();
        let one = &p.timeline.clips[..1];
        let take = |n: u32| TakeId::numbered(n);
        let lines = [
            ("timecode(2437)", timecode(2437, p.canvas.fps)),
            ("timecode(7391)", timecode(7391, p.canvas.fps)),
            ("size(12_345)", size(12_345)),
            ("size(1)", size(1)),
            ("size(0)", size(0)),
            ("size(999_499)", size(999_499)),
            ("size(999_500)", size(999_500)),
            ("size(129_000_000)", size(129_000_000)),
            ("size(436_400_000)", size(436_400_000)),
            ("size(88_412_345)", size(88_412_345)),
            ("size(999_960_000)", size(999_960_000)),
            ("size(1_234_000_000)", size(1_234_000_000)),
            ("size(5_600_000_000_000)", size(5_600_000_000_000)),
            ("gain_db(0.0)", gain_db(0.0)),
            ("gain_db(-6.0)", gain_db(-6.0)),
            ("gain_db(3.0)", gain_db(3.0)),
            ("gain_db(-0.04)", gain_db(-0.04)),
            ("offset_ms(0)", offset_ms(0)),
            ("offset_ms(80)", offset_ms(80)),
            ("offset_ms(-20)", offset_ms(-20)),
            ("count(3449)", count(3449)),
            ("count(7391)", count(7391)),
            ("count(999)", count(999)),
            ("count(1234567)", count(1_234_567)),
            ("hms(38)", hms(38)),
            ("hms(91)", hms(91)),
            ("hms(3725)", hms(3725)),
            ("fps(30)", fps(Fps::new(30, 1))),
            ("fps(30000/1001)", fps(Fps::new(30000, 1001))),
            ("fps(24000/1001)", fps(Fps::new(24000, 1001))),
            ("fps(25/2)", fps(Fps::new(25, 2))),
            ("canvas", canvas(&p.canvas)),
            ("clips_summary", format!("{}{}", summary.0, summary.1)),
            (
                "clips_summary(1)",
                clips_summary(one, p.canvas.fps)
                    .map(|(a, b)| a + &b)
                    .unwrap(),
            ),
            (
                "clips_summary(none)",
                clips_summary(&[], p.canvas.fps)
                    .map(|(a, b)| a + &b)
                    .unwrap_or_else(|| NO_CLIPS.to_owned()),
            ),
            (
                "take_color(1)",
                TAKE_COLORS[take_color(&take(1))].to_owned(),
            ),
            (
                "take_color(8)",
                TAKE_COLORS[take_color(&take(8))].to_owned(),
            ),
            (
                "take_color(9)",
                TAKE_COLORS[take_color(&take(9))].to_owned(),
            ),
            (
                "take_color(10)",
                TAKE_COLORS[take_color(&take(10))].to_owned(),
            ),
            ("peak(-12.43)", peak(-12.43)),
            ("peak(0.0)", peak(0.0)),
            ("peak(-60.0)", peak(-60.0)),
            ("peak(-60.01)", peak(-60.01)),
            ("peak(NaN)", peak(f64::NAN)),
            (
                "display_path(in project)",
                display_path(
                    "/home/me/videos/rust-traits-talk/exports/talk.mp4",
                    "/home/me/videos/rust-traits-talk",
                    Some("/home/me"),
                ),
            ),
            (
                "display_path(relative)",
                display_path(
                    "exports/rust-traits-talk-20261014-153012.mp4",
                    "/home/me/videos/rust-traits-talk",
                    Some("/home/me"),
                ),
            ),
            (
                "display_path(under home)",
                display_path(
                    "/home/me/Videos/talk-final.mp4",
                    "/home/me/videos/rust-traits-talk/",
                    Some("/home/me/"),
                ),
            ),
            (
                "display_path(home)",
                display_path("/home/me", "/srv/p", Some("/home/me")),
            ),
            (
                "display_path(elsewhere)",
                display_path("/tmp/x.mp4", "/home/me/p", Some("/home/me")),
            ),
            (
                "display_path(prefix only)",
                display_path("/home/meow/x.mp4", "/home/me/p", Some("/home/me")),
            ),
        ];
        let text: String = lines
            .iter()
            .map(|(what, out)| format!("{what:<28} {out}\n"))
            .collect();
        insta::assert_snapshot!("formats", text);
    }

    #[test]
    fn exact_strings() {
        assert_eq!(gain_db(-6.0), "\u{2212}6.0 dB");
        assert_eq!(offset_ms(-20), "\u{2212}20 ms");
        assert_eq!(count(3449), "3,449");
        assert_eq!(size(129_000_000), "129.0 MB");
        assert_eq!(hms(38), "00:00:38");
        assert_eq!(fps(Fps::new(30000, 1001)), "29.97 fps");
        let canvas_text = canvas(&testkit::design_project().canvas);
        assert_eq!(canvas_text, "1920\u{00d7}1080 \u{00b7} 30 fps");
        assert_eq!(peak(-61.0), "\u{2014}");
    }
}
