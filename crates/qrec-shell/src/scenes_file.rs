//! `scenes.json`, the saved scenes (SPEC §5.6, A§9.11): next to
//! `config.toml` in `$XDG_CONFIG_HOME/qrec/`. Read with the settings
//! files' bounds (a FIFO never blocks, at most 1 MiB) and written whole
//! with their writer (a temp file renamed over it), so the settings watcher
//! sees one change and a crash leaves the old file or the new one.

use std::path::{Path, PathBuf};

use qrec_core::scenes::{SavedScenes, ScenesFile};

use crate::config_file;

/// The file's name.
pub const SCENES_FILE: &str = "scenes.json";

/// `scenes.json` next to `config_file` (`config.toml`).
pub fn beside(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .unwrap_or(Path::new("."))
        .join(SCENES_FILE)
}

/// The library at `path`: none when there is no file; `Unreadable` when it
/// can't be read or parsed.
pub fn read(path: &Path) -> ScenesFile {
    match config_file::read_text(path) {
        Ok(None) => ScenesFile::default(),
        Ok(Some(text)) => ScenesFile::parse(&text),
        Err(why) => ScenesFile::Unreadable(why),
    }
}

/// Writes the library at `path` (its folder made when missing).
pub fn write(path: &Path, scenes: &SavedScenes) -> Result<(), String> {
    config_file::write_text(path, &scenes.to_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrec_core::model::Scene;

    #[test]
    fn reads_back_what_it_wrote_and_names_what_it_cant_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = beside(&dir.path().join("qrec").join("config.toml"));
        assert_eq!(read(&path), ScenesFile::default(), "no file: no scenes");
        let lib = SavedScenes {
            scenes: vec![qrec_core::scenes::saved_from(
                "Talk".into(),
                chrono::Utc::now(),
                &Scene::default(),
                None,
            )],
            ..SavedScenes::default()
        };
        write(&path, &lib).unwrap();
        assert_eq!(read(&path), ScenesFile::Read(lib));
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(read(&path), ScenesFile::Unreadable(_)));
    }
}
