//! Numbers, times, sizes and paths as the GUI writes them (G§4.2). Owner:
//! WP0a.
//!
//! Negative numbers use the true minus `−` (U+2212), sizes are SI (1 MB =
//! 1,000,000 bytes), `×` is U+00D7 and `·` U+00B7. Key caps come from
//! `keymap::KeyChord::cap_legends`.

use chrono::{DateTime, Datelike, FixedOffset, Utc};

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

/// `at` on the wall clock `utc_offset_secs` east of UTC (an offset chrono
/// can't take counts as UTC).
fn local(at: DateTime<Utc>, utc_offset_secs: i32) -> DateTime<FixedOffset> {
    let offset = FixedOffset::east_opt(utc_offset_secs)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC is a valid offset"));
    at.with_timezone(&offset)
}

/// A local wall-clock time: "14:12". The offset is the one in force at
/// `at` (the GUI asks its zone for each instant: a take recorded before a
/// DST change has another offset than now).
pub fn local_hm(at: DateTime<Utc>, utc_offset_secs: i32) -> String {
    local(at, utc_offset_secs).format("%H:%M").to_string()
}

/// When something happened, inside a sentence: "today at 15:02", "Oct 14
/// at 15:02", "Oct 14, 2025 at 15:02" (another year). Both times are on
/// the local clock of their own instant (each carries the offset in force
/// then); "today" is `now`'s date.
pub fn local_when(at: DateTime<FixedOffset>, now: DateTime<FixedOffset>) -> String {
    let today = now.date_naive();
    let time = at.format("%H:%M");
    if at.date_naive() == today {
        format!("today at {time}")
    } else if at.year() == today.year() {
        format!("{} at {time}", at.format("%b %-d"))
    } else {
        format!("{} at {time}", at.format("%b %-d, %Y"))
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

    /// Local times take the offset in force at their own instant: take 4
    /// recorded at 14:12 EDT reads 14:12 after the clocks went back too.
    #[test]
    fn local_times() {
        let utc = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let local = |s: &str| DateTime::parse_from_rfc3339(s).unwrap();
        let take_4 = utc("2026-10-14T18:12:00Z");
        assert_eq!(local_hm(take_4, -4 * 3600), "14:12");
        assert_eq!(local_hm(take_4, -5 * 3600), "13:12");
        assert_eq!(local_hm(take_4, 0), "18:12");
        assert_eq!(local_hm(take_4, 1 << 30), "18:12", "no such offset: UTC");

        let now = local("2026-11-02T09:00:00-05:00");
        let when = |at: &str| local_when(local(at), now);
        assert_eq!(when("2026-11-02T00:00:00-05:00"), "today at 00:00");
        assert_eq!(when("2026-11-01T23:59:00-05:00"), "Nov 1 at 23:59");
        assert_eq!(when("2026-10-14T15:02:00-04:00"), "Oct 14 at 15:02");
        assert_eq!(when("2025-10-14T15:02:00-04:00"), "Oct 14, 2025 at 15:02");
        // Another offset at the file's instant: its own wall clock.
        assert_eq!(when("2026-11-02T08:30:00+00:00"), "today at 08:30");
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
