//! Export and frame save (A§5.8, A§5.9). Owner: output.
//!
//! One export at a time (`Busy`); frame saves may run alongside. Each job
//! snapshots its plan and the files it reads, so editing goes on while it
//! runs. Hand-picked `out` paths are absolute and checked through
//! `ctx.facts.out`; auto names are project-relative (`exports/…`,
//! `frames/…`) and the shell avoids collisions (A§8.8). Nothing here is
//! undoable: jobs live in `runtime`, which is never persisted.

use serde_json::json;

use crate::apply::Outcome;
use crate::config::UserConfig;
use crate::ctx::Ctx;
use crate::error::CoreError;
use crate::export_plan::{ExportOptions, plan_export, require_ffmpeg, resolve_codec};
use crate::frame_plan::{frame_source, plan_frame};
use crate::ids::{JobId, LayerId};
use crate::model::{EncoderPref, Project};
use crate::naming::{export_file_name, frame_file_name};
use crate::on_event::output::export_progress_view;
use crate::protocol::command::{ExportFormat, Quality, VideoCodec};
use crate::protocol::effect::{Effect, LogLevel};
use crate::protocol::reply::CmdResult;
use crate::state::{Capabilities, Job, JobKind, State};
use crate::time::{TimeArg, frame_ns};
use crate::timeline::{absolute_frame, total_frames};

/// `qrec export`: plans the export and emits `RenderExport`.
pub fn export(
    state: &State,
    ctx: &Ctx,
    format: ExportFormat,
    quality: Quality,
    codec: Option<VideoCodec>,
    out: Option<String>,
) -> Result<Outcome, CoreError> {
    let codec = resolve_codec(format, codec)?;
    if let Some(running) = running_export(state) {
        return Err(
            CoreError::busy(format!("an export is already running ({running})"))
                .with_hint("wait for it to finish or cancel it with `qrec export cancel`"),
        );
    }
    let project = &state.project;
    if total_frames(&project.timeline.clips) == 0 {
        return Err(
            CoreError::invalid("nothing to export: the timeline is empty")
                .with_hint("record a take or add one from the bin first"),
        );
    }
    let out = match out {
        None => export_file_name(
            &project.name,
            ctx.now,
            ctx.utc_offset_secs,
            format.extension(),
        ),
        Some(path) => checked_out(ctx, &path, "export to")?,
    };
    let caps = &state.runtime.capabilities;
    let opts = ExportOptions {
        format,
        quality,
        codec,
        out,
        allow_vaapi: allow_vaapi(project, &ctx.facts.config, caps),
    };
    let plan = plan_export(project, caps, &opts)?;

    let mut next = state.clone();
    let id = next.runtime.alloc_job_id();
    let seg_frames: Vec<u64> = plan.segments.iter().map(|s| s.frames).collect();
    let job = Job {
        id,
        kind: JobKind::Export {
            out: plan.out.clone(),
            files: plan.files.clone(),
            total_frames: plan.total_frames,
            seg_done: vec![0; seg_frames.len()],
            seg_frames,
            stitch_total_ns: frame_ns(plan.total_frames as i64, project.canvas.fps).max(0) as u64,
            stitch_done_ns: 0,
            stitching: false,
        },
        req: ctx.req.clone(),
        started: ctx.now,
    };
    let mut effects = vec![Effect::Log {
        level: LogLevel::Info,
        message: format!(
            "{id}: exporting {} frames in {} segments to {}",
            plan.total_frames,
            plan.segments.len(),
            plan.out
        ),
    }];
    if let (Some(req), Some(progress)) = (&ctx.req, export_progress_view(&job, ctx.now)) {
        effects.push(Effect::Progress {
            req: req.clone(),
            progress,
        });
    }
    effects.push(Effect::RenderExport { job: id, plan });
    next.runtime.jobs.insert(id, job);
    Ok(Outcome::new(next, effects, None))
}

/// `qrec export cancel`: `job: None` cancels the running export. The job
/// ends when the shell reports `ExportCancelled`.
pub fn export_cancel(state: &State, _ctx: &Ctx, job: Option<JobId>) -> Result<Outcome, CoreError> {
    let id = match job {
        Some(id) => match state.runtime.jobs.get(&id) {
            Some(Job {
                kind: JobKind::Export { .. },
                ..
            }) => id,
            Some(_) => return Err(CoreError::invalid(format!("{id} isn't an export"))),
            None => {
                return Err(CoreError::not_found(format!("{id} isn't running"))
                    .with_hint("`qrec status` lists the running jobs"));
            }
        },
        None => running_export(state).ok_or_else(|| CoreError::invalid("no export is running"))?,
    };
    let result =
        CmdResult::new(format!("Cancelling the export ({id})")).with_result(&json!({ "job": id }));
    Ok(Outcome::new(
        state.clone(),
        vec![Effect::CancelJob { job: id }],
        Some(Ok(result)),
    ))
}

/// `qrec frame save --at T [--layer L] [--out P]`. The frame is rendered
/// by ffmpeg, which has no cache to fall back on: without it (or its PNG
/// encoder) the command is refused with `missing_dependency` (exit 5)
/// before a job starts, like an export.
pub fn frame_save(
    state: &State,
    ctx: &Ctx,
    at: TimeArg,
    layer: Option<LayerId>,
    out: Option<String>,
) -> Result<Outcome, CoreError> {
    let project = &state.project;
    let fps = project.canvas.fps;
    let at = absolute_frame(&at, fps, "frame save --at")?;
    let (take, _) = frame_source(project, at)?;
    let out = match out {
        None => frame_file_name(&project.name, &take, at, fps),
        Some(path) => checked_out(ctx, &path, "save a frame to")?,
    };
    let plan = plan_frame(project, at, layer.as_ref(), &out)?;
    require_ffmpeg(&state.runtime.capabilities, &["png"])?;

    let mut next = state.clone();
    let id = next.runtime.alloc_job_id();
    next.runtime.jobs.insert(
        id,
        Job {
            id,
            kind: JobKind::FrameSave {
                out: plan.out.clone(),
                files: plan.files.clone(),
            },
            req: ctx.req.clone(),
            started: ctx.now,
        },
    );
    Ok(Outcome::new(
        next,
        vec![Effect::SaveFrame { job: id, plan }],
        None,
    ))
}

/// The running export, if any.
fn running_export(state: &State) -> Option<JobId> {
    state
        .runtime
        .jobs
        .values()
        .find(|job| matches!(job.kind, JobKind::Export { .. }))
        .map(|job| job.id)
}

/// A hand-picked output path (A§5.9): inside `takes/`, `.qrec/` or
/// `qrec.json` is `Invalid` (not confirmable); an existing file needs
/// confirmation. Returns the absolute path to write.
fn checked_out(ctx: &Ctx, requested: &str, verb: &str) -> Result<String, CoreError> {
    match &ctx.facts.out {
        Some(facts) => {
            if let Some(why) = &facts.forbidden {
                return Err(
                    CoreError::invalid(format!("can't {verb} {}: {why}", facts.abs))
                        .with_hint("pick a path outside takes/ and .qrec/"),
                );
            }
            if facts.exists && !ctx.confirmed {
                return Err(
                    CoreError::confirmation_required(format!("overwrite {}", facts.abs))
                        .with_details(json!({ "files": [{ "path": facts.abs }] })),
                );
            }
            Ok(facts.abs.clone())
        }
        // The shell resolves every hand-picked path; without its facts the
        // path must at least be absolute (the CLI resolves it, A§10).
        None if requested.starts_with('/') => Ok(requested.to_owned()),
        None => Err(CoreError::usage(format!(
            "--out must be an absolute path, got {requested:?}"
        ))),
    }
}

/// The VA export chain may be used when the effective encoder (A§2.1) isn't
/// x264 and the VA node the shell verified is the effective device. Whether
/// the chain works on this machine is `VaapiCaps::ffmpeg_h264`, checked by
/// the planner.
fn allow_vaapi(project: &Project, config: &UserConfig, caps: &Capabilities) -> bool {
    let preference = match project.recording.encoder {
        EncoderPref::Auto => config.encoder.preference,
        explicit => explicit,
    };
    if preference == EncoderPref::X264 {
        return false;
    }
    let device = project
        .recording
        .vaapi_device
        .as_ref()
        .or(config.encoder.vaapi_device.as_ref());
    match (&caps.vaapi, device) {
        (Some(va), Some(device)) => va.device == *device,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::OutFacts;
    use crate::ids::{ReqId, TakeId};
    use crate::protocol::command::Command;
    use crate::state::{RecordMode, RecordPhase, VaapiCaps};
    use crate::testkit::{self, ProjectBuilder};
    use crate::{ErrorCode, apply};

    const NOW: &str = "2026-10-14T12:34:56Z";

    fn state() -> State {
        testkit::state(
            ProjectBuilder::new()
                .name("my-video")
                .screen_layer("L1", "DP-4")
                .mic("A1", "mic")
                .take("take-0001", 300)
                .clip("c1", "take-0001", 0, 100)
                .clip("c2", "take-0001", 150, 300)
                .build(),
        )
    }

    fn export_cmd(out: Option<&str>) -> Command {
        Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Standard,
            codec: None,
            out: out.map(String::from),
        }
    }

    fn render_plan(outcome: &Outcome) -> &crate::plans::ExportPlan {
        outcome
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::RenderExport { plan, .. } => Some(plan),
                _ => None,
            })
            .expect("a RenderExport effect")
    }

    #[test]
    fn export_starts_a_job() {
        let state = state();
        let ctx = testkit::ctx(NOW, 1);
        let out = apply(&state, &ctx, export_cmd(None)).unwrap();
        assert_eq!(out.reply, None, "the reply comes when the export ends");
        let plan = render_plan(&out);
        assert_eq!(plan.out, "exports/my-video-20261014-123456.mp4");
        assert_eq!(plan.segments.len(), 2);
        let job = &out.state.runtime.jobs[&JobId(1)];
        assert_eq!(job.req, Some(ReqId("r1".into())));
        assert_eq!(job.started, ctx.now);
        assert_eq!(
            job.kind,
            JobKind::Export {
                out: plan.out.clone(),
                files: plan.files.clone(),
                total_frames: 250,
                seg_frames: vec![100, 150],
                seg_done: vec![0, 0],
                stitch_total_ns: 8_333_333_333,
                stitch_done_ns: 0,
                stitching: false,
            }
        );
        assert_eq!(out.state.runtime.next_id, 2);
        assert_eq!(
            out.state.project, state.project,
            "exports change no project data"
        );
        assert!(
            matches!(&out.effects[1], Effect::Progress { progress, .. } if progress.done == 0 && progress.total == 250)
        );
        insta::assert_debug_snapshot!("export_effects_without_plan", out.effects[..2].to_vec());

        // One export at a time; frame saves may run alongside.
        let err = apply(&out.state, &ctx, export_cmd(None)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        assert_eq!(err.message, "an export is already running (job-1)");
        let frame = Command::FrameSave {
            at: "10f".parse().unwrap(),
            layer: None,
            out: None,
        };
        assert!(apply(&out.state, &ctx, frame).is_ok());
    }

    #[test]
    fn export_checks() {
        let state = state();
        let ctx = testkit::ctx(NOW, 1);
        let webm_h264 = Command::Export {
            format: ExportFormat::Webm,
            quality: Quality::Draft,
            codec: Some(VideoCodec::H264),
            out: None,
        };
        assert_eq!(
            apply(&state, &ctx, webm_h264).unwrap_err().code,
            ErrorCode::Usage
        );
        let mut empty = state.clone();
        empty.project.timeline.clips.clear();
        let err = apply(&empty, &ctx, export_cmd(None)).unwrap_err();
        assert_eq!(err.message, "nothing to export: the timeline is empty");
        let mut recording = state.clone();
        recording.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(2),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        assert_eq!(
            apply(&recording, &ctx, export_cmd(None)).unwrap_err().code,
            ErrorCode::Busy
        );
    }

    #[test]
    fn hand_picked_outputs() {
        let state = state();
        let mut ctx = testkit::ctx(NOW, 1);
        ctx.facts.out = Some(OutFacts {
            abs: "/home/me/v/takes/x.mp4".into(),
            exists: false,
            forbidden: Some("it is inside takes/".into()),
        });
        let err = apply(&state, &ctx, export_cmd(Some("/home/me/v/takes/x.mp4"))).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(
            err.message,
            "can't export to /home/me/v/takes/x.mp4: it is inside takes/"
        );
        ctx.confirmed = true;
        assert_eq!(
            apply(&state, &ctx, export_cmd(Some("/home/me/v/takes/x.mp4")))
                .unwrap_err()
                .code,
            ErrorCode::Invalid,
            "forbidden paths can't be confirmed"
        );

        ctx.confirmed = false;
        ctx.facts.out = Some(OutFacts {
            abs: "/home/me/out.mp4".into(),
            exists: true,
            forbidden: None,
        });
        let err = apply(&state, &ctx, export_cmd(Some("/home/me/out.mp4"))).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfirmationRequired);
        assert_eq!(
            err.message,
            "confirmation required: overwrite /home/me/out.mp4"
        );
        assert_eq!(
            err.details,
            Some(json!({"files": [{"path": "/home/me/out.mp4"}]}))
        );
        ctx.confirmed = true;
        let out = apply(&state, &ctx, export_cmd(Some("/home/me/out.mp4"))).unwrap();
        assert_eq!(render_plan(&out).out, "/home/me/out.mp4");
        assert_eq!(
            render_plan(&out).stitch.argv.last().unwrap(),
            "file:/home/me/out.mp4"
        );

        ctx.facts.out = None;
        assert_eq!(
            apply(&state, &ctx, export_cmd(Some("out.mp4")))
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
        assert!(apply(&state, &ctx, export_cmd(Some("/tmp/out.mp4"))).is_ok());
    }

    #[test]
    fn effective_encoder() {
        let mut state = state();
        state.runtime.capabilities.vaapi = Some(VaapiCaps {
            device: "/dev/dri/renderD129".into(),
            vendor: 0x8086,
            ffmpeg_h264: true,
            gst_h264_factory: None,
        });
        let mut config = UserConfig::default();
        let caps = state.runtime.capabilities.clone();
        assert!(allow_vaapi(&state.project, &config, &caps));
        config.encoder.preference = EncoderPref::X264;
        assert!(
            !allow_vaapi(&state.project, &config, &caps),
            "config prefers x264"
        );
        state.project.recording.encoder = EncoderPref::Vaapi;
        assert!(
            allow_vaapi(&state.project, &config, &caps),
            "the project wins"
        );
        config.encoder.vaapi_device = Some("/dev/dri/renderD128".into());
        assert!(
            !allow_vaapi(&state.project, &config, &caps),
            "verified another node"
        );
        state.project.recording.vaapi_device = Some("/dev/dri/renderD129".into());
        assert!(allow_vaapi(&state.project, &config, &caps));
        let ctx = testkit::ctx(NOW, 1);
        let out = apply(&state, &ctx, export_cmd(None)).unwrap();
        assert!(
            render_plan(&out).segments[0]
                .argv
                .contains(&"h264_vaapi".to_owned())
        );
        state.project.recording.encoder = EncoderPref::X264;
        let out = apply(&state, &ctx, export_cmd(None)).unwrap();
        assert!(
            render_plan(&out).segments[0]
                .argv
                .contains(&"libx264".to_owned())
        );
    }

    #[test]
    fn cancel() {
        let state = state();
        let ctx = testkit::ctx(NOW, 1);
        assert_eq!(
            apply(&state, &ctx, Command::ExportCancel { job: None })
                .unwrap_err()
                .message,
            "no export is running"
        );
        let running = apply(&state, &ctx, export_cmd(None)).unwrap().state;
        let out = apply(&running, &ctx, Command::ExportCancel { job: None }).unwrap();
        assert_eq!(out.effects, vec![Effect::CancelJob { job: JobId(1) }]);
        assert_eq!(out.state, running, "the job ends when the shell says so");
        let reply = out.reply.unwrap().unwrap();
        assert_eq!(reply.message, "Cancelling the export (job-1)");
        assert_eq!(reply.result, json!({"job": 1}));
        let err = apply(
            &running,
            &ctx,
            Command::ExportCancel {
                job: Some(JobId(7)),
            },
        )
        .unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::NotFound, "job-7 isn't running")
        );
        let frame = Command::FrameSave {
            at: "1f".parse().unwrap(),
            layer: None,
            out: None,
        };
        let both = apply(&running, &ctx, frame).unwrap().state;
        let err = apply(
            &both,
            &ctx,
            Command::ExportCancel {
                job: Some(JobId(2)),
            },
        )
        .unwrap_err();
        assert_eq!(err.message, "job-2 isn't an export");
        assert!(
            apply(
                &both,
                &ctx,
                Command::ExportCancel {
                    job: Some(JobId(1))
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn frame_save_starts_a_job() {
        let state = state();
        let ctx = testkit::ctx(NOW, 1);
        let cmd = Command::FrameSave {
            at: "00:00:04:00".parse().unwrap(),
            layer: None,
            out: None,
        };
        let out = apply(&state, &ctx, cmd).unwrap();
        assert_eq!(out.reply, None);
        let [Effect::SaveFrame { job, plan }] = out.effects.as_slice() else {
            panic!("{:?}", out.effects)
        };
        assert_eq!(*job, JobId(1));
        // Timeline frame 120 is take frame 170.
        assert_eq!(plan.out, "frames/my-video-take-0001-00000400.png");
        assert!(
            plan.argv
                .windows(2)
                .any(|w| w[0] == "-ss" && w[1] == "5.664667")
        );
        assert_eq!(
            out.state.runtime.jobs[&JobId(1)].kind,
            JobKind::FrameSave {
                out: plan.out.clone(),
                files: plan.files.clone()
            }
        );
    }

    #[test]
    fn frame_save_checks() {
        let state = state();
        let mut ctx = testkit::ctx(NOW, 1);
        let save = |at: &str, layer: Option<&str>, out: Option<&str>| Command::FrameSave {
            at: at.parse().unwrap(),
            layer: layer.map(|l| l.parse().unwrap()),
            out: out.map(String::from),
        };
        assert_eq!(
            apply(&state, &ctx, save("+1f", None, None))
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
        assert_eq!(
            apply(&state, &ctx, save("250f", None, None))
                .unwrap_err()
                .code,
            ErrorCode::Invalid
        );
        assert_eq!(
            apply(&state, &ctx, save("0f", Some("L7"), None))
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        ctx.facts.out = Some(OutFacts {
            abs: "/home/me/f.png".into(),
            exists: true,
            forbidden: None,
        });
        let err = apply(&state, &ctx, save("0f", None, Some("/home/me/f.png"))).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfirmationRequired);
        ctx.confirmed = true;
        let out = apply(&state, &ctx, save("0f", Some("L1"), Some("/home/me/f.png"))).unwrap();
        let Effect::SaveFrame { plan, .. } = &out.effects[0] else {
            panic!()
        };
        assert_eq!(plan.out, "/home/me/f.png");
    }

    /// Like an export, a frame save needs ffmpeg: without it the command is
    /// `missing_dependency` (exit 5) and no job starts (SPEC §14, A§4.8).
    #[test]
    fn frame_save_without_ffmpeg_is_a_missing_dependency() {
        let save = Command::FrameSave {
            at: "10f".parse().unwrap(),
            layer: None,
            out: None,
        };
        let ctx = testkit::ctx(NOW, 1);
        let mut state = state();
        state.runtime.capabilities.ffmpeg = None;
        let err = apply(&state, &ctx, save.clone()).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str(), err.exit_code()),
            (ErrorCode::MissingDependency, "ffmpeg wasn't found", 5)
        );
        assert!(err.hint.unwrap().contains("qrec doctor"));

        let mut state = self::state();
        let ffmpeg = state.runtime.capabilities.ffmpeg.as_mut().unwrap();
        ffmpeg.encoders.remove("png");
        let err = apply(&state, &ctx, save.clone()).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::MissingDependency, "ffmpeg has no png encoder")
        );
        // An empty list means the shell didn't list them.
        state
            .runtime
            .capabilities
            .ffmpeg
            .as_mut()
            .unwrap()
            .encoders
            .clear();
        assert!(apply(&state, &ctx, save).is_ok());
    }
}
