//! The bin's selection (D-37) and the bin preview's end (D-17), GTK-free.
//!
//! - Click selects one take, `Ctrl`+click toggles one, `Shift`+click
//!   extends from the anchor (the last plain or `Ctrl` click) in row order;
//!   `↑` `↓` move it (one take), `Shift`+`↑` `↓` extend it. Rows that leave
//!   the bin leave the selection; the cursor goes to the row now at its
//!   place, selected when the row that left had the keyboard and was the one
//!   selected (`Enter` inserted it: the keyboard goes on from the next).
//! - A bin preview (GUI-local, no command) ends with `Esc`, when the bin
//!   closes, and on any timeline action: the timeline, the clip selection
//!   or the playhead changes, playback starts, the canvas goes Live, a take
//!   starts recording, or its take leaves the bin. `Space` pauses the
//!   selected take's preview or plays it again, and previews the selected
//!   take otherwise ([`space`]).

use qrec_core::TakeId;
use qrec_core::state::{CanvasMode, State};

/// The bin's selection: what is selected (in row order), where a `Shift`
/// range starts and where the keyboard is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub selected: Vec<TakeId>,
    pub anchor: Option<TakeId>,
    pub cursor: Option<TakeId>,
}

impl Selection {
    /// A click on `take` (`ctrl`, `shift` held).
    pub fn click(&mut self, rows: &[TakeId], take: &TakeId, ctrl: bool, shift: bool) {
        if !rows.contains(take) {
            return;
        }
        if shift {
            let anchor = self
                .anchor
                .clone()
                .filter(|a| rows.contains(a))
                .unwrap_or_else(|| take.clone());
            self.selected = range(rows, &anchor, take);
            self.anchor = Some(anchor);
        } else if ctrl {
            if let Some(at) = self.selected.iter().position(|t| t == take) {
                self.selected.remove(at);
            } else {
                self.selected.push(take.clone());
                self.selected = in_order(rows, &self.selected);
            }
            self.anchor = Some(take.clone());
        } else {
            self.selected = vec![take.clone()];
            self.anchor = Some(take.clone());
        }
        self.cursor = Some(take.clone());
    }

    /// `↑` (`down` false) or `↓`; `extend` with `Shift`.
    pub fn step(&mut self, rows: &[TakeId], down: bool, extend: bool) {
        if rows.is_empty() {
            return;
        }
        let at = self
            .cursor
            .as_ref()
            .and_then(|c| rows.iter().position(|t| t == c));
        let next = match at {
            None if down => 0,
            None => rows.len() - 1,
            Some(i) if down => (i + 1).min(rows.len() - 1),
            Some(i) => i.saturating_sub(1),
        };
        let take = rows[next].clone();
        if extend {
            let anchor = self
                .anchor
                .clone()
                .filter(|a| rows.contains(a))
                .unwrap_or_else(|| take.clone());
            self.selected = range(rows, &anchor, &take);
            self.anchor = Some(anchor);
        } else {
            self.selected = vec![take.clone()];
            self.anchor = Some(take.clone());
        }
        self.cursor = Some(take);
    }

    /// Keeps only the takes still in `rows` (`before`: the rows as they
    /// were). A cursor whose take left goes to the row now at its place
    /// ([`place`]); `keyboard` (the bin's keyboard focus was on that row,
    /// `Enter` inserted its take): when it was the one take selected, that
    /// row is selected instead, so the keyboard goes on from there.
    pub fn retain(&mut self, before: &[TakeId], rows: &[TakeId], keyboard: bool) {
        let single = self.single() == self.cursor.as_ref() && self.cursor.is_some();
        self.selected.retain(|t| rows.contains(t));
        if self.anchor.as_ref().is_some_and(|a| !rows.contains(a)) {
            self.anchor = None;
        }
        let Some(left) = self.cursor.clone().filter(|c| !rows.contains(c)) else {
            return;
        };
        self.cursor = place(before, rows, &left);
        if keyboard
            && single
            && self.selected.is_empty()
            && let Some(next) = self.cursor.clone()
        {
            self.selected = vec![next.clone()];
            self.anchor = Some(next);
        }
    }

    /// The one selected take, if exactly one is.
    pub fn single(&self) -> Option<&TakeId> {
        match self.selected.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }
}

/// The row now at the place of `take`, which left the rows (`before` them,
/// `rows` now): the first row after it that stays, else the last row.
pub fn place(before: &[TakeId], rows: &[TakeId], take: &TakeId) -> Option<TakeId> {
    before
        .iter()
        .skip_while(|t| *t != take)
        .find(|t| rows.contains(t))
        .or_else(|| rows.last())
        .cloned()
}

/// The rows from `a` to `b`, both included, in row order.
fn range(rows: &[TakeId], a: &TakeId, b: &TakeId) -> Vec<TakeId> {
    let (Some(i), Some(j)) = (
        rows.iter().position(|t| t == a),
        rows.iter().position(|t| t == b),
    ) else {
        return vec![b.clone()];
    };
    let (lo, hi) = (i.min(j), i.max(j));
    rows[lo..=hi].to_vec()
}

fn in_order(rows: &[TakeId], takes: &[TakeId]) -> Vec<TakeId> {
    rows.iter().filter(|t| takes.contains(t)).cloned().collect()
}

/// What `Space` in the bin does to its preview (D-17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceStep {
    /// The selected take's preview pauses, or plays again.
    Toggle,
    /// The selected take is previewed (instead of another take's preview).
    Start(TakeId),
    /// The preview ends: no one take is selected.
    End,
    Nothing,
}

/// `Space` in the bin, `preview` the take previewed now and `single` the
/// one selected take (D-17): the selected take's preview pauses or plays
/// again (it doesn't start over, and Space stays the bin's: it never plays
/// the timeline), another take's gives way to it; with no one take
/// selected a preview ends.
pub fn space(preview: Option<&TakeId>, single: Option<&TakeId>) -> SpaceStep {
    match (preview, single) {
        (Some(previewed), Some(take)) if previewed == take => SpaceStep::Toggle,
        (_, Some(take)) => SpaceStep::Start(take.clone()),
        (Some(_), None) => SpaceStep::End,
        (None, None) => SpaceStep::Nothing,
    }
}

/// Whether a bin preview of `take` ends with the snapshot `next` after
/// `prev` (D-17): the bin closed, a timeline action, a recording, or the
/// take left the bin. While `parking` (it took the player from the playing
/// program, whose park at the frame the player showed is on its way,
/// G§3.10 step 8), a playhead change is the program's own, not a timeline
/// action: the park's, or a once-a-second seek still on its way.
pub fn preview_ends(
    prev: &State,
    next: &State,
    take: &TakeId,
    in_bin: bool,
    parking: bool,
) -> bool {
    !next.ui.bin_open
        || !in_bin
        || prev.project.timeline != next.project.timeline
        || prev.ui.selection != next.ui.selection
        || (prev.ui.playhead != next.ui.playhead && !parking)
        || (!prev.runtime.playing && next.runtime.playing)
        || (prev.ui.canvas_mode == CanvasMode::Program && next.ui.canvas_mode == CanvasMode::Live)
        || !next.runtime.record.is_idle()
        || !next.project.takes.contains_key(take)
}

#[cfg(test)]
mod tests {
    use qrec_core::{ClipId, testkit};

    use super::*;

    fn t(n: u32) -> TakeId {
        TakeId::numbered(n)
    }

    /// The fixture's rows, newest first.
    fn rows() -> Vec<TakeId> {
        [9, 7, 6, 4, 2].map(t).to_vec()
    }

    #[test]
    fn clicks_select_toggle_and_extend() {
        let rows = rows();
        let mut s = Selection::default();
        s.click(&rows, &t(4), false, false);
        assert_eq!(s.selected, [t(4)]);
        // S18: take 2, then Ctrl+click take 4.
        s.click(&rows, &t(2), false, false);
        s.click(&rows, &t(4), true, false);
        assert_eq!(s.selected, [t(4), t(2)], "row order");
        s.click(&rows, &t(4), true, false);
        assert_eq!(s.selected, [t(2)], "Ctrl+click again unselects");
        // Shift extends from the anchor (take 4, the last Ctrl click).
        s.click(&rows, &t(7), false, true);
        assert_eq!(s.selected, [t(7), t(6), t(4)]);
        s.click(&rows, &t(9), false, false);
        s.click(&rows, &t(6), false, true);
        assert_eq!(s.selected, [t(9), t(7), t(6)]);
        assert_eq!(s.single(), None);
        // A take that isn't a row does nothing.
        s.click(&rows, &t(5), false, false);
        assert_eq!(s.selected, [t(9), t(7), t(6)]);
    }

    #[test]
    fn arrows_move_and_extend() {
        let rows = rows();
        let mut s = Selection::default();
        s.step(&rows, true, false);
        assert_eq!(s.selected, [t(9)], "↓ from nothing: the first row");
        s.step(&rows, true, false);
        s.step(&rows, true, false);
        assert_eq!(s.selected, [t(6)]);
        s.step(&rows, true, true);
        s.step(&rows, true, true);
        assert_eq!(s.selected, [t(6), t(4), t(2)]);
        s.step(&rows, true, true);
        assert_eq!(s.cursor, Some(t(2)), "the last row stays");
        s.step(&rows, false, false);
        assert_eq!(s.selected, [t(4)]);
        let mut up = Selection::default();
        up.step(&rows, false, false);
        assert_eq!(up.selected, [t(2)], "↑ from nothing: the last row");
        up.step(&[], true, false);
        assert_eq!(up.selected, [t(2)]);
    }

    #[test]
    fn purged_takes_leave_the_selection() {
        let mut s = Selection::default();
        s.click(&rows(), &t(2), false, false);
        s.click(&rows(), &t(4), true, false);
        s.retain(&rows(), &[9, 7, 6].map(t), false);
        assert!(s.selected.is_empty());
        assert_eq!(s.anchor, None);
        // The cursor goes where the purged rows were: the last row now.
        assert_eq!(s.cursor, Some(t(6)));
        let mut none = Selection::default();
        none.click(&rows(), &t(4), false, false);
        none.retain(&rows(), &[], true);
        assert_eq!(none, Selection::default(), "an empty bin");
    }

    /// `Enter` inserted take 4 (S14: it leaves the bin): the keyboard goes on
    /// from the row now at its place, take 2, selected (`↓` stays there, `↑`
    /// goes to take 6), so `Enter`, `↓`, `Enter` inserts the next takes. The
    /// cursor and the selection went (`↓` then jumped to take 9).
    #[test]
    fn the_keyboard_goes_on_from_the_row_that_left() {
        let before = rows();
        let after = [9, 7, 6, 2].map(t);
        let mut s = Selection::default();
        s.click(&before, &t(4), false, false);
        s.retain(&before, &after, true);
        assert_eq!(s.selected, [t(2)]);
        assert_eq!(s.cursor, Some(t(2)));
        let mut down = s.clone();
        down.step(&after, true, false);
        assert_eq!(down.selected, [t(2)], "the last row stays");
        let mut up = s.clone();
        up.step(&after, false, false);
        assert_eq!(up.selected, [t(6)]);
        // The last row left: the row before it.
        let mut last = Selection::default();
        last.click(&before, &t(2), false, false);
        last.retain(&before, &[9, 7, 6, 4].map(t), true);
        assert_eq!(
            (last.selected.clone(), last.cursor),
            (vec![t(4)], Some(t(4)))
        );
        // A take landed at the top meanwhile: still the row after it.
        let mut landed = Selection::default();
        landed.click(&before, &t(6), false, false);
        landed.retain(&before, &[10, 9, 7, 4, 2].map(t), true);
        assert_eq!(landed.selected, [t(4)]);
        // Without the keyboard there (a CLI edit, a drop with the focus on
        // the timeline) the take only leaves the selection.
        let mut away = Selection::default();
        away.click(&before, &t(4), false, false);
        away.retain(&before, &after, false);
        assert!(away.selected.is_empty());
        assert_eq!(away.cursor, Some(t(2)));
        // Two selected: they leave it, nothing else is selected (S18).
        let mut two = Selection::default();
        two.click(&before, &t(2), false, false);
        two.click(&before, &t(4), true, false);
        two.retain(&before, &[9, 7, 6, 2].map(t), true);
        assert_eq!(two.selected, [t(2)]);
        // A row that stays keeps everything as it was.
        let mut kept = Selection::default();
        kept.click(&before, &t(7), false, false);
        let was = kept.clone();
        kept.retain(&before, &after, true);
        assert_eq!(kept, was);
    }

    /// D-17's `Space`: the selected take's preview pauses (and plays again),
    /// it doesn't start over; another take's preview, or none, gives way to
    /// the selected take's; with no one take selected a preview ends.
    #[test]
    fn space_pauses_the_selected_takes_preview() {
        let (four, two) = (t(4), t(2));
        assert_eq!(space(Some(&four), Some(&four)), SpaceStep::Toggle);
        assert_eq!(space(None, Some(&four)), SpaceStep::Start(four.clone()));
        assert_eq!(
            space(Some(&two), Some(&four)),
            SpaceStep::Start(four.clone()),
            "another take's preview"
        );
        assert_eq!(space(Some(&four), None), SpaceStep::End);
        assert_eq!(space(None, None), SpaceStep::Nothing);
    }

    /// D-17: a timeline action, the bin closing, a recording or the take
    /// leaving end a bin preview; its own player events don't.
    #[test]
    fn what_ends_a_bin_preview() {
        let mut prev = testkit::design_state();
        prev.ui.bin_open = true;
        prev.ui.canvas_mode = CanvasMode::Program;
        let take = t(4);
        assert!(!preview_ends(&prev, &prev, &take, true, false));
        let ends = |f: &dyn Fn(&mut State)| {
            let mut next = prev.clone();
            f(&mut next);
            preview_ends(&prev, &next, &take, true, false)
        };
        assert!(ends(&|s| s.ui.bin_open = false));
        assert!(ends(&|s| s.ui.playhead = 10));
        assert!(ends(&|s| s.ui.selection = vec![ClipId::from_raw("c2d0")]));
        assert!(ends(&|s| s.runtime.playing = true));
        assert!(ends(&|s| s.ui.canvas_mode = CanvasMode::Live));
        assert!(ends(&|s| {
            s.project.timeline.clips.pop();
        }));
        assert!(ends(&|s| {
            s.project.takes.remove(&TakeId::numbered(4));
        }));
        assert!(!ends(&|s| s.ui.zoom = Some(2.0)), "a zoom isn't an edit");
        assert!(
            preview_ends(&prev, &prev, &take, false, false),
            "it left the bin"
        );
        // The park of the program it took the player from (G§3.10 step 8):
        // the playhead moves, the preview stays; anything else still ends it.
        let parking = |f: &dyn Fn(&mut State)| {
            let mut next = prev.clone();
            f(&mut next);
            preview_ends(&prev, &next, &take, true, true)
        };
        assert!(!parking(&|s| s.ui.playhead = 2455), "the park's own");
        assert!(parking(&|s| s.ui.bin_open = false));
        assert!(parking(&|s| s.ui.selection = vec![ClipId::from_raw("c2d0")]));
    }
}
