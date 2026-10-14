//! Pure timeline operations over `&[Clip]` (A§5.2). Owner: timeline.
//!
//! The timeline is one magnetic track: clips sit back to back, so the
//! position of clip `i` is the sum of the lengths of the clips before it
//! (a prefix sum). Every function here takes the clip list and returns a
//! new one; none of them looks at the take pool except through the
//! arguments it is given.
//!
//! Time arguments (A§5.2) are resolved here too, so every command reads
//! them the same way: clip times are clip-relative unless `timeline`
//! (absolute `T` ⇒ `in + T` for both edges; a relative `±T` moves that
//! edge); with `timeline`, `T` is a timeline position inside the clip.
//! `FrameSave.at` and `SeekTarget::To` are timeline frames. Relative values
//! are only valid for `seek --by` and trim edges, everything else is `Usage`.

use std::collections::BTreeSet;

use crate::error::CoreError;
use crate::ids::{ClipId, Frame, TakeId};
use crate::model::{Clip, Fps};
use crate::protocol::command::MoveTo;
use crate::protocol::reply::ClipRow;
use crate::time::{TimeArg, timecode};

/// The timeline frame where clip `index` starts (`total_frames` for `index == len`).
pub fn position_of(clips: &[Clip], index: usize) -> Frame {
    clips[..index.min(clips.len())].iter().map(Clip::len).sum()
}

/// The timeline length in frames.
pub fn total_frames(clips: &[Clip]) -> Frame {
    clips.iter().map(Clip::len).sum()
}

/// The clip under timeline frame `frame` and the offset into it; `None` at
/// or past the end.
pub fn clip_at(clips: &[Clip], frame: Frame) -> Option<(usize, Frame)> {
    let mut start = 0;
    for (i, clip) in clips.iter().enumerate() {
        let end = start + clip.len();
        if frame < end {
            return Some((i, frame - start));
        }
        start = end;
    }
    None
}

/// What timeline frame `frame` shows: the take and the take frame. `None`
/// at or past the end. Splits never change this mapping.
pub fn source_at(clips: &[Clip], frame: Frame) -> Option<(&TakeId, Frame)> {
    clip_at(clips, frame).map(|(i, offset)| (&clips[i].take, clips[i].in_ + offset))
}

/// The index of the clip with this id.
pub fn index_of(clips: &[Clip], id: &ClipId) -> Option<usize> {
    clips.iter().position(|c| &c.id == id)
}

/// Every clip boundary on the timeline: `0`, each clip's end (ascending, deduplicated).
pub fn edit_points(clips: &[Clip]) -> Vec<Frame> {
    let mut points = Vec::with_capacity(clips.len() + 1);
    points.push(0);
    let mut end = 0;
    for clip in clips {
        end += clip.len();
        if points.last() != Some(&end) {
            points.push(end);
        }
    }
    points
}

/// The clips with these ids sit next to each other on the timeline (in any
/// order). Every id must be on the timeline; an empty list is not a run.
pub fn is_contiguous(clips: &[Clip], ids: &[ClipId]) -> bool {
    let wanted: BTreeSet<&ClipId> = ids.iter().collect();
    if wanted.is_empty() {
        return false;
    }
    let mut found = BTreeSet::new();
    let mut indices = Vec::new();
    for (i, clip) in clips.iter().enumerate() {
        if wanted.contains(&clip.id) {
            found.insert(&clip.id);
            indices.push(i);
        }
    }
    match (indices.first(), indices.last()) {
        (Some(first), Some(last)) => found.len() == wanted.len() && last - first + 1 == indices.len(),
        _ => false,
    }
}

/// Splits clip `i` at take frame `take_frame` (`in < take_frame < out`):
/// the left part keeps the id, the right part gets `new_id`. Property: the
/// timeline frame → (take, take frame) mapping and the export/GES plans
/// don't change (the two parts are continuous, so they stay one render unit).
pub fn split(
    clips: &[Clip],
    i: usize,
    take_frame: Frame,
    new_id: ClipId,
) -> Result<Vec<Clip>, CoreError> {
    let clip = clip_index(clips, i)?;
    if !(clip.in_ < take_frame && take_frame < clip.out) {
        return Err(CoreError::invalid(format!(
            "can't split clip {} at take frame {take_frame}: the cut must be strictly inside [{}, {})",
            clip.id, clip.in_, clip.out
        )));
    }
    if index_of(clips, &new_id).is_some() {
        return Err(CoreError::invalid(format!(
            "clip id {new_id} is already on the timeline"
        )));
    }
    let right = Clip {
        id: new_id,
        take: clip.take.clone(),
        in_: take_frame,
        out: clip.out,
    };
    let mut out = clips.to_vec();
    out[i].out = take_frame;
    out.insert(i + 1, right);
    Ok(out)
}

/// The result of [`trim`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trimmed {
    pub clips: Vec<Clip>,
    /// A requested edge was outside the take and got clamped.
    pub clamped: bool,
}

/// Sets clip `i`'s range to `[new_in, new_out)`, clamped to
/// `[0, take_duration]`; an empty result is `Invalid`.
pub fn trim(
    clips: &[Clip],
    i: usize,
    new_in: i64,
    new_out: i64,
    take_duration: Frame,
) -> Result<Trimmed, CoreError> {
    let clip = clip_index(clips, i)?;
    let max = i64::try_from(take_duration).unwrap_or(i64::MAX);
    let in_ = new_in.clamp(0, max);
    let out = new_out.clamp(0, max);
    if in_ >= out {
        return Err(CoreError::invalid(format!(
            "trimming clip {} to [{in_}, {out}) would leave it empty",
            clip.id
        ))
        .with_hint("a clip keeps at least one frame; remove it with qrec clip remove instead"));
    }
    let mut out_clips = clips.to_vec();
    out_clips[i].in_ = in_ as Frame;
    out_clips[i].out = out as Frame;
    Ok(Trimmed {
        clips: out_clips,
        clamped: in_ != new_in || out != new_out,
    })
}

/// Moves the clips `ids` as one block in their original (timeline) order.
/// `Index(i)` indexes the list without the block (clamped to its end);
/// `By(n)` shifts the block's first index (clamped); `Before`/`After(c)`
/// anchor on a clip outside the block. Property: the result is a
/// permutation with the block contiguous and in order.
pub fn move_block(clips: &[Clip], ids: &[ClipId], to: &MoveTo) -> Result<Vec<Clip>, CoreError> {
    let wanted: BTreeSet<&ClipId> = ids.iter().collect();
    if wanted.is_empty() {
        return Err(CoreError::usage("no clips to move"));
    }
    if let Some(missing) = wanted.iter().find(|id| index_of(clips, id).is_none()) {
        return Err(unknown_clip(missing));
    }
    let first = clips
        .iter()
        .position(|c| wanted.contains(&c.id))
        .unwrap_or_default();
    let (block, rest): (Vec<Clip>, Vec<Clip>) =
        clips.iter().cloned().partition(|c| wanted.contains(&c.id));
    let at = match to {
        MoveTo::Index(i) => (*i).min(rest.len()),
        MoveTo::By(n) => (first as i128 + i128::from(*n)).clamp(0, rest.len() as i128) as usize,
        MoveTo::Before(anchor) | MoveTo::After(anchor) => {
            if wanted.contains(anchor) {
                return Err(CoreError::invalid(format!(
                    "can't move clips {} {anchor}: {anchor} is one of the clips being moved",
                    if matches!(to, MoveTo::Before(_)) { "before" } else { "after" }
                )));
            }
            let idx = index_of(&rest, anchor).ok_or_else(|| unknown_clip(anchor))?;
            if matches!(to, MoveTo::Before(_)) { idx } else { idx + 1 }
        }
    };
    let mut out = rest;
    out.splice(at..at, block);
    Ok(out)
}

/// The timeline without these clips.
pub fn remove(clips: &[Clip], ids: &[ClipId]) -> Vec<Clip> {
    let gone: BTreeSet<&ClipId> = ids.iter().collect();
    clips.iter().filter(|c| !gone.contains(&c.id)).cloned().collect()
}

/// Inserts `new` before index `index` (clamped to the end).
pub fn insert(clips: &[Clip], index: usize, new: Vec<Clip>) -> Vec<Clip> {
    let at = index.min(clips.len());
    let mut out = clips.to_vec();
    out.splice(at..at, new);
    out
}

/// Replaces `len` clips starting at `start` with `with` (record over,
/// autocut). Both are clamped to the list.
pub fn replace_run(clips: &[Clip], start: usize, len: usize, with: Vec<Clip>) -> Vec<Clip> {
    let start = start.min(clips.len());
    let end = start.saturating_add(len).min(clips.len());
    let mut out = clips.to_vec();
    out.splice(start..end, with);
    out
}

/// `qrec clip list`'s row for clip `i`.
pub fn clip_row(clips: &[Clip], i: usize) -> ClipRow {
    let clip = &clips[i];
    ClipRow {
        id: clip.id.clone(),
        take: clip.take.clone(),
        in_: clip.in_,
        out: clip.out,
        start: position_of(clips, i),
        duration: clip.len(),
    }
}

// --------------------------------------------------------- time arguments

/// A clip edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    Start,
    End,
}

impl Edge {
    pub fn as_str(self) -> &'static str {
        match self {
            Edge::Start => "start",
            Edge::End => "end",
        }
    }
}

/// An absolute timeline frame (`FrameSave.at`, `SeekTarget::To`, `split
/// --timeline`). Relative values are `Usage`; `what` names the argument in
/// the message ("seek --to").
pub fn absolute_frame(arg: &TimeArg, fps: Fps, what: &str) -> Result<Frame, CoreError> {
    if arg.is_relative() {
        return Err(CoreError::usage(format!(
            "{what} takes an absolute time, not the relative {arg}"
        ))
        .with_hint("drop the + or -; relative times only work for seek --by and trim edges"));
    }
    Ok(magnitude(arg, fps)? as Frame)
}

/// A signed frame delta (`seek --by`, relative trim edges); an unsigned value counts as `+`.
pub fn delta_frames(arg: &TimeArg, fps: Fps) -> Result<i64, CoreError> {
    let m = magnitude(arg, fps)?;
    Ok(if arg.sign == crate::time::Sign::Minus { -m } else { m })
}

/// The take frame a trim argument puts `edge` of clip `i` at (A§5.2):
/// relative `±T` moves that edge; absolute `T` is `in + T` for both edges;
/// with `timeline`, an absolute `T` must be a timeline position inside the
/// clip (its end included) and maps to `in + (T − position)`. The result is
/// unclamped: [`trim`] clamps it to the take.
pub fn trim_edge(
    clips: &[Clip],
    i: usize,
    edge: Edge,
    arg: &TimeArg,
    timeline: bool,
    fps: Fps,
) -> Result<i64, CoreError> {
    let clip = clip_index(clips, i)?;
    if arg.is_relative() {
        let base = match edge {
            Edge::Start => clip.in_,
            Edge::End => clip.out,
        };
        return Ok(offset(base, delta_frames(arg, fps)?));
    }
    let t = magnitude(arg, fps)? as Frame;
    if !timeline {
        return Ok(offset(clip.in_, t as i64));
    }
    let pos = position_of(clips, i);
    let end = pos + clip.len();
    if t < pos || t > end {
        return Err(CoreError::invalid(format!(
            "{} is outside clip {} ({}–{} on the timeline)",
            timecode(t, fps),
            clip.id,
            timecode(pos, fps),
            timecode(end, fps)
        ))
        .with_hint(format!(
            "use a time inside the clip, or a relative value like {}-10f to move the {} edge",
            if edge == Edge::Start { "--start " } else { "--end " },
            edge.as_str()
        )));
    }
    Ok(offset(clip.in_, (t - pos) as i64))
}

/// The take frame where `split --at` cuts clip `i` (A§5.2): clip-relative
/// `T` ⇒ `in + T`; with `timeline`, `T` is a timeline position strictly
/// inside the clip. Relative values are `Usage`; a cut at or outside an
/// edge is `Invalid`.
pub fn split_point(
    clips: &[Clip],
    i: usize,
    arg: &TimeArg,
    timeline: bool,
    fps: Fps,
) -> Result<Frame, CoreError> {
    let clip = clip_index(clips, i)?;
    let t = absolute_frame(arg, fps, "clip split --at")?;
    if clip.len() < 2 {
        let frames = if clip.len() == 1 { "1 frame".to_owned() } else { format!("{} frames", clip.len()) };
        return Err(CoreError::invalid(format!(
            "clip {} is {frames} long and can't be split",
            clip.id
        )));
    }
    if !timeline {
        if t == 0 || t >= clip.len() {
            return Err(CoreError::invalid(format!(
                "can't split clip {} at {arg}: the cut must be inside the clip (1f to {}f)",
                clip.id,
                clip.len() - 1
            ))
            .with_hint("--at is relative to the clip's start; add --timeline for a timeline position"));
        }
        return Ok(clip.in_ + t);
    }
    let pos = position_of(clips, i);
    let end = pos + clip.len();
    if t <= pos || t >= end {
        return Err(CoreError::invalid(format!(
            "can't split clip {} at {}: the cut must be inside the clip ({}–{} on the timeline)",
            clip.id,
            timecode(t, fps),
            timecode(pos, fps),
            timecode(end, fps)
        )));
    }
    Ok(clip.in_ + (t - pos))
}

/// The magnitude of a time argument in frames; values too large for the
/// timeline are `Usage` (they would wrap).
fn magnitude(arg: &TimeArg, fps: Fps) -> Result<i64, CoreError> {
    let m = arg.magnitude_frames(fps)?;
    if m < 0 {
        return Err(CoreError::usage(format!("time {arg} is out of range")));
    }
    Ok(m)
}

/// `base + delta` without overflow (saturating at the `i64` range).
fn offset(base: Frame, delta: i64) -> i64 {
    (i128::from(base) + i128::from(delta)).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn clip_index(clips: &[Clip], i: usize) -> Result<&Clip, CoreError> {
    clips.get(i).ok_or_else(|| {
        CoreError::invalid(format!(
            "no clip at index {i} (the timeline has {} clips)",
            clips.len()
        ))
    })
}

/// `NotFound` for a clip id that isn't on the timeline.
pub fn unknown_clip(id: &ClipId) -> CoreError {
    CoreError::not_found(format!("no clip {id} on the timeline"))
        .with_hint("list the clips with qrec clip list")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    const F30: Fps = Fps::new(30, 1);

    fn clip(id: &str, in_: Frame, out: Frame) -> Clip {
        Clip {
            id: ClipId::from_raw(id),
            take: TakeId::numbered(1),
            in_,
            out,
        }
    }

    fn ids(clips: &[Clip]) -> Vec<&str> {
        clips.iter().map(|c| c.id.as_str()).collect()
    }

    fn cid(id: &str) -> ClipId {
        ClipId::from_raw(id)
    }

    fn t(s: &str) -> TimeArg {
        s.parse().unwrap()
    }

    /// Five clips of 10 frames each at take frames 0, 100, 200, 300, 400.
    fn five() -> Vec<Clip> {
        (0..5)
            .map(|n| clip(&format!("c{n}"), n * 100, n * 100 + 10))
            .collect()
    }

    #[test]
    fn positions_are_prefix_sums() {
        let clips = [clip("c1", 0, 10), clip("c2", 50, 55), clip("c3", 5, 25)];
        assert_eq!(total_frames(&clips), 35);
        assert_eq!(position_of(&clips, 0), 0);
        assert_eq!(position_of(&clips, 2), 15);
        assert_eq!(position_of(&clips, 3), 35);
        assert_eq!(clip_at(&clips, 0), Some((0, 0)));
        assert_eq!(clip_at(&clips, 10), Some((1, 0)));
        assert_eq!(clip_at(&clips, 34), Some((2, 19)));
        assert_eq!(clip_at(&clips, 35), None);
        assert_eq!(clip_at(&[], 0), None);
        assert_eq!(source_at(&clips, 12).map(|(_, f)| f), Some(52));
        assert_eq!(source_at(&clips, 35), None);
        assert_eq!(index_of(&clips, &cid("c3")), Some(2));
        assert_eq!(index_of(&clips, &cid("c9")), None);
        let row = clip_row(&clips, 2);
        assert_eq!((row.start, row.duration, row.in_, row.out), (15, 20, 5, 25));
    }

    #[test]
    fn edit_points_are_the_boundaries() {
        assert_eq!(edit_points(&[]), vec![0]);
        let clips = [clip("c1", 0, 10), clip("c2", 50, 55), clip("c3", 5, 25)];
        assert_eq!(edit_points(&clips), vec![0, 10, 15, 35]);
        // An (invalid) empty clip doesn't produce a duplicate point.
        let clips = [clip("c1", 0, 10), clip("c2", 7, 7), clip("c3", 0, 1)];
        assert_eq!(edit_points(&clips), vec![0, 10, 11]);
    }

    #[test]
    fn contiguity() {
        let clips = five();
        assert!(is_contiguous(&clips, &[cid("c1")]));
        assert!(is_contiguous(&clips, &[cid("c2"), cid("c1"), cid("c3")]));
        assert!(is_contiguous(&clips, &[cid("c1"), cid("c1"), cid("c2")]), "duplicates count once");
        assert!(!is_contiguous(&clips, &[cid("c1"), cid("c3")]));
        assert!(!is_contiguous(&clips, &[cid("c1"), cid("c9")]), "unknown ids aren't on the timeline");
        assert!(!is_contiguous(&clips, &[]), "an empty list is not a run");
        assert!(!is_contiguous(&[], &[cid("c1")]));
    }

    #[test]
    fn split_keeps_the_left_id() {
        let clips = vec![clip("c1", 0, 10), clip("c2", 100, 130)];
        let out = split(&clips, 1, 113, cid("c9")).unwrap();
        assert_eq!(ids(&out), ["c1", "c2", "c9"]);
        assert_eq!((out[1].in_, out[1].out), (100, 113));
        assert_eq!((out[2].in_, out[2].out), (113, 130));
        assert_eq!(out[2].take, clips[1].take);
        assert_eq!(total_frames(&out), total_frames(&clips));
        for f in 0..total_frames(&clips) {
            assert_eq!(source_at(&out, f), source_at(&clips, f), "frame {f}");
        }

        for bad in [100, 130, 99, 131] {
            let err = split(&clips, 1, bad, cid("c9")).unwrap_err();
            assert_eq!(err.code, ErrorCode::Invalid, "cut at {bad}");
        }
        assert_eq!(split(&clips, 2, 5, cid("c9")).unwrap_err().code, ErrorCode::Invalid);
        let err = split(&clips, 1, 113, cid("c1")).unwrap_err();
        assert!(err.message.contains("already on the timeline"), "{}", err.message);
    }

    #[test]
    fn trim_clamps_to_the_take() {
        let clips = vec![clip("c1", 10, 20), clip("c2", 0, 5)];
        let t = trim(&clips, 0, 12, 18, 100).unwrap();
        assert!(!t.clamped);
        assert_eq!((t.clips[0].in_, t.clips[0].out), (12, 18));
        assert_eq!(t.clips[1], clips[1]);

        let t = trim(&clips, 0, -5, 150, 100).unwrap();
        assert!(t.clamped, "both edges were outside the take");
        assert_eq!((t.clips[0].in_, t.clips[0].out), (0, 100));

        for (a, b) in [(20, 20), (30, 20), (-5, 0), (100, 150)] {
            let err = trim(&clips, 0, a, b, 100).unwrap_err();
            assert_eq!(err.code, ErrorCode::Invalid, "[{a}, {b})");
            assert!(err.message.contains("empty"), "{}", err.message);
        }
        assert_eq!(trim(&clips, 5, 0, 1, 100).unwrap_err().code, ErrorCode::Invalid);
        let huge = trim(&clips, 0, 0, i64::MAX, u64::MAX).unwrap();
        assert_eq!(huge.clips[0].out, i64::MAX as u64);
    }

    #[test]
    fn move_block_by_index_and_offset() {
        let clips = five();
        let by = |n: i64, sel: &[&str]| {
            let sel: Vec<ClipId> = sel.iter().map(|s| cid(s)).collect();
            let out = move_block(&clips, &sel, &MoveTo::By(n)).unwrap();
            ids(&out).join(" ")
        };
        assert_eq!(by(1, &["c1"]), "c0 c2 c1 c3 c4");
        assert_eq!(by(-1, &["c1"]), "c1 c0 c2 c3 c4");
        assert_eq!(by(-5, &["c3"]), "c3 c0 c1 c2 c4", "clamped at the start");
        assert_eq!(by(9, &["c1"]), "c0 c2 c3 c4 c1", "clamped at the end");
        assert_eq!(by(i64::MIN, &["c4"]), "c4 c0 c1 c2 c3");
        assert_eq!(by(i64::MAX, &["c0"]), "c1 c2 c3 c4 c0");
        // A block keeps its timeline order whatever order the ids come in.
        assert_eq!(by(1, &["c2", "c1"]), "c0 c3 c1 c2 c4");
        // A scattered selection gathers at its first clip's slot, shifted.
        assert_eq!(by(0, &["c3", "c1"]), "c0 c1 c3 c2 c4");
        assert_eq!(by(-1, &["c1", "c3"]), "c1 c3 c0 c2 c4");

        let to = |i: usize, sel: &[&str]| {
            let sel: Vec<ClipId> = sel.iter().map(|s| cid(s)).collect();
            ids(&move_block(&clips, &sel, &MoveTo::Index(i)).unwrap()).join(" ")
        };
        assert_eq!(to(0, &["c3", "c4"]), "c3 c4 c0 c1 c2");
        assert_eq!(to(2, &["c0"]), "c1 c2 c0 c3 c4", "the index ignores the block");
        assert_eq!(to(99, &["c0"]), "c1 c2 c3 c4 c0", "clamped to the end");
    }

    #[test]
    fn move_block_by_anchor() {
        let clips = five();
        let sel = [cid("c3"), cid("c0")];
        let out = move_block(&clips, &sel, &MoveTo::Before(cid("c2"))).unwrap();
        assert_eq!(ids(&out), ["c1", "c0", "c3", "c2", "c4"]);
        let out = move_block(&clips, &sel, &MoveTo::After(cid("c4"))).unwrap();
        assert_eq!(ids(&out), ["c1", "c2", "c4", "c0", "c3"]);

        let err = move_block(&clips, &sel, &MoveTo::After(cid("c3"))).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        let err = move_block(&clips, &sel, &MoveTo::Before(cid("c9"))).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let err = move_block(&clips, &[cid("c9")], &MoveTo::By(1)).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.message, "no clip c9 on the timeline");
        let err = move_block(&clips, &[], &MoveTo::By(1)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
    }

    #[test]
    fn remove_insert_replace() {
        let clips = five();
        assert_eq!(ids(&remove(&clips, &[cid("c1"), cid("c3"), cid("c9")])), ["c0", "c2", "c4"]);
        assert_eq!(remove(&clips, &[]), clips);

        let new = vec![clip("n1", 0, 1), clip("n2", 1, 2)];
        assert_eq!(ids(&insert(&clips, 0, new.clone())), ["n1", "n2", "c0", "c1", "c2", "c3", "c4"]);
        assert_eq!(ids(&insert(&clips, 2, new.clone())), ["c0", "c1", "n1", "n2", "c2", "c3", "c4"]);
        assert_eq!(ids(&insert(&clips, 99, new.clone())), ["c0", "c1", "c2", "c3", "c4", "n1", "n2"]);
        assert_eq!(ids(&insert(&[], 3, new.clone())), ["n1", "n2"]);

        assert_eq!(ids(&replace_run(&clips, 1, 2, new.clone())), ["c0", "n1", "n2", "c3", "c4"]);
        assert_eq!(ids(&replace_run(&clips, 4, 9, new.clone())), ["c0", "c1", "c2", "c3", "n1", "n2"]);
        assert_eq!(ids(&replace_run(&clips, 9, 1, new.clone())), ["c0", "c1", "c2", "c3", "c4", "n1", "n2"]);
        assert_eq!(ids(&replace_run(&clips, 1, 0, vec![])), ids(&clips));
    }

    #[test]
    fn trim_edges_follow_the_time_rules() {
        // c1 is take frames [100, 130) at timeline 10..40.
        let clips = vec![clip("c0", 0, 10), clip("c1", 100, 130)];
        let edge = |e: Edge, s: &str, tl: bool| trim_edge(&clips, 1, e, &t(s), tl, F30);
        // Clip-relative absolute values count from `in` for both edges.
        assert_eq!(edge(Edge::Start, "5f", false).unwrap(), 105);
        assert_eq!(edge(Edge::End, "20f", false).unwrap(), 120);
        // Relative values move that edge (and ignore `timeline`).
        assert_eq!(edge(Edge::Start, "-200f", false).unwrap(), -100);
        assert_eq!(edge(Edge::End, "+1s", true).unwrap(), 160);
        assert_eq!(edge(Edge::End, "-2f", false).unwrap(), 128);
        // Timeline positions inside the clip, ends included.
        assert_eq!(edge(Edge::Start, "12f", true).unwrap(), 102);
        assert_eq!(edge(Edge::End, "40f", true).unwrap(), 130);
        assert_eq!(edge(Edge::Start, "10f", true).unwrap(), 100);
        let err = edge(Edge::Start, "9f", true).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(err.message, "00:00:00:09 is outside clip c1 (00:00:00:10–00:00:01:10 on the timeline)");
        assert_eq!(edge(Edge::End, "41f", true).unwrap_err().code, ErrorCode::Invalid);
        // Seconds round to the nearest frame; huge values don't wrap.
        assert_eq!(edge(Edge::Start, "0.5s", false).unwrap(), 115);
        let err = edge(Edge::Start, "18446744073709551615f", false).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert_eq!(trim_edge(&clips, 7, Edge::Start, &t("1f"), false, F30).unwrap_err().code, ErrorCode::Invalid);
    }

    #[test]
    fn split_points_follow_the_time_rules() {
        let clips = vec![clip("c0", 0, 10), clip("c1", 100, 130), clip("c2", 0, 1)];
        let at = |i: usize, s: &str, tl: bool| split_point(&clips, i, &t(s), tl, F30);
        assert_eq!(at(1, "13f", false).unwrap(), 113);
        assert_eq!(at(1, "13f", true).unwrap(), 103);
        assert_eq!(at(1, "1f", false).unwrap(), 101);
        assert_eq!(at(1, "29f", false).unwrap(), 129);
        for (s, tl) in [("0f", false), ("30f", false), ("10f", true), ("40f", true), ("99f", true)] {
            assert_eq!(at(1, s, tl).unwrap_err().code, ErrorCode::Invalid, "{s} timeline={tl}");
        }
        assert_eq!(at(1, "+3f", false).unwrap_err().code, ErrorCode::Usage);
        let err = at(2, "0f", false).unwrap_err();
        assert_eq!(err.message, "clip c2 is 1 frame long and can't be split");
    }

    #[test]
    fn absolute_and_delta_frames() {
        assert_eq!(absolute_frame(&t("00:00:01:05"), F30, "seek --to").unwrap(), 35);
        let err = absolute_frame(&t("-1f"), F30, "seek --to").unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert_eq!(err.message, "seek --to takes an absolute time, not the relative -1f");
        assert_eq!(delta_frames(&t("-1s"), F30).unwrap(), -30);
        assert_eq!(delta_frames(&t("+2f"), F30).unwrap(), 2);
        assert_eq!(delta_frames(&t("2f"), F30).unwrap(), 2, "unsigned counts as +");
        assert_eq!(delta_frames(&t("00:00:00:30"), F30).unwrap_err().code, ErrorCode::Usage);
    }
}
