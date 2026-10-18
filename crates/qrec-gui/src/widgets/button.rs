//! C01 buttons (G§5.1): `gtk::Button` with the classes `.qrec-btn` plus
//! `.primary`, `.secondary`, `.flat`, `.destructive`, `.destructive-flat`,
//! `.icon-32`, `.icon-24`, `.sm`, `.lg` (common.css). Chrome buttons are
//! `focus-on-click = false` (D-11), so a click never makes `Space` or
//! `Enter` activate a button. A button with a key shows its key cap and
//! names the key in its accessible label ("Settings, Ctrl+Comma"; every
//! icon-only button needs one, G§5.1). A button with a binding runs it
//! through `KeyRouter::run_binding` (G§3.7), so the key's pre-steps apply.
//! A menu's button (C25) is [`menu_button`], which names its inner button
//! for screen readers too.
//!
//! The keys shown follow the keymap live (WP10): a control built from a
//! binding shows its keys through [`follow_keymap`], which shows them
//! again for a new keymap ([`keymap_changed`], `keys.toml` reloaded) as
//! long as the control lives: the cap ([`show_binding_cap`]), the
//! tooltip's caps and the accessible name.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use qrec_core::keymap::{Keymap, usable_keys};

use super::keycap::{self, CapSize, CapVariant};

/// How a control shows a keymap's keys.
type ShowKeys = Rc<dyn Fn(&gtk::Widget, &Keymap)>;

thread_local! {
    /// The controls that show a binding's keys, each with how it shows them
    /// (main thread only; a gone control's entry is dropped).
    static KEYED: RefCell<Vec<(glib::WeakRef<gtk::Widget>, ShowKeys)>> =
        const { RefCell::new(Vec::new()) };
}

/// Shows `keymap`'s keys on `widget` with `show` now, and the keys of each
/// new keymap ([`keymap_changed`]) while `widget` lives. `show` gets the
/// widget (it holds no strong reference to it, G§3.12 R11).
pub fn follow_keymap<W: IsA<gtk::Widget>>(
    widget: &W,
    keymap: &Keymap,
    show: impl Fn(&W, &Keymap) + 'static,
) {
    show(widget, keymap);
    let show: ShowKeys = Rc::new(move |widget, keymap| {
        if let Some(widget) = widget.downcast_ref::<W>() {
            show(widget, keymap);
        }
    });
    KEYED.with(|keyed| {
        let mut keyed = keyed.borrow_mut();
        keyed.retain(|(w, _)| w.upgrade().is_some());
        keyed.push((widget.upcast_ref::<gtk::Widget>().downgrade(), show));
    });
}

/// The keymap changed (`Store::set_keymap`): every control that follows it
/// shows its keys again.
pub fn keymap_changed(keymap: &Keymap) {
    // Out of the list first: a control's `show` may build widgets that
    // follow the keymap too.
    let live: Vec<(gtk::Widget, ShowKeys)> = KEYED.with(|keyed| {
        let mut keyed = keyed.borrow_mut();
        keyed.retain(|(w, _)| w.upgrade().is_some());
        keyed
            .iter()
            .filter_map(|(w, show)| Some((w.upgrade()?, Rc::clone(show))))
            .collect()
    });
    for (widget, show) in live {
        show(&widget, keymap);
    }
}

/// How many live controls follow the keymap.
pub fn keyed() -> usize {
    KEYED.with(|keyed| {
        keyed
            .borrow()
            .iter()
            .filter(|(w, _)| w.upgrade().is_some())
            .count()
    })
}

/// The name of the cap a control shows for its binding (in its content
/// box, last).
pub const BINDING_CAP: &str = "binding-cap";

/// Shows `caps` as `content`'s last child (the chord, vertically centered,
/// named `name`), in place of the one it showed (named `name` too); none
/// for `None`.
pub fn show_binding_cap(
    content: &gtk::Box,
    caps: Option<&[String]>,
    name: &str,
    size: CapSize,
    variant: CapVariant,
) {
    let mut child = content.first_child();
    while let Some(c) = child {
        child = c.next_sibling();
        if c.widget_name() == name {
            content.remove(&c);
        }
    }
    if let Some(caps) = caps {
        let cap = keycap::chord(caps, size, variant);
        cap.set_valign(gtk::Align::Center);
        cap.set_widget_name(name);
        content.append(&cap);
    }
}

/// The C01 kinds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Kind {
    Primary,
    #[default]
    Secondary,
    Flat,
    Destructive,
    DestructiveFlat,
}

impl Kind {
    pub fn class(self) -> &'static str {
        match self {
            Kind::Primary => "primary",
            Kind::Secondary => "secondary",
            Kind::Flat => "flat",
            Kind::Destructive => "destructive",
            Kind::DestructiveFlat => "destructive-flat",
        }
    }
}

/// The cap legends of `binding`'s first key that runs (rebound keys show;
/// conflicting ones drop out); `None` when it has none.
pub fn binding_caps(keymap: &Keymap, binding: &str) -> Option<Vec<String>> {
    let b = keymap.binding(binding)?;
    usable_keys(keymap, b).first().map(|k| k.cap_legends())
}

/// How a key reads in an accessible name: "Ctrl+K", "Ctrl+Comma", "R".
pub fn spoken_keys(caps: &[String]) -> String {
    caps.iter()
        .map(|cap| match cap.as_str() {
            "," => "Comma".to_owned(),
            "." => "Period".to_owned(),
            "/" => "Slash".to_owned(),
            "\u{2212}" | "-" => "Minus".to_owned(),
            "=" => "Equal".to_owned(),
            "\\" => "Backslash".to_owned(),
            "[" => "Left bracket".to_owned(),
            "]" => "Right bracket".to_owned(),
            "?" => "Question mark".to_owned(),
            ":" => "Colon".to_owned(),
            "<" => "Less than".to_owned(),
            ">" => "Greater than".to_owned(),
            "←" => "Left".to_owned(),
            "→" => "Right".to_owned(),
            "↑" => "Up".to_owned(),
            "↓" => "Down".to_owned(),
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// The accessible name of a control: its action, then its key.
pub fn accessible_name(action: &str, caps: Option<&[String]>) -> String {
    match caps {
        Some(caps) if !caps.is_empty() => format!("{action}, {}", spoken_keys(caps)),
        _ => action.to_owned(),
    }
}

fn base(kind: Kind) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("qrec-btn");
    button.add_css_class(kind.class());
    button.set_focus_on_click(false);
    button
}

/// A labeled button: optional icon, the label, and the key cap of
/// `binding` (looked up in `keymap`, and in each new one: [`follow_keymap`]),
/// vertically centered. Its content box is 8 px apart by CSS
/// (`button.qrec-btn > box`'s `border-spacing`: GtkBox adds it to its own
/// spacing, which stays 0, and only CSS px follow the UI scale); a button
/// that builds its own content does the same.
pub fn labeled(
    label: &str,
    icon: Option<&str>,
    binding: Option<&str>,
    keymap: &Keymap,
    kind: Kind,
) -> gtk::Button {
    let button = base(kind);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    if let Some(icon) = icon {
        content.append(&gtk::Image::from_icon_name(icon));
    }
    content.append(&gtk::Label::new(Some(label)));
    button.set_child(Some(&content));
    let Some(binding) = binding else {
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        return button;
    };
    let variant = if matches!(kind, Kind::Primary | Kind::Destructive) {
        CapVariant::OnFill
    } else {
        CapVariant::Plain
    };
    let (label, binding) = (label.to_owned(), binding.to_owned());
    follow_keymap(&button, keymap, move |button, keymap| {
        let caps = binding_caps(keymap, &binding);
        if let Some(content) = button.child().and_downcast::<gtk::Box>() {
            // A button's cap is a medium one (DESIGN Key-cap chip; S02's
            // toolbar, S12's footer).
            show_binding_cap(&content, caps.as_deref(), BINDING_CAP, CapSize::Md, variant);
        }
        button.update_property(&[gtk::accessible::Property::Label(&accessible_name(
            &label,
            caps.as_deref(),
        ))]);
    });
    button
}

/// An icon-only button (`.icon-32`): its tooltip and accessible name say
/// the action and the key of `binding` (and of each new keymap's:
/// [`follow_keymap`]).
pub fn icon(icon: &str, action: &str, binding: Option<&str>, keymap: &Keymap) -> gtk::Button {
    let button = base(Kind::Flat);
    button.add_css_class("icon-32");
    button.set_icon_name(icon);
    let Some(binding) = binding else {
        button.update_property(&[gtk::accessible::Property::Label(action)]);
        super::tooltip::text(&button, action);
        return button;
    };
    let tip = super::tooltip::keys(&button, action, &[]);
    let (action, binding) = (action.to_owned(), binding.to_owned());
    follow_keymap(&button, keymap, move |button, keymap| {
        let caps = binding_caps(keymap, &binding);
        button.update_property(&[gtk::accessible::Property::Label(&accessible_name(
            &action,
            caps.as_deref(),
        ))]);
        // Without a key, GTK's own text tooltip.
        tip.set(caps.as_slice());
        button.set_tooltip_text(caps.is_none().then_some(action.as_str()));
        button.set_has_tooltip(true);
    });
    button
}

/// A link button (`.qrec-link`: "Open log", "Open full log"; G§1.3: a real
/// underline). The underline is Pango's `low` one, drawn under the text's
/// descenders as the frames draw it (CSS `text-decoration` puts it at the
/// font's underline position, 3–4 px higher, through the p and the g: WP7
/// integration audit). Its size and color are the owner's CSS.
pub fn link(text: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("qrec-link");
    let label = gtk::Label::new(Some(text));
    let attrs = gtk::pango::AttrList::new();
    attrs.insert(gtk::pango::AttrInt::new_underline(
        gtk::pango::Underline::Low,
    ));
    label.set_attributes(Some(&attrs));
    button.set_child(Some(&label));
    button.update_property(&[gtk::accessible::Property::Label(text)]);
    button
}

/// C01's loading state: a 16 px spinner where the icon was (before the
/// label when there is none), the label kept; `false` puts the icon back.
/// Whether the button reacts meanwhile is the caller's (`set_sensitive`).
pub fn set_loading(button: &gtk::Button, loading: bool) {
    let Some(content) = button.child().and_downcast::<gtk::Box>() else {
        return;
    };
    let mut spinner = None;
    let mut image = None;
    let mut child = content.first_child();
    while let Some(c) = child {
        if c.widget_name() == "loading-spinner" {
            spinner = Some(c.clone());
        } else if c.is::<gtk::Image>() && image.is_none() {
            image = Some(c.clone());
        }
        child = c.next_sibling();
    }
    if loading {
        button.add_css_class("loading");
        if let Some(image) = &image {
            image.set_visible(false);
        }
        if spinner.is_none() {
            let spinner = super::Spinner::new(16);
            spinner.set_widget_name("loading-spinner");
            content.prepend(&spinner);
        }
    } else {
        button.remove_css_class("loading");
        if let Some(image) = &image {
            image.set_visible(true);
        }
        if let Some(spinner) = spinner {
            content.remove(&spinner);
        }
    }
}

/// A C25 menu's button: a `gtk::MenuButton` over `model`, its inner toggle
/// styled as C01 (`qrec-btn` plus `classes`, `focus-on-click = false`) and
/// both named `label` for screen readers ([`set_menu_label`]). Every
/// MenuButton is made here: GTK keeps an accessible label on the
/// MenuButton, and the a11y walk (G§3.18) sees its inner button too.
pub fn menu_button(
    model: &impl IsA<gtk::gio::MenuModel>,
    classes: &[&str],
    label: &str,
) -> gtk::MenuButton {
    let button = gtk::MenuButton::new();
    button.set_menu_model(Some(model));
    button.add_css_class("qrec-menu-button");
    if let Some(inner) = menu_inner_button(&button) {
        inner.add_css_class("qrec-btn");
        for class in classes {
            inner.add_css_class(class);
        }
        inner.set_focus_on_click(false);
    }
    set_menu_label(&button, label);
    button
}

/// Names a menu button and its inner button for screen readers (a row's
/// overflow says whose it is: "More for layer Face").
pub fn set_menu_label(button: &gtk::MenuButton, label: &str) {
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    if let Some(inner) = menu_inner_button(button) {
        inner.update_property(&[gtk::accessible::Property::Label(label)]);
    }
}

/// The toggle button a `gtk::MenuButton` draws.
fn menu_inner_button(button: &gtk::MenuButton) -> Option<gtk::Widget> {
    let mut child = button.first_child();
    while let Some(widget) = child {
        if widget.is::<gtk::Button>() {
            return Some(widget);
        }
        child = widget.next_sibling();
    }
    None
}

/// The header's close button (32×32): it asks its window to close, which
/// runs the window's close sequence (D-12).
pub fn close_window(keymap: &Keymap) -> gtk::Button {
    let close = icon("qrec-x-symbolic", "Close", Some("app.quit"), keymap);
    close.set_widget_name("close-button");
    close.connect_clicked(crate::guarded!("close button", move |button| {
        if let Some(window) = button.root().and_downcast::<gtk::Window>() {
            window.close();
        }
    }));
    close
}

#[cfg(test)]
mod tests {
    use qrec_core::keymap;

    use super::*;

    #[test]
    fn keys_in_accessible_names() {
        let map = keymap::resolve("");
        assert_eq!(
            binding_caps(&map, "settings.open"),
            Some(vec!["Ctrl".into(), ",".into()])
        );
        assert_eq!(binding_caps(&map, "record.toggle"), Some(vec!["R".into()]));
        assert_eq!(binding_caps(&map, "nope"), None);
        let caps = binding_caps(&map, "settings.open").unwrap();
        assert_eq!(
            accessible_name("Settings", Some(&caps)),
            "Settings, Ctrl+Comma"
        );
        assert_eq!(accessible_name("Commands", None), "Commands");
        let rebound = keymap::resolve("[bindings]\n\"record.toggle\" = \"F9\"\n");
        assert_eq!(
            binding_caps(&rebound, "record.toggle"),
            Some(vec!["F9".into()])
        );
        assert_eq!(spoken_keys(&["Shift".into(), "←".into()]), "Shift+Left");
    }

    /// Every `gtk::MenuButton` comes from [`menu_button`], which names its
    /// inner button too: one made elsewhere leaves that button unnamed for
    /// screen readers (and the a11y walk).
    #[test]
    fn every_menu_button_is_made_by_the_helper() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path.ends_with("widgets/button.rs")
                {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (i, line) in text.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or_default();
                    if code.contains("MenuButton::new(") || code.contains("MenuButton::builder(") {
                        offenders.push(format!("{}:{}", path.display(), i + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "use button::menu_button: {offenders:#?}"
        );
    }
}
