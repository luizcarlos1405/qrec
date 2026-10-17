//! C11 meter (G§5.1): a level meter drawn from `MediaHub::level`, looked at
//! [`REFRESH_MS`] apart while it shows (a timer, not a frame-clock tick: a
//! tick callback per meter perturbed the window's frame timing, G§8.6, and
//! readings come at 20 Hz); a frame redraws the bar only when what it shows
//! changed. A click resets the clip latch. Owner: WP5.
//!
//! [`Meter`] is the bar (a custom widget, `qrec-meter`) and the `mono-xs`
//! readout after it. The bar draws `view::meter::view` of its
//! [`MeterState`]: the `field` track, the lit 2 px segments in their zone's
//! color, the 2 px peak hold in `text`, and the 4 px red latch at the right
//! end; a muted track's level and peak at 35 %. Geometry is computed at
//! 100 % and drawn × the UI scale (D-46).
//!
//! Readings come from the media hub's latest-wins slot (`SideEvent::Level`,
//! 20 Hz per device: the live pipeline in Live mode and while recording,
//! the meter monitor in Program, D-03). [`LevelFeed`] turns that slot into
//! meter updates: a reading counts once, and a slot that stops changing for
//! [`STALE_MS`] reads as silence (the source went away).
//!
//! Test mode can pin the readings (`QREC_GUI_TEST_LEVELS=<device>=<rms>/<peak>,…`,
//! dBFS), so screenshots show the drawn levels (S02's −22/−16) and the
//! clip latch (S13b) instead of the fake tone's ticks.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};
use qrec_core::view::meter::{self, MeterState, SILENCE_DB, Zone};
use qrec_shell::capture::LevelReading;

use crate::media::MediaHub;
use crate::theme::{Palette, Theme, motion};
use crate::{guarded, testctl};

/// Told the readout's text and whether the latch is on.
pub type ReadoutFn = Rc<dyn Fn(&str, bool)>;

/// A slot that doesn't change for this long reads as silence.
pub const STALE_MS: u64 = 1500;

/// How often a showing meter looks at its slot (30 Hz: readings come at
/// 20 Hz, and a falling peak moves in steps under a dB).
pub const REFRESH_MS: u64 = 33;

thread_local! {
    /// Every bar made, weakly (test mode only): what [`ticking`] counts.
    static BARS: RefCell<Vec<glib::WeakRef<MeterBar>>> = const { RefCell::new(Vec::new()) };
}

/// How many meter bars look at their slot on their timer now (test mode;
/// `testctl`'s `local`, `metersTicking`): the mapped ones, but one whose
/// timer stopped (a caught panic) until the next change starts it again
/// (R65).
pub fn ticking() -> usize {
    BARS.with(|bars| {
        let mut bars = bars.borrow_mut();
        bars.retain(|bar| bar.upgrade().is_some());
        bars.iter()
            .filter_map(glib::WeakRef::upgrade)
            .filter(|bar| bar.imp().timer.is_running())
            .count()
    })
}

/// The loudest channel of a reading: (RMS, peak) in dBFS.
pub fn loudest(reading: &LevelReading) -> (f64, f64) {
    let max = |v: &[f64]| {
        v.iter()
            .copied()
            .filter(|x| !x.is_nan())
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let rms = max(&reading.rms_db);
    let peak = max(&reading.peak_db);
    let finite = |v: f64| if v.is_finite() { v } else { SILENCE_DB };
    (finite(rms), finite(peak.max(rms)))
}

/// Feeds a meter from a latest-wins slot (GTK-free).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LevelFeed {
    last: Option<(f64, f64)>,
    changed_at_ms: u64,
    silent: bool,
}

impl LevelFeed {
    /// The slot's reading at `now_ms` (`None`: no reading ever).
    pub fn feed(&mut self, meter: &mut MeterState, reading: Option<(f64, f64)>, now_ms: u64) {
        match reading {
            None => self.silence(meter, now_ms),
            Some(r) if self.last != Some(r) => {
                self.last = Some(r);
                self.changed_at_ms = now_ms;
                self.silent = false;
                meter.update_reading(r.0, r.1, now_ms);
            }
            Some(_) if now_ms.saturating_sub(self.changed_at_ms) >= STALE_MS => {
                self.silence(meter, now_ms);
            }
            Some(_) => {}
        }
    }

    fn silence(&mut self, meter: &mut MeterState, now_ms: u64) {
        if !self.silent {
            self.silent = true;
            meter.update_reading(SILENCE_DB, SILENCE_DB, now_ms);
        }
    }
}

/// `QREC_GUI_TEST_LEVELS`: pinned readings per device (test mode only).
pub fn test_levels() -> &'static BTreeMap<String, (f64, f64)> {
    static LEVELS: OnceLock<BTreeMap<String, (f64, f64)>> = OnceLock::new();
    LEVELS.get_or_init(|| {
        if !testctl::mode().on {
            return BTreeMap::new();
        }
        parse_test_levels(&std::env::var("QREC_GUI_TEST_LEVELS").unwrap_or_default())
    })
}

/// `device=rms/peak,…` (dBFS; the peak may be left out).
pub fn parse_test_levels(text: &str) -> BTreeMap<String, (f64, f64)> {
    text.split(',')
        .filter_map(|entry| {
            let (device, levels) = entry.trim().rsplit_once('=')?;
            let mut parts = levels.split('/');
            let rms: f64 = parts.next()?.trim().parse().ok()?;
            let peak: f64 = parts
                .next()
                .and_then(|p| p.trim().parse().ok())
                .unwrap_or(rms);
            Some((device.trim().to_owned(), (rms, peak.max(rms))))
        })
        .collect()
}

mod imp {
    use super::*;

    pub struct MeterBar {
        pub state: Cell<MeterState>,
        pub feed: RefCell<LevelFeed>,
        pub muted: Cell<bool>,
        pub now_ms: Cell<u64>,
        pub scale: Cell<f64>,
        pub palette: RefCell<Rc<Palette>>,
        pub device: RefCell<String>,
        pub media: RefCell<Option<MediaHub>>,
        /// The look at the slot every [`REFRESH_MS`] while mapped (a slot:
        /// a caught panic empties it, and the next change starts it
        /// again, R65).
        pub timer: crate::guard::Slot,
        pub on_change: RefCell<Option<ReadoutFn>>,
        pub shown: RefCell<(String, bool)>,
        pub theme: RefCell<Option<crate::store::Subscription>>,
        /// What the bar drew last: a frame that would draw the same asks
        /// for no redraw (a meter at rest costs the frame clock nothing).
        pub drawn: RefCell<Option<meter::MeterView>>,
    }

    impl Default for MeterBar {
        fn default() -> Self {
            Self {
                state: Cell::new(MeterState::default()),
                feed: RefCell::new(LevelFeed::default()),
                muted: Cell::new(false),
                now_ms: Cell::new(0),
                scale: Cell::new(1.0),
                palette: RefCell::new(Rc::new(Palette::for_theme(
                    qrec_core::config::Theme::SwampNight,
                ))),
                device: RefCell::new(String::new()),
                media: RefCell::new(None),
                timer: crate::guard::Slot::new(),
                on_change: RefCell::new(None),
                shown: RefCell::new((String::new(), false)),
                theme: RefCell::new(None),
                drawn: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MeterBar {
        const NAME: &'static str = "QrecMeterBar";
        type Type = super::MeterBar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("qrec-meter");
            klass.set_accessible_role(gtk::AccessibleRole::Meter);
        }
    }

    impl ObjectImpl for MeterBar {
        fn dispose(&self) {
            crate::guard::run("meter: dispose", || {
                self.timer.stop();
                self.theme.borrow_mut().take();
                self.on_change.borrow_mut().take();
            });
        }
    }

    impl WidgetImpl for MeterBar {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            crate::guard::run("meter: measure", || {
                let scale = self.scale.get();
                match orientation {
                    gtk::Orientation::Horizontal => (0, (meter::BAR_WIDTH * scale) as i32, -1, -1),
                    _ => {
                        let h = (meter::BAR_HEIGHT * scale).round() as i32;
                        (h, h, -1, -1)
                    }
                }
            })
        }

        fn map(&self) {
            self.parent_map();
            crate::guard::run("meter: map", || {
                self.obj().start_refresh();
                // Current at once, not a refresh later.
                self.obj().on_frame();
            });
        }

        fn unmap(&self) {
            crate::guard::run("meter: unmap", || self.timer.stop());
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            crate::guard::run("meter: snapshot", || self.draw(snapshot));
        }
    }

    impl MeterBar {
        /// The view a frame draws now.
        pub(super) fn current_view(&self) -> meter::MeterView {
            let scale = self.scale.get().max(0.1);
            let width = f64::from(self.obj().width()) / scale;
            meter::view(
                &self.state.get(),
                self.now_ms.get(),
                width,
                self.muted.get(),
            )
        }

        fn draw(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let scale = self.scale.get().max(0.1);
            let width = f64::from(obj.width()) / scale;
            let height = meter::BAR_HEIGHT;
            let palette = self.palette.borrow().clone();
            let view = self.current_view();
            *self.drawn.borrow_mut() = Some(view.clone());
            let rect = |x: f64, w: f64| {
                graphene::Rect::new(
                    (x * scale) as f32,
                    0.0,
                    (w * scale) as f32,
                    (height * scale) as f32,
                )
            };
            // The track.
            crate::widgets::draw::rounded_rect(
                snapshot,
                &rect(0.0, width),
                (2.0 * scale) as f32,
                &palette.field,
            );
            let alpha = view.alpha as f32;
            let tinted = |c: &gdk::RGBA| {
                let mut c = *c;
                c.set_alpha(c.alpha() * alpha);
                c
            };
            for segment in &view.segments {
                let color = match segment.zone {
                    Zone::Green => &palette.meter_green,
                    Zone::Yellow => &palette.meter_yellow,
                    Zone::Red => &palette.meter_red,
                };
                snapshot.append_color(&tinted(color), &rect(segment.x, segment.lit));
            }
            if let Some(x) = view.peak_x {
                snapshot.append_color(&tinted(&palette.text), &rect(x, meter::PEAK_WIDTH));
            }
            if view.latched {
                snapshot.append_color(
                    &palette.meter_red,
                    &rect(width - meter::LATCH_WIDTH, meter::LATCH_WIDTH),
                );
            }
        }
    }
}

glib::wrapper! {
    /// The meter's bar (C11).
    pub struct MeterBar(ObjectSubclass<imp::MeterBar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MeterBar {
    /// Looks at the slot every [`REFRESH_MS`] from now on.
    fn start_refresh(&self) {
        let weak = self.downgrade();
        self.imp().timer.timeout(
            std::time::Duration::from_millis(REFRESH_MS),
            "meter: refresh",
            move || match weak.upgrade() {
                Some(bar) => {
                    bar.on_frame();
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            },
        );
    }

    /// A change came (the panel's refresh): a mapped meter whose timer a
    /// caught panic stopped looks at its slot again (R65).
    fn keep_refreshing(&self) {
        if self.is_mapped() && !self.imp().timer.is_running() {
            self.start_refresh();
        }
    }

    fn on_frame(&self) {
        let imp = self.imp();
        let now = motion::now().as_millis() as u64;
        imp.now_ms.set(now);
        let device = imp.device.borrow().clone();
        let reading = match test_levels().get(&device) {
            Some(pinned) => Some(*pinned),
            None => imp
                .media
                .borrow()
                .as_ref()
                .and_then(|m| m.level(&device))
                .map(|r| loudest(&r)),
        };
        let mut state = imp.state.get();
        imp.feed.borrow_mut().feed(&mut state, reading, now);
        imp.state.set(state);
        // Only a look that changes what the bar shows redraws it: readings
        // come at 20 Hz and a held peak falls for a while; a meter at rest
        // draws nothing.
        if imp.drawn.borrow().as_ref() != Some(&imp.current_view()) {
            self.queue_draw();
        }
        self.report();
    }

    /// Tells the readout when its text or the latch changes.
    fn report(&self) {
        let imp = self.imp();
        let view = meter::view(&imp.state.get(), imp.now_ms.get(), meter::BAR_WIDTH, false);
        let now = (view.readout.clone(), view.latched);
        if *imp.shown.borrow() == now {
            return;
        }
        *imp.shown.borrow_mut() = now.clone();
        let callback = imp.on_change.borrow().clone();
        if let Some(f) = callback {
            f(&now.0, now.1);
        }
    }
}

/// A C11 meter: the bar and its readout. Cheap to clone.
#[derive(Clone)]
pub struct Meter {
    root: gtk::Box,
    bar: MeterBar,
    readout: gtk::Label,
}

impl std::fmt::Debug for Meter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Meter")
            .field("device", &*self.bar.imp().device.borrow())
            .finish()
    }
}

impl Meter {
    /// A meter of `device` (an `AudioTrack.source`), reading `media`.
    pub fn new(media: &MediaHub, theme: &Theme, device: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("meter");
        let bar: MeterBar = glib::Object::new();
        bar.set_hexpand(true);
        bar.set_valign(gtk::Align::Center);
        let readout = gtk::Label::new(Some("\u{2014}"));
        readout.add_css_class("meter-readout");
        readout.add_css_class("t-mono-xs");
        readout.add_css_class("muted");
        readout.set_xalign(1.0);
        root.append(&bar);
        root.append(&readout);
        let imp = bar.imp();
        *imp.media.borrow_mut() = Some(media.clone());
        *imp.palette.borrow_mut() = theme.palette();
        imp.scale.set(theme.scale());
        let meter = Self { root, bar, readout };
        if testctl::mode().on {
            BARS.with(|bars| bars.borrow_mut().push(meter.bar.downgrade()));
        }
        meter.set_device(device);
        let label = meter.readout.clone();
        *meter.bar.imp().on_change.borrow_mut() = Some(Rc::new(move |text: &str, latched| {
            label.set_text(text);
            if latched {
                label.remove_css_class("muted");
                label.add_css_class("record-text");
            } else {
                label.remove_css_class("record-text");
                label.add_css_class("muted");
            }
        }));
        // A click resets the latch.
        let click = gtk::GestureClick::new();
        let weak = meter.bar.downgrade();
        click.connect_released(guarded!("meter: click", move |_, _, _, _| {
            if let Some(bar) = weak.upgrade() {
                let mut state = bar.imp().state.get();
                state.reset_clip();
                bar.imp().state.set(state);
                bar.queue_draw();
                bar.report();
            }
        }));
        meter.bar.add_controller(click);
        // Theme switches redraw it (WP10).
        let weak = meter.bar.downgrade();
        let subscription = theme.subscribe(move |theme| {
            if let Some(bar) = weak.upgrade() {
                *bar.imp().palette.borrow_mut() = theme.palette();
                bar.imp().scale.set(theme.scale());
                bar.queue_resize();
                // New colors draw the same view: the frames' check skips it.
                bar.queue_draw();
            }
        });
        *meter.bar.imp().theme.borrow_mut() = Some(subscription);
        meter
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// The bar alone (its accessible name, tests).
    pub fn bar(&self) -> MeterBar {
        self.bar.clone()
    }

    /// Follows another device (a changed source).
    pub fn set_device(&self, device: &str) {
        self.bar.keep_refreshing();
        let imp = self.bar.imp();
        if *imp.device.borrow() != device {
            *imp.device.borrow_mut() = device.to_owned();
            *imp.feed.borrow_mut() = LevelFeed::default();
            imp.state.set(MeterState::default());
        }
        self.bar
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "Level of {device}"
            ))]);
    }

    /// A muted track's level and peak at 35 %.
    pub fn set_muted(&self, muted: bool) {
        self.bar.imp().muted.set(muted);
        self.bar.queue_draw();
    }

    /// The state it draws (tests, the readout).
    pub fn state(&self) -> MeterState {
        self.bar.imp().state.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(rms: &[f64], peak: &[f64]) -> LevelReading {
        LevelReading {
            device: "mic".into(),
            rms_db: rms.to_vec(),
            peak_db: peak.to_vec(),
        }
    }

    #[test]
    fn the_loudest_channel_counts() {
        assert_eq!(
            loudest(&reading(&[-30.0, -22.0], &[-18.0, -16.0])),
            (-22.0, -16.0)
        );
        // Digital silence reads −∞ from the level element.
        assert_eq!(
            loudest(&reading(&[f64::NEG_INFINITY], &[f64::NEG_INFINITY])),
            (SILENCE_DB, SILENCE_DB)
        );
        assert_eq!(loudest(&reading(&[], &[])), (SILENCE_DB, SILENCE_DB));
        // A peak never reads below the RMS.
        assert_eq!(loudest(&reading(&[-10.0], &[-20.0])), (-10.0, -10.0));
    }

    #[test]
    fn a_reading_counts_once_and_a_stale_slot_is_silence() {
        let mut feed = LevelFeed::default();
        let mut m = MeterState::default();
        feed.feed(&mut m, Some((-22.0, -16.0)), 0);
        assert_eq!((m.level_db, m.peak_db), (-22.0, -16.0));
        // The same reading again doesn't raise anything.
        feed.feed(&mut m, Some((-22.0, -16.0)), 100);
        assert_eq!(m.peak_at_ms, 0);
        // A new one does.
        feed.feed(&mut m, Some((-20.0, -12.0)), 200);
        assert_eq!((m.level_db, m.peak_at_ms), (-20.0, 200));
        // Unchanged for 1.5 s: the source went away.
        feed.feed(&mut m, Some((-20.0, -12.0)), 200 + STALE_MS);
        assert_eq!(m.level_db, SILENCE_DB);
        // No slot at all: silence.
        let mut feed = LevelFeed::default();
        let mut m = MeterState::default();
        feed.feed(&mut m, None, 0);
        assert_eq!(m.level_db, SILENCE_DB);
    }

    #[test]
    fn pinned_test_levels() {
        let levels = parse_test_levels(
            "alsa_input.fake=-22/-16, alsa_output.fake.monitor=-52/-48,x=bad,y=-3",
        );
        assert_eq!(levels["alsa_input.fake"], (-22.0, -16.0));
        assert_eq!(levels["alsa_output.fake.monitor"], (-52.0, -48.0));
        assert_eq!(levels["y"], (-3.0, -3.0));
        assert!(!levels.contains_key("x"));
        assert!(parse_test_levels("").is_empty());
    }
}
