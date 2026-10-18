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
//! while the window opens. When the window is too narrow for every item
//! (G§3.15's 1000 px windows), the trailing items that don't fit whole are
//! left out ([`fitting`]): an item is never cut in half (WP7 integration:
//! the bar used to clip them, so at 1000 px the `?` cap showed without its
//! "All keys"); [`shown`] says what is drawn.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use qrec_core::view::blocking;
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
        // Each page's frame (`view::blocking::hints`, WP8).
        StartupPhase::Blocked(kind) => HintState::Blocking {
            page: blocking::hints(kind),
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
        // At narrow widths the items that don't fit whole are left out,
        // instead of widening the window (G§3.15's 1000 px windows).
        let items = HintItems::new(ITEM_GAP);
        items.add_css_class("hint-items");
        items.set_widget_name(ITEMS);
        items.set_valign(gtk::Align::Center);
        items.set_hexpand(true);
        items.set_can_focus(false);
        let status = gtk::Label::new(None);
        status.add_css_class("hint-status");
        status.add_css_class("t-caption");
        status.add_css_class("muted");
        status.set_valign(gtk::Align::Center);
        status.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        root.append(&items);
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
                items.clear();
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

/// The items' container (its widget name).
pub const ITEMS: &str = "hint-items";
/// The gap between two items (DESIGN Hint bar: 20 px apart).
const ITEM_GAP: i32 = 20;

/// How many of the items `widths` px wide (in order, `gap` px apart) fit
/// whole in `room` px: the first that doesn't, and every one after it, is
/// left out (the order is the hints' priority, G§4.3).
pub fn fitting(widths: &[i32], gap: i32, room: i32) -> usize {
    let mut used = 0;
    for (n, w) in widths.iter().enumerate() {
        let need = if n == 0 { *w } else { used + gap + w };
        if need > room {
            return n;
        }
        used = need;
    }
    widths.len()
}

/// The labels of the items the bar draws now (whole), and whether any
/// drawn item runs past the bar's room for them (never, as built): the test
/// channel's `hints` op reports both. Empty without a hint bar under
/// `root`.
pub fn shown(root: &gtk::Widget) -> (Vec<String>, bool) {
    let Some(items) = crate::window::find_named(root, ITEMS) else {
        return (Vec::new(), false);
    };
    let room = items.width();
    let mut labels = Vec::new();
    let mut cut = false;
    let mut child = items.first_child();
    while let Some(c) = child {
        if c.is_visible() && c.is_child_visible() && c.is_mapped() {
            if let Some(b) = c.compute_bounds(&items) {
                cut |= b.x() < 0.0 || f64::from(b.x() + b.width()) > f64::from(room) + 0.5;
            }
            // The item's label is its last child.
            if let Some(label) = c.last_child().and_downcast::<gtk::Label>() {
                labels.push(label.text().to_string());
            }
        }
        child = c.next_sibling();
    }
    (labels, cut)
}

mod imp {
    use std::cell::Cell;

    use super::*;

    /// The hint items in a row, `gap` px apart; the trailing ones that
    /// don't fit whole are left out (`fitting`): not drawn, not
    /// allocated (`child-visible` false, like a stack's hidden pages).
    #[derive(Default)]
    pub struct HintItems {
        pub gap: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HintItems {
        const NAME: &'static str = "QrecHintItems";
        type Type = super::HintItems;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for HintItems {
        fn dispose(&self) {
            crate::guard::run("hint items: dispose", || {
                while let Some(child) = self.obj().first_child() {
                    child.unparent();
                }
            });
        }
    }

    impl HintItems {
        fn children(&self) -> Vec<gtk::Widget> {
            let mut out = Vec::new();
            let mut child = self.obj().first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                if c.should_layout() {
                    out.push(c);
                }
            }
            out
        }
    }

    impl WidgetImpl for HintItems {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            crate::guard::catch("hint items: measure", || {
                let children = self.children();
                match orientation {
                    // As narrow as nothing (the items that don't fit are
                    // left out), as wide as all of them by nature.
                    gtk::Orientation::Horizontal => {
                        let gaps = self.gap.get() * (children.len() as i32 - 1).max(0);
                        let natural: i32 = children
                            .iter()
                            .map(|c| c.measure(orientation, -1).1)
                            .sum::<i32>()
                            + gaps;
                        (0, natural, -1, -1)
                    }
                    _ => children.iter().fold((0, 0, -1, -1), |(min, nat, _, _), c| {
                        let (m, n, _, _) = c.measure(orientation, -1);
                        (min.max(m), nat.max(n), -1, -1)
                    }),
                }
            })
            .unwrap_or((0, 0, -1, -1))
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            crate::guard::run("hint items: allocate", || {
                let children = self.children();
                let widths: Vec<i32> = children
                    .iter()
                    .map(|c| c.measure(gtk::Orientation::Horizontal, -1).1)
                    .collect();
                let fit = fitting(&widths, self.gap.get(), width);
                let mut x = 0;
                for (n, (child, w)) in children.iter().zip(&widths).enumerate() {
                    let whole = n < fit;
                    child.set_child_visible(whole);
                    if whole {
                        let at = gtk::gsk::Transform::new()
                            .translate(&gtk::graphene::Point::new(x as f32, 0.0));
                        child.allocate(*w, height, -1, Some(at));
                        x += w + self.gap.get();
                    }
                }
            });
        }
    }
}

glib::wrapper! {
    /// The hint bar's items: a row that leaves out what doesn't fit whole.
    pub struct HintItems(ObjectSubclass<imp::HintItems>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HintItems {
    fn new(gap: i32) -> Self {
        let items: Self = glib::Object::new();
        items.imp().gap.set(gap);
        items
    }

    fn append(&self, child: &impl IsA<gtk::Widget>) {
        child.set_parent(self);
    }

    fn clear(&self) {
        while let Some(child) = self.first_child() {
            child.unparent();
        }
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

#[cfg(test)]
mod tests {
    use super::fitting;

    /// Items fit whole or not at all, in order: the first that doesn't fit
    /// and every one after it are left out (a 1000 px window's `?` used to
    /// show its cap without its label).
    #[test]
    fn trailing_items_that_dont_fit_whole_are_left_out() {
        let widths = [60, 90, 120, 70];
        // 60 + 20 + 90 + 20 + 120 + 20 + 70 = 400.
        assert_eq!(fitting(&widths, 20, 400), 4);
        assert_eq!(fitting(&widths, 20, 399), 3, "the last one, 1 px short");
        assert_eq!(fitting(&widths, 20, 310), 3);
        assert_eq!(fitting(&widths, 20, 309), 2);
        assert_eq!(fitting(&widths, 20, 59), 0);
        assert_eq!(fitting(&[], 20, 100), 0);
        // A small item after a big one that doesn't fit stays out too: the
        // order is the hints' priority.
        assert_eq!(fitting(&[60, 300, 10], 20, 150), 1);
    }
}
