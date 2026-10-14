//! Segment verification (A§9.3): before a segment enters the cache its video
//! packet count must equal the unit's frames and its decoded audio samples
//! the unit's samples. Two ffprobe runs: `-count_packets` on `v:0` (demux
//! only) and the per-frame `nb_samples` of `a:0` (decodes the PCM only).
//!
//! These argv are built here, not in `media_argv`: the core has no verify
//! argv yet (see the media owner's requested changes).

use std::path::Path;

use super::Ctx;
use super::child::{self, Spec, Stderr, Stdout};
use super::jobs::JobCtl;
use super::logs::LogFile;

/// The most ffprobe output kept (one short line per audio frame).
const STDOUT_LIMIT: usize = 64 << 20;

/// What a segment file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Counts {
    pub video_packets: u64,
    pub audio_samples: u64,
}

/// `ffprobe -v error -select_streams v:0 -count_packets -show_entries
/// stream=nb_read_packets -of csv=p=0 file:<file>`.
pub(crate) fn packets_argv(file: &Path) -> Vec<String> {
    probe_argv("v:0", &["-count_packets"], "stream=nb_read_packets", file)
}

/// `ffprobe -v error -select_streams a:0 -show_entries frame=nb_samples
/// -of csv=p=0 file:<file>`.
pub(crate) fn samples_argv(file: &Path) -> Vec<String> {
    probe_argv("a:0", &[], "frame=nb_samples", file)
}

fn probe_argv(select: &str, extra: &[&str], entries: &str, file: &Path) -> Vec<String> {
    let mut argv: Vec<String> = ["ffprobe", "-v", "error", "-select_streams", select]
        .map(String::from)
        .to_vec();
    argv.extend(extra.iter().map(|s| (*s).to_owned()));
    argv.extend(["-show_entries", entries, "-of", "csv=p=0"].map(String::from));
    argv.push(format!("file:{}", file.display()));
    argv
}

/// The first number printed (`300`), or 0 when the stream is absent.
pub(crate) fn parse_packets(stdout: &str) -> Result<u64, String> {
    match stdout.lines().map(str::trim).find(|l| !l.is_empty()) {
        None => Ok(0),
        Some(line) => line
            .trim_end_matches(',')
            .parse()
            .map_err(|_| format!("ffprobe printed {line:?} instead of a packet count")),
    }
}

/// The sum of every frame's `nb_samples`.
pub(crate) fn parse_samples(stdout: &str) -> Result<u64, String> {
    let mut total = 0u64;
    for line in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let n: u64 = line
            .trim_end_matches(',')
            .parse()
            .map_err(|_| format!("ffprobe printed {line:?} instead of a sample count"))?;
        total += n;
    }
    Ok(total)
}

/// Counts the video packets and audio samples of `file` (absolute). The
/// probes' stderr is appended to `log`.
pub(crate) fn segment_counts(
    ctx: &Ctx,
    ctl: &JobCtl,
    file: &Path,
    log: &LogFile,
) -> Result<Counts, String> {
    let bytes = std::fs::metadata(file)
        .map_err(|e| format!("can't read {}: {e}", file.display()))?
        .len();
    let run = |argv: Vec<String>| -> Result<String, String> {
        log.note(&format!("verify: {}", super::logs::shell_words(&argv)));
        let done = child::run(
            Spec::new(&argv, Stderr::Log(log.file()))
                .cwd(ctx.root())
                .stdout(Stdout::Capture {
                    limit: STDOUT_LIMIT,
                })
                .timeout(ctx.options.scaled(ctx.options.probe_timeout, bytes))
                .stop(&ctl.stop)
                .kill_grace(ctx.options.kill_grace),
        )?;
        if !done.exited_ok() {
            return Err(format!(
                "verifying the segment failed: {}",
                done.describe("ffprobe")
            ));
        }
        Ok(String::from_utf8_lossy(&done.stdout).into_owned())
    };
    let video_packets = parse_packets(&run(packets_argv(file))?)?;
    let audio_samples = parse_samples(&run(samples_argv(file))?)?;
    Ok(Counts {
        video_packets,
        audio_samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_shapes() {
        let file = Path::new("/p/.qrec/cache/segments/.k.qrec-part-1-2.mov");
        assert_eq!(
            packets_argv(file).join(" "),
            "ffprobe -v error -select_streams v:0 -count_packets -show_entries \
             stream=nb_read_packets -of csv=p=0 file:/p/.qrec/cache/segments/.k.qrec-part-1-2.mov"
        );
        assert_eq!(
            samples_argv(file).join(" "),
            "ffprobe -v error -select_streams a:0 -show_entries frame=nb_samples -of csv=p=0 \
             file:/p/.qrec/cache/segments/.k.qrec-part-1-2.mov"
        );
    }

    #[test]
    fn parses_counts() {
        assert_eq!(parse_packets("300\n"), Ok(300));
        assert_eq!(parse_packets("\n"), Ok(0));
        assert_eq!(parse_packets("12,\n"), Ok(12));
        assert!(parse_packets("N/A\n").is_err());
        assert_eq!(parse_samples("1024\n1024\n352\n"), Ok(2400));
        assert_eq!(parse_samples(""), Ok(0));
        assert!(parse_samples("1024\nx\n").is_err());
    }
}
