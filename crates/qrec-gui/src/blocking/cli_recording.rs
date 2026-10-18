//! The drafted X1 (a take is being recorded from the CLI), as the approved
//! frame draws it (WP8 after G§0.2): a modal at GUI start, C24 in the
//! window's modal layer over its scrim (to y 968: the minimal header dims
//! under it, the hint bar stays live), centered above the hint bar, over
//! an empty page. The `display-sm` title, the lead with the take's time,
//! the body, the well (the static REC badge, the take, the project, the
//! time) and the footer: Quit (secondary, focused: `Enter` doesn't stop
//! someone's take) and Stop & open (primary; "Stopping…", loading, once
//! pressed: D-29). `Esc` quits (exit 6) and the recording goes on. The time
//! follows the takeover's `hello` every second (G§3.4.3).
//!
//! The modal ([`ModalView`]) is D-29's export wait's too (`export_wait.rs`).
//! D-29's idle-host variant has no page: the window asks the idle host to
//! let go silently and the stage says "Opening <name>…" after 1 s (G§3.3).
//! Content: `view::blocking::{cli_recording, export_wait}`. Look:
//! `components/blocking.css`.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use qrec_core::view::blocking::{self, BlockKind, BlockingAction, ModalPage, PageAction, Well};

use super::missing_deps::{
    OnPress, focused_button, icon, page_buttons, plain, show_button, wrapping,
};
use super::not_my_swamp::project_shown;
use super::{OnAction, PageView};
use crate::window::layers::Layer;
use crate::{Ctx, guarded};

/// The drafted page for `kind` (X1), or `None`.
pub fn view(ctx: &Ctx, kind: &BlockKind, on_action: OnAction) -> Option<Box<dyn PageView>> {
    ModalView::new(ctx, kind, recording, on_action).map(|v| Box::new(v) as Box<dyn PageView>)
}

/// X1's content for `kind`.
fn recording(ctx: &Ctx, kind: &BlockKind) -> Option<ModalPage> {
    match kind {
        BlockKind::CliRecording {
            take,
            elapsed_secs,
            stopping,
        } => Some(blocking::cli_recording(
            *take,
            *elapsed_secs,
            *stopping,
            &project_shown(ctx),
        )),
        _ => None,
    }
}

/// What a modal page shows for a kind (`None`: not its kind).
pub(super) type ModalContent = fn(&Ctx, &BlockKind) -> Option<ModalPage>;

/// X1's modal and D-29's.
pub(super) struct ModalView {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    content: ModalContent,
    /// The page under the scrim (the page slot's: empty `bg`).
    root: gtk::Box,
    /// The modal, in the window's modal layer while the page shows.
    modal: gtk::Box,
    title: gtk::Label,
    lead: gtk::Label,
    body: gtk::Label,
    well: WellParts,
    buttons: Vec<(PageAction, gtk::Button)>,
    focus: Option<gtk::Widget>,
}

/// The well's parts that change.
enum WellParts {
    Recording {
        take: gtk::Label,
        project: gtk::Label,
        elapsed: gtk::Label,
    },
    Export {
        file: gtk::Label,
        percent: gtk::Label,
        bar: gtk::ProgressBar,
    },
}

impl ModalView {
    pub(super) fn new(
        ctx: &Ctx,
        kind: &BlockKind,
        content: ModalContent,
        on_action: OnAction,
    ) -> Option<Self> {
        let page = content(ctx, kind)?;
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("blocking-modal-page");
        root.add_css_class("qrec-blocking-page");
        root.set_hexpand(true);
        root.set_vexpand(true);
        let modal = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::AlertDialog)
            .build();
        modal.set_widget_name("blocking-modal");
        modal.add_css_class("qrec-modal");
        modal.add_css_class("blocking-modal");
        modal.set_width_request(520);
        modal.set_halign(gtk::Align::Center);
        // Centered above the hint bar (X1: top 351 for 266 px of 968).
        modal.set_valign(gtk::Align::Center);
        modal.set_margin_bottom(32);
        let title = wrapping(&["t-display-sm", "modal-title"]);
        modal.append(&title);
        let lead = wrapping(&["t-body-strong", "modal-lead"]);
        modal.append(&lead);
        let body = wrapping(&["t-body", "modal-body"]);
        modal.append(&body);
        let (well_box, well) = well(&page.well);
        modal.append(&well_box);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        footer.add_css_class("modal-footer");
        footer.add_css_class("blocking-modal-footer");
        footer.set_halign(gtk::Align::End);
        modal.append(&footer);
        let on_press: OnPress = Rc::new(|_: PageAction| {});
        let buttons = page_buttons(ctx, &footer, &page.buttons, &on_action, &on_press);
        let focus = focused_button(&page.buttons, &buttons);
        // `Esc` quits (the hint bar's `[Esc] Quit`): the router passes it to
        // the page before the project is open.
        let keys = gtk::EventControllerKey::new();
        let quit = Rc::clone(&on_action);
        keys.connect_key_pressed(guarded!("a blocking modal: Esc", move |_, key, _, _| {
            if key == gdk::Key::Escape {
                quit(BlockingAction::Quit);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }));
        modal.add_controller(keys);
        let inner = Rc::new(Inner {
            ctx: ctx.clone(),
            content,
            root,
            modal,
            title,
            lead,
            body,
            well,
            buttons,
            focus,
        });
        show(&inner, &page);
        // The modal is in the window's modal layer while the page shows:
        // over the scrim, which dims the minimal header too (X1).
        let weak = Rc::downgrade(&inner);
        inner
            .root
            .connect_map(guarded!("a blocking modal: shown", move |_| {
                if let Some(inner) = weak.upgrade() {
                    open(&inner);
                }
            }));
        let weak = Rc::downgrade(&inner);
        inner
            .root
            .connect_unmap(guarded!("a blocking modal: gone", move |_| {
                if let Some(inner) = weak.upgrade() {
                    close(&inner);
                }
            }));
        Some(Self { inner })
    }
}

/// The well (44 px, a `field` well): X1's static REC badge, the take, the
/// project and the time; D-29's output, its percent and a mini mud bar.
fn well(well: &Well) -> (gtk::Box, WellParts) {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.set_widget_name("blocking-well");
    root.add_css_class("blocking-well");
    let parts = match well {
        Well::Recording { .. } => {
            let badge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            badge.set_widget_name("blocking-rec");
            badge.add_css_class("blocking-rec");
            badge.set_valign(gtk::Align::Center);
            badge.append(&icon("qrec-record-symbolic", &["blocking-rec-glyph"]));
            badge.append(&plain("REC", &["t-rec"]));
            root.append(&badge);
            let take = plain("", &["t-body-strong"]);
            root.append(&take);
            let project = plain("", &["t-mono-sm", "muted"]);
            project.set_hexpand(true);
            project.set_xalign(0.0);
            project.set_ellipsize(gtk::pango::EllipsizeMode::Start);
            project.set_max_width_chars(1);
            root.append(&project);
            let elapsed = plain("", &["t-mono"]);
            root.append(&elapsed);
            WellParts::Recording {
                take,
                project,
                elapsed,
            }
        }
        Well::Export { .. } => {
            let glyph = icon(
                "qrec-download-symbolic",
                &["blocking-export-glyph", "muted"],
            );
            glyph.set_valign(gtk::Align::Center);
            root.append(&glyph);
            let file = plain("", &["t-mono-sm", "muted"]);
            file.set_hexpand(true);
            file.set_xalign(0.0);
            file.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            file.set_max_width_chars(1);
            root.append(&file);
            let percent = plain("", &["t-mono"]);
            root.append(&percent);
            let bar = gtk::ProgressBar::new();
            bar.set_widget_name("blocking-mini-mud");
            bar.add_css_class("mini-mud");
            bar.set_size_request(40, 4);
            bar.set_valign(gtk::Align::Center);
            root.append(&bar);
            WellParts::Export { file, percent, bar }
        }
    };
    (root, parts)
}

/// Shows `page`'s values (the time, the percent, the buttons' state).
fn show(inner: &Inner, page: &ModalPage) {
    set(&inner.title, &page.title);
    inner
        .modal
        .update_property(&[gtk::accessible::Property::Label(&page.title)]);
    set(&inner.lead, &page.lead);
    set(&inner.body, &page.body);
    match (&inner.well, &page.well) {
        (
            WellParts::Recording {
                take,
                project,
                elapsed,
            },
            Well::Recording {
                take: t,
                project: p,
                elapsed: e,
            },
        ) => {
            set(take, t);
            set(project, p);
            set(elapsed, e);
        }
        (
            WellParts::Export { file, percent, bar },
            Well::Export {
                file: f,
                percent: p,
            },
        ) => {
            set(file, f);
            set(percent, &p.map(|p| format!("{p}%")).unwrap_or_default());
            bar.set_fraction(p.map_or(0.0, |p| f64::from(p.min(100)) / 100.0));
        }
        _ => tracing::warn!("a blocking modal's well changed its kind"),
    }
    let focus = inner.root.root().and_then(|r| r.focus());
    for (action, button) in &inner.buttons {
        if let Some(spec) = page.buttons.iter().find(|b| b.action == *action) {
            let had_focus = focus
                .as_ref()
                .is_some_and(|f| f == button.upcast_ref::<gtk::Widget>());
            show_button(button, spec);
            // A button that turns busy ("Stopping…") lets go of the focus:
            // the keyboard stays on the page's focused one (Quit).
            if had_focus
                && spec.busy
                && let Some(target) = &inner.focus
            {
                target.grab_focus();
            }
        }
    }
}

fn set(label: &gtk::Label, text: &str) {
    if label.text() != text {
        label.set_text(text);
    }
}

/// Opens the modal over its scrim (the window's modal layer) with the
/// focus on its focused button, its ring showing (the frames').
fn open(inner: &Inner) {
    if inner.modal.parent().is_some() {
        return;
    }
    inner.ctx.window.open(Layer::Modal, &inner.modal);
    // The scrim under it, for the region checks (G§8.4: it ends at y 968).
    if let Some(scrim) = inner.modal.prev_sibling() {
        scrim.set_widget_name("scrim-blocking");
    }
    if let Some(target) = &inner.focus {
        target.grab_focus();
    }
    if let Some(window) = inner.modal.root().and_downcast::<gtk::Window>() {
        window.set_focus_visible(true);
    }
}

/// Takes the modal and its scrim out (the page went: the project opened,
/// another page replaced it, the window closes).
fn close(inner: &Inner) {
    if inner.modal.parent().is_some() {
        inner.ctx.window.close(Layer::Modal);
    }
}

impl Drop for ModalView {
    fn drop(&mut self) {
        crate::guard::run("a blocking modal: dropped", || close(&self.inner));
    }
}

impl PageView for ModalView {
    fn widget(&self) -> gtk::Widget {
        self.inner.root.clone().upcast()
    }

    fn update(&self, kind: &BlockKind) {
        if let Some(page) = (self.inner.content)(&self.inner.ctx, kind) {
            show(&self.inner, &page);
        }
    }

    fn focus_target(&self) -> Option<gtk::Widget> {
        self.inner.focus.clone()
    }
}
