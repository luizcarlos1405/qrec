//! A bin row's 96×54 thumbnail (S14): the take's strip image at the middle
//! of the take, center-cropped, under a 3 px stripe in the take's color;
//! a broken take's is a `muted` hatch (its error icon is the row's). The
//! image comes from the preview cache (WP2's `ThumbCache`, 72 px high × the
//! device scale, shared with the timeline's filmstrip).

use std::cell::{Cell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use qrec_core::view::bin::Thumbnail;

use crate::media::MediaHub;
use crate::theme::Palette;
use crate::widgets::draw::{self, Edges};

/// The thumbnail's size and its stripe, logical px.
pub const WIDTH: f64 = 96.0;
pub const HEIGHT: f64 = 54.0;
const STRIPE: f64 = 3.0;
const RADIUS: f32 = 2.0;
/// The strip images' height (the timeline's, so both share the cache).
const STRIP_PX: u32 = 72;

mod imp {
    use super::*;

    pub struct Thumb {
        pub image: RefCell<Thumbnail>,
        pub color: Cell<usize>,
        pub palette: RefCell<Palette>,
        pub scale: Cell<f64>,
        pub media: RefCell<Option<MediaHub>>,
    }

    impl Default for Thumb {
        fn default() -> Self {
            Self {
                image: RefCell::new(Thumbnail::Blank),
                color: Cell::new(0),
                palette: RefCell::new(Palette::default()),
                scale: Cell::new(1.0),
                media: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Thumb {
        const NAME: &'static str = "QrecBinThumb";
        type Type = super::Thumb;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("bin-thumb");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for Thumb {}

    impl WidgetImpl for Thumb {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            crate::guard::run("bin thumb: measure", || {
                let s = self.scale.get();
                let size = match orientation {
                    gtk::Orientation::Horizontal => WIDTH * s,
                    _ => HEIGHT * s,
                }
                .round() as i32;
                (size, size, -1, -1)
            })
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            crate::guard::run("bin thumb: snapshot", || self.draw(snapshot));
        }
    }

    impl Thumb {
        fn texture(&self) -> Option<gdk::Texture> {
            let Thumbnail::Image { file, second } = &*self.image.borrow() else {
                return None;
            };
            let media = self.media.borrow();
            let thumbs = media.as_ref()?.thumbs()?;
            let height = STRIP_PX * self.obj().scale_factor().max(1) as u32;
            // A take under a second or two may have no keyframe image in
            // its first strip (S14's take 6, 7 frames): its first frame.
            thumbs.strip_image(file, *second, height).or_else(|| {
                (*second == 0)
                    .then(|| thumbs.frame(file, 0, height))
                    .flatten()
            })
        }

        fn draw(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let s = self.scale.get() as f32;
            let palette = self.palette.borrow();
            let bounds = graphene::Rect::new(0.0, 0.0, w, h);
            snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, RADIUS * s));
            snapshot.append_color(&palette.raised, &bounds);
            match &*self.image.borrow() {
                Thumbnail::Broken => {
                    draw::hatch(
                        snapshot,
                        &bounds,
                        &palette.muted_40,
                        Edges::default(),
                        &palette.muted_40,
                        s,
                    );
                }
                Thumbnail::Image { .. } => {
                    if let Some(texture) = self.texture() {
                        // Center-cropped to the box (cover).
                        let (tw, th) = (texture.width() as f32, texture.height() as f32);
                        let k = (w / tw.max(1.0)).max(h / th.max(1.0));
                        let (dw, dh) = (tw * k, th * k);
                        let rect = graphene::Rect::new((w - dw) / 2.0, (h - dh) / 2.0, dw, dh);
                        snapshot.append_texture(&texture, &rect);
                    }
                }
                Thumbnail::Blank => {}
            }
            let color = palette.take[self.color.get() % palette.take.len()];
            snapshot.append_color(
                &color,
                &graphene::Rect::new(0.0, 0.0, w, (STRIPE as f32) * s),
            );
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    /// A bin row's thumbnail.
    pub struct Thumb(ObjectSubclass<imp::Thumb>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Thumb {
    pub fn new(media: &MediaHub, theme: &crate::theme::Theme) -> Self {
        let thumb: Self = glib::Object::new();
        *thumb.imp().media.borrow_mut() = Some(media.clone());
        thumb.follow(theme);
        let weak = thumb.downgrade();
        theme
            .subscribe(move |theme| {
                if let Some(thumb) = weak.upgrade() {
                    thumb.follow(theme);
                }
            })
            .detach();
        thumb.set_valign(gtk::Align::Center);
        thumb
    }

    fn follow(&self, theme: &crate::theme::Theme) {
        *self.imp().palette.borrow_mut() = (*theme.palette()).clone();
        self.imp().scale.set(theme.scale());
        self.queue_resize();
    }

    /// Shows `image` with the stripe of take color `color`.
    pub fn set(&self, image: &Thumbnail, color: usize) {
        if *self.imp().image.borrow() == *image && self.imp().color.get() == color {
            return;
        }
        *self.imp().image.borrow_mut() = image.clone();
        self.imp().color.set(color);
        self.queue_draw();
    }

    /// The strip file it shows, if any (a redraw when its images arrive).
    pub fn file(&self) -> Option<String> {
        match &*self.imp().image.borrow() {
            Thumbnail::Image { file, .. } => Some(file.clone()),
            _ => None,
        }
    }
}
