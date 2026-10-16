//! Thumbnails, recorded frames and waveform peaks for the GUI (docs/GUI.md
//! G§3.11): derived data, never State (SPEC §6.1: `.qrec/cache/` can be
//! deleted at any time). Owner: media.
//!
//! - **Requests** ([`PreviewCache::request`]) return at once (the GTK thread
//!   calls them) and are deduplicated: one already queued or running is
//!   joined, so it gets one answer. Each request gets exactly one answer
//!   through `on_data`, on a worker thread (`qrec-preview-<n>`): its data, or
//!   [`PreviewData::Failed`]. The newest request runs first: when the
//!   timeline scrolls, what is on screen now comes before what scrolled by.
//! - **ffmpeg** runs per A§8.7 (`media::child`): its own process group, a
//!   timeout of 20 s + 30 s/GiB of input, stdout drained on its own thread,
//!   killed on [`PreviewCache::shutdown`]. At most 2 children at once, 1
//!   while an export runs ([`PreviewCache::set_exporting`]). A strip or
//!   frames run first reads the video's size and rate from its header
//!   (`media_argv::preview_probe_argv`, once per file version).
//! - **Logs**: stderr is read line by line on its own thread and appended,
//!   `[<pid> <kind>]` first, to one rolling `.qrec/logs/preview.log` (moved
//!   to `preview.1.log` past 8 MiB); a failed run also writes its last 30
//!   lines to its own `YYYYMMDD-HHMMSS-<pid>-preview-<kind>.log`. The A§8.7
//!   prune counts only timestamp-named logs and leaves `preview*.log` alone
//!   (`media::is_stamped_log_name`).
//! - **Disk**: a strip chunk is `.qrec/cache/thumbnails/<key>.rgba` (its
//!   images back to back) plus `<key>.json` (`{width, height, count}`); a
//!   waveform is `.qrec/cache/waveforms/<key>.peaks` (the 1 ms level; the
//!   coarser ones are recomputed on load). Keys are
//!   `media_argv::preview_key` (the file's size and mtime included, like
//!   `SilenceKey`). Written to a temp file and renamed; a short or
//!   unreadable file is a miss and is made again; a hit refreshes the file's
//!   mtime. Recorded frames stay in memory (the GUI's caches).
//! - **Disk caps** (2 GiB of thumbnails, 512 MiB of waveforms) are enforced
//!   oldest first when a cache starts ([`trim_caches`], on its own thread):
//!   the GUI starts one when its host opened the project.
//! - [`PreviewRequest::CachedSilence`] only reads a silence analysis an
//!   autocut cached (`.qrec/cache/silence/<key>.json`, A§8.7), never runs
//!   ffmpeg.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use qrec_core::JobId;
use qrec_core::ctx::FileFact;
use qrec_core::media_argv::{
    MAX_PREVIEW_FRAMES, PEAK_SAMPLES_PER_MS, PreviewKind, STRIP_CHUNK_SECS, bucket_peaks,
    frames_argv, peak_levels, peaks_argv, preview_key, preview_probe_argv, preview_width,
    strip_argv,
};
use qrec_core::model::{Fps, StreamKind};
use qrec_core::parse::ffprobe::parse_ffprobe;
use qrec_core::protocol::Interval;
use qrec_core::state::SilenceKey;
use qrec_core::time::frame_ns;
use serde::{Deserialize, Serialize};

use super::child::{self, Spec, Stderr, Stdout, StopFlag};
use super::logs::{self, LogFile, TAIL_LINES};
use super::place::{is_temp_name, temp_path};
use crate::facts::file_fact;
use crate::paths::ProjectPaths;

/// A waveform at 1, 16 and 256 ms per bucket (`media_argv::PeakLevels`).
pub use qrec_core::media_argv::PeakLevels;

/// [`PreviewData::Failed`]'s `what` for each kind of request.
pub const WHAT_STRIP: &str = "thumbnails";
pub const WHAT_FRAMES: &str = "frames";
pub const WHAT_PEAKS: &str = "waveform";
pub const WHAT_SILENCE: &str = "silence analysis";

/// The thumbnails' disk cap (G§3.11).
pub const THUMBNAILS_CAP_BYTES: u64 = 2 << 30;
/// The waveforms' disk cap (G§3.11).
pub const WAVEFORMS_CAP_BYTES: u64 = 512 << 20;

/// The rolling log of every preview run, in `.qrec/logs/`.
pub const PREVIEW_LOG: &str = "preview.log";
/// Where [`PREVIEW_LOG`] moves past [`PREVIEW_LOG_MAX`] bytes.
pub const PREVIEW_LOG_OLD: &str = "preview.1.log";
pub const PREVIEW_LOG_MAX: u64 = 8 << 20;

/// The tallest image a request may ask for (the GUI asks 72 px × its
/// scale factor).
const MAX_HEIGHT: u32 = 1080;

/// Temp files older than this in the cache folders are swept by the trim.
const TEMP_MAX_AGE: Duration = Duration::from_secs(3600);

/// The most video headers remembered; past it they are all forgotten.
const MAX_VIDEO_INFOS: usize = 4096;

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

impl PreviewRequest {
    /// The project-relative file it is about.
    pub fn file(&self) -> &str {
        match self {
            PreviewRequest::Strip { file, .. }
            | PreviewRequest::Frames { file, .. }
            | PreviewRequest::Peaks { file }
            | PreviewRequest::CachedSilence { file, .. } => file,
        }
    }

    /// [`PreviewData::Failed`]'s `what` for this request.
    pub fn what(&self) -> &'static str {
        match self {
            PreviewRequest::Strip { .. } => WHAT_STRIP,
            PreviewRequest::Frames { .. } => WHAT_FRAMES,
            PreviewRequest::Peaks { .. } => WHAT_PEAKS,
            PreviewRequest::CachedSilence { .. } => WHAT_SILENCE,
        }
    }

    /// What two requests share when one joins the other (a silence
    /// threshold counts to 2 decimals, as in `SilenceKey`).
    fn identity(&self) -> String {
        match self {
            PreviewRequest::Strip {
                file,
                chunk,
                height,
            } => format!("strip\n{file}\n{chunk}\n{height}"),
            PreviewRequest::Frames {
                file,
                first,
                count,
                height,
            } => format!("frames\n{file}\n{first}\n{count}\n{height}"),
            PreviewRequest::Peaks { file } => format!("peaks\n{file}"),
            PreviewRequest::CachedSilence {
                file,
                threshold_db,
                min_silence_ms,
            } => format!("silence\n{file}\n{threshold_db:.2}\n{min_silence_ms}"),
        }
    }
}

/// What comes back.
#[derive(Clone)]
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

impl PreviewData {
    /// The project-relative file it is about.
    pub fn file(&self) -> &str {
        match self {
            PreviewData::Strip { file, .. }
            | PreviewData::Frames { file, .. }
            | PreviewData::Peaks { file, .. }
            | PreviewData::Silence { file, .. }
            | PreviewData::Failed { file, .. } => file,
        }
    }

    /// How many images a `Strip` or `Frames` answer holds (0 for others).
    pub fn image_count(&self) -> usize {
        match self {
            PreviewData::Strip {
                width,
                height,
                images,
                ..
            }
            | PreviewData::Frames {
                width,
                height,
                images,
                ..
            } => images
                .len()
                .checked_div(image_bytes(*width, *height))
                .unwrap_or(0),
            _ => 0,
        }
    }
}

/// Sizes, not the megabytes of RGBA.
impl std::fmt::Debug for PreviewData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreviewData::Strip {
                file,
                chunk,
                height,
                width,
                images,
            } => f
                .debug_struct("Strip")
                .field("file", file)
                .field("chunk", chunk)
                .field("size", &format_args!("{width}x{height}"))
                .field("bytes", &images.len())
                .finish(),
            PreviewData::Frames {
                file,
                first,
                width,
                height,
                images,
            } => f
                .debug_struct("Frames")
                .field("file", file)
                .field("first", first)
                .field("size", &format_args!("{width}x{height}"))
                .field("bytes", &images.len())
                .finish(),
            PreviewData::Peaks { file, levels } => f
                .debug_struct("Peaks")
                .field("file", file)
                .field("levels", levels)
                .finish(),
            PreviewData::Silence {
                file,
                key,
                intervals,
            } => f
                .debug_struct("Silence")
                .field("file", file)
                .field("key", key)
                .field("intervals", intervals)
                .finish(),
            PreviewData::Failed {
                file,
                what,
                message,
            } => f
                .debug_struct("Failed")
                .field("file", file)
                .field("what", what)
                .field("message", message)
                .finish(),
        }
    }
}

/// The bytes of one `width` × `height` RGBA image.
pub fn image_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

// ------------------------------------------------------------- the cache

/// The project's preview cache and its ffmpeg runs. Cheap to clone; the
/// workers stop when the last clone is dropped (or at [`Self::shutdown`]).
#[derive(Clone)]
pub struct PreviewCache {
    handle: Arc<Handle>,
}

/// Stops the workers when the last [`PreviewCache`] goes.
struct Handle {
    shared: Arc<Shared>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.stop();
    }
}

impl std::fmt::Debug for PreviewCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewCache")
            .field("root", &self.handle.shared.paths.root())
            .field("in_flight", &self.in_flight())
            .finish_non_exhaustive()
    }
}

/// Limits and programs of one cache (the tests shrink and swap them).
#[derive(Debug, Clone)]
pub(crate) struct Options {
    pub ffmpeg: OsString,
    pub ffprobe: OsString,
    pub workers: usize,
    /// Children at once (1 while exporting).
    pub max_children: usize,
    /// A run's timeout: `timeout + per_gib · input GiB`.
    pub timeout: Duration,
    pub per_gib: Duration,
    pub kill_grace: Duration,
    pub trim_on_start: bool,
    pub thumbnails_cap: u64,
    pub waveforms_cap: u64,
    pub log_max: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            workers: 2,
            max_children: 2,
            timeout: Duration::from_secs(20),
            per_gib: Duration::from_secs(30),
            kill_grace: child::KILL_GRACE,
            trim_on_start: true,
            thumbnails_cap: THUMBNAILS_CAP_BYTES,
            waveforms_cap: WAVEFORMS_CAP_BYTES,
            log_max: PREVIEW_LOG_MAX,
        }
    }
}

impl PreviewCache {
    /// A cache for `project_dir` that hands each result to `on_data`.
    /// Starts its workers, and trims the disk caches on a thread of its own.
    pub fn new(
        project_dir: impl Into<PathBuf>,
        on_data: Box<dyn Fn(PreviewData) + Send + Sync>,
    ) -> Self {
        Self::with_options(project_dir, on_data, Options::default())
    }

    pub(crate) fn with_options(
        project_dir: impl Into<PathBuf>,
        on_data: Box<dyn Fn(PreviewData) + Send + Sync>,
        options: Options,
    ) -> Self {
        let paths = ProjectPaths::new(project_dir);
        let shared = Arc::new(Shared {
            log: Arc::new(Mutex::new(RollingLog::new(
                paths.logs_dir(),
                options.log_max,
            ))),
            paths,
            on_data,
            sched: Mutex::new(Sched::default()),
            wake: Condvar::new(),
            stop: StopFlag::default(),
            videos: Mutex::new(BTreeMap::new()),
            stats: Stats::default(),
            workers: AtomicUsize::new(0),
            options,
        });
        for n in 1..=shared.options.workers {
            let worker = Arc::clone(&shared);
            let spawned = thread::Builder::new()
                .name(format!("qrec-preview-{n}"))
                .spawn(move || worker.work());
            match spawned {
                Ok(_) => {
                    shared.workers.fetch_add(1, Ordering::SeqCst);
                }
                Err(e) => tracing::error!("can't start a preview worker: {e}"),
            }
        }
        if shared.options.trim_on_start {
            let root = shared.paths.root().to_path_buf();
            let caps = (shared.options.thumbnails_cap, shared.options.waveforms_cap);
            let spawned = thread::Builder::new()
                .name("qrec-preview-trim".into())
                .spawn(move || {
                    let (thumbnails, waveforms) = trim_to(&root, caps, SystemTime::now());
                    let paths = ProjectPaths::new(root);
                    for (dir, result) in [
                        (paths.thumbnails_dir(), thumbnails),
                        (paths.waveforms_dir(), waveforms),
                    ] {
                        match result {
                            Ok(t) if t.removed > 0 => tracing::info!(
                                "trimmed {}: {} files, {} bytes freed, {} left",
                                dir.display(),
                                t.removed,
                                t.freed,
                                t.left
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!("can't trim {}: {e}", dir.display()),
                        }
                    }
                });
            if let Err(e) = spawned {
                tracing::warn!("can't start the preview cache trim: {e}");
            }
        }
        PreviewCache {
            handle: Arc::new(Handle { shared }),
        }
    }

    /// Asks for `req`'s data; a request already queued or running is
    /// joined. Returns at once.
    pub fn request(&self, req: PreviewRequest) {
        let shared = &self.handle.shared;
        if shared.workers.load(Ordering::SeqCst) == 0 {
            (shared.on_data)(PreviewData::Failed {
                file: req.file().to_owned(),
                what: req.what(),
                message: "the preview workers couldn't start".to_owned(),
            });
            return;
        }
        let id = req.identity();
        let mut sched = shared.lock_sched();
        if sched.stopped || !sched.pending.insert(id.clone()) {
            return;
        }
        sched.queue.push(Job { id, req });
        drop(sched);
        shared.wake.notify_all();
    }

    /// While an export runs, at most one preview child runs.
    pub fn set_exporting(&self, exporting: bool) {
        let shared = &self.handle.shared;
        let mut sched = shared.lock_sched();
        if sched.exporting != exporting {
            sched.exporting = exporting;
            drop(sched);
            shared.wake.notify_all();
        }
    }

    /// Kills the running children and stops the workers; queued requests
    /// are dropped and nothing more is answered. Returns at once (the
    /// children are gone within the kill grace: SIGTERM, then SIGKILL).
    pub fn shutdown(&self) {
        self.handle.shared.stop();
    }

    /// Requests queued or running (0: idle).
    pub fn in_flight(&self) -> usize {
        let sched = self.handle.shared.lock_sched();
        sched.queue.len() + sched.running
    }

    #[cfg(test)]
    fn stats(&self) -> &Stats {
        &self.handle.shared.stats
    }
}

/// What runs queue in.
#[derive(Debug, Default)]
struct Sched {
    /// Newest last: workers take the newest first.
    queue: Vec<Job>,
    /// The identities queued or running (dedup).
    pending: BTreeSet<String>,
    /// Requests being worked on.
    running: usize,
    /// Child slots taken.
    children: usize,
    exporting: bool,
    stopped: bool,
}

#[derive(Debug)]
struct Job {
    id: String,
    req: PreviewRequest,
}

/// Counters for the tests.
#[derive(Debug, Default)]
struct Stats {
    /// Children started (ffmpeg and ffprobe).
    spawns: AtomicU64,
    /// Children running now, and the most at once.
    running: AtomicUsize,
    max_running: AtomicUsize,
}

struct Shared {
    paths: ProjectPaths,
    options: Options,
    on_data: Box<dyn Fn(PreviewData) + Send + Sync>,
    sched: Mutex<Sched>,
    wake: Condvar,
    /// Set by `stop`: the running children are killed.
    stop: StopFlag,
    log: Arc<Mutex<RollingLog>>,
    videos: Mutex<BTreeMap<(String, u64, i64), VideoInfo>>,
    stats: Stats,
    workers: AtomicUsize,
}

/// What a strip or frames run needs from a video's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VideoInfo {
    width: u32,
    height: u32,
    fps: Option<Fps>,
}

/// A child slot (`Shared::slot`): freed on drop.
struct Slot<'a> {
    shared: &'a Shared,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut sched = self.shared.lock_sched();
        sched.children = sched.children.saturating_sub(1);
        drop(sched);
        self.shared.wake.notify_all();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn lock_sched(&self) -> MutexGuard<'_, Sched> {
        lock(&self.sched)
    }

    fn stop(&self) {
        let mut sched = self.lock_sched();
        sched.stopped = true;
        sched.queue.clear();
        sched.pending.clear();
        drop(sched);
        self.stop.set();
        self.wake.notify_all();
    }

    /// A worker: the newest job first, until stopped.
    fn work(self: Arc<Self>) {
        loop {
            let job = {
                let mut sched = self.lock_sched();
                loop {
                    if sched.stopped {
                        return;
                    }
                    if let Some(job) = sched.queue.pop() {
                        sched.running += 1;
                        break job;
                    }
                    sched = self
                        .wake
                        .wait(sched)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            };
            let data =
                catch_unwind(AssertUnwindSafe(|| self.run(&job.req))).unwrap_or_else(|panic| {
                    PreviewData::Failed {
                        file: job.req.file().to_owned(),
                        what: job.req.what(),
                        message: format!(
                            "internal error: {}",
                            child::panic_message(panic.as_ref())
                        ),
                    }
                });
            let deliver = {
                let mut sched = self.lock_sched();
                sched.running = sched.running.saturating_sub(1);
                sched.pending.remove(&job.id);
                !sched.stopped
            };
            self.wake.notify_all();
            if deliver && catch_unwind(AssertUnwindSafe(|| (self.on_data)(data))).is_err() {
                tracing::error!("the preview data handler panicked");
            }
        }
    }

    fn run(&self, req: &PreviewRequest) -> PreviewData {
        let result = match req {
            PreviewRequest::Strip {
                file,
                chunk,
                height,
            } => self.strip(file, *chunk, *height),
            PreviewRequest::Frames {
                file,
                first,
                count,
                height,
            } => self.frames(file, *first, *count, *height),
            PreviewRequest::Peaks { file } => self.peaks(file),
            PreviewRequest::CachedSilence {
                file,
                threshold_db,
                min_silence_ms,
            } => self.cached_silence(file, *threshold_db, *min_silence_ms),
        };
        result.unwrap_or_else(|message| PreviewData::Failed {
            file: req.file().to_owned(),
            what: req.what(),
            message,
        })
    }

    fn fact(&self, file: &str) -> Result<FileFact, String> {
        file_fact(&self.paths, file).map_err(|e| format!("can't read {file}: {e}"))
    }

    fn strip(&self, file: &str, chunk: u32, height: u32) -> Result<PreviewData, String> {
        check_height(height)?;
        let fact = self.fact(file)?;
        let key = strip_key(file, &fact, chunk, height);
        let dir = self.paths.thumbnails_dir();
        if let Some((meta, images)) = read_strip(&dir, &key, height) {
            return Ok(PreviewData::Strip {
                file: file.to_owned(),
                chunk,
                height,
                width: meta.width,
                images,
            });
        }
        let _slot = self.slot()?;
        let video = self.video_info(file, &fact)?;
        let width = preview_width(video.width, video.height, height);
        let frame = image_bytes(width, height);
        let most = STRIP_CHUNK_SECS as usize + 1;
        let bytes = self.run_child(
            "strip",
            &strip_argv(file, chunk, height),
            fact.bytes,
            Stdout::Capture {
                limit: (most + 1) * frame,
            },
        )?;
        let count = whole_images(&bytes, frame, most, width, height)?;
        let images: Arc<[u8]> = bytes.into();
        let meta = StripMeta {
            width,
            height,
            count: count as u32,
        };
        if let Err(e) = write_strip(&dir, &key, &meta, &images) {
            tracing::warn!(
                "can't cache the thumbnails of {file} in {}: {e}",
                dir.display()
            );
        }
        Ok(PreviewData::Strip {
            file: file.to_owned(),
            chunk,
            height,
            width,
            images,
        })
    }

    fn frames(
        &self,
        file: &str,
        first: u64,
        count: u32,
        height: u32,
    ) -> Result<PreviewData, String> {
        check_height(height)?;
        let count = count.min(MAX_PREVIEW_FRAMES);
        let fact = self.fact(file)?;
        let _slot = self.slot()?;
        let video = self.video_info(file, &fact)?;
        let width = preview_width(video.width, video.height, height);
        let empty = |images: Arc<[u8]>| PreviewData::Frames {
            file: file.to_owned(),
            first,
            width,
            height,
            images,
        };
        if count == 0 {
            return Ok(empty(Arc::from(Vec::new())));
        }
        let fps = video
            .fps
            .ok_or_else(|| format!("{file}: the video has no frame rate"))?;
        let start_ns = i64::try_from(first)
            .ok()
            .map(|f| frame_ns(f, fps))
            .ok_or_else(|| format!("{file}: frame {first} is out of range"))?;
        let frame = image_bytes(width, height);
        let bytes = self.run_child(
            "frames",
            &frames_argv(file, start_ns, count, height),
            fact.bytes,
            Stdout::Capture {
                limit: (count as usize + 1) * frame,
            },
        )?;
        whole_images(&bytes, frame, count as usize, width, height)?;
        Ok(empty(bytes.into()))
    }

    fn peaks(&self, file: &str) -> Result<PreviewData, String> {
        let fact = self.fact(file)?;
        let key = peaks_key(file, &fact);
        let dir = self.paths.waveforms_dir();
        if let Some(per_ms) = read_peaks(&dir, &key) {
            return Ok(PreviewData::Peaks {
                file: file.to_owned(),
                levels: Arc::new(peak_levels(per_ms.into())),
            });
        }
        let _slot = self.slot()?;
        let reader = Arc::new(Mutex::new(PeakReader::default()));
        let sink = Arc::clone(&reader);
        self.run_child(
            "peaks",
            &peaks_argv(file),
            fact.bytes,
            Stdout::Stream(Box::new(move |bytes| lock(&sink).push(bytes))),
        )?;
        let per_ms = std::mem::take(&mut *lock(&reader)).finish();
        if let Err(e) = write_atomic(&dir, &key, "peaks", &encode_peaks(&per_ms)) {
            tracing::warn!(
                "can't cache the waveform of {file} in {}: {e}",
                dir.display()
            );
        }
        Ok(PreviewData::Peaks {
            file: file.to_owned(),
            levels: Arc::new(peak_levels(per_ms.into())),
        })
    }

    fn cached_silence(
        &self,
        file: &str,
        threshold_db: f64,
        min_silence_ms: u32,
    ) -> Result<PreviewData, String> {
        let fact = self.fact(file)?;
        let key = SilenceKey::compute(
            file,
            fact.bytes,
            fact.mtime_ns,
            threshold_db,
            min_silence_ms,
        );
        let path = self
            .paths
            .silence_cache_dir()
            .join(format!("{}.json", key.0));
        let intervals = super::silence::read_cache(&path, &key);
        Ok(PreviewData::Silence {
            file: file.to_owned(),
            key,
            intervals,
        })
    }

    /// The video's size and rate from its header, once per file version.
    /// The caller holds a child slot.
    fn video_info(&self, file: &str, fact: &FileFact) -> Result<VideoInfo, String> {
        let key = (file.to_owned(), fact.bytes, fact.mtime_ns);
        if let Some(info) = lock(&self.videos).get(&key) {
            return Ok(*info);
        }
        let json = self.run_child(
            "probe",
            &preview_probe_argv(file),
            0,
            Stdout::Capture { limit: 1 << 16 },
        )?;
        let info = parse_ffprobe(&String::from_utf8_lossy(&json), StreamKind::Video)
            .map_err(|e| format!("{file}: {e}"))?;
        let (Some(width), Some(height)) = (info.width, info.height) else {
            return Err(format!("{file}: the video stream has no picture size"));
        };
        let video = VideoInfo {
            width,
            height,
            fps: info.fps,
        };
        let mut videos = lock(&self.videos);
        if videos.len() >= MAX_VIDEO_INFOS {
            videos.clear();
        }
        videos.insert(key, video);
        Ok(video)
    }

    /// Waits for a child slot: fewer than 2 children (1 while exporting).
    fn slot(&self) -> Result<Slot<'_>, String> {
        let mut sched = self.lock_sched();
        loop {
            if sched.stopped {
                return Err("cancelled: the preview cache stopped".to_owned());
            }
            let limit = if sched.exporting {
                1
            } else {
                self.options.max_children.max(1)
            };
            if sched.children < limit {
                sched.children += 1;
                return Ok(Slot { shared: self });
            }
            sched = self
                .wake
                .wait(sched)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Runs one child (A§8.7) and returns what it printed on stdout
    /// (captured; nothing for a stream). Its stderr goes to the rolling
    /// log; a failed run also gets its own log with its last lines.
    fn run_child(
        &self,
        kind: &'static str,
        argv: &[String],
        input_bytes: u64,
        stdout: Stdout,
    ) -> Result<Vec<u8>, String> {
        let program = argv.first().map_or("ffmpeg", String::as_str);
        let mut os_argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
        match program {
            "ffmpeg" => os_argv[0] = self.options.ffmpeg.clone(),
            "ffprobe" => os_argv[0] = self.options.ffprobe.clone(),
            _ => {}
        }
        let tail = Arc::new(Mutex::new(Tail::default()));
        let lines = Arc::clone(&tail);
        let log = Arc::clone(&self.log);
        let command = logs::shell_words(argv);
        let header = command.clone();
        let on_line = move |pid: u32, line: &str| {
            let mut lines = lock(&lines);
            let mut log = lock(&log);
            if !lines.started {
                lines.started = true;
                log.append(&format!("[{pid} {kind}] $ {header}"));
            }
            log.append(&format!("[{pid} {kind}] {line}"));
            lines.push(line);
        };
        // The command (if nothing was printed) and how the run ended.
        let ended = |pid: &str, how: &str| {
            let mut lines = lock(&tail);
            let mut log = lock(&self.log);
            if !lines.started {
                lines.started = true;
                log.append(&format!("[{pid} {kind}] $ {command}"));
            }
            log.append(&format!("[{pid} {kind}] # {how}"));
        };
        let timeout = scaled(self.options.timeout, self.options.per_gib, input_bytes);
        let spec = Spec::new(os_argv, Stderr::Lines(Box::new(on_line)))
            .cwd(self.paths.root())
            .stdout(stdout)
            .timeout(timeout)
            .stop(&self.stop)
            .kill_grace(self.options.kill_grace);
        self.stats.spawns.fetch_add(1, Ordering::SeqCst);
        let now = self.stats.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.stats.max_running.fetch_max(now, Ordering::SeqCst);
        let done = child::start(spec);
        self.stats.running.fetch_sub(1, Ordering::SeqCst);
        let done = match done {
            Ok(done) => done,
            Err(e) => {
                ended("-", &e.message);
                return Err(e.message);
            }
        };
        ended(
            &done.pid.to_string(),
            &format!(
                "{} in {}",
                done.describe(program),
                child::human(done.elapsed)
            ),
        );
        if done.exited_ok() {
            if let Some(error) = done.parser_error {
                return Err(format!("internal error: {error}"));
            }
            return Ok(done.stdout);
        }
        let mut message = done.describe(program);
        if done.ended == child::Ended::Stopped {
            return Err(message);
        }
        let tail = std::mem::take(&mut *lock(&tail));
        if let Some(last) = tail.lines.last() {
            message.push_str(&format!(": {last}"));
        }
        match self.failure_log(kind, argv, done.pid, &tail, &message) {
            Ok(path) => Err(format!("{message} (log: {path})")),
            Err(e) => {
                tracing::warn!("can't write the log of a failed preview run: {e}");
                Err(message)
            }
        }
    }

    /// A failed run's own log, `<stamp>-<our pid>-preview-<kind>.log`: its
    /// command, its last 30 lines, why (with the child's pid, as in
    /// `preview.log`).
    fn failure_log(
        &self,
        kind: &str,
        argv: &[String],
        pid: u32,
        tail: &Tail,
        message: &str,
    ) -> Result<String, String> {
        let base = format!("{}-{}-preview-{kind}", logs::stamp(), std::process::id());
        let log = LogFile::create(&self.paths.logs_dir(), &base, argv, self.paths.root())
            .map_err(|e| e.to_string())?;
        let mut file = log.file();
        for line in &tail.lines {
            let _ = writeln!(file, "{line}");
        }
        log.note(&format!("{message} (pid {pid})"));
        Ok(log.display())
    }
}

/// `base` plus `per_gib` in proportion to `bytes` (A§8.7).
fn scaled(base: Duration, per_gib: Duration, bytes: u64) -> Duration {
    let extra_ns = per_gib.as_nanos().saturating_mul(u128::from(bytes)) >> 30;
    base.saturating_add(Duration::from_nanos(
        u64::try_from(extra_ns).unwrap_or(u64::MAX),
    ))
}

fn check_height(height: u32) -> Result<(), String> {
    if height == 0 || height > MAX_HEIGHT {
        return Err(format!("{height} px is no image height (1–{MAX_HEIGHT})"));
    }
    Ok(())
}

/// How many whole `frame`-byte images `bytes` holds (at most `most`).
fn whole_images(
    bytes: &[u8],
    frame: usize,
    most: usize,
    width: u32,
    height: u32,
) -> Result<usize, String> {
    if frame == 0 || !bytes.len().is_multiple_of(frame) {
        return Err(format!(
            "ffmpeg printed {} bytes, not whole {width}×{height} RGBA images",
            bytes.len()
        ));
    }
    let count = bytes.len() / frame;
    if count > most {
        return Err(format!(
            "ffmpeg printed {count} images; at most {most} were asked for"
        ));
    }
    Ok(count)
}

/// The last [`TAIL_LINES`] lines of a run's stderr.
#[derive(Debug, Default)]
struct Tail {
    started: bool,
    lines: Vec<String>,
}

impl Tail {
    fn push(&mut self, line: &str) {
        if self.lines.len() == TAIL_LINES {
            self.lines.remove(0);
        }
        self.lines.push(line.to_owned());
    }
}

// --------------------------------------------------------------- the log

/// `.qrec/logs/preview.log`, appended to by every run and never truncated,
/// moved to `preview.1.log` past its limit. Lines of concurrent runs
/// interleave; each starts with its pid and kind.
struct RollingLog {
    dir: PathBuf,
    max: u64,
    file: Option<File>,
    size: u64,
    warned: bool,
}

impl RollingLog {
    fn new(dir: PathBuf, max: u64) -> Self {
        Self {
            dir,
            max,
            file: None,
            size: 0,
            warned: false,
        }
    }

    fn append(&mut self, line: &str) {
        if self.file.is_none()
            && let Err(e) = self.open()
        {
            if !self.warned {
                self.warned = true;
                tracing::warn!("can't write {}: {e}", self.dir.join(PREVIEW_LOG).display());
            }
            return;
        }
        let Some(file) = &mut self.file else { return };
        let mut text = String::with_capacity(line.len() + 1);
        text.push_str(line);
        text.push('\n');
        if file.write_all(text.as_bytes()).is_err() {
            self.file = None;
            return;
        }
        self.size += text.len() as u64;
        if self.size > self.max {
            self.rotate();
        }
    }

    fn open(&mut self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(PREVIEW_LOG))?;
        self.size = file.metadata()?.len();
        self.file = Some(file);
        if self.size > self.max {
            self.rotate();
        }
        Ok(())
    }

    /// `preview.log` → `preview.1.log` (replacing it); the next line opens
    /// a new `preview.log`.
    fn rotate(&mut self) {
        self.file = None;
        self.size = 0;
        if let Err(e) = fs::rename(self.dir.join(PREVIEW_LOG), self.dir.join(PREVIEW_LOG_OLD)) {
            tracing::warn!("can't rotate {}: {e}", self.dir.join(PREVIEW_LOG).display());
        }
    }
}

// ------------------------------------------------------------- waveforms

/// Bytes of one millisecond of [`peaks_argv`]'s mono s16le.
const BYTES_PER_MS: usize = PEAK_SAMPLES_PER_MS * 2;

/// Buckets ffmpeg's samples as they arrive: whole milliseconds at a time,
/// the rest with the next read (the same buckets as the whole stream's).
#[derive(Debug, Default)]
struct PeakReader {
    carry: Vec<u8>,
    per_ms: Vec<(i16, i16)>,
}

impl PeakReader {
    fn push(&mut self, bytes: &[u8]) {
        self.carry.extend_from_slice(bytes);
        let whole = self.carry.len() / BYTES_PER_MS * BYTES_PER_MS;
        if whole > 0 {
            self.bucket(whole);
            self.carry.drain(..whole);
        }
    }

    fn finish(mut self) -> Vec<(i16, i16)> {
        let samples = self.carry.len() / 2 * 2;
        self.bucket(samples);
        self.per_ms
    }

    fn bucket(&mut self, bytes: usize) {
        let samples: Vec<i16> = self.carry[..bytes]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        self.per_ms
            .extend(bucket_peaks(&samples, PEAK_SAMPLES_PER_MS));
    }
}

/// What a `.peaks` file starts with.
const PEAKS_MAGIC: &[u8; 8] = b"QRECPK1\n";

/// `.peaks`: [`PEAKS_MAGIC`], the bucket count (u64 LE), then each bucket's
/// min and max (i16 LE).
fn encode_peaks(per_ms: &[(i16, i16)]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 + per_ms.len() * 4);
    bytes.extend_from_slice(PEAKS_MAGIC);
    bytes.extend_from_slice(&(per_ms.len() as u64).to_le_bytes());
    for &(lo, hi) in per_ms {
        bytes.extend_from_slice(&lo.to_le_bytes());
        bytes.extend_from_slice(&hi.to_le_bytes());
    }
    bytes
}

/// The buckets of a `.peaks` file; `None` for a short or foreign one.
fn decode_peaks(bytes: &[u8]) -> Option<Vec<(i16, i16)>> {
    let body = bytes.strip_prefix(PEAKS_MAGIC)?;
    let (count, body) = body.split_first_chunk::<8>()?;
    let count = usize::try_from(u64::from_le_bytes(*count)).ok()?;
    if body.len() != count.checked_mul(4)? {
        return None;
    }
    let per_ms: Vec<(i16, i16)> = body
        .chunks_exact(4)
        .map(|b| {
            (
                i16::from_le_bytes([b[0], b[1]]),
                i16::from_le_bytes([b[2], b[3]]),
            )
        })
        .collect();
    per_ms.iter().all(|(lo, hi)| lo <= hi).then_some(per_ms)
}

fn read_peaks(dir: &Path, key: &str) -> Option<Vec<(i16, i16)>> {
    let path = dir.join(format!("{key}.peaks"));
    let per_ms = decode_peaks(&fs::read(&path).ok()?)?;
    touch(&path);
    Some(per_ms)
}

fn peaks_key(file: &str, fact: &FileFact) -> String {
    preview_key(PreviewKind::Peaks, file, fact.bytes, fact.mtime_ns, "")
}

// ---------------------------------------------------------------- strips

/// `<key>.json` next to a strip's `<key>.rgba`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StripMeta {
    width: u32,
    height: u32,
    count: u32,
}

fn strip_key(file: &str, fact: &FileFact, chunk: u32, height: u32) -> String {
    preview_key(
        PreviewKind::Strip,
        file,
        fact.bytes,
        fact.mtime_ns,
        &format!("chunk={chunk} height={height}"),
    )
}

/// A cached chunk: both files there, and the images exactly the size the
/// metadata says.
fn read_strip(dir: &Path, key: &str, height: u32) -> Option<(StripMeta, Arc<[u8]>)> {
    let meta_path = dir.join(format!("{key}.json"));
    let meta: StripMeta = serde_json::from_slice(&fs::read(&meta_path).ok()?).ok()?;
    if meta.height != height || meta.width == 0 || meta.count > STRIP_CHUNK_SECS + 1 {
        return None;
    }
    let expected = image_bytes(meta.width, meta.height).checked_mul(meta.count as usize)?;
    let path = dir.join(format!("{key}.rgba"));
    let mut file = File::open(&path).ok()?;
    if file.metadata().ok()?.len() != expected as u64 {
        return None;
    }
    let mut images = Vec::with_capacity(expected);
    file.read_to_end(&mut images).ok()?;
    if images.len() != expected {
        return None;
    }
    touch(&meta_path);
    Some((meta, images.into()))
}

/// The images first, then the metadata that makes them a hit.
fn write_strip(dir: &Path, key: &str, meta: &StripMeta, images: &[u8]) -> io::Result<()> {
    write_atomic(dir, key, "rgba", images)?;
    let json = serde_json::to_vec(meta).map_err(io::Error::other)?;
    write_atomic(dir, key, "json", &json)
}

/// Numbers the temp files of this process's cache writes.
static TEMP_SERIAL: AtomicU64 = AtomicU64::new(1);

/// `<dir>/<stem>.<ext>` through a temp file (`.<stem>.qrec-part-<pid>-<n>.<ext>`,
/// A§8.8's temp shape) and a rename. Same key, same content: replacing
/// another write of it is fine. No fsync: a file cut short by a crash is a
/// miss and is made again.
fn write_atomic(dir: &Path, stem: &str, ext: &str, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = temp_path(
        dir,
        stem,
        JobId(TEMP_SERIAL.fetch_add(1, Ordering::Relaxed)),
        ext,
    );
    let result = (|| {
        let mut file = File::create_new(&tmp)?;
        file.write_all(bytes)?;
        fs::rename(&tmp, dir.join(format!("{stem}.{ext}")))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// A hit is recent: the trim removes the least recently used first.
fn touch(path: &Path) {
    if let Ok(file) = OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

// ------------------------------------------------------------------ trim

/// What [`trim_dir`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trimmed {
    /// Files removed (stale temp files included).
    pub removed: usize,
    pub freed: u64,
    /// What the folder holds now.
    pub left: u64,
}

/// The disk caps of G§3.11 on the project at `project_dir`: thumbnails
/// over 2 GiB and waveforms over 512 MiB lose their oldest entries first.
/// [`PreviewCache::new`] runs this on a thread of its own.
pub fn trim_caches(project_dir: &Path) -> (io::Result<Trimmed>, io::Result<Trimmed>) {
    trim_to(
        project_dir,
        (THUMBNAILS_CAP_BYTES, WAVEFORMS_CAP_BYTES),
        SystemTime::now(),
    )
}

/// Thumbnails to `caps.0` bytes, waveforms to `caps.1`.
fn trim_to(
    project_dir: &Path,
    caps: (u64, u64),
    now: SystemTime,
) -> (io::Result<Trimmed>, io::Result<Trimmed>) {
    let paths = ProjectPaths::new(project_dir);
    (
        trim_dir(&paths.thumbnails_dir(), caps.0, now),
        trim_dir(&paths.waveforms_dir(), caps.1, now),
    )
}

/// Keeps `dir` (a cache folder) at most `cap` bytes: entries (the files of
/// one key: `<key>.rgba` and `<key>.json` go together) are removed least
/// recently written or hit first. Temp files older than an hour go too
/// (younger ones may be in the middle of a write) and don't count.
pub fn trim_dir(dir: &Path, cap: u64, now: SystemTime) -> io::Result<Trimmed> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Trimmed::default()),
        Err(e) => return Err(e),
    };
    let mut trimmed = Trimmed::default();
    // stem → (newest mtime, bytes, files)
    let mut groups: BTreeMap<String, (SystemTime, u64, Vec<PathBuf>)> = BTreeMap::new();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name();
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if is_temp_name(&name) {
            let old = now
                .duration_since(mtime)
                .is_ok_and(|age| age >= TEMP_MAX_AGE);
            if old && fs::remove_file(entry.path()).is_ok() {
                trimmed.removed += 1;
                trimmed.freed += meta.len();
            }
            continue;
        }
        let name = name.to_string_lossy();
        let stem = name.split('.').next().unwrap_or_default().to_owned();
        let group = groups
            .entry(stem)
            .or_insert((SystemTime::UNIX_EPOCH, 0, Vec::new()));
        group.0 = group.0.max(mtime);
        group.1 += meta.len();
        group.2.push(entry.path());
    }
    let mut total: u64 = groups.values().map(|g| g.1).sum();
    let mut oldest: Vec<(SystemTime, &String)> = groups.iter().map(|(s, g)| (g.0, s)).collect();
    oldest.sort();
    for (_, stem) in oldest {
        if total <= cap {
            break;
        }
        let (_, bytes, files) = &groups[stem];
        for file in files {
            if fs::remove_file(file).is_ok() {
                trimmed.removed += 1;
            }
        }
        total = total.saturating_sub(*bytes);
        trimmed.freed += bytes;
    }
    trimmed.left = total;
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::time::Instant;

    use crossbeam_channel::{Receiver, unbounded};
    use qrec_core::hash::sha256_hex;

    use super::*;
    use crate::testkit::{MediaSpec, fixture, fixtures_dir, run_tool};

    const WAIT: Duration = Duration::from_secs(90);
    const VIDEO: &str = "takes/take-0001/L1-screen-FAKE-1.mkv";
    const AUDIO: &str = "takes/take-0001/A1-mic.mka";
    /// Bars of the frame-ID videos: bit `i` of the frame index, left to right.
    const ID_BITS: u32 = 12;

    /// `frames` frames of 192×108 video at `fps` whose luma shows the frame
    /// index as [`ID_BITS`] vertical bars (white = 1); H.264 with a keyframe
    /// exactly every second (no scene-cut keyframes: the strips decode only
    /// keyframes), in Matroska (millisecond timestamps, like recordings).
    fn id_video(fps: Fps, frames: u64) -> PathBuf {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let key = format!(
            "qrec preview ids v1 192x108 {}/{} {frames}",
            fps.num, fps.den
        );
        let path = fixtures_dir().join(format!("{}.mkv", sha256_hex(key.as_bytes())));
        if fs::metadata(&path).is_ok_and(|m| m.len() > 0) {
            return path;
        }
        let tmp = path.with_extension(format!(
            "{}-{}.part.mkv",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let graph = format!(
            "nullsrc=size=192x108:rate={}/{},trim=end_frame={frames},setpts=PTS-STARTPTS,\
             format=yuv420p,geq=lum='if(bitand(N\\,pow(2\\,floor(X*{ID_BITS}/W)))\\,235\\,16)':cb=128:cr=128[v]",
            fps.num, fps.den
        );
        let gop = fps.num.div_ceil(fps.den).to_string();
        let out = run_tool(
            Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-nostdin",
                    "-nostats",
                    "-loglevel",
                    "error",
                    "-y",
                ])
                .args(["-filter_complex", &graph, "-map", "[v]"])
                .args([
                    "-c:v", "libx264", "-preset", "veryfast", "-crf", "16", "-bf", "0",
                ])
                .args(["-g", &gop, "-keyint_min", &gop, "-sc_threshold", "0"])
                .args(["-pix_fmt", "yuv420p", "-f", "matroska"])
                .arg(format!("file:{}", tmp.display())),
            Duration::from_secs(120),
        );
        assert!(out.status.success(), "frame-ID video: {}", out.stderr);
        fs::rename(&tmp, &path).unwrap();
        path
    }

    /// The frame index an image of an [`id_video`] shows.
    fn id_of(image: &[u8], width: u32, height: u32) -> u64 {
        let y = height / 2;
        (0..ID_BITS).fold(0, |id, bit| {
            let x = (2 * bit + 1) * width / (2 * ID_BITS);
            let red = image[((y * width + x) * 4) as usize];
            id | (u64::from(red > 128) << bit)
        })
    }

    /// The frame indexes of every image of a `Strip` or `Frames` answer.
    fn ids(data: &PreviewData) -> Vec<u64> {
        let (width, height, images) = match data {
            PreviewData::Strip {
                width,
                height,
                images,
                ..
            }
            | PreviewData::Frames {
                width,
                height,
                images,
                ..
            } => (*width, *height, images),
            other => panic!("no images: {other:?}"),
        };
        images
            .chunks(image_bytes(width, height))
            .map(|image| id_of(image, width, height))
            .collect()
    }

    /// A project folder holding `files` (project-relative name → source).
    fn project(files: &[(&str, &Path)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, source) in files {
            let to = dir.path().join(name);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::copy(source, &to).unwrap();
        }
        dir
    }

    fn quick() -> Options {
        Options {
            trim_on_start: false,
            kill_grace: Duration::from_millis(300),
            ..Options::default()
        }
    }

    /// A cache whose answers come out of the channel.
    fn open(dir: &Path, options: Options) -> (PreviewCache, Receiver<PreviewData>) {
        let (tx, rx) = unbounded();
        let cache = PreviewCache::with_options(
            dir,
            Box::new(move |data| {
                let _ = tx.send(data);
            }),
            options,
        );
        (cache, rx)
    }

    fn next(rx: &Receiver<PreviewData>) -> PreviewData {
        rx.recv_timeout(WAIT).expect("an answer within the wait")
    }

    fn strip(chunk: u32, height: u32) -> PreviewRequest {
        PreviewRequest::Strip {
            file: VIDEO.into(),
            chunk,
            height,
        }
    }

    fn spawns(cache: &PreviewCache) -> u64 {
        cache.stats().spawns.load(Ordering::SeqCst)
    }

    /// An executable script in `dir`.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// ffmpeg, `secs` later.
    fn slow_ffmpeg(dir: &Path, secs: f64) -> OsString {
        script(
            dir,
            "slow-ffmpeg",
            &format!("sleep {secs}\nexec ffmpeg \"$@\""),
        )
        .into()
    }

    /// The acceptance check (G§7 WP2): a 95 s video gives chunk 0 with 60
    /// images and chunk 1 with 35 (± 1), 72 px high, image `i` being file
    /// time `60·chunk + i` s; a chunk past the end has none.
    #[test]
    fn strips_of_a_95_second_video() {
        let fps = Fps::new(30, 1);
        let dir = project(&[(VIDEO, &id_video(fps, 95 * 30))]);
        let (cache, rx) = open(dir.path(), quick());
        cache.request(strip(0, 72));
        cache.request(strip(1, 72));
        cache.request(strip(2, 72));
        let mut chunks = BTreeMap::new();
        for _ in 0..3 {
            match next(&rx) {
                data @ PreviewData::Strip { .. } => {
                    let PreviewData::Strip {
                        chunk,
                        width,
                        height,
                        ..
                    } = &data
                    else {
                        unreachable!()
                    };
                    assert_eq!((*width, *height), (128, 72), "{data:?}");
                    chunks.insert(*chunk, data);
                }
                other => panic!("{other:?}"),
            }
        }
        let ids0 = ids(&chunks[&0]);
        assert_eq!(chunks[&0].image_count(), 60);
        assert_eq!(ids0, (0..60).map(|i| 30 * i).collect::<Vec<u64>>());
        let ids1 = ids(&chunks[&1]);
        assert!((34..=36).contains(&ids1.len()), "{} images", ids1.len());
        assert_eq!(
            ids1,
            (0..ids1.len() as u64)
                .map(|i| 30 * (60 + i))
                .collect::<Vec<u64>>()
        );
        assert_eq!(chunks[&2].image_count(), 0, "past the end");
        assert_eq!(cache.in_flight(), 0);
    }

    /// The acceptance check: a `Frames` request returns exactly `count`
    /// consecutive recorded frames from `first` (fewer at the end), at 30
    /// and at 29.97 fps (Matroska's millisecond timestamps).
    #[test]
    fn frames_are_exactly_the_ones_asked_for() {
        for (fps, frames) in [(Fps::new(30, 1), 2850), (Fps::new(30000, 1001), 1200)] {
            let dir = project(&[(VIDEO, &id_video(fps, frames))]);
            let (cache, rx) = open(dir.path(), quick());
            let ask = |first: u64, count: u32, height: u32| {
                cache.request(PreviewRequest::Frames {
                    file: VIDEO.into(),
                    first,
                    count,
                    height,
                });
                let data = next(&rx);
                let PreviewData::Frames { first: at, .. } = &data else {
                    panic!("{data:?}");
                };
                assert_eq!(*at, first);
                data
            };
            for first in [0, 1, 29, 30, 31, 1000, 1001] {
                let data = ask(first, 64, 72);
                assert_eq!(
                    ids(&data),
                    (first..first + 64).collect::<Vec<u64>>(),
                    "{fps:?} from {first}"
                );
            }
            let data = ask(frames - 10, 64, 72);
            assert_eq!(
                ids(&data),
                (frames - 10..frames).collect::<Vec<u64>>(),
                "the end"
            );
            assert_eq!(ask(frames + 50, 8, 72).image_count(), 0, "past the end");
            let before = spawns(&cache);
            assert_eq!(ask(5, 0, 72).image_count(), 0);
            assert_eq!(spawns(&cache), before, "no frames: no ffmpeg");
            let data = ask(500, 3, 144);
            let PreviewData::Frames { width, height, .. } = &data else {
                unreachable!()
            };
            assert_eq!((*width, *height), (256, 144));
            assert_eq!(ids(&data), [500, 501, 502]);
            // At most 64.
            assert_eq!(ask(10, 500, 72).image_count(), 64);
        }
    }

    /// `scale=-2:<h>` is `preview_width` for odd sizes too (else the images
    /// wouldn't be whole).
    #[test]
    fn the_image_width_is_ffmpegs() {
        for (w, h) in [(162, 120), (100, 56), (90, 160), (320, 240)] {
            let dir = project(&[(VIDEO, &fixture(&MediaSpec::video(w, h, 25, 125)))]);
            let (cache, rx) = open(dir.path(), quick());
            cache.request(strip(0, 72));
            let data = next(&rx);
            let PreviewData::Strip { width, .. } = &data else {
                panic!("{w}x{h}: {data:?}");
            };
            assert_eq!(*width, preview_width(w, h, 72), "{w}x{h}");
            assert!(
                (4..=5).contains(&data.image_count()),
                "{w}x{h}: 5 s, {data:?}"
            );
        }
    }

    /// The acceptance check: peaks have one bucket per ms (mono at 48 kHz,
    /// and stereo at 44.1 kHz resampled), silent where the audio is.
    #[test]
    fn peaks_have_a_bucket_per_millisecond() {
        let speech = [(500, 1500)];
        let mono = fixture(&MediaSpec::audio(48_000, 1, 2_000, &speech));
        let stereo = fixture(&MediaSpec::audio(44_100, 2, 2_000, &speech));
        let video = fixture(&MediaSpec::video(64, 36, 25, 25));
        let dir = project(&[
            (AUDIO, &mono),
            ("takes/take-0001/A2-system.mka", &stereo),
            (VIDEO, &video),
        ]);
        let (cache, rx) = open(dir.path(), quick());
        for (file, exact) in [(AUDIO, true), ("takes/take-0001/A2-system.mka", false)] {
            cache.request(PreviewRequest::Peaks { file: file.into() });
            let data = next(&rx);
            let PreviewData::Peaks { levels, .. } = &data else {
                panic!("{data:?}");
            };
            let n = levels.per_ms.len();
            if exact {
                assert_eq!(n, 2000);
            } else {
                assert!((1999..=2001).contains(&n), "{n} buckets");
            }
            assert_eq!(levels.per_16ms.len(), n.div_ceil(16));
            assert_eq!(levels.per_256ms.len(), n.div_ceil(256));
            for ms in [10, 400, 1600, 1990] {
                assert_eq!(levels.per_ms[ms], (0, 0), "{file} is silent at {ms} ms");
            }
            for ms in [600, 1000, 1400] {
                // A millisecond is less than half a period of the 440 Hz
                // tone; 16 ms hold both halves.
                let (lo, hi) = levels.per_ms[ms];
                assert!(
                    hi.max(lo.saturating_neg()) > 12_000,
                    "{file} at {ms} ms: {lo} {hi}"
                );
                let (lo, hi) = levels.per_16ms[ms / 16];
                assert!(lo < -12_000 && hi > 12_000, "{file} at {ms} ms: {lo} {hi}");
            }
        }
        // A file without audio has no waveform.
        cache.request(PreviewRequest::Peaks { file: VIDEO.into() });
        match next(&rx) {
            PreviewData::Failed { what, message, .. } => {
                assert_eq!(what, WHAT_PEAKS);
                assert!(message.contains("ffmpeg exited with status"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn peaks_are_bucketed_as_they_arrive() {
        let samples: Vec<i16> = (0..10_000)
            .map(|i| ((i * 7919) % 65_536 - 32_768) as i16)
            .collect();
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut reader = PeakReader::default();
        let mut at = 0;
        for size in [1, 95, 96, 97, 1000, 3, 4096].iter().cycle() {
            if at >= bytes.len() {
                break;
            }
            let end = (at + size).min(bytes.len());
            reader.push(&bytes[at..end]);
            at = end;
        }
        assert_eq!(reader.finish(), bucket_peaks(&samples, PEAK_SAMPLES_PER_MS));

        let per_ms = vec![(-3, 5), (0, 0), (i16::MIN, i16::MAX)];
        let encoded = encode_peaks(&per_ms);
        assert_eq!(encoded.len(), 16 + 3 * 4);
        assert_eq!(decode_peaks(&encoded), Some(per_ms));
        assert_eq!(decode_peaks(&encoded[..encoded.len() - 1]), None, "short");
        assert_eq!(decode_peaks(b"QRECPK1\n"), None);
        let mut foreign = encoded.clone();
        foreign[0] = b'X';
        assert_eq!(decode_peaks(&foreign), None);
        let mut inverted = encoded;
        inverted[16..20].copy_from_slice(&[5, 0, 0xfd, 0xff]); // (5, −3)
        assert_eq!(decode_peaks(&inverted), None);
        assert_eq!(decode_peaks(&encode_peaks(&[])), Some(Vec::new()));
    }

    /// The acceptance checks: a hit spawns no ffmpeg (the spawn counter);
    /// requests made while one is queued or running join it, one answer
    /// for all; a new cache on the same project hits the disk.
    #[test]
    fn hits_spawn_nothing_and_requests_join() {
        let fps = Fps::new(30, 1);
        let mono = fixture(&MediaSpec::audio(48_000, 1, 1_500, &[(200, 700)]));
        let dir = project(&[(VIDEO, &id_video(fps, 95 * 30)), (AUDIO, &mono)]);
        let bin = tempfile::tempdir().unwrap();
        let options = Options {
            ffmpeg: slow_ffmpeg(bin.path(), 0.5),
            ..quick()
        };
        let (cache, rx) = open(dir.path(), options.clone());
        for _ in 0..5 {
            cache.request(strip(0, 72));
        }
        assert!(cache.in_flight() >= 1);
        let first = next(&rx);
        assert_eq!(first.image_count(), 60, "{first:?}");
        assert!(
            rx.recv_timeout(Duration::from_millis(1500)).is_err(),
            "five requests, one answer"
        );
        assert_eq!(spawns(&cache), 2, "the probe and one ffmpeg");
        assert_eq!(cache.in_flight(), 0);

        // A hit: the same answer, nothing spawned.
        cache.request(strip(0, 72));
        let again = next(&rx);
        assert_eq!(ids(&again), ids(&first));
        assert_eq!(spawns(&cache), 2);

        // Peaks: one ffmpeg, then hits.
        cache.request(PreviewRequest::Peaks { file: AUDIO.into() });
        cache.request(PreviewRequest::Peaks { file: AUDIO.into() });
        let PreviewData::Peaks { levels, .. } = next(&rx) else {
            panic!()
        };
        assert_eq!(levels.per_ms.len(), 1500);
        assert!(
            rx.recv_timeout(Duration::from_millis(1000)).is_err(),
            "joined"
        );
        assert_eq!(spawns(&cache), 3);
        cache.request(PreviewRequest::Peaks { file: AUDIO.into() });
        let PreviewData::Peaks { levels: cached, .. } = next(&rx) else {
            panic!()
        };
        assert_eq!(cached, levels);
        assert_eq!(spawns(&cache), 3);

        // Another cache (a later window) on the same project hits the disk.
        let (later, rx) = open(dir.path(), options);
        later.request(strip(0, 72));
        later.request(PreviewRequest::Peaks { file: AUDIO.into() });
        for _ in 0..2 {
            match next(&rx) {
                PreviewData::Strip { .. } | PreviewData::Peaks { .. } => {}
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(spawns(&later), 0);
    }

    /// The acceptance check: at most 2 children at once, 1 while an export
    /// runs.
    #[test]
    fn at_most_two_children_and_one_while_exporting() {
        let fps = Fps::new(30, 1);
        let dir = project(&[(VIDEO, &id_video(fps, 95 * 30))]);
        let bin = tempfile::tempdir().unwrap();
        let options = Options {
            ffmpeg: slow_ffmpeg(bin.path(), 0.3),
            workers: 3,
            ..quick()
        };
        let (cache, rx) = open(dir.path(), options.clone());
        for chunk in 0..6 {
            cache.request(strip(chunk, 72));
        }
        for _ in 0..6 {
            assert!(matches!(next(&rx), PreviewData::Strip { .. }));
        }
        assert_eq!(cache.stats().max_running.load(Ordering::SeqCst), 2);

        let (cache, rx) = open(dir.path(), options);
        cache.set_exporting(true);
        for chunk in 0..4 {
            cache.request(strip(chunk, 36));
        }
        for _ in 0..4 {
            assert!(matches!(next(&rx), PreviewData::Strip { .. }));
        }
        assert_eq!(cache.stats().max_running.load(Ordering::SeqCst), 1);
        // The export finished: two again.
        cache.set_exporting(false);
        for chunk in 0..4 {
            cache.request(strip(chunk, 48));
        }
        for _ in 0..4 {
            assert!(matches!(next(&rx), PreviewData::Strip { .. }));
        }
        assert_eq!(cache.stats().max_running.load(Ordering::SeqCst), 2);
    }

    /// The acceptance check: a short, foreign or mismatched cache file is a
    /// miss, and the data is made again (and cached again).
    #[test]
    fn corrupt_cache_files_are_made_again() {
        let fps = Fps::new(30, 1);
        let mono = fixture(&MediaSpec::audio(48_000, 1, 1_000, &[(100, 900)]));
        let dir = project(&[(VIDEO, &id_video(fps, 95 * 30)), (AUDIO, &mono)]);
        let paths = ProjectPaths::new(dir.path());
        let (cache, rx) = open(dir.path(), quick());
        cache.request(strip(0, 72));
        let good = next(&rx);
        let fact = file_fact(&paths, VIDEO).unwrap();
        let key = strip_key(VIDEO, &fact, 0, 72);
        let rgba = paths.thumbnails_dir().join(format!("{key}.rgba"));
        let json = paths.thumbnails_dir().join(format!("{key}.json"));
        let whole = fs::metadata(&rgba).unwrap().len();
        assert_eq!(whole, 60 * 128 * 72 * 4);

        let breakages: [(&Path, &[u8]); 4] = [
            (&rgba, b"short"),
            (&json, b"{not json"),
            (&json, br#"{"width":128,"height":72,"count":61}"#),
            (&json, br#"{"width":128,"height":72,"count":60,"extra":1}"#),
        ];
        for (file, garbage) in breakages {
            fs::write(file, garbage).unwrap();
            let before = spawns(&cache);
            cache.request(strip(0, 72));
            let made = next(&rx);
            assert_eq!(ids(&made), ids(&good));
            assert!(spawns(&cache) > before, "made again after {garbage:?}");
            assert_eq!(fs::metadata(&rgba).unwrap().len(), whole);
            let meta: StripMeta = serde_json::from_slice(&fs::read(&json).unwrap()).unwrap();
            assert_eq!(
                meta,
                StripMeta {
                    width: 128,
                    height: 72,
                    count: 60
                }
            );
        }
        // A missing half is a miss too.
        fs::remove_file(&json).unwrap();
        let before = spawns(&cache);
        cache.request(strip(0, 72));
        next(&rx);
        assert!(spawns(&cache) > before);

        // The waveform.
        cache.request(PreviewRequest::Peaks { file: AUDIO.into() });
        let PreviewData::Peaks { levels: good, .. } = next(&rx) else {
            panic!()
        };
        let peaks = paths.waveforms_dir().join(format!(
            "{}.peaks",
            peaks_key(AUDIO, &file_fact(&paths, AUDIO).unwrap())
        ));
        let whole = fs::read(&peaks).unwrap();
        for garbage in [
            &whole[..whole.len() / 2],
            b"QRECPK1\n\x05".as_slice(),
            b"nothing",
        ] {
            fs::write(&peaks, garbage).unwrap();
            let before = spawns(&cache);
            cache.request(PreviewRequest::Peaks { file: AUDIO.into() });
            let PreviewData::Peaks { levels, .. } = next(&rx) else {
                panic!()
            };
            assert_eq!(levels, good);
            assert_eq!(spawns(&cache), before + 1);
            assert_eq!(fs::read(&peaks).unwrap(), whole);
        }
        // A changed file is another key: made again.
        let before = spawns(&cache);
        let file = fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join(AUDIO))
            .unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        cache.request(PreviewRequest::Peaks { file: AUDIO.into() });
        next(&rx);
        assert_eq!(spawns(&cache), before + 1);
    }

    /// The acceptance check: no ffmpeg (or ffprobe) → `Failed`, nothing
    /// hangs; a missing file fails without a child. `CachedSilence` never
    /// needs ffmpeg.
    #[test]
    fn without_ffmpeg_every_preview_fails() {
        let mono = fixture(&MediaSpec::audio(48_000, 1, 500, &[]));
        let video = fixture(&MediaSpec::video(64, 36, 25, 25));
        let dir = project(&[(VIDEO, &video), (AUDIO, &mono)]);
        let options = Options {
            ffmpeg: "qrec-no-such-ffmpeg".into(),
            ffprobe: "qrec-no-such-ffprobe".into(),
            ..quick()
        };
        let (cache, rx) = open(dir.path(), options);
        let requests = [
            (strip(0, 72), WHAT_STRIP),
            (
                PreviewRequest::Frames {
                    file: VIDEO.into(),
                    first: 0,
                    count: 4,
                    height: 72,
                },
                WHAT_FRAMES,
            ),
            (PreviewRequest::Peaks { file: AUDIO.into() }, WHAT_PEAKS),
        ];
        for (req, expected) in requests {
            cache.request(req.clone());
            match next(&rx) {
                PreviewData::Failed {
                    file,
                    what,
                    message,
                } => {
                    assert_eq!((file.as_str(), what), (req.file(), expected));
                    assert!(message.contains("wasn't found"), "{message}");
                }
                other => panic!("{req:?}: {other:?}"),
            }
        }
        cache.request(PreviewRequest::Peaks {
            file: "takes/take-0009/A1-mic.mka".into(),
        });
        match next(&rx) {
            PreviewData::Failed { what, message, .. } => {
                assert_eq!(what, WHAT_PEAKS);
                assert!(
                    message.contains("can't read takes/take-0009/A1-mic.mka"),
                    "{message}"
                );
            }
            other => panic!("{other:?}"),
        }
        cache.request(PreviewRequest::Peaks {
            file: "../outside.mka".into(),
        });
        assert!(matches!(next(&rx), PreviewData::Failed { .. }));
        cache.request(strip(0, 0));
        assert!(matches!(next(&rx), PreviewData::Failed { .. }), "no height");
        cache.request(PreviewRequest::CachedSilence {
            file: AUDIO.into(),
            threshold_db: -35.0,
            min_silence_ms: 400,
        });
        assert!(matches!(
            next(&rx),
            PreviewData::Silence {
                intervals: None,
                ..
            }
        ));
        assert_eq!(spawns(&cache), 3, "only the runs that couldn't start");
    }

    /// `CachedSilence` reads the analysis an autocut cached for exactly
    /// these parameters and this file version (the CLI's own dry run:
    /// `crates/qrec/tests/preview.rs`).
    #[test]
    fn cached_silence_reads_the_analysis_cache() {
        let mono = fixture(&MediaSpec::audio(48_000, 1, 1_000, &[(100, 900)]));
        let dir = project(&[(AUDIO, &mono)]);
        let paths = ProjectPaths::new(dir.path());
        let fact = file_fact(&paths, AUDIO).unwrap();
        let key = SilenceKey::compute(AUDIO, fact.bytes, fact.mtime_ns, -35.0, 400);
        let silences = vec![
            Interval {
                start_ns: 0,
                end_ns: 100_000_000,
            },
            Interval {
                start_ns: 900_000_000,
                end_ns: 1_000_000_000,
            },
        ];
        let cache_file = super::super::silence::CacheFile {
            version: 1,
            key: key.0.clone(),
            silences: silences.clone(),
        };
        fs::create_dir_all(paths.silence_cache_dir()).unwrap();
        fs::write(
            paths.silence_cache_dir().join(format!("{}.json", key.0)),
            serde_json::to_vec(&cache_file).unwrap(),
        )
        .unwrap();
        let (cache, rx) = open(dir.path(), quick());
        let ask = |threshold_db: f64, min_silence_ms: u32| {
            cache.request(PreviewRequest::CachedSilence {
                file: AUDIO.into(),
                threshold_db,
                min_silence_ms,
            });
            match next(&rx) {
                PreviewData::Silence {
                    file,
                    key,
                    intervals,
                } => {
                    assert_eq!(file, AUDIO);
                    (key, intervals)
                }
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(ask(-35.0, 400), (key.clone(), Some(silences)));
        assert_eq!(ask(-35.004, 400).0, key, "keyed to 2 decimals");
        let (other, none) = ask(-40.0, 400);
        assert_ne!(other, key);
        assert_eq!(none, None, "never analyzed at −40 dB");
        assert_eq!(ask(-35.0, 300).1, None);
        assert_eq!(spawns(&cache), 0);
    }

    /// The acceptance checks: every run appends to one `preview.log`, also
    /// across caches (pids and kinds first); a failed run gets its own log
    /// with its last lines; the log rolls over past its limit.
    #[test]
    fn one_rolling_log_and_failed_runs_get_their_own() {
        let fps = Fps::new(30, 1);
        let dir = project(&[(VIDEO, &id_video(fps, 95 * 30))]);
        let logs = ProjectPaths::new(dir.path()).logs_dir();
        for chunk in 0..2 {
            let (cache, rx) = open(dir.path(), quick());
            cache.request(strip(chunk, 72));
            assert!(matches!(next(&rx), PreviewData::Strip { .. }));
        }
        let text = fs::read_to_string(logs.join(PREVIEW_LOG)).unwrap();
        let pids: BTreeSet<&str> = text
            .lines()
            .filter(|l| l.contains(" strip] $ ffmpeg "))
            .filter_map(|l| l.strip_prefix('[')?.split(' ').next())
            .collect();
        assert_eq!(pids.len(), 2, "two runs, one log:\n{text}");
        assert!(
            text.lines().any(|l| l.contains(" probe] $ ffprobe ")),
            "{text}"
        );
        assert!(
            text.lines().all(|l| l.starts_with('[') && l.contains("] ")),
            "every line has its pid and kind:\n{text}"
        );
        let own_logs = || -> Vec<PathBuf> {
            fs::read_dir(&logs)
                .unwrap()
                .map(|e| e.unwrap())
                .filter(|e| e.file_name().to_string_lossy().contains("-preview-"))
                .map(|e| e.path())
                .collect()
        };
        assert!(
            own_logs().is_empty(),
            "runs that worked have no log of their own"
        );

        // A failed run: no audio to read.
        let (cache, rx) = open(dir.path(), quick());
        cache.request(PreviewRequest::Peaks { file: VIDEO.into() });
        let PreviewData::Failed { message, .. } = next(&rx) else {
            panic!()
        };
        let own = own_logs();
        assert_eq!(own.len(), 1, "{own:?}");
        let name = own[0].file_name().unwrap().to_string_lossy().into_owned();
        assert!(logs::is_stamped_log_name(&name), "{name}");
        assert!(
            name.ends_with(&format!("-{}-preview-peaks.log", std::process::id())),
            "{name}"
        );
        assert!(
            message.contains(&format!("(log: {})", own[0].display())),
            "{message}"
        );
        let failed = fs::read_to_string(&own[0]).unwrap();
        assert!(failed.starts_with("# qrec "), "{failed}");
        assert!(failed.contains("matches no streams"), "{failed}");
        assert!(
            failed.contains("# qrec: ffmpeg exited with status"),
            "{failed}"
        );
        assert!(failed.lines().count() <= 2 + TAIL_LINES, "{failed}");
        let rolled = fs::read_to_string(logs.join(PREVIEW_LOG)).unwrap();
        assert!(rolled.contains(" peaks] "), "{rolled}");
        assert!(rolled.starts_with(&text), "appended, never truncated");

        // Past its limit the log moves to preview.1.log.
        let (cache, rx) = open(
            dir.path(),
            Options {
                log_max: 2048,
                ..quick()
            },
        );
        for chunk in 2..5 {
            cache.request(strip(chunk, 36));
            next(&rx);
        }
        let old = fs::metadata(logs.join(PREVIEW_LOG_OLD)).unwrap().len();
        assert!(old > 2048, "{old}");
        assert!(fs::metadata(logs.join(PREVIEW_LOG)).map_or(0, |m| m.len()) <= 2048 + 4096);
    }

    /// Shutdown kills a running child at once (SIGTERM, then SIGKILL) and
    /// answers nothing more; a run past its timeout fails with its own log.
    #[test]
    fn shutdown_kills_and_timeouts_fail() {
        let fps = Fps::new(30, 1);
        let dir = project(&[(VIDEO, &id_video(fps, 95 * 30))]);
        let bin = tempfile::tempdir().unwrap();
        let hang: OsString = script(bin.path(), "hang", "trap '' TERM\nsleep 30").into();
        let (cache, rx) = open(
            dir.path(),
            Options {
                ffmpeg: hang.clone(),
                ..quick()
            },
        );
        cache.request(strip(0, 72));
        cache.request(strip(1, 72));
        let started = Instant::now();
        while cache.stats().running.load(Ordering::SeqCst) == 0 {
            assert!(started.elapsed() < WAIT, "the child never started");
            thread::sleep(Duration::from_millis(10));
        }
        cache.shutdown();
        let stopped = Instant::now();
        while cache.stats().running.load(Ordering::SeqCst) > 0 {
            assert!(stopped.elapsed() < Duration::from_secs(5), "still running");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
        cache.request(strip(2, 72));
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "stopped"
        );
        assert_eq!(cache.in_flight(), 0);

        let (cache, rx) = open(
            dir.path(),
            Options {
                ffmpeg: hang,
                timeout: Duration::from_secs(2),
                per_gib: Duration::ZERO,
                ..quick()
            },
        );
        cache.request(strip(0, 72));
        match next(&rx) {
            PreviewData::Failed { what, message, .. } => {
                assert_eq!(what, WHAT_STRIP);
                assert!(message.contains("ffmpeg timed out after 2 s"), "{message}");
                assert!(message.contains("-preview-strip.log)"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The acceptance check: the disk caps trim oldest first (a key's files
    /// go together; a hit makes an entry young again); stale temp files go,
    /// fresh ones stay.
    #[test]
    fn the_trim_removes_the_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let hours = |h: u64| now - Duration::from_secs(3600 * h);
        let put = |name: &str, bytes: usize, at: SystemTime| {
            let path = dir.path().join(name);
            fs::write(&path, vec![0u8; bytes]).unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        put("aaaa.rgba", 900, hours(5));
        put("aaaa.json", 100, hours(5));
        put("bbbb.rgba", 1000, hours(4));
        put("cccc.peaks", 1000, hours(3));
        put("dddd.rgba", 900, hours(1));
        put("dddd.json", 100, now - Duration::from_secs(10));
        put(".eeee.qrec-part-1-2.rgba", 5000, hours(2));
        put(".ffff.qrec-part-1-3.rgba", 5000, now);
        let trimmed = trim_dir(dir.path(), 2500, now).unwrap();
        let mut left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                ".ffff.qrec-part-1-3.rgba",
                "cccc.peaks",
                "dddd.json",
                "dddd.rgba"
            ]
        );
        assert_eq!(
            trimmed,
            Trimmed {
                removed: 4,
                freed: 1000 + 1000 + 5000,
                left: 2000
            }
        );
        // Under the cap: nothing goes.
        assert_eq!(trim_dir(dir.path(), 2000, now).unwrap().removed, 0);
        // A hit makes an entry young: the other one goes first.
        touch(&dir.path().join("cccc.peaks"));
        let trimmed = trim_dir(dir.path(), 1500, SystemTime::now()).unwrap();
        assert_eq!(trimmed.removed, 2, "{trimmed:?}");
        assert!(dir.path().join("cccc.peaks").exists());
        assert!(!dir.path().join("dddd.rgba").exists());
        assert_eq!(
            trim_dir(&dir.path().join("missing"), 0, now).unwrap(),
            Trimmed::default()
        );
        let (thumbs, waves) = trim_caches(dir.path());
        assert_eq!((thumbs.unwrap(), waves.unwrap()), Default::default());
    }

    /// A cache starts by trimming its project's caps (on its own thread).
    #[test]
    fn a_new_cache_trims_the_caps() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::new(dir.path());
        fs::create_dir_all(paths.waveforms_dir()).unwrap();
        for (i, name) in ["old", "new"].iter().enumerate() {
            let path = paths.waveforms_dir().join(format!("{name}.peaks"));
            fs::write(&path, vec![0u8; 1000]).unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(100 - i as u64))
                .unwrap();
        }
        let (_cache, _rx) = open(
            dir.path(),
            Options {
                trim_on_start: true,
                waveforms_cap: 1500,
                ..quick()
            },
        );
        let started = Instant::now();
        while paths.waveforms_dir().join("old.peaks").exists() {
            assert!(started.elapsed() < Duration::from_secs(10), "never trimmed");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(paths.waveforms_dir().join("new.peaks").exists());
    }
}
