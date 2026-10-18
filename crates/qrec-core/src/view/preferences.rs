//! The Settings pages besides Project (G§4.11): Appearance (S21) and
//! Keyboard (S22) as drawn; Recording and Autocut (not drawn) in their
//! patterns: groups of 60 px rows, each a label over its caption with its
//! control at the right, and the footer saying where the changes go.
//! Owner: WP9.
//!
//! Each control names the `config.toml` setting it writes
//! (`qrec_shell::config_file::set`, as `qrec config set` does), which the
//! window then applies (the settings follower). Keyboard lists every
//! binding with all its keys (a conflicting one flagged: "Also bound to
//! …"), "default C" when `keys.toml` changed it, and the banner when two
//! bindings share a key (Q5: neither runs; `keymap::conflict_notes`); its
//! edits go through `qrec_shell::keys_file`. The copy is the frames'
//! (S21, S22), but for D-30's caption under Reduce motion.

use serde::{Deserialize, Serialize};

use crate::config::{AutocutDefaults, Theme, UserConfig};
use crate::keymap::{
    Binding, ConflictNote, GROUPS, KeyChord, Keymap, conflict_notes, conflicting_keys,
};
use crate::model::EncoderPref;
use crate::view::autocut::Knob;
use crate::view::cheatsheet::{rebound_note, search_words, word_match};
use crate::view::format::MINUS;

/// A group's header: its title over an optional description (S21's
/// "Theme" over "Changes apply right away. …").
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupHeader {
    pub title: String,
    pub description: Option<String>,
}

/// A row's text: its label over its caption.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowText {
    pub label: String,
    pub caption: Option<String>,
}

/// S21: theme, interface size, reduce motion, the squelch.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearancePage {
    /// "Theme": the four C32 cards.
    pub theme: GroupHeader,
    pub themes: Vec<ThemeCard>,
    /// "Interface": Interface size, Reduce motion.
    pub interface: GroupHeader,
    /// "Interface size": 100% · 125% · 150%.
    pub scale: RowText,
    pub scales: Vec<Choice>,
    /// "Reduce motion", captioned per D-30.
    pub reduced_motion: Switch,
    /// "Sound": the squelch.
    pub sound: GroupHeader,
    pub sounds: Switch,
    /// "Changes are saved to ~/.config/qrec/config.toml."
    pub footer: String,
    /// The path in [`AppearancePage::footer`] (`mono-sm`).
    pub footer_path: String,
}

/// One theme card (C32): a mini window in the theme, its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeCard {
    /// `swamp-night`, … (the tokens' names).
    pub id: String,
    /// "Swamp Night".
    pub name: String,
    /// "Default" after Swamp Night's name.
    pub caption: Option<String>,
    pub selected: bool,
    /// What picking it writes: `("theme", "swamp-day")`.
    pub setting: (String, String),
}

/// One segment of a segmented control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    /// "125%".
    pub label: String,
    pub selected: bool,
    /// `("ui.scale", "1.25")`.
    pub setting: (String, String),
}

/// A switch row.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Switch {
    /// "Reduce motion"; "Squelch on record start and stop".
    pub label: String,
    pub caption: Option<String>,
    pub on: bool,
    /// The setting it writes, with `true` or `false`: `ui.reduced_motion`.
    pub key: String,
}

/// A number row (C29's spin field with its unit): the Recording and
/// Autocut pages.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NumberRow {
    pub text: RowText,
    /// The value as the field shows it: "3", "−35" (the true minus).
    pub value: String,
    /// "s", "dB", "ms".
    pub unit: String,
    /// The setting it writes (`qrec config set` takes the value as typed,
    /// the true minus as `-`).
    pub key: String,
    /// The setting's range, the field's arrow keys' too.
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

/// D-30's caption under Reduce motion (S21's "Follows your system setting
/// until you change it here." no longer fits: the system's setting and the
/// switch both count).
pub const REDUCED_MOTION_CAPTION: &str = "Also on when your system asks for less motion.";

/// The themes in S21's order, with their names.
pub const THEMES: [(Theme, &str, &str); 4] = [
    (Theme::SwampNight, "swamp-night", "Swamp Night"),
    (Theme::SwampDay, "swamp-day", "Swamp Day"),
    (Theme::DragonsKeep, "dragons-keep", "Dragon's Keep"),
    (Theme::OgreMode, "ogre-mode", "Ogre Mode"),
];

/// Interface size's segments: percent and `ui.scale`.
pub const SCALES: [(u32, &str); 3] = [(100, "1"), (125, "1.25"), (150, "1.5")];

/// "Changes are saved to <path>." (S21, S22).
fn saved_to(path: &str) -> String {
    format!("Changes are saved to {path}.")
}

/// The Appearance page of `config`; `config_path` is `config.toml` as
/// people read it (`~/.config/qrec/config.toml`).
pub fn appearance_page(config: &UserConfig, config_path: &str) -> AppearancePage {
    AppearancePage {
        theme: GroupHeader {
            title: "Theme".to_owned(),
            description: Some(
                "Changes apply right away. Every theme keeps the same layout and keys.".to_owned(),
            ),
        },
        themes: THEMES
            .iter()
            .map(|(theme, id, name)| ThemeCard {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
                caption: (*theme == Theme::default()).then(|| "Default".to_owned()),
                selected: config.theme == *theme,
                setting: ("theme".to_owned(), (*id).to_owned()),
            })
            .collect(),
        interface: GroupHeader {
            title: "Interface".to_owned(),
            description: None,
        },
        scale: RowText {
            label: "Interface size".to_owned(),
            caption: Some("Scales text, controls and the timeline together.".to_owned()),
        },
        scales: SCALES
            .iter()
            .map(|(percent, value)| Choice {
                label: format!("{percent}%"),
                // Another scale (`qrec config set ui.scale 1.1`) selects none.
                selected: (config.ui.scale * 100.0 - f64::from(*percent)).abs() < 0.5,
                setting: ("ui.scale".to_owned(), (*value).to_owned()),
            })
            .collect(),
        reduced_motion: Switch {
            label: "Reduce motion".to_owned(),
            caption: Some(REDUCED_MOTION_CAPTION.to_owned()),
            on: config.ui.reduced_motion,
            key: "ui.reduced_motion".to_owned(),
        },
        sound: GroupHeader {
            title: "Sound".to_owned(),
            description: None,
        },
        sounds: Switch {
            label: "Squelch on record start and stop".to_owned(),
            caption: Some("A short, wet squelch. Nothing else in Qrec makes a sound.".to_owned()),
            on: config.ui.sounds,
            key: "ui.sounds".to_owned(),
        },
        footer: saved_to(config_path),
        footer_path: config_path.to_owned(),
    }
}

/// Settings › Recording (not drawn; S21's patterns): the countdown of new
/// projects and the H.264 encoder a project that says auto gets.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingPage {
    /// "New projects": the countdown.
    pub takes: GroupHeader,
    pub countdown: NumberRow,
    /// "Encoder": the preference and the VA-API device.
    pub encoder: GroupHeader,
    pub preference: RowText,
    /// Auto · VA-API · x264 (`encoder.preference`).
    pub preferences: Vec<Choice>,
    pub device: RowText,
    /// `encoder.vaapi_device` as set; `None`: auto.
    pub vaapi_device: Option<String>,
    /// The dropdown's first item ("Auto"); the others are the render nodes
    /// found, as the window lists them.
    pub device_auto: String,
    pub footer: String,
    pub footer_path: String,
}

/// The Recording page of `config` (`config_path` as for
/// [`appearance_page`]).
pub fn recording_page(config: &UserConfig, config_path: &str) -> RecordingPage {
    let preference = config.encoder.preference;
    RecordingPage {
        takes: GroupHeader {
            title: "New projects".to_owned(),
            description: Some(
                "A project keeps its own settings in its qrec.json; these are where new ones start."
                    .to_owned(),
            ),
        },
        countdown: NumberRow {
            text: RowText {
                label: "Countdown".to_owned(),
                caption: Some("Seconds before each take starts; 0 starts at once.".to_owned()),
            },
            value: config.recording.countdown_secs.to_string(),
            unit: "s".to_owned(),
            key: "recording.countdown_secs".to_owned(),
            min: 0.0,
            max: 10.0,
            step: 1.0,
        },
        encoder: GroupHeader {
            title: "Encoder".to_owned(),
            description: Some(
                "How takes and exports encode H.264 when a project says auto.".to_owned(),
            ),
        },
        preference: RowText {
            label: "H.264 encoder".to_owned(),
            caption: Some("VA-API encodes on the GPU, x264 on the CPU.".to_owned()),
        },
        preferences: [
            (EncoderPref::Auto, "Auto", "auto"),
            (EncoderPref::Vaapi, "VA-API", "vaapi"),
            (EncoderPref::X264, "x264", "x264"),
        ]
        .into_iter()
        .map(|(pref, label, value)| Choice {
            label: label.to_owned(),
            selected: preference == pref,
            setting: ("encoder.preference".to_owned(), value.to_owned()),
        })
        .collect(),
        device: RowText {
            label: "VA-API device".to_owned(),
            caption: Some(
                "Auto picks the first Intel or AMD render node, never NVIDIA.".to_owned(),
            ),
        },
        vaapi_device: config.encoder.vaapi_device.clone(),
        device_auto: "Auto".to_owned(),
        footer: saved_to(config_path),
        footer_path: config_path.to_owned(),
    }
}

/// Settings › Autocut (not drawn; S21's patterns with S12's knobs): the
/// autocut settings new projects start with.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocutPage {
    pub group: GroupHeader,
    /// "Run autocut after every take" (S12's switch).
    pub auto_run: Switch,
    /// S12's knobs: Threshold, Min silence, Padding, Min keep.
    pub knobs: Vec<NumberRow>,
    pub footer: String,
    pub footer_path: String,
}

/// The Autocut page of `config` (`config_path` as for
/// [`appearance_page`]).
pub fn autocut_page(config: &UserConfig, config_path: &str) -> AutocutPage {
    let a: &AutocutDefaults = &config.autocut;
    let knob =
        |knob: Knob, value: f64, key: &str, caption: &str, (min, max): (f64, f64)| NumberRow {
            text: RowText {
                label: knob.label().to_owned(),
                caption: Some(caption.to_owned()),
            },
            value: shown_number(value),
            unit: knob.unit().to_owned(),
            key: key.to_owned(),
            min,
            max,
            step: knob.range().2,
        };
    AutocutPage {
        group: GroupHeader {
            title: "New projects".to_owned(),
            description: Some(
                "A project's own autocut settings are on its autocut page (Shift C).".to_owned(),
            ),
        },
        auto_run: Switch {
            label: crate::view::autocut::AUTO_RUN.to_owned(),
            caption: Some("Cuts the silences of each take as it lands.".to_owned()),
            on: a.auto_run_after_take,
            key: "autocut.auto_run_after_take".to_owned(),
        },
        knobs: vec![
            knob(
                Knob::Threshold,
                a.threshold_db,
                "autocut.threshold_db",
                "Anything quieter is silence.",
                (-90.0, 0.0),
            ),
            knob(
                Knob::MinSilence,
                f64::from(a.min_silence_ms),
                "autocut.min_silence_ms",
                "Only silences at least this long are cut.",
                (0.0, 60_000.0),
            ),
            knob(
                Knob::Padding,
                f64::from(a.padding_ms),
                "autocut.padding_ms",
                "Audio kept on each side of speech.",
                (0.0, 60_000.0),
            ),
            knob(
                Knob::MinKeep,
                f64::from(a.min_keep_ms),
                "autocut.min_keep_ms",
                "Kept pieces shorter than this are cut too.",
                (0.0, 60_000.0),
            ),
        ],
        footer: saved_to(config_path),
        footer_path: config_path.to_owned(),
    }
}

/// A value as a field shows it: whole numbers as they are, others with one
/// decimal, the true minus (DESIGN Copy: "−35").
pub fn shown_number(v: f64) -> String {
    let text = if v.fract() == 0.0 {
        format!("{}", v.abs() as i64)
    } else {
        format!("{:.1}", v.abs())
    };
    if v < 0.0 {
        format!("{MINUS}{text}")
    } else {
        text
    }
}

/// S22: the keyboard page.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardPage {
    /// Q5: the keys two bindings share, which run neither.
    pub banner: Option<ConflictBanner>,
    /// The search field's placeholder.
    pub placeholder: String,
    pub groups: Vec<KeyGroup>,
    /// "Changes are saved to ~/.config/qrec/keys.toml."
    pub footer: String,
    /// The path in [`KeyboardPage::footer`] (`mono-sm`).
    pub footer_path: String,
}

/// S22's conflict banner (a `warning` tint and stroke, `md` radius).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictBanner {
    /// "1 conflict in ~/.config/qrec/keys.toml".
    pub title: String,
    /// The path at the end of [`ConflictBanner::title`] (S22's mono part).
    pub path: String,
    /// One per conflict: "X is bound to both Autocut and Remove clips.
    /// Neither runs until one is changed."
    pub lines: Vec<String>,
    /// "Open keys.toml".
    pub button: String,
    /// The data: each key and the bindings that share it.
    pub conflicts: Vec<ConflictNote>,
}

/// A group of bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyGroup {
    pub title: String,
    pub rows: Vec<KeyRow>,
}

/// One binding (S22: label · key caps · state · edit and reset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyRow {
    pub id: String,
    pub label: String,
    /// Char indices of `label` the search matched.
    pub matched: Vec<usize>,
    /// Every key of the binding, a conflicting one flagged; "…" alone
    /// while it captures a key.
    pub keys: Vec<RowKey>,
    pub state: KeyState,
    /// "default C" when rebound.
    pub default_keys: Option<String>,
    /// "Also bound to Remove clips".
    pub conflict: Option<String>,
    /// The row shows its reset button (`keys.toml` sets it).
    pub resettable: bool,
    /// "Press a key · Esc cancels · Backspace clears" while it captures.
    pub capture: Option<String>,
}

/// One key cap group of a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowKey {
    /// Key-cap legends.
    pub caps: Vec<String>,
    /// Another binding has this key too (it runs neither).
    pub conflict: bool,
}

/// A binding's state on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyState {
    Default,
    /// `keys.toml` changed its keys.
    Rebound,
    /// One of its keys is another binding's too.
    Conflict,
}

/// S22's capture caption.
pub const CAPTURE_CAPTION: &str = "Press a key \u{b7} Esc cancels \u{b7} Backspace clears";

/// The Keyboard page; `keys_path` is `keys.toml` as people read it,
/// `capturing` the binding waiting for its new key.
pub fn keyboard_page(
    keymap: &Keymap,
    keys_path: &str,
    search: &str,
    capturing: Option<&str>,
) -> KeyboardPage {
    let words = search_words(search);
    let groups = GROUPS
        .iter()
        .filter_map(|group| {
            let rows: Vec<KeyRow> = keymap
                .bindings
                .iter()
                .filter(|b| b.group == *group)
                .filter_map(|b| {
                    let matched = word_match(&words, b.label)?;
                    Some(KeyRow {
                        matched,
                        ..key_row(keymap, b, capturing == Some(b.id))
                    })
                })
                .collect();
            (!rows.is_empty()).then(|| KeyGroup {
                title: (*group).to_owned(),
                rows,
            })
        })
        .collect();
    KeyboardPage {
        banner: banner(keymap, keys_path),
        placeholder: "Search shortcuts".to_owned(),
        groups,
        footer: saved_to(keys_path),
        footer_path: keys_path.to_owned(),
    }
}

fn key_row(keymap: &Keymap, b: &Binding, capturing: bool) -> KeyRow {
    let conflicted = conflicting_keys(keymap, b.id);
    let keys = if capturing {
        vec![RowKey {
            caps: vec!["\u{2026}".to_owned()],
            conflict: false,
        }]
    } else {
        b.keys
            .iter()
            .map(|k| RowKey {
                caps: k.cap_legends(),
                conflict: conflicted.contains(&k),
            })
            .collect()
    };
    let others = sharing(keymap, b.id, &conflicted);
    let rebound = rebound_note(b);
    KeyRow {
        id: b.id.to_owned(),
        label: b.label.to_owned(),
        matched: Vec::new(),
        keys,
        state: if !others.is_empty() {
            KeyState::Conflict
        } else if rebound.is_some() {
            KeyState::Rebound
        } else {
            KeyState::Default
        },
        resettable: rebound.is_some(),
        default_keys: rebound,
        conflict: (!others.is_empty()).then(|| format!("Also bound to {}", and_list(&others))),
        capture: capturing.then(|| CAPTURE_CAPTION.to_owned()),
    }
}

/// The labels of the other bindings that share one of `id`'s `keys`.
fn sharing(keymap: &Keymap, id: &str, keys: &[&KeyChord]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in keymap.conflicts.iter().filter(|c| keys.contains(&&c.chord)) {
        for other in c.ids.iter().filter(|o| *o != id) {
            if let Some(label) = keymap.binding(other).map(|b| b.label.to_owned())
                && !out.contains(&label)
            {
                out.push(label);
            }
        }
    }
    out
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// S22's banner: "1 conflict in ~/.config/qrec/keys.toml", then one line
/// per conflict; `None` without one.
fn banner(keymap: &Keymap, keys_path: &str) -> Option<ConflictBanner> {
    let conflicts = conflict_notes(keymap);
    if conflicts.is_empty() {
        return None;
    }
    let lines = conflicts
        .iter()
        .map(|c| match c.labels.as_slice() {
            [a, b] => format!(
                "{} is bound to both {a} and {b}. Neither runs until one is changed.",
                c.key
            ),
            labels => format!(
                "{} is bound to {}. None of them runs until only one has it.",
                c.key,
                and_list(labels)
            ),
        })
        .collect();
    let n = conflicts.len();
    Some(ConflictBanner {
        title: format!(
            "{n} {} in {keys_path}",
            if n == 1 { "conflict" } else { "conflicts" }
        ),
        path: keys_path.to_owned(),
        lines,
        button: "Open keys.toml".to_owned(),
        conflicts,
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::keymap::resolve;
    use crate::view::palette::bracketed;

    const KEYS: &str = "~/.config/qrec/keys.toml";
    const CONFIG: &str = "~/.config/qrec/config.toml";

    /// S21 as drawn: the copy word for word (D-30's caption under Reduce
    /// motion), the selection following the config, each control writing
    /// a setting `qrec config set` takes.
    #[test]
    fn snapshot_s21_appearance() {
        let page = appearance_page(&UserConfig::default(), CONFIG);
        insta::assert_json_snapshot!("appearance_s21", page);
        assert_eq!(page.theme.title, "Theme");
        assert_eq!(
            page.theme.description.as_deref(),
            Some("Changes apply right away. Every theme keeps the same layout and keys.")
        );
        assert_eq!(page.interface.title, "Interface");
        assert_eq!(page.scale.label, "Interface size");
        assert_eq!(
            page.scale.caption.as_deref(),
            Some("Scales text, controls and the timeline together.")
        );
        assert_eq!(page.sound.title, "Sound");
        assert_eq!(page.sounds.label, "Squelch on record start and stop");
        assert_eq!(
            page.sounds.caption.as_deref(),
            Some("A short, wet squelch. Nothing else in Qrec makes a sound.")
        );
        assert_eq!(
            page.footer,
            "Changes are saved to ~/.config/qrec/config.toml."
        );
        let names: Vec<&str> = page.themes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            ["Swamp Night", "Swamp Day", "Dragon's Keep", "Ogre Mode"]
        );
        let mut config = UserConfig {
            theme: Theme::DragonsKeep,
            ..UserConfig::default()
        };
        config.ui.scale = 1.25;
        config.ui.reduced_motion = true;
        config.ui.sounds = true;
        let page = appearance_page(&config, CONFIG);
        let selected: Vec<&str> = page
            .themes
            .iter()
            .filter(|t| t.selected)
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(selected, ["Dragon's Keep"]);
        assert_eq!(page.themes[0].caption.as_deref(), Some("Default"));
        assert!(page.themes[1..].iter().all(|t| t.caption.is_none()));
        let scales: Vec<(&str, bool)> = page
            .scales
            .iter()
            .map(|c| (c.label.as_str(), c.selected))
            .collect();
        assert_eq!(scales, [("100%", false), ("125%", true), ("150%", false)]);
        assert!(page.reduced_motion.on && page.sounds.on);
        assert_eq!(
            page.reduced_motion.caption.as_deref(),
            Some("Also on when your system asks for less motion.")
        );
        // A scale between the segments selects none.
        config.ui.scale = 1.1;
        assert!(
            appearance_page(&config, CONFIG)
                .scales
                .iter()
                .all(|c| !c.selected)
        );
        // Each control writes a setting `qrec config set` takes.
        let mut text = String::new();
        for card in &page.themes {
            text = crate::config::set_text(&text, &card.setting.0, &card.setting.1).unwrap();
            assert_eq!(
                crate::config::parse(&text).unwrap().theme,
                THEMES.iter().find(|t| t.1 == card.id).unwrap().0
            );
        }
        for choice in &page.scales {
            text = crate::config::set_text(&text, &choice.setting.0, &choice.setting.1).unwrap();
        }
        assert_eq!(crate::config::parse(&text).unwrap().ui.scale, 1.5);
        for switch in [&page.reduced_motion, &page.sounds] {
            text = crate::config::set_text(&text, &switch.key, "true").unwrap();
        }
        let cfg = crate::config::parse(&text).unwrap();
        assert!(cfg.ui.reduced_motion && cfg.ui.sounds);
    }

    fn text(page: &KeyboardPage) -> String {
        let mut out = String::new();
        if let Some(banner) = &page.banner {
            let _ = writeln!(out, "! {} <{}>", banner.title, banner.button);
            for line in &banner.lines {
                let _ = writeln!(out, "!   {line}");
            }
        }
        for group in &page.groups {
            let _ = writeln!(out, "## {}", group.title);
            for row in &group.rows {
                let caps: Vec<String> = row
                    .keys
                    .iter()
                    .map(|k| {
                        let cap: String = k.caps.iter().map(|c| format!("[{c}]")).collect();
                        if k.conflict { format!("{cap}!") } else { cap }
                    })
                    .collect();
                let _ = write!(
                    out,
                    "{:<40} {:<24} {:?}",
                    bracketed(&row.label, &row.matched),
                    caps.join(" or "),
                    row.state
                );
                for note in [&row.default_keys, &row.conflict, &row.capture]
                    .into_iter()
                    .flatten()
                {
                    let _ = write!(out, " · {note}");
                }
                if row.resettable {
                    out.push_str(" · <reset>");
                }
                out.push('\n');
            }
        }
        let _ = writeln!(out, "-- {}", page.footer);
        out
    }

    /// S22: Autocut rebound to X, which Remove clips has: the banner, the
    /// flags on both rows; Save frame rebound to F; Toggle snapping
    /// capturing. The copy is the frame's, word for word.
    #[test]
    fn snapshot_s22_keyboard() {
        let map = resolve("[bindings]\nautocut.run = \"x\"\nframe.save = \"f\"\n");
        let page = keyboard_page(&map, KEYS, "", Some("snapping.toggle"));
        insta::assert_snapshot!("keyboard_s22", text(&page));
        let banner = page.banner.as_ref().unwrap();
        assert_eq!(banner.title, "1 conflict in ~/.config/qrec/keys.toml");
        assert_eq!(banner.path, KEYS);
        assert_eq!(
            banner.lines,
            ["X is bound to both Autocut and Remove clips. Neither runs until one is changed."]
        );
        assert_eq!(banner.button, "Open keys.toml");
        assert_eq!(banner.conflicts[0].key, "X");
        let row = |id: &str| {
            page.groups
                .iter()
                .flat_map(|g| &g.rows)
                .find(|r| r.id == id)
                .unwrap()
                .clone()
        };
        let remove = row("clip.remove");
        assert_eq!(remove.label, "Remove clips");
        assert_eq!(remove.state, KeyState::Conflict);
        assert_eq!(
            remove.keys.iter().map(|k| k.conflict).collect::<Vec<_>>(),
            [false, true, false],
            "the flag on X"
        );
        assert_eq!(remove.conflict.as_deref(), Some("Also bound to Autocut"));
        assert!(!remove.resettable);
        let autocut = row("autocut.run");
        assert_eq!(autocut.label, "Autocut");
        assert_eq!(autocut.state, KeyState::Conflict);
        assert_eq!(autocut.default_keys.as_deref(), Some("default C"));
        assert_eq!(
            autocut.conflict.as_deref(),
            Some("Also bound to Remove clips")
        );
        assert!(autocut.resettable);
        let save = row("frame.save");
        assert_eq!(save.label, "Save frame as PNG");
        assert_eq!(save.state, KeyState::Rebound);
        assert_eq!(save.default_keys.as_deref(), Some("default P"));
        let snapping = row("snapping.toggle");
        assert_eq!(snapping.keys[0].caps, ["\u{2026}"]);
        assert_eq!(
            snapping.capture.as_deref(),
            Some("Press a key · Esc cancels · Backspace clears")
        );
        assert_eq!(row("clip.split").state, KeyState::Default);
        assert_eq!(row("clip.trim-start").label, "Trim start to playhead");
        assert_eq!(
            page.footer,
            "Changes are saved to ~/.config/qrec/keys.toml."
        );
        assert_eq!(page.footer_path, KEYS);
    }

    #[test]
    fn the_banner_counts_conflicts_and_the_search_filters() {
        assert!(keyboard_page(&resolve(""), KEYS, "", None).banner.is_none());
        let map = resolve(
            "[bindings]\nautocut.run = \"x\"\nzoom.selection = \"r\"\nbin.toggle = \"s\"\nzoom.fit = \"s\"\n",
        );
        let page = keyboard_page(&map, KEYS, "", None);
        let banner = page.banner.unwrap();
        assert_eq!(banner.title, "3 conflicts in ~/.config/qrec/keys.toml");
        assert_eq!(banner.conflicts.len(), 3, "{:?}", banner.conflicts);
        assert!(
            banner.lines.iter().any(|l| l.starts_with("S is bound to ")
                && l.contains("Toggle bin")
                && l.contains("Zoom to fit")
                && l.contains("Split clip")
                && l.ends_with("None of them runs until only one has it.")),
            "{:?}",
            banner.lines
        );
        let page = keyboard_page(&resolve(""), KEYS, "zoom", None);
        let ids: Vec<&str> = page
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(ids, ["zoom.in", "zoom.out", "zoom.fit", "zoom.selection"]);
        assert_eq!(page.groups.len(), 1);
        assert!(
            keyboard_page(&resolve(""), KEYS, "xyzzy", None)
                .groups
                .is_empty()
        );
    }

    /// SPEC §5.5: `recording.countdownSecs` defaults to 0 (BRIEF's S03
    /// mock sets 3 for its shot alone), everywhere a default comes from: a
    /// project without one, `config.toml` without one (none at all, or one
    /// that sets something else), what `Init` starts a project from, and so
    /// Settings › Recording on a fresh config: "0" s. (The snapshot below
    /// sets 3 to show a configured value.)
    #[test]
    fn the_countdown_defaults_to_specs_0() {
        use crate::model::RecordingSettings;
        assert_eq!(RecordingSettings::default().countdown_secs, 0);
        let fresh = UserConfig::default();
        assert_eq!(fresh.recording.countdown_secs, 0);
        assert_eq!(fresh.project_defaults().recording.countdown_secs, 0);
        let other = crate::config::parse("theme = \"swamp-day\"\n[recording]\n").unwrap();
        assert_eq!(other.recording.countdown_secs, 0);
        assert_eq!(recording_page(&fresh, CONFIG).countdown.value, "0");
        assert_eq!(recording_page(&other, CONFIG).countdown.value, "0");
    }

    /// The pages not drawn follow S21's pattern; each control writes a
    /// setting `qrec config set` takes, with the value its field shows.
    #[test]
    fn the_undrawn_pages_show_the_config() {
        let mut config = UserConfig::default();
        config.recording.countdown_secs = 3;
        config.encoder.vaapi_device = Some("/dev/dri/renderD129".into());
        config.autocut.padding_ms = 80;
        let recording = recording_page(&config, CONFIG);
        insta::assert_json_snapshot!("recording_page", recording);
        assert_eq!(recording.countdown.value, "3");
        assert_eq!(recording.countdown.unit, "s");
        assert_eq!(
            recording.vaapi_device.as_deref(),
            Some("/dev/dri/renderD129")
        );
        let prefs: Vec<(&str, bool)> = recording
            .preferences
            .iter()
            .map(|c| (c.label.as_str(), c.selected))
            .collect();
        assert_eq!(prefs, [("Auto", true), ("VA-API", false), ("x264", false)]);
        let autocut = autocut_page(&config, CONFIG);
        insta::assert_json_snapshot!("autocut_page", autocut);
        assert_eq!(autocut.auto_run.label, "Run autocut after every take");
        let knobs: Vec<(&str, &str, &str)> = autocut
            .knobs
            .iter()
            .map(|k| (k.text.label.as_str(), k.value.as_str(), k.unit.as_str()))
            .collect();
        assert_eq!(
            knobs,
            [
                ("Threshold", "\u{2212}35", "dB"),
                ("Min silence", "400", "ms"),
                ("Padding", "80", "ms"),
                ("Min keep", "200", "ms"),
            ]
        );
        let mut text = String::new();
        for row in std::iter::once(&recording.countdown).chain(&autocut.knobs) {
            let typed = row.value.replace('\u{2212}', "-");
            text = crate::config::set_text(&text, &row.key, &typed).unwrap();
        }
        for choice in &recording.preferences {
            text = crate::config::set_text(&text, &choice.setting.0, &choice.setting.1).unwrap();
        }
        text = crate::config::set_text(&text, &autocut.auto_run.key, "true").unwrap();
        let cfg = crate::config::parse(&text).unwrap();
        assert_eq!(cfg.recording.countdown_secs, 3);
        assert_eq!(cfg.autocut.padding_ms, 80);
        assert_eq!(cfg.autocut.threshold_db, -35.0);
        assert!(cfg.autocut.auto_run_after_take);
        assert_eq!(cfg.encoder.preference, EncoderPref::X264);
        assert_eq!(shown_number(-35.5), "\u{2212}35.5");
        assert_eq!(shown_number(0.0), "0");
    }
}
