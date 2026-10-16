//! The bundled fonts (SPEC §15.3, G§3.14): Fraunces (display),
//! Atkinson Hyperlegible Next (UI) and JetBrains Mono (numbers, key caps),
//! registered with Pango before any widget (R1 allows these three reads on
//! the GTK thread before the first window maps).
//!
//! Where: `$QREC_FONT_DIR` (the dev shell), else `<exe dir>/../share/qrec/fonts`
//! (the package). Missing files are a warning; the DESIGN fallback stacks
//! take over.

use std::path::{Path, PathBuf};

use gtk::prelude::*;

/// The variable font files Qrec ships.
pub const FILES: [&str; 3] = [
    "Fraunces[SOFT,WONK,opsz,wght].ttf",
    "AtkinsonHyperlegibleNext[wght].ttf",
    "JetBrainsMono[wght].ttf",
];

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
