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

use crate::model::StreamKind;

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

/// `-f image2 -update 1 file:<path>`: one PNG (no `%d` pattern).
pub fn png_output_args(path: &str) -> Vec<String> {
    vec![
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

/// `-c:v h264_vaapi -rc_mode CQP -qp <qp> -profile:v high`. `-pix_fmt
/// yuv420p` is left out: ffmpeg 8.1.2 warns it is incompatible and picks
/// `vaapi` anyway *(measured)*.
pub fn vaapi_h264_args(qp: u32) -> Vec<String> {
    vec![
        "-c:v".to_owned(),
        "h264_vaapi".to_owned(),
        "-rc_mode".to_owned(),
        "CQP".to_owned(),
        "-qp".to_owned(),
        qp.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
            png_output_args("frames/x.png").last().unwrap(),
            "file:frames/x.png"
        );
        let check = vaapi_check_argv("/dev/dri/renderD129");
        assert_eq!(check[0], "ffmpeg");
        assert!(check.contains(&"vaapi=va:/dev/dri/renderD129".to_owned()));
        assert_eq!(&check[check.len() - 3..], ["-f", "null", "-"]);
        insta::assert_snapshot!("vaapi_check_argv", check.join(" "));
    }
}
