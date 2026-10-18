//! The stage's plates over the video (C06, DESIGN Canvas overlays; inset
//! 12 px, `video-scrim` and `video-text`): the mode badges top-right (`LIVE`,
//! `PROGRAM`, `TAKE 4` for a bin preview, `PROXY` left of them), the REC
//! cluster top-left (the torch badge and its timer plate, or the "Starting
//! take 10…" / "Saving take 10…" pill), the S10 trim plate bottom-left and
//! D-05's "Live preview starts after this take".
//!
//! The torch flickers on a 1600 ms loop (DESIGN's keyframes, linear between
//! them) with its `record-glow`, both through the badge's opacity; reduced
//! motion keeps it still and drops the glow. The timer counts on the frame
//! clock from the last `RecordClock` (at most 300 ms past it). Test mode
//! freezes both (`motion::now`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use qrec_core::model::Canvas;
use qrec_core::view::canvas::{Badge, RecBadge, TrimPlate, rec_timer};

use crate::theme::motion;
use crate::widgets::Spinner;

/// The torch's keyframes: (percent of the loop, opacity).
pub const TORCH: [(f64, f64); 9] = [
    (0.0, 1.00),
    (9.0, 0.82),
    (17.0, 0.97),
    (31.0, 0.74),
    (46.0, 1.00),
    (63.0, 0.86),
    (78.0, 0.95),
    (90.0, 0.80),
    (100.0, 1.00),
];
/// The torch loop.
pub const TORCH_LOOP: Duration = Duration::from_millis(1600);
/// The timer never runs ahead of the last `RecordClock` by more than this.
const TIMER_LEAD: Duration = Duration::from_millis(300);

/// The REC badge's look `t` into the torch loop: its opacity and whether
/// it wears its `record-glow`. Reduced motion: a static badge (opacity 1)
/// without the glow (DESIGN Motion).
pub fn torch_look(reduced: bool, t: Duration) -> (f64, bool) {
    if reduced {
        (1.0, false)
    } else {
        (torch_opacity(t), true)
    }
}

/// The torch's opacity at `t` into its loop (linear between keyframes).
pub fn torch_opacity(t: Duration) -> f64 {
    let loop_ms = TORCH_LOOP.as_millis() as f64;
    let pct = (t.as_millis() as f64 % loop_ms) / loop_ms * 100.0;
    for pair in TORCH.windows(2) {
        let ((p0, o0), (p1, o1)) = (pair[0], pair[1]);
        if pct >= p0 && pct <= p1 {
            let f = if p1 > p0 { (pct - p0) / (p1 - p0) } else { 0.0 };
            return o0 + (o1 - o0) * f;
        }
    }
    1.0
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// A box no pointer reaches (the canvas under it takes the input).
fn plate(orientation: gtk::Orientation, class: &str) -> gtk::Box {
    let b = gtk::Box::new(orientation, 0);
    b.add_css_class(class);
    b.set_can_target(false);
    b
}

// ---------------------------------------------------------------- badges

/// The mode badges, top-right.
pub struct ModeBadges {
    pub root: gtk::Box,
    shown: RefCell<Vec<Badge>>,
}

impl Default for ModeBadges {
    fn default() -> Self {
        Self::new()
    }
}

impl ModeBadges {
    pub fn new() -> Self {
        let root = plate(gtk::Orientation::Horizontal, "stage-badges");
        root.set_halign(gtk::Align::End);
        root.set_valign(gtk::Align::Start);
        Self {
            root,
            shown: RefCell::new(Vec::new()),
        }
    }

    fn badge(badge: &Badge) -> gtk::Box {
        let b = plate(gtk::Orientation::Horizontal, "stage-badge");
        let icon = match badge {
            Badge::Live => "qrec-video-symbolic",
            Badge::Program | Badge::Take { .. } => "qrec-film-symbolic",
            Badge::Proxy => "qrec-gauge-symbolic",
        };
        b.append(&gtk::Image::from_icon_name(icon));
        b.append(&label(&badge.label(), &["t-badge"]));
        b
    }

    /// Shows `badges` (right to left, as `view::canvas::badges` gives them).
    pub fn update(&self, badges: &[Badge]) {
        if *self.shown.borrow() == badges {
            return;
        }
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        for badge in badges.iter().rev() {
            self.root.append(&Self::badge(badge));
        }
        *self.shown.borrow_mut() = badges.to_vec();
    }
}

// ----------------------------------------------------------- REC cluster

/// The REC cluster, top-left.
pub struct RecCluster {
    pub root: gtk::Box,
    badge: gtk::Box,
    timer_plate: gtk::Box,
    timer: gtk::Label,
    take: gtk::Label,
    pill: gtk::Box,
    pill_label: gtk::Label,
    shown: RefCell<RecBadge>,
    /// The last `RecordClock`: its elapsed time and when it came
    /// (`motion::now`).
    clock: Cell<Option<(u64, Duration)>>,
    canvas: RefCell<Canvas>,
    reduced: Cell<bool>,
    /// The torch and the timer on the frame clock (a slot: empty again
    /// after a caught panic, and the next update starts it, R65).
    tick: crate::guard::Slot,
}

impl RecCluster {
    pub fn new() -> Rc<Self> {
        let root = plate(gtk::Orientation::Vertical, "stage-rec-cluster");
        root.set_halign(gtk::Align::Start);
        root.set_valign(gtk::Align::Start);
        let badge = plate(gtk::Orientation::Horizontal, "stage-rec");
        badge.set_halign(gtk::Align::Start);
        badge.append(&gtk::Image::from_icon_name("qrec-record-symbolic"));
        badge.append(&label("REC", &["t-rec"]));
        let timer_plate = plate(gtk::Orientation::Vertical, "stage-timer");
        timer_plate.set_halign(gtk::Align::Start);
        let timer = label("00:00:00:00", &["t-mono-lg"]);
        timer.set_xalign(0.0);
        let take = label("", &["t-caption"]);
        take.set_xalign(0.0);
        timer_plate.append(&timer);
        timer_plate.append(&take);
        let pill = plate(gtk::Orientation::Horizontal, "stage-pill");
        pill.set_halign(gtk::Align::Start);
        let spinner = Spinner::new(14);
        spinner.set_valign(gtk::Align::Center);
        pill.append(&spinner);
        let pill_label = label("", &["t-body-strong"]);
        pill.append(&pill_label);
        root.append(&badge);
        root.append(&timer_plate);
        root.append(&pill);
        root.set_visible(false);
        Rc::new(Self {
            root,
            badge,
            timer_plate,
            timer,
            take,
            pill,
            pill_label,
            shown: RefCell::new(RecBadge::None),
            clock: Cell::new(None),
            canvas: RefCell::new(Canvas::default()),
            reduced: Cell::new(false),
            tick: crate::guard::Slot::new(),
        })
    }

    /// Shows `rec` (the REC badge with its timer, or a pill, or nothing).
    pub fn update(self: &Rc<Self>, rec: &RecBadge, canvas: &Canvas, reduced: bool) {
        *self.canvas.borrow_mut() = canvas.clone();
        let reduced_changed = self.reduced.replace(reduced) != reduced;
        if let RecBadge::Recording { elapsed_ns, .. } = rec {
            let changed = self.clock.get().is_none_or(|(ns, _)| ns != *elapsed_ns);
            if changed {
                self.clock.set(Some((*elapsed_ns, motion::now())));
            }
        } else {
            self.clock.set(None);
        }
        if *self.shown.borrow() == *rec && !reduced_changed {
            self.show_timer();
            // A frame clock stopped by a caught panic starts again.
            let recording = matches!(rec, RecBadge::Recording { .. });
            if recording && !self.tick.is_running() {
                self.follow_clock(true);
            }
            return;
        }
        *self.shown.borrow_mut() = rec.clone();
        self.root.set_visible(*rec != RecBadge::None);
        let recording = matches!(rec, RecBadge::Recording { .. });
        self.badge.set_visible(recording);
        self.timer_plate.set_visible(recording);
        let pill = rec.pill();
        self.pill.set_visible(pill.is_some());
        if let Some(text) = &pill {
            self.pill_label.set_text(text);
        }
        if let RecBadge::Recording { take, .. } = rec {
            self.take.set_text(&format!("Take {}", take.number()));
        }
        if torch_look(reduced, motion::now()).1 {
            self.badge.add_css_class("glow");
        } else {
            self.badge.remove_css_class("glow");
        }
        self.show_timer();
        self.follow_clock(recording);
    }

    /// The torch and the timer on the frame clock while recording (the
    /// torch not under reduced motion; neither in test mode, whose clock is
    /// frozen).
    fn follow_clock(self: &Rc<Self>, recording: bool) {
        self.tick.stop();
        let reduced = self.reduced.get();
        self.badge.set_opacity(torch_look(reduced, motion::now()).0);
        if !recording || motion::one_shots_at_end() {
            return;
        }
        let weak = Rc::downgrade(self);
        self.tick
            .tick(&self.root, "stage: the REC cluster", move |_, _| {
                let Some(this) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                this.badge
                    .set_opacity(torch_look(this.reduced.get(), motion::now()).0);
                this.show_timer();
                glib::ControlFlow::Continue
            });
    }

    /// The timer: the last `RecordClock` plus the time since, at most 300 ms
    /// ahead of it.
    fn show_timer(&self) {
        let Some((ns, at)) = self.clock.get() else {
            return;
        };
        let lead = motion::now().saturating_sub(at).min(TIMER_LEAD);
        let shown = ns.saturating_add(u64::try_from(lead.as_nanos()).unwrap_or(0));
        let text = rec_timer(shown, &self.canvas.borrow());
        if self.timer.text() != text {
            self.timer.set_text(&text);
        }
    }
}

// -------------------------------------------------------------- plates

/// The trim scrub's plate, bottom-left (S10).
pub struct TrimPlateView {
    pub root: gtk::Box,
    title: gtk::Label,
    detail: gtk::Label,
    frame: gtk::Label,
}

impl Default for TrimPlateView {
    fn default() -> Self {
        Self::new()
    }
}

impl TrimPlateView {
    pub fn new() -> Self {
        let root = plate(gtk::Orientation::Horizontal, "stage-plate");
        root.set_halign(gtk::Align::Start);
        root.set_valign(gtk::Align::End);
        let title = label("", &["t-body-strong"]);
        let detail = label("", &["t-body"]);
        let frame = label("", &["t-mono"]);
        for l in [&title, &detail, &frame] {
            l.set_valign(gtk::Align::Baseline);
            root.append(l);
        }
        root.set_visible(false);
        Self {
            root,
            title,
            detail,
            frame,
        }
    }

    pub fn update(&self, plate: Option<&TrimPlate>) {
        self.root.set_visible(plate.is_some());
        if let Some(plate) = plate {
            self.title.set_text(&plate.title);
            self.detail.set_text(&plate.detail);
            self.frame.set_text(&plate.frame);
        }
    }
}

/// A centered plate with one line (D-05, the opening caption).
pub fn center_plate(text: &str) -> gtk::Label {
    let l = label(text, &["stage-plate", "t-body"]);
    l.set_halign(gtk::Align::Center);
    l.set_valign(gtk::Align::Center);
    l.set_can_target(false);
    l.set_visible(false);
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_torch_follows_its_keyframes() {
        let at = |ms: u64| torch_opacity(Duration::from_millis(ms));
        assert_eq!(at(0), 1.0, "frame A: bright");
        assert!((at(496) - 0.74).abs() < 1e-9, "31 %: frame B");
        assert!((at(1600) - 1.0).abs() < 1e-9, "the loop");
        // Linear between 0 % (1.00) and 9 % (0.82).
        assert!((at(72) - 0.91).abs() < 1e-9);
        for ms in (0..1600).step_by(7) {
            let v = at(ms);
            assert!((0.74..=1.0).contains(&v), "{ms}: {v}");
        }
    }
}
