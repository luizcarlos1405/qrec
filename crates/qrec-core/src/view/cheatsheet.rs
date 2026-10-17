//! The cheat sheet (G§4.11, S20, S20b; SPEC §10.1 layer 2: "a full-screen
//! overlay grouped by category, searchable, with the current keys").
//! Owner: WP9.
//!
//! Three columns of SPEC §10.2's groups: Record, Output, then Global
//! (niri) │ Playback, View │ Edit, then Canvas at the bottom (D-22). A
//! row shows the keys that run (`keymap::usable_keys`) and, when
//! `keys.toml` changed them, the defaults ("default P"). Global (niri) is
//! the compositor binds of SPEC §10.2's snippet (`keymap::NIRI_SNIPPET`),
//! with "Copy niri snippet". A search keeps the rows whose label holds
//! each of its words (any case), bolds them, drops the groups left empty
//! and says how many match ("3 shortcuts match “trim”"). Labels are the
//! core keymap's (D-31); the sheet's own copy and the niri rows are
//! BRIEF S20's until its frame is drawn into the build.

use serde::{Deserialize, Serialize};

use crate::keymap::{
    Binding, KeyChord, Keymap, NIRI_SNIPPET, default_keys, is_rebound, key_name, usable_keys,
};

/// The sheet: three columns of groups.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatSheet {
    /// "Keyboard shortcuts".
    pub title: String,
    /// "Search shortcuts".
    pub placeholder: String,
    pub columns: Vec<Vec<CheatGroup>>,
    /// "3 shortcuts match “trim”" while searching.
    pub matches: Option<String>,
    /// "Keys come from ~/.config/qrec/keys.toml. Change them in Settings ›
    /// Keyboard."
    pub footer: String,
}

/// A group of the sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatGroup {
    pub title: String,
    pub rows: Vec<CheatRow>,
    /// Global (niri)'s snippet and its button.
    pub snippet: Option<Snippet>,
}

/// A text to copy, with its button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    /// "Copy niri snippet".
    pub button: String,
    /// What it copies (SPEC §10.2's `binds { … }`).
    pub text: String,
}

/// One shortcut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatRow {
    pub id: String,
    pub label: String,
    /// Char indices of `label` the search matched.
    pub matched: Vec<usize>,
    /// Key-cap legends per chord.
    pub keys: Vec<Vec<String>>,
    /// "default P" when rebound.
    pub default_keys: Option<String>,
}

/// The column of each group (S20): 0 Record, Output, Global (niri); 1
/// Playback, View; 2 Edit, Canvas (D-22).
const COLUMNS: [&[&str]; 3] = [
    &["Record", "Output", NIRI_GROUP],
    &["Playback", "View"],
    &["Edit", "Canvas"],
];

/// The compositor binds' group.
pub const NIRI_GROUP: &str = "Global (niri)";

/// The rows of Global (niri): SPEC §10.2's snippet, as BRIEF S20 words
/// them.
const NIRI_ROWS: [(&str, &str, &[&str]); 3] = [
    (
        "niri.record-toggle",
        "Record toggle",
        &["Mod", "Shift", "R"],
    ),
    ("niri.record-over", "Record over", &["Mod", "Shift", "O"]),
    (
        "niri.discard-last",
        "Discard last take",
        &["Mod", "Shift", "D"],
    ),
];

/// The sheet for `search` (empty: everything); `keys_path` is `keys.toml`
/// as people read it (`~/.config/qrec/keys.toml`).
pub fn cheat_sheet(keymap: &Keymap, keys_path: &str, search: &str) -> CheatSheet {
    let words = search_words(search);
    let mut count = 0;
    let columns = COLUMNS
        .iter()
        .map(|groups| {
            groups
                .iter()
                .filter_map(|group| {
                    let mut rows: Vec<CheatRow> = if *group == NIRI_GROUP {
                        NIRI_ROWS
                            .iter()
                            .map(|(id, label, caps)| CheatRow {
                                id: (*id).to_owned(),
                                label: (*label).to_owned(),
                                matched: Vec::new(),
                                keys: vec![caps.iter().map(|c| (*c).to_owned()).collect()],
                                default_keys: None,
                            })
                            .collect()
                    } else {
                        keymap
                            .bindings
                            .iter()
                            .filter(|b| b.group == *group)
                            .map(|b| row(keymap, b))
                            .collect()
                    };
                    if !words.is_empty() {
                        rows.retain_mut(|r| match word_match(&words, &r.label) {
                            Some(matched) => {
                                r.matched = matched;
                                true
                            }
                            None => false,
                        });
                    }
                    count += rows.len();
                    (!rows.is_empty()).then(|| CheatGroup {
                        title: (*group).to_owned(),
                        snippet: (*group == NIRI_GROUP).then(|| Snippet {
                            button: "Copy niri snippet".to_owned(),
                            text: NIRI_SNIPPET.to_owned(),
                        }),
                        rows,
                    })
                })
                .collect()
        })
        .collect();
    let shown = search.trim();
    CheatSheet {
        title: "Keyboard shortcuts".to_owned(),
        placeholder: "Search shortcuts".to_owned(),
        columns,
        matches: (!words.is_empty()).then(|| match count {
            0 => format!("No shortcuts match \u{201c}{shown}\u{201d}"),
            1 => format!("1 shortcut matches \u{201c}{shown}\u{201d}"),
            n => format!("{n} shortcuts match \u{201c}{shown}\u{201d}"),
        }),
        footer: format!("Keys come from {keys_path}. Change them in Settings \u{203a} Keyboard."),
    }
}

fn row(keymap: &Keymap, b: &Binding) -> CheatRow {
    CheatRow {
        id: b.id.to_owned(),
        label: b.label.to_owned(),
        matched: Vec::new(),
        keys: usable_keys(keymap, b)
            .into_iter()
            .map(KeyChord::cap_legends)
            .collect(),
        default_keys: rebound_note(b),
    }
}

/// "default P" for a binding `keys.toml` changed (S20, S22): its default
/// keys as they read in a sentence, `default Del, X, Backspace`.
pub fn rebound_note(b: &Binding) -> Option<String> {
    if !is_rebound(b) {
        return None;
    }
    let defaults = default_keys(b.id)?;
    let names: Vec<String> = defaults.iter().map(key_name).collect();
    Some(format!("default {}", names.join(", ")))
}

/// The words of a search, lowercased.
pub fn search_words(search: &str) -> Vec<String> {
    search.split_whitespace().map(str::to_lowercase).collect()
}

/// Where each of `words` first shows in `label` (any case), as the char
/// indices to bold; `None` when one doesn't. A search for the sheet and
/// the Keyboard page: every word as written, not scattered letters.
pub fn word_match(words: &[String], label: &str) -> Option<Vec<usize>> {
    let lower: Vec<char> = label
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    let mut matched: Vec<usize> = Vec::new();
    for word in words {
        let w: Vec<char> = word.chars().collect();
        let at =
            (0..=lower.len().checked_sub(w.len())?).find(|&i| lower[i..i + w.len()] == w[..])?;
        matched.extend(at..at + w.len());
    }
    matched.sort_unstable();
    matched.dedup();
    Some(matched)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::keymap::resolve;
    use crate::view::palette::bracketed;

    const KEYS: &str = "~/.config/qrec/keys.toml";

    fn text(sheet: &CheatSheet) -> String {
        let mut out = format!("# {} [{}]\n", sheet.title, sheet.placeholder);
        if let Some(matches) = &sheet.matches {
            let _ = writeln!(out, "({matches})");
        }
        for (n, column) in sheet.columns.iter().enumerate() {
            let _ = writeln!(out, "=== column {}", n + 1);
            for group in column {
                let _ = writeln!(out, "## {}", group.title);
                for row in &group.rows {
                    let caps: Vec<String> = row
                        .keys
                        .iter()
                        .map(|chord| chord.iter().map(|c| format!("[{c}]")).collect::<String>())
                        .collect();
                    let _ = writeln!(
                        out,
                        "{:<22} {}{}",
                        caps.join(" "),
                        bracketed(&row.label, &row.matched),
                        row.default_keys
                            .as_ref()
                            .map(|d| format!("  ({d})"))
                            .unwrap_or_default()
                    );
                }
                if let Some(snippet) = &group.snippet {
                    let _ = writeln!(out, "<{}>", snippet.button);
                }
            }
        }
        let _ = writeln!(out, "-- {}", sheet.footer);
        out
    }

    #[test]
    fn snapshot_s20_three_columns() {
        let sheet = cheat_sheet(&resolve("[bindings]\nframe.save = \"f\"\n"), KEYS, "");
        insta::assert_snapshot!("cheatsheet_s20", text(&sheet));
        let titles: Vec<Vec<&str>> = sheet
            .columns
            .iter()
            .map(|c| c.iter().map(|g| g.title.as_str()).collect())
            .collect();
        assert_eq!(
            titles,
            [
                vec!["Record", "Output", "Global (niri)"],
                vec!["Playback", "View"],
                vec!["Edit", "Canvas"],
            ],
            "D-22: Canvas at the bottom of column 3"
        );
        let output = &sheet.columns[0][1].rows;
        let save = output.iter().find(|r| r.id == "frame.save").unwrap();
        assert_eq!(save.keys, [vec!["F".to_owned()]]);
        assert_eq!(save.default_keys.as_deref(), Some("default P"));
        let niri = &sheet.columns[0][2];
        assert_eq!(niri.rows[0].keys, [vec!["Mod", "Shift", "R"]]);
        assert_eq!(niri.snippet.as_ref().unwrap().text, NIRI_SNIPPET);
        assert!(sheet.matches.is_none());
        assert_eq!(
            sheet.footer,
            "Keys come from ~/.config/qrec/keys.toml. Change them in Settings › Keyboard."
        );
    }

    #[test]
    fn snapshot_s20b_search() {
        let sheet = cheat_sheet(&resolve(""), KEYS, " Trim ");
        insta::assert_snapshot!("cheatsheet_s20b_trim", text(&sheet));
        assert_eq!(
            sheet.matches.as_deref(),
            Some("6 shortcuts match \u{201c}Trim\u{201d}")
        );
        let edit = &sheet.columns[2][0];
        assert_eq!(edit.title, "Edit");
        assert_eq!(edit.rows[0].matched, [0, 1, 2, 3]);
        assert!(sheet.columns[0].is_empty() && sheet.columns[1].is_empty());
        // Every word, anywhere in the label; the niri rows too.
        let sheet = cheat_sheet(&resolve(""), KEYS, "end trim");
        assert_eq!(
            sheet.matches.as_deref(),
            Some("3 shortcuts match \u{201c}end trim\u{201d}")
        );
        let sheet = cheat_sheet(&resolve(""), KEYS, "discard");
        let niri = sheet.columns[0]
            .iter()
            .find(|g| g.title == NIRI_GROUP)
            .unwrap();
        assert_eq!(niri.rows.len(), 1);
        assert!(niri.snippet.is_some());
        let none = cheat_sheet(&resolve(""), KEYS, "xyzzy");
        assert_eq!(
            none.matches.as_deref(),
            Some("No shortcuts match \u{201c}xyzzy\u{201d}")
        );
        assert!(none.columns.iter().all(Vec::is_empty));
        let one = cheat_sheet(&resolve(""), KEYS, "quit");
        assert_eq!(
            one.matches.as_deref(),
            Some("1 shortcut matches \u{201c}quit\u{201d}")
        );
    }

    /// Keys that run show; a conflicting one doesn't (it never runs), an
    /// unbound binding shows none and its defaults.
    #[test]
    fn rows_follow_keys_toml() {
        let sheet = cheat_sheet(
            &resolve("[bindings]\nautocut.run = \"x\"\nrecord.over = []\n"),
            KEYS,
            "",
        );
        let all: Vec<&CheatRow> = sheet
            .columns
            .iter()
            .flatten()
            .flat_map(|g| &g.rows)
            .collect();
        let row = |id: &str| *all.iter().find(|r| r.id == id).unwrap();
        assert!(row("autocut.run").keys.is_empty());
        assert_eq!(
            row("autocut.run").default_keys.as_deref(),
            Some("default C")
        );
        assert_eq!(row("clip.remove").keys.len(), 2, "x drops out");
        assert_eq!(row("clip.remove").default_keys, None);
        assert!(row("record.over").keys.is_empty());
        assert_eq!(
            row("record.over").default_keys.as_deref(),
            Some("default O")
        );
        let remove = resolve("[bindings]\nclip.remove = \"q\"\n");
        let b = remove.binding("clip.remove").unwrap();
        assert_eq!(
            rebound_note(b).as_deref(),
            Some("default Del, X, Backspace")
        );
    }

    #[test]
    fn words_match_where_they_are_written() {
        let words = search_words("PLAY head");
        assert_eq!(words, ["play", "head"]);
        assert_eq!(
            word_match(&words, "Split clip at the playhead"),
            Some((18..26).collect())
        );
        assert_eq!(
            word_match(&search_words("tp"), "Trim to the playhead"),
            None
        );
        assert_eq!(word_match(&[], "x"), Some(vec![]));
        assert_eq!(word_match(&search_words("longer than"), "short"), None);
    }
}
