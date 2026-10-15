//! The modal (C24, G§4.9): confirmations (S18 purge, S18b fps change, an
//! overwrite) and quitting while exporting (D-12), over a scrim that ends
//! above the hint bar (it stays live), stacked over Settings; Cancel has
//! the initial focus. Owner: WP6 (view from `view::confirm`).
//!
//! Until WP6 lands this is WP0b's plain text modal, so the confirmations
//! and the close sequence work: it follows the top overlay
//! (`Overlay::Confirm`, `Overlay::QuitWhileExporting`).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use qrec_core::view::format;

use crate::Ctx;
use crate::store::local::{LocalUi, Overlay};
use crate::window::layers::Layer;

/// The text of the stub modal: title, lead, body, cancel and confirm
/// labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubText {
    pub title: String,
    pub lead: String,
    pub body: Option<String>,
    pub cancel: String,
    pub confirm: String,
}

/// What the stub modal says for `overlay` (`None` for other overlays).
pub fn stub_text(overlay: &Overlay) -> Option<StubText> {
    match overlay {
        Overlay::Confirm { error, .. } => Some(StubText {
            title: "Get out of my swamp?".to_owned(),
            lead: format::capitalized(&error.message),
            body: error.hint.clone(),
            cancel: "Cancel".to_owned(),
            confirm: "Continue".to_owned(),
        }),
        Overlay::QuitWhileExporting { percent } => Some(StubText {
            title: "Quit while exporting?".to_owned(),
            lead: format!("The export is {percent} % done."),
            body: Some("Quitting cancels it. Nothing is written to exports/.".to_owned()),
            cancel: "Keep exporting".to_owned(),
            confirm: "Cancel export and quit".to_owned(),
        }),
        _ => None,
    }
}

/// The modal host.
pub struct ModalHost {
    root: gtk::Box,
}

impl ModalHost {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("modal-host");
        let shown: Rc<RefCell<Option<Overlay>>> = Rc::new(RefCell::new(None));
        let (ctx2, shown2) = (ctx.clone(), Rc::clone(&shown));
        ctx.store
            .subscribe_local(move |local| follow(&ctx2, &shown2, local))
            .detach();
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

/// Opens, replaces or closes the modal for the top overlay.
fn follow(ctx: &Ctx, shown: &Rc<RefCell<Option<Overlay>>>, local: &LocalUi) {
    let top = local
        .overlays
        .last()
        .filter(|o| stub_text(o).is_some())
        .cloned();
    if *shown.borrow() == top {
        return;
    }
    *shown.borrow_mut() = top.clone();
    ctx.window.close(Layer::Modal);
    let Some(overlay) = top else {
        return;
    };
    let Some(text) = stub_text(&overlay) else {
        return;
    };
    let modal = gtk::Box::new(gtk::Orientation::Vertical, 12);
    modal.add_css_class("qrec-modal-stub");
    modal.set_width_request(520);
    modal.set_halign(gtk::Align::Center);
    modal.set_valign(gtk::Align::Center);
    let title = gtk::Label::new(Some(&text.title));
    title.add_css_class("title-2");
    title.set_xalign(0.0);
    modal.append(&title);
    let lead = gtk::Label::new(Some(&text.lead));
    lead.set_wrap(true);
    lead.set_xalign(0.0);
    modal.append(&lead);
    if let Some(body) = &text.body {
        let body = gtk::Label::new(Some(body));
        body.set_wrap(true);
        body.set_xalign(0.0);
        body.add_css_class("dim-label");
        modal.append(&body);
    }
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label(&text.cancel);
    let confirm = gtk::Button::with_label(&text.confirm);
    confirm.add_css_class("destructive-action");
    buttons.append(&cancel);
    buttons.append(&confirm);
    modal.append(&buttons);

    let store = ctx.store.clone();
    let closing = overlay.clone();
    cancel.connect_clicked(move |_| {
        let closing = closing.clone();
        store.update_local(move |l| {
            l.close_overlays(|o| *o == closing);
        });
    });
    let ctx2 = ctx.clone();
    confirm.connect_clicked(move |_| {
        let closing = overlay.clone();
        let target = closing.clone();
        ctx2.store.update_local(move |l| {
            l.close_overlays(|o| *o == target);
        });
        match closing {
            Overlay::Confirm { cmd, .. } => ctx2.store.dispatch_confirmed(cmd),
            Overlay::QuitWhileExporting { .. } => {
                ctx2.actions.run("app.cancel-export-and-quit");
            }
            _ => {}
        }
    });
    ctx.window.open(Layer::Modal, &modal);
    cancel.grab_focus();
}

#[cfg(test)]
mod tests {
    use qrec_core::Command;
    use qrec_core::{CoreError, ErrorCode, TakeId};

    use super::*;

    #[test]
    fn the_stub_modal_says_what_the_core_said() {
        let confirm = Overlay::Confirm {
            cmd: Command::TakePurge {
                takes: vec![TakeId::numbered(4)],
            },
            error: CoreError::new(ErrorCode::ConfirmationRequired, "purge take 4 from disk?")
                .with_hint("this deletes 5 files"),
        };
        let text = stub_text(&confirm).unwrap();
        assert_eq!(text.lead, "Purge take 4 from disk?");
        assert_eq!(text.body.as_deref(), Some("this deletes 5 files"));
        let quit = stub_text(&Overlay::QuitWhileExporting { percent: 42 }).unwrap();
        assert_eq!(quit.lead, "The export is 42 % done.");
        assert_eq!(quit.cancel, "Keep exporting");
        assert!(stub_text(&Overlay::Palette).is_none());
    }
}
