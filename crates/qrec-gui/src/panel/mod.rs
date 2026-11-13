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
pub mod scenes;

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
use crate::store::Subscription;
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
    scene_bar: scenes::SceneBarView,
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
    /// The pages' transition follows the motion policy.
    _motion: Subscription,
}

/// The name of the panel's page stack (the layers page and the autocut
/// page; the test channel reports its transition, G§3.18).
pub const PAGES: &str = "panel-pages";

/// How the panel switches pages (C13): a 200 ms slide, instant under
/// reduced motion (D-30: transitions are instant, G§3.14).
pub fn page_transition(reduced: bool) -> gtk::StackTransitionType {
    if reduced {
        gtk::StackTransitionType::None
    } else {
        gtk::StackTransitionType::SlideLeftRight
    }
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
    content.append(&gtk::Image::from_icon_name("qrec-plus-symbolic"));
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
        let scene_bar = scenes::SceneBarView::new(ctx);
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
        column.append(&scene_bar.widget());
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
        stack.set_widget_name(PAGES);
        stack.set_transition_duration(200);
        // The pages slide unless motion is reduced (D-30), followed live.
        let motion = ctx.theme.follow_motion({
            let stack = stack.downgrade();
            move |motion| {
                if let Some(stack) = stack.upgrade() {
                    stack.set_transition_type(page_transition(motion.reduced));
                }
            }
        });
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
            scene_bar,
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
            _motion: motion,
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
                    inner.after_scene_switch(&change.prev, &change.next, &local);
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

/// For each Live layer whose source isn't connected, the screens or
/// cameras that are (and not in the scene yet), as `LayerSet`s giving the
/// layer that source (the row's Replace…, SPEC §5.6).
fn replace_items(
    state: &State,
    rows: &[panel::LayerRow],
    local: &LocalUi,
) -> std::collections::BTreeMap<qrec_core::ids::LayerId, Vec<(String, qrec_core::Command)>> {
    use qrec_core::model::VideoSource;
    use qrec_core::protocol::command::{LayerPatch, SceneTarget, VideoSourceArg};
    let mut out = std::collections::BTreeMap::new();
    let Some(inventory) = &local.sources else {
        return out;
    };
    let scene = &state.project.scene;
    let used = |source: &VideoSourceArg| {
        scene.layers.iter().any(|l| match (&l.source, source) {
            (VideoSource::Screen { output, .. }, VideoSourceArg::Screen { output: o }) => {
                output == o
            }
            (VideoSource::Camera { device, .. }, VideoSourceArg::Camera { device: d }) => {
                device == d
            }
            _ => false,
        })
    };
    for row in rows.iter().filter(|r| r.not_connected) {
        let Some(layer) = scene.layer(&row.layer) else {
            continue;
        };
        let candidates: Vec<(String, VideoSourceArg)> = match layer.source {
            VideoSource::Screen { .. } => inventory
                .screens
                .iter()
                .map(|s| {
                    let label = if s.description.is_empty() {
                        s.output.clone()
                    } else {
                        format!("{} \u{b7} {}", s.output, s.description)
                    };
                    (
                        label,
                        VideoSourceArg::Screen {
                            output: s.output.clone(),
                        },
                    )
                })
                .collect(),
            VideoSource::Camera { .. } => inventory
                .cameras
                .iter()
                .map(|c| {
                    let label = if c.label.is_empty() {
                        c.device.clone()
                    } else {
                        c.label.clone()
                    };
                    (
                        label,
                        VideoSourceArg::Camera {
                            device: c.device.clone(),
                        },
                    )
                })
                .collect(),
        };
        let items: Vec<(String, qrec_core::Command)> = candidates
            .into_iter()
            .filter(|(_, source)| !used(source))
            .map(|(label, source)| {
                (
                    label,
                    qrec_core::Command::LayerSet {
                        target: SceneTarget::Project,
                        layer: row.layer.clone(),
                        patch: LayerPatch {
                            source: Some(source),
                            ..LayerPatch::default()
                        },
                    },
                )
            })
            .collect();
        out.insert(row.layer.clone(), items);
    }
    out
}

impl Inner {
    /// After a switch to a saved scene (a new "Use scene" entry), a toast
    /// names the sources that aren't connected, with Undo (board 01).
    fn after_scene_switch(&self, prev: &State, next: &State, local: &LocalUi) {
        if next.history.cursor <= prev.history.cursor {
            return;
        }
        let Some(entry) = next
            .history
            .entries
            .iter()
            .find(|e| e.seq == next.history.cursor)
        else {
            return;
        };
        let Some(scene_name) = entry.label.strip_prefix("Use scene ") else {
            return;
        };
        let _ = local;
        // What discovery found may be old: look again, then say what the
        // scene names that isn't here.
        let scene = next.project.scene.clone();
        let scene_name = scene_name.to_owned();
        let store = self.ctx.store.clone();
        menus::refresh_sources_then(&self.ctx, move |inventory| {
            let gone: Vec<String> = scene
                .layers
                .iter()
                .filter_map(|l| {
                    panel::not_connected(&l.source, &inventory.screens, &inventory.cameras)
                })
                .collect();
            switch_notice(&store, &scene_name, &gone);
        });
    }

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
        let saved = match qrec_core::ui_normalize::edit_target(&state.project, &state.ui) {
            Some(qrec_core::protocol::command::SceneTarget::Take(take)) => {
                panel::apply_saved_items(state, &take)
            }
            _ => Vec::new(),
        };
        self.banner
            .update(&header, &panel::layout_enabled(state), &saved);
        let mut layer_rows = panel::layer_rows(state, &live);
        // A saved scene can name a screen or camera that isn't here (SPEC §5.6).
        if let Some(inventory) = &local.sources {
            panel::mark_not_connected(
                &mut layer_rows,
                state,
                &inventory.screens,
                &inventory.cameras,
            );
        }
        let missing: std::collections::BTreeSet<qrec_core::ids::LayerId> = layer_rows
            .iter()
            .filter(|r| r.not_connected)
            .map(|r| r.layer.clone())
            .collect();
        self.scene_bar
            .update(panel::scene_bar(state).as_ref(), missing);
        let replace = replace_items(state, &layer_rows, local);
        self.layers.set_replace(replace);
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

/// Sends an inline rename's command (R46): when the core refuses it at
/// once, `refused` puts the row's name back (given the state then) before
/// the default reply policy toasts why.
pub(crate) fn send_rename(
    store: &crate::store::Store,
    cmd: qrec_core::Command,
    refused: impl FnOnce(&State) + 'static,
) {
    let weak = store.downgrade();
    let sent = cmd.clone();
    store.dispatch_with(cmd, false, move |reply| {
        let Some(store) = weak.upgrade() else { return };
        if reply.result.is_err() && !reply.late {
            refused(&store.state());
        }
        store.apply_default(&sent, reply);
    });
}

/// The toast after a switch to `scene_name` when sources in `gone` aren't
/// connected (board 07-named-scenes/01): with Undo.
fn switch_notice(store: &crate::store::Store, scene_name: &str, gone: &[String]) {
    let Some(first) = gone.first() else { return };
    use qrec_core::protocol::toast::{ToastAction, ToastLevel, UiAction};
    use qrec_core::view::notices::{Notice, NoticeAction};
    let title = if gone.len() == 1 {
        format!("Switched to {scene_name}. {first}.")
    } else {
        format!(
            "Switched to {scene_name}. {} sources aren't connected.",
            gone.len()
        )
    };
    let body = if gone.len() == 1 {
        "Its layer stays in the scene, and new takes record without it until it's back or you replace it."
    } else {
        "Their layers stay in the scene, and new takes record without them until they're back or you replace them."
    };
    let notice = Notice::new(ToastLevel::Warning, title)
        .with_body(body)
        .with_action(NoticeAction::Core(ToastAction {
            label: "Undo".to_owned(),
            action: UiAction::Dispatch(Box::new(qrec_core::Command::Undo { steps: 1 })),
        }));
    store.notify(notice);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pages slide unless motion is reduced (D-30), when the switch is
    /// instant (the nested `the_pages_slide_unless_motion_is_reduced`
    /// checks the stack follows the policy).
    #[test]
    fn the_pages_switch_at_once_under_reduced_motion() {
        assert_eq!(
            page_transition(false),
            gtk::StackTransitionType::SlideLeftRight
        );
        assert_eq!(page_transition(true), gtk::StackTransitionType::None);
    }
}
