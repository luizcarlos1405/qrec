//! The waveform peak cache (G§3.11, C17 lanes). Owner: WP2. Stub until WP2
//! lands: the API is final (G§3.11), every getter misses.
//!
//! A miss requests the file's peaks from the preview cache and returns
//! `None`; subscribers get the file whose peaks arrived. An LRU by bytes
//! keeps it under its budget (128 MiB).

use std::sync::Arc;

use qrec_shell::media::preview::{PeakLevels, PreviewCache, PreviewData};

use crate::store::{Subscribers, Subscription};

/// The default budget: 128 MiB.
pub const BUDGET_BYTES: usize = 128 << 20;

/// Peaks per file. Main thread only.
pub struct PeakCache {
    cache: PreviewCache,
    budget: usize,
    arrived: Subscribers<str>,
}

impl std::fmt::Debug for PeakCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeakCache")
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl PeakCache {
    pub fn new(cache: PreviewCache, budget_bytes: usize) -> Self {
        Self {
            cache,
            budget: budget_bytes,
            arrived: Subscribers::default(),
        }
    }

    /// `file`'s peaks, or `None` (they are requested).
    pub fn peaks(&self, file: &str) -> Option<Arc<PeakLevels>> {
        let _ = (file, &self.cache);
        None
    }

    /// Prefetches `file`'s peaks.
    pub fn request(&self, file: &str) {
        let _ = file;
    }

    /// Calls `f` with the file whose peaks arrived.
    pub fn subscribe(&self, f: impl Fn(&str) + 'static) -> Subscription {
        self.arrived.add(f)
    }

    /// Preview data from the queue (`MediaHub` routes it here).
    pub fn on_data(&self, data: &PreviewData) {
        let _ = data;
    }
}
