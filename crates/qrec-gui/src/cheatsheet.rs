//! The cheat sheet (S20, S20b; SPEC §10.1 layer 2): every key in three
//! columns over the whole window. Owner: WP9. It registers `keys.help`
//! (`?`).
//!
//! What it shows is `qrec_core::view::cheatsheet` (S20's rows, the keys
//! that run, "default P", the search). S20's anatomy: `scrim-strong` over
//! the whole window (`Layer::Fullscreen`), the sheet 40 px in from each
//! edge (1520×920 at 1600×1000), `raised`, `xl` radius, e3, a 1 px
//! `border`; a 72 px header ("Keyboard shortcuts" in `headline`, the
//! 320×32 search field, "× Close [Esc]"); the body 24/32 padded in three
//! columns 48 apart, each its groups 24 apart: a 32 px title (`heading`),
//! 40 px rows with a hairline between them, the label (`body`) at the
//! left and the large key caps at the right, a chord's caps 2 apart,
//! "/" between bindings and "or" between a binding's keys (`caption`
//! `muted`), "default P" after them; a 48 px footer ("Keys come from
//! ~/.config/qrec/keys.toml. …" and `[Ctrl][,] Settings`).
//!
//! The search field has the keyboard while it is open: typing searches
//! (S20b: the rows left in one column under "3 shortcuts match “trim”",
//! the palette's tip under them), `Esc` closes, `Ctrl+,` opens Settings ›
//! Keyboard. Copy niri snippet copies SPEC §10.2's binds and says
//! "Copied" for 2 s.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use qrec_core::protocol::UiAction;
use qrec_core::view::cheatsheet::{CheatGroup, CheatRow, CheatSheet as View, cheat_sheet};
use qrec_core::view::palette::highlight_ranges;
use qrec_core::view::settings::SettingsPage;

use crate::store::local::{LocalUi, Overlay};
use crate::widgets::button;
use crate::widgets::keycap::{self, CapSize, CapVariant};
use crate::window::layers::Layer;
use crate::{Ctx, guard, guarded};

/// Whether the cheat sheet is built (`GuiFacts.built`).
pub const BUILT: bool = true;

/// How long Copy niri snippet says "Copied".
const COPIED_FOR: Duration = Duration::from_secs(2);

/// `label` as Pango markup with the chars `matched` at weight 600 (the
/// search's bold, S20b).
pub fn matched_markup(label: &str, matched: &[usize]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for range in highlight_ranges(label, matched) {
        out.push_str(&glib::markup_escape_text(&label[at..range.start]));
        out.push_str("<span weight=\"600\">");
        out.push_str(&glib::markup_escape_text(&label[range.clone()]));
        out.push_str("</span>");
        at = range.end;
    }
    out.push_str(&glib::markup_escape_text(&label[at..]));
    out
}

/// A separator between key caps: "/" between bindings, "or" between a
/// binding's keys (`caption` `muted`).
fn separator(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("t-caption");
    label.add_css_class("muted");
    label.add_css_class("key-separator");
    label
}

/// A row's keys: each binding's chords ("or" apart), the bindings "/"
/// apart, then "default P" (S20, S22).
pub fn row_keys(row: &CheatRow, size: CapSize) -> gtk::Box {
    let keys = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    keys.add_css_class("cheat-keys");
    keys.set_valign(gtk::Align::Center);
    for (n, part) in row.keys.iter().enumerate() {
        if n > 0 {
            keys.append(&separator("/"));
        }
        for (k, chord) in part.chords.iter().enumerate() {
            if k > 0 {
                keys.append(&separator("or"));
            }
            keys.append(&keycap::chord(chord, size, CapVariant::Plain));
        }
    }
    if let Some(note) = &row.default_keys {
        let label = gtk::Label::new(Some(note));
        label.add_css_class("t-caption");
        label.add_css_class("muted");
        label.add_css_class("default-note");
        keys.append(&label);
    }
    keys
}

/// The cheat sheet (opened in `Layer::Fullscreen`).
pub struct CheatSheet {
    root: gtk::Box,
}

impl CheatSheet {
    pub fn new(ctx: &Ctx) -> Self {
        let keymap = ctx.store.keymap();
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Dialog)
            .build();
        root.set_widget_name("cheatsheet");
        root.add_css_class("qrec-cheatsheet");
        // S20: the sheet at (40, 40, 1520, 920) of 1600×1000.
        root.set_margin_top(40);
        root.set_margin_bottom(40);
        root.set_margin_start(40);
        root.set_margin_end(40);
        root.set_overflow(gtk::Overflow::Hidden);
        root.update_property(&[gtk::accessible::Property::Label("Keyboard shortcuts")]);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("cheat-header");
        let title = gtk::Label::new(Some("Keyboard shortcuts"));
        title.add_css_class("t-headline");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        header.append(&title);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        actions.add_css_class("cheat-actions");
        actions.set_valign(gtk::Align::Center);
        let (field, text) = crate::palette::search_field(
            "cheatsheet-search",
            "Search shortcuts",
            "Search shortcuts",
            false,
        );
        field.set_size_request(320, -1);
        field.set_hexpand(false);
        actions.append(&field);
        let close = button::labeled(
            "Close",
            Some("qrec-x-symbolic"),
            Some("cancel"),
            &keymap,
            button::Kind::Flat,
        );
        close.set_widget_name("cheatsheet-close");
        close.set_valign(gtk::Align::Center);
        actions.append(&close);
        header.append(&actions);
        root.append(&header);

        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.set_widget_name("cheatsheet-body");
        body.add_css_class("cheat-body");
        body.set_homogeneous(true);
        let columns: Vec<gtk::Box> = (0..3)
            .map(|n| {
                let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
                column.set_widget_name(&format!("cheatsheet-column-{}", n + 1));
                column.add_css_class("cheat-column");
                body.append(&column);
                column
            })
            .collect();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&body)
            .build();
        scroller.set_can_focus(false);
        root.append(&scroller);

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        footer.add_css_class("cheat-footer");
        let note = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        note.set_hexpand(true);
        note.set_valign(gtk::Align::Center);
        footer.append(&note);
        let settings = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        settings.add_css_class("cheat-settings-hint");
        settings.set_valign(gtk::Align::Center);
        footer.append(&settings);
        root.append(&footer);

        let sheet = Rc::new(Sheet {
            ctx: ctx.clone(),
            root: root.clone(),
            text: text.clone(),
            columns,
            note,
            settings,
            open: Cell::new(false),
            copied: guard::Slot::new(),
            snippet_button: RefCell::new(None),
        });
        sheet.connect(&text, &close);

        let store = ctx.store.clone();
        ctx.actions.register(
            "keys.help",
            Rc::new(move || {
                store.update_local(|l| {
                    if !l.overlays.contains(&Overlay::CheatSheet) {
                        l.overlays.push(Overlay::CheatSheet);
                    }
                });
            }),
        );
        let follower = Rc::clone(&sheet);
        ctx.store
            .subscribe_local(move |local| follower.follow(local))
            .detach();
        let follower = Rc::downgrade(&sheet);
        button::follow_keymap(&root, &keymap, move |_, _| {
            if let Some(sheet) = follower.upgrade()
                && sheet.open.get()
            {
                sheet.refresh();
            }
        });
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

/// The sheet's widgets.
struct Sheet {
    ctx: Ctx,
    root: gtk::Box,
    text: gtk::Text,
    columns: Vec<gtk::Box>,
    note: gtk::Box,
    settings: gtk::Box,
    open: Cell<bool>,
    /// "Copied" goes back to "Copy niri snippet".
    copied: guard::Slot,
    snippet_button: RefCell<Option<gtk::Button>>,
}

impl Sheet {
    fn follow(self: &Rc<Self>, local: &LocalUi) {
        let wanted = local.overlays.contains(&Overlay::CheatSheet);
        if wanted == self.open.get() {
            return;
        }
        self.open.set(wanted);
        if !wanted {
            self.ctx.window.close(Layer::Fullscreen);
            return;
        }
        self.text.set_text("");
        self.refresh();
        self.ctx.window.open(Layer::Fullscreen, &self.root);
        if let Some(scrim) = self.root.prev_sibling() {
            scrim.set_widget_name("scrim-cheatsheet");
        }
        self.text.grab_focus();
    }

    /// The sheet for the search as it is.
    fn refresh(self: &Rc<Self>) {
        let keymap = self.ctx.store.keymap();
        let keys_path = crate::settings::user_files().keys_shown;
        let view = cheat_sheet(&keymap, &keys_path, &self.text.text());
        self.build(&view);
    }

    fn build(self: &Rc<Self>, view: &View) {
        for column in &self.columns {
            while let Some(child) = column.first_child() {
                column.remove(&child);
            }
        }
        *self.snippet_button.borrow_mut() = None;
        if let Some(first) = self.columns.first() {
            if view.matches.is_some() {
                first.add_css_class("results");
            } else {
                first.remove_css_class("results");
            }
        }
        if let (Some(matches), Some(first)) = (&view.matches, self.columns.first()) {
            let count = gtk::Label::new(Some(matches));
            count.set_widget_name("cheatsheet-count");
            count.add_css_class("t-caption");
            count.add_css_class("muted");
            count.add_css_class("cheat-count");
            count.set_xalign(0.0);
            first.append(&count);
        }
        for (column, groups) in self.columns.iter().zip(&view.columns) {
            for group in groups {
                column.append(&self.group(group));
            }
        }
        if let (Some(tip), Some(first)) = (&view.palette_tip, self.columns.first()) {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.set_widget_name("cheatsheet-palette-tip");
            row.add_css_class("cheat-tip");
            let label = gtk::Label::new(Some(&tip.label));
            label.add_css_class("t-caption");
            label.add_css_class("muted");
            row.append(&label);
            if !tip.keys.is_empty() {
                row.append(&keycap::chord(&tip.keys, CapSize::Md, CapVariant::Plain));
            }
            first.append(&row);
        }
        // The footer: the note with its mono path, the Settings hint.
        while let Some(child) = self.note.first_child() {
            self.note.remove(&child);
        }
        let (start, end) = view
            .footer
            .split_once(&view.footer_path)
            .unwrap_or((view.footer.as_str(), ""));
        for (text, class) in [
            (start, "t-caption"),
            (view.footer_path.as_str(), "t-mono-sm"),
            (end, "t-caption"),
        ] {
            let label = gtk::Label::new(Some(text));
            label.add_css_class(class);
            label.add_css_class("muted");
            label.set_valign(gtk::Align::BaselineCenter);
            self.note.append(&label);
        }
        while let Some(child) = self.settings.first_child() {
            self.settings.remove(&child);
        }
        if !view.settings.keys.is_empty() {
            self.settings.append(&keycap::chord(
                &view.settings.keys,
                CapSize::Md,
                CapVariant::Plain,
            ));
        }
        let label = gtk::Label::new(Some(&view.settings.label));
        label.add_css_class("t-body-sm");
        label.add_css_class("muted");
        self.settings.append(&label);
    }

    /// A group: its title, its rows, Global (niri)'s note and button.
    fn group(self: &Rc<Self>, group: &CheatGroup) -> gtk::Box {
        let block = gtk::Box::new(gtk::Orientation::Vertical, 0);
        block.add_css_class("cheat-group");
        block.set_widget_name(&format!(
            "cheatsheet-group-{}",
            group.title.to_lowercase().replace([' ', '(', ')'], "")
        ));
        let title = gtk::Label::new(Some(&group.title));
        title.add_css_class("t-heading");
        title.add_css_class("cheat-group-title");
        title.set_xalign(0.0);
        block.append(&title);
        let last = group.rows.len().saturating_sub(1);
        for (n, row) in group.rows.iter().enumerate() {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            line.add_css_class("cheat-row");
            if n < last {
                line.add_css_class("ruled");
            }
            line.set_widget_name(&format!("cheatsheet-row-{}", row.id));
            let label = gtk::Label::new(None);
            label.set_markup(&matched_markup(&row.label, &row.matched));
            label.add_css_class("t-body");
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            line.append(&label);
            line.append(&row_keys(row, CapSize::Lg));
            block.append(&line);
        }
        if let Some(snippet) = &group.snippet {
            let footer = gtk::Box::new(gtk::Orientation::Vertical, 0);
            footer.add_css_class("cheat-niri");
            let note = gtk::Label::new(Some(&snippet.note));
            note.add_css_class("t-caption");
            note.add_css_class("muted");
            note.set_xalign(0.0);
            note.set_wrap(true);
            footer.append(&note);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            line.add_css_class("cheat-snippet");
            let copy = button::labeled(
                &snippet.button,
                Some("qrec-copy-symbolic"),
                None,
                &self.ctx.store.keymap(),
                button::Kind::Secondary,
            );
            copy.set_widget_name("cheatsheet-copy-niri");
            // A button the keyboard can reach inside the sheet's trap.
            copy.set_focus_on_click(true);
            let (sheet, text) = (Rc::downgrade(self), snippet.text.clone());
            copy.connect_clicked(guarded!("cheat sheet: copy niri snippet", move |b| {
                if let Some(sheet) = sheet.upgrade() {
                    sheet.copy(b, &text);
                }
            }));
            line.append(&copy);
            let target = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            target.add_css_class("cheat-paste");
            target.set_valign(gtk::Align::Center);
            let into = gtk::Label::new(Some(&snippet.paste_into));
            into.add_css_class("t-caption");
            into.add_css_class("muted");
            into.set_valign(gtk::Align::BaselineCenter);
            let path = gtk::Label::new(Some(&snippet.path));
            path.add_css_class("t-mono-sm");
            path.add_css_class("muted");
            path.set_valign(gtk::Align::BaselineCenter);
            target.append(&into);
            target.append(&path);
            line.append(&target);
            footer.append(&line);
            block.append(&footer);
            *self.snippet_button.borrow_mut() = Some(copy);
        }
        block
    }

    /// Copies the snippet; the button says "Copied" for a while.
    fn copy(&self, button: &gtk::Button, text: &str) {
        let window = self.root.root().and_downcast::<gtk::Window>();
        crate::desktop::run(
            &self.ctx,
            window.as_ref(),
            self.ctx.store.project_dir(),
            &UiAction::CopyText(text.to_owned()),
        );
        set_button_label(button, "Copied");
        let button = button.downgrade();
        self.copied
            .timeout_once(COPIED_FOR, "cheat sheet: copied", move || {
                if let Some(button) = button.upgrade() {
                    set_button_label(&button, "Copy niri snippet");
                }
            });
    }

    fn close(&self) {
        self.ctx.store.update_local(|l| {
            l.close_overlays(|o| *o == Overlay::CheatSheet);
        });
    }

    fn connect(self: &Rc<Self>, text: &gtk::Text, close: &gtk::Button) {
        let sheet = Rc::downgrade(self);
        text.connect_changed(guarded!("cheat sheet: the search", move |_| {
            if let Some(sheet) = sheet.upgrade()
                && sheet.open.get()
            {
                sheet.refresh();
            }
        }));
        let sheet = Rc::downgrade(self);
        close.connect_clicked(guarded!("cheat sheet: close", move |_| {
            if let Some(sheet) = sheet.upgrade() {
                sheet.close();
            }
        }));
        let keys = gtk::EventControllerKey::new();
        let sheet = Rc::downgrade(self);
        keys.connect_key_pressed(guarded!(
            "cheat sheet: keys",
            after_panic = glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                let Some(sheet) = sheet.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if key == gtk::gdk::Key::Escape {
                    sheet.close();
                    return glib::Propagation::Stop;
                }
                // The footer's Ctrl+, : Settings › Keyboard.
                if key == gtk::gdk::Key::comma
                    && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                {
                    sheet.ctx.store.update_local(|l| {
                        l.close_overlays(|o| *o == Overlay::CheatSheet);
                        if !l.overlays.iter().any(|o| matches!(o, Overlay::Settings(_))) {
                            l.overlays.push(Overlay::Settings(SettingsPage::Keyboard));
                        }
                    });
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        ));
        text.add_controller(keys);
    }
}

/// Sets a `button::labeled` button's label.
fn set_button_label(button: &gtk::Button, text: &str) {
    let Some(content) = button.child() else {
        return;
    };
    let mut child = content.first_child();
    while let Some(c) = child {
        if let Some(label) = c.downcast_ref::<gtk::Label>() {
            label.set_text(text);
            button.update_property(&[gtk::accessible::Property::Label(text)]);
            return;
        }
        child = c.next_sibling();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_are_marked_up() {
        assert_eq!(
            matched_markup("Nudge trim edge", &[6, 7, 8, 9]),
            "Nudge <span weight=\"600\">trim</span> edge"
        );
        assert_eq!(matched_markup("a & b", &[]), "a &amp; b");
    }
}
