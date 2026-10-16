//! The stage (S01a–S13, C06–C08): the preview, its overlays, the
//! countdown, the transport row, the first run, the portal-denied layer
//! (gated). Owner: WP4.
//!
//! Until WP4 lands this is WP0b's stub: the region (`region-stage`) with
//! the media hub's picture (Live: the live sink's paintable, G§3.9;
//! Program: the program sink) letterboxed where the preview goes, and the
//! caption "Opening <name>…" while step 7 is slow (G§3.3).

use gtk::prelude::*;
use qrec_core::state::CanvasMode;

use crate::Ctx;
use crate::store::local::StartupPhase;

/// The stage region (`region-stage`).
pub struct Stage {
    root: gtk::Overlay,
}

impl Stage {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Overlay::new();
        root.set_widget_name("region-stage");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_focusable(true);
        root.add_css_class("qrec-stub");

        // The preview area of G§3.15: (16, 16) to (w − 16, h − 52).
        let picture = gtk::Picture::for_paintable(&ctx.media.live_paintable());
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        picture.set_margin_start(16);
        picture.set_margin_end(16);
        picture.set_margin_top(16);
        picture.set_margin_bottom(52);
        root.set_child(Some(&picture));

        let label = gtk::Label::new(Some("Stage (WP4)"));
        label.add_css_class("qrec-stub-label");
        label.set_halign(gtk::Align::Start);
        label.set_valign(gtk::Align::End);
        label.set_margin_start(16);
        label.set_margin_bottom(16);
        root.add_overlay(&label);

        let caption = gtk::Label::new(None);
        caption.set_halign(gtk::Align::Center);
        caption.set_valign(gtk::Align::Center);
        caption.set_visible(false);
        root.add_overlay(&caption);

        let media = ctx.media.clone();
        let show_mode = move |mode: CanvasMode| {
            let paintable = match mode {
                CanvasMode::Live => media.live_paintable(),
                CanvasMode::Program => media.program_paintable(),
            };
            picture.set_paintable(Some(&paintable));
        };
        show_mode(ctx.store.state().ui.canvas_mode);
        ctx.store
            .subscribe(move |change| {
                if change.prev.ui.canvas_mode != change.next.ui.canvas_mode {
                    show_mode(change.next.ui.canvas_mode);
                }
            })
            .detach();
        let store = ctx.store.clone();
        ctx.store
            .subscribe_local(move |local| {
                let slow = local.startup == StartupPhase::Opening { slow: true };
                if slow {
                    caption.set_text(&format!("Opening {}…", store.state().project.name));
                }
                caption.set_visible(slow);
            })
            .detach();
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}
