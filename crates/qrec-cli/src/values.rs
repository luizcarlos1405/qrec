//! Value parsers for arguments whose shape clap can't check by type alone.
//!
//! Times and canvas specs are deliberately not parsed here: the core owns
//! those grammars (`TimeArg`, `CanvasSpec`) and reports their errors.

use std::str::FromStr;

use qrec_core::{Crop, Rect, TrackId};

/// `auto` (stored as `null`: let Qrec choose) or an explicit value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutoOr<T> {
    Auto,
    Value(T),
}

const RECT_SHAPE: &str = "X,Y,W,H in canvas pixels, e.g. 0,0,1920,1080 or -100,40,640,360";
const CROP_SHAPE: &str = "TOP,RIGHT,BOTTOM,LEFT in source pixels, e.g. 0,0,40,0";

fn four<'a>(s: &'a str, shape: &str) -> Result<[&'a str; 4], String> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    <[&str; 4]>::try_from(parts).map_err(|_| format!("expected {shape}"))
}

/// `X,Y,W,H`: position may be negative (layers can extend past the canvas),
/// size must be at least 1.
pub(crate) fn parse_rect(s: &str) -> Result<Rect, String> {
    let [x, y, w, h] = four(s, RECT_SHAPE)?;
    let pos = |v: &str| {
        v.parse::<i32>()
            .map_err(|_| format!("{v:?} is not an integer; expected {RECT_SHAPE}"))
    };
    let size = |v: &str| match v.parse::<u32>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "{v:?} is not a size of at least 1; expected {RECT_SHAPE}"
        )),
    };
    Ok(Rect::new(pos(x)?, pos(y)?, size(w)?, size(h)?))
}

/// `T,R,B,L` (CSS order), non-negative.
pub(crate) fn parse_crop(s: &str) -> Result<Crop, String> {
    let [top, right, bottom, left] = four(s, CROP_SHAPE)?;
    let px = |v: &str| {
        v.parse::<u32>()
            .map_err(|_| format!("{v:?} is not a pixel count; expected {CROP_SHAPE}"))
    };
    Ok(Crop {
        top: px(top)?,
        right: px(right)?,
        bottom: px(bottom)?,
        left: px(left)?,
    })
}

/// A number from 0.0 to 1.0.
pub(crate) fn parse_opacity(s: &str) -> Result<f64, String> {
    match s.trim().parse::<f64>() {
        Ok(v) if (0.0..=1.0).contains(&v) => Ok(v),
        _ => Err(format!(
            "{s:?} is not an opacity; expected a number from 0.0 to 1.0, e.g. 0.8"
        )),
    }
}

/// A track id (`A1`) or `auto`.
pub(crate) fn parse_track_or_auto(s: &str) -> Result<AutoOr<TrackId>, String> {
    if s == "auto" {
        return Ok(AutoOr::Auto);
    }
    TrackId::from_str(s)
        .map(AutoOr::Value)
        .map_err(|e| format!("{} or auto", e.message))
}

/// A render node path (`/dev/dri/renderD129`) or `auto`.
pub(crate) fn parse_device_or_auto(s: &str) -> Result<AutoOr<String>, String> {
    match s {
        "auto" => Ok(AutoOr::Auto),
        _ if s.starts_with('/') => Ok(AutoOr::Value(s.to_owned())),
        _ => Err(format!(
            "{s:?} is not a device path; expected e.g. /dev/dri/renderD129, or auto"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects() {
        assert_eq!(parse_rect("0,0,1920,1080"), Ok(Rect::new(0, 0, 1920, 1080)));
        assert_eq!(
            parse_rect("-100, 40, 640, 360"),
            Ok(Rect::new(-100, 40, 640, 360))
        );
        assert!(parse_rect("0,0,0,1080").is_err());
        assert!(parse_rect("0,0,1920").is_err());
        assert!(parse_rect("0,0,1920,1080,1").is_err());
        assert!(parse_rect("a,0,1,1").is_err());
        assert!(parse_rect("0,0,-1,1").is_err());
    }

    #[test]
    fn crops() {
        assert_eq!(
            parse_crop("1,2,3,4"),
            Ok(Crop {
                top: 1,
                right: 2,
                bottom: 3,
                left: 4
            })
        );
        assert!(parse_crop("-1,0,0,0").is_err());
        assert!(parse_crop("0,0,0").is_err());
    }

    #[test]
    fn opacity() {
        assert_eq!(parse_opacity("0.8"), Ok(0.8));
        assert_eq!(parse_opacity("1"), Ok(1.0));
        assert!(parse_opacity("1.5").is_err());
        assert!(parse_opacity("-0.1").is_err());
        assert!(parse_opacity("NaN").is_err());
    }

    #[test]
    fn auto_values() {
        assert_eq!(parse_track_or_auto("auto"), Ok(AutoOr::Auto));
        assert_eq!(
            parse_track_or_auto("A2"),
            Ok(AutoOr::Value("A2".parse().unwrap()))
        );
        assert!(parse_track_or_auto("A 2").is_err());
        assert_eq!(parse_device_or_auto("auto"), Ok(AutoOr::Auto));
        assert_eq!(
            parse_device_or_auto("/dev/dri/renderD129"),
            Ok(AutoOr::Value("/dev/dri/renderD129".into()))
        );
        assert!(parse_device_or_auto("renderD129").is_err());
    }
}
