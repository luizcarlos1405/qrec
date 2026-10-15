//! `keys.toml` (A§9.7): reading it for the GUI's startup (docs/GUI.md
//! G§3.3 step 1, then `keymap::resolve`) and for `Query::Keys` (the
//! Session's facts), within the bounds of [`config_file::read_text`]. The
//! CLI's `qrec keys` reads it with that same reader, through
//! `Backend::settings_text`. Editing bindings comes with the GUI's Settings
//! pages. Owner: meta.

use std::path::Path;

use crate::config_file;

/// The text of `keys.toml` at `path`: empty when there is none (the
/// default bindings; `keymap::resolve` reports what's wrong with a file
/// that is there). An error says why it can't be read: the caller uses the
/// default bindings and says so.
pub fn read(path: &Path) -> Result<String, String> {
    config_file::read_text(path).map(Option::unwrap_or_default)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn keys_toml_is_read_whole_or_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("keys.toml");
        assert_eq!(read(&path), Ok(String::new()), "no file: the defaults");
        let text = "[bindings]\n\"record.toggle\" = \"F9\"\n";
        fs::write(&path, text).unwrap();
        assert_eq!(read(&path).unwrap(), text);
        let keymap = qrec_core::keymap::resolve(&read(&path).unwrap());
        assert!(qrec_core::keymap::problems(&keymap).is_empty());

        fs::write(&path, vec![b'#'; config_file::MAX_BYTES as usize + 1]).unwrap();
        assert!(read(&path).unwrap_err().contains("at most"));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(read(&path).unwrap_err().starts_with("can't read "));
    }
}
