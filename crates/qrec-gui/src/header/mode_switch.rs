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
                .icon_name("qrec-video-symbolic")
                .build(),
        );
        group.add(
            adw::Toggle::builder()
                .name(PROGRAM)
                .label("Program")
                .icon_name("qrec-film-symbolic")
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

    /// Shows `mode` (the snapshot's). A segment that has the focus hands it
    /// to the active one: only that one is in GTK's chain (a radio group's),
    /// so a focus left on the other was one `Tab` never came back to (the
    /// window's start focuses Live, then the project opens in Program;
    /// WP10's focus audit).
    pub fn sync(&self, mode: CanvasMode) {
        let name = name_of(mode);
        if self.group.active_name().as_deref() == Some(name) {
            return;
        }
        let focused = self.group.focus_child().is_some();
        self.syncing.set(true);
        self.group.set_active_name(Some(name));
        self.syncing.set(false);
        if focused && let Some(active) = self.active_segment() {
            active.grab_focus();
        }
    }

    /// The active segment's widget: the group's toggle buttons are its
    /// segments, in order, among its other children.
    fn active_segment(&self) -> Option<gtk::Widget> {
        let mut segments = Vec::new();
        let mut child = self.group.first_child();
        while let Some(c) = child {
            if c.is::<gtk::ToggleButton>() {
                segments.push(c.clone());
            }
            child = c.next_sibling();
        }
        segments.into_iter().nth(self.group.active() as usize)
    }
}
