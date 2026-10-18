//! Settings › Keyboard (S22): every binding with its keys, the conflict
//! banner, edits through `keys.toml`. Owner: WP9.
//!
//! What it shows is `view::preferences::keyboard_page` of the window's
//! keymap (`Store::keymap`, `keys.toml` as the window loaded it): the
//! banner when two bindings share a key (Q5: neither runs; "1 conflict in
//! ~/.config/qrec/keys.toml" / "X is bound to both Autocut and Remove
//! clips. Neither runs until one is changed." / Open keys.toml), the
//! search, the groups and their rows: the label (240 px), the keys (C02
//! medium caps, "or" apart; a conflicting one with C02's conflict stroke
//! and a 12 px warning), "default C" and "Also bound to Remove clips",
//! then Change shortcut (`pencil`) and, for a binding `keys.toml` sets,
//! Reset to default (`rotate-ccw`).
//!
//! Change shortcut captures a key (S22's capture state: the row R1's
//! highlight, C02's capture cap "…", "Press a key · Esc cancels ·
//! Backspace clears"): the capture has the keyboard (`keys::KEY_CAPTURE`,
//! an editable to the router, G§3.8 step 2), `Esc` ends it, `Backspace`
//! unbinds, any other key (with its modifiers) binds. The edit goes
//! through `qrec_shell::keys_file` (one line of `keys.toml`, comments and
//! order kept, an atomic write; G§3.12 R8) on `qrec-gui-io`, and the
//! window takes the keymap it gives at once (`Store::set_keymap`: the key
//! router, the hint bar, every cap). Losing the focus ends a capture; when
//! one ends or an edit lands, the row's Change shortcut has the keyboard.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use qrec_core::keymap::{KeyChord, Keymap};
use qrec_core::protocol::UiAction;
use qrec_core::view::preferences::{KeyRow, KeyboardPage as View, keyboard_page};

use super::Footer;
use crate::cheatsheet::matched_markup;
use crate::keys::KEY_CAPTURE;
use crate::widgets::button;
use crate::widgets::keycap::{self, CapSize, CapVariant};
use crate::{Ctx, guarded};

/// What a key pressed while capturing does (GTK-free).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captured {
    /// `Esc`: the capture ends, nothing changes.
    Cancel,
    /// `Backspace`: the binding loses its keys.
    Clear,
    /// The binding takes this key.
    Bind(KeyChord),
    /// A modifier alone (or a key Qrec can't bind): the capture waits.
    Wait,
}

/// The capture's answer to a press of `key` (a GDK keyval name) with the
/// modifiers held.
pub fn captured(key: &str, ctrl: bool, alt: bool, shift: bool, super_: bool) -> Captured {
    let plain = !(ctrl || alt || shift || super_);
    match key {
        "Escape" if plain => Captured::Cancel,
        "BackSpace" if plain => Captured::Clear,
        _ => match KeyChord::from_gdk(key, ctrl, alt, shift, super_) {
            Some(chord) => Captured::Bind(chord),
            None => Captured::Wait,
        },
    }
}

/// The Keyboard page.
pub struct KeyboardPage {
    root: gtk::Box,
    parts: Rc<Parts>,
}

impl KeyboardPage {
    pub fn new(ctx: &Ctx) -> Self {
        let files = super::user_files();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("settings-keyboard");
        root.add_css_class("settings-page");
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.add_css_class("keys-body");
        body.set_vexpand(true);

        // The banner (S22: a `warning` 16 % tint, a 1 px `warning` stroke).
        let banner = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        banner.set_widget_name("keys-banner");
        banner.add_css_class("keys-banner");
        banner.set_visible(false);
        banner.set_accessible_role(gtk::AccessibleRole::Alert);
        let icon = gtk::Image::from_icon_name("qrec-triangle-alert-symbolic");
        icon.add_css_class("warning-text");
        icon.set_pixel_size(20);
        icon.set_valign(gtk::Align::Center);
        banner.append(&icon);
        let words = gtk::Box::new(gtk::Orientation::Vertical, 0);
        words.add_css_class("keys-banner-text");
        words.set_hexpand(true);
        let title_line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        title_line.add_css_class("keys-banner-title");
        let title = gtk::Label::new(None);
        title.add_css_class("t-body-strong");
        title.set_valign(gtk::Align::BaselineCenter);
        let path = gtk::Label::new(None);
        path.add_css_class("t-mono");
        path.set_valign(gtk::Align::BaselineCenter);
        title_line.append(&title);
        title_line.append(&path);
        words.append(&title_line);
        let lines = gtk::Box::new(gtk::Orientation::Vertical, 0);
        words.append(&lines);
        banner.append(&words);
        let keymap = ctx.store.keymap();
        let open = button::labeled(
            "Open keys.toml",
            Some("qrec-external-link-symbolic"),
            None,
            &keymap,
            button::Kind::Secondary,
        );
        open.set_widget_name("keys-open");
        open.set_valign(gtk::Align::Center);
        open.set_focus_on_click(true);
        banner.append(&open);
        body.append(&banner);

        let (field, search) = crate::palette::search_field(
            "keys-search",
            "Search shortcuts",
            "Search shortcuts",
            false,
        );
        field.set_hexpand(true);
        body.append(&field);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.set_widget_name("keys-list");
        list.add_css_class("keys-list");
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        scroller.set_widget_name("keys-scroller");
        scroller.set_can_focus(false);
        body.append(&scroller);
        root.append(&body);

        let view = keyboard_page(&keymap, &files.keys_shown, "", None);
        let footer = Footer::new(&view.footer, &view.footer_path);
        root.append(&footer.widget());

        let parts = Rc::new(Parts {
            ctx: ctx.clone(),
            banner,
            banner_title: title,
            banner_path: path,
            banner_lines: lines,
            search: search.clone(),
            list,
            scroller,
            footer,
            capturing: RefCell::new(None),
            refocus: RefCell::new(None),
            capture: RefCell::new(None),
            built: Cell::new(false),
        });
        parts.connect(&search, &open);
        let follower = Rc::downgrade(&parts);
        button::follow_keymap(&root, &keymap, move |_, _| {
            if let Some(parts) = follower.upgrade() {
                parts.refresh();
            }
        });
        Self { root, parts }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Shows the keymap as it is, no capture (the dialog opens).
    pub fn refresh(&self) {
        *self.parts.capturing.borrow_mut() = None;
        self.parts.refresh();
    }

    /// The control the page's focus starts on: the search.
    pub fn focus_target(&self) -> gtk::Widget {
        self.parts.search.clone().upcast()
    }
}

struct Parts {
    ctx: Ctx,
    banner: gtk::Box,
    banner_title: gtk::Label,
    banner_path: gtk::Label,
    banner_lines: gtk::Box,
    search: gtk::Text,
    list: gtk::Box,
    scroller: gtk::ScrolledWindow,
    footer: Footer,
    /// The binding waiting for its new key.
    capturing: RefCell<Option<String>>,
    /// The binding whose Change shortcut takes the keyboard at the next
    /// refresh (a capture ended, an edit landed: the focus stays put).
    refocus: RefCell<Option<String>>,
    /// The capture widget while one runs.
    capture: RefCell<Option<gtk::Box>>,
    built: Cell<bool>,
}

impl Parts {
    /// The page for the keymap, the search and the capture as they are.
    fn refresh(self: &Rc<Self>) {
        let keymap = self.ctx.store.keymap();
        let keys_path = super::user_files().keys_shown;
        let capturing = self.capturing.borrow().clone();
        let view = keyboard_page(
            &keymap,
            &keys_path,
            &self.search.text(),
            capturing.as_deref(),
        );
        self.show_banner(&view);
        let scrolled = self.scroller.vadjustment().value();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        *self.capture.borrow_mut() = None;
        for group in &view.groups {
            let header = gtk::Label::new(Some(&group.title));
            header.add_css_class("t-heading");
            header.add_css_class("keys-group");
            header.set_xalign(0.0);
            header.set_yalign(1.0);
            self.list.append(&header);
            let last = group.rows.len().saturating_sub(1);
            for (n, row) in group.rows.iter().enumerate() {
                self.list.append(&self.row(row, n < last, &keymap));
            }
        }
        // A rebuild keeps the list where it was (the capture's row stays).
        if self.built.replace(true) {
            let adj = self.scroller.vadjustment();
            crate::guard::idle_local_once("settings: the keys list scroll", move || {
                adj.set_value(scrolled)
            });
        }
        // The capture takes the keyboard. A box's `grab_focus` offers the
        // focus to its children (none takes it), so the root's is set.
        if let Some(capture) = self.capture.borrow().clone()
            && let Some(root) = capture.root()
        {
            root.set_focus(Some(&capture));
        } else if let Some(id) = self.refocus.borrow_mut().take() {
            // The root's too: the rebuilt row isn't mapped yet, and the
            // focus GTK moves off the row it dropped would win over a grab.
            let list: gtk::Widget = self.list.clone().upcast();
            if let Some(change) = crate::window::find_named(&list, &format!("keys-change-{id}"))
                && let Some(root) = change.root()
            {
                root.set_focus(Some(&change));
            }
        }
    }

    fn show_banner(&self, view: &View) {
        let Some(banner) = &view.banner else {
            self.banner.set_visible(false);
            return;
        };
        let title = banner
            .title
            .strip_suffix(&banner.path)
            .map(str::trim_end)
            .unwrap_or(&banner.title);
        self.banner_title.set_text(title);
        self.banner_path.set_text(&banner.path);
        while let Some(child) = self.banner_lines.first_child() {
            self.banner_lines.remove(&child);
        }
        for line in &banner.lines {
            let label = gtk::Label::new(Some(line));
            label.add_css_class("t-body-sm");
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_max_width_chars(1);
            label.set_hexpand(true);
            self.banner_lines.append(&label);
        }
        self.banner.set_visible(true);
    }

    /// One S22 row.
    fn row(self: &Rc<Self>, row: &KeyRow, ruled: bool, keymap: &Keymap) -> gtk::Box {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        line.set_widget_name(&format!("keys-row-{}", row.id));
        line.add_css_class("keys-row");
        if ruled {
            line.add_css_class("ruled");
        }
        let label = gtk::Label::new(None);
        label.set_markup(&matched_markup(&row.label, &row.matched));
        label.add_css_class("t-body");
        label.add_css_class("keys-label");
        label.set_xalign(0.0);
        label.set_size_request(240, -1);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        line.append(&label);

        let binding = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        binding.add_css_class("keys-binding");
        binding.set_hexpand(true);
        binding.set_valign(gtk::Align::Center);
        if let Some(caption) = &row.capture {
            line.add_css_class("capturing");
            binding.append(&self.capture_widget(&row.id, caption));
        } else {
            for (n, key) in row.keys.iter().enumerate() {
                if n > 0 {
                    binding.append(&small("or", "muted"));
                }
                if key.conflict {
                    let flagged = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    flagged.add_css_class("keys-conflict");
                    flagged.append(&keycap::chord(&key.caps, CapSize::Md, CapVariant::Conflict));
                    let icon = gtk::Image::from_icon_name("qrec-triangle-alert-symbolic");
                    icon.set_pixel_size(12);
                    icon.add_css_class("warning-text");
                    flagged.append(&icon);
                    binding.append(&flagged);
                } else {
                    binding.append(&keycap::chord(&key.caps, CapSize::Md, CapVariant::Plain));
                }
            }
            if row.keys.is_empty() {
                binding.append(&small("No key", "muted"));
            }
            if let Some(note) = &row.default_keys {
                binding.append(&small(note, "muted"));
            }
            if let Some(conflict) = &row.conflict {
                if row.default_keys.is_some() {
                    binding.append(&small("\u{b7}", "muted"));
                }
                binding.append(&small(conflict, "warning-text"));
            }
        }
        line.append(&binding);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        actions.add_css_class("keys-actions");
        actions.set_valign(gtk::Align::Center);
        let change = button::icon(
            "qrec-pencil-symbolic",
            &format!("Change the shortcut of {}", row.label),
            None,
            keymap,
        );
        change.set_widget_name(&format!("keys-change-{}", row.id));
        change.remove_css_class("icon-32");
        change.add_css_class("icon-24");
        change.set_focus_on_click(true);
        let reset = button::icon(
            "qrec-rotate-ccw-symbolic",
            &format!("Reset {} to its default keys", row.label),
            None,
            keymap,
        );
        reset.set_widget_name(&format!("keys-reset-{}", row.id));
        reset.remove_css_class("icon-32");
        reset.add_css_class("icon-24");
        reset.set_focus_on_click(true);
        // S22: the buttons a row has no use for keep their room unseen.
        for (button, shown) in [(&change, row.capture.is_none()), (&reset, row.resettable)] {
            if !shown {
                button.set_opacity(0.0);
                button.set_sensitive(false);
                button.set_can_target(false);
            }
        }
        let (parts, id) = (Rc::downgrade(self), row.id.clone());
        change.connect_clicked(guarded!("settings: change a shortcut", move |_| {
            if let Some(parts) = parts.upgrade() {
                parts.start_capture(&id);
            }
        }));
        let (parts, id) = (Rc::downgrade(self), row.id.clone());
        reset.connect_clicked(guarded!("settings: reset a shortcut", move |_| {
            if let Some(parts) = parts.upgrade() {
                parts.edit(&id, None);
            }
        }));
        actions.append(&change);
        actions.append(&reset);
        line.append(&actions);
        line
    }

    /// The capture: C02's capture cap "…" and the caption; it takes the
    /// keyboard.
    fn capture_widget(self: &Rc<Self>, id: &str, caption: &str) -> gtk::Box {
        let capture = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        capture.set_widget_name("keys-capture");
        capture.add_css_class(KEY_CAPTURE);
        capture.add_css_class("keys-capture");
        capture.set_focusable(true);
        capture.set_can_focus(true);
        capture.set_accessible_role(gtk::AccessibleRole::Button);
        capture.update_property(&[gtk::accessible::Property::Label(&format!(
            "New key: {caption}"
        ))]);
        capture.append(&keycap::chord(
            &["\u{2026}".to_owned()],
            CapSize::Md,
            CapVariant::Capture,
        ));
        let words = gtk::Label::new(Some(caption));
        words.add_css_class("t-caption");
        capture.append(&words);
        let keys = gtk::EventControllerKey::new();
        let (parts, id_key) = (Rc::downgrade(self), id.to_owned());
        keys.connect_key_pressed(guarded!(
            "settings: a captured key",
            after_panic = glib::Propagation::Stop,
            move |_, keyval, _, modifiers| {
                let Some(parts) = parts.upgrade() else {
                    return glib::Propagation::Stop;
                };
                let Some(name) = keyval.name() else {
                    return glib::Propagation::Stop;
                };
                let answer = captured(
                    name.as_str(),
                    modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK),
                    modifiers.contains(gtk::gdk::ModifierType::ALT_MASK),
                    modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK),
                    modifiers.contains(gtk::gdk::ModifierType::SUPER_MASK),
                );
                match answer {
                    Captured::Wait => return glib::Propagation::Stop,
                    Captured::Cancel => parts.end_capture(),
                    Captured::Clear => parts.edit(&id_key, Some(Vec::new())),
                    Captured::Bind(chord) => parts.edit(&id_key, Some(vec![chord])),
                }
                glib::Propagation::Stop
            }
        ));
        capture.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        let parts = Rc::downgrade(self);
        focus.connect_leave(guarded!(
            "settings: the capture loses the focus",
            move |_| {
                if let Some(parts) = parts.upgrade() {
                    // After the press that moved the focus is handled.
                    crate::guard::idle_local_once("settings: the capture ends", move || {
                        if parts.capturing.borrow().is_some()
                            && !parts
                                .capture
                                .borrow()
                                .as_ref()
                                .is_some_and(|c| c.has_focus())
                        {
                            parts.end_capture();
                        }
                    });
                }
            }
        ));
        capture.add_controller(focus);
        *self.capture.borrow_mut() = Some(capture.clone());
        capture
    }

    fn start_capture(self: &Rc<Self>, id: &str) {
        *self.capturing.borrow_mut() = Some(id.to_owned());
        self.refresh();
    }

    fn end_capture(self: &Rc<Self>) {
        let ended = self.capturing.borrow_mut().take();
        if let Some(id) = ended {
            *self.refocus.borrow_mut() = Some(id);
            self.refresh();
        }
    }

    /// Sets binding `id`'s keys (`Some`; empty unbinds) or resets them
    /// (`None`) in `keys.toml`, on the io worker; the window then takes the
    /// keymap the file gives (or the footer says why it couldn't).
    fn edit(self: &Rc<Self>, id: &str, keys: Option<Vec<KeyChord>>) {
        let path = super::user_files().keys;
        let id_owned = id.to_owned();
        let refocus = id.to_owned();
        tracing::info!("settings: keys.toml {id} = {keys:?}");
        let weak = Rc::downgrade(self);
        self.ctx.media.run_io_then(
            move || {
                let Some(path) = path else {
                    return Err("no home folder to keep keys.toml in".to_owned());
                };
                let done = match &keys {
                    Some(keys) => qrec_shell::keys_file::set_binding(&path, &id_owned, keys),
                    None => qrec_shell::keys_file::reset_binding(&path, &id_owned),
                };
                done.map_err(|e| e.message.clone())
            },
            move |result| {
                let Some(parts) = weak.upgrade() else {
                    return;
                };
                *parts.capturing.borrow_mut() = None;
                *parts.refocus.borrow_mut() = Some(refocus);
                match result {
                    Ok((keymap, problems)) => {
                        parts.footer.show_error(None);
                        // Every cap, the router and the hint bar follow;
                        // the page too (it follows the keymap).
                        parts.ctx.store.set_keymap(keymap, problems);
                    }
                    Err(why) => {
                        tracing::warn!("settings: {why}");
                        parts.footer.show_error(Some(&why));
                        parts.refresh();
                    }
                }
            },
        );
    }

    fn connect(self: &Rc<Self>, search: &gtk::Text, open: &gtk::Button) {
        let parts = Rc::downgrade(self);
        search.connect_changed(guarded!("settings: search the keys", move |_| {
            if let Some(parts) = parts.upgrade() {
                parts.scroller.vadjustment().set_value(0.0);
                parts.built.set(false);
                parts.refresh();
            }
        }));
        // Esc clears the search first; empty, it goes on (the dialog
        // closes on it, settings/mod.rs).
        let keys = gtk::EventControllerKey::new();
        let field = search.clone();
        keys.connect_key_pressed(guarded!(
            "settings: the keys search's Esc",
            after_panic = glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key != gtk::gdk::Key::Escape || field.text().is_empty() {
                    return glib::Propagation::Proceed;
                }
                field.set_text("");
                glib::Propagation::Stop
            }
        ));
        search.add_controller(keys);
        let parts = Rc::downgrade(self);
        open.connect_clicked(guarded!("settings: open keys.toml", move |button| {
            let Some(parts) = parts.upgrade() else {
                return;
            };
            let Some(path) = super::user_files().keys else {
                return;
            };
            let window = button.root().and_downcast::<gtk::Window>();
            crate::desktop::run(
                &parts.ctx,
                window.as_ref(),
                parts.ctx.store.project_dir(),
                &UiAction::OpenPath(path.display().to_string()),
            );
        }));
    }
}

/// A `caption` label in `class` ("or", "default C", the conflict note).
fn small(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("t-caption");
    label.add_css_class(class);
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    /// S22's capture: Esc ends it, Backspace unbinds, a key with its
    /// modifiers binds, a modifier alone waits.
    #[test]
    fn what_a_captured_key_does() {
        assert_eq!(
            captured("Escape", false, false, false, false),
            Captured::Cancel
        );
        assert_eq!(
            captured("BackSpace", false, false, false, false),
            Captured::Clear
        );
        assert_eq!(
            captured("F8", false, false, false, false),
            Captured::Bind(KeyChord::parse("F8").unwrap())
        );
        assert_eq!(
            captured("k", true, false, true, false),
            Captured::Bind(KeyChord::parse("ctrl+shift+k").unwrap())
        );
        // Shift+Esc is a chord to bind; Shift alone waits.
        assert!(matches!(
            captured("Escape", false, false, true, false),
            Captured::Bind(_)
        ));
        assert_eq!(
            captured("Shift_L", false, false, true, false),
            Captured::Wait
        );
        assert_eq!(
            captured("Control_L", true, false, false, false),
            Captured::Wait
        );
    }
}
