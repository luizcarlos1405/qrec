//! Compiles `resources/` into one GResource (G§3.14): the manifest is
//! written from every file under it (no hand-kept list, so work packages
//! add files without touching a shared one), under the prefix
//! `/com/rsensato/qrec/`, and `glib-compile-resources` (the dev shell and
//! the package have it) builds `$OUT_DIR/qrec.gresource`, which the app
//! includes and registers at startup.
//!
//! The app icon the package installs (`data/icons/hicolor/<size>/apps/
//! com.rsensato.qrec.{png,svg}`, which `tests/art.rs` writes) goes in too,
//! as `icons/<size>/apps/…` under the icon theme's resource path
//! (`app::ICONS`): the window's icon (X11's `_NET_WM_ICON`) resolves when
//! Qrec isn't installed, from the same files (GUI-08).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The app id, the app icon's name (`app::APP_ID`).
const APP_ID: &str = "com.rsensato.qrec";

fn main() {
    let crate_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let root = crate_dir.join("resources");
    let hicolor = crate_dir.join("../../data/icons/hicolor");
    let out = PathBuf::from(env("OUT_DIR"));
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-changed={}", hicolor.display());
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();
    let icons = app_icons(&hicolor);
    assert!(
        !icons.is_empty(),
        "no app icon in {} (data/icons/hicolor/<size>/apps/{APP_ID}.png)",
        hicolor.display()
    );
    for file in &files {
        println!("cargo:rerun-if-changed={}", root.join(file).display());
    }
    for icon in &icons {
        println!("cargo:rerun-if-changed={}", hicolor.join(icon).display());
    }
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"/com/rsensato/qrec\">\n",
    );
    for file in &files {
        xml.push_str(&format!("    <file>{}</file>\n", escape(file)));
    }
    // Found in the second source dir, under the icon theme's path.
    for icon in &icons {
        xml.push_str(&format!(
            "    <file alias=\"icons/{0}\">{0}</file>\n",
            escape(icon)
        ));
    }
    xml.push_str("  </gresource>\n</gresources>\n");
    let manifest = out.join("qrec.gresource.xml");
    fs::write(&manifest, xml).expect("write the GResource manifest");
    let target = out.join("qrec.gresource");
    let status = Command::new("glib-compile-resources")
        .arg("--sourcedir")
        .arg(&root)
        .arg("--sourcedir")
        .arg(&hicolor)
        .arg("--target")
        .arg(&target)
        .arg(&manifest)
        .status()
        .unwrap_or_else(|e| {
            panic!("can't run glib-compile-resources (it comes with glib's dev tools): {e}")
        });
    assert!(status.success(), "glib-compile-resources failed: {status}");
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("{key} isn't set"))
}

/// Every file under `dir`, as `/`-separated paths relative to `root`.
fn collect(root: &Path, dir: &Path, files: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            let parts: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            files.push(parts.join("/"));
        }
    }
}

/// The app icon's files in the hicolor tree `hicolor`, as
/// `<size>/apps/com.rsensato.qrec.<ext>` (sorted).
fn app_icons(hicolor: &Path) -> Vec<String> {
    let mut all = Vec::new();
    collect(hicolor, hicolor, &mut all);
    let mut icons: Vec<String> = all
        .into_iter()
        .filter(|file| {
            let mut parts = file.split('/');
            let (Some(_size), Some("apps"), Some(name), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return false;
            };
            name.strip_prefix(APP_ID)
                .is_some_and(|ext| ext == ".png" || ext == ".svg")
        })
        .collect();
    icons.sort();
    icons
}

fn escape(path: &str) -> String {
    path.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
