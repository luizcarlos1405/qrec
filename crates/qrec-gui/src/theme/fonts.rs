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
//! Test mode can try another setup ([`Rendering`],
//! `QREC_GUI_TEST_FONT_RENDERING`: G§3.14's text-rendering experiment).

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
/// (`gtk-xft-*`), applied the same way to text layout and drawing
/// ([`Rendering::QREC`]; in test mode `QREC_GUI_TEST_FONT_RENDERING` may
/// name another, [`Rendering::parse`]).
///
/// Why: GTK 4.22's automatic mode measured a label's text a pixel shorter
/// at the top than it drew it, so a partial redraw of the header title
/// (its text changed: the folder name, then the project's) neither cleared
/// the old title's top row nor drew the new one's. The stray mark above the
/// title was the old title's ascender tops; `GSK_DEBUG=full-redraw` and the
/// manual mode both remove it (nested sway, GL and Cairo renderers alike).
///
/// The cost: Cairo hints the metrics, so every glyph advances a whole pixel
/// (JetBrains Mono 7, 7 and 8 px at 11, 12 and 13 px where the frames
/// advance 6.6, 7.2 and 7.8). WP10's experiment (G§3.14) measured the
/// other setups: unhinted metrics bring every text within a pixel of the
/// frames, and the mark back on GL with them (`gui_text`), so this stays.
pub fn configure_rendering(settings: &gtk::Settings) {
    let rendering = if crate::testctl::mode().on {
        test_rendering()
    } else {
        Rendering::QREC
    };
    rendering.apply(settings);
    tracing::info!("font rendering: {}", rendering.describe());
}

/// Test mode's `QREC_GUI_TEST_FONT_RENDERING`, else [`Rendering::QREC`] (a
/// value that doesn't parse is an error in the log, and Qrec's own).
fn test_rendering() -> Rendering {
    match std::env::var("QREC_GUI_TEST_FONT_RENDERING") {
        Ok(spec) if !spec.trim().is_empty() => Rendering::parse(&spec).unwrap_or_else(|e| {
            tracing::error!("QREC_GUI_TEST_FONT_RENDERING={spec:?}: {e}");
            Rendering::QREC
        }),
        _ => Rendering::QREC,
    }
}

/// A hint style (`gtk-xft-hintstyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintStyle {
    None,
    Slight,
    Medium,
    Full,
}

impl HintStyle {
    /// The setting's value (`hintslight`, …).
    pub fn xft(self) -> &'static str {
        match self {
            HintStyle::None => "hintnone",
            HintStyle::Slight => "hintslight",
            HintStyle::Medium => "hintmedium",
            HintStyle::Full => "hintfull",
        }
    }

    /// Its name in a [`Rendering`] spec (`slight`, …).
    fn word(self) -> &'static str {
        &self.xft()[4..]
    }
}

/// How GTK sets text up: `gtk-font-rendering` and, in manual mode, the
/// low-level settings it then follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rendering {
    /// GTK's manual mode; else its automatic one (grayscale, slight hinting,
    /// unhinted metrics), which ignores the two below.
    pub manual: bool,
    /// `gtk-hint-font-metrics` (GTK's default: on): Cairo hints the metrics
    /// and Pango rounds glyph positions, so every glyph advances a whole
    /// pixel; off, advances are fractional (subpixel positioning).
    pub hint_metrics: bool,
    /// `gtk-xft-hintstyle` (with `gtk-xft-hinting`), or the desktop's
    /// (`None`).
    pub hint_style: Option<HintStyle>,
}

impl Rendering {
    /// What Qrec uses: manual, metrics hinted, the desktop's hinting.
    pub const QREC: Rendering = Rendering {
        manual: true,
        hint_metrics: true,
        hint_style: None,
    };

    /// From `mode=manual|automatic`, `metrics=on|off` and
    /// `hintstyle=desktop|none|slight|medium|full`, comma-separated (a
    /// bare `manual` or `automatic` names the mode); what isn't named is
    /// [`Rendering::QREC`]'s.
    pub fn parse(spec: &str) -> Result<Rendering, String> {
        let mut rendering = Rendering::QREC;
        for item in spec.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            let (key, value) = item.split_once('=').unwrap_or(("mode", item));
            match (key.trim(), value.trim()) {
                ("mode", "manual") => rendering.manual = true,
                ("mode", "automatic") => rendering.manual = false,
                ("metrics", "on") => rendering.hint_metrics = true,
                ("metrics", "off") => rendering.hint_metrics = false,
                ("hintstyle", "desktop") => rendering.hint_style = None,
                ("hintstyle", style) => {
                    rendering.hint_style = Some(match style {
                        "none" => HintStyle::None,
                        "slight" => HintStyle::Slight,
                        "medium" => HintStyle::Medium,
                        "full" => HintStyle::Full,
                        other => return Err(format!("no hint style {other:?}")),
                    });
                }
                _ => return Err(format!("can't read {item:?}")),
            }
        }
        Ok(rendering)
    }

    /// The spec [`Rendering::parse`] reads back.
    pub fn describe(&self) -> String {
        if !self.manual {
            return "mode=automatic".to_owned();
        }
        format!(
            "mode=manual,metrics={},hintstyle={}",
            if self.hint_metrics { "on" } else { "off" },
            self.hint_style.map_or("desktop", HintStyle::word)
        )
    }

    /// Sets it on GTK's settings (before any widget: G§3.3).
    pub fn apply(&self, settings: &gtk::Settings) {
        settings.set_gtk_font_rendering(if self.manual {
            gtk::FontRendering::Manual
        } else {
            gtk::FontRendering::Automatic
        });
        settings.set_gtk_hint_font_metrics(self.hint_metrics);
        if let Some(style) = self.hint_style {
            settings.set_gtk_xft_hinting(i32::from(style != HintStyle::None));
            settings.set_gtk_xft_hintstyle(Some(style.xft()));
        }
    }
}

/// GTK's text settings as they are (the test channel's `fonts`).
pub fn settings_now(settings: &gtk::Settings) -> serde_json::Value {
    let mode = match settings.gtk_font_rendering() {
        gtk::FontRendering::Manual => "manual",
        gtk::FontRendering::Automatic => "automatic",
        _ => "other",
    };
    serde_json::json!({
        "mode": mode,
        "hintMetrics": settings.is_gtk_hint_font_metrics(),
        "hinting": settings.gtk_xft_hinting(),
        "hintStyle": settings.gtk_xft_hintstyle().map(|s| s.to_string()),
        "antialias": settings.gtk_xft_antialias(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rendering_spec_reads_back() {
        assert_eq!(Rendering::parse(""), Ok(Rendering::QREC));
        assert_eq!(Rendering::parse("manual"), Ok(Rendering::QREC));
        let automatic = Rendering::parse("automatic").unwrap();
        assert!(!automatic.manual);
        assert_eq!(automatic.describe(), "mode=automatic");
        let fractional = Rendering::parse("metrics=off, hintstyle=none").unwrap();
        assert_eq!(
            fractional,
            Rendering {
                manual: true,
                hint_metrics: false,
                hint_style: Some(HintStyle::None),
            }
        );
        assert_eq!(
            fractional.describe(),
            "mode=manual,metrics=off,hintstyle=none"
        );
        for spec in [
            Rendering::QREC.describe(),
            "mode=manual,metrics=off,hintstyle=slight".to_owned(),
            "mode=manual,metrics=on,hintstyle=full".to_owned(),
        ] {
            assert_eq!(Rendering::parse(&spec).unwrap().describe(), spec);
        }
        assert_eq!(
            Rendering::QREC.describe(),
            "mode=manual,metrics=on,hintstyle=desktop"
        );
        assert!(Rendering::parse("metrics=maybe").is_err());
        assert!(Rendering::parse("hintstyle=bold").is_err());
        assert!(Rendering::parse("subpixel").is_err());
    }

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
