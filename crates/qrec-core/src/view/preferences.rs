//! The Settings pages besides Project (G§4.11, S21, S22): Appearance,
//! Recording, Autocut and Keyboard, once drawn or waived (G§0.2). Owner:
//! WP9. Stub until WP9 lands: the signatures are final, the bodies return
//! defaults.

use serde::{Deserialize, Serialize};

use crate::config::UserConfig;
use crate::keymap::Keymap;

/// S21: theme, interface size, reduce motion, sounds.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearancePage {
    /// The selected theme's id (`swamp-night`, …).
    pub theme: String,
    /// 100, 125 or 150.
    pub scale_percent: u32,
    pub reduced_motion: bool,
    /// "Also on when your system asks for less motion." (D-30).
    pub reduced_motion_caption: String,
    pub sounds: bool,
}

/// The Appearance page of `config`.
pub fn appearance_page(config: &UserConfig) -> AppearancePage {
    let _ = config;
    AppearancePage::default()
}

/// Settings › Recording (not drawn yet): the user defaults for recording.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingPage {
    pub countdown_secs: u32,
    /// `auto`, `vaapi` or `x264`.
    pub encoder: String,
    /// The VA-API render node; `None` lets Qrec pick (never NVIDIA).
    pub vaapi_device: Option<String>,
}

/// The Recording page of `config`.
pub fn recording_page(config: &UserConfig) -> RecordingPage {
    let _ = config;
    RecordingPage::default()
}

/// Settings › Autocut (not drawn yet): the defaults of new projects.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocutPage {
    pub auto_run_after_take: bool,
    pub threshold_db: f64,
    pub min_silence_ms: u32,
    pub padding_ms: u32,
    pub min_keep_ms: u32,
}

/// The Autocut page of `config`.
pub fn autocut_page(config: &UserConfig) -> AutocutPage {
    let _ = config;
    AutocutPage::default()
}

/// S22: the keyboard page.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardPage {
    /// "1 conflict in ~/.config/qrec/keys.toml" and its lines ("X is bound
    /// to both Autocut and Remove clips. Neither runs until one is
    /// changed.").
    pub banner: Option<(String, Vec<String>)>,
    pub groups: Vec<KeyGroup>,
    /// "Changes are saved to ~/.config/qrec/keys.toml."
    pub footer: String,
}

/// A group of bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyGroup {
    pub title: String,
    pub rows: Vec<KeyRow>,
}

/// One binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyRow {
    pub id: String,
    pub label: String,
    /// Key-cap legends per chord.
    pub keys: Vec<Vec<String>>,
    /// "default C" when rebound.
    pub default_keys: Option<String>,
    /// "Also bound to Remove clips".
    pub conflict: Option<String>,
}

/// The Keyboard page; `keys_path` is `keys.toml` as people read it.
pub fn keyboard_page(keymap: &Keymap, keys_path: &str, search: &str) -> KeyboardPage {
    let _ = (keymap, keys_path, search);
    KeyboardPage::default()
}
