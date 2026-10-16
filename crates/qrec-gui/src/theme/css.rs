//! CSS preprocessing (G§3.14): the UI scale. Every `<n>px` in the CSS
//! text is multiplied by `ui.scale` (100/125/150 %) before it is loaded,
//! and so is Fraunces' optical size (`"opsz" <n>` in
//! `font-variation-settings`, which follows the font size: "opsz" = px);
//! custom-drawn widgets multiply their metrics by `Theme::scale` (D-46).

/// Fraunces' optical size axis.
const OPSZ: &str = "\"opsz\"";
/// Fraunces' optical sizes.
const OPSZ_RANGE: (f64, f64) = (9.0, 144.0);

/// `css` with every `<number>px` multiplied by `scale` (rounded to 0.01;
/// `0px` and percentages untouched), and every `"opsz" <number>` too
/// (rounded, within Fraunces' 9–144). `scale` 1 returns it as it is.
pub fn scale_px(css: &str, scale: f64) -> String {
    if (scale - 1.0).abs() < f64::EPSILON {
        return css.to_owned();
    }
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len() + css.len() / 8);
    let mut i = 0;
    while i < bytes.len() {
        if css[i..].starts_with(OPSZ) {
            i += scale_opsz(&css[i..], scale, &mut out);
            continue;
        }
        let starts_number =
            (bytes[i].is_ascii_digit() || bytes[i] == b'.') && number_start(bytes, i);
        if starts_number {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let number = &css[start..i];
            let is_px =
                css[i..].starts_with("px") && !bytes.get(i + 2).is_some_and(|b| is_word_byte(*b));
            match number.parse::<f64>() {
                Ok(n) if is_px => {
                    let scaled = (n * scale * 100.0).round() / 100.0;
                    out.push_str(&format_number(scaled));
                    out.push_str("px");
                    i += 2;
                }
                _ => out.push_str(number),
            }
            continue;
        }
        let ch = css[i..].chars().next().expect("in bounds");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// `"opsz" <number>` at the start of `css`, its number scaled, into `out`;
/// how many bytes it took (just the axis name when no number follows).
fn scale_opsz(css: &str, scale: f64, out: &mut String) -> usize {
    out.push_str(OPSZ);
    let after = &css[OPSZ.len()..];
    let gap = after.len() - after.trim_start_matches([' ', '\t']).len();
    let rest = &after[gap..];
    let digits = rest
        .bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b'.')
        .count();
    match rest[..digits].parse::<f64>() {
        Ok(size) if digits > 0 => {
            let scaled = (size * scale).round().clamp(OPSZ_RANGE.0, OPSZ_RANGE.1);
            out.push_str(&after[..gap]);
            out.push_str(&format_number(scaled));
            OPSZ.len() + gap + digits
        }
        _ => OPSZ.len(),
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'#'
}

/// Whether a number may start at `i`: not inside a name (`.e2`,
/// `--qrec-space-2`), but after a minus sign (`-4px`).
fn number_start(bytes: &[u8], i: usize) -> bool {
    match i.checked_sub(1).map(|p| bytes[p]) {
        None => true,
        Some(b'-') => i < 2 || !is_word_byte(bytes[i - 2]),
        Some(prev) => !is_word_byte(prev),
    }
}

fn format_number(n: f64) -> String {
    let text = format!("{n:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_scale_and_nothing_else() {
        let css = "a { min-height: 48px; padding: 0 12px; margin: -4px; border-radius: 0.5px; \
                   opacity: .45; width: 50%; --qrec-space-2: 8px; font-size: 13px; }\n\
                   b { letter-spacing: 0.72px; outline: 2px solid var(--qrec-focus-ring); }";
        assert_eq!(scale_px(css, 1.0), css);
        assert_eq!(
            scale_px(css, 1.5),
            "a { min-height: 72px; padding: 0 18px; margin: -6px; border-radius: 0.75px; \
             opacity: .45; width: 50%; --qrec-space-2: 12px; font-size: 19.5px; }\n\
             b { letter-spacing: 1.08px; outline: 3px solid var(--qrec-focus-ring); }"
        );
        assert_eq!(scale_px("x { margin: 0px; }", 1.25), "x { margin: 0px; }");
        // Names that contain digits or px are left alone.
        assert_eq!(scale_px(".e2 h1px a1 { }", 2.0), ".e2 h1px a1 { }");
        assert_eq!(scale_px("x { width: 10pxs; }", 2.0), "x { width: 10pxs; }");
        assert_eq!(scale_px("— 4px —", 2.0), "— 8px —");
    }

    /// Fraunces' optical size follows the font size, so it scales with the
    /// px (it stayed at its 100 % value: `.t-display` at 48 px kept opsz 32).
    #[test]
    fn the_optical_size_scales_too() {
        let css =
            "a { font-size: 32px; font-variation-settings: \"SOFT\" 50, \"WONK\" 1, \"opsz\" 32; }";
        assert_eq!(
            scale_px(css, 1.5),
            "a { font-size: 48px; font-variation-settings: \"SOFT\" 50, \"WONK\" 1, \"opsz\" 48; }"
        );
        assert_eq!(scale_px(css, 1.0), css);
        assert_eq!(scale_px("\"opsz\" 20;", 1.25), "\"opsz\" 25;");
        assert_eq!(
            scale_px("\"opsz\" 120", 1.5),
            "\"opsz\" 144",
            "Fraunces' largest"
        );
        assert_eq!(scale_px("\"opsz\" x", 2.0), "\"opsz\" x");
        // The real roles keep opsz = px at every scale.
        let base = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/theme/base.css"),
        )
        .unwrap();
        let scaled = scale_px(&base, 1.5);
        let rules: Vec<&str> = scaled
            .split('}')
            .filter(|r| r.contains("font-variation-settings"))
            .collect();
        assert!(rules.len() >= 3, "{rules:?}");
        for rule in rules {
            let px = rule
                .split("font-size: ")
                .nth(1)
                .and_then(|r| r.split("px").next())
                .expect("a font size");
            assert!(rule.contains(&format!("\"opsz\" {px};")), "{rule}");
        }
    }

    /// Every stylesheet in `resources/theme`, concatenated.
    fn all_css() -> String {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut css = String::new();
        let mut dirs = vec![root.join("resources/theme")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "css") {
                    css.push_str(&std::fs::read_to_string(&path).unwrap());
                }
            }
        }
        css
    }

    /// GTK sets `:focus-visible` on the whole focus chain, so a ring keyed
    /// on it alone rings every ancestor of the focused widget too (WP6
    /// undid that in its own CSS): every rule says `:focus:focus-visible`,
    /// which also keeps a field's or a menu item's override as specific as
    /// base.css's ring.
    #[test]
    fn focus_rings_go_on_the_focused_widget_only() {
        let css = all_css();
        let mut offenders = Vec::new();
        for rule in css.split('}') {
            let Some((selectors, _)) = rule.split_once('{') else {
                continue;
            };
            let selectors = match selectors.rfind("*/") {
                Some(end) => &selectors[end + 2..],
                None => selectors,
            };
            for selector in selectors.split(',') {
                let bare = selector.matches(":focus-visible").count();
                let keyed = selector.matches(":focus:focus-visible").count();
                if bare != keyed {
                    offenders.push(selector.trim().to_owned());
                }
            }
        }
        assert!(offenders.is_empty(), "{offenders:#?}");
        assert!(css.contains("\n:focus:focus-visible {"), "base.css's ring");
    }

    /// The classes a stylesheet gives `border-spacing` to (in a selector's
    /// last compound).
    fn spaced_classes(css: &str) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for rule in css.split('}') {
            let Some((selectors, body)) = rule.split_once('{') else {
                continue;
            };
            if !body.contains("border-spacing") {
                continue;
            }
            let selectors = match selectors.rfind("*/") {
                Some(end) => &selectors[end + 2..],
                None => selectors,
            };
            for selector in selectors.split(',') {
                let Some(last) = selector.split_whitespace().last() else {
                    continue;
                };
                for class in last.split('.').skip(1) {
                    let class: String = class
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                        .collect();
                    if !class.is_empty() {
                        out.insert(class);
                    }
                }
            }
        }
        out
    }

    /// GtkBox adds CSS `border-spacing` to its own spacing: a box the CSS
    /// spaces (by its class, or as a `.qrec-btn`'s content) is made with 0,
    /// or the gap doubles (WP6 found the buttons' 16 px and the chords'
    /// 4 px, and worked around them in its own CSS).
    #[test]
    fn boxes_the_css_spaces_have_no_spacing_of_their_own() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let classes = spaced_classes(&all_css());
        assert!(
            classes.contains("keycaps") && classes.contains("chip"),
            "{classes:?}"
        );
        let mut offenders = Vec::new();
        let mut dirs = vec![root.join("src")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let lines: Vec<&str> = text.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    // `let name = gtk::Box::new(…, N)` with N ≠ 0.
                    let Some((head, args)) = line.split_once("gtk::Box::new(") else {
                        continue;
                    };
                    let Some(name) = head
                        .trim()
                        .strip_prefix("let ")
                        .map(|h| h.trim_start_matches("mut ").trim_end_matches(['=', ' ']))
                    else {
                        continue;
                    };
                    let spacing = args
                        .rsplit_once(',')
                        .map(|(_, s)| s.trim_end_matches([')', ';', ' ']).trim());
                    if spacing.is_none_or(|s| s == "0") {
                        continue;
                    }
                    let later = &lines[i + 1..lines.len().min(i + 12)];
                    let styled = later.iter().any(|l| {
                        classes
                            .iter()
                            .any(|c| l.contains(&format!("{name}.add_css_class(\"{c}\")")))
                    });
                    // A content box of a `.qrec-btn` (`button.qrec-btn > box`).
                    // (A content box is set as the child once it is filled.)
                    let button_child = lines[i + 1..lines.len().min(i + 30)]
                        .iter()
                        .any(|l| l.contains(&format!(".set_child(Some(&{name}))")))
                        && lines[i.saturating_sub(12)..i].iter().any(|l| {
                            l.contains("add_css_class(\"qrec-btn\")") || l.contains("= base(kind)")
                        });
                    if styled || button_child {
                        offenders.push(format!(
                            "{}:{}: {}",
                            path.strip_prefix(root).unwrap().display(),
                            i + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "doubled box spacing: {offenders:#?}");
    }
}
