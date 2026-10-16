//! The Onion layers panel (S06, S07, S12, S13, S13b, C09–C13): header,
//! take banner, layer and audio rows, meters, the inspector, menus, the
//! autocut page. Owner: WP5. Registers `autocut.page`.
//!
//! - The region (`region-panel`) is docked at the width the layout gives
//!   (384; Medium 320, the inspector 2-up) or shown as the Narrow layout's
//!   overlay sheet (G§3.15). Its content scrolls (D-25).
//! - Two pages in a `gtk::Stack`: the layers page (the header, then a
//!   scrolled column: the banner, Layers, Add layer, Audio, Add audio
//!   track, the Inspector) and the autocut page (S12), which slides in.
//!   `LocalUi.panel_page` says which shows; the autocut page writes it and
//!   `LocalUi.autocut_preview` (the hatches WP3 draws).
//! - Everything is computed by `qrec_core::view::{panel, meter, autocut}`
//!   from the newest snapshot and `LocalUi` (live statuses, sources, the
//!   layout, a canvas drag), and every control sends a Command (R8).
//! - `autocut.page` (the `Shift+C` key, the toolbar's settings icon) opens
//!   the page, or closes it when it shows (`Esc` runs it too: its Cancel).

pub mod audio;
pub mod autocut;
pub mod dry_run;
pub mod header;
pub mod inspector;
pub mod menus;
pub mod rows;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use qrec_core::State;
use qrec_core::ctx::AudioDeviceInfo;
use qrec_core::view::notices::unavailable_notice;
use qrec_core::view::panel::{self, AddGroup};
use qrec_core::view::{PanelPageKind, autocut as autocut_view};
use qrec_shell::capture::live_sources_view;

use crate::Ctx;
use crate::store::local::{LayoutWidth, LocalUi};
use audio::AudioList;
use autocut::AutocutPage;
use header::{BannerView, HeaderView, SectionHeader, set_enabled_with_reason};
use inspector::InspectorView;
use menus::Menu;
use rows::LayerList;

/// The panel region (`region-panel`).
pub struct Panel {
    root: gtk::Box,
    _inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    stack: gtk::Stack,
    header: HeaderView,
    banner: BannerView,
    layers_header: SectionHeader,
    layers: LayerList,
    add_layer: gtk::MenuButton,
    add_layer_menu: Menu,
    audio_header: SectionHeader,
    audio: AudioList,
    add_audio: gtk::MenuButton,
    add_audio_menu: Menu,
    inspector: InspectorView,
    autocut: AutocutPage,
    /// What the last refresh showed (layout, page), to skip no-ops.
    shown: RefCell<Option<Shown>>,
}

#[derive(Debug, Clone, PartialEq)]
struct Shown {
    rev: u64,
    local: LocalBits,
}

/// The parts of `LocalUi` the panel shows.
#[derive(Debug, Clone, PartialEq)]
struct LocalBits {
    page: PanelPageKind,
    width: LayoutWidth,
    live: std::sync::Arc<Vec<qrec_shell::capture::LiveSourceStatus>>,
    sources: Option<std::sync::Arc<qrec_shell::sources::SourceInventory>>,
    drag: Option<crate::store::local::LayerDragPreview>,
}

impl LocalBits {
    fn of(local: &LocalUi) -> Self {
        Self {
            page: local.panel_page,
            width: local.layout.width,
            live: local.live_status.clone(),
            sources: local.sources.clone(),
            drag: local.layer_drag.clone(),
        }
    }
}

/// A flat "+ Add …" button opening `menu`.
fn add_button(ctx: &Ctx, text: &str, menu: &Menu, name: &str) -> gtk::MenuButton {
    let button = menus::menu_button(menu, &["flat", "add-button"], text);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.append(&gtk::Image::from_icon_name("list-add-symbolic"));
    content.append(&gtk::Label::new(Some(text)));
    button.set_child(Some(&content));
    button.set_halign(gtk::Align::Start);
    button.set_widget_name(name);
    let refresh = {
        let ctx = ctx.clone();
        Rc::new(move || menus::refresh_sources(&ctx)) as Rc<dyn Fn()>
    };
    menus::track_menu(ctx, &button, Some(refresh));
    button
}

impl Panel {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("region-panel");
        root.add_css_class("qrec-panel");
        root.set_vexpand(true);
        root.update_property(&[gtk::accessible::Property::Label("Onion layers")]);

        let add_layer_menu = Menu::new();
        let add_audio_menu = Menu::new();
        let header = HeaderView::new(ctx, &add_layer_menu);
        let banner = BannerView::new(ctx);
        let layers_header = SectionHeader::new("Layers");
        let layers = LayerList::new(ctx);
        let add_layer = add_button(ctx, "Add layer", &add_layer_menu, "add-layer");
        let audio_header = SectionHeader::new("Audio");
        let audio = AudioList::new(ctx);
        let add_audio = add_button(ctx, "Add audio track", &add_audio_menu, "add-audio");
        let inspector = InspectorView::new(ctx);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.add_css_class("panel-column");
        column.append(&banner.widget());
        let section =
            |header: &SectionHeader, body: &gtk::Widget, add: Option<&gtk::MenuButton>| {
                let group = gtk::Box::new(gtk::Orientation::Vertical, 0);
                group.add_css_class("panel-section");
                group.append(&header.widget());
                group.append(body);
                if let Some(add) = add {
                    group.append(add);
                }
                group
            };
        column.append(&section(&layers_header, &layers.widget(), Some(&add_layer)));
        column.append(&section(&audio_header, &audio.widget(), Some(&add_audio)));
        column.append(&inspector.widget());
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&column));
        let layers_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        layers_page.add_css_class("layers-page");
        layers_page.append(&header.widget());
        layers_page.append(&scroller);

        let autocut = AutocutPage::new(ctx);
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
        stack.set_transition_duration(200);
        stack.set_vexpand(true);
        stack.add_named(&layers_page, Some("layers"));
        stack.add_named(&autocut.widget(), Some("autocut"));
        stack.set_visible_child_name("layers");
        root.append(&stack);

        let inner = Rc::new(Inner {
            ctx: ctx.clone(),
            root: root.clone(),
            stack,
            header,
            banner,
            layers_header,
            layers,
            add_layer,
            add_layer_menu,
            audio_header,
            audio,
            add_audio,
            add_audio_menu,
            inspector,
            autocut,
            shown: RefCell::new(None),
        });
        let panel = Self {
            root,
            _inner: Rc::clone(&inner),
        };
        panel.wire(&inner);
        inner.refresh(&ctx.store.state(), &ctx.store.local().clone());
        panel
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn wire(&self, inner: &Rc<Inner>) {
        let ctx = &inner.ctx;
        // The renames the overflow menus start.
        let layers = inner.layers.clone();
        let audio = inner.audio.clone();
        menus::install_actions(
            &inner.root,
            ctx,
            Rc::new(move |id| layers.rename(id)),
            Rc::new(move |id| audio.rename(id)),
        );
        // `autocut.page`: open, or close what shows (its Cancel).
        let weak = Rc::downgrade(inner);
        ctx.actions.register(
            "autocut.page",
            Rc::new(move || {
                let Some(inner) = weak.upgrade() else { return };
                if inner.autocut.is_open() {
                    inner.autocut.close();
                    return;
                }
                let state = inner.ctx.store.state();
                match autocut_view::page_enabled(&state) {
                    Ok(()) => inner.autocut.open(),
                    Err(why) => {
                        if let Some(notice) = unavailable_notice("autocut.preview", &why, &state) {
                            inner.ctx.store.notify(notice);
                        }
                    }
                }
            }),
        );
        let weak = Rc::downgrade(inner);
        ctx.store
            .subscribe(move |change| {
                if let Some(inner) = weak.upgrade() {
                    let local = inner.ctx.store.local().clone();
                    inner.refresh(&change.next, &local);
                    inner.autocut.update(&change.next);
                }
            })
            .detach();
        let weak = Rc::downgrade(inner);
        ctx.store
            .subscribe_local(move |local| {
                if let Some(inner) = weak.upgrade() {
                    let state = inner.ctx.store.state();
                    inner.refresh(&state, local);
                }
            })
            .detach();
    }
}

impl Inner {
    /// Shows `state` and `local` (skipped when neither changed).
    fn refresh(&self, state: &State, local: &LocalUi) {
        let shown = Shown {
            rev: self.ctx.store.rev(),
            local: LocalBits::of(local),
        };
        if self.shown.borrow().as_ref() == Some(&shown) {
            return;
        }
        *self.shown.borrow_mut() = Some(shown.clone());
        let page = match shown.local.page {
            PanelPageKind::Layers => "layers",
            PanelPageKind::Autocut => "autocut",
        };
        if self.stack.visible_child_name().as_deref() != Some(page) {
            self.stack.set_visible_child_name(page);
        }
        let live = live_sources_view(&local.live_status);
        let header = panel::header(state);
        let add = panel::add_enabled(state);
        self.header.update(&header, &add);
        self.banner.update(&header, &panel::layout_enabled(state));
        let layer_rows = panel::layer_rows(state, &live);
        self.layers_header
            .set_caption(&layer_count(layer_rows.len()));
        self.layers.update(&layer_rows, state);
        let audio_rows = panel::audio_rows(state, &live);
        self.audio_header
            .set_caption(&layer_count(audio_rows.len()));
        let (mics, monitors): (&[AudioDeviceInfo], &[AudioDeviceInfo]) = match &local.sources {
            Some(inventory) => (&inventory.mics, &inventory.monitors),
            None => (&[], &[]),
        };
        self.audio.update(&audio_rows, state, (mics, monitors));
        self.inspector.update(state, local);
        // The Add menus, from what discovery found.
        set_enabled_with_reason(self.add_layer.upcast_ref(), &add, "Add layer");
        set_enabled_with_reason(self.add_audio.upcast_ref(), &add, "Add audio track");
        let (screens, cameras) = match &local.sources {
            Some(inventory) => (inventory.screens.as_slice(), inventory.cameras.as_slice()),
            None => (&[][..], &[][..]),
        };
        let layer_items = panel::add_layer_items(state, screens, cameras);
        self.add_layer_menu
            .update(menus::add_signature(&layer_items), |menu| {
                menus::fill_add_menu(menu, &layer_items, &[AddGroup::Screen, AddGroup::Camera]);
            });
        let audio_items = panel::add_audio_items(state, mics, monitors);
        self.add_audio_menu
            .update(menus::add_signature(&audio_items), |menu| {
                menus::fill_add_menu(menu, &audio_items, &[AddGroup::Mic, AddGroup::SystemAudio]);
            });
    }
}

/// A section's count caption ("2"; none when empty, S01a).
fn layer_count(n: usize) -> String {
    if n == 0 { String::new() } else { n.to_string() }
}
