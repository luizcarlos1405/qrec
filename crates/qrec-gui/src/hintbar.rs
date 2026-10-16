//! The hint bar (C03, G§4.3, D-02): the keys of what can be done now, from
//! `view::hints`, and the status on the right. Owner: WP6.
//!
//! A 32 px `surface` strip under everything (`region-hintbar`, never
//! focusable, so it never anchors a tooltip; BRIEF S01). Items from x 12:
//! key caps (2 px apart in a chord, 4 px between chords), 6 px, the
//! `body-sm` label; 20 px between items. The first label is `text`, the
//! others `muted` unless emphasized; disabled items are at 0.45. The
//! status ("Snapping on · 1920×1080 · 30 fps") is `caption` `muted`,
//! right-aligned at x 1588.
//!
//! [`current_items`] is what the bar renders (the test channel's `hints`
//! op reports it): the blocking pages' hints while a page shows, nothing
//! while the window opens.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use qrec_core::view::blocking::BlockKind;
use qrec_core::view::hints::{HintState, hint_bar, hint_state};
use qrec_core::view::{HintItem, OverlayKind};

use crate::Ctx;
use crate::store::local::StartupPhase;
use crate::widgets::keycap::{self, CapSize, CapVariant};

/// The hint items and the status text the bar shows now.
pub fn current_items(ctx: &Ctx) -> (Vec<HintItem>, String) {
    let state = ctx.store.state();
    let keymap = ctx.store.keymap();
    let local = ctx.store.local();
    let mut facts = local.facts();
    // A menu or a dropdown's list open inside an overlay (Settings' Size)
    // keeps that overlay's hints.
    if facts.top_overlay == Some(OverlayKind::Menu)
        && let Some(under) = local.overlays.iter().rev().nth(1)
    {
        facts.top_overlay = Some(under.kind());
    }
    let hint = match &local.startup {
        // The placeholder state says nothing of the project yet.
        StartupPhase::Opening { .. } => return (Vec::new(), String::new()),
        StartupPhase::Blocked(kind) => HintState::Blocking {
            check_again: matches!(kind, BlockKind::MissingDeps { .. }),
        },
        StartupPhase::Open | StartupPhase::Closing => hint_state(&state, &facts),
    };
    hint_bar(&hint, &state, &facts, &keymap)
}

/// What the bar last rendered: its items and status.
type Shown = Option<(Vec<HintItem>, String)>;

/// The hint bar region (`region-hintbar`).
pub struct HintBar {
    root: gtk::Box,
}

impl HintBar {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 20);
        root.set_widget_name("region-hintbar");
        root.add_css_class("hintbar");
        root.set_height_request(32);
        root.set_vexpand(false);
        root.set_focusable(false);
        root.set_can_focus(false);
        let items = gtk::Box::new(gtk::Orientation::Horizontal, 20);
        items.add_css_class("hint-items");
        items.set_valign(gtk::Align::Center);
        // The items clip at narrow widths instead of widening the window
        // (no scrollbar; G§3.15's 1000 px windows).
        let clip = gtk::ScrolledWindow::new();
        clip.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
        clip.set_propagate_natural_width(true);
        clip.set_hexpand(true);
        clip.set_can_focus(false);
        clip.set_child(Some(&items));
        let status = gtk::Label::new(None);
        status.add_css_class("hint-status");
        status.add_css_class("t-caption");
        status.add_css_class("muted");
        status.set_valign(gtk::Align::Center);
        status.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        root.append(&clip);
        root.append(&status);
        let shown: Rc<RefCell<Shown>> = Rc::default();
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
                    items.append(&item(hint, n == 0));
                }
                status.set_text(&text);
            }
        };
        show();
        let show = Rc::new(show);
        let again = Rc::clone(&show);
        ctx.store.subscribe(move |_| again()).detach();
        ctx.store.subscribe_local(move |_| show()).detach();
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

/// One hint: its chords, then its label.
fn item(hint: &HintItem, first: bool) -> gtk::Box {
    let item = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    item.add_css_class("hint");
    if first {
        item.add_css_class("first");
    }
    if hint.emphasized {
        item.add_css_class("emphasized");
    }
    if !hint.keys.is_empty() {
        let chords = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        chords.set_valign(gtk::Align::Center);
        for chord in &hint.keys {
            chords.append(&keycap::chord(chord, CapSize::Md, CapVariant::Plain));
        }
        item.append(&chords);
    }
    let label = gtk::Label::new(Some(&hint.label));
    label.add_css_class("hint-label");
    label.set_valign(gtk::Align::Center);
    item.append(&label);
    item.set_sensitive(hint.enabled);
    item
}
