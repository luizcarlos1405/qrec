//! File watchers (inotify through `notify`). Owner: store.
//!
//! **External edits** (A§8.9): inotify on the project directory
//! (`IN_CLOSE_WRITE|IN_MOVED_TO|IN_CREATE|IN_DELETE`), filtered on
//! `qrec.json`, debounced 200 ms; posts `Msg::DiskChanged`. The session
//! then compares the file's sha256 with what the host last wrote (equal ⇒
//! its own write) and only then parses it.
//!
//! The directory is watched, not the file: editors and the host itself
//! replace `qrec.json` by renaming a temp file over it, which a watch on
//! the old inode would miss. `IN_MODIFY` is ignored (an in-place writer
//! ends with `IN_CLOSE_WRITE`), so a half-written file is rarely read; if
//! it is, the core marks the disk invalid until the next event fixes it.
//!
//! **The user's settings** (docs/GUI.md G§3.14, WP10): [`watch_settings`]
//! watches the config dir (`$XDG_CONFIG_HOME/qrec/`) for `config.toml` and
//! `keys.toml` the same way (`qrec config set` renames a temp file over
//! `config.toml`), for a window that follows them live. A burst of events
//! is one change: the sink hears [`SettingsEvent::Pending`] at its first
//! event and [`SettingsEvent::Settled`] once it was quiet for [`DEBOUNCE`]
//! (at most [`MAX_DELAY`] after it began), with both files as they are
//! then ([`Settings::read`]). Reads (`IN_OPEN`, `IN_CLOSE_NOWRITE`) are
//! never changes, so the watcher's own reads, and the Session's, don't
//! report themselves. A config dir that doesn't exist yet (no `qrec config
//! set` ever ran) is watched from the nearest folder above it, and the
//! watch moves into it once it is created (or back up when it goes).

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use notify::event::{AccessKind, AccessMode, ModifyKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use qrec_core::config::UserConfig;
use qrec_core::keymap::Keymap;
use qrec_core::model::PROJECT_FILE;

use crate::error::ShellError;
use crate::facts::ConfigFile;
use crate::keys_file;
use crate::msg::{Inbox, Msg};

/// Quiet time after the last event before a change is reported.
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

/// An event kind that may have changed a watched file's content: a write
/// closed, a file created, removed or renamed. `None` for what never does
/// (opens, reads, attributes, in-place writes still open); `Some(true)` for
/// an overflow, which may hide anything.
fn changes_content(event: &Event) -> Option<bool> {
    match &event.kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write))
        | EventKind::Create(_)
        | EventKind::Remove(_)
        | EventKind::Modify(ModifyKind::Name(_)) => Some(false),
        // Overflow: something may have been missed.
        EventKind::Other if event.need_rescan() => Some(true),
        _ => None,
    }
}

/// A change of `qrec.json` that may have changed its content.
fn is_relevant(event: &Event) -> bool {
    match changes_content(event) {
        None => false,
        Some(true) => true,
        Some(false) => event
            .paths
            .iter()
            .any(|p| p.file_name() == Some(OsStr::new(PROJECT_FILE))),
    }
}

fn debounce(signals: &Receiver<()>, stop: &Receiver<()>, inbox: &Inbox) {
    loop {
        crossbeam_channel::select! {
            recv(signals) -> got => if got.is_err() { return },
            recv(stop) -> _ => return,
        }
        if !settle(signals, stop) {
            return;
        }
        if !inbox.send(Msg::DiskChanged) {
            return;
        }
    }
}

/// After a burst's first event: waits until `signals` was quiet for
/// [`DEBOUNCE`], at most [`MAX_DELAY`] in all. `false` when stopped.
fn settle(signals: &Receiver<()>, stop: &Receiver<()>) -> bool {
    let first = Instant::now();
    loop {
        let left = MAX_DELAY.saturating_sub(first.elapsed());
        if left.is_zero() {
            return true;
        }
        crossbeam_channel::select! {
            recv(signals) -> got => if got.is_err() { return false },
            recv(stop) -> _ => return false,
            default(DEBOUNCE.min(left)) => return true,
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

// ------------------------------------------------------------ settings

/// The user's settings file (`qrec config`, A§9.8).
pub const CONFIG_FILE: &str = "config.toml";
/// The keybinding overrides (A§9.7).
pub const KEYS_FILE: &str = "keys.toml";
/// How long dropping a [`SettingsWatcher`] waits for its thread (it only
/// ever waits on its channels or reads two small files).
pub const STOP_WAIT: Duration = Duration::from_secs(2);

/// The user's settings files as a window applies them (G§3.14): the config
/// (the defaults when `config.toml` can't be used) and the keymap, with what
/// is wrong with each.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub config: UserConfig,
    /// Why `config.toml` can't be used (`config` holds the defaults then).
    /// Taken from the [`ConfigFile`] the window shares with its Session, so
    /// the "config.toml is invalid" warning comes once per change of the
    /// file between them (A§9.8).
    pub config_problem: Option<String>,
    /// The defaults with `keys.toml`'s overrides.
    pub keymap: Keymap,
    /// What is wrong with `keys.toml`, as startup words it
    /// ([`keys_file::load`]): why it can't be read, then the conflicts and
    /// the errors (unknown ids, bad keys) as the core reports them.
    pub keymap_problems: Vec<String>,
}

impl Settings {
    /// Both files now: `config.toml` through `config` (bounded reads; its
    /// problem taken), `keys.toml` at `keys`.
    pub fn read(config: &ConfigFile, keys: &Path) -> Settings {
        let parsed = config.get();
        let config_problem = config.take_problem();
        let (keymap, keymap_problems) = keys_file::load(keys);
        Settings {
            config: parsed,
            config_problem,
            keymap,
            keymap_problems,
        }
    }
}

/// What the settings watcher tells its sink, per burst of changes:
/// `Pending`, then `Settled`. Its first pair comes as soon as the watch is
/// up (a change between the window's own read and the watch is not lost).
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsEvent {
    /// A change began: it is being debounced (a window counts it as work
    /// in progress until `Settled`).
    Pending,
    /// The change settled: both files as they are now.
    Settled(Box<Settings>),
}

/// Watches the config dir until dropped ([`watch_settings`]).
pub struct SettingsWatcher {
    stop: Sender<()>,
    done: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SettingsWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsWatcher").finish_non_exhaustive()
    }
}

/// Starts watching `config_dir` for `config.toml` and `keys.toml` (read
/// through `config`, the window's [`ConfigFile`] of `config_dir/config.toml`)
/// and telling `sink`, from a thread of its own (`qrec-settings`): inotify
/// is set up there too, so the caller (the GTK thread) never waits on the
/// file system. A watch that can't be set up is logged, and the settings
/// stay as they were read.
pub fn watch_settings(
    config_dir: &Path,
    config: ConfigFile,
    sink: impl Fn(SettingsEvent) + Send + 'static,
) -> Result<SettingsWatcher, ShellError> {
    let dir = config_dir.to_path_buf();
    let (stop_tx, stop_rx) = bounded::<()>(1);
    let (done_tx, done_rx) = bounded::<()>(1);
    let thread = thread::Builder::new()
        .name("qrec-settings".into())
        .spawn(move || {
            follow_settings(&dir, &config, &stop_rx, &sink);
            let _ = done_tx.send(());
        })
        .map_err(|e| ShellError::io("start the settings watcher thread", e))?;
    Ok(SettingsWatcher {
        stop: stop_tx,
        done: done_rx,
        thread: Some(thread),
    })
}

/// The settings watcher's thread: arms inotify, reports once, then once
/// per burst until stopped.
fn follow_settings(
    dir: &Path,
    config: &ConfigFile,
    stop: &Receiver<()>,
    sink: &dyn Fn(SettingsEvent),
) {
    let (signal_tx, signals) = bounded::<()>(1);
    let target = dir.to_path_buf();
    let handler = move |event: notify::Result<Event>| {
        let relevant = match &event {
            Ok(event) => settings_relevant(event, &target),
            // An error (a queue overflow) may hide a change: read again.
            Err(_) => true,
        };
        if relevant {
            // Full means a signal is already pending: nothing is lost.
            let _ = signal_tx.try_send(());
        }
    };
    let mut watcher = match notify::recommended_watcher(handler) {
        Ok(watcher) => watcher,
        Err(e) => {
            tracing::warn!("the settings won't follow changes: can't start the file watcher: {e}");
            let _ = stop.recv();
            return;
        }
    };
    let keys = dir.join(KEYS_FILE);
    let mut armed = arm(&mut watcher, dir, None);
    let report = |sink: &dyn Fn(SettingsEvent)| {
        sink(SettingsEvent::Settled(Box::new(Settings::read(
            config, &keys,
        ))));
    };
    sink(SettingsEvent::Pending);
    report(sink);
    loop {
        crossbeam_channel::select! {
            recv(signals) -> got => if got.is_err() { return },
            recv(stop) -> _ => return,
        }
        sink(SettingsEvent::Pending);
        if !settle(&signals, stop) {
            return;
        }
        // The config dir may have come or gone: the watch follows it before
        // the files are read, so no later change is missed.
        armed = arm(&mut watcher, dir, armed);
        report(sink);
    }
}

/// The folder a watch is on, and its inode (a config dir deleted and made
/// again under the same name is a new folder to watch).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Armed {
    path: PathBuf,
    inode: u64,
}

/// `dir`, or the nearest folder above it that exists.
fn nearest_dir(dir: &Path) -> Option<(PathBuf, u64)> {
    dir.ancestors().find_map(|at| {
        fs::metadata(at)
            .ok()
            .filter(fs::Metadata::is_dir)
            .map(|meta| (at.to_path_buf(), meta.ino()))
    })
}

/// Watches `dir`, or the nearest folder above it while it doesn't exist;
/// `armed` is what is watched now. The watch moves until it stays put (a
/// folder made on the way while it moved is watched too).
fn arm(watcher: &mut RecommendedWatcher, dir: &Path, mut armed: Option<Armed>) -> Option<Armed> {
    for _ in 0..8 {
        let Some((path, inode)) = nearest_dir(dir) else {
            tracing::warn!(
                "the settings won't follow changes: no folder of {} exists",
                dir.display()
            );
            return armed;
        };
        let want = Armed { path, inode };
        if armed.as_ref() == Some(&want) {
            return armed;
        }
        if let Some(old) = armed.take() {
            // Already gone when its folder was deleted.
            let _ = watcher.unwatch(&old.path);
        }
        match watcher.watch(&want.path, RecursiveMode::NonRecursive) {
            Ok(()) => {
                if want.path != dir {
                    tracing::info!(
                        "settings: {} doesn't exist yet; watching {}",
                        dir.display(),
                        want.path.display()
                    );
                }
                armed = Some(want);
            }
            Err(e) => {
                tracing::warn!(
                    "the settings won't follow changes: can't watch {}: {e}",
                    want.path.display()
                );
                return None;
            }
        }
    }
    armed
}

/// Whether `event` (in the config dir `dir`, or a folder above it) may have
/// changed the settings: `config.toml` or `keys.toml` written, created,
/// removed or renamed, or the config dir or a folder on its way created,
/// removed or renamed (the watch moves).
fn settings_relevant(event: &Event, dir: &Path) -> bool {
    match changes_content(event) {
        None => false,
        Some(true) => true,
        Some(false) => event.paths.iter().any(|path| {
            let settings_file = path.parent() == Some(dir)
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name == CONFIG_FILE || name == KEYS_FILE);
            settings_file || dir.starts_with(path)
        }),
    }
}

impl Drop for SettingsWatcher {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
        // Bounded: the thread only waits on its channels or reads two small
        // files; past that it is left to end on its own.
        match self.done.recv_timeout(STOP_WAIT) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(thread) = self.thread.take() {
                    let _ = thread.join();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                tracing::warn!(
                    "the settings watcher didn't stop within {} s",
                    STOP_WAIT.as_secs()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use notify::event::{CreateKind, DataChange, RemoveKind, RenameMode};
    use qrec_core::config::Theme;

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
        fs::rename(
            tmp.path().join("qrec.json.tmp"),
            tmp.path().join("qrec.json"),
        )
        .unwrap();
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
        assert!(
            next(&rx, Duration::from_millis(1600)),
            "within MAX_DELAY despite the stream"
        );
        writer.join().unwrap();
    }

    // ------------------------------------------------------- settings

    /// The settings watcher on `dir` (its `config.toml` through a
    /// [`ConfigFile`]), its events on a channel.
    fn settings_watcher(dir: &Path) -> (SettingsWatcher, Receiver<SettingsEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let config = ConfigFile::new(dir.join(CONFIG_FILE));
        let watcher = watch_settings(dir, config, move |event| {
            let _ = tx.send(event);
        })
        .unwrap();
        (watcher, rx)
    }

    /// The next `Settled` within `within` (a `Pending` first), or `None`.
    fn settled(rx: &Receiver<SettingsEvent>, within: Duration) -> Option<Settings> {
        let deadline = Instant::now() + within;
        let mut pending = false;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(SettingsEvent::Pending) => pending = true,
                Ok(SettingsEvent::Settled(settings)) => {
                    assert!(pending, "Settled without Pending first");
                    return Some(*settings);
                }
                Err(_) => return None,
            }
        }
    }

    /// Writes `text` to `dir/name` as `qrec config set` does: a temp file
    /// renamed over it.
    fn write_atomic(dir: &Path, name: &str, text: &str) {
        let tmp = dir.join(format!(".{name}.tmp-test"));
        fs::write(&tmp, text).unwrap();
        fs::rename(&tmp, dir.join(name)).unwrap();
    }

    /// One report as soon as the watch is up; a burst of writes is one
    /// change, read once it settled; other files, temp files and reads
    /// never count.
    #[test]
    fn settings_changes_are_reported_once_per_burst() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("qrec");
        fs::create_dir(&dir).unwrap();
        let (watcher, rx) = settings_watcher(&dir);
        let first = settled(&rx, Duration::from_secs(3)).expect("the first report");
        assert_eq!(first.config, UserConfig::default());
        assert_eq!(first.config_problem, None);
        assert!(first.keymap_problems.is_empty());

        // Other files, a temp file left alone and reads don't count.
        fs::write(dir.join("notes.txt"), "x").unwrap();
        fs::write(dir.join(".config.toml.tmp-1"), "theme = \"ogre-mode\"\n").unwrap();
        fs::write(tmp.path().join("config.toml"), "theme = \"ogre-mode\"\n").unwrap();
        assert_eq!(settled(&rx, Duration::from_millis(500)), None);

        // A burst: one change, read after the last write.
        for theme in ["swamp-day", "dragons-keep", "ogre-mode", "swamp-day"] {
            write_atomic(&dir, CONFIG_FILE, &format!("theme = \"{theme}\"\n"));
            thread::sleep(Duration::from_millis(30));
        }
        let burst = settled(&rx, Duration::from_secs(3)).expect("the burst");
        assert_eq!(burst.config.theme, Theme::SwampDay);
        assert_eq!(settled(&rx, Duration::from_millis(500)), None, "debounced");

        // Reading the files (the Session, `qrec config get`) is no change.
        let _ = fs::read_to_string(dir.join(CONFIG_FILE)).unwrap();
        assert_eq!(settled(&rx, Duration::from_millis(500)), None);

        // keys.toml, written in place.
        fs::write(dir.join(KEYS_FILE), "[bindings]\n\"bin.toggle\" = \"F8\"\n").unwrap();
        let keys = settled(&rx, Duration::from_secs(3)).expect("keys.toml");
        assert_eq!(keys.config.theme, Theme::SwampDay, "both files are read");
        let bin = keys.keymap.binding("bin.toggle").unwrap();
        assert_eq!(bin.keys[0].to_string(), "F8");

        // Removed: the defaults.
        fs::remove_file(dir.join(CONFIG_FILE)).unwrap();
        let removed = settled(&rx, Duration::from_secs(3)).expect("the removal");
        assert_eq!(removed.config, UserConfig::default());

        drop(watcher);
        write_atomic(&dir, CONFIG_FILE, "theme = \"ogre-mode\"\n");
        assert_eq!(settled(&rx, Duration::from_millis(500)), None, "stopped");
    }

    /// Before any `qrec config set` the config dir doesn't exist: the
    /// folder above is watched until it is made, then the dir itself; one
    /// deleted and made again is followed too.
    #[test]
    fn a_config_dir_made_later_is_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("config/qrec");
        let (_watcher, rx) = settings_watcher(&dir);
        assert_eq!(
            settled(&rx, Duration::from_secs(3)).map(|s| s.config),
            Some(UserConfig::default())
        );
        // `qrec config set theme dragons-keep` with no config dir yet.
        fs::create_dir_all(&dir).unwrap();
        write_atomic(&dir, CONFIG_FILE, "theme = \"dragons-keep\"\n");
        let made = settled(&rx, Duration::from_secs(3)).expect("the new dir");
        assert_eq!(made.config.theme, Theme::DragonsKeep);
        // Watched from inside now.
        write_atomic(&dir, CONFIG_FILE, "theme = \"ogre-mode\"\n");
        let inside = settled(&rx, Duration::from_secs(3)).expect("a change inside");
        assert_eq!(inside.config.theme, Theme::OgreMode);

        // Deleted: the defaults; made again: followed again.
        fs::remove_dir_all(tmp.path().join("config")).unwrap();
        let gone = settled(&rx, Duration::from_secs(3)).expect("the deletion");
        assert_eq!(gone.config, UserConfig::default());
        fs::create_dir_all(&dir).unwrap();
        write_atomic(&dir, CONFIG_FILE, "theme = \"swamp-day\"\n");
        let again = settled(&rx, Duration::from_secs(3)).expect("made again");
        assert_eq!(again.config.theme, Theme::SwampDay);
        write_atomic(&dir, CONFIG_FILE, "[ui]\nscale = 1.5\n");
        let later = settled(&rx, Duration::from_secs(3)).expect("a change in it");
        assert_eq!(later.config.ui.scale, 1.5);
    }

    /// A stream of writes is still reported within `MAX_DELAY`.
    #[test]
    fn a_settings_file_that_keeps_changing_is_still_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let (_watcher, rx) = settings_watcher(tmp.path());
        settled(&rx, Duration::from_secs(3)).expect("the first report");
        let started = Instant::now();
        let writer = {
            let path = tmp.path().join(KEYS_FILE);
            thread::spawn(move || {
                while started.elapsed() < Duration::from_millis(1800) {
                    fs::write(&path, "").unwrap();
                    thread::sleep(Duration::from_millis(50));
                }
            })
        };
        assert!(
            settled(&rx, Duration::from_millis(1600)).is_some(),
            "within MAX_DELAY despite the stream"
        );
        writer.join().unwrap();
    }

    /// Dropping the watcher ends its thread at once, and nothing is
    /// reported after.
    #[test]
    fn a_dropped_settings_watcher_stops_in_time() {
        let tmp = tempfile::tempdir().unwrap();
        let (watcher, rx) = settings_watcher(tmp.path());
        settled(&rx, Duration::from_secs(3)).expect("the first report");
        // Mid-burst.
        write_atomic(tmp.path(), CONFIG_FILE, "theme = \"swamp-day\"\n");
        let asked = Instant::now();
        drop(watcher);
        assert!(
            asked.elapsed() < Duration::from_millis(500),
            "{:?}",
            asked.elapsed()
        );
        while let Ok(event) = rx.try_recv() {
            assert_eq!(event, SettingsEvent::Pending, "no read after the drop");
        }
    }

    /// What `Settings::read` makes of the files: the config or its
    /// defaults with the problem (taken once, as the Session would take
    /// it), the keymap with its problems as startup words them.
    #[test]
    fn settings_are_read_as_the_window_applies_them() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ConfigFile::new(tmp.path().join(CONFIG_FILE));
        let keys = tmp.path().join(KEYS_FILE);
        let none = Settings::read(&config, &keys);
        assert_eq!(none.config, UserConfig::default());
        assert_eq!(none.config_problem, None);
        assert_eq!(none.keymap, qrec_core::keymap::resolve(""));
        assert!(none.keymap_problems.is_empty());

        fs::write(
            tmp.path().join(CONFIG_FILE),
            "theme = \"ogre-mode\"\n[ui]\nscale = 1.25\nreduced_motion = true\n",
        )
        .unwrap();
        let good = Settings::read(&config, &keys);
        assert_eq!(good.config.theme, Theme::OgreMode);
        assert_eq!(good.config.ui.scale, 1.25);
        assert!(good.config.ui.reduced_motion);
        assert_eq!(good.config_problem, None);

        // A typo: the defaults, and why, once (shared with the Session).
        fs::write(tmp.path().join(CONFIG_FILE), "theme = \"ogre-mod\"\n").unwrap();
        let bad = Settings::read(&config, &keys);
        assert_eq!(bad.config, UserConfig::default());
        let problem = bad.config_problem.expect("the problem");
        assert!(problem.contains("ogre-mod"), "{problem}");
        assert!(problem.ends_with("using the default settings"), "{problem}");
        assert_eq!(Settings::read(&config, &keys).config_problem, None, "once");
        assert_eq!(config.take_problem(), None, "the Session's share is taken");

        // A conflict and an unknown id, as the core reports them.
        fs::write(
            &keys,
            "[bindings]\n\"autocut.run\" = \"x\"\n\"nope.nothing\" = \"q\"\n",
        )
        .unwrap();
        let problems = Settings::read(&config, &keys).keymap_problems;
        assert_eq!(
            problems,
            [
                "x is bound to clip.remove and autocut.run in global; neither runs until \
                 keys.toml changes one",
                "keys.toml: unknown binding `nope.nothing` (`qrec keys` lists the ids)",
            ]
        );

        // One that can't be read: the default keys, and why.
        fs::remove_file(&keys).unwrap();
        fs::create_dir(&keys).unwrap();
        let unreadable = Settings::read(&config, &keys);
        assert_eq!(unreadable.keymap, qrec_core::keymap::resolve(""));
        assert_eq!(unreadable.keymap_problems.len(), 1);
        let why = &unreadable.keymap_problems[0];
        assert!(why.starts_with("can't read "), "{why}");
        assert!(why.ends_with("; using the default keys"), "{why}");
    }

    fn event(kind: EventKind, path: &Path) -> Event {
        Event::new(kind).add_path(path.to_path_buf())
    }

    /// Which events are changes of the settings: writes, creations,
    /// removals and renames of the two files, and of the config dir or a
    /// folder above it; never reads, opens, attributes or other files.
    #[test]
    fn settings_events_are_told_apart() {
        let dir = Path::new("/home/u/.config/qrec");
        let config = dir.join(CONFIG_FILE);
        let keys = dir.join(KEYS_FILE);
        let closed = EventKind::Access(AccessKind::Close(AccessMode::Write));
        let renamed = EventKind::Modify(ModifyKind::Name(RenameMode::To));
        let created = EventKind::Create(CreateKind::File);
        let removed = EventKind::Remove(RemoveKind::File);
        for (kind, path) in [
            (closed, &config),
            (renamed, &config),
            (created, &keys),
            (removed, &keys),
            (EventKind::Create(CreateKind::Folder), &dir.to_path_buf()),
            // The dir itself deleted or moved (its own watch).
            (EventKind::Remove(RemoveKind::Folder), &dir.to_path_buf()),
            (
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                &dir.to_path_buf(),
            ),
            // A folder on the way, made while the one above is watched.
            (
                EventKind::Create(CreateKind::Folder),
                &PathBuf::from("/home/u/.config"),
            ),
        ] {
            assert!(
                settings_relevant(&event(kind, path), dir),
                "{kind:?} {path:?}"
            );
        }
        let not = [
            (
                EventKind::Access(AccessKind::Open(AccessMode::Any)),
                config.clone(),
            ),
            (
                EventKind::Access(AccessKind::Close(AccessMode::Read)),
                config.clone(),
            ),
            (
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
                config.clone(),
            ),
            (
                EventKind::Modify(ModifyKind::Metadata(notify::event::MetadataKind::Any)),
                keys.clone(),
            ),
            (closed, dir.join(".config.toml.tmp-42")),
            (closed, dir.join("notes.txt")),
            (created, PathBuf::from("/home/u/.config/gtk-4.0")),
            (closed, PathBuf::from("/home/u/.config/config.toml")),
            (
                closed,
                PathBuf::from("/home/u/.config/qrec/sub/config.toml"),
            ),
        ];
        for (kind, path) in not {
            assert!(
                !settings_relevant(&event(kind, &path), dir),
                "{kind:?} {path:?}"
            );
        }
        // An overflow may hide anything.
        let overflow = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert!(settings_relevant(&overflow, dir));
        assert!(!settings_relevant(&Event::new(EventKind::Other), dir));
    }
}
