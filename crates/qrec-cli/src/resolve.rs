//! Which project a command uses (SPEC §4.4 folder rule, A§8.4):
//! `--project DIR`, else the current directory, when it holds `qrec.json`
//! or is strictly empty; inside a project's subfolder, exit 3 pointing at
//! the project; without `--project`, the single running host; else exit 3
//! "Not my swamp".
//!
//! Reading a folder never changes it: resolution writes nothing.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use qrec_core::protocol::HelloInfo;
use qrec_core::{CoreError, PROJECT_FILE};

/// What a folder is to the folder rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Folder {
    /// It holds `qrec.json`.
    Project,
    /// Strictly empty (hidden files count), or holding only `.qrec/` (an
    /// interrupted `init`, A§8.4): it becomes a project on the first change.
    Empty,
    /// Anything else.
    Other,
}

/// The project folder a command runs against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// An absolute folder holding `qrec.json`.
    Project(PathBuf),
    /// An absolute, strictly empty folder: no project yet.
    Empty(PathBuf),
}

impl Target {
    pub fn dir(&self) -> &Path {
        match self {
            Target::Project(dir) | Target::Empty(dir) => dir,
        }
    }
}

/// A resolved target and, when it came from a running host, the notice
/// `using project <dir> (pid N)` for stderr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub target: Target,
    pub notice: Option<String>,
}

/// Classifies `dir` without changing anything.
pub fn classify(dir: &Path) -> Result<Folder, CoreError> {
    let meta = fs::metadata(dir).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => CoreError::new(
            qrec_core::ErrorCode::NotProject,
            format!("{} doesn't exist", dir.display()),
        )
        .with_hint("create the folder first, or pass --project with an existing folder"),
        _ => CoreError::io(format!("can't read {}: {e}", dir.display())),
    })?;
    if !meta.is_dir() {
        return Err(CoreError::new(
            qrec_core::ErrorCode::NotProject,
            format!("{} is not a folder", dir.display()),
        )
        .with_hint("pass --project with a project folder"));
    }
    if dir.join(PROJECT_FILE).exists() {
        return Ok(Folder::Project);
    }
    let entries = fs::read_dir(dir)
        .map_err(|e| CoreError::io(format!("can't list {}: {e}", dir.display())))?;
    let mut only_qrec_dir = true;
    for entry in entries {
        let entry =
            entry.map_err(|e| CoreError::io(format!("can't list {}: {e}", dir.display())))?;
        let is_qrec_dir =
            entry.file_name() == ".qrec" && entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_qrec_dir {
            only_qrec_dir = false;
            break;
        }
    }
    Ok(if only_qrec_dir {
        Folder::Empty
    } else {
        Folder::Other
    })
}

/// The nearest strict ancestor of `dir` holding `qrec.json`.
pub fn ancestor_project(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .skip(1)
        .find(|a| a.join(PROJECT_FILE).exists())
        .map(Path::to_path_buf)
}

/// Applies the folder rule to `dir` (absolute). `explicit` says it came
/// from `--project`, which turns the running-host fallback off; `hosts`
/// lists the running hosts and is called only when needed.
pub fn resolve(
    dir: &Path,
    explicit: bool,
    hosts: impl FnOnce() -> Result<Vec<HelloInfo>, CoreError>,
) -> Result<Resolved, CoreError> {
    let local = |target| {
        Ok(Resolved {
            target,
            notice: None,
        })
    };
    match classify(dir)? {
        Folder::Project => return local(Target::Project(dir.to_path_buf())),
        Folder::Empty => return local(Target::Empty(dir.to_path_buf())),
        Folder::Other => {}
    }
    if let Some(project) = ancestor_project(dir) {
        return Err(inside_project(&project));
    }
    if explicit {
        return Err(CoreError::not_project());
    }
    let hosts = hosts()?;
    match hosts.as_slice() {
        [] => Err(CoreError::not_project()),
        [host] => Ok(Resolved {
            target: Target::Project(PathBuf::from(&host.project_dir)),
            notice: Some(format!(
                "using project {} (pid {})",
                host.project_dir, host.pid
            )),
        }),
        several => {
            let list: Vec<String> = several
                .iter()
                .map(|h| {
                    format!(
                        "{} ({} host, pid {})",
                        h.project_dir,
                        h.host.as_str(),
                        h.pid
                    )
                })
                .collect();
            Err(CoreError::usage(format!(
                "several Qrec hosts are running: {}",
                list.join("; ")
            ))
            .with_hint("pass --project DIR to pick one"))
        }
    }
}

/// Exit 3 inside a project's subfolder: the folder-rule message with a
/// hint naming the project (A§8.4).
pub fn inside_project(project: &Path) -> CoreError {
    let dir = project.display();
    CoreError::not_project().with_hint(format!(
        "you're inside project {dir}; run there or pass --project {dir}"
    ))
}

#[cfg(test)]
mod tests {
    use qrec_core::ErrorCode;
    use qrec_core::error::NOT_MY_SWAMP;
    use qrec_core::state::{HostKind, RecordPhase};

    use super::*;

    fn hello(dir: &str, pid: u32) -> HelloInfo {
        HelloInfo {
            pid,
            project_dir: dir.into(),
            host: HostKind::Gui,
            record: RecordPhase::Idle,
            jobs: vec![],
            version: "0.1.0".into(),
        }
    }

    fn no_hosts() -> Result<Vec<HelloInfo>, CoreError> {
        Ok(vec![])
    }

    #[test]
    fn folders_are_classified() {
        let tmp = tempdir();
        let dir = tmp.path();
        assert_eq!(classify(dir).unwrap(), Folder::Empty);
        fs::create_dir(dir.join(".qrec")).unwrap();
        assert_eq!(
            classify(dir).unwrap(),
            Folder::Empty,
            "only .qrec/ is empty"
        );
        fs::write(dir.join(".envrc"), "").unwrap();
        assert_eq!(classify(dir).unwrap(), Folder::Other, "hidden files count");
        fs::write(dir.join(PROJECT_FILE), "{}").unwrap();
        assert_eq!(classify(dir).unwrap(), Folder::Project);

        let file_qrec = tempdir();
        fs::write(file_qrec.path().join(".qrec"), "").unwrap();
        assert_eq!(
            classify(file_qrec.path()).unwrap(),
            Folder::Other,
            "a .qrec file isn't the dir"
        );

        let missing = classify(&dir.join("nope")).unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotProject);
        assert!(
            missing.message.contains("doesn't exist"),
            "{}",
            missing.message
        );
        let not_dir = classify(&dir.join(".envrc")).unwrap_err();
        assert!(
            not_dir.message.contains("is not a folder"),
            "{}",
            not_dir.message
        );
    }

    #[test]
    fn the_folder_rule() {
        let tmp = tempdir();
        let empty = tmp.path().join("empty");
        fs::create_dir(&empty).unwrap();
        let resolved = resolve(&empty, false, no_hosts).unwrap();
        assert_eq!(resolved.target, Target::Empty(empty.clone()));

        let project = tmp.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join(PROJECT_FILE), "{}").unwrap();
        assert_eq!(
            resolve(&project, true, no_hosts).unwrap().target,
            Target::Project(project.clone())
        );

        let busy = tmp.path().join("busy");
        fs::create_dir(&busy).unwrap();
        fs::write(busy.join("notes.txt"), "").unwrap();
        let err = resolve(&busy, false, no_hosts).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (ErrorCode::NotProject, NOT_MY_SWAMP)
        );
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn subfolders_of_a_project_point_at_it() {
        let tmp = tempdir();
        let project = tmp.path().join("proj");
        let sub = project.join("takes/take-0001");
        fs::create_dir_all(&sub).unwrap();
        fs::write(project.join(PROJECT_FILE), "{}").unwrap();
        fs::write(sub.join("A1-mic.mka"), "").unwrap();
        // Neither the hosts nor --project change it.
        for explicit in [false, true] {
            let err =
                resolve(&sub, explicit, || panic!("no host lookup inside a project")).unwrap_err();
            assert_eq!(err.message, NOT_MY_SWAMP);
            assert_eq!(
                err.hint.unwrap(),
                format!(
                    "you're inside project {0}; run there or pass --project {0}",
                    project.display()
                )
            );
        }
    }

    #[test]
    fn running_hosts_are_the_fallback() {
        let tmp = tempdir();
        let busy = tmp.path().join("home");
        fs::create_dir(&busy).unwrap();
        fs::write(busy.join(".bashrc"), "").unwrap();

        let one = resolve(&busy, false, || Ok(vec![hello("/v/swamp", 42)])).unwrap();
        assert_eq!(one.target, Target::Project("/v/swamp".into()));
        assert_eq!(one.notice.unwrap(), "using project /v/swamp (pid 42)");

        let err = resolve(&busy, false, || {
            Ok(vec![hello("/v/a", 1), hello("/v/b", 2)])
        })
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert!(
            err.message
                .contains("/v/a (gui host, pid 1); /v/b (gui host, pid 2)"),
            "{}",
            err.message
        );

        let explicit = resolve(&busy, true, || panic!("--project never falls back")).unwrap_err();
        assert_eq!(explicit.message, NOT_MY_SWAMP);

        let failed = resolve(&busy, false, || Err(CoreError::io("runtime dir"))).unwrap_err();
        assert_eq!(failed.code, ErrorCode::Io);
    }

    fn tempdir() -> TempDir {
        TempDir::new()
    }

    /// A small temp dir (qrec-cli has no tempfile dependency).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static N: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "qrec-cli-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
