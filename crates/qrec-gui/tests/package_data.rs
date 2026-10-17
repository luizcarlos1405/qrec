//! The desktop entry and the icons the package installs (docs/GUI.md G§6.4,
//! `data/`). The entry and the icon are named after the app id (SPEC's App
//! ID row): the compositor finds the entry from the window's Wayland
//! `app_id`. `data/install.sh` renders the hicolor icons from the theme's
//! placeholder art in Swamp Night's colors (until the pen export, G§0.6).
//! The package's build also runs `desktop-file-validate` on the entry
//! (`flake.nix`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use qrec_gui::app::APP_ID;
use qrec_shell::testkit::run_tool;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The entry's `key=value` lines, in order (after its `[Desktop Entry]`
/// header).
fn entry() -> Vec<(String, String)> {
    let path = repo().join(format!("data/{APP_ID}.desktop"));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("[Desktop Entry]"));
    lines
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (key, value) = l
                .split_once('=')
                .unwrap_or_else(|| panic!("not key=value: {l}"));
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

#[test]
fn the_desktop_entry_names_the_app() {
    let entry = entry();
    let get = |key: &str| {
        let found: Vec<&str> = entry
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect();
        assert!(found.len() <= 1, "{key} twice");
        found.first().copied()
    };
    assert_eq!(get("Type"), Some("Application"));
    assert_eq!(get("Name"), Some("Qrec"));
    assert_eq!(get("Exec"), Some("qrec"), "G§9 R18: no folder argument yet");
    assert_eq!(
        get("Icon"),
        Some(APP_ID),
        "the icon is named after the app id"
    );
    assert_eq!(
        get("StartupWMClass"),
        Some(APP_ID),
        "the GTK application id"
    );
    assert_eq!(get("Terminal"), Some("false"));
    assert_eq!(get("Categories"), Some("AudioVideo;Video;Recorder;"));
    for (key, value) in &entry {
        assert!(!value.trim().is_empty(), "{key} is empty");
    }
}

/// The color of the token `--qrec-<name>` in Swamp Night's tokens CSS.
fn token(name: &str) -> String {
    let css =
        fs::read_to_string(repo().join("crates/qrec-gui/resources/theme/tokens/swamp-night.css"))
            .expect("the tokens");
    let prefix = format!("--qrec-{name}:");
    css.lines()
        .find_map(|l| l.trim().strip_prefix(&prefix))
        .map(|v| v.trim().trim_end_matches(';').to_owned())
        .unwrap_or_else(|| panic!("no token {name}"))
}

/// A PNG's width and height (its IHDR).
fn png_size(file: &Path) -> (u32, u32) {
    let bytes = fs::read(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    assert!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() > 24 && &bytes[12..16] == b"IHDR",
        "{} isn't a PNG",
        file.display()
    );
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    (be(16), be(20))
}

/// The RGBA pixel at (`x`, `y`) of a PNG (ffmpeg).
fn pixel(png: &Path, x: u32, y: u32) -> [u8; 4] {
    let out = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(png)
            .args([
                "-vf",
                &format!("crop=1:1:{x}:{y}"),
                "-pix_fmt",
                "rgba",
                "-f",
                "rawvideo",
                "-",
            ]),
        Duration::from_secs(30),
    );
    assert_eq!(
        out.stdout.len(),
        4,
        "a pixel of {}: {}",
        png.display(),
        out.stderr
    );
    [out.stdout[0], out.stdout[1], out.stdout[2], out.stdout[3]]
}

fn hex([r, g, b, _]: [u8; 4]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

#[test]
fn install_sh_renders_the_icons_from_the_placeholder_art() {
    let prefix = tempfile::tempdir().expect("a prefix");
    let out = run_tool(
        Command::new("sh")
            .arg(repo().join("data/install.sh"))
            .arg(prefix.path()),
        Duration::from_secs(60),
    );
    assert!(out.status.success(), "install.sh: {}", out.stderr);
    let share = prefix.path().join("share");

    let entry = share.join(format!("applications/{APP_ID}.desktop"));
    assert_eq!(
        fs::read(&entry).expect("the installed entry"),
        fs::read(repo().join(format!("data/{APP_ID}.desktop"))).expect("the entry")
    );

    let icons = share.join("icons/hicolor");
    let svg = fs::read_to_string(icons.join(format!("scalable/apps/{APP_ID}.svg")))
        .expect("the scalable icon");
    assert!(!svg.contains("{{"), "every token substituted:\n{svg}");
    for name in ["accent", "accent-2", "wood"] {
        assert!(svg.contains(&token(name)), "{name}'s color in:\n{svg}");
    }
    for size in [32, 64, 128] {
        let png = icons.join(format!("{size}x{size}/apps/{APP_ID}.png"));
        assert_eq!(png_size(&png), (size, size), "{}", png.display());
    }
    // The 128 px icon: the bulb's middle is `accent-2` (between its two
    // `wood` skin lines), its corner transparent.
    let png = icons.join(format!("128x128/apps/{APP_ID}.png"));
    assert_eq!(hex(pixel(&png, 64, 75)), token("accent-2"));
    assert_eq!(pixel(&png, 2, 2)[3], 0, "a transparent corner");

    let symbolic = icons.join(format!("symbolic/apps/{APP_ID}-symbolic.svg"));
    assert_eq!(
        fs::read(symbolic).expect("the symbolic icon"),
        fs::read(
            repo()
                .join("crates/qrec-gui/resources/icons/scalable/actions/qrec-record-symbolic.svg")
        )
        .expect("the REC glyph"),
        "the app's symbolic icon is the REC glyph"
    );
}
