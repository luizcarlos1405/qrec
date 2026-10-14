//! ffmpeg `silencedetect` stderr → silence intervals (A§9.2). Owner: autocut.
//! Stub: signatures only.
#![allow(clippy::todo, unused_variables)]

use crate::protocol::event::Interval;

/// The `silence_start` / `silence_end` pairs in `stderr`, in file time. A
/// trailing silence without `silence_end` ends at the input's duration
/// (the `Duration:` line). `Err` when the output can't be understood.
pub fn parse_silencedetect(stderr: &str) -> Result<Vec<Interval>, String> {
    todo!("autocut: parse_silencedetect")
}
