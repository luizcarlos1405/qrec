//! The autocut planner (A§5.7, SPEC §9). Owner: autocut.
//!
//! Per clip, over the tracks it listens to (issue #15: several):
//! 1. silences arrive in **file time**, one analysis per track; shift each
//!    track's to take time `t = x + startOffsetNs + syncOffsetMs·10⁶`; with
//!    several tracks keep only what is silent on all of them, without the
//!    parts shorter than `minSilenceMs` ([`combined_silences`]);
//! 2. shrink each by `paddingMs` on both sides and drop the empty ones;
//! 3. kept = the clip range minus the shrunk silences;
//! 4. drop kept fragments with less than `minKeepMs` of sound: what isn't
//!    silence, so their padding doesn't count;
//! 5. snap outward to frames (`in = ns_to_frame_floor`, `out =
//!    ns_to_frame_ceil`), clamp to the clip, merge touching fragments;
//! 6. nothing kept ⇒ the clip is left unchanged and reported "entirely
//!    silent at −35 dB": autocut never deletes a whole clip;
//! 7. `edgesOnly` ⇒ the kept fragments become their hull, from where the
//!    first starts to where the last ends: only the silence before the
//!    first sound and after the last goes, and the clip stays one clip.
//!
//! Two precisions over A§5.7 (both pinned by `tests/props_autocut.rs`):
//! - Overlapping or touching silences are merged before step 2. ffmpeg never
//!   reports such pairs, but shrinking them one by one would leave a hole of
//!   fake "speech" where they meet.
//! - `minKeepMs` drops *leftover* fragments (SPEC §6.3): a clip no shrunk
//!   silence reaches is kept whole even when it is shorter than that. When
//!   only sounds under `minKeepMs` are left of a clip that does have sound,
//!   step 6 reports "no sound of 50 ms or more to keep" rather than
//!   "entirely silent".
//!
//! Step 4 counts sound, not length: a fragment between two silences is at
//! least twice the padding long however short its sound, so a click that
//! splits a silence in two (ffmpeg ends a silence at one loud sample) would
//! otherwise survive as a clip of padding around it.
//!
//! Everything here is pure planning; the command, the job and the history
//! entry live in `apply::autocut` and `on_event::autocut`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::{Frame, TakeId, TrackId};
use crate::model::AutocutSettings;
use crate::model::{AudioRole, AudioTrack, Clip, Fps, Project, Scene, Stream, StreamKind, Take};
use crate::protocol::event::Interval;
use crate::state::AutocutParams;
use crate::time::{
    NS_PER_MS, duration_label, frame_ns, ns_to_frame_ceil, ns_to_frame_floor, ns_to_frame_round,
};

/// The entry points other owners call: autocut after a take (`autoRunAfterTake`)
/// and jobs that finished analyzing while a recording ran.
pub use crate::apply::autocut::start_auto_run;
pub use crate::on_event::autocut::apply_ready_autocuts;

/// The planned cut of one clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipCut {
    /// Kept fragments as `[in, out)` take frames, sorted and separated by
    /// at least one removed frame; empty = nothing worth keeping (the clip
    /// stays as it is, see [`nothing_kept_reason`]).
    pub kept: Vec<[Frame; 2]>,
    /// Silences removed: the runs of frames the cut takes out of the clip.
    pub silences: usize,
    pub removed_frames: Frame,
}

impl ClipCut {
    /// Nothing would be kept: the clip is left alone (A§5.7 step 6).
    pub fn keeps_nothing(&self) -> bool {
        self.kept.is_empty()
    }

    /// Applying the cut changes `clip` (something is kept and something removed).
    pub fn changes(&self, clip: &Clip) -> bool {
        !self.kept.is_empty() && self.kept != [[clip.in_, clip.out]]
    }

    /// The removed runs `[in, out)` of take frames; none when the cut keeps
    /// nothing (the clip isn't touched).
    pub fn removed(&self, clip: &Clip) -> Vec<[Frame; 2]> {
        if self.kept.is_empty() {
            return Vec::new();
        }
        let mut gaps = Vec::new();
        let mut cursor = clip.in_;
        for &[in_, out] in &self.kept {
            if in_ > cursor {
                gaps.push([cursor, in_]);
            }
            cursor = cursor.max(out);
        }
        if cursor < clip.out {
            gaps.push([cursor, clip.out]);
        }
        gaps
    }

    /// The padding the cut keeps beside each removed run: `padding_ms`
    /// before its start and after its end (rounded to frames), as `[in,
    /// out)` take frames inside the clip, sorted; none on a side where the
    /// run reaches the clip's edge. The silence there stays: what the
    /// timeline shows in `padding-fill` next to the hatch (S08, S12).
    pub fn padding(&self, clip: &Clip, padding_ms: u32, fps: Fps) -> Vec<[Frame; 2]> {
        let pad = i64::from(padding_ms) * NS_PER_MS;
        let mut runs = Vec::new();
        if pad == 0 {
            return runs;
        }
        for [a, b] in self.removed(clip) {
            if a > clip.in_ {
                let from = ns_to_frame_round(frame_ns(as_i64(a), fps).saturating_sub(pad), fps);
                let from = as_frame(from).max(clip.in_);
                if from < a {
                    runs.push([from, a]);
                }
            }
            if b < clip.out {
                let to = ns_to_frame_round(frame_ns(as_i64(b), fps).saturating_add(pad), fps);
                let to = as_frame(to).min(clip.out);
                if b < to {
                    runs.push([b, to]);
                }
            }
        }
        runs
    }

    /// The removed media time (each run as `frame_ns(out) − frame_ns(in)`).
    pub fn removed_ns(&self, clip: &Clip, fps: Fps) -> i64 {
        self.removed(clip)
            .iter()
            .map(|&[a, b]| frame_ns(as_i64(b), fps) - frame_ns(as_i64(a), fps))
            .sum()
    }
}

/// The parameters a project's autocut settings plan with.
pub fn params_of(settings: &AutocutSettings) -> AutocutParams {
    AutocutParams {
        threshold_db: settings.threshold_db,
        min_silence_ms: settings.min_silence_ms,
        padding_ms: settings.padding_ms,
        min_keep_ms: settings.min_keep_ms,
        edges_only: settings.edges_only,
    }
}

/// One analyzed track of a take: the track (take scene), its stream, and
/// its silences in file time, as ffmpeg reports them.
#[derive(Debug, Clone, Copy)]
pub struct TrackSilences<'a> {
    pub track: &'a AudioTrack,
    pub stream: &'a Stream,
    pub silences: &'a [Interval],
}

/// Plans the cut of `clip` from the silences of `stream` (its `track` in
/// the take scene). `silences` are in file time, as ffmpeg reports them.
pub fn plan_clip(
    clip: &Clip,
    stream: &Stream,
    track: &AudioTrack,
    silences: &[Interval],
    params: &AutocutParams,
    fps: Fps,
) -> ClipCut {
    let one = [TrackSilences {
        track,
        stream,
        silences,
    }];
    plan_clip_on(clip, &one, params, fps)
}

/// Plans the cut of `clip` from several tracks: a region is cut only when
/// it is silent on every one of them ([`combined_silences`]).
pub fn plan_clip_on(
    clip: &Clip,
    tracks: &[TrackSilences<'_>],
    params: &AutocutParams,
    fps: Fps,
) -> ClipCut {
    cut_clip(
        clip,
        &combined_silences(tracks, params.min_silence_ms),
        params,
        fps,
    )
}

/// Step 1 for every analyzed track: each one's silences in take time (its
/// own offsets), intersected, since sound on any track keeps a region.
/// With several tracks, intersections shorter than `min_silence_ms` are
/// dropped: each track's silences are that long, their overlap may not be.
/// One track's silences come back as they are.
pub fn combined_silences(tracks: &[TrackSilences<'_>], min_silence_ms: u32) -> Vec<Interval> {
    let per_track: Vec<Vec<Interval>> = tracks
        .iter()
        .map(|t| to_take_time(t.silences, t.stream, t.track))
        .collect();
    intersect_silences(&per_track, min_silence_ms)
}

/// [`combined_silences`] for silences already in take time: one list as
/// it is; several intersected, without the parts shorter than
/// `min_silence_ms`.
pub fn intersect_silences(per_track: &[Vec<Interval>], min_silence_ms: u32) -> Vec<Interval> {
    match per_track {
        [] => Vec::new(),
        [one] => one.clone(),
        [first, rest @ ..] => {
            let min = i64::from(min_silence_ms) * NS_PER_MS;
            rest.iter()
                .fold(union(first), |acc, next| intersect(&acc, &union(next)))
                .into_iter()
                .filter(|s| s.end_ns - s.start_ns >= min)
                .collect()
        }
    }
}

/// The intersection of two sorted, disjoint interval lists.
fn intersect(a: &[Interval], b: &[Interval]) -> Vec<Interval> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        let start_ns = a[i].start_ns.max(b[j].start_ns);
        let end_ns = a[i].end_ns.min(b[j].end_ns);
        if start_ns < end_ns {
            out.push(Interval { start_ns, end_ns });
        }
        if a[i].end_ns < b[j].end_ns {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Step 1: file time → take time, `t = x + startOffsetNs + syncOffsetMs·10⁶`.
pub fn to_take_time(silences: &[Interval], stream: &Stream, track: &AudioTrack) -> Vec<Interval> {
    let shift = stream
        .start_offset_ns
        .saturating_add(i64::from(track.sync_offset_ms).saturating_mul(NS_PER_MS));
    silences
        .iter()
        .map(|s| Interval {
            start_ns: s.start_ns.saturating_add(shift),
            end_ns: s.end_ns.saturating_add(shift),
        })
        .collect()
}

/// Steps 2–7 for silences already in take time.
pub fn cut_clip(clip: &Clip, silences: &[Interval], params: &AutocutParams, fps: Fps) -> ClipCut {
    let range = Interval {
        start_ns: frame_ns(as_i64(clip.in_), fps),
        end_ns: frame_ns(as_i64(clip.out), fps),
    };
    let spans = kept_spans(range, silences, params.padding_ms, params.min_keep_ms);
    let mut kept = snap_to_frames(&spans, clip, fps);
    if params.edges_only {
        kept = hull(&kept);
    }
    if kept.is_empty() {
        return ClipCut {
            kept,
            silences: 0,
            removed_frames: 0,
        };
    }
    let mut cut = ClipCut {
        kept,
        silences: 0,
        removed_frames: 0,
    };
    let removed = cut.removed(clip);
    cut.silences = removed.len();
    cut.removed_frames = removed.iter().map(|&[a, b]| b - a).sum();
    cut
}

/// Steps 2–4 in take-time nanoseconds: the parts of `range` outside every
/// silence shrunk by `padding_ms`, without those holding less than
/// `min_keep_ms` of sound (their part outside every silence: padding
/// doesn't count). Sorted and disjoint. These are the padded speech regions
/// the cut must keep.
///
/// A range no shrunk silence reaches comes back whole whatever its length:
/// `minKeepMs` drops *leftover* fragments (SPEC §6.3), and nothing was cut.
pub fn kept_spans(
    range: Interval,
    silences: &[Interval],
    padding_ms: u32,
    min_keep_ms: u32,
) -> Vec<Interval> {
    let pad = i64::from(padding_ms) * NS_PER_MS;
    let min_keep = i64::from(min_keep_ms) * NS_PER_MS;
    let silences = union(silences);
    let mut spans = Vec::new();
    let mut cursor = range.start_ns;
    for &silence in &silences {
        let start = silence.start_ns.saturating_add(pad);
        let end = silence.end_ns.saturating_sub(pad);
        if start >= end || end <= cursor {
            continue;
        }
        if start >= range.end_ns {
            break;
        }
        if start > cursor {
            spans.push(Interval {
                start_ns: cursor,
                end_ns: start,
            });
        }
        cursor = end;
        if cursor >= range.end_ns {
            break;
        }
    }
    if cursor < range.end_ns {
        spans.push(Interval {
            start_ns: cursor,
            end_ns: range.end_ns,
        });
    }
    if spans != [range] {
        spans.retain(|&s| sound_ns(s, &silences) >= min_keep);
    }
    spans
}

/// How much of `span` lies outside every silence of `silences` (sorted and
/// disjoint, as [`union`] makes them).
fn sound_ns(span: Interval, silences: &[Interval]) -> i64 {
    let first = silences.partition_point(|s| s.end_ns <= span.start_ns);
    let quiet: i64 = silences[first..]
        .iter()
        .take_while(|s| s.start_ns < span.end_ns)
        .map(|s| {
            s.end_ns
                .min(span.end_ns)
                .saturating_sub(s.start_ns.max(span.start_ns))
        })
        .sum();
    span.end_ns
        .saturating_sub(span.start_ns)
        .saturating_sub(quiet)
}

/// Why a clip whose cut keeps nothing is left alone: "entirely silent at
/// −35 dB" when the (combined) silences cover all of it, else "no sound of
/// 50 ms or more to keep" (its sounds are all shorter than `minKeepMs`).
pub fn nothing_kept_reason(
    clip: &Clip,
    tracks: &[TrackSilences<'_>],
    params: &AutocutParams,
    fps: Fps,
) -> String {
    let range = Interval {
        start_ns: frame_ns(as_i64(clip.in_), fps),
        end_ns: frame_ns(as_i64(clip.out), fps),
    };
    let covered = union(&combined_silences(tracks, params.min_silence_ms))
        .iter()
        .any(|s| s.start_ns <= range.start_ns && range.end_ns <= s.end_ns);
    if covered {
        entirely_silent(params)
    } else {
        format!("no sound of {} ms or more to keep", params.min_keep_ms)
    }
}

/// Step 5: each span snapped outward to whole frames (`in` = the frame
/// containing its start, `out` = the first frame starting at or after its
/// end), clamped to the clip; touching or overlapping results are merged.
pub fn snap_to_frames(spans: &[Interval], clip: &Clip, fps: Fps) -> Vec<[Frame; 2]> {
    let mut sorted = spans.to_vec();
    sorted.sort();
    let mut kept: Vec<[Frame; 2]> = Vec::new();
    for span in sorted {
        let in_ = as_frame(ns_to_frame_floor(span.start_ns, fps))
            .max(clip.in_)
            .min(clip.out);
        let out = as_frame(ns_to_frame_ceil(span.end_ns, fps))
            .max(clip.in_)
            .min(clip.out);
        if in_ >= out {
            continue;
        }
        match kept.last_mut() {
            Some(last) if last[1] >= in_ => last[1] = last[1].max(out),
            _ => kept.push([in_, out]),
        }
    }
    kept
}

/// Step 7: the kept fragments as one, from the first's start to the last's
/// end (none stays none).
pub fn hull(kept: &[[Frame; 2]]) -> Vec<[Frame; 2]> {
    match (kept.first(), kept.last()) {
        (Some(first), Some(last)) => vec![[first[0], last[1]]],
        _ => Vec::new(),
    }
}

/// The union of `silences`: sorted, empty ones dropped, overlapping and
/// touching ones merged.
fn union(silences: &[Interval]) -> Vec<Interval> {
    let mut sorted: Vec<Interval> = silences
        .iter()
        .copied()
        .filter(|s| s.start_ns < s.end_ns)
        .collect();
    sorted.sort();
    let mut merged: Vec<Interval> = Vec::with_capacity(sorted.len());
    for s in sorted {
        match merged.last_mut() {
            Some(last) if s.start_ns <= last.end_ns => last.end_ns = last.end_ns.max(s.end_ns),
            _ => merged.push(s),
        }
    }
    merged
}

/// The tracks autocut is asked to listen to: the override when it names
/// any, else `autocut.tracks`. Empty = each take's [`default_track`].
pub fn wanted_tracks<'a>(project: &'a Project, override_tracks: &'a [TrackId]) -> &'a [TrackId] {
    if override_tracks.is_empty() {
        &project.autocut.tracks
    } else {
        override_tracks
    }
}

/// The tracks that drive detection for `take`, as found in its scene:
/// [`wanted_tracks`] (in that order) or, when none is named, the take
/// scene's [`default_track`]. Named tracks the take lacks are left out
/// (`analysis_sources` says so).
pub fn choose_tracks(project: &Project, take: &Take, override_tracks: &[TrackId]) -> Vec<TrackId> {
    let wanted = wanted_tracks(project, override_tracks);
    if wanted.is_empty() {
        return default_track(&take.scene)
            .map(|t| vec![t.id.clone()])
            .unwrap_or_default();
    }
    wanted
        .iter()
        .filter(|id| take.scene.track(id).is_some())
        .cloned()
        .collect()
}

/// The tracks autocut analyzes for `clips` ([`choose_tracks`] of each
/// one's take), in clip order, each once. A take recorded with a track the
/// project scene no longer has may be analyzed on it, and takes of
/// different scenes on different tracks: the page's "Detect silence on"
/// shows its scope's, the lanes' scissors sit on each (R77's audit).
pub fn analyzed_tracks<'a>(
    project: &Project,
    clips: impl IntoIterator<Item = &'a Clip>,
    override_tracks: &[TrackId],
) -> Vec<TrackId> {
    let mut tracks: Vec<TrackId> = Vec::new();
    for clip in clips {
        let Some(take) = project.take(&clip.take) else {
            continue;
        };
        for track in choose_tracks(project, take, override_tracks) {
            if !tracks.contains(&track) {
                tracks.push(track);
            }
        }
    }
    tracks
}

/// The track autocut listens to in `scene` when no track is named: its
/// first mic, else its first track. The one rule every view of it follows
/// (the panel's "Autocut" tag on the scene it shows, and through
/// [`analyzed_tracks`] the page's "Detect silence on" and the lanes'
/// scissors).
pub fn default_track(scene: &Scene) -> Option<&AudioTrack> {
    scene
        .audio
        .iter()
        .find(|t| t.role == AudioRole::Mic)
        .or_else(|| scene.audio.first())
}

/// What autocut analyzes for clips of a take: each usable track (take
/// scene) with its stream, and a warning when some named track can't be
/// used ("take 3 has no track A2: silence detected on A1 only").
#[derive(Debug, Clone)]
pub struct AnalysisSources<'t> {
    pub tracks: Vec<(&'t AudioTrack, &'t Stream)>,
    pub warning: Option<String>,
}

/// The tracks and streams autocut analyzes for clips of `take`, or why it
/// can't: "take 3 has no track A2", "take 3 has none of the tracks A2,
/// A3", "take 3 has no audio track", "track A1 of take 3 is missing
/// media". A take that has only some of the named tracks uses those, with
/// a warning; it's skipped only when it has none of them.
pub fn analysis_sources<'t>(
    project: &Project,
    take_id: &TakeId,
    take: &'t Take,
    override_tracks: &[TrackId],
) -> Result<AnalysisSources<'t>, String> {
    let n = take_id.number();
    let wanted = wanted_tracks(project, override_tracks);
    let chosen = choose_tracks(project, take, override_tracks);
    let mut usable = Vec::new();
    let mut problems = Vec::new();
    for id in wanted.iter().filter(|id| !chosen.contains(id)) {
        problems.push(format!("take {n} has no track {id}"));
    }
    for id in &chosen {
        let Some(track) = take.scene.track(id) else {
            continue;
        };
        match take.stream(track.id.as_str()) {
            Some(stream) if stream.kind == StreamKind::Audio && !track.missing => {
                usable.push((track, stream));
            }
            _ => problems.push(format!("track {} of take {n} is missing media", track.id)),
        }
    }
    if usable.is_empty() {
        return Err(match (chosen.is_empty(), wanted) {
            (false, _) => problems
                .into_iter()
                .find(|p| p.ends_with("missing media"))
                .unwrap_or_else(|| format!("take {n} has no audio track")),
            (true, []) => format!("take {n} has no audio track"),
            (true, [one]) => format!("take {n} has no track {one}"),
            (true, many) => format!(
                "take {n} has none of the tracks {}",
                many.iter()
                    .map(TrackId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    let warning = (!problems.is_empty()).then(|| {
        let used: Vec<&str> = usable.iter().map(|(t, _)| t.id.as_str()).collect();
        format!(
            "{}: silence detected on {} only",
            problems.join(", "),
            used.join(" and ")
        )
    });
    Ok(AnalysisSources {
        tracks: usable,
        warning,
    })
}

/// The skip reason of a clip that would lose everything: "entirely silent at −35 dB".
pub fn entirely_silent(params: &AutocutParams) -> String {
    format!("entirely silent at {}", db_label(params.threshold_db))
}

/// "Autocut take 3: removed 4 silences (6.2 s)" (one take) or "Autocut 2
/// takes: …": the history label of a run that changed clips of `takes`;
/// with `edges_only`, "Autocut take 3: trimmed 2 edges (3.1 s)".
pub fn run_label(
    takes: &BTreeSet<TakeId>,
    silences: usize,
    removed_ns: i64,
    edges_only: bool,
) -> String {
    let (verb, what) = if edges_only {
        ("trimmed", plural(silences, "edge", "edges"))
    } else {
        ("removed", plural(silences, "silence", "silences"))
    };
    format!(
        "Autocut {}: {verb} {silences} {what} ({})",
        takes_label(takes),
        duration_label(removed_ns)
    )
}

/// "Dry run: autocut take 3 would remove 4 silences (6.2 s)"; with
/// `edges_only`, "Dry run: autocut take 3 would trim 2 edges (3.1 s)".
pub fn dry_run_label(
    takes: &BTreeSet<TakeId>,
    silences: usize,
    removed_ns: i64,
    edges_only: bool,
) -> String {
    let (verb, what) = if edges_only {
        ("trim", plural(silences, "edge", "edges"))
    } else {
        ("remove", plural(silences, "silence", "silences"))
    };
    format!(
        "Dry run: autocut {} would {verb} {silences} {what} ({})",
        takes_label(takes),
        duration_label(removed_ns)
    )
}

/// "take 3", or "2 takes".
fn takes_label(takes: &BTreeSet<TakeId>) -> String {
    match takes.iter().next() {
        Some(take) if takes.len() == 1 => format!("take {}", take.number()),
        _ => format!("{} takes", takes.len()),
    }
}

/// A level for people, with the true minus sign (DESIGN.md): `−35 dB`, `−35.5 dB`, `0 dB`.
pub fn db_label(db: f64) -> String {
    let db = if db == 0.0 { 0.0 } else { db };
    format!("{} dB", format!("{db}").replacen('-', "\u{2212}", 1))
}

pub(crate) fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

fn as_i64(f: Frame) -> i64 {
    i64::try_from(f).unwrap_or(i64::MAX)
}

fn as_frame(f: i64) -> Frame {
    Frame::try_from(f).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ClipId;
    use crate::parse::silencedetect::parse_silencedetect;
    use crate::testkit::ProjectBuilder;
    use pretty_assertions::assert_eq;

    const MS: i64 = NS_PER_MS;

    fn params() -> AutocutParams {
        AutocutParams {
            threshold_db: -35.0,
            min_silence_ms: 400,
            padding_ms: 120,
            min_keep_ms: 200,
            edges_only: false,
        }
    }

    fn clip(in_: Frame, out: Frame) -> Clip {
        Clip {
            id: ClipId::from_raw("c001"),
            take: TakeId::numbered(1),
            in_,
            out,
        }
    }

    fn ms(start: i64, end: i64) -> Interval {
        Interval {
            start_ns: start * MS,
            end_ns: end * MS,
        }
    }

    const FPS30: Fps = Fps::new(30, 1);

    #[test]
    fn real_silencedetect_output_end_to_end() {
        // The 7 s speech sample: silences at 0–0.512, 2.005–3.008,
        // 3.605–5.013 and 6.016–7 s; a 7 s clip at 30 fps.
        let stderr = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/silencedetect/speech-trailing.stderr"
        ));
        let silences = parse_silencedetect(stderr).unwrap();
        let cut = cut_clip(&clip(0, 210), &silences, &params(), FPS30);
        // Shrunk by 120 ms: 0.12–0.392, 2.125–2.888, 3.725–4.893, 6.136–6.88.
        // Kept spans 0–0.12 and 6.88–7 are under 200 ms and dropped; the
        // rest snap outward: 0.392 → frame 11, 2.125 → 64, 2.888 → 86,
        // 3.725 → 112, 4.893 → 146, 6.136 → 185.
        assert_eq!(cut.kept, vec![[11, 64], [86, 112], [146, 185]]);
        assert_eq!(cut.silences, 4);
        assert_eq!(cut.removed_frames, 11 + 22 + 34 + 25);
        assert_eq!(
            cut.removed(&clip(0, 210)),
            vec![[0, 11], [64, 86], [112, 146], [185, 210]]
        );
        assert!(cut.changes(&clip(0, 210)));
        // frame_ns(11) + (86 − 64) + (146 − 112) + (210 − 185) frames' worth.
        assert_eq!(cut.removed_ns(&clip(0, 210), FPS30), 3_066_666_666);
    }

    #[test]
    fn offsets_shift_file_time_into_take_time() {
        let project = ProjectBuilder::new()
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 300)
            .with_take("take-0001", |t| {
                t.streams[0].start_offset_ns = 3_000_000;
                t.scene.audio[0].sync_offset_ms = 80;
            })
            .build();
        let take = project.take(&TakeId::numbered(1)).unwrap();
        let (track, stream) = only_source(&project, take);
        assert_eq!(
            to_take_time(&[ms(1000, 2000)], stream, track),
            vec![ms(1083, 2083)],
            "3 ms start offset + 80 ms sync offset"
        );
        let shifted = plan_clip(
            &clip(0, 300),
            stream,
            track,
            &[ms(1000, 2000)],
            &params(),
            FPS30,
        );
        let unshifted = cut_clip(&clip(0, 300), &[ms(1000, 2000)], &params(), FPS30);
        // Removal 1.203–1.963 s → frames [37, 58); unshifted 1.12–1.88 → [34, 56).
        assert_eq!(shifted.kept, vec![[0, 37], [58, 300]]);
        assert_eq!(unshifted.kept, vec![[0, 34], [56, 300]]);

        // A negative sync offset moves the audio earlier.
        let mut early = track.clone();
        early.sync_offset_ms = -1000;
        assert_eq!(
            to_take_time(&[ms(1000, 2000)], stream, &early),
            vec![ms(3, 1003)]
        );
    }

    #[test]
    fn a_clip_without_silence_is_unchanged() {
        for silences in [
            vec![],
            vec![ms(20_000, 30_000)], // after the clip
            vec![ms(1000, 1240)],     // ≤ 2 × padding: shrinks away
            vec![ms(1000, 1000)],     // empty
            vec![ms(-5000, -10)],     // before the take
        ] {
            let cut = cut_clip(&clip(0, 300), &silences, &params(), FPS30);
            assert_eq!(
                cut,
                ClipCut {
                    kept: vec![[0, 300]],
                    silences: 0,
                    removed_frames: 0
                },
                "{silences:?}"
            );
            assert!(!cut.changes(&clip(0, 300)));
        }
    }

    #[test]
    fn an_entirely_silent_clip_keeps_nothing() {
        let cut = cut_clip(&clip(30, 90), &[ms(0, 10_000)], &params(), FPS30);
        assert!(cut.keeps_nothing());
        assert_eq!(cut.silences, 0, "nothing is removed: the clip stays");
        assert_eq!(cut.removed_frames, 0);
        assert!(!cut.changes(&clip(30, 90)));
        assert!(cut.removed(&clip(30, 90)).is_empty());
        // Only edge fragments of padding are left (0–0.1 s and 9.88–10 s),
        // without a sound: still entirely silent.
        let cut = cut_clip(&clip(0, 300), &[ms(-20, 10_000)], &params(), FPS30);
        assert!(cut.keeps_nothing(), "{cut:?}");
        assert_eq!(
            entirely_silent(&params()),
            "entirely silent at \u{2212}35 dB"
        );
    }

    #[test]
    fn min_keep_only_drops_leftovers() {
        // A one-frame clip (33 ms) no silence reaches stays whole even though
        // it is shorter than the 200 ms minimum: nothing was cut from it.
        let whole = ClipCut {
            kept: vec![[0, 1]],
            silences: 0,
            removed_frames: 0,
        };
        assert_eq!(cut_clip(&clip(0, 1), &[], &params(), FPS30), whole);
        assert_eq!(
            cut_clip(&clip(0, 1), &[ms(500, 2000)], &params(), FPS30),
            whole
        );
    }

    #[test]
    fn why_nothing_is_kept() {
        let project = ProjectBuilder::new()
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 300)
            .build();
        let take = project.take(&TakeId::numbered(1)).unwrap();
        let (track, stream) = only_source(&project, take);
        let reason = |c: &Clip, silences: &[Interval]| {
            let cut = plan_clip(c, stream, track, silences, &params(), FPS30);
            assert!(cut.keeps_nothing(), "{cut:?}");
            let one = [TrackSilences {
                track,
                stream,
                silences,
            }];
            nothing_kept_reason(c, &one, &params(), FPS30)
        };
        assert_eq!(
            reason(&clip(30, 90), &[ms(0, 2500), ms(2500, 4000)]),
            "entirely silent at \u{2212}35 dB"
        );
        // 300 ms with a 250 ms silence: 120 ms and 170 ms of padded audio
        // are left, with 50 ms of sound, under 200 ms.
        assert_eq!(
            reason(&clip(0, 9), &[ms(0, 250)]),
            "no sound of 200 ms or more to keep"
        );
    }

    #[test]
    fn short_fragments_are_dropped_and_removals_merge() {
        // Speech 3.9–4.3 s (400 ms, padded to 3.78–4.42) survives a 200 ms
        // min keep, but not a 700 ms one; then both silences become one cut.
        let silences = [ms(0, 3900), ms(4300, 10_000)];
        let cut = cut_clip(&clip(0, 300), &silences, &params(), FPS30);
        assert_eq!(cut.kept, vec![[113, 133]]);
        assert_eq!(cut.silences, 2);
        let strict = AutocutParams {
            min_keep_ms: 700,
            ..params()
        };
        assert!(cut_clip(&clip(0, 300), &silences, &strict, FPS30).keeps_nothing());
        let silences = [ms(1000, 3900), ms(4300, 6000)];
        let cut = cut_clip(&clip(0, 300), &silences, &strict, FPS30);
        assert_eq!(
            cut.kept,
            vec![[0, 34], [176, 300]],
            "one removal 1.12–5.88 s"
        );
        assert_eq!(cut.silences, 1);
    }

    #[test]
    fn min_keep_counts_sound_not_padding() {
        // A 21 µs click splits a 4 s silence in two (ffmpeg ends a silence
        // at one loud sample). Padded, it is a 240 ms fragment, longer than
        // a 200 ms min keep; its sound is not: one cut, 1.12–4.88 s.
        let click = [
            ms(1000, 3000),
            Interval {
                start_ns: 3000 * MS + 21_000,
                end_ns: 5000 * MS,
            },
        ];
        let cut = cut_clip(&clip(0, 300), &click, &params(), FPS30);
        assert_eq!(cut.kept, vec![[0, 34], [146, 300]]);
        assert_eq!(cut.silences, 1);
        // Min keep 0 keeps every sound: the click and its padding stay.
        let all = AutocutParams {
            min_keep_ms: 0,
            ..params()
        };
        let cut = cut_clip(&clip(0, 300), &click, &all, FPS30);
        assert_eq!(cut.kept, vec![[0, 34], [86, 94], [146, 300]]);
        // A 250 ms word between the silences stays at 200 ms.
        let word = [ms(1000, 3000), ms(3250, 5000)];
        let cut = cut_clip(&clip(0, 300), &word, &params(), FPS30);
        assert_eq!(cut.kept, vec![[0, 34], [86, 102], [146, 300]]);
    }

    #[test]
    fn padding_sits_beside_each_cut_inside_the_clip() {
        // Removal 1.12–1.88 s → frames [34, 56): 120 ms (3.6 → 4 frames)
        // of kept silence on each side.
        let one = cut_clip(&clip(0, 300), &[ms(1000, 2000)], &params(), FPS30);
        assert_eq!(
            one.padding(&clip(0, 300), 120, FPS30),
            vec![[30, 34], [56, 60]]
        );
        // A silence across the clip's start: the cut reaches the edge, so
        // only its end has padding; none at all without padding.
        let head = cut_clip(&clip(90, 150), &[ms(2000, 3500)], &params(), FPS30);
        assert_eq!(head.removed(&clip(90, 150)), vec![[90, 101]]);
        assert_eq!(head.padding(&clip(90, 150), 120, FPS30), vec![[101, 105]]);
        assert!(head.padding(&clip(90, 150), 0, FPS30).is_empty());
        // Two silences merged across a click: padding at the outer ends only.
        let click = [
            ms(1000, 3000),
            Interval {
                start_ns: 3000 * MS + 21_000,
                end_ns: 5000 * MS,
            },
        ];
        let merged = cut_clip(&clip(0, 300), &click, &params(), FPS30);
        assert_eq!(
            merged.padding(&clip(0, 300), 120, FPS30),
            vec![[30, 34], [146, 150]]
        );
        // A clip left alone has none.
        let whole = cut_clip(&clip(0, 300), &[], &params(), FPS30);
        assert!(whole.padding(&clip(0, 300), 120, FPS30).is_empty());
    }

    #[test]
    fn touching_and_overlapping_silences_are_one_silence() {
        let apart = cut_clip(
            &clip(0, 300),
            &[ms(1000, 2000), ms(2000, 3000)],
            &params(),
            FPS30,
        );
        let joined = cut_clip(&clip(0, 300), &[ms(1000, 3000)], &params(), FPS30);
        assert_eq!(apart, joined);
        let overlapping = cut_clip(
            &clip(0, 300),
            &[ms(2500, 3000), ms(1000, 2600), ms(1500, 1600)],
            &params(),
            FPS30,
        );
        assert_eq!(overlapping, joined);
        assert_eq!(joined.kept, vec![[0, 34], [86, 300]]);
    }

    #[test]
    fn fragments_touching_after_snapping_merge() {
        // A 50 ms silence with no padding: kept spans 0–1.01 and 1.06–10 s
        // snap to [0, 31) and [31, 300) and merge back into the whole clip.
        let p = AutocutParams {
            padding_ms: 0,
            min_keep_ms: 0,
            ..params()
        };
        let cut = cut_clip(&clip(0, 300), &[ms(1010, 1060)], &p, FPS30);
        assert_eq!(cut.kept, vec![[0, 300]]);
        assert!(!cut.changes(&clip(0, 300)));
        // 100 ms on frame boundaries drops whole frames 30–32.
        let cut = cut_clip(&clip(0, 300), &[ms(1000, 1100)], &p, FPS30);
        assert_eq!(cut.kept, vec![[0, 30], [33, 300]]);
    }

    #[test]
    fn a_clip_inside_a_take_only_sees_its_range() {
        // Clip [90, 150) = 3–5 s of the take. The silence 2–3.5 s starts
        // before the clip, so the clip's head is cut; padding comes from the
        // speech, not from the clip edge.
        let cut = cut_clip(
            &clip(90, 150),
            &[ms(2000, 3500), ms(4600, 9000)],
            &params(),
            FPS30,
        );
        assert_eq!(cut.kept, vec![[101, 142]]);
        assert_eq!(cut.removed(&clip(90, 150)), vec![[90, 101], [142, 150]]);
        assert_eq!(cut.silences, 2);
        assert_eq!(cut.removed_frames, 19);
    }

    #[test]
    fn ntsc_rates_snap_outward() {
        let fps = Fps::new(30000, 1001);
        let cut = cut_clip(&clip(0, 300), &[ms(1000, 2000)], &params(), fps);
        // Removal 1.12–1.88 s. Frame 33 (from 1.1011 s) still holds speech,
        // so the first fragment ends at 34; speech resumes inside frame 56
        // (from 1.8685 s), so the second starts there.
        assert_eq!(cut.kept, vec![[0, 34], [56, 300]]);
        for &[a, b] in &cut.kept {
            assert!(a < b);
        }
    }

    /// The one track and stream autocut analyzes for `take` by default.
    fn only_source<'t>(project: &Project, take: &'t Take) -> (&'t AudioTrack, &'t Stream) {
        let sources = analysis_sources(project, &TakeId::numbered(1), take, &[]).unwrap();
        assert_eq!(sources.tracks.len(), 1);
        sources.tracks[0]
    }

    #[test]
    fn choosing_the_tracks() {
        let project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .system("A1", "alsa_output.monitor")
            .mic("A2", "alsa_input.mic")
            .mic("A3", "alsa_input.usb")
            .take("take-0001", 100)
            .build();
        let take_id = TakeId::numbered(1);
        let take = project.take(&take_id).unwrap();
        let a = |s: &str| s.parse::<TrackId>().unwrap();
        let ids = |sources: AnalysisSources<'_>| {
            sources
                .tracks
                .iter()
                .map(|(t, _)| t.id.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(choose_tracks(&project, take, &[]), [a("A2")], "first mic");
        assert_eq!(choose_tracks(&project, take, &[a("A3")]), [a("A3")]);
        assert_eq!(
            choose_tracks(&project, take, &[a("A3"), a("A1")]),
            [a("A3"), a("A1")]
        );
        assert!(choose_tracks(&project, take, &[a("A9")]).is_empty());

        let mut with_setting = project.clone();
        with_setting.autocut.tracks = vec![a("A1")];
        assert_eq!(choose_tracks(&with_setting, take, &[]), [a("A1")]);
        assert_eq!(
            choose_tracks(&with_setting, take, &[a("A3")]),
            [a("A3")],
            "the override wins over the setting"
        );
        with_setting.autocut.tracks = vec![a("A7")];
        assert!(
            choose_tracks(&with_setting, take, &[]).is_empty(),
            "no fallback"
        );
        assert_eq!(
            analysis_sources(&with_setting, &take_id, take, &[]).unwrap_err(),
            "take 1 has no track A7"
        );
        with_setting.autocut.tracks = vec![a("A7"), a("A8")];
        assert_eq!(
            analysis_sources(&with_setting, &take_id, take, &[]).unwrap_err(),
            "take 1 has none of the tracks A7, A8"
        );
        // Some of them: those, with a warning.
        with_setting.autocut.tracks = vec![a("A1"), a("A7")];
        let sources = analysis_sources(&with_setting, &take_id, take, &[]).unwrap();
        assert_eq!(
            sources.warning.as_deref(),
            Some("take 1 has no track A7: silence detected on A1 only")
        );
        assert_eq!(ids(sources), ["A1"]);
        with_setting.autocut.tracks = vec![a("A1"), a("A3")];
        let sources = analysis_sources(&with_setting, &take_id, take, &[]).unwrap();
        assert_eq!(sources.warning, None);
        assert_eq!(ids(sources), ["A1", "A3"]);

        // No mic: the first track.
        let system_only = ProjectBuilder::new()
            .system("A1", "alsa_output.monitor")
            .system("A2", "alsa_output.monitor2")
            .take("take-0001", 100)
            .build();
        let take = system_only.take(&take_id).unwrap();
        assert_eq!(choose_tracks(&system_only, take, &[]), [a("A1")]);

        // No audio at all.
        let silent = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 100)
            .build();
        let take = silent.take(&take_id).unwrap();
        assert!(choose_tracks(&silent, take, &[]).is_empty());
        assert_eq!(
            analysis_sources(&silent, &take_id, take, &[]).unwrap_err(),
            "take 1 has no audio track"
        );

        // Missing media.
        let lost = project.clone();
        let mut lost_take = lost.take(&take_id).unwrap().clone();
        lost_take.scene.audio[1].missing = true;
        assert_eq!(
            analysis_sources(&lost, &take_id, &lost_take, &[]).unwrap_err(),
            "track A2 of take 1 is missing media"
        );
        let sources = analysis_sources(&lost, &take_id, &lost_take, &[a("A2"), a("A3")]).unwrap();
        assert_eq!(
            sources.warning.as_deref(),
            Some("track A2 of take 1 is missing media: silence detected on A3 only")
        );
        lost_take.scene.audio[1].missing = false;
        lost_take.streams.retain(|s| s.id != "A2");
        assert_eq!(
            analysis_sources(&lost, &take_id, &lost_take, &[]).unwrap_err(),
            "track A2 of take 1 is missing media"
        );
        let (track, stream) = only_source(&project, project.take(&take_id).unwrap());
        assert_eq!(
            (track.id.as_str(), stream.file.as_str()),
            ("A2", "takes/take-0001/A2-mic.mka")
        );
    }

    /// Several tracks: a region is cut only where every one is silent, and
    /// an overlap shorter than Min silence isn't cut.
    #[test]
    fn several_tracks_cut_only_where_all_are_silent() {
        let project = ProjectBuilder::new()
            .mic("A1", "alsa_input.mic")
            .system("A2", "alsa_output.monitor")
            .take("take-0001", 300)
            .build();
        let take = project.take(&TakeId::numbered(1)).unwrap();
        let (a1, a2) = (&take.scene.audio[0], &take.scene.audio[1]);
        let (s1, s2) = (take.stream("A1").unwrap(), take.stream("A2").unwrap());
        // A1 silent 1–3 s and 5–7 s; A2 silent 2–4 s and 6.8–9 s.
        let on_a1 = [ms(1000, 3000), ms(5000, 7000)];
        let on_a2 = [ms(2000, 4000), ms(6800, 9000)];
        let both = [
            TrackSilences {
                track: a1,
                stream: s1,
                silences: &on_a1,
            },
            TrackSilences {
                track: a2,
                stream: s2,
                silences: &on_a2,
            },
        ];
        // 2–3 s is silent on both; 6.8–7 s (200 ms) is under Min silence.
        assert_eq!(combined_silences(&both, 400), vec![ms(2000, 3000)]);
        assert_eq!(
            combined_silences(&both, 100),
            vec![ms(2000, 3000), ms(6800, 7000)]
        );
        // One track: its silences as they are, even short ones.
        assert_eq!(combined_silences(&both[..1], 5000), on_a1.to_vec());
        let p = AutocutParams {
            padding_ms: 0,
            ..params()
        };
        let cut = plan_clip_on(&clip(0, 300), &both, &p, FPS30);
        assert_eq!(cut.kept, vec![[0, 60], [90, 300]]);
    }

    #[test]
    fn labels() {
        let one: BTreeSet<TakeId> = [TakeId::numbered(3)].into();
        assert_eq!(
            run_label(&one, 4, 6_200_000_000, false),
            "Autocut take 3: removed 4 silences (6.2 s)"
        );
        assert_eq!(
            run_label(&one, 1, 450_000_000, false),
            "Autocut take 3: removed 1 silence (0.5 s)"
        );
        let two: BTreeSet<TakeId> = [TakeId::numbered(1), TakeId::numbered(3)].into();
        assert_eq!(
            run_label(&two, 7, 64_000_000_000, false),
            "Autocut 2 takes: removed 7 silences (1 min 4.0 s)"
        );
        assert_eq!(
            run_label(&one, 2, 3_100_000_000, true),
            "Autocut take 3: trimmed 2 edges (3.1 s)"
        );
        assert_eq!(
            dry_run_label(&one, 1, 900_000_000, true),
            "Dry run: autocut take 3 would trim 1 edge (0.9 s)"
        );
        assert_eq!(
            dry_run_label(&two, 4, 6_200_000_000, false),
            "Dry run: autocut 2 takes would remove 4 silences (6.2 s)"
        );
        assert_eq!(db_label(-35.0), "\u{2212}35 dB");
        assert_eq!(db_label(-35.5), "\u{2212}35.5 dB");
        assert_eq!(db_label(0.0), "0 dB");
        assert_eq!(db_label(-0.0), "0 dB");
    }

    #[test]
    fn plans_snapshot() {
        // A small table of cuts across rates and parameters, pinned as a whole.
        #[derive(Serialize)]
        struct Case {
            name: &'static str,
            fps: String,
            clip: [Frame; 2],
            silences_ms: Vec<[i64; 2]>,
            params: AutocutParams,
            cut: ClipCut,
        }
        let p = params();
        let cases = [
            (
                "speech sample",
                FPS30,
                (0, 210),
                vec![[0, 512], [2005, 3008], [3605, 5013], [6016, 7000]],
                p.clone(),
            ),
            (
                "no padding",
                FPS30,
                (0, 210),
                vec![[0, 512], [2005, 3008], [3605, 5013], [6016, 7000]],
                AutocutParams {
                    padding_ms: 0,
                    ..p.clone()
                },
            ),
            (
                "big padding",
                FPS30,
                (0, 210),
                vec![[0, 512], [2005, 3008], [3605, 5013], [6016, 7000]],
                AutocutParams {
                    padding_ms: 400,
                    ..p.clone()
                },
            ),
            (
                "29.97 middle clip",
                Fps::new(30000, 1001),
                (45, 200),
                vec![[1000, 2500], [4100, 4700], [6000, 9000]],
                p.clone(),
            ),
            (
                "25 fps long silence",
                Fps::new(25, 1),
                (0, 500),
                vec![[2000, 15_000]],
                p.clone(),
            ),
            // 250 ms and 40 ms of speech between the silences: both under 300.
            (
                "60 fps min keep",
                Fps::new(60, 1),
                (0, 600),
                vec![[500, 1000], [1250, 4000], [4040, 9000]],
                AutocutParams {
                    min_keep_ms: 300,
                    ..p.clone()
                },
            ),
            (
                "entirely silent",
                FPS30,
                (10, 50),
                vec![[0, 3000]],
                p.clone(),
            ),
            // Issue #15: the speech sample's edges only; its two pauses stay.
            (
                "edges only",
                FPS30,
                (0, 210),
                vec![[0, 512], [2005, 3008], [3605, 5013], [6016, 7000]],
                AutocutParams {
                    edges_only: true,
                    ..p.clone()
                },
            ),
            // A click 40 ms long before the first word goes with the edge.
            (
                "edges only, a click before the first word",
                FPS30,
                (0, 210),
                vec![[0, 1000], [1040, 2005], [3008, 7000]],
                AutocutParams {
                    edges_only: true,
                    ..p.clone()
                },
            ),
            // Silences only inside: nothing to trim.
            (
                "edges only, pauses inside only",
                FPS30,
                (0, 210),
                vec![[2005, 3008], [3605, 5013]],
                AutocutParams {
                    edges_only: true,
                    ..p.clone()
                },
            ),
        ];
        let table: Vec<Case> = cases
            .into_iter()
            .map(|(name, fps, (a, b), silences_ms, params)| {
                let silences: Vec<Interval> = silences_ms.iter().map(|&[s, e]| ms(s, e)).collect();
                Case {
                    name,
                    fps: fps.to_string(),
                    clip: [a, b],
                    cut: cut_clip(&clip(a, b), &silences, &params, fps),
                    silences_ms,
                    params,
                }
            })
            .collect();
        insta::assert_yaml_snapshot!(table);
    }

    /// Issue #15: two tracks, a region cut only when silent on both.
    #[test]
    fn two_tracks_snapshot() {
        #[derive(Serialize)]
        struct Case {
            name: &'static str,
            a1_ms: Vec<[i64; 2]>,
            a2_ms: Vec<[i64; 2]>,
            combined_ms: Vec<[i64; 2]>,
            params: AutocutParams,
            cut: ClipCut,
        }
        let p = params();
        let speech = vec![[0, 512], [2005, 3008], [3605, 5013], [6016, 7000]];
        let cases = [
            // System audio plays a sound under the second pause: it stays.
            (
                "sound on A2 keeps a pause",
                speech.clone(),
                vec![[0, 3500], [5200, 7000]],
                p.clone(),
            ),
            // The silences overlap by 208 ms only, under Min silence (400).
            (
                "a short overlap isn't cut",
                vec![[0, 512], [2005, 3008]],
                vec![[2800, 4000]],
                p.clone(),
            ),
            (
                "edges only on the combined silences",
                speech.clone(),
                vec![[0, 3500], [5200, 7000]],
                AutocutParams {
                    edges_only: true,
                    ..p.clone()
                },
            ),
        ];
        let to_ms = |list: &[Interval]| -> Vec<[i64; 2]> {
            list.iter()
                .map(|i| [i.start_ns / NS_PER_MS, i.end_ns / NS_PER_MS])
                .collect()
        };
        let table: Vec<Case> = cases
            .into_iter()
            .map(|(name, a1_ms, a2_ms, params)| {
                let list = |l: &[[i64; 2]]| l.iter().map(|&[a, b]| ms(a, b)).collect::<Vec<_>>();
                let combined =
                    intersect_silences(&[list(&a1_ms), list(&a2_ms)], params.min_silence_ms);
                Case {
                    name,
                    combined_ms: to_ms(&combined),
                    cut: cut_clip(&clip(0, 210), &combined, &params, FPS30),
                    a1_ms,
                    a2_ms,
                    params,
                }
            })
            .collect();
        insta::assert_yaml_snapshot!(table);
    }
}
