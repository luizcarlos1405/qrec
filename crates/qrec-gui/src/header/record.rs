//! C01 Record, the header's record button: a fixed 120×32 at (1240, 8)
//! whose content is centered, so it never moves or resizes (DESIGN
//! Buttons). Idle `● Record [R]`; countdown, outlined `Cancel [R]`;
//! starting or recording `■ Stop [R]` (D-04); finalizing `Saving…` with a
//! spinner, disabled. A click runs `record.toggle` through the key router,
//! with its pre-steps (D-05: from Program, Live first).

use std::cell::RefCell;

use gtk::prelude::*;
use qrec_core::view::header::{RecordButton as View, RecordButtonKind};

use super::LiveTooltip;
use crate::widgets::Spinner;
use crate::widgets::button::accessible_name;
use crate::widgets::keycap::{self, CapSize, CapVariant};
use crate::{Ctx, guarded};

/// The record button.
pub struct RecordButton {
    button: gtk::Button,
    icon: gtk::Image,
    spinner: Spinner,
    label: gtk::Label,
    caps: gtk::Box,
    tooltip: LiveTooltip,
    shown: RefCell<Option<View>>,
}

const KINDS: [&str; 4] = ["idle", "cancel", "stop", "saving"];

impl RecordButton {
    pub fn new(ctx: &Ctx) -> Self {
        let button = gtk::Button::new();
        button.set_widget_name("record-button");
        button.add_css_class("qrec-btn");
        button.add_css_class("record");
        button.set_focus_on_click(false);
        button.set_size_request(120, 32);
        button.set_valign(gtk::Align::Center);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.set_halign(gtk::Align::Center);
        let icon = gtk::Image::from_icon_name("qrec-record-symbolic");
        icon.set_pixel_size(16);
        let spinner = Spinner::new(16);
        spinner.set_visible(false);
        let label = gtk::Label::new(Some("Record"));
        let caps = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        caps.set_valign(gtk::Align::Center);
        content.append(&icon);
        content.append(&spinner);
        content.append(&label);
        content.append(&caps);
        button.set_child(Some(&content));
        let tooltip = LiveTooltip::attach(&button);
        let keys = ctx.keys.clone();
        button.connect_clicked(guarded!("header: record", move |_| {
            keys.run_binding("record.toggle")
        }));
        Self {
            button,
            icon,
            spinner,
            label,
            caps,
            tooltip,
            shown: RefCell::new(None),
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.button.clone().upcast()
    }

    /// Shows `view` (`view::header`'s record button).
    pub fn show(&self, view: &View) {
        if self.shown.borrow().as_ref() == Some(view) {
            return;
        }
        let (class, icon, text, variant) = match view.kind {
            RecordButtonKind::Record => (
                "idle",
                Some("qrec-record-symbolic"),
                "Record",
                Some(CapVariant::OnFill),
            ),
            RecordButtonKind::Cancel => ("cancel", None, "Cancel", Some(CapVariant::Plain)),
            RecordButtonKind::Stop => (
                "stop",
                Some("media-playback-stop-symbolic"),
                "Stop",
                Some(CapVariant::OnFill),
            ),
            RecordButtonKind::Saving => ("saving", None, "Saving\u{2026}", None),
        };
        for kind in KINDS {
            if kind == class {
                self.button.add_css_class(kind);
            } else {
                self.button.remove_css_class(kind);
            }
        }
        match icon {
            Some(name) => {
                self.icon.set_icon_name(Some(name));
                self.icon.set_visible(true);
            }
            None => self.icon.set_visible(false),
        }
        self.spinner
            .set_visible(view.kind == RecordButtonKind::Saving);
        self.label.set_text(text);
        while let Some(child) = self.caps.first_child() {
            self.caps.remove(&child);
        }
        if let Some(variant) = variant {
            for chord in &view.keys {
                self.caps
                    .append(&keycap::chord(chord, CapSize::Md, variant));
            }
        }
        self.caps
            .set_visible(variant.is_some() && !view.keys.is_empty());
        self.button.set_sensitive(view.enabled);
        let keys: &[Vec<String>] = if view.enabled { &view.keys } else { &[] };
        self.tooltip.set(&view.tooltip, keys);
        let spoken = view.keys.first().map(Vec::as_slice);
        let name = if view.enabled {
            accessible_name(text, spoken)
        } else {
            format!("{text}: {}", view.tooltip)
        };
        self.button
            .update_property(&[gtk::accessible::Property::Label(&name)]);
        *self.shown.borrow_mut() = Some(view.clone());
    }
}
