//! A widget the timeline draws itself (C15 ruler, C16/C17 canvas, C18
//! overlay): its `snapshot` and its allocation call back into the
//! timeline, through the guard (G§3.12 R10). GSK nodes, not Cairo: clip
//! filmstrips are textures (`append_texture`), and the canvas caches its
//! render node per data and viewport change.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// Draws into the snapshot at the widget's size (logical px).
pub type DrawFn = Rc<dyn Fn(&gtk::Snapshot, f64, f64)>;
/// Told the new allocation (logical px).
pub type ResizeFn = Rc<dyn Fn(i32, i32)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Surface {
        pub draw: RefCell<Option<DrawFn>>,
        pub resized: RefCell<Option<ResizeFn>>,
        pub natural: Cell<(i32, i32)>,
        pub size: Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Surface {
        const NAME: &'static str = "QrecTimelineSurface";
        type Type = super::Surface;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("timeline-surface");
        }
    }

    impl ObjectImpl for Surface {}

    impl WidgetImpl for Surface {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            crate::guard::catch("timeline surface: measure", || {
                let (w, h) = self.natural.get();
                match orientation {
                    gtk::Orientation::Horizontal => (0, w.max(0), -1, -1),
                    _ => (h.max(0), h.max(0), -1, -1),
                }
            })
            .unwrap_or((0, 0, -1, -1))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            crate::guard::run("timeline surface: allocate", || {
                if self.size.get() != (width, height) {
                    self.size.set((width, height));
                    let resized = self.resized.borrow().clone();
                    if let Some(f) = resized {
                        f(width, height);
                    }
                }
            });
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            crate::guard::run("timeline surface: snapshot", || {
                let draw = self.draw.borrow().clone();
                if let Some(draw) = draw {
                    let obj = self.obj();
                    draw(snapshot, f64::from(obj.width()), f64::from(obj.height()));
                }
            });
        }
    }
}

glib::wrapper! {
    /// A self-drawn part of the timeline.
    pub struct Surface(ObjectSubclass<imp::Surface>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Surface {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Surface {
    pub fn new() -> Self {
        Self::default()
    }

    /// What draws it.
    pub fn set_draw(&self, f: impl Fn(&gtk::Snapshot, f64, f64) + 'static) {
        *self.imp().draw.borrow_mut() = Some(Rc::new(f));
        self.queue_draw();
    }

    /// Who hears of a new allocation.
    pub fn set_on_resize(&self, f: impl Fn(i32, i32) + 'static) {
        *self.imp().resized.borrow_mut() = Some(Rc::new(f));
    }

    /// Its natural size (`measure`): `width` (its minimum is 0) and `height`
    /// (minimum and natural).
    pub fn set_natural_size(&self, width: i32, height: i32) {
        if self.imp().natural.get() != (width, height) {
            self.imp().natural.set((width, height));
            self.queue_resize();
        }
    }

    /// Its last allocation.
    pub fn size(&self) -> (i32, i32) {
        self.imp().size.get()
    }
}
