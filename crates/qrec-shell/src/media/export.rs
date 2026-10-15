//! `Effect::RenderExport` → `ExportProgress`… then `ExportFinished` |
//! `ExportFailed` | `ExportCancelled` | `OutputAppeared` (A§9.3, SPEC
//! §13.1).
//!
//! 1. Segments whose cache file (`.qrec/cache/segments/<key>.<ext>`) exists
//!    are skipped; identical keys are rendered once.
//! 2. The others render in parallel, at most `max_parallel` ffmpeg at once,
//!    each to a temp file next to its cache file, with `-progress` feeding
//!    `ExportProgress` and the stall watchdog. A rendered segment must have
//!    exactly the unit's video packets and audio samples before it is
//!    fsynced and renamed into the cache (never replacing a file).
//! 3. The stitch writes the concat list, joins the segments into a temp file
//!    in the destination folder (`ExportStitchProgress`) and the result is
//!    renamed into place without replacing anything but a confirmed
//!    hand-picked path (`plan.replace`). A hand-picked path that appeared
//!    meanwhile leaves the result where it is: `OutputAppeared` (A§8.8).
//!
//! A failure stops the other segments; a cancel stops everything. Either way
//! the job's temp files are deleted (finished segments stay cached). A
//! failed segment of a VA-API plan re-runs the whole export once with
//! `export_plan::without_vaapi` (libx264, other keys: nothing mixes).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use qrec_core::export_plan::without_vaapi;
use qrec_core::parse::progress::ProgressBlock;
use qrec_core::plans::{ExportPlan, SegmentPlan};
use qrec_core::state::{Capabilities, FfmpegCaps};
use qrec_core::{Event, JobId};

use super::Ctx;
use super::child::{self, Ended, Finished, Spec, Stderr, Stdout};
use super::jobs::JobCtl;
use super::logs::{LogFile, TAIL_LINES};
use super::place::{self, Destination, PlaceError, rename_noreplace, temp_path};
use super::verify;
use crate::msg::Inbox;

/// How a job (or one of its steps) didn't finish.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The person cancelled it.
    Cancelled,
    /// Stopped because another segment failed (never reported itself).
    Aborted,
    Failed {
        message: String,
        stderr_tail: Vec<String>,
        log: String,
    },
    /// A segment of a VA-API plan failed (a `Failed`): the export can be
    /// re-planned with libx264.
    VaSegment(Box<Failure>),
    /// The hand-picked `out` appeared while the job ran and may not be
    /// replaced: the result was kept at `kept` (A§8.8).
    Appeared { out: PathBuf, kept: PathBuf },
}

impl Failure {
    /// A failure without ffmpeg output, pointing at `log`.
    pub(crate) fn plain(message: impl Into<String>, log: &LogFile) -> Self {
        Failure::Failed {
            message: message.into(),
            stderr_tail: Vec::new(),
            log: log.display(),
        }
    }

    /// A failed step: its message, the tail of its log and the log path.
    pub(crate) fn step(message: impl Into<String>, log: &LogFile) -> Self {
        Failure::Failed {
            message: message.into(),
            stderr_tail: log.tail(TAIL_LINES),
            log: log.display(),
        }
    }
}

/// Runs the export and returns its terminal event.
pub(crate) fn run(
    ctx: &Arc<Ctx>,
    ctl: &JobCtl,
    job: JobId,
    plan: &ExportPlan,
    out: &Inbox,
) -> Event {
    let result = match export(ctx, ctl, job, plan, out) {
        Err(Failure::VaSegment(va)) => retry_without_vaapi(ctx, ctl, job, plan, out, *va),
        other => other,
    };
    match result {
        Ok((path, bytes)) => Event::ExportFinished {
            job,
            path: path.to_string_lossy().into_owned(),
            bytes,
        },
        Err(failure) => {
            ctl.remove_temps();
            match failure {
                Failure::Cancelled | Failure::Aborted => Event::ExportCancelled { job },
                Failure::Failed {
                    message,
                    stderr_tail,
                    log,
                } => Event::ExportFailed {
                    job,
                    message,
                    stderr_tail,
                    log,
                },
                Failure::VaSegment(_) => Event::ExportFailed {
                    job,
                    message: "internal error: an unhandled VA-API failure".to_owned(),
                    stderr_tail: Vec::new(),
                    log: String::new(),
                },
                Failure::Appeared { out, kept } => appeared(job, &out, &kept),
            }
        }
    }
}

/// `OutputAppeared` for a hand-picked output that appeared while the job
/// ran; the core turns it into `confirmation_required` naming both paths.
pub(crate) fn appeared(job: JobId, out: &Path, kept: &Path) -> Event {
    Event::OutputAppeared {
        job,
        out: out.to_string_lossy().into_owned(),
        bytes: fs::symlink_metadata(out).map(|m| m.len()).unwrap_or(0),
        kept: kept.to_string_lossy().into_owned(),
    }
}

/// A§9.3 "one encoder per export": after a VA segment failed, the whole
/// export runs again with `without_vaapi`'s libx264 plan. Without one (no
/// ffmpeg identity, not a VA plan) the VA failure stands.
fn retry_without_vaapi(
    ctx: &Arc<Ctx>,
    ctl: &JobCtl,
    job: JobId,
    plan: &ExportPlan,
    out: &Inbox,
    va: Failure,
) -> Result<(PathBuf, u64), Failure> {
    let va_message = match &va {
        Failure::Failed { message, .. } => message.clone(),
        _ => return Err(va),
    };
    let fallback =
        ffmpeg_identity().and_then(|identity| without_vaapi(plan, &identity_caps(identity)));
    let Some(x264) = fallback else {
        return Err(va);
    };
    if ctl.is_cancelled() {
        return Err(Failure::Cancelled);
    }
    ctl.remove_temps();
    ctl.resume();
    tracing::warn!("{job}: h264_vaapi failed ({va_message}); exporting with libx264");
    match export(ctx, ctl, job, &x264, out) {
        Err(Failure::Failed {
            message,
            stderr_tail,
            log,
        }) => Err(Failure::Failed {
            message: format!("{message} (libx264, after h264_vaapi failed: {va_message})"),
            stderr_tail,
            log,
        }),
        Err(Failure::VaSegment(inner)) => Err(*inner),
        other => other,
    }
}

/// The ffmpeg that runs the plans, resolved like `exec` does and
/// canonicalized: the identity in export cache keys (A§9.3).
fn ffmpeg_identity() -> Option<String> {
    let found = which::which("ffmpeg").ok()?;
    let path = fs::canonicalize(&found).unwrap_or(found);
    Some(path.to_string_lossy().into_owned())
}

/// What `without_vaapi` reads: the ffmpeg identity.
fn identity_caps(identity: String) -> Capabilities {
    Capabilities {
        ffmpeg: Some(FfmpegCaps {
            version: String::new(),
            identity,
            encoders: BTreeSet::new(),
        }),
        vaapi: None,
        cores: 0,
    }
}

/// Whether the plan uses the VA-API export chain.
fn is_vaapi(plan: &ExportPlan) -> bool {
    plan.segments
        .iter()
        .any(|s| s.argv.iter().any(|a| a == "-init_hw_device"))
}

/// One cache key to render: the first segment with it and every index
/// that shares it.
#[derive(Debug, Clone)]
struct Unit<'a> {
    first: usize,
    indices: Vec<usize>,
    segment: &'a SegmentPlan,
}

fn export(
    ctx: &Arc<Ctx>,
    ctl: &JobCtl,
    job: JobId,
    plan: &ExportPlan,
    out: &Inbox,
) -> Result<(PathBuf, u64), Failure> {
    let note_argv = vec![
        "export".to_owned(),
        plan.out.clone(),
        format!("{} segments", plan.segments.len()),
    ];
    let log = ctx
        .open_log(&ctx.job_log_base(job, "export"), &note_argv)
        .map_err(|message| Failure::Failed {
            message,
            stderr_tail: Vec::new(),
            log: String::new(),
        })?;
    if plan.segments.is_empty() {
        return Err(Failure::plain(
            "nothing to export: the plan has no segments",
            &log,
        ));
    }
    let dest = destination(ctx, &plan.out, plan.replace).map_err(|m| Failure::plain(m, &log))?;
    log.note(&format!("destination {:?}", dest));

    // Unique keys, cache hits first.
    let mut by_key: BTreeMap<&str, usize> = BTreeMap::new();
    let mut units: Vec<Unit<'_>> = Vec::new();
    for (i, segment) in plan.segments.iter().enumerate() {
        match by_key.get(segment.key.as_str()) {
            Some(&u) => units[u].indices.push(i),
            None => {
                by_key.insert(&segment.key, units.len());
                units.push(Unit {
                    first: i,
                    indices: vec![i],
                    segment,
                });
            }
        }
    }
    let mut pending = VecDeque::new();
    for unit in units {
        let path = resolve(ctx, &unit.segment.path).map_err(|m| Failure::plain(m, &log))?;
        if is_cached(&path) {
            log.note(&format!(
                "segment {} is cached: {}",
                unit.first, unit.segment.path
            ));
            for &i in &unit.indices {
                out.event(Event::ExportProgress {
                    job,
                    segment: i,
                    frames_done: plan.segments[i].frames,
                });
            }
        } else {
            pending.push_back(unit);
        }
    }
    if !pending.is_empty() {
        fs::create_dir_all(ctx.project.segments_dir()).map_err(|e| {
            Failure::plain(
                format!("can't create {}: {e}", ctx.project.segments_dir().display()),
                &log,
            )
        })?;
        render_all(ctx, ctl, job, plan.segments.len(), pending, out).map_err(|f| match f {
            f @ Failure::Failed { .. } if is_vaapi(plan) => Failure::VaSegment(Box::new(f)),
            other => other,
        })?;
    }
    if ctl.is_cancelled() {
        return Err(Failure::Cancelled);
    }

    let tmp = stitch(ctx, ctl, job, plan, &dest, out)?;
    if ctl.is_cancelled() {
        return Err(Failure::Cancelled);
    }
    match place::place(&tmp, &dest) {
        Ok(path) => {
            ctl.forget_temp(&tmp);
            let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            log.note(&format!("exported {} ({bytes} bytes)", path.display()));
            Ok((path, bytes))
        }
        Err(PlaceError::Appeared { out, kept }) => Err(kept_aside(ctl, &log, out, kept)),
        Err(e) => Err(Failure::plain(e.to_string(), &log)),
    }
}

/// The hand-picked output appeared meanwhile: the result stays at `kept`
/// (no longer a temp file of the job) and the job ends with `Appeared`.
pub(crate) fn kept_aside(ctl: &JobCtl, log: &LogFile, out: PathBuf, kept: PathBuf) -> Failure {
    ctl.forget_temp(&kept);
    log.note(&format!(
        "{} appeared meanwhile; the result was kept as {}",
        out.display(),
        kept.display()
    ));
    Failure::Appeared { out, kept }
}

/// Renders the pending units on at most `max_parallel` threads. The first
/// failure stops the others.
fn render_all(
    ctx: &Arc<Ctx>,
    ctl: &JobCtl,
    job: JobId,
    total: usize,
    pending: VecDeque<Unit<'_>>,
    out: &Inbox,
) -> Result<(), Failure> {
    let workers = ctx.options.max_parallel.max(1).min(pending.len());
    let queue = Mutex::new(pending);
    let failure: Mutex<Option<Failure>> = Mutex::new(None);
    let record = |f: Failure| {
        let mut slot = failure.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(f);
        }
        drop(slot);
        ctl.abort();
    };
    let work = || {
        loop {
            if ctl.stop.is_set() {
                break;
            }
            let next = queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            let Some(unit) = next else { break };
            let result = catch_unwind(AssertUnwindSafe(|| {
                render_unit(ctx, ctl, job, total, &unit, out)
            }))
            .unwrap_or_else(|panic| {
                Err(Failure::Failed {
                    message: format!(
                        "internal error while rendering segment {}: {}",
                        unit.first + 1,
                        child::panic_message(panic.as_ref())
                    ),
                    stderr_tail: Vec::new(),
                    log: String::new(),
                })
            });
            if let Err(f) = result {
                record(f);
                break;
            }
        }
    };
    thread::scope(|scope| {
        let mut started = 0;
        for w in 0..workers {
            let spawned = thread::Builder::new()
                .name(format!("qrec-export-j{}-w{w}", job.0))
                .spawn_scoped(scope, work);
            match spawned {
                Ok(_) => started += 1,
                Err(e) => tracing::warn!("can't start export worker {w}: {e}"),
            }
        }
        if started == 0 {
            work();
        }
    });
    if ctl.is_cancelled() {
        return Err(Failure::Cancelled);
    }
    match failure.into_inner().unwrap_or_else(PoisonError::into_inner) {
        None => Ok(()),
        Some(f) => Err(f),
    }
}

/// "segment 2/5 (take-0002, frames 120–300)".
fn label(index: usize, total: usize, segment: &SegmentPlan) -> String {
    format!(
        "segment {}/{total} ({}, frames {}–{})",
        index + 1,
        segment.take,
        segment.in_,
        segment.out
    )
}

fn render_unit(
    ctx: &Ctx,
    ctl: &JobCtl,
    job: JobId,
    total: usize,
    unit: &Unit<'_>,
    out: &Inbox,
) -> Result<(), Failure> {
    let segment = unit.segment;
    let label = label(unit.first, total, segment);
    let mut argv = segment.argv.clone();
    let log = ctx
        .open_log(&ctx.job_log_base(job, &format!("seg{}", unit.first)), &argv)
        .map_err(|message| Failure::Failed {
            message,
            stderr_tail: Vec::new(),
            log: String::new(),
        })?;
    if let Some(missing) = missing_input(ctx, &argv) {
        return Err(Failure::plain(
            format!("{label}: {missing} is missing"),
            &log,
        ));
    }
    let final_path = resolve(ctx, &segment.path).map_err(|m| Failure::plain(m, &log))?;
    let dir = final_path
        .parent()
        .ok_or_else(|| Failure::plain(format!("{label}: bad path {}", segment.path), &log))?;
    let ext = extension(&segment.path, "mkv");
    let tmp = temp_path(dir, &segment.key, job, &ext);
    ctl.add_temp(tmp.clone());
    replace_output(&mut argv, &tmp).map_err(|m| Failure::plain(m, &log))?;
    log.note(&format!("rendering to {}", tmp.display()));

    let (indices, frames, progress_out) = (unit.indices.clone(), segment.frames, out.clone());
    let on_block = move |block: &ProgressBlock| {
        if let Some(frame) = block.frame {
            for &segment in &indices {
                progress_out.event(Event::ExportProgress {
                    job,
                    segment,
                    frames_done: frame.min(frames),
                });
            }
        }
    };
    let mut spec = Spec::new(&argv, Stderr::Log(log.file()))
        .cwd(ctx.root())
        .stdout(Stdout::Progress(Box::new(on_block)))
        .stall(ctx.options.stall_timeout)
        .stop(&ctl.stop)
        .kill_grace(ctx.options.kill_grace);
    if let Some(driver) = vaapi_driver(&argv) {
        spec = spec.env("LIBVA_DRIVER_NAME", driver);
    }
    let done = child::run(spec).map_err(|m| Failure::plain(format!("{label}: {m}"), &log))?;
    log.note(&format!(
        "{} after {}",
        done.describe("ffmpeg"),
        child::human(done.elapsed)
    ));
    stopped(ctl, &done)?;
    check_ffmpeg(&done, &label, &log)?;

    let counts = verify::segment_counts(ctx, ctl, &tmp, &log).map_err(|m| {
        if ctl.stop.is_set() {
            stop_reason(ctl)
        } else {
            Failure::step(format!("{label}: {m}"), &log)
        }
    })?;
    counts
        .check(segment.frames, segment.samples)
        .map_err(|why| Failure::step(format!("{label}: {why}"), &log))?;
    fs::File::open(&tmp)
        .and_then(|f| f.sync_all())
        .map_err(|e| Failure::plain(format!("{label}: can't sync the segment: {e}"), &log))?;
    match rename_noreplace(&tmp, &final_path) {
        Ok(()) => {}
        // Same key, same content: someone cached it meanwhile.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp);
        }
        Err(e) => {
            return Err(Failure::plain(
                format!("{label}: can't move the segment into the cache: {e}"),
                &log,
            ));
        }
    }
    ctl.forget_temp(&tmp);
    log.note(&format!("cached {}", segment.path));
    for &i in &unit.indices {
        out.event(Event::ExportProgress {
            job,
            segment: i,
            frames_done: segment.frames,
        });
    }
    Ok(())
}

/// Joins the segments into a temp file next to the destination.
fn stitch(
    ctx: &Ctx,
    ctl: &JobCtl,
    job: JobId,
    plan: &ExportPlan,
    dest: &Destination,
    out: &Inbox,
) -> Result<PathBuf, Failure> {
    let mut argv = plan.stitch.argv.clone();
    let log = ctx
        .open_log(&ctx.job_log_base(job, "stitch"), &argv)
        .map_err(|message| Failure::Failed {
            message,
            stderr_tail: Vec::new(),
            log: String::new(),
        })?;
    let list = resolve(ctx, &plan.stitch.list_path).map_err(|m| Failure::plain(m, &log))?;
    write_list(ctl, job, &list, &plan.stitch.list_content)
        .map_err(|e| Failure::plain(format!("can't write the concat list: {e}"), &log))?;
    out.event(Event::ExportStitchProgress {
        job,
        out_time_ns: 0,
    });
    let dir = dest
        .path()
        .parent()
        .ok_or_else(|| Failure::plain(format!("bad output path {}", plan.out), &log))?;
    let ext = if plan.tmp_ext.is_empty() {
        extension(&plan.out, "mp4")
    } else {
        plan.tmp_ext.clone()
    };
    let tmp = temp_path(dir, &place::file_stem(dest.path()), job, &ext);
    ctl.add_temp(tmp.clone());
    replace_output(&mut argv, &tmp).map_err(|m| Failure::plain(m, &log))?;
    log.note(&format!("stitching to {}", tmp.display()));

    let progress_out = out.clone();
    let on_block = move |block: &ProgressBlock| {
        if let Some(ns) = block.out_time_ns {
            progress_out.event(Event::ExportStitchProgress {
                job,
                out_time_ns: ns.max(0) as u64,
            });
        }
    };
    let done = child::run(
        Spec::new(&argv, Stderr::Log(log.file()))
            .cwd(ctx.root())
            .stdout(Stdout::Progress(Box::new(on_block)))
            .stall(ctx.options.stall_timeout)
            .stop(&ctl.stop)
            .kill_grace(ctx.options.kill_grace),
    )
    .map_err(|m| Failure::plain(format!("stitch: {m}"), &log))?;
    let _ = fs::remove_file(&list);
    ctl.forget_temp(&list);
    stopped(ctl, &done)?;
    check_ffmpeg(&done, "stitch", &log)?;
    match fs::metadata(&tmp) {
        Ok(meta) if meta.len() > 0 => {}
        _ => return Err(Failure::step("stitch: ffmpeg wrote no output", &log)),
    }
    // "Your video is ready" must survive a crash right after it.
    fs::File::open(&tmp)
        .and_then(|f| f.sync_all())
        .map_err(|e| Failure::plain(format!("can't sync the export: {e}"), &log))?;
    Ok(tmp)
}

/// The concat list, written atomically (it lives in the segments dir).
fn write_list(ctl: &JobCtl, job: JobId, list: &Path, content: &str) -> io::Result<()> {
    let dir = list
        .parent()
        .ok_or_else(|| io::Error::other("the list has no folder"))?;
    fs::create_dir_all(dir)?;
    let stem = place::file_stem(list);
    let tmp = temp_path(dir, &stem, job, "ffconcat");
    ctl.add_temp(tmp.clone());
    let mut file = fs::File::create(&tmp)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    ctl.add_temp(list.to_path_buf());
    fs::rename(&tmp, list)?;
    ctl.forget_temp(&tmp);
    Ok(())
}

/// The output's destination: hand-picked paths are absolute (A§5.9), auto
/// names are project-relative (`exports/…`, `frames/…`). `replace` is the
/// plan's: whether the person confirmed replacing the file at a hand-picked
/// path (never guessed from what is there when the job starts).
pub(crate) fn destination(ctx: &Ctx, out: &str, replace: bool) -> Result<Destination, String> {
    let path = Path::new(out);
    if path.is_absolute() {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| format!("{out} is not a file path"))?;
        if !parent.is_dir() {
            return Err(format!("the folder {} doesn't exist", parent.display()));
        }
        Ok(Destination::Chosen {
            path: path.to_path_buf(),
            replace,
        })
    } else {
        let abs = ctx.project.resolve(out).map_err(|e| e.message)?;
        if let Some(dir) = abs.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        }
        Ok(Destination::Auto(abs))
    }
}

/// A project-relative path from a plan, joined with the project dir.
pub(crate) fn resolve(ctx: &Ctx, relative: &str) -> Result<PathBuf, String> {
    ctx.project.resolve(relative).map_err(|e| e.message)
}

/// A segment file that exists and isn't empty.
fn is_cached(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// The extension of a `/`-separated path, else `default`.
pub(crate) fn extension(path: &str, default: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// Replaces the argv's output (its last element) with `file:<tmp>`.
pub(crate) fn replace_output(argv: &mut [String], tmp: &Path) -> Result<(), String> {
    match argv {
        [_program, .., last] => {
            *last = format!("file:{}", tmp.to_string_lossy());
            Ok(())
        }
        _ => Err("internal error: the plan's command line has no output".to_owned()),
    }
}

/// The first `-i file:<path>` input that doesn't exist.
pub(crate) fn missing_input(ctx: &Ctx, argv: &[String]) -> Option<String> {
    argv.windows(2)
        .filter(|w| w[0] == "-i")
        .filter_map(|w| w[1].strip_prefix("file:"))
        .find(|path| {
            let p = Path::new(path);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                ctx.root().join(p)
            };
            !abs.exists()
        })
        .map(str::to_owned)
}

/// `Cancelled` or `Aborted` when the job's stop flag ended the child.
pub(crate) fn stopped(ctl: &JobCtl, done: &Finished) -> Result<(), Failure> {
    if ctl.is_cancelled() || (done.ended == Ended::Stopped && ctl.stop.is_set()) {
        Err(stop_reason(ctl))
    } else {
        Ok(())
    }
}

fn stop_reason(ctl: &JobCtl) -> Failure {
    if ctl.is_cancelled() {
        Failure::Cancelled
    } else {
        Failure::Aborted
    }
}

/// Success = exit 0 and `progress=end` (A§8.7).
pub(crate) fn check_ffmpeg(done: &Finished, what: &str, log: &LogFile) -> Result<(), Failure> {
    if let Some(e) = &done.parser_error {
        return Err(Failure::step(format!("{what}: internal error: {e}"), log));
    }
    if !done.exited_ok() {
        return Err(Failure::step(
            format!("{what}: {}", done.describe("ffmpeg")),
            log,
        ));
    }
    if !done.progress_end {
        return Err(Failure::step(
            format!("{what}: ffmpeg exited without finishing (no progress=end)"),
            log,
        ));
    }
    Ok(())
}

/// For VA plans (`-init_hw_device vaapi=va:<node>`), the `LIBVA_DRIVER_NAME`
/// of the node's vendor (A§8.10): Intel `iHD`, AMD `radeonsi`.
pub(crate) fn vaapi_driver(argv: &[String]) -> Option<&'static str> {
    let device = argv
        .windows(2)
        .find(|w| w[0] == "-init_hw_device")
        .and_then(|w| w[1].strip_prefix("vaapi=va:"))?;
    let node = Path::new(device).file_name()?.to_str()?;
    let vendor = fs::read_to_string(format!("/sys/class/drm/{node}/device/vendor")).ok()?;
    match vendor.trim() {
        "0x8086" => Some("iHD"),
        "0x1002" => Some("radeonsi"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_replacement_and_inputs() {
        let mut argv: Vec<String> = [
            "ffmpeg",
            "-i",
            "file:a.mkv",
            "-f",
            "mov",
            "file:.qrec/x.mov",
        ]
        .map(String::from)
        .to_vec();
        replace_output(&mut argv, Path::new("/p/.qrec/.x.qrec-part-1-2.mov")).unwrap();
        assert_eq!(argv.last().unwrap(), "file:/p/.qrec/.x.qrec-part-1-2.mov");
        assert!(replace_output(&mut [], Path::new("/x")).is_err());
        assert!(replace_output(&mut ["ffmpeg".to_owned()], Path::new("/x")).is_err());
        assert_eq!(extension(".qrec/cache/segments/k.mov", "mkv"), "mov");
        assert_eq!(extension("noext", "mkv"), "mkv");
    }

    #[test]
    fn vaapi_driver_needs_a_known_node() {
        let argv: Vec<String> = ["ffmpeg", "-init_hw_device", "vaapi=va:/dev/dri/nope"]
            .map(String::from)
            .to_vec();
        assert_eq!(vaapi_driver(&argv), None);
        assert_eq!(vaapi_driver(&["ffmpeg".to_owned()]), None);
    }
}
