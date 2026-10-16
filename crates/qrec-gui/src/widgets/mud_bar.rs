//! C30 mud bar (G§5.1, DESIGN Progress): the export progress bar, drawn
//! from `motion::now()` with a seeded PRNG for its bubbles. Owner: WP7.
//!
//! 20 px tall (× the UI scale), `md` radius, a `field` track with a 1 px
//! `border`. The fill is `mud` with a 3 px `wood` sediment band along its
//! bottom; its leading edge is rounded 6 px and wobbles ±1.5 px on a 2.4 s
//! sine. Bubbles are 3–6 px `mud-bubble` circles, at most 3 at a time: one
//! spawns every 0.6–1.4 s, rises 6–10 px over 1.4–2.2 s (ease-in) and pops
//! with a 120 ms fade. The fill eases to each new level over 400 ms
//! (ease-out). Under reduced motion it is a flat fill that jumps to each
//! level, with no bubbles or wobble. A failed export's bar stops where it
//! was, still; a done one is full, its bubbles still.
//!
//! It redraws every frame (a tick callback) only while it visibly animates
//! ([`animates`]: an export running, motion allowed, the clock moving):
//! done and failed bars, reduced motion and test mode's frozen clock are
//! still pictures and cost no frames (G§8.6, like the meters' timer).
//!
//! [`MudSim`] is the GTK-free part (the bubbles, the easing, the wobble):
//! a pure function of the animation clock and its seed, so test mode
//! (`motion::now()` frozen) draws frame A every time, bubbles included.
//! Frame A, the clock's 0, holds the frames' own bubbles
//! ([`RUNNING_FRAME_A`]: S15b and the S15c board; [`DONE_FRAME_A`]: S16's
//! full bar); the seed draws every bubble after them (until the WP7
//! integration audit frame A was the seed's, two bubbles where the frames
//! draw three).
//!
//! The sediment band is 3 px from the bar's outer bottom, as the pen draws
//! it under its 1 px inner stroke: 2 px of it show above the border, under
//! 16 px of mud (it was 3 px above the border, under 15 px).

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::theme::Palette;
use crate::theme::motion::{self, Curve};

/// The bar's height, logical px.
pub const HEIGHT: f64 = 20.0;
/// The sediment band, from the bar's outer bottom (its 1 px border covers
/// the lowest px of it).
const SEDIMENT: f64 = 3.0;
/// The track's and the leading edge's radius.
const RADIUS: f64 = 6.0;
/// The wobble's amplitude (px) and period (ms).
const WOBBLE_PX: f64 = 1.5;
const WOBBLE_MS: f64 = 2400.0;
/// The level's easing.
const EASE_MS: f64 = 400.0;
/// CSS `ease-out`.
const EASE_OUT: Curve = Curve(0.0, 0.0, 0.58, 1.0);
/// CSS `ease-in`, for a bubble's rise.
const EASE_IN: Curve = Curve(0.42, 0.0, 1.0, 1.0);
/// At most this many bubbles at once.
const MAX_BUBBLES: usize = 3;
/// A bubble's pop.
const POP_MS: f64 = 120.0;

/// A bubble as a frame draws it at the clock's 0 (frame A): where along the
/// fill (a fraction of its width), its radius (px) and how far it has risen
/// (px above where bubbles start, 1 px over the sediment).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameBubble {
    pub x: f64,
    pub radius: f64,
    pub rise: f64,
}

/// The running bar's frame A (S15b, the S15c board's frame A): bubbles of
/// 4, 6 and 3 px at 23 %, 58 % and 87 % of the fill, risen 3, 5 and 3 px.
pub const RUNNING_FRAME_A: [FrameBubble; 3] = [
    FrameBubble {
        x: 0.229,
        radius: 2.0,
        rise: 3.0,
    },
    FrameBubble {
        x: 0.576,
        radius: 3.0,
        rise: 5.0,
    },
    FrameBubble {
        x: 0.874,
        radius: 1.5,
        rise: 3.0,
    },
];

/// The done bar's (S16's full bar): bubbles of 4 and 5 px at 30 % and 74 %,
/// risen 4 and 5 px.
pub const DONE_FRAME_A: [FrameBubble; 2] = [
    FrameBubble {
        x: 0.298,
        radius: 2.0,
        rise: 4.0,
    },
    FrameBubble {
        x: 0.743,
        radius: 2.5,
        rise: 5.0,
    },
];

/// What the bar shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MudState {
    /// Running at a level (0–1): the fill eases to it, bubbles rise.
    Running(f64),
    /// Done: full (bubbles still rise, S16).
    Done,
    /// Failed at a level: the fill stops there, still (S17).
    Failed(f64),
}

impl MudState {
    fn level(self) -> f64 {
        match self {
            MudState::Running(l) | MudState::Failed(l) => l.clamp(0.0, 1.0),
            MudState::Done => 1.0,
        }
    }

    fn alive(self) -> bool {
        !matches!(self, MudState::Failed(_))
    }
}

thread_local! {
    /// Every bar made, weakly (test mode only): what [`ticking`] counts.
    static BARS: RefCell<Vec<glib::WeakRef<MudBar>>> = const { RefCell::new(Vec::new()) };
}

/// How many bars run their tick callback now (test mode; `testctl`'s
/// `local`, `mudBarsTicking`): none under test mode's frozen clock, whatever
/// they show, as [`animates`] says (G§8.6).
pub fn ticking() -> usize {
    BARS.with(|bars| {
        let mut bars = bars.borrow_mut();
        bars.retain(|bar| bar.upgrade().is_some());
        bars.iter()
            .filter_map(glib::WeakRef::upgrade)
            .filter(|bar| {
                let tick = bar.imp().tick.take();
                let on = tick.is_some();
                bar.imp().tick.set(tick);
                on
            })
            .count()
    })
}

/// Whether a bar showing `state` animates, and so redraws every frame: only
/// while an export runs (`Running`) with motion allowed (`reduced` false)
/// and the animation clock moving (`frozen` false: test mode's stands
/// still). Everything else is a still picture.
pub fn animates(state: MudState, reduced: bool, frozen: bool) -> bool {
    matches!(state, MudState::Running(_)) && !reduced && !frozen
}

/// One bubble on screen: where (`x` as a fraction of the fill's width, `y`
/// px up from its start above the sediment), how big and how opaque.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bubble {
    pub x: f64,
    pub rise: f64,
    pub radius: f64,
    pub alpha: f64,
}

/// A spawned bubble's whole life.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Life {
    born_ms: f64,
    rise_ms: f64,
    rise_px: f64,
    radius: f64,
    x: f64,
}

impl Life {
    fn end_ms(&self) -> f64 {
        self.born_ms + self.rise_ms + POP_MS
    }

    fn at(&self, t: f64) -> Option<Bubble> {
        let age = t - self.born_ms;
        if age < 0.0 || t >= self.end_ms() {
            return None;
        }
        let progress = (age / self.rise_ms).min(1.0);
        let alpha = if age <= self.rise_ms {
            1.0
        } else {
            1.0 - (age - self.rise_ms) / POP_MS
        };
        Some(Bubble {
            x: self.x,
            rise: EASE_IN.at(progress) * self.rise_px,
            radius: self.radius,
            alpha: alpha.clamp(0.0, 1.0),
        })
    }
}

/// splitmix64: small, seeded, the same everywhere.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [lo, hi).
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        let unit = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        lo + unit * (hi - lo)
    }
}

/// The bar's animation (GTK-free): the eased level, the wobble and the
/// bubbles at a time `t` of the animation clock (ms).
#[derive(Debug, Clone)]
pub struct MudSim {
    rng: Rng,
    /// The next bubble's birth.
    next_ms: f64,
    lives: Vec<Life>,
    /// The time simulated up to.
    at_ms: f64,
    /// The level easing: from, to, since.
    from: f64,
    to: f64,
    since_ms: f64,
}

impl MudSim {
    /// A simulation with its own `seed` (test mode uses a fixed one) and
    /// the running bar's frame A ([`RUNNING_FRAME_A`]).
    pub fn new(seed: u64) -> Self {
        Self::with_frame_a(seed, &RUNNING_FRAME_A)
    }

    /// A simulation whose clock's 0 shows `frame_a` (at most
    /// [`MAX_BUBBLES`]): each bubble is a life born before 0 that has risen
    /// as far as the frame draws it; the seed spawns every later one.
    pub fn with_frame_a(seed: u64, frame_a: &[FrameBubble]) -> Self {
        let mut rng = Rng(seed);
        let lives = frame_a
            .iter()
            .take(MAX_BUBBLES)
            .enumerate()
            .map(|(n, b)| {
                // Rises and durations inside DESIGN's ranges, a little
                // apart so the three don't move as one.
                let rise_px = (7.0 + n as f64).max(b.rise + 0.5).min(10.0);
                let rise_ms = 1600.0 + 250.0 * n as f64;
                let progress = progress_for(b.rise / rise_px);
                Life {
                    born_ms: -progress * rise_ms,
                    rise_ms,
                    rise_px,
                    radius: b.radius,
                    x: b.x,
                }
            })
            .collect();
        let next_ms = rng.range(600.0, 1400.0);
        Self {
            rng,
            next_ms,
            lives,
            at_ms: 0.0,
            from: 0.0,
            to: 0.0,
            since_ms: f64::NEG_INFINITY,
        }
    }

    /// Simulates up to `t` (ms; earlier times are ignored: the clock only
    /// goes forward).
    pub fn advance(&mut self, t: f64) {
        if t < self.at_ms {
            return;
        }
        while self.next_ms <= t {
            let born = self.next_ms;
            self.lives.retain(|l| l.end_ms() > born);
            let life = Life {
                born_ms: born,
                rise_ms: self.rng.range(1400.0, 2200.0),
                rise_px: self.rng.range(6.0, 10.0),
                radius: self.rng.range(1.5, 3.0),
                x: self.rng.range(0.08, 0.92),
            };
            if self.lives.len() < MAX_BUBBLES {
                self.lives.push(life);
            }
            self.next_ms = born + self.rng.range(600.0, 1400.0);
        }
        self.lives.retain(|l| l.end_ms() > t);
        self.at_ms = t;
    }

    /// The bubbles at the time simulated to.
    pub fn bubbles(&self) -> Vec<Bubble> {
        self.lives.iter().filter_map(|l| l.at(self.at_ms)).collect()
    }

    /// A new target level at `t`; `jump` (reduced motion, or the first
    /// level shown) skips the easing.
    pub fn set_level(&mut self, level: f64, t: f64, jump: bool) {
        let level = level.clamp(0.0, 1.0);
        if (level - self.to).abs() < f64::EPSILON && !jump {
            return;
        }
        self.from = if jump { level } else { self.level_at(t) };
        self.to = level;
        self.since_ms = t;
    }

    /// The level shown at `t` (eased over 400 ms).
    pub fn level_at(&self, t: f64) -> f64 {
        let p = ((t - self.since_ms) / EASE_MS).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * EASE_OUT.at(p)
    }

    /// The leading edge's offset at `t` (px, ±1.5).
    pub fn wobble(t: f64) -> f64 {
        WOBBLE_PX * (2.0 * PI * t / WOBBLE_MS).sin()
    }
}

/// The rise's progress (0–1) at which the ease-in has covered `fraction`
/// of it (its inverse, by bisection: the curve is monotonic).
fn progress_for(fraction: f64) -> f64 {
    let fraction = fraction.clamp(0.0, 1.0);
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..50 {
        let mid = (lo + hi) / 2.0;
        if EASE_IN.at(mid) < fraction {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}

/// Every bar's seed: the same bubbles after frame A in every run.
const SEED: u64 = 0x51EC_0D0E;

/// The bar's colors (from the theme's palette).
#[derive(Debug, Clone)]
pub(crate) struct Colors {
    track: gdk::RGBA,
    border: gdk::RGBA,
    mud: gdk::RGBA,
    wood: gdk::RGBA,
    bubble: gdk::RGBA,
}

impl Colors {
    fn of(p: &Palette) -> Self {
        Self {
            track: p.field,
            border: p.border,
            mud: p.mud,
            wood: p.wood,
            bubble: p.mud_bubble,
        }
    }
}

mod imp {
    use super::*;

    pub struct MudBar {
        pub sim: RefCell<MudSim>,
        pub state: Cell<MudState>,
        pub reduced: Cell<bool>,
        pub scale: Cell<f64>,
        pub(crate) colors: RefCell<Colors>,
        pub shown: Cell<bool>,
        pub tick: Cell<Option<gtk::TickCallbackId>>,
    }

    impl Default for MudBar {
        fn default() -> Self {
            Self {
                sim: RefCell::new(MudSim::new(SEED)),
                state: Cell::new(MudState::Running(0.0)),
                reduced: Cell::new(false),
                scale: Cell::new(1.0),
                colors: RefCell::new(Colors::of(&Palette::default())),
                shown: Cell::new(false),
                tick: Cell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MudBar {
        const NAME: &'static str = "QrecMudBar";
        type Type = super::MudBar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("mud-bar");
            klass.set_accessible_role(gtk::AccessibleRole::ProgressBar);
        }
    }

    impl ObjectImpl for MudBar {
        fn dispose(&self) {
            crate::guard::run("mud bar: dispose", || {
                if let Some(tick) = self.tick.take() {
                    tick.remove();
                }
            });
        }
    }

    impl WidgetImpl for MudBar {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            crate::guard::run("mud bar: measure", || {
                let height = (HEIGHT * self.scale.get()).round() as i32;
                match orientation {
                    gtk::Orientation::Vertical => (height, height, -1, -1),
                    _ => (0, 0, -1, -1),
                }
            })
        }

        fn map(&self) {
            self.parent_map();
            crate::guard::run("mud bar: map", || self.sync_tick());
        }

        fn unmap(&self) {
            crate::guard::run("mud bar: unmap", || {
                if let Some(tick) = self.tick.take() {
                    tick.remove();
                }
            });
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            crate::guard::run("mud bar: snapshot", || self.draw(snapshot));
        }
    }

    impl MudBar {
        /// Runs the tick callback while the bar is mapped and animates
        /// ([`animates`]), and removes it otherwise.
        pub fn sync_tick(&self) {
            let obj = self.obj();
            let wanted =
                obj.is_mapped() && animates(self.state.get(), self.reduced.get(), motion::frozen());
            let running = self.tick.take();
            match (wanted, running) {
                (true, Some(tick)) => self.tick.set(Some(tick)),
                (true, None) => {
                    let tick = crate::guard::tick(&*obj, "mud bar: tick", |bar, _clock| {
                        bar.queue_draw();
                        glib::ControlFlow::Continue
                    });
                    self.tick.set(Some(tick));
                }
                (false, Some(tick)) => tick.remove(),
                (false, None) => {}
            }
        }

        fn draw(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let s = self.scale.get();
            let (w, h) = (f64::from(obj.width()), f64::from(obj.height()));
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let t = motion::now().as_secs_f64() * 1000.0;
            let state = self.state.get();
            let reduced = self.reduced.get();
            let frozen = motion::frozen();
            let (level, bubbles, wobble) = {
                let mut sim = self.sim.borrow_mut();
                // Bubbles show unless motion is reduced or it failed; they
                // move only while it animates (or to the frozen test time),
                // so a done bar keeps the bubbles it had.
                let live = !reduced && state.alive();
                if live && (animates(state, reduced, frozen) || frozen) {
                    sim.advance(t);
                }
                let level = if reduced || !state.alive() {
                    state.level()
                } else {
                    sim.level_at(t)
                };
                let bubbles = if live { sim.bubbles() } else { Vec::new() };
                let wobble = if live && level < 1.0 {
                    MudSim::wobble(t) * s
                } else {
                    0.0
                };
                (level, bubbles, wobble)
            };
            let colors = self.colors.borrow();
            let bar_h = (HEIGHT * s).min(h);
            let top = ((h - bar_h) / 2.0).round();
            let rect = |x: f64, y: f64, w: f64, h: f64| {
                graphene::Rect::new(x as f32, y as f32, w.max(0.0) as f32, h.max(0.0) as f32)
            };
            let radius = (RADIUS * s) as f32;
            let border = s.max(1.0).round();
            // The track: `field` with a 1 px `border`.
            let outer = gsk::RoundedRect::from_rect(rect(0.0, top, w, bar_h), radius);
            snapshot.push_rounded_clip(&outer);
            snapshot.append_color(&colors.border, &rect(0.0, top, w, bar_h));
            snapshot.pop();
            let inner_rect = rect(border, top + border, w - 2.0 * border, bar_h - 2.0 * border);
            let inner = gsk::RoundedRect::from_rect(inner_rect, (radius - border as f32).max(0.0));
            snapshot.push_rounded_clip(&inner);
            snapshot.append_color(&colors.track, &inner_rect);
            // The fill, its sediment and its bubbles, inside the track.
            let span = w - 2.0 * border;
            let fill_w = (level * span + if level > 0.0 { wobble } else { 0.0 }).clamp(0.0, span);
            if fill_w > 0.0 {
                let fill_rect = rect(border, top + border, fill_w, bar_h - 2.0 * border);
                let edge = graphene::Size::new(radius.min(fill_w as f32), radius);
                let flat = graphene::Size::new(0.0, 0.0);
                let fill = gsk::RoundedRect::new(fill_rect, flat, edge, edge, flat);
                snapshot.push_rounded_clip(&fill);
                snapshot.append_color(&colors.mud, &fill_rect);
                // From the bar's outer bottom: the border covers its lowest
                // px (the clip), as the pen's inner stroke does.
                let sediment = SEDIMENT * s;
                let sediment_top = top + bar_h - sediment;
                snapshot.append_color(
                    &colors.wood,
                    &rect(border, sediment_top, fill_w, sediment - border),
                );
                for b in bubbles {
                    let r = b.radius * s;
                    let cx = border + b.x * fill_w;
                    let cy = sediment_top - r - 1.0 * s - b.rise * s;
                    if cx - r < border || cx + r > border + fill_w || cy - r < top {
                        continue;
                    }
                    let builder = gsk::PathBuilder::new();
                    builder.add_circle(&graphene::Point::new(cx as f32, cy as f32), r as f32);
                    let mut color = colors.bubble;
                    color.set_alpha(color.alpha() * b.alpha as f32);
                    snapshot.append_fill(&builder.to_path(), gsk::FillRule::Winding, &color);
                }
                snapshot.pop();
            }
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    /// The C30 mud bar.
    pub struct MudBar(ObjectSubclass<imp::MudBar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for MudBar {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl MudBar {
    /// A bar drawn with `theme`'s palette, scale and motion (it follows
    /// them).
    pub fn new(theme: &crate::theme::Theme) -> Self {
        let bar = Self::default();
        if crate::testctl::mode().on {
            BARS.with(|bars| bars.borrow_mut().push(bar.downgrade()));
        }
        bar.follow(theme);
        let weak = bar.downgrade();
        theme
            .subscribe(move |theme| {
                if let Some(bar) = weak.upgrade() {
                    bar.follow(theme);
                }
            })
            .detach();
        bar.update_property(&[gtk::accessible::Property::Label("Export progress")]);
        bar
    }

    fn follow(&self, theme: &crate::theme::Theme) {
        let imp = self.imp();
        *imp.colors.borrow_mut() = Colors::of(&theme.palette());
        imp.scale.set(theme.scale());
        imp.reduced.set(theme.motion().reduced);
        imp.sync_tick();
        self.queue_resize();
        self.queue_draw();
    }

    /// Shows `state`: a new level eases in over 400 ms (jumps under reduced
    /// motion, and the first time).
    pub fn set_state(&self, state: MudState) {
        let imp = self.imp();
        let t = motion::now().as_secs_f64() * 1000.0;
        // Test mode freezes the clock: one-shot animations land at their end.
        let jump = imp.reduced.get()
            || !imp.shown.replace(true)
            || !state.alive()
            || motion::one_shots_at_end();
        imp.sim.borrow_mut().set_level(state.level(), t, jump);
        imp.state.set(state);
        imp.sync_tick();
        let percent = (state.level() * 100.0).round();
        self.update_property(&[
            gtk::accessible::Property::ValueMin(0.0),
            gtk::accessible::Property::ValueMax(100.0),
            gtk::accessible::Property::ValueNow(percent),
        ]);
        self.queue_draw();
    }

    /// A fresh bar: the next level shown jumps there.
    pub fn reset(&self) {
        self.imp().shown.set(false);
    }

    /// The bubbles its clock's 0 shows (the running bar's
    /// [`RUNNING_FRAME_A`] unless set: the done page's bar draws S16's
    /// [`DONE_FRAME_A`]).
    pub fn set_frame_a(&self, frame_a: &[FrameBubble]) {
        let imp = self.imp();
        let mut sim = MudSim::with_frame_a(SEED, frame_a);
        let level = imp.sim.borrow().level_at(f64::INFINITY);
        sim.set_level(level, 0.0, true);
        *imp.sim.borrow_mut() = sim;
        self.queue_draw();
    }

    /// What it shows.
    pub fn state(&self) -> MudState {
        self.imp().state.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The PRNG's sequence is the same everywhere (G§8.2): every frame is
    /// the same picture in every run.
    #[test]
    fn the_bubbles_are_a_function_of_the_seed_and_the_clock() {
        let mut a = MudSim::new(7);
        let mut b = MudSim::new(7);
        assert_eq!(a.bubbles(), b.bubbles());
        assert!(!a.bubbles().is_empty(), "frame A has bubbles");
        a.advance(5000.0);
        b.advance(5000.0);
        assert_eq!(a.bubbles(), b.bubbles());
        let mut c = MudSim::new(8);
        c.advance(5000.0);
        assert_ne!(a.bubbles(), c.bubbles(), "another seed, other bubbles");
        // Advancing in steps or at once ends the same.
        let mut steps = MudSim::new(7);
        for t in (0..5000).step_by(16) {
            steps.advance(f64::from(t));
        }
        steps.advance(5000.0);
        let mut once = MudSim::new(7);
        once.advance(5000.0);
        assert_eq!(steps.bubbles(), once.bubbles());
    }

    /// DESIGN Progress: at most 3 at a time, 3–6 px, rising 6–10 px, a
    /// spawn every 0.6–1.4 s, each living 1.4–2.2 s plus a 120 ms pop.
    #[test]
    fn bubbles_keep_to_the_design() {
        let mut sim = MudSim::new(42);
        let mut seen = 0;
        for t in (0..60_000).step_by(20) {
            sim.advance(f64::from(t));
            let bubbles = sim.bubbles();
            assert!(bubbles.len() <= MAX_BUBBLES, "{bubbles:?} at {t}");
            for b in &bubbles {
                assert!((1.5..=3.0).contains(&b.radius), "{b:?}");
                assert!((0.0..10.0).contains(&b.rise), "{b:?}");
                assert!((0.08..0.92).contains(&b.x), "{b:?}");
                assert!((0.0..=1.0).contains(&b.alpha), "{b:?}");
            }
            seen = seen.max(bubbles.len());
        }
        assert!(seen >= 2, "bubbles overlap in time");
        // A minute holds about 60 / 1 s spawns.
        let lives: Vec<Life> = sim.lives.clone();
        assert!(lives.iter().all(|l| (1400.0..2200.0).contains(&l.rise_ms)));
    }

    /// Frame A (the clock's 0, test mode's frozen time) is the frames' own
    /// picture whatever the seed: S15b's and the S15c board's three bubbles
    /// (4, 6 and 3 px at 23, 58 and 87 % of the fill), S16's two on the
    /// done bar; later frames are the seed's, inside DESIGN's ranges.
    #[test]
    fn frame_a_is_the_frames() {
        for seed in [1, 7, SEED] {
            for frame in [&RUNNING_FRAME_A[..], &DONE_FRAME_A[..]] {
                let sim = MudSim::with_frame_a(seed, frame);
                let shown = sim.bubbles();
                assert_eq!(shown.len(), frame.len(), "{shown:?}");
                for (b, want) in shown.iter().zip(frame) {
                    assert_eq!((b.x, b.radius), (want.x, want.radius));
                    assert!((b.rise - want.rise).abs() < 1e-6, "{b:?} vs {want:?}");
                    assert!((b.alpha - 1.0).abs() < 1e-9);
                }
            }
        }
        let running = MudSim::new(SEED).bubbles();
        let diameters: Vec<f64> = running.iter().map(|b| b.radius * 2.0).collect();
        assert_eq!(diameters, [4.0, 6.0, 3.0]);
        // They rise on: 300 ms later each is higher (or popping).
        let mut later = MudSim::new(SEED);
        later.advance(300.0);
        for (b, a) in later.bubbles().iter().zip(&running) {
            assert!(b.rise > a.rise, "{b:?} after {a:?}");
            assert!(b.rise < 10.0);
        }
    }

    /// The level eases over 400 ms (ease-out) and a jump lands at once.
    #[test]
    fn the_level_eases_and_jumps() {
        let mut sim = MudSim::new(1);
        sim.set_level(0.2, 0.0, true);
        assert!((sim.level_at(0.0) - 0.2).abs() < 1e-9);
        sim.set_level(0.6, 1000.0, false);
        assert!((sim.level_at(1000.0) - 0.2).abs() < 1e-9);
        let half = sim.level_at(1200.0);
        assert!(half > 0.4 && half < 0.6, "ease-out is past halfway: {half}");
        assert!((sim.level_at(1400.0) - 0.6).abs() < 1e-9);
        assert!((sim.level_at(9000.0) - 0.6).abs() < 1e-9);
        // A new level mid-ease starts from where it is.
        sim.set_level(0.2, 1100.0, false);
        sim.set_level(1.0, 1150.0, false);
        let from = sim.level_at(1150.0);
        assert!(from > 0.2 && from < 0.6, "{from}");
        sim.set_level(0.3, 2000.0, true);
        assert!((sim.level_at(2000.0) - 0.3).abs() < 1e-9, "reduced: jumps");
    }

    #[test]
    fn the_edge_wobbles_one_and_a_half_px_on_a_2_4_s_sine() {
        assert!(MudSim::wobble(0.0).abs() < 1e-9);
        assert!((MudSim::wobble(600.0) - 1.5).abs() < 1e-9);
        assert!((MudSim::wobble(1800.0) + 1.5).abs() < 1e-9);
        assert!(MudSim::wobble(2400.0).abs() < 1e-9);
    }

    /// Only a running export's bar with motion allowed and a moving clock
    /// redraws every frame: done, failed, reduced motion and the frozen
    /// test clock are still pictures (no tick callback, G§8.6).
    #[test]
    fn only_a_running_bar_animates() {
        assert!(animates(MudState::Running(0.42), false, false));
        assert!(!animates(MudState::Running(0.42), true, false), "reduced");
        assert!(!animates(MudState::Running(0.42), false, true), "frozen");
        assert!(!animates(MudState::Done, false, false));
        assert!(!animates(MudState::Failed(0.37), false, false));
    }

    #[test]
    fn states_have_their_levels() {
        assert_eq!(MudState::Running(0.42).level(), 0.42);
        assert_eq!(MudState::Done.level(), 1.0);
        assert_eq!(MudState::Failed(1.7).level(), 1.0);
        assert!(!MudState::Failed(0.37).alive());
        assert!(MudState::Done.alive());
    }
}
