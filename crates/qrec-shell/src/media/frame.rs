//! `Effect::SaveFrame` → `FrameSaved` | `FrameFailed` | `OutputAppeared`
//! (A§9.4, SPEC §13.2):
//! the core-built ffmpeg argv writes a PNG to a temp file in the
//! destination's folder (held open, checked like an export's, A§8.8), which
//! must be a PNG before it is renamed into place without replacing anything
//! but the file the person confirmed. Copying to the clipboard (`wl-copy`)
//! is not done here: the toast's Copy image action does it later.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

use qrec_core::plans::FramePlan;
use qrec_core::{Event, JobId};

use super::Ctx;
use super::child::{self, OUTPUT_FD_PATH, Spec, Stderr, Stdout};
use super::export::{
    Failure, appeared, check_ffmpeg, destination, extension, finish, missing_input, stopped,
    temp_out, with_file,
};
use super::jobs::JobCtl;
use super::logs::LogFile;
use crate::paths::path_text;

/// The first bytes of every PNG file.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

pub(crate) fn run(ctx: &Ctx, ctl: &JobCtl, job: JobId, plan: &FramePlan) -> Event {
    let mut log_path = String::new();
    let result = save(ctx, ctl, job, plan, &mut log_path);
    match result {
        Ok(path) => Event::FrameSaved {
            job,
            path: path_text(&path),
        },
        Err(failure) => {
            ctl.remove_temps();
            let (message, log) = match failure {
                Failure::Cancelled | Failure::Aborted => ("cancelled".to_owned(), log_path),
                Failure::Failed { message, log, .. } => (message, log),
                // Frame saves have no segments; a VA failure can't happen.
                Failure::VaSegment(inner) => match *inner {
                    Failure::Failed { message, log, .. } => (message, log),
                    _ => ("cancelled".to_owned(), log_path),
                },
                Failure::Appeared { out, bytes, kept } => {
                    return appeared(job, &out, bytes, &kept);
                }
            };
            Event::FrameFailed { job, message, log }
        }
    }
}

fn save(
    ctx: &Ctx,
    ctl: &JobCtl,
    job: JobId,
    plan: &FramePlan,
    log_path: &mut String,
) -> Result<std::path::PathBuf, Failure> {
    let argv = &plan.argv;
    let log = ctx
        .open_log(&ctx.job_log_base(job, "frame"), argv)
        .map_err(|message| Failure::Failed {
            message,
            stderr_tail: Vec::new(),
            log: String::new(),
        })?;
    *log_path = log.display();
    if let Some(missing) = missing_input(ctx, argv) {
        return Err(Failure::plain(format!("{missing} is missing"), &log));
    }
    let dest = destination(ctx, &plan.out, plan.replace).map_err(|m| Failure::plain(m, &log))?;
    let ext = if plan.tmp_ext.is_empty() {
        extension(&plan.out, "png")
    } else {
        plan.tmp_ext.clone()
    };
    let tmp = temp_out(ctx, ctl, job, &dest, &ext, &log)?;
    // ffmpeg writes the temp file through its own descriptor 3 (A§8.8).
    let args = with_file(argv, Path::new(OUTPUT_FD_PATH)).map_err(|m| Failure::plain(m, &log))?;
    log.note(&format!("rendering to {}", path_text(&tmp.shown())));

    // Plans normally ask for `-progress pipe:1`; without it only the exit
    // status counts.
    let reports_progress = argv.windows(2).any(|w| w[0] == "-progress");
    let mut spec = Spec::new(&args, Stderr::Log(log.file()))
        .cwd(ctx.root())
        .output(&tmp.file)
        .timeout(ctx.options.frame_timeout)
        .stop(&ctl.stop)
        .kill_grace(ctx.options.kill_grace);
    if reports_progress {
        spec = spec
            .stdout(Stdout::Progress(Box::new(|_| {})))
            .stall(ctx.options.stall_timeout);
    }
    let done = child::run(spec).map_err(|m| Failure::plain(m, &log))?;
    stopped(ctl, &done)?;
    if reports_progress {
        check_ffmpeg(&done, "frame save", &log)?;
    } else if !done.exited_ok() {
        return Err(Failure::step(
            format!("frame save: {}", done.describe("ffmpeg")),
            &log,
        ));
    }
    check_png(&tmp.file, &log)?;
    tmp.file
        .sync_all()
        .map_err(|e| Failure::plain(format!("can't sync the frame: {e}"), &log))?;
    if ctl.is_cancelled() {
        return Err(Failure::Cancelled);
    }
    let path = finish(ctx, ctl, &log, &tmp, &dest)?;
    log.note(&format!("saved {}", path.display()));
    Ok(path)
}

/// The temp file (read through the job's own descriptor) starts like a PNG.
fn check_png(file: &File, log: &LogFile) -> Result<(), Failure> {
    let mut head = [0u8; 8];
    let ok = file.read_exact_at(&mut head, 0).is_ok() && head == PNG_SIGNATURE;
    if ok {
        Ok(())
    } else {
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Err(Failure::step(
            format!("ffmpeg didn't write a PNG ({size} bytes)"),
            log,
        ))
    }
}
