//! The bin's selection (D-37) and the bin preview's end (D-17), GTK-free.
//!
//! - Click selects one take, `Ctrl`+click toggles one, `Shift`+click
//!   extends from the anchor (the last plain or `Ctrl` click) in row order;
//!   `↑` `↓` move it (one take), `Shift`+`↑` `↓` extend it. Rows that leave
//!   the bin leave the selection.
//! - A bin preview (GUI-local, no command) ends with `Esc`, when the bin
//!   closes, and on any timeline action: the timeline, the clip selection
//!   or the playhead changes, playback starts, the canvas goes Live, a take
//!   starts recording, or its take leaves the bin.

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

    /// Keeps only the takes still in `rows`.
    pub fn retain(&mut self, rows: &[TakeId]) {
        self.selected.retain(|t| rows.contains(t));
        if self.anchor.as_ref().is_some_and(|a| !rows.contains(a)) {
            self.anchor = None;
        }
        if self.cursor.as_ref().is_some_and(|c| !rows.contains(c)) {
            self.cursor = self.selected.last().cloned();
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

/// Whether a bin preview of `take` ends with the snapshot `next` after
/// `prev` (D-17): the bin closed, a timeline action, a recording, or the
/// take left the bin.
pub fn preview_ends(prev: &State, next: &State, take: &TakeId, in_bin: bool) -> bool {
    !next.ui.bin_open
        || !in_bin
        || prev.project.timeline != next.project.timeline
        || prev.ui.selection != next.ui.selection
        || prev.ui.playhead != next.ui.playhead
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
        s.retain(&[9, 7, 6].map(t));
        assert!(s.selected.is_empty());
        assert_eq!((s.anchor.clone(), s.cursor.clone()), (None, None));
    }

    /// D-17: a timeline action, the bin closing, a recording or the take
    /// leaving end a bin preview; its own player events don't.
    #[test]
    fn what_ends_a_bin_preview() {
        let mut prev = testkit::design_state();
        prev.ui.bin_open = true;
        prev.ui.canvas_mode = CanvasMode::Program;
        let take = t(4);
        assert!(!preview_ends(&prev, &prev, &take, true));
        let ends = |f: &dyn Fn(&mut State)| {
            let mut next = prev.clone();
            f(&mut next);
            preview_ends(&prev, &next, &take, true)
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
        assert!(preview_ends(&prev, &prev, &take, false), "it left the bin");
    }
}
