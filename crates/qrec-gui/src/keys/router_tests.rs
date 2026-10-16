//! Table tests of the key router (G§8.2): every default binding with a
//! recording sink, the `Esc` chain, editable passthrough, drags (D-42), the
//! bin's keys (D-11), the passive export panel (D-49), pre-steps and the
//! region cycle.

use qrec_core::keymap::{self, Action, KeyChord, KeyContext, Keymap, action_for, usable_keys};
use qrec_core::protocol::command::{Nullable, SelectOp, ShuttleDir, UiPatch};
use qrec_core::state::{CanvasMode, RecordMode, RecordPhase};
use qrec_core::view::notices::unavailable_notice;
use qrec_core::view::{DragKind, ExportPanelKind, FocusRegion, PanelPageKind};
use qrec_core::{ClipId, Command, Frame, LayerId, State, TakeId, TimeArg, testkit};

use super::*;
use crate::store::local::{LocalUi, Overlay, StartupPhase, TakePreview};

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Dispatch(Command),
    Gui(String),
    Notice(String),
    Rate(f64),
    PlayerRate(f64),
    CancelDrag,
    CloseOverlay,
}

#[derive(Default)]
struct Recorder(Vec<Call>);

impl KeySink for Recorder {
    fn dispatch(&mut self, cmd: Command) {
        self.0.push(Call::Dispatch(cmd));
    }
    fn gui(&mut self, id: &str) {
        self.0.push(Call::Gui(id.to_owned()));
    }
    fn notice(&mut self, notice: Notice) {
        self.0.push(Call::Notice(notice.title));
    }
    fn set_playing_rate(&mut self, rate: f64) {
        self.0.push(Call::Rate(rate));
    }
    fn player_rate(&mut self, rate: f64) {
        self.0.push(Call::PlayerRate(rate));
    }
    fn cancel_drag(&mut self) {
        self.0.push(Call::CancelDrag);
    }
    fn close_overlay(&mut self) {
        self.0.push(Call::CloseOverlay);
    }
}

/// A window state to press keys in.
struct Scene {
    state: State,
    local: LocalUi,
    keymap: Keymap,
    player_frame: Option<Frame>,
}

fn clip(id: &str) -> ClipId {
    id.parse().unwrap()
}

impl Scene {
    /// The design project, open, Idle, Live, the timeline focused.
    fn new() -> Self {
        Self {
            state: testkit::design_state(),
            local: LocalUi {
                startup: StartupPhase::Open,
                focus: FocusRegion::Timeline,
                ..LocalUi::default()
            },
            keymap: keymap::resolve(""),
            player_frame: None,
        }
    }

    /// Program mode with c2d0 selected (S06).
    fn program() -> Self {
        let mut scene = Self::new();
        scene.state.ui.canvas_mode = CanvasMode::Program;
        scene.state.ui.selection = vec![clip("c2d0")];
        scene
    }

    /// Live, the stage canvas focused with L2 selected.
    fn canvas() -> Self {
        let mut scene = Self::new();
        scene.local.focus = FocusRegion::Stage;
        scene.state.ui.selected_layer = Some(LayerId::numbered(2));
        scene
    }

    /// Take 10 recording.
    fn recording() -> Self {
        let mut scene = Self::new();
        scene.state.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        scene
    }

    /// The palette open.
    fn overlay() -> Self {
        let mut scene = Self::new();
        scene.local.overlays.push(Overlay::Palette);
        scene
    }

    fn rs(&self) -> RouterState<'_> {
        RouterState {
            state: &self.state,
            local: &self.local,
            keymap: &self.keymap,
            player_frame: self.player_frame,
        }
    }

    fn decide_input(&self, input: &KeyInput) -> (KeyDecision, Vec<Call>) {
        let decision = decide(input, &self.rs());
        let mut sink = Recorder::default();
        let used = apply(&decision, &self.state, &mut sink);
        assert_eq!(used, decision != KeyDecision::Propagate);
        (decision, sink.0)
    }

    fn press(&self, chord: &str) -> (KeyDecision, Vec<Call>) {
        self.decide_input(&KeyInput::chord(KeyChord::parse(chord).unwrap()))
    }

    fn calls(&self, chord: &str) -> Vec<Call> {
        self.press(chord).1
    }
}

fn dispatch(cmd: Command) -> Call {
    Call::Dispatch(cmd)
}

fn gui(id: &str) -> Call {
    Call::Gui(id.to_owned())
}

/// What binding `id` should do in `scene`, from the keymap's own
/// `action_for` and the pre-step rules (nothing plays in these scenes).
fn expected(scene: &Scene, id: &str, ctx: KeyContext) -> Vec<Call> {
    match (id, ctx) {
        ("cancel", KeyContext::Overlay) => return vec![Call::CloseOverlay],
        ("app.quit", _) => return vec![gui("app.quit")],
        ("export", _) => return vec![gui("export.panel")],
        ("autocut.preview", _) => return vec![gui("autocut.page")],
        _ => {}
    }
    match action_for(id, &scene.state).expect("a known id") {
        // D-33: the stage coalesces nudge bursts; the timeline fits its
        // own view (`zoom.fit`, step 10).
        Action::Run(_) if NUDGE_BINDINGS.contains(&id) || id == "zoom.fit" => vec![gui(id)],
        Action::Run(cmd) => {
            let mut calls = Vec::new();
            let state = &scene.state;
            if matches!(id, "record.toggle" | "record.over")
                && state.runtime.record.is_idle()
                && state.ui.canvas_mode == CanvasMode::Program
            {
                calls.push(dispatch(Command::SetCanvasMode {
                    mode: CanvasMode::Live,
                }));
            }
            match id {
                "shuttle.forward" => calls.push(Call::Rate(1.0)),
                "shuttle.reverse" => calls.push(Call::Rate(-1.0)),
                "shuttle.stop" => calls.push(Call::Rate(1.0)),
                _ => {}
            }
            calls.push(dispatch(cmd));
            calls
        }
        Action::Gui => vec![gui(id)],
        Action::Unavailable(reason) => unavailable_notice(id, &reason, &scene.state)
            .map(|n| Call::Notice(n.title))
            .into_iter()
            .collect(),
    }
}

#[test]
fn every_default_binding_runs_what_its_action_says() {
    let keymap = keymap::resolve("");
    assert!(keymap.conflicts.is_empty() && keymap.errors.is_empty());
    let mut checked = 0;
    for binding in &keymap.bindings {
        for ctx in &binding.contexts {
            let scene = match ctx {
                KeyContext::Global | KeyContext::Timeline => Scene::program(),
                KeyContext::Canvas => Scene::canvas(),
                KeyContext::Recording => Scene::recording(),
                KeyContext::Overlay => Scene::overlay(),
            };
            let want = match ctx {
                KeyContext::Global => KeyContext::Timeline,
                other => *other,
            };
            if *ctx != KeyContext::Overlay {
                assert_eq!(context(&scene.rs()), want, "{}", binding.id);
            }
            for chord in usable_keys(&keymap, binding) {
                let (decision, calls) = scene.decide_input(&KeyInput::chord(chord.clone()));
                assert_eq!(
                    calls,
                    expected(&scene, binding.id, *ctx),
                    "{} on {chord} in {ctx:?}: {decision:?}",
                    binding.id
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 70, "{checked} key presses checked");
}

#[test]
fn keys_nothing_is_bound_to_propagate() {
    let scene = Scene::program();
    for chord in ["q", "ctrl+shift+F11", "F1", "Insert"] {
        assert_eq!(scene.press(chord).0, KeyDecision::Propagate, "{chord}");
    }
    let unbindable = KeyInput {
        chord: None,
        editable: false,
        activatable: false,
    };
    assert_eq!(scene.decide_input(&unbindable).0, KeyDecision::Propagate);
}

#[test]
fn the_esc_chain_closes_the_newest_thing_first() {
    let mut scene = Scene::program();
    scene.state.ui.selected_layer = Some(LayerId::numbered(1));
    scene
        .local
        .overlays
        .push(Overlay::ExportPanel(ExportPanelKind::Running));
    scene.local.panel_page = PanelPageKind::Autocut;
    scene.local.take_preview = Some(TakePreview {
        take: TakeId::numbered(4),
        playing: true,
    });
    // 1. the passive export panel (D-49)
    assert_eq!(scene.calls("Escape"), [gui("export.panel")]);
    scene.local.overlays.clear();
    // 2. the autocut page (its Cancel)
    assert_eq!(scene.calls("Escape"), [gui("autocut.page")]);
    scene.local.panel_page = PanelPageKind::Layers;
    // 3. the bin preview
    assert_eq!(scene.calls("Escape"), [gui("bin.preview")]);
    scene.local.take_preview = None;
    // 4. `cancel`: the clip selection, then the layer selection
    assert_eq!(
        scene.calls("Escape"),
        [dispatch(Command::Select {
            op: SelectOp::Clear
        })]
    );
    scene.state.ui.selection.clear();
    assert_eq!(
        scene.calls("Escape"),
        [dispatch(Command::SetUi {
            patch: UiPatch {
                selected_layer: Nullable::Clear,
                ..UiPatch::default()
            }
        })]
    );
    scene.state.ui.selected_layer = None;
    // Nothing to cancel: silent, and the key is used.
    let (decision, calls) = scene.press("Escape");
    assert_eq!(
        decision,
        KeyDecision::Unavailable {
            id: "cancel".into(),
            reason: "Nothing to cancel".into()
        }
    );
    assert!(calls.is_empty());
    // Recording: Esc cancels the take.
    assert_eq!(
        Scene::recording().calls("Escape"),
        [dispatch(Command::RecordCancel)]
    );
}

#[test]
fn editables_get_every_key() {
    let scene = Scene::program();
    for chord in ["r", "Escape", "Tab", "ctrl+z", "space", "Left", "ctrl+q"] {
        let input = KeyInput {
            chord: Some(KeyChord::parse(chord).unwrap()),
            editable: true,
            activatable: false,
        };
        assert_eq!(
            scene.decide_input(&input).0,
            KeyDecision::Propagate,
            "{chord}"
        );
    }
}

#[test]
fn keys_during_a_drag() {
    let mut scene = Scene::program();
    scene.local.drag = Some(DragKind::TrimStart);
    assert_eq!(scene.calls("Escape"), [Call::CancelDrag]);
    assert_eq!(
        scene.calls("n"),
        [dispatch(Command::SetUi {
            patch: UiPatch {
                snapping: Some(false),
                ..UiPatch::default()
            }
        })]
    );
    // Every other key is swallowed: `s` mid-trim does nothing (D-42).
    for chord in ["s", "r", "space", "ctrl+z", "Tab", "Left"] {
        assert_eq!(
            scene.press(chord),
            (KeyDecision::Swallow, vec![]),
            "{chord}"
        );
    }
    // Snapping is off while moving (D-23).
    scene.local.drag = Some(DragKind::Move);
    assert_eq!(scene.press("n"), (KeyDecision::Swallow, vec![]));
    assert_eq!(scene.calls("Escape"), [Call::CancelDrag]);
    // A field being edited still gets its keys.
    let input = KeyInput {
        chord: Some(KeyChord::parse("Escape").unwrap()),
        editable: true,
        activatable: false,
    };
    assert_eq!(scene.decide_input(&input).0, KeyDecision::Propagate);
}

#[test]
fn the_bins_keys_are_its_own() {
    let mut scene = Scene::program();
    scene.local.focus = FocusRegion::Bin;
    for chord in ["Up", "Down", "shift+Up", "shift+Down", "Return", "space"] {
        assert_eq!(scene.press(chord).0, KeyDecision::Propagate, "{chord}");
    }
    // Other bindings still work with the bin focused.
    assert!(matches!(
        scene.press("r").0,
        KeyDecision::Run { ref id, .. } if id == "record.toggle"
    ));
    // Esc goes through the chain (here: the selection).
    assert_eq!(
        scene.calls("Escape"),
        [dispatch(Command::Select {
            op: SelectOp::Clear
        })]
    );
}

#[test]
fn focused_buttons_keep_the_navigation_keys() {
    let scene = Scene::program();
    for chord in ["space", "Return", "Left", "Down", "Home", "Page_Down"] {
        let input = KeyInput {
            chord: Some(KeyChord::parse(chord).unwrap()),
            editable: false,
            activatable: true,
        };
        assert_eq!(
            scene.decide_input(&input).0,
            KeyDecision::Propagate,
            "{chord}"
        );
    }
    // Not a navigation key: the binding runs.
    let input = KeyInput {
        chord: Some(KeyChord::parse("s").unwrap()),
        editable: false,
        activatable: true,
    };
    assert!(matches!(
        scene.decide_input(&input).0,
        KeyDecision::Run { ref id, .. } if id == "clip.split"
    ));
}

#[test]
fn overlays_that_trap_focus_keep_the_keyboard() {
    let mut scene = Scene::overlay();
    for chord in ["Tab", "shift+Tab", "Return", "space", "Up", "Down"] {
        assert_eq!(scene.press(chord).0, KeyDecision::Propagate, "{chord}");
    }
    assert_eq!(scene.calls("Escape"), [Call::CloseOverlay]);
    assert_eq!(scene.calls("ctrl+q"), [gui("app.quit")]);
    for chord in ["r", "s", "ctrl+z", "ctrl+e", "question"] {
        assert_eq!(
            scene.press(chord),
            (KeyDecision::Swallow, vec![]),
            "{chord}"
        );
    }
    // A popover closes itself on Esc.
    scene.local.overlays.push(Overlay::Menu);
    assert_eq!(scene.press("Escape").0, KeyDecision::Propagate);
    // The export panel traps in Setup, Done and Failed.
    for mode in [
        ExportPanelKind::Setup,
        ExportPanelKind::Done,
        ExportPanelKind::Failed,
    ] {
        let mut scene = Scene::program();
        scene.local.overlays.push(Overlay::ExportPanel(mode));
        assert_eq!(scene.press("s").0, KeyDecision::Swallow, "{mode:?}");
        assert_eq!(scene.calls("Escape"), [Call::CloseOverlay], "{mode:?}");
    }
}

#[test]
fn the_running_export_panel_is_passive() {
    let mut scene = Scene::program();
    scene
        .local
        .overlays
        .push(Overlay::ExportPanel(ExportPanelKind::Running));
    // Keys reach the window (S15b: the S06 hints stay live).
    assert!(matches!(
        scene.press("s").0,
        KeyDecision::Run { ref id, .. } if id == "clip.split"
    ));
    // Esc and Ctrl+E close it.
    assert_eq!(scene.calls("Escape"), [gui("export.panel")]);
    assert_eq!(scene.calls("ctrl+e"), [gui("export.panel")]);
}

#[test]
fn pre_steps() {
    // A recording started in Program first switches to Live (D-05).
    let scene = Scene::program();
    assert_eq!(
        scene.calls("r"),
        [
            dispatch(Command::SetCanvasMode {
                mode: CanvasMode::Live
            }),
            dispatch(Command::RecordToggle { wait: false })
        ]
    );
    // In Live mode it doesn't.
    assert_eq!(
        Scene::new().calls("r"),
        [dispatch(Command::RecordToggle { wait: false })]
    );
    // Playhead-dependent commands while playing seek to the player first,
    // and use its frame.
    let mut playing = Scene::program();
    playing.state.runtime.playing = true;
    playing.player_frame = Some(2500);
    let calls = playing.calls("s");
    assert_eq!(
        calls[0],
        dispatch(Command::Seek {
            to: qrec_core::protocol::command::SeekTarget::To(TimeArg::frames(2500))
        })
    );
    assert_eq!(
        calls[1],
        dispatch(Command::ClipSplit {
            clip: None,
            at: TimeArg::frames(2500),
            timeline: true
        })
    );
    // Not for commands that don't read the playhead.
    assert_eq!(playing.calls("comma").len(), 1);
    // Shuttle: L while playing at 1 → 2, told to the player too (D-44).
    assert_eq!(
        playing.calls("l"),
        [
            Call::Rate(2.0),
            Call::PlayerRate(2.0),
            dispatch(Command::Shuttle {
                dir: ShuttleDir::Forward
            })
        ]
    );
    playing.local.playing_rate = 4.0;
    assert_eq!(playing.calls("l")[0], Call::Rate(4.0));
    assert_eq!(playing.calls("j")[0], Call::Rate(-1.0));
    playing.local.playing_rate = -2.0;
    assert_eq!(
        playing.calls("j")[..2],
        [Call::Rate(-4.0), Call::PlayerRate(-4.0)]
    );
    assert_eq!(
        playing.calls("k"),
        [
            Call::Rate(1.0),
            dispatch(Command::Shuttle {
                dir: ShuttleDir::Stop
            })
        ]
    );
}

#[test]
fn shuttle_rates() {
    use ShuttleDir::*;
    assert_eq!(next_rate(1.0, false, Forward), 1.0);
    assert_eq!(next_rate(1.0, true, Forward), 2.0);
    assert_eq!(next_rate(2.0, true, Forward), 4.0);
    assert_eq!(next_rate(4.0, true, Forward), 4.0);
    assert_eq!(next_rate(-2.0, true, Forward), 1.0);
    assert_eq!(next_rate(1.0, true, Reverse), -1.0);
    assert_eq!(next_rate(-1.0, true, Reverse), -2.0);
    assert_eq!(next_rate(-4.0, true, Reverse), -4.0);
    assert_eq!(next_rate(-4.0, false, Reverse), -1.0);
    assert_eq!(next_rate(4.0, true, Stop), 1.0);
}

#[test]
fn play_keys_say_why_in_live_mode() {
    // D-01: Space in Live toasts the reason.
    assert_eq!(
        Scene::new().calls("space"),
        [Call::Notice(keymap::PLAY_IN_LIVE.to_owned())]
    );
}

#[test]
fn palette_and_cheat_sheet_are_gui_actions() {
    let scene = Scene::program();
    assert_eq!(scene.calls("ctrl+k"), [gui("palette.open")]);
    assert_eq!(scene.calls("colon"), [gui("palette.open")]);
    assert_eq!(scene.calls("question"), [gui("keys.help")]);
    assert_eq!(scene.calls("F6"), [gui("region.next")]);
    assert_eq!(scene.calls("shift+F6"), [gui("region.prev")]);
    assert_eq!(scene.calls("ctrl+q"), [gui("app.quit")]);
    // Rebound in keys.toml: the new key works, the old one is free.
    let mut rebound = Scene::program();
    rebound.keymap = keymap::resolve("[bindings]\n\"record.toggle\" = \"F9\"\n");
    assert!(matches!(
        rebound.press("F9").0,
        KeyDecision::Run { ref id, .. } if id == "record.toggle"
    ));
    assert_eq!(rebound.press("r").0, KeyDecision::Propagate);
}

#[test]
fn before_the_project_opens_only_quit_works() {
    for startup in [
        StartupPhase::Opening { slow: false },
        StartupPhase::Blocked(qrec_core::view::blocking::BlockKind::GstStuck),
        StartupPhase::Closing,
    ] {
        let mut scene = Scene::program();
        scene.local.startup = startup.clone();
        assert_eq!(scene.calls("ctrl+q"), [gui("app.quit")], "{startup:?}");
        for chord in ["r", "Return", "Escape", "Tab", "space"] {
            assert_eq!(scene.press(chord).0, KeyDecision::Propagate, "{chord}");
        }
    }
}

#[test]
fn layer_nudges_need_the_canvas_focused() {
    let scene = Scene::canvas();
    assert_eq!(context(&scene.rs()), KeyContext::Canvas);
    // D-33: to the stage's nudge burst (one `LayerSet` per burst).
    assert_eq!(scene.calls("Left"), [gui("layer.nudge-left")]);
    assert_eq!(scene.calls("shift+Down"), [gui("layer.nudge-down-10")]);
    // A locked layer: why, not a burst.
    let mut locked = Scene::canvas();
    locked.state.project.scene.layers[1].locked = true;
    assert_eq!(
        locked.calls("Left"),
        [Call::Notice("Layer L2 is locked".to_owned())]
    );
    // The timeline focused: the arrows seek.
    let mut timeline = Scene::canvas();
    timeline.local.focus = FocusRegion::Timeline;
    assert!(matches!(
        timeline.press("Left").0,
        KeyDecision::Run { ref id, .. } if id == "seek.prev-frame"
    ));
    // Program mode with nothing selected: nothing to edit (D-06).
    let mut program = Scene::canvas();
    program.state.ui.canvas_mode = CanvasMode::Program;
    assert_eq!(context(&program.rs()), KeyContext::Timeline);
}

/// D-33 with D-42: during a nudge burst the nudge keys go on with it,
/// `Esc` cancels it and other keys are swallowed.
#[test]
fn a_nudge_burst_takes_the_nudge_keys() {
    let mut scene = Scene::canvas();
    scene.local.drag = Some(DragKind::LayerNudge);
    assert_eq!(scene.calls("Left"), [gui("layer.nudge-left")]);
    assert_eq!(scene.calls("shift+Up"), [gui("layer.nudge-up-10")]);
    assert_eq!(scene.calls("Escape"), [Call::CancelDrag]);
    assert_eq!(scene.press("r").0, KeyDecision::Swallow);
    // Another layer drag swallows the arrows.
    scene.local.drag = Some(DragKind::LayerMove);
    assert_eq!(scene.press("Left").0, KeyDecision::Swallow);
}

#[test]
fn the_region_cycle() {
    use FocusRegion::*;
    let walk = |start: FocusRegion, forward: bool, bin: bool| {
        let mut out = vec![start];
        let mut at = start;
        for _ in 0..7 {
            at = next_region(at, forward, bin);
            out.push(at);
        }
        out
    };
    assert_eq!(
        walk(Header, true, false),
        [
            Header,
            Transport,
            TimelineToolbar,
            Timeline,
            Panel,
            Header,
            Transport,
            TimelineToolbar
        ]
    );
    assert_eq!(
        walk(Header, true, true),
        [
            Header,
            Transport,
            TimelineToolbar,
            Timeline,
            Panel,
            Bin,
            Header,
            Transport
        ]
    );
    assert_eq!(
        walk(Header, false, true),
        [
            Header,
            Bin,
            Panel,
            Timeline,
            TimelineToolbar,
            Transport,
            Header,
            Bin
        ]
    );
    assert_eq!(next_region(Stage, true, false), Transport);
    assert_eq!(next_region(Stage, false, false), Header);
    assert_eq!(next_region(Other, true, false), Header);
    assert_eq!(next_region(Other, false, false), Panel);
    assert_eq!(next_region(Field, true, true), Header);
    // A bin that closed while focused: around to the header.
    assert_eq!(next_region(Bin, true, false), Header);
}

#[test]
fn region_names() {
    assert_eq!(region_for_name("region-header"), Some(FocusRegion::Header));
    assert_eq!(region_for_name("transport"), Some(FocusRegion::Transport));
    assert_eq!(
        region_for_name("timeline-toolbar"),
        Some(FocusRegion::TimelineToolbar)
    );
    assert_eq!(region_for_name("region-bin"), Some(FocusRegion::Bin));
    assert_eq!(region_for_name("region-hintbar"), None);
    assert_eq!(region_for_name("preview"), None);
}

/// The user's keymap (`keys.toml`): a rebound key runs its binding and the
/// default key no longer does; a chord bound to two actions runs neither
/// (Q5) and reaches GTK.
#[test]
fn rebound_and_conflicting_keys_follow_the_users_keymap() {
    let mut scene = Scene::new();
    scene.keymap =
        keymap::resolve("[bindings]\n\"record.toggle\" = \"F9\"\n\"autocut.run\" = \"x\"\n");
    assert!(!scene.keymap.conflicts.is_empty(), "x is a conflict");
    let record = match action_for("record.toggle", &scene.state).expect("a binding") {
        Action::Run(cmd) => cmd,
        other => panic!("{other:?}"),
    };
    assert_eq!(scene.calls("F9"), [dispatch(record)]);
    assert_eq!(
        scene.press("r").0,
        KeyDecision::Propagate,
        "r isn't bound now"
    );
    assert_eq!(scene.press("x").0, KeyDecision::Propagate, "neither runs");
    // The other defaults are untouched.
    assert_eq!(
        scene.press("b").0,
        decide_binding("bin.toggle", &scene.rs())
    );
}
