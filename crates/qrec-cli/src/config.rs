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
use qrec_core::config::{MAX_FILE_BYTES, UserConfig};
use serde_json::{Map, Value};

/// The config file in Qrec's config dir.
pub(crate) fn file(config_dir: &Path) -> PathBuf {
    config_dir.join("config.toml")
}

/// The text of a settings file (`config.toml`, `keys.toml`), `None` when
/// there is none, read like the hosts read it (A§8.2): a regular file of at
/// most `MAX_FILE_BYTES` (1 MiB). Anything else is an error that says so,
/// never waited on (a FIFO would block a plain read) or read whole. The
/// `Backend::settings_text` of a build without the shell; the `qrec`
/// binary opens it non-blocking (`qrec_shell::config_file::read_text`).
pub fn read_settings(path: &Path) -> Result<Option<String>, CoreError> {
    let cant =
        |e: &dyn std::fmt::Display| CoreError::io(format!("can't read {}: {e}", path.display()));
    let too_big = |bytes: u64| {
        CoreError::io(format!(
            "{} is {bytes} bytes; settings files can be at most {MAX_FILE_BYTES} bytes (1 MiB)",
            path.display()
        ))
    };
    let regular = |meta: &fs::Metadata| {
        if !meta.is_file() {
            return Err(cant(&"it isn't a regular file"));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(too_big(meta.len()));
        }
        Ok(())
    };
    // Checked before it is opened: opening a FIFO waits for a writer.
    match fs::metadata(path) {
        Ok(meta) => regular(&meta)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(cant(&e)),
    }
    let file = fs::File::open(path).map_err(|e| cant(&e))?;
    regular(&file.metadata().map_err(|e| cant(&e))?)?;
    let mut text = String::new();
    io::Read::read_to_string(&mut io::Read::take(file, MAX_FILE_BYTES + 1), &mut text)
        .map_err(|e| cant(&e))?;
    // It may have grown since the `stat`.
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(too_big(text.len() as u64));
    }
    Ok(Some(text))
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

/// Validates the user config `text` read from `path` (empty: the
/// defaults): the shape through `UserConfig`, the ranges through the core's
/// `config::problems` (the same rules hosts apply when they read it).
pub(crate) fn load(text: &str, path: &Path) -> Result<UserConfig, CoreError> {
    let json = to_json(text, path)?;
    let cfg: UserConfig = serde_json::from_value(json).map_err(|e| {
        CoreError::invalid(format!("{}: {e}", path.display()))
            .with_hint("`qrec config` lists the settings and their values")
    })?;
    match qrec_core::config::problems(&cfg).first() {
        None => Ok(cfg),
        Some(problem) => Err(CoreError::invalid(format!("{}: {problem}", path.display()))
            .with_hint("fix it with `qrec config set KEY VALUE`")),
    }
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

/// The text of the file at `path` (whose text is `text`, empty when there
/// is none) with `key` set to `value`, and the new value: the one line
/// changed, added or removed, every other one as it was
/// (`qrec_core::config::set_text`: comments and order stay). The caller
/// writes it (`Backend::write_settings`).
pub(crate) fn set(
    path: &Path,
    text: &str,
    key: &str,
    value: &str,
) -> Result<(String, Value), CoreError> {
    document(text, path, qrec_core::config::set)?;
    let new_text = qrec_core::config::set_text(text, key, value)?;
    let cfg: UserConfig = serde_json::from_value(to_json(&new_text, path)?)
        .map_err(|e| CoreError::invalid(format!("{key} = {value} doesn't fit: {e}")))?;
    let value = get(&cfg, key)?;
    Ok((new_text, value))
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
/// over `path`, so a crash leaves the old file or the new one (the default
/// `Backend::write_settings`).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
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
