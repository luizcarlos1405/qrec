//! The toast host (C23, G§3.7): one toast at a time with a "+N" queue
//! chip, bottom-centered 12 px above the hint bar, 360–560 wide; info and
//! success 5 s (8 s with actions), warning 8 s, error until dismissed; at
//! most two actions. Owner: WP6.
//!
//! Until WP6 lands this is WP0b's stub: the same queue and timings in a
//! plain box, so the window's own notices ("Not built yet", keymap
//! problems, failed commands) and the core's toasts show.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use qrec_core::protocol::ToastLevel;
use qrec_core::view::notices::{Notice, NoticeAction};
use qrec_core::{Command, protocol::command::UiPatch};

use crate::widgets::button;
use crate::{Ctx, desktop, guard, guarded};

/// How long a toast stays: `None` until dismissed (errors).
pub fn duration(notice: &Notice) -> Option<Duration> {
    let with_actions = !notice.actions.is_empty();
    match notice.level {
        ToastLevel::Error => None,
        ToastLevel::Warning => Some(Duration::from_secs(8)),
        ToastLevel::Info | ToastLevel::Success if with_actions => Some(Duration::from_secs(8)),
        ToastLevel::Info | ToastLevel::Success => Some(Duration::from_secs(5)),
    }
}

/// The toast host (in the content layer, passive).
pub struct ToastHost {
    root: gtk::Box,
}

struct State {
    queue: VecDeque<Notice>,
    showing: bool,
    timer: Option<glib::SourceId>,
}

impl ToastHost {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("toast-host");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::End);
        root.set_margin_bottom(12);
        root.set_visible(false);
        let state = Rc::new(RefCell::new(State {
            queue: VecDeque::new(),
            showing: false,
            timer: None,
        }));
        let host = Rc::new(Host {
            ctx: ctx.clone(),
            root: root.clone(),
            state,
        });
        let receiver = Rc::clone(&host);
        ctx.store
            .subscribe_notices(move |notice| receiver.push(notice.clone()))
            .detach();
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

struct Host {
    ctx: Ctx,
    root: gtk::Box,
    state: Rc<RefCell<State>>,
}

impl Host {
    fn push(self: &Rc<Self>, notice: Notice) {
        tracing::info!(
            "toast: {} {}",
            notice.title,
            notice.body.as_deref().unwrap_or_default()
        );
        self.state.borrow_mut().queue.push_back(notice);
        if !self.state.borrow().showing {
            self.show_next();
        }
    }

    fn show_next(self: &Rc<Self>) {
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        if let Some(timer) = self.state.borrow_mut().timer.take() {
            timer.remove();
        }
        let next = self.state.borrow_mut().queue.pop_front();
        let Some(notice) = next else {
            self.state.borrow_mut().showing = false;
            self.root.set_visible(false);
            return;
        };
        self.state.borrow_mut().showing = true;
        let toast = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        toast.add_css_class("qrec-toast-stub");
        toast.set_width_request(360);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let title = gtk::Label::new(Some(&notice.title));
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.add_css_class("t-body-strong");
        text.append(&title);
        if let Some(body) = &notice.body {
            let body = gtk::Label::new(Some(body));
            body.set_xalign(0.0);
            body.set_wrap(true);
            body.set_max_width_chars(60);
            body.add_css_class("t-body-sm");
            body.add_css_class("muted");
            text.append(&body);
        }
        let waiting = self.state.borrow().queue.len();
        if waiting > 0 {
            let more = gtk::Label::new(Some(&format!("+{waiting}")));
            more.add_css_class("muted");
            text.append(&more);
        }
        toast.append(&text);
        let keymap = self.ctx.store.keymap();
        for action in notice.actions.iter().take(2) {
            let button = button::labeled(action.label(), None, None, &keymap, button::Kind::Flat);
            button.add_css_class("accent-text");
            let (host, action) = (Rc::clone(self), action.clone());
            button.connect_clicked(guarded!("toast: action", move |_| {
                host.run(&action);
                host.show_next();
            }));
            toast.append(&button);
        }
        let close = button::icon("window-close-symbolic", "Dismiss", None, &keymap);
        close.remove_css_class("icon-32");
        close.add_css_class("icon-24");
        close.set_valign(gtk::Align::Center);
        let host = Rc::clone(self);
        close.connect_clicked(guarded!("toast: dismiss", move |_| host.show_next()));
        toast.append(&close);
        self.root.append(&toast);
        self.root.set_visible(true);
        if let Some(after) = duration(&notice) {
            let host = Rc::clone(self);
            let id = guard::timeout_local_once(after, "toast: timer", move || {
                host.state.borrow_mut().timer = None;
                host.show_next();
            });
            self.state.borrow_mut().timer = Some(id);
        }
    }

    fn run(&self, action: &NoticeAction) {
        let ctx = &self.ctx;
        match action {
            NoticeAction::Core(toast_action) => {
                let window = self.root.root().and_downcast::<gtk::Window>();
                desktop::run(
                    ctx,
                    window.as_ref(),
                    ctx.store.project_dir(),
                    &toast_action.action,
                );
            }
            NoticeAction::ShowExport => {
                ctx.actions.run("export.panel");
            }
            NoticeAction::OpenBin => ctx.store.dispatch(Command::SetUi {
                patch: UiPatch {
                    bin_open: Some(true),
                    ..UiPatch::default()
                },
            }),
            NoticeAction::OpenSettings(_) => {
                ctx.actions.run("settings.open");
            }
            NoticeAction::RetrySource(_) => ctx.store.retry_live(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_durations() {
        let n = |level| Notice::new(level, "x");
        assert_eq!(duration(&n(ToastLevel::Info)), Some(Duration::from_secs(5)));
        assert_eq!(
            duration(&n(ToastLevel::Success)),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            duration(&n(ToastLevel::Warning)),
            Some(Duration::from_secs(8))
        );
        assert_eq!(duration(&n(ToastLevel::Error)), None);
        let with_action = n(ToastLevel::Info).with_action(NoticeAction::ShowExport);
        assert_eq!(duration(&with_action), Some(Duration::from_secs(8)));
    }
}
