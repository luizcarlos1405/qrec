//! The drawer's slide (DESIGN Motion: the bin drawer opens over 240 ms
//! expo-out and closes over 160 ms ease-in, a 120 ms crossfade under
//! reduced motion; instant in test mode and at startup, "nothing animates
//! on app start"): a container as wide as `progress` × its child, which it
//! lays out at full width and slides in from the window's left edge.

use std::cell::Cell;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};

use crate::theme::motion;

/// Open and close durations, ms (DESIGN Motion).
pub const OPEN_MS: u32 = 240;
pub const CLOSE_MS: u32 = 160;
/// The reduced-motion crossfade.
pub const FADE_MS: u32 = 120;

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    pub struct Slide {
        /// 0 closed … 1 open.
        pub progress: Cell<f64>,
        /// The child's full width.
        pub width: Cell<i32>,
        pub animation: RefCell<Option<adw::TimedAnimation>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Slide {
        const NAME: &'static str = "QrecBinSlide";
        type Type = super::Slide;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Slide {
        fn dispose(&self) {
            crate::guard::run("bin slide: dispose", || {
                if let Some(animation) = self.animation.take() {
                    animation.skip();
                }
                while let Some(child) = self.obj().first_child() {
                    child.unparent();
                }
            });
        }
    }

    impl WidgetImpl for Slide {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            crate::guard::run("bin slide: measure", || {
                let Some(child) = self.obj().first_child() else {
                    return (0, 0, -1, -1);
                };
                match orientation {
                    gtk::Orientation::Horizontal => {
                        let w = (f64::from(self.width.get()) * self.progress.get()).round() as i32;
                        (w, w, -1, -1)
                    }
                    _ => {
                        let (min, nat, _, _) = child.measure(orientation, for_size);
                        (min, nat, -1, -1)
                    }
                }
            })
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            crate::guard::run("bin slide: allocate", || {
                if let Some(child) = self.obj().first_child() {
                    let full = self.width.get();
                    let offset = (width - full) as f32;
                    let transform =
                        gsk::Transform::new().translate(&graphene::Point::new(offset, 0.0));
                    child.allocate(full, height, baseline, Some(transform));
                }
            });
        }
    }
}

glib::wrapper! {
    /// The drawer's sliding container.
    pub struct Slide(ObjectSubclass<imp::Slide>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Slide {
    /// A slide holding `child`, `width` px wide when open; closed.
    pub fn new(child: &impl IsA<gtk::Widget>, width: i32) -> Self {
        let slide: Self = glib::Object::new();
        slide.set_overflow(gtk::Overflow::Hidden);
        child.set_parent(&slide);
        slide.imp().width.set(width);
        slide.set_visible(false);
        slide
    }

    /// The open width (the UI scale changed).
    pub fn set_width(&self, width: i32) {
        self.imp().width.set(width);
        self.queue_resize();
    }

    fn set_progress(&self, p: f64) {
        self.imp().progress.set(p.clamp(0.0, 1.0));
        self.queue_resize();
    }

    /// Whether it is open, or opening.
    pub fn is_open(&self) -> bool {
        self.is_visible() && self.imp().progress.get() > 0.0
    }

    /// Opens (`open`) or closes it; `animate` false (startup, test mode)
    /// jumps to the end. `reduced`: a crossfade instead of the slide.
    pub fn show(&self, open: bool, animate: bool, reduced: bool) {
        if let Some(animation) = self.imp().animation.take() {
            animation.skip();
        }
        let animate = animate && !motion::one_shots_at_end();
        if open {
            self.set_visible(true);
        }
        if !animate {
            self.set_opacity(1.0);
            self.set_progress(if open { 1.0 } else { 0.0 });
            self.set_visible(open);
            return;
        }
        let weak = self.downgrade();
        let (from, to, ms, easing) = if reduced {
            self.set_progress(1.0);
            let from = if open { 0.0 } else { self.opacity() };
            (
                from,
                if open { 1.0 } else { 0.0 },
                FADE_MS,
                adw::Easing::Linear,
            )
        } else if open {
            self.set_opacity(1.0);
            (
                self.imp().progress.get(),
                1.0,
                OPEN_MS,
                adw::Easing::EaseOutExpo,
            )
        } else {
            self.set_opacity(1.0);
            (
                self.imp().progress.get(),
                0.0,
                CLOSE_MS,
                adw::Easing::EaseInCubic,
            )
        };
        let target = adw::CallbackAnimationTarget::new(move |v| {
            crate::guard::run("bin slide: a frame", || {
                if let Some(slide) = weak.upgrade() {
                    if reduced {
                        slide.set_opacity(v);
                    } else {
                        slide.set_progress(v);
                    }
                }
            });
        });
        let animation = adw::TimedAnimation::new(self, from, to, ms, target);
        animation.set_easing(easing);
        let weak = self.downgrade();
        animation.connect_done(crate::guarded!("bin slide: done", move |_| {
            if let Some(slide) = weak.upgrade()
                && !open
            {
                slide.set_visible(false);
                slide.set_progress(0.0);
                slide.set_opacity(1.0);
            }
        }));
        *self.imp().animation.borrow_mut() = Some(animation.clone());
        animation.play();
    }
}
