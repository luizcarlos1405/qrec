//! Keyboard (G§3.8, A§9.7): one `gtk::EventControllerKey` on the window
//! in the capture phase asks the GTK-free [`router::decide`] about every
//! press and carries the decision out ([`router::apply`]) over the store,
//! the GUI actions and the player. It follows the releases too, so a press
//! of a key already down is known as its auto-repeat ([`router::HeldKeys`],
//! R30); the held keys are forgotten when the window loses the keyboard.
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
//!
//! An edit in progress ends before a press anywhere outside it acts
//! (G§3.12 R12): a chrome button keeps the focus where it was, so a field
//! being typed in stayed focused through the button's action, and its edit
//! was dropped when that changed what the panel edits (the mode switch,
//! R38) or stayed unsent (Play). The window's capture-phase press handler
//! takes the focus out of the field first (a rename is saved as its Enter
//! saves it): its commit runs against the scene it was typed for, and its
//! command enters the FIFO ahead of whatever the press does. Keys need no
//! such step: while an editable has the focus they all go to it (step 2).

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
use router::{HeldKeys, KeyDecision, KeyInput, KeySink, RouterState};

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
    /// The keys down in the window (auto-repeats).
    held: RefCell<HeldKeys>,
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
                held: RefCell::new(HeldKeys::default()),
            }),
        }
    }

    /// Routes `window`'s key presses (capture phase, before any widget).
    pub fn attach(&self, window: &impl IsA<gtk::Window>) {
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let router = self.clone();
        let win = window.as_ref().downgrade();
        controller.connect_key_pressed(guarded!("keys", move |_, keyval, code, modifiers| {
            let Some(window) = win.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let repeat = router.inner.held.borrow_mut().press(code);
            let input = key_input(&window, keyval, modifiers, repeat);
            if router.handle(&input) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }));
        let router = self.clone();
        controller.connect_key_released(guarded!("keys: a release", move |_, _, code, _| {
            router.inner.held.borrow_mut().release(code);
        }));
        window.as_ref().add_controller(controller);
        end_edits_on_press(window.as_ref());
        // A window that loses the keyboard mid-hold never hears the release.
        let router = self.clone();
        window.as_ref().connect_is_active_notify(guarded!(
            "keys: the window's activity",
            move |window| {
                if !window.is_active() {
                    router.inner.held.borrow_mut().clear();
                }
            }
        ));
    }

    /// Runs binding `id` as its key would, pre-steps included (a button
    /// or menu item with a key, G§3.7): before the project opens, only
    /// `app.quit` (`router::decide_button`).
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
            router::decide_button(id, &rs)
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
            "key {} (editable {}, activatable {}, repeat {}): {decision:?}",
            input
                .chord
                .as_ref()
                .map_or_else(|| "-".to_owned(), ToString::to_string),
            input.editable,
            input.activatable,
            input.repeat
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

    /// The player's frame while the program plays (the effective
    /// playhead); `None` while a bin preview's take plays. The sync takes
    /// a seek to it as its own (the pre-step's, R22).
    fn player_frame(&self, local: &crate::store::LocalUi) -> Option<Frame> {
        if !local.program_playing {
            return None;
        }
        let ns = self.inner.media.position_ns()?;
        let fps = self.inner.store.state().project.canvas.fps;
        let frame = ns_to_frame_floor(i64::try_from(ns).unwrap_or(i64::MAX), fps).max(0) as Frame;
        crate::media::program::key_took_player_frame(frame);
        Some(frame)
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

    fn key_parks(&mut self, frame: Frame) {
        crate::media::program::key_parks(frame);
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

/// The router's view of a press in `window` (`repeat`: the key was down).
fn key_input(
    window: &gtk::Window,
    keyval: gdk::Key,
    modifiers: gdk::ModifierType,
    repeat: bool,
) -> KeyInput {
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
        repeat,
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

/// The widget an edit in progress fills, when the focus `focus` is in
/// one: C29's `.field` box (a numeric field: its text and unit), an
/// `EditableLabel` being edited (a rename), an entry; else the text widget
/// itself. `None` when nothing is being edited.
fn edit_root(focus: &gtk::Widget) -> Option<gtk::Widget> {
    if !is_editable(focus) {
        return None;
    }
    let mut at = Some(focus.clone());
    while let Some(w) = at {
        if w.is::<gtk::Native>() {
            break;
        }
        if w.is::<gtk::EditableLabel>()
            || w.is::<gtk::Entry>()
            || w.is::<gtk::SpinButton>()
            || w.is::<gtk::SearchEntry>()
            || w.is::<gtk::PasswordEntry>()
            || w.has_css_class("field")
        {
            return Some(w);
        }
        at = w.parent();
    }
    Some(focus.clone())
}

/// The widget under a press `event`, in whichever surface it came (the
/// window's, a popover's).
fn press_target(event: &gdk::Event) -> Option<gtk::Widget> {
    let surface = event.surface()?;
    let native = gtk::Native::for_surface(&surface)?;
    let (x, y) = event.position()?;
    let (dx, dy) = native.surface_transform();
    native
        .upcast_ref::<gtk::Widget>()
        .pick(x - dx, y - dy, gtk::PickFlags::DEFAULT)
}

/// Ends the edit in progress before a press anywhere outside it acts
/// (G§3.12 R12): the focus leaves the field, so its own leave-commit runs
/// now, against the scene it was typed for, and its command goes out ahead
/// of the press's; a rename is saved as its Enter saves it. A press inside
/// the edit (its text, its unit, a menu of its own) leaves it alone. The
/// press itself is never claimed: it goes on to what is under it.
fn end_edits_on_press(window: &gtk::Window) {
    let press = gtk::GestureClick::new();
    press.set_button(0);
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    press.connect_pressed(guarded!(
        "keys: a press ends the edit",
        move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Denied);
            let Some(window) = gesture.widget().and_downcast::<gtk::Window>() else {
                return;
            };
            let Some(edit) = gtk::prelude::RootExt::focus(&window).and_then(|f| edit_root(&f))
            else {
                return;
            };
            let target = gesture.current_event().as_ref().and_then(press_target);
            if target.is_some_and(|t| t == edit || t.is_ancestor(&edit)) {
                return;
            }
            tracing::debug!(
                "keys: a press outside the edit ends it ({} {:?})",
                edit.type_().name(),
                edit.widget_name()
            );
            match edit.downcast_ref::<gtk::EditableLabel>() {
                // A rename ends as Enter ends it, saved (its owner puts the
                // keyboard back on its row): losing the focus doesn't end
                // an `EditableLabel`'s edit.
                Some(label) => label.stop_editing(true),
                None => gtk::prelude::RootExt::set_focus(&window, None::<&gtk::Widget>),
            }
        }
    ));
    window.add_controller(press);
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
