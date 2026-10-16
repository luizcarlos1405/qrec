//! C05, the mode switch: Live (video icon, 72 wide) and Program (film icon,
//! 100 wide) in a `field` well, 176×32 at (110, 8) (BRIEF §2.3). Always
//! enabled (R-allowed, A§5.3, D-43). A click on the other segment runs
//! `canvas.mode` through the key router, as `Tab` does (G§3.7); the switch
//! follows the snapshot's mode.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use qrec_core::state::CanvasMode;

use crate::{Ctx, guarded};

/// The mode switch.
pub struct ModeSwitch {
    group: adw::ToggleGroup,
    /// Set while the switch follows a snapshot (no command then).
    syncing: Rc<Cell<bool>>,
}

const LIVE: &str = "live";
const PROGRAM: &str = "program";

fn name_of(mode: CanvasMode) -> &'static str {
    match mode {
        CanvasMode::Live => LIVE,
        CanvasMode::Program => PROGRAM,
    }
}

impl ModeSwitch {
    pub fn new(ctx: &Ctx) -> Self {
        let group = adw::ToggleGroup::new();
        group.set_widget_name("mode-switch");
        group.add_css_class("mode-switch");
        group.add_css_class("qrec-segmented");
        group.set_valign(gtk::Align::Center);
        group.add(
            adw::Toggle::builder()
                .name(LIVE)
                .label("Live")
                .icon_name("camera-video-symbolic")
                .build(),
        );
        group.add(
            adw::Toggle::builder()
                .name(PROGRAM)
                .label("Program")
                .icon_name("video-x-generic-symbolic")
                .build(),
        );
        // Chrome is focus-on-click = false (D-11): a click never leaves a
        // segment focused for Space or Enter.
        let mut child = group.first_child();
        while let Some(c) = child {
            c.set_focus_on_click(false);
            child = c.next_sibling();
        }
        group
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label("Canvas mode")]);
        let syncing = Rc::new(Cell::new(true));
        group.set_active_name(Some(name_of(ctx.store.state().ui.canvas_mode)));
        syncing.set(false);
        let (store, keys, quiet) = (ctx.store.clone(), ctx.keys.clone(), Rc::clone(&syncing));
        group.connect_active_name_notify(guarded!("header: mode switch", move |group| {
            if quiet.get() {
                return;
            }
            let wanted = match group.active_name().as_deref() {
                Some(PROGRAM) => CanvasMode::Program,
                _ => CanvasMode::Live,
            };
            if store.state().ui.canvas_mode != wanted {
                keys.run_binding("canvas.mode");
            }
        }));
        Self { group, syncing }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.group.clone().upcast()
    }

    /// Shows `mode` (the snapshot's).
    pub fn sync(&self, mode: CanvasMode) {
        let name = name_of(mode);
        if self.group.active_name().as_deref() == Some(name) {
            return;
        }
        self.syncing.set(true);
        self.group.set_active_name(Some(name));
        self.syncing.set(false);
    }
}
