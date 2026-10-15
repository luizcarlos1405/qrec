//! Record bins (A§8.11, *measured*): one per recorded layer or track,
//! attached to its source's tee while a take records.
//!
//! - Video: `queue ! videorate skip-to-first=true ! video/x-raw,framerate=N/D
//!   ! videoconvert ! video/x-raw,format=I420|NV12,colorimetry=bt709 !
//!   <encoder> ! h264parse ! matroskamux offset-to-zero=true ! filesink`.
//! - Audio: `queue ! audioconvert ! audioresample ! audiorate
//!   skip-to-first=true ! audio/x-raw,rate=48000 ! flacenc ! matroskamux
//!   offset-to-zero=true ! filesink`.
//!
//! A data probe on the tee's src pad drops buffers whose running time is
//! before T0 and measures `startOffsetNs = first passed − T0`; after stop it
//! drops buffers at or after T1. It also moves the bin to *file time*: the
//! bin gets a plain `[0, ∞)` segment and every passed buffer's timestamps
//! minus the first passed running time, so each file starts at exactly 0.
//! (`matroskamux offset-to-zero` alone isn't enough *(measured)*: it takes
//! its offset from the first buffer queued when the header is written, and
//! for FLAC that is an untimestamped header buffer, so audio files started
//! at T0 + δ instead of 0. The property stays set; it's a no-op now.)
//! A probe on the filesink's pad reports the first write (for
//! `CaptureStarted`) and EOS (for `CaptureStopped`). Closing a bin unlinks
//! it from the tee in an IDLE probe, fills video up to T1 with copies of the
//! last frame (a static screen may not produce another one), pads audio
//! shorter than one FLAC block with silence (see [`MIN_AUDIO_NS`]), then
//! sends EOS into the bin.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crossbeam_channel::Sender;
use gst::prelude::*;
use qrec_core::Fps;
use qrec_core::model::StreamKind;
use qrec_core::plans::{AudioRecord, VideoRecord};

use super::engine::{Cmd, ProbeEvent};
use super::spec::{encoder_input_format, encoder_settings, frame_duration_ns};
use super::util::{capsfilter, make, set_str, set_value};

/// No more than this many fill frames are pushed when closing a bin (an
/// hour at 60 fps; a longer stall means the source is gone anyway).
const MAX_FILL_FRAMES: u64 = 216_000;

/// The shortest audio stream a bin closes with *(measured)*. flacenc sets
/// its output caps with its first block (4608 samples, 96 ms at 48 kHz); a
/// stream that ends before that makes GStreamer push the segment before
/// the caps (warnings from flacenc and matroskamux; the file is still
/// valid). A take that short is too short or cancelled anyway, so its
/// audio is padded with silence up to this length.
const MIN_AUDIO_NS: u64 = 120_000_000;

/// What the streaming threads share with the engine about one bin.
pub(super) struct RecProbe {
    pub index: usize,
    pub kind: StreamKind,
    /// Running time of the take start; `u64::MAX` drops everything.
    pub t0: AtomicU64,
    /// Running time of the stop; `u64::MAX` while recording.
    pub t1: AtomicU64,
    /// The filesink got its first buffer.
    pub written: AtomicBool,
    pub state: Mutex<ProbeState>,
}

#[derive(Default)]
pub(super) struct ProbeState {
    /// The source's segment (running times are computed in it).
    pub segment: Option<gst::FormattedSegment<gst::ClockTime>>,
    /// Running time of the first passed buffer.
    pub first: Option<u64>,
    /// Running time of the last passed buffer.
    pub last: Option<u64>,
    /// The last passed video buffer (for filling to T1).
    pub last_buffer: Option<gst::Buffer>,
    /// Running time where the last passed audio buffer ends (for padding
    /// a stream shorter than one FLAC block).
    pub audio_end: Option<u64>,
    /// A buffer at or after T1 arrived; everything is dropped now.
    pub reached_end: bool,
    pub passed: u64,
}

impl RecProbe {
    pub fn new(index: usize, kind: StreamKind) -> Arc<Self> {
        Arc::new(Self {
            index,
            kind,
            t0: AtomicU64::new(u64::MAX),
            t1: AtomicU64::new(u64::MAX),
            written: AtomicBool::new(false),
            state: Mutex::new(ProbeState::default()),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, ProbeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The running time of `pts` in the current segment.
    fn running(&self, state: &ProbeState, pad: &gst::Pad, pts: gst::ClockTime) -> u64 {
        let from_segment = |seg: &gst::FormattedSegment<gst::ClockTime>| seg.to_running_time(pts);
        if let Some(seg) = &state.segment {
            return from_segment(seg).unwrap_or(pts).nseconds();
        }
        pad.sticky_event::<gst::event::Segment>(0)
            .and_then(|ev| ev.segment().clone().downcast::<gst::ClockTime>().ok())
            .and_then(|seg| from_segment(&seg))
            .unwrap_or(pts)
            .nseconds()
    }

    /// Decides one buffer's fate and moves it to file time (streaming thread).
    fn on_buffer(
        &self,
        pad: &gst::Pad,
        buffer: &mut gst::Buffer,
        tx: &Sender<Cmd>,
        tag: u64,
    ) -> gst::PadProbeReturn {
        let Some(pts) = buffer.pts() else {
            return gst::PadProbeReturn::Drop;
        };
        let mut state = self.lock();
        if state.reached_end {
            return gst::PadProbeReturn::Drop;
        }
        let running = self.running(&state, pad, pts);
        let t0 = self.t0.load(Ordering::SeqCst);
        if t0 == u64::MAX || running < t0 {
            return gst::PadProbeReturn::Drop;
        }
        if running >= self.t1.load(Ordering::SeqCst) {
            state.reached_end = true;
            drop(state);
            let _ = tx.send(Cmd::Probe(tag, ProbeEvent::ReachedEnd(self.index)));
            return gst::PadProbeReturn::Drop;
        }
        let first = match state.first {
            Some(first) => first,
            None => {
                state.first = Some(running);
                let _ = tx.send(Cmd::Probe(tag, ProbeEvent::FirstPass(self.index)));
                running
            }
        };
        state.last = Some(running);
        state.passed += 1;
        let dts = buffer
            .dts()
            .map(|dts| self.running(&state, pad, dts).saturating_sub(first));
        {
            // Copy-on-write of the metadata only; the other tee branches keep theirs.
            let b = buffer.make_mut();
            b.set_pts(gst::ClockTime::from_nseconds(running - first));
            b.set_dts(dts.map(gst::ClockTime::from_nseconds));
        }
        match self.kind {
            StreamKind::Video => state.last_buffer = Some(buffer.clone()),
            StreamKind::Audio => {
                let duration = buffer.duration().map_or(0, gst::ClockTime::nseconds);
                state.audio_end = Some(running + duration);
            }
        }
        gst::PadProbeReturn::Ok
    }

    /// Passes the bin one `[0, ∞)` segment (file time) and remembers the
    /// source's segment for running-time math (streaming thread).
    ///
    /// Whether the bin already has its segment is read from the sticky
    /// events of the peer (the bin's ghost pad), not remembered here: the
    /// tee may push on its new pad before the bin is linked, and that push
    /// fails, so the segment is sent again once the link exists.
    fn on_segment(&self, pad: &gst::Pad, info: &mut gst::PadProbeInfo) -> gst::PadProbeReturn {
        let Some(gst::PadProbeData::Event(ev)) = &info.data else {
            return gst::PadProbeReturn::Ok;
        };
        let seg = match ev.view() {
            gst::EventView::Segment(seg) => seg,
            // A gap carries a source running time, not file time: audiorate
            // would fill silence up to it. The rate elements fill gaps anyway.
            gst::EventView::Gap(_) => return gst::PadProbeReturn::Drop,
            _ => return gst::PadProbeReturn::Ok,
        };
        let delivered = pad
            .peer()
            .is_some_and(|peer| peer.sticky_event::<gst::event::Segment>(0).is_some());
        if let Ok(original) = seg.segment().clone().downcast::<gst::ClockTime>() {
            self.lock().segment = Some(original);
        }
        if delivered {
            // The bin's timeline never restarts (a new segment would reset
            // videorate/audiorate mid-take).
            return gst::PadProbeReturn::Drop;
        }
        let zero = gst::FormattedSegment::<gst::ClockTime>::new();
        info.data = Some(gst::PadProbeData::Event(gst::event::Segment::new(&zero)));
        gst::PadProbeReturn::Ok
    }
}

/// One attached record bin.
pub(super) struct RecordBin {
    /// The layer or track id (`StreamLost.stream`, `RecordedStream.id`).
    pub id: String,
    pub kind: StreamKind,
    /// Project-relative file.
    pub file: String,
    /// Human name for errors (`L1 screen DP-4 (…)`).
    pub label: String,
    /// Index of the source branch in the live pipeline.
    pub source: usize,
    pub bin: gst::Bin,
    pub tee: gst::Element,
    pub tee_pad: Option<gst::Pad>,
    pub sink_pad: gst::Pad,
    pub probe: Arc<RecProbe>,
    /// EOS reached the filesink.
    pub eos: bool,
    /// An unlink was requested (IDLE probe installed).
    pub closing: bool,
    /// The bin is unlinked from the tee; fill/EOS were sent.
    pub closed: bool,
    /// Why the stream was lost while recording.
    pub lost: Option<String>,
}

impl RecordBin {
    pub fn written(&self) -> bool {
        self.probe.written.load(Ordering::SeqCst)
    }

    pub fn first_running(&self) -> Option<u64> {
        self.probe.lock().first
    }

    /// Unlinks the bin from its tee as soon as the tee pad is idle (no
    /// buffer in flight), then reports `Unlinked`. Idempotent.
    pub fn begin_close(&mut self, tx: &Sender<Cmd>, tag: u64) {
        if self.closing {
            return;
        }
        self.closing = true;
        let Some(tee_pad) = self.tee_pad.clone() else {
            let _ = tx.send(Cmd::Probe(tag, ProbeEvent::Unlinked(self.probe.index)));
            return;
        };
        let sink_pad = self.sink_pad.clone();
        let tx = tx.clone();
        let index = self.probe.index;
        tee_pad.add_probe(gst::PadProbeType::IDLE, move |pad, _info| {
            let _ = pad.unlink(&sink_pad);
            let _ = tx.send(Cmd::Probe(tag, ProbeEvent::Unlinked(index)));
            gst::PadProbeReturn::Remove
        });
    }

    /// Unlinks right away (failed starts: the file is abandoned anyway).
    pub fn unlink_now(&mut self) {
        self.closing = true;
        if let Some(tee_pad) = &self.tee_pad {
            let _ = tee_pad.unlink(&self.sink_pad);
        }
    }

    /// Gives the tee its request pad back (engine thread, after unlinking).
    pub fn release_tee_pad(&mut self) {
        if let Some(pad) = self.tee_pad.take() {
            self.tee.release_request_pad(&pad);
        }
    }

    /// After `Unlinked`: fills video up to T1 with copies of the last frame
    /// (one per frame slot of `fps`), then sends EOS into the bin.
    /// Returns the number of fill frames pushed.
    pub fn finish_close(&mut self, fps: Fps, fill: bool) -> u64 {
        self.closed = true;
        let mut pushed = 0;
        if fill && self.kind == StreamKind::Video {
            let t1 = self.probe.t1.load(Ordering::SeqCst);
            let (first, last, buffer) = {
                let state = self.probe.lock();
                (state.first, state.last, state.last_buffer.clone())
            };
            if let (Some(first), Some(last), Some(buffer), true) =
                (first, last, buffer, t1 != u64::MAX)
            {
                let frame = frame_duration_ns(fps);
                let mut running = last + frame;
                while running < t1 && pushed < MAX_FILL_FRAMES {
                    // File time, like every buffer the probe passed.
                    let pts = gst::ClockTime::from_nseconds(running - first);
                    let mut copy = buffer.copy();
                    {
                        let copy = copy.make_mut();
                        copy.set_pts(pts);
                        copy.set_dts(gst::ClockTime::NONE);
                        copy.set_duration(gst::ClockTime::from_nseconds(frame));
                    }
                    if self.sink_pad.chain(copy).is_err() {
                        break;
                    }
                    pushed += 1;
                    running += frame;
                }
            }
        }
        if self.kind == StreamKind::Audio {
            pushed += self.pad_short_audio();
        }
        let _ = self.sink_pad.send_event(gst::event::Eos::new());
        pushed
    }

    /// Pads an audio stream shorter than [`MIN_AUDIO_NS`] with silence in
    /// its input format, right after its last passed buffer (file time).
    /// Returns the number of buffers pushed (0 or 1).
    fn pad_short_audio(&mut self) -> u64 {
        let (first, end) = {
            let state = self.probe.lock();
            (state.first, state.audio_end)
        };
        let (Some(first), Some(end)) = (first, end) else {
            // Nothing passed: flacenc never starts, nothing to pad.
            return 0;
        };
        let have = end.saturating_sub(first);
        if have >= MIN_AUDIO_NS {
            return 0;
        }
        let Some(info) = self
            .sink_pad
            .current_caps()
            .and_then(|caps| gst_audio::AudioInfo::from_caps(&caps).ok())
        else {
            return 0;
        };
        let missing = MIN_AUDIO_NS - have;
        let frames = (u128::from(missing) * u128::from(info.rate())).div_ceil(1_000_000_000);
        let Ok(frames) = usize::try_from(frames) else {
            return 0;
        };
        let Ok(mut buffer) = gst::Buffer::with_size(frames * info.bpf() as usize) else {
            return 0;
        };
        {
            let Some(b) = buffer.get_mut() else { return 0 };
            b.set_pts(gst::ClockTime::from_nseconds(have));
            b.set_duration(gst::ClockTime::from_nseconds(missing));
            let Ok(mut map) = b.map_writable() else {
                return 0;
            };
            info.format_info().fill_silence(map.as_mut_slice());
        }
        u64::from(self.sink_pad.chain(buffer).is_ok())
    }
}

/// Builds, adds and links a video record bin; buffers are dropped until
/// T0 is set on the probe.
#[allow(clippy::too_many_arguments)]
pub(super) fn attach_video(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    source: usize,
    index: usize,
    rec: &VideoRecord,
    fps: Fps,
    location: &Path,
    file: String,
    label: String,
    tx: &Sender<Cmd>,
    tag: u64,
) -> Result<RecordBin, String> {
    let p = format!("r{index}");
    let queue = make("queue", &format!("{p}-queue"))?;
    // Never block the source and the other branches: a slow encoder drops
    // old frames (videorate keeps the file CFR) instead of stalling the tee.
    // Up to 1 s of raw frames, but never more than 256 MiB (4K at 60 fps).
    set_value(&queue, "max-size-buffers", 0u32)?;
    set_value(&queue, "max-size-bytes", 256u32 << 20)?;
    set_value(&queue, "max-size-time", 1_000_000_000u64)?;
    set_str(&queue, "leaky", "downstream")?;
    let rate = make("videorate", &format!("{p}-rate"))?;
    set_value(&rate, "skip-to-first", true)?;
    let rate_caps = capsfilter(
        &format!("{p}-fps"),
        &format!("video/x-raw,framerate={}/{}", fps.num, fps.den),
    )?;
    let convert = make("videoconvert", &format!("{p}-convert"))?;
    let format_caps = capsfilter(
        &format!("{p}-format"),
        &format!(
            "video/x-raw,format={},colorimetry=bt709",
            encoder_input_format(&rec.encoder)
        ),
    )?;
    let (factory, props) = encoder_settings(rec);
    let encoder = make(&factory, &format!("{p}-encoder"))?;
    for (name, value) in &props {
        set_str(&encoder, name, value)?;
    }
    let parse = make("h264parse", &format!("{p}-parse"))?;
    let elements = [queue, rate, rate_caps, convert, format_caps, encoder, parse];
    attach(
        pipeline,
        tee,
        source,
        index,
        &rec.id.to_string(),
        StreamKind::Video,
        &elements,
        location,
        file,
        label,
        tx,
        tag,
    )
}

/// Builds, adds and links an audio record bin.
#[allow(clippy::too_many_arguments)]
pub(super) fn attach_audio(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    source: usize,
    index: usize,
    rec: &AudioRecord,
    location: &Path,
    file: String,
    label: String,
    tx: &Sender<Cmd>,
    tag: u64,
) -> Result<RecordBin, String> {
    let p = format!("r{index}");
    let queue = make("queue", &format!("{p}-queue"))?;
    set_value(&queue, "max-size-buffers", 0u32)?;
    set_value(&queue, "max-size-bytes", 0u32)?;
    set_value(&queue, "max-size-time", 2_000_000_000u64)?;
    set_str(&queue, "leaky", "downstream")?;
    let convert = make("audioconvert", &format!("{p}-convert"))?;
    let resample = make("audioresample", &format!("{p}-resample"))?;
    let rate = make("audiorate", &format!("{p}-rate"))?;
    set_value(&rate, "skip-to-first", true)?;
    let sample_rate = if rec.sample_rate == 0 {
        48_000
    } else {
        rec.sample_rate
    };
    let rate_caps = capsfilter(
        &format!("{p}-caps"),
        &format!("audio/x-raw,rate={sample_rate}"),
    )?;
    let encoder = make("flacenc", &format!("{p}-encoder"))?;
    let elements = [queue, convert, resample, rate, rate_caps, encoder];
    attach(
        pipeline,
        tee,
        source,
        index,
        &rec.id.to_string(),
        StreamKind::Audio,
        &elements,
        location,
        file,
        label,
        tx,
        tag,
    )
}

#[allow(clippy::too_many_arguments)]
fn attach(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    source: usize,
    index: usize,
    id: &str,
    kind: StreamKind,
    head: &[gst::Element],
    location: &Path,
    file: String,
    label: String,
    tx: &Sender<Cmd>,
    tag: u64,
) -> Result<RecordBin, String> {
    let p = format!("r{index}");
    let mux = make("matroskamux", &format!("{p}-mux"))?;
    set_value(&mux, "offset-to-zero", true)?;
    if mux.find_property("writing-app").is_some() {
        set_value(&mux, "writing-app", "Qrec")?;
    }
    let sink = make("filesink", &format!("{p}-sink"))?;
    set_value(&sink, "location", location.to_string_lossy().as_ref())?;
    // Writing a file needs neither clock sync nor preroll.
    set_value(&sink, "sync", false)?;
    set_value(&sink, "async", false)?;

    let bin = gst::Bin::with_name(&format!("rec{index}-{id}"));
    let mut chain: Vec<&gst::Element> = head.iter().collect();
    chain.push(&mux);
    chain.push(&sink);
    bin.add_many(chain.iter().copied())
        .map_err(|e| format!("can't build the record bin for {id}: {e}"))?;
    gst::Element::link_many(chain.iter().copied())
        .map_err(|e| format!("can't link the record bin for {id}: {e}"))?;
    let target = head[0]
        .static_pad("sink")
        .ok_or("the queue has no sink pad")?;
    let ghost = gst::GhostPad::builder_with_target(&target)
        .map_err(|e| format!("can't expose the record bin's input: {e}"))?
        .name("sink")
        .build();
    bin.add_pad(&ghost)
        .map_err(|e| format!("can't expose the record bin's input: {e}"))?;

    let probe = RecProbe::new(index, kind);
    // The filesink side: first write and EOS.
    let sink_pad = sink
        .static_pad("sink")
        .ok_or("the filesink has no sink pad")?;
    {
        let probe = probe.clone();
        let tx = tx.clone();
        sink_pad.add_probe(
            gst::PadProbeType::BUFFER
                | gst::PadProbeType::BUFFER_LIST
                | gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_pad, info| {
                match &info.data {
                    Some(gst::PadProbeData::Buffer(_)) | Some(gst::PadProbeData::BufferList(_)) => {
                        if !probe.written.swap(true, Ordering::SeqCst) {
                            let _ = tx.send(Cmd::Probe(tag, ProbeEvent::FirstWrite(probe.index)));
                        }
                    }
                    Some(gst::PadProbeData::Event(ev)) if ev.type_() == gst::EventType::Eos => {
                        let _ = tx.send(Cmd::Probe(tag, ProbeEvent::SinkEos(probe.index)));
                    }
                    _ => {}
                }
                gst::PadProbeReturn::Ok
            },
        );
    }

    pipeline
        .add(&bin)
        .map_err(|e| format!("can't add the record bin for {id}: {e}"))?;
    let tee_pad = match tee.request_pad_simple("src_%u") {
        Some(pad) => pad,
        None => {
            let _ = pipeline.remove(&bin);
            return Err(format!("the source of {id} can't feed another branch"));
        }
    };
    // The tee side: T0/T1 filtering, segment tracking.
    {
        let probe = probe.clone();
        let tx = tx.clone();
        tee_pad.add_probe(
            gst::PadProbeType::BUFFER
                | gst::PadProbeType::BUFFER_LIST
                | gst::PadProbeType::EVENT_DOWNSTREAM,
            move |pad, info| match &mut info.data {
                Some(gst::PadProbeData::Buffer(buffer)) => probe.on_buffer(pad, buffer, &tx, tag),
                Some(gst::PadProbeData::BufferList(list)) => {
                    // Rare for capture sources: judge the list's buffers one by one.
                    let mut kept = gst::BufferList::new();
                    for buffer in list.iter() {
                        let mut buffer = buffer.to_owned();
                        if probe.on_buffer(pad, &mut buffer, &tx, tag) == gst::PadProbeReturn::Ok {
                            kept.get_mut().expect("a new list is writable").add(buffer);
                        }
                    }
                    if kept.is_empty() {
                        gst::PadProbeReturn::Drop
                    } else {
                        *list = kept;
                        gst::PadProbeReturn::Ok
                    }
                }
                Some(gst::PadProbeData::Event(_)) => probe.on_segment(pad, info),
                _ => gst::PadProbeReturn::Ok,
            },
        );
    }
    let mut rec_bin = RecordBin {
        id: id.to_owned(),
        kind,
        file,
        label,
        source,
        bin: bin.clone(),
        tee: tee.clone(),
        tee_pad: Some(tee_pad.clone()),
        sink_pad: ghost.upcast(),
        probe,
        eos: false,
        closing: false,
        closed: false,
        lost: None,
    };
    if let Err(e) = bin.sync_state_with_parent() {
        rec_bin.release_tee_pad();
        let _ = bin.set_state(gst::State::Null);
        let _ = pipeline.remove(&bin);
        return Err(format!("the record bin for {id} can't start: {e}"));
    }
    if let Err(e) = tee_pad.link(&rec_bin.sink_pad) {
        rec_bin.release_tee_pad();
        let _ = bin.set_state(gst::State::Null);
        let _ = pipeline.remove(&bin);
        return Err(format!("can't connect the record bin for {id}: {e:?}"));
    }
    Ok(rec_bin)
}
