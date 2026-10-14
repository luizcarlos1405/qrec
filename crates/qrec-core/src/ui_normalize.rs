//! Keeps `UiState` consistent with the project after every change (A§5.10).
//! Owner: timeline.
//!
//! PLACEHOLDER: the timeline owner replaces this. The real version drops
//! selection ids not on the timeline (sorted by index), selects the new clip
//! after a record-over and every fragment of an autocut clip, clears
//! `selected_layer`/`solo` that no longer exist in the scene `canvas_mode`
//! targets, and converts `playhead`/`zoom` through nanoseconds on an fps
//! change (clamping the playhead to the timeline length).

use crate::model::Project;
use crate::state::UiState;

/// Runs in every `commit`/`persist`. Placeholder: returns `ui` unchanged.
pub fn ui_normalize(prev: &Project, next: &Project, ui: &UiState) -> UiState {
    let _ = (prev, next);
    ui.clone()
}
