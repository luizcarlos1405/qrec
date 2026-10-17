//! The export panel (G§4.8, S15a–S17): setup captions, the running
//! readout, done and failed. Owner: WP7. [`caption`] was built by WP0a: it
//! names the encoder the export will use, so it shares the export's rule
//! (`export_plan::export_vaapi`).
//!
//! The running numbers come from `on_event::output::export_readout` (WP0a).
//! Lines that mix the UI face and the mono face (the summary, the done
//! details) also come as [`Run`]s, one label each (G§3.14).

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};

use crate::config::UserConfig;
use crate::export_plan::{export_vaapi, render_units};
use crate::ids::ClipId;
use crate::naming::export_file_name;
use crate::on_event::output::{STDERR_TAIL_LINES, clips_done, export_readout};
use crate::protocol::command::{ExportFormat, VideoCodec};
use crate::state::{Capabilities, Job, JobKind, State};
use crate::timeline::total_frames;
use crate::view::format;

/// A piece of a line in one face: the UI face, or the mono face for
/// numbers (G§3.14: a line that mixes families is baseline-aligned labels).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub text: String,
    pub mono: bool,
}

impl Run {
    fn ui(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            mono: false,
        }
    }

    fn mono(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            mono: true,
        }
    }
}

/// The runs' text joined: the line as one string.
pub fn joined(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// The setup step's caption for a format (G§4.8). MP4: "H.264 + AAC 192
/// kbps · VA-API on renderD129" when the export would encode on that node
/// (`export_plan::export_vaapi`: the effective encoder isn't x264, the
/// node is the pinned one, its chain works), else "H.264 + AAC 192 kbps ·
/// software encode (x264)". WebM: "VP9 + Opus 128 kbps · software
/// encode", with AV1 "AV1 + Opus 128 kbps · software encode (libsvtav1)".
/// `config` is the user config the GUI read at startup (the one it gives
/// `Ctx::preview`).
pub fn caption(
    format: ExportFormat,
    codec: Option<VideoCodec>,
    state: &State,
    config: &UserConfig,
) -> String {
    match (format, codec) {
        (ExportFormat::Webm, Some(VideoCodec::Av1)) => {
            "AV1 + Opus 128 kbps \u{b7} software encode (libsvtav1)".to_owned()
        }
        (ExportFormat::Webm, _) => "VP9 + Opus 128 kbps \u{b7} software encode".to_owned(),
        (ExportFormat::Mp4, _) => {
            match export_vaapi(&state.project, config, &state.runtime.capabilities) {
                Some(va) => {
                    let node = va.device.rsplit('/').next().unwrap_or(&va.device);
                    format!("H.264 + AAC 192 kbps \u{b7} VA-API on {node}")
                }
                None => "H.264 + AAC 192 kbps \u{b7} software encode (x264)".to_owned(),
            }
        }
    }
}

/// The AV1 switch's caption (S15c).
pub const AV1_CAPTION: &str = "Encodes with libsvtav1 instead of VP9.";

/// Why the AV1 switch is off: "This ffmpeg has no libsvtav1"; `None` when
/// it can be used.
pub fn av1_unavailable(caps: &Capabilities) -> Option<String> {
    match &caps.ffmpeg {
        Some(ffmpeg) if ffmpeg.encoders.contains("libsvtav1") => None,
        _ => Some("This ffmpeg has no libsvtav1".to_owned()),
    }
}

/// Whether the export panel can open now (`Ctrl+E`, the header slot):
/// always while an export runs (it shows it, D-49); else not while
/// recording ("Not while recording") or on an empty timeline ("The
/// timeline is empty"). `Err` is the reason a refused `Ctrl+E` toasts
/// (D-34), the header slot's tooltip (`view::header`'s export slot reads
/// the same).
pub fn panel_enabled(state: &State) -> Result<(), String> {
    let exporting = state
        .runtime
        .jobs
        .values()
        .any(|job| matches!(job.kind, JobKind::Export { .. }));
    if exporting {
        Ok(())
    } else if !state.runtime.record.is_idle() {
        Err("Not while recording".to_owned())
    } else if state.project.timeline.clips.is_empty() {
        Err("The timeline is empty".to_owned())
    } else {
        Ok(())
    }
}

/// The auto output name: `naming::export_file_name(name, local now)`.
pub fn output_preview(
    state: &State,
    format: ExportFormat,
    now: DateTime<Utc>,
    utc_offset_secs: i32,
) -> String {
    export_file_name(
        &state.project.name,
        now,
        utc_offset_secs,
        format.extension(),
    )
}

/// "1920×1080 · 30 fps · 00:04:06:11 · 10 clips".
pub fn summary(state: &State) -> String {
    joined(&summary_runs(state))
}

/// [`summary`] in faces: the size and the duration in the mono face (S15a).
pub fn summary_runs(state: &State) -> Vec<Run> {
    let project = &state.project;
    let canvas = &project.canvas;
    let clips = &project.timeline.clips;
    let n = clips.len();
    let noun = if n == 1 { "clip" } else { "clips" };
    vec![
        Run::mono(format!("{}\u{d7}{}", canvas.width, canvas.height)),
        Run::ui(format!(" \u{b7} {} \u{b7} ", format::fps(canvas.fps))),
        Run::mono(format::timecode(total_frames(clips), canvas.fps)),
        Run::ui(format!(" \u{b7} {n} {noun}")),
    ]
}

/// The running panel (S15b).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Running {
    /// The output, as the job names it ("exports/…mp4" for an auto name).
    pub out: String,
    /// "42%".
    pub percent: String,
    pub fraction: u8,
    /// "Rendering clip 6/10" or "Stitching".
    pub label: String,
    /// "3,449 / 7,391 frames".
    pub frames: String,
    /// "Elapsed 00:00:38 · About 00:00:52 left" ("Elapsed 00:00:38" until
    /// an ETA is known).
    pub times: String,
    /// "Exporting the timeline as it was at 15:30:12. You can keep editing."
    pub note: String,
}

/// The running panel of `job` at `now` (local times at `utc_offset_secs`);
/// `None` for a job that isn't an export.
pub fn running(job: &Job, now: DateTime<Utc>, utc_offset_secs: i32) -> Option<Running> {
    let readout = export_readout(job, now)?;
    let JobKind::Export { out, .. } = &job.kind else {
        return None;
    };
    let times = match readout.eta_secs {
        Some(eta) => format!(
            "Elapsed {} \u{b7} About {} left",
            format::hms(readout.elapsed_secs),
            format::hms(eta)
        ),
        None => format!("Elapsed {}", format::hms(readout.elapsed_secs)),
    };
    let started = local(job.started, utc_offset_secs).format("%H:%M:%S");
    Some(Running {
        out: out.clone(),
        percent: format!("{}%", readout.percent),
        fraction: readout.percent,
        label: readout.label,
        frames: format!(
            "{} / {} frames",
            format::count(readout.rendered),
            format::count(readout.total)
        ),
        times,
        note: format!("Exporting the timeline as it was at {started}. You can keep editing."),
    })
}

fn local(at: DateTime<Utc>, utc_offset_secs: i32) -> DateTime<FixedOffset> {
    let offset = FixedOffset::east_opt(utc_offset_secs)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC is a valid offset"));
    at.with_timezone(&offset)
}

/// "Layers! Like an onion. Your video is ready." (S16).
pub const DONE_LINE: &str = crate::on_event::output::EXPORT_DONE_TITLE;

/// The done panel (S16).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Done {
    /// The output as people read it.
    pub path: String,
    /// "88.4 MB · 1920×1080 · 30 fps · 00:04:06:11 · took 00:01:31".
    pub details: String,
    /// [`Done::details`] in faces: the numbers mono, "· took" in the UI
    /// face (S16).
    pub details_runs: Vec<Run>,
}

/// The done panel for an export of `state`'s timeline (the snapshot it
/// started from) written to `path` (as people read it: `bytes`, after
/// `took_secs`).
pub fn done(state: &State, path: &str, bytes: u64, took_secs: u64) -> Done {
    let canvas = &state.project.canvas;
    let frames = state
        .runtime
        .jobs
        .values()
        .find_map(|job| match &job.kind {
            JobKind::Export { total_frames, .. } => Some(*total_frames),
            _ => None,
        })
        .unwrap_or_else(|| total_frames(&state.project.timeline.clips));
    let runs = vec![
        Run::mono(format!(
            "{} \u{b7} {}\u{d7}{} \u{b7} {} \u{b7} {}",
            format::size(bytes),
            canvas.width,
            canvas.height,
            format::fps(canvas.fps),
            format::timecode(frames, canvas.fps)
        )),
        Run::ui(" \u{b7} took "),
        Run::mono(format::hms(took_secs)),
    ];
    Done {
        path: path.to_owned(),
        details: joined(&runs),
        details_runs: runs,
    }
}

/// The failed panel (S17).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failed {
    /// "ffmpeg stopped while rendering clip 3/10 (take 3, c1b2). Nothing
    /// was written to exports/."
    pub lead: String,
    /// From a pattern table over the stderr tail ("The disk is full. Free
    /// some space and try again."); none when nothing matches.
    pub hint: Option<String>,
    /// "Failed at 37%".
    pub at: String,
    /// Where the bar stopped, 0–100 (the percent of `at`).
    pub fraction: u8,
    /// At most 30 lines (`details.stderrTail`).
    pub stderr_tail: Vec<String>,
    /// `details.log`.
    pub log: Option<String>,
}

/// What the GUI knows of an export that failed.
#[derive(Debug, Clone, Copy)]
pub struct FailedExport<'a> {
    /// The snapshot the export started from: its timeline is what was
    /// rendered (its render units name the clip, A§9.3).
    pub started: &'a State,
    /// The export's job as last seen before it ended: how far each unit
    /// got (`seg_done`), and so the percent the bar stopped at. `None`
    /// when no progress was seen.
    pub job: Option<&'a Job>,
    /// The error's message ("export failed: segment 3/9 (take-0003, frames
    /// 0–268): ffmpeg exited with code 1"): its segment names the unit
    /// that failed when several render at once.
    pub message: &'a str,
    pub stderr_tail: &'a [String],
    pub log: Option<&'a str>,
    /// Where the output would have gone, as people read it ("exports/").
    pub folder: &'a str,
}

/// The failed panel (S17): the lead names the timeline clip the failed
/// unit was rendering (the error's segment, else the first unit the job
/// hadn't finished), counted among the clips of the timeline the export
/// started from (a unit of continuous clips counts each, A§9.3), with the
/// frames the unit had rendered placing it among its clips; the percent is
/// the job's last; the hint comes from the stderr tail.
pub fn failed(f: &FailedExport<'_>) -> Failed {
    let units = render_units(&f.started.project);
    let timeline = &f.started.project.timeline.clips;
    let length = |id: &ClipId| timeline.iter().find(|c| c.id == *id).map_or(0, |c| c.len());
    let count: usize = units.iter().map(|u| u.clips.len()).sum();
    let (seg_frames, seg_done) = match f.job.map(|j| &j.kind) {
        Some(JobKind::Export {
            seg_frames,
            seg_done,
            ..
        }) => (seg_frames.as_slice(), seg_done.as_slice()),
        _ => (&[][..], &[][..]),
    };
    let nothing = format!("Nothing was written to {}.", f.folder);
    let message = f
        .message
        .strip_prefix("export failed: ")
        .unwrap_or(f.message);
    let stitching = message.starts_with("stitch");
    // Where the job was says which unit failed only when ffmpeg is what
    // failed (not a folder that went away, say).
    let ffmpeg = message.contains("ffmpeg");
    let unit = segment_of(message)
        .map(|k| k.wrapping_sub(1))
        .or_else(|| {
            ffmpeg
                .then(|| seg_done.iter().zip(seg_frames).position(|(d, n)| d < n))
                .flatten()
        })
        .filter(|u| *u < units.len());
    let lead = match unit {
        Some(u) if !stitching => {
            let unit = &units[u];
            let lengths: Vec<u64> = unit.clips.iter().map(length).collect();
            let done = seg_done.get(u).copied().unwrap_or(0);
            let within = clips_done(&lengths, done).min(lengths.len().saturating_sub(1));
            let before: usize = units[..u].iter().map(|u| u.clips.len()).sum();
            let clip = unit
                .clips
                .get(within)
                .map(ToString::to_string)
                .unwrap_or_default();
            format!(
                "ffmpeg stopped while rendering clip {}/{count} (take {}, {clip}). {nothing}",
                before + within + 1,
                unit.take.number()
            )
        }
        _ if stitching => format!("ffmpeg stopped while joining the clips. {nothing}"),
        _ => format!(
            "{}. {nothing}",
            format::capitalized(message.trim_end_matches('.'))
        ),
    };
    let percent = f
        .job
        .and_then(|job| export_readout(job, job.started))
        .map_or(0, |r| r.percent);
    let cut = f.stderr_tail.len().saturating_sub(STDERR_TAIL_LINES);
    Failed {
        lead,
        hint: hint(f.stderr_tail, &f.started.runtime.capabilities),
        at: format!("Failed at {percent}%"),
        fraction: percent,
        stderr_tail: f.stderr_tail[cut..].to_vec(),
        log: f.log.map(str::to_owned),
    }
}

/// The 1-based unit of "segment 3/9 (…)" in a failure's message.
fn segment_of(message: &str) -> Option<usize> {
    let rest = &message[message.find("segment ")? + "segment ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !rest[digits.len()..].starts_with('/') {
        return None;
    }
    digits.parse().ok()
}

/// The hint line (G§4.8): what the stderr tail says went wrong, when it
/// is something the person can fix.
pub fn hint(tail: &[String], caps: &Capabilities) -> Option<String> {
    let lower: Vec<String> = tail.iter().map(|l| l.to_lowercase()).collect();
    let va = lower.iter().any(|l| {
        (l.contains("vaapi") || l.contains("libva") || l.contains("va-api"))
            && (l.contains("fail") || l.contains("error") || l.contains("cannot"))
    });
    if va {
        let node = tail
            .iter()
            .find_map(|l| render_node(l))
            .or_else(|| caps.vaapi.as_ref().map(|v| v.device.clone()));
        let on = node.map_or_else(String::new, |n| format!(" on {n}"));
        return Some(format!(
            "VA-API couldn't start{on}. Try Settings \u{203a} Recording \u{203a} Encoder: x264."
        ));
    }
    if lower.iter().any(|l| l.contains("no space left")) {
        return Some("The disk is full. Free some space and try again.".to_owned());
    }
    if let Some(line) = tail
        .iter()
        .find(|l| l.to_lowercase().contains("unknown encoder"))
    {
        let name = line
            .split(['\'', '"'])
            .nth(1)
            .filter(|n| !n.is_empty())
            .unwrap_or("an encoder it needs");
        return Some(format!("This ffmpeg has no {name}. Run qrec doctor."));
    }
    None
}

/// A `/dev/dri/renderD<n>` named in `line`.
fn render_node(line: &str) -> Option<String> {
    let at = line.find("/dev/dri/renderD")?;
    let rest = &line[at + "/dev/dri/renderD".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then(|| format!("/dev/dri/renderD{digits}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ids::JobId;
    use crate::model::EncoderPref;
    use crate::protocol::command::{Command, Quality};
    use crate::state::VaapiCaps;
    use crate::{apply, testkit};

    const X264: &str = "H.264 + AAC 192 kbps \u{b7} software encode (x264)";
    const VA: &str = "H.264 + AAC 192 kbps \u{b7} VA-API on renderD129";

    /// A machine whose VA chain works on renderD129.
    fn va_state() -> State {
        let mut state = testkit::design_state();
        state.runtime.capabilities.vaapi = Some(VaapiCaps {
            device: "/dev/dri/renderD129".into(),
            vendor: 0x8086,
            ffmpeg_h264: true,
            gst_h264_factory: None,
        });
        state
    }

    fn at(time: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(time)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// The design export started at 15:30:12 (S15b's job).
    fn exporting() -> State {
        let state = testkit::design_state();
        let ctx = testkit::ctx("2026-10-14T15:30:12Z", 1);
        let cmd = Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Standard,
            codec: None,
            out: None,
        };
        apply(&state, &ctx, cmd).unwrap().state
    }

    /// D-34: a refused `Ctrl+E` says why (it opened nothing, silently): an
    /// empty timeline, a take recording; a running export always opens it.
    #[test]
    fn the_panel_opens_unless_the_timeline_is_empty_or_recording() {
        use crate::state::{RecordMode, RecordPhase};
        let mut state = testkit::design_state();
        assert_eq!(panel_enabled(&state), Ok(()));
        let clips = std::mem::take(&mut state.project.timeline.clips);
        assert_eq!(
            panel_enabled(&state),
            Err("The timeline is empty".to_owned())
        );
        state.project.timeline.clips = clips;
        state.runtime.record = RecordPhase::Recording {
            take: crate::ids::TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        assert_eq!(panel_enabled(&state), Err("Not while recording".to_owned()));
        let mut running = exporting();
        assert_eq!(panel_enabled(&running), Ok(()));
        running.project.timeline.clips.clear();
        assert_eq!(panel_enabled(&running), Ok(()), "it shows the export");
        let notice = crate::view::notices::unavailable_notice(
            "export",
            "The timeline is empty",
            &testkit::design_state(),
        )
        .expect("a toast");
        assert_eq!(notice.title, "The timeline is empty");
    }

    /// The caption names the encoder the export will use: VA-API only where
    /// `export_vaapi` (the export's own rule) picks it, so x264 preferred by
    /// the project or the config, or a pin on another node, read x264
    /// although the machine's VA chain works.
    #[test]
    fn the_mp4_caption_names_the_encoder_the_export_uses() {
        let config = UserConfig::default();
        let mp4 =
            |state: &State, config: &UserConfig| caption(ExportFormat::Mp4, None, state, config);
        let mut state = va_state();
        assert_eq!(mp4(&state, &config), VA);
        assert_eq!(
            caption(ExportFormat::Mp4, Some(VideoCodec::H264), &state, &config),
            VA
        );

        // The project prefers x264 (`qrec project set --encoder x264`).
        state.project.recording.encoder = EncoderPref::X264;
        assert_eq!(mp4(&state, &config), X264);
        state.project.recording.encoder = EncoderPref::Auto;

        // config.toml prefers x264.
        let mut prefers_x264 = config.clone();
        prefers_x264.encoder.preference = EncoderPref::X264;
        assert_eq!(mp4(&state, &prefers_x264), X264);

        // A pin on a node other than the verified one.
        let mut pinned = config.clone();
        pinned.encoder.vaapi_device = Some("/dev/dri/renderD128".into());
        assert_eq!(mp4(&state, &pinned), X264);
        state.project.recording.vaapi_device = Some("/dev/dri/renderD128".into());
        assert_eq!(mp4(&state, &config), X264);
        state.project.recording.vaapi_device = Some("/dev/dri/renderD129".into());
        assert_eq!(mp4(&state, &pinned), VA, "the project's pin wins");

        // No VA, or a chain that doesn't encode.
        assert_eq!(mp4(&testkit::design_state(), &config), X264);
        let mut broken = va_state();
        broken
            .runtime
            .capabilities
            .vaapi
            .as_mut()
            .unwrap()
            .ffmpeg_h264 = false;
        assert_eq!(mp4(&broken, &config), X264);
    }

    #[test]
    fn webm_captions_and_the_av1_switch() {
        let state = va_state();
        let config = UserConfig::default();
        let vp9 = "VP9 + Opus 128 kbps \u{b7} software encode";
        assert_eq!(caption(ExportFormat::Webm, None, &state, &config), vp9);
        assert_eq!(
            caption(ExportFormat::Webm, Some(VideoCodec::Vp9), &state, &config),
            vp9
        );
        assert_eq!(
            caption(ExportFormat::Webm, Some(VideoCodec::Av1), &state, &config),
            "AV1 + Opus 128 kbps \u{b7} software encode (libsvtav1)"
        );
        let mut caps = state.runtime.capabilities.clone();
        assert_eq!(av1_unavailable(&caps), None, "the testkit's ffmpeg has it");
        caps.ffmpeg.as_mut().unwrap().encoders.remove("libsvtav1");
        assert_eq!(
            av1_unavailable(&caps).as_deref(),
            Some("This ffmpeg has no libsvtav1")
        );
        caps.ffmpeg = None;
        assert!(av1_unavailable(&caps).is_some());
    }

    /// S15a: the auto name at the local now, and the summary.
    #[test]
    fn the_setup_reads_like_s15a() {
        let state = testkit::design_state();
        let now = at("2026-10-14T13:30:12Z");
        assert_eq!(
            output_preview(&state, ExportFormat::Mp4, now, 2 * 3600),
            "exports/rust-traits-talk-20261014-153012.mp4"
        );
        assert_eq!(
            output_preview(&state, ExportFormat::Webm, now, 2 * 3600),
            "exports/rust-traits-talk-20261014-153012.webm"
        );
        assert_eq!(
            summary(&state),
            "1920\u{d7}1080 \u{b7} 30 fps \u{b7} 00:04:06:11 \u{b7} 10 clips"
        );
        let runs = summary_runs(&state);
        assert_eq!(
            runs.iter()
                .filter(|r| r.mono)
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>(),
            ["1920\u{d7}1080", "00:04:06:11"]
        );
    }

    /// S15b's readout: 3,449 of 7,391 frames at 15:30:50 of an export
    /// started at 15:30:12, five clips rendered whole. The design timeline
    /// renders in 9 units (c2d0 and c2d1 are one run of take 5) and the
    /// label counts its 10 clips.
    #[test]
    fn the_running_panel_reads_like_s15b() {
        let mut state = exporting();
        let job = *state.runtime.jobs.keys().next().unwrap();
        for (segment, frames_done) in [
            (0, 410),
            (1, 457),
            (2, 268),
            (3, 307),
            (4, 238),
            (5, 100),
            (6, 50),
            (7, 1000),
            (8, 619),
        ] {
            let event = crate::protocol::event::Event::ExportProgress {
                job,
                segment,
                frames_done,
            };
            let ctx = testkit::ctx("2026-10-14T15:30:12Z", 1);
            state = crate::on_event(&state, &ctx, event).state;
        }
        let panel = running(&state.runtime.jobs[&job], at("2026-10-14T15:30:50Z"), 0).unwrap();
        insta::assert_json_snapshot!("running_42", panel);
        assert_eq!(panel.percent, "42%");
        assert_eq!(panel.label, "Rendering clip 6/10");
        assert_eq!(panel.frames, "3,449 / 7,391 frames");
        assert_eq!(panel.times, "Elapsed 00:00:38 \u{b7} About 00:00:52 left");
        // Nothing done yet: no ETA.
        let fresh = exporting();
        let job = fresh.runtime.jobs.values().next().unwrap();
        let early = running(job, at("2026-10-14T15:30:13Z"), 0).unwrap();
        assert_eq!(early.times, "Elapsed 00:00:01");
        assert_eq!(early.percent, "0%");
    }

    /// S16: the done details of the design export.
    #[test]
    fn the_done_panel_reads_like_s16() {
        let state = exporting();
        let done = done(
            &state,
            "exports/rust-traits-talk-20261014-153012.mp4",
            88_400_000,
            91,
        );
        assert_eq!(
            done.details,
            "88.4 MB \u{b7} 1920\u{d7}1080 \u{b7} 30 fps \u{b7} 00:04:06:11 \u{b7} took 00:01:31"
        );
        insta::assert_json_snapshot!("done", done);
    }

    fn tail() -> Vec<String> {
        [
            "ffmpeg version 7.1 Copyright (c) 2000-2024 the FFmpeg developers",
            "Input #0, matroska,webm, from 'takes/take-0003/L1-screen-DP-4.mkv':",
            "[AVHWDeviceContext @ 0x5581c3e0a2c0] Failed to initialise VAAPI connection: -1 (unknown libva error).",
            "Device creation failed: -5.",
            "[vf#0:0 @ 0x5581c3dfa840] Error initializing filter 'hwupload' with args 'derive_device=vaapi'",
            "Error opening output files: Input/output error",
        ]
        .map(String::from)
        .to_vec()
    }

    /// `exporting()`'s job with `seg_done`.
    fn job_at(state: &State, seg_done: [u64; 9]) -> Job {
        let mut job = state.runtime.jobs.values().next().unwrap().clone();
        if let JobKind::Export { seg_done: done, .. } = &mut job.kind {
            *done = seg_done.to_vec();
        }
        job
    }

    /// S17: the third render unit (c1b2 of take 3) failed with a VA-API
    /// stderr; the lead names it from the error's segment among the
    /// timeline's 10 clips (units render in parallel: others got further),
    /// and the bar stops at the job's last percent (3,039 frames: 37 %).
    #[test]
    fn the_failed_panel_reads_like_s17() {
        let state = exporting();
        let tail = tail();
        let log = ".qrec/logs/export-20261014-153012.log";
        let job = job_at(&state, [410, 457, 0, 307, 238, 249, 155, 1223, 0]);
        let f = FailedExport {
            started: &state,
            job: Some(&job),
            message: "export failed: segment 3/9 (take-0003, frames 0\u{2013}268): ffmpeg exited with code 1",
            stderr_tail: &tail,
            log: Some(log),
            folder: "exports/",
        };
        let panel = failed(&f);
        assert_eq!(
            panel.lead,
            "ffmpeg stopped while rendering clip 3/10 (take 3, c1b2). Nothing was written to exports/."
        );
        assert_eq!(
            panel.hint.as_deref(),
            Some("VA-API couldn't start. Try Settings \u{203a} Recording \u{203a} Encoder: x264."),
            "no node in the tail, none verified"
        );
        assert_eq!((panel.at.as_str(), panel.fraction), ("Failed at 37%", 37));
        insta::assert_json_snapshot!("failed", panel);
        // The node from the tail, else from the machine's VA facts.
        let va = va_state();
        assert_eq!(
            hint(&tail, &va.runtime.capabilities).as_deref(),
            Some(
                "VA-API couldn't start on /dev/dri/renderD129. Try Settings \u{203a} Recording \u{203a} Encoder: x264."
            )
        );
        let mut named = tail.clone();
        named.push("Failed to open /dev/dri/renderD130 for vaapi".into());
        assert!(
            hint(&named, &va.runtime.capabilities)
                .unwrap()
                .contains("/dev/dri/renderD130")
        );
        // No segment in the message: the first unit the job hadn't
        // finished, c2d0 and c2d1's, 1,500 frames in: past c2d0, so c2d1,
        // the timeline's 9th clip.
        let further = job_at(&state, [410, 457, 268, 307, 238, 249, 155, 1500, 0]);
        let unnamed = FailedExport {
            job: Some(&further),
            message: "export failed: segment: ffmpeg exited with code 1",
            ..f
        };
        assert_eq!(
            failed_lead(&unnamed),
            "ffmpeg stopped while rendering clip 9/10 (take 5, c2d1). Nothing was written to exports/."
        );
        // A named unit with its frames: the clip they reach into.
        let in_c2d1 = FailedExport {
            job: Some(&further),
            message: "export failed: segment 8/9 (take-0005, frames 12\u{2013}2214): ffmpeg exited with code 1",
            ..f
        };
        assert!(failed_lead(&in_c2d1).contains("clip 9/10 (take 5, c2d1)"));
        // No progress seen: the unit's first clip, at 0 %.
        let blind = failed(&FailedExport { job: None, ..f });
        assert!(
            blind.lead.contains("clip 3/10 (take 3, c1b2)"),
            "{}",
            blind.lead
        );
        assert_eq!((blind.at.as_str(), blind.fraction), ("Failed at 0%", 0));
        let stitch = FailedExport {
            message: "export failed: stitch: ffmpeg exited with code 1",
            ..f
        };
        assert_eq!(
            failed_lead(&stitch),
            "ffmpeg stopped while joining the clips. Nothing was written to exports/."
        );
        let other = FailedExport {
            message: "export failed: the folder /home/me/gone doesn't exist",
            folder: "~/gone/",
            ..f
        };
        assert_eq!(
            failed_lead(&other),
            "The folder /home/me/gone doesn't exist. Nothing was written to ~/gone/.",
            "not ffmpeg's failure: the label doesn't name a clip"
        );
        // At most 30 lines of stderr.
        let long: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
        let many = failed(&FailedExport {
            stderr_tail: &long,
            ..f
        });
        assert_eq!(many.stderr_tail.len(), 30);
        assert_eq!(many.stderr_tail[0], "line 11");
        let _ = json!(JobId(1));
    }

    fn failed_lead(f: &FailedExport<'_>) -> String {
        failed(f).lead
    }

    #[test]
    fn hints_for_a_full_disk_and_a_missing_encoder() {
        let caps = Capabilities::default();
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            hint(
                &lines(&["av_interleaved_write_frame(): No space left on device"]),
                &caps
            )
            .as_deref(),
            Some("The disk is full. Free some space and try again.")
        );
        assert_eq!(
            hint(&lines(&["Unknown encoder 'libsvtav1'"]), &caps).as_deref(),
            Some("This ffmpeg has no libsvtav1. Run qrec doctor.")
        );
        assert_eq!(hint(&lines(&["Conversion failed!"]), &caps), None);
    }
}
