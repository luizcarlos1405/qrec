//! C02 key cap (DESIGN Components): a key's legend in a small cap. CSS
//! name `keycap`; never focusable or targetable. Sizes sm, md, lg; variants
//! on-fill (inside a filled button), disabled, rebound, capture (waiting
//! for a key), conflict. CSS `min-width` applies to the content box:
//! subtract padding and border.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// The cap's size class (`.sm`, `.md`, `.lg`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CapSize {
    Sm,
    #[default]
    Md,
    Lg,
}

/// The cap's look (`.on-fill`, `.disabled`, `.rebound`, `.capture`,
/// `.conflict`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CapVariant {
    #[default]
    Plain,
    OnFill,
    Disabled,
    Rebound,
    Capture,
    Conflict,
}

impl CapSize {
    fn class(self) -> &'static str {
        match self {
            CapSize::Sm => "sm",
            CapSize::Md => "md",
            CapSize::Lg => "lg",
        }
    }
}

impl CapVariant {
    fn class(self) -> Option<&'static str> {
        match self {
            CapVariant::Plain => None,
            CapVariant::OnFill => Some("on-fill"),
            CapVariant::Disabled => Some("disabled"),
            CapVariant::Rebound => Some("rebound"),
            CapVariant::Capture => Some("capture"),
            CapVariant::Conflict => Some("conflict"),
        }
    }

    const ALL: [CapVariant; 6] = [
        CapVariant::Plain,
        CapVariant::OnFill,
        CapVariant::Disabled,
        CapVariant::Rebound,
        CapVariant::Capture,
        CapVariant::Conflict,
    ];
}

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    pub struct KeyCap {
        pub label: RefCell<Option<gtk::Label>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for KeyCap {
        const NAME: &'static str = "QrecKeyCap";
        type Type = super::KeyCap;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("keycap");
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for KeyCap {
        fn constructed(&self) {
            self.parent_constructed();
            crate::guard::run("key cap: constructed", || {
                let obj = self.obj();
                obj.set_focusable(false);
                obj.set_can_focus(false);
                obj.set_can_target(false);
                obj.set_accessible_role(gtk::AccessibleRole::Presentation);
                let label = gtk::Label::new(None);
                label.set_parent(&*obj);
                *self.label.borrow_mut() = Some(label);
            });
        }

        fn dispose(&self) {
            crate::guard::run("key cap: dispose", || {
                let label = self.label.borrow_mut().take();
                if let Some(label) = label {
                    label.unparent();
                }
            });
        }
    }

    impl WidgetImpl for KeyCap {}
}

glib::wrapper! {
    /// A key cap (C02).
    pub struct KeyCap(ObjectSubclass<imp::KeyCap>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl KeyCap {
    /// A cap showing `legend` ("Ctrl", "R", "Esc", "←").
    pub fn new(legend: &str) -> Self {
        let cap: Self = glib::Object::new();
        cap.set_legend(legend);
        cap.set_size(CapSize::Md);
        cap
    }

    pub fn legend(&self) -> String {
        self.imp()
            .label
            .borrow()
            .as_ref()
            .map(|l| l.text().to_string())
            .unwrap_or_default()
    }

    pub fn set_legend(&self, legend: &str) {
        if let Some(label) = &*self.imp().label.borrow() {
            label.set_text(legend);
        }
    }

    pub fn set_size(&self, size: CapSize) {
        for s in [CapSize::Sm, CapSize::Md, CapSize::Lg] {
            self.remove_css_class(s.class());
        }
        self.add_css_class(size.class());
    }

    pub fn set_variant(&self, variant: CapVariant) {
        for v in CapVariant::ALL {
            if let Some(class) = v.class() {
                self.remove_css_class(class);
            }
        }
        if let Some(class) = variant.class() {
            self.add_css_class(class);
        }
    }
}

/// One cap per legend, in a row (a chord: `Ctrl` `K`), 2 px apart:
/// `.keycaps`' CSS `border-spacing` (GtkBox adds it to its own spacing,
/// which stays 0, and only CSS px follow the UI scale).
pub fn chord(legends: &[String], size: CapSize, variant: CapVariant) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("keycaps");
    for legend in legends {
        let cap = KeyCap::new(legend);
        cap.set_size(size);
        cap.set_variant(variant);
        row.append(&cap);
    }
    row
}
