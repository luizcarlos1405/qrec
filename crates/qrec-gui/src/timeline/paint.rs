//! Drawing helpers of the timeline (C15–C19): text in the type roles, the
//! label plates, hatches with a phase (the saving ghost's moving stripes),
//! the 270° spinner arc, symbolic icons, waveforms from peak levels. Every
//! color comes from the theme's `Palette`; geometry is in 100 % px (the
//! caller scales the snapshot by the UI scale, D-46).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::f32::consts::PI;

use gtk::prelude::*;
use gtk::{gdk, graphene, gsk, pango};

/// A rect in 100 % px.
pub fn rect(x: f64, y: f64, w: f64, h: f64) -> graphene::Rect {
    graphene::Rect::new(x as f32, y as f32, w.max(0.0) as f32, h.max(0.0) as f32)
}

/// The type roles the timeline draws text in (DESIGN Typography).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// `mono-xs` 11/14 500: ruler labels, frame numbers.
    MonoXs,
    /// `mono-sm` 12/16 500: tags, the scrub flag.
    MonoSm,
    /// `caption-strong` 12/16 600: take labels on clips.
    CaptionStrong,
    /// `caption` 12/16 400.
    Caption,
}

impl Role {
    fn font(self) -> pango::FontDescription {
        let (family, size, weight) = match self {
            Role::MonoXs => (MONO, 11.0, pango::Weight::Medium),
            Role::MonoSm => (MONO, 12.0, pango::Weight::Medium),
            Role::CaptionStrong => (UI, 12.0, pango::Weight::Semibold),
            Role::Caption => (UI, 12.0, pango::Weight::Normal),
        };
        let mut font = pango::FontDescription::new();
        font.set_family(family);
        font.set_absolute_size(size * f64::from(pango::SCALE));
        font.set_weight(weight);
        font
    }

    /// The role's line height.
    pub fn line(self) -> f64 {
        match self {
            Role::MonoXs => 14.0,
            Role::MonoSm | Role::CaptionStrong | Role::Caption => 16.0,
        }
    }
}

/// The bundled families first, then DESIGN's fallbacks.
const MONO: &str = "JetBrains Mono, DejaVu Sans Mono, monospace";
const UI: &str = "Atkinson Hyperlegible Next, Atkinson Hyperlegible, Cantarell, sans-serif";

/// Pango layouts by role and text, made once (the canvas redraws often).
#[derive(Default)]
pub struct Texts {
    cache: RefCell<BTreeMap<(Role, String), pango::Layout>>,
}

impl Texts {
    /// The layout of `text` in `role`, on `widget`'s Pango context.
    pub fn layout(&self, widget: &gtk::Widget, role: Role, text: &str) -> pango::Layout {
        let key = (role, text.to_owned());
        if let Some(layout) = self.cache.borrow().get(&key) {
            return layout.clone();
        }
        let layout = widget.create_pango_layout(Some(text));
        layout.set_font_description(Some(&role.font()));
        let mut cache = self.cache.borrow_mut();
        if cache.len() > 4096 {
            cache.clear();
        }
        cache.insert(key, layout.clone());
        layout
    }

    /// The text's logical width and height (px).
    pub fn size(&self, widget: &gtk::Widget, role: Role, text: &str) -> (f64, f64) {
        let (_, logical) = self.layout(widget, role, text).pixel_extents();
        (f64::from(logical.width()), f64::from(logical.height()))
    }

    /// Draws `text` with its line box's top-left at (x, y), vertically
    /// centered in the role's line height.
    pub fn draw(
        &self,
        snapshot: &gtk::Snapshot,
        widget: &gtk::Widget,
        role: Role,
        text: &str,
        (x, y): (f64, f64),
        color: &gdk::RGBA,
    ) -> f64 {
        let layout = self.layout(widget, role, text);
        let (_, logical) = layout.pixel_extents();
        let dy = (role.line() - f64::from(logical.height())) / 2.0;
        snapshot.save();
        snapshot.translate(&graphene::Point::new(x as f32, (y + dy) as f32));
        snapshot.append_layout(&layout, color);
        snapshot.restore();
        f64::from(logical.width())
    }

    /// Forgets the layouts (the theme or the fonts changed).
    pub fn clear(&self) {
        self.cache.borrow_mut().clear();
    }
}

/// A filled rounded rect.
pub fn rounded(snapshot: &gtk::Snapshot, r: &graphene::Rect, radius: f32, color: &gdk::RGBA) {
    let rounded = gsk::RoundedRect::from_rect(*r, radius);
    snapshot.push_rounded_clip(&rounded);
    snapshot.append_color(color, r);
    snapshot.pop();
}

/// An inner stroke `width` px wide (C16's selection, hover).
pub fn inner_stroke(snapshot: &gtk::Snapshot, r: &graphene::Rect, width: f32, color: &gdk::RGBA) {
    let (x, y, w, h) = (r.x(), r.y(), r.width(), r.height());
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let width = width.min(w / 2.0).min(h / 2.0);
    for side in [
        graphene::Rect::new(x, y, w, width),
        graphene::Rect::new(x, y + h - width, w, width),
        graphene::Rect::new(x, y + width, width, h - 2.0 * width),
        graphene::Rect::new(x + w - width, y + width, width, h - 2.0 * width),
    ] {
        snapshot.append_color(color, &side);
    }
}

/// A dashed outline inside `r` (`width` px, `dash` on/off).
pub fn dashed(
    snapshot: &gtk::Snapshot,
    r: &graphene::Rect,
    width: f32,
    dash: &[f32],
    color: &gdk::RGBA,
) {
    crate::widgets::draw::dashed_rect(snapshot, r, color, width, dash);
}

/// The C19 hatch shifted by `phase` px (0 for a still one): 45° stripes 2
/// px wide every 6 px, clipped to `r`.
pub fn hatch(snapshot: &gtk::Snapshot, r: &graphene::Rect, stripe: &gdk::RGBA, phase: f32) {
    const PITCH: f32 = 6.0;
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return;
    }
    snapshot.push_clip(r);
    let builder = gsk::PathBuilder::new();
    let (top, bottom, h) = (r.y(), r.y() + r.height(), r.height());
    let phase = phase.rem_euclid(PITCH);
    let mut x = ((r.x() - h) / PITCH).floor() * PITCH + phase - PITCH;
    while x < r.x() + r.width() {
        builder.move_to(x, bottom);
        builder.line_to(x + h, top);
        x += PITCH;
    }
    snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(2.0), stripe);
    snapshot.pop();
}

/// A 1 px line on each end of `r` (a silence hatch's warning edges).
pub fn edges(snapshot: &gtk::Snapshot, r: &graphene::Rect, color: &gdk::RGBA) {
    snapshot.append_color(color, &graphene::Rect::new(r.x(), r.y(), 1.0, r.height()));
    snapshot.append_color(
        color,
        &graphene::Rect::new(r.x() + r.width() - 1.0, r.y(), 1.0, r.height()),
    );
}

/// The C01 spinner's arc (270°, one turn a second at `now_ms`), `size` px
/// across with its top-left at (x, y).
pub fn spinner(
    snapshot: &gtk::Snapshot,
    (x, y): (f64, f64),
    size: f64,
    now_ms: u64,
    color: &gdk::RGBA,
) {
    let size = size as f32;
    let stroke_width = (size / 8.0).max(1.5);
    let radius = (size - stroke_width) / 2.0;
    let turn = (now_ms % 1000) as f32 / 1000.0;
    snapshot.save();
    snapshot.translate(&graphene::Point::new(
        x as f32 + size / 2.0,
        y as f32 + size / 2.0,
    ));
    snapshot.rotate(turn * 360.0);
    let builder = gsk::PathBuilder::new();
    let point = |a: f32| (radius * a.cos(), radius * a.sin());
    let (x0, y0) = point(-PI / 2.0);
    let (x1, y1) = point(PI);
    builder.move_to(x0, y0);
    builder.svg_arc_to(radius, radius, 0.0, true, true, x1, y1);
    let stroke = gsk::Stroke::new(stroke_width);
    stroke.set_line_cap(gsk::LineCap::Round);
    snapshot.append_stroke(&builder.to_path(), &stroke, color);
    snapshot.restore();
}

/// A symbolic icon `size` px in `color`, top-left at (x, y).
pub fn icon(
    snapshot: &gtk::Snapshot,
    widget: &gtk::Widget,
    name: &str,
    (x, y): (f64, f64),
    size: i32,
    color: &gdk::RGBA,
) {
    let theme = gtk::IconTheme::for_display(&widget.display());
    let paintable = theme.lookup_icon(
        name,
        &[],
        size,
        widget.scale_factor(),
        gtk::TextDirection::Ltr,
        gtk::IconLookupFlags::empty(),
    );
    snapshot.save();
    snapshot.translate(&graphene::Point::new(x as f32, y as f32));
    paintable.snapshot_symbolic(snapshot, f64::from(size), f64::from(size), &[*color]);
    snapshot.restore();
}

/// A label plate (C16's take label, tags): a filled `sm` rounded rect with
/// `text` in `role`, its left at `x` and top at `y`, 18 px tall; returns its
/// width. `lead` reserves room before the text (a dot, a spinner).
#[allow(clippy::too_many_arguments)]
pub fn plate(
    snapshot: &gtk::Snapshot,
    widget: &gtk::Widget,
    texts: &Texts,
    role: Role,
    text: &str,
    (x, y): (f64, f64),
    lead: f64,
    fill: &gdk::RGBA,
    ink: &gdk::RGBA,
) -> f64 {
    let (tw, _) = texts.size(widget, role, text);
    let w = tw + lead + 2.0 * PLATE_PAD;
    rounded(snapshot, &rect(x, y, w, PLATE_H), 4.0, fill);
    texts.draw(
        snapshot,
        widget,
        role,
        text,
        (x + PLATE_PAD + lead, y + (PLATE_H - role.line()) / 2.0),
        ink,
    );
    w
}

/// A plate's height and side padding.
pub const PLATE_H: f64 = 18.0;
pub const PLATE_PAD: f64 = 5.0;

/// A plate's width for `text` (with `lead` before it).
pub fn plate_width(widget: &gtk::Widget, texts: &Texts, role: Role, text: &str, lead: f64) -> f64 {
    texts.size(widget, role, text).0 + lead + 2.0 * PLATE_PAD
}

/// The mirrored waveform of `amps` (0..1 per px column from `x0`), around
/// `center`, at most `half` px from it, within `[lo, hi)` horizontally: a
/// rect per run of columns of the same height (color nodes, cheap for GSK
/// to draw at any zoom, where a path fill is rasterized each frame).
pub fn waveform(
    snapshot: &gtk::Snapshot,
    x0: f64,
    (lo, hi): (f64, f64),
    center: f64,
    half: f64,
    amps: &[f32],
    color: &gdk::RGBA,
) {
    let height = |a: f32| f64::from(a.max(0.02)) * half;
    let mut i = 0;
    while i < amps.len() {
        let h = height(amps[i]);
        let mut j = i + 1;
        while j < amps.len() && (height(amps[j]) - h).abs() < 0.25 {
            j += 1;
        }
        let left = (x0 + i as f64).max(lo);
        let right = (x0 + j as f64).min(hi);
        if right > left {
            snapshot.append_color(color, &rect(left, center - h, right - left, 2.0 * h));
        }
        i = j;
    }
}

/// A triangle (the caret's ends): `tip` pointing down when `down`.
pub fn triangle(
    snapshot: &gtk::Snapshot,
    (x, y): (f64, f64),
    w: f64,
    h: f64,
    down: bool,
    color: &gdk::RGBA,
) {
    let builder = gsk::PathBuilder::new();
    let (x, y, w, h) = (x as f32, y as f32, w as f32, h as f32);
    if down {
        builder.move_to(x - w / 2.0, y);
        builder.line_to(x + w / 2.0, y);
        builder.line_to(x, y + h);
    } else {
        builder.move_to(x - w / 2.0, y + h);
        builder.line_to(x + w / 2.0, y + h);
        builder.line_to(x, y);
    }
    builder.close();
    snapshot.append_fill(&builder.to_path(), gsk::FillRule::Winding, color);
}
