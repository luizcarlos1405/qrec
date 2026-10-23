//! The window (G§3.15): its tree, its regions, the overlay layers, the
//! adaptive modes, the focus regions and the blocking page.
//!
//! ```text
//! adw::ApplicationWindow #qrec  (default 1600×1000 within 90 % of the
//!                                largest monitor, min 800×600)
//! └ gtk::Overlay  window_layer          ← CheatSheet (scrim-strong over everything)
//!   └ gtk::Overlay  content_layer       ← Modal, Settings, Palette (scrim to y 968), ExportPanel, ToastHost
//!     └ Box V
//!       ├ gtk::Stack: editor | page
//!       │ ├ adw::ToolbarView  editor
//!       │ │ ├ top: Header
//!       │ │ └ adw::OverlaySplitView, sidebar at the end: Panel (384; 320; overlay sheet)
//!       │ │   └ Box V  work
//!       │ │       ├ Box H  stage_row: BinDrawer (352) | Stage (hexpand)
//!       │ │       └ Timeline (312 px; 232 compact)
//!       │ └ adw::ToolbarView  page: minimal header + BlockingPage
//!       └ HintBar (32 px, not focusable; under the content layer's overlays
//!         since WP10's shot review: a toast's shadow falls over it)
//! ```
//!
//! `LocalUi.focus` follows the window's focus widget: the nearest ancestor
//! named `region-<name>` (or a named part that takes focus), `Field` in an
//! editable. `region.next` and `region.prev` move between the regions
//! (D-10).

pub mod adaptive;
pub mod announce;
pub mod layers;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use qrec_core::view::FocusRegion;

use crate::bin_drawer::BinDrawer;
use crate::blocking::BlockingPage;
use crate::cheatsheet::CheatSheet;
use crate::export::ExportPanel;
use crate::header::Header;
use crate::hintbar::HintBar;
use crate::keys::{is_editable, router};
use crate::modal::ModalHost;
use crate::palette::Palette;
use crate::panel::Panel;
use crate::settings::Settings;
use crate::stage::Stage;
use crate::store::local::{LayoutClass, LayoutWidth, LocalUi, Overlay, StartupPhase};
use crate::theme::motion;
use crate::timeline::Timeline;
use crate::toasts::ToastHost;
use crate::{Ctx, guard, guarded};
use layers::WindowLayers;

/// The window's default size, where a monitor holds it ([`first_size`]).
pub const DEFAULT_SIZE: (i32, i32) = (1600, 1000);
/// The smallest window (the breakpoints need it).
pub const MIN_SIZE: (i32, i32) = (800, 600);

/// The share of the monitor the first window may take in each dimension,
/// in percent: the rest is left to the desktop's panels and docks.
pub const MONITOR_SHARE: i64 = 90;

/// The size the first window asks for, its default size (GUI-16):
/// [`DEFAULT_SIZE`], but in each dimension at most [`MONITOR_SHARE`] % of
/// the largest monitor's geometry (by area; logical px, as GDK gives it:
/// an X11 output's size over `GDK_SCALE`, a Wayland output's logical
/// size), so where the window manager lets the window pick its size (a
/// floating one: GNOME, KDE, Xfce) it opens as a window on a 1366×768
/// laptop, 1920×1080 at 150 % (1280×720) or a 1024×768 VM console (GTK
/// only bounds a default size to the monitor: 1600×1000 filled such a
/// screen edge to edge, a maximized window that isn't one); never below
/// [`MIN_SIZE`] (a smaller monitor gets MIN_SIZE). A tiling compositor
/// (sway, Hyprland) sizes the window itself. The largest, not the first:
/// with a laptop next to a bigger monitor the window keeps the size the
/// bigger one holds (where the compositor lets the window pick its width,
/// as niri's `default-column-width {}` does, it opens as wide as before).
/// Without a monitor of known size (none, or 0×0) it is DEFAULT_SIZE.
pub fn first_size(monitors: &[(i32, i32)]) -> (i32, i32) {
    let largest = monitors
        .iter()
        .copied()
        .filter(|&(w, h)| w > 0 && h > 0)
        .max_by_key(|&(w, h)| i64::from(w) * i64::from(h));
    let Some((width, height)) = largest else {
        return DEFAULT_SIZE;
    };
    let fit = |default: i32, min: i32, monitor: i32| {
        let share = i64::from(monitor) * MONITOR_SHARE / 100;
        // At most `default` (an i32), so the conversion holds.
        let limit = i32::try_from(share.min(i64::from(default))).unwrap_or(default);
        limit.max(min)
    };
    (
        fit(DEFAULT_SIZE.0, MIN_SIZE.0, width),
        fit(DEFAULT_SIZE.1, MIN_SIZE.1, height),
    )
}

/// Every monitor's geometry on `display` (logical px), in GDK's order.
fn monitor_sizes(display: &gtk::gdk::Display) -> Vec<(i32, i32)> {
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_downcast::<gtk::gdk::Monitor>())
        .map(|monitor| {
            let geometry = monitor.geometry();
            (geometry.width(), geometry.height())
        })
        .collect()
}

/// The named parts the screenshot checks measure besides `region-*`
/// (G§8.4); each owner names its widget.
pub const NAMED_PARTS: [&str; 9] = [
    "preview",
    "transport",
    "timeline-toolbar",
    "ruler",
    "track-headers",
    "time-area",
    "video-row",
    "lane-A1",
    "lane-A2",
];

/// The window and its layers, before and after its regions exist.
pub struct MainWindow {
    pub window: adw::ApplicationWindow,
    pub layers: WindowLayers,
    content_layer: gtk::Overlay,
    parts: RefCell<Option<Rc<Parts>>>,
}

/// What `populate` built (kept for the life of the window).
struct Parts {
    stack: gtk::Stack,
    split: adw::OverlaySplitView,
    timeline: gtk::Widget,
    panel: gtk::Widget,
    page_slot: gtk::Box,
    page: RefCell<Option<BlockingPage>>,
    /// The Narrow sheet is open under reduced motion with GTK's animations
    /// turned off for it ([`still_sheet`]).
    stilled: Cell<bool>,
    _regions: Regions,
}

/// The regions, kept alive with the window.
struct Regions {
    _header: Header,
    _stage: Stage,
    _timeline: Timeline,
    _panel: Panel,
    _hintbar: HintBar,
    _bin: BinDrawer,
    _export: ExportPanel,
    _toasts: ToastHost,
    _modal: ModalHost,
    _settings: Settings,
    _palette: Palette,
    _cheatsheet: CheatSheet,
}

impl MainWindow {
    /// The window and its two overlay layers, empty (G§3.3 step 2), asking
    /// for [`first_size`] for the display's monitors as its default size.
    pub fn new(app: &adw::Application, title: &str) -> Self {
        let monitors = gtk::gdk::Display::default()
            .map(|display| monitor_sizes(&display))
            .unwrap_or_default();
        let (width, height) = first_size(&monitors);
        if (width, height) != DEFAULT_SIZE {
            tracing::info!(
                "the window asks for {width}×{height} (its default size), to fit the monitors \
                 {monitors:?}"
            );
        }
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(title)
            .default_width(width)
            .default_height(height)
            .width_request(MIN_SIZE.0)
            .height_request(MIN_SIZE.1)
            .build();
        window.set_widget_name("qrec");
        window.add_css_class("qrec");
        let window_layer = gtk::Overlay::new();
        // The whole window: the editor (or a page) and the hint bar under
        // it (`populate`), the content layer's overlays over both.
        let content_layer = gtk::Overlay::new();
        content_layer.set_vexpand(true);
        window_layer.set_child(Some(&content_layer));
        window.set_content(Some(&window_layer));
        let layers = WindowLayers::new(&window_layer, &content_layer);
        Self {
            window,
            layers,
            content_layer,
            parts: RefCell::new(None),
        }
    }

    /// Builds every region and the layout; follows `LocalUi` (layout,
    /// startup page, overlays) and the focus. `_title`, the folder's name,
    /// is the window's title already ([`MainWindow::new`]): the blocking
    /// pages' minimal header shows none (WP8).
    pub fn populate(&self, ctx: &Ctx, _title: &str) {
        let header = Header::new(ctx);
        let stage = Stage::new(ctx);
        let timeline = Timeline::new(ctx);
        let panel = Panel::new(ctx);
        let hintbar = HintBar::new(ctx);
        let bin = BinDrawer::new(ctx);
        let export = ExportPanel::new(ctx);
        let toasts = ToastHost::new(ctx);
        let modal = ModalHost::new(ctx);
        let settings = Settings::new(ctx);
        let palette = Palette::new(ctx);
        let cheatsheet = CheatSheet::new(ctx);

        let stage_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        stage_row.set_vexpand(true);
        stage_row.append(&bin.widget());
        stage_row.append(&stage.widget());
        let work = gtk::Box::new(gtk::Orientation::Vertical, 0);
        work.set_hexpand(true);
        work.append(&stage_row);
        let timeline_widget = timeline.widget();
        timeline_widget.set_height_request(LayoutClass::default().timeline_height());
        work.append(&timeline_widget);

        let panel_widget = panel.widget();
        let split = adw::OverlaySplitView::new();
        split.set_sidebar_position(gtk::PackType::End);
        split.set_content(Some(&work));
        split.set_sidebar(Some(&panel_widget));
        split.set_enable_hide_gesture(false);
        split.set_enable_show_gesture(false);
        set_panel_width(&split, 384);

        let editor = adw::ToolbarView::new();
        editor.add_top_bar(&header.widget());
        editor.set_content(Some(&split));

        // The blocking pages' minimal header (C04 `Xba2H`, as WP8's frames
        // draw it): the app icon, the wordmark and close; no title (the
        // window's own title is still the folder's name).
        let page_header = adw::HeaderBar::new();
        page_header.set_widget_name("minimal-header");
        page_header.add_css_class("qrec-minimal-header");
        let start = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        start.add_css_class("header-start");
        start.set_valign(gtk::Align::Center);
        let icon = gtk::Image::new();
        icon.set_pixel_size(24);
        icon.add_css_class("app-icon");
        icon.set_valign(gtk::Align::Center);
        icon.update_property(&[gtk::accessible::Property::Label("Qrec")]);
        let show_icon = {
            let icon = icon.downgrade();
            move |theme: &crate::theme::Theme| {
                if let Some(icon) = icon.upgrade() {
                    match theme.art(crate::theme::art::Art::AppIcon24) {
                        Some(svg) => icon.set_paintable(Some(&svg)),
                        None => icon.set_icon_name(Some("qrec-record-symbolic")),
                    }
                }
            }
        };
        show_icon(&ctx.theme);
        ctx.theme.subscribe(show_icon).detach();
        start.append(&icon);
        let wordmark = gtk::Label::new(Some("Qrec"));
        wordmark.add_css_class("wordmark");
        wordmark.add_css_class("t-wordmark");
        // At x 44, as in the main header (12 + 24 + 8).
        wordmark.set_margin_start(8);
        start.append(&wordmark);
        page_header.pack_start(&start);
        page_header.set_title_widget(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
        page_header.set_show_end_title_buttons(false);
        page_header.set_show_start_title_buttons(false);
        let keymap = ctx.store.keymap();
        page_header.pack_end(&crate::widgets::button::close_window(&keymap));
        let page_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page_slot.set_vexpand(true);
        let page_view = adw::ToolbarView::new();
        page_view.add_top_bar(&page_header);
        page_view.set_content(Some(&page_slot));

        let stack = gtk::Stack::new();
        stack.add_named(&editor, Some("editor"));
        stack.add_named(&page_view, Some("page"));
        stack.set_visible_child_name("editor");
        stack.set_vexpand(true);
        let hint_widget = hintbar.widget();
        // The hint bar never takes the window's spare height.
        hint_widget.set_vexpand(false);
        // The hint bar is in the content layer, under its overlays: a
        // toast's shadow falls over it (the bar, drawn after the layer,
        // cut it at y 968; WP10's shot review), and the scrims stop above
        // it (`.scrim`'s bottom margin), so it stays live under a modal.
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&stack);
        content.append(&hint_widget);
        self.content_layer.set_child(Some(&content));
        // The toasts float in the content layer, passive, 12 px above the
        // hint bar (`#toast-host`'s bottom margin is the bar's height).
        let toast_widget = toasts.widget();
        self.content_layer.add_overlay(&toast_widget);
        self.content_layer.set_clip_overlay(&toast_widget, false);
        // A trap takes what is below it out of the focus chain: the toasts
        // too (Tab past a modal's last button reached a toast's button,
        // and Space ran its action under the scrim).
        self.layers.set_base(
            vec![stack.clone().upcast(), toast_widget.clone()],
            vec![stack.clone().upcast(), hint_widget, toast_widget],
        );

        let parts = Rc::new(Parts {
            stack,
            split,
            timeline: timeline_widget,
            panel: panel_widget,
            page_slot,
            page: RefCell::new(None),
            stilled: Cell::new(false),
            _regions: Regions {
                _header: header,
                _stage: stage,
                _timeline: timeline,
                _panel: panel,
                _hintbar: hintbar,
                _bin: bin,
                _export: export,
                _toasts: toasts,
                _modal: modal,
                _settings: settings,
                _palette: palette,
                _cheatsheet: cheatsheet,
            },
        });
        *self.parts.borrow_mut() = Some(Rc::clone(&parts));

        adaptive::install(&self.window, &ctx.store);
        self.follow_local(ctx, &parts);
        follow_sheet_motion(ctx, &parts);
        self.follow_focus(ctx);
        self.register_actions(ctx);
        apply_local(ctx, &parts, &ctx.store.local());
    }

    /// The sheet's own close (a click outside, in Narrow) leaves the
    /// overlays in step; `LocalUi` drives the rest.
    fn follow_local(&self, ctx: &Ctx, parts: &Rc<Parts>) {
        let (ctx2, parts2) = (ctx.clone(), Rc::clone(parts));
        ctx.store
            .subscribe_local(move |local| apply_local(&ctx2, &parts2, local))
            .detach();
        let store = ctx.store.clone();
        let stilled = Rc::downgrade(parts);
        parts
            .split
            .connect_show_sidebar_notify(guarded!("window: the sheet", move |split| {
                if split.is_collapsed() && !split.shows_sidebar() {
                    // Gone (its own close too): GTK's animations back on.
                    if let Some(parts) = stilled.upgrade() {
                        unstill_sheet(&parts);
                    }
                    store.update_local(|l| {
                        l.close_overlays(|o| *o == Overlay::LayersSheet);
                    });
                }
            }));
    }

    /// `LocalUi.focus` follows the focus widget.
    fn follow_focus(&self, ctx: &Ctx) {
        let store = ctx.store.clone();
        self.window
            .connect_focus_widget_notify(guarded!("window: focus", move |window| {
                let focus = GtkWindowExt::focus(window);
                if let Some(w) = &focus {
                    tracing::debug!("focus: {} {:?}", w.type_().name(), w.widget_name());
                }
                let region = focus.map_or(FocusRegion::Other, |w| focus_region(&w));
                if store.local().focus != region {
                    store.update_local(move |l| l.focus = region);
                }
            }));
    }

    /// `region.next` and `region.prev` (D-10).
    fn register_actions(&self, ctx: &Ctx) {
        for forward in [true, false] {
            let id = if forward {
                "region.next"
            } else {
                "region.prev"
            };
            let window = self.window.downgrade();
            let store = ctx.store.clone();
            ctx.actions.register(
                id,
                Rc::new(move || {
                    let Some(window) = window.upgrade() else {
                        return;
                    };
                    let bin_open = store.state().ui.bin_open;
                    let mut at = store.local().focus;
                    for _ in 0..7 {
                        at = router::next_region(at, forward, bin_open);
                        let Some(target) = find_named(window.upcast_ref(), region_entry(at)) else {
                            continue;
                        };
                        if target.child_focus(gtk::DirectionType::TabForward) || target.grab_focus()
                        {
                            // Keyboard navigation: the focus shows (and a
                            // focused button takes Space and Enter, G§3.8).
                            window.set_focus_visible(true);
                            return;
                        }
                    }
                }),
            );
        }
    }

    /// Every region's and named part's bounds in window coordinates
    /// (logical px): the screenshot checks' `regions` (G§8.4).
    pub fn regions(&self) -> BTreeMap<String, [f64; 4]> {
        let mut out = BTreeMap::new();
        let root: gtk::Widget = self.window.clone().upcast();
        walk(&root, &mut |w| {
            let name = w.widget_name();
            let wanted = name.starts_with("region-") || NAMED_PARTS.contains(&name.as_str());
            if wanted
                && w.is_visible()
                && w.is_mapped()
                && let Some(b) = w.compute_bounds(&self.window)
            {
                out.insert(
                    name.to_string(),
                    [
                        f64::from(b.x()),
                        f64::from(b.y()),
                        f64::from(b.width()),
                        f64::from(b.height()),
                    ],
                );
            }
        });
        out
    }
}

/// Applies the layout class, the startup page and the sheet.
fn apply_local(ctx: &Ctx, parts: &Rc<Parts>, local: &LocalUi) {
    let layout = local.layout;
    let narrow = layout.width == LayoutWidth::Narrow;
    if parts.split.is_collapsed() != narrow {
        parts.split.set_collapsed(narrow);
    }
    let width = layout.panel_width().unwrap_or(384);
    set_panel_width(&parts.split, width);
    if layout.width == LayoutWidth::Medium {
        parts.panel.add_css_class("panel-medium");
    } else {
        parts.panel.remove_css_class("panel-medium");
    }
    let sheet = local.overlays.contains(&Overlay::LayersSheet);
    let show = !narrow || sheet;
    if !narrow {
        unstill_sheet(parts);
    }
    if parts.split.shows_sidebar() != show {
        if narrow && show {
            still_sheet(parts, ctx.theme.motion().reduced);
        }
        parts.split.set_show_sidebar(show);
    }
    if !narrow && sheet {
        // The sheet only exists in Narrow: a wider window docks the panel.
        ctx.store.update_local(|l| {
            l.close_overlays(|o| *o == Overlay::LayersSheet);
        });
    }
    if parts.timeline.height_request() != layout.timeline_height() {
        parts.timeline.set_height_request(layout.timeline_height());
    }
    // The editor works only once the Session is open.
    parts.split.set_sensitive(local.is_open());
    match &local.startup {
        StartupPhase::Blocked(kind) => {
            let reuse = parts.page.borrow().as_ref().is_some_and(|p| p.update(kind));
            if !reuse {
                let page = BlockingPage::new(ctx, kind.clone());
                while let Some(child) = parts.page_slot.first_child() {
                    parts.page_slot.remove(&child);
                }
                parts.page_slot.append(&page.widget());
                *parts.page.borrow_mut() = Some(page);
            }
            let switching = parts.stack.visible_child_name().as_deref() != Some("page");
            if switching {
                parts.stack.set_visible_child_name("page");
            }
            // A page that replaces another takes the focus too: the old
            // page's focused button is gone, and keys sent to a widget out
            // of the window never reach it (X1 replaced by S23 lost Quit).
            if switching || !reuse {
                let parts = Rc::clone(parts);
                guard::idle_local_once("window: the page's focus", move || {
                    // Cloned out first: focusing runs the window's focus
                    // handlers, which may rebuild the page.
                    let page = parts.page.borrow().as_ref().map(BlockingPage::focus_target);
                    if let Some(Some(target)) = page {
                        target.grab_focus();
                        // A page is keyboard-first: its focused button shows
                        // its ring (WP8's frames draw it).
                        if let Some(window) = target.root().and_downcast::<gtk::Window>() {
                            window.set_focus_visible(true);
                        }
                    }
                });
            }
        }
        StartupPhase::Opening { .. } | StartupPhase::Open | StartupPhase::Closing => {
            if parts.stack.visible_child_name().as_deref() != Some("editor") {
                parts.stack.set_visible_child_name("editor");
            }
        }
    }
}

/// Under reduced motion the Narrow sheet neither slides in nor out (D-30,
/// DESIGN Motion's panel overlay: no motion). `adw::OverlaySplitView`
/// slides it with a spring that follows only GTK's `gtk-enable-animations`
/// (`adw_get_enable_animations`; libadwaita reads no reduced motion), so
/// that is off while the sheet is open under reduced motion, from its
/// opening or from the policy's change ([`follow_sheet_motion`]) until it
/// goes: its own close (a click on the content it dims) is libadwaita's
/// too. [`unstill_sheet`] gives the setting back. The other animations
/// meanwhile go at once, also still under reduced motion; the policy reads
/// the setting as the desktop's on meanwhile (`motion::hold_animations_off`).
fn still_sheet(parts: &Parts, reduced: bool) {
    let settings = parts.split.settings();
    if reduced && settings.is_gtk_enable_animations() && !parts.stilled.replace(true) {
        motion::hold_animations_off(true);
        settings.set_gtk_enable_animations(false);
    }
}

/// GTK's animations back to the desktop's once the sheet went (or docked,
/// or motion is allowed again): `reset_property`, so GTK follows the
/// desktop's setting again (a value the window sets, `true` too, is the
/// application's to GTK, which ignores the desktop's changes from then on:
/// WP10's code audit).
fn unstill_sheet(parts: &Parts) {
    if parts.stilled.replace(false) {
        motion::hold_animations_off(false);
        parts
            .split
            .settings()
            .reset_property("gtk-enable-animations");
    }
}

/// The open Narrow sheet follows a live change of the motion policy (the
/// config's `ui.reduced_motion`, the system's): held still when motion is
/// reduced while it is open, so its close doesn't slide either, and given
/// back to GTK's animations when motion is allowed again (WP10's code
/// audit: only a sheet's opening held it).
fn follow_sheet_motion(ctx: &Ctx, parts: &Rc<Parts>) {
    let weak = Rc::downgrade(parts);
    ctx.theme
        .follow_motion(move |policy| {
            let Some(parts) = weak.upgrade() else {
                return;
            };
            if !(parts.split.is_collapsed() && parts.split.shows_sidebar()) {
                return;
            }
            if policy.reduced {
                still_sheet(&parts, true);
            } else {
                unstill_sheet(&parts);
            }
        })
        .detach();
}

fn set_panel_width(split: &adw::OverlaySplitView, width: i32) {
    let width = f64::from(width);
    if (split.max_sidebar_width() - width).abs() > f64::EPSILON
        || (split.min_sidebar_width() - width).abs() > f64::EPSILON
    {
        split.set_max_sidebar_width(width);
        split.set_min_sidebar_width(width);
    }
}

/// The widget name of a focus region's root (the inverse of
/// `router::region_for_name`).
pub fn region_name(region: FocusRegion) -> &'static str {
    match region {
        FocusRegion::Header => "region-header",
        FocusRegion::Stage => "region-stage",
        FocusRegion::Transport => "transport",
        FocusRegion::TimelineToolbar => "timeline-toolbar",
        FocusRegion::Timeline => "region-timeline",
        FocusRegion::Panel => "region-panel",
        FocusRegion::Bin => "region-bin",
        FocusRegion::Field | FocusRegion::Other => "",
    }
}

/// The timeline canvas's name: where `region.next` and `region.prev` enter
/// the timeline region.
pub const TIMELINE_CANVAS: &str = "timeline-canvas";

/// Where `region.next` and `region.prev` (D-10) put the focus in `region`:
/// the first widget of GTK's chain under the widget named this. That is the
/// region's root, but for the timeline: its canvas, past the toolbar, which
/// is a region of its own inside `region-timeline` (GTK's chain, `Tab` and
/// `Shift+Tab`, still goes through the toolbar and the track headers: BRIEF
/// §6's order, WP10's focus audit).
pub fn region_entry(region: FocusRegion) -> &'static str {
    match region {
        FocusRegion::Timeline => TIMELINE_CANVAS,
        other => region_name(other),
    }
}

/// The focus region of the focused widget `w`.
pub fn focus_region(w: &gtk::Widget) -> FocusRegion {
    if is_editable(w) {
        return FocusRegion::Field;
    }
    let mut at = Some(w.clone());
    while let Some(widget) = at {
        if let Some(region) = router::region_for_name(widget.widget_name().as_str()) {
            return region;
        }
        at = widget.parent();
    }
    FocusRegion::Other
}

/// The first widget named `name` under `root` (depth first).
pub fn find_named(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if name.is_empty() {
        return None;
    }
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        if let Some(found) = find_named(&c, name) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    f(root);
    let mut child = root.first_child();
    while let Some(c) = child {
        walk(&c, f);
        child = c.next_sibling();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_names_round_trip() {
        for region in [
            FocusRegion::Header,
            FocusRegion::Stage,
            FocusRegion::Transport,
            FocusRegion::TimelineToolbar,
            FocusRegion::Timeline,
            FocusRegion::Panel,
            FocusRegion::Bin,
        ] {
            assert_eq!(router::region_for_name(region_name(region)), Some(region));
        }
        assert_eq!(region_name(FocusRegion::Other), "");
    }

    /// F6 enters every region at its root, the timeline at its canvas (past
    /// the toolbar, a region of its own); none at a field or nowhere.
    #[test]
    fn regions_are_entered_at_their_root_but_the_timeline() {
        assert_eq!(region_entry(FocusRegion::Timeline), TIMELINE_CANVAS);
        for region in [
            FocusRegion::Header,
            FocusRegion::Transport,
            FocusRegion::TimelineToolbar,
            FocusRegion::Panel,
            FocusRegion::Bin,
        ] {
            assert_eq!(region_entry(region), region_name(region), "{region:?}");
        }
        assert_eq!(region_entry(FocusRegion::Field), "");
    }

    /// The first window fits the largest monitor (90 % of it), never
    /// beyond 1600×1000 nor below 800×600 (GUI-16).
    #[test]
    fn the_first_window_fits_the_largest_monitor() {
        /// What, the monitors, the first window's size.
        type Row = (&'static str, &'static [(i32, i32)], (i32, i32));
        #[rustfmt::skip]
        let rows: [Row; 11] = [
            ("no monitor", &[], DEFAULT_SIZE),
            ("a 0×0 monitor", &[(0, 0)], DEFAULT_SIZE),
            ("1920×1080 holds it", &[(1920, 1080)], (1600, 972)),
            ("2560×1440 holds it", &[(2560, 1440)], DEFAULT_SIZE),
            ("a 1366×768 laptop", &[(1366, 768)], (1229, 691)),
            ("1920×1080 at 150 %", &[(1280, 720)], (1152, 648)),
            ("a 1024×768 VM console", &[(1024, 768)], (921, 691)),
            ("800×600: the minimum", &[(800, 600)], MIN_SIZE),
            ("640×480: still the minimum", &[(640, 480)], MIN_SIZE),
            ("a laptop next to a bigger monitor", &[(1366, 768), (2560, 1440)], DEFAULT_SIZE),
            ("the largest by area", &[(1080, 1920), (1920, 1200)], (1600, 1000)),
        ];
        for (what, monitors, want) in rows {
            assert_eq!(first_size(monitors), want, "{what}: {monitors:?}");
        }
        // A portrait monitor alone: narrow and as tall as the default.
        assert_eq!(first_size(&[(1080, 1920)]), (972, 1000));
    }

    proptest::proptest! {
        /// Whatever the monitors: the first window is at most the default
        /// size and at least the minimum, at most 90 % of the largest
        /// monitor in each dimension unless that is below the minimum,
        /// and exactly the default when that monitor holds it (or there is
        /// none of known size).
        #[test]
        fn the_first_window_is_within_the_monitor_and_the_minimum(
            monitors in proptest::collection::vec((-10..10_000i32, -10..10_000i32), 0..4),
        ) {
            let (w, h) = first_size(&monitors);
            proptest::prop_assert!(w <= DEFAULT_SIZE.0 && h <= DEFAULT_SIZE.1);
            proptest::prop_assert!(w >= MIN_SIZE.0 && h >= MIN_SIZE.1);
            let largest = monitors
                .iter()
                .filter(|&&(mw, mh)| mw > 0 && mh > 0)
                .max_by_key(|&&(mw, mh)| i64::from(mw) * i64::from(mh));
            match largest {
                None => proptest::prop_assert_eq!((w, h), DEFAULT_SIZE),
                Some(&(mw, mh)) => {
                    for (size, monitor, default, min) in
                        [(w, mw, DEFAULT_SIZE.0, MIN_SIZE.0), (h, mh, DEFAULT_SIZE.1, MIN_SIZE.1)]
                    {
                        // 90 % of the monitor, in hundredths of a px.
                        let share = i64::from(monitor) * 90;
                        let (size, default, min) =
                            (i64::from(size), i64::from(default), i64::from(min));
                        if share >= min * 100 {
                            proptest::prop_assert!(size * 100 <= share, "{size} over 90 % of {monitor}");
                            // As large as fits: the next px would be over.
                            proptest::prop_assert!(size == default || (size + 1) * 100 > share);
                        } else {
                            proptest::prop_assert_eq!(size, min);
                        }
                        if share >= default * 100 {
                            proptest::prop_assert_eq!(size, default);
                        }
                    }
                }
            }
        }
    }
}
