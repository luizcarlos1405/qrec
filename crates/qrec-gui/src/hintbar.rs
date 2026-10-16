//! The hint bar (C03, G§4.3): the keys of what can be done now, from
//! `view::hints`, and the status on the right. Owner: WP6.
//!
//! Until WP6 lands this is WP0b's stub: the region (`region-hintbar`,
//! 32 px, never focusable) showing [`current_items`]: `view::hints` once it
//! returns items (WP6's core module), else a stand-in, G§4.3's Live idle
//! list with the user's keys (so a rebound key shows: `record.toggle =
//! "F9"` reads `[F9] Record`). The test channel's `hints` op reports the
//! same items. WP6 keeps [`current_items`] as what its bar renders.

use gtk::prelude::*;
use qrec_core::view::HintItem;
use qrec_core::view::format;
use qrec_core::view::hints::{hint_bar, hint_state};

use crate::Ctx;
use crate::widgets::button::binding_caps;
use crate::widgets::keycap::{self, CapSize, CapVariant};

/// G§4.3's Live idle items (binding id, label): the stand-in list.
const STAND_IN: [(&str, &str); 8] = [
    ("record.toggle", "Record"),
    ("take.discard-last", "Discard last take"),
    ("play.toggle", "Play"),
    ("canvas.mode", "Program"),
    ("autocut.run", "Autocut all"),
    ("bin.toggle", "Bin"),
    ("palette.open", "Commands"),
    ("keys.help", "All keys"),
];

/// The hint items and the status text the bar shows now.
pub fn current_items(ctx: &Ctx) -> (Vec<HintItem>, String) {
    let state = ctx.store.state();
    let keymap = ctx.store.keymap();
    let facts = ctx.store.local().facts();
    let (items, status) = hint_bar(&hint_state(&state, &facts), &state, &facts, &keymap);
    if !items.is_empty() {
        return (items, status);
    }
    let items = STAND_IN
        .iter()
        .filter_map(|(id, label)| {
            let caps = binding_caps(&keymap, id)?;
            Some(HintItem {
                keys: vec![caps],
                label: (*label).to_owned(),
                enabled: true,
                emphasized: false,
            })
        })
        .collect();
    let snapping = if state.ui.snapping {
        "Snapping on"
    } else {
        "Snapping off"
    };
    let status = format!("{snapping} · {}", format::canvas(&state.project.canvas));
    (items, status)
}

/// The hint bar region (`region-hintbar`).
pub struct HintBar {
    root: gtk::Box,
}

impl HintBar {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 20);
        root.set_widget_name("region-hintbar");
        root.add_css_class("qrec-stub");
        root.set_height_request(32);
        root.set_vexpand(false);
        root.set_focusable(false);
        root.set_can_focus(false);
        let items = gtk::Box::new(gtk::Orientation::Horizontal, 20);
        items.set_valign(gtk::Align::Center);
        // The items clip at narrow widths instead of widening the window
        // (no scrollbar; G§3.15's 1000 px windows).
        let clip = gtk::ScrolledWindow::new();
        clip.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
        clip.set_propagate_natural_width(true);
        clip.set_hexpand(true);
        clip.set_child(Some(&items));
        let status = gtk::Label::new(None);
        status.add_css_class("hint-status");
        status.set_valign(gtk::Align::Center);
        status.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        root.append(&clip);
        root.append(&status);
        let shown: std::cell::RefCell<Option<(Vec<HintItem>, String)>> = Default::default();
        let show = {
            let ctx = ctx.clone();
            move || {
                let current = current_items(&ctx);
                if shown.borrow().as_ref() == Some(&current) {
                    return;
                }
                let (hints, text) = current.clone();
                *shown.borrow_mut() = Some(current);
                while let Some(child) = items.first_child() {
                    items.remove(&child);
                }
                for (n, hint) in hints.iter().enumerate() {
                    let item = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    item.add_css_class("hint");
                    if n == 0 {
                        item.add_css_class("first");
                    }
                    if hint.emphasized {
                        item.add_css_class("emphasized");
                    }
                    item.set_sensitive(hint.enabled);
                    for chord in &hint.keys {
                        item.append(&keycap::chord(chord, CapSize::Md, CapVariant::Plain));
                    }
                    item.append(&gtk::Label::new(Some(&hint.label)));
                    items.append(&item);
                }
                status.set_text(&text);
            }
        };
        show();
        let show = std::rc::Rc::new(show);
        let again = std::rc::Rc::clone(&show);
        ctx.store.subscribe(move |_| again()).detach();
        ctx.store.subscribe_local(move |_| show()).detach();
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
