//! `LocalUi` (G§3.6): the GUI-only state that more than one region reads.
//! Never part of `State`: drags, overlays, focus and previews stay in the
//! window (R7). Frozen by WP0b; each field names its writers.
//!
//! Regions change it with `Store::update_local` and follow it with
//! `Store::subscribe_local`.

use std::sync::Arc;

use qrec_core::model::{Crop, Rect};
use qrec_core::protocol::command::SceneTarget;
use qrec_core::view::blocking::BlockKind;
use qrec_core::view::timeline::Viewport;
use qrec_core::view::{
    AutocutPreview, Built, DragKind, FocusRegion, GuiFacts, OverlayKind, PanelPageKind,
};
use qrec_core::{ClipId, CoreError, Frame, LayerId, TakeId};
use qrec_shell::capture::{LiveSource, LiveSourceState, LiveSourceStatus};
use qrec_shell::doctor::DoctorReport;
use qrec_shell::sources::SourceInventory;

pub use crate::export::ExportPanelState;
pub use qrec_core::Command;
pub use qrec_core::view::ExportPanelKind as ExportPanelMode;
pub use qrec_core::view::settings::SettingsPage;

/// The GUI-only state (G§3.6).
#[derive(Debug, Clone, PartialEq)]
pub struct LocalUi {
    /// Where startup is (WP0b).
    pub startup: StartupPhase,
    /// The last dependency check (WP0b): S23, and whether `wl-copy` works.
    pub doctor: Option<Arc<DoctorReport>>,
    /// The region holding the keyboard focus (WP0b, from the window's
    /// focus widget).
    pub focus: FocusRegion,
    /// A drag in progress: WP3 trim/move/scrub, WP4 layer drags and nudges,
    /// WP5 row reorder, WP7 bin → timeline.
    pub drag: Option<DragKind>,
    /// The overlays, top last: WP0b, WP6, WP7, WP9.
    pub overlays: Vec<Overlay>,
    /// The right panel's page (WP5).
    pub panel_page: PanelPageKind,
    /// The autocut page's preview (WP5 writes, WP3 draws).
    pub autocut_preview: Option<Arc<AutocutPreview>>,
    /// A take playing in the Program picture from the bin (WP7 writes, WP4
    /// plays, D-17).
    pub take_preview: Option<TakePreview>,
    /// The frame at a trimmed edge (WP3 writes, WP4 shows, S10).
    pub trim_frame: Option<TrimFrame>,
    /// A layer drag's transient geometry (WP4 writes, WP5's inspector).
    pub layer_drag: Option<LayerDragPreview>,
    /// The timeline as shown (WP3; frame zoom → hints).
    pub viewport: Viewport,
    /// The fit zoom of the timeline as shown (WP3, with `viewport`).
    pub fit_px_per_frame: f64,
    /// The clip under the pointer (WP3).
    pub hover_clip: Option<ClipId>,
    /// Takes selected in the bin (WP7, D-37).
    pub bin_selection: Vec<TakeId>,
    /// First run: how many sources are checked (WP4 → WP6's hint).
    pub first_run_checked: Option<usize>,
    /// Every live source's state (WP0b's side pump).
    pub live_status: Arc<Vec<LiveSourceStatus>>,
    /// What the last discovery found (WP0b; S01a, the Add menus).
    pub sources: Option<Arc<SourceInventory>>,
    /// The player plays, the program or a take (WP4 player sync).
    pub playing: bool,
    /// The program plays in the player (WP4 player sync, G§3.10 step 3):
    /// not a take, and only once the program's load after one landed, so
    /// the player's position is the program's. What the timeline's
    /// playhead, the transport, the key router's effective playhead and
    /// `GuiFacts::playing` follow.
    pub program_playing: bool,
    /// The shuttle rate (WP4 player sync, D-44; the key router's pre-step).
    pub playing_rate: f64,
    /// The export panel's own state (WP7).
    pub export_view: ExportPanelState,
    /// The window's layout class (WP0b, G§3.15).
    pub layout: LayoutClass,
    /// What is wrong with `keys.toml` (WP0b: startup and reloads).
    pub keymap_problems: Vec<String>,
}

impl Default for LocalUi {
    fn default() -> Self {
        Self {
            startup: StartupPhase::default(),
            doctor: None,
            focus: FocusRegion::default(),
            drag: None,
            overlays: Vec::new(),
            panel_page: PanelPageKind::default(),
            autocut_preview: None,
            take_preview: None,
            trim_frame: None,
            layer_drag: None,
            viewport: Viewport::default(),
            fit_px_per_frame: 0.0,
            hover_clip: None,
            bin_selection: Vec::new(),
            first_run_checked: None,
            live_status: Arc::default(),
            sources: None,
            playing: false,
            program_playing: false,
            playing_rate: 1.0,
            export_view: ExportPanelState::default(),
            layout: LayoutClass::default(),
            keymap_problems: Vec::new(),
        }
    }
}

impl LocalUi {
    /// What the hint bar and the header need to know (`view::GuiFacts`).
    pub fn facts(&self) -> GuiFacts {
        GuiFacts {
            drag: self.drag,
            top_overlay: self.overlays.last().map(Overlay::kind),
            export_panel: self.export_panel(),
            focus: self.focus,
            panel_page: self.panel_page,
            px_per_frame: self.viewport.px_per_frame,
            fit_px_per_frame: self.fit_px_per_frame,
            first_run_checked: self.first_run_checked,
            portal_denied: self.live_status.iter().any(|s| {
                matches!(s.source, LiveSource::Screen { .. })
                    && matches!(s.state, LiveSourceState::Refused { .. })
            }),
            playing: self.program_playing,
            bin_selection: self.bin_selection.len(),
            built: Built {
                palette: crate::palette::BUILT,
                cheat_sheet: crate::cheatsheet::BUILT,
                settings_pages: crate::settings::appearance::BUILT,
            },
        }
    }

    /// The export panel's mode while it is open.
    pub fn export_panel(&self) -> Option<ExportPanelMode> {
        self.overlays.iter().rev().find_map(|o| match o {
            Overlay::ExportPanel(mode) => Some(*mode),
            _ => None,
        })
    }

    /// The overlay on top, if any.
    pub fn top_overlay(&self) -> Option<&Overlay> {
        self.overlays.last()
    }

    /// Whether the top overlay keeps the keyboard (G§3.8 step 4): every
    /// overlay but the export panel while it runs (passive, D-49).
    pub fn overlay_traps_focus(&self) -> bool {
        self.overlays.last().is_some_and(Overlay::traps_focus)
    }

    /// Removes every overlay `pred` matches; whether any was.
    pub fn close_overlays(&mut self, pred: impl Fn(&Overlay) -> bool) -> bool {
        let before = self.overlays.len();
        self.overlays.retain(|o| !pred(o));
        self.overlays.len() != before
    }

    /// The Session is open and the editor works.
    pub fn is_open(&self) -> bool {
        self.startup == StartupPhase::Open
    }
}

/// Where the window's startup is (G§3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupPhase {
    /// Steps 1–7: the window shows the layout empty and disabled. `slow`:
    /// step 7 hasn't finished after 1 s (the stage shows "Opening <name>…").
    Opening { slow: bool },
    /// The Session is open (`HostEvent::Opened`).
    Open,
    /// A blocking page replaces the editor (C31 interim pages, G§1.1 #2).
    Blocked(BlockKind),
    /// The window asked to close: it waits for the take to finalize or the
    /// host to stop (D-12).
    Closing,
}

impl Default for StartupPhase {
    fn default() -> Self {
        StartupPhase::Opening { slow: false }
    }
}

/// An overlay of the window (G§3.6), stacked top last.
// A few on the main thread at a time; boxing `Confirm`'s command would
// only change the G§3.6 signature.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    /// A confirmation (`confirmation_required`): S18, S18b, overwrite.
    /// View: WP6 (WP0b's text modal until then).
    Confirm { cmd: Command, error: CoreError },
    /// D-12: closing while an export runs. View: WP6 (text modal until then).
    QuitWhileExporting { percent: u8 },
    /// The export panel; `Running` is passive (D-49). View: WP7.
    ExportPanel(ExportPanelMode),
    /// Settings on a page. View: WP6 (shell, Project), WP9 (other pages).
    Settings(SettingsPage),
    /// The command palette (WP9).
    Palette,
    /// The cheat sheet (WP9).
    CheatSheet,
    /// Narrow windows: the panel as an overlay sheet (layer WP0b, content WP5).
    LayersSheet,
    /// A `PopoverMenu` or `DropDown` is open.
    Menu,
}

impl Overlay {
    /// Its kind, for `GuiFacts`.
    pub fn kind(&self) -> OverlayKind {
        match self {
            Overlay::Confirm { .. } | Overlay::QuitWhileExporting { .. } => OverlayKind::Modal,
            Overlay::ExportPanel(_) => OverlayKind::ExportPanel,
            Overlay::Settings(_) => OverlayKind::Settings,
            Overlay::Palette => OverlayKind::Palette,
            Overlay::CheatSheet => OverlayKind::CheatSheet,
            Overlay::LayersSheet => OverlayKind::LayersSheet,
            Overlay::Menu => OverlayKind::Menu,
        }
    }

    /// It keeps the keyboard while on top (G§3.8 step 4): all but the
    /// running export panel (D-49).
    pub fn traps_focus(&self) -> bool {
        !matches!(self, Overlay::ExportPanel(ExportPanelMode::Running))
    }
}

/// A take playing in the Program picture from the bin (D-17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakePreview {
    pub take: TakeId,
    /// Playing (else paused on its frame).
    pub playing: bool,
    /// It took the player from the playing program, whose playhead's park
    /// at the frame the player showed hasn't been answered yet (WP4's
    /// sync, G§3.10 step 8): until then a playhead change is the program's
    /// own (the park, a once-a-second seek on its way), not a timeline
    /// action, and doesn't end the preview.
    pub parking: bool,
}

/// The frame a trim drag shows on the stage (S10): `frame` of `take`'s
/// recording, in take frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimFrame {
    pub take: TakeId,
    pub frame: Frame,
}

/// A layer being dragged or nudged (D-32, D-33): its transient placement,
/// before the one `LayerSet` of the gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerDragPreview {
    pub target: SceneTarget,
    pub layer: LayerId,
    pub rect: Rect,
    pub crop: Crop,
}

/// The window's width class (G§3.15).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LayoutWidth {
    /// ≥ 1400 px: the panel docked at 384.
    #[default]
    Wide,
    /// 1100–1399 px: the panel docked at 320, inspector 2-up.
    Medium,
    /// < 1100 px: the panel is an overlay sheet.
    Narrow,
}

/// The window's layout class (G§3.15): its width class, and compact under
/// 820 px of height (the timeline keeps 232 px).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct LayoutClass {
    pub width: LayoutWidth,
    pub compact: bool,
}

impl LayoutClass {
    /// The panel's docked width; `None` when it is an overlay sheet.
    pub fn panel_width(self) -> Option<i32> {
        match self.width {
            LayoutWidth::Wide => Some(384),
            LayoutWidth::Medium => Some(320),
            LayoutWidth::Narrow => None,
        }
    }

    /// The timeline's height (312; compact 232).
    pub fn timeline_height(self) -> i32 {
        if self.compact { 232 } else { 312 }
    }

    /// The class for a window `width` × `height` (logical px): what the
    /// breakpoints give (`window::adaptive`).
    pub fn for_size(width: i32, height: i32) -> Self {
        Self {
            width: if width >= 1400 {
                LayoutWidth::Wide
            } else if width >= 1100 {
                LayoutWidth::Medium
            } else {
                LayoutWidth::Narrow
            },
            compact: height < 820,
        }
    }
}

#[cfg(test)]
mod tests {
    use qrec_core::view::ExportPanelKind;

    use super::*;

    #[test]
    fn facts_follow_the_local_state() {
        // WP9 built them all.
        let built = Built {
            palette: true,
            cheat_sheet: true,
            settings_pages: true,
        };
        let mut local = LocalUi::default();
        assert_eq!(
            local.facts(),
            GuiFacts {
                playing: false,
                built,
                ..GuiFacts::default()
            }
        );
        local
            .overlays
            .push(Overlay::ExportPanel(ExportPanelKind::Running));
        local.overlays.push(Overlay::Menu);
        local.drag = Some(DragKind::TrimEnd);
        local.bin_selection = vec![TakeId::numbered(2), TakeId::numbered(4)];
        local.live_status = Arc::new(vec![LiveSourceStatus {
            source: LiveSource::Screen {
                output: "FAKE-1".into(),
            },
            state: LiveSourceState::Refused {
                cancelled: false,
                message: "denied".into(),
            },
            size: None,
        }]);
        let facts = local.facts();
        assert_eq!(facts.top_overlay, Some(OverlayKind::Menu));
        assert_eq!(facts.export_panel, Some(ExportPanelKind::Running));
        assert_eq!(facts.drag, Some(DragKind::TrimEnd));
        assert_eq!(facts.bin_selection, 2);
        assert!(facts.portal_denied);
        assert_eq!(facts.built, built);
    }

    /// R16: the hint bar's Play/Pause follows the program, not a take the
    /// player plays (a bin preview).
    #[test]
    fn the_playing_fact_is_the_programs() {
        let mut local = LocalUi {
            playing: true,
            ..LocalUi::default()
        };
        assert!(!local.facts().playing, "a take plays");
        local.program_playing = true;
        assert!(local.facts().playing);
    }

    #[test]
    fn only_the_running_export_panel_is_passive() {
        for mode in [
            ExportPanelKind::Setup,
            ExportPanelKind::Done,
            ExportPanelKind::Failed,
        ] {
            assert!(Overlay::ExportPanel(mode).traps_focus(), "{mode:?}");
        }
        assert!(!Overlay::ExportPanel(ExportPanelKind::Running).traps_focus());
        for overlay in [
            Overlay::QuitWhileExporting { percent: 42 },
            Overlay::Settings(SettingsPage::Project),
            Overlay::Palette,
            Overlay::CheatSheet,
            Overlay::LayersSheet,
            Overlay::Menu,
        ] {
            assert!(overlay.traps_focus(), "{overlay:?}");
        }
    }

    #[test]
    fn layout_classes_follow_the_breakpoints() {
        let class = |w, h| LayoutClass::for_size(w, h);
        assert_eq!(class(1600, 1000), LayoutClass::default());
        assert_eq!(class(1400, 820).width, LayoutWidth::Wide);
        assert_eq!(class(1399, 1000).width, LayoutWidth::Medium);
        assert_eq!(class(1300, 1000).panel_width(), Some(320));
        assert_eq!(class(1100, 1000).width, LayoutWidth::Medium);
        assert_eq!(class(1099, 1000).width, LayoutWidth::Narrow);
        assert_eq!(class(1000, 1000).panel_width(), None);
        assert!(class(1600, 800).compact);
        assert_eq!(class(1600, 800).timeline_height(), 232);
        assert_eq!(class(1600, 820).timeline_height(), 312);
    }
}
