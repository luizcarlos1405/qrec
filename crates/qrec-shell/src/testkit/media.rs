//! Fixture media generated with ffmpeg and cached in
//! `<target>/qrec-fixtures/<sha256 of the spec>.<ext>`, written to a temp
//! file and renamed into place so parallel test processes share one copy.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use qrec_core::Fps;
use qrec_core::hash::sha256_hex;
use qrec_core::time::{NS_PER_MS, NS_PER_SEC, frame_ns};
use serde_json::Value;

use super::target_dir;
use super::tool::run_tool;

/// Bump when the generated media changes, so cached files aren't reused.
const GENERATOR_VERSION: u32 = 1;
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(120);

/// Serializes generation inside one process; other processes are covered by
/// the temp file + rename.
static GENERATING: Mutex<()> = Mutex::new(());
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// What a fixture file holds: video, audio or both, in one Matroska file
/// (`.mkv` with video, `.mka` for audio only).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MediaSpec {
    pub video: Option<VideoSpec>,
    pub audio: Option<AudioSpec>,
}

/// `testsrc2` video: H.264 (no B-frames) with a keyframe every second,
/// yuv420p tagged limited-range BT.709.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VideoSpec {
    pub width: u32,
    pub height: u32,
    pub fps: Fps,
    pub frames: u64,
}

/// A 440 Hz tone at -6 dBFS, audible only inside `speech_ms` (half-open
/// `[start, end)` intervals in ms); digital silence everywhere else. FLAC,
/// 16-bit.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AudioSpec {
    pub sample_rate: u32,
    pub channels: u32,
    /// The audio length; `None` = the video's length, or the end of the last
    /// speech interval without video.
    pub duration_ms: Option<u64>,
    pub speech_ms: Vec<(u64, u64)>,
}

impl MediaSpec {
    /// `frames` frames of `width`×`height` video at `fps` frames per second.
    pub fn video(width: u32, height: u32, fps: u32, frames: u64) -> Self {
        Self {
            video: Some(VideoSpec {
                width,
                height,
                fps: Fps::new(fps, 1),
                frames,
            }),
            audio: None,
        }
    }

    /// Audio only, `duration_ms` long, audible inside `speech_ms`.
    pub fn audio(
        sample_rate: u32,
        channels: u32,
        duration_ms: u64,
        speech_ms: &[(u64, u64)],
    ) -> Self {
        let audio = AudioSpec {
            sample_rate,
            channels,
            duration_ms: Some(duration_ms),
            speech_ms: speech_ms.to_vec(),
        };
        Self {
            video: None,
            audio: Some(audio),
        }
    }

    /// Adds audio exactly as long as the video, audible inside `speech_ms`.
    pub fn with_audio(mut self, sample_rate: u32, channels: u32, speech_ms: &[(u64, u64)]) -> Self {
        self.audio = Some(AudioSpec {
            sample_rate,
            channels,
            duration_ms: None,
            speech_ms: speech_ms.to_vec(),
        });
        self
    }

    /// Changes the video frame rate, e.g. to `Fps::new(30000, 1001)`.
    pub fn with_fps(mut self, fps: Fps) -> Self {
        if let Some(video) = &mut self.video {
            video.fps = fps;
        }
        self
    }

    /// `mkv` when there is video, `mka` for audio only.
    pub fn extension(&self) -> &'static str {
        if self.video.is_some() { "mkv" } else { "mka" }
    }

    /// The video's length in ns (`frame_ns(frames)`), else the audio's.
    pub fn duration_ns(&self) -> i64 {
        match self.video {
            Some(video) => frame_ns(video.frames as i64, video.fps),
            None => self.audio_duration_ns(),
        }
    }

    /// The video part alone: one layer's file in a take.
    pub fn video_only(&self) -> Option<MediaSpec> {
        self.video.map(|video| MediaSpec {
            video: Some(video),
            audio: None,
        })
    }

    /// The audio part alone: one track's file in a take. A length taken from
    /// the video is rounded up to whole milliseconds.
    pub fn audio_only(&self) -> Option<MediaSpec> {
        let mut audio = self.audio.clone()?;
        audio.duration_ms =
            Some((self.audio_duration_ns().max(0) as u64).div_ceil(NS_PER_MS as u64));
        Some(MediaSpec {
            video: None,
            audio: Some(audio),
        })
    }

    fn audio_duration_ns(&self) -> i64 {
        let Some(audio) = &self.audio else { return 0 };
        match (audio.duration_ms, self.video) {
            (Some(ms), _) => ms as i64 * NS_PER_MS,
            (None, Some(video)) => frame_ns(video.frames as i64, video.fps),
            (None, None) => audio
                .speech_ms
                .iter()
                .map(|&(_, end)| end as i64 * NS_PER_MS)
                .max()
                .unwrap_or(0),
        }
    }

    fn audio_samples(&self) -> u64 {
        let rate = self.audio.as_ref().map_or(0, |a| i128::from(a.sample_rate));
        ((i128::from(self.audio_duration_ns()) * rate + i128::from(NS_PER_SEC) / 2)
            / i128::from(NS_PER_SEC)) as u64
    }

    /// Everything that determines the file's content, resolved.
    fn cache_key(&self) -> String {
        let mut key = format!("qrec-fixture v{GENERATOR_VERSION}\n");
        if let Some(v) = self.video {
            let _ = writeln!(
                key,
                "video {}x{} {}/{} {}",
                v.width, v.height, v.fps.num, v.fps.den, v.frames
            );
        }
        if let Some(a) = &self.audio {
            let _ = writeln!(
                key,
                "audio {} {} {} {:?}",
                a.sample_rate,
                a.channels,
                self.audio_samples(),
                a.speech_ms
            );
        }
        key
    }

    /// The ffmpeg arguments that write this media to `out` (Matroska).
    fn ffmpeg_args(&self, out: &Path) -> Vec<String> {
        let mut graph = Vec::new();
        let mut args: Vec<String> = [
            "-hide_banner",
            "-nostdin",
            "-nostats",
            "-loglevel",
            "error",
            "-y",
        ]
        .map(String::from)
        .to_vec();
        let mut outputs: Vec<String> = Vec::new();
        if let Some(v) = self.video {
            graph.push(format!(
                "testsrc2=size={w}x{h}:rate={num}/{den},trim=end_frame={frames},setpts=PTS-STARTPTS,\
                 format=pix_fmts=yuv420p:color_spaces=bt709:color_ranges=tv,\
                 setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709[v]",
                w = v.width,
                h = v.height,
                num = v.fps.num,
                den = v.fps.den,
                frames = v.frames,
            ));
            let gop = ((v.fps.num + v.fps.den / 2) / v.fps.den).max(1).to_string();
            outputs.extend(
                [
                    "-map",
                    "[v]",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "veryfast",
                    "-crf",
                    "20",
                    "-bf",
                    "0",
                    "-g",
                    &gop,
                    "-keyint_min",
                    &gop,
                    "-sc_threshold",
                    "0",
                    "-pix_fmt",
                    "yuv420p",
                    "-color_range",
                    "tv",
                    "-colorspace",
                    "bt709",
                    "-color_primaries",
                    "bt709",
                    "-color_trc",
                    "bt709",
                ]
                .map(String::from),
            );
        }
        if let Some(a) = &self.audio {
            graph.push(format!(
                "aevalsrc=exprs='{expr}':s={rate}:c={layout},atrim=end_sample={samples}[a]",
                expr = tone_expression(&a.speech_ms),
                rate = a.sample_rate,
                layout = channel_layout(a.channels),
                samples = self.audio_samples(),
            ));
            outputs.extend(["-map", "[a]", "-c:a", "flac", "-sample_fmt", "s16"].map(String::from));
        }
        args.push("-filter_complex".into());
        args.push(graph.join(";"));
        args.extend(outputs);
        args.extend([
            "-f".into(),
            "matroska".into(),
            format!("file:{}", out.display()),
        ]);
        args
    }

    fn check(&self) {
        if let Some(v) = self.video {
            assert!(
                v.width >= 2 && v.height >= 2 && v.width % 2 == 0 && v.height % 2 == 0,
                "{v:?}: use an even size"
            );
            assert!(
                v.frames > 0 && v.fps.num > 0 && v.fps.den > 0,
                "{v:?}: needs frames and a frame rate"
            );
        }
        if let Some(a) = &self.audio {
            assert!(
                a.sample_rate > 0 && (1..=8).contains(&a.channels),
                "{a:?}: bad rate or channel count"
            );
            assert!(
                a.speech_ms.iter().all(|&(start, end)| start < end),
                "{a:?}: speech intervals need start < end"
            );
            assert!(self.audio_samples() > 0, "{a:?}: the audio has no length");
        }
        assert!(
            self.video.is_some() || self.audio.is_some(),
            "an empty MediaSpec"
        );
    }
}

/// `if(gt(Σ gte(t,start)·lt(t,end), 0), 0.5·sin(2π·440·t), 0)`.
fn tone_expression(speech_ms: &[(u64, u64)]) -> String {
    if speech_ms.is_empty() {
        return "0".into();
    }
    let secs = |ms: u64| format!("{}.{:03}", ms / 1000, ms % 1000);
    let gates: Vec<String> = speech_ms
        .iter()
        .map(|&(start, end)| format!("gte(t,{})*lt(t,{})", secs(start), secs(end)))
        .collect();
    format!("if(gt({},0),0.5*sin(2*PI*440*t),0)", gates.join("+"))
}

fn channel_layout(channels: u32) -> String {
    match channels {
        1 => "mono".into(),
        2 => "stereo".into(),
        n => format!("{n}c"),
    }
}

/// `<target>/qrec-fixtures/`, created if needed.
pub fn fixtures_dir() -> PathBuf {
    let dir = target_dir().join("qrec-fixtures");
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    dir
}

/// The fixture file for `spec`, generated with ffmpeg on first use and
/// cached by the hash of the spec. Treat it as read-only: tests share it.
pub fn fixture(spec: &MediaSpec) -> PathBuf {
    spec.check();
    let dir = fixtures_dir();
    let path = dir.join(format!(
        "{}.{}",
        sha256_hex(spec.cache_key().as_bytes()),
        spec.extension()
    ));
    if path.is_file() {
        return path;
    }
    let _guard = GENERATING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if path.is_file() {
        return path;
    }
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        ".{}.{}-{n}.part",
        path.file_stem().unwrap().to_string_lossy(),
        std::process::id()
    ));
    let output = run_tool(
        Command::new("ffmpeg").args(spec.ffmpeg_args(&tmp)),
        FFMPEG_TIMEOUT,
    );
    if !output.status.success() {
        let _ = fs::remove_file(&tmp);
        panic!(
            "ffmpeg failed to generate {spec:?} ({}):\n{}",
            output.status, output.stderr
        );
    }
    verify(spec, &tmp);
    fs::rename(&tmp, &path).unwrap_or_else(|e| panic!("rename {} into place: {e}", tmp.display()));
    path
}

/// Checks a freshly generated file against its spec before it is cached.
fn verify(spec: &MediaSpec, file: &Path) {
    let probe = ffprobe(file);
    let streams = probe["streams"].as_array().cloned().unwrap_or_default();
    let of_type = |kind: &str| {
        streams
            .iter()
            .find(|s| s["codec_type"] == kind)
            .cloned()
            .unwrap_or(Value::Null)
    };
    if let Some(v) = spec.video {
        let video = of_type("video");
        assert_eq!(
            video["nb_read_packets"].as_str(),
            Some(v.frames.to_string().as_str()),
            "{spec:?}: frame count"
        );
        assert_eq!(
            (video["width"].as_u64(), video["height"].as_u64()),
            (Some(v.width.into()), Some(v.height.into()))
        );
    }
    if let Some(a) = &spec.audio {
        let audio = of_type("audio");
        assert_eq!(
            audio["sample_rate"].as_str(),
            Some(a.sample_rate.to_string().as_str()),
            "{spec:?}: sample rate"
        );
        assert_eq!(
            audio["channels"].as_u64(),
            Some(a.channels.into()),
            "{spec:?}: channels"
        );
    }
}

/// `ffprobe -show_streams -show_format -count_packets` of `file`, as JSON.
pub fn ffprobe(file: &Path) -> Value {
    let output = run_tool(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_packets",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
            ])
            .arg(format!("file:{}", file.display())),
        FFMPEG_TIMEOUT,
    );
    assert!(
        output.status.success(),
        "ffprobe {} failed:\n{}",
        file.display(),
        output.stderr
    );
    serde_json::from_slice(&output.stdout).expect("ffprobe prints JSON")
}

/// The silences `(start, end)` in seconds that ffmpeg's `silencedetect`
/// finds in `file`'s first audio stream. A silence still running at the end
/// of the file ends at the stream's duration.
pub fn detect_silences(file: &Path, noise_db: f64, min_silence_ms: u64) -> Vec<(f64, f64)> {
    let filter = format!(
        "silencedetect=noise={noise_db}dB:d={}",
        min_silence_ms as f64 / 1000.0
    );
    let output = run_tool(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-nostats", "-i"])
            .arg(format!("file:{}", file.display()))
            .args(["-map", "0:a:0", "-af", &filter, "-f", "null", "-"]),
        FFMPEG_TIMEOUT,
    );
    assert!(
        output.status.success(),
        "silencedetect on {} failed:\n{}",
        file.display(),
        output.stderr
    );
    let value_after = |line: &str, key: &str| -> Option<f64> {
        let rest = &line[line.find(key)? + key.len()..];
        rest.split_whitespace().next()?.parse().ok()
    };
    let mut silences = Vec::new();
    let mut start = None;
    for line in output.stderr.lines() {
        if let Some(s) = value_after(line, "silence_start: ") {
            start = Some(s);
        } else if let Some(end) = value_after(line, "silence_end: ") {
            silences.push((start.take().unwrap_or(0.0), end));
        }
    }
    if let Some(start) = start {
        let probe = ffprobe(file);
        let audio = probe["streams"]
            .as_array()
            .and_then(|s| s.iter().find(|s| s["codec_type"] == "audio").cloned());
        let duration = audio
            .and_then(|a| a["duration"].as_str().and_then(|d| d.parse().ok()))
            .or_else(|| {
                probe["format"]["duration"]
                    .as_str()
                    .and_then(|d| d.parse().ok())
            })
            .expect("ffprobe reports a duration");
        silences.push((start, duration));
    }
    silences
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.005
    }

    #[test]
    fn video_and_audio_fixture() {
        let spec =
            MediaSpec::video(320, 180, 30, 60).with_audio(48_000, 1, &[(0, 500), (1500, 2000)]);
        let file = fixture(&spec);
        assert_eq!(file.extension().unwrap(), "mkv");
        assert!(file.starts_with(fixtures_dir()));

        let probe = ffprobe(&file);
        let streams = probe["streams"].as_array().unwrap();
        let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
        let audio = streams.iter().find(|s| s["codec_type"] == "audio").unwrap();
        assert_eq!(video["codec_name"], "h264");
        assert_eq!(video["nb_read_packets"], "60");
        assert_eq!(
            (video["width"].as_u64(), video["height"].as_u64()),
            (Some(320), Some(180))
        );
        assert_eq!(video["r_frame_rate"], "30/1");
        assert_eq!(
            (video["color_range"].as_str(), video["color_space"].as_str()),
            (Some("tv"), Some("bt709"))
        );
        assert_eq!(audio["codec_name"], "flac");
        assert_eq!(audio["sample_rate"], "48000");
        assert_eq!(audio["channels"], 1);
        let duration: f64 = probe["format"]["duration"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(close(duration, 2.0), "{duration}");

        // Exactly the gap between the two speech intervals is silent.
        let silences = detect_silences(&file, -50.0, 100);
        assert_eq!(silences.len(), 1, "{silences:?}");
        let (start, end) = silences[0];
        assert!(close(start, 0.5) && close(end, 1.5), "{silences:?}");

        // Cached: the same spec maps to the same file without regenerating.
        let modified = fs::metadata(&file).unwrap().modified().unwrap();
        assert_eq!(fixture(&spec.clone()), file);
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), modified);
    }

    #[test]
    fn keyframes_every_second() {
        let file = fixture(&MediaSpec::video(64, 36, 25, 75));
        let output = run_tool(
            Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "v:0",
                    "-skip_frame",
                    "nokey",
                ])
                .args(["-show_entries", "frame=pts_time", "-of", "csv=p=0"])
                .arg(format!("file:{}", file.display())),
            FFMPEG_TIMEOUT,
        );
        let keyframes: Vec<f64> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|l| l.trim_end_matches(',').parse().unwrap())
            .collect();
        assert_eq!(keyframes, [0.0, 1.0, 2.0]);
    }

    #[test]
    fn audio_only_fixture_with_trailing_silence() {
        let spec = MediaSpec::audio(44_100, 2, 1500, &[(200, 700)]);
        let file = fixture(&spec);
        assert_eq!(file.extension().unwrap(), "mka");
        let probe = ffprobe(&file);
        let audio = &probe["streams"][0];
        assert_eq!(
            (audio["sample_rate"].as_str(), audio["channels"].as_u64()),
            (Some("44100"), Some(2))
        );
        let silences = detect_silences(&file, -50.0, 100);
        assert_eq!(silences.len(), 2, "{silences:?}");
        assert!(
            close(silences[0].0, 0.0) && close(silences[0].1, 0.2),
            "{silences:?}"
        );
        assert!(
            close(silences[1].0, 0.7) && close(silences[1].1, 1.5),
            "{silences:?}"
        );
    }

    #[test]
    fn splitting_a_spec_keeps_the_lengths() {
        let spec = MediaSpec::video(320, 180, 30, 61)
            .with_fps(Fps::new(30000, 1001))
            .with_audio(48_000, 2, &[]);
        assert_eq!(spec.duration_ns(), 2_035_366_666);
        let video = spec.video_only().unwrap();
        assert_eq!(
            (video.audio.is_none(), video.duration_ns()),
            (true, 2_035_366_666)
        );
        let audio = spec.audio_only().unwrap();
        assert_eq!(audio.audio.as_ref().unwrap().duration_ms, Some(2036));
        assert_eq!(audio.extension(), "mka");
        // Audio without video or an explicit length ends with its last speech.
        let speech = AudioSpec {
            sample_rate: 48_000,
            channels: 1,
            duration_ms: None,
            speech_ms: vec![(0, 250)],
        };
        assert_eq!(
            MediaSpec {
                video: None,
                audio: Some(speech)
            }
            .duration_ns(),
            250 * NS_PER_MS
        );
    }

    #[test]
    fn cache_keys_follow_the_resolved_content() {
        let implicit = MediaSpec::video(320, 180, 30, 60).with_audio(48_000, 1, &[(0, 500)]);
        let mut explicit = implicit.clone();
        explicit.audio.as_mut().unwrap().duration_ms = Some(2000);
        assert_eq!(implicit.cache_key(), explicit.cache_key());
        assert_ne!(
            implicit.cache_key(),
            MediaSpec::video(320, 180, 30, 61).cache_key()
        );
        assert_eq!(tone_expression(&[]), "0");
        assert_eq!(
            tone_expression(&[(0, 500), (1500, 2250)]),
            "if(gt(gte(t,0.000)*lt(t,0.500)+gte(t,1.500)*lt(t,2.250),0),0.5*sin(2*PI*440*t),0)"
        );
    }
}
