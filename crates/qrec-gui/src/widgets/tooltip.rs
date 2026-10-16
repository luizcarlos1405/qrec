//! C33 tooltips (G§5.1): a text, or a text with key caps, built on
//! `query-tooltip` and `Tooltip::set_custom` so the caps are real widgets.
//! (The clip tooltip is the timeline's, WP3.)

use gtk::prelude::*;

use super::keycap::{self, CapSize, CapVariant};

/// A plain text tooltip.
pub fn text(widget: &impl IsA<gtk::Widget>, text: &str) {
    widget.set_tooltip_text(Some(text));
}

/// A tooltip with `text` and key caps after it: each chord in `keys` is a
/// row of caps ("Commands" `Ctrl` `K`).
pub fn keys(widget: &impl IsA<gtk::Widget>, text: &str, keys: &[Vec<String>]) {
    let widget = widget.as_ref();
    widget.set_has_tooltip(true);
    let text = text.to_owned();
    let keys = keys.to_vec();
    widget.connect_query_tooltip(crate::guarded!("a tooltip", move |_, _, _, _, tooltip| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.add_css_class("qrec-tooltip");
        row.append(&gtk::Label::new(Some(&text)));
        for chord in &keys {
            row.append(&keycap::chord(chord, CapSize::Sm, CapVariant::Plain));
        }
        tooltip.set_custom(Some(&row));
        true
    }));
}
