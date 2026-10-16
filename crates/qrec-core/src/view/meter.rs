//! The level meter (G§4.7, C11). Owner: WP5.
//!
//! x(db) = clamp((db + 60) / 60, 0, 1) × the bar width; zones −60…−18
//! `meter-green`, −18…−6 `meter-yellow`, −6…0 `meter-red`; 2 px segments
//! with 1 px gaps; a 2 px peak hold falling at 20 dB/s after 1.5 s; a clip
//! latch from −0.1 dBFS until clicked.
//!
//! The bar is the loudest channel's RMS level, the hold its peak and the
//! readout the held peak (`format::peak`: "−12.4", "—" below −60). Muted
//! tracks draw their level and peak at 35 %. A widget feeds every reading
//! to [`MeterState::update_reading`] and draws [`view`] on each frame.

use serde::{Deserialize, Serialize};

use crate::view::format;

/// The bar width in the 384 px panel.
pub const BAR_WIDTH: f64 = 296.0;
/// The bar width in the 320 px panel (Medium layout).
pub const BAR_WIDTH_MEDIUM: f64 = 232.0;
/// The bar's height.
pub const BAR_HEIGHT: f64 = 8.0;
/// The bottom of the scale, dBFS.
pub const FLOOR_DB: f64 = -60.0;
/// Where yellow starts, dBFS.
pub const YELLOW_DB: f64 = -18.0;
/// Where red starts, dBFS.
pub const RED_DB: f64 = -6.0;
/// A segment's width and the gap after it, px.
pub const SEGMENT_WIDTH: f64 = 2.0;
pub const SEGMENT_GAP: f64 = 1.0;
/// How long the peak holds before it falls, ms.
pub const HOLD_MS: u64 = 1500;
/// How fast a held peak falls, dB per second.
pub const FALL_DB_PER_S: f64 = 20.0;
/// A peak at or above this latches the clip marker, dBFS.
pub const CLIP_DB: f64 = -0.1;
/// The clip latch's width at the bar's right end, px.
pub const LATCH_WIDTH: f64 = 4.0;
/// The peak hold's width, px.
pub const PEAK_WIDTH: f64 = 2.0;
/// The opacity of a muted track's level and peak.
pub const MUTED_ALPHA: f64 = 0.35;
/// What a meter reads before its first reading: silence.
pub const SILENCE_DB: f64 = -120.0;

/// A meter color zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Zone {
    Green,
    Yellow,
    Red,
}

/// The x of `db` on a bar `width` wide.
pub fn x_of_db(db: f64, width: f64) -> f64 {
    if db.is_nan() {
        return 0.0;
    }
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) * width.max(0.0)
}

/// The level at `x` on a bar `width` wide (the inverse of [`x_of_db`]).
pub fn db_of_x(x: f64, width: f64) -> f64 {
    if width <= 0.0 {
        return FLOOR_DB;
    }
    x / width * -FLOOR_DB + FLOOR_DB
}

/// The zone `db` falls in.
pub fn zone(db: f64) -> Zone {
    if db < YELLOW_DB {
        Zone::Green
    } else if db < RED_DB {
        Zone::Yellow
    } else {
        Zone::Red
    }
}

/// The meter's state between frames: peak hold and clip latch.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterState {
    /// The current level (the maximum over channels), dBFS.
    pub level_db: f64,
    /// The held peak, dBFS.
    pub peak_db: f64,
    /// When the peak was last raised, ms on the frame clock.
    pub peak_at_ms: u64,
    pub clipped: bool,
}

impl Default for MeterState {
    /// Silence, nothing held, no latch.
    fn default() -> Self {
        Self {
            level_db: SILENCE_DB,
            peak_db: SILENCE_DB,
            peak_at_ms: 0,
            clipped: false,
        }
    }
}

impl MeterState {
    /// A reading of `level_db` at `now_ms` (the level is its own peak).
    pub fn update(&mut self, level_db: f64, now_ms: u64) {
        self.update_reading(level_db, level_db, now_ms);
    }

    /// A reading of the loudest channel's RMS `rms_db` and peak `peak_db`
    /// at `now_ms`: the bar follows the RMS; a peak at or over the held one
    /// holds for [`HOLD_MS`], then the hold falls at [`FALL_DB_PER_S`] until
    /// a peak catches it; a peak at or over [`CLIP_DB`] latches the clip
    /// marker.
    pub fn update_reading(&mut self, rms_db: f64, peak_db: f64, now_ms: u64) {
        let rms = if rms_db.is_nan() { SILENCE_DB } else { rms_db };
        let peak = if peak_db.is_nan() {
            rms
        } else {
            peak_db.max(rms)
        };
        self.level_db = rms;
        let held = self.hold_db(now_ms);
        if peak >= held {
            self.peak_db = peak;
            self.peak_at_ms = now_ms;
        } else if now_ms > self.peak_at_ms.saturating_add(HOLD_MS) {
            // Falling: re-anchor at the value it fell to, so the fall goes
            // on from there at the same speed.
            self.peak_db = held;
            self.peak_at_ms = now_ms - HOLD_MS;
        }
        if peak >= CLIP_DB {
            self.clipped = true;
        }
    }

    /// The held peak at `now_ms`: [`MeterState::peak_db`] for [`HOLD_MS`],
    /// then falling at [`FALL_DB_PER_S`].
    pub fn hold_db(&self, now_ms: u64) -> f64 {
        let falling_ms = now_ms.saturating_sub(self.peak_at_ms.saturating_add(HOLD_MS));
        (self.peak_db - falling_ms as f64 / 1000.0 * FALL_DB_PER_S).max(SILENCE_DB)
    }

    /// A click on the meter resets the clip latch.
    pub fn reset_clip(&mut self) {
        self.clipped = false;
    }
}

/// One segment of the bar: at `x`, `lit` of its 2 px lit in its zone's
/// color (the last lit one may be partly lit).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub x: f64,
    pub lit: f64,
    pub zone: Zone,
}

/// What the meter draws now (C11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterView {
    /// The lit segments, left to right (the unlit ones show the track).
    pub segments: Vec<Segment>,
    /// Where the 2 px peak hold starts; `None` below the scale.
    pub peak_x: Option<f64>,
    /// The red clip latch at the right end.
    pub latched: bool,
    /// The held peak as the readout: "−16.0", "0.0", "—".
    pub readout: String,
    /// The opacity of the level and the peak (35 % when muted).
    pub alpha: f64,
}

/// Every segment's x on a bar `width` wide: one per 3 px, the last one
/// clipped by the bar's end.
pub fn segment_xs(width: f64) -> impl Iterator<Item = f64> {
    let step = SEGMENT_WIDTH + SEGMENT_GAP;
    let count = if width > 0.0 {
        (width / step).ceil() as usize
    } else {
        0
    };
    (0..count).map(move |i| i as f64 * step)
}

/// The meter of `state` at `now_ms` on a bar `width` wide.
pub fn view(state: &MeterState, now_ms: u64, width: f64, muted: bool) -> MeterView {
    let level_x = x_of_db(state.level_db, width);
    let segments = segment_xs(width)
        .take_while(|x| *x < level_x)
        .map(|x| Segment {
            x,
            lit: (level_x - x).min(SEGMENT_WIDTH).min(width - x),
            zone: zone(db_of_x(x, width)),
        })
        .collect();
    let hold = state.hold_db(now_ms);
    let peak_x =
        (hold >= FLOOR_DB).then(|| x_of_db(hold, width).min((width - PEAK_WIDTH).max(0.0)));
    MeterView {
        segments,
        peak_x,
        latched: state.clipped,
        readout: format::peak(hold),
        alpha: if muted { MUTED_ALPHA } else { 1.0 },
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Mapping {
        width: f64,
        segments: usize,
        first_yellow_x: Option<f64>,
        first_red_x: Option<f64>,
        x: Vec<(String, f64)>,
    }

    fn mapping(width: f64) -> Mapping {
        let all = view(
            &MeterState {
                level_db: 0.0,
                peak_db: 0.0,
                peak_at_ms: 0,
                clipped: false,
            },
            0,
            width,
            false,
        );
        let first = |z| all.segments.iter().find(|s| s.zone == z).map(|s| s.x);
        Mapping {
            width,
            segments: all.segments.len(),
            first_yellow_x: first(Zone::Yellow),
            first_red_x: first(Zone::Red),
            x: [
                -90.0, -60.0, -52.0, -48.0, -22.0, -18.0, -16.0, -6.0, -0.1, 0.0, 3.0,
            ]
            .into_iter()
            .map(|db| {
                (
                    format!("{db} dB"),
                    (x_of_db(db, width) * 100.0).round() / 100.0,
                )
            })
            .collect(),
        }
    }

    #[test]
    fn the_mapping_at_384_and_320() {
        insta::assert_json_snapshot!("mapping", [mapping(BAR_WIDTH), mapping(BAR_WIDTH_MEDIUM)]);
    }

    #[test]
    fn s02_and_s13b_meters() {
        // S02: A1 at about −22 dBFS with the peak at −16; A2 at −52 / −48.
        let mut a1 = MeterState::default();
        a1.update_reading(-22.0, -16.0, 1000);
        let mut a2 = MeterState::default();
        a2.update_reading(-52.0, -48.0, 1000);
        // S13b: clipping, the red latch.
        let mut clip = MeterState::default();
        clip.update_reading(-3.0, 0.0, 1000);
        let views = [
            ("A1", view(&a1, 1000, BAR_WIDTH, false)),
            ("A2", view(&a2, 1000, BAR_WIDTH, false)),
            ("A1 muted", view(&a1, 1000, BAR_WIDTH, true)),
            ("clipping", view(&clip, 1000, BAR_WIDTH, false)),
            ("A1 at 320", view(&a1, 1000, BAR_WIDTH_MEDIUM, false)),
        ];
        let summary: Vec<serde_json::Value> = views
            .iter()
            .map(|(name, v)| {
                let last = v.segments.last().copied();
                serde_json::json!({
                    "name": name,
                    "litSegments": v.segments.len(),
                    "lastSegment": last,
                    "peakX": v.peak_x,
                    "latched": v.latched,
                    "readout": v.readout,
                    "alpha": v.alpha,
                })
            })
            .collect();
        insta::assert_json_snapshot!("design_meters", summary);
        assert_eq!(view(&a1, 1000, BAR_WIDTH, false).readout, "\u{2212}16.0");
        assert_eq!(view(&clip, 1000, BAR_WIDTH, false).readout, "0.0");
        let silent = MeterState::default();
        let v = view(&silent, 0, BAR_WIDTH, false);
        assert!(v.segments.is_empty() && v.peak_x.is_none());
        assert_eq!(v.readout, "\u{2014}");
    }

    #[test]
    fn the_peak_holds_then_falls_and_the_latch_waits_for_a_click() {
        let mut m = MeterState::default();
        m.update_reading(-20.0, -10.0, 0);
        assert_eq!(m.hold_db(0), -10.0);
        // Quieter readings within 1.5 s keep the hold.
        m.update_reading(-30.0, -25.0, 1000);
        assert_eq!(m.hold_db(1000), -10.0);
        assert_eq!(m.hold_db(HOLD_MS), -10.0);
        // Then it falls at 20 dB/s.
        assert!((m.hold_db(HOLD_MS + 500) - -20.0).abs() < 1e-9);
        m.update_reading(-30.0, -25.0, HOLD_MS + 500);
        assert!(
            (m.peak_db - -20.0).abs() < 1e-9,
            "re-anchored where it fell to"
        );
        assert!(
            (m.hold_db(HOLD_MS + 750) - -25.0).abs() < 1e-9,
            "still falling"
        );
        // A peak over the falling hold catches it and holds again.
        m.update_reading(-30.0, -24.0, HOLD_MS + 750);
        assert_eq!(m.hold_db(HOLD_MS + 750), -24.0);
        assert_eq!(m.hold_db(2 * HOLD_MS + 750), -24.0);
        assert!(!m.clipped);
        m.update_reading(-1.0, -0.1, 5000);
        assert!(m.clipped, "−0.1 dBFS latches");
        m.update_reading(-40.0, -40.0, 9000);
        assert!(m.clipped, "until clicked");
        m.reset_clip();
        assert!(!m.clipped);
        // `update` is a reading whose level is its own peak.
        let mut plain = MeterState::default();
        plain.update(-12.4, 0);
        assert_eq!((plain.level_db, plain.peak_db), (-12.4, -12.4));
        assert_eq!(view(&plain, 0, BAR_WIDTH, false).readout, "\u{2212}12.4");
        // NaN readings are silence.
        plain.update(f64::NAN, 10);
        assert_eq!(plain.level_db, SILENCE_DB);
    }

    #[test]
    fn zones_and_segments() {
        assert_eq!(zone(-60.0), Zone::Green);
        assert_eq!(zone(-18.01), Zone::Green);
        assert_eq!(zone(-18.0), Zone::Yellow);
        assert_eq!(zone(-6.0), Zone::Red);
        assert_eq!(zone(0.0), Zone::Red);
        assert_eq!(segment_xs(BAR_WIDTH).count(), 99);
        assert_eq!(segment_xs(BAR_WIDTH).last(), Some(294.0));
        assert_eq!(segment_xs(0.0).count(), 0);
        // A full bar ends exactly at the width (the last segment clipped).
        let full = view(
            &MeterState {
                level_db: 0.0,
                peak_db: 0.0,
                peak_at_ms: 0,
                clipped: true,
            },
            0,
            BAR_WIDTH,
            false,
        );
        let last = full.segments.last().unwrap();
        assert_eq!(last.x + last.lit, BAR_WIDTH);
        assert_eq!(full.peak_x, Some(BAR_WIDTH - PEAK_WIDTH));
    }

    proptest! {
        #[test]
        fn x_is_monotone_and_on_the_bar(a in -200.0f64..200.0, b in -200.0f64..200.0, w in 0.0f64..600.0) {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(x_of_db(lo, w) <= x_of_db(hi, w));
            prop_assert!((0.0..=w).contains(&x_of_db(a, w)));
            if (-60.0..=0.0).contains(&a) && w > 0.0 {
                prop_assert!((db_of_x(x_of_db(a, w), w) - a).abs() < 1e-9);
            }
        }

        /// The hold is never below the last reading's peak, never rises
        /// without a reading, and the lit part never passes the level.
        #[test]
        fn the_hold_covers_the_last_peak(readings in proptest::collection::vec((-80.0f64..6.0, 0.0f64..12.0, 0u64..3000), 1..40)) {
            let mut m = MeterState::default();
            let mut now = 0;
            for (rms, extra, dt) in readings {
                now += dt;
                let before = m.hold_db(now);
                m.update_reading(rms, rms + extra, now);
                prop_assert!(m.hold_db(now) >= rms + extra - 1e-9);
                prop_assert!(m.hold_db(now) >= before.min(rms + extra) - 1e-9);
                prop_assert!(m.hold_db(now + 5000) <= m.hold_db(now) + 1e-9);
                let v = view(&m, now, BAR_WIDTH, false);
                let lit_end = v.segments.last().map_or(0.0, |s| s.x + s.lit);
                prop_assert!(lit_end <= x_of_db(m.level_db, BAR_WIDTH) + 1e-9);
            }
        }
    }
}
