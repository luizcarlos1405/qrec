//! The project picker (SPEC §4.4, G§4.12): the window `qrec` shows when it
//! has no project to open. Owner: meta.
//!
//! It is its own small window and has none of the editor's machinery: no
//! lock, no host, no GStreamer. Picking a project replaces this process
//! with `qrec --project <dir>` (`exec`), so the project goes through the one
//! startup path there is (G§3.3), logs to its own log, and the shell still
//! waits for the window and gets its exit code. From a running window
//! ("Open or create a project…") the picker is a process of its own
//! ([`spawn`]), so what that window is doing carries on.
//!
//! Nothing that touches the disk runs on the GTK thread (R1): the folders
//! are looked at, and new ones made, on `qrec-picker-io`. Content:
//! `view::picker`. Look: `components/picker.css` (S21's and X2's patterns;
//! no frame in `design/qrec.pen` yet).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::rc::Rc;
use std::thread;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use qrec_core::CoreError;
use qrec_core::config::{ProjectsDefaults, UserConfig};
use qrec_core::error::NOT_MY_SWAMP;
use qrec_core::keymap::{self, Keymap};
use qrec_core::new_project;
use qrec_core::project_folders;
use qrec_core::recents::RecentProjects;
use qrec_core::view::format;
use qrec_core::view::picker::{self, FolderState, NewProjectHint, PickerRow};
use qrec_shell::config_file;
use qrec_shell::paths::{Env, UserDirs};
use qrec_shell::projects::{self, ProjectFolder};

use crate::app::{self, APP_ID};
use crate::theme::art::Art;
use crate::theme::{MotionPolicy, Theme};
use crate::widgets::button::{self, Kind};
use crate::widgets::keycap::{CapSize, CapVariant, KeyCap};
use crate::widgets::spinner::Spinner;
use crate::{guard, guarded};

/// The window's GUI action for "Open or create a project…".
pub const OPEN_ACTION: &str = "projects.open";

/// The window's default size, and the smallest it goes.
const DEFAULT_SIZE: (i32, i32) = (760, 640);
const MIN_SIZE: (i32, i32) = (480, 420);
/// The content's width at 100 %.
const COLUMN: f64 = 760.0;
/// How long the folders may take to be looked at before a row still
/// waiting says "Checking…" (the board's checking state): most answer at
/// once, and a spinner on every row at start would only flicker.
const SLOW_CHECK_MS: u64 = 500;

/// How the picker was started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerArgs {
    /// Where a relative new-project path starts; `None` when the current
    /// directory can't be read.
    pub cwd: Option<PathBuf>,
    /// `-v` count, passed on to the project's window.
    pub verbose: u8,
}

thread_local! {
    /// The picker (main thread): what the worker's results come back to.
    static CURRENT: RefCell<Option<Rc<Picker>>> = const { RefCell::new(None) };
    /// This window wasn't asked for by folder ([`offered`]).
    static OFFERED: Cell<bool> = const { Cell::new(false) };
}

/// Notes that this window's project was Qrec's choice, not `--project`.
pub fn set_offered(offered: bool) {
    OFFERED.with(|o| o.set(offered));
}

/// A project that can't open offers the picker (X3): a window Qrec chose to
/// open must never be a dead end; with `--project` the page stays as drawn.
pub fn offered() -> bool {
    OFFERED.with(Cell::get)
}

/// Runs the picker until it closes (`crate::run_picker`).
pub fn run(args: PickerArgs) -> Result<u8, CoreError> {
    app::init_toolkit()?;
    let application = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application.connect_startup(guarded!("the picker's startup", move |_| app::on_startup()));
    let args = RefCell::new(Some(args));
    application.connect_activate(guarded!("the picker's activate", move |application| {
        let args = args.borrow_mut().take();
        if let Some(args) = args {
            activate(application, args);
        }
    }));
    application.run_with_args::<&str>(&[]);
    CURRENT.with(|c| c.borrow_mut().take());
    Ok(0)
}

/// Opens the picker in a process of its own, from a running window. GLib
/// reaps it when it ends; nothing waits for it.
pub fn spawn() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let argv = [exe.as_os_str(), OsStr::new("--pick")];
    let flags = gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE;
    let child = gio::Subprocess::newv(&argv, flags).map_err(|e| e.to_string())?;
    child.wait_async(
        gio::Cancellable::NONE,
        guarded!("the picker's process", move |_| {}),
    );
    Ok(())
}

/// GTK `activate`: the settings and the list, the look, the window.
fn activate(application: &adw::Application, args: PickerArgs) {
    let env = Env::from_process();
    let dirs = UserDirs::resolve(&env).ok();
    let config = dirs.as_ref().map_or_else(UserConfig::default, |dirs| {
        config_file::read(&dirs.config_file()).0
    });
    let theme = Theme::new(
        config.theme,
        config.ui.scale,
        MotionPolicy::from_settings(config.ui.reduced_motion),
    );
    app::install_look(&theme);
    let recents = dirs
        .as_ref()
        .map_or_else(RecentProjects::default, projects::read_recents);
    let home = env.get("HOME").map(|h| h.to_string_lossy().into_owned());
    let picker = Picker::new(application, args, dirs, config, recents, home, theme);
    CURRENT.with(|c| *c.borrow_mut() = Some(Rc::clone(&picker)));
    picker.connect();
    picker.refresh_rows();
    picker.refresh_hint();
    picker.window.present();
    picker.search.grab_focus();
    tracing::info!("the project picker is up");
    picker.check_folders();
    let weak = Rc::downgrade(&picker);
    guard::timeout_local_once(
        std::time::Duration::from_millis(SLOW_CHECK_MS),
        "the picker: slow folders",
        move || {
            if let Some(picker) = weak.upgrade() {
                picker.slow_checks.set(true);
                let waiting = picker
                    .shown
                    .borrow()
                    .iter()
                    .any(|row| row.state == FolderState::Checking);
                if waiting {
                    picker.refresh_rows();
                }
            }
        },
    );
}

/// `name` with the characters a search matched (`at`, in chars) in bold.
fn name_markup(name: &str, at: &[usize]) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        let text = glib::markup_escape_text(&c.to_string());
        if at.contains(&i) {
            out.push_str("<span weight=\"800\">");
            out.push_str(&text);
            out.push_str("</span>");
        } else {
            out.push_str(&text);
        }
    }
    out
}

/// `work` on `qrec-picker-io`, then `then` on the GTK thread with what it
/// returned (dropped when the picker is gone).
fn background<T: Send + 'static>(
    what: &'static str,
    work: impl FnOnce() -> T + Send + 'static,
    then: impl FnOnce(&Rc<Picker>, T) + Send + 'static,
) {
    let spawned = thread::Builder::new()
        .name("qrec-picker-io".into())
        .spawn(move || {
            let done = work();
            guard::invoke(what, move || {
                if let Some(picker) = CURRENT.with(|c| c.borrow().clone()) {
                    then(&picker, done);
                }
            });
        });
    if let Err(e) = spawned {
        tracing::error!("can't start the picker's worker: {e}");
    }
}

/// Replaces this process with `dir`'s window. Only returns on failure.
fn exec_project(dir: &Path, verbose: u8, token: Option<String>) -> io::Error {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return e,
    };
    let mut process = Process::new(exe);
    process.arg("--project").arg(dir);
    for _ in 0..verbose {
        process.arg("-v");
    }
    // GTK took the one this process was started with: the new window asks
    // the compositor for the focus with this one.
    if let Some(token) = token {
        process.env("XDG_ACTIVATION_TOKEN", token);
    }
    process.exec()
}

/// The two pages of the window.
const LIST: &str = "list";
const NEW: &str = "new";

/// The picker's window.
struct Picker {
    window: adw::ApplicationWindow,
    args: PickerArgs,
    dirs: Option<UserDirs>,
    home: Option<String>,
    keymap: Keymap,
    /// Kept for the window's life: its providers and its art.
    _theme: Theme,
    /// The project folders (SPEC §4.4), as the file held them last.
    projects: RefCell<ProjectsDefaults>,
    /// The folder chosen in the menu for this project; `None`: the default.
    /// Choosing never changes the default (only Make default does).
    chosen: RefCell<Option<String>>,
    recents: RefCell<RecentProjects>,
    /// What each recent folder turned out to be.
    states: RefCell<BTreeMap<String, FolderState>>,
    /// The rows the list shows, in order.
    shown: RefCell<Vec<PickerRow>>,
    stack: gtk::Stack,
    new_button: gtk::Button,
    open_button: gtk::Button,
    here: gtk::Box,
    here_path: gtk::Label,
    here_button: gtk::Button,
    error: gtk::Label,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    /// "Recent" and the count over the list (without a search).
    recent: gtk::Box,
    recent_count: gtk::Label,
    /// No projects at all: C31's empty state (the quiet swamp).
    empty: gtk::Box,
    /// A search that matches nothing.
    no_match: gtk::Label,
    /// The folders took longer than [`SLOW_CHECK_MS`]: waiting rows say so.
    slow_checks: Cell<bool>,
    entry: gtk::Entry,
    hint: gtk::Label,
    /// "in 📁 work ▾": the folder a name goes in, and its menu.
    folder_button: gtk::MenuButton,
    folder_name: gtk::Label,
    folder_popover: gtk::Popover,
    folder_rows: gtk::ListBox,
    /// "Add a project folder…", while there are none.
    folder_link: gtk::Button,
    create: gtk::Button,
    cancel: gtk::Button,
    new_error: gtk::Label,
}

impl Picker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        application: &adw::Application,
        args: PickerArgs,
        dirs: Option<UserDirs>,
        config: UserConfig,
        recents: RecentProjects,
        home: Option<String>,
        theme: Theme,
    ) -> Rc<Self> {
        let keymap = keymap::resolve("");
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Qrec")
            .default_width(DEFAULT_SIZE.0)
            .default_height(DEFAULT_SIZE.1)
            .width_request(MIN_SIZE.0)
            .height_request(MIN_SIZE.1)
            .build();
        window.set_widget_name("qrec-picker");
        window.add_css_class("qrec");
        window.add_css_class("qrec-picker");
        if config.ui.reduced_motion {
            window.add_css_class("reduced-motion");
        }

        // The list page.
        let heading = label("Projects", &["t-display-sm"]);
        heading.set_hexpand(true);
        let new_button = button::labeled(
            "New project…",
            Some("qrec-plus-symbolic"),
            None,
            &keymap,
            Kind::Primary,
        );
        new_button.set_widget_name("picker-new");
        let open_button = button::labeled(
            "Open folder…",
            Some("qrec-folder-open-symbolic"),
            None,
            &keymap,
            Kind::Secondary,
        );
        open_button.set_widget_name("picker-open-folder");
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        head.add_css_class("picker-head");
        head.append(&heading);
        head.append(&open_button);
        head.append(&new_button);

        let here = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        here.set_widget_name("picker-here");
        here.add_css_class("picker-here");
        here.set_visible(false);
        let here_words = gtk::Box::new(gtk::Orientation::Vertical, 0);
        here_words.set_hexpand(true);
        here_words.set_valign(gtk::Align::Center);
        here_words.append(&label("This folder is empty", &["t-body"]));
        let here_path = label("", &["t-caption", "muted"]);
        here_path.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        here_words.append(&here_path);
        here.append(&gtk::Image::from_icon_name("qrec-folder-symbolic"));
        here.append(&here_words);
        let here_button =
            button::labeled("Start a project here", None, None, &keymap, Kind::Secondary);
        here_button.set_widget_name("picker-start-here");
        here_button.set_valign(gtk::Align::Center);
        here.append(&here_button);

        let error = wrapping(&["picker-error"]);
        error.set_widget_name("picker-error");
        error.set_visible(false);

        let search = gtk::SearchEntry::new();
        search.set_widget_name("picker-search");
        search.add_css_class("qrec-search");
        search.set_placeholder_text(Some("Search projects"));
        search.update_property(&[gtk::accessible::Property::Label("Search projects")]);

        let list = gtk::ListBox::new();
        list.set_widget_name("picker-list");
        list.add_css_class("picker-list");
        list.set_selection_mode(gtk::SelectionMode::Single);
        // The list's placeholder: the empty state, or a search's no match.
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 0);
        empty.set_widget_name("picker-empty");
        empty.add_css_class("picker-empty");
        empty.set_valign(gtk::Align::Center);
        empty.set_vexpand(true);
        if let Some(svg) = theme.art(Art::QuietSwamp) {
            // 200×120, as the art is drawn (`rfUfx`), times the UI scale.
            let art = gtk::Picture::for_paintable(&svg);
            art.set_can_shrink(true);
            art.set_content_fit(gtk::ContentFit::Contain);
            let scale = theme.scale();
            art.set_size_request(
                (200.0 * scale).round() as i32,
                (120.0 * scale).round() as i32,
            );
            art.set_widget_name("picker-empty-art");
            // In a column of its width: a picture is as tall as the width it
            // gets (the timeline's empty state does the same).
            let column = crate::timeline::root::Column::new(&art, (200.0 * scale).round() as i32);
            column.set_halign(gtk::Align::Center);
            empty.append(&column);
        }
        let empty_title = label("No projects yet", &["t-display-sm"]);
        empty_title.set_xalign(0.5);
        empty.append(&empty_title);
        let empty_body = wrapping(&["t-body", "muted"]);
        empty_body.set_text(
            "Make a new one, or open a folder that holds a qrec.json. Projects you open show up here.",
        );
        empty_body.set_xalign(0.5);
        empty_body.set_justify(gtk::Justification::Center);
        empty.append(&empty_body);
        let no_match = wrapping(&["picker-no-match", "t-body", "muted"]);
        no_match.set_widget_name("picker-placeholder");
        no_match.set_text("No project matches that.");
        no_match.set_xalign(0.5);
        no_match.set_justify(gtk::Justification::Center);
        let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        placeholder.append(&empty);
        placeholder.append(&no_match);
        list.set_placeholder(Some(&placeholder));
        let recent = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        recent.set_widget_name("picker-recent");
        recent.add_css_class("picker-recent");
        recent.append(&label("Recent", &["t-caption-strong", "muted"]));
        let recent_count = label("", &["t-caption", "muted"]);
        recent.append(&recent_count);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&list));

        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.add_css_class("picker-page");
        page.append(&head);
        page.append(&here);
        page.append(&error);
        page.append(&search);
        page.append(&recent);
        page.append(&scroller);
        let list_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list_page.append(&column(&page, theme.scale()));
        list_page.append(&hints(&[
            (&["Enter"], "Open"),
            (&["Ctrl", "N"], "New project"),
            (&["Ctrl", "O"], "Open folder"),
            (&["Ctrl", "Q"], "Quit"),
        ]));

        // The New project page.
        let title = label("New project", &["t-display-sm"]);
        let field_label = label("Name or path", &["t-body"]);
        let entry = gtk::Entry::new();
        entry.set_widget_name("picker-new-entry");
        entry.set_placeholder_text(Some("demo"));
        entry.update_property(&[gtk::accessible::Property::Label("Name or path")]);
        let hint = wrapping(&["t-body-sm", "muted", "picker-new-hint"]);
        hint.set_widget_name("picker-new-hint");
        let new_error = wrapping(&["picker-error"]);
        new_error.set_widget_name("picker-new-error");
        new_error.set_visible(false);
        // The folder menu (design/boards/09-project-folders).
        let folder_rows = gtk::ListBox::new();
        folder_rows.set_widget_name("picker-folder-rows");
        folder_rows.add_css_class("picker-folder-rows");
        folder_rows.set_selection_mode(gtk::SelectionMode::None);
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        menu.add_css_class("picker-folder-menu");
        let menu_label = label("Project folders", &["t-caption-strong", "muted"]);
        menu_label.add_css_class("picker-folder-menu-label");
        menu.append(&menu_label);
        menu.append(&folder_rows);
        let folder_popover = gtk::Popover::new();
        folder_popover.set_has_arrow(false);
        folder_popover.set_position(gtk::PositionType::Bottom);
        folder_popover.add_css_class("qrec-popover");
        folder_popover.set_child(Some(&menu));
        let folder_name = label("", &["t-body-strong"]);
        folder_name.set_widget_name("picker-folder-name");
        let folder_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        folder_content.append(&label("in", &["t-body", "muted"]));
        folder_content.append(&gtk::Image::from_icon_name("qrec-folder-symbolic"));
        folder_content.append(&folder_name);
        folder_content.append(&gtk::Image::from_icon_name("qrec-chevron-down-symbolic"));
        let folder_button = button::popover_button(
            &folder_popover,
            &folder_content,
            &["secondary", "picker-folder-button"],
            "Project folder",
        );
        folder_button.set_widget_name("picker-folder");
        folder_button.set_valign(gtk::Align::Center);
        let field_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        entry.set_hexpand(true);
        field_line.append(&entry);
        field_line.append(&folder_button);
        let folder_link = button::link("Add a project folder…");
        folder_link.set_widget_name("picker-projects-folder");
        folder_link.set_halign(gtk::Align::Start);
        let create = button::labeled("Create", None, None, &keymap, Kind::Primary);
        create.set_widget_name("picker-create");
        // Enter creates (a fixed key, not a keys.toml binding: G§4.12).
        if let Some(content) = create.child().and_downcast::<gtk::Box>() {
            button::show_binding_cap(
                &content,
                Some(&["Enter".to_owned()]),
                "picker-create-cap",
                CapSize::Md,
                CapVariant::OnFill,
            );
        }
        create.update_property(&[gtk::accessible::Property::Label("Create, Enter")]);
        let cancel = button::labeled("Cancel", None, None, &keymap, Kind::Secondary);
        cancel.set_widget_name("picker-cancel");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        actions.add_css_class("picker-head");
        actions.set_halign(gtk::Align::End);
        actions.append(&cancel);
        actions.append(&create);
        let form = gtk::Box::new(gtk::Orientation::Vertical, 0);
        form.add_css_class("picker-page");
        form.append(&title);
        form.append(&field_label);
        form.append(&field_line);
        form.append(&hint);
        form.append(&new_error);
        form.append(&folder_link);
        form.append(&actions);
        let new_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        new_page.append(&column(&form, theme.scale()));
        new_page.append(&hints(&[
            (&["Enter"], "Create"),
            (&["Alt", "↓"], "Folder"),
            (&["Esc"], "Back"),
            (&["Ctrl", "Q"], "Quit"),
        ]));

        let stack = gtk::Stack::new();
        stack.add_named(&list_page, Some(LIST));
        stack.add_named(&new_page, Some(NEW));
        stack.set_visible_child_name(LIST);
        stack.set_vexpand(true);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&crate::window::minimal_header(&theme, &keymap));
        view.set_content(Some(&stack));
        window.set_content(Some(&view));

        Rc::new(Self {
            window,
            args,
            dirs,
            home,
            keymap,
            _theme: theme,
            projects: RefCell::new(config.projects),
            chosen: RefCell::new(None),
            recents: RefCell::new(recents),
            states: RefCell::new(BTreeMap::new()),
            shown: RefCell::new(Vec::new()),
            stack,
            new_button,
            open_button,
            here,
            here_path,
            here_button,
            error,
            search,
            list,
            recent,
            recent_count,
            empty,
            no_match,
            slow_checks: Cell::new(false),
            entry,
            hint,
            folder_button,
            folder_name,
            folder_popover,
            folder_rows,
            folder_link,
            create,
            cancel,
            new_error,
        })
    }

    /// The signals. Each holds the picker weakly: the window owns nothing
    /// that keeps it alive.
    fn connect(self: &Rc<Self>) {
        let on = |f: fn(&Rc<Self>)| {
            let weak = Rc::downgrade(self);
            move || {
                if let Some(picker) = weak.upgrade() {
                    f(&picker);
                }
            }
        };
        let run = on(Self::show_new);
        self.new_button
            .connect_clicked(guarded!("the picker: New project", move |_| run()));
        let run = on(Self::choose_folder);
        self.open_button
            .connect_clicked(guarded!("the picker: Open folder", move |_| run()));
        let run = on(Self::start_here);
        self.here_button
            .connect_clicked(guarded!("the picker: Start here", move |_| run()));
        let run = on(Self::show_list);
        self.cancel
            .connect_clicked(guarded!("the picker: Cancel", move |_| run()));
        let run = on(Self::create_project);
        self.create
            .connect_clicked(guarded!("the picker: Create", move |_| run()));
        let run = on(Self::create_project);
        self.entry
            .connect_activate(guarded!("the picker: Enter in the field", move |_| run()));
        let run = on(Self::refresh_hint);
        self.entry
            .connect_changed(guarded!("the picker: the field", move |_| run()));
        let run = on(Self::add_project_folder);
        self.folder_link
            .connect_clicked(guarded!("the picker: add a project folder", move |_| run()));
        // The menu opens on the chosen folder, so ↑ ↓ Enter start there.
        let weak = Rc::downgrade(self);
        self.folder_popover
            .connect_map(guarded!("the picker: the folder menu opens", move |_| {
                let Some(picker) = weak.upgrade() else { return };
                let want = format!("picker-folder-row-{}", picker.folder_name.text());
                let mut child = picker.folder_rows.first_child();
                while let Some(row) = child {
                    if row.widget_name() == want {
                        row.grab_focus();
                        break;
                    }
                    child = row.next_sibling();
                }
            }));
        let weak = Rc::downgrade(self);
        self.folder_rows.connect_row_activated(guarded!(
            "the picker: a project folder",
            move |_, row| {
                if let Some(picker) = weak.upgrade() {
                    picker.folder_row_activated(row);
                }
            }
        ));
        let run = on(Self::refresh_rows);
        self.search
            .connect_search_changed(guarded!("the picker: the search", move |_| run()));
        let run = on(Self::open_selected);
        self.search
            .connect_activate(guarded!("the picker: Enter in the search", move |_| run()));
        let weak = Rc::downgrade(self);
        self.list
            .connect_row_activated(guarded!("the picker: a row", move |_, row| {
                if let Some(picker) = weak.upgrade() {
                    picker.open_row(row.index());
                }
            }));
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(guarded!(
            "the picker: a key",
            after_panic = glib::Propagation::Proceed,
            move |_, key, _, modifiers| match weak.upgrade() {
                Some(picker) => picker.on_key(key, modifiers),
                None => glib::Propagation::Proceed,
            }
        ));
        self.window.add_controller(keys);
    }

    fn on_key(self: &Rc<Self>, key: gdk::Key, modifiers: gdk::ModifierType) -> glib::Propagation {
        let on_list = self.stack.visible_child_name().as_deref() == Some(LIST);
        let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        match key {
            gdk::Key::q if ctrl => self.window.close(),
            gdk::Key::n if ctrl && on_list => self.show_new(),
            gdk::Key::o if ctrl && on_list => self.choose_folder(),
            gdk::Key::Down if on_list => self.move_selection(1),
            gdk::Key::Up if on_list => self.move_selection(-1),
            gdk::Key::Down
                if !on_list
                    && modifiers.contains(gdk::ModifierType::ALT_MASK)
                    && self.folder_button.is_visible()
                    && self.folder_button.is_sensitive() =>
            {
                self.folder_button.popup();
            }
            gdk::Key::Escape if !on_list && self.folder_popover.is_visible() => {
                self.folder_popover.popdown();
            }
            gdk::Key::Escape if !on_list => self.show_list(),
            gdk::Key::Escape if !self.search.text().is_empty() => self.search.set_text(""),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    // ------------------------------------------------------------- the list

    /// Rebuilds the rows for the search, the first that can be opened
    /// selected.
    fn refresh_rows(self: &Rc<Self>) {
        let query = self.search.text();
        let now = chrono::Local::now().fixed_offset();
        let rows = {
            let states = self.states.borrow();
            picker::rows(
                &self.recents.borrow(),
                |dir| states.get(dir).copied().unwrap_or(FolderState::Checking),
                &query,
                self.home.as_deref(),
                now,
            )
        };
        let searching = !query.trim().is_empty();
        self.empty.set_visible(!searching);
        self.no_match.set_visible(searching);
        self.recent.set_visible(!searching && !rows.is_empty());
        self.recent_count.set_text(&rows.len().to_string());
        // One by one: `remove_all` takes the placeholder as well.
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        for row in &rows {
            self.list.append(&self.row_widget(row));
        }
        let first = rows.iter().position(PickerRow::openable);
        *self.shown.borrow_mut() = rows;
        self.list.select_row(
            first
                .and_then(|i| self.list.row_at_index(i as i32))
                .as_ref(),
        );
    }

    fn row_widget(self: &Rc<Self>, row: &PickerRow) -> gtk::ListBoxRow {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        line.add_css_class("picker-row");
        // A gone folder wears the missing-media pattern (DESIGN, C09).
        let folder = if row.openable() {
            let icon = gtk::Image::from_icon_name("qrec-folder-symbolic");
            icon.add_css_class("muted");
            icon
        } else {
            let icon = gtk::Image::from_icon_name("qrec-triangle-alert-symbolic");
            icon.add_css_class("warning-text");
            icon
        };
        folder.set_valign(gtk::Align::Center);
        line.append(&folder);
        let words = gtk::Box::new(gtk::Orientation::Vertical, 0);
        words.set_hexpand(true);
        words.set_valign(gtk::Align::Center);
        let name = label("", &["t-body", "picker-name"]);
        name.set_markup(&name_markup(&row.name, &row.matches));
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        words.append(&name);
        let path = label(&row.path, &["t-mono-sm", "muted", "picker-path"]);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        words.append(&path);
        line.append(&words);
        let checking = row.state == FolderState::Checking && self.slow_checks.get();
        if checking {
            let spinner = Spinner::new(12);
            spinner.set_valign(gtk::Align::Center);
            spinner.add_css_class("muted");
            line.append(&spinner);
        }
        let detail = label(
            if checking { "Checking…" } else { &row.detail },
            &["t-caption", "muted", "picker-detail"],
        );
        detail.set_valign(gtk::Align::Center);
        if !row.openable() {
            detail.remove_css_class("muted");
            detail.add_css_class("warning-text");
        }
        line.append(&detail);
        let forget = button::icon("qrec-x-symbolic", "Remove from list", None, &self.keymap);
        forget.add_css_class("picker-forget");
        forget.set_valign(gtk::Align::Center);
        let weak = Rc::downgrade(self);
        let dir = row.dir.clone();
        forget.connect_clicked(guarded!("the picker: remove from list", move |_| {
            if let Some(picker) = weak.upgrade() {
                picker.forget(&dir);
            }
        }));
        line.append(&forget);
        if !row.openable() {
            line.add_css_class("gone");
        }
        let list_row = gtk::ListBoxRow::new();
        list_row.set_child(Some(&line));
        list_row.set_activatable(row.openable());
        list_row.set_selectable(row.openable());
        list_row.update_property(&[gtk::accessible::Property::Label(&format!(
            "{}, {}, {}",
            row.name, row.path, row.detail
        ))]);
        list_row
    }

    /// Moves the selection, skipping what can't be opened.
    fn move_selection(&self, by: i32) {
        let current = self.list.selected_row().map_or(-1, |row| row.index());
        let openable: Vec<i32> = (0i32..)
            .zip(self.shown.borrow().iter())
            .filter(|(_, row)| row.openable())
            .map(|(i, _)| i)
            .collect();
        let next = if by > 0 {
            openable.iter().find(|i| **i > current)
        } else {
            openable.iter().rev().find(|i| **i < current)
        };
        if let Some(row) = next.and_then(|i| self.list.row_at_index(*i)) {
            self.list.select_row(Some(&row));
            // Scrolls it into view without taking the focus from the search.
            let bounds = row.compute_bounds(&self.list);
            if let (Some(bounds), Some(adjustment)) = (bounds, self.list.adjustment()) {
                adjustment.clamp_page(
                    f64::from(bounds.y()),
                    f64::from(bounds.y() + bounds.height()),
                );
            }
        }
    }

    fn open_selected(self: &Rc<Self>) {
        if let Some(row) = self.list.selected_row() {
            self.open_row(row.index());
        }
    }

    fn open_row(self: &Rc<Self>, index: i32) {
        let dir = usize::try_from(index).ok().and_then(|i| {
            let shown = self.shown.borrow();
            shown
                .get(i)
                .filter(|row| row.openable())
                .map(|row| row.dir.clone())
        });
        if let Some(dir) = dir {
            self.open(PathBuf::from(dir));
        }
    }

    fn forget(self: &Rc<Self>, dir: &str) {
        self.recents.borrow_mut().forget(dir);
        self.refresh_rows();
        if let Some(dirs) = self.dirs.clone() {
            let dir = dir.to_owned();
            background(
                "the picker: forgot a project",
                move || projects::forget(&dirs, &dir),
                |_, ()| {},
            );
        }
    }

    /// Finds out what the recent folders are, and whether a project can
    /// start in the current one.
    fn check_folders(self: &Rc<Self>) {
        let list: Vec<String> = self
            .recents
            .borrow()
            .projects
            .iter()
            .map(|p| p.dir.clone())
            .collect();
        let cwd = self.args.cwd.clone();
        background(
            "the picker: the folders",
            move || {
                let states: BTreeMap<String, FolderState> = list
                    .into_iter()
                    .map(|dir| {
                        let state = match projects::project_folder(Path::new(&dir)) {
                            ProjectFolder::Project | ProjectFolder::LostFile => FolderState::Ready,
                            ProjectFolder::Empty | ProjectFolder::NotProject => FolderState::Gone,
                        };
                        (dir, state)
                    })
                    .collect();
                let empty_here =
                    cwd.filter(|cwd| projects::project_folder(cwd) == ProjectFolder::Empty);
                (states, empty_here)
            },
            |picker, (states, empty_here)| {
                *picker.states.borrow_mut() = states;
                picker.refresh_rows();
                if let Some(cwd) = empty_here {
                    picker
                        .here_path
                        .set_text(&picker.shown_path(&cwd.to_string_lossy()));
                    picker.here.set_visible(true);
                }
            },
        );
    }

    fn shown_path(&self, path: &str) -> String {
        format::display_path(path, "", self.home.as_deref())
    }

    // ---------------------------------------------------------- opening one

    /// Looks at the folder, then opens it, or says why it can't.
    fn open(self: &Rc<Self>, dir: PathBuf) {
        self.set_busy(true);
        background(
            "the picker: a folder to open",
            move || {
                let kind = projects::project_folder(&dir);
                (dir, kind)
            },
            |picker, (dir, kind)| match kind {
                ProjectFolder::NotProject => {
                    picker.set_busy(false);
                    picker.show_error(&format!(
                        "{}: {}",
                        NOT_MY_SWAMP,
                        picker.shown_path(&dir.to_string_lossy())
                    ));
                }
                _ => picker.become_project(&dir),
            },
        );
    }

    fn start_here(self: &Rc<Self>) {
        if let Some(cwd) = self.args.cwd.clone() {
            self.open(cwd);
        }
    }

    fn choose_folder(self: &Rc<Self>) {
        self.clear_errors();
        let dialog = gtk::FileDialog::builder()
            .title("Open a project folder")
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        dialog.select_folder(
            Some(&self.window),
            gio::Cancellable::NONE,
            guarded!("the picker: the folder dialog", move |result| {
                let Some(picker) = weak.upgrade() else { return };
                match result {
                    Ok(folder) => match folder.path() {
                        Some(dir) => picker.open(dir),
                        None => picker.show_error("That folder isn't on this computer."),
                    },
                    Err(e) => tracing::debug!("the picker: no folder picked: {e}"),
                }
            }),
        );
    }

    /// Replaces this process with the project's window. Only comes back
    /// when that couldn't happen.
    fn become_project(&self, dir: &Path) {
        tracing::info!("the picker opens {}", dir.display());
        let error = exec_project(dir, self.args.verbose, self.activation_token());
        tracing::error!("can't open {}: {error}", dir.display());
        self.set_busy(false);
        let message = format!(
            "Couldn't open {}: {error}",
            self.shown_path(&dir.to_string_lossy())
        );
        self.show_error(&message);
        self.new_error.set_text(&message);
        self.new_error.set_visible(true);
    }

    /// A token for the window that replaces this one to ask for the focus
    /// (`xdg_activation`; GDK has it from the compositor).
    fn activation_token(&self) -> Option<String> {
        let context = WidgetExt::display(&self.window).app_launch_context();
        context
            .startup_notify_id(None::<&gio::AppInfo>, &[])
            .map(|token| token.to_string())
    }

    // ---------------------------------------------------------- new project

    fn show_new(self: &Rc<Self>) {
        self.clear_errors();
        self.stack.set_visible_child_name(NEW);
        self.entry.grab_focus();
    }

    fn show_list(self: &Rc<Self>) {
        self.clear_errors();
        self.stack.set_visible_child_name(LIST);
        self.search.grab_focus();
    }

    /// What the field says about what was typed; Create follows it. The
    /// folder button shows the chosen folder, dimmed for a path.
    fn refresh_hint(self: &Rc<Self>) {
        self.new_error.set_visible(false);
        self.entry.remove_css_class("invalid");
        let typed = self.entry.text();
        let (hint, menu) = {
            let projects = self.projects.borrow();
            let chosen = self.chosen.borrow();
            let folder = picker::chosen_folder(&projects, chosen.as_deref());
            (
                picker::new_project_hint(
                    &typed,
                    self.args.cwd.as_deref(),
                    folder,
                    self.home.as_deref(),
                ),
                picker::folder_menu(&projects, chosen.as_deref(), self.home.as_deref()),
            )
        };
        self.create.set_sensitive(hint.can_create());
        self.hint.remove_css_class("record-text");
        let mono = |path: &str| {
            format!(
                "<span font_family=\"JetBrains Mono\" size=\"92%\">{}</span>",
                glib::markup_escape_text(path)
            )
        };
        let markup = match &hint {
            NewProjectHint::Idle { text } | NewProjectHint::NeedsFolder { text } => {
                glib::markup_escape_text(text).to_string()
            }
            NewProjectHint::GoesIn { name, path } => format!(
                "A name goes in <b>{}</b>, {}. A path, with a slash, goes where it says.",
                glib::markup_escape_text(name),
                mono(path)
            ),
            // The path in mono, as every path (DESIGN Typography).
            NewProjectHint::Creates { path, note } => {
                let note = note
                    .as_deref()
                    .map(|n| format!(". {}", glib::markup_escape_text(n)))
                    .unwrap_or_default();
                format!("Creates {}{note}", mono(path))
            }
            NewProjectHint::Problem { text } => {
                self.entry.add_css_class("invalid");
                self.hint.add_css_class("record-text");
                glib::markup_escape_text(text).to_string()
            }
        };
        self.hint.set_markup(&markup);
        match &menu.chosen {
            Some(name) => {
                self.folder_button.set_visible(true);
                self.folder_link.set_visible(false);
                self.folder_name.set_text(name);
                button::set_menu_label(&self.folder_button, &menu.accessible);
            }
            None => {
                self.folder_button.set_visible(false);
                self.folder_link.set_visible(true);
            }
        }
        // A path goes where it says: the menu plays no part.
        self.folder_button
            .set_sensitive(!new_project::is_path(OsStr::new(typed.trim())));
        self.rebuild_folder_rows(&menu);
    }

    /// The menu's rows: each folder (the chosen one checked, the default
    /// one's badge, Make default on the others), then Add a folder….
    fn rebuild_folder_rows(self: &Rc<Self>, menu: &picker::FolderMenuView) {
        while let Some(row) = self.folder_rows.first_child() {
            self.folder_rows.remove(&row);
        }
        for folder in &menu.rows {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            line.add_css_class("picker-folder-row");
            let check = gtk::Image::from_icon_name("qrec-check-symbolic");
            check.add_css_class("accent-text");
            check.set_opacity(if folder.chosen { 1.0 } else { 0.0 });
            line.append(&check);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
            text.set_hexpand(true);
            text.append(&label(&folder.name, &["t-body"]));
            let path = label(&folder.path, &["t-mono-xs", "muted"]);
            path.set_ellipsize(gtk::pango::EllipsizeMode::Start);
            path.set_max_width_chars(36);
            text.append(&path);
            line.append(&text);
            if folder.default {
                let badge = label("Default", &["folder-badge"]);
                badge.set_valign(gtk::Align::Center);
                line.append(&badge);
            } else {
                let make = gtk::Button::with_label("Make default");
                make.add_css_class("qrec-link");
                make.add_css_class("folder-make-default");
                make.set_valign(gtk::Align::Center);
                make.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Make {} the default folder",
                    folder.name
                ))]);
                let (weak, name) = (Rc::downgrade(self), folder.name.clone());
                make.connect_clicked(guarded!(
                    "the picker: make a folder the default",
                    move |_| {
                        if let Some(picker) = weak.upgrade() {
                            picker.edit_folders(project_folders::FolderEdit::SetDefault {
                                name: name.clone(),
                            });
                        }
                    }
                ));
                line.append(&make);
            }
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(&format!("picker-folder-row-{}", folder.name));
            row.set_child(Some(&line));
            row.update_property(&[gtk::accessible::Property::Label(&format!(
                "{}, {}{}",
                folder.name,
                folder.path,
                if folder.default { ", default" } else { "" }
            ))]);
            self.folder_rows.append(&row);
        }
        let add = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        add.add_css_class("picker-folder-row");
        add.add_css_class("picker-folder-add");
        add.append(&gtk::Image::from_icon_name("qrec-plus-symbolic"));
        add.append(&label("Add a folder…", &["t-body"]));
        let row = gtk::ListBoxRow::new();
        row.set_widget_name("picker-folder-add");
        row.set_child(Some(&add));
        self.folder_rows.append(&row);
    }

    /// A row of the menu: that folder chosen, or Add a folder….
    fn folder_row_activated(self: &Rc<Self>, row: &gtk::ListBoxRow) {
        let name = row.widget_name();
        self.folder_popover.popdown();
        if name == "picker-folder-add" {
            return self.add_project_folder();
        }
        if let Some(folder) = name.strip_prefix("picker-folder-row-") {
            *self.chosen.borrow_mut() = Some(folder.to_owned());
            self.refresh_hint();
            self.entry.grab_focus();
        }
    }

    fn create_project(self: &Rc<Self>) {
        if !self.create.is_sensitive() {
            return;
        }
        let typed = self.entry.text().trim().to_owned();
        let target = {
            let projects = self.projects.borrow();
            let chosen = self.chosen.borrow();
            let folder =
                picker::chosen_folder(&projects, chosen.as_deref()).map(|f| f.name.clone());
            new_project::resolve_in(
                OsStr::new(&typed),
                self.args.cwd.as_deref(),
                &projects,
                folder.as_deref(),
            )
        };
        let target = match target {
            Ok(target) => target,
            Err(e) => return self.show_new_error(&format::capitalized(&e.message)),
        };
        self.set_busy(true);
        background(
            "the picker: a new project",
            move || {
                projects::make_folder(&target).map(|dir| {
                    let kind = projects::project_folder(&dir);
                    (dir, kind)
                })
            },
            |picker, made| match made {
                Ok((dir, ProjectFolder::Empty)) => picker.become_project(&dir),
                Ok((dir, ProjectFolder::Project | ProjectFolder::LostFile)) => {
                    picker.set_busy(false);
                    let shown = picker.shown_path(&dir.to_string_lossy());
                    picker.show_new_error(&format!("{shown} is already a project."));
                }
                Ok((_, ProjectFolder::NotProject)) => {
                    picker.set_busy(false);
                    picker.show_new_error(NOT_MY_SWAMP);
                }
                Err(e) => {
                    picker.set_busy(false);
                    let hint = e.hint.map(|h| format!(" ({h})")).unwrap_or_default();
                    picker.show_new_error(&format!("{}{hint}", format::capitalized(&e.message)));
                }
            },
        );
    }

    /// Add a folder…: the folder chooser, then the folder under its own
    /// name (`videos 2` when taken), chosen for this project.
    fn add_project_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Add a project folder")
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        dialog.select_folder(
            Some(&self.window),
            gio::Cancellable::NONE,
            guarded!("the picker: the project folder dialog", move |result| {
                let Some(picker) = weak.upgrade() else { return };
                match result.map(|folder| folder.path()) {
                    Ok(Some(dir)) => picker.add_folder_at(&dir),
                    Ok(None) => picker.show_new_error("That folder isn't on this computer."),
                    Err(e) => tracing::debug!("the picker: no project folder picked: {e}"),
                }
            }),
        );
    }

    fn add_folder_at(self: &Rc<Self>, dir: &Path) {
        let Some(path) = dir.to_str().map(str::to_owned) else {
            return self.show_new_error("That folder's path can't be saved in the settings.");
        };
        let projects = self.projects.borrow().clone();
        let edit = match projects.folders.iter().find(|f| Path::new(&f.path) == dir) {
            // Already one: just choose it.
            Some(folder) => {
                *self.chosen.borrow_mut() = Some(folder.name.clone());
                return self.refresh_hint();
            }
            None => project_folders::FolderEdit::Add {
                name: project_folders::unused_name_for_path(&projects, &path),
                path,
                make_default: false,
            },
        };
        if let project_folders::FolderEdit::Add { name, .. } = &edit {
            *self.chosen.borrow_mut() = Some(name.clone());
        }
        self.edit_folders(edit);
    }

    /// Saves one change to the project folders, as Settings and `qrec
    /// folders` do, then shows the folders the file holds.
    fn edit_folders(self: &Rc<Self>, edit: project_folders::FolderEdit) {
        let Some(dirs) = self.dirs.clone() else {
            return self.show_new_error("Can't find where the settings live.");
        };
        background(
            "the picker: the project folders",
            move || config_file::edit_folders(&dirs.config_file(), &edit),
            |picker, saved| match saved {
                Ok(config) => {
                    *picker.projects.borrow_mut() = config.projects;
                    picker.refresh_hint();
                }
                Err(e) => {
                    picker.refresh_hint();
                    picker.show_new_error(&format::capitalized(&e.message));
                }
            },
        );
    }

    // ------------------------------------------------------------- feedback

    /// While a folder is looked at or made nothing else can be started.
    fn set_busy(&self, busy: bool) {
        self.stack.set_sensitive(!busy);
    }

    fn show_error(&self, message: &str) {
        self.error.set_text(message);
        self.error.set_visible(true);
    }

    fn show_new_error(&self, message: &str) {
        self.new_error.set_text(message);
        self.new_error.set_visible(true);
    }

    fn clear_errors(&self) {
        self.error.set_visible(false);
        self.new_error.set_visible(false);
    }
}

/// `page` in a centered column, so a wide (tiled) window doesn't stretch it.
fn column(page: &gtk::Box, scale: f64) -> adw::Clamp {
    let width = (COLUMN * scale).round() as i32;
    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(width);
    clamp.set_tightening_threshold(width);
    clamp.set_vexpand(true);
    clamp.set_child(Some(page));
    clamp
}

/// A one-line label at the left of its box.
fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    for class in classes {
        label.add_css_class(class);
    }
    label.set_xalign(0.0);
    label
}

/// A label that wraps at whatever width its box gives it.
fn wrapping(classes: &[&str]) -> gtk::Label {
    let label = label("", classes);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(1);
    label
}

/// The bar at the bottom: each hint's key caps and what they do.
fn hints(items: &[(&[&str], &str)]) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar.add_css_class("picker-hints");
    for (keys, what) in items {
        let hint = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        hint.add_css_class("picker-hint");
        for key in *keys {
            let cap = KeyCap::new(key);
            cap.set_valign(gtk::Align::Center);
            hint.append(&cap);
        }
        hint.append(&label(what, &["picker-hint-label"]));
        bar.append(&hint);
    }
    bar
}
