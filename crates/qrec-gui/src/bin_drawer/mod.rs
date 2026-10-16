//! The bin drawer (S14, S18, C21): the takes not on the timeline, their
//! rows and actions, multi-select (D-37), the drag source (D-38), the bin
//! preview (D-17). Owner: WP7 (`src/bin/` would be a Cargo binary target).
//! Registers `bin.preview`.
//!
//! - `region-bin`, 352 px (× the UI scale) at the window's left, pushing
//!   the stage, shown while `ui.binOpen` (it slides: [`slide`]). A 48 px
//!   header ("Bin", "5 takes · 436.4 MB", close `[B]`), the rows
//!   (`view::bin::bin_rows`, newest first), and with two or more takes
//!   selected the footer "2 takes selected · 184.1 MB · Purge from disk"
//!   instead of any row's actions (S18).
//! - Sizes come from `media::take_sizes` on `qrec-gui-io` (it blocks):
//!   when the bin opens, and 500 ms after its takes change (the debounce
//!   holds `MediaHub::hold_io_busy`, so `waitIdle` waits for it).
//! - Keys while the bin has the focus (D-11, the router's step 6): `↑` `↓`
//!   select (± `Shift` extends), `Enter` inserts at the end, `Space`
//!   toggles the preview. A change of the takes keeps the rows of the
//!   takes still listed; when the focused row leaves (`Enter` inserted it),
//!   the row now at its place takes the focus (and the selection, D-37).
//! - The bin preview (D-17, GUI-local): `LocalUi.take_preview` after
//!   `SetCanvasMode{Program}` when Live (the Program picture shows it, with
//!   the TAKE badge; WP4's sync plays it and clears `playing` at its end);
//!   `Esc`, closing the bin or any timeline action ends it
//!   ([`select::preview_ends`]).
//! - Rows of ok takes drag `qrec-take:<id>` onto the timeline (WP3's drop
//!   target makes it `ClipAdd{take, at: Index(slot)}`); `LocalUi.drag` is
//!   `BinInsert` while the drag lasts.

mod row;
pub mod select;
mod slide;
mod thumb;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Local, Offset, TimeZone, Utc};
use gtk::prelude::*;
use gtk::{gdk, glib};
use qrec_core::protocol::UiAction;
use qrec_core::protocol::command::InsertAt;
use qrec_core::state::{CanvasMode, State};
use qrec_core::view::bin::{self as view, BinAction};
use qrec_core::view::{DragKind, enabled};
use qrec_core::{Command, TakeId};

use crate::media::io::BusyToken;
use crate::store::local::TakePreview;
use crate::store::{Change, Subscription};
use crate::widgets::button;
use crate::{Ctx, desktop, guard, guarded};
use row::{RowEvent, RowView, Sink};
use select::Selection;
use slide::Slide;

/// The drawer's width, logical px (BRIEF §2.8).
pub const WIDTH: f64 = 352.0;
/// The sizes' debounce after the bin's takes change (G§3.6).
pub const SIZES_DEBOUNCE: Duration = Duration::from_millis(500);

/// The bin drawer region (`region-bin`, 352 px).
pub struct BinDrawer {
    root: Slide,
    /// The drawer itself: its subscriptions hold it weakly, the window
    /// keeps the region (and so this) for its life.
    _drawer: Rc<Drawer>,
}

impl BinDrawer {
    pub fn new(ctx: &Ctx) -> Self {
        let drawer = Drawer::build(ctx);
        Self {
            root: drawer.slide.clone(),
            _drawer: drawer,
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

struct Drawer {
    ctx: Ctx,
    slide: Slide,
    count: gtk::Label,
    /// Its close button (`bin-close`): the focus goes there when the row
    /// that had it was the last.
    close: gtk::Button,
    list: gtk::Box,
    footer: gtk::Box,
    footer_selected: gtk::Label,
    footer_size: gtk::Label,
    footer_purge: gtk::Button,
    rows: RefCell<Vec<RowView>>,
    sizes: RefCell<BTreeMap<TakeId, u64>>,
    /// Takes whose size was asked for (one that can't be known isn't asked
    /// again).
    asked: RefCell<BTreeSet<TakeId>>,
    selection: RefCell<Selection>,
    /// The sizes' debounce, and the token that makes `waitIdle` wait.
    debounce: RefCell<Option<(glib::SourceId, BusyToken)>>,
    thumbs: RefCell<Option<Subscription>>,
    /// The texture cache exists (the Session is open).
    thumbs_ready: std::cell::Cell<bool>,
    sink: RefCell<Option<Sink>>,
}

thread_local! {
    /// Each bin row's main area made so far (the widget its gestures and
    /// drag source hang on), weakly; test mode only: what [`rows_alive`]
    /// counts.
    static ROWS: RefCell<Vec<glib::WeakRef<gtk::Box>>> = const { RefCell::new(Vec::new()) };
}

/// Notes a new row's main area for [`rows_alive`] (test mode only).
fn track_row(main: &gtk::Box) {
    if crate::testctl::mode().on {
        ROWS.with(|rows| rows.borrow_mut().push(main.downgrade()));
    }
}

/// How many bin rows are alive (test mode; `testctl`'s `local`,
/// `binRowsAlive`): the rows shown, once those a rebuild dropped are gone.
/// A row a callback holds strongly would outlive its rebuild (G§3.12).
pub fn rows_alive() -> usize {
    ROWS.with(|rows| {
        let mut rows = rows.borrow_mut();
        rows.retain(|row| row.upgrade().is_some());
        rows.len()
    })
}

/// The UTC offset of the local zone at `at`.
fn offset_at(at: DateTime<Utc>) -> i32 {
    Local
        .offset_from_utc_datetime(&at.naive_utc())
        .fix()
        .local_minus_utc()
}

impl Drawer {
    fn build(ctx: &Ctx) -> Rc<Self> {
        let s = ctx.theme.scale();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.add_css_class("qrec-bin");
        content.set_widget_name("bin");
        let width = (WIDTH * s).round() as i32;
        content.set_size_request(width, -1);
        content.update_property(&[gtk::accessible::Property::Label("Bin")]);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("bin-header");
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_hexpand(true);
        titles.set_valign(gtk::Align::Center);
        let title = gtk::Label::new(Some("Bin"));
        title.add_css_class("t-title");
        title.set_xalign(0.0);
        titles.append(&title);
        let count = gtk::Label::new(None);
        count.add_css_class("t-caption");
        count.add_css_class("muted");
        count.set_xalign(0.0);
        count.set_widget_name("bin-count");
        titles.append(&count);
        header.append(&titles);
        let keymap = ctx.store.keymap();
        let close = button::icon(
            "window-close-symbolic",
            "Close bin",
            Some("bin.toggle"),
            &keymap,
        );
        close.set_widget_name("bin-close");
        close.set_valign(gtk::Align::Center);
        let keys = ctx.keys.clone();
        close.connect_clicked(guarded!("bin: close", move |_| {
            keys.run_binding("bin.toggle")
        }));
        header.append(&close);
        content.append(&header);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("bin-list");
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&list));
        content.append(&scroller);

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        footer.add_css_class("bin-footer");
        footer.set_widget_name("bin-footer");
        // "2 takes selected": 13/600, as S18's frame draws it (it was
        // `body-strong` 14 until the WP7 integration audit).
        let footer_selected = gtk::Label::new(None);
        footer_selected.set_widget_name("bin-footer-selected");
        footer_selected.add_css_class("t-body-sm");
        footer_selected.add_css_class("bin-footer-selected");
        footer_selected.set_valign(gtk::Align::Center);
        footer.append(&footer_selected);
        let footer_size = gtk::Label::new(None);
        footer_size.add_css_class("t-mono-sm");
        footer_size.add_css_class("muted");
        footer_size.set_valign(gtk::Align::Center);
        footer_size.set_hexpand(true);
        footer_size.set_xalign(0.0);
        footer.append(&footer_size);
        let footer_purge = gtk::Button::new();
        footer_purge.add_css_class("qrec-btn");
        footer_purge.add_css_class("destructive-flat");
        footer_purge.set_focus_on_click(false);
        footer_purge.set_widget_name("bin-purge-selected");
        // 8 px apart by CSS (`button.qrec-btn > box`).
        let purge_content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        purge_content.append(&gtk::Image::from_icon_name("user-trash-symbolic"));
        purge_content.append(&gtk::Label::new(Some("Purge from disk")));
        footer_purge.set_child(Some(&purge_content));
        footer_purge.update_property(&[gtk::accessible::Property::Label("Purge from disk")]);
        footer_purge.set_valign(gtk::Align::Center);
        footer.append(&footer_purge);
        footer.set_visible(false);
        content.append(&footer);

        let slide = Slide::new(&content, width);
        slide.set_widget_name("region-bin");
        let drawer = Rc::new(Self {
            ctx: ctx.clone(),
            slide,
            count,
            close,
            list,
            footer,
            footer_selected,
            footer_size,
            footer_purge,
            rows: RefCell::new(Vec::new()),
            sizes: RefCell::new(BTreeMap::new()),
            asked: RefCell::new(BTreeSet::new()),
            selection: RefCell::new(Selection::default()),
            debounce: RefCell::new(None),
            thumbs: RefCell::new(None),
            thumbs_ready: std::cell::Cell::new(false),
            sink: RefCell::new(None),
        });
        let weak = Rc::downgrade(&drawer);
        let sink: Sink = Rc::new(move |take: &TakeId, event| {
            if let Some(drawer) = weak.upgrade() {
                drawer.on_row(take, event);
            }
        });
        *drawer.sink.borrow_mut() = Some(sink);
        drawer.connect(&content);
        // Open at startup without animating ("nothing animates on app
        // start").
        let open = ctx.store.state().ui.bin_open;
        drawer.slide.show(open, false, ctx.theme.motion().reduced);
        drawer.refresh(&ctx.store.state());
        if open {
            drawer.want_sizes(false);
        }
        let weak = Rc::downgrade(&drawer);
        ctx.store
            .subscribe(move |change| {
                if let Some(drawer) = weak.upgrade() {
                    drawer.on_change(change);
                }
            })
            .detach();
        // The Session opens after the first snapshot: the thumbnails ask
        // the texture cache once it exists.
        let weak = Rc::downgrade(&drawer);
        ctx.store
            .subscribe_local(move |local| {
                if let Some(drawer) = weak.upgrade()
                    && local.is_open()
                    && !drawer.thumbs_ready.get()
                    && drawer.ctx.media.thumbs().is_some()
                {
                    drawer.thumbs_ready.set(true);
                    for row in drawer.rows.borrow().iter() {
                        row.thumb.queue_draw();
                    }
                }
            })
            .detach();
        // A thumbnail redraws when its file's images arrive (the texture
        // cache took them first).
        let weak = Rc::downgrade(&drawer);
        let sub = ctx.media.subscribe_preview(move |data| {
            if let Some(drawer) = weak.upgrade() {
                for row in drawer.rows.borrow().iter() {
                    if row.thumb.file().as_deref() == Some(data.file()) {
                        row.thumb.queue_draw();
                    }
                }
            }
        });
        *drawer.thumbs.borrow_mut() = Some(sub);
        let weak = Rc::downgrade(&drawer);
        ctx.theme
            .subscribe(move |theme| {
                if let Some(drawer) = weak.upgrade() {
                    let width = (WIDTH * theme.scale()).round() as i32;
                    drawer.slide.set_width(width);
                    if let Some(content) = drawer.slide.first_child() {
                        content.set_size_request(width, -1);
                    }
                }
            })
            .detach();
        let weak = Rc::downgrade(&drawer);
        ctx.actions.register(
            "bin.preview",
            Rc::new(move || {
                if let Some(drawer) = weak.upgrade() {
                    drawer.preview_action();
                }
            }),
        );
        drawer
    }

    fn connect(self: &Rc<Self>, content: &gtk::Box) {
        // The bin list's keys (D-11): the router lets them through while
        // the bin has the focus; a focused button takes Space and Enter
        // first.
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(guarded!(
            "bin: keys",
            after_panic = glib::Propagation::Proceed,
            move |_, keyval, _code, state| {
                let Some(drawer) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if drawer.key(keyval, state) {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        content.add_controller(keys);
        let weak = Rc::downgrade(self);
        self.footer_purge
            .connect_clicked(guarded!("bin: purge the selection", move |_| {
                let Some(drawer) = weak.upgrade() else { return };
                let selected = drawer.selection.borrow().selected.clone();
                let sizes = drawer.sizes.borrow().clone();
                if let Some(footer) = view::footer_view(&selected, &sizes) {
                    drawer.ctx.store.dispatch(footer.command);
                }
            }));
    }

    // ------------------------------------------------------- following

    fn on_change(self: &Rc<Self>, change: &Change) {
        let (prev, next) = (&change.prev, &change.next);
        if prev.ui.bin_open != next.ui.bin_open {
            self.slide
                .show(next.ui.bin_open, true, self.ctx.theme.motion().reduced);
            if next.ui.bin_open {
                self.want_sizes(false);
            }
        }
        if change.takes() || change.timeline() || prev.project.canvas != next.project.canvas {
            // Purged takes free their sizes.
            self.sizes
                .borrow_mut()
                .retain(|take, _| next.project.takes.contains_key(take));
            if next.ui.bin_open {
                self.want_sizes(true);
            }
        }
        self.refresh(next);
        let preview = self.ctx.store.local().take_preview.clone();
        if let Some(preview) = preview {
            let in_bin = view::bin_takes(next).contains(&preview.take);
            if select::preview_ends(prev, next, &preview.take, in_bin) {
                self.end_preview();
            }
        }
    }

    /// The rows, header, selection and footer of `state`.
    fn refresh(self: &Rc<Self>, state: &State) {
        let sizes = self.sizes.borrow().clone();
        let offsets: BTreeMap<TakeId, i32> = state
            .project
            .takes
            .iter()
            .map(|(id, take)| (id.clone(), offset_at(take.recorded_at)))
            .collect();
        let rows = view::bin_rows(state, &sizes, &offsets, offset_at(Utc::now()));
        self.count.set_text(&view::header(state, &sizes));
        let before = self.row_takes();
        let focused = self.focused_row();
        self.place_rows(&rows);
        let takes: Vec<TakeId> = rows.iter().map(|r| r.take.clone()).collect();
        // The row with the keyboard focus left the bin (Enter inserted its
        // take): the keyboard goes on from the row now at its place.
        let left = focused.filter(|take| !takes.contains(take));
        let selection = self.selection.borrow().clone();
        self.selection
            .borrow_mut()
            .retain(&before, &takes, left.is_some());
        if *self.selection.borrow() != selection {
            self.publish_selection();
        }
        if let Some(left) = left {
            // At once, before GTK moves the focus out of the bin (the stage,
            // a broken take's Open log): the bin keys need it (D-11).
            let main = select::place(&before, &takes, &left).and_then(|take| {
                self.rows
                    .borrow()
                    .iter()
                    .find(|r| r.take == take)
                    .map(|r| r.main.clone())
            });
            match main {
                Some(main) => main.grab_focus(),
                None => self.close.grab_focus(),
            };
        }
        let idle = state.runtime.record.is_idle();
        for (view, row) in self.rows.borrow().iter().zip(&rows) {
            let insert = enabled(state, &view::insert_at(&row.take, InsertAt::End));
            view.set_enabled(insert, idle);
        }
        // The footer's Purge from disk follows the same gate as the rows'
        // (D-43: it depends on the state, not on which takes).
        let selected = self.selection.borrow().selected.clone();
        let purge = enabled(state, &Command::TakePurge { takes: selected }).err();
        self.footer_purge.set_sensitive(purge.is_none());
        self.footer_purge.set_tooltip_text(purge.as_deref());
        // The texture cache comes with the open Session: the thumbnails
        // drawn before it ask again (a draw asks for its images).
        if !self.thumbs_ready.get() && self.ctx.media.thumbs().is_some() {
            self.thumbs_ready.set(true);
            for row in self.rows.borrow().iter() {
                row.thumb.queue_draw();
            }
        }
        self.show_selection();
    }

    /// Shows `rows`: the rows of takes still listed stay (moved into the new
    /// order, their focus and state kept), new takes get rows, the others
    /// go. (Every change of the takes made every row again: the focused one
    /// went with them.)
    fn place_rows(self: &Rc<Self>, rows: &[view::BinRow]) {
        let sink = self.sink.borrow().clone();
        let Some(sink) = sink else { return };
        let mut old: BTreeMap<TakeId, RowView> = self
            .rows
            .take()
            .into_iter()
            .map(|v| (v.take.clone(), v))
            .collect();
        // A row whose take became draggable (or stopped) is made again: its
        // drag source comes with it.
        old.retain(|take, v| {
            let keep = rows.iter().any(|r| {
                r.take == *take
                    && v.row()
                        .is_some_and(|shown| shown.insertable == r.insertable)
            });
            if !keep {
                self.list.remove(&v.root);
            }
            keep
        });
        let mut views = Vec::with_capacity(rows.len());
        let mut prev: Option<gtk::Widget> = None;
        for row in rows {
            let view = match old.remove(&row.take) {
                Some(view) => {
                    view.show(row);
                    self.list.reorder_child_after(&view.root, prev.as_ref());
                    view
                }
                None => {
                    let view = RowView::new(&self.ctx, row, &sink);
                    track_row(&view.main);
                    self.list.insert_child_after(&view.root, prev.as_ref());
                    view
                }
            };
            prev = Some(view.root.clone().upcast());
            views.push(view);
        }
        *self.rows.borrow_mut() = views;
    }

    /// The take whose row has the keyboard focus, if one does.
    fn focused_row(&self) -> Option<TakeId> {
        let focus = self.list.root().and_then(|root| root.focus())?;
        self.rows
            .borrow()
            .iter()
            .find(|r| focus.is_ancestor(&r.root))
            .map(|r| r.take.clone())
    }

    /// The selection's look: R1 rows, the single row's actions, the footer.
    fn show_selection(&self) {
        let selection = self.selection.borrow();
        let single = selection.single().cloned();
        for row in self.rows.borrow().iter() {
            let selected = selection.selected.contains(&row.take);
            row.set_selected(selected, single.as_ref() == Some(&row.take));
        }
        let sizes = self.sizes.borrow();
        match view::footer_view(&selection.selected, &sizes) {
            Some(footer) => {
                self.footer_selected.set_text(&footer.selected);
                self.footer_size
                    .set_text(footer.size.as_deref().unwrap_or_default());
                self.footer.set_visible(true);
            }
            None => self.footer.set_visible(false),
        }
    }

    /// `LocalUi.bin_selection` follows the drawer's (D-37).
    fn publish_selection(&self) {
        let selected = self.selection.borrow().selected.clone();
        self.ctx
            .store
            .update_local(move |l| l.bin_selection = selected);
    }

    /// The rows' takes, in order.
    fn row_takes(&self) -> Vec<TakeId> {
        self.rows.borrow().iter().map(|r| r.take.clone()).collect()
    }

    // ------------------------------------------------------- sizes

    /// Asks `qrec-gui-io` for the sizes not known yet; `debounce`: 500 ms
    /// from now (holding an io token, so `waitIdle` waits for them).
    fn want_sizes(self: &Rc<Self>, debounce: bool) {
        if !debounce {
            if let Some((id, _token)) = self.debounce.borrow_mut().take() {
                id.remove();
            }
            self.fetch_sizes();
            return;
        }
        if self.debounce.borrow().is_some() {
            return;
        }
        let token = self.ctx.media.hold_io_busy();
        let weak = Rc::downgrade(self);
        let id = guard::timeout_local_once(SIZES_DEBOUNCE, "bin: the sizes' debounce", move || {
            if let Some(drawer) = weak.upgrade() {
                // The job counts as pending io before the token goes.
                let held = drawer.debounce.borrow_mut().take();
                drawer.fetch_sizes();
                drop(held);
            }
        });
        *self.debounce.borrow_mut() = Some((id, token));
    }

    fn fetch_sizes(self: &Rc<Self>) {
        let state = self.ctx.store.state();
        let missing: Vec<TakeId> = {
            let (sizes, asked) = (self.sizes.borrow(), self.asked.borrow());
            view::bin_takes(&state)
                .into_iter()
                .filter(|t| !sizes.contains_key(t) && !asked.contains(t))
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        self.asked.borrow_mut().extend(missing.iter().cloned());
        let dir: PathBuf = self.ctx.store.project_dir().to_path_buf();
        let weak = Rc::downgrade(self);
        self.ctx.media.run_io_then(
            move || qrec_shell::media::take_sizes(&dir, &missing),
            move |found| {
                let Some(drawer) = weak.upgrade() else { return };
                drawer.sizes.borrow_mut().extend(found);
                let state = drawer.ctx.store.state();
                drawer.refresh(&state);
            },
        );
    }

    // ------------------------------------------------------- rows

    fn on_row(self: &Rc<Self>, take: &TakeId, event: RowEvent) {
        match event {
            RowEvent::Select { ctrl, shift } => {
                let takes = self.row_takes();
                self.selection.borrow_mut().click(&takes, take, ctrl, shift);
                self.publish_selection();
                self.show_selection();
            }
            RowEvent::Preview => self.start_preview(take),
            RowEvent::InsertEnd => self
                .ctx
                .store
                .dispatch(view::insert_at(take, InsertAt::End)),
            RowEvent::InsertAfter => self
                .ctx
                .store
                .dispatch(view::insert_at(take, InsertAt::AfterSelection)),
            RowEvent::Purge => self.ctx.store.dispatch(Command::TakePurge {
                takes: vec![take.clone()],
            }),
            RowEvent::Reveal | RowEvent::OpenLog => {
                let row = self
                    .rows
                    .borrow()
                    .iter()
                    .find(|r| r.take == *take)
                    .and_then(|r| r.row());
                let action = row.and_then(|row| {
                    row.actions.into_iter().find_map(|a| match (a, &event) {
                        (BinAction::Reveal { action }, RowEvent::Reveal) => Some(action),
                        (BinAction::OpenLog { action }, RowEvent::OpenLog) => Some(action),
                        _ => None,
                    })
                });
                if let Some(action) = action {
                    // `desktop::run` opens project-relative paths under the
                    // project folder.
                    self.desktop(&action);
                }
            }
            RowEvent::DragBegin => {
                let keys = self.ctx.keys.clone();
                let store = self.ctx.store.clone();
                // `Esc` mid-drag (D-42): GTK cancels the drag itself; the
                // router only forgets it.
                keys.set_drag_cancel(Some(Rc::new(move || {
                    store.update_local(|l| {
                        if l.drag == Some(DragKind::BinInsert) {
                            l.drag = None;
                        }
                    });
                })));
                self.ctx.store.update_local(|l| {
                    if l.drag.is_none() {
                        l.drag = Some(DragKind::BinInsert);
                    }
                });
            }
            RowEvent::DragEnd => {
                self.ctx.keys.set_drag_cancel(None);
                self.ctx.store.update_local(|l| {
                    if l.drag == Some(DragKind::BinInsert) {
                        l.drag = None;
                    }
                });
            }
        }
    }

    fn desktop(&self, action: &UiAction) {
        let window = self.slide.root().and_downcast::<gtk::Window>();
        desktop::run(
            &self.ctx,
            window.as_ref(),
            self.ctx.store.project_dir(),
            action,
        );
    }

    /// The list's keys (D-11, D-37); `true` when used.
    fn key(self: &Rc<Self>, keyval: gdk::Key, state: gdk::ModifierType) -> bool {
        let plain = !state.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK,
        );
        if !plain {
            return false;
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        match keyval {
            gdk::Key::Up | gdk::Key::Down => {
                let takes = self.row_takes();
                self.selection
                    .borrow_mut()
                    .step(&takes, keyval == gdk::Key::Down, shift);
                self.publish_selection();
                self.show_selection();
                let cursor = self.selection.borrow().cursor.clone();
                if let Some(row) = self
                    .rows
                    .borrow()
                    .iter()
                    .find(|r| Some(&r.take) == cursor.as_ref())
                {
                    row.main.grab_focus();
                }
                true
            }
            gdk::Key::Return | gdk::Key::KP_Enter if !shift => {
                let single = self.selection.borrow().single().cloned();
                if let Some(take) = single
                    && self.insertable(&take)
                {
                    self.ctx
                        .store
                        .dispatch(view::insert_at(&take, InsertAt::End));
                }
                true
            }
            gdk::Key::space if !shift => {
                self.toggle_preview();
                true
            }
            _ => false,
        }
    }

    fn insertable(&self, take: &TakeId) -> bool {
        self.rows
            .borrow()
            .iter()
            .find(|r| r.take == *take)
            .and_then(|r| r.row())
            .is_some_and(|r| r.insertable)
    }

    // ------------------------------------------------------- preview

    /// `bin.preview` (D-17): ends the preview when there is one (the `Esc`
    /// chain runs it), else previews the selected take.
    fn preview_action(self: &Rc<Self>) {
        if self.ctx.store.local().take_preview.is_some() {
            self.end_preview();
            return;
        }
        let single = self.selection.borrow().single().cloned();
        if let Some(take) = single {
            self.start_preview(&take);
        }
    }

    /// `Space` in the bin (D-17): pauses or plays the selected take's
    /// preview again, else previews the selected take.
    fn toggle_preview(self: &Rc<Self>) {
        let preview = self.ctx.store.local().take_preview.clone();
        let single = self.selection.borrow().single().cloned();
        match (preview, single) {
            (Some(p), Some(take)) if p.take == take => {
                self.ctx.store.update_local(|l| {
                    if let Some(p) = &mut l.take_preview {
                        p.playing = !p.playing;
                    }
                });
            }
            (_, Some(take)) => self.start_preview(&take),
            (Some(_), None) => self.end_preview(),
            (None, None) => {}
        }
    }

    /// Plays `take` in the Program picture (D-17): Live switches to
    /// Program first, and stays there.
    fn start_preview(self: &Rc<Self>, take: &TakeId) {
        let state = self.ctx.store.state();
        if !state.runtime.record.is_idle() || !self.insertable(take) {
            return;
        }
        if state.ui.canvas_mode == CanvasMode::Live {
            self.ctx.store.dispatch(Command::SetCanvasMode {
                mode: CanvasMode::Program,
            });
        }
        let take = take.clone();
        self.ctx.store.update_local(move |l| {
            l.take_preview = Some(TakePreview {
                take,
                playing: true,
            });
        });
    }

    fn end_preview(&self) {
        self.ctx.store.update_local(|l| l.take_preview = None);
    }
}
