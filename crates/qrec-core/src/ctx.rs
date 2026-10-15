//! Everything a dispatch needs from outside the core (A§4.1–A§4.3): the
//! time, entropy, confirmation and the I/O-derived `Facts`, plus the id
//! allocators that turn them into stable ids.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::UserConfig;
use crate::hash::sha256_hex;
use crate::ids::{ClipId, LayerId, ReqId, TakeId, TrackId};
use crate::model::{Fps, Project};
use crate::state::State;

/// The inputs of one `apply`/`on_event` call besides the state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ctx {
    /// Truncated to whole seconds by the shell.
    pub now: DateTime<Utc>,
    /// Local offset, only for human file names.
    pub utc_offset_secs: i32,
    /// Fresh per dispatch; seeds `IdGen`.
    pub entropy: u64,
    /// `--yes` or a confirmed modal.
    pub confirmed: bool,
    /// The request being served.
    pub req: Option<ReqId>,
    /// I/O-derived inputs gathered before `apply` (see `facts_needed`).
    pub facts: Facts,
}

// ---------------------------------------------------------------- facts

/// I/O-derived inputs. The shell gathers only what `facts_needed` asks for;
/// everything else stays at its default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Facts {
    /// Always filled (defaults if there is no config file).
    pub config: UserConfig,
    /// The text of `~/.config/qrec/keys.toml` (empty when absent). Always
    /// filled, like `config`; `Query::Keys` resolves it.
    #[serde(default)]
    pub keys_toml: String,
    /// `Init`: the project folder's name.
    pub folder_name: Option<String>,
    /// `AudioAdd` mic without a source.
    pub default_mic: Option<AudioDeviceInfo>,
    /// `AudioAdd` system without a source.
    pub default_monitor: Option<AudioDeviceInfo>,
    /// Labels for an explicit source.
    pub audio_devices: Vec<AudioDeviceInfo>,
    /// `LayerAdd` screen (label, size; unknown output ⇒ `NotFound`).
    pub screens: Vec<ScreenInfo>,
    /// `LayerAdd` camera, camera modes for the capture plan.
    pub cameras: Vec<CameraInfo>,
    /// `Export`/`FrameSave` with `--out`.
    pub out: Option<OutFacts>,
    /// Purge confirmation: the files of each take with their sizes.
    pub take_files: BTreeMap<TakeId, Vec<FileFact>>,
    /// 1 + the highest `takes/take-N` folder on disk.
    pub min_take_number: u32,
    /// Size and mtime of project files, keyed by project-relative path (silence keys).
    pub files: BTreeMap<String, FileFact>,
}

/// A PulseAudio/PipeWire source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioDeviceInfo {
    /// Source name (`alsa_input.…` or `….monitor`).
    pub name: String,
    pub description: String,
    pub is_monitor: bool,
}

/// A monitor (Wayland `wl_output`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScreenInfo {
    /// Bare output name (`DP-4`).
    pub output: String,
    pub description: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

/// A V4L2 camera and the modes it offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraInfo {
    pub device: String,
    pub label: String,
    pub modes: Vec<CameraMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraMode {
    pub width: u32,
    pub height: u32,
    pub fps: Fps,
    /// Pixel format or `image/jpeg` (MJPEG).
    pub format: String,
}

/// What the shell found at a hand-picked output path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutFacts {
    /// The absolute path.
    pub abs: String,
    pub exists: bool,
    /// The size of what is at the path (`lstat`: a symlink there is what an
    /// overwrite replaces), when something is. The overwrite confirmation
    /// lists it (SPEC §11).
    #[serde(default)]
    pub bytes: Option<u64>,
    /// Why the path can't be used: inside `takes/` or `.qrec/` (also through
    /// a symlinked folder), `qrec.json` itself, a directory, or a name over
    /// 255 bytes.
    pub forbidden: Option<String>,
}

/// Size and modification time of a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileFact {
    /// Project-relative, `/`-separated.
    pub path: String,
    pub bytes: u64,
    pub mtime_ns: i64,
}

/// Which facts a command needs (computed by `facts_needed` before `apply`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct FactsNeeded {
    pub folder_name: bool,
    /// Default mic/monitor and the audio device list.
    pub audio_devices: bool,
    /// Screens and cameras.
    pub video_sources: bool,
    /// Resolve and check this output path.
    pub out: Option<String>,
    pub take_files: Vec<TakeId>,
    /// Scan `takes/` for `min_take_number`.
    pub take_dirs: bool,
    /// Project-relative paths to stat into `Facts::files`.
    pub files: Vec<String>,
}

impl FactsNeeded {
    /// True when nothing needs gathering.
    pub fn is_empty(&self) -> bool {
        *self == FactsNeeded::default()
    }
}

// ------------------------------------------------------------------ ids

/// Generates clip ids `c` + hex from `sha256(entropy ‖ counter)` (A§4.3).
///
/// Ids are 3 hex digits; after every 64 collisions with ids in use the
/// generator widens by one digit (4, then 5, …). The same entropy and the
/// same ids in use always give the same sequence.
#[derive(Debug, Clone)]
pub struct IdGen {
    entropy: u64,
    counter: u64,
    width: usize,
    collisions: u32,
    used: BTreeSet<ClipId>,
}

impl IdGen {
    /// Hex digits of the first ids.
    pub const START_WIDTH: usize = 3;
    /// Collisions before the ids get one digit longer.
    pub const WIDEN_AFTER: u32 = 64;

    /// A generator that avoids nothing yet; see [`IdGen::for_state`].
    pub fn new(entropy: u64) -> Self {
        Self {
            entropy,
            counter: 0,
            width: Self::START_WIDTH,
            collisions: 0,
            used: BTreeSet::new(),
        }
    }

    /// A generator that skips every id on the timeline, in `ui.selection`
    /// and in `history.seen_clip_ids`.
    pub fn for_state(state: &State, entropy: u64) -> Self {
        let mut ids = Self::new(entropy);
        ids.avoid(state.project.timeline.clips.iter().map(|c| &c.id));
        ids.avoid(state.ui.selection.iter());
        ids.avoid(state.history.seen_clip_ids.iter());
        ids
    }

    /// Never yield these ids.
    pub fn avoid<'a>(&mut self, ids: impl IntoIterator<Item = &'a ClipId>) {
        self.used.extend(ids.into_iter().cloned());
    }

    /// The next unused clip id. It counts as used afterwards.
    pub fn next_clip_id(&mut self) -> ClipId {
        loop {
            let mut input = [0u8; 16];
            input[..8].copy_from_slice(&self.entropy.to_be_bytes());
            input[8..].copy_from_slice(&self.counter.to_be_bytes());
            self.counter += 1;
            let hex = sha256_hex(&input);
            let id = ClipId::from_raw(format!("c{}", &hex[..self.width.min(hex.len())]));
            if self.used.insert(id.clone()) {
                return id;
            }
            self.collisions += 1;
            if self.collisions % Self::WIDEN_AFTER == 0 {
                self.width += 1;
            }
        }
    }

    /// How many candidates collided so far.
    pub fn collisions(&self) -> u32 {
        self.collisions
    }
}

/// The next layer id `L{n}`: `n = 1 + max` over the ids of the project
/// scene, every take scene (and its streams) and every id seen in history.
/// Layers and tracks share a scene's ids, so a track named `L{n}` counts
/// too. Nothing is above a hand-edited `L4294967295`: then `n` is the
/// lowest number none of them uses, so an id is never reused.
pub fn next_layer_id(state: &State) -> LayerId {
    let parse = |s: &str| s.parse::<LayerId>().ok().and_then(|id| id.number());
    LayerId::numbered(next_number(&scene_numbers(state, parse)))
}

/// The next audio track id `A{n}`: like [`next_layer_id`] for tracks.
pub fn next_track_id(state: &State) -> TrackId {
    let parse = |s: &str| s.parse::<TrackId>().ok().and_then(|id| id.number());
    TrackId::numbered(next_number(&scene_numbers(state, parse)))
}

/// One above every number in `used` or, when that would pass `u32::MAX`,
/// the lowest number `used` lacks. Numbers start at 1.
fn next_number(used: &BTreeSet<u32>) -> u32 {
    used.last()
        .map_or(Some(1), |max| max.checked_add(1))
        .or_else(|| (1..=u32::MAX).find(|n| !used.contains(n)))
        .unwrap_or(1)
}

/// Every number `parse` finds among the layer and track ids of the project
/// scene and every take scene, the take streams and the `"id"`s in history.
fn scene_numbers(state: &State, parse: impl Fn(&str) -> Option<u32>) -> BTreeSet<u32> {
    let mut used = BTreeSet::new();
    let in_scene = |scene: &crate::model::Scene, used: &mut BTreeSet<u32>| {
        let layers = scene.layers.iter().map(|l| l.id.as_str());
        let tracks = scene.audio.iter().map(|t| t.id.as_str());
        used.extend(layers.chain(tracks).filter_map(&parse));
    };
    let project = &state.project;
    in_scene(&project.scene, &mut used);
    for take in project.takes.values() {
        in_scene(&take.scene, &mut used);
        used.extend(take.streams.iter().filter_map(|s| parse(&s.id)));
    }
    for entry in state.history.entries.iter() {
        for op in entry.forward.0.iter().chain(entry.inverse.0.iter()) {
            let value = match op {
                json_patch::PatchOperation::Add(o) => &o.value,
                json_patch::PatchOperation::Replace(o) => &o.value,
                json_patch::PatchOperation::Test(o) => &o.value,
                _ => continue,
            };
            if op.path().back().is_some_and(|t| t.decoded() == "id") {
                used.extend(value.as_str().and_then(&parse));
            }
            object_ids(value, &parse, &mut used);
        }
    }
    used
}

/// Adds the number of every `"id"` member of the objects inside `value`.
fn object_ids(value: &Value, parse: &impl Fn(&str) -> Option<u32>, used: &mut BTreeSet<u32>) {
    match value {
        Value::Object(map) => {
            used.extend(map.get("id").and_then(Value::as_str).and_then(parse));
            for v in map.values() {
                object_ids(v, parse, used);
            }
        }
        Value::Array(items) => {
            for v in items {
                object_ids(v, parse, used);
            }
        }
        _ => {}
    }
}

/// The number for a new take: `max(nextTakeNumber, facts.min_take_number,
/// 1 + the highest take number in the pool)` (A§4.3).
pub fn next_take_number(project: &Project, facts: &Facts) -> u32 {
    let in_pool = project
        .takes
        .keys()
        .map(|t| t.number().saturating_add(1))
        .max()
        .unwrap_or(1);
    project
        .next_take_number
        .max(facts.min_take_number)
        .max(in_pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    #[test]
    fn ids_are_deterministic() {
        let mut a = IdGen::new(42);
        let mut b = IdGen::new(42);
        let xs: Vec<ClipId> = (0..20).map(|_| a.next_clip_id()).collect();
        let ys: Vec<ClipId> = (0..20).map(|_| b.next_clip_id()).collect();
        assert_eq!(xs, ys);
        assert!(
            xs.iter()
                .all(|id| id.as_str().len() == 4 && id.as_str().starts_with('c'))
        );
        assert!(xs.iter().all(|id| id.as_str().parse::<ClipId>().is_ok()));
        let unique: BTreeSet<&ClipId> = xs.iter().collect();
        assert_eq!(unique.len(), xs.len(), "never the same id twice");
        assert_ne!(IdGen::new(43).next_clip_id(), xs[0]);
    }

    #[test]
    fn ids_skip_used_ones() {
        let first = IdGen::new(7).next_clip_id();
        let mut ids = IdGen::new(7);
        ids.avoid([&first]);
        let next = ids.next_clip_id();
        assert_ne!(next, first);
        assert_eq!(ids.collisions(), 1);

        let project = testkit::ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 100)
            .clip(first.as_str(), "take-0001", 0, 50)
            .build();
        let mut state = testkit::state(project);
        let selected = IdGen::new(7).nth_after(1);
        state.ui.selection.push(selected.clone());
        let seen = IdGen::new(7).nth_after(2);
        state.history.remember_clip_ids([&seen]);
        let mut ids = IdGen::for_state(&state, 7);
        let id = ids.next_clip_id();
        assert!(![&first, &selected, &seen].contains(&&id));
        assert_eq!(ids.collisions(), 3);
    }

    #[test]
    fn ids_widen_after_64_collisions() {
        let mut ids = IdGen::new(1);
        let all_three_digit: Vec<ClipId> = (0..4096)
            .map(|n| ClipId::from_raw(format!("c{n:03x}")))
            .collect();
        ids.avoid(all_three_digit.iter());
        let id = ids.next_clip_id();
        assert_eq!(id.as_str().len(), 5, "c + 4 hex digits: {id}");
        assert_eq!(ids.collisions(), 64);

        let all_four_digit: Vec<ClipId> = (0..65536)
            .map(|n| ClipId::from_raw(format!("c{n:04x}")))
            .collect();
        let mut ids = IdGen::new(1);
        ids.avoid(all_three_digit.iter().chain(all_four_digit.iter()));
        assert_eq!(ids.next_clip_id().as_str().len(), 6, "then 5 hex digits");
        assert_eq!(ids.collisions(), 128);
    }

    impl IdGen {
        /// The id a fresh generator would yield after skipping `n` ids.
        fn nth_after(mut self, n: usize) -> ClipId {
            for _ in 0..n {
                self.next_clip_id();
            }
            self.next_clip_id()
        }
    }

    #[test]
    fn layer_track_and_take_numbers() {
        let project = testkit::ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .camera_layer("L4", "/dev/video0")
            .mic("A2", "alsa_input.mic")
            .take("take-0003", 100)
            .build();
        let mut state = testkit::state(project);
        assert_eq!(next_layer_id(&state), LayerId::numbered(5));
        assert_eq!(next_track_id(&state), TrackId::numbered(3));

        // A layer that only survives in history still counts.
        state.project.scene.layers.clear();
        state.project.takes.clear();
        assert_eq!(next_layer_id(&state), LayerId::numbered(1));
        let entry = crate::history::HistoryEntry {
            seq: 1,
            ts: state.project.created_at,
            label: "Remove layer L9".into(),
            cmd: serde_json::json!({"cmd": "layerRemove"}),
            forward: json_patch::Patch(vec![]),
            inverse: serde_json::from_value(serde_json::json!([
                {"op": "add", "path": "/scene/layers/0", "value": {"id": "L9", "name": "x"}},
                {"op": "replace", "path": "/scene/audio/0/id", "value": "A7"}
            ]))
            .unwrap(),
            after_hash: String::new(),
        };
        state.history.append(entry);
        assert_eq!(next_layer_id(&state), LayerId::numbered(10));
        assert_eq!(next_track_id(&state), TrackId::numbered(8));

        // Tracks share the layers' ids: a track named like a layer counts.
        state.project.scene.audio[0].id = "L12".parse().unwrap();
        assert_eq!(next_layer_id(&state), LayerId::numbered(13));

        let project = testkit::ProjectBuilder::new().take("take-0003", 50).build();
        let mut facts = Facts::default();
        assert_eq!(next_take_number(&project, &facts), 4);
        facts.min_take_number = 9;
        assert_eq!(next_take_number(&project, &facts), 9);
        let empty = testkit::ProjectBuilder::new().build();
        assert_eq!(next_take_number(&empty, &Facts::default()), 1);
    }

    #[test]
    fn hand_edited_maximum_ids_dont_overflow() {
        // Valid hand-edited ids (`1 + max` overflowed u32: a panic that
        // took the host down, or a wrapped `L0`).
        let project = testkit::ProjectBuilder::new()
            .screen_layer("L4294967295", "DP-1")
            .camera_layer("L1", "/dev/video0")
            .mic("A4294967295", "alsa_input.mic")
            .system("A2", "alsa_output.monitor")
            .build();
        assert!(crate::validate::ensure_valid(&project).is_ok());
        let mut state = testkit::state(project);
        assert_eq!(next_layer_id(&state), LayerId::numbered(2));
        assert_eq!(next_track_id(&state), TrackId::numbered(1));
        // The lowest number nothing uses, history included.
        state.project.scene.layers[1].id = "L3".parse().unwrap();
        assert_eq!(next_layer_id(&state), LayerId::numbered(1));
        let entry = crate::history::HistoryEntry {
            seq: 1,
            ts: state.project.created_at,
            label: "Remove layer L1".into(),
            cmd: serde_json::json!({"cmd": "layerRemove"}),
            forward: json_patch::Patch(vec![]),
            inverse: serde_json::from_value(serde_json::json!([
                {"op": "add", "path": "/scene/layers/0", "value": {"id": "L1", "name": "x"}},
                {"op": "add", "path": "/scene/layers/1", "value": {"id": "L2", "name": "y"}}
            ]))
            .unwrap(),
            after_hash: String::new(),
        };
        state.history.append(entry);
        assert_eq!(next_layer_id(&state), LayerId::numbered(4));

        let mut max = testkit::ProjectBuilder::new().take("take-0003", 50).build();
        let take = max.takes.remove(&TakeId::numbered(3)).unwrap();
        max.takes.insert("take-4294967295".parse().unwrap(), take);
        assert_eq!(next_take_number(&max, &Facts::default()), u32::MAX);
    }
}
