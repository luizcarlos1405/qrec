//! The header's export slot (C04, BRIEF §2.3): a 104×32 secondary button at
//! x 1368 whose content is a stack, `Export…` (download icon) or, while any
//! export runs, "42%" and a 40×4 mini mud bar ("Exporting 42%" goes to the
//! tooltip so Record never moves, README polishing call). Pressed while the
//! export panel is open. A click runs `export` through the key router,
//! which opens or closes the export panel (G§3.8 step 10, WP7's).

use std::cell::RefCell;

use gtk::prelude::*;
use qrec_core::view::header::ExportSlot as View;

use super::LiveTooltip;
use crate::widgets::button::accessible_name;
use crate::{Ctx, guarded};

/// The export slot.
pub struct ExportSlot {
    button: gtk::Button,
    stack: gtk::Stack,
    percent: gtk::Label,
    bar: gtk::ProgressBar,
    tooltip: LiveTooltip,
    shown: RefCell<Option<View>>,
}

impl ExportSlot {
    pub fn new(ctx: &Ctx) -> Self {
        let button = gtk::Button::new();
        button.set_widget_name("export-button");
        button.add_css_class("qrec-btn");
        button.add_css_class("secondary");
        button.add_css_class("export-slot");
        button.set_focus_on_click(false);
        button.set_size_request(104, 32);
        button.set_valign(gtk::Align::Center);
        let stack = gtk::Stack::new();
        stack.set_hhomogeneous(false);
        let idle = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        idle.set_halign(gtk::Align::Center);
        let icon = gtk::Image::from_icon_name("qrec-download-symbolic");
        icon.set_pixel_size(16);
        idle.append(&icon);
        idle.append(&gtk::Label::new(Some("Export\u{2026}")));
        stack.add_named(&idle, Some("idle"));
        let running = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        running.set_halign(gtk::Align::Center);
        // "42%" in `mono-sm` (JetBrains Mono 12, as S15b and the S15c
        // board draw it; it was the button's Atkinson 14 until the WP7
        // integration audit).
        let percent = gtk::Label::new(Some("0%"));
        percent.set_widget_name("export-slot-percent");
        percent.add_css_class("t-mono-sm");
        percent.add_css_class("export-percent");
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("mini-mud");
        bar.set_size_request(40, 4);
        bar.set_valign(gtk::Align::Center);
        running.append(&percent);
        running.append(&bar);
        stack.add_named(&running, Some("running"));
        stack.set_visible_child_name("idle");
        button.set_child(Some(&stack));
        let tooltip = LiveTooltip::attach(&button);
        let keys = ctx.keys.clone();
        button.connect_clicked(guarded!("header: export", move |_| {
            keys.run_binding("export")
        }));
        Self {
            button,
            stack,
            percent,
            bar,
            tooltip,
            shown: RefCell::new(None),
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.button.clone().upcast()
    }

    /// Shows `view` (`view::header`'s export slot).
    pub fn show(&self, view: &View) {
        if self.shown.borrow().as_ref() == Some(view) {
            return;
        }
        match view.percent {
            Some(percent) => {
                self.percent.set_text(&format!("{percent}%"));
                self.bar.set_fraction(f64::from(percent.min(100)) / 100.0);
                self.stack.set_visible_child_name("running");
            }
            None => self.stack.set_visible_child_name("idle"),
        }
        if view.pressed {
            self.button.add_css_class("pressed");
        } else {
            self.button.remove_css_class("pressed");
        }
        self.button.set_sensitive(view.enabled);
        let keys: &[Vec<String>] = if view.enabled { &view.keys } else { &[] };
        self.tooltip.set(&view.tooltip, keys);
        let action = match view.percent {
            Some(_) => view.tooltip.clone(),
            None => "Export\u{2026}".to_owned(),
        };
        let name = if view.enabled {
            accessible_name(&action, view.keys.first().map(Vec::as_slice))
        } else {
            format!("{action}: {}", view.tooltip)
        };
        self.button
            .update_property(&[gtk::accessible::Property::Label(&name)]);
        *self.shown.borrow_mut() = Some(view.clone());
    }
}
