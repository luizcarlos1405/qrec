//! The live pipeline (SPEC §5.2, A§8.11): one source branch per capture
//! device, each ending in a tee that record bins attach to; an always
//! linked `fakesink` keeps every branch flowing; the GUI's preview branch
//! composites the project scene onto the canvas.
//!
//! ```text
//! video: src [! caps] [! jpegdec] ! videoconvert ! video/x-raw,format=I420,colorimetry=bt709 ! tee
//! screen: src ! caps ! appsink → bridge (I420, keepalive) → appsrc ! videoconvert ! … ! tee
//! audio: src [! caps] ! audioconvert [! level] ! tee
//! tee ! fakesink sync=false async=false        (always)
//! tee ! queue ! videocrop ! compositor.sink_N   (preview, per visible layer)
//! compositor ! video/x-raw,WxH@fps ! videoconvert ! RGBA ! appsink → SideEvent::Preview
//! ```
//!
//! Element names carry their branch (`s<N>-…`, `pv-…`) so bus errors can be
//! attributed to one source. The pipeline runs on the system (monotonic)
//! clock; audio sources slave to it.

use std::collections::BTreeMap;
use std::str::FromStr;

use crossbeam_channel::Sender;
use gst::glib;
use gst::prelude::*;
use qrec_core::layout::place;
use qrec_core::model::Layer;
use qrec_core::plans::LiveScene;

use super::bridge;
use super::engine::Cmd;
use super::fake::{FAKE_SCREEN_FPS, FAKE_SCREEN_SIZE, FakeTone, fake_screen_is_still, fake_tone};
use super::portal::ScreenShare;
use super::spec::{SourceSpec, tick_settings};
use super::util::{capsfilter, make, set_str, set_value};
use super::{Backend, LevelReading, PreviewFrame, SideEvent};

/// One capture device.
pub(super) struct SourceBranch {
    pub spec: SourceSpec,
    /// Element name prefix (`s0`).
    pub prefix: String,
    /// The source element (errors are attributed to it).
    pub src: gst::Element,
    pub tee: gst::Element,
    /// The negotiated video size, once known.
    pub size: Option<(u32, u32)>,
    /// Why the source stopped working.
    pub dead: Option<String>,
    /// A screen's bridge keepalive (kicked at T0 and T1, A§8.11).
    pub keepalive: Option<bridge::Keepalive>,
}

/// A layer drawn by the preview compositor.
pub(super) struct PreviewLayer {
    pub layer: Layer,
    pub source: usize,
    pub crop: gst::Element,
    pub pad: gst::Pad,
}

pub(super) struct Preview {
    pub layers: Vec<PreviewLayer>,
}

/// What a live pipeline was built for; equal keys ⇒ only layout changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveKey {
    pub sources: Vec<SourceSpec>,
    /// Preview layers (id and source) in z-order, `None` without preview.
    pub preview: Option<Vec<(String, SourceSpec)>>,
}

impl LiveKey {
    pub fn of(sources: Vec<SourceSpec>, scene: Option<&LiveScene>) -> Self {
        let preview = scene.filter(|s| s.preview).map(|s| {
            s.scene
                .layers
                .iter()
                .map(|l| (l.id.to_string(), SourceSpec::of_video(&l.source, None)))
                .collect()
        });
        Self { sources, preview }
    }
}

pub(super) struct Live {
    /// Bus messages and caps notifications carry it; stale ones are ignored.
    pub generation: u64,
    pub pipeline: gst::Pipeline,
    pub sources: Vec<SourceBranch>,
    pub preview: Option<Preview>,
    pub key: LiveKey,
}

/// Why a pipeline couldn't be built: the source (if one is to blame) and a message.
#[derive(Debug, Clone)]
pub(super) struct BuildError {
    pub source: Option<SourceSpec>,
    pub message: String,
}

impl BuildError {
    fn of(spec: &SourceSpec, message: impl Into<String>) -> Self {
        Self {
            source: Some(spec.clone()),
            message: message.into(),
        }
    }

    fn pipeline(message: impl Into<String>) -> Self {
        Self {
            source: None,
            message: message.into(),
        }
    }
}

/// What building a pipeline needs from the engine.
pub(super) struct BuildCtx<'a> {
    pub backend: Backend,
    pub generation: u64,
    pub tx: &'a Sender<Cmd>,
    pub side: Option<&'a Sender<SideEvent>>,
    /// Portal casts of the real backend's screens, by output.
    pub casts: &'a BTreeMap<String, ScreenShare>,
}

impl Live {
    /// Builds (but doesn't start) a pipeline with `sources` and, when
    /// `scene.preview`, the preview compositor.
    pub fn build(
        sources: &[SourceSpec],
        scene: Option<&LiveScene>,
        ctx: &BuildCtx<'_>,
    ) -> Result<Live, BuildError> {
        let pipeline = gst::Pipeline::with_name(&format!("qrec-live-{}", ctx.generation));
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        {
            let tx = ctx.tx.clone();
            let generation = ctx.generation;
            pipeline
                .bus()
                .ok_or_else(|| BuildError::pipeline("the pipeline has no bus"))?
                .set_sync_handler(move |_bus, msg| {
                    let _ = tx.send(Cmd::Bus(generation, msg.clone()));
                    gst::BusSyncReply::Drop
                });
        }
        let mut branches = Vec::with_capacity(sources.len());
        for (index, spec) in sources.iter().enumerate() {
            branches.push(add_source(&pipeline, index, spec, ctx)?);
        }
        let mut live = Live {
            generation: ctx.generation,
            pipeline,
            sources: branches,
            preview: None,
            key: LiveKey::of(sources.to_vec(), scene),
        };
        if let Some(scene) = scene.filter(|s| s.preview) {
            live.preview = Some(add_preview(&live, scene, ctx).map_err(BuildError::pipeline)?);
        }
        Ok(live)
    }

    /// The branch that can feed `wanted`, if it's alive.
    pub fn find(&self, wanted: &SourceSpec) -> Option<usize> {
        self.sources
            .iter()
            .position(|b| b.dead.is_none() && b.spec.serves(wanted))
    }

    /// Whether every source in `wanted` is present and alive.
    pub fn serves_all(&self, wanted: &[SourceSpec]) -> bool {
        wanted.iter().all(|w| self.find(w).is_some())
    }

    /// The branch an element belongs to, by its `s<N>-` name prefix.
    pub fn source_of(&self, names: &[String]) -> Option<usize> {
        names.iter().find_map(|name| {
            let rest = name.strip_prefix('s')?;
            let (index, _) = rest.split_once('-')?;
            let index: usize = index.parse().ok()?;
            (index < self.sources.len() && self.sources[index].prefix == format!("s{index}"))
                .then_some(index)
        })
    }

    /// Updates the preview's per-layer crop, position, size, opacity and
    /// z-order from `scene` (same layers and sources as when built).
    pub fn apply_layout(&self, scene: &LiveScene) {
        let Some(preview) = &self.preview else { return };
        for (z, layer) in scene.scene.layers.iter().enumerate() {
            let Some(pl) = preview.layers.iter().find(|p| p.layer.id == layer.id) else {
                continue;
            };
            let size = self.sources[pl.source].size;
            apply_layer(pl, layer, size, z as u32 + 1);
        }
    }
}

fn add_source(
    pipeline: &gst::Pipeline,
    index: usize,
    spec: &SourceSpec,
    ctx: &BuildCtx<'_>,
) -> Result<SourceBranch, BuildError> {
    let prefix = format!("s{index}");
    let name = |role: &str| format!("{prefix}-{role}");
    let err = |message: String| BuildError::of(spec, message);
    let mut chain: Vec<gst::Element> = Vec::new();
    let mut keepalive = None;
    let src = match (ctx.backend, spec) {
        (Backend::Fake, SourceSpec::Screen { output }) => {
            // Like a screencast: BGRx frames through the screen bridge, and
            // a still screen sends its frame once (damage-driven), so every
            // later frame of it is the bridge's keepalive.
            let src = make("videotestsrc", &name("src")).map_err(err)?;
            set_value(&src, "is-live", true).map_err(err)?;
            let still = fake_screen_is_still(output);
            set_str(&src, "pattern", if still { "smpte" } else { "ball" }).map_err(err)?;
            if still {
                first_buffer_only(&src);
            }
            let (w, h) = FAKE_SCREEN_SIZE;
            let caps = capsfilter(
                &name("caps"),
                &format!(
                    "video/x-raw,format=BGRx,width={w},height={h},framerate={}/{}",
                    FAKE_SCREEN_FPS.num, FAKE_SCREEN_FPS.den
                ),
            )
            .map_err(err)?;
            let (feed, handle) = bridged(pipeline, &[&src, &caps], &name).map_err(err)?;
            keepalive = Some(handle);
            chain.push(feed);
            src
        }
        (Backend::Fake, SourceSpec::Camera { mode, .. }) => {
            let src = make("videotestsrc", &name("src")).map_err(err)?;
            set_value(&src, "is-live", true).map_err(err)?;
            set_str(&src, "pattern", "ball").map_err(err)?;
            let (w, h, num, den) = mode.as_ref().map_or((640, 480, 30, 1), |m| {
                (m.width, m.height, m.fps_num, m.fps_den)
            });
            chain.push(src.clone());
            chain.push(
                capsfilter(
                    &name("caps"),
                    &format!("video/x-raw,width={w},height={h},framerate={num}/{den}"),
                )
                .map_err(err)?,
            );
            src
        }
        (Backend::Fake, SourceSpec::Audio { device }) => {
            let tone = fake_tone(device);
            let src = fake_audio_source(&name("src"), &tone).map_err(err)?;
            chain.push(src.clone());
            chain.push(
                capsfilter(
                    &name("caps"),
                    &format!("audio/x-raw,rate=48000,channels={}", tone.channels),
                )
                .map_err(err)?,
            );
            src
        }
        (Backend::Real, SourceSpec::Screen { output }) => {
            let cast = ctx
                .casts
                .get(output)
                .ok_or_else(|| err(format!("no screen-share session for {output}")))?;
            let src = make("pipewiresrc", &name("src")).map_err(err)?;
            set_value(&src, "fd", cast.fd_raw()).map_err(err)?;
            set_value(&src, "path", cast.node_id.to_string()).map_err(err)?;
            set_value(&src, "do-timestamp", true).map_err(err)?;
            // Never `keepalive-time`: PipeWire 1.6 stamps the keepalive
            // buffer with the absolute clock time and basesrc waits for it,
            // stalling the branch for good; the bridge keeps a still screen
            // going instead (A§8.11).
            if src.find_property("client-name").is_some() {
                set_value(&src, "client-name", "Qrec").map_err(err)?;
            }
            // A screencast that ends (sharing stopped, monitor unplugged)
            // must become a bus error (StreamLost), not a frozen last frame.
            if src.find_property("on-disconnect").is_some() {
                set_str(&src, "on-disconnect", "error").map_err(err)?;
            }
            // The capsfilter gets its DMA_DRM caps only after linking, which
            // they would fail.
            let accept = make("capsfilter", &name("accept")).map_err(err)?;
            let (feed, handle) = bridged(pipeline, &[&src, &accept], &name).map_err(err)?;
            keepalive = Some(handle);
            let caps = gst::Caps::from_str(bridge::SCREEN_CAPS)
                .map_err(|e| err(format!("bad screencast caps: {e}")))?;
            accept.set_property("caps", &caps);
            chain.push(feed);
            src
        }
        (Backend::Real, SourceSpec::Camera { device, mode }) => {
            let src = make("v4l2src", &name("src")).map_err(err)?;
            set_value(&src, "device", device.as_str()).map_err(err)?;
            set_value(&src, "do-timestamp", true).map_err(err)?;
            chain.push(src.clone());
            if let Some(mode) = mode {
                chain.push(capsfilter(&name("caps"), &mode.caps_string()).map_err(err)?);
                if mode.is_mjpeg() {
                    chain.push(make("jpegdec", &name("jpegdec")).map_err(err)?);
                }
            }
            src
        }
        (Backend::Real, SourceSpec::Audio { device }) => {
            let src = make("pulsesrc", &name("src")).map_err(err)?;
            set_value(&src, "device", device.as_str()).map_err(err)?;
            set_value(&src, "client-name", "Qrec").map_err(err)?;
            set_value(&src, "provide-clock", false).map_err(err)?;
            chain.push(src.clone());
            src
        }
    };
    if spec.is_video() {
        let convert = make("videoconvert", &name("convert")).map_err(err)?;
        chain.push(convert);
        chain.push(
            capsfilter(&name("format"), "video/x-raw,format=I420,colorimetry=bt709")
                .map_err(err)?,
        );
    } else {
        chain.push(make("audioconvert", &name("convert")).map_err(err)?);
        if ctx.side.is_some() {
            let level = make("level", &name("level")).map_err(err)?;
            set_value(&level, "interval", 50_000_000u64).map_err(err)?;
            set_value(&level, "post-messages", true).map_err(err)?;
            chain.push(level);
        }
    }
    let tee = make("tee", &name("tee")).map_err(err)?;
    set_value(&tee, "allow-not-linked", true).map_err(err)?;
    chain.push(tee.clone());
    let sink = make("fakesink", &name("sink")).map_err(err)?;
    set_value(&sink, "sync", false).map_err(err)?;
    set_value(&sink, "async", false).map_err(err)?;

    pipeline
        .add_many(chain.iter().chain(std::iter::once(&sink)))
        .map_err(|e| err(format!("can't build the source branch: {e}")))?;
    gst::Element::link_many(chain.iter())
        .map_err(|e| err(format!("can't link the source branch: {e}")))?;
    tee.link(&sink)
        .map_err(|e| err(format!("can't link the source branch: {e}")))?;

    if spec.is_video() {
        // Report the negotiated size (preview placement needs it).
        let tx = ctx.tx.clone();
        let generation = ctx.generation;
        if let Some(pad) = tee.static_pad("sink") {
            pad.connect_notify(Some("caps"), move |pad, _| {
                if let Some(caps) = pad.current_caps()
                    && let Some(s) = caps.structure(0)
                    && let (Ok(w), Ok(h)) = (s.get::<i32>("width"), s.get::<i32>("height"))
                {
                    let _ = tx.send(Cmd::SourceSize {
                        generation,
                        source: index,
                        width: w.max(0) as u32,
                        height: h.max(0) as u32,
                    });
                }
            });
        }
    }
    Ok(SourceBranch {
        spec: spec.clone(),
        prefix,
        src,
        tee,
        size: None,
        dead: None,
        keepalive,
    })
}

/// `upstream… ! appsink` → [`bridge::bridge`] → `appsrc`: adds and links
/// `upstream` (a screen's elements) and the appsink, starts the bridge
/// and returns the appsrc, where the rest of the branch starts, with the
/// bridge's keepalive.
fn bridged(
    pipeline: &gst::Pipeline,
    upstream: &[&gst::Element],
    name: &dyn Fn(&str) -> String,
) -> Result<(gst::Element, bridge::Keepalive), String> {
    let sink = gst_app::AppSink::builder()
        .name(name("bridge-sink"))
        .sync(false)
        // The pipeline plays without waiting for a first frame (a screen
        // that sends none fails the first-buffer check, naming it).
        .async_(false)
        .max_buffers(2)
        .drop(true)
        // The bridge reads each frame and lets go of it at once.
        .enable_last_sample(false)
        .build();
    let feed = gst_app::AppSrc::builder()
        .name(name("bridge-src"))
        .is_live(true)
        .format(gst::Format::Time)
        .do_timestamp(false)
        // Two frames, whatever their size; the oldest goes first.
        .max_buffers(2)
        .max_bytes(0)
        .leaky_type(gst_app::AppLeakyType::Downstream)
        .build();
    let elements: Vec<&gst::Element> = upstream
        .iter()
        .copied()
        .chain([sink.upcast_ref::<gst::Element>()])
        .collect();
    pipeline
        .add_many(elements.iter().copied())
        .map_err(|e| format!("can't build the screen branch: {e}"))?;
    gst::Element::link_many(elements.iter().copied())
        .map_err(|e| format!("can't link the screen branch: {e}"))?;
    // Dropping the thread's handle detaches it; it ends with the branch.
    let (keepalive, _thread) =
        bridge::bridge(&sink, &feed, bridge::KEEPALIVE_IDLE, name("keepalive"))?;
    Ok((feed.upcast(), keepalive))
}

/// Lets only the first buffer of `src` through (a damage-driven
/// compositor sends a still screen once).
fn first_buffer_only(src: &gst::Element) {
    let Some(pad) = src.static_pad("src") else {
        return;
    };
    let sent = std::sync::atomic::AtomicBool::new(false);
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
        if sent.swap(true, std::sync::atomic::Ordering::Relaxed) {
            gst::PadProbeReturn::Drop
        } else {
            gst::PadProbeReturn::Ok
        }
    });
}

/// `audiotestsrc is-live=true wave=ticks`: a tone at the start of every
/// period, digital silence in between.
fn fake_audio_source(name: &str, tone: &FakeTone) -> Result<gst::Element, String> {
    let src = make("audiotestsrc", name)?;
    set_value(&src, "is-live", true)?;
    let (periods, interval) = tick_settings(tone);
    set_str(&src, "wave", "ticks")?;
    set_value(&src, "freq", f64::from(tone.freq_hz))?;
    set_value(&src, "volume", 0.5f64)?;
    set_value(&src, "sine-periods-per-tick", periods)?;
    set_value(&src, "tick-interval", interval)?;
    // 10 ms buffers.
    set_value(&src, "samplesperbuffer", 480i32)?;
    Ok(src)
}

fn add_preview(live: &Live, scene: &LiveScene, ctx: &BuildCtx<'_>) -> Result<Preview, String> {
    let pipeline = &live.pipeline;
    let canvas = &scene.canvas;
    let raw = |extra: &str| {
        format!(
            "video/x-raw,width={},height={},framerate={}/{}{extra}",
            canvas.width, canvas.height, canvas.fps.num, canvas.fps.den
        )
    };
    // The canvas background, as the bottom compositor input.
    let background = make("videotestsrc", "pv-background")?;
    set_value(&background, "is-live", true)?;
    set_str(&background, "pattern", "solid-color")?;
    set_value(
        &background,
        "foreground-color",
        argb(canvas.background.as_str()),
    )?;
    let background_caps = capsfilter("pv-background-caps", &raw(""))?;
    let compositor = make("compositor", "pv-compositor")?;
    let out_caps = capsfilter("pv-caps", &raw(""))?;
    let convert = make("videoconvert", "pv-convert")?;
    let rgba = capsfilter("pv-rgba", "video/x-raw,format=RGBA")?;
    let appsink = gst_app::AppSink::builder()
        .name("pv-sink")
        .sync(false)
        .max_buffers(1)
        .drop(true)
        .build();
    let tail: [&gst::Element; 4] = [&compositor, &out_caps, &convert, &rgba];
    pipeline
        .add_many([&background, &background_caps])
        .map_err(|e| e.to_string())?;
    pipeline.add_many(tail).map_err(|e| e.to_string())?;
    pipeline
        .add(appsink.upcast_ref::<gst::Element>())
        .map_err(|e| e.to_string())?;
    gst::Element::link_many([&background, &background_caps]).map_err(|e| e.to_string())?;
    let bg_pad = compositor
        .request_pad_simple("sink_%u")
        .ok_or("the compositor has no input pads")?;
    set_value(&bg_pad, "zorder", 0u32)?;
    background_caps
        .static_pad("src")
        .ok_or("no background pad")?
        .link(&bg_pad)
        .map_err(|e| format!("{e:?}"))?;
    gst::Element::link_many(tail).map_err(|e| e.to_string())?;
    rgba.link(appsink.upcast_ref::<gst::Element>())
        .map_err(|e| e.to_string())?;

    if let Some(side) = ctx.side.cloned() {
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(frame) = preview_frame(&sample) {
                        let _ = side.send(SideEvent::Preview(frame));
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }

    let mut layers = Vec::new();
    for (z, layer) in scene.scene.layers.iter().enumerate() {
        let wanted = SourceSpec::of_video(&layer.source, None);
        let Some(source) = live
            .sources
            .iter()
            .position(|b| b.spec.same_device(&wanted))
        else {
            continue;
        };
        let p = format!("pv{z}");
        let queue = make("queue", &format!("{p}-queue"))?;
        set_str(&queue, "leaky", "downstream")?;
        set_value(&queue, "max-size-buffers", 1u32)?;
        set_value(&queue, "max-size-bytes", 0u32)?;
        set_value(&queue, "max-size-time", 0u64)?;
        let crop = make("videocrop", &format!("{p}-crop"))?;
        pipeline
            .add_many([&queue, &crop])
            .map_err(|e| e.to_string())?;
        queue.link(&crop).map_err(|e| e.to_string())?;
        let tee_pad = live.sources[source]
            .tee
            .request_pad_simple("src_%u")
            .ok_or("the source can't feed the preview")?;
        tee_pad
            .link(&queue.static_pad("sink").ok_or("no queue pad")?)
            .map_err(|e| format!("{e:?}"))?;
        let pad = compositor
            .request_pad_simple("sink_%u")
            .ok_or("the compositor has no input pads")?;
        crop.static_pad("src")
            .ok_or("no crop pad")?
            .link(&pad)
            .map_err(|e| format!("{e:?}"))?;
        let pl = PreviewLayer {
            layer: layer.clone(),
            source,
            crop,
            pad,
        };
        apply_layer(&pl, layer, None, z as u32 + 1);
        layers.push(pl);
    }
    Ok(Preview { layers })
}

/// Sets one preview layer's crop and compositor pad from its layout.
fn apply_layer(pl: &PreviewLayer, layer: &Layer, size: Option<(u32, u32)>, zorder: u32) {
    let placement = size.and_then(|(w, h)| place(w, h, layer.crop, layer.rect, layer.fit));
    let (crop, dst) = match (size, placement) {
        (Some((w, h)), Some(p)) => (
            (
                p.src.x,
                p.src.y,
                w.saturating_sub(p.src.x + p.src.w),
                h.saturating_sub(p.src.y + p.src.h),
            ),
            p.dst,
        ),
        // Size not known yet: no crop, the whole rect.
        _ => ((0, 0, 0, 0), layer.rect),
    };
    let (left, top, right, bottom) = crop;
    for (name, value) in [
        ("left", left),
        ("top", top),
        ("right", right),
        ("bottom", bottom),
    ] {
        let _ = set_value(&pl.crop, name, value as i32);
    }
    let visible = layer.visible && (size.is_none() || placement.is_some());
    let _ = set_value(&pl.pad, "xpos", dst.x);
    let _ = set_value(&pl.pad, "ypos", dst.y);
    let _ = set_value(&pl.pad, "width", dst.w as i32);
    let _ = set_value(&pl.pad, "height", dst.h as i32);
    let _ = set_value(
        &pl.pad,
        "alpha",
        if visible {
            layer.opacity.clamp(0.0, 1.0)
        } else {
            0.0
        },
    );
    let _ = set_value(&pl.pad, "zorder", zorder);
}

/// `#RRGGBB` → `0xFFRRGGBB`.
fn argb(color: &str) -> u32 {
    let rgb = color
        .strip_prefix('#')
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        .unwrap_or(0);
    0xFF00_0000 | (rgb & 0x00FF_FFFF)
}

fn preview_frame(sample: &gst::Sample) -> Option<PreviewFrame> {
    let caps = sample.caps()?;
    let info = gst_video::VideoInfo::from_caps(caps).ok()?;
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;
    Some(PreviewFrame {
        width: info.width(),
        height: info.height(),
        stride: info.stride()[0].max(0) as u32,
        rgba: map.as_slice().to_vec(),
        pts_ns: buffer.pts().map(gst::ClockTime::nseconds),
    })
}

/// A `level` element message as a reading for `device`.
pub(super) fn level_reading(device: &str, s: &gst::StructureRef) -> Option<LevelReading> {
    let read = |field: &str| -> Option<Vec<f64>> {
        let list = s.get::<glib::ValueArray>(field).ok()?;
        Some(list.iter().filter_map(|v| v.get::<f64>().ok()).collect())
    };
    Some(LevelReading {
        device: device.to_owned(),
        rms_db: read("rms")?,
        peak_db: read("peak")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_become_argb() {
        assert_eq!(argb("#000000"), 0xFF00_0000);
        assert_eq!(argb("#12170F"), 0xFF12_170F);
        assert_eq!(argb("bogus"), 0xFF00_0000);
    }
}
