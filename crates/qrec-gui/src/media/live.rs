//! The live picture's glue (G§3.9): what the stage needs of the live
//! preview besides its paintable. Owner: WP4.
//!
//! - [`live_sources`]: `LocalUi.live_status` as the view models' map
//!   (source name → state and size: the layer boxes in Live mode).
//! - [`transient`]: `LiveControl::transient_layout` during a Live layer
//!   drag or nudge burst (D-32, D-33): the project scene with the dragged
//!   layer where the gesture has it; `None` on cancel or after a failed
//!   release. A `SetLive` (the `LayerSet`'s) ends it in the engine.
//! - [`LiveWatch`]: whether the live picture has shown its sources yet,
//!   for the test channel's `waitIdle` (G§3.18): after the window opens in
//!   Live, `waitIdle` waits until every visible video layer's source the
//!   engine reports `Live` has its size and the picture drew
//!   [`FIRST_FRAMES`] frames since (the sink's paintable has no size before
//!   its first frame; the fake screen showed 0.2–0.7 s late without it).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use qrec_core::model::Scene;
use qrec_core::view::canvas::source_name;
use qrec_core::view::{LiveSourceState, LiveSources};
use qrec_shell::capture::{LiveSourceStatus, live_sources_view};

use crate::{Ctx, guarded};

/// Frames the live picture draws after its sources negotiated before
/// `waitIdle` counts it as shown (about a third of a second at 30 fps).
pub const FIRST_FRAMES: u64 = 10;
/// How long `waitIdle` waits for the engine to report a layer's source at
/// all before it stops waiting for it (an engine that never answers).
pub const REPORT_WAIT: Duration = Duration::from_secs(15);

/// The live statuses as the view models take them.
pub fn live_sources(statuses: &[LiveSourceStatus]) -> LiveSources {
    live_sources_view(statuses)
}

/// Shows `scene`'s layout in the live preview until the next `SetLive`
/// (`None`: back to the live scene's own).
pub fn transient(ctx: &Ctx, scene: Option<Scene>) {
    ctx.media.live_control().transient_layout(scene);
}

/// Where the live picture is (for `waitIdle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// Nothing to wait for: the stage doesn't show the live picture, or no
    /// visible layer's source is live.
    Settled,
    /// A visible layer's source isn't reported yet, or is live without a
    /// size yet.
    Negotiating,
    /// Every live source has its size: the frames drawn since count.
    Ready,
}

/// The readiness of the live picture of `scene` from the statuses: what
/// [`LiveWatch`] counts frames from.
pub fn readiness(scene: &Scene, live: &LiveSources) -> Readiness {
    let mut any_live = false;
    for layer in scene.layers.iter().filter(|l| l.visible) {
        match live.get(source_name(&layer.source)) {
            None => return Readiness::Negotiating,
            Some(view) => match &view.state {
                LiveSourceState::Live if view.size.is_none() => return Readiness::Negotiating,
                LiveSourceState::Live => any_live = true,
                // The test channel waits for the portal itself; refused and
                // dead sources show nothing to wait for.
                LiveSourceState::AwaitingPortal
                | LiveSourceState::Refused { .. }
                | LiveSourceState::Dead { .. } => {}
            },
        }
    }
    if any_live {
        Readiness::Ready
    } else {
        Readiness::Settled
    }
}

thread_local! {
    static WATCH: RefCell<Option<Rc<LiveWatch>>> = const { RefCell::new(None) };
}

/// Counts the live picture's frames since its sources became ready.
pub struct LiveWatch {
    frames: Cell<u64>,
    /// The frame count when the picture became ready (`None`: not ready).
    ready_at: Cell<Option<u64>>,
    readiness: Cell<Readiness>,
    /// Since when the stage waits for a report.
    waiting_since: Cell<Option<Instant>>,
}

impl LiveWatch {
    /// Follows the live paintable's frames (once per window).
    pub fn install(ctx: &Ctx) -> Rc<LiveWatch> {
        let watch = Rc::new(LiveWatch {
            frames: Cell::new(0),
            ready_at: Cell::new(None),
            readiness: Cell::new(Readiness::Settled),
            waiting_since: Cell::new(None),
        });
        let weak = Rc::downgrade(&watch);
        let paintable = ctx.media.live_paintable();
        paintable.connect_invalidate_contents(guarded!("live picture: a frame", move |_| {
            if let Some(watch) = weak.upgrade() {
                watch.frames.set(watch.frames.get() + 1);
            }
        }));
        // The sink's size (the composite: the canvas fitted into the
        // preview's max size × the window's scale, G§3.4.2), logged.
        paintable.connect_invalidate_size(guarded!("live picture: its size", move |p| {
            let (w, h) = (p.intrinsic_width(), p.intrinsic_height());
            if w > 0 && h > 0 {
                tracing::info!("the live picture is {w}\u{d7}{h}");
            }
        }));
        WATCH.with(|w| *w.borrow_mut() = Some(Rc::clone(&watch)));
        watch
    }

    /// The stage shows the live picture (`showing`) with these statuses.
    pub fn update(&self, showing: bool, scene: &Scene, live: &LiveSources) {
        let now = if showing {
            readiness(scene, live)
        } else {
            Readiness::Settled
        };
        let before = self.readiness.replace(now);
        match now {
            Readiness::Ready if before != Readiness::Ready => {
                self.ready_at.set(Some(self.frames.get()));
            }
            Readiness::Ready => {}
            _ => self.ready_at.set(None),
        }
        match now {
            Readiness::Negotiating if self.waiting_since.get().is_none() => {
                self.waiting_since.set(Some(Instant::now()));
            }
            Readiness::Negotiating => {}
            _ => self.waiting_since.set(None),
        }
    }

    /// Why the live picture isn't shown yet.
    fn busy(&self) -> Option<String> {
        match self.readiness.get() {
            Readiness::Settled => None,
            Readiness::Negotiating => {
                let stale = self
                    .waiting_since
                    .get()
                    .is_some_and(|since| since.elapsed() >= REPORT_WAIT);
                (!stale).then(|| "the live sources".to_owned())
            }
            Readiness::Ready => {
                let since = self.ready_at.get().unwrap_or(0);
                (self.frames.get().saturating_sub(since) < FIRST_FRAMES)
                    .then(|| "the live picture's first frames".to_owned())
            }
        }
    }
}

/// Why the live picture isn't shown yet (the test channel's `waitIdle`).
pub fn busy() -> Option<String> {
    WATCH.with(|w| w.borrow().as_ref().and_then(|watch| watch.busy()))
}

#[cfg(test)]
mod tests {
    use qrec_core::testkit;
    use qrec_core::view::LiveSourceView;

    use super::*;

    fn view(state: LiveSourceState, size: Option<(u32, u32)>) -> LiveSourceView {
        LiveSourceView { state, size }
    }

    #[test]
    fn the_picture_is_ready_once_every_live_source_negotiated() {
        let scene = testkit::design_project().scene;
        let mut live = LiveSources::new();
        assert_eq!(
            readiness(&scene, &live),
            Readiness::Negotiating,
            "not reported"
        );
        live.insert(
            "DP-4".into(),
            view(LiveSourceState::Live, Some((2560, 1440))),
        );
        assert_eq!(
            readiness(&scene, &live),
            Readiness::Negotiating,
            "the camera"
        );
        live.insert("/dev/video0".into(), view(LiveSourceState::Live, None));
        assert_eq!(
            readiness(&scene, &live),
            Readiness::Negotiating,
            "no size yet"
        );
        live.insert(
            "/dev/video0".into(),
            view(LiveSourceState::Live, Some((1280, 720))),
        );
        assert_eq!(readiness(&scene, &live), Readiness::Ready);
        // A refused screen and a dead camera: nothing to wait for.
        live.insert(
            "DP-4".into(),
            view(
                LiveSourceState::Refused {
                    cancelled: false,
                    message: "denied".into(),
                },
                None,
            ),
        );
        assert_eq!(
            readiness(&scene, &live),
            Readiness::Ready,
            "the camera still"
        );
        live.insert(
            "/dev/video0".into(),
            view(
                LiveSourceState::Dead {
                    message: "gone".into(),
                },
                None,
            ),
        );
        assert_eq!(readiness(&scene, &live), Readiness::Settled);
        // Hidden layers don't count.
        let mut hidden = scene.clone();
        for layer in &mut hidden.layers {
            layer.visible = false;
        }
        assert_eq!(readiness(&hidden, &LiveSources::new()), Readiness::Settled);
    }

    #[test]
    fn frames_count_from_readiness() {
        let watch = LiveWatch {
            frames: Cell::new(100),
            ready_at: Cell::new(None),
            readiness: Cell::new(Readiness::Settled),
            waiting_since: Cell::new(None),
        };
        let scene = testkit::design_project().scene;
        let mut live = LiveSources::new();
        watch.update(true, &scene, &live);
        assert_eq!(watch.busy().as_deref(), Some("the live sources"));
        for (name, size) in [("DP-4", (2560, 1440)), ("/dev/video0", (1280, 720))] {
            live.insert(name.into(), view(LiveSourceState::Live, Some(size)));
        }
        watch.update(true, &scene, &live);
        assert!(watch.busy().is_some(), "no frame yet");
        watch.frames.set(100 + FIRST_FRAMES);
        watch.update(true, &scene, &live);
        assert_eq!(watch.busy(), None);
        // Not showing the live picture: never busy.
        watch.update(false, &scene, &LiveSources::new());
        assert_eq!(watch.busy(), None);
    }
}
