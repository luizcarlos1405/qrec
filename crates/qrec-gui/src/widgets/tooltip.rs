//! C33 tooltips (G§5.1): a text, or a text with key caps, built on
//! `query-tooltip` and `Tooltip::set_custom` so the caps are real widgets.
//! (The clip tooltip is the timeline's, WP3.)

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use super::keycap::{self, CapSize, CapVariant};

/// A plain text tooltip.
pub fn text(widget: &impl IsA<gtk::Widget>, text: &str) {
    widget.set_tooltip_text(Some(text));
}

/// The keys a [`keys`] tooltip shows; they change with the keymap (WP10).
#[derive(Clone, Debug, Default)]
pub struct KeysTip(Rc<RefCell<Vec<Vec<String>>>>);

impl KeysTip {
    /// Shows `keys` from the next time the tooltip shows (none: the
    /// widget's text tooltip, if it has one).
    pub fn set(&self, keys: &[Vec<String>]) {
        *self.0.borrow_mut() = keys.to_vec();
    }
}

/// A tooltip with `text` and key caps after it: each chord in `keys` is a
/// row of caps ("Commands" `Ctrl` `K`); with no keys, the widget's own text
/// tooltip shows instead. The handle changes the keys.
pub fn keys(widget: &impl IsA<gtk::Widget>, text: &str, keys: &[Vec<String>]) -> KeysTip {
    let widget = widget.as_ref();
    widget.set_has_tooltip(true);
    let text = text.to_owned();
    let tip = KeysTip::default();
    tip.set(keys);
    let shown = tip.clone();
    widget.connect_query_tooltip(crate::guarded!("a tooltip", move |_, _, _, _, tooltip| {
        let keys = shown.0.borrow();
        if keys.is_empty() {
            return false;
        }
        // 8 px apart: `.qrec-tooltip`'s CSS border-spacing (GtkBox adds it
        // to its own spacing, which stays 0).
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("qrec-tooltip");
        row.append(&gtk::Label::new(Some(&text)));
        for chord in keys.iter() {
            row.append(&keycap::chord(chord, CapSize::Sm, CapVariant::Plain));
        }
        tooltip.set_custom(Some(&row));
        true
    }));
    tip
}
