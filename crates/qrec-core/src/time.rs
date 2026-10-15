//! Frames, nanoseconds, timecodes and CLI time arguments (SPEC §14).
//!
//! All conversions use integer math with 128-bit intermediates, so they are
//! exact and identical everywhere (export, preview, autocut, CLI).

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::CoreError;
use crate::ids::Frame;
use crate::model::Fps;

pub const NS_PER_SEC: i64 = 1_000_000_000;
pub const NS_PER_MS: i64 = 1_000_000;

fn div_floor(a: i128, b: i128) -> i128 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) { q - 1 } else { q }
}

fn div_round(a: i128, b: i128) -> i128 {
    // Rounds half up (towards +inf) for positive b: floor((2a + b) / 2b).
    div_floor(2 * a + b, 2 * b)
}

/// The start of frame `f` in nanoseconds: `floor(f · 10⁹ · den / num)`.
///
/// This is exactly GStreamer's `gst_util_uint64_scale(f, den·GST_SECOND, num)`
/// (truncation). Rounding instead makes GES pick the previous clip's frame
/// at about one cut in three (measured with ges-launch 1.26). Durations of
/// ranges are always `frame_ns(end) − frame_ns(start)`; never sum ns durations.
pub fn frame_ns(f: i64, fps: Fps) -> i64 {
    div_floor(f as i128 * NS_PER_SEC as i128 * fps.den as i128, fps.num as i128) as i64
}

/// The duration of `frames` frames starting at frame 0, in nanoseconds.
pub fn frames_to_ns(frames: Frame, fps: Fps) -> i64 {
    frame_ns(frames as i64, fps)
}

/// The frame containing instant `ns`: the `f` with `frame_ns(f) ≤ ns < frame_ns(f + 1)`.
pub fn ns_to_frame_floor(ns: i64, fps: Fps) -> i64 {
    let unit = NS_PER_SEC as i128 * fps.den as i128;
    div_floor((ns as i128 + 1) * fps.num as i128 - 1, unit) as i64
}

/// The first frame starting at or after `ns`: the smallest `f` with `frame_ns(f) ≥ ns`.
pub fn ns_to_frame_ceil(ns: i64, fps: Fps) -> i64 {
    let unit = NS_PER_SEC as i128 * fps.den as i128;
    div_floor(ns as i128 * fps.num as i128 + unit - 1, unit) as i64
}

/// The frame whose start is nearest to `ns`. The frame starts are compared
/// in 128 bits: near `i64::MAX` the next frame starts past the `i64` range.
pub fn ns_to_frame_round(ns: i64, fps: Fps) -> i64 {
    let f = ns_to_frame_floor(ns, fps);
    let start_of = |f: i64| div_floor(f as i128 * NS_PER_SEC as i128 * fps.den as i128, fps.num as i128);
    let (start, next) = (start_of(f), start_of(f + 1));
    if (ns as i128 - start) * 2 >= next - start { f + 1 } else { f }
}

/// `HH:MM:SS:FF` at the nominal rate (non-drop-frame).
pub fn timecode(f: Frame, fps: Fps) -> String {
    let (h, m, s, ff) = split_timecode(f, fps);
    format!("{h:02}:{m:02}:{s:02}:{ff:02}")
}

/// `HHMMSSFF`, for file names (SPEC §13.2).
pub fn timecode_compact(f: Frame, fps: Fps) -> String {
    let (h, m, s, ff) = split_timecode(f, fps);
    format!("{h:02}{m:02}{s:02}{ff:02}")
}

fn split_timecode(f: Frame, fps: Fps) -> (u64, u64, u64, u64) {
    let nominal = u64::from(fps.nominal().max(1));
    let ff = f % nominal;
    let secs = f / nominal;
    (secs / 3600, (secs / 60) % 60, secs % 60, ff)
}

/// A human duration such as `6.2 s` or `1 min 4.0 s` (labels and summaries).
pub fn duration_label(ns: i64) -> String {
    let tenths = div_round(ns as i128, (NS_PER_SEC / 10) as i128) as i64;
    let (mins, tenths) = (tenths / 600, tenths % 600);
    if mins > 0 {
        format!("{mins} min {}.{} s", tenths / 10, tenths % 10)
    } else {
        format!("{}.{} s", tenths / 10, tenths % 10)
    }
}

/// Formats nanoseconds as a decimal number of seconds without trailing zeros (`13.7`).
fn decimal_seconds(ns: u64) -> String {
    let (secs, frac) = (ns / NS_PER_SEC as u64, ns % NS_PER_SEC as u64);
    if frac == 0 {
        return secs.to_string();
    }
    let frac = format!("{frac:09}");
    format!("{secs}.{}", frac.trim_end_matches('0'))
}

/// Whether a time argument is absolute or moves something by a delta.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sign {
    Absolute,
    Plus,
    Minus,
}

/// The magnitude of a time argument, kept in the form it was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeValue {
    /// `412f`
    Frames(u64),
    /// `13.7s`, stored exactly in nanoseconds.
    Seconds(u64),
    /// `00:00:13.700`, stored exactly in nanoseconds.
    Clock(u64),
    /// `00:00:13:21`
    Timecode { h: u64, m: u8, s: u8, f: u32 },
}

/// A CLI time argument: `412f`, `13.7s`, `00:00:13:21`, `00:00:13.700`,
/// optionally prefixed with `+`/`-` to make it relative.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimeArg {
    pub sign: Sign,
    pub value: TimeValue,
}

impl TimeArg {
    pub const fn frames(n: u64) -> Self {
        Self { sign: Sign::Absolute, value: TimeValue::Frames(n) }
    }

    pub const fn plus_frames(n: u64) -> Self {
        Self { sign: Sign::Plus, value: TimeValue::Frames(n) }
    }

    pub const fn minus_frames(n: u64) -> Self {
        Self { sign: Sign::Minus, value: TimeValue::Frames(n) }
    }

    pub fn is_relative(&self) -> bool {
        self.sign != Sign::Absolute
    }

    /// The magnitude in frames (seconds round to the nearest frame).
    /// Fails for a timecode whose frame field is ≥ the nominal rate.
    pub fn magnitude_frames(&self, fps: Fps) -> Result<i64, CoreError> {
        // No wrap into negative i64; callers clamp to take bounds before any
        // nanosecond conversion, so huge-but-representable values are fine.
        let max = i64::MAX as u64;
        let too_big = || CoreError::usage(format!("time {self} is too large"));
        let frames = match self.value {
            TimeValue::Frames(n) => n,
            TimeValue::Seconds(ns) | TimeValue::Clock(ns) => {
                let ns = i64::try_from(ns).map_err(|_| too_big())?;
                ns_to_frame_round(ns, fps) as u64
            }
            TimeValue::Timecode { h, m, s, f } => {
                let nominal = fps.nominal();
                if f >= nominal {
                    return Err(CoreError::usage(format!(
                        "timecode frame {f:02} is out of range at {fps} fps (max {:02})",
                        nominal - 1
                    )));
                }
                h.checked_mul(3600)
                    .and_then(|v| v.checked_add(u64::from(m) * 60 + u64::from(s)))
                    .and_then(|v| v.checked_mul(u64::from(nominal)))
                    .and_then(|v| v.checked_add(u64::from(f)))
                    .ok_or_else(too_big)?
            }
        };
        if frames > max {
            return Err(too_big());
        }
        Ok(frames as i64)
    }

    /// The signed value in frames: the magnitude, negated for `-`.
    pub fn signed_frames(&self, fps: Fps) -> Result<i64, CoreError> {
        let m = self.magnitude_frames(fps)?;
        Ok(if self.sign == Sign::Minus { -m } else { m })
    }

    /// Resolves against `base`: absolute values replace it, relative values move it.
    pub fn resolve(&self, base: i64, fps: Fps) -> Result<i64, CoreError> {
        let v = self.signed_frames(fps)?;
        if !self.is_relative() {
            return Ok(v);
        }
        base.checked_add(v).ok_or_else(|| CoreError::usage(format!("time {self} is too large")))
    }
}

impl fmt::Display for TimeArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.sign {
            Sign::Absolute => {}
            Sign::Plus => f.write_str("+")?,
            Sign::Minus => f.write_str("-")?,
        }
        match self.value {
            TimeValue::Frames(n) => write!(f, "{n}f"),
            TimeValue::Seconds(ns) => write!(f, "{}s", decimal_seconds(ns)),
            TimeValue::Clock(ns) => {
                let secs = ns / NS_PER_SEC as u64;
                let frac = ns % NS_PER_SEC as u64;
                write!(f, "{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)?;
                let frac = format!("{frac:09}");
                let trimmed = frac.trim_end_matches('0');
                write!(f, ".{}", if trimmed.len() < 3 { &frac[..3] } else { trimmed })
            }
            TimeValue::Timecode { h, m, s, f: ff } => write!(f, "{h:02}:{m:02}:{s:02}:{ff:02}"),
        }
    }
}

fn parse_fraction_ns(frac: &str) -> Option<u64> {
    if frac.is_empty() || frac.len() > 9 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let padded = format!("{frac:0<9}");
    padded.parse().ok()
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

impl FromStr for TimeArg {
    type Err = CoreError;

    fn from_str(input: &str) -> Result<Self, CoreError> {
        let usage = || {
            CoreError::usage(format!("invalid time {input:?}"))
                .with_hint("use frames (412f), seconds (13.7s), a timecode (00:00:13:21 or 00:00:13.700), optionally with + or -")
        };
        // Numbers past u64 (and nanoseconds past u64) are `Usage` here,
        // never an overflow (debug builds) or a wrapped time (release).
        let too_large = || CoreError::usage(format!("time {} is too large", input.trim()));
        let s = input.trim();
        let (sign, body) = match s.as_bytes().first() {
            Some(b'+') => (Sign::Plus, &s[1..]),
            Some(b'-') => (Sign::Minus, &s[1..]),
            _ => (Sign::Absolute, s),
        };
        let value = if let Some(n) = body.strip_suffix('f') {
            if !all_digits(n) {
                return Err(usage());
            }
            TimeValue::Frames(n.parse().map_err(|_| too_large())?)
        } else if let Some(n) = body.strip_suffix('s') {
            let (int, frac) = n.split_once('.').unwrap_or((n, ""));
            if !all_digits(int) {
                return Err(usage());
            }
            let frac_ns = if frac.is_empty() && !n.contains('.') { 0 } else { parse_fraction_ns(frac).ok_or_else(usage)? };
            let secs: u64 = int.parse().map_err(|_| too_large())?;
            TimeValue::Seconds(secs.checked_mul(NS_PER_SEC as u64).and_then(|v| v.checked_add(frac_ns)).ok_or_else(too_large)?)
        } else if body.contains(':') {
            let parts: Vec<&str> = body.split(':').collect();
            match parts.as_slice() {
                [h, m, s, f] if all_digits(h) && m.len() == 2 && s.len() == 2 && all_digits(m) && all_digits(s) && all_digits(f) && f.len() <= 3 => {
                    let (m, s): (u8, u8) = (m.parse().map_err(|_| usage())?, s.parse().map_err(|_| usage())?);
                    if m >= 60 || s >= 60 {
                        return Err(usage());
                    }
                    TimeValue::Timecode { h: h.parse().map_err(|_| too_large())?, m, s, f: f.parse().map_err(|_| usage())? }
                }
                [h, m, sf] if all_digits(h) && m.len() == 2 && all_digits(m) => {
                    let (s, frac) = sf.split_once('.').ok_or_else(usage)?;
                    if s.len() != 2 || !all_digits(s) {
                        return Err(usage());
                    }
                    let (h, m, s): (u64, u64, u64) =
                        (h.parse().map_err(|_| too_large())?, m.parse().map_err(|_| usage())?, s.parse().map_err(|_| usage())?);
                    if m >= 60 || s >= 60 {
                        return Err(usage());
                    }
                    let frac_ns = parse_fraction_ns(frac).ok_or_else(usage)?;
                    let ns = h
                        .checked_mul(3600)
                        .and_then(|v| v.checked_add(m * 60 + s))
                        .and_then(|v| v.checked_mul(NS_PER_SEC as u64))
                        .and_then(|v| v.checked_add(frac_ns))
                        .ok_or_else(too_large)?;
                    TimeValue::Clock(ns)
                }
                _ => return Err(usage()),
            }
        } else if all_digits(body) || body.parse::<f64>().is_ok() {
            return Err(CoreError::usage(format!("time {input:?} has no unit")).with_hint("add f (frames) or s (seconds), e.g. 412f or 13.7s"));
        } else {
            return Err(usage());
        };
        Ok(TimeArg { sign, value })
    }
}

impl Serialize for TimeArg {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TimeArg {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(|e: CoreError| serde::de::Error::custom(e.message))
    }
}

impl JsonSchema for TimeArg {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TimeArg".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "412f | 13.7s | 00:00:13:21 | 00:00:13.700, optionally prefixed with + or -",
            "examples": ["412f", "13.7s", "00:00:13:21", "+1f", "-2s"]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const F30: Fps = Fps::new(30, 1);
    const F2997: Fps = Fps::new(30000, 1001);

    fn parse(s: &str) -> TimeArg {
        s.parse().unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn grammar() {
        assert_eq!(parse("412f"), TimeArg::frames(412));
        assert_eq!(parse("+1f"), TimeArg::plus_frames(1));
        assert_eq!(parse("-2s").value, TimeValue::Seconds(2 * NS_PER_SEC as u64));
        assert_eq!(parse("13.7s").value, TimeValue::Seconds(13_700_000_000));
        assert_eq!(parse("00:00:13:21").value, TimeValue::Timecode { h: 0, m: 0, s: 13, f: 21 });
        assert_eq!(parse("00:00:13.700").value, TimeValue::Clock(13_700_000_000));
        for bad in ["", "f", "12", "1.5", "1.5f", "-", "1:2:3", "00:60:00:00", "00:00:13.", "13.s", "abc", "1x", "00:00:13.1234567890"] {
            assert!(bad.parse::<TimeArg>().is_err(), "{bad:?} should fail");
        }
        let err = "412".parse::<TimeArg>().unwrap_err();
        assert!(err.hint.unwrap().contains("412f"));
    }

    #[test]
    fn conversions() {
        assert_eq!(parse("13.7s").magnitude_frames(F30).unwrap(), 411);
        assert_eq!(parse("00:00:13:21").magnitude_frames(F30).unwrap(), 411);
        assert!(parse("00:00:00:30").magnitude_frames(F30).is_err());
        assert_eq!(parse("00:01:00:00").magnitude_frames(F2997).unwrap(), 1800);
        assert_eq!(parse("+1f").resolve(10, F30).unwrap(), 11);
        assert_eq!(parse("-2s").resolve(100, F30).unwrap(), 40);
        assert_eq!(parse("5f").resolve(100, F30).unwrap(), 5);
        assert_eq!(frame_ns(1, F30), 33_333_333);
        assert_eq!(frame_ns(2, F30), 66_666_666);
        assert_eq!(frame_ns(3, F30), 100_000_000);
        assert_eq!(frame_ns(1, F2997), 33_366_666);
        assert_eq!(ns_to_frame_floor(33_333_332, F30), 0);
        assert_eq!(ns_to_frame_floor(33_333_333, F30), 1);
        assert_eq!(ns_to_frame_ceil(33_333_333, F30), 1);
        assert_eq!(ns_to_frame_ceil(33_333_334, F30), 2);
        assert_eq!(ns_to_frame_ceil(1, F30), 1);
        assert_eq!(ns_to_frame_floor(-1, F30), -1);
        assert_eq!(ns_to_frame_round(16_666_666, F30), 0);
        assert_eq!(ns_to_frame_round(16_666_667, F30), 1);
        assert_eq!(timecode(411, F30), "00:00:13:21");
        assert_eq!(timecode_compact(411, F30), "00001321");
        assert_eq!(timecode(30 * 3600, F30), "01:00:00:00");
        assert_eq!(duration_label(6_249_000_000), "6.2 s");
        assert_eq!(duration_label(64_000_000_000), "1 min 4.0 s");
    }

    #[test]
    fn huge_values_are_usage_errors() {
        // Large but representable values are fine (callers clamp them).
        assert!(parse("999999:00:00:00").magnitude_frames(F30).is_ok());
        assert!(parse("2562047:00:00.000").magnitude_frames(F30).is_ok());
        for big in [
            "99999999999999999:00:00:00",
            "18446744073709551615f",
            "18446744073709551s",
            "99999999999999999999f",
            "99999999999999999999s",
            // Fits u64 nanoseconds, not i64: rejected while converting.
            "5124095:00:00.000",
            // HH:MM:SS.fff past u64 nanoseconds: these overflowed (a panic
            // in debug builds; release builds wrapped 5124096 h to 25 min).
            "5124096:00:00.000",
            "5124095:34:33.999",
            "-5124096:00:00.000",
            "99999999999999999999:00:00.000",
            // Past i64 nanoseconds: representable, too large to convert.
            "9223372036.854775808s",
        ] {
            // Rejected either while parsing or while converting, never a panic.
            let err = big
                .parse::<TimeArg>()
                .and_then(|t| t.magnitude_frames(F30))
                .unwrap_err();
            assert_eq!(err.code, crate::error::ErrorCode::Usage, "{big}");
            assert!(err.message.contains("too large"), "{big}: {}", err.message);
        }
        // The last nanosecond of the i64 range converts (the frame after it
        // starts past i64, which rounding compares in 128 bits).
        let last = i64::MAX as u64;
        assert_eq!(parse("9223372036.854775807s").value, TimeValue::Seconds(last));
        assert_eq!(parse("2562047:47:16.854775807").value, TimeValue::Clock(last));
        for edge in ["9223372036.854775807s", "2562047:47:16.854775807"] {
            for fps in [F30, F2997, Fps::new(240, 1), Fps::new(1, 1)] {
                let frames = parse(edge).magnitude_frames(fps).unwrap();
                assert_eq!(frames, ns_to_frame_round(i64::MAX, fps), "{edge} at {fps}");
            }
        }
        assert_eq!(parse("+9223372036854775807f").resolve(0, F30).unwrap(), i64::MAX);
        let err = parse("+9223372036854775807f").resolve(1, F30).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::Usage);
    }

    #[test]
    fn display_round_trips() {
        for s in ["412f", "+1f", "-2s", "13.7s", "00:00:13:21", "00:00:13.700", "-00:00:01:00", "0.000000001s"] {
            assert_eq!(parse(s).to_string(), s);
        }
        assert_eq!(serde_json::to_string(&parse("13.7s")).unwrap(), "\"13.7s\"");
    }

    /// The nanoseconds a seconds or clock argument names, computed in 128
    /// bits (a reference for the parser); `None` for frames and timecodes.
    fn reference_ns(text: &str) -> Option<u128> {
        let body = text.trim_start_matches(['+', '-']);
        let (whole, frac) = if let Some(secs) = body.strip_suffix('s') {
            let (int, frac) = secs.split_once('.').unwrap_or((secs, ""));
            (int.parse::<u128>().ok()?, frac)
        } else {
            let parts: Vec<&str> = body.split(':').collect();
            let [h, m, sf] = parts.as_slice() else {
                return None;
            };
            let (sec, frac) = sf.split_once('.')?;
            let secs = h.parse::<u128>().ok()? * 3600 + m.parse::<u128>().ok()? * 60 + sec.parse::<u128>().ok()?;
            (secs, frac)
        };
        let frac_ns: u128 = if frac.is_empty() { 0 } else { format!("{frac:0<9}").parse().ok()? };
        Some(whole * 1_000_000_000 + frac_ns)
    }

    fn any_fps() -> impl Strategy<Value = Fps> {
        prop_oneof![
            Just(Fps::new(24, 1)),
            Just(Fps::new(25, 1)),
            Just(Fps::new(30, 1)),
            Just(Fps::new(60, 1)),
            Just(Fps::new(24000, 1001)),
            Just(Fps::new(30000, 1001)),
            Just(Fps::new(60000, 1001)),
        ]
    }

    proptest! {
        #[test]
        fn frame_ns_round_trips(f in -1_000i64..10_000_000, fps in any_fps()) {
            let ns = frame_ns(f, fps);
            let next = frame_ns(f + 1, fps);
            prop_assert!(next > ns);
            // frame_ns(f) is the first instant of frame f, frame_ns(f+1)-1 its last.
            prop_assert_eq!(ns_to_frame_floor(ns, fps), f);
            prop_assert_eq!(ns_to_frame_floor(next - 1, fps), f);
            prop_assert_eq!(ns_to_frame_ceil(ns, fps), f);
            prop_assert_eq!(ns_to_frame_ceil(ns + 1, fps), f + 1);
            prop_assert_eq!(ns_to_frame_round(ns, fps), f);
        }

        /// Every time argument, however large, parses and converts to
        /// frames or is a usage error: no overflow panics, no wrapped times.
        #[test]
        fn time_arguments_never_overflow(
            text in "[+-]?[0-9]{1,24}(f|s|\\.[0-9]{1,9}s|:[0-5][0-9]:[0-5][0-9](:[0-9]{1,3}|\\.[0-9]{1,9}))",
            fps in any_fps(),
        ) {
            let parsed = text.parse::<TimeArg>();
            if let Some(exact) = reference_ns(&text) {
                // Seconds and clock values are exact, or too large for u64.
                match &parsed {
                    Ok(arg) => {
                        let ns = match arg.value {
                            TimeValue::Seconds(ns) | TimeValue::Clock(ns) => ns,
                            other => panic!("{text} parsed as {other:?}"),
                        };
                        prop_assert_eq!(u128::from(ns), exact, "{}", text);
                    }
                    Err(e) => {
                        prop_assert!(exact > u128::from(u64::MAX), "{} rejected: {}", text, e);
                        prop_assert!(e.message.contains("too large"), "{}: {}", text, e);
                    }
                }
            }
            match parsed {
                Ok(arg) => match arg.signed_frames(fps) {
                    Ok(frames) => {
                        // A converted clock or seconds value is the nearest frame.
                        if let TimeValue::Seconds(ns) | TimeValue::Clock(ns) = arg.value {
                            let ns = i64::try_from(ns).unwrap();
                            prop_assert_eq!(frames.unsigned_abs(), ns_to_frame_round(ns, fps).unsigned_abs());
                        }
                        prop_assert!(arg.resolve(0, fps).is_ok());
                    }
                    Err(e) => prop_assert_eq!(e.code, crate::error::ErrorCode::Usage),
                },
                Err(e) => prop_assert_eq!(e.code, crate::error::ErrorCode::Usage),
            }
        }

        #[test]
        fn parse_display_round_trips(n in 0u64..100_000_000, which in 0u8..4, sign in 0u8..3) {
            let value = match which {
                0 => TimeValue::Frames(n),
                1 => TimeValue::Seconds(n * 1_000_000),
                2 => TimeValue::Clock(n * 1_000_000),
                _ => TimeValue::Timecode { h: n % 100, m: (n % 60) as u8, s: (n / 7 % 60) as u8, f: (n % 30) as u32 },
            };
            let sign = [Sign::Absolute, Sign::Plus, Sign::Minus][sign as usize];
            let arg = TimeArg { sign, value };
            prop_assert_eq!(arg.to_string().parse::<TimeArg>().unwrap(), arg);
        }
    }
}
