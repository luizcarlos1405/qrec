//! Toasts the GUI makes itself (G§4.9, G§3.7): for a command that failed
//! at once and for a key whose action is unavailable (D-34). Owner: WP0a.
//!
//! Core toasts (`Effect::Notify`) reach the window as they are; a late
//! error's toast is the core's own (H-7), so these are only for immediate
//! replies and refused keys. [`Notice`] is what the toast host shows; a
//! core [`Toast`] converts into one.

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, ErrorCode};
use crate::protocol::command::Command;
use crate::protocol::toast::{Toast, ToastAction, ToastLevel};
use crate::state::{JobKind, State};
use crate::view::format::capitalized;
use crate::view::settings::SettingsPage;

/// A toast made by the GUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub level: ToastLevel,
    pub title: String,
    pub body: Option<String>,
    /// At most two are shown (DESIGN).
    pub actions: Vec<NoticeAction>,
}

/// A toast button.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoticeAction {
    /// A core toast's action, with its label.
    Core(ToastAction),
    /// Opens the export panel.
    ShowExport,
    /// Opens the bin.
    OpenBin,
    OpenSettings(SettingsPage),
    /// Retries a refused or dead live source (the output or device).
    RetrySource(String),
}

impl NoticeAction {
    /// The button's label.
    pub fn label(&self) -> &str {
        match self {
            NoticeAction::Core(action) => &action.label,
            NoticeAction::ShowExport => "Show export",
            NoticeAction::OpenBin => "Open bin",
            NoticeAction::OpenSettings(_) => "Open settings",
            NoticeAction::RetrySource(_) => "Try again",
        }
    }
}

impl Notice {
    pub fn new(level: ToastLevel, title: impl Into<String>) -> Self {
        Self {
            level,
            title: title.into(),
            body: None,
            actions: Vec::new(),
        }
    }

    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn with_action(mut self, action: NoticeAction) -> Self {
        self.actions.push(action);
        self
    }
}

impl From<Toast> for Notice {
    /// A core toast, its actions as [`NoticeAction::Core`].
    fn from(toast: Toast) -> Self {
        Self {
            level: toast.level,
            title: toast.title,
            body: toast.body,
            actions: toast.actions.into_iter().map(NoticeAction::Core).collect(),
        }
    }
}

/// The toast for a command `cmd` that failed at once with `err`: an error
/// for `io`, `internal`, `capture_failed` and `missing_dependency`, a
/// warning otherwise; the title is the message with its first letter
/// uppercased and the body the hint. `None` for `confirmation_required`,
/// which opens the confirmation modal instead.
pub fn error_notice(err: &CoreError, cmd: &Command, state: &State) -> Option<Notice> {
    let _ = (cmd, state);
    let level = match err.code {
        ErrorCode::ConfirmationRequired => return None,
        ErrorCode::Io
        | ErrorCode::Internal
        | ErrorCode::CaptureFailed
        | ErrorCode::MissingDependency => ToastLevel::Error,
        ErrorCode::Usage
        | ErrorCode::NotProject
        | ErrorCode::Busy
        | ErrorCode::NotFound
        | ErrorCode::Invalid
        | ErrorCode::Unavailable
        | ErrorCode::Interrupted => ToastLevel::Warning,
    };
    Some(Notice {
        level,
        title: capitalized(&err.message),
        body: err.hint.clone(),
        actions: Vec::new(),
    })
}

/// The toast for a key whose binding `binding_id` is unavailable (D-34),
/// `reason` being `keymap::action_for`'s: none for "Nothing to cancel"
/// (`Esc` with nothing to cancel is silent); recording keys during an
/// export say why and offer the export; `d` with the newest take gone says
/// so (S25 #7); anything else is an info toast titled with the reason.
pub fn unavailable_notice(binding_id: &str, reason: &str, state: &State) -> Option<Notice> {
    if reason == "Nothing to cancel" {
        return None;
    }
    let exporting = state
        .runtime
        .jobs
        .values()
        .any(|job| matches!(job.kind, JobKind::Export { .. }));
    let notice = match binding_id {
        "record.toggle" | "record.over" if exporting && state.runtime.record.is_idle() => {
            Notice::new(ToastLevel::Warning, "Can't record while exporting")
                .with_body("Recording works again once the export finishes or you cancel it.")
                .with_action(NoticeAction::ShowExport)
        }
        // S25 #7 (D-21): `d` when the newest take's clips are gone.
        "take.discard-last" if state.runtime.record.is_idle() => match gone_take(state) {
            Some(n) => Notice::new(ToastLevel::Info, "Nothing to discard")
                .with_body(format!("Take {n} has no clips on the timeline.")),
            None => Notice::new(ToastLevel::Info, reason),
        },
        _ => Notice::new(ToastLevel::Info, reason),
    };
    Some(notice)
}

/// The newest placed take (the one `d` discards, A§5.5) when none of its
/// clips is on the timeline.
fn gone_take(state: &State) -> Option<u32> {
    let (id, _) = state
        .project
        .takes
        .iter()
        .filter(|(_, t)| t.placed)
        .max_by_key(|(id, _)| id.number())?;
    (!state.project.timeline.clips.iter().any(|c| &c.take == id)).then(|| id.number())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply;
    use crate::ids::{ClipId, JobId};
    use crate::keymap::{Action, action_for};
    use crate::protocol::command::{ExportFormat, Quality, SelectOp};
    use crate::protocol::toast::UiAction;
    use crate::state::Job;
    use crate::testkit;

    /// The design state with an export running (S15b).
    fn exporting() -> State {
        let mut state = testkit::design_state();
        state.runtime.jobs.insert(
            JobId(1),
            Job {
                id: JobId(1),
                kind: JobKind::Export {
                    out: "exports/rust-traits-talk-20261014-153012.mp4".into(),
                    files: Default::default(),
                    total_frames: 7391,
                    seg_frames: vec![7391],
                    seg_done: vec![3449],
                    seg_clips: Vec::new(),
                    stitch_total_ns: 1,
                    stitch_done_ns: 0,
                    stitching: false,
                },
                req: None,
                started: state.project.created_at,
            },
        );
        state
    }

    fn reason(id: &str, state: &State) -> String {
        match action_for(id, state) {
            Some(Action::Unavailable(why)) => why,
            other => panic!("{id} is available: {other:?}"),
        }
    }

    #[test]
    fn notices_snapshot() {
        let state = testkit::design_state();
        let ctx = testkit::ctx(testkit::CREATED_AT, 1);
        let mut notices: Vec<(String, Option<Notice>)> = Vec::new();
        // Immediate errors, from real replies of the core.
        let fail = |state: &State, cmd: Command| {
            let err = apply(state, &ctx, cmd.clone()).unwrap_err();
            (cmd, err)
        };
        let mut errors = vec![
            fail(
                &state,
                Command::Select {
                    op: SelectOp::Set(vec![ClipId::from_raw("c404")]),
                },
            ),
            fail(
                &state,
                Command::ClipSplit {
                    clip: None,
                    at: crate::time::TimeArg::frames(410),
                    timeline: true,
                },
            ),
            fail(
                &exporting(),
                Command::Export {
                    format: ExportFormat::Mp4,
                    quality: Quality::Standard,
                    codec: None,
                    out: None,
                },
            ),
            fail(
                &state,
                Command::TakePurge {
                    takes: vec![crate::ids::TakeId::numbered(2)],
                },
            ),
        ];
        errors.push((
            Command::RecordToggle { wait: false },
            CoreError::capture_failed("capture failed: L1 (GIGA-BYTE GS27QA): access denied")
                .with_hint("check L1 (GIGA-BYTE GS27QA); qrec doctor lists capture problems"),
        ));
        errors.push((
            Command::Export {
                format: ExportFormat::Mp4,
                quality: Quality::Standard,
                codec: None,
                out: None,
            },
            CoreError::io("export failed: ffmpeg exited with status 1"),
        ));
        for (cmd, err) in &errors {
            notices.push((
                format!("error_notice({}, {:?})", cmd.name(), err.code),
                error_notice(err, cmd, &state),
            ));
        }
        // Refused keys (D-34), with the reasons `action_for` gives.
        let mut live = testkit::design_state();
        live.ui.selection.clear();
        let mut empty = live.clone();
        empty.project.timeline.clips.clear();
        let mut cases: Vec<(&str, String, State)> = vec![
            ("cancel", reason("cancel", &live), live.clone()),
            ("play.toggle", reason("play.toggle", &empty), empty),
            (
                "record.toggle",
                reason("record.toggle", &exporting()),
                exporting(),
            ),
            (
                "record.over",
                "Select the clips to record over".into(),
                exporting(),
            ),
            ("history.undo", reason("history.undo", &live), live.clone()),
        ];
        let mut program = live;
        program.ui.canvas_mode = crate::state::CanvasMode::Program;
        program.ui.selected_layer = Some(crate::ids::LayerId::numbered(2));
        cases.push((
            "layer.nudge-left",
            reason("layer.nudge-left", &program),
            program,
        ));
        for (id, why, state) in &cases {
            notices.push((
                format!("unavailable_notice({id}, {why:?})"),
                unavailable_notice(id, why, state),
            ));
        }
        insta::assert_json_snapshot!("notices", notices);
    }

    #[test]
    fn toast_levels_and_titles() {
        let state = testkit::design_state();
        let cmd = Command::Undo { steps: 1 };
        let n = error_notice(&CoreError::internal("oops"), &cmd, &state).unwrap();
        assert_eq!(
            (n.level, n.title.as_str(), n.body),
            (ToastLevel::Error, "Oops", None)
        );
        let n = error_notice(
            &CoreError::not_found("no clip c9 on the timeline").with_hint("list them"),
            &cmd,
            &state,
        )
        .unwrap();
        assert_eq!(n.level, ToastLevel::Warning);
        assert_eq!(n.title, "No clip c9 on the timeline");
        assert_eq!(n.body.as_deref(), Some("list them"));
        assert_eq!(
            error_notice(&CoreError::confirmation_required("purge"), &cmd, &state),
            None,
            "the modal asks instead"
        );
        assert_eq!(capitalized("éclair"), "Éclair");
        assert_eq!(capitalized(""), "");
        // "Nothing to cancel" is silent.
        assert_eq!(
            unavailable_notice("cancel", "Nothing to cancel", &state),
            None
        );
        // Recording keys while exporting offer the export (S25 #9).
        let n = unavailable_notice(
            "record.toggle",
            "Wait for the export to finish",
            &exporting(),
        )
        .unwrap();
        assert_eq!(n.title, "Can't record while exporting");
        assert_eq!(n.actions, [NoticeAction::ShowExport]);
        assert_eq!(n.actions[0].label(), "Show export");
    }

    /// S25 #7 (GUI D-21): `d` once the newest take's clips are gone says
    /// which take; other refusals of `d` keep `action_for`'s words.
    #[test]
    fn nothing_to_discard_names_the_take() {
        let mut state = testkit::design_state();
        // Take 8 is the newest placed take; c3e9 is its only clip.
        state
            .project
            .timeline
            .clips
            .retain(|c| c.id.as_str() != "c3e9");
        let why = reason("take.discard-last", &state);
        assert_eq!(why, "Take 8 is already in the bin");
        let n = unavailable_notice("take.discard-last", &why, &state).unwrap();
        assert_eq!(
            (n.level, n.title.as_str(), n.body.as_deref()),
            (
                ToastLevel::Info,
                "Nothing to discard",
                Some("Take 8 has no clips on the timeline.")
            )
        );
        assert!(n.actions.is_empty());
        let empty = testkit::state(testkit::ProjectBuilder::new().build());
        let why = reason("take.discard-last", &empty);
        let n = unavailable_notice("take.discard-last", &why, &empty).unwrap();
        assert_eq!((n.title.as_str(), n.body), ("No take to discard yet", None));
    }

    #[test]
    fn core_toasts_convert() {
        let toast = Toast::new(ToastLevel::Success, "Frame saved")
            .with_body("frames/x.png")
            .with_action("Copy image", UiAction::CopyImage("/p/frames/x.png".into()))
            .with_action(
                "Open folder",
                UiAction::RevealPath("/p/frames/x.png".into()),
            );
        let notice = Notice::from(toast);
        assert_eq!(notice.title, "Frame saved");
        let labels: Vec<&str> = notice.actions.iter().map(NoticeAction::label).collect();
        assert_eq!(labels, ["Copy image", "Open folder"]);
        assert_eq!(NoticeAction::OpenBin.label(), "Open bin");
        assert_eq!(
            NoticeAction::RetrySource("DP-4".into()).label(),
            "Try again"
        );
        assert_eq!(
            NoticeAction::OpenSettings(SettingsPage::Project).label(),
            "Open settings"
        );
    }
}
