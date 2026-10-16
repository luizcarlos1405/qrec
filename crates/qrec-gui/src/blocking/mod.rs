//! Blocking pages (C31 Blocking, G§1.1 #2, G§4.11): what the window shows
//! instead of the editor when the project can't open (yet): S23, X1, the
//! D-29 export wait, X2, X3, a project held by another window (D-13), a
//! holder that doesn't respond, GStreamer stuck, the folder or the host
//! failing. Owner: WP0b (this file, `interim.rs`), WP8 (the drafted pages,
//! gated on G§0.2).
//!
//! A drafted page (WP8's files) is used when it exists for the page's
//! kind (its `view` returns `Some`); otherwise the interim page shows
//! SPEC's copy and buttons as a plain C31 block. The flows behind the
//! buttons are startup's (`startup/mod.rs`): a button runs the GUI action
//! of its [`BlockingAction`] ([`action_id`]).

pub mod cli_recording;
pub mod export_wait;
pub mod interim;
pub mod missing_deps;
pub mod not_my_swamp;
pub mod unreadable;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use qrec_core::view::blocking::{BlockKind, BlockingAction};

use crate::Ctx;

/// A page's view: built once per kind of page, updated in place while it
/// shows (X1's timer, D-29's percent, the buttons' busy state).
pub trait PageView {
    fn widget(&self) -> gtk::Widget;
    /// The same page with new values.
    fn update(&self, kind: &BlockKind);
    /// What gets the keyboard focus when the page shows: the primary
    /// button, else the first button that can take it (so `Enter` and
    /// `Space` act on the page, not on the hidden editor).
    fn focus_target(&self) -> Option<gtk::Widget>;
}

/// What a page's button does: runs its GUI action.
pub type OnAction = Rc<dyn Fn(BlockingAction)>;

/// The GUI action id of a page button (registered by the app).
pub fn action_id(action: BlockingAction) -> &'static str {
    match action {
        BlockingAction::CheckAgain => "blocking.check-again",
        BlockingAction::StopAndOpen => "blocking.stop-and-open",
        BlockingAction::Wait => "blocking.wait",
        BlockingAction::Quit => "blocking.quit",
    }
}

/// The page for a [`BlockKind`].
pub struct BlockingPage {
    root: gtk::Box,
    kind: RefCell<BlockKind>,
    view: Box<dyn PageView>,
}

impl BlockingPage {
    pub fn new(ctx: &Ctx, kind: BlockKind) -> Self {
        let actions = ctx.actions.clone();
        let on_action: OnAction = Rc::new(move |action| {
            actions.run(action_id(action));
        });
        let drafted = match &kind {
            BlockKind::MissingDeps { .. } | BlockKind::GstStuck => {
                missing_deps::view(ctx, &kind, Rc::clone(&on_action))
            }
            BlockKind::CliRecording { .. } => {
                cli_recording::view(ctx, &kind, Rc::clone(&on_action))
            }
            BlockKind::ExportWait { .. } => export_wait::view(ctx, &kind, Rc::clone(&on_action)),
            BlockKind::NotMySwamp { .. } => not_my_swamp::view(ctx, &kind, Rc::clone(&on_action)),
            BlockKind::Unreadable { .. } => unreadable::view(ctx, &kind, Rc::clone(&on_action)),
            BlockKind::HeldByWindow { .. }
            | BlockKind::NotResponding { .. }
            | BlockKind::FolderUnreadable { .. }
            | BlockKind::HostFailed { .. } => None,
        };
        let view = drafted.unwrap_or_else(|| {
            Box::new(interim::InterimPage::new(
                &kind,
                on_action,
                (*ctx.store.keymap()).clone(),
            ))
        });
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("region-blocking");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.append(&view.widget());
        Self {
            root,
            kind: RefCell::new(kind),
            view,
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// The page it shows.
    pub fn kind(&self) -> BlockKind {
        self.kind.borrow().clone()
    }

    /// Shows `kind`'s new values; `false` when it is another kind of page
    /// (make a new one).
    pub fn update(&self, kind: &BlockKind) -> bool {
        if std::mem::discriminant(&*self.kind.borrow()) != std::mem::discriminant(kind) {
            return false;
        }
        if *self.kind.borrow() != *kind {
            self.view.update(kind);
            *self.kind.borrow_mut() = kind.clone();
        }
        true
    }

    /// What gets the keyboard focus when the page shows.
    pub fn focus_target(&self) -> Option<gtk::Widget> {
        self.view.focus_target()
    }
}
