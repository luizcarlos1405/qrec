//! The drafted X2 (not my swamp) with D-14's line, as the approved frame
//! draws it (C31 Blocking in S23's shell, under the minimal header
//! `Xba2H`; WP8 after G§0.2): the folder in its row, why it isn't a
//! project, the hidden-files note and Quit (exit 3). The two pages G§0.2
//! left undrawn take its shape: D-13's (a Qrec window holds the project)
//! and the not-responding one, each with the project in the row and what
//! holds it (exit 6). Content: `view::blocking::{not_my_swamp,
//! held_by_window, not_responding}`. Look: `components/blocking.css`.

use std::rc::Rc;

use gtk::prelude::*;
use qrec_core::view::blocking::{self, BlockKind, FolderPage, PageAction, RowIcon};
use qrec_core::view::format;

use super::missing_deps::{
    OnPress, Shell, clear, focused_button, icon, page_buttons, plain, wrapping,
};
use super::{OnAction, PageView};
use crate::Ctx;

/// The drafted page for `kind` (X2, D-13, not responding), or `None`.
pub fn view(ctx: &Ctx, kind: &BlockKind, on_action: OnAction) -> Option<Box<dyn PageView>> {
    let page = content(ctx, kind)?;
    Some(Box::new(FolderView::new(ctx, &page, on_action)))
}

/// The project folder as people read it (`~/…`).
pub(super) fn project_shown(ctx: &Ctx) -> String {
    let dir = ctx.store.project_dir().to_string_lossy().into_owned();
    let home = std::env::var("HOME").ok();
    format::display_path(&dir, "", home.as_deref())
}

/// The page `kind` shows, or `None` for another kind.
fn content(ctx: &Ctx, kind: &BlockKind) -> Option<FolderPage> {
    Some(match kind {
        BlockKind::NotMySwamp { dir, inside } => blocking::not_my_swamp(dir, inside.as_deref()),
        BlockKind::HeldByWindow { pid } => blocking::held_by_window(*pid, &project_shown(ctx)),
        BlockKind::NotResponding { pid, kind, since } => {
            blocking::not_responding(*pid, kind.as_deref(), since.as_deref(), &project_shown(ctx))
        }
        _ => return None,
    })
}

/// X2's view (and D-13's, not responding's).
struct FolderView {
    ctx: Ctx,
    shell: Shell,
    on_action: OnAction,
    focus: std::cell::RefCell<Option<gtk::Widget>>,
}

impl FolderView {
    fn new(ctx: &Ctx, page: &FolderPage, on_action: OnAction) -> Self {
        let shell = Shell::new(ctx);
        shell.widget().set_widget_name("blocking-folder-page");
        let view = Self {
            ctx: ctx.clone(),
            shell,
            on_action,
            focus: std::cell::RefCell::new(None),
        };
        view.render(page);
        view
    }

    fn render(&self, page: &FolderPage) {
        self.shell.set_head(&page.head);
        let content = &self.shell.content;
        clear(content);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_widget_name("blocking-folder");
        row.add_css_class("blocking-folder");
        let name = match page.row.icon {
            RowIcon::Folder => "qrec-folder-symbolic",
            RowIcon::FolderOpen => "qrec-folder-open-symbolic",
        };
        let folder = icon(name, &["blocking-status", "muted"]);
        folder.set_valign(gtk::Align::Center);
        row.append(&folder);
        let path = plain(&page.row.path, &["blocking-path", "t-mono"]);
        path.set_hexpand(true);
        path.set_xalign(0.0);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        path.set_max_width_chars(1);
        row.append(&path);
        let why = plain(&page.row.why, &["t-caption", "muted"]);
        why.set_valign(gtk::Align::Center);
        row.append(&why);
        content.append(&row);
        for line in &page.lines {
            let label = wrapping(&["blocking-line", "t-body"]);
            label.set_text(line);
            label.set_justify(gtk::Justification::Center);
            label.set_xalign(0.5);
            content.append(&label);
        }
        if let Some(note) = &page.note {
            let label = wrapping(&["blocking-note", "t-caption", "muted"]);
            label.set_widget_name("blocking-note");
            label.set_text(note);
            label.set_justify(gtk::Justification::Center);
            label.set_xalign(0.5);
            content.append(&label);
        }
        clear(&self.shell.actions);
        // Only flows here (Quit).
        let on_press: OnPress = Rc::new(|_: PageAction| {});
        let buttons = page_buttons(
            &self.ctx,
            &self.shell.actions,
            &page.buttons,
            &self.on_action,
            &on_press,
        );
        *self.focus.borrow_mut() = focused_button(&page.buttons, &buttons);
    }
}

impl PageView for FolderView {
    fn widget(&self) -> gtk::Widget {
        self.shell.widget()
    }

    fn update(&self, kind: &BlockKind) {
        if let Some(page) = content(&self.ctx, kind) {
            self.render(&page);
        }
    }

    fn focus_target(&self) -> Option<gtk::Widget> {
        self.focus.borrow().clone()
    }
}
