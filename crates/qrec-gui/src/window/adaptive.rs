//! The adaptive modes (G§3.15, DESIGN Layout, G§0.3): `adw::Breakpoint`s
//! on the window set `LocalUi.layout`; the window applies it (the panel's
//! width or sheet, the timeline's height) and each region lays itself out
//! for it.
//!
//! - Wide (≥ 1400 px): the panel docked at 384.
//! - Medium (1100–1399): docked at 320, class `.panel-medium` (WP5 lays
//!   the inspector fields 2-up).
//! - Narrow (< 1100): the panel is an overlay sheet (`Overlay::LayersSheet`)
//!   toggled by the header's layers button and closed by `Esc`.
//! - compact (height < 820): the timeline keeps 232 px; the stage shrinks.
//!
//! The conditions exclude each other, so exactly one breakpoint (or none:
//! Wide, regular) applies whatever order libadwaita checks them in.

use adw::prelude::*;

use crate::store::Store;
use crate::store::local::{LayoutClass, LayoutWidth};

/// Each breakpoint's condition and the class it means. No breakpoint:
/// Wide, regular.
pub const BREAKPOINTS: [(&str, LayoutWidth, bool); 5] = [
    (
        "min-width: 1100px and max-width: 1399px and min-height: 820px",
        LayoutWidth::Medium,
        false,
    ),
    (
        "max-width: 1099px and min-height: 820px",
        LayoutWidth::Narrow,
        false,
    ),
    (
        "min-width: 1400px and max-height: 819px",
        LayoutWidth::Wide,
        true,
    ),
    (
        "min-width: 1100px and max-width: 1399px and max-height: 819px",
        LayoutWidth::Medium,
        true,
    ),
    (
        "max-width: 1099px and max-height: 819px",
        LayoutWidth::Narrow,
        true,
    ),
];

/// Adds the breakpoints to `window`; `LocalUi.layout` follows them.
pub fn install(window: &adw::ApplicationWindow, store: &Store) {
    let mut classes = Vec::new();
    for (condition, width, compact) in BREAKPOINTS {
        let condition = match adw::BreakpointCondition::parse(condition) {
            Ok(condition) => condition,
            Err(e) => {
                tracing::error!("breakpoint {condition:?}: {e}");
                continue;
            }
        };
        let breakpoint = adw::Breakpoint::new(condition);
        window.add_breakpoint(breakpoint.clone());
        classes.push((breakpoint, LayoutClass { width, compact }));
    }
    let store = store.clone();
    window.connect_current_breakpoint_notify(move |window| {
        let class = window
            .current_breakpoint()
            .and_then(|current| {
                classes
                    .iter()
                    .find(|(bp, _)| *bp == current)
                    .map(|(_, class)| *class)
            })
            .unwrap_or_default();
        tracing::debug!("layout: {class:?}");
        store.update_local(move |l| l.layout = class);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny evaluator of the conditions above, to check they cover each
    /// size exactly once and agree with `LayoutClass::for_size`.
    fn matches(condition: &str, width: i32, height: i32) -> bool {
        condition.split(" and ").all(|part| {
            let (key, value) = part.split_once(": ").expect("key: value");
            let value: i32 = value.trim_end_matches("px").parse().expect("px");
            match key {
                "min-width" => width >= value,
                "max-width" => width <= value,
                "min-height" => height >= value,
                "max-height" => height <= value,
                other => panic!("{other}"),
            }
        })
    }

    #[test]
    fn exactly_one_class_per_size() {
        for width in [800, 999, 1000, 1099, 1100, 1300, 1399, 1400, 1600, 2560] {
            for height in [600, 800, 819, 820, 1000, 1440] {
                let hits: Vec<LayoutClass> = BREAKPOINTS
                    .iter()
                    .filter(|(c, _, _)| matches(c, width, height))
                    .map(|(_, w, compact)| LayoutClass {
                        width: *w,
                        compact: *compact,
                    })
                    .collect();
                assert!(hits.len() <= 1, "{width}×{height}: {hits:?}");
                let class = hits.first().copied().unwrap_or_default();
                assert_eq!(
                    class,
                    LayoutClass::for_size(width, height),
                    "{width}×{height}"
                );
            }
        }
    }
}
