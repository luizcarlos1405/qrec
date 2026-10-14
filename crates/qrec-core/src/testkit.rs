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

/// ffmpeg 8.1.2 with the encoders Qrec uses; no VA-API.
pub fn capabilities() -> Capabilities {
    let encoders: BTreeSet<String> = [
        "libx264",
        "libvpx-vp9",
        "libsvtav1",
        "aac",
        "libopus",
        "pcm_s16le",
        "flac",
    ]
    .map(String::from)
    .into();
    Capabilities {
        ffmpeg: Some(FfmpegCaps {
            version: "8.1.2".to_owned(),
            identity: "/usr/bin/ffmpeg".to_owned(),
            encoders,
        }),
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
