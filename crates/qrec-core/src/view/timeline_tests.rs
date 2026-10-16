//! `view::timeline` on the design sample (BRIEF §3, G§4.5): the four zoom
//! levels of the mocks with fit's 8 px inset, S12's zoom to c3e9, the
//! ghosts of S04, S04b and S05, S08's silences, the S10 trim and the S11
//! move, the hit test's trim zones (D-45), the toolbar, the headers and
//! the tooltip. Window x = time-area x + 176.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::*;
use crate::keymap;
use crate::protocol::command::SelectOp;
use crate::state::{AutocutParams, AutocutTarget, ClipRef, Job};
use crate::testkit;
use crate::{ClipId, JobId, TakeId};

const W: f64 = 1040.0;

fn id(s: &str) -> ClipId {
    s.parse().unwrap()
}

fn fit(state: &State) -> f64 {
    Viewport::fit_px_per_frame(W, shown_total(state, recording_elapsed_ns(state))).unwrap()
}

/// The design state at zoom `zoom` scrolled to frame `from`.
fn zoomed(zoom: f64, from: Frame) -> State {
    let mut state = testkit::design_state();
    state.ui.zoom = Some(zoom);
    state.ui.scroll_px = from as f64 * zoom;
    state
}

/// The visible boxes as window px.
fn boxes_text(state: &State, view: &Viewport) -> String {
    let mut out = String::new();
    for b in clip_boxes(state, view, None) {
        if b.x + b.w <= 0.0 || b.x >= view.width {
            continue;
        }
        writeln!(
            out,
            "{:<5} take {:<2} x {:>8.1} w {:>7.1} {:<8} {}{}",
            b.clip.as_str(),
            b.take.number(),
            b.x + HEADER_WIDTH,
            b.w,
            match &b.label {
                ClipLabel::Full(t) | ClipLabel::Short(t) => format!("{t:?}"),
                ClipLabel::None => "none".to_owned(),
                ClipLabel::Sliver => "sliver".to_owned(),
            },
            if b.selected { "selected " } else { "" },
            if b.missing { "missing" } else { "" },
        )
        .unwrap();
    }
    out
}

fn level(name: &str, state: &State, view: &Viewport) -> String {
    let fit = fit(state);
    let fps = state.project.canvas.fps;
    format!(
        "== {name}: {:.5} px/frame, scroll {:.1}, readout {:?}, slider {:.3}, visible {:.1} → {:.1}\n{}",
        view.px_per_frame,
        view.scroll_px,
        zoom_readout(view, fit, fps),
        slider_value(view.px_per_frame, fit),
        view.visible_frames().0,
        view.visible_frames().1,
        boxes_text(state, view)
    )
}

/// The four zoom levels of the mocks on the design state (G§7 WP3
/// acceptance): fit with the 8 px inset, seconds (S08), frame level (S09)
/// and S12's zoom to c3e9.
#[test]
fn the_four_zoom_levels() {
    let base = testkit::design_state();
    let fit_view = Viewport::from_state(&base, W).unwrap();
    assert!((fit_view.px_per_frame - 1032.0 / 7391.0).abs() < 1e-12);
    // At fit with the inset, c8a1 is 63.8 px: its label is the number.
    let fit_boxes = clip_boxes(&base, &fit_view, None);
    assert_eq!(fit_boxes[1].label, ClipLabel::Short("1".into()));
    assert_eq!(fit_boxes[6].label, ClipLabel::None, "c1b6 is 21.6 px");
    let end = fit_boxes.last().map(|b| b.x + b.w).unwrap();
    assert!((end - (W - FIT_INSET_PX)).abs() < 1e-9, "the 8 px inset");

    let seconds = zoomed(1.6, 1950);
    let seconds_view = Viewport::from_state(&seconds, W).unwrap();
    let s_boxes = clip_boxes(&seconds, &seconds_view, None);
    let c1b6 = &s_boxes[6];
    let c2d0 = &s_boxes[7];
    assert!((c1b6.x + c1b6.w + HEADER_WIDTH - 390.4).abs() < 1e-6);
    assert!((c2d0.x + HEADER_WIDTH - 390.4).abs() < 1e-6);
    assert!((seconds_view.x_of_frame(2437) + HEADER_WIDTH - 955.2).abs() < 1e-6);

    let mut frame = zoomed(32.0, 3160);
    frame.ui.selection = vec![id("c2d1")];
    frame.ui.playhead = 3176;
    let frame_view = Viewport::from_state(&frame, W).unwrap();
    let f_boxes = clip_boxes(&frame, &frame_view, None);
    assert_eq!(f_boxes[8].x + HEADER_WIDTH, 688.0, "the c2d0 | c2d1 cut");
    assert_eq!(f_boxes[8].label, ClipLabel::Full("Take 5".into()));

    let mut selection = testkit::design_state();
    selection.ui.selection = vec![id("c3e9")];
    let to_c3e9 = zoom_to_selection(&selection, &fit_view, 7391).unwrap();
    assert!((to_c3e9.px_per_frame - 1040.0 / 3105.0).abs() < 1e-12);
    assert!((to_c3e9.px_per_frame - 0.33494).abs() < 1e-5);
    let c3e9 = clip_boxes(&selection, &to_c3e9, None).pop().unwrap();
    assert!((c3e9.x + HEADER_WIDTH - 176.0).abs() < 1e-6);
    assert!((c3e9.x + c3e9.w + HEADER_WIDTH - 1216.0).abs() < 1e-6);
    assert_eq!(
        zoom_readout(&to_c3e9, fit(&selection), selection.project.canvas.fps),
        "10 px/s"
    );

    let text = [
        level("fit", &base, &fit_view),
        level("seconds (S08)", &seconds, &seconds_view),
        level("frame level (S09)", &frame, &frame_view),
        level("zoom to c3e9 (S12)", &selection, &to_c3e9),
    ]
    .concat();
    insta::assert_snapshot!("zoom_levels", text);
}

#[test]
fn zoom_to_selection_needs_one() {
    let state = testkit::design_state();
    let view = Viewport::from_state(&state, W).unwrap();
    assert_eq!(
        zoom_to_selection(&state, &view, 7391),
        Err(ZOOM_TO_NOTHING.to_owned())
    );
    assert_eq!(ZOOM_TO_NOTHING, "Select the clips to zoom to");
    // A selection too short for 128 px/frame is centered.
    let mut one = state.clone();
    one.project.timeline.clips[0].out = 3;
    one.ui.selection = vec![id("c7f3")];
    let total = total_frames(&one.project.timeline.clips);
    let v = zoom_to_selection(&one, &view, total).unwrap();
    assert_eq!(v.px_per_frame, MAX_PX_PER_FRAME);
    assert_eq!(v.scroll_px, 0.0, "clamped at the start");
}

#[test]
fn zoom_steps_slider_and_ui_patches() {
    let state = testkit::design_state();
    let total = 7391;
    let view = Viewport::from_state(&state, W).unwrap();
    let fit = view.px_per_frame;
    assert!(!can_zoom_out(&view, fit) && can_zoom_in(&view, fit));
    assert_eq!(zoom_readout(&view, fit, state.project.canvas.fps), "Fit");
    assert_eq!(slider_value(fit, fit), 0.0);
    assert!((slider_zoom(1.0, fit) - MAX_PX_PER_FRAME).abs() < 1e-9);
    assert!((slider_value(slider_zoom(0.37, fit), fit) - 0.37).abs() < 1e-12);
    // `=` anchors at the playhead (on screen at fit).
    let x = view.x_of_frame(2437);
    let zoomed = zoom_step(&view, fit, 2437, true);
    assert!((zoomed.px_per_frame - 2.0 * fit).abs() < 1e-12);
    assert!((zoomed.x_of_frame(2437) - x).abs() < 1e-6);
    // `−` back at fit: no scroll.
    let back = zoom_step(&zoomed, fit, 2437, false);
    assert!(is_fit(back.px_per_frame, fit));
    assert_eq!(back.scroll_px, 0.0);
    assert_eq!(
        ui_patch(&back, fit),
        UiPatch {
            zoom: Nullable::Clear,
            scroll_px: Some(0.0),
            ..UiPatch::default()
        }
    );
    assert_eq!(ui_patch(&zoomed, fit).zoom, Nullable::Set(2.0 * fit));
    // Off-screen playhead: the center.
    let far = Viewport {
        px_per_frame: 1.6,
        scroll_px: 0.0,
        ..view
    };
    let stepped = zoom_step(&far, fit, 5000, true);
    assert!((stepped.time_at_x(W / 2.0) - far.time_at_x(W / 2.0)).abs() < 1e-9);
    // Max zoom and the wheel.
    let max = view.with_zoom(1000.0, 0.0, fit);
    assert_eq!(max.px_per_frame, MAX_PX_PER_FRAME);
    assert!(!can_zoom_in(&max, fit));
    assert_eq!(far.scroll_for_wheel(0.0, 2.0, total), 128.0);
    assert_eq!(far.scroll_for_wheel(-1.0, 0.0, total), 0.0);
    let end = far.max_scroll(total);
    assert_eq!(far.scroll_for_wheel(0.0, 1e6, total), end);
    assert!((end - (7391.0 * 1.6 + FIT_INSET_PX - W)).abs() < 1e-9);
    // Scrubbing stays within the timeline.
    assert_eq!(far.scrub_frame(-50.0, total), 0);
    assert_eq!(far.scrub_frame(80.0, total), 50);
    let late = Viewport {
        scroll_px: end,
        ..far
    };
    assert_eq!(late.scrub_frame(W, total), total);
}

#[test]
fn auto_page_and_the_shown_playhead() {
    let state = zoomed(1.6, 1950);
    let view = Viewport::from_state(&state, W).unwrap();
    assert_eq!(auto_page(&view, 2437, 7391), None);
    let next = auto_page(&view, 2600, 7391).unwrap();
    assert!((next - (2600.0 * 1.6 - AUTO_PAGE_PX)).abs() < 1e-9);
    assert_eq!(shown_playhead(&state, 0, None), 2437);
    assert_eq!(shown_playhead(&state, 0, Some(2500)), 2500);
    // Past the end (a ui.json older than the timeline): drawn at the end,
    // as the transport reads it.
    let mut past = state.clone();
    past.ui.playhead = 9_000;
    past.ui.canvas_mode = crate::state::CanvasMode::Program;
    assert_eq!(shown_playhead(&past, 0, None), 7391);
    let transport = crate::view::canvas::transport(&past, 0, false, 1.0);
    assert_eq!(
        transport.left,
        crate::view::canvas::TransportLeft::Timecode {
            now: "00:04:06:11".into(),
            total: "00:04:06:11".into(),
        }
    );
}

// ------------------------------------------------------------- ghosts

fn recording(mode: RecordMode, frames: Frame) -> State {
    let mut state = testkit::design_state();
    state.runtime.record = RecordPhase::Recording {
        take: TakeId::numbered(10),
        mode,
        elapsed_ns: frames * NS_PER_SEC as u64 / 30,
        waiters: vec![],
    };
    state
}

/// S04: fit includes the ghost (12,233 frames); S04b: the ghost in c2d0's
/// slot; S05: the saving ghost.
#[test]
fn ghosts() {
    let s04 = recording(RecordMode::Append, 4842);
    assert_eq!(shown_total(&s04, recording_elapsed_ns(&s04)), 12_233);
    let view = Viewport::from_state(&s04, W).unwrap();
    assert!((view.px_per_frame - 1032.0 / 12_233.0).abs() < 1e-12);
    let ghost04 = ghost(&s04, &view, recording_elapsed_ns(&s04)).unwrap();
    assert_eq!(ghost04.plate, "\u{25cf} Take 10 \u{00b7} recording");
    assert!((ghost04.x + ghost04.w - (W - FIT_INSET_PX)).abs() < 1e-9);
    assert_eq!(
        shown_playhead(&s04, recording_elapsed_ns(&s04), None),
        12_233
    );
    assert!(
        clip_boxes(&s04, &view, None)
            .iter()
            .all(|b| b.opacity == 0.6)
    );

    let c2d0 = testkit::design_state().project.timeline.clips[7].clone();
    let s04b = recording(
        RecordMode::Over {
            clips: vec![ClipRef::of(&c2d0)],
        },
        932,
    );
    let view_b = Viewport::from_state(&s04b, W).unwrap();
    let ghost04b = ghost(&s04b, &view_b, recording_elapsed_ns(&s04b)).unwrap();
    assert_eq!(ghost04b.over.as_deref(), Some("over c2d0 (take 5)"));
    assert_eq!(ghost04b.replaced, vec![id("c2d0")]);
    let boxes = clip_boxes(&s04b, &view_b, None);
    assert!(boxes[7].recorded_over && !boxes[8].recorded_over);
    assert_eq!(ghost04b.x, boxes[7].x);
    assert_eq!(shown_total(&s04b, recording_elapsed_ns(&s04b)), 7391);
    // Once the ghost outgrows c2d0 it pushes the clips after it.
    let long = 1092 + 100;
    let pushed = clip_layout(&s04b, &view_b, None, long * NS_PER_SEC as u64 / 30);
    assert!((pushed[8].x - boxes[8].x - 100.0 * view_b.px_per_frame).abs() < 1e-9);

    let mut s05 = s04.clone();
    s05.runtime.record = RecordPhase::Finalizing {
        take: TakeId::numbered(10),
        mode: RecordMode::Append,
        cancelled: false,
        stop_reqs: vec![],
        waiters: vec![],
        discard: None,
    };
    let elapsed = recording_elapsed_ns(&s04);
    let ghost05 = ghost(&s05, &view, elapsed).unwrap();
    assert!(ghost05.saving);
    assert_eq!(ghost05.plate, "Take 10 \u{00b7} saving\u{2026}");
    assert_eq!(shown_total(&s05, elapsed), 12_233);
    // Esc while finalizing: no ghost.
    if let RecordPhase::Finalizing { cancelled, .. } = &mut s05.runtime.record {
        *cancelled = true;
    }
    assert_eq!(ghost(&s05, &view, elapsed), None);
    insta::assert_json_snapshot!("ghosts", [ghost04, ghost04b, ghost05]);
}

// ------------------------------------------------------------- previews

/// S10: c2d0's start edge dragged right by 46.4 px at 48 px/s.
#[test]
fn the_s10_trim() {
    let mut state = zoomed(1.6, 1950);
    state.ui.selection = vec![id("c2d0")];
    let view = Viewport::from_state(&state, W).unwrap();
    let p = trim_preview(
        &state,
        &view,
        &id("c2d0"),
        TrimEdge::Start,
        46.4,
        true,
        2437,
    )
    .unwrap();
    assert_eq!(p.d, 29);
    assert!((p.edge_x + HEADER_WIDTH - 436.8).abs() < 1e-6);
    let (gx, gw) = p.ghost.unwrap();
    assert!((gx + HEADER_WIDTH - 390.4).abs() < 1e-6 && (gw - 46.4).abs() < 1e-6);
    assert_eq!(p.tag, "\u{2212}29 f");
    assert_eq!(
        p.tooltip,
        [
            "In \u{00b7} take 5 frame 41 \u{00b7} 00:00:01:11".to_owned(),
            "\u{0394} \u{2212}29 f".to_owned()
        ]
    );
    assert_eq!(p.trim_frame, (TakeId::numbered(5), 41));
    assert_eq!(
        p.ripple,
        Some(RippleTag {
            text: "\u{2190} 00:00:00:29".into(),
            anchor: TagAnchor::AfterLabel { clip: id("c2d0") },
        })
    );
    assert_eq!(
        p.command,
        Some(Command::ClipTrim {
            clip: id("c2d0"),
            start: Some(TimeArg::plus_frames(29)),
            end: None,
            timeline: false,
        })
    );
    let trimmed = &p.boxes[7];
    assert_eq!(trimmed.trimming, Some(TrimEdge::Start));
    assert_eq!(trimmed.in_frame, 41);
    // Back to where it was: nothing to send.
    let zero = trim_preview(&state, &view, &id("c2d0"), TrimEdge::Start, 0.4, true, 2437).unwrap();
    assert_eq!((zero.d, zero.command), (0, None));
    // Clamped to the take: c2d0 starts at take frame 12.
    let left = trim_preview(
        &state,
        &view,
        &id("c2d0"),
        TrimEdge::Start,
        -500.0,
        false,
        2437,
    )
    .unwrap();
    assert_eq!(left.d, -12);
    assert_eq!(left.tag, "+12 f");
    assert_eq!(left.ghost, None, "nothing left before take frame 0");
    assert!(left.extension.is_some());
    // An end edge extended past the take: clamped at its duration.
    let out = trim_preview(
        &state,
        &view,
        &id("c1b6"),
        TrimEdge::End,
        10_000.0,
        false,
        2437,
    )
    .unwrap();
    assert_eq!(out.d, 0, "c1b6 already ends at take 3's last frame");
    let shorter = trim_preview(
        &state,
        &view,
        &id("c1b6"),
        TrimEdge::End,
        -16.0,
        false,
        2437,
    )
    .unwrap();
    assert_eq!(shorter.d, -10);
    assert_eq!(shorter.trim_frame, (TakeId::numbered(3), 1366));
    assert_eq!(
        shorter.ripple.map(|r| r.text),
        Some("\u{2190} 00:00:00:10".to_owned())
    );
    insta::assert_json_snapshot!("s10_trim", p);
}

/// The edge snaps to the playhead within 8 px and flashes the guide.
#[test]
fn trims_snap_to_the_playhead() {
    let state = zoomed(1.6, 1950);
    let view = Viewport::from_state(&state, W).unwrap();
    // c2d0 starts at 2084; the playhead at 2437 is 564.8 px away.
    let near = trim_preview(
        &state,
        &view,
        &id("c2d0"),
        TrimEdge::Start,
        560.0,
        true,
        2437,
    )
    .unwrap();
    assert_eq!(near.d, 353);
    assert_eq!(near.snapped_x, Some(view.x_of_frame(2437)));
    let off = trim_preview(
        &state,
        &view,
        &id("c2d0"),
        TrimEdge::Start,
        560.0,
        false,
        2437,
    )
    .unwrap();
    assert_eq!((off.d, off.snapped_x), (350, None));
    // Small trims never snap back to where the edge was.
    let small = trim_preview(&state, &view, &id("c2d0"), TrimEdge::Start, 3.2, true, 2437).unwrap();
    assert_eq!(small.d, 2);
}

/// S11: c8a1 dragged after c1b6 at fit.
#[test]
fn the_s11_move() {
    let state = testkit::design_state();
    let view = Viewport::from_state(&state, W).unwrap();
    let p = move_preview(&state, &view, &[id("c8a1")], 405.0 - HEADER_WIDTH).unwrap();
    assert_eq!(p.slot, 6);
    assert_eq!(
        p.tooltip,
        [
            "Move after c1b6 \u{00b7} slot 7 of 10".to_owned(),
            "Starts at 00:00:54:07".to_owned()
        ]
    );
    assert_eq!(p.starts_at, 1627);
    assert!((p.caret_x - 1627.0 * view.px_per_frame).abs() < 1e-9);
    assert_eq!(
        p.command,
        Some(Command::ClipMove {
            clips: vec![id("c8a1")],
            to: MoveTo::Index(6),
        })
    );
    // The others closed c8a1's place and left its new one open: c2d0
    // stays where it is.
    let c2d0 = p.boxes.iter().find(|b| b.clip == id("c2d0")).unwrap();
    assert!((c2d0.x - 2084.0 * view.px_per_frame).abs() < 1e-9);
    let c1b2 = p.boxes.iter().find(|b| b.clip == id("c1b2")).unwrap();
    assert!((c1b2.x - 410.0 * view.px_per_frame).abs() < 1e-9);
    let ripple = p.ripple.clone().unwrap();
    assert_eq!(ripple.text, "\u{2190} 00:00:15:07");
    let TagAnchor::Center { x } = ripple.anchor else {
        panic!("{ripple:?}")
    };
    assert!((x - (410.0 + 1627.0) / 2.0 * view.px_per_frame).abs() < 1e-9);
    // To the start, and back where it was.
    let start = move_preview(&state, &view, &[id("c8a1")], -50.0).unwrap();
    assert_eq!(start.tooltip[0], "Move to the start \u{00b7} slot 1 of 10");
    let home = move_preview(&state, &view, &[id("c8a1")], 58.0).unwrap();
    assert_eq!((home.slot, home.command), (1, None));
    assert_eq!(home.ripple, None);
    insta::assert_json_snapshot!("s11_move", p);
}

#[test]
fn bin_drops() {
    let state = testkit::design_state();
    let clips = &state.project.timeline.clips;
    let view = Viewport::from_state(&state, W).unwrap();
    assert_eq!(drop_slot(&view, clips, -10.0), 0);
    assert_eq!(drop_slot(&view, clips, 1000.0), 10);
    let c2d0_start = boundary_x(&view, clips, 7);
    assert_eq!(drop_slot(&view, clips, c2d0_start + 3.0), 7);
    assert_eq!(
        drop_tooltip(&TakeId::numbered(4), 7, 10),
        "Insert take 4 · slot 8 of 11"
    );
    assert_eq!(
        drop_command(&TakeId::numbered(4), 7),
        Command::ClipAdd {
            take: TakeId::numbered(4),
            at: InsertAt::Index(7),
        }
    );
}

// ------------------------------------------------------------- hit test

/// D-45 at S09's c2d0 | c2d1 cut (x 512): each clip's 4 px; a selected
/// edge takes both; clips under 6 px have no zones.
#[test]
fn trim_zones() {
    let mut state = zoomed(32.0, 3160);
    let view = Viewport::from_state(&state, W).unwrap();
    let boxes = clip_boxes(&state, &view, None);
    assert_eq!(hit(&boxes, 509.0), Hit::TrimEnd(id("c2d0")));
    assert_eq!(hit(&boxes, 512.0), Hit::TrimStart(id("c2d1")));
    assert_eq!(hit(&boxes, 515.9), Hit::TrimStart(id("c2d1")));
    assert_eq!(hit(&boxes, 516.0), Hit::Body(id("c2d1")));
    assert_eq!(hit(&boxes, 507.9), Hit::Body(id("c2d0")));
    state.ui.selection = vec![id("c2d1")];
    let boxes = clip_boxes(&state, &view, None);
    assert_eq!(hit(&boxes, 508.0), Hit::TrimStart(id("c2d1")));
    assert_eq!(hit(&boxes, 507.9), Hit::Body(id("c2d0")));
    state.ui.selection = vec![id("c2d0"), id("c2d1")];
    let boxes = clip_boxes(&state, &view, None);
    assert_eq!(
        hit(&boxes, 509.0),
        Hit::TrimEnd(id("c2d0")),
        "both: 4 px each"
    );
    assert_eq!(hit(&boxes, 512.0), Hit::TrimStart(id("c2d1")));

    // At fit: the timeline's end, and the empty row past it.
    let fit_state = testkit::design_state();
    let fit_view = Viewport::from_state(&fit_state, W).unwrap();
    let fit_boxes = clip_boxes(&fit_state, &fit_view, None);
    let end = W - FIT_INSET_PX;
    assert_eq!(hit(&fit_boxes, end - 1.0), Hit::TrimEnd(id("c3e9")));
    assert_eq!(hit(&fit_boxes, end + 1.0), Hit::Empty);
    let mut selected = fit_state.clone();
    selected.ui.selection = vec![id("c3e9")];
    let sel_boxes = clip_boxes(&selected, &fit_view, None);
    assert_eq!(hit(&sel_boxes, end + 1.0), Hit::TrimEnd(id("c3e9")));

    // A 5 px clip: body only, and it lends its neighbours nothing.
    let mut narrow = testkit::design_state();
    narrow.project.timeline.clips[1].out = 455 + 36;
    narrow.ui.selection = vec![id("c7f3")];
    let narrow_view = Viewport::from_state(&narrow, W).unwrap();
    let nb = clip_boxes(&narrow, &narrow_view, None);
    assert!(nb[1].w < TRIM_ZONE_MIN_CLIP_PX);
    let c8a1_mid = nb[1].x + nb[1].w / 2.0;
    assert_eq!(hit(&nb, c8a1_mid), Hit::Body(id("c8a1")));
    assert_eq!(hit(&nb, nb[1].x + 1.0), Hit::Body(id("c8a1")));
    assert_eq!(hit(&nb, nb[1].x - 1.0), Hit::TrimEnd(id("c7f3")));
}

// ------------------------------------------------------------- lanes

#[test]
fn lanes_and_headers() {
    let mut state = testkit::design_state();
    let lanes = lanes(&state);
    assert_eq!(
        lanes
            .iter()
            .map(|l| (l.track.as_str(), l.label.as_str(), l.autocut, l.dimmed))
            .collect::<Vec<_>>(),
        [
            ("A1", "Yeti Nano", true, false),
            ("A2", "System audio", false, false)
        ]
    );
    state.ui.lane_mute.insert("A2".parse().unwrap());
    state.ui.solo = Some("A1".parse().unwrap());
    let headers = lane_headers(&state);
    assert!(headers[0].lane.solo && !headers[0].lane.dimmed);
    assert!(headers[1].lane.muted && headers[1].lane.dimmed);
    assert_eq!(
        headers[0].mute_tooltip,
        "Mute A1 while playing (export unaffected)"
    );
    // A1's M mutes it too; A2's M unmutes A2; A1's S clears the solo.
    let Command::SetUi { patch } = &headers[0].mute else {
        panic!()
    };
    assert_eq!(patch.lane_mute.as_ref().map(|m| m.len()), Some(2));
    let Command::SetUi { patch } = &headers[1].mute else {
        panic!()
    };
    assert_eq!(patch.lane_mute.as_ref().map(|m| m.len()), Some(0));
    let Command::SetUi { patch } = &headers[0].solo else {
        panic!()
    };
    assert_eq!(patch.solo, Nullable::Clear);
    assert_eq!(
        video_header(&state),
        VideoHeader {
            title: "Video".into(),
            caption: Some("Thumbnails: Face (L2)".into())
        }
    );
    // A track only a take has comes after the project's, by id.
    let mut extra = testkit::design_state();
    let take = extra.project.takes.get_mut(&TakeId::numbered(8)).unwrap();
    let mut a3 = take.scene.audio[0].clone();
    a3.id = "A3".parse().unwrap();
    a3.label = "Lav".into();
    take.scene.audio.push(a3);
    assert_eq!(
        super::lanes(&extra)
            .iter()
            .map(|l| l.track.as_str())
            .collect::<Vec<_>>(),
        ["A1", "A2", "A3"]
    );
}

#[test]
fn lane_segments_map_file_time() {
    let state = zoomed(1.6, 1950);
    let view = Viewport::from_state(&state, W).unwrap();
    let boxes = clip_boxes(&state, &view, None);
    let lane = &lanes(&state)[0];
    let segs = lane_segments(&state, &view, &boxes, lane);
    let c2d0 = &segs[7];
    assert_eq!(c2d0.file.as_deref(), Some("takes/take-0005/A1-mic.mka"));
    assert!(
        (c2d0.file_ms_at_x - 400.0).abs() < 1e-9,
        "take frame 12 = 400 ms"
    );
    assert!((c2d0.ms_per_px - 1000.0 / 30.0 / 1.6).abs() < 1e-9);
    assert_eq!(c2d0.color, format::take_color(&TakeId::numbered(5)));
}

/// S08's two silences of c2d0, from the analysis cache.
#[test]
fn silence_spans_from_the_cache() {
    let state = zoomed(1.6, 1950);
    let files = silence_files(&state);
    assert!(files.contains(&"takes/take-0005/A1-mic.mka".to_owned()));
    let fact = FileFact {
        path: "takes/take-0005/A1-mic.mka".into(),
        bytes: 1234,
        mtime_ns: 99,
    };
    let key = SilenceKey::compute(&fact.path, fact.bytes, fact.mtime_ns, -35.0, 400);
    let facts = BTreeMap::from([(fact.path.clone(), fact.clone())]);
    let ms = |a: i64, b: i64| Interval {
        start_ns: a * NS_PER_MS,
        end_ns: b * NS_PER_MS,
    };
    let cached = BTreeMap::from([(key.clone(), vec![ms(4_000, 5_267), ms(9_600, 10_100)])]);
    let spans = silence_spans(&state, &facts, &cached);
    let view = Viewport::from_state(&state, W).unwrap();
    let xs: Vec<(f64, f64)> = spans
        .iter()
        .map(|s| {
            (
                view.x_of_frame(s.from) + HEADER_WIDTH,
                view.x_of_frame(s.to) + HEADER_WIDTH,
            )
        })
        .collect();
    assert_eq!(
        spans.iter().map(|s| (s.from, s.to)).collect::<Vec<_>>(),
        [(2192, 2230), (2360, 2375)]
    );
    assert!((xs[0].0 - 563.2).abs() < 1e-6 && (xs[0].1 - 624.0).abs() < 1e-6);
    assert!((xs[1].0 - 832.0).abs() < 1e-6 && (xs[1].1 - 856.0).abs() < 1e-6);
    assert!(
        spans
            .iter()
            .all(|s| s.track.as_str() == "A1" && s.clip == id("c2d0"))
    );
    // The runtime's analyses count too; a stale key shows nothing.
    let mut live = state.clone();
    live.runtime.silence.insert(key, vec![ms(4_000, 5_267)]);
    assert_eq!(silence_spans(&live, &facts, &BTreeMap::new()).len(), 1);
    let stale = BTreeMap::from([(fact.path.clone(), FileFact { bytes: 1, ..fact })]);
    assert!(silence_spans(&state, &stale, &cached).is_empty());
}

// ------------------------------------------------------------- thumbnails

#[test]
fn thumbnails() {
    let state = testkit::design_state();
    let view = Viewport::from_state(&state, W).unwrap();
    let boxes = clip_boxes(&state, &view, None);
    let c3e9 = boxes.last().unwrap();
    let tiles = thumb_tiles(&state, &view, c3e9);
    assert_eq!(tiles[0].x, c3e9.x);
    assert_eq!(
        tiles[0].image,
        ThumbImage::Strip {
            file: "takes/take-0008/L2-camera-0.mkv".into(),
            second: 0
        }
    );
    // Each tile at the take time of its left edge, nearest second.
    let ThumbImage::Strip { second, .. } = &tiles[1].image else {
        panic!()
    };
    assert_eq!(
        *second,
        (128.0 / view.px_per_frame / 30.0_f64).round() as u32
    );
    assert!(tiles.iter().all(|t| t.w <= TILE_PX));
    let mut frame = zoomed(32.0, 3160);
    frame.ui.selection = vec![id("c2d1")];
    let fview = Viewport::from_state(&frame, W).unwrap();
    let c2d1 = clip_boxes(&frame, &fview, None)[8].clone();
    let ftiles = thumb_tiles(&frame, &fview, &c2d1);
    assert_eq!(
        ftiles.len(),
        17,
        "frames 3176 to 3192 (window x 688 to 1216)"
    );
    assert_eq!(
        ftiles[0].image,
        ThumbImage::Frame {
            file: "takes/take-0005/L2-camera-0.mkv".into(),
            recorded: 1104,
            second: 37
        }
    );
    assert!(ftiles.iter().all(|t| t.w == 32.0));
    // A take whose thumbnail layer is missing: hatch, no tiles.
    let mut missing = testkit::design_state();
    let take = missing.project.takes.get_mut(&TakeId::numbered(5)).unwrap();
    take.scene.layers[1].missing = true;
    let mboxes = clip_boxes(&missing, &view, None);
    assert!(mboxes[7].missing && !mboxes[6].missing);
    assert!(thumb_tiles(&missing, &view, &mboxes[7]).is_empty());
}

/// G§4.5 (review): the thumbnail layer is the take's topmost visible one
/// whatever its media; without media there is no fallback to a lower
/// layer (the clip is hatched as missing, DESIGN's "thumbnails replaced by
/// hatch"). A hidden top layer hands over to the one below it.
#[test]
fn the_thumbnail_layer_never_falls_back_to_a_lower_one() {
    let mut missing = testkit::design_state();
    let take = missing.project.takes.get_mut(&TakeId::numbered(5)).unwrap();
    take.scene.layers[1].missing = true;
    let take = &missing.project.takes[&TakeId::numbered(5)];
    assert_eq!(thumbnail_layer(take).map(|l| l.id.as_str()), Some("L2"));
    assert_eq!(thumbnail_stream(take), None, "no L1 frames instead");
    let mut hidden = testkit::design_state();
    let take = hidden.project.takes.get_mut(&TakeId::numbered(5)).unwrap();
    take.scene.layers[1].visible = false;
    let view = Viewport::from_state(&hidden, W).unwrap();
    let c2d0 = clip_boxes(&hidden, &view, None)[7].clone();
    assert_eq!(c2d0.clip, id("c2d0"));
    assert!(!c2d0.missing);
    let tiles = thumb_tiles(&hidden, &view, &c2d0);
    assert!(!tiles.is_empty());
    assert!(
        tiles.iter().all(|t| matches!(&t.image,
            ThumbImage::Strip { file, .. } if file.starts_with("takes/take-0005/L1-"))),
        "{tiles:?}"
    );
}

// ------------------------------------------------------------- tooltip

#[test]
fn clip_tooltips() {
    let state = testkit::design_state();
    let keymap = keymap::resolve("");
    let tip = clip_tooltip(&state, &id("c2d1"), &keymap).unwrap();
    assert_eq!(tip.title, "Take 5 \u{00b7} c2d1");
    assert_eq!(
        tip.times,
        "In 00:00:36:24 \u{00b7} Out 00:01:13:24 \u{00b7} 00:00:37:00"
    );
    let keys: Vec<(Vec<Vec<String>>, &str)> = tip
        .keys
        .iter()
        .map(|i| (i.keys.clone(), i.label.as_str()))
        .collect();
    assert_eq!(
        keys,
        [
            (vec![vec!["S".to_owned()]], "Split"),
            (vec![vec!["[".to_owned()], vec!["]".to_owned()]], "Trim"),
            (vec![vec!["X".to_owned()]], "Remove"),
        ]
    );
    let c1b6 = clip_tooltip(&state, &id("c1b6"), &keymap).unwrap();
    assert_eq!(c1b6.title, "Take 3 \u{00b7} c1b6");
    assert_eq!(
        c1b6.times,
        "In 00:00:40:22 \u{00b7} Out 00:00:45:27 \u{00b7} 00:00:05:05"
    );
    // Rebound keys show.
    let rebound = keymap::resolve("[bindings]\n\"clip.split\" = \"w\"\n");
    let tip = clip_tooltip(&state, &id("c2d1"), &rebound).unwrap();
    assert_eq!(tip.keys[0].keys, vec![vec!["W".to_owned()]]);
    assert_eq!(clip_tooltip(&state, &id("cfff"), &keymap), None);
}

// ------------------------------------------------------------- toolbar

#[test]
fn toolbar_variants() {
    let state = testkit::design_state();
    let view = Viewport::from_state(&state, W).unwrap();
    let normal = toolbar(&state, Some(&view), 7391);
    assert_eq!(normal.variant, ToolbarVariant::Normal);
    assert_eq!(normal.bin_count, Some(5));
    assert_eq!(
        normal.summary,
        ("10 clips \u{00b7} ".to_owned(), "00:04:06:11".to_owned())
    );
    assert!(normal.autocut.is_ok() && normal.autocut_settings);
    assert!(normal.zoom && normal.zoom_in && !normal.zoom_out && normal.fit);
    assert_eq!(normal.readout, "Fit");

    let empty_project = testkit::ProjectBuilder::new()
        .screen_layer("L1", "DP-1")
        .mic("A1", "mic")
        .build();
    let empty = testkit::state(empty_project);
    let e = toolbar(&empty, None, 0);
    assert_eq!(e.variant, ToolbarVariant::Empty);
    assert_eq!(e.bin_count, None);
    assert_eq!(e.summary, ("No clips yet".to_owned(), String::new()));
    assert!(e.autocut.is_err() && !e.autocut_settings && !e.zoom && !e.fit);

    let rec = recording(RecordMode::Append, 4842);
    let rview = Viewport::from_state(&rec, W).unwrap();
    let r = toolbar(&rec, Some(&rview), 12_233);
    assert_eq!(r.variant, ToolbarVariant::Recording);
    assert_eq!(r.autocut, Err("Not while recording".to_owned()));
    assert!(!r.autocut_settings && r.zoom);
    assert_eq!(edits_enabled(&rec), Err("Not while recording".to_owned()));
    assert_eq!(edits_enabled(&state), Ok(()));
    insta::assert_json_snapshot!("toolbars", [normal, e, r]);
}

#[test]
fn analyzing_and_dimmed_clips() {
    let mut state = testkit::design_state();
    let c3e9 = state.project.timeline.clips[9].clone();
    state.runtime.jobs.insert(
        JobId(1),
        Job {
            id: JobId(1),
            kind: JobKind::Autocut {
                cmd: serde_json::json!({}),
                targets: vec![AutocutTarget {
                    clip: ClipRef::of(&c3e9),
                    track: "A1".parse().unwrap(),
                    key: SilenceKey("k".into()),
                }],
                params: AutocutParams {
                    threshold_db: -35.0,
                    min_silence_ms: 400,
                    padding_ms: 120,
                    min_keep_ms: 200,
                },
                dry_run: false,
                pending: Default::default(),
                ready: false,
            },
            req: None,
            started: chrono::DateTime::parse_from_rfc3339("2026-10-14T15:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        },
    );
    let view = Viewport::from_state(&state, W).unwrap();
    let hovered = id("c1b2");
    let boxes = clip_boxes(&state, &view, Some(&hovered));
    assert!(boxes[9].analyzing && !boxes[8].analyzing);
    assert!(boxes[2].hovered && !boxes[3].hovered);
    state.runtime.record = RecordPhase::Countdown {
        remaining: 3,
        mode: RecordMode::Append,
        timer: crate::TimerId(1),
        start_req: None,
        wait: false,
    };
    assert_eq!(clip_opacity(&state), 0.7);
    let _ = SelectOp::Clear;
}

#[test]
fn the_empty_state_has_the_record_key() {
    let empty = empty_state(&keymap::resolve(""));
    assert_eq!(
        (
            empty.title.as_str(),
            empty.before.as_str(),
            empty.after.as_str()
        ),
        ("This swamp is quiet.", "Press", "and make some noise.")
    );
    assert_eq!(empty.key, Some(vec!["R".to_owned()]));
    let f9 = empty_state(&keymap::resolve("[bindings]\n\"record.toggle\" = \"F9\"\n"));
    assert_eq!(f9.key, Some(vec!["F9".to_owned()]));
}

/// `record.toggle` unbound, or in a keys.toml conflict (neither binding
/// runs): no cap, and a line that needs none, as the hint bar leaves the
/// Record item out.
#[test]
fn the_empty_state_without_a_record_key_names_the_button() {
    let state = testkit::design_state();
    for toml in [
        "",
        "[bindings]\n\"record.toggle\" = []\n",
        "[bindings]\n\"record.over\" = \"r\"\n",
        "[bindings]\n\"record.toggle\" = \"F9\"\n",
    ] {
        let map = keymap::resolve(toml);
        let empty = empty_state(&map);
        let (items, _) = crate::view::hints::hint_bar(
            &crate::view::hints::HintState::Empty,
            &state,
            &crate::view::GuiFacts::default(),
            &map,
        );
        let record = items.iter().any(|i| i.label == "Record");
        assert_eq!(empty.key.is_some(), record, "{toml:?}: {empty:?}");
        let line = match &empty.key {
            Some(key) => format!("{} [{}] {}", empty.before, key.join("+"), empty.after),
            None => format!("{} {}", empty.before, empty.after),
        };
        let want = if record {
            format!(
                "Press [{}] and make some noise.",
                empty.key.clone().unwrap().join("+")
            )
        } else {
            "Click Record and make some noise.".to_owned()
        };
        assert_eq!(line, want, "{toml:?}");
    }
}

#[test]
fn compact_rows_fit_232_px() {
    let rows = COMPACT_ROWS;
    assert_eq!(
        rows.toolbar + rows.ruler + rows.content_height(2),
        232.0,
        "DESIGN Layout's minimum"
    );
    assert_eq!(ROWS.lane_y(0), 80.0);
    assert_eq!(ROWS.lane_y(1), 140.0);
    assert_eq!(ROWS.content_height(2), 196.0);
    assert_eq!(Rows::for_layout(true), COMPACT_ROWS);
}
