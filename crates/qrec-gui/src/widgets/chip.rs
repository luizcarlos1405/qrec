//! C22 status chip (G§5.1): a small label in a tinted pill, with an
//! optional icon (`.chip` plus the level's class in common.css).

use gtk::prelude::*;

/// The chip's level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChipLevel {
    #[default]
    Neutral,
    Success,
    Warning,
    Error,
    Accent,
}

impl ChipLevel {
    fn class(self) -> &'static str {
        match self {
            ChipLevel::Neutral => "neutral",
            ChipLevel::Success => "success",
            ChipLevel::Warning => "warning",
            ChipLevel::Error => "error",
            ChipLevel::Accent => "accent",
        }
    }

    const ALL: [ChipLevel; 5] = [
        ChipLevel::Neutral,
        ChipLevel::Success,
        ChipLevel::Warning,
        ChipLevel::Error,
        ChipLevel::Accent,
    ];
}

/// A status chip ("Recovered", "A1", "Autocut").
#[derive(Debug, Clone)]
pub struct Chip {
    root: gtk::Box,
    label: gtk::Label,
    icon: gtk::Image,
}

impl Chip {
    pub fn new(text: &str, level: ChipLevel) -> Self {
        // 4 px between the icon and the text: `.chip`'s CSS border-spacing
        // (GtkBox adds it to its own spacing, which stays 0).
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("chip");
        root.set_valign(gtk::Align::Center);
        let icon = gtk::Image::new();
        icon.set_visible(false);
        let label = gtk::Label::new(Some(text));
        root.append(&icon);
        root.append(&label);
        let chip = Self { root, label, icon };
        chip.set_level(level);
        chip
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn set_text(&self, text: &str) {
        self.label.set_text(text);
    }

    /// An icon before the text (`None` hides it).
    pub fn set_icon(&self, icon: Option<&str>) {
        match icon {
            Some(name) => {
                self.icon.set_icon_name(Some(name));
                self.icon.set_visible(true);
            }
            None => self.icon.set_visible(false),
        }
    }

    pub fn set_level(&self, level: ChipLevel) {
        for l in ChipLevel::ALL {
            self.root.remove_css_class(l.class());
        }
        self.root.add_css_class(level.class());
    }
}
