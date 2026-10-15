//! Thumbnails, recorded frames and waveform peaks for the GUI (docs/GUI.md
//! G§3.11): derived data, never State (SPEC §6.1: `.qrec/cache/` can be
//! deleted at any time). ffmpeg runs per the A§8.7 rules (a log, a
//! timeout, no unread pipe), at most 2 at once (1 while an export runs);
//! strips and peaks are cached under `.qrec/cache/{thumbnails,waveforms}/`,
//! frames stay in memory. Results come back through `on_data`, on a worker
//! thread. Owner: media.
//!
//! Not built yet: requests are ignored, so the GUI's caches keep their
//! placeholders, and nothing runs.

use std::path::PathBuf;
use std::sync::Arc;

use qrec_core::protocol::Interval;
use qrec_core::state::SilenceKey;

/// What the GUI asks for (deduplicated by the cache key).
#[derive(Debug, Clone, PartialEq)]
pub enum PreviewRequest {
    /// 60 s of one-per-second images (keyframes) from `60 · chunk` s on.
    Strip {
        file: String,
        chunk: u32,
        height: u32,
    },
    /// Up to 64 consecutive recorded frames from `first` (frame zoom).
    Frames {
        file: String,
        first: u64,
        count: u32,
        height: u32,
    },
    /// The waveform of the file's first audio stream.
    Peaks { file: String },
    /// A silence analysis already cached for these parameters (never runs
    /// ffmpeg).
    CachedSilence {
        file: String,
        threshold_db: f64,
        min_silence_ms: u32,
    },
}

/// What comes back.
#[derive(Debug, Clone)]
pub enum PreviewData {
    /// RGBA images of `width` × `height`, one after the other.
    Strip {
        file: String,
        chunk: u32,
        height: u32,
        width: u32,
        images: Arc<[u8]>,
    },
    Frames {
        file: String,
        first: u64,
        width: u32,
        height: u32,
        images: Arc<[u8]>,
    },
    Peaks {
        file: String,
        levels: Arc<PeakLevels>,
    },
    /// `None`: never analyzed with these parameters.
    Silence {
        file: String,
        key: SilenceKey,
        intervals: Option<Vec<Interval>>,
    },
    Failed {
        file: String,
        what: &'static str,
        message: String,
    },
}

/// A waveform as (min, max) sample pairs per bucket of 1, 16 and 256 ms, so
/// a lane draws from the coarsest level with at least one bucket per pixel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeakLevels {
    pub per_ms: Arc<[(i16, i16)]>,
    pub per_16ms: Arc<[(i16, i16)]>,
    pub per_256ms: Arc<[(i16, i16)]>,
}

/// The project's preview cache and its ffmpeg runs. Cheap to clone.
#[derive(Debug, Clone)]
pub struct PreviewCache {
    _private: (),
}

impl PreviewCache {
    /// A cache for `project_dir` that hands each result to `on_data`.
    pub fn new(
        project_dir: impl Into<PathBuf>,
        on_data: Box<dyn Fn(PreviewData) + Send + Sync>,
    ) -> Self {
        let _ = (project_dir.into(), on_data);
        PreviewCache { _private: () }
    }

    /// Asks for `req`'s data; a request already queued or running is
    /// joined. Returns at once.
    pub fn request(&self, req: PreviewRequest) {
        tracing::trace!("previews aren't built yet: {req:?} ignored");
    }

    /// While an export runs, at most one preview ffmpeg runs.
    pub fn set_exporting(&self, exporting: bool) {
        let _ = exporting;
    }

    /// Kills the running children and stops the workers.
    pub fn shutdown(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unbuilt_cache_takes_requests_and_answers_none() {
        let answered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = Arc::clone(&answered);
        let cache = PreviewCache::new(
            "/p",
            Box::new(move |_| seen.store(true, std::sync::atomic::Ordering::SeqCst)),
        );
        let copy = cache.clone();
        copy.request(PreviewRequest::Peaks {
            file: "takes/take-0001/A1-mic.mka".into(),
        });
        cache.set_exporting(true);
        cache.shutdown();
        assert!(!answered.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(PeakLevels::default().per_ms.len(), 0);
    }
}
