//! The command palette (S19, S19b, C34; gated on G§0.2). Owner: WP9. It
//! re-registers `palette.open`.
//!
//! Until WP9 lands, `Ctrl+K`, `:` and the header's Commands button show the
//! info toast "Not built yet" (G§1.1, registered by the app).

use gtk::prelude::*;

use crate::Ctx;

/// Whether the palette is built (`GuiFacts.built`). WP9 sets it.
pub const BUILT: bool = false;

/// The palette (opened in `Layer::Palette`).
pub struct Palette {
    root: gtk::Box,
}

impl Palette {
    pub fn new(ctx: &Ctx) -> Self {
        let _ = ctx;
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("palette");
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
