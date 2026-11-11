//! The project folders (SPEC §4.4, §6.4): the named folders of
//! `config.toml` a new project given by name goes in, and which one is the
//! default. Owner: meta.
//!
//! ```toml
//! [projects]
//! default_folder = "work"
//!
//! [[projects.folders]]
//! name = "work"
//! path = "/mnt/videos/work"
//! ```
//!
//! [`problems`] is what reading the file checks; [`apply`] makes one
//! [`FolderEdit`] (what `qrec folders`, Settings › Project and the picker
//! do) and [`edit_text`] writes it into the file's text, every other
//! section as it was. Whether a path is a folder is the shell's question:
//! nothing here looks at the disk.

use serde::{Deserialize, Serialize};

use crate::config::{self, ProjectFolder, ProjectsDefaults};
use crate::error::CoreError;

/// The longest name, in characters.
pub const MAX_NAME_CHARS: usize = 40;

/// What a folder is called when it came from the single `projects.folder`
/// of before, or when added without a name (the picker's "Add folder…"):
/// the path's last part, `projects` when that is no usable name.
pub fn name_for_path(path: &str) -> String {
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let name: String = last.chars().take(MAX_NAME_CHARS).collect();
    match check_name(&name) {
        Ok(name) => name,
        Err(_) => "projects".to_owned(),
    }
}

/// [`name_for_path`], made different from every name in `projects` with a
/// number (`videos 2`), for a folder added without one.
pub fn unused_name_for_path(projects: &ProjectsDefaults, path: &str) -> String {
    let base = name_for_path(path);
    if projects.find(&base).is_none() {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!(" {n}");
            let room = MAX_NAME_CHARS - suffix.chars().count();
            format!("{}{suffix}", base.chars().take(room).collect::<String>())
        })
        .find(|name| projects.find(name).is_none())
        .unwrap_or(base)
}

/// A name as it is kept: trimmed, 1 to [`MAX_NAME_CHARS`] characters,
/// without `/` or control characters.
pub fn check_name(name: &str) -> Result<String, CoreError> {
    let trimmed = name.trim();
    let bad = |why: &str| {
        CoreError::usage(format!("`{trimmed}` can't name a project folder: {why}")).with_hint(
            format!("use 1 to {MAX_NAME_CHARS} characters, without `/`, such as work or Clients"),
        )
    };
    if trimmed.is_empty() {
        return Err(
            CoreError::usage("a project folder needs a name").with_hint("such as work or Clients")
        );
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(bad(&format!("it is over {MAX_NAME_CHARS} characters")));
    }
    if trimmed.contains('/') {
        return Err(bad("it has a slash"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(bad("it has a control character"));
    }
    Ok(trimmed.to_owned())
}

/// A path as it is kept: absolute, without a trailing slash (but `/`).
pub fn check_path(path: &str) -> Result<String, CoreError> {
    if !path.starts_with('/') {
        return Err(CoreError::usage(format!(
            "a project folder's path must be absolute (got {path:?})"
        ))
        .with_hint("such as /mnt/videos/work"));
    }
    let trimmed = path.trim_end_matches('/');
    Ok(if trimmed.is_empty() { "/" } else { trimmed }.to_owned())
}

/// Everything wrong with the project folders of a config (what
/// [`config::parse`] refuses): bad names or paths, a name or path twice, a
/// default that names no folder.
pub fn problems(projects: &ProjectsDefaults) -> Vec<String> {
    let mut out = Vec::new();
    for (i, folder) in projects.folders.iter().enumerate() {
        let at = format!("projects.folders[{i}]");
        if let Err(e) = check_name(&folder.name) {
            out.push(format!("{at}: {}", e.message));
        } else if folder.name != folder.name.trim() {
            out.push(format!("{at}: the name has spaces around it"));
        }
        if !folder.path.starts_with('/') {
            out.push(format!(
                "{at}: the path must be absolute (got {:?})",
                folder.path
            ));
        }
        let earlier = &projects.folders[..i];
        if earlier
            .iter()
            .any(|f| f.name.trim().to_lowercase() == folder.name.trim().to_lowercase())
        {
            out.push(format!(
                "{at}: `{}` is the name of two folders",
                folder.name
            ));
        }
        if earlier.iter().any(|f| same_path(&f.path, &folder.path)) {
            out.push(format!("{at}: {} is listed twice", folder.path));
        }
    }
    if let Some(name) = &projects.default_folder
        && projects.find(name).is_none()
    {
        out.push(format!(
            "projects.default_folder: `{name}` is none of the project folders"
        ));
    }
    out
}

fn same_path(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches('/');
    let b = b.trim_end_matches('/');
    a == b
}

/// One change to the project folders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "edit")]
pub enum FolderEdit {
    /// Add a folder (the default when it is the first, or `make_default`).
    /// The shell has checked that `path` is a folder.
    Add {
        name: String,
        path: String,
        make_default: bool,
    },
    Rename {
        name: String,
        new_name: String,
    },
    SetDefault {
        name: String,
    },
    /// Forget a folder; the folder on disk is untouched. The first one
    /// left becomes the default when this one was.
    Remove {
        name: String,
    },
}

/// The project folders after `edit`.
pub fn apply(
    projects: &ProjectsDefaults,
    edit: &FolderEdit,
) -> Result<ProjectsDefaults, CoreError> {
    let mut out = projects.clone();
    // The default is always written, so the first folder's being the
    // default never depends on the order of the list.
    out.default_folder = projects.default_folder().map(|f| f.name.clone());
    let index_of = |name: &str| {
        let name = name.trim().to_lowercase();
        projects
            .folders
            .iter()
            .position(|f| f.name.to_lowercase() == name)
            .ok_or_else(|| unknown(projects, name.as_str()))
    };
    let taken = |name: &str, except: Option<usize>| {
        let lower = name.to_lowercase();
        let clash = projects
            .folders
            .iter()
            .enumerate()
            .any(|(i, f)| Some(i) != except && f.name.to_lowercase() == lower);
        if clash {
            Err(
                CoreError::invalid(format!("there is a project folder called `{name}` already"))
                    .with_hint("choose another name"),
            )
        } else {
            Ok(())
        }
    };
    match edit {
        FolderEdit::Add {
            name,
            path,
            make_default,
        } => {
            let name = check_name(name)?;
            let path = check_path(path)?;
            taken(&name, None)?;
            if let Some(there) = projects.folders.iter().find(|f| same_path(&f.path, &path)) {
                return Err(CoreError::invalid(format!(
                    "{path} is the project folder `{}` already",
                    there.name
                ))
                .with_hint(format!(
                    "rename it with `qrec folders rename {} NEW`",
                    there.name
                )));
            }
            if *make_default || out.folders.is_empty() {
                out.default_folder = Some(name.clone());
            }
            out.folders.push(ProjectFolder { name, path });
        }
        FolderEdit::Rename { name, new_name } => {
            let i = index_of(name)?;
            let new_name = check_name(new_name)?;
            taken(&new_name, Some(i))?;
            let old = std::mem::replace(&mut out.folders[i].name, new_name.clone());
            if out.default_folder.as_deref() == Some(old.as_str()) {
                out.default_folder = Some(new_name);
            }
        }
        FolderEdit::SetDefault { name } => {
            let i = index_of(name)?;
            out.default_folder = Some(out.folders[i].name.clone());
        }
        FolderEdit::Remove { name } => {
            let i = index_of(name)?;
            let gone = out.folders.remove(i);
            if out.default_folder.as_deref() == Some(gone.name.as_str()) {
                out.default_folder = out.folders.first().map(|f| f.name.clone());
            }
        }
    }
    Ok(out)
}

/// No folder is called `name`: `NotFound`, with the names there are.
fn unknown(projects: &ProjectsDefaults, name: &str) -> CoreError {
    CoreError::not_found(format!("no project folder is called `{name}`"))
        .with_hint(folders_hint(projects))
}

/// The names there are, or how to add one.
pub fn folders_hint(projects: &ProjectsDefaults) -> String {
    if projects.folders.is_empty() {
        "there are no project folders yet: `qrec folders add NAME /path/to/projects`".to_owned()
    } else {
        let names: Vec<&str> = projects.folders.iter().map(|f| f.name.as_str()).collect();
        format!(
            "the project folders: {} (`qrec folders` lists them)",
            names.join(", ")
        )
    }
}

/// `config.toml`'s text with `edit` made, and the folders it holds now.
/// Only `[projects]`'s `folder`, `default_folder` and `folders` change: the
/// old single `folder` goes, the list is written whole. A file that can't
/// be read as settings is refused (fix it first).
pub fn edit_text(text: &str, edit: &FolderEdit) -> Result<(String, ProjectsDefaults), CoreError> {
    let current = config::parse(text)?;
    let projects = apply(&current.projects, edit)?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
        CoreError::invalid(format!(
            "config.toml is not valid TOML: {}",
            e.message().trim()
        ))
    })?;
    let by_hand = || {
        CoreError::usage("[projects] in config.toml isn't a table Qrec can add folders to")
            .with_hint("write it as a [projects] section, or change it by hand")
    };
    let root = doc.as_table_mut();
    let section = root
        .entry("projects")
        .or_insert_with(toml_edit::table)
        .as_table_mut()
        .ok_or_else(by_hand)?;
    section.remove("folder");
    match &projects.default_folder {
        Some(name) => match section.get_mut("default_folder") {
            Some(item) if item.is_value() => {
                let decor = item.as_value().map(|v| v.decor().clone());
                let mut value = toml_edit::Value::from(name.as_str());
                if let Some(decor) = decor {
                    *value.decor_mut() = decor;
                }
                *item = toml_edit::Item::Value(value);
            }
            _ => {
                section.insert("default_folder", toml_edit::value(name.as_str()));
            }
        },
        None => {
            section.remove("default_folder");
        }
    }
    section.remove("folders");
    if !projects.folders.is_empty() {
        let mut list = toml_edit::ArrayOfTables::new();
        for folder in &projects.folders {
            let mut table = toml_edit::Table::new();
            table.insert("name", toml_edit::value(folder.name.as_str()));
            table.insert("path", toml_edit::value(folder.path.as_str()));
            list.push(table);
        }
        section.insert("folders", toml_edit::Item::ArrayOfTables(list));
    }
    let edited = crate::toml_text::with_newlines_of(text, doc.to_string());
    let read_back = config::parse(&edited)?;
    if read_back.projects != projects {
        return Err(CoreError::internal(
            "the project folders didn't read back as written",
        ));
    }
    Ok((edited, projects))
}

/// The edit that makes the folder at `path` the default: that one when it
/// is listed already, else a new one named after it ([`unused_name_for_path`]).
/// What choosing a folder in the picker or Settings does.
pub fn make_default_path(projects: &ProjectsDefaults, path: &str) -> FolderEdit {
    match projects.folders.iter().find(|f| same_path(&f.path, path)) {
        Some(folder) => FolderEdit::SetDefault {
            name: folder.name.clone(),
        },
        None => FolderEdit::Add {
            name: unused_name_for_path(projects, path),
            path: path.to_owned(),
            make_default: true,
        },
    }
}

/// One project folder as `qrec folders --json` and the GUI show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRow {
    pub name: String,
    pub path: String,
    pub default: bool,
}

/// The folders in order, the default one marked.
pub fn rows(projects: &ProjectsDefaults) -> Vec<FolderRow> {
    let default = projects.default_folder().map(|f| f.name.as_str());
    projects
        .folders
        .iter()
        .map(|f| FolderRow {
            name: f.name.clone(),
            path: f.path.clone(),
            default: Some(f.name.as_str()) == default,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    fn add(name: &str, path: &str) -> FolderEdit {
        FolderEdit::Add {
            name: name.into(),
            path: path.into(),
            make_default: false,
        }
    }

    fn edits(text: &str, list: &[FolderEdit]) -> (String, ProjectsDefaults) {
        let mut text = text.to_owned();
        let mut projects = ProjectsDefaults::default();
        for edit in list {
            (text, projects) = edit_text(&text, edit).unwrap();
        }
        (text, projects)
    }

    fn names(projects: &ProjectsDefaults) -> Vec<(&str, bool)> {
        let default = projects.default_folder().map(|f| f.name.as_str());
        projects
            .folders
            .iter()
            .map(|f| (f.name.as_str(), Some(f.name.as_str()) == default))
            .collect()
    }

    #[test]
    fn the_first_folder_is_the_default_until_another_is_chosen() {
        let (text, projects) = edits("", &[add("work", "/srv/work"), add("home", "/srv/home/")]);
        assert_eq!(names(&projects), [("work", true), ("home", false)]);
        assert_eq!(projects.folders[1].path, "/srv/home", "no trailing slash");
        insta::assert_snapshot!(text, @r#"
        [projects]
        default_folder = "work"

        [[projects.folders]]
        name = "work"
        path = "/srv/work"

        [[projects.folders]]
        name = "home"
        path = "/srv/home"
        "#);
        let (_, projects) = edit_text(
            &text,
            &FolderEdit::SetDefault {
                name: "HOME".into(),
            },
        )
        .unwrap();
        assert_eq!(names(&projects), [("work", false), ("home", true)]);
        let made = FolderEdit::Add {
            name: "clients".into(),
            path: "/srv/clients".into(),
            make_default: true,
        };
        let (_, projects) = edit_text(&text, &made).unwrap();
        assert_eq!(names(&projects)[2], ("clients", true));
    }

    #[test]
    fn removing_the_default_makes_the_first_one_left_the_default() {
        let (text, _) = edits(
            "",
            &[
                add("a", "/srv/a"),
                add("b", "/srv/b"),
                add("c", "/srv/c"),
                FolderEdit::SetDefault { name: "b".into() },
            ],
        );
        let (text, projects) = edit_text(&text, &FolderEdit::Remove { name: "b".into() }).unwrap();
        assert_eq!(names(&projects), [("a", true), ("c", false)]);
        let (text, projects) = edit_text(&text, &FolderEdit::Remove { name: "c".into() }).unwrap();
        assert_eq!(names(&projects), [("a", true)]);
        let (text, projects) = edit_text(&text, &FolderEdit::Remove { name: "a".into() }).unwrap();
        assert!(projects.folders.is_empty());
        assert_eq!(projects.default_folder, None);
        assert_eq!(text.trim(), "[projects]");
    }

    #[test]
    fn renaming_the_default_keeps_it_the_default() {
        let (text, _) = edits("", &[add("a", "/srv/a"), add("b", "/srv/b")]);
        let rename = FolderEdit::Rename {
            name: "a".into(),
            new_name: " Clients ".into(),
        };
        let (_, projects) = edit_text(&text, &rename).unwrap();
        assert_eq!(names(&projects), [("Clients", true), ("b", false)]);
        // Its own name in another case is fine.
        let recase = FolderEdit::Rename {
            name: "b".into(),
            new_name: "B".into(),
        };
        assert_eq!(
            names(&edit_text(&text, &recase).unwrap().1)[1],
            ("B", false)
        );
    }

    #[test]
    fn bad_edits_change_nothing() {
        let (text, _) = edits("", &[add("work", "/srv/work")]);
        let cases = [
            (add("Work", "/srv/other"), ErrorCode::Invalid, "already"),
            (add("other", "/srv/work/"), ErrorCode::Invalid, "already"),
            (add("other", "videos"), ErrorCode::Usage, "absolute"),
            (add("a/b", "/srv/ab"), ErrorCode::Usage, "slash"),
            (add("  ", "/srv/ab"), ErrorCode::Usage, "needs a name"),
            (add(&"x".repeat(41), "/srv/ab"), ErrorCode::Usage, "40"),
            (add("tab\there", "/srv/ab"), ErrorCode::Usage, "control"),
            (
                FolderEdit::Remove {
                    name: "nope".into(),
                },
                ErrorCode::NotFound,
                "nope",
            ),
            (
                FolderEdit::SetDefault {
                    name: "nope".into(),
                },
                ErrorCode::NotFound,
                "nope",
            ),
            (
                FolderEdit::Rename {
                    name: "nope".into(),
                    new_name: "x".into(),
                },
                ErrorCode::NotFound,
                "nope",
            ),
        ];
        for (edit, code, says) in cases {
            let error = edit_text(&text, &edit).unwrap_err();
            assert_eq!(error.code, code, "{edit:?}");
            assert!(error.message.contains(says), "{edit:?}: {error:?}");
        }
        let unknown = edit_text(
            &text,
            &FolderEdit::Remove {
                name: "nope".into(),
            },
        )
        .unwrap_err();
        assert!(unknown.hint.unwrap().contains("work"));
    }

    #[test]
    fn the_old_single_folder_is_saved_in_the_new_shape() {
        let old = "# mine\ntheme = \"swamp-day\"\n\n[projects]\nfolder = \"/srv/videos\" # here\nopen_last = true\n";
        let (text, projects) = edit_text(old, &add("work", "/srv/work")).unwrap();
        assert_eq!(names(&projects), [("videos", true), ("work", false)]);
        insta::assert_snapshot!(text, @r#"
        # mine
        theme = "swamp-day"

        [projects]
        open_last = true
        default_folder = "videos"

        [[projects.folders]]
        name = "videos"
        path = "/srv/videos"

        [[projects.folders]]
        name = "work"
        path = "/srv/work"
        "#);
    }

    #[test]
    fn hand_edits_are_checked() {
        let parse = |text: &str| config::parse(text).map(|c| c.projects);
        let folder = |name: &str, path: &str| {
            format!("[[projects.folders]]\nname = \"{name}\"\npath = \"{path}\"\n")
        };
        let twice = format!("{}{}", folder("a", "/srv/a"), folder("A", "/srv/b"));
        assert!(parse(&twice).unwrap_err().message.contains("two folders"));
        let same = format!("{}{}", folder("a", "/srv/a"), folder("b", "/srv/a/"));
        assert!(parse(&same).unwrap_err().message.contains("twice"));
        assert!(
            parse(&folder("a", "srv"))
                .unwrap_err()
                .message
                .contains("absolute")
        );
        let missing = format!(
            "[projects]\ndefault_folder = \"b\"\n{}",
            folder("a", "/srv/a")
        );
        assert!(parse(&missing).unwrap_err().message.contains("none of"));
        // No default written: the first one.
        let two = format!("{}{}", folder("a", "/srv/a"), folder("b", "/srv/b"));
        let projects = parse(&two).unwrap();
        assert_eq!(names(&projects), [("a", true), ("b", false)]);
    }

    #[test]
    fn names_for_paths() {
        assert_eq!(name_for_path("/srv/videos/"), "videos");
        assert_eq!(name_for_path("/"), "projects");
        let mut projects = ProjectsDefaults::default();
        assert_eq!(unused_name_for_path(&projects, "/a/videos"), "videos");
        projects = apply(&projects, &add("videos", "/a/videos")).unwrap();
        projects = apply(&projects, &add("videos 2", "/b/videos")).unwrap();
        assert_eq!(unused_name_for_path(&projects, "/c/videos"), "videos 3");
        let long = format!("/{}", "x".repeat(60));
        assert_eq!(name_for_path(&long).chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn choosing_a_path_makes_it_the_default() {
        let (_, projects) = edits("", &[add("a", "/srv/a"), add("b", "/srv/b")]);
        assert_eq!(
            make_default_path(&projects, "/srv/b/"),
            FolderEdit::SetDefault { name: "b".into() }
        );
        let new = make_default_path(&projects, "/mnt/a");
        assert_eq!(
            new,
            FolderEdit::Add {
                name: "a 2".into(),
                path: "/mnt/a".into(),
                make_default: true
            }
        );
        let projects = apply(&projects, &new).unwrap();
        assert_eq!(names(&projects)[2], ("a 2", true));
    }

    #[test]
    fn rows_mark_the_default() {
        let (_, projects) = edits("", &[add("a", "/srv/a"), add("b", "/srv/b")]);
        let rows = rows(&projects);
        assert!(rows[0].default && !rows[1].default);
        insta::assert_json_snapshot!(rows[0], @r#"
        {
          "name": "a",
          "path": "/srv/a",
          "default": true
        }
        "#);
    }
}
