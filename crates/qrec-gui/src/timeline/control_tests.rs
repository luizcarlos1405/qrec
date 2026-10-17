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
        rows_top: 0.0,
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
    assert_eq!(
        decide_drop(&state, Some(&view), &TakeId::numbered(4), 0.0),
        None
    );
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
    assert_eq!(
        sink.pans,
        [10.0, 10.0, 10.0],
        "the lanes follow the vertical drag: 10 px down from the press"
    );
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
    g.on_state(&gone, true, &mut sink);
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
        decide_drop(&state, Some(&view), &TakeId::numbered(4), c2d0 + 2.0),
        Some(Command::ClipAdd {
            take: TakeId::numbered(4),
            at: qrec_core::protocol::command::InsertAt::Index(7),
        })
    );
    // Broken and too-short takes don't go on the timeline.
    assert_eq!(
        decide_drop(&state, Some(&view), &TakeId::numbered(9), 0.0),
        None
    );
    assert_eq!(
        decide_drop(&state, Some(&view), &TakeId::numbered(6), 0.0),
        None
    );
    assert_eq!(parse_drop("qrec-take:take-0004"), Some(TakeId::numbered(4)));
    assert_eq!(parse_drop("take-0004"), None);
    assert_eq!(parse_drop("qrec-take:nope"), None);
}

/// SPEC §7.1, D-38: a take dragged onto an empty timeline (the S01b page,
/// no canvas and no viewport: the fit zoom of 0 frames is none) goes in at
/// its only slot, the first (the drop was refused).
#[test]
fn a_bin_drop_on_an_empty_timeline_is_the_first_clip() {
    let mut state = testkit::design_state();
    state.project.timeline.clips.clear();
    assert!(
        Viewport::from_state(&state, 1040.0).is_none(),
        "no viewport"
    );
    assert_eq!(
        decide_drop(&state, None, &TakeId::numbered(4), 300.0),
        Some(Command::ClipAdd {
            take: TakeId::numbered(4),
            at: qrec_core::protocol::command::InsertAt::Index(0),
        })
    );
    assert_eq!(decide_drop(&state, None, &TakeId::numbered(9), 300.0), None);
    // With clips, the slot needs the viewport the canvas shows.
    let state = testkit::design_state();
    assert_eq!(decide_drop(&state, None, &TakeId::numbered(4), 300.0), None);
}

// ------------------------------------------------------------- the ghost

/// RMS readings in dB, oldest first.
fn readings(db: &[f32]) -> Vec<f32> {
    db.to_vec()
}

/// The ghost's waveform for a window of its columns is that window of the
/// whole width's (review: only the columns on screen are computed).
#[test]
fn the_ghost_waveform_in_a_window_is_the_window_of_the_whole() {
    let history = readings(
        &(0..400)
            .map(|i| -60.0 + (i % 37) as f32 * 1.5)
            .collect::<Vec<_>>(),
    );
    let (ms, w): (f64, f64) = (20_000.0, 733.4);
    let all = w.ceil() as usize;
    let full = ghost_amplitudes(&history, |db| *db, ms, w, 0..all);
    assert_eq!(full.len(), all);
    for (lo, hi) in [(0, 10), (100, 400), (700, all), (0, all)] {
        let window = ghost_amplitudes(&history, |db| *db, ms, w, lo..hi);
        assert_eq!(window, full[all - hi..all - lo], "{lo}..{hi}");
        // Reading only the newest readings the window needs changes nothing.
        let wanted = ghost_readings(ms, w, &(lo..hi));
        let newest = &history[history.len().saturating_sub(wanted)..];
        assert_eq!(
            ghost_amplitudes(newest, |db| *db, ms, w, lo..hi),
            window,
            "{lo}..{hi} from the newest {wanted}"
        );
    }
}

/// Columns older than the readings reach are silent, not a flat band at
/// the oldest reading's level (review): ten readings (500 ms) under a 60 s
/// ghost of 1000 px fill its newest 8 columns.
#[test]
fn columns_older_than_the_readings_are_silent() {
    let history = readings(&[-6.0; 10]);
    let amps = ghost_amplitudes(&history, |db| *db, 60_000.0, 1000.0, 0..1000);
    let loud = (10f32.powf(-6.0 / 20.0) * std::f32::consts::SQRT_2).min(1.0);
    // Screen order: the newest (column 0) is last.
    let newest: Vec<f32> = amps.iter().rev().take(8).copied().collect();
    assert!(newest.iter().all(|a| (a - loud).abs() < 1e-6), "{newest:?}");
    assert!(
        amps[..992].iter().all(|a| *a == 0.0),
        "{:?}",
        &amps[980..992]
    );
}

/// R05: a new project's first take at fit (no clips, 30 fps, 1000 px), its
/// record clocks every 250 ms from 1 s, a frame every 10 ms: the ghost's
/// end and the playhead parked there never pass the fit's 8 px inset, also
/// between clocks (the ghost grows on the frame clock by up to 300 ms; at
/// 1 s it ran 25 % past the time area when the fit followed the clocks
/// only). Through the steps the timeline runs itself, `follow_snapshot`
/// per snapshot and `follow_frame` per frame (its audit: a copy of them
/// here passed with the frame's step gone from the timeline).
#[test]
fn fit_follows_the_ghost_between_record_clocks() {
    use qrec_core::view::timeline::{FIT_INSET_PX, ghost, shown_playhead};
    let mut state = testkit::design_state();
    state.project.timeline.clips.clear();
    let recording = |elapsed_ns: u64| RecordPhase::Recording {
        take: TakeId::numbered(10),
        mode: RecordMode::Append,
        elapsed_ns,
        waiters: vec![],
    };
    let mut vm = ViewModel::new(false);
    vm.set_width(1000.0);
    let mut clock = GhostClock::default();
    let mut elapsed = 1_000_000_000u64;
    let mut last_clock = ms(0);
    let mut grew = 0;
    state.runtime.record = recording(elapsed);
    follow_snapshot(&mut vm, &mut clock, &state, ms(0));
    for step in 0..=300u64 {
        let now = ms(step * 10);
        // A clock 250 ms after the last (one late by 60 ms, past the lead).
        let every = if step / 25 == 4 { 310 } else { 250 };
        if now.saturating_sub(last_clock) >= ms(every) {
            elapsed += (now - last_clock).as_nanos() as u64;
            last_clock = now;
            state.runtime.record = recording(elapsed);
            follow_snapshot(&mut vm, &mut clock, &state, now);
        }
        // A frame.
        grew += usize::from(follow_frame(&mut vm, &clock, &state, now, false));
        let e = clock.elapsed(now, false);
        let view = vm.shown(now).unwrap();
        let bound = view.width - FIT_INSET_PX + 1e-6;
        let parked = view.x_of_frame(shown_playhead(&state, e, None));
        assert!(parked <= bound, "{now:?}: the playhead at {parked}");
        let g = ghost(&state, &view, e).unwrap();
        assert!(
            g.x + g.w <= bound,
            "{now:?}: the ghost ends at {}",
            g.x + g.w
        );
    }
    // About a frame of total every 33 ms of the 3 s (89 of the 301 frames
    // grew it): between clocks too.
    assert!(grew > 60, "the fit grew on {grew} frames of 301");
    // Test mode's frozen clock: the frames grow nothing.
    let mut frozen = vm.clone();
    assert!(!follow_frame(&mut frozen, &clock, &state, ms(3005), true));
    // Stopped: the ghost keeps the last clock's length, and the fit too.
    state.runtime.record = RecordPhase::Finalizing {
        take: TakeId::numbered(10),
        mode: RecordMode::Append,
        cancelled: false,
        stop_reqs: vec![],
        waiters: vec![],
        discard: None,
    };
    follow_snapshot(&mut vm, &mut clock, &state, ms(3010));
    assert_eq!(vm.total(), shown_total(&state, elapsed));
    assert!(!follow_frame(&mut vm, &clock, &state, ms(3020), false));
}

/// R14: the filmstrip is drawn 72 px × the UI scale × the window's scale
/// factor high: its textures are made at that height, not upscaled.
#[test]
fn thumbnails_are_made_at_the_drawn_height() {
    for (ui, factor, want) in [
        (1.0, 1, 72),
        (1.25, 1, 90),
        (1.5, 1, 108),
        (1.0, 2, 144),
        (1.25, 2, 180),
        (1.5, 2, 216),
        (2.0, 0, 144),
    ] {
        assert_eq!(thumb_px(ui, factor), want, "ui {ui} at {factor}");
    }
}

// ------------------------------------------------------------- fit

/// R03: `\` (and the toolbar's Fit) go to the timeline's own GUI action,
/// so a local zoom still waiting for its `SetUi` can't win over them: from
/// fit, the core's `SetUi{zoom: Clear}` changes nothing a snapshot shows.
#[test]
fn fit_goes_through_the_timeline() {
    use crate::keys::router::{KeyDecision, RouterState, decide_binding};
    let state = testkit::design_state();
    let keymap = qrec_core::keymap::resolve("");
    let local = crate::store::LocalUi::default();
    let rs = RouterState {
        state: &state,
        local: &local,
        keymap: &keymap,
        player_frame: None,
    };
    assert_eq!(
        decide_binding("zoom.fit", &rs),
        KeyDecision::Gui {
            id: "zoom.fit".into()
        }
    );
}

/// `=` from fit, then `\` within the 300 ms: fit shows at once and nothing
/// goes out (the state is at fit already); from a zoom it sends the clear.
#[test]
fn fit_drops_a_waiting_local_zoom() {
    let state = testkit::design_state();
    let (mut vm, fit, _) = setup(&state);
    vm.zoom_step(true, 2437, ms(0));
    assert!(vm.zoom_fit(ms(100)));
    assert_eq!(vm.shown(ms(100)), Some(fit));
    assert_eq!(vm.flush(&state, ms(400)), None);
    assert!(!vm.dirty());
    let zoomed = seconds();
    let (mut vm, _, _) = setup(&zoomed);
    assert!(vm.zoom_fit(ms(0)));
    let Some(Command::SetUi { patch }) = vm.flush(&zoomed, ms(300)) else {
        panic!("the clear goes out")
    };
    assert_eq!(patch.zoom, Nullable::Clear);
    // No view (an empty timeline): nothing to fit here.
    let mut empty = state.clone();
    empty.project.timeline.clips.clear();
    let mut vm = ViewModel::new(true);
    vm.set_width(W);
    vm.on_state(&empty, 0, ms(0));
    assert!(!vm.zoom_fit(ms(0)));
}

/// The echo rule with a stale send: once a snapshot showed what the
/// timeline sent, a later snapshot back at that value is someone else's
/// (the CLI fitting while a local zoom waits): adopted, the local one
/// dropped.
#[test]
fn a_send_echoed_once_is_forgotten() {
    let zoomed = seconds();
    let (mut vm, _, total) = setup(&zoomed);
    // The timeline fits (`\`): the clear goes out, its echo comes back.
    vm.zoom_fit(ms(0));
    vm.flush(&zoomed, ms(300)).expect("the clear");
    let mut fitted = zoomed.clone();
    fitted.ui.zoom = None;
    fitted.ui.scroll_px = 0.0;
    vm.on_state(&fitted, total, ms(310));
    // The CLI zooms; then a local zoom waits while the CLI fits again.
    vm.on_state(&zoomed, total, ms(400));
    vm.zoom_step(true, 2437, ms(500));
    vm.on_state(&fitted, total, ms(600));
    assert!(!vm.dirty(), "the CLI's fit wins");
    assert_eq!(vm.flush(&fitted, ms(900)), None);
}

/// R04: a middle drag pans the lanes with the pointer. Five lanes overflow
/// the rows, scrolled 20 px; the pointer goes up 5 px a frame for 12
/// frames, and GTK reports each motion against the canvas as laid out
/// then (it moves with the scroll the pan sets): the lanes follow 5 px a
/// frame, 60 in all (stepping by the canvas-relative travel gave 5, 0, 5,
/// 0, …: half the pointer, and a stutter).
#[test]
fn a_middle_drag_pans_the_lanes_with_the_pointer() {
    let state = seconds();
    let (mut vm, view, total) = setup(&state);
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let from = 20.0;
    // The canvas is laid out at -from; the press at y 120 in the window.
    let mut laid_out = from;
    let mut c = cx(&state, view, total, ms(0));
    c.rows_top = -laid_out;
    let press = Press {
        button: Button::Middle,
        y: 120.0 + laid_out,
        ..rows(500.0)
    };
    g.decide_press(&c, &mut sink, &mut vm, press);
    let mut lanes = from;
    let mut steps = Vec::new();
    for frame in 1..=12u64 {
        let window_y = 120.0 - 5.0 * frame as f64;
        let mut c = cx(&state, view, total, ms(16 * frame));
        c.rows_top = -laid_out;
        // GTK's canvas-relative y, against the layout of this frame.
        let y = window_y + laid_out;
        g.decide_motion(&c, &mut sink, &mut vm, Area::Rows, (500.0, y));
        // The glue: the lanes' offset is the press's minus the travel.
        let before = lanes;
        if let Some(dy) = sink.pans.last() {
            lanes = from - dy;
        }
        steps.push(lanes - before);
        // The next frame's layout puts the canvas at the new offset.
        laid_out = lanes;
    }
    assert!(
        steps.iter().all(|s| (s - 5.0).abs() < 1e-9),
        "the lanes follow the pointer: {steps:?}"
    );
    assert!((lanes - from - 60.0).abs() < 1e-9, "{lanes}");
}

/// R07: a ruler scrub or a middle-drag pan whose timeline goes empty
/// (another client's `clip remove` or `undo`) ends with it: the ruler and
/// the canvas are gone, so their release never comes, and the scrub kept
/// `LocalUi.drag` (the router swallowed every key but `Esc`) and the pan the
/// next press.
#[test]
fn drags_end_when_the_timeline_has_no_view() {
    let state = testkit::design_state();
    let mut empty = state.clone();
    empty.project.timeline.clips.clear();
    for button in [Button::Left, Button::Middle] {
        let (mut vm, view, total) = setup(&state);
        let c = cx(&state, view, total, ms(0));
        let mut g = Gestures::new();
        let mut sink = Recorder::default();
        let at = Press {
            button,
            ..press(Area::Ruler, 10.0, 0.0, Mods::default())
        };
        assert!(g.decide_press(&c, &mut sink, &mut vm, at));
        assert!(g.drag().is_some(), "{button:?}");
        vm.on_state(&empty, 0, ms(10));
        g.on_state(&empty, vm.target_view().is_some(), &mut sink);
        assert_eq!(g.drag(), None, "{button:?}");
        if button == Button::Left {
            assert_eq!(sink.drags, [Some(DragKind::Scrub), None]);
        }
        // The clips come back: a press is a press again.
        let (mut vm, view, total) = setup(&state);
        let c = cx(&state, view, total, ms(20));
        let mut sink = Recorder::default();
        g.decide_press(&c, &mut sink, &mut vm, rows(350.0));
        assert!(matches!(g.drag(), Some(Drag::Pending { .. })), "{button:?}");
    }
    // A pan over a recording's ghost on an empty timeline goes on: it has a
    // view.
    let mut recording = empty.clone();
    recording.runtime.record = RecordPhase::Recording {
        take: TakeId::numbered(10),
        mode: RecordMode::Append,
        elapsed_ns: 10_000_000_000,
        waiters: vec![],
    };
    let mut vm = ViewModel::new(true);
    vm.set_width(W);
    vm.on_state(&recording, shown_total(&recording, 10_000_000_000), ms(0));
    let view = vm.shown(ms(0)).unwrap();
    let c = cx(&recording, view, vm.total(), ms(0));
    let mut g = Gestures::new();
    let mut sink = Recorder::default();
    let middle = Press {
        button: Button::Middle,
        ..rows(300.0)
    };
    g.decide_press(&c, &mut sink, &mut vm, middle);
    g.on_state(&recording, vm.target_view().is_some(), &mut sink);
    assert!(matches!(g.drag(), Some(Drag::Pan { .. })));
}

// ------------------------------------------------------------- silences

/// R08: the cached analyses are asked for by the key their answer carries,
/// the minimum silence in it: with c2d0's file asked at −35 dB and 400 ms,
/// a change to 600 ms asks again (the dedupe left the minimum out, so the
/// 600 ms analysis on disk was never fetched and the hatch went).
#[test]
fn a_new_min_silence_asks_for_its_cached_analysis() {
    use qrec_core::model::AutocutSettings;
    let file = "takes/take-0005/A1-mic.mka";
    let files = std::collections::BTreeMap::from([(
        file.to_owned(),
        qrec_core::ctx::FileFact {
            path: file.to_owned(),
            bytes: 4_000_000,
            mtime_ns: 1_760_000_000_000_000_000,
        },
    )]);
    let at = |min_silence_ms| AutocutSettings {
        threshold_db: -35.0,
        min_silence_ms,
        ..AutocutSettings::default()
    };
    let none = std::collections::BTreeMap::new();
    let mut asked = std::collections::BTreeSet::new();
    let first = silence_requests(&files, &at(400), &asked, &none);
    assert_eq!(first.len(), 1);
    asked.extend(first.into_iter().map(|(_, k)| k));
    assert!(silence_requests(&files, &at(400), &asked, &none).is_empty());
    let six = silence_requests(&files, &at(600), &asked, &none);
    let key = SilenceKey::compute(file, 4_000_000, 1_760_000_000_000_000_000, -35.0, 600);
    assert_eq!(six, [(file.to_owned(), key.clone())]);
    // Answered already (this session analyzed it): nothing to ask.
    let cached = std::collections::BTreeMap::from([(key, Vec::new())]);
    assert!(
        silence_requests(
            &files,
            &at(600),
            &std::collections::BTreeSet::new(),
            &cached
        )
        .is_empty()
    );
}

/// C16, C18: the playing playhead and the snap flash move nothing the
/// canvas draws (the overlay draws them over its cached node), so a frame
/// of them alone doesn't re-render it; easing, a take recording, an
/// analysis, the fit growing with the ghost, the gap closing and a trim
/// snapping to the playing playhead do. The canvas re-rendered every frame
/// of a playback (review R10).
#[test]
fn only_what_the_canvas_draws_moving_re_renders_it() {
    let still = [
        TickFacts {
            playing: true,
            ..TickFacts::default()
        },
        TickFacts {
            flash: true,
            playing: true,
            ..TickFacts::default()
        },
        TickFacts {
            trimming: true,
            ..TickFacts::default()
        },
        TickFacts::default(),
    ];
    for facts in still {
        assert!(!facts.canvas_moves(), "{facts:?}");
    }
    let moving = [
        TickFacts {
            easing: true,
            playing: true,
            ..TickFacts::default()
        },
        TickFacts {
            recording: true,
            ..TickFacts::default()
        },
        TickFacts {
            analyzing: true,
            ..TickFacts::default()
        },
        TickFacts {
            grew: true,
            ..TickFacts::default()
        },
        TickFacts {
            gap_easing: true,
            ..TickFacts::default()
        },
        TickFacts {
            trimming: true,
            playing: true,
            ..TickFacts::default()
        },
    ];
    for facts in moving {
        assert!(facts.canvas_moves(), "{facts:?}");
    }
    // A take recording moves it, but not under test mode's frozen clock.
    let mut state = testkit::design_state();
    assert_eq!(TickFacts::of(&state, false), TickFacts::default());
    state.runtime.record = RecordPhase::Recording {
        take: TakeId::numbered(10),
        mode: RecordMode::Append,
        elapsed_ns: 1_000_000_000,
        waiters: vec![],
    };
    assert!(TickFacts::of(&state, false).recording);
    assert!(!TickFacts::of(&state, true).canvas_moves());
}

// ------------------------------------------------------------- the gap

/// G§4.5, DESIGN Motion: a move drag's other clips close the gap in 120 ms
/// `EASE_OUT_EXPO` (S11: c8a1 after c1b6, c1b2–c1b6 one c8a1 to the left),
/// starting where they are drawn; at once under reduced motion. They
/// jumped there in one frame: nothing eased (review R11).
#[test]
fn a_move_drags_neighbours_ease_to_their_places() {
    let state = testkit::design_state();
    let (_, view, _) = setup(&state);
    let resting = tl::clip_boxes(&state, &view, None);
    let rest = || box_places(&resting, &view);
    let x_of = |boxes: &[ClipBox], clip: &str| {
        boxes
            .iter()
            .find(|b| b.clip == id(clip))
            .map(|b| b.x)
            .unwrap()
    };
    // x 405 in the window (S11).
    let p = tl::move_preview(&state, &view, &[id("c8a1")], 229.0).expect("a preview");
    assert_eq!(p.tooltip[0], "Move after c1b6 \u{b7} slot 7 of 10");
    let (rest_x, target_x) = (x_of(&resting, "c1b2"), x_of(&p.boxes, "c1b2"));
    let c8a1 = resting.iter().find(|b| b.clip == id("c8a1")).unwrap().w;
    assert!(
        (rest_x - target_x - c8a1).abs() < 1e-6,
        "{rest_x} → {target_x}"
    );
    let mut gap = GapEase::default();
    gap.retarget(&box_places(&p.boxes, &view), rest, ms(0), false);
    let drawn = |gap: &GapEase, now: Duration, clip: &str| {
        let mut boxes = p.boxes.clone();
        gap.ease(&mut boxes, &view, now);
        x_of(&boxes, clip)
    };
    assert!(
        (drawn(&gap, ms(0), "c1b2") - rest_x).abs() < 1e-6,
        "where it was"
    );
    let mid = drawn(&gap, ms(60), "c1b2");
    assert!(target_x < mid && mid < rest_x, "{mid}");
    assert!(
        (rest_x - mid) / (rest_x - target_x) > 0.9,
        "expo-out: most of the way at 60 ms ({mid})"
    );
    assert!((drawn(&gap, ms(120), "c1b2") - target_x).abs() < 1e-6);
    assert!(gap.animating(ms(60)) && !gap.animating(ms(120)));
    for clip in ["c1b3", "c1b6"] {
        assert!((drawn(&gap, ms(0), clip) - x_of(&resting, clip)).abs() < 1e-6);
    }
    // c7f3 (before the block) and c2d0 (after its slot) don't move.
    for clip in ["c7f3", "c2d0"] {
        assert_eq!(gap.offset(&id(clip), ms(0)), 0.0, "{clip}");
        assert!((drawn(&gap, ms(0), clip) - x_of(&resting, clip)).abs() < 1e-6);
    }
    // Reduced motion (and test mode): there at once.
    let mut instant = GapEase::default();
    instant.retarget(&box_places(&p.boxes, &view), rest, ms(0), true);
    assert!((drawn(&instant, ms(0), "c1b2") - target_x).abs() < 1e-6);
    assert!(!instant.animating(ms(0)));
    // As the canvas paints them (`painted_clips`, its audit: the paint's
    // own call to the ease was untested, and taking it out passed): the
    // drag's boxes mid-ease, the lifted copy centered on the pointer.
    let drag = Drag::Move {
        block: vec![id("c8a1")],
        x: 229.0,
    };
    let painted = |gap: &GapEase, now: Duration| {
        painted_clips(&state, &view, Some(&drag), gap, None, 0, 0, now)
    };
    for (at, want) in [
        (0, rest_x),
        (60, drawn(&gap, ms(60), "c1b2")),
        (120, target_x),
    ] {
        let paint = painted(&gap, ms(at));
        assert!(
            (x_of(&paint.boxes, "c1b2") - want).abs() < 1e-6,
            "{at} ms: c1b2 painted at {}, want {want}",
            x_of(&paint.boxes, "c1b2")
        );
        let (preview, lifted) = paint.moving.expect("a move drag's preview");
        assert_eq!(preview.tooltip, p.tooltip);
        assert_eq!(lifted.len(), 1);
        assert!(
            (lifted[0].x + lifted[0].w / 2.0 - 229.0).abs() < 1e-6,
            "the lifted copy centered on the pointer"
        );
        assert!(paint.trim.is_none());
    }
    let mid = x_of(&painted(&gap, ms(60)).boxes, "c1b2");
    assert!(target_x < mid && mid < rest_x, "{mid}");
    // No drag: the timeline's boxes as they rest.
    let still = painted_clips(&state, &view, None, &gap, None, 0, 0, ms(60));
    assert_eq!(still.boxes, tl::clip_layout(&state, &view, None, 0));
    assert!(still.moving.is_none() && still.trim.is_none());
}

/// A slot change in the middle of an ease starts the next from where the
/// clip is drawn (no jump of a block's width); a clip whose place didn't
/// change goes on easing; a zoom moves the boxes, not the eases (frames);
/// the release drops them.
#[test]
fn a_slot_change_in_the_middle_of_an_ease_never_jumps() {
    let (a, b) = (id("c1b2"), id("c1b3"));
    let rest = || vec![(id("c1b2"), 100.0), (id("c1b3"), 200.0)];
    let mut gap = GapEase::default();
    // The block went past them: both 50 frames to the left.
    gap.retarget(&[(a.clone(), 50.0), (b.clone(), 150.0)], rest, ms(0), false);
    let at = |gap: &GapEase, clip: &ClipId, place: f64, now| place + gap.offset(clip, now);
    let (a_before, b_before) = (at(&gap, &a, 50.0, ms(30)), at(&gap, &b, 150.0, ms(30)));
    assert!(50.0 < a_before && a_before < 100.0, "{a_before}");
    // Back across one boundary 30 ms in: `a` heads back to 100.
    gap.retarget(
        &[(a.clone(), 100.0), (b.clone(), 150.0)],
        rest,
        ms(30),
        false,
    );
    let a_after = at(&gap, &a, 100.0, ms(30));
    assert!((a_after - a_before).abs() < 1e-9, "{a_before} → {a_after}");
    assert!((at(&gap, &b, 150.0, ms(30)) - b_before).abs() < 1e-9);
    assert!((at(&gap, &b, 150.0, ms(120)) - 150.0).abs() < 1e-9);
    assert!((at(&gap, &a, 100.0, ms(150)) - 100.0).abs() < 1e-9);
    assert!(gap.animating(ms(149)) && !gap.animating(ms(150)));
    // The same ease in a view zoomed ×2: twice the px, the same frames.
    let view = Viewport {
        width: W,
        px_per_frame: 1.0,
        scroll_px: 0.0,
        lanes_scroll_px: 0.0,
    };
    let zoomed = Viewport {
        px_per_frame: 2.0,
        ..view
    };
    let state = testkit::design_state();
    let mut one = tl::clip_boxes(&state, &view, None);
    let mut two = tl::clip_boxes(&state, &zoomed, None);
    let x_of = |boxes: &[ClipBox]| boxes.iter().find(|c| c.clip == a).unwrap().x;
    let (x1, x2) = (x_of(&one), x_of(&two));
    gap.ease(&mut one, &view, ms(60));
    gap.ease(&mut two, &zoomed, ms(60));
    assert!(x_of(&one) != x1, "still easing at 60 ms");
    assert!(((x_of(&two) - x2) - 2.0 * (x_of(&one) - x1)).abs() < 1e-9);
    // Released (or `Esc`): none left.
    gap.clear();
    assert_eq!(gap.offset(&a, ms(31)), 0.0);
    assert!(!gap.animating(ms(31)));
}
