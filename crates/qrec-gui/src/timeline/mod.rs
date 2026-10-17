//! The timeline (S01b–S12, C14–C18): toolbar, ruler, track headers, the
//! canvas of clips and lanes, the playhead overlay, gestures, the drop
//! target, the clip tooltip, zoom. Owner: WP3. Registers `zoom.in`,
//! `zoom.out`, `zoom.fit`, `zoom.selection`; writes `LocalUi.{drag,
//! viewport, fit_px_per_frame, hover_clip, trim_frame}`.
//!
//! ```text
//! region-timeline (TimelineRoot, 312 px; compact 232)
//! ├ timeline-toolbar (36)
//! └ Overlay
//!   ├ Box V
//!   │ ├ ruler (28): corner cell (176, the playhead's timecode) | ruler
//!   │ └ ScrolledWindow (the lanes scroll vertically, D-25)
//!   │   └ track-headers (176) | time-area: canvas | the empty state (S01b)
//!   │                                       └ video-row, lane-A1, … (named rows)
//!   ├ the overlay: playhead, scrub flag, snap guide (C18)
//!   └ the tooltip layer
//! ```
//!
//! - Everything drawn comes from `view::timeline` and `view::ruler` (pure):
//!   the canvas re-renders on a change of state, view or preview data and
//!   caches its render node meanwhile; the playhead's overlay redraws
//!   alone.
//! - The zoom and scroll ([`control::ViewModel`]) show at once and go out
//!   as one `SetUi` 300 ms after the last change; the gestures
//!   ([`control::Gestures`]) send one command per gesture (D-41).
//! - Thumbnails and waveforms come from the media hub's caches once the
//!   Session is open (`raised` and no waveform until they arrive); the
//!   silence hatches from the analysis cache (`PreviewRequest::
//!   CachedSilence`) keyed with the files' facts, stat'ed on `qrec-gui-io`.

pub mod control;
mod draw;
mod headers;
mod paint;
mod root;
mod surface;
mod tips;
mod toolbar;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, glib, graphene};
use qrec_core::ctx::FileFact;
use qrec_core::protocol::event::Interval;
use qrec_core::state::SilenceKey;
use qrec_core::view::DragKind;
use qrec_core::view::notices::unavailable_notice;
use qrec_core::view::timeline::{self as tl, ClipBox, HEADER_WIDTH, Lane, Rows, Viewport};
use qrec_core::{ClipId, Command, Frame, State, TakeId};
use qrec_shell::media::preview::{PreviewData, PreviewRequest};

use crate::store::local::TrimFrame;
use crate::store::{Change, Subscription};
use crate::theme::motion;
use crate::widgets::keycap::{CapSize, KeyCap};
use crate::{Ctx, guard, guarded, testctl};
use control::{Area, Button, Cx, Drag, Gestures, GhostClock, Mods, Press, Sink, ViewModel};
use draw::Scene;
use paint::Texts;
use root::{Column, TimelineRoot};
use surface::Surface;
use tips::Tips;
use toolbar::Toolbar;

/// The clip tooltip shows after the pointer rests this long (at once in
/// test mode).
const HOVER_DELAY: Duration = Duration::from_millis(500);
/// The snap guide flashes this long (G§4.5).
const FLASH: Duration = Duration::from_millis(300);

/// The timeline region (`region-timeline`).
pub struct Timeline {
    inner: Rc<Inner>,
}

impl Timeline {
    pub fn new(ctx: &Ctx) -> Self {
        Self {
            inner: Inner::new(ctx),
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.inner.root.clone().upcast()
    }
}

/// What the timeline knows besides the state.
struct Model {
    vm: ViewModel,
    g: Gestures,
    rows: Rows,
    lanes: Vec<Lane>,
    ghost: GhostClock,
    /// The facts of the silence files, and the cached analyses.
    files: BTreeMap<String, FileFact>,
    cached: BTreeMap<SilenceKey, Vec<Interval>>,
    /// Bumps when `files` or `cached` change.
    silence_version: u64,
    /// The silence spans drawn: for which state and `silence_version`.
    silences: Option<(Arc<State>, u64, Rc<Vec<tl::SilenceSpan>>)>,
    /// What the facts were asked for (files, threshold, min silence).
    silence_key: Option<(Vec<String>, u64, u32)>,
    /// The CachedSilence requests sent, per file version.
    silence_asked: Vec<(String, i64, u64)>,
    /// A bin drag over the canvas: its take and x.
    drop: Option<(TakeId, f64)>,
    /// The drop target set `LocalUi.drag = BinInsert` (and clears it):
    /// a drag the bin's source (WP7) set is left alone.
    drop_drag: bool,
    /// The snap guide's x and when it started.
    flash: Option<(f64, Duration)>,
    /// The pointer over the rows (canvas px, 100 %).
    pointer: Option<(f64, f64)>,
    /// The clip whose tooltip is up.
    tip_clip: Option<ClipId>,
    /// The last trim snap target (a new one flashes).
    snapped: Option<f64>,
    /// The canvas's cached render node and what it shows: its generation,
    /// size and thumbnail height (a new scale factor makes new textures).
    cache: Option<(u64, (i32, i32, u32), gtk::gsk::RenderNode)>,
    /// Bumps on every change of what the canvas shows.
    generation: u64,
    /// The thumbnail and peak caches are followed.
    caches_hooked: bool,
    /// What `LocalUi.viewport` says.
    local_view: Option<(Viewport, f64)>,
    /// The move drag's neighbours easing to their places (G§4.5).
    gap: BTreeMap<ClipId, (f64, f64, Duration)>,
    /// The scrolled-to lane offset (D-25) before a pan.
    opened: bool,
}

struct Inner {
    ctx: Ctx,
    me: Weak<Inner>,
    root: TimelineRoot,
    toolbar: Toolbar,
    headers: headers::Headers,
    ruler_row: gtk::Box,
    corner: gtk::Label,
    ruler: Surface,
    scroller: gtk::ScrolledWindow,
    stack: gtk::Stack,
    canvas: Surface,
    rows_box: gtk::Box,
    overlay: Surface,
    tips: Tips,
    empty_key: KeyCap,
    /// "Press" (or "Click Record" without a record key) before the cap.
    empty_before: gtk::Label,
    /// The quiet swamp and its fixed-width column (smaller in the compact
    /// rows).
    empty_art: Option<(Column, gtk::Picture)>,
    texts: Texts,
    started: Instant,
    m: RefCell<Model>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    flush_source: RefCell<Option<glib::SourceId>>,
    hover_source: RefCell<Option<glib::SourceId>>,
    subs: RefCell<Vec<Subscription>>,
    /// The project folder (the silence files' facts).
    project: PathBuf,
    /// The lane offset a middle drag started from (widget px): the pan
    /// sets it absolutely, from there (`Sink::pan_lanes`).
    pan_from: Cell<Option<f64>>,
    /// The frame-time probe in progress (G§8.6, `QREC_GUI_PROBE=timeline`).
    probe: RefCell<Option<Probe>>,
}

/// The frame-time probe (G§8.6): 4 s of scripted zooming and scrolling on
/// the frame clock, its frame intervals logged as p50 and p99.
#[derive(Debug, Default)]
struct Probe {
    frame: u32,
    last_us: Option<i64>,
    intervals_ms: Vec<f64>,
    /// The canvas's snapshot times (its render node recorded).
    paint_ms: Vec<f64>,
}

/// The probe's length in frames: 2 s of zoom in and out, 2 s of scrolling.
const PROBE_FRAMES: u32 = 240;

/// Whether `QREC_GUI_PROBE` asks for the timeline's probe.
fn probing() -> bool {
    std::env::var("QREC_GUI_PROBE").is_ok_and(|v| v.split(',').any(|p| p.trim() == "timeline"))
}

/// The p-th percentile of sorted `v`.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

impl Inner {
    fn new(ctx: &Ctx) -> Rc<Inner> {
        let reduced = ctx.theme.motion().reduced || testctl::mode().on;
        let inner = Rc::new_cyclic(|me: &Weak<Inner>| {
            let slider_me = me.clone();
            let toolbar = Toolbar::new(ctx, move |s| {
                if let Some(inner) = slider_me.upgrade() {
                    inner.on_slider(s);
                }
            });
            let root = TimelineRoot::new();
            let headers = headers::Headers::new(ctx);

            let ruler_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            ruler_row.set_widget_name("ruler");
            ruler_row.add_css_class("tl-ruler-row");
            let corner = gtk::Label::new(None);
            corner.add_css_class("tl-corner");
            corner.add_css_class("t-mono");
            corner.set_xalign(0.0);
            corner.set_valign(gtk::Align::Start);
            corner.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let header_px = (HEADER_WIDTH * ctx.theme.scale()).round() as i32;
            let corner_col = Column::new(&corner, header_px);
            let ruler = Surface::new();
            ruler.add_css_class("tl-ruler");
            ruler.set_hexpand(true);
            ruler_row.append(&corner_col);
            ruler_row.append(&ruler);

            let canvas = Surface::new();
            canvas.add_css_class("tl-canvas");
            canvas.set_hexpand(true);
            canvas.set_vexpand(true);
            canvas.set_focusable(true);
            canvas.set_focus_on_click(true);
            canvas.update_property(&[gtk::accessible::Property::Label("Timeline clips")]);
            let rows_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
            rows_box.set_valign(gtk::Align::Start);
            rows_box.set_can_target(false);
            let canvas_layer = gtk::Overlay::new();
            canvas_layer.set_child(Some(&canvas));
            canvas_layer.add_overlay(&rows_box);
            canvas_layer.set_clip_overlay(&rows_box, true);

            let empty_key = KeyCap::new("R");
            empty_key.set_size(CapSize::Lg);
            let (empty, empty_before, empty_art) = empty_state(ctx, &empty_key);
            // Not homogeneous: the hidden page must not size the rows.
            let stack = gtk::Stack::new();
            stack.set_hhomogeneous(false);
            stack.set_vhomogeneous(false);
            stack.add_named(&canvas_layer, Some("canvas"));
            stack.add_named(&empty, Some("empty"));
            stack.set_visible_child_name("canvas");
            stack.set_hexpand(true);
            stack.set_vexpand(true);
            let area = gtk::Box::new(gtk::Orientation::Vertical, 0);
            area.set_widget_name("time-area");
            area.set_hexpand(true);
            area.set_vexpand(true);
            area.append(&stack);

            let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let headers_col = Column::new(&headers.root, header_px);
            content.append(&headers_col);
            content.append(&area);
            let scroller = gtk::ScrolledWindow::new();
            scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            scroller.set_vexpand(true);
            scroller.set_child(Some(&content));
            scroller.add_css_class("tl-scroller");

            let stack_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
            stack_box.append(&ruler_row);
            stack_box.append(&scroller);
            let overlay = Surface::new();
            overlay.set_can_target(false);
            overlay.set_hexpand(true);
            overlay.set_vexpand(true);
            let tips = Tips::new();
            let body = gtk::Overlay::new();
            body.set_vexpand(true);
            body.set_child(Some(&stack_box));
            body.add_overlay(&overlay);
            body.set_clip_overlay(&overlay, true);
            body.add_overlay(&tips.layer);
            body.set_clip_overlay(&tips.layer, true);

            root.append(&toolbar.root);
            root.append(&body);
            root.set_hexpand(true);
            root.set_vexpand(false);
            root.set_focus_target(&canvas);

            Inner {
                ctx: ctx.clone(),
                me: me.clone(),
                root,
                toolbar,
                headers,
                ruler_row,
                corner,
                ruler,
                scroller,
                stack,
                canvas,
                rows_box,
                overlay,
                tips,
                empty_key,
                empty_before,
                empty_art,
                texts: Texts::default(),
                started: Instant::now(),
                m: RefCell::new(Model {
                    vm: ViewModel::new(reduced),
                    g: Gestures::new(),
                    rows: tl::ROWS,
                    lanes: Vec::new(),
                    ghost: GhostClock::default(),
                    files: BTreeMap::new(),
                    cached: BTreeMap::new(),
                    silence_version: 0,
                    silences: None,
                    silence_key: None,
                    silence_asked: Vec::new(),
                    drop: None,
                    drop_drag: false,
                    flash: None,
                    pointer: None,
                    tip_clip: None,
                    snapped: None,
                    cache: None,
                    generation: 0,
                    caches_hooked: false,
                    local_view: None,
                    gap: BTreeMap::new(),
                    opened: false,
                }),
                tick: RefCell::new(None),
                flush_source: RefCell::new(None),
                hover_source: RefCell::new(None),
                subs: RefCell::new(Vec::new()),
                project: ctx.store.project_dir().to_path_buf(),
                pan_from: Cell::new(None),
                probe: RefCell::new(None),
            }
        });
        inner.connect();
        inner.refresh_all();
        inner
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    /// The UI scale (D-46): view geometry × it is widget px.
    fn scale(&self) -> f64 {
        self.ctx.theme.scale()
    }

    /// The thumbnails' height in device px (G§3.11).
    fn thumb_px(&self) -> u32 {
        control::thumb_px(self.scale(), self.canvas.scale_factor())
    }

    // ------------------------------------------------------- wiring

    fn connect(&self) {
        let me = self.me.clone();
        let mut subs = Vec::new();
        subs.push(self.ctx.store.subscribe({
            let me = me.clone();
            move |change| {
                if let Some(inner) = me.upgrade() {
                    inner.on_change(change);
                }
            }
        }));
        subs.push(self.ctx.store.subscribe_local({
            let me = me.clone();
            move |local| {
                if let Some(inner) = me.upgrade() {
                    inner.on_local(local);
                }
            }
        }));
        subs.push(self.ctx.theme.subscribe({
            let me = me.clone();
            move |_| {
                if let Some(inner) = me.upgrade() {
                    // New colors or a new scale: the cached canvas goes.
                    inner.texts.clear();
                    inner.bump();
                    inner.refresh_all();
                }
            }
        }));
        // Zooms ease unless motion is reduced (or in test mode), live.
        subs.push(self.ctx.theme.follow_motion({
            let me = me.clone();
            move |motion| {
                if let Some(inner) = me.upgrade() {
                    let instant = motion.reduced || testctl::mode().on;
                    inner.m.borrow_mut().vm.set_instant(instant);
                }
            }
        }));
        subs.push(self.ctx.media.subscribe_preview({
            let me = me.clone();
            move |data| {
                if let Some(inner) = me.upgrade() {
                    inner.on_preview(data);
                }
            }
        }));
        *self.subs.borrow_mut() = subs;

        let draw_me = me.clone();
        self.canvas.set_draw(move |s, w, h| {
            if let Some(inner) = draw_me.upgrade() {
                inner.draw_canvas(s, w, h);
            }
        });
        let resize_me = me.clone();
        self.canvas.set_on_resize(move |w, _| {
            if let Some(inner) = resize_me.upgrade() {
                inner.on_resize(w);
            }
        });
        let ruler_me = me.clone();
        self.ruler.set_draw(move |s, w, h| {
            if let Some(inner) = ruler_me.upgrade() {
                inner.draw_ruler(s, w, h);
            }
        });
        let overlay_me = me.clone();
        self.overlay.set_draw(move |s, w, h| {
            if let Some(inner) = overlay_me.upgrade() {
                inner.draw_overlay(s, w, h);
            }
        });
        self.connect_canvas_input();
        self.connect_ruler_input();
        self.connect_drop();
        self.register_actions();
        let adj = self.scroller.vadjustment();
        let scroll_me = me.clone();
        adj.connect_value_changed(guarded!("timeline: the lanes scroll", move |_| {
            if let Some(inner) = scroll_me.upgrade() {
                inner.overlay.queue_draw();
            }
        }));
    }

    fn register_actions(&self) {
        for (id, zoom_in) in [("zoom.in", true), ("zoom.out", false)] {
            let me = self.me.clone();
            self.ctx.actions.register(
                id,
                Rc::new(move || {
                    if let Some(inner) = me.upgrade() {
                        inner.zoom_step(zoom_in);
                    }
                }),
            );
        }
        let me = self.me.clone();
        self.ctx.actions.register(
            "zoom.fit",
            Rc::new(move || {
                if let Some(inner) = me.upgrade() {
                    inner.zoom_fit();
                }
            }),
        );
        let me = self.me.clone();
        self.ctx.actions.register(
            "zoom.selection",
            Rc::new(move || {
                if let Some(inner) = me.upgrade() {
                    inner.zoom_selection();
                }
            }),
        );
        if probing() {
            let me = self.me.clone();
            self.ctx.actions.register(
                "timeline.probe",
                Rc::new(move || {
                    if let Some(inner) = me.upgrade() {
                        inner.start_probe();
                    }
                }),
            );
        }
    }

    /// Starts the frame-time probe (G§8.6).
    fn start_probe(&self) {
        if self.probe.borrow().is_some() {
            return;
        }
        *self.probe.borrow_mut() = Some(Probe::default());
        tracing::info!("timeline probe: started");
        let me = self.me.clone();
        guard::tick(
            &self.canvas,
            "timeline: the probe",
            move |_, clock| match me.upgrade() {
                Some(inner) if inner.probe_frame(clock.frame_time()) => glib::ControlFlow::Continue,
                _ => glib::ControlFlow::Break,
            },
        );
    }

    /// One probe frame at frame time `us`: zoom in and out (log, at the
    /// center), then scroll end to end; `false` when done (logged).
    fn probe_frame(&self, us: i64) -> bool {
        let now = self.now();
        let (frame, done) = {
            let mut probe = self.probe.borrow_mut();
            let Some(p) = probe.as_mut() else {
                return false;
            };
            if let Some(last) = p.last_us {
                p.intervals_ms.push((us - last) as f64 / 1000.0);
            }
            p.last_us = Some(us);
            p.frame += 1;
            (p.frame, p.frame > PROBE_FRAMES)
        };
        if done {
            let probe = self.probe.borrow_mut().take().unwrap_or_default();
            let mut sorted = probe.intervals_ms.clone();
            sorted.sort_by(f64::total_cmp);
            let mut paints = probe.paint_ms.clone();
            paints.sort_by(f64::total_cmp);
            let slow: Vec<String> = probe
                .intervals_ms
                .iter()
                .enumerate()
                .filter(|(_, ms)| **ms > 20.0)
                .map(|(i, ms)| format!("{}:{ms:.0}", i + 1))
                .collect();
            tracing::info!(
                "timeline probe: {} frames, p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms; \
                 canvas snapshots {}: p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms; \
                 intervals over 20 ms (frame:ms) [{}]",
                sorted.len(),
                percentile(&sorted, 0.5),
                percentile(&sorted, 0.99),
                sorted.last().copied().unwrap_or_default(),
                paints.len(),
                percentile(&paints, 0.5),
                percentile(&paints, 0.99),
                paints.last().copied().unwrap_or_default(),
                slow.join(" ")
            );
            self.after_view_change();
            return false;
        }
        let (view, fit, total) = {
            let m = self.m.borrow();
            (m.vm.target_view(), m.vm.fit(), m.vm.total())
        };
        let (Some(view), Some(fit)) = (view, fit) else {
            return true;
        };
        let half = PROBE_FRAMES / 2;
        let next = if frame <= half {
            let t = f64::from(frame) / f64::from(half);
            let s = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * t).cos();
            view.with_zoom(tl::slider_zoom(s * 0.8, fit), view.width / 2.0, fit)
        } else {
            let at = Viewport {
                px_per_frame: tl::clamp_zoom(1.6, fit),
                ..view
            };
            let t = f64::from(frame - half) / f64::from(half);
            Viewport {
                scroll_px: at.max_scroll(total) * t,
                ..at
            }
        };
        self.m.borrow_mut().vm.set_view(next, now, false);
        self.bump();
        self.canvas.queue_draw();
        self.ruler.queue_draw();
        self.overlay.queue_draw();
        true
    }

    fn connect_canvas_input(&self) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        let me = self.me.clone();
        drag.connect_drag_begin(guarded!("timeline: press", move |g, x, y| {
            if let Some(inner) = me.upgrade() {
                let button = g.current_button();
                let mods = mods_of(g.current_event_state());
                if inner.press(Area::Rows, button, x, y, mods) {
                    g.set_state(gtk::EventSequenceState::Claimed);
                } else {
                    g.set_state(gtk::EventSequenceState::Denied);
                }
            }
        }));
        let me = self.me.clone();
        drag.connect_drag_update(guarded!("timeline: drag", move |g, dx, dy| {
            if let Some(inner) = me.upgrade()
                && let Some((x0, y0)) = g.start_point()
            {
                inner.motion(Area::Rows, x0 + dx, y0 + dy);
            }
        }));
        let me = self.me.clone();
        drag.connect_drag_end(guarded!("timeline: release", move |g, dx, dy| {
            if let Some(inner) = me.upgrade()
                && let Some((x0, y0)) = g.start_point()
            {
                inner.release(x0 + dx, y0 + dy);
            }
        }));
        // GTK gave up the press (a grab, the widget unmapped): no release.
        let me = self.me.clone();
        drag.connect_cancel(guarded!("timeline: a press cancelled", move |_, _| {
            if let Some(inner) = me.upgrade() {
                inner.cancel_drag();
            }
        }));
        self.canvas.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        let me = self.me.clone();
        motion.connect_motion(guarded!("timeline: hover", move |_, x, y| {
            if let Some(inner) = me.upgrade() {
                inner.hover(x, y);
            }
        }));
        let me = self.me.clone();
        motion.connect_leave(guarded!("timeline: leave", move |_| {
            if let Some(inner) = me.upgrade() {
                inner.leave();
            }
        }));
        self.canvas.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::DISCRETE,
        );
        let me = self.me.clone();
        scroll.connect_scroll(guarded!(
            "timeline: wheel",
            after_panic = glib::Propagation::Proceed,
            move |c, dx, dy| {
                match me.upgrade() {
                    Some(inner) if inner.wheel(c, dx, dy) => glib::Propagation::Stop,
                    _ => glib::Propagation::Proceed,
                }
            }
        ));
        self.canvas.add_controller(scroll);
    }

    fn connect_ruler_input(&self) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let me = self.me.clone();
        drag.connect_drag_begin(guarded!("timeline: ruler press", move |g, x, y| {
            if let Some(inner) = me.upgrade() {
                let mods = mods_of(g.current_event_state());
                if inner.press(Area::Ruler, gdk::BUTTON_PRIMARY, x, y, mods) {
                    g.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        }));
        let me = self.me.clone();
        drag.connect_drag_update(guarded!("timeline: scrub", move |g, dx, dy| {
            if let Some(inner) = me.upgrade()
                && let Some((x0, y0)) = g.start_point()
            {
                inner.motion(Area::Ruler, x0 + dx, y0 + dy);
            }
        }));
        let me = self.me.clone();
        drag.connect_drag_end(guarded!("timeline: ruler release", move |g, dx, dy| {
            if let Some(inner) = me.upgrade()
                && let Some((x0, y0)) = g.start_point()
            {
                inner.release(x0 + dx, y0 + dy);
            }
        }));
        let me = self.me.clone();
        drag.connect_cancel(guarded!(
            "timeline: a ruler press cancelled",
            move |_, _| {
                if let Some(inner) = me.upgrade() {
                    inner.cancel_drag();
                }
            }
        ));
        self.ruler.add_controller(drag);
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::DISCRETE,
        );
        let me = self.me.clone();
        scroll.connect_scroll(guarded!(
            "timeline: ruler wheel",
            after_panic = glib::Propagation::Proceed,
            move |c, dx, dy| {
                match me.upgrade() {
                    Some(inner) if inner.wheel(c, dx, dy) => glib::Propagation::Stop,
                    _ => glib::Propagation::Proceed,
                }
            }
        ));
        self.ruler.add_controller(scroll);
    }

    /// D-38: bin rows drag `qrec-take:<id>` strings onto the canvas.
    fn connect_drop(&self) {
        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::COPY);
        // The take is read while it hovers (the caret, the tooltip): a
        // Wayland drag's data is only there once loaded (WP7 found it).
        target.set_preload(true);
        let me = self.me.clone();
        target.connect_motion(guarded!(
            "timeline: a drop over",
            after_panic = gdk::DragAction::empty(),
            move |t, x, _y| {
                let Some(inner) = me.upgrade() else {
                    return gdk::DragAction::empty();
                };
                let take = t
                    .value()
                    .and_then(|v| v.get::<String>().ok())
                    .and_then(|s| control::parse_drop(&s));
                inner.drop_over(take, x)
            }
        ));
        let me = self.me.clone();
        target.connect_leave(guarded!("timeline: a drop leaves", move |_| {
            if let Some(inner) = me.upgrade() {
                inner.drop_over(None, 0.0);
            }
        }));
        let me = self.me.clone();
        target.connect_drop(guarded!(
            "timeline: a drop",
            after_panic = false,
            move |_, value, x, _y| {
                let Some(inner) = me.upgrade() else {
                    return false;
                };
                let take = value
                    .get::<String>()
                    .ok()
                    .and_then(|s| control::parse_drop(&s));
                inner.drop_at(take, x)
            }
        ));
        self.canvas.add_controller(target);
    }

    // ------------------------------------------------------- state

    fn on_change(&self, change: &Change) {
        let state = &change.next;
        let now = self.now();
        let mut pending = Pending::default();
        {
            let mut m = self.m.borrow_mut();
            // The record clock, for the ghost's growth.
            let grows = m.ghost.follow(&state.runtime.record, now);
            let mut total = tl::shown_total(state, m.ghost.elapsed_ns);
            if grows {
                // While a take records its ghost grows on the frame clock
                // (`follow_ghost`, at once): a clock past it too, and one
                // behind it never shrinks it.
                m.vm.follow_ghost(total);
                total = m.vm.total();
            }
            m.vm.on_state(state, total, now);
            let has_view = m.vm.target_view().is_some();
            m.g.on_state(state, has_view, &mut pending);
            m.lanes = tl::lanes(state);
            m.generation += 1;
        }
        self.apply(pending);
        if change.timeline() || change.scene() || change.takes() || change.ui() {
            self.refresh_silences(state);
        }
        self.refresh_all();
    }

    fn on_local(&self, local: &crate::store::LocalUi) {
        let rows = Rows::for_layout(local.layout.compact);
        let opened = local.is_open();
        let (rows_changed, opened_now) = {
            let mut m = self.m.borrow_mut();
            let changed = m.rows != rows;
            m.rows = rows;
            let opened_now = opened && !m.opened;
            m.opened = opened;
            (changed, opened_now)
        };
        if rows_changed {
            self.bump();
            self.refresh_all();
        }
        if opened_now {
            // The media hub makes its caches right after the window is told
            // the Session opened.
            let me = self.me.clone();
            guard::idle_local_once("timeline: the caches", move || {
                if let Some(inner) = me.upgrade() {
                    inner.hook_caches();
                    let state = inner.ctx.store.state();
                    inner.refresh_silences(&state);
                    inner.refresh_all();
                }
            });
        }
        // The autocut page's preview (WP5) and the playing player redraw.
        self.bump();
        self.canvas.queue_draw();
        self.overlay.queue_draw();
        self.ensure_tick();
    }

    fn on_preview(&self, data: &PreviewData) {
        if let PreviewData::Silence {
            key,
            intervals: Some(intervals),
            ..
        } = data
        {
            {
                let mut m = self.m.borrow_mut();
                m.cached.insert(key.clone(), intervals.clone());
                m.silence_version += 1;
            }
            self.bump();
            self.canvas.queue_draw();
        }
    }

    /// Follows the thumbnail and peak caches (they exist once the Session is
    /// open): an arrival redraws.
    fn hook_caches(&self) {
        if self.m.borrow().caches_hooked {
            return;
        }
        let (Some(thumbs), Some(peaks)) = (self.ctx.media.thumbs(), self.ctx.media.peaks()) else {
            return;
        };
        self.m.borrow_mut().caches_hooked = true;
        let me = self.me.clone();
        let a = thumbs.subscribe(move |_| {
            if let Some(inner) = me.upgrade() {
                inner.bump();
                inner.canvas.queue_draw();
            }
        });
        let me = self.me.clone();
        let b = peaks.subscribe(move |_| {
            if let Some(inner) = me.upgrade() {
                inner.bump();
                inner.canvas.queue_draw();
            }
        });
        self.subs.borrow_mut().extend([a, b]);
    }

    /// The silence files' facts (on `qrec-gui-io`), then their cached
    /// analyses at the project's threshold and minimum silence.
    fn refresh_silences(&self, state: &State) {
        let files = tl::silence_files(state);
        let settings = &state.project.autocut;
        let key = (
            files.clone(),
            settings.threshold_db.to_bits(),
            settings.min_silence_ms,
        );
        {
            let mut m = self.m.borrow_mut();
            if m.silence_key.as_ref() == Some(&key) {
                drop(m);
                self.ask_silences(state);
                return;
            }
            m.silence_key = Some(key);
        }
        if files.is_empty() {
            return;
        }
        let paths = qrec_shell::paths::ProjectPaths::new(self.project.clone());
        let me = self.me.clone();
        self.ctx.media.run_io_then(
            move || {
                files
                    .iter()
                    .filter_map(|f| qrec_shell::facts::file_fact(&paths, f).ok())
                    .map(|fact| (fact.path.clone(), fact))
                    .collect::<BTreeMap<_, _>>()
            },
            move |facts| {
                if let Some(inner) = me.upgrade() {
                    {
                        let mut m = inner.m.borrow_mut();
                        m.files = facts;
                        m.silence_version += 1;
                    }
                    let state = inner.ctx.store.state();
                    inner.ask_silences(&state);
                    inner.bump();
                    inner.canvas.queue_draw();
                }
            },
        );
    }

    /// Asks the preview cache for each file's cached analysis (once per
    /// file version and settings).
    fn ask_silences(&self, state: &State) {
        let Some(cache) = self.ctx.media.preview_cache() else {
            return;
        };
        let settings = &state.project.autocut;
        let wanted: Vec<(String, i64, u64)> = {
            let m = self.m.borrow();
            m.files
                .values()
                .map(|f| (f.path.clone(), f.mtime_ns, settings.threshold_db.to_bits()))
                .filter(|w| !m.silence_asked.contains(w))
                .collect()
        };
        for (file, mtime, threshold) in wanted {
            cache.request(PreviewRequest::CachedSilence {
                file: file.clone(),
                threshold_db: settings.threshold_db,
                min_silence_ms: settings.min_silence_ms,
            });
            self.m
                .borrow_mut()
                .silence_asked
                .push((file, mtime, threshold));
        }
    }

    fn on_resize(&self, width: i32) {
        let w = f64::from(width) / self.scale();
        self.m.borrow_mut().vm.set_width(w);
        self.bump();
        self.refresh_all();
    }

    /// Something the canvas shows changed.
    fn bump(&self) {
        self.m.borrow_mut().generation += 1;
    }

    /// Re-renders everything from the newest state.
    fn refresh_all(&self) {
        let state = self.ctx.store.state();
        let now = self.now();
        let (rows, view, total, lanes) = {
            let m = self.m.borrow();
            (m.rows, m.vm.target_view(), m.vm.total(), m.lanes.len())
        };
        let opened = self.ctx.store.local().is_open();
        let empty = state.project.timeline.clips.is_empty() && state.runtime.record.is_idle();
        self.headers.update(&state, rows, self.scale());
        self.toolbar
            .update(&tl::toolbar(&state, view.as_ref(), total));
        let page = if empty && opened { "empty" } else { "canvas" };
        if self.stack.visible_child_name().as_deref() != Some(page) {
            self.stack.set_visible_child_name(page);
        }
        self.ruler_row.set_visible(!empty);
        self.overlay.set_visible(!empty);
        self.ruler_row
            .set_size_request(-1, (rows.ruler * self.scale()) as i32);
        self.update_rows(rows);
        let content = rows.content_height(lanes) * self.scale();
        self.canvas.set_natural_size(0, content.ceil() as i32);
        let playhead = self.shown_playhead(&state, now);
        self.corner.set_text(&qrec_core::view::ruler::corner(
            playhead,
            state.project.canvas.fps,
        ));
        show_empty_copy(
            &tl::empty_state(&self.ctx.store.keymap()),
            &self.empty_before,
            &self.empty_key,
        );
        if let Some((column, art)) = &self.empty_art {
            let (w, h) = empty_art_size(rows);
            column.set_width(w);
            art.set_size_request(w, h);
        }
        self.sync_local_view();
        self.canvas.queue_draw();
        self.ruler.queue_draw();
        self.overlay.queue_draw();
        self.ensure_tick();
    }

    /// The named rows (`video-row`, `lane-A1`, …) at the rows' heights.
    fn update_rows(&self, rows: Rows) {
        let lanes = self.m.borrow().lanes.clone();
        let names: Vec<String> = std::iter::once("video-row".to_owned())
            .chain(lanes.iter().map(|l| format!("lane-{}", l.track)))
            .collect();
        let s = self.scale();
        let mut child = self.rows_box.first_child();
        let mut same = true;
        for name in &names {
            match &child {
                Some(c) if c.widget_name() == name.as_str() => child = c.next_sibling(),
                _ => {
                    same = false;
                    break;
                }
            }
        }
        same &= child.is_none();
        if !same {
            while let Some(c) = self.rows_box.first_child() {
                self.rows_box.remove(&c);
            }
            for name in &names {
                let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
                row.set_widget_name(name);
                row.set_can_target(false);
                self.rows_box.append(&row);
            }
        }
        // The headers' whole px (`Rows::px_heights`): each row at its
        // rounded top on the canvas, a lane's gap as its top margin.
        let (video, lane_px) = rows.px_heights(lanes.len(), s);
        let mut c = self.rows_box.first_child();
        let mut heights = std::iter::once((0, video)).chain(lane_px);
        while let Some(row) = c {
            let (gap, h) = heights.next().unwrap_or((0, 0));
            row.set_margin_top(gap);
            row.set_size_request(-1, h);
            c = row.next_sibling();
        }
    }

    /// `LocalUi.viewport` follows the target view (not each eased frame);
    /// its scroll once the scrolling stops.
    fn sync_local_view(&self) {
        let (target, fit, dirty) = {
            let m = self.m.borrow();
            (m.vm.target_view(), m.vm.fit(), m.vm.dirty())
        };
        let (Some(view), Some(fit)) = (target, fit) else {
            return;
        };
        let last = self.m.borrow().local_view;
        let changed = match last {
            None => true,
            Some((v, f)) => {
                (v.px_per_frame - view.px_per_frame).abs() > 1e-12
                    || (v.width - view.width).abs() > 1e-9
                    || (f - fit).abs() > 1e-12
                    || (!dirty && (v.scroll_px - view.scroll_px).abs() > 0.5)
            }
        };
        if changed {
            self.m.borrow_mut().local_view = Some((view, fit));
            self.ctx.store.update_local(move |l| {
                l.viewport = view;
                l.fit_px_per_frame = fit;
            });
        }
    }

    // ------------------------------------------------------- input

    /// The view as shown now, and the gestures' context.
    fn with_cx<R>(&self, f: impl FnOnce(&Cx<'_>, &mut Model, &mut Pending) -> R) -> Option<R> {
        let state = self.ctx.store.state();
        let now = self.now();
        let mut pending = Pending::default();
        let result = {
            let mut m = self.m.borrow_mut();
            let view = m.vm.shown(now)?;
            let playhead = self.shown_playhead_with(&state, &m, now);
            let cx = Cx {
                state: &state,
                view,
                total: m.vm.total(),
                rows: m.rows,
                lanes: m.lanes.len(),
                playhead,
                now,
                rows_top: self.rows_top(),
            };
            f(&cx, &mut m, &mut pending)
        };
        self.apply(pending);
        Some(result)
    }

    /// The rows' top in the lanes' scrolled window (100 % px) as the canvas
    /// is laid out now: the transform GTK gave this event's coordinates.
    fn rows_top(&self) -> f64 {
        let origin = graphene::Point::new(0.0, 0.0);
        self.canvas
            .compute_point(&self.scroller, &origin)
            .map_or(0.0, |p| f64::from(p.y()))
            / self.scale()
    }

    fn press(&self, area: Area, button: u32, x: f64, y: f64, mods: Mods) -> bool {
        let button = match button {
            gdk::BUTTON_PRIMARY => Button::Left,
            gdk::BUTTON_MIDDLE => Button::Middle,
            _ => return false,
        };
        if area == Area::Rows && button == Button::Left {
            self.canvas.grab_focus();
        }
        self.cancel_hover_tip();
        self.tips.hide();
        let s = self.scale();
        let used = self
            .with_cx(|cx, m, pending| {
                let Model { g, vm, .. } = m;
                g.decide_press(
                    cx,
                    pending,
                    vm,
                    Press {
                        area,
                        button,
                        x: x / s,
                        y: y / s,
                        mods,
                    },
                )
            })
            .unwrap_or(false);
        if button == Button::Middle && used {
            self.pan_from.set(Some(self.scroller.vadjustment().value()));
        }
        self.after_input();
        used
    }

    fn motion(&self, area: Area, x: f64, y: f64) {
        let s = self.scale();
        let (x, y) = (x / s, y / s);
        self.with_cx(|cx, m, pending| {
            let Model { g, vm, .. } = m;
            g.decide_motion(cx, pending, vm, area, (x, y));
        });
        self.m.borrow_mut().pointer = Some((x, y));
        self.after_input();
    }

    fn release(&self, x: f64, y: f64) {
        let s = self.scale();
        let (x, y) = (x / s, y / s);
        self.with_cx(|cx, m, pending| m.g.decide_release(cx, pending, (x, y)));
        self.pan_from.set(None);
        {
            let mut m = self.m.borrow_mut();
            m.snapped = None;
            m.gap.clear();
        }
        self.tips.hide();
        self.after_input();
    }

    /// `Esc` during a drag (D-42), through the key router.
    fn cancel_drag(&self) {
        let mut pending = Pending::default();
        self.m.borrow_mut().g.decide_cancel(&mut pending);
        self.apply(pending);
        {
            let mut m = self.m.borrow_mut();
            m.snapped = None;
            m.gap.clear();
        }
        self.tips.hide();
        self.after_input();
    }

    fn hover(&self, x: f64, y: f64) {
        let s = self.scale();
        let (x, y) = (x / s, y / s);
        if self.m.borrow().g.drag().is_some() {
            return;
        }
        self.with_cx(|cx, m, pending| {
            let Model { g, vm, .. } = m;
            g.decide_motion(cx, pending, vm, Area::Rows, (x, y));
        });
        self.m.borrow_mut().pointer = Some((x, y));
        self.update_cursor(x, y);
        self.schedule_hover_tip();
    }

    fn leave(&self) {
        let mut pending = Pending::default();
        self.m.borrow_mut().g.leave(&mut pending);
        self.m.borrow_mut().pointer = None;
        self.apply(pending);
        self.cancel_hover_tip();
        if self.m.borrow().g.drag().is_none() {
            self.tips.hide();
            self.m.borrow_mut().tip_clip = None;
        }
    }

    /// A wheel event (D-39): plain scrolls horizontally, `Shift` scrolls the
    /// lanes, `Ctrl` zooms at the pointer.
    fn wheel(&self, c: &gtk::EventControllerScroll, dx: f64, dy: f64) -> bool {
        let state = c.current_event_state();
        let notches = match c.unit() {
            gdk::ScrollUnit::Wheel => (dx, dy),
            _ => (dx / tl::WHEEL_NOTCH_PX, dy / tl::WHEEL_NOTCH_PX),
        };
        let now = self.now();
        if state.contains(gdk::ModifierType::SHIFT_MASK) {
            let adj = self.scroller.vadjustment();
            adj.set_value(adj.value() + (notches.0 + notches.1) * 40.0 * self.scale());
            return true;
        }
        if state.contains(gdk::ModifierType::CONTROL_MASK) {
            // At the pointer, from the event itself: over the ruler nothing
            // records the pointer, and a stale x could be left from a scrub.
            let anchor = event_x(c)
                .map(|x| x / self.scale())
                .or_else(|| self.m.borrow().pointer.map(|(x, _)| x))
                .unwrap_or_else(|| self.m.borrow().vm.width() / 2.0);
            self.m.borrow_mut().vm.zoom_wheel(notches.1, anchor, now);
        } else {
            self.m
                .borrow_mut()
                .vm
                .scroll_wheel(notches.0, notches.1, now);
        }
        self.tips.hide();
        self.after_view_change();
        true
    }

    fn on_slider(&self, s: f64) {
        let state = self.ctx.store.state();
        let now = self.now();
        let playhead = self.shown_playhead(&state, now);
        self.m.borrow_mut().vm.zoom_slider(s, playhead, now);
        self.after_view_change();
    }

    fn zoom_step(&self, zoom_in: bool) {
        let state = self.ctx.store.state();
        let now = self.now();
        let playhead = self.shown_playhead(&state, now);
        self.m.borrow_mut().vm.zoom_step(zoom_in, playhead, now);
        self.after_view_change();
    }

    /// `\` and the toolbar's Fit: the view fits at once (a local zoom
    /// still waiting is dropped); without a view, the core's own command.
    fn zoom_fit(&self) {
        let now = self.now();
        if self.m.borrow_mut().vm.zoom_fit(now) {
            self.after_view_change();
            return;
        }
        let state = self.ctx.store.state();
        if let Some(cmd) = qrec_core::keymap::command_for("zoom.fit", &state) {
            self.ctx.store.dispatch(cmd);
        }
    }

    fn zoom_selection(&self) {
        let state = self.ctx.store.state();
        let now = self.now();
        let result = self.m.borrow_mut().vm.zoom_selection(&state, now);
        match result {
            Ok(()) => self.after_view_change(),
            Err(reason) => {
                if let Some(notice) = unavailable_notice("zoom.selection", &reason, &state) {
                    self.ctx.store.notify(notice);
                }
            }
        }
    }

    /// The view changed locally: redraw, and the `SetUi` 300 ms later.
    fn after_view_change(&self) {
        self.bump();
        let at = self.m.borrow().vm.flush_at();
        if let Some(at) = at {
            let wait = at.saturating_sub(self.now());
            if let Some(old) = self.flush_source.borrow_mut().take() {
                old.remove();
            }
            let me = self.me.clone();
            let id = guard::timeout_local_once(wait, "timeline: SetUi", move || {
                if let Some(inner) = me.upgrade() {
                    inner.flush_source.borrow_mut().take();
                    inner.flush();
                }
            });
            *self.flush_source.borrow_mut() = Some(id);
        }
        self.refresh_all();
    }

    /// Sends the debounced `SetUi` (D-39); its timer fired (a newer change
    /// re-arms it, so this is the newest deadline).
    fn flush(&self) {
        let state = self.ctx.store.state();
        let cmd = self.m.borrow_mut().vm.flush_now(&state);
        if let Some(cmd) = cmd {
            self.ctx.store.dispatch(cmd);
        }
        self.sync_local_view();
    }

    /// After a gesture step: the drag's previews, the cursor, redraws.
    fn after_input(&self) {
        self.bump();
        self.update_drag_tip();
        self.ensure_tick();
        if self.m.borrow().vm.dirty() {
            self.after_view_change();
        } else {
            self.canvas.queue_draw();
            self.ruler.queue_draw();
            self.overlay.queue_draw();
        }
    }

    fn update_cursor(&self, x: f64, y: f64) {
        let name = self
            .with_cx(|cx, _, _| match cx.hit(x, y) {
                tl::Hit::TrimStart(_) | tl::Hit::TrimEnd(_)
                    if tl::edits_enabled(cx.state).is_ok() =>
                {
                    Some("col-resize")
                }
                tl::Hit::Body(_) => Some("grab"),
                _ => None,
            })
            .flatten();
        self.canvas.set_cursor_from_name(name);
    }

    // ------------------------------------------------------- tooltips

    fn cancel_hover_tip(&self) {
        if let Some(id) = self.hover_source.borrow_mut().take() {
            id.remove();
        }
    }

    /// The clip tooltip after the pointer rests on a clip (C33 clip).
    fn schedule_hover_tip(&self) {
        let hovered = self.ctx.store.local().hover_clip.clone();
        let shown = self.m.borrow().tip_clip.clone();
        if hovered.is_none() {
            self.cancel_hover_tip();
            if shown.is_some() {
                self.tips.hide();
                self.m.borrow_mut().tip_clip = None;
            }
            return;
        }
        if hovered == shown {
            self.show_hover_tip();
            return;
        }
        self.tips.hide();
        self.m.borrow_mut().tip_clip = None;
        self.cancel_hover_tip();
        let delay = if testctl::mode().on {
            Duration::ZERO
        } else {
            HOVER_DELAY
        };
        let me = self.me.clone();
        let id = guard::timeout_local_once(delay, "timeline: the clip tooltip", move || {
            if let Some(inner) = me.upgrade() {
                inner.hover_source.borrow_mut().take();
                inner.show_hover_tip();
            }
        });
        *self.hover_source.borrow_mut() = Some(id);
    }

    fn show_hover_tip(&self) {
        let Some(clip) = self.ctx.store.local().hover_clip.clone() else {
            return;
        };
        let Some(pointer) = self.m.borrow().pointer else {
            return;
        };
        let state = self.ctx.store.state();
        let Some(tip) = tl::clip_tooltip(&state, &clip, &self.ctx.store.keymap()) else {
            return;
        };
        let at = self.to_layer(pointer);
        self.tips.show_clip(&tip, at);
        self.m.borrow_mut().tip_clip = Some(clip);
    }

    /// The drag tooltips (S10, S11) and the snap flash.
    fn update_drag_tip(&self) {
        let state = self.ctx.store.state();
        let now = self.now();
        let (drag, pointer, view, playhead) = {
            let m = self.m.borrow();
            (
                m.g.drag().cloned(),
                m.pointer,
                m.vm.shown(now),
                self.shown_playhead_with(&state, &m, now),
            )
        };
        let (Some(view), Some(pointer)) = (view, pointer) else {
            return;
        };
        match drag {
            Some(Drag::Move { block, x }) => {
                if let Some(p) = tl::move_preview(&state, &view, &block, x) {
                    self.tips
                        .show(&tips::move_lines(&p.tooltip), self.to_layer(pointer));
                }
            }
            Some(Drag::Trim {
                clip,
                edge,
                x0,
                x,
                started: true,
            }) => {
                if let Some(p) = tl::trim_preview(
                    &state,
                    &view,
                    &clip,
                    edge,
                    x - x0,
                    state.ui.snapping,
                    playhead,
                ) {
                    let edge_y = pointer.1;
                    self.tips.show(
                        &tips::trim_lines(&p.tooltip),
                        self.to_layer((p.edge_x, edge_y)),
                    );
                    let mut m = self.m.borrow_mut();
                    if p.snapped_x != m.snapped {
                        m.snapped = p.snapped_x;
                        if let Some(sx) = p.snapped_x
                            && !motion::one_shots_at_end()
                        {
                            m.flash = Some((sx, now));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Canvas px (100 %) to the tooltip layer's px.
    fn to_layer(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let s = self.scale();
        let point = graphene::Point::new((x * s) as f32, (y * s) as f32);
        self.canvas
            .compute_point(&self.tips.layer, &point)
            .map_or((x * s, y * s), |p| (f64::from(p.x()), f64::from(p.y())))
    }

    // ------------------------------------------------------- drops

    /// A bin drag over the canvas: the caret at the nearest boundary and
    /// "Insert take 4 · slot k of n + 1" (D-38).
    fn drop_over(&self, take: Option<TakeId>, x: f64) -> gdk::DragAction {
        let state = self.ctx.store.state();
        let x = x / self.scale();
        let now = self.now();
        let view = self.m.borrow().vm.shown(now);
        let ok = take
            .as_ref()
            .zip(view.as_ref())
            .and_then(|(t, v)| control::decide_drop(&state, v, t, x).map(|_| (t.clone(), *v)));
        match ok {
            Some((take, view)) => {
                let clips = &state.project.timeline.clips;
                let slot = tl::drop_slot(&view, clips, x);
                let caret = tl::boundary_x(&view, clips, slot);
                self.m.borrow_mut().drop = Some((take.clone(), caret));
                self.set_drop_drag(true);
                let text = tl::drop_tooltip(&take, slot, clips.len());
                self.tips.show(
                    &[vec![(text, tips::Ink::Plain)]],
                    self.to_layer((caret, 30.0)),
                );
                self.bump();
                self.canvas.queue_draw();
                gdk::DragAction::COPY
            }
            None => {
                if self.m.borrow_mut().drop.take().is_some() {
                    self.tips.hide();
                    self.bump();
                    self.canvas.queue_draw();
                }
                self.set_drop_drag(false);
                gdk::DragAction::empty()
            }
        }
    }

    /// While an accepted take hovers the canvas, `LocalUi.drag` is
    /// `BinInsert` (D-38): the hint bar shows "Release to insert" (G§4.3)
    /// and the key router treats it as a drag. Only a drag this target set
    /// is cleared (WP7's drag source may hold its own from the bin).
    fn set_drop_drag(&self, on: bool) {
        let mut m = self.m.borrow_mut();
        if on == m.drop_drag {
            return;
        }
        let local = self.ctx.store.local();
        if on && local.drag.is_some() {
            return;
        }
        m.drop_drag = on;
        drop(m);
        drop(local);
        self.ctx.store.update_local(move |l| {
            l.drag = if on {
                Some(DragKind::BinInsert)
            } else if l.drag == Some(DragKind::BinInsert) {
                None
            } else {
                l.drag
            };
        });
    }

    fn drop_at(&self, take: Option<TakeId>, x: f64) -> bool {
        let state = self.ctx.store.state();
        let x = x / self.scale();
        let view = self.m.borrow().vm.shown(self.now());
        self.m.borrow_mut().drop = None;
        self.set_drop_drag(false);
        self.tips.hide();
        self.bump();
        self.canvas.queue_draw();
        let (Some(take), Some(view)) = (take, view) else {
            return false;
        };
        match control::decide_drop(&state, &view, &take, x) {
            Some(cmd) => {
                self.ctx.store.dispatch(cmd);
                true
            }
            None => false,
        }
    }

    // ------------------------------------------------------- ticking

    /// The playhead as drawn: a scrub's frame, the ghost's end while
    /// recording, the player's frame while it plays, else `ui.playhead`.
    fn shown_playhead(&self, state: &State, now: Duration) -> Frame {
        let m = self.m.borrow();
        self.shown_playhead_with(state, &m, now)
    }

    fn shown_playhead_with(&self, state: &State, m: &Model, now: Duration) -> Frame {
        if let Some(frame) = m.g.scrub_frame() {
            return frame;
        }
        let player = (self.ctx.store.local().playing)
            .then(|| self.ctx.media.position_ns())
            .flatten()
            .map(|ns| {
                qrec_core::time::ns_to_frame_floor(
                    i64::try_from(ns).unwrap_or(i64::MAX),
                    state.project.canvas.fps,
                )
                .max(0) as Frame
            });
        tl::shown_playhead(state, ghost_elapsed(&m.ghost, now), player)
    }

    fn ensure_tick(&self) {
        if self.tick.borrow().is_some() || !self.needs_tick() {
            return;
        }
        let me = self.me.clone();
        let id = guard::tick(&self.canvas, "timeline: a frame", move |_, _| {
            match me.upgrade() {
                Some(inner) if inner.on_tick() => glib::ControlFlow::Continue,
                Some(inner) => {
                    inner.tick.borrow_mut().take();
                    glib::ControlFlow::Break
                }
                None => glib::ControlFlow::Break,
            }
        });
        *self.tick.borrow_mut() = Some(id);
    }

    /// What needs a frame each frame: easing, a growing or saving ghost, a
    /// spinner, the playing playhead, the snap flash.
    fn needs_tick(&self) -> bool {
        let now = self.now();
        let state = self.ctx.store.state();
        let m = self.m.borrow();
        let frozen = testctl::mode().on;
        let recording = !state.runtime.record.is_idle() && !frozen;
        let analyzing = !frozen
            && state.runtime.jobs.values().any(|j| {
                matches!(
                    j.kind,
                    qrec_core::state::JobKind::Autocut { dry_run: false, .. }
                )
            });
        m.vm.animating(now)
            || recording
            || analyzing
            || self.ctx.store.local().playing
            || m.flash.is_some()
            || m.gap
                .values()
                .any(|(_, _, at)| now.saturating_sub(*at) < control::GAP_EASE)
    }

    /// One frame; `false` once nothing moves any more.
    fn on_tick(&self) -> bool {
        let now = self.now();
        let state = self.ctx.store.state();
        let playing = self.ctx.store.local().playing;
        {
            let mut m = self.m.borrow_mut();
            if m.flash
                .is_some_and(|(_, at)| now.saturating_sub(at) >= FLASH)
            {
                m.flash = None;
            }
            // Fit includes the ghost as it grows between record clocks.
            if m.ghost.running {
                let total = tl::shown_total(&state, ghost_elapsed(&m.ghost, now));
                m.vm.follow_ghost(total);
            }
            m.generation += 1;
        }
        if playing {
            // Auto-page (G§4.5): the playing playhead pages the view.
            let (view, total, playhead) = {
                let m = self.m.borrow();
                (
                    m.vm.target_view(),
                    m.vm.total(),
                    self.shown_playhead_with(&state, &m, now),
                )
            };
            if let Some(view) = view
                && let Some(scroll) = tl::auto_page(&view, playhead, total)
            {
                self.m.borrow_mut().vm.scroll_to(scroll, now);
                self.after_view_change();
            }
            let fps = state.project.canvas.fps;
            self.corner
                .set_text(&qrec_core::view::ruler::corner(playhead, fps));
        }
        self.canvas.queue_draw();
        self.ruler.queue_draw();
        self.overlay.queue_draw();
        self.needs_tick()
    }

    // ------------------------------------------------------- drawing

    /// The silence spans of `state`, made once per state and inputs (a
    /// zoom or a scroll doesn't change them).
    fn silences(&self, state: &Arc<State>) -> Rc<Vec<tl::SilenceSpan>> {
        let mut m = self.m.borrow_mut();
        let version = m.silence_version;
        if let Some((shown, v, spans)) = &m.silences
            && Arc::ptr_eq(shown, state)
            && *v == version
        {
            return spans.clone();
        }
        let spans = Rc::new(tl::silence_spans(state, &m.files, &m.cached));
        m.silences = Some((state.clone(), version, spans.clone()));
        spans
    }

    fn draw_canvas(&self, s: &gtk::Snapshot, w: f64, h: f64) {
        let scale = self.scale();
        let size = (w as i32, h as i32, self.thumb_px());
        let generation = self.m.borrow().generation;
        let cached = self
            .m
            .borrow()
            .cache
            .as_ref()
            .filter(|(g, sz, _)| *g == generation && *sz == size)
            .map(|(_, _, node)| node.clone());
        if let Some(node) = cached {
            s.append_node(&node);
            return;
        }
        let started = self.probe.borrow().is_some().then(Instant::now);
        let snapshot = gtk::Snapshot::new();
        snapshot.scale(scale as f32, scale as f32);
        self.paint_canvas(&snapshot, h / scale);
        let node = snapshot.to_node();
        if let Some(t0) = started
            && let Some(p) = self.probe.borrow_mut().as_mut()
        {
            p.paint_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        if let Some(node) = node {
            s.append_node(&node);
            self.m.borrow_mut().cache = Some((generation, size, node));
        }
    }

    fn paint_canvas(&self, s: &gtk::Snapshot, height: f64) {
        let state = self.ctx.store.state();
        let local = self.ctx.store.local().clone();
        let now = self.now();
        let palette = self.ctx.theme.palette();
        let thumbs = self.ctx.media.thumbs();
        let peaks = self.ctx.media.peaks();
        let widget: gtk::Widget = self.canvas.clone().upcast();
        let silences = self.silences(&state);
        let m = self.m.borrow();
        let Some(view) = m.vm.shown(now) else {
            s.append_color(&palette.bg, &paint::rect(0.0, 0.0, m.vm.width(), height));
            return;
        };
        let elapsed = ghost_elapsed(&m.ghost, now);
        let playhead = self.shown_playhead_with(&state, &m, now);
        let hovered = local.hover_clip.as_ref();
        let mut boxes = tl::clip_layout(&state, &view, hovered, elapsed);
        let mut trim = None;
        let mut moving = None;
        match m.g.drag() {
            Some(Drag::Trim {
                clip,
                edge,
                x0,
                x,
                started: true,
            }) => {
                if let Some(p) = tl::trim_preview(
                    &state,
                    &view,
                    clip,
                    *edge,
                    x - x0,
                    state.ui.snapping,
                    playhead,
                ) {
                    boxes = p.boxes.clone();
                    trim = Some(p);
                }
            }
            Some(Drag::Move { block, x }) => {
                if let Some(p) = tl::move_preview(&state, &view, block, *x) {
                    boxes = p.boxes.clone();
                    let lifted = lifted_boxes(&state, &view, block, *x);
                    moving = Some((p, lifted));
                }
            }
            _ => {}
        }
        let ghost = tl::ghost(&state, &view, elapsed);
        let ghost_waves = ghost
            .as_ref()
            .and_then(|g| self.ghost_waves(&state, &m.lanes, g, elapsed, view.width));
        let scene = Scene {
            widget: &widget,
            texts: &self.texts,
            palette: &palette,
            state: &state,
            view,
            rows: m.rows,
            lanes: &m.lanes,
            thumbs: thumbs.as_deref(),
            peaks: peaks.as_deref(),
            thumb_px: self.thumb_px(),
            now_ms: motion::now().as_millis() as u64,
            boxes,
            ghost,
            ghost_waves,
            silences: &silences,
            autocut: local.autocut_preview.clone(),
            trim,
            moving,
            drop_caret: m.drop.as_ref().map(|(_, x)| *x),
        };
        draw::canvas(s, &scene, height);
    }

    /// The live waveform under the ghost (G§4.5): the level ring's RMS per
    /// 50 ms, newest at the ghost's right edge, one amplitude per px, for
    /// its columns in the time area `width` wide only (a long take zoomed
    /// in is a view of columns, not its length: `tl::ghost_columns`).
    fn ghost_waves(
        &self,
        state: &State,
        lanes: &[Lane],
        ghost: &tl::Ghost,
        elapsed_ns: u64,
        width: f64,
    ) -> Option<draw::GhostWaves> {
        let columns = tl::ghost_columns(ghost, width)?;
        let take = state.project.takes.get(&ghost.take);
        let ms = elapsed_ns as f64 / 1e6;
        let wanted = control::ghost_readings(ms, ghost.w, &columns.columns).min(media_history());
        let lanes = lanes
            .iter()
            .map(|lane| {
                let source = take
                    .and_then(|t| t.scene.track(&lane.track))
                    .or_else(|| state.project.scene.track(&lane.track))
                    .map(|t| t.source.clone());
                let Some(source) = source else {
                    return Vec::new();
                };
                let history = self.ctx.media.level_history(&source, wanted);
                control::ghost_amplitudes(
                    &history,
                    |l| l.rms_db,
                    ms,
                    ghost.w,
                    columns.columns.clone(),
                )
            })
            .collect();
        Some(draw::GhostWaves {
            x0: columns.x0,
            lanes,
        })
    }

    fn draw_ruler(&self, s: &gtk::Snapshot, _w: f64, h: f64) {
        let scale = self.scale();
        let state = self.ctx.store.state();
        let now = self.now();
        let Some(view) = self.m.borrow().vm.shown(now) else {
            return;
        };
        let playhead = self.shown_playhead(&state, now);
        let palette = self.ctx.theme.palette();
        let widget: gtk::Widget = self.ruler.clone().upcast();
        s.save();
        s.scale(scale as f32, scale as f32);
        draw::ruler(
            s,
            &widget,
            &self.texts,
            &palette,
            &view,
            &state,
            playhead,
            h / scale,
        );
        s.restore();
    }

    fn draw_overlay(&self, s: &gtk::Snapshot, _w: f64, h: f64) {
        let scale = self.scale();
        let state = self.ctx.store.state();
        let now = self.now();
        let m = self.m.borrow();
        let Some(view) = m.vm.shown(now) else {
            return;
        };
        let playhead = self.shown_playhead_with(&state, &m, now);
        let rows = m.rows;
        let lanes = m.lanes.len();
        let vscroll = self.scroller.vadjustment().value() / scale;
        let bottom = (rows.ruler + rows.content_height(lanes) - vscroll).min(h / scale);
        let flag =
            m.g.scrub_frame()
                .map(|f| qrec_core::view::format::timecode(f, state.project.canvas.fps));
        let o = draw::Overlay {
            playhead_x: view.x_of_frame(playhead),
            flag,
            flash: m.flash.map(|(x, _)| x),
            bottom,
            ruler: rows.ruler,
        };
        drop(m);
        let palette = self.ctx.theme.palette();
        let widget: gtk::Widget = self.overlay.clone().upcast();
        s.save();
        s.scale(scale as f32, scale as f32);
        s.translate(&graphene::Point::new(HEADER_WIDTH as f32, 0.0));
        draw::overlay(s, &widget, &self.texts, &palette, &view, &o);
        s.restore();
    }

    // ------------------------------------------------------- effects

    /// Carries out what the gestures asked for, outside the model's
    /// borrow (their `LocalUi` writes call subscribers back).
    fn apply(&self, pending: Pending) {
        let Pending {
            commands,
            drag,
            trim_frame,
            hover,
            scrubs,
            notices,
            pans,
        } = pending;
        if let Some(drag) = drag {
            let cancel: Option<Rc<dyn Fn()>> = drag.map(|_| {
                let me = self.me.clone();
                Rc::new(move || {
                    if let Some(inner) = me.upgrade() {
                        inner.cancel_drag();
                    }
                }) as Rc<dyn Fn()>
            });
            self.ctx.keys.set_drag_cancel(cancel);
            if drag.is_some() {
                self.cancel_hover_tip();
                self.tips.hide();
                self.m.borrow_mut().tip_clip = None;
            }
            self.ctx.store.update_local(move |l| l.drag = drag);
        }
        if let Some(frame) = trim_frame {
            let frame = frame.map(|(take, frame)| TrimFrame { take, frame });
            self.ctx.store.update_local(move |l| l.trim_frame = frame);
        }
        if let Some(hover) = hover {
            self.ctx.store.update_local(move |l| l.hover_clip = hover);
        }
        if let Some(frame) = scrubs.last().copied() {
            let state = self.ctx.store.state();
            // The stage's sync owns the player's commands: it scrubs a
            // paused Program player and keeps the drag's seeks from fighting
            // the scrub (D-40, G§3.10); the Seeks go out from here.
            crate::media::program::scrub(frame);
            self.corner.set_text(&qrec_core::view::ruler::corner(
                frame,
                state.project.canvas.fps,
            ));
        }
        for cmd in commands {
            self.ctx.store.dispatch(cmd);
        }
        for notice in notices {
            self.ctx.store.notify(notice);
        }
        if let (Some(dy), Some(from)) = (pans.last(), self.pan_from.get()) {
            self.scroller
                .vadjustment()
                .set_value(from - dy * self.scale());
        }
    }
}

/// What the gestures asked for, carried out after the model's borrow.
#[derive(Default)]
struct Pending {
    commands: Vec<Command>,
    drag: Option<Option<DragKind>>,
    trim_frame: Option<Option<(TakeId, Frame)>>,
    hover: Option<Option<ClipId>>,
    scrubs: Vec<Frame>,
    notices: Vec<qrec_core::view::notices::Notice>,
    pans: Vec<f64>,
}

impl Sink for Pending {
    fn dispatch(&mut self, cmd: Command) {
        self.commands.push(cmd);
    }
    fn set_drag(&mut self, drag: Option<DragKind>) {
        self.drag = Some(drag);
    }
    fn set_trim_frame(&mut self, frame: Option<(TakeId, Frame)>) {
        self.trim_frame = Some(frame);
    }
    fn set_hover(&mut self, clip: Option<ClipId>) {
        self.hover = Some(clip);
    }
    fn scrub(&mut self, frame: Frame) {
        self.scrubs.push(frame);
    }
    fn notice(&mut self, notice: qrec_core::view::notices::Notice) {
        self.notices.push(notice);
    }
    fn pan_lanes(&mut self, dy: f64) {
        self.pans.push(dy);
    }
}

/// The ghost's length now: the last record clock, grown on the frame clock
/// for at most 300 ms (not in test mode: screenshots show the clock).
fn ghost_elapsed(ghost: &GhostClock, now: Duration) -> u64 {
    ghost.elapsed(now, testctl::mode().on)
}

/// How many level readings the media hub keeps per device.
fn media_history() -> usize {
    crate::media::LEVEL_HISTORY
}

/// The moved block's boxes as the lifted copy draws them: in a row,
/// centered on the pointer.
fn lifted_boxes(state: &State, view: &Viewport, block: &[ClipId], pointer_x: f64) -> Vec<ClipBox> {
    let all = tl::clip_boxes(state, view, None);
    let moved: Vec<ClipBox> = all
        .into_iter()
        .filter(|b| block.contains(&b.clip))
        .collect();
    let width: f64 = moved.iter().map(|b| b.w).sum();
    let mut x = pointer_x - width / 2.0;
    moved
        .into_iter()
        .map(|mut b| {
            b.x = x;
            x += b.w;
            b.selected = false;
            b.hovered = false;
            b.trimming = None;
            b
        })
        .collect()
}

/// Where the pointer is for the event a controller handles, as an x in
/// its widget's px: the event's surface position, or the seat pointer's on
/// the surface, mapped into the widget. A scroll event has no position on
/// GTK 4.22, and asking its own device ("Wayland Wheel Scrolling") for one
/// stalled the window.
fn event_x(c: &impl IsA<gtk::EventController>) -> Option<f64> {
    let c = c.as_ref();
    let widget = c.widget()?;
    let native = widget.native()?;
    let (sx, sy) = match c.current_event().and_then(|e| e.position()) {
        Some(at) => at,
        None => {
            let pointer = widget.display().default_seat()?.pointer()?;
            let (x, y, _) = native.surface()?.device_position(&pointer)?;
            (x, y)
        }
    };
    let (tx, ty) = native.surface_transform();
    let point = graphene::Point::new((sx - tx) as f32, (sy - ty) as f32);
    let at = native.compute_point(&widget, &point)?;
    Some(f64::from(at.x()))
}

fn mods_of(state: gdk::ModifierType) -> Mods {
    Mods {
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
    }
}

/// The quiet swamp's size: 200×120; 160×96 in the compact rows, so the
/// empty state fits the 196 px under their toolbar.
fn empty_art_size(rows: Rows) -> (i32, i32) {
    if rows == tl::COMPACT_ROWS {
        (160, 96)
    } else {
        (200, 120)
    }
}

/// The empty timeline (S01b, C31 Timeline): the quiet swamp, "This swamp
/// is quiet.", "Press [R] and make some noise." ("Click Record and make
/// some noise." without a record key); the line's first label.
fn empty_state(ctx: &Ctx, key: &KeyCap) -> (gtk::Box, gtk::Label, Option<(Column, gtk::Picture)>) {
    let copy = tl::empty_state(&ctx.store.keymap());
    let block = gtk::Box::new(gtk::Orientation::Vertical, 0);
    block.add_css_class("tl-empty");
    block.set_halign(gtk::Align::Center);
    block.set_valign(gtk::Align::Center);
    block.set_hexpand(true);
    block.set_vexpand(true);
    let art = ctx
        .theme
        .art(crate::theme::art::Art::QuietSwamp)
        .map(|svg| {
            // In a column of its width: a picture is as tall as the width it
            // gets, and the block is as wide as its `display` line.
            let art = gtk::Picture::for_paintable(&svg);
            art.set_can_shrink(true);
            art.set_content_fit(gtk::ContentFit::Contain);
            let (w, h) = empty_art_size(tl::ROWS);
            art.set_size_request(w, h);
            art.add_css_class("tl-empty-art");
            let column = Column::new(&art, w);
            column.set_halign(gtk::Align::Center);
            block.append(&column);
            (column, art)
        });
    let title = gtk::Label::new(Some(&copy.title));
    title.add_css_class("t-display");
    title.add_css_class("tl-empty-title");
    block.append(&title);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.add_css_class("tl-empty-line");
    line.set_halign(gtk::Align::Center);
    let before = gtk::Label::new(None);
    before.add_css_class("t-body");
    before.add_css_class("muted");
    let after = gtk::Label::new(Some(&copy.after));
    after.add_css_class("t-body");
    after.add_css_class("muted");
    show_empty_copy(&copy, &before, key);
    line.append(&before);
    line.append(key);
    line.append(&after);
    block.append(&line);
    (block, before, art)
}

/// The empty state's line for the keymap's `copy`: the record key's cap,
/// or none (G§4.5).
fn show_empty_copy(copy: &tl::EmptyState, before: &gtk::Label, key: &KeyCap) {
    before.set_text(&copy.before);
    if let Some(legends) = &copy.key {
        key.set_legend(&legends.join("+"));
    }
    key.set_visible(copy.key.is_some());
}
