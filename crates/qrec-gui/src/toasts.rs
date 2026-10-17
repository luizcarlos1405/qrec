//! The toast host (C23, G§3.7): one toast at a time with a "+N" queue
//! chip, bottom-centered 12 px above the hint bar, 360–560 wide; info and
//! success 5 s (8 s with actions), warning 8 s, error until dismissed; at
//! most two actions. Owner: WP6.
//!
//! [`ToastQueue`] is the GTK-free part (timings, the queue, the actions
//! shown, the Undo guard): a toast equal to the newest one is that one
//! again (a held key's auto-repeat toasts once, its time starting over), an
//! Undo (`Dispatch(Undo{..})`) stays enabled
//! only while `history.cursor` is what it was when the toast arrived, and
//! the core's export-done and export-failed toasts are dropped while the
//! export panel shows that export's Done or Failed state
//! ([`shown_by_the_panel`]; the failed one since the WP7 integration: S17
//! draws no toast).
//!
//! A toast is `raised`, `lg` radius, e2 with a 1 px `border`: a 20 px level
//! icon, the `body-strong` title over a `body-sm` `muted` body (a path in
//! the mono face, S16b), up to two flat `accent-text` actions under them,
//! and a 24 px close button. It rises 12 px and fades in over 200 ms
//! (expo-out) and fades out over 140 ms (ease-in); only the fades under
//! reduced motion; at their end in test mode (G§8.4).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use qrec_core::on_event::output::{EXPORT_DONE_TITLE, EXPORT_FAILED_TITLE};
use qrec_core::protocol::{ToastLevel, UiAction};
use qrec_core::view::notices::{Notice, NoticeAction};
use qrec_core::{Command, protocol::command::UiPatch};

use crate::store::local::ExportPanelMode;
use crate::theme::motion::{self, EASE_IN, EASE_OUT_EXPO};
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

/// The actions a toast shows: at most two (DESIGN), in order.
pub fn shown_actions(notice: &Notice) -> &[NoticeAction] {
    &notice.actions[..notice.actions.len().min(2)]
}

/// An action that undoes (`Dispatch(Undo{..})`): enabled only while the
/// history hasn't moved since its toast arrived.
pub fn is_undo(action: &NoticeAction) -> bool {
    matches!(
        action,
        NoticeAction::Core(a) if matches!(&a.action, UiAction::Dispatch(cmd) if matches!(**cmd, Command::Undo { .. }))
    )
}

/// Whether `action` of a toast that arrived at history cursor `arrived`
/// can run at cursor `now`.
pub fn action_enabled(action: &NoticeAction, arrived: u64, now: u64) -> bool {
    !is_undo(action) || arrived == now
}

/// The core's export-done toast ("Layers! Like an onion. …").
pub fn is_export_done(notice: &Notice) -> bool {
    notice.title == EXPORT_DONE_TITLE
}

/// The core's export-failed toast ("Export failed", an error).
pub fn is_export_failed(notice: &Notice) -> bool {
    notice.title == EXPORT_FAILED_TITLE && notice.level == ToastLevel::Error
}

/// Whether the export panel open in `panel` shows what `notice` says (its
/// own export's Done or Failed page: it answers before the core's toast
/// comes), so the toast is dropped. A closed panel, or one showing
/// Running or Setup, lets it through (S16b, a CLI export).
pub fn shown_by_the_panel(notice: &Notice, panel: Option<ExportPanelMode>) -> bool {
    match panel {
        Some(ExportPanelMode::Done) => is_export_done(notice),
        Some(ExportPanelMode::Failed) => is_export_failed(notice),
        _ => false,
    }
}

/// The GUI action (G§3.8) a toast button runs, for the buttons that open
/// a region: Show export the export panel (`export.show`: it opens it and
/// leaves an open one as it is; `export.panel`, a toggle, closed the
/// passive panel the toast shows under), Open settings Settings.
pub fn gui_action(action: &NoticeAction) -> Option<&'static str> {
    match action {
        NoticeAction::ShowExport => Some(crate::export::SHOW),
        NoticeAction::OpenSettings(_) => Some("settings.open"),
        _ => None,
    }
}

/// The body is a path a toast action names (`exports/….mp4 · 88.4 MB`,
/// `frames/….png`): it reads in the mono face (S16b).
pub fn body_is_path(notice: &Notice) -> bool {
    let Some(body) = &notice.body else {
        return false;
    };
    notice.actions.iter().any(|action| match action {
        NoticeAction::Core(a) => match &a.action {
            UiAction::OpenPath(p)
            | UiAction::RevealPath(p)
            | UiAction::CopyText(p)
            | UiAction::CopyImage(p) => !p.is_empty() && body.starts_with(p.as_str()),
            UiAction::Dispatch(_) => false,
        },
        _ => false,
    })
}

/// The queue chip's text: "+2" while two toasts wait.
pub fn chip_text(waiting: usize) -> Option<String> {
    (waiting > 0).then(|| format!("+{waiting}"))
}

/// A toast in the queue, with the history cursor when it arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct Queued {
    pub notice: Notice,
    pub cursor: u64,
}

/// The toasts: the one shown and those waiting (GTK-free).
#[derive(Debug, Default)]
pub struct ToastQueue {
    showing: Option<Queued>,
    waiting: VecDeque<Queued>,
}

/// Where a pushed toast went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    /// It shows now (nothing was showing).
    Shown,
    /// It waits behind the one shown (the "+N" chip).
    Queued,
    /// The newest toast says the same: it is that one again (its history
    /// cursor the new one's), shown or waiting.
    Coalesced { shown: bool },
}

impl ToastQueue {
    /// Adds a toast at history cursor `cursor`. One that says what the
    /// newest toast says (a held key's auto-repeat: "Nothing to undo" 25
    /// times a second) is that toast again; `coalesce_shown` false (the
    /// shown toast is fading out) queues it after the shown one instead.
    pub fn push(&mut self, notice: Notice, cursor: u64, coalesce_shown: bool) -> Pushed {
        if let Some(newest) = self.waiting.back_mut()
            && newest.notice == notice
        {
            newest.cursor = cursor;
            return Pushed::Coalesced { shown: false };
        }
        if let Some(shown) = &mut self.showing
            && self.waiting.is_empty()
            && coalesce_shown
            && shown.notice == notice
        {
            shown.cursor = cursor;
            return Pushed::Coalesced { shown: true };
        }
        let queued = Queued { notice, cursor };
        if self.showing.is_none() {
            self.showing = Some(queued);
            Pushed::Shown
        } else {
            self.waiting.push_back(queued);
            Pushed::Queued
        }
    }

    /// The shown toast goes (its time ran out, or it was dismissed or
    /// acted on); the next one shows, if any.
    pub fn done(&mut self) -> Option<&Queued> {
        self.showing = self.waiting.pop_front();
        self.showing.as_ref()
    }

    /// The toast shown now.
    pub fn current(&self) -> Option<&Queued> {
        self.showing.as_ref()
    }

    /// How many wait behind it (the "+N" chip).
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }
}

/// The toast host (in the content layer, passive).
pub struct ToastHost {
    root: gtk::Box,
}

impl ToastHost {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("toast-host");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::End);
        root.set_margin_bottom(RISE);
        root.set_visible(false);
        let host = Rc::new(Host {
            ctx: ctx.clone(),
            root: root.clone(),
            inner: RefCell::new(Inner::default()),
        });
        let receiver = Rc::clone(&host);
        ctx.store
            .subscribe_notices(move |notice| receiver.push(notice.clone()))
            .detach();
        // The Undo guard follows the history.
        let follower = Rc::clone(&host);
        ctx.store
            .subscribe(move |change| {
                if change.history() {
                    follower.refresh_actions();
                }
            })
            .detach();
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

/// How far a toast rises as it comes in, and its gap above the hint bar.
const RISE: i32 = 12;
/// Toast in and out (DESIGN Motion).
const IN_MS: u32 = 200;
const OUT_MS: u32 = 140;

#[derive(Default)]
struct Inner {
    queue: ToastQueue,
    timer: Option<glib::SourceId>,
    /// The toast's action buttons and the actions they run.
    actions: Vec<(gtk::Button, NoticeAction)>,
    /// The running in or out animation (kept alive while it plays).
    animation: Option<adw::TimedAnimation>,
    /// A toast is fading out: further dismissals wait for it.
    leaving: bool,
}

struct Host {
    ctx: Ctx,
    root: gtk::Box,
    inner: RefCell<Inner>,
}

impl Host {
    fn push(self: &Rc<Self>, notice: Notice) {
        tracing::info!(
            "toast: {} {}",
            notice.title,
            notice.body.as_deref().unwrap_or_default()
        );
        // The export panel shows this result already (its Done or Failed
        // state).
        if shown_by_the_panel(&notice, self.ctx.store.local().export_panel()) {
            tracing::debug!("toast: the export panel shows it; dropped");
            return;
        }
        let cursor = self.ctx.store.state().history.cursor;
        let leaving = self.inner.borrow().leaving;
        let pushed = self.inner.borrow_mut().queue.push(notice, cursor, !leaving);
        match pushed {
            Pushed::Shown => self.show_current(),
            Pushed::Queued => self.refresh_chip(),
            // The toast on screen again: its time starts over (a held key
            // keeps one up, gone its time after the last repeat), its Undo
            // follows the newest arrival.
            Pushed::Coalesced { shown: true } => {
                self.start_timer();
                self.refresh_actions();
            }
            Pushed::Coalesced { shown: false } => {}
        }
    }

    /// The shown toast's timer, from now (none for an error, dismissed by
    /// hand).
    fn start_timer(self: &Rc<Self>) {
        if let Some(timer) = self.inner.borrow_mut().timer.take() {
            timer.remove();
        }
        let after = self
            .inner
            .borrow()
            .queue
            .current()
            .and_then(|q| duration(&q.notice));
        if let Some(after) = after {
            let host = Rc::clone(self);
            let id = guard::timeout_local_once(after, "toast: timer", move || {
                host.inner.borrow_mut().timer = None;
                host.dismiss();
            });
            self.inner.borrow_mut().timer = Some(id);
        }
    }

    /// Builds the toast now at the queue's head (or hides the host).
    fn show_current(self: &Rc<Self>) {
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        if let Some(timer) = self.inner.borrow_mut().timer.take() {
            timer.remove();
        }
        let current = self.inner.borrow().queue.current().cloned();
        let Some(queued) = current else {
            self.inner.borrow_mut().actions.clear();
            self.root.set_visible(false);
            return;
        };
        let waiting = self.inner.borrow().queue.waiting();
        let (toast, actions) = self.build(&queued.notice, waiting);
        self.inner.borrow_mut().actions = actions;
        self.root.append(&toast);
        self.root.set_visible(true);
        self.refresh_actions();
        self.animate_in(&toast);
        self.start_timer();
    }

    fn build(
        self: &Rc<Self>,
        notice: &Notice,
        waiting: usize,
    ) -> (gtk::Box, Vec<(gtk::Button, NoticeAction)>) {
        let toast = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        toast.add_css_class("qrec-toast");
        toast.add_css_class(level_class(notice.level));
        toast.set_width_request(360);
        let (icon_name, icon_class) = level_icon(notice.level);
        let icon = gtk::Image::from_icon_name(icon_name);
        icon.set_pixel_size(20);
        icon.add_css_class("toast-icon");
        icon.add_css_class(icon_class);
        icon.set_valign(gtk::Align::Start);
        toast.append(&icon);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_hexpand(true);
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::new(Some(&notice.title));
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.set_max_width_chars(58);
        title.add_css_class("t-body-strong");
        title.add_css_class("toast-title");
        title_row.append(&title);
        if let Some(chip) = chip_text(waiting) {
            let more = gtk::Label::new(Some(&chip));
            more.set_widget_name("toast-more");
            more.add_css_class("chip");
            more.add_css_class("neutral");
            more.set_valign(gtk::Align::Center);
            title_row.append(&more);
        }
        text.append(&title_row);
        if let Some(body) = &notice.body {
            let body_label = gtk::Label::new(Some(body));
            body_label.set_xalign(0.0);
            body_label.set_wrap(true);
            body_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            body_label.set_lines(2);
            body_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            body_label.add_css_class("toast-body");
            body_label.add_css_class("muted");
            if body_is_path(notice) {
                body_label.add_css_class("t-mono-sm");
                body_label.set_max_width_chars(64);
            } else {
                body_label.add_css_class("t-body-sm");
                body_label.set_max_width_chars(66);
            }
            text.append(&body_label);
        }
        let keymap = self.ctx.store.keymap();
        let mut buttons = Vec::new();
        let shown = shown_actions(notice);
        if !shown.is_empty() {
            toast.add_css_class("with-actions");
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            row.add_css_class("toast-actions");
            for (n, action) in shown.iter().enumerate() {
                let button =
                    button::labeled(action.label(), None, None, &keymap, button::Kind::Flat);
                button.set_widget_name(&format!("toast-action-{n}"));
                button.add_css_class("accent-text");
                button.add_css_class("toast-action");
                let (host, run) = (Rc::clone(self), action.clone());
                button.connect_clicked(guarded!("toast: action", move |_| {
                    host.run(&run);
                    host.dismiss();
                }));
                row.append(&button);
                buttons.push((button, action.clone()));
            }
            text.append(&row);
        }
        toast.append(&text);

        let close = button::icon("window-close-symbolic", "Dismiss", None, &keymap);
        close.set_widget_name("toast-dismiss");
        close.remove_css_class("icon-32");
        close.add_css_class("icon-24");
        close.set_valign(gtk::Align::Start);
        let host = Rc::clone(self);
        close.connect_clicked(guarded!("toast: dismiss", move |_| host.dismiss()));
        toast.append(&close);
        (toast, buttons)
    }

    /// The Undo guard: actions follow the history cursor.
    fn refresh_actions(&self) {
        let now = self.ctx.store.state().history.cursor;
        let inner = self.inner.borrow();
        let Some(current) = inner.queue.current() else {
            return;
        };
        for (button, action) in &inner.actions {
            button.set_sensitive(action_enabled(action, current.cursor, now));
        }
    }

    /// The "+N" chip of the toast shown follows the queue.
    fn refresh_chip(&self) {
        let waiting = self.inner.borrow().queue.waiting();
        let Some(chip) = chip_text(waiting) else {
            return;
        };
        let root: gtk::Widget = self.root.clone().upcast();
        match crate::window::find_named(&root, "toast-more") {
            Some(label) => {
                if let Ok(label) = label.downcast::<gtk::Label>() {
                    label.set_text(&chip);
                }
            }
            None => {
                // The title row is the text column's first child.
                let title_row = root
                    .first_child()
                    .and_then(|toast| toast.first_child())
                    .and_then(|icon| icon.next_sibling())
                    .and_then(|text| text.first_child())
                    .and_downcast::<gtk::Box>();
                if let Some(row) = title_row {
                    let more = gtk::Label::new(Some(&chip));
                    more.set_widget_name("toast-more");
                    more.add_css_class("chip");
                    more.add_css_class("neutral");
                    more.set_valign(gtk::Align::Center);
                    row.append(&more);
                }
            }
        }
    }

    /// The shown toast goes: it fades out, then the next one shows.
    fn dismiss(self: &Rc<Self>) {
        if self.inner.borrow().leaving {
            return;
        }
        if let Some(timer) = self.inner.borrow_mut().timer.take() {
            timer.remove();
        }
        let Some(toast) = self.root.first_child() else {
            self.advance();
            return;
        };
        if motion::one_shots_at_end() {
            self.advance();
            return;
        }
        self.inner.borrow_mut().leaving = true;
        let target_toast = toast.clone();
        let target = adw::CallbackAnimationTarget::new(move |v| {
            guard::run("toast: out", || {
                target_toast.set_opacity(1.0 - EASE_IN.at(v));
            });
        });
        let animation = adw::TimedAnimation::new(&toast, 0.0, 1.0, OUT_MS, target);
        animation.set_easing(adw::Easing::Linear);
        let host = Rc::clone(self);
        animation.connect_done(guarded!("toast: gone", move |_| {
            host.inner.borrow_mut().leaving = false;
            host.advance();
        }));
        self.replace_animation(&animation);
        animation.play();
    }

    /// Keeps `animation` alive while it plays; one still running (a toast
    /// dismissed while it rises) jumps to its end first.
    fn replace_animation(&self, animation: &adw::TimedAnimation) {
        let old = self.inner.borrow_mut().animation.replace(animation.clone());
        if let Some(old) = old
            && old.state() == adw::AnimationState::Playing
        {
            old.skip();
        }
    }

    /// The next toast in the queue (or none).
    fn advance(self: &Rc<Self>) {
        self.inner.borrow_mut().queue.done();
        self.show_current();
    }

    /// Rises 12 px and fades in (the fade only under reduced motion).
    fn animate_in(&self, toast: &gtk::Box) {
        if motion::one_shots_at_end() {
            toast.set_opacity(1.0);
            self.root.set_margin_bottom(RISE);
            return;
        }
        let reduced = self.ctx.theme.motion().reduced;
        let (target_toast, root) = (toast.clone(), self.root.clone());
        let target = adw::CallbackAnimationTarget::new(move |v| {
            guard::run("toast: in", || {
                let eased = EASE_OUT_EXPO.at(v);
                target_toast.set_opacity(eased);
                let lift = if reduced {
                    RISE
                } else {
                    (f64::from(RISE) * eased).round() as i32
                };
                root.set_margin_bottom(lift);
            });
        });
        toast.set_opacity(0.0);
        let animation = adw::TimedAnimation::new(toast, 0.0, 1.0, IN_MS, target);
        animation.set_easing(adw::Easing::Linear);
        self.replace_animation(&animation);
        animation.play();
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
            NoticeAction::ShowExport | NoticeAction::OpenSettings(_) => {
                if let Some(id) = gui_action(action) {
                    ctx.actions.run(id);
                }
            }
            NoticeAction::OpenBin => ctx.store.dispatch(Command::SetUi {
                patch: UiPatch {
                    bin_open: Some(true),
                    ..UiPatch::default()
                },
            }),
            NoticeAction::RetrySource(_) => ctx.store.retry_live(),
        }
    }
}

/// The toast's level class (`.info`, `.success`, `.warning`, `.error`).
fn level_class(level: ToastLevel) -> &'static str {
    match level {
        ToastLevel::Info => "info",
        ToastLevel::Success => "success",
        ToastLevel::Warning => "warning",
        ToastLevel::Error => "error",
    }
}

/// The level's icon and its color class (DESIGN Lists: info `text`,
/// success `success`, warning `warning-text`, error `record-text`).
fn level_icon(level: ToastLevel) -> (&'static str, &'static str) {
    match level {
        ToastLevel::Info => ("dialog-information-symbolic", "info-icon"),
        ToastLevel::Success => ("qrec-success-symbolic", "success-icon"),
        ToastLevel::Warning => ("dialog-warning-symbolic", "warning-text"),
        ToastLevel::Error => ("dialog-error-symbolic", "record-text"),
    }
}

#[cfg(test)]
mod tests {
    use qrec_core::protocol::ToastAction;

    use super::*;

    fn undo() -> NoticeAction {
        NoticeAction::Core(ToastAction {
            label: "Undo".into(),
            action: UiAction::Dispatch(Box::new(Command::Undo { steps: 1 })),
        })
    }

    fn open(path: &str) -> NoticeAction {
        NoticeAction::Core(ToastAction {
            label: "Open file".into(),
            action: UiAction::OpenPath(path.into()),
        })
    }

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
        let success = n(ToastLevel::Success).with_action(undo());
        assert_eq!(duration(&success), Some(Duration::from_secs(8)));
        // An error stays even with actions.
        let error = n(ToastLevel::Error).with_action(NoticeAction::OpenBin);
        assert_eq!(duration(&error), None);
    }

    #[test]
    fn one_toast_at_a_time_and_the_queue_chip() {
        let mut queue = ToastQueue::default();
        assert!(queue.current().is_none());
        let push = |queue: &mut ToastQueue, level, title: &str, cursor| {
            queue.push(Notice::new(level, title), cursor, true)
        };
        assert_eq!(
            push(&mut queue, ToastLevel::Info, "first", 3),
            Pushed::Shown
        );
        assert_eq!(
            push(&mut queue, ToastLevel::Warning, "second", 3),
            Pushed::Queued
        );
        assert_eq!(
            push(&mut queue, ToastLevel::Error, "third", 4),
            Pushed::Queued
        );
        assert_eq!(queue.current().unwrap().notice.title, "first");
        assert_eq!(queue.waiting(), 2);
        assert_eq!(chip_text(queue.waiting()).as_deref(), Some("+2"));
        assert_eq!(queue.done().unwrap().notice.title, "second");
        assert_eq!(chip_text(queue.waiting()).as_deref(), Some("+1"));
        let third = queue.done().unwrap();
        assert_eq!((third.notice.title.as_str(), third.cursor), ("third", 4));
        assert_eq!(chip_text(queue.waiting()), None);
        assert!(queue.done().is_none());
        assert!(queue.current().is_none());
        // Empty again: the next toast shows at once.
        assert_eq!(
            push(&mut queue, ToastLevel::Info, "again", 5),
            Pushed::Shown
        );
    }

    /// A held key whose action is unavailable (`Ctrl+Z` past the oldest
    /// entry: GTK delivers each auto-repeat as a press) toasts one
    /// "Nothing to undo", not one per repeat queued minutes ahead of real
    /// toasts; the same toast between others still counts (S25's "+2").
    #[test]
    fn a_repeated_toast_is_the_same_toast() {
        let mut queue = ToastQueue::default();
        let nothing = Notice::new(ToastLevel::Info, "Nothing to undo");
        assert_eq!(queue.push(nothing.clone(), 7, true), Pushed::Shown);
        for _ in 0..34 {
            assert_eq!(
                queue.push(nothing.clone(), 7, true),
                Pushed::Coalesced { shown: true }
            );
        }
        assert_eq!(queue.waiting(), 0);
        assert_eq!(queue.current().map(|q| &q.notice), Some(&nothing));
        // Behind another, it is the newest waiting one again.
        let error = Notice::new(ToastLevel::Error, "Recording didn't start");
        assert_eq!(queue.push(error.clone(), 7, true), Pushed::Queued);
        assert_eq!(
            queue.push(error.clone(), 7, true),
            Pushed::Coalesced { shown: false }
        );
        assert_eq!(queue.waiting(), 1);
        // A, B, A: three toasts.
        assert_eq!(queue.push(nothing.clone(), 7, true), Pushed::Queued);
        assert_eq!(queue.waiting(), 2);
        // Fading out, the shown one takes no more: the next shows after it.
        let mut leaving = ToastQueue::default();
        leaving.push(nothing.clone(), 7, true);
        assert_eq!(leaving.push(nothing.clone(), 7, false), Pushed::Queued);
        // The newest arrival's history cursor: its Undo is the one enabled.
        let mut undos = ToastQueue::default();
        let removed = Notice::new(ToastLevel::Success, "Removed a clip").with_action(undo());
        undos.push(removed.clone(), 5, true);
        assert_eq!(
            undos.push(removed, 6, true),
            Pushed::Coalesced { shown: true }
        );
        assert_eq!(undos.current().map(|q| q.cursor), Some(6));
    }

    #[test]
    fn at_most_two_actions_show() {
        // The core's export-done toast has three (Copy path stays in the
        // panel, S16b).
        let notice = Notice::new(ToastLevel::Success, EXPORT_DONE_TITLE)
            .with_body("exports/talk.mp4")
            .with_action(open("exports/talk.mp4"))
            .with_action(NoticeAction::OpenBin)
            .with_action(NoticeAction::ShowExport);
        let labels: Vec<&str> = shown_actions(&notice)
            .iter()
            .map(NoticeAction::label)
            .collect();
        assert_eq!(labels, ["Open file", "Open bin"]);
        assert!(is_export_done(&notice));
        assert!(body_is_path(&notice));
        assert!(shown_actions(&Notice::new(ToastLevel::Info, "x")).is_empty());
    }

    /// S16 and S17: the panel's Done or Failed page shows the export's end,
    /// so the core's toast for it is dropped; with the panel closed, or
    /// still on Running (a CLI export), it shows (S16b).
    #[test]
    fn the_panels_own_ending_drops_its_toast() {
        let done = Notice::new(ToastLevel::Success, EXPORT_DONE_TITLE);
        let failed = Notice::new(ToastLevel::Error, EXPORT_FAILED_TITLE);
        let other = Notice::new(ToastLevel::Error, "Couldn't open exports/a.mp4");
        assert!(shown_by_the_panel(&done, Some(ExportPanelMode::Done)));
        assert!(shown_by_the_panel(&failed, Some(ExportPanelMode::Failed)));
        assert!(!shown_by_the_panel(&failed, Some(ExportPanelMode::Done)));
        assert!(!shown_by_the_panel(&done, Some(ExportPanelMode::Failed)));
        for panel in [
            None,
            Some(ExportPanelMode::Running),
            Some(ExportPanelMode::Setup),
        ] {
            assert!(!shown_by_the_panel(&done, panel), "{panel:?}");
            assert!(!shown_by_the_panel(&failed, panel), "{panel:?}");
        }
        assert!(!shown_by_the_panel(&other, Some(ExportPanelMode::Failed)));
        let warning = Notice::new(ToastLevel::Warning, EXPORT_FAILED_TITLE);
        assert!(!is_export_failed(&warning), "only the core's error toast");
    }

    /// D-34's Show export opens the export panel and leaves an open one as
    /// it is: the toast shows under the passive Running panel, whose toggle
    /// closed the very panel the button names.
    #[test]
    fn show_export_opens_the_panel_and_never_closes_it() {
        use crate::export::{ACTIONS, PanelStep, panel_step};
        let id = gui_action(&NoticeAction::ShowExport).expect("a GUI action");
        let (_, request) = ACTIONS
            .iter()
            .find(|(i, _)| *i == id)
            .unwrap_or_else(|| panic!("{id} isn't the panel's"));
        assert_eq!(panel_step(None, *request), PanelStep::Open);
        for mode in [ExportPanelMode::Running, ExportPanelMode::Done] {
            assert_eq!(
                panel_step(Some(mode), *request),
                PanelStep::Stay,
                "{mode:?}"
            );
        }
        assert_eq!(
            gui_action(&NoticeAction::OpenSettings(
                crate::store::local::SettingsPage::Project
            )),
            Some("settings.open")
        );
        assert_eq!(gui_action(&NoticeAction::OpenBin), None);
    }

    #[test]
    fn undo_stays_enabled_while_the_history_stands_still() {
        let undo = undo();
        assert!(is_undo(&undo));
        assert!(action_enabled(&undo, 12, 12));
        assert!(!action_enabled(&undo, 12, 13), "something else was done");
        assert!(!action_enabled(&undo, 12, 11), "it was undone already");
        let other = open("exports/talk.mp4");
        assert!(!is_undo(&other));
        assert!(action_enabled(&other, 12, 40));
        assert!(action_enabled(&NoticeAction::ShowExport, 1, 2));
    }

    #[test]
    fn path_bodies_read_in_mono() {
        let path = "exports/rust-traits-talk-20261014-153012.mp4";
        let done = Notice::new(ToastLevel::Success, EXPORT_DONE_TITLE)
            .with_body(format!("{path} \u{b7} 88.4 MB"))
            .with_action(open(path));
        assert!(body_is_path(&done));
        let sentence = Notice::new(ToastLevel::Info, "Recovered take 7 from the bog.")
            .with_body("Qrec closed while it was recording. The take is in the bin.")
            .with_action(NoticeAction::OpenBin);
        assert!(!body_is_path(&sentence));
        assert!(!body_is_path(&Notice::new(ToastLevel::Info, "no body")));
    }
}
