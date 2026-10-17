//! The Settings pages besides Project (G§4.11, S21, S22): Appearance and
//! Keyboard as drafted; Recording and Autocut not drawn yet (their values
//! only, until frames or a waiver, D-20). Owner: WP9.
//!
//! Each control of Appearance names the `config.toml` setting it writes
//! (`qrec_shell::config_file::set`, as `qrec config set` does): a window
//! that follows the file applies it (WP10's watcher). Keyboard lists every
//! binding with all its keys (a conflicting one flagged: "Also bound to
//! …"), "default C" when `keys.toml` changed it, and the banner when two
//! bindings share a key (Q5: neither runs; `keymap::conflict_notes`); its
//! edits go through `qrec_shell::keys_file`. Labels are the core keymap's
//! (D-31); the pages' own copy is BRIEF S21's and S22's until their frames
//! are drawn into the build, but for D-30's caption.

use serde::{Deserialize, Serialize};

use crate::config::{Theme, UserConfig};
use crate::keymap::{
    Binding, ConflictNote, GROUPS, KeyChord, Keymap, conflict_notes, conflicting_keys,
};
use crate::view::cheatsheet::{rebound_note, search_words, word_match};

/// S21: theme, interface size, reduce motion, sounds.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearancePage {
    /// "Theme": the four C32 cards.
    pub theme_title: String,
    pub themes: Vec<ThemeCard>,
    /// "Interface size": 100 % · 125 % · 150 %.
    pub scale_title: String,
    pub scales: Vec<Choice>,
    /// "Reduce motion", captioned per D-30.
    pub reduced_motion: Switch,
    /// "Sounds": "Squelch on record start and stop".
    pub sounds: Switch,
}

/// One theme card (C32): a mini window in the theme, its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeCard {
    /// `swamp-night`, … (the tokens' names).
    pub id: String,
    /// "Swamp Night".
    pub name: String,
    /// "Default" under Swamp Night.
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
    /// The row's title ("Reduce motion"; "Sounds").
    pub title: String,
    /// The switch's own label, when it has one ("Squelch on record start
    /// and stop").
    pub label: Option<String>,
    pub caption: Option<String>,
    pub on: bool,
    /// The setting it writes, with `true` or `false`: `ui.reduced_motion`.
    pub key: String,
}

/// D-30's caption under Reduce motion (BRIEF's "Follows your system
/// setting until you change it here." no longer fits: the system's setting
/// and the switch both count).
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

/// The Appearance page of `config`.
pub fn appearance_page(config: &UserConfig) -> AppearancePage {
    AppearancePage {
        theme_title: "Theme".to_owned(),
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
        scale_title: "Interface size".to_owned(),
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
            title: "Reduce motion".to_owned(),
            label: None,
            caption: Some(REDUCED_MOTION_CAPTION.to_owned()),
            on: config.ui.reduced_motion,
            key: "ui.reduced_motion".to_owned(),
        },
        sounds: Switch {
            title: "Sounds".to_owned(),
            label: Some("Squelch on record start and stop".to_owned()),
            caption: None,
            on: config.ui.sounds,
            key: "ui.sounds".to_owned(),
        },
    }
}

/// Settings › Recording (not drawn yet): the user defaults for recording.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingPage {
    pub countdown_secs: u32,
    /// `auto`, `vaapi` or `x264`.
    pub encoder: String,
    /// The VA-API render node; `None` lets Qrec pick (never NVIDIA).
    pub vaapi_device: Option<String>,
}

/// The Recording page of `config`.
pub fn recording_page(config: &UserConfig) -> RecordingPage {
    RecordingPage {
        countdown_secs: config.recording.countdown_secs,
        encoder: serde_json::to_value(config.encoder.preference)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        vaapi_device: config.encoder.vaapi_device.clone(),
    }
}

/// Settings › Autocut (not drawn yet): the defaults of new projects.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocutPage {
    pub auto_run_after_take: bool,
    pub threshold_db: f64,
    pub min_silence_ms: u32,
    pub padding_ms: u32,
    pub min_keep_ms: u32,
}

/// The Autocut page of `config`.
pub fn autocut_page(config: &UserConfig) -> AutocutPage {
    let a = &config.autocut;
    AutocutPage {
        auto_run_after_take: a.auto_run_after_take,
        threshold_db: a.threshold_db,
        min_silence_ms: a.min_silence_ms,
        padding_ms: a.padding_ms,
        min_keep_ms: a.min_keep_ms,
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
}

/// S22's conflict banner (a `warning` tint and stroke, `md` radius).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictBanner {
    /// "1 conflict in ~/.config/qrec/keys.toml".
    pub title: String,
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
        footer: format!("Changes are saved to {keys_path}."),
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

    #[test]
    fn snapshot_s21_appearance() {
        insta::assert_json_snapshot!("appearance_s21", appearance_page(&UserConfig::default()));
        let mut config = UserConfig {
            theme: Theme::DragonsKeep,
            ..UserConfig::default()
        };
        config.ui.scale = 1.25;
        config.ui.reduced_motion = true;
        config.ui.sounds = true;
        let page = appearance_page(&config);
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
        assert!(appearance_page(&config).scales.iter().all(|c| !c.selected));
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
                    caps.join(" "),
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
    /// capturing.
    #[test]
    fn snapshot_s22_keyboard() {
        let map = resolve("[bindings]\nautocut.run = \"x\"\nframe.save = \"f\"\n");
        let page = keyboard_page(&map, KEYS, "", Some("snapping.toggle"));
        insta::assert_snapshot!("keyboard_s22", text(&page));
        let banner = page.banner.as_ref().unwrap();
        assert_eq!(banner.title, "1 conflict in ~/.config/qrec/keys.toml");
        assert_eq!(
            banner.lines,
            [
                "X is bound to both Autocut the selection, or every clip and Remove selected \
              clips (to the bin). Neither runs until one is changed."
            ]
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
        assert_eq!(remove.state, KeyState::Conflict);
        assert_eq!(
            remove.keys.iter().map(|k| k.conflict).collect::<Vec<_>>(),
            [false, true, false],
            "the flag on X"
        );
        assert_eq!(
            remove.conflict.as_deref(),
            Some("Also bound to Autocut the selection, or every clip")
        );
        assert!(!remove.resettable);
        let autocut = row("autocut.run");
        assert_eq!(autocut.state, KeyState::Conflict);
        assert_eq!(autocut.default_keys.as_deref(), Some("default C"));
        assert!(autocut.resettable);
        let save = row("frame.save");
        assert_eq!(save.state, KeyState::Rebound);
        assert_eq!(save.default_keys.as_deref(), Some("default P"));
        let snapping = row("snapping.toggle");
        assert_eq!(snapping.keys[0].caps, ["\u{2026}"]);
        assert_eq!(
            snapping.capture.as_deref(),
            Some("Press a key · Esc cancels · Backspace clears")
        );
        assert_eq!(row("clip.split").state, KeyState::Default);
        assert_eq!(
            page.footer,
            "Changes are saved to ~/.config/qrec/keys.toml."
        );
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
                && l.contains("Toggle the bin")
                && l.contains("Zoom to fit")
                && l.contains("Split clip at the playhead")
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

    #[test]
    fn the_undrawn_pages_show_the_config() {
        let mut config = UserConfig::default();
        config.recording.countdown_secs = 3;
        config.encoder.vaapi_device = Some("/dev/dri/renderD129".into());
        config.autocut.padding_ms = 80;
        assert_eq!(
            recording_page(&config),
            RecordingPage {
                countdown_secs: 3,
                encoder: "auto".into(),
                vaapi_device: Some("/dev/dri/renderD129".into()),
            }
        );
        assert_eq!(autocut_page(&config).padding_ms, 80);
        assert_eq!(autocut_page(&config).threshold_db, -35.0);
    }
}
