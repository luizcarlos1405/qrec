//! Qrec's window (docs/GUI.md): GTK 4 and libadwaita views over the shell's
//! GTK-free GUI host (`qrec_shell::host::gui`), with the pure view models of
//! `qrec_core::view`.
//!
//! - [`run`] is `qrec` without a command (G§3.2): the startup order of G§3.3
//!   (the project lock and socket before GStreamer and the dependency
//!   check), the window, the close sequence (D-12) and the exit code.
//! - State reaches GTK as snapshots through one ordered queue
//!   ([`host::queue`], G§3.5); widgets read it from the [`store::Store`] and
//!   change it only with Commands (R8, SPEC §4.2): nothing else submits
//!   requests to the host.
//! - Keys go through one router ([`keys`], G§3.8): a GTK-free decision
//!   ([`keys::router::decide`]) and its glue.
//! - The look is the theme's tokens ([`theme`], G§3.14): CSS generated from
//!   `design/tokens/qrec-tokens.json`, the libadwaita bridge, the type
//!   roles and the shared components; custom drawing reads the
//!   [`theme::Palette`].
//! - No panic crosses into GLib's C frames ([`guard`], G§3.12): every
//!   source, signal handler and virtual method is guarded, and a test
//!   greps the crate for unguarded ones.
//! - Test mode and its control channel ([`testctl`], G§3.18, G§8.4).
//!
//! Every file of G§3.16 exists with its final public signatures; regions of
//! work packages that haven't landed yet are stubs (a region of the right
//! size showing its name), so no work package edits another's module list.
//! Module owners are in G§3.16.

pub mod app;
pub mod bin_drawer;
pub mod blocking;
pub mod cheatsheet;
pub mod desktop;
pub mod export;
pub mod guard;
pub mod header;
pub mod hintbar;
pub mod host;
pub mod keys;
pub mod media;
pub mod modal;
pub mod palette;
pub mod panel;
pub mod settings;
pub mod stage;
pub mod startup;
pub mod store;
pub mod testctl;
pub mod theme;
pub mod timeline;
pub mod toasts;
pub mod widgets;
pub mod window;

use std::path::PathBuf;

/// How `qrec` without a command was started (G§3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuiArgs {
    /// `--project DIR` made absolute, else the current directory (not
    /// canonical yet: startup step 1 canonicalizes it).
    pub project: PathBuf,
    /// The folder came from `--project`.
    pub explicit: bool,
    /// `-v` count (the log is already set up by the binary).
    pub verbose: u8,
}

/// Runs the window until it closes and returns the process's exit code
/// (G§3.2: 0 closed, 130 a signal, 3 not a project, 5 a missing dependency
/// or GStreamer stuck, 6 the project is held, 1 otherwise).
///
/// Startup step 1 runs here, before GTK (G§3.3): the signal handlers, the
/// folder rule, `config.toml` and `keys.toml`; then steps 3 and 4 start (the
/// lock and the socket come before the window), and the libadwaita
/// application runs on this thread; its `activate` builds the window. `Err`
/// only when no window can open at all (no Wayland display: GDK doesn't
/// fall back to X11).
pub fn run(args: GuiArgs) -> Result<u8, qrec_core::CoreError> {
    app::run(args)
}

/// What every region constructor gets (G§3.6): the shared handles of the
/// window. Cheap to clone.
#[derive(Clone)]
pub struct Ctx {
    /// State, `LocalUi`, dispatch, toasts (G§3.6, G§3.7).
    pub store: store::Store,
    /// The key router: `run_binding` for buttons with a key (G§3.8).
    pub keys: keys::KeyRouter,
    /// GUI actions by id (G§3.8).
    pub actions: keys::actions::ActionRouter,
    /// Pictures, meters, the player and the caches (G§3.9–G§3.11).
    pub media: media::MediaHub,
    /// Palette, scale and motion (G§3.14).
    pub theme: theme::Theme,
    /// The overlay layers of the window (G§3.15).
    pub window: window::layers::WindowLayers,
}
