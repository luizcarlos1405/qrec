//! Autocut properties (A§5.7, SPEC §17): kept ⊇ every padded-speech region
//! of a fragment ≥ `minKeepMs`; fragments stay inside the clip; a clip
//! without silence is unchanged. Plus: the planner equals an independent
//! oracle, re-cutting a fragment changes nothing, more padding never keeps
//! less, and the whole command (job, analysis events, one history entry)
//! applies exactly the planned cuts.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;

use qrec_core::apply::history::{redo_checked, undo_checked};
use qrec_core::autocut::{ClipCut, analysis_source, cut_clip, kept_spans, plan_clip};
use qrec_core::ctx::{Ctx, FileFact};
use qrec_core::history::ops::unchecked;
use qrec_core::protocol::command::{AutocutOverrides, ClipScope, Command};
use qrec_core::protocol::effect::Effect;
use qrec_core::protocol::event::{Event, Interval};
use qrec_core::state::{AutocutParams, State};
use qrec_core::testkit::{self, strategies};
use qrec_core::time::{NS_PER_MS, frame_ns, ns_to_frame_ceil, ns_to_frame_floor};
use qrec_core::{Clip, ClipId, Fps, Project, StreamKind, TakeId};

// ----------------------------------------------------------------- oracle

/// The padded speech regions of `range`, computed the other way round from
/// the planner: speech = the complement of the silences on the whole line,
/// dilated by `pad`, merged, cut to the range; then the short ones dropped.
fn oracle_spans(range: Interval, silences: &[Interval], pad: i64, min_keep: i64) -> Vec<Interval> {
    let mut sorted: Vec<(i64, i64)> = silences
        .iter()
        .filter(|s| s.start_ns < s.end_ns)
        .map(|s| (s.start_ns, s.end_ns))
        .collect();
    sorted.sort();
    let mut speech = Vec::new();
    let mut cursor = i64::MIN;
    for (a, b) in sorted {
        if a > cursor {
            speech.push((cursor, a));
        }
        cursor = cursor.max(b);
    }
    if cursor < i64::MAX {
        speech.push((cursor, i64::MAX));
    }
    let mut dilated: Vec<(i64, i64)> = Vec::new();
    for (x, y) in speech {
        let (x, y) = (x.saturating_sub(pad), y.saturating_add(pad));
        match dilated.last_mut() {
            Some(last) if x <= last.1 => last.1 = last.1.max(y),
            _ => dilated.push((x, y)),
        }
    }
    let inside: Vec<Interval> = dilated
        .into_iter()
        .map(|(x, y)| (x.max(range.start_ns), y.min(range.end_ns)))
        .filter(|(x, y)| x < y)
        .map(|(start_ns, end_ns)| Interval { start_ns, end_ns })
        .collect();
    if inside == [range] {
        // No silence reaches the range: nothing is a leftover.
        return inside;
    }
    inside
        .into_iter()
        .filter(|s| s.end_ns - s.start_ns >= min_keep)
        .collect()
}

/// Snap outward and merge, written out independently of the planner.
fn snapped(spans: &[Interval], clip: &Clip, fps: Fps) -> Vec<[u64; 2]> {
    let mut out: Vec<[u64; 2]> = Vec::new();
    for s in spans {
        let a = (ns_to_frame_floor(s.start_ns, fps).max(0) as u64).clamp(clip.in_, clip.out);
        let b = (ns_to_frame_ceil(s.end_ns, fps).max(0) as u64).clamp(clip.in_, clip.out);
        if a >= b {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if a <= last[1] {
                last[1] = last[1].max(b);
                continue;
            }
        }
        out.push([a, b]);
    }
    out
}

fn range_of(clip: &Clip, fps: Fps) -> Interval {
    Interval {
        start_ns: frame_ns(clip.in_ as i64, fps),
        end_ns: frame_ns(clip.out as i64, fps),
    }
}

// ------------------------------------------------------------- strategies

fn arb_clip() -> impl Strategy<Value = Clip> {
    (0u64..2000, 1u64..2000).prop_map(|(in_, len)| Clip {
        id: ClipId::from_raw("c001"),
        take: TakeId::numbered(1),
        in_,
        out: in_ + len,
    })
}

/// Silences in take time: up to 12, maybe overlapping, with ns jitter,
/// from before the take to past the longest clip.
fn arb_silences() -> impl Strategy<Value = Vec<Interval>> {
    prop::collection::vec(
        (
            -2_000i64..90_000,
            0i64..12_000,
            0i64..1_000_000,
            0i64..1_000_000,
        ),
        0..12,
    )
    .prop_map(|raw| {
        raw.into_iter()
            .map(|(start_ms, len_ms, j1, j2)| {
                let start_ns = start_ms * NS_PER_MS + j1;
                Interval {
                    start_ns,
                    end_ns: start_ns + len_ms * NS_PER_MS + j2,
                }
            })
            .collect()
    })
}

fn arb_params() -> impl Strategy<Value = AutocutParams> {
    (0u32..1_000, 0u32..2_000).prop_map(|(padding_ms, min_keep_ms)| AutocutParams {
        threshold_db: -35.0,
        min_silence_ms: 400,
        padding_ms,
        min_keep_ms,
    })
}

fn ms_ns(ms: u32) -> i64 {
    i64::from(ms) * NS_PER_MS
}

// ------------------------------------------------------------ the planner

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The planner's spans are the oracle's, and its frames the oracle's
    /// spans snapped outward and merged.
    #[test]
    fn the_plan_matches_the_oracle(
        fps in strategies::arb_fps(),
        clip in arb_clip(),
        silences in arb_silences(),
        params in arb_params(),
    ) {
        let range = range_of(&clip, fps);
        let pad = ms_ns(params.padding_ms);
        let min_keep = ms_ns(params.min_keep_ms);
        let expected = oracle_spans(range, &silences, pad, min_keep);
        prop_assert_eq!(
            kept_spans(range, &silences, params.padding_ms, params.min_keep_ms),
            expected.clone()
        );
        let cut = cut_clip(&clip, &silences, &params, fps);
        prop_assert_eq!(&cut.kept, &snapped(&expected, &clip, fps));
    }

    /// A§5.7: kept ⊇ every padded-speech region of a fragment ≥ minKeepMs.
    /// Speech is never clipped: each such region lies inside one kept range.
    #[test]
    fn padded_speech_is_always_kept(
        fps in strategies::arb_fps(),
        clip in arb_clip(),
        silences in arb_silences(),
        params in arb_params(),
    ) {
        let cut = cut_clip(&clip, &silences, &params, fps);
        let regions = oracle_spans(
            range_of(&clip, fps),
            &silences,
            ms_ns(params.padding_ms),
            ms_ns(params.min_keep_ms),
        );
        for region in &regions {
            let covered = cut.kept.iter().any(|&[a, b]| {
                frame_ns(a as i64, fps) <= region.start_ns && region.end_ns <= frame_ns(b as i64, fps)
            });
            prop_assert!(covered, "{region:?} not inside {:?}", cut.kept);
        }
        prop_assert_eq!(cut.keeps_nothing(), regions.is_empty());
    }

    /// A§5.7: fragments stay inside the clip; they are sorted, non-empty and
    /// separated by removed frames; the counts add up.
    #[test]
    fn fragments_stay_inside_the_clip(
        fps in strategies::arb_fps(),
        clip in arb_clip(),
        silences in arb_silences(),
        params in arb_params(),
    ) {
        let cut = cut_clip(&clip, &silences, &params, fps);
        let mut previous_out = None;
        for &[a, b] in &cut.kept {
            prop_assert!(clip.in_ <= a && a < b && b <= clip.out, "{:?} outside {clip:?}", cut.kept);
            if let Some(out) = previous_out {
                prop_assert!(out < a, "touching fragments must be merged: {:?}", cut.kept);
            }
            previous_out = Some(b);
        }
        if cut.keeps_nothing() {
            prop_assert_eq!((cut.silences, cut.removed_frames), (0, 0));
            prop_assert!(!cut.changes(&clip));
        } else {
            let kept: u64 = cut.kept.iter().map(|[a, b]| b - a).sum();
            prop_assert_eq!(kept + cut.removed_frames, clip.len());
            prop_assert_eq!(cut.silences, cut.removed(&clip).len());
            prop_assert_eq!(cut.changes(&clip), cut.removed_frames > 0);
        }
    }

    /// A§5.7: a clip without silence is unchanged — no silences at all, or
    /// only ones that miss the clip or vanish in the padding.
    #[test]
    fn a_clip_without_silence_is_unchanged(
        fps in strategies::arb_fps(),
        clip in arb_clip(),
        params in arb_params(),
        outside in arb_silences(),
    ) {
        let whole = ClipCut { kept: vec![[clip.in_, clip.out]], silences: 0, removed_frames: 0 };
        prop_assert_eq!(cut_clip(&clip, &[], &params, fps), whole.clone());

        // Move every silence out of the clip's reach: ending at most `pad`
        // into its start or starting at most `pad` before its end. (Both
        // sides only when they can't meet: a silence across the whole clip
        // is a different story.)
        let range = range_of(&clip, fps);
        let pad = ms_ns(params.padding_ms);
        let both_sides = range.end_ns - pad > range.start_ns + pad;
        let away: Vec<Interval> = outside
            .iter()
            .map(|s| {
                let len = s.end_ns - s.start_ns;
                if s.start_ns % 2 == 0 || !both_sides {
                    Interval { start_ns: range.end_ns - pad, end_ns: range.end_ns - pad + len }
                } else {
                    Interval { start_ns: range.start_ns + pad - len, end_ns: range.start_ns + pad }
                }
            })
            .collect();
        prop_assert_eq!(cut_clip(&clip, &away, &params, fps), whole.clone());

        // A silence no longer than twice the padding shrinks to nothing.
        // (Overlapping short silences are one longer silence, so one at a time.)
        for s in &outside {
            let short = Interval { start_ns: s.start_ns, end_ns: s.start_ns + 2 * pad };
            prop_assert_eq!(cut_clip(&clip, &[short], &params, fps), whole.clone());
        }
    }

    /// Cutting a fragment again with the same silences changes nothing.
    #[test]
    fn recutting_a_fragment_changes_nothing(
        fps in strategies::arb_fps(),
        clip in arb_clip(),
        silences in arb_silences(),
        params in arb_params(),
    ) {
        let cut = cut_clip(&clip, &silences, &params, fps);
        for &[a, b] in &cut.kept {
            let fragment = Clip { in_: a, out: b, ..clip.clone() };
            let again = cut_clip(&fragment, &silences, &params, fps);
            prop_assert_eq!(&again.kept, &vec![[a, b]], "fragment {:?} of {:?}", [a, b], cut.kept);
        }
    }

    /// More padding never keeps less; a longer minimum fragment never keeps more.
    #[test]
    fn padding_and_min_keep_are_monotonic(
        fps in strategies::arb_fps(),
        clip in arb_clip(),
        silences in arb_silences(),
        params in arb_params(),
        more in 0u32..500,
    ) {
        let base = cut_clip(&clip, &silences, &params, fps);
        let padded = cut_clip(&clip, &silences, &AutocutParams { padding_ms: params.padding_ms + more, ..params.clone() }, fps);
        let strict = cut_clip(&clip, &silences, &AutocutParams { min_keep_ms: params.min_keep_ms + more, ..params.clone() }, fps);
        let inside = |small: &[[u64; 2]], big: &[[u64; 2]]| {
            small.iter().all(|&[a, b]| big.iter().any(|&[c, d]| c <= a && b <= d))
        };
        prop_assert!(inside(&base.kept, &padded.kept), "{:?} ⊄ {:?}", base.kept, padded.kept);
        prop_assert!(inside(&strict.kept, &base.kept), "{:?} ⊄ {:?}", strict.kept, base.kept);
    }
}

// --------------------------------------------------------------- the flow

/// Silences (file time) for each audio file of `project`, cycled from `pool`.
fn assign(project: &Project, pool: &[Vec<Interval>]) -> BTreeMap<String, Vec<Interval>> {
    project
        .takes
        .values()
        .flat_map(|t| t.streams.iter())
        .filter(|s| s.kind == StreamKind::Audio)
        .enumerate()
        .map(|(i, s)| (s.file.clone(), pool[i % pool.len()].clone()))
        .collect()
}

fn ctx_for(project: &Project) -> Ctx {
    let mut ctx = testkit::ctx("2026-10-14T12:30:00Z", 99);
    for (i, s) in project
        .takes
        .values()
        .flat_map(|t| t.streams.iter())
        .enumerate()
    {
        ctx.facts.files.insert(
            s.file.clone(),
            FileFact {
                path: s.file.clone(),
                bytes: 1_000 + i as u64,
                mtime_ns: 1,
            },
        );
    }
    ctx
}

/// A fake shell that answers every `DetectSilence` from `silences`.
fn shell(
    silences: &BTreeMap<String, Vec<Interval>>,
) -> impl FnMut(&State, &Effect) -> Vec<Event> + '_ {
    move |_, effect| match effect {
        Effect::DetectSilence { job, key, argv } => {
            let file = argv
                .iter()
                .find_map(|a| a.strip_prefix("file:"))
                .expect("the argv names its input");
            vec![Event::SilenceDetected {
                job: *job,
                key: key.clone(),
                silences: silences.get(file).cloned().unwrap_or_default(),
            }]
        }
        _ => Vec::new(),
    }
}

/// Random offsets: audio files start a little late, tracks are re-synced.
fn with_offsets(mut project: Project, offsets: &[(i64, i32)]) -> Project {
    let mut i = 0;
    for take in project.takes.values_mut() {
        for track in take.scene.audio.iter_mut() {
            let (start, sync) = offsets[i % offsets.len()];
            i += 1;
            track.sync_offset_ms = sync;
            if let Some(stream) = take.streams.iter_mut().find(|s| s.id == track.id.as_str()) {
                stream.start_offset_ns = start;
            }
        }
    }
    project
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// `qrec autocut` end to end: one job, the analysis events, then exactly
    /// the planned cuts in place of each clip (first fragment keeps the id,
    /// fresh unique ids for the others, untouched clips stay), and one
    /// history entry if and only if something changed, undone and redone
    /// as one step.
    #[test]
    fn the_command_applies_exactly_the_plan(
        project in strategies::arb_project(),
        pool in prop::collection::vec(arb_silences(), 1..4),
        offsets in prop::collection::vec((0i64..50_000_000, -300i32..300), 1..4),
        params in arb_params(),
    ) {
        let project = with_offsets(project, &offsets);
        let silences = assign(&project, &pool);
        let mut state = testkit::state(project);
        state.project.autocut.padding_ms = params.padding_ms;
        state.project.autocut.min_keep_ms = params.min_keep_ms;
        let ctx = ctx_for(&state.project);
        let cmd = Command::Autocut { scope: ClipScope::All, overrides: AutocutOverrides::default(), dry_run: false };
        let result = testkit::drive(&state, &ctx, &[cmd], shell(&silences));
        if state.project.timeline.clips.is_empty() {
            prop_assert!(result.is_err(), "nothing to autocut");
            return Ok(());
        }
        let after = result.expect("autocut runs");
        prop_assert!(after.runtime.jobs.is_empty(), "the job completed");

        // The expected timeline, from the pure planner.
        let project = &state.project;
        let fps = project.canvas.fps;
        let mut expected: Vec<(Option<ClipId>, TakeId, u64, u64)> = Vec::new();
        for clip in &project.timeline.clips {
            let take = project.take(&clip.take).unwrap();
            let kept = match analysis_source(project, &clip.take, take, None) {
                Ok((track, stream)) => {
                    let cut = plan_clip(clip, stream, track, &silences[&stream.file], &params, fps);
                    if cut.keeps_nothing() { vec![[clip.in_, clip.out]] } else { cut.kept }
                }
                Err(_) => vec![[clip.in_, clip.out]],
            };
            for (i, [a, b]) in kept.into_iter().enumerate() {
                let id = (i == 0).then(|| clip.id.clone());
                expected.push((id, clip.take.clone(), a, b));
            }
        }
        let got = &after.project.timeline.clips;
        prop_assert_eq!(got.len(), expected.len());
        for (clip, (id, take, a, b)) in got.iter().zip(&expected) {
            prop_assert_eq!((&clip.take, clip.in_, clip.out), (take, *a, *b));
            if let Some(id) = id {
                prop_assert_eq!(&clip.id, id, "the first fragment keeps the id");
            }
        }
        let ids: BTreeSet<&ClipId> = got.iter().map(|c| &c.id).collect();
        prop_assert_eq!(ids.len(), got.len(), "clip ids stay unique");

        let changed = after.project.timeline != state.project.timeline;
        prop_assert_eq!(after.history.entries.len(), usize::from(changed));
        if changed {
            let entry = &after.history.entries[0];
            prop_assert!(entry.label.starts_with("Autocut "), "{}", entry.label);
            prop_assert_eq!(&entry.cmd["cmd"], "autocut");
            // One entry for the whole run: one undo restores every clip, one
            // redo cuts them all again (with every guarding test passing).
            let undone = undo_checked(&after, &ctx, 1, unchecked).expect("undo");
            prop_assert_eq!(&undone.state.project, &state.project);
            let redone = redo_checked(&undone.state, &ctx, 1, unchecked).expect("redo");
            prop_assert_eq!(&redone.state.project, &after.project);
        }
        // Nothing but the timeline (and the runtime cache) changed.
        let mut same = after.project.clone();
        same.timeline = state.project.timeline.clone();
        prop_assert_eq!(&same, &state.project);
    }
}
