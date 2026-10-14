//! Job logs (A§8.7): `.qrec/logs/YYYYMMDD-HHMMSS-<pid>-j<job>-<step>.log`,
//! created with `O_EXCL` (a `-2`, `-3`, … suffix when two logs of the same
//! step start in the same second), the child's stderr appended after a
//! one-line header, and the tail read back for `ExportFailed`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::Local;

/// How many lines of stderr a failure shows (SPEC §13.1).
pub(crate) const TAIL_LINES: usize = 30;

/// How many logs a host keeps (A§8.7).
pub const KEEP_LOGS: usize = 200;

/// The most of a log read back into memory.
const READ_LIMIT: u64 = 8 << 20;

/// `YYYYMMDD-HHMMSS` in local time.
pub(crate) fn stamp() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// One log file. The child's stderr is appended after the header.
#[derive(Debug)]
pub(crate) struct LogFile {
    pub path: PathBuf,
    file: File,
    /// Where the child's output starts.
    body_start: u64,
}

impl LogFile {
    /// Creates `<dir>/<base>.log` (or `<base>-2.log`, …) exclusively and
    /// writes a header naming the command and its directory.
    pub(crate) fn create(dir: &Path, base: &str, argv: &[String], cwd: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let mut n = 1u32;
        let (path, mut file) = loop {
            let name = if n == 1 {
                format!("{base}.log")
            } else {
                format!("{base}-{n}.log")
            };
            let path = dir.join(name);
            // O_APPEND: the child shares this file description (and its
            // offset); reading the tail must never move where it writes.
            match OpenOptions::new()
                .append(true)
                .read(true)
                .create_new(true)
                .custom_flags(libc::O_CLOEXEC)
                .open(&path)
            {
                Ok(file) => break (path, file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
                Err(e) => return Err(e),
            }
        };
        let header = format!(
            "# qrec {}: in {}: {}\n",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            cwd.display(),
            shell_words(argv)
        );
        file.write_all(header.as_bytes())?;
        let body_start = header.len() as u64;
        Ok(Self {
            path,
            file,
            body_start,
        })
    }

    /// The handle a child's stderr is appended to.
    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    /// Appends a note from the runner (not from the child).
    pub(crate) fn note(&self, text: &str) {
        let mut file = &self.file;
        let _ = writeln!(file, "# qrec: {text}");
    }

    /// The child's output (after the header), at most the last 8 MiB.
    pub(crate) fn body(&self) -> String {
        let mut file = &self.file;
        let len = file.seek(SeekFrom::End(0)).unwrap_or(0);
        let start = self.body_start.max(len.saturating_sub(READ_LIMIT));
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(start)).is_ok() {
            let _ = file.take(READ_LIMIT).read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// The last `n` non-empty lines of the child's output.
    pub(crate) fn tail(&self, n: usize) -> Vec<String> {
        tail_lines(&self.body(), n)
    }

    pub(crate) fn display(&self) -> String {
        self.path.display().to_string()
    }
}

/// The last `n` non-empty lines of `text` (`\r` progress lines split too).
pub(crate) fn tail_lines(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text
        .split(['\n', '\r'])
        .map(str::trim_end)
        .filter(|l| !l.is_empty() && !l.starts_with("# qrec"))
        .collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| (*l).to_owned())
        .collect()
}

/// `argv` as a shell would read it (for the log header).
pub(crate) fn shell_words(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty()
                && a.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_./:=,+@%".contains(&b))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Deletes all but the newest `keep` `*.log` files in `dir` (A§8.7: at host
/// start). Names start with their stamp, so name order is age order.
/// Returns how many were deleted.
pub fn prune_logs(dir: &Path, keep: usize) -> io::Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut logs: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let is_file = entry.file_type().is_ok_and(|t| t.is_file());
        if is_file && name.ends_with(".log") {
            logs.push((name, entry.path()));
        }
    }
    logs.sort();
    let excess = logs.len().saturating_sub(keep);
    let mut deleted = 0;
    for (_, path) in logs.into_iter().take(excess) {
        if fs::remove_file(&path).is_ok() {
            deleted += 1;
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_are_exclusive_and_keep_a_header() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        let argv: Vec<String> = ["ffmpeg", "-i", "file:a b.mkv"].map(String::from).to_vec();
        let a = LogFile::create(&logs, "20261014-120000-1-j3-seg0", &argv, dir.path()).unwrap();
        let b = LogFile::create(&logs, "20261014-120000-1-j3-seg0", &argv, dir.path()).unwrap();
        assert_eq!(a.path.file_name().unwrap(), "20261014-120000-1-j3-seg0.log");
        assert_eq!(
            b.path.file_name().unwrap(),
            "20261014-120000-1-j3-seg0-2.log"
        );
        let header = fs::read_to_string(&a.path).unwrap();
        assert!(header.starts_with("# qrec "), "{header}");
        assert!(header.contains("ffmpeg -i 'file:a b.mkv'"), "{header}");

        // The child's output is what follows the header.
        let mut out = a.file();
        writeln!(out, "line 1\nline 2\n\nline 3").unwrap();
        a.note("a note from the runner");
        assert!(a.body().starts_with("line 1"));
        assert_eq!(a.tail(2), ["line 2", "line 3"]);
        assert_eq!(a.tail(30).len(), 3);
        assert!(
            fs::read_to_string(&a.path)
                .unwrap()
                .contains("# qrec: a note")
        );
    }

    #[test]
    fn tails_and_words() {
        assert_eq!(tail_lines("a\r\nb\rc\n\n", 2), ["b", "c"]);
        assert!(tail_lines("", 3).is_empty());
        assert_eq!(
            shell_words(&["ffmpeg".into(), "it's".into(), "".into(), "-c:v".into()]),
            r"ffmpeg 'it'\''s' '' -c:v"
        );
    }

    #[test]
    fn prune_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fs::write(dir.path().join(format!("2026101{i}-120000-1-x.log")), "").unwrap();
        }
        fs::write(dir.path().join("notes.txt"), "").unwrap();
        assert_eq!(prune_logs(dir.path(), 2).unwrap(), 3);
        let mut left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "20261013-120000-1-x.log",
                "20261014-120000-1-x.log",
                "notes.txt"
            ]
        );
        assert_eq!(prune_logs(&dir.path().join("missing"), 2).unwrap(), 0);
    }
}
