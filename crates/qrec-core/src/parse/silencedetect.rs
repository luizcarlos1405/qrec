//! ffmpeg `silencedetect` stderr → silence intervals (A§9.2). Owner: autocut.
//!
//! What ffmpeg 8.1 prints for Qrec's argv (`media_argv::silence_argv`),
//! measured on the samples in `tests/fixtures/silencedetect/`:
//!
//! ```text
//!   Duration: 00:00:07.00, start: 0.000000, bitrate: 52 kb/s
//! [Parsed_silencedetect_0 @ 0x77b040004800] silence_start: 2.005333
//! [Parsed_silencedetect_0 @ 0x77b040004800] silence_end: 3.008021 | silence_duration: 1.002687
//! size=N/A time=00:00:07.00 bitrate=N/A speed= 778x elapsed=0:00:00.00
//! ```
//!
//! - Times are seconds since the file's first sample: ffmpeg subtracts the
//!   container's `start` (a file starting at 0.25 s reports a silence at
//!   1.0 s, not 1.25 s). That is the file time autocut expects.
//! - Values have up to six decimals with trailing zeros trimmed (`0`,
//!   `0.512`, `1234.568`); older ffmpeg printed `%.6g` (`1e-05`).
//! - A silence still running at EOF gets its `silence_end` when the filter is
//!   torn down, truncated files included. When it is missing (older ffmpeg, a
//!   cut-off log) the silence ends at the input's duration: the given one, else
//!   the final `time=` report, else `Duration − start` of the input.
//! - Only the default `mono=0` output is understood: per-channel lines
//!   (`channel: 0 | silence_start: …`) are rejected rather than merged wrongly.

use crate::protocol::event::Interval;
use crate::time::NS_PER_SEC;

/// The `silence_start` / `silence_end` pairs in `stderr`, in file time,
/// in the order ffmpeg reported them. A trailing silence without
/// `silence_end` ends at the input's duration (the final `time=` report,
/// else the `Duration:` line). `Err` when the output can't be understood.
pub fn parse_silencedetect(stderr: &str) -> Result<Vec<Interval>, String> {
    parse_silencedetect_until(stderr, None)
}

/// Like [`parse_silencedetect`], but a trailing silence without
/// `silence_end` ends at `duration_ns` (file time) when it is given.
pub fn parse_silencedetect_until(
    stderr: &str,
    duration_ns: Option<i64>,
) -> Result<Vec<Interval>, String> {
    let mut intervals = Vec::new();
    let mut open: Option<(i64, usize)> = None;
    let mut input_duration: Option<i64> = None;
    let mut reported: Option<i64> = None;
    let mut saw_ffmpeg = false;

    for (index, raw) in stderr.split('\n').enumerate() {
        let line_no = index + 1;
        // Stats reports are separated by `\r` when ffmpeg prints them live.
        for line in raw.split('\r') {
            if let Some(message) = silencedetect_message(line) {
                saw_ffmpeg = true;
                match parse_event(message).map_err(|e| format!("line {line_no}: {e}"))? {
                    Some(Event::Start(start)) => {
                        if let Some((_, at)) = open {
                            return Err(format!(
                                "line {line_no}: silence_start while the silence from line {at} has no silence_end"
                            ));
                        }
                        open = Some((start, line_no));
                    }
                    Some(Event::End(end)) => {
                        let Some((start, _)) = open.take() else {
                            return Err(format!(
                                "line {line_no}: silence_end without a silence_start"
                            ));
                        };
                        if end < start {
                            return Err(format!(
                                "line {line_no}: silence_end {} is before its silence_start {}",
                                seconds(end),
                                seconds(start)
                            ));
                        }
                        if end > start {
                            intervals.push(Interval {
                                start_ns: start,
                                end_ns: end,
                            });
                        }
                    }
                    None => {}
                }
            } else if let Some(duration) = input_duration_line(line) {
                saw_ffmpeg = true;
                // Only the first input's (there is exactly one).
                if input_duration.is_none() {
                    input_duration = duration;
                }
            } else if let Some(time) = report_time(line) {
                saw_ffmpeg = true;
                if time.is_some() {
                    reported = time;
                }
            }
        }
    }

    if !saw_ffmpeg {
        return Err(
            "no silencedetect output: ffmpeg printed neither silences nor its input and progress lines"
                .to_owned(),
        );
    }
    if let Some((start, line_no)) = open {
        let end = duration_ns
            .or(reported)
            .or(input_duration)
            .ok_or_else(|| {
                format!(
                    "line {line_no}: the silence starting at {} s never ends and ffmpeg reported no duration",
                    seconds(start)
                )
            })?;
        if end > start {
            intervals.push(Interval {
                start_ns: start,
                end_ns: end,
            });
        }
    }
    Ok(intervals)
}

enum Event {
    Start(i64),
    End(i64),
}

/// The message of a line logged by the silencedetect filter: the text after
/// its `[Parsed_silencedetect_0 @ 0x…]` context (and any `[info]` level tag).
fn silencedetect_message(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    let mut ours = false;
    while let Some(after) = rest.strip_prefix('[') {
        let close = after.find(']')?;
        ours |= after[..close].contains("silencedetect");
        rest = after[close + 1..].trim_start();
    }
    ours.then_some(rest.trim_end())
}

fn parse_event(message: &str) -> Result<Option<Event>, String> {
    if message.starts_with("channel:") {
        return Err("per-channel silencedetect output (mono=1) isn't supported".to_owned());
    }
    if let Some(value) = message.strip_prefix("silence_start:") {
        return parse_seconds(value)
            .map(|ns| Some(Event::Start(ns)))
            .ok_or_else(|| format!("bad silence_start value {:?}", value.trim()));
    }
    if let Some(rest) = message.strip_prefix("silence_end:") {
        let value = rest.split('|').next().unwrap_or_default();
        return parse_seconds(value)
            .map(|ns| Some(Event::End(ns)))
            .ok_or_else(|| format!("bad silence_end value {:?}", value.trim()));
    }
    Ok(None)
}

/// `  Duration: 00:00:07.00, start: 0.000000, bitrate: 52 kb/s` →
/// `Some(Some(duration − start))`; `Duration: N/A` → `Some(None)`.
fn input_duration_line(line: &str) -> Option<Option<i64>> {
    let rest = line.trim_start().strip_prefix("Duration:")?;
    let mut parts = rest.split(',');
    let duration = parse_clock(parts.next().unwrap_or_default());
    let start = parts
        .find_map(|p| p.trim().strip_prefix("start:"))
        .and_then(parse_seconds)
        .unwrap_or(0);
    Some(duration.map(|d| d.saturating_sub(start)))
}

/// The `time=` of a final (or live) stats report such as
/// `size=N/A time=00:00:07.00 bitrate=N/A speed= 778x`: `Some(None)` for `N/A`.
fn report_time(line: &str) -> Option<Option<i64>> {
    let line = line.trim_start();
    if !(line.starts_with("size=") || line.starts_with("frame=")) {
        return None;
    }
    let value = line
        .split_whitespace()
        .find_map(|token| token.strip_prefix("time="))?;
    Some(parse_clock(value))
}

/// `[-]HH:MM:SS[.frac]` → nanoseconds.
fn parse_clock(text: &str) -> Option<i64> {
    let text = text.trim();
    let (negative, text) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let mut parts = text.split(':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || !all_digits(h) || !all_digits(m) {
        return None;
    }
    let secs = parse_seconds(s).filter(|ns| *ns >= 0)?;
    let minutes = h
        .parse::<i64>()
        .ok()?
        .checked_mul(60)?
        .checked_add(m.parse().ok()?)?;
    let total = minutes.checked_mul(60 * NS_PER_SEC)?.checked_add(secs)?;
    Some(if negative { -total } else { total })
}

/// A decimal number of seconds (`2.005333`, `7`, `-0.0065`, `1e-05`) →
/// nanoseconds, exact for up to nine decimals (rounded beyond).
fn parse_seconds(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.contains(['e', 'E']) {
        // `%.6g` from older ffmpeg; it is only six significant digits anyway.
        let value: f64 = text.parse().ok()?;
        let ns = (value * NS_PER_SEC as f64).round();
        return (ns.is_finite() && ns.abs() < i64::MAX as f64).then_some(ns as i64);
    }
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    if (int.is_empty() && frac.is_empty())
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let whole: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let digit = |i: usize| i64::from(frac.as_bytes().get(i).map_or(0, |b| b - b'0'));
    let mut nanos = (0..9).fold(0i64, |acc, i| acc * 10 + digit(i));
    if digit(9) >= 5 {
        nanos += 1;
    }
    let total = whole.checked_mul(NS_PER_SEC)?.checked_add(nanos)?;
    Some(if negative { -total } else { total })
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Nanoseconds as seconds for error messages (`3.008021`).
fn seconds(ns: i64) -> String {
    let sign = if ns < 0 { "-" } else { "" };
    let abs = ns.unsigned_abs();
    let (whole, frac) = (abs / NS_PER_SEC as u64, abs % NS_PER_SEC as u64);
    let frac = format!("{frac:09}");
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{frac}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Real `ffmpeg 8.1.2` stderr for Qrec's argv, run from a project folder
    /// (`ffmpeg -hide_banner -nostdin -nostats -loglevel info -y -i
    /// file:takes/take-000N/<track>.mka -af silencedetect=noise=-35dB:d=0.4
    /// -vn -f null -`).
    macro_rules! sample {
        ($name:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/silencedetect/",
                $name,
                ".stderr"
            ))
        };
    }

    fn iv(start: &str, end: &str) -> Interval {
        Interval {
            start_ns: parse_seconds(start).unwrap(),
            end_ns: parse_seconds(end).unwrap(),
        }
    }

    fn without_lines(text: &str, drop: impl Fn(&str) -> bool) -> String {
        text.split_inclusive('\n').filter(|l| !drop(l)).collect()
    }

    #[test]
    fn speech_with_leading_and_trailing_silence() {
        // Mono FLAC/MKA, 7 s: silence at the start, two in the middle, and
        // one running to EOF (ffmpeg closes it at 7 when the filter ends).
        assert_eq!(
            parse_silencedetect(sample!("speech-trailing")).unwrap(),
            vec![
                iv("0", "0.512"),
                iv("2.005333", "3.008021"),
                iv("3.605333", "5.013333"),
                iv("6.016", "7"),
            ]
        );
    }

    #[test]
    fn stereo_reports_where_every_channel_is_silent() {
        assert_eq!(
            parse_silencedetect(sample!("stereo")).unwrap(),
            vec![iv("0.999979", "1.51"), iv("2.499979", "4")]
        );
    }

    #[test]
    fn entirely_silent_file() {
        assert_eq!(
            parse_silencedetect(sample!("all-silent")).unwrap(),
            vec![iv("0", "3")]
        );
    }

    #[test]
    fn no_silence() {
        assert_eq!(parse_silencedetect(sample!("no-silence")).unwrap(), vec![]);
    }

    #[test]
    fn truncated_file_ends_the_silence_at_the_last_sample() {
        // "File ended prematurely": the header still says 7 s, but ffmpeg
        // closes the open silence where decoding stopped (time=00:00:04.22).
        assert_eq!(
            parse_silencedetect(sample!("truncated")).unwrap(),
            vec![
                iv("0", "0.512"),
                iv("2.005333", "3.008021"),
                iv("3.605333", "4.224"),
            ]
        );
    }

    #[test]
    fn file_without_duration() {
        // MKA written to a pipe: `Duration: N/A`.
        assert_eq!(
            parse_silencedetect(sample!("no-duration")).unwrap(),
            vec![iv("1.199958", "2.51"), iv("3.999958", "5")]
        );
    }

    #[test]
    fn times_are_relative_to_the_file_start() {
        // The container starts at 0.25 s; the silence muted at 1.0–2.0 s of
        // the audio is reported at 1.0, not 1.25.
        assert_eq!(
            parse_silencedetect(sample!("start-offset")).unwrap(),
            vec![iv("0.999958", "2.01")]
        );
    }

    #[test]
    fn long_files_keep_millisecond_precision() {
        assert_eq!(
            parse_silencedetect(sample!("long")).unwrap(),
            vec![Interval {
                start_ns: 1_234_568_000_000,
                end_ns: 1_236_001_000_000
            }]
        );
    }

    #[test]
    fn a_trailing_silence_without_end_ends_at_the_reported_time() {
        // Older ffmpeg never printed the EOF silence_end: drop it from a real log.
        let text = without_lines(sample!("speech-trailing"), |l| {
            l.contains("silence_end: 7 |")
        });
        assert_eq!(
            parse_silencedetect(&text).unwrap().last(),
            Some(&iv("6.016", "7")),
            "time=00:00:07.00"
        );
        // Truncated log: the header's 7 s would be wrong, time= wins.
        let text = without_lines(sample!("truncated"), |l| l.contains("silence_end: 4.224"));
        assert_eq!(
            parse_silencedetect(&text).unwrap().last(),
            Some(&iv("3.605333", "4.22"))
        );
        // A given duration wins over both.
        assert_eq!(
            parse_silencedetect_until(&text, Some(4_224_000_000))
                .unwrap()
                .last(),
            Some(&iv("3.605333", "4.224"))
        );
    }

    #[test]
    fn without_a_report_the_input_duration_minus_start_is_used() {
        let text = without_lines(sample!("start-offset"), |l| {
            l.contains("silence_end") || l.starts_with("size=")
        });
        // Duration 3.25 − start 0.25: the audio is 3 s long.
        assert_eq!(
            parse_silencedetect(&text).unwrap(),
            vec![iv("0.999958", "3")]
        );
    }

    #[test]
    fn an_open_silence_without_any_duration_is_an_error() {
        let text = without_lines(sample!("no-duration"), |l| {
            l.contains("silence_end: 5 |") || l.starts_with("size=")
        });
        let err = parse_silencedetect(&text).unwrap_err();
        assert!(err.contains("never ends"), "{err}");
        assert!(err.contains("3.999958"), "{err}");
    }

    #[test]
    fn line_endings_and_level_tags() {
        let text = "Input #0, matroska,webm, from 'file:takes/take-0001/A1-mic.mka':\r\n\
                    \x20 Duration: 00:00:02.00, start: 0.000000, bitrate: 52 kb/s\r\n\
                    [Parsed_silencedetect_0 @ 0x55] [info] silence_start: 0.5\r\n\
                    [Parsed_silencedetect_0 @ 0x55] [info] silence_end: 1.25 | silence_duration: 0.75\r\n\
                    size=N/A time=00:00:01.00 bitrate=N/A\rsize=N/A time=00:00:02.00 bitrate=N/A speed= 500x\r\n";
        assert_eq!(parse_silencedetect(text).unwrap(), vec![iv("0.5", "1.25")]);
    }

    #[test]
    fn unexpected_sequences_are_errors() {
        let header = "  Duration: 00:00:09.00, start: 0.000000, bitrate: 52 kb/s\n";
        let two_starts = format!(
            "{header}[Parsed_silencedetect_0 @ 0x1] silence_start: 1\n[Parsed_silencedetect_0 @ 0x1] silence_start: 2\n"
        );
        assert!(
            parse_silencedetect(&two_starts)
                .unwrap_err()
                .contains("line 3: silence_start while the silence from line 2")
        );
        let orphan_end = format!(
            "{header}[Parsed_silencedetect_0 @ 0x1] silence_end: 2 | silence_duration: 1\n"
        );
        assert!(
            parse_silencedetect(&orphan_end)
                .unwrap_err()
                .contains("without a silence_start")
        );
        let backwards = format!(
            "{header}[Parsed_silencedetect_0 @ 0x1] silence_start: 3\n[Parsed_silencedetect_0 @ 0x1] silence_end: 2 | silence_duration: -1\n"
        );
        assert!(
            parse_silencedetect(&backwards)
                .unwrap_err()
                .contains("before its silence_start 3")
        );
        let per_channel =
            format!("{header}[Parsed_silencedetect_0 @ 0x1] channel: 0 | silence_start: 1\n");
        assert!(
            parse_silencedetect(&per_channel)
                .unwrap_err()
                .contains("mono=1")
        );
        let bad_value = format!("{header}[Parsed_silencedetect_0 @ 0x1] silence_start: NOPTS\n");
        assert!(
            parse_silencedetect(&bad_value)
                .unwrap_err()
                .contains("bad silence_start value \"NOPTS\"")
        );
        // Other filters' lines that mention silences are not ours.
        let other = format!("{header}[in#0 @ 0x2] silence_start: 1\n");
        assert_eq!(parse_silencedetect(&other).unwrap(), vec![]);
    }

    #[test]
    fn empty_or_foreign_output_is_an_error() {
        assert!(parse_silencedetect("").is_err());
        assert!(parse_silencedetect("file:takes/x.mka: No such file or directory\n").is_err());
    }

    #[test]
    fn zero_length_silences_are_dropped() {
        let text = "[Parsed_silencedetect_0 @ 0x1] silence_start: 1\n\
                    [Parsed_silencedetect_0 @ 0x1] silence_end: 1 | silence_duration: 0\n";
        assert_eq!(parse_silencedetect(text).unwrap(), vec![]);
    }

    #[test]
    fn number_formats() {
        assert_eq!(parse_seconds("0"), Some(0));
        assert_eq!(parse_seconds("7"), Some(7 * NS_PER_SEC));
        assert_eq!(parse_seconds("2.005333"), Some(2_005_333_000));
        assert_eq!(parse_seconds(" 0.512 "), Some(512_000_000));
        assert_eq!(parse_seconds("-0.0065"), Some(-6_500_000));
        assert_eq!(parse_seconds("+1.5"), Some(1_500_000_000));
        assert_eq!(parse_seconds(".5"), Some(500_000_000));
        assert_eq!(
            parse_seconds("0.0000000015"),
            Some(2),
            "rounded at the 10th digit"
        );
        assert_eq!(parse_seconds("1e-05"), Some(10_000));
        assert_eq!(parse_seconds("1.23457e+06"), Some(1_234_570 * NS_PER_SEC));
        for bad in ["", "-", ".", "NOPTS", "nan", "inf", "1.2.3", "1,5", "0x10"] {
            assert_eq!(parse_seconds(bad), None, "{bad:?}");
        }
        assert_eq!(parse_clock("00:20:50.00"), Some(1250 * NS_PER_SEC));
        assert_eq!(
            parse_clock("01:00:00.5"),
            Some(3600 * NS_PER_SEC + NS_PER_SEC / 2)
        );
        assert_eq!(parse_clock("-00:00:00.02"), Some(-20_000_000));
        assert_eq!(parse_clock("N/A"), None);
        assert_eq!(parse_clock("00:00"), None);
        assert_eq!(seconds(3_008_021_000), "3.008021");
        assert_eq!(seconds(-7 * NS_PER_SEC), "-7");
    }
}
