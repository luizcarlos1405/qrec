//! The theme palettes as `gdk::RGBA`s for custom-drawn widgets (G§3.14):
//! GTK can't look a CSS custom property up from code. Generated from
//! `design/tokens/qrec-tokens.json` by `crates/qrec-gui/tests/tokens.rs`
//! (WP0b part 2), one per theme, with every color token plus the eight
//! take colors and their tints; the only file in `src/` that may hold
//! color values.
//!
//! Until the generator lands this is a placeholder: transparent colors.

use gtk::gdk;
use qrec_core::config::Theme as ThemeId;

/// A theme's colors for drawing.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    /// The eight take colors (moss, donkey, onion, mud, lily, ginger,
    /// dragon, fairy; `view::format::take_color` indexes them).
    pub take: [gdk::RGBA; 8],
    /// Their tints (clip bodies).
    pub take_tint: [gdk::RGBA; 8],
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            take: [gdk::RGBA::TRANSPARENT; 8],
            take_tint: [gdk::RGBA::TRANSPARENT; 8],
        }
    }
}

impl Palette {
    /// The palette of theme `id`.
    pub fn for_theme(id: ThemeId) -> Palette {
        let _ = id;
        Palette::default()
    }
}
