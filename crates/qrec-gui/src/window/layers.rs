//! The window's overlay layers (G§3.15): what opens over the editor and
//! how it keeps the keyboard.
//!
//! - `Modal`, `Settings` and `Palette` go into the content layer over a
//!   `scrim` that stops above the hint bar (its CSS bottom margin: the
//!   bar stays live, S18, S18b); stacked layers stack their scrims. They
//!   trap focus: everything below stops taking pointer events and leaves
//!   the focus chain.
//! - `Fullscreen` (the cheat sheet, gated) covers the whole window with a
//!   `scrim-strong`.
//! - `FloatingTrap` (the export panel in Setup, Done and Failed) traps focus
//!   and adds a transparent click-catcher that dismisses it, like a
//!   popover.
//! - `FloatingPassive` (the export panel while Running, D-49) adds nothing:
//!   no trap, no click-catcher.
//!
//! Opening moves the focus into the widget (its first focusable child; the
//! caller may focus another one right after); closing gives the focus back
//! to what had it, unless a trap stays open above that (the focus stays in
//! the top trap); a layer opened over it that took the focus from inside
//! it gives it back where this one would have. Where a widget sits is its
//! own `halign`, `valign` and margins (BRIEF §2.8 placements).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// Where an overlay goes (G§3.15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    Modal,
    Settings,
    Palette,
    Fullscreen,
    FloatingTrap,
    FloatingPassive,
}

impl Layer {
    /// It keeps the keyboard and the pointer while it is the top trap.
    pub fn traps(self) -> bool {
        !matches!(self, Layer::FloatingPassive)
    }

    /// It dims what is below.
    fn scrim(self) -> Option<&'static str> {
        match self {
            Layer::Modal | Layer::Settings | Layer::Palette => Some("scrim"),
            Layer::Fullscreen => Some("scrim-strong"),
            Layer::FloatingTrap | Layer::FloatingPassive => None,
        }
    }
}

/// The layers of one window. Cheap to clone; main thread only.
#[derive(Clone)]
pub struct WindowLayers {
    inner: Rc<Inner>,
}

struct Inner {
    /// Over the whole window (the cheat sheet).
    window_layer: gtk::Overlay,
    /// Over everything but the hint bar.
    content_layer: gtk::Overlay,
    /// What a trap disables below the content layer's overlays (the
    /// editor, the page) and below the window layer's (also the hint bar).
    content_base: RefCell<Vec<gtk::Widget>>,
    window_base: RefCell<Vec<gtk::Widget>>,
    /// The base widgets a trap took out, with their own `can-target` and
    /// `can-focus` from before (given back when it goes: the hint bar
    /// never takes the focus, whatever traps came and went).
    taken: RefCell<Vec<(gtk::Widget, bool, bool)>>,
    open: RefCell<Vec<Open>>,
}

struct Open {
    layer: Layer,
    widget: gtk::Widget,
    /// The scrim or click-catcher under it.
    under: Option<gtk::Widget>,
    focus_before: Option<glib::WeakRef<gtk::Widget>>,
}

impl std::fmt::Debug for WindowLayers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let open: Vec<Layer> = self.inner.open.borrow().iter().map(|o| o.layer).collect();
        f.debug_struct("WindowLayers").field("open", &open).finish()
    }
}

impl WindowLayers {
    /// The layers of a window whose root is `window_layer`, with the
    /// content layer `content_layer` inside it.
    pub fn new(window_layer: &gtk::Overlay, content_layer: &gtk::Overlay) -> Self {
        Self {
            inner: Rc::new(Inner {
                window_layer: window_layer.clone(),
                content_layer: content_layer.clone(),
                content_base: RefCell::new(Vec::new()),
                window_base: RefCell::new(Vec::new()),
                taken: RefCell::new(Vec::new()),
                open: RefCell::new(Vec::new()),
            }),
        }
    }

    /// What traps disable: `content` under the content layer's overlays,
    /// `window` (also the hint bar) under the window layer's.
    pub fn set_base(&self, content: Vec<gtk::Widget>, window: Vec<gtk::Widget>) {
        *self.inner.content_base.borrow_mut() = content;
        *self.inner.window_base.borrow_mut() = window;
        self.refresh();
    }

    /// Opens `widget` in `layer` (a layer already open is replaced).
    pub fn open(&self, layer: Layer, widget: &impl IsA<gtk::Widget>) {
        self.open_with(layer, widget.as_ref(), None);
    }

    /// Opens `widget` as a floating trap (`Layer::FloatingTrap`) whose
    /// click-catcher calls `dismiss` (it should close the layer).
    pub fn open_dismissable(&self, widget: &impl IsA<gtk::Widget>, dismiss: impl Fn() + 'static) {
        self.open_with(Layer::FloatingTrap, widget.as_ref(), Some(Rc::new(dismiss)));
    }

    /// Closes `layer` if it is open; the focus goes back to what had it, as
    /// long as no trap stays open above that: a layer closing under a trap
    /// (the passive export panel under Settings, Settings under the export
    /// panel's Done page) never takes the focus out of the top trap.
    pub fn close(&self, layer: Layer) {
        let closed = {
            let mut open = self.inner.open.borrow_mut();
            let closed = open
                .iter()
                .position(|o| o.layer == layer)
                .map(|i| (i, open.remove(i)));
            // A layer opened over it took the focus from inside it: it gives
            // the focus back to where this one would have.
            if let Some((i, closed)) = &closed {
                for above in open.iter_mut().skip(*i) {
                    let inside = above
                        .focus_before
                        .as_ref()
                        .and_then(glib::WeakRef::upgrade)
                        .is_none_or(|w| w == closed.widget || w.is_ancestor(&closed.widget));
                    if inside {
                        above.focus_before.clone_from(&closed.focus_before);
                    }
                }
            }
            closed.map(|(_, closed)| closed)
        };
        let Some(closed) = closed else {
            return;
        };
        let parent = self.overlay_of(layer);
        if let Some(under) = &closed.under {
            parent.remove_overlay(under);
        }
        parent.remove_overlay(&closed.widget);
        self.refresh();
        let top = self.top_trap();
        let before = closed
            .focus_before
            .and_then(|w| w.upgrade())
            .filter(|w| w.root().is_some() && w.is_sensitive());
        match (top, before) {
            (None, Some(before)) => {
                before.grab_focus();
            }
            (Some(top), Some(before)) if before == top || before.is_ancestor(&top) => {
                before.grab_focus();
            }
            (Some(top), _) => {
                let focus = parent.root().and_then(|root| root.focus());
                if !focus.is_some_and(|f| f == top || f.is_ancestor(&top)) {
                    top.child_focus(gtk::DirectionType::TabForward);
                }
            }
            (None, None) => {}
        }
    }

    /// The top trap's widget, if a trap is open.
    fn top_trap(&self) -> Option<gtk::Widget> {
        self.inner
            .open
            .borrow()
            .iter()
            .rev()
            .find(|o| o.layer.traps())
            .map(|o| o.widget.clone())
    }

    /// Whether `layer` is open.
    pub fn is_open(&self, layer: Layer) -> bool {
        self.inner.open.borrow().iter().any(|o| o.layer == layer)
    }

    /// The newest open layer.
    pub fn top(&self) -> Option<Layer> {
        self.inner.open.borrow().last().map(|o| o.layer)
    }

    /// The newest open layer's widget (the test channel's `local`: whether
    /// the focus is inside it).
    pub fn top_widget(&self) -> Option<gtk::Widget> {
        self.inner.open.borrow().last().map(|o| o.widget.clone())
    }

    fn overlay_of(&self, layer: Layer) -> &gtk::Overlay {
        match layer {
            Layer::Fullscreen => &self.inner.window_layer,
            _ => &self.inner.content_layer,
        }
    }

    fn open_with(&self, layer: Layer, widget: &gtk::Widget, dismiss: Option<Rc<dyn Fn()>>) {
        self.close(layer);
        let parent = self.overlay_of(layer);
        let focus_before = parent
            .root()
            .and_then(|root| root.focus())
            .map(|w| w.downgrade());
        let under = match (layer.scrim(), dismiss) {
            (Some(class), _) => {
                let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
                scrim.add_css_class(class);
                scrim.set_hexpand(true);
                scrim.set_vexpand(true);
                Some(scrim.upcast::<gtk::Widget>())
            }
            (None, Some(dismiss)) => {
                let catcher = gtk::Box::new(gtk::Orientation::Vertical, 0);
                catcher.set_hexpand(true);
                catcher.set_vexpand(true);
                let click = gtk::GestureClick::new();
                click.connect_pressed(crate::guarded!(
                    "layers: the click-catcher",
                    move |gesture, _, _, _| {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        dismiss();
                    }
                ));
                catcher.add_controller(click);
                Some(catcher.upcast())
            }
            (None, None) => None,
        };
        if let Some(under) = &under {
            parent.add_overlay(under);
        }
        parent.add_overlay(widget);
        self.inner.open.borrow_mut().push(Open {
            layer,
            widget: widget.clone(),
            under,
            focus_before,
        });
        self.refresh();
        if layer.traps() {
            widget.child_focus(gtk::DirectionType::TabForward);
        }
    }

    /// Enables the top trap and what is above it; disables the rest.
    fn refresh(&self) {
        let open = self.inner.open.borrow();
        let top_trap = open.iter().rposition(|o| o.layer.traps());
        let window_trap = open
            .iter()
            .any(|o| o.layer == Layer::Fullscreen && o.layer.traps());
        // Scrims and click-catchers stay targetable: the top one takes the
        // clicks meant for what is below it.
        for (i, o) in open.iter().enumerate() {
            set_live(&o.widget, top_trap.is_none_or(|t| i >= t));
        }
        // A widget of both bases (the stack, the toasts) is out when either
        // trap applies, and one given back gets its own state again: the
        // window base's pass used to make the content trap's stack and
        // toasts live again (Tab left a modal), and the hint bar focusable.
        let content_trapped = top_trap.is_some();
        let content = self.inner.content_base.borrow();
        let window = self.inner.window_base.borrow();
        for w in content.iter().chain(window.iter()) {
            let trapped =
                (content_trapped && content.contains(w)) || (window_trap && window.contains(w));
            self.take_out(w, trapped);
        }
    }

    /// Takes `widget` out of the pointer and the focus (`out`), keeping
    /// what it had, or gives that back.
    fn take_out(&self, widget: &gtk::Widget, out: bool) {
        let mut taken = self.inner.taken.borrow_mut();
        let at = taken.iter().position(|(w, _, _)| w == widget);
        match (out, at) {
            (true, None) => {
                taken.push((widget.clone(), widget.can_target(), widget.can_focus()));
                set_live(widget, false);
            }
            (false, Some(i)) => {
                let (widget, target, focus) = taken.remove(i);
                widget.set_can_target(target);
                widget.set_can_focus(focus);
            }
            _ => {}
        }
    }
}

/// Takes pointer events and the focus, or neither.
fn set_live(widget: &gtk::Widget, live: bool) {
    widget.set_can_target(live);
    widget.set_can_focus(live);
}
