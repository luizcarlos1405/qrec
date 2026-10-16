//! The header (C04, C05, the record button, the export slot, the narrow
//! layout's layers button; G§4.4). Owner: WP6.
//!
//! A 48 px `surface` bar with a bottom hairline (BRIEF §2.3), built on
//! `adw::HeaderBar` so the window can be dragged by it:
//!
//! - start: the 24 px app icon at x 12, the "Qrec" wordmark at x 44, the
//!   mode switch at (110, 8) and its `Tab` key cap;
//! - center: the project's name (`title`) over its summary, "10 clips · "
//!   in the UI face and "00:04:06:11" in the mono face, baseline-aligned
//!   (G§3.14); "Recovering take 7…" with a 12 px spinner meanwhile (D-47);
//! - end: the narrow layout's layers button, Record (120×32 at x 1240),
//!   the export slot (104×32 at 1368), Commands, Settings and close.
//!
//! Everything it shows comes from `view::header`, recomputed on every
//! snapshot and `LocalUi` change. Its buttons run their bindings through
//! the key router (G§3.7), so a click does what the key does. While a take
//! is recorded a 2 px `record` line runs under it (`.recording`).

mod export_slot;
mod mode_switch;
mod record;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use qrec_core::view::header::{HeaderView, Subtitle, header};

use crate::store::local::{LayoutWidth, LocalUi, Overlay};
use crate::theme::art::Art;
use crate::widgets::Spinner;
use crate::widgets::button;
use crate::widgets::keycap::{self, CapSize, CapVariant};
use crate::{Ctx, guarded};
use export_slot::ExportSlot;
use mode_switch::ModeSwitch;
use record::RecordButton;

/// The header region (`region-header`, 48 px).
pub struct Header {
    root: adw::HeaderBar,
}

impl Header {
    pub fn new(ctx: &Ctx) -> Self {
        let root = adw::HeaderBar::new();
        root.set_widget_name("region-header");
        root.add_css_class("qrec-header");
        root.set_height_request(48);
        // The header's own close button, not libadwaita's window controls
        // (G§3.15; BRIEF §2.3: only close, 32×32 at the right).
        root.set_show_end_title_buttons(false);
        root.set_show_start_title_buttons(false);
        root.set_centering_policy(adw::CenteringPolicy::Loose);
        let keymap = ctx.store.keymap();

        // Start: icon, wordmark, mode switch, its key.
        let start = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        start.add_css_class("header-start");
        start.set_valign(gtk::Align::Center);
        let icon = gtk::Image::new();
        icon.set_pixel_size(24);
        icon.add_css_class("app-icon");
        icon.set_valign(gtk::Align::Center);
        let show_icon = {
            let icon = icon.clone();
            move |theme: &crate::theme::Theme| match theme.art(Art::AppIcon24) {
                Some(svg) => icon.set_paintable(Some(&svg)),
                None => icon.set_icon_name(Some("qrec-record-symbolic")),
            }
        };
        show_icon(&ctx.theme);
        ctx.theme.subscribe(move |theme| show_icon(theme)).detach();
        icon.update_property(&[gtk::accessible::Property::Label("Qrec")]);
        start.append(&icon);
        let wordmark = gtk::Label::new(Some("Qrec"));
        wordmark.add_css_class("wordmark");
        wordmark.add_css_class("t-wordmark");
        wordmark.set_xalign(0.0);
        // The switch sits at x 110 whatever the wordmark's metrics (44 + 66).
        wordmark.set_width_request(66);
        wordmark.set_margin_start(8);
        start.append(&wordmark);
        let mode = ModeSwitch::new(ctx);
        start.append(&mode.widget());
        if let Some(caps) = button::binding_caps(&keymap, "canvas.mode") {
            let cap = keycap::chord(&caps, CapSize::Md, CapVariant::Plain);
            cap.set_margin_start(8);
            cap.set_valign(gtk::Align::Center);
            start.append(&cap);
        }
        root.pack_start(&start);

        // Center: the title over the summary.
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.add_css_class("header-titles");
        titles.set_valign(gtk::Align::Center);
        let title = gtk::Label::new(None);
        title.add_css_class("t-title");
        title.add_css_class("header-title");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        titles.append(&title);
        let subtitle = SubtitleRow::new();
        titles.append(&subtitle.root);
        root.set_title_widget(Some(&titles));

        // End: the layers button (narrow), Record, Export, Commands,
        // Settings, close.
        let end = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        end.add_css_class("header-end");
        end.set_valign(gtk::Align::Center);
        let layers = button::icon("qrec-layers-symbolic", "Onion layers", None, &keymap);
        layers.set_widget_name("layers-button");
        layers.set_margin_end(8);
        let store = ctx.store.clone();
        layers.connect_clicked(guarded!("header: layers", move |_| {
            store.update_local(|l| {
                if !l.close_overlays(|o| *o == Overlay::LayersSheet) {
                    l.overlays.push(Overlay::LayersSheet);
                }
            });
        }));
        layers.set_visible(false);
        end.append(&layers);
        let record = RecordButton::new(ctx);
        record.widget().set_margin_end(8);
        end.append(&record.widget());
        let export = ExportSlot::new(ctx);
        export.widget().set_margin_end(12);
        end.append(&export.widget());
        let commands = button::icon(
            "edit-find-symbolic",
            "Commands",
            Some("palette.open"),
            &keymap,
        );
        commands.set_widget_name("commands-button");
        commands.set_margin_end(4);
        let keys = ctx.keys.clone();
        commands.connect_clicked(guarded!("header: commands", move |_| {
            keys.run_binding("palette.open")
        }));
        end.append(&commands);
        // A gear, as drawn (G§1.3 named preferences-system-symbolic, which
        // Adwaita 50 draws as tools; applications-system-symbolic is its
        // gear).
        let settings = button::icon(
            "applications-system-symbolic",
            "Settings",
            Some("settings.open"),
            &keymap,
        );
        settings.set_widget_name("settings-button");
        settings.set_margin_end(8);
        let keys = ctx.keys.clone();
        settings.connect_clicked(guarded!("header: settings", move |_| {
            keys.run_binding("settings.open")
        }));
        end.append(&settings);
        end.append(&button::close_window(&keymap));
        root.pack_end(&end);

        let parts = Rc::new(Parts {
            root: root.clone(),
            mode,
            title,
            subtitle,
            record,
            export,
            layers,
            shown: RefCell::new(None),
        });
        let (ctx2, parts2) = (ctx.clone(), Rc::clone(&parts));
        ctx.store.subscribe(move |_| parts2.show(&ctx2)).detach();
        let (ctx2, parts2) = (ctx.clone(), Rc::clone(&parts));
        ctx.store
            .subscribe_local(move |_| parts2.show(&ctx2))
            .detach();
        parts.show(ctx);
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

/// The header's widgets and what they show.
struct Parts {
    root: adw::HeaderBar,
    mode: ModeSwitch,
    title: gtk::Label,
    subtitle: SubtitleRow,
    record: RecordButton,
    export: ExportSlot,
    layers: gtk::Button,
    shown: RefCell<Option<HeaderView>>,
}

impl Parts {
    /// Shows the header of the newest snapshot and `LocalUi`.
    fn show(&self, ctx: &Ctx) {
        let state = ctx.store.state();
        let keymap = ctx.store.keymap();
        let (facts, narrow) = {
            let local: std::cell::Ref<'_, LocalUi> = ctx.store.local();
            (local.facts(), local.layout.width == LayoutWidth::Narrow)
        };
        self.mode.sync(state.ui.canvas_mode);
        let view = header(&state, &facts, &keymap, narrow);
        if self.shown.borrow().as_ref() == Some(&view) {
            return;
        }
        if self.title.text() != view.title {
            self.title.set_text(&view.title);
        }
        self.subtitle.show(&view.subtitle);
        self.record.show(&view.record);
        self.export.show(&view.export);
        self.layers.set_visible(view.layers_button);
        if view.record_line {
            self.root.add_css_class("recording");
        } else {
            self.root.remove_css_class("recording");
        }
        *self.shown.borrow_mut() = Some(view);
    }
}

/// The summary under the title: two labels on one baseline (the UI face
/// and the mono face, G§3.14), or the recovery line with its spinner.
struct SubtitleRow {
    root: gtk::Box,
    spinner: Spinner,
    count: gtk::Label,
    duration: gtk::Label,
}

impl SubtitleRow {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("header-subtitle");
        root.set_halign(gtk::Align::Center);
        let spinner = Spinner::new(12);
        spinner.set_valign(gtk::Align::Center);
        spinner.set_margin_end(6);
        spinner.set_visible(false);
        let count = gtk::Label::new(None);
        count.add_css_class("t-caption");
        count.add_css_class("muted");
        count.set_valign(gtk::Align::BaselineCenter);
        let duration = gtk::Label::new(None);
        duration.add_css_class("t-mono-sm");
        duration.add_css_class("muted");
        duration.set_valign(gtk::Align::BaselineCenter);
        root.append(&spinner);
        root.append(&count);
        root.append(&duration);
        Self {
            root,
            spinner,
            count,
            duration,
        }
    }

    fn show(&self, subtitle: &Subtitle) {
        let (spin, count, duration) = match subtitle {
            Subtitle::NoClips => (false, qrec_core::view::format::NO_CLIPS, ""),
            Subtitle::Clips { count, duration } => (false, count.as_str(), duration.as_str()),
            Subtitle::Recovering { text } => (true, text.as_str(), ""),
        };
        self.spinner.set_visible(spin);
        if self.count.text() != count {
            self.count.set_text(count);
        }
        if self.duration.text() != duration {
            self.duration.set_text(duration);
        }
        self.duration.set_visible(!duration.is_empty());
    }
}

/// A tooltip whose text and keys change with the state (the record button,
/// the export slot): one `query-tooltip` handler reads the current ones.
/// Disabled controls keep theirs (S01a: Record says why it's disabled).
#[derive(Clone)]
pub(crate) struct LiveTooltip {
    current: Rc<RefCell<(String, Vec<Vec<String>>)>>,
}

impl LiveTooltip {
    pub(crate) fn attach(widget: &impl IsA<gtk::Widget>) -> Self {
        let current: Rc<RefCell<(String, Vec<Vec<String>>)>> = Rc::default();
        let widget = widget.as_ref();
        widget.set_has_tooltip(true);
        let shown = Rc::clone(&current);
        widget.connect_query_tooltip(guarded!("header: a tooltip", move |_, _, _, _, tooltip| {
            let (text, keys) = shown.borrow().clone();
            if text.is_empty() {
                return false;
            }
            // 8 px apart by CSS (`.qrec-tooltip`, common.css).
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.add_css_class("qrec-tooltip");
            row.append(&gtk::Label::new(Some(&text)));
            for chord in &keys {
                row.append(&keycap::chord(chord, CapSize::Sm, CapVariant::Plain));
            }
            tooltip.set_custom(Some(&row));
            true
        }));
        Self { current }
    }

    /// Shows `text`, with `keys` after it.
    pub(crate) fn set(&self, text: &str, keys: &[Vec<String>]) {
        *self.current.borrow_mut() = (text.to_owned(), keys.to_vec());
    }
}
