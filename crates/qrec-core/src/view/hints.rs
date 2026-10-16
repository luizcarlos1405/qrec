//! The hint bar (G§4.3, C03, D-02). Owner: WP6.
//!
//! [`hint_state`] picks what the bar describes (first match of G§4.3's
//! list), [`hint_bar`] the items for it with the user's keys
//! (`keymap::usable_keys`, cap legends) and the status text ("Snapping on ·
//! 1920×1080 · 30 fps"), and [`hint_enabled`] decides for ids the GUI
//! handles itself. `keymap::hints` stays for `qrec keys` and the agent
//! guide.
//!
//! An item names bindings (their first usable key each, so a rebound key
//! shows and a conflicting one drops out; an item none of whose bindings
//! has a usable key is left out) or literal caps (`[Esc]` inside a modal,
//! `[Shift]` while editing a layer). The first item's label is the bar's
//! `text`, the others `muted` unless emphasized; disabled items show at
//! 0.45 (the GUI's business).

use serde::{Deserialize, Serialize};

use crate::keymap::{Action, KeyChord, Keymap, action_for, usable_keys};
use crate::state::{CanvasMode, JobKind, RecordPhase, State};
use crate::ui_normalize::edit_target;
use crate::view::{
    DragKind, ExportPanelKind, FocusRegion, GuiFacts, HintItem, OverlayKind, PanelPageKind, format,
};

/// What the hint bar describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HintState {
    Modal,
    Palette,
    Settings,
    TrimDrag,
    MoveDrag,
    BinDrag,
    LayerEdit,
    Countdown,
    Recording,
    Finalizing,
    ExportSetup,
    ExportDone,
    ExportFailed,
    AutocutPage,
    Bin,
    FirstRun,
    PortalDenied,
    Empty,
    FrameZoom,
    ClipSelected,
    ProgramIdle,
    LiveIdle,
    /// A blocking page: S23 offers Check again.
    Blocking {
        check_again: bool,
    },
}

/// The zoom from which the timeline shows single frames (G§4.5).
pub const FRAME_LEVEL_PX_PER_FRAME: f64 = 24.0;
/// The timeline's closest zoom (G§4.5).
pub const MAX_PX_PER_FRAME: f64 = 128.0;

/// The state the hint bar describes, first match of G§4.3. (The GUI picks
/// [`HintState::Blocking`] itself: the facts don't know the startup page.)
pub fn hint_state(state: &State, facts: &GuiFacts) -> HintState {
    // 1. The overlay on top (the cheat sheet covers the bar: the state
    //    below shows).
    match facts.top_overlay {
        Some(OverlayKind::Modal) => return HintState::Modal,
        Some(OverlayKind::Palette) => return HintState::Palette,
        Some(OverlayKind::Settings) => return HintState::Settings,
        _ => {}
    }
    // 2. A drag (a scrub keeps the state below).
    match facts.drag {
        Some(DragKind::TrimStart | DragKind::TrimEnd) => return HintState::TrimDrag,
        Some(DragKind::Move) => return HintState::MoveDrag,
        Some(DragKind::BinInsert) => return HintState::BinDrag,
        Some(drag) if drag.is_layer() => return HintState::LayerEdit,
        _ => {}
    }
    // 3. The record phase.
    match &state.runtime.record {
        RecordPhase::Countdown { .. } => return HintState::Countdown,
        RecordPhase::Starting { .. } | RecordPhase::Recording { .. } => {
            return HintState::Recording;
        }
        RecordPhase::Finalizing { .. } => return HintState::Finalizing,
        RecordPhase::Idle => {}
    }
    // 4. The export panel, unless it runs (passive: the state below, with
    //    the export prefix).
    match facts.export_panel {
        Some(ExportPanelKind::Setup) => return HintState::ExportSetup,
        Some(ExportPanelKind::Done) => return HintState::ExportDone,
        Some(ExportPanelKind::Failed) => return HintState::ExportFailed,
        Some(ExportPanelKind::Running) | None => {}
    }
    // 5.–7. The autocut page, the bin, a layer selected on the canvas.
    if facts.panel_page == PanelPageKind::Autocut {
        return HintState::AutocutPage;
    }
    if facts.focus == FocusRegion::Bin {
        return HintState::Bin;
    }
    if facts.focus == FocusRegion::Stage
        && state.ui.selected_layer.is_some()
        && edit_target(&state.project, &state.ui).is_some()
    {
        return HintState::LayerEdit;
    }
    // 8.–13. What the project holds and how it is shown.
    if state.project.scene.is_empty() {
        return HintState::FirstRun;
    }
    // S24 is approved (G§0.2): a refused screen is its own state.
    if facts.portal_denied {
        return HintState::PortalDenied;
    }
    if state.project.timeline.clips.is_empty() {
        return HintState::Empty;
    }
    if facts.px_per_frame >= FRAME_LEVEL_PX_PER_FRAME {
        return HintState::FrameZoom;
    }
    if !state.ui.selection.is_empty() {
        return HintState::ClipSelected;
    }
    match state.ui.canvas_mode {
        CanvasMode::Program => HintState::ProgramIdle,
        CanvasMode::Live => HintState::LiveIdle,
    }
}

/// The items for `hint` and the status text on the right.
pub fn hint_bar(
    hint: &HintState,
    state: &State,
    facts: &GuiFacts,
    keymap: &Keymap,
) -> (Vec<HintItem>, String) {
    let b = Items {
        state,
        facts,
        keymap,
        out: Vec::new(),
    };
    let items = b.build(hint);
    let status = match hint {
        HintState::Blocking { .. } => String::new(),
        _ => status(state),
    };
    (items, status)
}

/// Whether the item of binding `id` is enabled (G§4.3's table): ids the
/// GUI handles itself by their own rule, anything else when
/// `keymap::action_for` isn't `Unavailable`.
pub fn hint_enabled(id: &str, state: &State, facts: &GuiFacts) -> bool {
    let clips = !state.project.timeline.clips.is_empty();
    let idle = state.runtime.record.is_idle();
    match id {
        // The keys of `export` and `autocut.preview` open their GUI (G§3.8
        // step 10), whatever `action_for` says of the command.
        "export.panel" | "export" => exporting(state) || (clips && idle),
        "autocut.page" | "autocut.preview" => clips && idle,
        "zoom.selection" => !state.ui.selection.is_empty(),
        "zoom.in" => facts.px_per_frame < MAX_PX_PER_FRAME,
        "zoom.out" => facts.px_per_frame > facts.fit_px_per_frame,
        "zoom.fit" => clips,
        // D-23: `n` works mid-trim, not mid-move.
        "snapping.toggle" => facts.drag != Some(DragKind::Move),
        // Until WP9 these show "Not built yet" (G§1.1); the window's own.
        "palette.open" | "keys.help" | "settings.open" | "region.next" | "region.prev"
        | "app.quit" => true,
        _ => matches!(action_for(id, state), Some(Action::Run(_) | Action::Gui)),
    }
}

/// "Snapping on · 1920×1080 · 30 fps".
fn status(state: &State) -> String {
    let snapping = if state.ui.snapping {
        "Snapping on"
    } else {
        "Snapping off"
    };
    format!(
        "{snapping} \u{b7} {}",
        format::canvas(&state.project.canvas)
    )
}

/// An export job runs (whoever started it).
fn exporting(state: &State) -> bool {
    state
        .runtime
        .jobs
        .values()
        .any(|job| matches!(job.kind, JobKind::Export { .. }))
}

/// The binding whose keys an item id shows: the GUI's own ids stand for
/// the keys that open them (G§3.8 step 10).
fn key_binding(id: &str) -> &str {
    match id {
        "export.panel" => "export",
        "autocut.page" => "autocut.preview",
        other => other,
    }
}

/// The cap legends of binding `id`'s first usable key (`prefer` when it
/// still has that one); `None` without one.
fn caps(keymap: &Keymap, id: &str, prefer: Option<&str>) -> Option<Vec<String>> {
    let binding = keymap.binding(key_binding(id))?;
    let usable = usable_keys(keymap, binding);
    let preferred = prefer
        .and_then(|p| KeyChord::parse(p).ok())
        .and_then(|p| usable.iter().find(|k| ***k == p).copied());
    preferred
        .or(usable.first().copied())
        .map(KeyChord::cap_legends)
}

/// Builds one state's items.
struct Items<'a> {
    state: &'a State,
    facts: &'a GuiFacts,
    keymap: &'a Keymap,
    out: Vec<HintItem>,
}

impl Items<'_> {
    /// An item for bindings `ids`, enabled when any of them is (G§4.3:
    /// "a combined item when either is").
    fn bind(&mut self, ids: &[&str], label: &str) -> &mut Self {
        let enabled = ids
            .iter()
            .any(|id| hint_enabled(id, self.state, self.facts));
        self.push_bound(ids, None, label, enabled)
    }

    /// An item for bindings `ids` that is always enabled (`Esc` cancels a
    /// drag whatever `cancel` would do outside it, D-42).
    fn bind_on(&mut self, ids: &[&str], label: &str) -> &mut Self {
        self.push_bound(ids, None, label, true)
    }

    /// `bind` showing `prefer` when the binding still has it (`x` for
    /// remove).
    fn bind_prefer(&mut self, id: &str, prefer: &str, label: &str) -> &mut Self {
        let enabled = hint_enabled(id, self.state, self.facts);
        self.push_bound(&[id], Some(prefer), label, enabled)
    }

    fn push_bound(
        &mut self,
        ids: &[&str],
        prefer: Option<&str>,
        label: &str,
        enabled: bool,
    ) -> &mut Self {
        let keys: Vec<Vec<String>> = ids
            .iter()
            .filter_map(|id| caps(self.keymap, id, prefer))
            .collect();
        // An item whose bindings have no usable key is left out.
        if !keys.is_empty() {
            self.out.push(HintItem {
                keys,
                label: label.to_owned(),
                enabled,
                emphasized: false,
            });
        }
        self
    }

    /// An item with literal caps (`[Esc]`, `[↑][↓]`).
    fn literal(&mut self, keys: &[&[&str]], label: &str, enabled: bool) -> &mut Self {
        self.out.push(HintItem {
            keys: keys
                .iter()
                .map(|chord| chord.iter().map(|cap| (*cap).to_owned()).collect())
                .collect(),
            label: label.to_owned(),
            enabled,
            emphasized: false,
        });
        self
    }

    /// A label-only item ("Release to trim").
    fn text(&mut self, label: &str) -> &mut Self {
        self.literal(&[], label, true)
    }

    /// The last item pushed is emphasized when `on`.
    fn emphasize_last(&mut self, on: bool) -> &mut Self {
        if let Some(last) = self.out.last_mut() {
            last.emphasized = on;
        }
        self
    }

    /// `canvas.mode`'s label: the mode `Tab` switches to.
    fn mode_label(&self) -> &'static str {
        match self.state.ui.canvas_mode {
            CanvasMode::Live => "Program",
            CanvasMode::Program => "Live",
        }
    }

    /// `play.toggle`'s label: what `Space` does now.
    fn play_label(&self) -> &'static str {
        if self.facts.playing { "Pause" } else { "Play" }
    }

    fn build(mut self, hint: &HintState) -> Vec<HintItem> {
        use HintState as H;
        if self.export_prefix(hint) {
            if self.facts.export_panel == Some(ExportPanelKind::Running) {
                self.literal(&[&["Esc"]], "Close panel (export continues)", true);
            }
            self.bind(&["export.panel"], "Show export");
        }
        let mode = self.mode_label();
        let play = self.play_label();
        match hint {
            H::FirstRun => {
                // The first run's button says the same (WP4's label).
                let n = self.facts.first_run_checked.unwrap_or(0);
                let label = super::canvas::add_sources_label(n);
                self.literal(&[&["Enter"]], &label, n > 0)
                    .bind(&["record.toggle"], "Record")
                    .bind(&["canvas.mode"], mode)
                    .bind(&["palette.open"], "Commands")
                    .bind(&["keys.help"], "All keys");
            }
            H::Empty => {
                self.bind(&["record.toggle"], "Record")
                    .bind(&["canvas.mode"], mode)
                    .bind(&["palette.open"], "Commands")
                    .bind(&["keys.help"], "All keys");
            }
            H::LiveIdle => {
                let landed = take_just_landed(self.state);
                self.bind(&["record.toggle"], "Record")
                    .bind(&["take.discard-last"], "Discard last take")
                    .emphasize_last(landed)
                    .bind(&["play.toggle"], "Play")
                    .bind(&["canvas.mode"], mode)
                    .bind(&["autocut.run"], "Autocut all")
                    .bind(&["bin.toggle"], "Bin")
                    .bind(&["palette.open"], "Commands")
                    .bind(&["keys.help"], "All keys");
            }
            H::ProgramIdle => {
                self.bind(&["play.toggle"], play)
                    .bind(&["select.prev", "select.next"], "Select clip")
                    .bind(&["zoom.in"], "Zoom in")
                    .bind(&["zoom.selection"], "Zoom to selection")
                    .bind(&["canvas.mode"], mode)
                    .bind(&["keys.help"], "All keys");
            }
            H::ClipSelected => {
                let zoom = if self.state.ui.selection.len() >= 2 {
                    "Zoom to selection"
                } else {
                    "Zoom to clip"
                };
                self.bind(&["clip.split"], "Split")
                    .bind(&["clip.trim-start", "clip.trim-end"], "Trim to playhead")
                    .bind(&["clip.move-left", "clip.move-right"], "Move")
                    .bind_prefer("clip.remove", "x", "Remove")
                    .bind(&["autocut.run"], "Autocut")
                    .bind(&["record.over"], "Record over")
                    .bind(&["zoom.selection"], zoom)
                    .bind(&["keys.help"], "All keys");
            }
            H::FrameZoom => {
                self.bind(
                    &["seek.prev-frame", "seek.next-frame"],
                    "Previous / next frame",
                )
                .bind(
                    &["clip.nudge-start", "clip.nudge-end"],
                    "Nudge edge 1 frame",
                )
                .bind(&["clip.split"], "Split")
                .bind(&["zoom.out"], "Zoom out")
                .bind(&["zoom.fit"], "Fit");
            }
            H::Countdown => {
                self.bind(&["cancel"], "Cancel")
                    .bind(&["record.toggle"], "Cancel");
            }
            H::Recording => {
                self.bind(&["record.toggle"], "Stop")
                    .bind(&["cancel"], "Cancel take (files kept)")
                    .bind(&["canvas.mode"], "Switch view")
                    .bind(&["zoom.in", "zoom.out"], "Zoom")
                    .bind(&["zoom.fit"], "Fit");
            }
            H::Finalizing => {
                let take = self.state.runtime.record.take().map_or(0, |t| t.number());
                self.text(&format!(
                    "Finalizing take {take}. Waiting for every file to close (up to 10 s)."
                ))
                .bind(&["canvas.mode"], "Switch view")
                .bind(&["zoom.in", "zoom.out"], "Zoom");
            }
            H::TrimDrag => {
                self.text("Release to trim")
                    .bind_on(&["cancel"], "Cancel drag")
                    .bind(&["snapping.toggle"], "Snapping on/off")
                    .bind(
                        &["clip.nudge-start", "clip.nudge-end"],
                        "Nudge after release",
                    );
            }
            H::MoveDrag => {
                self.text("Release to move")
                    .bind_on(&["cancel"], "Cancel drag")
                    .bind(&["snapping.toggle"], "Snapping on/off")
                    .bind(
                        &["clip.move-left", "clip.move-right"],
                        "Move with keys instead",
                    );
            }
            H::BinDrag => {
                self.text("Release to insert")
                    .bind_on(&["cancel"], "Cancel drag");
            }
            H::AutocutPage => {
                self.literal(&[&["Enter"]], "Apply autocut", true)
                    .literal(&[&["Esc"]], "Cancel", true)
                    .bind(&["play.toggle"], play)
                    .bind(&["zoom.in", "zoom.out"], "Zoom");
            }
            H::LayerEdit => {
                self.bind(
                    &[
                        "layer.nudge-left",
                        "layer.nudge-up",
                        "layer.nudge-down",
                        "layer.nudge-right",
                    ],
                    "Nudge 1 px",
                )
                .literal(&[&["Shift"]], "\u{d7}10 \u{b7} free aspect", true)
                .literal(&[&["Alt"]], "Crop edges", true)
                .bind(&["cancel"], "Deselect")
                .bind(&["canvas.mode"], mode);
            }
            H::Bin => {
                self.bind(&["bin.toggle"], "Close bin")
                    .literal(&[&["\u{2191}"], &["\u{2193}"]], "Select take", true)
                    .literal(&[&["Enter"]], "Insert at end", true)
                    .literal(&[&["Space"]], "Preview", true);
            }
            H::ExportSetup | H::ExportDone | H::ExportFailed => {
                let enter = match hint {
                    H::ExportSetup => "Export",
                    H::ExportDone => "Open file",
                    _ => "Try again",
                };
                self.literal(&[&["Enter"]], enter, true)
                    .literal(&[&["Tab"]], "Move focus", true)
                    .literal(&[&["Esc"]], "Close", true);
            }
            H::Modal => {
                self.literal(&[&["Tab"]], "Move focus", true)
                    .literal(&[&["Enter"]], "Activate focused button", true)
                    .literal(&[&["Esc"]], "Cancel", true);
            }
            H::Settings => {
                self.literal(&[&["Tab"]], "Move focus", true)
                    .literal(&[&["Esc"]], "Close", true);
            }
            H::Palette => {
                self.literal(&[&["\u{2191}"], &["\u{2193}"]], "Choose", true)
                    .literal(&[&["Enter"]], "Run", true)
                    .literal(&[&["Esc"]], "Close", true);
            }
            H::PortalDenied => {
                self.bind(&["record.toggle"], "Try again")
                    .bind(&["canvas.mode"], mode)
                    .bind(&["bin.toggle"], "Bin")
                    .bind(&["keys.help"], "All keys");
            }
            H::Blocking { check_again } => {
                if *check_again {
                    self.literal(&[&["Enter"]], "Check again", true);
                }
                self.bind(&["app.quit"], "Quit");
            }
        }
        self.out
    }

    /// While an export runs, `export.panel` "Show export" leads the states
    /// whose keys reach the window (and `[Esc]` "Close panel" before it
    /// while the passive panel is open, S15b); overlays, drags, the record
    /// phases and the export panel's own states have their keys elsewhere.
    fn export_prefix(&self, hint: &HintState) -> bool {
        use HintState as H;
        let window_keys = match hint {
            H::LayerEdit => self.facts.drag.is_none(),
            H::AutocutPage
            | H::Bin
            | H::FirstRun
            | H::PortalDenied
            | H::Empty
            | H::FrameZoom
            | H::ClipSelected
            | H::ProgramIdle
            | H::LiveIdle => true,
            _ => false,
        };
        window_keys && exporting(self.state)
    }
}

/// The history's cursor entry is a take placement (`{"event":
/// "takeFinalized", …}`): `[D] Discard last take` is emphasized (S05b).
fn take_just_landed(state: &State) -> bool {
    let history = &state.history;
    history
        .entries
        .iter()
        .find(|e| e.seq == history.cursor)
        .is_some_and(|e| e.cmd.get("event").and_then(|v| v.as_str()) == Some("takeFinalized"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::history::HistoryEntry;
    use crate::ids::{ClipId, JobId, LayerId, TakeId, TimerId};
    use crate::keymap;
    use crate::state::{Job, RecordMode};
    use crate::testkit;

    fn clip(id: &str) -> ClipId {
        ClipId::from_raw(id)
    }

    /// The facts of the design window at fit zoom (BRIEF §2.6: 0.1396
    /// px/frame with the 8 px end inset).
    fn fit() -> GuiFacts {
        GuiFacts {
            px_per_frame: 1032.0 / 7391.0,
            fit_px_per_frame: 1032.0 / 7391.0,
            ..GuiFacts::default()
        }
    }

    fn program(selection: &[&str]) -> State {
        let mut state = testkit::design_state();
        state.ui.canvas_mode = CanvasMode::Program;
        state.ui.selection = selection.iter().map(|c| clip(c)).collect();
        state
    }

    fn export_job(state: &mut State) {
        state.runtime.jobs.insert(
            JobId(7),
            Job {
                id: JobId(7),
                kind: JobKind::Export {
                    out: "exports/rust-traits-talk-20261014-153012.mp4".into(),
                    files: Default::default(),
                    total_frames: 7391,
                    seg_frames: vec![7391],
                    seg_done: vec![3449],
                    stitch_total_ns: 1,
                    stitch_done_ns: 0,
                    stitching: false,
                },
                req: None,
                started: state.project.created_at,
            },
        );
    }

    fn recording(state: &mut State, phase: &str) {
        let take = TakeId::numbered(10);
        state.runtime.record = match phase {
            "countdown" => RecordPhase::Countdown {
                remaining: 3,
                mode: RecordMode::Append,
                timer: TimerId(1),
                start_req: None,
                wait: false,
            },
            "starting" => RecordPhase::Starting {
                take,
                mode: RecordMode::Append,
                start_req: None,
                wait: false,
            },
            "recording" => RecordPhase::Recording {
                take,
                mode: RecordMode::Append,
                elapsed_ns: 161_400_000_000,
                waiters: vec![],
            },
            _ => RecordPhase::Finalizing {
                take,
                mode: RecordMode::Append,
                cancelled: false,
                stop_reqs: vec![],
                waiters: vec![],
                discard: None,
            },
        };
    }

    /// The items as BRIEF writes them: `[Ctrl][K] Commands`, a space
    /// between chords, `(disabled)` and `(emphasized)` marks.
    fn line(items: &[HintItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| {
                let keys: Vec<String> = item
                    .keys
                    .iter()
                    .map(|chord| chord.iter().map(|cap| format!("[{cap}]")).collect())
                    .collect();
                let mut text = if keys.is_empty() {
                    item.label.clone()
                } else {
                    format!("{} {}", keys.join(" "), item.label)
                };
                if !item.enabled {
                    text.push_str(" (disabled)");
                }
                if item.emphasized {
                    text.push_str(" (emphasized)");
                }
                text
            })
            .collect()
    }

    /// The screens' states, each with its BRIEF §5 list (G§8.1: verbatim,
    /// labels, order, enabled).
    fn screens() -> Vec<(&'static str, State, GuiFacts, HintState, Vec<&'static str>)> {
        let mut out = Vec::new();

        // S01a: an empty folder; WP4 counts 4 checked sources.
        let mut s01a = testkit::state(crate::model::Project::new(
            "rust-traits-talk",
            testkit::design_project().created_at,
            &Default::default(),
        ));
        s01a.ui.snapping = true;
        let facts = GuiFacts {
            first_run_checked: Some(4),
            ..GuiFacts::default()
        };
        out.push((
            "S01a",
            s01a,
            facts,
            HintState::FirstRun,
            vec![
                "[Enter] Add 4 sources",
                "[R] Record (disabled)",
                "[Tab] Program",
                "[Ctrl][K] Commands",
                "[?] All keys",
            ],
        ));

        // S01b: sources added, no takes.
        let mut s01b = testkit::design_state();
        s01b.project.timeline.clips.clear();
        out.push((
            "S01b",
            s01b,
            GuiFacts::default(),
            HintState::Empty,
            vec![
                "[R] Record",
                "[Tab] Program",
                "[Ctrl][K] Commands",
                "[?] All keys",
            ],
        ));

        // S02: Live, idle, playhead at the end.
        let mut s02 = testkit::design_state();
        s02.ui.playhead = 7391;
        let live_idle = vec![
            "[R] Record",
            "[D] Discard last take",
            "[Space] Play (disabled)",
            "[Tab] Program",
            "[C] Autocut all",
            "[B] Bin",
            "[Ctrl][K] Commands",
            "[?] All keys",
        ];
        out.push((
            "S02",
            s02.clone(),
            fit(),
            HintState::LiveIdle,
            live_idle.clone(),
        ));

        // S03 to S05: the record phases.
        let mut s03 = s02.clone();
        recording(&mut s03, "countdown");
        out.push((
            "S03",
            s03,
            fit(),
            HintState::Countdown,
            vec!["[Esc] Cancel", "[R] Cancel"],
        ));
        let mut s04 = s02.clone();
        recording(&mut s04, "recording");
        let ghost_fit = GuiFacts {
            px_per_frame: 1032.0 / 12_233.0,
            fit_px_per_frame: 1032.0 / 12_233.0,
            ..GuiFacts::default()
        };
        let recording_hints = vec![
            "[R] Stop",
            "[Esc] Cancel take (files kept)",
            "[Tab] Switch view",
            "[=] [\u{2212}] Zoom",
            "[\\] Fit",
        ];
        out.push((
            "S04",
            s04,
            ghost_fit.clone(),
            HintState::Recording,
            recording_hints.clone(),
        ));
        let mut starting = s02.clone();
        recording(&mut starting, "starting");
        out.push((
            "S04 (starting, D-04)",
            starting,
            ghost_fit.clone(),
            HintState::Recording,
            recording_hints,
        ));
        let mut s05 = s02.clone();
        recording(&mut s05, "finalizing");
        out.push((
            "S05",
            s05,
            ghost_fit,
            HintState::Finalizing,
            vec![
                "Finalizing take 10. Waiting for every file to close (up to 10 s).",
                "[Tab] Switch view",
                "[=] [\u{2212}] Zoom",
            ],
        ));

        // S05b: take 10 landed (the history's cursor entry places it).
        let mut s05b = s02.clone();
        s05b.history.entries = Arc::new(vec![HistoryEntry {
            seq: 1,
            ts: s05b.project.created_at,
            label: "Record take 10".into(),
            cmd: json!({"event": "takeFinalized", "take": "take-0010"}),
            forward: json_patch::Patch(vec![]),
            inverse: json_patch::Patch(vec![]),
            after_hash: String::new(),
        }]);
        s05b.history.cursor = 1;
        let mut landed = live_idle.clone();
        landed[1] = "[D] Discard last take (emphasized)";
        out.push(("S05b", s05b, fit(), HintState::LiveIdle, landed));

        // S06: Program, c2d0 selected.
        let s06 = program(&["c2d0"]);
        let clip_selected = vec![
            "[S] Split",
            "[[] []] Trim to playhead",
            "[<] [>] Move",
            "[X] Remove",
            "[C] Autocut",
            "[O] Record over",
            "[Z] Zoom to clip",
            "[?] All keys",
        ];
        out.push((
            "S06",
            s06.clone(),
            fit(),
            HintState::ClipSelected,
            clip_selected.clone(),
        ));

        // S07, S08: Program, nothing selected (fit; 48 px/s).
        let program_idle = vec![
            "[Space] Play",
            "[,] [.] Select clip",
            "[=] Zoom in",
            "[Z] Zoom to selection (disabled)",
            "[Tab] Live",
            "[?] All keys",
        ];
        out.push((
            "S07",
            program(&[]),
            fit(),
            HintState::ProgramIdle,
            program_idle.clone(),
        ));
        out.push((
            "S08",
            program(&[]),
            GuiFacts {
                px_per_frame: 1.6,
                ..fit()
            },
            HintState::ProgramIdle,
            program_idle,
        ));

        // S09: frame level, c2d1 selected, playhead 3176. The playhead sits
        // on the c2d0/c2d1 cut, where the core refuses a split, so `[S]`
        // is disabled (the frame draws it enabled: G§4.3's rule wins).
        let mut s09 = program(&["c2d1"]);
        s09.ui.playhead = 3176;
        out.push((
            "S09",
            s09,
            GuiFacts {
                px_per_frame: 32.0,
                ..fit()
            },
            HintState::FrameZoom,
            vec![
                "[\u{2190}] [\u{2192}] Previous / next frame",
                "[Alt][[] [Alt][]] Nudge edge 1 frame",
                "[S] Split (disabled)",
                "[\u{2212}] Zoom out",
                "[\\] Fit",
            ],
        ));

        // S10, S11: drags.
        out.push((
            "S10",
            program(&["c2d0"]),
            GuiFacts {
                drag: Some(DragKind::TrimStart),
                px_per_frame: 1.6,
                ..fit()
            },
            HintState::TrimDrag,
            vec![
                "Release to trim",
                "[Esc] Cancel drag",
                "[N] Snapping on/off",
                "[Alt][[] [Alt][]] Nudge after release",
            ],
        ));
        out.push((
            "S11",
            program(&["c8a1"]),
            GuiFacts {
                drag: Some(DragKind::Move),
                ..fit()
            },
            HintState::MoveDrag,
            vec![
                "Release to move",
                "[Esc] Cancel drag",
                "[N] Snapping on/off (disabled)",
                "[<] [>] Move with keys instead",
            ],
        ));

        // S12: the autocut page.
        let mut s12 = program(&["c3e9"]);
        s12.ui.playhead = 4842;
        out.push((
            "S12",
            s12,
            GuiFacts {
                panel_page: PanelPageKind::Autocut,
                px_per_frame: 1040.0 / 3105.0,
                ..fit()
            },
            HintState::AutocutPage,
            vec![
                "[Enter] Apply autocut",
                "[Esc] Cancel",
                "[Space] Play",
                "[=] [\u{2212}] Zoom",
            ],
        ));

        // S13: L2 selected on the Live canvas.
        let mut s13 = s02.clone();
        s13.ui.selected_layer = Some(LayerId::numbered(2));
        out.push((
            "S13",
            s13,
            GuiFacts {
                focus: FocusRegion::Stage,
                ..fit()
            },
            HintState::LayerEdit,
            vec![
                "[\u{2190}] [\u{2191}] [\u{2193}] [\u{2192}] Nudge 1 px",
                "[Shift] \u{d7}10 \u{b7} free aspect",
                "[Alt] Crop edges",
                "[Esc] Deselect",
                "[Tab] Program",
            ],
        ));

        // S14: the bin has the focus.
        let mut s14 = program(&["c2d0"]);
        s14.ui.bin_open = true;
        out.push((
            "S14",
            s14,
            GuiFacts {
                focus: FocusRegion::Bin,
                ..fit()
            },
            HintState::Bin,
            vec![
                "[B] Close bin",
                "[\u{2191}] [\u{2193}] Select take",
                "[Enter] Insert at end",
                "[Space] Preview",
            ],
        ));

        // S15a, S16, S17: the export panel's trapping states.
        for (name, kind, state_kind, enter) in [
            (
                "S15a",
                ExportPanelKind::Setup,
                HintState::ExportSetup,
                "[Enter] Export",
            ),
            (
                "S16",
                ExportPanelKind::Done,
                HintState::ExportDone,
                "[Enter] Open file",
            ),
            (
                "S17",
                ExportPanelKind::Failed,
                HintState::ExportFailed,
                "[Enter] Try again",
            ),
        ] {
            out.push((
                name,
                program(&["c2d0"]),
                GuiFacts {
                    export_panel: Some(kind),
                    top_overlay: Some(OverlayKind::ExportPanel),
                    ..fit()
                },
                state_kind,
                vec![enter, "[Tab] Move focus", "[Esc] Close"],
            ));
        }

        // S15b: an export runs with the passive panel open over S06.
        let mut s15b = program(&["c2d0"]);
        export_job(&mut s15b);
        let mut s15b_hints = vec![
            "[Esc] Close panel (export continues)",
            "[Ctrl][E] Show export",
        ];
        let mut rest = clip_selected.clone();
        rest[5] = "[O] Record over (disabled)";
        s15b_hints.extend(rest);
        out.push((
            "S15b",
            s15b.clone(),
            GuiFacts {
                export_panel: Some(ExportPanelKind::Running),
                top_overlay: Some(OverlayKind::ExportPanel),
                ..fit()
            },
            HintState::ClipSelected,
            s15b_hints,
        ));

        // S18, S18b: a modal (over the bin; over Settings).
        let modal = vec![
            "[Tab] Move focus",
            "[Enter] Activate focused button",
            "[Esc] Cancel",
        ];
        out.push((
            "S18",
            program(&["c2d0"]),
            GuiFacts {
                top_overlay: Some(OverlayKind::Modal),
                ..fit()
            },
            HintState::Modal,
            modal.clone(),
        ));
        out.push((
            "S18b",
            s02.clone(),
            GuiFacts {
                top_overlay: Some(OverlayKind::Modal),
                ..fit()
            },
            HintState::Modal,
            modal,
        ));

        // Settings alone (not drawn), the palette (S19), a refused screen
        // (S24).
        out.push((
            "Settings",
            s02.clone(),
            GuiFacts {
                top_overlay: Some(OverlayKind::Settings),
                ..fit()
            },
            HintState::Settings,
            vec!["[Tab] Move focus", "[Esc] Close"],
        ));
        out.push((
            "S19",
            program(&["c2d0"]),
            GuiFacts {
                top_overlay: Some(OverlayKind::Palette),
                ..fit()
            },
            HintState::Palette,
            vec!["[\u{2191}] [\u{2193}] Choose", "[Enter] Run", "[Esc] Close"],
        ));
        out.push((
            "S24",
            s02,
            GuiFacts {
                portal_denied: true,
                ..fit()
            },
            HintState::PortalDenied,
            vec!["[R] Try again", "[Tab] Program", "[B] Bin", "[?] All keys"],
        ));
        out
    }

    #[test]
    fn every_screen_shows_its_brief_hints() {
        let keymap = keymap::resolve("");
        for (name, state, facts, want_state, want) in screens() {
            let got_state = hint_state(&state, &facts);
            assert_eq!(got_state, want_state, "{name}");
            let (items, status) = hint_bar(&got_state, &state, &facts, &keymap);
            assert_eq!(line(&items), want, "{name}");
            assert_eq!(
                status, "Snapping on \u{b7} 1920\u{d7}1080 \u{b7} 30 fps",
                "{name}"
            );
        }
    }

    #[test]
    fn hints_snapshot() {
        let keymap = keymap::resolve("");
        let all: Vec<serde_json::Value> = screens()
            .into_iter()
            .map(|(name, state, facts, hint, _)| {
                let (items, status) = hint_bar(&hint, &state, &facts, &keymap);
                json!({"screen": name, "state": hint, "items": items, "status": status})
            })
            .collect();
        insta::assert_json_snapshot!(all);
    }

    #[test]
    fn the_blocking_pages_offer_check_again_and_quit() {
        let keymap = keymap::resolve("");
        let state = testkit::design_state();
        let facts = GuiFacts::default();
        let (items, status) = hint_bar(
            &HintState::Blocking { check_again: true },
            &state,
            &facts,
            &keymap,
        );
        assert_eq!(line(&items), ["[Enter] Check again", "[Ctrl][Q] Quit"]);
        assert_eq!(status, "");
        let (items, _) = hint_bar(
            &HintState::Blocking { check_again: false },
            &state,
            &facts,
            &keymap,
        );
        assert_eq!(line(&items), ["[Ctrl][Q] Quit"]);
    }

    /// Rebound keys show; an item whose binding has no usable key (a
    /// conflict, an unbound key) is left out.
    #[test]
    fn hints_use_the_users_keys() {
        let keymap = keymap::resolve(
            "[bindings]\n\"record.toggle\" = \"F9\"\n\"autocut.run\" = \"x\"\n\"bin.toggle\" = []\n",
        );
        let state = testkit::design_state();
        let (items, _) = hint_bar(&HintState::LiveIdle, &state, &fit(), &keymap);
        assert_eq!(
            line(&items),
            [
                "[F9] Record",
                "[D] Discard last take",
                "[Space] Play (disabled)",
                "[Tab] Program",
                "[Ctrl][K] Commands",
                "[?] All keys",
            ]
        );
        // `clip.remove` keeps Delete and Backspace when `x` conflicts.
        let s06 = program(&["c2d0"]);
        let (items, _) = hint_bar(&HintState::ClipSelected, &s06, &fit(), &keymap);
        assert!(
            line(&items).contains(&"[Del] Remove".to_owned()),
            "{items:?}"
        );
    }

    #[test]
    fn the_first_run_counts_its_sources() {
        let keymap = keymap::resolve("");
        let mut state = testkit::design_state();
        state.project.scene = Default::default();
        for (checked, want) in [
            (Some(1), "[Enter] Add 1 source"),
            (Some(3), "[Enter] Add 3 sources"),
            // As the button reads with nothing checked.
            (Some(0), "[Enter] Add sources (disabled)"),
            (None, "[Enter] Add sources (disabled)"),
        ] {
            let facts = GuiFacts {
                first_run_checked: checked,
                ..GuiFacts::default()
            };
            assert_eq!(hint_state(&state, &facts), HintState::FirstRun);
            let (items, _) = hint_bar(&HintState::FirstRun, &state, &facts, &keymap);
            assert_eq!(line(&items)[0], want);
        }
    }

    #[test]
    fn precedence_follows_g4_3() {
        let mut state = program(&["c2d0"]);
        // A drag beats the record phase; a scrub keeps the state below.
        recording(&mut state, "recording");
        let drag = |kind| GuiFacts {
            drag: Some(kind),
            ..fit()
        };
        assert_eq!(
            hint_state(&state, &drag(DragKind::LayerMove)),
            HintState::LayerEdit
        );
        assert_eq!(
            hint_state(&state, &drag(DragKind::BinInsert)),
            HintState::BinDrag
        );
        assert_eq!(
            hint_state(&state, &drag(DragKind::Scrub)),
            HintState::Recording
        );
        // A modal beats a drag; the cheat sheet and a menu don't count.
        let over = |kind| GuiFacts {
            top_overlay: Some(kind),
            drag: Some(DragKind::Move),
            ..fit()
        };
        assert_eq!(
            hint_state(&state, &over(OverlayKind::Modal)),
            HintState::Modal
        );
        assert_eq!(
            hint_state(&state, &over(OverlayKind::CheatSheet)),
            HintState::MoveDrag
        );
        assert_eq!(
            hint_state(&state, &over(OverlayKind::Menu)),
            HintState::MoveDrag
        );
        // The running export panel shows the state below.
        let idle = program(&[]);
        let running = GuiFacts {
            export_panel: Some(ExportPanelKind::Running),
            ..fit()
        };
        assert_eq!(hint_state(&idle, &running), HintState::ProgramIdle);
        // A layer selected without the stage's focus is a clip state; in
        // Program with nothing selected there is nothing to edit (D-06).
        let mut live = testkit::design_state();
        live.ui.selected_layer = Some(LayerId::numbered(2));
        assert_eq!(hint_state(&live, &fit()), HintState::LiveIdle);
        let stage = GuiFacts {
            focus: FocusRegion::Stage,
            ..fit()
        };
        assert_eq!(hint_state(&live, &stage), HintState::LayerEdit);
        let mut nothing = program(&[]);
        nothing.ui.selected_layer = Some(LayerId::numbered(2));
        assert_eq!(hint_state(&nothing, &stage), HintState::ProgramIdle);
    }

    #[test]
    fn enablement_of_the_guis_own_ids() {
        let state = program(&[]);
        let facts = fit();
        assert!(hint_enabled("export.panel", &state, &facts));
        assert!(hint_enabled("autocut.page", &state, &facts));
        assert!(!hint_enabled("zoom.selection", &state, &facts));
        assert!(hint_enabled("zoom.in", &state, &facts));
        assert!(!hint_enabled("zoom.out", &state, &facts), "at fit");
        let zoomed = GuiFacts {
            px_per_frame: MAX_PX_PER_FRAME,
            ..fit()
        };
        assert!(!hint_enabled("zoom.in", &state, &zoomed));
        assert!(hint_enabled("zoom.out", &state, &zoomed));
        assert!(hint_enabled("zoom.fit", &state, &facts));
        for id in [
            "palette.open",
            "keys.help",
            "settings.open",
            "region.next",
            "app.quit",
        ] {
            assert!(hint_enabled(id, &state, &facts), "{id}");
        }
        // Recording: the export key still opens a running export's panel,
        // the autocut page waits.
        let mut busy = state.clone();
        recording(&mut busy, "recording");
        assert!(!hint_enabled("export.panel", &busy, &facts));
        assert!(!hint_enabled("autocut.page", &busy, &facts));
        export_job(&mut busy);
        assert!(hint_enabled("export.panel", &busy, &facts));
        // An empty timeline: nothing to export, fit or autocut.
        let mut empty = state;
        empty.project.timeline.clips.clear();
        assert!(!hint_enabled("export.panel", &empty, &facts));
        assert!(!hint_enabled("zoom.fit", &empty, &facts));
        assert!(!hint_enabled("autocut.page", &empty, &facts));
    }

    /// While an export runs, "Show export" leads the window's states but
    /// not the overlays or the record phases.
    #[test]
    fn the_export_prefix_is_for_the_windows_keys() {
        let keymap = keymap::resolve("");
        let mut state = program(&[]);
        export_job(&mut state);
        let (items, _) = hint_bar(&HintState::ProgramIdle, &state, &fit(), &keymap);
        assert_eq!(line(&items)[0], "[Ctrl][E] Show export");
        for hint in [HintState::Modal, HintState::Settings, HintState::TrimDrag] {
            let (items, _) = hint_bar(&hint, &state, &fit(), &keymap);
            assert!(
                !line(&items).iter().any(|i| i.contains("Show export")),
                "{hint:?}"
            );
        }
        let snapping_off = {
            let mut s = state.clone();
            s.ui.snapping = false;
            s
        };
        let (_, status) = hint_bar(&HintState::ProgramIdle, &snapping_off, &fit(), &keymap);
        assert_eq!(status, "Snapping off \u{b7} 1920\u{d7}1080 \u{b7} 30 fps");
    }
}
