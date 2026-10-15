//! The thumbnail texture cache (G§3.11, C16 filmstrips, S09 frame zoom).
//! Owner: WP2. Stub until WP2 lands: the API is final (G§3.11), every
//! getter misses and requests nothing.
//!
//! Getters never block: a miss requests the containing chunk or block from
//! the preview cache and returns `None`; subscribers get the file whose
//! data arrived and redraw. Textures are `gdk::MemoryTexture` (R8G8B8A8)
//! built on the main thread from the shared bytes (R22); an LRU by bytes
//! keeps the cache under its budget (256 MiB).

use gtk::gdk;
use qrec_shell::media::preview::{PreviewCache, PreviewData};

use crate::store::{Subscribers, Subscription};

/// The default budget: 256 MiB of RGBA.
pub const BUDGET_BYTES: usize = 256 << 20;

/// Strip images and frame-zoom frames as textures. Main thread only.
pub struct ThumbCache {
    cache: PreviewCache,
    budget: usize,
    arrived: Subscribers<str>,
}

impl std::fmt::Debug for ThumbCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbCache")
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl ThumbCache {
    pub fn new(cache: PreviewCache, budget_bytes: usize) -> Self {
        Self {
            cache,
            budget: budget_bytes,
            arrived: Subscribers::default(),
        }
    }

    /// The strip image of `file` at second `file_time_s` (`height` device
    /// px), or `None` (its 60 s chunk is requested).
    pub fn strip_image(&self, file: &str, file_time_s: u32, height: u32) -> Option<gdk::Texture> {
        let _ = (file, file_time_s, height, &self.cache);
        None
    }

    /// Recorded frame `recorded_frame` of `file` (frame zoom), or `None`
    /// (its block of 64 is requested).
    pub fn frame(&self, file: &str, recorded_frame: u64, height: u32) -> Option<gdk::Texture> {
        let _ = (file, recorded_frame, height);
        None
    }

    /// Prefetches a strip chunk; idempotent.
    pub fn request_strip(&self, file: &str, chunk: u32, height: u32) {
        let _ = (file, chunk, height);
    }

    /// Prefetches `count` (≤ 64) recorded frames from `first`.
    pub fn request_frames(&self, file: &str, first: u64, count: u32, height: u32) {
        let _ = (file, first, count, height);
    }

    /// Calls `f` with the file whose images arrived.
    pub fn subscribe(&self, f: impl Fn(&str) + 'static) -> Subscription {
        self.arrived.add(f)
    }

    /// The bytes held now (under the budget).
    pub fn bytes(&self) -> usize {
        0
    }

    /// Preview data from the queue (`MediaHub` routes it here).
    pub fn on_data(&self, data: &PreviewData) {
        let _ = data;
    }
}
