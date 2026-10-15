//! ffprobe output parsers (A§9.2). Owner: output.
//!
//! [`parse_ffprobe`] reads what `media_argv::probe_argv` asks for (`-of
//! json`). ffprobe prints some numbers as JSON strings (`"nb_read_packets":
//! "150"`, `"sample_rate": "48000"`) and others as numbers (`"width": 320`);
//! both are accepted. `N/A`, `unknown` and missing fields are `None`.
//!
//! A duration comes from the stream, the Matroska `DURATION` tag or the
//! format; a recording cut short by a crash has none of them, so its length
//! comes from the packets `-count_packets` counted (A§2.2): video frames at
//! the stream's rate, or FLAC frames of `capture_plan::FLAC_BLOCK_SAMPLES`
//! samples each (every frame of such a file is full).
//!
//! [`parse_segment_check`] reads `media_argv::segment_check_argv`'s packet
//! lines: the counts an export segment is verified against (A§9.3).

use serde_json::Value;

use crate::capture_plan::FLAC_BLOCK_SAMPLES;
use crate::model::{Fps, StreamKind};
use crate::protocol::event::MediaInfo;
use crate::time::{NS_PER_SEC, frame_ns};

/// Parses the JSON printed by `media_argv::probe_argv(_, kind)`. `Err` for
/// unreadable files: bad JSON, no stream of that kind, no packets, a video
/// stream without a size or an audio stream without a sample rate.
pub fn parse_ffprobe(json: &str, kind: StreamKind) -> Result<MediaInfo, String> {
    let what = match kind {
        StreamKind::Video => "video",
        StreamKind::Audio => "audio",
    };
    let root: Value = serde_json::from_str(json)
        .map_err(|e| format!("ffprobe printed something that isn't JSON: {e}"))?;
    let stream = root
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| streams.first())
        .ok_or_else(|| format!("the file has no {what} stream"))?;
    let format = root.get("format");

    let packets = count(stream.get("nb_read_packets"));
    if packets == Some(0) {
        return Err(format!("the {what} stream has no packets"));
    }
    let positive = |key: &str| count(stream.get(key)).filter(|&v| v > 0).map(|v| v as u32);
    let (width, height) = (positive("width"), positive("height"));
    let (sample_rate, channels) = (positive("sample_rate"), positive("channels"));
    let fps = match kind {
        StreamKind::Video => stream.get("r_frame_rate").and_then(rational),
        StreamKind::Audio => None,
    };
    match kind {
        StreamKind::Video if width.is_none() || height.is_none() => {
            return Err("the video stream has no picture size".to_owned());
        }
        StreamKind::Audio if sample_rate.is_none() => {
            return Err("the audio stream has no sample rate".to_owned());
        }
        _ => {}
    }
    let frames = match kind {
        StreamKind::Video => packets,
        StreamKind::Audio => None,
    };
    let flac = stream.get("codec_name").and_then(Value::as_str) == Some("flac");
    let from_packets = || match kind {
        StreamKind::Video => Some(frame_ns(frames? as i64, fps?)),
        StreamKind::Audio if flac => flac_ns(packets?, sample_rate?),
        StreamKind::Audio => None,
    };
    let duration_ns = stream
        .get("duration")
        .and_then(seconds)
        .or_else(|| {
            stream
                .get("tags")
                .and_then(|tags| tags.get("DURATION"))
                .and_then(clock)
        })
        .or_else(|| format.and_then(|f| f.get("duration")).and_then(seconds))
        .or_else(from_packets);
    let start_time_ns = format
        .and_then(|f| f.get("start_time"))
        .and_then(seconds)
        .unwrap_or(0);
    Ok(MediaInfo {
        kind,
        frames,
        fps,
        width: if kind == StreamKind::Video {
            width
        } else {
            None
        },
        height: if kind == StreamKind::Video {
            height
        } else {
            None
        },
        sample_rate: if kind == StreamKind::Audio {
            sample_rate
        } else {
            None
        },
        channels: if kind == StreamKind::Audio {
            channels
        } else {
            None
        },
        duration_ns,
        start_time_ns,
        color_range: name(stream.get("color_range")),
        color_space: name(stream.get("color_space")),
    })
}

/// Bytes per sample of a segment's audio: stereo `pcm_s16le`.
pub const SEGMENT_SAMPLE_BYTES: u64 = 4;

/// What a rendered segment holds, from `media_argv::segment_check_argv`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentCounts {
    pub video_packets: u64,
    /// Stereo samples of the PCM audio.
    pub audio_samples: u64,
}

impl SegmentCounts {
    /// `Ok` when the segment has exactly `frames` video packets and
    /// `samples` audio samples (A§9.3); otherwise what is wrong.
    pub fn check(&self, frames: u64, samples: u64) -> Result<(), String> {
        let mut wrong = Vec::new();
        if self.video_packets != frames {
            wrong.push(format!(
                "{} video frames instead of {frames}",
                self.video_packets
            ));
        }
        if self.audio_samples != samples {
            wrong.push(format!(
                "{} audio samples instead of {samples}",
                self.audio_samples
            ));
        }
        if wrong.is_empty() {
            Ok(())
        } else {
            Err(format!("the segment has {}", wrong.join(" and ")))
        }
    }
}

/// Parses the `codec_type,size` lines of `media_argv::segment_check_argv`.
/// `Err` for a line that isn't one, or audio that isn't whole stereo s16
/// samples.
pub fn parse_segment_check(csv: &str) -> Result<SegmentCounts, String> {
    let mut counts = SegmentCounts::default();
    let mut audio_bytes: u64 = 0;
    for line in csv.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (kind, size) = line
            .split_once(',')
            .ok_or_else(|| format!("ffprobe printed an unexpected packet line: {line:?}"))?;
        let size: u64 = size
            .trim()
            .parse()
            .map_err(|_| format!("ffprobe printed an unexpected packet size: {line:?}"))?;
        match kind.trim() {
            "video" => counts.video_packets += 1,
            "audio" => audio_bytes += size,
            _ => {}
        }
    }
    if !audio_bytes.is_multiple_of(SEGMENT_SAMPLE_BYTES) {
        return Err(format!(
            "the segment's audio is {audio_bytes} bytes, not whole stereo 16-bit samples"
        ));
    }
    counts.audio_samples = audio_bytes / SEGMENT_SAMPLE_BYTES;
    Ok(counts)
}

/// The length of `packets` FLAC frames of `FLAC_BLOCK_SAMPLES` samples at
/// `rate` Hz, in nanoseconds (rounded down).
fn flac_ns(packets: u64, rate: u32) -> Option<i64> {
    if rate == 0 {
        return None;
    }
    let samples = u128::from(packets) * u128::from(FLAC_BLOCK_SAMPLES);
    i64::try_from(samples * NS_PER_SEC as u128 / u128::from(rate)).ok()
}

/// A non-negative integer printed as a number or a string.
fn count(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `30/1`, `30000/1001` → `Fps` (reduced); `0/0` → `None`.
fn rational(value: &Value) -> Option<Fps> {
    let (num, den) = value.as_str()?.split_once('/')?;
    let (num, den): (u32, u32) = (num.trim().parse().ok()?, den.trim().parse().ok()?);
    if num == 0 || den == 0 {
        return None;
    }
    let g = gcd(num, den);
    Some(Fps::new(num / g, den / g))
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Decimal seconds (`"5.000000"`, `"-0.007"`, or a number) → nanoseconds,
/// exactly for up to nine decimals.
fn seconds(value: &Value) -> Option<i64> {
    match value {
        Value::String(s) => decimal_ns(s.trim()),
        Value::Number(n) => n.as_i64().map(|s| s * NS_PER_SEC).or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite())
                .map(|f| (f * NS_PER_SEC as f64).round() as i64)
        }),
        _ => None,
    }
}

fn decimal_ns(s: &str) -> Option<i64> {
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut frac_ns: i64 = 0;
    for (i, digit) in frac.bytes().take(9).enumerate() {
        frac_ns += i64::from(digit - b'0') * 10i64.pow(8 - i as u32);
    }
    let ns = int
        .parse::<i64>()
        .ok()?
        .checked_mul(NS_PER_SEC)?
        .checked_add(frac_ns)?;
    Some(if negative { -ns } else { ns })
}

/// The Matroska `DURATION` tag: `HH:MM:SS.nnnnnnnnn` → nanoseconds.
fn clock(value: &Value) -> Option<i64> {
    let text = value.as_str()?.trim();
    let mut parts = text.splitn(3, ':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
    let s = decimal_ns(s)?;
    if h < 0 || !(0..60).contains(&m) || s < 0 {
        return None;
    }
    Some((h * 3600 + m * 60) * NS_PER_SEC + s)
}

/// A colour property name; `unknown`/`unspecified` count as absent.
fn name(value: Option<&Value>) -> Option<String> {
    let s = value?.as_str()?.trim();
    match s {
        "" | "unknown" | "unspecified" | "N/A" => None,
        _ => Some(s.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What ffprobe 8.1.2 prints for an ffmpeg-written MKV (H.264, 30 fps).
    const VIDEO: &str = r#"{
    "programs": [

    ],
    "stream_groups": [

    ],
    "streams": [
        {
            "width": 320,
            "height": 180,
            "pix_fmt": "yuv420p",
            "color_range": "tv",
            "color_space": "bt709",
            "r_frame_rate": "30/1",
            "nb_read_packets": "150",
            "tags": {
                "DURATION": "00:00:05.000000000"
            }
        }
    ],
    "format": {
        "start_time": "0.000000",
        "duration": "5.000000"
    }
}"#;

    /// An FLAC MKA (mono, 48 kHz).
    const AUDIO: &str = r#"{
    "programs": [],
    "stream_groups": [],
    "streams": [
        {
            "codec_name": "flac",
            "sample_rate": "48000",
            "channels": 1,
            "r_frame_rate": "0/0",
            "nb_read_packets": "32",
            "tags": {
                "DURATION": "00:00:03.000000000"
            }
        }
    ],
    "format": {
        "start_time": "0.000000",
        "duration": "3.000000"
    }
}"#;

    #[test]
    fn video_streams() {
        let info = parse_ffprobe(VIDEO, StreamKind::Video).unwrap();
        assert_eq!(
            info,
            MediaInfo {
                kind: StreamKind::Video,
                frames: Some(150),
                fps: Some(Fps::new(30, 1)),
                width: Some(320),
                height: Some(180),
                sample_rate: None,
                channels: None,
                duration_ns: Some(5_000_000_000),
                start_time_ns: 0,
                color_range: Some("tv".into()),
                color_space: Some("bt709".into()),
            }
        );
    }

    #[test]
    fn audio_streams() {
        let info = parse_ffprobe(AUDIO, StreamKind::Audio).unwrap();
        assert_eq!(info.kind, StreamKind::Audio);
        assert_eq!(info.sample_rate, Some(48_000));
        assert_eq!(info.channels, Some(1));
        assert_eq!(info.frames, None, "packets only count video frames");
        assert_eq!(info.fps, None);
        assert_eq!((info.width, info.height), (None, None));
        assert_eq!(info.duration_ns, Some(3_000_000_000));
        assert_eq!(info.color_range, None);
    }

    #[test]
    fn duration_fallbacks() {
        // Stream duration wins, then the DURATION tag, then the format.
        let json = r#"{"streams":[{"width":2,"height":2,"r_frame_rate":"30000/1001","nb_read_packets":"30",
            "duration":"1.001000","tags":{"DURATION":"00:00:09.000000000"}}],"format":{"duration":"7.0"}}"#;
        let info = parse_ffprobe(json, StreamKind::Video).unwrap();
        assert_eq!(info.duration_ns, Some(1_001_000_000));
        assert_eq!(info.fps, Some(Fps::new(30000, 1001)));
        let json = r#"{"streams":[{"width":2,"height":2,"r_frame_rate":"30/1","nb_read_packets":"30",
            "tags":{"DURATION":"01:02:03.500000000"}}],"format":{"duration":"7.0"}}"#;
        let info = parse_ffprobe(json, StreamKind::Video).unwrap();
        assert_eq!(info.duration_ns, Some((3723 * 1000 + 500) * 1_000_000));
        let json = r#"{"streams":[{"width":2,"height":2,"r_frame_rate":"30/1","nb_read_packets":"30"}],
            "format":{"duration":"7.25","start_time":"-0.007000"}}"#;
        let info = parse_ffprobe(json, StreamKind::Video).unwrap();
        assert_eq!(info.duration_ns, Some(7_250_000_000));
        assert_eq!(info.start_time_ns, -7_000_000);
        // A crashed recording: no durations at all. Video falls back to the
        // packet count at the stream's rate.
        let json = r#"{"streams":[{"width":2,"height":2,"r_frame_rate":"30/1","nb_read_packets":"45"}],"format":{}}"#;
        let info = parse_ffprobe(json, StreamKind::Video).unwrap();
        assert_eq!(info.duration_ns, Some(1_500_000_000));
        assert_eq!(info.start_time_ns, 0);
        // FLAC audio falls back to its packets, 4608 samples each.
        let json = r#"{"streams":[{"codec_name":"flac","sample_rate":44100,"channels":2,"nb_read_packets":"9"}],"format":{"duration":"N/A"}}"#;
        let info = parse_ffprobe(json, StreamKind::Audio).unwrap();
        assert_eq!(info.duration_ns, Some(9 * 4608 * 1_000_000_000 / 44_100));
        assert_eq!(info.channels, Some(2));
        // Other codecs have no fixed frame size: unknown.
        let json = r#"{"streams":[{"codec_name":"opus","sample_rate":48000,"channels":2,"nb_read_packets":"9"}],"format":{}}"#;
        assert_eq!(
            parse_ffprobe(json, StreamKind::Audio).unwrap().duration_ns,
            None
        );
    }

    /// What ffprobe 8.1.2 prints for the mic of a take whose host was
    /// SIGKILLed after ~3.4 s: matroskamux never wrote a duration, and
    /// the last packet ends at 3.456 s (36 FLAC frames of 96 ms).
    const CRASHED_AUDIO: &str = r#"{
    "programs": [

    ],
    "stream_groups": [

    ],
    "streams": [
        {
            "codec_name": "flac",
            "sample_rate": "48000",
            "channels": 1,
            "r_frame_rate": "0/0",
            "nb_read_packets": "36",
            "tags": {

            }
        }
    ],
    "format": {
        "start_time": "0.000000"
    }
}"#;

    #[test]
    fn audio_cut_short_by_a_crash_is_as_long_as_its_packets() {
        let info = parse_ffprobe(CRASHED_AUDIO, StreamKind::Audio).unwrap();
        assert_eq!(info.duration_ns, Some(3_456_000_000));
        assert_eq!((info.sample_rate, info.channels), (Some(48_000), Some(1)));
        assert_eq!(info.frames, None, "packets only count video frames");
    }

    #[test]
    fn unreadable_files() {
        assert!(
            parse_ffprobe("", StreamKind::Video)
                .unwrap_err()
                .contains("isn't JSON")
        );
        assert!(
            parse_ffprobe("{}", StreamKind::Video)
                .unwrap_err()
                .contains("no video stream")
        );
        assert!(
            parse_ffprobe(r#"{"programs":[],"streams":[]}"#, StreamKind::Audio)
                .unwrap_err()
                .contains("no audio stream")
        );
        let empty = r#"{"streams":[{"width":2,"height":2,"nb_read_packets":"0"}]}"#;
        assert!(
            parse_ffprobe(empty, StreamKind::Video)
                .unwrap_err()
                .contains("no packets")
        );
        let no_size = r#"{"streams":[{"nb_read_packets":"3"}]}"#;
        assert!(
            parse_ffprobe(no_size, StreamKind::Video)
                .unwrap_err()
                .contains("size")
        );
        let no_rate = r#"{"streams":[{"channels":1,"nb_read_packets":"3"}]}"#;
        assert!(
            parse_ffprobe(no_rate, StreamKind::Audio)
                .unwrap_err()
                .contains("sample rate")
        );
    }

    #[test]
    fn segment_checks() {
        let csv = "video,4394\nvideo,1789\naudio,4096\n\naudio,4096\r\naudio,2048\nvideo,881\nsubtitle,3\n";
        let counts = parse_segment_check(csv).unwrap();
        assert_eq!(
            counts,
            SegmentCounts {
                video_packets: 3,
                audio_samples: 2560
            }
        );
        assert_eq!(counts.check(3, 2560), Ok(()));
        assert_eq!(
            counts.check(4, 2560).unwrap_err(),
            "the segment has 3 video frames instead of 4"
        );
        assert_eq!(
            counts.check(2, 2561).unwrap_err(),
            "the segment has 3 video frames instead of 2 and 2560 audio samples instead of 2561"
        );
        assert_eq!(parse_segment_check("").unwrap(), SegmentCounts::default());
        assert!(
            parse_segment_check("audio,6")
                .unwrap_err()
                .contains("whole stereo")
        );
        assert!(
            parse_segment_check("video")
                .unwrap_err()
                .contains("unexpected packet line")
        );
        assert!(
            parse_segment_check("video,N/A")
                .unwrap_err()
                .contains("unexpected packet size")
        );
    }

    #[test]
    fn odd_values() {
        let json = r#"{"streams":[{"width":2,"height":2,"r_frame_rate":"0/0","nb_read_packets":"N/A",
            "color_range":"unknown","color_space":"bt470bg"}]}"#;
        let info = parse_ffprobe(json, StreamKind::Video).unwrap();
        assert_eq!(info.fps, None);
        assert_eq!(info.frames, None);
        assert_eq!(info.duration_ns, None);
        assert_eq!(info.color_range, None);
        assert_eq!(info.color_space.as_deref(), Some("bt470bg"));
        assert_eq!(rational(&Value::from("60/2")), Some(Fps::new(30, 1)));
        assert_eq!(decimal_ns("1.2345678919"), Some(1_234_567_891));
        assert_eq!(decimal_ns("12"), Some(12_000_000_000));
        assert_eq!(decimal_ns("N/A"), None);
        assert_eq!(decimal_ns("1.2x"), None);
        assert_eq!(clock(&Value::from("00:61:00.0")), None);
    }
}
