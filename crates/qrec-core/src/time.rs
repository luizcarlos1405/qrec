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

/// The frame whose start is nearest to `ns`.
pub fn ns_to_frame_round(ns: i64, fps: Fps) -> i64 {
    let f = ns_to_frame_floor(ns, fps);
    let (start, next) = (frame_ns(f, fps), frame_ns(f + 1, fps));
    if (ns - start) * 2 >= next - start { f + 1 } else { f }
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
        Ok(match self.value {
            TimeValue::Frames(n) => n as i64,
            TimeValue::Seconds(ns) | TimeValue::Clock(ns) => ns_to_frame_round(ns as i64, fps),
            TimeValue::Timecode { h, m, s, f } => {
                let nominal = fps.nominal();
                if f >= nominal {
                    return Err(CoreError::usage(format!(
                        "timecode frame {f:02} is out of range at {fps} fps (max {:02})",
                        nominal - 1
                    )));
                }
                ((h * 3600 + u64::from(m) * 60 + u64::from(s)) * u64::from(nominal) + u64::from(f)) as i64
            }
        })
    }

    /// The signed value in frames: the magnitude, negated for `-`.
    pub fn signed_frames(&self, fps: Fps) -> Result<i64, CoreError> {
        let m = self.magnitude_frames(fps)?;
        Ok(if self.sign == Sign::Minus { -m } else { m })
    }

    /// Resolves against `base`: absolute values replace it, relative values move it.
    pub fn resolve(&self, base: i64, fps: Fps) -> Result<i64, CoreError> {
        let v = self.signed_frames(fps)?;
        Ok(if self.is_relative() { base + v } else { v })
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
            TimeValue::Frames(n.parse().map_err(|_| usage())?)
        } else if let Some(n) = body.strip_suffix('s') {
            let (int, frac) = n.split_once('.').unwrap_or((n, ""));
            if !all_digits(int) {
                return Err(usage());
            }
            let frac_ns = if frac.is_empty() && !n.contains('.') { 0 } else { parse_fraction_ns(frac).ok_or_else(usage)? };
            let secs: u64 = int.parse().map_err(|_| usage())?;
            TimeValue::Seconds(secs.checked_mul(NS_PER_SEC as u64).and_then(|v| v.checked_add(frac_ns)).ok_or_else(usage)?)
        } else if body.contains(':') {
            let parts: Vec<&str> = body.split(':').collect();
            match parts.as_slice() {
                [h, m, s, f] if all_digits(h) && m.len() == 2 && s.len() == 2 && all_digits(m) && all_digits(s) && all_digits(f) && f.len() <= 3 => {
                    let (m, s): (u8, u8) = (m.parse().map_err(|_| usage())?, s.parse().map_err(|_| usage())?);
                    if m >= 60 || s >= 60 {
                        return Err(usage());
                    }
                    TimeValue::Timecode { h: h.parse().map_err(|_| usage())?, m, s, f: f.parse().map_err(|_| usage())? }
                }
                [h, m, sf] if all_digits(h) && m.len() == 2 && all_digits(m) => {
                    let (s, frac) = sf.split_once('.').ok_or_else(usage)?;
                    if s.len() != 2 || !all_digits(s) {
                        return Err(usage());
                    }
                    let (h, m, s): (u64, u64, u64) =
                        (h.parse().map_err(|_| usage())?, m.parse().map_err(|_| usage())?, s.parse().map_err(|_| usage())?);
                    if m >= 60 || s >= 60 {
                        return Err(usage());
                    }
                    let frac_ns = parse_fraction_ns(frac).ok_or_else(usage)?;
                    TimeValue::Clock((h * 3600 + m * 60 + s) * NS_PER_SEC as u64 + frac_ns)
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
    fn display_round_trips() {
        for s in ["412f", "+1f", "-2s", "13.7s", "00:00:13:21", "00:00:13.700", "-00:00:01:00", "0.000000001s"] {
            assert_eq!(parse(s).to_string(), s);
        }
        assert_eq!(serde_json::to_string(&parse("13.7s")).unwrap(), "\"13.7s\"");
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
