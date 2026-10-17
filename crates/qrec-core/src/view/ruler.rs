//! The timeline ruler (G§4.5, C15): label interval, ticks, labels, the
//! frame-level cells and the corner timecode. Owner: WP3.
//!
//! - The label interval L is the smallest of 1f, 2f, 5f, 10f, 15f, 1s, 2s,
//!   5s, 10s, 15s, 30s, 1min, 2min, 5min, 10min, 15min, 30min, 1h that is
//!   at least 90 px wide; L picks the major and minor ticks ([`tick_steps`]).
//!   Minor ticks closer than 6 px are hidden.
//! - Labels are `HH:MM:SS:FF` 4 px right of their tick and show while
//!   their tick shows: cut at the right edge, dropped at the left (S07, S08,
//!   S12; G§4.5). A tick left of the view never has a label:
//!
//!   ```
//!   use qrec_core::Fps;
//!   use qrec_core::view::ruler::{self, TickKind};
//!   use qrec_core::view::timeline::Viewport;
//!
//!   // S08: 48 px/s from 00:01:05:00; 00:01:04:00's major is 48 px left.
//!   let view = Viewport { width: 1040.0, px_per_frame: 1.6, scroll_px: 1950.0 * 1.6, lanes_scroll_px: 0.0 };
//!   let ticks = ruler::ticks(&view, Fps::new(30, 1), 7391);
//!   assert!(ticks.iter().any(|t| t.x < 0.0 && t.kind == TickKind::Major));
//!   assert!(ticks.iter().filter(|t| t.x < 0.0).all(|t| t.label.is_none()));
//!   ```
//! - Frame level (≥ 24 px per frame): a tick per frame with its `FF`
//!   centered in the cell, and at each second a tall tick with the full
//!   timecode; the playhead's cell gets a band ([`current_cell`]).
//!
//! Seconds count at the nominal rate, like timecodes (`time::timecode`).

use serde::{Deserialize, Serialize};

use crate::ids::Frame;
use crate::model::Fps;
use crate::view::format;
use crate::view::timeline::Viewport;

/// Labels are at least this far apart.
pub const LABEL_MIN_PX: f64 = 90.0;
/// Minor ticks closer than this are hidden.
pub const MINOR_MIN_PX: f64 = 6.0;
/// How far left of the view ticks are still listed, unlabeled: nothing it
/// adds shows (a label left of the view is dropped, and the frame cell the
/// left edge cuts is listed anyway: the first frame is the floor of the
/// time at the edge).
pub const LABEL_REACH_PX: f64 = 84.0;

/// How far apart labels are: the smallest of 1f, 2f, 5f, 10f, 15f, 1s,
/// 2s, 5s, 10s, 15s, 30s, 1min, 2min, 5min, 10min, 15min, 30min, 1h that
/// is at least 90 px wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "unit", content = "n", rename_all = "camelCase")]
pub enum LabelInterval {
    Frames(u32),
    Seconds(u32),
    Minutes(u32),
    Hours(u32),
}

/// The candidate intervals, smallest first.
const INTERVALS: [LabelInterval; 18] = [
    LabelInterval::Frames(1),
    LabelInterval::Frames(2),
    LabelInterval::Frames(5),
    LabelInterval::Frames(10),
    LabelInterval::Frames(15),
    LabelInterval::Seconds(1),
    LabelInterval::Seconds(2),
    LabelInterval::Seconds(5),
    LabelInterval::Seconds(10),
    LabelInterval::Seconds(15),
    LabelInterval::Seconds(30),
    LabelInterval::Minutes(1),
    LabelInterval::Minutes(2),
    LabelInterval::Minutes(5),
    LabelInterval::Minutes(10),
    LabelInterval::Minutes(15),
    LabelInterval::Minutes(30),
    LabelInterval::Hours(1),
];

impl LabelInterval {
    /// Its length in frames at `fps` (seconds at the nominal rate).
    pub fn frames(self, fps: Fps) -> Frame {
        let second = Frame::from(fps.nominal().max(1));
        match self {
            LabelInterval::Frames(n) => Frame::from(n),
            LabelInterval::Seconds(n) => Frame::from(n) * second,
            LabelInterval::Minutes(n) => Frame::from(n) * 60 * second,
            LabelInterval::Hours(n) => Frame::from(n) * 3600 * second,
        }
    }
}

/// The label interval at `px_per_frame`.
pub fn label_interval(px_per_frame: f64, fps: Fps) -> LabelInterval {
    INTERVALS
        .iter()
        .copied()
        .find(|i| i.frames(fps) as f64 * px_per_frame >= LABEL_MIN_PX)
        .unwrap_or(LabelInterval::Hours(1))
}

/// The major and minor tick steps for label interval `interval` (G§4.5's
/// table), in frames; `None` without minor ticks.
///
/// From 10 s up to a minute a major is a label's tick and the minors split
/// the interval as the frames do: S12 (10 s) has a 5 px tick at 5 s and no
/// 1 s ticks; S04, S05 and S05b (1 min) have 5 px ticks every 10 s and none
/// taller at 30 s.
///
/// The 1 s and 2 s intervals' frame minors divide the nominal second
/// ([`second_minor`]): 5 f and 10 f at 30, 50 and 60 fps, 4 f and 8 f at
/// 24, 5 f at 25.
pub fn tick_steps(interval: LabelInterval, fps: Fps) -> (Frame, Option<Frame>) {
    use LabelInterval::*;
    let s = |n: u32| Seconds(n).frames(fps);
    let m = |n: u32| Minutes(n).frames(fps);
    match interval {
        Frames(n) => (Frame::from(n), Some(1)),
        Seconds(1) => (s(1), Some(second_minor(fps, 5))),
        Seconds(2) => (s(1), Some(second_minor(fps, 10))),
        Seconds(5) => (s(1), None),
        Seconds(n @ (10 | 15)) => (s(n), Some(s(5))),
        Seconds(_) => (s(30), Some(s(10))),
        Minutes(1) => (m(1), Some(s(10))),
        Minutes(2) | Minutes(5) => (m(1), Some(s(30))),
        Minutes(_) | Hours(_) => (m(5), Some(m(1))),
    }
}

/// A frame minor of at most `most` frames that splits the nominal second
/// evenly (its largest divisor up to `most`), so the minors land on each
/// second's major: 5 f in a 30 f second, 4 f in a 24 f one, 1 f in a 13 f
/// one (12.5 fps).
pub fn second_minor(fps: Fps, most: Frame) -> Frame {
    let second = Frame::from(fps.nominal().max(1));
    (1..=most.min(second))
        .rev()
        .find(|m| second.is_multiple_of(*m))
        .unwrap_or(1)
}

fn gcd(mut a: Frame, mut b: Frame) -> Frame {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// What a tick is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TickKind {
    /// 10 px, with a label.
    Major,
    /// 5 px.
    Minor,
    /// Frame level: 5 px per frame, with its `FF` number centered.
    Frame,
    /// Frame level: 28 px at a second, with the full timecode in `text`.
    Second,
}

/// One tick, x relative to the time area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tick {
    pub x: f64,
    pub kind: TickKind,
    /// The timecode 4 px right of the tick (majors at the label interval,
    /// seconds at frame level).
    pub label: Option<String>,
    /// Frame level: the `FF` centered in the cell right of the tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<String>,
}

/// The ticks visible in `viewport` (the frame cell its left edge cuts
/// included), and a few unlabeled ones left of it ([`LABEL_REACH_PX`]). A
/// tick left of the view has no label: the frames show a label while its
/// tick shows, cut at the right edge, never at the left (S07, S08, S12).
/// `total` is the timeline's length; the ruler runs on past it to the
/// view's edge.
pub fn ticks(viewport: &Viewport, fps: Fps, total: Frame) -> Vec<Tick> {
    let _ = total;
    let ppf = viewport.px_per_frame;
    if !(ppf.is_finite() && ppf > 0.0) {
        return Vec::new();
    }
    let nominal = Frame::from(fps.nominal().max(1));
    let first = viewport.time_at_x(-LABEL_REACH_PX).floor().max(0.0) as Frame;
    let last = viewport.time_at_x(viewport.width).floor().max(0.0) as Frame;
    let mut out = Vec::new();
    if viewport.is_frame_level() {
        for f in first..=last {
            let ff = format!("{:02}", f % nominal);
            let second = f % nominal == 0;
            let x = viewport.x_of_frame(f);
            out.push(Tick {
                x,
                kind: if second {
                    TickKind::Second
                } else {
                    TickKind::Frame
                },
                label: (second && x >= 0.0).then(|| format::timecode(f, fps)),
                cell: Some(ff),
            });
        }
        return out;
    }
    let interval = label_interval(ppf, fps);
    let label_every = interval.frames(fps);
    let (major, minor) = tick_steps(interval, fps);
    let minor = minor.filter(|m| *m as f64 * ppf >= MINOR_MIN_PX);
    // Every major and every shown minor, whether or not one divides the
    // other: stepping by their gcd visits both grids (a label's frame is a
    // major's).
    let step = minor.map_or(major, |m| gcd(major, m)).max(1);
    let mut f = first.div_ceil(step) * step;
    while f <= last {
        let is_major = f.is_multiple_of(major);
        if is_major || minor.is_some_and(|m| f.is_multiple_of(m)) {
            let x = viewport.x_of_frame(f);
            out.push(Tick {
                x,
                kind: if is_major {
                    TickKind::Major
                } else {
                    TickKind::Minor
                },
                label: (f.is_multiple_of(label_every) && x >= 0.0)
                    .then(|| format::timecode(f, fps)),
                cell: None,
            });
        }
        f += step;
    }
    out
}

/// The corner cell: the playhead's timecode.
pub fn corner(playhead: Frame, fps: Fps) -> String {
    format::timecode(playhead, fps)
}

/// At frame level, the playhead's cell (x, width) for its `accent-2-20`
/// band; `None` otherwise.
pub fn current_cell(viewport: &Viewport, playhead: Frame) -> Option<(f64, f64)> {
    viewport
        .is_frame_level()
        .then(|| (viewport.x_of_frame(playhead), viewport.px_per_frame))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::timeline::Viewport;

    fn fps30() -> Fps {
        Fps::new(30, 1)
    }

    fn at(ppf: f64, scroll_px: f64) -> Viewport {
        Viewport {
            width: 1040.0,
            px_per_frame: ppf,
            scroll_px,
            lanes_scroll_px: 0.0,
        }
    }

    /// G§4.5's checks against the mocks: fit 0.1396 → 30 s / 30 s / 10 s;
    /// 48 px/s → 2 s / 1 s / 10 f; 10 px/s → 10 s; recording fit 0.0850 →
    /// 1 min.
    #[test]
    fn the_intervals_match_the_mocks() {
        let fps = fps30();
        let fit = 1032.0 / 7391.0;
        assert_eq!(label_interval(fit, fps), LabelInterval::Seconds(30));
        assert_eq!(
            tick_steps(LabelInterval::Seconds(30), fps),
            (900, Some(300))
        );
        assert_eq!(label_interval(1.6, fps), LabelInterval::Seconds(2));
        assert_eq!(tick_steps(LabelInterval::Seconds(2), fps), (30, Some(10)));
        assert_eq!(
            label_interval(1040.0 / 3105.0, fps),
            LabelInterval::Seconds(10)
        );
        assert_eq!(label_interval(0.0850, fps), LabelInterval::Minutes(1));
        assert_eq!(
            label_interval(1032.0 / 12_233.0, fps),
            LabelInterval::Minutes(1)
        );
        // The ends of the table.
        assert_eq!(label_interval(128.0, fps), LabelInterval::Frames(1));
        assert_eq!(label_interval(0.000_1, fps), LabelInterval::Hours(1));
        assert_eq!(
            tick_steps(LabelInterval::Minutes(10), fps),
            (9000, Some(1800))
        );
        assert_eq!(tick_steps(LabelInterval::Seconds(5), fps), (30, None));
    }

    /// S08 (48 px/s from 00:01:05:00): labels every 2 s at x 48, 144, …
    /// (window 224, 320, …), majors every second, minors every 10 frames;
    /// none for 00:01:04:00, whose tick is left of the view, and the last
    /// one cut by the right edge.
    #[test]
    fn seconds_zoom_ticks() {
        let view = at(1.6, 1950.0 * 1.6);
        let ticks = ticks(&view, fps30(), 7391);
        let labels: Vec<(f64, &str)> = ticks
            .iter()
            .filter_map(|t| t.label.as_deref().map(|l| (t.x + 176.0, l)))
            .collect();
        assert_eq!(labels[0], (224.0, "00:01:06:00"));
        assert_eq!(labels[1], (320.0, "00:01:08:00"));
        assert_eq!(labels.last().unwrap(), &(1184.0, "00:01:26:00"));
        assert!(ticks.iter().any(|t| t.x < 0.0 && t.kind == TickKind::Major));
        let majors = ticks.iter().filter(|t| t.kind == TickKind::Major).count();
        let minors = ticks.iter().filter(|t| t.kind == TickKind::Minor).count();
        assert_eq!((majors, minors), (23, 48));
        assert!(
            ticks
                .windows(2)
                .all(|w| ((w[1].x - w[0].x) - 16.0).abs() < 1e-9)
        );
    }

    /// S09 (32 px/frame from frame 3160): a cell per frame with its `FF`,
    /// `00:01:46:00` in `text` at x 816 (window), the playhead's cell at 688.
    #[test]
    fn frame_level_cells() {
        let view = at(32.0, 3160.0 * 32.0);
        let ticks = ticks(&view, fps30(), 7391);
        let visible: Vec<&Tick> = ticks.iter().filter(|t| t.x >= 0.0).collect();
        assert_eq!(visible[0].cell.as_deref(), Some("10"));
        assert_eq!(visible[0].kind, TickKind::Frame);
        let second = visible.iter().find(|t| t.kind == TickKind::Second).unwrap();
        assert_eq!(second.x + 176.0, 816.0);
        assert_eq!(second.label.as_deref(), Some("00:01:46:00"));
        assert_eq!(second.cell.as_deref(), Some("00"));
        let cells: Vec<&str> = visible.iter().filter_map(|t| t.cell.as_deref()).collect();
        assert_eq!(&cells[..3], ["10", "11", "12"]);
        assert_eq!(cells.last(), Some(&"12"));
        assert_eq!(current_cell(&view, 3176), Some((512.0, 32.0)));
        assert_eq!(current_cell(&at(1.6, 0.0), 3176), None);
    }

    #[test]
    fn fit_ticks_and_the_corner() {
        let view = at(1032.0 / 7391.0, 0.0);
        let ticks = ticks(&view, fps30(), 7391);
        let labels: Vec<&str> = ticks.iter().filter_map(|t| t.label.as_deref()).collect();
        assert_eq!(
            labels,
            [
                "00:00:00:00",
                "00:00:30:00",
                "00:01:00:00",
                "00:01:30:00",
                "00:02:00:00",
                "00:02:30:00",
                "00:03:00:00",
                "00:03:30:00",
                "00:04:00:00"
            ]
        );
        // Majors every 30 s, minors every 10 s (41.9 px apart).
        assert!(ticks.iter().all(|t| t.cell.is_none()));
        assert_eq!(
            ticks.iter().filter(|t| t.kind == TickKind::Minor).count(),
            16
        );
        assert_eq!(corner(2437, fps30()), "00:01:21:07");
        let json = serde_json::to_value(&ticks[0]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"x": 0.0, "kind": "major", "label": "00:00:00:00"})
        );
    }

    /// The frame a tick of `view` stands at.
    fn frame_of(view: &Viewport, tick: &Tick) -> Frame {
        ((tick.x + view.scroll_px) / view.px_per_frame).round() as Frame
    }

    /// S12 (10 px/s, c3e9 from x 0): a 10 px tick at each label (every
    /// 10 s, window x 248, 348, …) and a 5 px tick halfway (5 s, x 197,
    /// 298, …), nothing else: the frame draws no 1 s ticks and no 10 px
    /// tick at 5 s.
    #[test]
    fn s12_ticks_are_the_labels_and_5_s_between() {
        let ppf = 1040.0 / 3105.0;
        let view = at(ppf, 4286.0 * ppf);
        let ticks = ticks(&view, fps30(), 7391);
        assert!(ticks.iter().all(|t| frame_of(&view, t).is_multiple_of(150)));
        for tick in &ticks {
            let at_label = frame_of(&view, tick).is_multiple_of(300);
            assert_eq!(tick.kind == TickKind::Major, at_label, "{tick:?}");
            assert_eq!(tick.label.is_some(), at_label && tick.x >= 0.0, "{tick:?}");
        }
        let window = |kind: TickKind| -> Vec<f64> {
            ticks
                .iter()
                .filter(|t| t.kind == kind && t.x >= 0.0)
                .map(|t| (t.x + 176.0).round())
                .collect()
        };
        assert_eq!(window(TickKind::Major)[..3], [248.0, 348.0, 449.0]);
        assert_eq!(window(TickKind::Minor)[..3], [197.0, 298.0, 398.0]);
        assert_eq!(
            ticks
                .iter()
                .find(|t| t.x >= 0.0 && t.label.is_some())
                .and_then(|t| t.label.as_deref()),
            Some("00:02:30:00")
        );
    }

    /// S04, S05 and S05b (the recording fit, 0.0850 px/frame): a 10 px tick
    /// at each label (every minute, window x 176, 329, 482, …) and 5 px
    /// ticks every 10 s between, none taller at the half minute.
    #[test]
    fn recording_fit_ticks_are_the_labels_and_10_s_between() {
        let view = at(0.0850, 0.0);
        let ticks = ticks(&view, fps30(), 12_000);
        assert!(ticks.iter().all(|t| frame_of(&view, t).is_multiple_of(300)));
        for tick in &ticks {
            let at_label = frame_of(&view, tick).is_multiple_of(1800);
            assert_eq!(tick.kind == TickKind::Major, at_label, "{tick:?}");
            assert_eq!(tick.label.is_some(), at_label, "{tick:?}");
        }
        let majors: Vec<f64> = ticks
            .iter()
            .filter(|t| t.kind == TickKind::Major)
            .map(|t| (t.x + 176.0).round())
            .collect();
        assert_eq!(majors[..4], [176.0, 329.0, 482.0, 635.0]);
    }

    #[test]
    fn minor_ticks_hide_when_crowded() {
        let fps = fps30();
        let ppf = 0.006;
        assert_eq!(label_interval(ppf, fps), LabelInterval::Minutes(10));
        let view = at(ppf, 0.0);
        let ticks = ticks(&view, fps, 300_000);
        // 10 min labels: 5 min majors, 1 min minors 10.8 px apart.
        assert!(ticks.iter().any(|t| t.kind == TickKind::Minor));
        let tight = at(0.0029, 0.0);
        assert_eq!(label_interval(0.0029, fps), LabelInterval::Minutes(30));
        // 1 min minors are 5.2 px apart: hidden.
        assert!(ticks_of(&tight).iter().all(|t| t.kind == TickKind::Major));
    }

    fn ticks_of(view: &Viewport) -> Vec<Tick> {
        ticks(view, fps30(), 0)
    }

    /// 24 fps at 4 px/frame labels every second (11 labels in 1040 px), and
    /// 25 fps at 2 px/frame has its 1 s major at frame 25: the frame minors
    /// (5 f, 10 f at 30 fps) didn't divide those seconds, so stepping by
    /// them skipped the majors and labels between their common multiples.
    #[test]
    fn every_second_shows_at_24_and_25_fps() {
        let labels: Vec<String> = ticks(&at(4.0, 0.0), Fps::new(24, 1), 0)
            .into_iter()
            .filter_map(|t| t.label)
            .collect();
        let want: Vec<String> = (0..=10).map(|s| format!("00:00:{s:02}:00")).collect();
        assert_eq!(labels, want);
        let ticks = ticks(&at(2.0, 0.0), Fps::new(25, 1), 0);
        let at_50 = ticks.iter().find(|t| (t.x - 50.0).abs() < 1e-9);
        assert_eq!(at_50.map(|t| t.kind), Some(TickKind::Major), "{ticks:?}");
        assert_eq!(
            tick_steps(LabelInterval::Seconds(1), Fps::new(24, 1)),
            (24, Some(4))
        );
        assert_eq!(
            tick_steps(LabelInterval::Seconds(2), Fps::new(24, 1)),
            (24, Some(8))
        );
        assert_eq!(
            tick_steps(LabelInterval::Seconds(2), Fps::new(25, 1)),
            (25, Some(5))
        );
    }

    /// At each offered rate (and 23.976, 29.97), from an hour per label to
    /// just under frame level: every multiple of the major in view has a
    /// major tick, every multiple of the label interval in view a label,
    /// and the 1 s and 2 s intervals' frame minors split the second evenly.
    #[test]
    fn majors_and_labels_show_at_every_rate() {
        let rates = crate::view::settings::RATES
            .iter()
            .map(|&n| Fps::new(n, 1))
            .chain([Fps::new(24_000, 1001), Fps::new(30_000, 1001)]);
        for fps in rates {
            let second = Frame::from(fps.nominal());
            let mut ppf = 0.003;
            while ppf < 23.9 {
                for scroll in [0.0, 333.3, 12_345.6] {
                    let view = at(ppf, scroll);
                    let ticks = ticks(&view, fps, 0);
                    let interval = label_interval(ppf, fps);
                    let (major, minor) = tick_steps(interval, fps);
                    let every = interval.frames(fps);
                    let frame = |t: &Tick| ((t.x + scroll) / ppf).round() as Frame;
                    let majors: std::collections::BTreeSet<Frame> = ticks
                        .iter()
                        .filter(|t| t.kind == TickKind::Major)
                        .map(frame)
                        .collect();
                    let labeled: std::collections::BTreeSet<Frame> = ticks
                        .iter()
                        .filter(|t| t.label.is_some())
                        .map(frame)
                        .collect();
                    let first = view.time_at_x(-LABEL_REACH_PX).floor().max(0.0) as Frame;
                    let last = view.time_at_x(view.width).floor().max(0.0) as Frame;
                    let mut f = first.div_ceil(major) * major;
                    while f <= last {
                        let case = format!("{fps:?} at {ppf} px/frame from {scroll}: frame {f}");
                        assert!(majors.contains(&f), "no major: {case}");
                        if f % every == 0 && view.x_of_frame(f) >= 0.0 {
                            assert!(labeled.contains(&f), "no label: {case}");
                        }
                        f += major;
                    }
                    if let (LabelInterval::Seconds(1 | 2), Some(m)) = (interval, minor) {
                        assert_eq!(second % m, 0, "{fps:?}: {m} f minors");
                    }
                }
                ppf *= 1.07;
            }
        }
    }
}
