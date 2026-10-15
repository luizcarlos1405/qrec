//! Output files are never overwritten (A§8.8): jobs write a temp file in the
//! destination directory, `.<stem>.qrec-part-<pid>-<job>.<ext>`, and the
//! final step is `renameat2(RENAME_NOREPLACE)` (fallbacks: `link` +
//! `unlink`, then an `O_EXCL` placeholder). Auto names loop `-2`, `-3`, …;
//! a hand-picked path that appeared meanwhile fails the job, and the result
//! is kept next to it as `<stem>-kept.<ext>` (`-kept-2`, …): a name that is
//! not a temp file's, so the host-start sweep (A§8.7) never deletes it.
//!
//! An output's folder is opened once, when the job starts ([`OutDir`]), and
//! everything after that names it by that descriptor, never by a path
//! again: the temp file is created in it (`O_EXCL | O_NOFOLLOW`), ffmpeg
//! writes through that very file (its descriptor 3), and the final rename,
//! a result kept aside and a temp's deletion all happen in it. A folder on
//! the path swapped for a symlink while the job runs (into `takes/`, say)
//! can't redirect any of it; the job checks the open folder itself against
//! what an output may not touch (A§5.9) before it writes.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use qrec_core::JobId;
use qrec_core::ctx::FileId;

use crate::paths::{NAME_MAX, path_text};

/// The marker in every temp name; host start sweeps old temps (A§8.7),
/// recognized by their whole shape ([`is_temp_name`]), never by the marker
/// alone.
pub const PART_MARKER: &str = qrec_core::naming::TEMP_MARKER;

/// What a result kept aside gets after its stem (A§8.8).
const KEPT_SUFFIX: &str = "-kept";

/// How many `-N` names an auto name (or a kept result) tries.
const MAX_SUFFIX: u32 = 10_000;

/// How many folders [`OutDir::chain`] climbs at most.
const MAX_DEPTH: usize = 4096;

/// The mode a new file is created with, less the umask (like ffmpeg's).
const NEW_FILE_MODE: libc::c_uint = 0o666;

/// `.<stem>.qrec-part-<pid>-<job>.<ext>`. The stem is cut (on a char
/// boundary) so the name fits in [`NAME_MAX`]: any destination name that
/// fits gets a temp file that fits too (the final rename keeps the exact
/// name the person picked).
pub(crate) fn temp_name(stem: &str, job: JobId, ext: &str) -> String {
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
    format!(".{}{suffix}", &stem[..cut])
}

/// `<dir>/`[`temp_name`]: a temp file in the project's own folders
/// (segments, the concat list, the silence cache).
pub(crate) fn temp_path(dir: &Path, stem: &str, job: JobId, ext: &str) -> PathBuf {
    dir.join(temp_name(stem, job, ext))
}

/// Whether `name` has exactly the shape [`temp_name`] gives:
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

/// The name a result is kept under when its hand-picked name `name` was
/// taken while the job ran (A§8.8): `<stem>-kept.<ext>` for `n == 1`, else
/// `<stem>-kept-<n>.<ext>`. The stem is cut (on a char boundary) so the name
/// fits in [`NAME_MAX`].
pub(crate) fn kept_name(name: &OsStr, n: u32) -> OsString {
    let name = Path::new(name);
    let stem = name
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = name
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
    format!("{}{suffix}", &stem[..cut]).into()
}

/// The file name of `path` without its extension (`clip` for `a/clip.mp4`).
pub(crate) fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "qrec".to_owned())
}

/// The folder an output lands in, held open from the start of the job to
/// its end (A§8.8): `O_PATH | O_DIRECTORY`, opened by the path the plan
/// gives (symlinks followed, as the OS reads the path then). What the job
/// writes is named relative to this descriptor, so a path that leads
/// elsewhere later changes nothing. Clones share the descriptor.
#[derive(Debug, Clone)]
pub(crate) struct OutDir {
    fd: Arc<OwnedFd>,
    /// The path it was opened by, as the plan gave it (not resolved).
    named: PathBuf,
}

impl OutDir {
    /// Opens the folder `path` leads to now.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let fd = open_at(
            libc::AT_FDCWD,
            &c_name(path.as_os_str())?,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )?;
        Ok(Self {
            fd: Arc::new(fd),
            named: path.to_path_buf(),
        })
    }

    /// The path it was opened by.
    pub(crate) fn named(&self) -> &Path {
        &self.named
    }

    /// Whether both share one descriptor (one [`OutDir::open`]).
    pub(crate) fn same(&self, other: &OutDir) -> bool {
        Arc::ptr_eq(&self.fd, &other.fd)
    }

    fn raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Its identity, then those of the folders above it up to `/`, walked
    /// through `..` from the descriptor: where the folder really is, which
    /// no path to it can fake.
    pub(crate) fn chain(&self) -> Vec<FileId> {
        let mut ids: Vec<FileId> = Vec::new();
        let mut up: Option<OwnedFd> = None;
        for _ in 0..MAX_DEPTH {
            let fd = up.as_ref().map_or(self.raw(), AsRawFd::as_raw_fd);
            let Ok(st) = fstat(fd) else { break };
            let id = file_id(&st);
            // `..` of `/` is `/` itself.
            if ids.last() == Some(&id) {
                break;
            }
            ids.push(id);
            match open_at(
                fd,
                c"..",
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            ) {
                Ok(parent) => up = Some(parent),
                Err(_) => break,
            }
        }
        ids
    }

    /// Where the folder is now (the kernel's own path for the descriptor),
    /// or `None` once it was deleted.
    pub(crate) fn real_path(&self) -> Option<PathBuf> {
        let st = fstat(self.raw()).ok()?;
        if st.st_nlink == 0 {
            return None;
        }
        fs::read_link(format!("/proc/self/fd/{}", self.raw())).ok()
    }

    /// The folder's path to report: the plan's while that still leads
    /// here, else where the folder is now.
    pub(crate) fn location(&self) -> PathBuf {
        let here = fstat(self.raw()).ok().map(|st| file_id(&st));
        let named = fs::metadata(&self.named).ok().map(|m| FileId {
            dev: m.dev(),
            ino: m.ino(),
        });
        if here.is_some() && named == here {
            return self.named.clone();
        }
        self.real_path().unwrap_or_else(|| self.named.clone())
    }

    /// The path to report for `name` in this folder ([`OutDir::location`]).
    pub(crate) fn shown(&self, name: &OsStr) -> PathBuf {
        self.location().join(name)
    }

    /// Creates `name` here, readable and writable (`0666` less the umask):
    /// a new file, never one that was there or a symlink's target
    /// (`O_CREAT | O_EXCL | O_NOFOLLOW`).
    pub(crate) fn create(&self, name: &OsStr) -> io::Result<File> {
        let c = c_name(name)?;
        // SAFETY: a valid descriptor and NUL-terminated name; the mode is a plain integer.
        let fd = unsafe {
            libc::openat(
                self.raw(),
                c.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                NEW_FILE_MODE,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` was just returned by openat and is owned by nobody else.
        Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// The identity and size of the entry `name` here (`lstat`: a symlink is
    /// itself the entry), if there is one.
    pub(crate) fn entry(&self, name: &OsStr) -> io::Result<Option<(FileId, u64)>> {
        let c = c_name(name)?;
        // SAFETY: an all-zero `stat` is a valid value for fstatat to fill in.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a valid descriptor, a NUL-terminated name and a writable buffer.
        let rc =
            unsafe { libc::fstatat(self.raw(), c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
        if rc == 0 {
            return Ok(Some((file_id(&st), u64::try_from(st.st_size).unwrap_or(0))));
        }
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(e)
        }
    }

    /// Deletes the entry `name` here (never what a symlink there leads to).
    pub(crate) fn unlink(&self, name: &OsStr) -> io::Result<()> {
        let c = c_name(name)?;
        // SAFETY: a valid descriptor and a NUL-terminated name.
        if unsafe { libc::unlinkat(self.raw(), c.as_ptr(), 0) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Renames `from` to `to` here, replacing what `to` names.
    fn rename_over(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        rename_at(self.raw(), &c_name(from)?, self.raw(), &c_name(to)?)
    }

    /// Renames `from` to `to` here without replacing anything.
    fn rename_noreplace(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        noreplace_at(self.raw(), &c_name(from)?, self.raw(), &c_name(to)?)
    }
}

/// Where a finished output goes: a name in a folder held open (A§8.8).
#[derive(Debug, Clone)]
pub(crate) enum Destination {
    /// A name Qrec picked (`exports/…`, `frames/…`): taken ⇒ `-2`, `-3`, ….
    Auto { dir: OutDir, name: OsString },
    /// A path the person picked. `replace` is the file there they confirmed
    /// replacing (A§5.9), the only one that may be.
    Chosen {
        dir: OutDir,
        name: OsString,
        replace: Option<FileId>,
    },
}

impl Destination {
    pub(crate) fn dir(&self) -> &OutDir {
        match self {
            Destination::Auto { dir, .. } | Destination::Chosen { dir, .. } => dir,
        }
    }

    pub(crate) fn name(&self) -> &OsStr {
        match self {
            Destination::Auto { name, .. } | Destination::Chosen { name, .. } => name,
        }
    }

    /// The path the plan gave (before anything is placed).
    pub(crate) fn path(&self) -> PathBuf {
        self.dir().named().join(self.name())
    }
}

/// A job's temp output: a file it created in the destination's folder
/// ([`OutDir::create`]), which ffmpeg writes through its descriptor.
#[derive(Debug)]
pub(crate) struct TempOut {
    pub dir: OutDir,
    pub name: OsString,
    pub file: File,
}

impl TempOut {
    /// Creates [`temp_name`] in `dir`.
    pub(crate) fn create(dir: &OutDir, stem: &str, job: JobId, ext: &str) -> io::Result<Self> {
        let name = OsString::from(temp_name(stem, job, ext));
        let file = dir.create(&name)?;
        Ok(Self {
            dir: dir.clone(),
            name,
            file,
        })
    }

    /// Where it is, for logs and messages.
    pub(crate) fn shown(&self) -> PathBuf {
        self.dir.shown(&self.name)
    }
}

/// Why the final step failed.
#[derive(Debug)]
pub(crate) enum PlaceError {
    /// A hand-picked path appeared (or holds another file than the one the
    /// person confirmed) while the job ran; `bytes` is what is there. The
    /// output was kept at `kept` ([`kept_name`], or the temp file if it
    /// couldn't be moved).
    Appeared {
        out: PathBuf,
        bytes: u64,
        kept: PathBuf,
    },
    Io(String),
}

impl std::fmt::Display for PlaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaceError::Appeared { out, kept, .. } => write!(
                f,
                "confirmation required: {} appeared while Qrec was writing it; \
                 the new file was kept as {} (rename it, or run again with --yes to replace)",
                path_text(out),
                path_text(kept)
            ),
            PlaceError::Io(message) => f.write_str(message),
        }
    }
}

/// Moves the finished temp file `tmp` (a name in the destination's folder)
/// to its destination in that same folder, without replacing anything but
/// the file a person confirmed replacing. Returns where it is now.
pub(crate) fn place(tmp: &OsStr, dest: &Destination) -> Result<PathBuf, PlaceError> {
    let dir = dest.dir();
    let failed = |to: &OsStr, e: io::Error| {
        PlaceError::Io(format!(
            "can't move the output to {}: {e}",
            path_text(&dir.shown(to))
        ))
    };
    match dest {
        Destination::Chosen { name, replace, .. } => {
            if let Some(confirmed) = replace {
                match dir.entry(name) {
                    Ok(Some((id, _))) if id == *confirmed => {
                        return dir
                            .rename_over(tmp, name)
                            .map(|()| dir.shown(name))
                            .map_err(|e| failed(name, e));
                    }
                    // Gone, or another file now: nothing is replaced.
                    Ok(_) => {}
                    Err(e) => return Err(failed(name, e)),
                }
            }
            match dir.rename_noreplace(tmp, name) {
                Ok(()) => Ok(dir.shown(name)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(PlaceError::Appeared {
                    out: dir.shown(name),
                    bytes: dir.entry(name).ok().flatten().map_or(0, |(_, bytes)| bytes),
                    kept: keep_aside(dir, tmp, name),
                }),
                Err(e) => Err(failed(name, e)),
            }
        }
        Destination::Auto { name, .. } => {
            for n in 1..=MAX_SUFFIX {
                let candidate = numbered(name, n);
                match dir.rename_noreplace(tmp, &candidate) {
                    Ok(()) => return Ok(dir.shown(&candidate)),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(failed(&candidate, e)),
                }
            }
            Err(PlaceError::Io(format!(
                "{} and {MAX_SUFFIX} numbered variants all exist",
                path_text(&dir.shown(name))
            )))
        }
    }
}

/// Moves a finished result whose hand-picked name `out` was taken meanwhile
/// to the first free [`kept_name`] in the same folder (never replacing
/// anything) and returns where it is. It is the result now (A§8.8), so it
/// must not keep a temp name that host start sweeps (A§8.7). If it can't be
/// moved it stays at `tmp`, which is reported instead.
fn keep_aside(dir: &OutDir, tmp: &OsStr, out: &OsStr) -> PathBuf {
    for n in 1..=MAX_SUFFIX {
        let kept = kept_name(out, n);
        match dir.rename_noreplace(tmp, &kept) {
            Ok(()) => return dir.shown(&kept),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                tracing::warn!(
                    "can't move {} to {}: {e}; the result stays where it is",
                    dir.shown(tmp).display(),
                    dir.shown(&kept).display()
                );
                return dir.shown(tmp);
            }
        }
    }
    tracing::warn!(
        "{} and {MAX_SUFFIX} numbered variants all exist; the result stays at {}",
        dir.shown(&kept_name(out, 1)).display(),
        dir.shown(tmp).display()
    );
    dir.shown(tmp)
}

/// `n == 1`: `name`; otherwise `-<n>` before the extension
/// (`a-20261014-120000.mp4` → `a-20261014-120000-2.mp4`).
pub(crate) fn numbered(name: &OsStr, n: u32) -> OsString {
    if n <= 1 {
        return name.to_os_string();
    }
    let name = Path::new(name);
    let stem = name
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.extension() {
        Some(ext) => format!("{stem}-{n}.{}", ext.to_string_lossy()).into(),
        None => format!("{stem}-{n}").into(),
    }
}

/// `rename(from, to)` that fails with `AlreadyExists` instead of replacing
/// `to`: `renameat2(RENAME_NOREPLACE)`; where the file system doesn't
/// support it, `link` + `unlink`; where it can't link either, an `O_EXCL`
/// placeholder renamed over.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    noreplace_at(
        libc::AT_FDCWD,
        &c_name(from.as_os_str())?,
        libc::AT_FDCWD,
        &c_name(to.as_os_str())?,
    )
}

/// [`rename_noreplace`] relative to folder descriptors (or `AT_FDCWD`).
fn noreplace_at(from_dir: RawFd, from: &CStr, to_dir: RawFd, to: &CStr) -> io::Result<()> {
    // SAFETY: valid descriptors (or AT_FDCWD) and NUL-terminated names that
    // outlive the call.
    let rc = unsafe {
        libc::renameat2(
            from_dir,
            from.as_ptr(),
            to_dir,
            to.as_ptr(),
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
    // SAFETY: as above; linkat doesn't follow a symlink at `from` (no
    // AT_SYMLINK_FOLLOW) and never replaces `to`.
    if unsafe { libc::linkat(from_dir, from.as_ptr(), to_dir, to.as_ptr(), 0) } == 0 {
        // SAFETY: as above.
        if unsafe { libc::unlinkat(from_dir, from.as_ptr(), 0) } != 0 {
            tracing::warn!(
                "linked {} but can't unlink the temp: {}",
                to.to_string_lossy(),
                io::Error::last_os_error()
            );
        }
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::AlreadyExists {
        return Err(err);
    }
    // No hard links (e.g. vfat): reserve the name, then replace our own placeholder.
    let placeholder = open_at(
        to_dir,
        to,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    )?;
    drop(placeholder);
    rename_at(from_dir, from, to_dir, to)
}

/// `renameat(from_dir, from, to_dir, to)`, replacing `to`.
fn rename_at(from_dir: RawFd, from: &CStr, to_dir: RawFd, to: &CStr) -> io::Result<()> {
    // SAFETY: valid descriptors (or AT_FDCWD) and NUL-terminated names.
    if unsafe { libc::renameat(from_dir, from.as_ptr(), to_dir, to.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `openat(dir, name, flags)` (a new file gets `0666` less the umask).
fn open_at(dir: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: a valid descriptor (or AT_FDCWD) and a NUL-terminated name;
    // the mode is a plain integer.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags, NEW_FILE_MODE) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by openat and is owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    // SAFETY: an all-zero `stat` is a valid value for fstat to fill in.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a valid descriptor and a writable buffer.
    if unsafe { libc::fstat(fd, &mut st) } == 0 {
        Ok(st)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `st_dev` and `st_ino` (both 64-bit on Linux).
fn file_id(st: &libc::stat) -> FileId {
    FileId {
        dev: st.st_dev,
        ino: st.st_ino,
    }
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path contains a NUL byte"))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn os(name: &str) -> &OsStr {
        OsStr::new(name)
    }

    /// The identity of what is at `path` (`lstat`).
    fn lstat_id(path: &Path) -> FileId {
        let meta = fs::symlink_metadata(path).unwrap();
        FileId {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }

    /// A temp file in `dir`, as a job makes one.
    fn temp_in(dir: &OutDir, stem: &str, job: u64, content: &str) -> OsString {
        let tmp = TempOut::create(dir, stem, JobId(job), "mp4").unwrap();
        fs::write(tmp.dir.shown(&tmp.name), content).unwrap();
        tmp.name
    }

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
        assert_eq!(numbered(os("a.mp4"), 1), os("a.mp4"));
        assert_eq!(numbered(os("a.mp4"), 2), os("a-2.mp4"));
        assert_eq!(numbered(os("a.b.png"), 3), os("a.b-3.png"));
        assert_eq!(numbered(os("noext"), 2), os("noext-2"));
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
        let out = OutDir::open(dir.path()).unwrap();
        for (n, expected) in [(1, "v.mp4"), (2, "v-2.mp4"), (3, "v-3.mp4")] {
            let tmp = temp_in(&out, "v", n, &format!("render {n}"));
            let dest = Destination::Auto {
                dir: out.clone(),
                name: "v.mp4".into(),
            };
            let placed = place(&tmp, &dest).unwrap();
            assert_eq!(placed, dir.path().join(expected));
            assert_eq!(fs::read_to_string(&placed).unwrap(), format!("render {n}"));
            assert!(!dir.path().join(&tmp).exists());
        }
    }

    #[test]
    fn chosen_paths_need_confirmation_to_be_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let folder = OutDir::open(dir.path()).unwrap();
        let out = dir.path().join("mine.mp4");
        let tmp = temp_in(&folder, "mine", 1, "new");
        fs::write(&out, "someone else's").unwrap();
        let dest = Destination::Chosen {
            dir: folder.clone(),
            name: "mine.mp4".into(),
            replace: None,
        };
        let kept = dir.path().join("mine-kept.mp4");
        match place(&tmp, &dest) {
            Err(PlaceError::Appeared {
                out: o,
                bytes,
                kept: k,
            }) => {
                assert_eq!(
                    (o.as_path(), bytes, k.as_path()),
                    (out.as_path(), 14, kept.as_path())
                );
                let message = PlaceError::Appeared {
                    out: o,
                    bytes,
                    kept: k,
                }
                .to_string();
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
        assert!(!dir.path().join(&tmp).exists());

        let tmp = temp_in(&folder, "mine", 2, "newer");
        let dest = Destination::Chosen {
            dir: folder.clone(),
            name: "mine.mp4".into(),
            replace: Some(lstat_id(&out)),
        };
        assert_eq!(place(&tmp, &dest).unwrap(), out);
        assert_eq!(fs::read_to_string(&out).unwrap(), "newer");
        assert_eq!(fs::read_to_string(&kept).unwrap(), "new");
    }

    /// The confirmation is for one file: another one at the path by the
    /// time the job ends (the confirmed one moved away, or replaced) is
    /// kept, like a file that appeared (A§8.8). A confirmed file that is
    /// gone is simply not there to replace.
    #[test]
    fn only_the_confirmed_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let folder = OutDir::open(dir.path()).unwrap();
        let out = dir.path().join("final.webm");
        fs::write(&out, "confirmed").unwrap();
        let confirmed = lstat_id(&out);
        fs::rename(&out, dir.path().join("moved.webm")).unwrap();
        fs::write(&out, "another").unwrap();
        let dest = Destination::Chosen {
            dir: folder.clone(),
            name: "final.webm".into(),
            replace: Some(confirmed),
        };
        let tmp = temp_in(&folder, "final", 1, "render");
        let Err(PlaceError::Appeared { bytes, kept, .. }) = place(&tmp, &dest) else {
            panic!("another file is there")
        };
        assert_eq!(bytes, 7);
        assert_eq!(fs::read_to_string(&out).unwrap(), "another");
        assert_eq!(fs::read_to_string(&kept).unwrap(), "render");
        assert_eq!(
            fs::read_to_string(dir.path().join("moved.webm")).unwrap(),
            "confirmed"
        );

        fs::remove_file(&out).unwrap();
        let tmp = temp_in(&folder, "final", 2, "render 2");
        assert_eq!(place(&tmp, &dest).unwrap(), out);
        assert_eq!(fs::read_to_string(&out).unwrap(), "render 2");
    }

    /// A result kept aside never replaces anything either, and never keeps
    /// a temp name (the host-start sweep deletes those, A§8.7).
    #[test]
    fn kept_results_count_up_and_are_not_temps() {
        let dir = tempfile::tempdir().unwrap();
        let folder = OutDir::open(dir.path()).unwrap();
        let out = dir.path().join("final.webm");
        fs::write(&out, "someone else's").unwrap();
        fs::write(dir.path().join("final-kept.webm"), "kept before").unwrap();
        let dest = Destination::Chosen {
            dir: folder.clone(),
            name: "final.webm".into(),
            replace: None,
        };
        for (job, expected) in [(1, "final-kept-2.webm"), (2, "final-kept-3.webm")] {
            let tmp = temp_in(&folder, "final", job, &format!("render {job}"));
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
        assert_eq!(kept_name(os("noext"), 1), os("noext-kept"));
        assert_eq!(kept_name(os(".hidden.png"), 2), os(".hidden-kept-2.png"));
        let tempish = kept_name(os(".x.qrec-part-1-2.mp4"), 1);
        assert_eq!(tempish, os(".x.qrec-part-1-2-kept.mp4"));
        assert!(!is_temp_name(&tempish));
        // The longest name a person may pick still gets a kept name that fits.
        for stem in ["y".repeat(251), "é".repeat(125)] {
            let long = format!("{stem}.mp4");
            for n in [1, MAX_SUFFIX] {
                let kept = kept_name(os(&long), n);
                let name = kept.to_str().unwrap();
                assert!(name.len() <= NAME_MAX, "{} bytes", name.len());
                assert!(name.ends_with(".mp4") && name.contains("-kept"), "{name}");
                fs::write(dir.path().join(&kept), b"x").unwrap();
            }
        }
    }

    /// Only names [`temp_name`] makes are temps: not every name with the
    /// marker in it (a project called `talk.qrec-part-final`, a kept result).
    #[test]
    fn temp_names_have_one_exact_shape() {
        let made = [
            temp_name("v-20261014-120000", JobId(7), "mp4"),
            temp_name("a.b", JobId(1), "png"),
            temp_name("k", JobId(u64::MAX), ""),
            temp_name("x.qrec-part-1-2", JobId(3), "mov"),
            temp_name(&"y".repeat(300), JobId(3), "json"),
        ];
        for tmp in &made {
            assert!(is_temp_name(os(tmp)), "{tmp:?}");
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

    /// Everything a job does in its output folder goes through the
    /// descriptor it opened: once the path leads somewhere else (the folder
    /// moved, a symlink in its place), the temp file, the final rename and
    /// the reported path stay with the folder, and nothing reaches where
    /// the path leads now (A§8.8).
    #[test]
    fn an_open_folder_is_never_found_again_by_its_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let (folder, moved, elsewhere) = (root.join("out"), root.join("moved"), root.join("else"));
        fs::create_dir(&folder).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("precious.mkv"), "recorded").unwrap();
        let dir = OutDir::open(&folder).unwrap();
        assert_eq!(dir.shown(os("a.mp4")), folder.join("a.mp4"));
        let chain = dir.chain();
        assert_eq!(chain[0], lstat_id(&folder));
        assert_eq!(chain[1], lstat_id(&root));
        assert_eq!(*chain.last().unwrap(), lstat_id(Path::new("/")));

        // The folder moves away and a symlink takes its place.
        fs::rename(&folder, &moved).unwrap();
        symlink(&elsewhere, &folder).unwrap();
        assert_eq!(dir.real_path().unwrap(), moved);
        assert_eq!(dir.chain()[0], lstat_id(&moved));
        let tmp_name = temp_in(&dir, "precious", 1, "export");
        assert!(moved.join(&tmp_name).is_file());
        assert!(!elsewhere.join(&tmp_name).exists());
        let dest = Destination::Chosen {
            dir: dir.clone(),
            name: "precious.mkv".into(),
            replace: None,
        };
        assert_eq!(place(&tmp_name, &dest).unwrap(), moved.join("precious.mkv"));
        assert_eq!(
            fs::read_to_string(elsewhere.join("precious.mkv")).unwrap(),
            "recorded"
        );
        assert_eq!(
            fs::read_to_string(moved.join("precious.mkv")).unwrap(),
            "export"
        );

        // A name taken by a symlink is never written through.
        symlink(elsewhere.join("precious.mkv"), moved.join("link.mkv")).unwrap();
        assert_eq!(
            dir.create(os("link.mkv")).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        dir.unlink(os("link.mkv")).unwrap();
        assert!(elsewhere.join("precious.mkv").exists());

        // Deleted: nothing can be created in it any more.
        fs::remove_dir_all(&moved).unwrap();
        assert_eq!(dir.real_path(), None);
        assert!(dir.create(os("x.mp4")).is_err());
        assert_eq!(names(&elsewhere), ["precious.mkv"]);
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}
