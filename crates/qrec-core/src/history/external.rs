//! Merging an external edit of `qrec.json` (A§7.4). Owner: history.
//!
//! A hand edit is merged, not adopted: from the file only the undoable
//! fields count (`name, canvas, scene, recording, autocut, timeline,
//! takes[*].scene`). Takes are facts: takes missing from the file are kept,
//! takes only in the file are added, and for takes in both the in-memory
//! `status, streams, durationFrames, recordedAt, error, placed, replaced`
//! win. `nextTakeNumber` never goes down. The take being recorded is kept
//! exactly as in memory.
//!
//! The `missing` flags of layers and tracks are facts too (A§5.1: a patch
//! never contains facts): a take scene keeps the in-memory flag of every
//! layer/track it already had, and the project scene never has any.

use std::collections::BTreeMap;

use crate::error::CoreError;
use crate::ids::{Frame, TakeId};
use crate::model::{Project, Scene, Take, TakeStatus};
use crate::requantize::{derive_duration_frames, rederive_durations};
use crate::state::State;

/// The merged project and the warnings to show.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalMerge {
    pub project: Project,
    /// "takes can only be removed with `qrec take purge`", ignored pointers, …
    pub warnings: Vec<String>,
}

/// Merges the undoable fields of `file` into the current project; facts
/// (take status, streams, durations, …) stay as in memory (A§7.4). Take
/// durations are re-derived when the canvas fps changed. The caller
/// validates the result and commits it as an "External edit" entry.
pub fn merge_external(state: &State, file: Project) -> Result<ExternalMerge, CoreError> {
    let current = &state.project;
    let in_flight = state.runtime.record.take();
    let mut ignored: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let Project {
        version,
        name,
        created_at,
        canvas,
        mut scene,
        recording,
        autocut,
        takes: mut file_takes,
        next_take_number,
        timeline,
    } = file;
    if version != current.version {
        ignored.push("/version".to_owned());
    }
    if created_at != current.created_at {
        ignored.push("/createdAt".to_owned());
    }
    keep_facts(&mut scene, None, "/scene", &mut ignored);
    // After an fps change the durations are re-derived; a file that already
    // has the re-derived values didn't edit a fact.
    let fps_changed = canvas.fps != current.canvas.fps;
    let new_fps = canvas.fps;
    let expected_duration = |take: &Take| {
        if fps_changed && take.status != TakeStatus::Recording {
            derive_duration_frames(take, new_fps)
        } else {
            take.duration_frames
        }
    };

    let mut merged = current.clone();
    merged.name = name;
    merged.canvas = canvas;
    merged.scene = scene;
    merged.recording = recording;
    merged.autocut = autocut;
    merged.timeline = timeline;
    if next_take_number < current.next_take_number {
        warnings.push(format!(
            "nextTakeNumber can't go down: kept {}",
            current.next_take_number
        ));
    }
    merged.next_take_number = next_take_number.max(current.next_take_number);

    let mut kept: Vec<&TakeId> = Vec::new();
    for (id, take) in &current.takes {
        let Some(edited) = file_takes.remove(id) else {
            kept.push(id);
            continue;
        };
        if Some(id) == in_flight {
            if edited != *take {
                ignored.push(format!("/takes/{id}"));
            }
            continue;
        }
        let merged_take = merged.takes.get_mut(id).expect("merged starts as a copy of current");
        merged_take.scene = merge_take(id, take, expected_duration(take), edited, &mut ignored);
    }
    merged.takes.extend(file_takes);

    if !kept.is_empty() {
        let ids: Vec<&str> = kept.iter().map(|id| id.as_str()).collect();
        warnings.push(format!(
            "takes can only be removed with `qrec take purge`: kept {}",
            ids.join(", ")
        ));
    }
    if !ignored.is_empty() {
        warnings.push(format!(
            "facts can't be edited by hand: ignored {}",
            ignored.join(", ")
        ));
    }
    if fps_changed {
        rederive_durations(&mut merged);
    }
    Ok(ExternalMerge {
        project: merged,
        warnings,
    })
}

/// The scene of a take in both projects: the file's, with the in-memory
/// `missing` flags. Records the pointers of every fact the file changed
/// (`duration` is the take's duration at the merged fps).
fn merge_take(
    id: &TakeId,
    current: &Take,
    duration: Frame,
    edited: Take,
    ignored: &mut Vec<String>,
) -> Scene {
    let Take {
        status,
        recorded_at,
        duration_frames,
        mut scene,
        streams,
        error,
        placed,
        replaced,
    } = edited;
    let facts = [
        ("status", status != current.status),
        ("recordedAt", recorded_at != current.recorded_at),
        // Untouched (the old value) or already re-derived: not an edit.
        (
            "durationFrames",
            duration_frames != current.duration_frames && duration_frames != duration,
        ),
        ("streams", streams != current.streams),
        ("error", error != current.error),
        ("placed", placed != current.placed),
        ("replaced", replaced != current.replaced),
    ];
    for (field, changed) in facts {
        if changed {
            ignored.push(format!("/takes/{id}/{field}"));
        }
    }
    keep_facts(
        &mut scene,
        Some(&current.scene),
        &format!("/takes/{id}/scene"),
        ignored,
    );
    scene
}

/// Sets each layer's and track's `missing` to the in-memory value (`false`
/// when `memory` doesn't have it, or for the project scene), recording the
/// pointers the file changed.
fn keep_facts(scene: &mut Scene, memory: Option<&Scene>, at: &str, ignored: &mut Vec<String>) {
    let layers: BTreeMap<&str, bool> = memory
        .map(|m| m.layers.iter().map(|l| (l.id.as_str(), l.missing)).collect())
        .unwrap_or_default();
    let tracks: BTreeMap<&str, bool> = memory
        .map(|m| m.audio.iter().map(|t| (t.id.as_str(), t.missing)).collect())
        .unwrap_or_default();
    for (i, layer) in scene.layers.iter_mut().enumerate() {
        let fact = match memory {
            Some(_) => layers.get(layer.id.as_str()).copied().unwrap_or(layer.missing),
            None => false,
        };
        if layer.missing != fact {
            ignored.push(format!("{at}/layers/{i}/missing"));
            layer.missing = fact;
        }
    }
    for (i, track) in scene.audio.iter_mut().enumerate() {
        let fact = match memory {
            Some(_) => tracks.get(track.id.as_str()).copied().unwrap_or(track.missing),
            None => false,
        };
        if track.missing != fact {
            ignored.push(format!("{at}/audio/{i}/missing"));
            track.missing = fact;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ClipId;
    use crate::model::{Clip, Fps, TakeStatus};
    use crate::state::RecordMode;
    use crate::testkit::{self, ProjectBuilder};

    fn take(n: u32) -> TakeId {
        TakeId::numbered(n)
    }

    fn state() -> State {
        testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-1")
                .mic("A1", "alsa_input.mic")
                .take("take-0001", 300)
                .take("take-0002", 200)
                .clip("c001", "take-0001", 0, 300)
                .build(),
        )
    }

    #[test]
    fn undoable_fields_come_from_the_file() {
        let state = state();
        let mut file = state.project.clone();
        file.name = "renamed".into();
        file.timeline.clips[0].out = 200;
        file.scene.layers[0].opacity = 0.5;
        file.autocut.threshold_db = -40.0;
        file.recording.countdown_secs = 3;
        file.canvas.background = "#112233".parse().unwrap();
        file.takes.get_mut(&take(2)).unwrap().scene.audio[0].gain_db = -6.0;
        let merge = merge_external(&state, file.clone()).unwrap();
        assert_eq!(merge.project, file);
        assert!(merge.warnings.is_empty(), "{:?}", merge.warnings);
    }

    #[test]
    fn facts_stay_as_in_memory() {
        let state = state();
        let mut file = state.project.clone();
        file.created_at = "2020-01-01T00:00:00Z".parse().unwrap();
        let t1 = file.takes.get_mut(&take(1)).unwrap();
        t1.duration_frames = 9999;
        t1.status = TakeStatus::Broken;
        t1.streams.pop();
        t1.placed = true;
        t1.scene.layers[0].missing = true;
        t1.scene.layers[0].name = "Main".into();
        file.scene.audio[0].missing = true;
        file.takes.remove(&take(2));
        file.next_take_number = 1;
        let merge = merge_external(&state, file).unwrap();

        let mut expected = state.project.clone();
        expected.takes.get_mut(&take(1)).unwrap().scene.layers[0].name = "Main".into();
        assert_eq!(merge.project, expected);
        assert_eq!(
            merge.warnings,
            [
                "nextTakeNumber can't go down: kept 3",
                "takes can only be removed with `qrec take purge`: kept take-0002",
                "facts can't be edited by hand: ignored /createdAt, /scene/audio/0/missing, \
                 /takes/take-0001/status, /takes/take-0001/durationFrames, \
                 /takes/take-0001/streams, /takes/take-0001/placed, \
                 /takes/take-0001/scene/layers/0/missing"
            ]
        );
    }

    #[test]
    fn new_takes_are_added_and_the_take_number_only_grows() {
        let state = state();
        let mut file = state.project.clone();
        let mut new = file.takes[&take(2)].clone();
        new.scene.layers[0].missing = true;
        file.takes.insert(take(7), new.clone());
        file.next_take_number = 8;
        file.timeline.clips.push(Clip {
            id: ClipId::from_raw("c0aa"),
            take: take(7),
            in_: 0,
            out: 10,
        });
        let merge = merge_external(&state, file).unwrap();
        assert_eq!(merge.project.takes[&take(7)], new, "a new take keeps the file's facts");
        assert_eq!(merge.project.next_take_number, 8);
        assert_eq!(merge.project.timeline.clips.len(), 2);
        assert!(merge.warnings.is_empty());
    }

    #[test]
    fn the_take_being_recorded_is_kept() {
        let mut state = state();
        state.runtime.record = crate::state::RecordPhase::Recording {
            take: take(2),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        let mut file = state.project.clone();
        file.takes.get_mut(&take(2)).unwrap().scene.layers[0].opacity = 0.1;
        file.name = "new".into();
        let merge = merge_external(&state, file).unwrap();
        assert_eq!(merge.project.takes[&take(2)], state.project.takes[&take(2)]);
        assert_eq!(merge.project.name, "new");
        assert_eq!(
            merge.warnings,
            ["facts can't be edited by hand: ignored /takes/take-0002"]
        );
    }

    #[test]
    fn an_fps_change_rederives_durations() {
        let state = state();
        let mut file = state.project.clone();
        file.canvas.fps = Fps::new(60, 1);
        let merge = merge_external(&state, file.clone()).unwrap();
        assert_eq!(merge.project.takes[&take(1)].duration_frames, 600);
        assert_eq!(merge.project.takes[&take(2)].duration_frames, 400);
        assert!(merge.warnings.is_empty());
        // A file that already has the new durations edited no fact…
        file.takes.get_mut(&take(1)).unwrap().duration_frames = 600;
        file.takes.get_mut(&take(2)).unwrap().duration_frames = 400;
        let merge = merge_external(&state, file.clone()).unwrap();
        assert_eq!(merge.project, file);
        assert!(merge.warnings.is_empty(), "{:?}", merge.warnings);
        // …one with other durations did.
        file.takes.get_mut(&take(2)).unwrap().duration_frames = 401;
        let merge = merge_external(&state, file).unwrap();
        assert_eq!(merge.project.takes[&take(2)].duration_frames, 400);
        assert_eq!(
            merge.warnings,
            ["facts can't be edited by hand: ignored /takes/take-0002/durationFrames"]
        );
    }
}
