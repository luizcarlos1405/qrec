//! The modal (C24, G§4.9): confirmations (S18 purge, S18b fps change, an
//! overwrite, clearing the history) and quitting while exporting (D-12),
//! over a scrim that ends above the hint bar (it stays live), stacked over
//! Settings; Cancel has the initial focus. Owner: WP6.
//!
//! It follows the top overlay (`Overlay::Confirm`, `QuitWhileExporting`):
//! its content is `view::confirm`'s. Confirming sends the command again
//! with `confirmed: true` (the core checks again: a confirmation isn't a
//! lock) and closes it: `Store::dispatch_confirmed`, which hands it to the
//! region that asked when that one follows the answer itself
//! (`Store::ask_confirmation`: the export panel's Replace modal, D-24; WP7
//! integration). Cancel, `Esc` (the key router closes the top overlay) and
//! `Enter` right after it opens (Cancel has the focus) dispatch nothing.
//!
//! 520 wide, `raised`, `xl` radius, e3 with a 1 px `border`, 24 px padding:
//! the `display-sm` title, the `body-strong` lead, the `body` details, the
//! loss list (a `field` well, `mono-sm` rows with right-aligned sizes, 8
//! rows visible, then it scrolls) and the footer's large buttons, Cancel
//! (secondary) then the destructive one. Centered on the window like the
//! frames (S18, S18b: y 277 to 723 at 1000 high), not on the 968 px above
//! the hint bar: the content layer spans the window.
//!
//! An overwrite names a file the core can't describe: the GUI shows its
//! path for people and the time it was modified (stat on the io worker,
//! the body updates when it answers).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use chrono::{DateTime, FixedOffset, Local};
use gtk::prelude::*;
use qrec_core::view::confirm::{self, ConfirmFacts, ModalContent, SeenFile};
use qrec_core::view::format;
use qrec_core::{Command, CoreError};
use serde_json::Value;

use crate::store::local::{LocalUi, Overlay};
use crate::widgets::button;
use crate::window::layers::Layer;
use crate::{Ctx, guarded};

/// The modal host.
pub struct ModalHost {
    root: gtk::Box,
}

impl ModalHost {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("modal-host");
        let shown: Rc<RefCell<Option<Shown>>> = Rc::new(RefCell::new(None));
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

/// The modal on screen: its overlay and its body label (an overwrite's
/// facts arrive later).
struct Shown {
    overlay: Overlay,
    body: gtk::Label,
}

/// The modal an overlay asks for, if it is one.
fn is_modal(overlay: &Overlay) -> bool {
    matches!(
        overlay,
        Overlay::Confirm { .. } | Overlay::QuitWhileExporting { .. }
    )
}

/// The content for a modal overlay with what the GUI saw (`facts`).
pub fn content_for(ctx: &Ctx, overlay: &Overlay, facts: &ConfirmFacts) -> Option<ModalContent> {
    match overlay {
        Overlay::Confirm { cmd, error } => {
            Some(confirm::modal(cmd, error, &ctx.store.state(), facts))
        }
        Overlay::QuitWhileExporting { percent } => Some(confirm::quit_while_exporting(*percent)),
        _ => None,
    }
}

/// Opens, replaces or closes the modal for the top overlay.
fn follow(ctx: &Ctx, shown: &Rc<RefCell<Option<Shown>>>, local: &LocalUi) {
    let top = local.overlays.last().filter(|o| is_modal(o)).cloned();
    if shown.borrow().as_ref().map(|s| &s.overlay) == top.as_ref() {
        return;
    }
    *shown.borrow_mut() = None;
    ctx.window.close(Layer::Modal);
    let Some(overlay) = top else {
        return;
    };
    let facts = initial_facts(ctx, &overlay);
    let Some(content) = content_for(ctx, &overlay, &facts) else {
        return;
    };
    let view = build(ctx, &overlay, &content);
    ctx.window.open(Layer::Modal, &view.root);
    // The scrim under it, for the region checks (G§8.4: it ends at y 968).
    if let Some(scrim) = view.root.prev_sibling() {
        scrim.set_widget_name("scrim-modal");
    }
    view.cancel.grab_focus();
    // The focused Cancel shows its ring (S18): keyboard focus is the
    // point of a confirmation.
    if let Some(window) = view.root.root().and_downcast::<gtk::Window>() {
        window.set_focus_visible(true);
    }
    *shown.borrow_mut() = Some(Shown {
        overlay: overlay.clone(),
        body: view.body.clone(),
    });
    stat_files(ctx, shown, &overlay, facts);
}

/// The modal's widgets.
struct ModalView {
    root: gtk::Box,
    body: gtk::Label,
    cancel: gtk::Button,
}

fn build(ctx: &Ctx, overlay: &Overlay, content: &ModalContent) -> ModalView {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .accessible_role(gtk::AccessibleRole::AlertDialog)
        .build();
    root.set_widget_name("modal");
    root.add_css_class("qrec-modal");
    root.set_width_request(520);
    root.set_halign(gtk::Align::Center);
    // Centered on the window (S18: top 277 for 446 px): the content layer
    // spans it, the hint bar too (window/mod.rs).
    root.set_valign(gtk::Align::Center);
    root.update_property(&[gtk::accessible::Property::Label(&content.title)]);

    let title = label(&content.title, &["t-display-sm", "modal-title"]);
    root.append(&title);
    let lead = label(&content.lead, &["t-body-strong", "modal-lead"]);
    lead.set_wrap(true);
    root.append(&lead);
    let body = label(
        content.body.as_deref().unwrap_or_default(),
        &["t-body", "modal-body"],
    );
    body.set_wrap(true);
    body.set_visible(content.body.is_some());
    root.append(&body);
    if !content.list.is_empty() {
        root.append(&loss_list(&content.list));
    }

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    footer.add_css_class("modal-footer");
    footer.set_halign(gtk::Align::End);
    let keymap = ctx.store.keymap();
    let cancel = button::labeled(
        &content.cancel,
        None,
        None,
        &keymap,
        button::Kind::Secondary,
    );
    cancel.add_css_class("lg");
    cancel.set_widget_name("modal-cancel");
    // A modal is keyboard-first: its buttons take the focus on click.
    cancel.set_focus_on_click(true);
    let confirm = button::labeled(
        &content.confirm,
        None,
        None,
        &keymap,
        button::Kind::Destructive,
    );
    confirm.set_widget_name("modal-confirm");
    confirm.set_focus_on_click(true);
    footer.append(&cancel);
    footer.append(&confirm);
    root.append(&footer);

    let store = ctx.store.clone();
    let closing = overlay.clone();
    cancel.connect_clicked(guarded!("modal: cancel", move |_| {
        let closing = closing.clone();
        store.update_local(move |l| {
            l.close_overlays(|o| *o == closing);
        });
    }));
    let ctx2 = ctx.clone();
    let confirming = overlay.clone();
    confirm.connect_clicked(guarded!("modal: confirm", move |_| {
        let target = confirming.clone();
        let store = ctx2.store.clone();
        let close = move || {
            store.update_local(move |l| {
                l.close_overlays(|o| *o == target);
            });
        };
        match &confirming {
            Overlay::Confirm { cmd, .. } => {
                // The confirmed command before the overlay closes: the hook
                // its asker left (`Store::ask_confirmation`, the export
                // panel's D-24) goes with the overlay.
                ctx2.store.dispatch_confirmed(cmd.clone());
                close();
            }
            Overlay::QuitWhileExporting { .. } => {
                close();
                ctx2.actions.run("app.cancel-export-and-quit");
            }
            _ => close(),
        }
    }));
    ModalView { root, body, cancel }
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_max_width_chars(1);
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// The loss list: a `field` well of `mono-sm` rows, sizes on the right; 8
/// rows show, more scroll.
fn loss_list(rows: &[(String, Option<String>)]) -> gtk::ScrolledWindow {
    const ROW: i32 = 24;
    const PAD: i32 = 6;
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("modal-rows");
    for (text, right) in rows {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("modal-row");
        row.set_height_request(ROW);
        let left = gtk::Label::new(Some(text));
        left.add_css_class("t-mono-sm");
        left.set_xalign(0.0);
        left.set_hexpand(true);
        left.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        left.set_max_width_chars(1);
        row.append(&left);
        if let Some(right) = right {
            let size = gtk::Label::new(Some(right));
            size.add_css_class("t-mono-sm");
            size.set_xalign(1.0);
            row.append(&size);
        }
        list.append(&row);
    }
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_widget_name("modal-list");
    scrolled.add_css_class("modal-list");
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_overlay_scrolling(false);
    scrolled.set_propagate_natural_height(true);
    scrolled.set_max_content_height(8 * ROW + 2 * PAD);
    scrolled.set_child(Some(&list));
    // While it scrolls, the rows make room for the bar (`.scrolls`).
    let rows = list.clone();
    scrolled
        .vadjustment()
        .connect_changed(guarded!("modal: the list scrolls", move |adj| {
            if adj.upper() > adj.page_size() + 0.5 {
                rows.add_css_class("scrolls");
            } else {
                rows.remove_css_class("scrolls");
            }
        }));
    scrolled
}

/// The overwrite's files as the GUI shows them, before their mtimes are
/// known (`format::display_path`; "now" on the local clock).
fn initial_facts(ctx: &Ctx, overlay: &Overlay) -> ConfirmFacts {
    let now = Local::now().fixed_offset();
    let Overlay::Confirm { cmd, error } = overlay else {
        return ConfirmFacts {
            now,
            ..ConfirmFacts::default()
        };
    };
    if !matches!(cmd, Command::Export { .. } | Command::FrameSave { .. }) {
        return ConfirmFacts {
            now,
            ..ConfirmFacts::default()
        };
    }
    let dir = ctx.store.project_dir().display().to_string();
    let home = std::env::var("HOME").ok();
    let files = named_files(error)
        .into_iter()
        .map(|path| {
            let display = format::display_path(&path, &dir, home.as_deref());
            (
                path,
                SeenFile {
                    display,
                    modified: None,
                },
            )
        })
        .collect();
    ConfirmFacts { files, now }
}

/// The paths `details.files[].path` names.
fn named_files(error: &CoreError) -> Vec<String> {
    error
        .details
        .as_ref()
        .and_then(|d| d.get("files"))
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(|f| f.get("path").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Stats the overwrite's files on the io worker (R1: no file I/O on the
/// GTK thread) and updates the body when they answer, if the same modal
/// is still up.
fn stat_files(
    ctx: &Ctx,
    shown: &Rc<RefCell<Option<Shown>>>,
    overlay: &Overlay,
    facts: ConfirmFacts,
) {
    if facts.files.is_empty() {
        return;
    }
    let dir = ctx.store.project_dir().to_path_buf();
    let paths: Vec<String> = facts.files.keys().cloned().collect();
    let (ctx2, shown2, overlay2) = (ctx.clone(), Rc::clone(shown), overlay.clone());
    ctx.media.run_io_then(
        move || {
            paths
                .into_iter()
                .map(|p| {
                    let modified = modified_at(&dir, &p);
                    (p, modified)
                })
                .collect::<Vec<_>>()
        },
        move |found| {
            let mut facts = facts;
            for (path, modified) in found {
                if let Some(seen) = facts.files.get_mut(&path) {
                    seen.modified = modified;
                }
            }
            let shown = shown2.borrow();
            let Some(current) = shown.as_ref().filter(|s| s.overlay == overlay2) else {
                return;
            };
            if let Some(content) = content_for(&ctx2, &overlay2, &facts) {
                current
                    .body
                    .set_text(content.body.as_deref().unwrap_or_default());
            }
        },
    );
}

/// When `path` (relative to the project, or absolute) was last modified,
/// on the local clock of that instant.
fn modified_at(dir: &Path, path: &str) -> Option<DateTime<FixedOffset>> {
    let full: PathBuf = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        dir.join(path)
    };
    let modified = std::fs::metadata(full).ok()?.modified().ok()?;
    Some(DateTime::<Local>::from(modified).fixed_offset())
}

#[cfg(test)]
mod tests {
    use qrec_core::{ErrorCode, TakeId};
    use serde_json::json;

    use super::*;

    #[test]
    fn only_confirmations_and_the_quit_question_are_modals() {
        let confirm = Overlay::Confirm {
            cmd: Command::TakePurge {
                takes: vec![TakeId::numbered(4)],
            },
            error: CoreError::new(ErrorCode::ConfirmationRequired, "purge take 4 from disk?"),
        };
        assert!(is_modal(&confirm));
        assert!(is_modal(&Overlay::QuitWhileExporting { percent: 42 }));
        assert!(!is_modal(&Overlay::Palette));
        assert!(!is_modal(&Overlay::Menu));
    }

    #[test]
    fn an_overwrite_names_its_files() {
        let error = CoreError::confirmation_required("overwrite")
            .with_details(json!({"files": [{"path": "/home/me/talk.mp4", "bytes": 3}]}));
        assert_eq!(named_files(&error), ["/home/me/talk.mp4"]);
        assert!(named_files(&CoreError::confirmation_required("x")).is_empty());
        let dir = std::env::temp_dir();
        assert!(modified_at(&dir, "surely-not-here-qrec.mp4").is_none());
    }
}
