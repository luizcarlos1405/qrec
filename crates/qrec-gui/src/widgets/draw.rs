//! Drawing helpers for custom widgets (G§5.1): the C19 hatch, dashed and
//! rounded rectangles, text. Colors come from the theme's `Palette` (GTK
//! can't look CSS custom properties up from code); geometry is in the
//! widget's px (already × the UI scale).

use gtk::prelude::*;
use gtk::{gdk, graphene, gsk, pango};

/// Which edges of a hatched area get a 1 px line (C19: silence hatches
/// have warning edges at their ends).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
}

/// The C19 hatch: 45° stripes 2 px wide every 6 px, anchored to content x
/// = 0 (so neighbouring areas line up), clipped to `rect`; `edge` lines
/// where `edges` asks.
pub fn hatch(
    snapshot: &gtk::Snapshot,
    rect: &graphene::Rect,
    stripe: &gdk::RGBA,
    edges: Edges,
    edge: &gdk::RGBA,
) {
    const PITCH: f32 = 6.0;
    const WIDTH: f32 = 2.0;
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    snapshot.push_clip(rect);
    let builder = gsk::PathBuilder::new();
    let (top, bottom) = (rect.y(), rect.y() + rect.height());
    let h = rect.height();
    // Stripes run from bottom-left to top-right; start where the first one
    // can reach the rect.
    let first = ((rect.x() - h) / PITCH).floor() * PITCH;
    let mut x = first;
    while x < rect.x() + rect.width() {
        builder.move_to(x, bottom);
        builder.line_to(x + h, top);
        x += PITCH;
    }
    let stroke = gsk::Stroke::new(WIDTH);
    snapshot.append_stroke(&builder.to_path(), &stroke, stripe);
    snapshot.pop();
    if edges.left {
        snapshot.append_color(
            edge,
            &graphene::Rect::new(rect.x(), rect.y(), 1.0, rect.height()),
        );
    }
    if edges.right {
        snapshot.append_color(
            edge,
            &graphene::Rect::new(rect.x() + rect.width() - 1.0, rect.y(), 1.0, rect.height()),
        );
    }
}

/// A dashed outline (`width` px, dashes `dash` px on/off), inset by half
/// the width so it stays inside `rect`.
pub fn dashed_rect(
    snapshot: &gtk::Snapshot,
    rect: &graphene::Rect,
    color: &gdk::RGBA,
    width: f32,
    dash: &[f32],
) {
    let inset = width / 2.0;
    let builder = gsk::PathBuilder::new();
    builder.add_rect(&graphene::Rect::new(
        rect.x() + inset,
        rect.y() + inset,
        (rect.width() - width).max(0.0),
        (rect.height() - width).max(0.0),
    ));
    let stroke = gsk::Stroke::new(width);
    stroke.set_dash(dash);
    snapshot.append_stroke(&builder.to_path(), &stroke, color);
}

/// A filled rounded rectangle.
pub fn rounded_rect(
    snapshot: &gtk::Snapshot,
    rect: &graphene::Rect,
    radius: f32,
    color: &gdk::RGBA,
) {
    let rounded = gsk::RoundedRect::from_rect(*rect, radius);
    snapshot.push_rounded_clip(&rounded);
    snapshot.append_color(color, rect);
    snapshot.pop();
}

/// `layout` drawn at (x, y) in `color`.
pub fn text(snapshot: &gtk::Snapshot, layout: &pango::Layout, x: f32, y: f32, color: &gdk::RGBA) {
    snapshot.save();
    snapshot.translate(&graphene::Point::new(x, y));
    snapshot.append_layout(layout, color);
    snapshot.restore();
}
