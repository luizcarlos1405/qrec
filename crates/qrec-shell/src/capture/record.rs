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
//! drops buffers at or after T1, except a stream's first buffer: a stop
//! that comes before it (a quick double-tap) still lets one buffer through,
//! because a bin closed empty writes a file nothing can read (then the
//! take is `tooShort`, not `broken`). It also moves the bin to *file time*: the
//! bin gets a plain `[0, ∞)` segment and every passed buffer's timestamps
//! minus the first passed running time, so each file starts at exactly 0.
//! (`matroskamux offset-to-zero` alone isn't enough *(measured)*: it takes
//! its offset from the first buffer queued when the header is written, and
//! for FLAC that is an untimestamped header buffer, so audio files started
//! at T0 + δ instead of 0. The property stays set; it's a no-op now.)
//! A buffer about to reach a bin that has no segment yet gets the bin's
//! zero segment pushed first (and the stream start and caps if those are
//! missing too), with one `recorder.log` line ([`segment_pushed_line`]):
//! once, in a stop-and-cancel while starting, a buffer reached the audio
//! bin before any segment and the file came out unreadable (not
//! reproduced). A probe on the filesink's pad reports the first write (for
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
use qrec_core::capture_plan::FLAC_BLOCK_SAMPLES;
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
    /// The bin's segment was pushed by [`RecProbe::ensure_segment`] (it
    /// came in without one); logged once.
    pub pushed_segment: AtomicBool,
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
            pushed_segment: AtomicBool::new(false),
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
        let at_end = running >= self.t1.load(Ordering::SeqCst);
        if at_end && state.first.is_some() {
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
        if at_end {
            // The stop came before this stream's first buffer (a quick
            // double-tap). A bin closed before any buffer writes a file
            // nothing can read, so the first buffer at or after T0 always
            // passes, and it is also the last.
            state.reached_end = true;
        }
        drop(state);
        self.ensure_segment(pad, tx, tag);
        if at_end {
            let _ = tx.send(Cmd::Probe(tag, ProbeEvent::ReachedEnd(self.index)));
        }
        gst::PadProbeReturn::Ok
    }

    /// A buffer passes to the bin (the tee's pad, its peer the bin's ghost
    /// pad): one that would get there before any segment gets the bin's
    /// `[0, ∞)` segment pushed first ([`Self::on_segment`] would have passed
    /// that one), with the stream start and the caps if those are missing
    /// too, so the file stays valid; the first time, the take's log says so
    /// (`ProbeEvent::SegmentPushed`). Streaming thread, the probe's state
    /// unlocked.
    fn ensure_segment(&self, pad: &gst::Pad, tx: &Sender<Cmd>, tag: u64) {
        let Some(peer) = pad.peer() else {
            return;
        };
        if peer.sticky_event::<gst::event::Segment>(0).is_some() {
            return;
        }
        if peer.sticky_event::<gst::event::StreamStart>(0).is_none() {
            let start = pad
                .sticky_event::<gst::event::StreamStart>(0)
                .map(gst::Event::from)
                .unwrap_or_else(|| {
                    gst::event::StreamStart::new(&format!("qrec-record-{}", self.index))
                });
            let _ = peer.send_event(start);
        }
        if peer.current_caps().is_none()
            && let Some(caps) = pad.current_caps()
        {
            let _ = peer.send_event(gst::event::Caps::new(&caps));
        }
        let zero = gst::FormattedSegment::<gst::ClockTime>::new();
        let _ = peer.send_event(gst::event::Segment::new(&zero));
        if !self.pushed_segment.swap(true, Ordering::SeqCst) {
            let _ = tx.send(Cmd::Probe(tag, ProbeEvent::SegmentPushed(self.index)));
        }
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

/// The `recorder.log` line of a bin whose segment
/// [`RecProbe::ensure_segment`] pushed: `<id>: no segment before the first
/// buffer; pushed one`.
pub(super) fn segment_pushed_line(id: &str) -> String {
    format!("{id}: no segment before the first buffer; pushed one")
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

    /// `startOffsetNs` (SPEC §5.2): the running time of the first passed
    /// buffer minus T0; 0 before one passed.
    pub fn start_offset_ns(&self, t0: u64) -> i64 {
        self.first_running().map_or(0, |first| {
            i64::try_from(first.saturating_sub(t0)).unwrap_or(i64::MAX)
        })
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
    // Every frame but the last of a file that ends normally holds this many
    // samples, so a file cut short by a crash (no duration) is as long as
    // its packets say (A§2.2). It is flacenc's default; pinned here.
    set_value(&encoder, "blocksize", FLAC_BLOCK_SAMPLES)?;
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
                    // Only the first write is reported.
                    Some(gst::PadProbeData::Buffer(_)) | Some(gst::PadProbeData::BufferList(_))
                        if !probe.written.swap(true, Ordering::SeqCst) =>
                    {
                        let _ = tx.send(Cmd::Probe(tag, ProbeEvent::FirstWrite(probe.index)));
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
    // Without a RECONFIGURE upstream: the tee gives its new pad the
    // source's sticky caps, and the record bin takes any. One would make the
    // source query its caps again at the take's start, through every
    // preview layer's `videocrop`, and a crop that changes meanwhile (two
    // caps transforms, each with another crop) leaves it no caps: the
    // source dies "not-negotiated", and the take with it.
    if let Err(e) = tee_pad.link_full(
        &rec_bin.sink_pad,
        gst::PadLinkCheck::DEFAULT | gst::PadLinkCheck::NO_RECONFIGURE,
    ) {
        rec_bin.release_tee_pad();
        let _ = bin.set_state(gst::State::Null);
        let _ = pipeline.remove(&bin);
        return Err(format!("can't connect the record bin for {id}: {e:?}"));
    }
    Ok(rec_bin)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use qrec_core::model::VideoSource;
    use qrec_core::plans::VideoEncoder;

    use super::*;
    use crate::gst_init::testing::{assert_quiet_since, problems, serial};
    use crate::testkit::TestEnv;

    /// A take's record bin joins a running source's tee without asking the
    /// source to negotiate again (no RECONFIGURE upstream), and still gets
    /// the source's caps: a new caps query would cross every preview
    /// layer's `videocrop` while the engine may change its crop (G§3.9).
    #[test]
    fn a_record_bin_joins_its_tee_without_a_reconfigure() {
        let _serial = serial();
        let quiet = problems();
        crate::gst_init::init().expect("GStreamer");
        let env = TestEnv::new();
        let dir = env.empty_dir("attach");
        let pipeline = gst::Pipeline::new();
        let src = make("videotestsrc", "s0-src").unwrap();
        set_value(&src, "is-live", true).unwrap();
        let caps = capsfilter(
            "s0-caps",
            "video/x-raw,format=I420,width=320,height=240,framerate=30/1,colorimetry=bt709",
        )
        .unwrap();
        let tee = make("tee", "s0-tee").unwrap();
        set_value(&tee, "allow-not-linked", true).unwrap();
        let sink = make("fakesink", "s0-sink").unwrap();
        set_value(&sink, "sync", false).unwrap();
        set_value(&sink, "async", false).unwrap();
        pipeline.add_many([&src, &caps, &tee, &sink]).unwrap();
        gst::Element::link_many([&src, &caps, &tee, &sink]).unwrap();
        let reconfigures = Arc::new(AtomicUsize::new(0));
        let flowing = Arc::new(AtomicBool::new(false));
        let tee_sink = tee.static_pad("sink").unwrap();
        {
            let (reconfigures, flowing) = (Arc::clone(&reconfigures), Arc::clone(&flowing));
            tee_sink.add_probe(
                gst::PadProbeType::EVENT_UPSTREAM | gst::PadProbeType::BUFFER,
                move |_, info| {
                    match &info.data {
                        Some(gst::PadProbeData::Event(e))
                            if e.type_() == gst::EventType::Reconfigure =>
                        {
                            reconfigures.fetch_add(1, Ordering::SeqCst);
                        }
                        Some(gst::PadProbeData::Buffer(_)) => {
                            flowing.store(true, Ordering::SeqCst);
                        }
                        _ => {}
                    }
                    gst::PadProbeReturn::Ok
                },
            );
        }
        pipeline.set_state(gst::State::Playing).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !flowing.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the source never ran");
            std::thread::sleep(Duration::from_millis(5));
        }
        let before = reconfigures.load(Ordering::SeqCst);
        let rec = VideoRecord {
            id: "L1".parse().unwrap(),
            source: VideoSource::Screen {
                output: "FAKE-1".into(),
                label: "Fake FAKE-1".into(),
            },
            file: "L1-screen-FAKE-1.mkv".into(),
            encoder: VideoEncoder::X264 {
                pass: "qual".into(),
                quantizer: 18,
                speed_preset: "veryfast".into(),
                tune: "zerolatency".into(),
            },
            key_int_max: 30,
            camera_mode: None,
        };
        let (tx, _rx) = crossbeam_channel::unbounded();
        let bin = attach_video(
            &pipeline,
            &tee,
            0,
            0,
            &rec,
            Fps::new(30, 1),
            &dir.join(&rec.file),
            rec.file.clone(),
            "L1".into(),
            &tx,
            1,
        )
        .expect("the bin attaches");
        let deadline = Instant::now() + Duration::from_secs(10);
        while bin.sink_pad.current_caps().is_none() {
            assert!(
                Instant::now() < deadline,
                "the bin never got the source's caps"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            reconfigures.load(Ordering::SeqCst),
            before,
            "the take's bin made the source negotiate again"
        );
        let _ = pipeline.set_state(gst::State::Null);
        assert_quiet_since(quiet);
    }

    /// Waits (10 s) for `want` on the probe channel; returns what came.
    fn wait_for(rx: &crossbeam_channel::Receiver<Cmd>, want: ProbeEvent) -> Vec<ProbeEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(Cmd::Probe(_, event)) => {
                    let done = event == want;
                    seen.push(event);
                    if done {
                        return seen;
                    }
                }
                Ok(_) => {}
                Err(_) => panic!("no {want:?}; got {seen:?}"),
            }
        }
    }

    /// A buffer that would reach a record bin before any segment gets the
    /// bin's zero segment pushed first (once, a stop-and-cancel while
    /// starting gave the audio bin one, with GStreamer's "data flow before
    /// segment" warnings, and its file came out unreadable): no warning, a
    /// file that probes the way finalization reads it, and one
    /// `SegmentPushed` (its `recorder.log` line). Here a probe after the
    /// record probe drops the segment on its way to the bin.
    #[test]
    fn a_record_bin_gets_its_segment_before_a_buffer_without_one() {
        use qrec_core::media_argv::probe_argv;
        use qrec_core::parse::ffprobe::parse_ffprobe;
        use qrec_core::plans::AudioRecord;

        let _serial = serial();
        let quiet = problems();
        crate::gst_init::init().expect("GStreamer");
        let env = TestEnv::new();
        let dir = env.empty_dir("no-segment");
        let pipeline = gst::Pipeline::new();
        let caps = gst::Caps::builder("audio/x-raw")
            .field("format", "S16LE")
            .field("rate", 48_000i32)
            .field("channels", 1i32)
            .field("layout", "interleaved")
            .build();
        let src = gst_app::AppSrc::builder()
            .name("s0-src")
            .caps(&caps)
            .format(gst::Format::Time)
            .build();
        let tee = make("tee", "s0-tee").unwrap();
        set_value(&tee, "allow-not-linked", true).unwrap();
        pipeline.add_many([src.upcast_ref(), &tee]).unwrap();
        src.link(&tee).unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let rec = AudioRecord {
            id: "A1".parse().unwrap(),
            device: "fake".into(),
            file: "A1-mic.mka".into(),
            sample_rate: 48_000,
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut bin = attach_audio(
            &pipeline,
            &tee,
            0,
            0,
            &rec,
            &dir.join(&rec.file),
            rec.file.clone(),
            "A1".into(),
            &tx,
            7,
        )
        .expect("the bin attaches");
        // The segment never reaches the bin.
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let drops = Arc::clone(&drops);
            bin.tee_pad.as_ref().unwrap().add_probe(
                gst::PadProbeType::EVENT_DOWNSTREAM,
                move |_, info| match &info.data {
                    Some(gst::PadProbeData::Event(e)) if e.type_() == gst::EventType::Segment => {
                        drops.fetch_add(1, Ordering::SeqCst);
                        gst::PadProbeReturn::Drop
                    }
                    _ => gst::PadProbeReturn::Ok,
                },
            );
        }
        bin.probe.t0.store(0, Ordering::SeqCst);
        // 200 ms of silence in 20 ms buffers from 0.
        for i in 0..10u64 {
            let mut buffer = gst::Buffer::with_size(960 * 2).unwrap();
            {
                let b = buffer.get_mut().unwrap();
                b.set_pts(gst::ClockTime::from_mseconds(20 * i));
                b.set_duration(gst::ClockTime::from_mseconds(20));
            }
            src.push_buffer(buffer).expect("push a buffer");
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while bin.probe.lock().passed < 10 {
            assert!(
                Instant::now() < deadline,
                "the buffers never reached the bin"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            drops.load(Ordering::SeqCst) > 0,
            "the segment was never dropped"
        );
        // Closed like the engine closes it: unlinked when idle, then EOS.
        bin.begin_close(&tx, 7);
        let mut seen = wait_for(&rx, ProbeEvent::Unlinked(0));
        bin.release_tee_pad();
        bin.finish_close(Fps::new(30, 1), false);
        seen.extend(wait_for(&rx, ProbeEvent::SinkEos(0)));
        let _ = pipeline.set_state(gst::State::Null);
        let pushed = seen
            .iter()
            .filter(|e| **e == ProbeEvent::SegmentPushed(0))
            .count();
        assert_eq!(pushed, 1, "{seen:?}");
        assert_eq!(
            segment_pushed_line(&bin.id),
            "A1: no segment before the first buffer; pushed one"
        );
        // The file reads the way finalization reads it.
        let argv = probe_argv(&rec.file, StreamKind::Audio);
        let out = crate::testkit::run_tool(
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .current_dir(&dir),
            Duration::from_secs(30),
        );
        assert!(out.status.success(), "unreadable: {}", out.stderr);
        let info = parse_ffprobe(&String::from_utf8_lossy(&out.stdout), StreamKind::Audio)
            .expect("an audio file");
        assert!(info.duration_ns >= Some(190_000_000), "{info:?}");
        assert_quiet_since(quiet);
    }
}
