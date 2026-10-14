//! User config `~/.config/qrec/config.toml` (A§9.8, SPEC §6.4). Owner: meta.
//!
//! Keys are snake_case; every field has a default, so a missing file or
//! section means the defaults. [`parse`] reads the file and checks the
//! ranges (the same ones project validation enforces, since `Init` copies
//! these defaults into new projects); [`get`], [`get_json`] and [`list`]
//! answer `qrec config [get]`; [`set`] edits the document with `toml_edit`,
//! so comments, order and formatting survive, and validates the result by
//! re-parsing it. The shell only reads and atomically writes the file.
//! [`KEYS`] lists every setting with the values it takes.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value as Json, json};

use crate::error::CoreError;
use crate::keymap::toml_error;
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

// ------------------------------------------------------------------ keys

/// What a setting holds.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Theme,
    Int { min: i64, max: i64, even: bool },
    Float { min: f64, max: f64 },
    Bool,
    Fps,
    Color,
    Encoder,
    /// A render node path; `auto` unsets it.
    Device,
}

/// One setting of `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfigKey {
    /// The dotted key: `canvas.fps`.
    pub key: &'static str,
    /// The values it takes, as a phrase: "an integer from 0 to 10".
    pub values: &'static str,
    pub help: &'static str,
    kind: Kind,
}

const fn key(key: &'static str, kind: Kind, values: &'static str, help: &'static str) -> ConfigKey {
    ConfigKey {
        key,
        values,
        help,
        kind,
    }
}

const MS: Kind = Kind::Int {
    min: 0,
    max: 60_000,
    even: false,
};
const SIZE: Kind = Kind::Int {
    min: 16,
    max: 8192,
    even: true,
};

/// Every setting, in file order.
pub const KEYS: &[ConfigKey] = &[
    key("theme", Kind::Theme, "swamp-night, swamp-day, dragons-keep or ogre-mode", "The color theme."),
    key("canvas.width", SIZE, "an even integer from 16 to 8192", "Canvas width of new projects."),
    key("canvas.height", SIZE, "an even integer from 16 to 8192", "Canvas height of new projects."),
    key("canvas.fps", Kind::Fps, "a frame rate such as 30, 29.97 or 30000/1001 (at most 240)", "Frame rate of new projects."),
    key("canvas.background", Kind::Color, "a #RRGGBB color", "Canvas background of new projects."),
    key("recording.countdown_secs", Kind::Int { min: 0, max: 10, even: false }, "an integer from 0 to 10", "Countdown before each take, in new projects."),
    key("autocut.auto_run_after_take", Kind::Bool, "true or false", "Autocut every new take, in new projects."),
    key("autocut.threshold_db", Kind::Float { min: -90.0, max: 0.0 }, "a number from -90 to 0", "Silence threshold in dB."),
    key("autocut.min_silence_ms", MS, "an integer from 0 to 60000", "Only silences at least this long are removed."),
    key("autocut.padding_ms", MS, "an integer from 0 to 60000", "Audio kept on each side of speech."),
    key("autocut.min_keep_ms", MS, "an integer from 0 to 60000", "Kept fragments shorter than this are dropped."),
    key("encoder.preference", Kind::Encoder, "auto, vaapi or x264", "H.264 encoder when a project says auto."),
    key("encoder.vaapi_device", Kind::Device, "a render node such as /dev/dri/renderD129, or auto", "VA-API node when a project names none; auto = the first Intel or AMD node, never NVIDIA."),
    key("ui.scale", Kind::Float { min: 0.5, max: 3.0 }, "a number from 0.5 to 3", "Interface scale."),
    key("ui.reduced_motion", Kind::Bool, "true or false", "Reduce animations."),
    key("ui.sounds", Kind::Bool, "true or false", "A squelch on record start and stop."),
];

fn find_key(key: &str) -> Option<&'static ConfigKey> {
    KEYS.iter().find(|k| k.key == key)
}

fn unknown_key(key: &str) -> CoreError {
    let known: Vec<&str> = KEYS.iter().map(|k| k.key).collect();
    CoreError::usage(format!("unknown config key `{key}`"))
        .with_hint(format!("known keys: {}", known.join(", ")))
}

impl ConfigKey {
    /// Checks a value (as JSON) against the key's range; the message names the key.
    fn check(&self, value: &Json) -> Result<(), String> {
        let bad = || format!("{} must be {} (got {value})", self.key, self.values);
        match self.kind {
            Kind::Int { min, max, even } => {
                let n = value.as_i64().ok_or_else(bad)?;
                if n < min || n > max || (even && n % 2 != 0) {
                    return Err(bad());
                }
            }
            Kind::Float { min, max } => {
                let x = value.as_f64().ok_or_else(bad)?;
                if !(x.is_finite() && min <= x && x <= max) {
                    return Err(bad());
                }
            }
            Kind::Fps => {
                let text = value.as_str().ok_or_else(bad)?;
                text.parse::<Fps>().map_err(|_| bad())?;
            }
            Kind::Device => {
                if value.as_str().is_some_and(|path| !path.starts_with('/')) {
                    return Err(bad());
                }
            }
            Kind::Theme | Kind::Bool | Kind::Color | Kind::Encoder => {}
        }
        Ok(())
    }

    /// The TOML value for a command-line value; `None` unsets the key.
    fn parse_value(&self, text: &str) -> Result<Option<toml_edit::Value>, CoreError> {
        let t = text.trim();
        let bad = || {
            CoreError::usage(format!("{} must be {} (got {t:?})", self.key, self.values))
                .with_hint(self.help)
        };
        let checked = |value: Json| self.check(&value).map_err(|_| bad());
        Ok(Some(match self.kind {
            Kind::Theme => {
                serde_json::from_value::<Theme>(Json::from(t)).map_err(|_| bad())?;
                t.into()
            }
            Kind::Int { .. } => {
                let n: i64 = t.parse().map_err(|_| bad())?;
                checked(Json::from(n))?;
                n.into()
            }
            Kind::Float { .. } => {
                let x: f64 = t.parse().map_err(|_| bad())?;
                checked(json!(x))?;
                x.into()
            }
            Kind::Bool => match t {
                "true" => true.into(),
                "false" => false.into(),
                _ => return Err(bad()),
            },
            Kind::Fps => t.parse::<Fps>().map_err(|_| bad())?.to_string().into(),
            Kind::Color => t.parse::<Color>().map_err(|_| bad())?.as_str().into(),
            Kind::Encoder => {
                serde_json::from_value::<EncoderPref>(Json::from(t)).map_err(|_| bad())?;
                t.into()
            }
            Kind::Device => match t {
                "auto" | "" => return Ok(None),
                path => {
                    checked(Json::from(path))?;
                    path.into()
                }
            },
        }))
    }
}

// ------------------------------------------------------------- read/write

/// Everything wrong with a config's values (empty when it is valid).
pub fn problems(cfg: &UserConfig) -> Vec<String> {
    KEYS.iter()
        .filter_map(|k| {
            let value = get_json(cfg, k.key).ok()?;
            k.check(&value).err()
        })
        .collect()
}

/// Reads `config.toml` text: missing settings take their defaults; unknown
/// keys, wrong types and out-of-range values are `Invalid`.
pub fn parse(text: &str) -> Result<UserConfig, CoreError> {
    let hint = "fix ~/.config/qrec/config.toml (or change a value with `qrec config set KEY VALUE`)";
    let cfg: UserConfig = toml::from_str(text)
        .map_err(|e| CoreError::invalid(toml_error("config.toml", text, &e)).with_hint(hint))?;
    let problems = problems(&cfg);
    match problems.first() {
        None => Ok(cfg),
        Some(first) => Err(CoreError::invalid(format!("config.toml: {first}"))
            .with_hint(hint)
            .with_details(json!({ "problems": problems }))),
    }
}

/// The value at a dotted key (`canvas.fps`, `theme`), or a whole section
/// (`autocut`); unknown keys are `Usage`, an unset optional key
/// (`encoder.vaapi_device`) is `NotFound`.
pub fn get(cfg: &UserConfig, key: &str) -> Result<toml::Value, CoreError> {
    let table = toml::Table::try_from(cfg)
        .map_err(|e| CoreError::internal(format!("config doesn't serialize: {e}")))?;
    let mut current = toml::Value::Table(table);
    for part in key.split('.') {
        let found = match &mut current {
            toml::Value::Table(t) if !part.is_empty() => t.remove(part),
            _ => None,
        };
        match found {
            Some(value) => current = value,
            None if find_key(key).is_some() => {
                return Err(CoreError::not_found(format!("{key} is not set")).with_hint(
                    "Qrec uses the first Intel or AMD render node (never NVIDIA); set one with \
                     `qrec config set encoder.vaapi_device /dev/dri/renderD129`",
                ));
            }
            None => return Err(unknown_key(key)),
        }
    }
    Ok(current)
}

/// [`get`] as JSON, with `null` for an unset optional key.
pub fn get_json(cfg: &UserConfig, key: &str) -> Result<Json, CoreError> {
    match get(cfg, key) {
        Ok(value) => serde_json::to_value(value)
            .map_err(|e| CoreError::internal(format!("config value doesn't convert: {e}"))),
        Err(e) if e.code == crate::ErrorCode::NotFound => Ok(Json::Null),
        Err(e) => Err(e),
    }
}

/// One line of `qrec config`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigEntry {
    pub key: String,
    /// `null` when unset.
    pub value: Json,
}

/// Every setting with its current value, in [`KEYS`] order.
pub fn list(cfg: &UserConfig) -> Vec<ConfigEntry> {
    KEYS.iter()
        .map(|k| ConfigEntry {
            key: k.key.to_owned(),
            value: get_json(cfg, k.key).unwrap_or(Json::Null),
        })
        .collect()
}

/// Sets a dotted key in the document (comments and order survive); the
/// result is validated by re-parsing it as `UserConfig`. `auto` unsets
/// `encoder.vaapi_device`. On error the document is unchanged.
pub fn set(doc: &mut toml_edit::DocumentMut, key: &str, value: &str) -> Result<(), CoreError> {
    let spec = find_key(key).ok_or_else(|| unknown_key(key))?;
    let new = spec.parse_value(value)?;
    let mut edited = doc.clone();
    let (section, field) = match key.split_once('.') {
        Some((section, field)) => (Some(section), field),
        None => (None, key),
    };
    let root = edited.as_table_mut();
    let inline = section.is_some_and(|s| root.get(s).is_some_and(toml_edit::Item::is_inline_table));
    let table: Option<&mut dyn toml_edit::TableLike> = match (section, &new) {
        (None, _) => Some(root as &mut dyn toml_edit::TableLike),
        // Unsetting never creates a section.
        (Some(s), None) => root.get_mut(s).and_then(toml_edit::Item::as_table_like_mut),
        (Some(s), Some(_)) => {
            let item = root.entry(s).or_insert_with(toml_edit::table);
            Some(item.as_table_like_mut().ok_or_else(|| {
                CoreError::usage(format!("`{s}` in config.toml isn't a table"))
                    .with_hint("fix config.toml by hand")
            })?)
        }
    };
    if let Some(table) = table {
        match new {
            Some(mut value) => match table.get_mut(field) {
                // Replace the value in place: the key keeps its comments, the
                // value its spacing and trailing comment.
                Some(item) => {
                    if let Some(old) = item.as_value() {
                        *value.decor_mut() = old.decor().clone();
                    }
                    *item = toml_edit::Item::Value(value);
                }
                None => {
                    table.insert(field, toml_edit::Item::Value(value));
                }
            },
            None => {
                table.remove(field);
            }
        }
        if inline {
            // `ui = { scale = 1.0 }`: respace the one-line table.
            table.fmt();
        }
    }
    let text = edited.to_string();
    let cfg: UserConfig = toml::from_str(&text).map_err(|e| {
        CoreError::usage(format!(
            "config.toml would be invalid: {}",
            toml_error("config.toml", &text, &e)
        ))
        .with_hint("fix config.toml by hand")
    })?;
    if let Some(problem) = problems(&cfg).into_iter().next() {
        return Err(CoreError::usage(problem).with_hint("fix config.toml by hand"));
    }
    *doc = edited;
    Ok(())
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
    use crate::ErrorCode;

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

    #[test]
    fn keys_cover_every_setting() {
        // Every leaf of a fully set config is in KEYS, in order, and vice versa.
        let mut cfg = UserConfig::default();
        cfg.encoder.vaapi_device = Some("/dev/dri/renderD129".into());
        let table = toml::Table::try_from(&cfg).unwrap();
        let mut leaves = Vec::new();
        for (section, value) in &table {
            match value.as_table() {
                Some(inner) => leaves.extend(inner.keys().map(|k| format!("{section}.{k}"))),
                None => leaves.push(section.clone()),
            }
        }
        let mut known: Vec<String> = KEYS.iter().map(|k| k.key.to_owned()).collect();
        leaves.sort();
        known.sort();
        assert_eq!(leaves, known);
        assert!(problems(&UserConfig::default()).is_empty());
    }

    #[test]
    fn get_values() {
        let cfg = UserConfig::default();
        assert_eq!(get(&cfg, "theme").unwrap(), toml::Value::String("swamp-night".into()));
        assert_eq!(get(&cfg, "canvas.width").unwrap(), toml::Value::Integer(1920));
        assert_eq!(get(&cfg, "canvas.fps").unwrap(), toml::Value::String("30".into()));
        assert_eq!(get(&cfg, "autocut.threshold_db").unwrap(), toml::Value::Float(-35.0));
        assert_eq!(get(&cfg, "encoder.preference").unwrap(), toml::Value::String("auto".into()));
        assert!(get(&cfg, "autocut").unwrap().as_table().unwrap().contains_key("padding_ms"));
        let unset = get(&cfg, "encoder.vaapi_device").unwrap_err();
        assert_eq!(unset.code, ErrorCode::NotFound);
        assert_eq!(get_json(&cfg, "encoder.vaapi_device").unwrap(), Json::Null);
        assert_eq!(get_json(&cfg, "ui.scale").unwrap(), json!(1.0));
        for bad in ["", "nope", "canvas.nope", "theme.x", "canvas.", ".theme", "canvas.width.x"] {
            let err = get(&cfg, bad).unwrap_err();
            assert_eq!(err.code, ErrorCode::Usage, "{bad:?}");
            assert!(err.hint.unwrap().contains("canvas.fps"));
        }
        let entries = list(&cfg);
        assert_eq!(entries.len(), KEYS.len());
        assert_eq!(entries[0], ConfigEntry { key: "theme".into(), value: json!("swamp-night") });
        assert!(entries.iter().any(|e| e.key == "encoder.vaapi_device" && e.value.is_null()));
    }

    const COMMENTED: &str = "\
# Qrec settings
theme = \"swamp-day\"   # easier on the eyes

[autocut]
# lower = more sensitive
threshold_db = -35.0  # default
padding_ms = 120

[ui]
scale = 1.25
";

    fn doc(text: &str) -> toml_edit::DocumentMut {
        text.parse().unwrap()
    }

    #[test]
    fn set_keeps_comments_and_order() {
        let mut d = doc(COMMENTED);
        set(&mut d, "autocut.threshold_db", "-40").unwrap();
        set(&mut d, "theme", "ogre-mode").unwrap();
        set(&mut d, "canvas.fps", "29.97").unwrap();
        set(&mut d, "encoder.vaapi_device", "/dev/dri/renderD129").unwrap();
        set(&mut d, "ui.reduced_motion", "true").unwrap();
        let text = d.to_string();
        pretty_assertions::assert_eq!(
            text,
            "\
# Qrec settings
theme = \"ogre-mode\"   # easier on the eyes

[autocut]
# lower = more sensitive
threshold_db = -40.0  # default
padding_ms = 120

[ui]
scale = 1.25
reduced_motion = true

[canvas]
fps = \"30000/1001\"

[encoder]
vaapi_device = \"/dev/dri/renderD129\"
"
        );
        let cfg = parse(&text).unwrap();
        assert_eq!(cfg.autocut.threshold_db, -40.0);
        assert_eq!(cfg.canvas.fps, Fps::new(30000, 1001));
        assert_eq!(cfg.theme, Theme::OgreMode);

        // `auto` unsets the device; unsetting a missing key changes nothing.
        set(&mut d, "encoder.vaapi_device", "auto").unwrap();
        assert!(!d.to_string().contains("vaapi_device"));
        let before = d.to_string();
        set(&mut d, "encoder.vaapi_device", "auto").unwrap();
        assert_eq!(d.to_string(), before);
        let mut empty = doc("");
        set(&mut empty, "encoder.vaapi_device", "auto").unwrap();
        assert_eq!(empty.to_string(), "", "unsetting doesn't create a section");
    }

    #[test]
    fn set_rejects_bad_values_and_leaves_the_document_alone() {
        let cases = [
            ("canvas.width", "1921", "must be an even integer from 16 to 8192"),
            ("canvas.width", "20000", "must be an even integer"),
            ("canvas.height", "wide", "must be an even integer"),
            ("canvas.fps", "1000", "at most 240"),
            ("canvas.fps", "0", "at most 240"),
            ("canvas.background", "green", "#RRGGBB"),
            ("theme", "shrek", "swamp-night"),
            ("recording.countdown_secs", "11", "from 0 to 10"),
            ("recording.countdown_secs", "-1", "from 0 to 10"),
            ("autocut.threshold_db", "5", "from -90 to 0"),
            ("autocut.threshold_db", "nan", "from -90 to 0"),
            ("autocut.min_keep_ms", "60001", "from 0 to 60000"),
            ("autocut.auto_run_after_take", "yes", "true or false"),
            ("encoder.preference", "nvenc", "auto, vaapi or x264"),
            ("encoder.vaapi_device", "renderD129", "render node"),
            ("ui.scale", "10", "from 0.5 to 3"),
        ];
        for (key, value, expected) in cases {
            let mut d = doc(COMMENTED);
            let err = set(&mut d, key, value).unwrap_err();
            assert_eq!(err.code, ErrorCode::Usage, "{key} = {value}");
            assert!(err.message.contains(expected), "{key} = {value}: {}", err.message);
            assert_eq!(d.to_string(), COMMENTED, "{key} = {value} changed the document");
        }
        let mut d = doc(COMMENTED);
        let err = set(&mut d, "canvas.depth", "8").unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert!(err.message.contains("unknown config key `canvas.depth`"));
        let err = set(&mut d, "autocut", "x").unwrap_err();
        assert!(err.message.contains("unknown config key `autocut`"));

        // A bad value elsewhere in the file blocks the write and is named.
        let mut d = doc("[canvas]\nwidth = 7\n");
        let err = set(&mut d, "theme", "swamp-day").unwrap_err();
        assert!(err.message.contains("canvas.width must be"), "{}", err.message);
        let mut d = doc("canvas = 5\n");
        let err = set(&mut d, "canvas.width", "1280").unwrap_err();
        assert!(err.message.contains("isn't a table"), "{}", err.message);
        let mut d = doc("[ui]\nsclae = 2\n");
        let err = set(&mut d, "ui.scale", "2").unwrap_err();
        assert!(err.message.contains("would be invalid"), "{}", err.message);
        assert!(err.message.contains("sclae"), "{}", err.message);
    }

    #[test]
    fn set_works_with_inline_tables() {
        let mut d = doc("ui = { scale = 1.0 }\n");
        set(&mut d, "ui.scale", "2").unwrap();
        set(&mut d, "ui.sounds", "true").unwrap();
        assert_eq!(d.to_string(), "ui = { scale = 2.0, sounds = true }\n");
    }

    #[test]
    fn parse_reports_every_problem() {
        let cfg = parse("theme = \"ogre-mode\"\n[recording]\ncountdown_secs = 3\n").unwrap();
        assert_eq!(cfg.recording.countdown_secs, 3);
        let err = parse("[canvas]\nwidth = 15\n[ui]\nscale = 0.1\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(err.message, "config.toml: canvas.width must be an even integer from 16 to 8192 (got 15)");
        assert_eq!(err.details.unwrap()["problems"].as_array().unwrap().len(), 2);
        let err = parse("\n\n[canvas\n").unwrap_err();
        assert!(err.message.starts_with("config.toml line 3:"), "{}", err.message);
        let err = parse("[encoder]\nvaapi_device = \"renderD128\"\n").unwrap_err();
        assert!(err.message.contains("encoder.vaapi_device must be"), "{}", err.message);
    }
}
