//! Blocking pages: what the window shows instead of the editor when the
//! project can't open (G§4.11, G§1.1 #2). Owner: WP0a (interim content),
//! WP8 (the drafted pages, once approved).
//!
//! [`interim`] gives each page SPEC's copy and the buttons SPEC names, as a
//! plain C31 block without art or per-row layout: S23 (dependencies
//! missing), X1 (a take recorded from the CLI), the export wait (D-29), X2
//! (not my swamp, D-14), X3 (`qrec.json` can't be read or is missing), a
//! project held by another window (D-13), a holder that doesn't respond,
//! GStreamer stuck, and the folder or the host failing.

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, NOT_MY_SWAMP};
use crate::view::format::{self, capitalized};

/// Which page, with what it shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "page",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BlockKind {
    /// S23: a required dependency is missing (exit 5). `report` is
    /// `DoctorReport::to_text` of the failed checks.
    MissingDeps { report: String },
    /// X1: another host records a take (SPEC §4.3). `stopping` once Stop &
    /// open was pressed (D-29).
    CliRecording {
        take: u32,
        elapsed_secs: u64,
        stopping: bool,
    },
    /// D-29: an export from the CLI runs. `name` is the output as people
    /// read it (`exports/…`); `percent` from `export_progress_view`.
    ExportWait {
        name: String,
        percent: Option<u8>,
        waiting: bool,
    },
    /// X2: the folder is neither a project nor empty (SPEC §4.4, exit 3).
    /// `inside`: the project it is in, for D-14's extra line.
    NotMySwamp { dir: String, inside: Option<String> },
    /// X3: opening failed on `qrec.json` (an invalid or missing file, exit
    /// 1); the error's hint names the history restore.
    Unreadable { error: CoreError },
    /// D-13: a Qrec window holds the project (exit 6).
    HeldByWindow { pid: u32 },
    /// The process holding the lock doesn't answer (exit 6). `kind` and
    /// `since` as people read them ("headless host", "15:02").
    NotResponding {
        pid: Option<u32>,
        kind: Option<String>,
        since: Option<String>,
    },
    /// GStreamer didn't initialize within 30 s (exit 5).
    GstStuck,
    /// The folder can't be read (exit 1).
    FolderUnreadable { dir: String, error: String },
    /// The host couldn't start or the project couldn't open for another
    /// reason (exit 1).
    HostFailed { error: CoreError },
}

impl BlockKind {
    /// The exit code once the person quits (G§3.2): 3 for X2, 5 for a
    /// missing dependency or GStreamer stuck, 6 while another process holds
    /// the project, 1 otherwise.
    pub fn exit_code(&self) -> u8 {
        match self {
            BlockKind::NotMySwamp { .. } => 3,
            BlockKind::MissingDeps { .. } | BlockKind::GstStuck => 5,
            BlockKind::CliRecording { .. }
            | BlockKind::ExportWait { .. }
            | BlockKind::HeldByWindow { .. }
            | BlockKind::NotResponding { .. } => 6,
            BlockKind::Unreadable { .. }
            | BlockKind::FolderUnreadable { .. }
            | BlockKind::HostFailed { .. } => 1,
        }
    }
}

/// A page's content (C31 Blocking).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockingContent {
    /// The `display` line.
    pub title: String,
    /// Body lines, in order.
    pub lines: Vec<String>,
    /// A monospace block: a folder, a command, a report.
    pub monospace: Option<String>,
    /// Primary first.
    pub buttons: Vec<BlockingButton>,
}

/// One button of a blocking page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockingButton {
    pub label: String,
    pub action: BlockingAction,
    pub primary: bool,
    /// The loading state: a spinner, and the button doesn't react.
    pub busy: bool,
}

/// What a blocking page's button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockingAction {
    /// S23: run the checks again.
    CheckAgain,
    /// X1: stop the other host's recording, then open.
    StopAndOpen,
    /// D-29: wait for the export, then open.
    Wait,
    /// Quit with [`BlockKind::exit_code`].
    Quit,
}

fn button(label: &str, action: BlockingAction, primary: bool) -> BlockingButton {
    BlockingButton {
        label: label.to_owned(),
        action,
        primary,
        busy: false,
    }
}

fn quit() -> BlockingButton {
    button("Quit", BlockingAction::Quit, false)
}

/// The interim page of `kind` (G§4.11).
pub fn interim(kind: &BlockKind) -> BlockingContent {
    let page = |title: &str, lines: Vec<String>, monospace: Option<String>| BlockingContent {
        title: title.to_owned(),
        lines,
        monospace,
        buttons: vec![quit()],
    };
    match kind {
        BlockKind::MissingDeps { report } => BlockingContent {
            title: "The swamp is missing something".to_owned(),
            lines: vec![
                "Qrec can't start until these are installed. Run qrec doctor in a terminal \
                 for the same report."
                    .to_owned(),
            ],
            monospace: Some(report.clone()),
            buttons: vec![
                button("Check again", BlockingAction::CheckAgain, true),
                quit(),
            ],
        },
        BlockKind::CliRecording {
            take,
            elapsed_secs,
            stopping,
        } => BlockingContent {
            title: "A take is being recorded from the CLI.".to_owned(),
            lines: vec![format!(
                "Take {take} started from the terminal {} ago. Stop it to open the project \
                 here. Quit leaves it recording.",
                format::hms(*elapsed_secs)
            )],
            monospace: None,
            buttons: vec![
                BlockingButton {
                    busy: *stopping,
                    ..button(
                        if *stopping {
                            "Stopping…"
                        } else {
                            "Stop & open"
                        },
                        BlockingAction::StopAndOpen,
                        true,
                    )
                },
                quit(),
            ],
        },
        BlockKind::ExportWait {
            name,
            percent,
            waiting,
        } => BlockingContent {
            title: "An export from the CLI is running.".to_owned(),
            lines: vec![match percent {
                Some(p) => format!("{name} \u{00b7} {p} %"),
                None => name.clone(),
            }],
            monospace: None,
            buttons: vec![
                BlockingButton {
                    busy: *waiting,
                    ..button(
                        if *waiting { "Waiting…" } else { "Wait" },
                        BlockingAction::Wait,
                        true,
                    )
                },
                quit(),
            ],
        },
        BlockKind::NotMySwamp { dir, inside } => {
            let mut lines = vec![
                "Start Qrec in an empty folder to create a project, or in a folder that has \
                 qrec.json."
                    .to_owned(),
            ];
            lines.extend(
                inside
                    .iter()
                    .map(|project| format!("It's inside the project {project}; run qrec there.")),
            );
            page(NOT_MY_SWAMP, lines, Some(dir.clone()))
        }
        BlockKind::Unreadable { error } => {
            let restore = error.hint.as_deref().and_then(restore_command);
            if error.message == "qrec.json is missing" {
                let lines = match &restore {
                    Some(_) => vec!["Rebuild it from the history:".to_owned()],
                    None => Vec::new(),
                };
                page("qrec.json is missing", lines, restore)
            } else {
                let mut lines = vec![error.message.clone()];
                if restore.is_some() {
                    lines.push("Restore the last good state from history:".to_owned());
                }
                page("qrec.json can't be read", lines, restore)
            }
        }
        BlockKind::HeldByWindow { pid } => page(
            "This project is open in another Qrec window",
            vec![format!(
                "Switch to that window (process {pid}), or close it and start Qrec here again."
            )],
            None,
        ),
        BlockKind::NotResponding { pid, kind, since } => {
            let who = match (pid, kind) {
                (Some(pid), Some(kind)) => format!("Process {pid} ({kind})"),
                (Some(pid), None) => format!("Process {pid}"),
                (None, Some(kind)) => format!("A {kind}"),
                (None, None) => "A process".to_owned(),
            };
            let line = match since {
                Some(since) => format!("{who} has held it since {since}."),
                None => format!("{who} holds it."),
            };
            page(
                "The Qrec process holding this project isn't responding",
                vec![line],
                None,
            )
        }
        BlockKind::GstStuck => page(
            "GStreamer didn't start",
            vec![
                "It didn't finish starting within 30 s. Run qrec doctor in a terminal, then \
                 start Qrec again."
                    .to_owned(),
            ],
            None,
        ),
        BlockKind::FolderUnreadable { dir, error } => page(
            "Can't open this folder",
            vec![capitalized(error)],
            Some(dir.clone()),
        ),
        BlockKind::HostFailed { error } => {
            let mut lines = vec![capitalized(&error.message)];
            lines.extend(error.hint.iter().map(|h| capitalized(h)));
            page("Qrec can't open this project", lines, None)
        }
    }
}

/// The `qrec history restore <seq> --yes` an error's hint names, if any
/// (the hint quotes it in backticks).
pub fn restore_command(hint: &str) -> Option<String> {
    let start = hint.find("qrec history restore")?;
    let rest = &hint[start..];
    let end = rest.find('`').unwrap_or(rest.len());
    Some(rest[..end].trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds() -> Vec<BlockKind> {
        vec![
            BlockKind::MissingDeps {
                report: "✗ gtk4paintablesink: missing (GStreamer GTK 4 video sink)\n  \
                         hint: nix develop"
                    .into(),
            },
            BlockKind::CliRecording {
                take: 11,
                elapsed_secs: 72,
                stopping: false,
            },
            BlockKind::CliRecording {
                take: 11,
                elapsed_secs: 75,
                stopping: true,
            },
            BlockKind::ExportWait {
                name: "exports/rust-traits-talk-20261014-153012.mp4".into(),
                percent: Some(43),
                waiting: false,
            },
            BlockKind::ExportWait {
                name: "exports/rust-traits-talk-20261014-153012.mp4".into(),
                percent: None,
                waiting: true,
            },
            BlockKind::NotMySwamp {
                dir: "~/Downloads".into(),
                inside: None,
            },
            BlockKind::NotMySwamp {
                dir: "~/videos/rust-traits-talk/exports".into(),
                inside: Some("~/videos/rust-traits-talk".into()),
            },
            BlockKind::Unreadable {
                error: CoreError::invalid(
                    "qrec.json is invalid: /timeline/clips/4/out: 1400 > 1377",
                )
                .with_hint(
                    "fix it, or rebuild it from the history with `qrec history restore 212 --yes`",
                ),
            },
            BlockKind::Unreadable {
                error: CoreError::invalid("qrec.json is missing").with_hint(
                    "fix it, or rebuild it from the history with `qrec history restore 1 --yes`",
                ),
            },
            BlockKind::Unreadable {
                error: CoreError::invalid("qrec.json is not valid JSON: EOF at line 1"),
            },
            BlockKind::HeldByWindow { pid: 4242 },
            BlockKind::NotResponding {
                pid: Some(4242),
                kind: Some("headless host".into()),
                since: Some("15:02".into()),
            },
            BlockKind::NotResponding {
                pid: None,
                kind: None,
                since: None,
            },
            BlockKind::GstStuck,
            BlockKind::FolderUnreadable {
                dir: "/srv/locked".into(),
                error: "permission denied (os error 13)".into(),
            },
            BlockKind::HostFailed {
                error: CoreError::io("can't create .qrec/: read-only file system")
                    .with_hint("check the folder's permissions"),
            },
        ]
    }

    #[test]
    fn interim_pages() {
        let pages: Vec<(BlockKind, u8, BlockingContent)> = kinds()
            .into_iter()
            .map(|k| {
                let content = interim(&k);
                (k.clone(), k.exit_code(), content)
            })
            .collect();
        insta::assert_json_snapshot!("interim_pages", pages);
    }

    #[test]
    fn spec_copy() {
        let x2 = interim(&BlockKind::NotMySwamp {
            dir: "~/Downloads".into(),
            inside: None,
        });
        assert_eq!(
            x2.title,
            "Not my swamp: this folder isn't empty and has no qrec.json"
        );
        assert_eq!(x2.buttons, [quit()]);
        let s23 = interim(&BlockKind::MissingDeps { report: "r".into() });
        assert_eq!(s23.title, "The swamp is missing something");
        assert_eq!(s23.buttons[0].action, BlockingAction::CheckAgain);
        assert!(s23.buttons[0].primary);
        let x1 = interim(&BlockKind::CliRecording {
            take: 11,
            elapsed_secs: 72,
            stopping: false,
        });
        assert_eq!(x1.title, "A take is being recorded from the CLI.");
        assert_eq!(
            x1.lines,
            [
                "Take 11 started from the terminal 00:01:12 ago. Stop it to open the project \
              here. Quit leaves it recording."
            ]
        );
        let labels: Vec<&str> = x1.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Stop & open", "Quit"]);
        let x3 = interim(&BlockKind::Unreadable {
            error: CoreError::invalid("qrec.json is invalid: bad").with_hint(
                "fix it, or rebuild it from the history with `qrec history restore 212 --yes`",
            ),
        });
        assert_eq!(x3.title, "qrec.json can't be read");
        assert_eq!(
            x3.monospace.as_deref(),
            Some("qrec history restore 212 --yes")
        );
        assert_eq!(restore_command("no command here"), None);
        assert_eq!(
            restore_command("run qrec history restore 3 --yes"),
            Some("qrec history restore 3 --yes".to_owned())
        );
        let exits: Vec<u8> = kinds().iter().map(BlockKind::exit_code).collect();
        assert_eq!(exits, [5, 6, 6, 6, 6, 3, 3, 1, 1, 1, 6, 6, 6, 5, 1, 1]);
    }
}
