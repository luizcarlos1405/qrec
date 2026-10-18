//! Gathers `Facts` for a `FactsNeeded` before `apply`/`on_event` (A§4.2):
//! the user config, the folder name, audio devices (`pactl`), screens and
//! cameras, output path checks, take files and dirs, file sizes and
//! mtimes. Exactly what was asked is gathered (no `pactl` for a clip
//! split). Under `QREC_CAPTURE_BACKEND=fake` device facts come from the
//! fake inventory (A§8.11). Owner: session.
//!
//! **Config.** `config.toml` is read on every gather (cached by size and
//! mtime) so `qrec config set` reaches a running host, with the core's
//! `config::parse`. A config that can't be used falls back to the
//! defaults, and its problem waits in the [`ConfigFile`] until the Session
//! reports it (a warning toast, once per change of the file): reading the
//! config never swallows it.
//!
//! **Devices.** [`Sources`] is the seam to device discovery: the fake
//! inventory, or [`SystemSources`] (audio through `pactl --format=json`;
//! screens and cameras through hooks the media owner's `sources` module
//! fills in), or [`SharedSources`], the GUI's cache that a worker refreshes
//! (GUI H-5), so the session thread never waits for discovery once it has
//! run.
//!
//! **The player.** A relative seek while the program plays goes from the
//! GUI's Program player's frame ([`PlayerPosition`], GUI G§3.10): the GUI
//! hands its Session one that reads the player's position atomically.

use std::collections::{HashSet, VecDeque};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use qrec_core::TakeId;
use qrec_core::config::UserConfig;
use qrec_core::ctx::{
    AudioDeviceInfo, CameraInfo, Facts, FactsNeeded, FileFact, FileId, OutFacts, ScreenInfo,
};
use qrec_core::model::PROJECT_FILE;
use qrec_core::protocol::{Toast, ToastLevel};

use crate::capture::fake::{FakeInventoryKind, fake_inventory_of};
use crate::media::{is_temp_name, take_contents};
use crate::paths::{NAME_MAX, ProjectPaths, path_text};
use crate::proc::{self, Limit, PACTL_TIMEOUT, Spec};
use crate::sources::{self, SourceInventory};

// ------------------------------------------------------------------ sources

/// What audio devices exist and which are the defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioInventory {
    /// Every source: mics, monitors and the other inputs.
    pub devices: Vec<AudioDeviceInfo>,
    /// The default input, an `alsa_input.*` source.
    pub default_mic: Option<String>,
    /// The default sink's monitor.
    pub default_monitor: Option<String>,
}

/// The audio facts of a discovery: every [`Sources`] gives them this way
/// (one `pactl` parser, `sources::parse_pactl_audio`), so a GUI host and a
/// CLI or headless host label and check the same `--source` alike.
impl From<&SourceInventory> for AudioInventory {
    fn from(inventory: &SourceInventory) -> Self {
        AudioInventory {
            devices: inventory.audio_devices(),
            default_mic: inventory.default_mic.clone(),
            default_monitor: inventory.default_monitor.clone(),
        }
    }
}

/// Device discovery behind the facts.
pub trait Sources: Send {
    fn audio(&mut self) -> Result<AudioInventory, String>;
    fn screens(&mut self) -> Result<Vec<ScreenInfo>, String>;
    fn cameras(&mut self) -> Result<Vec<CameraInfo>, String>;
}

/// The fake backend's devices (A§8.11, `capture::fake::fake_inventory_of`):
/// screens `FAKE-1`, `FAKE-2` (1280×720, side by side), camera
/// `/dev/fake0`, mic `alsa_input.fake`, monitor `alsa_output.fake.monitor`;
/// the kind says which names and sizes they carry (`QREC_FAKE_INVENTORY`,
/// which only [`SessionEnv::from_process`](crate::session::SessionEnv::from_process)
/// reads: an in-process test gets the plain inventory unless it asks).
#[derive(Debug, Clone, Copy, Default)]
pub struct FakeSources(pub FakeInventoryKind);

impl Sources for FakeSources {
    fn audio(&mut self) -> Result<AudioInventory, String> {
        Ok(AudioInventory::from(&SourceInventory::fake_of(self.0)))
    }

    fn screens(&mut self) -> Result<Vec<ScreenInfo>, String> {
        Ok(fake_inventory_of(self.0).screens)
    }

    fn cameras(&mut self) -> Result<Vec<CameraInfo>, String> {
        Ok(fake_inventory_of(self.0).cameras)
    }
}

/// A discovery function for screens or cameras.
pub type Discover<T> = fn() -> Result<Vec<T>, String>;

/// The machine's devices: audio through `pactl --format=json` (3 s
/// timeout, stderr to `log_dir`); screens and cameras through the hooks
/// (`None` turns that discovery off).
pub struct SystemSources {
    pub log_dir: Option<PathBuf>,
    pub screens: Option<Discover<ScreenInfo>>,
    pub cameras: Option<Discover<CameraInfo>>,
}

impl SystemSources {
    /// Screens and cameras through the `sources` module (`wl_output`, then
    /// `niri msg`; GStreamer's device monitor, then sysfs).
    pub fn new(log_dir: Option<PathBuf>) -> Self {
        Self {
            log_dir,
            screens: Some(crate::sources::discover_screens),
            cameras: Some(crate::sources::discover_cameras),
        }
    }

    /// One log for the `pactl` runs of one discovery.
    fn log(&self) -> Option<fs::File> {
        let dir = self.log_dir.as_ref()?;
        let stamp = proc::log_stamp(chrono::Local::now());
        proc::create_log(dir, &format!("{stamp}-{}-pactl", std::process::id()))
            .ok()
            .map(|(file, _)| file)
    }

    fn pactl(log: Option<&fs::File>, args: &[&str]) -> Result<String, String> {
        let mut spec = Spec::new("pactl", Limit::Timeout(PACTL_TIMEOUT))
            .args(args)
            .capture_stdout(8 << 20);
        if let Some(file) = log.and_then(|f| f.try_clone().ok()) {
            spec = spec.stderr_to(file);
        }
        let out = spec.run().map_err(|e| e.to_string())?;
        if !out.success() {
            return Err(out.describe());
        }
        Ok(out.stdout_text().into_owned())
    }
}

impl Sources for SystemSources {
    fn audio(&mut self) -> Result<AudioInventory, String> {
        let log = self.log();
        let info = Self::pactl(log.as_ref(), &["--format=json", "info"])?;
        let sources = Self::pactl(log.as_ref(), &["--format=json", "list", "sources"])?;
        audio_facts(&sources, &info)
    }

    fn screens(&mut self) -> Result<Vec<ScreenInfo>, String> {
        match self.screens {
            Some(discover) => discover(),
            None => Err("screen discovery isn't available in this build".to_owned()),
        }
    }

    fn cameras(&mut self) -> Result<Vec<CameraInfo>, String> {
        match self.cameras {
            Some(discover) => discover(),
            None => Err("camera discovery isn't available in this build".to_owned()),
        }
    }
}

/// Device facts from a cache (GUI H-5): `LayerAdd`, `AudioAdd` and a take's
/// start read what the last discovery found, so the session thread never
/// waits for `pactl`, Wayland or the camera monitor once one has run.
///
/// Clones share the cache. The GUI keeps one to [`SharedSources::refresh`]
/// on a worker (at startup, when an Add menu opens; no hotplug watcher yet) and gives
/// another to the Session (`SessionConfig::sources`). Until a discovery
/// finished, the first caller discovers once, on its own thread; one that
/// comes while a refresh runs waits for that refresh instead of starting
/// another (each tool has its timeout, A§8.12).
#[derive(Clone)]
pub struct SharedSources {
    shared: Arc<SharedCache>,
}

struct SharedCache {
    discover: Box<dyn Fn() -> SourceInventory + Send + Sync>,
    /// What the last discovery found.
    cached: Mutex<Option<Arc<SourceInventory>>>,
    /// Held while discovering.
    discovering: Mutex<()>,
}

impl std::fmt::Debug for SharedSources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedSources")
            .field("cached", &self.cached().is_some())
            .finish_non_exhaustive()
    }
}

impl Default for SharedSources {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedSources {
    /// The machine's sources ([`SourceInventory::discover`]: the fake
    /// inventory under `QREC_CAPTURE_BACKEND=fake`); nothing cached yet.
    pub fn new() -> Self {
        Self::with_discovery(|| SourceInventory::discover(sources::Discover::ALL))
    }

    /// With another discovery (tests).
    pub fn with_discovery(discover: impl Fn() -> SourceInventory + Send + Sync + 'static) -> Self {
        Self {
            shared: Arc::new(SharedCache {
                discover: Box::new(discover),
                cached: Mutex::new(None),
                discovering: Mutex::new(()),
            }),
        }
    }

    /// Discovers every source on the caller's thread (up to seconds: never
    /// the session's or the GTK thread) and caches what it found.
    pub fn refresh(&self) -> Arc<SourceInventory> {
        let _discovering = self
            .shared
            .discovering
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.discover_now()
    }

    /// What the last discovery found, if one finished.
    pub fn cached(&self) -> Option<Arc<SourceInventory>> {
        self.shared
            .cached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The cached inventory. An empty cache discovers once, here, or waits
    /// for the discovery under way.
    pub fn inventory(&self) -> Arc<SourceInventory> {
        if let Some(inventory) = self.cached() {
            return inventory;
        }
        let _discovering = self
            .shared
            .discovering
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // A refresh may have filled it while this waited.
        match self.cached() {
            Some(inventory) => inventory,
            None => self.discover_now(),
        }
    }

    /// Discovers and caches (the caller holds `discovering`).
    fn discover_now(&self) -> Arc<SourceInventory> {
        let inventory = Arc::new((self.shared.discover)());
        for warning in &inventory.warnings {
            tracing::debug!("sources: {warning}");
        }
        *self
            .shared
            .cached
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&inventory));
        inventory
    }

    /// Why `kind` (`screens`, `cameras`, `audio`) came back empty, when the
    /// discovery said so (`SourceInventory::warnings` are `"<kind>: why"`).
    fn failure(inventory: &SourceInventory, kind: &str, empty: bool) -> Option<String> {
        let prefix = format!("{kind}: ");
        empty
            .then(|| {
                inventory
                    .warnings
                    .iter()
                    .find_map(|w| w.strip_prefix(&prefix).map(str::to_owned))
            })
            .flatten()
    }
}

impl Sources for SharedSources {
    fn audio(&mut self) -> Result<AudioInventory, String> {
        let inventory = self.inventory();
        let empty = inventory.audio_devices().is_empty();
        if let Some(why) = Self::failure(&inventory, "audio", empty) {
            return Err(why);
        }
        Ok(AudioInventory::from(&*inventory))
    }

    fn screens(&mut self) -> Result<Vec<ScreenInfo>, String> {
        let inventory = self.inventory();
        match Self::failure(&inventory, "screens", inventory.screens.is_empty()) {
            Some(why) => Err(why),
            None => Ok(inventory.screens.clone()),
        }
    }

    fn cameras(&mut self) -> Result<Vec<CameraInfo>, String> {
        let inventory = self.inventory();
        match Self::failure(&inventory, "cameras", inventory.cameras.is_empty()) {
            Some(why) => Err(why),
            None => Ok(inventory.cameras.clone()),
        }
    }
}

/// The audio facts of what `pactl --format=json list sources` and `pactl
/// --format=json info` printed ([`SystemSources`]): the shared parser
/// (`sources::parse_pactl_audio`) through a [`SourceInventory`], exactly as
/// [`SharedSources`] gives them. Mics are `alsa_input.*` only (SPEC §5.1),
/// monitors `*.monitor`, other inputs neither; a `(null)` description falls
/// back to the device's properties.
pub fn audio_facts(sources_json: &str, info_json: &str) -> Result<AudioInventory, String> {
    let mut inventory = SourceInventory::default();
    inventory.set_audio(sources::parse_pactl_audio(sources_json, info_json)?);
    Ok(AudioInventory::from(&inventory))
}

// ------------------------------------------------------------------- config

/// `config.toml`, re-read when its size, mtime, inode or change time
/// changes (a temp file renamed over it, as `qrec config set` does, is a
/// new inode whatever its size and mtime).
///
/// A file that can't be used (bad TOML, an unknown key, a wrong type, a
/// value out of range, or one that can't be read) gives the defaults, and
/// its problem waits for [`ConfigFile::take_problem`]: reading the config
/// ([`ConfigFile::get`]) never consumes it, so whoever reports it (the
/// Session, A§4.2) does, once per change of the file, however many reads
/// came first. Clones share all of it: the Sessions one process opens with
/// clones report a file once between them.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    path: PathBuf,
    read: Arc<Mutex<ConfigRead>>,
}

/// What the last read of a [`ConfigFile`] found.
#[derive(Debug, Default)]
struct ConfigRead {
    /// What `stat` said then; `None` before the first read.
    seen: Option<Seen>,
    config: UserConfig,
    /// Why that file can't be used, until someone takes it to report it.
    problem: Option<String>,
}

/// What `stat` says about `config.toml`, compared to know when to read it again.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    /// No file: the defaults, and nothing to say about it.
    Missing,
    /// `stat` failed otherwise (a config folder that can't be searched).
    Unreadable(String),
    /// A file of this size, mtime, inode and change time (s, ns).
    File(u64, SystemTime, u64, (i64, i64)),
}

impl ConfigFile {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            read: Arc::default(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> MutexGuard<'_, ConfigRead> {
        self.read.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The config to use now: the file's, or the defaults when there is
    /// none or it can't be used (then [`ConfigFile::take_problem`] says why).
    pub fn get(&self) -> UserConfig {
        let seen = match fs::metadata(&self.path) {
            Ok(m) => Seen::File(
                m.len(),
                m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                m.ino(),
                (m.ctime(), m.ctime_nsec()),
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Seen::Missing,
            Err(e) => Seen::Unreadable(e.to_string()),
        };
        let mut read = self.lock();
        if read.seen.as_ref() == Some(&seen) {
            return read.config.clone();
        }
        let parsed = match &seen {
            Seen::Missing => Ok(UserConfig::default()),
            Seen::Unreadable(e) => Err(format!("can't read {}: {e}", self.path.display())),
            // At most 1 MiB, a regular file only (never a FIFO's wait).
            Seen::File(..) => crate::config_file::read_text(&self.path).and_then(|text| {
                text.map_or_else(|| Ok(UserConfig::default()), |t| parse_config(&t))
            }),
        };
        read.seen = Some(seen);
        match parsed {
            Ok(config) => {
                read.config = config;
                read.problem = None;
            }
            Err(problem) => {
                read.config = UserConfig::default();
                read.problem = Some(format!("{problem}; using the default settings"));
            }
        }
        read.config.clone()
    }

    /// Why the file [`ConfigFile::get`] last read can't be used, once per
    /// change of the file: the caller reports it. `None` once taken, and
    /// for a file that is fine (again) or missing.
    pub fn take_problem(&self) -> Option<String> {
        self.lock().problem.take()
    }
}

/// The title of the warning about a `config.toml` that can't be used.
pub const CONFIG_INVALID: &str = "config.toml is invalid";

/// The warning toast for a [`ConfigFile::take_problem`] (it says why, and
/// that the defaults are used).
pub fn config_problem_toast(problem: String) -> Toast {
    Toast::new(ToastLevel::Warning, CONFIG_INVALID).with_body(problem)
}

/// Reads `config.toml` text into a `UserConfig` with the core's reader
/// (`config::parse`: missing settings take their defaults; unknown keys,
/// wrong types and out-of-range values are errors).
pub fn parse_config(text: &str) -> Result<UserConfig, String> {
    qrec_core::config::parse(text).map_err(|e| e.message)
}

// -------------------------------------------------------------------- facts

/// Where the GUI's Program player is while the program plays in it, in
/// timeline ns, `None` otherwise (`Facts::player_ns`, GUI G§3.10 step 3):
/// a relative seek then goes from its frame, not from the core's playhead,
/// which the window's sync moves once a second. The GUI keeps it
/// (`media::program::ProgramPosition`). It runs on the session thread, so
/// it returns at once: it reads atomics, never waits for the GTK thread.
pub type PlayerPosition = Arc<dyn Fn() -> Option<u64> + Send + Sync>;

/// Gathers facts for one project.
pub struct FactsGatherer {
    /// The canonical project folder.
    paths: ProjectPaths,
    config: ConfigFile,
    /// `keys.toml`, next to `config.toml`.
    keys_file: PathBuf,
    sources: Box<dyn Sources>,
    /// The GUI's player, in a GUI host.
    player: Option<PlayerPosition>,
}

impl std::fmt::Debug for FactsGatherer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactsGatherer")
            .field("root", &self.paths.root())
            .finish_non_exhaustive()
    }
}

/// Facts plus what went wrong getting them (for the log; the core turns
/// missing facts into errors of its own). A `config.toml` that can't be
/// used isn't among the warnings: [`FactsGatherer::take_config_problem`].
#[derive(Debug, Clone, PartialEq)]
pub struct Gathered {
    pub facts: Facts,
    pub warnings: Vec<String>,
}

impl FactsGatherer {
    /// `paths.root()` must be canonical (output paths are compared with
    /// it). `config` may be shared with other Sessions ([`ConfigFile`]).
    pub fn new(paths: ProjectPaths, config: ConfigFile, sources: Box<dyn Sources>) -> Self {
        Self {
            paths,
            keys_file: config.path().with_file_name("keys.toml"),
            config,
            sources,
            player: None,
        }
    }

    /// Reads `player` for `FactsNeeded::player` (the GUI's Session).
    pub fn with_player(mut self, player: Option<PlayerPosition>) -> Self {
        self.player = player;
        self
    }

    /// The config alone (every dispatch needs it).
    pub fn config(&self) -> UserConfig {
        self.config.get()
    }

    /// Why the config last read can't be used, once per change of the
    /// file ([`ConfigFile::take_problem`]): the caller reports it.
    pub fn take_config_problem(&self) -> Option<String> {
        self.config.take_problem()
    }

    /// The text of `keys.toml` (`Facts::keys_toml`, for `Query::Keys`):
    /// empty when there is none, or when it can't be read (a warning says
    /// why); the core reports what's wrong with one it got.
    pub fn keys_toml(&self) -> String {
        crate::keys_file::read(&self.keys_file).unwrap_or_else(|why| {
            tracing::warn!("{why}");
            String::new()
        })
    }

    /// Gathers exactly what `need` asks for, plus the config.
    pub fn gather(&mut self, need: &FactsNeeded) -> Gathered {
        let mut warnings = Vec::new();
        let mut facts = Facts {
            config: self.config.get(),
            ..Facts::default()
        };
        if need.folder_name {
            facts.folder_name = self
                .paths
                .root()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
        }
        if need.audio_devices {
            match self.sources.audio() {
                Ok(audio) => {
                    let find = |name: &Option<String>| {
                        name.as_ref()
                            .and_then(|n| audio.devices.iter().find(|d| &d.name == n))
                            .cloned()
                    };
                    facts.default_mic = find(&audio.default_mic);
                    facts.default_monitor = find(&audio.default_monitor);
                    facts.audio_devices = audio.devices;
                }
                Err(e) => warnings.push(format!("audio devices: {e}")),
            }
        }
        if need.video_sources {
            match self.sources.screens() {
                Ok(screens) => facts.screens = screens,
                Err(e) => warnings.push(format!("screens: {e}")),
            }
            match self.sources.cameras() {
                Ok(cameras) => facts.cameras = cameras,
                Err(e) => warnings.push(format!("cameras: {e}")),
            }
        }
        if let Some(out) = &need.out {
            facts.out = Some(out_facts(self.paths.root(), out));
        }
        for take in &need.take_files {
            let (files, left) = take_files(&self.paths, take);
            facts.take_files.insert(take.clone(), files);
            if !left.is_empty() {
                facts.take_left.insert(take.clone(), left);
            }
        }
        if need.take_dirs {
            facts.min_take_number = min_take_number(&self.paths);
        }
        for file in &need.files {
            match file_fact(&self.paths, file) {
                Ok(fact) => {
                    facts.files.insert(file.clone(), fact);
                }
                Err(e) => warnings.push(format!("{file}: {e}")),
            }
        }
        if need.player {
            facts.player_ns = self.player.as_ref().and_then(|position| position());
        }
        Gathered { facts, warnings }
    }
}

/// 1 + the highest `takes/take-N` folder (1 without any).
pub fn min_take_number(paths: &ProjectPaths) -> u32 {
    crate::store::take_dirs(paths)
        .iter()
        .map(|t| t.number().saturating_add(1))
        .max()
        .unwrap_or(1)
}

/// What a purge of `take` deletes (its regular files, with sizes) and what
/// it would leave on disk, found the way the purge walks the take folder
/// (`media::take_contents`: `O_NOFOLLOW`, regular files only), so the
/// confirmation lists exactly what is deleted (A§5.6). A symlinked
/// `takes/` or take folder is never followed: its files aren't listed, and
/// it is what is left.
pub fn take_files(paths: &ProjectPaths, take: &TakeId) -> (Vec<FileFact>, Vec<String>) {
    let contents = take_contents(paths.root(), take);
    let files = contents
        .files
        .into_iter()
        .map(|(name, bytes, mtime_ns)| FileFact {
            path: format!("{}/{}", take_rel(take), name.to_string_lossy()),
            bytes,
            mtime_ns,
        })
        .collect();
    (files, contents.left)
}

fn take_rel(take: &TakeId) -> String {
    format!("takes/{take}")
}

/// Size and mtime of a project file (following symlinks, like ffmpeg does).
pub fn file_fact(paths: &ProjectPaths, relative: &str) -> io::Result<FileFact> {
    let path = paths
        .resolve(relative)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.message))?;
    let meta = fs::metadata(path)?;
    Ok(FileFact {
        path: relative.to_owned(),
        bytes: meta.len(),
        mtime_ns: mtime_ns(&meta),
    })
}

fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.mtime_nsec())
}

/// What is at a hand-picked output path (A§5.9): its absolute form,
/// whether something is there (its size: what an overwrite loses; its
/// identity: the one file a confirmed overwrite may replace), and why it
/// can't be used ([`why_forbidden`]). A relative path is taken relative to
/// the project folder.
///
/// The path means what the OS says it means: `abs` keeps its `..`
/// components (only `.` goes), the job writes exactly that path, and the
/// checks run where the kernel resolves it. `link/..` is the parent of the
/// link's *target*, never lexically the folder holding `link`.
pub fn out_facts(project_root: &Path, out: &str) -> OutFacts {
    let given = Path::new(out);
    let joined = if given.is_absolute() {
        given.to_path_buf()
    } else {
        project_root.join(given)
    };
    // `components` drops `.` (and repeated or trailing slashes), not `..`;
    // `out` still says whether the path names a folder (`folder_marker`).
    let abs: PathBuf = joined.components().collect();
    let meta = fs::symlink_metadata(&abs).ok();
    let mut shown = path_text(&abs);
    // Such a path is refused; it is shown the way it names the folder.
    match folder_marker(out) {
        Some("/") if !shown.ends_with('/') => shown.push('/'),
        Some(".") => shown.push_str("/."),
        _ => {}
    }
    OutFacts {
        abs: shown,
        exists: meta.is_some(),
        // What a confirmed overwrite replaces: the entry at `abs` itself
        // (a symlink there is replaced, not its target), like
        // `OutputAppeared` reports it.
        bytes: meta.as_ref().map(fs::Metadata::len),
        id: meta.as_ref().map(|m| FileId {
            dev: m.dev(),
            ino: m.ino(),
        }),
        forbidden: why_forbidden(project_root, out, &abs),
    }
}

/// Why a job may not write `name` in a folder it holds open, if it may not
/// (A§5.9, checked again when the job writes, A§8.8). The folder is known
/// by `chain`, its identity and those of the folders above it (walked
/// through `..` from its descriptor: where it really is, whatever a path to
/// it leads to by now), and by `real`, its path now. Refused: the folder
/// is, or lies in, `takes/` or `.qrec/` (by identity, by path, or as one of
/// their folders found through symlinks or bind mounts); `name` in the
/// project folder is `qrec.json`, `takes` or `.qrec`; `entry`, what the
/// final rename would replace there, is the project file or a file a
/// symlink in `takes/` or `.qrec/` leads to.
pub(crate) fn why_landing_forbidden(
    project_root: &Path,
    chain: &[FileId],
    real: &Path,
    name: &OsStr,
    entry: Option<FileId>,
) -> Option<String> {
    let chain: Vec<(u64, u64)> = chain.iter().map(|id| (id.dev, id.ino)).collect();
    Protected::find(project_root).why_landing(&chain, real, name, entry.map(|id| (id.dev, id.ino)))
}

/// Why `abs` (`out` made absolute) can't be an output, if it can't (A§5.9):
/// its name is over 255 bytes; what the job touches ([`touched`]) is
/// `qrec.json`, in `takes/` or `.qrec/` ([`Protected`]), the project folder
/// or a directory; its name has the shape of Qrec's temp files, which host
/// start deletes (A§8.7); `out` names a folder; or the path isn't UTF-8
/// (the job is given it as text).
fn why_forbidden(project_root: &Path, out: &str, abs: &Path) -> Option<String> {
    // Caught here, before rendering, instead of by the final rename.
    let name_len = abs.file_name().map_or(0, |n| n.len());
    if name_len > NAME_MAX {
        return Some(format!(
            "its file name is {name_len} bytes long, and file names can have at most {NAME_MAX}"
        ));
    }
    let protected = Protected::find(project_root);
    if let Some(why) = touched(abs).iter().find_map(|place| protected.why(place)) {
        return Some(why);
    }
    // The name the result gets (a symlink there is replaced): one in the
    // temp shape would be swept as a stale temp (A§8.7).
    if abs.file_name().is_some_and(is_temp_name) {
        return Some(
            "its name has the shape of Qrec's temp files \
             (.<name>.qrec-part-<number>-<number>), which Qrec deletes"
                .to_owned(),
        );
    }
    if let Some(marker) = folder_marker(out) {
        return Some(format!(
            "it ends with '{marker}', so it names a folder, not a file"
        ));
    }
    if abs.to_str().is_none() {
        return Some("its path isn't valid UTF-8".to_owned());
    }
    None
}

/// How `out` names a folder, if it does: a trailing `/`, or a last part
/// that is `.` or `..`. The OS never creates a file there (creating
/// `dir/` fails with EISDIR), and neither does Qrec: without this check the
/// trailing `/`, which `components` drops, would become a file `dir`.
fn folder_marker(out: &str) -> Option<&'static str> {
    match out.rsplit('/').next() {
        Some("") => Some("/"),
        Some(".") => Some("."),
        Some("..") => Some(".."),
        _ => None,
    }
}

/// What a job writing `abs` touches, as the kernel resolves the path: the
/// entry the final rename replaces or creates (its folder resolved, its own
/// name kept; the temp file is written in that folder too) and, when that
/// entry is a symlink, where the link leads (the link is what gets
/// replaced, but a link into `takes/` is refused all the same).
fn touched(abs: &Path) -> Vec<PathBuf> {
    let entry = match (abs.parent(), abs.file_name()) {
        (Some(dir), Some(name)) => resolve_existing(dir).join(name),
        _ => resolve_existing(abs),
    };
    let is_link = fs::symlink_metadata(&entry).is_ok_and(|m| m.file_type().is_symlink());
    let target = is_link.then(|| link_target(&entry));
    std::iter::once(entry).chain(target).collect()
}

/// A file's identity: device and inode, symlinks followed.
fn identity(path: &Path) -> Option<(u64, u64)> {
    fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// How deep [`Area::find`] looks for folders and symlinks: Qrec's own
/// deepest folder is `.qrec/cache/segments/` (depth 2).
const PROTECTED_DEPTH: usize = 4;

/// The most entries [`Area::find`] looks at in one area.
const PROTECTED_ENTRIES: usize = 100_000;

/// What an output may not touch (A§5.9): `qrec.json`, and `takes/` and
/// `.qrec/` with everything in them. Each is known by name under the
/// project folder (even before it exists) and by identity (`st_dev`,
/// `st_ino`), and so is every folder found in `takes/` and `.qrec/` (a few
/// levels deep, symlinks followed) and every file a symlink in them leads
/// to. So a symlinked `takes/`, take folder, media file or `.qrec/cache`
/// (media moved to another disk), or a bind mount of one of their folders,
/// still guards what it holds, through the link or straight at its target.
#[derive(Debug)]
struct Protected {
    root: PathBuf,
    /// `qrec.json`, and where it leads when it is a symlink.
    project_file: Vec<Place>,
    /// `takes/`, then `.qrec/`.
    areas: [Area; 2],
}

/// A protected folder or file: where it is (resolved as far as it exists)
/// and its identity, when it exists.
#[derive(Debug)]
struct Place {
    path: PathBuf,
    id: Option<(u64, u64)>,
}

/// `takes/` or `.qrec/`: the folders in it (an output may not land in one)
/// and the files its symlinks lead to (an output may not replace one). A
/// regular file in a protected folder needs no entry of its own: its
/// folder guards it.
#[derive(Debug)]
struct Area {
    why: &'static str,
    dirs: Vec<Place>,
    files: Vec<Place>,
}

impl Protected {
    fn find(root: &Path) -> Self {
        let file = root.join(PROJECT_FILE);
        let mut project_file = vec![Place {
            id: identity(&file),
            path: file.clone(),
        }];
        if fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink()) {
            let target = link_target(&file);
            project_file.push(Place {
                id: identity(&target),
                path: target,
            });
        }
        Self {
            root: root.to_path_buf(),
            project_file,
            areas: [
                Area::find(
                    &root.join("takes"),
                    "it is inside takes/, where recorded media lives",
                ),
                Area::find(&root.join(".qrec"), "it is inside .qrec/, Qrec's own files"),
            ],
        }
    }

    /// Why an output may not touch `place` (a path as the kernel resolves
    /// it), if it may not.
    fn why(&self, place: &Path) -> Option<String> {
        // The place and each existing folder above it.
        let chain: Vec<(u64, u64)> = place.ancestors().filter_map(identity).collect();
        let own = identity(place);
        let is = |p: &Place| place == p.path || (p.id.is_some() && own == p.id);
        let within =
            |p: &Place| place.starts_with(&p.path) || p.id.is_some_and(|id| chain.contains(&id));
        if self.project_file.iter().any(is) {
            return Some("that is the project file, qrec.json".to_owned());
        }
        if let Some(area) = self
            .areas
            .iter()
            .find(|area| area.dirs.iter().any(within) || area.files.iter().any(is))
        {
            return Some(area.why.to_owned());
        }
        if place == self.root {
            Some("it is the project folder".to_owned())
        } else if fs::metadata(place).is_ok_and(|m| m.is_dir()) {
            Some("it is a directory".to_owned())
        } else {
            None
        }
    }

    /// [`why_landing_forbidden`]: `chain` holds the open folder's identity
    /// and those above it, `real` is its path now, `entry` the identity of
    /// what the final rename would replace in it.
    fn why_landing(
        &self,
        chain: &[(u64, u64)],
        real: &Path,
        name: &OsStr,
        entry: Option<(u64, u64)>,
    ) -> Option<String> {
        // In the project folder (however it is reached) a name means what
        // it means there: `qrec.json`, `takes`, `.qrec`.
        let in_root = real == self.root
            || chain
                .first()
                .is_some_and(|id| identity(&self.root) == Some(*id));
        let place = if in_root {
            self.root.join(name)
        } else {
            real.join(name)
        };
        let is = |p: &Place| place == p.path || (entry.is_some() && p.id == entry);
        if self.project_file.iter().any(is) {
            return Some("that is the project file, qrec.json".to_owned());
        }
        let within =
            |p: &Place| place.starts_with(&p.path) || p.id.is_some_and(|id| chain.contains(&id));
        self.areas
            .iter()
            .find(|area| area.dirs.iter().any(within) || area.files.iter().any(is))
            .map(|area| area.why.to_owned())
    }
}

impl Area {
    /// `top` (`takes/` or `.qrec/`) by name and by identity, then what is
    /// in it: every folder (real or through a symlink, each looked into
    /// once, [`PROTECTED_DEPTH`] levels deep) and every file a symlink leads
    /// to. A dangling link is kept by where it points.
    fn find(top: &Path, why: &'static str) -> Self {
        let mut area = Area {
            why,
            dirs: vec![Place {
                path: top.to_path_buf(),
                id: identity(top),
            }],
            files: Vec::new(),
        };
        let resolved = link_target(top);
        if resolved != top {
            area.dirs.push(Place {
                id: identity(&resolved),
                path: resolved.clone(),
            });
        }
        if !fs::metadata(&resolved).is_ok_and(|m| m.is_dir()) {
            return area;
        }
        let mut seen: HashSet<(u64, u64)> = identity(&resolved).into_iter().collect();
        let mut queue = VecDeque::from([(resolved, 0)]);
        let mut budget = PROTECTED_ENTRIES;
        while let Some((dir, depth)) = queue.pop_front() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if budget == 0 {
                    tracing::warn!(
                        "{} holds over {PROTECTED_ENTRIES} entries; outputs were checked against those",
                        top.display()
                    );
                    return area;
                }
                budget -= 1;
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = if kind.is_symlink() {
                    link_target(&entry.path())
                } else if kind.is_dir() {
                    entry.path()
                } else {
                    continue;
                };
                match fs::metadata(&path) {
                    Ok(meta) if meta.is_dir() => {
                        let id = (meta.dev(), meta.ino());
                        if seen.insert(id) {
                            if depth + 1 < PROTECTED_DEPTH {
                                queue.push_back((path.clone(), depth + 1));
                            }
                            area.dirs.push(Place { path, id: Some(id) });
                        }
                    }
                    Ok(meta) => area.files.push(Place {
                        path,
                        id: Some((meta.dev(), meta.ino())),
                    }),
                    Err(_) => area.dirs.push(Place { path, id: None }),
                }
            }
        }
        area
    }
}

/// Where the symlink at `path` leads, resolved as far as it exists (a
/// dangling link still says where it points), hop by hop and at most 40
/// hops, like the kernel. A path that isn't a symlink is just resolved.
fn link_target(path: &Path) -> PathBuf {
    let mut at = path.to_path_buf();
    for _ in 0..40 {
        if let Ok(full) = fs::canonicalize(&at) {
            return full;
        }
        let Ok(target) = fs::read_link(&at) else {
            break;
        };
        let dir = at
            .parent()
            .map_or_else(|| PathBuf::from("/"), resolve_existing);
        at = resolve_existing(&dir.join(target));
    }
    resolve_existing(&at)
}

/// Where the OS finds `path`: its longest existing prefix resolved by the
/// kernel (symlinks followed, and a `..` after a symlink going to the
/// target's parent), then the components that don't exist yet. A `..` among
/// those can't be resolved (the OS fails to open such a path); it is taken
/// lexically, which keeps the checks on the safe side.
fn resolve_existing(path: &Path) -> PathBuf {
    if let Ok(full) = fs::canonicalize(path) {
        return full;
    }
    let parts: Vec<Component<'_>> = path.components().collect();
    let Some((split, base)) = (1..parts.len()).rev().find_map(|n| {
        let prefix: PathBuf = parts[..n].iter().collect();
        fs::canonicalize(prefix).ok().map(|base| (n, base))
    }) else {
        return path.to_path_buf();
    };
    parts[split..].iter().fold(base, |mut out, part| {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use qrec_core::Fps;
    use qrec_core::config::Theme;

    use super::*;
    use crate::capture::fake::{FAKE_MIC, FAKE_MONITOR};

    #[test]
    fn config_files() {
        assert_eq!(parse_config("").unwrap(), UserConfig::default());
        let cfg = parse_config(
            "theme = \"ogre-mode\"\n[canvas]\nwidth = 1280\nheight = 720\nfps = 25\n[encoder]\nvaapi_device = \"/dev/dri/renderD129\"\n[autocut]\nthreshold_db = -40\n",
        )
        .unwrap();
        assert_eq!(cfg.theme, Theme::OgreMode);
        assert_eq!(
            (cfg.canvas.width, cfg.canvas.height, cfg.canvas.fps),
            (1280, 720, Fps::new(25, 1))
        );
        assert_eq!(
            cfg.encoder.vaapi_device.as_deref(),
            Some("/dev/dri/renderD129")
        );
        assert_eq!(cfg.autocut.threshold_db, -40.0);
        let err = parse_config("[canvas]\nfsp = 25\n").unwrap_err();
        assert!(err.contains("fsp"), "{err}");
        assert!(parse_config("theme = 3").is_err());
        assert!(
            parse_config("[recording]\ncountdown_secs = 99\n").is_err(),
            "values are range-checked"
        );

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let file = ConfigFile::new(path.clone());
        assert_eq!(file.get(), UserConfig::default(), "no file: defaults");
        assert_eq!(file.take_problem(), None);
        fs::write(&path, "[canvas]\nfps = 60\n").unwrap();
        assert_eq!(file.get().canvas.fps, Fps::new(60, 1));
        assert_eq!(file.take_problem(), None);
        fs::write(&path, "[canvas]\nfps = \"sixty\"\n").unwrap();
        assert_eq!(file.get(), UserConfig::default());
        assert_eq!(file.get(), UserConfig::default(), "read again: cached");
        let problem = file.take_problem().unwrap();
        assert!(problem.ends_with("using the default settings"), "{problem}");
        assert_eq!(file.take_problem(), None, "taken once per change");
        assert_eq!(file.get(), UserConfig::default());
        assert_eq!(
            file.take_problem(),
            None,
            "the same file isn't reported again"
        );
    }

    /// Reading the config never consumes its problem: it waits for whoever
    /// reports it (the Session), once per change of the file, and clones
    /// share it (the Sessions of one process report a file once).
    #[test]
    fn a_config_problem_waits_for_its_reporter() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        // A valid [canvas] and one misspelled key: the whole file is unusable.
        fs::write(
            &path,
            "[canvas]\nwidth = 1280\nheight = 720\nfps = \"25\"\n\n[autocut]\ntreshold_db = -40\n",
        )
        .unwrap();
        let file = ConfigFile::new(path.clone());
        let other = file.clone();
        for _ in 0..3 {
            assert_eq!(file.get(), UserConfig::default());
            assert_eq!(other.get(), UserConfig::default());
        }
        let problem = other.take_problem().unwrap();
        assert!(problem.contains("treshold_db"), "{problem}");
        assert!(
            problem.ends_with("; using the default settings"),
            "{problem}"
        );
        assert_eq!(file.take_problem(), None, "clones share the report");

        // A fix before anyone reported it: nothing left to say.
        fs::write(&path, "[canvas]\nwidth = 99\n").unwrap();
        assert_eq!(file.get(), UserConfig::default(), "out of range");
        fs::write(&path, "[canvas]\nwidth = 1280\nheight = 720\n").unwrap();
        assert_eq!(file.get().canvas.width, 1280);
        assert_eq!(file.take_problem(), None);
        // Broken again (another size, so another stamp): reported again.
        fs::write(&path, "[canvas]\nwidth = 99\nheight = 720\n").unwrap();
        assert_eq!(file.get(), UserConfig::default());
        let problem = file.take_problem().unwrap();
        assert!(problem.contains("width"), "{problem}");
        // Gone: the defaults, and nothing to report.
        fs::remove_file(&path).unwrap();
        assert_eq!(file.get(), UserConfig::default());
        assert_eq!(file.take_problem(), None);

        // A config that can't be read at all is reported too.
        fs::create_dir(&path).unwrap();
        assert_eq!(file.get(), UserConfig::default());
        let problem = file.take_problem().unwrap();
        assert!(problem.starts_with("can't read "), "{problem}");
        // So is one in a folder that can't be searched: that isn't "no file".
        let locked = tmp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let hidden = ConfigFile::new(locked.join("config.toml"));
        let enforced =
            fs::metadata(hidden.path()).is_err_and(|e| e.kind() == io::ErrorKind::PermissionDenied);
        assert_eq!(hidden.get(), UserConfig::default());
        let problem = hidden.take_problem();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if enforced {
            let problem = problem.unwrap();
            assert!(problem.starts_with("can't read "), "{problem}");
        }
    }

    /// A temp file renamed over `config.toml` (`qrec config set`, an editor)
    /// is read again even with the old one's size and mtime: the window
    /// follows the file live (WP10) and two quick `config set`s can match
    /// both (`swamp-day` and `ogre-mode` are as long).
    #[test]
    fn a_config_replaced_with_the_same_size_and_mtime_is_read_again() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, "theme = \"swamp-day\"\n").unwrap();
        let file = ConfigFile::new(path.clone());
        assert_eq!(file.get().theme, Theme::SwampDay);
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        let next = tmp.path().join(".config.toml.tmp-1");
        fs::write(&next, "theme = \"ogre-mode\"\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&next)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        fs::rename(&next, &path).unwrap();
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(
            (meta.len(), meta.modified().unwrap()),
            (20, mtime),
            "the same size and mtime"
        );
        assert_eq!(file.get().theme, Theme::OgreMode);
    }

    /// What `pactl` printed on the dev machine, trimmed: a mic and a monitor
    /// whose descriptions it can't encode ("(null)"), a Bluetooth headset
    /// and an HDMI monitor.
    const PACTL_SOURCES: &str = r#"[
        {"name":"alsa_output.hdmi.monitor","description":"Monitor of HDMI","monitor_source":"alsa_output.hdmi","properties":{}},
        {"name":"alsa_input.laptop.mic","description":"Laptop mic","monitor_source":"","properties":{}},
        {"name":"alsa_output.usb-yeti.analog-stereo.monitor","description":"(null)","monitor_source":"alsa_output.usb-yeti.analog-stereo","properties":{"device.description":"Yeti Nano"}},
        {"name":"alsa_input.usb-yeti.analog-stereo","description":"(null)","monitor_source":"","properties":{"device.description":"Yeti Nano"}},
        {"name":"bluez_input.00_11","description":"Headset","monitor_source":"","properties":{}}
    ]"#;
    const PACTL_INFO: &str = r#"{"default_sink_name":"alsa_output.usb-yeti.analog-stereo","default_source_name":"alsa_input.usb-yeti.analog-stereo"}"#;

    /// Every host reads audio facts with one parser: the GUI's cached
    /// `SharedSources` and the CLI's and headless hosts' `SystemSources`
    /// give equal facts for what `pactl` printed. So a `--source` labels and
    /// checks alike whichever host serves it: a "(null)" description becomes
    /// the device's name ("Yeti Nano", not "(null)"), and a Bluetooth input
    /// stays known (its description is the track's label) though it is
    /// never offered as a mic.
    #[test]
    fn every_host_gets_the_same_audio_facts() {
        let system = audio_facts(PACTL_SOURCES, PACTL_INFO).unwrap();
        let mut shared = SharedSources::with_discovery(|| {
            let mut inventory = SourceInventory::default();
            inventory.set_audio(sources::parse_pactl_audio(PACTL_SOURCES, PACTL_INFO).unwrap());
            inventory
        });
        assert_eq!(shared.audio().unwrap(), system);

        let described = |name: &str| {
            system
                .devices
                .iter()
                .find(|d| d.name == name)
                .map(|d| (d.description.as_str(), d.is_monitor))
        };
        assert_eq!(
            described("alsa_input.usb-yeti.analog-stereo"),
            Some(("Yeti Nano", false))
        );
        assert_eq!(
            described("alsa_output.usb-yeti.analog-stereo.monitor"),
            Some(("Monitor of Yeti Nano", true))
        );
        assert_eq!(described("bluez_input.00_11"), Some(("Headset", false)));
        assert_eq!(system.devices.len(), 5);
        assert_eq!(
            system.default_mic.as_deref(),
            Some("alsa_input.usb-yeti.analog-stereo")
        );
        assert_eq!(
            system.default_monitor.as_deref(),
            Some("alsa_output.usb-yeti.analog-stereo.monitor")
        );
        // Never offered or listed as a mic (SPEC §5.1).
        let listed = shared.inventory();
        assert!(
            listed
                .mics
                .iter()
                .all(|m| m.name.starts_with("alsa_input."))
        );
        assert!(listed.to_json().get("otherInputs").is_none());

        // A monitor as the default source isn't a mic; a missing sink
        // monitor falls back to the first monitor.
        let info =
            r#"{"default_sink_name":"gone","default_source_name":"alsa_output.hdmi.monitor"}"#;
        let inv = audio_facts(PACTL_SOURCES, info).unwrap();
        assert_eq!(inv.default_mic.as_deref(), Some("alsa_input.laptop.mic"));
        assert_eq!(
            inv.default_monitor.as_deref(),
            Some("alsa_output.hdmi.monitor")
        );
        assert!(audio_facts("{}", "{}").is_err());
    }

    /// A [`SharedSources`] whose discovery counts its runs and returns the
    /// fake inventory, its screen descriptions numbered by run.
    fn counting_sources() -> (SharedSources, Arc<Mutex<Vec<std::thread::ThreadId>>>) {
        let runs: Arc<Mutex<Vec<std::thread::ThreadId>>> = Arc::default();
        let seen = Arc::clone(&runs);
        let shared = SharedSources::with_discovery(move || {
            let mut runs = seen.lock().unwrap();
            runs.push(std::thread::current().id());
            let mut inventory = SourceInventory::fake();
            inventory.screens[0].description = format!("run {}", runs.len());
            inventory
        });
        (shared, runs)
    }

    /// GUI H-5: an empty cache discovers once, on the caller's thread; then
    /// every fact comes from the cache until a refresh replaces it.
    #[test]
    fn shared_sources_discover_once_then_serve_the_cache() {
        let (shared, runs) = counting_sources();
        assert!(shared.cached().is_none());
        let mut session_side: Box<dyn Sources> = Box::new(shared.clone());
        let screens = session_side.screens().unwrap();
        assert_eq!(screens[0].description, "run 1");
        assert_eq!(session_side.cameras().unwrap()[0].device, "/dev/fake0");
        let audio = session_side.audio().unwrap();
        assert_eq!(audio.default_mic.as_deref(), Some(FAKE_MIC));
        assert_eq!(audio.default_monitor.as_deref(), Some(FAKE_MONITOR));
        assert_eq!(audio.devices.len(), 2, "mics, then monitors");
        assert_eq!(*runs.lock().unwrap(), [std::thread::current().id()]);

        // A refresh on another thread replaces what the Session reads.
        let worker = {
            let shared = shared.clone();
            std::thread::spawn(move || {
                shared.refresh();
                std::thread::current().id()
            })
        };
        let worker = worker.join().unwrap();
        assert_eq!(session_side.screens().unwrap()[0].description, "run 2");
        assert_eq!(*runs.lock().unwrap(), [std::thread::current().id(), worker]);
        assert_eq!(shared.cached().unwrap().screens[0].description, "run 2");
        assert!(format!("{shared:?}").contains("cached: true"));
    }

    /// A caller that finds the cache empty while a refresh runs waits for it
    /// instead of discovering a second time.
    #[test]
    fn shared_sources_wait_for_a_refresh_under_way() {
        let (started_tx, started_rx) = crossbeam_channel::bounded::<()>(1);
        let runs = Arc::new(Mutex::new(0u32));
        let counted = Arc::clone(&runs);
        let shared = SharedSources::with_discovery(move || {
            *counted.lock().unwrap() += 1;
            let _ = started_tx.send(());
            std::thread::sleep(std::time::Duration::from_millis(150));
            SourceInventory::fake()
        });
        let refresher = {
            let shared = shared.clone();
            std::thread::spawn(move || shared.refresh())
        };
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the refresh started");
        let inventory = shared.inventory();
        refresher.join().unwrap();
        assert_eq!(inventory.screens.len(), 2);
        assert_eq!(*runs.lock().unwrap(), 1, "one discovery between them");
    }

    /// A kind that came back empty with a warning is a failure (logged by the
    /// gatherer), like `SystemSources` says it.
    #[test]
    fn shared_sources_pass_discovery_failures_on() {
        let shared = SharedSources::with_discovery(|| SourceInventory {
            cameras: SourceInventory::fake().cameras,
            warnings: vec![
                "screens: no Wayland display".to_owned(),
                "audio: pactl wasn't found".to_owned(),
            ],
            ..SourceInventory::default()
        });
        let mut sources: Box<dyn Sources> = Box::new(shared);
        assert_eq!(sources.screens().unwrap_err(), "no Wayland display");
        assert_eq!(sources.audio().unwrap_err(), "pactl wasn't found");
        assert_eq!(sources.cameras().unwrap().len(), 1);
        // Empty without a warning: just nothing there.
        let mut none: Box<dyn Sources> =
            Box::new(SharedSources::with_discovery(SourceInventory::default));
        assert_eq!(none.screens().unwrap(), []);
        assert_eq!(none.audio().unwrap(), AudioInventory::default());
    }

    fn project_dir() -> (tempfile::TempDir, ProjectPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap().join("swamp");
        fs::create_dir_all(root.join("takes/take-0002")).unwrap();
        fs::create_dir_all(root.join("takes/take-0007")).unwrap();
        fs::create_dir_all(root.join("takes/not-a-take")).unwrap();
        fs::create_dir_all(root.join(".qrec")).unwrap();
        fs::write(root.join("qrec.json"), "{}").unwrap();
        (tmp, ProjectPaths::new(root))
    }

    #[test]
    fn gathering_only_what_is_needed() {
        let (tmp, paths) = project_dir();
        fs::write(
            paths.take_dir(&TakeId::numbered(2)).join("A1-mic.mka"),
            b"12345",
        )
        .unwrap();
        fs::write(
            paths.take_dir(&TakeId::numbered(2)).join("recorder.log"),
            b"x",
        )
        .unwrap();
        symlink(
            "/etc/hostname",
            paths.take_dir(&TakeId::numbered(2)).join("link"),
        )
        .unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(&config, "[recording]\ncountdown_secs = 3\n").unwrap();
        let mut gatherer = FactsGatherer::new(
            paths.clone(),
            ConfigFile::new(config),
            Box::new(FakeSources::default()),
        );

        let none = gatherer.gather(&FactsNeeded::default());
        assert_eq!(none.facts.config.recording.countdown_secs, 3);
        assert_eq!(
            Facts {
                config: none.facts.config.clone(),
                ..Facts::default()
            },
            none.facts,
            "nothing else is gathered"
        );

        let all = gatherer.gather(&FactsNeeded {
            folder_name: true,
            audio_devices: true,
            video_sources: true,
            out: Some("exports/../x.mp4".into()),
            take_files: vec![TakeId::numbered(2)],
            take_dirs: true,
            files: vec![
                "takes/take-0002/A1-mic.mka".into(),
                "takes/take-0002/missing.mka".into(),
            ],
            player: true,
        });
        let facts = &all.facts;
        assert_eq!(facts.player_ns, None, "no player outside the GUI");
        assert_eq!(facts.folder_name.as_deref(), Some("swamp"));
        assert_eq!(facts.default_mic.as_ref().unwrap().name, FAKE_MIC);
        assert_eq!(facts.default_monitor.as_ref().unwrap().name, FAKE_MONITOR);
        assert_eq!(facts.screens.len(), 2);
        assert_eq!(facts.screens[1].output, "FAKE-2");
        assert_eq!(facts.cameras[0].device, "/dev/fake0");
        assert_eq!(facts.min_take_number, 8);
        let files: Vec<(&str, u64)> = facts.take_files[&TakeId::numbered(2)]
            .iter()
            .map(|f| (f.path.as_str(), f.bytes))
            .collect();
        assert_eq!(
            files,
            [
                ("takes/take-0002/A1-mic.mka", 5),
                ("takes/take-0002/recorder.log", 1)
            ]
        );
        // The link isn't deleted by a purge: it is what would be left.
        assert_eq!(
            facts.take_left[&TakeId::numbered(2)],
            ["takes/take-0002/link is a symlink"]
        );
        assert_eq!(facts.files["takes/take-0002/A1-mic.mka"].bytes, 5);
        assert!(facts.files["takes/take-0002/A1-mic.mka"].mtime_ns > 0);
        assert!(!facts.files.contains_key("takes/take-0002/missing.mka"));
        assert_eq!(all.warnings.len(), 1, "{:?}", all.warnings);
        let out = facts.out.as_ref().unwrap();
        // Relative to the project; `..` is left for the OS to resolve.
        assert_eq!(
            out.abs,
            paths.root().join("exports/../x.mp4").to_string_lossy()
        );
        assert_eq!((out.exists, out.forbidden.as_deref()), (false, None));

        let empty = ProjectPaths::new(tmp.path().join("nowhere"));
        assert_eq!(min_take_number(&empty), 1);
    }

    /// GUI G§3.10 step 3: a relative seek while the program plays asks for
    /// the GUI's player position; the GUI's gatherer reads the one the
    /// window gave it, and only when asked.
    #[test]
    fn the_players_position_is_read_when_asked() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (_tmp, paths) = project_dir();
        let config = ConfigFile::new("/nonexistent/config.toml".into());
        let reads = Arc::new(AtomicUsize::new(0));
        let position: PlayerPosition = {
            let reads = Arc::clone(&reads);
            Arc::new(move || {
                reads.fetch_add(1, Ordering::SeqCst);
                Some(40_500_000_000)
            })
        };
        let mut gatherer = FactsGatherer::new(paths, config, Box::new(FakeSources::default()))
            .with_player(Some(position));
        let need = FactsNeeded {
            player: true,
            ..FactsNeeded::default()
        };
        assert_eq!(gatherer.gather(&need).facts.player_ns, Some(40_500_000_000));
        let other = FactsNeeded {
            folder_name: true,
            ..FactsNeeded::default()
        };
        assert_eq!(gatherer.gather(&other).facts.player_ns, None);
        assert_eq!(reads.load(Ordering::SeqCst), 1, "read only when asked");
    }

    #[test]
    fn output_paths_inside_the_project_are_refused() {
        let (_tmp, paths) = project_dir();
        let root = paths.root().to_path_buf();
        let check = |out: &str| out_facts(&root, out);
        let at = |p: &str| root.join(p).to_string_lossy().into_owned();

        assert!(
            check(&at("qrec.json"))
                .forbidden
                .unwrap()
                .contains("qrec.json")
        );
        assert!(
            check(&at("takes/take-0002/x.mp4"))
                .forbidden
                .unwrap()
                .contains("takes/")
        );
        assert!(
            check(&at("exports/../takes/x.mp4"))
                .forbidden
                .unwrap()
                .contains("takes/")
        );
        assert!(
            check(&at(".qrec/x.mp4"))
                .forbidden
                .unwrap()
                .contains(".qrec/")
        );
        assert!(check(&at("takes")).forbidden.is_some());
        assert!(check(&at("")).forbidden.unwrap().contains("project folder"));
        let outside = root.parent().unwrap().join("out.mp4");
        let ok = check(&outside.to_string_lossy());
        assert_eq!((ok.exists, ok.bytes, ok.forbidden), (false, None, None));
        // A name the file system can't take is refused before rendering;
        // the longest one it takes is fine.
        let longest = root
            .parent()
            .unwrap()
            .join(format!("{}.mp4", "y".repeat(251)));
        assert_eq!(check(&longest.to_string_lossy()).forbidden, None);
        let too_long = root
            .parent()
            .unwrap()
            .join(format!("{}.mp4", "y".repeat(252)));
        assert!(
            check(&too_long.to_string_lossy())
                .forbidden
                .unwrap()
                .contains("256 bytes long")
        );

        // A symlink into takes/ is followed.
        let link = root.parent().unwrap().join("sneaky.mp4");
        symlink(root.join("takes/take-0002/A1.mka"), &link).unwrap();
        let sneaky = check(&link.to_string_lossy());
        assert!(
            sneaky.forbidden.unwrap().contains("takes/"),
            "dangling links count too"
        );
        let linked_dir = root.parent().unwrap().join("linked");
        symlink(root.join("takes"), &linked_dir).unwrap();
        assert!(
            check(&linked_dir.join("new.mp4").to_string_lossy())
                .forbidden
                .is_some()
        );

        // Existing files are reported with their size (the core asks for
        // confirmation and says what would be lost).
        fs::create_dir_all(root.join("exports")).unwrap();
        fs::write(root.join("exports/old.mp4"), "precious").unwrap();
        let existing = check(&at("exports/old.mp4"));
        assert_eq!(
            (existing.exists, existing.bytes, existing.forbidden),
            (true, Some(8), None)
        );
        // A symlink at the path is what an overwrite replaces (not its
        // target): its own size.
        let link_out = root.join("exports/link.mp4");
        symlink("old.mp4", &link_out).unwrap();
        let link_facts = check(&link_out.to_string_lossy());
        assert_eq!(
            (link_facts.exists, link_facts.bytes),
            (true, Some("old.mp4".len() as u64))
        );
        assert!(
            check(&at("exports"))
                .forbidden
                .unwrap()
                .contains("directory")
        );
        // Relative paths are taken from the project folder.
        assert_eq!(check("exports/old.mp4").abs, at("exports/old.mp4"));

        // A name in the shape of Qrec's temp files would be swept at host
        // start (A§8.7); a name that merely contains the marker is fine.
        assert!(
            check("exports/.v.qrec-part-1-2.mp4")
                .forbidden
                .unwrap()
                .contains("temp files")
        );
        let tempish_link = root.join("exports/.l.qrec-part-3-4.mp4");
        symlink("old.mp4", &tempish_link).unwrap();
        assert!(
            check(&tempish_link.to_string_lossy()).forbidden.is_some(),
            "the link is what gets replaced"
        );
        assert_eq!(check("exports/talk.qrec-part-final.mp4").forbidden, None);
    }

    /// `takes/`, `.qrec/` and `qrec.json` may be symlinks (media moved to
    /// another disk): a path that reaches them, through the link or
    /// straight at its target, is refused like the real thing (A§5.9), so a
    /// confirmed overwrite can't replace a recording.
    #[test]
    fn symlinked_project_folders_are_still_refused() {
        let (tmp, paths) = project_dir();
        let root = paths.root().to_path_buf();
        let disk = fs::canonicalize(tmp.path()).unwrap().join("disk");
        fs::create_dir_all(&disk).unwrap();
        for name in ["takes", ".qrec"] {
            fs::rename(root.join(name), disk.join(name)).unwrap();
            symlink(disk.join(name), root.join(name)).unwrap();
        }
        fs::rename(root.join("qrec.json"), disk.join("project.json")).unwrap();
        symlink(disk.join("project.json"), root.join("qrec.json")).unwrap();
        let media = root.join("takes/take-0002/L1-screen-FAKE-1.mkv");
        fs::write(&media, "recorded").unwrap();

        let check = |out: &Path| out_facts(&root, &out.to_string_lossy());
        let refused = |out: &Path, why: &str| {
            let facts = check(out);
            assert!(
                facts.forbidden.as_deref().unwrap_or("").contains(why),
                "{}: {facts:?}",
                out.display()
            );
        };
        // Through the links…
        refused(&media, "takes/");
        refused(&root.join("takes/take-0002/inside.mp4"), "takes/");
        refused(&root.join("takes/new/deeper/x.png"), "takes/");
        refused(&root.join(".qrec/inside.mp4"), ".qrec/");
        refused(&root.join("qrec.json"), "qrec.json");
        refused(Path::new("takes/take-0002/inside.png"), "takes/");
        // …and straight at their targets.
        refused(&disk.join("takes/take-0002/L1-screen-FAKE-1.mkv"), "takes/");
        refused(&disk.join("takes/take-0002/inside.mp4"), "takes/");
        refused(&disk.join(".qrec/cache/x.mp4"), ".qrec/");
        refused(&disk.join("project.json"), "qrec.json");
        // A link elsewhere that leads into the moved folder.
        let sneaky = disk.join("sneaky");
        symlink(disk.join("takes/take-0002"), &sneaky).unwrap();
        refused(&sneaky.join("x.mp4"), "takes/");
        // Next to them is fine.
        let beside = check(&disk.join("export.mp4"));
        assert_eq!((beside.exists, beside.forbidden), (false, None));
        let exports = check(&root.join("exports/x.mp4"));
        assert_eq!(exports.forbidden, None);
    }

    /// `..` after a symlinked folder is the parent of the link's target, as
    /// the kernel resolves it, not lexically the folder holding the link.
    #[test]
    fn dot_dot_after_a_symlink_means_what_the_os_says() {
        let (tmp, paths) = project_dir();
        let root = paths.root().to_path_buf();
        let outside = fs::canonicalize(tmp.path()).unwrap();
        let check = |out: &Path| out_facts(&root, &out.to_string_lossy());

        // lnk/dirlink -> takes/take-0002, so lnk/dirlink/.. is takes/.
        fs::create_dir(outside.join("lnk")).unwrap();
        symlink(root.join("takes/take-0002"), outside.join("lnk/dirlink")).unwrap();
        let sneaky = outside.join("lnk/dirlink/../x.png");
        let facts = check(&sneaky);
        assert_eq!(
            facts.abs,
            sneaky.to_string_lossy(),
            "the path is kept as given"
        );
        assert!(
            facts.forbidden.as_deref().unwrap_or("").contains("takes/"),
            "{facts:?}"
        );

        // a/b/cur -> real/sub: a/b/cur/../stills is real/stills.
        fs::create_dir_all(outside.join("real/sub")).unwrap();
        fs::create_dir_all(outside.join("real/stills")).unwrap();
        fs::create_dir_all(outside.join("a/b/stills")).unwrap();
        symlink(outside.join("real/sub"), outside.join("a/b/cur")).unwrap();
        let out = outside.join("a/b/cur/../stills/x.png");
        // A file where the path reads lexically is not what it names…
        fs::write(outside.join("a/b/stills/x.png"), "lexical").unwrap();
        let facts = check(&out);
        assert_eq!(
            (facts.exists, facts.forbidden.as_deref()),
            (false, None),
            "{facts:?}"
        );
        assert_eq!(facts.abs, out.to_string_lossy());
        // …the one where the OS resolves it is.
        fs::write(outside.join("real/stills/x.png"), "physical").unwrap();
        assert!(check(&out).exists);
        assert_eq!(fs::read_to_string(&out).unwrap(), "physical");

        // `.` goes, `..` stays; a `..` under a missing folder (which the OS
        // can't open) is read lexically, so it is still refused.
        assert_eq!(
            check(&root.join("./exports/./a.mp4")).abs,
            root.join("exports/a.mp4").to_string_lossy()
        );
        assert!(
            check(&root.join("exports/missing/../../takes/x.mp4"))
                .forbidden
                .is_some()
        );
        // A link whose target uses `..` is resolved from the link's folder.
        let relative = outside.join("lnk/rel.mp4");
        symlink("dirlink/../y.mp4", &relative).unwrap();
        assert!(
            check(&relative)
                .forbidden
                .as_deref()
                .unwrap_or("")
                .contains("takes/")
        );
    }

    /// Links *below* `takes/` and `.qrec/` (one take folder moved to another
    /// disk, one media file, `.qrec/cache`) guard what they lead to like the
    /// folders themselves (A§5.9): through the link, where the path never
    /// lies under the link's name once resolved, and straight at the
    /// target. Otherwise a confirmed export replaces a recording, and an
    /// unconfirmed one writes into a take folder (SPEC §11).
    #[test]
    fn links_below_takes_and_qrec_still_guard_what_they_hold() {
        let (tmp, paths) = project_dir();
        let root = paths.root().to_path_buf();
        let disk = fs::canonicalize(tmp.path()).unwrap().join("disk");
        fs::create_dir_all(&disk).unwrap();
        let check = |out: &Path| out_facts(&root, &out.to_string_lossy());
        let refused = |out: &Path, why: &str| {
            let facts = check(out);
            assert!(
                facts.forbidden.as_deref().unwrap_or("").contains(why),
                "{}: {facts:?}",
                out.display()
            );
        };
        let allowed = |out: &Path| {
            let facts = check(out);
            assert_eq!(facts.forbidden, None, "{}: {facts:?}", out.display());
        };

        // takes/take-0002 moved to another disk and linked back.
        fs::write(root.join("takes/take-0002/L1-screen-FAKE-1.mkv"), "rec").unwrap();
        fs::rename(root.join("takes/take-0002"), disk.join("take-0002")).unwrap();
        symlink(disk.join("take-0002"), root.join("takes/take-0002")).unwrap();
        refused(&root.join("takes/take-0002/L1-screen-FAKE-1.mkv"), "takes/");
        refused(&root.join("takes/take-0002/new.mp4"), "takes/");
        refused(&root.join("takes/take-0002/sub/new.mp4"), "takes/");
        refused(Path::new("takes/take-0002/A1-mic.mka"), "takes/");
        refused(&disk.join("take-0002/L1-screen-FAKE-1.mkv"), "takes/");
        refused(&disk.join("take-0002/new.mp4"), "takes/");
        allowed(&disk.join("beside.mp4"));

        // One media file of a real take folder moved and linked back.
        let take = root.join("takes/take-0007");
        fs::write(disk.join("A1-mic.mka"), "audio").unwrap();
        symlink(disk.join("A1-mic.mka"), take.join("A1-mic.mka")).unwrap();
        refused(&take.join("A1-mic.mka"), "takes/");
        refused(&disk.join("A1-mic.mka"), "takes/");
        // A link at the output into it is refused too (it would be replaced,
        // but it says where it leads).
        let alias = disk.join("alias.mka");
        symlink("A1-mic.mka", &alias).unwrap();
        refused(&alias, "takes/");

        // .qrec/cache moved and linked back; and a deeper link inside it.
        let qrec = root.join(".qrec");
        let cache = disk.join("cache2");
        fs::create_dir_all(cache.join("silence")).unwrap();
        symlink(&cache, qrec.join("cache")).unwrap();
        let segments = disk.join("segs");
        fs::create_dir_all(&segments).unwrap();
        symlink(&segments, cache.join("segments")).unwrap();
        refused(&qrec.join("cache/x.mp4"), ".qrec/");
        refused(&qrec.join("cache/segments/x.mov"), ".qrec/");
        refused(&cache.join("x.mp4"), ".qrec/");
        refused(&cache.join("silence/x.json"), ".qrec/");
        refused(&segments.join("x.mov"), ".qrec/");

        // A take folder whose disk isn't there (a dangling link) still
        // guards where it points.
        symlink(
            disk.join("unplugged/take-0009"),
            root.join("takes/take-0009"),
        )
        .unwrap();
        refused(&disk.join("unplugged/take-0009/x.mp4"), "takes/");
        allowed(&disk.join("unplugged.mp4"));

        // A loop of links is looked into once.
        symlink("..", disk.join("take-0002/up")).unwrap();
        refused(&disk.join("take-0002/new.png"), "takes/");
        allowed(&root.join("exports/x.mp4"));
    }

    /// What a job checks on the folder it holds open when it writes
    /// (A§8.8): the identities from that folder up, even when its path says
    /// nothing (a bind mount, a folder moved since); its path now; the name
    /// it writes in the project folder; and the file it would replace.
    #[test]
    fn an_open_folder_is_checked_by_identity_path_and_name() {
        let (tmp, paths) = project_dir();
        let root = paths.root().to_path_buf();
        let outside = fs::canonicalize(tmp.path()).unwrap();
        let file_id = |meta: fs::Metadata| FileId {
            dev: meta.dev(),
            ino: meta.ino(),
        };
        let chain = |dir: &Path| -> Vec<FileId> {
            dir.ancestors()
                .map(|p| file_id(fs::metadata(p).unwrap()))
                .collect()
        };
        let check = |dir: &Path, real: &Path, name: &str, entry: Option<FileId>| {
            why_landing_forbidden(&root, &chain(dir), real, OsStr::new(name), entry)
        };
        let says_nothing = Path::new("/mnt/qrec-nowhere/out");
        let take = root.join("takes/take-0002");
        fs::create_dir_all(take.join("sub")).unwrap();
        for dir in [take.clone(), take.join("sub"), root.join("takes")] {
            let why = check(&dir, says_nothing, "x.mp4", None).unwrap_or_default();
            assert!(why.contains("takes/"), "{}: {why}", dir.display());
        }
        let why = check(&root.join(".qrec"), says_nothing, "x.mp4", None).unwrap_or_default();
        assert!(why.contains(".qrec/"), "{why}");
        // The project folder: its own names are refused, others are fine.
        for name in ["qrec.json", "takes", ".qrec"] {
            assert!(check(&root, says_nothing, name, None).is_some(), "{name}");
        }
        assert_eq!(check(&root, says_nothing, "final.mp4", None), None);
        // Beside the project anything goes, unless what would be replaced
        // is the project file itself (a hard link to it).
        assert_eq!(check(&outside, &outside, "qrec.json", None), None);
        fs::hard_link(root.join("qrec.json"), outside.join("copy.json")).unwrap();
        let entry = Some(file_id(
            fs::symlink_metadata(outside.join("copy.json")).unwrap(),
        ));
        let why = check(&outside, &outside, "copy.json", entry).unwrap_or_default();
        assert!(why.contains("qrec.json"), "{why}");
        // A path now under takes/ is refused whatever the identities say.
        let why = check(&outside, &take, "x.mp4", None).unwrap_or_default();
        assert!(why.contains("takes/"), "{why}");
    }

    /// A path ending in `/`, `/.` or `/..` names a folder: refused
    /// (`Invalid`) whether or not the folder exists, never written as a
    /// file named like it.
    #[test]
    fn a_path_that_names_a_folder_is_refused() {
        let (tmp, paths) = project_dir();
        let root = paths.root().to_path_buf();
        let outside = fs::canonicalize(tmp.path()).unwrap();
        let why = |out: String| out_facts(&root, &out).forbidden.unwrap_or_default();
        let at = |p: &str| format!("{}/{p}", outside.display());
        assert!(why(at("newdir/")).contains("ends with '/'"));
        // Shown the way it names the folder.
        assert_eq!(out_facts(&root, &at("newdir/")).abs, at("newdir/"));
        assert_eq!(out_facts(&root, &at("newdir/.")).abs, at("newdir/."));
        assert!(why(at("newdir//")).contains("ends with '/'"));
        assert!(why(at("newdir/.")).contains("ends with '.'"));
        // `newdir/..` is the folder holding `newdir`, as the OS reads it.
        assert!(why(at("newdir/..")).contains("directory"));
        assert!(why(at("missing/new/..")).contains("names a folder"));
        assert!(why("newdir/".to_owned()).contains("names a folder"));
        fs::write(outside.join("final.webm"), "mine").unwrap();
        assert!(why(at("final.webm/")).contains("names a folder"));
        fs::create_dir(outside.join("existing")).unwrap();
        assert!(why(at("existing/")).contains("directory"));
        assert_eq!(why(at("file.mp4")), "");
    }

    /// The job is given the output path as text: in a project folder whose
    /// path isn't UTF-8, a relative output (from the GUI or IPC) can't be
    /// one, and is refused instead of written to a lossy look-alike.
    #[test]
    fn an_output_path_that_isnt_utf8_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path())
            .unwrap()
            .join(std::ffi::OsStr::from_bytes(b"pr\xffoj"));
        fs::create_dir_all(root.join("takes")).unwrap();
        let facts = out_facts(&root, "mine.mp4");
        assert_eq!(
            facts.forbidden.as_deref(),
            Some("its path isn't valid UTF-8")
        );
        assert!(facts.abs.ends_with(r"pr\xFFoj/mine.mp4"), "{}", facts.abs);
        let inside = out_facts(&root, "takes/x.mp4");
        assert!(inside.forbidden.unwrap().contains("takes/"));
        let outside = tmp.path().join("out.mp4");
        assert_eq!(out_facts(&root, &outside.to_string_lossy()).forbidden, None);
    }
}
