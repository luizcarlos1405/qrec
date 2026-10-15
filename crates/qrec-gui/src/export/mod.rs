//! The export panel (S15a, S15b, S15c, S16, S17, C25, C29, C30): setup,
//! running (passive, D-49), done, failed; in-window, under the header's
//! Export slot. Owner: WP7. Registers `export.panel` (it toggles the
//! panel; the `Esc` chain runs it to close the passive one).
//!
//! Until WP7 lands this is WP0b's stub: nothing opens.

use gtk::prelude::*;

use crate::Ctx;

/// The export panel's own state, kept while it is closed (WP7 decides what
/// it holds: the setup draft, the job it follows, the last outcome).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportPanelState {}

/// The export panel (opened in `Layer::FloatingTrap` or, while running,
/// `Layer::FloatingPassive`).
pub struct ExportPanel {
    root: gtk::Box,
}

impl ExportPanel {
    pub fn new(ctx: &Ctx) -> Self {
        let _ = ctx;
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("export-panel");
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
