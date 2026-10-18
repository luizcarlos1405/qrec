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
//!
//! The drafted pages (WP8, G§0.2 approved them as drawn) give the same
//! pages the frames' content: S23's rows from the doctor's checks
//! ([`missing_deps`], [`CheckView`]) and its GStreamer-stuck variant
//! ([`gst_stuck`]), X1's modal with its well ([`cli_recording`]) and D-29's
//! export wait in the same modal ([`export_wait`]), X2 with D-14's line
//! ([`not_my_swamp`]) and the two undrawn pages of its shape, D-13's held
//! page ([`held_by_window`]) and the not-responding one ([`not_responding`]),
//! X3 and its missing-file variant ([`unreadable`]); each with its hint bar
//! ([`hints`]). The folder and the host failing keep their interim page.

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

// ------------------------------------------------------- the drafted pages

/// What a blocking page's hint bar offers (the frames' bars).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockingHints {
    /// S23: `[Enter] Check again`, `[Ctrl][Q] Quit`.
    CheckAgain,
    /// X3: `[Enter] Copy command`, `[Ctrl][Q] Quit`.
    CopyCommand,
    /// X1 and the export wait, modals: `[Tab] Move focus`, `[Enter]
    /// Activate focused button`, `[Esc] Quit`.
    Modal,
    /// X2 and the rest: `[Ctrl][Q] Quit`.
    Quit,
}

/// The hint bar of `kind`'s page.
pub fn hints(kind: &BlockKind) -> BlockingHints {
    match kind {
        BlockKind::MissingDeps { .. } => BlockingHints::CheckAgain,
        BlockKind::CliRecording { .. } | BlockKind::ExportWait { .. } => BlockingHints::Modal,
        BlockKind::Unreadable { error }
            if error.hint.as_deref().and_then(restore_command).is_some() =>
        {
            BlockingHints::CopyCommand
        }
        BlockKind::Unreadable { .. }
        | BlockKind::NotMySwamp { .. }
        | BlockKind::HeldByWindow { .. }
        | BlockKind::NotResponding { .. }
        | BlockKind::GstStuck
        | BlockKind::FolderUnreadable { .. }
        | BlockKind::HostFailed { .. } => BlockingHints::Quit,
    }
}

/// What a drafted page's button does: a startup flow's action
/// ([`BlockingAction`]), or one the page carries out itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageAction {
    Flow(BlockingAction),
    /// S23: the whole doctor report to the clipboard.
    CopyReport,
    /// X3, S23's rows: the command shown next to it to the clipboard.
    CopyCommand,
}

/// A C01 kind, as the frames draw each page's buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ButtonKind {
    Primary,
    Secondary,
    Flat,
}

/// A large (40 px) button of a drafted page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageButton {
    pub label: String,
    pub action: PageAction,
    pub kind: ButtonKind,
    /// It has the keyboard focus when the page shows (the frames' ring).
    pub focused: bool,
    /// C01's loading state ("Stopping…"): a spinner, and it doesn't react.
    pub busy: bool,
}

fn page_button(label: &str, action: PageAction, kind: ButtonKind, focused: bool) -> PageButton {
    PageButton {
        label: label.to_owned(),
        action,
        kind,
        focused,
        busy: false,
    }
}

/// Quit as X2, X3 and the modals draw it: secondary.
fn quit_button(focused: bool) -> PageButton {
    page_button(
        "Quit",
        PageAction::Flow(BlockingAction::Quit),
        ButtonKind::Secondary,
        focused,
    )
}

/// A body line under the title (`body`, `muted`, centered).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "line",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BodyLine {
    Text {
        text: String,
    },
    /// A line with an inline command chip (S23: "Run `qrec doctor` in a
    /// terminal for the same report.").
    Command {
        before: String,
        command: String,
        after: String,
    },
}

fn text_line(text: &str) -> BodyLine {
    BodyLine::Text {
        text: text.to_owned(),
    }
}

/// The C31 block's head: the art, the title and the body lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHead {
    pub title: String,
    /// `display-sm` (X3) rather than `display`.
    pub small_title: bool,
    pub body: Vec<BodyLine>,
}

/// One dependency check as S23 reads it (`doctor::Check`, GTK-free).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckView {
    /// The doctor's id: `gst.gtk4paintablesink`, `portal.screencast`, …
    pub id: String,
    pub required: bool,
    pub ok: bool,
    pub detail: String,
    pub hint: Option<String>,
}

/// A failed check's state: a required one is missing (✕ `record-text`),
/// an optional one only warns (⚠ `warning-text`, the Optional chip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckStatus {
    Missing,
    Optional,
}

/// A piece of a row's item: a name in `mono` (an element, an encoder, a
/// check id) or in the UI face.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "face", content = "text", rename_all = "camelCase")]
pub enum ItemPart {
    Mono(String),
    Text(String),
}

/// One S23 row (C20, 72 px).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckRow {
    pub id: String,
    pub status: CheckStatus,
    pub item: Vec<ItemPart>,
    /// What it does, or what the doctor found.
    pub description: String,
    /// The fix's mono box with its Copy button.
    pub command: Option<String>,
    /// The line under it.
    pub note: Option<String>,
}

/// A check that passed: the disclosure's rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PassedCheck {
    pub id: String,
    pub detail: String,
}

/// S23 (and the GStreamer-stuck page in its shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingDepsPage {
    pub head: PageHead,
    /// The failed checks: the required ones, then the optional ones.
    pub rows: Vec<CheckRow>,
    /// The checks that passed, behind the disclosure; none, no disclosure.
    pub passed: Vec<PassedCheck>,
    /// "14 other checks passed".
    pub passed_label: String,
    pub buttons: Vec<PageButton>,
}

/// `check`'s row with BRIEF S23's copy for its id (the item, what it does,
/// the command and its note); `None` for the other ids.
fn known_row(check: &CheckView, status: CheckStatus) -> Option<CheckRow> {
    let mono = |s: &str| ItemPart::Mono(s.to_owned());
    let text = |s: &str| ItemPart::Text(s.to_owned());
    let (item, description, command, note) = match check.id.as_str() {
        "gst.gtk4paintablesink" => (
            vec![mono("gtk4paintablesink")],
            "GStreamer GTK 4 video sink (gst-plugins-rs). Qrec draws the preview with it.",
            Some("nix develop"),
            Some("The flake provides gst-plugins-rs."),
        ),
        "portal.screencast" => (
            vec![text("ScreenCast portal backend")],
            "Needed to capture screens on Wayland.",
            Some("xdg-desktop-portal-gnome"),
            Some("On niri, install it, then log out and back in."),
        ),
        "ffmpeg.libsvtav1" => (
            vec![mono("libsvtav1"), text("in ffmpeg")],
            "AV1 export will be unavailable. Everything else works.",
            None,
            None,
        ),
        _ => return None,
    };
    Some(CheckRow {
        id: check.id.clone(),
        status,
        item,
        description: description.to_owned(),
        command: command.map(str::to_owned),
        note: note.map(str::to_owned),
    })
}

fn check_row(check: &CheckView) -> CheckRow {
    let status = if check.required {
        CheckStatus::Missing
    } else {
        CheckStatus::Optional
    };
    // Other ids: the id and what the doctor found, its hint under it.
    known_row(check, status).unwrap_or_else(|| CheckRow {
        id: check.id.clone(),
        status,
        item: vec![ItemPart::Mono(check.id.clone())],
        description: check.detail.clone(),
        command: None,
        note: check.hint.clone(),
    })
}

/// "14 other checks passed", "1 other check passed".
fn passed_label(n: usize) -> String {
    let noun = if n == 1 { "check" } else { "checks" };
    format!("{} other {noun} passed", format::count(n as u64))
}

/// S23 from the doctor's checks (G§4.11): a row per failed check, the
/// required ones first, each group in the doctor's order; the rest behind
/// "✓ N other checks passed"; Check again (focused), Copy report, Quit.
pub fn missing_deps(checks: &[CheckView]) -> MissingDepsPage {
    let failed = |required: bool| {
        checks
            .iter()
            .filter(move |c| !c.ok && c.required == required)
            .map(check_row)
    };
    let rows: Vec<CheckRow> = failed(true).chain(failed(false)).collect();
    let passed: Vec<PassedCheck> = checks
        .iter()
        .filter(|c| c.ok)
        .map(|c| PassedCheck {
            id: c.id.clone(),
            detail: c.detail.clone(),
        })
        .collect();
    MissingDepsPage {
        head: PageHead {
            title: "The swamp is missing something".to_owned(),
            small_title: false,
            body: vec![
                text_line("Qrec can't start until these are installed."),
                BodyLine::Command {
                    before: "Run".to_owned(),
                    command: "qrec doctor".to_owned(),
                    after: "in a terminal for the same report.".to_owned(),
                },
            ],
        },
        rows,
        passed_label: passed_label(passed.len()),
        passed,
        buttons: vec![
            page_button(
                "Check again",
                PageAction::Flow(BlockingAction::CheckAgain),
                ButtonKind::Primary,
                true,
            ),
            page_button(
                "Copy report",
                PageAction::CopyReport,
                ButtonKind::Secondary,
                false,
            ),
            page_button(
                "Quit",
                PageAction::Flow(BlockingAction::Quit),
                ButtonKind::Flat,
                false,
            ),
        ],
    }
}

/// S23's GStreamer-stuck variant (not drawn: S23's shape): GStreamer
/// didn't start within 30 s, so there is no doctor report to show or copy
/// and nothing to check again in this process; Quit (exit 5).
pub fn gst_stuck() -> MissingDepsPage {
    MissingDepsPage {
        head: PageHead {
            title: "GStreamer didn't start".to_owned(),
            small_title: false,
            body: vec![
                text_line("Qrec can't record or draw the preview without it."),
                BodyLine::Command {
                    before: "Run".to_owned(),
                    command: "qrec doctor".to_owned(),
                    after: "in a terminal, then start Qrec again.".to_owned(),
                },
            ],
        },
        rows: vec![CheckRow {
            id: "gstreamer".to_owned(),
            status: CheckStatus::Missing,
            item: vec![ItemPart::Text("GStreamer".to_owned())],
            description: "It didn't finish starting within 30 s: a GPU driver probably hung its \
                          plugin scan."
                .to_owned(),
            command: None,
            note: None,
        }],
        passed: Vec::new(),
        passed_label: String::new(),
        buttons: vec![quit_button(true)],
    }
}

/// What the modal's well shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "well",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Well {
    /// X1: the static REC badge, "Take 11", the project, the time.
    Recording {
        take: String,
        project: String,
        elapsed: String,
    },
    /// D-29: the output and its percent, with a mini mud bar.
    Export { file: String, percent: Option<u8> },
}

/// X1's modal (C24 over the scrim, at GUI start), and D-29's export wait
/// in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModalPage {
    pub title: String,
    /// `body-strong` under the title.
    pub lead: String,
    pub body: String,
    pub well: Well,
    /// In the footer's order (right-aligned): Quit, then the primary.
    pub buttons: Vec<PageButton>,
}

/// X1 for take `take` recorded `elapsed_secs` so far in `project` (the
/// folder as people read it): Quit focused (the frame's: Enter doesn't
/// stop someone's take), Stop & open primary, "Stopping…" once pressed.
pub fn cli_recording(take: u32, elapsed_secs: u64, stopping: bool, project: &str) -> ModalPage {
    let elapsed = format::hms(elapsed_secs);
    let mut stop = page_button(
        if stopping {
            "Stopping\u{2026}"
        } else {
            "Stop & open"
        },
        PageAction::Flow(BlockingAction::StopAndOpen),
        ButtonKind::Primary,
        false,
    );
    stop.busy = stopping;
    ModalPage {
        title: "A take is being recorded from the CLI.".to_owned(),
        lead: format!("Take {take} started from the terminal {elapsed} ago."),
        body: "Stop it to open the project here. Quit leaves it recording.".to_owned(),
        well: Well::Recording {
            take: format!("Take {take}"),
            project: project.to_owned(),
            elapsed,
        },
        buttons: vec![quit_button(true), stop],
    }
}

/// D-29's export wait (not drawn: X1's modal): the output and how far it
/// got, Quit focused, Wait primary ("Waiting…" once pressed).
pub fn export_wait(name: &str, percent: Option<u8>, waiting: bool) -> ModalPage {
    let mut wait = page_button(
        if waiting { "Waiting\u{2026}" } else { "Wait" },
        PageAction::Flow(BlockingAction::Wait),
        ButtonKind::Primary,
        false,
    );
    wait.busy = waiting;
    ModalPage {
        title: "An export from the CLI is running.".to_owned(),
        lead: match percent {
            Some(p) => format!("It's {p}% done."),
            None => "It's starting.".to_owned(),
        },
        body: "Wait to open the project here once it's done. Quit leaves it running.".to_owned(),
        well: Well::Export {
            file: name.to_owned(),
            percent,
        },
        buttons: vec![quit_button(true), wait],
    }
}

/// The icon of an X2-shaped page's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RowIcon {
    /// X2's folder.
    Folder,
    /// A project another process holds.
    FolderOpen,
}

/// X2's folder row (56 px): the icon, the path in `mono`, why on the
/// right.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRow {
    pub icon: RowIcon,
    pub path: String,
    pub why: String,
}

/// X2, and the pages drawn after it: D-13's and the not-responding one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderPage {
    pub head: PageHead,
    pub row: FolderRow,
    /// Lines in `body` `text` under the row (D-14's).
    pub lines: Vec<String>,
    /// The `caption` note under them ("Hidden files count too…").
    pub note: Option<String>,
    pub buttons: Vec<PageButton>,
}

/// X2: `dir` (as people read it) isn't empty and has no `qrec.json`;
/// `inside`, the project it is in (D-14's line).
pub fn not_my_swamp(dir: &str, inside: Option<&str>) -> FolderPage {
    FolderPage {
        head: PageHead {
            title: NOT_MY_SWAMP.to_owned(),
            small_title: false,
            body: vec![text_line(
                "Start Qrec in an empty folder to create a project, or in a folder that has \
                 qrec.json.",
            )],
        },
        row: FolderRow {
            icon: RowIcon::Folder,
            path: dir.to_owned(),
            why: "Not empty \u{00b7} no qrec.json".to_owned(),
        },
        lines: inside
            .map(|project| format!("It's inside the project {project}; run qrec there."))
            .into_iter()
            .collect(),
        note: Some("Hidden files count too, such as .git or .envrc.".to_owned()),
        buttons: vec![quit_button(true)],
    }
}

/// D-13 (not drawn: X2's shape): `project` is open in the Qrec window of
/// process `pid`.
pub fn held_by_window(pid: u32, project: &str) -> FolderPage {
    FolderPage {
        head: PageHead {
            title: "This project is open in another Qrec window".to_owned(),
            small_title: false,
            body: vec![text_line("A project opens in one Qrec window at a time.")],
        },
        row: FolderRow {
            icon: RowIcon::FolderOpen,
            path: project.to_owned(),
            why: format!("Open in process {pid}"),
        },
        lines: Vec::new(),
        note: Some("Switch to that window, or close it and start Qrec here again.".to_owned()),
        buttons: vec![quit_button(true)],
    }
}

/// The holder that doesn't respond (not drawn: X2's shape), with what its
/// lock record says (`kind` and `since` as people read them).
pub fn not_responding(
    pid: Option<u32>,
    kind: Option<&str>,
    since: Option<&str>,
    project: &str,
) -> FolderPage {
    let mut why: Vec<String> = Vec::new();
    match pid {
        Some(pid) => why.push(format!("Process {pid}")),
        None => why.push("A Qrec process".to_owned()),
    }
    why.extend(kind.map(str::to_owned));
    why.extend(since.map(|since| format!("since {since}")));
    let end = match pid {
        Some(pid) => format!("If it stays stuck, end process {pid} and start Qrec again."),
        None => "If it stays stuck, end it and start Qrec again.".to_owned(),
    };
    FolderPage {
        head: PageHead {
            title: "The Qrec process holding this project isn't responding".to_owned(),
            small_title: false,
            body: vec![text_line(
                "Qrec can't open the project while that process holds it.",
            )],
        },
        row: FolderRow {
            icon: RowIcon::FolderOpen,
            path: project.to_owned(),
            why: why.join(" \u{00b7} "),
        },
        lines: Vec::new(),
        note: Some(end),
        buttons: vec![quit_button(true)],
    }
}

/// X3's validation box: the file, the problem in `mono`, what it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    pub file: String,
    pub message: String,
    pub explanation: Option<String>,
}

/// X3's way back: the line, the command in its well with Copy command
/// (primary, focused), and which entry it restores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Restore {
    pub line: String,
    pub command: String,
    pub note: Option<String>,
}

/// X3 and its missing-file variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadablePage {
    pub head: PageHead,
    pub problem: Option<Problem>,
    pub restore: Option<Restore>,
    /// What to do without a history to restore from.
    pub advice: Option<String>,
    /// Quit, focused only without a restore (its Copy command has the
    /// focus then).
    pub buttons: Vec<PageButton>,
}

/// The validation box of an invalid `qrec.json`: the first error the
/// validation lists (`details.issues`, with its JSON Pointer as the mono
/// line) and how many more; a parse error's detail with what it means.
fn problem(error: &CoreError, file: &str) -> Problem {
    let issues: Vec<(&str, &str)> = error
        .details
        .as_ref()
        .and_then(|d| d.get("issues"))
        .and_then(|i| i.as_array())
        .into_iter()
        .flatten()
        .filter(|i| i.get("severity").and_then(|s| s.as_str()) == Some("error"))
        .filter_map(|i| Some((i.get("path")?.as_str()?, i.get("message")?.as_str()?)))
        .collect();
    if let Some((path, message)) = issues.first() {
        let mut explanation = sentence(message);
        match issues.len() - 1 {
            0 => {}
            1 => explanation.push_str(" 1 more problem."),
            n => explanation.push_str(&format!(" {n} more problems.")),
        }
        return Problem {
            file: file.to_owned(),
            message: (*path).to_owned(),
            explanation: Some(explanation),
        };
    }
    let message = &error.message;
    let (message, explanation) =
        if let Some(rest) = message.strip_prefix("qrec.json is not valid JSON: ") {
            (rest, Some("It isn't valid JSON."))
        } else if let Some(rest) = message.strip_prefix("qrec.json is not UTF-8: ") {
            (rest, Some("It isn't UTF-8 text."))
        } else if let Some(rest) = message.strip_prefix("qrec.json is invalid: ") {
            (rest, None)
        } else {
            (message.as_str(), None)
        };
    Problem {
        file: file.to_owned(),
        message: message.to_owned(),
        explanation: explanation.map(str::to_owned),
    }
}

/// `text` as a sentence for people: capitalized, with a full stop, and the
/// takes as people name them ("take-0003" → "take 3", as X3 reads).
fn sentence(text: &str) -> String {
    let mut out = capitalized(&take_names(text.trim_end()));
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// Every take id in `text` (`take-` and its digits) as "take N".
fn take_names(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("take-") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "take-".len()..];
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        match after[..digits].parse::<u64>() {
            Ok(n) if digits > 0 => {
                out.push_str(&format!("take {n}"));
                rest = &after[digits..];
            }
            _ => {
                out.push_str("take-");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The history entry a restore command names (`qrec history restore 212
/// --yes`).
fn restore_seq(command: &str) -> Option<&str> {
    command
        .strip_prefix("qrec history restore ")?
        .split_whitespace()
        .next()
        .filter(|seq| seq.chars().all(|c| c.is_ascii_digit()))
}

/// X3 for the failed open's `error`; `file` is the project's `qrec.json`
/// as people read it. A missing file is the variant: no validation box,
/// "Rebuild it from the history:".
pub fn unreadable(error: &CoreError, file: &str) -> UnreadablePage {
    let missing = error.message == "qrec.json is missing";
    let command = error.hint.as_deref().and_then(restore_command);
    let restore = command.map(|command| Restore {
        line: if missing {
            "Rebuild it from the history:"
        } else {
            "Restore the last good state from history:"
        }
        .to_owned(),
        note: restore_seq(&command).map(|seq| format!("Entry {seq} is the last good state.")),
        command,
    });
    let advice = restore.is_none().then(|| {
        if missing {
            "There's no history to rebuild it from: restore it from a backup."
        } else {
            "There's no history to restore it from: fix it, or restore it from a backup."
        }
        .to_owned()
    });
    let (title, body) = if missing {
        (
            "qrec.json is missing",
            "Qrec won't open this project without it.",
        )
    } else {
        (
            "qrec.json can't be read",
            "Qrec won't open this project, and it never overwrites a broken qrec.json.",
        )
    };
    let focused_quit = restore.is_none();
    UnreadablePage {
        head: PageHead {
            title: title.to_owned(),
            small_title: true,
            body: vec![text_line(body)],
        },
        problem: (!missing).then(|| problem(error, file)),
        restore,
        advice,
        buttons: vec![quit_button(focused_quit)],
    }
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

    // ------------------------------------------------- the drafted pages

    fn check(id: &str, required: bool, ok: bool, detail: &str, hint: Option<&str>) -> CheckView {
        CheckView {
            id: id.into(),
            required,
            ok,
            detail: detail.into(),
            hint: hint.map(Into::into),
        }
    }

    /// BRIEF S23's machine: gtk4paintablesink and the ScreenCast portal
    /// missing, libsvtav1 optional and missing, 14 other checks fine (in
    /// the doctor's order: the optional failure comes first there).
    fn s23_checks() -> Vec<CheckView> {
        let mut checks = vec![
            check("gstreamer", true, true, "1.26.6", None),
            check(
                "ffmpeg.libsvtav1",
                false,
                false,
                "ffmpeg has no libsvtav1 encoder (AV1 WebM exports)",
                Some("install a full ffmpeg build (NixOS: ffmpeg-full)"),
            ),
            check(
                "gst.gtk4paintablesink",
                true,
                false,
                "gtk4paintablesink (the GUI preview) isn't in the GStreamer registry",
                Some("install gst-plugins-rs (gtk4)"),
            ),
            check(
                "portal.screencast",
                true,
                false,
                "no ScreenCast portal: no reply",
                Some("install xdg-desktop-portal with a ScreenCast backend"),
            ),
        ];
        for n in 0..13 {
            checks.push(check(&format!("gst.element{n}"), true, true, "found", None));
        }
        checks
    }

    /// An invalid `qrec.json` as the shell reports it (`store::load`'s
    /// `invalid_file`): the validation's error with its issues.
    fn invalid_project_error() -> CoreError {
        let mut project = crate::testkit::design_project();
        let take = project.timeline.clips[4].take.clone();
        let frames = project.takes[&take].duration_frames;
        project.timeline.clips[4].out = frames + 23;
        project.timeline.clips[6].in_ = project.timeline.clips[6].out;
        let invalid = crate::validate::ensure_valid(&project).expect_err("invalid");
        let mut err = CoreError::invalid(format!("qrec.json is invalid: {}", invalid.message))
            .with_hint(
                "fix it, or rebuild it from the history with `qrec history restore 212 --yes`",
            );
        err.details = invalid.details;
        err
    }

    #[test]
    fn drafted_pages() {
        let pages = serde_json::json!({
            "s23": missing_deps(&s23_checks()),
            "s23NoFfmpeg": missing_deps(&[
                check("ffmpeg", true, false, "ffmpeg wasn't found on PATH",
                    Some("install ffmpeg (a full build with libx264, libvpx and libopus; NixOS: ffmpeg-full)")),
                check("ffprobe", true, true, "ffprobe version 7.1", None),
            ]),
            "gstStuck": gst_stuck(),
            "x1": cli_recording(11, 72, false, "~/videos/rust-traits-talk"),
            "x1Stopping": cli_recording(11, 75, true, "~/videos/rust-traits-talk"),
            "exportWait": export_wait("exports/rust-traits-talk-20261014-153012.mp4", Some(43), false),
            "exportWaiting": export_wait("exports/rust-traits-talk-20261014-153012.mp4", None, true),
            "x2": not_my_swamp("~/Downloads", None),
            "x2Inside": not_my_swamp("~/videos/rust-traits-talk/exports", Some("~/videos/rust-traits-talk")),
            "heldByWindow": held_by_window(4242, "~/videos/rust-traits-talk"),
            "notResponding": not_responding(Some(4242), Some("headless host"), Some("15:02"), "~/videos/rust-traits-talk"),
            "notRespondingUnknown": not_responding(None, None, None, "~/videos/rust-traits-talk"),
            "x3": unreadable(&invalid_project_error(), "~/videos/rust-traits-talk/qrec.json"),
            "x3Json": unreadable(
                &CoreError::invalid("qrec.json is not valid JSON: key must be a string at line 1 column 15")
                    .with_hint("fix it, or rebuild it from the history with `qrec history restore 3 --yes`"),
                "~/videos/rust-traits-talk/qrec.json",
            ),
            "x3NoHistory": unreadable(
                &CoreError::invalid("qrec.json is not valid JSON: EOF while parsing a value at line 1 column 0")
                    .with_hint("fix it; there is no history to rebuild it from"),
                "~/talk/qrec.json",
            ),
            "x3Missing": unreadable(
                &CoreError::invalid("qrec.json is missing")
                    .with_hint("rebuild it from the history with `qrec history restore 41 --yes`"),
                "~/videos/rust-traits-talk/qrec.json",
            ),
            "x3MissingNoHistory": unreadable(
                &CoreError::invalid("qrec.json is missing").with_hint("restore it from a backup"),
                "~/talk/qrec.json",
            ),
        });
        insta::assert_json_snapshot!("drafted_pages", pages);
    }

    /// S23's rows (G§4.11): the failed required checks first, then the
    /// optional ones, each in the doctor's order; BRIEF's copy by id, the
    /// doctor's own words for the others; the rest behind the disclosure.
    #[test]
    fn s23_lists_the_failed_checks_required_first() {
        let page = missing_deps(&s23_checks());
        let ids: Vec<&str> = page.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "gst.gtk4paintablesink",
                "portal.screencast",
                "ffmpeg.libsvtav1"
            ]
        );
        let statuses: Vec<CheckStatus> = page.rows.iter().map(|r| r.status).collect();
        assert_eq!(
            statuses,
            [
                CheckStatus::Missing,
                CheckStatus::Missing,
                CheckStatus::Optional
            ]
        );
        assert_eq!(
            page.rows[0].item,
            [ItemPart::Mono("gtk4paintablesink".into())]
        );
        assert_eq!(page.rows[0].command.as_deref(), Some("nix develop"));
        assert_eq!(
            page.rows[1].item,
            [ItemPart::Text("ScreenCast portal backend".into())]
        );
        assert_eq!(
            page.rows[1].command.as_deref(),
            Some("xdg-desktop-portal-gnome")
        );
        assert_eq!(
            page.rows[2].item,
            [
                ItemPart::Mono("libsvtav1".into()),
                ItemPart::Text("in ffmpeg".into())
            ]
        );
        assert_eq!(page.rows[2].command, None);
        assert_eq!(page.passed.len(), 14);
        assert_eq!(page.passed_label, "14 other checks passed");
        assert_eq!(passed_label(1), "1 other check passed");
        // Another id: the id, the doctor's detail, its hint under it.
        let other = missing_deps(&[check(
            "pactl",
            true,
            false,
            "pactl wasn't found (it lists microphones and system audio)",
            Some("install the PulseAudio client tools (NixOS: pkgs.pulseaudio)"),
        )]);
        assert_eq!(other.rows[0].item, [ItemPart::Mono("pactl".into())]);
        assert_eq!(
            other.rows[0].description,
            "pactl wasn't found (it lists microphones and system audio)"
        );
        assert_eq!(
            other.rows[0].note.as_deref(),
            Some("install the PulseAudio client tools (NixOS: pkgs.pulseaudio)")
        );
        assert!(other.passed.is_empty());
        // Check again has the focus (S23's ring), then Copy report, Quit.
        let buttons: Vec<(&str, ButtonKind, bool)> = page
            .buttons
            .iter()
            .map(|b| (b.label.as_str(), b.kind, b.focused))
            .collect();
        assert_eq!(
            buttons,
            [
                ("Check again", ButtonKind::Primary, true),
                ("Copy report", ButtonKind::Secondary, false),
                ("Quit", ButtonKind::Flat, false)
            ]
        );
    }

    /// X1 as drawn: Quit (secondary) has the focus, so Enter doesn't stop
    /// the CLI's take; Stop & open turns into "Stopping…", busy; the well
    /// shows the take, the project and the time. The export wait is the
    /// same modal (D-29).
    #[test]
    fn x1_focuses_quit_and_its_stop_turns_busy() {
        let page = cli_recording(11, 72, false, "~/videos/rust-traits-talk");
        assert_eq!(page.lead, "Take 11 started from the terminal 00:01:12 ago.");
        assert_eq!(
            page.well,
            Well::Recording {
                take: "Take 11".into(),
                project: "~/videos/rust-traits-talk".into(),
                elapsed: "00:01:12".into()
            }
        );
        let focused: Vec<&str> = page
            .buttons
            .iter()
            .filter(|b| b.focused)
            .map(|b| b.label.as_str())
            .collect();
        assert_eq!(focused, ["Quit"]);
        assert_eq!(page.buttons[1].label, "Stop & open");
        assert!(!page.buttons[1].busy);
        let stopping = cli_recording(11, 80, true, "~/videos/rust-traits-talk");
        assert_eq!(stopping.buttons[1].label, "Stopping\u{2026}");
        assert!(stopping.buttons[1].busy);
        assert!(!stopping.buttons[0].busy, "Quit still works");
        let export = export_wait("exports/talk.mp4", Some(43), false);
        assert_eq!(export.lead, "It's 43% done.");
        assert_eq!(export.buttons[1].label, "Wait");
        assert!(export.buttons[0].focused);
        assert_eq!(
            export_wait("exports/talk.mp4", None, true).buttons[1].label,
            "Waiting\u{2026}"
        );
    }

    /// X3's box reads the validation's first error: its JSON Pointer in
    /// mono, what it means as a sentence, how many more; the restore
    /// command from the hint, focused; the entry it restores.
    #[test]
    fn x3_shows_the_first_error_and_the_restore() {
        let page = unreadable(&invalid_project_error(), "~/talk/qrec.json");
        let problem = page.problem.expect("a validation box");
        assert_eq!(problem.file, "~/talk/qrec.json");
        assert_eq!(problem.message, "/timeline/clips/4/out");
        let explanation = problem.explanation.expect("an explanation");
        assert!(explanation.starts_with("Clip "), "{explanation}");
        // The take as people name it (X3: "take 3").
        assert!(
            explanation.contains(", past the end of take 3 ("),
            "{explanation}"
        );
        assert!(explanation.ends_with(" 1 more problem."), "{explanation}");
        assert_eq!(
            take_names("take-0003 and take-12, take-x, retake-"),
            "take 3 and take 12, take-x, retake-"
        );
        let restore = page.restore.expect("a restore");
        assert_eq!(restore.command, "qrec history restore 212 --yes");
        assert_eq!(
            restore.note.as_deref(),
            Some("Entry 212 is the last good state.")
        );
        assert!(!page.buttons[0].focused, "Copy command has the focus");
        assert_eq!(page.advice, None);
        // A file that isn't JSON: the parser's words, what it means.
        let json = unreadable(
            &CoreError::invalid("qrec.json is not valid JSON: EOF at line 1"),
            "~/talk/qrec.json",
        );
        let problem = json.problem.expect("a box");
        assert_eq!(problem.message, "EOF at line 1");
        assert_eq!(problem.explanation.as_deref(), Some("It isn't valid JSON."));
        assert!(json.restore.is_none());
        assert!(json.advice.is_some());
        assert!(
            json.buttons[0].focused,
            "Quit has the focus without a restore"
        );
        // The missing file: no box, "Rebuild it from the history:".
        let missing = unreadable(
            &CoreError::invalid("qrec.json is missing")
                .with_hint("rebuild it from the history with `qrec history restore 41 --yes`"),
            "~/talk/qrec.json",
        );
        assert_eq!(missing.head.title, "qrec.json is missing");
        assert!(missing.problem.is_none());
        assert_eq!(
            missing.restore.map(|r| r.line),
            Some("Rebuild it from the history:".to_owned())
        );
        assert_eq!(restore_seq("qrec history restore 7 --yes"), Some("7"));
        assert_eq!(restore_seq("qrec history restore x --yes"), None);
    }

    /// Each page's hint bar is its frame's.
    #[test]
    fn each_page_has_its_frames_hints() {
        let hints_of: Vec<BlockingHints> = kinds().iter().map(hints).collect();
        assert_eq!(
            hints_of,
            [
                BlockingHints::CheckAgain,  // S23
                BlockingHints::Modal,       // X1
                BlockingHints::Modal,       // X1 stopping
                BlockingHints::Modal,       // the export wait
                BlockingHints::Modal,       // waiting
                BlockingHints::Quit,        // X2
                BlockingHints::Quit,        // X2 inside a project
                BlockingHints::CopyCommand, // X3 with a restore
                BlockingHints::CopyCommand, // missing, with a restore
                BlockingHints::Quit,        // X3 without one
                BlockingHints::Quit,        // D-13
                BlockingHints::Quit,        // not responding
                BlockingHints::Quit,
                BlockingHints::Quit, // GStreamer stuck
                BlockingHints::Quit, // the folder
                BlockingHints::Quit, // the host
            ]
        );
    }

    /// D-14's line and the undrawn pages of X2's shape name what holds the
    /// project and what to do.
    #[test]
    fn the_folder_pages_say_what_holds_it() {
        let inside = not_my_swamp("~/talk/exports", Some("~/talk"));
        assert_eq!(
            inside.lines,
            ["It's inside the project ~/talk; run qrec there."]
        );
        assert!(not_my_swamp("~/Downloads", None).lines.is_empty());
        assert_eq!(
            not_responding(Some(4242), Some("headless host"), Some("15:02"), "~/talk")
                .row
                .why,
            "Process 4242 \u{00b7} headless host \u{00b7} since 15:02"
        );
        assert_eq!(
            not_responding(None, None, None, "~/talk").row.why,
            "A Qrec process"
        );
        assert_eq!(
            held_by_window(4242, "~/talk").row.why,
            "Open in process 4242"
        );
        for page in [
            not_my_swamp("~/Downloads", None),
            held_by_window(1, "~/talk"),
            not_responding(None, None, None, "~/talk"),
        ] {
            assert_eq!(page.buttons, [quit_button(true)]);
        }
    }
}
