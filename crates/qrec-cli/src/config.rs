//! `qrec config [get KEY | set KEY VALUE]` (A§9.8): the user settings in
//! `config.toml`, read and validated as `qrec_core::config::UserConfig`
//! (missing keys take their defaults) and edited with the core's
//! `config::set`, which keeps comments and order. Writes are atomic.
//!
//! The file is read with `toml_edit` (through the core's `config::set`
//! signature: this crate has no TOML dependency of its own) and checked by
//! deserializing it into `UserConfig`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use qrec_core::CoreError;
use qrec_core::config::UserConfig;
use serde_json::{Map, Value};

/// The config file in Qrec's config dir.
pub(crate) fn file(config_dir: &Path) -> PathBuf {
    config_dir.join("config.toml")
}

/// The file's text, empty when it doesn't exist.
fn read(path: &Path) -> Result<String, CoreError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(CoreError::io(format!("can't read {}: {e}", path.display()))),
    }
}

/// Parses TOML into the document type the core's `config::set` edits.
fn document<D>(
    text: &str,
    path: &Path,
    _edit: fn(&mut D, &str, &str) -> Result<(), CoreError>,
) -> Result<D, CoreError>
where
    D: FromStr,
    D::Err: std::fmt::Display,
{
    text.parse().map_err(|e: D::Err| {
        CoreError::invalid(format!("{} is not valid TOML: {e}", path.display()))
            .with_hint("fix the file, or delete it to get the defaults back")
    })
}

/// The settings of `text` (TOML) as JSON, one level of tables deep, the
/// shape `UserConfig` has. Values are strings, integers, floats or booleans.
fn to_json(text: &str, path: &Path) -> Result<Value, CoreError> {
    let doc = document(text, path, qrec_core::config::set)?;
    let bad = |key: &str| {
        CoreError::invalid(format!(
            "{}: {key} has a value Qrec doesn't use (expected text, a number or true/false)",
            path.display()
        ))
    };
    // A macro rather than a closure: the value type is only known (and its
    // methods resolvable) where it is used.
    macro_rules! scalar {
        ($key:expr, $value:expr) => {{
            let (key, v) = ($key, $value);
            if let Some(s) = v.as_str() {
                Ok(Value::from(s))
            } else if let Some(i) = v.as_integer() {
                Ok(Value::from(i))
            } else if let Some(f) = v.as_float() {
                serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .ok_or_else(|| bad(key))
            } else if let Some(b) = v.as_bool() {
                Ok(Value::from(b))
            } else {
                Err(bad(key))
            }
        }};
    }
    let mut root = Map::new();
    for (key, item) in doc.iter() {
        if let Some(table) = item.as_table() {
            let mut section = Map::new();
            for (inner, item) in table.iter() {
                let dotted = format!("{key}.{inner}");
                let value = item.as_value().ok_or_else(|| bad(&dotted))?;
                section.insert(inner.to_owned(), scalar!(&dotted, value)?);
            }
            root.insert(key.to_owned(), Value::Object(section));
        } else if let Some(value) = item.as_value() {
            if let Some(inline) = value.as_inline_table() {
                let mut section = Map::new();
                for (inner, value) in inline.iter() {
                    let dotted = format!("{key}.{inner}");
                    section.insert(inner.to_owned(), scalar!(&dotted, value)?);
                }
                root.insert(key.to_owned(), Value::Object(section));
            } else {
                root.insert(key.to_owned(), scalar!(key, value)?);
            }
        } else {
            return Err(bad(key));
        }
    }
    Ok(Value::Object(root))
}

/// Reads and validates the user config at `path` (defaults when missing).
pub(crate) fn load(path: &Path) -> Result<UserConfig, CoreError> {
    let json = to_json(&read(path)?, path)?;
    serde_json::from_value(json).map_err(|e| {
        CoreError::invalid(format!("{}: {e}", path.display()))
            .with_hint("`qrec config` lists the settings and their values")
    })
}

/// Every setting as JSON (`UserConfig` with snake_case keys).
pub(crate) fn values(cfg: &UserConfig) -> Value {
    serde_json::to_value(cfg).unwrap_or(Value::Null)
}

/// `(dotted key, value)` for every setting: top-level keys (`theme`)
/// first, then each section, both in key order.
pub(crate) fn flatten(values: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    if let Value::Object(root) = values {
        // Top-level scalars first (`theme`), then each section.
        for (key, value) in root.iter().filter(|(_, v)| !v.is_object()) {
            out.push((key.clone(), value.clone()));
        }
        for (key, value) in root.iter() {
            if let Value::Object(section) = value {
                for (inner, value) in section {
                    out.push((format!("{key}.{inner}"), value.clone()));
                }
            }
        }
    }
    out
}

/// The value of one dotted key; unknown keys are usage errors.
pub(crate) fn get(cfg: &UserConfig, key: &str) -> Result<Value, CoreError> {
    let all = flatten(&values(cfg));
    all.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .ok_or_else(|| {
            let keys: Vec<&str> = all.iter().map(|(k, _)| k.as_str()).collect();
            CoreError::usage(format!("unknown setting {key:?}"))
                .with_hint(format!("settings: {}", keys.join(", ")))
        })
}

/// Sets `key` to `value` in the file at `path` (created when missing),
/// keeping its comments, and returns the new value.
pub(crate) fn set(path: &Path, key: &str, value: &str) -> Result<Value, CoreError> {
    let text = read(path)?;
    let mut doc = document(&text, path, qrec_core::config::set)?;
    qrec_core::config::set(&mut doc, key, value)?;
    let new_text = doc.to_string();
    let cfg: UserConfig = serde_json::from_value(to_json(&new_text, path)?)
        .map_err(|e| CoreError::invalid(format!("{key} = {value} doesn't fit: {e}")))?;
    write_atomic(path, new_text.as_bytes())?;
    get(&cfg, key)
}

/// `value` for people: text without quotes, `(not set)` for null.
pub(crate) fn display(value: &Value) -> String {
    match value {
        Value::Null => "(not set)".to_owned(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Writes `bytes` to a temp file next to `path`, syncs it and renames it
/// over `path`, so a crash leaves the old file or the new one.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let io_err =
        |what: &str, e: io::Error| CoreError::io(format!("can't {what} {}: {e}", path.display()));
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|e| io_err("create the folder of", e))?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config.toml"),
        std::process::id()
    ));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        if let Ok(dir) = fs::File::open(dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    result.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        io_err("write", e)
    })
}
