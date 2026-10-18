//! The cheat sheet (G§4.11, S20, S20b; SPEC §10.1 layer 2: "a full-screen
//! overlay grouped by category, searchable, with the current keys").
//! Owner: WP9.
//!
//! S20's rows in three columns: Record, Output, Global (niri), Window │
//! Playback, View │ Edit, Canvas (D-22: the layer nudges at the bottom of
//! column 3). A row is one action or a pair or trio drawn as one
//! ("Previous / next frame" with `←` / `→`): the keys of its bindings
//! that run (`keymap::usable_keys`), the bindings joined by "/" and a
//! binding's own keys by "or" ("Del or X"); "default P" when `keys.toml`
//! changed them. Global (niri) is the compositor binds of SPEC §10.2's
//! snippet (`keymap::NIRI_SNIPPET`), with "Copy niri snippet". Window (not
//! drawn: S20's View group has no room for them) holds the window's own
//! keys of D-10, F6 and Ctrl+Q, so every key is on the sheet.
//!
//! A search keeps the rows whose label holds each of its words (any case),
//! bolds them, and shows the groups left in one column, in reading order,
//! under "3 shortcuts match “trim”" and over the palette's tip (S20b).

use serde::{Deserialize, Serialize};

use crate::keymap::{
    Binding, KeyChord, Keymap, NIRI_SNIPPET, default_keys, is_rebound, key_name, usable_keys,
};

/// The sheet: its columns of groups (one while searching), its copy.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatSheet {
    /// "Keyboard shortcuts".
    pub title: String,
    /// "Search shortcuts".
    pub placeholder: String,
    /// Three columns; one while searching (S20b's results).
    pub columns: Vec<Vec<CheatGroup>>,
    /// "3 shortcuts match “trim”" while searching.
    pub matches: Option<String>,
    /// S20b: "To run a command by name, open the palette" and the
    /// palette's keys, while searching.
    pub palette_tip: Option<KeyHint>,
    /// "Keys come from ~/.config/qrec/keys.toml. Change them in Settings ›
    /// Keyboard."
    pub footer: String,
    /// The path in [`CheatSheet::footer`] (`mono-sm`).
    pub footer_path: String,
    /// The footer's "[Ctrl][,] Settings".
    pub settings: KeyHint,
}

/// A label with the keys of a binding (the footer's Settings hint, the
/// palette tip).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyHint {
    pub label: String,
    /// Key-cap legends of the binding's first key that runs; empty when it
    /// has none.
    pub keys: Vec<String>,
}

/// A group of the sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatGroup {
    pub title: String,
    pub rows: Vec<CheatRow>,
    /// Global (niri)'s note, snippet and button.
    pub snippet: Option<Snippet>,
}

/// SPEC §10.2's niri snippet, to copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    /// "Bind these in niri to record while another app has focus."
    pub note: String,
    /// "Copy niri snippet".
    pub button: String,
    /// What it copies (SPEC §10.2's `binds { … }`).
    pub text: String,
    /// "Paste into".
    pub paste_into: String,
    /// "~/.config/niri/config.kdl".
    pub path: String,
}

/// One row of the sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatRow {
    /// Its first binding's id (the niri rows: `niri.record-toggle`, …).
    pub id: String,
    pub label: String,
    /// Char indices of `label` the search matched.
    pub matched: Vec<usize>,
    /// One part per binding, drawn "/" apart.
    pub keys: Vec<RowKeys>,
    /// "default P" when `keys.toml` changed a binding of the row.
    pub default_keys: Option<String>,
}

/// A binding's part of a row: the keys that run it, drawn "or" apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowKeys {
    pub id: String,
    /// Key-cap legends per chord.
    pub chords: Vec<Vec<String>>,
}

/// The compositor binds' group.
pub const NIRI_GROUP: &str = "Global (niri)";

/// The window's own keys' group (D-10; not drawn).
pub const WINDOW_GROUP: &str = "Window";

/// One row of the table: its label and its bindings.
struct Row {
    label: &'static str,
    ids: &'static [&'static str],
}

const fn row(label: &'static str, ids: &'static [&'static str]) -> Row {
    Row { label, ids }
}

/// S20's sheet: each column's groups and their rows, as drawn (Window and
/// Canvas in its style, D-22).
const SHEET: [&[(&str, &[Row])]; 3] = [
    &[
        (
            "Record",
            &[
                row("Toggle record (start / stop)", &["record.toggle"]),
                row("Record over the selection", &["record.over"]),
                row("Discard the last take", &["take.discard-last"]),
                row("Cancel countdown or recording", &["cancel"]),
            ],
        ),
        (
            "Output",
            &[
                row("Save frame as PNG", &["frame.save"]),
                row("Export\u{2026}", &["export"]),
            ],
        ),
        (NIRI_GROUP, &[]),
        (
            WINDOW_GROUP,
            &[
                row(
                    "Focus next / previous region",
                    &["region.next", "region.prev"],
                ),
                row("Quit", &["app.quit"]),
            ],
        ),
    ],
    &[
        (
            "Playback",
            &[
                row("Play / pause", &["play.toggle"]),
                row(
                    "Shuttle reverse / stop / forward",
                    &["shuttle.reverse", "shuttle.stop", "shuttle.forward"],
                ),
                row(
                    "Previous / next frame",
                    &["seek.prev-frame", "seek.next-frame"],
                ),
                row("Back / forward 1 s", &["seek.back", "seek.forward"]),
                row(
                    "Previous / next edit point",
                    &["seek.prev-edit", "seek.next-edit"],
                ),
                row("Timeline start / end", &["seek.start", "seek.end"]),
            ],
        ),
        (
            "View",
            &[
                row("Zoom timeline in / out", &["zoom.in", "zoom.out"]),
                row("Zoom to fit", &["zoom.fit"]),
                row("Zoom to selection", &["zoom.selection"]),
                row("Toggle snapping", &["snapping.toggle"]),
                row("Switch Live / Program", &["canvas.mode"]),
                row("Toggle bin", &["bin.toggle"]),
                row("Keyboard shortcuts", &["keys.help"]),
                row("Command palette", &["palette.open"]),
                row("Settings", &["settings.open"]),
            ],
        ),
    ],
    &[
        (
            "Edit",
            &[
                row(
                    "Select previous / next clip",
                    &["select.prev", "select.next"],
                ),
                row(
                    "Move selection left / right",
                    &["clip.move-left", "clip.move-right"],
                ),
                row("Split at playhead", &["clip.split"]),
                row("Trim start to playhead", &["clip.trim-start"]),
                row("Trim end to playhead", &["clip.trim-end"]),
                row(
                    "Nudge trim edge 1 frame (Shift: 10)",
                    &["clip.nudge-start", "clip.nudge-end"],
                ),
                row("Remove selected clips", &["clip.remove"]),
                row("Autocut selection or all", &["autocut.run"]),
                row("Autocut settings and preview", &["autocut.preview"]),
                row("Undo", &["history.undo"]),
                row("Redo", &["history.redo"]),
                row("Select all", &["select.all"]),
            ],
        ),
        (
            "Canvas",
            &[row(
                "Nudge layer 1 px (Shift: 10)",
                &[
                    "layer.nudge-left",
                    "layer.nudge-right",
                    "layer.nudge-up",
                    "layer.nudge-down",
                ],
            )],
        ),
    ],
];

/// The rows of Global (niri): SPEC §10.2's snippet, as S20 words them.
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

/// The binding ids the sheet shows a row for (every binding but the
/// 10-frame nudges, which "(Shift: 10)" names, and the Canvas nudges,
/// shown by direction).
pub fn listed_ids() -> Vec<&'static str> {
    SHEET
        .iter()
        .flat_map(|column| column.iter())
        .flat_map(|(_, rows)| rows.iter())
        .flat_map(|r| r.ids.iter().copied())
        .collect()
}

/// The sheet for `search` (empty: everything); `keys_path` is `keys.toml`
/// as people read it (`~/.config/qrec/keys.toml`).
pub fn cheat_sheet(keymap: &Keymap, keys_path: &str, search: &str) -> CheatSheet {
    let words = search_words(search);
    let mut count = 0;
    let mut columns: Vec<Vec<CheatGroup>> = SHEET
        .iter()
        .map(|groups| {
            groups
                .iter()
                .filter_map(|(title, rows)| {
                    let mut rows: Vec<CheatRow> = if *title == NIRI_GROUP {
                        NIRI_ROWS
                            .iter()
                            .map(|(id, label, caps)| CheatRow {
                                id: (*id).to_owned(),
                                label: (*label).to_owned(),
                                matched: Vec::new(),
                                keys: vec![RowKeys {
                                    id: (*id).to_owned(),
                                    chords: vec![caps.iter().map(|c| (*c).to_owned()).collect()],
                                }],
                                default_keys: None,
                            })
                            .collect()
                    } else {
                        rows.iter().map(|r| sheet_row(keymap, r)).collect()
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
                        title: (*title).to_owned(),
                        snippet: (*title == NIRI_GROUP).then(niri_snippet),
                        rows,
                    })
                })
                .collect()
        })
        .collect();
    let searching = !words.is_empty();
    if searching {
        // S20b: the groups left, one column, in reading order.
        columns = vec![columns.into_iter().flatten().collect()];
    }
    let shown = search.trim();
    let first_keys = |id: &str| {
        keymap
            .binding(id)
            .and_then(|b| usable_keys(keymap, b).first().map(|k| k.cap_legends()))
            .unwrap_or_default()
    };
    CheatSheet {
        title: "Keyboard shortcuts".to_owned(),
        placeholder: "Search shortcuts".to_owned(),
        columns,
        matches: searching.then(|| match count {
            0 => format!("No shortcuts match \u{201c}{shown}\u{201d}"),
            1 => format!("1 shortcut matches \u{201c}{shown}\u{201d}"),
            n => format!("{n} shortcuts match \u{201c}{shown}\u{201d}"),
        }),
        palette_tip: searching.then(|| KeyHint {
            label: "To run a command by name, open the palette".to_owned(),
            keys: first_keys("palette.open"),
        }),
        footer: format!("Keys come from {keys_path}. Change them in Settings \u{203a} Keyboard."),
        footer_path: keys_path.to_owned(),
        settings: KeyHint {
            label: "Settings".to_owned(),
            keys: first_keys("settings.open"),
        },
    }
}

fn niri_snippet() -> Snippet {
    Snippet {
        note: "Bind these in niri to record while another app has focus.".to_owned(),
        button: "Copy niri snippet".to_owned(),
        text: NIRI_SNIPPET.to_owned(),
        paste_into: "Paste into".to_owned(),
        path: "~/.config/niri/config.kdl".to_owned(),
    }
}

/// A row of the table with its bindings' keys that run.
fn sheet_row(keymap: &Keymap, row: &Row) -> CheatRow {
    let bindings: Vec<&Binding> = row.ids.iter().filter_map(|id| keymap.binding(id)).collect();
    CheatRow {
        id: row.ids.first().copied().unwrap_or_default().to_owned(),
        label: row.label.to_owned(),
        matched: Vec::new(),
        keys: bindings
            .iter()
            .map(|b| RowKeys {
                id: b.id.to_owned(),
                chords: usable_keys(keymap, b)
                    .into_iter()
                    .map(KeyChord::cap_legends)
                    .collect(),
            })
            .collect(),
        default_keys: rebound_row_note(&bindings),
    }
}

/// "default P" for a row whose bindings `keys.toml` changed: each
/// binding's default keys as [`rebound_note`] names them, "/" apart.
fn rebound_row_note(bindings: &[&Binding]) -> Option<String> {
    if !bindings.iter().any(|b| is_rebound(b)) {
        return None;
    }
    let parts: Vec<String> = bindings
        .iter()
        .filter_map(|b| default_keys(b.id))
        .map(|keys| keys.iter().map(key_name).collect::<Vec<_>>().join(", "))
        .collect();
    Some(format!("default {}", parts.join(" / ")))
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

    fn caps(row: &CheatRow) -> String {
        row.keys
            .iter()
            .map(|part| {
                part.chords
                    .iter()
                    .map(|chord| chord.iter().map(|c| format!("[{c}]")).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(" or ")
            })
            .collect::<Vec<_>>()
            .join(" / ")
    }

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
                    let _ = writeln!(
                        out,
                        "{:<40} {}{}",
                        bracketed(&row.label, &row.matched),
                        caps(row),
                        row.default_keys
                            .as_ref()
                            .map(|d| format!("  ({d})"))
                            .unwrap_or_default()
                    );
                }
                if let Some(snippet) = &group.snippet {
                    let _ = writeln!(
                        out,
                        "{} <{}> {} {}",
                        snippet.note, snippet.button, snippet.paste_into, snippet.path
                    );
                }
            }
        }
        if let Some(tip) = &sheet.palette_tip {
            let _ = writeln!(out, "{} {:?}", tip.label, tip.keys);
        }
        let _ = writeln!(
            out,
            "-- {} | {:?} {}",
            sheet.footer, sheet.settings.keys, sheet.settings.label
        );
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
                vec!["Record", "Output", "Global (niri)", "Window"],
                vec!["Playback", "View"],
                vec!["Edit", "Canvas"],
            ],
            "D-22: Canvas at the bottom of column 3"
        );
        let output = &sheet.columns[0][1].rows;
        let save = output.iter().find(|r| r.id == "frame.save").unwrap();
        assert_eq!(save.label, "Save frame as PNG");
        assert_eq!(save.keys[0].chords, [vec!["F".to_owned()]]);
        assert_eq!(save.default_keys.as_deref(), Some("default P"));
        let niri = &sheet.columns[0][2];
        assert_eq!(niri.rows[0].keys[0].chords, [vec!["Mod", "Shift", "R"]]);
        assert_eq!(niri.snippet.as_ref().unwrap().text, NIRI_SNIPPET);
        assert!(sheet.matches.is_none() && sheet.palette_tip.is_none());
        assert_eq!(
            sheet.footer,
            "Keys come from ~/.config/qrec/keys.toml. Change them in Settings › Keyboard."
        );
        assert_eq!(sheet.settings.keys, ["Ctrl", ","]);
        // S20's pairs: J / K / L, Del or X (or Backspace), Ctrl K or :.
        let all: Vec<&CheatRow> = sheet
            .columns
            .iter()
            .flatten()
            .flat_map(|g| &g.rows)
            .collect();
        let row = |id: &str| *all.iter().find(|r| r.id == id).unwrap();
        assert_eq!(caps(row("shuttle.reverse")), "[J] / [K] / [L]");
        assert_eq!(caps(row("clip.remove")), "[Del] or [X] or [Backspace]");
        assert_eq!(caps(row("palette.open")), "[Ctrl][K] or [:]");
        assert_eq!(caps(row("clip.nudge-start")), "[Alt][[] / [Alt][]]");
    }

    /// Every binding has its row, once: but the 10-step nudges, which the
    /// 1-step rows' "(Shift: 10)" names (S20's trim row; the Canvas row in
    /// its style: four Shift combos don't fit a column).
    #[test]
    fn every_binding_is_on_the_sheet() {
        let listed = listed_ids();
        for b in &resolve("").bindings {
            let n = listed.iter().filter(|id| **id == b.id).count();
            let expected = usize::from(!b.id.ends_with("-10"));
            assert_eq!(n, expected, "{}", b.id);
        }
    }

    #[test]
    fn snapshot_s20b_search() {
        let sheet = cheat_sheet(&resolve(""), KEYS, " trim ");
        insta::assert_snapshot!("cheatsheet_s20b_trim", text(&sheet));
        assert_eq!(
            sheet.matches.as_deref(),
            Some("3 shortcuts match \u{201c}trim\u{201d}"),
            "S20b"
        );
        assert_eq!(sheet.columns.len(), 1, "the results in one column");
        let edit = &sheet.columns[0][0];
        assert_eq!(edit.title, "Edit");
        let labels: Vec<&str> = edit.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Trim start to playhead",
                "Trim end to playhead",
                "Nudge trim edge 1 frame (Shift: 10)"
            ]
        );
        assert_eq!(edit.rows[0].matched, [0, 1, 2, 3]);
        assert_eq!(edit.rows[2].matched, [6, 7, 8, 9]);
        let tip = sheet.palette_tip.as_ref().unwrap();
        assert_eq!(tip.label, "To run a command by name, open the palette");
        assert_eq!(tip.keys, ["Ctrl", "K"]);
        // Every word, anywhere in the label; the niri rows too.
        let sheet = cheat_sheet(&resolve(""), KEYS, "end trim");
        assert_eq!(
            sheet.matches.as_deref(),
            Some("1 shortcut matches \u{201c}end trim\u{201d}")
        );
        let sheet = cheat_sheet(&resolve(""), KEYS, "discard");
        let groups: Vec<&str> = sheet.columns[0].iter().map(|g| g.title.as_str()).collect();
        assert_eq!(groups, ["Record", NIRI_GROUP]);
        assert!(sheet.columns[0][1].snippet.is_some());
        let none = cheat_sheet(&resolve(""), KEYS, "xyzzy");
        assert_eq!(
            none.matches.as_deref(),
            Some("No shortcuts match \u{201c}xyzzy\u{201d}")
        );
        assert!(none.columns[0].is_empty());
    }

    /// Keys that run show; a conflicting one doesn't (it never runs), an
    /// unbound binding shows none and its defaults.
    #[test]
    fn rows_follow_keys_toml() {
        let sheet = cheat_sheet(
            &resolve(
                "[bindings]\nautocut.run = \"x\"\nrecord.over = []\nseek.prev-frame = \"h\"\n",
            ),
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
        assert!(row("autocut.run").keys[0].chords.is_empty());
        assert_eq!(
            row("autocut.run").default_keys.as_deref(),
            Some("default C")
        );
        assert_eq!(
            caps(row("clip.remove")),
            "[Del] or [Backspace]",
            "x drops out"
        );
        assert_eq!(row("clip.remove").default_keys, None);
        assert!(row("record.over").keys[0].chords.is_empty());
        assert_eq!(
            row("record.over").default_keys.as_deref(),
            Some("default O")
        );
        assert_eq!(caps(row("seek.prev-frame")), "[H] / [→]");
        assert_eq!(
            row("seek.prev-frame").default_keys.as_deref(),
            Some("default ← / →")
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
