//! The project as JSON for patching, and its undoable view (A§7.2).
//! Owner: history.
//!
//! Patches are computed between undoable views and applied to
//! [`project_value`], never to file text. Both write every optional member
//! explicitly (`locked: false`, `missing: false`, `error: null`, …), so the
//! same paths exist whatever the values are and a patch never has to add or
//! remove an object member.

use serde_json::{Map, Value};

use crate::error::CoreError;
use crate::model::Project;

/// The value an omitted optional member stands for.
#[derive(Clone, Copy)]
enum Omitted {
    False,
    Null,
    EmptyArray,
}

impl Omitted {
    fn value(self) -> Value {
        match self {
            Omitted::False => Value::Bool(false),
            Omitted::Null => Value::Null,
            Omitted::EmptyArray => Value::Array(Vec::new()),
        }
    }
}

/// `Layer` members serde skips when they hold their default.
const LAYER: &[(&str, Omitted)] = &[("locked", Omitted::False), ("missing", Omitted::False)];
/// `AudioTrack` members serde skips when they hold their default.
const TRACK: &[(&str, Omitted)] = &[("missing", Omitted::False)];
/// `Take` members serde skips when they hold their default.
const TAKE: &[(&str, Omitted)] = &[
    ("error", Omitted::Null),
    ("placed", Omitted::False),
    ("replaced", Omitted::EmptyArray),
];
/// `Stream` members serde skips when they are `None`.
const STREAM: &[(&str, Omitted)] = &[
    ("width", Omitted::Null),
    ("height", Omitted::Null),
    ("sampleRate", Omitted::Null),
    ("channels", Omitted::Null),
    ("durationNs", Omitted::Null),
    ("frames", Omitted::Null),
    ("fps", Omitted::Null),
];

/// `qrec.json` as a JSON value with every optional member written
/// explicitly. Patches are applied to this; [`project_from_value`] turns
/// the result back into a `Project`.
pub fn project_value(project: &Project) -> Value {
    let mut value = serde_json::to_value(project).expect("projects always serialize");
    if let Some(scene) = value.get_mut("scene") {
        explicit_scene(scene);
    }
    if let Some(Value::Object(takes)) = value.get_mut("takes") {
        for take in takes.values_mut() {
            explicit_take(take);
        }
    }
    value
}

/// [`project_value`] without `version`, `createdAt` and `nextTakeNumber`,
/// with every take reduced to `{scene}`: exactly what undo can change
/// (SPEC §8.2).
pub fn undoable_view(project: &Project) -> Value {
    let mut value = project_value(project);
    if let Value::Object(map) = &mut value {
        map.remove("version");
        map.remove("createdAt");
        map.remove("nextTakeNumber");
        if let Some(Value::Object(takes)) = map.get_mut("takes") {
            for take in takes.values_mut() {
                let scene = take
                    .get_mut("scene")
                    .map(Value::take)
                    .unwrap_or(Value::Null);
                let mut reduced = Map::new();
                reduced.insert("scene".to_owned(), scene);
                *take = Value::Object(reduced);
            }
        }
    }
    value
}

/// Turns a (patched) [`project_value`] back into a `Project`. A value that
/// doesn't deserialize is `Invalid`.
pub fn project_from_value(value: Value) -> Result<Project, CoreError> {
    serde_json::from_value(value)
        .map_err(|e| CoreError::invalid(format!("the patched project doesn't parse: {e}")))
}

fn explicit_scene(scene: &mut Value) {
    for (array, members) in [("layers", LAYER), ("audio", TRACK)] {
        if let Some(Value::Array(items)) = scene.get_mut(array) {
            for item in items.iter_mut() {
                fill(item, members);
            }
        }
    }
}

fn explicit_take(take: &mut Value) {
    fill(take, TAKE);
    if let Some(scene) = take.get_mut("scene") {
        explicit_scene(scene);
    }
    if let Some(Value::Array(streams)) = take.get_mut("streams") {
        for stream in streams.iter_mut() {
            fill(stream, STREAM);
        }
    }
}

fn fill(object: &mut Value, members: &[(&str, Omitted)]) {
    if let Value::Object(map) = object {
        for (name, omitted) in members {
            map.entry(*name).or_insert_with(|| omitted.value());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::model::{Clip, Fps};
    use crate::testkit::ProjectBuilder;

    fn project() -> Project {
        ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 100)
            .clip("c001", "take-0001", 0, 100)
            .build()
    }

    /// Every optional member set to a non-default value.
    fn everything_set() -> Project {
        let mut p = project();
        p.scene.layers[0].locked = true;
        p.scene.layers[0].missing = true;
        p.scene.audio[0].missing = true;
        let take = p.takes.values_mut().next().unwrap();
        take.scene = p.scene.clone();
        take.error = Some("boom".into());
        take.placed = true;
        take.replaced = vec![Clip {
            id: "c0aa".parse().unwrap(),
            take: "take-0001".parse().unwrap(),
            in_: 0,
            out: 5,
        }];
        for stream in &mut take.streams {
            stream.width = Some(1);
            stream.height = Some(1);
            stream.sample_rate = Some(48_000);
            stream.channels = Some(1);
            stream.duration_ns = Some(1);
            stream.frames = Some(1);
            stream.fps = Some(Fps::new(30, 1));
        }
        p
    }

    /// The members of every object, keyed by the object's path with array
    /// indices written as `*`.
    fn members(value: &Value, path: &str, out: &mut BTreeMap<String, BTreeSet<String>>) {
        match value {
            Value::Object(map) => {
                out.entry(path.to_owned())
                    .or_default()
                    .extend(map.keys().cloned());
                for (key, child) in map {
                    members(child, &format!("{path}/{key}"), out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    members(item, &format!("{path}/*"), out);
                }
            }
            _ => {}
        }
    }

    fn object_members(value: &Value) -> BTreeMap<String, BTreeSet<String>> {
        let mut out = BTreeMap::new();
        members(value, "", &mut out);
        out
    }

    #[test]
    fn views_are_explicit_and_round_trip() {
        let project = project();
        let value = project_value(&project);
        assert_eq!(value["scene"]["layers"][0]["locked"], false);
        assert_eq!(
            value["takes"]["take-0001"]["scene"]["audio"][0]["missing"],
            false
        );
        assert_eq!(value["takes"]["take-0001"]["error"], Value::Null);
        assert_eq!(
            value["takes"]["take-0001"]["streams"][1]["fps"],
            Value::Null
        );
        assert_eq!(project_from_value(value).unwrap(), project);

        let view = undoable_view(&project);
        assert!(view.get("version").is_none() && view.get("nextTakeNumber").is_none());
        assert!(view.get("createdAt").is_none());
        let take = view["takes"]["take-0001"].as_object().unwrap();
        assert_eq!(take.keys().collect::<Vec<_>>(), ["scene"]);
        assert_eq!(
            take["scene"],
            project_value(&project)["takes"]["take-0001"]["scene"]
        );
    }

    /// Guards the member lists above: a new optional member in `model.rs`
    /// that serde skips must be added there too, or patches would have to
    /// add or remove it.
    #[test]
    fn every_optional_member_is_written() {
        let full = everything_set();
        let with_all = object_members(&serde_json::to_value(&full).unwrap());
        let explicit = object_members(&project_value(&project()));
        for (path, keys) in &explicit {
            let all = &with_all[path];
            let missing: Vec<&String> = all.difference(keys).collect();
            assert!(
                missing.is_empty(),
                "{path}: not written explicitly: {missing:?}"
            );
        }
        // The full project has the same members everywhere (plus the
        // objects inside its non-empty `replaced` list).
        let explicit_full = object_members(&project_value(&full));
        for (path, keys) in &explicit {
            assert_eq!(&explicit_full[path], keys, "{path}");
        }
        assert_eq!(project_from_value(project_value(&full)).unwrap(), full);
    }

    #[test]
    fn bad_values_are_invalid() {
        let mut value = project_value(&project());
        value["timeline"]["clips"][0]["in"] = Value::String("zero".into());
        let err = project_from_value(value).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Invalid);
        assert!(
            err.message.starts_with("the patched project doesn't parse"),
            "{}",
            err.message
        );
    }
}
