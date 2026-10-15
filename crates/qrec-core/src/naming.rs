//! File names derived from the project name (A§9.9, A§5.9). Owner: output.
//!
//! Auto names are project-relative (`exports/…`, `frames/…`); the shell
//! joins them with the project dir and appends `-2`, `-3`, … when a name is
//! taken (A§8.8).

use chrono::{DateTime, FixedOffset, Utc};

use crate::ids::{Frame, TakeId};
use crate::model::Fps;
use crate::time::timecode_compact;

/// Longest stem in bytes.
pub const MAX_STEM_BYTES: usize = 120;

/// The marker in the shell's temp output names,
/// `.<stem>.qrec-part-<pid>-<job>.<ext>` (A§8.8). A file is a temp only
/// when its whole name has that shape (A§8.7); auto names never contain
/// the marker anyway ([`file_stem`]).
pub const TEMP_MARKER: &str = ".qrec-part-";

/// `name` made safe for a file name: `/`, NUL, control characters and `%`
/// become `_`, `.qrec-part` becomes `_qrec-part` (so no export or frame
/// name contains the temp marker, even after a cut stem and the `-` that
/// follows it), leading `.` and `-` are stripped, the result is capped at
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
    // The temp marker without its final `-`.
    let replaced = replaced.replace(".qrec-part", "_qrec-part");
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
    let offset = FixedOffset::east_opt(utc_offset_secs)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC is a valid offset"));
    let local = now.with_timezone(&offset);
    format!(
        "exports/{}-{}.{ext}",
        file_stem(project_name),
        local.format("%Y%m%d-%H%M%S")
    )
}

/// `frames/<stem>-<take>-<HHMMSSFF>.png` with the timeline timecode of `at`.
pub fn frame_file_name(project_name: &str, take: &TakeId, at: Frame, fps: Fps) -> String {
    format!(
        "frames/{}-{take}-{}.png",
        file_stem(project_name),
        timecode_compact(at, fps)
    )
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
        // No export or frame name carries the temp marker (A§8.7).
        assert_eq!(file_stem("talk.qrec-part-final"), "talk_qrec-part-final");
        assert_eq!(file_stem(".qrec-part-1-2"), "_qrec-part-1-2");
        assert_eq!(
            file_stem("a.qrec-part-.qrec-part-"),
            "a_qrec-part-_qrec-part-"
        );
        assert_eq!(file_stem("qrec-part-x"), "qrec-part-x");
        // A stem cut right after `.qrec-part` would make the marker with
        // the `-` that follows it.
        let cut = format!("{}.qrec-partX", "a".repeat(110));
        assert_eq!(file_stem(&cut), format!("{}_qrec-part", "a".repeat(110)));
        let now = DateTime::parse_from_rfc3339("2026-10-14T23:30:05Z")
            .unwrap()
            .with_timezone(&Utc);
        for name in [cut.as_str(), "talk.qrec-part-final", ".qrec-part-1-2"] {
            let export = export_file_name(name, now, 0, "mp4");
            let frame = frame_file_name(name, &TakeId::numbered(1), 3, Fps::new(30, 1));
            assert!(!export.contains(TEMP_MARKER), "{export}");
            assert!(!frame.contains(TEMP_MARKER), "{frame}");
        }
        let long = "é".repeat(100); // 200 bytes
        let stem = file_stem(&long);
        assert_eq!(stem.len(), 120);
        assert!(stem.chars().all(|c| c == 'é'));
    }

    #[test]
    fn export_names_use_local_time() {
        let now = DateTime::parse_from_rfc3339("2026-10-14T23:30:05Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            export_file_name("my-video", now, 0, "mp4"),
            "exports/my-video-20261014-233005.mp4"
        );
        // UTC−3 (São Paulo) and UTC+2: the local date can differ from UTC.
        assert_eq!(
            export_file_name("my-video", now, -3 * 3600, "webm"),
            "exports/my-video-20261014-203005.webm"
        );
        assert_eq!(
            export_file_name("../x", now, 2 * 3600, "mp4"),
            "exports/_x-20261015-013005.mp4"
        );
        // An impossible offset falls back to UTC.
        assert_eq!(
            export_file_name("", now, 100_000, "mp4"),
            "exports/qrec-20261014-233005.mp4"
        );
    }

    #[test]
    fn frame_names_use_the_timeline_timecode() {
        let take = TakeId::numbered(3);
        assert_eq!(
            frame_file_name("my-video", &take, 411, Fps::new(30, 1)),
            "frames/my-video-take-0003-00001321.png"
        );
        assert_eq!(
            frame_file_name("a/b", &take, 30 * 3600 + 2, Fps::new(30000, 1001)),
            "frames/a_b-take-0003-01000002.png"
        );
    }
}
