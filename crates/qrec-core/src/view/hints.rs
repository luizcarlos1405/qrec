//! The hint bar (G§4.3, C03, D-02). Owner: WP6. Stub until WP6 lands: the
//! signatures are final, the bodies return defaults.
//!
//! [`hint_state`] picks what the bar describes (first match of G§4.3's
//! list), [`hint_bar`] the items for it with the user's keys
//! (`keymap::usable_keys`, cap legends) and the status text ("Snapping on ·
//! 1920×1080 · 30 fps"), and [`hint_enabled`] decides for ids the GUI
//! handles itself. `keymap::hints` stays for `qrec keys` and the agent
//! guide.

use serde::{Deserialize, Serialize};

use crate::keymap::Keymap;
use crate::state::State;
use crate::view::{GuiFacts, HintItem};

/// What the hint bar describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HintState {
    Modal,
    Palette,
    Settings,
    TrimDrag,
    MoveDrag,
    BinDrag,
    LayerEdit,
    Countdown,
    Recording,
    Finalizing,
    ExportSetup,
    ExportDone,
    ExportFailed,
    AutocutPage,
    Bin,
    FirstRun,
    PortalDenied,
    Empty,
    FrameZoom,
    ClipSelected,
    ProgramIdle,
    LiveIdle,
    /// A blocking page: S23 offers Check again.
    Blocking {
        check_again: bool,
    },
}

/// The state the hint bar describes, first match of G§4.3.
pub fn hint_state(state: &State, facts: &GuiFacts) -> HintState {
    let _ = (state, facts);
    HintState::LiveIdle
}

/// The items for `hint` and the status text on the right.
pub fn hint_bar(
    hint: &HintState,
    state: &State,
    facts: &GuiFacts,
    keymap: &Keymap,
) -> (Vec<HintItem>, String) {
    let _ = (hint, state, facts, keymap);
    (Vec::new(), String::new())
}

/// Whether the item of binding `id` is enabled (G§4.3's table): ids the
/// GUI handles itself by their own rule, anything else when
/// `keymap::action_for` isn't `Unavailable`.
pub fn hint_enabled(id: &str, state: &State, facts: &GuiFacts) -> bool {
    let _ = (id, state, facts);
    true
}
