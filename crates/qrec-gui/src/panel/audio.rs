//! C10 audio rows (92 px, S02, S06, S13b): line 1 is the role icon, the
//! label (`body-strong`, renamed inline), the id chip, the "Autocut" tag,
//! `M` and the overflow (Rename, Change source ▸, Use for autocut,
//! Remove); line 2 the C11 meter; line 3 the Gain and Sync fields.
//!
//! - `M` sends `AudioSet{muted}` on the scene the panel edits (the lane
//!   header's `M` is playback-only, D-08); a muted track's meter draws at
//!   35 %.
//! - Gain (−60 … 60 dB, step 0.5) and Sync (−5000 … 5000 ms, step 10)
//!   commit on Enter or blur: one `AudioSet` each.
//! - While controls are disabled (D-43: not Idle; no edit target, D-06)
//!   the meter stays live.
//! - Live, a track whose device isn't live reads "Device not found. Plug
//!   it in, or pick another source." in place of its meter.
//! - An edit belongs to the scene it was typed for: when the scene the
//!   panel edits changes under it, the fields show the new scene's values
//!   and a rename ends unsaved (R38); a change the person starts with a
//!   press commits the edit first (G§3.12 R12).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use qrec_core::ctx::AudioDeviceInfo;
use qrec_core::protocol::command::{AudioPatch, SceneTarget};
use qrec_core::view::panel::{self, AddGroup, AudioRow, NO_TRACKS, RenameOutcome};
use qrec_core::{AudioRole, State, TrackId};

use super::menus::{self, Menu};
use super::rows::empty_row;
use crate::Ctx;
use crate::guarded;
use crate::widgets::meter::Meter;
use crate::widgets::numeric::{self, NumericField};

thread_local! {
    /// Each audio row made so far, weakly; test mode only: what
    /// [`rows_alive`] counts.
    static ROWS: RefCell<Vec<glib::WeakRef<gtk::Box>>> = const { RefCell::new(Vec::new()) };
}

/// How many audio rows are alive (test mode; `testctl`'s `local`,
/// `audioRowsAlive`): the rows shown, once those a rebuild dropped are
/// gone (G§3.12 R11).
pub fn rows_alive() -> usize {
    ROWS.with(|rows| {
        let mut rows = rows.borrow_mut();
        rows.retain(|row| row.upgrade().is_some());
        rows.len()
    })
}

/// The audio rows. Cheap to clone; main thread only.
#[derive(Clone)]
pub struct AudioList {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    list: gtk::Box,
    empty: gtk::Box,
    rows: RefCell<Vec<Rc<AudioRowView>>>,
}

struct AudioRowView {
    track: TrackId,
    root: gtk::Box,
    icon: gtk::Image,
    name: gtk::EditableLabel,
    chip: gtk::Label,
    tag: gtk::Box,
    mute: gtk::ToggleButton,
    menu_button: gtk::MenuButton,
    menu: Menu,
    meter: Meter,
    missing: gtk::Box,
    missing_text: gtk::Label,
    gain_label: gtk::Label,
    gain: NumericField,
    sync_label: gtk::Label,
    sync: NumericField,
    model: RefCell<Option<AudioRow>>,
    /// The scene the row's fields and rename edit (`edit_target`).
    edits: RefCell<Option<Option<SceneTarget>>>,
    /// A rename is being ended unsaved.
    dropping: Cell<bool>,
    /// The toggle is being set from the model, not clicked.
    setting: Cell<bool>,
}

impl std::fmt::Debug for AudioList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioList")
            .field("rows", &self.inner.rows.borrow().len())
            .finish()
    }
}

impl AudioList {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("audio-list");
        list.update_property(&[gtk::accessible::Property::Label("Audio tracks")]);
        let empty = empty_row("qrec-audio-lines-symbolic", NO_TRACKS);
        root.append(&list);
        root.append(&empty);
        Self {
            inner: Rc::new(Inner {
                ctx: ctx.clone(),
                root,
                list,
                empty,
                rows: RefCell::new(Vec::new()),
            }),
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.inner.root.clone().upcast()
    }

    /// Shows `rows` of `state`; `devices` are the mics and the monitors
    /// discovery found (Change source).
    pub fn update(
        &self,
        rows: &[AudioRow],
        state: &State,
        devices: (&[AudioDeviceInfo], &[AudioDeviceInfo]),
    ) {
        let inner = &self.inner;
        let same = {
            let views = inner.rows.borrow();
            views.len() == rows.len() && views.iter().zip(rows).all(|(v, r)| v.track == r.track)
        };
        if !same {
            let old: Vec<Rc<AudioRowView>> = inner.rows.borrow_mut().drain(..).collect();
            for view in old {
                // A rename on a row that goes saves nothing.
                view.drop_rename();
                inner.list.remove(&view.root);
            }
            let mut views = Vec::with_capacity(rows.len());
            for row in rows {
                let view = Rc::new(AudioRowView::new(&inner.ctx, &row.track));
                wire(&inner.ctx, &view);
                inner.list.append(&view.root);
                views.push(view);
            }
            *inner.rows.borrow_mut() = views;
        }
        for (view, row) in inner.rows.borrow().iter().zip(rows) {
            view.show(row, state, devices);
        }
        inner.empty.set_visible(rows.is_empty());
        inner.list.set_visible(!rows.is_empty());
    }

    /// Starts the inline rename of `track` (the overflow's Rename).
    pub fn rename(&self, track: &str) {
        let name = self
            .inner
            .rows
            .borrow()
            .iter()
            .find(|v| v.track.as_str() == track)
            .map(|v| v.name.clone());
        if let Some(name) = name {
            name.set_can_target(true);
            name.set_focusable(true);
            name.start_editing();
            name.grab_focus();
        }
    }
}

/// The row's controls send their commands.
fn wire(ctx: &Ctx, view: &Rc<AudioRowView>) {
    let store = ctx.store.clone();
    let weak = Rc::downgrade(view);
    view.mute
        .connect_toggled(guarded!("panel: M", move |button| {
            let Some(view) = weak.upgrade() else { return };
            if view.setting.get() {
                return;
            }
            let patch = AudioPatch {
                muted: Some(button.is_active()),
                ..AudioPatch::default()
            };
            if let Some(cmd) = panel::audio_set(&store.state(), &view.track, patch) {
                store.dispatch(cmd);
            }
        }));
    // An invalid value's message is the field's tooltip, as long as its
    // stroke shows.
    for field in [&view.gain, &view.sync] {
        let widget = field.widget().downgrade();
        field.on_error(move |error| {
            if let Some(widget) = widget.upgrade() {
                widget.set_tooltip_text(error);
            }
        });
    }
    let store = ctx.store.clone();
    let weak = Rc::downgrade(view);
    view.gain.on_commit(move |text| {
        let Some(view) = weak.upgrade() else { return };
        match panel::parse_gain(text) {
            Ok(db) => {
                view.gain.set_error(None);
                let unchanged = view
                    .model
                    .borrow()
                    .as_ref()
                    .is_some_and(|m| (m.gain_db - db).abs() < 1e-9);
                if unchanged {
                    view.gain.revert();
                    return;
                }
                let patch = AudioPatch {
                    gain_db: Some(db),
                    ..AudioPatch::default()
                };
                if let Some(cmd) = panel::audio_set(&store.state(), &view.track, patch) {
                    store.dispatch(cmd);
                }
            }
            Err(why) => view.gain.set_error(Some(&why)),
        }
    });
    let store = ctx.store.clone();
    let weak = Rc::downgrade(view);
    view.sync.on_commit(move |text| {
        let Some(view) = weak.upgrade() else { return };
        match panel::parse_sync(text) {
            Ok(ms) => {
                view.sync.set_error(None);
                let unchanged = view
                    .model
                    .borrow()
                    .as_ref()
                    .is_some_and(|m| m.sync_offset_ms == ms);
                if unchanged {
                    view.sync.revert();
                    return;
                }
                let patch = AudioPatch {
                    sync_offset_ms: Some(ms),
                    ..AudioPatch::default()
                };
                if let Some(cmd) = panel::audio_set(&store.state(), &view.track, patch) {
                    store.dispatch(cmd);
                }
            }
            Err(why) => view.sync.set_error(Some(&why)),
        }
    });
    let store = ctx.store.clone();
    let track = view.track.clone();
    let menu = view.menu_button.clone();
    let weak = Rc::downgrade(view);
    view.name.connect_notify_local(
        Some("editing"),
        guarded!("panel: rename a track", move |label, _| {
            if label.is_editing() {
                return;
            }
            // The keyboard comes back to the overflow the rename came from.
            if label.has_focus() || label.focus_child().is_some() {
                menu.grab_focus();
            }
            label.set_can_target(false);
            label.set_focusable(false);
            if weak.upgrade().is_some_and(|view| view.dropping.get()) {
                return;
            }
            // A rename that sends nothing, or that the core refuses, puts
            // the label back (R46).
            match panel::track_rename_outcome(&store.state(), &track, &label.text()) {
                RenameOutcome::Send(cmd) => {
                    let (label, track) = (label.downgrade(), track.clone());
                    super::send_rename(&store, cmd, move |state| {
                        if let Some(label) = label.upgrade()
                            && !label.is_editing()
                            && let Some(name) = panel::track_label(state, &track)
                        {
                            label.set_text(&name);
                        }
                    });
                }
                RenameOutcome::Restore(name) => {
                    if label.text() != name {
                        label.set_text(&name);
                    }
                }
            }
        }),
    );
}

impl AudioRowView {
    fn new(ctx: &Ctx, track: &TrackId) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        if crate::testctl::mode().on {
            ROWS.with(|rows| rows.borrow_mut().push(root.downgrade()));
        }
        root.add_css_class("audio-row");
        root.set_widget_name(&format!("audio-row-{track}"));
        // Line 1.
        let line1 = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        line1.add_css_class("audio-line1");
        line1.set_widget_name(&format!("audio-line1-{track}"));
        let icon = gtk::Image::new();
        icon.add_css_class("role-icon");
        let name = super::rows::editable_name("t-body-strong");
        name.set_valign(gtk::Align::Center);
        name.set_widget_name(&format!("audio-name-{track}"));
        let chip = gtk::Label::new(Some(track.as_str()));
        chip.add_css_class("track-id");
        chip.add_css_class("t-mono-xs");
        chip.add_css_class("muted");
        chip.set_valign(gtk::Align::Center);
        chip.set_widget_name(&format!("audio-chip-{track}"));
        let tag = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tag.add_css_class("autocut-tag");
        tag.set_valign(gtk::Align::Center);
        tag.set_widget_name(&format!("audio-tag-{track}"));
        tag.append(&gtk::Image::from_icon_name("edit-cut-symbolic"));
        let tag_label = gtk::Label::new(Some("Autocut"));
        tag_label.add_css_class("t-caption");
        tag.append(&tag_label);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let mute = gtk::ToggleButton::with_label("M");
        mute.add_css_class("track-toggle");
        mute.set_focus_on_click(false);
        mute.set_valign(gtk::Align::Center);
        mute.set_widget_name(&format!("audio-mute-{track}"));
        let menu = Menu::new();
        let menu_button = menus::menu_button(&menu, &["flat", "icon-24"], "More");
        menu_button.set_icon_name("view-more-symbolic");
        menu_button.set_valign(gtk::Align::Center);
        menu_button.set_widget_name(&format!("audio-menu-{track}"));
        let refresh = {
            let ctx = ctx.clone();
            Rc::new(move || menus::refresh_sources(&ctx)) as Rc<dyn Fn()>
        };
        menus::track_menu(ctx, &menu_button, Some(refresh));
        line1.append(&icon);
        line1.append(&name);
        line1.append(&chip);
        line1.append(&tag);
        line1.append(&spacer);
        line1.append(&mute);
        line1.append(&menu_button);
        // Line 2: the meter, or why there is none.
        let meter = Meter::new(&ctx.media, &ctx.theme, "");
        meter.widget().add_css_class("audio-meter");
        meter
            .widget()
            .set_widget_name(&format!("audio-meter-{track}"));
        let missing = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        missing.add_css_class("device-missing");
        let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
        warning.add_css_class("warning-text");
        let missing_text = gtk::Label::new(None);
        missing_text.add_css_class("t-caption");
        missing_text.set_xalign(0.0);
        missing_text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        missing.append(&warning);
        missing.append(&missing_text);
        missing.set_visible(false);
        // Line 3.
        let line3 = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        line3.add_css_class("audio-line3");
        line3.set_widget_name(&format!("audio-line3-{track}"));
        let gain_label = gtk::Label::new(Some("Gain"));
        gain_label.add_css_class("t-caption");
        gain_label.add_css_class("muted");
        gain_label.add_css_class("field-caption");
        let gain = NumericField::new("dB", &format!("Gain of {track}"));
        gain.set_step(0.5);
        gain.set_format(numeric::tenths);
        gain.set_unit_aliases(&["db"]);
        gain.widget().add_css_class("audio-field");
        gain.widget()
            .set_widget_name(&format!("audio-gain-{track}"));
        let fill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fill.set_hexpand(true);
        let sync_label = gtk::Label::new(Some("Sync"));
        sync_label.add_css_class("t-caption");
        sync_label.add_css_class("muted");
        sync_label.add_css_class("field-caption");
        let sync = NumericField::new("ms", &format!("Sync of {track}"));
        sync.set_step(10.0);
        sync.set_format(numeric::signed);
        sync.widget().add_css_class("audio-field");
        sync.widget()
            .set_widget_name(&format!("audio-sync-{track}"));
        line3.append(&gain_label);
        line3.append(&gain.widget());
        line3.append(&fill);
        line3.append(&sync_label);
        line3.append(&sync.widget());
        root.append(&line1);
        root.append(&meter.widget());
        root.append(&missing);
        root.append(&line3);
        Self {
            track: track.clone(),
            root,
            icon,
            name,
            chip,
            tag,
            mute,
            menu_button,
            menu,
            meter,
            missing,
            missing_text,
            gain_label,
            gain,
            sync_label,
            sync,
            model: RefCell::new(None),
            edits: RefCell::new(None),
            dropping: Cell::new(false),
            setting: Cell::new(false),
        }
    }

    /// Ends a rename in progress without saving it (the old name back).
    fn drop_rename(&self) {
        if !self.name.is_editing() {
            return;
        }
        self.dropping.set(true);
        self.name.stop_editing(false);
        self.dropping.set(false);
    }

    fn show(
        &self,
        row: &AudioRow,
        state: &State,
        (mics, monitors): (&[AudioDeviceInfo], &[AudioDeviceInfo]),
    ) {
        // Another scene: an edit in progress was for the one that went. Edits
        // stopped (D-43: a recording): it can't land either, so it ends
        // unsaved rather than be refused (R46).
        let target = qrec_core::ui_normalize::edit_target(&state.project, &state.ui);
        let scene_changed = self.edits.borrow().as_ref() != Some(&target);
        *self.edits.borrow_mut() = Some(target);
        if scene_changed || !row.enabled {
            self.drop_rename();
            self.gain.reset(&numeric::tenths(row.gain_db));
            self.sync
                .reset(&numeric::signed(f64::from(row.sync_offset_ms)));
        }
        let scene = qrec_core::ui_normalize::canvas_scene(&state.project, &state.ui);
        let device = scene
            .track(&row.track)
            .map(|t| t.source.clone())
            .unwrap_or_default();
        self.meter.set_device(&device);
        self.meter.set_muted(row.muted);
        self.update_menu(row, state, mics, monitors);
        if self.model.borrow().as_ref() == Some(row) {
            return;
        }
        *self.model.borrow_mut() = Some(row.clone());
        self.icon.set_icon_name(Some(match row.role {
            AudioRole::Mic => "audio-input-microphone-symbolic",
            AudioRole::System => "audio-speakers-symbolic",
        }));
        if !self.name.is_editing() && self.name.text() != row.label {
            self.name.set_text(&row.label);
        }
        self.chip.set_text(row.track.as_str());
        self.tag.set_visible(row.autocut);
        self.setting.set(true);
        self.mute.set_active(row.muted);
        self.setting.set(false);
        let mute_label = format!(
            "{} {} in this scene",
            if row.muted { "Unmute" } else { "Mute" },
            row.label
        );
        self.mute
            .update_property(&[gtk::accessible::Property::Label(&mute_label)]);
        self.mute.set_tooltip_text(Some(&mute_label));
        menus::set_menu_label(&self.menu_button, &format!("More for track {}", row.label));
        self.name
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "Name of track {}",
                row.label
            ))]);
        self.gain.set_value(&numeric::tenths(row.gain_db));
        self.sync
            .set_value(&numeric::signed(f64::from(row.sync_offset_ms)));
        match &row.device_missing {
            Some(text) => {
                self.missing_text.set_text(text);
                self.missing.set_visible(true);
                self.meter.widget().set_visible(false);
            }
            None => {
                self.missing.set_visible(false);
                self.meter.widget().set_visible(true);
            }
        }
        // D-43: the controls dim; the name, the chips and the meter stay.
        for widget in [
            self.mute.upcast_ref::<gtk::Widget>(),
            self.menu_button.upcast_ref(),
            self.gain_label.upcast_ref(),
            self.sync_label.upcast_ref(),
        ] {
            widget.set_sensitive(row.enabled);
        }
        self.gain.set_sensitive(row.enabled);
        self.sync.set_sensitive(row.enabled);
        self.root
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "{} {}",
                row.label, row.track
            ))]);
    }

    /// The overflow: Rename, Change source ▸ (project scene), Use for
    /// autocut, Remove.
    fn update_menu(
        &self,
        row: &AudioRow,
        state: &State,
        mics: &[AudioDeviceInfo],
        monitors: &[AudioDeviceInfo],
    ) {
        let enabled = row.enabled;
        let sources = if enabled {
            panel::source_items(state, &row.track, mics, monitors)
        } else {
            Vec::new()
        };
        let autocut = (enabled && !row.autocut).then(|| panel::use_for_autocut(&row.track));
        let remove = enabled
            .then(|| panel::audio_remove(state, &row.track))
            .flatten();
        let signature = format!(
            "{enabled}|{}|{:?}|{:?}|{}",
            menus::add_signature(&sources),
            autocut.as_ref().map(|c| c.to_json()),
            remove.as_ref().map(|c| c.to_json()),
            row.track
        );
        let id = row.track.to_string();
        let group = match row.role {
            AudioRole::Mic => AddGroup::Mic,
            AudioRole::System => AddGroup::SystemAudio,
        };
        self.menu.update(signature, |menu| {
            menu.append_item(&menus::ui_item("Rename", "rename-track", &id, enabled));
            if sources.is_empty() {
                menu.append_item(&menus::command_item("Change source", None));
            } else {
                let sub = gtk::gio::Menu::new();
                for item in sources.iter().filter(|i| i.group == group) {
                    let cmd = (!item.in_use).then_some(&item.command);
                    sub.append_item(&menus::command_item(&item.label, cmd));
                }
                menu.append_submenu(Some("Change source"), &sub);
            }
            menu.append_item(&menus::command_item("Use for autocut", autocut.as_ref()));
            menu.append_item(&menus::command_item("Remove", remove.as_ref()));
        });
    }
}
