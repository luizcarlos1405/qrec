//! The waveform peak cache (G§3.11, C17 lanes). Owner: WP2.
//!
//! A miss requests the file's peaks from the preview cache and returns
//! `None`; subscribers get the file whose peaks arrived (or failed). An LRU
//! by bytes keeps it under its budget (128 MiB: about eight hours of audio
//! at 1, 16 and 256 ms per bucket). A file whose waveform failed isn't asked
//! again for `thumbs::RETRY_AFTER`. The bookkeeping ([`PeakStore`]) is
//! GTK-free.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use qrec_shell::media::preview::{
    PeakLevels, PreviewCache, PreviewData, PreviewRequest, WHAT_PEAKS,
};

use super::thumbs::{ByteLru, RETRY_AFTER};
use crate::store::{Subscribers, Subscription};

/// The default budget: 128 MiB.
pub const BUDGET_BYTES: usize = 128 << 20;

/// Peaks per file. Main thread only.
pub struct PeakCache {
    cache: PreviewCache,
    store: RefCell<PeakStore>,
    arrived: Subscribers<str>,
}

impl std::fmt::Debug for PeakCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let store = self.store.borrow();
        f.debug_struct("PeakCache")
            .field("budget", &store.lru.budget())
            .field("bytes", &store.lru.bytes())
            .field("files", &store.lru.len())
            .finish_non_exhaustive()
    }
}

impl PeakCache {
    pub fn new(cache: PreviewCache, budget_bytes: usize) -> Self {
        Self {
            cache,
            store: RefCell::new(PeakStore::new(budget_bytes)),
            arrived: Subscribers::default(),
        }
    }

    /// `file`'s peaks, or `None` (they are requested).
    pub fn peaks(&self, file: &str) -> Option<Arc<PeakLevels>> {
        let (levels, request) = self.store.borrow_mut().look(file, Instant::now());
        if let Some(request) = request {
            self.cache.request(request);
        }
        levels
    }

    /// Prefetches `file`'s peaks.
    pub fn request(&self, file: &str) {
        let request = self.store.borrow_mut().want(file, Instant::now());
        if let Some(request) = request {
            self.cache.request(request);
        }
    }

    /// Calls `f` with the file whose peaks arrived (or failed).
    pub fn subscribe(&self, f: impl Fn(&str) + 'static) -> Subscription {
        self.arrived.add(f)
    }

    /// The bytes held now (under the budget).
    pub fn bytes(&self) -> usize {
        self.store.borrow().lru.bytes()
    }

    /// Preview data from the queue (`MediaHub` routes it here).
    pub fn on_data(&self, data: &PreviewData) {
        let file = self.store.borrow_mut().arrive(data, Instant::now());
        if let Some(file) = file {
            self.arrived.emit(&file);
        }
    }
}

/// The cache's bookkeeping, GTK-free.
#[derive(Debug)]
pub(crate) struct PeakStore {
    lru: ByteLru<String, Arc<PeakLevels>>,
    pending: BTreeSet<String>,
    failed: BTreeMap<String, Instant>,
}

impl PeakStore {
    pub fn new(budget: usize) -> Self {
        Self {
            lru: ByteLru::new(budget),
            pending: BTreeSet::new(),
            failed: BTreeMap::new(),
        }
    }

    /// The peaks if held, else the request to send (if any).
    pub fn look(
        &mut self,
        file: &str,
        now: Instant,
    ) -> (Option<Arc<PeakLevels>>, Option<PreviewRequest>) {
        match self.lru.get(file) {
            Some(levels) => (Some(Arc::clone(levels)), None),
            None => (None, self.want(file, now)),
        }
    }

    /// The request for `file`, unless it is held, asked for already, or
    /// failed less than `RETRY_AFTER` ago.
    pub fn want(&mut self, file: &str, now: Instant) -> Option<PreviewRequest> {
        if self.lru.contains(file) || self.pending.contains(file) {
            return None;
        }
        if let Some(at) = self.failed.get(file) {
            if now.saturating_duration_since(*at) < RETRY_AFTER {
                return None;
            }
            self.failed.remove(file);
        }
        self.pending.insert(file.to_owned());
        Some(PreviewRequest::Peaks {
            file: file.to_owned(),
        })
    }

    /// Takes an answer in; the file to tell subscribers about.
    pub fn arrive(&mut self, data: &PreviewData, now: Instant) -> Option<String> {
        match data {
            PreviewData::Peaks { file, levels } => {
                self.pending.remove(file);
                if self
                    .lru
                    .insert(file.clone(), Arc::clone(levels), levels.bytes())
                    .is_err()
                {
                    // Longer than the whole budget holds: don't ask on every draw.
                    tracing::debug!("the waveform of {file} is bigger than the peak budget");
                    self.failed.insert(file.clone(), now);
                }
                Some(file.clone())
            }
            PreviewData::Failed { file, what, .. } if *what == WHAT_PEAKS => {
                self.pending.remove(file);
                self.failed.insert(file.clone(), now);
                Some(file.clone())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use qrec_core::media_argv::peak_levels;

    use super::*;

    fn levels(ms: usize) -> Arc<PeakLevels> {
        Arc::new(peak_levels(vec![(-1, 1); ms].into()))
    }

    fn arrived(file: &str, ms: usize) -> PreviewData {
        PreviewData::Peaks {
            file: file.into(),
            levels: levels(ms),
        }
    }

    #[test]
    fn a_miss_asks_once_and_an_answer_is_held() {
        let now = Instant::now();
        let mut store = PeakStore::new(BUDGET_BYTES);
        let (none, request) = store.look("a.mka", now);
        assert!(none.is_none());
        assert_eq!(
            request,
            Some(PreviewRequest::Peaks {
                file: "a.mka".into()
            })
        );
        assert_eq!(store.look("a.mka", now).1, None, "asked already");
        assert_eq!(
            store.arrive(&arrived("a.mka", 2000), now).as_deref(),
            Some("a.mka")
        );
        let (held, request) = store.look("a.mka", now);
        assert_eq!(held.unwrap().per_ms.len(), 2000);
        assert_eq!(request, None);
        assert_eq!(store.lru.bytes(), levels(2000).bytes());
        // Strips and others aren't peaks.
        assert_eq!(
            store.arrive(
                &PreviewData::Failed {
                    file: "a.mka".into(),
                    what: qrec_shell::media::preview::WHAT_STRIP,
                    message: String::new()
                },
                now
            ),
            None
        );
    }

    #[test]
    fn a_failed_file_waits_before_it_is_asked_again() {
        let now = Instant::now();
        let mut store = PeakStore::new(BUDGET_BYTES);
        assert!(store.want("bad.mka", now).is_some());
        let failed = PreviewData::Failed {
            file: "bad.mka".into(),
            what: WHAT_PEAKS,
            message: "no audio".into(),
        };
        assert_eq!(store.arrive(&failed, now).as_deref(), Some("bad.mka"));
        assert_eq!(store.look("bad.mka", now + RETRY_AFTER / 2), (None, None));
        assert!(store.want("bad.mka", now + RETRY_AFTER).is_some());
    }

    /// The property over random request sequences: the store never holds
    /// more than its budget, whatever arrives in whatever order (a
    /// waveform longer than the whole budget is never held, nor asked for
    /// on every draw).
    #[test]
    fn random_requests_never_exceed_the_budget() {
        use super::super::thumbs::tests_rng::Rng;
        let files: Vec<String> = (0..12)
            .map(|i| format!("takes/take-{i:04}/A1-mic.mka"))
            .collect();
        // Waveforms of up to ~7 minutes, made once (only their size matters).
        let sizes: Vec<Arc<PeakLevels>> = [0, 100, 1_000, 10_000, 50_000, 150_000, 400_000]
            .into_iter()
            .map(levels)
            .collect();
        for seed in 0..300 {
            let mut rng = Rng::new(7000 + seed);
            let budget = rng.below(2 << 20) as usize;
            let mut store = PeakStore::new(budget);
            let mut now = Instant::now();
            let mut out: Vec<String> = Vec::new();
            for step in 0..300 {
                let file = &files[rng.below(files.len() as u64) as usize];
                match rng.below(6) {
                    0 | 1 => {
                        let (_, request) = store.look(file, now);
                        if let Some(PreviewRequest::Peaks { file }) = request {
                            out.push(file);
                        }
                    }
                    2..=4 if !out.is_empty() => {
                        let file = out.swap_remove(rng.below(out.len() as u64) as usize);
                        let data = if rng.below(8) == 0 {
                            PreviewData::Failed {
                                file,
                                what: WHAT_PEAKS,
                                message: "no".into(),
                            }
                        } else {
                            // Some are bigger than the whole budget.
                            PreviewData::Peaks {
                                file,
                                levels: Arc::clone(&sizes[rng.below(sizes.len() as u64) as usize]),
                            }
                        };
                        store.arrive(&data, now);
                    }
                    _ => now += Duration::from_secs(rng.below(20)),
                }
                assert!(store.lru.bytes() <= budget, "seed {seed} step {step}");
                for file in &store.pending {
                    assert!(
                        out.contains(file),
                        "seed {seed} step {step}: {file} waits for nothing"
                    );
                    assert!(!store.lru.contains(file), "seed {seed} step {step}");
                }
            }
        }
    }

    /// An hour of audio (3.6 M buckets) fits the default budget; several
    /// hours of takes stay under it, the least recently drawn going first.
    #[test]
    fn hours_of_audio_stay_under_the_budget() {
        let now = Instant::now();
        let mut store = PeakStore::new(BUDGET_BYTES);
        let hour = levels(3_600_000);
        assert!(hour.bytes() < 16 << 20, "{}", hour.bytes());
        for i in 0..20 {
            let file = format!("takes/take-{i:04}/A1-mic.mka");
            assert!(store.want(&file, now).is_some());
            store.arrive(
                &PreviewData::Peaks {
                    file: file.clone(),
                    levels: Arc::clone(&hour),
                },
                now,
            );
            assert!(store.lru.bytes() <= BUDGET_BYTES);
            assert!(store.look(&file, now).0.is_some(), "the newest is held");
        }
        assert!(
            store.lru.len() >= 8,
            "about eight hours fit: {}",
            store.lru.len()
        );
        assert!(
            store.look("takes/take-0000/A1-mic.mka", now).0.is_none(),
            "the oldest went"
        );
    }
}
