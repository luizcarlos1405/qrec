//! Settings › Autocut (not drawn: S21's patterns with S12's knobs, a
//! departure listed in G§7.2). Owner: WP9.
//!
//! What it shows is `view::preferences::autocut_page` of `config.toml`:
//! the autocut settings new projects start with. "Run autocut after every
//! take" (C27, S12's words) and S12's four knobs as C29 numeric fields
//! with their units (Threshold dB, Min silence, Padding and Min keep ms;
//! `config.toml`'s ranges, the knobs' steps). A project's own settings are
//! its autocut page's (S12, `Shift+C`). Each change writes its setting as
//! `qrec config set` does; a refused value says why under its field.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use qrec_core::config::UserConfig;
use qrec_core::view::preferences::{AutocutPage as View, RowText, autocut_page};

use super::Footer;
use crate::widgets::numeric::NumericField;
use crate::{Ctx, guarded};

/// A field's text as `config.toml` takes it: the unit and the true minus
/// gone ("−35 dB" → "-35"), spaces trimmed (GTK-free).
pub fn typed_value(text: &str, units: &[&str]) -> String {
    let mut value = text.trim().replace('\u{2212}', "-");
    for unit in units {
        if let Some(stripped) = value.strip_suffix(unit) {
            value = stripped.trim().to_owned();
            break;
        }
    }
    value
}

/// The Autocut page.
pub struct AutocutPage {
    root: gtk::Box,
    parts: Rc<Parts>,
}

impl AutocutPage {
    pub fn new(ctx: &Ctx) -> Self {
        let files = super::user_files();
        let view = autocut_page(&UserConfig::default(), &files.config_shown);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("settings-autocut");
        root.add_css_class("settings-page");
        let body = super::page_body();
        let group = super::group(&view.group, "gap-8");
        let rows = super::rows();
        let auto_run = gtk::Switch::new();
        auto_run.set_widget_name("settings-auto-run");
        auto_run.set_focus_on_click(false);
        rows.append(&super::row(
            &RowText {
                label: view.auto_run.label.clone(),
                caption: view.auto_run.caption.clone(),
            },
            &auto_run,
        ));
        let mut fields = Vec::new();
        for knob in &view.knobs {
            let field = NumericField::new(&knob.unit, &knob.text.label);
            field.set_step(knob.step);
            if knob.unit == "dB" {
                field.set_unit_aliases(&["db"]);
            }
            let name = format!("settings-{}", knob.key.replace(['.', '_'], "-"));
            let widget = super::field_with_helper(&field, &name, 104);
            rows.append(&super::row(&knob.text, &widget));
            fields.push((field, knob.key.clone(), knob.unit.clone()));
        }
        group.append(&rows);
        body.append(&group);
        root.append(&body);
        let footer = Footer::new(&view.footer, &view.footer_path);
        root.append(&footer.widget());
        let parts = Rc::new(Parts {
            ctx: ctx.clone(),
            auto_run,
            fields,
            footer,
            syncing: Cell::new(false),
            config: RefCell::new(UserConfig::default()),
        });
        parts.connect();
        Self { root, parts }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Shows `config.toml` as it is (the dialog opens).
    pub fn refresh(&self) {
        self.parts.read();
    }

    /// The control the page's focus starts on.
    pub fn focus_target(&self) -> gtk::Widget {
        self.parts.auto_run.clone().upcast()
    }
}

struct Parts {
    ctx: Ctx,
    auto_run: gtk::Switch,
    /// Each knob's field, its setting and its unit.
    fields: Vec<(NumericField, String, String)>,
    footer: Footer,
    syncing: Cell<bool>,
    config: RefCell<UserConfig>,
}

impl Parts {
    fn read(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        super::read_config(&self.ctx, move |config| {
            if let Some(parts) = weak.upgrade() {
                parts.show(&config);
            }
        });
    }

    fn show(&self, config: &UserConfig) {
        *self.config.borrow_mut() = config.clone();
        let view: View = autocut_page(config, "");
        self.syncing.set(true);
        if self.auto_run.is_active() != view.auto_run.on {
            self.auto_run.set_active(view.auto_run.on);
        }
        self.auto_run.set_state(view.auto_run.on);
        for ((field, _, _), knob) in self.fields.iter().zip(&view.knobs) {
            field.set_value(&knob.value);
        }
        self.syncing.set(false);
    }

    /// Writes `key`; a refused value says why under field `field` (else in
    /// the footer).
    fn write(self: &Rc<Self>, key: &str, value: &str, field: Option<usize>) {
        let weak = Rc::downgrade(self);
        super::write_setting(&self.ctx, key, value, move |written| {
            let Some(parts) = weak.upgrade() else {
                return;
            };
            match (written, field) {
                (Ok(config), _) => {
                    parts.footer.show_error(None);
                    if let Some((f, _, _)) = field.and_then(|n| parts.fields.get(n)) {
                        f.set_error(None);
                    }
                    parts.show(&config);
                }
                (Err(why), Some(n)) => {
                    if let Some((f, _, _)) = parts.fields.get(n) {
                        f.set_error(Some(&why));
                    }
                }
                (Err(why), None) => {
                    parts.footer.show_error(Some(&why));
                    let config = parts.config.borrow().clone();
                    parts.show(&config);
                }
            }
        });
    }

    fn connect(self: &Rc<Self>) {
        let parts = Rc::downgrade(self);
        self.auto_run.connect_state_set(guarded!(
            "settings: autocut after every take",
            after_panic = gtk::glib::Propagation::Stop,
            move |_, on| {
                let Some(parts) = parts.upgrade() else {
                    return gtk::glib::Propagation::Stop;
                };
                if parts.syncing.get() {
                    return gtk::glib::Propagation::Proceed;
                }
                parts.write(
                    "autocut.auto_run_after_take",
                    if on { "true" } else { "false" },
                    None,
                );
                gtk::glib::Propagation::Stop
            }
        ));
        for (n, (field, key, unit)) in self.fields.iter().enumerate() {
            let (parts, key, unit) = (Rc::downgrade(self), key.clone(), unit.clone());
            field.on_commit(move |text| {
                if let Some(parts) = parts.upgrade() {
                    let units: Vec<&str> = if unit == "dB" {
                        vec!["dB", "db"]
                    } else {
                        vec![unit.as_str()]
                    };
                    parts.write(&key, &typed_value(text, &units), Some(n));
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_value_is_what_config_toml_takes() {
        assert_eq!(typed_value(" \u{2212}35 dB ", &["dB", "db"]), "-35");
        assert_eq!(typed_value("-40db", &["dB", "db"]), "-40");
        assert_eq!(typed_value("400 ms", &["ms"]), "400");
        assert_eq!(typed_value("80", &["ms"]), "80");
    }
}
