//! The interim pages (G§1.1 #2): SPEC's copy and buttons for each blocking
//! page (`view::blocking::interim`), as a plain C31 block without art or
//! per-row layout: the title, the lines, a monospace block (a folder, a
//! command, a report; it scrolls when long) and the buttons, primary
//! first. Part of the G§0.2 review; WP8 replaces them one by one with the
//! drafted pages once approved.

use std::cell::RefCell;

use gtk::prelude::*;
use qrec_core::view::blocking::{BlockKind, BlockingAction, BlockingContent, interim};

use super::{OnAction, PageView};

/// An interim page.
pub struct InterimPage {
    root: gtk::Box,
    content: gtk::Box,
    on_action: OnAction,
    primary: RefCell<Option<gtk::Button>>,
}

impl InterimPage {
    pub fn new(kind: &BlockKind, on_action: OnAction) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("qrec-blocking");
        root.set_hexpand(true);
        root.set_vexpand(true);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        content.set_halign(gtk::Align::Center);
        content.set_valign(gtk::Align::Center);
        content.set_vexpand(true);
        content.set_width_request(560);
        root.append(&content);
        let page = Self {
            root,
            content,
            on_action,
            primary: RefCell::new(None),
        };
        page.render(&interim(kind));
        page
    }

    fn render(&self, page: &BlockingContent) {
        let focused_action = self.focused_action();
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        let title = gtk::Label::new(Some(&page.title));
        title.add_css_class("qrec-blocking-title");
        title.set_wrap(true);
        title.set_xalign(0.0);
        self.content.append(&title);
        for line in &page.lines {
            let label = gtk::Label::new(Some(line));
            label.set_wrap(true);
            label.set_xalign(0.0);
            label.set_selectable(true);
            label.set_focusable(false);
            self.content.append(&label);
        }
        if let Some(mono) = &page.monospace {
            let label = gtk::Label::new(Some(mono));
            label.add_css_class("qrec-blocking-mono");
            label.set_xalign(0.0);
            label.set_yalign(0.0);
            label.set_selectable(true);
            label.set_focusable(false);
            label.set_wrap(true);
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            scroll.set_propagate_natural_height(true);
            scroll.set_max_content_height(320);
            scroll.set_child(Some(&label));
            self.content.append(&scroll);
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_halign(gtk::Align::Start);
        let mut primary = None;
        let mut refocus = None;
        for spec in &page.buttons {
            let button = gtk::Button::with_label(&spec.label);
            button.add_css_class("pill");
            if spec.primary {
                button.add_css_class("suggested-action");
            }
            button.set_sensitive(!spec.busy);
            let (on_action, action) = (self.on_action.clone(), spec.action);
            button.connect_clicked(move |_| on_action(action));
            if spec.primary && primary.is_none() {
                primary = Some(button.clone());
            }
            if Some(spec.action) == focused_action && !spec.busy {
                refocus = Some(button.clone());
            }
            set_action(&button, spec.action);
            row.append(&button);
        }
        self.content.append(&row);
        *self.primary.borrow_mut() = primary;
        if let Some(button) = refocus {
            button.grab_focus();
        }
    }

    /// The action of the page button that has the focus, if one has it.
    fn focused_action(&self) -> Option<BlockingAction> {
        let root = self.root.root()?;
        let focus = root.focus()?;
        if !focus.is_ancestor(&self.content) {
            return None;
        }
        focus.downcast_ref::<gtk::Button>().and_then(action_of)
    }
}

impl PageView for InterimPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn update(&self, kind: &BlockKind) {
        self.render(&interim(kind));
    }

    fn focus_primary(&self) {
        let primary = self.primary.borrow().clone();
        match primary {
            Some(button) if button.is_sensitive() => {
                button.grab_focus();
            }
            _ => {
                self.content.child_focus(gtk::DirectionType::TabForward);
            }
        }
    }
}

/// Buttons remember their action by name (`widget_name`), so a re-render
/// can give the focus back to the same button.
fn set_action(button: &gtk::Button, action: BlockingAction) {
    button.set_widget_name(super::action_id(action));
}

fn action_of(button: &gtk::Button) -> Option<BlockingAction> {
    let name = button.widget_name();
    [
        BlockingAction::CheckAgain,
        BlockingAction::StopAndOpen,
        BlockingAction::Wait,
        BlockingAction::Quit,
    ]
    .into_iter()
    .find(|a| super::action_id(*a) == name.as_str())
}
