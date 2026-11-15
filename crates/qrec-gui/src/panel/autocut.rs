//! The autocut page (S12, D-15): it replaces the panel's content, a pushed
//! page with a back button. "Detect silence on", four sliders with value
//! fields, the "Trim edges only" check box (issue #15: part of the draft),
//! the summary of the newest dry run, the region list (a click seeks),
//! "Run autocut after every take", the note, and Cancel / Apply autocut.
//!
//! - Tuning is a draft (`view::autocut::AutocutDraft`): no history. Each
//!   change sends a dry run 250 ms later, one at a time
//!   ([`super::dry_run::DryRuns`]), and `LocalUi.autocut_preview` carries
//!   the hatches the timeline draws.
//! - A dry run that takes longer than 300 ms shows the analysis cue (issue
//!   #15, board 07): the summary title spins, "Analyzing take 8" with how
//!   far ffmpeg has read ("40%", and a 4 px bar under it); the old plan's
//!   hatches leave the timeline, where the analyzed clips' take plates
//!   spin, and its numbers and regions dim (and take no clicks) until the
//!   new plan shows. Re-plans and cached analyses answer sooner: no cue.
//! - Apply sends `AutocutSettings{patch}` with the changed fields (if any),
//!   then `Autocut{dry_run: false}`, and closes the page; the cut's answer
//!   toasts "Autocut take 8: removed 4 silences (6.2 s)" with Undo (S25).
//! - A previewed cut can be kept (issue #15): each region row's box
//!   (checked = cut) and a click on its hatch on the timeline toggle it in
//!   `LocalUi.autocut_keep`, which the draft follows (`keep`): a new dry
//!   run goes after the same debounce; Apply's cut keeps them. The page
//!   empties it when it opens and closes.
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

use gtk::prelude::*;
use qrec_core::protocol::ToastLevel;
use qrec_core::protocol::command::SeekTarget;
use qrec_core::protocol::reply::AutocutPlanView;
use qrec_core::view::autocut::{
    self, AUTO_RUN, AutocutDraft, AutocutSummary, DETECT_ON, DryRunKey, EDGES_ONLY,
    EDGES_ONLY_CAPTION, Knob, NOTE, RegionRow, TITLE,
};
use qrec_core::view::notices::Notice;
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

/// One row of the region list, and what it shows.
struct RegionView {
    row: gtk::ListBoxRow,
    /// Checked = cut; unchecked = kept.
    check: gtk::CheckButton,
    size: gtk::Label,
    region: RegionRow,
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
    /// "Detect silence on": a dropdown-like button whose menu checks the
    /// tracks (issue #15, board 03).
    dropdown: gtk::MenuButton,
    value: gtk::Label,
    /// The menu's rows, one per track item, rebuilt when the items change.
    track_list: gtk::Box,
    track_checks: RefCell<Vec<(TrackId, gtk::CheckButton)>>,
    tracks: RefCell<Vec<TrackId>>,
    knobs: Vec<KnobView>,
    /// "Trim edges only" (the draft's `edges_only`).
    edges: gtk::CheckButton,
    spinner: Spinner,
    summary_title: gtk::Label,
    summary_mono: gtk::Label,
    /// "40%" after the title while the cue shows (issue #15, board 07).
    summary_percent: gtk::Label,
    /// The analysis's 4 px bar under the title, while its length is known.
    progress: gtk::ProgressBar,
    summary_rows: gtk::Grid,
    regions: gtk::ListBox,
    region_rows: RefCell<Vec<RegionView>>,
    /// " · 1 kept" after the summary's title (issue #15, board 01).
    summary_kept: gtk::Label,
    selected_region: Cell<Option<usize>>,
    switch: gtk::Switch,
    apply: gtk::Button,
    runs: RefCell<DryRuns>,
    /// The dry run's debounce (a slot, R65).
    timer: guard::Slot,
    /// When the analysis cue is due (issue #15).
    cue_timer: guard::Slot,
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

/// "Detect silence on" (issue #15, board 03): a field-like button showing
/// the checked tracks, whose menu has a check box per track of the scope,
/// "Detect silence on" above and what checking several means below. The
/// menu is an overlay: Esc closes it, not the page (R32). Returns the
/// button, its value label and the menu's list of rows.
fn track_picker(ctx: &Ctx) -> (gtk::MenuButton, gtk::Label, gtk::Box) {
    let value = label("", &["autocut-track-value"]);
    value.set_hexpand(true);
    value.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let chevron = gtk::Image::from_icon_name("qrec-chevron-down-symbolic");
    chevron.add_css_class("muted");
    let face = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    face.append(&value);
    face.append(&chevron);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("autocut-track-list");
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu.add_css_class("autocut-track-menu");
    menu.append(&label(DETECT_ON, &["menu-label"]));
    menu.append(&list);
    menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let caption = label(
        autocut::DETECT_ON_CAPTION,
        &["t-caption", "muted", "menu-caption"],
    );
    caption.set_wrap(true);
    caption.set_max_width_chars(40);
    menu.append(&caption);
    let popover = gtk::Popover::new();
    popover.add_css_class("menu");
    popover.add_css_class("autocut-track-popover");
    popover.set_has_arrow(false);
    popover.set_child(Some(&menu));
    let store = ctx.store.clone();
    popover.connect_map(guarded!("autocut: the track menu opens", move |_| {
        store.update_local(|l| l.overlays.push(Overlay::Menu));
    }));
    let store = ctx.store.clone();
    popover.connect_unmap(guarded!("autocut: the track menu closes", move |_| {
        store.update_local(|l| {
            if let Some(at) = l.overlays.iter().rposition(|o| *o == Overlay::Menu) {
                l.overlays.remove(at);
            }
        });
    }));
    let button = crate::widgets::button::popover_button(&popover, &face, &[], DETECT_ON);
    button.add_css_class("autocut-track");
    button.set_widget_name("autocut-track");
    button.set_hexpand(true);
    (button, value, list)
}

/// One row of the track menu: a check box with the role's icon, the
/// track's name and its id chip ("🎤 Yeti Nano A1").
fn track_row(item: &autocut::TrackItem) -> gtk::CheckButton {
    let icon = gtk::Image::from_icon_name(match item.role {
        qrec_core::model::AudioRole::Mic => "qrec-mic-symbolic",
        qrec_core::model::AudioRole::System => "qrec-volume-2-symbolic",
    });
    let name = label(&item.title, &["track-name"]);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let chip = label(item.track.as_str(), &["id-chip", "t-mono-xs"]);
    chip.set_valign(gtk::Align::Center);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&icon);
    row.append(&name);
    row.append(&chip);
    let check = gtk::CheckButton::new();
    check.set_child(Some(&row));
    check.add_css_class("autocut-track-row");
    check.set_widget_name(&format!("autocut-track-{}", item.track));
    check.update_property(&[gtk::accessible::Property::Label(&item.label)]);
    check
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
        let back = button::icon(
            "qrec-chevron-left-symbolic",
            "Back",
            Some("cancel"),
            &keymap,
        );
        back.remove_css_class("icon-32");
        back.add_css_class("icon-24");
        back.set_widget_name("autocut-back");
        back.set_valign(gtk::Align::Center);
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_valign(gtk::Align::Center);
        titles.set_hexpand(true);
        let title = label(TITLE, &["t-title"]);
        title.set_widget_name("autocut-title");
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
        let (dropdown, value, track_list) = track_picker(ctx);
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
            scale.update_property(&[
                gtk::accessible::Property::Label(knob.label()),
                gtk::accessible::Property::Description(knob.help()),
            ]);
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
                .text_widget()
                .update_property(&[gtk::accessible::Property::Description(knob.help())]);
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
            // What the knob does, over its caption, slider and field; not
            // while the slider moves (its value tooltip shows then).
            block.set_has_tooltip(true);
            let moving = tooltip.clone();
            block.connect_query_tooltip(guarded!(
                "autocut: a knob's tooltip",
                move |_, _, _, _, tip| {
                    if moving.changing() {
                        return false;
                    }
                    tip.set_text(Some(knob.help()));
                    true
                }
            ));
            knobs.push(KnobView {
                knob,
                scale,
                field,
                _tooltip: tooltip,
            });
        }
        // "Trim edges only" under the sliders, its caption under its label
        // (issue #15, board 02).
        let edges_block = gtk::Box::new(gtk::Orientation::Vertical, 2);
        edges_block.add_css_class("autocut-block");
        edges_block.add_css_class("autocut-edges");
        let edges = gtk::CheckButton::with_label(EDGES_ONLY);
        edges.set_widget_name("autocut-edges-only");
        edges.set_focus_on_click(false);
        edges.update_property(&[gtk::accessible::Property::Description(EDGES_ONLY_CAPTION)]);
        let edges_caption = label(EDGES_ONLY_CAPTION, &["t-caption", "muted", "edges-caption"]);
        edges_caption.set_wrap(true);
        edges_block.append(&edges);
        edges_block.append(&edges_caption);
        content.append(&edges_block);
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
        let summary_kept = label("", &["summary-kept", "muted"]);
        summary_kept.set_valign(gtk::Align::Baseline);
        summary_kept.set_widget_name("autocut-summary-kept");
        let summary_percent = label("", &["summary-percent", "muted"]);
        summary_percent.set_valign(gtk::Align::Baseline);
        summary_percent.set_widget_name("autocut-summary-percent");
        summary_percent.set_visible(false);
        title_row.append(&spinner);
        title_row.append(&summary_title);
        title_row.append(&summary_mono);
        title_row.append(&summary_percent);
        title_row.append(&summary_kept);
        // The analysis's bar (board 07): `accent` on the `border` track.
        let progress = gtk::ProgressBar::new();
        progress.add_css_class("autocut-progress");
        progress.set_widget_name("autocut-progress");
        progress.set_visible(false);
        let summary_rows = gtk::Grid::new();
        summary_rows.add_css_class("summary-rows");
        summary_rows.set_column_spacing(12);
        summary_rows.set_row_spacing(4);
        summary.append(&title_row);
        summary.append(&progress);
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
        // `cancel`'s key, also after `keys.toml` changes (WP10).
        button::follow_keymap(&cancel, &keymap, |cancel, keymap| {
            use crate::widgets::keycap::{CapSize, CapVariant};
            let esc =
                button::binding_caps(keymap, "cancel").unwrap_or_else(|| vec!["Esc".to_owned()]);
            if let Some(content) = cancel.child().and_downcast::<gtk::Box>() {
                button::show_binding_cap(
                    &content,
                    Some(&esc),
                    button::BINDING_CAP,
                    CapSize::Md,
                    CapVariant::Plain,
                );
            }
            cancel.update_property(&[gtk::accessible::Property::Label(&button::accessible_name(
                "Cancel",
                Some(&esc),
            ))]);
        });
        let apply = labeled_with_caps(autocut::APPLY, &["Enter".to_owned()], Kind::Primary);
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
                value,
                track_list,
                track_checks: RefCell::new(Vec::new()),
                tracks: RefCell::new(Vec::new()),
                knobs,
                edges,
                spinner,
                summary_title,
                summary_mono,
                summary_percent,
                progress,
                summary_rows,
                regions,
                region_rows: RefCell::new(Vec::new()),
                summary_kept,
                selected_region: Cell::new(None),
                switch,
                apply,
                runs: RefCell::new(DryRuns::new()),
                timer: guard::Slot::new(),
                cue_timer: guard::Slot::new(),
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
            .edges
            .connect_toggled(guarded!("autocut: edges only", move |check| {
                let Some(inner) = weak.upgrade() else { return };
                if inner.setting.get() || !inner.open.get() {
                    return;
                }
                let page = AutocutPage { inner };
                let mut draft = page.inner.draft.borrow().clone();
                draft.edges_only = check.is_active();
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
                    let page = AutocutPage { inner };
                    page.follow_keep(local);
                    page.follow_sheet(local);
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

    /// `LocalUi.autocut_keep` changed (a hatch clicked, a region's box): the
    /// draft keeps those cuts, and the dry run follows.
    fn follow_keep(&self, local: &LocalUi) {
        let inner = &self.inner;
        if !inner.open.get() || inner.draft.borrow().keep == local.autocut_keep {
            return;
        }
        let mut draft = inner.draft.borrow().clone();
        draft.keep = local.autocut_keep.clone();
        self.set_draft(draft);
    }

    /// A region's box toggled: keep its cut, or cut it again.
    fn region_toggled(&self, i: usize) {
        let inner = &self.inner;
        if inner.setting.get() || !inner.open.get() {
            return;
        }
        let Some((clip, cut)) = inner
            .region_rows
            .borrow()
            .get(i)
            .map(|r| (r.region.clip.clone(), r.region.cut))
        else {
            return;
        };
        inner.ctx.store.update_local(move |l| {
            l.autocut_keep = autocut::toggle_keep(&l.autocut_keep, &clip, cut);
        });
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
    /// run at once; on an empty timeline none goes, and the summary says
    /// so (`autocut::previews`).
    pub fn open(&self) {
        let inner = &self.inner;
        let state = inner.ctx.store.state();
        inner.open.set(true);
        *inner.draft.borrow_mut() = AutocutDraft::from(&state.project);
        let previews = autocut::previews(&state);
        *inner.summary.borrow_mut() = (!previews).then(autocut::empty_summary);
        inner.spans.borrow_mut().clear();
        inner.selected_region.set(None);
        self.show_draft(&state);
        self.show_apply_label(&state);
        self.show_summary();
        let key = autocut::dry_run_key(&state, &inner.draft.borrow());
        let cmd = key.cmd.clone();
        *inner.wanted.borrow_mut() = Some(key);
        let step = if previews {
            inner.runs.borrow_mut().open(cmd, self.now_ms())
        } else {
            Step::Idle
        };
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
            l.autocut_keep.clear();
            if push && !l.overlays.contains(&Overlay::LayersSheet) {
                l.overlays.push(Overlay::LayersSheet);
            }
        });
        self.carry_out(step);
        // One still in flight from before (this page waits for it).
        self.arm_cue();
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
        inner.timer.stop();
        inner.cue_timer.stop();
        inner.debounce_busy.borrow_mut().take();
        *inner.wanted.borrow_mut() = None;
        let pushed = inner.pushed_sheet.replace(false);
        inner.ctx.store.update_local(move |l| {
            l.panel_page = PanelPageKind::Layers;
            l.autocut_preview = None;
            l.autocut_keep.clear();
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
        // The cut's toast (S25 #6) is the default reply policy's, as for
        // `c` and the toolbar's Autocut (`store::replies::autocut_done`).
        for cmd in autocut::apply(&state, &draft) {
            store.dispatch(cmd);
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
        if !autocut::previews(state) {
            // An empty timeline (emptied, or tuned while empty): nothing to
            // preview, no dry run; what was waiting goes.
            inner.runs.borrow_mut().close();
            inner.timer.stop();
            inner.debounce_busy.borrow_mut().take();
            inner.spans.borrow_mut().clear();
            *inner.summary.borrow_mut() = Some(autocut::empty_summary());
            inner.selected_region.set(None);
            self.publish();
            return;
        }
        if stale {
            inner.spans.borrow_mut().clear();
            *inner.summary.borrow_mut() = None;
            inner.selected_region.set(None);
        }
        // Clips came onto an empty timeline (an undo, another client): the
        // dry runs start again, the first at once.
        let reopen = !inner.runs.borrow().is_open();
        let step = if reopen {
            inner.runs.borrow_mut().open(cmd, self.now_ms())
        } else {
            inner.runs.borrow_mut().change(cmd, self.now_ms())
        };
        self.carry_out(step);
        self.publish();
    }

    fn carry_out(&self, step: Step) {
        let inner = &self.inner;
        match step {
            Step::Idle => {}
            Step::WaitUntil { at_ms } => {
                inner.timer.stop();
                inner
                    .debounce_busy
                    .borrow_mut()
                    .get_or_insert_with(|| inner.ctx.media.hold_io_busy());
                let wait = Duration::from_millis(at_ms.saturating_sub(self.now_ms()));
                let weak = Rc::downgrade(inner);
                inner
                    .timer
                    .timeout_once(wait, "autocut: debounce", move || {
                        if let Some(inner) = weak.upgrade() {
                            // Held until the step is carried out: a dry run
                            // sent now is a pending request before the token
                            // goes (and goes with a panic too).
                            let busy = inner.debounce_busy.borrow_mut().take();
                            let page = AutocutPage { inner };
                            let step = page.inner.runs.borrow_mut().poll(page.now_ms());
                            page.carry_out(step);
                            page.publish();
                            drop(busy);
                        }
                    });
            }
            Step::Send { id, cmd } => {
                let weak = Rc::downgrade(inner);
                inner.ctx.store.dispatch_with(cmd, false, move |reply| {
                    if let Some(inner) = weak.upgrade() {
                        AutocutPage { inner }.replied(id, reply);
                    }
                });
                self.arm_cue();
            }
        }
    }

    /// Arms the analysis cue's timer for the dry run in flight (issue #15):
    /// if it hasn't answered [`super::dry_run::CUE_MS`] after it went, the
    /// cue shows.
    fn arm_cue(&self) {
        let inner = &self.inner;
        let Some(at_ms) = inner.runs.borrow().cue_at() else {
            return;
        };
        let wait = Duration::from_millis(at_ms.saturating_sub(self.now_ms()));
        let weak = Rc::downgrade(inner);
        inner
            .cue_timer
            .timeout_once(wait, "autocut: the analysis cue", move || {
                if let Some(inner) = weak.upgrade() {
                    let page = AutocutPage { inner };
                    let now = page.now_ms();
                    if page.inner.runs.borrow_mut().check_cue(now) {
                        page.publish();
                    }
                }
            });
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
                            autocut::preview_spans_for(&plan, &state, draft.padding_ms),
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

    /// Hands the preview to the timeline (`LocalUi.autocut_preview`):
    /// while the analysis cue shows, no hatches, and the analyzed clips'
    /// take plates spin.
    fn publish(&self) {
        let inner = &self.inner;
        if !inner.open.get() {
            return;
        }
        let cue = inner.runs.borrow().cue();
        let analyzed = cue
            .then(|| autocut::analyzing(&inner.ctx.store.state()))
            .flatten();
        let preview = Arc::new(AutocutPreview {
            spans: if cue {
                Vec::new()
            } else {
                inner.spans.borrow().clone()
            },
            analyzing: inner.runs.borrow().analyzing(),
            spinning: analyzed.map(|a| a.clips).unwrap_or_default(),
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
            .find(|(_, r)| r.row == *row)
            .map(|(i, r)| (i, r.region.seek));
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
        for (i, r) in inner.region_rows.borrow().iter().enumerate() {
            if selected == Some(i) {
                r.row.add_css_class("selected");
            } else {
                r.row.remove_css_class("selected");
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
        self.show_apply_label(state);
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
        // How far the analysis read, and which clips spin.
        if inner.runs.borrow().cue() {
            self.publish();
        }
    }

    /// "Apply autocut", or "Save" on an empty timeline (the settings only),
    /// with its accessible name.
    fn show_apply_label(&self, state: &State) {
        let apply = &self.inner.apply;
        let text = autocut::apply_label(state);
        let Some(label) = apply
            .child()
            .and_then(|content| content.first_child())
            .and_downcast::<gtk::Label>()
        else {
            return;
        };
        if label.text() != text {
            label.set_text(text);
            apply.update_property(&[gtk::accessible::Property::Label(&button::accessible_name(
                text,
                Some(&["Enter".to_owned()]),
            ))]);
        }
    }

    fn show_tracks(&self, state: &State) {
        let inner = &self.inner;
        let items = autocut::track_items(state);
        let ids: Vec<TrackId> = items.iter().map(|i| i.track.clone()).collect();
        inner.setting.set(true);
        if *inner.tracks.borrow() != ids {
            while let Some(child) = inner.track_list.first_child() {
                inner.track_list.remove(&child);
            }
            let mut checks = Vec::with_capacity(items.len());
            for item in &items {
                let check = track_row(item);
                inner.track_list.append(&check);
                let weak = Rc::downgrade(inner);
                let track = item.track.clone();
                check.connect_toggled(guarded!("autocut: a track checked", move |_| {
                    let Some(inner) = weak.upgrade() else { return };
                    if inner.setting.get() {
                        return;
                    }
                    let page = AutocutPage { inner };
                    let state = page.inner.ctx.store.state();
                    let mut draft = page.inner.draft.borrow().clone();
                    match autocut::toggled_tracks(&state, &draft, &track) {
                        Some(tracks) => {
                            draft.tracks = tracks;
                            page.set_draft(draft);
                        }
                        // The last checked track stays checked.
                        None => page.show_tracks(&state),
                    }
                }));
                checks.push((item.track.clone(), check));
            }
            *inner.track_checks.borrow_mut() = checks;
            *inner.tracks.borrow_mut() = ids;
        }
        let draft = inner.draft.borrow().clone();
        let shown = autocut::shown_tracks(state, &draft);
        for (track, check) in inner.track_checks.borrow().iter() {
            let on = shown.contains(track);
            if check.is_active() != on {
                check.set_active(on);
            }
            // The last checked track can't be unchecked.
            check.set_sensitive(!(on && shown.len() == 1));
        }
        let text = autocut::shown_label(state, &draft);
        inner.value.set_text(&text);
        inner
            .dropdown
            .update_property(&[gtk::accessible::Property::Description(&text)]);
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
        if inner.edges.is_active() != draft.edges_only {
            inner.edges.set_active(draft.edges_only);
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
        let (cue, waiting) = {
            let runs = inner.runs.borrow();
            (runs.cue(), runs.analyzing())
        };
        // What the analysis says while the cue shows (issue #15, board 07).
        let analyzed = cue
            .then(|| autocut::analyzing(&inner.ctx.store.state()))
            .flatten();
        inner.spinner.set_visible(cue);
        let summary = inner.summary.borrow().clone();
        inner.summary_kept.set_text(match (&summary, cue) {
            (Some(s), false) => &s.kept,
            _ => "",
        });
        let percent = analyzed.as_ref().and_then(|a| a.percent.as_deref());
        inner.summary_percent.set_text(percent.unwrap_or_default());
        inner.summary_percent.set_visible(percent.is_some());
        let fraction = analyzed.as_ref().and_then(|a| a.fraction);
        inner.progress.set_fraction(fraction.unwrap_or(0.0));
        inner.progress.set_visible(fraction.is_some());
        // The percent sits 8 px after the title: no empty "(6.2 s)" between.
        inner.summary_mono.set_visible(!cue);
        match (&summary, cue) {
            (_, true) => {
                let title = analyzed
                    .as_ref()
                    .map_or(autocut::ANALYZING, |a| a.title.as_str());
                inner.summary_title.set_text(title);
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
        // The old plan while the cue shows: its numbers and regions dim, and
        // the regions take no clicks (board 07).
        for stale in [
            inner.summary_rows.upcast_ref::<gtk::Widget>(),
            inner.regions.upcast_ref(),
        ] {
            if cue {
                stale.add_css_class("stale");
            } else {
                stale.remove_css_class("stale");
            }
        }
        inner.regions.set_sensitive(!cue);
        // While a new dry run comes, a summary still right for the timeline
        // keeps its rows and regions (another knob, another scope); none
        // (just opened, or gone stale) shows none.
        if waiting && summary.is_some() {
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
        // The same cuts (only kept or not changed): the rows stay, so a box
        // keeps the keyboard focus it was toggled with.
        let same = {
            let rows = inner.region_rows.borrow();
            rows.len() == regions.len()
                && rows.iter().zip(&regions).all(|(a, b)| {
                    let a = &a.region;
                    (a.n, &a.range, a.seek, &a.clip, a.cut)
                        == (b.n, &b.range, b.seek, &b.clip, b.cut)
                })
        };
        if same {
            inner.setting.set(true);
            for (view, region) in inner.region_rows.borrow_mut().iter_mut().zip(regions) {
                show_kept(view, &region);
                view.region = region;
            }
            inner.setting.set(false);
        } else {
            for view in inner.region_rows.borrow_mut().drain(..) {
                inner.regions.remove(&view.row);
            }
            let mut rows = Vec::new();
            for (i, region) in regions.into_iter().enumerate() {
                let view = region_row(region);
                let weak = Rc::downgrade(inner);
                view.check
                    .connect_toggled(guarded!("autocut: keep a region", move |_| {
                        if let Some(inner) = weak.upgrade() {
                            AutocutPage { inner }.region_toggled(i);
                        }
                    }));
                inner.regions.append(&view.row);
                rows.push(view);
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
        kept: String::new(),
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

/// A region row (32 px, `mono-sm`): its box (checked = cut, issue #15),
/// its number, its range, its size ("kept" when kept, the row dimmed).
fn region_row(region: RegionRow) -> RegionView {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("region-row");
    row.add_css_class("r1");
    row.add_css_class("row-32");
    row.set_widget_name(&format!("autocut-region-{}", region.n));
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let check = gtk::CheckButton::new();
    check.add_css_class("region-check");
    check.set_widget_name(&format!("autocut-region-{}-cut", region.n));
    check.set_valign(gtk::Align::Center);
    check.set_focus_on_click(false);
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
    content.append(&check);
    content.append(&n);
    content.append(&range);
    content.append(&size);
    row.set_child(Some(&content));
    let mut view = RegionView {
        row,
        check,
        size,
        region: region.clone(),
    };
    show_kept(&mut view, &region);
    view
}

/// A row as cut (its box checked) or kept (unchecked, dimmed, "kept").
fn show_kept(view: &mut RegionView, region: &RegionRow) {
    view.check.set_active(!region.kept);
    view.size.set_text(&region.size);
    if region.kept {
        view.row.add_css_class("kept");
    } else {
        view.row.remove_css_class("kept");
    }
    let what = if region.kept { "kept" } else { "cut" };
    view.check
        .update_property(&[gtk::accessible::Property::Label(&format!(
            "Cut silence {}",
            region.n
        ))]);
    view.row
        .update_property(&[gtk::accessible::Property::Label(&format!(
            "Silence {}: {}, {}, {what}",
            region.n, region.range, region.size
        ))]);
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
    // The caps keep their own height, centered (S12), not the button's:
    // medium ones, as DESIGN gives buttons (S12's Esc is 20 px).
    let chord = keycap::chord(caps, CapSize::Md, variant);
    chord.set_valign(gtk::Align::Center);
    chord.set_widget_name(button::BINDING_CAP);
    content.append(&chord);
    button.set_child(Some(&content));
    button.update_property(&[gtk::accessible::Property::Label(&button::accessible_name(
        text,
        Some(caps),
    ))]);
    button
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
