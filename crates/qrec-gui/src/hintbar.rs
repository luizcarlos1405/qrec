//! The hint bar (C03, G§4.3): the keys of what can be done now, from
//! `view::hints`, and the status on the right. Owner: WP6.
//!
//! Until WP6 lands this is WP0b's stub: the region (`region-hintbar`,
//! 32 px, never focusable).

use gtk::prelude::*;

use crate::Ctx;
use crate::widgets::stub_region;

/// The hint bar region (`region-hintbar`).
pub struct HintBar {
    root: gtk::Box,
}

impl HintBar {
    pub fn new(ctx: &Ctx) -> Self {
        let _ = ctx;
        let root = stub_region("region-hintbar", "Hint bar (WP6)");
        root.add_css_class("qrec-hintbar-stub");
        root.set_height_request(32);
        root.set_vexpand(false);
        root.set_focusable(false);
        root.set_can_focus(false);
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
