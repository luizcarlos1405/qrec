//! Which project a command is for (A§8.4, SPEC §4.4):
//!
//! 1. `--project DIR` — used as given (it must hold `qrec.json` or be
//!    strictly empty);
//! 2. else the cwd, if it holds `qrec.json` or is strictly empty;
//! 3. else, if an ancestor holds `qrec.json`: exit 3 "you're inside project
//!    <dir>; run there or pass --project <dir>";
//! 4. else the single live host that answers `hello` on `<runtime>/*.sock`
//!    (the CLI prints `qrec: using project <dir> (pid N)` on stderr), so
//!    compositor key binds work from `$HOME`;
//! 5. else exit 3 ("Not my swamp"); several live hosts are a usage error
//!    listing them.
//!
//! A folder whose only entry is `.qrec/` counts as empty here too, not only
//! in the re-check under the lock (A§8.4 init order): it is a project
//! another `qrec` is initializing right now (it did `mkdir .qrec` and holds
//! or is taking the lock), or one whose initialization died before writing
//! `qrec.json`. Without this, two commands started together in a fresh
//! folder would race and one would exit 3 (`qrec.json` is written through
//! `.qrec/qrec.json.tmp`, so nothing else shows up meanwhile).
//!
//! A folder without `qrec.json` whose `.qrec/` holds a history is a project
//! that lost its file (A§7.7): it is that project, whatever else is in it.
//!
//! Scanning never deletes anything (A§8.3 step 2).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use qrec_core::model::PROJECT_FILE;
use qrec_core::protocol::HelloInfo;
use qrec_core::{CoreError, ErrorCode};

use super::HELLO_TIMEOUT;
use super::client::hello;
use crate::paths::is_socket_name;

/// At most this many sockets are probed when looking for live hosts.
const MAX_SCAN: usize = 64;

/// What a folder is, for the folder rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    /// It holds `qrec.json`.
    Project,
    /// No `qrec.json`, but `.qrec/` holds a history: the project lost its
    /// file and is restored from the history (A§7.7), never initialized again.
    MissingFile,
    /// Strictly empty (hidden entries count as content).
    Empty,
    /// Anything else: "Not my swamp".
    Other,
}

/// The files whose presence in `.qrec/` marks a project's history.
const HISTORY_FILES: [&str; 3] = ["history.head.json", "history.jsonl", "history.base.json"];

/// Classifies `dir`. With `qrec_dir_is_empty`, a folder whose only entry
/// is the `.qrec/` directory (without a history) counts as empty: the
/// re-check under the lock while initializing, after `mkdir .qrec` (A§8.4
/// init order).
pub fn classify(dir: &Path, qrec_dir_is_empty: bool) -> io::Result<Folder> {
    if fs::symlink_metadata(dir.join(PROJECT_FILE)).is_ok() {
        return Ok(Folder::Project);
    }
    let qrec = dir.join(".qrec");
    if qrec.is_dir()
        && HISTORY_FILES
            .iter()
            .any(|name| fs::symlink_metadata(qrec.join(name)).is_ok())
    {
        return Ok(Folder::MissingFile);
    }
    let mut entries = fs::read_dir(dir)?;
    let Some(first) = entries.next() else {
        return Ok(Folder::Empty);
    };
    let first = first?;
    let only_qrec = qrec_dir_is_empty
        && first.file_name() == ".qrec"
        && first.file_type()?.is_dir()
        && entries.next().is_none();
    Ok(if only_qrec {
        Folder::Empty
    } else {
        Folder::Other
    })
}

/// Where the project came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSource {
    Flag,
    Cwd,
    /// The only live host, found by `hello`.
    LiveHost {
        pid: u32,
    },
}

/// The project a command runs against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProject {
    /// Canonical, so it maps to one socket.
    pub dir: PathBuf,
    pub source: ProjectSource,
    /// A project (`qrec.json` exists, or `.qrec/` holds the history of one
    /// that lost it); `false` for a strictly empty folder (it becomes a
    /// project on the first change or with `qrec init`).
    pub initialized: bool,
}

impl ResolvedProject {
    /// The stderr line for a project found through its host.
    pub fn notice(&self) -> Option<String> {
        match self.source {
            ProjectSource::LiveHost { pid } => Some(format!(
                "qrec: using project {} (pid {pid})",
                self.dir.display()
            )),
            ProjectSource::Flag | ProjectSource::Cwd => None,
        }
    }
}

/// Resolves the project of a project-scoped command (A§8.4). `cwd` should
/// be absolute; `runtime` is the runtime dir holding host sockets.
pub fn resolve_project(
    flag: Option<&Path>,
    cwd: &Path,
    runtime: &Path,
) -> Result<ResolvedProject, CoreError> {
    if let Some(flag) = flag {
        let given = if flag.is_absolute() {
            flag.to_path_buf()
        } else {
            cwd.join(flag)
        };
        let dir = fs::canonicalize(&given).map_err(|e| unusable(&given, &e))?;
        return match classify(&dir, true).map_err(|e| unusable(&dir, &e))? {
            Folder::Project | Folder::MissingFile => Ok(resolved(dir, ProjectSource::Flag, true)),
            Folder::Empty => Ok(resolved(dir, ProjectSource::Flag, false)),
            Folder::Other => Err(CoreError::not_project()),
        };
    }
    if let Ok(dir) = fs::canonicalize(cwd) {
        match classify(&dir, true) {
            Ok(Folder::Project | Folder::MissingFile) => {
                return Ok(resolved(dir, ProjectSource::Cwd, true));
            }
            Ok(Folder::Empty) => return Ok(resolved(dir, ProjectSource::Cwd, false)),
            Ok(Folder::Other) | Err(_) => {}
        }
        if let Some(project) = dir
            .ancestors()
            .skip(1)
            .find(|a| fs::symlink_metadata(a.join(PROJECT_FILE)).is_ok())
        {
            return Err(inside_project(project));
        }
    }
    let hosts = live_hosts(runtime, HELLO_TIMEOUT);
    match hosts.as_slice() {
        [] => Err(CoreError::not_project()),
        [host] => {
            // The exact path, also when it isn't UTF-8.
            let dir = PathBuf::from(OsString::from_vec(host.project_dir_bytes()));
            let initialized = fs::symlink_metadata(dir.join(PROJECT_FILE)).is_ok();
            Ok(resolved(
                dir,
                ProjectSource::LiveHost { pid: host.pid },
                initialized,
            ))
        }
        several => {
            let list: Vec<String> = several
                .iter()
                .map(|h| format!("{} (pid {})", h.project_dir, h.pid))
                .collect();
            Err(CoreError::usage(format!(
                "{} Qrec hosts are running and this folder isn't a project: {}",
                several.len(),
                list.join(", ")
            ))
            .with_hint("pass --project DIR or run qrec in the project folder"))
        }
    }
}

fn resolved(dir: PathBuf, source: ProjectSource, initialized: bool) -> ResolvedProject {
    ResolvedProject {
        dir,
        source,
        initialized,
    }
}

/// Exit 3 for a folder inside a project (A§8.4).
pub fn inside_project(project: &Path) -> CoreError {
    let dir = project.display();
    CoreError::new(
        ErrorCode::NotProject,
        format!("you're inside project {dir}; run there or pass --project {dir}"),
    )
}

fn unusable(dir: &Path, e: &io::Error) -> CoreError {
    CoreError::new(
        ErrorCode::NotProject,
        format!("can't use {} as a project folder: {e}", dir.display()),
    )
    .with_hint("pass a folder that holds qrec.json, or an empty one")
}

/// Every host that answers `hello` on `<runtime>/<64 hex>.sock`, one per
/// project, probed in parallel (so the whole scan takes at most `timeout`).
pub fn live_hosts(runtime: &Path, timeout: Duration) -> Vec<HelloInfo> {
    let Ok(entries) = fs::read_dir(runtime) else {
        return Vec::new();
    };
    let sockets: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| is_socket_name(&e.file_name()))
        .map(|e| e.path())
        .take(MAX_SCAN)
        .collect();
    let mut hosts: Vec<HelloInfo> = thread::scope(|scope| {
        let probes: Vec<_> = sockets
            .iter()
            .filter_map(|socket| {
                thread::Builder::new()
                    .name("qrec-scan".into())
                    .spawn_scoped(scope, move || hello(socket, timeout).ok())
                    .ok()
            })
            .collect();
        probes
            .into_iter()
            .filter_map(|probe| probe.join().ok().flatten())
            .collect()
    });
    hosts.sort_by(|a, b| a.project_dir.cmp(&b.project_dir));
    hosts.dedup_by(|a, b| a.project_dir == b.project_dir);
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::testing::{socket_dir, start_server};
    use crate::paths::socket_path;
    use std::os::unix::net::UnixListener;

    fn project(dir: &Path) {
        fs::write(dir.join(PROJECT_FILE), "{}").unwrap();
    }

    #[test]
    fn classifies_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(classify(dir, false).unwrap(), Folder::Empty);
        fs::create_dir(dir.join(".qrec")).unwrap();
        assert_eq!(classify(dir, false).unwrap(), Folder::Other);
        assert_eq!(classify(dir, true).unwrap(), Folder::Empty);
        fs::write(dir.join(".envrc"), "").unwrap();
        assert_eq!(classify(dir, true).unwrap(), Folder::Other);
        project(dir);
        assert_eq!(classify(dir, false).unwrap(), Folder::Project);
        // A `.qrec` file (not a dir) doesn't count as empty.
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join(".qrec"), "").unwrap();
        assert_eq!(classify(other.path(), true).unwrap(), Folder::Other);
        assert!(classify(&dir.join("missing"), false).is_err());
    }

    #[test]
    fn flag_then_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("run");
        let proj = tmp.path().join("proj");
        fs::create_dir(&proj).unwrap();
        project(&proj);
        let empty = tmp.path().join("empty");
        fs::create_dir(&empty).unwrap();
        let junk = tmp.path().join("junk");
        fs::create_dir(&junk).unwrap();
        fs::write(junk.join("notes.txt"), "").unwrap();

        // --project is relative to the cwd and canonicalized.
        let r = resolve_project(Some(Path::new("../proj")), &empty, &runtime).unwrap();
        assert_eq!(r.dir, fs::canonicalize(&proj).unwrap());
        assert_eq!(
            (r.source.clone(), r.initialized),
            (ProjectSource::Flag, true)
        );
        assert_eq!(r.notice(), None);
        let r = resolve_project(Some(&empty), &junk, &runtime).unwrap();
        assert!(!r.initialized);
        // Being initialized by another process (only `.qrec/` so far).
        fs::create_dir(empty.join(".qrec")).unwrap();
        let r = resolve_project(None, &empty, &runtime).unwrap();
        assert_eq!(
            (r.source.clone(), r.initialized),
            (ProjectSource::Cwd, false)
        );
        fs::remove_dir(empty.join(".qrec")).unwrap();
        let err = resolve_project(Some(&junk), &proj, &runtime).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotProject);
        assert_eq!(err.message, qrec_core::error::NOT_MY_SWAMP);
        let err = resolve_project(Some(Path::new("nope")), &proj, &runtime).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotProject);

        let r = resolve_project(None, &proj, &runtime).unwrap();
        assert_eq!((r.source, r.initialized), (ProjectSource::Cwd, true));
        let r = resolve_project(None, &empty, &runtime).unwrap();
        assert_eq!((r.source, r.initialized), (ProjectSource::Cwd, false));
        // No project, no host.
        let err = resolve_project(None, &junk, &runtime).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotProject);
    }

    #[test]
    fn inside_a_project_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        let deep = proj.join("exports/old");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("a.mp4"), "").unwrap();
        project(&proj);
        let err = resolve_project(None, &deep, &tmp.path().join("run")).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotProject);
        let canonical = fs::canonicalize(&proj).unwrap();
        assert_eq!(
            err.message,
            format!(
                "you're inside project {0}; run there or pass --project {0}",
                canonical.display()
            )
        );
    }

    #[test]
    fn falls_back_to_the_single_live_host() {
        let tmp = tempfile::tempdir().unwrap();
        let junk = tmp.path().join("junk");
        fs::create_dir(&junk).unwrap();
        fs::write(junk.join("x"), "").unwrap();
        let runtime = socket_dir();
        let proj = tmp.path().join("proj");
        fs::create_dir(&proj).unwrap();
        project(&proj);

        // A stale socket (nobody listening) and an unrelated file are ignored.
        let stale = socket_path(runtime.path(), Path::new("/gone/project")).unwrap();
        drop(UnixListener::bind(&stale).unwrap());
        fs::write(runtime.path().join("notes.sock"), "").unwrap();

        let host = start_live_host(runtime.path(), &proj);
        let r = resolve_project(None, &junk, runtime.path()).unwrap();
        assert_eq!(r.dir, fs::canonicalize(&proj).unwrap());
        assert_eq!(
            r.source,
            ProjectSource::LiveHost {
                pid: std::process::id()
            }
        );
        assert!(r.initialized);
        assert_eq!(
            r.notice().unwrap(),
            format!(
                "qrec: using project {} (pid {})",
                r.dir.display(),
                std::process::id()
            )
        );
        assert!(stale.exists(), "scanners never delete anything");

        // Two live hosts: a usage error naming both.
        let other = tmp.path().join("other");
        fs::create_dir(&other).unwrap();
        let second = start_live_host(runtime.path(), &other);
        let err = resolve_project(None, &junk, runtime.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert!(err.message.starts_with("2 Qrec hosts"), "{}", err.message);
        drop((host, second));
    }

    /// A folder that isn't UTF-8 serves a host like any other: a command
    /// from elsewhere uses that exact folder, not its lossy display name.
    #[test]
    fn a_live_host_in_a_folder_that_isnt_utf8_is_found() {
        use std::os::unix::ffi::OsStrExt;
        let tmp = tempfile::tempdir().unwrap();
        let junk = tmp.path().join("junk");
        fs::create_dir(&junk).unwrap();
        fs::write(junk.join("x"), "").unwrap();
        let odd = tmp
            .path()
            .join(std::ffi::OsStr::from_bytes(b"\xff\xfe-bad"));
        fs::create_dir(&odd).unwrap();
        project(&odd);
        let runtime = socket_dir();
        let host = start_live_host(runtime.path(), &odd);
        let r = resolve_project(None, &junk, runtime.path()).unwrap();
        assert_eq!(r.dir, fs::canonicalize(&odd).unwrap());
        assert!(r.initialized);
        drop(host);
    }

    #[test]
    fn a_project_that_lost_its_file_is_still_a_project() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::create_dir_all(dir.join(".qrec")).unwrap();
        fs::create_dir(dir.join("takes")).unwrap();
        assert_eq!(classify(dir, true).unwrap(), Folder::Other);
        fs::write(dir.join(".qrec/history.jsonl"), "").unwrap();
        assert_eq!(classify(dir, true).unwrap(), Folder::MissingFile);
        let r = resolve_project(Some(dir), dir, &dir.join("run")).unwrap();
        assert_eq!((r.source, r.initialized), (ProjectSource::Flag, true));
    }

    /// A server for `dir` on its real socket name in `runtime`.
    fn start_live_host(runtime: &Path, dir: &Path) -> crate::ipc::testing::TestServer {
        let canonical = fs::canonicalize(dir).unwrap();
        let t = start_server(runtime, |cfg| {
            let (shown, exact) = HelloInfo::dir_fields(canonical.as_os_str().as_encoded_bytes());
            cfg.project_dir = shown;
            cfg.project_dir_hex = exact;
        });
        // Move it to the hashed name the scanner looks for.
        let socket = socket_path(runtime, &canonical).unwrap();
        fs::rename(&t.socket, &socket).unwrap();
        crate::ipc::testing::TestServer { socket, ..t }
    }
}
