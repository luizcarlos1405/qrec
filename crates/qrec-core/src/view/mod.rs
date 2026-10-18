//! View models for the GUI (docs/GUI.md G§4): what the window shows,
//! computed from `State` plus a few numbers the GUI knows (widths, zoom,
//! [`GuiFacts`]) into final strings, flags and geometry.
//!
//! Pure like the rest of the core (A§1.3): no GTK types, no clock, no
//! HashMap. Geometry is in logical px at 100 % UI scale (`f64`, GUI D-46);
//! widgets scale it when they draw. Strings are final copy.
//!
//! Every module has an owner (G§3.17) and is snapshot-tested with `insta`
//! over `testkit::design_state()` and the owner's per-screen variants:
//!
//! | module | owner | content |
//! | --- | --- | --- |
//! | `mod.rs`, `format`, `notices` | WP0a | shared types, number and text formats, GUI toasts |
//! | `blocking` | WP0a interim, WP8 drafts | blocking pages |
//! | `timeline`, `ruler` | WP3 | viewport, clip boxes, lanes, ticks |
//! | `canvas` | WP4 | stage geometry, layer boxes, overlays, first run |
//! | `panel`, `meter`, `autocut` | WP5 | Onion layers panel, meters, autocut page |
//! | `hints`, `header`, `confirm`, `settings` | WP6 | hint bar, header, modals, Settings › Project |
//! | `bin`, `export` | WP7 | bin rows, export panel |
//! | `palette`, `cheatsheet`, `preferences` | WP9 | palette, cheat sheet, Settings pages |
//!
//! Modules whose owner hasn't landed yet are stubs: their public items have
//! their final signatures and return defaults.

pub mod autocut;
pub mod bin;
pub mod blocking;
pub mod canvas;
pub mod cheatsheet;
pub mod confirm;
pub mod export;
pub mod format;
pub mod header;
pub mod hints;
pub mod meter;
pub mod notices;
pub mod palette;
pub mod panel;
pub mod preferences;
pub mod ruler;
pub mod settings;
pub mod timeline;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{ClipId, Frame, TrackId};
use crate::keymap::gate_reason;
use crate::protocol::command::Command;
use crate::state::State;

/// What the GUI knows that `State` doesn't, for hints and the header
/// (`LocalUi::facts()` in the GUI).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiFacts {
    /// A drag in progress.
    pub drag: Option<DragKind>,
    /// The overlay on top, if any.
    pub top_overlay: Option<OverlayKind>,
    /// The export panel: open, and in which mode.
    pub export_panel: Option<ExportPanelKind>,
    pub focus: FocusRegion,
    pub panel_page: PanelPageKind,
    /// The timeline's zoom as shown (it may lead `ui.zoom` while animating).
    pub px_per_frame: f64,
    pub fit_px_per_frame: f64,
    /// First run (S01a): how many sources are checked.
    pub first_run_checked: Option<usize>,
    /// A screen is `Refused` in the live status (used once S24 is approved).
    pub portal_denied: bool,
    /// The program plays in the player (the GUI's
    /// `LocalUi.program_playing`: not a bin preview's take, not
    /// `runtime.playing`).
    pub playing: bool,
    /// Takes selected in the bin.
    pub bin_selection: usize,
    pub built: Built,
}

/// GUI parts that exist yet (G§1.1): all of them since WP9; the core
/// keeps the facts for a GUI built without some.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Built {
    pub palette: bool,
    pub cheat_sheet: bool,
    pub settings_pages: bool,
}

/// What is being dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DragKind {
    TrimStart,
    TrimEnd,
    Move,
    Scrub,
    LayerMove,
    LayerResize,
    LayerCrop,
    LayerNudge,
    LayerReorder,
    BinInsert,
}

impl DragKind {
    /// A layer drag on the canvas or in the panel (`Layer*`).
    pub fn is_layer(self) -> bool {
        matches!(
            self,
            DragKind::LayerMove
                | DragKind::LayerResize
                | DragKind::LayerCrop
                | DragKind::LayerNudge
                | DragKind::LayerReorder
        )
    }
}

/// The kinds of overlays the window stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OverlayKind {
    Modal,
    Settings,
    Palette,
    CheatSheet,
    ExportPanel,
    Menu,
    LayersSheet,
}

/// The export panel's mode (S15a–S17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportPanelKind {
    Setup,
    Running,
    Done,
    Failed,
}

/// Which region holds the keyboard focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FocusRegion {
    Header,
    Stage,
    Transport,
    TimelineToolbar,
    Timeline,
    Panel,
    Bin,
    /// A text or numeric field: keys type.
    Field,
    #[default]
    Other,
}

/// The page the right panel shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PanelPageKind {
    #[default]
    Layers,
    Autocut,
}

/// The autocut page's preview, drawn on the timeline (S12).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocutPreview {
    pub spans: Vec<AutocutPreviewSpan>,
    /// A dry run is in flight.
    pub analyzing: bool,
}

/// One silence autocut would remove, in timeline frames before the cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocutPreviewSpan {
    pub clip: ClipId,
    pub track: TrackId,
    pub from: Frame,
    pub to: Frame,
    /// "−1.6 s".
    pub tag: String,
}

/// One hint of the hint bar (C03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HintItem {
    /// Chords, each a list of key-cap legends (`[["Ctrl", "Z"]]`); empty
    /// for a label-only item ("Release to trim").
    pub keys: Vec<Vec<String>>,
    pub label: String,
    pub enabled: bool,
    pub emphasized: bool,
}

/// A live source's state as the capture engine reports it (the shell's
/// `LiveSourceStatus`, GTK-free), keyed by the source: a screen's output,
/// a camera's device or an audio source's name.
pub type LiveSources = BTreeMap<String, LiveSourceView>;

/// One source of [`LiveSources`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSourceView {
    pub state: LiveSourceState,
    /// The size it delivers, once known (layer boxes in Live mode).
    pub size: Option<(u32, u32)>,
}

/// See [`LiveSourceView`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum LiveSourceState {
    AwaitingPortal,
    Live,
    /// The portal refused the screen: `cancelled` when the picker was
    /// closed, otherwise denied.
    Refused {
        cancelled: bool,
        message: String,
    },
    Dead {
        message: String,
    },
}

/// A rectangle in logical px (100 % UI scale).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Whether the point is inside (left and top edges included).
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Whether a control that sends `cmd` is enabled (D-43): `Err(reason)`
/// when the gates every command shares would refuse it now (the record
/// phase, the GUI host, an invalid `qrec.json`; `keymap::gate_reason`).
/// Controls that edit a scene also need `canvas::edit_target`.
pub fn enabled(state: &State, cmd: &Command) -> Result<(), String> {
    match gate_reason(state, cmd) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::command::{SelectOp, UiPatch};
    use crate::state::{RecordMode, RecordPhase};
    use crate::testkit;

    #[test]
    fn controls_follow_the_shared_gates() {
        let mut state = testkit::design_state();
        let split = Command::ClipSplit {
            clip: None,
            at: crate::time::TimeArg::frames(2437),
            timeline: true,
        };
        assert_eq!(enabled(&state, &split), Ok(()));
        state.runtime.record = RecordPhase::Recording {
            take: crate::ids::TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        assert_eq!(
            enabled(&state, &split),
            Err("Not while recording".to_owned())
        );
        // While recording, selection, seeks, the mode switch and lane
        // toggles stay enabled (D-43).
        for cmd in [
            Command::Select {
                op: SelectOp::Clear,
            },
            Command::SetUi {
                patch: UiPatch {
                    lane_mute: Some(Default::default()),
                    ..UiPatch::default()
                },
            },
            Command::SetCanvasMode {
                mode: crate::state::CanvasMode::Program,
            },
        ] {
            assert_eq!(enabled(&state, &cmd), Ok(()), "{}", cmd.name());
        }
    }

    #[test]
    fn shared_types_are_camel_case() {
        let facts = GuiFacts {
            drag: Some(DragKind::TrimStart),
            top_overlay: Some(OverlayKind::CheatSheet),
            export_panel: Some(ExportPanelKind::Running),
            focus: FocusRegion::TimelineToolbar,
            ..GuiFacts::default()
        };
        let json = serde_json::to_value(&facts).unwrap();
        assert_eq!(json["drag"], "trimStart");
        assert_eq!(json["topOverlay"], "cheatSheet");
        assert_eq!(json["focus"], "timelineToolbar");
        assert_eq!(json["panelPage"], "layers");
        assert_eq!(json["built"]["cheatSheet"], false);
        assert!(DragKind::LayerReorder.is_layer() && !DragKind::Scrub.is_layer());
        let refused = LiveSourceState::Refused {
            cancelled: true,
            message: "closed".into(),
        };
        assert_eq!(
            serde_json::to_value(refused).unwrap(),
            serde_json::json!({"kind": "refused", "cancelled": true, "message": "closed"})
        );
        let r = Rect::new(10.0, 20.0, 30.0, 40.0);
        assert!(r.contains(10.0, 20.0) && !r.contains(40.0, 20.0));
    }
}
