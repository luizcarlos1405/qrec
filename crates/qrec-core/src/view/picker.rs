//! The project picker (SPEC §4.4, G§4.12): the rows it lists and what the
//! New project field says about what was typed. Owner: meta.
//!
//! The rows are the recent list, most recent first, or the ones a search
//! matches, best first (the folder's name counts for more than the path
//! around it). What a folder is on disk comes from the caller: the window
//! finds out on a worker, so a folder on a slow mount can't hold it up.

use std::cmp::Reverse;
use std::ffi::OsStr;
use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::config::{ProjectFolder, ProjectsDefaults};
use crate::new_project;
use crate::recents::RecentProjects;
use crate::view::format;
use crate::view::palette::fuzzy_match;

/// What the picker knows about a recent project's folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FolderState {
    /// Not looked at yet.
    Checking,
    /// It can be opened.
    Ready,
    /// Moved, deleted, or no longer a project.
    Gone,
}

/// One project in the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickerRow {
    /// The folder, as the list holds it.
    pub dir: String,
    /// The folder's name.
    pub name: String,
    /// The folder as people read it (`~/…`).
    pub path: String,
    /// "Today at 15:02", or why it can't be opened.
    pub detail: String,
    /// The characters of `name` the search matched (indices in chars), to
    /// show in bold; empty without a search or when only the path matched.
    pub matches: Vec<usize>,
    pub state: FolderState,
}

impl PickerRow {
    /// Pressing the row opens it. A folder still being looked at can be
    /// tried: opening checks it again.
    pub fn openable(&self) -> bool {
        self.state != FolderState::Gone
    }
}

/// What a row says about a folder that can't be opened.
pub const GONE: &str = "Folder not found";

/// The rows for `query` (empty: all of them).
pub fn rows(
    recents: &RecentProjects,
    state_of: impl Fn(&str) -> FolderState,
    query: &str,
    home: Option<&str>,
    now: DateTime<FixedOffset>,
) -> Vec<PickerRow> {
    let query = query.trim();
    let mut found: Vec<(u32, PickerRow)> = recents
        .projects
        .iter()
        .filter_map(|project| {
            let name = Path::new(&project.dir)
                .file_name()
                .map_or_else(|| project.dir.clone(), |n| n.to_string_lossy().into_owned());
            let path = format::display_path(&project.dir, "", home);
            let (score, matches) = if query.is_empty() {
                (0, Vec::new())
            } else {
                fuzzy_match(query, &name)
                    .map(|(score, at)| (score + NAME_BONUS, at))
                    .or_else(|| fuzzy_match(query, &path).map(|(score, _)| (score, Vec::new())))?
            };
            let state = state_of(&project.dir);
            let detail = match state {
                FolderState::Gone => GONE.to_owned(),
                _ => format::capitalized(&format::local_when(
                    project.opened_at.with_timezone(now.offset()),
                    now,
                )),
            };
            Some((
                score,
                PickerRow {
                    dir: project.dir.clone(),
                    name,
                    path,
                    detail,
                    matches,
                    state,
                },
            ))
        })
        .collect();
    // Stable: equal scores stay most recent first.
    found.sort_by_key(|(score, _)| Reverse(*score));
    found.into_iter().map(|(_, row)| row).collect()
}

/// Lifts a match in the folder's name over any match in its path.
const NAME_BONUS: u32 = 100_000;

/// What the New project field says about what was typed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum NewProjectHint {
    /// Nothing typed, with no project folders: how the field works.
    Idle { text: String },
    /// Nothing typed: a name goes in the chosen folder (`name`, at `path`).
    GoesIn { name: String, path: String },
    /// Where the project would go; `note` says that a path ignores the
    /// folder menu (when there is one).
    Creates { path: String, note: Option<String> },
    /// A name, with no project folders to put it in.
    NeedsFolder { text: String },
    /// It can't be made.
    Problem { text: String },
}

impl NewProjectHint {
    /// Create can be pressed.
    pub fn can_create(&self) -> bool {
        matches!(self, NewProjectHint::Creates { .. })
    }
}

/// The folder a name goes in: the one `chosen` names, else the default
/// (a `chosen` that is gone, say renamed meanwhile, falls back too).
pub fn chosen_folder<'a>(
    projects: &'a ProjectsDefaults,
    chosen: Option<&str>,
) -> Option<&'a ProjectFolder> {
    chosen
        .and_then(|name| projects.find(name))
        .or_else(|| projects.default_folder())
}

/// The hint for `typed`. `cwd` as in [`new_project::resolve`]; `folder` is
/// where a name goes ([`chosen_folder`]), `has_folders` whether there are
/// project folders at all. Nothing here looks at the disk.
pub fn new_project_hint(
    typed: &str,
    cwd: Option<&Path>,
    folder: Option<&ProjectFolder>,
    home: Option<&str>,
) -> NewProjectHint {
    let typed = typed.trim();
    let shown = |path: &str| format::display_path(path, "", home);
    let none_yet = |example: &str| {
        format!("There are no project folders yet. Add one, or type a path such as ./{example}.")
    };
    if typed.is_empty() {
        return match folder {
            Some(folder) => NewProjectHint::GoesIn {
                name: folder.name.clone(),
                path: shown(&folder.path),
            },
            None => NewProjectHint::Idle {
                text: none_yet("demo"),
            },
        };
    }
    let target = OsStr::new(typed);
    let is_path = new_project::is_path(target);
    if folder.is_none() && !is_path && typed != "." && typed != ".." {
        return NewProjectHint::NeedsFolder {
            text: none_yet(typed),
        };
    }
    match new_project::resolve(target, cwd, folder.map(|f| f.path.as_str())) {
        Ok(path) => NewProjectHint::Creates {
            path: shown(&path.dir().to_string_lossy()),
            note: (is_path && folder.is_some())
                .then(|| "A path goes where it says, not in a project folder.".to_owned()),
        },
        Err(e) => NewProjectHint::Problem {
            text: format::capitalized(&e.message),
        },
    }
}

/// The New project page's folder menu (design/boards/09-project-folders).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderMenuView {
    /// The chosen folder's name, what the button shows after "in"; `None`
    /// when there are no folders (no button, the "Add a project folder…"
    /// link instead).
    pub chosen: Option<String>,
    /// For screen readers: "Project folder: work".
    pub accessible: String,
    pub rows: Vec<FolderMenuRow>,
}

/// One folder in the menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderMenuRow {
    pub name: String,
    /// The path as people read it (`~/Videos/work`).
    pub path: String,
    /// A name goes here now (the check).
    pub chosen: bool,
    /// The default folder (its badge; the others offer Make default).
    pub default: bool,
}

/// The menu for `projects` with `chosen` picked ([`chosen_folder`]).
pub fn folder_menu(
    projects: &ProjectsDefaults,
    chosen: Option<&str>,
    home: Option<&str>,
) -> FolderMenuView {
    let picked = chosen_folder(projects, chosen).map(|f| f.name.clone());
    let default = projects.default_folder().map(|f| f.name.clone());
    FolderMenuView {
        accessible: match &picked {
            Some(name) => format!("Project folder: {name}"),
            None => "No project folders".to_owned(),
        },
        rows: projects
            .folders
            .iter()
            .map(|f| FolderMenuRow {
                name: f.name.clone(),
                path: format::display_path(&f.path, "", home),
                chosen: picked.as_deref() == Some(f.name.as_str()),
                default: default.as_deref() == Some(f.name.as_str()),
            })
            .collect(),
        chosen: picked,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    const HOME: Option<&str> = Some("/home/shrek");

    fn now() -> DateTime<FixedOffset> {
        Utc.with_ymd_and_hms(2026, 11, 9, 18, 0, 0)
            .unwrap()
            .fixed_offset()
    }

    /// `dirs`, the first one opened last.
    fn list(dirs: &[&str]) -> RecentProjects {
        let mut list = RecentProjects::default();
        for dir in dirs.iter().rev() {
            list.opened(dir, Utc.with_ymd_and_hms(2026, 11, 9, 15, 2, 0).unwrap());
        }
        list
    }

    fn names(rows: &[PickerRow]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    fn ready(_: &str) -> FolderState {
        FolderState::Ready
    }

    #[test]
    fn a_row_says_what_it_is() {
        let rows = rows(&list(&["/home/shrek/talks/onions"]), ready, "", HOME, now());
        assert_eq!(
            rows,
            [PickerRow {
                dir: "/home/shrek/talks/onions".into(),
                name: "onions".into(),
                path: "~/talks/onions".into(),
                detail: "Today at 15:02".into(),
                matches: Vec::new(),
                state: FolderState::Ready,
            }]
        );
    }

    #[test]
    fn most_recent_first_without_a_search() {
        let all = rows(
            &list(&["/a/one", "/a/two", "/a/three"]),
            ready,
            "",
            HOME,
            now(),
        );
        assert_eq!(names(&all), ["one", "two", "three"]);
        let spaces = rows(&list(&["/a/one", "/a/two"]), ready, "   ", HOME, now());
        assert_eq!(names(&spaces), ["one", "two"]);
    }

    #[test]
    fn a_search_finds_the_name_before_the_path() {
        let recents = list(&["/onions/layers", "/home/shrek/onions"]);
        let found = rows(&recents, ready, "onions", HOME, now());
        assert_eq!(names(&found), ["onions", "layers"]);
        assert!(rows(&recents, ready, "zzz", HOME, now()).is_empty());
    }

    #[test]
    fn a_search_marks_the_letters_of_the_name_it_matched() {
        let recents = list(&["/talks/rust-traits-talk", "/onions/layers"]);
        let found = rows(&recents, ready, "rst", HOME, now());
        assert_eq!(found[0].matches, [0, 2, 3], "r, s, t of rust");
        // Only the path matched: nothing in the name to mark.
        let by_path = rows(&recents, ready, "onions", HOME, now());
        assert_eq!(names(&by_path), ["layers"]);
        assert!(by_path[0].matches.is_empty());
        assert!(rows(&recents, ready, "", HOME, now())[0].matches.is_empty());
    }

    #[test]
    fn a_folder_that_is_gone_says_so_and_cant_be_opened() {
        let state = |dir: &str| {
            if dir == "/a/gone" {
                FolderState::Gone
            } else {
                FolderState::Checking
            }
        };
        let all = rows(&list(&["/a/gone", "/a/here"]), state, "", HOME, now());
        assert_eq!(all[0].detail, GONE);
        assert!(!all[0].openable());
        assert!(all[1].openable(), "still being looked at");
    }

    fn folder(name: &str, path: &str) -> ProjectFolder {
        ProjectFolder {
            name: name.into(),
            path: path.into(),
        }
    }

    fn hint(typed: &str, path: Option<&str>) -> NewProjectHint {
        let folder = path.map(|p| folder("videos", p));
        new_project_hint(
            typed,
            Some(Path::new("/home/shrek/swamp")),
            folder.as_ref(),
            HOME,
        )
    }

    #[test]
    fn the_hint_follows_the_rule() {
        let creates = |path: &str, note: bool| NewProjectHint::Creates {
            path: path.into(),
            note: note.then(|| "A path goes where it says, not in a project folder.".into()),
        };
        let folder = Some("/home/shrek/videos");
        assert_eq!(hint("demo", folder), creates("~/videos/demo", false));
        assert_eq!(hint("  demo  ", folder), creates("~/videos/demo", false));
        assert_eq!(hint("./demo", folder), creates("~/swamp/demo", true));
        assert_eq!(hint("./demo", None), creates("~/swamp/demo", false));
        assert_eq!(hint("/mnt/demo", None), creates("/mnt/demo", false));
        assert!(hint("/mnt/demo", None).can_create());
    }

    #[test]
    fn a_name_without_a_folder_asks_for_one() {
        let needs = hint("demo", None);
        assert!(!needs.can_create());
        let NewProjectHint::NeedsFolder { text } = needs else {
            panic!("{needs:?}")
        };
        assert!(text.contains("no project folders"), "{text}");
        assert!(text.contains("./demo"), "{text}");
    }

    #[test]
    fn nothing_typed_explains_the_field() {
        assert_eq!(
            hint("", Some("/home/shrek/videos")),
            NewProjectHint::GoesIn {
                name: "videos".into(),
                path: "~/videos".into()
            }
        );
        let NewProjectHint::Idle { text } = hint("  ", None) else {
            panic!("not idle");
        };
        assert!(text.contains("./demo"), "{text}");
        assert!(!hint("", None).can_create());
    }

    #[test]
    fn the_folder_menu_marks_the_chosen_and_the_default() {
        let projects = ProjectsDefaults {
            default_folder: Some("work".into()),
            open_last: false,
            folders: vec![
                folder("work", "/home/shrek/Videos/work"),
                folder("clients", "/srv/clients"),
            ],
        };
        let menu = folder_menu(&projects, None, HOME);
        assert_eq!(menu.chosen.as_deref(), Some("work"));
        assert_eq!(menu.accessible, "Project folder: work");
        assert_eq!(menu.rows[0].path, "~/Videos/work");
        assert!(menu.rows[0].chosen && menu.rows[0].default);
        let menu = folder_menu(&projects, Some("clients"), HOME);
        assert!(!menu.rows[0].chosen && menu.rows[0].default);
        assert!(menu.rows[1].chosen && !menu.rows[1].default);
        // A choice that is gone falls back to the default.
        let menu = folder_menu(&projects, Some("renamed"), HOME);
        assert_eq!(menu.chosen.as_deref(), Some("work"));
        let none = folder_menu(&ProjectsDefaults::default(), None, HOME);
        assert_eq!(none.chosen, None);
        assert!(none.rows.is_empty());
    }

    #[test]
    fn what_isnt_a_folder_name_is_a_problem() {
        for typed in [".", "..", "/", "demo/.."] {
            let problem = hint(typed, Some("/srv"));
            assert!(
                matches!(problem, NewProjectHint::Problem { .. }),
                "{typed}: {problem:?}"
            );
            assert!(!problem.can_create());
        }
        assert!(matches!(hint(".", None), NewProjectHint::Problem { .. }));
    }
}
