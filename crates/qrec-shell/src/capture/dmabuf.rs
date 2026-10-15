//! DMA-BUF screencast frames whose memories don't know their size (A§8.11).
//!
//! niri sets the PipeWire data size of a DMA-BUF frame to 1, so
//! `pipewiresrc` (PipeWire 1.6) wraps each frame in a 1-byte dmabuf memory.
//! GL's DMA-BUF import then can't find the plane ("could not find memory
//! 0") and its CPU fallback maps one page and reads the whole frame past it
//! (SIGSEGV). [`resize_dmabufs`] swaps each such memory for one over a dup of
//! the same fd with the buffer object's real size (`lseek(fd, 0, SEEK_END)`),
//! keeping its offset, so the video meta's plane offsets still hold.

use std::os::fd::{BorrowedFd, RawFd};
use std::sync::Mutex;

use gst::prelude::*;
use gst_allocators::prelude::*;
use gst_allocators::{DmaBufAllocator, DmaBufMemory};

/// What a screencast may negotiate (set on the capsfilter after linking:
/// DMA_DRM caps fail the link-time checks). niri offers DMA-BUF only, and
/// GL import of its frames samples black, so only **linear** DMA-BUF is
/// taken (CPU-mappable, single plane), plus shared memory for compositors
/// that offer it (GNOME, KDE).
pub(super) const SCREEN_CAPS: &str = "video/x-raw(memory:DMABuf),format=DMA_DRM,\
     drm-format={XR24,AR24,XB24,AB24}; video/x-raw,format={BGRx,BGRA,RGBx,RGBA,xRGB,ARGB}";

/// Connects the screencast's `sink` to `src`, the start of the rest of
/// the branch: each frame is pushed on with its timestamps, a linear
/// DMA-BUF frame as the plain format it is (`XR24` → `BGRx`) with its
/// memory sized ([`resize_dmabufs`]) so `videoconvert` can map it. EOS is
/// passed on.
pub(super) fn bridge(sink: &gst_app::AppSink, src: &gst_app::AppSrc) {
    let allocator = DmaBufAllocator::new();
    let target = src.downgrade();
    let last_caps: Mutex<Option<gst::Caps>> = Mutex::new(None);
    let eos_target = src.downgrade();
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let src = target.upgrade().ok_or(gst::FlowError::Flushing)?;
                let caps = sample.caps().ok_or(gst::FlowError::NotNegotiated)?;
                let dmabuf = gst_video::is_dma_drm_caps(caps);
                {
                    let mut last = last_caps.lock().unwrap_or_else(|e| e.into_inner());
                    if last.as_ref().map(|c| c.as_ref()) != Some(caps) {
                        let plain = system_caps(caps).map_err(|e| {
                            gst::element_error!(src, gst::CoreError::Negotiation, ("{}", e));
                            gst::FlowError::NotNegotiated
                        })?;
                        src.set_caps(Some(&plain));
                        *last = Some(caps.to_owned());
                    }
                }
                let mut buffer = sample.buffer_owned().ok_or(gst::FlowError::Error)?;
                if dmabuf {
                    resize_dmabufs(buffer.make_mut(), &allocator).map_err(|e| {
                        gst::element_error!(src, gst::ResourceError::Read, ("{}", e));
                        gst::FlowError::Error
                    })?;
                }
                src.push_buffer(buffer)
            })
            .eos(move |_| {
                if let Some(src) = eos_target.upgrade() {
                    let _ = src.end_of_stream();
                }
            })
            .build(),
    );
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

    #[test]
    fn linear_dma_drm_caps_become_their_plain_format() {
        gst::init().unwrap();
        let caps = |s: &str| gst::Caps::from_str(s).unwrap();
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
        // SAFETY: plain syscalls; the fd is owned right after.
        let memfd = unsafe {
            let fd = libc::memfd_create(c"qrec-dmabuf-test".as_ptr(), libc::MFD_CLOEXEC);
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            assert_eq!(libc::ftruncate(fd, size as libc::off_t), 0);
            OwnedFd::from_raw_fd(fd)
        };
        let allocator = DmaBufAllocator::new();
        // SAFETY: a memfd is an fd the dmabuf allocator can wrap.
        let tiny = unsafe { allocator.alloc_dmabuf(memfd, 1) }.unwrap();
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
}
