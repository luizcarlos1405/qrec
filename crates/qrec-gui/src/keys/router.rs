//! The key router's decision (G§3.8, A§9.7), GTK-free: [`decide`] takes
//! one key press and what the window knows and says what happens; [`apply`]
//! carries a decision out through a [`KeySink`] (the GTK glue in
//! `keys/mod.rs`, a recorder in tests).
//!
//! For each press, first match:
//!
//! 1. A key Qrec can't bind (`KeyChord::from_gdk` gave `None`) propagates.
//! 2. Focus in an editable propagates: the field handles `Enter`, `Esc`
//!    and `Tab`, and a held key's repeats (typing, `Backspace`).
//! 3. During a drag (D-42): `Esc` cancels it; `n` toggles snapping during a
//!    trim; the nudge keys go on with a nudge burst (D-33); modifiers reach
//!    the gesture (they make no chord); anything else is swallowed.
//! 4. A top overlay that keeps the keyboard (all but the running export
//!    panel, D-49): `Tab`, `Enter`, `Space` and the arrows go to its
//!    widgets; in context `overlay`, `cancel` closes it (on the autocut
//!    page in the Narrow layers sheet: the page's Cancel, as step 5 does
//!    docked) and `app.quit` quits; nothing else.
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
//!     playhead while the program plays: a command (after its pre-steps:
//!     first a seek to the player's frame, R18), a GUI action, or why it
//!     can't run (D-34). A step (frame, ±1 s, edit) while the program
//!     plays pauses it first and goes from the frame the player shows: a
//!     `Pause`, then one absolute seek to where the step lands (D-51).
//!
//! A held key repeats as presses (`KeyInput::repeat`, from [`HeldKeys`]):
//! outside an editable a repeat goes on only for what is meant to repeat,
//! the seek, select, move, nudge, zoom and undo bindings
//! ([`REPEATING_BINDINGS`]) and the navigation keys a focused widget, list
//! or overlay uses (not `Enter` or `Space`). Anything else happens once per
//! press (R30: holding `R` started a take and its repeats stopped it).
//!
//! Before the Session is open (startup, blocking pages) only `app.quit`
//! runs; other keys go to the page's buttons. Buttons with a binding
//! follow the same rule ([`decide_button`]).

use std::borrow::Cow;

use qrec_core::apply::ui::seek_frame;
use qrec_core::keymap::{Action, KeyChord, KeyContext, Keymap, action_for, lookup, usable_keys};
use qrec_core::protocol::command::{SeekTarget, ShuttleDir};
use qrec_core::state::CanvasMode;
use qrec_core::time::Sign;
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
    /// The key is held down: this press is its auto-repeat, not a new one
    /// (GTK has no repeat flag: [`HeldKeys`] tells).
    pub repeat: bool,
}

impl KeyInput {
    /// A press of `chord` with the focus on nothing special.
    pub fn chord(chord: KeyChord) -> Self {
        Self {
            chord: Some(chord),
            editable: false,
            activatable: false,
            repeat: false,
        }
    }
}

/// The keys held down (hardware keycodes, whose keyval can change mid-hold
/// with the modifiers): a press of one already down is its auto-repeat
/// (Wayland clients make them, as presses without a release between).
#[derive(Debug, Default)]
pub struct HeldKeys {
    down: std::collections::BTreeSet<u32>,
}

impl HeldKeys {
    /// A press of `code`: whether it is a repeat.
    pub fn press(&mut self, code: u32) -> bool {
        !self.down.insert(code)
    }

    pub fn release(&mut self, code: u32) {
        self.down.remove(&code);
    }

    /// The window lost the keyboard: its releases go elsewhere, so nothing
    /// counts as held any more.
    pub fn clear(&mut self) {
        self.down.clear();
    }
}

/// The bindings a held key repeats (R30): moving through the timeline, the
/// clips and the layers, zooming, undo and redo. A repeat of any other
/// binding is swallowed: toggles, record, cancel, export, frame save.
pub const REPEATING_BINDINGS: [&str; 28] = [
    "seek.prev-frame",
    "seek.next-frame",
    "seek.back",
    "seek.forward",
    "seek.prev-edit",
    "seek.next-edit",
    "seek.start",
    "seek.end",
    "select.prev",
    "select.next",
    "clip.move-left",
    "clip.move-right",
    "clip.nudge-start",
    "clip.nudge-end",
    "clip.nudge-start-10",
    "clip.nudge-end-10",
    "layer.nudge-left",
    "layer.nudge-right",
    "layer.nudge-up",
    "layer.nudge-down",
    "layer.nudge-left-10",
    "layer.nudge-right-10",
    "layer.nudge-up-10",
    "layer.nudge-down-10",
    "zoom.in",
    "zoom.out",
    "history.undo",
    "history.redo",
];

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
    /// Seek to the player's frame: the program plays and the core's
    /// playhead lags it (R18).
    Seek(Frame),
    /// Pause the playing program: a step goes from the frame the player
    /// shows (D-51). `park` is the frame the step's seek puts the playhead
    /// on: the sync leaves the playhead to it instead of parking it at the
    /// player's frame, and holds the paused player until it lands.
    Pause { park: Frame },
    /// Switch the canvas to Live (a recording started in Program, D-05).
    SetLive,
    /// Set `LocalUi.playing_rate` (a shuttle, D-44).
    SetRate(f64),
    /// Play at this rate now: the player already plays, and a `Shuttle`
    /// while playing changes no state it could follow (D-44).
    PlayerRate(f64),
}

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

/// The steps (D-51): while the program plays, each pauses it first and
/// goes from the frame the player shows (`Pause`, then one absolute seek
/// to where it lands), as an editor's frame step does. Home and End, a
/// click on the ruler and the CLI's seeks move a playing program on.
pub const STEP_BINDINGS: [&str; 6] = [
    "seek.prev-frame",
    "seek.next-frame",
    "seek.back",
    "seek.forward",
    "seek.prev-edit",
    "seek.next-edit",
];

/// The router's decision for one press (the steps in the module docs),
/// and for a repeat whether it goes on.
pub fn decide(input: &KeyInput, rs: &RouterState<'_>) -> KeyDecision {
    let decision = decide_press(input, rs);
    match &input.chord {
        Some(chord) if input.repeat && !input.editable => on_repeat(decision, chord),
        _ => decision,
    }
}

/// A held key's repeat of `decision` (R30): it goes on for the repeating
/// bindings and for the navigation keys widgets use; anything else is
/// swallowed (an unavailable binding said why on its press, a drag's
/// cancel and an overlay's close ran then).
fn on_repeat(decision: KeyDecision, chord: &KeyChord) -> KeyDecision {
    let goes_on = match &decision {
        KeyDecision::Propagate => !is_activation_key(chord),
        KeyDecision::Run { id, .. } | KeyDecision::Gui { id } => {
            REPEATING_BINDINGS.contains(&id.as_str())
        }
        _ => false,
    };
    if goes_on {
        decision
    } else {
        KeyDecision::Swallow
    }
}

/// `Enter` and `Space`: a focused button, toggle or row activates on them
/// (its repeats would toggle it again and again).
fn is_activation_key(chord: &KeyChord) -> bool {
    matches!(
        chord.key.as_str(),
        "space" | "KP_Space" | "Return" | "KP_Enter" | "ISO_Enter"
    )
}

/// One press's decision, repeat or not (steps 1 to 11).
fn decide_press(input: &KeyInput, rs: &RouterState<'_>) -> KeyDecision {
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
            // The autocut page in the Narrow sheet: its Cancel (R33).
            Some(b)
                if b.id == "cancel"
                    && *top == Overlay::LayersSheet
                    && local.panel_page == PanelPageKind::Autocut =>
            {
                gui("autocut.page")
            }
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
        Some(Action::Run(cmd)) => match step_while_playing(id, &cmd, rs, &state) {
            Some(step) => step,
            None => KeyDecision::Run {
                id: id.to_owned(),
                pre: pre_steps(id, &cmd, rs),
                cmd,
            },
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

/// What a button or menu item with binding `id` does
/// (`KeyRouter::run_binding`, G§3.7): what its key does, so before the
/// Session is open nothing but `app.quit` (R36: the header's gear opened
/// Settings over the opening window, where `Ctrl+,` did nothing, and it
/// stayed over the blocking page that followed).
pub fn decide_button(id: &str, rs: &RouterState<'_>) -> KeyDecision {
    if !rs.local.is_open() && id != "app.quit" {
        return KeyDecision::Swallow;
    }
    decide_binding(id, rs)
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

/// D-51: a step (`cmd`, binding `id`) while the program plays pauses it,
/// then seeks to where the step lands from the frame the player shows
/// (`effective`'s playhead): one absolute seek, so what the picture ends
/// on is that frame's neighbour, not the core's lagging playhead's.
/// `None` for anything else, or with the program not playing.
fn step_while_playing(
    id: &str,
    cmd: &Command,
    rs: &RouterState<'_>,
    effective: &State,
) -> Option<KeyDecision> {
    if !STEP_BINDINGS.contains(&id) || !rs.state.runtime.playing || rs.player_frame.is_none() {
        return None;
    }
    let Command::Seek { to } = cmd else {
        return None;
    };
    let park = seek_frame(effective, to).ok()?;
    Some(KeyDecision::Run {
        id: id.to_owned(),
        pre: vec![PreStep::Pause { park }],
        cmd: Command::Seek {
            to: SeekTarget::To(TimeArg::frames(park)),
        },
    })
}

/// The pre-steps of binding `id`, whose command is `cmd` (G§3.8).
fn pre_steps(id: &str, cmd: &Command, rs: &RouterState<'_>) -> Vec<PreStep> {
    let state = rs.state;
    let mut pre = Vec::new();
    // While the program plays the core's playhead only moves with the
    // sync's once-a-second seek: a command resolved from it (a split, a
    // frame step, ±1 s, an edit jump, a shuttle's start, a selection by
    // the playhead…) first puts it on the player's frame (R18). Not an
    // absolute seek, which doesn't read it.
    if let Some(frame) = rs.player_frame
        && frame != state.ui.playhead
        && !absolute_seek(cmd)
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

/// A seek that doesn't read the playhead: to a frame, the start, the end.
fn absolute_seek(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Seek {
            to: SeekTarget::To(TimeArg {
                sign: Sign::Absolute,
                ..
            }) | SeekTarget::Start
                | SeekTarget::End
        }
    )
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
    /// A step pauses the playing program and parks its playhead at `frame`
    /// itself (D-51): the sync's stop leaves the playhead to it.
    fn key_parks(&mut self, frame: Frame);
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
                    PreStep::Pause { park } => {
                        sink.key_parks(*park);
                        sink.dispatch(Command::Pause);
                    }
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
