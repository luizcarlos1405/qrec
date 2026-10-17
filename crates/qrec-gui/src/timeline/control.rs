//! The timeline's GTK-free logic (G§8.2), tested without a display:
//!
//! - [`ViewModel`]: the zoom and scroll as shown. Local changes (wheel,
//!   pan, zoom steps, the slider) show at once and go out as one
//!   `SetUi{zoom, scroll_px}` 300 ms after the last one (D-39, G§3.6); a
//!   snapshot that shows a value the timeline didn't send is adopted (the
//!   echo rule); zoom changes ease over 200 ms in log(px/frame), keeping
//!   the anchor's x ([`ZoomAnim`]), instant under reduced motion and in
//!   test mode.
//! - [`Gestures`]: presses, drags and releases on the ruler and the rows
//!   as `decide_*` transitions over a [`Sink`]: D-37's selection, the move
//!   and trim drags (one undoable command on release, none on `Esc`,
//!   D-41, D-42), the ruler scrub (at most 10 `Seek`s a second plus one on
//!   release, D-40), the middle-drag pan (D-39), bin drops (D-38).

use std::time::Duration;

use std::collections::{BTreeMap, BTreeSet};

use qrec_core::ctx::FileFact;
use qrec_core::model::AutocutSettings;
use qrec_core::protocol::command::{Nullable, SeekTarget, SelectOp, UiPatch};
use qrec_core::protocol::event::Interval;
use qrec_core::state::{RecordPhase, SilenceKey};
use qrec_core::view::DragKind;
use qrec_core::view::notices::Notice;
use qrec_core::view::timeline::{
    self as tl, ClipBox, Hit, MOVE_START_PX, Rows, TrimEdge, Viewport, edits_enabled, is_fit,
};
use qrec_core::{ClipId, Command, Frame, State, TakeId, TakeStatus, TimeArg};

use crate::theme::motion::EASE_OUT_EXPO;

/// `SetUi{zoom, scroll_px}` goes out this long after the last change.
pub const FLUSH_AFTER: Duration = Duration::from_millis(300);
/// A scrub seeks at most this often (D-40).
pub const SCRUB_EVERY: Duration = Duration::from_millis(100);
/// Zoom changes ease over this long (DESIGN Motion).
pub const ZOOM_EASE: Duration = Duration::from_millis(200);
/// A move drag's neighbours close the gap over this long (G§4.5).
pub const GAP_EASE: Duration = Duration::from_millis(120);
/// The ghost grows on the frame clock at most this far past the last
/// record clock (G§4.5).
pub const GHOST_LEAD: Duration = Duration::from_millis(300);

/// What the timeline's logic asks of the window (the GTK glue; a recorder
/// in tests).
pub trait Sink {
    /// `Store::dispatch` (the default reply policy).
    fn dispatch(&mut self, cmd: Command);
    /// `LocalUi.drag`.
    fn set_drag(&mut self, drag: Option<DragKind>);
    /// `LocalUi.trim_frame` (the stage shows it, S10).
    fn set_trim_frame(&mut self, frame: Option<(TakeId, Frame)>);
    /// `LocalUi.hover_clip`.
    fn set_hover(&mut self, clip: Option<ClipId>);
    /// The player follows a scrub (`ProgramPlayer::scrub`, D-40).
    fn scrub(&mut self, frame: Frame);
    /// A toast (D-34).
    fn notice(&mut self, notice: Notice);
    /// The lanes follow a middle drag (D-25, D-39): their offset is the
    /// press's minus `dy`, the pointer's travel since the press in the
    /// lanes' window (100 % px; up is negative).
    fn pan_lanes(&mut self, dy: f64);
}

// ------------------------------------------------------------- the view

/// The zoom and scroll the way `ui.json` keeps them: `zoom: None` is fit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomTarget {
    pub zoom: Option<f64>,
    pub scroll: f64,
}

impl ZoomTarget {
    /// The state's.
    pub fn of(state: &State) -> Self {
        Self {
            zoom: state.ui.zoom,
            scroll: state.ui.scroll_px,
        }
    }

    fn same(&self, other: &ZoomTarget) -> bool {
        let zoom = match (self.zoom, other.zoom) {
            (None, None) => true,
            (Some(a), Some(b)) => (a - b).abs() <= a.abs() * 1e-12,
            _ => false,
        };
        zoom && (self.zoom.is_none() || (self.scroll - other.scroll).abs() < 0.01)
    }
}

/// A zoom or scroll easing from one viewport to another (200 ms
/// `EASE_OUT_EXPO`, log(px/frame); the point both viewports put at the same
/// x stays there).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomAnim {
    pub from: Viewport,
    pub to: Viewport,
    pub start: Duration,
    pub length: Duration,
}

impl ZoomAnim {
    /// The viewport at `now`.
    pub fn at(&self, now: Duration) -> Viewport {
        let t = if self.length.is_zero() {
            1.0
        } else {
            now.saturating_sub(self.start).as_secs_f64() / self.length.as_secs_f64()
        };
        if t >= 1.0 {
            return self.to;
        }
        let e = EASE_OUT_EXPO.at(t);
        let (p0, p1) = (self.from.px_per_frame, self.to.px_per_frame);
        let (s0, s1) = (self.from.scroll_px, self.to.scroll_px);
        let ppf = (p0.ln() + (p1.ln() - p0.ln()) * e).exp();
        let lerp = s0 + (s1 - s0) * e;
        let scroll = if (p0 - p1).abs() > p0 * 1e-9 {
            // The fixed point of the two viewports: t·p0 − s0 = t·p1 − s1.
            let anchor = (s0 - s1) / (p0 - p1);
            let x = anchor * p0 - s0;
            if (-self.from.width..=2.0 * self.from.width).contains(&x) {
                anchor * ppf - x
            } else {
                lerp
            }
        } else {
            lerp
        };
        Viewport {
            px_per_frame: ppf,
            scroll_px: scroll.max(0.0),
            ..self.to
        }
    }

    pub fn done(&self, now: Duration) -> bool {
        now.saturating_sub(self.start) >= self.length
    }
}

/// The zoom and scroll as shown (module docs).
#[derive(Debug, Clone, Default)]
pub struct ViewModel {
    width: f64,
    total: Frame,
    /// The view as the timeline wants it; `None` before the first state.
    target: Option<ZoomTarget>,
    /// The last snapshot's zoom and scroll (to see what changed).
    state_ui: Option<ZoomTarget>,
    /// What the timeline sent last.
    sent: Option<ZoomTarget>,
    anim: Option<ZoomAnim>,
    flush_at: Option<Duration>,
    /// No easing (reduced motion, test mode).
    instant: bool,
}

impl ViewModel {
    pub fn new(instant: bool) -> Self {
        Self {
            instant,
            ..Self::default()
        }
    }

    /// Reduced motion changed.
    pub fn set_instant(&mut self, instant: bool) {
        self.instant = instant;
        if instant {
            self.anim = None;
        }
    }

    /// The time area's width (100 % px). Resizing never eases.
    pub fn set_width(&mut self, width: f64) {
        if (width - self.width).abs() > f64::EPSILON {
            self.width = width;
            self.anim = None;
        }
    }

    pub fn width(&self) -> f64 {
        self.width
    }

    /// The frames shown (the timeline and the ghost).
    pub fn total(&self) -> Frame {
        self.total
    }

    /// Fit's px per frame now; `None` on an empty timeline.
    pub fn fit(&self) -> Option<f64> {
        Viewport::fit_px_per_frame(self.width, self.total)
    }

    /// The view the target describes at the current width and total.
    pub fn target_view(&self) -> Option<Viewport> {
        let target = self.target?;
        let ui = qrec_core::UiState {
            zoom: target.zoom,
            scroll_px: target.scroll,
            ..qrec_core::UiState::default()
        };
        Viewport::from_ui(&ui, self.width, self.total)
    }

    /// The view as drawn at `now` (eased toward the target).
    pub fn shown(&self, now: Duration) -> Option<Viewport> {
        let target = self.target_view()?;
        match &self.anim {
            Some(anim) if !anim.done(now) => Some(anim.at(now)),
            _ => Some(target),
        }
    }

    /// An easing runs at `now`.
    pub fn animating(&self, now: Duration) -> bool {
        self.anim.as_ref().is_some_and(|a| !a.done(now))
    }

    /// A `SetUi` is waiting to go out.
    pub fn dirty(&self) -> bool {
        self.flush_at.is_some()
    }

    /// When the waiting `SetUi` goes out.
    pub fn flush_at(&self) -> Option<Duration> {
        self.flush_at
    }

    /// A snapshot, with `total` frames shown: adopts a zoom or scroll the
    /// timeline didn't send (the echo rule) and eases to a view that moved
    /// (fit follows the timeline's length: an edit, a growing ghost).
    pub fn on_state(&mut self, state: &State, total: Frame, now: Duration) {
        let ui = ZoomTarget::of(state);
        let shown_before = self.shown(now);
        let view_before = self.target_view();
        self.total = total;
        match (self.target, self.state_ui) {
            (None, _) | (_, None) => self.target = Some(ui),
            (Some(_), Some(prev)) => {
                let changed = !ui.same(&prev);
                let ours = self.sent.is_some_and(|s| s.same(&ui));
                if changed && !ours {
                    // Someone else's (the CLI): theirs wins.
                    self.target = Some(ui);
                    self.flush_at = None;
                }
                if ours {
                    // Its echo came: a later snapshot back at this value is
                    // someone else's.
                    self.sent = None;
                }
            }
        }
        self.state_ui = Some(ui);
        let view_after = self.target_view();
        if view_after != view_before {
            self.ease_from(shown_before, now);
        }
    }

    /// A local change to `view` (it shows at once, or eases when `ease`);
    /// the `SetUi` goes out 300 ms after the last one.
    pub fn set_view(&mut self, view: Viewport, now: Duration, ease: bool) {
        let Some(fit) = self.fit() else {
            return;
        };
        let shown_before = self.shown(now);
        self.target = Some(if is_fit(view.px_per_frame, fit) {
            ZoomTarget {
                zoom: None,
                scroll: 0.0,
            }
        } else {
            ZoomTarget {
                zoom: Some(view.px_per_frame),
                scroll: view.scroll_px,
            }
        });
        if ease {
            self.ease_from(shown_before, now);
        } else {
            self.anim = None;
        }
        self.flush_at = Some(now + FLUSH_AFTER);
    }

    fn ease_from(&mut self, from: Option<Viewport>, now: Duration) {
        let (Some(from), Some(to)) = (from, self.target_view()) else {
            self.anim = None;
            return;
        };
        self.anim = (!self.instant && from != to).then_some(ZoomAnim {
            from,
            to,
            start: now,
            length: ZOOM_EASE,
        });
    }

    /// The `SetUi` with what changed since the state (`None` when nothing
    /// did), once its 300 ms passed (`now`).
    pub fn flush(&mut self, state: &State, now: Duration) -> Option<Command> {
        if self.flush_at.is_none_or(|at| now < at) {
            return None;
        }
        self.flush_now(state)
    }

    /// The waiting `SetUi` now (its timer fired: GLib's millisecond timers
    /// may fire a little before the exact deadline).
    pub fn flush_now(&mut self, state: &State) -> Option<Command> {
        self.flush_at?;
        self.flush_at = None;
        let target = self.target?;
        let ui = ZoomTarget::of(state);
        let mut patch = UiPatch::default();
        let zoom_differs = match (target.zoom, ui.zoom) {
            (None, None) => false,
            (Some(a), Some(b)) => (a - b).abs() > a.abs() * 1e-12,
            _ => true,
        };
        if zoom_differs {
            patch.zoom = match target.zoom {
                None => Nullable::Clear,
                Some(z) => Nullable::Set(z),
            };
        }
        if (target.scroll - ui.scroll).abs() >= 0.01 {
            patch.scroll_px = Some(target.scroll);
        }
        if patch == UiPatch::default() {
            return None;
        }
        self.sent = Some(target);
        Some(Command::SetUi { patch })
    }

    // ------------------------------------------- the zoom actions

    /// `=` / `−` (anchored at `playhead` when it shows).
    pub fn zoom_step(&mut self, zoom_in: bool, playhead: Frame, now: Duration) {
        let (Some(view), Some(fit)) = (self.target_view(), self.fit()) else {
            return;
        };
        let next = tl::zoom_step(&view, fit, playhead, zoom_in);
        if next != view {
            self.set_view(next, now, true);
        }
    }

    /// `\` and the toolbar's Fit (the timeline's `zoom.fit`, G§3.8 step
    /// 10): fit at once, easing, in place of a local zoom still waiting;
    /// the `SetUi` clears the state's zoom unless it is at fit. `false`
    /// without a view (an empty timeline).
    pub fn zoom_fit(&mut self, now: Duration) -> bool {
        let (Some(view), Some(fit)) = (self.target_view(), self.fit()) else {
            return false;
        };
        self.set_view(
            Viewport {
                px_per_frame: fit,
                scroll_px: 0.0,
                ..view
            },
            now,
            true,
        );
        true
    }

    /// `Ctrl`+wheel: ×1.25 per notch at `anchor_x`.
    pub fn zoom_wheel(&mut self, notches: f64, anchor_x: f64, now: Duration) {
        let (Some(view), Some(fit)) = (self.target_view(), self.fit()) else {
            return;
        };
        let next = view.zoom_at(tl::WHEEL_ZOOM.powf(-notches), anchor_x, fit);
        if next != view {
            self.set_view(next, now, true);
        }
    }

    /// `z`: the selection fills the time area; `Err` with nothing selected.
    pub fn zoom_selection(&mut self, state: &State, now: Duration) -> Result<(), String> {
        let Some(view) = self.target_view() else {
            return Err(tl::ZOOM_TO_NOTHING.to_owned());
        };
        let next = tl::zoom_to_selection(state, &view, self.total)?;
        self.set_view(next, now, true);
        Ok(())
    }

    /// The slider at `s` (anchored like `=`).
    pub fn zoom_slider(&mut self, s: f64, playhead: Frame, now: Duration) {
        let (Some(view), Some(fit)) = (self.target_view(), self.fit()) else {
            return;
        };
        let px = view.x_of_frame(playhead);
        let anchor = if (0.0..=view.width).contains(&px) {
            px
        } else {
            view.width / 2.0
        };
        let next = view.with_zoom(tl::slider_zoom(s, fit), anchor, fit);
        if next != view {
            self.set_view(next, now, false);
        }
    }

    /// A wheel or touchpad scroll (in notches, D-39).
    pub fn scroll_wheel(&mut self, dx: f64, dy: f64, now: Duration) {
        let Some(view) = self.target_view() else {
            return;
        };
        let scroll = view.scroll_for_wheel(dx, dy, self.total);
        self.scroll_to(scroll, now);
    }

    /// The frames shown grew on the frame clock: a recording's ghost
    /// between record clocks (G§4.5). The view follows at once, never
    /// easing (an ease in flight lands on it), so at fit the ghost's end
    /// and the playhead parked there stay inside the 8 px inset.
    pub fn follow_ghost(&mut self, total: Frame) {
        if total <= self.total {
            return;
        }
        self.total = total;
        let to = self.target_view();
        if let (Some(anim), Some(to)) = (self.anim.as_mut(), to) {
            anim.to = to;
        }
    }

    /// Scrolls to `scroll` px (a pan, auto-page).
    pub fn scroll_to(&mut self, scroll: f64, now: Duration) {
        let Some(view) = self.target_view() else {
            return;
        };
        let next = Viewport {
            scroll_px: scroll,
            ..view
        }
        .clamped(self.total);
        if (next.scroll_px - view.scroll_px).abs() > f64::EPSILON {
            self.set_view(next, now, false);
        }
    }
}

// ------------------------------------------------------------- gestures

/// Modifiers held at a press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// Where a pointer event is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// The ruler's time part.
    Ruler,
    /// The rows (video and lanes) of the time area.
    Rows,
}

/// Which button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
}

/// A press: where, which button, the modifiers held.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Press {
    pub area: Area,
    pub button: Button,
    /// Time-area px; `y` from the top of the rows (the ruler's: any).
    pub x: f64,
    pub y: f64,
    pub mods: Mods,
}

/// What the gestures read about the timeline at an event.
#[derive(Debug, Clone, Copy)]
pub struct Cx<'a> {
    pub state: &'a State,
    /// The view as shown.
    pub view: Viewport,
    /// The frames shown (the timeline and the ghost).
    pub total: Frame,
    pub rows: Rows,
    pub lanes: usize,
    /// The playhead as drawn (snapping, D-40).
    pub playhead: Frame,
    pub now: Duration,
    /// The rows' top in the lanes' scrolled window as the canvas is laid
    /// out for this event: 0, or above it by the lanes' scroll (D-25). An
    /// event's y plus it is where the pointer is in the window, which a
    /// vertical pan doesn't move.
    pub rows_top: f64,
}

impl Cx<'_> {
    fn boxes(&self) -> Vec<ClipBox> {
        tl::clip_boxes(self.state, &self.view, None)
    }

    /// What a press at (`x`, `y`) in the rows hits: a clip column, or an
    /// empty row below the lanes or past the clips.
    pub fn hit(&self, x: f64, y: f64) -> Hit {
        if y < 0.0 || y >= self.rows.content_height(self.lanes) {
            return Hit::Empty;
        }
        tl::hit(&self.boxes(), x)
    }
}

/// A gesture in progress.
#[derive(Debug, Clone, PartialEq)]
pub enum Drag {
    /// Pressed on a clip's body: a move once it travels 4 px.
    Pending {
        clip: ClipId,
        x0: f64,
        was_selected: bool,
        can_move: bool,
    },
    /// Moving `block` with the pointer at `x` (S11).
    Move { block: Vec<ClipId>, x: f64 },
    /// Trimming `clip`'s `edge`, dragged from `x0` to `x` (S10).
    Trim {
        clip: ClipId,
        edge: TrimEdge,
        x0: f64,
        x: f64,
        started: bool,
    },
    /// Scrubbing the ruler (D-40).
    Scrub {
        frame: Frame,
        last_seek: Duration,
        seeked: Frame,
    },
    /// Panning with the middle button (D-39): from the press's x and its
    /// y in the lanes' window (`Cx::rows_top`), which the pan doesn't
    /// move, and the scroll then.
    Pan { x0: f64, y0: f64, scroll0: f64 },
}

/// The timeline's gestures (module docs).
#[derive(Debug, Clone, Default)]
pub struct Gestures {
    drag: Option<Drag>,
    hover: Option<ClipId>,
}

impl Gestures {
    pub fn new() -> Self {
        Self::default()
    }

    /// The gesture in progress.
    pub fn drag(&self) -> Option<&Drag> {
        self.drag.as_ref()
    }

    /// The frame a scrub shows (the drawn playhead leads the snapshot).
    pub fn scrub_frame(&self) -> Option<Frame> {
        match &self.drag {
            Some(Drag::Scrub { frame, .. }) => Some(*frame),
            _ => None,
        }
    }

    /// A press (D-37, D-40, D-45). Returns whether it was used.
    pub fn decide_press(
        &mut self,
        cx: &Cx<'_>,
        sink: &mut dyn Sink,
        view: &mut ViewModel,
        press: Press,
    ) -> bool {
        let Press {
            area,
            button,
            x,
            y,
            mods,
        } = press;
        if self.drag.is_some() {
            return true;
        }
        if button == Button::Middle {
            let Some(shown) = view.target_view() else {
                return false;
            };
            self.drag = Some(Drag::Pan {
                x0: x,
                y0: y + cx.rows_top,
                scroll0: shown.scroll_px,
            });
            return true;
        }
        if area == Area::Ruler {
            if cx.state.project.timeline.clips.is_empty() {
                return false;
            }
            let frame = cx.view.scrub_frame(x, clips_total(cx.state));
            sink.dispatch(seek(frame));
            sink.scrub(frame);
            sink.set_drag(Some(DragKind::Scrub));
            self.drag = Some(Drag::Scrub {
                frame,
                last_seek: cx.now,
                seeked: frame,
            });
            return true;
        }
        let hit = cx.hit(x, y);
        let selection = &cx.state.ui.selection;
        let clip = match &hit {
            Hit::TrimStart(c) | Hit::TrimEnd(c) | Hit::Body(c) => Some(c.clone()),
            Hit::Empty => None,
        };
        let Some(clip) = clip else {
            if !selection.is_empty() {
                sink.dispatch(Command::Select {
                    op: SelectOp::Clear,
                });
            }
            return true;
        };
        // A modifier selects and never starts a drag (D-37).
        if mods.shift {
            sink.dispatch(Command::Select {
                op: SelectOp::RangeTo(clip),
            });
            return true;
        }
        if mods.ctrl {
            sink.dispatch(Command::Select {
                op: SelectOp::Toggle(clip),
            });
            return true;
        }
        let edits = edits_enabled(cx.state).is_ok();
        match hit {
            Hit::TrimStart(c) if edits && !mods.alt => {
                self.drag = Some(Drag::Trim {
                    clip: c,
                    edge: TrimEdge::Start,
                    x0: x,
                    x,
                    started: false,
                });
            }
            Hit::TrimEnd(c) if edits && !mods.alt => {
                self.drag = Some(Drag::Trim {
                    clip: c,
                    edge: TrimEdge::End,
                    x0: x,
                    x,
                    started: false,
                });
            }
            _ => {
                let was_selected = selection.contains(&clip);
                if !was_selected {
                    sink.dispatch(Command::Select {
                        op: SelectOp::Set(vec![clip.clone()]),
                    });
                }
                self.drag = Some(Drag::Pending {
                    clip,
                    x0: x,
                    was_selected,
                    can_move: edits && !mods.alt,
                });
            }
        }
        true
    }

    /// A motion: hover, or the drag in progress.
    pub fn decide_motion(
        &mut self,
        cx: &Cx<'_>,
        sink: &mut dyn Sink,
        view: &mut ViewModel,
        area: Area,
        (x, y): (f64, f64),
    ) {
        match &mut self.drag {
            None => {
                let hover = match (area, cx.hit(x, y)) {
                    (Area::Rows, Hit::TrimStart(c) | Hit::TrimEnd(c) | Hit::Body(c)) => Some(c),
                    _ => None,
                };
                if hover != self.hover {
                    self.hover.clone_from(&hover);
                    sink.set_hover(hover);
                }
            }
            Some(Drag::Pending {
                clip,
                x0,
                was_selected,
                can_move,
            }) => {
                if *can_move && (x - *x0).abs() >= MOVE_START_PX {
                    let block = if *was_selected {
                        cx.state.ui.selection.clone()
                    } else {
                        vec![clip.clone()]
                    };
                    self.drag = Some(Drag::Move { block, x });
                    sink.set_drag(Some(DragKind::Move));
                }
            }
            Some(Drag::Move { x: at, .. }) => *at = x,
            Some(Drag::Trim {
                clip,
                edge,
                x0,
                x: at,
                started,
            }) => {
                *at = x;
                if !*started && (x - *x0).abs() >= 1.0 {
                    *started = true;
                    sink.set_drag(Some(match edge {
                        TrimEdge::Start => DragKind::TrimStart,
                        TrimEdge::End => DragKind::TrimEnd,
                    }));
                }
                if *started
                    && let Some(p) = tl::trim_preview(
                        cx.state,
                        &cx.view,
                        clip,
                        *edge,
                        x - *x0,
                        cx.state.ui.snapping,
                        cx.playhead,
                    )
                {
                    sink.set_trim_frame(Some(p.trim_frame));
                }
            }
            Some(Drag::Scrub {
                frame,
                last_seek,
                seeked,
            }) => {
                let at = cx.view.scrub_frame(x, clips_total(cx.state));
                if at != *frame {
                    *frame = at;
                    sink.scrub(at);
                }
                if *frame != *seeked && cx.now.saturating_sub(*last_seek) >= SCRUB_EVERY {
                    sink.dispatch(seek(*frame));
                    *seeked = *frame;
                    *last_seek = cx.now;
                }
            }
            Some(Drag::Pan { x0, y0, scroll0 }) => {
                view.scroll_to(*scroll0 - (x - *x0), cx.now);
                // Absolute, in the lanes' window: the canvas moves with the
                // scroll the pan sets, so its own y would feed back.
                sink.pan_lanes(y + cx.rows_top - *y0);
            }
        }
    }

    /// A release: the gesture's one command (D-41).
    pub fn decide_release(&mut self, cx: &Cx<'_>, sink: &mut dyn Sink, (x, _y): (f64, f64)) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        match drag {
            Drag::Pending {
                clip, was_selected, ..
            } => {
                // A press on a selected clip selects just it on release.
                if was_selected && cx.state.ui.selection != [clip.clone()] {
                    sink.dispatch(Command::Select {
                        op: SelectOp::Set(vec![clip]),
                    });
                }
            }
            Drag::Move { block, .. } => {
                if let Some(cmd) =
                    tl::move_preview(cx.state, &cx.view, &block, x).and_then(|p| p.command)
                {
                    sink.dispatch(cmd);
                }
                sink.set_drag(None);
            }
            Drag::Trim {
                clip,
                edge,
                x0,
                started,
                ..
            } => {
                if started {
                    if let Some(cmd) = tl::trim_preview(
                        cx.state,
                        &cx.view,
                        &clip,
                        edge,
                        x - x0,
                        cx.state.ui.snapping,
                        cx.playhead,
                    )
                    .and_then(|p| p.command)
                    {
                        sink.dispatch(cmd);
                    }
                    sink.set_trim_frame(None);
                    sink.set_drag(None);
                }
            }
            Drag::Scrub { frame, seeked, .. } => {
                let at = cx.view.scrub_frame(x, clips_total(cx.state));
                let last = if at != frame {
                    sink.scrub(at);
                    at
                } else {
                    frame
                };
                if last != seeked {
                    sink.dispatch(seek(last));
                }
                sink.set_drag(None);
            }
            Drag::Pan { .. } => {}
        }
    }

    /// `Esc` (D-42), or the dragged clip went away: the gesture ends
    /// without a command.
    pub fn decide_cancel(&mut self, sink: &mut dyn Sink) {
        match self.drag.take() {
            Some(Drag::Move { .. } | Drag::Scrub { .. }) => sink.set_drag(None),
            Some(Drag::Trim { started, .. }) => {
                if started {
                    sink.set_trim_frame(None);
                    sink.set_drag(None);
                }
            }
            Some(Drag::Pending { .. } | Drag::Pan { .. }) | None => {}
        }
    }

    /// A snapshot mid-drag: a drag whose clip is gone is cancelled, and a
    /// scrub or a pan once the timeline has no view (`has_view`: it went
    /// empty, the ruler and the canvas with it, so their release never
    /// comes).
    pub fn on_state(&mut self, state: &State, has_view: bool, sink: &mut dyn Sink) {
        let gone = |id: &ClipId| state.project.clip(id).is_none();
        let cancel = match &self.drag {
            Some(Drag::Move { block, .. }) => block.iter().all(gone),
            Some(Drag::Trim { clip, .. }) | Some(Drag::Pending { clip, .. }) => gone(clip),
            Some(Drag::Scrub { .. } | Drag::Pan { .. }) => !has_view,
            None => false,
        };
        if cancel {
            self.decide_cancel(sink);
        }
        if let Some(hover) = &self.hover
            && gone(hover)
        {
            self.hover = None;
            sink.set_hover(None);
        }
    }

    /// The pointer left the rows.
    pub fn leave(&mut self, sink: &mut dyn Sink) {
        if self.hover.take().is_some() {
            sink.set_hover(None);
        }
    }
}

/// A bin drop of `take` at `x` (D-38): `ClipAdd{at: Index(slot)}`, unless
/// edits are off (recording) or the take can't go on the timeline.
pub fn decide_drop(state: &State, view: &Viewport, take: &TakeId, x: f64) -> Option<Command> {
    edits_enabled(state).ok()?;
    let t = state.project.takes.get(take)?;
    if t.status != TakeStatus::Ok {
        return None;
    }
    let slot = tl::drop_slot(view, &state.project.timeline.clips, x);
    Some(tl::drop_command(take, slot))
}

// ------------------------------------------------------- the frame clock

/// What moves on the timeline at a frame of its frame clock. The canvas
/// re-renders only for what it draws itself (C16); the playing playhead
/// and the snap flash are the overlay's (and the ruler's cell), drawn over
/// the canvas's cached render node (C18).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickFacts {
    /// A zoom or scroll eases.
    pub easing: bool,
    /// A take counts down, records or saves (the ghost grows, the saving
    /// stripes move); never under test mode's frozen clock.
    pub recording: bool,
    /// An autocut analysis runs (its clips' spinners turn); not frozen.
    pub analyzing: bool,
    /// The fit grew with the ghost this frame.
    pub grew: bool,
    /// The move drag's neighbours ease to their places.
    pub gap_easing: bool,
    /// A trim drag started: it snaps to the playhead.
    pub trimming: bool,
    /// The Program plays: the playhead moves.
    pub playing: bool,
    /// The snap guide flashes.
    pub flash: bool,
}

impl TickFacts {
    /// What `state` moves: a take recording, an analysis running (nothing
    /// under the `frozen` clock).
    pub fn of(state: &State, frozen: bool) -> Self {
        Self {
            recording: !frozen && !state.runtime.record.is_idle(),
            analyzing: !frozen
                && state.runtime.jobs.values().any(|j| {
                    matches!(
                        j.kind,
                        qrec_core::state::JobKind::Autocut { dry_run: false, .. }
                    )
                }),
            ..Self::default()
        }
    }

    /// The canvas draws something new this frame.
    pub fn canvas_moves(&self) -> bool {
        self.easing
            || self.recording
            || self.analyzing
            || self.grew
            || self.gap_easing
            || (self.trimming && self.playing)
    }
}

// ------------------------------------------------------------- the ghost

/// The record clock as last seen, for the ghost's growth between clocks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GhostClock {
    pub take: Option<TakeId>,
    pub elapsed_ns: u64,
    pub at: Duration,
    /// Recording (it grows); finalizing keeps the last length.
    pub running: bool,
}

impl GhostClock {
    /// Follows a snapshot's record phase at `now`; `true` while it is the
    /// take it followed before and records: the frame clock may have grown
    /// its ghost past this clock.
    pub fn follow(&mut self, record: &RecordPhase, now: Duration) -> bool {
        let before = self.take.clone();
        match record {
            RecordPhase::Recording {
                take, elapsed_ns, ..
            } => {
                if self.take.as_ref() != Some(take) || self.elapsed_ns != *elapsed_ns {
                    *self = GhostClock {
                        take: Some(take.clone()),
                        elapsed_ns: *elapsed_ns,
                        at: now,
                        running: true,
                    };
                }
            }
            RecordPhase::Finalizing { take, .. } => {
                if self.take.as_ref() == Some(take) {
                    self.running = false;
                } else {
                    *self = GhostClock::default();
                }
            }
            _ => *self = GhostClock::default(),
        }
        self.running && before.is_some() && before == self.take
    }

    /// The ghost's length at `now`: the last record clock, grown on the
    /// frame clock for at most 300 ms; the clock's once it finalizes, and
    /// when `frozen` (test mode: screenshots show the clock).
    pub fn elapsed(&self, now: Duration, frozen: bool) -> u64 {
        if !self.running || frozen {
            return self.elapsed_ns;
        }
        let lead = now.saturating_sub(self.at).min(GHOST_LEAD);
        self.elapsed_ns + lead.as_nanos() as u64
    }
}

/// The level ring's readings are this far apart (20 Hz).
pub const READING_MS: f64 = 50.0;

/// The live waveform under a ghost `ghost_w` px wide whose take is
/// `elapsed_ms` long (G§4.5), for `columns` (counted from its right edge
/// leftward, as `tl::ghost_columns` gives them), in screen order: per column
/// the loudest RMS among the level readings in its time (`history` oldest
/// first, one per 50 ms, the newest at the right edge), drawn as the peak
/// of a sine of that RMS (× √2), so a steady tone keeps its height when the
/// take lands and its peaks are drawn. A column older than the readings
/// reach is silent (0).
pub fn ghost_amplitudes<T>(
    history: &[T],
    rms_db: impl Fn(&T) -> f32,
    elapsed_ms: f64,
    ghost_w: f64,
    columns: std::ops::Range<usize>,
) -> Vec<f32> {
    let per_px_ms = if ghost_w > 0.0 {
        elapsed_ms / ghost_w
    } else {
        READING_MS
    };
    let n = history.len() as f64;
    columns
        .rev()
        .map(|c| {
            // The readings in the column's time: from `b` (its left edge)
            // to `a` (its right), as indices from the oldest.
            let t1 = c as f64 * per_px_ms;
            let t0 = t1 + per_px_ms;
            let a = n - 1.0 - t1 / READING_MS;
            let b = n - 1.0 - t0 / READING_MS;
            if a < 0.0 {
                return 0.0;
            }
            let (lo, hi) = (b.floor().max(0.0) as usize, a.ceil() as usize);
            let hi = hi.min(history.len() - 1);
            // The loudest first, one `powf` (dB to amplitude is monotone).
            let db = history[lo..=hi]
                .iter()
                .map(&rms_db)
                .fold(f32::NEG_INFINITY, f32::max);
            (10f32.powf(db / 20.0) * std::f32::consts::SQRT_2).min(1.0)
        })
        .collect()
}

/// How many of the newest level readings [`ghost_amplitudes`] reads for
/// `columns` (the oldest one's time, and one more).
pub fn ghost_readings(elapsed_ms: f64, ghost_w: f64, columns: &std::ops::Range<usize>) -> usize {
    let per_px_ms = if ghost_w > 0.0 {
        elapsed_ms / ghost_w
    } else {
        READING_MS
    };
    let all = (elapsed_ms / READING_MS).ceil().max(0.0);
    let oldest = (columns.end as f64 * per_px_ms / READING_MS).ceil() + 2.0;
    all.min(oldest) as usize
}

/// The cached analyses to ask the preview cache for (G§3.11): each silence
/// file's at the project's threshold and minimum silence, as (file, the
/// `SilenceKey` its answer carries), unless that key was asked for or
/// answered already.
pub fn silence_requests(
    files: &BTreeMap<String, FileFact>,
    settings: &AutocutSettings,
    asked: &BTreeSet<SilenceKey>,
    cached: &BTreeMap<SilenceKey, Vec<Interval>>,
) -> Vec<(String, SilenceKey)> {
    let key = |f: &FileFact| {
        SilenceKey::compute(
            &f.path,
            f.bytes,
            f.mtime_ns,
            settings.threshold_db,
            settings.min_silence_ms,
        )
    };
    files
        .values()
        .map(|f| (f.path.clone(), key(f)))
        .filter(|(_, k)| !asked.contains(k) && !cached.contains_key(k))
        .collect()
}

/// The height thumbnails are made at, in device px (G§3.11): the
/// filmstrip's 72 px × the UI scale (the canvas paints scaled by it, D-46)
/// × the window's scale factor, so a texture is drawn at its own size
/// (72/90/108 at 100/125/150 % on a 1× output, twice that on a 2× one);
/// at most 1080.
pub fn thumb_px(ui_scale: f64, scale_factor: i32) -> u32 {
    let device = ui_scale.max(1.0) * f64::from(scale_factor.max(1));
    ((72.0 * device).ceil() as u32).min(1080)
}

/// The take a drag's string names: `qrec-take:<id>` (D-38).
pub fn parse_drop(text: &str) -> Option<TakeId> {
    text.strip_prefix("qrec-take:")?.trim().parse().ok()
}

fn seek(frame: Frame) -> Command {
    Command::Seek {
        to: SeekTarget::To(TimeArg::frames(frame)),
    }
}

fn clips_total(state: &State) -> Frame {
    qrec_core::timeline::total_frames(&state.project.timeline.clips)
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
