//! Properties of the timeline's view model (docs/GUI.md G§4.5): boxes tile
//! the timeline, the label rank is monotone in width, frames and x round
//! trip, `zoom_at` keeps the anchor's frame under the anchor, a move
//! preview's command gives the previewed order, a trim preview's command
//! gives the previewed range, `drop_slot` is monotone in x, and the
//! ruler's labels show only in view (dropped at the left edge).

use proptest::prelude::*;
use proptest::sample::Index;
use qrec_core::apply::apply;
use qrec_core::protocol::command::Command;
use qrec_core::testkit::{self, strategies};
use qrec_core::timeline::total_frames;
use qrec_core::view::ruler::{self, TickKind};
use qrec_core::view::timeline::{
    ClipLabel, MAX_PX_PER_FRAME, TrimEdge, Viewport, clamp_zoom, clip_boxes, drop_slot,
    move_preview, trim_preview,
};
use qrec_core::{ClipId, Frame, State, TakeId};

/// A viewport over `state`'s timeline: a width, a zoom `s ∈ [0, 1]` of the
/// way from fit to 128 px per frame (log), and a scroll fraction.
fn viewport(state: &State, width: f64, s: f64, scroll: f64) -> Option<(Viewport, f64, Frame)> {
    let total = total_frames(&state.project.timeline.clips);
    let fit = Viewport::fit(width, total)?;
    let ppf = clamp_zoom(
        fit.px_per_frame * (MAX_PX_PER_FRAME / fit.px_per_frame).max(1.0).powf(s),
        fit.px_per_frame,
    );
    let view = Viewport {
        px_per_frame: ppf,
        scroll_px: 0.0,
        ..fit
    };
    let max = view.max_scroll(total);
    Some((
        Viewport {
            scroll_px: max * scroll,
            ..view
        },
        fit.px_per_frame,
        total,
    ))
}

proptest! {
    #[test]
    fn boxes_tile_the_timeline(
        project in strategies::arb_project(),
        width in 200.0f64..2000.0,
        s in 0.0f64..1.0,
        scroll in 0.0f64..1.0,
    ) {
        let state = testkit::state(project);
        let Some((view, _, _)) = viewport(&state, width, s, scroll) else {
            prop_assert!(state.project.timeline.clips.is_empty());
            return Ok(());
        };
        let boxes = clip_boxes(&state, &view, None);
        prop_assert_eq!(boxes.len(), state.project.timeline.clips.len());
        prop_assert!((boxes[0].x + view.scroll_px).abs() < 1e-6);
        for pair in boxes.windows(2) {
            let tol = 1e-9 * (1.0 + pair[0].x.abs() + pair[0].w);
            prop_assert!((pair[1].x - (pair[0].x + pair[0].w)).abs() <= tol,
                "{} + {} != {}", pair[0].x, pair[0].w, pair[1].x);
        }
    }

    #[test]
    fn labels_rank_with_width(a in 0.0f64..200.0, b in 0.0f64..200.0, n in 1u32..100) {
        let take = TakeId::numbered(n);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        prop_assert!(ClipLabel::for_width(&take, lo).rank() <= ClipLabel::for_width(&take, hi).rank());
    }

    #[test]
    fn frames_and_x_round_trip(
        width in 100.0f64..4000.0,
        total in 1u64..1_000_000,
        s in 0.0f64..1.0,
        scroll in 0.0f64..1.0,
        f in any::<Index>(),
    ) {
        let fit = Viewport::fit(width, total).unwrap();
        let ppf = clamp_zoom(fit.px_per_frame * (MAX_PX_PER_FRAME / fit.px_per_frame).max(1.0).powf(s), fit.px_per_frame);
        let mut view = Viewport { px_per_frame: ppf, ..fit };
        view.scroll_px = view.max_scroll(total) * scroll;
        let f = f.index(total as usize + 1) as Frame;
        prop_assert_eq!(view.frame_at_x(view.x_of_frame(f)), f);
    }

    #[test]
    fn zoom_at_keeps_the_anchor(
        width in 200.0f64..3000.0,
        total in 1u64..500_000,
        s in 0.0f64..1.0,
        scroll in 0.0f64..1.0,
        factor in 0.1f64..10.0,
        anchor in 0.0f64..1.0,
    ) {
        let fit = Viewport::fit(width, total).unwrap();
        let ppf = clamp_zoom(fit.px_per_frame * (MAX_PX_PER_FRAME / fit.px_per_frame).max(1.0).powf(s), fit.px_per_frame);
        let mut view = Viewport { px_per_frame: ppf, ..fit };
        view.scroll_px = view.max_scroll(total) * scroll;
        let x = anchor * width;
        let zoomed = view.zoom_at(factor, x, fit.px_per_frame);
        prop_assert!(zoomed.px_per_frame >= fit.px_per_frame * (1.0 - 1e-12));
        prop_assert!(zoomed.px_per_frame <= MAX_PX_PER_FRAME.max(fit.px_per_frame) * (1.0 + 1e-12));
        let max = zoomed.max_scroll(total);
        prop_assert!(zoomed.scroll_px >= 0.0 && zoomed.scroll_px <= max + 1e-6);
        let at_bound = zoomed.scroll_px <= 1e-9 || (zoomed.scroll_px - max).abs() <= 1e-6;
        let before = view.time_at_x(x);
        let after = zoomed.time_at_x(x);
        prop_assert!(at_bound || (before - after).abs() <= 1e-6 * (1.0 + before.abs()),
            "{before} → {after}");
    }

    #[test]
    fn a_move_previews_what_its_command_does(
        project in strategies::arb_project(),
        pick in any::<Index>(),
        two in any::<bool>(),
        other in any::<Index>(),
        pointer in 0.0f64..1.0,
        s in 0.0f64..1.0,
    ) {
        let state = testkit::state(project);
        let clips = &state.project.timeline.clips;
        prop_assume!(!clips.is_empty());
        let (view, _, _) = viewport(&state, 1040.0, s, 0.0).unwrap();
        let mut block: Vec<ClipId> = vec![clips[pick.index(clips.len())].id.clone()];
        if two {
            block.push(clips[other.index(clips.len())].id.clone());
        }
        let x = pointer * view.width * 1.2 - 0.1 * view.width;
        let preview = move_preview(&state, &view, &block, x).unwrap();
        let ctx = testkit::ctx("2026-10-14T15:00:00Z", 1);
        match &preview.command {
            None => {
                let now: Vec<ClipId> = clips.iter().map(|c| c.id.clone()).collect();
                prop_assert_eq!(&preview.order, &now);
            }
            Some(cmd) => {
                let out = apply(&state, &ctx, cmd.clone()).unwrap();
                let order: Vec<ClipId> = out.state.project.timeline.clips.iter().map(|c| c.id.clone()).collect();
                prop_assert_eq!(&preview.order, &order);
            }
        }
    }

    #[test]
    fn a_trim_previews_what_its_command_does(
        project in strategies::arb_project(),
        pick in any::<Index>(),
        start in any::<bool>(),
        dx in -2000.0f64..2000.0,
        s in 0.0f64..1.0,
    ) {
        let state = testkit::state(project);
        let clips = &state.project.timeline.clips;
        prop_assume!(!clips.is_empty());
        let (view, _, _) = viewport(&state, 1040.0, s, 0.0).unwrap();
        let clip = clips[pick.index(clips.len())].clone();
        let edge = if start { TrimEdge::Start } else { TrimEdge::End };
        let p = trim_preview(&state, &view, &clip.id, edge, dx, false, 0).unwrap();
        let ctx = testkit::ctx("2026-10-14T15:00:00Z", 1);
        match p.command {
            None => prop_assert_eq!(p.d, 0),
            Some(cmd) => {
                let is_trim = matches!(cmd, Command::ClipTrim { .. });
                prop_assert!(is_trim, "not a trim: {:?}", cmd);
                let out = apply(&state, &ctx, cmd).unwrap();
                let after = out.state.project.clip(&clip.id).unwrap();
                match edge {
                    TrimEdge::Start => {
                        prop_assert_eq!(after.in_ as i64, clip.in_ as i64 + p.d);
                        prop_assert_eq!(after.out, clip.out);
                        prop_assert_eq!(p.trim_frame.1, after.in_);
                    }
                    TrimEdge::End => {
                        prop_assert_eq!(after.out as i64, clip.out as i64 + p.d);
                        prop_assert_eq!(after.in_, clip.in_);
                        prop_assert_eq!(p.trim_frame.1, after.out - 1);
                    }
                }
            }
        }
    }

    #[test]
    fn drop_slots_are_monotone_in_x(
        project in strategies::arb_project(),
        a in -200.0f64..1400.0,
        b in -200.0f64..1400.0,
        s in 0.0f64..1.0,
        scroll in 0.0f64..1.0,
    ) {
        let state = testkit::state(project);
        let Some((view, _, _)) = viewport(&state, 1040.0, s, scroll) else {
            return Ok(());
        };
        let clips = &state.project.timeline.clips;
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        prop_assert!(drop_slot(&view, clips, lo) <= drop_slot(&view, clips, hi));
        prop_assert!(drop_slot(&view, clips, hi) <= clips.len());
    }

    /// G§4.5: a label shows while its tick shows, cut at the right edge and
    /// dropped at the left; at frame level the cell the left edge cuts is
    /// there with its number.
    #[test]
    fn ruler_labels_show_only_in_view(
        fps in strategies::arb_fps(),
        ppf in (-4.0f64..2.31).prop_map(|e| 10f64.powf(e)),
        scroll in 0.0f64..1e6,
    ) {
        let view = Viewport {
            width: 1040.0,
            px_per_frame: ppf,
            scroll_px: scroll,
            lanes_scroll_px: 0.0,
        };
        let ticks = ruler::ticks(&view, fps, 0);
        for tick in &ticks {
            if tick.x < 0.0 {
                prop_assert!(tick.label.is_none(), "{tick:?}");
            }
            if tick.label.is_some() {
                prop_assert!(tick.x >= 0.0 && tick.x <= view.width, "{tick:?}");
            }
        }
        if view.is_frame_level() {
            let cut = ticks
                .iter()
                .find(|t| t.x <= 0.0 && 0.0 < t.x + ppf)
                .map(|t| (t.kind, t.cell.is_some()));
            prop_assert!(
                matches!(cut, Some((TickKind::Frame | TickKind::Second, true))),
                "{cut:?}"
            );
        }
    }
}
