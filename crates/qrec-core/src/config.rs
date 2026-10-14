//! User config `~/.config/qrec/config.toml` (A§9.8, SPEC §6.4). Owner: meta.
//!
//! The types are final (they are part of `Facts`); `get`/`set` are
//! signatures only. Keys are snake_case; every field has a default, so a
//! missing file or section means the defaults.
#![allow(clippy::todo, unused_variables)]

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::CoreError;
use crate::model::{
    AutocutSettings, Canvas, Color, EncoderPref, Fps, ProjectDefaults, RecordingSettings,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserConfig {
    pub theme: Theme,
    pub canvas: CanvasDefaults,
    pub recording: RecordingDefaults,
    pub autocut: AutocutDefaults,
    pub encoder: EncoderDefaults,
    pub ui: UiDefaults,
}

impl UserConfig {
    /// What `Init` starts a project from (A§2.1). Never copies a VA device
    /// into the project.
    pub fn project_defaults(&self) -> ProjectDefaults {
        ProjectDefaults {
            canvas: Canvas {
                width: self.canvas.width,
                height: self.canvas.height,
                fps: self.canvas.fps,
                background: self.canvas.background.clone(),
            },
            recording: RecordingSettings {
                countdown_secs: self.recording.countdown_secs,
                encoder: EncoderPref::Auto,
                vaapi_device: None,
            },
            autocut: AutocutSettings {
                auto_run_after_take: self.autocut.auto_run_after_take,
                track: None,
                threshold_db: self.autocut.threshold_db,
                min_silence_ms: self.autocut.min_silence_ms,
                padding_ms: self.autocut.padding_ms,
                min_keep_ms: self.autocut.min_keep_ms,
            },
        }
    }
}

/// SPEC §15.2; the default is Swamp Night.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    SwampNight,
    SwampDay,
    DragonsKeep,
    OgreMode,
}

/// `[canvas]`: the canvas of new projects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CanvasDefaults {
    pub width: u32,
    pub height: u32,
    /// `"30"`, `"29.97"` or `"30000/1001"` (a bare integer works too).
    #[serde(with = "fps_text")]
    pub fps: Fps,
    pub background: Color,
}

impl Default for CanvasDefaults {
    fn default() -> Self {
        let canvas = Canvas::default();
        Self {
            width: canvas.width,
            height: canvas.height,
            fps: canvas.fps,
            background: canvas.background,
        }
    }
}

/// `[recording]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordingDefaults {
    /// 0 to 10.
    pub countdown_secs: u32,
}

/// `[autocut]`: the autocut settings of new projects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutocutDefaults {
    pub auto_run_after_take: bool,
    pub threshold_db: f64,
    pub min_silence_ms: u32,
    pub padding_ms: u32,
    pub min_keep_ms: u32,
}

impl Default for AutocutDefaults {
    fn default() -> Self {
        let a = AutocutSettings::default();
        Self {
            auto_run_after_take: a.auto_run_after_take,
            threshold_db: a.threshold_db,
            min_silence_ms: a.min_silence_ms,
            padding_ms: a.padding_ms,
            min_keep_ms: a.min_keep_ms,
        }
    }
}

/// `[encoder]`: used when a project says `auto` / `null` (A§2.1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EncoderDefaults {
    pub preference: EncoderPref,
    /// A render node such as `/dev/dri/renderD129`.
    pub vaapi_device: Option<String>,
}

/// `[ui]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiDefaults {
    pub scale: f64,
    pub reduced_motion: bool,
    /// Off by default (SPEC §15.3).
    pub sounds: bool,
}

impl Default for UiDefaults {
    fn default() -> Self {
        Self {
            scale: 1.0,
            reduced_motion: false,
            sounds: false,
        }
    }
}

/// The value at a dotted key (`canvas.fps`, `theme`); unknown keys are `Usage`.
pub fn get(cfg: &UserConfig, key: &str) -> Result<toml::Value, CoreError> {
    todo!("meta: config::get")
}

/// Sets a dotted key in the document (comments and order survive); the
/// result is validated by re-parsing it as `UserConfig`.
pub fn set(doc: &mut toml_edit::DocumentMut, key: &str, value: &str) -> Result<(), CoreError> {
    todo!("meta: config::set")
}

/// `Fps` as TOML text (`"30"`, `"30000/1001"`); integers are accepted too.
mod fps_text {
    use super::*;

    pub fn serialize<S: Serializer>(fps: &Fps, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(fps)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Fps, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(u32),
            Text(String),
        }
        let text = match Raw::deserialize(d)? {
            Raw::Int(n) => n.to_string(),
            Raw::Text(s) => s,
        };
        text.parse()
            .map_err(|e: CoreError| serde::de::Error::custom(e.message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_parsing() {
        let cfg: UserConfig = toml::from_str("").unwrap();
        assert_eq!(cfg, UserConfig::default());
        assert_eq!(cfg.canvas.fps, Fps::new(30, 1));
        assert_eq!(cfg.autocut.threshold_db, -35.0);
        assert!(!cfg.ui.sounds);

        let cfg: UserConfig = toml::from_str(
            "theme = \"dragons-keep\"\n[canvas]\nfps = \"30000/1001\"\nbackground = \"#12170f\"\n[ui]\nscale = 1.5\n",
        )
        .unwrap();
        assert_eq!(cfg.theme, Theme::DragonsKeep);
        assert_eq!(cfg.canvas.fps, Fps::new(30000, 1001));
        assert_eq!(cfg.canvas.width, 1920);
        let cfg: UserConfig = toml::from_str("[canvas]\nfps = 25\n").unwrap();
        assert_eq!(cfg.canvas.fps, Fps::new(25, 1));
        assert!(toml::from_str::<UserConfig>("[canvas]\nfsp = 25\n").is_err());

        let defaults = cfg.project_defaults();
        assert_eq!(defaults.canvas.fps, Fps::new(25, 1));
        assert_eq!(defaults.recording.vaapi_device, None);
    }
}
