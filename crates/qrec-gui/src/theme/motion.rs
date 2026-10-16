//! Motion (G§3.14, DESIGN Motion, D-30): the reduced-motion policy, the
//! easing curves, and [`now`], the clock every looping animation reads (the
//! torch flicker, the mud bar, the countdown, spinners, the ghost stripes).
//!
//! Every Qrec animation is driven from code, never CSS `@keyframes`, so
//! test mode can freeze them: under `QREC_GUI_TEST` [`now`] stays at
//! `QREC_GUI_TEST_T_MS` (default 0) and one-shot animations jump to their
//! end (G§8.4).

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::testctl;

/// Whether animations are reduced (D-30): the window gets the class
/// `.reduced-motion`; looping animations hold still, transitions are
/// instant (spinners keep spinning, G§1.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MotionPolicy {
    pub reduced: bool,
}

impl MotionPolicy {
    /// D-30: the config's `ui.reduced_motion`, or the system's
    /// (`gtk-interface-reduced-motion = reduce`, or animations off). Under
    /// `QREC_GUI_TEST` only the config counts: test mode turns GTK's
    /// animations off itself (R29).
    pub fn decide(config: bool, test_mode: bool, system_reduce: bool, animations: bool) -> Self {
        let system = !test_mode && (system_reduce || !animations);
        Self {
            reduced: config || system,
        }
    }

    /// The window's policy now: the config's choice and GTK's settings.
    pub fn from_settings(config: bool) -> Self {
        let test_mode = testctl::mode().on;
        let (reduce, animations) = gtk::Settings::default().map_or((false, true), |s| {
            (
                s.gtk_interface_reduced_motion() == gtk::ReducedMotion::Reduce,
                s.is_gtk_enable_animations(),
            )
        });
        Self::decide(config, test_mode, reduce, animations)
    }
}

/// A CSS-style cubic Bézier easing curve `(x1, y1, x2, y2)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve(pub f64, pub f64, pub f64, pub f64);

/// The standard curve (0.4, 0, 0.2, 1).
pub const EASE_STD: Curve = Curve(0.4, 0.0, 0.2, 1.0);
/// Fast out (0.16, 1, 0.3, 1): zoom, the bin drawer, toasts rising.
pub const EASE_OUT_EXPO: Curve = Curve(0.16, 1.0, 0.3, 1.0);
/// Accelerating (0.4, 0, 1, 1): things leaving.
pub const EASE_IN: Curve = Curve(0.4, 0.0, 1.0, 1.0);

impl Curve {
    /// The eased progress at `t` ∈ [0, 1].
    pub fn at(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        let Curve(x1, y1, x2, y2) = self;
        let bezier = |a: f64, b: f64, s: f64| {
            let u = 1.0 - s;
            3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
        };
        // Solve x(s) = t by bisection (x is monotonic for x1, x2 in [0, 1]).
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..40 {
            let mid = (lo + hi) / 2.0;
            if bezier(x1, x2, mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        bezier(y1, y2, (lo + hi) / 2.0)
    }
}

/// The animation clock: time since the window started, or the frozen test
/// time (`QREC_GUI_TEST_T_MS`).
pub fn now() -> Duration {
    let mode = testctl::mode();
    if mode.on {
        return Duration::from_millis(mode.t_ms);
    }
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// Whether one-shot animations should jump to their end (test mode).
pub fn one_shots_at_end() -> bool {
    testctl::mode().on
}

/// Whether [`now`] stands still (test mode): a looping animation is one
/// picture then, at `QREC_GUI_TEST_T_MS`, and needs no frames to show it
/// (the mud bar runs no tick callback, G§8.6).
pub fn frozen() -> bool {
    testctl::mode().on
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_motion_follows_d30() {
        let decide = MotionPolicy::decide;
        assert!(!decide(false, false, false, true).reduced);
        assert!(decide(true, false, false, true).reduced, "the config");
        assert!(decide(false, false, true, true).reduced, "the system");
        assert!(decide(false, false, false, false).reduced, "animations off");
        // Test mode: only the config (it turns animations off itself).
        assert!(!decide(false, true, true, false).reduced);
        assert!(decide(true, true, false, false).reduced);
    }

    #[test]
    fn curves_start_and_end_in_place() {
        for curve in [EASE_STD, EASE_OUT_EXPO, EASE_IN] {
            assert!(curve.at(0.0).abs() < 1e-6, "{curve:?}");
            assert!((curve.at(1.0) - 1.0).abs() < 1e-6, "{curve:?}");
            let mut last = 0.0;
            for i in 1..=20 {
                let v = curve.at(f64::from(i) / 20.0);
                assert!(v >= last - 1e-9, "{curve:?} is monotonic");
                last = v;
            }
        }
        // Expo out is far along early; ease-in is late.
        assert!(EASE_OUT_EXPO.at(0.2) > 0.6);
        assert!(EASE_IN.at(0.5) < 0.5);
    }
}
