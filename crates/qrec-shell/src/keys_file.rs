//! `keys.toml` (A§9.7): reading it for the GUI's startup (docs/GUI.md
//! G§3.3 step 1, then `keymap::resolve`), for its live reload (WP10, the
//! settings watcher) and for `Query::Keys` (the Session's facts), within
//! the bounds of [`config_file::read_text`]. The CLI's `qrec keys` reads it
//! with that same reader, through `Backend::settings_text`. Settings ›
//! Keyboard (S22, WP9) edits it: [`set_binding`] and [`reset_binding`]
//! change one binding's line (`keymap::set_binding`, every other line as
//! it was) and write the file atomically ([`config_file::write_text`]).
//! Owner: meta.

use std::path::Path;

use qrec_core::CoreError;
use qrec_core::keymap::{self, KeyChord, Keymap};

use crate::config_file;

/// The text of `keys.toml` at `path`: empty when there is none (the
/// default bindings; `keymap::resolve` reports what's wrong with a file
/// that is there). An error says why it can't be read: the caller uses the
/// default bindings and says so.
pub fn read(path: &Path) -> Result<String, String> {
    config_file::read_text(path).map(Option::unwrap_or_default)
}

/// The keymap of `keys.toml` at `path` and what is wrong with it, one line
/// each: why it can't be read ("…; using the default keys": the defaults
/// then), then `keymap::problems` (the conflicts, unknown ids and bad keys,
/// as the core words them). What the window loads at startup and again
/// when the file changes (G§3.3 step 1, WP10).
pub fn load(path: &Path) -> (Keymap, Vec<String>) {
    let mut problems = Vec::new();
    let text = read(path).unwrap_or_else(|why| {
        problems.push(format!("{why}; using the default keys"));
        String::new()
    });
    let keymap = keymap::resolve(&text);
    problems.extend(keymap::problems(&keymap));
    (keymap, problems)
}

/// Sets binding `id` to `keys` in `keys.toml` at `path` (none: unbound;
/// the file and its folder made when missing): S22's capture ("Press a
/// key · Esc cancels · Backspace clears"). Read within bounds, edited with
/// `keymap::set_binding` (comments and order stay), written with
/// [`config_file::write_text`]. The keymap the file gives now and its
/// problems, as [`load`] words them: the window uses it at once (its
/// watcher reads the file again too). A file that can't be read or isn't
/// TOML is refused, never written over.
pub fn set_binding(
    path: &Path,
    id: &str,
    keys: &[KeyChord],
) -> Result<(Keymap, Vec<String>), CoreError> {
    edit(path, |text| keymap::set_binding(text, id, keys))
}

/// Binding `id`'s default keys again (S22's reset): its lines leave
/// `keys.toml` at `path`, every other line as it was. Like
/// [`set_binding`] otherwise; nothing is written when the file doesn't
/// set `id`.
pub fn reset_binding(path: &Path, id: &str) -> Result<(Keymap, Vec<String>), CoreError> {
    edit(path, |text| keymap::reset_binding(text, id))
}

fn edit(
    path: &Path,
    change: impl FnOnce(&str) -> Result<String, CoreError>,
) -> Result<(Keymap, Vec<String>), CoreError> {
    let text = read(path).map_err(CoreError::io)?;
    let edited = change(&text)?;
    if edited != text {
        config_file::write_text(path, &edited).map_err(CoreError::io)?;
    }
    let keymap = keymap::resolve(&edited);
    let problems = keymap::problems(&keymap);
    Ok((keymap, problems))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A `keys.toml` a person wrote (comments, dotted keys out of order,
    /// a quoted key, a trailing comment).
    const COMMENTED: &str = "\
# My Qrec keys
[bindings]
clip.split = \"s\"
# closer to my left hand
frame.save = \"f\"   # was p
\"clip.remove\" = [\"Delete\", \"x\"]

# the end
";

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    #[test]
    fn edits_change_one_line_of_a_commented_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("qrec").join("keys.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, COMMENTED).unwrap();

        let (keymap, problems) = set_binding(&path, "frame.save", &[chord("g")]).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(keymap.binding("frame.save").unwrap().keys, [chord("g")]);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            COMMENTED.replace("frame.save = \"f\"", "frame.save = \"g\""),
            "only the value changed"
        );
        // A new binding goes after the last one; Backspace's unbound too.
        set_binding(&path, "export", &[chord("F5")]).unwrap();
        set_binding(&path, "record.over", &[]).unwrap();
        let (keymap, _) = reset_binding(&path, "clip.split").unwrap();
        assert_eq!(keymap.binding("clip.split").unwrap().keys, [chord("s")]);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\
# My Qrec keys
[bindings]
# closer to my left hand
frame.save = \"g\"   # was p
\"clip.remove\" = [\"Delete\", \"x\"]
export = \"F5\"
record.over = []

# the end
"
        );
        // What the file says is what the window and the CLI read back.
        let (loaded, problems) = load(&path);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(loaded, keymap);
        assert!(loaded.binding("record.over").unwrap().keys.is_empty());
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["keys.toml"], "no temp file left");
    }

    /// Q5 through the file: rebinding onto a key another binding has makes
    /// a conflict, which the keymap reports and runs neither of.
    #[test]
    fn a_conflicting_edit_is_reported_and_runs_neither() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("keys.toml");
        let (keymap, problems) = set_binding(&path, "autocut.run", &[chord("x")]).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[bindings]\nautocut.run = \"x\"\n"
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        let notes = keymap::conflict_notes(&keymap);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].key, "X");
        assert_eq!(notes[0].ids, ["autocut.run", "clip.remove"]);
        assert!(keymap::lookup(&keymap, &chord("x"), keymap::KeyContext::Timeline).is_none());
        // Resetting it ends the conflict.
        let (keymap, problems) = reset_binding(&path, "autocut.run").unwrap();
        assert!(problems.is_empty());
        assert!(keymap.conflicts.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), "[bindings]\n");
    }

    #[test]
    fn a_broken_file_is_never_written_over() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("keys.toml");
        fs::write(&path, "[bindings\n").unwrap();
        let err = set_binding(&path, "frame.save", &[chord("f")]).unwrap_err();
        assert_eq!(err.code, qrec_core::ErrorCode::Invalid);
        assert_eq!(fs::read_to_string(&path).unwrap(), "[bindings\n");
        assert_eq!(
            reset_binding(&path, "nope").unwrap_err().code,
            qrec_core::ErrorCode::Usage
        );
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let err = set_binding(&path, "frame.save", &[chord("f")]).unwrap_err();
        assert_eq!(err.code, qrec_core::ErrorCode::Io);
        assert!(err.message.starts_with("can't read "), "{}", err.message);
    }

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

    /// `load`: the keymap and its problems, why the file can't be read
    /// first (with the default keys), then the core's.
    #[test]
    fn keys_toml_loads_with_its_problems() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("keys.toml");
        let (keymap, problems) = load(&path);
        assert_eq!(keymap, keymap::resolve(""));
        assert!(problems.is_empty());

        fs::write(&path, "[bindings]\n\"autocut.run\" = \"x\"\n").unwrap();
        let (keymap, problems) = load(&path);
        assert_eq!(problems, keymap::problems(&keymap));
        assert_eq!(problems.len(), 1, "{problems:?}");

        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let (keymap, problems) = load(&path);
        assert_eq!(keymap, keymap::resolve(""));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("can't read "), "{problems:?}");
        assert!(problems[0].ends_with("; using the default keys"));
    }
}
