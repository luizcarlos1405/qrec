//! The transport row (BRIEF §2.4, named part `transport`, a focus region):
//! the timecode left (`now` in `text` over ` / total` in `muted`, and the
//! shuttle speed after it, D-26), or "Live · 1920×1080 · 30 fps" in Live;
//! five icon buttons centered (previous edit, previous frame, play/pause
//! 40×32, next frame, next edit), disabled in Live with the tooltip
//! "Switch to Program (Tab) to play the timeline"; Save frame right.
//! Every button runs its binding through the key router (G§3.7), so it
//! does what its key does. While playing the timecode follows the player
//! on the frame clock.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use qrec_core::keymap::{Action, Keymap, action_for};
use qrec_core::time::ns_to_frame_floor;
use qrec_core::timeline::total_frames;
use qrec_core::view::canvas::{TRANSPORT_LIVE_TOOLTIP, Transport, TransportLeft};
use qrec_core::view::{enabled, format};
use qrec_core::{Frame, State};

use crate::widgets::button::{self, accessible_name, binding_caps};
use crate::widgets::keycap::{self, CapSize, CapVariant};
use crate::{Ctx, guarded};

/// One transport button: its binding, icon and action.
struct Spec {
    binding: &'static str,
    icon: &'static str,
    action: &'static str,
}

const BUTTONS: [Spec; 5] = [
    Spec {
        binding: "seek.prev-edit",
        icon: "qrec-skip-back-symbolic",
        action: "Previous edit",
    },
    Spec {
        binding: "seek.prev-frame",
        icon: "qrec-chevron-left-symbolic",
        action: "Previous frame",
    },
    Spec {
        binding: "play.toggle",
        icon: "qrec-play-symbolic",
        action: "Play",
    },
    Spec {
        binding: "seek.next-frame",
        icon: "qrec-chevron-right-symbolic",
        action: "Next frame",
    },
    Spec {
        binding: "seek.next-edit",
        icon: "qrec-skip-forward-symbolic",
        action: "Next edit",
    },
];

/// A button's tooltip: its action and key, or why it is off in Live.
#[derive(Debug, Clone, Default)]
struct Tip {
    text: String,
    caps: Option<Vec<String>>,
}

pub struct TransportRow {
    pub root: gtk::CenterBox,
    now: gtk::Label,
    total: gtk::Label,
    rate: gtk::Label,
    live: gtk::Label,
    buttons: Vec<(gtk::Button, Rc<RefCell<Tip>>)>,
    save: gtk::Button,
    /// Playing: the timecode follows the player on the frame clock.
    playing: Cell<bool>,
    /// The timecode on the frame clock while playing (a slot: empty again
    /// after a caught panic, and the next update starts it, R65).
    tick: crate::guard::Slot,
    total_frames: Cell<Frame>,
    ctx: Ctx,
}

fn tooltip(button: &gtk::Button, tip: &Rc<RefCell<Tip>>) {
    button.set_has_tooltip(true);
    let tip = Rc::clone(tip);
    button.connect_query_tooltip(guarded!(
        "transport: a tooltip",
        move |_, _, _, _, tooltip| {
            let tip = tip.borrow();
            // 8 px apart by CSS (`.qrec-tooltip`, common.css).
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.add_css_class("qrec-tooltip");
            row.append(&gtk::Label::new(Some(&tip.text)));
            if let Some(caps) = &tip.caps {
                row.append(&keycap::chord(caps, CapSize::Sm, CapVariant::Plain));
            }
            tooltip.set_custom(Some(&row));
            true
        }
    ));
}

fn icon_button(spec: &Spec, keymap: &Keymap) -> (gtk::Button, Rc<RefCell<Tip>>) {
    let button = gtk::Button::from_icon_name(spec.icon);
    button.add_css_class("qrec-btn");
    button.add_css_class("flat");
    button.add_css_class("icon-32");
    button.set_focus_on_click(false);
    if spec.binding == "play.toggle" {
        button.add_css_class("play");
    }
    let caps = binding_caps(keymap, spec.binding);
    button.update_property(&[gtk::accessible::Property::Label(&accessible_name(
        spec.action,
        caps.as_deref(),
    ))]);
    let tip = Rc::new(RefCell::new(Tip {
        text: spec.action.to_owned(),
        caps,
    }));
    tooltip(&button, &tip);
    (button, tip)
}

impl TransportRow {
    pub fn new(ctx: &Ctx) -> Rc<Self> {
        let keymap = ctx.store.keymap();
        let root = gtk::CenterBox::new();
        root.set_widget_name("transport");
        root.add_css_class("transport");

        let left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        left.set_valign(gtk::Align::Center);
        let now = gtk::Label::new(None);
        now.add_css_class("t-mono");
        let total = gtk::Label::new(None);
        total.add_css_class("t-mono");
        total.add_css_class("muted");
        let rate = gtk::Label::new(None);
        rate.add_css_class("t-mono");
        rate.add_css_class("muted");
        let live = gtk::Label::new(None);
        live.add_css_class("t-mono");
        live.add_css_class("muted");
        for l in [&now, &total, &rate, &live] {
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            left.append(l);
        }
        root.set_start_widget(Some(&left));

        let center = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        center.add_css_class("transport-buttons");
        let mut buttons = Vec::new();
        for spec in &BUTTONS {
            let (button, tip) = icon_button(spec, &keymap);
            let keys = ctx.keys.clone();
            let id = spec.binding;
            button.connect_clicked(guarded!("transport: a button", move |_| keys.run_binding(id)));
            center.append(&button);
            buttons.push((button, tip));
        }
        root.set_center_widget(Some(&center));

        let save = gtk::Button::new();
        save.add_css_class("qrec-btn");
        save.add_css_class("flat");
        save.set_focus_on_click(false);
        // 8 px apart by CSS (`button.qrec-btn > box`, common.css).
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.append(&gtk::Image::from_icon_name("qrec-camera-symbolic"));
        let save_label = gtk::Label::new(Some("Save frame"));
        save_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&save_label);
        save.set_child(Some(&content));
        // Its key, also after `keys.toml` changes (WP10).
        button::follow_keymap(&save, &keymap, |save, keymap| {
            let caps = binding_caps(keymap, "frame.save");
            if let Some(content) = save.child().and_downcast::<gtk::Box>() {
                button::show_binding_cap(
                    &content,
                    caps.as_deref(),
                    button::BINDING_CAP,
                    CapSize::Md,
                    CapVariant::Plain,
                );
            }
            save.update_property(&[gtk::accessible::Property::Label(&accessible_name(
                "Save frame",
                caps.as_deref(),
            ))]);
        });
        let keys = ctx.keys.clone();
        save.connect_clicked(guarded!("transport: save frame", move |_| keys
            .run_binding("frame.save")));
        root.set_end_widget(Some(&save));

        Rc::new(Self {
            root,
            now,
            total,
            rate,
            live,
            buttons,
            save,
            playing: Cell::new(false),
            tick: crate::guard::Slot::new(),
            total_frames: Cell::new(0),
            ctx: ctx.clone(),
        })
    }

    /// Shows `t` (from `view::canvas::transport`) for `state`.
    pub fn update(self: &Rc<Self>, t: &Transport, state: &State, playing: bool) {
        let follows = playing && matches!(t.left, TransportLeft::Timecode { .. });
        self.total_frames
            .set(total_frames(&state.project.timeline.clips));
        self.follow_player(follows);
        match &t.left {
            TransportLeft::Live { text } => {
                self.live.set_text(text);
                self.live.set_visible(true);
                for l in [&self.now, &self.total, &self.rate] {
                    l.set_visible(false);
                }
            }
            TransportLeft::Timecode { now, total } => {
                self.live.set_visible(false);
                if !follows {
                    self.now.set_text(now);
                }
                self.total.set_text(&format!(" / {total}"));
                self.now.set_visible(true);
                self.total.set_visible(true);
                match &t.rate {
                    Some(rate) => {
                        self.rate.set_text(&format!(" {rate}"));
                        self.rate.set_visible(true);
                    }
                    None => self.rate.set_visible(false),
                }
            }
        }
        let keymap = self.ctx.store.keymap();
        for ((button, tip), spec) in self.buttons.iter().zip(&BUTTONS) {
            let mut action = spec.action;
            if spec.binding == "play.toggle" {
                let (icon, label) = if playing {
                    ("qrec-pause-symbolic", "Pause")
                } else {
                    ("qrec-play-symbolic", "Play")
                };
                button.set_icon_name(icon);
                action = label;
            }
            let runs = matches!(
                action_for(spec.binding, state),
                Some(Action::Run(_) | Action::Gui)
            );
            let on = t.enabled && runs;
            button.set_sensitive(on);
            let caps = binding_caps(&keymap, spec.binding);
            *tip.borrow_mut() = if !t.enabled && matches!(t.left, TransportLeft::Live { .. }) {
                Tip {
                    text: TRANSPORT_LIVE_TOOLTIP.to_owned(),
                    caps: None,
                }
            } else {
                Tip {
                    text: action.to_owned(),
                    caps: caps.clone(),
                }
            };
            button.update_property(&[gtk::accessible::Property::Label(&accessible_name(
                action,
                caps.as_deref(),
            ))]);
        }
        // Save frame stays on (BRIEF §2.4) unless the shared gates refuse
        // it (D-43); an empty timeline says why when pressed.
        let save_on = match action_for("frame.save", state) {
            Some(Action::Run(cmd)) => enabled(state, &cmd).is_ok(),
            _ => true,
        };
        self.save.set_sensitive(save_on);
    }

    /// While playing, the timecode is the player's frame on each frame.
    fn follow_player(self: &Rc<Self>, playing: bool) {
        self.playing.set(playing);
        if !playing {
            self.tick.stop();
            return;
        }
        if self.tick.is_running() {
            return;
        }
        let weak = Rc::downgrade(self);
        self.tick
            .tick(&self.root, "transport: the timecode", move |_, _| {
                let Some(this) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if let Some(ns) = this.ctx.media.position_ns() {
                    let fps = this.ctx.store.state().project.canvas.fps;
                    let frame = (ns_to_frame_floor(i64::try_from(ns).unwrap_or(i64::MAX), fps)
                        .max(0) as Frame)
                        .min(this.total_frames.get());
                    let text = format::timecode(frame, fps);
                    if this.now.text() != text {
                        this.now.set_text(&text);
                    }
                }
                glib::ControlFlow::Continue
            });
    }
}
