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
//! - A previewed cut can be kept (issue #15): a click on its hatch or its
//!   row's box toggles it ([`toggle_keep`]). The kept ones are the draft's
//!   `keep`, sent with each dry run and with Apply's cut, never saved.

use serde::{Deserialize, Serialize};

use crate::autocut::{ClipCut, db_label};
use crate::ids::{ClipId, Frame, TakeId, TrackId};
use crate::model::{AudioTrack, Clip, Project};
use crate::protocol::command::{AutocutKeep, AutocutOverrides, AutocutPatch, ClipScope, Command};
use crate::protocol::reply::AutocutPlanView;
use crate::state::{JobKind, State};
use crate::time::{duration_label, frame_ns};
use crate::timeline::{position_of, total_frames};
use crate::view::AutocutPreviewSpan;
use crate::view::format::{self, MINUS};
use crate::view::timeline::HatchKind;

/// The sliders' ranges: (min, max, step).
pub const THRESHOLD_DB: (f64, f64, f64) = (-60.0, -10.0, 1.0);
pub const MIN_SILENCE_MS: (u32, u32, u32) = (100, 2000, 50);
pub const PADDING_MS: (u32, u32, u32) = (0, 500, 10);
pub const MIN_KEEP_MS: (u32, u32, u32) = (0, 1000, 10);

/// The page's title.
pub const TITLE: &str = "Autocut";
/// The summary title while a dry run takes longer than the cue's delay and
/// no analysis says which take (with a 16 px spinner, D-15; issue #15's
/// [`analyzing`] names it otherwise).
pub const ANALYZING: &str = "Analyzing\u{2026}";
/// The summary title when the plan cuts nothing.
pub const NOTHING_TO_CUT: &str = "Nothing to cut";
/// The summary title when an edges-only plan trims nothing.
pub const NOTHING_TO_TRIM: &str = "Nothing to trim";
/// The check box under the sliders (issue #15, board 02).
pub const EDGES_ONLY: &str = "Trim edges only";
/// Its caption.
pub const EDGES_ONLY_CAPTION: &str = "Keeps the pauses inside each clip.";
/// The summary title on an empty timeline: no dry run goes.
pub const NOTHING_YET: &str = "Nothing to cut yet";
/// Its one row: what to do for a preview.
pub const RECORD_FIRST: &str = "Record a take to see what these settings cut.";
/// The primary button: settings and the cut.
pub const APPLY: &str = "Apply autocut";
/// The primary button on an empty timeline: the settings only.
pub const SAVE: &str = "Save";
/// The note under the switch (S12).
pub const NOTE: &str =
    "Removed audio and video come back if you drag a clip edge out, or with Ctrl+Z.";
/// The switch's label (S12).
pub const AUTO_RUN: &str = "Run autocut after every take";
/// The dropdown's label (S12).
pub const DETECT_ON: &str = "Detect silence on";
/// Under the tracks in the dropdown's menu (issue #15, board 03).
pub const DETECT_ON_CAPTION: &str =
    "Cuts only where every checked track is silent. Sound on any of them is kept.";

/// The page's settings while tuning: a draft, no history (D-15).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocutDraft {
    /// "Detect silence on": the tracks named; empty leaves them to the
    /// setting (which may change while the page is open).
    #[serde(default)]
    pub tracks: Vec<TrackId>,
    pub threshold_db: f64,
    pub min_silence_ms: u32,
    pub padding_ms: u32,
    pub min_keep_ms: u32,
    /// "Trim edges only".
    #[serde(default)]
    pub edges_only: bool,
    /// The previewed cuts kept (a click on a hatch or a row's box): sent
    /// with each dry run and with Apply's cut; not a setting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keep: Vec<AutocutKeep>,
}

impl From<&Project> for AutocutDraft {
    /// The project's autocut settings.
    fn from(project: &Project) -> Self {
        let a = &project.autocut;
        Self {
            tracks: a.tracks.clone(),
            threshold_db: a.threshold_db,
            min_silence_ms: a.min_silence_ms,
            padding_ms: a.padding_ms,
            min_keep_ms: a.min_keep_ms,
            edges_only: a.edges_only,
            keep: Vec::new(),
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

    /// What the knob does, exactly: its tooltip on the autocut page and in
    /// Settings › Autocut (the owner asked for them, 2026-11-14).
    pub fn help(self) -> &'static str {
        match self {
            Knob::Threshold => {
                "Audio quieter than this counts as silence. Raise it (toward 0 dB) if \
                 background noise hides your pauses; lower it if quiet words get cut."
            }
            Knob::MinSilence => {
                "Only quiet stretches at least this long can be cut. Shorter pauses, \
                 like the ones between words, always stay."
            }
            Knob::Padding => {
                "Silence kept on each side of every cut, next to the sound, so words \
                 don't start or end abruptly. The timeline shows it in blue."
            }
            Knob::MinKeep => {
                "The shortest sound worth keeping. A shorter one left between \
                 silences, like a click or a breath, is cut with them; padding \
                 doesn't count."
            }
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
    /// "A1 · Yeti Nano": how the value names it among several.
    pub name: String,
    /// The track's label alone ("Yeti Nano") and role, for the menu's row.
    pub title: String,
    pub role: crate::model::AudioRole,
}

/// The dropdown's items (S12): every track of the project scene, then a
/// track the scope's clips are analyzed on that the project scene no
/// longer has (a take recorded with it: R77's audit), named as that take
/// names it.
pub fn track_items(state: &State) -> Vec<TrackItem> {
    let project = &state.project;
    let mut tracks: Vec<&AudioTrack> = project.scene.audio.iter().collect();
    for id in crate::autocut::analyzed_tracks(project, scope_clips(state), &[]) {
        if tracks.iter().any(|t| t.id == id) {
            continue;
        }
        let named = scope_clips(state)
            .filter_map(|c| project.take(&c.take))
            .find_map(|take| take.scene.track(&id));
        if let Some(track) = named {
            tracks.push(track);
        }
    }
    tracks
        .into_iter()
        .map(|t| TrackItem {
            track: t.id.clone(),
            label: format!("{} \u{b7} {} ({})", t.id, t.label, t.role.as_str()),
            name: format!("{} \u{b7} {}", t.id, t.label),
            title: t.label.clone(),
            role: t.role,
        })
        .collect()
}

/// The tracks the dropdown checks for `draft`: its own, else the
/// project's setting (a draft that names none leaves them to the setting,
/// which may change while the page is open), else the tracks the dry run
/// analyzes its scope's clips on (each take's
/// [`default_track`](crate::autocut::default_track):
/// [`analyzed_tracks`](crate::autocut::analyzed_tracks)), else, with no
/// clip, the project scene's.
pub fn shown_tracks(state: &State, draft: &AutocutDraft) -> Vec<TrackId> {
    let project = &state.project;
    if !draft.tracks.is_empty() {
        return draft.tracks.clone();
    }
    if !project.autocut.tracks.is_empty() {
        return project.autocut.tracks.clone();
    }
    default_tracks(state)
}

/// What a draft and a setting that name no track analyze: each take's
/// first mic ([`analyzed_tracks`](crate::autocut::analyzed_tracks) of the
/// scope), else, with no clip, the project scene's.
fn default_tracks(state: &State) -> Vec<TrackId> {
    let project = &state.project;
    let analyzed = crate::autocut::analyzed_tracks(project, scope_clips(state), &[]);
    if !analyzed.is_empty() {
        return analyzed;
    }
    crate::autocut::default_track(&project.scene)
        .map(|t| vec![t.id.clone()])
        .unwrap_or_default()
}

/// The dropdown's value: the item's label for one track ("A1 · Yeti Nano
/// (mic)"), "A1 · Yeti Nano + A2 · System audio" for several.
pub fn shown_label(state: &State, draft: &AutocutDraft) -> String {
    let items = track_items(state);
    let shown = shown_tracks(state, draft);
    let name = |id: &TrackId| {
        items
            .iter()
            .find(|i| &i.track == id)
            .map(|i| i.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    match shown.as_slice() {
        [one] => items
            .iter()
            .find(|i| &i.track == one)
            .map_or_else(|| one.to_string(), |i| i.label.clone()),
        many => many.iter().map(name).collect::<Vec<_>>().join(" + "),
    }
}

/// The draft's tracks once `track` is checked or unchecked in the
/// dropdown, in the dropdown's order. The last checked track stays (its
/// box is disabled): `None`. A list equal to what naming none analyzes
/// goes back to naming none, while the setting names none too.
pub fn toggled_tracks(
    state: &State,
    draft: &AutocutDraft,
    track: &TrackId,
) -> Option<Vec<TrackId>> {
    let mut shown = shown_tracks(state, draft);
    if shown.contains(track) {
        if shown.len() == 1 {
            return None;
        }
        shown.retain(|id| id != track);
    } else {
        shown.push(track.clone());
    }
    let order: Vec<TrackId> = track_items(state).into_iter().map(|i| i.track).collect();
    shown.sort_by_key(|id| order.iter().position(|o| o == id).unwrap_or(usize::MAX));
    if state.project.autocut.tracks.is_empty() && shown == default_tracks(state) {
        return Some(Vec::new());
    }
    Some(shown)
}

/// The clips the page analyzes ([`scope`]), in timeline order.
fn scope_clips(state: &State) -> impl Iterator<Item = &Clip> {
    let selection = &state.ui.selection;
    state
        .project
        .timeline
        .clips
        .iter()
        .filter(move |c| selection.is_empty() || selection.contains(&c.id))
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
        tracks: draft.tracks.clone(),
        threshold_db: Some(draft.threshold_db),
        min_silence_ms: Some(draft.min_silence_ms),
        padding_ms: Some(draft.padding_ms),
        min_keep_ms: Some(draft.min_keep_ms),
        edges_only: Some(draft.edges_only),
        keep: draft.keep.clone(),
    }
}

/// The tooltip of a previewed cut under the pointer: what a click does.
pub fn span_tip(kept: bool) -> &'static str {
    if kept {
        "Click to cut this silence"
    } else {
        "Click to keep this silence"
    }
}

/// `keep` with the cut `range` (`[in, out)` take frames of `clip`)
/// toggled: the ranges overlapping it go (it is cut again), or it joins
/// them (it is kept). What a click on a hatch or a region's box does.
pub fn toggle_keep(keep: &[AutocutKeep], clip: &ClipId, range: [Frame; 2]) -> Vec<AutocutKeep> {
    let [a, b] = range;
    let overlaps = |k: &AutocutKeep| &k.clip == clip && k.in_ < b && a < k.out;
    if keep.iter().any(overlaps) {
        keep.iter().filter(|k| !overlaps(k)).cloned().collect()
    } else {
        let mut next = keep.to_vec();
        next.push(AutocutKeep {
            clip: clip.clone(),
            in_: a,
            out: b,
        });
        next
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
/// And the project's `autocut.tracks` while the draft names none: the
/// tracks the core then analyzes.
#[derive(Debug, Clone, PartialEq)]
pub struct DryRunKey {
    pub cmd: Command,
    pub clips: Vec<Clip>,
    /// The setting's tracks when the draft leaves the tracks to it.
    pub setting_tracks: Vec<TrackId>,
}

impl DryRunKey {
    /// A plan shown for `self` no longer matches `next`'s timeline (or
    /// track): its frames are out of date, not only its draft or scope.
    pub fn stale_for(&self, next: &DryRunKey) -> bool {
        self.clips != next.clips || self.setting_tracks != next.setting_tracks
    }
}

/// The key of the dry run of `draft` on `state`: the page asks again when
/// it changes, not only when the command does.
pub fn dry_run_key(state: &State, draft: &AutocutDraft) -> DryRunKey {
    DryRunKey {
        cmd: dry_run(state, draft),
        clips: state.project.timeline.clips.clone(),
        setting_tracks: if draft.tracks.is_empty() {
            state.project.autocut.tracks.clone()
        } else {
            Vec::new()
        },
    }
}

/// The draft's fields that differ from the project's settings.
pub fn changed(state: &State, draft: &AutocutDraft) -> AutocutPatch {
    let saved = AutocutDraft::from(&state.project);
    AutocutPatch {
        auto_run_after_take: None,
        tracks: (draft.tracks != saved.tracks).then(|| draft.tracks.clone()),
        threshold_db: (draft.threshold_db != saved.threshold_db).then_some(draft.threshold_db),
        min_silence_ms: (draft.min_silence_ms != saved.min_silence_ms)
            .then_some(draft.min_silence_ms),
        padding_ms: (draft.padding_ms != saved.padding_ms).then_some(draft.padding_ms),
        min_keep_ms: (draft.min_keep_ms != saved.min_keep_ms).then_some(draft.min_keep_ms),
        edges_only: (draft.edges_only != saved.edges_only).then_some(draft.edges_only),
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
    if previews(state) {
        commands.push(Command::Autocut {
            scope: scope(state),
            overrides: AutocutOverrides {
                keep: draft.keep.clone(),
                ..AutocutOverrides::default()
            },
            dry_run: false,
        });
    }
    commands
}

/// Whether the page previews a cut: the timeline has clips. On an empty
/// one it opens all the same, to set autocut up before the first take
/// (the owner asked, 2026-11-14), sends no dry run and shows
/// [`empty_summary`]; its primary button reads [`SAVE`] and saves the
/// settings only ([`apply`]).
pub fn previews(state: &State) -> bool {
    !state.project.timeline.clips.is_empty()
}

/// The primary button's label (and the hint of its `Enter`): [`APPLY`],
/// or [`SAVE`] on an empty timeline.
pub fn apply_label(state: &State) -> &'static str {
    if previews(state) { APPLY } else { SAVE }
}

/// The summary on an empty timeline: "Nothing to cut yet", and what to do.
pub fn empty_summary() -> AutocutSummary {
    AutocutSummary {
        title: NOTHING_YET.to_owned(),
        kept: String::new(),
        rows: vec![(String::new(), RECORD_FIRST.to_owned())],
        regions: Vec::new(),
    }
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
    /// "Removes 4 silences (6.2 s)", "Trims 2 edges (3.1 s)"; "Nothing to
    /// cut", "Nothing to trim".
    pub title: String,
    /// " · 1 kept" after the title when previewed cuts are kept; empty
    /// otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kept: String,
    /// ("Take 8", "00:01:43:15 → 00:01:37:09") per take, then
    /// ("Timeline", "00:04:06:11 → 00:04:00:05").
    pub rows: Vec<(String, String)>,
    pub regions: Vec<RegionRow>,
}

/// One region of the list: a click seeks to it; its box (checked = cut)
/// toggles it ([`toggle_keep`] with `clip` and `cut`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionRow {
    pub n: usize,
    /// "00:02:41:12 → 00:02:43:00".
    pub range: String,
    /// "48 f · 1.6 s"; "kept" when kept.
    pub size: String,
    pub seek: Frame,
    /// The clip the cut is in, and the cut in its take's frames.
    pub clip: ClipId,
    pub cut: [Frame; 2],
    /// A previewed cut kept: its box unchecked, the row dimmed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub kept: bool,
}

/// The region list's word for a kept cut.
pub const KEPT: &str = "kept";

/// One removed (or kept) run of a clip, on the timeline before the cut.
struct Removed {
    take: TakeId,
    clip: ClipId,
    /// In the take's frames.
    cut: [Frame; 2],
    from: Frame,
    to: Frame,
    kept: bool,
}

/// The plan's cuts in timeline order: every removed run of every clip the
/// plan changes, and every run it keeps (clips gone from the timeline are
/// left out), and the silences those clips lose.
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
        let cut = clip_cut(view);
        let start = position_of(clips, index);
        let mut push = |[a, b]: [Frame; 2], kept: bool| {
            runs.push(Removed {
                take: clip.take.clone(),
                clip: clip.id.clone(),
                cut: [a, b],
                from: start + (a - clip.in_),
                to: start + (b - clip.in_),
                kept,
            })
        };
        for &run in &view.excepted {
            push(run, true);
        }
        if cut.changes(clip) {
            silences += view.silences;
            for run in cut.removed(clip) {
                push(run, false);
            }
        }
    }
    runs.sort_by_key(|r| r.from);
    (runs, silences)
}

/// A clip's cut as the plan gives it.
fn clip_cut(view: &crate::protocol::reply::AutocutClipView) -> ClipCut {
    ClipCut {
        kept: view.kept.clone(),
        silences: view.silences,
        removed_frames: view.removed_frames,
        excepted: view.excepted.clone(),
    }
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
    let (all, silences) = removed_runs(plan, state);
    let kept_count = all.iter().filter(|r| r.kept).count();
    let kept = if kept_count > 0 {
        format!(" \u{b7} {kept_count} {KEPT}")
    } else {
        String::new()
    };
    let regions: Vec<RegionRow> = all
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let frames = r.to - r.from;
            let ns = frame_ns(r.to as i64, fps) - frame_ns(r.from as i64, fps);
            RegionRow {
                n: i + 1,
                range: format!("{} \u{2192} {}", tc(r.from), tc(r.to)),
                size: if r.kept {
                    KEPT.to_owned()
                } else {
                    format!("{frames} f \u{b7} {}", duration_label(ns))
                },
                seek: r.from,
                clip: r.clip.clone(),
                cut: r.cut,
                kept: r.kept,
            }
        })
        .collect();
    let runs: Vec<&Removed> = all.iter().filter(|r| !r.kept).collect();
    if runs.is_empty() {
        let why = message
            .map(nothing_to_cut_reason)
            .unwrap_or_else(|| default_reason(plan, state));
        return AutocutSummary {
            title: if plan.edges_only {
                NOTHING_TO_TRIM
            } else {
                NOTHING_TO_CUT
            }
            .to_owned(),
            kept,
            rows: vec![(String::new(), why)],
            regions,
        };
    }
    let removed_ns: i64 = runs
        .iter()
        .map(|r| frame_ns(r.to as i64, fps) - frame_ns(r.from as i64, fps))
        .sum();
    let title = if plan.edges_only {
        format!(
            "Trims {silences} {} ({})",
            if silences == 1 { "edge" } else { "edges" },
            duration_label(removed_ns)
        )
    } else {
        format!(
            "Removes {silences} {} ({})",
            if silences == 1 { "silence" } else { "silences" },
            duration_label(removed_ns)
        )
    };
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
    AutocutSummary {
        title,
        kept,
        rows,
        regions,
    }
}

/// The core's "nothing to cut" message as the summary's one row: its
/// "Dry run: nothing to cut: " lead dropped, the rest capitalized.
fn nothing_to_cut_reason(message: &str) -> String {
    let lower = message.to_lowercase();
    let mut rest = message;
    for lead in [
        "dry run: nothing to cut: ",
        "nothing to cut: ",
        "dry run: nothing to trim: ",
        "nothing to trim: ",
    ] {
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
    } else if plan.clips.iter().any(|c| !c.excepted.is_empty()) {
        "Every cut is kept".to_owned()
    } else if plan.edges_only {
        format!(
            "No silence long enough at the clips' edges at {}",
            db_label(state.project.autocut.threshold_db)
        )
    } else {
        format!(
            "No silence long enough to cut at {}",
            db_label(state.project.autocut.threshold_db)
        )
    }
}

/// What the timeline hatches for a dry run, in timeline frames before the
/// cut: each clip minus what it keeps (`HatchKind::Cut`, tagged "−1.6 s"),
/// the project's padding beside each cut (`HatchKind::Padding`), and the
/// cuts kept (`HatchKind::Kept`, tagged "kept").
pub fn preview_spans(plan: &AutocutPlanView, state: &State) -> Vec<AutocutPreviewSpan> {
    preview_spans_for(plan, state, state.project.autocut.padding_ms)
}

/// [`preview_spans`] on the lane of each track the dry run analyzed the
/// clip on (`tracks` of its plan; the tag on the first one only, as one
/// cut); with the padding the dry run kept, `padding_ms` (the draft's).
pub fn preview_spans_for(
    plan: &AutocutPlanView,
    state: &State,
    padding_ms: u32,
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
        let cut = clip_cut(view);
        let start = position_of(clips, index);
        let mut runs: Vec<([Frame; 2], HatchKind)> = view
            .excepted
            .iter()
            .map(|&r| (r, HatchKind::Kept))
            .collect();
        if cut.changes(clip) {
            runs.extend(cut.removed(clip).into_iter().map(|r| (r, HatchKind::Cut)));
            runs.extend(
                cut.padding(clip, padding_ms, fps)
                    .into_iter()
                    .map(|r| (r, HatchKind::Padding)),
            );
        }
        runs.sort_by_key(|([a, _], _)| *a);
        for (n, lane) in view.tracks.iter().enumerate() {
            for &([a, b], kind) in &runs {
                let tag = match kind {
                    HatchKind::Cut if n == 0 => {
                        let ns = frame_ns(b as i64, fps) - frame_ns(a as i64, fps);
                        format!("{MINUS}{}", duration_label(ns))
                    }
                    HatchKind::Kept if n == 0 => KEPT.to_owned(),
                    _ => String::new(),
                };
                spans.push(AutocutPreviewSpan {
                    clip: clip.id.clone(),
                    track: lane.clone(),
                    from: start + (a - clip.in_),
                    to: start + (b - clip.in_),
                    cut: [a, b],
                    kind,
                    tag,
                });
            }
        }
    }
    spans
}

/// What the summary shows while the dry run analyzes audio (issue #15,
/// board 07): the spinner's title, how far along, and the clips whose
/// take plates spin on the timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzingView {
    /// "Analyzing take 8", "Analyzing 3 takes".
    pub title: String,
    /// 0.0–1.0: what ffmpeg has read of the files analyzed, each weighing
    /// its length (the bar). `None` when no such file's length is known.
    pub fraction: Option<f64>,
    /// "40%" (rounded down, so never 100% while it runs); `None` with
    /// `fraction`.
    pub percent: Option<String>,
    /// The clips whose audio is being analyzed, in the jobs' target order.
    pub clips: Vec<ClipId>,
}

/// [`AnalyzingView`] of the dry runs waiting for an analysis (the page
/// sends one at a time; another client's preview would count too). Only
/// the takes and files still analyzed count: a run whose silences are all
/// known never waits, and a file analyzed earlier isn't read again.
/// `None` when no dry run analyzes anything.
pub fn analyzing(state: &State) -> Option<AnalyzingView> {
    let mut takes: Vec<TakeId> = Vec::new();
    let mut clips: Vec<ClipId> = Vec::new();
    let (mut done, mut total) = (0u128, 0u128);
    for job in state.runtime.jobs.values() {
        let JobKind::Autocut {
            dry_run: true,
            targets,
            pending,
            analysis,
            ..
        } = &job.kind
        else {
            continue;
        };
        if pending.is_empty() {
            continue;
        }
        for target in targets {
            if !target.tracks.iter().any(|s| pending.contains(&s.key)) {
                continue;
            }
            if !takes.contains(&target.clip.take) {
                takes.push(target.clip.take.clone());
            }
            if !clips.contains(&target.clip.id) {
                clips.push(target.clip.id.clone());
            }
        }
        for read in analysis.values() {
            done += u128::from(read.done_ns);
            total += u128::from(read.total_ns);
        }
    }
    let title = match takes.as_slice() {
        [] => return None,
        [take] => format!("Analyzing take {}", take.number()),
        many => format!("Analyzing {} takes", format::count(many.len() as u64)),
    };
    let fraction = (total > 0).then(|| (done as f64 / total as f64).clamp(0.0, 1.0));
    let percent = fraction.map(|f| format!("{}%", (f * 100.0).floor() as u32));
    Some(AnalyzingView {
        title,
        fraction,
        percent,
        clips,
    })
}

/// The panel subtitle while previewing: "Previewing take 8 · c3e9",
/// "Previewing 4 clips", "Previewing all 10 clips"; "No clips yet" on an
/// empty timeline.
pub fn subtitle(state: &State) -> String {
    let clips = &state.project.timeline.clips;
    let selected: Vec<_> = state
        .ui
        .selection
        .iter()
        .filter_map(|id| state.project.clip(id))
        .collect();
    match selected.as_slice() {
        [] if clips.is_empty() => format::NO_CLIPS.to_owned(),
        [] => format!(
            "Previewing all {} {}",
            format::count(clips.len() as u64),
            if clips.len() == 1 { "clip" } else { "clips" }
        ),
        [clip] => format!("Previewing take {} \u{b7} {}", clip.take.number(), clip.id),
        many => format!("Previewing {} clips", format::count(many.len() as u64)),
    }
}

/// Whether the page opens now (the `autocut.page` hint, G§4.3): the gates
/// of `Autocut` (Idle). An empty timeline opens it too ([`previews`]).
pub fn page_enabled(state: &State) -> Result<(), String> {
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
                        tracks: Some(vec!["A2".parse().unwrap()]),
                        ..AutocutPatch::default()
                    },
                },
            )
        };
        let mut unnamed = draft.clone();
        unnamed.tracks.clear();
        let a2 = set_a2(&before);
        assert!(dry_run_key(&before, &unnamed).stale_for(&dry_run_key(&a2, &unnamed)));
        let mut named = draft.clone();
        named.tracks = vec!["A1".parse().unwrap()];
        assert_eq!(dry_run_key(&a2, &named), dry_run_key(&before, &named));
    }

    /// The dry run of the design fixture (BRIEF §3.6): `qrec autocut
    /// --clips c3e9 --dry-run` at −35 dB, 400 ms, 120 ms, 200 ms keeps take
    /// 8 but for frames 556–604, 1284–1326, 1990–2029 and 2662–2719.
    fn s12_plan() -> AutocutPlanView {
        AutocutPlanView {
            clips: vec![AutocutClipView {
                clip: id("c3e9"),
                tracks: vec![TrackId::numbered(1)],
                warning: None,
                kept: vec![
                    [0, 556],
                    [604, 1284],
                    [1326, 1990],
                    [2029, 2662],
                    [2719, 3105],
                ],
                removed_frames: 186,
                silences: 4,
                excepted: Vec::new(),
                removed: Vec::new(),
            }],
            skipped: vec![],
            edges_only: false,
        }
    }

    /// Board 01 (issue #15): S12 with silence 2 kept. The summary counts
    /// three cuts and says "· 1 kept"; the kept region stays in the list,
    /// unchecked ("kept"); the timeline shows it as a kept span.
    #[test]
    fn s12_with_a_kept_cut() {
        let state = s12();
        let mut plan = s12_plan();
        let clip = &mut plan.clips[0];
        clip.kept = vec![[0, 556], [604, 1990], [2029, 2662], [2719, 3105]];
        clip.excepted = vec![[1284, 1326]];
        clip.silences = 3;
        clip.removed_frames = 186 - 42;
        let summary = summary(&plan, &state);
        assert_eq!(summary.title, "Removes 3 silences (4.8 s)");
        assert_eq!(summary.kept, " \u{b7} 1 kept");
        assert_eq!(
            summary.rows[0],
            ("Take 8".to_owned(), "00:01:43:15 → 00:01:38:21".to_owned())
        );
        assert_eq!(
            summary.rows[1],
            (
                "Timeline".to_owned(),
                "00:04:06:11 → 00:04:01:17".to_owned()
            )
        );
        let rows: Vec<(usize, &str, bool)> = summary
            .regions
            .iter()
            .map(|r| (r.n, r.size.as_str(), r.kept))
            .collect();
        assert_eq!(
            rows,
            [
                (1, "48 f · 1.6 s", false),
                (2, "kept", true),
                (3, "39 f · 1.3 s", false),
                (4, "57 f · 1.9 s", false),
            ]
        );
        assert_eq!(summary.regions[1].cut, [1284, 1326]);
        assert_eq!(summary.regions[1].clip, id("c3e9"));

        let spans = preview_spans_for(&plan, &state, 0);
        let kept: Vec<_> = spans.iter().filter(|s| s.kind == HatchKind::Kept).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!((kept[0].cut, kept[0].tag.as_str()), ([1284, 1326], "kept"));
        assert_eq!(spans.iter().filter(|s| s.kind == HatchKind::Cut).count(), 3);

        // Every cut kept: nothing to cut, the regions still listed to undo it.
        let mut all = s12_plan();
        all.clips[0].excepted = s12_plan().clips[0]
            .kept
            .windows(2)
            .map(|w| [w[0][1], w[1][0]])
            .collect();
        all.clips[0].kept = vec![[0, 3105]];
        all.clips[0].silences = 0;
        all.clips[0].removed_frames = 0;
        let summary = summary_with(
            &all,
            &state,
            Some("Dry run: nothing to cut: every cut is kept"),
        );
        assert_eq!(summary.title, "Nothing to cut");
        assert_eq!(summary.kept, " \u{b7} 4 kept");
        assert_eq!(summary.regions.len(), 4);
        assert!(summary.regions.iter().all(|r| r.kept));
        assert_eq!(
            summary.rows,
            [(String::new(), "Every cut is kept".to_owned())]
        );
    }

    #[test]
    fn toggling_a_cut_keeps_it_or_cuts_it_again() {
        let c = id("c3e9");
        let keep = toggle_keep(&[], &c, [1284, 1326]);
        assert_eq!(
            keep,
            [AutocutKeep {
                clip: c.clone(),
                in_: 1284,
                out: 1326
            }]
        );
        // A range from an older plan overlapping the cut: the cut is kept,
        // so toggling it cuts it again.
        let stale = vec![AutocutKeep {
            clip: c.clone(),
            in_: 1300,
            out: 1340,
        }];
        assert!(toggle_keep(&stale, &c, [1284, 1326]).is_empty());
        // Another clip's range or another cut stays.
        let other = AutocutKeep {
            clip: id("c2d0"),
            in_: 1284,
            out: 1326,
        };
        assert_eq!(
            toggle_keep(std::slice::from_ref(&other), &c, [1284, 1326]).len(),
            2
        );
        assert_eq!(toggle_keep(&keep, &c, [556, 604]).len(), 2);
        // The draft sends them with the dry run and with Apply's cut, and
        // never saves them.
        let state = s12();
        let draft = AutocutDraft {
            keep: keep.clone(),
            ..AutocutDraft::from(&state.project)
        };
        assert_eq!(overrides(&draft).keep, keep);
        assert_eq!(changed(&state, &draft), AutocutPatch::default());
        let commands = apply(&state, &draft);
        assert!(matches!(
            commands.as_slice(),
            [Command::Autocut { overrides, dry_run: false, .. }] if overrides.keep == keep
        ));
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

    /// Issue #15, board 02: edges only on c3e9 trims 2.2 s before the first
    /// sound and 0.9 s after the last.
    #[test]
    fn edges_only_trims_edges() {
        let state = s12();
        let plan = AutocutPlanView {
            clips: vec![AutocutClipView {
                clip: id("c3e9"),
                tracks: vec![TrackId::numbered(1)],
                warning: None,
                kept: vec![[66, 3078]],
                removed_frames: 93,
                silences: 2,
                excepted: Vec::new(),
                removed: Vec::new(),
            }],
            skipped: vec![],
            edges_only: true,
        };
        let got = summary(&plan, &state);
        assert_eq!(got.title, "Trims 2 edges (3.1 s)");
        assert_eq!(got.rows[0].1, "00:01:43:15 → 00:01:40:12");
        assert_eq!(got.regions.len(), 2);
        assert_eq!(got.regions[1].size, "27 f · 0.9 s");
        let spans = preview_spans(&plan, &state);
        let cuts: Vec<_> = spans
            .iter()
            .filter(|s| s.kind == HatchKind::Cut)
            .map(|s| s.tag.as_str())
            .collect();
        assert_eq!(cuts, ["\u{2212}2.2 s", "\u{2212}0.9 s"]);

        let none = AutocutPlanView {
            clips: vec![AutocutClipView {
                clip: id("c3e9"),
                tracks: vec![TrackId::numbered(1)],
                warning: None,
                kept: vec![[0, 3105]],
                removed_frames: 0,
                silences: 0,
                excepted: Vec::new(),
                removed: Vec::new(),
            }],
            skipped: vec![],
            edges_only: true,
        };
        let got = summary_with(
            &none,
            &state,
            Some(
                "Dry run: nothing to trim: no silence long enough at the clips' edges at \u{2212}35 dB",
            ),
        );
        assert_eq!(got.title, "Nothing to trim");
        assert_eq!(
            got.rows[0].1,
            "No silence long enough at the clips' edges at −35 dB"
        );
        assert_eq!(summary_with(&none, &state, None), got);
    }

    #[test]
    fn nothing_to_cut_reads_the_cores_message() {
        let state = s12();
        let plan = AutocutPlanView {
            clips: vec![AutocutClipView {
                clip: id("c3e9"),
                tracks: vec![TrackId::numbered(1)],
                warning: None,
                kept: vec![[0, 3105]],
                removed_frames: 0,
                silences: 0,
                excepted: Vec::new(),
                removed: Vec::new(),
            }],
            skipped: vec![],
            edges_only: false,
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
            edges_only: false,
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
                tracks: vec![TrackId::numbered(1)],
                warning: None,
                kept: vec![[12, 120], [158, 288], [303, 1104]],
                removed_frames: 53,
                silences: 2,
                excepted: Vec::new(),
                removed: Vec::new(),
            },
        );
        plan.clips.push(AutocutClipView {
            clip: id("c7f3"),
            tracks: vec![TrackId::numbered(1)],
            warning: None,
            kept: vec![[0, 410]],
            removed_frames: 0,
            silences: 0,
            excepted: Vec::new(),
            removed: Vec::new(),
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
        // The lanes are the plan's tracks (the dry run's).
        for clip in &mut plan.clips {
            clip.tracks = vec![TrackId::numbered(2)];
        }
        let spans = preview_spans_for(&plan, &state, 0);
        assert_eq!(spans.len(), 6);
        assert!(spans.iter().all(|s| s.track == TrackId::numbered(2)));
        // c2d0 starts at frame 2084 (00:01:09:14): its first span is
        // timeline 2084 + 108 … 2084 + 146.
        assert_eq!((spans[0].from, spans[0].to), (2192, 2230));
        assert_eq!(spans[0].tag, "\u{2212}1.3 s");
    }

    /// An empty timeline opens the page (the owner asked, 2026-11-14): no
    /// dry run to preview, "Nothing to cut yet", and Save keeps the
    /// settings without a cut. A recording still closes it.
    #[test]
    fn an_empty_timeline_opens_the_page_to_save_settings() {
        let mut state = s12();
        state.project.timeline.clips.clear();
        state.ui.selection.clear();
        assert_eq!(page_enabled(&state), Ok(()));
        assert!(!previews(&state));
        assert_eq!(subtitle(&state), "No clips yet");
        assert_eq!(apply_label(&state), SAVE);
        assert_eq!(apply_label(&s12()), APPLY);
        assert_eq!(empty_summary().title, NOTHING_YET);
        assert_eq!(
            empty_summary().rows,
            [(String::new(), RECORD_FIRST.to_owned())]
        );
        let draft = AutocutDraft::from(&state.project);
        assert!(apply(&state, &draft).is_empty(), "nothing changed: nothing");
        let tuned = Knob::MinKeep.set(&draft, 80.0);
        assert_eq!(
            apply(&state, &tuned),
            [Command::AutocutSettings {
                patch: AutocutPatch {
                    min_keep_ms: Some(80),
                    ..AutocutPatch::default()
                },
            }],
            "the settings only: no cut"
        );
        state.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        assert_eq!(page_enabled(&state), Err("Not while recording".to_owned()));
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
            tracks: vec![TrackId::numbered(1), TrackId::numbered(2)],
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
            tracks: vec![TrackId::numbered(1)],
            ..back
        };
        assert_eq!(changed(&state, &back), AutocutPatch::default());
        // "Trim edges only" is part of the draft: Apply saves it.
        let edges = AutocutDraft {
            edges_only: true,
            ..draft.clone()
        };
        assert_eq!(
            changed(&state, &edges),
            AutocutPatch {
                edges_only: Some(true),
                ..AutocutPatch::default()
            }
        );
        assert_eq!(overrides(&edges).edges_only, Some(true));
        // A draft that names no track clears the setting's.
        let mut cleared = state.clone();
        cleared.project.autocut.tracks.clear();
        let unset = AutocutDraft::from(&cleared.project);
        assert_eq!(changed(&cleared, &unset), AutocutPatch::default());
        assert_eq!(
            changed(
                &state,
                &AutocutDraft {
                    tracks: Vec::new(),
                    ..draft.clone()
                }
            )
            .tracks,
            Some(Vec::new())
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
                    "help": k.help(),
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
        assert_eq!(shown_tracks(&state, &draft), [TrackId::numbered(1)]);
        let none = AutocutDraft {
            tracks: Vec::new(),
            ..draft
        };
        assert_eq!(
            shown_tracks(&state, &none),
            [TrackId::numbered(1)],
            "the setting's"
        );
    }

    /// The dropdown's check boxes (issue #15): checking adds in the
    /// dropdown's order, the last checked one stays, and a list equal to
    /// the default goes back to naming none.
    #[test]
    fn checking_tracks_in_the_dropdown() {
        let mut state = s12();
        state.project.autocut.tracks.clear();
        let (a1, a2) = (TrackId::numbered(1), TrackId::numbered(2));
        let draft = AutocutDraft::from(&state.project);
        assert_eq!(
            shown_tracks(&state, &draft),
            std::slice::from_ref(&a1),
            "the first mic"
        );
        assert_eq!(shown_label(&state, &draft), "A1 \u{b7} Yeti Nano (mic)");
        let both = toggled_tracks(&state, &draft, &a2).unwrap();
        assert_eq!(both, [a1.clone(), a2.clone()]);
        let draft = AutocutDraft {
            tracks: both,
            ..draft
        };
        assert_eq!(
            shown_label(&state, &draft),
            "A1 \u{b7} Yeti Nano + A2 \u{b7} System audio"
        );
        assert_eq!(overrides(&draft).tracks, [a1.clone(), a2.clone()]);
        assert_eq!(
            changed(&state, &draft).tracks,
            Some(vec![a1.clone(), a2.clone()])
        );
        // Unchecking A2: back to the first mic, named by none.
        assert_eq!(toggled_tracks(&state, &draft, &a2), Some(vec![]));
        // A2 alone, then A1 added back: the dropdown's order.
        let a2_only = AutocutDraft {
            tracks: vec![a2.clone()],
            ..draft.clone()
        };
        assert_eq!(
            toggled_tracks(&state, &a2_only, &a2),
            None,
            "the last one stays"
        );
        assert_eq!(
            toggled_tracks(&state, &a2_only, &a1),
            Some(vec![a1.clone(), a2.clone()])
        );
        // With a setting, the list is kept as it is.
        state.project.autocut.tracks = vec![a2.clone()];
        let draft = AutocutDraft::from(&state.project);
        assert_eq!(
            toggled_tracks(&state, &draft, &a1),
            Some(vec![a1.clone(), a2.clone()])
        );
        let both = AutocutDraft {
            tracks: vec![a1.clone(), a2],
            ..draft
        };
        assert_eq!(
            toggled_tracks(&state, &both, &TrackId::numbered(2)),
            Some(vec![a1])
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
                tracks: vec![TrackId::numbered(1)],
                warning: None,
                kept: vec![[0, 40], [60, 100]],
                removed_frames: 20,
                silences: 1,
                excepted: Vec::new(),
                removed: Vec::new(),
            }],
            skipped: vec![],
            edges_only: false,
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
                // The setting unset, on each track, or on all of them.
                let settings = std::iter::once(Vec::new())
                    .chain((1..=n).map(|i| vec![TrackId::numbered(i)]))
                    .chain((n > 1).then(|| (1..=n).map(TrackId::numbered).collect()));
                for setting in settings {
                    let mut project = base.clone();
                    project.autocut.tracks = setting.clone();
                    let state = testkit::state(project);
                    let take = state.project.take(&TakeId::numbered(1)).unwrap();
                    let analyzed = crate::autocut::choose_tracks(&state.project, take, &[]);
                    assert!(!analyzed.is_empty(), "{n} tracks, mics {mics:b}");
                    let case = format!("{n} tracks, mics {mics:b}, setting {setting:?}");
                    let draft = AutocutDraft::from(&state.project);
                    assert_eq!(
                        shown_tracks(&state, &draft),
                        analyzed,
                        "the dropdown: {case}"
                    );
                    // A draft that names no track: the dry run's override
                    // is unset, so it analyzes what the setting says.
                    let unset = AutocutDraft {
                        tracks: Vec::new(),
                        ..draft.clone()
                    };
                    assert_eq!(shown_tracks(&state, &unset), analyzed, "unset: {case}");
                    let tagged: Vec<TrackId> =
                        crate::view::panel::audio_rows(&state, &Default::default())
                            .into_iter()
                            .filter(|r| r.autocut)
                            .map(|r| r.track)
                            .collect();
                    let mut sorted = analyzed.clone();
                    sorted.sort();
                    assert_eq!(tagged, sorted, "{case}");
                    let scissors: Vec<TrackId> = crate::view::timeline::lanes(&state)
                        .into_iter()
                        .filter(|l| l.autocut)
                        .map(|l| l.track)
                        .collect();
                    assert_eq!(scissors, sorted, "{case}");
                    // The dry run's plan names the tracks it analyzed.
                    let mut plan = plan.clone();
                    plan.clips[0].tracks = analyzed.clone();
                    let spans = preview_spans_for(&plan, &state, 0);
                    let lanes: Vec<TrackId> = spans.iter().map(|s| s.track.clone()).collect();
                    assert_eq!(lanes, analyzed, "the hatches: {case}");
                    let tags = spans.iter().filter(|s| !s.tag.is_empty()).count();
                    assert_eq!(tags, 1, "one tag: {case}");
                }
            }
        }
    }

    /// R77's audit: a take recorded with a track the project scene no
    /// longer has (system A1 in the project, system A1 and mic A2 in take
    /// 1), the setting unset: the dry run analyzes A2, so the dropdown
    /// shows A2 (listed after the project scene's tracks), the scissors and
    /// the hatch sit on A2, and the "Autocut" tag on take 1's A2 in Program;
    /// in Live the tag is the project scene's (A1: what a take recorded
    /// from it is analyzed on). The dropdown and the Live tag read only the
    /// project scene and named A1 while the cut listened to A2.
    #[test]
    fn a_take_of_another_scene_shows_the_track_it_is_analyzed_on() {
        let a1 = TrackId::numbered(1);
        let a2 = TrackId::numbered(2);
        let mut project = testkit::ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .system("A1", "a.monitor")
            .mic("A2", "a.mic")
            .take("take-0001", 100)
            .clip("c0a1", "take-0001", 0, 100)
            .build();
        project.scene.audio.retain(|t| t.id == a1);
        let plan = AutocutPlanView {
            clips: vec![AutocutClipView {
                clip: id("c0a1"),
                tracks: vec![a2.clone()],
                warning: None,
                kept: vec![[0, 40], [60, 100]],
                removed_frames: 20,
                silences: 1,
                excepted: Vec::new(),
                removed: Vec::new(),
            }],
            skipped: vec![],
            edges_only: false,
        };
        let mut state = testkit::state(project);
        let take = state.project.take(&TakeId::numbered(1)).unwrap();
        let analyzed = crate::autocut::choose_tracks(&state.project, take, &[]);
        assert_eq!(analyzed, std::slice::from_ref(&a2), "the dry run's track");
        for mode in [CanvasMode::Live, CanvasMode::Program] {
            state.ui.canvas_mode = mode;
            state.ui.selection = match mode {
                CanvasMode::Live => vec![],
                CanvasMode::Program => vec![id("c0a1")],
            };
            let draft = AutocutDraft::from(&state.project);
            assert_eq!(
                shown_tracks(&state, &draft),
                analyzed,
                "the dropdown in {mode:?}"
            );
            let items: Vec<TrackId> = track_items(&state).into_iter().map(|i| i.track).collect();
            assert_eq!(items, [a1.clone(), a2.clone()], "{mode:?}");
            let scissors: Vec<TrackId> = crate::view::timeline::lanes(&state)
                .into_iter()
                .filter(|l| l.autocut)
                .map(|l| l.track)
                .collect();
            assert_eq!(scissors, vec![a2.clone()], "the scissors in {mode:?}");
            let spans = preview_spans_for(&plan, &state, 0);
            assert_eq!(
                spans.iter().map(|s| &s.track).collect::<Vec<_>>(),
                [&a2],
                "the hatch in {mode:?}"
            );
            let tagged: Vec<TrackId> = crate::view::panel::audio_rows(&state, &Default::default())
                .into_iter()
                .filter(|r| r.autocut)
                .map(|r| r.track)
                .collect();
            let tag = match mode {
                CanvasMode::Live => &a1,
                CanvasMode::Program => &a2,
            };
            assert_eq!(tagged, vec![tag.clone()], "the tag in {mode:?}");
        }
        // A second take analyzed on A1 (its scene has no mic): the scissors
        // on both lanes, each clip's hatch on its own track; the dropdown
        // checks its scope's.
        let mut project = state.project.clone();
        let mut second = project.takes[&TakeId::numbered(1)].clone();
        second.scene.audio.retain(|t| t.id == a1);
        project.takes.insert(TakeId::numbered(2), second);
        project.timeline.clips.push(Clip {
            id: id("c0b1"),
            take: TakeId::numbered(2),
            in_: 0,
            out: 100,
        });
        let mut state = testkit::state(project);
        state.ui.canvas_mode = CanvasMode::Program;
        let scissors: Vec<TrackId> = crate::view::timeline::lanes(&state)
            .into_iter()
            .filter(|l| l.autocut)
            .map(|l| l.track)
            .collect();
        assert_eq!(scissors, [a1.clone(), a2.clone()], "both lanes");
        let draft = AutocutDraft::from(&state.project);
        assert_eq!(shown_tracks(&state, &draft), [a2.clone(), a1.clone()]);
        state.ui.selection = vec![id("c0b1")];
        assert_eq!(shown_tracks(&state, &draft), std::slice::from_ref(&a1));
        let both = AutocutPlanView {
            clips: vec![
                plan.clips[0].clone(),
                AutocutClipView {
                    clip: id("c0b1"),
                    tracks: vec![TrackId::numbered(1)],
                    warning: None,
                    ..plan.clips[0].clone()
                },
            ],
            skipped: vec![],
            edges_only: false,
        };
        let spans = preview_spans_for(&both, &state, 0);
        assert_eq!(
            spans.iter().map(|s| &s.track).collect::<Vec<_>>(),
            [&a2, &a1]
        );
        // The setting names the track: every view shows it.
        state.project.autocut.tracks = vec![a1.clone()];
        let lanes: Vec<TrackId> = crate::view::timeline::lanes(&state)
            .into_iter()
            .filter(|l| l.autocut)
            .map(|l| l.track)
            .collect();
        assert_eq!(lanes, vec![a1.clone()]);
        let draft = AutocutDraft::from(&state.project);
        assert_eq!(shown_tracks(&state, &draft), [a1]);
    }

    #[test]
    fn the_page_needs_idle() {
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
        assert_eq!(page_enabled(&empty), Ok(()), "an empty timeline opens it");
    }

    /// Issue #15, board 07: what the summary says while a dry run analyzes
    /// (the page shows it once the dry run took over 300 ms).
    mod analysis_cue {
        use super::*;
        use crate::apply::autocut::fixtures::{self, MIC1, MIC2};
        use crate::ctx::Ctx;
        use crate::ids::JobId;
        use crate::on_event::autocut::{silence_detected, silence_progress};
        use crate::state::SilenceKey;

        fn setup() -> (Ctx, SilenceKey, SilenceKey) {
            let ctx = fixtures::ctx(&fixtures::project());
            let p = fixtures::params();
            let (k1, k2) = (fixtures::key(&ctx, MIC1, &p), fixtures::key(&ctx, MIC2, &p));
            (ctx, k1, k2)
        }

        fn autocut(state: &State, ctx: &Ctx, dry_run: bool) -> State {
            let cmd = Command::Autocut {
                scope: ClipScope::All,
                overrides: AutocutOverrides::default(),
                dry_run,
            };
            crate::apply::apply(state, ctx, cmd).unwrap().state
        }

        fn clips(ids: &[&str]) -> Vec<ClipId> {
            ids.iter().map(|id| id.parse().unwrap()).collect()
        }

        #[test]
        fn it_names_the_takes_and_how_far_ffmpeg_read() {
            let (ctx, k1, k2) = setup();
            let state = testkit::state(fixtures::project());
            assert_eq!(analyzing(&state), None, "no dry run waits");
            // Both takes (10 s and 5 s), nothing read yet.
            let state = autocut(&state, &ctx, true);
            let cue = analyzing(&state).unwrap();
            assert_eq!(cue.title, "Analyzing 2 takes");
            assert_eq!(
                (cue.fraction, cue.percent.as_deref()),
                (Some(0.0), Some("0%"))
            );
            assert_eq!(cue.clips, clips(&["c001", "c002"]));
            // Half of take 1: 5 of 15 s, each file weighing its length.
            let state = silence_progress(&state, JobId(1), k1.clone(), 5_000_000_000).state;
            assert_eq!(analyzing(&state).unwrap().percent.as_deref(), Some("33%"));
            // Take 1's silences came: take 2 is left, and take 1 counts read.
            let found = fixtures::silences(MIC1);
            let state = silence_detected(&state, &ctx, JobId(1), k1, found).state;
            let cue = analyzing(&state).unwrap();
            assert_eq!(cue.title, "Analyzing take 2");
            assert_eq!(cue.percent.as_deref(), Some("66%"));
            assert_eq!(cue.clips, clips(&["c002"]));
            let state = silence_progress(&state, JobId(1), k2.clone(), 4_999_000_000).state;
            let cue = analyzing(&state).unwrap();
            assert_eq!(
                cue.percent.as_deref(),
                Some("99%"),
                "never 100% while it runs"
            );
            // Take 2's came: the plan, and nothing to say.
            let found = fixtures::silences(MIC2);
            let state = silence_detected(&state, &ctx, JobId(1), k2, found).state;
            assert_eq!(analyzing(&state), None);
        }

        #[test]
        fn known_analyses_and_cuts_stay_out_of_it() {
            let (ctx, k1, k2) = setup();
            // Take 1 analyzed before: the dry run reads take 2 only.
            let mut state = testkit::state(fixtures::project());
            state.runtime.silence.insert(k1, fixtures::silences(MIC1));
            let state = autocut(&state, &ctx, true);
            let cue = analyzing(&state).unwrap();
            assert_eq!(
                (cue.title.as_str(), cue.clips),
                ("Analyzing take 2", clips(&["c002"]))
            );
            let state = silence_progress(&state, JobId(1), k2, 2_500_000_000).state;
            assert_eq!(analyzing(&state).unwrap().percent.as_deref(), Some("50%"));
            // Files of unknown length: the takes, no percent (no bar).
            let mut project = fixtures::project();
            for take in project.takes.values_mut() {
                for stream in &mut take.streams {
                    stream.duration_ns = None;
                }
            }
            let state = autocut(&testkit::state(project), &ctx, true);
            let cue = analyzing(&state).unwrap();
            assert_eq!(
                (cue.title.as_str(), cue.fraction, cue.percent),
                ("Analyzing 2 takes", None, None)
            );
            // A cut analyzing isn't the page's preview.
            let state = autocut(&testkit::state(fixtures::project()), &ctx, false);
            assert!(!state.runtime.jobs.is_empty());
            assert_eq!(analyzing(&state), None);
        }
    }
}
