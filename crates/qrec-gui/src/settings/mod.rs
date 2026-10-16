//! The Settings dialog (S18b, G§4.10): its shell with five tabs as drawn
//! (only Project enabled until WP9, D-48), 840×640, centered, over a scrim
//! that ends above the hint bar. Owner: WP6 (shell, Project), WP9 (the
//! other pages). Registers `settings.open`.
//!
//! Until WP6 lands this is WP0b's stub: nothing opens.

pub mod appearance;
pub mod autocut;
pub mod keyboard;
pub mod project;
pub mod recording;

use gtk::prelude::*;

use crate::Ctx;

/// The Settings dialog (opened in `Layer::Settings`).
pub struct Settings {
    root: gtk::Box,
}

impl Settings {
    pub fn new(ctx: &Ctx) -> Self {
        let _ = ctx;
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("settings");
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
