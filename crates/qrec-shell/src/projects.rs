//! The machine's projects (SPEC §4.4, §6.4, A§9.10): making a new project's
//! folder, the recent list on disk, and what `qrec` opens when it isn't
//! told. The rules that need no disk are in the core
//! (`qrec_core::new_project`, `qrec_core::recents`).
//!
//! The recent list is a convenience: a file that can't be read counts as
//! empty and a write that fails is logged, so nothing here can keep a
//! project from opening.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use qrec_core::CoreError;
use qrec_core::config::UserConfig;
use qrec_core::new_project::NewProjectPath;
use qrec_core::recents::RecentProjects;

use crate::config_file;
use crate::ipc::resolve::{Folder, classify};
use crate::paths::UserDirs;

/// Makes the new project's folder, the last part of `path` only: the folder
/// it goes in must exist. A folder already there is returned as it is, and
/// the folder rule decides what becomes of it (SPEC §4.4: empty is fine, a
/// project or anything else isn't).
pub fn make_folder(path: &NewProjectPath) -> Result<PathBuf, CoreError> {
    let parent = fs::canonicalize(&path.parent).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => {
            CoreError::io(format!("{} doesn't exist", path.parent.display()))
                .with_hint("only the project's own folder is made: create the one it goes in first")
        }
        _ => CoreError::io(format!("can't read {}: {e}", path.parent.display())),
    })?;
    if !parent.is_dir() {
        return Err(CoreError::io(format!(
            "{} isn't a folder",
            parent.display()
        )));
    }
    let dir = parent.join(&path.name);
    match fs::create_dir(&dir) {
        Ok(()) => Ok(dir),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if dir.is_dir() {
                fs::canonicalize(&dir)
                    .map_err(|e| CoreError::io(format!("can't read {}: {e}", dir.display())))
            } else {
                Err(CoreError::io(format!(
                    "can't create the folder: {} already exists and isn't a folder",
                    dir.display()
                )))
            }
        }
        Err(e) => Err(CoreError::io(format!(
            "can't create the folder {}: {e}",
            dir.display()
        ))),
    }
}

/// The recent list; empty when there is none or it can't be read.
pub fn read_recents(dirs: &UserDirs) -> RecentProjects {
    let path = dirs.recent_projects();
    let text = match config_file::read_text(&path) {
        Ok(Some(text)) => text,
        Ok(None) => return RecentProjects::default(),
        Err(e) => {
            tracing::warn!("the recent projects: {e}");
            return RecentProjects::default();
        }
    };
    RecentProjects::parse(&text).unwrap_or_else(|e| {
        tracing::warn!(
            "the recent projects in {} are unusable: {e}",
            path.display()
        );
        RecentProjects::default()
    })
}

/// Reads the list, changes it and writes it back whole; a write that fails
/// is logged. Two windows writing at once can lose one's change, never the
/// file (the write is atomic).
fn update_recents(dirs: &UserDirs, change: impl FnOnce(&mut RecentProjects)) {
    let mut list = read_recents(dirs);
    change(&mut list);
    if let Err(e) = config_file::write_text(&dirs.recent_projects(), &list.to_text()) {
        tracing::warn!("the recent projects: {e}");
    }
}

/// The window opened the project in `dir` (canonical). A folder whose path
/// isn't UTF-8 isn't listed: it still opens with `--project`.
pub fn note_opened(dirs: &UserDirs, dir: &Path, now: DateTime<Utc>) {
    match dir.to_str() {
        Some(dir) => update_recents(dirs, |list| list.opened(dir, now)),
        None => tracing::info!("{} isn't UTF-8: not in the recent list", dir.display()),
    }
}

/// Takes `dir` off the recent list. The folder isn't touched.
pub fn forget(dirs: &UserDirs, dir: &str) {
    update_recents(dirs, |list| list.forget(dir));
}

/// What `dir` is to the project picker and to `projects.open_last`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectFolder {
    /// It holds `qrec.json`.
    Project,
    /// Strictly empty: a project can start there.
    Empty,
    /// A project that lost its `qrec.json` but kept its history (A§7.7).
    LostFile,
    /// Anything else, a folder that is gone or can't be read included.
    NotProject,
}

/// Classifies `dir` without changing anything.
pub fn project_folder(dir: &Path) -> ProjectFolder {
    match classify(dir, true) {
        Ok(Folder::Project) => ProjectFolder::Project,
        Ok(Folder::Empty) => ProjectFolder::Empty,
        Ok(Folder::MissingFile) => ProjectFolder::LostFile,
        Ok(Folder::Other) | Err(_) => ProjectFolder::NotProject,
    }
}

/// What `qrec` opens when no folder was named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BareLaunch {
    /// This project.
    Open(PathBuf),
    /// The project picker.
    Picker,
}

/// `qrec` without `--project` (SPEC §4.4): a project in the current folder
/// opens; elsewhere the last project does when `projects.open_last` says so
/// and it is still there; otherwise the picker asks.
pub fn bare_launch(cwd: &Path, config: &UserConfig, recents: &RecentProjects) -> BareLaunch {
    if matches!(
        project_folder(cwd),
        ProjectFolder::Project | ProjectFolder::LostFile
    ) {
        return BareLaunch::Open(cwd.to_path_buf());
    }
    if config.projects.open_last
        && let Some(last) = recents.last()
        && project_folder(Path::new(last)) == ProjectFolder::Project
    {
        return BareLaunch::Open(PathBuf::from(last));
    }
    BareLaunch::Picker
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use qrec_core::ErrorCode;
    use qrec_core::model::PROJECT_FILE;

    use super::*;
    use crate::paths::Env;

    struct Tmp(tempfile::TempDir);

    impl Tmp {
        fn new() -> Self {
            Self(tempfile::tempdir().unwrap())
        }

        fn path(&self, name: &str) -> PathBuf {
            fs::canonicalize(self.0.path()).unwrap().join(name)
        }

        fn empty(&self, name: &str) -> PathBuf {
            let dir = self.path(name);
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn project(&self, name: &str) -> PathBuf {
            let dir = self.empty(name);
            fs::write(dir.join(PROJECT_FILE), "{}").unwrap();
            dir
        }

        fn dirs(&self) -> UserDirs {
            let home = self.empty("home");
            UserDirs::resolve(&Env::new(1000).with("HOME", home)).unwrap()
        }

        fn at(&self, parent: &Path, name: &str) -> NewProjectPath {
            NewProjectPath {
                parent: parent.to_path_buf(),
                name: name.into(),
            }
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 9, 10, 0, 0).unwrap()
    }

    fn open_last() -> UserConfig {
        let mut config = UserConfig::default();
        config.projects.open_last = true;
        config
    }

    fn opened(dirs: &[&Path]) -> RecentProjects {
        let mut list = RecentProjects::default();
        for dir in dirs.iter().rev() {
            list.opened(dir.to_str().unwrap(), now());
        }
        list
    }

    #[test]
    fn makes_only_the_last_folder() {
        let tmp = Tmp::new();
        let parent = tmp.empty("videos");
        let dir = make_folder(&tmp.at(&parent, "demo")).unwrap();
        assert_eq!(dir, parent.join("demo"));
        assert!(dir.is_dir());

        let missing = tmp.path("nope");
        let error = make_folder(&tmp.at(&missing, "demo")).unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
        assert!(error.message.contains("doesn't exist"), "{error:?}");
        assert!(!missing.exists(), "the parent isn't made");
    }

    #[test]
    fn a_folder_already_there_is_left_to_the_folder_rule() {
        let tmp = Tmp::new();
        let parent = tmp.empty("videos");
        let existing = parent.join("demo");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("keep.txt"), "mine").unwrap();
        assert_eq!(make_folder(&tmp.at(&parent, "demo")).unwrap(), existing);
        assert_eq!(
            fs::read_to_string(existing.join("keep.txt")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn a_file_in_the_way_is_refused() {
        let tmp = Tmp::new();
        let parent = tmp.empty("videos");
        fs::write(parent.join("demo"), "a file").unwrap();
        let error = make_folder(&tmp.at(&parent, "demo")).unwrap_err();
        assert!(error.message.contains("isn't a folder"), "{error:?}");
        let file = parent.join("demo");
        let error = make_folder(&tmp.at(&file, "inner")).unwrap_err();
        assert!(error.message.contains("isn't a folder"), "{error:?}");
    }

    #[test]
    fn the_recent_list_survives_on_disk_and_a_broken_file_is_empty() {
        let tmp = Tmp::new();
        let dirs = tmp.dirs();
        assert_eq!(read_recents(&dirs), RecentProjects::default());
        note_opened(&dirs, Path::new("/a"), now());
        note_opened(&dirs, Path::new("/b"), now());
        assert_eq!(read_recents(&dirs).last(), Some("/b"));
        forget(&dirs, "/b");
        assert_eq!(read_recents(&dirs).last(), Some("/a"));
        fs::write(dirs.recent_projects(), "not json").unwrap();
        assert_eq!(read_recents(&dirs), RecentProjects::default());
        note_opened(&dirs, Path::new("/c"), now());
        assert_eq!(read_recents(&dirs).last(), Some("/c"), "it starts over");
    }

    #[test]
    fn a_project_here_opens_whatever_the_setting_says() {
        let tmp = Tmp::new();
        let here = tmp.project("here");
        let last = tmp.project("last");
        let recents = opened(&[&last]);
        for config in [UserConfig::default(), open_last()] {
            assert_eq!(
                bare_launch(&here, &config, &recents),
                BareLaunch::Open(here.clone())
            );
        }
    }

    #[test]
    fn elsewhere_the_picker_asks_unless_the_setting_is_on() {
        let tmp = Tmp::new();
        let empty = tmp.empty("empty");
        let busy = tmp.empty("busy");
        fs::write(busy.join("notes.txt"), "x").unwrap();
        let last = tmp.project("last");
        let recents = opened(&[&last]);
        for here in [&empty, &busy] {
            assert_eq!(
                bare_launch(here, &UserConfig::default(), &recents),
                BareLaunch::Picker
            );
            assert_eq!(
                bare_launch(here, &open_last(), &recents),
                BareLaunch::Open(last.clone())
            );
        }
    }

    #[test]
    fn the_last_project_must_still_be_there() {
        let tmp = Tmp::new();
        let here = tmp.empty("here");
        let gone = tmp.path("gone");
        let notes = tmp.empty("notes");
        fs::write(notes.join("x"), "x").unwrap();
        for last in [&gone, &notes] {
            assert_eq!(
                bare_launch(&here, &open_last(), &opened(&[last])),
                BareLaunch::Picker,
                "{}",
                last.display()
            );
        }
        assert_eq!(
            bare_launch(&here, &open_last(), &RecentProjects::default()),
            BareLaunch::Picker,
            "nothing opened yet"
        );
    }

    #[test]
    fn a_project_that_lost_its_file_opens_here_and_nowhere_else() {
        let tmp = Tmp::new();
        let lost = tmp.empty("lost");
        fs::create_dir(lost.join(".qrec")).unwrap();
        fs::write(lost.join(".qrec/history.jsonl"), "").unwrap();
        assert_eq!(project_folder(&lost), ProjectFolder::LostFile);
        assert_eq!(
            bare_launch(&lost, &UserConfig::default(), &RecentProjects::default()),
            BareLaunch::Open(lost.clone())
        );
        let here = tmp.empty("here");
        assert_eq!(
            bare_launch(&here, &open_last(), &opened(&[&lost])),
            BareLaunch::Picker,
            "it would open on a page that only says it is broken"
        );
    }
}
