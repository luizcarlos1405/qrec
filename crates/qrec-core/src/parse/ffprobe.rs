//! `ffprobe -of json` output → `MediaInfo` (A§9.2). Owner: output.
//! Stub: signatures only.
#![allow(clippy::todo, unused_variables)]

use crate::model::StreamKind;
use crate::protocol::event::MediaInfo;

/// Parses the JSON printed by `media_argv::probe_argv(_, kind)`. `Err` for
/// unreadable files (no stream, bad JSON).
pub fn parse_ffprobe(json: &str, kind: StreamKind) -> Result<MediaInfo, String> {
    todo!("output: parse_ffprobe")
}
