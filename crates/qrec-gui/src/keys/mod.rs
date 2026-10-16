//! Keyboard (G§3.8, A§9.7): one `gtk::EventControllerKey` on the window
//! in the capture phase asks the GTK-free [`router::decide`] about every
//! press and carries the decision out ([`router::apply`]) over the store,
//! the GUI actions and the player.
//!
//! Buttons and menu items that have a binding call
//! [`KeyRouter::run_binding`], so they get the key's pre-steps (G§3.7).
//! A widget that owns a drag hands the router its cancel
//! ([`KeyRouter::set_drag_cancel`]) for `Esc` (D-42).
//!
//! Focus: chrome buttons are `focus-on-click = false`; the stage canvas,
//! the timeline canvas and bin rows take focus on click (each region's
//! business); `LocalUi.focus` follows the window's focus widget
//! (`window/mod.rs`).

pub mod actions;
pub mod router;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use qrec_core::keymap::KeyChord;
use qrec_core::time::ns_to_frame_floor;
use qrec_core::view::notices::Notice;
use qrec_core::{Command, Frame};

use crate::guarded;
use crate::media::MediaHub;
use crate::store::Store;
use actions::ActionRouter;
use router::{KeyDecision, KeyInput, KeySink, RouterState};

/// The window's key router. Cheap to clone; main thread only.
#[derive(Clone)]
pub struct KeyRouter {
    inner: Rc<Inner>,
}

struct Inner {
    store: Store,
    actions: ActionRouter,
    media: MediaHub,
    drag_cancel: RefCell<Option<Rc<dyn Fn()>>>,
}

impl std::fmt::Debug for KeyRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyRouter").finish_non_exhaustive()
    }
}

impl KeyRouter {
    pub fn new(store: Store, actions: ActionRouter, media: MediaHub) -> Self {
        Self {
            inner: Rc::new(Inner {
                store,
                actions,
                media,
                drag_cancel: RefCell::new(None),
            }),
        }
    }

    /// Routes `window`'s key presses (capture phase, before any widget).
    pub fn attach(&self, window: &impl IsA<gtk::Window>) {
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let router = self.clone();
        let win = window.as_ref().downgrade();
        controller.connect_key_pressed(guarded!("keys", move |_, keyval, _code, modifiers| {
            let Some(window) = win.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let input = key_input(&window, keyval, modifiers);
            if router.handle(&input) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }));
        window.as_ref().add_controller(controller);
    }

    /// Runs binding `id` as its key would, pre-steps included (a button
    /// or menu item with a key, G§3.7).
    pub fn run_binding(&self, id: &str) {
        let store = &self.inner.store;
        let state = store.state();
        let keymap = store.keymap();
        let decision = {
            let local = store.local();
            let rs = RouterState {
                state: &state,
                local: &local,
                keymap: &keymap,
                player_frame: self.player_frame(&local),
            };
            router::decide_binding(id, &rs)
        };
        self.carry_out(&decision, &state);
    }

    /// The cancel of the drag in progress (`Esc`, D-42); `None` when it
    /// ends.
    pub fn set_drag_cancel(&self, cancel: Option<Rc<dyn Fn()>>) {
        *self.inner.drag_cancel.borrow_mut() = cancel;
    }

    /// One press: decide and carry out; `true` when it was used.
    pub fn handle(&self, input: &KeyInput) -> bool {
        let store = &self.inner.store;
        let state = store.state();
        let keymap = store.keymap();
        let decision = {
            let local = store.local();
            let rs = RouterState {
                state: &state,
                local: &local,
                keymap: &keymap,
                player_frame: self.player_frame(&local),
            };
            router::decide(input, &rs)
        };
        tracing::debug!(
            "key {} (editable {}, activatable {}): {decision:?}",
            input
                .chord
                .as_ref()
                .map_or_else(|| "-".to_owned(), ToString::to_string),
            input.editable,
            input.activatable
        );
        self.carry_out(&decision, &state)
    }

    fn carry_out(&self, decision: &KeyDecision, state: &qrec_core::State) -> bool {
        let mut sink = GtkSink {
            router: self,
            deferred_gui: Vec::new(),
        };
        let used = router::apply(decision, state, &mut sink);
        // GUI actions run last, outside any borrow.
        for id in sink.deferred_gui {
            self.inner.actions.run(&id);
        }
        used
    }

    /// The player's frame while it plays (the effective playhead).
    fn player_frame(&self, local: &crate::store::LocalUi) -> Option<Frame> {
        if !local.playing {
            return None;
        }
        let ns = self.inner.media.position_ns()?;
        let fps = self.inner.store.state().project.canvas.fps;
        Some(ns_to_frame_floor(i64::try_from(ns).unwrap_or(i64::MAX), fps).max(0) as Frame)
    }
}

/// The decision's effects on the window.
struct GtkSink<'a> {
    router: &'a KeyRouter,
    deferred_gui: Vec<String>,
}

impl KeySink for GtkSink<'_> {
    fn dispatch(&mut self, cmd: Command) {
        self.router.inner.store.dispatch(cmd);
    }

    fn gui(&mut self, id: &str) {
        self.deferred_gui.push(id.to_owned());
    }

    fn notice(&mut self, notice: Notice) {
        self.router.inner.store.notify(notice);
    }

    fn set_playing_rate(&mut self, rate: f64) {
        self.router
            .inner
            .store
            .update_local(move |l| l.playing_rate = rate);
    }

    fn player_rate(&mut self, rate: f64) {
        if let Some(player) = self.router.inner.media.player() {
            player.play(rate);
        }
    }

    fn cancel_drag(&mut self) {
        let cancel = self.router.inner.drag_cancel.borrow().clone();
        match cancel {
            Some(cancel) => cancel(),
            // Nobody to tell: just forget the drag.
            None => self.router.inner.store.update_local(|l| l.drag = None),
        }
    }

    fn close_overlay(&mut self) {
        self.router.inner.store.update_local(|l| {
            l.overlays.pop();
        });
    }
}

/// The router's view of a press in `window`.
fn key_input(window: &gtk::Window, keyval: gdk::Key, modifiers: gdk::ModifierType) -> KeyInput {
    let chord = keyval.name().and_then(|name| {
        KeyChord::from_gdk(
            name.as_str(),
            modifiers.contains(gdk::ModifierType::CONTROL_MASK),
            modifiers.contains(gdk::ModifierType::ALT_MASK),
            modifiers.contains(gdk::ModifierType::SHIFT_MASK),
            modifiers.contains(gdk::ModifierType::SUPER_MASK),
        )
    });
    let focus = gtk::prelude::RootExt::focus(window);
    KeyInput {
        chord,
        editable: focus.as_ref().is_some_and(is_editable),
        activatable: window.gets_focus_visible() && focus.as_ref().is_some_and(is_activatable),
    }
}

/// Typing goes to it: a text field, a numeric field, a label being edited.
pub fn is_editable(widget: &gtk::Widget) -> bool {
    let mut at = Some(widget.clone());
    while let Some(w) = at {
        if let Some(label) = w.downcast_ref::<gtk::EditableLabel>() {
            return label.is_editing();
        }
        if w.is::<gtk::Text>() || w.is::<gtk::TextView>() {
            return true;
        }
        if w.is::<gtk::Window>() {
            break;
        }
        at = w.parent();
    }
    false
}

/// A widget that uses `Space`, `Enter` or the arrows itself: a button,
/// toggle, check, switch, scale, dropdown or a list's row.
pub fn is_activatable(widget: &gtk::Widget) -> bool {
    if widget.is::<gtk::Button>()
        || widget.is::<gtk::CheckButton>()
        || widget.is::<gtk::Switch>()
        || widget.is::<gtk::Range>()
        || widget.is::<gtk::DropDown>()
        || widget.is::<gtk::ListBoxRow>()
    {
        return true;
    }
    let mut at = widget.parent();
    while let Some(w) = at {
        if w.is::<gtk::ListView>()
            || w.is::<gtk::GridView>()
            || w.is::<gtk::ColumnView>()
            || w.is::<gtk::DropDown>()
            || w.is::<adw::ToggleGroup>()
        {
            return true;
        }
        if w.is::<gtk::Window>() {
            break;
        }
        at = w.parent();
    }
    false
}
