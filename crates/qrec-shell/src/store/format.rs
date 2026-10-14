//! What the store's files contain.
//!
//! - `qrec.json`: the project, pretty-printed (2 spaces) with a final
//!   newline, so people, git and agents can read and diff it.
//! - `.qrec/history.jsonl`: one compact `HistoryEntry` per line.
//! - `.qrec/history.head.json`: the commit record (A§7.6)
//!   `{"cursor","baseSeq","nextSeq","projectSha256","project"}`.
//! - `.qrec/history.base.json`: `{"baseSeq","project"}`, the project before
//!   the oldest kept entry. Carrying its own seq keeps it meaningful when a
//!   crash lands between a history rewrite and the commit record. A bare
//!   project (no wrapper) is accepted too; its seq then comes from the head.

use qrec_core::hash::sha256_hex;
use qrec_core::history::HistoryEntry;
use qrec_core::migrate::Migrated;
use qrec_core::model::{CURRENT_VERSION, Project};
use qrec_core::{CoreError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How to get out of a broken `qrec.json` (A§7.7).
pub const RESTORE_HINT: &str =
    "fix qrec.json, or rebuild it from the history with `qrec history restore <seq> --yes`";

/// The commit record, `.qrec/history.head.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Head {
    pub cursor: u64,
    pub base_seq: u64,
    pub next_seq: u64,
    /// sha256 of the exact bytes of `qrec.json` this commit wrote.
    pub project_sha256: String,
    pub project: Project,
}

/// `.qrec/history.base.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaseFile {
    pub base_seq: u64,
    pub project: Project,
}

/// `qrec.json` bytes: pretty JSON and a final newline.
pub fn project_bytes(project: &Project) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(project).expect("projects always serialize");
    bytes.push(b'\n');
    bytes
}

/// sha256 hex of file bytes (what the commit record stores).
pub fn sha(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

/// The head file for these values (the project is borrowed, not copied).
pub fn head_bytes(
    cursor: u64,
    base_seq: u64,
    next_seq: u64,
    project_sha256: &str,
    project: &Project,
) -> Vec<u8> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HeadRef<'a> {
        cursor: u64,
        base_seq: u64,
        next_seq: u64,
        project_sha256: &'a str,
        project: &'a Project,
    }
    let head = HeadRef {
        cursor,
        base_seq,
        next_seq,
        project_sha256,
        project,
    };
    let mut bytes = serde_json::to_vec(&head).expect("heads always serialize");
    bytes.push(b'\n');
    bytes
}

/// The base file for `project` at `base_seq`.
pub fn base_bytes(base_seq: u64, project: &Project) -> Vec<u8> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct BaseRef<'a> {
        base_seq: u64,
        project: &'a Project,
    }
    let mut bytes =
        serde_json::to_vec(&BaseRef { base_seq, project }).expect("bases always serialize");
    bytes.push(b'\n');
    bytes
}

/// One history line, newline included.
pub fn entry_line(entry: &HistoryEntry) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(entry).expect("history entries always serialize");
    bytes.push(b'\n');
    bytes
}

/// The whole history file for `entries`.
pub fn history_bytes<'a>(entries: impl IntoIterator<Item = &'a HistoryEntry>) -> Vec<u8> {
    entries.into_iter().flat_map(entry_line).collect()
}

/// Reads `history.base.json` (wrapped or bare). `None` when it doesn't parse.
pub fn parse_base(bytes: &[u8]) -> Option<(Option<u64>, Project)> {
    if let Ok(base) = serde_json::from_slice::<BaseFile>(bytes) {
        return Some((Some(base.base_seq), base.project));
    }
    serde_json::from_slice::<Project>(bytes)
        .ok()
        .map(|project| (None, project))
}

/// Parses `qrec.json` text into a current-version project.
///
/// A current-version file is deserialized straight from the text (errors
/// carry line and column); older versions go through the core's
/// migrations (`migrate::parse_project`); a newer version is `Invalid`.
pub fn parse_project_text(text: &str) -> Result<Migrated, CoreError> {
    #[derive(Deserialize)]
    struct VersionOnly {
        version: Option<Value>,
    }
    let peek: VersionOnly = serde_json::from_str(text).map_err(|e| {
        CoreError::invalid(format!("qrec.json is not valid JSON: {e}")).with_hint(RESTORE_HINT)
    })?;
    let version = match peek.version {
        None => {
            return Err(CoreError::invalid("qrec.json has no version").with_hint(RESTORE_HINT));
        }
        Some(v) => v
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v >= 1)
            .ok_or_else(|| {
                CoreError::invalid(format!(
                    "qrec.json version {v} is not a schema version (1, 2, …)"
                ))
                .with_hint(RESTORE_HINT)
            })?,
    };
    if version > CURRENT_VERSION {
        return Err(CoreError::invalid(format!(
            "qrec.json is schema version {version}, but this Qrec reads up to version {CURRENT_VERSION}"
        ))
        .with_hint("the project was saved by a newer Qrec: update Qrec to open it"));
    }
    if version < CURRENT_VERSION {
        return qrec_core::migrate::parse_project(text);
    }
    let project: Project = serde_json::from_str(text).map_err(|e| {
        CoreError::invalid(format!("qrec.json doesn't match the project schema: {e}"))
            .with_hint(RESTORE_HINT)
    })?;
    Ok(Migrated {
        project,
        from_version: CURRENT_VERSION,
    })
}

/// `history.jsonl` after the A§7.6 open-time rules.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedHistory {
    pub entries: Vec<HistoryEntry>,
    /// A torn last line (no newline, or unparsable) was dropped.
    pub dropped_torn: bool,
}

/// Parses history lines: a torn last line (one without its newline, or a
/// last line that doesn't parse) is dropped; a bad line elsewhere is
/// `Invalid` naming its line number; blank lines are skipped; seqs must
/// increase.
pub fn parse_history(bytes: &[u8]) -> Result<ParsedHistory, CoreError> {
    let complete_end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let mut dropped_torn =
        complete_end < bytes.len() && !bytes[complete_end..].iter().all(u8::is_ascii_whitespace);
    let lines: Vec<&[u8]> = bytes[..complete_end]
        .split(|b| *b == b'\n')
        .take(
            bytes[..complete_end]
                .iter()
                .filter(|b| **b == b'\n')
                .count(),
        )
        .collect();
    let last_content = lines
        .iter()
        .rposition(|l| !l.iter().all(u8::is_ascii_whitespace));
    let mut entries: Vec<HistoryEntry> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let number = i + 1;
        let entry: HistoryEntry = match serde_json::from_slice(line) {
            Ok(entry) => entry,
            // The last line can be torn even with its newline (a crash
            // that left zeroes behind): drop it unless a torn tail follows.
            Err(_) if Some(i) == last_content && !dropped_torn => {
                dropped_torn = true;
                continue;
            }
            Err(e) => {
                return Err(CoreError::new(
                    ErrorCode::Invalid,
                    format!(".qrec/history.jsonl line {number} is corrupt: {e}"),
                )
                .with_hint("fix or delete that line; `qrec history` lists the entries that load"));
            }
        };
        if let Some(prev) = entries.last()
            && entry.seq <= prev.seq
        {
            return Err(CoreError::invalid(format!(
                ".qrec/history.jsonl line {number}: seq {} doesn't follow seq {}",
                entry.seq, prev.seq
            ))
            .with_hint("history entries must be in increasing seq order"));
        }
        entries.push(entry);
    }
    Ok(ParsedHistory {
        entries,
        dropped_torn,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testdata::{entry, sample_project};

    #[test]
    fn history_round_trip() {
        let entries = [entry(1), entry(2), entry(5)];
        let bytes = history_bytes(&entries);
        assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 3);
        let parsed = parse_history(&bytes).unwrap();
        assert_eq!(parsed.entries, entries);
        assert!(!parsed.dropped_torn);
        assert_eq!(parse_history(b"").unwrap().entries, []);
        assert_eq!(parse_history(b"\n\n").unwrap().entries, []);
    }

    #[test]
    fn a_torn_last_line_is_dropped() {
        let mut bytes = history_bytes(&[entry(1), entry(2)]);
        let full = entry_line(&entry(3));
        bytes.extend_from_slice(&full[..full.len() / 2]);
        let parsed = parse_history(&bytes).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert!(parsed.dropped_torn);

        // A complete-looking last line that doesn't parse is torn too.
        let mut bytes = history_bytes(&[entry(1)]);
        bytes.extend_from_slice(b"\0\0\0\0\n");
        let parsed = parse_history(&bytes).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert!(parsed.dropped_torn);
    }

    #[test]
    fn a_bad_line_elsewhere_is_invalid_with_its_number() {
        let mut bytes = history_bytes(&[entry(1)]);
        bytes.extend_from_slice(b"{not json}\n");
        bytes.extend_from_slice(&history_bytes(&[entry(3)]));
        let err = parse_history(&bytes).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert!(
            err.message
                .starts_with(".qrec/history.jsonl line 2 is corrupt"),
            "{}",
            err.message
        );

        // A bad last line followed by a torn tail: the bad line isn't last.
        let mut bytes = history_bytes(&[entry(1)]);
        bytes.extend_from_slice(b"{bad}\n{\"seq\":");
        let err = parse_history(&bytes).unwrap_err();
        assert!(err.message.contains("line 2"), "{}", err.message);

        let bytes = history_bytes(&[entry(2), entry(2)]);
        let err = parse_history(&bytes).unwrap_err();
        assert!(
            err.message.contains("line 2: seq 2 doesn't follow seq 2"),
            "{}",
            err.message
        );
    }

    #[test]
    fn projects_parse_with_versions_checked() {
        let project = sample_project();
        let text = String::from_utf8(project_bytes(&project)).unwrap();
        assert!(text.ends_with("}\n") && text.contains("\n  \"version\": 1,"));
        let migrated = parse_project_text(&text).unwrap();
        assert_eq!(migrated.project, project);
        assert_eq!(migrated.from_version, CURRENT_VERSION);

        for (text, needle) in [
            ("{", "not valid JSON"),
            ("[]", "not valid JSON"),
            ("{\"name\":\"x\"}", "has no version"),
            ("{\"version\":0}", "not a schema version"),
            ("{\"version\":\"1\"}", "not a schema version"),
            ("{\"version\":99}", "schema version 99"),
            ("{\"version\":1}", "doesn't match the project schema"),
        ] {
            let err = parse_project_text(text).unwrap_err();
            assert_eq!(err.code, ErrorCode::Invalid);
            assert!(err.message.contains(needle), "{text}: {}", err.message);
        }
    }

    #[test]
    fn base_files_wrapped_or_bare() {
        let project = sample_project();
        let wrapped = base_bytes(7, &project);
        assert_eq!(parse_base(&wrapped), Some((Some(7), project.clone())));
        let parsed: BaseFile = serde_json::from_slice(&wrapped).unwrap();
        assert_eq!(parsed.base_seq, 7);
        let head: Head = serde_json::from_slice(&head_bytes(3, 1, 4, "abc", &project)).unwrap();
        assert_eq!(
            (
                head.cursor,
                head.base_seq,
                head.next_seq,
                head.project_sha256.as_str()
            ),
            (3, 1, 4, "abc")
        );
        assert_eq!(head.project, project);
        let bare = project_bytes(&project);
        assert_eq!(parse_base(&bare), Some((None, project)));
        assert_eq!(parse_base(b"{}"), None);
    }
}
