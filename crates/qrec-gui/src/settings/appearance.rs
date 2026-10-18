//! Settings › Appearance (S21): the four C32 theme cards, Interface size,
//! Reduce motion and the squelch. Owner: WP9.
//!
//! What it shows is `view::preferences::appearance_page` of `config.toml`
//! (read on `qrec-gui-io` when the dialog opens, and after a change of the
//! look made anywhere: the CLI's `qrec config set`, an editor). A pick
//! writes its setting through `qrec_shell::config_file` (as `qrec config
//! set` does, G§3.12 R8) and the window takes the written look at once
//! (`settings::apply_look`): the theme, the scale, reduced motion. A write
//! that fails says why in the footer, and the page shows the file again.
//!
//! S21's anatomy: the page 24/32 in, its groups 24 apart; Theme's header
//! (`heading` over its description) 12 over the cards, 180×132, spread
//! over the row; Interface and Sound: the title 8 over 60 px rows with
//! hairlines, the label over its caption, the control at the right
//! (C28's segmented Interface size, C27's switches); the footer "Changes
//! are saved to ~/.config/qrec/config.toml.". D-30: Reduce motion's
//! caption is "Also on when your system asks for less motion.".

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use qrec_core::config::{Theme as ThemeId, UserConfig};
use qrec_core::view::preferences::{AppearancePage as View, THEMES, appearance_page};

use super::Footer;
use crate::widgets::theme_card::ThemeCard;
use crate::{Ctx, guarded};

/// Whether the Settings pages besides Project are built (`GuiFacts.built`).
pub const BUILT: bool = true;

/// The Appearance page.
pub struct AppearancePage {
    root: gtk::Box,
    parts: Rc<Parts>,
}

impl AppearancePage {
    pub fn new(ctx: &Ctx) -> Self {
        let files = super::user_files();
        let view = appearance_page(&UserConfig::default(), &files.config_shown);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("settings-appearance");
        root.add_css_class("settings-page");
        let body = super::page_body();

        // Theme: the cards.
        let theme = super::group(&view.theme, "gap-12");
        let cards_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        cards_row.set_widget_name("theme-cards");
        cards_row.add_css_class("theme-cards");
        cards_row.set_homogeneous(false);
        let mut cards = Vec::new();
        for (n, card) in view.themes.iter().enumerate() {
            let theme_id = THEMES[n].0;
            let widget = ThemeCard::new(theme_id, &card.id, &card.name, card.caption.as_deref());
            widget.set_scale(ctx.theme.scale());
            if let Some((first, _)) = cards.first() {
                let first: &ThemeCard = first;
                widget.button().set_group(Some(first.button()));
            }
            // Spread over the row (S21: space-between).
            widget.button().set_hexpand(n + 1 < view.themes.len());
            widget.button().set_halign(if n + 1 < view.themes.len() {
                gtk::Align::Start
            } else {
                gtk::Align::End
            });
            cards_row.append(widget.button());
            cards.push((widget, theme_id));
        }
        theme.append(&cards_row);
        body.append(&theme);

        // Interface: size, reduce motion.
        let interface = super::group(&view.interface, "gap-8");
        let rows = super::rows();
        let scales = adw::ToggleGroup::new();
        scales.set_widget_name("settings-scale");
        scales.add_css_class("qrec-segmented");
        scales.add_css_class("scale-switch");
        for choice in &view.scales {
            scales.add(
                adw::Toggle::builder()
                    .name(choice.setting.1.as_str())
                    .label(choice.label.as_str())
                    .build(),
            );
        }
        let names: Vec<&str> = view.scales.iter().map(|c| c.setting.1.as_str()).collect();
        super::name_segments(&scales, "settings-scale", &names);
        rows.append(&super::row(&view.scale, &scales));
        let motion = gtk::Switch::new();
        motion.set_widget_name("settings-reduced-motion");
        motion.set_focus_on_click(false);
        rows.append(&super::row(
            &qrec_core::view::preferences::RowText {
                label: view.reduced_motion.label.clone(),
                caption: view.reduced_motion.caption.clone(),
            },
            &motion,
        ));
        interface.append(&rows);
        body.append(&interface);

        // Sound: the squelch.
        let sound = super::group(&view.sound, "gap-8");
        let rows = super::rows();
        let squelch = gtk::Switch::new();
        squelch.set_widget_name("settings-sounds");
        squelch.set_focus_on_click(false);
        rows.append(&super::row(
            &qrec_core::view::preferences::RowText {
                label: view.sounds.label.clone(),
                caption: view.sounds.caption.clone(),
            },
            &squelch,
        ));
        sound.append(&rows);
        body.append(&sound);
        root.append(&body);

        let footer = Footer::new(&view.footer, &view.footer_path);
        root.append(&footer.widget());

        let parts = Rc::new(Parts {
            ctx: ctx.clone(),
            cards,
            scales,
            motion,
            squelch,
            footer,
            config: RefCell::new(UserConfig::default()),
            syncing: Cell::new(false),
        });
        parts.connect();
        let follower = Rc::downgrade(&parts);
        ctx.theme
            .subscribe(move |theme| {
                if let Some(parts) = follower.upgrade() {
                    for (card, _) in &parts.cards {
                        card.set_scale(theme.scale());
                    }
                    // A look changed elsewhere (the CLI): the file again.
                    if !parts.syncing.get() {
                        parts.read();
                    }
                }
            })
            .detach();
        Self { root, parts }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Shows `config.toml` as it is (the dialog opens).
    pub fn refresh(&self) {
        self.parts.read();
    }

    /// The control the page's focus starts on: the selected card.
    pub fn focus_target(&self) -> gtk::Widget {
        let theme = self.parts.config.borrow().theme;
        self.parts
            .cards
            .iter()
            .find(|(_, id)| *id == theme)
            .or(self.parts.cards.first())
            .map(|(card, _)| card.button().clone().upcast())
            .unwrap_or_else(|| self.root.clone().upcast())
    }
}

/// The page's controls and the config they show.
struct Parts {
    ctx: Ctx,
    cards: Vec<(ThemeCard, ThemeId)>,
    scales: adw::ToggleGroup,
    motion: gtk::Switch,
    squelch: gtk::Switch,
    footer: Footer,
    config: RefCell<UserConfig>,
    /// Set while the controls follow the config (no write then).
    syncing: Cell<bool>,
}

impl Parts {
    /// Reads `config.toml` (on the io worker), then shows it.
    fn read(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        super::read_config(&self.ctx, move |config| {
            if let Some(parts) = weak.upgrade() {
                parts.show(&config);
            }
        });
    }

    /// The controls show `config`.
    fn show(&self, config: &UserConfig) {
        *self.config.borrow_mut() = config.clone();
        let view: View = appearance_page(config, "");
        self.syncing.set(true);
        for ((card, _), shown) in self.cards.iter().zip(&view.themes) {
            card.set_selected(shown.selected);
            if card.button().is_active() != shown.selected {
                card.button().set_active(shown.selected);
            }
        }
        match view.scales.iter().position(|c| c.selected) {
            Some(i) => self.scales.set_active(i as u32),
            None => self.scales.set_active(gtk::INVALID_LIST_POSITION),
        }
        if self.motion.is_active() != view.reduced_motion.on {
            self.motion.set_active(view.reduced_motion.on);
        }
        self.motion.set_state(view.reduced_motion.on);
        if self.squelch.is_active() != view.sounds.on {
            self.squelch.set_active(view.sounds.on);
        }
        self.squelch.set_state(view.sounds.on);
        self.syncing.set(false);
    }

    /// Writes `key` = `value`, then shows and applies what was written (or
    /// says why it couldn't and shows the file as it was).
    fn write(self: &Rc<Self>, key: &str, value: &str) {
        let weak = Rc::downgrade(self);
        super::write_setting(&self.ctx, key, value, move |written| {
            let Some(parts) = weak.upgrade() else {
                return;
            };
            match written {
                Ok(config) => {
                    parts.footer.show_error(None);
                    parts.syncing.set(true);
                    super::apply_look(&parts.ctx, &config);
                    parts.syncing.set(false);
                    parts.show(&config);
                }
                Err(why) => {
                    tracing::warn!("settings: {why}");
                    parts.footer.show_error(Some(&why));
                    parts.read();
                }
            }
        });
    }

    fn connect(self: &Rc<Self>) {
        for (card, id) in &self.cards {
            let (parts, id) = (Rc::downgrade(self), *id);
            card.button()
                .connect_toggled(guarded!("settings: a theme card", move |button| {
                    let Some(parts) = parts.upgrade() else {
                        return;
                    };
                    if parts.syncing.get() || !button.is_active() {
                        return;
                    }
                    let name = THEMES
                        .iter()
                        .find(|(t, _, _)| *t == id)
                        .map(|(_, name, _)| *name)
                        .unwrap_or("swamp-night");
                    parts.write("theme", name);
                }));
        }
        let parts = Rc::downgrade(self);
        self.scales.connect_active_name_notify(guarded!(
            "settings: interface size",
            move |group| {
                let Some(parts) = parts.upgrade() else {
                    return;
                };
                if parts.syncing.get() {
                    return;
                }
                if let Some(value) = group.active_name() {
                    parts.write("ui.scale", &value);
                }
            }
        ));
        for (switch, key) in [
            (&self.motion, "ui.reduced_motion"),
            (&self.squelch, "ui.sounds"),
        ] {
            let parts = Rc::downgrade(self);
            switch.connect_state_set(guarded!(
                "settings: a switch",
                after_panic = gtk::glib::Propagation::Stop,
                move |_, on| {
                    let Some(parts) = parts.upgrade() else {
                        return gtk::glib::Propagation::Stop;
                    };
                    if parts.syncing.get() {
                        return gtk::glib::Propagation::Proceed;
                    }
                    parts.write(key, if on { "true" } else { "false" });
                    // The switch shows the file's value once written.
                    gtk::glib::Propagation::Stop
                }
            ));
        }
    }
}
