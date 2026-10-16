//! The cheat sheet (S20, S20b; gated on G§0.2): every key in three
//! columns over the whole window. Owner: WP9. It re-registers `keys.help`.
//!
//! Until WP9 lands, `?` shows the info toast "Not built yet" (G§1.1,
//! registered by the app).

use gtk::prelude::*;

use crate::Ctx;

/// Whether the cheat sheet is built (`GuiFacts.built`). WP9 sets it.
pub const BUILT: bool = false;

/// The cheat sheet (opened in `Layer::Fullscreen`).
pub struct CheatSheet {
    root: gtk::Box,
}

impl CheatSheet {
    pub fn new(ctx: &Ctx) -> Self {
        let _ = ctx;
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("cheatsheet");
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
