//! C01 spinner (G§1.3, G§5.1): one 270° arc, one turn per second, 16, 14
//! or 12 px; it keeps spinning under reduced motion. Driven by a tick
//! callback reading `motion::now()`, so test mode freezes it at 0°. The
//! arc takes the widget's CSS color. Its size is CSS (`qrec-spinner`, and
//! `.s14`, `.s12`: `common.css`), so `ui.scale` scales it like every icon
//! (D-46); the arc follows the allocation.

use std::f32::consts::PI;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};

use crate::theme::motion;

mod imp {
    use std::cell::Cell;

    use super::*;

    pub struct Spinner {
        pub size: Cell<i32>,
        pub tick: Cell<Option<gtk::TickCallbackId>>,
    }

    impl Default for Spinner {
        fn default() -> Self {
            Self {
                size: Cell::new(16),
                tick: Cell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Spinner {
        const NAME: &'static str = "QrecSpinner";
        type Type = super::Spinner;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("qrec-spinner");
            klass.set_accessible_role(gtk::AccessibleRole::ProgressBar);
        }
    }

    impl ObjectImpl for Spinner {
        fn dispose(&self) {
            crate::guard::run("spinner: dispose", || {
                if let Some(tick) = self.tick.take() {
                    tick.remove();
                }
            });
        }
    }

    impl WidgetImpl for Spinner {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            crate::guard::run("spinner: measure", || {
                // A C01 size comes from the CSS (scaled); another one is
                // the widget's own.
                let size = self.size.get();
                if super::css_class(size).is_some() {
                    (0, 0, -1, -1)
                } else {
                    (size, size, -1, -1)
                }
            })
        }

        fn map(&self) {
            self.parent_map();
            crate::guard::run("spinner: map", || {
                let tick = crate::guard::tick(&*self.obj(), "spinner: tick", |widget, _clock| {
                    widget.queue_draw();
                    glib::ControlFlow::Continue
                });
                if let Some(old) = self.tick.replace(Some(tick)) {
                    old.remove();
                }
            });
        }

        fn unmap(&self) {
            crate::guard::run("spinner: unmap", || {
                if let Some(tick) = self.tick.take() {
                    tick.remove();
                }
            });
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            crate::guard::run("spinner: snapshot", || self.draw(snapshot));
        }
    }

    impl Spinner {
        fn draw(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let w = obj.width() as f32;
            let h = obj.height() as f32;
            let size = w.min(h);
            let stroke_width = (size / 8.0).max(1.5);
            let radius = (size - stroke_width) / 2.0;
            let turn = (motion::now().as_millis() % 1000) as f32 / 1000.0;
            let (cx, cy) = (w / 2.0, h / 2.0);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(cx, cy));
            snapshot.rotate(turn * 360.0);
            let builder = gsk::PathBuilder::new();
            let point = |a: f32| (radius * a.cos(), radius * a.sin());
            let (x0, y0) = point(-PI / 2.0);
            let (x1, y1) = point(PI);
            builder.move_to(x0, y0);
            // 270° clockwise: a large arc.
            builder.svg_arc_to(radius, radius, 0.0, true, true, x1, y1);
            let path = builder.to_path();
            let stroke = gsk::Stroke::new(stroke_width);
            stroke.set_line_cap(gsk::LineCap::Round);
            snapshot.append_stroke(&path, &stroke, &obj.color());
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    /// A busy spinner (C01).
    pub struct Spinner(ObjectSubclass<imp::Spinner>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new(16)
    }
}

impl Spinner {
    /// A spinner `size` px across (16, 14 or 12: sized by the CSS, × the UI
    /// scale; another size is taken as it is).
    pub fn new(size: i32) -> Self {
        let spinner: Self = glib::Object::new();
        spinner.set_size(size);
        spinner.update_property(&[gtk::accessible::Property::Label("Busy")]);
        spinner
    }

    pub fn set_size(&self, size: i32) {
        let size = size.max(1);
        self.imp().size.set(size);
        for class in SIZE_CLASSES {
            self.remove_css_class(class);
        }
        if let Some(Some(class)) = css_class(size) {
            self.add_css_class(class);
        }
        self.queue_resize();
    }
}

/// The CSS classes of the C01 sizes but the default.
const SIZE_CLASSES: [&str; 2] = ["s14", "s12"];

/// The CSS class of a C01 size (`Some(None)`: 16, the plain node), or
/// `None` for another size.
pub(crate) fn css_class(size: i32) -> Option<Option<&'static str>> {
    match size {
        16 => Some(None),
        14 => Some(Some("s14")),
        12 => Some(Some("s12")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::css::scale_px;

    /// The spinner's sizes are CSS, so `ui.scale` scales them like the
    /// icons it stands in for (a 16 px spinner beside 24 px icons at 1.5
    /// was its own fixed measure).
    #[test]
    fn spinner_sizes_are_css_and_scale() {
        let css = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("resources/theme/components/common.css"),
        )
        .unwrap();
        for (size, selector) in [
            (16, "\nqrec-spinner {"),
            (14, "\nqrec-spinner.s14 {"),
            (12, "\nqrec-spinner.s12 {"),
        ] {
            let class = css_class(size).expect("a C01 size");
            assert_eq!(selector.contains(".s"), class.is_some(), "{size}");
            let rule = css
                .split(selector)
                .nth(1)
                .and_then(|r| r.split('}').next())
                .unwrap_or_else(|| panic!("no {selector} rule"));
            let at_1_5 = scale_px(rule, 1.5);
            let scaled = format!("{}px", f64::from(size) * 1.5);
            assert!(
                at_1_5.contains(&format!("min-width: {scaled}"))
                    && at_1_5.contains(&format!("min-height: {scaled}")),
                "{selector} at 1.5: {at_1_5}"
            );
        }
        assert_eq!(css_class(20), None);
    }
}
