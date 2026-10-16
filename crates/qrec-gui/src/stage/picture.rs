//! The preview picture (S02, S06, D-27): the project's canvas background,
//! the live or the program paintable fitted into it (a sink's picture is at
//! most the canvas size: it is letterboxed, never cropped), and the
//! preview's frame: a 1 px `border` stroke outside it, or S10's 2 px
//! `accent` stroke during a trim scrub.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::theme::Palette;

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
}
