//! The drafted S23 (the swamp is missing something) and its GStreamer-stuck
//! variant, as the approved frame draws them (C31 Blocking with art and
//! rows, under the minimal header `Xba2H`; WP8 after G§0.2): a row per
//! failed check from the doctor's report (`LocalUi.doctor`, read again
//! whenever a new report comes: Check again), the others behind the
//! "✓ N other checks passed" disclosure, then Check again, Copy report and
//! Quit. The stuck page is the same shape with GStreamer's row and Quit
//! (`view::blocking::gst_stuck`).
//!
//! It also holds the C31 shell the pages of its shape share ([`Shell`]:
//! BRIEF draws X2 and X3 "in the S23 shell", and D-13's and the
//! not-responding pages follow them) and their buttons ([`page_buttons`]).
//! Look: `components/blocking.css`.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gtk::prelude::*;
use qrec_core::protocol::ToastLevel;
use qrec_core::view::blocking::{
    self, BlockKind, BodyLine, ButtonKind, CheckRow, CheckStatus, CheckView, ItemPart,
    MissingDepsPage, PageAction, PageButton, PageHead,
};
use qrec_core::view::notices::Notice;
use qrec_shell::doctor::DoctorReport;

use super::{OnAction, PageView};
use crate::store::Subscription;
use crate::theme::art::Art;
use crate::widgets::button::{self, Kind};
use crate::widgets::{Chip, chip::ChipLevel};
use crate::{Ctx, guarded};

/// The drafted page for `kind` (S23, GStreamer stuck), or `None`.
pub fn view(ctx: &Ctx, kind: &BlockKind, on_action: OnAction) -> Option<Box<dyn PageView>> {
    let stuck = match kind {
        BlockKind::MissingDeps { .. } => false,
        BlockKind::GstStuck => true,
        _ => return None,
    };
    Some(Box::new(MissingDepsView::new(ctx, stuck, on_action)))
}

/// The doctor's checks as S23 reads them.
pub fn check_views(report: &DoctorReport) -> Vec<CheckView> {
    report
        .checks
        .iter()
        .map(|c| CheckView {
            id: c.id.clone(),
            required: c.required,
            ok: c.ok,
            detail: c.detail.clone(),
            hint: c.hint.clone(),
        })
        .collect()
}

// ------------------------------------------------------------- the shell

/// What a page's own button (not a flow's) does when pressed.
pub(super) type OnPress = Rc<dyn Fn(PageAction)>;

/// C31 Blocking (S23's shell): a column 640 wide (× the UI scale), 72 px
/// under the header, its parts 16 px apart: the signpost (160), the title
/// (`display`, or `display-sm`), the body lines (`body` `muted`), the page's
/// content and its actions, centered. It scrolls when a page is taller than
/// the window (many failed checks, a small window).
pub(super) struct Shell {
    root: gtk::ScrolledWindow,
    title: gtk::Label,
    body: gtk::Box,
    /// The page's own part (`blocking-content`).
    pub content: gtk::Box,
    /// The page's buttons (`blocking-actions`).
    pub actions: gtk::Box,
    /// The art and the column follow the theme (its colors, its scale).
    _theme: Subscription,
}

/// The column's width at 100 % (BRIEF S23).
const COLUMN: f64 = 640.0;

impl Shell {
    pub fn new(ctx: &Ctx) -> Self {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.set_widget_name("blocking-column");
        column.add_css_class("blocking-column");
        // The signpost, 160 px square (`-gtk-icon-size`, so the UI scale
        // scales it), whatever size the art's own canvas is.
        let art = gtk::Image::builder()
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        art.set_widget_name("blocking-art");
        art.add_css_class("blocking-art");
        art.set_halign(gtk::Align::Center);
        art.set_can_target(false);
        column.append(&art);
        let title = wrapping(&["blocking-title", "t-display"]);
        title.set_widget_name("blocking-title");
        title.set_justify(gtk::Justification::Center);
        title.set_xalign(0.5);
        column.append(&title);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.add_css_class("blocking-body");
        column.append(&body);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_widget_name("blocking-content");
        content.add_css_class("blocking-content");
        column.append(&content);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        actions.set_widget_name("blocking-actions");
        actions.add_css_class("blocking-actions");
        actions.set_halign(gtk::Align::Center);
        column.append(&actions);
        let clamp = adw::Clamp::new();
        clamp.set_child(Some(&column));
        let root = gtk::ScrolledWindow::new();
        root.add_css_class("qrec-blocking-page");
        root.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_child(Some(&clamp));
        let follow = {
            let (art, clamp) = (art.downgrade(), clamp.downgrade());
            move |theme: &crate::theme::Theme| {
                if let Some(art) = art.upgrade() {
                    art.set_paintable(theme.art(Art::Signpost).as_ref());
                }
                if let Some(clamp) = clamp.upgrade() {
                    let width = (COLUMN * theme.scale()).round() as i32;
                    clamp.set_maximum_size(width);
                    clamp.set_tightening_threshold(width);
                }
            }
        };
        follow(&ctx.theme);
        let theme = ctx.theme.subscribe(follow);
        Self {
            root,
            title,
            body,
            content,
            actions,
            _theme: theme,
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// The title and the body lines.
    pub fn set_head(&self, head: &PageHead) {
        self.title.set_text(&head.title);
        let (on, off) = if head.small_title {
            ("t-display-sm", "t-display")
        } else {
            ("t-display", "t-display-sm")
        };
        self.title.remove_css_class(off);
        self.title.add_css_class(on);
        clear(&self.body);
        for line in &head.body {
            self.body.append(&body_line(line));
        }
    }
}

/// A body line: plain, or with its inline command chip.
fn body_line(line: &BodyLine) -> gtk::Widget {
    match line {
        BodyLine::Text { text } => {
            let label = wrapping(&["blocking-line", "t-body", "muted"]);
            label.set_text(text);
            label.set_justify(gtk::Justification::Center);
            label.set_xalign(0.5);
            label.upcast()
        }
        BodyLine::Command {
            before,
            command,
            after,
        } => {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.add_css_class("blocking-command-line");
            row.set_halign(gtk::Align::Center);
            row.append(&plain(before, &["t-body", "muted"]));
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            chip.add_css_class("blocking-chip");
            chip.set_valign(gtk::Align::Center);
            chip.append(&plain(command, &["t-mono-sm"]));
            row.append(&chip);
            row.append(&plain(after, &["t-body", "muted"]));
            row.upcast()
        }
    }
}

/// A one-line label.
pub(super) fn plain(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// A label that wraps at whatever width its box gives it (its natural
/// width doesn't widen the box: one character's).
pub(super) fn wrapping(classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(None);
    for class in classes {
        label.add_css_class(class);
    }
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(1);
    label.set_xalign(0.0);
    label
}

/// A symbolic icon (`blocking.css` sizes it by its classes).
pub(super) fn icon(name: &str, classes: &[&str]) -> gtk::Image {
    let image = gtk::Image::from_icon_name(name);
    for class in classes {
        image.add_css_class(class);
    }
    image
}

pub(super) fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// C01 large buttons for `specs` in `row`, in order; a press runs its flow
/// through `on_action` or the page's own action through `on_press`. The
/// buttons, by action, for updates and the focus.
pub(super) fn page_buttons(
    ctx: &Ctx,
    row: &gtk::Box,
    specs: &[PageButton],
    on_action: &OnAction,
    on_press: &OnPress,
) -> Vec<(PageAction, gtk::Button)> {
    let keymap = ctx.store.keymap();
    specs
        .iter()
        .map(|spec| {
            let kind = match spec.kind {
                ButtonKind::Primary => Kind::Primary,
                ButtonKind::Secondary => Kind::Secondary,
                ButtonKind::Flat => Kind::Flat,
            };
            let button = button::labeled(&spec.label, None, None, &keymap, kind);
            button.add_css_class("lg");
            // Page buttons are the keyboard's way out: they take focus.
            button.set_focus_on_click(true);
            button.set_widget_name(&button_name(spec.action));
            let (on_action, on_press, action) =
                (Rc::clone(on_action), Rc::clone(on_press), spec.action);
            button.connect_clicked(guarded!("a blocking page's button", move |_| {
                press(&on_action, &on_press, action)
            }));
            show_button(&button, spec);
            row.append(&button);
            (spec.action, button)
        })
        .collect()
}

/// A button's label and loading state from its spec.
pub(super) fn show_button(button: &gtk::Button, spec: &PageButton) {
    if let Some(label) = button
        .child()
        .and_downcast::<gtk::Box>()
        .and_then(|content| content.last_child())
        .and_downcast::<gtk::Label>()
        && label.text() != spec.label
    {
        label.set_text(&spec.label);
        button.update_property(&[gtk::accessible::Property::Label(&spec.label)]);
    }
    button::set_loading(button, spec.busy);
    button.set_sensitive(!spec.busy);
}

/// Runs a page button's action.
pub(super) fn press(on_action: &OnAction, on_press: &OnPress, action: PageAction) {
    match action {
        PageAction::Flow(flow) => on_action(flow),
        own => on_press(own),
    }
}

/// A page button's widget name: `blocking-<action>` (the cases click it).
pub(super) fn button_name(action: PageAction) -> String {
    let id = match action {
        PageAction::Flow(flow) => super::action_id(flow).trim_start_matches("blocking."),
        PageAction::CopyReport => "copy-report",
        PageAction::CopyCommand => "copy-command",
    };
    format!("blocking-{id}")
}

/// The button the page focuses when it shows: the spec's focused one.
pub(super) fn focused_button(
    specs: &[PageButton],
    buttons: &[(PageAction, gtk::Button)],
) -> Option<gtk::Widget> {
    let action = specs.iter().find(|b| b.focused)?.action;
    buttons
        .iter()
        .find(|(a, _)| *a == action)
        .map(|(_, b)| b.clone().upcast())
}

/// Puts `text` on the clipboard and says so with a success toast titled
/// `title` ("Command copied").
pub(super) fn copy(ctx: &Ctx, widget: &impl IsA<gtk::Widget>, text: &str, title: &str) {
    widget.clipboard().set_text(text);
    ctx.store.notify(Notice::new(ToastLevel::Success, title));
}

// ---------------------------------------------------------------- S23

/// S23's view: the shell, the checks box and the buttons.
struct MissingDepsView {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    stuck: bool,
    shell: Shell,
    checks: gtk::Box,
    buttons: RefCell<Vec<(PageAction, gtk::Button)>>,
    focus: RefCell<Option<gtk::Widget>>,
    /// The report the rows show (a new one, from Check again, shows again).
    shown: RefCell<Option<Arc<DoctorReport>>>,
    on_action: OnAction,
    /// The doctor's report as it changes (`LocalUi.doctor`).
    _doctor: RefCell<Option<Subscription>>,
}

impl MissingDepsView {
    fn new(ctx: &Ctx, stuck: bool, on_action: OnAction) -> Self {
        let shell = Shell::new(ctx);
        shell.root.set_widget_name(if stuck {
            "blocking-gst-stuck"
        } else {
            "blocking-missing-deps"
        });
        let checks = gtk::Box::new(gtk::Orientation::Vertical, 0);
        checks.set_widget_name("blocking-checks");
        checks.add_css_class("blocking-checks");
        checks.set_overflow(gtk::Overflow::Hidden);
        shell.content.append(&checks);
        let inner = Rc::new(Inner {
            ctx: ctx.clone(),
            stuck,
            shell,
            checks,
            buttons: RefCell::new(Vec::new()),
            focus: RefCell::new(None),
            shown: RefCell::new(None),
            on_action,
            _doctor: RefCell::new(None),
        });
        render(&inner);
        if !stuck {
            let weak = Rc::downgrade(&inner);
            let sub = ctx.store.subscribe_local(move |local| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let changed = match (&local.doctor, &*inner.shown.borrow()) {
                    (Some(new), Some(old)) => !Arc::ptr_eq(new, old),
                    (new, old) => new.is_some() != old.is_some(),
                };
                if changed {
                    render(&inner);
                }
            });
            *inner._doctor.borrow_mut() = Some(sub);
        }
        Self { inner }
    }
}

/// The page for the report shown now (the stuck page has none).
fn content(inner: &Inner) -> MissingDepsPage {
    if inner.stuck {
        return blocking::gst_stuck();
    }
    let report = inner.ctx.store.local().doctor.clone();
    let checks = report.as_deref().map(check_views).unwrap_or_default();
    *inner.shown.borrow_mut() = report;
    blocking::missing_deps(&checks)
}

fn render(inner: &Rc<Inner>) {
    let page = content(inner);
    let focused = focused_action(inner);
    inner.shell.set_head(&page.head);
    clear(&inner.checks);
    inner
        .checks
        .set_visible(!page.rows.is_empty() || !page.passed.is_empty());
    let weak = Rc::downgrade(inner);
    for row in &page.rows {
        inner.checks.append(&check_row(inner, row, &weak));
    }
    if !page.passed.is_empty() {
        inner.checks.append(&passed(&page));
    }
    clear(&inner.shell.actions);
    let on_press: OnPress = {
        let weak = weak.clone();
        Rc::new(move |action| {
            if let Some(inner) = weak.upgrade() {
                on_own(&inner, action, None);
            }
        })
    };
    let buttons = page_buttons(
        &inner.ctx,
        &inner.shell.actions,
        &page.buttons,
        &inner.on_action,
        &on_press,
    );
    *inner.focus.borrow_mut() = focused_button(&page.buttons, &buttons);
    // Check again rebuilt the buttons: the one that had the focus keeps it.
    if let Some((_, button)) = buttons.iter().find(|(a, _)| Some(*a) == focused) {
        button.grab_focus();
    }
    *inner.buttons.borrow_mut() = buttons;
}

/// The page button that has the focus, if one has it.
fn focused_action(inner: &Inner) -> Option<PageAction> {
    let root = inner.shell.root.root()?;
    let focus = root.focus()?;
    inner
        .buttons
        .borrow()
        .iter()
        .find(|(_, b)| focus == *b.upcast_ref::<gtk::Widget>())
        .map(|(a, _)| *a)
}

/// The page's own actions: Copy report, a row's Copy (its `command`).
fn on_own(inner: &Inner, action: PageAction, command: Option<&str>) {
    match action {
        PageAction::CopyReport => {
            let report = inner.ctx.store.local().doctor.clone();
            if let Some(report) = report {
                copy(
                    &inner.ctx,
                    &inner.shell.root,
                    &report.to_text(),
                    "Report copied",
                );
            }
        }
        PageAction::CopyCommand => {
            if let Some(command) = command {
                copy(&inner.ctx, &inner.shell.root, command, "Command copied");
            }
        }
        PageAction::Flow(_) => {}
    }
}

/// One failed check (C20, 72 px): its status icon, what it is and does,
/// and the fix: the command with its Copy button, the note under it.
fn check_row(inner: &Inner, row: &CheckRow, weak: &Weak<Inner>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("blocking-check");
    root.set_widget_name(&format!("blocking-check-{}", row.id));
    let status = match row.status {
        CheckStatus::Missing => icon(
            "qrec-circle-x-symbolic",
            &["blocking-status", "record-text"],
        ),
        CheckStatus::Optional => icon(
            "qrec-triangle-alert-symbolic",
            &["blocking-status", "warning-text"],
        ),
    };
    status.set_valign(gtk::Align::Center);
    root.append(&status);
    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    columns.add_css_class("blocking-columns");
    columns.set_hexpand(true);
    columns.set_valign(gtk::Align::Center);
    let what = gtk::Box::new(gtk::Orientation::Vertical, 0);
    what.add_css_class("blocking-what");
    what.set_hexpand(true);
    what.set_valign(gtk::Align::Center);
    let item = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    item.add_css_class("blocking-item");
    item.set_halign(gtk::Align::Start);
    for part in &row.item {
        let label = match part {
            ItemPart::Mono(text) => plain(text, &["blocking-name", "t-mono"]),
            ItemPart::Text(text) => plain(text, &["blocking-name", "t-body-strong"]),
        };
        label.set_valign(gtk::Align::Center);
        item.append(&label);
    }
    if row.status == CheckStatus::Optional {
        let chip = Chip::new("Optional", ChipLevel::Neutral);
        item.append(&chip.widget());
    }
    what.append(&item);
    let description = wrapping(&["blocking-description", "t-caption", "muted"]);
    description.set_text(&row.description);
    what.append(&description);
    columns.append(&what);
    let fix = gtk::Box::new(gtk::Orientation::Vertical, 0);
    fix.add_css_class("blocking-fix");
    fix.set_valign(gtk::Align::Center);
    if let Some(command) = &row.command {
        let well = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        well.add_css_class("blocking-command");
        let text = plain(command, &["t-mono-sm"]);
        text.set_hexpand(true);
        text.set_xalign(0.0);
        text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.set_max_width_chars(1);
        well.append(&text);
        let keymap = inner.ctx.store.keymap();
        let copy_button = button::icon(
            "qrec-copy-symbolic",
            &format!("Copy {command}"),
            None,
            &keymap,
        );
        copy_button.remove_css_class("icon-32");
        copy_button.add_css_class("icon-24");
        copy_button.set_valign(gtk::Align::Center);
        copy_button.set_focus_on_click(true);
        copy_button.set_widget_name(&format!("blocking-copy-{}", row.id));
        let (weak, command) = (weak.clone(), command.clone());
        copy_button.connect_clicked(guarded!("S23: a row's Copy", move |_| {
            if let Some(inner) = weak.upgrade() {
                on_own(&inner, PageAction::CopyCommand, Some(&command));
            }
        }));
        well.append(&copy_button);
        fix.append(&well);
    }
    if let Some(note) = &row.note {
        let label = wrapping(&["blocking-note", "t-caption", "muted"]);
        label.set_text(note);
        fix.append(&label);
    }
    columns.append(&fix);
    root.append(&columns);
    root.upcast()
}

/// "✓ 14 other checks passed": a disclosure; open, the checks that passed
/// (each id and what the doctor found; not drawn: the frame draws it
/// closed).
fn passed(page: &MissingDepsPage) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let toggle = gtk::Button::new();
    toggle.add_css_class("qrec-btn");
    toggle.add_css_class("flat");
    toggle.add_css_class("blocking-disclosure");
    toggle.set_widget_name("blocking-passed-toggle");
    toggle.set_focus_on_click(true);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&icon(
        "qrec-circle-check-symbolic",
        &["blocking-status", "success-text"],
    ));
    let label = plain(&page.passed_label, &["t-body"]);
    label.set_hexpand(true);
    label.set_xalign(0.0);
    content.append(&label);
    let chevron = icon(
        "qrec-chevron-right-symbolic",
        &["blocking-chevron", "muted"],
    );
    content.append(&chevron);
    toggle.set_child(Some(&content));
    toggle.update_property(&[gtk::accessible::Property::Label(&page.passed_label)]);
    toggle.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.set_widget_name("blocking-passed");
    list.add_css_class("blocking-passed");
    for check in &page.passed {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("blocking-passed-row");
        row.append(&icon(
            "qrec-circle-check-symbolic",
            &["blocking-passed-icon", "success-text"],
        ));
        row.append(&plain(&check.id, &["t-mono-sm"]));
        let detail = plain(&check.detail, &["t-caption", "muted"]);
        detail.set_hexpand(true);
        detail.set_xalign(0.0);
        detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
        detail.set_max_width_chars(1);
        row.append(&detail);
        list.append(&row);
    }
    list.set_visible(false);
    root.append(&toggle);
    root.append(&list);
    toggle.connect_clicked(guarded!("S23: the passed checks", move |toggle| {
        let open = !list.is_visible();
        list.set_visible(open);
        chevron.set_icon_name(Some(if open {
            "qrec-chevron-down-symbolic"
        } else {
            "qrec-chevron-right-symbolic"
        }));
        toggle.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
    }));
    root.upcast()
}

impl PageView for MissingDepsView {
    fn widget(&self) -> gtk::Widget {
        self.inner.shell.widget()
    }

    fn update(&self, _kind: &BlockKind) {
        // A new report with other failures: the rows follow `LocalUi.doctor`
        // (its subscription); the kind's text report says the same.
        render(&self.inner);
    }

    fn focus_target(&self) -> Option<gtk::Widget> {
        self.inner.focus.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use qrec_core::view::blocking::BlockingAction;
    use qrec_shell::doctor::Check;

    use super::*;

    /// S23 reads every check the doctor made, as it made them.
    #[test]
    fn the_rows_read_the_doctors_checks() {
        let report = DoctorReport {
            checks: vec![
                Check {
                    id: "gst.gtk4paintablesink".into(),
                    required: true,
                    ok: false,
                    detail: "gtk4paintablesink (the GUI preview) isn't in the GStreamer registry"
                        .into(),
                    hint: Some("install gst-plugins-rs (gtk4)".into()),
                },
                Check {
                    id: "wl-copy".into(),
                    required: false,
                    ok: true,
                    detail: "/run/current-system/sw/bin/wl-copy".into(),
                    hint: None,
                },
            ],
            niri_snippet: String::new(),
            va: None,
        };
        let views = check_views(&report);
        assert_eq!(
            views,
            [
                CheckView {
                    id: "gst.gtk4paintablesink".into(),
                    required: true,
                    ok: false,
                    detail: report.checks[0].detail.clone(),
                    hint: Some("install gst-plugins-rs (gtk4)".into()),
                },
                CheckView {
                    id: "wl-copy".into(),
                    required: false,
                    ok: true,
                    detail: "/run/current-system/sw/bin/wl-copy".into(),
                    hint: None,
                },
            ]
        );
        let page = blocking::missing_deps(&views);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.passed_label, "1 other check passed");
    }

    /// The buttons' widget names (the nested cases click them by name).
    #[test]
    fn page_buttons_are_named_for_their_action() {
        let names: Vec<String> = [
            PageAction::Flow(BlockingAction::CheckAgain),
            PageAction::Flow(BlockingAction::StopAndOpen),
            PageAction::Flow(BlockingAction::Wait),
            PageAction::Flow(BlockingAction::Quit),
            PageAction::CopyReport,
            PageAction::CopyCommand,
        ]
        .into_iter()
        .map(button_name)
        .collect();
        assert_eq!(
            names,
            [
                "blocking-check-again",
                "blocking-stop-and-open",
                "blocking-wait",
                "blocking-quit",
                "blocking-copy-report",
                "blocking-copy-command"
            ]
        );
    }
}
