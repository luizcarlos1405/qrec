//! The level meter (G§4.7, C11). Owner: WP5. Stub until WP5 lands: the
//! signatures are final, the bodies return defaults.
//!
//! x(db) = clamp((db + 60) / 60, 0, 1) × the bar width; zones −60…−18
//! `meter-green`, −18…−6 `meter-yellow`, −6…0 `meter-red`; 2 px segments
//! with 1 px gaps; a 2 px peak hold falling at 20 dB/s after 1.5 s; a clip
//! latch from −0.1 dBFS until clicked.

use serde::{Deserialize, Serialize};

/// The bar width in the 384 px panel.
pub const BAR_WIDTH: f64 = 296.0;
/// The bar width in the 320 px panel (Medium layout).
pub const BAR_WIDTH_MEDIUM: f64 = 232.0;

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
    let _ = (db, width);
    0.0
}

/// The zone `db` falls in.
pub fn zone(db: f64) -> Zone {
    let _ = db;
    Zone::Green
}

/// The meter's state between frames: peak hold and clip latch.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
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

impl MeterState {
    /// A reading of `level_db` at `now_ms`.
    pub fn update(&mut self, level_db: f64, now_ms: u64) {
        let _ = (level_db, now_ms);
    }

    /// A click on the meter resets the clip latch.
    pub fn reset_clip(&mut self) {}
}
