//! The export and frame plans against real ffmpeg (A§9.3, A§9.4, A§11).
//!
//! Fixtures are generated in a temp project dir: 320×180 H.264 in Matroska
//! (no B-frames, a keyframe per second, like the recorder) at 30 or
//! 30000/1001 fps, whose frames show their own index as binary bars, and
//! FLAC tracks with 1 kHz tone bursts at known file times. The project's
//! streams start 0, +41 ms and −20 ms after T0 (audio +3 ms with a +20 ms
//! sync offset, and −20 ms). Every planned argv runs verbatim with the
//! project dir as cwd, then:
//!
//! - each segment has exactly its unit's video packets and PCM samples, and
//!   ffmpeg's `-progress` blocks say so;
//! - every output frame shows the recorded frame nearest to its take time,
//!   layer by layer (the measured frame-selection rule);
//! - every tone burst lands where the take time says, the mono track is
//!   copied to both channels at 0 dB, gains apply, and the 240-sample fades
//!   sit at cuts but not at take edges;
//! - the stitch keeps every frame bit-exact and constant-rate, with evenly
//!   rising DTS even after units of one or two frames, and keeps every unit
//!   at its timeline time however many there are (WebM included);
//! - a split timeline plans and renders byte-identical segments;
//! - frame saves are sRGB PNGs of the same pixels, and colours are BT.709.
//!
//! Needs `ffmpeg`/`ffprobe` 8.x with libx264, libvpx-vp9, libsvtav1, aac and
//! libopus on `PATH` (the Nix dev shell). Uses only std and `qrec-core`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use qrec_core::config::UserConfig;
use qrec_core::ctx::{Ctx, Facts};
use qrec_core::export_plan::{ExportOptions, SEGMENTS_DIR, plan_export, render_units};
use qrec_core::frame_plan::plan_frame;
use qrec_core::media_argv::{probe_argv, segment_check_argv};
use qrec_core::parse::ffprobe::{SegmentCounts, parse_ffprobe, parse_segment_check};
use qrec_core::parse::progress::{ProgressBlock, ProgressParser};
use qrec_core::plans::{ExportPlan, SegmentPlan};
use qrec_core::protocol::command::{Command, ExportFormat, Quality, VideoCodec};
use qrec_core::protocol::effect::Effect;
use qrec_core::protocol::event::{Event, MediaInfo};
use qrec_core::requantize::derive_duration_frames;
use qrec_core::state::{Capabilities, FfmpegCaps, History, HostKind, JobKind, State, UiState};
use qrec_core::time::{NS_PER_MS, frame_ns};
use qrec_core::{
    AudioRole, AudioTrack, Canvas, Clip, ClipId, Crop, Fit, Fps, Frame, Layer, LayerId, Project,
    ProjectDefaults, Rect, ReqId, Scene, Stream, StreamKind, Take, TakeId, TakeStatus, VideoSource,
};

const W: u32 = 320;
const H: u32 = 180;
const FPS30: Fps = Fps::new(30, 1);
const NTSC: Fps = Fps::new(30000, 1001);
/// Frames in every video fixture.
const VIDEO_FRAMES: u64 = 120;
/// Bars of the frame-ID pattern: bits 0–6 of the frame index, bar 7 always
/// on (a frame with the marker off is the background).
const BAR_PX: u32 = 40;
const ID_BITS: u32 = 7;
const SAMPLE_RATE: i64 = 48_000;
/// The tone bursts' amplitude at 0 dB (0.5 full scale).
const TONE_AMP: f64 = 16_384.0;
/// The saturated background, so a wrong YUV matrix shows.
const BACKGROUND: &str = "#C02040";
const BACKGROUND_RGB: [u8; 3] = [0xC0, 0x20, 0x40];
const TOOL_TIMEOUT: Duration = Duration::from_secs(180);

// ------------------------------------------------------------- fixtures

/// A layer of a fixture take: where it sits and when its recording starts.
struct LayerFx {
    id: &'static str,
    offset_ns: i64,
    rect: Rect,
    fit: Fit,
    /// The canvas row its bars are read on.
    probe_y: u32,
}

/// A track of a fixture take: tone bursts at file times.
struct TrackFx {
    id: &'static str,
    role: AudioRole,
    channels: u32,
    offset_ns: i64,
    sync_ms: i32,
    gain_db: f64,
    duration_ms: i64,
    bursts_ms: &'static [(i64, i64)],
}

struct TakeFx {
    id: &'static str,
    layers: Vec<LayerFx>,
    tracks: Vec<TrackFx>,
}

impl TakeFx {
    fn take_id(&self) -> TakeId {
        self.id.parse().unwrap()
    }

    fn video_file(&self, layer: &LayerFx) -> String {
        format!("takes/{}/{}-screen.mkv", self.id, layer.id)
    }

    fn audio_file(&self, track: &TrackFx) -> String {
        format!("takes/{}/{}-{}.mka", self.id, track.id, track.role.as_str())
    }
}

/// take-0001: three layers in horizontal bands (gaps of background between
/// them), a mono mic and stereo system audio. take-0002: one full-canvas
/// layer and a mic that ends early.
fn fixture_takes() -> Vec<TakeFx> {
    let band = |id, offset_ns, y: u32| LayerFx {
        id,
        offset_ns,
        rect: Rect::new(0, y as i32, W, 50),
        fit: Fit::Cover,
        probe_y: y + 25,
    };
    vec![
        TakeFx {
            id: "take-0001",
            layers: vec![
                band("L1", 0, 0),
                band("L2", 41 * NS_PER_MS, 60),
                band("L3", -20 * NS_PER_MS, 120),
            ],
            tracks: vec![
                TrackFx {
                    id: "A1",
                    role: AudioRole::Mic,
                    channels: 1,
                    offset_ns: 3 * NS_PER_MS,
                    sync_ms: 20,
                    gain_db: 0.0,
                    duration_ms: 4000,
                    bursts_ms: &[
                        (300, 600),
                        (1000, 1200),
                        (2200, 2500),
                        (3400, 3600),
                        (3700, 4000),
                    ],
                },
                TrackFx {
                    id: "A2",
                    role: AudioRole::System,
                    channels: 2,
                    offset_ns: -20 * NS_PER_MS,
                    sync_ms: 0,
                    gain_db: -6.0,
                    duration_ms: 4000,
                    bursts_ms: &[(700, 900), (1500, 1800), (2800, 3100)],
                },
            ],
        },
        TakeFx {
            id: "take-0002",
            layers: vec![LayerFx {
                id: "L1",
                offset_ns: 0,
                rect: Rect::new(0, 0, W, H),
                fit: Fit::Contain,
                probe_y: H / 2,
            }],
            tracks: vec![TrackFx {
                id: "A1",
                role: AudioRole::Mic,
                channels: 1,
                offset_ns: 0,
                sync_ms: 0,
                gain_db: 0.0,
                duration_ms: 500,
                bursts_ms: &[(100, 300)],
            }],
        },
    ]
}

/// The timeline: a continuous pair (one unit), a cut, a jump back, a unit
/// that ends at the take's end and one of the second take.
const CLIPS: &[(&str, &str, Frame, Frame)] = &[
    ("c1", "take-0001", 0, 30),
    ("c2", "take-0001", 30, 50),
    ("c3", "take-0001", 65, 90),
    ("c4", "take-0001", 12, 20),
    ("c5", "take-0001", 100, 119),
    ("c6", "take-0002", 5, 25),
];

/// A project dir with fixture media and the project that uses it.
struct Fixture {
    dir: TempDir,
    fps: Fps,
    takes: Vec<TakeFx>,
    project: Project,
}

impl Fixture {
    /// Fixtures recorded at `fps` on a canvas of the same rate.
    fn new(tag: &str, fps: Fps) -> Self {
        let dir = TempDir::new(tag);
        let takes = fixture_takes();
        let mut project = Project::new("plans", Default::default(), &ProjectDefaults::default());
        project.canvas = Canvas {
            width: W,
            height: H,
            fps,
            background: BACKGROUND.parse().unwrap(),
        };
        let mut video_template: Option<String> = None;
        for fx in &takes {
            fs::create_dir_all(dir.path().join("takes").join(fx.id)).unwrap();
            let mut scene = Scene::default();
            let mut streams = Vec::new();
            for layer in &fx.layers {
                let file = fx.video_file(layer);
                match &video_template {
                    Some(first) => {
                        fs::copy(dir.path().join(first), dir.path().join(&file)).unwrap();
                    }
                    None => {
                        make_video(dir.path(), &file, fps);
                        video_template = Some(file.clone());
                    }
                }
                let info = probe(dir.path(), &file, StreamKind::Video);
                assert_eq!(info.frames, Some(VIDEO_FRAMES), "{file}");
                assert_eq!(info.fps, Some(fps), "{file}");
                assert_eq!((info.width, info.height), (Some(W), Some(H)), "{file}");
                assert_eq!(info.color_range.as_deref(), Some("tv"), "{file}");
                assert_eq!(info.color_space.as_deref(), Some("bt709"), "{file}");
                assert_eq!(info.start_time_ns, 0, "{file}");
                scene.layers.push(screen_layer(layer));
                streams.push(stream(layer.id, file, layer.offset_ns, &info));
            }
            for track in &fx.tracks {
                let file = fx.audio_file(track);
                make_audio(dir.path(), &file, track);
                let info = probe(dir.path(), &file, StreamKind::Audio);
                assert_eq!(info.sample_rate, Some(48_000), "{file}");
                assert_eq!(info.channels, Some(track.channels), "{file}");
                assert_eq!(
                    info.duration_ns,
                    Some(track.duration_ms * NS_PER_MS),
                    "{file}"
                );
                scene.audio.push(AudioTrack {
                    id: track.id.parse().unwrap(),
                    role: track.role,
                    source: format!("fake.{}", track.id),
                    label: track.id.to_owned(),
                    gain_db: track.gain_db,
                    muted: false,
                    sync_offset_ms: track.sync_ms,
                    missing: false,
                });
                streams.push(stream(track.id, file, track.offset_ns, &info));
            }
            let mut take = Take {
                status: TakeStatus::Ok,
                recorded_at: Default::default(),
                duration_frames: 0,
                scene,
                streams,
                error: None,
                placed: true,
                replaced: Vec::new(),
            };
            take.duration_frames = derive_duration_frames(&take, fps);
            project.takes.insert(fx.take_id(), take);
        }
        project.next_take_number = 3;
        // The −20 ms layer ends a frame early: 119 frames of take 1.
        assert_eq!(project.takes[&TakeId::numbered(1)].duration_frames, 119);
        assert_eq!(
            project.takes[&TakeId::numbered(2)].duration_frames,
            VIDEO_FRAMES
        );
        project.timeline.clips = CLIPS
            .iter()
            .map(|&(id, take, in_, out)| Clip {
                id: ClipId::from_raw(id),
                take: take.parse().unwrap(),
                in_,
                out,
            })
            .collect();
        Self {
            dir,
            fps,
            takes,
            project,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn take(&self, id: &TakeId) -> &TakeFx {
        self.takes.iter().find(|t| &t.take_id() == id).unwrap()
    }

    fn duration_frames(&self, id: &TakeId) -> Frame {
        self.project.takes[id].duration_frames
    }
}

/// The scene layer of a fixture layer: a visible, opaque screen.
fn screen_layer(layer: &LayerFx) -> Layer {
    Layer {
        id: layer.id.parse::<LayerId>().unwrap(),
        name: layer.id.to_owned(),
        visible: true,
        opacity: 1.0,
        source: VideoSource::Screen {
            output: format!("FAKE-{}", layer.id),
            label: layer.id.to_owned(),
        },
        rect: layer.rect,
        crop: Crop::default(),
        fit: layer.fit,
        locked: false,
        missing: false,
    }
}

/// A take recorded at `recorded` (`VIDEO_FRAMES` frame-ID frames, stamped
/// like the recorder's Matroska) on a canvas at `canvas`: L1 from T0 and L2
/// 41 ms late, in bands, and no audio.
fn mixed_rate_fixture(tag: &str, recorded: Fps, canvas: Fps) -> Fixture {
    let dir = TempDir::new(tag);
    let band = |id, offset_ns, y: u32| LayerFx {
        id,
        offset_ns,
        rect: Rect::new(0, y as i32, W, 50),
        fit: Fit::Cover,
        probe_y: y + 25,
    };
    let fx = TakeFx {
        id: "take-0001",
        layers: vec![band("L1", 0, 0), band("L2", 41 * NS_PER_MS, 60)],
        tracks: Vec::new(),
    };
    let mut project = Project::new("mixed", Default::default(), &ProjectDefaults::default());
    project.canvas = Canvas {
        width: W,
        height: H,
        fps: canvas,
        background: BACKGROUND.parse().unwrap(),
    };
    fs::create_dir_all(dir.path().join("takes").join(fx.id)).unwrap();
    let mut scene = Scene::default();
    let mut streams = Vec::new();
    let first = fx.video_file(&fx.layers[0]);
    make_video(dir.path(), &first, recorded);
    for layer in &fx.layers {
        let file = fx.video_file(layer);
        if file != first {
            fs::copy(dir.path().join(&first), dir.path().join(&file)).unwrap();
        }
        let info = probe(dir.path(), &file, StreamKind::Video);
        assert_eq!(info.frames, Some(VIDEO_FRAMES), "{file}");
        assert_eq!(info.fps, Some(recorded), "{file}");
        scene.layers.push(screen_layer(layer));
        streams.push(stream(layer.id, file, layer.offset_ns, &info));
    }
    let mut take = Take {
        status: TakeStatus::Ok,
        recorded_at: Default::default(),
        duration_frames: 0,
        scene,
        streams,
        error: None,
        placed: true,
        replaced: Vec::new(),
    };
    take.duration_frames = derive_duration_frames(&take, canvas);
    project.takes.insert(fx.take_id(), take);
    project.next_take_number = 2;
    Fixture {
        dir,
        fps: canvas,
        takes: vec![fx],
        project,
    }
}

/// A finished stream from what ffprobe found, like take finalization does.
fn stream(id: &str, file: String, offset_ns: i64, info: &MediaInfo) -> Stream {
    Stream {
        id: id.to_owned(),
        kind: info.kind,
        file,
        width: info.width,
        height: info.height,
        sample_rate: info.sample_rate,
        channels: info.channels,
        start_offset_ns: offset_ns,
        duration_ns: info.duration_ns,
        frames: info.frames,
        fps: info.fps,
    }
}

/// 120 frames whose bars spell their index (H.264, Matroska, BT.709), with
/// the recorder's timestamps: GStreamer's `matroskamux` *truncates* them to
/// milliseconds where ffmpeg rounds, so the encode is remuxed with frame `k`
/// at `⌊k·1000/fps⌋` ms. That is what the planner's 2 ms seek back-off is
/// for: without it, every frame whose time truncates (k ≡ 2 mod 3 at 30
/// fps) would show the next one.
fn make_video(dir: &Path, file: &str, fps: Fps) {
    let id = format!(
        "if(eq(floor(X/{BAR_PX}),{ID_BITS}),235,if(mod(floor(N/pow(2,floor(X/{BAR_PX}))),2),235,16))"
    );
    let graph = format!(
        "color=c=black:s={W}x{H}:r={}/{},trim=end_frame={VIDEO_FRAMES},\
         format=pix_fmts=yuv420p:color_spaces=bt709:color_ranges=tv,geq=lum='{id}':cb=128:cr=128,\
         setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709[v]",
        fps.num, fps.den
    );
    let gop = fps.nominal().to_string();
    let encoded = format!("{file}.ffmpeg.mkv");
    #[rustfmt::skip]
    ffmpeg(dir, &[
        "-filter_complex", &graph, "-map", "[v]",
        "-c:v", "libx264", "-preset", "veryfast", "-crf", "12",
        "-bf", "0", "-g", &gop, "-keyint_min", &gop, "-sc_threshold", "0",
        "-pix_fmt", "yuv420p", "-color_range", "tv", "-colorspace", "bt709",
        "-color_primaries", "bt709", "-color_trc", "bt709",
        "-f", "matroska", &format!("file:{encoded}"),
    ]);
    let truncate = format!("setts=ts=floor(N*1000*{}/{})", fps.den, fps.num);
    #[rustfmt::skip]
    ffmpeg(dir, &[
        "-i", &format!("file:{encoded}"), "-map", "0", "-c", "copy", "-bsf:v", &truncate,
        "-f", "matroska", &format!("file:{file}"),
    ]);
    fs::remove_file(dir.join(&encoded)).unwrap();
    // At 30 fps, frame 2 at 66.67 ms (66.73 ms at 29.97) is stored as 66 ms.
    #[rustfmt::skip]
    let pts = run(dir, &args(&[
        "ffprobe", "-v", "error", "-read_intervals", "%+#3", "-show_entries", "packet=pts",
        "-of", "csv=p=0", &format!("file:{file}"),
    ]));
    let truncated: Vec<String> = (0..3i64)
        .map(|k| (frame_ns(k, fps) / NS_PER_MS).to_string())
        .collect();
    assert_eq!(
        String::from_utf8_lossy(&pts.stdout)
            .split_whitespace()
            .collect::<Vec<_>>(),
        truncated
    );
}

/// 1 kHz bursts at half scale (each starts at a whole millisecond, so at
/// phase 0), digital silence elsewhere; FLAC 16-bit in Matroska.
fn make_audio(dir: &Path, file: &str, track: &TrackFx) {
    let gates: Vec<String> = track
        .bursts_ms
        .iter()
        .map(|&(a, b)| format!("gte(t,{})*lt(t,{})", seconds(a), seconds(b)))
        .collect();
    let layout = if track.channels == 1 {
        "mono"
    } else {
        "stereo"
    };
    let graph = format!(
        "aevalsrc=exprs='if(gt({},0),0.5*sin(2*PI*1000*t),0)':s={SAMPLE_RATE}:c={layout}:d={}[a]",
        gates.join("+"),
        seconds(track.duration_ms)
    );
    #[rustfmt::skip]
    ffmpeg(dir, &[
        "-filter_complex", &graph, "-map", "[a]",
        "-c:a", "flac", "-sample_fmt", "s16",
        "-f", "matroska", &format!("file:{file}"),
    ]);
}

fn seconds(ms: i64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

// ---------------------------------------------------------- expectations

/// The recorded frame take frame `f` should show: the one whose take time
/// (`offset + j/fps`) is nearest to the frame's, if the file has it. Worked
/// out independently of the planner (no offset in these tests is a tie).
fn expected_frame(offset_ns: i64, f: Frame, fps: Fps) -> Option<u64> {
    let per_ns = f64::from(fps.num) / (f64::from(fps.den) * 1e9);
    let j = (f as f64 - offset_ns as f64 * per_ns).round();
    (j >= 0.0 && (j as u64) < VIDEO_FRAMES).then_some(j as u64)
}

/// The recorded frame (rate `recorded`, starting `offset_ns` after T0) that
/// take frame `f` of a timeline at `canvas` shows: the nearest by exact
/// time, the earlier one on a tie (A§9.3), if the file has it. In whole
/// numbers (ties are common between rates), independently of the planner.
fn expected_at_rate(recorded: Fps, offset_ns: i64, canvas: Fps, f: Frame) -> Option<u64> {
    let (cn, cd) = (i128::from(canvas.num), i128::from(canvas.den));
    let (rn, rd) = (i128::from(recorded.num), i128::from(recorded.den));
    // Times scaled by R·C·10⁹: the canvas frame's in the file, and one
    // recorded frame.
    let t = (i128::from(f) * cd * 1_000_000_000 - i128::from(offset_ns) * cn) * rn;
    let step = rd * cn * 1_000_000_000;
    let before = t.div_euclid(step);
    let j = if (before + 1) * step - t < t - before * step {
        before + 1
    } else {
        before
    };
    (j >= 0 && j < i128::from(VIDEO_FRAMES)).then_some(j as u64)
}

/// Every frame of a unit starting at take frame `in_` shows, layer by
/// layer, the frame `expected_at_rate` names.
fn check_rate_rule(
    frames: &[Vec<u8>],
    take: &TakeFx,
    (recorded, canvas): (Fps, Fps),
    in_: Frame,
    what: &str,
) {
    for (t, gray) in frames.iter().enumerate() {
        let f = in_ + t as u64;
        for layer in &take.layers {
            assert_eq!(
                read_id(gray, layer.probe_y),
                expected_at_rate(recorded, layer.offset_ns, canvas, f),
                "{what}: output frame {t} (take frame {f}), layer {} ({} ns late), {recorded} on {canvas}",
                layer.id,
                layer.offset_ns
            );
        }
    }
}

/// The frame index spelled by the bars on row `y`; `None` without the marker.
fn read_id(gray: &[u8], y: u32) -> Option<u64> {
    let bit = |b: u32| gray[(y * W + b * BAR_PX + BAR_PX / 2) as usize] >= 128;
    bit(ID_BITS).then(|| (0..ID_BITS).filter(|&b| bit(b)).map(|b| 1u64 << b).sum())
}

/// Every output frame of a unit shows the expected recorded frame of every
/// layer.
fn check_frame_ids(frames: &[Vec<u8>], take: &TakeFx, in_: Frame, fps: Fps, what: &str) {
    for (t, gray) in frames.iter().enumerate() {
        let f = in_ + t as u64;
        for layer in &take.layers {
            assert_eq!(
                read_id(gray, layer.probe_y),
                expected_frame(layer.offset_ns, f, fps),
                "{what}: output frame {t} (take frame {f}), layer {} starting {} ns after T0",
                layer.id,
                layer.offset_ns
            );
        }
    }
}

/// A tone burst in a segment, in samples from the segment's start.
struct Placed {
    track: &'static str,
    start: f64,
    end: f64,
    amp: f64,
}

/// Where every burst of the take lands in the unit starting at `in_`.
fn placed_bursts(take: &TakeFx, in_: Frame, fps: Fps) -> Vec<Placed> {
    let unit_ns = frame_ns(in_ as i64, fps) as f64;
    let to_samples = |take_ns: f64| (take_ns - unit_ns) * SAMPLE_RATE as f64 / 1e9;
    let mut out = Vec::new();
    for track in &take.tracks {
        let shift = (track.offset_ns + i64::from(track.sync_ms) * NS_PER_MS) as f64;
        for &(a, b) in track.bursts_ms {
            out.push(Placed {
                track: track.id,
                start: to_samples((a * NS_PER_MS) as f64 + shift),
                end: to_samples((b * NS_PER_MS) as f64 + shift),
                amp: TONE_AMP * 10f64.powf(track.gain_db / 20.0),
            });
        }
    }
    out
}

/// The audio of a unit `[in_, out)`: burst edges within 1 ms, levels within
/// 2 % (mono upmixed at 0 dB, gains applied), silence elsewhere, and the
/// boundary fades exactly at cuts (none at the take's own start and end).
fn check_audio(pcm: &[[i16; 2]], take: &TakeFx, unit: (Frame, Frame, Frame), fps: Fps, what: &str) {
    let (in_, out, take_end) = unit;
    let len = pcm.len() as f64;
    let level = |range: std::ops::Range<usize>| -> [f64; 2] {
        let mut peak = [0f64; 2];
        for s in &pcm[range] {
            for (p, &v) in peak.iter_mut().zip(s) {
                *p = p.max(f64::from(v).abs());
            }
        }
        peak
    };
    let audible = |i: usize| pcm[i].iter().any(|&x| i32::from(x).abs() > 1000);
    assert!(
        pcm.iter()
            .all(|s| (i32::from(s[0]) - i32::from(s[1])).abs() <= 2),
        "{what}: the channels differ (the mono upmix must copy, and both fixtures are L = R)"
    );
    let bursts = placed_bursts(take, in_, fps);
    for b in &bursts {
        // Burst edges well inside the unit land within 1 ms.
        for (edge, is_start) in [(b.start, true), (b.end, false)] {
            if edge < 400.0 || edge > len - 400.0 {
                continue;
            }
            let around = (edge as usize - 200)..(edge as usize + 200);
            let found = if is_start {
                around.clone().find(|&i| audible(i))
            } else {
                around.clone().rev().find(|&i| audible(i)).map(|i| i + 1)
            };
            let found = found.unwrap_or_else(|| {
                panic!(
                    "{what}: no {} edge of a burst near sample {edge:.0}",
                    b.track
                )
            });
            assert!(
                (found as f64 - edge).abs() <= 48.0,
                "{what}: a {} burst edge is at sample {found}, expected {edge:.1}",
                b.track
            );
        }
        // Full level away from the edges and the unit's fades.
        let (from, to) = (b.start.max(240.0) + 300.0, b.end.min(len - 240.0) - 300.0);
        if to > from {
            for p in level(from as usize..to as usize) {
                assert!(
                    (p / b.amp - 1.0).abs() < 0.02,
                    "{what}: a {} burst peaks at {p}, expected {:.0}",
                    b.track,
                    b.amp
                );
            }
        }
    }
    // Silence wherever no burst plays.
    for (i, s) in pcm.iter().enumerate() {
        let x = i as f64;
        let near_burst = bursts
            .iter()
            .any(|b| x > b.start - 150.0 && x < b.end + 150.0);
        if !near_burst {
            assert!(
                s.iter().all(|&v| i32::from(v).abs() <= 40),
                "{what}: sample {i} should be silent, got {s:?}"
            );
        }
    }
    // Fades: 240 linear samples where the unit cuts into the take, none at
    // the take's own edges. Checked where a burst plays across the edge.
    let playing = |from: f64, to: f64| bursts.iter().find(|b| b.start < from && b.end > to);
    if let Some(b) = playing(-50.0, 600.0) {
        let head = level(0..24);
        if in_ == 0 {
            assert!(
                head.iter().all(|&p| p > 0.5 * b.amp),
                "{what}: no fade-in expected at the take's start"
            );
        } else {
            assert!(
                head.iter().all(|&p| p < 0.12 * b.amp),
                "{what}: missing fade-in: {head:?}"
            );
            for (i, s) in pcm[..240].iter().enumerate() {
                let limit = b.amp * (i + 1) as f64 / 240.0 + 80.0;
                assert!(
                    s.iter().all(|&v| f64::from(v).abs() <= limit),
                    "{what}: fade-in sample {i} = {s:?}"
                );
            }
            let after = level(240..480);
            assert!(
                after.iter().all(|&p| p > 0.9 * b.amp),
                "{what}: level after the fade-in {after:?}"
            );
        }
    }
    if let Some(b) = playing(len - 600.0, len + 50.0) {
        let n = pcm.len();
        let tail = level(n - 24..n);
        if out == take_end {
            assert!(
                tail.iter().all(|&p| p > 0.5 * b.amp),
                "{what}: no fade-out expected at the take's end: {tail:?}"
            );
        } else {
            assert!(
                tail.iter().all(|&p| p < 0.12 * b.amp),
                "{what}: missing fade-out: {tail:?}"
            );
            for (k, s) in pcm[n - 240..].iter().enumerate() {
                let limit = b.amp * (240 - k) as f64 / 240.0 + 80.0;
                assert!(
                    s.iter().all(|&v| f64::from(v).abs() <= limit),
                    "{what}: fade-out sample {k} = {s:?}"
                );
            }
            let before = level(n - 480..n - 240);
            assert!(
                before.iter().all(|&p| p > 0.9 * b.amp),
                "{what}: level before the fade-out {before:?}"
            );
        }
    }
}

// ------------------------------------------------------- planning, running

fn capabilities() -> Capabilities {
    Capabilities {
        ffmpeg: Some(FfmpegCaps {
            version: "8".to_owned(),
            identity: "ffmpeg (PATH)".to_owned(),
            // Empty: the planner doesn't check encoders it wasn't told about.
            encoders: Default::default(),
        }),
        vaapi: None,
        cores: 4,
    }
}

fn options(format: ExportFormat, codec: VideoCodec, quality: Quality, out: &str) -> ExportOptions {
    ExportOptions {
        format,
        quality,
        codec,
        out: out.to_owned(),
        allow_vaapi: false,
    }
}

/// The plan without the clip ids its segments list (a split adds one).
fn without_clip_ids(mut plan: ExportPlan) -> ExportPlan {
    for segment in &mut plan.segments {
        segment.clips.clear();
    }
    plan
}

/// Runs a segment's argv, checks its counts and progress, and returns its
/// decoded frames (gray) and audio.
fn render_and_check_segment(
    fx: &Fixture,
    s: &SegmentPlan,
    what: &str,
) -> (Vec<Vec<u8>>, Vec<[i16; 2]>) {
    let out = run(fx.path(), &s.argv);
    let blocks = progress_blocks(&out.stdout);
    let last = blocks
        .last()
        .unwrap_or_else(|| panic!("{what}: no -progress output"));
    assert!(
        last.end,
        "{what}: the last progress block isn't progress=end"
    );
    assert_eq!(last.frame, Some(s.frames), "{what}: progress frame count");
    assert!(
        blocks.windows(2).all(|w| w[0].frame <= w[1].frame),
        "{what}: progress went back"
    );
    let counts = segment_counts(fx.path(), &s.path);
    assert_eq!(counts.check(s.frames, s.samples), Ok(()), "{what}");
    let frames = gray_frames(fx.path(), &s.path);
    assert_eq!(frames.len() as u64, s.frames, "{what}: decoded frames");
    let pcm = pcm(fx.path(), &s.path);
    assert_eq!(pcm.len() as u64, s.samples, "{what}: decoded samples");
    (frames, pcm)
}

/// Renders and checks every segment of `plan` (counts, progress, which
/// frames, audio); returns all their frames in timeline order.
fn render_segments(fx: &Fixture, plan: &ExportPlan, label: &str) -> Vec<Vec<u8>> {
    let mut all = Vec::new();
    for (i, s) in plan.segments.iter().enumerate() {
        let what = format!("{label} segment {i} ({} [{}, {}))", s.take, s.in_, s.out);
        let (frames, pcm) = render_and_check_segment(fx, s, &what);
        let take = fx.take(&s.take);
        check_frame_ids(&frames, take, s.in_, fx.fps, &what);
        let unit = (s.in_, s.out, fx.duration_frames(&s.take));
        check_audio(&pcm, take, unit, fx.fps, &what);
        all.extend(frames);
    }
    all
}

/// Writes the concat list and runs the stitch, into `tmp` (the runner may
/// replace only the last argv element) then renamed to the plan's output.
/// Returns its progress blocks and its log (stderr).
fn stitch(fx: &Fixture, plan: &ExportPlan, tmp: &str) -> (Vec<ProgressBlock>, String) {
    fs::write(
        fx.path().join(&plan.stitch.list_path),
        &plan.stitch.list_content,
    )
    .unwrap();
    fs::create_dir_all(fx.path().join("exports")).unwrap();
    let mut argv = plan.stitch.argv.clone();
    assert_eq!(argv.last().unwrap(), &format!("file:{}", plan.out));
    *argv.last_mut().unwrap() = format!("file:{tmp}");
    let out = run(fx.path(), &argv);
    let blocks = progress_blocks(&out.stdout);
    assert!(
        blocks.last().is_some_and(|b| b.end),
        "the stitch didn't end"
    );
    fs::rename(fx.path().join(tmp), fx.path().join(&plan.out)).unwrap();
    (blocks, out.stderr)
}

/// Runs a segment's argv and checks only its counts (no decoding).
fn render_segment_counts(fx: &Fixture, s: &SegmentPlan, what: &str) {
    run(fx.path(), &s.argv);
    let counts = segment_counts(fx.path(), &s.path);
    assert_eq!(counts.check(s.frames, s.samples), Ok(()), "{what}");
}

/// The stitched MP4 holds every segment frame bit-exactly at a constant rate,
/// tagged BT.709, and the AAC audio once (whole 1024-sample frames: the
/// priming is trimmed on decode, the last frame's padding stays).
fn check_stitched_mp4(fx: &Fixture, plan: &ExportPlan, frames: &[Vec<u8>]) {
    let info = probe(fx.path(), &plan.out, StreamKind::Video);
    assert_eq!(info.frames, Some(plan.total_frames));
    assert_eq!(info.fps, Some(fx.fps));
    assert_eq!((info.width, info.height), (Some(W), Some(H)));
    assert_eq!(info.color_range.as_deref(), Some("tv"));
    assert_eq!(info.color_space.as_deref(), Some("bt709"));
    let stitched = gray_frames(fx.path(), &plan.out);
    assert_eq!(stitched.len(), frames.len());
    assert!(stitched == frames, "the stitch changed or reordered frames");
    let pts = packet_pts(fx.path(), &plan.out);
    assert_eq!(pts.len() as u64, plan.total_frames);
    let steps: Vec<i64> = pts.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        steps.iter().all(|&d| d == steps[0]),
        "VFR timestamps: {steps:?}"
    );
    let samples: u64 = plan.segments.iter().map(|s| s.samples).sum();
    let decoded = pcm(fx.path(), &plan.out).len() as u64;
    assert!(
        decoded >= samples && decoded - samples < 1024,
        "the stitched audio has {decoded} samples, the segments {samples}"
    );
}

// ------------------------------------------------------------------ tests

/// The MP4 export of the fixture timeline, through the `Export` command:
/// segments, stitch, job progress and the reply, then a split timeline's
/// identical segments.
#[test]
fn mp4_export_renders_exact_frames_samples_and_fades() {
    let fx = Fixture::new("mp4", FPS30);
    let caps = capabilities();
    let mut state = State::new(
        fx.project.clone(),
        History::new(),
        UiState::default(),
        HostKind::Cli,
    );
    state.runtime.capabilities = caps.clone();
    let ctx = Ctx {
        now: Default::default(),
        utc_offset_secs: 0,
        entropy: 7,
        confirmed: false,
        req: Some(ReqId("r1".to_owned())),
        facts: Facts {
            config: UserConfig::default(),
            ..Facts::default()
        },
    };
    let export = Command::Export {
        format: ExportFormat::Mp4,
        quality: Quality::Standard,
        codec: None,
        out: None,
    };
    let outcome = qrec_core::apply(&state, &ctx, export).expect("export starts");
    let (job, plan) = outcome
        .effects
        .iter()
        .find_map(|e| match e {
            Effect::RenderExport { job, plan } => Some((*job, plan.clone())),
            _ => None,
        })
        .expect("a RenderExport effect");
    assert_eq!(plan.out, "exports/plans-19700101-000000.mp4");
    let opts = options(
        ExportFormat::Mp4,
        VideoCodec::H264,
        Quality::Standard,
        &plan.out,
    );
    assert_eq!(
        plan,
        plan_export(&fx.project, &caps, &opts).unwrap(),
        "the command plans what plan_export plans"
    );
    let units = render_units(&fx.project);
    let ranges: Vec<(TakeId, Frame, Frame)> = units
        .iter()
        .map(|u| (u.take.clone(), u.in_, u.out))
        .collect();
    assert_eq!(
        ranges,
        vec![
            (TakeId::numbered(1), 0, 50),
            (TakeId::numbered(1), 65, 90),
            (TakeId::numbered(1), 12, 20),
            (TakeId::numbered(1), 100, 119),
            (TakeId::numbered(2), 5, 25),
        ]
    );
    assert_eq!(plan.total_frames, 122);
    assert!(plan.segments.iter().all(|s| s.path.ends_with(".mov")));

    // Segments; the job follows the runner's progress.
    let frames = render_segments(&fx, &plan, "mp4");
    let mut state = outcome.state;
    for (i, s) in plan.segments.iter().enumerate() {
        let event = Event::ExportProgress {
            job,
            segment: i,
            frames_done: s.frames,
        };
        state = qrec_core::on_event(&state, &ctx, event).state;
    }
    match &state.runtime.jobs[&job].kind {
        JobKind::Export {
            seg_done,
            seg_frames,
            ..
        } => assert_eq!(seg_done, seg_frames),
        other => panic!("{other:?}"),
    }

    // Colour: the segments are tagged BT.709 and the background was drawn
    // with BT.709 coefficients.
    let first = &plan.segments[0].path;
    #[rustfmt::skip]
    let tags = run(fx.path(), &args(&[
        "ffprobe", "-v", "error", "-select_streams", "v:0",
        "-show_entries", "stream=color_range,color_space,color_primaries,color_transfer",
        "-of", "csv=p=0", &format!("file:{first}"),
    ]));
    assert_eq!(
        String::from_utf8_lossy(&tags.stdout).trim(),
        "tv,bt709,bt709,bt709"
    );
    let rgb = rgb_frame(
        fx.path(),
        first,
        "scale=in_color_matrix=bt709:in_range=tv,format=rgb24",
    );
    assert_close_rgb(
        pixel(&rgb, W / 2, 55),
        BACKGROUND_RGB,
        4,
        "segment background",
    );

    // The stitch, its progress through the job, and the stitched file.
    let (blocks, _) = stitch(&fx, &plan, "exports/.plans.qrec-part-1-1.mp4");
    for block in &blocks {
        let event = Event::ExportStitchProgress {
            job,
            out_time_ns: block.out_time_ns.unwrap_or(0).max(0) as u64,
        };
        state = qrec_core::on_event(&state, &ctx, event).state;
    }
    match &state.runtime.jobs[&job].kind {
        JobKind::Export {
            stitching,
            stitch_done_ns,
            stitch_total_ns,
            ..
        } => {
            // ffmpeg's last `out_time` may trail the end by a few frames.
            assert!(*stitching);
            assert!(
                stitch_total_ns - stitch_done_ns <= frame_ns(3, fx.fps) as u64,
                "the stitch's progress stopped at {stitch_done_ns} of {stitch_total_ns} ns"
            );
        }
        other => panic!("{other:?}"),
    }
    check_stitched_mp4(&fx, &plan, &frames);

    // The job ends with the reply the requester waits for.
    let bytes = fs::metadata(fx.path().join(&plan.out)).unwrap().len();
    let real = fx.path().join(&plan.out).display().to_string();
    let finished = qrec_core::on_event(
        &state,
        &ctx,
        Event::ExportFinished {
            job,
            path: real.clone(),
            bytes,
        },
    );
    assert!(finished.state.runtime.jobs.is_empty());
    let reply = finished
        .effects
        .iter()
        .find_map(|e| match e {
            Effect::Reply { reply, .. } => Some(reply.clone()),
            _ => None,
        })
        .unwrap()
        .unwrap();
    assert_eq!(reply.result["path"], real.as_str());
    assert_eq!(reply.result["frames"], plan.total_frames);
    assert_eq!(reply.result["bytes"], bytes);

    // A split timeline: the same plan, and byte-identical segments.
    let mut split = fx.project.clone();
    let cut = |clips: &mut Vec<Clip>, id: &str, at: Frame, new: &str| {
        let i = clips.iter().position(|c| c.id.as_str() == id).unwrap();
        *clips = qrec_core::timeline::split(clips, i, at, ClipId::from_raw(new)).unwrap();
    };
    cut(&mut split.timeline.clips, "c1", 17, "c1b");
    cut(&mut split.timeline.clips, "c3", 80, "c3b");
    cut(&mut split.timeline.clips, "c6", 6, "c6b");
    let split_plan = plan_export(&split, &caps, &opts).unwrap();
    assert_eq!(
        without_clip_ids(split_plan.clone()),
        without_clip_ids(plan.clone())
    );
    let first_render: BTreeMap<String, Vec<u8>> = plan
        .segments
        .iter()
        .map(|s| (s.path.clone(), fs::read(fx.path().join(&s.path)).unwrap()))
        .collect();
    for s in &plan.segments {
        let _ = fs::remove_file(fx.path().join(&s.path));
    }
    for s in &split_plan.segments {
        if !fx.path().join(&s.path).exists() {
            run(fx.path(), &s.argv);
        }
        assert!(
            fs::read(fx.path().join(&s.path)).unwrap() == first_render[&s.path],
            "{}: the split timeline rendered different bytes",
            s.path
        );
    }
}

/// At 30000/1001 fps the Matroska timestamps (whole ms) never match frame
/// times and a unit's samples aren't whole per frame (1601.6): the 2 ms
/// seek back-off, the in-graph trims and the stitch must still be exact.
#[test]
fn ntsc_export_renders_exact_frames_and_samples() {
    let fx = Fixture::new("ntsc", NTSC);
    let opts = options(
        ExportFormat::Mp4,
        VideoCodec::H264,
        Quality::Draft,
        "exports/plans-ntsc.mp4",
    );
    let plan = plan_export(&fx.project, &capabilities(), &opts).unwrap();
    // 50 frames at 29.97 are 80 080 samples; 25 are 40 040.
    let samples: Vec<u64> = plan.segments.iter().map(|s| s.samples).collect();
    assert_eq!(samples, vec![80_080, 40_040, 12_813, 30_430, 32_032]);
    let frames = render_segments(&fx, &plan, "ntsc");
    stitch(&fx, &plan, "exports/.plans-ntsc.qrec-part-1-2.mp4");
    check_stitched_mp4(&fx, &plan, &frames);
}

/// WebM: VP9 and AV1 segments are Matroska with PCM, exact counts; the
/// stitch copies the video and encodes Opus.
#[test]
fn webm_exports_render_exact_frames_and_samples() {
    let fx = Fixture::new("webm", FPS30);
    let caps = capabilities();
    // AV1 at Draft (crf 38) smears the 40 px bars at 320×180 enough to
    // misread them; High keeps them legible.
    for (codec, quality) in [
        (VideoCodec::Vp9, Quality::Draft),
        (VideoCodec::Av1, Quality::High),
    ] {
        let out = format!("exports/plans-{codec:?}.webm");
        let plan = plan_export(
            &fx.project,
            &caps,
            &options(ExportFormat::Webm, codec, quality, &out),
        )
        .unwrap();
        assert_eq!(plan.tmp_ext, "webm");
        assert!(plan.segments.iter().all(|s| s.path.ends_with(".mkv")));
        render_segments(&fx, &plan, &format!("{codec:?}"));
        stitch(
            &fx,
            &plan,
            &format!("exports/.plans-{codec:?}.qrec-part-1-3.webm"),
        );
        let info = probe(fx.path(), &plan.out, StreamKind::Video);
        assert_eq!(info.frames, Some(plan.total_frames), "{codec:?}");
        assert_eq!((info.width, info.height), (Some(W), Some(H)));
        // Opus in WebM trims its pre-skip and end padding exactly.
        let samples: u64 = plan.segments.iter().map(|s| s.samples).sum();
        let decoded = pcm(fx.path(), &plan.out).len() as u64;
        assert_eq!(decoded, samples, "{codec:?}: the stitched audio's samples");
    }
}

/// Many units whose Matroska segments round their duration the same way (2
/// frames = 66.67 ms at 30 fps, 66.73 ms at 29.97, both stored as 67 ms):
/// the stitch must still start every unit at its timeline time, so each
/// WebM frame sits on the format's millisecond grid around its own time
/// however many units came before it, and the Opus packets follow each
/// other without jumps (A§9.3 stitch, SPEC §13.1).
#[test]
fn webm_units_stay_on_time_however_many() {
    for (tag, fps) in [("webm-drift", FPS30), ("webm-drift-ntsc", NTSC)] {
        let mut fx = Fixture::new(tag, fps);
        // 36 units of 2 frames from take 2, a frame apart (not continuous).
        fx.project.timeline.clips = (0..36u64)
            .map(|i| Clip {
                id: ClipId::from_raw(format!("d{i}")),
                take: TakeId::numbered(2),
                in_: 3 * i,
                out: 3 * i + 2,
            })
            .collect();
        let out = format!("exports/{tag}.webm");
        let opts = options(ExportFormat::Webm, VideoCodec::Vp9, Quality::Draft, &out);
        let plan = plan_export(&fx.project, &capabilities(), &opts).unwrap();
        assert_eq!((plan.segments.len(), plan.total_frames), (36, 72));
        for (i, s) in plan.segments.iter().enumerate() {
            render_segment_counts(&fx, s, &format!("{tag} segment {i}"));
        }
        let (_, log) = stitch(&fx, &plan, &format!("exports/.{tag}.qrec-part-1-4.webm"));
        assert!(!log.contains("Non-monotonic"), "{tag}: {log}");

        let video = packet_times(fx.path(), &plan.out, "v:0");
        assert_eq!(video.len() as u64, plan.total_frames, "{tag}");
        for (i, t) in video.iter().enumerate() {
            // Timeline frame i, in ms (WebM stores whole milliseconds).
            let exact = i as f64 * 1000.0 * f64::from(fps.den) / f64::from(fps.num);
            assert!(
                (t * 1000.0 - exact).abs() <= 1.0 + 1e-6,
                "{tag}: frame {i} is at {:.3} ms, {exact:.3} ms on the timeline",
                t * 1000.0
            );
        }
        // 20 ms Opus packets, after the first one (stamped −7 ms: its
        // 6.5 ms pre-skip rounded to the millisecond).
        let audio = packet_times(fx.path(), &plan.out, "a:0");
        let steps: Vec<f64> = audio.windows(2).map(|w| (w[1] - w[0]) * 1000.0).collect();
        assert!(
            steps[1..].iter().all(|d| (d - 20.0).abs() < 1e-6),
            "{tag}: Opus packets jump: {steps:?}"
        );
        let samples: u64 = plan.segments.iter().map(|s| s.samples).sum();
        assert_eq!(pcm(fx.path(), &plan.out).len() as u64, samples, "{tag}");
    }
}

/// Units of one and two frames first, in the middle and last: H.264
/// segments without B-frames all start their decode order at their first
/// frame, so the stitched MP4's DTS rise evenly (ffmpeg bumps none of them),
/// and its video track lasts exactly the timeline at the canvas rate.
#[test]
fn mp4_units_of_one_and_two_frames_keep_even_timestamps() {
    let mut fx = Fixture::new("short-units", FPS30);
    let units = [
        (0, 1),
        (10, 40),
        (41, 43),
        (50, 80),
        (82, 83),
        (90, 110),
        (115, 116),
    ];
    fx.project.timeline.clips = units
        .iter()
        .enumerate()
        .map(|(i, &(in_, out))| Clip {
            id: ClipId::from_raw(format!("s{i}")),
            take: TakeId::numbered(2),
            in_,
            out,
        })
        .collect();
    let opts = options(
        ExportFormat::Mp4,
        VideoCodec::H264,
        Quality::Draft,
        "exports/short-units.mp4",
    );
    let plan = plan_export(&fx.project, &capabilities(), &opts).unwrap();
    assert_eq!(plan.total_frames, 1 + 30 + 2 + 30 + 1 + 20 + 1);
    let frames = render_segments(&fx, &plan, "short units");
    let (_, log) = stitch(&fx, &plan, "exports/.short-units.qrec-part-1-5.mp4");
    assert!(!log.contains("Non-monotonic"), "{log}");
    check_stitched_mp4(&fx, &plan, &frames);

    let dts: Vec<i64> = packet_entries(fx.path(), &plan.out, "v:0", "dts")
        .iter()
        .map(|d| d.parse().unwrap())
        .collect();
    assert_eq!(dts.len() as u64, plan.total_frames);
    let steps: Vec<i64> = dts.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        steps.iter().all(|&d| d > 0 && d == steps[0]),
        "uneven DTS: {steps:?}"
    );
    #[rustfmt::skip]
    let stream = run(fx.path(), &args(&[
        "ffprobe", "-v", "error", "-select_streams", "v:0",
        "-show_entries", "stream=duration,avg_frame_rate", "-of", "default=nw=1",
        &format!("file:{}", plan.out),
    ]));
    let stream = String::from_utf8(stream.stdout).unwrap();
    let field = |key: &str| {
        stream
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("no {key} in {stream}"))
            .to_owned()
    };
    assert_eq!(
        field("avg_frame_rate"),
        "30/1",
        "the sample table's average rate"
    );
    let duration: f64 = field("duration").parse().unwrap();
    let exact = plan.total_frames as f64 / 30.0;
    assert!(
        (duration - exact).abs() < 1e-4,
        "the video track lasts {duration} s, the timeline {exact} s"
    );
}

/// Frame saves go through the export graph as one-frame units: the same
/// recorded frames, sRGB PNGs, BT.709 colours; `--layer` is the raw frame.
#[test]
fn frame_saves_are_srgb_pngs_of_the_export_frames() {
    let fx = Fixture::new("frames", FPS30);
    fs::create_dir_all(fx.path().join("frames")).unwrap();
    // Timeline 0: L2 hasn't started (lead); 1; 53 = take frame 68; 120 =
    // the second take's frame 23.
    for at in [0, 1, 53, 120] {
        let out = format!("frames/at-{at}.png");
        let plan = plan_frame(&fx.project, at, None, &out).unwrap();
        let result = run(fx.path(), &plan.argv);
        assert!(progress_blocks(&result.stdout).last().unwrap().end);
        let png = fs::read(fx.path().join(&out)).unwrap();
        check_png(&png, W, H, &out);
        let (take, f) = qrec_core::timeline::source_at(&fx.project.timeline.clips, at).unwrap();
        let gray = gray_frames(fx.path(), &out);
        assert_eq!(gray.len(), 1);
        check_frame_ids(&gray, fx.take(take), f, fx.fps, &out);
        if *take == TakeId::numbered(1) {
            // The background between the bands, a bar that is off (bit 5 of
            // frames 0, 1 and 68) and the marker bar, full range.
            let rgb = rgb_frame(fx.path(), &out, "format=rgb24");
            assert_close_rgb(pixel(&rgb, W / 2, 55), BACKGROUND_RGB, 3, &out);
            let off_bar = pixel(&rgb, 5 * BAR_PX + BAR_PX / 2, 25);
            assert_close_rgb(off_bar, [0, 0, 0], 3, &out);
            let marker = pixel(&rgb, W - BAR_PX / 2, 25);
            assert_close_rgb(marker, [255, 255, 255], 3, &out);
        }
    }
    // Raw layer frames, at native size: L2 (41 ms late) at timeline 53 shows
    // its frame 67; L3 (20 ms early) at timeline 0 its frame 1.
    for (at, layer, want) in [(53, "L2", 67), (0, "L3", 1), (120, "L1", 23)] {
        let out = format!("frames/raw-{layer}-{at}.png");
        let layer: LayerId = layer.parse().unwrap();
        let plan = plan_frame(&fx.project, at, Some(&layer), &out).unwrap();
        run(fx.path(), &plan.argv);
        let png = fs::read(fx.path().join(&out)).unwrap();
        check_png(&png, W, H, &out);
        let gray = gray_frames(fx.path(), &out);
        assert_eq!(read_id(&gray[0], H / 2), Some(want), "{out}");
    }
    let l2: LayerId = "L2".parse().unwrap();
    let err = plan_frame(&fx.project, 0, Some(&l2), "frames/x.png").unwrap_err();
    assert!(
        err.message.contains("recording starts later"),
        "{}",
        err.message
    );
}

/// Takes recorded at another rate than the canvas's (A§9.3): every take
/// frame shows the recorded frame nearest to its time, the earlier one on a
/// tie, whatever unit it is in and wherever that unit starts (trimmed by a
/// frame, one frame long, over an earlier range), and a saved frame, plain
/// or raw, is the export's frame exactly (SPEC §13.2). 25 fps on 30, 30 on
/// 60 (the `project set` fps change), 50 on 30 and 29.97 on 25.
#[test]
fn other_recorded_rates_show_the_nearest_frame_everywhere() {
    for (tag, recorded, canvas) in [
        ("rates-25-30", Fps::new(25, 1), FPS30),
        ("rates-30-60", FPS30, Fps::new(60, 1)),
        ("rates-50-30", Fps::new(50, 1), FPS30),
        ("rates-ntsc-25", NTSC, Fps::new(25, 1)),
    ] {
        let rates = (recorded, canvas);
        let mut fx = mixed_rate_fixture(tag, recorded, canvas);
        let take = TakeId::numbered(1);
        let d = fx.duration_frames(&take);
        let units = [
            (0, 13),
            (14, 31),
            (d / 2 + 1, d / 2 + 9),
            (3, 12),
            (d / 3, d / 3 + 1),
            (d - 10, d),
        ];
        fx.project.timeline.clips = units
            .iter()
            .enumerate()
            .map(|(i, &(in_, out))| Clip {
                id: ClipId::from_raw(format!("m{i}")),
                take: take.clone(),
                in_,
                out,
            })
            .collect();
        let out = format!("exports/{tag}.mp4");
        let opts = options(ExportFormat::Mp4, VideoCodec::H264, Quality::Draft, &out);
        let plan = plan_export(&fx.project, &capabilities(), &opts).unwrap();
        assert_eq!(plan.segments.len(), units.len(), "{tag}");
        let take_fx = &fx.takes[0];
        let mut export = Vec::new();
        for (i, s) in plan.segments.iter().enumerate() {
            let what = format!("{tag} segment {i} ([{}, {}))", s.in_, s.out);
            run(fx.path(), &s.argv);
            let frames = gray_frames(fx.path(), &s.path);
            assert_eq!(frames.len() as u64, s.frames, "{what}");
            check_rate_rule(&frames, take_fx, rates, s.in_, &what);
            export.extend(frames);
        }
        // Saved frames: the export's, at every kind of timeline frame.
        fs::create_dir_all(fx.path().join("frames")).unwrap();
        let total = plan.total_frames;
        for at in [0, 13, 14, 15, 16, total / 2, total - 1] {
            let out = format!("frames/{tag}-{at}.png");
            let frame = plan_frame(&fx.project, at, None, &out).unwrap();
            run(fx.path(), &frame.argv);
            let gray = gray_frames(fx.path(), &out);
            let (_, f) = qrec_core::timeline::source_at(&fx.project.timeline.clips, at).unwrap();
            check_rate_rule(&gray, take_fx, rates, f, &out);
            for layer in &take_fx.layers {
                assert_eq!(
                    read_id(&gray[0], layer.probe_y),
                    read_id(&export[at as usize], layer.probe_y),
                    "{out}: layer {} isn't the export's frame",
                    layer.id
                );
                // The raw layer frame is the same recorded frame (when the
                // layer has one there).
                let expected = expected_at_rate(recorded, layer.offset_ns, canvas, f);
                let Some(want) = expected.filter(|_| at == 15 || at == total / 2) else {
                    continue;
                };
                let raw = format!("frames/{tag}-{at}-{}.png", layer.id);
                let id: LayerId = layer.id.parse().unwrap();
                let plan = plan_frame(&fx.project, at, Some(&id), &raw).unwrap();
                run(fx.path(), &plan.argv);
                let raw_gray = gray_frames(fx.path(), &raw);
                assert_eq!(read_id(&raw_gray[0], H / 2), Some(want), "{raw}");
            }
        }
    }
}

// ------------------------------------------------------------------ tools

/// A temp project dir; removed when the test passes, kept when it fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "qrec-plans-ffmpeg-{}-{tag}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join(SEGMENTS_DIR)).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!("kept {} for inspection", self.0.display());
        } else {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

struct ToolOutput {
    stdout: Vec<u8>,
    stderr: String,
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

/// Runs `argv` (`argv[0]` is `ffmpeg` or `ffprobe`) in `cwd` with a
/// timeout; stdout and stderr go to files, so no pipe can fill up.
fn run(cwd: &Path, argv: &[String]) -> ToolOutput {
    static N: AtomicU64 = AtomicU64::new(0);
    let logs = cwd.join(".qrec/logs");
    fs::create_dir_all(&logs).unwrap();
    let n = N.fetch_add(1, Ordering::Relaxed);
    let (out_path, err_path) = (logs.join(format!("{n}.out")), logs.join(format!("{n}.err")));
    let mut child = Process::new(&argv[0])
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(File::create(&out_path).unwrap())
        .stderr(File::create(&err_path).unwrap())
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "can't run {}: {e} (run the tests in the Nix dev shell)",
                argv[0]
            )
        });
    let deadline = Instant::now() + TOOL_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{argv:?} didn't finish within {TOOL_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(5));
    };
    let stderr = fs::read_to_string(&err_path).unwrap_or_default();
    assert!(status.success(), "{argv:?} failed ({status}):\n{stderr}");
    ToolOutput {
        stdout: fs::read(&out_path).unwrap(),
        stderr,
    }
}

fn ffmpeg(cwd: &Path, list: &[&str]) -> Vec<u8> {
    let mut argv = args(&[
        "ffmpeg",
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-loglevel",
        "error",
        "-y",
    ]);
    argv.extend(args(list));
    run(cwd, &argv).stdout
}

fn probe(cwd: &Path, file: &str, kind: StreamKind) -> MediaInfo {
    let out = run(cwd, &probe_argv(file, kind));
    parse_ffprobe(&String::from_utf8(out.stdout).unwrap(), kind)
        .unwrap_or_else(|e| panic!("{file}: {e}"))
}

fn segment_counts(cwd: &Path, path: &str) -> SegmentCounts {
    let out = run(cwd, &segment_check_argv(path));
    parse_segment_check(&String::from_utf8(out.stdout).unwrap()).unwrap()
}

fn progress_blocks(stdout: &[u8]) -> Vec<ProgressBlock> {
    let mut parser = ProgressParser::new();
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| parser.feed(line))
        .collect()
}

/// Every video frame as 8-bit gray (W×H bytes each).
fn gray_frames(cwd: &Path, file: &str) -> Vec<Vec<u8>> {
    #[rustfmt::skip]
    let raw = ffmpeg(cwd, &[
        "-i", &format!("file:{file}"), "-map", "0:v:0",
        "-f", "rawvideo", "-pix_fmt", "gray", "-",
    ]);
    let size = (W * H) as usize;
    assert_eq!(raw.len() % size, 0, "{file}: partial frame");
    raw.chunks(size).map(<[u8]>::to_vec).collect()
}

/// The first video frame as RGB through `filter`.
fn rgb_frame(cwd: &Path, file: &str, filter: &str) -> Vec<u8> {
    #[rustfmt::skip]
    let rgb = ffmpeg(cwd, &[
        "-i", &format!("file:{file}"), "-map", "0:v:0", "-frames:v", "1", "-vf", filter,
        "-f", "rawvideo", "-pix_fmt", "rgb24", "-",
    ]);
    rgb
}

fn pixel(rgb: &[u8], x: u32, y: u32) -> [u8; 3] {
    let at = ((y * W + x) * 3) as usize;
    [rgb[at], rgb[at + 1], rgb[at + 2]]
}

fn assert_close_rgb(got: [u8; 3], want: [u8; 3], tolerance: u8, what: &str) {
    assert!(
        got.iter()
            .zip(want)
            .all(|(g, w)| g.abs_diff(w) <= tolerance),
        "{what}: colour {got:?}, expected {want:?} ± {tolerance}"
    );
}

/// The audio as interleaved stereo s16 at 48 kHz.
fn pcm(cwd: &Path, file: &str) -> Vec<[i16; 2]> {
    #[rustfmt::skip]
    let raw = ffmpeg(cwd, &[
        "-i", &format!("file:{file}"), "-map", "0:a:0",
        "-c:a", "pcm_s16le", "-ac", "2", "-ar", "48000", "-f", "s16le", "-",
    ]);
    raw.chunks_exact(4)
        .map(|c| {
            [
                i16::from_le_bytes([c[0], c[1]]),
                i16::from_le_bytes([c[2], c[3]]),
            ]
        })
        .collect()
}

/// One field of every packet of a stream (`v:0`, `a:0`), in file order.
fn packet_entries(cwd: &Path, file: &str, stream: &str, entry: &str) -> Vec<String> {
    #[rustfmt::skip]
    let out = run(cwd, &args(&[
        "ffprobe", "-v", "error", "-select_streams", stream,
        "-show_entries", &format!("packet={entry}"), "-of", "csv=p=0", &format!("file:{file}"),
    ]));
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim().trim_end_matches(',').to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// A stream's packet times in seconds, in presentation order.
fn packet_times(cwd: &Path, file: &str, stream: &str) -> Vec<f64> {
    let mut times: Vec<f64> = packet_entries(cwd, file, stream, "pts_time")
        .iter()
        .map(|t| t.parse().unwrap())
        .collect();
    times.sort_by(f64::total_cmp);
    times
}

/// The video packets' timestamps, in presentation order.
fn packet_pts(cwd: &Path, file: &str) -> Vec<i64> {
    #[rustfmt::skip]
    let out = run(cwd, &args(&[
        "ffprobe", "-v", "error", "-select_streams", "v:0",
        "-show_entries", "packet=pts", "-of", "csv=p=0", &format!("file:{file}"),
    ]));
    let mut pts: Vec<i64> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim().trim_end_matches(',').parse().unwrap())
        .collect();
    pts.sort_unstable();
    pts
}

/// An 8-bit RGB PNG of `w`×`h`, tagged sRGB (an `sRGB` chunk, or `cHRM` +
/// `gAMA`), without a `cICP` chunk.
fn check_png(png: &[u8], w: u32, h: u32, what: &str) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{what}: not a PNG");
    let mut chunks = Vec::new();
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        let kind = String::from_utf8_lossy(&png[at + 4..at + 8]).into_owned();
        if kind == "IHDR" {
            let data = &png[at + 8..at + 8 + len];
            let size = (
                u32::from_be_bytes(data[0..4].try_into().unwrap()),
                u32::from_be_bytes(data[4..8].try_into().unwrap()),
            );
            assert_eq!(size, (w, h), "{what}: size");
            assert_eq!((data[8], data[9]), (8, 2), "{what}: 8-bit RGB");
        }
        chunks.push(kind);
        at += 12 + len;
    }
    let has = |name: &str| chunks.iter().any(|c| c == name);
    assert!(
        has("sRGB") || (has("cHRM") && has("gAMA")),
        "{what}: no sRGB tags in {chunks:?}"
    );
    assert!(!has("cICP"), "{what}: a cICP chunk in {chunks:?}");
}
