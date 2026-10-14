//! Renders a [`GesTimeline`] (A§9.6) with GStreamer Editing Services: the
//! timeline the GUI plays in Program mode, and single frames as PNG for the
//! GES-vs-frame-plan parity test (A§11, SPEC §12).
//!
//! [`build_timeline`] turns the plan into a `ges::Timeline`:
//! - the canvas background is a solid-color test clip on the bottom layer;
//! - each video clip goes on the GES layer of its `layer_priority` (0 = top),
//!   with a `videocrop` effect from `placement.src` (when it crops anything)
//!   and always `posx/posy/width/height` from `placement.dst` and `alpha`;
//! - each audio clip goes on the first audio layer where it overlaps nothing,
//!   with its `volume`, and 240-sample linear fades as a
//!   `GstInterpolationControlSource` on that `volume`;
//! - the video track is restricted to the canvas size and rate, the audio
//!   track to 48 kHz stereo.
//!
//! Recorded files are Matroska, whose timestamps are whole milliseconds: a
//! frame stamped up to 0.5 ms *after* its exact time would leave the
//! previous frame overlapping the start of the output frame by that much,
//! and the compositor would show it. Video in-points are therefore moved
//! [`MKV_INPOINT_NUDGE_NS`] later, like the export's `-ss … − 2 ms` the
//! other way round (*measured* by the frame-ID test below).
//!
//! [`FrameRenderer`] runs the timeline in a `ges::Pipeline` (video preview
//! mode, appsink) and renders chosen timeline frames to RGB or PNG with
//! accurate seeks. Every wait has a deadline; GStreamer is initialized
//! through [`crate::gst_init`]. GES decodes in software only: hardware
//! decoders are demoted the first time GES is initialized, so VA-API never
//! picks a device on its own (A§8.10). Use it from a worker thread: building
//! a timeline discovers every file and rendering blocks while it seeks.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ges::prelude::*;
use gst::glib;
use gst::glib::translate::{FromGlibPtrFull, ToGlibPtr};
use qrec_core::layout::Placement;
use qrec_core::model::Canvas;
use qrec_core::plans::{GesAudioClip, GesTimeline, GesVideoClip};
use qrec_core::time::{frame_ns, ns_to_frame_ceil};

use crate::gst_init::{self, InitError};

/// How long each step of a render may take (preroll, a seek, PNG encoding).
pub const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long tearing a pipeline down may take before it is abandoned.
pub const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Video in-points move this much later so that Matroska's millisecond
/// timestamps never let the previous frame overlap the output frame (see
/// the module docs).
pub const MKV_INPOINT_NUDGE_NS: u64 = 2_000_000;

/// The audio rate of the timeline (fades are counted in its samples).
pub const AUDIO_RATE: u32 = 48_000;

/// Why GES couldn't build or render a timeline.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GesError {
    /// GStreamer isn't usable.
    #[error(transparent)]
    Init(#[from] InitError),
    /// The plan couldn't become a GES timeline (missing file, bad value).
    #[error("{0}")]
    Build(String),
    /// The pipeline failed while rendering.
    #[error("{0}")]
    Render(String),
    /// A step didn't finish in time.
    #[error("{0} didn't finish within {1} s")]
    Timeout(String, u64),
}

/// Initializes GStreamer (with the A§8.10 deadline) and GES, and keeps GES
/// in software (once per process, before any timeline exists).
pub fn init() -> Result<(), GesError> {
    gst_init::init()?;
    static GES: OnceLock<Result<(), String>> = OnceLock::new();
    GES.get_or_init(|| {
        // Before ges_init: it already picks its compositor.
        software_only(&gst::Registry::get());
        ges::init().map_err(|e| format!("GES failed to initialize: {e}"))
    })
    .clone()
    .map_err(GesError::Build)
}

/// What [`software_only`] does to an element factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    Keep,
    /// Never autoplugged (rank NONE).
    Demote,
    /// Taken out of this process's registry.
    Hide,
}

/// Hardware decoders are demoted: they are bound to whatever render node
/// registered them (possibly the hanging NVIDIA one, A§8.10) and decode
/// differently from the export. Every compositor but the software
/// `compositor` is hidden: GES picks its mixer by *loading* every factory
/// whose class says "Compositor", and loading the VA plugin opens every
/// render node.
fn policy(name: &str, klass: &str) -> Policy {
    if klass.contains("Compositor") && name != "compositor" {
        Policy::Hide
    } else if klass.contains("Decoder") && klass.contains("Hardware") {
        Policy::Demote
    } else {
        Policy::Keep
    }
}

fn software_only(registry: &gst::Registry) {
    for feature in registry.features(gst::ElementFactory::static_type()) {
        let Some(factory) = feature.downcast_ref::<gst::ElementFactory>() else {
            continue;
        };
        let klass = factory
            .metadata(gst::ELEMENT_METADATA_KLASS)
            .unwrap_or_default();
        match policy(factory.name().as_str(), klass) {
            Policy::Keep => {}
            Policy::Demote => {
                tracing::debug!("ges: demoting the hardware decoder {}", factory.name());
                factory.set_rank(gst::Rank::NONE);
            }
            Policy::Hide => {
                tracing::debug!("ges: hiding the compositor {}", factory.name());
                registry.remove_feature(&feature);
            }
        }
    }
}

// ------------------------------------------------------------- pure parts

/// `video/x-raw,width=W,height=H,framerate=N/D,pixel-aspect-ratio=1/1`.
pub fn video_restriction(canvas: &Canvas) -> String {
    format!(
        "video/x-raw,width={},height={},framerate={}/{},pixel-aspect-ratio=1/1",
        canvas.width, canvas.height, canvas.fps.num, canvas.fps.den
    )
}

/// `audio/x-raw,rate=48000,channels=2`.
pub fn audio_restriction() -> String {
    format!("audio/x-raw,rate={AUDIO_RATE},channels=2")
}

/// Pixels to cut from each side of a `width`×`height` source so that
/// `placement.src` is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CropSides {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl CropSides {
    pub fn of(placement: &Placement, width: u32, height: u32) -> Self {
        let src = placement.src;
        let left = src.x.min(width);
        let top = src.y.min(height);
        Self {
            left,
            top,
            right: width.saturating_sub(left.saturating_add(src.w)),
            bottom: height.saturating_sub(top.saturating_add(src.h)),
        }
    }

    pub fn is_none(&self) -> bool {
        *self == Self::default()
    }

    /// The `videocrop` effect's bin description.
    pub fn effect_description(&self) -> String {
        format!(
            "videocrop left={} top={} right={} bottom={}",
            self.left, self.top, self.right, self.bottom
        )
    }
}

/// `n` samples at [`AUDIO_RATE`], in ns.
pub fn samples_ns(n: u32) -> u64 {
    u64::from(n) * 1_000_000_000 / u64::from(AUDIO_RATE)
}

/// The control points `(internal time, volume)` of an audio clip's fades:
/// linear ramps from 0 over `fade_in_samples` at its in-point and down to 0
/// over `fade_out_samples` at its end. Empty without fades. Times are the
/// clip's internal (media) time, as GES expects them.
pub fn fade_points(clip: &GesAudioClip) -> Vec<(u64, f64)> {
    let start = clip.inpoint_ns;
    let end = clip.inpoint_ns + clip.duration_ns;
    let fade_in = samples_ns(clip.fade_in_samples).min(clip.duration_ns);
    let fade_out = samples_ns(clip.fade_out_samples).min(clip.duration_ns);
    let mut points = Vec::new();
    if fade_in > 0 {
        points.push((start, 0.0));
        points.push((start + fade_in, clip.volume));
    }
    if fade_out > 0 {
        let from = (end - fade_out).max(points.last().map_or(start, |p| p.0));
        if points.last().is_none_or(|p| p.0 < from) {
            points.push((from, clip.volume));
        }
        points.push((end, 0.0));
    }
    points
}

/// Assigns each `[start, end)` interval to the first lane where it overlaps
/// nothing (GES can't stack clips that cover each other on one layer).
pub fn assign_lanes(intervals: &[(u64, u64)]) -> Vec<usize> {
    let mut lanes: Vec<Vec<(u64, u64)>> = Vec::new();
    intervals
        .iter()
        .map(|&(start, end)| {
            let free = lanes
                .iter()
                .position(|lane| lane.iter().all(|&(s, e)| end <= s || start >= e));
            let lane = free.unwrap_or_else(|| {
                lanes.push(Vec::new());
                lanes.len() - 1
            });
            lanes[lane].push((start, end));
            lane
        })
        .collect()
}

/// `#RRGGBB` → `0xFFRRGGBB`.
fn argb(color: &str) -> u32 {
    let rgb = color
        .strip_prefix('#')
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        .unwrap_or(0);
    0xFF00_0000 | (rgb & 0x00FF_FFFF)
}

/// The `file://` URI of a project-relative (or absolute) file.
fn file_uri(project_dir: &Path, file: &str) -> Result<(PathBuf, String), GesError> {
    let path = if Path::new(file).is_absolute() {
        PathBuf::from(file)
    } else {
        project_dir.join(file)
    };
    if !path.is_file() {
        return Err(GesError::Build(format!("{} is missing", path.display())));
    }
    let uri = glib::filename_to_uri(&path, None)
        .map_err(|e| GesError::Build(format!("{}: {e}", path.display())))?;
    Ok((path, uri.to_string()))
}

// --------------------------------------------------------------- timeline

/// Which tracks a timeline gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracks {
    /// Video and audio (playback).
    All,
    /// Video only (frame rendering).
    Video,
}

/// Builds the GES timeline of `plan` with video and audio tracks; files are
/// relative to `project_dir`.
pub fn build_timeline(plan: &GesTimeline, project_dir: &Path) -> Result<ges::Timeline, GesError> {
    build(plan, project_dir, Tracks::All)
}

/// Builds the GES timeline of `plan` with the given tracks.
pub fn build(plan: &GesTimeline, project_dir: &Path, tracks: Tracks) -> Result<ges::Timeline, GesError> {
    init()?;
    let timeline = ges::Timeline::new();
    timeline.set_auto_transition(false);
    let caps = |text: String| {
        text.parse::<gst::Caps>()
            .map_err(|_| GesError::Build(format!("invalid restriction caps {text:?}")))
    };
    let video = ges::VideoTrack::new();
    video.set_restriction_caps(&caps(video_restriction(&plan.canvas))?);
    timeline
        .add_track(&video)
        .map_err(|e| GesError::Build(format!("can't add the video track: {e}")))?;
    if tracks == Tracks::All {
        let audio = ges::AudioTrack::new();
        audio.set_restriction_caps(&caps(audio_restriction())?);
        timeline
            .add_track(&audio)
            .map_err(|e| GesError::Build(format!("can't add the audio track: {e}")))?;
    }

    let video_layers = plan
        .video
        .iter()
        .map(|c| c.layer_priority as usize + 1)
        .max()
        .unwrap_or(0);
    let layers: Vec<ges::Layer> = (0..video_layers).map(|_| timeline.append_layer()).collect();
    let background = timeline.append_layer();
    let mut assets = Assets::default();

    if plan.duration_ns > 0 {
        add_background(&background, &plan.canvas, plan.duration_ns)?;
    }
    for clip in &plan.video {
        add_video(&layers[clip.layer_priority as usize], clip, project_dir, &mut assets)?;
    }
    let audio = if tracks == Tracks::All { plan.audio.as_slice() } else { &[] };
    let intervals: Vec<(u64, u64)> = audio
        .iter()
        .map(|c| (c.start_ns, c.start_ns + c.duration_ns))
        .collect();
    let lanes = assign_lanes(&intervals);
    let lane_count = lanes.iter().map(|l| l + 1).max().unwrap_or(0);
    let audio_layers: Vec<ges::Layer> = (0..lane_count).map(|_| timeline.append_layer()).collect();
    for (clip, lane) in audio.iter().zip(lanes) {
        add_audio(&audio_layers[lane], clip, project_dir, &mut assets)?;
    }
    if !timeline.commit_sync() {
        tracing::debug!("ges: the first commit changed nothing");
    }
    Ok(timeline)
}

/// Discovered files, so each one is probed once.
#[derive(Default)]
struct Assets(BTreeMap<String, ges::UriClipAsset>);

impl Assets {
    fn get(&mut self, project_dir: &Path, file: &str) -> Result<ges::UriClipAsset, GesError> {
        let (path, uri) = file_uri(project_dir, file)?;
        if let Some(asset) = self.0.get(&uri) {
            return Ok(asset.clone());
        }
        let asset = ges::UriClipAsset::request_sync(&uri)
            .map_err(|e| GesError::Build(format!("can't read {}: {e}", path.display())))?;
        self.0.insert(uri, asset.clone());
        Ok(asset)
    }
}

fn clock(ns: u64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(ns)
}

fn add_background(layer: &ges::Layer, canvas: &Canvas, duration_ns: u64) -> Result<(), GesError> {
    let clip = ges::TestClip::new()
        .ok_or_else(|| GesError::Build("GES can't create a background clip".into()))?;
    clip.set_vpattern(ges::VideoTestPattern::SolidColor);
    clip.set_mute(true);
    clip.set_supported_formats(ges::TrackType::VIDEO);
    clip.set_start(clock(0));
    clip.set_duration(clock(duration_ns));
    layer
        .add_clip(&clip)
        .map_err(|e| GesError::Build(format!("can't add the canvas background: {e}")))?;
    // A child property of the clip's video test source, which exists now.
    clip.set_child_property("foreground-color", argb(canvas.background.as_str()))
        .map_err(|e| GesError::Build(format!("can't color the canvas background: {e}")))
}

fn add_video(
    layer: &ges::Layer,
    clip: &GesVideoClip,
    project_dir: &Path,
    assets: &mut Assets,
) -> Result<(), GesError> {
    let asset = assets.get(project_dir, &clip.file)?;
    let info = asset.info();
    let Some(stream) = info.video_streams().into_iter().next() else {
        return Err(GesError::Build(format!("{} has no video", clip.file)));
    };
    let added = asset
        .extract()
        .ok()
        .and_then(|e| e.downcast::<ges::Clip>().ok())
        .ok_or_else(|| GesError::Build(format!("GES can't make a clip of {}", clip.file)))?;
    // The nudged clip may end up to the nudge after the media's (millisecond)
    // end, where the last frame still shows; GES checks `max-duration`.
    if let Some(max) = asset.max_duration() {
        added.set_max_duration(clock(max.nseconds() + MKV_INPOINT_NUDGE_NS));
    }
    added.set_start(clock(clip.start_ns));
    added.set_inpoint(clock(clip.inpoint_ns + MKV_INPOINT_NUDGE_NS));
    added.set_duration(clock(clip.duration_ns));
    added.set_supported_formats(ges::TrackType::VIDEO);
    layer
        .add_clip_full(&added)
        .map_err(|e| GesError::Build(format!("can't place {}: {e}", clip.file)))?;
    let crop = CropSides::of(&clip.placement, stream.width(), stream.height());
    if !crop.is_none() {
        let effect = ges::Effect::new(&crop.effect_description())
            .map_err(|e| GesError::Build(format!("can't crop {}: {e}", clip.file)))?;
        added
            .add_top_effect(&effect, 0)
            .map_err(|e| GesError::Build(format!("can't crop {}: {e}", clip.file)))?;
    }
    let dst = clip.placement.dst;
    let set = |name: &str, value: glib::Value| {
        added
            .set_child_property(name, value)
            .map_err(|e| GesError::Build(format!("{}: {name}: {e}", clip.file)))
    };
    set("posx", (dst.x as i32).into())?;
    set("posy", (dst.y as i32).into())?;
    set("width", (dst.w.min(i32::MAX as u32) as i32).into())?;
    set("height", (dst.h.min(i32::MAX as u32) as i32).into())?;
    set("alpha", clip.alpha.clamp(0.0, 1.0).into())
}

fn add_audio(
    layer: &ges::Layer,
    clip: &GesAudioClip,
    project_dir: &Path,
    assets: &mut Assets,
) -> Result<(), GesError> {
    let asset = assets.get(project_dir, &clip.file)?;
    let added = layer
        .add_asset_full(
            &asset,
            clock(clip.start_ns),
            clock(clip.inpoint_ns),
            clock(clip.duration_ns),
            ges::TrackType::AUDIO,
        )
        .map_err(|e| GesError::Build(format!("can't place {}: {e}", clip.file)))?;
    let volume = clip.volume.clamp(0.0, 10.0);
    added
        .set_child_property("volume", volume)
        .map_err(|e| GesError::Build(format!("{}: volume: {e}", clip.file)))?;
    let points = fade_points(clip);
    if points.is_empty() {
        return Ok(());
    }
    let source = added
        .find_track_element(None::<&ges::Track>, ges::AudioSource::static_type())
        .ok_or_else(|| GesError::Build(format!("{} has no audio source", clip.file)))?;
    match controller::linear(&points) {
        Some(control) => {
            if !source.set_control_source(&control, "volume", "direct-absolute") {
                return Err(GesError::Build(format!("can't fade {}", clip.file)));
            }
        }
        None => tracing::warn!(
            "ges: GStreamer's controller library isn't loaded; {} plays without fades",
            clip.file
        ),
    }
    Ok(())
}

/// `GstInterpolationControlSource` without the gstreamer-controller crate:
/// libges links libgstcontroller, so its two entry points are looked up in
/// the running process.
mod controller {
    use super::*;

    type NewFn = unsafe extern "C" fn() -> *mut gst::ffi::GstControlSource;
    type SetFn = unsafe extern "C" fn(*mut c_void, u64, f64) -> glib::ffi::gboolean;

    struct Fns {
        new: NewFn,
        set: SetFn,
    }

    fn fns() -> Option<&'static Fns> {
        static FNS: OnceLock<Option<Fns>> = OnceLock::new();
        FNS.get_or_init(|| {
            // SAFETY: dlsym with RTLD_DEFAULT only reads the loader's symbol
            // tables; both names are nul-terminated literals.
            let (new, set) = unsafe {
                (
                    libc::dlsym(libc::RTLD_DEFAULT, c"gst_interpolation_control_source_new".as_ptr()),
                    libc::dlsym(libc::RTLD_DEFAULT, c"gst_timed_value_control_source_set".as_ptr()),
                )
            };
            if new.is_null() || set.is_null() {
                return None;
            }
            // SAFETY: the symbols have these C signatures in GStreamer ≥ 1.0:
            // `GstControlSource *gst_interpolation_control_source_new (void)`
            // and `gboolean gst_timed_value_control_source_set
            // (GstTimedValueControlSource *, GstClockTime, gdouble)`.
            unsafe {
                Some(Fns {
                    new: std::mem::transmute::<*mut c_void, NewFn>(new),
                    set: std::mem::transmute::<*mut c_void, SetFn>(set),
                })
            }
        })
        .as_ref()
    }

    /// A linear interpolation control source through `points`; `None`
    /// when the controller library isn't available.
    pub(super) fn linear(points: &[(u64, f64)]) -> Option<gst::ControlSource> {
        let fns = fns()?;
        // SAFETY: a plain constructor; it returns a new reference (sunk
        // since 1.x; a floating one is sunk here) or NULL.
        let raw = unsafe { (fns.new)() };
        if raw.is_null() {
            return None;
        }
        // SAFETY: `raw` is a valid GObject; sinking a floating reference
        // turns it into the reference `from_glib_full` takes over.
        let source = unsafe {
            if glib::gobject_ffi::g_object_is_floating(raw as *mut _) != 0 {
                glib::gobject_ffi::g_object_ref_sink(raw as *mut _);
            }
            gst::ControlSource::from_glib_full(raw)
        };
        source.set_property_from_str("mode", "linear");
        let ptr: *mut gst::ffi::GstControlSource = source.to_glib_none().0;
        for &(time, value) in points {
            // SAFETY: `ptr` is the live GstInterpolationControlSource
            // (a GstTimedValueControlSource) owned by `source`.
            if unsafe { (fns.set)(ptr as *mut c_void, time, value) } == 0 {
                return None;
            }
        }
        Some(source)
    }
}

// --------------------------------------------------------------- frames

/// One rendered frame, packed RGB (3 bytes per pixel, rows `stride` apart).
#[derive(Clone)]
pub struct RgbFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub data: Vec<u8>,
    /// The frame's timeline time.
    pub pts_ns: Option<u64>,
    sample: gst::Sample,
}

impl std::fmt::Debug for RgbFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RgbFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("bytes", &self.data.len())
            .field("pts_ns", &self.pts_ns)
            .finish()
    }
}

impl RgbFrame {
    /// The pixel at `(x, y)`.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = y as usize * self.stride as usize + x as usize * 3;
        self.data.get(at..at + 3).map(|p| [p[0], p[1], p[2]])
    }

    /// The frame as a PNG file.
    pub fn png(&self) -> Result<Vec<u8>, GesError> {
        let caps = gst::Caps::builder("image/png").build();
        let png = gst_video::convert_sample(&self.sample, &caps, Some(clock(STEP_TIMEOUT.as_nanos() as u64)))
            .map_err(|e| GesError::Render(format!("PNG encoding failed: {e}")))?;
        let buffer = png
            .buffer()
            .ok_or_else(|| GesError::Render("the PNG encoder produced nothing".into()))?;
        let map = buffer
            .map_readable()
            .map_err(|_| GesError::Render("can't read the PNG".into()))?;
        Ok(map.as_slice().to_vec())
    }
}

/// A GES pipeline that renders single timeline frames.
pub struct FrameRenderer {
    pipeline: ges::Pipeline,
    sink: gst_app::AppSink,
    canvas: Canvas,
    duration_ns: u64,
    _timeline: ges::Timeline,
}

impl FrameRenderer {
    /// Builds the timeline and prerolls the pipeline.
    pub fn new(plan: &GesTimeline, project_dir: &Path) -> Result<Self, GesError> {
        if plan.duration_ns == 0 {
            return Err(GesError::Build("nothing to render: the timeline is empty".into()));
        }
        let timeline = build(plan, project_dir, Tracks::Video)?;
        let pipeline = ges::Pipeline::new();
        pipeline
            .set_timeline(&timeline)
            .map_err(|e| GesError::Build(format!("can't use the timeline: {e}")))?;
        bilinear_compositors(&pipeline);
        pipeline
            .set_mode(ges::PipelineFlags::VIDEO_PREVIEW)
            .map_err(|e| GesError::Build(format!("can't set the preview mode: {e}")))?;
        let (bin, sink) = rgb_sink()?;
        pipeline.preview_set_video_sink(Some(&bin));
        let renderer = Self {
            pipeline,
            sink,
            canvas: plan.canvas.clone(),
            duration_ns: plan.duration_ns,
            _timeline: timeline,
        };
        let change = renderer.pipeline.set_state(gst::State::Paused);
        if change.is_err() {
            return Err(renderer.failure("the GES pipeline failed to start"));
        }
        renderer.wait_async_done("prerolling the GES pipeline")?;
        // A no-op unless GST_DEBUG_DUMP_DOT_DIR is set.
        renderer
            .pipeline
            .debug_to_dot_file(gst::DebugGraphDetails::all(), "qrec-ges-render");
        Ok(renderer)
    }

    /// Timeline frames available.
    pub fn frame_count(&self) -> u64 {
        let duration = i64::try_from(self.duration_ns).unwrap_or(i64::MAX);
        ns_to_frame_ceil(duration, self.canvas.fps).max(0) as u64
    }

    /// Renders timeline frame `frame` (at the canvas rate).
    pub fn render(&mut self, frame: u64) -> Result<RgbFrame, GesError> {
        let count = self.frame_count();
        if frame >= count {
            return Err(GesError::Render(format!(
                "frame {frame} is past the end of the timeline ({count} frames)"
            )));
        }
        let at = frame_ns(frame as i64, self.canvas.fps).max(0) as u64;
        self.pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, clock(at))
            .map_err(|_| self.failure(&format!("seeking to frame {frame} failed")))?;
        self.wait_async_done(&format!("seeking to frame {frame}"))?;
        let sample = self
            .sink
            .try_pull_preroll(clock(STEP_TIMEOUT.as_nanos() as u64))
            .ok_or_else(|| GesError::Timeout(format!("rendering frame {frame}"), STEP_TIMEOUT.as_secs()))?;
        rgb_frame(sample)
    }

    /// Renders `frame` and writes it to `out` as a PNG. Never overwrites.
    pub fn save_png(&mut self, frame: u64, out: &Path) -> Result<(), GesError> {
        let png = self.render(frame)?.png()?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out)
            .map_err(|e| GesError::Render(format!("can't create {}: {e}", out.display())))?;
        file.write_all(&png)
            .and_then(|()| file.sync_all())
            .map_err(|e| GesError::Render(format!("can't write {}: {e}", out.display())))
    }

    /// Waits for the pipeline to finish a state change or seek.
    fn wait_async_done(&self, what: &str) -> Result<(), GesError> {
        let bus = self
            .pipeline
            .bus()
            .ok_or_else(|| GesError::Render("the GES pipeline has no bus".into()))?;
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(GesError::Timeout(what.to_owned(), STEP_TIMEOUT.as_secs()));
            }
            let Some(msg) = bus.timed_pop_filtered(
                clock(left.as_nanos() as u64),
                &[gst::MessageType::AsyncDone, gst::MessageType::Error, gst::MessageType::Eos],
            ) else {
                return Err(GesError::Timeout(what.to_owned(), STEP_TIMEOUT.as_secs()));
            };
            match msg.view() {
                gst::MessageView::AsyncDone(_) => return Ok(()),
                gst::MessageView::Error(err) => {
                    return Err(GesError::Render(format!(
                        "{what}: {} ({})",
                        err.error(),
                        err.debug().unwrap_or_default()
                    )));
                }
                gst::MessageView::Eos(_) => {
                    return Err(GesError::Render(format!("{what}: the timeline ended")));
                }
                _ => {}
            }
        }
    }

    /// The pipeline's first error, or `message`.
    fn failure(&self, message: &str) -> GesError {
        let error = self.pipeline.bus().and_then(|bus| {
            bus.timed_pop_filtered(clock(100_000_000), &[gst::MessageType::Error])
        });
        match error.as_ref().map(|m| m.view()) {
            Some(gst::MessageView::Error(err)) => {
                GesError::Render(format!("{message}: {}", err.error()))
            }
            _ => GesError::Render(message.to_owned()),
        }
    }
}

impl Drop for FrameRenderer {
    fn drop(&mut self) {
        let pipeline = self.pipeline.clone().upcast::<gst::Element>();
        if !set_null_with_deadline(pipeline, TEARDOWN_TIMEOUT) {
            tracing::warn!("ges: the render pipeline didn't stop within {TEARDOWN_TIMEOUT:?}; abandoned");
        }
    }
}

/// Renders each `(frame, out)` of `plan` to a PNG file (never overwrites).
pub fn render_frames_png(
    plan: &GesTimeline,
    project_dir: &Path,
    frames: &[(u64, PathBuf)],
) -> Result<(), GesError> {
    let mut renderer = FrameRenderer::new(plan, project_dir)?;
    for (frame, out) in frames {
        renderer.save_png(*frame, out)?;
    }
    Ok(())
}

/// Makes every `compositor` inside `pipeline` (GES's mixer) scale its
/// inputs bilinearly, like the export's `scale=…:flags=bilinear` (A§9.3):
/// its pads default to a cubic resampler. Call once the timeline is set.
pub fn bilinear_compositors(pipeline: &impl IsA<gst::Bin>) {
    fn configure(pad: &gst::Pad) {
        if pad.find_property("converter-config").is_none() {
            return;
        }
        let config = gst::Structure::builder("GstVideoConverter")
            .field(
                "GstVideoConverter.resampler-method",
                gst_video::VideoResamplerMethod::Linear,
            )
            .field(
                "GstVideoConverter.chroma-resampler-method",
                gst_video::VideoResamplerMethod::Linear,
            )
            .build();
        pad.set_property("converter-config", config);
    }
    fn hook(element: &gst::Element) {
        if element.factory().is_none_or(|f| f.name() != "compositor") {
            return;
        }
        for pad in element.sink_pads() {
            configure(&pad);
        }
        element.connect_pad_added(|_, pad| {
            if pad.direction() == gst::PadDirection::Sink {
                configure(pad);
            }
        });
    }
    let bin = pipeline.as_ref();
    bin.connect_deep_element_added(|_, _, element| hook(element));
    for element in bin.iterate_recurse().into_iter().flatten() {
        hook(&element);
    }
}

/// `videoconvert ! video/x-raw,format=RGB ! appsink` as one bin.
fn rgb_sink() -> Result<(gst::Element, gst_app::AppSink), GesError> {
    let make = |factory: &str| {
        gst::ElementFactory::make(factory)
            .build()
            .map_err(|_| GesError::Build(format!("the GStreamer element {factory} is missing")))
    };
    let convert = make("videoconvert")?;
    let filter = make("capsfilter")?;
    filter.set_property(
        "caps",
        gst::Caps::builder("video/x-raw").field("format", "RGB").build(),
    );
    let sink = gst_app::AppSink::builder()
        .sync(false)
        .max_buffers(1)
        .drop(false)
        .build();
    let bin = gst::Bin::with_name("qrec-frame-sink");
    bin.add_many([&convert, &filter, sink.upcast_ref()])
        .map_err(|e| GesError::Build(format!("can't build the frame sink: {e}")))?;
    gst::Element::link_many([&convert, &filter, sink.upcast_ref()])
        .map_err(|e| GesError::Build(format!("can't build the frame sink: {e}")))?;
    let pad = convert
        .static_pad("sink")
        .ok_or_else(|| GesError::Build("videoconvert has no sink pad".into()))?;
    let ghost = gst::GhostPad::with_target(&pad)
        .map_err(|e| GesError::Build(format!("can't build the frame sink: {e}")))?;
    bin.add_pad(&ghost)
        .map_err(|e| GesError::Build(format!("can't build the frame sink: {e}")))?;
    Ok((bin.upcast(), sink))
}

fn rgb_frame(sample: gst::Sample) -> Result<RgbFrame, GesError> {
    let caps = sample
        .caps()
        .ok_or_else(|| GesError::Render("the rendered frame has no caps".into()))?;
    let info = gst_video::VideoInfo::from_caps(caps)
        .map_err(|_| GesError::Render(format!("unexpected frame caps {caps}")))?;
    let buffer = sample
        .buffer()
        .ok_or_else(|| GesError::Render("the rendered frame is empty".into()))?;
    let pts_ns = buffer.pts().map(gst::ClockTime::nseconds);
    let data = buffer
        .map_readable()
        .map_err(|_| GesError::Render("can't read the rendered frame".into()))?
        .as_slice()
        .to_vec();
    Ok(RgbFrame {
        width: info.width(),
        height: info.height(),
        stride: info.stride()[0].max(0) as u32,
        data,
        pts_ns,
        sample,
    })
}

/// Sets `element` to NULL on a helper thread, waiting at most `timeout`.
fn set_null_with_deadline(element: gst::Element, timeout: Duration) -> bool {
    let (tx, rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("qrec-ges-teardown".into())
        .spawn(move || {
            let _ = element.set_state(gst::State::Null);
            let _ = tx.send(());
        });
    spawned.is_ok() && rx.recv_timeout(timeout).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrec_core::layout::SourceRegion;
    use qrec_core::model::Rect;

    fn placement(src: (u32, u32, u32, u32), dst: (i32, i32, u32, u32)) -> Placement {
        Placement {
            src: SourceRegion { x: src.0, y: src.1, w: src.2, h: src.3 },
            dst: Rect::new(dst.0, dst.1, dst.2, dst.3),
        }
    }

    #[test]
    fn crops_from_the_placement() {
        let crop = CropSides::of(&placement((10, 20, 600, 300), (0, 0, 320, 160)), 640, 360);
        assert_eq!(crop, CropSides { left: 10, top: 20, right: 30, bottom: 40 });
        assert_eq!(crop.effect_description(), "videocrop left=10 top=20 right=30 bottom=40");
        assert!(CropSides::of(&placement((0, 0, 640, 360), (0, 0, 1, 1)), 640, 360).is_none());
    }

    #[test]
    fn keeps_ges_in_software() {
        let video = "Codec/Decoder/Video/Hardware";
        assert_eq!(policy("vah264dec", video), Policy::Demote);
        assert_eq!(policy("nvh264dec", video), Policy::Demote);
        assert_eq!(policy("avdec_h264", "Codec/Decoder/Video"), Policy::Keep);
        let hw = "Filter/Editor/Video/Compositor/Hardware";
        assert_eq!(policy("vacompositor", hw), Policy::Hide);
        assert_eq!(policy("glvideomixer", "Filter/Effect/Video/Compositor"), Policy::Hide);
        assert_eq!(policy("compositor", "Filter/Editor/Video/Compositor"), Policy::Keep);
        assert_eq!(policy("vah264enc", "Codec/Encoder/Video/Hardware"), Policy::Keep);
    }
}

#[cfg(test)]
#[path = "ges_render_tests.rs"]
mod parity_tests;
