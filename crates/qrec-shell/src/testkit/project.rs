//! Fixture projects: a valid `qrec.json` whose takes point at fixture media.
//! The JSON is built by hand with the SPEC §6.3 field names, so it doesn't
//! depend on core types that are still changing.

use std::fs;
use std::path::Path;

use qrec_core::Fps;
use qrec_core::time::{frame_ns, ns_to_frame_floor};
use serde_json::{Map, Value, json};

use super::media::{MediaSpec, fixture};

const RECORDED_AT: &str = "2026-10-14T12:00:00Z";
const VIDEO_FILE: &str = "L1-screen-FAKE-1.mkv";
const AUDIO_FILE: &str = "A1-mic.mka";

/// Writes `dir/qrec.json` and `dir/takes/` with one take per spec, each
/// placed on the timeline as one whole clip, and returns the project JSON.
///
/// Take `n` (from 1) is `take-000n` with clip `c00n` (hex) covering
/// `[0, durationFrames)`. A spec's video becomes layer `L1` (screen
/// `FAKE-1`) recorded to `L1-screen-FAKE-1.mkv`, its audio mic track `A1`
/// (`alsa_input.fake`) recorded to `A1-mic.mka`, like the fake capture
/// backend would. The canvas takes the first video's size and frame rate
/// (1920×1080 @ 30 without video), so keep fixture videos at least 16×16;
/// `durationFrames` follows A§2.2 with zero start offsets. Media files are
/// hard links to the cached fixtures (copies across file systems).
pub fn fixture_project(dir: &Path, takes: &[MediaSpec]) -> Value {
    let first_video = takes.iter().find_map(|spec| spec.video);
    let (width, height, fps) = first_video.map_or((1920, 1080, Fps::new(30, 1)), |v| {
        (v.width, v.height, v.fps)
    });
    let screen = json!({
        "id": "L1",
        "name": "Screen FAKE-1",
        "visible": true,
        "opacity": 1.0,
        "source": { "kind": "screen", "output": "FAKE-1", "label": "Fake screen 1" },
        "rect": { "x": 0, "y": 0, "w": width, "h": height },
        "crop": { "top": 0, "right": 0, "bottom": 0, "left": 0 },
        "fit": "contain",
    });
    let mic = json!({
        "id": "A1",
        "role": "mic",
        "source": "alsa_input.fake",
        "label": "Fake mic",
        "gainDb": 0.0,
        "muted": false,
        "syncOffsetMs": 0,
    });
    let scene = |video: bool, audio: bool| {
        json!({
            "layers": if video { vec![screen.clone()] } else { vec![] },
            "audio": if audio { vec![mic.clone()] } else { vec![] },
        })
    };

    let mut pool = Map::new();
    let mut clips = Vec::new();
    for (i, spec) in takes.iter().enumerate() {
        let n = i + 1;
        let take = format!("take-{n:04}");
        let take_dir = dir.join("takes").join(&take);
        assert!(!take_dir.exists(), "{} already exists", take_dir.display());
        fs::create_dir_all(&take_dir)
            .unwrap_or_else(|e| panic!("create {}: {e}", take_dir.display()));

        let mut streams = Vec::new();
        let mut duration_frames = None;
        if let Some(video_spec) = spec.video_only() {
            let v = video_spec.video.expect("video_only has video");
            let file = format!("takes/{take}/{VIDEO_FILE}");
            link(&fixture(&video_spec), &dir.join(&file));
            let duration_ns = frame_ns(v.frames as i64, v.fps);
            streams.push(json!({
                "id": "L1",
                "kind": "video",
                "file": file,
                "width": v.width,
                "height": v.height,
                "startOffsetNs": 0,
                "durationNs": duration_ns,
                "frames": v.frames,
                "fps": { "num": v.fps.num, "den": v.fps.den },
            }));
            duration_frames = Some(if v.fps == fps {
                v.frames
            } else {
                ns_to_frame_floor(duration_ns + frame_ns(1, fps) / 2, fps) as u64
            });
        }
        if let Some(audio_spec) = spec.audio_only() {
            let a = audio_spec.audio.clone().expect("audio_only has audio");
            let file = format!("takes/{take}/{AUDIO_FILE}");
            link(&fixture(&audio_spec), &dir.join(&file));
            let duration_ns = audio_spec.duration_ns();
            streams.push(json!({
                "id": "A1",
                "kind": "audio",
                "file": file,
                "sampleRate": a.sample_rate,
                "channels": a.channels,
                "startOffsetNs": 0,
                "durationNs": duration_ns,
            }));
            duration_frames.get_or_insert(ns_to_frame_floor(duration_ns, fps) as u64);
        }
        let duration_frames =
            duration_frames.unwrap_or_else(|| panic!("take {n}: a MediaSpec without media"));
        assert!(duration_frames > 0, "take {n} is shorter than one frame");
        pool.insert(
            take.clone(),
            json!({
                "status": "ok",
                "recordedAt": RECORDED_AT,
                "durationFrames": duration_frames,
                "scene": scene(spec.video.is_some(), spec.audio.is_some()),
                "streams": streams,
                "placed": true,
            }),
        );
        clips.push(
            json!({ "id": format!("c{n:03x}"), "take": take, "in": 0, "out": duration_frames }),
        );
    }

    let has_audio = takes.iter().any(|spec| spec.audio.is_some());
    let project = json!({
        "version": 1,
        "name": dir.file_name().and_then(|name| name.to_str()).unwrap_or("fixture"),
        "createdAt": RECORDED_AT,
        "canvas": {
            "width": width,
            "height": height,
            "fps": { "num": fps.num, "den": fps.den },
            "background": "#000000",
        },
        "scene": scene(first_video.is_some(), has_audio),
        "recording": { "countdownSecs": 0, "encoder": "auto", "vaapiDevice": null },
        "autocut": {
            "autoRunAfterTake": false,
            "track": if has_audio { json!("A1") } else { Value::Null },
            "thresholdDb": -35.0,
            "minSilenceMs": 400,
            "paddingMs": 120,
            "minKeepMs": 200,
        },
        "takes": pool,
        "nextTakeNumber": takes.len() + 1,
        "timeline": { "clips": clips },
    });
    write_project(dir, &project);
    project
}

/// Writes `project` to `dir/qrec.json` (pretty-printed), e.g. after a test
/// edited the JSON that [`fixture_project`] returned.
pub fn write_project(dir: &Path, project: &Value) {
    let mut text = serde_json::to_vec_pretty(project).expect("serialize the project");
    text.push(b'\n');
    let path = dir.join(qrec_core::PROJECT_FILE);
    fs::write(&path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// A hard link to a cached fixture (`fixture`), or a copy when the cache is
/// on another file system. `target` must not exist, and the copy only ever
/// creates a new file: copying onto a link to the cached file would open
/// the shared cache itself for writing and truncate it, for every later
/// test in every process.
pub(super) fn link(source: &Path, target: &Path) {
    assert!(
        fs::symlink_metadata(target).is_err(),
        "{} already exists: fixtures write into an empty folder",
        target.display()
    );
    if fs::hard_link(source, target).is_ok() {
        return;
    }
    let copied = fs::File::open(source).and_then(|mut from| {
        let mut to = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        std::io::copy(&mut from, &mut to)
    });
    if let Err(e) = copied {
        panic!("copy {} to {}: {e}", source.display(), target.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TestEnv;
    use qrec_core::{Project, StreamKind, TakeId, TakeStatus};

    /// Linking onto a file that is already there (a fixture written twice
    /// into one folder) stops with a clear message and leaves the cached
    /// file alone. It used to copy the cache onto its own hard link, which
    /// truncated the shared cache to 0 bytes for every later test.
    #[test]
    fn linking_twice_never_truncates_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        // The cache and the project on one file system: a real hard link.
        let cached = tmp.path().join("cached.mkv");
        fs::write(&cached, b"twenty bytes of media").unwrap();
        let target = tmp.path().join("L1-screen-FAKE-1.mkv");
        link(&cached, &target);
        let again = std::panic::catch_unwind(|| link(&cached, &target));
        let message = again.unwrap_err();
        let message = message
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or_default();
        assert!(message.contains("already exists"), "{message}");
        assert_eq!(fs::read(&cached).unwrap(), b"twenty bytes of media");
        assert_eq!(fs::read(&target).unwrap(), b"twenty bytes of media");
    }

    #[test]
    fn fixture_project_is_a_valid_project() {
        let env = TestEnv::new();
        let dir = env.empty_dir("swamp");
        let takes = [
            MediaSpec::video(320, 180, 30, 60).with_audio(48_000, 1, &[(0, 500), (1500, 2000)]),
            MediaSpec::audio(48_000, 2, 1500, &[(100, 900)]),
            MediaSpec::video(320, 180, 25, 50),
        ];
        let json = fixture_project(&dir, &takes);

        let text = fs::read_to_string(dir.join("qrec.json")).unwrap();
        let project: Project =
            serde_json::from_str(&text).expect("qrec.json matches the core model");
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json);
        assert_eq!(project.name, "swamp");
        assert_eq!(
            (
                project.canvas.width,
                project.canvas.height,
                project.canvas.fps
            ),
            (320, 180, Fps::new(30, 1))
        );
        assert_eq!(project.next_take_number, 4);
        assert_eq!(project.scene.layers.len(), 1);
        assert_eq!(project.scene.audio.len(), 1);
        assert_eq!(
            project.autocut.track.as_ref().map(|t| t.as_str()),
            Some("A1")
        );

        let take = |n| project.take(&TakeId::numbered(n)).unwrap();
        // Video at canvas fps: its frame count. Audio only: 1.5 s at 30 fps.
        // 25 fps video: 2 s re-derived at 30 fps (A§2.2).
        let durations: Vec<u64> = (1..=3).map(|n| take(n).duration_frames).collect();
        assert_eq!(durations, [60, 45, 60]);
        assert!((1..=3).all(|n| take(n).status == TakeStatus::Ok && take(n).placed));
        assert_eq!(take(1).streams.len(), 2);
        assert_eq!(take(2).streams[0].kind, StreamKind::Audio);
        assert_eq!(take(2).scene.layers.len(), 0);
        assert_eq!(take(3).streams[0].fps, Some(Fps::new(25, 1)));

        let clips: Vec<(String, String, u64, u64)> = project
            .timeline
            .clips
            .iter()
            .map(|c| (c.id.to_string(), c.take.to_string(), c.in_, c.out))
            .collect();
        assert_eq!(
            clips,
            [
                ("c001".into(), "take-0001".into(), 0, 60),
                ("c002".into(), "take-0002".into(), 0, 45),
                ("c003".into(), "take-0003".into(), 0, 60),
            ]
        );

        for take in project.takes.values() {
            for stream in &take.streams {
                let path = dir.join(&stream.file);
                assert!(path.is_file(), "{} is missing", path.display());
            }
        }
        // The take files carry the same media as their fixtures.
        let audio = crate::testkit::ffprobe(&dir.join("takes/take-0002/A1-mic.mka"));
        assert_eq!(audio["streams"][0]["channels"], 2);
        let video = crate::testkit::ffprobe(&dir.join("takes/take-0001/L1-screen-FAKE-1.mkv"));
        assert_eq!(
            video["streams"].as_array().unwrap().len(),
            1,
            "the layer file has video only"
        );
        assert_eq!(video["streams"][0]["nb_read_packets"], "60");
    }

    #[test]
    fn audio_only_projects_use_the_default_canvas() {
        let env = TestEnv::new();
        let dir = env.empty_dir("podcast");
        fixture_project(&dir, &[MediaSpec::audio(48_000, 1, 1000, &[(0, 1000)])]);
        let project: Project =
            serde_json::from_str(&fs::read_to_string(dir.join("qrec.json")).unwrap()).unwrap();
        assert_eq!((project.canvas.width, project.canvas.height), (1920, 1080));
        assert!(project.scene.layers.is_empty());
        assert_eq!(project.timeline.clips[0].out, 30);
    }
}
