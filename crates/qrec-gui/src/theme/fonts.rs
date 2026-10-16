//! The bundled fonts (SPEC §15.3, G§3.14): Fraunces (display),
//! Atkinson Hyperlegible Next (UI) and JetBrains Mono (numbers, key caps),
//! registered with Pango before any widget (R1 allows these three reads on
//! the GTK thread before the first window maps).
//!
//! Where: `$QREC_FONT_DIR` (the dev shell), else `<exe dir>/../share/qrec/fonts`
//! (the package). Missing files are a warning; the DESIGN fallback stacks
//! take over. [`resolve`] says which family Pango picks for each of the
//! three names (startup logs it; the test channel's `fonts` op reports it).
//!
//! Font rendering is GTK's manual mode ([`configure_rendering`]): the
//! desktop's antialiasing and hinting, the same for layout and drawing.

use std::path::{Path, PathBuf};

use gtk::pango;
use gtk::prelude::*;

/// The variable font files Qrec ships.
pub const FILES: [&str; 3] = [
    "Fraunces[SOFT,WONK,opsz,wght].ttf",
    "AtkinsonHyperlegibleNext[wght].ttf",
    "JetBrainsMono[wght].ttf",
];

/// The families in those files (the tokens' `font-*` names).
pub const FAMILIES: [&str; 3] = ["Fraunces", "Atkinson Hyperlegible Next", "JetBrains Mono"];

/// The folder holding [`FILES`]: `$QREC_FONT_DIR`, else next to the
/// executable.
pub fn font_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QREC_FONT_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("../share/qrec/fonts"))
}

/// Registers [`FILES`] from `dir` with the default font map; one warning
/// per file that couldn't be added.
pub fn register(dir: &Path) -> Vec<String> {
    let label = gtk::Label::new(None);
    let Some(map) = label.pango_context().font_map() else {
        return vec!["no Pango font map: the bundled fonts aren't used".to_owned()];
    };
    FILES
        .iter()
        .filter_map(|file| {
            let path = dir.join(file);
            match map.add_font_file(&path) {
                Ok(()) => {
                    tracing::debug!("font {}", path.display());
                    None
                }
                Err(e) => Some(format!("can't load the font {}: {e}", path.display())),
            }
        })
        .collect()
}

/// For each of [`FAMILIES`], the family Pango loads for it (the same name
/// once the bundled file is registered; a fallback otherwise).
pub fn resolve() -> Vec<(String, String)> {
    let context = gtk::Label::new(None).pango_context();
    FAMILIES
        .iter()
        .map(|family| {
            let mut desc = pango::FontDescription::new();
            desc.set_family(family);
            desc.set_size(12 * pango::SCALE);
            let loaded = context
                .load_font(&desc)
                .and_then(|font| font.describe().family())
                .map(|f| f.to_string())
                .unwrap_or_default();
            ((*family).to_owned(), loaded)
        })
        .collect()
}

/// Whether every bundled family resolves to itself.
pub fn all_resolved(resolved: &[(String, String)]) -> bool {
    resolved
        .iter()
        .all(|(wanted, got)| wanted.eq_ignore_ascii_case(got))
}

/// GTK's manual font rendering: the desktop's antialiasing and hinting
/// (`gtk-xft-*`), applied the same way to text layout and drawing.
///
/// Why: GTK 4.22's automatic mode measured a label's text a pixel shorter
/// at the top than it drew it, so a partial redraw of the header title
/// (its text changed: the folder name, then the project's) neither cleared
/// the old title's top row nor drew the new one's. The stray mark above the
/// title was the old title's ascender tops; `GSK_DEBUG=full-redraw` and the
/// manual mode both remove it (nested sway, GL and Cairo renderers alike).
pub fn configure_rendering(settings: &gtk::Settings) {
    settings.set_gtk_font_rendering(gtk::FontRendering::Manual);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_resolve_only_to_themselves() {
        let pairs = |got: [&str; 3]| -> Vec<(String, String)> {
            FAMILIES
                .iter()
                .zip(got)
                .map(|(w, g)| ((*w).to_owned(), g.to_owned()))
                .collect()
        };
        assert!(all_resolved(&pairs([
            "Fraunces",
            "atkinson hyperlegible next",
            "JetBrains Mono"
        ])));
        assert!(!all_resolved(&pairs([
            "Fraunces",
            "Cantarell",
            "JetBrains Mono"
        ])));
    }
}
