//! Output files are never overwritten (A§8.8): jobs write a temp file in the
//! destination directory, `.<stem>.qrec-part-<pid>-<job>.<ext>`, and the
//! final step is `renameat2(RENAME_NOREPLACE)` (fallbacks: `link` +
//! `unlink`, then an `O_EXCL` placeholder). Auto names loop `-2`, `-3`, …;
//! a hand-picked path that appeared meanwhile fails the job, and the result
//! is kept next to it as `<stem>-kept.<ext>` (`-kept-2`, …): a name that is
//! not a temp file's, so the host-start sweep (A§8.7) never deletes it.

use std::ffi::{CString, OsStr};
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use qrec_core::JobId;

use crate::paths::NAME_MAX;

/// The marker in every temp name; host start sweeps old temps (A§8.7),
/// recognized by their whole shape ([`is_temp_name`]), never by the marker
/// alone.
pub const PART_MARKER: &str = qrec_core::naming::TEMP_MARKER;

/// What a result kept aside gets after its stem (A§8.8).
const KEPT_SUFFIX: &str = "-kept";

/// How many `-N` names an auto name (or a kept result) tries.
const MAX_SUFFIX: u32 = 10_000;

/// `<dir>/.<stem>.qrec-part-<pid>-<job>.<ext>`. The stem is cut (on a char
/// boundary) so the name fits in [`NAME_MAX`]: any destination name that
/// fits gets a temp file that fits too (the final rename keeps the exact
/// name the person picked).
pub(crate) fn temp_path(dir: &Path, stem: &str, job: JobId, ext: &str) -> PathBuf {
    let ext = ext.trim_start_matches('.');
    let suffix = if ext.is_empty() {
        format!("{PART_MARKER}{}-{}", std::process::id(), job.0)
    } else {
        format!("{PART_MARKER}{}-{}.{ext}", std::process::id(), job.0)
    };
    let room = NAME_MAX.saturating_sub(1 + suffix.len());
    let mut cut = stem.len().min(room);
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    dir.join(format!(".{}{suffix}", &stem[..cut]))
}

/// Whether `name` has exactly the shape [`temp_path`] gives:
/// `.<stem>.qrec-part-<pid>-<job>`, then optionally `.<ext>` (one
/// extension). Only such files are swept as stale temps (A§8.7). Names that
/// merely contain the marker are not temps: an auto-named export of a
/// project called `talk.qrec-part-final`, or a result kept aside
/// (`<stem>-kept.<ext>`).
pub fn is_temp_name(name: &OsStr) -> bool {
    let Some(rest) = name.to_str().and_then(|n| n.strip_prefix('.')) else {
        return false;
    };
    let Some(at) = rest.rfind(PART_MARKER) else {
        return false;
    };
    let tail = &rest[at + PART_MARKER.len()..];
    let (ids, ext) = match tail.split_once('.') {
        Some((ids, ext)) => (ids, Some(ext)),
        None => (tail, None),
    };
    let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    ids.split_once('-')
        .is_some_and(|(pid, job)| number(pid) && number(job))
        && ext.is_none_or(|ext| !ext.is_empty() && !ext.contains('.'))
}

/// Where a result is kept when its hand-picked path `out` appeared while
/// the job ran (A§8.8): `<stem>-kept.<ext>` next to it for `n == 1`, else
/// `<stem>-kept-<n>.<ext>`. The stem is cut (on a char boundary) so the name
/// fits in [`NAME_MAX`].
pub(crate) fn kept_path(out: &Path, n: u32) -> PathBuf {
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = out
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let suffix = if n <= 1 {
        format!("{KEPT_SUFFIX}{ext}")
    } else {
        format!("{KEPT_SUFFIX}-{n}{ext}")
    };
    let mut cut = stem.len().min(NAME_MAX.saturating_sub(suffix.len()));
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    out.with_file_name(format!("{}{suffix}", &stem[..cut]))
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
    /// at `kept` ([`kept_path`], or the temp path if it couldn't be moved).
    Appeared {
        out: PathBuf,
        kept: PathBuf,
    },
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
        } => fs::rename(tmp, path).map(|()| path.clone()).map_err(|e| {
            PlaceError::Io(format!("can't move the output to {}: {e}", path.display()))
        }),
        Destination::Chosen {
            path,
            replace: false,
        } => match rename_noreplace(tmp, path) {
            Ok(()) => Ok(path.clone()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(PlaceError::Appeared {
                out: path.clone(),
                kept: keep_aside(tmp, path),
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

/// Moves a finished result whose hand-picked path `out` was taken meanwhile
/// to the first free [`kept_path`] (never replacing anything) and returns
/// where it is. It is the result now (A§8.8), so it must not keep a temp
/// name that host start sweeps (A§8.7). If it can't be moved it stays at
/// `tmp`, which is reported instead.
fn keep_aside(tmp: &Path, out: &Path) -> PathBuf {
    for n in 1..=MAX_SUFFIX {
        let kept = kept_path(out, n);
        match rename_noreplace(tmp, &kept) {
            Ok(()) => return kept,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                tracing::warn!(
                    "can't move {} to {}: {e}; the result stays where it is",
                    tmp.display(),
                    kept.display()
                );
                return tmp.to_path_buf();
            }
        }
    }
    tracing::warn!(
        "{} and {MAX_SUFFIX} numbered variants all exist; the result stays at {}",
        kept_path(out, 1).display(),
        tmp.display()
    );
    tmp.to_path_buf()
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
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
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
        let tmp = temp_path(
            Path::new("/p/exports"),
            "my-video-20261014-120000",
            JobId(7),
            "mp4",
        );
        let name = tmp.file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(tmp.parent().unwrap(), Path::new("/p/exports"));
        assert_eq!(
            name,
            format!(
                ".my-video-20261014-120000.qrec-part-{}-7.mp4",
                std::process::id()
            )
        );
        assert!(name.contains(PART_MARKER));
        assert_eq!(file_stem(Path::new("/p/exports/a.b.mp4")), "a.b");
        assert_eq!(file_stem(Path::new("/")), "qrec");
    }

    /// A hand-picked name of up to 255 bytes is valid, so its temp file
    /// must be too: the stem is cut, never the marker, job or extension.
    #[test]
    fn temp_names_of_long_outputs_fit_in_name_max() {
        let dir = tempfile::tempdir().unwrap();
        for stem in ["y".repeat(251), "é".repeat(120), "x".repeat(300)] {
            for job in [JobId(1), JobId(u64::MAX)] {
                let tmp = temp_path(dir.path(), &stem, job, "mp4");
                let name = tmp.file_name().unwrap().to_str().unwrap();
                assert!(name.len() <= NAME_MAX, "{} bytes", name.len());
                assert!(name.starts_with(&format!(".{}", &stem[..10])), "{name}");
                assert!(
                    name.ends_with(&format!(
                        "{PART_MARKER}{}-{}.mp4",
                        std::process::id(),
                        job.0
                    )),
                    "{name}"
                );
                // The file system takes it.
                fs::write(&tmp, b"x").unwrap();
                fs::remove_file(&tmp).unwrap();
            }
        }
        // Short names are kept whole.
        let tmp = temp_path(dir.path(), "short", JobId(2), "png");
        assert!(
            tmp.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(".short.qrec-part-")
        );
    }

    #[test]
    fn numbered_names() {
        assert_eq!(numbered(Path::new("/e/a.mp4"), 1), Path::new("/e/a.mp4"));
        assert_eq!(numbered(Path::new("/e/a.mp4"), 2), Path::new("/e/a-2.mp4"));
        assert_eq!(
            numbered(Path::new("/e/a.b.png"), 3),
            Path::new("/e/a.b-3.png")
        );
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
        let kept = dir.path().join("mine-kept.mp4");
        match place(&tmp, &dest) {
            Err(PlaceError::Appeared { out: o, kept: k }) => {
                assert_eq!((o.as_path(), k.as_path()), (out.as_path(), kept.as_path()));
                let message = PlaceError::Appeared { out: o, kept: k }.to_string();
                assert!(message.starts_with("confirmation required: "), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read_to_string(&out).unwrap(), "someone else's");
        assert_eq!(
            fs::read_to_string(&kept).unwrap(),
            "new",
            "the render is kept, under a name that isn't a temp's"
        );
        assert!(!tmp.exists());

        let tmp = temp_path(dir.path(), "mine", JobId(2), "mp4");
        fs::write(&tmp, "newer").unwrap();
        let dest = Destination::Chosen {
            path: out.clone(),
            replace: true,
        };
        assert_eq!(place(&tmp, &dest).unwrap(), out);
        assert_eq!(fs::read_to_string(&out).unwrap(), "newer");
        assert_eq!(fs::read_to_string(&kept).unwrap(), "new");
    }

    /// A result kept aside never replaces anything either, and never keeps
    /// a temp name (the host-start sweep deletes those, A§8.7).
    #[test]
    fn kept_results_count_up_and_are_not_temps() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("final.webm");
        fs::write(&out, "someone else's").unwrap();
        fs::write(dir.path().join("final-kept.webm"), "kept before").unwrap();
        let dest = Destination::Chosen {
            path: out.clone(),
            replace: false,
        };
        for (job, expected) in [(1, "final-kept-2.webm"), (2, "final-kept-3.webm")] {
            let tmp = temp_path(dir.path(), "final", JobId(job), "webm");
            fs::write(&tmp, format!("render {job}")).unwrap();
            let Err(PlaceError::Appeared { kept, .. }) = place(&tmp, &dest) else {
                panic!("the output was taken")
            };
            assert_eq!(kept, dir.path().join(expected));
            assert_eq!(fs::read_to_string(&kept).unwrap(), format!("render {job}"));
            assert!(!is_temp_name(kept.file_name().unwrap()), "{kept:?}");
        }
        assert_eq!(fs::read_to_string(&out).unwrap(), "someone else's");
        assert_eq!(
            fs::read_to_string(dir.path().join("final-kept.webm")).unwrap(),
            "kept before"
        );
        // Names without an extension, hidden names and names that look like
        // temps get the suffix too, which makes them no temps.
        assert_eq!(
            kept_path(Path::new("/e/noext"), 1),
            Path::new("/e/noext-kept")
        );
        assert_eq!(
            kept_path(Path::new("/e/.hidden.png"), 2),
            Path::new("/e/.hidden-kept-2.png")
        );
        let tempish = kept_path(Path::new("/e/.x.qrec-part-1-2.mp4"), 1);
        assert_eq!(tempish, Path::new("/e/.x.qrec-part-1-2-kept.mp4"));
        assert!(!is_temp_name(tempish.file_name().unwrap()));
        // The longest name a person may pick still gets a kept name that fits.
        for stem in ["y".repeat(251), "é".repeat(125)] {
            let long = dir.path().join(format!("{stem}.mp4"));
            for n in [1, MAX_SUFFIX] {
                let kept = kept_path(&long, n);
                let name = kept.file_name().unwrap().to_str().unwrap();
                assert!(name.len() <= NAME_MAX, "{} bytes", name.len());
                assert!(name.ends_with(".mp4") && name.contains("-kept"), "{name}");
                fs::write(&kept, b"x").unwrap();
            }
        }
    }

    /// Only names [`temp_path`] makes are temps: not every name with the
    /// marker in it (a project called `talk.qrec-part-final`, a kept result).
    #[test]
    fn temp_names_have_one_exact_shape() {
        let made = [
            temp_path(
                Path::new("/p/exports"),
                "v-20261014-120000",
                JobId(7),
                "mp4",
            ),
            temp_path(Path::new("/p/frames"), "a.b", JobId(1), "png"),
            temp_path(Path::new("/p/s"), "k", JobId(u64::MAX), ""),
            temp_path(Path::new("/p/s"), "x.qrec-part-1-2", JobId(3), "mov"),
            temp_path(Path::new("/p/s"), &"y".repeat(300), JobId(3), "json"),
        ];
        for tmp in &made {
            assert!(is_temp_name(tmp.file_name().unwrap()), "{tmp:?}");
        }
        for name in [
            "v-20261014-120000.mp4",
            "talk.qrec-part-final-20261014-120000.mp4",
            "talk.qrec-part-final-take-0001-00000003.png",
            ".talk.qrec-part-final.mp4",
            "x.qrec-part-1-2.mp4",
            ".x.qrec-part-1.mp4",
            ".x.qrec-part--2.mp4",
            ".x.qrec-part-1-2-kept.mp4",
            ".x.qrec-part-1-2.",
            ".x.qrec-part-1-2.tar.gz",
            ".x.qrec-part-1-two.mp4",
            ".x.qrec-part-",
            "final-kept.webm",
        ] {
            assert!(!is_temp_name(OsStr::new(name)), "{name}");
        }
    }
}
