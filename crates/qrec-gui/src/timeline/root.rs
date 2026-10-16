//! The timeline region's root (`region-timeline`): a vertical box whose
//! keyboard focus goes to the timeline canvas. `region.next` and
//! `region.prev` (D-10) focus a region with `child_focus`; GTK's own order
//! would land on the toolbar's first button (the toolbar is its own region,
//! `timeline-toolbar`), so the root hands the focus to the canvas unless it
//! already has it.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    pub struct TimelineRoot {
        /// Where the region's focus goes (the canvas).
        pub target: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TimelineRoot {
        const NAME: &'static str = "QrecTimelineRoot";
        type Type = super::TimelineRoot;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for TimelineRoot {
        fn constructed(&self) {
            self.parent_constructed();
            crate::guard::run("timeline root: constructed", || {
                let obj = self.obj();
                obj.set_orientation(gtk::Orientation::Vertical);
                obj.set_widget_name("region-timeline");
                obj.add_css_class("qrec-timeline");
                obj.update_property(&[gtk::accessible::Property::Label("Timeline")]);
            });
        }
    }

    impl WidgetImpl for TimelineRoot {
        fn focus(&self, direction: gtk::DirectionType) -> bool {
            let fallback = || self.parent_focus(direction);
            crate::guard::catch("timeline root: focus", || {
                let target = self.target.borrow().as_ref().and_then(|w| w.upgrade());
                match target {
                    Some(t) if t.is_sensitive() && t.is_visible() && !t.has_focus() => {
                        t.grab_focus() || fallback()
                    }
                    Some(t) if t.has_focus() => false,
                    _ => fallback(),
                }
            })
            .unwrap_or(false)
        }
    }

    impl BoxImpl for TimelineRoot {}
}

glib::wrapper! {
    /// The timeline region's root.
    pub struct TimelineRoot(ObjectSubclass<imp::TimelineRoot>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

mod column {
    use std::cell::Cell;

    use super::*;

    #[derive(Default)]
    pub struct Column {
        pub width: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Column {
        const NAME: &'static str = "QrecTimelineColumn";
        type Type = super::Column;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Column {
        fn dispose(&self) {
            crate::guard::run("timeline column: dispose", || {
                while let Some(child) = self.obj().first_child() {
                    child.unparent();
                }
            });
        }
    }

    impl WidgetImpl for Column {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            crate::guard::catch("timeline column: measure", || {
                let obj = self.obj();
                let child = obj.first_child();
                match orientation {
                    gtk::Orientation::Horizontal => {
                        let w = self.width.get();
                        (w, w, -1, -1)
                    }
                    // The child's height at the column's width, whatever
                    // the parent offers.
                    _ => child.map_or((0, 0, -1, -1), |c| {
                        let (min, nat, _, _) = c.measure(orientation, self.width.get());
                        (min, nat, -1, -1)
                    }),
                }
            })
            .unwrap_or((0, 0, -1, -1))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            crate::guard::run("timeline column: allocate", || {
                if let Some(child) = self.obj().first_child() {
                    child.allocate(width, height, baseline, None);
                }
            });
        }
    }
}

glib::wrapper! {
    /// A column exactly `width` px wide whatever its child would like (the
    /// 176 px track headers and the ruler's corner cell: their labels
    /// ellipsize).
    pub struct Column(ObjectSubclass<column::Column>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Column {
    pub fn new(child: &impl IsA<gtk::Widget>, width: i32) -> Self {
        let column: Self = glib::Object::new();
        column.imp().width.set(width);
        child.as_ref().set_parent(&column);
        column
    }

    pub fn set_width(&self, width: i32) {
        if self.imp().width.get() != width {
            self.imp().width.set(width);
            self.queue_resize();
        }
    }
}

impl Default for TimelineRoot {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl TimelineRoot {
    pub fn new() -> Self {
        Self::default()
    }

    /// The widget the region's focus goes to.
    pub fn set_focus_target(&self, target: &impl IsA<gtk::Widget>) {
        *self.imp().target.borrow_mut() = Some(target.as_ref().downgrade());
    }
}
