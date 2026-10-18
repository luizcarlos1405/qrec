//! The desktop entry and the icons the package installs (docs/GUI.md G§6.4,
//! `data/`). The entry and the icon are named after the app id (SPEC's App
//! ID row): the compositor finds the entry from the window's Wayland
//! `app_id`. `data/install.sh` installs the pen's app icon (`ptPs6`): the
//! scalable SVG and the pen's own hicolor PNGs in `data/icons/`, which
//! `tests/art.rs` writes from the export; the symbolic icon is the REC
//! glyph. The package's build also runs `desktop-file-validate` on the
//! entry (`flake.nix`).

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

/// The hicolor sizes the package installs: the pen's renders.
const SIZES: [u32; 7] = [16, 32, 48, 64, 128, 256, 512];

#[test]
fn install_sh_installs_the_pens_app_icon() {
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
    let data = repo().join("data/icons/hicolor");
    let svg = fs::read_to_string(icons.join(format!("scalable/apps/{APP_ID}.svg")))
        .expect("the scalable icon");
    assert_eq!(
        svg,
        fs::read_to_string(data.join(format!("scalable/apps/{APP_ID}.svg"))).expect("the art"),
        "the pen's app icon (tests/art.rs writes it)"
    );
    assert!(
        svg.starts_with("<?xml") && svg.contains("viewBox=\"0 0 128 128\"") && !svg.contains("{{"),
        "a 128 px SVG with its own colors:\n{svg}"
    );
    for size in SIZES {
        let at = format!("{size}x{size}/apps/{APP_ID}.png");
        let png = icons.join(&at);
        assert_eq!(png_size(&png), (size, size), "{}", png.display());
        assert_eq!(
            fs::read(&png).expect("the PNG"),
            fs::read(data.join(&at)).expect("the pen's PNG"),
            "{at}: the pen's render"
        );
    }
    // The 128 px icon: the bulb's middle is the pen's onion gold, its
    // corner transparent.
    let png = icons.join(format!("128x128/apps/{APP_ID}.png"));
    let [r, g, b, a] = pixel(&png, 64, 80);
    assert!(
        a == 255 && r > 200 && g > 150 && b < 140,
        "onion gold at the bulb: {r},{g},{b},{a}"
    );
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
    let license = share.join("licenses/qrec/LICENSE-lucide.txt");
    let text = fs::read_to_string(&license).expect("the Lucide license");
    assert!(
        text.contains("ISC License") && text.contains("Cole Bemis"),
        "{}",
        license.display()
    );
}
