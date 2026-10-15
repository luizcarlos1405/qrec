//! The `qrec.json` example from SPEC §6.3 must load and round-trip unchanged.

use qrec_core::{Project, StreamKind, TakeId, TakeStatus};

const SPEC_EXAMPLE: &str = include_str!("fixtures/spec_example.json");

#[test]
fn spec_example_round_trips() {
    let project: Project = serde_json::from_str(SPEC_EXAMPLE).expect("spec example parses");
    assert_eq!(project.version, 1);
    assert_eq!(project.scene.layers.len(), 2);
    assert_eq!(project.timeline.clips.len(), 2);
    let take = project.take(&TakeId::numbered(1)).expect("take-0001");
    assert_eq!(take.status, TakeStatus::Ok);
    assert_eq!(
        take.streams
            .iter()
            .filter(|s| s.kind == StreamKind::Audio)
            .count(),
        2
    );

    let original: serde_json::Value = serde_json::from_str(SPEC_EXAMPLE).unwrap();
    let reserialized = serde_json::to_value(&project).unwrap();
    pretty_assertions::assert_eq!(original, reserialized);
}

#[test]
fn unknown_fields_are_rejected() {
    let mut value: serde_json::Value = serde_json::from_str(SPEC_EXAMPLE).unwrap();
    value["canvas"]["fsp"] = serde_json::json!(30);
    let err = serde_json::from_value::<Project>(value)
        .unwrap_err()
        .to_string();
    assert!(err.contains("fsp"), "{err}");

    let mut value: serde_json::Value = serde_json::from_str(SPEC_EXAMPLE).unwrap();
    value["scene"]["layers"][0]["source"]["outptu"] = serde_json::json!("DP-1");
    assert!(serde_json::from_value::<Project>(value).is_err());
}
