//! The countdown overlay (C07, S03): the preview dimmed under `video-scrim`
//! at 65 % (black at 40 %), a 200 px ring (a 6 px `video-text` 20 % track
//! and a `record` sweep that empties over each second, linear) on a
//! `video-scrim` disc, the digit's plate (G§0's M4 decision: the digit
//! over the dim alone read 2.53:1 over a white screen), and the digit in
//! the `countdown` type. The digit comes in at 1.12× and settles
//! in 160 ms (expo out) with a fade. Reduced motion: a static full ring and
//! a plain swap. Test mode freezes the clock (`motion::now`): the ring is
//! full and the digit settled. The plate under the ring ("Take 10", "Esc
//! or R cancels") is the stage's, so its type is CSS's.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk, pango};

use crate::theme::{Palette, motion};

/// The ring's outer diameter and stroke (logical px at 100 %).
pub const RING: f64 = 200.0;
pub const RING_STROKE: f64 = 6.0;
/// The digit's settle (DESIGN Motion).
const SETTLE: Duration = Duration::from_millis(160);

/// The radius of the digit's `video-scrim` plate at `scale`: the ring's
/// outer edge, so the ring is drawn over it too.
pub fn plate_radius(scale: f64) -> f64 {
    RING / 2.0 * scale
}

/// The ring's sweep `elapsed` into a second (1 full … 0 empty): it
/// empties over the second, linear; a static full ring under reduced
/// motion (DESIGN Motion).
pub fn sweep(reduced: bool, elapsed: Duration) -> f64 {
    if reduced {
        1.0
    } else {
        1.0 - (elapsed.as_millis().min(1000) as f64 / 1000.0)
    }
}

/// How far the digit settled `elapsed` after it came (0 … 1, before the
/// easing): over 160 ms; at once under reduced motion (a plain swap) and
/// when one-shot animations are at their end (test mode).
pub fn settle(reduced: bool, at_end: bool, elapsed: Duration) -> f64 {
    if reduced || at_end {
        1.0
    } else {
        (elapsed.as_secs_f64() / SETTLE.as_secs_f64()).min(1.0)
    }
}

/// The digit's zoom at `settled` (`settle`'s): 1.12× in, 1 settled.
pub fn digit_zoom(settled: f64) -> f64 {
    1.12 - 0.12 * motion::EASE_OUT_EXPO.at(settled)
}

/// What the overlay shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CountdownState {
    pub digit: u32,
    /// `motion::now()` when this digit came.
    pub since: Duration,
    /// Reduced motion: no sweep, no settle.
    pub reduced: bool,
    /// The UI scale.
    pub scale: f64,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Countdown {
        pub state: Cell<Option<CountdownState>>,
        pub palette: RefCell<Option<Rc<Palette>>>,
        /// The sweep on the frame clock (a slot: empty again after a
        /// caught panic, and the next state starts it, R65).
        pub tick: crate::guard::Slot,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Countdown {
        const NAME: &'static str = "QrecCountdown";
        type Type = super::Countdown;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("qrec-countdown");
        }
    }

    impl ObjectImpl for Countdown {
        fn dispose(&self) {
            crate::guard::run("countdown: dispose", || self.tick.stop());
        }
    }

    impl WidgetImpl for Countdown {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            crate::guard::run("countdown: snapshot", || self.draw(snapshot));
        }

        fn unmap(&self) {
            crate::guard::run("countdown: unmap", || self.tick.stop());
            self.parent_unmap();
        }
    }

    impl Countdown {
        fn draw(&self, snapshot: &gtk::Snapshot) {
            let Some(state) = self.state.get() else {
                return;
            };
            let palette = self.palette.borrow();
            let Some(palette) = palette.as_ref() else {
                return;
            };
            let obj = self.obj();
            let (w, h) = (f64::from(obj.width()), f64::from(obj.height()));
            // The dim: video-scrim at 65 % (G§1.3).
            let mut dim = palette.video_scrim;
            dim.set_alpha(dim.alpha() * 0.65);
            snapshot.append_color(&dim, &graphene::Rect::new(0.0, 0.0, w as f32, h as f32));

            let s = state.scale;
            let elapsed = motion::now().saturating_sub(state.since);
            let (cx, cy) = (w / 2.0, h / 2.0);
            // The digit's plate: the ring's disc in `video-scrim`, as text
            // over video sits (DESIGN Canvas overlays, R5), so the digit
            // reads over a white screen too (10.1:1, where the dim alone
            // left 2.53: G§0's M4 decision; tests/contrast.rs).
            let plate = gsk::PathBuilder::new();
            plate.add_circle(
                &graphene::Point::new(cx as f32, cy as f32),
                plate_radius(s) as f32,
            );
            snapshot.append_fill(
                &plate.to_path(),
                gsk::FillRule::Winding,
                &palette.video_scrim,
            );
            let stroke_w = RING_STROKE * s;
            let radius = (RING / 2.0) * s - stroke_w / 2.0;
            let track = gsk::PathBuilder::new();
            track.add_circle(&graphene::Point::new(cx as f32, cy as f32), radius as f32);
            let stroke = gsk::Stroke::new(stroke_w as f32);
            snapshot.append_stroke(&track.to_path(), &stroke, &palette.video_text_20);
            // The sweep empties over the second (full at the tick).
            let sweep = sweep(state.reduced, elapsed);
            if sweep >= 0.999 {
                let ring = gsk::PathBuilder::new();
                ring.add_circle(&graphene::Point::new(cx as f32, cy as f32), radius as f32);
                snapshot.append_stroke(&ring.to_path(), &stroke, &palette.record);
            } else if sweep > 0.0 {
                // Clockwise from the top.
                let start = -PI / 2.0;
                let end = start + sweep * 2.0 * PI;
                let point = |a: f64| {
                    (
                        (cx + radius * a.cos()) as f32,
                        (cy + radius * a.sin()) as f32,
                    )
                };
                let (x0, y0) = point(start);
                let (x1, y1) = point(end);
                let arc = gsk::PathBuilder::new();
                arc.move_to(x0, y0);
                arc.svg_arc_to(radius as f32, radius as f32, 0.0, sweep > 0.5, true, x1, y1);
                snapshot.append_stroke(&arc.to_path(), &stroke, &palette.record);
            }

            // The digit, settling from 1.12× with a fade.
            let progress = settle(state.reduced, motion::one_shots_at_end(), elapsed);
            let eased = motion::EASE_OUT_EXPO.at(progress);
            let zoom = digit_zoom(progress);
            let layout = obj.create_pango_layout(Some(&state.digit.to_string()));
            let mut desc = pango::FontDescription::new();
            desc.set_family(crate::theme::fonts::FAMILIES[1]);
            desc.set_weight(pango::Weight::Ultrabold);
            desc.set_absolute_size(160.0 * s * f64::from(pango::SCALE));
            layout.set_font_description(Some(&desc));
            let attrs = pango::AttrList::new();
            attrs.insert(pango::AttrFontFeatures::new("tnum 1"));
            layout.set_attributes(Some(&attrs));
            let (ink, _) = layout.pixel_extents();
            let mut color = palette.video_text;
            color.set_alpha(color.alpha() * eased.clamp(0.0, 1.0) as f32);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(cx as f32, cy as f32));
            snapshot.scale(zoom as f32, zoom as f32);
            let x = -(f64::from(ink.x()) + f64::from(ink.width()) / 2.0);
            let y = -(f64::from(ink.y()) + f64::from(ink.height()) / 2.0);
            snapshot.translate(&graphene::Point::new(x as f32, y as f32));
            snapshot.append_layout(&layout, &color);
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    /// The countdown overlay over the preview.
    pub struct Countdown(ObjectSubclass<imp::Countdown>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Countdown {
    pub fn new(palette: Rc<Palette>) -> Self {
        let countdown: Self = glib::Object::new();
        countdown.set_palette(palette);
        countdown.set_can_target(false);
        countdown.set_hexpand(true);
        countdown.set_vexpand(true);
        countdown.set_visible(false);
        countdown
    }

    pub fn set_palette(&self, palette: Rc<Palette>) {
        *self.imp().palette.borrow_mut() = Some(palette);
        self.queue_draw();
    }

    /// Shows `state` (`None` hides the overlay). The ring sweeps and the
    /// digit settles on the frame clock unless the motion is reduced or
    /// the clock frozen (test mode).
    pub fn set_state(&self, state: Option<CountdownState>) {
        let imp = self.imp();
        if imp.state.get() == state {
            return;
        }
        imp.state.set(state);
        self.set_visible(state.is_some());
        let animate = state.is_some_and(|s| !s.reduced) && !motion::one_shots_at_end();
        match (animate, imp.tick.is_running()) {
            (true, false) => imp.tick.tick(self, "countdown: tick", |widget, _| {
                widget.queue_draw();
                glib::ControlFlow::Continue
            }),
            (false, true) => imp.tick.stop(),
            _ => {}
        }
        self.queue_draw();
    }
}
