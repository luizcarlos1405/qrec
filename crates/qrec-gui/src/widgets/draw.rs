//! Drawing helpers for custom widgets (G§5.1): the C19 hatch, dashed and
//! rounded rectangles, text. Colors come from the theme's `Palette` (GTK
//! can't look CSS custom properties up from code). Rects are in the
//! widget's px; the design's own metrics (the hatch's stripes and edges)
//! are multiplied by the `scale` a helper takes (`Theme::scale`, D-46), and
//! a caller's widths and dashes must be scaled by the caller.

use gtk::prelude::*;
use gtk::{gdk, graphene, gsk, pango};

/// Which edges of a hatched area get a 1 px line (C19: silence hatches
/// have warning edges at their ends).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
}

/// The C19 hatch's geometry at `scale` (`Theme::scale`): its stripes'
/// pitch and width and its edge lines' width, the design's 6, 2 and 1 px
/// × the scale. The pitch and the width are across the stripes (DESIGN: 2
/// px stripes, 4 px gaps, rotated 45°), so along x the stripes are
/// [`HatchMetrics::step`] = pitch · √2 apart (8.49 px at 100 %, as the
/// frames draw them; they were 6 apart along x, a 4.2 px pitch, until the
/// WP7 integration).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HatchMetrics {
    pub pitch: f32,
    pub stripe: f32,
    pub edge: f32,
}

impl HatchMetrics {
    pub fn at(scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        Self {
            pitch: 6.0 * scale,
            stripe: 2.0 * scale,
            edge: scale,
        }
    }

    /// How far apart the stripes are along x: the pitch across 45° stripes
    /// times √2.
    pub fn step(&self) -> f32 {
        self.pitch * std::f32::consts::SQRT_2
    }

    /// Where the stripes crossing `[x, x + width)` of a `height` px tall
    /// area start at its bottom, a [`step`](Self::step) apart and anchored
    /// to content x = 0 (so neighbouring areas line up); each runs 45° up
    /// to the right.
    pub fn stripe_starts(&self, x: f32, width: f32, height: f32) -> Vec<f32> {
        let step = self.step();
        let mut n = ((x - height) / step).floor();
        let mut starts = Vec::new();
        loop {
            let at = n * step;
            if at >= x + width {
                break;
            }
            starts.push(at);
            n += 1.0;
        }
        starts
    }
}

/// The C19 hatch: 45° stripes 2 px wide, 6 px apart across them (8.49
/// along x; × `scale`, the UI scale), anchored to content x = 0, clipped to
/// `rect`; `edge` lines (1 px × `scale`) where `edges` asks.
pub fn hatch(
    snapshot: &gtk::Snapshot,
    rect: &graphene::Rect,
    stripe: &gdk::RGBA,
    edges: Edges,
    edge: &gdk::RGBA,
    scale: f32,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let metrics = HatchMetrics::at(scale);
    snapshot.push_clip(rect);
    let builder = gsk::PathBuilder::new();
    let (top, bottom) = (rect.y(), rect.y() + rect.height());
    let h = rect.height();
    for x in metrics.stripe_starts(rect.x(), rect.width(), h) {
        builder.move_to(x, bottom);
        builder.line_to(x + h, top);
    }
    let stroke = gsk::Stroke::new(metrics.stripe);
    snapshot.append_stroke(&builder.to_path(), &stroke, stripe);
    snapshot.pop();
    let width = metrics.edge.min(rect.width());
    if edges.left {
        snapshot.append_color(
            edge,
            &graphene::Rect::new(rect.x(), rect.y(), width, rect.height()),
        );
    }
    if edges.right {
        snapshot.append_color(
            edge,
            &graphene::Rect::new(
                rect.x() + rect.width() - width,
                rect.y(),
                width,
                rect.height(),
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The hatch's stripes, pitch and edges follow the UI scale (D-46):
    /// 2 px every 6 px with 1 px edges at 100 %, 3 every 9 with 1.5 at
    /// 150 % (they stayed 2, 6 and 1 next to 1.5× CSS).
    #[test]
    fn the_hatch_scales_with_the_ui() {
        assert_eq!(
            HatchMetrics::at(1.0),
            HatchMetrics {
                pitch: 6.0,
                stripe: 2.0,
                edge: 1.0
            }
        );
        assert_eq!(
            HatchMetrics::at(1.5),
            HatchMetrics {
                pitch: 9.0,
                stripe: 3.0,
                edge: 1.5
            }
        );
        assert_eq!(HatchMetrics::at(f32::NAN), HatchMetrics::at(1.0));
        // Anchored to x = 0: a neighbour's stripes continue these.
        let at_150 = HatchMetrics::at(1.5);
        let step = at_150.step();
        let starts = at_150.stripe_starts(10.0, 20.0, 9.0);
        assert_eq!(starts.len(), 3, "{starts:?}");
        for (n, s) in starts.iter().enumerate() {
            assert!((s - n as f32 * step).abs() < 1e-4, "{starts:?}");
        }
    }

    /// DESIGN's pitch is across the 45° stripes (2 px stripe, 4 px gap): along
    /// x they are 6 · √2 = 8.49 px apart, as the frames draw them (S05's
    /// saving ghost, S14's broken thumbnail: 8.49 px measured; the stripes
    /// used to start every 6 px along x, a 4.2 px pitch).
    #[test]
    fn the_stripes_are_six_px_apart_across_them() {
        let m = HatchMetrics::at(1.0);
        assert!((m.step() - 8.485_281).abs() < 1e-4, "{}", m.step());
        let starts = m.stripe_starts(0.0, 100.0, 54.0);
        // The first stripe that reaches the area starts left of it (its
        // height earlier), the last one before its right end.
        assert!(
            starts[0] <= -54.0 && starts[0] > -54.0 - m.step(),
            "{starts:?}"
        );
        assert!(*starts.last().unwrap() < 100.0);
        for pair in starts.windows(2) {
            let along_x = pair[1] - pair[0];
            // Two parallel 45° lines `along_x` apart along x are
            // along_x / √2 apart across them.
            let across = along_x / std::f32::consts::SQRT_2;
            assert!((across - 6.0).abs() < 1e-3, "{across}");
        }
        // A neighbour starting further right continues the same lattice.
        let right = m.stripe_starts(100.0, 50.0, 54.0);
        for s in &right {
            let k = s / m.step();
            assert!((k - k.round()).abs() < 1e-3, "{s} is off the lattice");
        }
    }
}
