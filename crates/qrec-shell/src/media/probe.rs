//! `Effect::Probe` → `Event::TakeProbed` (A§5.3 finalize and recover): each
//! `ProbeJob` runs its core-built ffprobe argv (`-count_packets`) on its own
//! thread, with a timeout, and `parse::ffprobe` reads the JSON. Results keep
//! the jobs' order; a missing, empty or unreadable file is an `Err` result,
//! never a missing one.
//!
//! Every error says whose fault it is (`ProbeFailure`, A§4.7): the file's
//! (`Media`: missing, empty, ffprobe exited with an error or was killed by
//! it, unparsable output) or the probe's own (`MissingTool`: ffprobe can't
//! be run; `Tool`: it timed out, or the log or worker couldn't be made). The
//! core never changes a take's facts on the probe's own failure.

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use qrec_core::parse::ffprobe::parse_ffprobe;
use qrec_core::protocol::{MediaInfo, ProbeError, ProbeJob, ProbePurpose, ProbeResult};
use qrec_core::{Event, TakeId};

use super::Ctx;
use super::child::{self, Ended, Spec, Stderr, Stdout};
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
                        Err(ProbeError::tool(format!(
                            "internal error while probing: {}",
                            child::panic_message(panic.as_ref())
                        )))
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
                    .unwrap_or_else(|_| Err(ProbeError::tool("ffprobe didn't finish"))),
                None => Err(ProbeError::tool("can't start a probe thread")),
            };
            ProbeResult {
                id: job.id.clone(),
                file: job.file.clone(),
                info,
            }
        })
        .collect()
}

pub(crate) fn probe_one(ctx: &Ctx, take: &TakeId, job: &ProbeJob) -> Result<MediaInfo, ProbeError> {
    let path = ctx
        .project
        .resolve(&job.file)
        .map_err(|e| ProbeError::media(e.message))?;
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ProbeError::media(format!("{} is missing", job.file)));
        }
        Err(e) => return Err(ProbeError::media(format!("can't read {}: {e}", job.file))),
    };
    if !meta.is_file() {
        return Err(ProbeError::media(format!(
            "{} is not a regular file",
            job.file
        )));
    }
    if meta.len() == 0 {
        return Err(ProbeError::media(format!("{} is empty", job.file)));
    }
    let program = job.argv.first().map_or("ffprobe", String::as_str);
    let log = ctx
        .open_log(
            &ctx.log_base(&format!("probe-{take}-{}", job.id)),
            &job.argv,
        )
        .map_err(ProbeError::tool)?;
    let done = child::start(
        Spec::new(&job.argv, Stderr::Log(log.file()))
            .cwd(ctx.root())
            .stdout(Stdout::Capture {
                limit: STDOUT_LIMIT,
            })
            .timeout(ctx.options.scaled(ctx.options.probe_timeout, meta.len()))
            .kill_grace(ctx.options.kill_grace),
    )
    .map_err(|e| {
        if e.not_found {
            ProbeError::missing_tool(e.message)
        } else {
            ProbeError::tool(e.message)
        }
    })?;
    if !done.exited_ok() {
        let mut message = done.describe(program);
        if let Some(last) = tail_lines(&log.body(), 1).pop() {
            message.push_str(&format!(": {last}"));
        }
        // ffprobe that exited (with an error, or killed by its own crash)
        // judged the file; one that ran out of time or was stopped didn't,
        // nor did a wrapper that couldn't run it (126/127, like a shell).
        let tool_status = done
            .status
            .and_then(|s| s.code())
            .is_some_and(|code| code == 126 || code == 127);
        return Err(match done.ended {
            Ended::Exited if tool_status => ProbeError::missing_tool(message),
            Ended::Exited if done.status.is_some() => ProbeError::media(message),
            Ended::Exited | Ended::TimedOut(_) | Ended::Stalled(_) | Ended::Stopped => {
                ProbeError::tool(message)
            }
        });
    }
    let json = String::from_utf8_lossy(&done.stdout);
    match catch_unwind(AssertUnwindSafe(|| parse_ffprobe(&json, job.kind))) {
        Ok(result) => result.map_err(ProbeError::media),
        Err(panic) => Err(ProbeError::tool(format!(
            "internal error: the ffprobe parser failed: {}",
            child::panic_message(panic.as_ref())
        ))),
    }
}
