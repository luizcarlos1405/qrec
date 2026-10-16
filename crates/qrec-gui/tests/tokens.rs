//! The theme tokens (G§3.14): generated from `design/tokens/qrec-tokens.json`
//! and committed, because the Nix build source has no `design/`.
//!
//! - `resources/theme/tokens/<theme>.css`: `:root { --qrec-<token>: …; }`
//!   with every color, every number in px and the three font names.
//! - `src/theme/palette.rs`: one `Palette` per theme for custom-drawn
//!   widgets (GTK can't look a CSS custom property up from code).
//!
//! This test is the generator: it renders both in memory and fails when the
//! committed files differ (`QREC_UPDATE_TOKENS=1` writes them instead). It
//! is skipped when `design/` is absent. It also holds the rule that colors
//! live only in tokens: no hex color in the component CSS, the bridge, the
//! base CSS or in `src/` outside `theme/palette.rs`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tokens_json() -> Option<PathBuf> {
    let path = crate_dir().join("../../design/tokens/qrec-tokens.json");
    path.exists().then_some(path)
}

/// The tokens as the JSON gives them.
struct Tokens {
    themes: Vec<String>,
    /// token → theme → `#RRGGBB` or `#RRGGBBAA`, in the JSON's order.
    colors: Vec<(String, BTreeMap<String, String>)>,
    numbers: Vec<(String, f64)>,
    strings: Vec<(String, String)>,
}

fn read_tokens(path: &Path) -> Tokens {
    let text = fs::read_to_string(path).expect("read the tokens");
    let json: Value = serde_json::from_str(&text).expect("the tokens are JSON");
    let themes: Vec<String> = json["themes"]
        .as_array()
        .expect("themes")
        .iter()
        .map(|t| t.as_str().expect("a theme name").to_owned())
        .collect();
    let object = |key: &str| json[key].as_object().expect(key).clone();
    let colors = object("colors")
        .into_iter()
        .map(|(name, per_theme)| {
            let values: BTreeMap<String, String> = per_theme
                .as_object()
                .expect("a color per theme")
                .iter()
                .map(|(theme, v)| (theme.clone(), v.as_str().expect("a color").to_owned()))
                .collect();
            for theme in &themes {
                let value = values
                    .get(theme)
                    .unwrap_or_else(|| panic!("{name} has no {theme} value"));
                assert!(
                    is_hex_color(value),
                    "{name} in {theme} isn't #RRGGBB or #RRGGBBAA: {value}"
                );
            }
            (name, values)
        })
        .collect();
    let numbers = object("numbers")
        .into_iter()
        .map(|(name, v)| (name, v.as_f64().expect("a number")))
        .collect();
    let strings = object("strings")
        .into_iter()
        .map(|(name, v)| (name, v.as_str().expect("a string").to_owned()))
        .collect();
    Tokens {
        themes,
        colors,
        numbers,
        strings,
    }
}

fn is_hex_color(value: &str) -> bool {
    value
        .strip_prefix('#')
        .is_some_and(|hex| matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The eight take colors in cycle order (`view::format::take_color`).
const TAKES: [&str; 8] = [
    "moss", "donkey", "onion", "mud", "lily", "ginger", "dragon", "fairy",
];

const HEADER: &str = "Generated from design/tokens/qrec-tokens.json by \
crates/qrec-gui/tests/tokens.rs (QREC_UPDATE_TOKENS=1 cargo test -p qrec-gui --test tokens). \
Do not edit: change the variable in design/qrec.pen and regenerate.";

fn token_css(tokens: &Tokens, theme: &str) -> String {
    let mut out = String::new();
    writeln!(out, "/* {HEADER} */").unwrap();
    writeln!(out, "/* Theme: {theme}. */").unwrap();
    out.push_str(":root {\n");
    for (name, values) in &tokens.colors {
        writeln!(out, "  --qrec-{name}: {};", values[theme]).unwrap();
    }
    for (name, value) in &tokens.numbers {
        writeln!(out, "  --qrec-{name}: {}px;", trim_number(*value)).unwrap();
    }
    for (name, value) in &tokens.strings {
        writeln!(out, "  --qrec-{name}: \"{value}\";").unwrap();
    }
    out.push_str("}\n");
    out
}

fn trim_number(n: f64) -> String {
    let text = format!("{n}");
    text.strip_suffix(".0").map_or(text.clone(), str::to_owned)
}

fn rust_ident(token: &str) -> String {
    token.replace('-', "_")
}

fn const_name(theme: &str) -> String {
    theme.replace('-', "_").to_uppercase()
}

fn variant(theme: &str) -> String {
    theme
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn rgba_call(hex: &str) -> String {
    let hex = &hex[1..];
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex");
    let alpha = if hex.len() == 8 { byte(6) } else { 255 };
    format!("rgba({}, {}, {}, {})", byte(0), byte(2), byte(4), alpha)
}

fn palette_rs(tokens: &Tokens) -> String {
    let mut out = String::new();
    out.push_str(
        "//! The theme palettes as `gdk::RGBA`s for custom-drawn widgets (G§3.14):\n\
         //! GTK can't look a CSS custom property up from code. One [`Palette`] per\n\
         //! theme with every color token, plus the eight take colors and their\n\
         //! tints in cycle order (`view::format::take_color` indexes them); the\n\
         //! only file in `src/` that holds color values.\n//!\n",
    );
    for line in textwrap(HEADER, 74) {
        writeln!(out, "//! {line}").unwrap();
    }
    out.push_str(
        "\nuse gtk::gdk;\nuse qrec_core::config::Theme as ThemeId;\n\n\
         /// A theme's colors for drawing: one field per color token (`-` → `_`).\n\
         #[derive(Debug, Clone, PartialEq)]\npub struct Palette {\n",
    );
    for (name, _) in &tokens.colors {
        writeln!(out, "    pub {}: gdk::RGBA,", rust_ident(name)).unwrap();
    }
    out.push_str(
        "    /// The eight take colors: moss, donkey, onion, mud, lily, ginger,\n    \
         /// dragon, fairy (`view::format::take_color` indexes them).\n    \
         pub take: [gdk::RGBA; 8],\n    \
         /// Their tints (the take color at 12 %: audio lane segments).\n    \
         pub take_tint: [gdk::RGBA; 8],\n}\n\n",
    );
    out.push_str(
        "impl Default for Palette {\n    fn default() -> Self {\n        SWAMP_NIGHT\n    }\n}\n\n\
         impl Palette {\n    /// The palette of theme `id`.\n    \
         pub fn for_theme(id: ThemeId) -> Palette {\n        match id {\n",
    );
    for theme in &tokens.themes {
        writeln!(
            out,
            "            ThemeId::{} => {},",
            variant(theme),
            const_name(theme)
        )
        .unwrap();
    }
    out.push_str("        }\n    }\n\n");
    out.push_str(
        "    /// The color of token `name` (as `qrec-tokens.json` names it), for\n    \
         /// templates that take token names (the art).\n    \
         pub fn color(&self, name: &str) -> Option<gdk::RGBA> {\n        \
         Some(match name {\n",
    );
    for (name, _) in &tokens.colors {
        writeln!(out, "            \"{name}\" => self.{},", rust_ident(name)).unwrap();
    }
    out.push_str("            _ => return None,\n        })\n    }\n}\n\n");
    out.push_str(
        "/// A color from its 8-bit channels.\nconst fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> gdk::RGBA {\n    \
         gdk::RGBA::new(\n        red as f32 / 255.0,\n        green as f32 / 255.0,\n        \
         blue as f32 / 255.0,\n        alpha as f32 / 255.0,\n    )\n}\n",
    );
    let color = |theme: &str, token: &str| -> String {
        let values = &tokens
            .colors
            .iter()
            .find(|(name, _)| name == token)
            .unwrap_or_else(|| panic!("no token {token}"))
            .1;
        rgba_call(&values[theme])
    };
    for theme in &tokens.themes {
        writeln!(out, "\n/// {}.", variant_title(theme)).unwrap();
        writeln!(out, "pub const {}: Palette = Palette {{", const_name(theme)).unwrap();
        for (name, values) in &tokens.colors {
            writeln!(
                out,
                "    {}: {},",
                rust_ident(name),
                rgba_call(&values[theme])
            )
            .unwrap();
        }
        out.push_str("    take: [\n");
        for take in TAKES {
            writeln!(out, "        {},", color(theme, &format!("take-{take}"))).unwrap();
        }
        out.push_str("    ],\n    take_tint: [\n");
        for take in TAKES {
            writeln!(
                out,
                "        {},",
                color(theme, &format!("take-{take}-tint"))
            )
            .unwrap();
        }
        out.push_str("    ],\n};\n");
    }
    out
}

fn variant_title(theme: &str) -> String {
    match theme {
        "swamp-night" => "Swamp Night (the default)".to_owned(),
        "swamp-day" => "Swamp Day".to_owned(),
        "dragons-keep" => "Dragon's Keep".to_owned(),
        "ogre-mode" => "Ogre Mode".to_owned(),
        other => other.to_owned(),
    }
}

fn textwrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Writes `content` to `path` under `QREC_UPDATE_TOKENS=1`, else returns
/// whether the committed file differs.
fn check_or_write(path: &Path, content: &str, stale: &mut Vec<String>) {
    let update = std::env::var("QREC_UPDATE_TOKENS").is_ok_and(|v| v == "1");
    let current = fs::read_to_string(path).unwrap_or_default();
    if current == content {
        return;
    }
    if update {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).expect("create the tokens folder");
        }
        fs::write(path, content).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        eprintln!("wrote {}", path.display());
    } else {
        stale.push(path.display().to_string());
    }
}

#[test]
fn the_generated_files_match_the_tokens() {
    let Some(json) = tokens_json() else {
        eprintln!("design/ is absent: the token files aren't checked");
        return;
    };
    let tokens = read_tokens(&json);
    assert_eq!(tokens.themes.len(), 4, "four themes (SPEC §15.2)");
    assert_eq!(tokens.colors.len(), 75, "75 color tokens (G§3.14)");
    assert_eq!(tokens.numbers.len(), 15, "15 numbers (G§3.14)");
    assert_eq!(tokens.strings.len(), 3, "3 font names (G§3.14)");
    let mut stale = Vec::new();
    let dir = crate_dir().join("resources/theme/tokens");
    for theme in &tokens.themes {
        check_or_write(
            &dir.join(format!("{theme}.css")),
            &token_css(&tokens, theme),
            &mut stale,
        );
    }
    check_or_write(
        &crate_dir().join("src/theme/palette.rs"),
        &palette_rs(&tokens),
        &mut stale,
    );
    assert!(
        stale.is_empty(),
        "generated files out of date (QREC_UPDATE_TOKENS=1 cargo test -p qrec-gui --test tokens \
         rewrites them): {stale:?}"
    );
}

/// `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` anywhere in `text`, with the
/// line number: what a color written by hand looks like.
fn hex_colors(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'#' {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                    end += 1;
                }
                let len = end - start;
                let word_ends = bytes
                    .get(end)
                    .is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-'));
                if matches!(len, 3 | 4 | 6 | 8) && word_ends {
                    found.push((n + 1, line[i..end].to_owned()));
                }
                i = end.max(i + 1);
            } else {
                i += 1;
            }
        }
    }
    found
}

fn files_under(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

#[test]
fn colors_live_only_in_the_tokens() {
    let theme = crate_dir().join("resources/theme");
    let mut files = Vec::new();
    files_under(&theme.join("components"), "css", &mut files);
    files.push(theme.join("adwaita.css"));
    files.push(theme.join("base.css"));
    let mut sources = Vec::new();
    files_under(&crate_dir().join("src"), "rs", &mut sources);
    let palette = crate_dir().join("src/theme/palette.rs");
    files.extend(sources.into_iter().filter(|p| *p != palette));
    let mut offenders = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for (line, color) in hex_colors(&text) {
            offenders.push(format!(
                "{}:{line}: {color}",
                file.strip_prefix(crate_dir()).unwrap_or(file).display()
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "colors belong in design/tokens (G§3.14): {offenders:#?}"
    );
}

#[test]
fn the_hex_finder_finds_colors_and_nothing_else() {
    let found = hex_colors("a { color: #12170F; background: #fff; } /* #region-header, #1a */");
    let colors: Vec<&str> = found.iter().map(|(_, c)| c.as_str()).collect();
    assert_eq!(colors, ["#12170F", "#fff"]);
    assert!(hex_colors("window#qrec, #region-panel").is_empty());
    assert_eq!(hex_colors("x\n#E5483B73")[0], (2, "#E5483B73".to_owned()));
}
