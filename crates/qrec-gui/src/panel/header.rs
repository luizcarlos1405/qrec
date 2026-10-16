//! C13: the panel header (48 px: "Onion layers", the subtitle, the `+`
//! Add layer button), the take banner ("Changes here only affect take 5."
//! and `Layout ▾`, S06) and the section headers ("Layers 2").

use gtk::prelude::*;
use qrec_core::view::panel::PanelHeader;

use super::menus::{self, Menu};
use crate::Ctx;

/// The header of the layers page.
pub struct HeaderView {
    root: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    add: gtk::MenuButton,
}

impl HeaderView {
    /// `add_menu` is the Add layer menu (shared with "+ Add layer").
    pub fn new(ctx: &Ctx, add_menu: &Menu) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.add_css_class("panel-header");
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_valign(gtk::Align::Center);
        titles.set_hexpand(true);
        let title = gtk::Label::new(None);
        title.add_css_class("t-title");
        title.set_xalign(0.0);
        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("t-caption");
        subtitle.add_css_class("muted");
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        titles.append(&title);
        titles.append(&subtitle);
        let add = menus::menu_button(
            add_menu,
            &["flat", "icon-32", "panel-add-icon"],
            "Add layer",
        );
        add.set_icon_name("list-add-symbolic");
        add.set_widget_name("panel-add-layer");
        add.set_valign(gtk::Align::Center);
        let refresh = {
            let ctx = ctx.clone();
            std::rc::Rc::new(move || menus::refresh_sources(&ctx)) as std::rc::Rc<dyn Fn()>
        };
        menus::track_menu(ctx, &add, Some(refresh));
        root.append(&titles);
        root.append(&add);
        Self {
            root,
            title,
            subtitle,
            add,
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Shows `header`; `add` is whether sources can be added now, and why
    /// not (the disabled button's tooltip, D-07).
    pub fn update(&self, header: &PanelHeader, add: &Result<(), String>) {
        self.title.set_text(&header.title);
        self.subtitle.set_text(&header.subtitle);
        set_enabled_with_reason(self.add.upcast_ref(), add, "Add layer");
    }
}

/// The take banner (S06): shown only in a take layout.
pub struct BannerView {
    root: gtk::Box,
    text: gtk::Label,
    layout: gtk::MenuButton,
    menu: Menu,
}

impl BannerView {
    pub fn new(ctx: &Ctx) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.add_css_class("take-banner");
        root.set_widget_name("take-banner");
        let text = gtk::Label::new(None);
        text.add_css_class("t-body");
        text.set_xalign(0.0);
        text.set_hexpand(true);
        text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let menu = Menu::new();
        let layout = menus::menu_button(&menu, &["flat", "sm"], "Take layout");
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let label = gtk::Label::new(Some("Layout"));
        label.add_css_class("t-body-strong");
        content.append(&label);
        content.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
        layout.set_child(Some(&content));
        layout.set_valign(gtk::Align::Center);
        layout.set_widget_name("layout-menu");
        menus::track_menu(ctx, &layout, None);
        root.append(&text);
        root.append(&layout);
        root.set_visible(false);
        Self {
            root,
            text,
            layout,
            menu,
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Shows the banner of `header` (hidden without one); `enabled`: the
    /// gates of `SceneApply`.
    pub fn update(&self, header: &PanelHeader, enabled: &Result<(), String>) {
        let Some(banner) = &header.banner else {
            self.root.set_visible(false);
            return;
        };
        self.text.set_text(banner);
        let signature = format!("{:?}{enabled:?}", header.layout_menu);
        let items = header.layout_menu.clone();
        let ok = enabled.is_ok();
        self.menu.update(signature, |menu| {
            for item in &items {
                let cmd = item.command();
                menu.append_item(&menus::command_item(&item.label, ok.then_some(&cmd)));
            }
        });
        set_enabled_with_reason(self.layout.upcast_ref(), enabled, "Take layout actions");
        self.root.set_visible(true);
    }
}

/// A section header (32 px): `heading` and a `caption` `muted` count.
pub struct SectionHeader {
    root: gtk::Box,
    count: gtk::Label,
}

impl SectionHeader {
    pub fn new(heading: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.add_css_class("section-header");
        let title = gtk::Label::new(Some(heading));
        title.add_css_class("t-heading");
        title.set_valign(gtk::Align::Baseline);
        let count = gtk::Label::new(None);
        count.add_css_class("t-caption");
        count.add_css_class("muted");
        count.set_valign(gtk::Align::Baseline);
        count.set_ellipsize(gtk::pango::EllipsizeMode::End);
        root.append(&title);
        root.append(&count);
        Self { root, count }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// The caption after the heading ("2", "Face · L2"; empty hides it).
    pub fn set_caption(&self, caption: &str) {
        self.count.set_text(caption);
        self.count.set_visible(!caption.is_empty());
    }
}

/// Enables `widget`, or disables it with `reason` as its tooltip (a
/// disabled control says why, D-07, D-43); `action` names it otherwise.
pub fn set_enabled_with_reason(widget: &gtk::Widget, enabled: &Result<(), String>, action: &str) {
    match enabled {
        Ok(()) => {
            widget.set_sensitive(true);
            widget.set_tooltip_text(Some(action));
        }
        Err(reason) => {
            widget.set_sensitive(false);
            widget.set_tooltip_text(Some(reason));
        }
    }
}
