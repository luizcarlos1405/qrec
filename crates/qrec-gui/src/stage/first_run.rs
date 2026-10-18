//! The first run (S01a, D-16): with the project scene empty in Live mode the
//! preview gives way to "What should Qrec record?", the detected sources as
//! a checklist (the first screen and camera, the default mic and system
//! audio checked), the primary "Add 4 sources" button (focused, so `Enter`
//! adds them) and its caption. The button sends one command per checked
//! source in one FIFO: screens, cameras, the mic, system audio (so the
//! camera lands on top). How many are checked is `LocalUi.first_run_checked`
//! (the hint bar's "[Enter] Add N sources").

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use qrec_core::view::FocusRegion;
use qrec_core::view::canvas::{
    FIRST_RUN_CAPTION, FIRST_RUN_HEADING, FirstRunChoices, SourceKind, SourceRow,
    add_sources_label, first_run_commands, first_run_rows, with_choices,
};
use qrec_shell::sources::SourceInventory;

use crate::widgets::Spinner;
use crate::widgets::keycap::{self, CapSize, CapVariant};
use crate::{Ctx, guarded};

pub struct FirstRun {
    /// Scrolls in a window too short for it.
    pub root: gtk::ScrolledWindow,
    list: gtk::Box,
    looking: gtk::Box,
    button: gtk::Button,
    button_label: gtk::Label,
    rows: RefCell<Vec<(SourceRow, gtk::CheckButton)>>,
    /// The rows the person toggled: kept across a refreshed inventory (R25).
    choices: RefCell<FirstRunChoices>,
    inventory: RefCell<Option<Arc<SourceInventory>>>,
    /// Armed when the block shows: the button takes the focus once it can
    /// be pressed ([`FirstRun::take_focus`]).
    wants_focus: Cell<bool>,
    ctx: Ctx,
}

impl FirstRun {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.add_css_class("first-run");
        column.set_halign(gtk::Align::Center);
        column.set_valign(gtk::Align::Center);
        let heading = gtk::Label::new(Some(FIRST_RUN_HEADING));
        heading.add_css_class("t-heading");
        heading.add_css_class("first-run-heading");
        heading.set_xalign(0.0);
        column.append(&heading);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("first-run-list");
        column.append(&list);

        let looking = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        looking.add_css_class("first-run-looking");
        looking.append(&Spinner::new(16));
        let text = gtk::Label::new(Some("Looking for screens, cameras and mics\u{2026}"));
        text.add_css_class("t-body");
        text.add_css_class("muted");
        looking.append(&text);
        column.append(&looking);

        let button = gtk::Button::new();
        button.add_css_class("qrec-btn");
        button.add_css_class("primary");
        button.add_css_class("lg");
        button.add_css_class("first-run-add");
        // Named for the nested cases (G§8.4), as its rows are.
        button.set_widget_name("first-run-add");
        button.set_halign(gtk::Align::Start);
        button.set_focus_on_click(false);
        // 8 px apart by CSS (`button.qrec-btn > box`, common.css).
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let button_label = gtk::Label::new(Some(&add_sources_label(0)));
        content.append(&button_label);
        let cap = keycap::chord(&["Enter".to_owned()], CapSize::Md, CapVariant::OnFill);
        cap.set_valign(gtk::Align::Center);
        content.append(&cap);
        button.set_child(Some(&content));
        column.append(&button);

        let caption = gtk::Label::new(Some(FIRST_RUN_CAPTION));
        caption.add_css_class("t-caption");
        caption.add_css_class("muted");
        caption.add_css_class("first-run-caption");
        caption.set_xalign(0.0);
        caption.set_wrap(true);
        column.append(&caption);

        let root = gtk::ScrolledWindow::new();
        root.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        root.set_propagate_natural_height(true);
        root.set_propagate_natural_width(true);
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.set_child(Some(&column));
        root.add_css_class("first-run-scroll");
        root.set_visible(false);

        let this = Rc::new(Self {
            root,
            list,
            looking,
            button,
            button_label,
            rows: RefCell::new(Vec::new()),
            choices: RefCell::new(FirstRunChoices::new()),
            inventory: RefCell::new(None),
            wants_focus: Cell::new(false),
            ctx: ctx.clone(),
        });
        let weak = Rc::downgrade(&this);
        this.button
            .connect_clicked(guarded!("first run: add the sources", move |_| {
                if let Some(this) = weak.upgrade() {
                    this.add();
                }
            }));
        this.update_button();
        this
    }

    /// Rebuilds the rows when the discovery found something else, keeping
    /// what the person picked (R25) and the focus on its row.
    pub fn set_inventory(self: &Rc<Self>, inventory: Option<&Arc<SourceInventory>>) {
        let same = match (&*self.inventory.borrow(), inventory) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b) || **a == **b,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        *self.inventory.borrow_mut() = inventory.cloned();
        // The window's focus widget, active or not: discovery may answer
        // while the window is in the background (`has_focus` is false
        // then, and GTK moved the focus to the first row once its widget
        // went; R25's audit).
        let focus = self.list.root().and_then(|root| root.focus());
        let focused = self
            .rows
            .borrow()
            .iter()
            .find(|(_, check)| focus.as_ref() == Some(check.upcast_ref::<gtk::Widget>()))
            .map(|(row, _)| (row.kind, row.source.clone()));
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let mut rows = Vec::new();
        if let Some(inv) = inventory {
            let found = first_run_rows(
                &inv.screens,
                &inv.cameras,
                &inv.mics,
                &inv.monitors,
                inv.default_mic.as_deref(),
                inv.default_monitor.as_deref(),
            );
            // Picks of sources that went are forgotten.
            self.choices.borrow_mut().retain(|(kind, source), _| {
                found.iter().any(|r| r.kind == *kind && r.source == *source)
            });
            let found = with_choices(found, &self.choices.borrow());
            for (i, row) in found.into_iter().enumerate() {
                let check = self.row(&row);
                // Named for the nested cases (G§8.4): `first-run-row-0` ….
                check.set_widget_name(&format!("first-run-row-{i}"));
                self.list.append(&check);
                rows.push((row, check));
            }
        }
        self.list.set_visible(!rows.is_empty());
        self.looking.set_visible(inventory.is_none());
        if let Some(key) = focused {
            // The row that had the focus went with the rebuild: its new
            // one takes it (the button if the source is gone).
            match rows
                .iter()
                .find(|(row, _)| row.kind == key.0 && row.source == key.1)
            {
                Some((_, check)) => check.grab_focus(),
                None => self.button.grab_focus(),
            };
        }
        *self.rows.borrow_mut() = rows;
        self.update_button();
    }

    fn row(self: &Rc<Self>, row: &SourceRow) -> gtk::CheckButton {
        let check = gtk::CheckButton::new();
        check.add_css_class("first-run-row");
        check.set_active(row.checked);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let icon = match row.kind {
            SourceKind::Screen => "qrec-monitor-symbolic",
            SourceKind::Camera => "qrec-webcam-symbolic",
            SourceKind::Mic => "qrec-mic-symbolic",
            SourceKind::SystemAudio => "qrec-volume-2-symbolic",
        };
        content.append(&gtk::Image::from_icon_name(icon));
        let label = gtk::Label::new(Some(&row.label));
        label.add_css_class("t-body");
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&label);
        if let Some(meta) = &row.meta {
            let meta = gtk::Label::new(Some(meta));
            meta.add_css_class("t-mono-sm");
            meta.add_css_class("muted");
            meta.set_xalign(1.0);
            content.append(&meta);
        }
        check.set_child(Some(&content));
        check.update_property(&[gtk::accessible::Property::Label(&row.label)]);
        let weak = Rc::downgrade(self);
        let key = (row.kind, row.source.clone());
        check.connect_toggled(guarded!("first run: a source", move |check| {
            if let Some(this) = weak.upgrade() {
                this.choices
                    .borrow_mut()
                    .insert(key.clone(), check.is_active());
                this.update_button();
                this.ctx.store.update_local({
                    let checked = this.checked();
                    move |l| {
                        if l.first_run_checked.is_some() {
                            l.first_run_checked = Some(checked);
                        }
                    }
                });
            }
        }));
        check
    }

    /// How many sources are checked.
    pub fn checked(&self) -> usize {
        self.rows
            .borrow()
            .iter()
            .filter(|(_, check)| check.is_active())
            .count()
    }

    fn update_button(&self) {
        let n = self.checked();
        self.button_label.set_text(&add_sources_label(n));
        self.button.set_sensitive(n > 0);
        self.button
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "{}, Enter",
                add_sources_label(n)
            ))]);
    }

    /// The button: the checked sources' commands, in order (D-16).
    fn add(&self) {
        let rows: Vec<SourceRow> = self
            .rows
            .borrow()
            .iter()
            .map(|(row, check)| SourceRow {
                checked: check.is_active(),
                ..row.clone()
            })
            .collect();
        let cmds = first_run_commands(&rows);
        tracing::info!("first run: adding {} sources", cmds.len());
        for cmd in cmds {
            self.ctx.store.dispatch(cmd);
        }
    }

    /// The block shows (or not). Showing arms [`FirstRun::take_focus`].
    pub fn show(&self, visible: bool) {
        let was = self.root.is_visible();
        self.root.set_visible(visible);
        if visible != was {
            self.wants_focus.set(visible);
        }
    }

    /// Once per showing, as soon as the button can be pressed (the
    /// discovery may answer after the block shows): the button takes the
    /// focus, so `Enter` adds the sources. It takes it from GTK's initial
    /// focus (the header's first button) or the stage, not from a region
    /// the person moved to (S01a again after the last layer's delete in the
    /// panel keeps the panel's focus).
    pub fn take_focus(&self, focus: FocusRegion) {
        if !self.wants_focus.get() || !self.root.is_visible() || !self.button.is_sensitive() {
            return;
        }
        self.wants_focus.set(false);
        if matches!(
            focus,
            FocusRegion::Other | FocusRegion::Header | FocusRegion::Stage
        ) {
            let took = self.button.grab_focus();
            tracing::debug!("first run: the Add button takes the focus from {focus:?}: {took}");
        }
    }
}
