//! ffmpeg `-progress pipe:1` blocks (A§9.2). Owner: output.
//! Stub: signatures only.
//!
//! Blocks are `key=value` lines ending with `progress=continue|end`.
//! `out_time_us` (and, in ffmpeg 8.1, `out_time_ms`, also µs) give the
//! output time; `N/A` counts as 0.
#![allow(clippy::todo, unused_variables, dead_code)]

/// One complete progress block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressBlock {
    /// `frame=`.
    pub frame: Option<u64>,
    /// `out_time_us=` in nanoseconds.
    pub out_time_ns: Option<i64>,
    /// `progress=end`.
    pub end: bool,
}

/// Feeds lines one at a time; yields a block at each `progress=` line.
#[derive(Debug, Clone, Default)]
pub struct ProgressParser {
    current: ProgressBlock,
}

impl ProgressParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// The finished block when `line` is a `progress=` line.
    pub fn feed(&mut self, line: &str) -> Option<ProgressBlock> {
        todo!("output: ProgressParser::feed")
    }
}
