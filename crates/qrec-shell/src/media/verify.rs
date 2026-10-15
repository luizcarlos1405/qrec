//! Segment verification (A§9.3): before a segment enters the cache its video
//! packet count must equal the unit's frames and its audio the unit's
//! samples. The core builds the ffprobe argv (`media_argv::segment_check_argv`:
//! one `codec_type,size` line per packet, nothing decoded) and parses it
//! (`parse::ffprobe::parse_segment_check`, PCM bytes → stereo samples).

use std::path::Path;

use qrec_core::media_argv::segment_check_argv;
use qrec_core::parse::ffprobe::{SegmentCounts, parse_segment_check};

use super::Ctx;
use super::child::{self, Spec, Stderr, Stdout};
use super::export::with_file;
use super::jobs::JobCtl;
use super::logs::{LogFile, shell_words};
use crate::paths::path_text;

/// The most ffprobe output kept (one short line per packet).
const STDOUT_LIMIT: usize = 64 << 20;

/// Counts the video packets and audio samples of `file` (absolute). The
/// probe's stderr is appended to the segment's log.
pub(crate) fn segment_counts(
    ctx: &Ctx,
    ctl: &JobCtl,
    file: &Path,
    log: &LogFile,
) -> Result<SegmentCounts, String> {
    let bytes = std::fs::metadata(file)
        .map_err(|e| format!("can't read {}: {e}", path_text(file)))?
        .len();
    let argv = segment_check_argv(&path_text(file));
    log.note(&format!("verify: {}", shell_words(&argv)));
    // The file itself, byte for byte (its folder's path may not be UTF-8).
    let args = with_file(&argv, file)?;
    let done = child::run(
        Spec::new(&args, Stderr::Log(log.file()))
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
    parse_segment_check(&String::from_utf8_lossy(&done.stdout))
}
