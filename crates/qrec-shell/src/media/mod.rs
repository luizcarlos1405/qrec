//! The `MediaRunner` implementation (A§8.1, A§8.7, A§8.8): ffprobe probes,
//! silence detection with its cache, segment rendering with verification
//! and the segment cache, the stitch, frame saves, never-overwrite outputs
//! (`renameat2(RENAME_NOREPLACE)`) and take-directory deletion (purge).
//! Owner: media.
//!
//! Every `MediaRunner` call returns at once: the work runs on a named worker
//! thread and its result comes back as an `Event` through the `Inbox`. Every
//! effect gets exactly one terminal event, even when a worker panics. The
//! argv come from the core (A§9.2) and run with the project folder as their
//! working directory (their paths are project-relative); the runner only
//! replaces the last element (the output path) with a temp path: for an
//! export or a frame, `/proc/self/fd/<n>`, the descriptor of the temp file
//! it created in the destination folder it holds open (A§8.8, `place`).
//!
//! [`FfmpegRunner::new`] builds the runner for a project; hosts also call
//! [`housekeeping`] once at start (prune logs, sweep stale temp files).
//!
//! Children run through `child`, an A§8.7 runner written here while
//! `crate::proc` (owner: session) was still a stub; `sources` and `doctor`
//! use it too.

mod child;
mod export;
mod frame;
mod jobs;
mod logs;
mod place;
mod probe;
mod purge;
mod silence;
#[cfg(test)]
mod tests;
mod verify;

use std::fs;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use qrec_core::plans::{ExportPlan, FramePlan};
use qrec_core::protocol::{ProbeError, ProbeJob, ProbePurpose, ProbeResult};
use qrec_core::state::SilenceKey;
use qrec_core::{Event, JobId, TakeId};

use crate::deps::MediaRunner;
use crate::msg::Inbox;
use crate::paths::ProjectPaths;

pub use jobs::remove_temps_in_flight;
pub use logs::{KEEP_LOGS, prune_logs};
/// Never-overwrite renames, also for migration backups (A§8.8), and what a
/// temp output's name looks like (the host-start sweep, A§8.7).
pub use place::{PART_MARKER, is_temp_name, rename_noreplace};
/// What a purge deletes and leaves, for its confirmation (A§5.6).
pub use purge::{TakeContents, take_contents};

pub(crate) use child::{
    Spec as ChildSpec, Stderr as ChildStderr, Stdout as ChildStdout, run as run_child,
};

/// Timeouts and limits of the media runner (A§8.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaOptions {
    /// Segments rendered at once: `max(1, cores / 2)` (SPEC §13.1).
    pub max_parallel: usize,
    /// ffprobe: 20 s, plus [`MediaOptions::per_gib`] per GiB of input
    /// (`-count_packets` reads the whole file).
    pub probe_timeout: Duration,
    /// silencedetect: a base, plus `per_gib` per GiB of input.
    pub silence_timeout: Duration,
    /// Added per GiB of input to probe and silence timeouts.
    pub per_gib: Duration,
    /// ffmpeg jobs with `-progress`: no advancing block for this long ⇒
    /// "ffmpeg stalled" (A§8.7: 60 s).
    pub stall_timeout: Duration,
    /// A frame save's wall-clock limit.
    pub frame_timeout: Duration,
    /// SIGTERM → SIGKILL → giving up on `wait()` (A§8.7: 2 s).
    pub kill_grace: Duration,
}

impl Default for MediaOptions {
    fn default() -> Self {
        Self {
            max_parallel: default_parallelism(),
            probe_timeout: Duration::from_secs(20),
            silence_timeout: Duration::from_secs(60),
            per_gib: Duration::from_secs(30),
            stall_timeout: Duration::from_secs(60),
            frame_timeout: Duration::from_secs(300),
            kill_grace: child::KILL_GRACE,
        }
    }
}

impl MediaOptions {
    /// `base` plus `per_gib` in proportion to `bytes` (half a GiB adds
    /// half of it), so small files get exactly the A§8.7 timeout.
    pub(crate) fn scaled(&self, base: Duration, bytes: u64) -> Duration {
        let extra_ns = self.per_gib.as_nanos().saturating_mul(u128::from(bytes)) >> 30;
        let extra = Duration::from_nanos(u64::try_from(extra_ns).unwrap_or(u64::MAX));
        base.saturating_add(extra)
    }
}

/// `max(1, available cores / 2)`.
pub fn default_parallelism() -> usize {
    let cores = thread::available_parallelism().map_or(1, std::num::NonZero::get);
    (cores / 2).max(1)
}

/// What every worker of one runner shares.
#[derive(Debug)]
pub(crate) struct Ctx {
    pub project: ProjectPaths,
    pub options: MediaOptions,
    pub jobs: Arc<jobs::Registry>,
}

impl Ctx {
    pub(crate) fn root(&self) -> &Path {
        self.project.root()
    }

    /// `.qrec/logs/<stamp>-<pid>-j<job>-<step>` (without `.log`).
    pub(crate) fn job_log_base(&self, job: JobId, step: &str) -> String {
        format!("{}-{}-j{}-{step}", logs::stamp(), std::process::id(), job.0)
    }

    /// `.qrec/logs/<stamp>-<pid>-<step>` (without `.log`), for work without a job.
    pub(crate) fn log_base(&self, step: &str) -> String {
        format!("{}-{}-{step}", logs::stamp(), std::process::id())
    }

    pub(crate) fn open_log(&self, base: &str, argv: &[String]) -> Result<logs::LogFile, String> {
        logs::LogFile::create(&self.project.logs_dir(), base, argv, self.root()).map_err(|e| {
            format!(
                "can't create a log in {}: {e}",
                crate::paths::path_text(&self.project.logs_dir())
            )
        })
    }
}

/// The real media runner: ffmpeg and ffprobe child processes (A§8.7).
#[derive(Debug, Clone)]
pub struct FfmpegRunner {
    ctx: Arc<Ctx>,
}

impl FfmpegRunner {
    /// A runner for the project in `project_dir` with the default options.
    pub fn new(project_dir: impl Into<PathBuf>) -> Self {
        Self::with_options(project_dir, MediaOptions::default())
    }

    pub fn with_options(project_dir: impl Into<PathBuf>, options: MediaOptions) -> Self {
        Self {
            ctx: Arc::new(Ctx {
                project: ProjectPaths::new(project_dir),
                options,
                jobs: Arc::new(jobs::Registry::default()),
            }),
        }
    }

    pub fn project_dir(&self) -> &Path {
        self.ctx.root()
    }

    /// Whether a job still has work (or temp files) in flight here.
    pub fn is_running(&self, job: JobId) -> bool {
        self.ctx.jobs.is_running(job)
    }

    /// Runs `body` on a named worker thread. If the thread can't start or
    /// `body` panics, `fail(message)` is posted instead, so the core always
    /// gets a terminal event.
    fn spawn(
        &self,
        name: String,
        out: Inbox,
        fail: impl Fn(String) -> Event + Send + Sync + 'static,
        body: impl FnOnce(&Arc<Ctx>, &Inbox) + Send + 'static,
    ) {
        let fail = Arc::new(fail);
        let ctx = Arc::clone(&self.ctx);
        let (worker_out, worker_fail) = (out.clone(), Arc::clone(&fail));
        let spawned = thread::Builder::new().name(name.clone()).spawn(move || {
            if let Err(panic) = catch_unwind(AssertUnwindSafe(|| body(&ctx, &worker_out))) {
                let message = format!(
                    "internal error in {name}: {}",
                    child::panic_message(panic.as_ref())
                );
                tracing::error!("{message}");
                worker_out.event(worker_fail(message));
            }
        });
        if let Err(e) = spawned {
            out.event(fail(format!("can't start a worker thread: {e}")));
        }
    }
}

impl MediaRunner for FfmpegRunner {
    fn probe(&mut self, take: TakeId, purpose: ProbePurpose, jobs: Vec<ProbeJob>, out: Inbox) {
        let ids: Vec<(String, String)> = jobs
            .iter()
            .map(|j| (j.id.clone(), j.file.clone()))
            .collect();
        let take_for_fail = take.clone();
        self.spawn(
            format!("qrec-probe-{take}"),
            out,
            move |message| Event::TakeProbed {
                take: take_for_fail.clone(),
                purpose,
                results: ids
                    .iter()
                    .map(|(id, file)| ProbeResult {
                        id: id.clone(),
                        file: file.clone(),
                        // The worker failed, not the file.
                        info: Err(ProbeError::tool(message.clone())),
                    })
                    .collect(),
            },
            move |ctx, out| probe::run(ctx, take, purpose, jobs, out),
        );
    }

    fn detect_silence(&mut self, job: JobId, key: SilenceKey, argv: Vec<String>, out: Inbox) {
        let guard = self.ctx.jobs.enter(job);
        let fail_key = key.clone();
        self.spawn(
            format!("qrec-silence-j{}", job.0),
            out,
            move |message| Event::SilenceFailed {
                job,
                key: fail_key.clone(),
                message,
            },
            move |ctx, out| {
                let event = silence::run(ctx, &guard.ctl, job, key, &argv);
                drop(guard);
                out.event(event);
            },
        );
    }

    fn render_export(&mut self, job: JobId, plan: ExportPlan, out: Inbox) {
        let guard = self.ctx.jobs.enter(job);
        let ctl = Arc::clone(&guard.ctl);
        self.spawn(
            format!("qrec-export-j{}", job.0),
            out,
            move |message| {
                ctl.remove_temps();
                Event::ExportFailed {
                    job,
                    message,
                    stderr_tail: Vec::new(),
                    log: String::new(),
                }
            },
            move |ctx, out| {
                let event = export::run(ctx, &guard.ctl, job, &plan, out);
                drop(guard);
                out.event(event);
            },
        );
    }

    fn save_frame(&mut self, job: JobId, plan: FramePlan, out: Inbox) {
        let guard = self.ctx.jobs.enter(job);
        let ctl = Arc::clone(&guard.ctl);
        self.spawn(
            format!("qrec-frame-j{}", job.0),
            out,
            move |message| {
                ctl.remove_temps();
                Event::FrameFailed {
                    job,
                    message,
                    log: String::new(),
                }
            },
            move |ctx, out| {
                let event = frame::run(ctx, &guard.ctl, job, &plan);
                drop(guard);
                out.event(event);
            },
        );
    }

    fn delete_take_dirs(&mut self, job: JobId, takes: Vec<TakeId>, out: Inbox) {
        let names: Vec<String> = takes.iter().map(|t| format!("takes/{t}/")).collect();
        self.spawn(
            format!("qrec-purge-j{}", job.0),
            out,
            move |message| Event::TakeDirsDeleted {
                job,
                deleted: Vec::new(),
                errors: names.iter().map(|n| format!("{n}: {message}")).collect(),
            },
            move |ctx, out| {
                let (deleted, errors) = purge::delete_take_dirs(ctx.root(), &takes);
                out.event(Event::TakeDirsDeleted {
                    job,
                    deleted,
                    errors,
                });
            },
        );
    }

    fn cancel(&mut self, job: JobId) {
        if !self.ctx.jobs.cancel(job) {
            tracing::debug!("cancel {job}: not running here");
        }
    }
}

/// What [`housekeeping`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Housekeeping {
    pub logs_pruned: usize,
    pub temps_removed: Vec<PathBuf>,
}

/// Host start (A§8.7): keep the newest [`KEEP_LOGS`] logs and delete temp
/// files ([`is_temp_name`]: `.<stem>.qrec-part-<pid>-<job>[.<ext>]`)
/// older than an hour in `exports/`, `frames/`, `.qrec/cache/segments/` and
/// `.qrec/cache/silence/`. Only the lock holder may call this (a running
/// job's temps are younger than an hour anyway).
pub fn housekeeping(project_dir: &Path) -> Housekeeping {
    let project = ProjectPaths::new(project_dir);
    let logs_pruned = prune_logs(&project.logs_dir(), KEEP_LOGS).unwrap_or_else(|e| {
        tracing::warn!("can't prune {}: {e}", project.logs_dir().display());
        0
    });
    let mut temps_removed = Vec::new();
    for dir in [
        project.exports_dir(),
        project.frames_dir(),
        project.segments_dir(),
        project.silence_cache_dir(),
    ] {
        match sweep_temps(&dir, Duration::from_secs(3600), SystemTime::now()) {
            Ok(removed) => temps_removed.extend(removed),
            Err(e) => tracing::warn!("can't sweep {}: {e}", dir.display()),
        }
    }
    Housekeeping {
        logs_pruned,
        temps_removed,
    }
}

/// Deletes regular files in `dir` that have a temp file's name
/// ([`is_temp_name`]) and whose mtime is older than `age` before `now`.
fn sweep_temps(dir: &Path, age: Duration, now: SystemTime) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        if !is_temp_name(&entry.file_name()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|d| d >= age);
        if meta.is_file() && old && fs::remove_file(entry.path()).is_ok() {
            removed.push(entry.path());
        }
    }
    Ok(removed)
}
