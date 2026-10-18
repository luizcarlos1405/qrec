//! The preview picture (S02, S06, D-27): the project's canvas background,
//! the live or the program paintable fitted into it (a sink's picture is at
//! most the canvas size: it is letterboxed, never cropped), S24's hatch
//! over the refused screens' areas, and the preview's frame: a 1 px
//! `border` stroke outside it, or S10's 2 px `accent` stroke during a trim
//! scrub.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use qrec_core::view::Rect;

use crate::theme::Palette;
use crate::widgets::draw::HatchMetrics;

/// S24's hatch (`stage::denied`): disjoint rects of the canvas (canvas px,
/// scaled to the picture's size when it draws, so an allocation never
/// lags) in `muted-20`, C19's stripes at the UI scale (`scale`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hatch {
    pub canvas: (f64, f64),
    pub rects: Vec<Rect>,
    pub scale: f64,
}

/// The 45° stripes (rising to the right) of a hatch at `metrics` that
/// cross `rect` (widget px), each as the `c` of its center line `x + y =
/// c`. They are one family for the whole widget, `c = n · step` from its
/// origin, so the rects of one hatch line up wherever their edges are
/// (C19's `widgets::draw::hatch` anchors each area at its own bottom,
/// which would offset the stripes of the bands around a layer on top).
pub fn stripe_lines(rect: &graphene::Rect, metrics: HatchMetrics) -> Vec<f32> {
    let step = metrics.step();
    if step <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Vec::new();
    }
    // A line meets the rect from its top-left corner (the least x + y) to
    // its bottom-right one; a stroke reaches a stripe's width further.
    let reach = metrics.stripe;
    let first = ((rect.x() + rect.y() - reach) / step).floor();
    let last = ((rect.x() + rect.width() + rect.y() + rect.height() + reach) / step).ceil();
    let mut lines = Vec::new();
    let mut n = first;
    while n <= last {
        lines.push(n * step);
        n += 1.0;
    }
    lines
}

/// Draws `hatch` on a picture of `w`×`h` px in `color`: each rect clipped,
/// its stripes run past it by a stripe's width (so no stroke's end shows
/// at an edge between two rects).
fn draw_hatch(snapshot: &gtk::Snapshot, hatch: &Hatch, w: f32, h: f32, color: &gdk::RGBA) {
    let (cw, ch) = hatch.canvas;
    if hatch.rects.is_empty() || cw <= 0.0 || ch <= 0.0 {
        return;
    }
    let (sx, sy) = (f64::from(w) / cw, f64::from(h) / ch);
    let metrics = HatchMetrics::at(hatch.scale as f32);
    for r in &hatch.rects {
        let rect = graphene::Rect::new(
            (r.x * sx) as f32,
            (r.y * sy) as f32,
            (r.w * sx) as f32,
            (r.h * sy) as f32,
        );
        let lines = stripe_lines(&rect, metrics);
        if lines.is_empty() {
            continue;
        }
        let reach = metrics.stripe;
        let (low, high) = (rect.y() + rect.height() + reach, rect.y() - reach);
        let builder = gsk::PathBuilder::new();
        for c in lines {
            builder.move_to(c - low, low);
            builder.line_to(c - high, high);
        }
        snapshot.push_clip(&rect);
        snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(metrics.stripe), color);
        snapshot.pop();
    }
}

/// What surrounds the preview.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Frame {
    /// No preview at all (the first run, S01a).
    #[default]
    None,
    /// A 1 px `border` stroke outside (BRIEF §2.4).
    Border,
    /// The trim scrub's 2 px `accent` stroke outside (S10).
    Trim,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct StagePicture {
        pub paintable: RefCell<Option<(gdk::Paintable, Vec<glib::SignalHandlerId>)>>,
        pub background: RefCell<Option<gdk::RGBA>>,
        pub hatch: RefCell<Hatch>,
        pub frame: Cell<Frame>,
        pub palette: RefCell<Option<Rc<Palette>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StagePicture {
        const NAME: &'static str = "QrecStagePicture";
        type Type = super::StagePicture;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("qrec-stage-picture");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for StagePicture {
        fn dispose(&self) {
            crate::guard::run("stage picture: dispose", || self.disconnect());
        }
    }

    impl WidgetImpl for StagePicture {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            crate::guard::run("stage picture: snapshot", || self.draw(snapshot));
        }
    }

    impl StagePicture {
        pub fn disconnect(&self) {
            if let Some((old, handlers)) = self.paintable.borrow_mut().take() {
                for handler in handlers {
                    old.disconnect(handler);
                }
            }
        }

        fn draw(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let bounds = graphene::Rect::new(0.0, 0.0, w, h);
            if let Some(background) = &*self.background.borrow() {
                snapshot.append_color(background, &bounds);
            }
            if let Some((paintable, _)) = &*self.paintable.borrow() {
                let (iw, ih) = (
                    f64::from(paintable.intrinsic_width()),
                    f64::from(paintable.intrinsic_height()),
                );
                // A sink without a frame yet has no size: nothing to draw
                // (its placeholder would cover the canvas background).
                if iw > 0.0 && ih > 0.0 {
                    let scale = (f64::from(w) / iw).min(f64::from(h) / ih);
                    let (dw, dh) = (iw * scale, ih * scale);
                    let x = (f64::from(w) - dw) / 2.0;
                    let y = (f64::from(h) - dh) / 2.0;
                    snapshot.save();
                    snapshot.translate(&graphene::Point::new(x as f32, y as f32));
                    paintable.snapshot(snapshot.upcast_ref::<gdk::Snapshot>(), dw, dh);
                    snapshot.restore();
                }
            }
            let palette = self.palette.borrow();
            let Some(palette) = palette.as_ref() else {
                return;
            };
            draw_hatch(snapshot, &self.hatch.borrow(), w, h, &palette.muted_20);
            let (width, color) = match self.frame.get() {
                Frame::None => return,
                Frame::Border => (1.0, palette.border),
                Frame::Trim => (2.0, palette.accent),
            };
            // Outside the preview, like the frames' stroke.
            let outer = graphene::Rect::new(-width, -width, w + 2.0 * width, h + 2.0 * width);
            snapshot.append_border(
                &gsk::RoundedRect::from_rect(outer, 0.0),
                &[width; 4],
                &[color; 4],
            );
        }
    }
}

glib::wrapper! {
    /// The stage's picture (named `preview`'s ground).
    pub struct StagePicture(ObjectSubclass<imp::StagePicture>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl StagePicture {
    pub fn new(palette: Rc<Palette>) -> Self {
        let picture: Self = glib::Object::new();
        picture.set_palette(palette);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        picture.set_can_target(false);
        picture.update_property(&[gtk::accessible::Property::Label("Preview")]);
        picture
    }

    /// The theme's colors (a theme switch).
    pub fn set_palette(&self, palette: Rc<Palette>) {
        *self.imp().palette.borrow_mut() = Some(palette);
        self.queue_draw();
    }

    /// Shows `paintable` (`None`: only the background).
    pub fn set_paintable(&self, paintable: Option<&gdk::Paintable>) {
        let same = match (&*self.imp().paintable.borrow(), paintable) {
            (Some((current, _)), Some(new)) => current == new,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.imp().disconnect();
        if let Some(paintable) = paintable {
            let weak = self.downgrade();
            let contents = paintable.connect_invalidate_contents(crate::guarded!(
                "stage picture: contents",
                move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.queue_draw();
                    }
                }
            ));
            let weak = self.downgrade();
            let size = paintable.connect_invalidate_size(crate::guarded!(
                "stage picture: size",
                move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.queue_draw();
                    }
                }
            ));
            *self.imp().paintable.borrow_mut() = Some((paintable.clone(), vec![contents, size]));
        }
        self.queue_draw();
    }

    /// The canvas background (`None`: transparent, the first run).
    pub fn set_background(&self, color: Option<gdk::RGBA>) {
        if *self.imp().background.borrow() != color {
            *self.imp().background.borrow_mut() = color;
            self.queue_draw();
        }
    }

    pub fn set_frame(&self, frame: Frame) {
        if self.imp().frame.replace(frame) != frame {
            self.queue_draw();
        }
    }

    /// S24's hatch over the picture (none: no rects).
    pub fn set_hatch(&self, hatch: Hatch) {
        if *self.imp().hatch.borrow() != hatch {
            *self.imp().hatch.borrow_mut() = hatch;
            self.queue_draw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// S24's hatch is one family of 45° lines for the whole picture (`x +
    /// y = n · 8.49` at 100 %): the bands around a layer on top continue
    /// each other's stripes, and every stripe that touches a rect is drawn.
    #[test]
    fn the_hatch_is_one_family_of_stripes() {
        let metrics = HatchMetrics::at(1.0);
        let step = metrics.step();
        let top = graphene::Rect::new(0.0, 0.0, 960.0, 380.0);
        let left = graphene::Rect::new(0.0, 380.0, 750.0, 144.0);
        for rect in [top, left] {
            let lines = stripe_lines(&rect, metrics);
            for c in &lines {
                let n = c / step;
                assert!((n - n.round()).abs() < 1e-3, "{c} is off the family");
            }
            // From the top-left corner's x + y to the bottom-right one's,
            // a stripe's width beyond each.
            let lo = rect.x() + rect.y() - metrics.stripe;
            let hi = rect.x() + rect.width() + rect.y() + rect.height() + metrics.stripe;
            assert!(lines.first().is_some_and(|c| *c <= lo), "{rect:?}");
            assert!(lines.last().is_some_and(|c| *c >= hi), "{rect:?}");
            assert!(lines.windows(2).all(|p| (p[1] - p[0] - step).abs() < 1e-3));
        }
        // At 150 % the stripes are 9 px apart across them.
        let wide = HatchMetrics::at(1.5);
        let lines = stripe_lines(&top, wide);
        assert!((lines[1] - lines[0] - 9.0 * std::f32::consts::SQRT_2).abs() < 1e-3);
        assert!(stripe_lines(&graphene::Rect::new(0.0, 0.0, 0.0, 10.0), metrics).is_empty());
    }
}
