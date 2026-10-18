//! `config.toml` (A§9.8): reading it for the GUI's startup (docs/GUI.md
//! G§3.3 step 1, before the window maps) and for every Session
//! ([`ConfigFile`]). Reads are bounded: a settings file over [`MAX_BYTES`],
//! or one that isn't a regular file (a FIFO would block), is refused
//! instead of read. Writes ([`set`], [`write_text`]) change the one line
//! a setting is on (`qrec_core::config::set_text`) and replace the file
//! atomically: the Settings pages (WP9) and `qrec config set` (through the
//! CLI's `Backend::write_settings`) write the same way; `keys.toml`'s
//! edits too (`keys_file`). Owner: meta.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use qrec_core::CoreError;
use qrec_core::config::UserConfig;

pub use crate::facts::ConfigFile;

/// The most `config.toml` and `keys.toml` may hold: the GTK thread reads
/// them before the window maps (GUI R1). Every reader's bound
/// (`qrec_core::config::MAX_FILE_BYTES`).
pub const MAX_BYTES: u64 = qrec_core::config::MAX_FILE_BYTES;

/// `config.toml` at `path` as the GUI reads it at startup: its settings,
/// or the defaults when there is none or it can't be used, and the
/// [`ConfigFile`] that read it. The GUI gives that `ConfigFile` to its
/// Session (`SessionEnv::config`), which reports a file that can't be used
/// once (a warning toast, A§4.2) and sees later changes.
pub fn read(path: &Path) -> (UserConfig, ConfigFile) {
    let file = ConfigFile::new(path.to_path_buf());
    let config = file.get();
    (config, file)
}

/// The text of a small settings file: `Ok(None)` when there is none. One
/// that isn't a regular file, is over [`MAX_BYTES`], isn't UTF-8 or can't
/// be read is an error that says so.
pub fn read_text(path: &Path) -> Result<Option<String>, String> {
    let cant = |e: &dyn std::fmt::Display| format!("can't read {}: {e}", path.display());
    let file = match fs::OpenOptions::new()
        .read(true)
        // A FIFO opens at once instead of waiting for a writer.
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(cant(&e)),
    };
    let meta = file.metadata().map_err(|e| cant(&e))?;
    if !meta.is_file() {
        return Err(cant(&"it isn't a regular file"));
    }
    let too_big = |bytes: u64| {
        format!(
            "{} is {bytes} bytes; settings files can be at most {MAX_BYTES} bytes (1 MiB)",
            path.display()
        )
    };
    if meta.len() > MAX_BYTES {
        return Err(too_big(meta.len()));
    }
    let mut text = String::new();
    file.take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| cant(&e))?;
    // It may have grown since the `stat`.
    if text.len() as u64 > MAX_BYTES {
        return Err(too_big(text.len() as u64));
    }
    Ok(Some(text))
}

/// Sets `key` to `value` in `config.toml` at `path` (made, with its
/// folder, when missing): read within bounds, edited with
/// `qrec_core::config::set_text` (every other line as it was) and written
/// with [`write_text`]. The settings the file holds now. What the Settings
/// pages write (S21: theme, interface size, reduce motion, sounds); a
/// window that follows the file (WP10's watcher) applies the change.
pub fn set(path: &Path, key: &str, value: &str) -> Result<UserConfig, CoreError> {
    let text = read_text(path).map_err(CoreError::io)?.unwrap_or_default();
    let edited = qrec_core::config::set_text(&text, key, value)?;
    let config = qrec_core::config::parse(&edited)?;
    if edited != text {
        write_text(path, &edited).map_err(CoreError::io)?;
    }
    Ok(config)
}

/// Writes a settings file (`config.toml`, `keys.toml`) whole: a temp file
/// in its folder (made when missing), synced, renamed over it, the folder
/// synced, so a crash leaves the old file or the new one, never half of
/// one, and the settings watcher sees one change. The file keeps its
/// permissions, and a symlink (a dotfiles manager's) stays one: its
/// target is written. `qrec config set` writes through it
/// (`Backend::write_settings`), and so do [`set`] and `keys_file`'s edits.
pub fn write_text(path: &Path, text: &str) -> Result<(), String> {
    let target = write_target(path);
    let cant =
        |what: &str, e: &dyn std::fmt::Display| format!("can't {what} {}: {e}", target.display());
    let dir = target.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|e| cant("create the folder of", &e))?;
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("settings.toml");
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let permissions = fs::metadata(&target).ok().map(|m| m.permissions());
    let result = (|| -> io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        fs::rename(&tmp, &target)?;
        if let Ok(dir) = fs::File::open(dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    result.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        cant("write", &e)
    })
}

/// The file a write of `path` replaces: `path`, or what it links to (a
/// relative link from its folder; a dangling one is created).
fn write_target(path: &Path) -> PathBuf {
    let mut target = path.to_path_buf();
    // A chain of links ends at a file; a loop stops after a few.
    for _ in 0..8 {
        match fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => match fs::read_link(&target) {
                Ok(link) => {
                    target = match target.parent() {
                        Some(dir) if link.is_relative() => dir.join(link),
                        _ => link,
                    };
                }
                Err(_) => break,
            },
            _ => break,
        }
    }
    target
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    use qrec_core::Fps;

    use super::*;

    /// A `config.toml` a person wrote: comments, blank lines, a dotted
    /// key out of its section's order, a trailing comment.
    const COMMENTED: &str = "\
# My Qrec settings

theme = \"swamp-night\"   # dark, like my soul
ui.scale = 1.25
canvas.fps = \"30\"  # PAL later?

[autocut]
# speech is loud
threshold_db = -35.0
";

    #[test]
    fn set_changes_one_line_of_a_commented_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("qrec").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, COMMENTED).unwrap();

        let config = set(&path, "ui.scale", "1.5").unwrap();
        assert_eq!(config.ui.scale, 1.5);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            COMMENTED.replace("ui.scale = 1.25", "ui.scale = 1.5"),
            "only the value changed"
        );
        set(&path, "ui.sounds", "true").unwrap();
        set(&path, "autocut.padding_ms", "80").unwrap();
        set(&path, "theme", "swamp-day").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\
# My Qrec settings

theme = \"swamp-day\"   # dark, like my soul
ui.scale = 1.5
canvas.fps = \"30\"  # PAL later?
ui.sounds = true

[autocut]
# speech is loud
threshold_db = -35.0
padding_ms = 80
"
        );
        let (read_back, _) = read(&path);
        assert!(read_back.ui.sounds);
        assert_eq!(read_back.autocut.padding_ms, 80);
        // No temp file is left, and a refused value changes nothing.
        let before = fs::read_to_string(&path).unwrap();
        let err = set(&path, "ui.scale", "9").unwrap_err();
        assert_eq!(err.code, qrec_core::ErrorCode::Usage);
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
    }

    /// A `config.toml` saved with CRLF line endings keeps them on the
    /// lines a setting adds (its section's, a new section's).
    #[test]
    fn set_keeps_a_crlf_files_line_endings() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, "# mine\r\n[ui]\r\nscale = 1.25\r\n").unwrap();
        set(&path, "ui.sounds", "true").unwrap();
        set(&path, "autocut.padding_ms", "80").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# mine\r\n[ui]\r\nscale = 1.25\r\nsounds = true\r\n\r\n[autocut]\r\npadding_ms = 80\r\n"
        );
        let (config, _) = read(&path);
        assert!(config.ui.sounds);
    }

    #[test]
    fn set_makes_the_file_and_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("qrec").join("config.toml");
        set(&path, "theme", "ogre-mode").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "theme = \"ogre-mode\"\n"
        );
        // A file that isn't TOML is never written over.
        fs::write(&path, "theme = \n").unwrap();
        assert_eq!(
            set(&path, "theme", "swamp-day").unwrap_err().code,
            qrec_core::ErrorCode::Invalid
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \n");
    }

    #[test]
    fn a_write_keeps_the_permissions_and_the_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let dotfiles = tmp.path().join("dotfiles");
        fs::create_dir(&dotfiles).unwrap();
        let real = dotfiles.join("qrec-config.toml");
        fs::write(&real, "# mine\ntheme = \"swamp-night\"\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let dir = tmp.path().join("qrec");
        fs::create_dir(&dir).unwrap();
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink("../dotfiles/qrec-config.toml", &link).unwrap();

        set(&link, "theme", "swamp-day").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "still a link"
        );
        assert_eq!(
            fs::read_to_string(&real).unwrap(),
            "# mine\ntheme = \"swamp-day\"\n"
        );
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // A dangling link: its target is made.
        let gone = dir.join("keys.toml");
        std::os::unix::fs::symlink("../dotfiles/qrec-keys.toml", &gone).unwrap();
        write_text(&gone, "[bindings]\n").unwrap();
        assert_eq!(
            fs::read_to_string(dotfiles.join("qrec-keys.toml")).unwrap(),
            "[bindings]\n"
        );
    }

    #[test]
    fn the_gui_reads_the_config_once_and_the_session_reports_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let (config, file) = read(&path);
        assert_eq!(config, UserConfig::default(), "no file: the defaults");
        assert_eq!(file.take_problem(), None);

        fs::write(&path, "[canvas]\nfps = 60\n").unwrap();
        let (config, file) = read(&path);
        assert_eq!(config.canvas.fps, Fps::new(60, 1));
        assert_eq!(file.path(), path);

        // A file that can't be used gives the defaults; its problem waits
        // for the Session that gets this `ConfigFile`.
        fs::write(&path, "[canvas]\nfsp = 60\n").unwrap();
        let (config, file) = read(&path);
        assert_eq!(config, UserConfig::default());
        let session_side = file.clone();
        let problem = session_side.take_problem().unwrap();
        assert!(problem.contains("fsp"), "{problem}");
        assert_eq!(file.take_problem(), None, "reported once");
    }

    #[test]
    fn settings_files_are_read_within_bounds() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing.toml");
        assert_eq!(read_text(&missing), Ok(None));

        let small = tmp.path().join("small.toml");
        fs::write(&small, "theme = \"swamp-day\"\n").unwrap();
        assert_eq!(
            read_text(&small).unwrap().as_deref(),
            Some("theme = \"swamp-day\"\n")
        );

        let exact = tmp.path().join("exact.toml");
        fs::write(&exact, vec![b'#'; MAX_BYTES as usize]).unwrap();
        assert_eq!(read_text(&exact).unwrap().unwrap().len() as u64, MAX_BYTES);
        let big = tmp.path().join("big.toml");
        fs::write(&big, vec![b'#'; MAX_BYTES as usize + 1]).unwrap();
        let err = read_text(&big).unwrap_err();
        assert!(err.contains("at most 1048576 bytes"), "{err}");

        let folder = tmp.path().join("folder.toml");
        fs::create_dir(&folder).unwrap();
        let err = read_text(&folder).unwrap_err();
        assert!(err.starts_with("can't read "), "{err}");
        assert!(err.ends_with("it isn't a regular file"), "{err}");

        // A FIFO is refused at once, never waited on.
        let fifo = tmp.path().join("fifo.toml");
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path; mkfifo only creates the node.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let err = read_text(&fifo).unwrap_err();
        assert!(err.ends_with("it isn't a regular file"), "{err}");

        let binary = tmp.path().join("binary.toml");
        fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        assert!(read_text(&binary).unwrap_err().starts_with("can't read "));
    }
}
