//! Project validation (A§2.3). Owner: meta.
//!
//! [`validate`] lists every finding with a JSON Pointer into `qrec.json`,
//! in document order. Errors make a project invalid ([`ensure_valid`]);
//! warnings ("missing media", an autocut track no take has) are shown but
//! the project still loads.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::model::{
    AutocutSettings, CURRENT_VERSION, Canvas, Project, Scene, StreamKind, Take, TakeStatus,
};
use crate::ids::TakeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// Makes the project invalid.
    Error,
    /// Shown, but the project still loads ("missing media").
    Warning,
}

/// One validation finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Issue {
    pub severity: Severity,
    /// JSON Pointer into `qrec.json`.
    pub path: String,
    pub message: String,
}

impl fmt::Display for Issue {
    /// `error at /canvas/width: canvas width 1921 …`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{severity} at {}: {}", self.path, self.message)
    }
}

/// Canvas sizes Qrec accepts (even, so chroma subsampling works).
pub const MIN_CANVAS: u32 = 16;
pub const MAX_CANVAS: u32 = 8192;
/// The highest canvas frame rate.
pub const MAX_FPS: f64 = 240.0;
pub const MAX_COUNTDOWN_SECS: u32 = 10;
pub const MAX_GAIN_DB: f64 = 60.0;
pub const MAX_SYNC_OFFSET_MS: i32 = 5000;
pub const MIN_THRESHOLD_DB: f64 = -90.0;
pub const MAX_AUTOCUT_MS: u32 = 60_000;

/// Escapes one JSON Pointer reference token (RFC 6901).
fn token(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

#[derive(Default)]
struct Issues(Vec<Issue>);

impl Issues {
    fn error(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.push(Severity::Error, path.into(), message.into());
    }

    fn warning(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.push(Severity::Warning, path.into(), message.into());
    }

    fn push(&mut self, severity: Severity, path: String, message: String) {
        self.0.push(Issue {
            severity,
            path,
            message,
        });
    }
}

/// Every error and warning of A§2.3, in document order.
///
/// Errors:
/// - `version` is `CURRENT_VERSION`; `nextTakeNumber` is above every take number;
/// - canvas: even `16 ≤ width, height ≤ 8192`; `fps.num, fps.den > 0`, fps ≤ 240;
/// - scenes (project and takes): layer and track ids unique (they share one
///   namespace, the stream ids); `0 ≤ opacity ≤ 1`; `rect.w, rect.h ≥ 1`;
///   gain within ±60 dB; `|syncOffsetMs| ≤ 5000`;
/// - `countdownSecs ≤ 10`; autocut threshold −90…0 dB, ms values ≤ 60 000;
/// - streams: `file` is `takes/<takeId>/<name>`, `<name>` made of
///   `[A-Za-z0-9._-]` (not `.` or `..`), stream ids unique; finalized takes
///   (`ok`, `tooShort`) have the media fields of their kind (`width`/`height`,
///   `sampleRate`/`channels`) for every stream a non-missing layer or track
///   of the take scene uses — `recording` and `broken` takes and missing
///   sources may lack them;
/// - clips: unique ids; the take exists and is `ok`; `0 ≤ in < out ≤ durationFrames`.
///
/// Warnings: a take-scene layer or track without a stream ("missing
/// media"); `autocut.track` not in the project scene ("takes lacking it are
/// skipped").
pub fn validate(project: &Project) -> Vec<Issue> {
    let mut issues = Issues::default();
    if project.version != CURRENT_VERSION {
        issues.error(
            "/version",
            format!(
                "unsupported version {} (this Qrec reads and writes version {CURRENT_VERSION})",
                project.version
            ),
        );
    }
    canvas(&mut issues, &project.canvas);
    scene(&mut issues, "/scene", &project.scene, None);
    if project.recording.countdown_secs > MAX_COUNTDOWN_SECS {
        issues.error(
            "/recording/countdownSecs",
            format!(
                "countdown of {} s is above the maximum of {MAX_COUNTDOWN_SECS} s",
                project.recording.countdown_secs
            ),
        );
    }
    autocut(&mut issues, &project.autocut, &project.scene);
    for (id, take) in &project.takes {
        take_issues(&mut issues, id, take);
    }
    let highest = project.takes.keys().max_by_key(|id| id.number());
    if let Some(highest) = highest.filter(|h| project.next_take_number <= h.number()) {
        issues.error(
            "/nextTakeNumber",
            format!(
                "nextTakeNumber is {} but {highest} exists (it must be above every take number)",
                project.next_take_number
            ),
        );
    }
    timeline(&mut issues, project);
    issues.0
}

fn canvas(issues: &mut Issues, canvas: &Canvas) {
    for (field, value) in [("width", canvas.width), ("height", canvas.height)] {
        if value % 2 != 0 || !(MIN_CANVAS..=MAX_CANVAS).contains(&value) {
            issues.error(
                format!("/canvas/{field}"),
                format!(
                    "canvas {field} {value} must be an even number from {MIN_CANVAS} to {MAX_CANVAS}"
                ),
            );
        }
    }
    let fps = canvas.fps;
    if fps.num == 0 || fps.den == 0 {
        issues.error(
            "/canvas/fps",
            format!("canvas fps {}/{} needs a positive numerator and denominator", fps.num, fps.den),
        );
    } else if fps.as_f64() > MAX_FPS {
        issues.error("/canvas/fps", format!("canvas fps {fps} is above {MAX_FPS}"));
    }
}

/// Checks one scene. `take` is the take a snapshot belongs to (for the
/// "missing media" warning).
fn unique_id<'a>(issues: &mut Issues, ids: &mut BTreeSet<&'a str>, id: &'a str, path: String) {
    if !ids.insert(id) {
        issues.error(
            path,
            format!("duplicate id {id} in the scene (layer and track ids must be unique)"),
        );
    }
}

fn scene(issues: &mut Issues, path: &str, scene: &Scene, take: Option<(&TakeId, &Take)>) {
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    for (i, layer) in scene.layers.iter().enumerate() {
        let at = format!("{path}/layers/{i}");
        if let Some((take_id, _)) = take.filter(|(_, t)| t.stream(layer.id.as_str()).is_none()) {
            issues.warning(
                at.clone(),
                format!("layer {} of {take_id} has no stream (missing media)", layer.id),
            );
        }
        unique_id(issues, &mut ids, layer.id.as_str(), format!("{at}/id"));
        if !(0.0..=1.0).contains(&layer.opacity) {
            issues.error(
                format!("{at}/opacity"),
                format!("layer {} opacity {} is outside 0 to 1", layer.id, layer.opacity),
            );
        }
        if layer.rect.w < 1 || layer.rect.h < 1 {
            issues.error(
                format!("{at}/rect"),
                format!(
                    "layer {} rect is {}×{}; width and height must be at least 1",
                    layer.id, layer.rect.w, layer.rect.h
                ),
            );
        }
    }
    for (i, track) in scene.audio.iter().enumerate() {
        let at = format!("{path}/audio/{i}");
        if let Some((take_id, _)) = take.filter(|(_, t)| t.stream(track.id.as_str()).is_none()) {
            issues.warning(
                at.clone(),
                format!("track {} of {take_id} has no stream (missing media)", track.id),
            );
        }
        unique_id(issues, &mut ids, track.id.as_str(), format!("{at}/id"));
        if !(-MAX_GAIN_DB..=MAX_GAIN_DB).contains(&track.gain_db) {
            issues.error(
                format!("{at}/gainDb"),
                format!(
                    "track {} gain {} dB is outside -{MAX_GAIN_DB} to {MAX_GAIN_DB} dB",
                    track.id, track.gain_db
                ),
            );
        }
        if track.sync_offset_ms.unsigned_abs() > MAX_SYNC_OFFSET_MS.unsigned_abs() {
            issues.error(
                format!("{at}/syncOffsetMs"),
                format!(
                    "track {} sync offset {} ms is outside -{MAX_SYNC_OFFSET_MS} to {MAX_SYNC_OFFSET_MS} ms",
                    track.id, track.sync_offset_ms
                ),
            );
        }
    }
}

fn autocut(issues: &mut Issues, autocut: &AutocutSettings, project_scene: &Scene) {
    if let Some(track) = autocut.track.as_ref().filter(|t| project_scene.track(t).is_none()) {
        issues.warning(
            "/autocut/track",
            format!("autocut track {track} isn't in the project scene; takes lacking it are skipped"),
        );
    }
    if !(MIN_THRESHOLD_DB..=0.0).contains(&autocut.threshold_db) {
        issues.error(
            "/autocut/thresholdDb",
            format!(
                "autocut threshold {} dB is outside {MIN_THRESHOLD_DB} to 0 dB",
                autocut.threshold_db
            ),
        );
    }
    for (field, value) in [
        ("minSilenceMs", autocut.min_silence_ms),
        ("paddingMs", autocut.padding_ms),
        ("minKeepMs", autocut.min_keep_ms),
    ] {
        if value > MAX_AUTOCUT_MS {
            issues.error(
                format!("/autocut/{field}"),
                format!("autocut {field} {value} is above {MAX_AUTOCUT_MS}"),
            );
        }
    }
}

/// A stream file name: `[A-Za-z0-9._-]+`, but not `.` or `..` (purge
/// deletes `takes/<take>/`, A§5.6).
fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn take_issues(issues: &mut Issues, id: &TakeId, take: &Take) {
    let path = format!("/takes/{}", token(id.as_str()));
    scene(issues, &format!("{path}/scene"), &take.scene, Some((id, take)));
    let finalized = matches!(take.status, TakeStatus::Ok | TakeStatus::TooShort);
    let mut ids: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, stream) in take.streams.iter().enumerate() {
        let at = format!("{path}/streams/{i}");
        if let Some(first) = ids.insert(stream.id.as_str(), i) {
            issues.error(
                format!("{at}/id"),
                format!("duplicate stream id {} in {id} (also at {path}/streams/{first})", stream.id),
            );
        }
        let prefix = format!("takes/{id}/");
        let name_ok = stream
            .file
            .strip_prefix(&prefix)
            .is_some_and(valid_file_name);
        if !name_ok {
            issues.error(
                format!("{at}/file"),
                format!(
                    "stream {} file {:?} must be {prefix}<name> with <name> made of letters, digits, '.', '_' and '-'",
                    stream.id, stream.file
                ),
            );
        }
        // A stream used by a non-missing layer or track of the take scene.
        let used = match stream.kind {
            StreamKind::Video => take
                .scene
                .layers
                .iter()
                .any(|l| l.id.as_str() == stream.id && !l.missing),
            StreamKind::Audio => take
                .scene
                .audio
                .iter()
                .any(|t| t.id.as_str() == stream.id && !t.missing),
        };
        if finalized && used {
            let missing = match stream.kind {
                StreamKind::Video => match (stream.width, stream.height) {
                    (Some(_), Some(_)) => None,
                    _ => Some("width and height"),
                },
                StreamKind::Audio => match (stream.sample_rate, stream.channels) {
                    (Some(_), Some(_)) => None,
                    _ => Some("sampleRate and channels"),
                },
            };
            if let Some(fields) = missing {
                let kind = match stream.kind {
                    StreamKind::Video => "video",
                    StreamKind::Audio => "audio",
                };
                issues.error(
                    at,
                    format!(
                        "{kind} stream {} of {id} lacks {fields} (only recording or broken takes and missing sources may)",
                        stream.id
                    ),
                );
            }
        }
    }
}

fn timeline(issues: &mut Issues, project: &Project) {
    let mut ids: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, clip) in project.timeline.clips.iter().enumerate() {
        let at = format!("/timeline/clips/{i}");
        if let Some(first) = ids.insert(clip.id.as_str(), i) {
            issues.error(
                format!("{at}/id"),
                format!("duplicate clip id {} (also at /timeline/clips/{first})", clip.id),
            );
        }
        match project.take(&clip.take) {
            None => issues.error(
                format!("{at}/take"),
                format!("clip {} uses {}, which doesn't exist", clip.id, clip.take),
            ),
            Some(take) if take.status != TakeStatus::Ok => issues.error(
                format!("{at}/take"),
                format!(
                    "clip {} uses {}, which is {} (clips need an ok take)",
                    clip.id,
                    clip.take,
                    take.status.as_str()
                ),
            ),
            Some(take) => {
                if clip.out > take.duration_frames {
                    issues.error(
                        format!("{at}/out"),
                        format!(
                            "clip {} ends at frame {}, past the end of {} ({} frames)",
                            clip.id, clip.out, clip.take, take.duration_frames
                        ),
                    );
                }
            }
        }
        if clip.in_ >= clip.out {
            issues.error(
                at,
                format!("clip {} range [{}, {}) is empty", clip.id, clip.in_, clip.out),
            );
        }
    }
}

/// `Ok` when `validate` finds no errors; otherwise `Invalid` with the first
/// error as the message and every issue in `details.issues`.
pub fn ensure_valid(project: &Project) -> Result<(), CoreError> {
    let issues = validate(project);
    match issues.iter().find(|i| i.severity == Severity::Error) {
        None => Ok(()),
        Some(first) => Err(CoreError::invalid(format!(
            "invalid project: {} ({})",
            first.message, first.path
        ))
        .with_details(serde_json::json!({ "issues": issues }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ClipId, LayerId, TrackId};
    use crate::model::{Fps, Rect};
    use crate::testkit::ProjectBuilder;
    use pretty_assertions::assert_eq;

    const SPEC_EXAMPLE: &str = include_str!("../tests/fixtures/spec_example.json");

    fn project() -> Project {
        ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .camera_layer("L2", "/dev/video0")
            .mic("A1", "alsa_input.mic")
            .system("A2", "alsa_output.monitor")
            .take("take-0001", 300)
            .take("take-0002", 200)
            .clip("c001", "take-0001", 0, 100)
            .clip("c002", "take-0002", 50, 200)
            .build()
    }

    fn summary(project: &Project) -> Vec<String> {
        validate(project).iter().map(Issue::to_string).collect()
    }

    #[test]
    fn valid_projects_have_no_issues() {
        let spec: Project = serde_json::from_str(SPEC_EXAMPLE).unwrap();
        assert_eq!(summary(&spec), Vec::<String>::new());
        assert_eq!(summary(&project()), Vec::<String>::new());
        assert!(ensure_valid(&project()).is_ok());
        assert!(validate(&ProjectBuilder::new().build()).is_empty());
    }

    #[test]
    fn project_level_errors_in_document_order() {
        let mut p = project();
        p.version = 2;
        p.canvas.width = 1921;
        p.canvas.height = 8;
        p.canvas.fps = Fps::new(480, 1);
        p.recording.countdown_secs = 11;
        p.autocut.threshold_db = -95.0;
        p.autocut.padding_ms = 70_000;
        p.autocut.track = Some(TrackId::numbered(9));
        p.next_take_number = 2;
        assert_eq!(
            summary(&p),
            [
                "error at /version: unsupported version 2 (this Qrec reads and writes version 1)",
                "error at /canvas/width: canvas width 1921 must be an even number from 16 to 8192",
                "error at /canvas/height: canvas height 8 must be an even number from 16 to 8192",
                "error at /canvas/fps: canvas fps 480 is above 240",
                "error at /recording/countdownSecs: countdown of 11 s is above the maximum of 10 s",
                "warning at /autocut/track: autocut track A9 isn't in the project scene; takes lacking it are skipped",
                "error at /autocut/thresholdDb: autocut threshold -95 dB is outside -90 to 0 dB",
                "error at /autocut/paddingMs: autocut paddingMs 70000 is above 60000",
                "error at /nextTakeNumber: nextTakeNumber is 2 but take-0002 exists (it must be above every take number)",
            ]
        );
        p.canvas.fps = Fps::new(30, 0);
        assert!(summary(&p).contains(
            &"error at /canvas/fps: canvas fps 30/0 needs a positive numerator and denominator".to_owned()
        ));
        let err = ensure_valid(&p).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Invalid);
        assert_eq!(
            err.message,
            "invalid project: unsupported version 2 (this Qrec reads and writes version 1) (/version)"
        );
        assert_eq!(err.details.unwrap()["issues"].as_array().unwrap().len(), 9);
    }

    #[test]
    fn scene_errors() {
        let mut p = project();
        p.scene.layers[0].opacity = 1.5;
        p.scene.layers[1].rect = Rect::new(0, 0, 0, 10);
        p.scene.layers[1].id = LayerId::numbered(1);
        p.scene.audio[0].gain_db = -70.0;
        p.scene.audio[1].sync_offset_ms = -5001;
        p.scene.audio[1].id = "L1".parse().unwrap();
        let take = p.takes.get_mut(&TakeId::numbered(2)).unwrap();
        take.scene.layers[0].opacity = f64::NAN;
        take.scene.audio[1].gain_db = 61.0;
        assert_eq!(
            summary(&p),
            [
                "error at /scene/layers/0/opacity: layer L1 opacity 1.5 is outside 0 to 1",
                "error at /scene/layers/1/id: duplicate id L1 in the scene (layer and track ids must be unique)",
                "error at /scene/layers/1/rect: layer L1 rect is 0×10; width and height must be at least 1",
                "error at /scene/audio/0/gainDb: track A1 gain -70 dB is outside -60 to 60 dB",
                "error at /scene/audio/1/id: duplicate id L1 in the scene (layer and track ids must be unique)",
                "error at /scene/audio/1/syncOffsetMs: track L1 sync offset -5001 ms is outside -5000 to 5000 ms",
                "error at /takes/take-0002/scene/layers/0/opacity: layer L1 opacity NaN is outside 0 to 1",
                "error at /takes/take-0002/scene/audio/1/gainDb: track A2 gain 61 dB is outside -60 to 60 dB",
            ]
        );
        // The limits themselves are fine.
        let mut p = project();
        p.scene.audio[0].gain_db = 60.0;
        p.scene.audio[0].sync_offset_ms = -5000;
        p.scene.layers[0].opacity = 0.0;
        p.scene.layers[0].rect = Rect::new(-5000, 9000, 1, 1);
        assert!(validate(&p).is_empty());
    }

    #[test]
    fn stream_errors_and_missing_media() {
        let mut p = project();
        let take = p.takes.get_mut(&TakeId::numbered(1)).unwrap();
        take.streams[0].file = "takes/take-0002/L1-screen-DP-1.mkv".into();
        take.streams[1].file = "takes/take-0001/..".into();
        take.streams[2].file = "takes/take-0001/A1 mic.mka".into();
        take.streams[3].id = "A1".into();
        take.streams[3].sample_rate = None;
        take.scene.layers.push(take.scene.layers[0].clone());
        take.scene.layers[2].id = LayerId::numbered(7);
        assert_eq!(
            summary(&p),
            [
                "warning at /takes/take-0001/scene/layers/2: layer L7 of take-0001 has no stream (missing media)",
                "warning at /takes/take-0001/scene/audio/1: track A2 of take-0001 has no stream (missing media)",
                "error at /takes/take-0001/streams/0/file: stream L1 file \"takes/take-0002/L1-screen-DP-1.mkv\" must be takes/take-0001/<name> with <name> made of letters, digits, '.', '_' and '-'",
                "error at /takes/take-0001/streams/1/file: stream L2 file \"takes/take-0001/..\" must be takes/take-0001/<name> with <name> made of letters, digits, '.', '_' and '-'",
                "error at /takes/take-0001/streams/2/file: stream A1 file \"takes/take-0001/A1 mic.mka\" must be takes/take-0001/<name> with <name> made of letters, digits, '.', '_' and '-'",
                "error at /takes/take-0001/streams/3/id: duplicate stream id A1 in take-0001 (also at /takes/take-0001/streams/2)",
                "error at /takes/take-0001/streams/3: audio stream A1 of take-0001 lacks sampleRate and channels (only recording or broken takes and missing sources may)",
            ]
        );
    }

    #[test]
    fn media_fields_are_required_only_for_finalized_used_streams() {
        let lacking = |p: &mut Project, take: u32| {
            let take = p.takes.get_mut(&TakeId::numbered(take)).unwrap();
            for s in &mut take.streams {
                s.width = None;
                s.height = None;
                s.sample_rate = None;
                s.channels = None;
            }
        };
        let mut p = project();
        lacking(&mut p, 1);
        let errors: Vec<Issue> = validate(&p);
        assert_eq!(errors.len(), 4, "{errors:#?}");
        assert_eq!(
            errors[0].to_string(),
            "error at /takes/take-0001/streams/0: video stream L1 of take-0001 lacks width and height (only recording or broken takes and missing sources may)"
        );

        // Recording and broken takes may lack them.
        for status in [TakeStatus::Recording, TakeStatus::Broken] {
            let mut p = project();
            p.timeline.clips.retain(|c| c.take != TakeId::numbered(1));
            lacking(&mut p, 1);
            p.takes.get_mut(&TakeId::numbered(1)).unwrap().status = status;
            assert!(validate(&p).is_empty(), "{status:?}");
        }
        // So may missing sources and streams no layer uses.
        let mut p = project();
        lacking(&mut p, 1);
        let take = p.takes.get_mut(&TakeId::numbered(1)).unwrap();
        take.scene.layers[0].missing = true;
        take.scene.audio[0].missing = true;
        take.scene.layers.remove(1);
        take.scene.audio.remove(1);
        assert_eq!(summary(&p), Vec::<String>::new());
    }

    #[test]
    fn clip_errors() {
        let mut p = project();
        p.takes.get_mut(&TakeId::numbered(2)).unwrap().status = TakeStatus::TooShort;
        p.timeline.clips.push(p.timeline.clips[0].clone());
        p.timeline.clips[0].out = 301;
        p.timeline.clips.push(crate::model::Clip {
            id: ClipId::from_raw("c009"),
            take: TakeId::numbered(9),
            in_: 5,
            out: 5,
        });
        assert_eq!(
            summary(&p),
            [
                "error at /timeline/clips/0/out: clip c001 ends at frame 301, past the end of take-0001 (300 frames)",
                "error at /timeline/clips/1/take: clip c002 uses take-0002, which is tooShort (clips need an ok take)",
                "error at /timeline/clips/2/id: duplicate clip id c001 (also at /timeline/clips/0)",
                "error at /timeline/clips/3/take: clip c009 uses take-0009, which doesn't exist",
                "error at /timeline/clips/3: clip c009 range [5, 5) is empty",
            ]
        );
    }

    #[test]
    fn pointer_tokens_are_escaped() {
        assert_eq!(token("a/b~c"), "a~1b~0c");
        assert!(valid_file_name("L1-screen-DP-4.mkv"));
        assert!(valid_file_name(".hidden"));
        for bad in ["", ".", "..", "a/b", "a b", "é.mkv"] {
            assert!(!valid_file_name(bad), "{bad:?}");
        }
    }
}
