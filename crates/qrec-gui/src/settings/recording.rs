//! Settings › Recording (not drawn: S21's patterns and DESIGN's components,
//! a departure listed in G§7.2). Owner: WP9.
//!
//! What it shows is `view::preferences::recording_page` of `config.toml`:
//! the countdown new projects start with (C29's numeric field, "s"), the
//! H.264 encoder a project that says auto gets (C28: Auto · VA-API · x264)
//! and the VA-API device (C29's dropdown: Auto, then the machine's Intel
//! and AMD render nodes, read from sysfs on `qrec-gui-io`; never NVIDIA's,
//! CLAUDE.md, but one `config.toml` names already shows as it is). Each
//! change writes its setting as `qrec config set` does (`config_file::set`)
//! and the page shows what the file holds then; a refused value says why
//! under its field (C29 invalid) or in the footer.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use qrec_core::config::UserConfig;
use qrec_core::view::preferences::{RecordingPage as View, recording_page};
use qrec_shell::gpu::{VENDOR_NVIDIA, render_nodes, vendor_name};

use super::Footer;
use crate::widgets::numeric::NumericField;
use crate::{Ctx, guarded};

/// One item of the device dropdown: what it writes and how it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceItem {
    /// `encoder.vaapi_device`'s value: `auto` or a node's path.
    pub value: String,
    /// "Auto", "/dev/dri/renderD129 · Intel".
    pub label: String,
}

/// The dropdown's items (GTK-free): Auto, every Intel or AMD node found
/// (`nodes`: path and PCI vendor), and the configured device when it isn't
/// one of them (an NVIDIA node a person set by hand shows as it is: it is
/// never offered).
pub fn device_items(
    auto: &str,
    nodes: &[(String, Option<u16>)],
    set: Option<&str>,
) -> Vec<DeviceItem> {
    let mut items = vec![DeviceItem {
        value: "auto".to_owned(),
        label: auto.to_owned(),
    }];
    let named = |path: &str, vendor: Option<u16>| match vendor {
        Some(v) => format!("{path} \u{b7} {}", vendor_name(v)),
        None => path.to_owned(),
    };
    for (path, vendor) in nodes {
        if *vendor == Some(VENDOR_NVIDIA) {
            continue;
        }
        items.push(DeviceItem {
            value: path.clone(),
            label: named(path, *vendor),
        });
    }
    if let Some(set) = set.filter(|s| !items.iter().any(|i| i.value == *s)) {
        let vendor = nodes.iter().find(|(p, _)| p == set).and_then(|(_, v)| *v);
        items.push(DeviceItem {
            value: set.to_owned(),
            label: named(set, vendor),
        });
    }
    items
}

/// The Recording page.
pub struct RecordingPage {
    root: gtk::Box,
    parts: Rc<Parts>,
}

impl RecordingPage {
    pub fn new(ctx: &Ctx) -> Self {
        let files = super::user_files();
        let view = recording_page(&UserConfig::default(), &files.config_shown);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("settings-recording");
        root.add_css_class("settings-page");
        let body = super::page_body();

        let takes = super::group(&view.takes, "gap-8");
        let rows = super::rows();
        let countdown = NumericField::new(&view.countdown.unit, &view.countdown.text.label);
        countdown.set_step(view.countdown.step);
        let field = super::field_with_helper(&countdown, "settings-countdown", 104);
        rows.append(&super::row(&view.countdown.text, &field));
        takes.append(&rows);
        body.append(&takes);

        let encoder = super::group(&view.encoder, "gap-8");
        let rows = super::rows();
        let preference = adw::ToggleGroup::new();
        preference.set_widget_name("settings-encoder");
        preference.add_css_class("qrec-segmented");
        for choice in &view.preferences {
            preference.add(
                adw::Toggle::builder()
                    .name(choice.setting.1.as_str())
                    .label(choice.label.as_str())
                    .build(),
            );
        }
        let names: Vec<&str> = view
            .preferences
            .iter()
            .map(|c| c.setting.1.as_str())
            .collect();
        super::name_segments(&preference, "settings-encoder", &names);
        rows.append(&super::row(&view.preference, &preference));
        let device = gtk::DropDown::from_strings(&[view.device_auto.as_str()]);
        device.set_widget_name("settings-vaapi-device");
        device.add_css_class("settings-dropdown");
        device.set_size_request(260, -1);
        rows.append(&super::row(&view.device, &device));
        encoder.append(&rows);
        body.append(&encoder);
        root.append(&body);
        let footer = Footer::new(&view.footer, &view.footer_path);
        root.append(&footer.widget());
        crate::panel::menus::track_dropdown(ctx, &device);

        let parts = Rc::new(Parts {
            ctx: ctx.clone(),
            countdown,
            preference,
            device,
            devices: RefCell::new(Vec::new()),
            nodes: RefCell::new(Vec::new()),
            footer,
            config: RefCell::new(UserConfig::default()),
            syncing: Cell::new(false),
        });
        parts.connect();
        Self { root, parts }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Shows `config.toml` and the machine's render nodes as they are (the
    /// dialog opens).
    pub fn refresh(&self) {
        self.parts.read();
    }

    /// The control the page's focus starts on.
    pub fn focus_target(&self) -> gtk::Widget {
        self.parts.countdown.text_widget().upcast()
    }
}

struct Parts {
    ctx: Ctx,
    countdown: NumericField,
    preference: adw::ToggleGroup,
    device: gtk::DropDown,
    devices: RefCell<Vec<DeviceItem>>,
    /// The render nodes found (path, vendor).
    nodes: RefCell<Vec<(String, Option<u16>)>>,
    footer: Footer,
    config: RefCell<UserConfig>,
    syncing: Cell<bool>,
}

impl Parts {
    /// Reads `config.toml` and the render nodes (on the io worker).
    fn read(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.ctx.media.run_io_then(
            || {
                render_nodes()
                    .into_iter()
                    .map(|n| (n.path, n.vendor))
                    .collect::<Vec<_>>()
            },
            move |nodes| {
                if let Some(parts) = weak.upgrade() {
                    *parts.nodes.borrow_mut() = nodes;
                }
            },
        );
        let weak = Rc::downgrade(self);
        super::read_config(&self.ctx, move |config| {
            if let Some(parts) = weak.upgrade() {
                parts.show(&config);
            }
        });
    }

    fn show(&self, config: &UserConfig) {
        *self.config.borrow_mut() = config.clone();
        let view: View = recording_page(config, "");
        self.syncing.set(true);
        self.countdown.set_value(&view.countdown.value);
        match view.preferences.iter().position(|c| c.selected) {
            Some(i) => self.preference.set_active(i as u32),
            None => self.preference.set_active(gtk::INVALID_LIST_POSITION),
        }
        let items = device_items(
            &view.device_auto,
            &self.nodes.borrow(),
            view.vaapi_device.as_deref(),
        );
        if *self.devices.borrow() != items {
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            self.device.set_model(Some(&gtk::StringList::new(&labels)));
            *self.devices.borrow_mut() = items.clone();
        }
        let set = view.vaapi_device.as_deref().unwrap_or("auto");
        let at = items.iter().position(|i| i.value == set).unwrap_or(0);
        if self.device.selected() != at as u32 {
            self.device.set_selected(at as u32);
        }
        self.syncing.set(false);
    }

    fn write(self: &Rc<Self>, key: &str, value: &str) {
        let weak = Rc::downgrade(self);
        let field_key = key == "recording.countdown_secs";
        super::write_setting(&self.ctx, key, value, move |written| {
            let Some(parts) = weak.upgrade() else {
                return;
            };
            match written {
                Ok(config) => {
                    parts.footer.show_error(None);
                    parts.countdown.set_error(None);
                    parts.show(&config);
                }
                Err(why) if field_key => parts.countdown.set_error(Some(&why)),
                Err(why) => {
                    parts.footer.show_error(Some(&why));
                    let config = parts.config.borrow().clone();
                    parts.show(&config);
                }
            }
        });
    }

    fn connect(self: &Rc<Self>) {
        let parts = Rc::downgrade(self);
        self.countdown.on_commit(move |text| {
            if let Some(parts) = parts.upgrade() {
                let value = text.trim().trim_end_matches('s').trim().to_owned();
                parts.write("recording.countdown_secs", &value);
            }
        });
        let parts = Rc::downgrade(self);
        self.preference.connect_active_name_notify(guarded!(
            "settings: the encoder",
            move |group| {
                let Some(parts) = parts.upgrade() else {
                    return;
                };
                if parts.syncing.get() {
                    return;
                }
                if let Some(value) = group.active_name() {
                    parts.write("encoder.preference", &value);
                }
            }
        ));
        let parts = Rc::downgrade(self);
        self.device.connect_selected_notify(guarded!(
            "settings: the VA-API device",
            move |device| {
                let Some(parts) = parts.upgrade() else {
                    return;
                };
                if parts.syncing.get() {
                    return;
                }
                let item = parts
                    .devices
                    .borrow()
                    .get(device.selected() as usize)
                    .cloned();
                if let Some(item) = item {
                    parts.write("encoder.vaapi_device", &item.value);
                }
            }
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrec_shell::gpu::{VENDOR_AMD, VENDOR_INTEL};

    /// Auto, then the Intel and AMD nodes; never NVIDIA's (CLAUDE.md), but
    /// a configured one shows as it is, and so does a node not found.
    #[test]
    fn the_devices_offered() {
        let nodes = vec![
            ("/dev/dri/renderD128".to_owned(), Some(VENDOR_NVIDIA)),
            ("/dev/dri/renderD129".to_owned(), Some(VENDOR_INTEL)),
            ("/dev/dri/renderD130".to_owned(), Some(VENDOR_AMD)),
        ];
        let items = device_items("Auto", &nodes, None);
        let values: Vec<&str> = items.iter().map(|i| i.value.as_str()).collect();
        assert_eq!(
            values,
            ["auto", "/dev/dri/renderD129", "/dev/dri/renderD130"]
        );
        assert_eq!(items[1].label, "/dev/dri/renderD129 \u{b7} Intel");
        let set = device_items("Auto", &nodes, Some("/dev/dri/renderD128"));
        assert_eq!(set.len(), 4);
        assert_eq!(set[3].value, "/dev/dri/renderD128");
        assert!(set[3].label.contains("NVIDIA"), "{:?}", set[3]);
        let gone = device_items("Auto", &[], Some("/dev/dri/renderD131"));
        assert_eq!(gone[1].label, "/dev/dri/renderD131");
        assert_eq!(device_items("Auto", &nodes, Some("auto")).len(), 3);
    }
}
