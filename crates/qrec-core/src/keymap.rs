//! Keyboard shortcuts (A§9.7, SPEC §10). Owner: meta.
//!
//! [`resolve`] builds the effective [`Keymap`]: the default table (SPEC
//! §10.2, plus Backspace for remove, the canvas nudges of SPEC §12 and the
//! window's region focus and quit keys, GUI D-10) with the `keys.toml`
//! overrides applied and the conflicts found. [`hints`] picks the
//! status-bar hints for a [`HintContext`]; [`action_for`] / [`command_for`]
//! turn a binding into the `Command` it runs in a state (shared by the GUI
//! and the palette), and [`gate_reason`] says why the gates every command
//! shares refuse one; [`palette`] lists every binding with its keys, CLI
//! form and availability; [`cheat_sheet`] and [`keys_view`] are the human
//! and JSON forms of `qrec keys`, and [`problems`] words the conflicts and
//! `keys.toml` errors for notices. [`usable_keys`] are a binding's keys
//! that run (conflicting ones drop out); [`KeyChord::cap_legends`] is how
//! the GUI's key caps show one. [`conflict_notes`] are the conflicts as
//! Settings › Keyboard's banner names them (S22, Q5: the key and the
//! bindings, none of which runs). [`set_binding`] and [`reset_binding`]
//! edit `keys.toml`'s text, one line, the rest of the file as it was (the
//! Keyboard page, through `qrec_shell::keys_file`). [`KEYLESS`] are the
//! palette's actions no key runs ("Clear undo history", GUI D-19), which
//! [`action_for`] knows by id.
//!
//! **Chord grammar** (`keys.toml`, `qrec keys --json`): modifiers `ctrl`,
//! `alt`, `shift`, `super` joined with `+`, then a GDK keyval name:
//! `ctrl+shift+z`, `less`, `alt+bracketleft`, `Escape`, `shift+Left`. The
//! parser also takes the typed character (`<`, `[`, `?`) and common aliases
//! (`esc`, `del`, `enter`, `pgup`); case doesn't matter. Shift is recorded
//! only for letters and named keys: `<` is `less`, and `shift+comma` is an
//! error because shift changes the character a punctuation key types.
//!
//! **Contexts:** `global` bindings work whatever has focus (timeline or
//! canvas); `timeline` and `canvas` bindings only with that focus; while a
//! countdown, recording or finalization runs only `recording` bindings are
//! active; an open overlay (cheat sheet, palette, modal) sees only `overlay`
//! bindings. A chord used by two bindings that are active together is a
//! [`Conflict`], and neither of them runs ([`lookup`]).
//!
//! **`keys.toml`** (`~/.config/qrec/keys.toml`):
//! ```toml
//! [bindings]
//! frame.save = "f"                # replaces the default keys
//! clip.remove = ["Delete", "x"]   # several keys
//! record.over = []                # unbound
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::ids::{ClipId, Frame};
use crate::model::{Clip, Rect};
use crate::protocol::command::{
    AutocutOverrides, ClipScope, Command, ExportFormat, LayerPatch, MoveTo, Nullable, Quality,
    RecordModeArg, SceneTarget, SeekTarget, SelectOp, ShuttleDir, UiPatch,
};
use crate::state::{CanvasMode, HostKind, JobKind, RecordPhase, State};
use crate::time::{NS_PER_SEC, Sign, TimeArg, TimeValue};
use crate::timeline::{clip_at, position_of, total_frames};
use crate::ui_normalize::{NO_EDIT_TARGET, edit_target};

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

impl KeyContext {
    pub const ALL: [KeyContext; 5] = [
        KeyContext::Global,
        KeyContext::Timeline,
        KeyContext::Canvas,
        KeyContext::Recording,
        KeyContext::Overlay,
    ];

    /// `global`, `timeline`, …
    pub fn as_str(self) -> &'static str {
        match self {
            KeyContext::Global => "global",
            KeyContext::Timeline => "timeline",
            KeyContext::Canvas => "canvas",
            KeyContext::Recording => "recording",
            KeyContext::Overlay => "overlay",
        }
    }
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

impl HintContext {
    pub const ALL: [HintContext; 6] = [
        HintContext::Idle,
        HintContext::Countdown,
        HintContext::Recording,
        HintContext::Finalizing,
        HintContext::ClipSelected,
        HintContext::Dragging,
    ];
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

impl Binding {
    /// Whether the binding is active in `ctx`. `global` bindings are also
    /// active when the timeline or the canvas has focus.
    pub fn active_in(&self, ctx: KeyContext) -> bool {
        self.contexts.contains(&ctx)
            || (matches!(ctx, KeyContext::Timeline | KeyContext::Canvas)
                && self.contexts.contains(&KeyContext::Global))
    }
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

impl Keymap {
    /// The binding with this id.
    pub fn binding(&self, id: &str) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.id == id)
    }
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
    /// The key as shown ("r", "Ctrl+Z"); several keys are separated by a
    /// space ("[ ]"); empty for a label-only hint ("release to apply").
    pub keys: String,
    pub label: String,
}

// ----------------------------------------------------------------- chords

/// How a key behaves with shift and how it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyKind {
    /// `a`–`z`: shift is recorded.
    Letter,
    /// Esc, Tab, arrows, F-keys, …: shift is recorded.
    Named,
    /// Digits and punctuation: shift picks another character, so it isn't recorded.
    Char,
}

/// Keys that type a character: GDK keyval name and character.
const CHAR_KEYS: &[(&str, char)] = &[
    ("exclam", '!'),
    ("quotedbl", '"'),
    ("numbersign", '#'),
    ("dollar", '$'),
    ("percent", '%'),
    ("ampersand", '&'),
    ("apostrophe", '\''),
    ("parenleft", '('),
    ("parenright", ')'),
    ("asterisk", '*'),
    ("plus", '+'),
    ("comma", ','),
    ("minus", '-'),
    ("period", '.'),
    ("slash", '/'),
    ("colon", ':'),
    ("semicolon", ';'),
    ("less", '<'),
    ("equal", '='),
    ("greater", '>'),
    ("question", '?'),
    ("at", '@'),
    ("bracketleft", '['),
    ("backslash", '\\'),
    ("bracketright", ']'),
    ("asciicircum", '^'),
    ("underscore", '_'),
    ("grave", '`'),
    ("braceleft", '{'),
    ("bar", '|'),
    ("braceright", '}'),
    ("asciitilde", '~'),
];

/// Named keys: GDK keyval name, how it is shown, and extra spellings.
const NAMED_KEYS: &[(&str, &str, &[&str])] = &[
    ("Escape", "Esc", &["esc"]),
    ("Tab", "Tab", &[]),
    ("space", "Space", &[" "]),
    ("Return", "Enter", &["enter"]),
    ("BackSpace", "Backspace", &["bksp"]),
    ("Delete", "Del", &["del"]),
    ("Insert", "Ins", &["ins"]),
    ("Home", "Home", &[]),
    ("End", "End", &[]),
    ("Page_Up", "PgUp", &["pgup", "pageup", "prior"]),
    ("Page_Down", "PgDn", &["pgdn", "pagedown", "next"]),
    ("Left", "←", &["←"]),
    ("Right", "→", &["→"]),
    ("Up", "↑", &["↑"]),
    ("Down", "↓", &["↓"]),
    ("Menu", "Menu", &[]),
    ("Print", "Print", &[]),
    ("Pause", "Pause", &[]),
];

/// Finds the GDK name and kind of a key written as a name, an alias or the
/// character it types. Letters and names are case-insensitive.
fn normalize_key(text: &str) -> Option<(String, KeyKind)> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        if first.is_ascii_alphabetic() {
            return Some((first.to_ascii_lowercase().to_string(), KeyKind::Letter));
        }
        if first.is_ascii_digit() {
            return Some((first.to_string(), KeyKind::Char));
        }
        if let Some((name, _)) = CHAR_KEYS.iter().find(|(_, ch)| *ch == first) {
            return Some(((*name).to_owned(), KeyKind::Char));
        }
    }
    let lower = text.to_lowercase();
    for (name, _, aliases) in NAMED_KEYS {
        if name.to_lowercase() == lower || aliases.contains(&lower.as_str()) {
            return Some(((*name).to_owned(), KeyKind::Named));
        }
    }
    if let Some((name, _)) = CHAR_KEYS.iter().find(|(name, _)| *name == lower) {
        return Some(((*name).to_owned(), KeyKind::Char));
    }
    // F1 … F24 (no leading zero).
    let number = lower.strip_prefix('f').filter(|n| !n.starts_with('0'));
    match number.and_then(|n| n.parse::<u8>().ok()) {
        Some(n) if (1..=24).contains(&n) => Some((format!("F{n}"), KeyKind::Named)),
        _ => None,
    }
}

fn key_kind(key: &str) -> KeyKind {
    normalize_key(key).map_or(KeyKind::Named, |(_, kind)| kind)
}

impl KeyChord {
    /// A chord without modifiers. `key` must already be a GDK keyval name.
    pub fn key(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ctrl: false,
            alt: false,
            shift: false,
            super_: false,
        }
    }

    /// Parses the chord grammar (`ctrl+shift+z`, `less`, `alt+[`, `Esc`).
    pub fn parse(text: &str) -> Result<KeyChord, String> {
        let s = text.trim();
        if s.is_empty() {
            return Err("empty key".to_owned());
        }
        // The key itself may be `+`: "+", "ctrl++".
        let (mods, key): (Option<&str>, &str) = if s == "+" {
            (None, "+")
        } else if let Some(rest) = s.strip_suffix("++") {
            (Some(rest), "+")
        } else {
            match s.rsplit_once('+') {
                Some((mods, key)) => (Some(mods), key),
                None => (None, s),
            }
        };
        let mut chord = KeyChord::key("");
        for m in mods
            .map(|m| m.split('+').collect::<Vec<_>>())
            .unwrap_or_default()
        {
            let flag = match m.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut chord.ctrl,
                "alt" => &mut chord.alt,
                "shift" => &mut chord.shift,
                "super" => &mut chord.super_,
                "" => return Err(format!("{s:?}: empty modifier")),
                other => {
                    return Err(format!(
                        "{s:?}: unknown modifier {other:?} (use ctrl, alt, shift or super)"
                    ));
                }
            };
            if *flag {
                return Err(format!("{s:?}: {} is given twice", m.trim()));
            }
            *flag = true;
        }
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("{s:?}: a key is missing after the modifiers"));
        }
        let (name, kind) = normalize_key(key).ok_or_else(|| {
            format!("{s:?}: unknown key {key:?} (use a GDK key name such as s, less, bracketleft, Escape or Left)")
        })?;
        if chord.shift && kind == KeyKind::Char {
            return Err(format!(
                "{s:?}: shift can't be combined with {key:?} because it types another character; bind that character instead"
            ));
        }
        chord.key = name;
        Ok(chord)
    }

    /// A chord from a GDK key event: the keyval's name (`gdk::Key::name`) and
    /// the modifier state. Letters are lowercased and shift is dropped for
    /// keys that type a character (Shift+, arrives as `less`). `None` for
    /// keys Qrec can't bind (modifier keys alone, dead keys, non-ASCII letters).
    pub fn from_gdk(
        keyval: &str,
        ctrl: bool,
        alt: bool,
        shift: bool,
        super_: bool,
    ) -> Option<Self> {
        let (key, kind, shift) = if keyval == "ISO_Left_Tab" {
            ("Tab".to_owned(), KeyKind::Named, true)
        } else {
            let (key, kind) = normalize_key(keyval)?;
            (key, kind, shift)
        };
        Some(Self {
            key,
            ctrl,
            alt,
            shift: shift && kind != KeyKind::Char,
            super_,
        })
    }

    /// The legends of the key caps that show the chord in the GUI (C02,
    /// G§4.2): each modifier on a cap of its own (`Ctrl`, `Alt`, `Shift`,
    /// `Super`, in that order), then the key: letters uppercase, characters
    /// as typed (`minus` as the true minus `−`), named keys short (`Esc`,
    /// `Enter`, `Space`, `Del`, `Backspace`, `PgUp`, `←`, `F6`).
    pub fn cap_legends(&self) -> Vec<String> {
        let mut caps: Vec<String> = [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.super_, "Super"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| name.to_owned())
        .collect();
        let key = match key_kind(&self.key) {
            KeyKind::Letter => self.key.to_ascii_uppercase(),
            KeyKind::Char if self.key == "minus" => "\u{2212}".to_owned(),
            KeyKind::Char => CHAR_KEYS
                .iter()
                .find(|(name, _)| *name == self.key)
                .map_or_else(|| self.key.clone(), |(_, c)| c.to_string()),
            KeyKind::Named => NAMED_KEYS
                .iter()
                .find(|(name, _, _)| *name == self.key)
                .map_or_else(|| self.key.clone(), |(_, shown, _)| (*shown).to_owned()),
        };
        caps.push(key);
        caps
    }

    /// How the chord is shown to people: `r`, `Ctrl+Z`, `Shift+←`, `Esc`, `Alt+[`.
    pub fn display(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
            (self.super_, "Super+"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        let modified = !out.is_empty();
        match key_kind(&self.key) {
            KeyKind::Letter if modified => out.push_str(&self.key.to_ascii_uppercase()),
            KeyKind::Letter => out.push_str(&self.key),
            KeyKind::Char => match CHAR_KEYS.iter().find(|(name, _)| *name == self.key) {
                Some((_, c)) => out.push(*c),
                None => out.push_str(&self.key),
            },
            KeyKind::Named => match NAMED_KEYS.iter().find(|(name, _, _)| *name == self.key) {
                Some((_, shown, _)) => out.push_str(shown),
                None => out.push_str(&self.key),
            },
        }
        out
    }
}

impl fmt::Display for KeyChord {
    /// The chord grammar: `ctrl+alt+shift+super+<keyval>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "ctrl+"),
            (self.alt, "alt+"),
            (self.shift, "shift+"),
            (self.super_, "super+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        f.write_str(&self.key)
    }
}

impl FromStr for KeyChord {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, CoreError> {
        KeyChord::parse(s).map_err(CoreError::usage)
    }
}

// ---------------------------------------------------------- default table

use KeyContext::{Canvas, Global, Overlay, Recording, Timeline};

/// One row of the default table.
struct Def {
    id: &'static str,
    group: &'static str,
    label: &'static str,
    /// Chords in the grammar.
    keys: &'static [&'static str],
    cli: Option<&'static str>,
    contexts: &'static [KeyContext],
}

const fn def(
    id: &'static str,
    group: &'static str,
    label: &'static str,
    keys: &'static [&'static str],
    cli: Option<&'static str>,
    contexts: &'static [KeyContext],
) -> Def {
    Def {
        id,
        group,
        label,
        keys,
        cli,
        contexts,
    }
}

/// The groups in cheat-sheet order.
pub const GROUPS: [&str; 6] = ["Record", "Playback", "Edit", "Output", "View", "Canvas"];

const CLI_NUDGE_LAYER: Option<&str> = Some("qrec scene layer set <layer> --rect X,Y,W,H");

/// SPEC §10.2, plus Backspace for remove (A§9.7) and the canvas nudges of
/// SPEC §12 (arrow keys with the canvas focused; Shift = 10 px).
#[rustfmt::skip]
const DEFAULTS: &[Def] = &[
    // Record
    def("record.toggle", "Record", "Start / stop recording", &["r"], Some("qrec record toggle"), &[Global, Recording]),
    def("record.over", "Record", "Record over the selection", &["o"], Some("qrec record start --over <clip>…"), &[Global]),
    def("take.discard-last", "Record", "Discard the last take (to the bin)", &["d"], Some("qrec take discard-last"), &[Global, Recording]),
    def("cancel", "Record", "Cancel recording, deselect, close overlay", &["Escape"], Some("qrec record cancel"), &[Global, Recording, Overlay]),
    // Playback
    def("play.toggle", "Playback", "Play / pause", &["space"], Some("qrec play"), &[Global, Recording]),
    def("shuttle.reverse", "Playback", "Shuttle reverse (again: 2×, 4×)", &["j"], None, &[Global]),
    def("shuttle.stop", "Playback", "Shuttle stop", &["k"], None, &[Global]),
    def("shuttle.forward", "Playback", "Shuttle forward (again: 2×, 4×)", &["l"], None, &[Global]),
    // Seeking is allowed while recording (A§4.4 R); the arrows belong to the
    // canvas (layer nudges) when it has focus.
    def("seek.prev-frame", "Playback", "Previous frame", &["Left"], Some("qrec seek --by -1f"), &[Timeline, Recording]),
    def("seek.next-frame", "Playback", "Next frame", &["Right"], Some("qrec seek --by +1f"), &[Timeline, Recording]),
    def("seek.back", "Playback", "Back 1 s", &["shift+Left"], Some("qrec seek --by -1s"), &[Timeline, Recording]),
    def("seek.forward", "Playback", "Forward 1 s", &["shift+Right"], Some("qrec seek --by +1s"), &[Timeline, Recording]),
    def("seek.prev-edit", "Playback", "Previous edit point", &["Up"], Some("qrec seek --edit prev"), &[Timeline, Recording]),
    def("seek.next-edit", "Playback", "Next edit point", &["Down"], Some("qrec seek --edit next"), &[Timeline, Recording]),
    def("seek.start", "Playback", "Timeline start", &["Home"], Some("qrec seek --to start"), &[Global, Recording]),
    def("seek.end", "Playback", "Timeline end", &["End"], Some("qrec seek --to end"), &[Global, Recording]),
    // Edit
    def("select.prev", "Edit", "Select previous clip", &["comma"], None, &[Global, Recording]),
    def("select.next", "Edit", "Select next clip", &["period"], None, &[Global, Recording]),
    def("clip.move-left", "Edit", "Move selection left one slot", &["less"], Some("qrec clip move <clip>… --by -1"), &[Global]),
    def("clip.move-right", "Edit", "Move selection right one slot", &["greater"], Some("qrec clip move <clip>… --by 1"), &[Global]),
    def("clip.split", "Edit", "Split clip at the playhead", &["s"], Some("qrec clip split <clip> --at <t>"), &[Global]),
    def("clip.trim-start", "Edit", "Trim start to the playhead", &["bracketleft"], Some("qrec clip trim <clip> --start <t>"), &[Global]),
    def("clip.trim-end", "Edit", "Trim end to the playhead", &["bracketright"], Some("qrec clip trim <clip> --end <t>"), &[Global]),
    def("clip.nudge-start", "Edit", "Trim 1 frame off the start", &["alt+bracketleft"], Some("qrec clip trim <clip> --start +1f"), &[Global]),
    def("clip.nudge-end", "Edit", "Trim 1 frame off the end", &["alt+bracketright"], Some("qrec clip trim <clip> --end -1f"), &[Global]),
    def("clip.nudge-start-10", "Edit", "Trim 10 frames off the start", &["alt+braceleft"], Some("qrec clip trim <clip> --start +10f"), &[Global]),
    def("clip.nudge-end-10", "Edit", "Trim 10 frames off the end", &["alt+braceright"], Some("qrec clip trim <clip> --end -10f"), &[Global]),
    def("clip.remove", "Edit", "Remove selected clips (to the bin)", &["Delete", "x", "BackSpace"], Some("qrec clip remove <clip>…"), &[Global]),
    def("autocut.run", "Edit", "Autocut the selection, or every clip", &["c"], Some("qrec autocut [--clips <clip>…]"), &[Global]),
    def("autocut.preview", "Edit", "Autocut settings and preview", &["shift+c"], Some("qrec autocut --dry-run"), &[Global]),
    def("history.undo", "Edit", "Undo", &["ctrl+z"], Some("qrec undo"), &[Global]),
    def("history.redo", "Edit", "Redo", &["ctrl+shift+z", "ctrl+y"], Some("qrec redo"), &[Global]),
    def("select.all", "Edit", "Select all clips", &["ctrl+a"], None, &[Global, Recording]),
    // Output
    def("frame.save", "Output", "Save the frame at the playhead (PNG)", &["p"], Some("qrec frame save --at <t>"), &[Global]),
    def("export", "Output", "Export…", &["ctrl+e"], Some("qrec export"), &[Global]),
    // View
    def("zoom.in", "View", "Zoom timeline in", &["equal"], None, &[Global, Recording]),
    def("zoom.out", "View", "Zoom timeline out", &["minus"], None, &[Global, Recording]),
    def("zoom.fit", "View", "Zoom to fit", &["backslash"], None, &[Global, Recording]),
    def("zoom.selection", "View", "Zoom to the selection", &["z"], None, &[Global, Recording]),
    def("snapping.toggle", "View", "Toggle snapping", &["n"], None, &[Global, Recording]),
    def("canvas.mode", "View", "Switch canvas Live / Program", &["Tab"], None, &[Global, Recording]),
    def("bin.toggle", "View", "Toggle the bin", &["b"], Some("qrec take list --unused"), &[Global, Recording]),
    def("keys.help", "View", "Cheat sheet", &["question"], Some("qrec keys"), &[Global, Recording]),
    def("palette.open", "View", "Command palette", &["ctrl+k", "colon"], None, &[Global, Recording]),
    def("settings.open", "View", "Settings", &["ctrl+comma"], Some("qrec config"), &[Global]),
    // Window focus and quitting (GUI D-10): header → transport → timeline
    // toolbar → timeline → panel → bin; Ctrl+Q also works in overlays.
    def("region.next", "View", "Focus the next region", &["F6"], None, &[Global, Recording]),
    def("region.prev", "View", "Focus the previous region", &["shift+F6"], None, &[Global, Recording]),
    def("app.quit", "View", "Quit", &["ctrl+q"], None, &[Global, Recording, Overlay]),
    // Canvas (SPEC §12)
    def("layer.nudge-left", "Canvas", "Nudge layer left 1 px", &["Left"], CLI_NUDGE_LAYER, &[Canvas]),
    def("layer.nudge-right", "Canvas", "Nudge layer right 1 px", &["Right"], CLI_NUDGE_LAYER, &[Canvas]),
    def("layer.nudge-up", "Canvas", "Nudge layer up 1 px", &["Up"], CLI_NUDGE_LAYER, &[Canvas]),
    def("layer.nudge-down", "Canvas", "Nudge layer down 1 px", &["Down"], CLI_NUDGE_LAYER, &[Canvas]),
    def("layer.nudge-left-10", "Canvas", "Nudge layer left 10 px", &["shift+Left"], CLI_NUDGE_LAYER, &[Canvas]),
    def("layer.nudge-right-10", "Canvas", "Nudge layer right 10 px", &["shift+Right"], CLI_NUDGE_LAYER, &[Canvas]),
    def("layer.nudge-up-10", "Canvas", "Nudge layer up 10 px", &["shift+Up"], CLI_NUDGE_LAYER, &[Canvas]),
    def("layer.nudge-down-10", "Canvas", "Nudge layer down 10 px", &["shift+Down"], CLI_NUDGE_LAYER, &[Canvas]),
];

/// An action the command palette lists that no key runs (GUI D-19):
/// [`action_for`] knows its id like a binding's, `keys.toml` doesn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeylessAction {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub cli: Option<&'static str>,
    /// The binding it is listed after in its group.
    pub after: &'static str,
}

/// The palette's actions without a key: "Clear undo history" (D-19), after
/// Redo. The core asks for its confirmation (SPEC §11's modal).
pub const KEYLESS: &[KeylessAction] = &[KeylessAction {
    id: "history.clear",
    group: "Edit",
    label: "Clear undo history",
    cli: Some("qrec history clear --yes"),
    after: "history.redo",
}];

/// The niri key binds for recording while another app has focus (SPEC
/// §10.2); `qrec doctor` and the agent guide print it.
pub const NIRI_SNIPPET: &str = "binds {
    Mod+Shift+R { spawn \"qrec\" \"record\" \"toggle\"; }
    Mod+Shift+O { spawn \"qrec\" \"record\" \"start\" \"--over-selection\"; }
    Mod+Shift+D { spawn \"qrec\" \"take\" \"discard-last\"; }
}";

fn default_bindings(errors: &mut Vec<String>) -> Vec<Binding> {
    DEFAULTS
        .iter()
        .map(|d| Binding {
            id: d.id,
            group: d.group,
            label: d.label,
            keys: d
                .keys
                .iter()
                .filter_map(|k| {
                    KeyChord::parse(k)
                        .map_err(|e| errors.push(format!("default binding {}: {e}", d.id)))
                        .ok()
                })
                .collect(),
            cli: d.cli,
            contexts: d.contexts.to_vec(),
        })
        .collect()
}

// --------------------------------------------------------------- resolve

/// The default bindings overridden by `keys_toml` (may be empty).
pub fn resolve(keys_toml: &str) -> Keymap {
    let mut errors = Vec::new();
    let mut bindings = default_bindings(&mut errors);
    if !keys_toml.trim().is_empty() {
        match toml::from_str::<toml::Table>(keys_toml) {
            Ok(table) => apply_overrides(&mut bindings, &table, &mut errors),
            Err(e) => errors.push(toml_error("keys.toml", keys_toml, &e)),
        }
    }
    let conflicts = find_conflicts(&bindings);
    Keymap {
        bindings,
        conflicts,
        errors,
    }
}

/// `"<file> line N: message"` for a TOML parse error.
pub(crate) fn toml_error(file: &str, text: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(span) => {
            let line = 1 + text[..span.start.min(text.len())].matches('\n').count();
            format!("{file} line {line}: {}", e.message())
        }
        None => format!("{file}: {}", e.message()),
    }
}

fn apply_overrides(bindings: &mut [Binding], table: &toml::Table, errors: &mut Vec<String>) {
    for (section, value) in table {
        if section != "bindings" {
            errors.push(format!(
                "keys.toml: unknown section `{section}` (only [bindings] is read)"
            ));
            continue;
        }
        let Some(map) = value.as_table() else {
            errors.push("keys.toml: `bindings` must be a table".to_owned());
            continue;
        };
        let mut entries = Vec::new();
        flatten("", map, &mut entries);
        let mut seen = BTreeSet::new();
        for (id, value) in entries {
            let Some(binding) = bindings.iter_mut().find(|b| b.id == id) else {
                errors.push(format!(
                    "keys.toml: unknown binding `{id}` (`qrec keys` lists the ids)"
                ));
                continue;
            };
            if !seen.insert(id.clone()) {
                errors.push(format!(
                    "keys.toml: `{id}` is set twice; the first one is used"
                ));
                continue;
            }
            match override_keys(value) {
                Ok(keys) => binding.keys = keys,
                Err(e) => errors.push(format!("keys.toml: `{id}`: {e}; keeping the default keys")),
            }
        }
    }
}

/// Dotted TOML keys (`clip.split = "s"`) nest tables; they name ids again.
fn flatten<'a>(prefix: &str, table: &'a toml::Table, out: &mut Vec<(String, &'a toml::Value)>) {
    for (key, value) in table {
        let id = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value.as_table() {
            Some(inner) => flatten(&id, inner, out),
            None => out.push((id, value)),
        }
    }
}

fn override_keys(value: &toml::Value) -> Result<Vec<KeyChord>, String> {
    let texts: Vec<&str> = match value {
        toml::Value::String(s) => vec![s.as_str()],
        toml::Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| format!("expected key strings, found {item}"))
            })
            .collect::<Result<_, _>>()?,
        other => return Err(format!("expected a key or a list of keys, found {other}")),
    };
    let mut keys: Vec<KeyChord> = Vec::new();
    for text in texts {
        let chord = KeyChord::parse(text)?;
        if !keys.contains(&chord) {
            keys.push(chord);
        }
    }
    Ok(keys)
}

fn find_conflicts(bindings: &[Binding]) -> Vec<Conflict> {
    let mut out = Vec::new();
    for ctx in KeyContext::ALL {
        let mut by_chord: BTreeMap<&KeyChord, Vec<&Binding>> = BTreeMap::new();
        for binding in bindings.iter().filter(|b| b.active_in(ctx)) {
            let unique: BTreeSet<&KeyChord> = binding.keys.iter().collect();
            for chord in unique {
                by_chord.entry(chord).or_default().push(binding);
            }
        }
        for (chord, users) in by_chord {
            if users.len() < 2 {
                continue;
            }
            // Global bindings clashing with each other are reported once, under global.
            let all_global = users
                .iter()
                .all(|b| b.contexts.contains(&KeyContext::Global));
            if matches!(ctx, KeyContext::Timeline | KeyContext::Canvas) && all_global {
                continue;
            }
            out.push(Conflict {
                chord: chord.clone(),
                ids: users.iter().map(|b| b.id.to_owned()).collect(),
                context: ctx,
            });
        }
    }
    out
}

/// The binding a key press runs with `ctx` focused (`timeline`, `canvas`,
/// `recording` or `overlay`); `None` when nothing or more than one binding
/// uses the chord there (conflicting bindings never run).
pub fn lookup<'a>(map: &'a Keymap, chord: &KeyChord, ctx: KeyContext) -> Option<&'a Binding> {
    let mut found = map
        .bindings
        .iter()
        .filter(|b| b.active_in(ctx) && b.keys.contains(chord));
    let first = found.next()?;
    match found.next() {
        Some(_) => None,
        None => Some(first),
    }
}

/// The chords of `binding` that aren't part of a conflict: what hints,
/// tooltips and the palette show (rebound keys show, conflicting ones drop
/// out because they never run).
pub fn usable_keys<'a>(map: &Keymap, binding: &'a Binding) -> Vec<&'a KeyChord> {
    let conflicted = conflicting_keys(map, binding.id);
    binding
        .keys
        .iter()
        .filter(|k| !conflicted.contains(k))
        .collect()
}

/// The chords of binding `id` that another binding shares (Q5: neither
/// runs).
pub fn conflicting_keys<'a>(map: &'a Keymap, id: &str) -> Vec<&'a KeyChord> {
    let mut out: Vec<&KeyChord> = Vec::new();
    for c in map
        .conflicts
        .iter()
        .filter(|c| c.ids.iter().any(|i| i == id))
    {
        if !out.contains(&&c.chord) {
            out.push(&c.chord);
        }
    }
    out
}

/// The default keys of binding `id` (SPEC §10.2's table); `None` for an
/// unknown id.
pub fn default_keys(id: &str) -> Option<Vec<KeyChord>> {
    let def = DEFAULTS.iter().find(|d| d.id == id)?;
    Some(
        def.keys
            .iter()
            .filter_map(|k| KeyChord::parse(k).ok())
            .collect(),
    )
}

/// Whether `keys.toml` changed binding `id`'s keys (S20's and S22's
/// "default P").
pub fn is_rebound(binding: &Binding) -> bool {
    default_keys(binding.id).is_some_and(|keys| keys != binding.keys)
}

/// How a key reads in a sentence: its caps joined, `X`, `Ctrl+Shift+Z`,
/// `Alt+[` (S22's banner: "X is bound to both …").
pub fn key_name(chord: &KeyChord) -> String {
    chord.cap_legends().join("+")
}

/// One conflict as people read it (S22's banner, Q5): the key and the
/// bindings that share it, none of which runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConflictNote {
    pub chord: KeyChord,
    /// [`key_name`]: "X".
    pub key: String,
    /// The bindings, the rebound ones first (what `keys.toml` changed
    /// came to clash with what it left), then in table order.
    pub ids: Vec<String>,
    pub labels: Vec<String>,
}

/// The keymap's conflicts, one per key and set of bindings: two bindings
/// that clash in several contexts (`global` and `recording`) are one
/// conflict for people, and so is a clash of some of them in one context
/// (`recording`) that the others' clash (`global`) holds. In the order
/// `keys.toml`'s conflicts were found.
pub fn conflict_notes(map: &Keymap) -> Vec<ConflictNote> {
    let mut out: Vec<ConflictNote> = Vec::new();
    for c in &map.conflicts {
        let mut bindings: Vec<&Binding> = c.ids.iter().filter_map(|id| map.binding(id)).collect();
        // A stable sort: rebound first, the table order otherwise.
        bindings.sort_by_key(|b| !is_rebound(b));
        let ids: Vec<String> = bindings.iter().map(|b| b.id.to_owned()).collect();
        if out
            .iter()
            .any(|n| n.chord == c.chord && contains_all(&n.ids, &ids))
        {
            continue;
        }
        out.retain(|n| !(n.chord == c.chord && contains_all(&ids, &n.ids)));
        out.push(ConflictNote {
            chord: c.chord.clone(),
            key: key_name(&c.chord),
            labels: bindings.iter().map(|b| b.label.to_owned()).collect(),
            ids,
        });
    }
    out
}

/// Every id of `part` is in `all`.
fn contains_all(all: &[String], part: &[String]) -> bool {
    part.iter().all(|id| all.contains(id))
}

// --------------------------------------------------- editing keys.toml

/// `keys_toml` with binding `id` set to `keys` (none: unbound, `id = []`;
/// one: `id = "f"`; more: a list), the rest of the file as it was:
/// comments, blank lines, the order of its lines (`toml_text`). An entry
/// for `id` gets the new value in place (its spacing and comment stay; a
/// second entry for it goes); else a line goes after the last line of
/// `[bindings]` (`clip.split = "s"`, or under `[bindings.clip]` when the
/// file has that header), or a `[bindings]` at the end. The Keyboard
/// settings page writes it (S22, through `qrec_shell::keys_file`). A file
/// that isn't valid TOML is refused, never rewritten.
pub fn set_binding(keys_toml: &str, id: &str, keys: &[KeyChord]) -> Result<String, CoreError> {
    known_binding(id)?;
    let mut unique: Vec<KeyChord> = Vec::new();
    for k in keys {
        if !unique.contains(k) {
            unique.push(k.clone());
        }
    }
    let value = keys_value(&unique);
    let doc = keys_document(keys_toml)?;
    let text_edit = edit_as_text(keys_toml, &doc, id, Some(&value));
    finish_edit(keys_toml, id, text_edit, Some(value), &unique)
}

/// `keys_toml` without its entries for binding `id`, so it has its
/// default keys again (S22's reset), the rest of the file as it was (a
/// comment on the line above an entry stays). Unchanged when the file
/// doesn't set `id`.
pub fn reset_binding(keys_toml: &str, id: &str) -> Result<String, CoreError> {
    known_binding(id)?;
    let defaults = default_keys(id).unwrap_or_default();
    let doc = keys_document(keys_toml)?;
    if binding_entries(&doc, id).is_empty() {
        return Ok(keys_toml.to_owned());
    }
    let text_edit = edit_as_text(keys_toml, &doc, id, None);
    finish_edit(keys_toml, id, text_edit, None, &defaults)
}

fn known_binding(id: &str) -> Result<(), CoreError> {
    if DEFAULTS.iter().any(|d| d.id == id) {
        Ok(())
    } else {
        Err(CoreError::usage(format!("unknown binding `{id}`"))
            .with_hint("`qrec keys` lists the ids"))
    }
}

/// `keys.toml` parsed with its spans; one that isn't valid TOML is
/// refused, so a person's half-finished edit is never written over.
fn keys_document(text: &str) -> Result<toml_edit::Document<String>, CoreError> {
    crate::toml_text::parse(text).map_err(|e| {
        let line = e
            .span()
            .map(|s| 1 + text[..s.start.min(text.len())].matches('\n').count());
        let message = e.message().trim().to_owned();
        CoreError::invalid(match line {
            Some(line) => format!("keys.toml line {line}: {message}"),
            None => format!("keys.toml: {message}"),
        })
        .with_hint("fix keys.toml by hand: Qrec doesn't rewrite a file it can't read")
    })
}

/// The TOML value of `keys`: `"f"`, `["Delete", "x"]`, `[]`.
fn keys_value(keys: &[KeyChord]) -> String {
    match keys {
        [one] => toml_edit::Value::from(one.to_string()).to_string(),
        many => toml_edit::Value::Array(many.iter().map(|k| k.to_string()).collect()).to_string(),
    }
}

/// The entries of `doc` that set binding `id` (`[bindings]`'s keys,
/// dotted or quoted, as `resolve` reads them), in the file's order.
fn binding_entries(doc: &toml_edit::Document<String>, id: &str) -> Vec<crate::toml_text::Entry> {
    let mut found: Vec<crate::toml_text::Entry> = crate::toml_text::entries(doc)
        .into_iter()
        .filter(|e| e.path.len() > 1 && e.path[0] == "bindings" && e.path[1..].join(".") == id)
        .collect();
    found.sort_by_key(|e| e.key_start);
    found
}

/// The text edit: `value` set (`Some`) or the entries removed (`None`);
/// `None` when the file's layout needs a document edit (an inline table).
fn edit_as_text(
    text: &str,
    doc: &toml_edit::Document<String>,
    id: &str,
    value: Option<&str>,
) -> Option<String> {
    use crate::toml_text::{
        TableForm, add_line, key_text, remove_lines, replace_value, table_form,
    };
    let found = binding_entries(doc, id);
    match (value, found.split_first()) {
        (None, _) => remove_lines(text, &found.iter().collect::<Vec<_>>()),
        (Some(value), Some((first, rest))) => {
            // The others are on later lines: removing them leaves the
            // first one's place as it is (a value in an inline table is
            // replaced in place too; a line can't leave one).
            let kept = remove_lines(text, &rest.iter().collect::<Vec<_>>())?;
            Some(replace_value(&kept, first, value))
        }
        (Some(value), None) => {
            if !matches!(
                table_form(doc, &["bindings"]),
                TableForm::Header | TableForm::Missing
            ) {
                return None;
            }
            let parts: Vec<&str> = id.split('.').collect();
            match parts.split_first() {
                Some((group, rest)) if !rest.is_empty() => {
                    let rest_key = rest
                        .iter()
                        .map(|p| key_text(p))
                        .collect::<Vec<_>>()
                        .join(".");
                    match table_form(doc, &["bindings", group]) {
                        // `[bindings.clip]`: under that header.
                        TableForm::Header => add_line(
                            text,
                            doc,
                            &["bindings", group],
                            &format!("{rest_key} = {value}"),
                        ),
                        TableForm::Dotted | TableForm::Missing => add_line(
                            text,
                            doc,
                            &["bindings"],
                            &format!("{}.{rest_key} = {value}", key_text(group)),
                        ),
                        // `clip` is something else: the id as one name.
                        TableForm::Root | TableForm::Other => add_line(
                            text,
                            doc,
                            &["bindings"],
                            &format!("{} = {value}", key_text(id)),
                        ),
                    }
                }
                _ => add_line(
                    text,
                    doc,
                    &["bindings"],
                    &format!("{} = {value}", key_text(id)),
                ),
            }
        }
    }
}

/// The edited text once it reads back with `expected` as `id`'s keys: the
/// text edit, else a `toml_edit` document edit (which keeps the comments
/// but may print a dotted table's keys together), else an error.
fn finish_edit(
    original: &str,
    id: &str,
    text_edit: Option<String>,
    value: Option<String>,
    expected: &[KeyChord],
) -> Result<String, CoreError> {
    let reads_back = |text: &str| {
        crate::toml_text::parse(text).is_ok()
            && resolve(text)
                .binding(id)
                .is_some_and(|b| b.keys == expected)
    };
    if let Some(text) = text_edit.filter(|t| reads_back(t)) {
        return Ok(text);
    }
    match edit_as_document(original, id, value.as_deref()) {
        Some(text) if reads_back(&text) => Ok(text),
        _ => Err(CoreError::invalid(format!(
            "keys.toml is written in a way Qrec can't change `{id}` in"
        ))
        .with_hint("change it by hand: `qrec keys` shows the keys it ends up with")),
    }
}

/// [`edit_as_text`]'s fallback on a `toml_edit` document.
fn edit_as_document(text: &str, id: &str, value: Option<&str>) -> Option<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().ok()?;
    let bindings = doc
        .as_table_mut()
        .entry("bindings")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()?;
    remove_id(bindings, id);
    if let Some(value) = value {
        let value: toml_edit::Value = value.parse().ok()?;
        bindings.insert(id, toml_edit::Item::Value(value));
    }
    Some(doc.to_string())
}

/// Removes every entry of `table` that names `id` (`a.b` as one quoted
/// name, or dotted).
fn remove_id(table: &mut dyn toml_edit::TableLike, id: &str) {
    table.remove(id);
    if let Some((first, rest)) = id.split_once('.')
        && let Some(inner) = table
            .get_mut(first)
            .and_then(toml_edit::Item::as_table_like_mut)
    {
        remove_id(inner, rest);
    }
}

// ----------------------------------------------------------------- views

/// One row of `qrec keys --json` (A§9.7):
/// `{"id","group","label","keys":[..],"cli","contexts"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingView {
    pub id: String,
    pub group: String,
    pub label: String,
    /// Chords in the `keys.toml` grammar (`ctrl+shift+z`); empty when unbound.
    pub keys: Vec<String>,
    /// The CLI equivalent; `null` for actions only the GUI has.
    pub cli: Option<String>,
    pub contexts: Vec<KeyContext>,
}

/// `qrec keys --json` and `Query::Keys` (A§9.7): every binding, in table
/// order. Conflicts and `keys.toml` errors aren't rows; [`problems`] words
/// them for notices.
pub fn keys_view(map: &Keymap) -> Vec<BindingView> {
    map.bindings
        .iter()
        .map(|b| BindingView {
            id: b.id.to_owned(),
            group: b.group.to_owned(),
            label: b.label.to_owned(),
            keys: b.keys.iter().map(KeyChord::to_string).collect(),
            cli: b.cli.map(str::to_owned),
            contexts: b.contexts.clone(),
        })
        .collect()
}

/// What is wrong with a keymap, one line each: every conflict ("x is bound
/// to clip.remove and autocut.run in global; neither runs …"), then every
/// `keys.toml` error. Empty for a clean keymap. `qrec keys` prints them
/// (as notices on stderr under `--json`); the GUI shows them on load
/// (SPEC §10.1).
pub fn problems(map: &Keymap) -> Vec<String> {
    let conflicts = map.conflicts.iter().map(|c| {
        format!(
            "{} is bound to {} in {}; neither runs until keys.toml changes one",
            c.chord.display(),
            join_and(&c.ids),
            c.context.as_str()
        )
    });
    conflicts.chain(map.errors.iter().cloned()).collect()
}

/// `a`, `a and b`, `a, b and c`.
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The human form of `qrec keys` (the cheat sheet): per group, each
/// binding's keys, label and id (the name `keys.toml` uses), then the
/// [`problems`] (conflicts and `keys.toml` errors). CLI forms are in
/// [`keys_view`].
pub fn cheat_sheet(map: &Keymap) -> String {
    let keys_of = |b: &Binding| {
        if b.keys.is_empty() {
            "(unbound)".to_owned()
        } else {
            b.keys
                .iter()
                .map(KeyChord::display)
                .collect::<Vec<_>>()
                .join("  ")
        }
    };
    let width = |text: &str| text.chars().count();
    let key_width = map
        .bindings
        .iter()
        .map(|b| width(&keys_of(b)))
        .max()
        .unwrap_or(0);
    let label_width = map
        .bindings
        .iter()
        .map(|b| width(b.label))
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for group in GROUPS {
        let rows: Vec<&Binding> = map.bindings.iter().filter(|b| b.group == group).collect();
        if rows.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(group);
        out.push('\n');
        for b in rows {
            let keys = keys_of(b);
            out.push_str(&format!(
                "  {keys}{}  {}{}  {}\n",
                " ".repeat(key_width - width(&keys)),
                b.label,
                " ".repeat(label_width - width(b.label)),
                b.id,
            ));
        }
    }
    let problems = problems(map);
    if !problems.is_empty() {
        out.push_str("\nProblems\n");
        for p in problems {
            out.push_str(&format!("  {p}\n"));
        }
    }
    out
}

// ----------------------------------------------------------------- hints

/// One hint of a status-bar line.
struct HintDef {
    /// Bindings whose keys are shown, in order; none for a label-only hint.
    ids: &'static [&'static str],
    /// The chord shown when the binding still has it (`x` rather than `Del`).
    prefer: Option<&'static str>,
    label: &'static str,
}

const fn hint(ids: &'static [&'static str], label: &'static str) -> HintDef {
    HintDef {
        ids,
        prefer: None,
        label,
    }
}

/// SPEC §10.1, verbatim.
const IDLE_HINTS: &[HintDef] = &[
    hint(&["record.toggle"], "record"),
    hint(&["record.over"], "record over"),
    hint(&["clip.split"], "split"),
    hint(&["autocut.run"], "autocut"),
    hint(&["keys.help"], "all keys"),
];

const COUNTDOWN_HINTS: &[HintDef] = &[
    hint(&["cancel"], "cancel"),
    hint(&["record.toggle"], "cancel"),
];

const RECORDING_HINTS: &[HintDef] = &[
    hint(&["record.toggle"], "stop"),
    hint(&["cancel"], "cancel take (files kept)"),
    hint(&["canvas.mode"], "switch view"),
    hint(&["zoom.in", "zoom.out"], "zoom"),
    hint(&["zoom.fit"], "fit"),
];

const FINALIZING_HINTS: &[HintDef] = &[
    hint(
        &[],
        "finalizing the take, waiting for every file to close (up to 10 s)",
    ),
    hint(&["take.discard-last"], "discard it"),
    hint(&["canvas.mode"], "switch view"),
    hint(&["zoom.in", "zoom.out"], "zoom"),
];

const CLIP_SELECTED_HINTS: &[HintDef] = &[
    hint(&["clip.split"], "split"),
    hint(&["clip.trim-start", "clip.trim-end"], "trim to playhead"),
    hint(&["clip.move-left", "clip.move-right"], "move"),
    HintDef {
        ids: &["clip.remove"],
        prefer: Some("x"),
        label: "remove",
    },
    hint(&["autocut.run"], "autocut"),
    hint(&["record.over"], "record over"),
    hint(&["zoom.selection"], "zoom to clip"),
    hint(&["keys.help"], "all keys"),
];

const DRAGGING_HINTS: &[HintDef] = &[
    hint(&[], "release to apply"),
    hint(&["cancel"], "cancel drag"),
    hint(&["snapping.toggle"], "snapping on/off"),
];

fn hint_defs(ctx: HintContext) -> &'static [HintDef] {
    match ctx {
        HintContext::Idle => IDLE_HINTS,
        HintContext::Countdown => COUNTDOWN_HINTS,
        HintContext::Recording => RECORDING_HINTS,
        HintContext::Finalizing => FINALIZING_HINTS,
        HintContext::ClipSelected => CLIP_SELECTED_HINTS,
        HintContext::Dragging => DRAGGING_HINTS,
    }
}

/// "r record · o record over · s split · c autocut · ? all keys": the
/// status-bar hints for `ctx`, with the keys of `map` (rebound keys show;
/// unbound or conflicting ones drop out).
pub fn hints(ctx: HintContext, map: &Keymap) -> Vec<Hint> {
    let mut out = Vec::new();
    for def in hint_defs(ctx) {
        let keys: Vec<String> = def
            .ids
            .iter()
            .filter_map(|id| {
                let binding = map.binding(id)?;
                let usable = usable_keys(map, binding);
                let preferred = def
                    .prefer
                    .and_then(|p| KeyChord::parse(p).ok())
                    .and_then(|p| usable.iter().find(|k| ***k == p).copied());
                preferred.or(usable.first().copied()).map(KeyChord::display)
            })
            .collect();
        if !def.ids.is_empty() && keys.is_empty() {
            continue;
        }
        out.push(Hint {
            keys: keys.join(" "),
            label: def.label.to_owned(),
        });
    }
    out
}

/// The hints as one line: `r record · o record over · …`.
pub fn format_hints(hints: &[Hint]) -> String {
    hints
        .iter()
        .map(|h| {
            if h.keys.is_empty() {
                h.label.clone()
            } else {
                format!("{} {}", h.keys, h.label)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Which hints fit `state`: the record phase first, then a drag in
/// progress (GUI-only knowledge), then whether clips are selected.
pub fn hint_context(state: &State, dragging: bool) -> HintContext {
    match &state.runtime.record {
        RecordPhase::Countdown { .. } => HintContext::Countdown,
        RecordPhase::Starting { .. } | RecordPhase::Recording { .. } => HintContext::Recording,
        RecordPhase::Finalizing { .. } => HintContext::Finalizing,
        RecordPhase::Idle if dragging => HintContext::Dragging,
        RecordPhase::Idle if !state.ui.selection.is_empty() => HintContext::ClipSelected,
        RecordPhase::Idle => HintContext::Idle,
    }
}

// --------------------------------------------------------------- actions

/// What a binding does in a state.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Dispatch this command.
    Run(Command),
    /// The GUI does it itself: overlays, settings, and zooms that need the
    /// viewport width.
    Gui,
    /// Not possible right now; the reason is shown in the palette.
    Unavailable(String),
}

/// The command a binding runs in `state` (shared by the GUI and the
/// palette). `None` for unknown ids, actions the GUI performs itself
/// ([`Action::Gui`]) and actions that aren't available now.
pub fn command_for(id: &str, state: &State) -> Option<Command> {
    match action_for(id, state)? {
        Action::Run(cmd) => Some(cmd),
        Action::Gui | Action::Unavailable(_) => None,
    }
}

/// What binding `id` does in `state`; `None` for an unknown id.
///
/// The command mirrors the key: `s` splits the clip under the playhead
/// (`clipSplit` with `timeline`), `[`/`]` trim the selected clip or the clip
/// under the playhead to it, `Alt+[`/`Alt+]` nudge the start edge later and
/// the end edge earlier by one frame (`Alt+{`/`Alt+}`: 10), `o` records over
/// the contiguous selection, `Esc` cancels a countdown or recording, else
/// clears the selection, else deselects the layer. The gates of `apply`
/// (recording, GUI host, invalid `qrec.json`) make a command unavailable
/// here too, so the palette can say why.
pub fn action_for(id: &str, state: &State) -> Option<Action> {
    let action = match id {
        "record.toggle" => record_toggle(state),
        "record.over" => record_over(state),
        "take.discard-last" => discard_last(state),
        "cancel" => cancel(state),
        "play.toggle" => play_toggle(state),
        "shuttle.reverse" => playback(
            state,
            Command::Shuttle {
                dir: ShuttleDir::Reverse,
            },
        ),
        "shuttle.stop" => run(Command::Shuttle {
            dir: ShuttleDir::Stop,
        }),
        "shuttle.forward" => playback(
            state,
            Command::Shuttle {
                dir: ShuttleDir::Forward,
            },
        ),
        "seek.prev-frame" => seek_by(Sign::Minus, TimeValue::Frames(1)),
        "seek.next-frame" => seek_by(Sign::Plus, TimeValue::Frames(1)),
        "seek.back" => seek_by(Sign::Minus, TimeValue::Seconds(NS_PER_SEC as u64)),
        "seek.forward" => seek_by(Sign::Plus, TimeValue::Seconds(NS_PER_SEC as u64)),
        "seek.prev-edit" => run(Command::Seek {
            to: SeekTarget::EditPrev,
        }),
        "seek.next-edit" => run(Command::Seek {
            to: SeekTarget::EditNext,
        }),
        "seek.start" => run(Command::Seek {
            to: SeekTarget::Start,
        }),
        "seek.end" => run(Command::Seek {
            to: SeekTarget::End,
        }),
        "select.prev" => needs_clips(state, || Command::Select { op: SelectOp::Prev }),
        "select.next" => needs_clips(state, || Command::Select { op: SelectOp::Next }),
        "select.all" => needs_clips(state, || Command::Select { op: SelectOp::All }),
        "clip.move-left" => move_selection(state, -1),
        "clip.move-right" => move_selection(state, 1),
        "clip.split" => split(state),
        "clip.trim-start" => trim_to_playhead(state, Edge::Start),
        "clip.trim-end" => trim_to_playhead(state, Edge::End),
        "clip.nudge-start" => nudge_edge(state, Edge::Start, 1),
        "clip.nudge-end" => nudge_edge(state, Edge::End, 1),
        "clip.nudge-start-10" => nudge_edge(state, Edge::Start, 10),
        "clip.nudge-end-10" => nudge_edge(state, Edge::End, 10),
        "clip.remove" => {
            let selection = selected(state);
            if selection.is_empty() {
                Action::Unavailable("Select the clips to remove".to_owned())
            } else {
                run(Command::ClipRemove { clips: selection })
            }
        }
        "autocut.run" => autocut(state, false),
        "autocut.preview" => autocut(state, true),
        "history.undo" => {
            let h = &state.history;
            if h.entries.iter().any(|e| e.seq == h.cursor) {
                run(Command::Undo { steps: 1 })
            } else {
                Action::Unavailable("Nothing to undo".to_owned())
            }
        }
        "history.redo" => {
            let h = &state.history;
            if h.entries.iter().any(|e| e.seq > h.cursor) {
                run(Command::Redo { steps: 1 })
            } else {
                Action::Unavailable("Nothing to redo".to_owned())
            }
        }
        // D-19: the palette's "Clear undo history" (no key); the core asks
        // for the confirmation (SPEC §11).
        "history.clear" => {
            if state.history.entries.is_empty() {
                Action::Unavailable("The undo history is empty".to_owned())
            } else {
                run(Command::HistoryClear)
            }
        }
        "frame.save" => {
            let total = total_frames(&state.project.timeline.clips);
            if total == 0 {
                Action::Unavailable("The timeline is empty".to_owned())
            } else {
                run(Command::FrameSave {
                    at: TimeArg::frames(state.ui.playhead.min(total - 1)),
                    layer: None,
                    out: None,
                })
            }
        }
        "export" => {
            if state.project.timeline.clips.is_empty() {
                Action::Unavailable("The timeline is empty".to_owned())
            } else if export_running(state) {
                Action::Unavailable("An export is already running".to_owned())
            } else {
                run(Command::Export {
                    format: ExportFormat::Mp4,
                    quality: Quality::Standard,
                    codec: None,
                    out: None,
                })
            }
        }
        "zoom.fit" => run(Command::SetUi {
            patch: UiPatch {
                zoom: Nullable::Clear,
                ..UiPatch::default()
            },
        }),
        "snapping.toggle" => run(Command::SetUi {
            patch: UiPatch {
                snapping: Some(!state.ui.snapping),
                ..UiPatch::default()
            },
        }),
        "bin.toggle" => run(Command::SetUi {
            patch: UiPatch {
                bin_open: Some(!state.ui.bin_open),
                ..UiPatch::default()
            },
        }),
        "canvas.mode" => run(Command::SetCanvasMode {
            mode: match state.ui.canvas_mode {
                CanvasMode::Live => CanvasMode::Program,
                CanvasMode::Program => CanvasMode::Live,
            },
        }),
        "zoom.in" | "zoom.out" | "zoom.selection" | "keys.help" | "palette.open"
        | "settings.open" | "region.next" | "region.prev" | "app.quit" => Action::Gui,
        "layer.nudge-left" => nudge_layer(state, -1, 0),
        "layer.nudge-right" => nudge_layer(state, 1, 0),
        "layer.nudge-up" => nudge_layer(state, 0, -1),
        "layer.nudge-down" => nudge_layer(state, 0, 1),
        "layer.nudge-left-10" => nudge_layer(state, -10, 0),
        "layer.nudge-right-10" => nudge_layer(state, 10, 0),
        "layer.nudge-up-10" => nudge_layer(state, 0, -10),
        "layer.nudge-down-10" => nudge_layer(state, 0, 10),
        _ => return None,
    };
    Some(match action {
        Action::Run(cmd) => gate(state, cmd),
        other => other,
    })
}

fn run(cmd: Command) -> Action {
    Action::Run(cmd)
}

/// Why `Space`, `J` and `L` don't play in Live mode (GUI D-01): the canvas
/// shows the live sources there. `qrec play` still switches to Program.
pub const PLAY_IN_LIVE: &str = "Switch to Program (Tab) to play the timeline";

/// Why the gates `apply` enforces for every command (A§5.3, A§8.5, A§7.7)
/// would refuse `cmd` in `state`, in words for people: the record phase,
/// the GUI host, an invalid `qrec.json` on disk. `None` when they let it
/// through (the command itself may still fail). The GUI enables controls
/// with it (`view::enabled`, D-43).
pub fn gate_reason(state: &State, cmd: &Command) -> Option<String> {
    let phase = &state.runtime.record;
    if !phase.is_idle() && !cmd.allowed_while_recording(phase) {
        return Some(match phase {
            RecordPhase::Countdown { .. } => "Not during the countdown".to_owned(),
            RecordPhase::Finalizing { take, .. } => {
                format!("Not while take {} is finalizing", take.number())
            }
            _ => "Not while recording".to_owned(),
        });
    }
    if cmd.gui_only() && state.runtime.host != HostKind::Gui {
        return Some("Needs the Qrec window".to_owned());
    }
    if state.runtime.disk_invalid.is_some() && cmd.edits_project() {
        return Some("qrec.json on disk is invalid".to_owned());
    }
    None
}

/// `cmd` unless a gate refuses it ([`gate_reason`]).
fn gate(state: &State, cmd: Command) -> Action {
    match gate_reason(state, &cmd) {
        Some(why) => Action::Unavailable(why),
        None => Action::Run(cmd),
    }
}

fn needs_clips(state: &State, cmd: impl FnOnce() -> Command) -> Action {
    if state.project.timeline.clips.is_empty() {
        Action::Unavailable("The timeline is empty".to_owned())
    } else {
        run(cmd())
    }
}

fn seek_by(sign: Sign, value: TimeValue) -> Action {
    run(Command::Seek {
        to: SeekTarget::By(TimeArg { sign, value }),
    })
}

fn export_running(state: &State) -> bool {
    state
        .runtime
        .jobs
        .values()
        .any(|j| matches!(j.kind, JobKind::Export { .. }))
}

/// Why a take can't start now, if it can't (A§5.3): the engine is stuck
/// (future starts fail until a restart), the scene is empty, an export
/// runs. The header's Record button says the same (`view::header`).
fn start_blocker(state: &State) -> Option<String> {
    if state.runtime.engine_stuck.is_some() {
        Some("The capture engine is stuck (GPU driver?); restart Qrec".to_owned())
    } else if state.project.scene.is_empty() {
        Some("Add a layer or audio track first".to_owned())
    } else if export_running(state) {
        Some("Wait for the export to finish".to_owned())
    } else {
        None
    }
}

fn record_toggle(state: &State) -> Action {
    match &state.runtime.record {
        RecordPhase::Idle => match start_blocker(state) {
            Some(why) => Action::Unavailable(why),
            None => run(Command::RecordToggle { wait: false }),
        },
        RecordPhase::Finalizing { take, .. } => {
            Action::Unavailable(format!("Take {} is finalizing", take.number()))
        }
        // Countdown: cancels it; Starting/Recording: stops.
        _ => run(Command::RecordToggle { wait: false }),
    }
}

fn record_over(state: &State) -> Action {
    let selection = selected(state);
    if selection.is_empty() {
        return Action::Unavailable("Select the clips to record over".to_owned());
    }
    if !contiguous(&state.project.timeline.clips, &selection) {
        return Action::Unavailable("Needs a contiguous selection".to_owned());
    }
    if let Some(why) = start_blocker(state).filter(|_| state.runtime.record.is_idle()) {
        return Action::Unavailable(why);
    }
    run(Command::RecordStart {
        mode: RecordModeArg::Over(selection),
        countdown: None,
        wait: false,
    })
}

fn discard_last(state: &State) -> Action {
    if state.runtime.record.is_idle() {
        // A§5.5: the newest placed take, and only while its clips are on the timeline.
        let newest = state
            .project
            .takes
            .iter()
            .filter(|(_, t)| t.placed)
            .max_by_key(|(id, _)| id.number());
        match newest {
            None => return Action::Unavailable("No take to discard yet".to_owned()),
            Some((id, _)) if !state.project.timeline.clips.iter().any(|c| &c.take == id) => {
                return Action::Unavailable(format!("Take {} is already in the bin", id.number()));
            }
            Some(_) => {}
        }
    }
    run(Command::TakeDiscardLast)
}

fn cancel(state: &State) -> Action {
    match &state.runtime.record {
        RecordPhase::Countdown { .. }
        | RecordPhase::Starting { .. }
        | RecordPhase::Recording { .. } => run(Command::RecordCancel),
        RecordPhase::Finalizing { take, .. } => {
            Action::Unavailable(format!("Take {} is finalizing", take.number()))
        }
        RecordPhase::Idle if !state.ui.selection.is_empty() => run(Command::Select {
            op: SelectOp::Clear,
        }),
        RecordPhase::Idle if state.ui.selected_layer.is_some() => run(Command::SetUi {
            patch: UiPatch {
                selected_layer: Nullable::Clear,
                ..UiPatch::default()
            },
        }),
        RecordPhase::Idle => Action::Unavailable("Nothing to cancel".to_owned()),
    }
}

fn play_toggle(state: &State) -> Action {
    if state.runtime.playing {
        run(Command::Pause)
    } else {
        playback(state, Command::Play)
    }
}

/// `Play` or a shuttle: needs clips, passes the gates, and only plays on
/// the Program canvas (D-01). A refusing gate (recording, no GUI host)
/// says why first: switching to Program wouldn't help then.
fn playback(state: &State, cmd: Command) -> Action {
    if state.project.timeline.clips.is_empty() {
        return Action::Unavailable("The timeline is empty".to_owned());
    }
    if let Some(why) = gate_reason(state, &cmd) {
        return Action::Unavailable(why);
    }
    if state.ui.canvas_mode == CanvasMode::Live {
        return Action::Unavailable(PLAY_IN_LIVE.to_owned());
    }
    run(cmd)
}

/// The selected clips that are on the timeline, in timeline order.
fn selected(state: &State) -> Vec<ClipId> {
    state
        .project
        .timeline
        .clips
        .iter()
        .filter(|c| state.ui.selection.contains(&c.id))
        .map(|c| c.id.clone())
        .collect()
}

/// The clips `ids` sit next to each other (all of them on the timeline).
fn contiguous(clips: &[Clip], ids: &[ClipId]) -> bool {
    let mut indices: Vec<usize> = Vec::new();
    for id in ids {
        match clips.iter().position(|c| &c.id == id) {
            Some(i) => indices.push(i),
            None => return false,
        }
    }
    indices.sort_unstable();
    indices.dedup();
    !indices.is_empty() && indices.windows(2).all(|w| w[1] == w[0] + 1)
}

fn move_selection(state: &State, by: i64) -> Action {
    let selection = selected(state);
    if selection.is_empty() {
        Action::Unavailable("Select the clips to move".to_owned())
    } else {
        run(Command::ClipMove {
            clips: selection,
            to: MoveTo::By(by),
        })
    }
}

fn split(state: &State) -> Action {
    let clips = &state.project.timeline.clips;
    match clip_at(clips, state.ui.playhead) {
        None => Action::Unavailable("Move the playhead onto a clip".to_owned()),
        Some((_, 0)) => Action::Unavailable("The playhead is on a cut".to_owned()),
        Some(_) => run(Command::ClipSplit {
            clip: None,
            at: TimeArg::frames(state.ui.playhead),
            timeline: true,
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Start,
    End,
}

/// The single selected clip, with its index and timeline start.
fn single_selected(state: &State) -> Option<(usize, Frame, &Clip)> {
    let clips = &state.project.timeline.clips;
    let mut selected = clips
        .iter()
        .enumerate()
        .filter(|(_, c)| state.ui.selection.contains(&c.id));
    let (i, clip) = selected.next()?;
    if selected.next().is_some() {
        return None;
    }
    Some((i, position_of(clips, i), clip))
}

/// `[`/`]` act on the selected clip when the playhead is on it, else on the
/// clip under the playhead (for `]`, the clip that ends at or contains it).
fn trim_to_playhead(state: &State, edge: Edge) -> Action {
    let clips = &state.project.timeline.clips;
    let p = state.ui.playhead;
    let on_selected = single_selected(state).filter(|(_, start, clip)| match edge {
        Edge::Start => *start <= p && p < start + clip.len(),
        Edge::End => *start < p && p <= start + clip.len(),
    });
    let target = match on_selected {
        Some((i, _, _)) => Some(i),
        None => match edge {
            Edge::Start => clip_at(clips, p).map(|(i, _)| i),
            Edge::End => p
                .checked_sub(1)
                .and_then(|q| clip_at(clips, q))
                .map(|(i, _)| i),
        },
    };
    let Some(i) = target else {
        return Action::Unavailable("Move the playhead onto a clip".to_owned());
    };
    let at = Some(TimeArg::frames(p));
    run(Command::ClipTrim {
        clip: clips[i].id.clone(),
        start: if edge == Edge::Start { at } else { None },
        end: if edge == Edge::End { at } else { None },
        timeline: true,
    })
}

/// `Alt+[` moves the start edge `frames` later, `Alt+]` the end edge
/// `frames` earlier, on the selected clip (else the clip under the playhead).
fn nudge_edge(state: &State, edge: Edge, frames: u64) -> Action {
    let clips = &state.project.timeline.clips;
    let clip = if state.ui.selection.is_empty() {
        clip_at(clips, state.ui.playhead).map(|(i, _)| &clips[i])
    } else if let Some((_, _, clip)) = single_selected(state) {
        Some(clip)
    } else {
        return Action::Unavailable("Select one clip".to_owned());
    };
    let Some(clip) = clip else {
        return Action::Unavailable("Select a clip or move the playhead onto one".to_owned());
    };
    if clip.len() <= frames {
        return Action::Unavailable("The clip is too short".to_owned());
    }
    run(Command::ClipTrim {
        clip: clip.id.clone(),
        start: (edge == Edge::Start).then_some(TimeArg::plus_frames(frames)),
        end: (edge == Edge::End).then_some(TimeArg::minus_frames(frames)),
        timeline: false,
    })
}

fn autocut(state: &State, dry_run: bool) -> Action {
    if state.project.timeline.clips.is_empty() {
        return Action::Unavailable("The timeline is empty".to_owned());
    }
    let selection = selected(state);
    run(Command::Autocut {
        scope: if selection.is_empty() {
            ClipScope::All
        } else {
            ClipScope::Clips(selection)
        },
        overrides: AutocutOverrides::default(),
        dry_run,
    })
}

/// Arrow keys on the canvas move the selected layer of the scene the
/// person edits (`ui_normalize::edit_target`, D-06: the project scene in
/// Live mode, the take of the first selected clip in Program mode), unless
/// it is locked. Program mode with nothing selected is read-only.
fn nudge_layer(state: &State, dx: i32, dy: i32) -> Action {
    let Some(target) = edit_target(&state.project, &state.ui) else {
        return Action::Unavailable(NO_EDIT_TARGET.to_owned());
    };
    let Some(layer_id) = &state.ui.selected_layer else {
        return Action::Unavailable("Select a layer on the canvas".to_owned());
    };
    let scene = match &target {
        SceneTarget::Project => Some(&state.project.scene),
        SceneTarget::Take(take) => state.project.take(take).map(|t| &t.scene),
    };
    let Some(layer) = scene.and_then(|s| s.layer(layer_id)) else {
        return Action::Unavailable(format!("Layer {layer_id} isn't in this scene"));
    };
    if layer.locked {
        return Action::Unavailable(format!("Layer {layer_id} is locked"));
    }
    let rect = Rect {
        x: layer.rect.x.saturating_add(dx),
        y: layer.rect.y.saturating_add(dy),
        ..layer.rect
    };
    run(Command::LayerSet {
        target,
        layer: layer_id.clone(),
        patch: LayerPatch {
            rect: Some(rect),
            ..LayerPatch::default()
        },
    })
}

// --------------------------------------------------------------- palette

/// One row of the command palette: `Split clip at the playhead — s — qrec clip split <clip> --at <t>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaletteRow {
    pub id: String,
    pub group: String,
    pub label: String,
    /// The keys as shown (`Ctrl+K`, `:`); conflicting keys are left out.
    pub keys: Vec<String>,
    pub cli: Option<String>,
    pub enabled: bool,
    /// Why the row is disabled ("Needs a contiguous selection").
    pub reason: Option<String>,
}

impl PaletteRow {
    /// `label — keys — cli`, with `—` for a missing part.
    pub fn line(&self) -> String {
        let keys = if self.keys.is_empty() {
            "—".to_owned()
        } else {
            self.keys.join(" ")
        };
        format!(
            "{} — {} — {}",
            self.label,
            keys,
            self.cli.as_deref().unwrap_or("—")
        )
    }
}

/// Every binding as a palette row, in cheat-sheet order, enabled when it
/// can run in `state` (GUI-only actions are always enabled).
pub fn palette(map: &Keymap, state: &State) -> Vec<PaletteRow> {
    let mut rows = Vec::new();
    for group in GROUPS {
        for b in map.bindings.iter().filter(|b| b.group == group) {
            let action = action_for(b.id, state);
            let reason = match &action {
                Some(Action::Unavailable(why)) => Some(why.clone()),
                _ => None,
            };
            rows.push(PaletteRow {
                id: b.id.to_owned(),
                group: b.group.to_owned(),
                label: b.label.to_owned(),
                keys: usable_keys(map, b)
                    .into_iter()
                    .map(KeyChord::display)
                    .collect(),
                cli: b.cli.map(str::to_owned),
                enabled: reason.is_none(),
                reason,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{LayerId, TakeId};
    use crate::state::{Job, RecordMode};
    use crate::testkit::{self, ProjectBuilder};

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn chord_grammar() {
        let z = chord("ctrl+shift+z");
        assert_eq!(
            z,
            KeyChord {
                key: "z".into(),
                ctrl: true,
                alt: false,
                shift: true,
                super_: false
            }
        );
        assert_eq!(z.to_string(), "ctrl+shift+z");
        assert_eq!(z.display(), "Ctrl+Shift+Z");
        assert_eq!(chord("Ctrl+Shift+Z"), z, "case doesn't matter");
        assert_eq!(chord(" ctrl + shift + z "), z, "spaces don't matter");
        assert_eq!(chord("shift+ctrl+z"), z, "modifier order doesn't matter");
        assert_eq!(chord("control+shift+Z"), z);

        assert_eq!(chord("<"), chord("less"));
        assert_eq!(chord("less").display(), "<");
        assert_eq!(chord("alt+[").to_string(), "alt+bracketleft");
        assert_eq!(chord("alt+bracketleft").display(), "Alt+[");
        assert_eq!(chord("?").to_string(), "question");
        assert_eq!(chord("esc").to_string(), "Escape");
        assert_eq!(chord("Escape").display(), "Esc");
        assert_eq!(chord("shift+left").to_string(), "shift+Left");
        assert_eq!(chord("shift+Left").display(), "Shift+←");
        assert_eq!(chord("space").display(), "Space");
        assert_eq!(chord("del").to_string(), "Delete");
        assert_eq!(chord("backspace").to_string(), "BackSpace");
        assert_eq!(chord("pgup").to_string(), "Page_Up");
        assert_eq!(chord("f5").to_string(), "F5");
        assert_eq!(chord("F12").display(), "F12");
        assert_eq!(chord("r").display(), "r", "bare letters stay lowercase");
        assert_eq!(chord("shift+c").display(), "Shift+C");
        assert_eq!(chord("C"), chord("c"), "an uppercase letter isn't shift");
        assert_eq!(chord("ctrl+,").to_string(), "ctrl+comma");
        assert_eq!(chord("ctrl+,").display(), "Ctrl+,");
        assert_eq!(chord("+").to_string(), "plus");
        assert_eq!(chord("ctrl++").to_string(), "ctrl+plus");
        assert_eq!(chord("ctrl+plus").display(), "Ctrl++");
        assert_eq!(chord("1").to_string(), "1");
        assert_eq!(chord("super+r").to_string(), "super+r");
        assert_eq!(chord("shift+Tab").to_string(), "shift+Tab");

        for bad in [
            "",
            "ctrl+",
            "+a",
            "hyper+a",
            "ctrl+ctrl+a",
            "bogus",
            "ctrl+bogus",
            "shift+comma",
            "shift+<",
            "shift+1",
            "f0",
            "f25",
            "f01",
            "ctrl++a",
        ] {
            assert!(KeyChord::parse(bad).is_err(), "{bad:?} should not parse");
        }
        let err = "shift+,".parse::<KeyChord>().unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Usage);
        assert!(
            err.message.contains("bind that character instead"),
            "{}",
            err.message
        );
    }

    #[test]
    fn chords_from_gdk_events() {
        assert_eq!(
            KeyChord::from_gdk("C", false, false, true, false),
            Some(chord("shift+c"))
        );
        assert_eq!(
            KeyChord::from_gdk("less", false, false, true, false),
            Some(chord("less"))
        );
        assert_eq!(
            KeyChord::from_gdk("braceleft", false, true, true, false),
            Some(chord("alt+braceleft"))
        );
        assert_eq!(
            KeyChord::from_gdk("ISO_Left_Tab", false, false, true, false),
            Some(chord("shift+Tab"))
        );
        assert_eq!(
            KeyChord::from_gdk("Left", false, false, true, false),
            Some(chord("shift+Left"))
        );
        assert_eq!(
            KeyChord::from_gdk("Z", true, false, false, false),
            Some(chord("ctrl+z")),
            "caps lock"
        );
        assert_eq!(
            KeyChord::from_gdk("Shift_L", false, false, true, false),
            None
        );
        assert_eq!(
            KeyChord::from_gdk("eacute", false, false, false, false),
            None
        );
    }

    #[test]
    fn defaults_have_no_errors_or_conflicts() {
        let map = resolve("");
        assert!(map.errors.is_empty(), "{:?}", map.errors);
        assert!(map.conflicts.is_empty(), "{:?}", map.conflicts);
        assert_eq!(map.bindings.len(), DEFAULTS.len());
        let ids: BTreeSet<&str> = map.bindings.iter().map(|b| b.id).collect();
        assert_eq!(ids.len(), map.bindings.len(), "ids are unique");
        for b in &map.bindings {
            assert!(!b.keys.is_empty(), "{} has a default key", b.id);
            assert!(GROUPS.contains(&b.group), "{} has a known group", b.id);
            assert!(!b.contexts.is_empty(), "{} has a context", b.id);
            assert!(action_for(b.id, &testkit::state(ProjectBuilder::new().build())).is_some());
            if let Some(cli) = b.cli {
                assert!(cli.starts_with("qrec "), "{cli}");
            }
        }
        assert_eq!(
            map.binding("clip.remove").unwrap().keys,
            vec![chord("Delete"), chord("x"), chord("BackSpace")],
            "SPEC §10.2 plus Backspace"
        );
        assert_eq!(
            action_for("bogus", &testkit::state(ProjectBuilder::new().build())),
            None
        );
    }

    #[test]
    fn spec_table_keys() {
        // SPEC §10.2, row by row.
        let map = resolve("");
        let keys = |id: &str| -> Vec<String> {
            map.binding(id)
                .unwrap()
                .keys
                .iter()
                .map(KeyChord::display)
                .collect()
        };
        assert_eq!(keys("record.toggle"), ["r"]);
        assert_eq!(keys("record.over"), ["o"]);
        assert_eq!(keys("take.discard-last"), ["d"]);
        assert_eq!(keys("cancel"), ["Esc"]);
        assert_eq!(keys("play.toggle"), ["Space"]);
        assert_eq!(
            [
                keys("shuttle.reverse"),
                keys("shuttle.stop"),
                keys("shuttle.forward")
            ]
            .concat(),
            ["j", "k", "l"]
        );
        assert_eq!(
            [keys("seek.prev-frame"), keys("seek.next-frame")].concat(),
            ["←", "→"]
        );
        assert_eq!(
            [keys("seek.back"), keys("seek.forward")].concat(),
            ["Shift+←", "Shift+→"]
        );
        assert_eq!(
            [keys("seek.prev-edit"), keys("seek.next-edit")].concat(),
            ["↑", "↓"]
        );
        assert_eq!(
            [keys("seek.start"), keys("seek.end")].concat(),
            ["Home", "End"]
        );
        assert_eq!(
            [keys("select.prev"), keys("select.next")].concat(),
            [",", "."]
        );
        assert_eq!(
            [keys("clip.move-left"), keys("clip.move-right")].concat(),
            ["<", ">"]
        );
        assert_eq!(keys("clip.split"), ["s"]);
        assert_eq!(
            [keys("clip.trim-start"), keys("clip.trim-end")].concat(),
            ["[", "]"]
        );
        assert_eq!(
            [keys("clip.nudge-start"), keys("clip.nudge-end")].concat(),
            ["Alt+[", "Alt+]"]
        );
        assert_eq!(
            [keys("clip.nudge-start-10"), keys("clip.nudge-end-10")].concat(),
            ["Alt+{", "Alt+}"]
        );
        assert_eq!(keys("clip.remove"), ["Del", "x", "Backspace"]);
        assert_eq!(keys("autocut.run"), ["c"]);
        assert_eq!(keys("autocut.preview"), ["Shift+C"]);
        assert_eq!(keys("history.undo"), ["Ctrl+Z"]);
        assert_eq!(keys("history.redo"), ["Ctrl+Shift+Z", "Ctrl+Y"]);
        assert_eq!(keys("select.all"), ["Ctrl+A"]);
        assert_eq!(keys("frame.save"), ["p"]);
        assert_eq!(keys("export"), ["Ctrl+E"]);
        assert_eq!([keys("zoom.in"), keys("zoom.out")].concat(), ["=", "-"]);
        assert_eq!(keys("zoom.fit"), ["\\"]);
        assert_eq!(keys("zoom.selection"), ["z"]);
        assert_eq!(keys("snapping.toggle"), ["n"]);
        assert_eq!(keys("canvas.mode"), ["Tab"]);
        assert_eq!(keys("bin.toggle"), ["b"]);
        assert_eq!(keys("keys.help"), ["?"]);
        assert_eq!(keys("palette.open"), ["Ctrl+K", ":"]);
        assert_eq!(keys("settings.open"), ["Ctrl+,"]);
    }

    #[test]
    fn overrides_conflicts_and_errors() {
        let map = resolve(
            "# my keys\n[bindings]\nframe.save = \"f\"\n\"autocut.run\" = [\"x\", \"ctrl+shift+a\"]\nrecord.over = []\n",
        );
        assert!(map.errors.is_empty(), "{:?}", map.errors);
        assert_eq!(map.binding("frame.save").unwrap().keys, vec![chord("f")]);
        assert_eq!(
            map.binding("autocut.run").unwrap().keys,
            vec![chord("x"), chord("ctrl+shift+a")]
        );
        assert!(map.binding("record.over").unwrap().keys.is_empty());
        // `x` is still a default of clip.remove: neither runs (SPEC §10.1, settings S22).
        assert_eq!(
            map.conflicts,
            vec![Conflict {
                chord: chord("x"),
                ids: vec!["clip.remove".into(), "autocut.run".into()],
                context: KeyContext::Global,
            }]
        );
        assert_eq!(lookup(&map, &chord("x"), KeyContext::Timeline), None);
        assert_eq!(
            lookup(&map, &chord("Delete"), KeyContext::Timeline)
                .unwrap()
                .id,
            "clip.remove"
        );
        assert_eq!(
            lookup(&map, &chord("f"), KeyContext::Canvas).unwrap().id,
            "frame.save"
        );
        assert_eq!(
            lookup(&map, &chord("p"), KeyContext::Timeline),
            None,
            "p is unbound now"
        );

        // Errors keep the defaults and are reported one per problem.
        let map = resolve(
            "[bindings]\nclip.split = \"ctrl+bogus\"\nnope = \"q\"\nexport = 5\nundo = \"u\"\n[extra]\na = 1\n",
        );
        assert_eq!(map.binding("clip.split").unwrap().keys, vec![chord("s")]);
        assert_eq!(map.errors.len(), 5, "{:#?}", map.errors);
        assert!(
            map.errors
                .iter()
                .any(|e| e.contains("`clip.split`") && e.contains("bogus"))
        );
        assert!(
            map.errors
                .iter()
                .any(|e| e.contains("unknown binding `nope`"))
        );
        assert!(
            map.errors
                .iter()
                .any(|e| e.contains("unknown binding `undo`"))
        );
        assert!(
            map.errors
                .iter()
                .any(|e| e.contains("`export`") && e.contains("found 5"))
        );
        assert!(
            map.errors
                .iter()
                .any(|e| e.contains("unknown section `extra`"))
        );

        let map = resolve("[bindings\n");
        assert_eq!(map.errors.len(), 1);
        assert!(
            map.errors[0].starts_with("keys.toml line 1:"),
            "{}",
            map.errors[0]
        );
        assert_eq!(
            map.bindings,
            resolve("").bindings,
            "a broken file keeps the defaults"
        );
    }

    #[test]
    fn conflicts_follow_contexts() {
        // Left seeks on the timeline and nudges layers on the canvas: no conflict.
        let map = resolve("");
        assert_eq!(
            lookup(&map, &chord("Left"), KeyContext::Timeline)
                .unwrap()
                .id,
            "seek.prev-frame"
        );
        assert_eq!(
            lookup(&map, &chord("Left"), KeyContext::Canvas).unwrap().id,
            "layer.nudge-left"
        );
        assert_eq!(
            lookup(&map, &chord("Left"), KeyContext::Recording)
                .unwrap()
                .id,
            "seek.prev-frame",
            "seeking works while recording; layer edits don't"
        );
        assert_eq!(
            lookup(&map, &chord("r"), KeyContext::Recording).unwrap().id,
            "record.toggle"
        );
        assert_eq!(
            lookup(&map, &chord("s"), KeyContext::Recording),
            None,
            "editing stops while recording"
        );
        assert_eq!(
            lookup(&map, &chord("Escape"), KeyContext::Overlay)
                .unwrap()
                .id,
            "cancel"
        );
        assert_eq!(lookup(&map, &chord("r"), KeyContext::Overlay), None);

        // A global key reused for a timeline-only binding clashes on the timeline only.
        let map = resolve("[bindings]\nseek.prev-frame = \"s\"\n");
        assert_eq!(
            map.conflicts,
            vec![Conflict {
                chord: chord("s"),
                ids: vec!["seek.prev-frame".into(), "clip.split".into()],
                context: KeyContext::Timeline,
            }]
        );
        assert_eq!(
            lookup(&map, &chord("s"), KeyContext::Canvas).unwrap().id,
            "clip.split"
        );
        // Two globals clash once, under global.
        let map = resolve("[bindings]\nzoom.selection = \"r\"\n");
        let contexts: Vec<KeyContext> = map.conflicts.iter().map(|c| c.context).collect();
        assert_eq!(contexts, [KeyContext::Global, KeyContext::Recording]);
    }

    #[test]
    fn hints_follow_the_keymap() {
        let map = resolve("");
        assert_eq!(
            format_hints(&hints(HintContext::Idle, &map)),
            "r record · o record over · s split · c autocut · ? all keys",
            "SPEC §10.1"
        );
        assert_eq!(
            format_hints(&hints(HintContext::ClipSelected, &map)),
            "s split · [ ] trim to playhead · < > move · x remove · c autocut · o record over · z zoom to clip · ? all keys"
        );
        let map = resolve(
            "[bindings]\nrecord.toggle = \"F9\"\nrecord.over = []\nclip.remove = \"Delete\"\n",
        );
        assert_eq!(
            format_hints(&hints(HintContext::Idle, &map)),
            "F9 record · s split · c autocut · ? all keys"
        );
        assert!(format_hints(&hints(HintContext::ClipSelected, &map)).contains("Del remove"));
        // A conflicting key isn't advertised.
        let map = resolve("[bindings]\nautocut.run = \"s\"\n");
        assert_eq!(
            format_hints(&hints(HintContext::Idle, &map)),
            "r record · o record over · ? all keys"
        );
    }

    fn editing_state() -> State {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 300)
            .take("take-0002", 200)
            .clip("c001", "take-0001", 0, 100)
            .clip("c002", "take-0001", 150, 300)
            .clip("c003", "take-0002", 0, 200)
            .build();
        testkit::state(project)
    }

    fn ids(list: &[&str]) -> Vec<ClipId> {
        list.iter().map(|s| ClipId::from_raw(*s)).collect()
    }

    #[test]
    fn commands_for_the_hot_loop() {
        let mut state = editing_state();
        assert_eq!(
            command_for("record.toggle", &state),
            Some(Command::RecordToggle { wait: false })
        );
        assert_eq!(
            action_for("record.over", &state),
            Some(Action::Unavailable(
                "Select the clips to record over".into()
            ))
        );
        state.ui.selection = ids(&["c003", "c001"]);
        assert_eq!(
            action_for("record.over", &state),
            Some(Action::Unavailable("Needs a contiguous selection".into()))
        );
        state.ui.selection = ids(&["c002", "c001"]);
        assert_eq!(
            command_for("record.over", &state),
            Some(Command::RecordStart {
                mode: RecordModeArg::Over(ids(&["c001", "c002"])),
                countdown: None,
                wait: false
            }),
            "the selection in timeline order"
        );
        assert_eq!(
            command_for("cancel", &state),
            Some(Command::Select {
                op: SelectOp::Clear
            })
        );

        // Discard needs a placed take with clips on the timeline (A§5.5).
        assert_eq!(
            action_for("take.discard-last", &state),
            Some(Action::Unavailable("No take to discard yet".into()))
        );
        state
            .project
            .takes
            .get_mut(&TakeId::numbered(2))
            .unwrap()
            .placed = true;
        assert_eq!(
            command_for("take.discard-last", &state),
            Some(Command::TakeDiscardLast)
        );
        state
            .project
            .timeline
            .clips
            .retain(|c| c.take != TakeId::numbered(2));
        assert_eq!(
            action_for("take.discard-last", &state),
            Some(Action::Unavailable("Take 2 is already in the bin".into()))
        );

        // While recording: r stops, Esc cancels, editing is unavailable.
        state.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(3),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        assert_eq!(
            command_for("record.toggle", &state),
            Some(Command::RecordToggle { wait: false })
        );
        assert_eq!(command_for("cancel", &state), Some(Command::RecordCancel));
        assert_eq!(
            action_for("clip.remove", &state),
            Some(Action::Unavailable("Not while recording".into()))
        );
        assert_eq!(
            action_for("take.discard-last", &state),
            Some(Action::Unavailable("Not while recording".into()))
        );
        assert!(matches!(
            command_for("canvas.mode", &state),
            Some(Command::SetCanvasMode { .. })
        ));

        // Finalizing: d is remembered, r and Esc wait.
        state.runtime.record = RecordPhase::Finalizing {
            take: TakeId::numbered(3),
            mode: RecordMode::Append,
            cancelled: false,
            stop_reqs: vec![],
            waiters: vec![],
            discard: None,
        };
        assert_eq!(
            command_for("take.discard-last", &state),
            Some(Command::TakeDiscardLast)
        );
        assert_eq!(
            action_for("record.toggle", &state),
            Some(Action::Unavailable("Take 3 is finalizing".into()))
        );
    }

    #[test]
    fn record_needs_a_scene_and_no_export() {
        let mut state = testkit::state(ProjectBuilder::new().build());
        assert_eq!(
            action_for("record.toggle", &state),
            Some(Action::Unavailable(
                "Add a layer or audio track first".into()
            ))
        );
        state = editing_state();
        state.runtime.jobs.insert(
            crate::ids::JobId(1),
            Job {
                id: crate::ids::JobId(1),
                kind: JobKind::FrameSave {
                    out: "/tmp/x.png".into(),
                    files: Default::default(),
                },
                req: None,
                started: state.project.created_at,
            },
        );
        assert!(
            command_for("record.toggle", &state).is_some(),
            "a frame save doesn't block"
        );
        state.runtime.jobs.insert(
            crate::ids::JobId(2),
            Job {
                id: crate::ids::JobId(2),
                kind: JobKind::Export {
                    out: "/tmp/x.mp4".into(),
                    files: Default::default(),
                    total_frames: 1,
                    seg_frames: vec![],
                    seg_done: vec![],
                    seg_clips: Vec::new(),
                    stitch_total_ns: 0,
                    stitch_done_ns: 0,
                    stitching: false,
                },
                req: None,
                started: state.project.created_at,
            },
        );
        assert_eq!(
            action_for("record.toggle", &state),
            Some(Action::Unavailable("Wait for the export to finish".into()))
        );
        assert_eq!(
            action_for("export", &state),
            Some(Action::Unavailable("An export is already running".into()))
        );
    }

    #[test]
    fn commands_for_editing() {
        let mut state = editing_state();
        // Clips: c001 [0,100) at 0, c002 [150,300) at 100, c003 [0,200) at 250.
        state.ui.playhead = 130;
        assert_eq!(
            command_for("clip.split", &state),
            Some(Command::ClipSplit {
                clip: None,
                at: TimeArg::frames(130),
                timeline: true
            })
        );
        state.ui.playhead = 100;
        assert_eq!(
            action_for("clip.split", &state),
            Some(Action::Unavailable("The playhead is on a cut".into()))
        );
        // `]` at a cut trims the clip that ends there; `[` the one that starts there.
        let trim = |clip: &str, start: Option<u64>, end: Option<u64>| Command::ClipTrim {
            clip: ClipId::from_raw(clip),
            start: start.map(TimeArg::frames),
            end: end.map(TimeArg::frames),
            timeline: true,
        };
        assert_eq!(
            command_for("clip.trim-end", &state),
            Some(trim("c001", None, Some(100)))
        );
        assert_eq!(
            command_for("clip.trim-start", &state),
            Some(trim("c002", Some(100), None))
        );
        // The selected clip wins when the playhead is on it.
        state.ui.playhead = 250;
        state.ui.selection = ids(&["c002"]);
        assert_eq!(
            command_for("clip.trim-end", &state),
            Some(trim("c002", None, Some(250)))
        );
        assert_eq!(
            command_for("clip.trim-start", &state),
            Some(trim("c003", Some(250), None))
        );
        state.ui.playhead = 450;
        assert_eq!(
            action_for("clip.split", &state),
            Some(Action::Unavailable("Move the playhead onto a clip".into()))
        );
        assert_eq!(
            command_for("clip.trim-end", &state),
            Some(trim("c003", None, Some(450)))
        );

        // Nudges: start later, end earlier, on the selected clip.
        assert_eq!(
            command_for("clip.nudge-start", &state),
            Some(Command::ClipTrim {
                clip: ClipId::from_raw("c002"),
                start: Some(TimeArg::plus_frames(1)),
                end: None,
                timeline: false
            })
        );
        assert_eq!(
            command_for("clip.nudge-end-10", &state),
            Some(Command::ClipTrim {
                clip: ClipId::from_raw("c002"),
                start: None,
                end: Some(TimeArg::minus_frames(10)),
                timeline: false
            })
        );
        state.ui.selection = ids(&["c001", "c002"]);
        assert_eq!(
            action_for("clip.nudge-start", &state),
            Some(Action::Unavailable("Select one clip".into()))
        );
        assert_eq!(
            command_for("clip.move-left", &state),
            Some(Command::ClipMove {
                clips: ids(&["c001", "c002"]),
                to: MoveTo::By(-1)
            })
        );
        assert_eq!(
            command_for("clip.remove", &state),
            Some(Command::ClipRemove {
                clips: ids(&["c001", "c002"])
            })
        );
        assert_eq!(
            command_for("autocut.run", &state),
            Some(Command::Autocut {
                scope: ClipScope::Clips(ids(&["c001", "c002"])),
                overrides: AutocutOverrides::default(),
                dry_run: false
            })
        );
        state.ui.selection.clear();
        assert_eq!(
            command_for("autocut.preview", &state),
            Some(Command::Autocut {
                scope: ClipScope::All,
                overrides: AutocutOverrides::default(),
                dry_run: true
            })
        );
        assert_eq!(
            command_for("frame.save", &state),
            Some(Command::FrameSave {
                at: TimeArg::frames(449),
                layer: None,
                out: None
            }),
            "the last frame when the playhead is at the end"
        );
        assert_eq!(command_for("history.undo", &state), None, "nothing to undo");
        assert_eq!(
            action_for("cancel", &state),
            Some(Action::Unavailable("Nothing to cancel".into()))
        );
    }

    #[test]
    fn commands_for_view_and_playback() {
        let mut state = editing_state();
        assert_eq!(
            command_for("canvas.mode", &state),
            Some(Command::SetCanvasMode {
                mode: CanvasMode::Program
            })
        );
        state.ui.canvas_mode = CanvasMode::Program;
        assert_eq!(command_for("play.toggle", &state), Some(Command::Play));
        state.runtime.playing = true;
        assert_eq!(command_for("play.toggle", &state), Some(Command::Pause));
        assert_eq!(
            command_for("seek.back", &state),
            Some(Command::Seek {
                to: SeekTarget::By("-1s".parse().unwrap())
            })
        );
        assert_eq!(
            command_for("seek.next-frame", &state),
            Some(Command::Seek {
                to: SeekTarget::By("+1f".parse().unwrap())
            })
        );
        assert_eq!(
            command_for("canvas.mode", &state),
            Some(Command::SetCanvasMode {
                mode: CanvasMode::Live
            })
        );
        assert_eq!(
            command_for("snapping.toggle", &state),
            Some(Command::SetUi {
                patch: UiPatch {
                    snapping: Some(false),
                    ..UiPatch::default()
                }
            })
        );
        assert_eq!(action_for("zoom.in", &state), Some(Action::Gui));
        assert_eq!(command_for("zoom.in", &state), None);
        // Playback commands need the GUI host.
        state.runtime.host = HostKind::Headless;
        assert_eq!(
            action_for("play.toggle", &state),
            Some(Action::Unavailable("Needs the Qrec window".into()))
        );
        // Edits wait for a valid qrec.json; view changes don't.
        state.runtime.disk_invalid = Some("bad".into());
        state.ui.selection = ids(&["c001"]);
        assert_eq!(
            action_for("clip.remove", &state),
            Some(Action::Unavailable("qrec.json on disk is invalid".into()))
        );
        assert!(command_for("bin.toggle", &state).is_some());
    }

    #[test]
    fn layer_nudges_edit_the_edit_target() {
        let mut state = editing_state();
        assert_eq!(
            action_for("layer.nudge-left", &state),
            Some(Action::Unavailable("Select a layer on the canvas".into()))
        );
        state.ui.selected_layer = Some(LayerId::numbered(1));
        let rect = |x: i32, y: i32| Rect::new(x, y, 1920, 1080);
        let nudge = |target: SceneTarget, x: i32, y: i32| Command::LayerSet {
            target,
            layer: LayerId::numbered(1),
            patch: LayerPatch {
                rect: Some(rect(x, y)),
                ..LayerPatch::default()
            },
        };
        assert_eq!(
            command_for("layer.nudge-left-10", &state),
            Some(nudge(SceneTarget::Project, -10, 0)),
            "Live mode edits the project scene"
        );
        // D-06: Program mode with nothing selected is read-only, though the
        // canvas shows the take under the playhead; the palette says why.
        state.ui.canvas_mode = CanvasMode::Program;
        state.ui.playhead = 260;
        let nudges: Vec<&str> = DEFAULTS
            .iter()
            .map(|d| d.id)
            .filter(|id| id.starts_with("layer.nudge-"))
            .collect();
        assert_eq!(nudges.len(), 8);
        for id in &nudges {
            assert_eq!(
                action_for(id, &state),
                Some(Action::Unavailable(NO_EDIT_TARGET.into())),
                "{id}"
            );
        }
        let rows = palette(&resolve(""), &state);
        for row in rows.iter().filter(|r| nudges.contains(&r.id.as_str())) {
            assert!(!row.enabled, "{}", row.id);
            assert_eq!(
                row.reason.as_deref(),
                Some("Select a clip to edit its take layout")
            );
        }
        // A selection makes its take editable: the first selected clip's,
        // in timeline order.
        state.ui.selection = ids(&["c003"]);
        assert_eq!(
            command_for("layer.nudge-down", &state),
            Some(nudge(SceneTarget::Take(TakeId::numbered(2)), 0, 1))
        );
        state.ui.selection = ids(&["c003", "c001"]);
        assert_eq!(
            command_for("layer.nudge-up", &state),
            Some(nudge(SceneTarget::Take(TakeId::numbered(1)), 0, -1))
        );
        state.ui.selected_layer = Some(LayerId::numbered(7));
        assert_eq!(
            action_for("layer.nudge-up", &state),
            Some(Action::Unavailable("Layer L7 isn't in this scene".into()))
        );
        state.ui.selected_layer = Some(LayerId::numbered(1));
        state
            .project
            .takes
            .get_mut(&TakeId::numbered(1))
            .unwrap()
            .scene
            .layers[0]
            .locked = true;
        assert_eq!(
            action_for("layer.nudge-down", &state),
            Some(Action::Unavailable("Layer L1 is locked".into()))
        );
    }

    #[test]
    fn playback_keys_need_the_program_canvas() {
        // D-01: Space, J and L don't play in Live mode.
        let mut state = editing_state();
        assert_eq!(state.ui.canvas_mode, CanvasMode::Live);
        for id in ["play.toggle", "shuttle.reverse", "shuttle.forward"] {
            assert_eq!(
                action_for(id, &state),
                Some(Action::Unavailable(PLAY_IN_LIVE.into())),
                "{id}"
            );
        }
        assert_eq!(PLAY_IN_LIVE, "Switch to Program (Tab) to play the timeline");
        assert_eq!(
            command_for("shuttle.stop", &state),
            Some(Command::Shuttle {
                dir: ShuttleDir::Stop
            }),
            "stopping is harmless"
        );
        let rows = palette(&resolve(""), &state);
        let play = rows.iter().find(|r| r.id == "play.toggle").unwrap();
        assert!(!play.enabled);
        assert_eq!(play.reason.as_deref(), Some(PLAY_IN_LIVE));

        state.ui.canvas_mode = CanvasMode::Program;
        assert_eq!(command_for("play.toggle", &state), Some(Command::Play));
        assert_eq!(
            command_for("shuttle.reverse", &state),
            Some(Command::Shuttle {
                dir: ShuttleDir::Reverse
            })
        );
        assert_eq!(
            command_for("shuttle.forward", &state),
            Some(Command::Shuttle {
                dir: ShuttleDir::Forward
            })
        );
        // A refusing gate says why first, in either mode.
        state.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(3),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        for mode in [CanvasMode::Program, CanvasMode::Live] {
            state.ui.canvas_mode = mode;
            assert_eq!(
                action_for("shuttle.forward", &state),
                Some(Action::Unavailable("Not while recording".into())),
                "{mode:?}"
            );
        }
        let empty = testkit::state(ProjectBuilder::new().mic("A1", "mic").build());
        assert_eq!(
            action_for("play.toggle", &empty),
            Some(Action::Unavailable("The timeline is empty".into()))
        );
    }

    #[test]
    fn gate_reasons_follow_apply() {
        let mut state = editing_state();
        let split = Command::ClipSplit {
            clip: None,
            at: TimeArg::frames(10),
            timeline: true,
        };
        assert_eq!(gate_reason(&state, &split), None);
        assert_eq!(gate_reason(&state, &Command::Play), None);
        state.runtime.record = RecordPhase::Countdown {
            remaining: 3,
            mode: RecordMode::Append,
            timer: crate::ids::TimerId(1),
            start_req: None,
            wait: false,
        };
        assert_eq!(
            gate_reason(&state, &split).as_deref(),
            Some("Not during the countdown")
        );
        assert_eq!(
            gate_reason(&state, &Command::Select { op: SelectOp::Next }),
            None,
            "UI commands run while recording"
        );
        state.runtime.record = RecordPhase::Finalizing {
            take: TakeId::numbered(3),
            mode: RecordMode::Append,
            cancelled: false,
            stop_reqs: vec![],
            waiters: vec![],
            discard: None,
        };
        assert_eq!(
            gate_reason(&state, &split).as_deref(),
            Some("Not while take 3 is finalizing")
        );
        assert_eq!(gate_reason(&state, &Command::TakeDiscardLast), None);
        state.runtime.record = RecordPhase::Idle;
        state.runtime.host = HostKind::Cli;
        assert_eq!(
            gate_reason(&state, &Command::Pause).as_deref(),
            Some("Needs the Qrec window")
        );
        state.runtime.host = HostKind::Gui;
        state.runtime.disk_invalid = Some("bad".into());
        assert_eq!(
            gate_reason(&state, &split).as_deref(),
            Some("qrec.json on disk is invalid")
        );
        assert_eq!(gate_reason(&state, &Command::Pause), None);
    }

    #[test]
    fn cap_legends_name_each_key() {
        let caps = |s: &str| chord(s).cap_legends();
        assert_eq!(caps("r"), ["R"], "letters are uppercase");
        assert_eq!(caps("ctrl+shift+z"), ["Ctrl", "Shift", "Z"]);
        assert_eq!(
            caps("ctrl+alt+shift+super+x"),
            ["Ctrl", "Alt", "Shift", "Super", "X"],
            "modifiers first, in a fixed order"
        );
        assert_eq!(caps("minus"), ["\u{2212}"], "the true minus");
        assert_eq!(caps("equal"), ["="]);
        assert_eq!(caps("backslash"), ["\\"]);
        assert_eq!(caps("less"), ["<"]);
        assert_eq!(caps("alt+bracketleft"), ["Alt", "["]);
        assert_eq!(caps("alt+braceright"), ["Alt", "}"]);
        assert_eq!(caps("ctrl+comma"), ["Ctrl", ","]);
        assert_eq!(caps("question"), ["?"]);
        assert_eq!(caps("1"), ["1"]);
        for (key, shown) in [
            ("Escape", "Esc"),
            ("Return", "Enter"),
            ("space", "Space"),
            ("Tab", "Tab"),
            ("Delete", "Del"),
            ("BackSpace", "Backspace"),
            ("Home", "Home"),
            ("End", "End"),
            ("Page_Up", "PgUp"),
            ("Page_Down", "PgDn"),
            ("Left", "←"),
            ("Right", "→"),
            ("Up", "↑"),
            ("Down", "↓"),
            ("F6", "F6"),
        ] {
            assert_eq!(caps(key), [shown], "{key}");
        }
        assert_eq!(caps("shift+F6"), ["Shift", "F6"]);
        assert_eq!(caps("shift+Left"), ["Shift", "←"]);
    }

    #[test]
    fn snapshot_cap_legends() {
        // Every default chord, as the GUI's key caps show it (G§4.2).
        let map = resolve("");
        let mut text = String::new();
        for b in &map.bindings {
            let chords: Vec<String> = b
                .keys
                .iter()
                .map(|k| {
                    let caps = k.cap_legends();
                    let key = caps.last().expect("a chord always has its key");
                    assert!(
                        key.chars().count() == 1
                            || key.starts_with(|c: char| c.is_ascii_uppercase()),
                        "{}: {key:?} looks like a raw key name",
                        b.id
                    );
                    caps.iter().map(|c| format!("[{c}]")).collect()
                })
                .collect();
            text.push_str(&format!("{:<22} {}\n", b.id, chords.join("  ")));
        }
        insta::assert_snapshot!("cap_legends", text);
    }

    #[test]
    fn window_keys() {
        // D-10: region focus cycling and quitting are rebindable bindings
        // that the GUI performs itself.
        let map = resolve("");
        let keys = |id: &str| -> Vec<String> {
            map.binding(id)
                .unwrap()
                .keys
                .iter()
                .map(KeyChord::to_string)
                .collect()
        };
        assert_eq!(keys("region.next"), ["F6"]);
        assert_eq!(keys("region.prev"), ["shift+F6"]);
        assert_eq!(keys("app.quit"), ["ctrl+q"]);
        let state = editing_state();
        for id in ["region.next", "region.prev", "app.quit"] {
            let b = map.binding(id).unwrap();
            assert_eq!(action_for(id, &state), Some(Action::Gui), "{id}");
            assert_eq!(b.group, "View");
            assert_eq!(b.cli, None);
            assert!(b.active_in(KeyContext::Recording), "{id}");
        }
        assert_eq!(
            lookup(&map, &chord("ctrl+q"), KeyContext::Overlay)
                .unwrap()
                .id,
            "app.quit",
            "quitting works over an overlay"
        );
        assert_eq!(lookup(&map, &chord("F6"), KeyContext::Overlay), None);
        let map = resolve("[bindings]\nregion.next = \"ctrl+F6\"\n");
        assert!(map.errors.is_empty(), "{:?}", map.errors);
        assert_eq!(
            lookup(&map, &chord("ctrl+F6"), KeyContext::Timeline)
                .unwrap()
                .id,
            "region.next"
        );
    }

    #[test]
    fn hint_context_from_state() {
        let mut state = editing_state();
        assert_eq!(hint_context(&state, false), HintContext::Idle);
        assert_eq!(hint_context(&state, true), HintContext::Dragging);
        state.ui.selection = ids(&["c001"]);
        assert_eq!(hint_context(&state, false), HintContext::ClipSelected);
        state.runtime.record = RecordPhase::Countdown {
            remaining: 3,
            mode: RecordMode::Append,
            timer: crate::ids::TimerId(1),
            start_req: None,
            wait: false,
        };
        assert_eq!(hint_context(&state, true), HintContext::Countdown);
    }

    #[test]
    fn palette_rows() {
        let mut state = editing_state();
        state.ui.selection = ids(&["c001", "c003"]);
        let map = resolve("");
        let rows = palette(&map, &state);
        assert_eq!(rows.len(), map.bindings.len());
        let row = |id: &str| rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(
            row("clip.split").line(),
            "Split clip at the playhead — s — qrec clip split <clip> --at <t>"
        );
        assert_eq!(row("snapping.toggle").line(), "Toggle snapping — n — —");
        let over = row("record.over");
        assert!(!over.enabled);
        assert_eq!(over.reason.as_deref(), Some("Needs a contiguous selection"));
        assert!(
            row("palette.open").enabled,
            "GUI actions are always enabled"
        );
        assert_eq!(row("palette.open").keys, ["Ctrl+K", ":"]);
    }

    #[test]
    fn keys_view_json_shape() {
        // A§9.7: `[{"id","group","label","keys":[..],"cli","contexts"}]`.
        let view = keys_view(&resolve(""));
        let json = serde_json::to_value(&view).unwrap();
        let rows = json.as_array().unwrap();
        assert_eq!(rows.len(), DEFAULTS.len());
        let redo = rows.iter().find(|b| b["id"] == "history.redo").unwrap();
        assert_eq!(
            redo,
            &serde_json::json!({
                "id": "history.redo",
                "group": "Edit",
                "label": "Redo",
                "keys": ["ctrl+shift+z", "ctrl+y"],
                "cli": "qrec redo",
                "contexts": ["global"]
            })
        );
        let zoom = rows.iter().find(|b| b["id"] == "zoom.in").unwrap();
        assert_eq!(zoom["cli"], serde_json::Value::Null);
        assert_eq!(
            serde_json::from_value::<Vec<BindingView>>(json).unwrap(),
            view
        );
        // Rebound and unbound keys show as they are.
        let view = keys_view(&resolve(
            "[bindings]\nrecord.over = []\nframe.save = \"F5\"\n",
        ));
        let row = |id: &str| view.iter().find(|b| b.id == id).unwrap().keys.clone();
        assert!(row("record.over").is_empty());
        assert_eq!(row("frame.save"), ["F5"]);
    }

    #[test]
    fn problems_name_conflicts_then_errors() {
        assert!(problems(&resolve("")).is_empty());
        let map =
            resolve("[bindings]\nautocut.run = \"x\"\nzoom.selection = \"x\"\nnope = \"q\"\n");
        assert_eq!(
            problems(&map),
            [
                "x is bound to clip.remove, autocut.run and zoom.selection in global; neither runs until keys.toml changes one",
                "keys.toml: unknown binding `nope` (`qrec keys` lists the ids)",
            ]
        );
        assert_eq!(join_and(&["a".into(), "b".into()]), "a and b");
        assert_eq!(join_and(&[]), "");
    }

    /// D-19: the palette's "Clear undo history" is an id `action_for`
    /// knows (no key, no `keys.toml` binding); the shared gates apply.
    #[test]
    fn history_clear_is_an_action_without_a_key() {
        let mut state = testkit::design_state();
        state.history.entries = std::sync::Arc::new(
            (1..=3)
                .map(|seq| crate::history::HistoryEntry {
                    seq,
                    ts: state.project.created_at,
                    label: format!("Edit {seq}"),
                    cmd: serde_json::Value::Null,
                    forward: json_patch::Patch(vec![]),
                    inverse: json_patch::Patch(vec![]),
                    after_hash: String::new(),
                })
                .collect(),
        );
        state.history.cursor = 3;
        assert_eq!(
            action_for("history.clear", &state),
            Some(Action::Run(Command::HistoryClear))
        );
        let map = resolve("");
        assert!(map.binding("history.clear").is_none(), "not a binding");
        assert!(
            resolve("[bindings]\n\"history.clear\" = \"ctrl+alt+h\"\n").errors[0]
                .contains("unknown binding `history.clear`")
        );
        assert_eq!(
            KEYLESS.iter().map(|a| a.id).collect::<Vec<_>>(),
            ["history.clear"]
        );
        assert!(map.binding(KEYLESS[0].after).is_some());
        state.runtime.disk_invalid = Some("bad".into());
        assert_eq!(
            action_for("history.clear", &state),
            Some(Action::Unavailable("qrec.json on disk is invalid".into()))
        );
        state.runtime.disk_invalid = None;
        state.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        assert_eq!(
            action_for("history.clear", &state),
            Some(Action::Unavailable("Not while recording".into()))
        );
        let empty = testkit::state(ProjectBuilder::new().build());
        assert!(empty.history.entries.is_empty());
        assert_eq!(
            action_for("history.clear", &empty),
            Some(Action::Unavailable("The undo history is empty".into()))
        );
    }

    /// Q5: a key two bindings share runs neither, in any context, and
    /// neither shows it; the banner's data names it once.
    #[test]
    fn a_conflict_leaves_both_bindings_inert() {
        let map = resolve("[bindings]\nautocut.run = \"x\"\n");
        for ctx in [KeyContext::Timeline, KeyContext::Canvas, KeyContext::Global] {
            assert_eq!(lookup(&map, &chord("x"), ctx), None, "{ctx:?}");
        }
        let remove = map.binding("clip.remove").unwrap();
        let autocut = map.binding("autocut.run").unwrap();
        assert_eq!(
            usable_keys(&map, remove),
            [&chord("Delete"), &chord("BackSpace")]
        );
        assert!(usable_keys(&map, autocut).is_empty());
        assert_eq!(conflicting_keys(&map, "autocut.run"), [&chord("x")]);
        assert!(conflicting_keys(&map, "clip.split").is_empty());
        assert!(is_rebound(autocut));
        assert!(!is_rebound(remove));
        assert_eq!(
            conflict_notes(&map),
            [ConflictNote {
                chord: chord("x"),
                key: "X".into(),
                ids: vec!["autocut.run".into(), "clip.remove".into()],
                labels: vec![
                    "Autocut the selection, or every clip".into(),
                    "Remove selected clips (to the bin)".into()
                ],
            }],
            "the rebound binding first"
        );
        // A clash in two contexts (global and recording) is one conflict.
        let map = resolve("[bindings]\nzoom.selection = \"r\"\n");
        assert_eq!(map.conflicts.len(), 2);
        let notes = conflict_notes(&map);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].ids, ["zoom.selection", "record.toggle"]);
        // Three on S in `global`, two of them in `recording` too: one.
        let map = resolve("[bindings]\nbin.toggle = \"s\"\nzoom.fit = \"s\"\n");
        assert_eq!(map.conflicts.len(), 2, "{:?}", map.conflicts);
        let notes = conflict_notes(&map);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].ids, ["zoom.fit", "bin.toggle", "clip.split"]);
        assert_eq!(key_name(&chord("ctrl+shift+z")), "Ctrl+Shift+Z");
        assert_eq!(key_name(&chord("alt+bracketleft")), "Alt+[");
        assert!(conflict_notes(&resolve("")).is_empty());
    }

    /// A `keys.toml` people wrote: comments everywhere, dotted keys out of
    /// order, a quoted key, a multi-line list, a trailing comment.
    const COMMENTED_KEYS: &str = "\
# My Qrec keys

[bindings]
# Output
frame.save = \"f\"   # closer to my left hand
clip.split = \"s\"
# my editing keys
export = \"ctrl+e\"
clip.trim-start = \"q\"  # trailing
\"zoom.fit\" = [
  \"backslash\",
  \"F4\",
]

# the end
";

    fn keys_of(text: &str, id: &str) -> Vec<KeyChord> {
        let map = resolve(text);
        assert!(map.errors.is_empty(), "{:?}\n{text}", map.errors);
        map.binding(id).unwrap().keys.clone()
    }

    #[test]
    fn set_binding_changes_one_value_in_place() {
        let edited = set_binding(COMMENTED_KEYS, "frame.save", &[chord("g")]).unwrap();
        assert_eq!(
            edited,
            COMMENTED_KEYS.replace("frame.save = \"f\"", "frame.save = \"g\""),
            "only the value changed"
        );
        assert_eq!(keys_of(&edited, "frame.save"), [chord("g")]);
        // A quoted key, a list, then unbound.
        let edited = set_binding(COMMENTED_KEYS, "zoom.fit", &[chord("F4")]).unwrap();
        assert_eq!(
            edited,
            COMMENTED_KEYS.replace("[\n  \"backslash\",\n  \"F4\",\n]", "\"F4\"")
        );
        let edited =
            set_binding(COMMENTED_KEYS, "clip.split", &[chord("s"), chord("ctrl+s")]).unwrap();
        assert!(
            edited.contains("clip.split = [\"s\", \"ctrl+s\"]\n"),
            "{edited}"
        );
        assert_eq!(
            keys_of(&edited, "clip.split"),
            [chord("s"), chord("ctrl+s")]
        );
        let edited = set_binding(COMMENTED_KEYS, "clip.trim-start", &[]).unwrap();
        assert!(
            edited.contains("clip.trim-start = []  # trailing\n"),
            "{edited}"
        );
        assert!(keys_of(&edited, "clip.trim-start").is_empty());
        // The same chord twice is one.
        let edited = set_binding("", "export", &[chord("F5"), chord("F5")]).unwrap();
        assert_eq!(edited, "[bindings]\nexport = \"F5\"\n");
    }

    #[test]
    fn set_binding_adds_a_line_after_the_bindings() {
        let edited = set_binding(COMMENTED_KEYS, "clip.trim-end", &[chord("w")]).unwrap();
        assert_eq!(
            edited,
            COMMENTED_KEYS.replace("  \"F4\",\n]\n", "  \"F4\",\n]\nclip.trim-end = \"w\"\n"),
            "after the last binding, everything else as it was"
        );
        assert_eq!(keys_of(&edited, "clip.trim-end"), [chord("w")]);
        assert_eq!(keys_of(&edited, "clip.split"), [chord("s")]);
        // No file yet, no [bindings] yet, a [bindings.clip] header.
        assert_eq!(
            set_binding("", "frame.save", &[chord("f")]).unwrap(),
            "[bindings]\nframe.save = \"f\"\n"
        );
        assert_eq!(
            set_binding("# nothing yet\n", "cancel", &[chord("q")]).unwrap(),
            "# nothing yet\n\n[bindings]\ncancel = \"q\"\n"
        );
        let text = "[bindings.clip]\nsplit = \"s\"\n";
        let edited = set_binding(text, "clip.remove", &[chord("x")]).unwrap();
        assert_eq!(edited, "[bindings.clip]\nsplit = \"s\"\nremove = \"x\"\n");
        let edited = set_binding(text, "frame.save", &[chord("f")]).unwrap();
        assert_eq!(keys_of(&edited, "frame.save"), [chord("f")]);
        assert_eq!(keys_of(&edited, "clip.split"), [chord("s")]);
    }

    #[test]
    fn set_binding_keeps_one_entry_of_a_binding_set_twice() {
        let text = "[bindings]\n\"clip.split\" = \"q\" # first\nother = 1\nclip.split = \"w\"\n";
        // (`other` is an unknown id: left as it is.)
        let edited = set_binding(text, "clip.split", &[chord("e")]).unwrap();
        assert_eq!(
            edited,
            "[bindings]\n\"clip.split\" = \"e\" # first\nother = 1\n"
        );
        let map = resolve(&edited);
        assert_eq!(map.binding("clip.split").unwrap().keys, [chord("e")]);
        assert!(!map.errors.iter().any(|e| e.contains("set twice")));
    }

    #[test]
    fn reset_binding_removes_its_lines_only() {
        let edited = reset_binding(COMMENTED_KEYS, "export").unwrap();
        assert_eq!(
            edited,
            COMMENTED_KEYS.replace("export = \"ctrl+e\"\n", ""),
            "the comment above it stays"
        );
        assert_eq!(keys_of(&edited, "export"), [chord("ctrl+e")]);
        let edited = reset_binding(COMMENTED_KEYS, "zoom.fit").unwrap();
        assert_eq!(
            edited,
            COMMENTED_KEYS.replace("\"zoom.fit\" = [\n  \"backslash\",\n  \"F4\",\n]\n", "")
        );
        assert_eq!(keys_of(&edited, "zoom.fit"), [chord("backslash")]);
        // Not set: nothing changes.
        assert_eq!(
            reset_binding(COMMENTED_KEYS, "record.toggle").unwrap(),
            COMMENTED_KEYS
        );
        assert_eq!(reset_binding("", "record.toggle").unwrap(), "");
    }

    /// The order a person wrote stays: a `toml_edit` document would print
    /// `clip.split` next to `clip.trim-start`, and the other way round.
    #[test]
    fn edits_keep_dotted_keys_where_they_were() {
        let text = "[bindings]\nclip.split = \"s\"\nframe.save = \"f\"\nclip.remove = \"x\"\n";
        let edited = set_binding(text, "frame.save", &[chord("g")]).unwrap();
        assert_eq!(
            edited,
            "[bindings]\nclip.split = \"s\"\nframe.save = \"g\"\nclip.remove = \"x\"\n"
        );
        let edited = set_binding(text, "export", &[chord("F5")]).unwrap();
        assert_eq!(edited, format!("{text}export = \"F5\"\n"));
        let edited = reset_binding(text, "clip.split").unwrap();
        assert_eq!(
            edited,
            "[bindings]\nframe.save = \"f\"\nclip.remove = \"x\"\n"
        );
    }

    #[test]
    fn edits_refuse_a_broken_file_and_unknown_ids() {
        let err = set_binding("[bindings\nframe.save = \"f\"\n", "frame.save", &[]).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Invalid);
        assert!(
            err.message.starts_with("keys.toml line 1:"),
            "{}",
            err.message
        );
        let err = reset_binding("[bindings\n", "frame.save").unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Invalid);
        let err = set_binding("", "nope", &[chord("q")]).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Usage);
        assert_eq!(err.message, "unknown binding `nope`");
        let err = reset_binding("", "history.clear").unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Usage);
    }

    /// An inline `bindings` table can't take a line as text: the document
    /// edit does it, keeping the comments.
    #[test]
    fn an_inline_bindings_table_is_edited_as_a_document() {
        let text = "# keys\nbindings = { \"frame.save\" = \"f\" }\n";
        let edited = set_binding(text, "frame.save", &[chord("g")]).unwrap();
        assert_eq!(edited, "# keys\nbindings = { \"frame.save\" = \"g\" }\n");
        let edited = set_binding(text, "export", &[chord("F5")]).unwrap();
        assert!(edited.starts_with("# keys\n"), "{edited}");
        assert_eq!(keys_of(&edited, "export"), [chord("F5")]);
        assert_eq!(keys_of(&edited, "frame.save"), [chord("f")]);
        let edited = reset_binding(text, "frame.save").unwrap();
        assert!(edited.starts_with("# keys\n"), "{edited}");
        assert_eq!(keys_of(&edited, "frame.save"), [chord("p")]);
    }

    proptest::proptest! {
        /// Any run of sets and resets over a commented file: every comment
        /// stays, the lines nobody edited keep their order, and the file
        /// reads back with each binding's keys as last set (or its
        /// defaults after a reset).
        #[test]
        fn binding_edits_keep_the_file_and_read_back(
            edits in proptest::collection::vec(
                (0usize..DEFAULTS.len(), proptest::option::of(proptest::collection::vec(0usize..6, 0..3))),
                1..12,
            )
        ) {
            const CHORDS: [&str; 6] = ["f", "ctrl+shift+f", "F7", "less", "alt+bracketleft", "space"];
            let mut text = COMMENTED_KEYS.to_owned();
            let mut expected: BTreeMap<&str, Vec<KeyChord>> = BTreeMap::new();
            for (which, keys) in &edits {
                let id = DEFAULTS[*which].id;
                match keys {
                    Some(keys) => {
                        let mut chords: Vec<KeyChord> = Vec::new();
                        for k in keys {
                            let c = chord(CHORDS[*k]);
                            if !chords.contains(&c) {
                                chords.push(c);
                            }
                        }
                        text = set_binding(&text, id, &chords).unwrap();
                        expected.insert(id, chords);
                    }
                    None => {
                        text = reset_binding(&text, id).unwrap();
                        expected.insert(id, default_keys(id).unwrap());
                    }
                }
            }
            let map = resolve(&text);
            proptest::prop_assert!(map.errors.is_empty(), "{:?}\n{}", map.errors, text);
            for (id, keys) in &expected {
                proptest::prop_assert_eq!(&map.binding(id).unwrap().keys, keys, "{}\n{}", id, text);
            }
            let comments: Vec<&str> = COMMENTED_KEYS.lines().filter(|l| l.trim_start().starts_with('#')).collect();
            let kept: Vec<&str> = text.lines().filter(|l| l.trim_start().starts_with('#')).collect();
            proptest::prop_assert_eq!(kept, comments, "{}", text);
        }
    }

    #[test]
    fn snapshot_keymap_table() {
        insta::assert_snapshot!("keymap_table", cheat_sheet(&resolve("")));
    }

    #[test]
    fn snapshot_keymap_json() {
        insta::assert_json_snapshot!("keymap_json", keys_view(&resolve("")));
    }

    #[test]
    fn snapshot_hints() {
        let map = resolve("");
        let text: String = HintContext::ALL
            .iter()
            .map(|ctx| format!("{ctx:?}: {}\n", format_hints(&hints(*ctx, &map))))
            .collect();
        insta::assert_snapshot!("hints", text);
    }

    #[test]
    fn snapshot_cheat_sheet_with_problems() {
        let map = resolve("[bindings]\nautocut.run = \"x\"\nframe.save = \"f\"\nnope = \"q\"\n");
        insta::assert_snapshot!("cheat_sheet_with_problems", cheat_sheet(&map));
    }

    #[test]
    fn total_frames_helper_is_consistent() {
        let state = editing_state();
        assert_eq!(total_frames(&state.project.timeline.clips), 450);
    }
}
