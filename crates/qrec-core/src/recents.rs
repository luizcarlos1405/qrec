//! The recent projects (SPEC §6.4): the list behind the project picker and
//! `projects.open_last`, kept in `~/.local/state/qrec/recent-projects.json`
//! (A§9.10). Owner: meta.
//!
//! It is a history the app writes, not a setting, so it isn't in
//! `config.toml`. Losing it loses nothing but the list: a file that can't
//! be read counts as empty. The shell reads and writes the file
//! (`qrec_shell::projects`); `opened` gets its time from the caller.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The most projects the list keeps.
pub const MAX_RECENT: usize = 50;

/// A project the window opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentProject {
    /// Its canonical folder.
    pub dir: String,
    pub opened_at: DateTime<Utc>,
}

/// The list, most recently opened first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentProjects {
    pub projects: Vec<RecentProject>,
}

impl RecentProjects {
    /// Reads the file's text.
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        // A list of strings and dates always serializes.
        let mut text = serde_json::to_string_pretty(self).unwrap_or_default();
        text.push('\n');
        text
    }

    /// `dir` was opened now: it goes first, once, and the oldest beyond
    /// [`MAX_RECENT`] drop off.
    pub fn opened(&mut self, dir: &str, now: DateTime<Utc>) {
        self.forget(dir);
        self.projects.insert(
            0,
            RecentProject {
                dir: dir.to_owned(),
                opened_at: now,
            },
        );
        self.projects.truncate(MAX_RECENT);
    }

    /// Takes `dir` off the list. Only the list: the folder stays as it is.
    pub fn forget(&mut self, dir: &str) {
        self.projects.retain(|p| p.dir != dir);
    }

    /// The project opened last.
    pub fn last(&self) -> Option<&str> {
        self.projects.first().map(|p| p.dir.as_str())
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use proptest::prelude::*;

    use super::*;

    fn at(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 9, 10, 0, second).unwrap()
    }

    #[test]
    fn the_latest_goes_first_once() {
        let mut list = RecentProjects::default();
        assert_eq!(list.last(), None);
        list.opened("/a", at(1));
        list.opened("/b", at(2));
        list.opened("/a", at(3));
        let dirs: Vec<_> = list.projects.iter().map(|p| p.dir.as_str()).collect();
        assert_eq!(dirs, ["/a", "/b"]);
        assert_eq!(list.last(), Some("/a"));
        assert_eq!(list.projects[0].opened_at, at(3));
    }

    #[test]
    fn forgetting_leaves_the_others() {
        let mut list = RecentProjects::default();
        list.opened("/a", at(1));
        list.opened("/b", at(2));
        list.forget("/b");
        list.forget("/never-there");
        assert_eq!(list.last(), Some("/a"));
        assert_eq!(list.projects.len(), 1);
    }

    #[test]
    fn the_file_is_camel_case_and_round_trips() {
        let mut list = RecentProjects::default();
        list.opened("/a", at(1));
        let text = list.to_text();
        assert!(text.contains("\"openedAt\""), "{text}");
        assert_eq!(RecentProjects::parse(&text).unwrap(), list);
        assert!(RecentProjects::parse("not json").is_err());
    }

    proptest! {
        #[test]
        fn opening_keeps_the_list_short_unique_and_latest_first(
            dirs in prop::collection::vec("/[a-z]{1,2}", 0..150),
        ) {
            let mut list = RecentProjects::default();
            for dir in &dirs {
                list.opened(dir, at(0));
                prop_assert_eq!(list.last(), Some(dir.as_str()));
            }
            prop_assert!(list.projects.len() <= MAX_RECENT);
            let mut seen: Vec<&str> = list.projects.iter().map(|p| p.dir.as_str()).collect();
            seen.sort_unstable();
            let count = seen.len();
            seen.dedup();
            prop_assert_eq!(seen.len(), count);
        }
    }
}
