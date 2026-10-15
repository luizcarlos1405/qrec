//! `qrec schema`: the JSON Schema of `qrec.json` (SPEC §6.1). Owner: meta.
//!
//! The schema is derived from `Project` with schemars (draft 2020-12), then
//! refined with the limits `validate` enforces that JSON Schema can express
//! (id and color patterns, canvas and gain ranges, …), so editors and
//! agents catch most mistakes before Qrec does. Rules that need the whole
//! document (clips within their take, unique ids) stay in `validate` and
//! are summarized in the root description.

use serde_json::{Value, json};

use crate::model::{CURRENT_VERSION, Project};
use crate::validate::{
    MAX_AUTOCUT_MS, MAX_CANVAS, MAX_COUNTDOWN_SECS, MAX_GAIN_DB, MAX_SYNC_OFFSET_MS, MIN_CANVAS,
    MIN_THRESHOLD_DB,
};

/// Layer, track, clip and stream ids (`ids::is_short_id`).
pub const SHORT_ID_PATTERN: &str = "^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$";
/// `take-0001`.
pub const TAKE_ID_PATTERN: &str = "^take-[0-9]{4,}$";
/// `#RRGGBB`.
pub const COLOR_PATTERN: &str = "^#[0-9A-Fa-f]{6}$";
/// `takes/<take>/<name>` (the take must be the stream's own; `validate` checks it).
pub const STREAM_FILE_PATTERN: &str = "^takes/take-[0-9]{4,}/[A-Za-z0-9._-]+$";

const DESCRIPTION: &str = "A Qrec project: the qrec.json of a project folder (schema v1). \
Times are integer frames at the canvas fps. The timeline is one track of clips played back to \
back; each clip is a half-open range [in, out) of a take. `takes` is the media pool: facts about \
the recorded files that undo never changes. `scene` is the layout new takes copy; each take \
keeps its own snapshot. Qrec also checks what this schema can't: clip ids are unique and each \
clip uses an existing `ok` take with out <= durationFrames; layer and track ids are unique in a \
scene; a stream file lives in its own take's folder; nextTakeNumber is above every take number. \
Prefer `qrec` commands over hand edits: they keep the undo history.";

/// The extra constraints, by JSON Pointer into the derived schema.
fn refinements() -> Vec<(&'static str, Value)> {
    let size = json!({ "minimum": MIN_CANVAS, "maximum": MAX_CANVAS, "multipleOf": 2 });
    let short_id = json!({ "pattern": SHORT_ID_PATTERN });
    let ms = json!({ "maximum": MAX_AUTOCUT_MS });
    vec![
        (
            "",
            json!({ "title": "qrec.json", "description": DESCRIPTION }),
        ),
        ("/properties/version", json!({ "const": CURRENT_VERSION })),
        ("/properties/nextTakeNumber", json!({ "minimum": 1 })),
        (
            "/properties/takes",
            json!({ "propertyNames": { "pattern": TAKE_ID_PATTERN } }),
        ),
        ("/$defs/Canvas/properties/width", size.clone()),
        ("/$defs/Canvas/properties/height", size),
        (
            "/$defs/Fps",
            json!({ "description": "A frame rate as a rational number (`30/1`, `30000/1001`); the canvas allows up to 240 fps." }),
        ),
        ("/$defs/Fps/properties/num", json!({ "minimum": 1 })),
        ("/$defs/Fps/properties/den", json!({ "minimum": 1 })),
        ("/$defs/Color", json!({ "pattern": COLOR_PATTERN })),
        ("/$defs/TakeId", json!({ "pattern": TAKE_ID_PATTERN })),
        ("/$defs/LayerId", short_id.clone()),
        ("/$defs/TrackId", short_id.clone()),
        ("/$defs/ClipId", short_id.clone()),
        ("/$defs/Stream/properties/id", short_id),
        (
            "/$defs/Stream/properties/file",
            json!({ "pattern": STREAM_FILE_PATTERN }),
        ),
        (
            "/$defs/Layer/properties/opacity",
            json!({ "minimum": 0, "maximum": 1 }),
        ),
        ("/$defs/Rect/properties/w", json!({ "minimum": 1 })),
        ("/$defs/Rect/properties/h", json!({ "minimum": 1 })),
        (
            "/$defs/AudioTrack/properties/gainDb",
            json!({ "minimum": -MAX_GAIN_DB, "maximum": MAX_GAIN_DB }),
        ),
        (
            "/$defs/AudioTrack/properties/syncOffsetMs",
            json!({ "minimum": -MAX_SYNC_OFFSET_MS, "maximum": MAX_SYNC_OFFSET_MS }),
        ),
        (
            "/$defs/RecordingSettings/properties/countdownSecs",
            json!({ "maximum": MAX_COUNTDOWN_SECS }),
        ),
        (
            "/$defs/AutocutSettings/properties/thresholdDb",
            json!({ "minimum": MIN_THRESHOLD_DB, "maximum": 0 }),
        ),
        ("/$defs/AutocutSettings/properties/minSilenceMs", ms.clone()),
        ("/$defs/AutocutSettings/properties/paddingMs", ms.clone()),
        ("/$defs/AutocutSettings/properties/minKeepMs", ms),
    ]
}

/// The JSON Schema of `Project` (schemars), refined with the limits of
/// `validate`.
pub fn project_schema() -> Value {
    let mut schema = schemars::schema_for!(Project).to_value();
    for (pointer, extra) in refinements() {
        if let (Some(Value::Object(node)), Value::Object(extra)) =
            (schema.pointer_mut(pointer), extra)
        {
            node.extend(extra);
        }
    }
    schema
}

/// A minimal JSON Schema checker for tests: the draft 2020-12 keywords
/// [`project_schema`] uses (`$ref`, `type`, `properties`, `required`,
/// `additionalProperties`, `propertyNames`, `items`, `enum`, `const`,
/// `oneOf`/`anyOf`/`allOf`, `minimum`/`maximum`/`multipleOf`, `pattern`,
/// and the `date-time`/`uint32`/`int32`/`uint64`/`int64` formats).
#[cfg(any(test, feature = "testkit"))]
pub mod check {
    use serde_json::Value;

    /// Every way `instance` breaks `schema` (empty when it conforms).
    pub fn check(schema: &Value, instance: &Value) -> Vec<String> {
        let mut errors = Vec::new();
        walk(schema, schema, instance, "", &mut errors);
        errors
    }

    fn walk(root: &Value, schema: &Value, inst: &Value, path: &str, errors: &mut Vec<String>) {
        let Some(s) = schema.as_object() else {
            if schema == &Value::Bool(false) {
                errors.push(format!("{path}: no value is allowed here"));
            }
            return;
        };
        let keyword = |name: &str| s.get(name);
        if let Some(reference) = keyword("$ref").and_then(Value::as_str) {
            match reference.strip_prefix('#').and_then(|p| root.pointer(p)) {
                Some(target) => walk(root, target, inst, path, errors),
                None => errors.push(format!("{path}: unresolved $ref {reference}")),
            }
        }
        let mut problems: Vec<String> = Vec::new();
        if let Some(types) = keyword("type") {
            let names: Vec<&str> = match types {
                Value::String(t) => vec![t.as_str()],
                Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            if !names.iter().any(|t| has_type(inst, t)) {
                problems.push(format!("{inst} is not of type {}", names.join(" or ")));
            }
        }
        if let Some(expected) = keyword("const").filter(|c| *c != inst) {
            problems.push(format!("{inst} is not {expected}"));
        }
        if let Some(options) = keyword("enum")
            .and_then(Value::as_array)
            .filter(|o| !o.contains(inst))
        {
            problems.push(format!(
                "{inst} is not one of {}",
                Value::Array(options.clone())
            ));
        }
        if let Some(n) = inst.as_f64() {
            if let Some(min) = keyword("minimum")
                .and_then(Value::as_f64)
                .filter(|min| n < *min)
            {
                problems.push(format!("{inst} is below {min}"));
            }
            if let Some(max) = keyword("maximum")
                .and_then(Value::as_f64)
                .filter(|max| n > *max)
            {
                problems.push(format!("{inst} is above {max}"));
            }
            if let Some(m) = keyword("multipleOf")
                .and_then(Value::as_f64)
                .filter(|m| (n / m).fract() != 0.0)
            {
                problems.push(format!("{inst} is not a multiple of {m}"));
            }
        }
        if let Some(format) = keyword("format").and_then(Value::as_str) {
            let ok = match format {
                "date-time" => inst
                    .as_str()
                    .is_none_or(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok()),
                "uint32" => {
                    !inst.is_number() || inst.as_u64().is_some_and(|n| n <= u64::from(u32::MAX))
                }
                "int32" => {
                    !inst.is_number() || inst.as_i64().is_some_and(|n| i32::try_from(n).is_ok())
                }
                "uint64" => !inst.is_number() || inst.is_u64(),
                "int64" => !inst.is_number() || inst.is_i64(),
                _ => true,
            };
            if !ok {
                problems.push(format!("{inst} is not a valid {format}"));
            }
        }
        if let (Some(text), Some(pattern)) =
            (inst.as_str(), keyword("pattern").and_then(Value::as_str))
        {
            match super::regex::is_match(pattern, text) {
                Ok(true) => {}
                Ok(false) => problems.push(format!("{inst} doesn't match {pattern}")),
                Err(e) => problems.push(format!("bad pattern {pattern}: {e}")),
            }
        }
        if let Some(object) = inst.as_object() {
            let required = keyword("required").and_then(Value::as_array);
            for name in required.into_iter().flatten().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    problems.push(format!("missing required property {name}"));
                }
            }
            let properties = keyword("properties").and_then(Value::as_object);
            for (key, value) in object {
                let child = format!("{path}/{key}");
                if let Some(names) = keyword("propertyNames") {
                    walk(root, names, &Value::String(key.clone()), &child, errors);
                }
                match (
                    properties.and_then(|p| p.get(key)),
                    keyword("additionalProperties"),
                ) {
                    (Some(sub), _) => walk(root, sub, value, &child, errors),
                    (None, Some(Value::Bool(false))) => {
                        problems.push(format!("unknown property {key}"))
                    }
                    (None, Some(sub)) => walk(root, sub, value, &child, errors),
                    (None, None) => {}
                }
            }
        }
        if let (Some(items), Some(sub)) = (inst.as_array(), keyword("items")) {
            for (i, item) in items.iter().enumerate() {
                walk(root, sub, item, &format!("{path}/{i}"), errors);
            }
        }
        for sub in keyword("allOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            walk(root, sub, inst, path, errors);
        }
        let passes = |sub: &Value| {
            let mut errors = Vec::new();
            walk(root, sub, inst, path, &mut errors);
            errors.is_empty()
        };
        if let Some(any) = keyword("anyOf").and_then(Value::as_array)
            && !any.iter().any(passes)
        {
            problems.push(format!("{inst} matches none of anyOf"));
        }
        if let Some(one) = keyword("oneOf").and_then(Value::as_array) {
            let matching = one.iter().filter(|sub| passes(sub)).count();
            if matching != 1 {
                problems.push(format!("{inst} matches {matching} of oneOf (expected 1)"));
            }
        }
        errors.extend(problems.into_iter().map(|p| format!("{path}: {p}")));
    }

    fn has_type(inst: &Value, name: &str) -> bool {
        match name {
            "object" => inst.is_object(),
            "array" => inst.is_array(),
            "string" => inst.is_string(),
            "boolean" => inst.is_boolean(),
            "null" => inst.is_null(),
            "number" => inst.is_number(),
            "integer" => {
                inst.is_i64() || inst.is_u64() || inst.as_f64().is_some_and(|f| f.fract() == 0.0)
            }
            _ => false,
        }
    }
}

/// A tiny regular-expression matcher for the schema's patterns: `^`, `$`,
/// literals, `\`-escapes, `.`, classes `[...]` with ranges, and the
/// quantifiers `?`, `*`, `+`, `{n}`, `{n,}`, `{n,m}`.
#[cfg(any(test, feature = "testkit"))]
mod regex {
    enum Atom {
        Any,
        Char(char),
        Class(Vec<(char, char)>),
    }

    impl Atom {
        fn matches(&self, c: char) -> bool {
            match self {
                Atom::Any => true,
                Atom::Char(x) => *x == c,
                Atom::Class(ranges) => ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c)),
            }
        }
    }

    struct Piece {
        atom: Atom,
        min: usize,
        max: usize,
    }

    fn parse(body: &str) -> Result<Vec<Piece>, String> {
        let chars: Vec<char> = body.chars().collect();
        let mut pieces = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let atom = match chars[i] {
                '\\' => {
                    i += 1;
                    Atom::Char(*chars.get(i).ok_or("dangling \\")?)
                }
                '.' => Atom::Any,
                '[' => {
                    let mut ranges = Vec::new();
                    i += 1;
                    while chars.get(i) != Some(&']') {
                        let mut lo = *chars.get(i).ok_or("unclosed [")?;
                        if lo == '\\' {
                            i += 1;
                            lo = *chars.get(i).ok_or("dangling \\")?;
                        }
                        if chars.get(i + 1) == Some(&'-')
                            && chars.get(i + 2).is_some_and(|c| *c != ']')
                        {
                            ranges.push((lo, chars[i + 2]));
                            i += 3;
                        } else {
                            ranges.push((lo, lo));
                            i += 1;
                        }
                    }
                    Atom::Class(ranges)
                }
                '(' | ')' | '|' | '*' | '+' | '?' | '{' => {
                    return Err(format!("unsupported {:?} at {i}", chars[i]));
                }
                c => Atom::Char(c),
            };
            i += 1;
            let (min, max) = match chars.get(i) {
                Some('?') => (0, 1),
                Some('*') => (0, usize::MAX),
                Some('+') => (1, usize::MAX),
                Some('{') => {
                    let end = chars[i..]
                        .iter()
                        .position(|c| *c == '}')
                        .ok_or("unclosed {")?
                        + i;
                    let inner: String = chars[i + 1..end].iter().collect();
                    let bad = || format!("bad quantifier {{{inner}}}");
                    let (min, max) = match inner.split_once(',') {
                        None => {
                            let n = inner.parse().map_err(|_| bad())?;
                            (n, n)
                        }
                        Some((lo, "")) => (lo.parse().map_err(|_| bad())?, usize::MAX),
                        Some((lo, hi)) => (
                            lo.parse().map_err(|_| bad())?,
                            hi.parse().map_err(|_| bad())?,
                        ),
                    };
                    i = end;
                    (min, max)
                }
                _ => {
                    pieces.push(Piece {
                        atom,
                        min: 1,
                        max: 1,
                    });
                    continue;
                }
            };
            i += 1;
            pieces.push(Piece { atom, min, max });
        }
        Ok(pieces)
    }

    fn match_here(pieces: &[Piece], text: &[char], to_end: bool) -> bool {
        let Some((piece, rest)) = pieces.split_first() else {
            return !to_end || text.is_empty();
        };
        let mut n = 0;
        while n < piece.max && n < text.len() && piece.atom.matches(text[n]) {
            n += 1;
        }
        n >= piece.min
            && (piece.min..=n)
                .rev()
                .any(|k| match_here(rest, &text[k..], to_end))
    }

    /// Whether `pattern` matches somewhere in `text` (anchors as written).
    pub fn is_match(pattern: &str, text: &str) -> Result<bool, String> {
        let (from_start, body) = match pattern.strip_prefix('^') {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let (to_end, body) = match body.strip_suffix('$') {
            Some(rest) if !rest.ends_with('\\') => (true, rest),
            _ => (false, body),
        };
        let pieces = parse(body)?;
        let chars: Vec<char> = text.chars().collect();
        let starts = if from_start { 0 } else { chars.len() };
        Ok((0..=starts).any(|s| match_here(&pieces, &chars[s..], to_end)))
    }

    #[cfg(test)]
    mod tests {
        use super::is_match;

        #[test]
        fn patterns() {
            let m = |p: &str, t: &str| is_match(p, t).unwrap();
            assert!(m("^#[0-9A-Fa-f]{6}$", "#12170f"));
            assert!(!m("^#[0-9A-Fa-f]{6}$", "#12170"));
            assert!(!m("^#[0-9A-Fa-f]{6}$", "#12170fa"));
            assert!(m("^take-[0-9]{4,}$", "take-0001"));
            assert!(m("^take-[0-9]{4,}$", "take-12345"));
            assert!(!m("^take-[0-9]{4,}$", "take-001"));
            assert!(m("^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$", "intro_part-2"));
            assert!(!m("^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$", "-x"));
            assert!(!m("^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$", &"a".repeat(33)));
            assert!(m("^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$", &"a".repeat(32)));
            assert!(m(
                "^takes/take-[0-9]{4,}/[A-Za-z0-9._-]+$",
                "takes/take-0001/L1-screen-DP-4.mkv"
            ));
            assert!(!m(
                "^takes/take-[0-9]{4,}/[A-Za-z0-9._-]+$",
                "takes/take-0001/a b"
            ));
            assert!(!m(
                "^takes/take-[0-9]{4,}/[A-Za-z0-9._-]+$",
                "takes/take-0001/"
            ));
            assert!(m("b+c?", "aabbx"), "unanchored patterns search");
            assert!(m("^a.c$", "abc"));
            assert!(m("^a\\.c$", "a.c") && !m("^a\\.c$", "abc"));
            assert!(is_match("(a|b)", "a").is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::check::check;
    use super::*;

    const SPEC_EXAMPLE: &str = include_str!("../tests/fixtures/spec_example.json");

    fn spec() -> Value {
        serde_json::from_str(SPEC_EXAMPLE).unwrap()
    }

    #[test]
    fn every_refinement_lands() {
        let plain = schemars::schema_for!(Project).to_value();
        for (pointer, _) in refinements() {
            assert!(
                plain.pointer(pointer).is_some(),
                "{pointer} is not in the derived schema"
            );
        }
        let schema = project_schema();
        assert_eq!(schema["title"], "qrec.json");
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(schema["properties"]["version"]["const"], CURRENT_VERSION);
        assert_eq!(
            schema["$defs"]["Canvas"]["properties"]["width"]["maximum"],
            8192
        );
        assert_eq!(schema["$defs"]["Color"]["pattern"], COLOR_PATTERN);
        assert_eq!(schema["$defs"]["Clip"]["additionalProperties"], false);
    }

    #[test]
    fn the_spec_example_conforms() {
        let errors = check(&project_schema(), &spec());
        assert!(errors.is_empty(), "{errors:#?}");
    }

    #[test]
    fn mistakes_are_caught() {
        let cases: Vec<(&str, Value, &str)> = vec![
            ("/canvas/background", json!("#12345"), "doesn't match"),
            ("/canvas/width", json!(1921), "not a multiple of 2"),
            ("/canvas/height", json!(8), "below 16"),
            ("/canvas/fps/num", json!(0), "below 1"),
            ("/version", json!(2), "is not 1"),
            ("/scene/layers/0/opacity", json!(1.5), "above 1"),
            (
                "/scene/layers/0/fit",
                json!("stretch"),
                "matches 0 of oneOf",
            ),
            (
                "/scene/layers/0/source/kind",
                json!("window"),
                "matches 0 of oneOf",
            ),
            ("/scene/audio/0/gainDb", json!(-61), "below -60"),
            ("/scene/audio/0/syncOffsetMs", json!(5001), "above 5000"),
            ("/scene/audio/0/role", json!("speaker"), "is not one of"),
            ("/recording/countdownSecs", json!(11), "above 10"),
            ("/recording/encoder", json!("nvenc"), "matches 0 of oneOf"),
            ("/autocut/thresholdDb", json!(3), "above 0"),
            ("/autocut/minKeepMs", json!(60001), "above 60000"),
            ("/autocut/track", json!("A 1"), "matches none of anyOf"),
            (
                "/takes/take-0001/recordedAt",
                json!("yesterday"),
                "not a valid date-time",
            ),
            ("/takes/take-0001/status", json!("done"), "is not one of"),
            (
                "/takes/take-0001/streams/0/file",
                json!("/etc/passwd"),
                "doesn't match",
            ),
            ("/takes/take-0001/streams/0/width", json!(-1), "below 0"),
            ("/timeline/clips/0/id", json!("bad id"), "doesn't match"),
            ("/timeline/clips/0/in", json!(1.5), "not of type integer"),
            ("/timeline/clips/0/take", json!("take1"), "doesn't match"),
            ("/nextTakeNumber", json!(0), "below 1"),
        ];
        let schema = project_schema();
        for (pointer, value, expected) in cases {
            let mut doc = spec();
            *doc.pointer_mut(pointer)
                .unwrap_or_else(|| panic!("{pointer}")) = value.clone();
            let errors = check(&schema, &doc);
            assert!(
                errors.iter().any(|e| e.contains(expected)),
                "{pointer} = {value}: {errors:#?}"
            );
        }

        let mut doc = spec();
        doc["canvas"]["fsp"] = json!(30);
        assert!(
            check(&schema, &doc)
                .iter()
                .any(|e| e.contains("unknown property fsp"))
        );
        let mut doc = spec();
        doc.as_object_mut().unwrap().remove("timeline");
        assert!(
            check(&schema, &doc)
                .iter()
                .any(|e| e.contains("missing required property timeline"))
        );
        let mut doc = spec();
        let take = doc["takes"]["take-0001"].clone();
        doc["takes"]["take-1"] = take;
        assert!(
            check(&schema, &doc)
                .iter()
                .any(|e| e.contains("\"take-1\" doesn't match"))
        );
    }

    #[test]
    fn snapshot_schema() {
        insta::assert_json_snapshot!("project_schema", project_schema());
    }
}
