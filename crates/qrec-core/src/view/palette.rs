//! The command palette (G§4.11, S19, S19b, C34; SPEC §10.1 layer 3).
//! Owner: WP9.
//!
//! Rows are every binding (`keymap::GROUPS` order, the table order inside
//! a group) plus the keyless actions (`keymap::KEYLESS`: "Clear undo
//! history" after Redo, D-19), each with its key caps (the keys that run:
//! conflicting ones drop out, `keymap::usable_keys`), its CLI form and
//! whether it can run now, with the reason when it can't ("Needs a
//! contiguous selection"). Running a row is what its key does
//! (`KeyRouter::run_binding`), so a row is enabled by the rule its key
//! follows: `keymap::action_for`, and for the ids the window handles
//! itself the hint bar's rules (G§4.3: `export` opens the export panel,
//! `autocut.preview` the autocut page, `zoom.selection` needs a selection,
//! `zoom.fit` clips; `zoom.in` and `zoom.out` are always offered, the
//! palette doesn't know the zoom).
//!
//! An empty query shows "Recent" (the last three ids run this session,
//! newest first), then every row under its group: Record, Playback, Edit,
//! Output, View, Canvas (BRIEF S19's empty-query variant; the Canvas group
//! is the layer nudges, D-22). A query shows one untitled group of the
//! rows whose label holds it as a subsequence ([`fuzzy_match`]: case
//! doesn't matter, spaces in the query are ignored, consecutive characters
//! and word starts score higher), best first, ties in the table order.
//! Labels and CLI forms are the core keymap's until S19's frame is drawn
//! into the build (D-31).

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::keymap::{
    Action, GROUPS, KEYLESS, KeyChord, Keymap, action_for, gate_reason, usable_keys,
};
use crate::protocol::command::{AutocutOverrides, ClipScope, Command};
use crate::state::State;
use crate::view::export::panel_enabled;

/// The palette's content for a query.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteView {
    pub groups: Vec<PaletteGroup>,
}

/// A group of rows ("Recent", "Record", …; one unnamed group for a query).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteGroup {
    pub title: Option<String>,
    pub rows: Vec<PaletteItem>,
}

/// One row (C34).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteItem {
    pub id: String,
    pub label: String,
    /// Char indices of `label` the query matched (bold).
    pub matched: Vec<usize>,
    /// Key-cap legends per chord.
    pub keys: Vec<Vec<String>>,
    /// The short CLI form.
    pub cli: Option<String>,
    pub enabled: bool,
    pub reason: Option<String>,
}

impl PaletteItem {
    /// The byte ranges of `label` to draw bold: the runs of `matched`
    /// (Pango's attributes take bytes).
    pub fn highlights(&self) -> Vec<Range<usize>> {
        highlight_ranges(&self.label, &self.matched)
    }
}

/// How many "Recent" rows an empty query shows.
pub const RECENT: usize = 3;

/// The palette for `query`; `recent` are the ids run this session, newest
/// first.
pub fn palette(keymap: &Keymap, state: &State, query: &str, recent: &[String]) -> PaletteView {
    let rows = rows(keymap, state);
    let needle: String = query.chars().filter(|c| !c.is_whitespace()).collect();
    if needle.is_empty() {
        let mut groups = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        let recent_rows: Vec<PaletteItem> = recent
            .iter()
            .filter(|id| {
                let new = !seen.contains(&id.as_str());
                seen.push(id);
                new
            })
            .filter_map(|id| rows.iter().find(|(_, r)| &r.id == id))
            .take(RECENT)
            .map(|(_, r)| r.clone())
            .collect();
        if !recent_rows.is_empty() {
            groups.push(PaletteGroup {
                title: Some("Recent".to_owned()),
                rows: recent_rows,
            });
        }
        for group in GROUPS {
            let in_group: Vec<PaletteItem> = rows
                .iter()
                .filter(|(g, _)| *g == group)
                .map(|(_, r)| r.clone())
                .collect();
            if !in_group.is_empty() {
                groups.push(PaletteGroup {
                    title: Some(group.to_owned()),
                    rows: in_group,
                });
            }
        }
        return PaletteView { groups };
    }
    let mut found: Vec<(u32, usize, PaletteItem)> = rows
        .into_iter()
        .enumerate()
        .filter_map(|(order, (_, mut row))| {
            let (score, matched) = fuzzy_match(&needle, &row.label)?;
            row.matched = matched;
            Some((score, order, row))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let rows: Vec<PaletteItem> = found.into_iter().map(|(_, _, row)| row).collect();
    PaletteView {
        groups: if rows.is_empty() {
            Vec::new()
        } else {
            vec![PaletteGroup { title: None, rows }]
        },
    }
}

/// Every row with its group, in the order an empty query lists them.
fn rows(keymap: &Keymap, state: &State) -> Vec<(&'static str, PaletteItem)> {
    let mut out = Vec::new();
    for group in GROUPS {
        for b in keymap.bindings.iter().filter(|b| b.group == group) {
            out.push((
                b.group,
                item(
                    b.id,
                    b.label,
                    &usable_keys(keymap, b),
                    b.cli,
                    availability(b.id, state),
                ),
            ));
            for keyless in KEYLESS.iter().filter(|k| k.after == b.id) {
                out.push((
                    keyless.group,
                    item(
                        keyless.id,
                        keyless.label,
                        &[],
                        keyless.cli,
                        availability(keyless.id, state),
                    ),
                ));
            }
        }
    }
    out
}

fn item(
    id: &str,
    label: &str,
    keys: &[&KeyChord],
    cli: Option<&str>,
    available: Result<(), String>,
) -> PaletteItem {
    PaletteItem {
        id: id.to_owned(),
        label: label.to_owned(),
        matched: Vec::new(),
        keys: keys.iter().map(|k| k.cap_legends()).collect(),
        cli: cli.map(str::to_owned),
        enabled: available.is_ok(),
        reason: available.err(),
    }
}

/// Whether row `id` can run in `state`, and why not: the rule its key
/// follows (the module docs).
pub fn availability(id: &str, state: &State) -> Result<(), String> {
    let clips = !state.project.timeline.clips.is_empty();
    let empty = || Err("The timeline is empty".to_owned());
    match id {
        "export" => panel_enabled(state),
        "autocut.preview" if !clips => empty(),
        "autocut.preview" => {
            let page = Command::Autocut {
                scope: ClipScope::All,
                overrides: AutocutOverrides::default(),
                dry_run: true,
            };
            match gate_reason(state, &page) {
                Some(why) => Err(why),
                None => Ok(()),
            }
        }
        "zoom.selection" if state.ui.selection.is_empty() => {
            Err("Select the clips to zoom to".to_owned())
        }
        "zoom.fit" if !clips => empty(),
        _ => match action_for(id, state) {
            Some(Action::Run(_) | Action::Gui) => Ok(()),
            Some(Action::Unavailable(why)) => Err(why),
            None => Err(format!("Unknown command {id}")),
        },
    }
}

/// The fuzzy match of `query` in `text`: a score (higher is better) and
/// the matched char indices; `None` when `query` isn't a subsequence.
///
/// Case doesn't matter. Each matched character scores 1, and more where
/// it lands well: 5 for following the previous match, 4 for starting a
/// word (the text's start or after anything but a letter or digit), 2 more
/// for the text's first character. Of the alignments the best scoring one
/// wins (the earliest among equals).
pub fn fuzzy_match(query: &str, text: &str) -> Option<(u32, Vec<usize>)> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().collect();
    let lower: Vec<char> = t
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    if q.len() > t.len() {
        return None;
    }
    let word_start = |j: usize| j == 0 || !t[j - 1].is_alphanumeric();
    let own = |j: usize| 1 + if word_start(j) { 4 } else { 0 } + if j == 0 { 2 } else { 0 };
    // best[i][j]: the best score of q[..=i] with q[i] on t[j]; from[i][j]
    // where q[i - 1] went.
    let n = t.len();
    let mut best: Vec<Vec<Option<u32>>> = vec![vec![None; n]; q.len()];
    let mut from: Vec<Vec<usize>> = vec![vec![0; n]; q.len()];
    for j in 0..n {
        if lower[j] == q[0] {
            best[0][j] = Some(own(j));
        }
    }
    for i in 1..q.len() {
        // The best of row i - 1 so far, and where (earliest on ties).
        let mut running: Option<(u32, usize)> = None;
        for j in 0..n {
            if j > 0
                && let Some(prev) = best[i - 1][j - 1]
                && running.is_none_or(|(s, _)| prev > s)
            {
                running = Some((prev, j - 1));
            }
            if lower[j] != q[i] {
                continue;
            }
            let Some((prev, at)) = running else {
                continue;
            };
            // Right after the previous match beats the running best when
            // the consecutive bonus makes up for it.
            let mut score = prev + own(j);
            let mut came = at;
            if let Some(adjacent) = best[i - 1][j - 1]
                && adjacent + own(j) + 5 > score
            {
                score = adjacent + own(j) + 5;
                came = j - 1;
            }
            best[i][j] = Some(score);
            from[i][j] = came;
        }
    }
    let last = q.len() - 1;
    let (score, mut j) = (0..n).filter_map(|j| best[last][j].map(|s| (s, j))).fold(
        None,
        |acc: Option<(u32, usize)>, (s, j)| match acc {
            Some((best, _)) if best >= s => acc,
            _ => Some((s, j)),
        },
    )?;
    let mut matched = vec![j];
    for i in (1..q.len()).rev() {
        j = from[i][j];
        matched.push(j);
    }
    matched.reverse();
    Some((score, matched))
}

/// The byte ranges of `text` the char indices `matched` cover, one per run
/// of consecutive indices: what a search draws bold (the palette, the
/// cheat sheet, the Keyboard page).
pub fn highlight_ranges(text: &str, matched: &[usize]) -> Vec<Range<usize>> {
    let bytes: Vec<(usize, usize)> = text
        .char_indices()
        .map(|(at, c)| (at, at + c.len_utf8()))
        .collect();
    let mut out: Vec<Range<usize>> = Vec::new();
    for &i in matched {
        let Some(&(start, end)) = bytes.get(i) else {
            continue;
        };
        match out.last_mut() {
            Some(run) if run.end == start => run.end = end,
            _ => out.push(start..end),
        }
    }
    out
}

/// `label` with its matched characters in brackets: "[Sp]lit clip" (for
/// snapshots and logs).
pub fn bracketed(label: &str, matched: &[usize]) -> String {
    let mut out = String::new();
    let mut open = false;
    for (i, c) in label.chars().enumerate() {
        let on = matched.contains(&i);
        if on && !open {
            out.push('[');
        }
        if !on && open {
            out.push(']');
        }
        open = on;
        out.push(c);
    }
    if open {
        out.push(']');
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::sync::Arc;

    use super::*;
    use crate::history::HistoryEntry;
    use crate::ids::{ClipId, JobId, TakeId};
    use crate::keymap::resolve;
    use crate::state::{CanvasMode, Job, JobKind, RecordMode, RecordPhase};
    use crate::testkit;

    fn ids(list: &[&str]) -> Vec<ClipId> {
        list.iter().map(|s| ClipId::from_raw(*s)).collect()
    }

    /// The design state with `n` undo steps (BRIEF S18's clear-history
    /// note says 128).
    fn with_history(mut state: State, n: u64) -> State {
        state.history.entries = Arc::new(
            (1..=n)
                .map(|seq| HistoryEntry {
                    seq,
                    ts: state.project.created_at,
                    label: format!("Edit {seq}"),
                    cmd: serde_json::Value::Null,
                    forward: json_patch::Patch(vec![]),
                    inverse: json_patch::Patch(vec![]),
                    after_hash: String::new(),
                })
                .collect(),
        );
        state.history.cursor = n;
        state.history.next_seq = n + 1;
        state
    }

    /// S06: Program, c2d0 selected (S19's base), with a history.
    fn s06() -> State {
        let mut state = with_history(testkit::design_state(), 128);
        state.ui.canvas_mode = CanvasMode::Program;
        state.ui.selection = ids(&["c2d0"]);
        state
    }

    /// The view as text: each group, then its rows with their caps, the
    /// matched characters in brackets, the CLI form and why a row is off.
    fn text(view: &PaletteView) -> String {
        let mut out = String::new();
        for group in &view.groups {
            let _ = writeln!(out, "## {}", group.title.as_deref().unwrap_or("(query)"));
            for row in &group.rows {
                let caps: Vec<String> = row
                    .keys
                    .iter()
                    .map(|chord| chord.iter().map(|c| format!("[{c}]")).collect::<String>())
                    .collect();
                let _ = write!(
                    out,
                    "{:<44} {:<16} {}",
                    bracketed(&row.label, &row.matched),
                    caps.join(" "),
                    row.cli.as_deref().unwrap_or("—")
                );
                if let Some(why) = &row.reason {
                    let _ = write!(out, "  (off: {why})");
                }
                out.push('\n');
            }
        }
        out
    }

    #[test]
    fn snapshot_s19_split_query() {
        let view = palette(&resolve(""), &s06(), "sp", &[]);
        insta::assert_snapshot!("palette_s19_sp", text(&view));
        let rows = &view.groups[0].rows;
        assert_eq!(rows[0].id, "clip.split", "the best match first");
        assert_eq!(rows[0].matched, [0, 1]);
        assert_eq!(rows[0].highlights(), vec![0..2]);
        assert!(view.groups[0].title.is_none());
    }

    #[test]
    fn snapshot_empty_query_recent_then_groups() {
        let recent: Vec<String> = [
            "frame.save",
            "clip.split",
            "frame.save",
            "nope",
            "export",
            "undo",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let view = palette(&resolve(""), &s06(), "", &recent);
        insta::assert_snapshot!("palette_empty_query", text(&view));
        let titles: Vec<&str> = view
            .groups
            .iter()
            .map(|g| g.title.as_deref().unwrap())
            .collect();
        assert_eq!(
            titles,
            [
                "Recent", "Record", "Playback", "Edit", "Output", "View", "Canvas"
            ]
        );
        let recent: Vec<&str> = view.groups[0].rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            recent,
            ["frame.save", "clip.split", "export"],
            "three, once each"
        );
        // The keyless "Clear undo history" comes after Redo.
        let edit = &view.groups[3].rows;
        let redo = edit.iter().position(|r| r.id == "history.redo").unwrap();
        assert_eq!(edit[redo + 1].id, "history.clear");
        assert_eq!(edit[redo + 1].label, "Clear undo history");
        assert!(edit[redo + 1].keys.is_empty());
        assert_eq!(
            edit[redo + 1].cli.as_deref(),
            Some("qrec history clear --yes")
        );
        let none = palette(&resolve(""), &s06(), "  ", &[]);
        assert_eq!(none.groups.len(), 6, "no Recent group without one");
    }

    /// S19's disabled variant: c3e9 and c7f3 aren't side by side.
    #[test]
    fn snapshot_record_over_needs_a_contiguous_selection() {
        let mut state = s06();
        state.ui.selection = ids(&["c3e9", "c7f3"]);
        let view = palette(&resolve(""), &state, "over", &[]);
        insta::assert_snapshot!("palette_s19_disabled", text(&view));
        let over = view.groups[0]
            .rows
            .iter()
            .find(|r| r.id == "record.over")
            .unwrap();
        assert!(!over.enabled);
        assert_eq!(over.reason.as_deref(), Some("Needs a contiguous selection"));
    }

    #[test]
    fn clearing_the_history_is_a_row_with_its_reasons() {
        let row = |state: &State| {
            palette(&resolve(""), state, "clear undo", &[]).groups[0].rows[0].clone()
        };
        let on = row(&s06());
        assert_eq!(on.id, "history.clear");
        assert!(on.enabled, "{on:?}");
        assert_eq!(bracketed(&on.label, &on.matched), "[Clear] [undo] history");
        let empty = row(&testkit::design_state());
        assert_eq!(empty.reason.as_deref(), Some("The undo history is empty"));
        let mut recording = s06();
        recording.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        assert_eq!(
            row(&recording).reason.as_deref(),
            Some("Not while recording")
        );
    }

    /// The rows the window handles itself follow the hint bar's rules
    /// (G§4.3), not `action_for`'s commands.
    #[test]
    fn gui_rows_follow_the_windows_rules() {
        let state = s06();
        assert_eq!(availability("export", &state), Ok(()));
        assert_eq!(availability("autocut.preview", &state), Ok(()));
        assert_eq!(availability("zoom.selection", &state), Ok(()));
        assert_eq!(availability("zoom.in", &state), Ok(()));
        let mut nothing = state.clone();
        nothing.ui.selection.clear();
        assert_eq!(
            availability("zoom.selection", &nothing),
            Err("Select the clips to zoom to".into())
        );
        // An export running: the panel opens (on the running export) and
        // recording waits.
        let mut exporting = state.clone();
        exporting.runtime.jobs.insert(
            JobId(1),
            Job {
                id: JobId(1),
                kind: JobKind::Export {
                    out: "exports/x.mp4".into(),
                    files: Default::default(),
                    total_frames: 7391,
                    seg_frames: vec![7391],
                    seg_done: vec![0],
                    seg_clips: Vec::new(),
                    stitch_total_ns: 1,
                    stitch_done_ns: 0,
                    stitching: false,
                },
                req: None,
                started: exporting.project.created_at,
            },
        );
        assert_eq!(availability("export", &exporting), Ok(()));
        assert_eq!(
            availability("record.toggle", &exporting),
            Err("Wait for the export to finish".into())
        );
        let mut empty = testkit::state(crate::testkit::ProjectBuilder::new().build());
        empty.runtime.host = crate::state::HostKind::Gui;
        for id in ["export", "autocut.preview", "zoom.fit"] {
            assert_eq!(
                availability(id, &empty),
                Err("The timeline is empty".into()),
                "{id}"
            );
        }
        let mut counting = state.clone();
        counting.runtime.record = RecordPhase::Countdown {
            remaining: 3,
            mode: RecordMode::Append,
            timer: crate::ids::TimerId(1),
            start_req: None,
            wait: false,
        };
        assert_eq!(
            availability("autocut.preview", &counting),
            Err("Not during the countdown".into())
        );
        assert!(availability("bogus", &state).is_err());
    }

    /// Keys follow `keys.toml`: rebound keys show, conflicting ones drop
    /// out (they never run), unbound rows have none.
    #[test]
    fn rows_show_the_keys_that_run() {
        let map =
            resolve("[bindings]\nframe.save = \"f\"\nautocut.run = \"x\"\nrecord.over = []\n");
        let view = palette(&map, &s06(), "", &[]);
        let row = |id: &str| {
            view.groups
                .iter()
                .flat_map(|g| &g.rows)
                .find(|r| r.id == id)
                .unwrap()
                .clone()
        };
        assert_eq!(row("frame.save").keys, [vec!["F".to_owned()]]);
        assert_eq!(
            row("clip.remove").keys,
            [vec!["Del".to_owned()], vec!["Backspace".to_owned()]]
        );
        assert!(row("autocut.run").keys.is_empty());
        assert!(row("record.over").keys.is_empty());
        assert_eq!(row("history.redo").keys.len(), 2);
        assert_eq!(row("history.redo").keys[0], ["Ctrl", "Shift", "Z"]);
        assert!(palette(&map, &s06(), "zzzzzz", &[]).groups.is_empty());
    }

    /// The states the dispatch table runs in: S02 (Live, nothing
    /// selected), S06 (Program, c2d0), a layer selected on the canvas, a
    /// take recording; all with a history.
    fn dispatch_states() -> Vec<(&'static str, State)> {
        let s02 = with_history(testkit::design_state(), 128);
        let mut layer = s02.clone();
        layer.ui.selected_layer = Some(crate::ids::LayerId::numbered(2));
        let mut recording = s06();
        recording.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 2_000_000_000,
            waiters: vec![],
        };
        vec![
            ("S02", s02),
            ("S06", s06()),
            ("layer", layer),
            ("recording", recording),
        ]
    }

    /// The ids whose key the window takes over (G§3.8 step 10): they run
    /// a GUI action, not `action_for`'s command.
    const WINDOW_RUNS: [&str; 3] = ["export", "autocut.preview", "zoom.fit"];

    /// The dispatch table (WP9): every enabled row runs. A row whose key
    /// dispatches a command (`action_for`'s `Run`, the path a key takes)
    /// has one `apply` takes in that state: it acts, or asks for the
    /// confirmation (only "Clear undo history"); the window runs the
    /// others (`Gui`, and the ids it takes over). A disabled row's command
    /// is refused, or isn't one: its reason holds.
    #[test]
    fn every_enabled_row_runs() {
        let ctx = testkit::ctx(testkit::CREATED_AT, 7);
        let mut ran = 0;
        for (name, state) in dispatch_states() {
            let view = palette(&resolve(""), &state, "", &[]);
            for row in view
                .groups
                .iter()
                .filter(|g| g.title.as_deref() != Some("Recent"))
                .flat_map(|g| &g.rows)
            {
                let action = action_for(&row.id, &state);
                if !row.enabled {
                    assert!(
                        !matches!(action, Some(Action::Run(_) | Action::Gui))
                            || WINDOW_RUNS.contains(&row.id.as_str())
                            || row.id == "zoom.selection",
                        "{name}: {} is off ({:?}) but {action:?}",
                        row.id,
                        row.reason
                    );
                    continue;
                }
                match action {
                    Some(Action::Run(_)) if WINDOW_RUNS.contains(&row.id.as_str()) => {}
                    Some(Action::Run(cmd)) => {
                        ran += 1;
                        match crate::apply(&state, &ctx, cmd.clone()) {
                            Ok(_) => {}
                            Err(e) if e.code == crate::ErrorCode::ConfirmationRequired => {
                                assert_eq!(row.id, "history.clear", "{name}: {e}");
                            }
                            Err(e) => panic!("{name}: {} is on but {cmd:?} fails: {e}", row.id),
                        }
                    }
                    Some(Action::Gui) => {}
                    other => panic!("{name}: {} is on but {other:?}", row.id),
                }
            }
        }
        assert!(ran > 40, "{ran} commands ran");
    }

    /// "Clear undo history" → the confirmation (SPEC §11's modal, with
    /// G§4.9's copy) → confirmed, the history is gone and the row is off.
    #[test]
    fn clearing_the_history_asks_then_clears() {
        let state = s06();
        let Some(Action::Run(cmd)) = action_for("history.clear", &state) else {
            panic!("history.clear doesn't run");
        };
        assert_eq!(cmd, Command::HistoryClear);
        let mut ctx = testkit::ctx(testkit::CREATED_AT, 7);
        let err = crate::apply(&state, &ctx, cmd.clone()).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::ConfirmationRequired);
        let modal = crate::view::confirm::modal(
            &cmd,
            &err,
            &state,
            &crate::view::confirm::ConfirmFacts::default(),
        );
        assert_eq!(modal.title, "Get out of my swamp?");
        assert_eq!(modal.lead, "Clear undo history?");
        assert_eq!(
            modal.body.as_deref(),
            Some(
                "This removes 128 undo steps from .qrec/history.jsonl. The timeline stays as \
                 it is, but you can't undo anything done before now."
            )
        );
        assert_eq!(
            (modal.cancel.as_str(), modal.confirm.as_str()),
            ("Cancel", "Clear history")
        );
        ctx.confirmed = true;
        let out = crate::apply(&state, &ctx, cmd).unwrap();
        assert!(out.state.history.entries.is_empty());
        assert_eq!(
            out.state.project.timeline, state.project.timeline,
            "the timeline stays"
        );
        let row = palette(&resolve(""), &out.state, "clear undo", &[]).groups[0].rows[0].clone();
        assert_eq!(row.reason.as_deref(), Some("The undo history is empty"));
    }

    #[test]
    fn fuzzy_matching() {
        assert_eq!(fuzzy_match("", "x"), Some((0, vec![])));
        assert_eq!(fuzzy_match("ab", "a"), None);
        assert_eq!(fuzzy_match("xz", "Split clip"), None);
        // Word starts and runs beat scattered letters.
        let (split, at) = fuzzy_match("sp", "Split clip at the playhead").unwrap();
        assert_eq!(at, [0, 1]);
        let (snapping, at) = fuzzy_match("sp", "Toggle snapping").unwrap();
        assert_eq!(at, [7, 10]);
        assert!(split > snapping);
        // The best alignment, not the first one found: "pl" in "playhead"
        // over the "p" of "split" and the "l" of "clip".
        let (_, at) = fuzzy_match("pla", "Split clip at the playhead").unwrap();
        assert_eq!(at, [18, 19, 20]);
        assert_eq!(fuzzy_match("SPL", "split").unwrap().1, [0, 1, 2]);
        // Not ASCII: char indices.
        let (_, at) = fuzzy_match("sel", "Move selection left one slot").unwrap();
        assert_eq!(at, [5, 6, 7]);
        assert_eq!(fuzzy_match("éa", "Café au lait").unwrap().1, [3, 5]);
        assert_eq!(highlight_ranges("Café au lait", &[3, 5]), [3..5, 6..7]);
        assert_eq!(highlight_ranges("abc", &[0, 1, 2, 9]), vec![0..3]);
        assert_eq!(bracketed("Split clip", &[0, 1, 6]), "[Sp]lit [c]lip");
    }

    proptest::proptest! {
        /// A match is a subsequence: its indices rise, each on a character
        /// equal to the query's (any case), as many as the query has.
        #[test]
        fn a_match_is_an_ordered_subsequence(query in "[a-zA-Z ]{0,5}", text in "[a-zA-Z /()]{0,24}") {
            let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
            let needle: String = q.iter().collect();
            if let Some((_, at)) = fuzzy_match(&needle, &text) {
                proptest::prop_assert_eq!(at.len(), q.len());
                proptest::prop_assert!(at.windows(2).all(|w| w[0] < w[1]));
                let chars: Vec<char> = text.chars().collect();
                for (i, j) in at.iter().enumerate() {
                    proptest::prop_assert!(chars[*j].eq_ignore_ascii_case(&q[i]));
                }
            } else {
                // Not a subsequence, then.
                let mut it = text.chars().map(|c| c.to_ascii_lowercase());
                let found = q.iter().all(|c| it.any(|t| t == c.to_ascii_lowercase()));
                proptest::prop_assert!(!found, "{needle:?} is in {text:?}");
            }
        }
    }
}
