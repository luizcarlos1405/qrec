//! Where a new project goes (SPEC §4.4): the one rule behind `qrec create`
//! and the project picker's New project field. Owner: meta.
//!
//! What was typed is a **name** or a **path**, and a slash is what tells
//! them apart:
//!
//! - A name (`demo`) goes in the projects folder of `config.toml`
//!   (`projects.folder`). With none set it is refused: Qrec has no default
//!   place to put things.
//! - A path (`./demo`, `../demo`, `/mnt/videos/demo`) is used as it is,
//!   against the current directory when it is relative.
//!
//! Either way only the last part is made; the folder it goes in must exist
//! already, which the shell checks (`qrec_shell::projects::make_folder`).
//! Everything here is about the text: nothing looks at the disk.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::error::CoreError;

/// Where a new project goes, before anything on disk is looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProjectPath {
    /// The folder it goes in, absolute. It must exist.
    pub parent: PathBuf,
    /// The folder to make there: the project's default name too.
    pub name: OsString,
}

impl NewProjectPath {
    /// The project's folder.
    pub fn dir(&self) -> PathBuf {
        self.parent.join(&self.name)
    }
}

/// Whether `target` is a path rather than a name: it has a slash.
pub fn is_path(target: &OsStr) -> bool {
    target.as_bytes().contains(&b'/')
}

/// Applies the rule to what was typed. `cwd` makes a relative path absolute
/// (`None`: the current directory can't be read, which only matters for
/// one); `projects_folder` is `projects.folder`.
pub fn resolve(
    target: &OsStr,
    cwd: Option<&Path>,
    projects_folder: Option<&str>,
) -> Result<NewProjectPath, CoreError> {
    let shown = target.to_string_lossy();
    if target.is_empty() {
        return Err(CoreError::usage("the new project needs a name")
            .with_hint("give a name, or a path such as ./demo"));
    }
    if !is_path(target) {
        if target == "." || target == ".." {
            return Err(CoreError::usage(format!("`{shown}` isn't a project name"))
                .with_hint("give a name, or a path that ends with one, such as ./demo"));
        }
        let Some(parent) = projects_folder else {
            return Err(no_projects_folder(&shown));
        };
        return Ok(NewProjectPath {
            parent: PathBuf::from(parent),
            name: target.to_owned(),
        });
    }
    let path = Path::new(target);
    let absolute = match (path.is_absolute(), cwd) {
        (true, _) => path.to_path_buf(),
        (false, Some(cwd)) => cwd.join(path),
        (false, None) => {
            return Err(CoreError::io(format!(
                "{shown} is relative, and the current directory can't be read (was it deleted?)"
            ))
            .with_hint("pass an absolute path, or cd into an existing folder"));
        }
    };
    // `file_name` is `None` for `/` and for a path ending in `..`.
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => Ok(NewProjectPath {
            parent: parent.to_path_buf(),
            name: name.to_owned(),
        }),
        _ => Err(
            CoreError::usage(format!("`{shown}` doesn't end with a folder name"))
                .with_hint("end the path with the new project's folder, such as ./demo"),
        ),
    }
}

/// A name with no projects folder to put it in.
fn no_projects_folder(name: &str) -> CoreError {
    CoreError::usage(format!(
        "no projects folder is set, so `{name}` has nowhere to go"
    ))
    .with_hint(format!(
        "give a path (./{name} makes it here), or set the folder: \
         `qrec config set projects.folder /path/to/projects`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    const CWD: &str = "/home/shrek/swamp";

    fn run(target: &str, folder: Option<&str>) -> Result<NewProjectPath, CoreError> {
        resolve(OsStr::new(target), Some(Path::new(CWD)), folder)
    }

    fn at(parent: &str, name: &str) -> NewProjectPath {
        NewProjectPath {
            parent: parent.into(),
            name: name.into(),
        }
    }

    #[test]
    fn a_name_goes_in_the_projects_folder() {
        assert_eq!(
            run("demo", Some("/srv/qrec")).unwrap(),
            at("/srv/qrec", "demo")
        );
        assert_eq!(
            run("demo", Some("/srv/qrec")).unwrap().dir(),
            Path::new("/srv/qrec/demo")
        );
    }

    #[test]
    fn a_name_without_a_projects_folder_is_refused() {
        let error = run("demo", None).unwrap_err();
        assert_eq!(error.code, ErrorCode::Usage);
        assert!(error.message.contains("no projects folder"), "{error:?}");
        let hint = error.hint.unwrap();
        assert!(hint.contains("./demo"), "{hint}");
        assert!(hint.contains("projects.folder"), "{hint}");
    }

    #[test]
    fn a_slash_makes_it_a_path_and_the_folder_setting_is_ignored() {
        let cases = [
            ("./demo", CWD, "demo"),
            ("sub/demo", "/home/shrek/swamp/sub", "demo"),
            ("/mnt/videos/demo", "/mnt/videos", "demo"),
            ("/mnt/videos/demo/", "/mnt/videos", "demo"),
            ("/mnt/videos/demo/.", "/mnt/videos", "demo"),
            ("../demo", "/home/shrek/swamp/..", "demo"),
        ];
        for (target, parent, name) in cases {
            assert_eq!(run(target, None).unwrap(), at(parent, name), "{target}");
            assert_eq!(
                run(target, Some("/srv/qrec")).unwrap(),
                at(parent, name),
                "{target} with a folder set"
            );
        }
    }

    #[test]
    fn what_isnt_a_folder_name_is_refused() {
        for target in ["", ".", "..", "/", "./..", "demo/..", "/mnt/.."] {
            let error = run(target, Some("/srv/qrec")).unwrap_err();
            assert_eq!(error.code, ErrorCode::Usage, "{target}: {error:?}");
        }
    }

    #[test]
    fn only_a_relative_path_needs_the_current_directory() {
        let gone = |target: &str, folder| resolve(OsStr::new(target), None, folder);
        assert_eq!(
            gone("/mnt/demo", None).unwrap(),
            at("/mnt", "demo"),
            "absolute"
        );
        assert_eq!(
            gone("demo", Some("/srv")).unwrap(),
            at("/srv", "demo"),
            "a name"
        );
        assert_eq!(gone("./demo", None).unwrap_err().code, ErrorCode::Io);
    }

    #[test]
    fn a_folder_that_isnt_utf8_works() {
        let target = OsStr::from_bytes(b"./d\xffmo");
        let path = resolve(target, Some(Path::new(CWD)), None).unwrap();
        assert_eq!(path.parent, Path::new(CWD));
        assert_eq!(path.name, OsStr::from_bytes(b"d\xffmo"));
    }

    #[test]
    fn is_path_is_about_the_slash() {
        assert!(!is_path(OsStr::new("demo")));
        assert!(!is_path(OsStr::new("demo.v2")));
        assert!(is_path(OsStr::new("./demo")));
        assert!(is_path(OsStr::new("a/b")));
        assert!(is_path(OsStr::new("/a")));
    }
}
