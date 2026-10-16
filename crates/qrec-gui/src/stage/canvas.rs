//! The canvas overlay (C08): over the preview, the selected layer's box (a
//! 1.5 px `accent` stroke with eight 8×8 `accent` handles edged in `bg`;
//! with `Alt` the edge handles become crop brackets) and the snap guides
//! (1 px dashed 4/4 `snap-guide` lines across the preview). It takes the
//! stage's pointer input and the keyboard focus (the `Canvas` key context);
//! what the input does is the stage's (`stage/mod.rs`, `stage/edit.rs`).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};
use qrec_core::view::Rect;
use qrec_core::view::canvas::{Axis, HANDLE, Handle};

use crate::theme::Palette;

/// What the overlay draws, in canvas px (it scales them to its size when
/// it draws, so an allocation never lags behind).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CanvasDraw {
    /// The canvas's size.
    pub canvas: (f64, f64),
    /// The selected layer's box.
    pub selection: Option<Rect>,
    /// Edits are off (D-43): the box at 0.45.
    pub dim: bool,
    /// `Alt` is held: crop brackets on the edges.
    pub brackets: bool,
    /// Snap guides: the axis and the position across it.
    pub guides: Vec<(Axis, f64)>,
    /// The UI scale (handle sizes).
    pub scale: f64,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct StageCanvas {
        pub draw: RefCell<CanvasDraw>,
        pub palette: RefCell<Option<Rc<Palette>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StageCanvas {
        const NAME: &'static str = "QrecStageCanvas";
        type Type = super::StageCanvas;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("qrec-stage-canvas");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }
    }

    impl ObjectImpl for StageCanvas {}

    impl WidgetImpl for StageCanvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            crate::guard::run("stage canvas: snapshot", || self.paint(snapshot));
        }
    }

    impl StageCanvas {
        fn paint(&self, snapshot: &gtk::Snapshot) {
            let palette = self.palette.borrow();
            let Some(palette) = palette.as_ref() else {
                return;
            };
            let draw = self.draw.borrow();
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            let (cw, ch) = draw.canvas;
            if cw <= 0.0 || ch <= 0.0 {
                return;
            }
            let (sx, sy) = (f64::from(w) / cw, f64::from(h) / ch);
            for (axis, at) in &draw.guides {
                let builder = gsk::PathBuilder::new();
                let along = match axis {
                    Axis::X => *at * sx,
                    Axis::Y => *at * sy,
                };
                let at = (along as f32).round() + 0.5;
                match axis {
                    Axis::X => {
                        builder.move_to(at, 0.0);
                        builder.line_to(at, h);
                    }
                    Axis::Y => {
                        builder.move_to(0.0, at);
                        builder.line_to(w, at);
                    }
                }
                let stroke = gsk::Stroke::new(1.0);
                stroke.set_dash(&[4.0, 4.0]);
                snapshot.append_stroke(&builder.to_path(), &stroke, &palette.snap_guide);
            }
            let Some(r) = draw.selection else {
                return;
            };
            let r = Rect::new(r.x * sx, r.y * sy, r.w * sx, r.h * sy);
            if draw.dim {
                snapshot.push_opacity(0.45);
            }
            let s = draw.scale.max(0.5) as f32;
            let stroke_w = 1.5 * s;
            let builder = gsk::PathBuilder::new();
            builder.add_rect(&graphene::Rect::new(
                r.x as f32 + stroke_w / 2.0,
                r.y as f32 + stroke_w / 2.0,
                (r.w as f32 - stroke_w).max(0.0),
                (r.h as f32 - stroke_w).max(0.0),
            ));
            snapshot.append_stroke(
                &builder.to_path(),
                &gsk::Stroke::new(stroke_w),
                &palette.accent,
            );
            let size = HANDLE * f64::from(s);
            for handle in Handle::ALL {
                let sq = handle.square(r, size);
                let rect = if draw.brackets && handle.edge().is_some() {
                    // A crop bracket: a bar along the edge, 2.5× long.
                    let (cx, cy) = handle.center(r);
                    let (long, thick) = (size * 2.5, size * 0.5);
                    if matches!(handle, Handle::Top | Handle::Bottom) {
                        graphene::Rect::new(
                            (cx - long / 2.0) as f32,
                            (cy - thick / 2.0) as f32,
                            long as f32,
                            thick as f32,
                        )
                    } else {
                        graphene::Rect::new(
                            (cx - thick / 2.0) as f32,
                            (cy - long / 2.0) as f32,
                            thick as f32,
                            long as f32,
                        )
                    }
                } else {
                    graphene::Rect::new(sq.x as f32, sq.y as f32, sq.w as f32, sq.h as f32)
                };
                snapshot.append_color(&palette.accent, &rect);
                snapshot.append_border(
                    &gsk::RoundedRect::from_rect(rect, 0.0),
                    &[s; 4],
                    &[palette.bg; 4],
                );
            }
            if draw.dim {
                snapshot.pop();
            }
        }
    }
}

glib::wrapper! {
    /// The layer editing overlay of the preview.
    pub struct StageCanvas(ObjectSubclass<imp::StageCanvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl StageCanvas {
    pub fn new(palette: Rc<Palette>) -> Self {
        let canvas: Self = glib::Object::new();
        canvas.set_palette(palette);
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        canvas.set_focusable(true);
        canvas.update_property(&[gtk::accessible::Property::Label("Stage canvas")]);
        canvas
    }

    pub fn set_palette(&self, palette: Rc<Palette>) {
        *self.imp().palette.borrow_mut() = Some(palette);
        self.queue_draw();
    }

    pub fn set_draw(&self, draw: CanvasDraw) {
        if *self.imp().draw.borrow() != draw {
            *self.imp().draw.borrow_mut() = draw;
            self.queue_draw();
        }
    }
}
