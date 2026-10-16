//! The export panel (S15a, S15b, S15c, S16, S17, C25, C29, C30): setup,
//! running (passive, D-49), done, failed; in-window, under the header's
//! Export slot. Owner: WP7. Registers `export.panel` (it toggles the
//! panel; the `Esc` chain runs it to close the passive one) and
//! `export.show` (it only opens it: a toast's Show export, D-34).
//!
//! - Setup, Done and Failed trap the focus (`Layer::FloatingTrap`: the
//!   overlay keys, an outside click closes them); Running is passive
//!   (`Layer::FloatingPassive`: keys and clicks reach the window, S15b's
//!   hints stay live) and `Esc`, `Ctrl+E` or the header slot close it.
//!   It changes its layer only while it is the top overlay
//!   (`flow::layer`): its export ending under Settings or a modal opened
//!   over the passive panel, it takes the trap once they close.
//! - What it shows follows [`flow::mode`]: any running export (the CLI's
//!   too) shows Running; its own export's outcome shows Done or Failed
//!   until it closes; else Setup. A foreign export that ends closes it
//!   (the core's toast says how it went).
//! - Its own export goes out with `confirmed: false` (D-24): an existing
//!   hand-picked file opens the Replace modal (`Store::ask_confirmation`),
//!   whose confirmation sends it again, confirmed, still as the panel's
//!   own (Running, then Done or Failed); a refused path shows the core's
//!   message under the Output field. "Change…" picks the path with
//!   `gtk::FileDialog::save` on `exports/` with the auto name (under
//!   `QREC_GUI_TEST` the paths in `QREC_GUI_TEST_SAVE_AS`, `|`-separated,
//!   one per press: no portal in the tests, G§9 R7).
//! - The caption names the encoder as `config.toml` and the project choose
//!   (`view::export::caption`): the config is read once on `qrec-gui-io`.
//! - Placement (BRIEF §2.8): 440 wide, 8 px from the window's right edge,
//!   the body's top at y 52, its arrow 172 px left of its right edge.

mod flow;
mod pages;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, Offset, TimeZone, Utc};
use gtk::glib;
use gtk::prelude::*;
use qrec_core::Command;
use qrec_core::config::UserConfig;
use qrec_core::protocol::UiAction;
use qrec_core::protocol::command::{ExportFormat, Quality};
use qrec_core::state::{Job, JobKind, State};
use qrec_core::view::export::{self as view, FailedExport};
use qrec_core::view::format;

pub use flow::{
    ACTIONS, Answer, Draft, ExportPanelState, Outcome, PanelRequest, PanelStep, SHOW, TOGGLE,
    panel_step,
};

use crate::store::local::{ExportPanelMode, Overlay};
use crate::store::replies::{ReplyAction, default_action};
use crate::store::{Change, Reply};
use crate::widgets::mud_bar::MudState;
use crate::window::layers::Layer;
use crate::{Ctx, desktop, guard, guarded, testctl};
use pages::{Arrow, DonePage, FailedPage, RunningPage, STDERR_VISIBLE, SetupPage};

/// The export panel (opened in `Layer::FloatingTrap` or, while running,
/// `Layer::FloatingPassive`).
pub struct ExportPanel {
    root: gtk::Overlay,
    /// The panel itself: its subscriptions hold it weakly, the window
    /// keeps the region (and so this) for its life.
    _panel: Rc<Panel>,
}

impl ExportPanel {
    pub fn new(ctx: &Ctx) -> Self {
        let panel = Panel::build(ctx);
        Self {
            root: panel.root.clone(),
            _panel: panel,
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

/// The panel's own export on its way.
#[derive(Debug, Clone)]
struct Own {
    /// The first snapshot with its job: the timeline it renders.
    started: Option<Arc<State>>,
    /// Its job as last seen (the failed panel's "37%" and clip).
    last: Option<Job>,
}

struct Panel {
    ctx: Ctx,
    root: gtk::Overlay,
    stack: gtk::Stack,
    setup: SetupPage,
    running: RunningPage,
    done: DonePage,
    failed: FailedPage,
    /// The layer it is open in.
    layer: Cell<Option<Layer>>,
    /// The page on screen.
    shown: Cell<Option<ExportPanelMode>>,
    own: RefCell<Option<Own>>,
    config: RefCell<UserConfig>,
    /// The auto name shown in Setup, made when the panel opens.
    preview: RefCell<(String, String)>,
    ticker: RefCell<Option<glib::SourceId>>,
    /// Set while the setup controls follow the draft (no edits then).
    syncing: Cell<bool>,
    /// `QREC_GUI_TEST_SAVE_AS`'s paths still to hand out (test mode).
    test_paths: RefCell<VecDeque<String>>,
}

/// Scrolls `scroller` to its end once its new content is laid out (S17: the
/// stderr box shows its last lines): at the next frames, until the end
/// holds two frames in a row (at most 30).
fn pin_to_end(scroller: &gtk::ScrolledWindow) {
    let frames = std::cell::Cell::new(0u32);
    let steady = std::cell::Cell::new(0u32);
    guard::tick(scroller, "export: the stderr's end", move |scroller, _| {
        let adj = scroller.vadjustment();
        let end = (adj.upper() - adj.page_size()).max(0.0);
        if (adj.value() - end).abs() < 0.5 && adj.page_size() > 0.0 {
            steady.set(steady.get() + 1);
        } else {
            steady.set(0);
            adj.set_value(end);
        }
        frames.set(frames.get() + 1);
        if steady.get() >= 2 || frames.get() >= 30 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

/// The running export, if any (one at a time, A§5.9).
fn export_job(state: &State) -> Option<&Job> {
    state
        .runtime
        .jobs
        .values()
        .find(|job| matches!(job.kind, JobKind::Export { .. }))
}

/// The UTC offset of the local zone at `at` (each instant its own).
fn offset_at(at: DateTime<Utc>) -> i32 {
    Local
        .offset_from_utc_datetime(&at.naive_utc())
        .fix()
        .local_minus_utc()
}

impl Panel {
    fn build(ctx: &Ctx) -> Rc<Self> {
        let root = gtk::Overlay::new();
        root.set_widget_name("export-panel");
        root.add_css_class("qrec-export");
        root.set_halign(gtk::Align::End);
        root.set_valign(gtk::Align::Start);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.add_css_class("export-body");
        body.set_widget_name("export-body");
        let stack = gtk::Stack::new();
        stack.set_vhomogeneous(false);
        stack.set_hhomogeneous(true);
        stack.set_interpolate_size(false);
        let setup = SetupPage::new();
        let running = RunningPage::new(&ctx.theme);
        let done = DonePage::new(&ctx.theme);
        let failed = FailedPage::new(&ctx.theme);
        stack.add_named(&setup.root, Some("setup"));
        stack.add_named(&running.root, Some("running"));
        stack.add_named(&done.root, Some("done"));
        stack.add_named(&failed.root, Some("failed"));
        body.append(&stack);
        root.set_child(Some(&body));
        root.add_overlay(&Arrow::new(&ctx.theme));
        root.update_property(&[gtk::accessible::Property::Label("Export")]);
        let test_paths = if testctl::mode().on {
            std::env::var("QREC_GUI_TEST_SAVE_AS")
                .unwrap_or_default()
                .split('|')
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()
        } else {
            VecDeque::new()
        };
        let panel = Rc::new(Self {
            ctx: ctx.clone(),
            root,
            stack,
            setup,
            running,
            done,
            failed,
            layer: Cell::new(None),
            shown: Cell::new(None),
            own: RefCell::new(None),
            config: RefCell::new(UserConfig::default()),
            preview: RefCell::new((String::new(), String::new())),
            ticker: RefCell::new(None),
            syncing: Cell::new(false),
            test_paths: RefCell::new(test_paths),
        });
        panel.connect();
        panel.read_config();
        for (id, request) in ACTIONS {
            let weak = Rc::downgrade(&panel);
            ctx.actions.register(
                id,
                Rc::new(move || {
                    if let Some(panel) = weak.upgrade() {
                        panel.request(request);
                    }
                }),
            );
        }
        let weak = Rc::downgrade(&panel);
        ctx.store
            .subscribe(move |change| {
                if let Some(panel) = weak.upgrade() {
                    panel.on_change(change);
                }
            })
            .detach();
        let weak = Rc::downgrade(&panel);
        ctx.store
            .subscribe_local(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.sync();
                }
            })
            .detach();
        panel
    }

    /// `config.toml` as the Session reads it, for the encoder caption (on
    /// the io worker: no file I/O on the GTK thread, R1).
    fn read_config(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.ctx.media.run_io_then(
            || {
                let env = qrec_shell::paths::Env::from_process();
                match qrec_shell::paths::UserDirs::resolve(&env) {
                    Ok(dirs) => qrec_shell::config_file::read(&dirs.config_file()).0,
                    Err(_) => UserConfig::default(),
                }
            },
            move |config| {
                if let Some(panel) = weak.upgrade() {
                    *panel.config.borrow_mut() = config;
                    if panel.shown.get() == Some(ExportPanelMode::Setup) {
                        panel.fill_setup();
                    }
                }
            },
        );
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let edit = move |f: Box<dyn FnOnce(&mut Draft)>| {
            if let Some(panel) = weak.upgrade() {
                if panel.syncing.get() {
                    return;
                }
                panel.ctx.store.update_local(move |l| {
                    let draft = &mut l.export_view.draft;
                    f(draft);
                    draft.error = None;
                });
            }
        };
        let edit = Rc::new(edit);
        let e = Rc::clone(&edit);
        self.setup
            .format
            .connect_active_name_notify(guarded!("export: format", move |group| {
                let format = match group.active_name().as_deref() {
                    Some("webm") => ExportFormat::Webm,
                    _ => ExportFormat::Mp4,
                };
                e(Box::new(move |d| d.format = format));
            }));
        let e = Rc::clone(&edit);
        self.setup
            .quality
            .connect_active_name_notify(guarded!("export: quality", move |group| {
                let quality = match group.active_name().as_deref() {
                    Some("draft") => Quality::Draft,
                    Some("high") => Quality::High,
                    _ => Quality::Standard,
                };
                e(Box::new(move |d| d.quality = quality));
            }));
        let e = Rc::clone(&edit);
        self.setup
            .av1
            .connect_active_notify(guarded!("export: av1", move |switch| {
                let on = switch.is_active();
                e(Box::new(move |d| d.av1 = on));
            }));
        let weak = Rc::downgrade(self);
        self.setup
            .change
            .connect_clicked(guarded!("export: change", move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.choose_output();
                }
            }));
        let weak = Rc::downgrade(self);
        self.setup
            .export
            .connect_clicked(guarded!("export: export", move |_| {
                if let Some(panel) = weak.upgrade() {
                    let cmd = panel.ctx.store.local().export_view.draft.command();
                    panel.start(cmd);
                }
            }));
        let store = self.ctx.store.clone();
        self.running
            .cancel
            .connect_clicked(guarded!("export: cancel", move |_| {
                store.dispatch(Command::ExportCancel { job: None });
            }));
        for (button, what) in [
            (&self.done.open, "open"),
            (&self.done.folder, "folder"),
            (&self.done.copy, "copy"),
        ] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(guarded!("export: a done action", move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.done_action(what);
                }
            }));
        }
        let weak = Rc::downgrade(self);
        self.failed
            .retry
            .connect_clicked(guarded!("export: try again", move |_| {
                let Some(panel) = weak.upgrade() else { return };
                let retry = match &panel.ctx.store.local().export_view.outcome {
                    Some(Outcome::Failed { retry, .. }) => Some((**retry).clone()),
                    _ => None,
                };
                if let Some(cmd) = retry {
                    panel.start(cmd);
                }
            }));
        let weak = Rc::downgrade(self);
        self.failed
            .close
            .connect_clicked(guarded!("export: close", move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.close();
                }
            }));
        let weak = Rc::downgrade(self);
        self.failed
            .copy
            .connect_clicked(guarded!("export: copy error", move |_| {
                let Some(panel) = weak.upgrade() else { return };
                let text = match &panel.ctx.store.local().export_view.outcome {
                    Some(Outcome::Failed { copy, .. }) => Some(copy.clone()),
                    _ => None,
                };
                if let Some(text) = text {
                    panel.desktop(&UiAction::CopyText(text));
                }
            }));
        let weak = Rc::downgrade(self);
        self.failed
            .log_link
            .connect_clicked(guarded!("export: open the log", move |_| {
                let Some(panel) = weak.upgrade() else { return };
                let log = match &panel.ctx.store.local().export_view.outcome {
                    Some(Outcome::Failed { view, .. }) => view.log.clone(),
                    _ => None,
                };
                if let Some(log) = log {
                    let path = panel.absolute(&log);
                    panel.desktop(&UiAction::OpenPath(path));
                }
            }));
    }

    // ------------------------------------------------------- opening

    /// `export.panel` (`PanelRequest::Toggle`): opens the panel or closes
    /// it; `export.show` (`Show`, a toast's Show export): opens it, and
    /// leaves an open one as it is.
    fn request(self: &Rc<Self>, request: PanelRequest) {
        let open = self.ctx.store.local().export_panel();
        match panel_step(open, request) {
            PanelStep::Open => self.open(),
            PanelStep::Close => self.close(),
            PanelStep::Stay => {}
        }
    }

    /// Opens the panel: Running while an export runs, else Setup.
    fn open(self: &Rc<Self>) {
        let state = self.ctx.store.state();
        let exporting = export_job(&state).is_some();
        if !exporting
            && (state.project.timeline.clips.is_empty() || !state.runtime.record.is_idle())
        {
            tracing::debug!("export panel: nothing to export now");
            return;
        }
        self.refresh_preview();
        let mode = flow::mode(exporting, self.waiting(&state), None);
        self.ctx.store.update_local(move |l| {
            l.export_view.draft.error = None;
            l.export_view.outcome = None;
            l.overlays.push(Overlay::ExportPanel(mode));
        });
    }

    /// Closes the panel (its outcome goes with it).
    fn close(&self) {
        self.ctx.store.update_local(|l| {
            l.close_overlays(|o| matches!(o, Overlay::ExportPanel(_)));
            l.export_view.outcome = None;
        });
    }

    /// The auto name at the local now (`QREC_GUI_TEST_NOW` in test mode),
    /// for both formats.
    fn refresh_preview(&self) {
        let state = self.ctx.store.state();
        let (now, offset) = match testctl::mode().now {
            Some(local) => (local.and_utc(), 0),
            None => {
                let now = Utc::now();
                (now, offset_at(now))
            }
        };
        *self.preview.borrow_mut() = (
            view::output_preview(&state, ExportFormat::Mp4, now, offset),
            view::output_preview(&state, ExportFormat::Webm, now, offset),
        );
    }

    /// Its own export left the job table and its answer is on its way.
    fn waiting(&self, state: &State) -> bool {
        self.own
            .borrow()
            .as_ref()
            .is_some_and(|own| own.started.is_some())
            && export_job(state).is_none()
    }

    // ------------------------------------------------------- following

    fn on_change(self: &Rc<Self>, change: &Change) {
        if let Some(job) = export_job(&change.next) {
            let mut own = self.own.borrow_mut();
            let foreign = own.is_none();
            if let Some(own) = own.as_mut() {
                if own.started.is_none() {
                    own.started = Some(Arc::clone(&change.next));
                }
                own.last = Some(job.clone());
            }
            drop(own);
            // Another export runs (the CLI's: its own left the table before
            // its answer came): the panel follows that one now, so the Done
            // or Failed page of its own last export is history. Kept, it
            // would come back when this one ends, and drop that one's toast
            // (`toasts::shown_by_the_panel`) instead of closing (D-49).
            if foreign && self.ctx.store.local().export_view.outcome.is_some() {
                self.ctx
                    .store
                    .update_local(|l| l.export_view.outcome = None);
            }
            if self.ctx.store.local().export_view.draft.starting {
                self.ctx
                    .store
                    .update_local(|l| l.export_view.draft.starting = false);
            }
        }
        self.sync();
    }

    /// Shows what the panel should show now (or nothing).
    fn sync(self: &Rc<Self>) {
        let state = self.ctx.store.state();
        let (open, outcome, starting) = {
            let local = self.ctx.store.local();
            (
                local.export_panel(),
                local.export_view.outcome.clone(),
                local.export_view.draft.starting,
            )
        };
        let Some(current) = open else {
            self.hide();
            return;
        };
        let exporting = export_job(&state).is_some();
        let mode = flow::mode(exporting, self.waiting(&state), outcome.as_ref());
        // The export it showed ended, and it wasn't the panel's: the core's
        // toast says how it went.
        if self.shown.get() == Some(ExportPanelMode::Running)
            && mode == ExportPanelMode::Setup
            && !starting
            && self.own.borrow().is_none()
        {
            self.close();
            return;
        }
        if mode != current {
            self.ctx.store.update_local(move |l| {
                for o in &mut l.overlays {
                    if let Overlay::ExportPanel(m) = o {
                        *m = mode;
                    }
                }
            });
            return;
        }
        self.show(mode, &state);
    }

    fn show(self: &Rc<Self>, mode: ExportPanelMode, state: &State) {
        // Under Settings or a modal opened over the passive panel, it waits
        // for them to close before it takes the trap (G§3.15).
        let on_top = matches!(
            self.ctx.store.local().top_overlay(),
            Some(Overlay::ExportPanel(_))
        );
        let layer = flow::layer(mode, on_top, self.layer.get());
        let page_changed = self.shown.replace(Some(mode)) != Some(mode);
        let name = match mode {
            ExportPanelMode::Setup => "setup",
            ExportPanelMode::Running => "running",
            ExportPanelMode::Done => "done",
            ExportPanelMode::Failed => "failed",
        };
        self.stack.set_visible_child_name(name);
        match mode {
            ExportPanelMode::Setup => self.fill_setup(),
            ExportPanelMode::Running => self.fill_running(state),
            ExportPanelMode::Done => self.fill_done(),
            ExportPanelMode::Failed => self.fill_failed(),
        }
        let trapped = self.layer.get() != Some(layer) && layer == Layer::FloatingTrap;
        if self.layer.get() != Some(layer) {
            if let Some(old) = self.layer.take() {
                self.ctx.window.close(old);
            }
            match layer {
                Layer::FloatingTrap => {
                    let weak = Rc::downgrade(self);
                    self.ctx.window.open_dismissable(&self.root, move || {
                        if let Some(panel) = weak.upgrade() {
                            panel.close();
                        }
                    });
                }
                _ => self.ctx.window.open(layer, &self.root),
            }
            self.layer.set(Some(layer));
        }
        // A page shown in the trap, or the trap taken (the Done page once
        // Settings over it closed): its primary has the focus.
        if (page_changed || trapped) && layer == Layer::FloatingTrap && on_top {
            // Enter activates the page's primary (it has the focus).
            let primary = match mode {
                ExportPanelMode::Setup => &self.setup.export,
                ExportPanelMode::Done => &self.done.open,
                _ => &self.failed.retry,
            };
            primary.grab_focus();
            // The trap focused the first focusable on the way (the stderr
            // label, which selects all of itself on focus): no selection.
            self.failed.stderr.select_region(0, 0);
        }
        self.tick(mode == ExportPanelMode::Running);
    }

    /// Closes the layer the panel is in.
    fn hide(self: &Rc<Self>) {
        if let Some(layer) = self.layer.take() {
            self.ctx.window.close(layer);
        }
        if self.shown.take().is_some() {
            for bar in [&self.running.bar, &self.done.bar, &self.failed.bar] {
                bar.reset();
            }
        }
        self.tick(false);
    }

    /// The running page's clock (elapsed, about … left), once a second.
    fn tick(self: &Rc<Self>, on: bool) {
        let mut ticker = self.ticker.borrow_mut();
        match (on, ticker.is_some()) {
            (true, false) => {
                let weak = Rc::downgrade(self);
                *ticker = Some(guard::timeout_local(
                    Duration::from_secs(1),
                    "export: the running clock",
                    move || {
                        let Some(panel) = weak.upgrade() else {
                            return glib::ControlFlow::Break;
                        };
                        let state = panel.ctx.store.state();
                        if panel.shown.get() == Some(ExportPanelMode::Running) {
                            panel.fill_running(&state);
                        }
                        glib::ControlFlow::Continue
                    },
                ));
            }
            (false, true) => {
                if let Some(id) = ticker.take() {
                    id.remove();
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------- pages

    fn fill_setup(&self) {
        let state = self.ctx.store.state();
        let draft = self.ctx.store.local().export_view.draft.clone();
        let page = &self.setup;
        self.syncing.set(true);
        let format = match draft.format {
            ExportFormat::Mp4 => "mp4",
            ExportFormat::Webm => "webm",
        };
        if page.format.active_name().as_deref() != Some(format) {
            page.format.set_active_name(Some(format));
        }
        let quality = match draft.quality {
            Quality::Draft => "draft",
            Quality::Standard => "standard",
            Quality::High => "high",
        };
        if page.quality.active_name().as_deref() != Some(quality) {
            page.quality.set_active_name(Some(quality));
        }
        let webm = draft.format == ExportFormat::Webm;
        page.av1_row.set_visible(webm);
        let unavailable = view::av1_unavailable(&state.runtime.capabilities);
        page.av1.set_sensitive(unavailable.is_none());
        page.av1.set_active(draft.av1 && unavailable.is_none());
        page.av1_caption
            .set_text(unavailable.as_deref().unwrap_or(view::AV1_CAPTION));
        self.syncing.set(false);
        let codec = if unavailable.is_none() {
            draft.codec()
        } else {
            None
        };
        page.caption.set_text(&view::caption(
            draft.format,
            codec,
            &state,
            &self.config.borrow(),
        ));
        let output = match &draft.chosen {
            Some(path) => self.shown_path(path),
            None => {
                let preview = self.preview.borrow();
                match draft.format {
                    ExportFormat::Mp4 => preview.0.clone(),
                    ExportFormat::Webm => preview.1.clone(),
                }
            }
        };
        page.output.set_text(&output);
        page.output.set_tooltip_text(Some(&output));
        match &draft.error {
            Some(error) => {
                page.field.add_css_class("invalid");
                page.helper.set_text(error);
                page.helper.set_visible(true);
            }
            None => {
                page.field.remove_css_class("invalid");
                page.helper.set_visible(false);
            }
        }
        page.summary.show(&view::summary_runs(&state));
        let can = !state.project.timeline.clips.is_empty()
            && state.runtime.record.is_idle()
            && export_job(&state).is_none();
        page.export.set_sensitive(can && !draft.starting);
        pages::set_loading(&page.export, draft.starting);
    }

    fn fill_running(&self, state: &State) {
        let Some(job) = export_job(state) else {
            // Its own export's answer is on its way: the page stays.
            return;
        };
        let Some(r) = view::running(job, Utc::now(), offset_at(job.started)) else {
            return;
        };
        let page = &self.running;
        page.file.set_text(&self.shown_path(&r.out));
        page.percent.set_text(&r.percent);
        page.label.set_text(&r.label);
        page.frames.set_text(&r.frames);
        page.times.set_text(&r.times);
        page.note.set_text(&r.note);
        page.bar
            .set_state(MudState::Running(f64::from(r.fraction) / 100.0));
    }

    fn fill_done(&self) {
        let Some(Outcome::Done { view, .. }) = self.ctx.store.local().export_view.outcome.clone()
        else {
            return;
        };
        self.done.path.set_text(&view.path);
        self.done.path.set_tooltip_text(Some(&view.path));
        self.done.details.show(&view.details_runs);
        self.done.bar.set_state(MudState::Done);
    }

    fn fill_failed(&self) {
        let Some(Outcome::Failed { view, percent, .. }) =
            self.ctx.store.local().export_view.outcome.clone()
        else {
            return;
        };
        let page = &self.failed;
        page.lead.set_text(&view.lead);
        page.hint.set_text(view.hint.as_deref().unwrap_or_default());
        page.hint.set_visible(view.hint.is_some());
        page.bar
            .set_state(MudState::Failed(f64::from(percent) / 100.0));
        page.at.set_text(&view.at);
        let text = view.stderr_tail.join("\n");
        if page.stderr.text() != text {
            page.stderr.set_text(&text);
            pin_to_end(&page.scroller);
        }
        page.scroller.set_visible(!view.stderr_tail.is_empty());
        let s = self.ctx.theme.scale();
        // 10 lines of `mono-sm` (16 px); the well's padding comes on top.
        page.scroller
            .set_max_content_height((f64::from(STDERR_VISIBLE) * 16.0 * s).round() as i32);
        match &view.log {
            Some(log) => {
                page.log_path.set_text(&self.shown_path(log));
                page.log_row.set_visible(true);
            }
            None => page.log_row.set_visible(false),
        }
    }

    // ------------------------------------------------------- actions

    /// Sends `cmd` as the panel's own export (`confirmed: false`, D-24).
    fn start(self: &Rc<Self>, cmd: Command) {
        self.send(cmd, false);
    }

    /// Sends `cmd` as the panel's own export, `confirmed` once the Replace
    /// modal was confirmed (D-24): either way its answer comes back here,
    /// so it shows Running, then Done or Failed.
    fn send(self: &Rc<Self>, cmd: Command, confirmed: bool) {
        *self.own.borrow_mut() = Some(Own {
            started: None,
            last: None,
        });
        self.ctx.store.update_local(|l| {
            let draft = &mut l.export_view.draft;
            draft.starting = true;
            draft.error = None;
            l.export_view.outcome = None;
        });
        let weak = Rc::downgrade(self);
        let sent = cmd.clone();
        self.ctx.store.dispatch_with(cmd, confirmed, move |reply| {
            if let Some(panel) = weak.upgrade() {
                panel.answered(&sent, reply);
            }
        });
    }

    /// The answer to its own export.
    fn answered(self: &Rc<Self>, cmd: &Command, reply: Reply) {
        let own = self.own.borrow_mut().take();
        let open = self.ctx.store.local().export_panel().is_some();
        let state = self.ctx.store.state();
        let started = own
            .as_ref()
            .and_then(|o| o.started.clone())
            .unwrap_or_else(|| Arc::clone(&state));
        let answer = flow::answer(cmd, &reply);
        tracing::info!("export panel: its export answered: {answer:?}");
        let outcome = match answer {
            Answer::Done { path, bytes } => {
                let took = export_job(&started).map_or(0, |job| {
                    (Utc::now() - job.started).num_seconds().max(0) as u64
                });
                let shown = self.shown_path(&path);
                open.then(|| Outcome::Done {
                    view: view::done(&started, &shown, bytes, took),
                    file: path,
                })
            }
            Answer::Failed {
                message,
                stderr_tail,
                log,
            } => {
                let out = match cmd {
                    Command::Export { out, .. } => out.clone(),
                    _ => None,
                };
                let (dir, home) = self.dirs();
                let folder = flow::folder_shown(out.as_deref(), &dir, home.as_deref());
                let view = view::failed(&FailedExport {
                    started: &started,
                    job: own.as_ref().and_then(|o| o.last.as_ref()),
                    message: &message,
                    stderr_tail: &stderr_tail,
                    log: log.as_deref(),
                    folder: &folder,
                });
                let percent = view.fraction;
                open.then(|| Outcome::Failed {
                    view,
                    retry: Box::new(cmd.clone()),
                    copy: flow::error_text(&message, &stderr_tail),
                    percent,
                })
            }
            Answer::Confirm(error) => {
                self.ctx
                    .store
                    .update_local(|l| l.export_view.draft.starting = false);
                // The Replace modal (WP6's): confirming it sends the export
                // again, confirmed, as the panel's own (D-24).
                let weak = Rc::downgrade(self);
                self.ctx
                    .store
                    .ask_confirmation(cmd.clone(), error, move |confirmed| {
                        if let Some(panel) = weak.upgrade() {
                            panel.send(confirmed, true);
                        }
                    });
                return;
            }
            Answer::Inline(message) => {
                self.ctx.store.update_local(move |l| {
                    let draft = &mut l.export_view.draft;
                    draft.starting = false;
                    draft.error = Some(message);
                });
                return;
            }
            Answer::Toast => {
                self.ctx
                    .store
                    .update_local(|l| l.export_view.draft.starting = false);
                if let ReplyAction::Toast(notice) = default_action(cmd, &reply, &state) {
                    self.ctx.store.notify(notice);
                }
                return;
            }
            Answer::Close => {
                self.ctx
                    .store
                    .update_local(|l| l.export_view.draft.starting = false);
                if open {
                    self.close();
                }
                return;
            }
        };
        self.ctx.store.update_local(move |l| {
            l.export_view.draft.starting = false;
            if l.export_panel().is_some() {
                l.export_view.outcome = outcome;
            }
        });
        // The panel may have shown Running for an answer on its way.
        self.sync();
    }

    fn done_action(&self, what: &str) {
        let Some(Outcome::Done { file, .. }) = self.ctx.store.local().export_view.outcome.clone()
        else {
            return;
        };
        let action = match what {
            "open" => UiAction::OpenPath(file),
            "folder" => UiAction::RevealPath(file),
            _ => UiAction::CopyText(file),
        };
        self.desktop(&action);
    }

    fn desktop(&self, action: &UiAction) {
        let window = self.root.root().and_downcast::<gtk::Window>();
        desktop::run(
            &self.ctx,
            window.as_ref(),
            self.ctx.store.project_dir(),
            action,
        );
    }

    /// "Change…" (D-24): the save dialog on `exports/` with the auto name;
    /// in test mode the next of `QREC_GUI_TEST_SAVE_AS`'s paths.
    fn choose_output(self: &Rc<Self>) {
        if testctl::mode().on {
            let path = self.test_paths.borrow_mut().pop_front();
            if let Some(path) = path {
                tracing::info!("export panel: test mode picks {path}");
                self.chosen(path);
            }
            return;
        }
        let dir = self.ctx.store.project_dir().join("exports");
        let draft = self.ctx.store.local().export_view.draft.clone();
        let name = {
            let preview = self.preview.borrow();
            let auto = match draft.format {
                ExportFormat::Mp4 => &preview.0,
                ExportFormat::Webm => &preview.1,
            };
            Path::new(auto)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let dialog = gtk::FileDialog::builder()
            .title("Export to")
            .modal(true)
            .initial_name(name)
            .build();
        let folder = if dir.is_dir() {
            dir
        } else {
            self.ctx.store.project_dir().to_path_buf()
        };
        dialog.set_initial_folder(Some(&gtk::gio::File::for_path(folder)));
        let window = self.root.root().and_downcast::<gtk::Window>();
        let weak = Rc::downgrade(self);
        dialog.save(
            window.as_ref(),
            gtk::gio::Cancellable::NONE,
            guarded!("export: the save dialog", move |result| {
                let Some(panel) = weak.upgrade() else { return };
                match result {
                    Ok(file) => match file.path() {
                        Some(path) => panel.chosen(path.to_string_lossy().into_owned()),
                        None => tracing::warn!("export panel: the picked file has no path"),
                    },
                    Err(e) => tracing::debug!("export panel: no file picked: {e}"),
                }
            }),
        );
    }

    fn chosen(&self, path: String) {
        self.ctx.store.update_local(move |l| {
            let draft = &mut l.export_view.draft;
            draft.chosen = Some(path);
            draft.error = None;
        });
    }

    // ------------------------------------------------------- paths

    /// The project folder and the home folder, as text.
    fn dirs(&self) -> (String, Option<String>) {
        (
            self.ctx.store.project_dir().to_string_lossy().into_owned(),
            std::env::var("HOME").ok(),
        )
    }

    /// `path` as people read it (`exports/…`, `~/…`).
    fn shown_path(&self, path: &str) -> String {
        let (dir, home) = self.dirs();
        format::display_path(path, &dir, home.as_deref())
    }

    /// `path`, absolute (project-relative ones under the project folder).
    fn absolute(&self, path: &str) -> String {
        if Path::new(path).is_absolute() {
            return path.to_owned();
        }
        let full: PathBuf = self.ctx.store.project_dir().join(path);
        full.to_string_lossy().into_owned()
    }
}
