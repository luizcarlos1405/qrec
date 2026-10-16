//! The key router's decision (G§3.8, A§9.7), GTK-free: [`decide`] takes
//! one key press and what the window knows and says what happens; [`apply`]
//! carries a decision out through a [`KeySink`] (the GTK glue in
//! `keys/mod.rs`, a recorder in tests).
//!
//! For each press, first match:
//!
//! 1. A key Qrec can't bind (`KeyChord::from_gdk` gave `None`) propagates.
//! 2. Focus in an editable propagates: the field handles `Enter`, `Esc`
//!    and `Tab`.
//! 3. During a drag (D-42): `Esc` cancels it; `n` toggles snapping during a
//!    trim; the nudge keys go on with a nudge burst (D-33); modifiers reach
//!    the gesture (they make no chord); anything else is swallowed.
//! 4. A top overlay that keeps the keyboard (all but the running export
//!    panel, D-49): `Tab`, `Enter`, `Space` and the arrows go to its
//!    widgets; in context `overlay`, `cancel` closes it and `app.quit`
//!    quits; nothing else.
//! 5. `Esc` (the `cancel` keys), first that applies: close the passive
//!    export panel → close the autocut page → end a bin preview →
//!    `cancel` (`RecordCancel`, clear the clip selection, the layer
//!    selection). "Nothing to cancel" is silent.
//! 6. Focus in the bin (D-11, D-37): `↑` `↓` (± `Shift`), `Enter` and
//!    `Space` are the bin list's own keys.
//! 7. A focused activatable widget or list (with visible focus) keeps the
//!    navigation keys.
//! 8. The context: `recording` unless Idle; `canvas` with the stage canvas
//!    focused, something to edit (`edit_target`) and a layer selected;
//!    otherwise `timeline` (where `global` bindings are active too).
//! 9. `keymap::lookup`; nothing bound propagates.
//! 10. GUI overrides: `export` opens the export panel, `autocut.preview`
//!     the autocut page; a `layer.nudge-*` that can run goes to the stage's
//!     nudge burst (D-33: one `LayerSet` per burst, not per press);
//!     `zoom.fit` goes to the timeline, which fits its own view (a local
//!     zoom waiting for its `SetUi` would win over the core's `SetUi{zoom:
//!     Clear}`, which changes nothing a snapshot shows from fit).
//! 11. `keymap::action_for` on the state with the player's frame as the
//!     playhead while playing: a command (after its pre-steps), a GUI
//!     action, or why it can't run (D-34).
//!
//! Before the Session is open (startup, blocking pages) only `app.quit`
//! runs; other keys go to the page's buttons.

use std::borrow::Cow;

use qrec_core::keymap::{Action, KeyChord, KeyContext, Keymap, action_for, lookup, usable_keys};
use qrec_core::protocol::command::{SeekTarget, ShuttleDir};
use qrec_core::state::CanvasMode;
use qrec_core::ui_normalize::edit_target;
use qrec_core::view::notices::{Notice, unavailable_notice};
use qrec_core::view::{DragKind, FocusRegion, PanelPageKind};
use qrec_core::{Command, Frame, State, TimeArg};

use crate::store::local::{ExportPanelMode, LocalUi, Overlay};

/// One key press as the router sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInput {
    /// `KeyChord::from_gdk` of the press; `None` for keys Qrec can't bind
    /// (a modifier alone, a dead key).
    pub chord: Option<KeyChord>,
    /// The focus is in an editable: a text or numeric field, a label being
    /// edited.
    pub editable: bool,
    /// The focus is visibly on an activatable widget or a list: a button,
    /// toggle, check, switch, scale, dropdown or list row.
    pub activatable: bool,
}

impl KeyInput {
    /// A press of `chord` with the focus on nothing special.
    pub fn chord(chord: KeyChord) -> Self {
        Self {
            chord: Some(chord),
            editable: false,
            activatable: false,
        }
    }
}

/// What the router knows besides the key.
#[derive(Debug, Clone, Copy)]
pub struct RouterState<'a> {
    /// The newest snapshot.
    pub state: &'a State,
    pub local: &'a LocalUi,
    pub keymap: &'a Keymap,
    /// The player's frame while it plays; `None` otherwise.
    pub player_frame: Option<Frame>,
}

/// What a key press does.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyDecision {
    /// GTK handles it: a field, an overlay's widgets, the bin list, a
    /// focused button, or a key nothing is bound to.
    Propagate,
    /// Eaten; nothing happens.
    Swallow,
    /// `Esc` during a drag: cancel it (D-42).
    CancelDrag,
    /// Close the top overlay (`cancel` in context `overlay`).
    CloseOverlay,
    /// Binding `id`'s command, after the pre-steps.
    Run {
        id: String,
        pre: Vec<PreStep>,
        cmd: Command,
    },
    /// GUI action `id` (`ActionRouter::run`).
    Gui { id: String },
    /// Binding `id` can't run now, for `reason` (D-34).
    Unavailable { id: String, reason: String },
}

/// What a command needs first (G§3.8).
#[derive(Debug, Clone, PartialEq)]
pub enum PreStep {
    /// Seek to the player's frame (a playhead-dependent command while
    /// playing).
    Seek(Frame),
    /// Switch the canvas to Live (a recording started in Program, D-05).
    SetLive,
    /// Set `LocalUi.playing_rate` (a shuttle, D-44).
    SetRate(f64),
    /// Play at this rate now: the player already plays, and a `Shuttle`
    /// while playing changes no state it could follow (D-44).
    PlayerRate(f64),
}

/// The bindings whose command reads the playhead (G§3.8 pre-steps).
const PLAYHEAD_BINDINGS: [&str; 8] = [
    "clip.split",
    "clip.trim-start",
    "clip.trim-end",
    "clip.nudge-start",
    "clip.nudge-end",
    "clip.nudge-start-10",
    "clip.nudge-end-10",
    "frame.save",
];

/// The layer nudges (D-33): the stage coalesces a burst of them into one
/// `LayerSet` (its GUI actions of the same ids).
pub const NUDGE_BINDINGS: [&str; 8] = [
    "layer.nudge-left",
    "layer.nudge-right",
    "layer.nudge-up",
    "layer.nudge-down",
    "layer.nudge-left-10",
    "layer.nudge-right-10",
    "layer.nudge-up-10",
    "layer.nudge-down-10",
];

/// The router's decision for one press (the steps in the module docs).
pub fn decide(input: &KeyInput, rs: &RouterState<'_>) -> KeyDecision {
    // 1.
    let Some(chord) = &input.chord else {
        return KeyDecision::Propagate;
    };
    // 2.
    if input.editable {
        return KeyDecision::Propagate;
    }
    let local = rs.local;
    if !local.is_open() {
        return match lookup(rs.keymap, chord, KeyContext::Overlay) {
            Some(b) if b.id == "app.quit" => gui("app.quit"),
            _ => KeyDecision::Propagate,
        };
    }
    // 3.
    if let Some(drag) = local.drag {
        if is_key_of(rs.keymap, "cancel", chord) {
            return KeyDecision::CancelDrag;
        }
        let trimming = matches!(drag, DragKind::TrimStart | DragKind::TrimEnd);
        if trimming && is_key_of(rs.keymap, "snapping.toggle", chord) {
            return decide_binding("snapping.toggle", rs);
        }
        // D-33: a nudge burst goes on with the nudge keys.
        if drag == DragKind::LayerNudge
            && let Some(id) = NUDGE_BINDINGS
                .iter()
                .find(|id| is_key_of(rs.keymap, id, chord))
        {
            return decide_binding(id, rs);
        }
        return KeyDecision::Swallow;
    }
    // 4.
    if let Some(top) = local.top_overlay().filter(|o| o.traps_focus()) {
        if is_overlay_widget_key(chord) {
            return KeyDecision::Propagate;
        }
        return match lookup(rs.keymap, chord, KeyContext::Overlay) {
            // A popover closes itself on Esc (and says so with `closed`).
            Some(b) if b.id == "cancel" && *top == Overlay::Menu => KeyDecision::Propagate,
            Some(b) if b.id == "cancel" => KeyDecision::CloseOverlay,
            Some(b) if b.id == "app.quit" => gui("app.quit"),
            _ => KeyDecision::Swallow,
        };
    }
    // 5.
    if is_key_of(rs.keymap, "cancel", chord) {
        if local.export_panel() == Some(ExportPanelMode::Running) {
            return gui("export.panel");
        }
        if local.panel_page == PanelPageKind::Autocut {
            return gui("autocut.page");
        }
        if local.take_preview.is_some() {
            return gui("bin.preview");
        }
        return decide_binding("cancel", rs);
    }
    // 6.
    if local.focus == FocusRegion::Bin && is_bin_key(chord) {
        return KeyDecision::Propagate;
    }
    // 7.
    if input.activatable && is_navigation_key(chord) {
        return KeyDecision::Propagate;
    }
    // 8, 9.
    let Some(binding) = lookup(rs.keymap, chord, context(rs)) else {
        return KeyDecision::Propagate;
    };
    // 10, 11.
    decide_binding(binding.id, rs)
}

/// Steps 10 and 11 for binding `id`: what a button with that binding does
/// (`KeyRouter::run_binding`, G§3.7).
pub fn decide_binding(id: &str, rs: &RouterState<'_>) -> KeyDecision {
    match id {
        "export" => return gui("export.panel"),
        "autocut.preview" => return gui("autocut.page"),
        _ => {}
    }
    let state = effective_state(rs);
    match action_for(id, &state) {
        // D-33: the stage's nudge burst sends one `LayerSet` for all of it.
        Some(Action::Run(_)) if NUDGE_BINDINGS.contains(&id) || id == "zoom.fit" => gui(id),
        Some(Action::Run(cmd)) => KeyDecision::Run {
            id: id.to_owned(),
            pre: pre_steps(id, rs),
            cmd,
        },
        Some(Action::Gui) => gui(id),
        Some(Action::Unavailable(reason)) => KeyDecision::Unavailable {
            id: id.to_owned(),
            reason,
        },
        None => {
            tracing::debug!("keys: no binding {id}");
            KeyDecision::Propagate
        }
    }
}

/// Step 8: the context the press is looked up in.
pub fn context(rs: &RouterState<'_>) -> KeyContext {
    let state = rs.state;
    if !state.runtime.record.is_idle() {
        return KeyContext::Recording;
    }
    let canvas = rs.local.focus == FocusRegion::Stage
        && state.ui.selected_layer.is_some()
        && edit_target(&state.project, &state.ui).is_some();
    if canvas {
        KeyContext::Canvas
    } else {
        KeyContext::Timeline
    }
}

/// The snapshot with the player's frame as the playhead while it plays
/// (step 11).
pub fn effective_state<'a>(rs: &RouterState<'a>) -> Cow<'a, State> {
    match rs.player_frame {
        Some(frame) if frame != rs.state.ui.playhead => {
            let mut state = rs.state.clone();
            state.ui.playhead = frame;
            Cow::Owned(state)
        }
        _ => Cow::Borrowed(rs.state),
    }
}

/// The pre-steps of binding `id` (G§3.8).
fn pre_steps(id: &str, rs: &RouterState<'_>) -> Vec<PreStep> {
    let state = rs.state;
    let mut pre = Vec::new();
    if let Some(frame) = rs.player_frame
        && PLAYHEAD_BINDINGS.contains(&id)
    {
        pre.push(PreStep::Seek(frame));
    }
    if matches!(id, "record.toggle" | "record.over")
        && state.runtime.record.is_idle()
        && state.ui.canvas_mode == CanvasMode::Program
    {
        pre.push(PreStep::SetLive);
    }
    let shuttle = match id {
        "shuttle.forward" => Some(ShuttleDir::Forward),
        "shuttle.reverse" => Some(ShuttleDir::Reverse),
        "shuttle.stop" => Some(ShuttleDir::Stop),
        _ => None,
    };
    if let Some(dir) = shuttle {
        let playing = state.runtime.playing;
        let rate = next_rate(rs.local.playing_rate, playing, dir);
        pre.push(PreStep::SetRate(rate));
        if playing && dir != ShuttleDir::Stop {
            pre.push(PreStep::PlayerRate(rate));
        }
    }
    pre
}

/// The shuttle rate after pressing `dir` (G§3.10 step 7): `L` 1 → 2 → 4,
/// `J` −1 → −2 → −4, `K` stops (the rate goes back to 1).
pub fn next_rate(current: f64, playing: bool, dir: ShuttleDir) -> f64 {
    match dir {
        ShuttleDir::Forward if playing && current >= 1.0 => (current * 2.0).min(4.0),
        ShuttleDir::Forward => 1.0,
        ShuttleDir::Reverse if playing && current <= -1.0 => (current * 2.0).max(-4.0),
        ShuttleDir::Reverse => -1.0,
        ShuttleDir::Stop => 1.0,
    }
}

/// The region focus cycling of `region.next` / `region.prev` (D-10, BRIEF
/// §6): header → transport → timeline toolbar → timeline → panel → bin
/// (when open), around. From a region outside the cycle (the stage canvas
/// sits between the header and the transport; fields and the rest count
/// as before the header) it goes to the nearest one in that direction.
pub fn next_region(current: FocusRegion, forward: bool, bin_open: bool) -> FocusRegion {
    use FocusRegion::*;
    let mut cycle = vec![Header, Transport, TimelineToolbar, Timeline, Panel];
    if bin_open {
        cycle.push(Bin);
    }
    let n = cycle.len();
    match cycle.iter().position(|r| *r == current) {
        Some(i) if forward => cycle[(i + 1) % n],
        Some(i) => cycle[(i + n - 1) % n],
        None if current == Stage => {
            if forward {
                Transport
            } else {
                Header
            }
        }
        None if forward => cycle[0],
        None => cycle[n - 1],
    }
}

/// The focus region a widget named `name` roots (G§3.8: `region-<name>`
/// and the named parts that take focus).
pub fn region_for_name(name: &str) -> Option<FocusRegion> {
    Some(match name {
        "region-header" => FocusRegion::Header,
        "region-stage" => FocusRegion::Stage,
        "transport" => FocusRegion::Transport,
        "timeline-toolbar" => FocusRegion::TimelineToolbar,
        "region-timeline" => FocusRegion::Timeline,
        "region-panel" => FocusRegion::Panel,
        "region-bin" => FocusRegion::Bin,
        _ => return None,
    })
}

/// What a decision needs from the window.
pub trait KeySink {
    fn dispatch(&mut self, cmd: Command);
    /// `ActionRouter::run(id)`.
    fn gui(&mut self, id: &str);
    fn notice(&mut self, notice: Notice);
    /// `LocalUi.playing_rate`.
    fn set_playing_rate(&mut self, rate: f64);
    /// `ProgramPlayer::play(rate)`.
    fn player_rate(&mut self, rate: f64);
    /// The drag owner's cancel.
    fn cancel_drag(&mut self);
    /// Pops the top overlay.
    fn close_overlay(&mut self);
}

/// Carries `decision` out; `true` when the key was used (stop
/// propagation).
pub fn apply(decision: &KeyDecision, state: &State, sink: &mut dyn KeySink) -> bool {
    match decision {
        KeyDecision::Propagate => false,
        KeyDecision::Swallow => true,
        KeyDecision::CancelDrag => {
            sink.cancel_drag();
            true
        }
        KeyDecision::CloseOverlay => {
            sink.close_overlay();
            true
        }
        KeyDecision::Run { pre, cmd, .. } => {
            for step in pre {
                match step {
                    PreStep::Seek(frame) => sink.dispatch(Command::Seek {
                        to: SeekTarget::To(TimeArg::frames(*frame)),
                    }),
                    PreStep::SetLive => sink.dispatch(Command::SetCanvasMode {
                        mode: CanvasMode::Live,
                    }),
                    PreStep::SetRate(rate) => sink.set_playing_rate(*rate),
                    PreStep::PlayerRate(rate) => sink.player_rate(*rate),
                }
            }
            sink.dispatch(cmd.clone());
            true
        }
        KeyDecision::Gui { id } => {
            sink.gui(id);
            true
        }
        KeyDecision::Unavailable { id, reason } => {
            if let Some(notice) = unavailable_notice(id, reason, state) {
                sink.notice(notice);
            }
            true
        }
    }
}

fn gui(id: &str) -> KeyDecision {
    KeyDecision::Gui { id: id.to_owned() }
}

/// `chord` is one of binding `id`'s keys that run.
fn is_key_of(keymap: &Keymap, id: &str, chord: &KeyChord) -> bool {
    keymap
        .binding(id)
        .is_some_and(|b| usable_keys(keymap, b).contains(&chord))
}

fn plain(chord: &KeyChord) -> bool {
    !chord.ctrl && !chord.alt && !chord.super_
}

/// `Tab`, `Enter`, `Space` and the arrows: an overlay's widgets use them.
fn is_overlay_widget_key(chord: &KeyChord) -> bool {
    matches!(
        chord.key.as_str(),
        "Tab" | "Return" | "space" | "Left" | "Right" | "Up" | "Down"
    )
}

/// The bin list's keys (D-11, D-37).
fn is_bin_key(chord: &KeyChord) -> bool {
    plain(chord)
        && match chord.key.as_str() {
            "Up" | "Down" => true,
            "Return" | "space" => !chord.shift,
            _ => false,
        }
}

/// What a focused button, scale, dropdown or list uses (step 7).
fn is_navigation_key(chord: &KeyChord) -> bool {
    plain(chord)
        && matches!(
            chord.key.as_str(),
            "space"
                | "Return"
                | "Left"
                | "Right"
                | "Up"
                | "Down"
                | "Home"
                | "End"
                | "Page_Up"
                | "Page_Down"
        )
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
