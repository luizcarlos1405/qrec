//! Settings › Project's "All projects" group (G§4.10): the two machine-wide
//! settings about projects, in S18b's rows under the Canvas group: open the
//! last project on start, and the projects folder (Choose…, Clear). They
//! live in `config.toml`, written as `qrec config set` writes them; the
//! rest of the page is the open project's. Content:
//! `view::settings::all_projects`. Not drawn: S18b's patterns.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use qrec_core::config::{ProjectsDefaults, UserConfig};
use qrec_core::project_folders::{self, FolderEdit};
use qrec_core::protocol::ToastLevel;
use qrec_core::view::notices::Notice;
use qrec_core::view::settings::all_projects;

use super::project::row;
use crate::widgets::button::{self, Kind};
use crate::{Ctx, guarded};

/// The group.
pub struct AllProjects {
    root: gtk::Box,
    parts: Rc<Parts>,
}

struct Parts {
    ctx: Ctx,
    open_last: gtk::Switch,
    folder: gtk::Label,
    clear: gtk::Button,
    /// The project folders the file held last.
    projects: RefCell<ProjectsDefaults>,
    /// Set while the controls follow the file (no write then).
    syncing: Cell<bool>,
}

impl AllProjects {
    pub fn new(ctx: &Ctx) -> Self {
        let view = all_projects(&UserConfig::default(), home().as_deref());
        let keymap = ctx.store.keymap();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("settings-all-projects");
        root.set_margin_top(24);

        let group = gtk::Box::new(gtk::Orientation::Vertical, 0);
        group.add_css_class("settings-group");
        let heading = gtk::Label::new(Some(&view.group));
        heading.add_css_class("t-heading");
        heading.set_xalign(0.0);
        let caption = gtk::Label::new(Some(&view.caption));
        caption.add_css_class("t-caption");
        caption.add_css_class("muted");
        caption.set_xalign(0.0);
        group.append(&heading);
        group.append(&caption);
        root.append(&group);

        let open_last = gtk::Switch::new();
        open_last.set_widget_name("settings-open-last");
        open_last.set_focus_on_click(false);
        open_last.set_valign(gtk::Align::Center);
        open_last.update_property(&[gtk::accessible::Property::Label(&view.open_last_label)]);
        root.append(&row(&view.open_last_label, &view.open_last_caption, &open_last).0);

        let folder = gtk::Label::new(Some(&view.folder_shown));
        folder.set_widget_name("settings-projects-folder");
        folder.add_css_class("t-mono-sm");
        folder.add_css_class("muted");
        folder.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        folder.set_max_width_chars(24);
        folder.set_xalign(1.0);
        let choose = button::labeled("Choose…", None, None, &keymap, Kind::Secondary);
        choose.set_widget_name("settings-projects-folder-choose");
        let clear = button::labeled("Clear", None, None, &keymap, Kind::Flat);
        clear.set_widget_name("settings-projects-folder-clear");
        let control = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        control.set_valign(gtk::Align::Center);
        control.append(&folder);
        control.append(&choose);
        control.append(&clear);
        root.append(&row(&view.folder_label, &view.folder_caption, &control).0);

        let parts = Rc::new(Parts {
            ctx: ctx.clone(),
            open_last,
            folder,
            clear,
            projects: RefCell::default(),
            syncing: Cell::new(false),
        });
        parts.connect(&choose);
        parts.read();
        Self { root, parts }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Shows the file as it is now.
    pub fn refresh(&self) {
        self.parts.read();
    }
}

fn home() -> Option<String> {
    std::env::var("HOME").ok()
}

impl Parts {
    fn read(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        super::read_config(&self.ctx, move |config| {
            if let Some(parts) = weak.upgrade() {
                parts.show(&config);
            }
        });
    }

    fn show(&self, config: &UserConfig) {
        let view = all_projects(config, home().as_deref());
        self.syncing.set(true);
        if self.open_last.is_active() != view.open_last {
            self.open_last.set_active(view.open_last);
        }
        self.open_last.set_state(view.open_last);
        self.syncing.set(false);
        self.folder.set_text(&view.folder_shown);
        self.clear.set_sensitive(view.folder_set);
        *self.projects.borrow_mut() = config.projects.clone();
    }

    /// Makes one change to the project folders, then shows the file.
    fn edit_folders(self: &Rc<Self>, edit: FolderEdit) {
        let weak = Rc::downgrade(self);
        super::write_folders(&self.ctx, edit, move |written| {
            let Some(parts) = weak.upgrade() else {
                return;
            };
            match written {
                Ok(config) => parts.show(&config),
                Err(why) => {
                    tracing::warn!("settings: {why}");
                    parts.ctx.store.notify(
                        Notice::new(ToastLevel::Error, "Couldn't save the setting").with_body(why),
                    );
                    parts.read();
                }
            }
        });
    }

    /// Writes `key` = `value`, then shows the file; a failure is a toast
    /// and the file as it was.
    fn write(self: &Rc<Self>, key: &str, value: &str) {
        let weak = Rc::downgrade(self);
        super::write_setting(&self.ctx, key, value, move |written| {
            let Some(parts) = weak.upgrade() else {
                return;
            };
            match written {
                Ok(config) => parts.show(&config),
                Err(why) => {
                    tracing::warn!("settings: {why}");
                    parts.ctx.store.notify(
                        Notice::new(ToastLevel::Error, "Couldn't save the setting").with_body(why),
                    );
                    parts.read();
                }
            }
        });
    }

    fn choose_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Choose the projects folder")
            .modal(true)
            .build();
        let window = self.folder.root().and_downcast::<gtk::Window>();
        let weak = Rc::downgrade(self);
        dialog.select_folder(
            window.as_ref(),
            gio::Cancellable::NONE,
            guarded!("settings: the projects folder dialog", move |result| {
                let Some(parts) = weak.upgrade() else { return };
                match result.map(|folder| folder.path()) {
                    Ok(Some(dir)) => match dir.to_str() {
                        Some(dir) => {
                            let edit =
                                project_folders::make_default_path(&parts.projects.borrow(), dir);
                            parts.edit_folders(edit);
                        }
                        None => tracing::warn!("settings: the folder's path isn't UTF-8"),
                    },
                    Ok(None) => tracing::warn!("settings: the picked folder has no path"),
                    Err(e) => tracing::debug!("settings: no projects folder picked: {e}"),
                }
            }),
        );
    }

    fn connect(self: &Rc<Self>, choose: &gtk::Button) {
        let weak = Rc::downgrade(self);
        self.open_last.connect_state_set(guarded!(
            "settings: open the last project",
            after_panic = glib::Propagation::Stop,
            move |_, on| {
                let Some(parts) = weak.upgrade() else {
                    return glib::Propagation::Stop;
                };
                if parts.syncing.get() {
                    return glib::Propagation::Proceed;
                }
                parts.write("projects.open_last", if on { "true" } else { "false" });
                // The switch shows the file's value once written.
                glib::Propagation::Stop
            }
        ));
        let weak = Rc::downgrade(self);
        choose.connect_clicked(guarded!("settings: choose the folder", move |_| {
            if let Some(parts) = weak.upgrade() {
                parts.choose_folder();
            }
        }));
        let weak = Rc::downgrade(self);
        self.clear
            .connect_clicked(guarded!("settings: clear the folder", move |_| {
                if let Some(parts) = weak.upgrade() {
                    let default = parts.projects.borrow().default_folder().cloned();
                    if let Some(folder) = default {
                        parts.edit_folders(FolderEdit::Remove { name: folder.name });
                    }
                }
            }));
    }
}
