//! The command palette (S19, S19b, C34; SPEC §10.1 layer 3). Owner: WP9.
//! It registers `palette.open` (`Ctrl+K`, `:` and the header's Commands
//! button).
//!
//! What it shows is `qrec_core::view::palette` (rows, matches, Recent,
//! groups, why a row can't run); what a row does is [`decide_row`]: what
//! its key does, through the key router's own path
//! (`router::decide_button`, which `KeyRouter::run_binding` carries out),
//! so a row runs its command after the key's pre-steps, opens what the
//! key opens, or says why it can't. "Clear undo history" (D-19) sends
//! `HistoryClear`; the core's `confirmation_required` opens the modal by
//! the default reply policy, whose Clear history sends it again confirmed
//! (`Store::dispatch_confirmed`).
//!
//! S19's anatomy: 640 wide at (480, 96) over the `scrim` (G§3.15's
//! `Layer::Palette`, down to the hint bar, which shows the palette's keys),
//! `raised`, `lg` radius, e2, a 1 px `border`; a 16 px padded 40 px search
//! field (a 20 px `search` icon 10 px before a 16 px query, the focus ring
//! 2 px out), a hairline, then the results (8 px padding; 4 at the top
//! under group titles): 40 px rows (48 with a reason) padded 0 12, the
//! label (matches in 600) · the key caps · the CLI form, 200 px wide,
//! right-aligned, `mono-sm` `muted` (`text` on the highlighted row, R2's
//! treatment). It fits its content up to 560 px (S19b), then scrolls.
//!
//! The keyboard stays in the field: `↑` `↓` move the highlight over the
//! rows that can run (`Page_Up` `Page_Down` by 8; `Home` and `End` stay
//! the field's), `Enter` runs it (the palette closes first, so the action
//! lands on the window), `Esc` closes. A click on a row runs it. The ids
//! run this session feed "Recent" (newest first).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use qrec_core::view::palette::{PaletteItem, PaletteView, palette};

use crate::keys::router::{self, KeyDecision, RouterState};
use crate::store::local::{LocalUi, Overlay};
use crate::widgets::keycap::{self, CapSize, CapVariant};
use crate::window::layers::Layer;
use crate::{Ctx, guarded};

/// Whether the palette is built (`GuiFacts.built`).
pub const BUILT: bool = true;

/// The palette's most height (S19b: 560 px, then the results scroll).
pub const MAX_HEIGHT: i32 = 560;

/// How many ids "Recent" remembers (it shows `view::palette::RECENT`).
const REMEMBERED: usize = 12;

/// What running palette row `id` does: what its key does (the router's
/// `decide_button`, a button's path to a binding, G§3.7). The palette
/// closes, then `KeyRouter::run_binding(id)` carries it out.
pub fn decide_row(id: &str, rs: &RouterState<'_>) -> KeyDecision {
    router::decide_button(id, rs)
}

/// The ids of the rows the highlight can land on (those that can run), in
/// order (GTK-free).
pub fn runnable(view: &PaletteView) -> Vec<String> {
    view.groups
        .iter()
        .flat_map(|g| &g.rows)
        .filter(|r| r.enabled)
        .map(|r| r.id.clone())
        .collect()
}

/// Where the highlight goes for a key (GTK-free): `step` rows down (up
/// when negative) from `current` among `runnable`, held at the ends; the
/// first row when there is no current one.
pub fn moved(runnable: &[String], current: Option<&str>, step: i32) -> Option<String> {
    if runnable.is_empty() {
        return None;
    }
    let last = runnable.len() as i32 - 1;
    let at = current
        .and_then(|c| runnable.iter().position(|r| r == c))
        .map(|i| (i as i32 + step).clamp(0, last));
    Some(runnable[at.unwrap_or(0) as usize].clone())
}

/// `recent` with `id` run now: first, once (GTK-free).
pub fn remember(recent: &mut Vec<String>, id: &str) {
    recent.retain(|r| r != id);
    recent.insert(0, id.to_owned());
    recent.truncate(REMEMBERED);
}

/// The label's Pango markup: the matched characters at weight 600.
pub fn label_markup(item: &PaletteItem) -> String {
    let mut out = String::new();
    let mut at = 0;
    for range in item.highlights() {
        out.push_str(&glib::markup_escape_text(&item.label[at..range.start]));
        out.push_str("<span weight=\"600\">");
        out.push_str(&glib::markup_escape_text(&item.label[range.clone()]));
        out.push_str("</span>");
        at = range.end;
    }
    out.push_str(&glib::markup_escape_text(&item.label[at..]));
    out
}

/// C29's search field (the palette's 40 px one when `lg`, 32 elsewhere):
/// a `.field.search` box with the `search` icon (20 or 16 px) and a text
/// box, `placeholder` until something is typed, named `accessible` for
/// screen readers (a bare `GtkText` is left out of the tree). Named
/// `<name>-field` and `<name>-query`. The box draws the field and its
/// focus ring (`:focus-within`); the text has the keyboard (G§3.8 step 2).
pub(crate) fn search_field(
    name: &str,
    placeholder: &str,
    accessible: &str,
    lg: bool,
) -> (gtk::Box, gtk::Text) {
    let field = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    field.set_widget_name(&format!("{name}-field"));
    field.add_css_class("field");
    field.add_css_class("qrec-search");
    if lg {
        field.add_css_class("lg");
    }
    let icon = gtk::Image::from_icon_name("qrec-search-symbolic");
    icon.add_css_class("search-icon");
    icon.set_pixel_size(if lg { 20 } else { 16 });
    let text = gtk::Text::builder()
        .accessible_role(gtk::AccessibleRole::TextBox)
        .placeholder_text(placeholder)
        .hexpand(true)
        .build();
    text.set_widget_name(&format!("{name}-query"));
    text.update_property(&[gtk::accessible::Property::Label(accessible)]);
    field.append(&icon);
    field.append(&text);
    // A click anywhere on the field types into it.
    let click = gtk::GestureClick::new();
    let target = text.downgrade();
    click.connect_pressed(guarded!("search field: a click", move |_, _, _, _| {
        if let Some(text) = target.upgrade() {
            text.grab_focus();
        }
    }));
    field.add_controller(click);
    (field, text)
}

/// The palette (opened in `Layer::Palette`).
pub struct Palette {
    root: gtk::Box,
}

impl Palette {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Dialog)
            .build();
        root.set_widget_name("palette");
        root.add_css_class("qrec-palette");
        root.set_size_request(640, -1);
        // BRIEF §2.8: 640 wide at x 480, top at y 96 (centered in 1600).
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Start);
        root.set_margin_top(96);
        root.set_overflow(gtk::Overflow::Hidden);
        root.update_property(&[gtk::accessible::Property::Label("Command palette")]);

        // The search field: `.field.search.lg`, the icon and a text box.
        let search = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        search.add_css_class("palette-search");
        let (field, text) = search_field("palette", "Type a command", "Search commands", true);
        field.set_hexpand(true);
        search.append(&field);
        root.append(&search);

        let divider = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        divider.add_css_class("palette-divider");
        root.append(&divider);

        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rows.set_widget_name("palette-rows");
        rows.add_css_class("palette-results");
        // The results fit their rows up to the palette's 560 px (its 1 px
        // border twice, the 72 px search, the hairline), then scroll.
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(
                (f64::from(MAX_HEIGHT - 2 - 72 - 1) * ctx.theme.scale()).round() as i32,
            )
            .child(&rows)
            .build();
        scroller.set_widget_name("palette-scroller");
        scroller.set_can_focus(false);
        root.append(&scroller);
        // The maximum is in px at 100 %: it follows a live `ui.scale`.
        let (weak, max) = (scroller.downgrade(), MAX_HEIGHT - 2 - 72 - 1);
        ctx.theme
            .subscribe(move |theme| {
                if let Some(scroller) = weak.upgrade() {
                    scroller
                        .set_max_content_height((f64::from(max) * theme.scale()).round() as i32);
                }
            })
            .detach();

        let parts = Rc::new(Parts {
            ctx: ctx.clone(),
            root: root.clone(),
            text: text.clone(),
            rows,
            scroller,
            view: RefCell::new(PaletteView::default()),
            row_widgets: RefCell::new(Vec::new()),
            highlighted: RefCell::new(None),
            open: Cell::new(false),
        });
        parts.connect(&text);

        // `palette.open`: opens it (or leaves it as it is).
        let store = ctx.store.clone();
        ctx.actions.register(
            "palette.open",
            Rc::new(move || {
                store.update_local(|l| {
                    if !l.overlays.contains(&Overlay::Palette) {
                        l.overlays.push(Overlay::Palette);
                    }
                });
            }),
        );
        let follower = Rc::clone(&parts);
        ctx.store
            .subscribe_local(move |local| follower.follow(local))
            .detach();
        // While it is open, the rows follow the project (what can run).
        let follower = Rc::clone(&parts);
        ctx.store
            .subscribe(move |_| {
                if follower.open.get() {
                    follower.refresh();
                }
            })
            .detach();
        // And the keymap (`keys.toml` changed): their caps.
        let follower = Rc::downgrade(&parts);
        crate::widgets::button::follow_keymap(&root, &ctx.store.keymap(), move |_, _| {
            if let Some(parts) = follower.upgrade()
                && parts.open.get()
            {
                parts.refresh();
            }
        });
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

/// The palette's widgets and what they show.
struct Parts {
    ctx: Ctx,
    root: gtk::Box,
    text: gtk::Text,
    rows: gtk::Box,
    scroller: gtk::ScrolledWindow,
    view: RefCell<PaletteView>,
    /// Each row's id and widget, in order.
    row_widgets: RefCell<Vec<(String, gtk::Box)>>,
    /// The highlighted row (R2), one that can run.
    highlighted: RefCell<Option<String>>,
    open: Cell<bool>,
}

thread_local! {
    /// The ids run from the palette this session, newest first ("Recent";
    /// one window per process, main thread only).
    static RECENT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The ids run this session, newest first.
pub fn recent() -> Vec<String> {
    RECENT.with(|r| r.borrow().clone())
}

/// Recent as given, newest first (the test channel's `paletteRecent`:
/// S19b's shot shows the frame's Recent without running its rows).
pub fn seed_recent(ids: Vec<String>) {
    RECENT.with(|r| {
        let mut recent = r.borrow_mut();
        recent.clear();
        for id in ids.iter().rev() {
            remember(&mut recent, id);
        }
    });
}

impl Parts {
    /// Opens or closes with `Overlay::Palette`.
    fn follow(self: &Rc<Self>, local: &LocalUi) {
        let wanted = local.overlays.contains(&Overlay::Palette);
        if wanted == self.open.get() {
            return;
        }
        self.open.set(wanted);
        if !wanted {
            self.ctx.window.close(Layer::Palette);
            return;
        }
        self.text.set_text("");
        *self.highlighted.borrow_mut() = None;
        self.refresh();
        self.ctx.window.open(Layer::Palette, &self.root);
        if let Some(scrim) = self.root.prev_sibling() {
            scrim.set_widget_name("scrim-palette");
        }
        self.text.grab_focus();
    }

    /// The rows for the query as it is, the highlight kept on its row when
    /// that row is still there and can run, else the first that can.
    fn refresh(self: &Rc<Self>) {
        let state = self.ctx.store.state();
        let keymap = self.ctx.store.keymap();
        let query = self.text.text().to_string();
        let view = palette(&keymap, &state, &query, &recent());
        let runnable = runnable(&view);
        let keep = self
            .highlighted
            .borrow()
            .clone()
            .filter(|h| runnable.contains(h));
        *self.highlighted.borrow_mut() = keep.or_else(|| runnable.first().cloned());
        self.build_rows(&view, &query);
        *self.view.borrow_mut() = view;
        self.show_highlight(false);
    }

    fn build_rows(self: &Rc<Self>, view: &PaletteView, query: &str) {
        while let Some(child) = self.rows.first_child() {
            self.rows.remove(&child);
        }
        let grouped = view.groups.iter().any(|g| g.title.is_some());
        self.rows.remove_css_class("grouped");
        if grouped {
            self.rows.add_css_class("grouped");
        }
        let mut widgets = Vec::new();
        for group in &view.groups {
            if let Some(title) = &group.title {
                let header = gtk::Label::new(Some(title));
                header.add_css_class("palette-group");
                header.add_css_class("t-caption-strong");
                header.add_css_class("muted");
                header.set_xalign(0.0);
                header.set_yalign(1.0);
                self.rows.append(&header);
            }
            for item in &group.rows {
                let row = self.row(item);
                self.rows.append(&row);
                widgets.push((item.id.clone(), row));
            }
        }
        if view.groups.is_empty() {
            let none = gtk::Label::new(Some(&format!(
                "No commands match \u{201c}{}\u{201d}",
                query.trim()
            )));
            none.set_widget_name("palette-none");
            none.add_css_class("palette-none");
            none.add_css_class("t-body");
            none.add_css_class("muted");
            none.set_xalign(0.0);
            self.rows.append(&none);
        }
        *self.row_widgets.borrow_mut() = widgets;
    }

    /// One C34 row: label (and reason) · caps · CLI form.
    fn row(self: &Rc<Self>, item: &PaletteItem) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_widget_name(&format!("palette-row-{}", item.id));
        row.add_css_class("palette-row");
        if !item.enabled {
            row.add_css_class("disabled");
        }
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let label = gtk::Label::new(None);
        label.set_markup(&label_markup(item));
        label.add_css_class("t-body");
        label.add_css_class("palette-label");
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&label);
        if let Some(reason) = item.reason.as_deref().filter(|_| !item.enabled) {
            let why = gtk::Label::new(Some(reason));
            why.add_css_class("t-caption");
            why.add_css_class("muted");
            why.add_css_class("palette-reason");
            why.set_xalign(0.0);
            text.append(&why);
        }
        row.append(&text);
        if let Some(chord) = item.keys.first() {
            let caps = keycap::chord(chord, CapSize::Md, CapVariant::Plain);
            caps.add_css_class("palette-keys");
            row.append(&caps);
        }
        let cli = gtk::Label::new(Some(item.cli.as_deref().unwrap_or("\u{2014}")));
        cli.add_css_class("t-mono-sm");
        cli.add_css_class("palette-cli");
        cli.set_xalign(1.0);
        cli.set_size_request(200, -1);
        cli.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        row.append(&cli);
        // A screen reader hears the row as the field's active descendant;
        // its name says what it runs and why it can't.
        let spoken = match (&item.reason, item.enabled) {
            (Some(reason), false) => format!("{}, unavailable: {reason}", item.label),
            _ => item.label.clone(),
        };
        row.update_property(&[gtk::accessible::Property::Label(&spoken)]);
        if item.enabled {
            let click = gtk::GestureClick::new();
            let (parts, id) = (Rc::downgrade(self), item.id.clone());
            click.connect_released(guarded!("palette: a row", move |gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let Some(parts) = parts.upgrade() {
                    parts.run(&id);
                }
            }));
            row.add_controller(click);
            let hover = gtk::EventControllerMotion::new();
            let (parts, id) = (Rc::downgrade(self), item.id.clone());
            hover.connect_enter(guarded!("palette: hover", move |_, _, _| {
                if let Some(parts) = parts.upgrade() {
                    *parts.highlighted.borrow_mut() = Some(id.clone());
                    parts.show_highlight(false);
                }
            }));
            row.add_controller(hover);
        }
        row
    }

    /// Marks the highlighted row (R2), keeps it in view (`scroll`) and
    /// tells screen readers.
    fn show_highlight(&self, scroll: bool) {
        let highlighted = self.highlighted.borrow().clone();
        let mut shown = None;
        for (id, row) in self.row_widgets.borrow().iter() {
            if Some(id) == highlighted.as_ref() {
                row.add_css_class("highlighted");
                shown = Some(row.clone());
            } else {
                row.remove_css_class("highlighted");
            }
        }
        match &shown {
            Some(row) => {
                self.text
                    .update_relation(&[gtk::accessible::Relation::ActiveDescendant(
                        row.upcast_ref(),
                    )]);
                if scroll {
                    self.scroll_to(row);
                }
            }
            None => self
                .text
                .reset_relation(gtk::AccessibleRelation::ActiveDescendant),
        }
    }

    /// Scrolls the results so `row` shows whole.
    fn scroll_to(&self, row: &gtk::Box) {
        let Some(bounds) = row.compute_bounds(&self.rows) else {
            return;
        };
        let adj = self.scroller.vadjustment();
        let (top, bottom) = (
            f64::from(bounds.y()),
            f64::from(bounds.y() + bounds.height()),
        );
        let page = adj.page_size();
        if top < adj.value() {
            adj.set_value(top - 4.0);
        } else if bottom > adj.value() + page {
            adj.set_value(bottom - page + 8.0);
        }
    }

    /// Moves the highlight `step` rows.
    fn step(&self, step: i32) {
        let runnable = runnable(&self.view.borrow());
        let next = moved(&runnable, self.highlighted.borrow().as_deref(), step);
        *self.highlighted.borrow_mut() = next;
        self.show_highlight(true);
    }

    /// Runs row `id`: the palette closes, the id goes first in Recent, and
    /// the key's path runs it (`KeyRouter::run_binding`).
    fn run(&self, id: &str) {
        let runs = self
            .view
            .borrow()
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .any(|r| r.id == id && r.enabled);
        if !runs {
            return;
        }
        RECENT.with(|r| remember(&mut r.borrow_mut(), id));
        tracing::info!("palette: {id}");
        self.ctx.store.update_local(|l| {
            l.close_overlays(|o| *o == Overlay::Palette);
        });
        self.ctx.keys.run_binding(id);
    }

    fn connect(self: &Rc<Self>, text: &gtk::Text) {
        let parts = Rc::downgrade(self);
        text.connect_changed(guarded!("palette: the query", move |_| {
            if let Some(parts) = parts.upgrade()
                && parts.open.get()
            {
                *parts.highlighted.borrow_mut() = None;
                parts.refresh();
                parts.scroller.vadjustment().set_value(0.0);
            }
        }));
        let parts = Rc::downgrade(self);
        text.connect_activate(guarded!("palette: Enter", move |_| {
            if let Some(parts) = parts.upgrade() {
                let id = parts.highlighted.borrow().clone();
                if let Some(id) = id {
                    parts.run(&id);
                }
            }
        }));
        // The field keeps the keyboard (G§3.8 step 2): it takes the
        // palette's own keys before GTK's text does.
        let keys = gtk::EventControllerKey::new();
        let parts = Rc::downgrade(self);
        keys.connect_key_pressed(guarded!(
            "palette: keys",
            after_panic = glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                let Some(parts) = parts.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let plain = !modifiers.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK,
                );
                let page = 8;
                let step = match key {
                    gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up => -1,
                    gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down => 1,
                    gtk::gdk::Key::Page_Up | gtk::gdk::Key::KP_Page_Up => -page,
                    gtk::gdk::Key::Page_Down | gtk::gdk::Key::KP_Page_Down => page,
                    gtk::gdk::Key::Escape => {
                        parts.ctx.store.update_local(|l| {
                            l.close_overlays(|o| *o == Overlay::Palette);
                        });
                        return glib::Propagation::Stop;
                    }
                    _ => return glib::Propagation::Proceed,
                };
                if !plain {
                    return glib::Propagation::Proceed;
                }
                parts.step(step);
                glib::Propagation::Stop
            }
        ));
        text.add_controller(keys);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use qrec_core::history::HistoryEntry;
    use qrec_core::keymap::{self, Action, Keymap, action_for};
    use qrec_core::state::{CanvasMode, RecordMode, RecordPhase};
    use qrec_core::view::confirm::{self, ConfirmFacts};
    use qrec_core::view::notices::Notice;
    use qrec_core::view::{FocusRegion, palette};
    use qrec_core::{ClipId, Command, ErrorCode, Frame, LayerId, State, TakeId, testkit};

    use super::*;
    use crate::host::queue::Queue;
    use crate::keys::router::{KeySink, apply};
    use crate::store::Store;
    use crate::store::local::{LocalUi, Overlay, StartupPhase};

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Dispatch(Command),
        Gui(String),
        Other,
    }

    #[derive(Default)]
    struct Recorder(Vec<Call>);

    impl KeySink for Recorder {
        fn dispatch(&mut self, cmd: Command) {
            self.0.push(Call::Dispatch(cmd));
        }
        fn gui(&mut self, id: &str) {
            self.0.push(Call::Gui(id.to_owned()));
        }
        fn notice(&mut self, _: Notice) {
            self.0.push(Call::Other);
        }
        fn set_playing_rate(&mut self, _: f64) {
            self.0.push(Call::Other);
        }
        fn player_rate(&mut self, _: f64) {
            self.0.push(Call::Other);
        }
        fn key_parks(&mut self, _: Frame) {
            self.0.push(Call::Other);
        }
        fn cancel_drag(&mut self) {
            self.0.push(Call::Other);
        }
        fn close_overlay(&mut self) {
            self.0.push(Call::Other);
        }
    }

    /// The design state with 128 undo steps.
    fn with_history(mut state: State) -> State {
        state.history.entries = Arc::new(
            (1..=128)
                .map(|seq| HistoryEntry {
                    seq,
                    ts: state.project.created_at,
                    label: format!("Edit {seq}"),
                    cmd: serde_json::Value::Null,
                    forward: Default::default(),
                    inverse: Default::default(),
                    after_hash: String::new(),
                })
                .collect(),
        );
        state.history.cursor = 128;
        state.history.next_seq = 129;
        state
    }

    /// The window states the table runs in, open: S02, S06, the canvas
    /// with L2 selected, a take recording.
    fn scenes() -> Vec<(&'static str, State, LocalUi)> {
        let local = LocalUi {
            startup: StartupPhase::Open,
            focus: FocusRegion::Timeline,
            ..LocalUi::default()
        };
        let s02 = with_history(testkit::design_state());
        let mut s06 = s02.clone();
        s06.ui.canvas_mode = CanvasMode::Program;
        s06.ui.selection = vec![ClipId::from_raw("c2d0")];
        let mut canvas = s02.clone();
        canvas.ui.selected_layer = Some(LayerId::numbered(2));
        let mut recording = s06.clone();
        recording.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        vec![
            ("S02", s02, local.clone()),
            ("S06", s06, local.clone()),
            (
                "canvas",
                canvas,
                LocalUi {
                    focus: FocusRegion::Stage,
                    ..local.clone()
                },
            ),
            ("recording", recording, local),
        ]
    }

    /// The GUI action a row of `id` opens when its key does (G§3.8 step
    /// 10), else `id` itself.
    fn window_action(id: &str) -> &str {
        match id {
            "export" => "export.panel",
            "autocut.preview" => "autocut.page",
            other => other,
        }
    }

    /// The dispatch table (WP9): every row the palette offers runs, as its
    /// key would: a row with a command dispatches `action_for`'s command
    /// after the key's pre-steps (none here: nothing plays), a row the
    /// window handles runs its GUI action; none is refused or ignored.
    #[test]
    fn every_enabled_row_runs_as_its_key_would() {
        let keymap: Keymap = keymap::resolve("");
        let mut ran = (0, 0);
        for (name, state, local) in scenes() {
            let rs = RouterState {
                state: &state,
                local: &local,
                keymap: &keymap,
                player_frame: None,
            };
            let view = palette::palette(&keymap, &state, "", &[]);
            for row in view
                .groups
                .iter()
                .flat_map(|g| &g.rows)
                .filter(|r| r.enabled)
            {
                let decision = decide_row(&row.id, &rs);
                let mut sink = Recorder::default();
                assert!(
                    apply(&decision, &state, &mut sink),
                    "{name}: {} unused",
                    row.id
                );
                match &decision {
                    KeyDecision::Run { cmd, pre, .. } => {
                        assert_eq!(
                            action_for(&row.id, &state),
                            Some(Action::Run(cmd.clone())),
                            "{name}: {}",
                            row.id
                        );
                        assert!(
                            pre.is_empty()
                                || row.id.starts_with("record.")
                                || row.id.starts_with("shuttle."),
                            "{name}: {} {pre:?}",
                            row.id
                        );
                        assert_eq!(
                            sink.0.last(),
                            Some(&Call::Dispatch(cmd.clone())),
                            "{name}: {}",
                            row.id
                        );
                        ran.0 += 1;
                    }
                    KeyDecision::Gui { id } => {
                        assert_eq!(id, window_action(&row.id), "{name}: {}", row.id);
                        assert_eq!(sink.0, [Call::Gui(id.clone())], "{name}: {}", row.id);
                        ran.1 += 1;
                    }
                    other => panic!("{name}: the enabled row {} gives {other:?}", row.id),
                }
            }
        }
        assert!(ran.0 > 60 && ran.1 > 40, "{ran:?}");
    }

    /// "Clear undo history" → the confirmation modal → `HistoryClear`
    /// sent again, confirmed: the palette's path, the default reply policy
    /// on the core's own answer, the modal's copy, its Clear history.
    #[test]
    fn clear_undo_history_asks_then_sends_it_confirmed() {
        let (_, state, local) = scenes().remove(1);
        let keymap = keymap::resolve("");
        let rs = RouterState {
            state: &state,
            local: &local,
            keymap: &keymap,
            player_frame: None,
        };
        let row = &palette::palette(&keymap, &state, "clear", &[]).groups[0].rows[0];
        assert_eq!((row.id.as_str(), row.enabled), ("history.clear", true));
        let decision = decide_row(&row.id, &rs);
        assert_eq!(
            decision,
            KeyDecision::Run {
                id: "history.clear".into(),
                pre: vec![],
                cmd: Command::HistoryClear,
            }
        );

        let store = Store::new(
            Queue::new(|| {}),
            PathBuf::from("/projects/rust-traits-talk"),
            Arc::new(state.clone()),
            keymap.clone(),
            local.clone(),
        );
        // What `KeyRouter::run_binding` sends: the command, by `dispatch`.
        store.dispatch(Command::HistoryClear);
        let _ = store.queue().take_all(); // no host: its "busy" is dropped
        let ctx = testkit::ctx(testkit::CREATED_AT, 7);
        let asked = qrec_core::apply(&state, &ctx, Command::HistoryClear).unwrap_err();
        assert_eq!(asked.code, ErrorCode::ConfirmationRequired);
        store.on_reply(1, Err(asked.clone()), false);
        let overlay = Overlay::Confirm {
            cmd: Command::HistoryClear,
            error: asked.clone(),
        };
        assert_eq!(store.local().overlays, std::slice::from_ref(&overlay));
        // The modal shows `view::confirm`'s copy (`modal::content_for`).
        let content = confirm::modal(
            &Command::HistoryClear,
            &asked,
            &state,
            &ConfirmFacts::default(),
        );
        assert_eq!(content.title, "Get out of my swamp?");
        assert_eq!(content.lead, "Clear undo history?");
        assert!(
            content.body.as_deref().is_some_and(
                |b| b.starts_with("This removes 128 undo steps from .qrec/history.jsonl.")
            ),
            "{content:?}"
        );
        assert_eq!(content.confirm, "Clear history");
        // Its Clear history: the command again, confirmed, then it closes.
        store.dispatch_confirmed(Command::HistoryClear);
        store.update_local(move |l| {
            l.close_overlays(|o| *o == overlay);
        });
        assert_eq!(store.pending_requests(), 1, "sent again");
        assert!(store.local().overlays.is_empty());
        // The core clears it once confirmed.
        let confirmed = qrec_core::Ctx {
            confirmed: true,
            ..ctx
        };
        let cleared = qrec_core::apply(&state, &confirmed, Command::HistoryClear).unwrap();
        assert!(cleared.state.history.entries.is_empty());
        let rs = RouterState {
            state: &cleared.state,
            ..rs
        };
        assert_eq!(
            decide_row("history.clear", &rs),
            KeyDecision::Unavailable {
                id: "history.clear".into(),
                reason: "The undo history is empty".into(),
            }
        );
    }

    /// The highlight goes over the rows that can run, held at the ends
    /// (S19: the first row highlighted); Recent keeps each id once, newest
    /// first.
    #[test]
    fn the_highlight_and_recent() {
        let state = scenes().remove(1).1;
        let keymap = keymap::resolve("");
        let view = palette::palette(&keymap, &state, "", &[]);
        let ids = runnable(&view);
        assert!(!ids.contains(&"history.redo".to_owned()), "Nothing to redo");
        assert_eq!(moved(&ids, None, 1).as_deref(), Some(ids[0].as_str()));
        assert_eq!(
            moved(&ids, Some(&ids[0]), 1).as_deref(),
            Some(ids[1].as_str())
        );
        assert_eq!(
            moved(&ids, Some(&ids[0]), -1).as_deref(),
            Some(ids[0].as_str()),
            "held at the top"
        );
        let last = ids.last().unwrap();
        assert_eq!(moved(&ids, Some(last), 8).as_deref(), Some(last.as_str()));
        assert_eq!(moved(&[], None, 1), None);
        let mut recent = vec!["a".to_owned(), "b".to_owned()];
        remember(&mut recent, "b");
        assert_eq!(recent, ["b", "a"]);
        for n in 0..20 {
            remember(&mut recent, &format!("x{n}"));
        }
        assert_eq!(recent.len(), REMEMBERED);
        assert_eq!(recent[0], "x19");
    }

    /// The matched letters go bold (600) in the label's markup, the rest
    /// escaped.
    #[test]
    fn matches_are_marked_up() {
        let state = scenes().remove(1).1;
        let keymap = keymap::resolve("");
        let view = palette::palette(&keymap, &state, "sp", &[]);
        let split = &view.groups[0].rows[0];
        assert_eq!(
            label_markup(split),
            "<span weight=\"600\">Sp</span>lit clip"
        );
        let item = PaletteItem {
            id: "x".into(),
            label: "A <b> & c".into(),
            matched: vec![0, 8],
            keys: vec![],
            cli: None,
            enabled: true,
            reason: None,
        };
        assert_eq!(
            label_markup(&item),
            "<span weight=\"600\">A</span> &lt;b&gt; &amp; <span weight=\"600\">c</span>"
        );
    }
}
