//! ffmpeg `-progress pipe:1` blocks (A§9.2). Owner: output.
//!
//! Blocks are `key=value` lines ending with `progress=continue|end`.
//! `out_time_us` (and, in ffmpeg 8.1, `out_time_ms`, also µs) give the
//! output time; `N/A` counts as 0. An 8.1.2 block looks like:
//!
//! ```text
//! frame=30
//! fps=0.00
//! stream_0_0_q=-1.0
//! bitrate=1685.7kbits/s
//! total_size=196664
//! out_time_us=933333
//! out_time_ms=933333
//! out_time=00:00:00.933333
//! dup_frames=0
//! drop_frames=0
//! speed=7.46x
//! progress=end
//! ```

/// One complete progress block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressBlock {
    /// `frame=`.
    pub frame: Option<u64>,
    /// `out_time_us=` in nanoseconds (never negative).
    pub out_time_ns: Option<i64>,
    /// `progress=end`.
    pub end: bool,
}

/// Feeds lines one at a time; yields a block at each `progress=` line.
#[derive(Debug, Clone, Default)]
pub struct ProgressParser {
    current: ProgressBlock,
    /// Which key set `current.out_time_ns`: `out_time_us` beats
    /// `out_time_ms`, which beats `out_time`.
    time_rank: u8,
}

impl ProgressParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// The finished block when `line` is a `progress=` line.
    pub fn feed(&mut self, line: &str) -> Option<ProgressBlock> {
        let (key, value) = line.trim().split_once('=')?;
        let value = value.trim();
        match key.trim() {
            "frame" => self.current.frame = Some(integer(value).unwrap_or(0).max(0) as u64),
            "out_time_us" => self.set_time(3, integer(value).map(|us| us.saturating_mul(1000))),
            // ffmpeg 8.1 prints out_time_ms in microseconds too.
            "out_time_ms" => self.set_time(2, integer(value).map(|us| us.saturating_mul(1000))),
            "out_time" => self.set_time(1, clock_ns(value)),
            "progress" => {
                let mut block = std::mem::take(&mut self.current);
                block.end = value == "end";
                self.time_rank = 0;
                return Some(block);
            }
            _ => {}
        }
        None
    }

    /// `N/A` (and anything unparsable or negative) counts as 0.
    fn set_time(&mut self, rank: u8, ns: Option<i64>) {
        if rank >= self.time_rank {
            self.time_rank = rank;
            self.current.out_time_ns = Some(ns.unwrap_or(0).max(0));
        }
    }
}

fn integer(value: &str) -> Option<i64> {
    value.parse().ok()
}

/// `HH:MM:SS.micro` → nanoseconds.
fn clock_ns(value: &str) -> Option<i64> {
    let negative = value.starts_with('-');
    let mut parts = value.trim_start_matches('-').splitn(3, ':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
    let (secs, frac) = s.split_once('.').unwrap_or((s, ""));
    let secs: i64 = secs.parse().ok()?;
    let mut frac_ns = 0i64;
    for (i, digit) in frac.bytes().take(9).enumerate() {
        if !digit.is_ascii_digit() {
            return None;
        }
        frac_ns += i64::from(digit - b'0') * 10i64.pow(8 - i as u32);
    }
    let ns = ((h * 60 + m) * 60 + secs) * 1_000_000_000 + frac_ns;
    Some(if negative { -ns } else { ns })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(parser: &mut ProgressParser, text: &str) -> Vec<ProgressBlock> {
        text.lines().filter_map(|line| parser.feed(line)).collect()
    }

    #[test]
    fn blocks_from_ffmpeg_8_1() {
        let mut parser = ProgressParser::new();
        let blocks = feed_all(
            &mut parser,
            "frame=12\nfps=0.00\nstream_0_0_q=-1.0\nbitrate=N/A\ntotal_size=48\nout_time_us=N/A\nout_time_ms=N/A\nout_time=N/A\n\
             dup_frames=0\ndrop_frames=0\nspeed=N/A\nprogress=continue\n\
             frame=30\nfps=0.00\nout_time_us=933333\nout_time_ms=933333\nout_time=00:00:00.933333\nprogress=end\n",
        );
        assert_eq!(
            blocks,
            vec![
                ProgressBlock { frame: Some(12), out_time_ns: Some(0), end: false },
                ProgressBlock { frame: Some(30), out_time_ns: Some(933_333_000), end: true },
            ]
        );
    }

    #[test]
    fn time_keys_fall_back() {
        let mut parser = ProgressParser::new();
        assert_eq!(parser.feed("out_time=01:00:01.500000"), None);
        let block = parser.feed("progress=continue").unwrap();
        assert_eq!(block.out_time_ns, Some(3_601_500_000_000));
        assert_eq!(block.frame, None);
        // out_time_us wins whatever the order.
        feed_all(&mut parser, "out_time_us=2000\nout_time_ms=7000\nout_time=00:00:09.000000");
        assert_eq!(parser.feed("progress=continue").unwrap().out_time_ns, Some(2_000_000));
        feed_all(&mut parser, "out_time_ms=7000\nout_time=00:00:09.000000");
        assert_eq!(parser.feed("progress=continue").unwrap().out_time_ns, Some(7_000_000));
        // Negative times (before the first packet) count as 0.
        feed_all(&mut parser, "out_time_us=-33333\n");
        assert_eq!(parser.feed("progress=continue").unwrap().out_time_ns, Some(0));
        assert_eq!(clock_ns("-00:00:00.033333"), Some(-33_333_000));
    }

    #[test]
    fn ignores_noise() {
        let mut parser = ProgressParser::new();
        assert_eq!(parser.feed(""), None);
        assert_eq!(parser.feed("garbage"), None);
        assert_eq!(parser.feed("frame=abc"), None);
        assert_eq!(parser.feed("  progress=end \r").unwrap(), ProgressBlock {
            frame: Some(0),
            out_time_ns: None,
            end: true
        });
        assert_eq!(parser.feed("progress=continue").unwrap(), ProgressBlock::default());
    }
}
