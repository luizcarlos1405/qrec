//! `config.toml` (A§9.8): reading it for the GUI's startup (docs/GUI.md
//! G§3.3 step 1, before the window maps) and for every Session
//! ([`ConfigFile`]). Reads are bounded: a settings file over [`MAX_BYTES`],
//! or one that isn't a regular file (a FIFO would block), is refused
//! instead of read. Writing it comes with the GUI's Settings pages. Owner:
//! meta.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use qrec_core::config::UserConfig;

pub use crate::facts::ConfigFile;

/// The most `config.toml` and `keys.toml` may hold: the GTK thread reads
/// them before the window maps (GUI R1).
pub const MAX_BYTES: u64 = 1 << 20;

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

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    use qrec_core::Fps;

    use super::*;

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
