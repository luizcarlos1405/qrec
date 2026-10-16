//! How much disk each take uses, for the bin (docs/GUI.md G§3.11: "5
//! takes · 436.4 MB"): the regular files in its folder, found the way a
//! purge finds them (`take_contents`: `O_NOFOLLOW`, regular files only), so
//! the size the bin shows is what a purge would free. Blocking: the GUI
//! calls it on a worker (`qrec-gui-io`). Owner: media.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use qrec_core::TakeId;

use super::purge::take_contents;

/// The bytes on disk of each of `takes` in the project at `project_dir`:
/// the sum of the sizes of the regular files in `takes/<take>/`. A take
/// whose size can't be known is left out: its folder is gone (purged, or
/// never written), or couldn't be looked into (a symlink, which a purge
/// never follows, or unreadable). An empty folder is 0.
pub fn take_sizes(project_dir: &Path, takes: &[TakeId]) -> BTreeMap<TakeId, u64> {
    let mut sizes = BTreeMap::new();
    for take in takes {
        let contents = take_contents(project_dir, take);
        if contents.files.is_empty() && !is_real_dir(&project_dir.join("takes").join(take.as_str()))
        {
            continue;
        }
        if contents.files.is_empty() && !contents.left.is_empty() {
            // Nothing could be listed (a symlinked or unreadable folder).
            continue;
        }
        let bytes = contents
            .files
            .iter()
            .fold(0u64, |sum, (_, bytes, _)| sum.saturating_add(*bytes));
        sizes.insert(take.clone(), bytes);
    }
    sizes
}

/// A directory itself, not a symlink to one.
fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn take(n: u32) -> TakeId {
        TakeId::numbered(n)
    }

    /// The bin's sizes are `take_contents`' sums: what a purge would delete.
    #[test]
    fn sizes_are_the_sums_of_what_a_purge_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let takes = root.join("takes");
        fs::create_dir_all(takes.join("take-0001")).unwrap();
        fs::write(takes.join("take-0001/L1-screen-DP-4.mkv"), vec![0u8; 1500]).unwrap();
        fs::write(takes.join("take-0001/A1-mic.mka"), vec![0u8; 250]).unwrap();
        fs::write(takes.join("take-0001/recorder.log"), "log").unwrap();
        // A folder and a symlink inside are left out of the sum, as by the purge.
        fs::create_dir_all(takes.join("take-0001/sub")).unwrap();
        fs::write(takes.join("take-0001/sub/nested.bin"), vec![0u8; 999]).unwrap();
        let outside = root.join("outside.bin");
        fs::write(&outside, vec![0u8; 777]).unwrap();
        symlink(&outside, takes.join("take-0001/link.bin")).unwrap();
        // An empty take folder.
        fs::create_dir_all(takes.join("take-0002")).unwrap();
        // A symlinked take folder: never followed.
        fs::create_dir_all(root.join("elsewhere")).unwrap();
        fs::write(root.join("elsewhere/A1-mic.mka"), vec![0u8; 64]).unwrap();
        symlink(root.join("elsewhere"), takes.join("take-0004")).unwrap();

        let all = [take(1), take(2), take(3), take(4)];
        let sizes = take_sizes(root, &all);
        for t in &all {
            let contents = take_contents(root, t);
            let sum: u64 = contents.files.iter().map(|(_, bytes, _)| bytes).sum();
            if let Some(size) = sizes.get(t) {
                assert_eq!(*size, sum, "{t}");
            }
        }
        assert_eq!(sizes.get(&take(1)), Some(&(1500 + 250 + 3)));
        assert_eq!(sizes.get(&take(2)), Some(&0), "an empty folder");
        assert_eq!(sizes.get(&take(3)), None, "no folder: unknown");
        assert_eq!(sizes.get(&take(4)), None, "a symlinked folder: unknown");
        assert_eq!(sizes.len(), 2);
        assert!(take_sizes(root, &[]).is_empty());
        assert!(take_sizes(&root.join("nothing-here"), &[take(1)]).is_empty());
    }
}
