//! The timeline toolbar (C14, `timeline-toolbar`): Bin (with the bin's
//! count, `b`), Snap (`n`), a divider, Autocut (`c`) and the autocut page
//! (`Shift`+`C`); on the right the clip summary and the zoom cluster
//! (zoom out, the logarithmic slider from fit to 128 px/frame, zoom in,
//! Fit `\`, the readout). Buttons with a binding run it through the key
//! router (G§3.7); the variants (Empty, Recording) come from
//! `view::timeline::toolbar`.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use qrec_core::view::timeline::ToolbarView;

use crate::widgets::button::{self, Kind};
use crate::widgets::keycap::{self, CapSize, CapVariant};
use crate::{Ctx, guarded};

/// The toolbar's widgets.
pub struct Toolbar {
    pub root: gtk::Box,
    bin: gtk::ToggleButton,
    bin_count: gtk::Label,
    snap: gtk::ToggleButton,
    autocut: gtk::Button,
    settings: gtk::Button,
    summary_text: gtk::Label,
    summary_time: gtk::Label,
    zoom_out: gtk::Button,
    slider: gtk::Scale,
    zoom_in: gtk::Button,
    fit: gtk::Button,
    readout: gtk::Label,
    updating: Rc<Cell<bool>>,
}

/// A toggle like `button::labeled`: an icon, the label, the key cap, the
/// accessible name with the key.
fn toggle(
    ctx: &Ctx,
    label: &str,
    icon: &str,
    binding: &str,
    extra: Option<&gtk::Widget>,
) -> gtk::ToggleButton {
    let keymap = ctx.store.keymap();
    let button = gtk::ToggleButton::new();
    button.add_css_class("qrec-btn");
    button.add_css_class("flat");
    button.add_css_class("tl-toggle");
    button.set_focus_on_click(false);
    // Spacing is CSS's (`.tl-content`): GTK adds `border-spacing` to a
    // box's own spacing, and only CSS px follow the UI scale.
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.add_css_class("tl-content");
    content.append(&gtk::Image::from_icon_name(icon));
    content.append(&gtk::Label::new(Some(label)));
    // The count chip and the key cap are centered like `button::labeled`'s
    // (S02 draws them 16 px tall; filling the 28 px button they stretched).
    if let Some(extra) = extra {
        extra.set_valign(gtk::Align::Center);
        content.append(extra);
    }
    let caps = button::binding_caps(&keymap, binding);
    if let Some(caps) = &caps {
        let cap = keycap::chord(caps, CapSize::Sm, CapVariant::Plain);
        cap.set_valign(gtk::Align::Center);
        // For the nested cases (G§8.4): `tl-cap-bin`, `tl-cap-snap`.
        cap.set_widget_name(&format!("tl-cap-{}", label.to_lowercase()));
        content.append(&cap);
    }
    button.set_child(Some(&content));
    button.update_property(&[gtk::accessible::Property::Label(&button::accessible_name(
        label,
        caps.as_deref(),
    ))]);
    let keys = ctx.keys.clone();
    let id = binding.to_owned();
    button.connect_clicked(guarded!("timeline: a toolbar toggle", move |_| {
        keys.run_binding(&id)
    }));
    button
}

/// A flat button running `binding`.
fn labeled(ctx: &Ctx, label: &str, icon: &str, binding: &str) -> gtk::Button {
    let b = button::labeled(
        label,
        Some(icon),
        Some(binding),
        &ctx.store.keymap(),
        Kind::Flat,
    );
    b.add_css_class("tl-button");
    let keys = ctx.keys.clone();
    let id = binding.to_owned();
    b.connect_clicked(guarded!("timeline: a toolbar button", move |_| {
        keys.run_binding(&id)
    }));
    b
}

/// An icon-only button running `binding`.
fn icon(ctx: &Ctx, icon: &str, action: &str, binding: &str) -> gtk::Button {
    let b = button::icon(icon, action, Some(binding), &ctx.store.keymap());
    b.add_css_class("tl-icon");
    let keys = ctx.keys.clone();
    let id = binding.to_owned();
    b.connect_clicked(guarded!("timeline: a toolbar icon", move |_| {
        keys.run_binding(&id)
    }));
    b
}

impl Toolbar {
    /// The toolbar; `on_slider` gets the slider's position as the person
    /// drags it.
    pub fn new(ctx: &Ctx, on_slider: impl Fn(f64) + 'static) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.set_widget_name("timeline-toolbar");
        root.add_css_class("timeline-toolbar");

        let left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        left.add_css_class("tl-group");
        left.set_valign(gtk::Align::Center);
        let bin_count = gtk::Label::new(None);
        bin_count.add_css_class("tl-count");
        bin_count.set_widget_name("tl-bin-count");
        let bin = toggle(
            ctx,
            "Bin",
            "package-x-generic-symbolic",
            "bin.toggle",
            Some(bin_count.upcast_ref()),
        );
        let snap = toggle(ctx, "Snap", "qrec-magnet-symbolic", "snapping.toggle", None);
        let divider = gtk::Separator::new(gtk::Orientation::Vertical);
        divider.add_css_class("tl-divider");
        let autocut = labeled(ctx, "Autocut", "edit-cut-symbolic", "autocut.run");
        // `autocut.preview` runs WP5's `autocut.page` (G§3.8 step 10):
        // opens the autocut page, or closes it when it shows.
        let settings = icon(
            ctx,
            "preferences-other-symbolic",
            "Autocut settings and preview",
            "autocut.preview",
        );
        settings.set_widget_name("tl-autocut-settings");
        left.append(&bin);
        left.append(&snap);
        left.append(&divider);
        left.append(&autocut);
        left.append(&settings);
        root.append(&left);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        root.append(&spacer);

        let right = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        right.add_css_class("tl-group");
        right.set_valign(gtk::Align::Center);
        let summary = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        summary.add_css_class("tl-summary");
        summary.set_valign(gtk::Align::Center);
        let summary_text = gtk::Label::new(None);
        summary_text.add_css_class("t-caption");
        summary_text.add_css_class("muted");
        summary_text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let summary_time = gtk::Label::new(None);
        summary_time.add_css_class("t-mono-sm");
        summary_time.add_css_class("muted");
        summary.append(&summary_text);
        summary.append(&summary_time);
        let zoom_out = icon(ctx, "zoom-out-symbolic", "Zoom out", "zoom.out");
        let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        slider.add_css_class("tl-zoom");
        slider.set_draw_value(false);
        slider.set_size_request(120, -1);
        slider.set_focus_on_click(false);
        slider.set_valign(gtk::Align::Center);
        slider.update_property(&[gtk::accessible::Property::Label("Timeline zoom")]);
        let zoom_in = icon(ctx, "zoom-in-symbolic", "Zoom in", "zoom.in");
        let fit = labeled(ctx, "Fit", "zoom-fit-best-symbolic", "zoom.fit");
        let readout = gtk::Label::new(Some("Fit"));
        readout.add_css_class("t-mono-xs");
        readout.add_css_class("muted");
        readout.add_css_class("tl-readout");
        readout.set_xalign(1.0);
        readout.set_width_chars(11);
        right.append(&summary);
        right.append(&zoom_out);
        right.append(&slider);
        right.append(&zoom_in);
        right.append(&fit);
        right.append(&readout);
        root.append(&right);

        let updating = Rc::new(Cell::new(false));
        let busy = Rc::clone(&updating);
        slider.connect_value_changed(guarded!("timeline: the zoom slider", move |s| {
            if !busy.get() {
                on_slider(s.value());
            }
        }));

        Self {
            root,
            bin,
            bin_count,
            snap,
            autocut,
            settings,
            summary_text,
            summary_time,
            zoom_out,
            slider,
            zoom_in,
            fit,
            readout,
            updating,
        }
    }

    /// Shows `view`.
    pub fn update(&self, view: &ToolbarView) {
        self.updating.set(true);
        if self.bin.is_active() != view.bin_open {
            self.bin.set_active(view.bin_open);
        }
        match view.bin_count {
            Some(n) => {
                self.bin_count.set_text(&n.to_string());
                self.bin_count.set_visible(true);
            }
            None => self.bin_count.set_visible(false),
        }
        if self.snap.is_active() != view.snapping {
            self.snap.set_active(view.snapping);
        }
        self.autocut.set_sensitive(view.autocut.is_ok());
        match &view.autocut {
            Ok(()) => self.autocut.set_tooltip_text(None),
            Err(why) => self.autocut.set_tooltip_text(Some(why)),
        }
        self.settings.set_sensitive(view.autocut_settings);
        self.summary_text.set_text(&view.summary.0);
        self.summary_time.set_text(&view.summary.1);
        self.summary_time.set_visible(!view.summary.1.is_empty());
        self.zoom_out.set_sensitive(view.zoom && view.zoom_out);
        self.zoom_in.set_sensitive(view.zoom && view.zoom_in);
        self.slider.set_sensitive(view.zoom);
        self.fit.set_sensitive(view.fit);
        self.readout.set_sensitive(view.zoom);
        self.readout.set_text(&view.readout);
        if (self.slider.value() - view.slider).abs() > 1e-4 {
            self.slider.set_value(view.slider);
        }
        self.updating.set(false);
    }
}
