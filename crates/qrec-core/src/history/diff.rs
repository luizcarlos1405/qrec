//! Patches between undoable views (A§7.2). Owner: history.
//!
//! PLACEHOLDER: history owner replaces this (A§7.2). The real diff matches
//! `clips`/`layers`/`audio` by id with an LCS and guards every
//! `replace`/`remove` with a `test`. This version diffs objects member by
//! member, replaces differing arrays whole and emits no `test` ops. The
//! workspace builds `json-patch` without its `diff` feature, so this is a
//! small hand-written diff.

use json_patch::jsonptr::PointerBuf;
use json_patch::{AddOperation, Patch, PatchOperation, RemoveOperation, ReplaceOperation};
use serde_json::Value;

use crate::history::view::undoable_view;
use crate::model::Project;

/// The undoable views of `prev` and `next`, built over the same set of
/// takes: takes created by the step are copied into "before", takes gone
/// from "after" are dropped from both. Patches then only touch
/// `/takes/<id>/scene/…`, never whole takes.
pub fn aligned_views(prev: &Project, next: &Project) -> (Value, Value) {
    let mut before = undoable_view(prev);
    let mut after = undoable_view(next);
    if let (Some(Value::Object(b)), Some(Value::Object(a))) =
        (before.get_mut("takes"), after.get_mut("takes"))
    {
        for (id, take) in a.iter() {
            if !b.contains_key(id) {
                b.insert(id.clone(), take.clone());
            }
        }
        b.retain(|id, _| a.contains_key(id));
    }
    (before, after)
}

/// The patch that turns `before` into `after`.
pub fn diff(before: &Value, after: &Value) -> Patch {
    let mut ops = Vec::new();
    diff_into(&mut PointerBuf::root(), before, after, &mut ops);
    Patch(ops)
}

fn diff_into(path: &mut PointerBuf, before: &Value, after: &Value, ops: &mut Vec<PatchOperation>) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(b), Value::Object(a)) => {
            for (key, old) in b {
                match a.get(key) {
                    Some(new) => {
                        path.push_back(key.as_str());
                        diff_into(path, old, new, ops);
                        path.pop_back();
                    }
                    None => ops.push(PatchOperation::Remove(RemoveOperation {
                        path: path.with_trailing_token(key.as_str()),
                    })),
                }
            }
            for (key, new) in a {
                if !b.contains_key(key) {
                    ops.push(PatchOperation::Add(AddOperation {
                        path: path.with_trailing_token(key.as_str()),
                        value: new.clone(),
                    }));
                }
            }
        }
        _ => ops.push(PatchOperation::Replace(ReplaceOperation {
            path: path.clone(),
            value: after.clone(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patches_apply_both_ways() {
        let a = json!({"name": "x", "timeline": {"clips": [1, 2]}, "gone": true, "keep": {"a": 1}});
        let b = json!({"name": "y", "timeline": {"clips": [2]}, "new": null, "keep": {"a": 1}});
        let mut doc = a.clone();
        json_patch::patch(&mut doc, &diff(&a, &b)).unwrap();
        assert_eq!(doc, b);
        json_patch::patch(&mut doc, &diff(&b, &a)).unwrap();
        assert_eq!(doc, a);
        assert!(diff(&a, &a).0.is_empty());
    }
}
