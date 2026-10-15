//! Where a layer's pixels go on the canvas (ARCHITECTURE §9.1).
//!
//! Export, frame save, GES preview and the GUI canvas all place layers with
//! [`place`], so they can't disagree. The result is "take this region of the
//! source, scale it to this size, put it here": every renderer can do that
//! with a crop, a scale and an overlay.
//!
//! Source regions are aligned to even pixels and output sizes are even, so
//! 4:2:0 chroma subsampling crops and scales the same way in ffmpeg and in
//! GStreamer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{Crop, Fit, Rect};

/// A region of the source frame, in source pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceRegion {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// The source region to take and the canvas rectangle to draw it into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub src: SourceRegion,
    pub dst: Rect,
}

fn even_down(v: u32) -> u32 {
    v & !1
}

/// The nearest even integer, ties to the multiple of 4 (`2·round_half_even(v/2)`).
/// ffmpeg floors odd overlay positions while GES rounds them up, so positions
/// are made even before either sees them.
fn even_pos(v: i64) -> i32 {
    let half = v.div_euclid(2);
    let even = if v.rem_euclid(2) == 0 {
        v
    } else if half.rem_euclid(2) == 0 {
        2 * half
    } else {
        2 * half + 2
    };
    even as i32
}

/// Rounds to the nearest even number, at least 2.
fn even_round(v: f64) -> u32 {
    let v = (v / 2.0).round() * 2.0;
    v.max(2.0) as u32
}

/// Places a `src_w × src_h` source cropped by `crop` into `rect` with `fit`.
///
/// Returns `None` when nothing would be visible: an empty crop or rect, or a
/// result thinner than a pixel.
pub fn place(src_w: u32, src_h: u32, crop: Crop, rect: Rect, fit: Fit) -> Option<Placement> {
    if rect.w == 0 || rect.h == 0 {
        return None;
    }
    let left = even_down(crop.left);
    let top = even_down(crop.top);
    let cw = even_down(src_w.checked_sub(crop.left.checked_add(crop.right)?)?);
    let ch = even_down(src_h.checked_sub(crop.top.checked_add(crop.bottom)?)?);
    if cw < 2 || ch < 2 {
        return None;
    }
    let (cwf, chf, rwf, rhf) = (
        f64::from(cw),
        f64::from(ch),
        f64::from(rect.w),
        f64::from(rect.h),
    );
    match fit {
        Fit::Contain => {
            let scale = (rwf / cwf).min(rhf / chf);
            if cwf * scale < 1.0 || chf * scale < 1.0 {
                return None; // thinner than a pixel: nothing visible
            }
            let w = even_round(cwf * scale).min(even_down(rect.w).max(2));
            let h = even_round(chf * scale).min(even_down(rect.h).max(2));
            let x = even_pos(i64::from(rect.x) + (i64::from(rect.w) - i64::from(w)) / 2);
            let y = even_pos(i64::from(rect.y) + (i64::from(rect.h) - i64::from(h)) / 2);
            Some(Placement {
                src: SourceRegion {
                    x: left,
                    y: top,
                    w: cw,
                    h: ch,
                },
                dst: Rect::new(x, y, w, h),
            })
        }
        Fit::Cover => {
            // Crop the source (centered) to the rect's aspect ratio, then scale.
            let (raw_w, raw_h) = if cwf * rhf > chf * rwf {
                (chf * rwf / rhf, chf)
            } else {
                (cwf, cwf * rhf / rwf)
            };
            if raw_w < 1.0 || raw_h < 1.0 {
                return None; // the visible source region is thinner than a pixel
            }
            let (sw, sh) = (even_round(raw_w).min(cw), even_round(raw_h).min(ch));
            let sx = left + even_down((cw - sw) / 2);
            let sy = top + even_down((ch - sh) / 2);
            let w = even_down(rect.w).max(2);
            let h = even_down(rect.h).max(2);
            let dst = Rect::new(
                even_pos(i64::from(rect.x)),
                even_pos(i64::from(rect.y)),
                w,
                h,
            );
            Some(Placement {
                src: SourceRegion {
                    x: sx,
                    y: sy,
                    w: sw,
                    h: sh,
                },
                dst,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const NO_CROP: Crop = Crop {
        top: 0,
        right: 0,
        bottom: 0,
        left: 0,
    };

    #[test]
    fn contain_letterboxes_and_centers() {
        // 2560x1440 (16:9) into a 4:3 rect: full width, letterboxed.
        let p = place(
            2560,
            1440,
            NO_CROP,
            Rect::new(100, 100, 800, 600),
            Fit::Contain,
        )
        .unwrap();
        assert_eq!(
            p.src,
            SourceRegion {
                x: 0,
                y: 0,
                w: 2560,
                h: 1440
            }
        );
        assert_eq!(p.dst, Rect::new(100, 176, 800, 450)); // 175 rounded to even
        // Same aspect: fills the rect exactly.
        let p = place(
            2560,
            1440,
            NO_CROP,
            Rect::new(0, 0, 1920, 1080),
            Fit::Contain,
        )
        .unwrap();
        assert_eq!(p.dst, Rect::new(0, 0, 1920, 1080));
    }

    #[test]
    fn cover_crops_the_source_to_the_rect_aspect() {
        // The spec's camera layer: 1280x720 into 384x288 (4:3), fit cover.
        let p = place(
            1280,
            720,
            NO_CROP,
            Rect::new(1500, 760, 384, 288),
            Fit::Cover,
        )
        .unwrap();
        assert_eq!(p.dst, Rect::new(1500, 760, 384, 288));
        assert_eq!(
            p.src,
            SourceRegion {
                x: 160,
                y: 0,
                w: 960,
                h: 720
            }
        );
    }

    #[test]
    fn crop_is_applied_first() {
        let crop = Crop {
            top: 0,
            right: 640,
            bottom: 0,
            left: 0,
        };
        let p = place(2560, 1440, crop, Rect::new(0, 0, 1920, 1080), Fit::Contain).unwrap();
        assert_eq!(
            p.src,
            SourceRegion {
                x: 0,
                y: 0,
                w: 1920,
                h: 1440
            }
        );
        assert_eq!(p.dst, Rect::new(240, 0, 1440, 1080));
    }

    #[test]
    fn positions_are_even() {
        assert_eq!(even_pos(33), 32);
        assert_eq!(even_pos(35), 36);
        assert_eq!(even_pos(-3), -4);
        assert_eq!(even_pos(-1), 0);
        let p = place(1280, 720, NO_CROP, Rect::new(33, 17, 384, 288), Fit::Cover).unwrap();
        assert_eq!((p.dst.x, p.dst.y), (32, 16));
    }

    #[test]
    fn degenerate_inputs() {
        assert!(
            place(
                100,
                100,
                Crop {
                    top: 0,
                    right: 60,
                    bottom: 0,
                    left: 60
                },
                Rect::new(0, 0, 10, 10),
                Fit::Contain
            )
            .is_none()
        );
        assert!(place(100, 100, NO_CROP, Rect::new(0, 0, 0, 10), Fit::Cover).is_none());
        assert!(place(1, 1, NO_CROP, Rect::new(0, 0, 10, 10), Fit::Cover).is_none());
    }

    proptest! {
        #[test]
        fn placements_stay_in_bounds(
            sw in 2u32..5000, sh in 2u32..5000,
            ct in 0u32..400, cr in 0u32..400, cb in 0u32..400, cl in 0u32..400,
            x in -2000i32..4000, y in -2000i32..4000, w in 2u32..4000, h in 2u32..4000,
            cover in any::<bool>(),
        ) {
            let fit = if cover { Fit::Cover } else { Fit::Contain };
            let crop = Crop { top: ct, right: cr, bottom: cb, left: cl };
            if let Some(p) = place(sw, sh, crop, Rect::new(x, y, w, h), fit) {
                // The source region stays inside the cropped source, aligned to even pixels.
                prop_assert!(p.src.x >= cl & !1 && p.src.y >= ct & !1);
                prop_assert!(p.src.x + p.src.w <= sw && p.src.y + p.src.h <= sh);
                prop_assert!(p.src.x % 2 == 0 && p.src.y % 2 == 0 && p.src.w % 2 == 0 && p.src.h % 2 == 0);
                prop_assert!(p.dst.w % 2 == 0 && p.dst.h % 2 == 0 && p.dst.w >= 2 && p.dst.h >= 2);
                prop_assert!(p.dst.x % 2 == 0 && p.dst.y % 2 == 0);
                if fit == Fit::Contain {
                    // Inside the rect, give or take the 1 px of even rounding.
                    prop_assert!(p.dst.x >= x - 1 && p.dst.y >= y - 1);
                    prop_assert!(i64::from(p.dst.x) + i64::from(p.dst.w) <= i64::from(x) + i64::from(w.max(2)) + 1);
                    prop_assert!(i64::from(p.dst.y) + i64::from(p.dst.h) <= i64::from(y) + i64::from(h.max(2)) + 1);
                }
                // Aspect ratio is preserved within rounding.
                let src_aspect = f64::from(p.src.w) / f64::from(p.src.h);
                let dst_aspect = f64::from(p.dst.w) / f64::from(p.dst.h);
                let tolerance = 2.5 / f64::from(p.dst.w.min(p.dst.h)) + 2.5 / f64::from(p.src.w.min(p.src.h));
                prop_assert!((src_aspect / dst_aspect - 1.0).abs() <= tolerance * 4.0,
                    "src {:?} dst {:?}", p.src, p.dst);
            }
        }
    }
}
