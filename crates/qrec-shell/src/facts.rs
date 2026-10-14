//! Gathers `Facts` for a `FactsNeeded` before `apply`/`on_event` (A§4.2):
//! the user config, the folder name, audio devices (`pactl`), screens and
//! cameras, output path checks, take files and dirs, file sizes and
//! mtimes. Exactly what was asked is gathered (no `pactl` for a clip
//! split). Under `QREC_CAPTURE_BACKEND=fake` device facts come from the
//! fake inventory (A§8.11). Owner: session.
//!
//! **Config.** `config.toml` is read on every gather (cached by size and
//! mtime) so `qrec config set` reaches a running host. The shell has no
//! TOML dependency, so it is read by [`parse_config`], a reader for the
//! TOML subset the config uses (tables, dotted keys, strings, numbers,
//! booleans, arrays, inline tables); an unreadable config falls back to
//! the defaults with a warning. Once the core's `config::parse` lands it
//! should replace it.
//!
//! **Devices.** [`Sources`] is the seam to device discovery: the fake
//! inventory, or [`SystemSources`] (audio through `pactl --format=json`;
//! screens and cameras through hooks the media owner's `sources` module
//! fills in).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use qrec_core::config::UserConfig;
use qrec_core::ctx::{
    AudioDeviceInfo, CameraInfo, CameraMode, Facts, FactsNeeded, FileFact, OutFacts, ScreenInfo,
};
use qrec_core::model::PROJECT_FILE;
use qrec_core::{Fps, TakeId};
use serde_json::{Map, Number, Value};

use crate::paths::ProjectPaths;
use crate::proc::{self, Limit, PACTL_TIMEOUT, Spec};

// ------------------------------------------------------------------ sources

/// What audio devices exist and which are the defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioInventory {
    /// Every source (inputs and monitors).
    pub devices: Vec<AudioDeviceInfo>,
    /// The default input, an `alsa_input.*` source.
    pub default_mic: Option<String>,
    /// The default sink's monitor.
    pub default_monitor: Option<String>,
}

/// Device discovery behind the facts.
pub trait Sources: Send {
    fn audio(&mut self) -> Result<AudioInventory, String>;
    fn screens(&mut self) -> Result<Vec<ScreenInfo>, String>;
    fn cameras(&mut self) -> Result<Vec<CameraInfo>, String>;
}

/// The fake backend's devices (A§8.11): screens `FAKE-1`, `FAKE-2`
/// (1280×720, side by side), camera `/dev/fake0`, mic `alsa_input.fake`,
/// monitor `alsa_output.fake.monitor`. Mirrors the capture owner's
/// `capture::fake::fake_inventory`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FakeSources;

pub const FAKE_MIC: &str = "alsa_input.fake";
pub const FAKE_MONITOR: &str = "alsa_output.fake.monitor";

impl Sources for FakeSources {
    fn audio(&mut self) -> Result<AudioInventory, String> {
        Ok(AudioInventory {
            devices: vec![
                AudioDeviceInfo {
                    name: FAKE_MIC.to_owned(),
                    description: "Fake mic".to_owned(),
                    is_monitor: false,
                },
                AudioDeviceInfo {
                    name: FAKE_MONITOR.to_owned(),
                    description: "Fake system audio".to_owned(),
                    is_monitor: true,
                },
            ],
            default_mic: Some(FAKE_MIC.to_owned()),
            default_monitor: Some(FAKE_MONITOR.to_owned()),
        })
    }

    fn screens(&mut self) -> Result<Vec<ScreenInfo>, String> {
        Ok((0..2)
            .map(|i| ScreenInfo {
                output: format!("FAKE-{}", i + 1),
                description: format!("Fake screen {}", i + 1),
                x: i * 1280,
                y: 0,
                width: 1280,
                height: 720,
                scale: 1.0,
            })
            .collect())
    }

    fn cameras(&mut self) -> Result<Vec<CameraInfo>, String> {
        let mode = |width, height, fps, format: &str| CameraMode {
            width,
            height,
            fps: Fps::new(fps, 1),
            format: format.to_owned(),
        };
        Ok(vec![CameraInfo {
            device: "/dev/fake0".to_owned(),
            label: "Fake camera".to_owned(),
            modes: vec![
                mode(640, 480, 30, "YUY2"),
                mode(640, 480, 60, "YUY2"),
                mode(1280, 720, 30, "image/jpeg"),
            ],
        }])
    }
}

/// A discovery function for screens or cameras.
pub type Discover<T> = fn() -> Result<Vec<T>, String>;

/// The machine's devices: audio through `pactl --format=json` (3 s
/// timeout, stderr to `log_dir`); screens and cameras through the hooks
/// (`None` until the media owner's discovery is wired in).
pub struct SystemSources {
    pub log_dir: Option<PathBuf>,
    pub screens: Option<Discover<ScreenInfo>>,
    pub cameras: Option<Discover<CameraInfo>>,
}

impl SystemSources {
    pub fn new(log_dir: Option<PathBuf>) -> Self {
        Self {
            log_dir,
            screens: None,
            cameras: None,
        }
    }

    fn pactl(&self, args: &[&str]) -> Result<String, String> {
        let mut spec = Spec::new("pactl", Limit::Timeout(PACTL_TIMEOUT))
            .args(args)
            .capture_stdout(8 << 20);
        if let Some(dir) = &self.log_dir {
            let stamp = proc::log_stamp(chrono::Local::now());
            if let Ok((log, _)) = proc::create_log(dir, &format!("{stamp}-{}-pactl", std::process::id())) {
                spec = spec.stderr_to(log);
            }
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
        let info = self.pactl(&["--format=json", "info"])?;
        let sources = self.pactl(&["--format=json", "list", "sources"])?;
        parse_pactl(&info, &sources)
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

/// `pactl --format=json info` + `list sources` → the inventory. Mics are
/// `alsa_input.*` only (SPEC §5.1); monitors are `*.monitor` (or have a
/// `monitor_source`).
pub fn parse_pactl(info: &str, sources: &str) -> Result<AudioInventory, String> {
    let info: Value = serde_json::from_str(info).map_err(|e| format!("pactl info: {e}"))?;
    let list: Value = serde_json::from_str(sources).map_err(|e| format!("pactl list sources: {e}"))?;
    let list = list
        .as_array()
        .ok_or_else(|| "pactl list sources: not a JSON array".to_owned())?;
    let devices: Vec<AudioDeviceInfo> = list
        .iter()
        .filter_map(|s| {
            let name = s.get("name")?.as_str()?.to_owned();
            let description = s
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or(&name)
                .to_owned();
            let monitors_a_sink = s
                .get("monitor_source")
                .and_then(Value::as_str)
                .is_some_and(|m| !m.is_empty() && m != "n/a");
            Some(AudioDeviceInfo {
                is_monitor: name.ends_with(".monitor") || monitors_a_sink,
                name,
                description,
            })
        })
        .collect();
    let has = |name: &str| devices.iter().any(|d| d.name == name);
    let is_mic = |d: &AudioDeviceInfo| !d.is_monitor && d.name.starts_with("alsa_input.");
    let default_source = info.get("default_source_name").and_then(Value::as_str);
    let default_mic = default_source
        .filter(|name| name.starts_with("alsa_input.") && has(name))
        .map(str::to_owned)
        .or_else(|| devices.iter().find(|d| is_mic(d)).map(|d| d.name.clone()));
    let default_monitor = info
        .get("default_sink_name")
        .and_then(Value::as_str)
        .map(|sink| format!("{sink}.monitor"))
        .filter(|name| has(name))
        .or_else(|| devices.iter().find(|d| d.is_monitor).map(|d| d.name.clone()));
    Ok(AudioInventory {
        devices,
        default_mic,
        default_monitor,
    })
}

// ------------------------------------------------------------------- config

/// `config.toml`, re-read when its size or mtime changes.
#[derive(Debug)]
pub struct ConfigFile {
    path: PathBuf,
    stamp: Option<(u64, SystemTime)>,
    config: UserConfig,
    problem: Option<String>,
}

impl ConfigFile {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            stamp: None,
            config: UserConfig::default(),
            problem: None,
        }
    }

    /// The current config (defaults when missing or unreadable) and, when
    /// it was (re)read and is unusable, why.
    pub fn get(&mut self) -> (UserConfig, Option<String>) {
        let stamp = fs::metadata(&self.path)
            .ok()
            .and_then(|m| Some((m.len(), m.modified().ok()?)));
        if stamp.is_none() {
            self.stamp = None;
            self.problem = None;
            self.config = UserConfig::default();
            return (self.config.clone(), None);
        }
        if stamp == self.stamp {
            return (self.config.clone(), None);
        }
        self.stamp = stamp;
        let read = fs::read_to_string(&self.path).map_err(|e| format!("can't read {}: {e}", self.path.display()));
        match read.and_then(|text| parse_config(&text)) {
            Ok(config) => {
                self.config = config;
                self.problem = None;
                (self.config.clone(), None)
            }
            Err(problem) => {
                self.config = UserConfig::default();
                let message = format!("{problem}; using the default settings");
                self.problem = Some(message.clone());
                (self.config.clone(), Some(message))
            }
        }
    }
}

/// Reads `config.toml` text into a `UserConfig` (missing settings take
/// their defaults; unknown keys and wrong types are errors).
pub fn parse_config(text: &str) -> Result<UserConfig, String> {
    let table = toml_to_json(text).map_err(|e| format!("config.toml: {e}"))?;
    serde_json::from_value(Value::Object(table)).map_err(|e| format!("config.toml: {e}"))
}

/// The TOML subset into a JSON object. Errors name the line.
pub fn toml_to_json(text: &str) -> Result<Map<String, Value>, String> {
    let mut root = Map::new();
    let mut table: Vec<String> = Vec::new();
    let mut tables: BTreeSet<Vec<String>> = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let err = |msg: String| format!("line {n}: {msg}");
        let mut p = Cursor::new(line);
        p.skip_ws();
        if p.at_end() {
            continue;
        }
        if p.eat('[') {
            if p.eat('[') {
                return Err(err("arrays of tables aren't supported".into()));
            }
            p.skip_ws();
            let path = p.key().map_err(&err)?;
            p.skip_ws();
            if !p.eat(']') {
                return Err(err("expected ] after the table name".into()));
            }
            p.end().map_err(&err)?;
            if !tables.insert(path.clone()) {
                return Err(err(format!("table [{}] is defined twice", path.join("."))));
            }
            table_at(&mut root, &path).map_err(&err)?;
            table = path;
            continue;
        }
        let key = p.key().map_err(&err)?;
        p.skip_ws();
        if !p.eat('=') {
            return Err(err("expected = after the key".into()));
        }
        p.skip_ws();
        let value = p.value().map_err(&err)?;
        p.end().map_err(&err)?;
        let full: Vec<String> = table.iter().chain(&key).cloned().collect();
        let (last, parents) = full.split_last().expect("keys have at least one part");
        let parent = table_at(&mut root, parents).map_err(&err)?;
        if parent.contains_key(last) {
            return Err(err(format!("{} is set twice", full.join("."))));
        }
        parent.insert(last.clone(), value);
    }
    Ok(root)
}

/// The table at `path`, created as needed; a non-table in the way is an error.
fn table_at<'a>(root: &'a mut Map<String, Value>, path: &[String]) -> Result<&'a mut Map<String, Value>, String> {
    let mut table = root;
    for part in path {
        let entry = table
            .entry(part.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        table = entry
            .as_object_mut()
            .ok_or_else(|| format!("{part} is a value, not a table"))?;
    }
    Ok(table)
}

struct Cursor<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(s: &'a str) -> Self {
        Self { s, i: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.i += 1;
        }
    }

    /// Only whitespace or a comment left.
    fn at_end(&self) -> bool {
        let rest = self.rest().trim_start_matches([' ', '\t']);
        rest.is_empty() || rest.starts_with('#')
    }

    fn end(&mut self) -> Result<(), String> {
        self.skip_ws();
        if self.at_end() {
            Ok(())
        } else {
            Err(format!("unexpected {:?}", self.rest()))
        }
    }

    /// `a`, `a.b`, `"quoted key".c`.
    fn key(&mut self) -> Result<Vec<String>, String> {
        let mut parts = Vec::new();
        loop {
            self.skip_ws();
            let part = match self.peek() {
                Some('"') => self.basic_string()?,
                Some('\'') => self.literal_string()?,
                Some(c) if is_bare(c) => {
                    let start = self.i;
                    while self.peek().is_some_and(is_bare) {
                        self.i += 1;
                    }
                    self.s[start..self.i].to_owned()
                }
                other => return Err(format!("expected a key, found {other:?}")),
            };
            parts.push(part);
            self.skip_ws();
            if !self.eat('.') {
                return Ok(parts);
            }
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('"') => self.basic_string().map(Value::String),
            Some('\'') => self.literal_string().map(Value::String),
            Some('[') => self.array(),
            Some('{') => self.inline_table(),
            Some(_) => self.scalar(),
            None => Err("a value is missing".into()),
        }
    }

    fn basic_string(&mut self) -> Result<String, String> {
        if self.rest().starts_with("\"\"\"") {
            return Err("multi-line strings aren't supported".into());
        }
        self.bump();
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err("unterminated string".into()),
                Some('"') => return Ok(out),
                Some('\\') => {
                    let c = match self.bump() {
                        Some('b') => '\u{8}',
                        Some('t') => '\t',
                        Some('n') => '\n',
                        Some('f') => '\u{c}',
                        Some('r') => '\r',
                        Some('"') => '"',
                        Some('\\') => '\\',
                        Some(u @ ('u' | 'U')) => {
                            let len = if u == 'u' { 4 } else { 8 };
                            let hex: String = (0..len).filter_map(|_| self.bump()).collect();
                            u32::from_str_radix(&hex, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .ok_or_else(|| format!("bad escape \\{u}{hex}"))?
                        }
                        other => return Err(format!("bad escape \\{}", other.unwrap_or(' '))),
                    };
                    out.push(c);
                }
                Some(c) => out.push(c),
            }
        }
    }

    fn literal_string(&mut self) -> Result<String, String> {
        if self.rest().starts_with("'''") {
            return Err("multi-line strings aren't supported".into());
        }
        self.bump();
        let rest = self.rest();
        let end = rest.find('\'').ok_or("unterminated string")?;
        self.i += end + 1;
        Ok(rest[..end].to_owned())
    }

    fn array(&mut self) -> Result<Value, String> {
        self.bump();
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.eat(']') {
                return Ok(Value::Array(items));
            }
            items.push(self.value()?);
            self.skip_ws();
            if self.eat(',') {
                continue;
            }
            self.skip_ws();
            if self.eat(']') {
                return Ok(Value::Array(items));
            }
            return Err("expected , or ] in an array (arrays must fit on one line)".into());
        }
    }

    fn inline_table(&mut self) -> Result<Value, String> {
        self.bump();
        let mut table = Map::new();
        self.skip_ws();
        if self.eat('}') {
            return Ok(Value::Object(table));
        }
        loop {
            let key = self.key()?;
            self.skip_ws();
            if !self.eat('=') {
                return Err("expected = in an inline table".into());
            }
            self.skip_ws();
            let value = self.value()?;
            let (last, parents) = key.split_last().expect("keys have at least one part");
            let parent = table_at(&mut table, parents)?;
            if parent.insert(last.clone(), value).is_some() {
                return Err(format!("{} is set twice", key.join(".")));
            }
            self.skip_ws();
            if self.eat('}') {
                return Ok(Value::Object(table));
            }
            if !self.eat(',') {
                return Err("expected , or } in an inline table".into());
            }
        }
    }

    fn scalar(&mut self) -> Result<Value, String> {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|c| !matches!(c, ' ' | '\t' | ',' | ']' | '}' | '#'))
        {
            self.bump();
        }
        let token = &self.s[start..self.i];
        match token {
            "true" => return Ok(Value::Bool(true)),
            "false" => return Ok(Value::Bool(false)),
            "" => return Err("a value is missing".into()),
            _ => {}
        }
        let digits = token.replace('_', "");
        let (sign, body) = match digits.strip_prefix('-') {
            Some(rest) => (-1i64, rest),
            None => (1, digits.strip_prefix('+').unwrap_or(&digits)),
        };
        let special = matches!(body, "inf" | "nan");
        if !special && !body.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(format!("unknown value {token:?} (strings need quotes)"));
        }
        for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
            if let Some(n) = body.strip_prefix(prefix) {
                return i64::from_str_radix(n, radix)
                    .map(|v| Value::from(sign * v))
                    .map_err(|_| format!("bad number {token}"));
            }
        }
        let date = body.len() > 4 && body.as_bytes()[4] == b'-' && body[..4].bytes().all(|b| b.is_ascii_digit());
        if date || body.contains(':') {
            return Err(format!("dates and times aren't supported ({token})"));
        }
        if special || body.contains(['.', 'e', 'E']) {
            let v: f64 = digits.parse().map_err(|_| format!("bad number {token}"))?;
            return Number::from_f64(v)
                .map(Value::Number)
                .ok_or_else(|| format!("{token} isn't a usable number"));
        }
        digits
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("bad number {token}"))
    }
}

fn is_bare(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

// -------------------------------------------------------------------- facts

/// Gathers facts for one project.
pub struct FactsGatherer {
    /// The canonical project folder.
    paths: ProjectPaths,
    config: ConfigFile,
    sources: Box<dyn Sources>,
}

impl std::fmt::Debug for FactsGatherer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactsGatherer")
            .field("root", &self.paths.root())
            .finish_non_exhaustive()
    }
}

/// Facts plus what went wrong getting them (for the log; the core turns
/// missing facts into errors of its own).
#[derive(Debug, Clone, PartialEq)]
pub struct Gathered {
    pub facts: Facts,
    pub warnings: Vec<String>,
    /// `config.toml` was just (re)read and is unusable (reported once per change).
    pub config_problem: Option<String>,
}

impl FactsGatherer {
    /// `paths.root()` must be canonical (output paths are compared with it).
    pub fn new(paths: ProjectPaths, config_file: PathBuf, sources: Box<dyn Sources>) -> Self {
        Self {
            paths,
            config: ConfigFile::new(config_file),
            sources,
        }
    }

    /// The config alone (every dispatch needs it).
    pub fn config(&mut self) -> (UserConfig, Option<String>) {
        self.config.get()
    }

    /// Gathers exactly what `need` asks for, plus the config.
    pub fn gather(&mut self, need: &FactsNeeded) -> Gathered {
        let mut warnings = Vec::new();
        let (config, config_problem) = self.config.get();
        let mut facts = Facts {
            config,
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
            facts.take_files.insert(take.clone(), take_files(&self.paths, take));
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
        Gathered {
            facts,
            warnings,
            config_problem,
        }
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

/// The regular files in `takes/<take>/` with their sizes (symlinks and
/// folders aren't followed: purge deletes only regular files).
pub fn take_files(paths: &ProjectPaths, take: &TakeId) -> Vec<FileFact> {
    let Ok(entries) = fs::read_dir(paths.take_dir(take)) else {
        return Vec::new();
    };
    let mut files: Vec<FileFact> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let meta = e.path().symlink_metadata().ok()?;
            meta.file_type().is_file().then(|| FileFact {
                path: format!("{}/{}", take_rel(take), e.file_name().to_string_lossy()),
                bytes: meta.len(),
                mtime_ns: mtime_ns(&meta),
            })
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
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
/// whether something is there, and why it can't be used (inside `takes/`
/// or `.qrec/`, `qrec.json` itself, a directory). Symlinks are resolved
/// for the check, so a link into `takes/` is refused too. A relative path
/// is taken relative to the project folder.
pub fn out_facts(project_root: &Path, out: &str) -> OutFacts {
    let given = Path::new(out);
    let joined = if given.is_absolute() {
        given.to_path_buf()
    } else {
        project_root.join(given)
    };
    let abs = normalize(&joined);
    let meta = fs::symlink_metadata(&abs).ok();
    let mut candidates = vec![resolve_existing(&abs)];
    // A (possibly dangling) symlink: where it points counts too.
    if meta.as_ref().is_some_and(|m| m.file_type().is_symlink())
        && let Ok(target) = fs::read_link(&abs)
    {
        let target = abs.parent().map_or_else(|| target.clone(), |dir| dir.join(&target));
        candidates.push(resolve_existing(&normalize(&target)));
    }
    let forbidden = candidates
        .iter()
        .find_map(|path| why_forbidden(project_root, path));
    OutFacts {
        abs: abs.to_string_lossy().into_owned(),
        exists: meta.is_some(),
        forbidden,
    }
}

fn why_forbidden(project_root: &Path, resolved: &Path) -> Option<String> {
    let inside = |dir: &str| resolved.starts_with(project_root.join(dir));
    if resolved == project_root.join(PROJECT_FILE) {
        Some("that is the project file, qrec.json".to_owned())
    } else if inside("takes") {
        Some("it is inside takes/, where recorded media lives".to_owned())
    } else if inside(".qrec") {
        Some("it is inside .qrec/, Qrec's own files".to_owned())
    } else if resolved == project_root {
        Some("it is the project folder".to_owned())
    } else if fs::metadata(resolved).is_ok_and(|m| m.is_dir()) {
        Some("it is a directory".to_owned())
    } else {
        None
    }
}

/// `.` and `..` removed lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The path with its existing prefix resolved through symlinks.
fn resolve_existing(path: &Path) -> PathBuf {
    if let Ok(full) = fs::canonicalize(path) {
        return full;
    }
    let mut missing = Vec::new();
    let mut cursor = path.to_path_buf();
    while let Some(name) = cursor.file_name().map(|n| n.to_owned()) {
        cursor.pop();
        missing.push(name);
        if let Ok(base) = fs::canonicalize(&cursor) {
            return missing.iter().rev().fold(base, |acc, part| acc.join(part));
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use qrec_core::config::Theme;
    use serde_json::json;

    use super::*;

    #[test]
    fn toml_subset() {
        let text = r#"
# comment
theme = "dragons-keep"   # trailing comment
a.b = 'literal \n'
[canvas]
fps = "30000/1001"
width = 1_280
[autocut]
threshold_db = -35.5
min_silence_ms = 0x190
padding_ms = 1.2e2
[ "quoted table" ]
list = [1, "two", true, ]
inline = { x = 1, y.z = "w" }
esc = "tab\there é"
"#;
        let map = toml_to_json(text).unwrap();
        assert_eq!(
            Value::Object(map),
            json!({
                "theme": "dragons-keep",
                "a": { "b": "literal \\n" },
                "canvas": { "fps": "30000/1001", "width": 1280 },
                "autocut": { "threshold_db": -35.5, "min_silence_ms": 400, "padding_ms": 120.0 },
                "quoted table": {
                    "list": [1, "two", true],
                    "inline": { "x": 1, "y": { "z": "w" } },
                    "esc": "tab\there é"
                }
            })
        );
        for (bad, needle) in [
            ("a = 1\na = 2", "line 2: a is set twice"),
            ("[t]\n[t]", "line 2: table [t] is defined twice"),
            ("[[t]]", "arrays of tables"),
            ("s = \"\"\"x", "multi-line"),
            ("d = 1979-05-27", "dates and times"),
            ("t = 07:32:00", "dates and times"),
            ("x = bare", "strings need quotes"),
            ("x = 1e", "bad number"),
            ("x = 1 2", "unexpected"),
            ("x", "expected ="),
            ("x = \"open", "unterminated"),
            ("a = 1\n[a]", "a is a value, not a table"),
            ("x = inf", "isn't a usable number"),
        ] {
            let err = toml_to_json(bad).unwrap_err();
            assert!(err.contains(needle), "{bad:?}: {err}");
        }
    }

    #[test]
    fn config_files() {
        assert_eq!(parse_config("").unwrap(), UserConfig::default());
        let cfg = parse_config(
            "theme = \"ogre-mode\"\n[canvas]\nwidth = 1280\nheight = 720\nfps = 25\n[encoder]\nvaapi_device = \"/dev/dri/renderD129\"\n[autocut]\nthreshold_db = -40\n",
        )
        .unwrap();
        assert_eq!(cfg.theme, Theme::OgreMode);
        assert_eq!((cfg.canvas.width, cfg.canvas.height, cfg.canvas.fps), (1280, 720, Fps::new(25, 1)));
        assert_eq!(cfg.encoder.vaapi_device.as_deref(), Some("/dev/dri/renderD129"));
        assert_eq!(cfg.autocut.threshold_db, -40.0);
        let err = parse_config("[canvas]\nfsp = 25\n").unwrap_err();
        assert!(err.starts_with("config.toml: unknown field `fsp`"), "{err}");
        assert!(parse_config("theme = 3").is_err());

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let mut file = ConfigFile::new(path.clone());
        assert_eq!(file.get(), (UserConfig::default(), None), "no file: defaults");
        fs::write(&path, "[canvas]\nfps = 60\n").unwrap();
        assert_eq!(file.get().0.canvas.fps, Fps::new(60, 1));
        fs::write(&path, "[canvas]\nfps = \"sixty\"\n").unwrap();
        let (cfg, problem) = file.get();
        assert_eq!(cfg, UserConfig::default());
        assert!(problem.unwrap().ends_with("using the default settings"));
        assert_eq!(file.get().1, None, "reported once");
    }

    #[test]
    fn pactl_inventory() {
        let info = r#"{"default_sink_name":"alsa_output.usb-yeti.analog-stereo","default_source_name":"alsa_input.usb-yeti.analog-stereo"}"#;
        let sources = r#"[
            {"name":"alsa_output.hdmi.monitor","description":"Monitor of HDMI","monitor_source":"alsa_output.hdmi"},
            {"name":"alsa_input.laptop.mic","description":"Laptop mic","monitor_source":""},
            {"name":"alsa_output.usb-yeti.analog-stereo.monitor","description":"Monitor of Yeti","monitor_source":"alsa_output.usb-yeti.analog-stereo"},
            {"name":"alsa_input.usb-yeti.analog-stereo","description":"Yeti Nano","monitor_source":""},
            {"name":"bluez_input.headset","description":"Headset","monitor_source":""}
        ]"#;
        let inv = parse_pactl(info, sources).unwrap();
        assert_eq!(inv.devices.len(), 5);
        assert_eq!(inv.default_mic.as_deref(), Some("alsa_input.usb-yeti.analog-stereo"));
        assert_eq!(inv.default_monitor.as_deref(), Some("alsa_output.usb-yeti.analog-stereo.monitor"));
        assert!(inv.devices[0].is_monitor && !inv.devices[1].is_monitor);

        // A monitor as the default source isn't a mic; a missing sink monitor
        // falls back to the first monitor.
        let info = r#"{"default_sink_name":"gone","default_source_name":"alsa_output.hdmi.monitor"}"#;
        let inv = parse_pactl(info, sources).unwrap();
        assert_eq!(inv.default_mic.as_deref(), Some("alsa_input.laptop.mic"));
        assert_eq!(inv.default_monitor.as_deref(), Some("alsa_output.hdmi.monitor"));
        assert!(parse_pactl("{}", "{}").is_err());
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
        fs::write(paths.take_dir(&TakeId::numbered(2)).join("A1-mic.mka"), b"12345").unwrap();
        fs::write(paths.take_dir(&TakeId::numbered(2)).join("recorder.log"), b"x").unwrap();
        symlink("/etc/hostname", paths.take_dir(&TakeId::numbered(2)).join("link")).unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(&config, "[recording]\ncountdown_secs = 3\n").unwrap();
        let mut gatherer = FactsGatherer::new(paths.clone(), config, Box::new(FakeSources));

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
            files: vec!["takes/take-0002/A1-mic.mka".into(), "takes/take-0002/missing.mka".into()],
        });
        let facts = &all.facts;
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
        assert_eq!(files, [("takes/take-0002/A1-mic.mka", 5), ("takes/take-0002/recorder.log", 1)]);
        assert_eq!(facts.files["takes/take-0002/A1-mic.mka"].bytes, 5);
        assert!(facts.files["takes/take-0002/A1-mic.mka"].mtime_ns > 0);
        assert!(!facts.files.contains_key("takes/take-0002/missing.mka"));
        assert_eq!(all.warnings.len(), 1, "{:?}", all.warnings);
        let out = facts.out.as_ref().unwrap();
        assert_eq!(out.abs, paths.root().join("x.mp4").to_string_lossy());
        assert_eq!((out.exists, out.forbidden.as_deref()), (false, None));

        let empty = ProjectPaths::new(tmp.path().join("nowhere"));
        assert_eq!(min_take_number(&empty), 1);
    }

    #[test]
    fn output_paths_inside_the_project_are_refused() {
        let (_tmp, paths) = project_dir();
        let root = paths.root().to_path_buf();
        let check = |out: &str| out_facts(&root, out);
        let at = |p: &str| root.join(p).to_string_lossy().into_owned();

        assert!(check(&at("qrec.json")).forbidden.unwrap().contains("qrec.json"));
        assert!(check(&at("takes/take-0002/x.mp4")).forbidden.unwrap().contains("takes/"));
        assert!(check(&at("exports/../takes/x.mp4")).forbidden.unwrap().contains("takes/"));
        assert!(check(&at(".qrec/x.mp4")).forbidden.unwrap().contains(".qrec/"));
        assert!(check(&at("takes")).forbidden.is_some());
        assert!(check(&at("")).forbidden.unwrap().contains("project folder"));
        let outside = root.parent().unwrap().join("out.mp4");
        let ok = check(&outside.to_string_lossy());
        assert_eq!((ok.exists, ok.forbidden), (false, None));

        // A symlink into takes/ is followed.
        let link = root.parent().unwrap().join("sneaky.mp4");
        symlink(root.join("takes/take-0002/A1.mka"), &link).unwrap();
        let sneaky = check(&link.to_string_lossy());
        assert!(sneaky.forbidden.unwrap().contains("takes/"), "dangling links count too");
        let linked_dir = root.parent().unwrap().join("linked");
        symlink(root.join("takes"), &linked_dir).unwrap();
        assert!(check(&linked_dir.join("new.mp4").to_string_lossy()).forbidden.is_some());

        // Existing files are reported (the core asks for confirmation).
        fs::create_dir_all(root.join("exports")).unwrap();
        fs::write(root.join("exports/old.mp4"), "x").unwrap();
        let existing = check(&at("exports/old.mp4"));
        assert_eq!((existing.exists, existing.forbidden), (true, None));
        assert!(check(&at("exports")).forbidden.unwrap().contains("directory"));
        // Relative paths are taken from the project folder.
        assert_eq!(check("exports/old.mp4").abs, at("exports/old.mp4"));
    }
}
