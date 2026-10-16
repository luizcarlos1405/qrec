//! The window (G§3.15): its tree, its regions, the overlay layers, the
//! adaptive modes, the focus regions and the blocking page.
//!
//! ```text
//! adw::ApplicationWindow #qrec  (default 1600×1000, min 800×600)
//! └ gtk::Overlay  window_layer          ← CheatSheet (scrim-strong over everything)
//!   └ Box V
//!     ├ gtk::Overlay  content_layer     ← Modal, Settings, Palette (scrim to y 968), ExportPanel, ToastHost
//!     │ └ gtk::Stack: editor | page
//!     │   ├ adw::ToolbarView  editor
//!     │   │ ├ top: Header
//!     │   │ └ adw::OverlaySplitView, sidebar at the end: Panel (384; 320; overlay sheet)
//!     │   │   └ Box V  work
//!     │   │       ├ Box H  stage_row: BinDrawer (352) | Stage (hexpand)
//!     │   │       └ Timeline (312 px; 232 compact)
//!     │   └ adw::ToolbarView  page: minimal header + BlockingPage
//!     └ HintBar (32 px, not focusable)
//! ```
//!
//! `LocalUi.focus` follows the window's focus widget: the nearest ancestor
//! named `region-<name>` (or a named part that takes focus), `Field` in an
//! editable. `region.next` and `region.prev` move between the regions
//! (D-10).

pub mod adaptive;
pub mod layers;

use std::cell::RefCell;
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
use crate::timeline::Timeline;
use crate::toasts::ToastHost;
use crate::{Ctx, guard, guarded};
use layers::WindowLayers;

/// The window's default size.
pub const DEFAULT_SIZE: (i32, i32) = (1600, 1000);
/// The smallest window (the breakpoints need it).
pub const MIN_SIZE: (i32, i32) = (800, 600);

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
    root_box: gtk::Box,
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
    /// The window and its two overlay layers, empty (G§3.3 step 2).
    pub fn new(app: &adw::Application, title: &str) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(title)
            .default_width(DEFAULT_SIZE.0)
            .default_height(DEFAULT_SIZE.1)
            .width_request(MIN_SIZE.0)
            .height_request(MIN_SIZE.1)
            .build();
        window.set_widget_name("qrec");
        window.add_css_class("qrec");
        let window_layer = gtk::Overlay::new();
        let root_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let content_layer = gtk::Overlay::new();
        content_layer.set_vexpand(true);
        root_box.append(&content_layer);
        window_layer.set_child(Some(&root_box));
        window.set_content(Some(&window_layer));
        let layers = WindowLayers::new(&window_layer, &content_layer);
        Self {
            window,
            layers,
            content_layer,
            root_box,
            parts: RefCell::new(None),
        }
    }

    /// Builds every region and the layout; follows `LocalUi` (layout,
    /// startup page, overlays) and the focus.
    pub fn populate(&self, ctx: &Ctx, title: &str) {
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

        // The blocking pages' minimal header (C04 `Xba2H`): the wordmark,
        // the folder's name, close.
        let page_header = adw::HeaderBar::new();
        page_header.add_css_class("qrec-minimal-header");
        let wordmark = gtk::Label::new(Some("Qrec"));
        wordmark.add_css_class("wordmark");
        wordmark.add_css_class("t-wordmark");
        page_header.pack_start(&wordmark);
        page_header.set_title_widget(Some(&adw::WindowTitle::new(title, "")));
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
        self.content_layer.set_child(Some(&stack));
        // The toasts float in the content layer, passive.
        self.content_layer.add_overlay(&toasts.widget());
        self.content_layer.set_clip_overlay(&toasts.widget(), false);

        let hint_widget = hintbar.widget();
        // The hint bar never takes the window's spare height.
        hint_widget.set_vexpand(false);
        self.root_box.append(&hint_widget);
        self.layers.set_base(
            vec![stack.clone().upcast()],
            vec![stack.clone().upcast(), hint_widget],
        );

        let parts = Rc::new(Parts {
            stack,
            split,
            timeline: timeline_widget,
            panel: panel_widget,
            page_slot,
            page: RefCell::new(None),
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
        parts
            .split
            .connect_show_sidebar_notify(guarded!("window: the sheet", move |split| {
                if split.is_collapsed() && !split.shows_sidebar() {
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
                        let Some(target) = find_named(window.upcast_ref(), region_name(at)) else {
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
    if parts.split.shows_sidebar() != show {
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
            if parts.stack.visible_child_name().as_deref() != Some("page") {
                parts.stack.set_visible_child_name("page");
                let parts = Rc::clone(parts);
                guard::idle_local_once("window: the page's focus", move || {
                    // Cloned out first: focusing runs the window's focus
                    // handlers, which may rebuild the page.
                    let page = parts.page.borrow().as_ref().map(BlockingPage::focus_target);
                    if let Some(Some(target)) = page {
                        target.grab_focus();
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
}
