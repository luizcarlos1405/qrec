//! File names derived from the project name (A§9.9, A§5.9). Owner: output.
//!
//! `file_stem` is implemented by the foundation (capture file names need it
//! too); the auto names are signatures only.
#![allow(clippy::todo, unused_variables)]

use chrono::{DateTime, Utc};

use crate::ids::{Frame, TakeId};
use crate::model::Fps;

/// Longest stem in bytes.
pub const MAX_STEM_BYTES: usize = 120;

/// `name` made safe for a file name: `/`, NUL, control characters and `%`
/// become `_`, leading `.` and `-` are stripped, the result is capped at
/// 120 bytes on a char boundary; empty ⇒ `qrec`.
pub fn file_stem(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '%' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = replaced.trim_start_matches(['.', '-']);
    let mut end = trimmed.len().min(MAX_STEM_BYTES);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let stem = &trimmed[..end];
    if stem.is_empty() {
        "qrec".to_owned()
    } else {
        stem.to_owned()
    }
}

/// `exports/<stem>-YYYYMMDD-HHMMSS.<ext>` in local time (`utc_offset_secs`).
pub fn export_file_name(
    project_name: &str,
    now: DateTime<Utc>,
    utc_offset_secs: i32,
    ext: &str,
) -> String {
    todo!("output: export_file_name")
}

/// `frames/<stem>-<take>-<HHMMSSFF>.png` with the timeline timecode of `at`.
pub fn frame_file_name(project_name: &str, take: &TakeId, at: Frame, fps: Fps) -> String {
    todo!("output: frame_file_name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems() {
        assert_eq!(file_stem("my-video"), "my-video");
        assert_eq!(file_stem("a/b%c\u{0}d\ne"), "a_b_c_d_e");
        assert_eq!(file_stem("..-.hidden"), "hidden");
        assert_eq!(file_stem(""), "qrec");
        assert_eq!(file_stem("..."), "qrec");
        let long = "é".repeat(100); // 200 bytes
        let stem = file_stem(&long);
        assert_eq!(stem.len(), 120);
        assert!(stem.chars().all(|c| c == 'é'));
    }
}
