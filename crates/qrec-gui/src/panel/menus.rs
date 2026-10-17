//! The panel's menus (C25: `gtk::PopoverMenu` over a `gio::Menu`): Add
//! layer and Add audio track (from `LocalUi.sources`, refreshed when they
//! open), the rows' overflows, the banner's `Layout ▾`.
//!
//! Every item that sends a command carries it: the action `panel.run` with
//! the command's JSON as its target, so an item always sends what it showed
//! (the menus are rebuilt when the state changes). An item that can't run
//! points at the disabled action `panel.unavailable` and shows insensitive.
//! Renames are `panel.rename-layer` and `panel.rename-track` (the id as
//! target): they start the inline edit.
//!
//! An open menu is `Overlay::Menu` in `LocalUi` (the key router lets its
//! keys through, G§3.8 step 4): a `MenuButton`'s ([`track_menu`]) and a
//! `DropDown`'s list ([`track_dropdown`]: the autocut page's "Detect
//! silence on", R32).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use qrec_core::Command;
use qrec_core::view::panel::{AddGroup, AddItem};

use crate::Ctx;
use crate::guarded;
use crate::store::local::Overlay;

/// The panel's action group prefix.
pub const PREFIX: &str = "panel";

/// An item that sends `cmd`, or a disabled one (`None`).
pub fn command_item(label: &str, cmd: Option<&Command>) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    match cmd {
        Some(cmd) => {
            let json = cmd.to_json().to_string();
            item.set_action_and_target_value(
                Some(&format!("{PREFIX}.run")),
                Some(&json.to_variant()),
            );
        }
        None => item.set_action_and_target_value(Some(&format!("{PREFIX}.unavailable")), None),
    }
    item
}

/// An item that runs UI action `action` (`rename-layer`) on `id`.
pub fn ui_item(label: &str, action: &str, id: &str, enabled: bool) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    if enabled {
        item.set_action_and_target_value(
            Some(&format!("{PREFIX}.{action}")),
            Some(&id.to_variant()),
        );
    } else {
        item.set_action_and_target_value(Some(&format!("{PREFIX}.unavailable")), None);
    }
    item
}

/// A persistent menu whose items are rebuilt only when they change (an
/// open popover follows the model).
#[derive(Clone)]
pub struct Menu {
    model: gio::Menu,
    shown: Rc<RefCell<String>>,
}

impl std::fmt::Debug for Menu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Menu").finish_non_exhaustive()
    }
}

impl Default for Menu {
    fn default() -> Self {
        Self::new()
    }
}

impl Menu {
    pub fn new() -> Self {
        Self {
            model: gio::Menu::new(),
            shown: Rc::default(),
        }
    }

    pub fn model(&self) -> gio::Menu {
        self.model.clone()
    }

    /// Replaces the items with `build`'s when `signature` changed.
    pub fn update(&self, signature: String, build: impl FnOnce(&gio::Menu)) {
        if *self.shown.borrow() == signature {
            return;
        }
        *self.shown.borrow_mut() = signature;
        self.model.remove_all();
        build(&self.model);
    }
}

/// The Add menus: one submenu per group ("Screen ▸", "Camera ▸"; "Mic ▸",
/// "System audio ▸"), its sources inside; a source in use shows disabled.
/// With nothing found: one disabled "No sources found" item.
pub fn fill_add_menu(menu: &gio::Menu, items: &[AddItem], groups: &[AddGroup]) {
    let mut any = false;
    for group in groups {
        let sub = gio::Menu::new();
        for item in items.iter().filter(|i| i.group == *group) {
            let cmd = (!item.in_use).then_some(&item.command);
            sub.append_item(&command_item(&item.label, cmd));
            any = true;
        }
        if sub.n_items() > 0 {
            menu.append_submenu(Some(group.label()), &sub);
        }
    }
    if !any {
        menu.append_item(&command_item("No sources found", None));
    }
}

/// The signature of a list of add items (for [`Menu::update`]).
pub fn add_signature(items: &[AddItem]) -> String {
    items
        .iter()
        .map(|i| {
            format!(
                "{:?}|{}|{}|{}",
                i.group,
                i.label,
                i.in_use,
                i.command.to_json()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `gtk::MenuButton` over `menu` (`widgets::button::menu_button`: its
/// inner button styled as C01 with `classes`, both named `label` for
/// screen readers).
pub fn menu_button(menu: &Menu, classes: &[&str], label: &str) -> gtk::MenuButton {
    crate::widgets::button::menu_button(&menu.model(), classes, label)
}

pub use crate::widgets::button::set_menu_label;

/// Wraps `button` (a `gtk::MenuButton`): while its menu is open the
/// window's overlays say `Menu`; `on_open` runs as it opens.
pub fn track_menu(ctx: &Ctx, button: &gtk::MenuButton, on_open: Option<Rc<dyn Fn()>>) {
    button.set_focus_on_click(false);
    let store = ctx.store.clone();
    button.connect_active_notify(guarded!("panel: menu open", move |button| {
        if button.is_active() {
            if let Some(f) = &on_open {
                f();
            }
            store.update_local(|l| l.overlays.push(Overlay::Menu));
        } else {
            store.update_local(|l| {
                if let Some(at) = l.overlays.iter().rposition(|o| *o == Overlay::Menu) {
                    l.overlays.remove(at);
                }
            });
        }
    }));
}

/// While `dropdown`'s list is open the window's overlays say `Menu`, as
/// they do for a `MenuButton`'s menu (G§3.6; R32, R41): the key router's
/// step 4 leaves a menu its keys (GTK 4.22 hands an open list's keys to
/// its popover, and the window's router doesn't see them; the overlay
/// keeps `LocalUi` saying what is open). It follows the list's popover being
/// mapped, so the entry also goes when the dropdown hides under an open
/// list (its page closes). Every dropdown's: the autocut page's track list
/// and Settings › Project's Size.
pub fn track_dropdown(ctx: &Ctx, dropdown: &gtk::DropDown) {
    let mut child = dropdown.first_child();
    while let Some(c) = child {
        if let Ok(popover) = c.clone().downcast::<gtk::Popover>() {
            let store = ctx.store.clone();
            popover.connect_map(guarded!("menus: a list opens", move |_| {
                store.update_local(|l| l.overlays.push(Overlay::Menu));
            }));
            let store = ctx.store.clone();
            popover.connect_unmap(guarded!("menus: a list closes", move |_| {
                store.update_local(|l| {
                    if let Some(at) = l.overlays.iter().rposition(|o| *o == Overlay::Menu) {
                        l.overlays.remove(at);
                    }
                });
            }));
        }
        child = c.next_sibling();
    }
}

/// Discovers the sources again on `qrec-gui-io` and hands them to
/// `LocalUi.sources` (the Add menus open, H-5): through the app's shared
/// cache, so the Session's `LayerAdd` sees the same facts. The result comes
/// back through the queue, in its order (`run_io_then`).
pub fn refresh_sources(ctx: &Ctx) {
    let Some(app) = crate::app::App::current() else {
        return;
    };
    let sources = app.startup.sources().clone();
    let store = ctx.store.clone();
    ctx.media.run_io_then(
        move || sources.refresh(),
        move |found| store.update_local(move |l| l.sources = Some(found)),
    );
}

/// The panel's actions on `widget`: `run` (a command's JSON),
/// `unavailable` (disabled), and the two renames.
pub fn install_actions(
    widget: &impl IsA<gtk::Widget>,
    ctx: &Ctx,
    rename_layer: Rc<dyn Fn(&str)>,
    rename_track: Rc<dyn Fn(&str)>,
) {
    let group = gio::SimpleActionGroup::new();
    let run = gio::SimpleAction::new("run", Some(glib::VariantTy::STRING));
    let store = ctx.store.clone();
    run.connect_activate(guarded!("panel: run", move |_, target| {
        let Some(json) = target.and_then(|t| t.get::<String>()) else {
            return;
        };
        match serde_json::from_str::<Command>(&json) {
            Ok(cmd) => store.dispatch(cmd),
            Err(e) => tracing::warn!("panel: a menu item's command doesn't parse: {e}"),
        }
    }));
    group.add_action(&run);
    let unavailable = gio::SimpleAction::new("unavailable", None);
    unavailable.set_enabled(false);
    group.add_action(&unavailable);
    for (name, f) in [
        ("rename-layer", rename_layer),
        ("rename-track", rename_track),
    ] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        action.connect_activate(guarded!("panel: rename", move |_, target| {
            if let Some(id) = target.and_then(|t| t.get::<String>()) {
                f(&id);
            }
        }));
        group.add_action(&action);
    }
    widget.insert_action_group(PREFIX, Some(&group));
}
