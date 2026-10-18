//! C29 numeric field (G§5.1): a `.field` box with a right-aligned
//! `gtk::Text` and a unit label; the arrow keys and the scroll wheel step
//! the value; no ± buttons. Owner: WP5.
//!
//! - The model's value comes in with [`NumericField::set_value`]; while
//!   the person edits (the field has the focus and they typed or stepped
//!   since the text was last handed over or put back), a new model value
//!   doesn't overwrite what they type.
//! - Enter and leaving the field commit: the commit callback gets the text
//!   (once per edit, and only when it differs from the model's). The edit
//!   ends there: what the owner answers shows at once, the field still
//!   focused (the autocut draft's clamped or rounded value), and a later
//!   blur commits nothing again. A value the owner sent as a command stays
//!   until the model moves (its snapshot). `Esc` puts the model's value
//!   back. The rules are [`FieldState`]'s, GTK-free.
//! - `↑` `↓` step by the step (`Shift`: ×10), `PgUp` `PgDn` by ten steps;
//!   the wheel steps only while the field has the focus, so scrolling the
//!   panel never changes a value. A step changes the text; Enter or leaving
//!   commits it (one command per commit).
//! - [`NumericField::set_error`] draws the C29 invalid state with its
//!   message (the text stays the person's to fix); the owner shows the
//!   message where it goes ([`NumericField::on_error`]: a helper text, a
//!   tooltip). The stroke and the message go together: typing, `Esc` and a
//!   model value that replaces the text clear both. With nothing to put
//!   back, `Esc` goes on to what holds the field (the Settings dialog
//!   closes on it).
//!
//! Keys reach the field: the window's router lets everything through while
//! an editable has the focus (G§3.8 step 2).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use qrec_core::view::panel::parse_number;

use crate::guarded;

/// A field's text and whose it is, GTK-free: the rules of
/// [`NumericField`] (its text widget only shows [`FieldState::text`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldState {
    /// The model's text.
    model: String,
    /// The text shown.
    text: String,
    /// The person typed or stepped since the text was last handed over,
    /// put back or replaced by the model's: the text is theirs.
    edited: bool,
    /// A commit is being handed to the owner now.
    committing: bool,
    /// The owner answered the commit being handed over (a model value, a
    /// revert, the invalid state).
    answered: bool,
    /// Handed over and not answered at once (the owner sent a command):
    /// the model it was handed over against. The text stays until the
    /// model moves from it.
    awaiting: Option<String>,
    /// Why the text is invalid (C29), until it is typed over, put back or
    /// replaced.
    error: Option<String>,
}

impl FieldState {
    /// The text shown.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text differs from the model's.
    pub fn is_dirty(&self) -> bool {
        self.text != self.model
    }

    /// Why the text is invalid, if it is.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// `Esc` has something to put back (the text isn't the model's, or is
    /// invalid). Otherwise it goes on to what holds the field: the
    /// Settings dialog closes on it (WP9).
    pub fn takes_escape(&self) -> bool {
        self.is_dirty() || self.error.is_some()
    }

    /// The model's value came in (`focused`: the field has the focus): the
    /// text to show now, when it changes. The person's own text stays
    /// while they edit it, and a value handed over stays until the model
    /// moves; what the owner answers a commit with always shows.
    pub fn set_value(&mut self, model: &str, focused: bool) -> Option<String> {
        self.model = model.to_owned();
        if self.committing {
            self.answered = true;
        } else if (focused && self.edited) || self.awaiting.as_deref() == Some(model) {
            return None;
        }
        self.edited = false;
        self.awaiting = None;
        self.error = None;
        if self.text == self.model {
            return None;
        }
        self.text = self.model.clone();
        Some(self.text.clone())
    }

    /// The person changed the text (typing, a step): an error goes.
    pub fn typed(&mut self, text: &str) {
        self.text = text.to_owned();
        self.edited = true;
        self.error = None;
    }

    /// Enter or leaving: the text to hand to the owner, once per edit and
    /// only when it differs from the model's. [`FieldState::end_commit`]
    /// follows the owner's callback.
    pub fn begin_commit(&mut self) -> Option<String> {
        let edited = std::mem::take(&mut self.edited);
        if !edited || !self.is_dirty() {
            return None;
        }
        self.committing = true;
        self.answered = false;
        Some(self.text.clone())
    }

    /// The owner's callback returned: unanswered, the handed text waits
    /// for the model to move.
    pub fn end_commit(&mut self) {
        if self.committing && !self.answered {
            self.awaiting = Some(self.model.clone());
        }
        self.committing = false;
        self.answered = false;
    }

    /// `Esc`, or the owner refused the text: the model's text back, and
    /// no error.
    pub fn revert(&mut self) -> String {
        self.answer();
        self.edited = false;
        self.awaiting = None;
        self.error = None;
        self.text = self.model.clone();
        self.text.clone()
    }

    /// What the field edits changed under it (another scene, another
    /// layer): `model`'s text, even while the person edits (their edit was
    /// for what went), and no error. The text to show, when it changes.
    pub fn reset(&mut self, model: &str) -> Option<String> {
        self.answer();
        self.model = model.to_owned();
        self.edited = false;
        self.awaiting = None;
        self.error = None;
        if self.text == self.model {
            return None;
        }
        self.text = self.model.clone();
        Some(self.text.clone())
    }

    /// The owner found the text invalid (`Some`, why): it stays the
    /// person's to fix (a new model value doesn't replace it while they are
    /// in the field). `None`: the text is fine.
    pub fn set_error(&mut self, error: Option<&str>) {
        if let Some(why) = error {
            self.answer();
            self.awaiting = None;
            self.edited = true;
            self.error = Some(why.to_owned());
        } else {
            self.error = None;
        }
    }

    fn answer(&mut self) {
        if self.committing {
            self.answered = true;
        }
    }
}

/// How a field writes a stepped value ("1280", "−6.5").
pub type Format = Rc<dyn Fn(f64) -> String>;

/// Called with a committed text.
pub type CommitFn = Rc<dyn Fn(&str)>;

/// Called when the field's error changes (`None`: it went).
pub type ErrorFn = Rc<dyn Fn(Option<&str>)>;

/// A C29 numeric field. Cheap to clone; main thread only.
#[derive(Clone)]
pub struct NumericField {
    inner: Rc<Inner>,
}

struct Inner {
    root: gtk::Box,
    text: gtk::Text,
    unit: gtk::Label,
    units: RefCell<Vec<String>>,
    /// The model's text and whose the shown text is.
    state: RefCell<FieldState>,
    step: Cell<f64>,
    format: RefCell<Format>,
    on_commit: RefCell<Option<CommitFn>>,
    on_error: RefCell<Option<ErrorFn>>,
    /// The error the field draws (and told its owner about).
    shown_error: RefCell<Option<String>>,
    /// Text set by the code, not typed.
    setting: Cell<bool>,
}

impl std::fmt::Debug for NumericField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NumericField")
            .field("text", &self.text())
            .field("state", &*self.inner.state.borrow())
            .finish()
    }
}

impl NumericField {
    /// A field showing `unit` ("px", "%", "dB", "ms") after the value; its
    /// accessible name is `name` ("Position X").
    pub fn new(unit: &str, name: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        root.add_css_class("field");
        root.add_css_class("numeric");
        // The text expands inside the box; the box's width is its parent's
        // call (a 104 px gain field, a column of the inspector).
        root.set_hexpand(false);
        // A text box to screen readers: a bare `GtkText` is an editable's
        // delegate, left out of the tree (role `None`), so its name was
        // heard nowhere (WP10's a11y walk).
        let text = gtk::Text::builder()
            .accessible_role(gtk::AccessibleRole::TextBox)
            .build();
        text.set_hexpand(true);
        text.set_alignment(1.0);
        text.set_width_chars(1);
        text.set_max_width_chars(1);
        text.set_input_purpose(gtk::InputPurpose::Number);
        text.update_property(&[gtk::accessible::Property::Label(name)]);
        let unit_label = gtk::Label::new(Some(unit));
        unit_label.add_css_class("field-unit");
        unit_label.set_visible(!unit.is_empty());
        unit_label.set_can_target(false);
        root.append(&text);
        root.append(&unit_label);
        let field = Self {
            inner: Rc::new(Inner {
                root,
                text,
                unit: unit_label,
                units: RefCell::new(vec![unit.to_owned()]),
                state: RefCell::new(FieldState::default()),
                step: Cell::new(1.0),
                format: RefCell::new(Rc::new(|v: f64| whole(v))),
                on_commit: RefCell::new(None),
                on_error: RefCell::new(None),
                shown_error: RefCell::new(None),
                setting: Cell::new(false),
            }),
        };
        field.wire();
        field
    }

    pub fn widget(&self) -> gtk::Widget {
        self.inner.root.clone().upcast()
    }

    /// The text widget (focus, accessibility).
    pub fn text_widget(&self) -> gtk::Text {
        self.inner.text.clone()
    }

    /// What the field shows now.
    pub fn text(&self) -> String {
        self.inner.text.text().to_string()
    }

    /// The model's value as text. Shown at once unless the person is
    /// editing ([`FieldState::set_value`]); `Esc` and a cancelled edit come
    /// back to it.
    pub fn set_value(&self, text: &str) {
        let focused = self.inner.text.has_focus();
        let shown = self.inner.state.borrow_mut().set_value(text, focused);
        if let Some(shown) = shown {
            self.set_text(&shown);
        }
        self.sync_error();
    }

    /// What the field edits changed under it (another scene, another
    /// layer): `text` shows at once, even while the person edits (their
    /// edit was for what went, so it is dropped, never committed to what
    /// came), and no error.
    pub fn reset(&self, text: &str) {
        let shown = self.inner.state.borrow_mut().reset(text);
        if let Some(shown) = shown {
            self.set_text(&shown);
        }
        self.sync_error();
    }

    /// The text differs from the model's.
    pub fn is_dirty(&self) -> bool {
        self.inner.state.borrow().is_dirty()
    }

    /// The arrow keys' step.
    pub fn set_step(&self, step: f64) {
        self.inner.step.set(step);
    }

    /// How a stepped value is written.
    pub fn set_format(&self, format: impl Fn(f64) -> String + 'static) {
        *self.inner.format.borrow_mut() = Rc::new(format);
    }

    /// Other spellings of the unit the person may type ("db" for "dB").
    pub fn set_unit_aliases(&self, aliases: &[&str]) {
        let mut units = self.inner.units.borrow_mut();
        units.truncate(1);
        units.extend(aliases.iter().map(|a| (*a).to_owned()));
    }

    /// Called with the text on each commit (Enter, leaving the field)
    /// that changed it.
    pub fn on_commit(&self, f: impl Fn(&str) + 'static) {
        *self.inner.on_commit.borrow_mut() = Some(Rc::new(f));
    }

    /// Called with the error each time it changes (`None`: it went), to
    /// show its message: the inspector's helper text, a tooltip.
    pub fn on_error(&self, f: impl Fn(Option<&str>) + 'static) {
        *self.inner.on_error.borrow_mut() = Some(Rc::new(f));
    }

    /// The C29 invalid state (a `record-text` stroke) with why (`Some`), or
    /// none. Invalid text stays the person's to fix: a new model value
    /// doesn't replace it while they are in the field.
    pub fn set_error(&self, error: Option<&str>) {
        self.inner.state.borrow_mut().set_error(error);
        self.sync_error();
    }

    /// Why the text is invalid, if it is.
    pub fn error(&self) -> Option<String> {
        self.inner.state.borrow().error().map(str::to_owned)
    }

    /// Draws the state's error and tells the owner when it changed.
    fn sync_error(&self) {
        let error = self.error();
        if *self.inner.shown_error.borrow() == error {
            return;
        }
        *self.inner.shown_error.borrow_mut() = error.clone();
        if error.is_some() {
            self.inner.root.add_css_class("invalid");
        } else {
            self.inner.root.remove_css_class("invalid");
        }
        let hook = self.inner.on_error.borrow().clone();
        if let Some(f) = hook {
            f(error.as_deref());
        }
    }

    pub fn set_sensitive(&self, sensitive: bool) {
        self.inner.root.set_sensitive(sensitive);
    }

    /// The unit label (its text: "px").
    pub fn unit(&self) -> gtk::Label {
        self.inner.unit.clone()
    }

    /// Puts the model's value back (and no error).
    pub fn revert(&self) {
        let model = self.inner.state.borrow_mut().revert();
        self.set_text(&model);
        self.sync_error();
    }

    /// Hands the person's text to the owner, once per edit, if it changed
    /// ([`FieldState::begin_commit`]).
    pub fn commit(&self) {
        let Some(text) = self.inner.state.borrow_mut().begin_commit() else {
            return;
        };
        let callback = self.inner.on_commit.borrow().clone();
        if let Some(f) = callback {
            // A panic in the owner is reported here, and the commit still
            // ends (else every later value would count as its answer).
            crate::guard::catch("numeric field: commit", || f(&text));
        }
        self.inner.state.borrow_mut().end_commit();
    }

    fn set_text(&self, text: &str) {
        self.inner.setting.set(true);
        self.inner.text.set_text(text);
        self.inner.setting.set(false);
    }

    /// Steps the value by `steps` steps (text only; Enter commits).
    pub fn step_by(&self, steps: f64) {
        let units = self.inner.units.borrow().clone();
        let units: Vec<&str> = units.iter().map(String::as_str).collect();
        let model = self.inner.state.borrow().model.clone();
        let current = parse_number(&self.text(), &units)
            .or_else(|| parse_number(&model, &units))
            .unwrap_or(0.0);
        let next = current + steps * self.inner.step.get();
        let format = self.inner.format.borrow().clone();
        let next = format(next);
        self.inner.state.borrow_mut().typed(&next);
        self.set_text(&next);
        self.inner.text.set_position(-1);
        self.sync_error();
    }

    fn wire(&self) {
        let inner = &self.inner;
        // Enter commits.
        let weak = Rc::downgrade(inner);
        inner
            .text
            .connect_activate(guarded!("numeric field: activate", move |_| {
                if let Some(inner) = weak.upgrade() {
                    NumericField { inner }.commit();
                }
            }));
        // Leaving commits.
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(inner);
        focus.connect_leave(guarded!("numeric field: leave", move |_| {
            if let Some(inner) = weak.upgrade() {
                NumericField { inner }.commit();
            }
        }));
        inner.text.add_controller(focus);
        // Arrows step, Esc reverts (before the text widget sees them).
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(inner);
        keys.connect_key_pressed(guarded!(
            "numeric field: keys",
            move |_, key, _code, modifiers| {
                let Some(inner) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let field = NumericField { inner };
                let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
                let ten = if shift { 10.0 } else { 1.0 };
                match key {
                    gdk::Key::Up | gdk::Key::KP_Up => field.step_by(ten),
                    gdk::Key::Down | gdk::Key::KP_Down => field.step_by(-ten),
                    gdk::Key::Page_Up | gdk::Key::KP_Page_Up => field.step_by(10.0),
                    gdk::Key::Page_Down | gdk::Key::KP_Page_Down => field.step_by(-10.0),
                    gdk::Key::Escape if field.inner.state.borrow().takes_escape() => field.revert(),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        ));
        inner.root.add_controller(keys);
        // The wheel steps while the field has the focus.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = Rc::downgrade(inner);
        scroll.connect_scroll(guarded!(
            "numeric field: scroll",
            after_panic = glib::Propagation::Proceed,
            move |_, _dx, dy| {
                let Some(inner) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if !inner.text.has_focus() || dy == 0.0 {
                    return glib::Propagation::Proceed;
                }
                NumericField { inner }.step_by(if dy < 0.0 { 1.0 } else { -1.0 });
                glib::Propagation::Stop
            }
        ));
        inner.root.add_controller(scroll);
        // A click anywhere on the box focuses the text.
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(inner);
        click.connect_pressed(guarded!("numeric field: click", move |_, _, _, _| {
            if let Some(inner) = weak.upgrade()
                && !inner.text.has_focus()
            {
                inner.text.grab_focus();
            }
        }));
        inner.root.add_controller(click);
        // Typing makes the text the person's and clears the invalid state
        // (the stroke and its message).
        let weak = Rc::downgrade(inner);
        inner
            .text
            .connect_changed(guarded!("numeric field: changed", move |text| {
                if let Some(inner) = weak.upgrade()
                    && !inner.setting.get()
                {
                    inner.state.borrow_mut().typed(&text.text());
                    NumericField { inner }.sync_error();
                }
            }));
    }
}

/// A whole number with the true minus ("1280", "−20").
pub fn whole(v: f64) -> String {
    let v = v.round() as i64;
    if v < 0 {
        format!("\u{2212}{}", v.unsigned_abs())
    } else {
        v.to_string()
    }
}

/// One decimal with the true minus ("0.0", "−6.5").
pub fn tenths(v: f64) -> String {
    let text = format!("{:.1}", v.abs());
    if v < 0.0 && text != "0.0" {
        format!("\u{2212}{text}")
    } else {
        text
    }
}

/// A signed whole number ("+0", "+80", "−20"): sync offsets.
pub fn signed(v: f64) -> String {
    let v = v.round() as i64;
    if v < 0 {
        format!("\u{2212}{}", v.unsigned_abs())
    } else {
        format!("+{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R47: what the owner answers a commit with shows at once, the field
    /// still focused: the autocut draft clamps 5000 to 500 and rounds
    /// −35.4 to −35 (the draft unchanged), and a blur after Enter commits
    /// nothing again.
    #[test]
    fn a_commit_shows_the_owners_answer() {
        let mut f = FieldState::default();
        assert_eq!(f.set_value("120", false).as_deref(), Some("120"));
        f.typed("5000");
        assert_eq!(f.set_value("120", true), None, "the person's text stays");
        assert_eq!(f.begin_commit().as_deref(), Some("5000"));
        assert_eq!(f.set_value("500", true).as_deref(), Some("500"));
        f.end_commit();
        assert_eq!(f.text(), "500");
        assert_eq!(f.begin_commit(), None, "a blur after Enter");
        // Rounded back to the same value: its text replaces the typed one.
        f.set_value("\u{2212}35", true);
        f.typed("\u{2212}35.4");
        assert_eq!(f.begin_commit().as_deref(), Some("\u{2212}35.4"));
        assert_eq!(
            f.set_value("\u{2212}35", true).as_deref(),
            Some("\u{2212}35")
        );
        f.end_commit();
        assert!(!f.is_dirty());
        // A slider moved after Enter: the field follows it, and leaving
        // doesn't send the typed value back.
        f.typed("-40");
        assert_eq!(f.begin_commit().as_deref(), Some("-40"));
        f.set_value("\u{2212}40", true);
        f.end_commit();
        assert_eq!(
            f.set_value("\u{2212}50", true).as_deref(),
            Some("\u{2212}50")
        );
        assert_eq!(f.begin_commit(), None, "the blur after the drag");
    }

    /// A value the owner sent as a command stays until its snapshot moves
    /// the model; a refresh before it doesn't flash the old value.
    #[test]
    fn a_sent_value_waits_for_the_model() {
        let mut f = FieldState::default();
        f.set_value("1500", false);
        f.typed("1280");
        assert_eq!(f.begin_commit().as_deref(), Some("1280"));
        f.end_commit();
        assert_eq!(f.set_value("1500", true), None, "an unrelated refresh");
        assert_eq!(f.text(), "1280");
        assert_eq!(f.set_value("1280", true), None, "its snapshot");
        assert!(!f.is_dirty());
        assert_eq!(f.begin_commit(), None);
        // A later change from elsewhere shows, the field still focused.
        assert_eq!(f.set_value("1300", true).as_deref(), Some("1300"));
    }

    /// Esc puts the model's text back; invalid text stays the person's
    /// while they are in the field, and goes with the next value once
    /// they left it.
    #[test]
    fn esc_and_invalid_text() {
        let mut f = FieldState::default();
        f.set_value("384", false);
        f.typed("0");
        assert_eq!(f.begin_commit().as_deref(), Some("0"));
        f.set_error(Some("Width must be at least 1"));
        f.end_commit();
        assert_eq!(f.set_value("384", true), None, "the person fixes it");
        assert_eq!(f.text(), "0");
        assert_eq!(f.error(), Some("Width must be at least 1"));
        assert_eq!(f.revert(), "384");
        assert_eq!(f.error(), None, "Esc clears the error too");
        assert_eq!(f.begin_commit(), None);
        f.typed("0");
        assert_eq!(f.begin_commit().as_deref(), Some("0"));
        f.set_error(Some("Width must be at least 1"));
        f.end_commit();
        assert_eq!(f.set_value("384", false).as_deref(), Some("384"), "left");
        assert_eq!(f.error(), None, "a value that replaces the text clears it");
        // Typing clears it as it clears the stroke.
        f.typed("0");
        f.begin_commit();
        f.set_error(Some("Width must be at least 1"));
        f.end_commit();
        f.typed("1");
        assert_eq!(f.error(), None);
        // Typed back to the model's text: nothing to commit.
        f.typed("38");
        f.typed("384");
        assert_eq!(f.begin_commit(), None);
    }

    /// Esc puts back an edit or an invalid text; with nothing to put back
    /// it goes on (the Settings dialog closes on it, WP9).
    #[test]
    fn esc_goes_on_with_nothing_to_put_back() {
        let mut f = FieldState::default();
        f.set_value("3", false);
        assert!(!f.takes_escape());
        f.typed("5");
        assert!(f.takes_escape(), "an edit");
        f.revert();
        assert!(!f.takes_escape(), "put back");
        f.typed("0");
        f.begin_commit();
        f.set_error(Some("must be at least 1"));
        f.end_commit();
        f.typed("3");
        f.set_error(Some("must be at least 1"));
        assert!(f.takes_escape(), "invalid, though it reads as the model");
    }

    /// R38: what the field edits changed under the edit (another scene, an
    /// other layer): the new value shows though the person edits, and the
    /// edit is gone (nothing left to commit, no error).
    #[test]
    fn a_new_scene_drops_the_edit() {
        let mut f = FieldState::default();
        f.set_value("1500", false);
        f.typed("200");
        assert_eq!(f.set_value("1500", true), None, "the person edits");
        assert_eq!(f.reset("1500").as_deref(), Some("1500"));
        assert_eq!(f.begin_commit(), None, "nothing to commit");
        f.typed("0");
        f.begin_commit();
        f.set_error(Some("Width must be at least 1"));
        f.end_commit();
        assert_eq!(f.reset("384").as_deref(), Some("384"));
        assert_eq!(f.error(), None);
    }

    #[test]
    fn formats_use_the_true_minus() {
        assert_eq!(whole(1280.0), "1280");
        assert_eq!(whole(-20.4), "\u{2212}20");
        assert_eq!(tenths(-6.0), "\u{2212}6.0");
        assert_eq!(tenths(-0.04), "0.0");
        assert_eq!(tenths(3.5), "3.5");
        assert_eq!(signed(0.0), "+0");
        assert_eq!(signed(-20.0), "\u{2212}20");
        // What a step writes parses back.
        for text in [whole(-20.0), tenths(-6.5), signed(-30.0)] {
            assert!(parse_number(&text, &[]).is_some(), "{text}");
        }
    }
}
