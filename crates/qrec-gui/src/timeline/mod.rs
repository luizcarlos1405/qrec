//! The timeline (S05b–S12, C14–C18): toolbar, ruler, track headers, the
//! canvas of clips and lanes, the playhead overlay, gestures, the drop
//! target, the clip tooltip, zoom. Owner: WP3. Registers `zoom.in`,
//! `zoom.out`, `zoom.selection`.
//!
//! Until WP3 lands this is WP0b's stub: the region (`region-timeline`),
//! its height set by the window (312; compact 232).

use gtk::prelude::*;

use crate::Ctx;
use crate::widgets::stub_region;

/// The timeline region (`region-timeline`).
pub struct Timeline {
    root: gtk::Box,
}

impl Timeline {
    pub fn new(ctx: &Ctx) -> Self {
        let _ = ctx;
        let root = stub_region("region-timeline", "Timeline (WP3)");
        root.set_focusable(true);
        root.set_hexpand(true);
        root.set_vexpand(false);
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
