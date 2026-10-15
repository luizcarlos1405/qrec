//! Pure test helpers (A§11): contexts, a project builder, states, a command
//! driver and proptest strategies. Available under `cfg(test)` and to other
//! crates with the `testkit` feature.

use std::collections::{BTreeSet, VecDeque};

use chrono::{DateTime, Duration, Utc};

use crate::config::UserConfig;
use crate::ctx::{Ctx, Facts};
use crate::error::CoreError;
use crate::ids::{ClipId, Frame, LayerId, ReqId, TakeId, TrackId};
use crate::model::{
    AudioRole, AudioTrack, Canvas, Clip, Crop, Fit, Fps, Layer, Project, Rect, Stream, StreamKind,
    Take, TakeStatus, VideoSource,
};
use crate::protocol::command::Command;
use crate::protocol::effect::Effect;
use crate::protocol::event::Event;
use crate::state::{Capabilities, FfmpegCaps, History, HostKind, State, UiState};
use crate::time::frames_to_ns;

/// When every test project was created.
pub const CREATED_AT: &str = "2026-10-14T12:00:00Z";

/// A context at `now` (RFC 3339) with `entropy`, UTC, not confirmed,
/// request `r1` and default facts.
pub fn ctx(now: &str, entropy: u64) -> Ctx {
    Ctx {
        now: parse_time(now),
        utc_offset_secs: 0,
        entropy,
        confirmed: false,
        req: Some(ReqId("r1".to_owned())),
        facts: Facts::default(),
    }
}

fn parse_time(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .unwrap_or_else(|e| panic!("bad RFC 3339 time {s:?}: {e}"))
        .with_timezone(&Utc)
}

/// A GUI-host state for `project` with an empty history, default UI and
/// ffmpeg capabilities detected (no VA-API).
pub fn state(project: Project) -> State {
    let mut state = State::new(project, History::new(), UiState::default(), HostKind::Gui);
    state.runtime.capabilities = capabilities();
    state
}

/// ffmpeg 8.1.2 with the encoders Qrec uses, and ffprobe; no VA-API.
pub fn capabilities() -> Capabilities {
    let encoders: BTreeSet<String> = [
        "libx264",
        "libvpx-vp9",
        "libsvtav1",
        "aac",
        "libopus",
        "pcm_s16le",
        "flac",
        "png",
    ]
    .map(String::from)
    .into();
    Capabilities {
        ffmpeg: Some(FfmpegCaps {
            version: "8.1.2".to_owned(),
            identity: "/usr/bin/ffmpeg".to_owned(),
            encoders,
        }),
        ffprobe: Some("/usr/bin/ffprobe".to_owned()),
        vaapi: None,
        cores: 8,
    }
}

/// Builds projects for tests. Add layers and tracks before takes: a take
/// snapshots the project scene and gets one stream per layer and track.
#[derive(Debug, Clone)]
pub struct ProjectBuilder {
    project: Project,
}

impl Default for ProjectBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectBuilder {
    /// An empty project named `test`, 1920×1080 @ 30, created at [`CREATED_AT`].
    pub fn new() -> Self {
        let defaults = UserConfig::default().project_defaults();
        Self {
            project: Project::new("test", parse_time(CREATED_AT), &defaults),
        }
    }

    pub fn name(mut self, name: &str) -> Self {
        self.project.name = name.to_owned();
        self
    }

    pub fn canvas(mut self, width: u32, height: u32, fps: Fps) -> Self {
        self.project.canvas = Canvas {
            width,
            height,
            fps,
            background: self.project.canvas.background,
        };
        self
    }

    /// A full-canvas screen layer, fit `contain`.
    pub fn screen_layer(mut self, id: &str, output: &str) -> Self {
        let canvas = &self.project.canvas;
        self.project.scene.layers.push(Layer {
            id: parse_id(id),
            name: format!("Screen {output}"),
            visible: true,
            opacity: 1.0,
            source: VideoSource::Screen {
                output: output.to_owned(),
                label: format!("Monitor {output}"),
            },
            rect: Rect::new(0, 0, canvas.width, canvas.height),
            crop: Crop::default(),
            fit: Fit::Contain,
            locked: false,
            missing: false,
        });
        self
    }

    /// A camera layer: 20 % of the canvas width at 4:3, bottom-right with a
    /// 2 % margin, fit `cover` (A§2.1).
    pub fn camera_layer(mut self, id: &str, device: &str) -> Self {
        let canvas = &self.project.canvas;
        let w = (canvas.width / 5) & !1;
        let h = (w * 3 / 4) & !1;
        let margin = canvas.width / 50;
        self.project.scene.layers.push(Layer {
            id: parse_id(id),
            name: "Camera".to_owned(),
            visible: true,
            opacity: 1.0,
            source: VideoSource::Camera {
                device: device.to_owned(),
                label: format!("Camera {device}"),
            },
            rect: Rect::new(
                (canvas.width - w - margin) as i32,
                (canvas.height.saturating_sub(h + margin)) as i32,
                w,
                h,
            ),
            crop: Crop::default(),
            fit: Fit::Cover,
            locked: false,
            missing: false,
        });
        self
    }

    pub fn mic(self, id: &str, source: &str) -> Self {
        self.audio(id, AudioRole::Mic, source, "Mic")
    }

    pub fn system(self, id: &str, source: &str) -> Self {
        self.audio(id, AudioRole::System, source, "System audio")
    }

    fn audio(mut self, id: &str, role: AudioRole, source: &str, label: &str) -> Self {
        self.project.scene.audio.push(AudioTrack {
            id: parse_id(id),
            role,
            source: source.to_owned(),
            label: label.to_owned(),
            gain_db: 0.0,
            muted: false,
            sync_offset_ms: 0,
            missing: false,
        });
        self
    }

    /// An `ok` take of `frames` frames (`id` like `take-0001`) with the
    /// current project scene and one finished stream per layer and track:
    /// `takes/<id>/L1-screen-DP-1.mkv` (canvas size), `L2-camera-0.mkv`
    /// (1280×720), `A1-mic.mka` (48 kHz mono), `A2-system.mka` (stereo); all
    /// at offset 0 and the canvas fps.
    pub fn take(mut self, id: &str, frames: Frame) -> Self {
        let take_id: TakeId = parse_id(id);
        let fps = self.project.canvas.fps;
        let scene = self.project.scene.clone();
        let duration_ns = Some(frames_to_ns(frames, fps));
        let mut streams = Vec::new();
        for layer in &scene.layers {
            let (name, width, height) = match &layer.source {
                VideoSource::Screen { output, .. } => (
                    format!("screen-{}", sanitize(output)),
                    self.project.canvas.width,
                    self.project.canvas.height,
                ),
                VideoSource::Camera { device, .. } => {
                    let n: String = device
                        .chars()
                        .rev()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    let n: String = n.chars().rev().collect();
                    (
                        format!("camera-{}", if n.is_empty() { "0" } else { &n }),
                        1280,
                        720,
                    )
                }
            };
            streams.push(Stream {
                id: layer.id.to_string(),
                kind: StreamKind::Video,
                file: format!("takes/{take_id}/{}-{name}.mkv", layer.id),
                width: Some(width),
                height: Some(height),
                sample_rate: None,
                channels: None,
                start_offset_ns: 0,
                duration_ns,
                frames: Some(frames),
                fps: Some(fps),
            });
        }
        for track in &scene.audio {
            streams.push(Stream {
                id: track.id.to_string(),
                kind: StreamKind::Audio,
                file: format!("takes/{take_id}/{}-{}.mka", track.id, track.role.as_str()),
                width: None,
                height: None,
                sample_rate: Some(48_000),
                channels: Some(if track.role == AudioRole::Mic { 1 } else { 2 }),
                start_offset_ns: 0,
                duration_ns,
                frames: None,
                fps: None,
            });
        }
        let number = take_id.number();
        let take = Take {
            status: TakeStatus::Ok,
            recorded_at: self.project.created_at + Duration::minutes(i64::from(number)),
            duration_frames: frames,
            scene,
            streams,
            error: None,
            placed: false,
            replaced: Vec::new(),
            recovered: false,
        };
        self.project.takes.insert(take_id, take);
        self.project.next_take_number = self.project.next_take_number.max(number + 1);
        self
    }

    /// Changes a take added earlier.
    pub fn with_take(mut self, id: &str, edit: impl FnOnce(&mut Take)) -> Self {
        let take = self
            .project
            .takes
            .get_mut(&parse_id::<TakeId>(id))
            .unwrap_or_else(|| panic!("no take {id}"));
        edit(take);
        self
    }

    /// Appends a clip `[in_, out)` of `take`.
    pub fn clip(mut self, id: &str, take: &str, in_: Frame, out: Frame) -> Self {
        self.project.timeline.clips.push(Clip {
            id: parse_id(id),
            take: parse_id(take),
            in_,
            out,
        });
        self
    }

    pub fn build(self) -> Project {
        self.project
    }
}

// ------------------------------------------------------- design sample

/// The design sample's mic source (BRIEF §3.1).
pub const DESIGN_MIC: &str =
    "alsa_input.usb-Blue_Microphones_Yeti_Nano_2041SG000000-00.analog-stereo";
/// The design sample's monitor source (BRIEF §3.1).
pub const DESIGN_MONITOR: &str = "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor";
/// The design sample's playhead: `00:01:21:07`, inside c2d0 (take 5 frame
/// 365), the default of the Program screens (BRIEF §3.3).
pub const DESIGN_PLAYHEAD: Frame = 2437;
/// The clip of take 4 that take 5 recorded over (`take-0005.replaced`).
pub const DESIGN_REPLACED_CLIP: &str = "c4a7";

/// Takes 1–9 of the design sample (BRIEF §3.2): number, frames, and when it
/// was recorded (taken as UTC). Take 9's frames were never probed.
const DESIGN_TAKES: [(u32, Frame, &str); 9] = [
    (1, 912, "2026-10-14T14:03:00Z"),
    (2, 664, "2026-10-14T14:06:00Z"),
    (3, 1377, "2026-10-14T14:08:00Z"),
    (4, 1548, "2026-10-14T14:12:00Z"),
    (5, 2214, "2026-10-14T14:15:00Z"),
    (6, 7, "2026-10-14T14:19:00Z"),
    (7, 1870, "2026-10-14T14:21:00Z"),
    (8, 3105, "2026-10-14T14:26:00Z"),
    (9, 0, "2026-10-14T14:31:00Z"),
];

/// The design sample's timeline (BRIEF §3.3): clip, take, in, out.
const DESIGN_CLIPS: [(&str, u32, Frame, Frame); 10] = [
    ("c7f3", 1, 0, 410),
    ("c8a1", 1, 455, 912),
    ("c1b2", 3, 0, 268),
    ("c1b3", 3, 304, 611),
    ("c1b4", 3, 652, 890),
    ("c1b5", 3, 931, 1180),
    ("c1b6", 3, 1222, 1377),
    ("c2d0", 5, 12, 1104),
    ("c2d1", 5, 1104, 2214),
    ("c3e9", 8, 0, 3105),
];

/// The project scene of the design sample (BRIEF §3.1): L1 "Main screen"
/// (DP-4, GIGA-BYTE GS27QA, full canvas, contain) under L2 "Face" (/dev/video0,
/// Logitech C920, at 1500, 760, 384×288, cover); A1 "Yeti Nano" (mic, the
/// autocut track) and A2 "System audio" at −6 dB.
fn design_builder() -> ProjectBuilder {
    let mut builder = ProjectBuilder::new()
        .name("rust-traits-talk")
        .screen_layer("L1", "DP-4")
        .camera_layer("L2", "/dev/video0")
        .mic("A1", DESIGN_MIC)
        .system("A2", DESIGN_MONITOR);
    let project = &mut builder.project;
    let [l1, l2] = &mut project.scene.layers[..] else {
        unreachable!("two layers were added");
    };
    l1.name = "Main screen".to_owned();
    l1.source = VideoSource::Screen {
        output: "DP-4".to_owned(),
        label: "GIGA-BYTE GS27QA".to_owned(),
    };
    l2.name = "Face".to_owned();
    l2.source = VideoSource::Camera {
        device: "/dev/video0".to_owned(),
        label: "Logitech C920".to_owned(),
    };
    l2.rect = Rect::new(1500, 760, 384, 288);
    let [a1, a2] = &mut project.scene.audio[..] else {
        unreachable!("two tracks were added");
    };
    a1.label = "Yeti Nano".to_owned();
    a2.label = "System audio".to_owned();
    a2.gain_db = -6.0;
    project.autocut.track = Some(parse_id("A1"));
    builder
}

/// The sample project every GUI design shows (BRIEF §3, GUI G§8.4 `Base`):
/// `rust-traits-talk`, 1920×1080 @ 30, the [`design_builder`] scene, takes
/// 1–9 with BRIEF's statuses and facts, and the 10 clips c7f3 … c3e9
/// (7,391 frames, `00:04:06:11`).
///
/// - Takes 1, 3, 5 and 8 are on the timeline; 2 was placed and discarded
///   (`d`); 4 was placed and recorded over by 5 (`O`: take 5's `replaced`
///   holds take 4's clip c4a7); 6 is `tooShort` (7 frames); 7 is `ok` and
///   `recovered` after a crash, never placed; 9 is `broken` with
///   [`EOS_TIMEOUT_REASON`](crate::on_event::record::EOS_TIMEOUT_REASON):
///   its unprobed streams left the take and its sources are `missing`.
/// - Screens record at 2560×1440, the camera at 1280×720, all at 30 fps.
/// - No history (like the shell's fixture); `nextTakeNumber` is 10.
pub fn design_project() -> Project {
    let mut builder = design_builder();
    for (n, frames, _) in DESIGN_TAKES {
        builder = builder.take(TakeId::numbered(n).as_str(), frames);
    }
    for (id, take, in_, out) in DESIGN_CLIPS {
        builder = builder.clip(id, TakeId::numbered(take).as_str(), in_, out);
    }
    let mut project = builder.build();
    for (n, _, recorded) in DESIGN_TAKES {
        let take = project
            .takes
            .get_mut(&TakeId::numbered(n))
            .expect("added above");
        take.recorded_at = parse_time(recorded);
        for stream in take.streams.iter_mut().filter(|s| s.id == "L1") {
            stream.width = Some(2560);
            stream.height = Some(1440);
        }
        match n {
            1 | 2 | 3 | 4 | 8 => take.placed = true,
            5 => {
                take.placed = true;
                take.replaced = vec![Clip {
                    id: parse_id(DESIGN_REPLACED_CLIP),
                    take: TakeId::numbered(4),
                    in_: 0,
                    out: 1548,
                }];
            }
            6 => take.status = TakeStatus::TooShort,
            7 => take.recovered = true,
            9 => {
                // The EOS timeout: nothing was probed, so no stream has its
                // media facts; they left the take and the sources are missing.
                take.status = TakeStatus::Broken;
                take.error = Some(crate::on_event::record::EOS_TIMEOUT_REASON.to_owned());
                take.duration_frames = 0;
                take.streams.clear();
                for layer in &mut take.scene.layers {
                    layer.missing = true;
                }
                for track in &mut take.scene.audio {
                    track.missing = true;
                }
            }
            _ => {}
        }
    }
    project
}

/// [`design_project`] in a GUI host with the playhead at
/// [`DESIGN_PLAYHEAD`]: Live mode, nothing selected, fit zoom, the bin
/// closed (S02 but for the playhead). Screens set their own UI.
pub fn design_state() -> State {
    let mut state = state(design_project());
    state.ui.playhead = DESIGN_PLAYHEAD;
    state
}

/// A long timeline for performance runs (GUI G§8.6): the design scene, 8
/// takes of 3,600 frames (2 min) and `clips` clips (`c0000`, `c0001`, …)
/// of 30–600 frames, cycling through the takes.
pub fn perf_project(clips: usize) -> Project {
    const TAKES: u32 = 8;
    const TAKE_FRAMES: Frame = 3600;
    let mut builder = design_builder().name("perf");
    for n in 1..=TAKES {
        builder = builder.take(TakeId::numbered(n).as_str(), TAKE_FRAMES);
    }
    for i in 0..clips as u64 {
        let take = TakeId::numbered((i % u64::from(TAKES)) as u32 + 1);
        let len = 30 + (i * 97) % 571;
        let in_ = (i * 131) % (TAKE_FRAMES - len);
        builder = builder.clip(&format!("c{i:04x}"), take.as_str(), in_, in_ + len);
    }
    let mut project = builder.build();
    for take in project.takes.values_mut() {
        take.placed = true;
    }
    project
}

fn parse_id<T: std::str::FromStr<Err = CoreError>>(s: &str) -> T {
    s.parse()
        .unwrap_or_else(|e: CoreError| panic!("{}", e.message))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A fake shell for [`drive`] that never answers.
pub fn no_shell(_: &State, _: &Effect) -> Vec<Event> {
    Vec::new()
}

/// Applies `cmds` in order (entropy `ctx.entropy + i` for command `i`) and
/// feeds every effect to `fake_shell`, whose events go to `on_event` until
/// no effects are left. Returns the first `apply` error.
///
/// Panics after 10,000 events (a fake shell that never settles).
pub fn drive(
    state: &State,
    ctx: &Ctx,
    cmds: &[Command],
    mut fake_shell: impl FnMut(&State, &Effect) -> Vec<Event>,
) -> Result<State, CoreError> {
    let mut state = state.clone();
    let mut budget = 10_000usize;
    for (i, cmd) in cmds.iter().enumerate() {
        let mut ctx = ctx.clone();
        ctx.entropy = ctx.entropy.wrapping_add(i as u64);
        let outcome = crate::apply::apply(&state, &ctx, cmd.clone())?;
        state = outcome.state;
        let mut queue: VecDeque<Effect> = outcome.effects.into();
        while let Some(effect) = queue.pop_front() {
            for event in fake_shell(&state, &effect) {
                budget = budget
                    .checked_sub(1)
                    .expect("drive: the fake shell never settles");
                let outcome = crate::on_event::on_event(&state, &ctx, event);
                state = outcome.state;
                queue.extend(outcome.effects);
            }
        }
    }
    Ok(state)
}

/// Proptest strategies.
pub mod strategies {
    use proptest::prelude::*;
    use proptest::sample::Index;

    use super::*;

    pub fn arb_fps() -> impl Strategy<Value = Fps> {
        prop_oneof![
            Just(Fps::new(30, 1)),
            Just(Fps::new(25, 1)),
            Just(Fps::new(24, 1)),
            Just(Fps::new(60, 1)),
            Just(Fps::new(30000, 1001)),
            Just(Fps::new(24000, 1001)),
        ]
    }

    /// Small valid projects: 0–2 screen layers, maybe a camera, 0–2 audio
    /// tracks (at least one source in all), 1–3 `ok` takes of 10–600 frames
    /// and 0–8 clips with valid ranges.
    pub fn arb_project() -> impl Strategy<Value = Project> {
        (
            arb_fps(),
            0usize..=2,
            any::<bool>(),
            0usize..=2,
            prop::collection::vec(10u64..600, 1..=3),
            prop::collection::vec((any::<Index>(), 0u64..10_000, 0u64..10_000), 0..8),
        )
            .prop_map(|(fps, screens, camera, tracks, takes, clips)| {
                // A take without any stream can't be `ok`.
                let tracks = if screens == 0 && !camera {
                    tracks.max(1)
                } else {
                    tracks
                };
                let mut b = ProjectBuilder::new().canvas(1920, 1080, fps);
                for n in 1..=screens {
                    b = b.screen_layer(LayerId::numbered(n as u32).as_str(), &format!("DP-{n}"));
                }
                if camera {
                    b = b.camera_layer(
                        LayerId::numbered(screens as u32 + 1).as_str(),
                        "/dev/video0",
                    );
                }
                for n in 1..=tracks {
                    let id = TrackId::numbered(n as u32);
                    b = if n == 1 {
                        b.mic(id.as_str(), "alsa_input.mic")
                    } else {
                        b.system(id.as_str(), "alsa_output.monitor")
                    };
                }
                for (n, frames) in takes.iter().enumerate() {
                    b = b.take(TakeId::numbered(n as u32 + 1).as_str(), *frames);
                }
                for (i, (take, a, len)) in clips.into_iter().enumerate() {
                    let t = take.index(takes.len());
                    let frames = takes[t];
                    let in_ = a % frames;
                    let out = in_ + 1 + len % (frames - in_);
                    let id = ClipId::from_raw(format!("c{:03x}", i + 1));
                    b = b.clip(
                        id.as_str(),
                        TakeId::numbered(t as u32 + 1).as_str(),
                        in_,
                        out,
                    );
                }
                b.build()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn builder_makes_spec_like_projects() {
        let p = ProjectBuilder::new()
            .screen_layer("L1", "DP-4")
            .camera_layer("L2", "/dev/video0")
            .mic("A1", "alsa_input.usb")
            .system("A2", "alsa_output.monitor")
            .take("take-0001", 912)
            .clip("c7f3", "take-0001", 0, 410)
            .clip("c8a1", "take-0001", 455, 912)
            .build();
        assert_eq!(p.next_take_number, 2);
        let take = p.take(&TakeId::numbered(1)).unwrap();
        let files: Vec<&str> = take.streams.iter().map(|s| s.file.as_str()).collect();
        assert_eq!(
            files,
            [
                "takes/take-0001/L1-screen-DP-4.mkv",
                "takes/take-0001/L2-camera-0.mkv",
                "takes/take-0001/A1-mic.mka",
                "takes/take-0001/A2-system.mka"
            ]
        );
        assert_eq!(p.scene.layers[1].rect, Rect::new(1498, 754, 384, 288));
        assert_eq!(
            crate::requantize::derive_duration_frames(take, p.canvas.fps),
            912
        );
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
    }

    #[test]
    fn the_design_sample_follows_the_brief() {
        use crate::query::{query, take_list};
        use crate::time::timecode;
        use crate::timeline::{clip_at, position_of, total_frames};

        let state = design_state();
        let p = &state.project;
        crate::validate::ensure_valid(p).unwrap();
        assert_eq!(p.name, "rust-traits-talk");
        assert_eq!((p.canvas.width, p.canvas.height), (1920, 1080));
        assert_eq!(p.canvas.fps, Fps::new(30, 1));
        assert_eq!(p.canvas.background.as_str(), "#000000");
        assert_eq!(p.next_take_number, 10);
        assert_eq!(state.runtime.host, HostKind::Gui);

        // §3.1: the scene, bottom layer first.
        let names: Vec<(&str, &str, &str)> = p
            .scene
            .layers
            .iter()
            .map(|l| (l.id.as_str(), l.name.as_str(), l.source.label()))
            .collect();
        assert_eq!(
            names,
            [
                ("L1", "Main screen", "GIGA-BYTE GS27QA"),
                ("L2", "Face", "Logitech C920")
            ]
        );
        let face = &p.scene.layers[1];
        assert_eq!(
            (face.rect, face.fit, face.opacity, face.visible),
            (Rect::new(1500, 760, 384, 288), Fit::Cover, 1.0, true)
        );
        assert_eq!(p.scene.layers[0].rect, Rect::new(0, 0, 1920, 1080));
        assert_eq!(p.scene.layers[0].fit, Fit::Contain);
        let tracks: Vec<(&str, &str, AudioRole, f64)> = p
            .scene
            .audio
            .iter()
            .map(|t| (t.id.as_str(), t.label.as_str(), t.role, t.gain_db))
            .collect();
        assert_eq!(
            tracks,
            [
                ("A1", "Yeti Nano", AudioRole::Mic, 0.0),
                ("A2", "System audio", AudioRole::System, -6.0)
            ]
        );
        assert_eq!(p.autocut.track.as_ref().map(TrackId::as_str), Some("A1"));
        assert_eq!(
            (
                p.autocut.threshold_db,
                p.autocut.min_silence_ms,
                p.autocut.padding_ms,
                p.autocut.min_keep_ms
            ),
            (-35.0, 400, 120, 200),
            "BRIEF §3.6's autocut settings"
        );

        // §3.3: 10 clips, 7,391 frames, their starts.
        let clips = &p.timeline.clips;
        let ids: Vec<&str> = clips.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "c7f3", "c8a1", "c1b2", "c1b3", "c1b4", "c1b5", "c1b6", "c2d0", "c2d1", "c3e9"
            ]
        );
        assert_eq!(total_frames(clips), 7391);
        assert_eq!(timecode(7391, p.canvas.fps), "00:04:06:11");
        let starts: Vec<String> = (0..clips.len())
            .map(|i| timecode(position_of(clips, i), p.canvas.fps))
            .collect();
        assert_eq!(
            starts,
            [
                "00:00:00:00",
                "00:00:13:20",
                "00:00:28:27",
                "00:00:37:25",
                "00:00:48:02",
                "00:00:56:00",
                "00:01:04:09",
                "00:01:09:14",
                "00:01:45:26",
                "00:02:22:26"
            ]
        );
        // The playhead: 00:01:21:07, inside c2d0 at take 5 frame 365.
        assert_eq!(state.ui.playhead, DESIGN_PLAYHEAD);
        assert_eq!(timecode(state.ui.playhead, p.canvas.fps), "00:01:21:07");
        let (i, offset) = clip_at(clips, state.ui.playhead).unwrap();
        assert_eq!(clips[i].id.as_str(), "c2d0");
        assert_eq!(clips[i].in_ + offset, 365);
        assert!(state.ui.selection.is_empty());

        // §3.2: statuses and facts.
        let take = |n: u32| &p.takes[&TakeId::numbered(n)];
        let facts: Vec<(u32, TakeStatus, Frame, bool, bool)> = (1..=9)
            .map(|n| {
                let t = take(n);
                (n, t.status, t.duration_frames, t.placed, t.recovered)
            })
            .collect();
        assert_eq!(
            facts,
            [
                (1, TakeStatus::Ok, 912, true, false),
                (2, TakeStatus::Ok, 664, true, false),
                (3, TakeStatus::Ok, 1377, true, false),
                (4, TakeStatus::Ok, 1548, true, false),
                (5, TakeStatus::Ok, 2214, true, false),
                (6, TakeStatus::TooShort, 7, false, false),
                (7, TakeStatus::Ok, 1870, false, true),
                (8, TakeStatus::Ok, 3105, true, false),
                (9, TakeStatus::Broken, 0, false, false),
            ]
        );
        assert_eq!(
            take(9).error.as_deref(),
            Some(crate::on_event::record::EOS_TIMEOUT_REASON)
        );
        assert!(take(9).streams.is_empty());
        assert!(take(9).scene.layers.iter().all(|l| l.missing));
        assert_eq!(take(5).replaced.len(), 1);
        assert_eq!(take(5).replaced[0].id.as_str(), DESIGN_REPLACED_CLIP);
        assert_eq!(take(5).replaced[0].take, TakeId::numbered(4));
        assert_eq!(
            take(3)
                .streams
                .iter()
                .map(|s| s.file.as_str())
                .collect::<Vec<_>>(),
            [
                "takes/take-0003/L1-screen-DP-4.mkv",
                "takes/take-0003/L2-camera-0.mkv",
                "takes/take-0003/A1-mic.mka",
                "takes/take-0003/A2-system.mka"
            ]
        );
        let l1 = take(1).stream("L1").unwrap();
        assert_eq!((l1.width, l1.height), (Some(2560), Some(1440)));
        let l2 = take(1).stream("L2").unwrap();
        assert_eq!((l2.width, l2.height), (Some(1280), Some(720)));
        assert_eq!(
            take(7).recorded_at.to_rfc3339(),
            "2026-10-14T14:21:00+00:00"
        );

        // §3.4: the bin holds takes 2, 4, 6, 7 and 9.
        let rows = take_list(&state, true).unwrap();
        let bin: Vec<&str> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            bin,
            [
                "take-0002",
                "take-0004",
                "take-0006",
                "take-0007",
                "take-0009"
            ]
        );
        let status = query(&state, &ctx(CREATED_AT, 1), &crate::Query::Status).unwrap();
        assert_eq!(
            (status["takes"].as_u64(), status["clips"].as_u64()),
            (Some(9), Some(10))
        );
        // `d` discards take 8, the newest placed take on the timeline.
        assert_eq!(
            crate::keymap::command_for("take.discard-last", &state),
            Some(Command::TakeDiscardLast)
        );
    }

    #[test]
    fn perf_projects_are_valid_and_long() {
        let p = perf_project(300);
        crate::validate::ensure_valid(&p).unwrap();
        assert_eq!(p.timeline.clips.len(), 300);
        let ids: BTreeSet<&str> = p.timeline.clips.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), 300, "unique clip ids");
        assert!(
            p.timeline
                .clips
                .iter()
                .all(|c| (30..=600).contains(&c.len()))
        );
        assert_eq!(p.takes.len(), 8);
        assert!(crate::timeline::total_frames(&p.timeline.clips) > 300 * 30);
        assert_eq!(perf_project(300), p, "deterministic");
        assert!(perf_project(0).timeline.clips.is_empty());
    }

    #[test]
    fn drive_stops_at_the_first_error() {
        let state = state(ProjectBuilder::new().build());
        let cmds = [Command::Play];
        let mut s = state.clone();
        s.runtime.host = HostKind::Cli;
        let err = drive(&s, &ctx(CREATED_AT, 1), &cmds, no_shell).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Unavailable);
    }

    proptest! {
        #[test]
        fn arbitrary_projects_are_well_formed(p in strategies::arb_project()) {
            for clip in &p.timeline.clips {
                let take = p.take(&clip.take).unwrap();
                prop_assert!(clip.in_ < clip.out && clip.out <= take.duration_frames);
                prop_assert_eq!(crate::requantize::derive_duration_frames(take, p.canvas.fps), take.duration_frames);
            }
            let json = serde_json::to_string(&p).unwrap();
            prop_assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
        }
    }
}
