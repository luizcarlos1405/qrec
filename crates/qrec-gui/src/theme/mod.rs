//! Theme, scale and motion (G§3.14). The theme, the UI scale and the
//! motion policy come from `config.toml` at startup step 1, before the
//! window maps; a switch later (WP10's config watcher, S21) reloads the
//! tokens, swaps the [`Palette`] and tells subscribers, so custom-drawn
//! widgets redraw (gtk4-rs doesn't expose `css_changed`).
//!
//! Providers: the theme's generated tokens (`resources/theme/tokens/`,
//! priority APPLICATION), then one provider with the libadwaita bridge,
//! the base type roles and the components (APPLICATION + 1), every `px`
//! scaled by `ui.scale` ([`css::scale_px`]). The generated token files and
//! the component CSS come with WP0b part 2; until then the window carries
//! the skeleton's own few rules ([`SKELETON_CSS`]).

pub mod css;
pub mod fonts;
pub mod motion;
pub mod palette;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk;
use qrec_core::config::Theme as ThemeId;

use crate::store::{Subscribers, Subscription};
pub use motion::MotionPolicy;
pub use palette::Palette;

/// The skeleton's rules until the theme pipeline lands (WP0b part 2): stub
/// regions, scrims and the interim pages, in libadwaita's colors.
pub const SKELETON_CSS: &str = "
.qrec-stub { outline: 1px dashed alpha(currentColor, 0.2); outline-offset: -1px; }
.qrec-stub-label { opacity: 0.55; font-size: 13px; }
.scrim { background-color: alpha(black, 0.5); }
.scrim-strong { background-color: alpha(black, 0.72); }
.qrec-blocking { padding: 48px; }
.qrec-blocking .qrec-blocking-title { font-size: 26px; font-weight: 700; }
.qrec-blocking .qrec-blocking-mono {
  font-family: monospace; font-size: 12px; padding: 12px;
  background-color: alpha(currentColor, 0.06); border-radius: 6px;
}
.qrec-toast-stub, .qrec-modal-stub {
  background-color: var(--popover-bg-color); color: var(--popover-fg-color);
  border-radius: 12px; padding: 12px 16px;
  box-shadow: 0 2px 8px alpha(black, 0.4);
}
.qrec-hintbar-stub { min-height: 32px; }
";

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

    /// (Re)loads the providers for the current theme and scale.
    fn reload(&self) {
        let Some(display) = self.inner.display.borrow().clone() else {
            return;
        };
        for old in self.inner.providers.borrow_mut().drain(..) {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        let skeleton = gtk::CssProvider::new();
        skeleton.connect_parsing_error(|_, section, error| {
            tracing::warn!("CSS: {}: {error}", section.to_str());
        });
        skeleton.load_from_string(&css::scale_px(SKELETON_CSS, self.scale()));
        gtk::style_context_add_provider_for_display(
            &display,
            &skeleton,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        self.inner.providers.borrow_mut().push(skeleton);
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
