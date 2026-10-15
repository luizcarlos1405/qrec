//! Errors returned by the core, with the CLI exit codes from SPEC §14.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What went wrong, in a form the CLI maps to an exit code and `--json`
/// prints as `{"code","message","hint"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoreError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Structured data for confirmations and failures (files and sizes,
    /// history entries lost, affected clips). Emitted as `error.details`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " (hint: {hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for CoreError {}

/// Error categories. Serialized as snake_case strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Bad arguments. Exit 2.
    Usage,
    /// Not a project folder, or a non-empty folder without `qrec.json`. Exit 3.
    NotProject,
    /// A destructive action needs `--yes` or a confirmed modal. Exit 4.
    ConfirmationRequired,
    /// A runtime dependency is missing. Exit 5.
    MissingDependency,
    /// Recording, exporting, or another host holds the project. Exit 6.
    Busy,
    /// A bad id. Exit 7.
    NotFound,
    /// The edit is impossible or the project doesn't validate. Exit 1.
    Invalid,
    /// A capture source failed to start (health check). Exit 1.
    CaptureFailed,
    /// The command needs something that isn't available (e.g. the GUI host). Exit 1.
    Unavailable,
    /// A file or process operation failed. Exit 1.
    Io,
    /// A bug. Exit 1.
    Internal,
    /// The CLI was stopped by Ctrl+C or SIGTERM before it got a reply.
    /// Exit 130 (A§8.6).
    Interrupted,
}

impl ErrorCode {
    pub fn exit_code(self) -> u8 {
        match self {
            ErrorCode::Usage => 2,
            ErrorCode::NotProject => 3,
            ErrorCode::ConfirmationRequired => 4,
            ErrorCode::MissingDependency => 5,
            ErrorCode::Busy => 6,
            ErrorCode::NotFound => 7,
            ErrorCode::Invalid
            | ErrorCode::CaptureFailed
            | ErrorCode::Unavailable
            | ErrorCode::Io
            | ErrorCode::Internal => 1,
            // 128 + SIGINT, like shells report it.
            ErrorCode::Interrupted => 130,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Usage => "usage",
            ErrorCode::NotProject => "not_project",
            ErrorCode::ConfirmationRequired => "confirmation_required",
            ErrorCode::MissingDependency => "missing_dependency",
            ErrorCode::Busy => "busy",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Invalid => "invalid",
            ErrorCode::CaptureFailed => "capture_failed",
            ErrorCode::Unavailable => "unavailable",
            ErrorCode::Io => "io",
            ErrorCode::Internal => "internal",
            ErrorCode::Interrupted => "interrupted",
        }
    }
}

/// The exact folder-rule message from SPEC §4.4.
pub const NOT_MY_SWAMP: &str = "Not my swamp: this folder isn't empty and has no qrec.json";

impl CoreError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            details: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn capture_failed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::CaptureFailed, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Io, message)
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Usage, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Invalid, message)
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Busy, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unavailable, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// `confirmation required: <what will be lost>` (SPEC §11).
    pub fn confirmation_required(what: impl AsRef<str>) -> Self {
        Self::new(
            ErrorCode::ConfirmationRequired,
            format!("confirmation required: {}", what.as_ref()),
        )
        .with_hint("pass --yes to confirm")
    }

    pub fn not_project() -> Self {
        Self::new(ErrorCode::NotProject, NOT_MY_SWAMP).with_hint(
            "run qrec in a folder with qrec.json or in an empty folder, or pass --project DIR",
        )
    }

    pub fn exit_code(&self) -> u8 {
        self.code.exit_code()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_spec() {
        assert_eq!(CoreError::usage("x").exit_code(), 2);
        assert_eq!(CoreError::not_project().exit_code(), 3);
        assert_eq!(CoreError::confirmation_required("x").exit_code(), 4);
        assert_eq!(
            CoreError::new(ErrorCode::MissingDependency, "x").exit_code(),
            5
        );
        assert_eq!(CoreError::busy("x").exit_code(), 6);
        assert_eq!(CoreError::not_found("x").exit_code(), 7);
        assert_eq!(CoreError::invalid("x").exit_code(), 1);
        assert_eq!(CoreError::new(ErrorCode::Interrupted, "x").exit_code(), 130);
        assert_eq!(
            serde_json::to_value(ErrorCode::Interrupted).unwrap(),
            serde_json::json!(ErrorCode::Interrupted.as_str())
        );
    }

    #[test]
    fn json_shape() {
        let e = CoreError::confirmation_required("purge take-0003 (2 files, 412 MB)");
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({
                "code": "confirmation_required",
                "message": "confirmation required: purge take-0003 (2 files, 412 MB)",
                "hint": "pass --yes to confirm"
            })
        );
    }
}
