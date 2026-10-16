//! The export panel's pages (S15a, S15b, S15c, S16, S17) and its arrow:
//! widgets only; what they show and do is `export/mod.rs`'s.
//!
//! The panel is 440 wide: a 12×6 arrow 172 px left of its right edge
//! (pointing at the header's Export slot), over a `raised` body with a 1
//! px `border`, `lg` radius, e2 and 16 px of padding (DESIGN Popover).

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};
use qrec_core::view::export::Run;

use crate::theme::Palette;
use crate::widgets::keycap::{self, CapSize, CapVariant};
use crate::widgets::mud_bar::MudBar;
use crate::widgets::{Spinner, button};

/// The arrow's tip from the panel's right edge, and its size (BRIEF §2.8).
pub const ARROW_FROM_RIGHT: f64 = 172.0;
pub const ARROW_W: f64 = 12.0;
pub const ARROW_H: f64 = 6.0;

/// A label with type role `role` (`t-…`) and more classes.
pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// A label that wraps (a lead, a hint), never wider than the panel.
pub fn wrapping(text: &str, classes: &[&str]) -> gtk::Label {
    let label = label(text, classes);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(1);
    label.set_hexpand(true);
    label
}

/// A line of [`Run`]s: one label per run, baseline-aligned (G§3.14), the
/// UI runs with `ui`'s classes and the mono ones with `mono`'s.
pub struct RunsLine {
    pub root: gtk::Box,
    ui: Vec<&'static str>,
    mono: Vec<&'static str>,
    shown: std::cell::RefCell<Vec<Run>>,
}

impl RunsLine {
    pub fn new(ui: &[&'static str], mono: &[&'static str]) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.set_valign(gtk::Align::Center);
        Self {
            root,
            ui: ui.to_vec(),
            mono: mono.to_vec(),
            shown: std::cell::RefCell::new(Vec::new()),
        }
    }

    pub fn show(&self, runs: &[Run]) {
        if *self.shown.borrow() == runs {
            return;
        }
        *self.shown.borrow_mut() = runs.to_vec();
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        for run in runs {
            let classes = if run.mono { &self.mono } else { &self.ui };
            let l = label(&run.text, classes);
            l.set_valign(gtk::Align::Baseline);
            self.root.append(&l);
        }
    }
}

/// A button with a literal key cap after its label ("Export [Enter]"), the
/// primary of a page: Enter activates it while it has the focus (the
/// panel gives it the focus when the page shows).
pub fn primary_with_key(text: &str, key: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("qrec-btn");
    button.add_css_class("primary");
    // The panel is keyboard-first (it traps the focus, D-49): its buttons
    // take the focus on click, like a modal's.
    button.set_focus_on_click(true);
    // 8 px apart by CSS (`button.qrec-btn > box`).
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let text_label = gtk::Label::new(Some(text));
    content.append(&text_label);
    let cap = keycap::chord(&[key.to_owned()], CapSize::Sm, CapVariant::OnFill);
    cap.set_valign(gtk::Align::Center);
    content.append(&cap);
    let spinner = Spinner::new(16);
    spinner.set_visible(false);
    spinner.set_widget_name("loading-spinner");
    content.prepend(&spinner);
    button.set_child(Some(&content));
    button.update_property(&[gtk::accessible::Property::Label(&button::accessible_name(
        text,
        Some(&[key.to_owned()]),
    ))]);
    button
}

/// A page's plain button (`kind`), focus on click (keyboard-first).
pub fn plain(text: &str, icon: Option<&str>, kind: button::Kind) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("qrec-btn");
    b.add_css_class(kind.class());
    b.set_focus_on_click(true);
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

/// The loading look of a primary built by [`primary_with_key`]: the
/// spinner before the label (C01 loading), the label kept.
pub fn set_loading(button: &gtk::Button, loading: bool) {
    let Some(content) = button.child() else {
        return;
    };
    let mut child = content.first_child();
    while let Some(c) = child {
        if c.widget_name() == "loading-spinner" {
            c.set_visible(loading);
        }
        child = c.next_sibling();
    }
    if loading {
        button.add_css_class("loading");
    } else {
        button.remove_css_class("loading");
    }
}

/// The "Export" setup page (S15a, S15c).
pub struct SetupPage {
    pub root: gtk::Box,
    pub format: adw::ToggleGroup,
    pub caption: gtk::Label,
    pub av1_row: gtk::Box,
    pub av1: gtk::Switch,
    pub av1_caption: gtk::Label,
    pub quality: adw::ToggleGroup,
    pub change: gtk::Button,
    pub field: gtk::Box,
    pub output: gtk::Label,
    pub helper: gtk::Label,
    pub summary: RunsLine,
    pub export: gtk::Button,
}

/// A C28 segmented control of `(name, label)` toggles.
fn segmented(name: &str, items: &[(&str, &str)]) -> adw::ToggleGroup {
    let group = adw::ToggleGroup::new();
    group.set_widget_name(name);
    group.add_css_class("qrec-segmented");
    group.add_css_class("export-segmented");
    group.set_halign(gtk::Align::Start);
    for (id, text) in items {
        group.add(adw::Toggle::builder().name(*id).label(*text).build());
    }
    group
}

/// "Format", "Quality", "Output": a group's caption.
fn group_label(text: &str) -> gtk::Label {
    label(text, &["t-caption", "muted", "export-group-label"])
}

impl SetupPage {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("export-page");
        root.set_widget_name("export-setup");
        root.append(&label("Export", &["t-title", "export-title"]));

        root.append(&group_label("Format"));
        let format = segmented("export-format", &[("mp4", "MP4"), ("webm", "WebM")]);
        format
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label("Format")]);
        root.append(&format);
        let caption = label("", &["t-caption", "muted", "export-caption"]);
        caption.set_widget_name("export-caption");
        root.append(&caption);

        // WebM only: "Use AV1 (slower, smaller)" with its switch (S15c).
        let av1_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        av1_row.add_css_class("export-av1");
        let av1_text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        av1_text.set_hexpand(true);
        av1_text.append(&label("Use AV1 (slower, smaller)", &["t-body"]));
        let av1_caption = label("", &["t-caption", "muted"]);
        av1_text.append(&av1_caption);
        av1_row.append(&av1_text);
        let av1 = gtk::Switch::new();
        av1.set_widget_name("export-av1");
        av1.set_valign(gtk::Align::Center);
        av1.update_property(&[gtk::accessible::Property::Label(
            "Use AV1 (slower, smaller)",
        )]);
        av1_row.append(&av1);
        root.append(&av1_row);

        root.append(&group_label("Quality"));
        let quality = segmented(
            "export-quality",
            &[
                ("draft", "Draft"),
                ("standard", "Standard"),
                ("high", "High"),
            ],
        );
        quality
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label("Quality")]);
        root.append(&quality);

        let output_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        output_row.add_css_class("export-output-row");
        let output_label = group_label("Output");
        output_label.set_hexpand(true);
        output_label.set_valign(gtk::Align::Center);
        output_row.append(&output_label);
        let change = plain("Change\u{2026}", None, button::Kind::Flat);
        change.set_widget_name("export-change");
        change.add_css_class("sm");
        output_row.append(&change);
        root.append(&output_row);
        let field = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        field.add_css_class("field");
        field.add_css_class("export-field");
        field.set_widget_name("export-output");
        let output = label("", &["export-output"]);
        output.set_hexpand(true);
        output.set_valign(gtk::Align::Center);
        output.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        output.set_max_width_chars(1);
        output.set_selectable(false);
        field.append(&output);
        root.append(&field);
        let helper = wrapping("", &["field-helper", "export-helper"]);
        helper.set_widget_name("export-output-error");
        helper.set_visible(false);
        root.append(&helper);

        let rule = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        rule.add_css_class("export-rule");
        root.append(&rule);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        footer.add_css_class("export-footer");
        let summary = RunsLine::new(&["t-caption", "muted"], &["t-mono-sm", "muted"]);
        summary.root.set_hexpand(true);
        summary.root.set_widget_name("export-summary");
        footer.append(&summary.root);
        let export = primary_with_key("Export", "Enter");
        export.set_widget_name("export-start");
        footer.append(&export);
        root.append(&footer);
        Self {
            root,
            format,
            caption,
            av1_row,
            av1,
            av1_caption,
            quality,
            change,
            field,
            output,
            helper,
            summary,
            export,
        }
    }
}

/// The running page (S15b): passive (D-49).
pub struct RunningPage {
    pub root: gtk::Box,
    pub file: gtk::Label,
    pub percent: gtk::Label,
    pub label: gtk::Label,
    pub frames: gtk::Label,
    pub bar: MudBar,
    pub times: gtk::Label,
    pub note: gtk::Label,
    pub cancel: gtk::Button,
}

impl RunningPage {
    pub fn new(theme: &crate::theme::Theme) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("export-page");
        root.set_widget_name("export-running");
        root.append(&label("Exporting", &["t-title", "export-title"]));
        let file = label("", &["t-mono-sm", "muted", "export-file"]);
        file.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        file.set_max_width_chars(1);
        root.append(&file);
        let progress = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        progress.add_css_class("export-progress");
        let percent = label("", &["t-mono-xl", "export-percent"]);
        percent.set_widget_name("export-percent");
        percent.set_valign(gtk::Align::Center);
        progress.append(&percent);
        let phase = gtk::Box::new(gtk::Orientation::Vertical, 0);
        phase.set_valign(gtk::Align::Center);
        let phase_label = label("", &["t-body"]);
        phase_label.set_widget_name("export-phase");
        phase.append(&phase_label);
        let frames = label("", &["t-mono-sm", "muted"]);
        phase.append(&frames);
        progress.append(&phase);
        root.append(&progress);
        let bar = MudBar::new(theme);
        bar.set_widget_name("export-bar");
        bar.add_css_class("export-bar");
        root.append(&bar);
        let times = label("", &["t-mono-sm", "export-times"]);
        root.append(&times);
        let note = wrapping("", &["t-caption", "muted", "export-note"]);
        root.append(&note);
        let cancel = plain("Cancel export", None, button::Kind::Secondary);
        cancel.set_widget_name("export-cancel");
        cancel.set_halign(gtk::Align::End);
        cancel.add_css_class("export-cancel");
        root.append(&cancel);
        Self {
            root,
            file,
            percent,
            label: phase_label,
            frames,
            bar,
            times,
            note,
            cancel,
        }
    }
}

/// A page's title row: a 20 px status icon and a `headline`.
fn status_title(icon: &str, icon_class: &str, text: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("export-status");
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(20);
    image.add_css_class(icon_class);
    image.set_valign(gtk::Align::Center);
    row.append(&image);
    let title = label(text, &["t-headline"]);
    title.set_valign(gtk::Align::Center);
    row.append(&title);
    row
}

/// The done page (S16).
pub struct DonePage {
    pub root: gtk::Box,
    pub path: gtk::Label,
    pub details: RunsLine,
    pub bar: MudBar,
    pub copy: gtk::Button,
    pub folder: gtk::Button,
    pub open: gtk::Button,
}

impl DonePage {
    pub fn new(theme: &crate::theme::Theme) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("export-page");
        root.set_widget_name("export-done");
        root.append(&status_title(
            "qrec-success-symbolic",
            "success-text",
            "Export finished",
        ));
        root.append(&wrapping(
            qrec_core::view::export::DONE_LINE,
            &["t-body", "export-line"],
        ));
        let path = label("", &["t-mono-sm", "export-path"]);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        path.set_max_width_chars(1);
        root.append(&path);
        let details = RunsLine::new(&["t-caption", "muted"], &["t-mono-sm", "muted"]);
        root.append(&details.root);
        let bar = MudBar::new(theme);
        bar.add_css_class("export-bar");
        root.append(&bar);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        buttons.add_css_class("export-buttons");
        buttons.set_halign(gtk::Align::End);
        let copy = plain("Copy path", None, button::Kind::Secondary);
        copy.set_widget_name("export-copy-path");
        let folder = plain("Open folder", None, button::Kind::Secondary);
        folder.set_widget_name("export-open-folder");
        let open = primary_with_key("Open file", "Enter");
        open.set_widget_name("export-open-file");
        buttons.append(&copy);
        buttons.append(&folder);
        buttons.append(&open);
        root.append(&buttons);
        Self {
            root,
            path,
            details,
            bar,
            copy,
            folder,
            open,
        }
    }
}

/// The failed page (S17).
pub struct FailedPage {
    pub root: gtk::Box,
    pub lead: gtk::Label,
    pub hint: gtk::Label,
    pub bar: MudBar,
    pub at: gtk::Label,
    pub stderr: gtk::Label,
    pub scroller: gtk::ScrolledWindow,
    pub log_row: gtk::Box,
    pub log_link: gtk::Button,
    pub log_path: gtk::Label,
    pub copy: gtk::Button,
    pub close: gtk::Button,
    pub retry: gtk::Button,
}

/// How many stderr lines show before the box scrolls (BRIEF S17).
pub const STDERR_VISIBLE: i32 = 10;

impl FailedPage {
    pub fn new(theme: &crate::theme::Theme) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("export-page");
        root.set_widget_name("export-failed");
        root.append(&status_title(
            "dialog-error-symbolic",
            "record-text",
            "Export failed",
        ));
        let lead = wrapping("", &["t-body", "export-lead"]);
        lead.set_widget_name("export-lead");
        root.append(&lead);
        let hint = wrapping("", &["t-body-sm", "muted", "export-hint"]);
        hint.set_widget_name("export-hint");
        root.append(&hint);
        let bar = MudBar::new(theme);
        bar.add_css_class("export-bar");
        root.append(&bar);
        let at = label("", &["t-body-sm", "record-text", "export-failed-at"]);
        root.append(&at);
        let stderr = label("", &["t-mono-sm", "export-stderr-text"]);
        stderr.set_wrap(true);
        stderr.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        stderr.set_max_width_chars(1);
        stderr.set_selectable(true);
        stderr.set_valign(gtk::Align::End);
        stderr.set_yalign(1.0);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_widget_name("export-stderr");
        scroller.add_css_class("export-stderr");
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_propagate_natural_height(true);
        scroller.set_overlay_scrolling(false);
        scroller.set_child(Some(&stderr));
        root.append(&scroller);
        let log_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        log_row.add_css_class("export-log");
        let log_link = gtk::Button::with_label("Open full log");
        log_link.add_css_class("qrec-link");
        log_link.set_widget_name("export-open-log");
        log_link.set_focus_on_click(true);
        log_row.append(&log_link);
        let log_path = label("", &["t-mono-sm", "muted"]);
        log_path.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        log_path.set_max_width_chars(1);
        log_path.set_hexpand(true);
        log_path.set_valign(gtk::Align::Center);
        log_row.append(&log_path);
        root.append(&log_row);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        buttons.add_css_class("export-buttons");
        let copy = plain("Copy error", None, button::Kind::Secondary);
        copy.set_widget_name("export-copy-error");
        copy.set_hexpand(true);
        copy.set_halign(gtk::Align::Start);
        let close = plain("Close", None, button::Kind::Flat);
        close.set_widget_name("export-close");
        let retry = primary_with_key("Try again", "Enter");
        retry.set_widget_name("export-retry");
        buttons.append(&copy);
        buttons.append(&close);
        buttons.append(&retry);
        root.append(&buttons);
        Self {
            root,
            lead,
            hint,
            bar,
            at,
            stderr,
            scroller,
            log_row,
            log_link,
            log_path,
            copy,
            close,
            retry,
        }
    }
}

mod imp {
    use std::cell::{Cell, RefCell};

    use super::*;

    pub struct Arrow {
        pub fill: RefCell<gtk::gdk::RGBA>,
        pub stroke: RefCell<gtk::gdk::RGBA>,
        pub scale: Cell<f64>,
    }

    impl Default for Arrow {
        fn default() -> Self {
            let p = Palette::default();
            Self {
                fill: RefCell::new(p.raised),
                stroke: RefCell::new(p.border),
                scale: Cell::new(1.0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Arrow {
        const NAME: &'static str = "QrecExportArrow";
        type Type = super::Arrow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("export-arrow");
        }
    }

    impl ObjectImpl for Arrow {}

    impl WidgetImpl for Arrow {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            crate::guard::run("export arrow: measure", || match orientation {
                gtk::Orientation::Vertical => {
                    let h = ((ARROW_H + 1.0) * self.scale.get()).ceil() as i32;
                    (h, h, -1, -1)
                }
                _ => (0, 0, -1, -1),
            })
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            crate::guard::run("export arrow: snapshot", || {
                let obj = self.obj();
                let s = self.scale.get();
                let w = f64::from(obj.width());
                let tip = (w - ARROW_FROM_RIGHT * s).round() + 0.5;
                let (half, h) = (ARROW_W / 2.0 * s, ARROW_H * s);
                let builder = gsk::PathBuilder::new();
                builder.move_to((tip - half) as f32, (h + 1.0) as f32);
                builder.line_to(tip as f32, 0.0);
                builder.line_to((tip + half) as f32, (h + 1.0) as f32);
                builder.close();
                let path = builder.to_path();
                snapshot.append_fill(&path, gsk::FillRule::Winding, &self.fill.borrow());
                let sides = gsk::PathBuilder::new();
                sides.move_to((tip - half) as f32, (h + 0.5) as f32);
                sides.line_to(tip as f32, 0.5);
                sides.line_to((tip + half) as f32, (h + 0.5) as f32);
                let stroke = gsk::Stroke::new(s.max(1.0) as f32);
                snapshot.append_stroke(&sides.to_path(), &stroke, &self.stroke.borrow());
                // The body's top border under the base stays open.
                snapshot.append_color(
                    &self.fill.borrow(),
                    &graphene::Rect::new(
                        (tip - half + 1.0) as f32,
                        h as f32,
                        (2.0 * half - 2.0).max(0.0) as f32,
                        s.max(1.0) as f32 + 0.5,
                    ),
                );
            });
        }
    }
}

glib::wrapper! {
    /// The panel's 12×6 arrow (C25), over the body's top edge.
    pub struct Arrow(ObjectSubclass<imp::Arrow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Arrow {
    pub fn new(theme: &crate::theme::Theme) -> Self {
        let arrow: Self = glib::Object::new();
        arrow.set_can_target(false);
        arrow.set_valign(gtk::Align::Start);
        arrow.follow(theme);
        let weak = arrow.downgrade();
        theme
            .subscribe(move |theme| {
                if let Some(arrow) = weak.upgrade() {
                    arrow.follow(theme);
                }
            })
            .detach();
        arrow
    }

    fn follow(&self, theme: &crate::theme::Theme) {
        let p = theme.palette();
        *self.imp().fill.borrow_mut() = p.raised;
        *self.imp().stroke.borrow_mut() = p.border;
        self.imp().scale.set(theme.scale());
        self.queue_resize();
    }
}
