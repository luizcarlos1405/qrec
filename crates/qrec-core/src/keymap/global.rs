//! Global shortcuts (SPEC §10.2, requirement 25): keys that run a Qrec
//! command while another app has focus. Qrec grabs no keys itself; the
//! desktop binds them, and [`global_snippet`] says how on the desktop Qrec
//! runs on (`platform::Desktop`): lines for the config file of niri (SPEC
//! §10.2's snippet), sway, i3, Hyprland and river; the steps in GNOME's and
//! KDE Plasma's settings with the three commands; the commands and their
//! keys for any other desktop. The cheat sheet's Global shortcuts group
//! shows and copies it, `qrec doctor` prints it (JSON `shortcuts`), and so
//! does the agent guide.

use serde::{Deserialize, Serialize};

use crate::platform::Desktop;

/// A global shortcut: what it runs and the key the snippets bind it to,
/// with the desktop's modifier and Shift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalShortcut {
    /// Its cheat-sheet row's id: `global.record-toggle`.
    pub id: &'static str,
    /// "Record toggle".
    pub label: &'static str,
    /// The letter: `R`.
    pub key: &'static str,
    /// The command it runs.
    pub argv: &'static [&'static str],
}

/// The global shortcuts, in the snippets' order.
pub const GLOBAL_SHORTCUTS: [GlobalShortcut; 3] = [
    GlobalShortcut {
        id: "global.record-toggle",
        label: "Record toggle",
        key: "R",
        argv: &["qrec", "record", "toggle"],
    },
    GlobalShortcut {
        id: "global.record-over",
        label: "Record over",
        key: "O",
        argv: &["qrec", "record", "start", "--over-selection"],
    },
    GlobalShortcut {
        id: "global.discard-last",
        label: "Discard last take",
        key: "D",
        argv: &["qrec", "take", "discard-last"],
    },
];

/// The niri key binds (SPEC §10.2, verbatim): niri's [`global_snippet`],
/// and the doctor's JSON `niriSnippet` on any desktop.
pub const NIRI_SNIPPET: &str = "binds {
    Mod+Shift+R { spawn \"qrec\" \"record\" \"toggle\"; }
    Mod+Shift+O { spawn \"qrec\" \"record\" \"start\" \"--over-selection\"; }
    Mod+Shift+D { spawn \"qrec\" \"take\" \"discard-last\"; }
}";

/// The copy button's label, whatever the desktop.
pub const COPY_SNIPPET: &str = "Copy snippet";

/// How to bind [`GLOBAL_SHORTCUTS`] on a desktop ([`global_snippet`]).
/// JSON (the doctor's `shortcuts`): `{"desktop": "niri", "language":
/// "kdl", "file": "~/.config/niri/config.kdl", "heading": …, "text": …,
/// "note": …, "button": "Copy snippet", "modifier": "Mod"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GlobalSnippet {
    /// The desktop it is written for; `None` for the generic list, which
    /// every desktop without its own snippet gets.
    pub desktop: Option<Desktop>,
    /// What `text` is written in, as a Markdown code block names it: `kdl`,
    /// `swayconfig`, `i3config`, `hyprlang`, `sh` or `text`.
    pub language: String,
    /// The file `text` goes into, as people write it
    /// (`~/.config/niri/config.kdl`); `None` when the shortcuts are added
    /// one by one in the desktop's settings.
    pub file: Option<String>,
    /// The doctor's line over `text`: "Global shortcuts for niri (paste
    /// into ~/.config/niri/config.kdl):".
    pub heading: String,
    /// What to copy: the lines for `file`, else the steps and each
    /// shortcut's name, command and keys.
    pub text: String,
    /// The cheat sheet's sentence: "Bind these in niri to record while
    /// another app has focus."
    pub note: String,
    /// The copy button: [`COPY_SNIPPET`].
    pub button: String,
    /// The key cap of the modifier the keys use with Shift: `Mod` (the
    /// compositor's own, as its config names it), `Super`, or `Meta` (KDE
    /// Plasma's name for it).
    pub modifier: String,
}

impl GlobalSnippet {
    /// `shortcut`'s keys as caps: `["Mod", "Shift", "R"]`.
    pub fn caps(&self, shortcut: &GlobalShortcut) -> Vec<String> {
        vec![
            self.modifier.clone(),
            "Shift".to_owned(),
            shortcut.key.to_owned(),
        ]
    }
}

/// The global shortcuts for `desktop`: niri's binds (SPEC §10.2), sway's
/// and i3's `bindsym`, Hyprland's `bind`, river's `riverctl map`; GNOME's
/// and KDE Plasma's custom shortcuts, each a name, a command and keys
/// typed in their settings; and for every other desktop (Xfce, Cinnamon,
/// COSMIC, an X11 window manager, one nothing names) the commands with
/// their keys, to bind wherever it binds keys.
pub fn global_snippet(desktop: &Desktop) -> GlobalSnippet {
    let lines = |line: fn(&GlobalShortcut) -> String| {
        GLOBAL_SHORTCUTS
            .iter()
            .map(line)
            .collect::<Vec<_>>()
            .join("\n")
    };
    match desktop {
        Desktop::Niri => config_file(
            desktop,
            ("niri", "kdl", "~/.config/niri/config.kdl", "Mod"),
            format!(
                "binds {{\n{}\n}}",
                lines(|s| {
                    let argv: Vec<String> = s.argv.iter().map(|a| format!("\"{a}\"")).collect();
                    format!("    Mod+Shift+{} {{ spawn {}; }}", s.key, argv.join(" "))
                })
            ),
        ),
        Desktop::Sway => config_file(
            desktop,
            ("sway", "swayconfig", "~/.config/sway/config", "Mod"),
            lines(|s| {
                format!(
                    "bindsym $mod+Shift+{} exec {}",
                    s.key.to_lowercase(),
                    s.argv.join(" ")
                )
            }),
        ),
        // i3 shows a busy cursor for a minute after an `exec` without
        // --no-startup-id, and its default config has $mod+Shift+r.
        Desktop::I3 => config_file(
            desktop,
            ("i3", "i3config", "~/.config/i3/config", "Mod"),
            format!(
                "# i3's default config binds $mod+Shift+r to restart: remove that line.\n{}",
                lines(|s| format!(
                    "bindsym $mod+Shift+{} exec --no-startup-id {}",
                    s.key.to_lowercase(),
                    s.argv.join(" ")
                ))
            ),
        ),
        Desktop::Hyprland => config_file(
            desktop,
            (
                "Hyprland",
                "hyprlang",
                "~/.config/hypr/hyprland.conf",
                "Super",
            ),
            lines(|s| format!("bind = SUPER SHIFT, {}, exec, {}", s.key, s.argv.join(" "))),
        ),
        Desktop::River => config_file(
            desktop,
            ("river", "sh", "~/.config/river/init", "Super"),
            lines(|s| {
                format!(
                    "riverctl map normal Super+Shift {} spawn '{}'",
                    s.key,
                    s.argv.join(" ")
                )
            }),
        ),
        // "GNOME Settings", not "Settings": the cheat sheet's footer says
        // Settings › Keyboard for Qrec's own page.
        Desktop::Gnome => settings(
            Some(desktop),
            "Global shortcuts for GNOME:",
            "In GNOME Settings \u{203a} Keyboard \u{203a} View and Customize Shortcuts \u{203a} \
             Custom Shortcuts,\nfor each line choose Add Shortcut (or +), type its name and \
             command, then set its shortcut.",
            "Add these as custom shortcuts in GNOME Settings \u{203a} Keyboard to record while \
             another app has focus.",
            "Super",
        ),
        Desktop::Kde => settings(
            Some(desktop),
            "Global shortcuts for KDE Plasma:",
            "In System Settings \u{203a} Keyboard \u{203a} Shortcuts, for each line choose\n\
             Add New \u{203a} Command or Script\u{2026}, type its command, then set its \
             shortcut.",
            "Add these as commands in System Settings \u{203a} Keyboard \u{203a} Shortcuts to \
             record while another app has focus.",
            "Meta",
        ),
        Desktop::Wayfire
        | Desktop::Labwc
        | Desktop::Cosmic
        | Desktop::Xfce
        | Desktop::Mate
        | Desktop::Cinnamon
        | Desktop::Other(_)
        | Desktop::Unknown => settings(
            None,
            "Global shortcuts:",
            "Bind each command to its shortcut in your desktop's keyboard settings.",
            "Bind these commands in your desktop's keyboard settings to record while another \
             app has focus.",
            "Super",
        ),
    }
}

/// A snippet for a config file: `(name, language, file, modifier)` and
/// its lines.
fn config_file(
    desktop: &Desktop,
    (name, language, file, modifier): (&str, &str, &str, &str),
    text: String,
) -> GlobalSnippet {
    GlobalSnippet {
        desktop: Some(desktop.clone()),
        language: language.to_owned(),
        file: Some(file.to_owned()),
        heading: format!("Global shortcuts for {name} (paste into {file}):"),
        text,
        note: format!("Bind these in {name} to record while another app has focus."),
        button: COPY_SNIPPET.to_owned(),
        modifier: modifier.to_owned(),
    }
}

/// Shortcuts added one by one: the steps, then each shortcut's name,
/// command and keys.
fn settings(
    desktop: Option<&Desktop>,
    heading: &str,
    steps: &str,
    note: &str,
    modifier: &str,
) -> GlobalSnippet {
    GlobalSnippet {
        desktop: desktop.cloned(),
        language: "text".to_owned(),
        file: None,
        heading: heading.to_owned(),
        text: format!("{steps}\n\n{}", command_table(modifier)),
        note: note.to_owned(),
        button: COPY_SNIPPET.to_owned(),
        modifier: modifier.to_owned(),
    }
}

/// Each shortcut's name, command and keys (`Super+Shift+R`), aligned under
/// a header.
fn command_table(modifier: &str) -> String {
    let rows: Vec<[String; 3]> =
        std::iter::once(["Name", "Command", "Shortcut"].map(str::to_owned))
            .chain(GLOBAL_SHORTCUTS.iter().map(|s| {
                [
                    s.label.to_owned(),
                    s.argv.join(" "),
                    format!("{modifier}+Shift+{}", s.key),
                ]
            }))
            .collect();
    let width = |column: usize| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (name, command) = (width(0), width(1));
    rows.iter()
        .map(|[label, argv, keys]| format!("{label:<name$}  {argv:<command$}  {keys}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use proptest::prelude::*;

    use super::*;

    /// Every desktop `platform::Desktop` names, and one it doesn't.
    fn desktops() -> Vec<Desktop> {
        vec![
            Desktop::Niri,
            Desktop::Sway,
            Desktop::I3,
            Desktop::Hyprland,
            Desktop::River,
            Desktop::Gnome,
            Desktop::Kde,
            Desktop::Wayfire,
            Desktop::Labwc,
            Desktop::Cosmic,
            Desktop::Xfce,
            Desktop::Mate,
            Desktop::Cinnamon,
            Desktop::Other("budgie".into()),
            Desktop::Unknown,
        ]
    }

    /// Each desktop's snippet as the doctor prints it, with its other
    /// fields; the generic list once, after the desktops that get it.
    #[test]
    fn snapshot_every_desktop() {
        let show = |out: &mut String, title: &str, snippet: &GlobalSnippet| {
            let _ = writeln!(
                out,
                "=== {title} (language {}, file {:?}, modifier {})\nnote: {}\nbutton: {}\n{}\n\n{}\n",
                snippet.language,
                snippet.file,
                snippet.modifier,
                snippet.note,
                snippet.button,
                snippet.heading,
                snippet.text
            );
        };
        let mut out = String::new();
        let mut generic = Vec::new();
        for desktop in desktops() {
            let snippet = global_snippet(&desktop);
            if snippet.desktop.is_none() {
                assert_eq!(snippet, global_snippet(&Desktop::Unknown), "{desktop:?}");
                generic.push(desktop.as_str().to_owned());
            } else {
                show(&mut out, desktop.as_str(), &snippet);
            }
        }
        let title = format!("the generic list: {}", generic.join(", "));
        show(&mut out, &title, &global_snippet(&Desktop::Unknown));
        insta::assert_snapshot!("global_snippets", out);
    }

    /// niri's is SPEC §10.2's, built from the same commands as the others.
    #[test]
    fn niri_gets_spec_snippet() {
        let niri = global_snippet(&Desktop::Niri);
        assert_eq!(niri.text, NIRI_SNIPPET);
        assert_eq!(
            niri.heading,
            "Global shortcuts for niri (paste into ~/.config/niri/config.kdl):"
        );
        assert_eq!(
            niri.note,
            "Bind these in niri to record while another app has focus."
        );
        assert_eq!(niri.caps(&GLOBAL_SHORTCUTS[0]), ["Mod", "Shift", "R"]);
        let json = serde_json::to_value(&niri).unwrap();
        assert_eq!(json["desktop"], "niri");
        assert_eq!(json["file"], "~/.config/niri/config.kdl");
        let generic = serde_json::to_value(global_snippet(&Desktop::Xfce)).unwrap();
        assert_eq!(generic["desktop"], serde_json::Value::Null);
        assert_eq!(generic["file"], serde_json::Value::Null);
    }

    /// Only the desktops with a config file Qrec knows get a file; the
    /// settings' ones and the generic list name every command whole.
    #[test]
    fn files_and_settings() {
        for desktop in desktops() {
            let snippet = global_snippet(&desktop);
            let configured = matches!(
                desktop,
                Desktop::Niri | Desktop::Sway | Desktop::I3 | Desktop::Hyprland | Desktop::River
            );
            assert_eq!(snippet.file.is_some(), configured, "{desktop:?}");
            assert_eq!(snippet.language == "text", !configured, "{desktop:?}");
            if !configured {
                for s in GLOBAL_SHORTCUTS {
                    let row = snippet
                        .text
                        .lines()
                        .find(|l| l.starts_with(s.label))
                        .unwrap_or_else(|| panic!("{desktop:?}: no {}", s.label));
                    assert!(row.contains(&s.argv.join(" ")), "{row}");
                    assert!(
                        row.ends_with(&format!("{}+Shift+{}", snippet.modifier, s.key)),
                        "{row}"
                    );
                }
            }
        }
        assert_eq!(global_snippet(&Desktop::Kde).modifier, "Meta");
        assert_eq!(global_snippet(&Desktop::Gnome).modifier, "Super");
    }

    /// A desktop's settings app goes by its name (GNOME Settings, KDE's
    /// System Settings): a bare "Settings ›" is Qrec's own, which the cheat
    /// sheet's footer names and which has no global shortcuts.
    #[test]
    fn the_desktops_settings_go_by_their_names() {
        for desktop in desktops() {
            let snippet = global_snippet(&desktop);
            for text in [&snippet.heading, &snippet.text, &snippet.note] {
                for (at, _) in text.match_indices("Settings \u{203a}") {
                    assert!(
                        text[..at].ends_with("GNOME ") || text[..at].ends_with("System "),
                        "{desktop:?}: {text}"
                    );
                }
            }
        }
    }

    fn any_desktop() -> impl Strategy<Value = Desktop> {
        prop_oneof![
            proptest::sample::select(desktops()),
            "[a-z0-9:_-]{0,12}".prop_map(|name| Desktop::from_name(&name)),
        ]
    }

    proptest! {
        /// Whatever the desktop: every shortcut's command is in the text
        /// (niri's quoted word by word), a key cap each, the doctor's line
        /// ends in a colon, the button is the same, and the JSON reads back.
        #[test]
        fn every_desktop_gets_every_command(desktop in any_desktop()) {
            let snippet = global_snippet(&desktop);
            for s in GLOBAL_SHORTCUTS {
                let quoted: Vec<String> = s.argv.iter().map(|a| format!("\"{a}\"")).collect();
                prop_assert!(
                    snippet.text.contains(&s.argv.join(" "))
                        || snippet.text.contains(&quoted.join(" ")),
                    "{:?}: {}", desktop, snippet.text
                );
                prop_assert_eq!(snippet.caps(&s).len(), 3);
            }
            prop_assert!(snippet.heading.starts_with("Global shortcuts"));
            prop_assert!(snippet.heading.ends_with(':'));
            prop_assert!(snippet.note.ends_with('.'));
            prop_assert_eq!(snippet.button.as_str(), COPY_SNIPPET);
            prop_assert!(snippet.desktop.is_none() || snippet.desktop.as_ref() == Some(&desktop));
            let json = serde_json::to_string(&snippet).unwrap();
            prop_assert_eq!(serde_json::from_str::<GlobalSnippet>(&json).unwrap(), snippet);
        }
    }
}
