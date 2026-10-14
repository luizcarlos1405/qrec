//! ScreenCast restore tokens (SPEC §5.1, A§8.11): `<state>/portal-tokens.json`,
//! mode 0600, keyed by output name, so the portal's picker appears once per
//! monitor and later sessions (headless ones too) restore without a dialog.
//!
//! ```json
//! {"version":1,"outputs":{"DP-4":{"token":"…","position":[0,0],"size":[2560,1440],"updated":"2026-10-14T12:00:00Z"}}}
//! ```
//!
//! A missing, unreadable or invalid file means "no tokens" (the picker shows
//! again); it never fails a recording. Writes are tmp + fsync + rename.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file format version.
pub const TOKENS_VERSION: u32 = 1;

/// The token file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalTokens {
    #[serde(default)]
    pub version: u32,
    /// By output name (`DP-4`).
    #[serde(default)]
    pub outputs: BTreeMap<String, TokenEntry>,
}

/// One monitor's restore token and where the portal said it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenEntry {
    pub token: String,
    /// Stream position in compositor coordinates, as the portal reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(i32, i32)>,
    /// Stream size in compositor coordinates, as the portal reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<(i32, i32)>,
    /// When the token was saved (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

impl PortalTokens {
    /// Reads `path`; anything but a valid file yields no tokens (logged).
    /// A file readable by others is tightened to 0600.
    pub fn load(path: &Path) -> Self {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!("can't read {}: {e}; the screen picker will show", path.display());
                return Self::default();
            }
        };
        if let Ok(meta) = fs::metadata(path)
            && meta.permissions().mode() & 0o077 != 0
        {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
        match serde_json::from_slice::<PortalTokens>(&bytes) {
            Ok(tokens) => tokens,
            Err(e) => {
                tracing::warn!("ignoring invalid {}: {e}", path.display());
                Self::default()
            }
        }
    }

    pub fn get(&self, output: &str) -> Option<&TokenEntry> {
        self.outputs.get(output)
    }

    pub fn set(&mut self, output: &str, entry: TokenEntry) {
        self.outputs.insert(output.to_owned(), entry);
    }

    pub fn remove(&mut self, output: &str) -> Option<TokenEntry> {
        self.outputs.remove(output)
    }

    /// Writes the file (0600) atomically, creating its directory (0700).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !dir.is_dir() {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        let mut doc = self.clone();
        doc.version = TOKENS_VERSION;
        let mut text = serde_json::to_vec_pretty(&doc).map_err(io::Error::other)?;
        text.push(b'\n');
        let tmp = temp_path(path);
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&text)?;
            file.sync_all()?;
            fs::rename(&tmp, path)?;
            if let Ok(dir) = fs::File::open(dir) {
                let _ = dir.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "portal-tokens.json".into());
    let nonce = getrandom::u64().unwrap_or(0);
    path.with_file_name(format!(".{name}.{}.{nonce:016x}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn entry(token: &str) -> TokenEntry {
        TokenEntry {
            token: token.into(),
            position: Some((0, 0)),
            size: Some((2560, 1440)),
            updated: Some("2026-10-14T12:00:00Z".into()),
        }
    }

    #[test]
    fn round_trips_with_private_permissions() {
        let env = crate::testkit::TestEnv::new();
        let path = env.state_home().join("qrec").join("portal-tokens.json");
        assert_eq!(PortalTokens::load(&path), PortalTokens::default());

        let mut tokens = PortalTokens::default();
        tokens.set("DP-4", entry("abc"));
        tokens.set("eDP-1", entry("def"));
        tokens.save(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );

        let loaded = PortalTokens::load(&path);
        assert_eq!(loaded.version, TOKENS_VERSION);
        assert_eq!(loaded.get("DP-4").unwrap().token, "abc");
        assert_eq!(loaded.outputs.len(), 2);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"outputs\""), "{text}");

        // Overwrite (replace) keeps 0600 and leaves no temp files behind.
        let mut loaded = loaded;
        assert!(loaded.remove("eDP-1").is_some());
        loaded.save(&path).unwrap();
        assert_eq!(PortalTokens::load(&path).outputs.len(), 1);
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn bad_files_mean_no_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("portal-tokens.json");
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(PortalTokens::load(&path), PortalTokens::default());
        // A world-readable file is tightened.
        fs::write(&path, r#"{"version":1,"outputs":{"DP-1":{"token":"t"}}}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let tokens = PortalTokens::load(&path);
        assert_eq!(tokens.get("DP-1").unwrap().token, "t");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    }
}
