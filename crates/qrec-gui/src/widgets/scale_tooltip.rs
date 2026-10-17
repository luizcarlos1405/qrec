//! C26 value tooltip (G§5.1): the value shown above the thumb while a
//! `gtk::Scale` is dragged, and the scale's release. Owner: WP5.
//!
//! [`attach`] gives a scale:
//! - a small popover (`raised`, `md` radius, e1, `mono-sm`; no arrow, never
//!   focused or targeted) over the thumb while the pointer drags it, its
//!   text from `format` ("−35 dB", "100 %");
//! - `on_release(value)` when a change ends: the pointer lets go, or a key
//!   that moved the slider is released. Controls that commit on release
//!   (the inspector's opacity, G§4.7) send their one command there and say
//!   whether they did; the autocut page's sliders tune a draft as they
//!   move. [`ValueTooltip::connect_end`] follows every change's end, sent or
//!   not, and [`ValueTooltip::changing`] says a change is under way: a
//!   snapshot then leaves the slider where the person has it (R44).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::guarded;

/// Called with the value a change ended at; whether it sent a command.
pub type ReleaseFn = Rc<dyn Fn(f64) -> bool>;

/// Called when a change ended: whether its release sent a command.
pub type EndFn = Rc<dyn Fn(bool)>;

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
    /// The change under way was cancelled: its end sends nothing.
    cancelled: Cell<bool>,
    on_release: RefCell<Option<ReleaseFn>>,
    on_end: RefCell<Option<EndFn>>,
}

impl std::fmt::Debug for ValueTooltip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValueTooltip")
            .field("dragging", &self.inner.dragging.get())
            .finish()
    }
}

/// Shows `format(value)` over `scale`'s thumb while it is dragged and
/// calls `on_release` when a change ends (it says whether it sent).
pub fn attach(
    scale: &gtk::Scale,
    format: impl Fn(f64) -> String + 'static,
    on_release: impl Fn(f64) -> bool + 'static,
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
            cancelled: Cell::new(false),
            on_release: RefCell::new(Some(Rc::new(on_release))),
            on_end: RefCell::new(None),
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

    /// A change is under way: the pointer drags the thumb, or a key moved
    /// the slider and isn't released yet. The slider and what follows it
    /// keep the person's value meanwhile.
    pub fn changing(&self) -> bool {
        let inner = &self.inner;
        inner.dragging.get()
            || inner
                .start
                .get()
                .is_some_and(|start| (inner.scale.value() - start).abs() >= f64::EPSILON)
    }

    /// The change under way ends without its release (what the slider
    /// edits went away): nothing is sent for it.
    pub fn cancel(&self) {
        if self.inner.start.take().is_some() {
            self.inner.cancelled.set(true);
        }
    }

    /// Calls `f` when a change ends, with whether its release sent a
    /// command (unsent, the owner shows its model's value again).
    pub fn connect_end(&self, f: impl Fn(bool) + 'static) {
        *self.inner.on_end.borrow_mut() = Some(Rc::new(f));
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
        // Keys that move the slider end their change on release (only
        // those: Ctrl held for an undo starts no change).
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(inner);
        keys.connect_key_pressed(guarded!(
            "slider: key down",
            move |_, key, _code, _modifiers| {
                if let Some(inner) = weak.upgrade()
                    && moves_a_range(key)
                    && inner.start.get().is_none()
                {
                    inner.cancelled.set(false);
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
        inner.cancelled.set(false);
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
            // A cancelled change ends unsent: the owner shows its model.
            if inner.cancelled.replace(false)
                && let Some(f) = inner.on_end.borrow().clone()
            {
                f(false);
            }
            return;
        };
        let value = inner.scale.value();
        let mut sent = false;
        if (value - start).abs() >= f64::EPSILON {
            let callback = inner.on_release.borrow().clone();
            if let Some(f) = callback {
                sent = f(value);
            }
        }
        let end = inner.on_end.borrow().clone();
        if let Some(f) = end {
            f(sent);
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

/// A key a focused `gtk::Range` moves by: the arrows, Home, End, Page Up and
/// Page Down (a change starts on them only).
fn moves_a_range(key: gdk::Key) -> bool {
    matches!(
        key,
        gdk::Key::Left
            | gdk::Key::Right
            | gdk::Key::Up
            | gdk::Key::Down
            | gdk::Key::KP_Left
            | gdk::Key::KP_Right
            | gdk::Key::KP_Up
            | gdk::Key::KP_Down
            | gdk::Key::Home
            | gdk::Key::End
            | gdk::Key::KP_Home
            | gdk::Key::KP_End
            | gdk::Key::Page_Up
            | gdk::Key::Page_Down
            | gdk::Key::KP_Page_Up
            | gdk::Key::KP_Page_Down
    )
}
