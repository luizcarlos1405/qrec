//! `Effect::DeleteTakeDirs` → `TakeDirsDeleted` (A§5.6): after the purge
//! commit is durable, delete exactly `takes/<take>/` of each purged take.
//! Directories are opened with `O_NOFOLLOW` (a symlinked `takes/` or take
//! folder is never followed), only regular files are unlinked (through the
//! directory fd, so nothing outside it can be reached), then the empty
//! folder is removed. Anything else is left alone and reported.

use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use qrec_core::TakeId;

/// Deletes `takes/<take>/` for every take. Returns the deleted paths
/// (project-relative: each file, then `takes/<take>/`) and one message per
/// problem.
pub(crate) fn delete_take_dirs(project: &Path, takes: &[TakeId]) -> (Vec<String>, Vec<String>) {
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    let takes_path = project.join("takes");
    let takes_dir = match open_dir(None, &takes_path) {
        Ok(fd) => fd,
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return (deleted, errors),
        Err(e) => {
            let link = std::fs::symlink_metadata(&takes_path).is_ok_and(|m| m.is_symlink());
            let why = describe_open_error(&e, link);
            errors.extend(takes.iter().map(|t| format!("takes/{t}/: takes/ {why}")));
            return (deleted, errors);
        }
    };
    for take in takes {
        let name = take.as_str();
        if !is_take_name(name) {
            errors.push(format!("{name:?} is not a take folder name; nothing deleted"));
            continue;
        }
        delete_one(&takes_dir, name, &mut deleted, &mut errors);
    }
    (deleted, errors)
}

/// `take-` followed by digits (what `TakeId` produces).
fn is_take_name(name: &str) -> bool {
    name.strip_prefix("take-")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn delete_one(takes_dir: &OwnedFd, name: &str, deleted: &mut Vec<String>, errors: &mut Vec<String>) {
    let rel = format!("takes/{name}/");
    let dir = match open_dir(Some(takes_dir), Path::new(name)) {
        Ok(fd) => fd,
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return, // already gone
        Err(e) => {
            let link = matches!(
                file_type_at(takes_dir, OsStr::new(name)),
                Ok(FileType::Other("a symlink"))
            );
            errors.push(format!("{rel}: {}; left alone", describe_open_error(&e, link)));
            return;
        }
    };
    let entries = match list_dir(&dir) {
        Ok(entries) => entries,
        Err(e) => {
            errors.push(format!("{rel}: can't list it: {e}"));
            return;
        }
    };
    for entry in entries {
        let shown = format!("{rel}{}", entry.to_string_lossy());
        match file_type_at(&dir, &entry) {
            Ok(FileType::Regular) => match unlink_at(&dir, &entry, 0) {
                Ok(()) => deleted.push(shown),
                Err(e) => errors.push(format!("{shown}: can't delete it: {e}")),
            },
            Ok(FileType::Gone) => {}
            Ok(FileType::Other(kind)) => {
                errors.push(format!("{shown} is {kind}, not a regular file; left alone"));
            }
            Err(e) => errors.push(format!("{shown}: can't inspect it: {e}")),
        }
    }
    drop(dir);
    match unlink_at(takes_dir, OsStr::new(name), libc::AT_REMOVEDIR) {
        Ok(()) => deleted.push(rel),
        Err(e) if e.raw_os_error() == Some(libc::ENOTEMPTY) => {
            errors.push(format!("{rel} still has other entries; the folder was kept"));
        }
        Err(e) => errors.push(format!("{rel}: can't remove the folder: {e}")),
    }
}

/// Why a folder couldn't be opened (`O_NOFOLLOW | O_DIRECTORY` reports a
/// symlink as `ELOOP` or `ENOTDIR`, so `symlink` says which it was).
fn describe_open_error(e: &io::Error, symlink: bool) -> String {
    match e.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) if symlink => "is a symlink (not followed)".to_owned(),
        Some(libc::ENOTDIR) => "is not a folder".to_owned(),
        _ => format!("can't be opened: {e}"),
    }
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a name contains a NUL byte"))
}

/// `openat(dir, path, O_DIRECTORY | O_NOFOLLOW | O_RDONLY | O_CLOEXEC)`.
fn open_dir(dir: Option<&OwnedFd>, path: &Path) -> io::Result<OwnedFd> {
    let c = c_name(path.as_os_str())?;
    let base = dir.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
    // SAFETY: `c` is a valid NUL-terminated string; `base` is AT_FDCWD or an open fd.
    let fd = unsafe {
        libc::openat(
            base,
            c.as_ptr(),
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by openat and is owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The names in a directory (without `.` and `..`).
fn list_dir(dir: &OwnedFd) -> io::Result<Vec<std::ffi::OsString>> {
    // fdopendir takes ownership of the fd it is given: hand it a duplicate.
    let dup = File::from(dir.try_clone()?).into_raw_fd();
    // SAFETY: `dup` is an open directory fd we own; fdopendir takes it over.
    let stream = unsafe { libc::fdopendir(dup) };
    if stream.is_null() {
        let e = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so `dup` is still ours to close.
        unsafe { libc::close(dup) };
        return Err(e);
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: `stream` is a valid DIR* until closedir below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: readdir returned a valid dirent whose d_name is NUL-terminated.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(OsStr::from_bytes(bytes).to_os_string());
        }
    }
    // SAFETY: `stream` came from fdopendir and is closed exactly once.
    unsafe { libc::closedir(stream) };
    names.sort();
    Ok(names)
}

enum FileType {
    Regular,
    Gone,
    Other(&'static str),
}

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`.
fn file_type_at(dir: &OwnedFd, name: &OsStr) -> io::Result<FileType> {
    let c = c_name(name)?;
    // SAFETY: an all-zero `stat` is a valid value for fstatat to fill in.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: valid fd, NUL-terminated name and a writable stat buffer.
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOENT) {
            Ok(FileType::Gone)
        } else {
            Err(e)
        };
    }
    Ok(match st.st_mode & libc::S_IFMT {
        libc::S_IFREG => FileType::Regular,
        libc::S_IFDIR => FileType::Other("a folder"),
        libc::S_IFLNK => FileType::Other("a symlink"),
        _ => FileType::Other("a special file"),
    })
}

/// `unlinkat(dir, name, flags)`.
fn unlink_at(dir: &OwnedFd, name: &OsStr, flags: libc::c_int) -> io::Result<()> {
    let c = c_name(name)?;
    // SAFETY: valid fd and NUL-terminated name.
    let rc = unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), flags) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn take(n: u32) -> TakeId {
        TakeId::numbered(n)
    }

    #[test]
    fn deletes_exactly_the_take_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for n in 1..=3 {
            let t = root.join(format!("takes/take-000{n}"));
            fs::create_dir_all(&t).unwrap();
            fs::write(t.join("L1-screen-DP-4.mkv"), "video").unwrap();
            fs::write(t.join("recorder.log"), "log").unwrap();
        }
        let (deleted, errors) = delete_take_dirs(root, &[take(1), take(3)]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            deleted,
            [
                "takes/take-0001/L1-screen-DP-4.mkv",
                "takes/take-0001/recorder.log",
                "takes/take-0001/",
                "takes/take-0003/L1-screen-DP-4.mkv",
                "takes/take-0003/recorder.log",
                "takes/take-0003/",
            ]
        );
        assert!(!root.join("takes/take-0001").exists());
        assert!(root.join("takes/take-0002/L1-screen-DP-4.mkv").exists());
        assert!(!root.join("takes/take-0003").exists());
        // Already gone: nothing to do, no error.
        assert_eq!(delete_take_dirs(root, &[take(1)]), (vec![], vec![]));
        // No takes/ at all.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(delete_take_dirs(empty.path(), &[take(1)]), (vec![], vec![]));
    }

    #[test]
    fn never_follows_symlinks_or_deletes_other_things() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("precious.mkv"), "keep me").unwrap();
        let takes = root.join("takes");
        fs::create_dir_all(takes.join("take-0001/sub")).unwrap();
        fs::write(takes.join("take-0001/A1-mic.mka"), "audio").unwrap();
        fs::write(takes.join("take-0001/sub/nested.txt"), "nested").unwrap();
        symlink(outside.join("precious.mkv"), takes.join("take-0001/link.mkv")).unwrap();
        // A take folder that is a symlink to somewhere else.
        symlink(&outside, takes.join("take-0002")).unwrap();

        let (deleted, errors) = delete_take_dirs(root, &[take(1), take(2)]);
        assert_eq!(deleted, ["takes/take-0001/A1-mic.mka"]);
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(errors[0].contains("link.mkv is a symlink"), "{errors:?}");
        assert!(errors[1].contains("sub is a folder"), "{errors:?}");
        assert!(errors[2].contains("still has other entries"), "{errors:?}");
        assert!(errors[3].contains("take-0002/: is a symlink"), "{errors:?}");
        assert_eq!(fs::read_to_string(outside.join("precious.mkv")).unwrap(), "keep me");
        assert!(takes.join("take-0001/sub/nested.txt").exists());
        assert!(takes.join("take-0001/link.mkv").is_symlink());

        // A symlinked takes/ is never entered.
        let other = tempfile::tempdir().unwrap();
        symlink(&takes, other.path().join("takes")).unwrap();
        let (deleted, errors) = delete_take_dirs(other.path(), &[take(1)]);
        assert!(deleted.is_empty());
        assert!(errors[0].contains("takes/ is a symlink"), "{errors:?}");
        assert!(takes.join("take-0001/sub").exists());
    }

    #[test]
    fn take_names_are_checked() {
        assert!(is_take_name("take-0001") && is_take_name("take-12345"));
        assert!(!is_take_name("take-") && !is_take_name("..") && !is_take_name("take-1/x"));
    }
}
