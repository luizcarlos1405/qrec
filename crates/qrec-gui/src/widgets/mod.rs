//! Shared components (G§5.1). Owners per file in G§3.16: WP0b's are
//! `button` (C01 helpers), `keycap` (C02), `tooltip` (C33), `spinner` (C01
//! spinner), `chip` (C22) and `draw` (C19 hatch and drawing helpers);
//! `numeric`, `meter`, `scale_tooltip` are WP5's, `mud_bar` WP7's,
//! `theme_card` WP9's. Their look is CSS (`components/common.css`) and the
//! theme's palette.

pub mod button;
pub mod chip;
pub mod draw;
pub mod keycap;
pub mod meter;
pub mod mud_bar;
pub mod numeric;
pub mod scale_tooltip;
pub mod spinner;
pub mod theme_card;
pub mod tooltip;

use gtk::prelude::*;

pub use chip::Chip;
pub use keycap::KeyCap;
pub use spinner::Spinner;

/// A region that isn't built yet (WP0b's stubs, G§3.16): a box named
/// `widget_name` showing `label`, the size its owner gives it.
pub fn stub_region(widget_name: &str, label: &str) -> gtk::Box {
    let region = gtk::Box::new(gtk::Orientation::Vertical, 0);
    region.set_widget_name(widget_name);
    region.add_css_class("qrec-stub");
    // Regions take focus: they need a name for screen readers ("Timeline").
    let name = label.split(" (").next().unwrap_or(label);
    region.update_property(&[gtk::accessible::Property::Label(name)]);
    let text = gtk::Label::new(Some(label));
    text.add_css_class("qrec-stub-label");
    text.set_vexpand(true);
    text.set_valign(gtk::Align::Center);
    region.append(&text);
    region
}
