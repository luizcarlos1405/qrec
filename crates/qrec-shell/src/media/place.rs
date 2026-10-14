//! Output files are never overwritten (A§8.8): jobs write a temp file in the
//! destination directory, `.<stem>.qrec-part-<pid>-<job>.<ext>`, and the
//! final step is `renameat2(RENAME_NOREPLACE)` (fallbacks: `link` +
//! `unlink`, then an `O_EXCL` placeholder). Auto names loop `-2`, `-3`, …;
//! a hand-picked path that appeared meanwhile keeps the temp and fails.

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use qrec_core::JobId;

/// The marker in every temp name; host start sweeps old ones (A§8.7).
pub const PART_MARKER: &str = ".qrec-part-";

/// How many `-N` names an auto name tries.
const MAX_SUFFIX: u32 = 10_000;

/// `<dir>/.<stem>.qrec-part-<pid>-<job>.<ext>`.
pub(crate) fn temp_path(dir: &Path, stem: &str, job: JobId, ext: &str) -> PathBuf {
    let ext = ext.trim_start_matches('.');
    let name = if ext.is_empty() {
        format!(".{stem}{PART_MARKER}{}-{}", std::process::id(), job.0)
    } else {
        format!(".{stem}{PART_MARKER}{}-{}.{ext}", std::process::id(), job.0)
    };
    dir.join(name)
}

/// The file name of `path` without its extension (`clip` for `a/clip.mp4`).
pub(crate) fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "qrec".to_owned())
}

/// Where a finished output goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Destination {
    /// A name Qrec picked (`exports/…`, `frames/…`): taken ⇒ `-2`, `-3`, ….
    Auto(PathBuf),
    /// A path the person picked. `replace` only when it already existed
    /// when the job started (so the core had their confirmation).
    Chosen { path: PathBuf, replace: bool },
}

impl Destination {
    pub(crate) fn path(&self) -> &Path {
        match self {
            Destination::Auto(path) | Destination::Chosen { path, .. } => path,
        }
    }
}

/// Why the final step failed.
#[derive(Debug)]
pub(crate) enum PlaceError {
    /// A hand-picked path appeared while the job ran; the output was kept
    /// at `kept`.
    Appeared { out: PathBuf, kept: PathBuf },
    Io(String),
}

impl std::fmt::Display for PlaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaceError::Appeared { out, kept } => write!(
                f,
                "confirmation required: {} appeared while Qrec was writing it; \
                 the new file was kept as {} (rename it, or run again with --yes to replace)",
                out.display(),
                kept.display()
            ),
            PlaceError::Io(message) => f.write_str(message),
        }
    }
}

/// Moves the finished temp file to its destination without replacing
/// anything (except a confirmed hand-picked path). Returns the real path.
pub(crate) fn place(tmp: &Path, dest: &Destination) -> Result<PathBuf, PlaceError> {
    match dest {
        Destination::Chosen {
            path,
            replace: true,
        } => fs::rename(tmp, path)
            .map(|()| path.clone())
            .map_err(|e| PlaceError::Io(format!("can't move the output to {}: {e}", path.display()))),
        Destination::Chosen {
            path,
            replace: false,
        } => match rename_noreplace(tmp, path) {
            Ok(()) => Ok(path.clone()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(PlaceError::Appeared {
                out: path.clone(),
                kept: tmp.to_path_buf(),
            }),
            Err(e) => Err(PlaceError::Io(format!(
                "can't move the output to {}: {e}",
                path.display()
            ))),
        },
        Destination::Auto(path) => {
            for n in 1..=MAX_SUFFIX {
                let candidate = numbered(path, n);
                match rename_noreplace(tmp, &candidate) {
                    Ok(()) => return Ok(candidate),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => {
                        return Err(PlaceError::Io(format!(
                            "can't move the output to {}: {e}",
                            candidate.display()
                        )));
                    }
                }
            }
            Err(PlaceError::Io(format!(
                "{} and {MAX_SUFFIX} numbered variants all exist",
                path.display()
            )))
        }
    }
}

/// `n == 1`: `path`; otherwise `-<n>` before the extension
/// (`a-20261014-120000.mp4` → `a-20261014-120000-2.mp4`).
pub(crate) fn numbered(path: &Path, n: u32) -> PathBuf {
    if n <= 1 {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match path.extension() {
        Some(ext) => format!("{stem}-{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{n}"),
    };
    path.with_file_name(name)
}

/// `rename(from, to)` that fails with `AlreadyExists` instead of replacing
/// `to`: `renameat2(RENAME_NOREPLACE)`; where the file system doesn't
/// support it, `link` + `unlink`; where it can't link either, an `O_EXCL`
/// placeholder renamed over.
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let c_from = c_path(from)?;
    let c_to = c_path(to)?;
    // SAFETY: both arguments are valid NUL-terminated paths that outlive the call.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            c_from.as_ptr(),
            libc::AT_FDCWD,
            c_to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP) => {}
        _ => return Err(err),
    }
    match fs::hard_link(from, to) {
        Ok(()) => {
            if let Err(e) = fs::remove_file(from) {
                tracing::warn!("linked {} but can't unlink the temp: {e}", to.display());
            }
            return Ok(());
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
        Err(_) => {}
    }
    // No hard links (e.g. vfat): reserve the name, then replace our own placeholder.
    OpenOptions::new().write(true).create_new(true).open(to)?;
    fs::rename(from, to)
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path contains a NUL byte"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_live_next_to_the_output() {
        let tmp = temp_path(Path::new("/p/exports"), "my-video-20261014-120000", JobId(7), "mp4");
        let name = tmp.file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(tmp.parent().unwrap(), Path::new("/p/exports"));
        assert_eq!(
            name,
            format!(".my-video-20261014-120000.qrec-part-{}-7.mp4", std::process::id())
        );
        assert!(name.contains(PART_MARKER));
        assert_eq!(file_stem(Path::new("/p/exports/a.b.mp4")), "a.b");
        assert_eq!(file_stem(Path::new("/")), "qrec");
    }

    #[test]
    fn numbered_names() {
        assert_eq!(numbered(Path::new("/e/a.mp4"), 1), Path::new("/e/a.mp4"));
        assert_eq!(numbered(Path::new("/e/a.mp4"), 2), Path::new("/e/a-2.mp4"));
        assert_eq!(numbered(Path::new("/e/a.b.png"), 3), Path::new("/e/a.b-3.png"));
        assert_eq!(numbered(Path::new("/e/noext"), 2), Path::new("/e/noext-2"));
    }

    #[test]
    fn rename_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "new").unwrap();
        fs::write(&b, "old").unwrap();
        let err = rename_noreplace(&a, &b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&b).unwrap(), "old");
        assert!(a.exists());
        let c = dir.path().join("c");
        rename_noreplace(&a, &c).unwrap();
        assert_eq!(fs::read_to_string(&c).unwrap(), "new");
        assert!(!a.exists());
    }

    #[test]
    fn auto_names_count_up() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("v.mp4");
        for (n, expected) in [(1, "v.mp4"), (2, "v-2.mp4"), (3, "v-3.mp4")] {
            let tmp = temp_path(dir.path(), "v", JobId(n), "mp4");
            fs::write(&tmp, format!("render {n}")).unwrap();
            let placed = place(&tmp, &Destination::Auto(out.clone())).unwrap();
            assert_eq!(placed, dir.path().join(expected));
            assert_eq!(fs::read_to_string(&placed).unwrap(), format!("render {n}"));
            assert!(!tmp.exists());
        }
    }

    #[test]
    fn chosen_paths_need_confirmation_to_be_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("mine.mp4");
        let tmp = temp_path(dir.path(), "mine", JobId(1), "mp4");
        fs::write(&tmp, "new").unwrap();
        fs::write(&out, "someone else's").unwrap();
        let dest = Destination::Chosen {
            path: out.clone(),
            replace: false,
        };
        match place(&tmp, &dest) {
            Err(PlaceError::Appeared { out: o, kept }) => {
                assert_eq!((o.as_path(), kept.as_path()), (out.as_path(), tmp.as_path()));
                let message = PlaceError::Appeared { out: o, kept }.to_string();
                assert!(message.starts_with("confirmation required: "), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read_to_string(&out).unwrap(), "someone else's");
        assert_eq!(fs::read_to_string(&tmp).unwrap(), "new", "the render is kept");

        let dest = Destination::Chosen {
            path: out.clone(),
            replace: true,
        };
        assert_eq!(place(&tmp, &dest).unwrap(), out);
        assert_eq!(fs::read_to_string(&out).unwrap(), "new");
    }
}
