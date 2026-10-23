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
    KeyParks(Frame),
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
    fn key_parks(&mut self, frame: Frame) {
        self.0.push(Call::KeyParks(frame));
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
            // D-01: a play or a shuttle from Live switches to Program.
            if matches!(id, "play.toggle" | "shuttle.forward" | "shuttle.reverse")
                && !state.runtime.playing
                && state.ui.canvas_mode == CanvasMode::Live
            {
                calls.push(dispatch(Command::SetCanvasMode {
                    mode: CanvasMode::Program,
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
            // The timeline's bindings in Program (S06) and in Live (S02).
            let scenes = match ctx {
                KeyContext::Global | KeyContext::Timeline => vec![Scene::program(), Scene::new()],
                KeyContext::Canvas => vec![Scene::canvas()],
                KeyContext::Recording => vec![Scene::recording()],
                KeyContext::Overlay => vec![Scene::overlay()],
            };
            let want = match ctx {
                KeyContext::Global => KeyContext::Timeline,
                other => *other,
            };
            for scene in &scenes {
                if *ctx != KeyContext::Overlay {
                    assert_eq!(context(&scene.rs()), want, "{}", binding.id);
                }
                for chord in usable_keys(&keymap, binding) {
                    let (decision, calls) = scene.decide_input(&KeyInput::chord(chord.clone()));
                    assert_eq!(
                        calls,
                        expected(scene, binding.id, *ctx),
                        "{} on {chord} in {ctx:?}, {:?}: {decision:?}",
                        binding.id,
                        scene.state.ui.canvas_mode
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 140, "{checked} key presses checked");
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
        repeat: false,
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
        parking: false,
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
            repeat: false,
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
        repeat: false,
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
            repeat: false,
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
        repeat: false,
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
    // Every command does (R18): `,` reads the playhead too with nothing
    // selected (the clip under it).
    let seek_2500 = dispatch(Command::Seek {
        to: qrec_core::protocol::command::SeekTarget::To(TimeArg::frames(2500)),
    });
    assert_eq!(
        playing.calls("comma"),
        [
            seek_2500.clone(),
            dispatch(Command::Select { op: SelectOp::Prev })
        ]
    );
    // Shuttle: L while playing at 1 → 2, told to the player too (D-44),
    // after the seek.
    assert_eq!(
        playing.calls("l"),
        [
            seek_2500.clone(),
            Call::Rate(2.0),
            Call::PlayerRate(2.0),
            dispatch(Command::Shuttle {
                dir: ShuttleDir::Forward
            })
        ]
    );
    playing.local.playing_rate = 4.0;
    assert_eq!(playing.calls("l")[1], Call::Rate(4.0));
    assert_eq!(playing.calls("j")[1], Call::Rate(-1.0));
    playing.local.playing_rate = -2.0;
    assert_eq!(
        playing.calls("j")[1..3],
        [Call::Rate(-4.0), Call::PlayerRate(-4.0)]
    );
    assert_eq!(
        playing.calls("k"),
        [
            seek_2500,
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

/// D-01 (issue #12), D-05's mirror: `Space`, `J` and `L` in Live switch
/// the canvas to Program, then play, in one FIFO and with no toast; the
/// core plays the timeline there. The transport's Play does what `Space`
/// does. `K` only stops, and Program plays as ever.
#[test]
fn play_keys_switch_to_program_in_live_mode() {
    let live = Scene::new();
    assert_eq!(live.state.ui.canvas_mode, CanvasMode::Live);
    let to_program = dispatch(Command::SetCanvasMode {
        mode: CanvasMode::Program,
    });
    assert_eq!(
        live.calls("space"),
        [to_program.clone(), dispatch(Command::Play)]
    );
    assert_eq!(
        live.calls("l"),
        [
            to_program.clone(),
            Call::Rate(1.0),
            dispatch(Command::Shuttle {
                dir: ShuttleDir::Forward
            })
        ]
    );
    assert_eq!(
        live.calls("j"),
        [
            to_program,
            Call::Rate(-1.0),
            dispatch(Command::Shuttle {
                dir: ShuttleDir::Reverse
            })
        ]
    );
    assert_eq!(
        live.calls("k"),
        [
            Call::Rate(1.0),
            dispatch(Command::Shuttle {
                dir: ShuttleDir::Stop
            })
        ]
    );
    // Through the core: the Program canvas, playing.
    for chord in ["space", "l", "j"] {
        let state = fold(&live, chord);
        assert_eq!(state.ui.canvas_mode, CanvasMode::Program, "{chord}");
        assert!(state.runtime.playing, "{chord}");
    }
    // The transport's Play runs its binding (G§3.7): the key's decision.
    let space = live.press("space").0;
    assert!(
        matches!(&space, KeyDecision::Run { pre, .. } if pre[..] == [PreStep::SetProgram]),
        "{space:?}"
    );
    assert_eq!(decide_button("play.toggle", &live.rs()), space);
    // Program: no switch.
    assert_eq!(Scene::program().calls("space"), [dispatch(Command::Play)]);
}

/// D-01: the gates keep their order and win in Live, and nothing
/// switches: an empty timeline says so; a countdown or a recording says
/// why (`J` and `L` aren't bound while recording: they reach GTK).
#[test]
fn play_keys_in_live_say_why_they_cant_and_stay() {
    let mut empty = Scene::new();
    empty.state.project.timeline.clips.clear();
    for chord in ["space", "l", "j"] {
        assert_eq!(
            empty.calls(chord),
            [Call::Notice("The timeline is empty".to_owned())],
            "{chord}"
        );
    }
    assert_eq!(
        decide_button("play.toggle", &empty.rs()),
        KeyDecision::Unavailable {
            id: "play.toggle".into(),
            reason: "The timeline is empty".into()
        }
    );
    let mut countdown = Scene::new();
    countdown.state.runtime.record = RecordPhase::Countdown {
        remaining: 3,
        mode: RecordMode::Append,
        timer: qrec_core::ids::TimerId(1),
        start_req: None,
        wait: false,
    };
    for (scene, why) in [
        (countdown, "Not during the countdown"),
        (Scene::recording(), "Not while recording"),
    ] {
        assert_eq!(scene.state.ui.canvas_mode, CanvasMode::Live);
        assert_eq!(scene.calls("space"), [Call::Notice(why.to_owned())]);
        assert_eq!(
            decide_button("play.toggle", &scene.rs()),
            KeyDecision::Unavailable {
                id: "play.toggle".into(),
                reason: why.into()
            }
        );
        for chord in ["l", "j"] {
            assert_eq!(scene.press(chord).0, KeyDecision::Propagate, "{chord}");
        }
    }
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

/// Q5 in every context: a key two bindings share where both are active
/// runs neither of them anywhere, as the hint bar, the caps and S22's
/// banner say ("Neither runs until one is changed"). Esc rebound to Split
/// clip cancelled a take and closed an overlay (where `cancel` alone has
/// it); S rebound to Previous frame split a clip with the canvas focused
/// and stepped back while recording. The press reaches GTK now (an
/// overlay swallows it); a button still runs its binding.
#[test]
fn a_conflicting_key_runs_neither_binding_in_any_context() {
    let esc = keymap::resolve("[bindings]\n\"clip.split\" = \"Escape\"\n");
    for mut scene in [
        Scene::new(),
        Scene::program(),
        Scene::canvas(),
        Scene::recording(),
    ] {
        scene.keymap = esc.clone();
        assert_eq!(
            scene.press("Escape").0,
            KeyDecision::Propagate,
            "Esc in {:?}",
            context(&scene.rs())
        );
    }
    let mut overlay = Scene::overlay();
    overlay.keymap = esc.clone();
    assert_eq!(
        overlay.press("Escape").0,
        KeyDecision::Swallow,
        "the palette stays open"
    );
    let mut trimming = Scene::program();
    trimming.local.drag = Some(DragKind::TrimEnd);
    trimming.keymap = esc;
    assert_eq!(trimming.press("Escape").0, KeyDecision::Swallow);

    let s = keymap::resolve("[bindings]\n\"seek.prev-frame\" = \"s\"\n");
    for mut scene in [Scene::program(), Scene::canvas(), Scene::recording()] {
        scene.keymap = s.clone();
        assert_eq!(
            scene.press("s").0,
            KeyDecision::Propagate,
            "S in {:?}",
            context(&scene.rs())
        );
    }
    let mut canvas = Scene::canvas();
    canvas.keymap = s;
    assert_eq!(
        decide_button("clip.split", &canvas.rs()),
        decide_binding("clip.split", &canvas.rs())
    );
    assert_ne!(
        decide_button("clip.split", &canvas.rs()),
        KeyDecision::Propagate
    );
}

/// The state the press leaves: its dispatched commands run through the
/// core in order (what the person ends on).
fn fold(scene: &Scene, chord: &str) -> State {
    let ctx = testkit::ctx("2026-10-14T15:00:00Z", 7);
    scene
        .calls(chord)
        .into_iter()
        .fold(scene.state.clone(), |state, call| match call {
            Call::Dispatch(cmd) => {
                qrec_core::apply::apply(&state, &ctx, cmd).map_or(state, |out| out.state)
            }
            _ => state,
        })
}

/// R18: while the program plays the core's playhead lags the player (it
/// moves with the sync's once-a-second seek), and frame steps, ±1 s, edit
/// jumps and shuttles are resolved from it. A step pauses first and goes
/// from the player's frame (D-51: `Pause`, then one absolute seek to where
/// it lands, the frame the sync leaves the playhead on); a shuttle first
/// seeks to the player's frame, so it doesn't restart at 0 or stop at 0
/// on a stale playhead.
#[test]
fn relative_seeks_and_shuttles_while_playing_start_at_the_player() {
    use qrec_core::protocol::command::SeekTarget;
    let mut playing = Scene::program();
    playing.state.runtime.playing = true;
    playing.player_frame = Some(2500);
    // The edit points around 2500: c2d0's start (2084) and end (3176).
    for (chord, lands) in [
        ("Right", 2501),
        ("Left", 2499),
        ("shift+Right", 2530),
        ("shift+Left", 2470),
        ("Up", 2084),
        ("Down", 3176),
    ] {
        assert_eq!(
            playing.calls(chord),
            [
                Call::KeyParks(lands),
                dispatch(Command::Pause),
                dispatch(Command::Seek {
                    to: SeekTarget::To(TimeArg::frames(lands))
                })
            ],
            "{chord}"
        );
        let state = fold(&playing, chord);
        assert_eq!(state.ui.playhead, lands, "{chord}");
        assert!(!state.runtime.playing, "{chord} paused");
    }
    // An absolute seek doesn't read the playhead, and moves a playing
    // program on; the player on the core's playhead needs no seek first.
    assert_eq!(
        playing.calls("Home"),
        [dispatch(Command::Seek {
            to: SeekTarget::Start
        })]
    );
    assert!(fold(&playing, "Home").runtime.playing);
    playing.player_frame = Some(2437);
    assert_eq!(playing.calls("s").len(), 1);
    // Paused, a step is the keymap's relative seek, as ever.
    let paused = Scene::program();
    let Some(Action::Run(step)) = action_for("seek.next-frame", &paused.state) else {
        panic!("seek.next-frame runs");
    };
    assert_eq!(paused.calls("Right"), [dispatch(step)]);

    // L after the end (the core's playhead still at the total while the
    // player reversed 10 frames): on from the player, not from 0.
    let total = 7391;
    let mut ended = Scene::program();
    ended.state.runtime.playing = true;
    ended.state.ui.playhead = total;
    ended.player_frame = Some(total - 10);
    ended.local.playing_rate = -1.0;
    let state = fold(&ended, "l");
    assert_eq!(state.ui.playhead, total - 10);
    assert!(state.runtime.playing);
    // J just after a start from 0: backwards, not a stop at 0.
    let mut started = Scene::program();
    started.state.runtime.playing = true;
    started.state.ui.playhead = 0;
    started.player_frame = Some(15);
    let state = fold(&started, "j");
    assert!(state.runtime.playing, "reverse from 15");
    assert_eq!(state.ui.playhead, 15);
}

/// The chord a hint item's first key caps stand for (the legends
/// `KeyChord::cap_legends` gives the keys these hint states use).
fn chord_of(caps: &[String]) -> KeyChord {
    let mut parts: Vec<String> = Vec::new();
    for cap in caps {
        parts.push(match cap.as_str() {
            "Ctrl" | "Alt" | "Shift" | "Super" => cap.to_lowercase(),
            "Esc" => "Escape".into(),
            "Enter" => "Return".into(),
            "Space" => "space".into(),
            "Tab" => "Tab".into(),
            "?" => "question".into(),
            "=" => "equal".into(),
            "\u{2212}" => "minus".into(),
            "\\" => "backslash".into(),
            letter if letter.len() == 1 && letter.chars().all(|c| c.is_ascii_alphabetic()) => {
                letter.to_lowercase()
            }
            other => panic!("no chord for the cap {other:?}"),
        });
    }
    KeyChord::parse(&parts.join("+")).unwrap_or_else(|e| panic!("{parts:?}: {e}"))
}

/// The hint bar names only keys the router honors (R33): every enabled
/// item's key does something under the overlays the window stacks; the
/// Narrow layers sheet traps the keyboard (step 4), so its hints aren't
/// the editor's (`[R] Record` there was swallowed), and on the autocut
/// page in the sheet `Esc` is the page's Cancel, as docked.
#[test]
fn the_hint_bar_names_keys_the_router_honors() {
    use qrec_core::view::hints::{hint_bar, hint_state};
    let mut cases: Vec<(&str, Scene)> = Vec::new();
    cases.push(("docked", Scene::new()));
    let mut sheet = Scene::new();
    sheet.local.overlays.push(Overlay::LayersSheet);
    cases.push(("sheet", sheet));
    let mut autocut = Scene::new();
    autocut.local.overlays.push(Overlay::LayersSheet);
    autocut.local.panel_page = PanelPageKind::Autocut;
    cases.push(("autocut in the sheet", autocut));
    let mut settings = Scene::new();
    settings.local.overlays.push(Overlay::Settings(
        qrec_core::view::settings::SettingsPage::Project,
    ));
    cases.push(("settings", settings));
    for (name, scene) in &cases {
        let facts = scene.local.facts();
        let hint = hint_state(&scene.state, &facts);
        let (items, _) = hint_bar(&hint, &scene.state, &facts, &scene.keymap);
        assert!(!items.is_empty(), "{name}");
        for item in items.iter().filter(|i| i.enabled && !i.keys.is_empty()) {
            let chord = chord_of(&item.keys[0]);
            let decision = scene.decide_input(&KeyInput::chord(chord.clone())).0;
            assert_ne!(
                decision,
                KeyDecision::Swallow,
                "{name}: {hint:?} names {chord} {:?}",
                item.label
            );
            if *name == "autocut in the sheet" && chord.key == "Escape" {
                assert_eq!(item.label, "Cancel", "{name}");
                assert_eq!(
                    decision,
                    KeyDecision::Gui {
                        id: "autocut.page".into()
                    }
                );
            }
        }
    }
    // The sheet alone: `Esc` closes it.
    assert_eq!(cases[1].1.calls("Escape"), [Call::CloseOverlay]);
}

/// A button does what its key does before the project opens (R36): only
/// `app.quit`. The header's gear opened Settings over the opening window,
/// and it stayed over the blocking page that followed.
#[test]
fn buttons_wait_for_the_project_like_their_keys() {
    let ids = [
        "settings.open",
        "record.toggle",
        "export",
        "palette.open",
        "canvas.mode",
        "bin.toggle",
        "frame.save",
    ];
    for startup in [
        StartupPhase::Opening { slow: false },
        StartupPhase::Blocked(qrec_core::view::blocking::BlockKind::GstStuck),
        StartupPhase::Closing,
    ] {
        let mut scene = Scene::program();
        scene.local.startup = startup.clone();
        for id in ids {
            assert_eq!(
                decide_button(id, &scene.rs()),
                KeyDecision::Swallow,
                "{id} {startup:?}"
            );
        }
        assert_eq!(
            decide_button("app.quit", &scene.rs()),
            KeyDecision::Gui {
                id: "app.quit".into()
            }
        );
    }
    // Open: what the key's steps 10 and 11 do.
    let scene = Scene::program();
    for id in ids.into_iter().chain(["app.quit"]) {
        assert_eq!(
            decide_button(id, &scene.rs()),
            decide_binding(id, &scene.rs()),
            "{id}"
        );
    }
    assert_eq!(
        decide_button("settings.open", &scene.rs()),
        KeyDecision::Gui {
            id: "settings.open".into()
        }
    );
}

/// The held keys tell a key's auto-repeat from a new press (R30), by its
/// keycode, until its release or the window losing the keyboard.
#[test]
fn held_keys_tell_repeats_from_presses() {
    let mut held = HeldKeys::default();
    assert!(!held.press(27), "a new press");
    assert!(held.press(27), "held: its repeat");
    assert!(!held.press(28), "another key");
    held.release(27);
    assert!(!held.press(27), "pressed again");
    held.clear();
    assert!(!held.press(27), "the window lost the keyboard");
    assert!(!held.press(28));
}

/// A held key repeats only what is meant to repeat (R30): holding `R`
/// started a take and its repeats stopped it; held toggles flapped.
#[test]
fn a_held_key_repeats_only_what_is_meant_to() {
    let held = |scene: &Scene, chord: &str| {
        let mut input = KeyInput::chord(KeyChord::parse(chord).unwrap());
        input.repeat = true;
        scene.decide_input(&input)
    };
    let swallowed = (KeyDecision::Swallow, vec![]);
    // Recording: `R` stops the take once.
    let recording = Scene::recording();
    assert_eq!(
        recording.calls("r"),
        [dispatch(Command::RecordToggle { wait: false })]
    );
    assert_eq!(held(&recording, "r"), swallowed);
    assert_eq!(held(&recording, "Escape"), swallowed);
    // Program: toggles and one-off actions happen once per press…
    let program = Scene::program();
    for chord in [
        "space", "Tab", "b", "n", "s", "p", "c", "l", "Escape", "ctrl+e", "x",
    ] {
        assert!(!program.calls(chord).is_empty(), "{chord} does something");
        assert_eq!(held(&program, chord), swallowed, "{chord}");
    }
    // … moving, selecting, nudging and zooming go on.
    for chord in [
        "Left",
        "Right",
        "shift+Left",
        "Up",
        "comma",
        "period",
        "less",
        "greater",
        "alt+bracketleft",
        "equal",
    ] {
        let (decision, calls) = held(&program, chord);
        assert!(
            matches!(calls[..], [Call::Dispatch(_)] | [Call::Gui(_)]),
            "{chord}: {decision:?}"
        );
        assert_eq!(calls, program.calls(chord), "{chord}");
    }
    // Space in Live switches to Program and plays once (D-01); its
    // repeats don't pause it again.
    let live = Scene::new();
    assert_eq!(live.calls("space").len(), 2, "the switch, then Play");
    assert_eq!(held(&live, "space"), swallowed);
    assert_eq!(held(&live, "l"), swallowed);
    // An unavailable binding said why on its press: Space with nothing
    // to play.
    let mut empty = Scene::new();
    empty.state.project.timeline.clips.clear();
    assert_eq!(
        empty.calls("space"),
        [Call::Notice("The timeline is empty".to_owned())]
    );
    assert_eq!(held(&empty, "space"), swallowed);
    // The canvas: layer nudges repeat, in a nudge burst too (D-33).
    assert_eq!(held(&Scene::canvas(), "Left").1, [gui("layer.nudge-left")]);
    let mut burst = Scene::canvas();
    burst.local.drag = Some(DragKind::LayerNudge);
    assert_eq!(held(&burst, "shift+Up").1, [gui("layer.nudge-up-10")]);
    assert_eq!(held(&burst, "Escape"), swallowed);
    // Widgets keep their navigation keys, not `Enter` and `Space`.
    let mut bin = Scene::program();
    bin.local.focus = FocusRegion::Bin;
    assert_eq!(held(&bin, "Down").0, KeyDecision::Propagate);
    assert_eq!(held(&bin, "Return"), swallowed);
    let overlay = Scene::overlay();
    for chord in ["Tab", "Down", "Up"] {
        assert_eq!(held(&overlay, chord).0, KeyDecision::Propagate, "{chord}");
    }
    for chord in ["Return", "space", "Escape"] {
        assert_eq!(held(&overlay, chord), swallowed, "{chord}");
    }
    let focused = |chord: &str| KeyInput {
        chord: Some(KeyChord::parse(chord).unwrap()),
        editable: false,
        activatable: true,
        repeat: true,
    };
    assert_eq!(
        program.decide_input(&focused("Right")).0,
        KeyDecision::Propagate
    );
    assert_eq!(program.decide_input(&focused("space")), swallowed);
    // A field gets every repeat (typing, Backspace).
    for chord in ["BackSpace", "r", "space"] {
        let typing = KeyInput {
            chord: Some(KeyChord::parse(chord).unwrap()),
            editable: true,
            activatable: false,
            repeat: true,
        };
        assert_eq!(
            program.decide_input(&typing).0,
            KeyDecision::Propagate,
            "{chord}"
        );
    }
    // Before the project opens: a page's button activates once.
    let mut opening = Scene::program();
    opening.local.startup = StartupPhase::Opening { slow: false };
    assert_eq!(held(&opening, "Return"), swallowed);
    assert_eq!(held(&opening, "Tab").0, KeyDecision::Propagate);
    // Undo goes on (while there is something to undo).
    let undo = program.calls("ctrl+z");
    if matches!(undo[..], [Call::Dispatch(Command::Undo { .. })]) {
        assert_eq!(held(&program, "ctrl+z").1, undo);
    }
}
