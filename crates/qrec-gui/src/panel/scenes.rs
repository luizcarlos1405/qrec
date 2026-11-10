//! The scene bar (SPEC §5.6, the boards in `design/boards/07-named-scenes/`):
//! under the panel header in Live, the saved scene the project uses with its
//! thumbnail, `Edited` and Save when it changed, Save as… when it is none;
//! its menu lists the saved scenes (thumbnail, sources, the one in use,
//! `Alt` key) with each one's actions (Rename, Replace with current scene,
//! Duplicate, Delete…), then Save as new scene…, Save changes and Revert to
//! saved. Save as and Rename share one popover under the bar.
//!
//! Everything shown comes from `view::panel::scene_bar`; every control
//! sends a command (R8): `SceneUse`, `SceneSave`, `SceneRename`,
//! `SceneDuplicate`, `SceneDelete`. Deleting, and replacing a scene that
//! exists, are confirmed by the core's modal (`view::confirm`), except the
//! popover's own Replace, which the person just read.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use qrec_core::Command;
use qrec_core::ids::LayerId;
use qrec_core::model::{Scene, VideoSource};
use qrec_core::scenes::{self, SceneRow};
use qrec_core::view::panel::SceneBar;

use super::menus::{self, Menu};
use crate::Ctx;
use crate::guarded;
use crate::widgets::keycap::{self, CapSize, CapVariant};

/// The action group of the scene bar (Rename opens the popover).
const PREFIX: &str = "scenes";

/// A scene's layout drawn small: the canvas, each layer's rect (screens
/// `muted-40`, cameras `take-moss`, a source that isn't connected in
/// `warning`), on `project-canvas` with a `border-strong` edge.
#[derive(Clone)]
struct Thumb {
    area: gtk::DrawingArea,
    drawn: Rc<RefCell<Drawn>>,
}

#[derive(Default, Clone, PartialEq)]
struct Drawn {
    scene: Scene,
    canvas: (u32, u32),
    missing: BTreeSet<LayerId>,
}

impl Thumb {
    fn new(ctx: &Ctx, width: i32, height: i32) -> Self {
        let area = gtk::DrawingArea::new();
        area.set_content_width(width);
        area.set_content_height(height);
        area.add_css_class("scene-thumb");
        area.set_valign(gtk::Align::Center);
        area.set_accessible_role(gtk::AccessibleRole::Presentation);
        let drawn: Rc<RefCell<Drawn>> = Rc::default();
        let theme = ctx.theme.clone();
        let shown = Rc::clone(&drawn);
        area.set_draw_func(move |_, cr, w, h| {
            let palette = theme.palette();
            let d = shown.borrow();
            let (w, h) = (f64::from(w), f64::from(h));
            let rgba = |c: &gtk::gdk::RGBA| {
                cr.set_source_rgba(
                    f64::from(c.red()),
                    f64::from(c.green()),
                    f64::from(c.blue()),
                    f64::from(c.alpha()),
                );
            };
            rgba(&palette.project_canvas);
            cr.rectangle(0.0, 0.0, w, h);
            let _ = cr.fill();
            let (cw, ch) = (f64::from(d.canvas.0.max(1)), f64::from(d.canvas.1.max(1)));
            for layer in d.scene.layers.iter().filter(|l| l.visible) {
                let x = f64::from(layer.rect.x) / cw * w;
                let y = f64::from(layer.rect.y) / ch * h;
                let lw = f64::from(layer.rect.w) / cw * w;
                let lh = f64::from(layer.rect.h) / ch * h;
                if d.missing.contains(&layer.id) {
                    rgba(&palette.warning);
                    cr.set_line_width(1.0);
                    cr.rectangle(x + 0.5, y + 0.5, (lw - 1.0).max(0.0), (lh - 1.0).max(0.0));
                    let _ = cr.stroke();
                    continue;
                }
                match layer.source {
                    VideoSource::Camera { .. } => rgba(&palette.take_moss),
                    VideoSource::Screen { .. } => rgba(&palette.muted_40),
                }
                cr.rectangle(x, y, lw, lh);
                let _ = cr.fill_preserve();
                rgba(&palette.bg);
                cr.set_line_width(1.0);
                let _ = cr.stroke();
            }
            rgba(&palette.border_strong);
            cr.set_line_width(1.0);
            cr.rectangle(0.5, 0.5, w - 1.0, h - 1.0);
            let _ = cr.stroke();
        });
        Self { area, drawn }
    }

    fn show(&self, scene: &Scene, canvas: (u32, u32), missing: BTreeSet<LayerId>) {
        let next = Drawn {
            scene: scene.clone(),
            canvas,
            missing,
        };
        if *self.drawn.borrow() != next {
            *self.drawn.borrow_mut() = next;
            self.area.queue_draw();
        }
    }
}

/// What the save popover does.
#[derive(Debug, Clone, PartialEq)]
enum SaveMode {
    /// Save the project scene under a new name.
    SaveAs,
    /// Rename the saved scene `from`.
    Rename(String),
}

/// The scene bar and its popovers.
#[derive(Clone)]
pub struct SceneBarView {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    button: gtk::MenuButton,
    thumb: Thumb,
    name: gtk::Label,
    edited: gtk::Box,
    save: gtk::Button,
    save_as: gtk::Button,
    menu: gtk::Popover,
    count: gtk::Label,
    list: gtk::ListBox,
    note: gtk::Label,
    save_changes: gtk::Button,
    revert: gtk::Button,
    /// The save popover (Save as, Rename).
    pop: gtk::Popover,
    pop_title: gtk::Label,
    entry: gtk::Entry,
    helper: gtk::Label,
    what: gtk::Label,
    pop_confirm: gtk::Button,
    pop_label: gtk::Label,
    mode: RefCell<SaveMode>,
    /// What the bar shows now.
    bar: RefCell<Option<SceneBar>>,
    /// The menu's rows as last built.
    built: RefCell<String>,
    /// The Live layers whose source isn't connected (for the thumbnail).
    missing: RefCell<BTreeSet<LayerId>>,
}

/// A flat item at the menu's foot: a 16 px icon and its label.
fn foot_item(label: &str, icon: Option<&str>) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("scene-menu-item");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    if let Some(icon) = icon {
        content.append(&gtk::Image::from_icon_name(icon));
    }
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_hexpand(true);
    content.append(&text);
    button.set_child(Some(&content));
    button
}

impl SceneBarView {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.add_css_class("scene-row");
        root.set_widget_name("scene-row");

        // The bar: thumbnail, name, Edited, chevron.
        let thumb = Thumb::new(ctx, 32, 18);
        let name = gtk::Label::new(None);
        name.add_css_class("t-body-strong");
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let edited = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        edited.add_css_class("scene-edited");
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("scene-edited-dot");
        dot.set_valign(gtk::Align::Center);
        let edited_label = gtk::Label::new(Some("Edited"));
        edited_label.add_css_class("t-caption");
        edited_label.add_css_class("muted");
        edited.append(&dot);
        edited.append(&edited_label);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let chevron = gtk::Image::from_icon_name("qrec-chevron-down-symbolic");
        chevron.add_css_class("muted");
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        content.append(&thumb.area);
        content.append(&name);
        content.append(&edited);
        content.append(&spacer);
        content.append(&chevron);
        // The helper's button (its inner one styled and named); the
        // popover below replaces its empty model.
        let button = crate::widgets::button::menu_button(
            &gio::Menu::new(),
            &["scene-bar-button"],
            "Saved scenes",
        );
        button.add_css_class("scene-bar");
        button.set_widget_name("scene-bar");
        button.set_hexpand(true);
        button.set_child(Some(&content));
        button.set_always_show_arrow(false);

        let save = gtk::Button::with_label("Save");
        save.add_css_class("qrec-btn");
        save.add_css_class("secondary");
        save.add_css_class("scene-save");
        save.set_widget_name("scene-save");
        save.set_tooltip_text(Some("Save changes to the scene"));
        let save_as = gtk::Button::with_label("Save as\u{2026}");
        save_as.add_css_class("qrec-btn");
        save_as.add_css_class("secondary");
        save_as.add_css_class("scene-save");
        save_as.set_widget_name("scene-save-as");
        root.append(&button);
        root.append(&save);
        root.append(&save_as);

        // The scene menu.
        let menu = gtk::Popover::new();
        menu.add_css_class("scene-menu");
        menu.set_has_arrow(false);
        menu.set_position(gtk::PositionType::Bottom);
        let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        menu_box.set_size_request(352, -1);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.add_css_class("scene-menu-head");
        let head_title = gtk::Label::new(Some("Scenes"));
        head_title.add_css_class("t-caption-strong");
        head_title.add_css_class("muted");
        head_title.set_hexpand(true);
        head_title.set_xalign(0.0);
        let count = gtk::Label::new(None);
        count.add_css_class("t-caption");
        count.add_css_class("muted");
        head.append(&head_title);
        head.append(&count);
        let list = gtk::ListBox::new();
        list.add_css_class("scene-list");
        list.set_widget_name("scene-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let note = gtk::Label::new(None);
        note.add_css_class("t-caption");
        note.add_css_class("muted");
        note.add_css_class("scene-menu-note");
        note.set_wrap(true);
        // Its width is the popover's, not its whole line's.
        note.set_max_width_chars(1);
        note.set_xalign(0.0);
        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        sep.add_css_class("scene-menu-sep");
        let menu_save_as = foot_item("Save as new scene\u{2026}", None);
        menu_save_as.set_widget_name("scene-menu-save-as");
        let save_changes = foot_item("Save changes", None);
        save_changes.set_widget_name("scene-menu-save");
        let revert = foot_item("Revert to saved", Some("qrec-rotate-ccw-symbolic"));
        revert.set_widget_name("scene-menu-revert");
        menu_box.append(&head);
        menu_box.append(&list);
        menu_box.append(&note);
        menu_box.append(&sep);
        menu_box.append(&save_changes);
        menu_box.append(&menu_save_as);
        menu_box.append(&revert);
        menu.set_child(Some(&menu_box));
        button.set_popover(Some(&menu));
        menus::track_menu(
            ctx,
            &button,
            Some({
                let ctx = ctx.clone();
                Rc::new(move || menus::refresh_sources(&ctx)) as Rc<dyn Fn()>
            }),
        );

        // The save popover (Save as, Rename), under the bar.
        let pop = gtk::Popover::new();
        pop.add_css_class("scene-save-pop");
        pop.set_has_arrow(false);
        pop.set_position(gtk::PositionType::Bottom);
        pop.set_parent(&root);
        {
            let pop = pop.clone();
            root.connect_destroy(guarded!("scenes: the bar goes", move |_| pop.unparent()));
        }
        let pop_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pop_box.set_size_request(320, -1);
        let pop_title = gtk::Label::new(Some("Save scene"));
        pop_title.add_css_class("t-title");
        pop_title.set_xalign(0.0);
        let field_label = gtk::Label::new(Some("Name"));
        field_label.add_css_class("t-caption");
        field_label.add_css_class("muted");
        field_label.add_css_class("scene-field-label");
        field_label.set_xalign(0.0);
        let entry = gtk::Entry::new();
        entry.set_widget_name("scene-name");
        entry.update_property(&[gtk::accessible::Property::Label("Scene name")]);
        let helper = gtk::Label::new(None);
        helper.add_css_class("t-caption");
        helper.add_css_class("warning-text");
        helper.add_css_class("scene-helper");
        helper.set_wrap(true);
        // Its width is the popover's, not its whole line's.
        helper.set_max_width_chars(1);
        helper.set_xalign(0.0);
        helper.set_visible(false);
        let what = gtk::Label::new(None);
        what.add_css_class("t-caption");
        what.add_css_class("scene-what");
        what.set_wrap(true);
        // Its width is the popover's, not its whole line's.
        what.set_max_width_chars(1);
        what.set_xalign(0.0);
        let where_note = gtk::Label::new(Some(
            "Every project on this computer can use it. Takes already recorded keep their own layout.",
        ));
        where_note.add_css_class("t-caption");
        where_note.add_css_class("muted");
        where_note.add_css_class("scene-where");
        where_note.set_wrap(true);
        // Its width is the popover's, not its whole line's.
        where_note.set_max_width_chars(1);
        where_note.set_xalign(0.0);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("qrec-btn");
        cancel.add_css_class("secondary");
        let pop_confirm = gtk::Button::new();
        pop_confirm.add_css_class("qrec-btn");
        pop_confirm.add_css_class("primary");
        pop_confirm.set_widget_name("scene-save-confirm");
        let confirm_content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let pop_label = gtk::Label::new(Some("Save"));
        confirm_content.append(&pop_label);
        crate::widgets::button::show_binding_cap(
            &confirm_content,
            Some(&["Enter".to_owned()]),
            "scene-save-cap",
            CapSize::Md,
            CapVariant::OnFill,
        );
        pop_confirm.set_child(Some(&confirm_content));
        let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        foot.add_css_class("scene-pop-foot");
        foot.set_halign(gtk::Align::End);
        foot.append(&cancel);
        foot.append(&pop_confirm);
        pop_box.append(&pop_title);
        pop_box.append(&field_label);
        pop_box.append(&entry);
        pop_box.append(&helper);
        pop_box.append(&what);
        pop_box.append(&where_note);
        pop_box.append(&foot);
        pop.set_child(Some(&pop_box));
        {
            let pop = pop.clone();
            cancel.connect_clicked(guarded!("scenes: cancel", move |_| pop.popdown()));
        }
        track_popover(ctx, &pop);

        let inner = Rc::new(Inner {
            ctx: ctx.clone(),
            root,
            button,
            thumb,
            name,
            edited,
            save,
            save_as,
            menu,
            count,
            list,
            note,
            save_changes,
            revert,
            pop,
            pop_title,
            entry,
            helper,
            what,
            pop_confirm,
            pop_label,
            mode: RefCell::new(SaveMode::SaveAs),
            bar: RefCell::new(None),
            built: RefCell::new(String::new()),
            missing: RefCell::new(BTreeSet::new()),
        });
        Self::wire(&inner, &menu_save_as);
        Self { inner }
    }

    fn wire(inner: &Rc<Inner>, menu_save_as: &gtk::Button) {
        let weak = Rc::downgrade(inner);
        inner
            .save
            .connect_clicked(guarded!("scenes: save changes", move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.ctx.store.dispatch(Command::SceneSave { name: None });
                }
            }));
        let weak = Rc::downgrade(inner);
        inner
            .save_changes
            .connect_clicked(guarded!("scenes: save changes", move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.menu.popdown();
                    inner.ctx.store.dispatch(Command::SceneSave { name: None });
                }
            }));
        let weak = Rc::downgrade(inner);
        inner
            .revert
            .connect_clicked(guarded!("scenes: revert", move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.menu.popdown();
                    inner.ctx.store.dispatch(Command::SceneUse { name: None });
                }
            }));
        for button in [&inner.save_as, menu_save_as] {
            let weak = Rc::downgrade(inner);
            button.connect_clicked(guarded!("scenes: save as", move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.menu.popdown();
                    inner.open_save(SaveMode::SaveAs);
                }
            }));
        }
        let weak = Rc::downgrade(inner);
        inner
            .list
            .connect_row_activated(guarded!("scenes: use", move |_, row| {
                let Some(inner) = weak.upgrade() else { return };
                let name = row.widget_name().to_string();
                let Some(name) = name.strip_prefix("scene-row-") else {
                    return;
                };
                inner.menu.popdown();
                inner.ctx.store.dispatch(Command::SceneUse {
                    name: Some(name.to_owned()),
                });
            }));
        let weak = Rc::downgrade(inner);
        inner
            .entry
            .connect_changed(guarded!("scenes: the name", move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.check_name();
                }
            }));
        for confirm in [ConfirmFrom::Button, ConfirmFrom::Enter] {
            let weak = Rc::downgrade(inner);
            let run = move || {
                if let Some(inner) = weak.upgrade() {
                    inner.confirm_save();
                }
            };
            match confirm {
                ConfirmFrom::Button => {
                    inner
                        .pop_confirm
                        .connect_clicked(guarded!("scenes: save", move |_| run()));
                }
                ConfirmFrom::Enter => {
                    inner
                        .entry
                        .connect_activate(guarded!("scenes: save", move |_| run()));
                }
            }
        }
        // Rename, from a row's actions.
        let group = gio::SimpleActionGroup::new();
        let rename = gio::SimpleAction::new("rename", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(inner);
        rename.connect_activate(guarded!("scenes: rename", move |_, target| {
            let (Some(inner), Some(name)) =
                (weak.upgrade(), target.and_then(|t| t.get::<String>()))
            else {
                return;
            };
            inner.menu.popdown();
            inner.open_save(SaveMode::Rename(name));
        }));
        group.add_action(&rename);
        inner.root.insert_action_group(PREFIX, Some(&group));
        // The palette's "Save scene as…".
        let weak = Rc::downgrade(inner);
        inner.ctx.actions.register(
            "scene.save-as",
            Rc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.open_save(SaveMode::SaveAs);
                }
            }),
        );
    }

    pub fn widget(&self) -> gtk::Widget {
        self.inner.root.clone().upcast()
    }

    /// Shows `bar` (hidden without one: a take layout, Program);
    /// `missing`: the Live layers whose source isn't connected.
    pub fn update(&self, bar: Option<&SceneBar>, missing: BTreeSet<LayerId>) {
        self.inner.update(bar, missing);
    }
}

#[derive(Clone, Copy)]
enum ConfirmFrom {
    Button,
    Enter,
}

/// While `pop` shows, the window's overlays say `Menu` (the key router
/// leaves it its keys: typing a name, Enter, Esc).
fn track_popover(ctx: &Ctx, pop: &gtk::Popover) {
    use crate::store::local::Overlay;
    let store = ctx.store.clone();
    pop.connect_map(guarded!("scenes: the popover opens", move |_| {
        store.update_local(|l| l.overlays.push(Overlay::Menu));
    }));
    let store = ctx.store.clone();
    pop.connect_unmap(guarded!("scenes: the popover closes", move |_| {
        store.update_local(|l| {
            if let Some(at) = l.overlays.iter().rposition(|o| *o == Overlay::Menu) {
                l.overlays.remove(at);
            }
        });
    }));
}

impl Inner {
    fn update(&self, bar: Option<&SceneBar>, missing: BTreeSet<LayerId>) {
        let Some(bar) = bar else {
            self.root.set_visible(false);
            *self.bar.borrow_mut() = None;
            return;
        };
        self.root.set_visible(true);
        *self.missing.borrow_mut() = missing.clone();
        self.thumb.show(&bar.scene, bar.canvas, missing);
        match &bar.name {
            Some(name) => {
                self.name.set_text(name);
                self.name.remove_css_class("muted");
                self.name.add_css_class("t-body-strong");
            }
            None => {
                self.name.set_text("No saved scene");
                self.name.add_css_class("muted");
                self.name.remove_css_class("t-body-strong");
            }
        }
        self.edited.set_visible(bar.edited);
        self.save.set_visible(bar.edited);
        self.save_as
            .set_visible(bar.name.is_none() && bar.unreadable.is_none());
        let label = match (&bar.name, bar.edited) {
            (Some(name), true) => format!("Scene {name}, edited"),
            (Some(name), false) => format!("Scene {name}"),
            (None, _) => "No saved scene".to_owned(),
        };
        self.button
            .update_property(&[gtk::accessible::Property::Label(&label)]);
        let enabled = bar.enabled.is_ok();
        self.button.set_sensitive(enabled);
        self.save.set_sensitive(enabled);
        self.save_as.set_sensitive(enabled);
        if let Err(why) = &bar.enabled {
            self.button.set_tooltip_text(Some(why));
        } else {
            self.button.set_tooltip_text(Some("Saved scenes"));
        }
        self.save_changes.set_visible(bar.edited);
        self.revert.set_sensitive(bar.edited);
        self.count.set_text(&match bar.rows.len() {
            0 => String::new(),
            1 => "1 on this computer".to_owned(),
            n => format!("{n} on this computer"),
        });
        let note = match (&bar.unreadable, bar.rows.is_empty()) {
            (Some(why), _) => format!("scenes.json can't be read: {why}"),
            (None, true) => {
                "No saved scenes yet. Save this one to use it again in any project.".to_owned()
            }
            (None, false) => String::new(),
        };
        self.note.set_text(&note);
        self.note.set_visible(!note.is_empty());
        self.build_rows(bar);
        *self.bar.borrow_mut() = Some(bar.clone());
        if self.pop.is_visible() {
            self.check_name();
        }
    }

    /// The menu's rows, rebuilt when what they show changed.
    fn build_rows(&self, bar: &SceneBar) {
        let signature = format!(
            "{:?}|{:?}|{}",
            bar.rows
                .iter()
                .map(|r| (&r.name, &r.summary, r.in_use, r.key, &r.scene))
                .collect::<Vec<_>>(),
            bar.enabled,
            bar.edited
        );
        if *self.built.borrow() == signature {
            return;
        }
        *self.built.borrow_mut() = signature;
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        for row in &bar.rows {
            self.list.append(&self.scene_row(row, bar));
        }
    }

    fn scene_row(&self, row: &SceneRow, bar: &SceneBar) -> gtk::ListBoxRow {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.add_css_class("scene-item");
        let thumb = Thumb::new(&self.ctx, 48, 27);
        thumb.show(&row.scene, bar.canvas, BTreeSet::new());
        line.append(&thumb.area);
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 0);
        texts.set_hexpand(true);
        texts.set_valign(gtk::Align::Center);
        let name = gtk::Label::new(Some(&row.name));
        name.add_css_class("t-body-strong");
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let summary = gtk::Label::new(Some(&row.summary));
        summary.add_css_class("t-caption");
        summary.add_css_class("muted");
        summary.set_xalign(0.0);
        summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        texts.append(&name);
        texts.append(&summary);
        line.append(&texts);
        if row.in_use {
            let check = gtk::Image::from_icon_name("qrec-check-symbolic");
            check.add_css_class("accent-text");
            check.set_valign(gtk::Align::Center);
            check.set_tooltip_text(Some(if bar.edited {
                "In use, edited"
            } else {
                "In use"
            }));
            line.append(&check);
        }
        if let Some(key) = row.key {
            let caps = keycap::chord(
                &["Alt".to_owned(), key.to_string()],
                CapSize::Md,
                CapVariant::Plain,
            );
            caps.set_valign(gtk::Align::Center);
            line.append(&caps);
        }
        // The scene's own actions (C25), revealed on hover.
        let actions = Menu::new();
        let rename_target = row.name.to_variant();
        actions.update(String::new(), |menu| {
            let rename = gio::MenuItem::new(Some("Rename\u{2026}"), None);
            rename.set_action_and_target_value(
                Some(&format!("{PREFIX}.rename")),
                Some(&rename_target),
            );
            menu.append_item(&rename);
            menu.append_item(&menus::command_item(
                "Replace with current scene",
                Some(&Command::SceneSave {
                    name: Some(row.name.clone()),
                }),
            ));
            menu.append_item(&menus::command_item(
                "Duplicate",
                Some(&Command::SceneDuplicate {
                    name: row.name.clone(),
                    to: None,
                }),
            ));
            let danger = gio::Menu::new();
            danger.append_item(&menus::command_item(
                "Delete\u{2026}",
                Some(&Command::SceneDelete {
                    name: row.name.clone(),
                }),
            ));
            menu.append_section(None, &danger);
        });
        let more = menus::menu_button(&actions, &["flat", "icon-24", "scene-more"], "More");
        more.set_icon_name("qrec-ellipsis-vertical-symbolic");
        more.set_valign(gtk::Align::Center);
        menus::set_menu_label(&more, &format!("More for scene {}", row.name));
        line.append(&more);
        let list_row = gtk::ListBoxRow::new();
        list_row.set_widget_name(&format!("scene-row-{}", row.name));
        list_row.add_css_class("scene-list-row");
        list_row.set_child(Some(&line));
        list_row.set_activatable(bar.enabled.is_ok());
        list_row.update_property(&[gtk::accessible::Property::Label(&format!(
            "Use scene {}, {}",
            row.name, row.summary
        ))]);
        list_row
    }

    /// Opens the save popover in `mode`, its name selected.
    fn open_save(&self, mode: SaveMode) {
        let bar = self.bar.borrow().clone();
        let Some(bar) = bar else { return };
        let (title, text, confirm) = match &mode {
            SaveMode::SaveAs => {
                let base = bar.name.clone().unwrap_or_else(|| "Scene".to_owned());
                let names: Vec<&str> = bar.rows.iter().map(|r| r.name.as_str()).collect();
                let suggested = if bar.name.is_some() {
                    next_free(&names, &base)
                } else {
                    next_free(&names, &default_name(&bar.scene))
                };
                ("Save scene", suggested, "Save")
            }
            SaveMode::Rename(from) => ("Rename scene", from.clone(), "Rename"),
        };
        *self.mode.borrow_mut() = mode.clone();
        self.pop_title.set_text(title);
        self.pop_label.set_text(confirm);
        let layers = bar.scene.layers.len();
        let tracks = bar.scene.audio.len();
        let what = match &mode {
            SaveMode::SaveAs => format!(
                "{} {}: their sources, place, size, crop, opacity, fit and order.\n{} {}: gain, mute, sync offset and the autocut track.",
                layers,
                if layers == 1 { "layer" } else { "layers" },
                tracks,
                if tracks == 1 {
                    "audio track"
                } else {
                    "audio tracks"
                },
            ),
            SaveMode::Rename(_) => "The project and its takes don't change.".to_owned(),
        };
        self.what.set_text(&what);
        self.entry.set_text(&text);
        self.check_name();
        self.pop.popup();
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
    }

    /// The helper under the name: a name a scene has is replaced (Save
    /// as) or taken (Rename); an empty one can't be saved.
    fn check_name(&self) {
        let typed = self.entry.text().trim().to_owned();
        let bar = self.bar.borrow();
        let rows: &[SceneRow] = bar.as_ref().map_or(&[], |b| b.rows.as_slice());
        let taken = rows
            .iter()
            .find(|r| r.name.to_lowercase() == typed.to_lowercase())
            .map(|r| r.name.clone());
        let mode = self.mode.borrow().clone();
        let (helper, ok, confirm) = match (&mode, &taken, typed.is_empty()) {
            (_, _, true) => (None, false, None),
            (SaveMode::SaveAs, Some(name), _) => (
                Some(format!(
                    "A scene named \u{201c}{name}\u{201d} exists. Saving replaces it."
                )),
                true,
                Some("Replace"),
            ),
            (SaveMode::Rename(from), Some(name), _)
                if name.to_lowercase() != from.to_lowercase() =>
            {
                (
                    Some(format!("A scene is already named \u{201c}{name}\u{201d}.")),
                    false,
                    None,
                )
            }
            _ => (None, scenes::clean_name(&typed).is_ok(), None),
        };
        self.helper.set_visible(helper.is_some());
        self.helper.set_text(helper.as_deref().unwrap_or_default());
        if helper.is_some() {
            self.entry.add_css_class("warning");
        } else {
            self.entry.remove_css_class("warning");
        }
        self.pop_confirm.set_sensitive(ok);
        let default = match mode {
            SaveMode::SaveAs => "Save",
            SaveMode::Rename(_) => "Rename",
        };
        self.pop_label.set_text(confirm.unwrap_or(default));
    }

    fn confirm_save(&self) {
        if !self.pop_confirm.is_sensitive() {
            return;
        }
        let typed = self.entry.text().trim().to_owned();
        let mode = self.mode.borrow().clone();
        let store = self.ctx.store.clone();
        match mode {
            SaveMode::SaveAs => {
                let exists = self.bar.borrow().as_ref().is_some_and(|b| {
                    b.rows
                        .iter()
                        .any(|r| r.name.to_lowercase() == typed.to_lowercase())
                });
                let cmd = Command::SceneSave { name: Some(typed) };
                // The popover said it replaces: confirmed (SPEC §11).
                let sent = cmd.clone();
                let weak = store.downgrade();
                store.dispatch_with(cmd, exists, move |reply| {
                    if let Some(store) = weak.upgrade() {
                        store.apply_default(&sent, reply);
                    }
                });
            }
            SaveMode::Rename(from) => store.dispatch(Command::SceneRename {
                name: from,
                to: typed,
            }),
        }
        self.pop.popdown();
    }
}

/// A name for a scene saved for the first time, from what it shows:
/// "Screen + camera", "Camera only", "2 screens".
fn default_name(scene: &Scene) -> String {
    let screens = scene
        .layers
        .iter()
        .filter(|l| matches!(l.source, VideoSource::Screen { .. }))
        .count();
    let cameras = scene.layers.len() - screens;
    match (screens, cameras) {
        (0, 0) => "Scene".to_owned(),
        (0, 1) => "Camera only".to_owned(),
        (1, 0) => "Screen only".to_owned(),
        (1, 1) => "Screen + camera".to_owned(),
        (s, 0) => format!("{s} screens"),
        (0, c) => format!("{c} cameras"),
        (s, c) => format!(
            "{s} {} + {c} {}",
            if s == 1 { "screen" } else { "screens" },
            if c == 1 { "camera" } else { "cameras" }
        ),
    }
}

/// `base`, or "<base> 2", "<base> 3", … : the first no scene has.
fn next_free(names: &[&str], base: &str) -> String {
    let taken = |n: &str| names.iter().any(|t| t.to_lowercase() == n.to_lowercase());
    if !taken(base) {
        return base.to_owned();
    }
    (2u32..)
        .map(|n| format!("{base} {n}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| base.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_scene_is_named_after_what_it_shows_and_never_twice() {
        assert_eq!(default_name(&Scene::default()), "Scene");
        assert_eq!(
            next_free(&["Screen + camera"], "Screen + camera"),
            "Screen + camera 2"
        );
        assert_eq!(next_free(&["A"], "B"), "B");
    }
}
