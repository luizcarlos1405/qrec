//! The autocut page (G§4.7, S12, D-15): the draft settings, the dry run and
//! apply commands, the summary and the timeline preview. Owner: WP5. Stub
//! until WP5 lands: the signatures are final, the bodies return defaults.

use serde::{Deserialize, Serialize};

use crate::ids::{Frame, TrackId};
use crate::model::Project;
use crate::protocol::command::{AutocutOverrides, ClipScope, Command};
use crate::protocol::reply::AutocutPlanView;
use crate::state::State;
use crate::view::AutocutPreviewSpan;

/// The sliders' ranges: (min, max, step).
pub const THRESHOLD_DB: (f64, f64, f64) = (-60.0, -10.0, 1.0);
pub const MIN_SILENCE_MS: (u32, u32, u32) = (100, 2000, 50);
pub const PADDING_MS: (u32, u32, u32) = (0, 500, 10);
pub const MIN_KEEP_MS: (u32, u32, u32) = (0, 1000, 10);

/// The page's settings while tuning: a draft, no history (D-15).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocutDraft {
    /// "Detect silence on".
    pub track: Option<TrackId>,
    pub threshold_db: f64,
    pub min_silence_ms: u32,
    pub padding_ms: u32,
    pub min_keep_ms: u32,
}

impl From<&Project> for AutocutDraft {
    /// The project's autocut settings.
    fn from(project: &Project) -> Self {
        let a = &project.autocut;
        Self {
            track: a.track.clone(),
            threshold_db: a.threshold_db,
            min_silence_ms: a.min_silence_ms,
            padding_ms: a.padding_ms,
            min_keep_ms: a.min_keep_ms,
        }
    }
}

/// The dry run of `draft`: the selection, or every clip.
pub fn dry_run(state: &State, draft: &AutocutDraft) -> Command {
    let _ = (state, draft);
    Command::Autocut {
        scope: ClipScope::All,
        overrides: AutocutOverrides::default(),
        dry_run: true,
    }
}

/// Apply: `AutocutSettings{patch}` with the changed fields (if any), then
/// `Autocut{dry_run: false}` (two entries; the cut undoes first).
pub fn apply(state: &State, draft: &AutocutDraft) -> Vec<Command> {
    let _ = (state, draft);
    Vec::new()
}

/// The page's summary (S12).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocutSummary {
    /// "Removes 4 silences (6.2 s)"; "Nothing to cut".
    pub title: String,
    /// ("Take 8", "00:01:43:15 → 00:01:37:09") per take, then
    /// ("Timeline", "00:04:06:11 → 00:04:00:05").
    pub rows: Vec<(String, String)>,
    pub regions: Vec<RegionRow>,
}

/// One region of the list: a click seeks to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionRow {
    pub n: usize,
    /// "00:02:41:12 → 00:02:43:00".
    pub range: String,
    /// "48 f · 1.6 s".
    pub size: String,
    pub seek: Frame,
}

/// The summary of a dry run's plan.
pub fn summary(plan: &AutocutPlanView, state: &State) -> AutocutSummary {
    let _ = (plan, state);
    AutocutSummary::default()
}

/// What the timeline hatches for a dry run: each clip minus what it keeps,
/// in timeline frames before the cut.
pub fn preview_spans(plan: &AutocutPlanView, state: &State) -> Vec<AutocutPreviewSpan> {
    let _ = (plan, state);
    Vec::new()
}

/// The panel subtitle while previewing: "Previewing take 8 · c3e9",
/// "Previewing 4 clips", "Previewing all 10 clips".
pub fn subtitle(state: &State) -> String {
    let _ = state;
    String::new()
}
