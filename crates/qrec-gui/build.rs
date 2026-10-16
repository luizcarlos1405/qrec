//! Compiles `resources/` into one GResource (G§3.14): the manifest is
//! written from every file under it (no hand-kept list, so work packages
//! add files without touching a shared one), under the prefix
//! `/com/rsensato/qrec/`, and `glib-compile-resources` (the dev shell and
//! the package have it) builds `$OUT_DIR/qrec.gresource`, which the app
//! includes and registers at startup.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let root = PathBuf::from(env("CARGO_MANIFEST_DIR")).join("resources");
    let out = PathBuf::from(env("OUT_DIR"));
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();
    for file in &files {
        println!("cargo:rerun-if-changed={}", root.join(file).display());
    }
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"/com/rsensato/qrec\">\n",
    );
    for file in &files {
        xml.push_str(&format!("    <file>{}</file>\n", escape(file)));
    }
    xml.push_str("  </gresource>\n</gresources>\n");
    let manifest = out.join("qrec.gresource.xml");
    fs::write(&manifest, xml).expect("write the GResource manifest");
    let target = out.join("qrec.gresource");
    let status = Command::new("glib-compile-resources")
        .arg("--sourcedir")
        .arg(&root)
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

fn escape(path: &str) -> String {
    path.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
