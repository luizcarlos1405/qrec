//! Properties of the meta modules (A§11): migration round trip, project
//! validation and schema conformance of arbitrary projects, query views,
//! the chord grammar, keymap overrides and conflicts, and config edits.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use proptest::sample::{Index, select};
use qrec_core::config;
use qrec_core::keymap::{self, KeyChord, KeyContext};
use qrec_core::migrate::{self, Step};
use qrec_core::protocol::reply::{ClipRow, StatusView, TakeRow};
use qrec_core::schema::{self, check::check};
use qrec_core::state::{RecordMode, RecordPhase};
use qrec_core::testkit::{self, strategies::arb_project};
use qrec_core::validate::{Severity, validate};
use qrec_core::{CoreError, Project, Query, TakeId, TakeStatus};
use serde_json::{Value, json};

// ------------------------------------------------------------- migration

/// A synthetic legacy schema for the framework test: "v1" stored the fps as
/// `"num/den"`, the takes as an array with their id inside, and the clips as
/// `[id, take, in, out]` rows under `edit`. "v2" is today's shape.
fn downgrade(mut value: Value) -> Value {
    let object = value.as_object_mut().unwrap();
    let fps = &object["canvas"]["fps"];
    let text = format!("{}/{}", fps["num"], fps["den"]);
    object["canvas"]["fps"] = json!(text);
    let takes: Vec<Value> = object["takes"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, take)| {
            let mut take = take.clone();
            take["id"] = json!(id);
            take
        })
        .collect();
    object.insert("takes".into(), json!(takes));
    let clips: Vec<Value> = object["timeline"]["clips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| json!([c["id"], c["take"], c["in"], c["out"]]))
        .collect();
    object.remove("timeline");
    object.insert("edit".into(), json!(clips));
    object.insert("version".into(), json!(1));
    value
}

fn v1_to_v2(mut value: Value) -> Result<Value, CoreError> {
    let bad = |what: &str| CoreError::invalid(format!("bad v1 {what}"));
    let object = value.as_object_mut().ok_or_else(|| bad("document"))?;
    let fps = object["canvas"]["fps"]
        .as_str()
        .ok_or_else(|| bad("fps"))?
        .to_owned();
    let (num, den) = fps.split_once('/').ok_or_else(|| bad("fps"))?;
    let parse = |n: &str| n.parse::<u32>().map_err(|_| bad("fps"));
    object["canvas"]["fps"] = json!({ "num": parse(num)?, "den": parse(den)? });
    let mut takes = serde_json::Map::new();
    for take in object["takes"]
        .as_array()
        .ok_or_else(|| bad("takes"))?
        .clone()
    {
        let mut take = take;
        let id = take
            .as_object_mut()
            .and_then(|t| t.remove("id"))
            .ok_or_else(|| bad("take"))?;
        takes.insert(id.as_str().ok_or_else(|| bad("take id"))?.to_owned(), take);
    }
    object.insert("takes".into(), Value::Object(takes));
    let clips: Vec<Value> = object
        .remove("edit")
        .and_then(|e| e.as_array().cloned())
        .ok_or_else(|| bad("edit"))?
        .into_iter()
        .map(|row| json!({ "id": row[0], "take": row[1], "in": row[2], "out": row[3] }))
        .collect();
    object.insert("timeline".into(), json!({ "clips": clips }));
    object.insert("version".into(), json!(2));
    Ok(value)
}

const SYNTHETIC: &[Step] = &[v1_to_v2];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn migration_round_trips(project in arb_project()) {
        // At the current version a file loads unchanged, as a value or as text.
        let value = serde_json::to_value(&project).unwrap();
        let migrated = migrate::migrate(value.clone()).unwrap();
        prop_assert_eq!(&migrated.project, &project);
        prop_assert_eq!(migrated.from_version, 1);
        prop_assert_eq!(migrate::parse_project(&value.to_string()).unwrap(), migrated);

        // Through a synthetic v1 → v2 step: upgrade ∘ downgrade = id.
        let mut v2 = value;
        v2["version"] = json!(2);
        let v1 = downgrade(v2.clone());
        prop_assert_ne!(&v1, &v2);
        let (upgraded, from) = migrate::upgrade(v1.clone(), SYNTHETIC).unwrap();
        prop_assert_eq!(from, 1);
        prop_assert_eq!(&upgraded, &v2);
        let mut expected = project;
        expected.version = 2;
        let migrated = migrate::migrate_with(v1, SYNTHETIC).unwrap();
        prop_assert_eq!(&migrated.project, &expected);
        prop_assert_eq!(migrated.from_version, 1);
        // A v2 file is already current for that table.
        prop_assert_eq!(migrate::upgrade(v2.clone(), SYNTHETIC).unwrap(), (v2, 2));
    }

    #[test]
    fn newer_versions_are_refused(project in arb_project(), newer in 2u32..1000) {
        let mut value = serde_json::to_value(&project).unwrap();
        value["version"] = json!(newer);
        let err = migrate::migrate(value).unwrap_err();
        prop_assert_eq!(err.code, qrec_core::ErrorCode::Invalid);
        let needle = format!("version {}", newer);
        prop_assert!(err.message.contains(&needle), "{}", err.message);
    }
}

// ------------------------------------------------- validation and schema

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn arbitrary_projects_are_valid_and_match_the_schema(project in arb_project()) {
        let issues = validate(&project);
        prop_assert!(issues.is_empty(), "{:#?}", issues);
        let errors = check(&schema::project_schema(), &serde_json::to_value(&project).unwrap());
        prop_assert!(errors.is_empty(), "{:#?}", errors);
    }

    #[test]
    fn clips_past_their_take_are_errors(project in arb_project(), pick in any::<Index>(), extra in 1u64..1000) {
        prop_assume!(!project.timeline.clips.is_empty());
        let mut project = project;
        let i = pick.index(project.timeline.clips.len());
        let take = project.timeline.clips[i].take.clone();
        let duration = project.takes[&take].duration_frames;
        project.timeline.clips[i].out = duration + extra;
        let path = format!("/timeline/clips/{i}/out");
        prop_assert!(
            validate(&project).iter().any(|issue| issue.severity == Severity::Error && issue.path == path)
        );
    }

    #[test]
    fn issues_point_into_the_document(project in arb_project(), version in 0u32..5, width in 0u32..10_000) {
        let mut project = project;
        project.version = version;
        project.canvas.width = width;
        project.recording.countdown_secs = width % 20;
        let value = serde_json::to_value(&project).unwrap();
        for issue in validate(&project) {
            // Every path resolves in qrec.json, except members that only the
            // explicit (`missing`, `locked`) forms would carry.
            prop_assert!(value.pointer(&issue.path).is_some(), "{}", issue);
        }
    }
}

// ----------------------------------------------------------------- queries

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn query_views_are_consistent(project in arb_project(), recording in any::<bool>()) {
        let mut state = testkit::state(project);
        if recording {
            // A take being recorded is never in the bin.
            let id = TakeId::numbered(state.project.next_take_number);
            let mut take = state.project.takes.values().next().unwrap().clone();
            take.status = TakeStatus::Recording;
            state.project.takes.insert(id.clone(), take);
            state.project.next_take_number += 1;
            state.runtime.record = RecordPhase::Recording {
                take: id,
                mode: RecordMode::Append,
                elapsed_ns: 0,
                waiters: vec![],
            };
        }
        let ctx = testkit::ctx(testkit::CREATED_AT, 1);
        let clips: Vec<ClipRow> =
            serde_json::from_value(qrec_core::query(&state, &ctx, &Query::ClipList).unwrap()).unwrap();
        let mut position = 0;
        for (row, clip) in clips.iter().zip(&state.project.timeline.clips) {
            prop_assert_eq!(&row.id, &clip.id);
            prop_assert_eq!(row.start, position);
            prop_assert_eq!(row.duration, clip.out - clip.in_);
            position += row.duration;
        }
        prop_assert_eq!(clips.len(), state.project.timeline.clips.len());

        let status: StatusView =
            serde_json::from_value(qrec_core::query(&state, &ctx, &Query::Status).unwrap()).unwrap();
        prop_assert_eq!(status.duration_frames, position);
        prop_assert_eq!(status.clips, clips.len());
        prop_assert_eq!(status.takes, state.project.takes.len());

        let takes: Vec<TakeRow> = serde_json::from_value(
            qrec_core::query(&state, &ctx, &Query::TakeList { unused_only: false }).unwrap(),
        )
        .unwrap();
        prop_assert_eq!(takes.iter().map(|t| t.clips).sum::<usize>(), clips.len());
        for row in &takes {
            prop_assert_eq!(row.unused, row.clips == 0 && row.status != TakeStatus::Recording);
        }
        let bin: Vec<TakeRow> = serde_json::from_value(
            qrec_core::query(&state, &ctx, &Query::TakeList { unused_only: true }).unwrap(),
        )
        .unwrap();
        let expected: Vec<&TakeRow> = takes.iter().filter(|t| t.unused).collect();
        prop_assert_eq!(bin.iter().collect::<Vec<_>>(), expected);
        let numbers: Vec<u32> = takes.iter().map(|t| t.id.number()).collect();
        prop_assert!(numbers.windows(2).all(|w| w[0] < w[1]));
    }
}

// ------------------------------------------------------------------ chords

const NAMED: &[&str] = &[
    "Escape",
    "Tab",
    "space",
    "Return",
    "BackSpace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "Page_Up",
    "Page_Down",
    "Left",
    "Right",
    "Up",
    "Down",
    "F1",
    "F12",
    "F24",
];
const CHARS: &[&str] = &[
    "less",
    "greater",
    "comma",
    "period",
    "question",
    "colon",
    "bracketleft",
    "bracketright",
    "braceleft",
    "braceright",
    "equal",
    "minus",
    "plus",
    "backslash",
    "slash",
    "0",
    "7",
];

fn arb_chord() -> impl Strategy<Value = KeyChord> {
    let key = prop_oneof![
        (b'a'..=b'z').prop_map(|c| (char::from(c).to_string(), true)),
        select(NAMED).prop_map(|k| (k.to_owned(), true)),
        select(CHARS).prop_map(|k| (k.to_owned(), false)),
    ];
    (key, any::<[bool; 4]>()).prop_map(|((key, shift_ok), [ctrl, alt, shift, super_])| KeyChord {
        key,
        ctrl,
        alt,
        shift: shift && shift_ok,
        super_,
    })
}

proptest! {
    #[test]
    fn chords_round_trip(chord in arb_chord()) {
        let text = chord.to_string();
        prop_assert_eq!(KeyChord::parse(&text), Ok(chord.clone()));
        prop_assert_eq!(KeyChord::parse(&text.to_uppercase()), Ok(chord.clone()), "case-insensitive");
        prop_assert!(!chord.display().is_empty());
        // A GDK event with the same keyval and modifiers gives the same chord.
        prop_assert_eq!(
            KeyChord::from_gdk(&chord.key, chord.ctrl, chord.alt, chord.shift, chord.super_),
            Some(chord)
        );
    }

    #[test]
    fn overrides_apply_and_conflicts_match_lookup(
        picks in prop::collection::vec((any::<Index>(), prop::collection::vec(arb_chord(), 0..3)), 0..6)
    ) {
        let defaults = keymap::resolve("");
        let mut expected: BTreeMap<&str, Vec<KeyChord>> = BTreeMap::new();
        for (pick, chords) in &picks {
            let id = defaults.bindings[pick.index(defaults.bindings.len())].id;
            if expected.contains_key(id) {
                continue;
            }
            let mut unique: Vec<KeyChord> = Vec::new();
            for c in chords {
                if !unique.contains(c) {
                    unique.push(c.clone());
                }
            }
            expected.insert(id, unique);
        }
        let mut toml = String::from("[bindings]\n");
        for (id, chords) in &expected {
            let list: Vec<String> = chords.iter().map(|c| format!("{:?}", c.to_string())).collect();
            toml.push_str(&format!("\"{id}\" = [{}]\n", list.join(", ")));
        }
        let map = keymap::resolve(&toml);
        prop_assert!(map.errors.is_empty(), "{:?}\n{}", map.errors, toml);
        for b in &map.bindings {
            let want = expected.get(b.id).cloned().unwrap_or_else(|| defaults.binding(b.id).unwrap().keys.clone());
            prop_assert_eq!(&b.keys, &want, "{}", b.id);
        }
        // A chord is a conflict in a context exactly when lookup refuses it there.
        let chords: BTreeSet<&KeyChord> = map.bindings.iter().flat_map(|b| &b.keys).collect();
        for ctx in KeyContext::ALL {
            for chord in &chords {
                let users = map.bindings.iter().filter(|b| b.active_in(ctx) && b.keys.contains(chord)).count();
                let found = keymap::lookup(&map, chord, ctx);
                prop_assert_eq!(found.is_some(), users == 1, "{} in {:?}", chord, ctx);
                if users >= 2 {
                    let reported = map.conflicts.iter().any(|c| &&c.chord == chord && (c.context == ctx
                        || (matches!(ctx, KeyContext::Timeline | KeyContext::Canvas) && c.context == KeyContext::Global)));
                    prop_assert!(reported, "{} in {:?} is not reported", chord, ctx);
                }
            }
        }
        // Hints never advertise a conflicting chord.
        for ctx in keymap::HintContext::ALL {
            for hint in keymap::hints(ctx, &map) {
                for conflict in &map.conflicts {
                    let shown = conflict.chord.display();
                    prop_assert!(!hint.keys.split(' ').any(|k| k == shown), "{} shows {}", hint.label, shown);
                }
            }
        }
    }

    #[test]
    fn every_binding_has_an_action(
        project in arb_project(),
        playhead in 0u64..3000,
        selection in prop::collection::vec(any::<Index>(), 0..3),
        layer in any::<Option<Index>>(),
        program in any::<bool>(),
        phase in 0u8..4,
    ) {
        let mut state = testkit::state(project);
        state.ui.playhead = playhead;
        let clips = &state.project.timeline.clips;
        if !clips.is_empty() {
            state.ui.selection = selection.iter().map(|i| clips[i.index(clips.len())].id.clone()).collect();
        }
        let layers = &state.project.scene.layers;
        if let Some(i) = layer.filter(|_| !layers.is_empty()) {
            state.ui.selected_layer = Some(layers[i.index(layers.len())].id.clone());
        }
        if program {
            state.ui.canvas_mode = qrec_core::state::CanvasMode::Program;
        }
        let take = TakeId::numbered(99);
        state.runtime.record = match phase {
            0 => RecordPhase::Idle,
            1 => RecordPhase::Countdown { remaining: 2, mode: RecordMode::Append, timer: qrec_core::TimerId(1), start_req: None, wait: false },
            2 => RecordPhase::Recording { take, mode: RecordMode::Append, elapsed_ns: 0, waiters: vec![] },
            _ => RecordPhase::Finalizing { take, mode: RecordMode::Append, cancelled: false, stop_reqs: vec![], waiters: vec![], discard: None },
        };
        let map = keymap::resolve("");
        for b in &map.bindings {
            let action = keymap::action_for(b.id, &state);
            prop_assert!(action.is_some(), "{}", b.id);
            if let Some(keymap::Action::Run(cmd)) = action {
                // Commands are plain data: they round-trip through JSON and
                // respect the recording gate (A§5.3).
                let back: qrec_core::Command = serde_json::from_value(cmd.to_json()).unwrap();
                prop_assert_eq!(&back, &cmd);
                prop_assert!(state.runtime.record.is_idle() || cmd.allowed_while_recording(&state.runtime.record), "{}", b.id);
            }
        }
        prop_assert_eq!(keymap::palette(&map, &state).len(), map.bindings.len());
    }
}

// ------------------------------------------------------------------ config

const COMMENTED: &str = "\
# my Qrec settings
theme = \"swamp-night\"  # dark

[autocut]
# speech is loud
threshold_db = -35.0  # keep in sync with the mic
padding_ms = 120

[ui]
scale = 1.0
";

/// A setting with a valid value and how `get_json` shows it back.
fn arb_setting() -> impl Strategy<Value = (&'static str, String, Value)> {
    prop_oneof![
        select(&["swamp-night", "swamp-day", "dragons-keep", "ogre-mode"][..]).prop_map(|t| (
            "theme",
            t.to_owned(),
            json!(t)
        )),
        (8u32..=4096).prop_map(|n| ("canvas.width", (n * 2).to_string(), json!(n * 2))),
        (8u32..=4096).prop_map(|n| ("canvas.height", (n * 2).to_string(), json!(n * 2))),
        select(
            &[
                ("30", "30"),
                ("29.97", "30000/1001"),
                ("60000/1001", "60000/1001"),
                ("25", "25"),
                ("240", "240")
            ][..]
        )
        .prop_map(|(text, shown)| ("canvas.fps", text.to_owned(), json!(shown))),
        (0u32..0x0100_0000).prop_map(|c| (
            "canvas.background",
            format!("#{c:06x}"),
            json!(format!("#{c:06X}"))
        )),
        (0u32..=10).prop_map(|n| ("recording.countdown_secs", n.to_string(), json!(n))),
        any::<bool>().prop_map(|b| ("autocut.auto_run_after_take", b.to_string(), json!(b))),
        (-90i32..=0).prop_map(|n| ("autocut.threshold_db", n.to_string(), json!(f64::from(n)))),
        (0u32..=60_000).prop_map(|n| ("autocut.min_silence_ms", n.to_string(), json!(n))),
        (0u32..=60_000).prop_map(|n| ("autocut.padding_ms", n.to_string(), json!(n))),
        (0u32..=60_000).prop_map(|n| ("autocut.min_keep_ms", n.to_string(), json!(n))),
        select(&["auto", "vaapi", "x264"][..]).prop_map(|e| (
            "encoder.preference",
            e.to_owned(),
            json!(e)
        )),
        (128u32..140).prop_map(|n| (
            "encoder.vaapi_device",
            format!("/dev/dri/renderD{n}"),
            json!(format!("/dev/dri/renderD{n}"))
        )),
        Just(("encoder.vaapi_device", "auto".to_owned(), Value::Null)),
        (2u32..=12).prop_map(|q| (
            "ui.scale",
            (f64::from(q) / 4.0).to_string(),
            json!(f64::from(q) / 4.0)
        )),
        any::<bool>().prop_map(|b| ("ui.reduced_motion", b.to_string(), json!(b))),
        any::<bool>().prop_map(|b| ("ui.sounds", b.to_string(), json!(b))),
    ]
}

proptest! {
    #[test]
    fn config_edits_keep_comments_and_read_back(edits in prop::collection::vec(arb_setting(), 1..8)) {
        let mut doc: toml_edit::DocumentMut = COMMENTED.parse().unwrap();
        let mut last: BTreeMap<&str, Value> = BTreeMap::new();
        for (key, text, shown) in &edits {
            config::set(&mut doc, key, text).unwrap();
            last.insert(key, shown.clone());
        }
        let text = doc.to_string();
        let cfg = config::parse(&text).unwrap();
        for (key, shown) in &last {
            prop_assert_eq!(&config::get_json(&cfg, key).unwrap(), shown, "{}", key);
        }
        for line in ["# my Qrec settings", "# speech is loud", "# keep in sync with the mic", "# dark"] {
            prop_assert!(text.contains(line), "lost {:?}:\n{}", line, text);
        }
        // Setting the same values again changes nothing.
        let mut again = doc.clone();
        for (key, text, _) in &edits {
            config::set(&mut again, key, text).unwrap();
        }
        let cfg_again = config::parse(&again.to_string()).unwrap();
        prop_assert_eq!(cfg_again, cfg);
    }
}

#[test]
fn spec_example_is_valid_everywhere() {
    let text = include_str!("fixtures/spec_example.json");
    let migrated = migrate::parse_project(text).unwrap();
    assert!(validate(&migrated.project).is_empty());
    let value: Value = serde_json::from_str(text).unwrap();
    assert!(check(&schema::project_schema(), &value).is_empty());
    let project: Project = serde_json::from_value(value).unwrap();
    assert_eq!(project, migrated.project);
}
