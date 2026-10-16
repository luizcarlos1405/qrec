//! The thumbnail texture cache (G§3.11, C16 filmstrips, S09 frame zoom).
//! Owner: WP2.
//!
//! Getters never block: a miss requests the containing chunk or block from
//! the preview cache and returns `None`; subscribers get the file whose
//! data arrived and redraw. Textures are `gdk::MemoryTexture` (R8G8B8A8)
//! built on the main thread from the shared bytes (R22): each image is a
//! slice of its chunk's `Arc<[u8]>` handed to GDK without a copy, made once
//! and kept with its chunk. An LRU by bytes keeps the cache under its
//! budget (256 MiB); a chunk that goes takes its textures with it.
//! ([`ThumbCache::bytes`] counts what the cache holds: a texture a widget
//! still holds, in a cached render node say, keeps its chunk's bytes alive
//! until the widget lets go.)
//!
//! - **Strips**: image `i` of chunk `c` is file time `60·c + i` s; a time
//!   past a chunk's last image (the end of the file) shows that last image.
//! - **Frames**: recorded frames are asked for in aligned blocks of
//!   [`FRAME_BLOCK`] (64), the most one run returns; a frame past the end of
//!   the file has no image.
//! - **Failures**: a file whose thumbnails (or frames) failed isn't asked
//!   again for [`RETRY_AFTER`], so a broken file costs one run per draw
//!   burst, not one per frame.
//!
//! The bookkeeping ([`ByteLru`], [`ThumbStore`]) is GTK-free and tested
//! without a display; the textures are made in the nested smoke run
//! (`textures_in_a_nested_compositor`, ignored by default).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, glib};
use qrec_core::media_argv::{MAX_PREVIEW_FRAMES, STRIP_CHUNK_SECS};
use qrec_shell::media::preview::{
    PreviewCache, PreviewData, PreviewRequest, WHAT_FRAMES, WHAT_STRIP, image_bytes,
};

use crate::store::{Subscribers, Subscription};

/// The default budget: 256 MiB of RGBA.
pub const BUDGET_BYTES: usize = 256 << 20;

/// Recorded frames are asked for in aligned blocks of this many.
pub const FRAME_BLOCK: u64 = MAX_PREVIEW_FRAMES as u64;

/// A file whose previews failed isn't asked again for this long.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Strip images and frame-zoom frames as textures. Main thread only.
pub struct ThumbCache {
    cache: PreviewCache,
    store: RefCell<ThumbStore>,
    /// The textures made so far, per cached entry and image.
    textures: RefCell<BTreeMap<ThumbKey, Vec<Option<gdk::Texture>>>>,
    arrived: Subscribers<str>,
}

impl std::fmt::Debug for ThumbCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let store = self.store.borrow();
        f.debug_struct("ThumbCache")
            .field("budget", &store.budget())
            .field("bytes", &store.bytes())
            .field("entries", &store.len())
            .finish_non_exhaustive()
    }
}

impl ThumbCache {
    pub fn new(cache: PreviewCache, budget_bytes: usize) -> Self {
        Self {
            cache,
            store: RefCell::new(ThumbStore::new(budget_bytes)),
            textures: RefCell::new(BTreeMap::new()),
            arrived: Subscribers::default(),
        }
    }

    /// The strip image of `file` at second `file_time_s` (`height` device
    /// px), or `None` (its 60 s chunk is requested).
    pub fn strip_image(&self, file: &str, file_time_s: u32, height: u32) -> Option<gdk::Texture> {
        let key = ThumbKey::strip(file, file_time_s / STRIP_CHUNK_SECS, height);
        self.image(key, file_time_s % STRIP_CHUNK_SECS)
    }

    /// Recorded frame `recorded_frame` of `file` (frame zoom), or `None`
    /// (its block of 64 is requested).
    pub fn frame(&self, file: &str, recorded_frame: u64, height: u32) -> Option<gdk::Texture> {
        let key = ThumbKey::frames(file, recorded_frame / FRAME_BLOCK, height);
        self.image(key, (recorded_frame % FRAME_BLOCK) as u32)
    }

    /// Prefetches a strip chunk; idempotent.
    pub fn request_strip(&self, file: &str, chunk: u32, height: u32) {
        self.want(ThumbKey::strip(file, chunk, height));
    }

    /// Prefetches `count` (≤ 64) recorded frames from `first`: the aligned
    /// blocks that hold them.
    pub fn request_frames(&self, file: &str, first: u64, count: u32, height: u32) {
        if count == 0 {
            return;
        }
        let last = first.saturating_add(u64::from(count.min(MAX_PREVIEW_FRAMES)) - 1);
        for block in first / FRAME_BLOCK..=last / FRAME_BLOCK {
            self.want(ThumbKey::frames(file, block, height));
        }
    }

    /// Calls `f` with the file whose images arrived (or failed).
    pub fn subscribe(&self, f: impl Fn(&str) + 'static) -> Subscription {
        self.arrived.add(f)
    }

    /// The bytes held now (under the budget).
    pub fn bytes(&self) -> usize {
        self.store.borrow().bytes()
    }

    /// Preview data from the queue (`MediaHub` routes it here).
    pub fn on_data(&self, data: &PreviewData) {
        let arrival = self.store.borrow_mut().arrive(data, Instant::now());
        {
            let mut textures = self.textures.borrow_mut();
            for key in &arrival.dropped {
                textures.remove(key);
            }
        }
        if let Some(file) = arrival.file {
            self.arrived.emit(&file);
        }
    }

    fn want(&self, key: ThumbKey) {
        let request = self.store.borrow_mut().want(key, Instant::now());
        if let Some(request) = request {
            self.cache.request(request);
        }
    }

    fn image(&self, key: ThumbKey, index: u32) -> Option<gdk::Texture> {
        let look = self.store.borrow_mut().look(&key, index, Instant::now());
        match look {
            Look::Hit(index, images) => Some(self.texture(&key, index, &images)),
            Look::Request(request) => {
                self.cache.request(request);
                None
            }
            Look::Wait | Look::PastEnd => None,
        }
    }

    /// Image `index` of `images` (the entry of `key`) as a texture, made
    /// once from a slice of the shared bytes.
    fn texture(&self, key: &ThumbKey, index: u32, images: &Images) -> gdk::Texture {
        let mut textures = self.textures.borrow_mut();
        let slots = textures
            .entry(key.clone())
            .or_insert_with(|| vec![None; images.count as usize]);
        if let Some(Some(texture)) = slots.get(index as usize) {
            return texture.clone();
        }
        let texture = memory_texture(images, index);
        if let Some(slot) = slots.get_mut(index as usize) {
            *slot = Some(texture.clone());
        }
        texture
    }
}

/// A `gdk::MemoryTexture` over image `index` of `images`, sharing its bytes.
fn memory_texture(images: &Images, index: u32) -> gdk::Texture {
    let bytes = glib::Bytes::from_owned(images.slice(index));
    gdk::MemoryTexture::new(
        images.width as i32,
        images.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        images.width as usize * 4,
    )
    .upcast()
}

// ------------------------------------------------------- GTK-free parts

/// What a cached entry is: a strip chunk or a block of recorded frames, of
/// one file at one height.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ThumbKey {
    Strip {
        file: String,
        height: u32,
        chunk: u32,
    },
    Frames {
        file: String,
        height: u32,
        block: u64,
    },
}

impl ThumbKey {
    fn strip(file: &str, chunk: u32, height: u32) -> Self {
        ThumbKey::Strip {
            file: file.to_owned(),
            height,
            chunk,
        }
    }

    fn frames(file: &str, block: u64, height: u32) -> Self {
        ThumbKey::Frames {
            file: file.to_owned(),
            height,
            block,
        }
    }

    fn file(&self) -> &str {
        match self {
            ThumbKey::Strip { file, .. } | ThumbKey::Frames { file, .. } => file,
        }
    }

    /// `PreviewData::Failed`'s `what` for this kind of entry.
    fn what(&self) -> &'static str {
        match self {
            ThumbKey::Strip { .. } => WHAT_STRIP,
            ThumbKey::Frames { .. } => WHAT_FRAMES,
        }
    }

    fn request(&self) -> PreviewRequest {
        match self {
            ThumbKey::Strip {
                file,
                height,
                chunk,
            } => PreviewRequest::Strip {
                file: file.clone(),
                chunk: *chunk,
                height: *height,
            },
            ThumbKey::Frames {
                file,
                height,
                block,
            } => PreviewRequest::Frames {
                file: file.clone(),
                first: block * FRAME_BLOCK,
                count: MAX_PREVIEW_FRAMES,
                height: *height,
            },
        }
    }
}

/// One entry's images: `count` RGBA images of `width` × `height`, back to
/// back in `bytes`.
#[derive(Debug, Clone)]
pub(crate) struct Images {
    pub width: u32,
    pub height: u32,
    pub count: u32,
    pub bytes: Arc<[u8]>,
}

impl Images {
    /// Image `index` (clamped to the last), without copying.
    pub fn slice(&self, index: u32) -> ImageSlice {
        let len = image_bytes(self.width, self.height);
        let index = index.min(self.count.saturating_sub(1)) as usize;
        ImageSlice {
            bytes: Arc::clone(&self.bytes),
            start: index * len,
            len,
        }
    }
}

/// One image of a chunk: its bytes shared, not copied (for
/// `glib::Bytes::from_owned`).
#[derive(Debug, Clone)]
pub(crate) struct ImageSlice {
    bytes: Arc<[u8]>,
    start: usize,
    len: usize,
}

impl AsRef<[u8]> for ImageSlice {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[self.start..self.start + self.len]
    }
}

/// What a getter finds.
#[derive(Debug)]
pub(crate) enum Look {
    /// Image `index` (clamped) of these images.
    Hit(u32, Images),
    /// Not here: ask the preview cache for this.
    Request(PreviewRequest),
    /// Asked for already (or failed a moment ago): nothing to draw yet.
    Wait,
    /// Known to have no image there (past the end of the file).
    PastEnd,
}

/// What an arrival changed.
#[derive(Debug, Default)]
pub(crate) struct Arrival {
    /// The file to tell subscribers about.
    pub file: Option<String>,
    /// Entries that went (evicted or replaced): their textures go too.
    pub dropped: Vec<ThumbKey>,
}

/// The cache's bookkeeping, GTK-free: the entries under a byte budget, the
/// requests out, the files that failed a moment ago.
#[derive(Debug)]
pub(crate) struct ThumbStore {
    lru: ByteLru<ThumbKey, Images>,
    pending: BTreeSet<ThumbKey>,
    failed: BTreeMap<(String, &'static str), Instant>,
}

impl ThumbStore {
    pub fn new(budget: usize) -> Self {
        Self {
            lru: ByteLru::new(budget),
            pending: BTreeSet::new(),
            failed: BTreeMap::new(),
        }
    }

    pub fn budget(&self) -> usize {
        self.lru.budget()
    }

    pub fn bytes(&self) -> usize {
        self.lru.bytes()
    }

    pub fn len(&self) -> usize {
        self.lru.len()
    }

    /// Image `index` of `key`'s entry, or what to do about it.
    pub fn look(&mut self, key: &ThumbKey, index: u32, now: Instant) -> Look {
        if let Some(images) = self.lru.get(key) {
            let past_end = match key {
                ThumbKey::Strip { .. } => images.count == 0,
                ThumbKey::Frames { .. } => index >= images.count,
            };
            return if past_end {
                Look::PastEnd
            } else {
                Look::Hit(index.min(images.count - 1), images.clone())
            };
        }
        match self.want(key.clone(), now) {
            Some(request) => Look::Request(request),
            None => Look::Wait,
        }
    }

    /// The request for `key`, unless it is cached, asked for already, or
    /// its file failed less than [`RETRY_AFTER`] ago.
    pub fn want(&mut self, key: ThumbKey, now: Instant) -> Option<PreviewRequest> {
        if self.lru.contains(&key) || self.pending.contains(&key) {
            return None;
        }
        let failed = (key.file().to_owned(), key.what());
        if let Some(at) = self.failed.get(&failed) {
            if now.saturating_duration_since(*at) < RETRY_AFTER {
                return None;
            }
            self.failed.remove(&failed);
        }
        let request = key.request();
        self.pending.insert(key);
        Some(request)
    }

    /// Takes an answer of the preview cache in.
    pub fn arrive(&mut self, data: &PreviewData, now: Instant) -> Arrival {
        let (key, images) = match data {
            PreviewData::Strip {
                file,
                chunk,
                height,
                width,
                images,
            } => (
                ThumbKey::strip(file, *chunk, *height),
                (*width, *height, images),
            ),
            PreviewData::Frames {
                file,
                first,
                width,
                height,
                images,
            } => {
                // Only aligned blocks are asked for (by this cache).
                if first % FRAME_BLOCK != 0 {
                    return Arrival::default();
                }
                (
                    ThumbKey::frames(file, first / FRAME_BLOCK, *height),
                    (*width, *height, images),
                )
            }
            PreviewData::Failed { file, what, .. } if [WHAT_STRIP, WHAT_FRAMES].contains(what) => {
                self.pending
                    .retain(|key| !(key.file() == file && key.what() == *what));
                self.failed.insert((file.clone(), what), now);
                return Arrival {
                    file: Some(file.clone()),
                    dropped: Vec::new(),
                };
            }
            _ => return Arrival::default(),
        };
        self.pending.remove(&key);
        let (width, height, bytes) = images;
        let frame = image_bytes(width, height);
        let count = bytes.len().checked_div(frame).unwrap_or(0);
        let entry = Images {
            width,
            height,
            count: u32::try_from(count).unwrap_or(u32::MAX),
            bytes: Arc::clone(bytes),
        };
        let file = key.file().to_owned();
        let what = key.what();
        match self.lru.insert(key, entry, bytes.len()) {
            Ok(dropped) => Arrival {
                file: Some(file),
                dropped,
            },
            Err(key) => {
                // Bigger than the whole budget: never cached, so don't ask
                // for it on every draw.
                tracing::debug!("{key:?} is bigger than the thumbnail budget");
                self.failed.insert((file.clone(), what), now);
                Arrival {
                    file: Some(file),
                    dropped: Vec::new(),
                }
            }
        }
    }

    #[cfg(test)]
    fn pending(&self) -> &BTreeSet<ThumbKey> {
        &self.pending
    }
}

/// A map that never holds more than its byte budget: inserting evicts the
/// least recently used entries first (a `get` is a use). Shared with
/// `peaks.rs`.
#[derive(Debug)]
pub(crate) struct ByteLru<K: Ord + Clone, V> {
    budget: usize,
    bytes: usize,
    tick: u64,
    entries: BTreeMap<K, (V, usize, u64)>,
    /// tick → key, oldest first.
    order: BTreeMap<u64, K>,
}

impl<K: Ord + Clone, V> ByteLru<K, V> {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            bytes: 0,
            tick: 0,
            entries: BTreeMap::new(),
            order: BTreeMap::new(),
        }
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// The bytes of the entries held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn contains<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
    {
        self.entries.contains_key(key)
    }

    /// The entry, now the most recently used.
    pub fn get<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
    {
        self.tick += 1;
        let tick = self.tick;
        let (_, _, used) = self.entries.get_mut(key)?;
        let old = std::mem::replace(used, tick);
        if let Some(k) = self.order.remove(&old) {
            self.order.insert(tick, k);
        }
        self.entries.get(key).map(|(value, _, _)| value)
    }

    /// Adds (or replaces) an entry of `bytes`, evicting the least recently
    /// used until everything fits. Returns the keys that went (`key` itself
    /// when it replaced an entry); `Err(key)` when `bytes` alone is over the
    /// budget (nothing changes).
    pub fn insert(&mut self, key: K, value: V, bytes: usize) -> Result<Vec<K>, K> {
        if bytes > self.budget {
            return Err(key);
        }
        let mut dropped = Vec::new();
        if self.remove(&key).is_some() {
            dropped.push(key.clone());
        }
        while self.bytes + bytes > self.budget {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((_, size, _)) = self.entries.remove(&oldest) {
                self.bytes -= size;
            }
            dropped.push(oldest);
        }
        self.tick += 1;
        self.order.insert(self.tick, key.clone());
        self.entries.insert(key, (value, bytes, self.tick));
        self.bytes += bytes;
        Ok(dropped)
    }

    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
    {
        let (value, bytes, tick) = self.entries.remove(key)?;
        self.order.remove(&tick);
        self.bytes -= bytes;
        Some(value)
    }

    /// Keys from the least to the most recently used.
    #[cfg(test)]
    pub fn keys_by_age(&self) -> Vec<K> {
        self.order.values().cloned().collect()
    }
}

/// A seeded PRNG for the random-sequence properties (this crate has no
/// proptest dev-dependency): splitmix64, so a failing sequence is replayed
/// by its seed.
#[cfg(test)]
pub(crate) mod tests_rng {
    pub(crate) struct Rng(u64);

    impl Rng {
        pub fn new(seed: u64) -> Self {
            Rng(seed)
        }

        pub fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        pub fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_rng::Rng;
    use super::*;

    /// A naive model: a list, least recently used first.
    struct Model {
        budget: usize,
        list: Vec<(u8, usize)>,
    }

    impl Model {
        fn insert(&mut self, key: u8, bytes: usize) -> bool {
            if bytes > self.budget {
                return false;
            }
            self.list.retain(|(k, _)| *k != key);
            while self.list.iter().map(|(_, b)| b).sum::<usize>() + bytes > self.budget {
                self.list.remove(0);
            }
            self.list.push((key, bytes));
            true
        }

        fn get(&mut self, key: u8) -> bool {
            match self.list.iter().position(|(k, _)| *k == key) {
                Some(at) => {
                    let entry = self.list.remove(at);
                    self.list.push(entry);
                    true
                }
                None => false,
            }
        }
    }

    /// The property: over random sequences of inserts, gets and removes,
    /// the LRU never holds more than its budget, its byte count is the sum
    /// of its entries, and it evicts exactly what an LRU must (a naive
    /// model agrees on every step).
    #[test]
    fn the_lru_never_exceeds_its_budget() {
        for seed in 0..500 {
            let mut rng = Rng::new(seed);
            let budget = rng.below(4000) as usize;
            let mut lru: ByteLru<u8, usize> = ByteLru::new(budget);
            let mut model = Model {
                budget,
                list: Vec::new(),
            };
            for step in 0..300 {
                let key = rng.below(24) as u8;
                match rng.below(10) {
                    0..=4 => {
                        let bytes = rng.below(budget as u64 / 3 + 50) as usize;
                        let inserted = lru.insert(key, bytes, bytes).is_ok();
                        assert_eq!(
                            inserted,
                            model.insert(key, bytes),
                            "seed {seed} step {step}"
                        );
                    }
                    5..=8 => {
                        let hit = lru.get(&key).copied();
                        assert_eq!(hit.is_some(), model.get(key), "seed {seed} step {step}");
                        if let Some(bytes) = hit {
                            assert_eq!(model.list.last(), Some(&(key, bytes)));
                        }
                    }
                    _ => {
                        let removed = lru.remove(&key);
                        let at = model.list.iter().position(|(k, _)| *k == key);
                        assert_eq!(removed.is_some(), at.is_some(), "seed {seed} step {step}");
                        if let Some(at) = at {
                            model.list.remove(at);
                        }
                    }
                }
                assert!(lru.bytes() <= budget, "seed {seed} step {step}");
                let entries: usize = model.list.iter().map(|(_, b)| b).sum();
                assert_eq!(lru.bytes(), entries, "seed {seed} step {step}");
                assert_eq!(
                    lru.keys_by_age(),
                    model.list.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
                    "seed {seed} step {step}"
                );
            }
        }
    }

    #[test]
    fn the_lru_reports_what_went() {
        let mut lru: ByteLru<&str, ()> = ByteLru::new(100);
        assert_eq!(lru.insert("a", (), 40), Ok(vec![]));
        assert_eq!(lru.insert("b", (), 40), Ok(vec![]));
        assert!(lru.get(&"a").is_some(), "a is now the newest");
        assert_eq!(lru.insert("c", (), 40), Ok(vec!["b"]));
        assert_eq!(lru.insert("a", (), 10), Ok(vec!["a"]), "replaced");
        assert_eq!(lru.bytes(), 50);
        assert_eq!(lru.insert("huge", (), 101), Err("huge"));
        assert_eq!(lru.insert("full", (), 100), Ok(vec!["c", "a"]));
        assert_eq!((lru.len(), lru.bytes()), (1, 100));
        assert_eq!(lru.remove(&"full"), Some(()));
        assert_eq!((lru.len(), lru.bytes()), (0, 0));
    }

    /// `len` bytes of black images, shared between answers (their content
    /// doesn't matter here; allocating them anew made the properties slow).
    fn shared_bytes(len: usize) -> Arc<[u8]> {
        thread_local! {
            static BYTES: std::cell::RefCell<BTreeMap<usize, Arc<[u8]>>> = Default::default();
        }
        BYTES.with(|bytes| {
            Arc::clone(
                bytes
                    .borrow_mut()
                    .entry(len)
                    .or_insert_with(|| vec![0; len].into()),
            )
        })
    }

    fn strip_data(file: &str, chunk: u32, height: u32, count: u32) -> PreviewData {
        let width = height * 16 / 9;
        PreviewData::Strip {
            file: file.into(),
            chunk,
            height,
            width,
            images: shared_bytes(image_bytes(width, height) * count as usize),
        }
    }

    fn frames_data(file: &str, first: u64, height: u32, count: u32) -> PreviewData {
        let width = height * 16 / 9;
        PreviewData::Frames {
            file: file.into(),
            first,
            width,
            height,
            images: shared_bytes(image_bytes(width, height) * count as usize),
        }
    }

    /// Images are slices of their chunk's bytes: the right bytes, shared,
    /// handed to glib without a copy (a texture wraps them as they are).
    #[test]
    fn images_are_slices_of_their_chunk() {
        let (width, height, count) = (4u32, 2u32, 3u32);
        let frame = image_bytes(width, height);
        let bytes: Arc<[u8]> = (0..frame * count as usize).map(|i| i as u8).collect();
        let images = Images {
            width,
            height,
            count,
            bytes: Arc::clone(&bytes),
        };
        for index in 0..count {
            let slice = images.slice(index);
            let start = index as usize * frame;
            assert_eq!(slice.as_ref(), &bytes[start..start + frame]);
            assert_eq!(slice.as_ref().as_ptr(), bytes[start..].as_ptr(), "no copy");
            let glib_bytes = glib::Bytes::from_owned(slice);
            assert_eq!(&glib_bytes[..], &bytes[start..start + frame]);
            assert_eq!(
                glib_bytes[..].as_ptr(),
                bytes[start..].as_ptr(),
                "glib shares it"
            );
        }
        assert_eq!(
            images.slice(7).as_ref(),
            images.slice(2).as_ref(),
            "clamped"
        );
        assert_eq!(
            Arc::strong_count(&bytes),
            2,
            "the slices went with their glib bytes"
        );
    }

    #[test]
    fn strips_map_seconds_to_chunks_and_images() {
        let now = Instant::now();
        let mut store = ThumbStore::new(BUDGET_BYTES);
        let key = ThumbKey::strip("a.mkv", 1, 72);
        // A miss asks once; asking again waits.
        let Look::Request(request) = store.look(&key, 5, now) else {
            panic!()
        };
        assert_eq!(
            request,
            PreviewRequest::Strip {
                file: "a.mkv".into(),
                chunk: 1,
                height: 72
            }
        );
        assert!(matches!(store.look(&key, 6, now), Look::Wait));
        assert_eq!(store.want(key.clone(), now), None, "pending");
        let arrival = store.arrive(&strip_data("a.mkv", 1, 72, 34), now);
        assert_eq!(arrival.file.as_deref(), Some("a.mkv"));
        assert!(store.pending().is_empty());
        assert_eq!(store.bytes(), image_bytes(128, 72) * 34);
        match store.look(&key, 5, now) {
            Look::Hit(5, images) => assert_eq!((images.width, images.count), (128, 34)),
            other => panic!("{other:?}"),
        }
        // Past the last image (the end of the file): the last one.
        assert!(matches!(store.look(&key, 59, now), Look::Hit(33, _)));
        // A chunk past the end has none, and isn't asked again.
        let past = ThumbKey::strip("a.mkv", 2, 72);
        assert!(matches!(store.look(&past, 0, now), Look::Request(_)));
        store.arrive(&strip_data("a.mkv", 2, 72, 0), now);
        assert!(matches!(store.look(&past, 0, now), Look::PastEnd));
        // Another height is another entry.
        assert!(matches!(
            store.look(&ThumbKey::strip("a.mkv", 1, 144), 5, now),
            Look::Request(_)
        ));
    }

    #[test]
    fn frames_come_in_aligned_blocks() {
        let now = Instant::now();
        let mut store = ThumbStore::new(BUDGET_BYTES);
        let key = ThumbKey::frames("a.mkv", 1000 / FRAME_BLOCK, 72);
        let Look::Request(request) = store.look(&key, (1000 % FRAME_BLOCK) as u32, now) else {
            panic!()
        };
        assert_eq!(
            request,
            PreviewRequest::Frames {
                file: "a.mkv".into(),
                first: 960,
                count: 64,
                height: 72
            }
        );
        // Someone else's unaligned frames aren't kept.
        assert!(
            store
                .arrive(&frames_data("a.mkv", 970, 72, 64), now)
                .file
                .is_none()
        );
        store.arrive(&frames_data("a.mkv", 960, 72, 20), now);
        assert!(matches!(store.look(&key, 19, now), Look::Hit(19, _)));
        assert!(
            matches!(store.look(&key, 20, now), Look::PastEnd),
            "past the end"
        );
    }

    #[test]
    fn a_failed_file_waits_before_it_is_asked_again() {
        let now = Instant::now();
        let mut store = ThumbStore::new(BUDGET_BYTES);
        let a = ThumbKey::strip("bad.mkv", 0, 72);
        let b = ThumbKey::strip("bad.mkv", 1, 72);
        let frames = ThumbKey::frames("bad.mkv", 0, 72);
        assert!(store.want(a.clone(), now).is_some());
        assert!(store.want(b.clone(), now).is_some());
        assert!(store.want(frames.clone(), now).is_some());
        let failed = PreviewData::Failed {
            file: "bad.mkv".into(),
            what: WHAT_STRIP,
            message: "ffmpeg exited with status 1".into(),
        };
        assert_eq!(store.arrive(&failed, now).file.as_deref(), Some("bad.mkv"));
        assert_eq!(store.pending().len(), 1, "the frames are still out");
        assert!(matches!(store.look(&a, 0, now), Look::Wait));
        assert_eq!(store.want(b.clone(), now + RETRY_AFTER / 2), None);
        assert!(
            store.want(b, now + RETRY_AFTER).is_some(),
            "asked again later"
        );
        // Other files and other answers don't care.
        assert!(
            store
                .want(ThumbKey::strip("good.mkv", 0, 72), now)
                .is_some()
        );
        let peaks = PreviewData::Failed {
            file: "good.mkv".into(),
            what: qrec_shell::media::preview::WHAT_PEAKS,
            message: String::new(),
        };
        assert!(store.arrive(&peaks, now).file.is_none());
    }

    #[test]
    fn an_entry_over_the_whole_budget_is_never_kept() {
        let now = Instant::now();
        let mut store = ThumbStore::new(image_bytes(128, 72) * 10);
        let key = ThumbKey::strip("a.mkv", 0, 72);
        assert!(store.want(key.clone(), now).is_some());
        let arrival = store.arrive(&strip_data("a.mkv", 0, 72, 60), now);
        assert_eq!(arrival.file.as_deref(), Some("a.mkv"));
        assert_eq!(store.bytes(), 0);
        assert!(
            matches!(store.look(&key, 0, now), Look::Wait),
            "not asked on every draw"
        );
    }

    fn key_of(request: &PreviewRequest) -> ThumbKey {
        match request {
            PreviewRequest::Strip {
                file,
                chunk,
                height,
            } => ThumbKey::strip(file, *chunk, *height),
            PreviewRequest::Frames {
                file,
                first,
                height,
                ..
            } => ThumbKey::frames(file, first / FRAME_BLOCK, *height),
            other => panic!("{other:?}"),
        }
    }

    /// The property over random request sequences (G§7 WP2): getters,
    /// prefetches, answers (in any order, of any size, some failed) and
    /// time passing never take the store over its budget; whatever it
    /// waits for has a request out; nothing is both held and waited for.
    #[test]
    fn random_requests_never_exceed_the_budget() {
        let files = ["a.mkv", "b.mkv", "c.mkv"];
        for seed in 0..300 {
            let mut rng = Rng::new(1000 + seed);
            let budget = image_bytes(128, 72) * rng.below(200) as usize;
            let mut store = ThumbStore::new(budget);
            let mut now = Instant::now();
            let mut out: Vec<PreviewRequest> = Vec::new();
            for step in 0..400 {
                let file = files[rng.below(3) as usize];
                let height = [72, 144][rng.below(2) as usize];
                match rng.below(8) {
                    0..=2 => {
                        let key = if rng.below(3) == 0 {
                            ThumbKey::frames(file, rng.below(40), height)
                        } else {
                            ThumbKey::strip(file, rng.below(60) as u32, height)
                        };
                        if let Look::Request(request) = store.look(&key, rng.below(64) as u32, now)
                        {
                            out.push(request);
                        }
                    }
                    3 => {
                        let key = ThumbKey::strip(file, rng.below(60) as u32, height);
                        out.extend(store.want(key, now));
                    }
                    4..=6 if !out.is_empty() => {
                        let request = out.swap_remove(rng.below(out.len() as u64) as usize);
                        let data = match &request {
                            _ if rng.below(10) == 0 => PreviewData::Failed {
                                file: request.file().to_owned(),
                                what: request.what(),
                                message: "no".into(),
                            },
                            PreviewRequest::Strip {
                                file,
                                chunk,
                                height,
                            } => strip_data(file, *chunk, *height, rng.below(62) as u32),
                            PreviewRequest::Frames {
                                file,
                                first,
                                height,
                                ..
                            } => frames_data(file, *first, *height, rng.below(65) as u32),
                            other => panic!("{other:?}"),
                        };
                        store.arrive(&data, now);
                    }
                    _ => now += Duration::from_secs(rng.below(20)),
                }
                assert!(store.bytes() <= budget, "seed {seed} step {step}");
                // Everything waited for has a request out (else it would
                // wait forever), and nothing is both held and waited for.
                let out_keys: BTreeSet<ThumbKey> = out.iter().map(key_of).collect();
                for key in store.pending() {
                    assert!(
                        out_keys.contains(key),
                        "seed {seed} step {step}: {key:?} waits for nothing"
                    );
                    assert!(
                        !store.lru.contains(key),
                        "seed {seed} step {step}: {key:?} held and waited for"
                    );
                }
            }
        }
    }

    /// G§8.6: scrolling a one-hour take (60 chunks) end to end at 72 and
    /// 144 px, with frame zoom on the way, keeps the store under its budget
    /// (the real files: the nested smoke run).
    #[test]
    fn an_hour_long_take_scrolls_under_budget() {
        for (budget, height) in [(BUDGET_BYTES, 72), (BUDGET_BYTES, 144), (32 << 20, 72)] {
            let mut store = ThumbStore::new(budget);
            let now = Instant::now();
            let mut peak = 0;
            // The view: 8 tiles of 60 s (fit zoom of a long take), moving 30 s at a time.
            for start in (0..3600u32).step_by(30) {
                for tile in 0..8 {
                    let t = (start + tile * 60).min(3599);
                    let key = ThumbKey::strip("hour.mkv", t / 60, height);
                    if let Look::Request(PreviewRequest::Strip { chunk, .. }) =
                        store.look(&key, t % 60, now)
                    {
                        store.arrive(&strip_data("hour.mkv", chunk, height, 60), now);
                    }
                    assert!(store.bytes() <= budget);
                }
                // Frame zoom at the playhead: 32 frames around it.
                let frame = u64::from(start) * 30;
                for f in frame..frame + 32 {
                    let key = ThumbKey::frames("hour.mkv", f / FRAME_BLOCK, height);
                    if let Look::Request(PreviewRequest::Frames { first, .. }) =
                        store.look(&key, (f % FRAME_BLOCK) as u32, now)
                    {
                        store.arrive(&frames_data("hour.mkv", first, height, 64), now);
                    }
                }
                peak = peak.max(store.bytes());
                assert!(store.bytes() <= budget, "{budget} at {start} s");
            }
            assert!(peak > 0);
            if height == 144 {
                assert!(peak > budget * 9 / 10, "an hour at 144 px fills the budget");
            }
        }
    }

    /// Texture creation in a nested compositor (G§8.4, G§8.6): a real
    /// preview cache on fixture media, its strips and frames as textures
    /// (made once, sharing the bytes, drawable by GSK), and `bytes()` under
    /// budget while a one-hour take scrolls. Run it inside the nested
    /// headless sway (never the user's display):
    /// `cargo test -p qrec-gui --lib -- --ignored --test-threads=1 nested`.
    #[test]
    #[ignore = "needs a Wayland display: run it in the nested headless compositor"]
    fn textures_in_a_nested_compositor() {
        nested::textures_and_an_hour_of_scrolling();
    }
}

/// The nested smoke run (an ignored test; also used by `peaks.rs`).
#[cfg(test)]
pub(crate) mod nested {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use crossbeam_channel::{Receiver, unbounded};
    use gtk::prelude::*;
    use gtk::{gdk, glib, graphene, gsk};
    use qrec_shell::media::preview::{PreviewCache, PreviewData, image_bytes};
    use qrec_shell::testkit::{MediaSpec, fixture};

    use super::*;
    use crate::media::peaks::PeakCache;

    pub(crate) const VIDEO: &str = "takes/take-0001/L1-screen-FAKE-1.mkv";

    /// GTK on the nested compositor's display, never the user's session
    /// (whose runtime dir is under `/run/user/`; the nested one is private).
    pub(crate) fn init_gtk() {
        let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
        assert!(
            !display.is_empty() && !runtime.is_empty() && !runtime.starts_with("/run/user/"),
            "refusing {display:?} in {runtime:?}: run this in the nested compositor"
        );
        gtk::init().expect("GTK on the nested compositor");
    }

    /// A project holding `files` (project-relative name → source), copied.
    pub(crate) fn project(files: &[(&str, &Path)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, source) in files {
            let to = dir.path().join(name);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(source, &to).unwrap();
        }
        dir
    }

    /// A preview cache whose answers come out of the channel.
    pub(crate) fn preview_cache(dir: &Path) -> (PreviewCache, Receiver<PreviewData>) {
        let (tx, rx) = unbounded();
        let cache = PreviewCache::new(
            PathBuf::from(dir),
            Box::new(move |data| {
                let _ = tx.send(data);
            }),
        );
        (cache, rx)
    }

    /// Iterates the main context, handing answers to `on_data`, until
    /// `done` (or the deadline: a panic).
    pub(crate) fn pump(
        rx: &Receiver<PreviewData>,
        on_data: &dyn Fn(&PreviewData),
        done: &dyn Fn() -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(120);
        let context = glib::MainContext::default();
        while !done() {
            assert!(Instant::now() < deadline, "the previews never came");
            while context.iteration(false) {}
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(data) => on_data(&data),
                Err(_) => continue,
            }
        }
    }

    /// The pixels of `texture`, as RGBA rows without padding.
    fn rgba(texture: &gdk::Texture) -> Vec<u8> {
        let mut downloader = gdk::TextureDownloader::new(texture);
        downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
        let (bytes, stride) = downloader.download_bytes();
        let row = texture.width() as usize * 4;
        bytes
            .chunks(stride)
            .take(texture.height() as usize)
            .flat_map(|r| r[..row].to_vec())
            .collect()
    }

    pub(crate) fn textures_and_an_hour_of_scrolling() {
        init_gtk();
        let video = fixture(&MediaSpec::video(192, 108, 30, 95 * 30));
        let dir = project(&[(VIDEO, &video)]);
        let (cache, rx) = preview_cache(dir.path());
        let thumbs = ThumbCache::new(cache.clone(), BUDGET_BYTES);
        let arrived = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let seen = std::rc::Rc::clone(&arrived);
        let _sub = thumbs.subscribe(move |file| seen.borrow_mut().push(file.to_owned()));
        let raw = std::rc::Rc::new(std::cell::RefCell::new(None::<PreviewData>));

        // A strip image: a miss, then a texture of the chunk's image.
        assert!(thumbs.strip_image(VIDEO, 65, 72).is_none());
        let keep = std::rc::Rc::clone(&raw);
        pump(
            &rx,
            &|data| {
                *keep.borrow_mut() = Some(data.clone());
                thumbs.on_data(data);
            },
            &|| thumbs.strip_image(VIDEO, 65, 72).is_some(),
        );
        assert_eq!(arrived.borrow().as_slice(), [VIDEO]);
        let texture = thumbs.strip_image(VIDEO, 65, 72).unwrap();
        assert_eq!((texture.width(), texture.height()), (128, 72));
        let Some(PreviewData::Strip { images, width, .. }) = raw.borrow().clone() else {
            panic!("no strip")
        };
        let frame = image_bytes(width, 72);
        assert_eq!(
            rgba(&texture),
            images[5 * frame..6 * frame],
            "image 5 of chunk 1"
        );
        let again = thumbs.strip_image(VIDEO, 65, 72).unwrap();
        assert_eq!(again, texture, "made once");
        assert_ne!(thumbs.strip_image(VIDEO, 66, 72).unwrap(), texture);

        // GSK draws it.
        let renderer = gsk::CairoRenderer::new();
        renderer
            .realize_for_display(&gdk::Display::default().expect("a display"))
            .expect("a renderer");
        let snapshot = gtk::Snapshot::new();
        snapshot.append_texture(&texture, &graphene::Rect::new(0.0, 0.0, 128.0, 72.0));
        let node = snapshot.to_node().expect("a node");
        let drawn = renderer.render_texture(&node, None);
        assert_eq!((drawn.width(), drawn.height()), (128, 72));
        let drawn = rgba(&drawn);
        let close = drawn
            .iter()
            .zip(&images[5 * frame..6 * frame])
            .all(|(a, b)| a.abs_diff(*b) <= 2);
        assert!(close, "the drawn texture is the image");
        renderer.unrealize();

        // A recorded frame (frame zoom).
        assert!(thumbs.frame(VIDEO, 1000, 144).is_none());
        pump(&rx, &|data| thumbs.on_data(data), &|| {
            thumbs.frame(VIDEO, 1000, 144).is_some()
        });
        let texture = thumbs.frame(VIDEO, 1000, 144).unwrap();
        assert_eq!((texture.width(), texture.height()), (256, 144));
        assert!(thumbs.frame(VIDEO, 1023, 144).is_some(), "the same block");
        assert!(thumbs.bytes() <= BUDGET_BYTES);

        // Peaks of an hour of audio, and an hour of video scrolled at fit
        // zoom with a budget that makes the cache evict (G§8.6).
        let hour = fixture(&MediaSpec::video(64, 36, 30, 3600 * 30).with_audio(
            48_000,
            1,
            &[(0, 1_800_000)],
        ));
        let dir = project(&[(VIDEO, &hour)]);
        let (cache, rx) = preview_cache(dir.path());
        let budget = 24 << 20;
        let thumbs = ThumbCache::new(cache.clone(), budget);
        let peaks = PeakCache::new(cache.clone(), crate::media::peaks::BUDGET_BYTES);
        let on_data = |data: &PreviewData| {
            thumbs.on_data(data);
            peaks.on_data(data);
            assert!(thumbs.bytes() <= budget);
            assert!(peaks.bytes() <= crate::media::peaks::BUDGET_BYTES);
        };
        assert!(peaks.peaks(VIDEO).is_none());
        pump(&rx, &on_data, &|| peaks.peaks(VIDEO).is_some());
        let levels = peaks.peaks(VIDEO).unwrap();
        assert!((3_599_999..=3_600_001).contains(&levels.per_ms.len()));
        let mut most = 0;
        for start in (0..3600u32).step_by(120) {
            let tiles: Vec<u32> = (0..8).map(|i| (start + i * 60).min(3599)).collect();
            for &t in &tiles {
                let _ = thumbs.strip_image(VIDEO, t, 72);
            }
            pump(&rx, &on_data, &|| {
                tiles
                    .iter()
                    .all(|&t| thumbs.strip_image(VIDEO, t, 72).is_some())
            });
            most = most.max(thumbs.bytes());
            assert!(thumbs.bytes() <= budget, "at {start} s");
        }
        assert!(most > budget / 2, "the scroll filled the budget: {most}");
        eprintln!(
            "an hour scrolled: at most {most} of {budget} thumbnail bytes; peaks {} buckets, \
             {} of {} bytes",
            levels.per_ms.len(),
            peaks.bytes(),
            crate::media::peaks::BUDGET_BYTES
        );
        cache.shutdown();
    }
}
