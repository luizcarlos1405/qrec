//! File primitives of the store: atomic replace (tmp → fsync → rename →
//! fsync(dir)), durable appends (`O_APPEND` + `fdatasync`) and
//! never-overwrite placement (`renameat2(RENAME_NOREPLACE)`, falling back
//! to `link` + `unlink`; A§8.8).

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// How hard [`replace`] works to make a write survive a power loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sync {
    /// No fsync at all (`ui.json`).
    None,
    /// fsync the file before the rename.
    File,
    /// fsync the file, rename, then fsync the directory (`qrec.json`).
    FileAndDir,
}

/// `<path>.tmp`, the temp name of an atomic replace.
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Replaces `path` with `bytes` atomically: write `<path>.tmp` (mode 0644,
/// truncating a leftover), optionally fsync it, `rename` it over `path`,
/// optionally fsync the directory. Readers see the old or the new file,
/// never a mix.
pub fn replace(path: &Path, bytes: &[u8], sync: Sync) -> io::Result<()> {
    let tmp = tmp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&tmp)?;
        file.write_all(bytes)?;
        if sync != Sync::None {
            file.sync_all()?;
        }
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    if sync == Sync::FileAndDir {
        sync_dir(path.parent().unwrap_or(Path::new(".")))?;
    }
    Ok(())
}

/// fsyncs a directory, making renames and creations in it durable.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    File::open(dir)?.sync_all()
}

/// Appends `bytes` (one whole line, newline included) with a single
/// `O_APPEND` write, then `fdatasync`s the file. Creates it (0644) if missing.
pub fn append_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o644)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_data()
}

/// Moves `from` to `to` without ever replacing an existing `to`:
/// `renameat2(RENAME_NOREPLACE)`, or `link` + `unlink` where the file
/// system doesn't support it. `AlreadyExists` when `to` exists.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let c_from = cstring(from)?;
    let c_to = cstring(to)?;
    // SAFETY: both paths are valid NUL-terminated strings that outlive the call.
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
        // The file system (or kernel) can't do it: link never replaces either.
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP) => {
            fs::hard_link(from, to)?;
            fs::remove_file(from)
        }
        _ => Err(err),
    }
}

fn cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL byte"))
}

/// `name.ext` → `name-2.ext`, `name-3.ext`, … for `n = 2, 3, …` (the
/// suffix goes before the first `.` after the stem's start, so
/// `qrec.v1.json.bak` becomes `qrec.v1-2.json.bak`).
pub fn numbered(path: &Path, n: u32, ext_from: &str) -> PathBuf {
    if n <= 1 {
        return path.to_path_buf();
    }
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = match name.find(ext_from) {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name.as_str(), ""),
    };
    path.with_file_name(format!("{stem}-{n}{ext}"))
}

/// Writes `bytes` to the first free name among `path`, `path-2`, … (see
/// [`numbered`]) without replacing anything, durably. Returns the name used.
pub fn write_new(path: &Path, bytes: &[u8], ext_from: &str) -> io::Result<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".qrec-part-{}", std::process::id()));
        PathBuf::from(name)
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&tmp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let placed = move_new(&tmp, path, ext_from);
    if placed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    let placed = placed?;
    sync_dir(dir)?;
    Ok(placed)
}

/// Moves `from` to the first free name among `to`, `to-2`, … (see
/// [`numbered`]) without replacing anything. Returns the name used.
pub fn move_new(from: &Path, to: &Path, ext_from: &str) -> io::Result<PathBuf> {
    for n in 1..=10_000 {
        let target = numbered(to, n, ext_from);
        match rename_no_replace(from, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("no free name like {}", to.display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_is_atomic_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("qrec.json");
        replace(&path, b"one", Sync::FileAndDir).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        // A leftover temp file from a crash is simply overwritten.
        fs::write(tmp_path(&path), b"garbage that is longer").unwrap();
        replace(&path, b"two", Sync::File).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!tmp_path(&path).exists());
        replace(&path, b"three", Sync::None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"three");
        // A missing directory fails without leaving anything behind.
        assert!(replace(&tmp.path().join("no/such.json"), b"x", Sync::None).is_err());
    }

    #[test]
    fn appends_are_whole_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("h.jsonl");
        append_durable(&path, b"{\"seq\":1}\n").unwrap();
        append_durable(&path, b"{\"seq\":2}\n").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\"seq\":1}\n{\"seq\":2}\n"
        );
    }

    #[test]
    fn nothing_is_ever_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        let err = rename_no_replace(&a, &b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&b).unwrap(), "b");
        rename_no_replace(&a, &tmp.path().join("c")).unwrap();
        assert!(!a.exists());

        let bak = tmp.path().join(".qrec/qrec.v1.json.bak");
        assert_eq!(write_new(&bak, b"1", ".json").unwrap(), bak);
        let second = write_new(&bak, b"2", ".json").unwrap();
        assert_eq!(second, tmp.path().join(".qrec/qrec.v1-2.json.bak"));
        assert_eq!(fs::read(&bak).unwrap(), b"1");
        assert_eq!(fs::read(&second).unwrap(), b"2");
        let leftovers: Vec<_> = fs::read_dir(tmp.path().join(".qrec"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().contains("qrec-part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn numbering() {
        let p = Path::new("/p/.qrec/rejected/qrec-20261014-120000.json");
        assert_eq!(numbered(p, 1, ".json"), p);
        assert_eq!(
            numbered(p, 3, ".json"),
            Path::new("/p/.qrec/rejected/qrec-20261014-120000-3.json")
        );
        assert_eq!(
            numbered(Path::new("/x/noext"), 2, ".json"),
            Path::new("/x/noext-2")
        );
    }
}
