//! Theme, scale and motion (G§3.14). The theme, the UI scale and the
//! motion policy come from `config.toml` at startup step 1, before the
//! window maps; a switch later (WP10's config watcher, S21) reloads the
//! tokens, swaps the [`Palette`] and tells subscribers, so custom-drawn
//! widgets redraw (gtk4-rs doesn't expose `css_changed`).
//!
//! Providers (all from the GResource, every `px` multiplied by `ui.scale`
//! through [`css::scale_px`]):
//! - the theme's generated tokens, `theme/tokens/<theme>.css`, at
//!   `APPLICATION`;
//! - one provider at `APPLICATION + 1` with `theme/adwaita.css` (the
//!   libadwaita bridge), `theme/base.css` (type roles, focus, disabled,
//!   selection, elevation, take colors) and every file under
//!   `theme/components/`, `common.css` first, the others by name (no
//!   hand-kept list: a work package adds its file and it loads).
//!
//! libadwaita's color scheme follows the theme: Swamp Day forces light,
//! the others dark. CSS parse errors are logged with the file and line.

pub mod art;
pub mod css;
pub mod fonts;
pub mod motion;
pub mod palette;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::{gdk, gio, glib};
use qrec_core::config::Theme as ThemeId;

use crate::store::{Subscribers, Subscription};
pub use motion::MotionPolicy;
pub use palette::Palette;

/// Where the theme's CSS is in the GResource.
pub const CSS_ROOT: &str = "/com/rsensato/qrec/theme";

/// The file name stem of a theme's tokens (`qrec-tokens.json`'s names).
pub fn css_name(id: ThemeId) -> &'static str {
    match id {
        ThemeId::SwampNight => "swamp-night",
        ThemeId::SwampDay => "swamp-day",
        ThemeId::DragonsKeep => "dragons-keep",
        ThemeId::OgreMode => "ogre-mode",
    }
}

/// Whether the theme is light (libadwaita's color scheme follows it).
pub fn is_light(id: ThemeId) -> bool {
    id == ThemeId::SwampDay
}

/// The window's look. Cheap to clone; main thread only.
#[derive(Clone)]
pub struct Theme {
    inner: Rc<Inner>,
}

struct Inner {
    id: Cell<ThemeId>,
    scale: Cell<f64>,
    motion: Cell<MotionPolicy>,
    palette: RefCell<Rc<Palette>>,
    providers: RefCell<Vec<gtk::CssProvider>>,
    display: RefCell<Option<gdk::Display>>,
    changed: Subscribers<Theme>,
}

impl std::fmt::Debug for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Theme")
            .field("id", &self.id())
            .field("scale", &self.scale())
            .field("motion", &self.motion())
            .finish()
    }
}

impl Theme {
    /// The look `config.toml` asks for.
    pub fn new(id: ThemeId, scale: f64, motion: MotionPolicy) -> Self {
        Self {
            inner: Rc::new(Inner {
                id: Cell::new(id),
                scale: Cell::new(sane_scale(scale)),
                motion: Cell::new(motion),
                palette: RefCell::new(Rc::new(Palette::for_theme(id))),
                providers: RefCell::new(Vec::new()),
                display: RefCell::new(None),
                changed: Subscribers::default(),
            }),
        }
    }

    /// Loads the providers on `display` (startup, before the window maps).
    pub fn install(&self, display: &gdk::Display) {
        *self.inner.display.borrow_mut() = Some(display.clone());
        self.reload();
    }

    /// The theme in use.
    pub fn id(&self) -> ThemeId {
        self.inner.id.get()
    }

    /// The UI scale (1, 1.25, 1.5): custom widgets multiply their metrics
    /// by it (D-46).
    pub fn scale(&self) -> f64 {
        self.inner.scale.get()
    }

    /// The motion policy (D-30).
    pub fn motion(&self) -> MotionPolicy {
        self.inner.motion.get()
    }

    /// The theme's colors for drawing.
    pub fn palette(&self) -> Rc<Palette> {
        Rc::clone(&self.inner.palette.borrow())
    }

    /// Follows switches of theme, scale or motion.
    pub fn subscribe(&self, f: impl Fn(&Theme) + 'static) -> Subscription {
        self.inner.changed.add(f)
    }

    /// An illustration in this theme's colors (G§3.14), through `gtk::Svg`.
    pub fn art(&self, which: art::Art) -> Option<gtk::Svg> {
        art::paintable(which, &self.palette())
    }

    /// Switches the theme live (WP10, S21).
    pub fn set_theme(&self, id: ThemeId) {
        if self.id() == id {
            return;
        }
        self.inner.id.set(id);
        *self.inner.palette.borrow_mut() = Rc::new(Palette::for_theme(id));
        self.reload();
        self.inner.changed.emit(self);
    }

    /// Changes the UI scale live (WP10).
    pub fn set_scale(&self, scale: f64) {
        let scale = sane_scale(scale);
        if (self.scale() - scale).abs() < f64::EPSILON {
            return;
        }
        self.inner.scale.set(scale);
        self.reload();
        self.inner.changed.emit(self);
    }

    /// Changes the motion policy live (WP10, the system setting).
    pub fn set_motion(&self, motion: MotionPolicy) {
        if self.motion() == motion {
            return;
        }
        self.inner.motion.set(motion);
        self.inner.changed.emit(self);
    }

    /// (Re)loads the providers for the current theme and scale and sets
    /// libadwaita's color scheme.
    fn reload(&self) {
        let Some(display) = self.inner.display.borrow().clone() else {
            return;
        };
        let scale = self.scale();
        let tokens = Stylesheet::load(&[format!("tokens/{}.css", css_name(self.id()))]);
        let main = Stylesheet::load(&main_files(&component_files()));
        let mut fresh = Vec::new();
        for (sheet, priority) in [
            (tokens, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION),
            (main, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1),
        ] {
            let provider = gtk::CssProvider::new();
            let files = sheet.files.clone();
            provider.connect_parsing_error(crate::guarded!(
                "theme: a CSS error",
                move |_, section, error| {
                    let line = section.start_location().lines();
                    let (file, local) = files.locate(line);
                    tracing::warn!("CSS {file}:{}: {error}", local + 1);
                }
            ));
            provider.load_from_string(&css::scale_px(&sheet.text, scale));
            gtk::style_context_add_provider_for_display(&display, &provider, priority);
            fresh.push(provider);
        }
        // The new providers are in before the old ones go: no frame without
        // a theme.
        let old = std::mem::replace(&mut *self.inner.providers.borrow_mut(), fresh);
        for provider in old {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
        adw::StyleManager::default().set_color_scheme(if is_light(self.id()) {
            adw::ColorScheme::ForceLight
        } else {
            adw::ColorScheme::ForceDark
        });
        tracing::debug!("theme {} at scale {scale} loaded", css_name(self.id()));
    }
}

/// The files under `theme/components/` in the GResource.
fn component_files() -> Vec<String> {
    let dir = format!("{CSS_ROOT}/components/");
    gio::resources_enumerate_children(&dir, gio::ResourceLookupFlags::NONE)
        .map(|names| names.iter().map(|n| n.to_string()).collect())
        .unwrap_or_else(|e| {
            tracing::error!("the component CSS is missing from the resources: {e}");
            Vec::new()
        })
}

/// The main provider's files, in load order: the bridge, the base, then
/// the components with `common.css` first and the rest by name (later
/// rules win among equals, so an owner's file can refine a shared rule).
pub fn main_files(components: &[String]) -> Vec<String> {
    let mut names: Vec<&String> = components.iter().filter(|n| n.ends_with(".css")).collect();
    names.sort_by_key(|name| (name.as_str() != "common.css", name.as_str()));
    ["adwaita.css".to_owned(), "base.css".to_owned()]
        .into_iter()
        .chain(names.into_iter().map(|n| format!("components/{n}")))
        .collect()
}

/// CSS text put together from resource files, with where each starts (for
/// the parse errors).
struct Stylesheet {
    text: String,
    files: FileLines,
}

impl Stylesheet {
    fn load(files: &[String]) -> Self {
        let mut text = String::new();
        let mut starts = Vec::new();
        for file in files {
            let path = format!("{CSS_ROOT}/{file}");
            match gio::resources_lookup_data(&path, gio::ResourceLookupFlags::NONE) {
                Ok(bytes) => {
                    starts.push((text.lines().count(), file.clone()));
                    text.push_str(&String::from_utf8_lossy(&bytes));
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                }
                Err(e) => tracing::error!("the theme file {path} is missing: {e}"),
            }
        }
        Self {
            text,
            files: FileLines(Rc::new(starts)),
        }
    }
}

/// Which file a line of the joined text came from.
#[derive(Clone)]
struct FileLines(Rc<Vec<(usize, String)>>);

impl FileLines {
    /// The file and the line in it of joined line `line` (0-based).
    fn locate(&self, line: usize) -> (String, usize) {
        self.0
            .iter()
            .rev()
            .find(|(start, _)| *start <= line)
            .map_or_else(
                || ("?".to_owned(), line),
                |(start, file)| (file.clone(), line - start),
            )
    }
}

/// `ui.scale` limited to what the CSS can take (1–2).
fn sane_scale(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(1.0, 2.0)
    } else {
        1.0
    }
}

/// Loads a GResource file as text (art templates, the theme's CSS).
pub fn resource_text(path: &str) -> Option<String> {
    gio::resources_lookup_data(path, gio::ResourceLookupFlags::NONE)
        .ok()
        .map(|bytes: glib::Bytes| String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_loads_before_the_owners_files() {
        let components: Vec<String> = [
            "timeline.css",
            "common.css",
            "bin.css",
            "blocking.css",
            "notes.txt",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(
            main_files(&components),
            [
                "adwaita.css",
                "base.css",
                "components/common.css",
                "components/bin.css",
                "components/blocking.css",
                "components/timeline.css",
            ]
        );
    }

    #[test]
    fn parse_errors_name_their_file() {
        let lines = FileLines(Rc::new(vec![
            (0, "adwaita.css".to_owned()),
            (40, "base.css".to_owned()),
            (200, "components/common.css".to_owned()),
        ]));
        assert_eq!(lines.locate(3), ("adwaita.css".to_owned(), 3));
        assert_eq!(lines.locate(40), ("base.css".to_owned(), 0));
        assert_eq!(lines.locate(250), ("components/common.css".to_owned(), 50));
    }

    #[test]
    fn every_theme_has_a_token_file_and_a_scheme() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/theme/tokens");
        for id in [
            ThemeId::SwampNight,
            ThemeId::SwampDay,
            ThemeId::DragonsKeep,
            ThemeId::OgreMode,
        ] {
            let file = dir.join(format!("{}.css", css_name(id)));
            assert!(file.exists(), "{}", file.display());
            assert_eq!(is_light(id), id == ThemeId::SwampDay);
            assert_ne!(Palette::for_theme(id).bg, gdk::RGBA::TRANSPARENT);
        }
    }
}
