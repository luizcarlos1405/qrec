//! Patches between undoable views (A§7.2). Owner: history.
//!
//! A custom diff (the workspace builds `json-patch` without its `diff`
//! feature, and its diff knows nothing about ids):
//!
//! - Arrays of objects that all have a unique string `id` (`timeline.clips`,
//!   `scene.layers`, `scene.audio`) are matched by id with a longest common
//!   subsequence: ids that leave the subsequence are removed (`test` +
//!   `remove`), ids that enter it are added at their final index, so a moved
//!   element is a remove plus an add; ids in the subsequence are diffed
//!   member by member. Other arrays are replaced whole.
//! - Objects with the same members are diffed member by member; objects
//!   whose members differ (a source switching `kind`) are replaced whole,
//!   so a patch never adds or removes an object member.
//! - Every `replace` and `remove` is immediately preceded by a `test` of the
//!   value at that path in the intermediate document: the ops are generated
//!   by applying them one by one to a scratch copy. A failing `test` means
//!   the project drifted from the history, and undo refuses instead of
//!   corrupting it.
//!
//! The patch size grows with what changed (clips, layers, fields), never
//! with the length of the timeline.

use std::collections::{BTreeMap, BTreeSet};

use json_patch::jsonptr::PointerBuf;
use json_patch::{
    AddOperation, Patch, PatchOperation, RemoveOperation, ReplaceOperation, TestOperation,
};
use serde_json::Value;

use crate::history::view::undoable_view;
use crate::model::Project;

/// The undoable views of `prev` and `next`, built over the same set of
/// takes: takes created by the step are copied into "before", takes gone
/// from "after" are dropped from both. Patches then only touch
/// `/takes/<id>/scene/…`, never whole takes (takes are facts, SPEC §8.2).
pub fn aligned_views(prev: &Project, next: &Project) -> (Value, Value) {
    let mut before = undoable_view(prev);
    let mut after = undoable_view(next);
    if let (Some(Value::Object(b)), Some(Value::Object(a))) =
        (before.get_mut("takes"), after.get_mut("takes"))
    {
        b.retain(|id, _| a.contains_key(id));
        for (id, take) in a.iter() {
            if !b.contains_key(id) {
                b.insert(id.clone(), take.clone());
            }
        }
    }
    (before, after)
}

/// The patch that turns `before` into `after`, with a `test` right before
/// every `replace` and `remove`.
pub fn diff(before: &Value, after: &Value) -> Patch {
    let mut differ = Differ {
        doc: before.clone(),
        ops: Vec::new(),
    };
    differ.value(&mut PointerBuf::root(), after);
    debug_assert_eq!(&differ.doc, after, "the diff reproduces its target");
    Patch(differ.ops)
}

/// Generates ops while applying each one to `doc`, a scratch copy of
/// "before", so every path and every `test` value refers to the
/// intermediate document the op will really see.
struct Differ {
    doc: Value,
    ops: Vec<PatchOperation>,
}

impl Differ {
    fn current(&self, path: &PointerBuf) -> &Value {
        self.doc
            .pointer(path.as_str())
            .expect("diff paths exist in the scratch document")
    }

    fn push(&mut self, op: PatchOperation) {
        json_patch::patch(&mut self.doc, std::slice::from_ref(&op))
            .expect("diff ops apply to the scratch document");
        self.ops.push(op);
    }

    fn test(&mut self, path: &PointerBuf) {
        let value = self.current(path).clone();
        self.ops.push(PatchOperation::Test(TestOperation {
            path: path.clone(),
            value,
        }));
    }

    fn replace(&mut self, path: &PointerBuf, value: Value) {
        self.test(path);
        self.push(PatchOperation::Replace(ReplaceOperation {
            path: path.clone(),
            value,
        }));
    }

    fn remove(&mut self, path: &PointerBuf) {
        self.test(path);
        self.push(PatchOperation::Remove(RemoveOperation {
            path: path.clone(),
        }));
    }

    fn add(&mut self, path: &PointerBuf, value: Value) {
        self.push(PatchOperation::Add(AddOperation {
            path: path.clone(),
            value,
        }));
    }

    /// Makes the value at `path` equal to `after`.
    fn value(&mut self, path: &mut PointerBuf, after: &Value) {
        let before = self.current(path);
        if before == after {
            return;
        }
        match (before, after) {
            (Value::Object(b), Value::Object(a)) if b.keys().eq(a.keys()) => {
                for (key, child) in a {
                    path.push_back(key.as_str());
                    self.value(path, child);
                    path.pop_back();
                }
            }
            (Value::Array(b), Value::Array(a)) => match (ids(b), ids(a)) {
                (Some(before_ids), Some(after_ids)) => {
                    self.array(path, before_ids, &after_ids, a);
                }
                _ => self.replace(path, after.clone()),
            },
            _ => self.replace(path, after.clone()),
        }
    }

    /// The id-keyed array diff: removals (descending), then additions and
    /// member diffs in final order (ascending).
    fn array(
        &mut self,
        path: &mut PointerBuf,
        before_ids: Vec<String>,
        after_ids: &[String],
        after: &[Value],
    ) {
        let kept = common_ids(&before_ids, after_ids);
        for (i, id) in before_ids.iter().enumerate().rev() {
            if !kept.contains(id.as_str()) {
                path.push_back(i);
                self.remove(path);
                path.pop_back();
            }
        }
        // The scratch array now holds the kept elements in their common
        // order, so each kept id sits exactly at its final index once the
        // additions before it are in.
        for (j, (id, element)) in after_ids.iter().zip(after).enumerate() {
            path.push_back(j);
            if kept.contains(id.as_str()) {
                self.value(path, element);
            } else {
                self.add(path, element.clone());
            }
            path.pop_back();
        }
    }
}

/// The ids of an array whose elements are all objects with a unique string
/// `id`; `None` for any other array.
fn ids(items: &[Value]) -> Option<Vec<String>> {
    let mut seen = BTreeSet::new();
    items
        .iter()
        .map(|item| {
            let id = item.get("id")?.as_str()?;
            seen.insert(id).then(|| id.to_owned())
        })
        .collect()
}

/// A longest common subsequence of two sequences of unique ids. With
/// unique ids it is the longest increasing run of `before` positions taken
/// in `after` order (patience sorting, O(n log n)).
fn common_ids<'a>(before: &'a [String], after: &[String]) -> BTreeSet<&'a str> {
    let position: BTreeMap<&str, usize> = before
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    let seq: Vec<usize> = after
        .iter()
        .filter_map(|id| position.get(id.as_str()).copied())
        .collect();
    // tails[k]: the index into `seq` ending the best increasing run of length k + 1.
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; seq.len()];
    for i in 0..seq.len() {
        let k = tails.partition_point(|&t| seq[t] < seq[i]);
        if k > 0 {
            prev[i] = Some(tails[k - 1]);
        }
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut kept = BTreeSet::new();
    let mut cursor = tails.last().copied();
    while let Some(i) = cursor {
        kept.insert(before[seq[i]].as_str());
        cursor = prev[i];
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::ProjectBuilder;
    use serde_json::json;

    fn apply(doc: &Value, patch: &Patch) -> Value {
        let mut doc = doc.clone();
        json_patch::patch(&mut doc, patch).unwrap();
        doc
    }

    /// `diff` both ways, applied both ways.
    fn round_trip(a: &Value, b: &Value) -> (Patch, Patch) {
        let forward = diff(a, b);
        let inverse = diff(b, a);
        assert_eq!(&apply(a, &forward), b);
        assert_eq!(&apply(b, &inverse), a);
        assert_tests_guard(&forward);
        assert_tests_guard(&inverse);
        (forward, inverse)
    }

    /// Every `replace`/`remove` is right after a `test` of the same path.
    fn assert_tests_guard(patch: &Patch) {
        for (i, op) in patch.0.iter().enumerate() {
            if matches!(op, PatchOperation::Replace(_) | PatchOperation::Remove(_)) {
                let guard = i.checked_sub(1).map(|j| &patch.0[j]);
                assert!(
                    matches!(guard, Some(PatchOperation::Test(t)) if t.path == *op.path()),
                    "op {i} ({op}) isn't guarded: {patch}"
                );
            }
        }
    }

    fn clips(ids: &[&str]) -> Value {
        json!({"clips": ids.iter().map(|id| json!({"id": id, "in": 0, "out": 10})).collect::<Vec<_>>()})
    }

    fn op_names(patch: &Patch) -> Vec<String> {
        patch
            .0
            .iter()
            .map(|op| {
                let name = match op {
                    PatchOperation::Add(_) => "add",
                    PatchOperation::Remove(_) => "remove",
                    PatchOperation::Replace(_) => "replace",
                    PatchOperation::Move(_) => "move",
                    PatchOperation::Copy(_) => "copy",
                    PatchOperation::Test(_) => "test",
                };
                format!("{name} {}", op.path())
            })
            .collect()
    }

    #[test]
    fn equal_values_give_an_empty_patch() {
        let a = json!({"name": "x", "clips": [{"id": "a"}]});
        assert!(diff(&a, &a).0.is_empty());
    }

    #[test]
    fn scalars_are_replaced_after_a_test() {
        let a = json!({"name": "x", "keep": {"a": 1}, "list": [1, 2]});
        let b = json!({"name": "y", "keep": {"a": 1}, "list": [2]});
        let (forward, _) = round_trip(&a, &b);
        assert_eq!(
            op_names(&forward),
            ["test /list", "replace /list", "test /name", "replace /name"]
        );
        assert_eq!(
            forward.0[0],
            PatchOperation::Test(TestOperation {
                path: PointerBuf::parse("/list").unwrap(),
                value: json!([1, 2]),
            })
        );
    }

    #[test]
    fn objects_with_other_members_are_replaced_whole() {
        let a = json!({"source": {"kind": "screen", "output": "DP-1"}});
        let b = json!({"source": {"kind": "camera", "device": "/dev/video0"}});
        let (forward, _) = round_trip(&a, &b);
        assert_eq!(op_names(&forward), ["test /source", "replace /source"]);
        // Different roots are replaced at "".
        let (forward, _) = round_trip(&json!([1]), &json!({"a": 1}));
        assert_eq!(op_names(&forward), ["test ", "replace "]);
    }

    #[test]
    fn id_arrays_are_matched_by_id() {
        // Remove one, insert one, change one field.
        let a = clips(&["a", "b", "c", "d"]);
        let mut b = clips(&["a", "c", "x", "d"]);
        b["clips"][1]["out"] = json!(7);
        let (forward, inverse) = round_trip(&a, &b);
        assert_eq!(
            op_names(&forward),
            [
                "test /clips/1",
                "remove /clips/1",
                "test /clips/1/out",
                "replace /clips/1/out",
                "add /clips/2"
            ]
        );
        assert_eq!(
            op_names(&inverse),
            [
                "test /clips/2",
                "remove /clips/2",
                "add /clips/1",
                "test /clips/2/out",
                "replace /clips/2/out"
            ]
        );
    }

    #[test]
    fn moves_are_a_remove_and_an_add() {
        let (forward, _) = round_trip(&clips(&["a", "b", "c", "d"]), &clips(&["b", "c", "d", "a"]));
        assert_eq!(
            op_names(&forward),
            ["test /clips/0", "remove /clips/0", "add /clips/3"]
        );
        let (forward, _) = round_trip(&clips(&["a", "b"]), &clips(&["b", "a"]));
        assert_eq!(forward.0.len(), 3, "a swap moves one element");
        round_trip(
            &clips(&["a", "b", "c", "d", "e"]),
            &clips(&["e", "d", "c", "b", "a"]),
        );
        round_trip(&clips(&[]), &clips(&["a", "b"]));
        round_trip(&clips(&["a", "b"]), &clips(&[]));
    }

    #[test]
    fn arrays_without_unique_ids_are_replaced_whole() {
        let a = json!({"clips": [{"id": "a"}, {"id": "a"}]});
        let b = json!({"clips": [{"id": "a"}]});
        let (forward, _) = round_trip(&a, &b);
        assert_eq!(op_names(&forward), ["test /clips", "replace /clips"]);
        let a = json!({"clips": [{"id": "a"}, {"name": "no id"}]});
        let (forward, _) = round_trip(&a, &b);
        assert_eq!(op_names(&forward), ["test /clips", "replace /clips"]);
    }

    #[test]
    fn keys_with_slashes_are_escaped() {
        let a = json!({"panels": {"a/b": 1, "c~d": 2}});
        let b = json!({"panels": {"a/b": 3, "c~d": 4}});
        let (forward, _) = round_trip(&a, &b);
        assert_eq!(
            op_names(&forward),
            [
                "test /panels/a~1b",
                "replace /panels/a~1b",
                "test /panels/c~0d",
                "replace /panels/c~0d"
            ]
        );
    }

    #[test]
    fn longest_common_subsequence() {
        let ids = |s: &str| s.chars().map(String::from).collect::<Vec<_>>();
        let lcs = |a: &str, b: &str| {
            let (a, b) = (ids(a), ids(b));
            common_ids(&a, &b)
                .into_iter()
                .map(str::to_owned)
                .collect::<String>()
        };
        assert_eq!(lcs("abcd", "abcd"), "abcd");
        assert_eq!(lcs("abcd", "bcda"), "bcd");
        assert_eq!(lcs("abcdef", "axcyfz"), "acf");
        assert_eq!(lcs("", "abc"), "");
        assert_eq!(lcs("abc", "cba").len(), 1);
    }

    #[test]
    fn views_are_aligned_over_the_same_takes() {
        let prev = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 100)
            .take("take-0002", 100)
            .build();
        let mut next = prev.clone();
        next.takes.remove(&"take-0001".parse().unwrap());
        let mut new_take = next.takes.values().next().unwrap().clone();
        new_take.scene.layers[0].opacity = 0.5;
        next.takes.insert("take-0003".parse().unwrap(), new_take);
        next.takes.values_mut().next().unwrap().scene.layers[0].visible = false;

        let (before, after) = aligned_views(&prev, &next);
        let keys = |v: &Value| {
            v["takes"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&before), ["take-0002", "take-0003"]);
        assert_eq!(keys(&after), keys(&before));
        assert_eq!(before["takes"]["take-0003"], after["takes"]["take-0003"]);
        assert_eq!(
            op_names(&diff(&before, &after)),
            [
                "test /takes/take-0002/scene/layers/0/visible",
                "replace /takes/take-0002/scene/layers/0/visible"
            ]
        );
    }
}
