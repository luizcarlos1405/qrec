//! C09 layer rows (S02, S06, S13, S13b): a `gtk::ListBox` of 44 px rows,
//! topmost layer first. A row is the grip, the eye, the source icon, the
//! name (a `gtk::EditableLabel`, edited from the overflow's Rename) over
//! the source line, the lock and the overflow menu (Rename, Move up, Move
//! down, Remove). The grip, the eye and the lock show on hover only
//! (revealed by opacity); a hidden layer keeps its eye-off, a locked one its
//! lock.
//!
//! - A click selects the layer (`SetUi{selected_layer}`).
//! - The eye sends `LayerSet{visible}`, the lock `LayerSet{locked}`, a
//!   rename `LayerSet{name}` (Enter saves, Esc cancels).
//! - Dragging a row (4 px) lifts a copy of it (e1) and shows a 2 px drop
//!   line at the nearest row boundary; the release sends one
//!   `LayerOrder{to: n − 1 − row}` (`view::panel::reorder`), `Esc` cancels
//!   (D-42 through the key router). The drag is the panel's own gesture,
//!   inside the list: no compositor drag icon.
//! - Errors (Live: the portal refused the screen, a source died) and a
//!   take's missing source replace the source line and the icon.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::graphene;
use gtk::prelude::*;
use qrec_core::protocol::command::{LayerPatch, SceneTarget};
use qrec_core::view::DragKind;
use qrec_core::view::panel::{self, LayerRow, NO_LAYERS};
use qrec_core::{LayerId, State, VideoSource};

use super::menus::{self, Menu};
use crate::Ctx;
use crate::guarded;

/// How far a press moves before it is a reorder drag.
const DRAG_THRESHOLD: f64 = 4.0;

/// The layer rows. Cheap to clone; main thread only.
#[derive(Clone)]
pub struct LayerList {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    overlay: gtk::Overlay,
    list: gtk::ListBox,
    empty: gtk::Box,
    ghost: Ghost,
    line: gtk::Box,
    rows: RefCell<Vec<RowView>>,
    drag: RefCell<Option<Drag>>,
}

/// The lifted copy of a dragged row.
struct Ghost {
    root: gtk::Box,
    icon: gtk::Image,
    name: gtk::Label,
    source: gtk::Label,
}

struct RowView {
    layer: LayerId,
    row: gtk::ListBoxRow,
    grip: gtk::Image,
    eye: gtk::Button,
    icon: gtk::Image,
    name: gtk::EditableLabel,
    source: gtk::Label,
    lock: gtk::Button,
    menu_button: gtk::MenuButton,
    menu: Menu,
    model: RefCell<Option<LayerRow>>,
    /// The scene the row's rename edits (`edit_target`).
    edits: RefCell<Option<Option<SceneTarget>>>,
    /// A rename is being ended unsaved.
    dropping: Rc<Cell<bool>>,
}

/// A row being dragged.
#[derive(Debug, Clone)]
struct Drag {
    layer: LayerId,
    /// Where the press was, in the list's coordinates.
    start_y: f64,
    /// The press's offset inside its row.
    grab_y: f64,
    started: bool,
    slot: usize,
}

impl std::fmt::Debug for LayerList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerList")
            .field("rows", &self.inner.rows.borrow().len())
            .finish()
    }
}

impl LayerList {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let list = gtk::ListBox::new();
        list.add_css_class("qrec-list");
        list.add_css_class("layer-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.set_activate_on_single_click(true);
        list.update_property(&[gtk::accessible::Property::Label("Layers")]);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&list));
        let ghost = Ghost::new();
        overlay.add_overlay(&ghost.root);
        let line = drop_line();
        overlay.add_overlay(&line);
        let empty = empty_row("qrec-layers-symbolic", NO_LAYERS);
        root.append(&overlay);
        root.append(&empty);
        let layers = Self {
            inner: Rc::new(Inner {
                ctx: ctx.clone(),
                root,
                overlay,
                list,
                empty,
                ghost,
                line,
                rows: RefCell::new(Vec::new()),
                drag: RefCell::new(None),
            }),
        };
        let weak = Rc::downgrade(&layers.inner);
        layers
            .inner
            .list
            .connect_row_activated(guarded!("panel: a layer row", move |_, row| {
                if let Some(inner) = weak.upgrade() {
                    LayerList { inner }.activated(row);
                }
            }));
        layers
    }

    pub fn widget(&self) -> gtk::Widget {
        self.inner.root.clone().upcast()
    }

    /// Shows `rows` (topmost first) of `state`.
    pub fn update(&self, rows: &[LayerRow], state: &State) {
        let inner = &self.inner;
        let same = {
            let views = inner.rows.borrow();
            views.len() == rows.len() && views.iter().zip(rows).all(|(v, r)| v.layer == r.layer)
        };
        if !same {
            self.rebuild(rows);
        }
        let n = rows.len();
        for (view, row) in inner.rows.borrow().iter().zip(rows) {
            view.show(row, state, n);
        }
        inner.empty.set_visible(rows.is_empty());
        inner.overlay.set_visible(!rows.is_empty());
        // A drag whose layer went away is over.
        let gone = inner
            .drag
            .borrow()
            .as_ref()
            .is_some_and(|d| !rows.iter().any(|r| r.layer == d.layer));
        if gone {
            self.end_drag(false);
        }
    }

    /// Starts the inline rename of `layer` (the overflow's Rename).
    pub fn rename(&self, layer: &str) {
        let rows = self.inner.rows.borrow();
        let Some(view) = rows.iter().find(|v| v.layer.as_str() == layer) else {
            return;
        };
        let name = view.name.clone();
        // The field takes the row's height: the source line steps aside.
        view.source.set_visible(false);
        drop(rows);
        name.set_can_target(true);
        name.set_focusable(true);
        name.start_editing();
        name.grab_focus();
    }

    /// The row widgets, topmost first (screenshot checks).
    pub fn row_widgets(&self) -> Vec<gtk::Widget> {
        self.inner
            .rows
            .borrow()
            .iter()
            .map(|v| v.row.clone().upcast())
            .collect()
    }

    fn rebuild(&self, rows: &[LayerRow]) {
        let inner = &self.inner;
        let old: Vec<RowView> = inner.rows.borrow_mut().drain(..).collect();
        for view in old {
            // A rename on a row that goes saves nothing.
            view.drop_rename();
            inner.list.remove(&view.row);
        }
        let mut views = Vec::with_capacity(rows.len());
        for row in rows {
            let view = RowView::new(&inner.ctx, &row.layer);
            self.wire(&view);
            inner.list.append(&view.row);
            views.push(view);
        }
        *inner.rows.borrow_mut() = views;
    }

    fn wire(&self, view: &RowView) {
        let ctx = &self.inner.ctx;
        let layer = view.layer.clone();
        let store = ctx.store.clone();
        let weak = Rc::downgrade(&self.inner);
        let id = layer.clone();
        view.eye
            .connect_clicked(guarded!("panel: the eye", move |_| {
                let Some(inner) = weak.upgrade() else { return };
                let hidden = LayerList { inner }.model_of(&id).is_some_and(|m| m.hidden);
                if let Some(cmd) = panel::layer_set(
                    &store.state(),
                    &id,
                    LayerPatch {
                        visible: Some(hidden),
                        ..LayerPatch::default()
                    },
                ) {
                    store.dispatch(cmd);
                }
            }));
        let store = ctx.store.clone();
        let weak = Rc::downgrade(&self.inner);
        let id = layer.clone();
        view.lock
            .connect_clicked(guarded!("panel: the lock", move |_| {
                let Some(inner) = weak.upgrade() else { return };
                let locked = LayerList { inner }.model_of(&id).is_some_and(|m| m.locked);
                if let Some(cmd) = panel::layer_set(
                    &store.state(),
                    &id,
                    LayerPatch {
                        locked: Some(!locked),
                        ..LayerPatch::default()
                    },
                ) {
                    store.dispatch(cmd);
                }
            }));
        // A rename ends: save it (Esc already put the old name back; one
        // dropped because its scene went saves nothing).
        let store = ctx.store.clone();
        let id = layer.clone();
        let source = view.source.clone();
        let row = view.row.clone();
        let dropping = Rc::clone(&view.dropping);
        view.name.connect_notify_local(
            Some("editing"),
            guarded!("panel: rename", move |label, _| {
                if label.is_editing() {
                    return;
                }
                source.set_visible(true);
                // The keyboard comes back to the row (the name takes no
                // focus outside an edit).
                if label.has_focus() || label.focus_child().is_some() {
                    row.grab_focus();
                }
                label.set_can_target(false);
                label.set_focusable(false);
                if dropping.get() {
                    return;
                }
                if let Some(cmd) = panel::layer_rename(&store.state(), &id, &label.text()) {
                    store.dispatch(cmd);
                }
            }),
        );
        // Reorder drags.
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let weak = Rc::downgrade(&self.inner);
        let id = layer.clone();
        let row = view.row.clone();
        drag.connect_drag_begin(guarded!("panel: row drag", move |_, _x, y| {
            if let Some(inner) = weak.upgrade() {
                LayerList { inner }.press(&id, &row, y);
            }
        }));
        let weak = Rc::downgrade(&self.inner);
        drag.connect_drag_update(guarded!("panel: row drag", move |gesture, _dx, dy| {
            if let Some(inner) = weak.upgrade() {
                LayerList { inner }.moved(gesture, dy);
            }
        }));
        let weak = Rc::downgrade(&self.inner);
        drag.connect_drag_end(guarded!("panel: row drop", move |_, _dx, _dy| {
            if let Some(inner) = weak.upgrade() {
                LayerList { inner }.end_drag(true);
            }
        }));
        view.row.add_controller(drag);
    }

    fn model_of(&self, layer: &LayerId) -> Option<LayerRow> {
        self.inner
            .rows
            .borrow()
            .iter()
            .find(|v| &v.layer == layer)
            .and_then(|v| v.model.borrow().clone())
    }

    fn activated(&self, row: &gtk::ListBoxRow) {
        let layer = self
            .inner
            .rows
            .borrow()
            .iter()
            .find(|v| &v.row == row)
            .map(|v| v.layer.clone());
        if let Some(layer) = layer {
            self.inner.ctx.store.dispatch(panel::select_layer(&layer));
        }
    }

    fn press(&self, layer: &LayerId, row: &gtk::ListBoxRow, y: f64) {
        if !row.is_sensitive() {
            return;
        }
        let list = &self.inner.list;
        let at = row
            .compute_point(list, &graphene::Point::new(0.0, y as f32))
            .map_or(y, |p| f64::from(p.y()));
        *self.inner.drag.borrow_mut() = Some(Drag {
            layer: layer.clone(),
            start_y: at,
            grab_y: y,
            started: false,
            slot: 0,
        });
    }

    fn moved(&self, gesture: &gtk::GestureDrag, dy: f64) {
        let mut drag = self.inner.drag.borrow_mut();
        let Some(d) = drag.as_mut() else {
            return;
        };
        if !d.started {
            if dy.abs() < DRAG_THRESHOLD {
                return;
            }
            d.started = true;
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let layer = d.layer.clone();
            drop(drag);
            self.start_drag(&layer);
            drag = self.inner.drag.borrow_mut();
        }
        let Some(d) = drag.as_mut() else {
            return;
        };
        let y = d.start_y + dy;
        d.slot = self.slot_at(y);
        let grab_y = d.grab_y;
        let slot = d.slot;
        drop(drag);
        self.place(y - grab_y, slot);
    }

    /// The drag passed its threshold: the row lifts (D-42's keys apply).
    fn start_drag(&self, layer: &LayerId) {
        let inner = &self.inner;
        let model = self.model_of(layer);
        if let Some(model) = &model {
            inner.ghost.show(model, &inner.ctx.store.state());
        }
        for view in inner.rows.borrow().iter() {
            if &view.layer == layer {
                view.row.add_css_class("drag-source");
            }
        }
        inner.ghost.root.set_visible(true);
        inner.line.set_visible(true);
        inner
            .ctx
            .store
            .update_local(|l| l.drag = Some(DragKind::LayerReorder));
        let weak: Weak<Inner> = Rc::downgrade(inner);
        inner.ctx.keys.set_drag_cancel(Some(Rc::new(move || {
            if let Some(inner) = weak.upgrade() {
                LayerList { inner }.end_drag(false);
            }
        })));
    }

    /// The gap under list y `y` (0 … n).
    fn slot_at(&self, y: f64) -> usize {
        let list = &self.inner.list;
        self.inner
            .rows
            .borrow()
            .iter()
            .filter(|v| {
                v.row
                    .compute_bounds(list)
                    .is_some_and(|b| f64::from(b.y() + b.height() / 2.0) < y)
            })
            .count()
    }

    /// The list y of gap `slot`'s boundary.
    fn slot_y(&self, slot: usize) -> f64 {
        let list = &self.inner.list;
        let rows = self.inner.rows.borrow();
        let bounds = |i: usize| rows.get(i).and_then(|v| v.row.compute_bounds(list));
        match slot {
            0 => bounds(0).map_or(0.0, |b| f64::from(b.y())),
            s => bounds(s - 1).map_or(0.0, |b| f64::from(b.y() + b.height())),
        }
    }

    fn place(&self, ghost_y: f64, slot: usize) {
        let inner = &self.inner;
        let height = f64::from(inner.list.height());
        let ghost_h = f64::from(inner.ghost.root.height().max(44));
        let top = ghost_y.clamp(0.0, (height - ghost_h).max(0.0));
        inner.ghost.root.set_margin_top(top.round() as i32);
        let line_h = f64::from(inner.line.height().max(8));
        let line = (self.slot_y(slot) - line_h / 2.0).clamp(0.0, (height - line_h).max(0.0));
        inner.line.set_margin_top(line.round() as i32);
    }

    /// The drag ends: `drop` sends its `LayerOrder`; otherwise (`Esc`, the
    /// layer gone) nothing is sent.
    fn end_drag(&self, drop: bool) {
        let inner = &self.inner;
        let Some(drag) = inner.drag.borrow_mut().take() else {
            return;
        };
        if !drag.started {
            return;
        }
        inner.ghost.root.set_visible(false);
        inner.line.set_visible(false);
        for view in inner.rows.borrow().iter() {
            view.row.remove_css_class("drag-source");
        }
        inner.ctx.keys.set_drag_cancel(None);
        inner.ctx.store.update_local(|l| {
            if l.drag == Some(DragKind::LayerReorder) {
                l.drag = None;
            }
        });
        if drop && let Some(cmd) = panel::reorder(&inner.ctx.store.state(), &drag.layer, drag.slot)
        {
            inner.ctx.store.dispatch(cmd);
        }
    }
}

impl Ghost {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("layer-row");
        root.add_css_class("ghost");
        root.add_css_class("e1");
        root.set_valign(gtk::Align::Start);
        root.set_can_target(false);
        root.set_visible(false);
        let grip = gtk::Image::from_icon_name("list-drag-handle-symbolic");
        grip.add_css_class("row-slot");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.add_css_class("row-slot");
        let icon = gtk::Image::new();
        icon.add_css_class("row-slot");
        let (texts, name, source) = texts();
        root.append(&grip);
        root.append(&spacer);
        root.append(&icon);
        root.append(&texts);
        Self {
            root,
            icon,
            name,
            source,
        }
    }

    fn show(&self, row: &LayerRow, state: &State) {
        self.name.set_text(&row.name);
        self.source.set_text(&row.source);
        self.icon
            .set_icon_name(Some(source_icon(state, &row.layer)));
    }
}

/// The 2 px drop line with its ring at the start (S13b).
fn drop_line() -> gtk::Box {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.add_css_class("drop-line");
    line.set_valign(gtk::Align::Start);
    line.set_can_target(false);
    line.set_visible(false);
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("drop-dot");
    dot.set_valign(gtk::Align::Center);
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar.add_css_class("drop-bar");
    bar.set_hexpand(true);
    bar.set_valign(gtk::Align::Center);
    line.append(&dot);
    line.append(&bar);
    line
}

/// The empty state of a section (S01a): an icon and a `caption` `muted`.
pub fn empty_row(icon: &str, text: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("empty-row");
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class("muted");
    let label = gtk::Label::new(Some(text));
    label.add_css_class("t-caption");
    label.add_css_class("muted");
    row.append(&image);
    row.append(&label);
    row.set_visible(false);
    row
}

/// The name over the source line.
fn texts() -> (gtk::Box, gtk::Label, gtk::Label) {
    let texts = gtk::Box::new(gtk::Orientation::Vertical, 0);
    texts.set_valign(gtk::Align::Center);
    texts.set_hexpand(true);
    let name = gtk::Label::new(None);
    name.add_css_class("t-body");
    name.add_css_class("row-name");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let source = gtk::Label::new(None);
    source.add_css_class("t-caption");
    source.add_css_class("muted");
    source.add_css_class("row-source");
    source.set_xalign(0.0);
    source.set_ellipsize(gtk::pango::EllipsizeMode::End);
    texts.append(&name);
    texts.append(&source);
    (texts, name, source)
}

/// The icon of `layer`'s source in the scene the canvas shows.
fn source_icon(state: &State, layer: &LayerId) -> &'static str {
    let scene = qrec_core::ui_normalize::canvas_scene(&state.project, &state.ui);
    match scene.layer(layer).map(|l| &l.source) {
        Some(VideoSource::Camera { .. }) => "camera-web-symbolic",
        _ => "video-display-symbolic",
    }
}

fn is_screen(state: &State, layer: &LayerId) -> bool {
    let scene = qrec_core::ui_normalize::canvas_scene(&state.project, &state.ui);
    matches!(
        scene.layer(layer).map(|l| &l.source),
        Some(VideoSource::Screen { .. })
    )
}

/// An inline-renamable name (C09, C10): as wide as its text, ellipsized
/// when the row is narrower (GTK's text field would ask for 150 px).
pub fn editable_name(class: &str) -> gtk::EditableLabel {
    let name = gtk::EditableLabel::new("");
    name.add_css_class(class);
    name.add_css_class("row-name");
    name.set_width_chars(1);
    name.set_can_target(false);
    name.set_focusable(false);
    let mut child = name.first_child();
    while let Some(widget) = child {
        let mut inner = widget.first_child();
        while let Some(w) = inner {
            if let Some(label) = w.downcast_ref::<gtk::Label>() {
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            }
            if let Some(text) = w.downcast_ref::<gtk::Text>() {
                text.set_width_chars(1);
                text.set_max_width_chars(1);
            }
            inner = w.next_sibling();
        }
        child = widget.next_sibling();
    }
    name
}

/// A 24 px icon button of a row.
fn row_button(icon: &str) -> (gtk::Button, gtk::Image) {
    let image = gtk::Image::from_icon_name(icon);
    let button = gtk::Button::new();
    button.set_child(Some(&image));
    button.add_css_class("qrec-btn");
    button.add_css_class("flat");
    button.add_css_class("icon-24");
    button.set_focus_on_click(false);
    button.set_valign(gtk::Align::Center);
    (button, image)
}

impl RowView {
    fn new(ctx: &Ctx, layer: &LayerId) -> Self {
        let row = gtk::ListBoxRow::new();
        row.add_css_class("layer-row");
        row.add_css_class("r1");
        row.add_css_class("row-44");
        row.set_widget_name(&format!("layer-row-{layer}"));
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let grip = gtk::Image::from_icon_name("list-drag-handle-symbolic");
        grip.add_css_class("row-slot");
        grip.add_css_class("reveal");
        grip.add_css_class("muted");
        let (eye, _) = row_button("view-reveal-symbolic");
        eye.add_css_class("row-slot");
        let icon = gtk::Image::new();
        icon.add_css_class("row-slot");
        icon.add_css_class("source-icon");
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 0);
        texts.set_valign(gtk::Align::Center);
        texts.set_hexpand(true);
        texts.add_css_class("row-texts");
        let name = editable_name("t-body");
        name.set_hexpand(true);
        let source = gtk::Label::new(None);
        source.add_css_class("t-caption");
        source.add_css_class("muted");
        source.add_css_class("row-source");
        source.set_xalign(0.0);
        source.set_ellipsize(gtk::pango::EllipsizeMode::End);
        texts.append(&name);
        texts.append(&source);
        let (lock, _) = row_button("changes-allow-symbolic");
        lock.add_css_class("lock");
        eye.add_css_class("eye");
        let menu = Menu::new();
        let menu_button = menus::menu_button(&menu, &["flat", "icon-24"], "More");
        menu_button.set_icon_name("view-more-symbolic");
        menu_button.set_valign(gtk::Align::Center);
        menu_button.set_widget_name(&format!("layer-menu-{layer}"));
        menus::track_menu(ctx, &menu_button, None);
        content.append(&grip);
        content.append(&eye);
        content.append(&icon);
        content.append(&texts);
        content.append(&lock);
        content.append(&menu_button);
        row.set_child(Some(&content));
        Self {
            layer: layer.clone(),
            row,
            grip,
            eye,
            icon,
            name,
            source,
            lock,
            menu_button,
            menu,
            model: RefCell::new(None),
            edits: RefCell::new(None),
            dropping: Rc::new(Cell::new(false)),
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

    fn show(&self, row: &LayerRow, state: &State, rows: usize) {
        // Another scene: a rename in progress was for the one that went.
        let target = qrec_core::ui_normalize::edit_target(&state.project, &state.ui);
        let scene_changed = self.edits.borrow().as_ref() != Some(&target);
        *self.edits.borrow_mut() = Some(target);
        if scene_changed {
            self.drop_rename();
        }
        if self.model.borrow().as_ref() == Some(row) {
            self.update_menu(row, state, rows);
            return;
        }
        *self.model.borrow_mut() = Some(row.clone());
        if !self.name.is_editing() && self.name.text() != row.name {
            self.name.set_text(&row.name);
        }
        let (subtitle, icon, icon_class) = match (&row.error, &row.missing) {
            (Some(error), _) => (
                error.as_str(),
                if is_screen(state, &row.layer) {
                    "qrec-monitor-x-symbolic"
                } else {
                    "dialog-error-symbolic"
                },
                Some("record-text"),
            ),
            (None, Some(missing)) => (
                missing.as_str(),
                "dialog-warning-symbolic",
                Some("warning-text"),
            ),
            (None, None) => (row.source.as_str(), source_icon(state, &row.layer), None),
        };
        self.source.set_text(subtitle);
        self.icon.set_icon_name(Some(icon));
        for class in ["record-text", "warning-text"] {
            self.icon.remove_css_class(class);
        }
        if let Some(class) = icon_class {
            self.icon.add_css_class(class);
        }
        // The message of an error or a missing source reads in `text`.
        if icon_class.is_some() {
            self.source.remove_css_class("muted");
            self.row.add_css_class("problem");
        } else {
            self.source.add_css_class("muted");
            self.row.remove_css_class("problem");
        }
        let set = |w: &gtk::Widget, class: &str, on: bool| {
            if on {
                w.add_css_class(class);
            } else {
                w.remove_css_class(class);
            }
        };
        set(self.row.upcast_ref(), "selected", row.selected);
        set(self.row.upcast_ref(), "hidden", row.hidden);
        set(self.row.upcast_ref(), "locked", row.locked);
        if let Some(image) = self.eye.child().and_downcast::<gtk::Image>() {
            image.set_icon_name(Some(if row.hidden {
                "view-conceal-symbolic"
            } else {
                "view-reveal-symbolic"
            }));
        }
        if let Some(image) = self.lock.child().and_downcast::<gtk::Image>() {
            image.set_icon_name(Some(if row.locked {
                "changes-prevent-symbolic"
            } else {
                "changes-allow-symbolic"
            }));
        }
        set(self.eye.upcast_ref(), "reveal", !row.hidden);
        set(self.lock.upcast_ref(), "reveal", !row.locked);
        let eye_label = format!(
            "{} layer {}",
            if row.hidden { "Show" } else { "Hide" },
            row.name
        );
        self.eye
            .update_property(&[gtk::accessible::Property::Label(&eye_label)]);
        self.eye.set_tooltip_text(Some(&eye_label));
        let lock_label = format!(
            "{} layer {}",
            if row.locked { "Unlock" } else { "Lock" },
            row.name
        );
        self.lock
            .update_property(&[gtk::accessible::Property::Label(&lock_label)]);
        self.lock.set_tooltip_text(Some(&lock_label));
        menus::set_menu_label(&self.menu_button, &format!("More for layer {}", row.name));
        self.row
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "{}, {}",
                row.name, subtitle
            ))]);
        self.row.set_sensitive(row.enabled);
        let _ = &self.grip;
        self.update_menu(row, state, rows);
    }

    /// The overflow: Rename, Move up, Move down, Remove.
    fn update_menu(&self, row: &LayerRow, state: &State, _rows: usize) {
        let up = row
            .enabled
            .then(|| panel::move_up(state, &row.layer))
            .flatten();
        let down = row
            .enabled
            .then(|| panel::move_down(state, &row.layer))
            .flatten();
        let remove = row
            .enabled
            .then(|| panel::layer_remove(state, &row.layer))
            .flatten();
        let signature = format!(
            "{}|{:?}|{:?}|{:?}|{}",
            row.enabled,
            up.as_ref().map(|c| c.to_json()),
            down.as_ref().map(|c| c.to_json()),
            remove.as_ref().map(|c| c.to_json()),
            row.layer
        );
        let id = row.layer.to_string();
        let enabled = row.enabled;
        self.menu.update(signature, |menu| {
            menu.append_item(&menus::ui_item("Rename", "rename-layer", &id, enabled));
            menu.append_item(&menus::command_item("Move up", up.as_ref()));
            menu.append_item(&menus::command_item("Move down", down.as_ref()));
            menu.append_item(&menus::command_item("Remove", remove.as_ref()));
        });
    }
}
