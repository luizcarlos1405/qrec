//! The project as JSON for patching, and its undoable view (A§7.2).
//! Owner: history.
//!
//! PLACEHOLDER: history owner replaces this (A§7.2). The foundation version
//! already writes the optional scene members explicitly and strips the
//! non-undoable fields, so `commit` works end to end.

use serde_json::{Map, Value};

use crate::model::Project;

/// `qrec.json` as a JSON value with every optional scene member written
/// explicitly (`locked: false`, `missing: false`), so patches never add or
/// remove object members. Patches are applied to this, never to file text.
pub fn project_value(project: &Project) -> Value {
    let mut value = serde_json::to_value(project).expect("projects always serialize");
    if let Some(scene) = value.get_mut("scene") {
        explicit_scene_members(scene);
    }
    if let Some(Value::Object(takes)) = value.get_mut("takes") {
        for take in takes.values_mut() {
            if let Some(scene) = take.get_mut("scene") {
                explicit_scene_members(scene);
            }
        }
    }
    value
}

/// [`project_value`] without `version`, `createdAt` and `nextTakeNumber`,
/// with every take reduced to `{scene}`: exactly what undo can change.
pub fn undoable_view(project: &Project) -> Value {
    let mut value = project_value(project);
    if let Value::Object(map) = &mut value {
        map.remove("version");
        map.remove("createdAt");
        map.remove("nextTakeNumber");
        if let Some(Value::Object(takes)) = map.get_mut("takes") {
            for take in takes.values_mut() {
                let scene = take.get("scene").cloned().unwrap_or(Value::Null);
                let mut reduced = Map::new();
                reduced.insert("scene".to_owned(), scene);
                *take = Value::Object(reduced);
            }
        }
    }
    value
}

fn explicit_scene_members(scene: &mut Value) {
    let defaults: [(&str, &[&str]); 2] =
        [("layers", &["locked", "missing"]), ("audio", &["missing"])];
    for (array, members) in defaults {
        if let Some(Value::Array(items)) = scene.get_mut(array) {
            for item in items.iter_mut().filter_map(Value::as_object_mut) {
                for member in members {
                    item.entry(*member).or_insert(Value::Bool(false));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::ProjectBuilder;

    #[test]
    fn views_are_explicit_and_round_trip() {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 100)
            .clip("c001", "take-0001", 0, 100)
            .build();
        let value = project_value(&project);
        assert_eq!(value["scene"]["layers"][0]["locked"], false);
        assert_eq!(
            value["takes"]["take-0001"]["scene"]["audio"][0]["missing"],
            false
        );
        let back: Project = serde_json::from_value(value).unwrap();
        assert_eq!(back, project);

        let view = undoable_view(&project);
        assert!(view.get("version").is_none() && view.get("nextTakeNumber").is_none());
        let take = view["takes"]["take-0001"].as_object().unwrap();
        assert_eq!(take.keys().collect::<Vec<_>>(), ["scene"]);
    }
}
