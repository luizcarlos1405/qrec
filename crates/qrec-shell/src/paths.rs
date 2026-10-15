//! Where Qrec keeps files: per-user directories (A§8.2) and the fixed
//! layout inside a project folder (SPEC §6.2, A§7.6, A§8.7).
//!
//! Resolution is pure over an [`Env`] snapshot, so tests pass their own
//! variables instead of mutating the process environment. Only
//! [`ensure_runtime_dir`], [`ensure_private_dir`] and [`check_private_dir`]
//! touch the disk.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use qrec_core::hash::sha256_hex;
use qrec_core::model::PROJECT_FILE;
use qrec_core::{CoreError, TakeId};

/// The variables path resolution reads.
pub const ENV_KEYS: [&str; 6] = [
    "QREC_RUNTIME_DIR",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "HOME",
];

/// The longest Unix socket path Linux accepts: `sun_path` is 108 bytes
/// including the terminating NUL.
pub const MAX_SOCKET_PATH: usize = 107;

/// A snapshot of the environment variables in [`ENV_KEYS`] plus the real
/// uid. Build it with [`Env::from_process`] in binaries and with
/// [`Env::new`] + [`Env::with`] in tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, OsString>,
    uid: u32,
}

impl Env {
    /// The current process's variables and real uid.
    pub fn from_process() -> Self {
        let mut env = Env::new(rustix::process::getuid().as_raw());
        for key in ENV_KEYS {
            if let Some(value) = std::env::var_os(key) {
                env.vars.insert(key.to_owned(), value);
            }
        }
        env
    }

    /// No variables set, running as `uid`.
    pub fn new(uid: u32) -> Self {
        Self {
            vars: BTreeMap::new(),
            uid,
        }
    }

    /// Sets `key` (builder style).
    pub fn with(mut self, key: impl Into<String>, value: impl Into<OsString>) -> Self {
        self.set(key, value);
        self
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<OsString>) {
        self.vars.insert(key.into(), value.into());
    }

    pub fn remove(&mut self, key: &str) {
        self.vars.remove(key);
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        self.vars.get(key).map(OsString::as_os_str)
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// `key` as a path if it is set, non-empty and absolute. Relative values
    /// are ignored, as the XDG Base Directory spec requires.
    fn abs_path(&self, key: &str) -> Option<PathBuf> {
        let path = Path::new(self.get(key)?);
        path.is_absolute().then(|| path.to_path_buf())
    }
}

/// Qrec's per-user directories (A§8.2). Resolving does no I/O.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserDirs {
    /// Sockets: `$QREC_RUNTIME_DIR`, else `$XDG_RUNTIME_DIR/qrec`, else `/tmp/qrec-<uid>`.
    pub runtime: PathBuf,
    /// `$XDG_CONFIG_HOME/qrec`, else `~/.config/qrec`: `config.toml`, `keys.toml`.
    pub config: PathBuf,
    /// `$XDG_STATE_HOME/qrec`, else `~/.local/state/qrec`: portal tokens, last session log.
    pub state: PathBuf,
    /// `$XDG_CACHE_HOME/qrec`, else `~/.cache/qrec`: detected capabilities.
    pub cache: PathBuf,
}

impl UserDirs {
    /// Fails only when neither an XDG variable nor `$HOME` is an absolute path.
    pub fn resolve(env: &Env) -> Result<Self, CoreError> {
        Ok(Self {
            runtime: runtime_dir(env),
            config: xdg_dir(env, "XDG_CONFIG_HOME", ".config")?,
            state: xdg_dir(env, "XDG_STATE_HOME", ".local/state")?,
            cache: xdg_dir(env, "XDG_CACHE_HOME", ".cache")?,
        })
    }

    /// `config/config.toml`: user settings (A§9.8).
    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    /// `config/keys.toml`: key binding overrides (A§9.7).
    pub fn keys_file(&self) -> PathBuf {
        self.config.join("keys.toml")
    }

    /// `state/portal-tokens.json` (mode 0600): ScreenCast restore tokens by output name.
    pub fn portal_tokens(&self) -> PathBuf {
        self.state.join("portal-tokens.json")
    }

    /// `state/last-session.log`.
    pub fn last_session_log(&self) -> PathBuf {
        self.state.join("last-session.log")
    }

    /// `cache/capabilities.json`: detected ffmpeg encoders and VA-API support.
    pub fn capabilities_cache(&self) -> PathBuf {
        self.cache.join("capabilities.json")
    }

    /// The host socket of a project, see [`socket_path`].
    pub fn socket_path(&self, canonical_project_dir: &Path) -> Result<PathBuf, CoreError> {
        socket_path(&self.runtime, canonical_project_dir)
    }
}

/// `$QREC_RUNTIME_DIR`, else `$XDG_RUNTIME_DIR/qrec`, else `/tmp/qrec-<uid>`.
pub fn runtime_dir(env: &Env) -> PathBuf {
    if let Some(dir) = env.abs_path("QREC_RUNTIME_DIR") {
        dir
    } else if let Some(dir) = env.abs_path("XDG_RUNTIME_DIR") {
        dir.join("qrec")
    } else {
        PathBuf::from(format!("/tmp/qrec-{}", env.uid()))
    }
}

fn xdg_dir(env: &Env, key: &str, home_default: &str) -> Result<PathBuf, CoreError> {
    if let Some(base) = env.abs_path(key) {
        return Ok(base.join("qrec"));
    }
    match env.abs_path("HOME") {
        Some(home) => Ok(home.join(home_default).join("qrec")),
        None => Err(CoreError::io(format!(
            "can't place Qrec's files: neither ${key} nor $HOME is an absolute path"
        ))
        .with_hint("set HOME")),
    }
}

/// Resolves the runtime dir, creates it (mode 0700) if needed and checks it
/// with [`check_private_dir`]. Anything else is refused, never repaired.
pub fn ensure_runtime_dir(env: &Env) -> Result<PathBuf, CoreError> {
    let dir = runtime_dir(env);
    ensure_private_dir(&dir, env.uid()).map_err(|e| {
        e.with_hint(format!(
            "fix or remove {}, or set QREC_RUNTIME_DIR to a private directory",
            dir.display()
        ))
    })?;
    Ok(dir)
}

/// Creates `dir` (and missing parents) with mode 0700 unless it exists, then
/// checks it with [`check_private_dir`].
pub fn ensure_private_dir(dir: &Path, uid: u32) -> Result<(), CoreError> {
    match fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|e| io_error(format!("can't create {}", dir.display()), &e))?;
            // The umask may have cleared bits of the mode we asked for.
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .map_err(|e| io_error(format!("can't chmod {}", dir.display()), &e))?;
        }
        Err(e) => return Err(io_error(format!("can't inspect {}", dir.display()), &e)),
    }
    check_private_dir(dir, uid)
}

/// `lstat`s `dir`: it must be a real directory (not a symlink), owned by
/// `uid`, with permission bits exactly 0700.
pub fn check_private_dir(dir: &Path, uid: u32) -> Result<(), CoreError> {
    let meta = fs::symlink_metadata(dir)
        .map_err(|e| io_error(format!("can't inspect {}", dir.display()), &e))?;
    let refuse = |why: String| CoreError::io(format!("refusing to use {}: {why}", dir.display()));
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return Err(refuse("it is a symlink".into()));
    }
    if !file_type.is_dir() {
        return Err(refuse("it is not a directory".into()));
    }
    if meta.uid() != uid {
        return Err(refuse(format!(
            "it belongs to uid {}, not {uid}",
            meta.uid()
        )));
    }
    let mode = meta.mode() & 0o777;
    if mode != 0o700 {
        return Err(refuse(format!("its mode is {mode:04o}, not 0700")));
    }
    Ok(())
}

fn io_error(what: String, e: &io::Error) -> CoreError {
    CoreError::io(format!("{what}: {e}"))
}

/// `<runtime>/<sha256 hex of the canonical project dir>.sock` (A§8.2).
///
/// `canonical_project_dir` must already be canonical (`fs::canonicalize`),
/// so every path to one project maps to one socket. Paths longer than
/// [`MAX_SOCKET_PATH`] bytes are refused.
pub fn socket_path(runtime: &Path, canonical_project_dir: &Path) -> Result<PathBuf, CoreError> {
    if !canonical_project_dir.is_absolute() {
        return Err(CoreError::internal(format!(
            "socket_path needs a canonical project dir, got {}",
            canonical_project_dir.display()
        )));
    }
    let digest = sha256_hex(canonical_project_dir.as_os_str().as_bytes());
    let path = runtime.join(format!("{digest}.sock"));
    check_socket_len(&path)?;
    Ok(path)
}

/// `<socket>.<pid>`: the name a host binds before renaming it over the
/// socket path, so the socket appears atomically (A§8.3). Also length-checked.
pub fn socket_publish_path(socket: &Path, pid: u32) -> Result<PathBuf, CoreError> {
    let mut name = socket.as_os_str().to_owned();
    name.push(format!(".{pid}"));
    let path = PathBuf::from(name);
    check_socket_len(&path)?;
    Ok(path)
}

/// Whether a runtime-dir entry is a host socket (`<64 hex>.sock`), for
/// finding running hosts. Publish names (`….sock.<pid>`) don't match.
pub fn is_socket_name(name: &OsStr) -> bool {
    name.as_bytes().strip_suffix(b".sock").is_some_and(|hex| {
        hex.len() == 64 && hex.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn check_socket_len(path: &Path) -> Result<(), CoreError> {
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH {
        return Err(CoreError::io(format!(
            "socket path {} is {len} bytes; Unix sockets allow at most {MAX_SOCKET_PATH}",
            path.display()
        ))
        .with_hint("set QREC_RUNTIME_DIR to a shorter directory, e.g. /tmp/qrec-$(id -u)"));
    }
    Ok(())
}

/// The fixed layout of a project folder (SPEC §6.2). The commit record of
/// A§7.6 is `history.head.json`; `history.cursor` is SPEC's copy of the
/// cursor for readers, written after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectPaths {
    root: PathBuf,
}

impl ProjectPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The project folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `qrec.json`: the project, the source of truth.
    pub fn project_file(&self) -> PathBuf {
        self.root.join(PROJECT_FILE)
    }

    /// `qrec.json.tmp`: written, fsynced and renamed over `qrec.json`.
    pub fn project_tmp(&self) -> PathBuf {
        self.root.join(format!("{PROJECT_FILE}.tmp"))
    }

    /// `takes/`.
    pub fn takes_dir(&self) -> PathBuf {
        self.root.join("takes")
    }

    /// `takes/<take>/`: one file per layer and track, plus `recorder.log`.
    pub fn take_dir(&self, take: &TakeId) -> PathBuf {
        self.takes_dir().join(take.as_str())
    }

    /// `takes/<take>/recorder.log`.
    pub fn recorder_log(&self, take: &TakeId) -> PathBuf {
        self.take_dir(take).join("recorder.log")
    }

    /// `exports/`: exports, never overwritten.
    pub fn exports_dir(&self) -> PathBuf {
        self.root.join("exports")
    }

    /// `frames/`: saved PNG stills.
    pub fn frames_dir(&self) -> PathBuf {
        self.root.join("frames")
    }

    /// `.qrec/`: Qrec's own state.
    pub fn qrec_dir(&self) -> PathBuf {
        self.root.join(".qrec")
    }

    /// `.qrec/lock`: the host's flock and its `{pid, kind, socket, since, state}`.
    pub fn lock_file(&self) -> PathBuf {
        self.qrec_dir().join("lock")
    }

    /// `.qrec/history.jsonl`: one history entry per line.
    pub fn history_file(&self) -> PathBuf {
        self.qrec_dir().join("history.jsonl")
    }

    /// `.qrec/history.head.json`: the commit record (cursor, seqs, project hash and copy).
    pub fn history_head(&self) -> PathBuf {
        self.qrec_dir().join("history.head.json")
    }

    /// `.qrec/history.base.json`: the project before the oldest kept entry.
    pub fn history_base(&self) -> PathBuf {
        self.qrec_dir().join("history.base.json")
    }

    /// `.qrec/history.cursor`: the current seq (SPEC §6.2, §8.1), a copy of
    /// the head's cursor for readers; Qrec never reads it back.
    pub fn history_cursor(&self) -> PathBuf {
        self.qrec_dir().join("history.cursor")
    }

    /// `.qrec/ui.json`: selection, playhead, zoom and panels; never undoable.
    pub fn ui_file(&self) -> PathBuf {
        self.qrec_dir().join("ui.json")
    }

    /// `.qrec/logs/`: one log per child process.
    pub fn logs_dir(&self) -> PathBuf {
        self.qrec_dir().join("logs")
    }

    /// `.qrec/logs/<stamp>-<pid>-<step>.log`, e.g. `…-host.log`. `stamp` is
    /// `YYYYMMDD-HHMMSS`.
    pub fn log_file(&self, stamp: &str, pid: u32, step: &str) -> PathBuf {
        self.logs_dir().join(format!("{stamp}-{pid}-{step}.log"))
    }

    /// `.qrec/logs/<stamp>-<pid>-j<job>-<step>.log` for a job's child process.
    pub fn job_log_file(&self, stamp: &str, pid: u32, job: u64, step: &str) -> PathBuf {
        self.logs_dir()
            .join(format!("{stamp}-{pid}-j{job}-{step}.log"))
    }

    /// `.qrec/cache/`: derived data, safe to delete at any time.
    pub fn cache_dir(&self) -> PathBuf {
        self.qrec_dir().join("cache")
    }

    /// `.qrec/cache/silence/`: silencedetect results by file and parameters.
    pub fn silence_cache_dir(&self) -> PathBuf {
        self.cache_dir().join("silence")
    }

    /// `.qrec/cache/segments/`: rendered export segments by plan hash.
    pub fn segments_dir(&self) -> PathBuf {
        self.cache_dir().join("segments")
    }

    /// `.qrec/cache/waveforms/`: waveform peaks.
    pub fn waveforms_dir(&self) -> PathBuf {
        self.cache_dir().join("waveforms")
    }

    /// `.qrec/cache/thumbnails/`.
    pub fn thumbnails_dir(&self) -> PathBuf {
        self.cache_dir().join("thumbnails")
    }

    /// `.qrec/rejected/`: invalid `qrec.json` files moved aside (A§7.7).
    pub fn rejected_dir(&self) -> PathBuf {
        self.qrec_dir().join("rejected")
    }

    /// `.qrec/rejected/qrec-<stamp>.json`.
    pub fn rejected_file(&self, stamp: &str) -> PathBuf {
        self.rejected_dir().join(format!("qrec-{stamp}.json"))
    }

    /// `.qrec/qrec.corrupt-<stamp>.json`: a corrupt file kept by `history restore`.
    pub fn corrupt_copy(&self, stamp: &str) -> PathBuf {
        self.qrec_dir().join(format!("qrec.corrupt-{stamp}.json"))
    }

    /// `.qrec/qrec.v<version>.json.bak`: the file before a schema migration.
    pub fn migration_backup(&self, version: u32) -> PathBuf {
        self.qrec_dir().join(format!("qrec.v{version}.json.bak"))
    }

    /// Joins a project-relative, `/`-separated path from the core
    /// (`takes/take-0001/L1-screen-DP-4.mkv`). Absolute paths, `.`, `..` and
    /// empty components are refused.
    pub fn resolve(&self, relative: &str) -> Result<PathBuf, CoreError> {
        let bad = || CoreError::invalid(format!("{relative:?} is not a project-relative path"));
        if relative.is_empty()
            || relative
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(bad());
        }
        let path = Path::new(relative);
        if !path.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(bad());
        }
        Ok(self.root.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn my_uid(dir: &Path) -> u32 {
        fs::metadata(dir).unwrap().uid()
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().mode() & 0o777
    }

    #[test]
    fn runtime_dir_order() {
        let env = Env::new(1000)
            .with("QREC_RUNTIME_DIR", "/q/run")
            .with("XDG_RUNTIME_DIR", "/run/user/1000");
        assert_eq!(runtime_dir(&env), Path::new("/q/run"));

        let mut env = env;
        env.remove("QREC_RUNTIME_DIR");
        assert_eq!(runtime_dir(&env), Path::new("/run/user/1000/qrec"));

        env.remove("XDG_RUNTIME_DIR");
        assert_eq!(runtime_dir(&env), Path::new("/tmp/qrec-1000"));

        // Empty and relative values don't count.
        let env = Env::new(7)
            .with("QREC_RUNTIME_DIR", "")
            .with("XDG_RUNTIME_DIR", "run/user/7");
        assert_eq!(runtime_dir(&env), Path::new("/tmp/qrec-7"));
    }

    #[test]
    fn xdg_dirs_and_home_defaults() {
        let env = Env::new(1000)
            .with("HOME", "/home/shrek")
            .with("XDG_CONFIG_HOME", "/cfg")
            .with("XDG_STATE_HOME", "/st")
            .with("XDG_CACHE_HOME", "/ca")
            .with("XDG_RUNTIME_DIR", "/run/user/1000");
        let dirs = UserDirs::resolve(&env).unwrap();
        assert_eq!(
            dirs,
            UserDirs {
                runtime: "/run/user/1000/qrec".into(),
                config: "/cfg/qrec".into(),
                state: "/st/qrec".into(),
                cache: "/ca/qrec".into(),
            }
        );
        assert_eq!(dirs.config_file(), Path::new("/cfg/qrec/config.toml"));
        assert_eq!(dirs.keys_file(), Path::new("/cfg/qrec/keys.toml"));
        assert_eq!(
            dirs.portal_tokens(),
            Path::new("/st/qrec/portal-tokens.json")
        );
        assert_eq!(
            dirs.last_session_log(),
            Path::new("/st/qrec/last-session.log")
        );
        assert_eq!(
            dirs.capabilities_cache(),
            Path::new("/ca/qrec/capabilities.json")
        );

        let env = Env::new(1000)
            .with("HOME", "/home/shrek")
            .with("XDG_CONFIG_HOME", "relative/cfg");
        let dirs = UserDirs::resolve(&env).unwrap();
        assert_eq!(dirs.config, Path::new("/home/shrek/.config/qrec"));
        assert_eq!(dirs.state, Path::new("/home/shrek/.local/state/qrec"));
        assert_eq!(dirs.cache, Path::new("/home/shrek/.cache/qrec"));
        assert_eq!(dirs.runtime, Path::new("/tmp/qrec-1000"));

        let err = UserDirs::resolve(&Env::new(1000)).unwrap_err();
        assert_eq!(err.code, qrec_core::ErrorCode::Io);
        assert!(err.message.contains("$HOME"), "{}", err.message);
    }

    #[test]
    fn from_process_reads_only_known_keys() {
        let env = Env::from_process();
        assert!(env.vars.keys().all(|k| ENV_KEYS.contains(&k.as_str())));
        assert_eq!(env.uid(), rustix::process::getuid().as_raw());
    }

    #[test]
    fn runtime_dir_is_created_private() {
        let tmp = tempfile::tempdir().unwrap();
        let uid = my_uid(tmp.path());
        let target = tmp.path().join("deep/run/qrec");
        let env = Env::new(uid).with("QREC_RUNTIME_DIR", &target);
        assert_eq!(ensure_runtime_dir(&env).unwrap(), target);
        assert_eq!(mode(&target), 0o700);
        // Idempotent.
        assert_eq!(ensure_runtime_dir(&env).unwrap(), target);

        let env = Env::new(uid).with("XDG_RUNTIME_DIR", tmp.path());
        assert_eq!(ensure_runtime_dir(&env).unwrap(), tmp.path().join("qrec"));
        assert_eq!(mode(&tmp.path().join("qrec")), 0o700);
    }

    #[test]
    fn unsafe_runtime_dirs_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let uid = my_uid(tmp.path());

        let open = tmp.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
        let err = ensure_private_dir(&open, uid).unwrap_err();
        assert!(err.message.contains("mode is 0755"), "{}", err.message);
        assert_eq!(mode(&open), 0o755, "an existing dir is never chmodded");

        let private = tmp.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        check_private_dir(&private, uid).unwrap();
        let err = check_private_dir(&private, uid.wrapping_add(1)).unwrap_err();
        assert!(err.message.contains("belongs to uid"), "{}", err.message);

        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&private, &link).unwrap();
        let err = ensure_private_dir(&link, uid).unwrap_err();
        assert!(err.message.contains("symlink"), "{}", err.message);

        let file = tmp.path().join("file");
        fs::write(&file, b"").unwrap();
        let err = ensure_private_dir(&file, uid).unwrap_err();
        assert!(err.message.contains("not a directory"), "{}", err.message);

        let env = Env::new(uid).with("QREC_RUNTIME_DIR", &open);
        let err = ensure_runtime_dir(&env).unwrap_err();
        assert_eq!(err.code, qrec_core::ErrorCode::Io);
        assert!(err.hint.unwrap().contains("QREC_RUNTIME_DIR"));
    }

    #[test]
    fn socket_path_hashes_the_canonical_dir() {
        let path = socket_path(
            Path::new("/run/user/1000/qrec"),
            Path::new("/home/shrek/swamp"),
        )
        .unwrap();
        let expected = format!(
            "/run/user/1000/qrec/{}.sock",
            sha256_hex(b"/home/shrek/swamp")
        );
        assert_eq!(path, Path::new(&expected));
        assert!(is_socket_name(path.file_name().unwrap()));
        assert_ne!(
            path,
            socket_path(
                Path::new("/run/user/1000/qrec"),
                Path::new("/home/shrek/swamp2")
            )
            .unwrap()
        );

        let publish = socket_publish_path(&path, 4242).unwrap();
        assert_eq!(publish, Path::new(&format!("{expected}.4242")));
        assert!(!is_socket_name(publish.file_name().unwrap()));
        assert!(!is_socket_name(OsStr::new("notes.sock")));

        assert_eq!(
            socket_path(Path::new("/run"), Path::new("relative"))
                .unwrap_err()
                .code,
            qrec_core::ErrorCode::Internal
        );
    }

    #[test]
    fn socket_paths_over_107_bytes_are_refused() {
        // "<dir>/" + 64 hex + ".sock" = dir + 70 bytes.
        let fits = format!("/{}", "r".repeat(MAX_SOCKET_PATH - 71));
        let path = socket_path(Path::new(&fits), Path::new("/p")).unwrap();
        assert_eq!(path.as_os_str().len(), MAX_SOCKET_PATH);

        let too_long = format!("/{}", "r".repeat(MAX_SOCKET_PATH - 70));
        let err = socket_path(Path::new(&too_long), Path::new("/p")).unwrap_err();
        assert_eq!(err.code, qrec_core::ErrorCode::Io);
        assert!(err.message.contains("108 bytes"), "{}", err.message);
        assert!(err.hint.unwrap().contains("QREC_RUNTIME_DIR"));

        // The publish name is longer than the socket itself.
        assert!(socket_publish_path(&path, 1).is_err());
    }

    #[test]
    fn project_layout() {
        let p = ProjectPaths::new("/v/my-video");
        let take: TakeId = "take-0001".parse().unwrap();
        let cases: [(PathBuf, &str); 25] = [
            (p.project_file(), "qrec.json"),
            (p.project_tmp(), "qrec.json.tmp"),
            (p.takes_dir(), "takes"),
            (p.take_dir(&take), "takes/take-0001"),
            (p.recorder_log(&take), "takes/take-0001/recorder.log"),
            (p.exports_dir(), "exports"),
            (p.frames_dir(), "frames"),
            (p.qrec_dir(), ".qrec"),
            (p.lock_file(), ".qrec/lock"),
            (p.history_file(), ".qrec/history.jsonl"),
            (p.history_head(), ".qrec/history.head.json"),
            (p.history_base(), ".qrec/history.base.json"),
            (p.history_cursor(), ".qrec/history.cursor"),
            (p.ui_file(), ".qrec/ui.json"),
            (p.logs_dir(), ".qrec/logs"),
            (
                p.log_file("20261014-120000", 42, "host"),
                ".qrec/logs/20261014-120000-42-host.log",
            ),
            (
                p.job_log_file("20261014-120000", 42, 7, "segment-3"),
                ".qrec/logs/20261014-120000-42-j7-segment-3.log",
            ),
            (p.cache_dir(), ".qrec/cache"),
            (p.silence_cache_dir(), ".qrec/cache/silence"),
            (p.segments_dir(), ".qrec/cache/segments"),
            (p.waveforms_dir(), ".qrec/cache/waveforms"),
            (p.thumbnails_dir(), ".qrec/cache/thumbnails"),
            (
                p.rejected_file("20261014-120000"),
                ".qrec/rejected/qrec-20261014-120000.json",
            ),
            (
                p.corrupt_copy("20261014-120000"),
                ".qrec/qrec.corrupt-20261014-120000.json",
            ),
            (p.migration_backup(1), ".qrec/qrec.v1.json.bak"),
        ];
        for (path, relative) in cases {
            assert_eq!(path, Path::new("/v/my-video").join(relative));
        }
        assert_eq!(p.root(), Path::new("/v/my-video"));
        assert_eq!(p.rejected_dir(), Path::new("/v/my-video/.qrec/rejected"));
    }

    #[test]
    fn resolve_accepts_only_project_relative_paths() {
        let p = ProjectPaths::new("/v/my-video");
        assert_eq!(
            p.resolve("takes/take-0001/L1-screen-DP-4.mkv").unwrap(),
            Path::new("/v/my-video/takes/take-0001/L1-screen-DP-4.mkv")
        );
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "takes/../../x",
            "takes//x",
            "./qrec.json",
            "takes/",
        ] {
            assert!(p.resolve(bad).is_err(), "{bad:?} was accepted");
        }
    }
}
