//! Layer edits on the canvas, GTK-free (D-32, D-33, D-41): a drag
//! ([`DragSession`]: move, resize, crop) and a nudge burst
//! ([`NudgeCoalescer`]) each end in exactly one `LayerSet`, and nothing
//! when the layer ends where it was. The stage's glue feeds them pointer
//! and key events and carries out what they say (the transient preview,
//! `LocalUi.layer_drag`, the command).

use qrec_core::LayerId;
use qrec_core::model::{Crop, Layer, Rect, Scene};
use qrec_core::protocol::command::{Command, LayerPatch, SceneTarget};
use qrec_core::view::DragKind;
use qrec_core::view::canvas::{DragFrame, DragOp, DragStart, Guide, Scale, Snap, drag, nudge};

use crate::store::local::LayerDragPreview;

/// How far (preview px) the pointer travels before a press is a drag: a
/// click only selects.
pub const DRAG_THRESHOLD: f64 = 3.0;

/// A nudge burst ends this long after its last press when no key release
/// came (D-33).
pub const NUDGE_IDLE_MS: u64 = 300;

/// The command that puts `layer` of `target` at `rect` and `crop`, if
/// either changed from `from`.
fn layer_set(
    target: &SceneTarget,
    layer: &LayerId,
    from: (Rect, Crop),
    to: (Rect, Crop),
) -> Option<Command> {
    let rect = (to.0 != from.0).then_some(to.0);
    let crop = (to.1 != from.1).then_some(to.1);
    if rect.is_none() && crop.is_none() {
        return None;
    }
    Some(Command::LayerSet {
        target: target.clone(),
        layer: layer.clone(),
        patch: LayerPatch {
            rect,
            crop,
            ..LayerPatch::default()
        },
    })
}

/// `scene` with `layer` at `rect` and `crop`: the transient layout the live
/// preview shows during a Live gesture.
pub fn with_layer(scene: &Scene, layer: &LayerId, rect: Rect, crop: Crop) -> Scene {
    let mut scene = scene.clone();
    if let Some(l) = scene.layer_mut(layer) {
        l.rect = rect;
        l.crop = crop;
    }
    scene
}

/// A layer drag from press to release.
#[derive(Debug, Clone, PartialEq)]
pub struct DragSession {
    pub target: SceneTarget,
    pub layer: LayerId,
    pub op: DragOp,
    pub start: DragStart,
    pub frame: DragFrame,
    /// Past [`DRAG_THRESHOLD`]: a drag, not a click.
    pub moved: bool,
    /// The last update, in canvas px, with its aspect and snapping: a
    /// rebase applies it to the new start.
    last: Option<(f64, f64, bool, Option<Snap>)>,
}

impl DragSession {
    /// A press on `layer` of `target` that may become `op`.
    pub fn new(target: SceneTarget, layer: &Layer, op: DragOp, source: Option<(u32, u32)>) -> Self {
        let start = DragStart::of(layer, source);
        Self {
            target,
            layer: layer.id.clone(),
            op,
            frame: DragFrame {
                rect: start.rect,
                crop: start.crop,
                guides: Vec::new(),
            },
            start,
            moved: false,
            last: None,
        }
    }

    /// The pointer is `(dx, dy)` preview px from the press (100 % units);
    /// `scale` is canvas → preview. `true` when this update made it a drag.
    pub fn update(
        &mut self,
        dx: f64,
        dy: f64,
        scale: Scale,
        free_aspect: bool,
        snap: Option<&Snap>,
    ) -> bool {
        let became = !self.moved && dx.hypot(dy) >= DRAG_THRESHOLD;
        if became {
            self.moved = true;
        }
        if self.moved {
            let (cx, cy) = (dx / scale.x.max(1e-9), dy / scale.y.max(1e-9));
            self.frame = drag(&self.start, self.op, cx, cy, free_aspect, snap);
            self.last = Some((cx, cy, free_aspect, snap.copied()));
        }
        became
    }

    /// The `LocalUi.drag` kind.
    pub fn kind(&self) -> DragKind {
        match self.op {
            DragOp::Move => DragKind::LayerMove,
            DragOp::Resize { .. } => DragKind::LayerResize,
            DragOp::Crop { .. } => DragKind::LayerCrop,
        }
    }

    /// The guides the drag snapped to.
    pub fn guides(&self) -> &[Guide] {
        &self.frame.guides
    }

    /// The one command of the gesture (D-41): `None` for a click or a drag
    /// that ended where it began.
    pub fn command(&self) -> Option<Command> {
        if !self.moved {
            return None;
        }
        layer_set(
            &self.target,
            &self.layer,
            (self.start.rect, self.start.crop),
            (self.frame.rect, self.frame.crop),
        )
    }

    /// The inspector's view of it (`LocalUi.layer_drag`).
    pub fn preview(&self) -> LayerDragPreview {
        LayerDragPreview {
            target: self.target.clone(),
            layer: self.layer.clone(),
            rect: self.frame.rect,
            crop: self.frame.crop,
        }
    }

    /// The layer changed under the drag (a snapshot mid-drag): carry on from
    /// its new layout, the drag's offset applied to it at once (a release
    /// without another motion sends that, R23). `false` when it is gone
    /// (the drag is cancelled).
    pub fn rebase(&mut self, scene: Option<&Scene>) -> bool {
        let Some(layer) = scene.and_then(|s| s.layer(&self.layer)) else {
            return false;
        };
        if layer.rect != self.start.rect || layer.crop != self.start.crop {
            let source = self.start.source;
            self.start = DragStart::of(layer, source);
            self.frame = match (self.moved, self.last) {
                (true, Some((cx, cy, free, snap))) => {
                    drag(&self.start, self.op, cx, cy, free, snap.as_ref())
                }
                _ => DragFrame {
                    rect: self.start.rect,
                    crop: self.start.crop,
                    guides: Vec::new(),
                },
            };
        }
        true
    }
}

/// The gestures' `LayerSet`s waiting for their replies (D-41): the
/// newest one's layout shows until its own reply. An older reply ends
/// nothing (R23: it cleared a newer commit's preview, and the next gesture
/// started from the snapshot's older layout).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InFlight {
    next: u64,
    newest: Option<(u64, LayerDragPreview)>,
}

impl InFlight {
    /// A gesture's `LayerSet` goes out with `preview`: its ticket.
    pub fn issue(&mut self, preview: LayerDragPreview) -> u64 {
        self.next += 1;
        self.newest = Some((self.next, preview));
        self.next
    }

    /// The reply of `ticket` came: `true` when it was the newest commit's
    /// (its preview goes).
    pub fn answered(&mut self, ticket: u64) -> bool {
        if self.newest.as_ref().is_some_and(|(t, _)| *t == ticket) {
            self.newest = None;
            true
        } else {
            false
        }
    }

    /// The newest unanswered commit's layout.
    pub fn current(&self) -> Option<&LayerDragPreview> {
        self.newest.as_ref().map(|(_, p)| p)
    }
}

/// `layer` of `target` as the newest gesture left it: an unanswered
/// commit's layout (what the box shows) when it is that layer's, else as
/// the snapshot has it. A drag or a nudge starts from it (R23).
pub fn newest(layer: &Layer, target: &SceneTarget, pending: Option<&LayerDragPreview>) -> Layer {
    let mut layer = layer.clone();
    if let Some(p) = pending.filter(|p| p.layer == layer.id && p.target == *target) {
        layer.rect = p.rect;
        layer.crop = p.crop;
    }
    layer
}

/// A nudge burst in progress (D-33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NudgeBurst {
    pub target: SceneTarget,
    pub layer: LayerId,
    /// Where the layer was when the burst began.
    pub start: Rect,
    pub rect: Rect,
    pub crop: Crop,
    last_ms: u64,
}

impl NudgeBurst {
    pub fn preview(&self) -> LayerDragPreview {
        LayerDragPreview {
            target: self.target.clone(),
            layer: self.layer.clone(),
            rect: self.rect,
            crop: self.crop,
        }
    }

    fn command(&self) -> Option<Command> {
        layer_set(
            &self.target,
            &self.layer,
            (self.start, self.crop),
            (self.rect, self.crop),
        )
    }
}

/// Arrow-key nudges, coalesced into one `LayerSet` per burst (D-33): the
/// first press starts it, repeats add to it, a key release (or
/// [`NUDGE_IDLE_MS`] after the last press) ends it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NudgeCoalescer {
    burst: Option<NudgeBurst>,
}

impl NudgeCoalescer {
    /// A press of a `layer.nudge-*` key: `(dx, dy)` per press, ×10 with
    /// `ten`. A burst of another layer ends first (its command is the
    /// second value).
    pub fn press(
        &mut self,
        target: &SceneTarget,
        layer: &Layer,
        step: (i32, i32, bool),
        now_ms: u64,
    ) -> (&NudgeBurst, Option<Command>) {
        let other = self
            .burst
            .as_ref()
            .is_some_and(|b| b.layer != layer.id || b.target != *target);
        let flushed = if other { self.end() } else { None };
        let burst = self.burst.get_or_insert_with(|| NudgeBurst {
            target: target.clone(),
            layer: layer.id.clone(),
            start: layer.rect,
            rect: layer.rect,
            crop: layer.crop,
            last_ms: now_ms,
        });
        let (dx, dy, ten) = step;
        burst.rect = nudge(burst.rect, dx, dy, ten);
        burst.last_ms = now_ms;
        (burst, flushed)
    }

    /// A key release: the burst's one command.
    pub fn release(&mut self) -> Option<Command> {
        self.end()
    }

    /// The burst's command once [`NUDGE_IDLE_MS`] passed since its last
    /// press without a release.
    pub fn due(&mut self, now_ms: u64) -> Option<Command> {
        let due = self
            .burst
            .as_ref()
            .is_some_and(|b| now_ms.saturating_sub(b.last_ms) >= NUDGE_IDLE_MS);
        if due { self.end() } else { None }
    }

    /// When [`due`](Self::due) ends the burst.
    pub fn deadline(&self) -> Option<u64> {
        self.burst.as_ref().map(|b| b.last_ms + NUDGE_IDLE_MS)
    }

    /// `Esc`: the burst is dropped, nothing goes out. `true` when there
    /// was one.
    pub fn cancel(&mut self) -> bool {
        self.burst.take().is_some()
    }

    pub fn active(&self) -> Option<&NudgeBurst> {
        self.burst.as_ref()
    }

    fn end(&mut self) -> Option<Command> {
        self.burst.take().and_then(|b| b.command())
    }
}

#[cfg(test)]
mod tests {
    use qrec_core::model::Canvas;
    use qrec_core::testkit;
    use qrec_core::view::canvas::{Edge, Handle};

    use super::*;

    fn face() -> (SceneTarget, Layer) {
        let project = testkit::design_project();
        (SceneTarget::Project, project.scene.layers[1].clone())
    }

    fn half() -> Scale {
        Scale { x: 0.5, y: 0.5 }
    }

    fn rect_of(cmd: &Command) -> Option<Rect> {
        match cmd {
            Command::LayerSet { patch, .. } => patch.rect,
            _ => None,
        }
    }

    /// D-33: press, repeats and the release are one `LayerSet` with the
    /// sum of the steps.
    #[test]
    fn a_held_arrow_is_one_layer_set_on_release() {
        let (target, layer) = face();
        let mut nudges = NudgeCoalescer::default();
        for (i, step) in [(-1, 0, false), (-1, 0, false), (-1, 0, false), (0, 1, true)]
            .into_iter()
            .enumerate()
        {
            let (burst, flushed) = nudges.press(&target, &layer, step, 1000 + i as u64 * 30);
            assert_eq!(flushed, None);
            assert_eq!(burst.start, layer.rect);
        }
        assert_eq!(
            nudges.active().map(|b| b.rect),
            Some(Rect::new(1497, 770, 384, 288))
        );
        assert_eq!(nudges.due(1100), None, "repeats keep it going");
        let cmd = nudges.release().expect("one command");
        assert_eq!(rect_of(&cmd), Some(Rect::new(1497, 770, 384, 288)));
        assert!(matches!(&cmd, Command::LayerSet { patch, .. } if patch.crop.is_none()));
        assert_eq!(nudges.release(), None, "and only one");
        assert_eq!(nudges.active(), None);
    }

    #[test]
    fn without_a_release_the_burst_ends_after_300_ms() {
        let (target, layer) = face();
        let mut nudges = NudgeCoalescer::default();
        nudges.press(&target, &layer, (0, -1, false), 5000);
        assert_eq!(nudges.deadline(), Some(5000 + NUDGE_IDLE_MS));
        assert_eq!(nudges.due(5299), None);
        let cmd = nudges.due(5300).expect("ended by the timeout");
        assert_eq!(rect_of(&cmd), Some(Rect::new(1500, 759, 384, 288)));
        assert_eq!(nudges.due(9999), None);
        // A release after the timeout sends nothing more.
        assert_eq!(nudges.release(), None);
    }

    #[test]
    fn esc_and_net_zero_bursts_send_nothing() {
        let (target, layer) = face();
        let mut nudges = NudgeCoalescer::default();
        nudges.press(&target, &layer, (1, 0, false), 0);
        assert!(nudges.cancel());
        assert_eq!(nudges.release(), None);
        // Left then right: back where it was.
        nudges.press(&target, &layer, (-1, 0, false), 0);
        nudges.press(&target, &layer, (1, 0, false), 10);
        assert_eq!(nudges.release(), None);
        assert!(!nudges.cancel());
    }

    #[test]
    fn a_burst_on_another_layer_ends_the_first() {
        let (target, layer) = face();
        let mut screen = testkit::design_project().scene.layers[0].clone();
        screen.locked = false;
        let mut nudges = NudgeCoalescer::default();
        nudges.press(&target, &layer, (1, 0, false), 0);
        let (burst, flushed) = nudges.press(&target, &screen, (0, 1, false), 10);
        assert_eq!(burst.layer, screen.id);
        assert_eq!(
            flushed.as_ref().and_then(rect_of),
            Some(Rect::new(1501, 760, 384, 288))
        );
        assert_eq!(
            nudges.release().as_ref().and_then(rect_of),
            Some(Rect::new(0, 1, 1920, 1080))
        );
    }

    /// D-32, D-41: a click selects only; a drag is one `LayerSet`, of the
    /// fields it changed; a drag back to the start is none.
    #[test]
    fn a_drag_is_one_layer_set_and_a_click_none() {
        let (target, layer) = face();
        let mut click = DragSession::new(target.clone(), &layer, DragOp::Move, Some((1280, 720)));
        assert!(!click.update(1.0, 1.0, half(), false, None));
        assert_eq!(click.command(), None);
        assert_eq!(click.kind(), DragKind::LayerMove);

        let mut drag = DragSession::new(target.clone(), &layer, DragOp::Move, Some((1280, 720)));
        assert!(drag.update(-110.0, 0.0, half(), false, None), "a drag now");
        assert!(!drag.update(-113.5, 1.5, half(), false, None));
        assert_eq!(drag.preview().rect, Rect::new(1273, 763, 384, 288));
        let cmd = drag.command().expect("one command");
        assert_eq!(rect_of(&cmd), Some(Rect::new(1273, 763, 384, 288)));
        // Snapped (S13): the left edge on the right third.
        let canvas = Canvas::default();
        let snap = Snap::new(&canvas, half());
        drag.update(-113.5, 1.5, half(), false, Some(&snap));
        assert_eq!(drag.frame.rect.x, 1280);
        assert_eq!(drag.guides()[0].label, "Right third");
        // Back to the start: nothing.
        drag.update(0.0, 0.0, half(), false, None);
        assert_eq!(drag.command(), None);

        // A crop changes only the crop.
        let mut crop = DragSession::new(
            target.clone(),
            &layer,
            DragOp::Crop { edge: Edge::Left },
            Some((1280, 720)),
        );
        crop.update(10.0, 0.0, half(), false, None);
        assert_eq!(crop.kind(), DragKind::LayerCrop);
        match crop.command() {
            Some(Command::LayerSet { patch, .. }) => {
                assert_eq!(patch.rect, None);
                assert_eq!(patch.crop.map(|c| c.left), Some(50));
            }
            other => panic!("{other:?}"),
        }
        let resize = DragSession::new(
            target,
            &layer,
            DragOp::Resize {
                handle: Handle::Right,
            },
            None,
        );
        assert_eq!(resize.kind(), DragKind::LayerResize);
    }

    fn preview_at(x: i32) -> LayerDragPreview {
        let (target, layer) = face();
        LayerDragPreview {
            target,
            layer: layer.id,
            rect: Rect::new(x, 760, 384, 288),
            crop: Crop::default(),
        }
    }

    /// R23: an older reply leaves a newer commit's preview alone; the
    /// newest one's reply ends it.
    #[test]
    fn only_the_newest_commits_reply_ends_its_preview() {
        let mut flight = InFlight::default();
        let a = flight.issue(preview_at(1499));
        let b = flight.issue(preview_at(1498));
        assert!(!flight.answered(a), "A's reply isn't the newest");
        assert_eq!(flight.current().map(|p| p.rect.x), Some(1498));
        assert!(flight.answered(b));
        assert_eq!(flight.current(), None);
        assert!(!flight.answered(b), "once");
    }

    /// R23: a gesture that starts while a commit is unanswered starts from
    /// its layout (the box drawn), not from the snapshot's older one; a
    /// commit of another layer or scene doesn't count.
    #[test]
    fn a_gesture_starts_from_the_unanswered_layout() {
        let (target, layer) = face();
        let pending = preview_at(1400);
        assert_eq!(newest(&layer, &target, Some(&pending)).rect.x, 1400);
        assert_eq!(newest(&layer, &target, None).rect.x, 1500);
        let other = SceneTarget::Take(qrec_core::TakeId::numbered(5));
        assert_eq!(newest(&layer, &other, Some(&pending)).rect.x, 1500);
        let mut screen = layer.clone();
        screen.id = LayerId::numbered(1);
        assert_eq!(newest(&screen, &target, Some(&pending)).rect, screen.rect);
    }

    /// R23: a snapshot that moves the layer mid-drag carries the drag on
    /// at once: the release sends the start plus the offset, not the frame
    /// of the last motion.
    #[test]
    fn a_rebase_mid_drag_moves_the_frame_too() {
        let (target, layer) = face();
        let mut scene = testkit::design_project().scene;
        let mut drag = DragSession::new(target, &layer, DragOp::Move, None);
        drag.update(20.0, 0.0, half(), false, None);
        assert_eq!(drag.frame.rect, Rect::new(1540, 760, 384, 288));
        scene.layers[1].rect.y = 700;
        assert!(drag.rebase(Some(&scene)));
        assert_eq!(drag.frame.rect, Rect::new(1540, 700, 384, 288));
        assert_eq!(
            drag.command().as_ref().and_then(rect_of),
            Some(Rect::new(1540, 700, 384, 288))
        );
    }

    #[test]
    fn a_drag_follows_its_layer_and_stops_when_it_goes() {
        let (target, layer) = face();
        let mut scene = testkit::design_project().scene;
        let mut drag = DragSession::new(target, &layer, DragOp::Move, None);
        drag.update(20.0, 0.0, half(), false, None);
        scene.layers[1].rect.y = 700;
        assert!(drag.rebase(Some(&scene)));
        drag.update(20.0, 0.0, half(), false, None);
        assert_eq!(drag.frame.rect, Rect::new(1540, 700, 384, 288));
        let moved = with_layer(&scene, &layer.id, drag.frame.rect, drag.frame.crop);
        assert_eq!(moved.layers[1].rect, Rect::new(1540, 700, 384, 288));
        assert_eq!(moved.layers[0], scene.layers[0]);
        scene.layers.pop();
        assert!(!drag.rebase(Some(&scene)), "the layer went");
        assert!(!drag.rebase(None));
    }
}
