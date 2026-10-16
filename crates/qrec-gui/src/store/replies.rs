//! The default reply policy (G§3.7): what the window does with the answer
//! to a command it sent, unless the widget that sent it handles the answer
//! itself (`Store::dispatch_with`).
//!
//! - `Ok` → nothing: the snapshot before the reply already shows the change.
//! - An immediate `confirmation_required` → the confirmation modal
//!   (`Overlay::Confirm`); confirming sends the command again with
//!   `confirmed: true`. A confirmation isn't a lock: the core checks again.
//! - Another immediate error → `view::notices::error_notice`: a toast.
//! - A late error → the log only: the core's `Notify` already toasted it
//!   (H-7; `qrec-core/tests/props_late_errors.rs` holds every `on_event`
//!   path to that).

use qrec_core::view::notices::{Notice, error_notice};
use qrec_core::{Command, CoreError, ErrorCode, State};

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
        Ok(_) => return ReplyAction::Nothing,
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

#[cfg(test)]
mod tests {
    use qrec_core::protocol::ToastLevel;
    use qrec_core::protocol::command::SelectOp;
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
}
