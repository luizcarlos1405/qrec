//! The stage (S01a–S13, C06–C08; G§3.15, G§4.6): the preview, its
//! overlays, the countdown, the transport row, the first run and layer
//! editing on the canvas. Owner: WP4.
//!
//! ```text
//! region-stage: gtk::Overlay ("Stage"; the window's `bg` around)
//! ├ preview: gtk::Overlay at stage_geometry().preview
//! │ ├ StagePicture: canvas background, the live or program paintable,
//! │ │   the 1 px border outside (S10: 2 px accent)
//! │ ├ Countdown (C07) and its plate
//! │ ├ StageCanvas (C08): selection box, handles, snap guides; the input
//! │ ├ guide tags, the position readout
//! │ ├ badges top-right, the REC cluster top-left, the trim plate
//! │ │   bottom-left, D-05's plate centered
//! │ └ FirstRun (S01a) centered, instead of the picture
//! ├ transport at stage_geometry().transport
//! └ "Opening <name>…" (G§3.3)
//! ```
//!
//! What the picture shows: the program sink's in Program, during a trim
//! scrub (S10) and a bin preview (D-17), but only the canvas background on
//! an empty timeline (D-27); the live sink's in Live; the first run when the
//! project scene is empty in Live; the canvas background with "Live preview
//! starts after this take" in Live during a take started in Program (D-05:
//! the engine adds no preview branch mid-take).
//!
//! Layer editing (D-09, D-32, D-33, D-41, D-43): in the scene
//! `canvas::edit_target` names (Live: the project scene; Program: the
//! selected clip's take; nothing to edit otherwise). A press selects the
//! layer under it (`SetUi{selectedLayer}`, UI state); a drag past 3 px
//! moves, resizes or (`Alt` + an edge) crops it, snapped to the canvas'
//! edges, thirds and center while `ui.snapping`; the release sends one
//! `LayerSet`. Arrow keys (the router's `layer.nudge-*` GUI actions) nudge
//! in bursts: one `LayerSet` on the key release, or 300 ms after the last
//! press. In Live the preview follows a gesture through
//! `LiveControl::transient_layout` (`None` once the newest `LayerSet`'s
//! reply came, after its `SetLive`, or on `Esc`); in Program the box and
//! the readout move and the picture changes on release. A gesture's
//! `LayerSet` shows until its own reply ([`edit::InFlight`]), and a drag
//! or nudge that starts meanwhile starts from it (R23). The box shows while the canvas has
//! the focus or a gesture runs (S06 and S14 show none with the focus
//! elsewhere). The edit preview is `LocalUi.layer_drag` (the inspector's).

pub mod canvas;
pub mod countdown;
pub mod edit;
pub mod first_run;
pub mod overlays;
pub mod picture;
pub mod transport;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, glib};
use qrec_core::model::Scene;
use qrec_core::protocol::ToastLevel;
use qrec_core::protocol::command::{LayerPatch, Nullable, SceneTarget, UiPatch};
use qrec_core::state::{CanvasMode, RecordPhase};
use qrec_core::view::canvas::{
    DragOp, Guide, Hit, LIVE_AFTER_TAKE, Scale, Snap, badges, countdown, countdown_plate,
    edit_target, hit, layer_boxes, nudge_step, placed, readout, readout_origin, rec, scene_of,
    source_sizes, stage_geometry, transport, trim_plate,
};
use qrec_core::view::notices::{Notice, error_notice};
use qrec_core::view::{self, DragKind, FocusRegion, Rect};
use qrec_core::{Command, Frame, LayerId, State};

use crate::Ctx;
use crate::keys::router::NUDGE_BINDINGS;
use crate::media::live::{self, LiveWatch};
use crate::media::program::{self, ProgramSync};
use crate::store::Reply;
use crate::store::local::{LayerDragPreview, LocalUi, StartupPhase};
use crate::theme::motion;
use crate::widgets::button::binding_caps;
use canvas::{CanvasDraw, StageCanvas};
use countdown::{Countdown, CountdownState};
use edit::{DRAG_THRESHOLD, DragSession, InFlight, NudgeCoalescer, newest, with_layer};
use first_run::FirstRun;
use overlays::{ModeBadges, RecCluster, TrimPlateView, center_plate};
use picture::{Frame as PictureFrame, StagePicture};
use transport::TransportRow;

/// The countdown plate's gap under the ring (logical px at 100 %).
const PLATE_GAP: f64 = 12.0;
/// A guide tag's offset from its line and from the preview's edge.
const TAG_OFFSET: f64 = 6.0;
const TAG_INSET: f64 = 8.0;

/// The stage region (`region-stage`).
pub struct Stage {
    root: gtk::Overlay,
    _inner: Rc<Inner>,
}

impl Stage {
    pub fn new(ctx: &Ctx) -> Self {
        let inner = Inner::new(ctx);
        Self {
            root: inner.root.clone(),
            _inner: inner,
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

/// Why the stage isn't settled yet (the test channel's `waitIdle`, G§3.18):
/// the live picture's first frames, a program reload still due.
pub fn busy() -> Vec<String> {
    live::busy().into_iter().chain(program::busy()).collect()
}

/// What is being edited, and how.
#[derive(Default)]
struct Editing {
    drag: Option<DragSession>,
    /// The pressed layer is locked: a drag only says so (once).
    locked: Option<LayerId>,
    locked_told: bool,
    /// The drag's last offset (widget px): a modifier change re-applies it.
    last_offset: (f64, f64),
    nudges: NudgeCoalescer,
    /// Gestures' `LayerSet`s wait for their replies: the newest one's
    /// preview stays until its own reply (R23).
    pending: InFlight,
}

impl Editing {
    /// A gesture shows: a drag past the threshold, a nudge burst, a commit
    /// in flight.
    fn active(&self) -> bool {
        self.drag.as_ref().is_some_and(|d| d.moved)
            || self.nudges.active().is_some()
            || self.pending.current().is_some()
    }

    /// The layout the gesture has for a layer now.
    fn preview(&self) -> Option<LayerDragPreview> {
        if let Some(drag) = self.drag.as_ref().filter(|d| d.moved) {
            return Some(drag.preview());
        }
        if let Some(burst) = self.nudges.active() {
            return Some(burst.preview());
        }
        self.pending.current().cloned()
    }

    /// `scene` (of `target`) as the newest unanswered commit left it: what
    /// the box shows, so a press hits and starts from that (R23).
    fn newest_scene(&self, scene: &Scene, target: &SceneTarget) -> Scene {
        match self.pending.current().filter(|p| p.target == *target) {
            Some(p) => with_layer(scene, &p.layer, p.rect, p.crop),
            None => scene.clone(),
        }
    }
}

/// Where the preview's positioned children go, in canvas px.
#[derive(Debug, Clone, Default)]
struct Placed {
    canvas: (f64, f64),
    readout: Option<Rect>,
    guides: Vec<Guide>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Overlay,
    preview: gtk::Overlay,
    picture: StagePicture,
    countdown: Countdown,
    countdown_plate: gtk::Box,
    countdown_take: gtk::Label,
    countdown_hint: gtk::Label,
    canvas: StageCanvas,
    readout: gtk::Label,
    guide_tags: [gtk::Label; 2],
    badges: ModeBadges,
    rec: Rc<RecCluster>,
    trim_plate: TrimPlateView,
    live_plate: gtk::Label,
    first_run: Rc<FirstRun>,
    transport: Rc<TransportRow>,
    caption: gtk::Label,
    live_watch: Rc<LiveWatch>,
    _program: Rc<ProgramSync>,
    placed: RefCell<Placed>,
    edit: RefCell<Editing>,
    /// The countdown's current digit and timer, and when it came.
    countdown_tick: Cell<Option<(u32, u64, Duration)>>,
    /// D-05: the take running now started while Program showed.
    take_in_program: Cell<bool>,
    /// `Alt` is held over the canvas (crop brackets).
    alt: Cell<bool>,
    rendering: Cell<bool>,
    dirty: Cell<bool>,
    nudge_timer: RefCell<Option<glib::SourceId>>,
    epoch: Instant,
}

fn label(classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(None);
    for class in classes {
        l.add_css_class(class);
    }
    l.set_can_target(false);
    l
}

fn rectangle(x: f64, y: f64, w: f64, h: f64) -> gdk::Rectangle {
    let (x0, y0) = (x.round(), y.round());
    gdk::Rectangle::new(
        x0 as i32,
        y0 as i32,
        ((x + w).round() - x0).max(0.0) as i32,
        ((y + h).round() - y0).max(0.0) as i32,
    )
}

/// The natural size of `widget`.
fn natural(widget: &impl IsA<gtk::Widget>) -> (f64, f64) {
    let (_, nat) = widget.preferred_size();
    (f64::from(nat.width()), f64::from(nat.height()))
}

impl Inner {
    fn new(ctx: &Ctx) -> Rc<Self> {
        let palette = ctx.theme.palette();
        let root = gtk::Overlay::new();
        root.set_widget_name("region-stage");
        root.add_css_class("stage");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.update_property(&[gtk::accessible::Property::Label("Stage")]);
        let ground = gtk::Box::new(gtk::Orientation::Vertical, 0);
        ground.set_hexpand(true);
        ground.set_vexpand(true);
        root.set_child(Some(&ground));

        let preview = gtk::Overlay::new();
        preview.set_widget_name("preview");
        preview.add_css_class("stage-preview");
        let picture = StagePicture::new(Rc::clone(&palette));
        preview.set_child(Some(&picture));
        let countdown = Countdown::new(Rc::clone(&palette));
        preview.add_overlay(&countdown);
        let canvas = StageCanvas::new(Rc::clone(&palette));
        preview.add_overlay(&canvas);
        let guide_tags = [
            label(&["guide-tag", "t-caption"]),
            label(&["guide-tag", "t-caption"]),
        ];
        for tag in &guide_tags {
            tag.set_visible(false);
            preview.add_overlay(tag);
        }
        let readout = label(&["layer-readout", "t-mono-sm"]);
        readout.set_visible(false);
        preview.add_overlay(&readout);
        let countdown_plate = gtk::Box::new(gtk::Orientation::Vertical, 0);
        countdown_plate.add_css_class("countdown-plate");
        countdown_plate.set_can_target(false);
        let countdown_take = label(&["t-body-strong"]);
        let countdown_hint = label(&["t-caption"]);
        countdown_plate.append(&countdown_take);
        countdown_plate.append(&countdown_hint);
        countdown_plate.set_visible(false);
        preview.add_overlay(&countdown_plate);
        let live_plate = center_plate(LIVE_AFTER_TAKE);
        preview.add_overlay(&live_plate);
        let trim_plate = TrimPlateView::new();
        preview.add_overlay(&trim_plate.root);
        let badges = ModeBadges::new();
        preview.add_overlay(&badges.root);
        let rec = RecCluster::new();
        preview.add_overlay(&rec.root);
        let first_run = FirstRun::new(ctx);
        preview.add_overlay(&first_run.root);
        root.add_overlay(&preview);

        let transport = TransportRow::new(ctx);
        root.add_overlay(&transport.root);

        let caption = center_plate("");
        caption.remove_css_class("stage-plate");
        caption.add_css_class("muted");
        root.add_overlay(&caption);

        let inner = Rc::new(Inner {
            ctx: ctx.clone(),
            root,
            preview,
            picture,
            countdown,
            countdown_plate,
            countdown_take,
            countdown_hint,
            canvas,
            readout,
            guide_tags,
            badges,
            rec,
            trim_plate,
            live_plate,
            first_run,
            transport,
            caption,
            live_watch: LiveWatch::install(ctx),
            _program: ProgramSync::install(ctx),
            placed: RefCell::new(Placed::default()),
            edit: RefCell::new(Editing::default()),
            countdown_tick: Cell::new(None),
            take_in_program: Cell::new(false),
            alt: Cell::new(false),
            rendering: Cell::new(false),
            dirty: Cell::new(false),
            nudge_timer: RefCell::new(None),
            epoch: Instant::now(),
        });
        inner.position_children();
        inner.attach_input();
        inner.follow(ctx);
        inner.register_actions(ctx);
        inner.render();
        inner
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn scale(&self) -> f64 {
        self.ctx.theme.scale().max(0.5)
    }

    // ------------------------------------------------------- placement

    /// The preview and the transport at `stage_geometry` (in 100 % units,
    /// × the UI scale); the preview's positioned children.
    fn position_children(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.root.connect_get_child_position(crate::guarded!(
            "stage: placement",
            move |root, child| {
                let this = weak.upgrade()?;
                let s = this.scale();
                let canvas = this.ctx.store.state().project.canvas.clone();
                let g = stage_geometry(
                    f64::from(root.width()) / s,
                    f64::from(root.height()) / s,
                    &canvas,
                );
                let r = if child == this.preview.upcast_ref::<gtk::Widget>() {
                    g.preview
                } else if child == this.transport.root.upcast_ref::<gtk::Widget>() {
                    g.transport
                } else {
                    return None;
                };
                Some(rectangle(r.x * s, r.y * s, r.w * s, r.h * s))
            }
        ));
        let weak = Rc::downgrade(self);
        self.preview.connect_get_child_position(crate::guarded!(
            "stage: the preview's plates",
            move |preview, child| {
                let this = weak.upgrade()?;
                this.place_on_preview(preview, child)
            }
        ));
    }

    fn place_on_preview(
        &self,
        preview: &gtk::Overlay,
        child: &gtk::Widget,
    ) -> Option<gdk::Rectangle> {
        let s = self.scale();
        let (w, h) = (f64::from(preview.width()), f64::from(preview.height()));
        let (nw, nh) = natural(child);
        if child == self.countdown_plate.upcast_ref::<gtk::Widget>() {
            let y = h / 2.0 + (countdown::RING / 2.0 + PLATE_GAP) * s;
            return Some(rectangle(w / 2.0 - nw / 2.0, y, nw, nh));
        }
        let placed = self.placed.borrow();
        let (cw, ch) = placed.canvas;
        if cw <= 0.0 || ch <= 0.0 {
            return None;
        }
        let (sx, sy) = (w / cw, h / ch);
        if child == self.readout.upcast_ref::<gtk::Widget>() {
            let b = placed.readout?;
            let in_100 = Rect::new(b.x * sx / s, b.y * sy / s, b.w * sx / s, b.h * sy / s);
            let (x, y) = readout_origin(in_100, (nw / s, nh / s));
            return Some(rectangle(x * s, y * s, nw, nh));
        }
        for (tag, guide) in self.guide_tags.iter().zip(&placed.guides) {
            if child == tag.upcast_ref::<gtk::Widget>() {
                let (x, y) = match guide.axis {
                    view::canvas::Axis::X => (guide.at * sx + TAG_OFFSET * s, TAG_INSET * s),
                    view::canvas::Axis::Y => (TAG_INSET * s, guide.at * sy + TAG_OFFSET * s),
                };
                // Kept on the preview near its right and bottom edges.
                let x = x.min(w - nw - TAG_INSET * s).max(0.0);
                let y = y.min(h - nh - TAG_INSET * s).max(0.0);
                return Some(rectangle(x, y, nw, nh));
            }
        }
        None
    }

    // ------------------------------------------------------- following

    fn follow(self: &Rc<Self>, ctx: &Ctx) {
        let weak = Rc::downgrade(self);
        ctx.store
            .subscribe(move |change| {
                let Some(this) = weak.upgrade() else { return };
                let (prev, next) = (&change.prev.runtime.record, &change.next.runtime.record);
                let starting = matches!(prev, RecordPhase::Idle | RecordPhase::Countdown { .. })
                    && matches!(
                        next,
                        RecordPhase::Starting { .. } | RecordPhase::Recording { .. }
                    );
                if starting {
                    this.take_in_program
                        .set(change.next.ui.canvas_mode == CanvasMode::Program);
                }
                if next.is_idle() {
                    this.take_in_program.set(false);
                }
                if change.prev.project.canvas != change.next.project.canvas {
                    this.root.queue_allocate();
                }
                this.check_drag(&change.next);
                this.render();
            })
            .detach();
        let weak = Rc::downgrade(self);
        ctx.store
            .subscribe_local(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.render();
                }
            })
            .detach();
        let weak = Rc::downgrade(self);
        ctx.theme
            .subscribe(move |theme| {
                if let Some(this) = weak.upgrade() {
                    let palette = theme.palette();
                    this.picture.set_palette(Rc::clone(&palette));
                    this.canvas.set_palette(Rc::clone(&palette));
                    this.countdown.set_palette(palette);
                    this.root.queue_allocate();
                    this.render();
                }
            })
            .detach();
    }

    /// `layer.nudge-*` (the key router hands them over when they can run,
    /// D-33).
    fn register_actions(self: &Rc<Self>, ctx: &Ctx) {
        for id in NUDGE_BINDINGS {
            let weak = Rc::downgrade(self);
            ctx.actions.register(
                id,
                Rc::new(move || {
                    if let Some(this) = weak.upgrade() {
                        this.nudge(id);
                    }
                }),
            );
        }
    }

    // ------------------------------------------------------- rendering

    /// Renders from the newest snapshot and `LocalUi` (a render a render
    /// causes runs after it).
    fn render(self: &Rc<Self>) {
        if self.rendering.replace(true) {
            self.dirty.set(true);
            return;
        }
        for _ in 0..4 {
            self.dirty.set(false);
            crate::guard::run("stage: render", || self.render_once());
            if !self.dirty.get() {
                break;
            }
        }
        self.rendering.set(false);
    }

    /// The trim scrub, while a trim drag shows one (S10).
    fn trim(local: &LocalUi) -> Option<(bool, qrec_core::TakeId, Frame)> {
        let start = match local.drag {
            Some(DragKind::TrimStart) => true,
            Some(DragKind::TrimEnd) => false,
            _ => return None,
        };
        local
            .trim_frame
            .as_ref()
            .map(|t| (start, t.take.clone(), t.frame))
    }

    fn render_once(self: &Rc<Self>) {
        let state = self.ctx.store.state();
        let local = self.ctx.store.local().clone();
        let open = local.is_open();
        let mode = state.ui.canvas_mode;
        let reduced = self.ctx.theme.motion().reduced;
        let live_sources = live::live_sources(&local.live_status);
        let trim = Self::trim(&local);
        let take_preview = local.take_preview.as_ref().map(|t| t.take.clone());
        let first_run = open
            && mode == CanvasMode::Live
            && state.project.scene.is_empty()
            && take_preview.is_none()
            && trim.is_none();
        let live_after_take = mode == CanvasMode::Live
            && !state.runtime.record.is_idle()
            && self.take_in_program.get();
        let program_picture =
            take_preview.is_some() || trim.is_some() || mode == CanvasMode::Program;
        let shows_live = open && !program_picture && !first_run && !live_after_take;

        // The picture.
        let has_clips = !state.project.timeline.clips.is_empty();
        let paintable = if first_run {
            None
        } else if program_picture {
            (has_clips || take_preview.is_some() || trim.is_some())
                .then(|| self.ctx.media.program_paintable())
        } else if shows_live {
            Some(self.ctx.media.live_paintable())
        } else {
            None
        };
        self.picture.set_paintable(paintable.as_ref());
        self.picture.set_background(if first_run {
            None
        } else {
            gdk::RGBA::parse(state.project.canvas.background.as_str()).ok()
        });
        self.picture.set_frame(if first_run {
            PictureFrame::None
        } else if trim.is_some() {
            PictureFrame::Trim
        } else {
            PictureFrame::Border
        });
        self.live_watch
            .update(shows_live, &state.project.scene, &live_sources);

        // The overlays.
        self.badges.root.set_visible(open && !first_run);
        self.badges
            .update(&badges(&state, take_preview.as_ref(), false));
        self.rec
            .update(&rec(&state), &state.project.canvas, reduced);
        self.render_countdown(&state, reduced);
        self.trim_plate.update(
            trim.as_ref()
                .map(|(start, take, frame)| trim_plate(*start, take, *frame))
                .as_ref(),
        );
        self.live_plate.set_visible(live_after_take);
        self.first_run.set_inventory(local.sources.as_ref());
        self.first_run.show(first_run);
        self.first_run.take_focus(local.focus);
        let slow = local.startup == StartupPhase::Opening { slow: true };
        if slow {
            self.caption
                .set_text(&format!("Opening {}\u{2026}", state.project.name));
        }
        self.caption.set_visible(slow);

        // The transport.
        let player_frame = self.ctx.media.position_ns().map_or(0, |ns| {
            qrec_core::time::ns_to_frame_floor(
                i64::try_from(ns).unwrap_or(i64::MAX),
                state.project.canvas.fps,
            )
            .max(0) as Frame
        });
        // The program's transport: a bin preview's take playing isn't the
        // program playing (R16).
        let playing = local.program_playing;
        let t = transport(&state, player_frame, playing, local.playing_rate);
        self.transport.update(&t, &state, playing);

        // The canvas.
        let editable =
            open && !first_run && take_preview.is_none() && trim.is_none() && !live_after_take;
        self.canvas.set_visible(!first_run);
        self.render_canvas(&state, &local, editable, &live_sources);

        // Last: what other regions read.
        let checked = first_run.then(|| self.first_run.checked());
        if local.first_run_checked != checked {
            self.ctx
                .store
                .update_local(move |l| l.first_run_checked = checked);
        }
    }

    fn render_countdown(&self, state: &State, reduced: bool) {
        let RecordPhase::Countdown {
            remaining, timer, ..
        } = &state.runtime.record
        else {
            self.countdown_tick.set(None);
            self.countdown.set_state(None);
            self.countdown_plate.set_visible(false);
            return;
        };
        let since = match self.countdown_tick.get() {
            Some((digit, id, since)) if digit == *remaining && id == timer.0 => since,
            _ => {
                let now = motion::now();
                self.countdown_tick.set(Some((*remaining, timer.0, now)));
                now
            }
        };
        let Some(c) = countdown(state, 0) else {
            return;
        };
        self.countdown.set_state(Some(CountdownState {
            digit: c.digit,
            since,
            reduced,
            scale: self.scale(),
        }));
        let keymap = self.ctx.store.keymap();
        let keys: Vec<String> = ["cancel", "record.toggle"]
            .iter()
            .filter_map(|id| binding_caps(&keymap, id).map(|caps| caps.join("+")))
            .collect();
        let (take, hint) = countdown_plate(&c.take, &keys.join(" or "));
        self.countdown_take.set_text(&take);
        self.countdown_hint.set_text(&hint);
        self.countdown_plate.set_visible(true);
        self.preview.queue_allocate();
    }

    /// The selection box, the guides and the readout of the scene being
    /// edited.
    fn render_canvas(
        &self,
        state: &State,
        local: &LocalUi,
        editable: bool,
        live_sources: &view::LiveSources,
    ) {
        let canvas_size = (
            f64::from(state.project.canvas.width),
            f64::from(state.project.canvas.height),
        );
        let hide = |this: &Self| {
            this.canvas.set_draw(CanvasDraw::default());
            this.readout.set_visible(false);
            for tag in &this.guide_tags {
                tag.set_visible(false);
            }
            *this.placed.borrow_mut() = Placed {
                canvas: canvas_size,
                ..Placed::default()
            };
        };
        let Some(target) = edit_target(state).filter(|_| editable) else {
            return hide(self);
        };
        let Some(scene) = scene_of(&state.project, &target) else {
            return hide(self);
        };
        let edit = self.edit.borrow();
        let preview = edit
            .preview()
            .filter(|p| p.target == target && scene.layer(&p.layer).is_some());
        let selected = state
            .ui
            .selected_layer
            .clone()
            .filter(|id| scene.layer(id).is_some())
            .or_else(|| preview.as_ref().map(|p| p.layer.clone()));
        let active = edit.active();
        let show = selected.is_some() && (local.focus == FocusRegion::Stage || active);
        let sizes = source_sizes(&state.project, &target, live_sources);
        let selection = selected.as_ref().and_then(|id| {
            let layer = scene.layer(id)?;
            if !layer.visible {
                return None;
            }
            let (rect, crop) = match &preview {
                Some(p) if &p.layer == id => (p.rect, p.crop),
                _ => (layer.rect, layer.crop),
            };
            let dst = placed(rect, crop, layer.fit, sizes.get(id).copied());
            Some((
                Rect::new(
                    f64::from(dst.x),
                    f64::from(dst.y),
                    f64::from(dst.w),
                    f64::from(dst.h),
                ),
                rect,
            ))
        });
        let guides: Vec<Guide> = edit
            .drag
            .as_ref()
            .filter(|d| d.moved)
            .map(|d| d.guides().to_vec())
            .unwrap_or_default();
        drop(edit);
        let dim = selected.as_ref().is_some_and(|id| {
            view::enabled(
                state,
                &Command::LayerSet {
                    target: target.clone(),
                    layer: id.clone(),
                    patch: LayerPatch::default(),
                },
            )
            .is_err()
        });
        self.canvas.set_draw(CanvasDraw {
            canvas: canvas_size,
            selection: selection.filter(|_| show).map(|(r, _)| r),
            dim,
            brackets: self.alt.get() && show,
            guides: guides.iter().map(|g| (g.axis, g.at)).collect(),
            scale: self.scale(),
        });
        let readout_on = show && active;
        match selection.filter(|_| readout_on) {
            Some((box_rect, rect)) => {
                self.readout.set_text(&readout(rect));
                self.readout.set_visible(true);
                self.placed.borrow_mut().readout = Some(box_rect);
            }
            None => {
                self.readout.set_visible(false);
                self.placed.borrow_mut().readout = None;
            }
        }
        for (i, tag) in self.guide_tags.iter().enumerate() {
            match guides.get(i) {
                Some(guide) => {
                    tag.set_text(&guide.label);
                    tag.set_visible(true);
                }
                None => tag.set_visible(false),
            }
        }
        {
            let mut placed = self.placed.borrow_mut();
            placed.canvas = canvas_size;
            placed.guides = guides;
        }
        self.preview.queue_allocate();
    }

    // ----------------------------------------------------------- input

    fn attach_input(self: &Rc<Self>) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let weak = Rc::downgrade(self);
        drag.connect_drag_begin(crate::guarded!("stage: press", move |g, x, y| {
            if let Some(this) = weak.upgrade() {
                this.press(x, y, g.current_event_state());
            }
        }));
        let weak = Rc::downgrade(self);
        drag.connect_drag_update(crate::guarded!("stage: drag", move |g, dx, dy| {
            if let Some(this) = weak.upgrade() {
                this.drag_to(dx, dy, g.current_event_state());
            }
        }));
        let weak = Rc::downgrade(self);
        drag.connect_drag_end(crate::guarded!("stage: release", move |_, _, _| {
            if let Some(this) = weak.upgrade() {
                this.release();
            }
        }));
        self.canvas.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(crate::guarded!("stage: hover", move |c, x, y| {
            if let Some(this) = weak.upgrade() {
                this.hover(x, y, c.current_event_state());
            }
        }));
        let weak = Rc::downgrade(self);
        motion.connect_leave(crate::guarded!("stage: leave", move |_| {
            if let Some(this) = weak.upgrade() {
                this.canvas.set_cursor_from_name(None);
                if this.alt.replace(false) {
                    this.render();
                }
            }
        }));
        self.canvas.add_controller(motion);

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_released(crate::guarded!(
            "stage: a key release",
            move |_, keyval, _, _| {
                if let Some(this) = weak.upgrade() {
                    this.key_released(keyval);
                }
            }
        ));
        let weak = Rc::downgrade(self);
        keys.connect_modifiers(crate::guarded!("stage: modifiers", move |_, modifiers| {
            if let Some(this) = weak.upgrade() {
                this.modifiers(modifiers);
            }
            glib::Propagation::Proceed
        }));
        self.root.add_controller(keys);
    }

    /// Whether the canvas edits now (S01a, previews and D-05 don't).
    fn editable(&self, state: &State, local: &LocalUi) -> bool {
        let live_after_take = state.ui.canvas_mode == CanvasMode::Live
            && !state.runtime.record.is_idle()
            && self.take_in_program.get();
        local.is_open()
            && self.canvas.is_visible()
            && local.take_preview.is_none()
            && Self::trim(local).is_none()
            && !live_after_take
    }

    /// The preview in 100 % units and its scale from canvas px.
    fn preview_scale(&self, state: &State) -> (Rect, Scale) {
        let s = self.scale();
        let preview = Rect::new(
            0.0,
            0.0,
            f64::from(self.canvas.width()) / s,
            f64::from(self.canvas.height()) / s,
        );
        (preview, Scale::of(&state.project.canvas, preview))
    }

    fn press(self: &Rc<Self>, x: f64, y: f64, modifiers: gdk::ModifierType) {
        self.canvas.grab_focus();
        let state = self.ctx.store.state();
        let local = self.ctx.store.local().clone();
        if !self.editable(&state, &local) {
            return;
        }
        let Some(target) = edit_target(&state) else {
            return;
        };
        let Some(scene) = scene_of(&state.project, &target) else {
            return;
        };
        let scene = &self.edit.borrow().newest_scene(scene, &target);
        let live_sources = live::live_sources(&local.live_status);
        let sizes = source_sizes(&state.project, &target, &live_sources);
        let (preview, _) = self.preview_scale(&state);
        let boxes = layer_boxes(scene, &state.project.canvas, &sizes, preview);
        let selected = state
            .ui
            .selected_layer
            .clone()
            .filter(|id| scene.layer(id).is_some());
        let s = self.scale();
        let hit = hit(&boxes, selected.as_ref(), x / s, y / s);
        let (layer_id, op) = match hit {
            Hit::None => {
                if state.ui.selected_layer.is_some() {
                    self.select(None);
                }
                return;
            }
            Hit::Body { layer } => (layer, DragOp::Move),
            Hit::Handle { layer, handle } => (
                layer,
                DragOp::for_handle(handle, modifiers.contains(gdk::ModifierType::ALT_MASK)),
            ),
        };
        if selected.as_ref() != Some(&layer_id) {
            self.select(Some(layer_id.clone()));
        }
        let Some(layer) = scene.layer(&layer_id) else {
            return;
        };
        // D-43: no edits while recording (or whatever the gates refuse).
        let probe = Command::LayerSet {
            target: target.clone(),
            layer: layer_id.clone(),
            patch: LayerPatch::default(),
        };
        if view::enabled(&state, &probe).is_err() {
            return;
        }
        let mut edit = self.edit.borrow_mut();
        edit.locked = layer.locked.then(|| layer_id.clone());
        edit.locked_told = false;
        edit.last_offset = (0.0, 0.0);
        edit.drag = (!layer.locked)
            .then(|| DragSession::new(target, layer, op, sizes.get(&layer_id).copied()));
    }

    /// `SetUi{selectedLayer}` (UI state: the press may send it, D-41).
    fn select(&self, layer: Option<LayerId>) {
        self.ctx.store.dispatch(Command::SetUi {
            patch: UiPatch {
                selected_layer: match layer {
                    Some(id) => Nullable::Set(id),
                    None => Nullable::Clear,
                },
                ..UiPatch::default()
            },
        });
    }

    fn drag_to(self: &Rc<Self>, dx: f64, dy: f64, modifiers: gdk::ModifierType) {
        let state = self.ctx.store.state();
        let s = self.scale();
        let free = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
        let (_, scale) = self.preview_scale(&state);
        let snap = Snap::new(&state.project.canvas, scale);
        let mut tell_locked = None;
        let update = {
            let mut edit = self.edit.borrow_mut();
            edit.last_offset = (dx, dy);
            if let Some(id) = edit.locked.clone() {
                if !edit.locked_told && dx.hypot(dy) / s >= DRAG_THRESHOLD {
                    edit.locked_told = true;
                    tell_locked = Some(id);
                }
                None
            } else if let Some(session) = edit.drag.as_mut() {
                let became = session.update(
                    dx / s,
                    dy / s,
                    scale,
                    free,
                    state.ui.snapping.then_some(&snap),
                );
                session
                    .moved
                    .then(|| (became, session.kind(), session.preview()))
            } else {
                None
            }
        };
        if let Some(id) = tell_locked {
            self.ctx.store.notify(Notice::new(
                ToastLevel::Info,
                format!("Layer {id} is locked"),
            ));
            return;
        }
        let Some((became, kind, preview)) = update else {
            return;
        };
        if became {
            let weak = Rc::downgrade(self);
            self.ctx.keys.set_drag_cancel(Some(Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.cancel();
                }
            })));
            self.canvas.set_cursor_from_name(Some(match kind {
                DragKind::LayerMove => "grabbing",
                _ => "move",
            }));
        }
        self.show_gesture(&state, Some(kind), preview);
    }

    /// A gesture's step: `LocalUi.drag` and `layer_drag`, the Live preview.
    fn show_gesture(
        self: &Rc<Self>,
        state: &State,
        kind: Option<DragKind>,
        preview: LayerDragPreview,
    ) {
        if state.ui.canvas_mode == CanvasMode::Live && preview.target == SceneTarget::Project {
            live::transient(
                &self.ctx,
                Some(with_layer(
                    &state.project.scene,
                    &preview.layer,
                    preview.rect,
                    preview.crop,
                )),
            );
        }
        self.ctx.store.update_local(move |l| {
            if kind.is_some() {
                l.drag = kind;
            }
            l.layer_drag = Some(preview);
        });
        self.render();
    }

    fn release(self: &Rc<Self>) {
        let session = {
            let mut edit = self.edit.borrow_mut();
            edit.locked = None;
            edit.drag.take()
        };
        let Some(session) = session.filter(|s| s.moved) else {
            return;
        };
        self.ctx.keys.set_drag_cancel(None);
        self.ctx.store.update_local(|l| l.drag = None);
        self.hover_cursor_reset();
        self.commit(session.command(), session.preview());
    }

    fn hover_cursor_reset(&self) {
        self.canvas.set_cursor_from_name(None);
    }

    /// The gesture's one `LayerSet` (D-41), its preview kept until the reply;
    /// a gesture that changed nothing just ends.
    fn commit(self: &Rc<Self>, cmd: Option<Command>, preview: LayerDragPreview) {
        let state = self.ctx.store.state();
        let Some(cmd) = cmd else {
            // Back where it began: the live preview's own layout again.
            self.end_preview(&state);
            return;
        };
        let ticket = self.edit.borrow_mut().pending.issue(preview);
        let weak = Rc::downgrade(self);
        let sent = cmd.clone();
        self.ctx.store.dispatch_with(cmd, false, move |reply| {
            if let Some(this) = weak.upgrade() {
                this.committed(ticket, &sent, reply);
            }
        });
        self.render();
    }

    /// The reply of commit `ticket`: the newest one's ends its preview (an
    /// older one's leaves a newer commit's alone, R23); an error toasts.
    fn committed(self: &Rc<Self>, ticket: u64, cmd: &Command, reply: Reply) {
        let newest = self.edit.borrow_mut().pending.answered(ticket);
        let state = self.ctx.store.state();
        match &reply.result {
            Ok(_) if newest => self.end_preview(&state),
            Ok(_) => {}
            Err(err) => {
                // Back where it was, and why (the snapshot didn't move it).
                if newest {
                    self.end_preview(&state);
                }
                if !reply.late
                    && let Some(notice) = error_notice(err, cmd, &state)
                {
                    self.ctx.store.notify(notice);
                }
            }
        }
    }

    /// The preview goes (unless another gesture runs), and in Live the
    /// live preview shows the scene's own layout again. A reply comes after
    /// its `LayerSet`'s `SetLive`; but a no-op `LayerSet` sends none, and a
    /// gesture started once that `SetLive` was in set a transient again,
    /// which stayed (R23).
    fn end_preview(self: &Rc<Self>, state: &State) {
        let busy = {
            let edit = self.edit.borrow();
            edit.drag.as_ref().is_some_and(|d| d.moved)
                || edit.nudges.active().is_some()
                || edit.pending.current().is_some()
        };
        if busy {
            return;
        }
        if state.ui.canvas_mode == CanvasMode::Live {
            live::transient(&self.ctx, None);
        }
        self.ctx.store.update_local(|l| l.layer_drag = None);
        self.render();
    }

    /// `Esc` during a drag or a nudge burst (D-42): nothing is sent, the
    /// layer and the live preview go back.
    fn cancel(self: &Rc<Self>) {
        {
            let mut edit = self.edit.borrow_mut();
            edit.drag = None;
            edit.locked = None;
            edit.nudges.cancel();
        }
        if let Some(timer) = self.nudge_timer.borrow_mut().take() {
            timer.remove();
        }
        self.ctx.keys.set_drag_cancel(None);
        self.hover_cursor_reset();
        let state = self.ctx.store.state();
        if state.ui.canvas_mode == CanvasMode::Live {
            // With a commit unanswered the live preview shows its layout
            // (its `SetLive` may be in already), not the cancelled one (R23).
            let pending = self
                .edit
                .borrow()
                .pending
                .current()
                .filter(|p| p.target == SceneTarget::Project)
                .cloned();
            live::transient(
                &self.ctx,
                pending.map(|p| with_layer(&state.project.scene, &p.layer, p.rect, p.crop)),
            );
        }
        self.ctx.store.update_local(|l| {
            l.drag = None;
            l.layer_drag = None;
        });
        self.render();
    }

    /// A snapshot mid-gesture: carry on from the layer's new layout, or
    /// cancel when it (or the scene being edited) went.
    fn check_drag(self: &Rc<Self>, state: &State) {
        let (gone, rebased) = {
            let mut edit = self.edit.borrow_mut();
            let Some(session) = edit.drag.as_mut() else {
                return;
            };
            let before = session.frame.clone();
            let target_kept = edit_target(state).as_ref() == Some(&session.target);
            let kept = target_kept && session.rebase(scene_of(&state.project, &session.target));
            let rebased = (kept && session.moved && session.frame != before)
                .then(|| (session.kind(), session.preview()));
            (!kept, rebased)
        };
        if gone {
            self.cancel();
        } else if let Some((kind, preview)) = rebased {
            // The drag goes on from the layer's new layout at once (R23).
            self.show_gesture(state, Some(kind), preview);
        }
    }

    fn hover(self: &Rc<Self>, x: f64, y: f64, modifiers: gdk::ModifierType) {
        let alt = modifiers.contains(gdk::ModifierType::ALT_MASK);
        if self.alt.replace(alt) != alt {
            self.render();
        }
        if self.edit.borrow().drag.as_ref().is_some_and(|d| d.moved) {
            return;
        }
        let state = self.ctx.store.state();
        let local = self.ctx.store.local().clone();
        let cursor = (|| {
            if !self.editable(&state, &local) {
                return None;
            }
            let target = edit_target(&state)?;
            let scene = scene_of(&state.project, &target)?;
            let scene = &self.edit.borrow().newest_scene(scene, &target);
            let live_sources = live::live_sources(&local.live_status);
            let sizes = source_sizes(&state.project, &target, &live_sources);
            let (preview, _) = self.preview_scale(&state);
            let boxes = layer_boxes(scene, &state.project.canvas, &sizes, preview);
            let selected = state
                .ui
                .selected_layer
                .clone()
                .filter(|id| scene.layer(id).is_some());
            let s = self.scale();
            match hit(&boxes, selected.as_ref(), x / s, y / s) {
                Hit::None => None,
                Hit::Body { layer } => {
                    let locked = scene.layer(&layer).is_some_and(|l| l.locked);
                    Some(if locked { "not-allowed" } else { "grab" })
                }
                Hit::Handle { handle, .. } => Some(handle.cursor()),
            }
        })();
        self.canvas.set_cursor_from_name(cursor);
    }

    /// `Shift` and `Alt` change a drag's constraints while it runs (D-42).
    fn modifiers(self: &Rc<Self>, modifiers: gdk::ModifierType) {
        let alt = modifiers.contains(gdk::ModifierType::ALT_MASK);
        let alt_changed = self.alt.replace(alt) != alt;
        let offset = {
            let edit = self.edit.borrow();
            edit.drag
                .as_ref()
                .filter(|d| d.moved)
                .map(|_| edit.last_offset)
        };
        match offset {
            Some((dx, dy)) => self.drag_to(dx, dy, modifiers),
            None if alt_changed => self.render(),
            None => {}
        }
    }

    // ---------------------------------------------------------- nudges

    /// One press of a `layer.nudge-*` key (D-33).
    fn nudge(self: &Rc<Self>, id: &str) {
        let Some(step) = nudge_step(id) else {
            return;
        };
        let state = self.ctx.store.state();
        let Some(target) = edit_target(&state) else {
            return;
        };
        let Some(layer_id) = state.ui.selected_layer.clone() else {
            return;
        };
        let Some(mut layer) = scene_of(&state.project, &target)
            .and_then(|s| s.layer(&layer_id))
            .cloned()
        else {
            return;
        };
        if layer.locked {
            return;
        }
        let now = self.now_ms();
        let (preview, flushed, deadline) = {
            let mut edit = self.edit.borrow_mut();
            // A `LayerSet` still unanswered: go on from where it put the
            // layer (the newest one's, R23).
            layer = newest(&layer, &target, edit.pending.current());
            let (burst, flushed) = edit.nudges.press(&target, &layer, step, now);
            let preview = burst.preview();
            (preview, flushed, edit.nudges.deadline())
        };
        if let Some(cmd) = flushed {
            self.send_flushed(cmd);
        }
        let weak = Rc::downgrade(self);
        self.ctx.keys.set_drag_cancel(Some(Rc::new(move || {
            if let Some(this) = weak.upgrade() {
                this.cancel();
            }
        })));
        self.show_gesture(&state, Some(DragKind::LayerNudge), preview);
        self.arm_nudge_timer(deadline);
    }

    /// The `LayerSet` of a burst that another layer's press ended.
    fn send_flushed(self: &Rc<Self>, cmd: Command) {
        let weak = Rc::downgrade(self);
        let sent = cmd.clone();
        self.ctx.store.dispatch_with(cmd, false, move |reply| {
            if let Some(this) = weak.upgrade()
                && let Err(err) = &reply.result
                && !reply.late
                && let Some(notice) = error_notice(err, &sent, &this.ctx.store.state())
            {
                this.ctx.store.notify(notice);
            }
        });
    }

    fn arm_nudge_timer(self: &Rc<Self>, deadline: Option<u64>) {
        if let Some(timer) = self.nudge_timer.borrow_mut().take() {
            timer.remove();
        }
        let Some(at) = deadline else {
            return;
        };
        let wait = at.saturating_sub(self.now_ms()).max(1);
        let weak = Rc::downgrade(self);
        let id = crate::guard::timeout_local_once(
            Duration::from_millis(wait),
            "stage: a nudge burst's end",
            move || {
                if let Some(this) = weak.upgrade() {
                    this.nudge_timer.borrow_mut().take();
                    let now = this.now_ms();
                    this.end_burst(|nudges| nudges.due(now));
                    // Not due yet (a timer a millisecond early): the rest.
                    let deadline = this.edit.borrow().nudges.deadline();
                    if deadline.is_some() && this.nudge_timer.borrow().is_none() {
                        this.arm_nudge_timer(deadline);
                    }
                }
            },
        );
        *self.nudge_timer.borrow_mut() = Some(id);
    }

    fn key_released(self: &Rc<Self>, keyval: gdk::Key) {
        let arrow = keyval.name().is_some_and(|name| {
            matches!(
                name.as_str(),
                "Left" | "Right" | "Up" | "Down" | "KP_Left" | "KP_Right" | "KP_Up" | "KP_Down"
            )
        });
        if arrow {
            self.end_burst(NudgeCoalescer::release);
        }
    }

    /// Ends the burst through `end` (a release or the timeout): its one
    /// `LayerSet`, or nothing for a burst that came back where it began.
    fn end_burst(self: &Rc<Self>, end: impl FnOnce(&mut NudgeCoalescer) -> Option<Command>) {
        let (ended, cmd) = {
            let mut edit = self.edit.borrow_mut();
            let Some(preview) = edit.nudges.active().map(|b| b.preview()) else {
                return;
            };
            let cmd = end(&mut edit.nudges);
            if edit.nudges.active().is_some() {
                // Not due yet.
                return;
            }
            (preview, cmd)
        };
        if let Some(timer) = self.nudge_timer.borrow_mut().take() {
            timer.remove();
        }
        self.ctx.keys.set_drag_cancel(None);
        self.ctx.store.update_local(|l| l.drag = None);
        self.commit(cmd, ended);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangles_round_their_edges() {
        let r = rectangle(127.6, 63.5, 960.0, 540.2);
        assert_eq!((r.x(), r.y(), r.width(), r.height()), (128, 64, 960, 540));
        let empty = rectangle(10.0, 10.0, -5.0, 0.0);
        assert_eq!((empty.width(), empty.height()), (0, 0));
    }

    #[test]
    fn a_gesture_is_active_while_it_shows() {
        let mut edit = Editing::default();
        assert!(!edit.active());
        edit.pending.issue(LayerDragPreview {
            target: SceneTarget::Project,
            layer: LayerId::numbered(2),
            rect: qrec_core::model::Rect::new(0, 0, 16, 16),
            crop: qrec_core::model::Crop::default(),
        });
        assert!(edit.active());
        assert_eq!(edit.preview().map(|p| p.layer), Some(LayerId::numbered(2)));
    }
}
