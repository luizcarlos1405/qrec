//! Settings › Project's "All projects" group (G§4.10): the machine-wide
//! settings about projects, under the Canvas group: open the last project
//! on start, and the project folders (design/boards/09-project-folders: the
//! list, the default one marked, Make default, rename in place, forget, Add
//! folder…). They live in `config.toml`, written as `qrec config set` and
//! `qrec folders` write them; the rest of the page is the open project's.
//! Content: `view::settings::all_projects`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use qrec_core::config::{ProjectsDefaults, UserConfig};
use qrec_core::project_folders::{self, FolderEdit};
use qrec_core::protocol::ToastLevel;
use qrec_core::view::notices::Notice;
use qrec_core::view::settings::{FolderRowView, all_projects};

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
    /// Add folder… (secondary; primary while there are none).
    add: gtk::Button,
    /// The folder rows (after `empty`), rebuilt when the folders change.
    rows: gtk::Box,
    empty: gtk::Label,
    /// The project folders the file held last.
    projects: RefCell<ProjectsDefaults>,
    /// What the rows show now: a re-read that changed nothing leaves a
    /// rename in progress alone.
    shown: RefCell<Option<Vec<FolderRowView>>>,
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

        // Project folders: the head with Add folder…, then the list.
        let add = button::labeled(
            "Add folder…",
            Some("qrec-plus-symbolic"),
            None,
            &keymap,
            Kind::Secondary,
        );
        add.set_widget_name("settings-folders-add");
        add.set_valign(gtk::Align::Center);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        list.add_css_class("settings-folders");
        let (head, _) = row(&view.folders_label, &view.folders_caption, &add);
        head.remove_css_class("settings-row");
        list.append(&head);
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rows.set_widget_name("settings-folders");
        rows.add_css_class("settings-folder-list");
        let empty = gtk::Label::new(Some(&view.folders_empty));
        empty.set_widget_name("settings-folders-empty");
        empty.add_css_class("t-body-sm");
        empty.add_css_class("muted");
        empty.add_css_class("settings-folders-empty");
        empty.set_xalign(0.0);
        empty.set_wrap(true);
        rows.append(&empty);
        list.append(&rows);
        root.append(&list);

        let parts = Rc::new(Parts {
            ctx: ctx.clone(),
            open_last,
            add,
            rows,
            empty,
            projects: RefCell::default(),
            shown: RefCell::new(None),
            syncing: Cell::new(false),
        });
        parts.connect();
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

    fn show(self: &Rc<Self>, config: &UserConfig) {
        let view = all_projects(config, home().as_deref());
        self.syncing.set(true);
        if self.open_last.is_active() != view.open_last {
            self.open_last.set_active(view.open_last);
        }
        self.open_last.set_state(view.open_last);
        self.syncing.set(false);
        *self.projects.borrow_mut() = config.projects.clone();
        let none = view.folders.is_empty();
        self.empty.set_visible(none);
        let (on, off) = if none {
            ("primary", "secondary")
        } else {
            ("secondary", "primary")
        };
        self.add.remove_css_class(off);
        self.add.add_css_class(on);
        if self.shown.borrow().as_ref() != Some(&view.folders) {
            self.rebuild(&view.folders);
            *self.shown.borrow_mut() = Some(view.folders);
        }
    }

    /// The folder rows for `folders`, after the empty line.
    fn rebuild(self: &Rc<Self>, folders: &[FolderRowView]) {
        while let Some(child) = self.rows.last_child() {
            if child == self.empty {
                break;
            }
            self.rows.remove(&child);
        }
        for folder in folders {
            self.rows.append(&self.folder_row(folder));
        }
    }

    /// One folder: icon, name, path; the Default badge or Make default (on
    /// hover); rename and forget.
    fn folder_row(self: &Rc<Self>, folder: &FolderRowView) -> gtk::Box {
        let keymap = self.ctx.store.keymap();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("settings-folder");
        let icon = gtk::Image::from_icon_name("qrec-folder-symbolic");
        icon.add_css_class("muted");
        row.append(&icon);
        let text = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        text.set_hexpand(true);
        let name = gtk::Label::new(Some(&folder.name));
        name.add_css_class("t-body-strong");
        name.set_xalign(0.0);
        let path = gtk::Label::new(Some(&folder.path));
        path.add_css_class("t-mono-sm");
        path.add_css_class("muted");
        path.set_xalign(0.0);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        path.set_hexpand(true);
        text.append(&name);
        text.append(&path);
        row.append(&text);
        let acts = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        acts.set_valign(gtk::Align::Center);
        if folder.default {
            let badge = gtk::Label::new(Some("Default"));
            badge.add_css_class("folder-badge");
            badge.set_widget_name("settings-folder-default");
            acts.append(&badge);
        } else {
            let make = button::labeled("Make default", None, None, &keymap, Kind::Flat);
            make.add_css_class("folder-make-default");
            make.update_property(&[gtk::accessible::Property::Label(&format!(
                "Make {} the default folder",
                folder.name
            ))]);
            let (weak, target) = (Rc::downgrade(self), folder.name.clone());
            make.connect_clicked(guarded!("settings: make a folder the default", move |_| {
                if let Some(parts) = weak.upgrade() {
                    parts.edit_folders(FolderEdit::SetDefault {
                        name: target.clone(),
                    });
                }
            }));
            acts.append(&make);
        }
        let rename = button::icon(
            "qrec-pencil-symbolic",
            &format!("Rename {}", folder.name),
            None,
            &keymap,
        );
        let (weak, target) = (Rc::downgrade(self), folder.name.clone());
        let (row_ref, text_ref, name_ref, acts_ref) =
            (row.clone(), text.clone(), name.clone(), acts.clone());
        rename.connect_clicked(guarded!("settings: rename a folder", move |_| {
            if let Some(parts) = weak.upgrade() {
                parts.start_rename(&row_ref, &text_ref, &name_ref, &acts_ref, &target);
            }
        }));
        acts.append(&rename);
        let forget = button::icon(
            "qrec-x-symbolic",
            &format!("Forget {}", folder.name),
            None,
            &keymap,
        );
        let (weak, target) = (Rc::downgrade(self), folder.name.clone());
        forget.connect_clicked(guarded!("settings: forget a folder", move |_| {
            if let Some(parts) = weak.upgrade() {
                parts.forget(&target);
            }
        }));
        acts.append(&forget);
        row.append(&acts);
        row
    }

    /// The name becomes a field: Enter saves, Esc or leaving it cancels.
    fn start_rename(
        self: &Rc<Self>,
        row: &gtk::Box,
        text: &gtk::Box,
        name: &gtk::Label,
        acts: &gtk::Box,
        target: &str,
    ) {
        let field = gtk::Entry::new();
        field.set_widget_name("settings-folder-rename");
        field.set_text(target);
        field.set_width_chars(22);
        field.update_property(&[gtk::accessible::Property::Label(&format!(
            "New name for {target}"
        ))]);
        name.set_visible(false);
        acts.set_visible(false);
        text.prepend(&field);
        let hint = gtk::Label::new(Some("Enter saves · Esc cancels"));
        hint.add_css_class("t-caption");
        hint.add_css_class("muted");
        row.append(&hint);
        field.grab_focus();
        let done = Rc::new(Cell::new(false));
        let end: Rc<dyn Fn()> = {
            let (text, field, row, hint, name, acts) = (
                text.clone(),
                field.clone(),
                row.clone(),
                hint.clone(),
                name.clone(),
                acts.clone(),
            );
            Rc::new(move || {
                if done.replace(true) {
                    return;
                }
                text.remove(&field);
                row.remove(&hint);
                name.set_visible(true);
                acts.set_visible(true);
            })
        };
        let (weak, from, on_save) = (Rc::downgrade(self), target.to_owned(), Rc::clone(&end));
        field.connect_activate(guarded!("settings: save a folder's name", move |field| {
            let new_name = field.text().trim().to_owned();
            on_save();
            let Some(parts) = weak.upgrade() else { return };
            if !new_name.is_empty() && new_name != from {
                parts.edit_folders(FolderEdit::Rename {
                    name: from.clone(),
                    new_name,
                });
            }
        }));
        let keys = gtk::EventControllerKey::new();
        let on_esc = Rc::clone(&end);
        keys.connect_key_pressed(guarded!(
            "settings: a folder's name keys",
            after_panic = glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key != gtk::gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                // The field has the keys (G§3.8 step 2): Esc cancels the
                // rename, not Settings.
                on_esc();
                glib::Propagation::Stop
            }
        ));
        field.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(guarded!("settings: leave a folder's name", move |_| end()));
        field.add_controller(focus);
    }

    /// Forgets a folder (nothing on disk changes) and says so.
    fn forget(self: &Rc<Self>, name: &str) {
        let weak = Rc::downgrade(self);
        let gone = name.to_owned();
        self.edit_then(FolderEdit::Remove { name: gone.clone() }, move || {
            if let Some(parts) = weak.upgrade() {
                parts.ctx.store.notify(Notice::new(
                    ToastLevel::Info,
                    format!("Forgot {gone}. The folder is untouched."),
                ));
            }
        });
    }

    /// Makes one change to the project folders, then shows the file.
    fn edit_folders(self: &Rc<Self>, edit: FolderEdit) {
        self.edit_then(edit, || {});
    }

    /// [`Self::edit_folders`], then `after` once it is saved.
    fn edit_then(self: &Rc<Self>, edit: FolderEdit, after: impl FnOnce() + 'static) {
        let weak = Rc::downgrade(self);
        super::write_folders(&self.ctx, edit, move |written| {
            let Some(parts) = weak.upgrade() else {
                return;
            };
            match written {
                Ok(config) => {
                    parts.show(&config);
                    after();
                }
                Err(why) => parts.failed(why),
            }
        });
    }

    /// A write that didn't happen: a toast, and the file as it is.
    fn failed(self: &Rc<Self>, why: String) {
        tracing::warn!("settings: {why}");
        self.ctx
            .store
            .notify(Notice::new(ToastLevel::Error, "Couldn't save the setting").with_body(why));
        self.read();
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
                Err(why) => parts.failed(why),
            }
        });
    }

    /// Add folder…: the folder chooser, then the folder under its own name
    /// (`videos 2` when that is taken).
    fn add_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Add a project folder")
            .modal(true)
            .build();
        let window = self.add.root().and_downcast::<gtk::Window>();
        let weak = Rc::downgrade(self);
        dialog.select_folder(
            window.as_ref(),
            gio::Cancellable::NONE,
            guarded!("settings: the project folder dialog", move |result| {
                let Some(parts) = weak.upgrade() else { return };
                match result.map(|folder| folder.path()) {
                    Ok(Some(dir)) => match dir.to_str() {
                        Some(dir) => {
                            let name = project_folders::unused_name_for_path(
                                &parts.projects.borrow(),
                                dir,
                            );
                            parts.edit_folders(FolderEdit::Add {
                                name,
                                path: dir.to_owned(),
                                make_default: false,
                            });
                        }
                        None => parts.failed("That folder's path isn't UTF-8 text.".to_owned()),
                    },
                    Ok(None) => parts.failed("That folder isn't on this computer.".to_owned()),
                    Err(e) => tracing::debug!("settings: no project folder picked: {e}"),
                }
            }),
        );
    }

    fn connect(self: &Rc<Self>) {
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
        self.add
            .connect_clicked(guarded!("settings: add a project folder", move |_| {
                if let Some(parts) = weak.upgrade() {
                    parts.add_folder();
                }
            }));
    }
}
