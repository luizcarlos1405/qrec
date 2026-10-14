//! Every ffmpeg/ffprobe command line is built here (A§9.2, SPEC §3.1).
//! Owner: output.
//!
//! The contract: `argv[0]` is `ffmpeg` or `ffprobe`; ffmpeg argv start with
//! [`ffmpeg_prelude`]; the output path is the last element, preceded by an
//! explicit muxer (`-f …`); every path is prefixed with `file:`. The runner
//! may replace only the last element (with its temp path).
//!
//! The foundation drafted `probe_argv` and `silence_argv` from A§9.2 so the
//! record and autocut owners can emit effects; the output owner owns and may
//! refine them (verified against ffmpeg 8.1).

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

/// `ffprobe -v error -select_streams v:0 -count_packets -show_entries
/// stream=nb_read_packets,r_frame_rate,width,height,pix_fmt,color_range,color_space:format=start_time
/// -of json file:<f>`; audio selects `a:0` and adds `sample_rate,channels,duration`.
pub fn probe_argv(file: &str, kind: StreamKind) -> Vec<String> {
    let video_entries = "nb_read_packets,r_frame_rate,width,height,pix_fmt,color_range,color_space";
    let (select, entries) = match kind {
        StreamKind::Video => ("v:0", format!("stream={video_entries}:format=start_time")),
        StreamKind::Audio => (
            "a:0",
            format!("stream={video_entries},sample_rate,channels,duration:format=start_time"),
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
    argv.extend([entries, "-of".to_owned(), "json".to_owned(), file_arg(file)]);
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
        format!("silencedetect=noise={threshold_db}dB:d={seconds}"),
        "-vn".to_owned(),
        "-f".to_owned(),
        "null".to_owned(),
        "-".to_owned(),
    ]);
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
    }

    #[test]
    fn probe_argv_shape() {
        let argv = probe_argv("takes/take-0001/L1-screen-DP-4.mkv", StreamKind::Video);
        assert_eq!(argv[0], "ffprobe");
        assert_eq!(
            argv.last().unwrap(),
            "file:takes/take-0001/L1-screen-DP-4.mkv"
        );
        assert!(
            probe_argv("a.mka", StreamKind::Audio)
                .iter()
                .any(|a| a.contains("sample_rate,channels,duration"))
        );
    }
}
