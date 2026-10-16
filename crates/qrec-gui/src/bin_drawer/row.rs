//! A bin row (C21, S14): 72 px with its 96×54 thumbnail, the title and its
//! status chip, the mono line, the secondary line (D-18) and, for broken
//! takes, the Open log link. The selected row (R1) reveals its actions
//! under it: Preview, Insert at end, Insert after selection, then the
//! reveal icon and Purge from disk (destructive-flat).
//!
//! The row's main area takes the focus on click (G§3.8) and is the drag
//! source of ok takes (D-38): the string `qrec-take:<id>`.

use std::rc::Rc;

use gtk::gdk;
use gtk::prelude::*;
use qrec_core::TakeId;
use qrec_core::view::bin::{BinAction, BinRow, Chip};

use crate::Ctx;
use crate::guarded;
use crate::widgets::button;
use crate::widgets::chip::{Chip as ChipView, ChipLevel};

use super::thumb::Thumb;

/// What a row asks the drawer for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowEvent {
    Select { ctrl: bool, shift: bool },
    Preview,
    InsertEnd,
    InsertAfter,
    Reveal,
    Purge,
    OpenLog,
    DragBegin,
    DragEnd,
}

/// Where a row's events go: the drawer.
pub type Sink = Rc<dyn Fn(&TakeId, RowEvent)>;

/// One row's widgets.
pub struct RowView {
    pub take: TakeId,
    pub root: gtk::Box,
    pub main: gtk::Box,
    pub thumb: Thumb,
    error_icon: gtk::Image,
    title: gtk::Label,
    chip: ChipView,
    mono: gtk::Label,
    secondary: gtk::Label,
    log: gtk::Button,
    pub actions: gtk::Box,
    first: gtk::Box,
    pub preview: gtk::Button,
    pub insert_end: gtk::Button,
    pub insert_after: gtk::Button,
    pub reveal: gtk::Button,
    pub purge: gtk::Button,
    shown: std::cell::RefCell<Option<BinRow>>,
}

fn flat(text: &str, icon: Option<&str>, kind: button::Kind) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("qrec-btn");
    b.add_css_class(kind.class());
    b.set_focus_on_click(false);
    // 8 px apart by CSS (`button.qrec-btn > box`).
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    if let Some(icon) = icon {
        content.append(&gtk::Image::from_icon_name(icon));
    }
    content.append(&gtk::Label::new(Some(text)));
    b.set_child(Some(&content));
    b.update_property(&[gtk::accessible::Property::Label(text)]);
    b
}

fn chip_level(chip: Chip) -> ChipLevel {
    match chip {
        Chip::Broken => ChipLevel::Error,
        Chip::TooShort => ChipLevel::Warning,
        Chip::Recovered | Chip::Unused => ChipLevel::Neutral,
    }
}

impl RowView {
    pub fn new(ctx: &Ctx, row: &BinRow, sink: &Sink) -> Self {
        let take = row.take.clone();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("bin-row");
        root.set_widget_name(&format!("bin-row-{}", take.number()));
        let main = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .accessible_role(gtk::AccessibleRole::ListItem)
            .build();
        main.add_css_class("bin-row-main");
        main.set_focusable(true);
        main.set_widget_name(&format!("bin-take-{}", take.number()));
        let thumb_box = gtk::Overlay::new();
        let thumb = Thumb::new(&ctx.media, &ctx.theme);
        thumb_box.set_child(Some(&thumb));
        let error_icon = gtk::Image::from_icon_name("dialog-error-symbolic");
        error_icon.set_pixel_size(20);
        error_icon.add_css_class("record-text");
        error_icon.set_halign(gtk::Align::Center);
        error_icon.set_valign(gtk::Align::Center);
        error_icon.set_can_target(false);
        thumb_box.add_overlay(&error_icon);
        thumb_box.set_valign(gtk::Align::Center);
        main.append(&thumb_box);
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 0);
        texts.add_css_class("bin-row-texts");
        texts.set_hexpand(true);
        texts.set_valign(gtk::Align::Center);
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        title_row.add_css_class("bin-row-title");
        let title = gtk::Label::new(None);
        title.add_css_class("t-body-strong");
        title.set_xalign(0.0);
        title_row.append(&title);
        let chip = ChipView::new("", ChipLevel::Neutral);
        title_row.append(&chip.widget());
        texts.append(&title_row);
        let mono = gtk::Label::new(None);
        mono.add_css_class("t-mono-sm");
        mono.add_css_class("muted");
        mono.set_xalign(0.0);
        mono.set_ellipsize(gtk::pango::EllipsizeMode::End);
        mono.set_max_width_chars(1);
        mono.set_hexpand(true);
        texts.append(&mono);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        line.add_css_class("bin-row-line");
        let secondary = gtk::Label::new(None);
        secondary.add_css_class("t-caption");
        secondary.add_css_class("muted");
        secondary.set_xalign(0.0);
        secondary.set_hexpand(true);
        secondary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        secondary.set_max_width_chars(1);
        line.append(&secondary);
        let log = gtk::Button::with_label("Open log");
        log.add_css_class("qrec-link");
        log.set_widget_name("bin-open-log");
        log.set_focus_on_click(false);
        log.set_valign(gtk::Align::Center);
        line.append(&log);
        texts.append(&line);
        main.append(&texts);
        root.append(&main);

        let actions = gtk::Box::new(gtk::Orientation::Vertical, 0);
        actions.add_css_class("bin-row-actions");
        let first = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        first.add_css_class("bin-actions-row");
        let preview = flat("Preview", None, button::Kind::Flat);
        preview.set_widget_name("bin-preview");
        let insert_end = flat("Insert at end", None, button::Kind::Flat);
        insert_end.set_widget_name("bin-insert-end");
        let insert_after = flat("Insert after selection", None, button::Kind::Flat);
        insert_after.set_widget_name("bin-insert-after");
        first.append(&preview);
        first.append(&insert_end);
        first.append(&insert_after);
        actions.append(&first);
        let second = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        second.add_css_class("bin-actions-row");
        let keymap = ctx.store.keymap();
        let reveal = button::icon(
            "folder-open-symbolic",
            "Reveal in file manager",
            None,
            &keymap,
        );
        reveal.add_css_class("bin-reveal");
        reveal.set_widget_name("bin-reveal");
        second.append(&reveal);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        second.append(&spacer);
        let purge = flat(
            "Purge from disk",
            Some("user-trash-symbolic"),
            button::Kind::DestructiveFlat,
        );
        purge.set_widget_name("bin-purge");
        second.append(&purge);
        actions.append(&second);
        actions.set_visible(false);
        root.append(&actions);

        let view = Self {
            take,
            root,
            main,
            thumb,
            error_icon,
            title,
            chip,
            mono,
            secondary,
            log,
            actions,
            first,
            preview,
            insert_end,
            insert_after,
            reveal,
            purge,
            shown: std::cell::RefCell::new(None),
        };
        view.connect(sink, row.insertable);
        view.show(row);
        view
    }

    fn connect(&self, sink: &Sink, insertable: bool) {
        let send = |event: RowEvent| {
            let (sink, take) = (Rc::clone(sink), self.take.clone());
            move || sink(&take, event.clone())
        };
        // The row's callbacks never hold its widgets strongly: a gesture's
        // closure keeping the widget it hangs on would be a cycle, and a
        // rebuild's dropped rows would never go (G§3.12).
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        let (sink2, take) = (Rc::clone(sink), self.take.clone());
        click.connect_pressed(guarded!("bin: a row's click", move |g, _n, _x, _y| {
            let state = g.current_event_state();
            if let Some(main) = g.widget() {
                main.grab_focus();
            }
            sink2(
                &take,
                RowEvent::Select {
                    ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
                    shift: state.contains(gdk::ModifierType::SHIFT_MASK),
                },
            );
        }));
        self.main.add_controller(click);
        for (button, event) in [
            (&self.preview, RowEvent::Preview),
            (&self.insert_end, RowEvent::InsertEnd),
            (&self.insert_after, RowEvent::InsertAfter),
            (&self.reveal, RowEvent::Reveal),
            (&self.purge, RowEvent::Purge),
            (&self.log, RowEvent::OpenLog),
        ] {
            let go = send(event);
            button.connect_clicked(guarded!("bin: a row action", move |_| go()));
        }
        if insertable {
            self.drag_source(sink);
        }
    }

    /// D-38: an ok take's row drags `qrec-take:<id>` onto the timeline.
    fn drag_source(&self, sink: &Sink) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::COPY);
        let text = format!("qrec-take:{}", self.take);
        source.connect_prepare(guarded!(
            "bin: a drag starts",
            after_panic = None,
            move |_, _, _| Some(gdk::ContentProvider::for_value(&text.to_value()))
        ));
        let (sink2, take, thumb) = (Rc::clone(sink), self.take.clone(), self.thumb.downgrade());
        source.connect_drag_begin(guarded!("bin: drag begins", move |source, _| {
            if let Some(thumb) = thumb.upgrade() {
                source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&thumb))), 0, 0);
            }
            sink2(&take, RowEvent::DragBegin);
        }));
        let (sink2, take) = (Rc::clone(sink), self.take.clone());
        source.connect_drag_end(guarded!("bin: drag ends", move |_, _, _| {
            sink2(&take, RowEvent::DragEnd);
        }));
        self.main.add_controller(source);
    }

    /// Shows `row`.
    pub fn show(&self, row: &BinRow) {
        if self.shown.borrow().as_ref() == Some(row) {
            return;
        }
        self.title.set_text(&row.title);
        self.chip.set_text(row.chip.label());
        self.chip.set_level(chip_level(row.chip));
        self.mono.set_text(&row.mono);
        self.secondary.set_text(&row.secondary);
        self.secondary.set_tooltip_text(Some(&row.secondary));
        self.thumb.set(&row.thumbnail, row.color);
        self.error_icon.set_visible(row.chip == Chip::Broken);
        self.log.set_visible(row.open_log().is_some());
        let has = |f: fn(&BinAction) -> bool| row.actions.iter().any(f);
        self.first
            .set_visible(has(|a| matches!(a, BinAction::Preview)));
        let name = format!(
            "{}, {}, {}, {}",
            row.title,
            row.chip.label(),
            row.mono,
            row.secondary
        );
        self.main
            .update_property(&[gtk::accessible::Property::Label(&name)]);
        *self.shown.borrow_mut() = Some(row.clone());
    }

    /// The row's last shown model.
    pub fn row(&self) -> Option<BinRow> {
        self.shown.borrow().clone()
    }

    /// Why Insert after selection waits (no timeline selection), if it does.
    fn after_disabled(&self) -> Option<String> {
        self.shown.borrow().as_ref().and_then(|row| {
            row.actions.iter().find_map(|a| match a {
                BinAction::InsertAfterSelection { disabled, .. } => disabled.clone(),
                _ => None,
            })
        })
    }

    /// The row as selected (R1), with its actions when it is the only one.
    pub fn set_selected(&self, selected: bool, actions: bool) {
        let expanded = selected && actions;
        for (class, on) in [("selected", selected), ("expanded", expanded)] {
            if on {
                self.root.add_css_class(class);
            } else {
                self.root.remove_css_class(class);
            }
        }
        self.actions.set_visible(expanded);
    }

    /// Enables what the record phase and the gates allow (D-43): `insert`
    /// for the inserts, `preview` for Preview.
    pub fn set_enabled(&self, insert: Result<(), String>, preview: bool) {
        self.insert_end.set_sensitive(insert.is_ok());
        self.insert_end
            .set_tooltip_text(insert.as_ref().err().map(String::as_str));
        let after = insert.err().or_else(|| self.after_disabled());
        self.insert_after.set_sensitive(after.is_none());
        self.insert_after.set_tooltip_text(after.as_deref());
        self.preview.set_sensitive(preview);
    }
}
