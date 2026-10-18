//! The track header column (C17, `track-headers`, 176 px): the video row's
//! header ("Video", "Thumbnails: Face (L2)") and a header per lane (role
//! icon, label, id chip, the scissors on the autocut track, `M` and `S`).
//! `M` mutes the lane for playback only (D-08, `SetUi{lane_mute}`), `S`
//! solos it (`SetUi{solo}`); both work while recording (D-43).

use std::cell::RefCell;

use gtk::prelude::*;
use qrec_core::model::AudioRole;
use qrec_core::view::timeline::{LaneHeader, Rows, VideoHeader};
use qrec_core::{Command, State};

use crate::store::Store;
use crate::{Ctx, guarded};

/// What the column shows: the headers at the rows' heights and the UI
/// scale (its bits).
type Shown = (VideoHeader, Vec<LaneHeader>, Rows, u64);

/// The header column.
pub struct Headers {
    pub root: gtk::Box,
    store: Store,
    /// What the column shows now (rebuilt when it changes).
    shown: RefCell<Option<Shown>>,
}

impl Headers {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("track-headers");
        root.add_css_class("tl-headers");
        root.set_size_request(qrec_core::view::timeline::HEADER_WIDTH as i32, -1);
        root.set_vexpand(true);
        Self {
            root,
            store: ctx.store.clone(),
            shown: RefCell::new(None),
        }
    }

    /// Shows `state`'s headers at `rows`' heights at UI scale `scale`, the
    /// canvas rows' whole px (`Rows::px_heights`, D-46); rebuilt only when
    /// they change.
    pub fn update(&self, state: &State, rows: Rows, scale: f64) {
        let video = qrec_core::view::timeline::video_header(state);
        let lanes = qrec_core::view::timeline::lane_headers(state);
        let key: Shown = (video, lanes, rows, scale.to_bits());
        if self.shown.borrow().as_ref() == Some(&key) {
            return;
        }
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        let (video, lanes, _, _) = &key;
        let (video_px, lane_px) = rows.px_heights(lanes.len(), scale);
        // The compact rows' 48 px lanes take 3 px paddings (G§3.15).
        if rows == qrec_core::view::timeline::COMPACT_ROWS {
            self.root.add_css_class("compact");
        } else {
            self.root.remove_css_class("compact");
        }
        self.root.append(&video_header(video, video_px));
        for (lane, (gap_px, height)) in lanes.iter().zip(lane_px) {
            if gap_px > 0 {
                let gap = gtk::Box::new(gtk::Orientation::Vertical, 0);
                gap.set_size_request(-1, gap_px);
                self.root.append(&gap);
            }
            self.root.append(&lane_header(&self.store, lane, height));
        }
        *self.shown.borrow_mut() = Some(key);
    }
}

fn video_header(header: &VideoHeader, height: i32) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.set_widget_name("header-video");
    b.add_css_class("tl-video-header");
    b.set_size_request(-1, height);
    // Its centered content expands inside it, not the column.
    b.set_vexpand(false);
    b.set_valign(gtk::Align::Start);
    let inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
    inner.add_css_class("tl-video-lines");
    inner.set_valign(gtk::Align::Center);
    inner.set_vexpand(true);
    let title = gtk::Label::new(Some(&header.title));
    title.add_css_class("tl-header-title");
    title.set_xalign(0.0);
    inner.append(&title);
    if let Some(caption) = &header.caption {
        let c = gtk::Label::new(Some(caption));
        c.add_css_class("t-caption");
        c.add_css_class("muted");
        c.set_xalign(0.0);
        c.set_ellipsize(gtk::pango::EllipsizeMode::End);
        inner.append(&c);
    }
    b.append(&inner);
    b
}

fn lane_header(store: &Store, header: &LaneHeader, height: i32) -> gtk::Box {
    let lane = &header.lane;
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.set_widget_name(&format!("header-{}", lane.track));
    b.add_css_class("tl-lane-header");
    b.set_size_request(-1, height);
    b.set_vexpand(false);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.add_css_class("tl-lane-line");
    let role = gtk::Image::from_icon_name(match lane.role {
        AudioRole::Mic => "qrec-mic-symbolic",
        AudioRole::System => "qrec-volume-2-symbolic",
    });
    line.append(&role);
    let label = gtk::Label::new(Some(&lane.label));
    label.add_css_class("tl-header-title");
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_xalign(0.0);
    line.append(&label);
    let chip = gtk::Label::new(Some(lane.track.as_str()));
    chip.add_css_class("tl-chip");
    chip.add_css_class("t-mono-xs");
    chip.add_css_class("muted");
    line.append(&chip);
    b.append(&line);
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tools.add_css_class("tl-lane-tools");
    tools.set_vexpand(true);
    tools.set_valign(gtk::Align::End);
    let scissors = gtk::Image::from_icon_name("qrec-scissors-symbolic");
    scissors.add_css_class("tl-scissors");
    scissors.set_opacity(if lane.autocut { 1.0 } else { 0.0 });
    if lane.autocut {
        scissors.set_tooltip_text(Some("Autocut detects silence on this track"));
    }
    tools.append(&scissors);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    tools.append(&spacer);
    tools.append(&lane_toggle(
        store,
        "M",
        "mute",
        lane.muted,
        &header.mute_tooltip,
        header.mute.clone(),
    ));
    tools.append(&lane_toggle(
        store,
        "S",
        "solo",
        lane.solo,
        &header.solo_tooltip,
        header.solo.clone(),
    ));
    b.append(&tools);
    b
}

fn lane_toggle(
    store: &Store,
    letter: &str,
    class: &str,
    active: bool,
    tooltip: &str,
    cmd: Command,
) -> gtk::ToggleButton {
    let t = gtk::ToggleButton::with_label(letter);
    t.add_css_class("track-toggle");
    t.add_css_class(class);
    t.set_focus_on_click(false);
    t.set_active(active);
    t.set_tooltip_text(Some(tooltip));
    t.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    let store = store.clone();
    t.connect_clicked(guarded!("timeline: a lane toggle", move |_| {
        store.dispatch(cmd.clone())
    }));
    t
}
