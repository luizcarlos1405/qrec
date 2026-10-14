//! Pure parsers for ffmpeg/ffprobe output (A§9.2). The shell runs the
//! processes; these turn their text into data.
//!
//! `silencedetect` belongs to the autocut owner; `ffprobe` and `progress`
//! to the output owner.

pub mod ffprobe;
pub mod progress;
pub mod silencedetect;
