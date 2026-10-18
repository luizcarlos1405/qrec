//! The drafted X3 (qrec.json can't be read) and its missing-file variant,
//! as the approved frame draws it (C31 Blocking in S23's shell, under the
//! minimal header `Xba2H`; WP8 after G§0.2): the validation box (the file,
//! the first error's JSON Pointer, what it means), the restore command in
//! its well with Copy command (focused: `Enter` copies it), which entry it
//! restores, and Quit (exit 1). The page stays while the CLI works
//! in-process, so `qrec history restore <seq> --yes` can repair the project
//! meanwhile (G§3.3). Content: `view::blocking::unreadable`. Look:
//! `components/blocking.css`.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use qrec_core::model::PROJECT_FILE;
use qrec_core::view::blocking::{self, BlockKind, PageAction, UnreadablePage};
use qrec_core::view::format;

use super::missing_deps::{
    OnPress, Shell, button_name, clear, copy, focused_button, icon, page_buttons, plain, wrapping,
};
use super::{OnAction, PageView};
use crate::widgets::button::{self, Kind};
use crate::{Ctx, guarded};

/// The drafted page for `kind` (X3, missing or unreadable), or `None`.
pub fn view(ctx: &Ctx, kind: &BlockKind, on_action: OnAction) -> Option<Box<dyn PageView>> {
    let page = content(ctx, kind)?;
    Some(Box::new(UnreadableView::new(ctx, &page, on_action)))
}

/// The project's `qrec.json` as people read it.
fn project_file(ctx: &Ctx) -> String {
    let file = ctx.store.project_dir().join(PROJECT_FILE);
    let home = std::env::var("HOME").ok();
    format::display_path(&file.to_string_lossy(), "", home.as_deref())
}

fn content(ctx: &Ctx, kind: &BlockKind) -> Option<UnreadablePage> {
    match kind {
        BlockKind::Unreadable { error } => Some(blocking::unreadable(error, &project_file(ctx))),
        _ => None,
    }
}

/// X3's view.
struct UnreadableView {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    shell: Shell,
    on_action: OnAction,
    focus: RefCell<Option<gtk::Widget>>,
    /// The command Copy command copies.
    command: RefCell<Option<String>>,
}

impl UnreadableView {
    fn new(ctx: &Ctx, page: &UnreadablePage, on_action: OnAction) -> Self {
        let shell = Shell::new(ctx);
        shell.widget().set_widget_name("blocking-unreadable");
        // X3's content is 24 px apart (the box, then the restore).
        shell.content.add_css_class("wide");
        let inner = Rc::new(Inner {
            ctx: ctx.clone(),
            shell,
            on_action,
            focus: RefCell::new(None),
            command: RefCell::new(None),
        });
        render(&inner, page);
        Self { inner }
    }
}

fn render(inner: &Rc<Inner>, page: &UnreadablePage) {
    let shell = &inner.shell;
    shell.set_head(&page.head);
    let content = &shell.content;
    clear(content);
    if let Some(problem) = &page.problem {
        let boxed = gtk::Box::new(gtk::Orientation::Vertical, 0);
        boxed.set_widget_name("blocking-problem");
        boxed.add_css_class("blocking-problem");
        let file_line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        file_line.add_css_class("blocking-file-line");
        let error = icon(
            "qrec-circle-x-symbolic",
            &["blocking-error-icon", "record-text"],
        );
        error.set_valign(gtk::Align::Center);
        file_line.append(&error);
        let file = plain(&problem.file, &["t-mono-sm", "muted"]);
        file.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        file.set_max_width_chars(1);
        file.set_hexpand(true);
        file.set_xalign(0.0);
        file_line.append(&file);
        boxed.append(&file_line);
        let message = wrapping(&["blocking-message", "t-mono"]);
        message.set_text(&problem.message);
        boxed.append(&message);
        if let Some(explanation) = &problem.explanation {
            let label = wrapping(&["t-caption", "muted"]);
            label.set_text(explanation);
            boxed.append(&label);
        }
        content.append(&boxed);
    }
    let weak = Rc::downgrade(inner);
    let mut copy_button = None;
    *inner.command.borrow_mut() = page.restore.as_ref().map(|r| r.command.clone());
    if let Some(restore) = &page.restore {
        let block = gtk::Box::new(gtk::Orientation::Vertical, 0);
        block.set_widget_name("blocking-restore");
        block.add_css_class("blocking-restore");
        let line = plain(&restore.line, &["t-body"]);
        line.set_xalign(0.0);
        block.append(&line);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("blocking-command-row");
        let well = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        well.set_widget_name("blocking-command-well");
        well.add_css_class("blocking-command-well");
        well.set_hexpand(true);
        let command = plain(&restore.command, &["t-mono"]);
        command.set_hexpand(true);
        command.set_xalign(0.0);
        command.set_ellipsize(gtk::pango::EllipsizeMode::End);
        command.set_max_width_chars(1);
        well.append(&command);
        row.append(&well);
        let keymap = inner.ctx.store.keymap();
        let copy_it = button::labeled(
            "Copy command",
            Some("qrec-copy-symbolic"),
            None,
            &keymap,
            Kind::Primary,
        );
        copy_it.add_css_class("lg");
        copy_it.set_focus_on_click(true);
        copy_it.set_widget_name(&button_name(PageAction::CopyCommand));
        let weak = weak.clone();
        copy_it.connect_clicked(guarded!("X3: Copy command", move |button| {
            if let Some(inner) = weak.upgrade() {
                let command = inner.command.borrow().clone();
                if let Some(command) = command {
                    copy(&inner.ctx, button, &command, "Command copied");
                }
            }
        }));
        row.append(&copy_it);
        block.append(&row);
        if let Some(note) = &restore.note {
            let label = plain(note, &["t-caption", "muted"]);
            label.set_xalign(0.0);
            block.append(&label);
        }
        content.append(&block);
        copy_button = Some(copy_it);
    }
    if let Some(advice) = &page.advice {
        let label = wrapping(&["blocking-line", "t-body"]);
        label.set_text(advice);
        label.set_justify(gtk::Justification::Center);
        label.set_xalign(0.5);
        content.append(&label);
    }
    clear(&shell.actions);
    let on_press: OnPress = Rc::new(|_: PageAction| {});
    let buttons = page_buttons(
        &inner.ctx,
        &shell.actions,
        &page.buttons,
        &inner.on_action,
        &on_press,
    );
    // Copy command has the focus when there is one (X3's ring), else Quit.
    *inner.focus.borrow_mut() = copy_button
        .map(|b| b.upcast())
        .or_else(|| focused_button(&page.buttons, &buttons));
}

impl PageView for UnreadableView {
    fn widget(&self) -> gtk::Widget {
        self.inner.shell.widget()
    }

    fn update(&self, kind: &BlockKind) {
        if let Some(page) = content(&self.inner.ctx, kind) {
            render(&self.inner, &page);
        }
    }

    fn focus_target(&self) -> Option<gtk::Widget> {
        self.inner.focus.borrow().clone()
    }
}
