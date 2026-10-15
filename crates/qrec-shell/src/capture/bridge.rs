//! The screen branch's bridge (A§8.11): `… ! appsink` → `appsrc ! …`.
//! Screencast frames go in; I420 frames come out, at least one per frame
//! period.
//!
//! - **Sized memories.** niri sets the PipeWire data size of a DMA-BUF frame
//!   to 1, so `pipewiresrc` (PipeWire 1.6) wraps each frame in a 1-byte
//!   dmabuf memory. GL's DMA-BUF import then can't find the plane ("could
//!   not find memory 0") and its CPU fallback maps one page and reads the
//!   whole frame past it (SIGSEGV). [`resize_dmabufs`] swaps each such
//!   memory for one over a dup of the same fd with the buffer object's real
//!   size (`lseek(fd, 0, SEEK_END)`), keeping its offset, so the video
//!   meta's plane offsets still hold.
//! - **Converted here.** Each frame becomes I420 (bt709) in the callback,
//!   while pipewiresrc still holds it: the compositor gets the buffer back
//!   once it is read, never while it is still being read, and repeating a
//!   frame costs nothing. A 2560×1440 frame converts in ~4.5 ms on one
//!   thread (more threads don't help) *(measured)*.
//! - **Keepalive.** Screencasts are damage-driven: a still screen sends no
//!   frames. `pipewiresrc keepalive-time` can't stand in: PipeWire 1.6
//!   stamps its keepalive buffer with the absolute clock time, and basesrc
//!   waits for that time (hundreds of hours) before pushing it, so the
//!   branch stalls for good the first time the screen is still
//!   *(measured)*. Instead a thread pushes the last frame again, stamped
//!   with the running time, whenever no frame came for a frame period: a
//!   record bin gets its first frame within a frame of T0 and its last
//!   within a frame of T1, however still the screen is.

use std::os::fd::{BorrowedFd, RawFd};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gst::glib;
use gst::prelude::*;
use gst_allocators::prelude::*;
use gst_allocators::{DmaBufAllocator, DmaBufMemory};

use super::util::running_time;

/// What a screencast may negotiate (set on the capsfilter after linking:
/// DMA_DRM caps fail the link-time checks). niri offers DMA-BUF only, and
/// GL import of its frames samples black, so only **linear** DMA-BUF is
/// taken (CPU-mappable, single plane), plus shared memory for compositors
/// that offer it (GNOME, KDE).
pub(super) const SCREEN_CAPS: &str = "video/x-raw(memory:DMABuf),format=DMA_DRM,\
     drm-format={XR24,AR24,XB24,AB24}; video/x-raw,format={BGRx,BGRA,RGBx,RGBA,xRGB,ARGB}";

/// The colorimetry of the frames the bridge hands on (what every video
/// source branch converts to).
const OUTPUT_COLORIMETRY: &str = "bt709";

/// Converted frames kept ready for reuse.
const POOL_MIN_BUFFERS: u32 = 2;

/// Connects `sink` (the end of the screencast's part of the branch) to
/// `src` (the start of the rest): each frame is converted to I420 and
/// pushed on with its timestamps; when none came for `period`, the last
/// one is pushed again ([module docs](self)). EOS is passed on. The
/// keepalive runs on a thread named `thread` (returned; dropping the
/// handle detaches it), which ends at EOS or when `src` is gone.
pub(super) fn bridge(
    sink: &gst_app::AppSink,
    src: &gst_app::AppSrc,
    period: Duration,
    thread: String,
) -> Result<JoinHandle<()>, String> {
    let shared = Arc::new(Shared::default());
    let keepalive = {
        let target = src.downgrade();
        let shared = shared.clone();
        std::thread::Builder::new()
            .name(thread)
            .spawn(move || keepalive(&target, &shared, period))
            .map_err(|e| format!("can't start the screen's keepalive: {e}"))?
    };
    let allocator = DmaBufAllocator::new();
    let mut converter: Option<Converter> = None;
    let target = src.downgrade();
    let eos = (src.downgrade(), shared.clone());
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let src = target.upgrade().ok_or(gst::FlowError::Flushing)?;
                let (frame, caps) = convert(&sample, &mut converter, &allocator).map_err(
                    |failure| match failure {
                        Failure::Format(e) => {
                            gst::element_error!(src, gst::CoreError::Negotiation, ("{}", e));
                            gst::FlowError::NotNegotiated
                        }
                        Failure::Read(e) => {
                            gst::element_error!(src, gst::ResourceError::Read, ("{}", e));
                            gst::FlowError::Error
                        }
                    },
                )?;
                // pipewiresrc gets its buffer back.
                drop(sample);
                shared.push(&src, caps.as_ref(), frame)
            })
            .eos(move |_| {
                let (target, shared) = &eos;
                shared.stop();
                if let Some(src) = target.upgrade() {
                    let _ = src.end_of_stream();
                }
            })
            .build(),
    );
    Ok(keepalive)
}

/// Why a frame couldn't be handed on.
enum Failure {
    /// Its format can't be read or converted.
    Format(String),
    /// It can't be mapped or written out.
    Read(String),
}

/// One screencast frame as an I420 frame with its timestamps, plus the
/// caps to set first when its format is new.
fn convert(
    sample: &gst::Sample,
    converter: &mut Option<Converter>,
    allocator: &DmaBufAllocator,
) -> Result<(gst::Buffer, Option<gst::Caps>), Failure> {
    let caps = sample
        .caps()
        .ok_or_else(|| Failure::Format("a screencast frame came without caps".into()))?;
    let buffer = sample
        .buffer()
        .ok_or_else(|| Failure::Read("a screencast sample came without a frame".into()))?;
    let mut new_caps = None;
    let converter = match converter {
        Some(current) if current.caps.as_ref() == caps => current,
        _ => {
            let fresh = Converter::new(caps).map_err(Failure::Format)?;
            new_caps = Some(fresh.output_caps.clone());
            converter.insert(fresh)
        }
    };
    let frame = if gst_video::is_dma_drm_caps(caps) {
        let mut sized = buffer.copy();
        resize_dmabufs(sized.make_mut(), allocator).map_err(Failure::Read)?;
        converter.convert(&sized)?
    } else {
        converter.convert(buffer)?
    };
    Ok((frame, new_caps))
}

/// Converts frames of one negotiated screencast format to I420.
struct Converter {
    /// The caps it was made for.
    caps: gst::Caps,
    input: gst_video::VideoInfo,
    output: gst_video::VideoInfo,
    output_caps: gst::Caps,
    converter: gst_video::VideoConverter,
    pool: gst::BufferPool,
}

impl Converter {
    fn new(caps: &gst::CapsRef) -> Result<Self, String> {
        let plain = system_caps(caps)?;
        let input = gst_video::VideoInfo::from_caps(&plain)
            .map_err(|e| format!("can't read the screencast format ({plain}): {e}"))?;
        let output = output_info(&input)?;
        let output_caps = output
            .to_caps()
            .map_err(|e| format!("can't describe the screen's frames: {e}"))?;
        let converter = gst_video::VideoConverter::new(&input, &output, None)
            .map_err(|e| format!("can't convert the screencast format ({plain}): {e}"))?;
        let pool = gst::BufferPool::new();
        let mut config = pool.config();
        let size = u32::try_from(output.size())
            .map_err(|_| format!("the screen's frames are too large ({plain})"))?;
        // No maximum: acquiring a buffer never waits for one to come back.
        config.set_params(Some(&output_caps), size, POOL_MIN_BUFFERS, 0);
        pool.set_config(config)
            .and_then(|()| pool.set_active(true))
            .map_err(|e| format!("can't set up the screen's frame pool: {e}"))?;
        Ok(Self {
            caps: caps.to_owned(),
            input,
            output,
            output_caps,
            converter,
            pool,
        })
    }

    /// `input` (mappable, in the format this was made for) as a pooled
    /// I420 frame with its timestamps.
    fn convert(&self, input: &gst::BufferRef) -> Result<gst::Buffer, Failure> {
        let mut output = self
            .pool
            .acquire_buffer(None)
            .map_err(|e| Failure::Read(format!("no buffer for a screen frame: {e:?}")))?;
        let out = output
            .get_mut()
            .ok_or_else(|| Failure::Read("a pooled screen frame isn't writable".into()))?;
        {
            let source = gst_video::VideoFrameRef::from_buffer_ref_readable(input, &self.input)
                .map_err(|e| Failure::Read(format!("can't read a screencast frame: {e}")))?;
            let mut target = gst_video::VideoFrameRef::from_buffer_ref_writable(out, &self.output)
                .map_err(|e| Failure::Read(format!("can't write a screen frame: {e}")))?;
            self.converter.frame_ref(&source, &mut target);
        }
        let out = output.get_mut().expect("still the only reference");
        out.set_pts(input.pts());
        out.set_dts(input.dts());
        out.set_duration(input.duration());
        Ok(output)
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        // Frames still downstream are freed instead of coming back.
        let _ = self.pool.set_active(false);
    }
}

/// I420 (bt709) at `input`'s size, rate and pixel aspect ratio.
fn output_info(input: &gst_video::VideoInfo) -> Result<gst_video::VideoInfo, String> {
    let colorimetry: gst_video::VideoColorimetry = OUTPUT_COLORIMETRY
        .parse()
        .map_err(|e| format!("bad output colorimetry: {e}"))?;
    gst_video::VideoInfo::builder(gst_video::VideoFormat::I420, input.width(), input.height())
        .fps(input.fps())
        .par(input.par())
        .colorimetry(&colorimetry)
        .build()
        .map_err(|e| format!("can't describe the screen's frames: {e}"))
}

/// What the callbacks and the keepalive thread share.
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    /// The last frame pushed (what the keepalive repeats).
    last: Option<gst::Buffer>,
    /// The PTS of the last push, repeats included: pushed PTS only grow.
    last_pts: Option<gst::ClockTime>,
    /// When the last frame or repeat was pushed.
    pushed_at: Option<Instant>,
    /// EOS was passed on: nothing more is pushed.
    stopped: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Pushes a converted frame (after `caps`, when its format is new) and
    /// keeps it for the keepalive. All pushes happen under the lock, so a
    /// repeat never lands between a frame and its caps or out of order.
    fn push(
        &self,
        src: &gst_app::AppSrc,
        caps: Option<&gst::Caps>,
        mut frame: gst::Buffer,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut state = self.lock();
        if state.stopped {
            return Err(gst::FlowError::Eos);
        }
        let stamped = frame
            .pts()
            .or_else(|| running_time(src).map(gst::ClockTime::from_nseconds));
        let pts = after(state.last_pts, stamped);
        {
            let frame = frame.make_mut();
            frame.set_pts(pts);
            frame.set_dts(pts);
        }
        if let Some(caps) = caps {
            src.set_caps(Some(caps));
        }
        state.last = Some(frame.clone());
        state.last_pts = pts.or(state.last_pts);
        state.pushed_at = Some(Instant::now());
        // The appsrc is leaky: a push never waits.
        src.push_buffer(frame)
    }

    /// No more pushes (EOS); wakes the keepalive so it ends.
    fn stop(&self) {
        let mut state = self.lock();
        state.stopped = true;
        state.last = None;
        drop(state);
        self.wake.notify_all();
    }
}

/// `pts`, or just after `last` when it isn't later (a repeat stamped while
/// a frame was on its way).
fn after(last: Option<gst::ClockTime>, pts: Option<gst::ClockTime>) -> Option<gst::ClockTime> {
    match (last, pts) {
        (Some(last), Some(pts)) if pts <= last => Some(last + gst::ClockTime::NSECOND),
        (_, pts) => pts,
    }
}

/// The keepalive thread: pushes the last frame again, stamped with the
/// running time, whenever nothing was pushed for `period`. Ends at EOS or
/// when `target` is gone. A repeat is a new buffer over the same memory.
fn keepalive(target: &glib::WeakRef<gst_app::AppSrc>, shared: &Shared, period: Duration) {
    let mut state = shared.lock();
    loop {
        if state.stopped {
            return;
        }
        let Some(src) = target.upgrade() else {
            return;
        };
        let now = Instant::now();
        if state.pushed_at.is_some_and(|at| now >= at + period) {
            if let (Some(last), Some(running)) = (state.last.clone(), running_time(&src)) {
                let pts = after(state.last_pts, Some(gst::ClockTime::from_nseconds(running)));
                let mut repeat = last.copy();
                {
                    let repeat = repeat.make_mut();
                    repeat.set_pts(pts);
                    repeat.set_dts(pts);
                    repeat.set_duration(gst::ClockTime::from_nseconds(
                        u64::try_from(period.as_nanos()).unwrap_or(u64::MAX),
                    ));
                    repeat.unset_flags(gst::BufferFlags::DISCONT);
                }
                state.last_pts = pts.or(state.last_pts);
                // Refused while flushing or stopping; the next frame or the
                // EOS sorts that out.
                let _ = src.push_buffer(repeat);
            }
            state.pushed_at = Some(now);
        }
        drop(src);
        let wait = state.pushed_at.map_or(period, |at| {
            (at + period).saturating_duration_since(Instant::now())
        });
        state = shared
            .wake
            .wait_timeout(state, wait.max(Duration::from_millis(1)))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// Plain system-memory caps for what a screencast negotiated: a linear
/// DMA_DRM format becomes its video format; other caps pass unchanged.
pub(super) fn system_caps(caps: &gst::CapsRef) -> Result<gst::Caps, String> {
    if !gst_video::is_dma_drm_caps(caps) {
        return Ok(caps.to_owned());
    }
    let info = gst_video::VideoInfoDmaDrm::from_caps(caps)
        .map_err(|e| format!("can't read the screencast format ({caps}): {e}"))?;
    let not_linear = || {
        format!(
            "the screencast's DMA-BUF layout isn't linear ({caps}); only linear frames are read"
        )
    };
    // DRM_FORMAT_MOD_LINEAR: other layouts are tiled or compressed.
    if info.modifier() != 0 {
        return Err(not_linear());
    }
    let video = info.to_video_info().map_err(|_| not_linear())?;
    video
        .to_caps()
        .map_err(|e| format!("can't describe the screencast format: {e}"))
}

/// Gives every dmabuf memory of `buffer` the size of its buffer object.
/// Memories that already cover it are left alone.
pub(super) fn resize_dmabufs(
    buffer: &mut gst::BufferRef,
    allocator: &DmaBufAllocator,
) -> Result<(), String> {
    for index in 0..buffer.n_memory() {
        let memory = buffer.peek_memory(index);
        let Some(dmabuf) = memory.downcast_memory_ref::<DmaBufMemory>() else {
            continue;
        };
        let fd = dmabuf.fd();
        let size = object_size(fd)?;
        let offset = memory.offset();
        if memory.maxsize() >= size || offset >= size {
            continue;
        }
        // SAFETY: the memory keeps `fd` open while we hold the buffer; the
        // dup is owned by the new memory, which closes it.
        let dup = unsafe { BorrowedFd::borrow_raw(fd) }
            .try_clone_to_owned()
            .map_err(|e| format!("can't dup a screencast dmabuf: {e}"))?;
        // SAFETY: `dup` is a dmabuf fd and `size` is its buffer object's size.
        let mut wider = unsafe { allocator.alloc_dmabuf(dup, size) }
            .map_err(|e| format!("can't wrap a screencast dmabuf: {e}"))?;
        wider
            .get_mut()
            .ok_or("a new dmabuf memory isn't writable")?
            .resize(offset..size);
        buffer.replace_memory(index, wider);
    }
    Ok(())
}

/// The size of the buffer object behind a dmabuf fd.
fn object_size(fd: RawFd) -> Result<usize, String> {
    // SAFETY: lseek on a valid fd; a dmabuf reports its size at SEEK_END and
    // its file position doesn't matter to anyone.
    let end = unsafe { libc::lseek(fd, 0, libc::SEEK_END) };
    usize::try_from(end).map_err(|_| {
        format!(
            "can't read a screencast dmabuf's size: {}",
            std::io::Error::last_os_error()
        )
    })
}

#[cfg(test)]
mod tests {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::str::FromStr;

    use super::*;

    fn caps(s: &str) -> gst::Caps {
        gst::Caps::from_str(s).unwrap()
    }

    #[test]
    fn linear_dma_drm_caps_become_their_plain_format() {
        gst::init().unwrap();
        let plain = system_caps(&caps(
            "video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format=XR24,width=2560,height=1440,framerate=0/1",
        ))
        .unwrap();
        let s = plain.structure(0).unwrap();
        assert_eq!(s.get::<&str>("format").unwrap(), "BGRx");
        assert_eq!(s.get::<i32>("width").unwrap(), 2560);
        assert!(
            !plain.features(0).unwrap().contains("memory:DMABuf"),
            "{plain}"
        );

        // A tiled layout can't be read on the CPU.
        let tiled = system_caps(&caps(
            "video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format=XR24:0x0100000000000009,width=64,height=64,framerate=0/1",
        ))
        .unwrap_err();
        assert!(tiled.contains("isn't linear"), "{tiled}");

        // Shared-memory caps pass unchanged.
        let shm = caps("video/x-raw,format=BGRx,width=64,height=64,framerate=0/1");
        assert_eq!(system_caps(&shm).unwrap(), shm);
    }

    /// niri's frames arrive as 1-byte dmabuf memories; a memfd stands in for
    /// the dmabuf (both are fd memories whose size lseek reports).
    #[test]
    fn a_one_byte_dmabuf_memory_gets_its_object_size() {
        gst::init().unwrap();
        let size = 64 * 1024;
        let allocator = DmaBufAllocator::new();
        // SAFETY: a memfd is an fd the dmabuf allocator can wrap.
        let tiny = unsafe { allocator.alloc_dmabuf(memfd(size), 1) }.unwrap();
        let mut buffer = gst::Buffer::new();
        buffer.get_mut().unwrap().append_memory(tiny);
        resize_dmabufs(buffer.get_mut().unwrap(), &allocator).unwrap();
        let memory = buffer.peek_memory(0);
        assert_eq!(memory.size(), size);
        assert!(memory.downcast_memory_ref::<DmaBufMemory>().is_some());
        // Already right: left alone.
        let before = buffer.peek_memory(0).as_ptr();
        resize_dmabufs(buffer.get_mut().unwrap(), &allocator).unwrap();
        assert_eq!(buffer.peek_memory(0).as_ptr(), before);
    }

    /// A memfd of `size` bytes.
    fn memfd(size: usize) -> OwnedFd {
        // SAFETY: plain syscalls; the fd is owned right after.
        unsafe {
            let fd = libc::memfd_create(c"qrec-dmabuf-test".as_ptr(), libc::MFD_CLOEXEC);
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            assert_eq!(libc::ftruncate(fd, size as libc::off_t), 0);
            OwnedFd::from_raw_fd(fd)
        }
    }

    /// `input ! appsink` → bridge → `appsrc ! out`, playing on the system
    /// clock. Neither sink waits for preroll, like the live pipeline's.
    struct Rig {
        pipeline: gst::Pipeline,
        input: gst_app::AppSrc,
        out: gst_app::AppSink,
    }

    /// Whether `done` holds within `limit`.
    fn within(limit: Duration, done: impl Fn() -> bool) -> bool {
        let until = Instant::now() + limit;
        while !done() {
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    impl Rig {
        /// The rig and its keepalive thread.
        fn new(input_caps: &str, period: Duration) -> (Self, JoinHandle<()>) {
            gst::init().unwrap();
            let pipeline = gst::Pipeline::new();
            pipeline.use_clock(Some(&gst::SystemClock::obtain()));
            let input = gst_app::AppSrc::builder()
                .caps(&caps(input_caps))
                .is_live(true)
                .format(gst::Format::Time)
                .do_timestamp(true)
                .build();
            let sink = gst_app::AppSink::builder()
                .sync(false)
                .async_(false)
                .build();
            let feed = gst_app::AppSrc::builder()
                .is_live(true)
                .format(gst::Format::Time)
                .build();
            let out = gst_app::AppSink::builder()
                .sync(false)
                .async_(false)
                .build();
            pipeline
                .add_many([
                    input.upcast_ref::<gst::Element>(),
                    sink.upcast_ref(),
                    feed.upcast_ref(),
                    out.upcast_ref(),
                ])
                .unwrap();
            input.link(&sink).unwrap();
            feed.link(&out).unwrap();
            let keepalive = bridge(&sink, &feed, period, "test-keepalive".into()).unwrap();
            pipeline.set_state(gst::State::Playing).unwrap();
            let rig = Self {
                pipeline,
                input,
                out,
            };
            (rig, keepalive)
        }

        /// A `size`² BGRx frame of one byte value.
        fn push_solid(&self, size: usize, byte: u8) {
            let mut buffer = gst::Buffer::with_size(size * size * 4).unwrap();
            buffer.get_mut().unwrap().map_writable().unwrap().fill(byte);
            self.input.push_buffer(buffer).unwrap();
        }

        /// Every sample `out` gets within `window`.
        fn collect(&self, window: Duration) -> Vec<gst::Sample> {
            let until = Instant::now() + window;
            let mut samples = Vec::new();
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                match self
                    .out
                    .try_pull_sample(gst::ClockTime::from_nseconds(left.as_nanos() as u64))
                {
                    Some(sample) => samples.push(sample),
                    None => break,
                }
            }
            samples
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = self.pipeline.set_state(gst::State::Null);
        }
    }

    /// Y, U and V of the first pixel of an I420 sample.
    fn yuv(sample: &gst::Sample) -> (u8, u8, u8) {
        let info = gst_video::VideoInfo::from_caps(sample.caps().unwrap()).unwrap();
        let frame =
            gst_video::VideoFrameRef::from_buffer_ref_readable(sample.buffer().unwrap(), &info)
                .unwrap();
        (
            frame.plane_data(0).unwrap()[0],
            frame.plane_data(1).unwrap()[0],
            frame.plane_data(2).unwrap()[0],
        )
    }

    #[test]
    fn a_still_screen_keeps_sending_its_last_frame() {
        let period = Duration::from_millis(20);
        let (rig, keepalive) = Rig::new(
            "video/x-raw,format=BGRx,width=64,height=64,framerate=0/1",
            period,
        );
        // One white frame, then nothing: a still screen.
        rig.push_solid(64, 0xff);
        let samples = rig.collect(Duration::from_millis(300));
        // The frame and ~14 repeats.
        assert!(samples.len() >= 6, "{} samples", samples.len());

        let s = samples[0].caps().unwrap().structure(0).unwrap();
        assert_eq!(s.get::<&str>("format").unwrap(), "I420");
        assert_eq!(s.get::<&str>("colorimetry").unwrap(), "bt709");
        assert_eq!(s.get::<i32>("width").unwrap(), 64);
        for sample in &samples {
            // White in limited-range bt709.
            let (y, u, v) = yuv(sample);
            assert!(
                y.abs_diff(235) <= 1 && u.abs_diff(128) <= 1 && v.abs_diff(128) <= 1,
                "{y} {u} {v}"
            );
        }
        let pts: Vec<u64> = samples
            .iter()
            .map(|s| s.buffer().unwrap().pts().unwrap().nseconds())
            .collect();
        assert!(pts.windows(2).all(|w| w[0] < w[1]), "{pts:?}");
        // Repeats are stamped when they're sent: never early, about a
        // period apart (the median, so a loaded machine doesn't fail it).
        let mut gaps: Vec<Duration> = pts
            .windows(2)
            .map(|w| Duration::from_nanos(w[1] - w[0]))
            .collect();
        assert!(gaps.iter().all(|gap| *gap >= period / 2), "{pts:?}");
        gaps.sort();
        assert!(gaps[gaps.len() / 2] < period * 3, "{pts:?}");

        // EOS passes through and ends the keepalive.
        rig.input.end_of_stream().unwrap();
        rig.collect(Duration::from_secs(2));
        assert!(rig.out.is_eos());
        assert!(
            within(Duration::from_secs(1), || keepalive.is_finished()),
            "the keepalive outlived EOS"
        );
    }

    #[test]
    fn new_frames_replace_the_repeated_one() {
        let (rig, _keepalive) = Rig::new(
            "video/x-raw,format=BGRx,width=32,height=32,framerate=0/1",
            Duration::from_millis(20),
        );
        rig.push_solid(32, 0xff);
        let white = rig.collect(Duration::from_millis(100));
        rig.push_solid(32, 0x00);
        let black = rig.collect(Duration::from_millis(100));
        assert!(
            white.len() >= 2 && black.len() >= 2,
            "{} {}",
            white.len(),
            black.len()
        );
        // From the black frame on, only black ones (Y 16 in limited range).
        assert!(black.iter().all(|s| yuv(s).0.abs_diff(16) <= 1));
        let last_white = white.last().unwrap().buffer().unwrap().pts().unwrap();
        let first_black = black[0].buffer().unwrap().pts().unwrap();
        assert!(first_black > last_white);
    }

    #[test]
    fn the_keepalive_ends_with_the_pipeline() {
        let (rig, keepalive) = Rig::new(
            "video/x-raw,format=BGRx,width=32,height=32,framerate=0/1",
            Duration::from_millis(20),
        );
        rig.push_solid(32, 0x80);
        assert!(!rig.collect(Duration::from_millis(60)).is_empty());
        assert!(!keepalive.is_finished());
        drop(rig);
        assert!(
            within(Duration::from_secs(1), || keepalive.is_finished()),
            "the keepalive outlived its pipeline"
        );
    }

    #[test]
    fn pts_never_go_back() {
        let t = gst::ClockTime::from_mseconds;
        assert_eq!(after(None, Some(t(5))), Some(t(5)));
        assert_eq!(after(Some(t(5)), Some(t(6))), Some(t(6)));
        assert_eq!(
            after(Some(t(5)), Some(t(5))),
            Some(t(5) + gst::ClockTime::NSECOND)
        );
        assert_eq!(
            after(Some(t(5)), Some(t(4))),
            Some(t(5) + gst::ClockTime::NSECOND)
        );
        assert_eq!(after(Some(t(5)), None), None);
    }
}
