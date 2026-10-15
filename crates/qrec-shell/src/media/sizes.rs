//! How much disk each take uses, for the bin (docs/GUI.md G§3.11: "5
//! takes · 436.4 MB"): the regular files in its folder, found the way a
//! purge finds them (`take_contents`). Blocking: the GUI calls it on a
//! worker. Owner: media.
//!
//! Not built yet: no take has a known size.

use std::collections::BTreeMap;
use std::path::Path;

use qrec_core::TakeId;

/// The bytes on disk of each of `takes` in the project at `project_dir`;
/// a take whose size can't be known is left out.
pub fn take_sizes(project_dir: &Path, takes: &[TakeId]) -> BTreeMap<TakeId, u64> {
    let _ = (project_dir, takes);
    BTreeMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_size_is_known_yet() {
        assert!(take_sizes(Path::new("/p"), &[TakeId::numbered(1)]).is_empty());
    }
}
