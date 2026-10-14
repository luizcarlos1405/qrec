//! Keyboard shortcuts (A§9.7, SPEC §10). Owner: meta. Types are final; the
//! functions are signatures only.
//!
//! Grammar `ctrl+shift+z`; shift is recorded only for letters and named
//! keys (`<` is `less`). The default table is SPEC §10.2 plus Backspace for
//! remove. `keys.toml`: `[bindings]` id → string or array of strings.
#![allow(clippy::todo, unused_variables)]

use serde::{Deserialize, Serialize};

use crate::protocol::command::Command;
use crate::state::State;

/// One key combination.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyChord {
    /// GDK keyval name: "s", "less", "question", "bracketleft", "Left".
    pub key: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    #[serde(rename = "super")]
    pub super_: bool,
}

/// Where a binding is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyContext {
    Global,
    Timeline,
    Canvas,
    Recording,
    Overlay,
}

/// Which hints the status bar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HintContext {
    Idle,
    Countdown,
    Recording,
    Finalizing,
    ClipSelected,
    Dragging,
}

/// One bindable action. `qrec keys --json` prints
/// `[{"id","group","label","keys":[..],"cli","contexts"}]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub keys: Vec<KeyChord>,
    /// The CLI equivalent, e.g. `qrec clip split`.
    pub cli: Option<&'static str>,
    pub contexts: Vec<KeyContext>,
}

/// The effective keymap after applying `keys.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Keymap {
    pub bindings: Vec<Binding>,
    pub conflicts: Vec<Conflict>,
    /// Unparsable chords, unknown ids, …
    pub errors: Vec<String>,
}

/// Two or more bindings on one chord in one context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Conflict {
    pub chord: KeyChord,
    pub ids: Vec<String>,
    pub context: KeyContext,
}

/// One status-bar hint: "r record".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hint {
    /// The key as shown ("r", "Ctrl+Z").
    pub keys: String,
    pub label: String,
}

/// The default bindings overridden by `keys_toml` (may be empty).
pub fn resolve(keys_toml: &str) -> Keymap {
    todo!("meta: keymap::resolve")
}

/// "r record · o record over · s split · c autocut · ? all keys".
pub fn hints(ctx: HintContext, map: &Keymap) -> Vec<Hint> {
    todo!("meta: keymap::hints")
}

/// The command a binding runs in `state` (shared by the GUI and the palette).
pub fn command_for(id: &str, state: &State) -> Option<Command> {
    todo!("meta: keymap::command_for")
}
