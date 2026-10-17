//! The autocut planner (A§5.7, SPEC §9). Owner: autocut.
//!
//! Per clip and track:
//! 1. silences arrive in **file time**; shift them to take time
//!    `t = x + startOffsetNs + syncOffsetMs·10⁶`;
//! 2. shrink each by `paddingMs` on both sides and drop the empty ones;
//! 3. kept = the clip range minus the shrunk silences;
//! 4. drop kept fragments shorter than `minKeepMs`;
//! 5. snap outward to frames (`in = ns_to_frame_floor`, `out =
//!    ns_to_frame_ceil`), clamp to the clip, merge touching fragments;
//! 6. nothing kept ⇒ the clip is left unchanged and reported "entirely
//!    silent at −35 dB": autocut never deletes a whole clip.
//!
//! Two precisions over A§5.7 (both pinned by `tests/props_autocut.rs`):
//! - Overlapping or touching silences are merged before step 2. ffmpeg never
//!   reports such pairs, but shrinking them one by one would leave a hole of
//!   fake "speech" where they meet.
//! - `minKeepMs` drops *leftover* fragments (SPEC §6.3): a clip no shrunk
//!   silence reaches is kept whole even when it is shorter than that. When
//!   only slivers under `minKeepMs` are left of a clip that does have sound,
//!   step 6 reports "nothing of 200 ms or more to keep" rather than
//!   "entirely silent".
//!
//! Everything here is pure planning; the command, the job and the history
//! entry live in `apply::autocut` and `on_event::autocut`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::{Frame, TakeId, TrackId};
use crate::model::{AudioRole, AudioTrack, Clip, Fps, Project, Scene, Stream, StreamKind, Take};
use crate::protocol::event::Interval;
use crate::state::AutocutParams;
use crate::time::{NS_PER_MS, duration_label, frame_ns, ns_to_frame_ceil, ns_to_frame_floor};

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

    /// The removed media time (each run as `frame_ns(out) − frame_ns(in)`).
    pub fn removed_ns(&self, clip: &Clip, fps: Fps) -> i64 {
        self.removed(clip)
            .iter()
            .map(|&[a, b]| frame_ns(as_i64(b), fps) - frame_ns(as_i64(a), fps))
            .sum()
    }
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
    cut_clip(clip, &to_take_time(silences, stream, track), params, fps)
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

/// Steps 2–6 for silences already in take time.
pub fn cut_clip(clip: &Clip, silences: &[Interval], params: &AutocutParams, fps: Fps) -> ClipCut {
    let range = Interval {
        start_ns: frame_ns(as_i64(clip.in_), fps),
        end_ns: frame_ns(as_i64(clip.out), fps),
    };
    let spans = kept_spans(range, silences, params.padding_ms, params.min_keep_ms);
    let kept = snap_to_frames(&spans, clip, fps);
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
/// silence shrunk by `padding_ms`, without those shorter than `min_keep_ms`.
/// Sorted and disjoint. These are the padded speech regions the cut must keep.
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
    let mut spans = Vec::new();
    let mut cursor = range.start_ns;
    for silence in union(silences) {
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
        spans.retain(|s| s.end_ns.saturating_sub(s.start_ns) >= min_keep);
    }
    spans
}

/// Why a clip whose cut keeps nothing is left alone: "entirely silent at
/// −35 dB" when the silences (file time) cover all of it, else "nothing of
/// 200 ms or more to keep" (its sounds are slivers under `minKeepMs`).
pub fn nothing_kept_reason(
    clip: &Clip,
    stream: &Stream,
    track: &AudioTrack,
    silences: &[Interval],
    params: &AutocutParams,
    fps: Fps,
) -> String {
    let range = Interval {
        start_ns: frame_ns(as_i64(clip.in_), fps),
        end_ns: frame_ns(as_i64(clip.out), fps),
    };
    let covered = union(&to_take_time(silences, stream, track))
        .iter()
        .any(|s| s.start_ns <= range.start_ns && range.end_ns <= s.end_ns);
    if covered {
        entirely_silent(params)
    } else {
        format!("nothing of {} ms or more to keep", params.min_keep_ms)
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

/// The track that drives detection for `take`: the override, else
/// `autocut.track`, else [`default_track`] of the take scene. `None` when
/// the take lacks it (the clip is skipped with a warning).
pub fn choose_track(
    project: &Project,
    take: &Take,
    override_track: Option<&TrackId>,
) -> Option<TrackId> {
    match override_track.or(project.autocut.track.as_ref()) {
        Some(id) => take.scene.track(id).map(|t| t.id.clone()),
        None => default_track(&take.scene).map(|t| t.id.clone()),
    }
}

/// The tracks autocut analyzes for `clips` ([`choose_track`] of each one's
/// take with `override_track`), in clip order, each once. A take recorded
/// with a track the project scene no longer has may be analyzed on it, and
/// takes of different scenes on different tracks: the page's "Detect
/// silence on" shows its scope's first, the lanes' scissors sit on each
/// (R77's audit). A clip whose take lacks the track (skipped) adds none.
pub fn analyzed_tracks<'a>(
    project: &Project,
    clips: impl IntoIterator<Item = &'a Clip>,
    override_track: Option<&TrackId>,
) -> Vec<TrackId> {
    let mut tracks: Vec<TrackId> = Vec::new();
    for clip in clips {
        let Some(take) = project.take(&clip.take) else {
            continue;
        };
        if let Some(track) = choose_track(project, take, override_track)
            && !tracks.contains(&track)
        {
            tracks.push(track);
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

/// The audio track (take scene) and stream autocut analyzes for clips of
/// `take`, or why it can't: "take 3 has no track A2", "take 3 has no audio
/// track", "track A1 of take 3 is missing media".
pub fn analysis_source<'t>(
    project: &Project,
    take_id: &TakeId,
    take: &'t Take,
    override_track: Option<&TrackId>,
) -> Result<(&'t AudioTrack, &'t Stream), String> {
    let n = take_id.number();
    let chosen = choose_track(project, take, override_track);
    let Some(track) = chosen.as_ref().and_then(|id| take.scene.track(id)) else {
        return Err(match override_track.or(project.autocut.track.as_ref()) {
            Some(wanted) => format!("take {n} has no track {wanted}"),
            None => format!("take {n} has no audio track"),
        });
    };
    match take.stream(track.id.as_str()) {
        Some(stream) if stream.kind == StreamKind::Audio && !track.missing => Ok((track, stream)),
        _ => Err(format!("track {} of take {n} is missing media", track.id)),
    }
}

/// The skip reason of a clip that would lose everything: "entirely silent at −35 dB".
pub fn entirely_silent(params: &AutocutParams) -> String {
    format!("entirely silent at {}", db_label(params.threshold_db))
}

/// "Autocut take 3: removed 4 silences (6.2 s)" (one take) or "Autocut 2
/// takes: …": the history label of a run that changed clips of `takes`.
pub fn run_label(takes: &BTreeSet<TakeId>, silences: usize, removed_ns: i64) -> String {
    format!(
        "Autocut {}: removed {silences} {} ({})",
        takes_label(takes),
        plural(silences, "silence", "silences"),
        duration_label(removed_ns)
    )
}

/// "Dry run: autocut take 3 would remove 4 silences (6.2 s)".
pub fn dry_run_label(takes: &BTreeSet<TakeId>, silences: usize, removed_ns: i64) -> String {
    format!(
        "Dry run: autocut {} would remove {silences} {} ({})",
        takes_label(takes),
        plural(silences, "silence", "silences"),
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
        let (track, stream) = analysis_source(&project, &TakeId::numbered(1), take, None).unwrap();
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
        // Only edge fragments under min keep survive (0–0.1 s and 9.88–10 s):
        // still entirely silent. (Between two silences a fragment is at least
        // 2 × padding long.)
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
        let (track, stream) = analysis_source(&project, &TakeId::numbered(1), take, None).unwrap();
        let reason = |c: &Clip, silences: &[Interval]| {
            let cut = plan_clip(c, stream, track, silences, &params(), FPS30);
            assert!(cut.keeps_nothing(), "{cut:?}");
            nothing_kept_reason(c, stream, track, silences, &params(), FPS30)
        };
        assert_eq!(
            reason(&clip(30, 90), &[ms(0, 2500), ms(2500, 4000)]),
            "entirely silent at \u{2212}35 dB"
        );
        // 300 ms with a 250 ms silence: 120 ms and 170 ms of padded sound
        // are left, both under 200 ms.
        assert_eq!(
            reason(&clip(0, 9), &[ms(0, 250)]),
            "nothing of 200 ms or more to keep"
        );
    }

    #[test]
    fn short_fragments_are_dropped_and_removals_merge() {
        // Speech 3.9–4.3 s padded to 3.78–4.42 (640 ms) survives a 200 ms
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

    #[test]
    fn choosing_the_track() {
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
        assert_eq!(
            choose_track(&project, take, None),
            Some(a("A2")),
            "first mic"
        );
        assert_eq!(choose_track(&project, take, Some(&a("A3"))), Some(a("A3")));
        assert_eq!(choose_track(&project, take, Some(&a("A9"))), None);

        let mut with_setting = project.clone();
        with_setting.autocut.track = Some(a("A1"));
        assert_eq!(choose_track(&with_setting, take, None), Some(a("A1")));
        assert_eq!(
            choose_track(&with_setting, take, Some(&a("A3"))),
            Some(a("A3")),
            "the override wins over the setting"
        );
        with_setting.autocut.track = Some(a("A7"));
        assert_eq!(choose_track(&with_setting, take, None), None, "no fallback");
        assert_eq!(
            analysis_source(&with_setting, &take_id, take, None).unwrap_err(),
            "take 1 has no track A7"
        );

        // No mic: the first track.
        let system_only = ProjectBuilder::new()
            .system("A1", "alsa_output.monitor")
            .system("A2", "alsa_output.monitor2")
            .take("take-0001", 100)
            .build();
        let take = system_only.take(&take_id).unwrap();
        assert_eq!(choose_track(&system_only, take, None), Some(a("A1")));

        // No audio at all.
        let silent = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .take("take-0001", 100)
            .build();
        let take = silent.take(&take_id).unwrap();
        assert_eq!(choose_track(&silent, take, None), None);
        assert_eq!(
            analysis_source(&silent, &take_id, take, None).unwrap_err(),
            "take 1 has no audio track"
        );

        // Missing media.
        let lost = project.clone();
        let mut lost_take = lost.take(&take_id).unwrap().clone();
        lost_take.scene.audio[1].missing = true;
        assert_eq!(
            analysis_source(&lost, &take_id, &lost_take, None).unwrap_err(),
            "track A2 of take 1 is missing media"
        );
        lost_take.scene.audio[1].missing = false;
        lost_take.streams.retain(|s| s.id != "A2");
        assert_eq!(
            analysis_source(&lost, &take_id, &lost_take, None).unwrap_err(),
            "track A2 of take 1 is missing media"
        );
        let (track, stream) =
            analysis_source(&project, &take_id, project.take(&take_id).unwrap(), None).unwrap();
        assert_eq!(
            (track.id.as_str(), stream.file.as_str()),
            ("A2", "takes/take-0001/A2-mic.mka")
        );
    }

    #[test]
    fn labels() {
        let one: BTreeSet<TakeId> = [TakeId::numbered(3)].into();
        assert_eq!(
            run_label(&one, 4, 6_200_000_000),
            "Autocut take 3: removed 4 silences (6.2 s)"
        );
        assert_eq!(
            run_label(&one, 1, 450_000_000),
            "Autocut take 3: removed 1 silence (0.5 s)"
        );
        let two: BTreeSet<TakeId> = [TakeId::numbered(1), TakeId::numbered(3)].into();
        assert_eq!(
            run_label(&two, 7, 64_000_000_000),
            "Autocut 2 takes: removed 7 silences (1 min 4.0 s)"
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
            // 40 ms of speech between the last two silences pads to 280 ms < 300.
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
}
