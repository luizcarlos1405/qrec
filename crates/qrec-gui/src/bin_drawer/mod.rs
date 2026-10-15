//! The bin drawer (S14, S18, C21): the takes not on the timeline, their
//! rows and actions, multi-select (D-37), the drag source (D-38), the bin
//! preview (D-17). Owner: WP7 (`src/bin/` would be a Cargo binary target).
//! Registers `bin.preview`.
//!
//! Until WP7 lands this is WP0b's stub: the region (`region-bin`, 352 px),
//! shown while `ui.binOpen`.

use gtk::prelude::*;

use crate::Ctx;
use crate::widgets::stub_region;

/// The bin drawer region (`region-bin`).
pub struct BinDrawer {
    root: gtk::Box,
}

impl BinDrawer {
    pub fn new(ctx: &Ctx) -> Self {
        let root = stub_region("region-bin", "Bin (WP7)");
        root.set_width_request(352);
        root.set_focusable(true);
        root.set_visible(ctx.store.state().ui.bin_open);
        let shown = root.clone();
        ctx.store
            .subscribe(move |change| shown.set_visible(change.next.ui.bin_open))
            .detach();
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
