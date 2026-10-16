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
//!   A rebuild whose sources keep their states and sizes (an audio track
//!   added: S01b's steps) sends no `LiveStatus`, and its new pipeline
//!   draws black until its sources deliver: so `waitIdle` also waits for
//!   the engine's [`LiveControl::progress`] (every `SetLive` handled, each
//!   video source negotiated in the pipeline it built), then counts
//!   [`FIRST_FRAMES`] again for each new pipeline.
//!
//! [`LiveControl::progress`]: qrec_shell::capture::LiveControl::progress

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use qrec_core::model::Scene;
use qrec_core::view::canvas::source_name;
use qrec_core::view::{LiveSourceState, LiveSources};
use qrec_shell::capture::{LiveProgress, LiveSourceStatus, live_sources_view};

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
    /// The stage shows the live picture.
    showing: Cell<bool>,
    /// The engine's [`LiveProgress`] (`None`: no engine).
    progress: Box<dyn Fn() -> Option<LiveProgress>>,
    /// The live pipeline the frames are counted for.
    generation: Cell<Option<u64>>,
    /// Since when the engine hasn't settled its live scenes.
    unsettled_since: Cell<Option<Instant>>,
}

impl LiveWatch {
    fn new(progress: Box<dyn Fn() -> Option<LiveProgress>>) -> Self {
        LiveWatch {
            frames: Cell::new(0),
            ready_at: Cell::new(None),
            readiness: Cell::new(Readiness::Settled),
            waiting_since: Cell::new(None),
            showing: Cell::new(false),
            progress,
            generation: Cell::new(None),
            unsettled_since: Cell::new(None),
        }
    }

    /// Follows the live paintable's frames (once per window).
    pub fn install(ctx: &Ctx) -> Rc<LiveWatch> {
        let media = ctx.media.clone();
        let watch = Rc::new(LiveWatch::new(Box::new(move || {
            media.live_control().progress()
        })));
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
        self.showing.set(showing);
        let now = if showing {
            readiness(scene, live)
        } else {
            Readiness::Settled
        };
        let before = self.readiness.replace(now);
        match now {
            Readiness::Ready if before != Readiness::Ready => {
                self.ready_at.set(Some(self.frames.get()));
                // The pipeline these frames come from.
                self.generation.set((self.progress)().map(|p| p.generation));
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
        if !self.showing.get() {
            return None;
        }
        if let Some(progress) = (self.progress)() {
            if !progress.settled {
                let since = self.unsettled_since.get().unwrap_or_else(Instant::now);
                self.unsettled_since.set(Some(since));
                // An engine that never settles (a wedged one) is waited
                // for as long as one that never reports.
                if since.elapsed() < REPORT_WAIT {
                    return Some("the live pipeline".to_owned());
                }
            } else {
                self.unsettled_since.set(None);
                if self.readiness.get() == Readiness::Ready
                    && self.generation.replace(Some(progress.generation))
                        != Some(progress.generation)
                {
                    // A new pipeline whose sources negotiated now: the
                    // frames drawn before (the last pipeline's, then black)
                    // don't count.
                    self.ready_at.set(Some(self.frames.get()));
                }
            }
        }
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
        let watch = LiveWatch::new(Box::new(|| None));
        watch.frames.set(100);
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

    /// A rebuild that keeps every source's state and size (S01b's audio
    /// tracks) sends no status: the engine's progress holds `waitIdle`
    /// until the new pipeline's sources negotiated, then its own first
    /// frames count. Before, `waitIdle` answered at once and the S01b shot
    /// caught the new pipeline's black frames.
    #[test]
    fn a_rebuild_without_a_new_status_waits_for_its_own_frames() {
        let progress = Rc::new(Cell::new(LiveProgress {
            settled: true,
            generation: 1,
        }));
        let watch = LiveWatch::new(Box::new({
            let progress = Rc::clone(&progress);
            move || Some(progress.get())
        }));
        let scene = testkit::design_project().scene;
        let mut live = LiveSources::new();
        for (name, size) in [("DP-4", (2560, 1440)), ("/dev/video0", (1280, 720))] {
            live.insert(name.into(), view(LiveSourceState::Live, Some(size)));
        }
        watch.update(true, &scene, &live);
        watch.frames.set(FIRST_FRAMES);
        assert_eq!(watch.busy(), None, "pipeline 1 shown");
        // An audio track added: the engine rebuilds; the statuses stay.
        progress.set(LiveProgress {
            settled: false,
            generation: 1,
        });
        watch.update(true, &scene, &live);
        assert_eq!(watch.busy().as_deref(), Some("the live pipeline"));
        progress.set(LiveProgress {
            settled: false,
            generation: 2,
        });
        watch.frames.set(3 * FIRST_FRAMES);
        assert_eq!(watch.busy().as_deref(), Some("the live pipeline"));
        // Negotiated: pipeline 2's frames count from now.
        progress.set(LiveProgress {
            settled: true,
            generation: 2,
        });
        assert_eq!(
            watch.busy().as_deref(),
            Some("the live picture's first frames")
        );
        watch.frames.set(4 * FIRST_FRAMES - 1);
        assert!(watch.busy().is_some(), "one frame short");
        watch.frames.set(4 * FIRST_FRAMES);
        assert_eq!(watch.busy(), None);
        // A layout change settles in the same pipeline: nothing to count.
        progress.set(LiveProgress {
            settled: false,
            generation: 2,
        });
        assert!(watch.busy().is_some());
        progress.set(LiveProgress {
            settled: true,
            generation: 2,
        });
        assert_eq!(watch.busy(), None);
        // Not showing the live picture: the engine isn't waited for.
        progress.set(LiveProgress {
            settled: false,
            generation: 3,
        });
        watch.update(false, &scene, &live);
        assert_eq!(watch.busy(), None);
    }
}
