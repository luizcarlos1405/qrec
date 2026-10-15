//! The drafted page: X2 (not my swamp) with the D-14 line, as the approved
//! frame draws it (C31 Blocking with art and rows, the minimal header
//! `Xba2H`). Owner: WP8, gated on G§0.2. Until it lands, [`view`] returns
//! `None` and the interim page shows (`interim.rs`).

use qrec_core::view::blocking::BlockKind;

use super::{OnAction, PageView};
use crate::Ctx;

/// The drafted page for `kind`, or `None` (the interim page shows).
pub fn view(ctx: &Ctx, kind: &BlockKind, on_action: OnAction) -> Option<Box<dyn PageView>> {
    let _ = (ctx, kind, on_action);
    None
}
