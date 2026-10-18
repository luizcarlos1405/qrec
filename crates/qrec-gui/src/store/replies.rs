//! The default reply policy (G§3.7): what the window does with the answer
//! to a command it sent, unless the widget that sent it handles the answer
//! itself (`Store::dispatch_with`).
//!
//! - `Ok` → nothing: the snapshot before the reply already shows the change;
//!   but an autocut run's cut is S25 #6's toast ("Autocut take 8: removed 4
//!   silences (6.2 s)" with Undo), however the window asked for it: the
//!   autocut page's Apply, `c` (`autocut.run`), the toolbar's Autocut or
//!   the palette's row (the core toasts a run only when nobody waits for its
//!   reply, and the window always does).
//! - An immediate `confirmation_required` → the confirmation modal
//!   (`Overlay::Confirm`); confirming sends the command again with
//!   `confirmed: true`. A confirmation isn't a lock: the core checks again.
//! - Another immediate error → `view::notices::error_notice`: a toast.
//! - A late error → the log only: the core's `Notify` already toasted it
//!   (H-7; `qrec-core/tests/props_late_errors.rs` holds every `on_event`
//!   path to that).

use qrec_core::protocol::{ToastAction, ToastLevel, UiAction};
use qrec_core::view::format;
use qrec_core::view::notices::{Notice, NoticeAction, error_notice};
use qrec_core::{CmdResult, Command, CoreError, ErrorCode, State};
use serde_json::Value;

use super::Reply;

/// What the default policy does with a reply.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplyAction {
    Nothing,
    /// Open `Overlay::Confirm { cmd, error }`.
    Confirm {
        cmd: Command,
        error: CoreError,
    },
    /// Show this toast.
    Toast(Notice),
    /// Log this line at warning level.
    Log(String),
}

/// The default policy for the reply to `cmd` (sent while `state` was
/// current).
pub fn default_action(cmd: &Command, reply: &Reply, state: &State) -> ReplyAction {
    let err = match &reply.result {
        Ok(value) => {
            return match cmd {
                Command::Autocut { dry_run: false, .. } => {
                    autocut_done(value).map_or(ReplyAction::Nothing, ReplyAction::Toast)
                }
                _ => ReplyAction::Nothing,
            };
        }
        Err(err) => err,
    };
    if reply.late {
        return ReplyAction::Log(format!(
            "{} failed later: {err} (the core toasted it)",
            cmd.name()
        ));
    }
    if err.code == ErrorCode::ConfirmationRequired {
        return ReplyAction::Confirm {
            cmd: cmd.clone(),
            error: err.clone(),
        };
    }
    match error_notice(err, cmd, state) {
        Some(notice) => ReplyAction::Toast(notice),
        None => ReplyAction::Log(format!("{} failed: {err}", cmd.name())),
    }
}

/// S25 #6: an autocut run that cut (a history entry): its label as the
/// title, success, Undo; one that cut nothing says so, info, no Undo. Its
/// reply comes when the run ends, so it is a late one too.
pub fn autocut_done(value: &Value) -> Option<Notice> {
    let result = serde_json::from_value::<CmdResult>(value.clone()).ok()?;
    let level = if result.seq.is_some() {
        ToastLevel::Success
    } else {
        ToastLevel::Info
    };
    let mut notice = Notice::new(level, format::capitalized(&result.message));
    if result.seq.is_some() {
        notice = notice.with_action(NoticeAction::Core(ToastAction {
            label: "Undo".to_owned(),
            action: UiAction::Dispatch(Box::new(Command::Undo { steps: 1 })),
        }));
    }
    Some(notice)
}

#[cfg(test)]
mod tests {
    use qrec_core::protocol::command::{AutocutOverrides, ClipScope, SelectOp};
    use qrec_core::{TakeId, testkit};
    use serde_json::json;

    use super::*;

    fn reply(result: Result<serde_json::Value, CoreError>, late: bool) -> Reply {
        Reply { result, late }
    }

    #[test]
    fn the_default_policy() {
        let state = testkit::design_state();
        let purge = Command::TakePurge {
            takes: vec![TakeId::numbered(2), TakeId::numbered(4)],
        };
        assert_eq!(
            default_action(&purge, &reply(Ok(json!({"seq": 3})), false), &state),
            ReplyAction::Nothing
        );
        let confirm = CoreError::new(ErrorCode::ConfirmationRequired, "purge 2 takes?");
        assert_eq!(
            default_action(&purge, &reply(Err(confirm.clone()), false), &state),
            ReplyAction::Confirm {
                cmd: purge.clone(),
                error: confirm
            }
        );
        let select = Command::Select { op: SelectOp::All };
        match default_action(
            &select,
            &reply(
                Err(CoreError::invalid("not while recording").with_hint("stop first")),
                false,
            ),
            &state,
        ) {
            ReplyAction::Toast(notice) => {
                assert_eq!(notice.level, ToastLevel::Warning);
                assert_eq!(notice.title, "Not while recording");
                assert_eq!(notice.body.as_deref(), Some("stop first"));
            }
            other => panic!("{other:?}"),
        }
        match default_action(
            &select,
            &reply(Err(CoreError::io("disk full")), false),
            &state,
        ) {
            ReplyAction::Toast(notice) => assert_eq!(notice.level, ToastLevel::Error),
            other => panic!("{other:?}"),
        }
        // Late errors were toasted by the core.
        assert!(matches!(
            default_action(&select, &reply(Err(CoreError::io("late")), true), &state),
            ReplyAction::Log(_)
        ));
    }

    /// S25 #6 for every way the window runs autocut (the page's Apply, `c`,
    /// the toolbar, the palette): the run's late reply is the toast, with
    /// Undo when it cut; a dry run or a run that cut nothing toasts no Undo.
    #[test]
    fn an_autocut_runs_reply_is_s25s_toast() {
        let state = testkit::design_state();
        let run = |dry_run| Command::Autocut {
            scope: ClipScope::All,
            overrides: AutocutOverrides::default(),
            dry_run,
        };
        let cut = json!({
            "seq": 7,
            "created": ["c9a1"],
            "message": "Autocut take 8: removed 4 silences (6.2 s)",
            "result": null,
        });
        for late in [true, false] {
            let ReplyAction::Toast(notice) =
                default_action(&run(false), &reply(Ok(cut.clone()), late), &state)
            else {
                panic!("a toast");
            };
            assert_eq!(notice.level, ToastLevel::Success);
            assert_eq!(notice.title, "Autocut take 8: removed 4 silences (6.2 s)");
            assert_eq!(notice.body, None);
            let [NoticeAction::Core(undo)] = notice.actions.as_slice() else {
                panic!("{:?}", notice.actions);
            };
            assert_eq!(undo.label, "Undo");
            assert_eq!(
                undo.action,
                UiAction::Dispatch(Box::new(Command::Undo { steps: 1 }))
            );
        }
        let nothing =
            json!({"seq": null, "message": "nothing to cut: no silences", "result": null});
        let ReplyAction::Toast(notice) =
            default_action(&run(false), &reply(Ok(nothing), true), &state)
        else {
            panic!("a toast");
        };
        assert_eq!(notice.level, ToastLevel::Info);
        assert_eq!(notice.title, "Nothing to cut: no silences");
        assert!(notice.actions.is_empty());
        // The page's previews (dry runs) are its own business.
        assert_eq!(
            default_action(&run(true), &reply(Ok(cut), true), &state),
            ReplyAction::Nothing
        );
    }
}
