//! Every ffmpeg/ffprobe command line is built here or from these helpers
//! (A§9.2, SPEC §3.1). Owner: output.
//!
//! The contract: `argv[0]` is `ffmpeg` or `ffprobe`; ffmpeg argv start with
//! [`ffmpeg_prelude`]; the output path is the last element, preceded by an
//! explicit muxer (`-f …`); every path is prefixed with `file:`. The runner
//! may replace only the last element (with its temp path).
//!
//! The export and frame-save planners (`export_plan`, `frame_plan`) build
//! their filter graphs with the helpers below; the probe, segment-check,
//! silence and VA-check argv are complete here. Verified against ffmpeg
//! 8.1.2 (`crates/qrec-shell/tests/plans_ffmpeg.rs` runs them).
//!
//! The GUI's previews (docs/GUI.md G§3.11) are here too: the thumbnail
//! strip, recorded-frame and waveform argv (raw RGBA or s16le on stdout,
//! `pipe:1`), the cache key, and the waveform's peak buckets
//! ([`bucket_peaks`], [`peak_levels`]). `qrec_shell::media::preview` runs
//! them (owner: media).

use std::sync::Arc;

use crate::export_plan::SEEK_BACKOFF_NS;
use crate::hash::sha256_hex;
use crate::model::StreamKind;
use crate::time::NS_PER_SEC;

/// `file:` + path, so names starting with `-` or containing `:` stay paths.
pub fn file_arg(path: &str) -> String {
    format!("file:{path}")
}

/// `ffmpeg -hide_banner -nostdin -nostats -loglevel info -y`, plus
/// `-progress pipe:1` for jobs that report progress.
pub fn ffmpeg_prelude(progress: bool) -> Vec<String> {
    let mut argv = Vec::from(
        [
            "ffmpeg",
            "-hide_banner",
            "-nostdin",
            "-nostats",
            "-loglevel",
            "info",
            "-y",
        ]
        .map(String::from),
    );
    if progress {
        argv.extend(["-progress", "pipe:1"].map(String::from));
    }
    argv
}

/// The stream fields every probe asks for. `duration` (stream), the
/// Matroska `DURATION` tag and the format duration are fallbacks for each
/// other: Matroska has no stream duration, and a file cut short by a crash
/// has neither tag nor format duration. Then the length comes from the
/// packets `-count_packets` counts (A§2.2): video frames, or for audio
/// (`codec_name` FLAC) `capture_plan::FLAC_BLOCK_SAMPLES` per packet.
const PROBE_STREAM_ENTRIES: &str =
    "nb_read_packets,r_frame_rate,width,height,pix_fmt,color_range,color_space";

/// `ffprobe -v error -select_streams v:0 -count_packets -show_entries
/// stream=nb_read_packets,r_frame_rate,width,height,pix_fmt,color_range,color_space,duration:stream_tags=DURATION:format=start_time,duration
/// -of json file:<f>`; audio selects `a:0` and adds
/// `codec_name,sample_rate,channels`. `parse::ffprobe::parse_ffprobe` reads
/// the output.
pub fn probe_argv(file: &str, kind: StreamKind) -> Vec<String> {
    let (select, stream_entries) = match kind {
        StreamKind::Video => ("v:0", format!("{PROBE_STREAM_ENTRIES},duration")),
        StreamKind::Audio => (
            "a:0",
            format!("{PROBE_STREAM_ENTRIES},codec_name,sample_rate,channels,duration"),
        ),
    };
    let mut argv = Vec::from(
        [
            "ffprobe",
            "-v",
            "error",
            "-select_streams",
            select,
            "-count_packets",
            "-show_entries",
        ]
        .map(String::from),
    );
    argv.extend([
        format!("stream={stream_entries}:stream_tags=DURATION:format=start_time,duration"),
        "-of".to_owned(),
        "json".to_owned(),
        file_arg(file),
    ]);
    argv
}

/// `ffprobe -v error -show_entries packet=codec_type,size -of csv=p=0
/// file:<segment>`: one `video,<bytes>` or `audio,<bytes>` line per packet,
/// without decoding anything. The shell checks every rendered segment with
/// it before caching it (A§9.3): the video packets must be the unit's frames
/// and the PCM audio (stereo `s16le`, 4 bytes per sample) its samples.
/// `parse::ffprobe::parse_segment_check` reads the output.
pub fn segment_check_argv(path: &str) -> Vec<String> {
    let mut argv = Vec::from(
        [
            "ffprobe",
            "-v",
            "error",
            "-show_entries",
            "packet=codec_type,size",
            "-of",
            "csv=p=0",
        ]
        .map(String::from),
    );
    argv.push(file_arg(path));
    argv
}

/// `ffmpeg … -i file:<f> -af silencedetect=noise=<t>dB:d=<s> -vn -f null -`.
/// `parse::silencedetect` turns its stderr into intervals.
pub fn silence_argv(file: &str, threshold_db: f64, min_silence_ms: u32) -> Vec<String> {
    let mut argv = ffmpeg_prelude(false);
    let seconds = f64::from(min_silence_ms) / 1000.0;
    argv.extend([
        "-i".to_owned(),
        file_arg(file),
        "-af".to_owned(),
        format!("silencedetect=noise={}dB:d={seconds}", number(threshold_db)),
        "-vn".to_owned(),
        "-f".to_owned(),
        "null".to_owned(),
        "-".to_owned(),
    ]);
    argv
}

/// Nanoseconds as the seconds ffmpeg's time options take (`-ss 1.233333`):
/// six decimals, rounded to the nearest microsecond (ffmpeg ignores digits
/// past the sixth).
pub fn seconds_arg(ns: i64) -> String {
    let us = (i128::from(ns) * 2 + if ns < 0 { -1000 } else { 1000 }) / 2000;
    let sign = if us < 0 { "-" } else { "" };
    let us = us.unsigned_abs();
    format!("{sign}{}.{:06}", us / 1_000_000, us % 1_000_000)
}

/// A float the way filter options take it: the shortest decimal that
/// round-trips (`0.8`, `-6`), never exponent notation, `-0` printed as `0`.
pub fn number(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else {
        format!("{value}")
    }
}

/// `-f <muxer> file:<path>`: the explicit muxer and the output path, which
/// is always the last element.
pub fn output_args(muxer: &str, path: &str) -> Vec<String> {
    vec!["-f".to_owned(), muxer.to_owned(), file_arg(path)]
}

/// `-c:v png -f image2 -update 1 file:<path>`: one PNG (no `%d` pattern).
/// The encoder is named: image2 would pick it from the file's extension,
/// and the runner hands ffmpeg its temp output as `/proc/self/fd/<n>`,
/// which has none (A§8.8).
pub fn png_output_args(path: &str) -> Vec<String> {
    vec![
        "-c:v".to_owned(),
        "png".to_owned(),
        "-f".to_owned(),
        "image2".to_owned(),
        "-update".to_owned(),
        "1".to_owned(),
        file_arg(path),
    ]
}

/// The output colour options of every export segment.
///
/// ffmpeg 8.1.2 uses `-color_range`/`-colorspace` to negotiate the graph (so
/// the `color` background is drawn with BT.709 coefficients) but takes the
/// primaries and transfer from the frames, so the graph also tags them with
/// [`TAG_BT709`] *(measured)*.
pub fn color_args() -> Vec<String> {
    [
        "-color_range",
        "tv",
        "-colorspace",
        "bt709",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
    ]
    .map(String::from)
    .to_vec()
}

/// Tags the composited frames' primaries and transfer (see [`color_args`]).
/// Tagging range or matrix here too would break the negotiation and draw the
/// background with BT.601 coefficients *(measured)*.
pub const TAG_BT709: &str = "setparams=color_primaries=bt709:color_trc=bt709";

/// Turns the limited-range BT.709 composite into an sRGB PNG frame. The
/// primaries tag makes ffmpeg write `sRGB`/`cHRM`/`gAMA` chunks; without it
/// the PNG gets a `cICP` chunk with unspecified primaries *(measured)*.
pub const TO_SRGB_PNG: &str = "scale=out_color_matrix=bt709:out_range=pc,format=rgb24,setparams=color_primaries=bt709:color_trc=iec61966-2-1";

/// The global options that bind ffmpeg to one VA-API render node
/// (`-init_hw_device vaapi=va:<device> -filter_hw_device va`). Never let
/// VA-API pick a device on its own (A§8.10).
pub fn vaapi_device_args(device: &str) -> Vec<String> {
    vec![
        "-init_hw_device".to_owned(),
        format!("vaapi=va:{device}"),
        "-filter_hw_device".to_owned(),
        "va".to_owned(),
    ]
}

/// Uploads the composited `[v]` to the VA device: `[v]format=nv12,hwupload[vv]`.
pub const VAAPI_UPLOAD: &str = "format=nv12,hwupload";

/// `-c:v h264_vaapi -rc_mode CQP -qp <qp> -bf 0 -profile:v high`. `-pix_fmt
/// yuv420p` is left out: ffmpeg 8.1.2 warns it is incompatible and picks
/// `vaapi` anyway *(measured)*. No B-frames, like every H.264 segment
/// (`export_plan`, "Decode order").
pub fn vaapi_h264_args(qp: u32) -> Vec<String> {
    vec![
        "-c:v".to_owned(),
        "h264_vaapi".to_owned(),
        "-rc_mode".to_owned(),
        "CQP".to_owned(),
        "-qp".to_owned(),
        qp.to_string(),
        "-bf".to_owned(),
        "0".to_owned(),
        "-profile:v".to_owned(),
        "high".to_owned(),
    ]
}

/// The check behind `VaapiCaps::ffmpeg_h264`: the exact VA export chain
/// ([`vaapi_device_args`], [`VAAPI_UPLOAD`], [`vaapi_h264_args`]) encoding
/// two frames of a generated picture to the null muxer. Run it with
/// `LIBVA_DRIVER_NAME` set for the device's vendor and a 10 s timeout (A§8.7).
pub fn vaapi_check_argv(device: &str) -> Vec<String> {
    let mut argv = ffmpeg_prelude(false);
    argv.extend(vaapi_device_args(device));
    argv.extend([
        "-f".to_owned(),
        "lavfi".to_owned(),
        "-i".to_owned(),
        "color=c=0x000000:s=256x144:r=30/1".to_owned(),
        "-filter_complex".to_owned(),
        format!("[0:v]trim=end_frame=2,format=yuv420p,{TAG_BT709}[v];[v]{VAAPI_UPLOAD}[vv]"),
        "-map".to_owned(),
        "[vv]".to_owned(),
    ]);
    argv.extend(vaapi_h264_args(22));
    argv.extend(color_args());
    argv.extend(["-f", "null", "-"].map(String::from));
    argv
}

// ----------------------------------------------------------- GUI previews

/// Seconds of video one thumbnail strip chunk covers (G§3.11): image `i`
/// of chunk `c` shows file time `60·c + i` s.
pub const STRIP_CHUNK_SECS: u32 = 60;

/// The most recorded frames one [`frames_argv`] run returns (frame zoom).
pub const MAX_PREVIEW_FRAMES: u32 = 64;

/// The waveform's sample rate: [`peaks_argv`] decodes to it, mono.
pub const PEAK_SAMPLE_RATE: u32 = 48_000;

/// Samples of [`PEAK_SAMPLE_RATE`] in one millisecond: one finest bucket.
pub const PEAK_SAMPLES_PER_MS: usize = 48;

/// Finer buckets in one coarser bucket: 1 → 16 → 256 ms.
pub const PEAK_LEVEL_STEP: usize = 16;

/// Bumped when a preview cache file's content changes for the same input
/// (an argv or format change), so older files stop matching.
pub const PREVIEW_CACHE_VERSION: u32 = 1;

/// What a [`preview_key`] is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PreviewKind {
    /// One 60 s chunk of one-per-second thumbnails.
    Strip,
    /// Consecutive recorded frames (frame zoom; memory only).
    Frames,
    /// A waveform's 1 ms peaks.
    Peaks,
}

impl PreviewKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewKind::Strip => "strip",
            PreviewKind::Frames => "frames",
            PreviewKind::Peaks => "peaks",
        }
    }
}

/// `ffmpeg … -ss <60·chunk> -t 60 -skip_frame nokey -i file:<f> -an -vf
/// fps=1,scale=-2:<h> -f rawvideo -pix_fmt rgba pipe:1`: one RGBA image per
/// second of chunk `chunk`, `height` px high ([`preview_width`] wide),
/// image after image on stdout. Only keyframes are decoded (one per second
/// in recordings, A§8.11), so image `i` is the keyframe nearest to file
/// time `60·chunk + i` s. A chunk that reaches the end of the file has
/// fewer images (one less than its whole seconds, or as many); one past it
/// has none.
pub fn strip_argv(file: &str, chunk: u32, height: u32) -> Vec<String> {
    let mut argv = ffmpeg_prelude(false);
    let start_ns = i64::from(chunk)
        .saturating_mul(i64::from(STRIP_CHUNK_SECS))
        .saturating_mul(NS_PER_SEC);
    argv.extend([
        "-ss".to_owned(),
        seconds_arg(start_ns),
        "-t".to_owned(),
        STRIP_CHUNK_SECS.to_string(),
        "-skip_frame".to_owned(),
        "nokey".to_owned(),
        "-i".to_owned(),
        file_arg(file),
        "-an".to_owned(),
        "-vf".to_owned(),
        format!("fps=1,scale=-2:{height}"),
    ]);
    argv.extend(raw_rgba_output());
    argv
}

/// `ffmpeg … [-ss <start − 2 ms>] -i file:<f> -an -frames:v <count> -vf
/// scale=-2:<h> -f rawvideo -pix_fmt rgba pipe:1`: `count` (at most
/// [`MAX_PREVIEW_FRAMES`]) consecutive recorded frames from the one that
/// starts at `start_ns` in the file, as RGBA images on stdout (fewer at the
/// end of the file). The seek backs off 2 ms like a layer input's
/// (`export_plan::SEEK_BACKOFF_NS`: Matroska rounds timestamps to the
/// millisecond) and is left out when that is ≤ 0. Every decoded frame is
/// one image (the rawvideo muxer keeps no timestamps, so nothing is
/// duplicated or dropped). Timeline frames map to recorded ones with
/// `export_plan::shown_frame` (A§9.3).
pub fn frames_argv(file: &str, start_ns: i64, count: u32, height: u32) -> Vec<String> {
    let mut argv = ffmpeg_prelude(false);
    let seek = start_ns.saturating_sub(SEEK_BACKOFF_NS);
    if seek > 0 {
        argv.extend(["-ss".to_owned(), seconds_arg(seek)]);
    }
    argv.extend([
        "-i".to_owned(),
        file_arg(file),
        "-an".to_owned(),
        "-frames:v".to_owned(),
        count.min(MAX_PREVIEW_FRAMES).to_string(),
        "-vf".to_owned(),
        format!("scale=-2:{height}"),
    ]);
    argv.extend(raw_rgba_output());
    argv
}

/// `-f rawvideo -pix_fmt rgba pipe:1`.
fn raw_rgba_output() -> [String; 5] {
    ["-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"].map(String::from)
}

/// `ffmpeg … -i file:<f> -map 0:a:0 -ac 1 -ar 48000 -f s16le pipe:1`: the
/// first audio stream, mono at [`PEAK_SAMPLE_RATE`], as 16-bit samples on
/// stdout ([`bucket_peaks`] turns them into the waveform). 96 bytes are one
/// millisecond.
pub fn peaks_argv(file: &str) -> Vec<String> {
    let mut argv = ffmpeg_prelude(false);
    argv.extend([
        "-i".to_owned(),
        file_arg(file),
        "-map".to_owned(),
        "0:a:0".to_owned(),
        "-ac".to_owned(),
        "1".to_owned(),
        "-ar".to_owned(),
        PEAK_SAMPLE_RATE.to_string(),
        "-f".to_owned(),
        "s16le".to_owned(),
        "pipe:1".to_owned(),
    ]);
    argv
}

/// `ffprobe -v error -select_streams v:0 -show_entries
/// stream=width,height,r_frame_rate -of json file:<f>`: what a strip or a
/// frames run needs from a video before it starts (the image width, and
/// the recorded rate a frame's start comes from), read from the header
/// alone: unlike [`probe_argv`], it counts no packets, so it is quick on an
/// hour-long file. `parse::ffprobe::parse_ffprobe(_, Video)` reads it.
pub fn preview_probe_argv(file: &str) -> Vec<String> {
    let mut argv = Vec::from(
        [
            "ffprobe",
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate",
            "-of",
            "json",
        ]
        .map(String::from),
    );
    argv.push(file_arg(file));
    argv
}

/// The width `scale=-2:<height>` gives a `src_w`×`src_h` picture: ffmpeg's
/// `av_rescale(height, src_w, 2·src_h) · 2` (to the nearest, halves away
/// from zero), at least 2. The source's storage size counts, not its
/// display aspect, as in ffmpeg.
pub fn preview_width(src_w: u32, src_h: u32, height: u32) -> u32 {
    if src_h == 0 {
        return 2;
    }
    let (w, h, out) = (u64::from(src_w), u64::from(src_h), u64::from(height));
    let half = (out * w + h) / (2 * h);
    let width = (half * 2).clamp(2, u64::from(u32::MAX - 1));
    u32::try_from(width).unwrap_or(2)
}

/// The cache key of one preview of `file` (G§3.11): the SHA-256 hex of
/// `qrec preview v<version>\n<kind>\n<file>\n<size>\n<mtime_ns>\n<params>`,
/// so it changes with the file (like `SilenceKey`), the kind and the
/// parameters (`params`: e.g. `chunk=3 height=72`). A plain file name.
pub fn preview_key(
    kind: PreviewKind,
    file: &str,
    size: u64,
    mtime_ns: i64,
    params: &str,
) -> String {
    let text = format!(
        "qrec preview v{PREVIEW_CACHE_VERSION}\n{}\n{file}\n{size}\n{mtime_ns}\n{params}",
        kind.as_str()
    );
    sha256_hex(text.as_bytes())
}

/// (min, max) of each run of `per_bucket` samples (the last run may be
/// shorter); `per_bucket` 0 counts as 1. With [`PEAK_SAMPLES_PER_MS`] at
/// [`PEAK_SAMPLE_RATE`], one bucket per millisecond. Buckets of a stream
/// cut into runs of whole buckets are the buckets of the whole stream, so
/// a reader may bucket as samples arrive.
pub fn bucket_peaks(samples: &[i16], per_bucket: usize) -> Vec<(i16, i16)> {
    samples
        .chunks(per_bucket.max(1))
        .map(|run| {
            run.iter()
                .fold((i16::MAX, i16::MIN), |(lo, hi), &s| (lo.min(s), hi.max(s)))
        })
        .collect()
}

/// A waveform at three resolutions: (min, max) per 1, 16 and 256 ms, so a
/// lane draws from the coarsest level with at least one bucket per pixel
/// ([`PeakLevels::level`]). Each coarser bucket is the min and the max of
/// its 16 finer ones (the last of fewer).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PeakLevels {
    pub per_ms: Arc<[(i16, i16)]>,
    pub per_16ms: Arc<[(i16, i16)]>,
    pub per_256ms: Arc<[(i16, i16)]>,
}

/// The lengths, not the millions of buckets of an hour-long take.
impl std::fmt::Debug for PeakLevels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeakLevels")
            .field("per_ms", &self.per_ms.len())
            .field("per_16ms", &self.per_16ms.len())
            .field("per_256ms", &self.per_256ms.len())
            .finish()
    }
}

impl PeakLevels {
    /// The coarsest level with at least one bucket per pixel at `ms_per_px`
    /// milliseconds per pixel (the finest below 16 ms/px), and its bucket
    /// length in ms.
    pub fn level(&self, ms_per_px: f64) -> (u32, &[(i16, i16)]) {
        let step = PEAK_LEVEL_STEP as f64;
        if ms_per_px >= step * step {
            (256, &self.per_256ms)
        } else if ms_per_px >= step {
            (16, &self.per_16ms)
        } else {
            (1, &self.per_ms)
        }
    }

    /// The waveform's length in ms (its finest buckets).
    pub fn duration_ms(&self) -> usize {
        self.per_ms.len()
    }

    /// The bytes the three levels hold.
    pub fn bytes(&self) -> usize {
        (self.per_ms.len() + self.per_16ms.len() + self.per_256ms.len())
            * std::mem::size_of::<(i16, i16)>()
    }
}

/// The three levels of a waveform from its 1 ms buckets ([`bucket_peaks`]).
pub fn peak_levels(per_ms: Arc<[(i16, i16)]>) -> PeakLevels {
    let per_16ms: Arc<[(i16, i16)]> = coarser(&per_ms, PEAK_LEVEL_STEP).into();
    let per_256ms: Arc<[(i16, i16)]> = coarser(&per_16ms, PEAK_LEVEL_STEP).into();
    PeakLevels {
        per_ms,
        per_16ms,
        per_256ms,
    }
}

/// (min of the mins, max of the maxes) of each run of `step` buckets.
fn coarser(finer: &[(i16, i16)], step: usize) -> Vec<(i16, i16)> {
    finer
        .chunks(step)
        .map(|run| {
            run.iter().fold((i16::MAX, i16::MIN), |(lo, hi), &(a, b)| {
                (lo.min(a), hi.max(b))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Fps;
    use crate::time::frame_ns;

    #[test]
    fn strip_argv_reads_keyframes_of_one_chunk() {
        let argv = strip_argv("takes/take-0005/L2-camera-fake0.mkv", 3, 72);
        assert_eq!(argv[0], "ffmpeg");
        assert_eq!(&argv[..7], &ffmpeg_prelude(false)[..]);
        let at = |flag: &str| argv.iter().position(|a| a == flag).unwrap();
        assert_eq!(argv[at("-ss") + 1], "180.000000");
        assert_eq!(argv[at("-t") + 1], "60");
        assert!(
            at("-skip_frame") < at("-i") && at("-ss") < at("-i"),
            "input options: {argv:?}"
        );
        assert_eq!(
            argv[at("-i") + 1],
            "file:takes/take-0005/L2-camera-fake0.mkv"
        );
        assert_eq!(argv[at("-vf") + 1], "fps=1,scale=-2:72");
        assert_eq!(&argv[argv.len() - 5..], raw_rgba_output());
        // An absurd chunk saturates instead of overflowing.
        let far = strip_argv("a.mkv", u32::MAX, 72);
        assert_eq!(
            far[far.iter().position(|a| a == "-ss").unwrap() + 1],
            "9223372036.854776"
        );
        insta::assert_snapshot!(
            "strip_argv",
            format!(
                "{}\n{}",
                strip_argv("takes/take-0005/L2-camera-fake0.mkv", 0, 72).join(" "),
                strip_argv("takes/take-0005/L2-camera-fake0.mkv", 3, 144).join(" ")
            )
        );
    }

    #[test]
    fn frames_argv_seeks_just_before_the_frame() {
        let fps = Fps::new(30, 1);
        let argv = frames_argv(
            "takes/take-0005/L1-screen-FAKE-1.mkv",
            frame_ns(1000, fps),
            64,
            72,
        );
        let at = |flag: &str| argv.iter().position(|a| a == flag).unwrap();
        // 1000 / 30 s = 33.333333 s, less 2 ms.
        assert_eq!(argv[at("-ss") + 1], "33.331333");
        assert!(at("-ss") < at("-i"), "an input seek: {argv:?}");
        assert_eq!(argv[at("-frames:v") + 1], "64");
        assert_eq!(argv[at("-vf") + 1], "scale=-2:72");
        assert_eq!(argv.last().unwrap(), "pipe:1");
        // The first frames: no seek at all.
        for start in [0, frame_ns(0, fps), SEEK_BACKOFF_NS, i64::MIN] {
            let argv = frames_argv("a.mkv", start, 8, 72);
            assert!(!argv.contains(&"-ss".to_owned()), "{argv:?}");
        }
        // At most 64 frames.
        let argv = frames_argv("a.mkv", 0, 500, 72);
        assert_eq!(
            argv[argv.iter().position(|a| a == "-frames:v").unwrap() + 1],
            "64"
        );
        insta::assert_snapshot!(
            "frames_argv",
            format!(
                "{}\n{}",
                frames_argv(
                    "takes/take-0005/L1-screen-FAKE-1.mkv",
                    frame_ns(1000, fps),
                    64,
                    72
                )
                .join(" "),
                frames_argv("takes/take-0005/L1-screen-FAKE-1.mkv", 0, 10, 144).join(" ")
            )
        );
    }

    #[test]
    fn peaks_and_probe_argv() {
        let argv = peaks_argv("takes/take-0005/A1-mic.mka");
        assert_eq!(&argv[..7], &ffmpeg_prelude(false)[..]);
        assert_eq!(&argv[argv.len() - 3..], ["-f", "s16le", "pipe:1"]);
        assert!(argv.contains(&"file:takes/take-0005/A1-mic.mka".to_owned()));
        let probe = preview_probe_argv("takes/take-0005/L1-screen-FAKE-1.mkv");
        assert_eq!(probe[0], "ffprobe");
        assert!(!probe.contains(&"-count_packets".to_owned()), "header only");
        assert_eq!(
            probe.last().unwrap(),
            "file:takes/take-0005/L1-screen-FAKE-1.mkv"
        );
        insta::assert_snapshot!(
            "peaks_and_probe_argv",
            format!("{}\n{}", argv.join(" "), probe.join(" "))
        );
    }

    #[test]
    fn preview_width_is_ffmpegs_even_width() {
        // (source, height) → what `scale=-2:<h>` gives (ffmpeg 8.1.2;
        // `media::preview` tests check a few against ffmpeg itself).
        let cases = [
            ((1920, 1080, 72), 128),
            ((2560, 1440, 72), 128),
            ((3840, 2160, 72), 128),
            ((640, 360, 72), 128),
            ((320, 240, 72), 96),
            ((160, 120, 72), 96),
            ((1366, 768, 72), 128),
            ((720, 480, 72), 108),
            ((1920, 1080, 144), 256),
            ((1920, 1080, 90), 160),
            ((1080, 1920, 72), 40),
            ((1, 1000, 72), 2),
            ((5, 3, 0), 2),
        ];
        for ((w, h, out), expected) in cases {
            assert_eq!(preview_width(w, h, out), expected, "{w}x{h} at {out}");
        }
        assert_eq!(preview_width(1920, 0, 72), 2);
    }

    #[test]
    fn preview_keys_follow_the_file_kind_and_parameters() {
        let key = |kind, file: &str, size, mtime, params: &str| {
            preview_key(kind, file, size, mtime, params)
        };
        let a = key(
            PreviewKind::Strip,
            "takes/take-0001/L1.mkv",
            10,
            20,
            "chunk=0 height=72",
        );
        assert_eq!(
            a,
            key(
                PreviewKind::Strip,
                "takes/take-0001/L1.mkv",
                10,
                20,
                "chunk=0 height=72"
            )
        );
        assert_eq!(a.len(), 64);
        assert!(
            a.bytes().all(|b| b.is_ascii_hexdigit()),
            "a plain file name"
        );
        for other in [
            key(
                PreviewKind::Peaks,
                "takes/take-0001/L1.mkv",
                10,
                20,
                "chunk=0 height=72",
            ),
            key(
                PreviewKind::Strip,
                "takes/take-0002/L1.mkv",
                10,
                20,
                "chunk=0 height=72",
            ),
            key(
                PreviewKind::Strip,
                "takes/take-0001/L1.mkv",
                11,
                20,
                "chunk=0 height=72",
            ),
            key(
                PreviewKind::Strip,
                "takes/take-0001/L1.mkv",
                10,
                21,
                "chunk=0 height=72",
            ),
            key(
                PreviewKind::Strip,
                "takes/take-0001/L1.mkv",
                10,
                20,
                "chunk=1 height=72",
            ),
        ] {
            assert_ne!(a, other);
        }
        insta::assert_snapshot!(
            "preview_keys",
            format!(
                "{a}\n{}",
                key(
                    PreviewKind::Peaks,
                    "takes/take-0001/A1-mic.mka",
                    4096,
                    1_760_000_000_123_456_789,
                    ""
                )
            )
        );
    }

    #[test]
    fn peaks_bucket_per_millisecond() {
        assert!(bucket_peaks(&[], PEAK_SAMPLES_PER_MS).is_empty());
        let samples: Vec<i16> = (0..100).map(|i| (i * 300 - 15_000) as i16).collect();
        let buckets = bucket_peaks(&samples, PEAK_SAMPLES_PER_MS);
        // 100 samples: two whole milliseconds and a partial one.
        assert_eq!(buckets, [(-15_000, -900), (-600, 13_500), (13_800, 14_700)]);
        assert_eq!(
            bucket_peaks(&[5, -3], 0),
            [(5, 5), (-3, -3)],
            "0 counts as 1"
        );
        assert_eq!(
            bucket_peaks(&[i16::MIN, i16::MAX], 48),
            [(i16::MIN, i16::MAX)]
        );
    }

    #[test]
    fn peak_levels_merge_sixteen_at_a_time() {
        let per_ms: Vec<(i16, i16)> = (0..300).map(|i| (-(i as i16), i as i16 * 2)).collect();
        let levels = peak_levels(per_ms.clone().into());
        assert_eq!(levels.per_ms.len(), 300);
        assert_eq!(levels.per_16ms.len(), 19, "⌈300 / 16⌉");
        assert_eq!(levels.per_256ms.len(), 2, "⌈300 / 256⌉");
        assert_eq!(levels.per_16ms[0], (-15, 30));
        assert_eq!(levels.per_16ms[18], (-299, 598), "the last of 12");
        assert_eq!(levels.per_256ms[0], (-255, 510));
        assert_eq!(levels.per_256ms[1], (-299, 598));
        assert_eq!(levels.duration_ms(), 300);
        assert_eq!(levels.bytes(), (300 + 19 + 2) * 4);
        assert_eq!(peak_levels(Vec::new().into()), PeakLevels::default());

        // A lane takes the coarsest level with a bucket per pixel.
        assert_eq!(levels.level(0.25).0, 1);
        assert_eq!(levels.level(15.9).0, 1);
        assert_eq!(levels.level(16.0).0, 16);
        assert_eq!(levels.level(255.0).1.len(), 19);
        assert_eq!(levels.level(256.0).0, 256);
        assert_eq!(levels.level(10_000.0).1.len(), 2);
    }

    #[test]
    fn silence_argv_shape() {
        let argv = silence_argv("takes/take-0001/A1-mic.mka", -35.0, 400);
        assert_eq!(argv[0], "ffmpeg");
        assert!(
            argv.contains(&"silencedetect=noise=-35dB:d=0.4".to_owned()),
            "{argv:?}"
        );
        assert_eq!(argv.last().unwrap(), "-");
        assert!(argv.contains(&"file:takes/take-0001/A1-mic.mka".to_owned()));
        let argv = silence_argv("a.mka", -42.5, 1250);
        assert!(argv.contains(&"silencedetect=noise=-42.5dB:d=1.25".to_owned()));
    }

    #[test]
    fn probe_argv_shape() {
        let argv = probe_argv("takes/take-0001/L1-screen-DP-4.mkv", StreamKind::Video);
        assert_eq!(argv[0], "ffprobe");
        assert_eq!(
            argv.last().unwrap(),
            "file:takes/take-0001/L1-screen-DP-4.mkv"
        );
        assert!(argv.contains(&"-count_packets".to_owned()));
        assert!(argv.contains(&"v:0".to_owned()));
        assert!(
            probe_argv("a.mka", StreamKind::Audio)
                .iter()
                .any(|a| a.contains("codec_name,sample_rate,channels,duration"))
        );
        insta::assert_snapshot!(
            "probe_argv",
            format!(
                "{}\n{}",
                probe_argv("takes/take-0001/L1-screen-DP-4.mkv", StreamKind::Video).join(" "),
                probe_argv("takes/take-0001/A1-mic.mka", StreamKind::Audio).join(" ")
            )
        );
    }

    #[test]
    fn segment_check_argv_shape() {
        let argv = segment_check_argv(".qrec/cache/segments/abc.mov");
        assert_eq!(
            argv.join(" "),
            "ffprobe -v error -show_entries packet=codec_type,size -of csv=p=0 file:.qrec/cache/segments/abc.mov"
        );
    }

    #[test]
    fn silence_argv_snapshot() {
        insta::assert_snapshot!(
            "silence_argv",
            silence_argv("takes/take-0001/A1-mic.mka", -35.0, 400).join(" ")
        );
    }

    #[test]
    fn seconds_round_to_microseconds() {
        assert_eq!(seconds_arg(0), "0.000000");
        assert_eq!(seconds_arg(1_233_333_333), "1.233333");
        assert_eq!(seconds_arg(1_231_333_333), "1.231333");
        assert_eq!(seconds_arg(66_666_666), "0.066667");
        assert_eq!(seconds_arg(499), "0.000000");
        assert_eq!(seconds_arg(500), "0.000001");
        assert_eq!(seconds_arg(3_600_000_000_000), "3600.000000");
        assert_eq!(seconds_arg(-20_000_000), "-0.020000");
        assert_eq!(seconds_arg(-1_500), "-0.000002");
    }

    #[test]
    fn numbers_never_use_exponents() {
        assert_eq!(number(0.8), "0.8");
        assert_eq!(number(-6.0), "-6");
        assert_eq!(number(-0.0), "0");
        assert_eq!(number(1e-7), "0.0000001");
        assert_eq!(number(12.25), "12.25");
    }

    #[test]
    fn outputs_end_with_the_path() {
        assert_eq!(output_args("mov", "a b.mov"), ["-f", "mov", "file:a b.mov"]);
        assert_eq!(
            png_output_args("frames/x.png"),
            [
                "-c:v",
                "png",
                "-f",
                "image2",
                "-update",
                "1",
                "file:frames/x.png"
            ],
            "the encoder is named, never guessed from the output's name"
        );
        let check = vaapi_check_argv("/dev/dri/renderD129");
        assert_eq!(check[0], "ffmpeg");
        assert!(check.contains(&"vaapi=va:/dev/dri/renderD129".to_owned()));
        assert_eq!(&check[check.len() - 3..], ["-f", "null", "-"]);
        insta::assert_snapshot!("vaapi_check_argv", check.join(" "));
    }
}
