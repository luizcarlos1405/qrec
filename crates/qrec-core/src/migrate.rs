//! Schema migrations `vN → vN+1` (SPEC §6.3). Owner: meta.
//!
//! `qrec.json` carries a `version`. [`migrate`] reads it, runs the pure
//! steps of [`STEPS`] up to [`CURRENT_VERSION`] and deserializes the
//! result; a file newer than this build is `Invalid` ("update Qrec"). The
//! shell writes [`backup_path`] (`.qrec/qrec.v{N}.json.bak`, never
//! overwritten) before saving a migrated project; read-only commands
//! migrate in memory only. Validation is separate (`validate::ensure_valid`).

use serde_json::Value;

use crate::error::CoreError;
use crate::model::{CURRENT_VERSION, Project};

/// The first schema version.
pub const FIRST_VERSION: u32 = 1;

/// One schema step: a `vN` document in, the `vN+1` document out, with
/// `version` set to `N+1`. Pure and total over valid `vN` documents.
pub type Step = fn(Value) -> Result<Value, CoreError>;

/// `STEPS[i]` migrates version `FIRST_VERSION + i` to `FIRST_VERSION + i + 1`,
/// so `FIRST_VERSION + STEPS.len() == CURRENT_VERSION`. Empty while v1 is
/// the only schema.
pub const STEPS: &[Step] = &[];

/// A project read from disk, at `CURRENT_VERSION`.
#[derive(Debug, Clone, PartialEq)]
pub struct Migrated {
    pub project: Project,
    /// The version found in the file (`< CURRENT_VERSION` ⇒ it was migrated).
    pub from_version: u32,
}

impl Migrated {
    /// The file was written by an older Qrec: back it up before saving.
    pub fn was_migrated(&self) -> bool {
        self.from_version < CURRENT_VERSION
    }
}

/// Where the shell keeps the original of a migrated file (A§8.8: never
/// overwritten; it loops `-2`, `-3`, … like other outputs).
pub fn backup_path(from_version: u32) -> String {
    format!(".qrec/qrec.v{from_version}.json.bak")
}

const RESTORE_HINT: &str =
    "fix qrec.json, or rebuild it from the history with `qrec history restore <seq> --yes`";

/// The `version` of a `qrec.json` document.
pub fn version_of(value: &Value) -> Result<u32, CoreError> {
    let object = value
        .as_object()
        .ok_or_else(|| CoreError::invalid("qrec.json is not a JSON object").with_hint(RESTORE_HINT))?;
    let version = object
        .get("version")
        .ok_or_else(|| CoreError::invalid("qrec.json has no version").with_hint(RESTORE_HINT))?;
    version
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v >= FIRST_VERSION)
        .ok_or_else(|| {
            CoreError::invalid(format!(
                "qrec.json version {version} is not a schema version (1, 2, …)"
            ))
            .with_hint(RESTORE_HINT)
        })
}

/// Runs `steps` (`steps[i]`: version `FIRST_VERSION + i` → `+ 1`) on a
/// document; returns the upgraded document and the version it had.
pub fn upgrade(value: Value, steps: &[Step]) -> Result<(Value, u32), CoreError> {
    let current = FIRST_VERSION + steps.len() as u32;
    let from = version_of(&value)?;
    if from > current {
        return Err(CoreError::invalid(format!(
            "qrec.json is schema version {from}, but this Qrec reads up to version {current}"
        ))
        .with_hint("the project was saved by a newer Qrec: update Qrec to open it"));
    }
    let mut value = value;
    for version in from..current {
        let step = steps[(version - FIRST_VERSION) as usize];
        value = step(value).map_err(|mut e| {
            e.message = format!(
                "can't migrate qrec.json from version {version} to {}: {}",
                version + 1,
                e.message
            );
            e
        })?;
        let written = version_of(&value)?;
        if written != version + 1 {
            return Err(CoreError::internal(format!(
                "the migration from version {version} wrote version {written}"
            )));
        }
    }
    Ok((value, from))
}

/// Migrates a parsed `qrec.json` to `CURRENT_VERSION` and deserializes it.
/// A newer version than this build knows is `Invalid`.
pub fn migrate(value: Value) -> Result<Migrated, CoreError> {
    migrate_with(value, STEPS)
}

/// [`migrate`] with an explicit step table (tests use synthetic steps).
pub fn migrate_with(value: Value, steps: &[Step]) -> Result<Migrated, CoreError> {
    let (value, from_version) = upgrade(value, steps)?;
    let project = serde_json::from_value(value).map_err(schema_error)?;
    Ok(Migrated {
        project,
        from_version,
    })
}

fn schema_error(e: serde_json::Error) -> CoreError {
    CoreError::invalid(format!("qrec.json doesn't match the project schema: {e}"))
        .with_hint(RESTORE_HINT)
}

/// Parses `qrec.json` text, then [`migrate`]s it. A current-version file is
/// deserialized straight from the text, so errors carry line and column.
pub fn parse_project(text: &str) -> Result<Migrated, CoreError> {
    let value: Value = serde_json::from_str(text).map_err(|e| {
        CoreError::invalid(format!("qrec.json is not valid JSON: {e}")).with_hint(RESTORE_HINT)
    })?;
    if version_of(&value)? == CURRENT_VERSION {
        let project = serde_json::from_str(text).map_err(schema_error)?;
        return Ok(Migrated {
            project,
            from_version: CURRENT_VERSION,
        });
    }
    migrate(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use serde_json::json;

    const SPEC_EXAMPLE: &str = include_str!("../tests/fixtures/spec_example.json");

    #[test]
    fn the_step_table_reaches_the_current_version() {
        assert_eq!(FIRST_VERSION + STEPS.len() as u32, CURRENT_VERSION);
        assert_eq!(backup_path(1), ".qrec/qrec.v1.json.bak");
    }

    #[test]
    fn current_files_load_unchanged() {
        let migrated = parse_project(SPEC_EXAMPLE).unwrap();
        assert_eq!(migrated.from_version, 1);
        assert!(!migrated.was_migrated());
        let expected: Project = serde_json::from_str(SPEC_EXAMPLE).unwrap();
        assert_eq!(migrated.project, expected);
        let value: Value = serde_json::from_str(SPEC_EXAMPLE).unwrap();
        assert_eq!(migrate(value).unwrap(), migrated);
    }

    #[test]
    fn versions_are_detected() {
        let with_version = |v: Value| {
            let mut value: Value = serde_json::from_str(SPEC_EXAMPLE).unwrap();
            value["version"] = v;
            value
        };
        let err = migrate(with_version(json!(2))).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(
            err.message,
            "qrec.json is schema version 2, but this Qrec reads up to version 1"
        );
        assert!(err.hint.unwrap().contains("update Qrec"));
        for bad in [json!(0), json!(-1), json!(1.5), json!("1"), json!(null), json!(4294967296u64)] {
            let err = migrate(with_version(bad.clone())).unwrap_err();
            assert_eq!(err.code, ErrorCode::Invalid, "{bad}");
            assert!(err.message.contains("is not a schema version"), "{bad}: {}", err.message);
        }
        let mut value = with_version(json!(1));
        value.as_object_mut().unwrap().remove("version");
        assert_eq!(migrate(value).unwrap_err().message, "qrec.json has no version");
        assert_eq!(migrate(json!([1])).unwrap_err().message, "qrec.json is not a JSON object");
    }

    #[test]
    fn parse_errors_say_where() {
        let err = parse_project("{\"version\": 1,").unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert!(err.message.starts_with("qrec.json is not valid JSON:"), "{}", err.message);
        assert!(err.hint.unwrap().contains("qrec history restore"));
        let text = SPEC_EXAMPLE.replace("\"fps\": { \"num\": 30", "\"fsp\": { \"num\": 30");
        let err = parse_project(&text).unwrap_err();
        assert!(err.message.contains("unknown field `fsp`"), "{}", err.message);
        assert!(err.message.contains("line 8"), "{}", err.message);
    }

    // A synthetic history for the framework: "v1" kept the canvas fps as a
    // string and named the timeline `edit`; "v2" is today's shape. Step 2→3
    // only renames nothing but bumps the version, to check ordering.
    fn v1_to_v2(mut value: Value) -> Result<Value, CoreError> {
        let object = value.as_object_mut().expect("an object");
        let fps = object["canvas"]["fps"]
            .as_str()
            .ok_or_else(|| CoreError::invalid("canvas.fps must be a string in v1"))?
            .to_owned();
        let (num, den) = fps.split_once('/').unwrap_or((&fps, "1"));
        object["canvas"]["fps"] = json!({ "num": num.parse::<u32>().unwrap(), "den": den.parse::<u32>().unwrap() });
        let edit = object.remove("edit").unwrap_or(json!({ "clips": [] }));
        object.insert("timeline".into(), edit);
        object.insert("version".into(), json!(2));
        Ok(value)
    }

    fn v2_to_v3(mut value: Value) -> Result<Value, CoreError> {
        value["version"] = json!(3);
        Ok(value)
    }

    fn writes_wrong_version(mut value: Value) -> Result<Value, CoreError> {
        value["version"] = json!(7);
        Ok(value)
    }

    fn v1_document() -> Value {
        let mut value: Value = serde_json::from_str(SPEC_EXAMPLE).unwrap();
        value["canvas"]["fps"] = json!("30000/1001");
        let timeline = value.as_object_mut().unwrap().remove("timeline").unwrap();
        value["edit"] = timeline;
        value
    }

    #[test]
    fn steps_run_in_order() {
        let migrated = migrate_with(v1_document(), &[v1_to_v2, v2_to_v3]).unwrap();
        assert_eq!(migrated.from_version, 1);
        assert_eq!(migrated.project.version, 3);
        assert_eq!(migrated.project.canvas.fps, crate::Fps::new(30000, 1001));
        assert_eq!(migrated.project.timeline.clips.len(), 2);

        // A v2 file only runs the second step; a v3 file none.
        let mut v2: Value = serde_json::from_str(SPEC_EXAMPLE).unwrap();
        v2["version"] = json!(2);
        let (upgraded, from) = upgrade(v2.clone(), &[v1_to_v2, v2_to_v3]).unwrap();
        assert_eq!(from, 2);
        assert_eq!(upgraded["version"], 3);
        assert_eq!(upgraded["canvas"], v2["canvas"]);

        let err = migrate_with(v1_document(), &[writes_wrong_version]).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        let mut broken = v1_document();
        broken["canvas"]["fps"] = json!(30);
        let err = migrate_with(broken, &[v1_to_v2]).unwrap_err();
        assert_eq!(
            err.message,
            "can't migrate qrec.json from version 1 to 2: canvas.fps must be a string in v1"
        );
    }
}
