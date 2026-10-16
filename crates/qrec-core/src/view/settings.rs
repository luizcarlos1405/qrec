//! The Settings dialog's shell and its Project page (G§4.10, S18b). Owner:
//! WP6.
//!
//! The shell has the five tabs as drawn; only Project opens until WP9
//! builds the others (D-48: they show disabled, "Not built yet"). The
//! Project page holds the "Canvas" group: Size, Frame rate and Background,
//! each a `ProjectSet` (an fps change on a project with takes needs the
//! S18b confirmation, which the core asks for).
//!
//! The row captions come from the pen frame `GPmrg`, which the approved PNG
//! cuts off at the modal's edge: the texts after the visible words are
//! stand-ins until someone reads the frame (docs/GUI.md G§4.10).

use serde::{Deserialize, Serialize};

use crate::model::{Color, Fps};
use crate::protocol::command::{CanvasSpec, Command, Nullable};
use crate::state::State;
use crate::view::{GuiFacts, format};

/// The dialog's pages, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsPage {
    Appearance,
    Recording,
    Project,
    Autocut,
    Keyboard,
}

impl SettingsPage {
    pub const ALL: [SettingsPage; 5] = [
        SettingsPage::Appearance,
        SettingsPage::Recording,
        SettingsPage::Project,
        SettingsPage::Autocut,
        SettingsPage::Keyboard,
    ];

    /// The tab's label.
    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::Appearance => "Appearance",
            SettingsPage::Recording => "Recording",
            SettingsPage::Project => "Project",
            SettingsPage::Autocut => "Autocut",
            SettingsPage::Keyboard => "Keyboard",
        }
    }

    /// A stable name for the page (`appearance`, …).
    pub fn name(self) -> &'static str {
        match self {
            SettingsPage::Appearance => "appearance",
            SettingsPage::Recording => "recording",
            SettingsPage::Project => "project",
            SettingsPage::Autocut => "autocut",
            SettingsPage::Keyboard => "keyboard",
        }
    }

    /// The page named `name` ([`SettingsPage::name`]).
    pub fn from_name(name: &str) -> Option<SettingsPage> {
        SettingsPage::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// One tab: the page, its label and whether it opens (only Project until
/// WP9, D-48; the others show the tooltip "Not built yet").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsTab {
    pub page: SettingsPage,
    pub label: String,
    pub enabled: bool,
}

/// The tabs, Appearance first.
pub fn tabs(facts: &GuiFacts) -> Vec<SettingsTab> {
    SettingsPage::ALL
        .into_iter()
        .map(|page| SettingsTab {
            page,
            label: page.label().to_owned(),
            enabled: page == SettingsPage::Project || facts.built.settings_pages,
        })
        .collect()
}

/// The Project page (S18b): the "Canvas" group.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPage {
    /// "Canvas".
    pub group: String,
    /// The group's caption ("Applies to this project only.").
    pub caption: String,
    pub size: SizeRow,
    pub rate: RateRow,
    pub background: BackgroundRow,
    /// "Saved in this project's ~/videos/rust-traits-talk/qrec.json."
    pub footer: String,
    /// The path in the footer ("~/videos/rust-traits-talk/qrec.json"), set
    /// in the mono face.
    pub footer_path: String,
}

/// The Size dropdown: "1920×1080 (16:9)", …, plus the current size when it
/// isn't listed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeRow {
    pub label: String,
    pub caption: String,
    pub options: Vec<SizeOption>,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeOption {
    pub width: u32,
    pub height: u32,
    /// "1920×1080 (16:9)".
    pub label: String,
}

/// The Frame rate segmented control: 24 / 25 / 30 / 50 / 60; none
/// selected for another rate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateRow {
    pub label: String,
    pub caption: String,
    pub rates: Vec<u32>,
    pub selected: Option<usize>,
}

/// The Background field (`#RRGGBB`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundRow {
    pub label: String,
    pub caption: String,
    pub value: String,
}

/// The sizes the dropdown offers (G§4.10), in order.
pub const SIZES: [(u32, u32); 6] = [
    (1280, 720),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
    (1080, 1920),
    (1080, 1080),
];

/// The frame rates the segmented control offers.
pub const RATES: [u32; 5] = [24, 25, 30, 50, 60];

/// What an invalid Background says (C29 invalid).
pub const BACKGROUND_INVALID: &str = "Use #RRGGBB";

/// "1920×1080 (16:9)"; a size whose reduced ratio isn't a short one shows
/// no ratio ("1366×768").
pub fn size_label(width: u32, height: u32) -> String {
    let g = gcd(width, height).max(1);
    let (w, h) = (width / g, height / g);
    if w <= 32 && h <= 32 {
        format!("{width}\u{d7}{height} ({w}:{h})")
    } else {
        format!("{width}\u{d7}{height}")
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The Project page of `state`; `dir_display` is the project folder as
/// people read it (`format::display_path`).
pub fn project_page(state: &State, dir_display: &str) -> ProjectPage {
    let canvas = &state.project.canvas;
    let mut options: Vec<SizeOption> = SIZES
        .iter()
        .map(|&(width, height)| SizeOption {
            width,
            height,
            label: size_label(width, height),
        })
        .collect();
    let current = (canvas.width, canvas.height);
    let selected = match SIZES.iter().position(|s| *s == current) {
        Some(i) => i,
        None => {
            options.push(SizeOption {
                width: canvas.width,
                height: canvas.height,
                label: size_label(canvas.width, canvas.height),
            });
            options.len() - 1
        }
    };
    let fps = canvas.fps;
    let whole = (fps.den > 0 && fps.num.is_multiple_of(fps.den)).then(|| fps.num / fps.den);
    let rate = RATES.iter().position(|r| Some(*r) == whole);
    let dir = dir_display.trim_end_matches('/');
    let footer_path = format!("{dir}/qrec.json");
    ProjectPage {
        group: "Canvas".to_owned(),
        caption: "Applies to this project only.".to_owned(),
        size: SizeRow {
            label: "Size".to_owned(),
            caption: "Preview and export resolution.".to_owned(),
            options,
            selected,
        },
        rate: RateRow {
            label: "Frame rate".to_owned(),
            caption: format!(
                "Currently {}. Clip bounds are re-quantized when it changes.",
                format::fps(fps)
            ),
            rates: RATES.to_vec(),
            selected: rate,
        },
        background: BackgroundRow {
            label: "Background".to_owned(),
            caption: "Shows where layers don't cover the canvas.".to_owned(),
            value: canvas.background.as_str().to_owned(),
        },
        footer: format!("Saved in this project's {footer_path}."),
        footer_path,
    }
}

/// A `ProjectSet` that changes only `canvas` or `background`.
fn project_set(canvas: Option<CanvasSpec>, background: Option<Color>) -> Command {
    Command::ProjectSet {
        name: None,
        canvas,
        background,
        countdown_secs: None,
        encoder: None,
        vaapi_device: Nullable::Keep,
    }
}

/// Picking a size: `ProjectSet{canvas}` with it.
pub fn size_command(state: &State, width: u32, height: u32) -> Command {
    let _ = state;
    project_set(
        Some(CanvasSpec {
            width: Some(width),
            height: Some(height),
            fps: None,
        }),
        None,
    )
}

/// Picking a frame rate: `ProjectSet{canvas}` with it (with takes, the
/// core answers `confirmation_required` and the S18b modal opens).
pub fn rate_command(state: &State, fps: Fps) -> Command {
    let _ = state;
    project_set(
        Some(CanvasSpec {
            width: None,
            height: None,
            fps: Some(fps),
        }),
        None,
    )
}

/// The Background field committed (Enter or blur): `ProjectSet{background}`,
/// or the field's error ("Use #RRGGBB").
pub fn background_command(text: &str) -> Result<Command, String> {
    let color: Color = text
        .trim()
        .parse()
        .map_err(|_| BACKGROUND_INVALID.to_owned())?;
    Ok(project_set(None, Some(color)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;
    use crate::view::Built;
    use crate::{CoreError, ErrorCode, apply};

    #[test]
    fn only_project_opens_until_wp9() {
        let tabs = tabs(&GuiFacts::default());
        let shown: Vec<(&str, bool)> = tabs.iter().map(|t| (t.label.as_str(), t.enabled)).collect();
        assert_eq!(
            shown,
            [
                ("Appearance", false),
                ("Recording", false),
                ("Project", true),
                ("Autocut", false),
                ("Keyboard", false),
            ]
        );
        let built = GuiFacts {
            built: Built {
                settings_pages: true,
                ..Built::default()
            },
            ..GuiFacts::default()
        };
        assert!(super::tabs(&built).iter().all(|t| t.enabled));
        for page in SettingsPage::ALL {
            assert_eq!(SettingsPage::from_name(page.name()), Some(page));
        }
    }

    #[test]
    fn the_project_page_is_s18b() {
        let state = testkit::design_state();
        let page = project_page(&state, "~/videos/rust-traits-talk");
        insta::assert_json_snapshot!(page);
        assert_eq!(
            page.size.options[page.size.selected].label,
            "1920\u{d7}1080 (16:9)"
        );
        assert_eq!(page.rate.selected, Some(2));
        assert_eq!(page.background.value, "#000000");
        assert_eq!(
            page.footer,
            "Saved in this project's ~/videos/rust-traits-talk/qrec.json."
        );
        assert_eq!(page.footer_path, "~/videos/rust-traits-talk/qrec.json");
    }

    #[test]
    fn unlisted_sizes_and_rates() {
        let mut state = testkit::design_state();
        state.project.canvas.width = 1366;
        state.project.canvas.height = 768;
        state.project.canvas.fps = Fps::new(30_000, 1001);
        let page = project_page(&state, "/srv/talk/");
        assert_eq!(page.size.options.len(), 7);
        assert_eq!(page.size.options[page.size.selected].label, "1366\u{d7}768");
        assert_eq!(page.rate.selected, None);
        assert_eq!(
            page.rate.caption,
            "Currently 29.97 fps. Clip bounds are re-quantized when it changes."
        );
        assert_eq!(page.footer_path, "/srv/talk/qrec.json");
        assert_eq!(size_label(1024, 768), "1024\u{d7}768 (4:3)");
        // 60 fps as 120/2 still selects 60.
        state.project.canvas.fps = Fps::new(120, 2);
        assert_eq!(project_page(&state, "x").rate.selected, Some(4));
    }

    /// The commands are what `qrec project set` sends: a size applies at
    /// once; 25 fps on a project with takes asks first (S18b); a background
    /// is checked.
    #[test]
    fn the_rows_send_project_set() {
        let state = testkit::design_state();
        let ctx = testkit::ctx(testkit::CREATED_AT, 1);
        let size = size_command(&state, 2560, 1440);
        let out = apply(&state, &ctx, size).unwrap();
        assert_eq!(
            (
                out.state.project.canvas.width,
                out.state.project.canvas.height
            ),
            (2560, 1440)
        );
        let rate = rate_command(&state, Fps::new(25, 1));
        let err: CoreError = apply(&state, &ctx, rate.clone()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfirmationRequired);
        let mut confirmed = ctx.clone();
        confirmed.confirmed = true;
        let out = apply(&state, &confirmed, rate).unwrap();
        assert_eq!(out.state.project.canvas.fps, Fps::new(25, 1));

        let background = background_command(" #12aBcD ").unwrap();
        let out = apply(&state, &ctx, background).unwrap();
        assert_eq!(out.state.project.canvas.background.as_str(), "#12ABCD");
        for bad in ["", "black", "#12345", "#1234567", "12345G"] {
            assert_eq!(
                background_command(bad),
                Err("Use #RRGGBB".to_owned()),
                "{bad:?}"
            );
        }
    }
}
