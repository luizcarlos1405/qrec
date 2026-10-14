//! The backend of a `qrec` built without its shell: the project-less
//! commands that need only the core and the user's config files work;
//! everything that needs a project host, devices or tools reports that it
//! isn't available in this build.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use qrec_core::CoreError;
use qrec_core::protocol::HelloInfo;
use serde_json::Value;

use crate::args::HostArgs;
use crate::backend::{Backend, Events, Finish, ProjectRequest, RouteOpts};

/// A [`Backend`] without `qrec-shell`.
#[derive(Clone, Debug, Default)]
pub struct Standalone {
    /// `$XDG_CONFIG_HOME` and `$HOME` as read at startup (or set by tests).
    pub xdg_config_home: Option<OsString>,
    pub home: Option<OsString>,
}

impl Standalone {
    /// Reads `$XDG_CONFIG_HOME` and `$HOME` from the process environment.
    pub fn from_env() -> Self {
        Self {
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            home: std::env::var_os("HOME"),
        }
    }
}

fn no_shell(what: &str) -> CoreError {
    CoreError::unavailable(format!(
        "{what} can't run: this qrec build doesn't include its shell (qrec-shell)"
    ))
    .with_hint("project-less commands work: qrec --help, version, schema, keys, agent-guide, config, completions")
}

impl Backend for Standalone {
    /// `$XDG_CONFIG_HOME/qrec`, else `$HOME/.config/qrec` (A§8.2; relative
    /// values are ignored, as the XDG spec says).
    fn config_dir(&mut self) -> Result<PathBuf, CoreError> {
        let absolute =
            |v: &Option<OsString>| v.as_ref().map(PathBuf::from).filter(|p| p.is_absolute());
        if let Some(dir) = absolute(&self.xdg_config_home) {
            return Ok(dir.join("qrec"));
        }
        absolute(&self.home)
            .map(|home| home.join(".config").join("qrec"))
            .ok_or_else(|| {
                CoreError::io("can't find the config folder: neither $XDG_CONFIG_HOME nor $HOME is an absolute path")
                    .with_hint("set HOME")
            })
    }

    fn live_hosts(&mut self) -> Result<Vec<HelloInfo>, CoreError> {
        Ok(Vec::new())
    }

    fn request(
        &mut self,
        _project: &Path,
        _request: ProjectRequest,
        _opts: &RouteOpts,
        _events: &mut dyn Events,
    ) -> Finish {
        Finish::Reply(Err(no_shell("project commands")))
    }

    fn doctor(&mut self) -> Result<Value, CoreError> {
        Err(no_shell("qrec doctor"))
    }

    fn sources(&mut self) -> Result<Value, CoreError> {
        Err(no_shell("qrec sources list"))
    }

    fn host(&mut self, _project: &Path, _args: &HostArgs) -> Result<(), CoreError> {
        Err(no_shell("qrec host"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_follows_xdg() {
        let mut both = Standalone {
            xdg_config_home: Some("/cfg".into()),
            home: Some("/home/shrek".into()),
        };
        assert_eq!(both.config_dir().unwrap(), Path::new("/cfg/qrec"));
        let mut relative = Standalone {
            xdg_config_home: Some("cfg".into()),
            home: Some("/home/shrek".into()),
        };
        assert_eq!(
            relative.config_dir().unwrap(),
            Path::new("/home/shrek/.config/qrec")
        );
        assert!(Standalone::default().config_dir().is_err());
    }

    #[test]
    fn everything_else_is_unavailable() {
        let mut backend = Standalone::default();
        assert_eq!(backend.live_hosts().unwrap(), vec![]);
        let err = backend.doctor().unwrap_err();
        assert_eq!(err.code, qrec_core::ErrorCode::Unavailable);
        assert!(err.message.contains("qrec doctor"), "{}", err.message);
    }
}
