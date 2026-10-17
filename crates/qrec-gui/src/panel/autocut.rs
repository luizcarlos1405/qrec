//! The autocut page (S12, D-15): it replaces the panel's content, a pushed
//! page with a back button. "Detect silence on", four sliders with value
//! fields, the summary of the newest dry run, the region list (a click
//! seeks), "Run autocut after every take", the note, and Cancel / Apply
//! autocut.
//!
//! - Tuning is a draft (`view::autocut::AutocutDraft`): no history. Each
//!   change sends a dry run 250 ms later, one at a time
//!   ([`super::dry_run::DryRuns`]); the summary title shows a spinner and
//!   "Analyzing…" meanwhile, and `LocalUi.autocut_preview` carries the
//!   hatches the timeline draws.
//! - Apply sends `AutocutSettings{patch}` with the changed fields (if any),
//!   then `Autocut{dry_run: false}`, and closes the page; the cut's answer
//!   toasts "Autocut take 8: removed 4 silences (6.2 s)" with Undo (S25).
//! - Cancel, the back button and `Esc` close it and send nothing.
//! - The switch "Run autocut after every take" is saved at once.
//! - `Enter` applies (the page's Apply is the window's default widget
//!   while it shows).
//! - The page open always shows (R39): in the Narrow layout it shows in
//!   the layers sheet (opening it pushes one), and goes with it. The sheet
//!   closing (`Esc`, a click beside it, the header's layers button) closes
//!   the page, a window narrowing under the open page shows the sheet, and
//!   the page's own close takes away a sheet it pushed ([`sheet_step`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use qrec_core::protocol::command::SeekTarget;
use qrec_core::protocol::reply::AutocutPlanView;
use qrec_core::protocol::{ToastAction, ToastLevel, UiAction};
use qrec_core::view::autocut::{
    self, AUTO_RUN, AutocutDraft, AutocutSummary, DETECT_ON, DryRunKey, Knob, NOTE, RegionRow,
    TITLE,
};
use qrec_core::view::notices::{Notice, NoticeAction, error_notice};
use qrec_core::view::{AutocutPreview, PanelPageKind, format};
use qrec_core::{CmdResult, Command, State, TimeArg, TrackId};

use super::dry_run::{DryRuns, Step};
use crate::media::io::BusyToken;
use crate::store::Reply;
use crate::store::local::{LayoutWidth, LocalUi, Overlay};
use crate::widgets::Spinner;
use crate::widgets::button::{self, Kind};
use crate::widgets::numeric::{self, NumericField};
use crate::widgets::scale_tooltip::{self, ValueTooltip};
use crate::{Ctx, guard, guarded};

/// One slider row.
struct KnobView {
    knob: Knob,
    scale: gtk::Scale,
    field: NumericField,
    _tooltip: ValueTooltip,
}

/// The page. Cheap to clone; main thread only.
#[derive(Clone)]
pub struct AutocutPage {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    subtitle: gtk::Label,
    dropdown: gtk::DropDown,
    tracks: RefCell<Vec<TrackId>>,
    knobs: Vec<KnobView>,
    spinner: Spinner,
    summary_title: gtk::Label,
    summary_mono: gtk::Label,
    summary_rows: gtk::Grid,
    regions: gtk::ListBox,
    region_rows: RefCell<Vec<(gtk::ListBoxRow, RegionRow)>>,
    selected_region: Cell<Option<usize>>,
    switch: gtk::Switch,
    apply: gtk::Button,
    runs: RefCell<DryRuns>,
    timer: RefCell<Option<glib::SourceId>>,
    /// Held while the debounce runs: the dry run it leads to is work
    /// `waitIdle` waits for (G§3.18), from the change to its reply (the
    /// request counts once sent).
    debounce_busy: RefCell<Option<BusyToken>>,
    draft: RefCell<AutocutDraft>,
    /// The dry run the page wants now (draft and scope) and what its answer
    /// is worked out on (the clips, the setting's track: R40, R76).
    wanted: RefCell<Option<DryRunKey>>,
    summary: RefCell<Option<AutocutSummary>>,
    spans: RefCell<Vec<qrec_core::view::AutocutPreviewSpan>>,
    open: Cell<bool>,
    setting: Cell<bool>,
    clock: Instant,
    /// The page pushed the Narrow layout's sheet it shows in (its close
    /// takes it away; one opened before the page stays).
    pushed_sheet: Cell<bool>,
    /// The layout was Narrow at the last `LocalUi` change.
    narrow: Cell<bool>,
    /// The window narrowed under the open page: the sheet shows it at the
    /// next idle.
    sheet_due: Cell<bool>,
}

/// What the open page does when `LocalUi` changes (R39).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetStep {
    /// Nothing: the page shows (docked, or in the sheet), or is closed.
    Stay,
    /// The Narrow layout's sheet went (`Esc`, a click beside it, the
    /// header's layers button): the page goes with it, as its Cancel.
    Close,
    /// The window narrowed under the open page: the sheet shows it.
    ShowSheet,
}

/// [`SheetStep`] for a page that is `open` (and shows: `panel_page`),
/// given whether the layout was Narrow at the change before `local`.
pub fn sheet_step(open: bool, was_narrow: bool, local: &LocalUi) -> SheetStep {
    let hidden = local.layout.width == LayoutWidth::Narrow
        && !local.overlays.contains(&Overlay::LayersSheet);
    if !open || local.panel_page != PanelPageKind::Autocut || !hidden {
        SheetStep::Stay
    } else if was_narrow {
        SheetStep::Close
    } else {
        SheetStep::ShowSheet
    }
}

impl std::fmt::Debug for AutocutPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutocutPage")
            .field("open", &self.inner.open.get())
            .field("draft", &*self.inner.draft.borrow())
            .finish()
    }
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    for class in classes {
        label.add_css_class(class);
    }
    label.set_xalign(0.0);
    label
}

impl AutocutPage {
    pub fn new(ctx: &Ctx) -> Self {
        let keymap = ctx.store.keymap();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("autocut-page");
        root.set_widget_name("autocut-page");
        // The header: back, the title, the subtitle.
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("panel-header");
        header.add_css_class("autocut-header");
        let back = button::icon("go-previous-symbolic", "Back", Some("cancel"), &keymap);
        back.remove_css_class("icon-32");
        back.add_css_class("icon-24");
        back.set_widget_name("autocut-back");
        back.set_valign(gtk::Align::Center);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_valign(gtk::Align::Center);
        titles.set_hexpand(true);
        let title = label(TITLE, &["t-title"]);
        let subtitle = label("", &["t-caption", "muted"]);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        titles.append(&title);
        titles.append(&subtitle);
        header.append(&back);
        header.append(&titles);
        // The controls.
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.add_css_class("autocut-content");
        let detect = gtk::Box::new(gtk::Orientation::Vertical, 4);
        detect.add_css_class("autocut-block");
        detect.append(&label(DETECT_ON, &["t-caption", "muted", "field-caption"]));
        let dropdown = gtk::DropDown::from_strings(&[]);
        dropdown.add_css_class("autocut-track");
        dropdown.set_widget_name("autocut-track");
        dropdown.set_hexpand(true);
        dropdown.update_property(&[gtk::accessible::Property::Label(DETECT_ON)]);
        // Its open list is a menu: Esc closes the list, not the page (R32).
        super::menus::track_dropdown(ctx, &dropdown);
        detect.append(&dropdown);
        content.append(&detect);
        let mut knobs = Vec::new();
        for knob in Knob::ALL {
            let block = gtk::Box::new(gtk::Orientation::Vertical, 4);
            block.add_css_class("autocut-block");
            block.add_css_class("knob");
            block.append(&label(
                knob.label(),
                &["t-caption", "muted", "field-caption"],
            ));
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let (min, max, step) = knob.range();
            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
            scale.set_draw_value(false);
            scale.set_hexpand(true);
            scale.set_valign(gtk::Align::Start);
            scale.set_focus_on_click(false);
            scale.update_property(&[gtk::accessible::Property::Label(knob.label())]);
            // Five ticks; the range's ends labelled under the track's ends
            // (S12: "−60" flush left, "−10" flush right).
            for i in 0..5 {
                let v = min + (max - min) * f64::from(i) / 4.0;
                scale.add_mark(v, gtk::PositionType::Bottom, None);
            }
            let (low, high) = knob.end_labels();
            let ends = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            ends.add_css_class("knob-ends");
            let low = label(&low, &["t-mono-xs", "muted"]);
            low.set_hexpand(true);
            let high = label(&high, &["t-mono-xs", "muted"]);
            high.set_xalign(1.0);
            ends.append(&low);
            ends.append(&high);
            let track = gtk::Box::new(gtk::Orientation::Vertical, 0);
            track.set_hexpand(true);
            track.set_widget_name(&format!("knob-track-{}", knobs.len()));
            scale.set_widget_name(&format!("knob-scale-{}", knobs.len()));
            track.append(&scale);
            track.append(&ends);
            let field = NumericField::new(knob.unit(), knob.label());
            field.set_step(step);
            field.set_format(numeric::whole);
            // The spellings `Knob::parse` takes ("db" for the threshold).
            field.set_unit_aliases(&knob.units()[1..]);
            field.widget().add_css_class("knob-field");
            field
                .widget()
                .set_widget_name(&format!("knob-field-{}", knobs.len()));
            field.widget().set_valign(gtk::Align::Start);
            row.append(&track);
            row.append(&field.widget());
            block.append(&row);
            content.append(&block);
            let unit = knob.unit();
            let tooltip = scale_tooltip::attach(
                &scale,
                move |v| format!("{} {unit}", numeric::whole(v)),
                // The draft follows the slider as it moves: nothing to send.
                |_| false,
            );
            knobs.push(KnobView {
                knob,
                scale,
                field,
                _tooltip: tooltip,
            });
        }
        // The summary.
        let summary = gtk::Box::new(gtk::Orientation::Vertical, 2);
        summary.add_css_class("autocut-summary");
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let spinner = Spinner::new(16);
        spinner.set_valign(gtk::Align::Center);
        spinner.set_visible(false);
        let summary_title = label("", &["t-title"]);
        summary_title.set_valign(gtk::Align::Baseline);
        let summary_mono = label("", &["summary-mono"]);
        summary_mono.set_valign(gtk::Align::Baseline);
        title_row.append(&spinner);
        title_row.append(&summary_title);
        title_row.append(&summary_mono);
        let summary_rows = gtk::Grid::new();
        summary_rows.add_css_class("summary-rows");
        summary_rows.set_column_spacing(12);
        summary_rows.set_row_spacing(4);
        summary.append(&title_row);
        summary.append(&summary_rows);
        content.append(&summary);
        let regions = gtk::ListBox::new();
        regions.add_css_class("qrec-list");
        regions.add_css_class("region-list");
        regions.set_selection_mode(gtk::SelectionMode::None);
        regions.set_widget_name("autocut-regions");
        regions.update_property(&[gtk::accessible::Property::Label("Silences")]);
        content.append(&regions);
        let switch_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        switch_row.add_css_class("autocut-switch-row");
        let switch_label = label(AUTO_RUN, &["t-body"]);
        switch_label.set_hexpand(true);
        let switch = gtk::Switch::new();
        switch.set_valign(gtk::Align::Center);
        switch.update_property(&[gtk::accessible::Property::Label(AUTO_RUN)]);
        switch_row.append(&switch_label);
        switch_row.append(&switch);
        content.append(&switch_row);
        let note = label(NOTE, &["t-caption", "muted", "autocut-note"]);
        note.set_wrap(true);
        content.append(&note);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&content));
        // The footer.
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        footer.add_css_class("autocut-footer");
        footer.set_halign(gtk::Align::Fill);
        let fill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fill.set_hexpand(true);
        let esc = button::binding_caps(&keymap, "cancel").unwrap_or_else(|| vec!["Esc".to_owned()]);
        let cancel = labeled_with_caps("Cancel", &esc, Kind::Secondary);
        cancel.set_widget_name("autocut-cancel");
        let apply = labeled_with_caps("Apply autocut", &["Enter".to_owned()], Kind::Primary);
        apply.set_widget_name("autocut-apply");
        footer.append(&fill);
        footer.append(&cancel);
        footer.append(&apply);
        root.append(&header);
        root.append(&scroller);
        root.append(&footer);
        let page = Self {
            inner: Rc::new(Inner {
                ctx: ctx.clone(),
                root,
                subtitle,
                dropdown,
                tracks: RefCell::new(Vec::new()),
                knobs,
                spinner,
                summary_title,
                summary_mono,
                summary_rows,
                regions,
                region_rows: RefCell::new(Vec::new()),
                selected_region: Cell::new(None),
                switch,
                apply,
                runs: RefCell::new(DryRuns::new()),
                timer: RefCell::new(None),
                debounce_busy: RefCell::new(None),
                draft: RefCell::new(AutocutDraft::default()),
                wanted: RefCell::new(None),
                summary: RefCell::new(None),
                spans: RefCell::new(Vec::new()),
                open: Cell::new(false),
                setting: Cell::new(false),
                clock: Instant::now(),
                pushed_sheet: Cell::new(false),
                narrow: Cell::new(ctx.store.local().layout.width == LayoutWidth::Narrow),
                sheet_due: Cell::new(false),
            }),
        };
        page.wire(&back, &cancel);
        page
    }

    pub fn widget(&self) -> gtk::Widget {
        self.inner.root.clone().upcast()
    }

    pub fn is_open(&self) -> bool {
        self.inner.open.get()
    }

    fn now_ms(&self) -> u64 {
        self.inner.clock.elapsed().as_millis() as u64
    }

    fn wire(&self, back: &gtk::Button, cancel: &gtk::Button) {
        let inner = &self.inner;
        for button in [back, cancel] {
            let weak = Rc::downgrade(inner);
            button.connect_clicked(guarded!("autocut: cancel", move |_| {
                if let Some(inner) = weak.upgrade() {
                    AutocutPage { inner }.close();
                }
            }));
        }
        let weak = Rc::downgrade(inner);
        inner
            .apply
            .connect_clicked(guarded!("autocut: apply", move |_| {
                if let Some(inner) = weak.upgrade() {
                    AutocutPage { inner }.apply();
                }
            }));
        for (i, knob) in inner.knobs.iter().enumerate() {
            let weak = Rc::downgrade(inner);
            knob.scale
                .connect_value_changed(guarded!("autocut: slider", move |scale| {
                    if let Some(inner) = weak.upgrade() {
                        AutocutPage { inner }.tuned(i, scale.value(), true);
                    }
                }));
            let weak = Rc::downgrade(inner);
            knob.field.on_commit(move |text| {
                if let Some(inner) = weak.upgrade() {
                    let page = AutocutPage { inner };
                    let knob = page.inner.knobs[i].knob;
                    match knob.parse(text) {
                        Some(v) => page.tuned(i, v, false),
                        None => page.inner.knobs[i].field.revert(),
                    }
                }
            });
        }
        let weak = Rc::downgrade(inner);
        inner
            .dropdown
            .connect_selected_notify(guarded!("autocut: track", move |dropdown| {
                let Some(inner) = weak.upgrade() else { return };
                if inner.setting.get() {
                    return;
                }
                let track = inner
                    .tracks
                    .borrow()
                    .get(dropdown.selected() as usize)
                    .cloned();
                let page = AutocutPage { inner };
                let mut draft = page.inner.draft.borrow().clone();
                draft.track = track;
                page.set_draft(draft);
            }));
        let weak = Rc::downgrade(inner);
        inner
            .switch
            .connect_active_notify(guarded!("autocut: after every take", move |switch| {
                let Some(inner) = weak.upgrade() else { return };
                if inner.setting.get() {
                    return;
                }
                inner
                    .ctx
                    .store
                    .dispatch(autocut::auto_run(switch.is_active()));
            }));
        let weak = Rc::downgrade(inner);
        inner
            .regions
            .connect_row_activated(guarded!("autocut: a region", move |_, row| {
                if let Some(inner) = weak.upgrade() {
                    AutocutPage { inner }.region_clicked(row);
                }
            }));
        // The page shows in the Narrow layout's sheet, and goes with it.
        let weak = Rc::downgrade(inner);
        inner
            .ctx
            .store
            .subscribe_local(move |local| {
                if let Some(inner) = weak.upgrade() {
                    AutocutPage { inner }.follow_sheet(local);
                }
            })
            .detach();
    }

    /// `LocalUi` changed: the open page goes with the Narrow layout's
    /// sheet, or shows it when the window narrows under it (R39). Edits
    /// made here are queued until the subscribers have run.
    fn follow_sheet(&self, local: &LocalUi) {
        let inner = &self.inner;
        let was_narrow = inner
            .narrow
            .replace(local.layout.width == LayoutWidth::Narrow);
        if inner.sheet_due.get() {
            // The sheet is about to show the page (below).
            return;
        }
        match sheet_step(inner.open.get(), was_narrow, local) {
            SheetStep::Stay => {}
            SheetStep::Close => self.close(),
            SheetStep::ShowSheet => {
                // Once this change's queued edits are done: narrowing hides
                // the window's sidebar, and its follower drops a sheet then.
                inner.sheet_due.set(true);
                let weak = Rc::downgrade(inner);
                guard::idle_local_once("autocut: the sheet shows the page", move || {
                    if let Some(inner) = weak.upgrade() {
                        inner.sheet_due.set(false);
                        AutocutPage { inner }.show_in_sheet();
                    }
                });
            }
        }
    }

    /// The window narrowed under the open page: the sheet shows it (unless
    /// the page closed, or the window widened, meanwhile).
    fn show_in_sheet(&self) {
        let inner = &self.inner;
        let due = {
            let local = inner.ctx.store.local();
            sheet_step(inner.open.get(), false, &local) == SheetStep::ShowSheet
        };
        if !due {
            return;
        }
        inner.pushed_sheet.set(true);
        inner.ctx.store.update_local(|l| {
            if !l.overlays.contains(&Overlay::LayersSheet) {
                l.overlays.push(Overlay::LayersSheet);
            }
        });
    }

    /// Opens the page on the project's settings and sends its first dry
    /// run at once.
    pub fn open(&self) {
        let inner = &self.inner;
        let state = inner.ctx.store.state();
        inner.open.set(true);
        *inner.draft.borrow_mut() = AutocutDraft::from(&state.project);
        *inner.summary.borrow_mut() = None;
        inner.spans.borrow_mut().clear();
        inner.selected_region.set(None);
        self.show_draft(&state);
        self.show_summary();
        let key = autocut::dry_run_key(&state, &inner.draft.borrow());
        let cmd = key.cmd.clone();
        *inner.wanted.borrow_mut() = Some(key);
        let step = inner.runs.borrow_mut().open(cmd, self.now_ms());
        // Narrow: the page shows in the sheet (one already open stays the
        // person's when the page closes).
        let push = {
            let local = inner.ctx.store.local();
            local.layout.width == LayoutWidth::Narrow
                && !local.overlays.contains(&Overlay::LayersSheet)
        };
        inner.pushed_sheet.set(push);
        inner.ctx.store.update_local(move |l| {
            l.panel_page = PanelPageKind::Autocut;
            if push && !l.overlays.contains(&Overlay::LayersSheet) {
                l.overlays.push(Overlay::LayersSheet);
            }
        });
        self.carry_out(step);
        self.publish();
        self.update(&state);
        if let Some(window) = inner.root.root().and_downcast::<gtk::Window>() {
            window.set_default_widget(Some(&inner.apply));
        }
    }

    /// Closes the page (Cancel, back, `Esc`, or after Apply): nothing more
    /// is sent, the preview goes, and so does a Narrow sheet the page
    /// pushed.
    pub fn close(&self) {
        let inner = &self.inner;
        if !inner.open.replace(false) {
            return;
        }
        inner.runs.borrow_mut().close();
        if let Some(timer) = inner.timer.borrow_mut().take() {
            timer.remove();
        }
        inner.debounce_busy.borrow_mut().take();
        *inner.wanted.borrow_mut() = None;
        let pushed = inner.pushed_sheet.replace(false);
        inner.ctx.store.update_local(move |l| {
            l.panel_page = PanelPageKind::Layers;
            l.autocut_preview = None;
            if pushed && l.layout.width == LayoutWidth::Narrow {
                l.close_overlays(|o| *o == Overlay::LayersSheet);
            }
        });
        if let Some(window) = inner.root.root().and_downcast::<gtk::Window>()
            && window
                .default_widget()
                .is_some_and(|w| w == *inner.apply.upcast_ref::<gtk::Widget>())
        {
            window.set_default_widget(None::<&gtk::Widget>);
        }
    }

    /// Apply (D-15): the changed settings, then the cut; the page closes.
    pub fn apply(&self) {
        let inner = &self.inner;
        if !inner.open.get() {
            return;
        }
        let store = inner.ctx.store.clone();
        let state = store.state();
        if let Err(why) = autocut::page_enabled(&state) {
            store.notify(Notice::new(ToastLevel::Info, why));
            return;
        }
        let draft = inner.draft.borrow().clone();
        for cmd in autocut::apply(&state, &draft) {
            if matches!(cmd, Command::Autocut { .. }) {
                let notify = store.clone();
                let sent = cmd.clone();
                store.dispatch_with(cmd, false, move |reply| {
                    cut_done(&notify, &sent, reply);
                });
            } else {
                store.dispatch(cmd);
            }
        }
        self.close();
    }

    /// A slider or a field moved knob `i` to `value`.
    fn tuned(&self, i: usize, value: f64, from_slider: bool) {
        let inner = &self.inner;
        if inner.setting.get() || !inner.open.get() {
            return;
        }
        let knob = inner.knobs[i].knob;
        let draft = knob.set(&inner.draft.borrow(), value);
        if from_slider {
            inner.knobs[i].field.set_value(&knob.text(&draft));
        }
        self.set_draft(draft);
    }

    fn set_draft(&self, draft: AutocutDraft) {
        let inner = &self.inner;
        if *inner.draft.borrow() == draft {
            self.show_draft(&inner.ctx.store.state());
            return;
        }
        *inner.draft.borrow_mut() = draft;
        let state = inner.ctx.store.state();
        self.show_draft(&state);
        self.request(&state);
    }

    /// The dry run the page wants changed (the draft, the scope, or what
    /// its answer is worked out on): debounce it. When the timeline changed
    /// under the shown plan its frames are wrong: the hatches, the summary
    /// and the regions go until the new answer comes (a reply to an older
    /// dry run is never shown once a change waits, `DryRuns::replied`).
    fn request(&self, state: &State) {
        let inner = &self.inner;
        let key = autocut::dry_run_key(state, &inner.draft.borrow());
        let stale = match inner.wanted.borrow().as_ref() {
            Some(wanted) if *wanted == key => return,
            Some(wanted) => wanted.stale_for(&key),
            None => false,
        };
        let cmd = key.cmd.clone();
        *inner.wanted.borrow_mut() = Some(key);
        if stale {
            inner.spans.borrow_mut().clear();
            *inner.summary.borrow_mut() = None;
            inner.selected_region.set(None);
        }
        let step = inner.runs.borrow_mut().change(cmd, self.now_ms());
        self.carry_out(step);
        self.publish();
    }

    fn carry_out(&self, step: Step) {
        let inner = &self.inner;
        match step {
            Step::Idle => {}
            Step::WaitUntil { at_ms } => {
                if let Some(timer) = inner.timer.borrow_mut().take() {
                    timer.remove();
                }
                inner
                    .debounce_busy
                    .borrow_mut()
                    .get_or_insert_with(|| inner.ctx.media.hold_io_busy());
                let wait = Duration::from_millis(at_ms.saturating_sub(self.now_ms()));
                let weak = Rc::downgrade(inner);
                let id = guard::timeout_local_once(wait, "autocut: debounce", move || {
                    if let Some(inner) = weak.upgrade() {
                        inner.timer.borrow_mut().take();
                        // Held until the step is carried out: a dry run sent
                        // now is a pending request before the token goes.
                        let busy = inner.debounce_busy.borrow_mut().take();
                        let page = AutocutPage { inner };
                        let step = page.inner.runs.borrow_mut().poll(page.now_ms());
                        page.carry_out(step);
                        page.publish();
                        drop(busy);
                    }
                });
                *inner.timer.borrow_mut() = Some(id);
            }
            Step::Send { id, cmd } => {
                let weak = Rc::downgrade(inner);
                inner.ctx.store.dispatch_with(cmd, false, move |reply| {
                    if let Some(inner) = weak.upgrade() {
                        AutocutPage { inner }.replied(id, reply);
                    }
                });
            }
        }
    }

    /// Dry run `id` answered.
    fn replied(&self, id: u64, reply: Reply) {
        let inner = &self.inner;
        let (show, step) = inner.runs.borrow_mut().replied(id, self.now_ms());
        if show {
            let state = inner.ctx.store.state();
            let draft = inner.draft.borrow().clone();
            let (summary, spans) = match &reply.result {
                Ok(value) => {
                    let result: Option<CmdResult> = serde_json::from_value(value.clone()).ok();
                    let plan: Option<AutocutPlanView> = result
                        .as_ref()
                        .and_then(|r| serde_json::from_value(r.result.clone()).ok());
                    match plan {
                        Some(plan) => (
                            autocut::summary_with(
                                &plan,
                                &state,
                                result.as_ref().map(|r| r.message.as_str()),
                            ),
                            autocut::preview_spans_for(&plan, &state, draft.track.as_ref()),
                        ),
                        None => (
                            nothing(&format::capitalized(
                                &result.map(|r| r.message).unwrap_or_default(),
                            )),
                            Vec::new(),
                        ),
                    }
                }
                Err(e) => (nothing(&format::capitalized(&e.message)), Vec::new()),
            };
            *inner.summary.borrow_mut() = Some(summary);
            *inner.spans.borrow_mut() = spans;
            inner.selected_region.set(None);
        }
        self.carry_out(step);
        self.publish();
        self.show_summary();
    }

    /// Hands the preview to the timeline (`LocalUi.autocut_preview`).
    fn publish(&self) {
        let inner = &self.inner;
        if !inner.open.get() {
            return;
        }
        let preview = Arc::new(AutocutPreview {
            spans: inner.spans.borrow().clone(),
            analyzing: inner.runs.borrow().analyzing(),
        });
        inner.ctx.store.update_local(move |l| {
            if l.autocut_preview.as_deref() != Some(&*preview) {
                l.autocut_preview = Some(preview);
            }
        });
        self.show_summary();
    }

    fn region_clicked(&self, row: &gtk::ListBoxRow) {
        let inner = &self.inner;
        let hit = inner
            .region_rows
            .borrow()
            .iter()
            .enumerate()
            .find(|(_, (r, _))| r == row)
            .map(|(i, (_, region))| (i, region.seek));
        let Some((i, frame)) = hit else { return };
        inner.selected_region.set(Some(i));
        inner.ctx.store.dispatch(Command::Seek {
            to: SeekTarget::To(TimeArg::frames(frame)),
        });
        self.mark_region();
    }

    fn mark_region(&self) {
        let inner = &self.inner;
        let selected = inner.selected_region.get();
        for (i, (row, _)) in inner.region_rows.borrow().iter().enumerate() {
            if selected == Some(i) {
                row.add_css_class("selected");
            } else {
                row.remove_css_class("selected");
            }
        }
    }

    /// Follows a new snapshot while open: the scope (the selection), the
    /// clips (a trim, a move, an undo, another client's edit) and the
    /// setting's track change the dry run (`view::autocut::dry_run_key`);
    /// a recording or an empty timeline closes the page.
    pub fn update(&self, state: &State) {
        let inner = &self.inner;
        if !inner.open.get() {
            return;
        }
        if autocut::page_enabled(state).is_err() {
            self.close();
            return;
        }
        inner.subtitle.set_text(&autocut::subtitle(state));
        inner.setting.set(true);
        inner
            .switch
            .set_active(state.project.autocut.auto_run_after_take);
        inner.setting.set(false);
        self.show_tracks(state);
        let changed = inner
            .wanted
            .borrow()
            .as_ref()
            .is_some_and(|w| *w != autocut::dry_run_key(state, &inner.draft.borrow()));
        if changed {
            self.request(state);
        }
    }

    fn show_tracks(&self, state: &State) {
        let inner = &self.inner;
        let items = autocut::track_items(state);
        let ids: Vec<TrackId> = items.iter().map(|i| i.track.clone()).collect();
        inner.setting.set(true);
        if *inner.tracks.borrow() != ids {
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            inner
                .dropdown
                .set_model(Some(&gtk::StringList::new(&labels)));
            *inner.tracks.borrow_mut() = ids.clone();
        }
        let shown = autocut::shown_track(state, &inner.draft.borrow());
        let index = shown
            .and_then(|t| ids.iter().position(|i| *i == t))
            .map_or(gtk::INVALID_LIST_POSITION, |i| i as u32);
        if inner.dropdown.selected() != index {
            inner.dropdown.set_selected(index);
        }
        inner.setting.set(false);
    }

    fn show_draft(&self, state: &State) {
        let inner = &self.inner;
        let draft = inner.draft.borrow().clone();
        inner.setting.set(true);
        for knob in &inner.knobs {
            let value = knob.knob.get(&draft);
            if (knob.scale.value() - value).abs() > f64::EPSILON {
                knob.scale.set_value(value);
            }
            knob.field.set_value(&knob.knob.text(&draft));
        }
        inner.setting.set(false);
        self.show_tracks(state);
        inner.subtitle.set_text(&autocut::subtitle(state));
        inner.setting.set(true);
        inner
            .switch
            .set_active(state.project.autocut.auto_run_after_take);
        inner.setting.set(false);
    }

    fn show_summary(&self) {
        let inner = &self.inner;
        let analyzing = inner.runs.borrow().analyzing();
        inner.spinner.set_visible(analyzing);
        let summary = inner.summary.borrow().clone();
        match (&summary, analyzing) {
            (_, true) => {
                inner.summary_title.set_text(autocut::ANALYZING);
                inner.summary_mono.set_text("");
            }
            (Some(s), false) => {
                // The parenthesized duration is a number: mono (G§3.14).
                match s.title.split_once(" (") {
                    Some((words, number)) => {
                        inner.summary_title.set_text(words);
                        inner.summary_mono.set_text(&format!("({number}"));
                    }
                    None => {
                        inner.summary_title.set_text(&s.title);
                        inner.summary_mono.set_text("");
                    }
                }
            }
            (None, false) => {
                inner.summary_title.set_text("");
                inner.summary_mono.set_text("");
            }
        }
        // While a new dry run comes, a summary still right for the timeline
        // keeps its rows and regions (another knob, another scope); none
        // (just opened, or gone stale) shows none.
        if analyzing && summary.is_some() {
            return;
        }
        // The rows and the regions of the shown summary.
        while let Some(child) = inner.summary_rows.first_child() {
            inner.summary_rows.remove(&child);
        }
        if let Some(s) = &summary {
            for (i, (name, value)) in s.rows.iter().enumerate() {
                if name.is_empty() {
                    let line = label(value, &["t-caption", "summary-reason"]);
                    line.set_wrap(true);
                    inner.summary_rows.attach(&line, 0, i as i32, 2, 1);
                    continue;
                }
                let key = label(name, &["t-caption", "muted", "summary-key"]);
                inner.summary_rows.attach(&key, 0, i as i32, 1, 1);
                inner
                    .summary_rows
                    .attach(&summary_value(value), 1, i as i32, 1, 1);
            }
        }
        let regions = summary.map(|s| s.regions).unwrap_or_default();
        let same = {
            let rows = inner.region_rows.borrow();
            rows.len() == regions.len() && rows.iter().zip(&regions).all(|((_, a), b)| a == b)
        };
        if !same {
            for (row, _) in inner.region_rows.borrow_mut().drain(..) {
                inner.regions.remove(&row);
            }
            let mut rows = Vec::new();
            for region in regions {
                let row = region_row(&region);
                inner.regions.append(&row);
                rows.push((row, region));
            }
            *inner.region_rows.borrow_mut() = rows;
        }
        inner
            .regions
            .set_visible(!inner.region_rows.borrow().is_empty());
        self.mark_region();
    }
}

/// "Nothing to cut" with one reason row.
fn nothing(why: &str) -> AutocutSummary {
    AutocutSummary {
        title: autocut::NOTHING_TO_CUT.to_owned(),
        rows: vec![(String::new(), why.to_owned())],
        regions: Vec::new(),
    }
}

/// A summary row's value: "before → after" reads the before and the arrow
/// in `muted`, the after in `text`, stronger (S12); anything else is one
/// line.
fn summary_value(value: &str) -> gtk::Widget {
    let Some((before, after)) = value.split_once(" → ") else {
        let line = label(value, &["t-mono-sm", "summary-value"]);
        line.set_wrap(true);
        return line.upcast();
    };
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.append(&label(before, &["t-mono-sm", "muted", "summary-before"]));
    row.append(&label(" → ", &["t-mono-sm", "muted"]));
    row.append(&label(
        after,
        &["t-mono-sm", "summary-value", "summary-after"],
    ));
    row.upcast()
}

/// A region row (32 px, `mono-sm`): its number, its range, its size.
fn region_row(region: &RegionRow) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("region-row");
    row.add_css_class("r1");
    row.add_css_class("row-32");
    row.set_widget_name(&format!("autocut-region-{}", region.n));
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    // The number and the size read `muted` (R1 turns them `text` on the
    // marked row).
    let n = label(&region.n.to_string(), &["t-mono-sm", "muted", "region-n"]);
    let range = label(&region.range, &["t-mono-sm", "region-range"]);
    range.set_hexpand(true);
    // Never wider than the panel (the Medium layout's 320 px).
    range.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let size = label(&region.size, &["t-mono-sm", "muted", "region-size"]);
    size.set_xalign(1.0);
    size.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    content.append(&n);
    content.append(&range);
    content.append(&size);
    row.set_child(Some(&content));
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "Silence {}: {}, {}",
        region.n, region.range, region.size
    ))]);
    row
}

/// A footer button with its key cap ("Cancel" `Esc`, "Apply autocut"
/// `Enter`, S12). The content box adds no spacing of its own: common.css's
/// `border-spacing` puts the 8 px between the label and the cap.
fn labeled_with_caps(text: &str, caps: &[String], kind: Kind) -> gtk::Button {
    use crate::widgets::keycap::{self, CapSize, CapVariant};
    let button = gtk::Button::new();
    button.add_css_class("qrec-btn");
    button.add_css_class(kind.class());
    button.set_focus_on_click(false);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&gtk::Label::new(Some(text)));
    let variant = if kind == Kind::Primary {
        CapVariant::OnFill
    } else {
        CapVariant::Plain
    };
    // The caps keep their own height, centered (S12), not the button's.
    let chord = keycap::chord(caps, CapSize::Sm, variant);
    chord.set_valign(gtk::Align::Center);
    content.append(&chord);
    button.set_child(Some(&content));
    button.update_property(&[gtk::accessible::Property::Label(&button::accessible_name(
        text,
        Some(caps),
    ))]);
    button
}

/// The cut's answer (the page closed already): its success toasts with
/// Undo (S25 #6); an immediate failure toasts the core's error; a late one
/// was toasted by the core (H-7).
fn cut_done(store: &crate::store::Store, cmd: &Command, reply: Reply) {
    match reply.result {
        Ok(value) => {
            let Some(result) = serde_json::from_value::<CmdResult>(value).ok() else {
                return;
            };
            let level = if result.seq.is_some() {
                ToastLevel::Success
            } else {
                ToastLevel::Info
            };
            let mut notice = Notice::new(level, format::capitalized(&result.message));
            if result.seq.is_some() {
                notice = notice.with_action(NoticeAction::Core(ToastAction {
                    label: "Undo".to_owned(),
                    action: UiAction::Dispatch(Box::new(Command::Undo { steps: 1 })),
                }));
            }
            store.notify(notice);
        }
        Err(e) if !reply.late => {
            if let Some(notice) = error_notice(&e, cmd, &store.state()) {
                store.notify(notice);
            }
        }
        Err(e) => tracing::info!("autocut failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::local::LayoutClass;

    fn local(width: LayoutWidth, page: PanelPageKind, sheet: bool) -> LocalUi {
        let mut local = LocalUi {
            layout: LayoutClass {
                width,
                compact: false,
            },
            panel_page: page,
            ..LocalUi::default()
        };
        if sheet {
            local.overlays.push(Overlay::LayersSheet);
        }
        local
    }

    /// R39: the open page always shows. In Narrow it is in the sheet: the
    /// sheet going closes it, a window narrowing under it shows the sheet;
    /// docked (Wide, Medium), or closed, nothing happens.
    #[test]
    fn the_page_goes_with_the_narrow_sheet() {
        use LayoutWidth::{Medium, Narrow, Wide};
        use PanelPageKind::{Autocut, Layers};
        // Narrow, the sheet showing the page: it stays.
        assert_eq!(
            sheet_step(true, true, &local(Narrow, Autocut, true)),
            SheetStep::Stay
        );
        // The sheet went (Esc, a click beside it, the layers button).
        assert_eq!(
            sheet_step(true, true, &local(Narrow, Autocut, false)),
            SheetStep::Close
        );
        // The window narrowed under the open page.
        assert_eq!(
            sheet_step(true, false, &local(Narrow, Autocut, false)),
            SheetStep::ShowSheet
        );
        // Docked, the page shows; a wider window drops the sheet itself.
        for width in [Wide, Medium] {
            for was_narrow in [false, true] {
                assert_eq!(
                    sheet_step(true, was_narrow, &local(width, Autocut, false)),
                    SheetStep::Stay
                );
            }
        }
        // Closed, or still opening (its page not written yet): nothing.
        assert_eq!(
            sheet_step(false, true, &local(Narrow, Layers, false)),
            SheetStep::Stay
        );
        assert_eq!(
            sheet_step(true, true, &local(Narrow, Layers, false)),
            SheetStep::Stay
        );
    }
}
