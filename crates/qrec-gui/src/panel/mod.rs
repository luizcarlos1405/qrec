//! The Onion layers panel (S06, S07, S12, S13, S13b, C09–C13): header,
//! take banner, layer and audio rows, meters, the inspector, menus, the
//! autocut page. Owner: WP5. Registers `autocut.page`.
//!
//! Until WP5 lands this is WP0b's stub: the region (`region-panel`), docked
//! at the width the layout gives (384; Medium 320) or shown as the Narrow
//! layout's overlay sheet.

use gtk::prelude::*;

use crate::Ctx;
use crate::widgets::stub_region;

/// The panel region (`region-panel`).
pub struct Panel {
    root: gtk::Box,
}

impl Panel {
    pub fn new(ctx: &Ctx) -> Self {
        let _ = ctx;
        let root = stub_region("region-panel", "Onion layers (WP5)");
        root.set_focusable(true);
        root.set_vexpand(true);
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
