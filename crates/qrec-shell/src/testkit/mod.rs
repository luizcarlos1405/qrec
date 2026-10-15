//! Test helpers for shell and CLI tests (A§11). Enabled by the `testkit`
//! feature, and always in this crate's own tests.
//!
//! - [`TestEnv`]: a temp dir that isolates spawned `qrec` processes from the
//!   developer's config, state, portal tokens and running GUI.
//! - [`fixture`]: ffmpeg-generated media (`testsrc2` video, a 440 Hz tone
//!   gated by speech intervals), cached across test processes.
//! - [`fixture_project`]: a valid project whose takes point at fixture media.
//! - [`design`] (feature `testkit`: it builds on `qrec_core::testkit`): the
//!   GUI's fixture projects, BRIEF §3's sample among them (docs/GUI.md
//!   G§8.4).
//!
//! Helpers panic with a clear message instead of returning errors: they only
//! run in tests.

#[cfg(feature = "testkit")]
pub mod design;
mod media;
mod project;
mod tool;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

pub use media::{AudioSpec, MediaSpec, VideoSpec, detect_silences, ffprobe, fixture, fixtures_dir};
pub use project::{fixture_project, write_project};
pub use tool::{ToolOutput, run_tool};

/// An isolated environment for spawned processes.
///
/// Every [`Command`] built by [`TestEnv::command`] gets `HOME`,
/// `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`, `XDG_RUNTIME_DIR`
/// and `QREC_RUNTIME_DIR` inside the env's temp dirs and
/// `QREC_CAPTURE_BACKEND=fake`; other inherited `QREC_*` variables are
/// removed, and so are `DISPLAY`, `WAYLAND_DISPLAY` and `WAYLAND_SOCKET`
/// (a window never opens on the developer's display). The runtime dir lives
/// under `/tmp` so socket paths stay under the 107-byte limit. Everything is
/// deleted on drop.
pub struct TestEnv {
    root: TempDir,
    runtime_root: TempDir,
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl TestEnv {
    pub fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("qrec-test-")
            .tempdir()
            .expect("create the test env dir");
        // Short and private: `<runtime>/<64 hex>.sock` must fit in 107 bytes.
        let runtime_root = tempfile::Builder::new()
            .prefix("qrt-")
            .tempdir_in("/tmp")
            .expect("create the test runtime dir");
        for dir in ["home", "config", "state", "cache", "projects"] {
            fs::create_dir(root.path().join(dir)).expect("create a test env dir");
        }
        Self { root, runtime_root }
    }

    /// The temp dir holding `home/`, `config/`, `state/`, `cache/` and `projects/`.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// `$HOME`.
    pub fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    /// `$XDG_CONFIG_HOME`; Qrec's files go in its `qrec/` subdir.
    pub fn config_home(&self) -> PathBuf {
        self.root().join("config")
    }

    /// `$XDG_STATE_HOME`.
    pub fn state_home(&self) -> PathBuf {
        self.root().join("state")
    }

    /// `$XDG_CACHE_HOME`.
    pub fn cache_home(&self) -> PathBuf {
        self.root().join("cache")
    }

    /// `$XDG_RUNTIME_DIR` (mode 0700).
    pub fn xdg_runtime_dir(&self) -> PathBuf {
        self.runtime_root.path().to_path_buf()
    }

    /// `$QREC_RUNTIME_DIR`: where hosts put their sockets. Not created: Qrec
    /// creates it with mode 0700 on first use.
    pub fn runtime_dir(&self) -> PathBuf {
        self.runtime_root.path().join("qrec")
    }

    /// The variables applied to every command, in a fixed order.
    pub fn vars(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("HOME", self.home().into()),
            ("XDG_CONFIG_HOME", self.config_home().into()),
            ("XDG_STATE_HOME", self.state_home().into()),
            ("XDG_CACHE_HOME", self.cache_home().into()),
            ("XDG_RUNTIME_DIR", self.xdg_runtime_dir().into()),
            ("QREC_RUNTIME_DIR", self.runtime_dir().into()),
            ("QREC_CAPTURE_BACKEND", "fake".into()),
        ]
    }

    /// The same variables as a [`crate::paths::Env`], for in-process tests.
    pub fn paths_env(&self) -> crate::paths::Env {
        let uid = fs::metadata(self.root())
            .expect("stat the test env dir")
            .uid();
        self.vars()
            .into_iter()
            .fold(crate::paths::Env::new(uid), |env, (key, value)| {
                env.with(key, value)
            })
    }

    /// A new empty folder `projects/<name>/`, e.g. for a project.
    pub fn empty_dir(&self, name: &str) -> PathBuf {
        let dir = self.root().join("projects").join(name);
        fs::create_dir(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        dir
    }

    /// `program` with the isolated environment.
    pub fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(|k| k.starts_with("QREC_")) {
                command.env_remove(&key);
            }
        }
        // Never the developer's display: a `qrec` without a command opens
        // the window (the GUI tests give theirs a nested compositor).
        for key in ["DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"] {
            command.env_remove(key);
        }
        command.envs(self.vars());
        command
    }

    /// The `qrec` binary ([`qrec_bin`]) with the isolated environment.
    pub fn qrec(&self) -> Command {
        self.command(&qrec_bin())
    }
}

/// The `qrec` binary for CLI tests: `$CARGO_BIN_EXE_qrec` when set at run
/// time, else `qrec` in the test executable's profile dir
/// (`target/debug/deps/<test>` → `target/debug/qrec`), else
/// `<target>/debug/qrec`. Integration tests of the `qrec` crate can also
/// pass `env!("CARGO_BIN_EXE_qrec")` to [`TestEnv::command`].
pub fn qrec_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_qrec") {
        return PathBuf::from(path);
    }
    let exe = std::env::current_exe().ok();
    let mut candidates: Vec<PathBuf> = exe
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|dir| dir.join("qrec"))
        .into_iter()
        .collect();
    candidates.push(target_dir().join("debug").join("qrec"));
    match candidates.iter().find(|path| path.is_file()) {
        Some(path) => path.clone(),
        None => panic!(
            "the qrec binary isn't built; run `cargo build -p qrec` (looked at {candidates:?})"
        ),
    }
}

/// Cargo's target dir: `$CARGO_TARGET_DIR`, else the `target` ancestor of
/// the test executable, else `target/` at the workspace root.
pub fn target_dir() -> PathBuf {
    let workspace_root = || {
        let manifest = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
        manifest.and_then(|dir| {
            dir.ancestors()
                .find(|a| a.join("Cargo.lock").is_file())
                .map(Path::to_path_buf)
        })
    };
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        if dir.is_absolute() {
            return dir;
        }
        if let Some(root) = workspace_root() {
            return root.join(dir);
        }
    }
    let exe = std::env::current_exe().ok();
    if let Some(target) = exe.as_deref().and_then(|e| {
        e.ancestors()
            .find(|a| a.file_name().is_some_and(|n| n == "target"))
    }) {
        return target.to_path_buf();
    }
    match workspace_root() {
        Some(root) => root.join("target"),
        None => std::env::temp_dir().join("qrec-target"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_get_the_isolated_environment() {
        let env = TestEnv::new();
        let output = env
            .command(Path::new("sh"))
            .args(["-c", "printf '%s\\n' \"$HOME\" \"$XDG_CONFIG_HOME\" \"$XDG_STATE_HOME\" \"$XDG_CACHE_HOME\" \"$XDG_RUNTIME_DIR\" \"$QREC_RUNTIME_DIR\" \"$QREC_CAPTURE_BACKEND\""])
            .output()
            .expect("run sh");
        assert!(output.status.success());
        let lines: Vec<String> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        let expected: Vec<String> = env
            .vars()
            .into_iter()
            .map(|(_, v)| v.into_string().unwrap())
            .collect();
        assert_eq!(lines, expected);
        assert_eq!(lines[6], "fake");
        assert!(Path::new(&lines[0]).starts_with(env.root()));
        // Room for `<64 hex>.sock` under the 107-byte socket limit.
        assert!(env.runtime_dir().as_os_str().len() + 1 + 64 + ".sock".len() <= 107);
    }

    #[test]
    fn paths_env_matches_the_spawned_environment() {
        let env = TestEnv::new();
        let dirs = crate::paths::UserDirs::resolve(&env.paths_env()).unwrap();
        assert_eq!(dirs.runtime, env.runtime_dir());
        assert_eq!(dirs.config, env.config_home().join("qrec"));
        assert_eq!(dirs.state, env.state_home().join("qrec"));
        assert_eq!(dirs.cache, env.cache_home().join("qrec"));
        let runtime = crate::paths::ensure_runtime_dir(&env.paths_env()).unwrap();
        assert_eq!(runtime, env.runtime_dir());
        let socket = dirs.socket_path(&env.empty_dir("p")).unwrap();
        assert!(socket.starts_with(env.runtime_dir()));
    }

    #[test]
    fn dropping_the_env_removes_its_dirs() {
        let env = TestEnv::new();
        let (root, runtime) = (env.root().to_path_buf(), env.xdg_runtime_dir());
        assert!(root.join("home").is_dir() && runtime.is_dir());
        drop(env);
        assert!(!root.exists() && !runtime.exists());
    }

    #[test]
    fn target_dir_holds_the_test_binary() {
        let exe = std::env::current_exe().unwrap();
        assert!(
            exe.starts_with(target_dir()),
            "{} is not under {}",
            exe.display(),
            target_dir().display()
        );
    }
}
