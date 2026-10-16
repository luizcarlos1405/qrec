//! The Settings dialog (S18b, G§4.10): its shell with five tabs as drawn
//! (only Project enabled until WP9, D-48), 840×640, centered, over a scrim
//! that ends above the hint bar. Owner: WP6 (shell, Project), WP9 (the
//! other pages). Registers `settings.open`.
//!
//! The shell is `raised`, `xl` radius, e3 with a 1 px `border`: a 56 px
//! header ("Settings" in `headline`, the tab switcher centered, "× Close
//! [Esc]" at the right) over the page. The tabs are a segmented control
//! (C28, the anatomy of `adw::InlineViewSwitcher`, whose pages can't be
//! disabled): Appearance, Recording, Autocut and Keyboard show disabled
//! with the tooltip "Not built yet" until WP9 builds them. It follows
//! `LocalUi`: open while an `Overlay::Settings` is in the stack (a modal
//! may stack over it: the S18b fps confirmation), on that overlay's page.
//! `Esc` (the key router pops the top overlay) and Close close it.

pub mod appearance;
pub mod autocut;
pub mod keyboard;
pub mod project;
pub mod recording;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use qrec_core::view::notices::NOT_BUILT;
use qrec_core::view::settings::{SettingsPage, tabs};

use crate::store::local::{LocalUi, Overlay};
use crate::widgets::button;
use crate::window::layers::Layer;
use crate::{Ctx, guarded};
use project::ProjectPage;

/// The Settings dialog (opened in `Layer::Settings`).
pub struct Settings {
    root: gtk::Box,
}

impl Settings {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Dialog)
            .build();
        root.set_widget_name("settings");
        root.add_css_class("qrec-settings");
        root.set_size_request(840, 640);
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        // Centered on the window (BRIEF §2.8: at (380, 180) in 1600×1000),
        // above the 32 px hint bar the layer stops at.
        root.set_margin_top(32);
        root.update_property(&[gtk::accessible::Property::Label("Settings")]);
        let keymap = ctx.store.keymap();

        let header = gtk::CenterBox::new();
        header.add_css_class("settings-header");
        let title = gtk::Label::new(Some("Settings"));
        title.add_css_class("t-headline");
        title.add_css_class("settings-title");
        header.set_start_widget(Some(&title));
        let switcher = adw::ToggleGroup::new();
        switcher.set_widget_name("settings-tabs");
        switcher.add_css_class("qrec-segmented");
        switcher.add_css_class("settings-tabs");
        switcher.set_valign(gtk::Align::Center);
        switcher
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label("Settings pages")]);
        let facts = ctx.store.local().facts();
        for tab in tabs(&facts) {
            let toggle = adw::Toggle::builder()
                .name(tab.page.name())
                .label(tab.label.as_str())
                .enabled(tab.enabled)
                .build();
            if !tab.enabled {
                toggle.set_tooltip(NOT_BUILT);
            }
            switcher.add(toggle);
        }
        header.set_center_widget(Some(&switcher));
        let close = button::labeled(
            "Close",
            Some("window-close-symbolic"),
            Some("cancel"),
            &keymap,
            button::Kind::Flat,
        );
        close.set_widget_name("settings-close");
        close.set_valign(gtk::Align::Center);
        // Its key cap keeps its own height in the 32 px button.
        let mut part = close.child().and_then(|content| content.first_child());
        while let Some(widget) = part {
            widget.set_valign(gtk::Align::Center);
            part = widget.next_sibling();
        }
        header.set_end_widget(Some(&close));
        root.append(&header);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        let project = ProjectPage::new(ctx);
        for page in SettingsPage::ALL {
            if page == SettingsPage::Project {
                stack.add_named(&project.widget(), Some(page.name()));
            } else {
                // WP9's pages (S21, S22; Recording and Autocut once drawn).
                stack.add_named(
                    &gtk::Box::new(gtk::Orientation::Vertical, 0),
                    Some(page.name()),
                );
            }
        }
        stack.set_visible_child_name(SettingsPage::Project.name());
        root.append(&stack);

        let store = ctx.store.clone();
        close.connect_clicked(guarded!("settings: close", move |_| {
            store.update_local(|l| {
                l.close_overlays(|o| matches!(o, Overlay::Settings(_)));
            });
        }));
        let syncing = Rc::new(Cell::new(false));
        let (store, quiet) = (ctx.store.clone(), Rc::clone(&syncing));
        switcher.connect_active_name_notify(guarded!("settings: a tab", move |group| {
            if quiet.get() {
                return;
            }
            let Some(page) = group
                .active_name()
                .and_then(|n| SettingsPage::from_name(&n))
            else {
                return;
            };
            store.update_local(move |l| {
                for overlay in &mut l.overlays {
                    if let Overlay::Settings(shown) = overlay {
                        *shown = page;
                    }
                }
            });
        }));

        // `settings.open` (Ctrl+, and the header's button): opens it on
        // Project, or leaves it as it is when it is open.
        let store = ctx.store.clone();
        ctx.actions.register(
            "settings.open",
            Rc::new(move || {
                store.update_local(|l| {
                    if !l.overlays.iter().any(|o| matches!(o, Overlay::Settings(_))) {
                        l.overlays.push(Overlay::Settings(SettingsPage::Project));
                    }
                });
            }),
        );

        let dialog = Rc::new(Dialog {
            ctx: ctx.clone(),
            root: root.clone(),
            switcher,
            stack,
            project,
            syncing,
            shown: RefCell::new(None),
        });
        let follower = Rc::clone(&dialog);
        ctx.store
            .subscribe_local(move |local| follower.follow(local))
            .detach();
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

/// The dialog and the page it shows.
struct Dialog {
    ctx: Ctx,
    root: gtk::Box,
    switcher: adw::ToggleGroup,
    stack: gtk::Stack,
    project: ProjectPage,
    syncing: Rc<Cell<bool>>,
    /// The page shown while it is open.
    shown: RefCell<Option<SettingsPage>>,
}

impl Dialog {
    /// Opens, turns to its page or closes with the overlays.
    fn follow(&self, local: &LocalUi) {
        let wanted = local.overlays.iter().rev().find_map(|o| match o {
            Overlay::Settings(page) => Some(*page),
            _ => None,
        });
        let was = *self.shown.borrow();
        if was == wanted {
            return;
        }
        *self.shown.borrow_mut() = wanted;
        let Some(page) = wanted else {
            self.ctx.window.close(Layer::Settings);
            return;
        };
        self.syncing.set(true);
        self.switcher.set_active_name(Some(page.name()));
        self.syncing.set(false);
        self.stack.set_visible_child_name(page.name());
        if was.is_none() {
            // Before it shows: the page drops what it was left with.
            self.project.refresh();
            self.ctx.window.open(Layer::Settings, &self.root);
            // The scrim under it, for the region checks (G§8.4).
            if let Some(scrim) = self.root.prev_sibling() {
                scrim.set_widget_name("scrim-settings");
            }
            if page == SettingsPage::Project {
                self.project.focus_target().grab_focus();
            }
        }
    }
}
