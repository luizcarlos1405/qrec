//! Settings › Project (S18b, G§4.10): the "Canvas" group (Size, Frame
//! rate, Background) and its footer; an fps change with takes opens the
//! S18b confirmation over Settings. Owner: WP6.
//!
//! Each row is 60 px with a hairline under it: the label (`body`) over its
//! caption (`caption` `muted`), the control on the right (260 px dropdown
//! and field, the 220 px segmented rate). The controls show the project as
//! the snapshots have it: a pick sends its `ProjectSet` and the control
//! goes back to the project's value at once, so under the S18b modal the
//! rate still reads 30 (the frame) until the change is confirmed. The
//! Background field commits on Enter or when it loses the focus; an
//! invalid color shows "Use #RRGGBB" under it (C29 invalid); `Esc` puts the
//! project's color back.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use qrec_core::model::Fps;
use qrec_core::view::format;
use qrec_core::view::settings::{
    ProjectPage as View, RATES, background_command, project_page, rate_command, size_command,
};

use crate::store::local::Overlay;
use crate::{Ctx, guarded};

/// The Project page.
pub struct ProjectPage {
    root: gtk::Box,
    size: gtk::DropDown,
}

impl ProjectPage {
    pub fn new(ctx: &Ctx) -> Self {
        let view = page_view(ctx);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("settings-project");
        root.add_css_class("settings-page");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.add_css_class("settings-content");
        content.set_vexpand(true);

        let group = gtk::Box::new(gtk::Orientation::Vertical, 0);
        group.add_css_class("settings-group");
        let heading = gtk::Label::new(Some(&view.group));
        heading.add_css_class("t-heading");
        heading.set_xalign(0.0);
        let caption = caption_label(&view.caption);
        group.append(&heading);
        group.append(&caption);
        content.append(&group);

        // Size.
        let sizes: Vec<&str> = view.size.options.iter().map(|o| o.label.as_str()).collect();
        let size = gtk::DropDown::from_strings(&sizes);
        size.set_widget_name("settings-size");
        size.add_css_class("settings-dropdown");
        size.set_size_request(260, -1);
        size.set_valign(gtk::Align::Center);
        size.set_selected(view.size.selected as u32);
        size.update_property(&[gtk::accessible::Property::Label("Size")]);
        let (size_row, size_caption) = row(&view.size.label, &view.size.caption, &size);
        content.append(&size_row);

        // Frame rate.
        let rate = adw::ToggleGroup::new();
        rate.set_widget_name("settings-rate");
        rate.add_css_class("qrec-segmented");
        rate.add_css_class("rate-switch");
        rate.set_valign(gtk::Align::Center);
        for r in RATES {
            let name = r.to_string();
            rate.add(
                adw::Toggle::builder()
                    .name(name.as_str())
                    .label(name.as_str())
                    .description(format!("{r} fps"))
                    .build(),
            );
        }
        rate.upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label("Frame rate")]);
        let (rate_row, rate_caption) = row(&view.rate.label, &view.rate.caption, &rate);
        content.append(&rate_row);

        // Background.
        let background = gtk::Box::new(gtk::Orientation::Vertical, 4);
        background.set_valign(gtk::Align::Center);
        let field = gtk::Entry::new();
        field.set_widget_name("settings-background");
        field.add_css_class("settings-field");
        field.set_size_request(260, -1);
        field.set_valign(gtk::Align::Center);
        field.set_max_length(7);
        field.set_text(&view.background.value);
        field.update_property(&[gtk::accessible::Property::Label("Background")]);
        let helper = gtk::Label::new(None);
        helper.add_css_class("field-helper");
        helper.set_xalign(0.0);
        helper.set_visible(false);
        background.append(&field);
        background.append(&helper);
        let (background_row, background_caption) = row(
            &view.background.label,
            &view.background.caption,
            &background,
        );
        content.append(&background_row);
        root.append(&content);

        // The footer: where this lives.
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        footer.add_css_class("settings-footer");
        let prefix = view
            .footer
            .strip_suffix(&format!("{}.", view.footer_path))
            .unwrap_or(&view.footer)
            .to_owned();
        let saved = gtk::Label::new(Some(&prefix));
        saved.add_css_class("t-caption");
        saved.add_css_class("muted");
        saved.set_valign(gtk::Align::BaselineCenter);
        let path = gtk::Label::new(Some(&format!("{}.", view.footer_path)));
        path.add_css_class("t-mono-sm");
        path.add_css_class("muted");
        path.set_valign(gtk::Align::BaselineCenter);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        footer.append(&saved);
        footer.append(&path);
        root.append(&footer);

        let parts = Rc::new(Parts {
            ctx: ctx.clone(),
            size: size.clone(),
            sizes: RefCell::new(view.size.options.iter().map(|o| o.label.clone()).collect()),
            size_caption,
            rate: rate.clone(),
            rate_caption,
            field: field.clone(),
            helper,
            background_caption,
            syncing: Cell::new(false),
            shown: RefCell::new(None),
        });
        parts.sync();
        parts.connect(&size, &rate, &field);
        let follower = Rc::clone(&parts);
        ctx.store
            .subscribe(move |change| {
                if change.prev.project.canvas != change.next.project.canvas {
                    follower.sync();
                }
            })
            .detach();
        track_menu(ctx, &size);
        Self { root, size }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// The control that gets the focus when the page opens.
    pub fn focus_target(&self) -> gtk::Widget {
        self.size.clone().upcast()
    }
}

/// The page's view for the newest snapshot.
fn page_view(ctx: &Ctx) -> View {
    let dir = ctx.store.project_dir().display().to_string();
    let home = std::env::var("HOME").ok();
    let shown = format::display_path(&dir, &dir, home.as_deref());
    project_page(&ctx.store.state(), &shown)
}

fn caption_label(text: &str) -> gtk::Label {
    let caption = gtk::Label::new(Some(text));
    caption.add_css_class("t-caption");
    caption.add_css_class("muted");
    caption.set_xalign(0.0);
    caption.set_wrap(true);
    caption.set_max_width_chars(1);
    caption
}

/// A 60 px row: label over caption on the left, `control` on the right.
fn row(label: &str, caption: &str, control: &impl IsA<gtk::Widget>) -> (gtk::Box, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    row.add_css_class("settings-row");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let name = gtk::Label::new(Some(label));
    name.add_css_class("t-body");
    name.set_xalign(0.0);
    let caption = caption_label(caption);
    text.append(&name);
    text.append(&caption);
    row.append(&text);
    let control = control.as_ref();
    control.set_halign(gtk::Align::End);
    row.append(control);
    (row, caption)
}

/// The page's controls and what they show.
struct Parts {
    ctx: Ctx,
    size: gtk::DropDown,
    sizes: RefCell<Vec<String>>,
    size_caption: gtk::Label,
    rate: adw::ToggleGroup,
    rate_caption: gtk::Label,
    field: gtk::Entry,
    helper: gtk::Label,
    background_caption: gtk::Label,
    /// Set while the controls follow the snapshot (no command then).
    syncing: Cell<bool>,
    shown: RefCell<Option<View>>,
}

impl Parts {
    /// Shows the project as the newest snapshot has it.
    fn sync(&self) {
        let view = page_view(&self.ctx);
        if self.shown.borrow().as_ref() == Some(&view) {
            return;
        }
        self.syncing.set(true);
        let labels: Vec<String> = view.size.options.iter().map(|o| o.label.clone()).collect();
        if *self.sizes.borrow() != labels {
            let strs: Vec<&str> = labels.iter().map(String::as_str).collect();
            self.size.set_model(Some(&gtk::StringList::new(&strs)));
            *self.sizes.borrow_mut() = labels;
        }
        if self.size.selected() != view.size.selected as u32 {
            self.size.set_selected(view.size.selected as u32);
        }
        self.size_caption.set_text(&view.size.caption);
        match view.rate.selected {
            Some(i) => self.rate.set_active(i as u32),
            None => self.rate.set_active(gtk::INVALID_LIST_POSITION),
        }
        self.rate_caption.set_text(&view.rate.caption);
        // A field being typed in keeps its text.
        if !self.field.has_focus() && !self.field_text_focused() {
            self.field.set_text(&view.background.value);
            self.set_invalid(None);
        }
        self.background_caption.set_text(&view.background.caption);
        self.syncing.set(false);
        *self.shown.borrow_mut() = Some(view);
    }

    /// The entry's text widget holds the focus (GTK focuses the inner
    /// `GtkText`).
    fn field_text_focused(&self) -> bool {
        self.field
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| focus.is_ancestor(&self.field))
    }

    fn set_invalid(&self, error: Option<&str>) {
        match error {
            Some(text) => {
                self.field.add_css_class("invalid");
                self.helper.set_text(text);
                self.helper.set_visible(true);
            }
            None => {
                self.field.remove_css_class("invalid");
                self.helper.set_visible(false);
            }
        }
    }

    fn connect(self: &Rc<Self>, size: &gtk::DropDown, rate: &adw::ToggleGroup, field: &gtk::Entry) {
        let parts = Rc::clone(self);
        size.connect_selected_notify(guarded!("settings: size", move |size| {
            if parts.syncing.get() {
                return;
            }
            let state = parts.ctx.store.state();
            let canvas = &state.project.canvas;
            let picked = parts
                .shown
                .borrow()
                .as_ref()
                .and_then(|v| v.size.options.get(size.selected() as usize).cloned());
            if let Some(option) = picked
                && (option.width, option.height) != (canvas.width, canvas.height)
            {
                parts
                    .ctx
                    .store
                    .dispatch(size_command(&state, option.width, option.height));
            }
        }));
        let parts = Rc::clone(self);
        rate.connect_active_name_notify(guarded!("settings: frame rate", move |rate| {
            if parts.syncing.get() {
                return;
            }
            let Some(fps) = rate.active_name().and_then(|n| n.parse::<u32>().ok()) else {
                return;
            };
            let state = parts.ctx.store.state();
            if state.project.canvas.fps != Fps::new(fps, 1) {
                parts
                    .ctx
                    .store
                    .dispatch(rate_command(&state, Fps::new(fps, 1)));
            }
            // The control shows the project's rate until the change lands
            // (with takes, the S18b modal asks first).
            *parts.shown.borrow_mut() = None;
            parts.sync();
        }));
        let parts = Rc::clone(self);
        field.connect_activate(guarded!("settings: background", move |field| {
            parts.commit(&field.text());
        }));
        let focus = gtk::EventControllerFocus::new();
        let parts = Rc::clone(self);
        focus.connect_leave(guarded!("settings: background blur", move |_| {
            parts.commit(&parts.field.text());
        }));
        field.add_controller(focus);
        let keys = gtk::EventControllerKey::new();
        let parts = Rc::clone(self);
        keys.connect_key_pressed(guarded!(
            "settings: background keys",
            after_panic = glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key != gtk::gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                // Esc puts the project's color back (the field handles
                // its keys, G§3.8 step 2).
                let value = parts.ctx.store.state().project.canvas.background.clone();
                parts.field.set_text(value.as_str());
                parts.set_invalid(None);
                glib::Propagation::Stop
            }
        ));
        field.add_controller(keys);
    }

    /// The Background field committed: `ProjectSet{background}` when it
    /// changed, or the field's error.
    fn commit(&self, text: &str) {
        match background_command(text) {
            Ok(cmd) => {
                self.set_invalid(None);
                let state = self.ctx.store.state();
                let same = text
                    .trim()
                    .eq_ignore_ascii_case(state.project.canvas.background.as_str());
                if !same {
                    self.ctx.store.dispatch(cmd);
                }
            }
            Err(error) => self.set_invalid(Some(&error)),
        }
    }
}

/// While the Size dropdown's list is open, the top overlay is a `Menu`: its
/// keys reach the list, and `Esc` closes the list, not Settings (G§3.8 step
/// 4).
fn track_menu(ctx: &Ctx, dropdown: &gtk::DropDown) {
    let mut child = dropdown.first_child();
    while let Some(c) = child {
        if let Ok(popover) = c.clone().downcast::<gtk::Popover>() {
            let store = ctx.store.clone();
            popover.connect_map(guarded!("settings: a list opens", move |_| {
                store.update_local(|l| l.overlays.push(Overlay::Menu));
            }));
            let store = ctx.store.clone();
            popover.connect_unmap(guarded!("settings: a list closes", move |_| {
                store.update_local(|l| {
                    if let Some(i) = l.overlays.iter().rposition(|o| *o == Overlay::Menu) {
                        l.overlays.remove(i);
                    }
                });
            }));
        }
        child = c.next_sibling();
    }
}
