//! Illustrations (G§3.14): SVG templates in `resources/art/` whose
//! `{{token}}` placeholders take the theme's colors, drawn through
//! `gtk::Svg` (GTK 4.22). The quiet swamp is the empty timeline's (S01b,
//! WP3), the signpost the drafted blocking pages' (WP8), the 24 px app icon
//! the header's (WP6).
//!
//! The drawn art is the pen file's (`w7Qg8`: `uH1BD`, `rfUfx`, `W1Ia5`),
//! exported with the pencil MCP while `design/qrec.pen` is the active
//! document (G§0.6). Until that export the files are placeholders of the
//! right size and colors, so the path from template to `gtk::Svg` is
//! ready and tested; the PNG fallback of G§3.14 is only needed if
//! `gtk::Svg` misrenders the real art.

use gtk::{gdk, glib};

use super::Palette;

/// An illustration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Art {
    /// The header's 24 px app icon (`uH1BD`).
    AppIcon24,
    /// The quiet swamp pool with the onion, 200×120 (`rfUfx`).
    QuietSwamp,
    /// The wooden signpost (`W1Ia5`).
    Signpost,
}

impl Art {
    pub const ALL: [Art; 3] = [Art::AppIcon24, Art::QuietSwamp, Art::Signpost];

    /// Its template in the GResource.
    pub fn resource(self) -> &'static str {
        match self {
            Art::AppIcon24 => "/com/rsensato/qrec/art/app-icon-24.svg",
            Art::QuietSwamp => "/com/rsensato/qrec/art/quiet-swamp.svg",
            Art::Signpost => "/com/rsensato/qrec/art/signpost.svg",
        }
    }

    /// Its name (the test channel's `art` op).
    pub fn name(self) -> &'static str {
        match self {
            Art::AppIcon24 => "app-icon-24",
            Art::QuietSwamp => "quiet-swamp",
            Art::Signpost => "signpost",
        }
    }
}

/// `svg` with every `{{token}}` replaced by the palette's color as
/// `#RRGGBB` (and `fill-opacity`-free: alpha tokens come out opaque, so
/// templates use the base tokens). An unknown token stays as it is and is
/// logged.
pub fn substitute(svg: &str, palette: &Palette) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let token = after[..end].trim();
        match palette.color(token) {
            Some(color) => out.push_str(&css_hex(&color)),
            None => {
                tracing::warn!("art: no color token {token}");
                out.push_str(&rest[start..start + 2 + end + 2]);
            }
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// `color` as `#RRGGBB` (the alpha dropped).
pub fn css_hex(color: &gdk::RGBA) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02X}{:02X}{:02X}",
        byte(color.red()),
        byte(color.green()),
        byte(color.blue())
    )
}

/// `which` in `palette`'s colors, as a paintable; `None` when its template
/// is missing from the resources.
pub fn paintable(which: Art, palette: &Palette) -> Option<gtk::Svg> {
    let template = super::resource_text(which.resource())?;
    let svg = substitute(&template, palette);
    let paintable = gtk::Svg::from_bytes(&glib::Bytes::from_owned(svg.into_bytes()));
    paintable.connect_error(crate::guarded!("art: an SVG error", move |_, error| {
        tracing::warn!("art {}: {error}", which.name());
    }));
    Some(paintable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_take_the_theme_colors() {
        let night = Palette::for_theme(qrec_core::config::Theme::SwampNight);
        let svg = r#"<rect fill="{{muted}}" stroke="{{ border }}"/><x a="{{nope}}"/>"#;
        let out = substitute(svg, &night);
        assert_eq!(
            out,
            format!(
                r#"<rect fill="{}" stroke="{}"/><x a="{{{{nope}}}}"/>"#,
                css_hex(&night.muted),
                css_hex(&night.border)
            )
        );
        assert!(!out.contains("{{muted}}"));
        assert_eq!(substitute("no tokens", &night), "no tokens");
        assert_eq!(substitute("open {{muted", &night), "open {{muted");
    }

    #[test]
    fn every_template_is_in_the_resources_folder() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        for art in Art::ALL {
            let relative = art
                .resource()
                .strip_prefix("/com/rsensato/qrec/")
                .expect("under the prefix");
            let text = std::fs::read_to_string(dir.join(relative))
                .unwrap_or_else(|e| panic!("{relative}: {e}"));
            assert!(text.contains("<svg"), "{relative}");
            assert!(text.contains("{{"), "{relative} takes the theme's colors");
        }
    }
}
