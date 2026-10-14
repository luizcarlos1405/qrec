//! The export planner (A§9.3): render units, ffmpeg filter graphs, cache
//! keys and the stitch step. Owner: output.
//!
//! An export renders each *render unit* (a maximal run of continuous clips)
//! to a cached segment, then joins the segments with the concat demuxer and
//! encodes the audio once. Every rule marked *(measured)* was checked with
//! ffmpeg 8.1.2 and must not be "simplified" away:
//!
//! - **Frame selection:** a layer input seeks to `frame_ns_rec(k0) − 2 ms`
//!   (the 2 ms absorb Matroska's millisecond timestamps), `fps=<canvas>`
//!   makes it constant-rate and `setpts=PTS-STARTPTS+<lead>` puts its first
//!   frame `lead` canvas frames into the unit.
//! - **Lengths** are enforced in the graph (`trim=end_frame`,
//!   `atrim=end_sample`), never with `-frames:v`/`-t`, which cut the audio
//!   short at 29.97 fps.
//! - **Colour:** `-color_range tv -colorspace bt709` drive the graph's
//!   negotiation (the `color` background is drawn with BT.709 coefficients);
//!   primaries and transfer are tagged in the graph (`media_argv::TAG_BT709`)
//!   because ffmpeg 8.1.2 takes them from the frames, not from the options.
//!
//! [`frame_plan`](crate::frame_plan) and [`ges_plan`](crate::ges_plan) use
//! the same units, layer inputs and frame selection.

use std::collections::BTreeSet;

use crate::error::{CoreError, ErrorCode};
use crate::hash::sha256_hex;
use crate::ids::{ClipId, Frame, TakeId};
use crate::layout::{Placement, place};
use crate::media_argv::{
    TAG_BT709, VAAPI_UPLOAD, color_args, ffmpeg_prelude, file_arg, number, output_args,
    seconds_arg, vaapi_device_args, vaapi_h264_args,
};
use crate::model::{AudioTrack, Canvas, Fps, Layer, Project, Stream, StreamKind, Take};
use crate::plans::{ExportPlan, SegmentPlan, StitchPlan};
use crate::protocol::command::{ExportFormat, Quality, VideoCodec};
use crate::state::{Capabilities, FfmpegCaps};
use crate::time::{NS_PER_MS, NS_PER_SEC, frame_ns, ns_to_frame_ceil, ns_to_frame_round};

/// Bumped whenever generated argv change meaning (part of every cache key).
pub const PLAN_VERSION: u32 = 1;

/// The audio rate of segments and exports.
pub const SAMPLE_RATE: u64 = 48_000;

/// Boundary fades: 240 samples = 5 ms at 48 kHz (SPEC §9.5).
pub const FADE_SAMPLES: u64 = 240;

/// How far before a frame's timestamp a layer input seeks *(measured)*.
pub const SEEK_BACKOFF_NS: i64 = 2 * NS_PER_MS;

/// Where segments are cached (project-relative).
pub const SEGMENTS_DIR: &str = ".qrec/cache/segments";

/// What the user asked for, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub quality: Quality,
    /// Already checked against the format (mp4 ⇒ h264; webm ⇒ vp9 | av1).
    pub codec: VideoCodec,
    /// The output path: absolute when picked by hand, project-relative
    /// (`exports/…`) for auto names.
    pub out: String,
    /// Use the VA export chain when `caps.vaapi.ffmpeg_h264`; false re-plans
    /// with libx264 after a VA failure.
    pub allow_vaapi: bool,
}

/// A maximal run of continuous clips (same take, `out == next.in`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderUnit {
    pub clips: Vec<ClipId>,
    pub take: TakeId,
    pub in_: Frame,
    pub out: Frame,
    /// Timeline position of the unit's first frame.
    pub position: Frame,
}

impl RenderUnit {
    /// The unit's length in frames.
    pub fn frames(&self) -> Frame {
        self.out - self.in_
    }
}

/// The render units of the timeline. Property: a split never changes them
/// (except for the clip ids they list). Empty clips are skipped.
pub fn render_units(project: &Project) -> Vec<RenderUnit> {
    let mut units: Vec<RenderUnit> = Vec::new();
    let mut position = 0;
    for clip in &project.timeline.clips {
        if clip.is_empty() {
            continue;
        }
        match units.last_mut() {
            Some(unit) if unit.take == clip.take && unit.out == clip.in_ => {
                unit.out = clip.out;
                unit.clips.push(clip.id.clone());
            }
            _ => units.push(RenderUnit {
                clips: vec![clip.id.clone()],
                take: clip.take.clone(),
                in_: clip.in_,
                out: clip.out,
                position,
            }),
        }
        position += clip.len();
    }
    units
}

/// mp4 ⇒ h264 only; webm ⇒ vp9 (the default) or av1; anything else is `Usage`.
pub fn resolve_codec(
    format: ExportFormat,
    codec: Option<VideoCodec>,
) -> Result<VideoCodec, CoreError> {
    match (format, codec) {
        (ExportFormat::Mp4, None | Some(VideoCodec::H264)) => Ok(VideoCodec::H264),
        (ExportFormat::Webm, None | Some(VideoCodec::Vp9)) => Ok(VideoCodec::Vp9),
        (ExportFormat::Webm, Some(VideoCodec::Av1)) => Ok(VideoCodec::Av1),
        (ExportFormat::Mp4, Some(_)) => Err(CoreError::usage("mp4 exports are H.264")
            .with_hint("use --format webm for --codec vp9 or av1")),
        (ExportFormat::Webm, Some(VideoCodec::H264)) => {
            Err(CoreError::usage("webm exports are VP9 or AV1")
                .with_hint("use --format mp4 for --codec h264"))
        }
    }
}

// --------------------------------------------------------- frame selection

/// Which recorded frame a layer starts a unit with, and how many canvas
/// frames of the unit pass before it appears (A§9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSelection {
    /// The first recorded frame shown: an index into the stream.
    pub k0: u64,
    /// Canvas frames at the start of the unit before the layer's first
    /// frame (the stream started after the unit's start).
    pub lead: u64,
}

impl FrameSelection {
    /// The input seek `frame_ns_rec(k0) − 2 ms`; `None` when that is ≤ 0.
    pub fn seek_ns(&self, recorded: Fps) -> Option<i64> {
        Some(frame_ns(self.k0 as i64, recorded) - SEEK_BACKOFF_NS).filter(|&ns| ns > 0)
    }
}

/// The frame selection of `stream` for a unit starting at take frame `in_`
/// *(measured)*.
///
/// At the canvas rate, with `off = ns_to_frame_round(startOffsetNs)`:
/// `k = in − off`, `lead = max(0, −k)`, `k0 = max(0, k)`.
///
/// At another recorded rate, a canvas frame `t` shows the recorded frame
/// nearest to its time, `ns_to_frame_round(frame_ns(t) − startOffsetNs)`.
/// When that is negative at `in`, `lead` counts the *canvas* frames before
/// the first one that has a recorded frame, and `k0` is that frame's (the
/// canvas-rate formula `lead = −k` would count recorded frames).
pub fn select_frames(stream: &Stream, canvas_fps: Fps, in_: Frame) -> FrameSelection {
    let recorded = stream.fps.unwrap_or(canvas_fps);
    let offset = stream.start_offset_ns;
    let in_ = in_ as i64;
    if same_rate(recorded, canvas_fps) {
        let k = in_ - ns_to_frame_round(offset, canvas_fps);
        return FrameSelection {
            k0: k.max(0) as u64,
            lead: (-k).max(0) as u64,
        };
    }
    let nearest = |t: i64| ns_to_frame_round(frame_ns(t, canvas_fps) - offset, recorded);
    let k = nearest(in_);
    if k >= 0 {
        return FrameSelection {
            k0: k as u64,
            lead: 0,
        };
    }
    // A time x rounds to recorded frame 0 or later once x ≥ −⌊D/2⌋, with D
    // the length of recorded frame −1.
    let half = -frame_ns(-1, recorded) / 2;
    let first = ns_to_frame_ceil(offset - half, canvas_fps).max(in_ + 1);
    FrameSelection {
        k0: nearest(first).max(0) as u64,
        lead: (first - in_) as u64,
    }
}

/// `a` and `b` are the same rate (`60/2` is `30/1`; ffprobe reduces its
/// rates, a canvas rate may not be).
fn same_rate(a: Fps, b: Fps) -> bool {
    u64::from(a.num) * u64::from(b.den) == u64::from(b.num) * u64::from(a.den)
}

// ----------------------------------------------------------------- inputs

/// A layer drawn in a unit: visible, not missing, opacity > 0, recorded with
/// a size, placed on the canvas, and showing at least one frame.
#[derive(Debug, Clone)]
pub(crate) struct LayerInput<'a> {
    pub layer: &'a Layer,
    /// Index in the take scene (z-order, 0 = bottom).
    pub index: usize,
    pub stream: &'a Stream,
    pub placement: Placement,
    pub selection: FrameSelection,
    /// The stream's recorded rate (the canvas rate when unknown).
    pub recorded: Fps,
}

/// The layers of `take` drawn in the unit `[in_, in_ + n)`, bottom first.
pub(crate) fn layer_inputs<'a>(
    take: &'a Take,
    canvas: &Canvas,
    in_: Frame,
    n: Frame,
) -> Vec<LayerInput<'a>> {
    take.scene
        .layers
        .iter()
        .enumerate()
        .filter_map(|(index, layer)| {
            if !layer.visible || layer.missing || layer.opacity.is_nan() || layer.opacity <= 0.0 {
                return None;
            }
            let stream = take
                .stream(layer.id.as_str())
                .filter(|s| s.kind == StreamKind::Video)?;
            let placement = place(
                stream.width?,
                stream.height?,
                layer.crop,
                layer.rect,
                layer.fit,
            )?;
            if !on_canvas(&placement, canvas) {
                return None;
            }
            let selection = select_frames(stream, canvas.fps, in_);
            if selection.lead >= n {
                return None;
            }
            Some(LayerInput {
                layer,
                index,
                stream,
                placement,
                selection,
                recorded: stream.fps.unwrap_or(canvas.fps),
            })
        })
        .collect()
}

fn on_canvas(p: &Placement, canvas: &Canvas) -> bool {
    let (x, y) = (i64::from(p.dst.x), i64::from(p.dst.y));
    x < i64::from(canvas.width)
        && y < i64::from(canvas.height)
        && x + i64::from(p.dst.w) > 0
        && y + i64::from(p.dst.h) > 0
}

/// An audible track in a unit: unmuted, not missing, recorded.
#[derive(Debug, Clone)]
pub(crate) struct TrackInput<'a> {
    pub track: &'a AudioTrack,
    pub stream: &'a Stream,
    /// The file time at the unit's start:
    /// `frame_ns(in) − startOffsetNs − syncOffsetMs·10⁶` (negative: the
    /// audio starts after the unit does).
    pub file_ns: i64,
}

/// The tracks of `take` heard in a unit starting at take frame `in_`.
pub(crate) fn track_inputs<'a>(take: &'a Take, canvas_fps: Fps, in_: Frame) -> Vec<TrackInput<'a>> {
    take.scene
        .audio
        .iter()
        .filter_map(|track| {
            if track.muted || track.missing {
                return None;
            }
            let stream = take
                .stream(track.id.as_str())
                .filter(|s| s.kind == StreamKind::Audio)?;
            let file_ns = frame_ns(in_ as i64, canvas_fps)
                - stream.start_offset_ns
                - i64::from(track.sync_offset_ms) * NS_PER_MS;
            Some(TrackInput {
                track,
                stream,
                file_ns,
            })
        })
        .collect()
}

/// `round(n·48000·den/num)`: the samples of `n` canvas frames.
pub fn unit_samples(n: Frame, fps: Fps) -> u64 {
    let num = u128::from(n) * u128::from(SAMPLE_RATE) * u128::from(fps.den);
    let den = u128::from(fps.num);
    ((2 * num + den) / (2 * den)) as u64
}

/// `round(ns·48000/10⁹)`.
fn ns_to_samples(ns: i64) -> u64 {
    let num = u128::from(ns.unsigned_abs()) * u128::from(SAMPLE_RATE);
    let den = NS_PER_SEC as u128;
    ((2 * num + den) / (2 * den)) as u64
}

// ------------------------------------------------------------------ graphs

/// The video half of a unit's ffmpeg command: inputs and filter chains up
/// to the composited picture (`[<label>]`), before trimming.
#[derive(Debug, Clone, Default)]
pub(crate) struct VideoGraph {
    /// `[-ss <s>] -i file:<f>` per layer, in z-order.
    pub inputs: Vec<String>,
    pub chains: Vec<String>,
    /// The composited stream's label, without brackets.
    pub label: String,
    pub files: Vec<String>,
}

/// The canvas-coloured base with each layer overlaid in z-order (A§9.3).
pub(crate) fn video_graph(canvas: &Canvas, layers: &[LayerInput<'_>]) -> VideoGraph {
    let rate = format!("{}/{}", canvas.fps.num, canvas.fps.den);
    let mut graph = VideoGraph {
        chains: vec![format!(
            "color=c={}:s={}x{}:r={rate}[base]",
            canvas.background.ffmpeg_hex(),
            canvas.width,
            canvas.height
        )],
        label: "base".to_owned(),
        ..VideoGraph::default()
    };
    let mut overlays = Vec::new();
    for (i, input) in layers.iter().enumerate() {
        if let Some(seek) = input.selection.seek_ns(input.recorded) {
            graph.inputs.extend(["-ss".to_owned(), seconds_arg(seek)]);
        }
        graph
            .inputs
            .extend(["-i".to_owned(), file_arg(&input.stream.file)]);
        graph.files.push(input.stream.file.clone());
        let (src, dst) = (input.placement.src, input.placement.dst);
        let lead = match input.selection.lead {
            0 => String::new(),
            lead => format!("+{lead}"),
        };
        let mut chain = format!(
            "[{i}:v]fps={rate},setpts=PTS-STARTPTS{lead},crop={}:{}:{}:{},scale={}:{}:flags=bilinear,format=yuva420p",
            src.w, src.h, src.x, src.y, dst.w, dst.h
        );
        if input.layer.opacity < 1.0 {
            chain.push_str(&format!(",lutyuv=a=val*{}", number(input.layer.opacity)));
        }
        chain.push_str(&format!("[l{i}]"));
        graph.chains.push(chain);
        overlays.push(format!(
            "[{}][l{i}]overlay=x={}:y={}:eof_action=pass[o{i}]",
            graph.label, dst.x, dst.y
        ));
        graph.label = format!("o{i}");
    }
    graph.chains.extend(overlays);
    graph
}

/// The audio half of a segment: inputs (numbered from `first_input`) and
/// chains ending in `[a]` with exactly `samples` stereo samples at 48 kHz.
#[derive(Debug, Clone, Default)]
pub(crate) struct AudioGraph {
    pub inputs: Vec<String>,
    pub chains: Vec<String>,
    pub files: Vec<String>,
}

pub(crate) fn audio_graph(
    tracks: &[TrackInput<'_>],
    first_input: usize,
    samples: u64,
    fade_in: bool,
    fade_out: bool,
) -> AudioGraph {
    let mut graph = AudioGraph::default();
    if tracks.is_empty() {
        graph.chains.push(format!(
            "anullsrc=r={SAMPLE_RATE}:cl=stereo,atrim=end_sample={samples}[a]"
        ));
        return graph;
    }
    let mut mix = String::new();
    for (j, input) in tracks.iter().enumerate() {
        if input.file_ns > 0 {
            graph
                .inputs
                .extend(["-ss".to_owned(), seconds_arg(input.file_ns)]);
        }
        graph
            .inputs
            .extend(["-i".to_owned(), file_arg(&input.stream.file)]);
        graph.files.push(input.stream.file.clone());
        // Mono is copied to both channels at 0 dB, like GStreamer's
        // audioconvert (ffmpeg's default upmix is −3 dB).
        let upmix = if input.stream.channels == Some(1) {
            "pan=stereo|c0=c0|c1=c0"
        } else {
            "aformat=channel_layouts=stereo"
        };
        let mut chain = format!(
            "[{}:a]aresample={SAMPLE_RATE},{upmix},volume={}dB",
            first_input + j,
            number(input.track.gain_db)
        );
        if input.file_ns < 0 {
            let delay = ns_to_samples(input.file_ns);
            if delay > 0 {
                chain.push_str(&format!(",adelay={delay}S:all=1"));
            }
        }
        chain.push_str(&format!("[a{j}]"));
        graph.chains.push(chain);
        mix.push_str(&format!("[a{j}]"));
    }
    if tracks.len() >= 2 {
        mix.push_str(&format!(
            "amix=inputs={}:normalize=0:duration=longest,",
            tracks.len()
        ));
    }
    mix.push_str(&format!("apad,atrim=end_sample={samples}"));
    let fade = FADE_SAMPLES.min(samples);
    if fade_in && fade > 0 {
        mix.push_str(&format!(",afade=t=in:ns={fade}"));
    }
    if fade_out && fade > 0 {
        mix.push_str(&format!(",afade=t=out:ss={}:ns={fade}", samples - fade));
    }
    mix.push_str("[a]");
    graph.chains.push(mix);
    graph
}

// ---------------------------------------------------------------- encoders

/// The video encoder of one export (one per export: nothing mixes).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Encoder {
    X264,
    Vaapi { device: String },
    Vp9,
    Av1,
}

impl Encoder {
    fn choose(codec: VideoCodec, caps: &Capabilities, allow_vaapi: bool) -> Self {
        match codec {
            VideoCodec::H264 => match &caps.vaapi {
                Some(va) if allow_vaapi && va.ffmpeg_h264 => Encoder::Vaapi {
                    device: va.device.clone(),
                },
                _ => Encoder::X264,
            },
            VideoCodec::Vp9 => Encoder::Vp9,
            VideoCodec::Av1 => Encoder::Av1,
        }
    }

    /// The ffmpeg encoder name.
    fn name(&self) -> &'static str {
        match self {
            Encoder::X264 => "libx264",
            Encoder::Vaapi { .. } => "h264_vaapi",
            Encoder::Vp9 => "libvpx-vp9",
            Encoder::Av1 => "libsvtav1",
        }
    }

    /// H.264 segments are `.mov` (Matroska's millisecond timestamps make a
    /// stitched MP4 look VFR); VP9/AV1 segments are `.mkv`.
    fn segment_container(&self) -> (&'static str, &'static str) {
        match self {
            Encoder::X264 | Encoder::Vaapi { .. } => ("mov", "mov"),
            Encoder::Vp9 | Encoder::Av1 => ("matroska", "mkv"),
        }
    }

    /// The quality presets (A§9.3; Standard is SPEC §13.1).
    fn args(&self, quality: Quality) -> Vec<String> {
        let pick = |draft: &'static str, standard: &'static str, high: &'static str| match quality {
            Quality::Draft => draft,
            Quality::Standard => standard,
            Quality::High => high,
        };
        let args: Vec<&str> = match self {
            Encoder::X264 => vec![
                "-c:v",
                "libx264",
                "-preset",
                pick("veryfast", "medium", "slow"),
                "-crf",
                pick("23", "20", "18"),
                "-pix_fmt",
                "yuv420p",
                "-profile:v",
                "high",
            ],
            Encoder::Vaapi { .. } => {
                let qp = match quality {
                    Quality::Draft => 26,
                    Quality::Standard => 22,
                    Quality::High => 19,
                };
                return vaapi_h264_args(qp);
            }
            Encoder::Vp9 => vec![
                "-c:v",
                "libvpx-vp9",
                "-crf",
                pick("36", "32", "28"),
                "-b:v",
                "0",
                "-row-mt",
                "1",
                "-deadline",
                "good",
                "-cpu-used",
                pick("5", "4", "2"),
            ],
            Encoder::Av1 => vec![
                "-c:v",
                "libsvtav1",
                "-preset",
                pick("10", "8", "6"),
                "-crf",
                pick("38", "35", "30"),
            ],
        };
        args.into_iter().map(String::from).collect()
    }
}

/// The stitch's audio: encoded once (AAC 192k for MP4, Opus 128k for WebM).
fn stitch_audio(format: ExportFormat) -> (&'static str, Vec<String>) {
    let args: &[&str] = match format {
        ExportFormat::Mp4 => &["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"],
        ExportFormat::Webm => &["-c:a", "libopus", "-b:a", "128k"],
    };
    let encoder = match format {
        ExportFormat::Mp4 => "aac",
        ExportFormat::Webm => "libopus",
    };
    (encoder, args.iter().map(|a| (*a).to_owned()).collect())
}

/// ffmpeg must be detected, and the encoders this export uses present. An
/// empty encoder list means the shell didn't list them: nothing to check.
fn check_ffmpeg<'a>(
    caps: &'a Capabilities,
    encoder: &Encoder,
    format: ExportFormat,
) -> Result<&'a FfmpegCaps, CoreError> {
    let ffmpeg = caps.ffmpeg.as_ref().ok_or_else(|| {
        CoreError::new(ErrorCode::MissingDependency, "ffmpeg wasn't found").with_hint(
            "install ffmpeg with libx264, libvpx-vp9 and libopus; `qrec doctor` checks it",
        )
    })?;
    if !ffmpeg.encoders.is_empty() {
        let video = match encoder {
            Encoder::Vaapi { .. } => None, // verified with the VA check itself
            other => Some(other.name()),
        };
        for name in video
            .into_iter()
            .chain(["pcm_s16le", stitch_audio(format).0])
        {
            if !ffmpeg.encoders.contains(name) {
                return Err(CoreError::new(
                    ErrorCode::MissingDependency,
                    format!("ffmpeg has no {name} encoder"),
                )
                .with_hint("install a full ffmpeg build; `qrec doctor` lists what is missing"));
            }
        }
    }
    Ok(ffmpeg)
}

// -------------------------------------------------------------------- plan

/// The whole export plan. `Invalid` "nothing to export" for an empty
/// timeline; `MissingDependency` without ffmpeg or one of its encoders.
pub fn plan_export(
    project: &Project,
    caps: &Capabilities,
    opts: &ExportOptions,
) -> Result<ExportPlan, CoreError> {
    let codec = resolve_codec(opts.format, Some(opts.codec))?;
    let units = render_units(project);
    if units.is_empty() {
        return Err(CoreError::invalid(
            "nothing to export: the timeline is empty",
        ));
    }
    let encoder = Encoder::choose(codec, caps, opts.allow_vaapi);
    let ffmpeg = check_ffmpeg(caps, &encoder, opts.format)?;
    let mut segments = Vec::with_capacity(units.len());
    let mut files = BTreeSet::new();
    for unit in &units {
        let take = project.take(&unit.take).ok_or_else(|| {
            CoreError::invalid(format!(
                "clip {} uses {}, which isn't in the project",
                unit.clips[0], unit.take
            ))
        })?;
        let segment = plan_segment(
            project,
            take,
            unit,
            &encoder,
            opts.quality,
            &ffmpeg.identity,
        );
        files.extend(segment.1);
        segments.push(segment.0);
    }
    let total_frames = segments.iter().map(|s| s.frames).sum();
    let stitch = plan_stitch(&segments, opts.format, &opts.out);
    Ok(ExportPlan {
        out: opts.out.clone(),
        tmp_ext: opts.format.extension().to_owned(),
        segments,
        stitch,
        total_frames,
        files,
    })
}

/// One segment and the files it reads.
fn plan_segment(
    project: &Project,
    take: &Take,
    unit: &RenderUnit,
    encoder: &Encoder,
    quality: Quality,
    identity: &str,
) -> (SegmentPlan, Vec<String>) {
    let canvas = &project.canvas;
    let n = unit.frames();
    let samples = unit_samples(n, canvas.fps);
    let layers = layer_inputs(take, canvas, unit.in_, n);
    let video = video_graph(canvas, &layers);
    let tracks = track_inputs(take, canvas.fps, unit.in_);
    let fade_in = unit.in_ != 0;
    let fade_out = unit.out < take.duration_frames;
    let audio = audio_graph(&tracks, layers.len(), samples, fade_in, fade_out);

    let mut chains = video.chains;
    chains.push(format!(
        "[{}]trim=end_frame={n},setsar=1,format=yuv420p,{TAG_BT709}[v]",
        video.label
    ));
    let video_out = match encoder {
        Encoder::Vaapi { .. } => {
            chains.push(format!("[v]{VAAPI_UPLOAD}[vv]"));
            "[vv]"
        }
        _ => "[v]",
    };
    chains.extend(audio.chains);

    let mut argv = ffmpeg_prelude(true);
    if let Encoder::Vaapi { device } = encoder {
        argv.extend(vaapi_device_args(device));
    }
    argv.extend(video.inputs);
    argv.extend(audio.inputs);
    argv.extend([
        "-filter_complex".to_owned(),
        chains.join(";"),
        "-map".to_owned(),
        video_out.to_owned(),
        "-map".to_owned(),
        "[a]".to_owned(),
    ]);
    argv.extend(encoder.args(quality));
    argv.extend(color_args());
    argv.extend(["-c:a".to_owned(), "pcm_s16le".to_owned()]);
    let (muxer, ext) = encoder.segment_container();
    argv.extend(["-f".to_owned(), muxer.to_owned()]);

    let device = match encoder {
        Encoder::Vaapi { device } => Some(device.as_str()),
        _ => None,
    };
    let key = cache_key(identity, device, &argv);
    let path = format!("{SEGMENTS_DIR}/{key}.{ext}");
    argv.push(file_arg(&path));
    let mut files = video.files;
    files.extend(audio.files);
    let segment = SegmentPlan {
        clips: unit.clips.clone(),
        take: unit.take.clone(),
        in_: unit.in_,
        out: unit.out,
        frames: n,
        samples,
        key,
        path,
        argv,
    };
    (segment, files)
}

/// `sha256(PLAN_VERSION ‖ ffmpeg identity ‖ VA device for VA plans ‖ argv
/// without the output path)`, the parts joined with `\0`.
pub fn cache_key(
    identity: &str,
    vaapi_device: Option<&str>,
    argv_without_output: &[String],
) -> String {
    let version = PLAN_VERSION.to_string();
    let mut parts: Vec<&str> = vec![&version, identity];
    parts.extend(vaapi_device);
    parts.extend(argv_without_output.iter().map(String::as_str));
    sha256_hex(parts.join("\0").as_bytes())
}

/// The concat list (segment file names only, in the segments dir) and the
/// command that copies the video and encodes the audio once.
fn plan_stitch(segments: &[SegmentPlan], format: ExportFormat, out: &str) -> StitchPlan {
    let mut list_content = String::from("ffconcat version 1.0\n");
    for segment in segments {
        let name = segment.path.rsplit('/').next().unwrap_or(&segment.path);
        list_content.push_str(&format!("file '{name}'\n"));
    }
    let list_path = format!(
        "{SEGMENTS_DIR}/stitch-{}.ffconcat",
        &sha256_hex(list_content.as_bytes())[..16]
    );
    let mut argv = ffmpeg_prelude(true);
    argv.extend(
        [
            "-f",
            "concat",
            "-i",
            &file_arg(&list_path),
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-c:v",
            "copy",
        ]
        .map(String::from),
    );
    argv.extend(stitch_audio(format).1);
    argv.extend(output_args(format.extension(), out));
    StitchPlan {
        list_path,
        list_content,
        argv,
    }
}

/// The libx264 plan of a VA plan (A§9.3 "one encoder per export"): when a
/// VA segment fails, the runner throws the VA renders away and runs this
/// instead. Equal to `plan_export` with `allow_vaapi: false` on the same
/// project. `None` when `plan` doesn't use VA-API.
pub fn without_vaapi(plan: &ExportPlan, caps: &Capabilities) -> Option<ExportPlan> {
    let identity = caps.ffmpeg.as_ref()?.identity.as_str();
    let mut segments = Vec::with_capacity(plan.segments.len());
    for segment in &plan.segments {
        let argv = &segment.argv;
        let hw = argv.iter().position(|a| a == "-init_hw_device")?;
        let qp: u32 = argv
            .iter()
            .position(|a| a == "-qp")
            .and_then(|i| argv.get(i + 1))?
            .parse()
            .ok()?;
        let quality = match qp {
            26 => Quality::Draft,
            22 => Quality::Standard,
            19 => Quality::High,
            _ => return None,
        };
        let codec_at = argv.iter().position(|a| a == "-c:v")?;
        let codec_end = argv.iter().position(|a| a == "-color_range")?;
        let mut x264: Vec<String> = argv[..hw].to_vec();
        x264.extend_from_slice(&argv[hw + 4..codec_at]);
        x264.extend(Encoder::X264.args(quality));
        x264.extend_from_slice(&argv[codec_end..argv.len() - 1]);
        let graph_at = x264.iter().position(|a| a == "-filter_complex")? + 1;
        let upload = format!(";[v]{VAAPI_UPLOAD}[vv]");
        x264[graph_at] = x264[graph_at].replacen(&upload, "", 1);
        let map_at = x264.iter().position(|a| a == "[vv]")?;
        x264[map_at] = "[v]".to_owned();
        let key = cache_key(identity, None, &x264);
        let path = format!("{SEGMENTS_DIR}/{key}.mov");
        x264.push(file_arg(&path));
        segments.push(SegmentPlan {
            key,
            path,
            argv: x264,
            ..segment.clone()
        });
    }
    let format = match plan.tmp_ext.as_str() {
        "webm" => ExportFormat::Webm,
        _ => ExportFormat::Mp4,
    };
    let stitch = plan_stitch(&segments, format, &plan.out);
    Some(ExportPlan {
        segments,
        stitch,
        ..plan.clone()
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::{Color, Crop, Fit, Rect};
    use crate::state::VaapiCaps;
    use crate::testkit::{self, ProjectBuilder};

    const F30: Fps = Fps::new(30, 1);

    fn opts(out: &str) -> ExportOptions {
        ExportOptions {
            format: ExportFormat::Mp4,
            quality: Quality::Standard,
            codec: VideoCodec::H264,
            out: out.to_owned(),
            allow_vaapi: true,
        }
    }

    /// The SPEC §6.3 scene: a screen, a camera (41 ms late, 80 % opaque), a
    /// mono mic (3 ms late, synced +20 ms) and stereo system audio (−6 dB,
    /// starting 20 ms *before* T0).
    fn spec_project() -> Project {
        ProjectBuilder::new()
            .name("my-video")
            .screen_layer("L1", "DP-4")
            .camera_layer("L2", "/dev/video0")
            .mic("A1", "alsa_input.usb")
            .system("A2", "alsa_output.monitor")
            .take("take-0001", 912)
            .with_take("take-0001", |take| {
                take.scene.layers[1].opacity = 0.8;
                take.scene.audio[0].sync_offset_ms = 20;
                take.scene.audio[1].gain_db = -6.0;
                for s in &mut take.streams {
                    s.start_offset_ns = match s.id.as_str() {
                        "L1" => 0,
                        "L2" => 41_000_000,
                        "A1" => 3_000_000,
                        _ => -20_000_000,
                    };
                    if s.id == "L1" {
                        s.width = Some(2560);
                        s.height = Some(1440);
                    }
                }
            })
            .clip("c7f3", "take-0001", 0, 410)
            .clip("c8a1", "take-0001", 455, 912)
            .build()
    }

    /// A readable rendering of a plan for snapshots: one argv element per
    /// line, the filter graph one chain per line.
    pub(crate) fn render_argv(argv: &[String]) -> String {
        let mut out = String::new();
        let mut graph = false;
        for arg in argv {
            if graph {
                for chain in arg.split(';') {
                    out.push_str(&format!("    {chain}\n"));
                }
                graph = false;
                continue;
            }
            graph = arg == "-filter_complex";
            out.push_str(&format!("  {arg}\n"));
        }
        out
    }

    fn render_plan(plan: &ExportPlan) -> String {
        let mut out = format!(
            "out: {}\ntmp_ext: {}\ntotal_frames: {}\nfiles: {:?}\n",
            plan.out, plan.tmp_ext, plan.total_frames, plan.files
        );
        for (i, s) in plan.segments.iter().enumerate() {
            let clips: Vec<&str> = s.clips.iter().map(ClipId::as_str).collect();
            out.push_str(&format!(
                "\nsegment {i}: {} [{}, {}) clips={} frames={} samples={}\nkey: {}\npath: {}\nargv:\n{}",
                s.take,
                s.in_,
                s.out,
                clips.join(","),
                s.frames,
                s.samples,
                s.key,
                s.path,
                render_argv(&s.argv)
            ));
        }
        out.push_str(&format!(
            "\nstitch list {}:\n{}argv:\n{}",
            plan.stitch.list_path,
            plan.stitch.list_content,
            render_argv(&plan.stitch.argv)
        ));
        out
    }

    fn va_caps() -> Capabilities {
        let mut caps = testkit::capabilities();
        caps.vaapi = Some(VaapiCaps {
            device: "/dev/dri/renderD129".into(),
            vendor: 0x8086,
            ffmpeg_h264: true,
            gst_h264_factory: Some("vah264enc".into()),
        });
        caps
    }

    #[test]
    fn units_merge_continuous_clips() {
        let p = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 300)
            .take("take-0002", 100)
            .clip("c1", "take-0001", 0, 50)
            .clip("c2", "take-0001", 50, 80)
            .clip("c3", "take-0002", 80, 100)
            .clip("c4", "take-0001", 80, 120)
            .clip("c5", "take-0001", 130, 140)
            .clip("c6", "take-0001", 140, 150)
            .build();
        let units = render_units(&p);
        let summary: Vec<(Vec<&str>, &str, Frame, Frame, Frame)> = units
            .iter()
            .map(|u| {
                (
                    u.clips.iter().map(ClipId::as_str).collect(),
                    u.take.as_str(),
                    u.in_,
                    u.out,
                    u.position,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (vec!["c1", "c2"], "take-0001", 0, 80, 0),
                (vec!["c3"], "take-0002", 80, 100, 80),
                // Same take and c3.out == c4.in, but c3 is another take.
                (vec!["c4"], "take-0001", 80, 120, 100),
                (vec!["c5", "c6"], "take-0001", 130, 150, 140),
            ]
        );
        assert!(render_units(&ProjectBuilder::new().build()).is_empty());
    }

    #[test]
    fn frame_selection_at_the_canvas_rate() {
        let mut s = spec_project().takes.values().next().unwrap().streams[0].clone();
        s.start_offset_ns = 0;
        assert_eq!(
            select_frames(&s, F30, 37),
            FrameSelection { k0: 37, lead: 0 }
        );
        // 41 ms rounds to frame 1: the layer starts one frame late.
        s.start_offset_ns = 41_000_000;
        assert_eq!(select_frames(&s, F30, 0), FrameSelection { k0: 0, lead: 1 });
        assert_eq!(
            select_frames(&s, F30, 37),
            FrameSelection { k0: 36, lead: 0 }
        );
        // −20 ms rounds to frame −1: the recording started a frame early.
        s.start_offset_ns = -20_000_000;
        assert_eq!(select_frames(&s, F30, 0), FrameSelection { k0: 1, lead: 0 });
        // 16 ms rounds to frame 0.
        s.start_offset_ns = 16_000_000;
        assert_eq!(select_frames(&s, F30, 0), FrameSelection { k0: 0, lead: 0 });
        // An unreduced rate is the same rate, even at an exact half frame.
        s.start_offset_ns = 16_666_667;
        let reduced = select_frames(&s, F30, 0);
        assert_eq!(reduced, FrameSelection { k0: 0, lead: 1 });
        s.fps = Some(Fps::new(60, 2));
        assert_eq!(select_frames(&s, F30, 0), reduced);
        assert!(same_rate(Fps::new(60, 2), F30) && !same_rate(Fps::new(60, 1), F30));
        let sel = FrameSelection { k0: 37, lead: 0 };
        assert_eq!(sel.seek_ns(F30), Some(1_233_333_333 - 2_000_000));
        assert_eq!(FrameSelection { k0: 0, lead: 3 }.seek_ns(F30), None);
    }

    #[test]
    fn frame_selection_at_another_recorded_rate() {
        let mut s = spec_project().takes.values().next().unwrap().streams[0].clone();
        s.fps = Some(Fps::new(60, 1));
        s.start_offset_ns = 0;
        // 60 fps into 30: frame 10 starts at 333.3 ms = recorded frame 20.
        assert_eq!(
            select_frames(&s, F30, 10),
            FrameSelection { k0: 20, lead: 0 }
        );
        // 100 ms late: canvas frames 0-2 come before recorded frame 0 is
        // nearest; frame 3 (100 ms) shows it.
        s.start_offset_ns = 100_000_000;
        assert_eq!(select_frames(&s, F30, 0), FrameSelection { k0: 0, lead: 3 });
        assert_eq!(select_frames(&s, F30, 2), FrameSelection { k0: 0, lead: 1 });
        assert_eq!(select_frames(&s, F30, 3), FrameSelection { k0: 0, lead: 0 });
        assert_eq!(select_frames(&s, F30, 4), FrameSelection { k0: 2, lead: 0 });
        // Brute force: lead is the number of canvas frames whose nearest
        // recorded frame is before frame 0.
        for (rec, canvas) in [
            (Fps::new(60, 1), F30),
            (Fps::new(24, 1), F30),
            (Fps::new(30000, 1001), Fps::new(25, 1)),
            (F30, Fps::new(60, 1)),
        ] {
            s.fps = Some(rec);
            for offset in [
                -50_000_000,
                -1,
                0,
                1,
                8_333_333,
                8_333_334,
                41_000_000,
                250_000_000,
            ] {
                s.start_offset_ns = offset;
                for in_ in 0..20u64 {
                    let sel = select_frames(&s, canvas, in_);
                    let nearest =
                        |t: u64| ns_to_frame_round(frame_ns(t as i64, canvas) - offset, rec);
                    let lead = (in_..in_ + 1000).take_while(|&t| nearest(t) < 0).count() as u64;
                    assert_eq!(
                        sel.lead, lead,
                        "{rec} → {canvas}, offset {offset}, in {in_}"
                    );
                    assert_eq!(sel.k0, nearest(in_ + lead).max(0) as u64);
                }
            }
        }
    }

    #[test]
    fn samples_round_to_nearest() {
        assert_eq!(unit_samples(30, F30), 48_000);
        assert_eq!(unit_samples(1, Fps::new(30000, 1001)), 1602); // 1601.6
        assert_eq!(unit_samples(5, Fps::new(30000, 1001)), 8008);
        assert_eq!(unit_samples(1, Fps::new(240, 1)), 200);
        assert_eq!(ns_to_samples(-41_000_000), 1968);
        assert_eq!(ns_to_samples(10_416), 0);
        assert_eq!(ns_to_samples(10_417), 1);
    }

    #[test]
    fn codecs_follow_the_format() {
        assert_eq!(
            resolve_codec(ExportFormat::Mp4, None).unwrap(),
            VideoCodec::H264
        );
        assert_eq!(
            resolve_codec(ExportFormat::Webm, None).unwrap(),
            VideoCodec::Vp9
        );
        assert_eq!(
            resolve_codec(ExportFormat::Webm, Some(VideoCodec::Av1)).unwrap(),
            VideoCodec::Av1
        );
        assert_eq!(
            resolve_codec(ExportFormat::Mp4, Some(VideoCodec::Vp9))
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
        assert_eq!(
            resolve_codec(ExportFormat::Webm, Some(VideoCodec::H264))
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
    }

    #[test]
    fn spec_project_plan() {
        let plan = plan_export(
            &spec_project(),
            &testkit::capabilities(),
            &opts("/home/me/out.mp4"),
        )
        .unwrap();
        assert_eq!(plan.segments.len(), 2);
        assert_eq!(plan.total_frames, 410 + 457);
        let s = &plan.segments[0];
        assert_eq!((s.frames, s.samples), (410, 656_000));
        assert_eq!(s.argv.last().unwrap(), &format!("file:{}", s.path));
        assert_eq!(s.path, format!(".qrec/cache/segments/{}.mov", s.key));
        assert_eq!(&s.argv[..9], &ffmpeg_prelude(true)[..]);
        insta::assert_snapshot!("spec_project_mp4", render_plan(&plan));
    }

    #[test]
    fn vaapi_plan_and_its_x264_fallback() {
        let project = spec_project();
        let plan = plan_export(
            &project,
            &va_caps(),
            &opts("exports/my-video-20261014-120000.mp4"),
        )
        .unwrap();
        assert!(
            plan.segments
                .iter()
                .all(|s| s.argv.contains(&"h264_vaapi".to_owned()))
        );
        insta::assert_snapshot!("spec_project_vaapi", render_plan(&plan));

        let mut no_va = opts("exports/my-video-20261014-120000.mp4");
        no_va.allow_vaapi = false;
        let x264 = plan_export(&project, &va_caps(), &no_va).unwrap();
        assert!(
            x264.segments
                .iter()
                .all(|s| s.argv.contains(&"libx264".to_owned()))
        );
        assert_ne!(plan.segments[0].key, x264.segments[0].key);
        assert_eq!(without_vaapi(&plan, &va_caps()), Some(x264.clone()));
        assert_eq!(without_vaapi(&x264, &va_caps()), None, "not a VA plan");
        // Same argv as the plain-caps plan: only the encoder differs.
        assert_eq!(
            x264,
            plan_export(&project, &testkit::capabilities(), &no_va).unwrap()
        );
        for quality in [Quality::Draft, Quality::High] {
            let mut o = opts("x.mp4");
            o.quality = quality;
            let va = plan_export(&project, &va_caps(), &o).unwrap();
            o.allow_vaapi = false;
            assert_eq!(
                without_vaapi(&va, &va_caps()).unwrap(),
                plan_export(&project, &va_caps(), &o).unwrap()
            );
        }
    }

    #[test]
    fn webm_plans() {
        let project = spec_project();
        let mut o = opts("/tmp/out.webm");
        o.format = ExportFormat::Webm;
        o.codec = VideoCodec::Vp9;
        o.quality = Quality::High;
        let vp9 = plan_export(&project, &va_caps(), &o).unwrap();
        assert!(vp9.segments[0].path.ends_with(".mkv"));
        assert_eq!(vp9.tmp_ext, "webm");
        o.codec = VideoCodec::Av1;
        o.quality = Quality::Draft;
        let av1 = plan_export(&project, &va_caps(), &o).unwrap();
        let mut both = render_argv(&vp9.segments[0].argv);
        both.push_str("--- av1\n");
        both.push_str(&render_argv(&av1.segments[1].argv));
        both.push_str("--- stitch\n");
        both.push_str(&render_argv(&av1.stitch.argv));
        insta::assert_snapshot!("webm_vp9_av1", both);
        o.codec = VideoCodec::H264;
        assert_eq!(
            plan_export(&project, &va_caps(), &o).unwrap_err().code,
            ErrorCode::Usage
        );
    }

    #[test]
    fn layers_and_tracks_that_are_left_out() {
        let project = ProjectBuilder::new()
            .canvas(1280, 720, F30)
            .screen_layer("L1", "DP-1")
            .screen_layer("L2", "DP-2")
            .camera_layer("L3", "/dev/video0")
            .screen_layer("L4", "DP-3")
            .screen_layer("L5", "DP-4")
            .mic("A1", "mic")
            .system("A2", "monitor")
            .mic("A3", "mic2")
            .take("take-0001", 100)
            .with_take("take-0001", |take| {
                take.scene.layers[1].visible = false;
                take.scene.layers[2].opacity = 0.0;
                take.scene.layers[3].rect = Rect::new(2000, 0, 100, 100); // off the canvas
                take.scene.layers[4].missing = true;
                take.scene.audio[0].muted = true;
                take.scene.audio[2].missing = true;
                take.streams.retain(|s| s.id != "A2");
            })
            .clip("c1", "take-0001", 0, 100)
            .build();
        let take = project.takes.values().next().unwrap();
        let layers: Vec<&str> = layer_inputs(take, &project.canvas, 0, 100)
            .iter()
            .map(|l| l.layer.id.as_str())
            .collect();
        assert_eq!(layers, ["L1"]);
        assert!(track_inputs(take, F30, 0).is_empty());
        let plan = plan_export(&project, &testkit::capabilities(), &opts("x.mp4")).unwrap();
        let graph = &plan.segments[0].argv[plan.segments[0]
            .argv
            .iter()
            .position(|a| a == "-filter_complex")
            .unwrap()
            + 1];
        assert!(
            graph.contains("anullsrc=r=48000:cl=stereo,atrim=end_sample=160000[a]"),
            "{graph}"
        );
        assert_eq!(plan.files.len(), 1);
    }

    #[test]
    fn layers_that_start_after_a_short_unit_are_skipped() {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 100)
            .with_take("take-0001", |take| {
                take.streams[0].start_offset_ns = 100_000_000
            })
            .clip("c1", "take-0001", 0, 3)
            .clip("c2", "take-0001", 50, 52)
            .build();
        let plan = plan_export(&project, &testkit::capabilities(), &opts("x.mp4")).unwrap();
        // 100 ms = 3 frames of lead ≥ the 3-frame unit: background only.
        assert!(
            !plan.segments[0]
                .argv
                .iter()
                .any(|a| a.starts_with("file:takes/"))
        );
        assert!(
            plan.segments[1]
                .argv
                .contains(&"file:takes/take-0001/L1-screen-DP-1.mkv".to_owned())
        );
    }

    #[test]
    fn fades_mark_cuts_but_not_take_edges() {
        let project = ProjectBuilder::new()
            .mic("A1", "mic")
            .take("take-0001", 100)
            .clip("c1", "take-0001", 0, 40)
            .clip("c2", "take-0001", 60, 100)
            .clip("c3", "take-0001", 45, 46)
            .build();
        let plan = plan_export(&project, &testkit::capabilities(), &opts("x.mp4")).unwrap();
        let graphs: Vec<String> = plan
            .segments
            .iter()
            .map(|s| {
                s.argv[s.argv.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone()
            })
            .collect();
        assert!(
            graphs[0].ends_with("apad,atrim=end_sample=64000,afade=t=out:ss=63760:ns=240[a]"),
            "{}",
            graphs[0]
        );
        assert!(
            graphs[1].ends_with("apad,atrim=end_sample=64000,afade=t=in:ns=240[a]"),
            "{}",
            graphs[1]
        );
        assert!(
            graphs[2].ends_with(
                "apad,atrim=end_sample=1600,afade=t=in:ns=240,afade=t=out:ss=1360:ns=240[a]"
            ),
            "{}",
            graphs[2]
        );
        // Mono, no seek at frame 0, seek at frame 60.
        assert!(graphs[0].contains("[0:a]aresample=48000,pan=stereo|c0=c0|c1=c0,volume=0dB[a0]"));
        assert!(
            plan.segments[1]
                .argv
                .windows(2)
                .any(|w| w[0] == "-ss" && w[1] == "2.000000")
        );
    }

    #[test]
    fn identical_units_share_a_key() {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .mic("A1", "mic")
            .take("take-0001", 100)
            .take("take-0002", 100)
            .clip("c1", "take-0001", 10, 20)
            .clip("c2", "take-0002", 0, 10)
            .clip("c3", "take-0001", 10, 20)
            .build();
        let plan = plan_export(&project, &testkit::capabilities(), &opts("x.mp4")).unwrap();
        assert_eq!(plan.segments[0].key, plan.segments[2].key);
        assert_ne!(plan.segments[0].key, plan.segments[1].key);
        assert_eq!(plan.segments[0].argv, plan.segments[2].argv);
        // The key depends on the ffmpeg binary.
        let mut caps = testkit::capabilities();
        caps.ffmpeg.as_mut().unwrap().identity = "/nix/store/x-ffmpeg/bin/ffmpeg".into();
        let other = plan_export(&project, &caps, &opts("x.mp4")).unwrap();
        assert_ne!(other.segments[0].key, plan.segments[0].key);
        assert_eq!(
            plan.stitch
                .list_content
                .lines()
                .filter(|l| l.contains(&plan.segments[0].key))
                .count(),
            2
        );
    }

    #[test]
    fn crop_fit_opacity_and_background() {
        let mut project = ProjectBuilder::new()
            .canvas(1920, 1080, Fps::new(30000, 1001))
            .camera_layer("L1", "/dev/video0")
            .take("take-0001", 300)
            .clip("c1", "take-0001", 100, 130)
            .build();
        project.canvas.background = "#12170f".parse::<Color>().unwrap();
        let take = project.takes.get_mut(&TakeId::numbered(1)).unwrap();
        let layer = &mut take.scene.layers[0];
        layer.crop = Crop {
            top: 10,
            right: 0,
            bottom: 0,
            left: 21,
        };
        layer.rect = Rect::new(-31, 17, 640, 480);
        layer.fit = Fit::Contain;
        layer.opacity = 0.5;
        let plan = plan_export(&project, &testkit::capabilities(), &opts("x.mp4")).unwrap();
        insta::assert_snapshot!("crop_fit_opacity_2997", render_argv(&plan.segments[0].argv));
    }

    #[test]
    fn missing_ffmpeg_and_encoders() {
        let project = spec_project();
        let mut caps = testkit::capabilities();
        caps.ffmpeg = None;
        assert_eq!(
            plan_export(&project, &caps, &opts("x.mp4"))
                .unwrap_err()
                .code,
            ErrorCode::MissingDependency
        );
        let mut caps = testkit::capabilities();
        caps.ffmpeg.as_mut().unwrap().encoders.remove("libsvtav1");
        let mut o = opts("x.webm");
        o.format = ExportFormat::Webm;
        o.codec = VideoCodec::Av1;
        let err = plan_export(&project, &caps, &o).unwrap_err();
        assert_eq!(err.code, ErrorCode::MissingDependency);
        assert!(err.message.contains("libsvtav1"));
        // An empty list means "not listed": nothing to check.
        caps.ffmpeg.as_mut().unwrap().encoders.clear();
        assert!(plan_export(&project, &caps, &o).is_ok());
        let empty = ProjectBuilder::new().build();
        assert_eq!(
            plan_export(&empty, &testkit::capabilities(), &opts("x.mp4"))
                .unwrap_err()
                .message,
            "nothing to export: the timeline is empty"
        );
    }
}
