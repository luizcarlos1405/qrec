//! The autocut page (G§4.7, S12, D-15): the draft settings, the dry run and
//! apply commands, the summary and the timeline preview. Owner: WP5.
//!
//! - Tuning the sliders writes no history: the page keeps an
//!   [`AutocutDraft`] and sends [`dry_run`]s with it as the overrides.
//! - [`apply`] saves the draft's changed fields (`AutocutSettings`, only if
//!   any changed), then runs the cut (`Autocut{dry_run: false}`): two
//!   history entries, the cut undoes first.
//! - A dry run's plan becomes the summary (the title, the before → after
//!   rows, the region list) and the hatches the timeline draws: each clip
//!   minus what it keeps, in timeline frames before the cut.

use serde::{Deserialize, Serialize};

use crate::autocut::{ClipCut, db_label};
use crate::ids::{Frame, TakeId, TrackId};
use crate::model::{Clip, Project};
use crate::protocol::command::{AutocutOverrides, AutocutPatch, ClipScope, Command, Nullable};
use crate::protocol::reply::AutocutPlanView;
use crate::state::State;
use crate::time::{duration_label, frame_ns};
use crate::timeline::{position_of, total_frames};
use crate::view::AutocutPreviewSpan;
use crate::view::format::{self, MINUS};

/// The sliders' ranges: (min, max, step).
pub const THRESHOLD_DB: (f64, f64, f64) = (-60.0, -10.0, 1.0);
pub const MIN_SILENCE_MS: (u32, u32, u32) = (100, 2000, 50);
pub const PADDING_MS: (u32, u32, u32) = (0, 500, 10);
pub const MIN_KEEP_MS: (u32, u32, u32) = (0, 1000, 10);

/// The page's title.
pub const TITLE: &str = "Autocut";
/// The summary title while a dry run runs (with a 16 px spinner, D-15).
pub const ANALYZING: &str = "Analyzing\u{2026}";
/// The summary title when the plan cuts nothing.
pub const NOTHING_TO_CUT: &str = "Nothing to cut";
/// The note under the switch (S12).
pub const NOTE: &str =
    "Removed audio and video come back if you drag a clip edge out, or with Ctrl+Z.";
/// The switch's label (S12).
pub const AUTO_RUN: &str = "Run autocut after every take";
/// The dropdown's label (S12).
pub const DETECT_ON: &str = "Detect silence on";

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

/// The page's sliders (S12, top to bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Knob {
    Threshold,
    MinSilence,
    Padding,
    MinKeep,
}

impl Knob {
    pub const ALL: [Knob; 4] = [
        Knob::Threshold,
        Knob::MinSilence,
        Knob::Padding,
        Knob::MinKeep,
    ];

    /// The caption over the slider.
    pub fn label(self) -> &'static str {
        match self {
            Knob::Threshold => "Threshold",
            Knob::MinSilence => "Min silence",
            Knob::Padding => "Padding",
            Knob::MinKeep => "Min keep",
        }
    }

    /// The unit in the value field.
    pub fn unit(self) -> &'static str {
        self.units()[0]
    }

    /// The unit's spellings a person may type after the value, the shown
    /// one first ("dB", "db"): [`Knob::parse`] and the field's arrow keys
    /// take the same ones.
    pub fn units(self) -> &'static [&'static str] {
        match self {
            Knob::Threshold => &["dB", "db"],
            _ => &["ms"],
        }
    }

    /// (min, max, step).
    pub fn range(self) -> (f64, f64, f64) {
        let ms = |(a, b, s): (u32, u32, u32)| (f64::from(a), f64::from(b), f64::from(s));
        match self {
            Knob::Threshold => THRESHOLD_DB,
            Knob::MinSilence => ms(MIN_SILENCE_MS),
            Knob::Padding => ms(PADDING_MS),
            Knob::MinKeep => ms(MIN_KEEP_MS),
        }
    }

    /// The labels under the slider's ends: "−60", "−10"; "100", "2000".
    pub fn end_labels(self) -> (String, String) {
        let (min, max, _) = self.range();
        (number(min), number(max))
    }

    /// The knob's value in `draft`.
    pub fn get(self, draft: &AutocutDraft) -> f64 {
        match self {
            Knob::Threshold => draft.threshold_db,
            Knob::MinSilence => f64::from(draft.min_silence_ms),
            Knob::Padding => f64::from(draft.padding_ms),
            Knob::MinKeep => f64::from(draft.min_keep_ms),
        }
    }

    /// `draft` with the knob at `value`, clamped to its range and rounded
    /// to its step.
    pub fn set(self, draft: &AutocutDraft, value: f64) -> AutocutDraft {
        let (min, max, step) = self.range();
        let value = if value.is_finite() {
            value
        } else {
            self.get(draft)
        };
        let v = ((value / step).round() * step).clamp(min, max);
        let mut next = draft.clone();
        match self {
            Knob::Threshold => next.threshold_db = v,
            Knob::MinSilence => next.min_silence_ms = v as u32,
            Knob::Padding => next.padding_ms = v as u32,
            Knob::MinKeep => next.min_keep_ms = v as u32,
        }
        next
    }

    /// The value field's text: "−35", "400".
    pub fn text(self, draft: &AutocutDraft) -> String {
        number(self.get(draft))
    }

    /// The value field's text as the knob's value (clamped and rounded by
    /// [`Knob::set`]); `None` when it isn't a number.
    pub fn parse(self, text: &str) -> Option<f64> {
        crate::view::panel::parse_number(text, self.units())
    }
}

/// A whole number with the true minus: "−35", "400".
fn number(v: f64) -> String {
    let v = v.round() as i64;
    if v < 0 {
        format!("{MINUS}{}", v.unsigned_abs())
    } else {
        v.to_string()
    }
}

/// One item of "Detect silence on": the project scene's tracks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackItem {
    pub track: TrackId,
    /// "A1 · Yeti Nano (mic)".
    pub label: String,
}

/// The dropdown's items (S12): every track of the project scene.
pub fn track_items(state: &State) -> Vec<TrackItem> {
    state
        .project
        .scene
        .audio
        .iter()
        .map(|t| TrackItem {
            track: t.id.clone(),
            label: format!("{} \u{b7} {} ({})", t.id, t.label, t.role.as_str()),
        })
        .collect()
}

/// The track the dropdown shows for `draft`: its own, else the project's
/// setting (a draft that names no track leaves it to the setting, which
/// may change while the page is open), else the project scene's
/// [`default_track`](crate::autocut::default_track): what the dry run
/// analyzes.
pub fn shown_track(state: &State, draft: &AutocutDraft) -> Option<TrackId> {
    let project = &state.project;
    draft
        .track
        .clone()
        .or_else(|| project.autocut.track.clone())
        .or_else(|| crate::autocut::default_track(&project.scene).map(|t| t.id.clone()))
}

/// What the page analyzes: the selection, or every clip.
pub fn scope(state: &State) -> ClipScope {
    if state.ui.selection.is_empty() {
        ClipScope::All
    } else {
        ClipScope::Clips(state.ui.selection.clone())
    }
}

/// The draft as one-off overrides.
pub fn overrides(draft: &AutocutDraft) -> AutocutOverrides {
    AutocutOverrides {
        track: draft.track.clone(),
        threshold_db: Some(draft.threshold_db),
        min_silence_ms: Some(draft.min_silence_ms),
        padding_ms: Some(draft.padding_ms),
        min_keep_ms: Some(draft.min_keep_ms),
    }
}

/// The dry run of `draft`: the selection, or every clip.
pub fn dry_run(state: &State, draft: &AutocutDraft) -> Command {
    Command::Autocut {
        scope: scope(state),
        overrides: overrides(draft),
        dry_run: true,
    }
}

/// What a dry run's answer is worked out on: its command, and what the
/// command doesn't carry (R40, R76). The timeline's clips: the summary,
/// the regions and the hatches are their frames, and a trim, a move, an
/// undo or another client's edit under the same selection changes them.
/// And the project's `autocut.track` while the draft names no track: the
/// track the core then analyzes.
#[derive(Debug, Clone, PartialEq)]
pub struct DryRunKey {
    pub cmd: Command,
    pub clips: Vec<Clip>,
    /// The setting's track when the draft leaves the track to it.
    pub setting_track: Option<TrackId>,
}

impl DryRunKey {
    /// A plan shown for `self` no longer matches `next`'s timeline (or
    /// track): its frames are out of date, not only its draft or scope.
    pub fn stale_for(&self, next: &DryRunKey) -> bool {
        self.clips != next.clips || self.setting_track != next.setting_track
    }
}

/// The key of the dry run of `draft` on `state`: the page asks again when
/// it changes, not only when the command does.
pub fn dry_run_key(state: &State, draft: &AutocutDraft) -> DryRunKey {
    DryRunKey {
        cmd: dry_run(state, draft),
        clips: state.project.timeline.clips.clone(),
        setting_track: match &draft.track {
            Some(_) => None,
            None => state.project.autocut.track.clone(),
        },
    }
}

/// The draft's fields that differ from the project's settings.
pub fn changed(state: &State, draft: &AutocutDraft) -> AutocutPatch {
    let saved = AutocutDraft::from(&state.project);
    AutocutPatch {
        auto_run_after_take: None,
        track: match &draft.track {
            Some(t) if draft.track != saved.track => Nullable::Set(t.clone()),
            None if saved.track.is_some() => Nullable::Clear,
            _ => Nullable::Keep,
        },
        threshold_db: (draft.threshold_db != saved.threshold_db).then_some(draft.threshold_db),
        min_silence_ms: (draft.min_silence_ms != saved.min_silence_ms)
            .then_some(draft.min_silence_ms),
        padding_ms: (draft.padding_ms != saved.padding_ms).then_some(draft.padding_ms),
        min_keep_ms: (draft.min_keep_ms != saved.min_keep_ms).then_some(draft.min_keep_ms),
    }
}

/// Apply: `AutocutSettings{patch}` with the changed fields (if any), then
/// `Autocut{dry_run: false}` (two entries; the cut undoes first).
pub fn apply(state: &State, draft: &AutocutDraft) -> Vec<Command> {
    let patch = changed(state, draft);
    let mut commands = Vec::with_capacity(2);
    if patch != AutocutPatch::default() {
        commands.push(Command::AutocutSettings { patch });
    }
    commands.push(Command::Autocut {
        scope: scope(state),
        overrides: AutocutOverrides::default(),
        dry_run: false,
    });
    commands
}

/// The switch "Run autocut after every take": saved at once (D-15).
pub fn auto_run(on: bool) -> Command {
    Command::AutocutSettings {
        patch: AutocutPatch {
            auto_run_after_take: Some(on),
            ..AutocutPatch::default()
        },
    }
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

/// One removed run of a clip, on the timeline before the cut.
struct Removed {
    take: TakeId,
    from: Frame,
    to: Frame,
}

/// The plan's cuts in timeline order: every removed run of every clip the
/// plan changes (clips gone from the timeline are left out), and the
/// silences those clips lose.
fn removed_runs(plan: &AutocutPlanView, state: &State) -> (Vec<Removed>, usize) {
    let clips = &state.project.timeline.clips;
    let mut planned: Vec<(usize, &crate::protocol::reply::AutocutClipView)> = plan
        .clips
        .iter()
        .filter_map(|view| Some((state.project.clip_index(&view.clip)?, view)))
        .collect();
    planned.sort_by_key(|(index, _)| *index);
    let mut runs = Vec::new();
    let mut silences = 0;
    for (index, view) in planned {
        let clip = &clips[index];
        let cut = ClipCut {
            kept: view.kept.clone(),
            silences: view.silences,
            removed_frames: view.removed_frames,
        };
        if !cut.changes(clip) {
            continue;
        }
        silences += view.silences;
        let start = position_of(clips, index);
        for [a, b] in cut.removed(clip) {
            runs.push(Removed {
                take: clip.take.clone(),
                from: start + (a - clip.in_),
                to: start + (b - clip.in_),
            });
        }
    }
    (runs, silences)
}

/// The summary of a dry run's plan.
pub fn summary(plan: &AutocutPlanView, state: &State) -> AutocutSummary {
    summary_with(plan, state, None)
}

/// The summary of a dry run's reply: its plan and its message (the core's
/// words for "nothing to cut" and the skipped clips).
pub fn summary_with(
    plan: &AutocutPlanView,
    state: &State,
    message: Option<&str>,
) -> AutocutSummary {
    let fps = state.project.canvas.fps;
    let tc = |f: Frame| format::timecode(f, fps);
    let (runs, silences) = removed_runs(plan, state);
    if runs.is_empty() {
        let why = message
            .map(nothing_to_cut_reason)
            .unwrap_or_else(|| default_reason(plan, state));
        return AutocutSummary {
            title: NOTHING_TO_CUT.to_owned(),
            rows: vec![(String::new(), why)],
            regions: Vec::new(),
        };
    }
    let removed_ns: i64 = runs
        .iter()
        .map(|r| frame_ns(r.to as i64, fps) - frame_ns(r.from as i64, fps))
        .sum();
    let title = format!(
        "Removes {silences} {} ({})",
        if silences == 1 { "silence" } else { "silences" },
        duration_label(removed_ns)
    );
    let clips = &state.project.timeline.clips;
    let mut takes: Vec<TakeId> = Vec::new();
    for run in &runs {
        if !takes.contains(&run.take) {
            takes.push(run.take.clone());
        }
    }
    let mut rows: Vec<(String, String)> = takes
        .iter()
        .map(|take| {
            let before = total_frames(
                &clips
                    .iter()
                    .filter(|c| &c.take == take)
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            let removed: Frame = runs
                .iter()
                .filter(|r| &r.take == take)
                .map(|r| r.to - r.from)
                .sum();
            (
                format!("Take {}", take.number()),
                format!("{} \u{2192} {}", tc(before), tc(before - removed)),
            )
        })
        .collect();
    let total = total_frames(clips);
    let removed: Frame = runs.iter().map(|r| r.to - r.from).sum();
    rows.push((
        "Timeline".to_owned(),
        format!("{} \u{2192} {}", tc(total), tc(total - removed)),
    ));
    if !plan.skipped.is_empty() {
        let skipped: Vec<String> = plan
            .skipped
            .iter()
            .take(3)
            .map(|s| format!("{} ({})", s.clip, s.reason))
            .collect();
        let more = if plan.skipped.len() > 3 {
            ", \u{2026}"
        } else {
            ""
        };
        rows.push((
            "Skipped".to_owned(),
            format!("{}{more}", skipped.join(", ")),
        ));
    }
    let regions = runs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let frames = r.to - r.from;
            let ns = frame_ns(r.to as i64, fps) - frame_ns(r.from as i64, fps);
            RegionRow {
                n: i + 1,
                range: format!("{} \u{2192} {}", tc(r.from), tc(r.to)),
                size: format!("{frames} f \u{b7} {}", duration_label(ns)),
                seek: r.from,
            }
        })
        .collect();
    AutocutSummary {
        title,
        rows,
        regions,
    }
}

/// The core's "nothing to cut" message as the summary's one row: its
/// "Dry run: nothing to cut: " lead dropped, the rest capitalized.
fn nothing_to_cut_reason(message: &str) -> String {
    let lower = message.to_lowercase();
    let mut rest = message;
    for lead in ["dry run: nothing to cut: ", "nothing to cut: "] {
        if lower.starts_with(lead) {
            rest = &message[lead.len()..];
            break;
        }
    }
    format::capitalized(rest.trim())
}

/// The reason without the core's message: as the core words it, at the
/// project's threshold.
fn default_reason(plan: &AutocutPlanView, state: &State) -> String {
    if plan.clips.is_empty() {
        "No clip could be analyzed".to_owned()
    } else {
        format!(
            "No silence long enough to cut at {}",
            db_label(state.project.autocut.threshold_db)
        )
    }
}

/// What the timeline hatches for a dry run: each clip minus what it keeps,
/// in timeline frames before the cut.
pub fn preview_spans(plan: &AutocutPlanView, state: &State) -> Vec<AutocutPreviewSpan> {
    let track = state.project.autocut.track.clone();
    preview_spans_for(plan, state, track.as_ref())
}

/// [`preview_spans`] on the lane of the track the dry run analyzes for
/// each clip's take: `track` (the draft's), else the setting, else the
/// take scene's first mic, else its first track
/// ([`choose_track`](crate::autocut::choose_track)).
pub fn preview_spans_for(
    plan: &AutocutPlanView,
    state: &State,
    track: Option<&TrackId>,
) -> Vec<AutocutPreviewSpan> {
    let fps = state.project.canvas.fps;
    let clips = &state.project.timeline.clips;
    let mut planned: Vec<(usize, &crate::protocol::reply::AutocutClipView)> = plan
        .clips
        .iter()
        .filter_map(|view| Some((state.project.clip_index(&view.clip)?, view)))
        .collect();
    planned.sort_by_key(|(index, _)| *index);
    let mut spans = Vec::new();
    for (index, view) in planned {
        let clip = &clips[index];
        let cut = ClipCut {
            kept: view.kept.clone(),
            silences: view.silences,
            removed_frames: view.removed_frames,
        };
        if !cut.changes(clip) {
            continue;
        }
        let lane = state
            .project
            .take(&clip.take)
            .and_then(|take| crate::autocut::choose_track(&state.project, take, track));
        let Some(lane) = lane else {
            continue;
        };
        let start = position_of(clips, index);
        for [a, b] in cut.removed(clip) {
            let ns = frame_ns(b as i64, fps) - frame_ns(a as i64, fps);
            spans.push(AutocutPreviewSpan {
                clip: clip.id.clone(),
                track: lane.clone(),
                from: start + (a - clip.in_),
                to: start + (b - clip.in_),
                tag: format!("{MINUS}{}", duration_label(ns)),
            });
        }
    }
    spans
}

/// The panel subtitle while previewing: "Previewing take 8 · c3e9",
/// "Previewing 4 clips", "Previewing all 10 clips".
pub fn subtitle(state: &State) -> String {
    let clips = &state.project.timeline.clips;
    let selected: Vec<_> = state
        .ui
        .selection
        .iter()
        .filter_map(|id| state.project.clip(id))
        .collect();
    match selected.as_slice() {
        [] => format!(
            "Previewing all {} {}",
            format::count(clips.len() as u64),
            if clips.len() == 1 { "clip" } else { "clips" }
        ),
        [clip] => format!("Previewing take {} \u{b7} {}", clip.take.number(), clip.id),
        many => format!("Previewing {} clips", format::count(many.len() as u64)),
    }
}

/// Whether the page opens now (the `autocut.page` hint, G§4.3): clips on
/// the timeline and the gates of `Autocut` (Idle).
pub fn page_enabled(state: &State) -> Result<(), String> {
    if state.project.timeline.clips.is_empty() {
        return Err("The timeline is empty".to_owned());
    }
    crate::view::enabled(
        state,
        &Command::Autocut {
            scope: ClipScope::All,
            overrides: AutocutOverrides::default(),
            dry_run: true,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ClipId;
    use crate::protocol::reply::{AutocutClipView, AutocutSkipView};
    use crate::state::{CanvasMode, RecordMode, RecordPhase};
    use crate::testkit;

    fn id(s: &str) -> ClipId {
        s.parse().unwrap()
    }

    /// S12: Program, c3e9 selected, zoomed to it.
    fn s12() -> State {
        let mut state = testkit::design_state();
        state.ui.canvas_mode = CanvasMode::Program;
        state.ui.selection = vec![id("c3e9")];
        state.ui.playhead = 4842;
        state
    }

    /// R40, R76: a dry run is keyed on what its answer is worked out on.
    /// A trim under the same selection, or its undo, leaves the command as
    /// it was but changes the key, and the plan shown goes stale; a seek
    /// changes neither; a knob changes the command but leaves the plan's
    /// frames right; the setting's track counts for a draft that names no
    /// track only.
    #[test]
    fn a_dry_run_is_keyed_on_the_timeline() {
        let ctx = testkit::ctx(testkit::CREATED_AT, 1);
        let run = |state: &State, cmd: Command| crate::apply(state, &ctx, cmd).unwrap().state;
        let before = s12();
        let draft = AutocutDraft::from(&before.project);
        let key = dry_run_key(&before, &draft);
        // c2d1, before c3e9, 30 frames shorter: the scope stays c3e9.
        let trimmed = run(
            &before,
            Command::ClipTrim {
                clip: id("c2d1"),
                start: None,
                end: Some(crate::TimeArg::minus_frames(30)),
                timeline: false,
            },
        );
        assert_eq!(trimmed.ui.selection, [id("c3e9")]);
        assert_eq!(dry_run(&trimmed, &draft), dry_run(&before, &draft));
        let after = dry_run_key(&trimmed, &draft);
        assert_ne!(after, key);
        assert!(key.stale_for(&after));
        // Its undo: the first key again.
        let undone = run(&trimmed, Command::Undo { steps: 1 });
        assert_eq!(dry_run_key(&undone, &draft), key);
        // A seek changes nothing the plan is worked out on.
        let mut moved = before.clone();
        moved.ui.playhead = 30;
        assert_eq!(dry_run_key(&moved, &draft), key);
        // A knob: another command, the same frames.
        let tuned = Knob::Padding.set(&draft, 500.0);
        let tuned_key = dry_run_key(&before, &tuned);
        assert_ne!(tuned_key, key);
        assert!(!key.stale_for(&tuned_key));
        // "Use for autocut" A2: the track a draft without one analyzes.
        let set_a2 = |state: &State| {
            run(
                state,
                Command::AutocutSettings {
                    patch: AutocutPatch {
                        track: Nullable::Set("A2".parse().unwrap()),
                        ..AutocutPatch::default()
                    },
                },
            )
        };
        let mut unnamed = draft.clone();
        unnamed.track = None;
        let a2 = set_a2(&before);
        assert!(dry_run_key(&before, &unnamed).stale_for(&dry_run_key(&a2, &unnamed)));
        let mut named = draft.clone();
        named.track = Some("A1".parse().unwrap());
        assert_eq!(dry_run_key(&a2, &named), dry_run_key(&before, &named));
    }

    /// The dry run of the design fixture (BRIEF §3.6): `qrec autocut
    /// --clips c3e9 --dry-run` at −35 dB, 400 ms, 120 ms, 200 ms keeps take
    /// 8 but for frames 556–604, 1284–1326, 1990–2029 and 2662–2719.
    fn s12_plan() -> AutocutPlanView {
        AutocutPlanView {
            clips: vec![AutocutClipView {
                clip: id("c3e9"),
                kept: vec![
                    [0, 556],
                    [604, 1284],
                    [1326, 1990],
                    [2029, 2662],
                    [2719, 3105],
                ],
                removed_frames: 186,
                silences: 4,
            }],
            skipped: vec![],
        }
    }

    #[test]
    fn s12_summary_rows_and_regions() {
        let state = s12();
        let summary = summary(&s12_plan(), &state);
        assert_eq!(summary.title, "Removes 4 silences (6.2 s)");
        assert_eq!(
            summary.rows,
            [
                ("Take 8".to_owned(), "00:01:43:15 → 00:01:37:09".to_owned()),
                (
                    "Timeline".to_owned(),
                    "00:04:06:11 → 00:04:00:05".to_owned()
                ),
            ]
        );
        let regions: Vec<(String, String)> = summary
            .regions
            .iter()
            .map(|r| (r.range.clone(), r.size.clone()))
            .collect();
        assert_eq!(
            regions,
            [
                (
                    "00:02:41:12 → 00:02:43:00".to_owned(),
                    "48 f · 1.6 s".to_owned()
                ),
                (
                    "00:03:05:20 → 00:03:07:02".to_owned(),
                    "42 f · 1.4 s".to_owned()
                ),
                (
                    "00:03:29:06 → 00:03:30:15".to_owned(),
                    "39 f · 1.3 s".to_owned()
                ),
                (
                    "00:03:51:18 → 00:03:53:15".to_owned(),
                    "57 f · 1.9 s".to_owned()
                ),
            ]
        );
        assert_eq!(summary.regions[0].seek, 4842, "the canvas of S12");
        insta::assert_json_snapshot!("s12_summary", summary);
        insta::assert_json_snapshot!("s12_preview", preview_spans(&s12_plan(), &state));
        assert_eq!(subtitle(&state), "Previewing take 8 · c3e9");
    }

    #[test]
    fn nothing_to_cut_reads_the_cores_message() {
        let state = s12();
        let plan = AutocutPlanView {
            clips: vec![AutocutClipView {
                clip: id("c3e9"),
                kept: vec![[0, 3105]],
                removed_frames: 0,
                silences: 0,
            }],
            skipped: vec![],
        };
        let got = summary_with(
            &plan,
            &state,
            Some("Dry run: nothing to cut: no silence long enough to cut at \u{2212}35 dB"),
        );
        assert_eq!(got.title, "Nothing to cut");
        assert_eq!(
            got.rows,
            [(
                String::new(),
                "No silence long enough to cut at −35 dB".to_owned()
            )]
        );
        assert!(got.regions.is_empty());
        assert!(preview_spans(&plan, &state).is_empty());
        // Without the message: the same words, at the project's threshold.
        assert_eq!(summary_with(&plan, &state, None), got);
        let none = AutocutPlanView {
            clips: vec![],
            skipped: vec![AutocutSkipView {
                clip: id("c3e9"),
                reason: "entirely silent at \u{2212}35 dB".into(),
            }],
        };
        assert_eq!(
            summary(&none, &state).rows,
            [(String::new(), "No clip could be analyzed".to_owned())]
        );
    }

    #[test]
    fn several_takes_and_skips() {
        // All clips: c2d0 loses take 5 frames 120–158 and 288–303 (BRIEF
        // §3.3's silences), c3e9 its four; c1b6 is skipped.
        let mut state = testkit::design_state();
        state.ui.canvas_mode = CanvasMode::Program;
        let mut plan = s12_plan();
        plan.clips.insert(
            0,
            AutocutClipView {
                clip: id("c2d0"),
                kept: vec![[12, 120], [158, 288], [303, 1104]],
                removed_frames: 53,
                silences: 2,
            },
        );
        plan.clips.push(AutocutClipView {
            clip: id("c7f3"),
            kept: vec![[0, 410]],
            removed_frames: 0,
            silences: 0,
        });
        plan.skipped.push(AutocutSkipView {
            clip: id("c1b6"),
            reason: "entirely silent at \u{2212}35 dB".into(),
        });
        let summary = summary(&plan, &state);
        insta::assert_json_snapshot!("all_clips_summary", summary);
        assert_eq!(summary.title, "Removes 6 silences (8.0 s)");
        assert_eq!(subtitle(&state), "Previewing all 10 clips");
        state.ui.selection = vec![id("c2d0"), id("c2d1"), id("c3e9"), id("c1b6")];
        assert_eq!(subtitle(&state), "Previewing 4 clips");
        let spans = preview_spans_for(&plan, &state, Some(&TrackId::numbered(2)));
        assert_eq!(spans.len(), 6);
        assert!(spans.iter().all(|s| s.track == TrackId::numbered(2)));
        // c2d0 starts at frame 2084 (00:01:09:14): its first span is
        // timeline 2084 + 108 … 2084 + 146.
        assert_eq!((spans[0].from, spans[0].to), (2192, 2230));
        assert_eq!(spans[0].tag, "\u{2212}1.3 s");
    }

    #[test]
    fn the_d15_command_sequence() {
        let state = s12();
        let draft = AutocutDraft::from(&state.project);
        let json = |cmds: Vec<Command>| -> Vec<serde_json::Value> {
            cmds.iter().map(Command::to_json).collect()
        };
        // Nothing changed: only the cut.
        let unchanged = apply(&state, &draft);
        // Two changes: the settings first (only those fields), then the cut.
        let tuned = Knob::Padding.set(&Knob::Threshold.set(&draft, -42.4), 87.0);
        let tuned = AutocutDraft {
            track: Some(TrackId::numbered(2)),
            ..tuned
        };
        insta::assert_json_snapshot!(
            "d15_commands",
            serde_json::json!({
                "dryRun": dry_run(&state, &tuned).to_json(),
                "applyUnchanged": json(unchanged),
                "applyTuned": json(apply(&state, &tuned)),
                "autoRun": auto_run(true).to_json(),
            })
        );
        assert_eq!(tuned.threshold_db, -42.0, "rounded to the step");
        assert_eq!(tuned.padding_ms, 90);
        // Back to the saved values: nothing to save.
        let back = Knob::Threshold.set(&tuned, -35.0);
        let back = Knob::Padding.set(&back, 120.0);
        let back = AutocutDraft {
            track: Some(TrackId::numbered(1)),
            ..back
        };
        assert_eq!(changed(&state, &back), AutocutPatch::default());
        // A draft that unsets the track clears it.
        let mut cleared = state.clone();
        cleared.project.autocut.track = None;
        let unset = AutocutDraft::from(&cleared.project);
        assert_eq!(changed(&cleared, &unset), AutocutPatch::default());
        assert_eq!(
            changed(
                &state,
                &AutocutDraft {
                    track: None,
                    ..draft.clone()
                }
            )
            .track,
            Nullable::Clear
        );
        // Scope: every clip without a selection.
        let mut all = state.clone();
        all.ui.selection.clear();
        assert!(matches!(
            dry_run(&all, &draft),
            Command::Autocut {
                scope: ClipScope::All,
                dry_run: true,
                ..
            }
        ));
    }

    #[test]
    fn knobs_and_tracks() {
        let state = s12();
        let draft = AutocutDraft::from(&state.project);
        let knobs: Vec<serde_json::Value> = Knob::ALL
            .iter()
            .map(|k| {
                serde_json::json!({
                    "label": k.label(),
                    "unit": k.unit(),
                    "text": k.text(&draft),
                    "ends": k.end_labels(),
                })
            })
            .collect();
        insta::assert_json_snapshot!(
            "knobs",
            serde_json::json!({"knobs": knobs, "tracks": track_items(&state)})
        );
        assert_eq!(Knob::Threshold.parse("\u{2212}35 dB"), Some(-35.0));
        // The field's own alias parses too (R47: "−40 db" quietly reverted).
        assert_eq!(Knob::Threshold.parse("-40 db"), Some(-40.0));
        assert_eq!(Knob::Threshold.parse("-40db"), Some(-40.0));
        assert_eq!(Knob::Threshold.units(), ["dB", "db"]);
        assert_eq!(Knob::Padding.units(), ["ms"]);
        assert_eq!(Knob::MinKeep.parse("200ms"), Some(200.0));
        assert_eq!(Knob::MinKeep.parse("a lot"), None);
        assert_eq!(Knob::MinSilence.set(&draft, 5000.0).min_silence_ms, 2000);
        assert_eq!(Knob::MinSilence.set(&draft, 0.0).min_silence_ms, 100);
        assert_eq!(Knob::MinSilence.set(&draft, f64::NAN).min_silence_ms, 400);
        assert_eq!(shown_track(&state, &draft), Some(TrackId::numbered(1)));
        let none = AutocutDraft {
            track: None,
            ..draft
        };
        assert_eq!(
            shown_track(&state, &none),
            Some(TrackId::numbered(1)),
            "the first mic"
        );
    }

    /// One rule for the track autocut listens to (R77): the setting, else
    /// the scene's first mic, else its first track. The dropdown, the
    /// panel's "Autocut" tag, the lane's scissors and the preview's hatches
    /// all name the track the dry run analyzes, a scene without a mic
    /// included, and a draft that leaves the track to the setting follows
    /// the setting.
    #[test]
    fn every_view_names_the_track_autocut_analyzes() {
        let plan = AutocutPlanView {
            clips: vec![AutocutClipView {
                clip: id("c0a1"),
                kept: vec![[0, 40], [60, 100]],
                removed_frames: 20,
                silences: 1,
            }],
            skipped: vec![],
        };
        // Every scene of one to three tracks, each a mic or system audio.
        for n in 1..=3u32 {
            for mics in 0..(1u32 << n) {
                let mut builder = testkit::ProjectBuilder::new().screen_layer("L1", "DP-1");
                for i in 0..n {
                    let track = format!("A{}", i + 1);
                    builder = if mics & (1 << i) != 0 {
                        builder.mic(&track, "a.mic")
                    } else {
                        builder.system(&track, "a.monitor")
                    };
                }
                let base = builder
                    .take("take-0001", 100)
                    .clip("c0a1", "take-0001", 0, 100)
                    .build();
                // The setting unset, or on each track.
                let settings =
                    std::iter::once(None).chain((1..=n).map(|i| Some(TrackId::numbered(i))));
                for setting in settings {
                    let mut project = base.clone();
                    project.autocut.track = setting.clone();
                    let state = testkit::state(project);
                    let take = state.project.take(&TakeId::numbered(1)).unwrap();
                    let analyzed = crate::autocut::choose_track(&state.project, take, None);
                    assert!(analyzed.is_some(), "{n} tracks, mics {mics:b}");
                    let case = format!("{n} tracks, mics {mics:b}, setting {setting:?}");
                    let draft = AutocutDraft::from(&state.project);
                    assert_eq!(
                        shown_track(&state, &draft),
                        analyzed,
                        "the dropdown: {case}"
                    );
                    // A draft that names no track: the dry run's override
                    // is unset, so it analyzes what the setting says.
                    let unset = AutocutDraft {
                        track: None,
                        ..draft.clone()
                    };
                    assert_eq!(shown_track(&state, &unset), analyzed, "unset: {case}");
                    let tagged: Vec<TrackId> =
                        crate::view::panel::audio_rows(&state, &Default::default())
                            .into_iter()
                            .filter(|r| r.autocut)
                            .map(|r| r.track)
                            .collect();
                    assert_eq!(Some(tagged), analyzed.clone().map(|t| vec![t]), "{case}");
                    let scissors: Vec<TrackId> = crate::view::timeline::lanes(&state)
                        .into_iter()
                        .filter(|l| l.autocut)
                        .map(|l| l.track)
                        .collect();
                    assert_eq!(Some(scissors), analyzed.clone().map(|t| vec![t]), "{case}");
                    for draft in [&draft, &unset] {
                        let spans = preview_spans_for(&plan, &state, draft.track.as_ref());
                        assert_eq!(spans.len(), 1, "the hatch: {case}");
                        assert_eq!(Some(&spans[0].track), analyzed.as_ref(), "{case}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_page_needs_clips_and_idle() {
        let mut state = s12();
        assert_eq!(page_enabled(&state), Ok(()));
        state.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        assert_eq!(page_enabled(&state), Err("Not while recording".into()));
        let empty = testkit::state(testkit::ProjectBuilder::new().build());
        assert_eq!(page_enabled(&empty), Err("The timeline is empty".into()));
    }
}
