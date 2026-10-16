//! C01 buttons (G§5.1): `gtk::Button` with the classes `.qrec-btn` plus
//! `.primary`, `.secondary`, `.flat`, `.destructive`, `.destructive-flat`,
//! `.icon-32`, `.icon-24`, `.sm`, `.lg` (common.css). Chrome buttons are
//! `focus-on-click = false` (D-11), so a click never makes `Space` or
//! `Enter` activate a button. A button with a key shows its key cap and
//! names the key in its accessible label ("Settings, Ctrl+Comma"; every
//! icon-only button needs one, G§5.1). A button with a binding runs it
//! through `KeyRouter::run_binding` (G§3.7), so the key's pre-steps apply.

use gtk::prelude::*;
use qrec_core::keymap::{Keymap, usable_keys};

use super::keycap::{self, CapSize, CapVariant};

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
/// `binding` (looked up in `keymap`).
pub fn labeled(
    label: &str,
    icon: Option<&str>,
    binding: Option<&str>,
    keymap: &Keymap,
    kind: Kind,
) -> gtk::Button {
    let button = base(kind);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    if let Some(icon) = icon {
        content.append(&gtk::Image::from_icon_name(icon));
    }
    content.append(&gtk::Label::new(Some(label)));
    let caps = binding.and_then(|b| binding_caps(keymap, b));
    if let Some(caps) = &caps {
        let variant = if matches!(kind, Kind::Primary | Kind::Destructive) {
            CapVariant::OnFill
        } else {
            CapVariant::Plain
        };
        content.append(&keycap::chord(caps, CapSize::Sm, variant));
    }
    button.set_child(Some(&content));
    button.update_property(&[gtk::accessible::Property::Label(&accessible_name(
        label,
        caps.as_deref(),
    ))]);
    button
}

/// An icon-only button (`.icon-32`): its tooltip and accessible name say
/// the action and the key of `binding`.
pub fn icon(icon: &str, action: &str, binding: Option<&str>, keymap: &Keymap) -> gtk::Button {
    let button = base(Kind::Flat);
    button.add_css_class("icon-32");
    button.set_icon_name(icon);
    let caps = binding.and_then(|b| binding_caps(keymap, b));
    button.update_property(&[gtk::accessible::Property::Label(&accessible_name(
        action,
        caps.as_deref(),
    ))]);
    match &caps {
        Some(caps) => super::tooltip::keys(&button, action, std::slice::from_ref(caps)),
        None => super::tooltip::text(&button, action),
    }
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

/// The header's close button (32×32): it asks its window to close, which
/// runs the window's close sequence (D-12).
pub fn close_window(keymap: &Keymap) -> gtk::Button {
    let close = icon("window-close-symbolic", "Close", Some("app.quit"), keymap);
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
}
