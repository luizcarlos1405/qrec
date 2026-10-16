//! The export panel's GTK-free part: the setup draft, what the panel shows
//! (its mode), and what its own export's answer does (G§4.8, D-24, D-49).
//!
//! - The panel shows `Running` whenever an export runs, from any host (one
//!   runs at a time: the core refuses a second, A§5.9), and while its own
//!   export's answer is still on its way after the job left the table;
//!   `Done` or `Failed` for its own export's outcome until it closes;
//!   otherwise `Setup`.
//! - Its own export (sent with `Store::dispatch_with`, `confirmed: false`):
//!   an immediate `confirmation_required` opens the Replace modal (whose
//!   confirmation re-dispatches it, `Store::dispatch_confirmed`, D-24); a
//!   refused hand-picked path shows the core's message under the Output
//!   field; other immediate errors toast; the late answer is `Done` or
//!   `Failed`, and a cancel or anything else closes the panel (the core
//!   toasted it, H-7).

use qrec_core::protocol::command::{ExportFormat, Quality, VideoCodec};
use qrec_core::view::export::{Done, Failed};
use qrec_core::view::format;
use qrec_core::{Command, CoreError, ErrorCode};
use serde_json::Value;

use crate::store::Reply;
use crate::store::local::ExportPanelMode;

/// The setup step's choices (S15a, S15c), kept while the panel is closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub format: ExportFormat,
    pub quality: Quality,
    /// WebM with AV1 instead of VP9.
    pub av1: bool,
    /// A path picked with Change… (absolute); `None`: the auto name.
    pub chosen: Option<String>,
    /// The core's message about `chosen` (C29 invalid, D-24).
    pub error: Option<String>,
    /// Export was pressed and its job hasn't shown yet.
    pub starting: bool,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            format: ExportFormat::Mp4,
            quality: Quality::Standard,
            av1: false,
            chosen: None,
            error: None,
            starting: false,
        }
    }
}

impl Draft {
    /// The codec the draft asks for: AV1 for WebM with the switch on, else
    /// the format's default.
    pub fn codec(&self) -> Option<VideoCodec> {
        (self.format == ExportFormat::Webm && self.av1).then_some(VideoCodec::Av1)
    }

    /// The `Export` command of the draft.
    pub fn command(&self) -> Command {
        Command::Export {
            format: self.format,
            quality: self.quality,
            codec: self.codec(),
            out: self.chosen.clone(),
        }
    }
}

/// What the panel shows of its own export once it ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// S16. `file` is the output's path for Open file, Open folder and Copy
    /// path (absolute, as the shell wrote it).
    Done { view: Done, file: String },
    /// S17. `retry` is the command Try again sends; `copy` what Copy error
    /// copies; `percent` where the bar stopped.
    Failed {
        view: Failed,
        retry: Box<Command>,
        copy: String,
        percent: u8,
    },
}

/// The panel's own state (`LocalUi::export_view`, G§3.6): the draft and its
/// own export's outcome (until the panel closes).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportPanelState {
    pub draft: Draft,
    pub outcome: Option<Outcome>,
}

/// What the open panel shows (D-49): `exporting` an export runs; `waiting`
/// its own export left the table and its answer hasn't come.
pub fn mode(exporting: bool, waiting: bool, outcome: Option<&Outcome>) -> ExportPanelMode {
    if exporting || waiting {
        return ExportPanelMode::Running;
    }
    match outcome {
        Some(Outcome::Done { .. }) => ExportPanelMode::Done,
        Some(Outcome::Failed { .. }) => ExportPanelMode::Failed,
        None => ExportPanelMode::Setup,
    }
}

/// What the answer to the panel's own export does.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// Written: `path` (absolute), `bytes`.
    Done { path: String, bytes: u64 },
    /// ffmpeg failed: the error's message, stderr tail and log.
    Failed {
        message: String,
        stderr_tail: Vec<String>,
        log: Option<String>,
    },
    /// The hand-picked path exists: the Replace modal (D-24).
    Confirm(CoreError),
    /// The core refused the hand-picked path: its message under the field.
    Inline(String),
    /// Another immediate error: the default policy's toast.
    Toast,
    /// Cancelled, kept aside, or another late error the core toasted: the
    /// panel closes.
    Close,
}

/// Sorts the answer to `cmd`, the panel's own export.
pub fn answer(cmd: &Command, reply: &Reply) -> Answer {
    let err = match &reply.result {
        Ok(value) => {
            let result = &value["result"];
            return Answer::Done {
                path: result["path"].as_str().unwrap_or_default().to_owned(),
                bytes: result["bytes"].as_u64().unwrap_or(0),
            };
        }
        Err(err) => err,
    };
    if reply.late {
        if err.message.starts_with("export failed") {
            let details = err.details.as_ref();
            let tail = details
                .and_then(|d| d.get("stderrTail"))
                .and_then(Value::as_array)
                .map(|lines| {
                    lines
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let log = details
                .and_then(|d| d.get("log"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            return Answer::Failed {
                message: err.message.clone(),
                stderr_tail: tail,
                log,
            };
        }
        return Answer::Close;
    }
    let chosen = matches!(cmd, Command::Export { out: Some(_), .. });
    match err.code {
        ErrorCode::ConfirmationRequired => Answer::Confirm(err.clone()),
        ErrorCode::Invalid | ErrorCode::Usage if chosen => {
            Answer::Inline(format::capitalized(&err.message))
        }
        _ => Answer::Toast,
    }
}

/// Where an export writes, as the failed panel names it ("exports/", or
/// the hand-picked file's folder as people read it).
pub fn folder_shown(out: Option<&str>, project_dir: &str, home: Option<&str>) -> String {
    match out {
        None => "exports/".to_owned(),
        Some(path) => {
            let dir = path.rsplit_once('/').map_or("/", |(d, _)| d);
            let dir = if dir.is_empty() { "/" } else { dir };
            let shown = format::display_path(dir, project_dir, home);
            if shown.ends_with('/') {
                shown
            } else {
                format!("{shown}/")
            }
        }
    }
}

/// What Copy error copies: the message, then the stderr tail.
pub fn error_text(message: &str, tail: &[String]) -> String {
    if tail.is_empty() {
        return message.to_owned();
    }
    format!("{message}\n\n{}", tail.join("\n"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn export(out: Option<&str>) -> Command {
        Command::Export {
            format: ExportFormat::Mp4,
            quality: Quality::Standard,
            codec: None,
            out: out.map(str::to_owned),
        }
    }

    fn reply(result: Result<Value, CoreError>, late: bool) -> Reply {
        Reply { result, late }
    }

    #[test]
    fn the_panel_shows_what_runs_then_its_own_outcome() {
        let done = Outcome::Done {
            view: Done::default(),
            file: "/p/exports/a.mp4".into(),
        };
        assert_eq!(mode(false, false, None), ExportPanelMode::Setup);
        assert_eq!(mode(true, false, None), ExportPanelMode::Running);
        assert_eq!(mode(true, false, Some(&done)), ExportPanelMode::Running);
        assert_eq!(mode(false, true, None), ExportPanelMode::Running);
        assert_eq!(mode(false, false, Some(&done)), ExportPanelMode::Done);
        let failed = Outcome::Failed {
            view: Failed::default(),
            retry: Box::new(export(None)),
            copy: String::new(),
            percent: 37,
        };
        assert_eq!(mode(false, false, Some(&failed)), ExportPanelMode::Failed);
    }

    #[test]
    fn the_draft_is_the_command() {
        let mut draft = Draft::default();
        assert_eq!(draft.command(), export(None));
        draft.format = ExportFormat::Webm;
        draft.av1 = true;
        draft.quality = Quality::Draft;
        draft.chosen = Some("/home/me/talk.webm".into());
        assert_eq!(
            draft.command(),
            Command::Export {
                format: ExportFormat::Webm,
                quality: Quality::Draft,
                codec: Some(VideoCodec::Av1),
                out: Some("/home/me/talk.webm".into()),
            }
        );
        draft.format = ExportFormat::Mp4;
        assert_eq!(draft.codec(), None, "AV1 is WebM's only");
    }

    /// D-24: an existing file asks (the Replace modal), a refused path says
    /// why under the field, the late answer is done, failed or closes.
    #[test]
    fn answers_sort_by_kind() {
        let picked = export(Some("/p/takes/x.mp4"));
        let exists = CoreError::confirmation_required("overwrite /home/me/talk.mp4");
        assert_eq!(
            answer(&picked, &reply(Err(exists.clone()), false)),
            Answer::Confirm(exists)
        );
        let forbidden = CoreError::invalid("can't export to /p/takes/x.mp4: it is inside takes/");
        assert_eq!(
            answer(&picked, &reply(Err(forbidden.clone()), false)),
            Answer::Inline("Can't export to /p/takes/x.mp4: it is inside takes/".into())
        );
        // The auto name can't be refused for its path: a toast.
        assert_eq!(
            answer(&export(None), &reply(Err(forbidden), false)),
            Answer::Toast
        );
        assert_eq!(
            answer(
                &picked,
                &reply(Err(CoreError::busy("an export is already running")), false)
            ),
            Answer::Toast
        );
        let ok = json!({"message": "Exported x", "result": {"path": "/p/exports/a.mp4", "bytes": 88, "frames": 90}});
        assert_eq!(
            answer(&export(None), &reply(Ok(ok), true)),
            Answer::Done {
                path: "/p/exports/a.mp4".into(),
                bytes: 88
            }
        );
        let failed = CoreError::io(
            "export failed: segment 3/9 (take-0003, frames 0–268): ffmpeg exited with code 1",
        )
        .with_details(json!({"log": "/p/.qrec/logs/x.log", "stderrTail": ["a", "b"]}));
        assert_eq!(
            answer(&export(None), &reply(Err(failed), true)),
            Answer::Failed {
                message: "export failed: segment 3/9 (take-0003, frames 0–268): ffmpeg exited with code 1".into(),
                stderr_tail: vec!["a".into(), "b".into()],
                log: Some("/p/.qrec/logs/x.log".into()),
            }
        );
        assert_eq!(
            answer(
                &export(None),
                &reply(Err(CoreError::invalid("export cancelled")), true)
            ),
            Answer::Close
        );
    }

    #[test]
    fn folders_and_copied_errors() {
        assert_eq!(folder_shown(None, "/p", Some("/home/me")), "exports/");
        assert_eq!(
            folder_shown(Some("/home/me/Videos/talk.mp4"), "/p", Some("/home/me")),
            "~/Videos/"
        );
        assert_eq!(
            folder_shown(Some("/p/renders/talk.mp4"), "/p", Some("/home/me")),
            "renders/"
        );
        assert_eq!(folder_shown(Some("/talk.mp4"), "/p", None), "/");
        assert_eq!(error_text("boom", &[]), "boom");
        assert_eq!(
            error_text("boom", &["a".into(), "b".into()]),
            "boom\n\na\nb"
        );
    }
}
