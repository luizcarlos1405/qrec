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

/// Rounds to the nearest even number, at least 2.
fn even_round(v: f64) -> u32 {
    let v = (v / 2.0).round() * 2.0;
    v.max(2.0) as u32
}

/// Places a `src_w × src_h` source cropped by `crop` into `rect` with `fit`.
///
/// Returns `None` when nothing would be visible (empty crop or empty rect).
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
    let (cwf, chf, rwf, rhf) = (f64::from(cw), f64::from(ch), f64::from(rect.w), f64::from(rect.h));
    match fit {
        Fit::Contain => {
            let scale = (rwf / cwf).min(rhf / chf);
            let w = even_round(cwf * scale).min(even_down(rect.w).max(2));
            let h = even_round(chf * scale).min(even_down(rect.h).max(2));
            let x = rect.x + ((i64::from(rect.w) - i64::from(w)) / 2) as i32;
            let y = rect.y + ((i64::from(rect.h) - i64::from(h)) / 2) as i32;
            Some(Placement { src: SourceRegion { x: left, y: top, w: cw, h: ch }, dst: Rect::new(x, y, w, h) })
        }
        Fit::Cover => {
            // Crop the source (centered) to the rect's aspect ratio, then scale.
            let (sw, sh) = if cwf * rhf > chf * rwf {
                (even_round(chf * rwf / rhf).min(cw), ch)
            } else {
                (cw, even_round(cwf * rhf / rwf).min(ch))
            };
            let sx = left + even_down((cw - sw) / 2);
            let sy = top + even_down((ch - sh) / 2);
            let w = even_down(rect.w).max(2);
            let h = even_down(rect.h).max(2);
            Some(Placement { src: SourceRegion { x: sx, y: sy, w: sw, h: sh }, dst: Rect::new(rect.x, rect.y, w, h) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const NO_CROP: Crop = Crop { top: 0, right: 0, bottom: 0, left: 0 };

    #[test]
    fn contain_letterboxes_and_centers() {
        // 2560x1440 (16:9) into a 4:3 rect: full width, letterboxed.
        let p = place(2560, 1440, NO_CROP, Rect::new(100, 100, 800, 600), Fit::Contain).unwrap();
        assert_eq!(p.src, SourceRegion { x: 0, y: 0, w: 2560, h: 1440 });
        assert_eq!(p.dst, Rect::new(100, 175, 800, 450));
        // Same aspect: fills the rect exactly.
        let p = place(2560, 1440, NO_CROP, Rect::new(0, 0, 1920, 1080), Fit::Contain).unwrap();
        assert_eq!(p.dst, Rect::new(0, 0, 1920, 1080));
    }

    #[test]
    fn cover_crops_the_source_to_the_rect_aspect() {
        // The spec's camera layer: 1280x720 into 384x288 (4:3), fit cover.
        let p = place(1280, 720, NO_CROP, Rect::new(1500, 760, 384, 288), Fit::Cover).unwrap();
        assert_eq!(p.dst, Rect::new(1500, 760, 384, 288));
        assert_eq!(p.src, SourceRegion { x: 160, y: 0, w: 960, h: 720 });
    }

    #[test]
    fn crop_is_applied_first() {
        let crop = Crop { top: 0, right: 640, bottom: 0, left: 0 };
        let p = place(2560, 1440, crop, Rect::new(0, 0, 1920, 1080), Fit::Contain).unwrap();
        assert_eq!(p.src, SourceRegion { x: 0, y: 0, w: 1920, h: 1440 });
        assert_eq!(p.dst, Rect::new(240, 0, 1440, 1080));
    }

    #[test]
    fn degenerate_inputs() {
        assert!(place(100, 100, Crop { top: 0, right: 60, bottom: 0, left: 60 }, Rect::new(0, 0, 10, 10), Fit::Contain).is_none());
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
                if fit == Fit::Contain {
                    // Inside the rect (allowing the even rounding of odd rects).
                    prop_assert!(p.dst.x >= x && p.dst.y >= y);
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
