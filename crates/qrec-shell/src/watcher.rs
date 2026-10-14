//! External-edit watcher (A§8.9): inotify on the project directory
//! (`IN_CLOSE_WRITE|IN_MOVED_TO|IN_CREATE|IN_DELETE`), filtered on
//! `qrec.json`, debounced 200 ms; posts `Msg::DiskChanged`. The session
//! then compares the file's sha256 with what the host last wrote (equal ⇒
//! its own write) and only then parses it. Owner: store.
//!
//! The directory is watched, not the file: editors and the host itself
//! replace `qrec.json` by renaming a temp file over it, which a watch on
//! the old inode would miss. `IN_MODIFY` is ignored (an in-place writer
//! ends with `IN_CLOSE_WRITE`), so a half-written file is rarely read; if
//! it is, the core marks the disk invalid until the next event fixes it.

use std::ffi::OsStr;
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};
use notify::event::{AccessKind, AccessMode, ModifyKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use qrec_core::model::PROJECT_FILE;

use crate::error::ShellError;
use crate::msg::{Inbox, Msg};

/// Quiet time after the last event before `DiskChanged` is posted.
pub const DEBOUNCE: Duration = Duration::from_millis(200);
/// A file that keeps changing is still reported this often.
pub const MAX_DELAY: Duration = Duration::from_secs(1);

/// Watches one project directory until dropped.
pub struct Watcher {
    watcher: Option<RecommendedWatcher>,
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher").finish_non_exhaustive()
    }
}

/// Starts watching `project_dir` for changes of `qrec.json`.
pub fn watch(project_dir: &Path, inbox: Inbox) -> Result<Watcher, ShellError> {
    let (signal_tx, signal_rx) = bounded::<()>(1);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        let relevant = match &event {
            Ok(event) => is_relevant(event),
            // An error (e.g. a queue overflow) may hide a change: re-check.
            Err(_) => true,
        };
        if relevant {
            // Full means a signal is already pending: nothing is lost.
            let _ = signal_tx.try_send(());
        }
    })
    .map_err(|e| ShellError::Internal(format!("can't start the file watcher: {e}")))?;
    watcher
        .watch(project_dir, RecursiveMode::NonRecursive)
        .map_err(|e| ShellError::Internal(format!("can't watch {}: {e}", project_dir.display())))?;
    let (stop_tx, stop_rx) = bounded::<()>(1);
    let thread = thread::Builder::new()
        .name("qrec-watcher".into())
        .spawn(move || debounce(&signal_rx, &stop_rx, &inbox))
        .map_err(|e| ShellError::io("start the watcher thread", e))?;
    Ok(Watcher {
        watcher: Some(watcher),
        stop: stop_tx,
        thread: Some(thread),
    })
}

/// A change of `qrec.json` that may have changed its content.
fn is_relevant(event: &Event) -> bool {
    let kind_matches = match &event.kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write))
        | EventKind::Create(_)
        | EventKind::Remove(_)
        | EventKind::Modify(ModifyKind::Name(_)) => true,
        // Overflow: something may have been missed.
        EventKind::Other => return event.need_rescan(),
        _ => false,
    };
    kind_matches
        && event
            .paths
            .iter()
            .any(|p| p.file_name() == Some(OsStr::new(PROJECT_FILE)))
}

fn debounce(signals: &Receiver<()>, stop: &Receiver<()>, inbox: &Inbox) {
    loop {
        crossbeam_channel::select! {
            recv(signals) -> got => if got.is_err() { return },
            recv(stop) -> _ => return,
        }
        let first = Instant::now();
        loop {
            let left = MAX_DELAY.saturating_sub(first.elapsed());
            if left.is_zero() {
                break;
            }
            crossbeam_channel::select! {
                recv(signals) -> got => if got.is_err() { return },
                recv(stop) -> _ => return,
                default(DEBOUNCE.min(left)) => break,
            }
        }
        if !inbox.send(Msg::DiskChanged) {
            return;
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // Stop inotify first so no event arrives after this.
        drop(self.watcher.take());
        let _ = self.stop.try_send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn next(rx: &crossbeam_channel::Receiver<Msg>, within: Duration) -> bool {
        match rx.recv_timeout(within) {
            Ok(Msg::DiskChanged) => true,
            Ok(other) => panic!("unexpected {other:?}"),
            Err(_) => false,
        }
    }

    #[test]
    fn qrec_json_changes_are_reported_once_per_burst() {
        let tmp = tempfile::tempdir().unwrap();
        let (inbox, rx) = Inbox::channel();
        let watcher = watch(tmp.path(), inbox).unwrap();

        // Other files don't count.
        fs::write(tmp.path().join("notes.txt"), "x").unwrap();
        fs::create_dir(tmp.path().join(".qrec")).unwrap();
        fs::write(tmp.path().join(".qrec/ui.json"), "{}").unwrap();
        assert!(!next(&rx, Duration::from_millis(500)));

        // A burst of in-place writes is one message.
        for i in 0..5 {
            fs::write(tmp.path().join("qrec.json"), format!("{{\"v\":{i}}}")).unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        assert!(next(&rx, Duration::from_secs(3)));
        assert!(!next(&rx, Duration::from_millis(400)), "debounced");

        // Atomic replace (what editors and the host do).
        fs::write(tmp.path().join("qrec.json.tmp"), "{}").unwrap();
        fs::rename(tmp.path().join("qrec.json.tmp"), tmp.path().join("qrec.json")).unwrap();
        assert!(next(&rx, Duration::from_secs(3)));

        fs::remove_file(tmp.path().join("qrec.json")).unwrap();
        assert!(next(&rx, Duration::from_secs(3)));

        drop(watcher);
        fs::write(tmp.path().join("qrec.json"), "{}").unwrap();
        assert!(!next(&rx, Duration::from_millis(400)), "stopped");
    }

    #[test]
    fn a_file_that_keeps_changing_is_still_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let (inbox, rx) = Inbox::channel();
        let _watcher = watch(tmp.path(), inbox).unwrap();
        let started = Instant::now();
        let writer = {
            let path = tmp.path().join("qrec.json");
            thread::spawn(move || {
                while started.elapsed() < Duration::from_millis(1800) {
                    fs::write(&path, "{}").unwrap();
                    thread::sleep(Duration::from_millis(50));
                }
            })
        };
        assert!(next(&rx, Duration::from_millis(1600)), "within MAX_DELAY despite the stream");
        writer.join().unwrap();
    }
}
