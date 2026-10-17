//! The libadwaita bridge (G§3.14, `resources/theme/adwaita.css`): every
//! libadwaita color variable a widget of the window reads is one of the
//! theme's tokens, so no libadwaita default shows in any theme. GTK-free:
//! it reads the stylesheet and the generated token files.
//!
//! WP10 found the gap in Swamp Day: the narrow layout's sheet (the split
//! view collapsed) dimmed the content and cast its edge shadow in
//! libadwaita's own neutral `rgb(0 0 6 / 7%)` (25 % in the dark themes),
//! not the theme's shadow (`gui_themes`' `the_narrow_sheet_shades_in_the_
//! themes_tokens` shoots it). libadwaita 1.9 reads the variables below
//! for the widgets Qrec shows; those it reads only for widgets Qrec
//! doesn't use (banners, tab overviews, boxed lists, thumbnails, the file
//! chooser) are left out.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const THEMES: [&str; 4] = ["swamp-night", "swamp-day", "dragons-keep", "ogre-mode"];

/// The libadwaita variables the bridge maps, with what reads them.
const BRIDGED: &[(&str, &str)] = &[
    ("--window-bg-color", "the window's ground"),
    ("--window-fg-color", "the window's text"),
    ("--view-bg-color", "entries, text views (the field well)"),
    ("--view-fg-color", "entries, text views"),
    ("--headerbar-bg-color", "the header bar"),
    (
        "--headerbar-backdrop-color",
        "the header bar of an inactive window",
    ),
    ("--headerbar-fg-color", "the header bar's text"),
    ("--headerbar-border-color", "the header bar's edges"),
    (
        "--headerbar-shade-color",
        "the header bar's bottom shade (off)",
    ),
    ("--popover-bg-color", "menus, dropdowns, popovers"),
    ("--popover-fg-color", "menus, dropdowns, popovers"),
    ("--dialog-bg-color", "dialogs"),
    ("--dialog-fg-color", "dialogs"),
    ("--card-bg-color", "cards"),
    ("--card-fg-color", "cards"),
    ("--sidebar-bg-color", "the panel, the split view's sidebar"),
    ("--sidebar-fg-color", "the panel's text"),
    (
        "--sidebar-backdrop-color",
        "the panel of an inactive window",
    ),
    ("--sidebar-border-color", "the panel's edge line"),
    (
        "--sidebar-shade-color",
        "shades inside the panel (undershoots)",
    ),
    ("--accent-bg-color", "switches, checks, selections"),
    ("--accent-fg-color", "text on accent fills"),
    (
        "--accent-color",
        "accent text and icons (the system accent)",
    ),
    ("--destructive-bg-color", "destructive fills"),
    ("--destructive-fg-color", "text on destructive fills"),
    ("--destructive-color", "destructive text"),
    ("--success-color", "success icons"),
    ("--warning-color", "warning icons"),
    ("--error-color", "error text and icons, invalid entries"),
    ("--border-color", "separators, outlines"),
    (
        "--shade-color",
        "the narrow sheet's dimming and edge shadow, undershoots",
    ),
    (
        "--popover-shade-color",
        "shades inside popovers (undershoots)",
    ),
    (
        "--active-toggle-bg-color",
        "a checked segment (C28), disabled too",
    ),
    (
        "--active-toggle-fg-color",
        "a checked segment's label (C28)",
    ),
    (
        "--scrollbar-outline-color",
        "the outline of an overlay scrollbar",
    ),
];

/// Values that aren't colors of a token but are meant: the header bar's
/// shade is off (G§3.14).
const NOT_A_TOKEN: &[(&str, &str)] = &[("--headerbar-shade-color", "transparent")];

fn theme_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/theme")
}

/// `name: value;` pairs of the bridge's `:root` block.
fn bridge() -> BTreeMap<String, String> {
    let path = theme_dir().join("adwaita.css");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let start = text.find(":root {").expect("the bridge's :root block");
    let end = start + text[start..].find('}').expect("its end");
    let mut vars = BTreeMap::new();
    let mut in_comment = false;
    for line in text[start + ":root {".len()..end].lines() {
        let line = line.trim();
        if in_comment || line.starts_with("/*") {
            in_comment = !line.contains("*/");
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            vars.insert(
                name.trim().to_owned(),
                value.trim().trim_end_matches(';').trim().to_owned(),
            );
        }
    }
    vars
}

/// The color tokens of `theme`'s generated CSS.
fn tokens(theme: &str) -> Vec<String> {
    let path = theme_dir().join(format!("tokens/{theme}.css"));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .filter_map(|l| {
            let (name, value) = l.trim().strip_prefix("--qrec-")?.split_once(':')?;
            value.trim().starts_with('#').then(|| name.to_owned())
        })
        .collect()
}

#[test]
fn every_libadwaita_color_the_window_reads_is_a_token() {
    let bridge = bridge();
    let mut wrong = Vec::new();
    for (name, what) in BRIDGED {
        let Some(value) = bridge.get(*name) else {
            wrong.push(format!(
                "{name} ({what}) isn't mapped: libadwaita's default shows"
            ));
            continue;
        };
        if NOT_A_TOKEN.contains(&(*name, value.as_str())) {
            continue;
        }
        let token = value
            .strip_prefix("var(--qrec-")
            .and_then(|rest| rest.strip_suffix(')'));
        match token {
            Some(token) => {
                for theme in THEMES {
                    if !tokens(theme).iter().any(|t| t == token) {
                        wrong.push(format!("{name}: {theme} has no color token {token}"));
                    }
                }
            }
            None => wrong.push(format!("{name} ({what}) is {value}, not a token")),
        }
    }
    assert!(
        wrong.is_empty(),
        "the libadwaita bridge:\n{}",
        wrong.join("\n")
    );
}

/// The parser reads the block as written: names, values, comments left out.
#[test]
fn the_bridge_parses() {
    let bridge = bridge();
    assert_eq!(bridge["--window-bg-color"], "var(--qrec-bg)");
    assert_eq!(bridge["--disabled-opacity"], "45%");
    assert!(bridge.keys().all(|k| k.starts_with("--")), "{bridge:?}");
}
