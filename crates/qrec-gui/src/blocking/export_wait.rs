//! The drafted D-29 export wait (WP8 after G§0.2; not drawn, so built in
//! X1's modal, `cli_recording.rs`): "An export from the CLI is running.",
//! how far it got, the well with the output, its percent and a mini mud
//! bar (the header slot's), Quit (secondary, focused) and Wait (primary;
//! "Waiting…", loading, once pressed). `Esc` quits (exit 6) and the export
//! goes on. The percent follows the takeover's `hello` every 500 ms
//! (G§3.4.3). Content: `view::blocking::export_wait`. Look:
//! `components/blocking.css`.

use qrec_core::view::blocking::{self, BlockKind, ModalPage};

use super::cli_recording::ModalView;
use super::{OnAction, PageView};
use crate::Ctx;

/// The drafted page for `kind` (the export wait), or `None`.
pub fn view(ctx: &Ctx, kind: &BlockKind, on_action: OnAction) -> Option<Box<dyn PageView>> {
    ModalView::new(ctx, kind, waiting, on_action).map(|v| Box::new(v) as Box<dyn PageView>)
}

/// The export wait's content for `kind`.
fn waiting(_ctx: &Ctx, kind: &BlockKind) -> Option<ModalPage> {
    match kind {
        BlockKind::ExportWait {
            name,
            percent,
            waiting,
        } => Some(blocking::export_wait(name, *percent, *waiting)),
        _ => None,
    }
}
