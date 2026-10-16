//! The header (C04, C05, the record button, the export slot, the narrow
//! layout's layers button; G§4.4). Owner: WP6.
//!
//! Until WP6 lands this is WP0b's minimal header (C04 `Xba2H`): the
//! project's name over its `clips_summary`, which follows every snapshot;
//! the Commands button (the "Not built yet" toast until WP9, G§1.1); and,
//! in the Narrow layout, the layers button that toggles the panel's
//! overlay sheet (G§3.15).

use std::rc::Rc;

use adw::prelude::*;
use qrec_core::State;
use qrec_core::view::format;

use crate::store::local::{LayoutWidth, Overlay};
use crate::widgets::button;
use crate::{Ctx, guarded};

/// The header region (`region-header`, 48 px).
pub struct Header {
    root: adw::HeaderBar,
}

impl Header {
    pub fn new(ctx: &Ctx) -> Self {
        let root = adw::HeaderBar::new();
        root.set_widget_name("region-header");
        root.add_css_class("qrec-minimal-header");
        root.set_height_request(48);
        // The header's own close button, not libadwaita's window controls
        // (G§3.15; BRIEF §2.3: only close, 32×32 at the right).
        root.set_show_end_title_buttons(false);
        root.set_show_start_title_buttons(false);
        let keymap = ctx.store.keymap();

        let wordmark = gtk::Label::new(Some("Qrec"));
        wordmark.add_css_class("wordmark");
        wordmark.add_css_class("t-wordmark");
        root.pack_start(&wordmark);

        let title = adw::WindowTitle::new("", "");
        root.set_title_widget(Some(&title));

        root.pack_end(&button::close_window(&keymap));

        let commands = button::icon(
            "edit-find-symbolic",
            "Commands",
            Some("palette.open"),
            &keymap,
        );
        let keys = ctx.keys.clone();
        commands.connect_clicked(guarded!("header: commands", move |_| {
            keys.run_binding("palette.open")
        }));
        root.pack_end(&commands);

        let layers = button::icon("qrec-layers-symbolic", "Onion layers", None, &keymap);
        layers.set_widget_name("layers-button");
        let store = ctx.store.clone();
        layers.connect_clicked(guarded!("header: layers", move |_| {
            tracing::debug!("header: the layers button toggles the sheet");
            store.update_local(|l| {
                if !l.close_overlays(|o| *o == Overlay::LayersSheet) {
                    l.overlays.push(Overlay::LayersSheet);
                }
            });
        }));
        layers.set_visible(false);
        root.pack_end(&layers);

        let show = {
            let title = title.clone();
            move |state: &State| {
                title.set_title(&state.project.name);
                let subtitle = match format::clips_summary(
                    &state.project.timeline.clips,
                    state.project.canvas.fps,
                ) {
                    Some((count, length)) => format!("{count}{length}"),
                    None => "No clips yet".to_owned(),
                };
                title.set_subtitle(&subtitle);
            }
        };
        show(&ctx.store.state());
        let show = Rc::new(show);
        ctx.store
            .subscribe(move |change| {
                if change.timeline() || change.prev.project.name != change.next.project.name {
                    show(&change.next);
                }
            })
            .detach();
        ctx.store
            .subscribe_local(move |local| {
                layers.set_visible(local.layout.width == LayoutWidth::Narrow);
            })
            .detach();
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
