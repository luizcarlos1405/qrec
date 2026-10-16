//! C26 value tooltip (G§5.1): the value shown above the thumb while a
//! `gtk::Scale` is dragged, and the scale's release. Owner: WP5.
//!
//! [`attach`] gives a scale:
//! - a small popover (`raised`, `md` radius, e1, `mono-sm`; no arrow, never
//!   focused or targeted) over the thumb while the pointer drags it, its
//!   text from `format` ("−35 dB", "100 %");
//! - `on_release(value)` when a change ends: the pointer lets go, or a key
//!   that moved the slider is released. Controls that commit on release
//!   (the inspector's opacity, G§4.7) send their one command there; the
//!   autocut page's sliders tune a draft as they move.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::guarded;

/// Called with the value a change ended at.
pub type ReleaseFn = Rc<dyn Fn(f64)>;

/// The tooltip of one scale. Keep it as long as the scale (or drop it:
/// the scale keeps what it needs).
#[derive(Clone)]
pub struct ValueTooltip {
    inner: Rc<Inner>,
}

struct Inner {
    scale: gtk::Scale,
    popover: gtk::Popover,
    label: gtk::Label,
    format: Box<dyn Fn(f64) -> String>,
    dragging: Cell<bool>,
    /// The value when the change began.
    start: Cell<Option<f64>>,
    on_release: RefCell<Option<ReleaseFn>>,
}

impl std::fmt::Debug for ValueTooltip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValueTooltip")
            .field("dragging", &self.inner.dragging.get())
            .finish()
    }
}

/// Shows `format(value)` over `scale`'s thumb while it is dragged and
/// calls `on_release` when a change ends.
pub fn attach(
    scale: &gtk::Scale,
    format: impl Fn(f64) -> String + 'static,
    on_release: impl Fn(f64) + 'static,
) -> ValueTooltip {
    let label = gtk::Label::new(None);
    label.add_css_class("t-mono-sm");
    let popover = gtk::Popover::new();
    popover.add_css_class("qrec-value-tooltip");
    popover.set_child(Some(&label));
    popover.set_has_arrow(false);
    popover.set_autohide(false);
    popover.set_position(gtk::PositionType::Top);
    popover.set_can_target(false);
    popover.set_can_focus(false);
    popover.set_parent(scale);
    let tooltip = ValueTooltip {
        inner: Rc::new(Inner {
            scale: scale.clone(),
            popover,
            label,
            format: Box::new(format),
            dragging: Cell::new(false),
            start: Cell::new(None),
            on_release: RefCell::new(Some(Rc::new(on_release))),
        }),
    };
    tooltip.wire();
    tooltip
}

impl ValueTooltip {
    /// Whether the pointer drags the thumb now.
    pub fn dragging(&self) -> bool {
        self.inner.dragging.get()
    }

    fn wire(&self) {
        let inner = &self.inner;
        let pointer = gtk::EventControllerLegacy::new();
        pointer.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(inner);
        pointer.connect_event(guarded!(
            "slider: pointer",
            after_panic = glib::Propagation::Proceed,
            move |_, event| {
                if let Some(inner) = weak.upgrade() {
                    let tooltip = ValueTooltip { inner };
                    match event.event_type() {
                        gdk::EventType::ButtonPress | gdk::EventType::TouchBegin => {
                            tooltip.begin();
                        }
                        gdk::EventType::ButtonRelease
                        | gdk::EventType::TouchEnd
                        | gdk::EventType::TouchCancel => tooltip.end(),
                        _ => {}
                    }
                }
                glib::Propagation::Proceed
            }
        ));
        inner.scale.add_controller(pointer);
        // Keys that move the slider end their change on release.
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(inner);
        keys.connect_key_pressed(guarded!(
            "slider: key down",
            move |_, _key, _code, _modifiers| {
                if let Some(inner) = weak.upgrade()
                    && inner.start.get().is_none()
                {
                    inner.start.set(Some(inner.scale.value()));
                }
                glib::Propagation::Proceed
            }
        ));
        let weak = Rc::downgrade(inner);
        keys.connect_key_released(guarded!(
            "slider: key up",
            move |_, _key, _code, _modifiers| {
                if let Some(inner) = weak.upgrade()
                    && !inner.dragging.get()
                {
                    ValueTooltip { inner }.release();
                }
            }
        ));
        inner.scale.add_controller(keys);
        let weak = Rc::downgrade(inner);
        inner
            .scale
            .connect_value_changed(guarded!("slider: value", move |_| {
                if let Some(inner) = weak.upgrade()
                    && inner.dragging.get()
                {
                    ValueTooltip { inner }.show();
                }
            }));
        let popover = inner.popover.clone();
        inner
            .scale
            .connect_destroy(guarded!("slider: destroy", move |_| {
                popover.unparent();
            }));
    }

    fn begin(&self) {
        let inner = &self.inner;
        inner.dragging.set(true);
        inner.start.set(Some(inner.scale.value()));
        // The value under the pointer comes with the next change.
        self.show();
    }

    fn end(&self) {
        let inner = &self.inner;
        if !inner.dragging.replace(false) {
            return;
        }
        inner.popover.popdown();
        self.release();
    }

    fn release(&self) {
        let inner = &self.inner;
        let Some(start) = inner.start.take() else {
            return;
        };
        let value = inner.scale.value();
        if (value - start).abs() < f64::EPSILON {
            return;
        }
        let callback = inner.on_release.borrow().clone();
        if let Some(f) = callback {
            f(value);
        }
    }

    fn show(&self) {
        let inner = &self.inner;
        let value = inner.scale.value();
        inner.label.set_text(&(inner.format)(value));
        let (start, end) = inner.scale.slider_range();
        let rect = inner.scale.range_rect();
        let x = rect.x() + (start + end) / 2;
        inner
            .popover
            .set_pointing_to(Some(&gdk::Rectangle::new(x, rect.y(), 1, 1)));
        if inner.popover.is_visible() {
            inner.popover.present();
        } else {
            inner.popover.popup();
        }
    }
}
