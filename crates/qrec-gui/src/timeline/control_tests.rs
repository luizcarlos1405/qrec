//! The timeline's gestures and view model with a fake dispatcher (G§7 WP3
//! acceptance): one undoable command per gesture and none on `Esc`; D-37's
//! selection commands; the scrub's 10 seeks a second plus one on release;
//! one `SetUi{scroll_px}` 300 ms after the last wheel or pan event; bin
//! drops; the zoom easing, instant under reduced motion; the echo rule.

use qrec_core::protocol::command::MoveTo;
use qrec_core::state::{RecordMode, RecordPhase};
use qrec_core::testkit;
use qrec_core::view::timeline::{ROWS, shown_total};

use super::*;

/// What the gestures asked for.
#[derive(Debug, Default)]
struct Recorder {
    commands: Vec<Command>,
    drags: Vec<Option<DragKind>>,
    trim_frames: Vec<Option<(TakeId, Frame)>>,
    hovers: Vec<Option<ClipId>>,
    scrubs: Vec<Frame>,
    notices: Vec<String>,
    pans: Vec<f64>,
}

impl Sink for Recorder {
    fn dispatch(&mut self, cmd: Command) {
        self.commands.push(cmd);
    }
    fn set_drag(&mut self, drag: Option<DragKind>) {
        self.drags.push(drag);
    }
    fn set_trim_frame(&mut self, frame: Option<(TakeId, Frame)>) {
        self.trim_frames.push(frame);
    }
    fn set_hover(&mut self, clip: Option<ClipId>) {
        self.hovers.push(clip);
    }
    fn scrub(&mut self, frame: Frame) {
        self.scrubs.push(frame);
    }
    fn notice(&mut self, notice: Notice) {
        self.notices.push(notice.title);
    }
    fn pan_lanes(&mut self, dy: f64) {
        self.pans.push(dy);
    }
}

impl Recorder {
    /// The undoable commands (everything but selection, seeks and UI).
    fn undoable(&self) -> Vec<&Command> {
        self.commands.iter().filter(|c| !c.ui_only()).collect()
    }

    fn seeks(&self) -> Vec<Frame> {
        self.commands
            .iter()
            .filter_map(|c| match c {
                Command::Seek {
                    to: SeekTarget::To(t),
                } => t.magnitude_frames(qrec_core::Fps::new(30, 1)).ok(),
                _ => None,
            })
            .map(|f| f as Frame)
            .collect()
    }
}

const W: f64 = 1040.0;

fn id(s: &str) -> ClipId {
    s.parse().unwrap()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The design state, its view model and the shown view.
fn setup(state: &State) -> (ViewModel, Viewport, Frame) {
    let mut view = ViewModel::new(true);
    view.set_width(W);
    let total = shown_total(state, 0);
    view.on_state(state, total, ms(0));
    let shown = view.shown(ms(0)).unwrap();
    (view, shown, total)
}

fn cx<'a>(state: &'a State, view: Viewport, total: Frame, now: Duration) -> Cx<'a> {
    Cx {
        state,
        view,
        total,
        rows: ROWS,
        lanes: 2,
        playhead: state.ui.playhead,
        now,
    }
}

fn press(area: Area, x: f64, y: f64, mods: Mods) -> Press {
    Press {
        area,
        button: Button::Left,
        x,
        y,
        mods,
    }
}

fn rows(x: f64) -> Press {
    press(Area::Rows, x, 30.0, Mods::default())
}

/// The design state at 48 px/s from 00:01:05:00 (S08).
fn seconds() -> State {
    let mut state = testkit::design_state();
    state.ui.zoom = Some(1.6);
    state.ui.scroll_px = 1950.0 * 1.6;
    state
}

// ------------------------------------------------------------- D-37

#[test]
fn a_press_on_an_unselected_clip_selects_it() {
    let state = testkit::design_state();
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut sink = Recorder::default();
    let mut g = Gestures::new();
    // c2d0 is at 291.0 → 443.5 at fit.
    assert!(g.decide_press(&c, &mut sink, &mut vm, rows(350.0)));
    assert_eq!(
        sink.commands,
        [Command::Select {
            op: SelectOp::Set(vec![id("c2d0")])
        }]
    );
    g.decide_release(&c, &mut sink, (350.0, 30.0));
    assert_eq!(sink.commands.len(), 1, "nothing on release");
    assert!(sink.undoable().is_empty() && sink.drags.is_empty());
}

#[test]
fn a_press_on_a_selected_clip_selects_it_alone_on_release() {
    let mut state = testkit::design_state();
    state.ui.selection = vec![id("c2d0"), id("c2d1")];
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut sink = Recorder::default();
    let mut g = Gestures::new();
    g.decide_press(&c, &mut sink, &mut vm, rows(350.0));
    assert!(sink.commands.is_empty(), "nothing at the press");
    g.decide_release(&c, &mut sink, (350.0, 30.0));
    assert_eq!(
        sink.commands,
        [Command::Select {
            op: SelectOp::Set(vec![id("c2d0")])
        }]
    );
    // Already the only one selected: nothing to send.
    state.ui.selection = vec![id("c2d0")];
    let c = cx(&state, view, total, ms(0));
    let mut sink = Recorder::default();
    g.decide_press(&c, &mut sink, &mut vm, rows(350.0));
    g.decide_release(&c, &mut sink, (350.0, 30.0));
    assert!(sink.commands.is_empty());
}

#[test]
fn modifiers_and_empty_rows_select_without_a_drag() {
    let mut state = testkit::design_state();
    state.ui.selection = vec![id("c1b2")];
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut g = Gestures::new();
    let shift = Mods {
        shift: true,
        ..Mods::default()
    };
    let ctrl = Mods {
        ctrl: true,
        ..Mods::default()
    };
    let mut sink = Recorder::default();
    g.decide_press(
        &c,
        &mut sink,
        &mut vm,
        press(Area::Rows, 350.0, 30.0, shift),
    );
    assert_eq!(g.drag(), None, "no drag with a modifier");
    g.decide_press(&c, &mut sink, &mut vm, press(Area::Rows, 350.0, 30.0, ctrl));
    // Below the lanes: an empty row.
    g.decide_press(&c, &mut sink, &mut vm, rows(350.0).with_y(230.0));
    // Past the end of the timeline: an empty row too.
    g.decide_press(&c, &mut sink, &mut vm, rows(1036.0));
    assert_eq!(
        sink.commands,
        [
            Command::Select {
                op: SelectOp::RangeTo(id("c2d0"))
            },
            Command::Select {
                op: SelectOp::Toggle(id("c2d0"))
            },
            Command::Select {
                op: SelectOp::Clear
            },
            Command::Select {
                op: SelectOp::Clear
            },
        ]
    );
    // Nothing selected: an empty row sends nothing.
    state.ui.selection.clear();
    let c = cx(&state, view, total, ms(0));
    let mut sink = Recorder::default();
    g.decide_press(&c, &mut sink, &mut vm, rows(1036.0));
    assert!(sink.commands.is_empty());
}

impl Press {
    fn with_y(mut self, y: f64) -> Self {
        self.y = y;
        self
    }
}

// ------------------------------------------------------------- moves

/// S11: c8a1 dragged after c1b6; one `ClipMove` on release.
#[test]
fn a_move_is_one_command_on_release() {
    let state = testkit::design_state();
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let c8a1 = view.x_of_frame(410) + 30.0;
    g.decide_press(&c, &mut sink, &mut vm, rows(c8a1));
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (c8a1 + 3.0, 30.0));
    assert!(
        matches!(g.drag(), Some(Drag::Pending { .. })),
        "3 px: not a move yet"
    );
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (c8a1 + 4.0, 30.0));
    assert!(matches!(g.drag(), Some(Drag::Move { .. })));
    assert_eq!(sink.drags, [Some(DragKind::Move)]);
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (229.0, 30.0));
    g.decide_release(&c, &mut sink, (229.0, 30.0));
    assert_eq!(
        sink.undoable(),
        [&Command::ClipMove {
            clips: vec![id("c8a1")],
            to: MoveTo::Index(6)
        }]
    );
    assert_eq!(sink.drags, [Some(DragKind::Move), None]);
}

#[test]
fn esc_cancels_a_move_and_a_drop_back_home_sends_nothing() {
    let state = testkit::design_state();
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let c8a1 = view.x_of_frame(410) + 30.0;
    g.decide_press(&c, &mut sink, &mut vm, rows(c8a1));
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (229.0, 30.0));
    g.decide_cancel(&mut sink);
    g.decide_release(&c, &mut sink, (229.0, 30.0));
    assert!(sink.undoable().is_empty(), "{:?}", sink.commands);
    assert_eq!(sink.drags, [Some(DragKind::Move), None]);
    // Dropped where it was: nothing.
    let mut sink = Recorder::default();
    g.decide_press(&c, &mut sink, &mut vm, rows(c8a1));
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (c8a1 + 10.0, 30.0));
    g.decide_release(&c, &mut sink, (view.x_of_frame(410) + 1.0, 30.0));
    assert!(sink.undoable().is_empty());
}

#[test]
fn a_selected_block_moves_together() {
    let mut state = testkit::design_state();
    state.ui.selection = vec![id("c1b2"), id("c1b3")];
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let c1b2 = view.x_of_frame(867) + 10.0;
    g.decide_press(&c, &mut sink, &mut vm, rows(c1b2));
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (0.0, 30.0));
    g.decide_release(&c, &mut sink, (-5.0, 30.0));
    assert_eq!(
        sink.undoable(),
        [&Command::ClipMove {
            clips: vec![id("c1b2"), id("c1b3")],
            to: MoveTo::Index(0)
        }]
    );
}

#[test]
fn no_drags_while_recording() {
    let mut state = testkit::design_state();
    state.runtime.record = RecordPhase::Recording {
        take: TakeId::numbered(10),
        mode: RecordMode::Append,
        elapsed_ns: 0,
        waiters: vec![],
    };
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let c8a1 = view.x_of_frame(410) + 30.0;
    g.decide_press(&c, &mut sink, &mut vm, rows(c8a1));
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (229.0, 30.0));
    g.decide_release(&c, &mut sink, (229.0, 30.0));
    // Selecting still works (D-43); nothing moves.
    assert_eq!(
        sink.commands,
        [Command::Select {
            op: SelectOp::Set(vec![id("c8a1")])
        }]
    );
    assert!(sink.drags.is_empty());
    assert_eq!(decide_drop(&state, &view, &TakeId::numbered(4), 0.0), None);
}

// ------------------------------------------------------------- trims

/// S10: c2d0's start edge, +46.4 px at 48 px/s.
#[test]
fn a_trim_is_one_command_on_release() {
    let mut state = seconds();
    state.ui.selection = vec![id("c2d0")];
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let edge = view.x_of_frame(2084);
    g.decide_press(&c, &mut sink, &mut vm, rows(edge + 1.0));
    assert!(sink.commands.is_empty(), "a trim zone doesn't select");
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (edge + 47.4, 30.0));
    assert_eq!(sink.drags, [Some(DragKind::TrimStart)]);
    assert_eq!(
        sink.trim_frames.last(),
        Some(&Some((TakeId::numbered(5), 41)))
    );
    g.decide_release(&c, &mut sink, (edge + 47.4, 30.0));
    assert_eq!(
        sink.undoable(),
        [&Command::ClipTrim {
            clip: id("c2d0"),
            start: Some(TimeArg::plus_frames(29)),
            end: None,
            timeline: false,
        }]
    );
    assert_eq!(sink.drags, [Some(DragKind::TrimStart), None]);
    assert_eq!(sink.trim_frames.last(), Some(&None));
}

#[test]
fn esc_cancels_a_trim_and_a_click_on_an_edge_does_nothing() {
    let state = seconds();
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let edge = view.x_of_frame(2084);
    g.decide_press(&c, &mut sink, &mut vm, rows(edge + 1.0));
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (edge + 60.0, 30.0));
    g.decide_cancel(&mut sink);
    g.decide_release(&c, &mut sink, (edge + 60.0, 30.0));
    assert!(sink.commands.is_empty());
    assert_eq!(sink.drags, [Some(DragKind::TrimStart), None]);
    assert_eq!(sink.trim_frames.last(), Some(&None));
    let mut sink = Recorder::default();
    g.decide_press(&c, &mut sink, &mut vm, rows(edge + 1.0));
    g.decide_release(&c, &mut sink, (edge + 1.0, 30.0));
    assert!(sink.commands.is_empty() && sink.drags.is_empty());
}

/// `n` mid-trim (D-42) toggles `ui.snapping`; the release follows it.
#[test]
fn a_trim_follows_snapping() {
    let mut state = seconds();
    let (mut vm, view, total) = setup(&state);
    let edge = view.x_of_frame(2084);
    // 2084 + 350 is 4.8 px from the playhead at 2437.
    let to = edge + 560.0;
    for (snapping, d) in [(true, 353), (false, 350)] {
        state.ui.snapping = snapping;
        let c = cx(&state, view, total, ms(0));
        let mut g = Gestures::new();
        let mut sink = Recorder::default();
        g.decide_press(&c, &mut sink, &mut vm, rows(edge + 1.0));
        g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (to + 1.0, 30.0));
        g.decide_release(&c, &mut sink, (to + 1.0, 30.0));
        assert_eq!(
            sink.undoable(),
            [&Command::ClipTrim {
                clip: id("c2d0"),
                start: Some(TimeArg::plus_frames(d)),
                end: None,
                timeline: false,
            }],
            "snapping {snapping}"
        );
    }
}

/// D-45 through the glue: the zones at a cut, and a narrow clip's body.
#[test]
fn trim_zones_at_a_cut_and_on_narrow_clips() {
    let mut state = testkit::design_state();
    state.ui.zoom = Some(32.0);
    state.ui.scroll_px = 3160.0 * 32.0;
    let (_, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    assert_eq!(c.hit(509.0, 30.0), Hit::TrimEnd(id("c2d0")));
    assert_eq!(c.hit(513.0, 100.0), Hit::TrimStart(id("c2d1")), "in a lane");
    assert_eq!(c.hit(513.0, 200.0), Hit::Empty, "below the lanes");
    let mut narrow = testkit::design_state();
    narrow.project.timeline.clips[1].out = 455 + 30;
    let (_, nview, ntotal) = setup(&narrow);
    let c = cx(&narrow, nview, ntotal, ms(0));
    let x = nview.x_of_frame(410);
    assert_eq!(c.hit(x + 2.0, 30.0), Hit::Body(id("c8a1")));
}

// ------------------------------------------------------------- scrub

/// D-40: a second of scrubbing sends at most 10 seeks after the press's,
/// plus one on release; the player follows every motion.
#[test]
fn a_scrub_seeks_ten_times_a_second() {
    let state = testkit::design_state();
    let (mut vm, view, total) = setup(&state);
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let c = cx(&state, view, total, ms(0));
    g.decide_press(
        &c,
        &mut sink,
        &mut vm,
        press(Area::Ruler, 10.0, 0.0, Mods::default()),
    );
    assert_eq!(sink.drags, [Some(DragKind::Scrub)]);
    for i in 1..=62u64 {
        let c = cx(&state, view, total, ms(i * 16));
        g.decide_motion(
            &c,
            &mut sink,
            &mut vm,
            Area::Ruler,
            (10.0 + i as f64 * 5.0, 0.0),
        );
    }
    // The press's, then one every ≥ 100 ms (112 ms at 16 ms motions).
    let during = sink.seeks().len();
    assert!(during <= 11, "{during} seeks in a second");
    assert_eq!(during, 9);
    assert_eq!(sink.scrubs.len(), 63, "the player follows every motion");
    let c = cx(&state, view, total, ms(1000));
    g.decide_release(&c, &mut sink, (330.0, 0.0));
    let seeks = sink.seeks();
    assert_eq!(seeks.len(), during + 1, "one on release");
    assert_eq!(*seeks.last().unwrap(), view.scrub_frame(330.0, 7391));
    assert_eq!(sink.drags, [Some(DragKind::Scrub), None]);
    assert!(sink.undoable().is_empty());
}

// ------------------------------------------------------------- wheel, pan

/// D-39: the view follows at once; one `SetUi{scroll_px}` goes out 300 ms
/// after the last event.
#[test]
fn wheel_scrolls_send_one_set_ui_after_300_ms() {
    let state = seconds();
    let (mut vm, view, _) = setup(&state);
    for t in [0, 100, 200] {
        vm.scroll_wheel(0.0, 1.0, ms(t));
    }
    assert_eq!(
        vm.shown(ms(200)).unwrap().scroll_px,
        view.scroll_px + 3.0 * 64.0
    );
    assert_eq!(vm.flush(&state, ms(450)), None, "250 ms after the last");
    let sent = vm.flush(&state, ms(500)).unwrap();
    assert_eq!(
        sent,
        Command::SetUi {
            patch: UiPatch {
                scroll_px: Some(view.scroll_px + 192.0),
                ..UiPatch::default()
            }
        }
    );
    assert_eq!(vm.flush(&state, ms(900)), None, "once");
}

#[test]
fn a_middle_drag_pans_and_sends_one_set_ui() {
    let state = seconds();
    let (mut vm, view, total) = setup(&state);
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let c = cx(&state, view, total, ms(0));
    let middle = Press {
        button: Button::Middle,
        ..rows(500.0)
    };
    g.decide_press(&c, &mut sink, &mut vm, middle);
    for (i, x) in [480.0, 450.0, 400.0].into_iter().enumerate() {
        let c = cx(&state, view, total, ms(16 * (i as u64 + 1)));
        g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (x, 40.0));
    }
    let c = cx(&state, view, total, ms(64));
    g.decide_release(&c, &mut sink, (400.0, 40.0));
    assert!(sink.commands.is_empty(), "nothing at once");
    assert_eq!(sink.pans, [-10.0], "the lanes follow the vertical drag");
    assert_eq!(vm.flush(&state, ms(300)), None);
    let Some(Command::SetUi { patch }) = vm.flush(&state, ms(348)) else {
        panic!("one SetUi 300 ms after the last motion")
    };
    assert_eq!(patch.scroll_px, Some(view.scroll_px + 100.0));
    assert_eq!(patch.zoom, Nullable::Keep);
}

// ------------------------------------------------------------- zoom

#[test]
fn zoom_eases_unless_motion_is_reduced() {
    let state = testkit::design_state();
    let mut vm = ViewModel::new(false);
    vm.set_width(W);
    vm.on_state(&state, 7391, ms(0));
    let fit = vm.shown(ms(0)).unwrap();
    vm.zoom_step(true, 2437, ms(1000));
    let mid = vm.shown(ms(1050)).unwrap();
    let end = vm.shown(ms(1200)).unwrap();
    assert!(mid.px_per_frame > fit.px_per_frame && mid.px_per_frame < end.px_per_frame);
    assert!((end.px_per_frame - 2.0 * fit.px_per_frame).abs() < 1e-12);
    assert!(vm.animating(ms(1100)) && !vm.animating(ms(1200)));
    // The playhead (the anchor) stays put while it eases.
    let x0 = fit.x_of_frame(2437);
    assert!((mid.x_of_frame(2437) - x0).abs() < 1e-6);
    // Reduced motion: instant.
    let mut reduced = ViewModel::new(true);
    reduced.set_width(W);
    reduced.on_state(&state, 7391, ms(0));
    reduced.zoom_step(true, 2437, ms(1000));
    assert_eq!(reduced.shown(ms(1000)), reduced.target_view());
    assert!(!reduced.animating(ms(1000)));
    // `−` back to fit within 300 ms: the state is at fit already, nothing
    // goes out.
    reduced.zoom_step(false, 2437, ms(1100));
    assert_eq!(reduced.flush(&state, ms(1300)), None, "not yet");
    assert!(reduced.dirty());
    assert_eq!(reduced.flush(&state, ms(1400)), None);
    assert!(!reduced.dirty());
}

#[test]
fn zoom_to_selection_and_its_toast() {
    let mut state = testkit::design_state();
    let (mut vm, _, _) = setup(&state);
    assert_eq!(
        vm.zoom_selection(&state, ms(0)),
        Err(tl::ZOOM_TO_NOTHING.to_owned())
    );
    state.ui.selection = vec![id("c3e9")];
    vm.zoom_selection(&state, ms(0)).unwrap();
    let view = vm.shown(ms(0)).unwrap();
    assert!((view.px_per_frame - 1040.0 / 3105.0).abs() < 1e-12);
    let Some(Command::SetUi { patch }) = vm.flush(&state, ms(300)) else {
        panic!()
    };
    assert_eq!(patch.zoom, Nullable::Set(1040.0 / 3105.0));
    assert!((patch.scroll_px.unwrap() - 4286.0 * 1040.0 / 3105.0).abs() < 1e-9);
}

#[test]
fn ctrl_wheel_zooms_at_the_pointer() {
    let state = testkit::design_state();
    let (mut vm, fit, _) = setup(&state);
    let anchor = 700.0;
    let before = fit.time_at_x(anchor);
    vm.zoom_wheel(-2.0, anchor, ms(0));
    let view = vm.shown(ms(0)).unwrap();
    assert!((view.px_per_frame - fit.px_per_frame * 1.25 * 1.25).abs() < 1e-12);
    assert!((view.time_at_x(anchor) - before).abs() < 1e-6);
}

// ------------------------------------------------------------- echo

/// G§3.6: the timeline keeps its own value until a snapshot shows it, and
/// adopts one it didn't send at once (cancelling its own).
#[test]
fn the_echo_rule() {
    let state = seconds();
    let (mut vm, view, total) = setup(&state);
    vm.scroll_wheel(0.0, 1.0, ms(0));
    let mine = vm.flush(&state, ms(300)).unwrap();
    let Command::SetUi { patch } = &mine else {
        panic!()
    };
    // Scrolled again before the snapshot comes back.
    vm.scroll_wheel(0.0, 1.0, ms(310));
    let mut echoed = state.clone();
    echoed.ui.scroll_px = patch.scroll_px.unwrap();
    vm.on_state(&echoed, total, ms(320));
    assert_eq!(
        vm.shown(ms(320)).unwrap().scroll_px,
        view.scroll_px + 128.0,
        "the newer local value stays"
    );
    // Someone else's zoom (`\`, the CLI): adopted, the local one dropped.
    let mut theirs = echoed.clone();
    theirs.ui.zoom = None;
    vm.on_state(&theirs, total, ms(330));
    assert_eq!(vm.shown(ms(330)).unwrap().scroll_px, 0.0);
    assert!(!vm.dirty());
    assert_eq!(vm.flush(&theirs, ms(700)), None);
    // Fit follows the timeline's length (a take landed).
    let mut longer = theirs.clone();
    longer.project.timeline.clips[9].out = 3105;
    let c = longer.project.timeline.clips[9].clone();
    let mut extra = c.clone();
    extra.id = id("c5e1");
    longer.project.timeline.clips.push(extra);
    vm.on_state(&longer, total + 3105, ms(800));
    let fitted = vm.shown(ms(800)).unwrap();
    assert!((fitted.px_per_frame - 1032.0 / (total + 3105) as f64).abs() < 1e-12);
}

#[test]
fn snapshots_cancel_drags_whose_clip_is_gone() {
    let state = testkit::design_state();
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let c8a1 = view.x_of_frame(410) + 30.0;
    g.decide_press(&c, &mut sink, &mut vm, rows(c8a1));
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (229.0, 30.0));
    let mut gone = state.clone();
    gone.project.timeline.clips.remove(1);
    g.on_state(&gone, &mut sink);
    assert_eq!(g.drag(), None);
    assert_eq!(sink.drags, [Some(DragKind::Move), None]);
}

#[test]
fn hover_follows_the_pointer() {
    let state = testkit::design_state();
    let (mut vm, view, total) = setup(&state);
    let c = cx(&state, view, total, ms(0));
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (350.0, 30.0));
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (351.0, 30.0));
    g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (1036.0, 30.0));
    g.leave(&mut sink);
    assert_eq!(sink.hovers, [Some(id("c2d0")), None]);
}

// ------------------------------------------------------------- drops

#[test]
fn bin_drops_insert_at_the_nearest_boundary() {
    let state = testkit::design_state();
    let (_, view, _) = setup(&state);
    let c2d0 = view.x_of_frame(2084);
    assert_eq!(
        decide_drop(&state, &view, &TakeId::numbered(4), c2d0 + 2.0),
        Some(Command::ClipAdd {
            take: TakeId::numbered(4),
            at: qrec_core::protocol::command::InsertAt::Index(7),
        })
    );
    // Broken and too-short takes don't go on the timeline.
    assert_eq!(decide_drop(&state, &view, &TakeId::numbered(9), 0.0), None);
    assert_eq!(decide_drop(&state, &view, &TakeId::numbered(6), 0.0), None);
    assert_eq!(parse_drop("qrec-take:take-0004"), Some(TakeId::numbered(4)));
    assert_eq!(parse_drop("take-0004"), None);
    assert_eq!(parse_drop("qrec-take:nope"), None);
}
