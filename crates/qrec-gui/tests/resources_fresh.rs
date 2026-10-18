//! The window's compiled resources are its sources (G§3.14). `build.rs`
//! compiles every file under `resources/` into `$OUT_DIR/qrec.gresource`
//! and reruns only when one of them has a newer mtime, so a file put back
//! with its old mtime (a revert check that restored its copy with
//! `cp -p`) leaves the window on the reverted file while the sources have
//! the fix. That happened once: two CSS files of WP10's revert checks
//! stayed in the integration worktree's bundle for hours, and every nested
//! run there shot the reverted `panel.css` and `chrome.css` (S12's summary
//! heading a pixel low; found at the play, perf and wp9 lanes'
//! integration). This holds the bundle the window and the nested suites
//! run against: every source file in it byte for byte, and nothing else.

use std::fs;
use std::path::Path;

use gtk::{gio, glib};

/// The bundle's prefix (`build.rs`, `app::register_resources`).
const PREFIX: &str = "/com/rsensato/qrec/";

/// Every file under `dir`, as `/`-separated paths relative to `root`
/// (as `build.rs` lists them).
fn sources(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(root, &path, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            let parts: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push(parts.join("/"));
        }
    }
}

/// Every file in the bundle under `dir` (a path ending in `/`), relative
/// to [`PREFIX`].
fn bundled(resource: &gio::Resource, dir: &str, out: &mut Vec<String>) {
    let children = resource
        .enumerate_children(dir, gio::ResourceLookupFlags::NONE)
        .unwrap_or_else(|e| panic!("{dir}: {e}"));
    for child in children {
        let path = format!("{dir}{child}");
        if child.ends_with('/') {
            bundled(resource, &path, out);
        } else {
            out.push(path[PREFIX.len()..].to_owned());
        }
    }
}

#[test]
fn the_compiled_resources_are_the_sources() {
    let bytes =
        glib::Bytes::from_static(include_bytes!(concat!(env!("OUT_DIR"), "/qrec.gresource")));
    let resource = gio::Resource::from_data(&bytes).expect("the bundle loads");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
    let mut files = Vec::new();
    sources(&root, &root, &mut files);
    files.sort();
    assert!(files.len() > 20, "the resources: {files:?}");
    let mut stale = Vec::new();
    for file in &files {
        let path = format!("{PREFIX}{file}");
        match resource.lookup_data(&path, gio::ResourceLookupFlags::NONE) {
            Ok(data) => {
                let source = fs::read(root.join(file)).expect("read a resource");
                if data.as_ref() != source.as_slice() {
                    stale.push(format!(
                        "{file}: {} bytes in the bundle, {} in the source",
                        data.len(),
                        source.len()
                    ));
                }
            }
            Err(e) => stale.push(format!("{file}: not in the bundle ({e})")),
        }
    }
    let mut in_bundle = Vec::new();
    bundled(&resource, PREFIX, &mut in_bundle);
    for file in in_bundle {
        if !files.contains(&file) {
            stale.push(format!("{file}: in the bundle, no longer a source"));
        }
    }
    assert!(
        stale.is_empty(),
        "the compiled resources aren't the sources (touch crates/qrec-gui/build.rs \
         to compile them again): {stale:#?}"
    );
}
