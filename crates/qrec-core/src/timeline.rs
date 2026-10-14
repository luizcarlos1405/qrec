//! Pure timeline operations over `&[Clip]` (A§5.2). Owner: timeline.
//!
//! Positions are prefix sums: clip `i` starts at the sum of the lengths of
//! the clips before it. `position_of`, `total_frames` and `clip_at` are
//! implemented by the foundation because every planner needs them; the
//! rest are signatures only.
#![allow(clippy::todo, unused_variables)]

use crate::error::CoreError;
use crate::ids::{ClipId, Frame};
use crate::model::Clip;
use crate::protocol::command::MoveTo;

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

/// Every clip boundary on the timeline: `0`, each clip's end (ascending, deduplicated).
pub fn edit_points(clips: &[Clip]) -> Vec<Frame> {
    todo!("timeline: edit_points")
}

/// The clips with these ids sit next to each other on the timeline (in any order).
pub fn is_contiguous(clips: &[Clip], ids: &[ClipId]) -> bool {
    todo!("timeline: is_contiguous")
}

/// Splits clip `i` at take frame `take_frame` (`in < take_frame < out`):
/// the left part keeps the id, the right part gets `new_id`. Property: the
/// timeline frame → (take, take frame) mapping and the export/GES plans
/// don't change.
pub fn split(
    clips: &[Clip],
    i: usize,
    take_frame: Frame,
    new_id: ClipId,
) -> Result<Vec<Clip>, CoreError> {
    todo!("timeline: split")
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
    todo!("timeline: trim")
}

/// Moves the clips `ids` as one block in their original order. Property:
/// the result is a permutation.
pub fn move_block(clips: &[Clip], ids: &[ClipId], to: &MoveTo) -> Result<Vec<Clip>, CoreError> {
    todo!("timeline: move_block")
}

/// The timeline without these clips.
pub fn remove(clips: &[Clip], ids: &[ClipId]) -> Vec<Clip> {
    todo!("timeline: remove")
}

/// Inserts `new` before index `index` (clamped to the end).
pub fn insert(clips: &[Clip], index: usize, new: Vec<Clip>) -> Vec<Clip> {
    todo!("timeline: insert")
}

/// Replaces `len` clips starting at `start` with `with` (record over, autocut).
pub fn replace_run(clips: &[Clip], start: usize, len: usize, with: Vec<Clip>) -> Vec<Clip> {
    todo!("timeline: replace_run")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TakeId;

    fn clip(id: &str, in_: Frame, out: Frame) -> Clip {
        Clip {
            id: ClipId::from_raw(id),
            take: TakeId::numbered(1),
            in_,
            out,
        }
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
    }
}
