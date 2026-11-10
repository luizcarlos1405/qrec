//! Saved scenes (SPEC §5.6, A§9.11): the library in
//! `$XDG_CONFIG_HOME/qrec/scenes.json` and the pure rules behind the
//! `Scene*` commands: names, "same layout", which scene is in use, turning a
//! saved scene into the project scene, and applying one to a take.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::ids::{LayerId, TrackId};
use crate::model::{AudioTrack, Layer, Scene, VideoSource};

/// `scenes.json`'s format version.
pub const VERSION: u32 = 1;
/// The longest name, in chars.
pub const MAX_NAME: usize = 80;

/// The saved scenes, in the order they were saved (`Alt+1` is the first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedScenes {
    pub version: u32,
    #[serde(default)]
    pub scenes: Vec<SavedScene>,
}

impl Default for SavedScenes {
    fn default() -> Self {
        Self {
            version: VERSION,
            scenes: Vec::new(),
        }
    }
}

/// One saved scene: the layers and tracks with their own ids, and which of
/// its tracks is the autocut track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedScene {
    pub name: String,
    pub saved_at: DateTime<Utc>,
    pub scene: Scene,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autocut_track: Option<TrackId>,
}

/// `scenes.json` as the shell read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "state", content = "value")]
pub enum ScenesFile {
    /// Read (no file is an empty library).
    Read(SavedScenes),
    /// It exists but can't be used: why. Counts as empty; nothing writes it.
    Unreadable(String),
}

impl Default for ScenesFile {
    fn default() -> Self {
        ScenesFile::Read(SavedScenes::default())
    }
}

impl ScenesFile {
    /// The scenes to list and use (none when the file can't be read).
    pub fn scenes(&self) -> &[SavedScene] {
        match self {
            ScenesFile::Read(lib) => &lib.scenes,
            ScenesFile::Unreadable(_) => &[],
        }
    }

    /// The library to change, or why it can't be written.
    pub fn writable(&self) -> Result<&SavedScenes, CoreError> {
        match self {
            ScenesFile::Read(lib) => Ok(lib),
            ScenesFile::Unreadable(why) => Err(CoreError::invalid(format!(
                "scenes.json can't be read: {why}"
            ))
            .with_hint("fix or move ~/.config/qrec/scenes.json; until then no scene is saved")),
        }
    }

    /// Parses `scenes.json`'s text: a newer version or bad JSON is
    /// `Unreadable`.
    pub fn parse(text: &str) -> ScenesFile {
        match serde_json::from_str::<SavedScenes>(text) {
            Ok(lib) if lib.version > VERSION => ScenesFile::Unreadable(format!(
                "it is version {}, this Qrec reads version {VERSION}",
                lib.version
            )),
            Ok(lib) => ScenesFile::Read(lib),
            Err(e) => ScenesFile::Unreadable(e.to_string()),
        }
    }
}

impl SavedScenes {
    /// The scene named `name` (without case).
    pub fn find(&self, name: &str) -> Option<usize> {
        find(&self.scenes, name)
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("scenes always serialize");
        text.push('\n');
        text
    }
}

/// The index of the scene named `name` (without case).
pub fn find(scenes: &[SavedScene], name: &str) -> Option<usize> {
    let key = name.trim().to_lowercase();
    scenes.iter().position(|s| s.name.to_lowercase() == key)
}

/// A name as stored: trimmed, not empty, at most [`MAX_NAME`] chars, no
/// control characters.
pub fn clean_name(name: &str) -> Result<String, CoreError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CoreError::usage("a scene needs a name"));
    }
    if name.chars().any(char::is_control) {
        return Err(CoreError::usage(
            "a scene name can't hold control characters",
        ));
    }
    if name.chars().count() > MAX_NAME {
        return Err(CoreError::usage(format!(
            "a scene name can be at most {MAX_NAME} characters"
        )));
    }
    Ok(name.to_owned())
}

/// "<name> copy", then "<name> copy 2", … : the first one not taken.
pub fn copy_name(scenes: &[SavedScene], name: &str) -> String {
    let base: String = format!("{name} copy").chars().take(MAX_NAME).collect();
    if find(scenes, &base).is_none() {
        return base;
    }
    (2u32..)
        .map(|n| {
            let suffix = format!(" {n}");
            let room = MAX_NAME - suffix.chars().count();
            format!("{}{suffix}", base.chars().take(room).collect::<String>())
        })
        .find(|candidate| find(scenes, candidate).is_none())
        .expect("some number is free")
}

/// What the project scene becomes saved: `missing` cleared, and the autocut
/// track only when it is a track of the scene.
pub fn saved_from(
    name: String,
    at: DateTime<Utc>,
    scene: &Scene,
    autocut: Option<&TrackId>,
) -> SavedScene {
    let mut scene = scene.clone();
    for layer in &mut scene.layers {
        layer.missing = false;
    }
    for track in &mut scene.audio {
        track.missing = false;
    }
    let autocut_track = autocut.filter(|id| scene.track(id).is_some()).cloned();
    SavedScene {
        name,
        saved_at: at,
        scene,
        autocut_track,
    }
}

/// Whether two sources are the same device (kind and output or device; the
/// label is the machine's).
pub fn same_source(a: &VideoSource, b: &VideoSource) -> bool {
    match (a, b) {
        (VideoSource::Screen { output: x, .. }, VideoSource::Screen { output: y, .. }) => x == y,
        (VideoSource::Camera { device: x, .. }, VideoSource::Camera { device: y, .. }) => x == y,
        _ => false,
    }
}

fn same_layer(a: &Layer, b: &Layer) -> bool {
    same_source(&a.source, &b.source)
        && a.name == b.name
        && a.visible == b.visible
        && a.opacity == b.opacity
        && a.rect == b.rect
        && a.crop == b.crop
        && a.fit == b.fit
        && a.locked == b.locked
}

fn same_track(a: &AudioTrack, b: &AudioTrack) -> bool {
    a.role == b.role
        && a.source == b.source
        && a.label == b.label
        && a.gain_db == b.gain_db
        && a.muted == b.muted
        && a.sync_offset_ms == b.sync_offset_ms
}

/// Whether the project scene (with its autocut track) has the saved
/// scene's layout: the same layers and tracks in the same order, equal but
/// for ids and source labels, and the autocut track at the same position.
pub fn same_layout(scene: &Scene, autocut: Option<&TrackId>, saved: &SavedScene) -> bool {
    let other = &saved.scene;
    let position = |scene: &Scene, id: Option<&TrackId>| {
        id.and_then(|id| scene.audio.iter().position(|t| &t.id == id))
    };
    scene.layers.len() == other.layers.len()
        && scene.audio.len() == other.audio.len()
        && scene
            .layers
            .iter()
            .zip(&other.layers)
            .all(|(a, b)| same_layer(a, b))
        && scene
            .audio
            .iter()
            .zip(&other.audio)
            .all(|(a, b)| same_track(a, b))
        && position(scene, autocut) == position(other, saved.autocut_track.as_ref())
}

/// The saved scene the project uses (SPEC §5.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneStatus {
    /// None: the project scene is no saved scene.
    pub name: Option<String>,
    /// Changed since it was used or saved.
    pub edited: bool,
}

/// The first saved scene with the same layout (`last` first, when it
/// matches), not edited; else `last` when the library still has it,
/// edited; else none.
pub fn status(
    scenes: &[SavedScene],
    scene: &Scene,
    autocut: Option<&TrackId>,
    last: Option<&str>,
) -> SceneStatus {
    let last = last.and_then(|name| find(scenes, name)).map(|i| &scenes[i]);
    if let Some(saved) = last
        && same_layout(scene, autocut, saved)
    {
        return SceneStatus {
            name: Some(saved.name.clone()),
            edited: false,
        };
    }
    if let Some(saved) = scenes.iter().find(|s| same_layout(scene, autocut, s)) {
        return SceneStatus {
            name: Some(saved.name.clone()),
            edited: false,
        };
    }
    match last {
        Some(saved) => SceneStatus {
            name: Some(saved.name.clone()),
            edited: true,
        },
        None => SceneStatus {
            name: None,
            edited: false,
        },
    }
}

/// The project scene a saved scene becomes: a layer or track whose source
/// the current scene has keeps that id; the others get new ids from
/// `next_layer` / `next_track` (the first free numbers, counted up). Also
/// the autocut track's id in the new scene.
pub fn into_project(
    saved: &SavedScene,
    current: &Scene,
    next_layer: LayerId,
    next_track: TrackId,
) -> (Scene, Option<TrackId>) {
    let mut layer_n = next_layer.number().unwrap_or(1);
    let mut track_n = next_track.number().unwrap_or(1);
    let mut used_layers: BTreeSet<LayerId> = BTreeSet::new();
    let mut used_tracks: BTreeSet<TrackId> = BTreeSet::new();
    let layers = saved
        .scene
        .layers
        .iter()
        .map(|layer| {
            let kept = current
                .layers
                .iter()
                .find(|l| same_source(&l.source, &layer.source) && !used_layers.contains(&l.id))
                .map(|l| l.id.clone());
            let id = kept.unwrap_or_else(|| {
                let id = LayerId::numbered(layer_n);
                layer_n += 1;
                id
            });
            used_layers.insert(id.clone());
            Layer {
                id,
                missing: false,
                ..layer.clone()
            }
        })
        .collect();
    let mut autocut = None;
    let audio = saved
        .scene
        .audio
        .iter()
        .map(|track| {
            let kept = current
                .audio
                .iter()
                .find(|t| t.source == track.source && !used_tracks.contains(&t.id))
                .map(|t| t.id.clone());
            let id = kept.unwrap_or_else(|| {
                let id = TrackId::numbered(track_n);
                track_n += 1;
                id
            });
            used_tracks.insert(id.clone());
            if saved.autocut_track.as_ref() == Some(&track.id) {
                autocut = Some(id.clone());
            }
            AudioTrack {
                id,
                missing: false,
                ..track.clone()
            }
        })
        .collect();
    (Scene { layers, audio }, autocut)
}

/// A take's scene with a saved scene's layout (A§9.11): the take's layers
/// matched by source get the layout fields and the saved z-order among
/// themselves; the unmatched are hidden; tracks matched by source get gain,
/// mute and sync. Also how many layers matched (0: nothing to apply).
pub fn onto_take(saved: &SavedScene, take: &Scene) -> (Scene, usize) {
    let mut out = take.clone();
    let rank = |layer: &Layer| {
        saved
            .scene
            .layers
            .iter()
            .position(|s| same_source(&s.source, &layer.source))
    };
    let mut matched = 0;
    for layer in &mut out.layers {
        match saved
            .scene
            .layers
            .iter()
            .find(|s| same_source(&s.source, &layer.source))
        {
            Some(from) => {
                matched += 1;
                layer.rect = from.rect;
                layer.crop = from.crop;
                layer.opacity = from.opacity;
                layer.visible = from.visible;
                layer.fit = from.fit;
            }
            None => layer.visible = false,
        }
    }
    // The matched layers take the saved order, in the slots they hold now.
    let slots: Vec<usize> = (0..out.layers.len())
        .filter(|&i| rank(&out.layers[i]).is_some())
        .collect();
    let mut ordered: Vec<Layer> = slots.iter().map(|&i| out.layers[i].clone()).collect();
    ordered.sort_by_key(|l| rank(l));
    for (slot, layer) in slots.into_iter().zip(ordered) {
        out.layers[slot] = layer;
    }
    for track in &mut out.audio {
        if let Some(from) = saved.scene.audio.iter().find(|s| s.source == track.source) {
            track.gain_db = from.gain_db;
            track.muted = from.muted;
            track.sync_offset_ms = from.sync_offset_ms;
        }
    }
    (out, matched)
}

/// A one-line summary of a saved scene, as the menu's subtitle says it:
/// "DP-4 · Logitech C920 · 2 audio".
pub fn summary(saved: &SavedScene) -> String {
    let mut parts: Vec<String> = saved
        .scene
        .layers
        .iter()
        .map(|l| match &l.source {
            VideoSource::Screen { output, .. } => output.clone(),
            VideoSource::Camera { label, device } => {
                if label.is_empty() {
                    device.clone()
                } else {
                    label.clone()
                }
            }
        })
        .collect();
    let tracks = saved.scene.audio.len();
    if tracks > 0 {
        parts.push(format!("{tracks} audio"));
    }
    if parts.is_empty() {
        "Empty".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// One saved scene as the panel's menu, the palette and `qrec scene list`
/// show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SceneRow {
    pub name: String,
    /// "DP-4 · Logitech C920 · 2 audio".
    pub summary: String,
    pub layers: usize,
    pub audio: usize,
    pub saved_at: DateTime<Utc>,
    /// The scene the project uses (edited or not).
    pub in_use: bool,
    /// Its `Alt` digit: the first nine have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<u8>,
    /// Its layers and tracks, for the thumbnail (canvas px).
    pub scene: Scene,
}

/// `Query::Scenes`: the saved scenes and which one the project uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScenesView {
    pub scenes: Vec<SceneRow>,
    pub current: SceneStatus,
    /// Why `scenes.json` can't be read (nothing lists or saves until then).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<String>,
}

/// The view of the library: `file` when the shell just read it, else the
/// runtime's copy.
pub fn view(state: &crate::state::State, file: Option<&ScenesFile>) -> ScenesView {
    let file = file.unwrap_or(&state.runtime.saved_scenes);
    let scenes = file.scenes();
    let project = &state.project;
    let current = status(
        scenes,
        &project.scene,
        project.autocut.track.as_ref(),
        state.ui.scene.as_deref(),
    );
    let rows = scenes
        .iter()
        .enumerate()
        .map(|(i, saved)| SceneRow {
            name: saved.name.clone(),
            summary: summary(saved),
            layers: saved.scene.layers.len(),
            audio: saved.scene.audio.len(),
            saved_at: saved.saved_at,
            in_use: current.name.as_deref() == Some(saved.name.as_str()),
            key: u8::try_from(i + 1).ok().filter(|k| *k <= 9),
            scene: saved.scene.clone(),
        })
        .collect();
    ScenesView {
        scenes: rows,
        current,
        unreadable: match file {
            ScenesFile::Unreadable(why) => Some(why.clone()),
            ScenesFile::Read(_) => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AudioRole, Crop, Fit, Rect};
    use chrono::TimeZone;

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap()
    }

    fn screen(id: u32, output: &str, rect: Rect) -> Layer {
        Layer {
            id: LayerId::numbered(id),
            name: format!("Screen {output}"),
            visible: true,
            opacity: 1.0,
            source: VideoSource::Screen {
                output: output.into(),
                label: "Monitor".into(),
            },
            rect,
            crop: Crop::default(),
            fit: Fit::Contain,
            locked: false,
            missing: false,
        }
    }

    fn camera(id: u32, rect: Rect) -> Layer {
        Layer {
            id: LayerId::numbered(id),
            name: "Face".into(),
            visible: true,
            opacity: 1.0,
            source: VideoSource::Camera {
                device: "/dev/video0".into(),
                label: "Logitech C920".into(),
            },
            rect,
            crop: Crop::default(),
            fit: Fit::Cover,
            locked: false,
            missing: false,
        }
    }

    fn mic(id: u32) -> AudioTrack {
        AudioTrack {
            id: TrackId::numbered(id),
            role: AudioRole::Mic,
            source: "alsa_input.yeti".into(),
            label: "Yeti Nano".into(),
            gain_db: 0.0,
            muted: false,
            sync_offset_ms: 0,
            missing: false,
        }
    }

    const FULL: Rect = Rect::new(0, 0, 1920, 1080);
    const CORNER: Rect = Rect::new(1500, 760, 384, 288);

    fn screen_camera() -> Scene {
        Scene {
            layers: vec![screen(1, "DP-4", FULL), camera(2, CORNER)],
            audio: vec![mic(1)],
        }
    }

    #[test]
    fn names_are_trimmed_unique_without_case_and_bounded() {
        assert_eq!(clean_name("  Talk  ").unwrap(), "Talk");
        assert!(clean_name("   ").is_err());
        assert!(clean_name("a\nb").is_err());
        assert!(clean_name(&"x".repeat(81)).is_err());
        let lib = [saved_from("Talk".into(), at(), &Scene::default(), None)];
        assert_eq!(find(&lib, "talk"), Some(0));
        assert_eq!(copy_name(&lib, "Talk"), "Talk copy");
        let lib = [
            lib[0].clone(),
            saved_from("Talk copy".into(), at(), &Scene::default(), None),
        ];
        assert_eq!(copy_name(&lib, "Talk"), "Talk copy 2");
    }

    #[test]
    fn the_same_layout_ignores_ids_and_labels() {
        let saved = saved_from(
            "Screen + camera".into(),
            at(),
            &screen_camera(),
            Some(&TrackId::numbered(1)),
        );
        let mut other = screen_camera();
        other.layers[0].id = LayerId::numbered(7);
        other.audio[0].id = TrackId::numbered(4);
        if let VideoSource::Screen { label, .. } = &mut other.layers[0].source {
            *label = "Another name".into();
        }
        assert!(same_layout(&other, Some(&TrackId::numbered(4)), &saved));
        assert!(
            !same_layout(&other, None, &saved),
            "the autocut track counts"
        );
        other.layers[1].rect.x = 1180;
        assert!(!same_layout(&other, Some(&TrackId::numbered(4)), &saved));
    }

    #[test]
    fn status_names_the_scene_in_use_and_whether_it_was_edited() {
        let lib = vec![
            saved_from("Screen + camera".into(), at(), &screen_camera(), None),
            saved_from(
                "Camera only".into(),
                at(),
                &Scene {
                    layers: vec![camera(1, FULL)],
                    audio: vec![],
                },
                None,
            ),
        ];
        let current = screen_camera();
        let clean = status(&lib, &current, None, None);
        assert_eq!(clean.name.as_deref(), Some("Screen + camera"));
        assert!(!clean.edited);
        let mut moved = current.clone();
        moved.layers[1].rect.x = 1180;
        let edited = status(&lib, &moved, None, Some("screen + camera"));
        assert_eq!(edited.name.as_deref(), Some("Screen + camera"));
        assert!(edited.edited);
        assert_eq!(status(&lib, &moved, None, None).name, None);
        assert_eq!(status(&lib, &moved, None, Some("gone")).name, None);
    }

    #[test]
    fn using_a_scene_keeps_the_ids_of_the_sources_already_there() {
        let saved = saved_from(
            "Two".into(),
            at(),
            &Scene {
                layers: vec![
                    screen(1, "DP-4", Rect::new(0, 0, 960, 1080)),
                    screen(2, "DP-2", Rect::new(960, 0, 960, 1080)),
                    camera(3, CORNER),
                ],
                audio: vec![mic(1)],
            },
            Some(&TrackId::numbered(1)),
        );
        let current = Scene {
            layers: vec![camera(5, FULL)],
            audio: vec![mic(3)],
        };
        let (scene, autocut) =
            into_project(&saved, &current, LayerId::numbered(6), TrackId::numbered(4));
        let ids: Vec<&str> = scene.layers.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["L6", "L7", "L5"], "the camera keeps L5");
        assert_eq!(scene.layers[2].rect, CORNER);
        assert_eq!(scene.audio[0].id, TrackId::numbered(3));
        assert_eq!(autocut, Some(TrackId::numbered(3)));
    }

    #[test]
    fn a_take_gets_the_layout_of_the_layers_it_shares() {
        let saved = saved_from(
            "Camera only".into(),
            at(),
            &Scene {
                layers: vec![camera(1, FULL)],
                audio: vec![AudioTrack {
                    gain_db: -6.0,
                    ..mic(1)
                }],
            },
            None,
        );
        let (take, matched) = onto_take(&saved, &screen_camera());
        assert_eq!(matched, 1);
        assert!(!take.layers[0].visible, "the screen isn't in the scene");
        assert_eq!(take.layers[1].rect, FULL);
        assert_eq!(take.audio[0].gain_db, -6.0);
        let none = saved_from("Other".into(), at(), &Scene::default(), None);
        assert_eq!(onto_take(&none, &screen_camera()).1, 0);
    }

    #[test]
    fn the_file_reads_back_and_refuses_what_it_cant_read() {
        let lib = SavedScenes {
            version: VERSION,
            scenes: vec![saved_from("A".into(), at(), &screen_camera(), None)],
        };
        assert_eq!(
            ScenesFile::parse(&lib.to_text()),
            ScenesFile::Read(lib.clone())
        );
        assert!(matches!(ScenesFile::parse("{"), ScenesFile::Unreadable(_)));
        let newer = lib.to_text().replace("\"version\": 1", "\"version\": 9");
        let file = ScenesFile::parse(&newer);
        assert!(matches!(file, ScenesFile::Unreadable(_)));
        assert!(file.scenes().is_empty());
        assert!(file.writable().is_err());
    }

    #[test]
    fn the_summary_names_the_sources() {
        let saved = saved_from("A".into(), at(), &screen_camera(), None);
        assert_eq!(summary(&saved), "DP-4 · Logitech C920 · 1 audio");
    }
}
