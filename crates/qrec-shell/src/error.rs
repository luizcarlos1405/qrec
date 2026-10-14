//! Shell errors (foundation). Everything ends up as a `CoreError` for the
//! CLI exit code and the `{"ok":false,"error":…}` object.

use qrec_core::CoreError;
use qrec_core::protocol::HelloInfo;

#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    /// A core error passed through unchanged.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// A file or process operation failed; `what` says which ("write qrec.json").
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
    /// Another process holds the project lock and answered `hello` (A§8.3).
    /// Boxed to keep `Result<_, ShellError>` small.
    #[error("the project is open in a {} host (pid {})", .0.host.as_str(), .0.pid)]
    HostRunning(Box<HelloInfo>),
    /// A bug.
    #[error("{0}")]
    Internal(String),
}

impl ShellError {
    pub fn io(what: impl Into<String>, source: std::io::Error) -> Self {
        ShellError::Io {
            what: what.into(),
            source,
        }
    }

    /// The equivalent `CoreError` (exit code and JSON form).
    pub fn to_core(&self) -> CoreError {
        match self {
            ShellError::Core(e) => e.clone(),
            ShellError::Io { .. } => CoreError::io(self.to_string()),
            ShellError::HostRunning(hello) => CoreError::busy(self.to_string()).with_hint(format!(
                "send commands to it (they are routed automatically) or stop pid {}",
                hello.pid
            )),
            ShellError::Internal(_) => CoreError::internal(self.to_string()),
        }
    }
}

impl From<ShellError> for CoreError {
    fn from(e: ShellError) -> Self {
        match e {
            ShellError::Core(e) => e,
            other => other.to_core(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrec_core::ErrorCode;
    use qrec_core::state::{HostKind, RecordPhase};

    #[test]
    fn converts_to_core_errors() {
        let e: CoreError = ShellError::from(CoreError::not_found("no clip c1")).into();
        assert_eq!(e.code, ErrorCode::NotFound);
        let e: CoreError =
            ShellError::io("write qrec.json", std::io::Error::other("disk full")).into();
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrorCode::Io, "write qrec.json: disk full")
        );
        let hello = HelloInfo {
            pid: 42,
            project_dir: "/p".into(),
            host: HostKind::Gui,
            record: RecordPhase::Idle,
            jobs: vec![],
            version: "0.1.0".into(),
        };
        let e: CoreError = ShellError::HostRunning(Box::new(hello)).into();
        assert_eq!(e.code, ErrorCode::Busy);
        assert_eq!(e.message, "the project is open in a gui host (pid 42)");
    }
}
