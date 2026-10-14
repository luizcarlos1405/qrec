//! `Effect::Probe` → `Event::TakeProbed` (A§5.3 finalize and recover): each
//! `ProbeJob` runs its core-built ffprobe argv (`-count_packets`) on its own
//! thread, with a timeout, and `parse::ffprobe` reads the JSON. Results keep
//! the jobs' order; a missing, empty or unreadable file is an `Err` result,
//! never a missing one.

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use qrec_core::parse::ffprobe::parse_ffprobe;
use qrec_core::protocol::{MediaInfo, ProbeJob, ProbePurpose, ProbeResult};
use qrec_core::{Event, TakeId};

use super::Ctx;
use super::child::{self, Spec, Stderr, Stdout};
use super::logs::tail_lines;
use crate::msg::Inbox;

/// The most ffprobe JSON kept.
const STDOUT_LIMIT: usize = 16 << 20;

/// Probes every job (in parallel) and posts one `TakeProbed`.
pub(crate) fn run(
    ctx: &Arc<Ctx>,
    take: TakeId,
    purpose: ProbePurpose,
    jobs: Vec<ProbeJob>,
    out: &Inbox,
) {
    let results = probe_all(ctx, &take, &jobs);
    out.event(Event::TakeProbed {
        take,
        purpose,
        results,
    });
}

/// One result per job, in order.
pub(crate) fn probe_all(ctx: &Arc<Ctx>, take: &TakeId, jobs: &[ProbeJob]) -> Vec<ProbeResult> {
    // Each probe ends within its timeout plus the kill grace; the extra
    // margin only covers a worker that couldn't even start ffprobe.
    let mut receivers = Vec::with_capacity(jobs.len());
    for job in jobs {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let size = ctx
            .project
            .resolve(&job.file)
            .ok()
            .and_then(|p| fs::metadata(p).ok())
            .map_or(0, |m| m.len());
        let deadline = ctx.options.scaled(ctx.options.probe_timeout, size)
            + 3 * ctx.options.kill_grace
            + Duration::from_secs(5);
        let (ctx, take, job) = (Arc::clone(ctx), take.clone(), job.clone());
        let spawned = thread::Builder::new()
            .name(format!("qrec-probe-{take}-{}", job.id))
            .spawn(move || {
                let info = catch_unwind(AssertUnwindSafe(|| probe_one(&ctx, &take, &job)))
                    .unwrap_or_else(|panic| {
                        Err(format!(
                            "internal error while probing: {}",
                            child::panic_message(panic.as_ref())
                        ))
                    });
                let _ = tx.send(info);
            });
        match spawned {
            Ok(_) => receivers.push((Some(rx), deadline)),
            Err(e) => {
                tracing::error!("can't start a probe thread: {e}");
                receivers.push((None, deadline));
            }
        }
    }
    jobs.iter()
        .zip(receivers)
        .map(|(job, (rx, deadline))| {
            let info = match rx {
                Some(rx) => rx
                    .recv_timeout(deadline)
                    .unwrap_or_else(|_| Err("ffprobe didn't finish".to_owned())),
                None => Err("can't start a probe thread".to_owned()),
            };
            ProbeResult {
                id: job.id.clone(),
                file: job.file.clone(),
                info,
            }
        })
        .collect()
}

fn probe_one(ctx: &Ctx, take: &TakeId, job: &ProbeJob) -> Result<MediaInfo, String> {
    let path = ctx.project.resolve(&job.file).map_err(|e| e.message)?;
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("{} is missing", job.file));
        }
        Err(e) => return Err(format!("can't read {}: {e}", job.file)),
    };
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", job.file));
    }
    if meta.len() == 0 {
        return Err(format!("{} is empty", job.file));
    }
    let program = job.argv.first().map_or("ffprobe", String::as_str);
    let log = ctx.open_log(
        &ctx.log_base(&format!("probe-{take}-{}", job.id)),
        &job.argv,
    )?;
    let done = child::run(
        Spec::new(&job.argv, Stderr::Log(log.file()))
            .cwd(ctx.root())
            .stdout(Stdout::Capture {
                limit: STDOUT_LIMIT,
            })
            .timeout(ctx.options.scaled(ctx.options.probe_timeout, meta.len()))
            .kill_grace(ctx.options.kill_grace),
    )?;
    if !done.exited_ok() {
        let mut message = done.describe(program);
        if let Some(last) = tail_lines(&log.body(), 1).pop() {
            message.push_str(&format!(": {last}"));
        }
        return Err(message);
    }
    let json = String::from_utf8_lossy(&done.stdout);
    match catch_unwind(AssertUnwindSafe(|| parse_ffprobe(&json, job.kind))) {
        Ok(result) => result,
        Err(panic) => Err(format!(
            "internal error: the ffprobe parser failed: {}",
            child::panic_message(panic.as_ref())
        )),
    }
}
