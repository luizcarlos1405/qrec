//! The Settings dialog (S18b, S21, S22, G§4.10): its shell with the five
//! tabs as drawn, 840×640, centered, over a scrim that ends above the hint
//! bar. Owner: WP6 (shell, Project), WP9 (the other pages and the parts
//! they share). Registers `settings.open`.
//!
//! The shell is `raised`, `xl` radius, e3 with a 1 px `border`: a 56 px
//! header ("Settings" in `headline`, the tab switcher centered, "× Close
//! [Esc]" at the right) over the page. The tabs are a segmented control
//! (C28, the anatomy of `adw::InlineViewSwitcher`): Appearance (S21),
//! Recording and Autocut (not drawn, in S21's patterns), Project (S18b)
//! and Keyboard (S22), every one enabled since WP9 (D-48's "Not built yet"
//! is gone). It follows `LocalUi`: open while an `Overlay::Settings` is in
//! the stack (a modal may stack over it: the S18b fps confirmation), on
//! that overlay's page; a page shows its settings again each time the
//! dialog opens. `Esc` (the key router pops the top overlay) and Close
//! close it.

pub mod appearance;
pub mod autocut;
pub mod keyboard;
pub mod project;
pub mod recording;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use qrec_core::config::UserConfig;
use qrec_core::view::preferences::{GroupHeader, RowText};
use qrec_core::view::settings::{SettingsPage, tabs};

use crate::store::local::{LocalUi, Overlay};
use crate::widgets::button;
use crate::window::layers::Layer;
use crate::{Ctx, guarded};
use appearance::AppearancePage;
use autocut::AutocutPage;
use keyboard::KeyboardPage;
use project::ProjectPage;
use recording::RecordingPage;

/// The user's settings files (`config.toml`, `keys.toml`) and their paths
/// as people read them (`~/.config/qrec/keys.toml`): what the pages write
/// and what their footers say. `None` without user dirs (no `$HOME`): the
/// pages show the usual paths and their writes fail with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFiles {
    pub config: Option<std::path::PathBuf>,
    pub keys: Option<std::path::PathBuf>,
    pub config_shown: String,
    pub keys_shown: String,
}

/// [`UserFiles`] of this process's environment (no file I/O).
pub fn user_files() -> UserFiles {
    let env = qrec_shell::paths::Env::from_process();
    let home = env.get("HOME").map(|h| h.to_string_lossy().into_owned());
    let shown = |path: &std::path::Path| {
        qrec_core::view::format::display_path(&path.to_string_lossy(), "", home.as_deref())
    };
    match qrec_shell::paths::UserDirs::resolve(&env) {
        Ok(dirs) => {
            let (config, keys) = (dirs.config_file(), dirs.keys_file());
            UserFiles {
                config_shown: shown(&config),
                keys_shown: shown(&keys),
                config: Some(config),
                keys: Some(keys),
            }
        }
        Err(_) => UserFiles {
            config: None,
            keys: None,
            config_shown: "~/.config/qrec/config.toml".to_owned(),
            keys_shown: "~/.config/qrec/keys.toml".to_owned(),
        },
    }
}

// ------------------------------------------------ the pages' parts (WP9)

/// A page's body: 24/32 in, its groups 24 apart (S21, S22, S18b).
pub(crate) fn page_body() -> gtk::Box {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.add_css_class("pref-body");
    body.set_vexpand(true);
    body
}

/// A group: its header (the title in `heading` over its description in
/// `caption` `muted`, 2 apart) and what the caller adds under it, `gap`
/// apart ("gap-12" under Theme's, S21; "gap-8" elsewhere).
pub(crate) fn group(header: &GroupHeader, gap: &str) -> gtk::Box {
    let group = gtk::Box::new(gtk::Orientation::Vertical, 0);
    group.add_css_class("pref-group");
    group.add_css_class(gap);
    let head = gtk::Box::new(gtk::Orientation::Vertical, 0);
    head.add_css_class("pref-group-header");
    let title = gtk::Label::new(Some(&header.title));
    title.add_css_class("t-heading");
    title.set_xalign(0.0);
    head.append(&title);
    if let Some(description) = &header.description {
        let text = gtk::Label::new(Some(description));
        text.add_css_class("t-caption");
        text.add_css_class("muted");
        text.set_xalign(0.0);
        text.set_wrap(true);
        head.append(&text);
    }
    group.append(&head);
    group
}

/// A group's rows: a hairline above them (each row draws its own under it).
pub(crate) fn rows() -> gtk::Box {
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rows.add_css_class("pref-rows");
    rows
}

/// A 60 px row: its label (`body`) over its caption (`caption` `muted`), 2
/// apart, at the left; `control` at the right, 24 from them.
pub(crate) fn row(text: &RowText, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("pref-row");
    let words = gtk::Box::new(gtk::Orientation::Vertical, 0);
    words.add_css_class("pref-row-text");
    words.set_hexpand(true);
    words.set_valign(gtk::Align::Center);
    let label = gtk::Label::new(Some(&text.label));
    label.add_css_class("t-body");
    label.set_xalign(0.0);
    words.append(&label);
    if let Some(caption) = &text.caption {
        let caption = gtk::Label::new(Some(caption));
        caption.add_css_class("t-caption");
        caption.add_css_class("muted");
        caption.set_xalign(0.0);
        caption.set_wrap(true);
        caption.set_max_width_chars(1);
        caption.set_hexpand(true);
        words.append(&caption);
    }
    row.append(&words);
    let control = control.as_ref();
    control.set_halign(gtk::Align::End);
    control.set_valign(gtk::Align::Center);
    control.update_property(&[gtk::accessible::Property::Label(&text.label)]);
    row.append(control);
    row
}

/// A C29 numeric field `width` wide with its helper text under it: why a
/// value was refused (`record-text`, C29 invalid), shown while the field
/// holds the refused text.
pub(crate) fn field_with_helper(
    field: &crate::widgets::numeric::NumericField,
    name: &str,
    width: i32,
) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.add_css_class("pref-field");
    let widget = field.widget();
    widget.set_widget_name(name);
    widget.set_size_request(width, -1);
    column.append(&widget);
    let helper = gtk::Label::new(None);
    helper.set_widget_name(&format!("{name}-helper"));
    helper.add_css_class("field-helper");
    helper.set_xalign(1.0);
    helper.set_wrap(true);
    helper.set_max_width_chars(1);
    helper.set_size_request(width.max(240), -1);
    helper.set_visible(false);
    column.append(&helper);
    field.on_error(move |error| {
        helper.set_text(error.unwrap_or_default());
        helper.set_visible(error.is_some());
    });
    column
}

/// A page's 44 px footer: "Changes are saved to " · the path in
/// `mono-sm` · "." (S21, S22), or why the last change couldn't be saved.
pub(crate) struct Footer {
    root: gtk::Box,
    labels: [gtk::Label; 3],
    text: (String, String, String),
}

impl Footer {
    /// The footer of `text`, its `path` drawn in the mono face.
    pub(crate) fn new(text: &str, path: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("settings-footer");
        let (start, end) = text
            .split_once(path)
            .map_or((text.to_owned(), String::new()), |(a, b)| {
                (a.to_owned(), b.to_owned())
            });
        let labels = [
            gtk::Label::new(Some(&start)),
            gtk::Label::new(Some(path)),
            gtk::Label::new(Some(&end)),
        ];
        for (label, class) in labels.iter().zip(["t-caption", "t-mono-sm", "t-caption"]) {
            label.add_css_class(class);
            label.add_css_class("muted");
            label.set_valign(gtk::Align::BaselineCenter);
            root.append(label);
        }
        labels[1].set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        labels[2].set_hexpand(true);
        labels[2].set_xalign(0.0);
        Self {
            root,
            labels,
            text: (start, path.to_owned(), end),
        }
    }

    pub(crate) fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Says why the last change wasn't saved (`record-text`), or the
    /// usual line again (`None`).
    pub(crate) fn show_error(&self, error: Option<&str>) {
        let [start, path, end] = &self.labels;
        match error {
            Some(why) => {
                start.set_text("Couldn't save to ");
                end.set_text(&format!(": {why}"));
                for label in &self.labels {
                    label.add_css_class("record-text");
                    label.remove_css_class("muted");
                }
            }
            None => {
                start.set_text(&self.text.0);
                end.set_text(&self.text.2);
                for label in &self.labels {
                    label.remove_css_class("record-text");
                    label.add_css_class("muted");
                }
            }
        }
        path.set_text(&self.text.1);
    }
}

/// Names the segments of `group` `<prefix>-<name>` in order (`names`: its
/// toggles' names), for the region checks and the clicks of the nested
/// suites (G§8.4): `adw::ToggleGroup` keeps its segment widgets (CSS
/// `toggle`) to itself.
pub(crate) fn name_segments(group: &adw::ToggleGroup, prefix: &str, names: &[&str]) {
    let mut names = names.iter();
    let mut child = group.first_child();
    while let Some(widget) = child {
        if widget.css_name() == "toggle"
            && let Some(name) = names.next()
        {
            widget.set_widget_name(&format!("{prefix}-{name}"));
        }
        child = widget.next_sibling();
    }
}

/// `config.toml`'s settings as they are, read on `qrec-gui-io` (no file
/// I/O on the GTK thread, R1), then `then` with them (the defaults when
/// there is no file, or no user dirs).
pub(crate) fn read_config(ctx: &Ctx, then: impl FnOnce(UserConfig) + 'static) {
    let path = user_files().config;
    ctx.media.run_io_then(
        move || match path {
            Some(path) => qrec_shell::config_file::read(&path).0,
            None => UserConfig::default(),
        },
        then,
    );
}

/// Writes `key` = `value` to `config.toml` on `qrec-gui-io`, as `qrec
/// config set` does (`config_file::set`: the value checked, one line
/// edited, the rest of the file as it was, an atomic write; G§3.12 R8),
/// then `then` with the settings the file holds now, or why it couldn't.
pub(crate) fn write_setting(
    ctx: &Ctx,
    key: &str,
    value: &str,
    then: impl FnOnce(Result<UserConfig, String>) + 'static,
) {
    let path = user_files().config;
    let (key, value) = (key.to_owned(), value.to_owned());
    tracing::info!("settings: config.toml {key} = {value}");
    ctx.media.run_io_then(
        move || match path {
            Some(path) => {
                qrec_shell::config_file::set(&path, &key, &value).map_err(|e| e.message.clone())
            }
            None => Err("no home folder to keep config.toml in".to_owned()),
        },
        then,
    );
}

/// The look `config` asks for, applied to the window at once (the theme,
/// the UI scale, reduced motion with the system's: D-30); the settings
/// follower sees the write too and finds nothing left to change.
pub(crate) fn apply_look(ctx: &Ctx, config: &UserConfig) {
    ctx.theme.apply(
        config.theme,
        config.ui.scale,
        crate::theme::MotionPolicy::from_settings(config.ui.reduced_motion),
    );
}

/// The Settings dialog (opened in `Layer::Settings`).
pub struct Settings {
    root: gtk::Box,
}

impl Settings {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Dialog)
            .build();
        root.set_widget_name("settings");
        root.add_css_class("qrec-settings");
        root.set_size_request(840, 640);
        root.set_halign(gtk::Align::Center);
        // Centered on the window (BRIEF §2.8: at (380, 180) in 1600×1000):
        // the content layer spans it, the hint bar too (window/mod.rs).
        root.set_valign(gtk::Align::Center);
        root.update_property(&[gtk::accessible::Property::Label("Settings")]);
        let keymap = ctx.store.keymap();

        let header = gtk::CenterBox::new();
        header.add_css_class("settings-header");
        let title = gtk::Label::new(Some("Settings"));
        title.add_css_class("t-headline");
        title.add_css_class("settings-title");
        header.set_start_widget(Some(&title));
        let switcher = adw::ToggleGroup::new();
        switcher.set_widget_name("settings-tabs");
        switcher.add_css_class("qrec-segmented");
        switcher.add_css_class("settings-tabs");
        switcher.set_valign(gtk::Align::Center);
        switcher
            .upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label("Settings pages")]);
        let facts = ctx.store.local().facts();
        let tabs = tabs(&facts);
        for tab in &tabs {
            let toggle = adw::Toggle::builder()
                .name(tab.page.name())
                .label(tab.label.as_str())
                .enabled(tab.enabled)
                .build();
            switcher.add(toggle);
        }
        let names: Vec<&str> = tabs.iter().map(|t| t.page.name()).collect();
        name_segments(&switcher, "settings-tab", &names);
        header.set_center_widget(Some(&switcher));
        let close = button::labeled(
            "Close",
            Some("qrec-x-symbolic"),
            Some("cancel"),
            &keymap,
            button::Kind::Flat,
        );
        close.set_widget_name("settings-close");
        close.set_valign(gtk::Align::Center);
        // Its key cap keeps its own height in the 32 px button.
        let mut part = close.child().and_then(|content| content.first_child());
        while let Some(widget) = part {
            widget.set_valign(gtk::Align::Center);
            part = widget.next_sibling();
        }
        header.set_end_widget(Some(&close));
        root.append(&header);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        let pages = Pages {
            appearance: AppearancePage::new(ctx),
            recording: RecordingPage::new(ctx),
            project: ProjectPage::new(ctx),
            autocut: AutocutPage::new(ctx),
            keyboard: KeyboardPage::new(ctx),
        };
        for page in SettingsPage::ALL {
            stack.add_named(&pages.widget(page), Some(page.name()));
        }
        stack.set_visible_child_name(SettingsPage::Project.name());
        root.append(&stack);

        let store = ctx.store.clone();
        close.connect_clicked(guarded!("settings: close", move |_| {
            store.update_local(|l| {
                l.close_overlays(|o| matches!(o, Overlay::Settings(_)));
            });
        }));
        // `Esc` from a field with nothing to put back (a field has the
        // keys, G§3.8 step 2; from anywhere else the router closes it).
        let keys = gtk::EventControllerKey::new();
        let store = ctx.store.clone();
        keys.connect_key_pressed(guarded!(
            "settings: Esc from a field",
            after_panic = gtk::glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                use gtk::gdk::ModifierType as M;
                let held = M::CONTROL_MASK | M::ALT_MASK | M::SHIFT_MASK | M::SUPER_MASK;
                if key != gtk::gdk::Key::Escape || modifiers.intersects(held) {
                    return gtk::glib::Propagation::Proceed;
                }
                store.update_local(|l| {
                    l.close_overlays(|o| matches!(o, Overlay::Settings(_)));
                });
                gtk::glib::Propagation::Stop
            }
        ));
        root.add_controller(keys);
        let syncing = Rc::new(Cell::new(false));
        let (store, quiet) = (ctx.store.clone(), Rc::clone(&syncing));
        switcher.connect_active_name_notify(guarded!("settings: a tab", move |group| {
            if quiet.get() {
                return;
            }
            let Some(page) = group
                .active_name()
                .and_then(|n| SettingsPage::from_name(&n))
            else {
                return;
            };
            store.update_local(move |l| {
                for overlay in &mut l.overlays {
                    if let Overlay::Settings(shown) = overlay {
                        *shown = page;
                    }
                }
            });
        }));

        // `settings.open` (Ctrl+, and the header's button): opens it on
        // Project, or leaves it as it is when it is open.
        let store = ctx.store.clone();
        ctx.actions.register(
            "settings.open",
            Rc::new(move || {
                store.update_local(|l| {
                    if !l.overlays.iter().any(|o| matches!(o, Overlay::Settings(_))) {
                        l.overlays.push(Overlay::Settings(SettingsPage::Project));
                    }
                });
            }),
        );

        let dialog = Rc::new(Dialog {
            ctx: ctx.clone(),
            root: root.clone(),
            switcher,
            stack,
            pages,
            syncing,
            shown: RefCell::new(None),
        });
        let follower = Rc::clone(&dialog);
        ctx.store
            .subscribe_local(move |local| follower.follow(local))
            .detach();
        Self { root }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }
}

/// The five pages.
struct Pages {
    appearance: AppearancePage,
    recording: RecordingPage,
    project: ProjectPage,
    autocut: AutocutPage,
    keyboard: KeyboardPage,
}

impl Pages {
    fn widget(&self, page: SettingsPage) -> gtk::Widget {
        match page {
            SettingsPage::Appearance => self.appearance.widget(),
            SettingsPage::Recording => self.recording.widget(),
            SettingsPage::Project => self.project.widget(),
            SettingsPage::Autocut => self.autocut.widget(),
            SettingsPage::Keyboard => self.keyboard.widget(),
        }
    }

    /// Every page shows its settings as they are now (the dialog opens).
    fn refresh(&self) {
        self.appearance.refresh();
        self.recording.refresh();
        self.project.refresh();
        self.autocut.refresh();
        self.keyboard.refresh();
    }

    /// Where the focus starts on `page`.
    fn focus_target(&self, page: SettingsPage) -> gtk::Widget {
        match page {
            SettingsPage::Appearance => self.appearance.focus_target(),
            SettingsPage::Recording => self.recording.focus_target(),
            SettingsPage::Project => self.project.focus_target(),
            SettingsPage::Autocut => self.autocut.focus_target(),
            SettingsPage::Keyboard => self.keyboard.focus_target(),
        }
    }
}

/// The dialog and the page it shows.
struct Dialog {
    ctx: Ctx,
    root: gtk::Box,
    switcher: adw::ToggleGroup,
    stack: gtk::Stack,
    pages: Pages,
    syncing: Rc<Cell<bool>>,
    /// The page shown while it is open.
    shown: RefCell<Option<SettingsPage>>,
}

impl Dialog {
    /// Opens, turns to its page or closes with the overlays.
    fn follow(&self, local: &LocalUi) {
        let wanted = local.overlays.iter().rev().find_map(|o| match o {
            Overlay::Settings(page) => Some(*page),
            _ => None,
        });
        let was = *self.shown.borrow();
        if was == wanted {
            return;
        }
        *self.shown.borrow_mut() = wanted;
        let Some(page) = wanted else {
            self.ctx.window.close(Layer::Settings);
            return;
        };
        self.syncing.set(true);
        self.switcher.set_active_name(Some(page.name()));
        self.syncing.set(false);
        self.stack.set_visible_child_name(page.name());
        if was.is_none() {
            // Before it shows: the pages drop what they were left with.
            self.pages.refresh();
            self.ctx.window.open(Layer::Settings, &self.root);
            // The scrim under it, for the region checks (G§8.4).
            if let Some(scrim) = self.root.prev_sibling() {
                scrim.set_widget_name("scrim-settings");
            }
            self.pages.focus_target(page).grab_focus();
        }
    }
}
