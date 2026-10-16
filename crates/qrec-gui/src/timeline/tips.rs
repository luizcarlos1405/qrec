//! The timeline's tooltips (C33 clip tooltip, the drag tooltips of S10 and
//! S11, the bin drop's): in-window widgets in a layer over the timeline,
//! like the export panel and the toasts (G§3.15), so they follow the
//! pointer during a drag (GTK hides its own tooltips while a button is
//! down) and show without GTK's hover delay in test mode. 8 px below the
//! pointer, kept inside the timeline (above the pointer near its bottom).

use gtk::prelude::*;
use qrec_core::view::HintItem;
use qrec_core::view::timeline::ClipTooltip;

use crate::widgets::keycap::{self, CapSize, CapVariant};

/// A run of a tooltip line in one style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    Plain,
    Strong,
    Muted,
    Mono,
}

/// The tooltip layer.
pub struct Tips {
    /// The layer (over the timeline's ruler and rows).
    pub layer: gtk::Fixed,
    tip: gtk::Box,
}

impl Default for Tips {
    fn default() -> Self {
        Self::new()
    }
}

impl Tips {
    pub fn new() -> Self {
        let layer = gtk::Fixed::new();
        layer.set_can_target(false);
        layer.set_hexpand(true);
        layer.set_vexpand(true);
        let tip = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tip.set_widget_name("timeline-tooltip");
        tip.add_css_class("qrec-tl-tooltip");
        tip.set_can_target(false);
        tip.set_visible(false);
        layer.put(&tip, 0.0, 0.0);
        Self { layer, tip }
    }

    fn clear(&self) {
        while let Some(child) = self.tip.first_child() {
            self.tip.remove(&child);
        }
    }

    /// Shows lines of styled runs 8 px under (`x`, `y`), layer px.
    pub fn show(&self, lines: &[Vec<(String, Ink)>], (x, y): (f64, f64)) {
        self.clear();
        for line in lines {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            for (text, ink) in line {
                row.append(&run(text, *ink));
            }
            self.tip.append(&row);
        }
        self.place(x, y);
    }

    /// The clip tooltip (S06, S07): "Take 5 · c2d1", the times in mono, the
    /// keys.
    pub fn show_clip(&self, tip: &ClipTooltip, (x, y): (f64, f64)) {
        self.clear();
        self.tip.append(&run(&tip.title, Ink::Strong));
        self.tip.append(&run(&tip.times, Ink::Mono));
        // The keys as the S06 and S07 frames draw them: no separators.
        let keys = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        keys.add_css_class("tl-tip-keys");
        for item in &tip.keys {
            keys.append(&hint(item));
        }
        self.tip.append(&keys);
        self.place(x, y);
    }

    pub fn hide(&self) {
        self.tip.set_visible(false);
    }

    fn place(&self, x: f64, y: f64) {
        self.tip.set_visible(true);
        // Its rows changed while it was hidden, which queues no resize: its
        // cached size is still the old content's. Measured stale, the first
        // tooltip after a hide (a clip's after a drop's) was placed and
        // allocated at the previous one's size for a frame ("Allocation
        // width too small" for its 322 px times line; WP7 integration audit).
        self.tip.queue_resize();
        let (_, natural) = self.tip.preferred_size();
        let (w, h) = (f64::from(natural.width()), f64::from(natural.height()));
        let (lw, lh) = (
            f64::from(self.layer.width()),
            f64::from(self.layer.height()),
        );
        let mut left = x;
        if lw > 0.0 && left + w > lw - 4.0 {
            left = (lw - 4.0 - w).max(0.0);
        }
        let mut top = y + 8.0;
        if lh > 0.0 && top + h > lh {
            top = (y - 8.0 - h).max(0.0);
        }
        self.layer.move_(&self.tip, left, top);
    }
}

fn run(text: &str, ink: Ink) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    match ink {
        Ink::Plain => {}
        Ink::Strong => label.add_css_class("tl-tip-strong"),
        Ink::Muted => label.add_css_class("muted"),
        Ink::Mono => label.add_css_class("t-mono-sm"),
    }
    label
}

fn hint(item: &HintItem) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    b.add_css_class("tl-tip-hint");
    for chord in &item.keys {
        b.append(&keycap::chord(chord, CapSize::Sm, CapVariant::Plain));
    }
    let label = gtk::Label::new(Some(&item.label));
    label.add_css_class("muted");
    b.append(&label);
    b
}

/// The trim tooltip's lines (S10): "In" strong, the frame in plain, the
/// timecode in mono; "Δ −29 f" in mono.
pub fn trim_lines(lines: &[String; 2]) -> Vec<Vec<(String, Ink)>> {
    let first = &lines[0];
    let mut runs = Vec::new();
    match (first.find(" \u{00b7} "), first.rfind(" \u{00b7} ")) {
        (Some(a), Some(b)) if b > a => {
            runs.push((first[..a].to_owned(), Ink::Strong));
            runs.push((first[a..b + " \u{00b7} ".len()].to_owned(), Ink::Plain));
            runs.push((first[b + " \u{00b7} ".len()..].to_owned(), Ink::Mono));
        }
        _ => runs.push((first.clone(), Ink::Plain)),
    }
    vec![runs, vec![(lines[1].clone(), Ink::Mono)]]
}

/// The move tooltip's lines (S11): the slot, then "Starts at" muted and the
/// timecode in mono.
pub fn move_lines(lines: &[String; 2]) -> Vec<Vec<(String, Ink)>> {
    let second = &lines[1];
    let split = second.rfind(' ').map_or(0, |i| i + 1);
    vec![
        vec![(lines[0].clone(), Ink::Plain)],
        vec![
            (second[..split].to_owned(), Ink::Muted),
            (second[split..].to_owned(), Ink::Mono),
        ],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_lines_split_into_runs() {
        let trim = trim_lines(&[
            "In \u{00b7} take 5 frame 41 \u{00b7} 00:00:01:11".into(),
            "\u{0394} \u{2212}29 f".into(),
        ]);
        assert_eq!(
            trim[0],
            [
                ("In".to_owned(), Ink::Strong),
                (" \u{00b7} take 5 frame 41 \u{00b7} ".to_owned(), Ink::Plain),
                ("00:00:01:11".to_owned(), Ink::Mono),
            ]
        );
        assert_eq!(trim[1], [("\u{0394} \u{2212}29 f".to_owned(), Ink::Mono)]);
        let moved = move_lines(&[
            "Move after c1b6 \u{00b7} slot 7 of 10".into(),
            "Starts at 00:00:54:07".into(),
        ]);
        assert_eq!(
            moved[1],
            [
                ("Starts at ".to_owned(), Ink::Muted),
                ("00:00:54:07".to_owned(), Ink::Mono),
            ]
        );
    }
}
