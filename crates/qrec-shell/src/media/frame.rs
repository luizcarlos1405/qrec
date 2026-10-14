//! `Effect::SaveFrame` → `FrameSaved` | `FrameFailed` (A§9.4, SPEC §13.2):
//! the core-built ffmpeg argv writes a PNG to a temp file next to the
//! destination, which must be a PNG before it is renamed into place without
//! replacing anything (A§8.8). Copying to the clipboard (`wl-copy`) is not
//! done here: the toast's Copy image action does it later.

use std::fs::{self, File};
use std::io::Read;

use qrec_core::plans::FramePlan;
use qrec_core::{Event, JobId};

use super::Ctx;
use super::child::{self, Spec, Stderr, Stdout};
use super::export::{
    Failure, check_ffmpeg, destination, extension, missing_input, replace_output, stopped,
};
use super::jobs::JobCtl;
use super::logs::LogFile;
use super::place::{self, PlaceError, temp_path};

/// The first bytes of every PNG file.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

pub(crate) fn run(ctx: &Ctx, ctl: &JobCtl, job: JobId, plan: &FramePlan) -> Event {
    let mut log_path = String::new();
    let result = save(ctx, ctl, job, plan, &mut log_path);
    match result {
        Ok(path) => Event::FrameSaved {
            job,
            path: path.to_string_lossy().into_owned(),
        },
        Err(failure) => {
            ctl.remove_temps();
            let (message, log) = match failure {
                Failure::Cancelled | Failure::Aborted => ("cancelled".to_owned(), log_path),
                Failure::Failed { message, log, .. } => (message, log),
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
    let mut argv = plan.argv.clone();
    let log = ctx
        .open_log(&ctx.job_log_base(job, "frame"), &argv)
        .map_err(|message| Failure::Failed {
            message,
            stderr_tail: Vec::new(),
            log: String::new(),
        })?;
    *log_path = log.display();
    if let Some(missing) = missing_input(ctx, &argv) {
        return Err(Failure::plain(format!("{missing} is missing"), &log));
    }
    let dest = destination(ctx, &plan.out).map_err(|m| Failure::plain(m, &log))?;
    let dir = dest
        .path()
        .parent()
        .ok_or_else(|| Failure::plain(format!("bad output path {}", plan.out), &log))?;
    let ext = if plan.tmp_ext.is_empty() {
        extension(&plan.out, "png")
    } else {
        plan.tmp_ext.clone()
    };
    let tmp = temp_path(dir, &place::file_stem(dest.path()), job, &ext);
    ctl.add_temp(tmp.clone());
    replace_output(&mut argv, &tmp).map_err(|m| Failure::plain(m, &log))?;
    log.note(&format!("rendering to {}", tmp.display()));

    // Plans normally ask for `-progress pipe:1`; without it only the exit
    // status counts.
    let reports_progress = argv.windows(2).any(|w| w[0] == "-progress");
    let mut spec = Spec::new(&argv, Stderr::Log(log.file()))
        .cwd(ctx.root())
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
    check_png(&tmp, &log)?;
    if ctl.is_cancelled() {
        return Err(Failure::Cancelled);
    }
    match place::place(&tmp, &dest) {
        Ok(path) => {
            ctl.forget_temp(&tmp);
            log.note(&format!("saved {}", path.display()));
            Ok(path)
        }
        Err(e @ PlaceError::Appeared { .. }) => {
            if let PlaceError::Appeared { kept, .. } = &e {
                ctl.forget_temp(kept);
            }
            Err(Failure::plain(e.to_string(), &log))
        }
        Err(e) => Err(Failure::plain(e.to_string(), &log)),
    }
}

fn check_png(path: &std::path::Path, log: &LogFile) -> Result<(), Failure> {
    let mut head = [0u8; 8];
    let ok = File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && head == PNG_SIGNATURE;
    if ok {
        Ok(())
    } else {
        let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Err(Failure::step(
            format!("ffmpeg didn't write a PNG ({size} bytes)"),
            log,
        ))
    }
}
