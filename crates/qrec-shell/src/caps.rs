//! Capability detection (A§3 `Capabilities`, A§8.10): the ffmpeg version,
//! resolved path (its identity, part of export cache keys) and encoders;
//! the effective VA render node and its vendor; the h264_vaapi export
//! check, run as a child process with a 10 s timeout (a hung GPU driver
//! can't hang Qrec); optionally the H.264 encoders GStreamer records with
//! ([`CapsOptions::gst_probe`], e.g. [`gst_probe`] for hosts that record):
//! the `va*h264enc` and `va*h264lpenc` bound to the node, x264enc and
//! openh264enc. Cached in `$XDG_CACHE_HOME/qrec/capabilities.json`, keyed by
//! the ffmpeg binary, the node and its PCI device id, `LIBVA_DRIVER_NAME`
//! and Qrec's libva driver files (installing or updating one probes
//! again); GStreamer's answers also by its plugin paths and registry
//! ([`GstEnv`]), so a plugin that comes or goes is noticed. The session
//! sends the result as `Event::Capabilities` before its first dispatch.
//! Owner: session.
//!
//! The VA node is never chosen by VA-API itself (A§2.1, the capture
//! owner's `gpu::effective_va_device`): the project's
//! `recording.vaapiDevice`, else the config's `encoder.vaapi_device`, else
//! the first Intel or AMD render node. An NVIDIA node, which only a
//! configuration names, is never probed (NVIDIA has no VA-API encoder; its
//! `renderD128` hangs the dev machine): VA-API is off then. The checks load
//! the node's libva driver (`gpu::child_libva_driver`), the one `main()`
//! pinned. Where the h264_vaapi check fails with it on an Intel node, it
//! runs again with Intel's other driver when that one is installed and
//! drives the chip (`gpu::retry_driver`); when that one passes, exports
//! load it there, and the file keeps the pick (`exportDriver`,
//! [`read_export_driver`]) for `gpu::export_libva_driver_for`. GStreamer is
//! asked once, with the node's driver: its VA plugin registers no encoder
//! with `i965`, so recordings never switch drivers.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use qrec_core::state::{Capabilities, FfmpegCaps, GstH264Caps, VaapiCaps};
use serde::{Deserialize, Serialize};

use crate::gpu::{self, VaDriverPick};
use crate::proc::{self, Limit, PROBE_TIMEOUT, Spec};
use crate::store::fsx;

/// Asks GStreamer which H.264 encoders it records with: those bound to a
/// render node, when given one, and the software ones. Its child's stderr
/// goes to the log, when given one.
pub type GstProbe = fn(Option<&str>, Option<&Path>) -> GstFound;

/// What a [`GstProbe`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GstFound {
    /// `VaapiCaps::gst_h264_factory`: the node's `va*h264enc`.
    pub va_factory: Option<String>,
    /// `VaapiCaps::gst_h264_lp_factory`: the node's `va*h264lpenc`.
    pub va_lp_factory: Option<String>,
    /// `Capabilities::gst_h264`; `None` when GStreamer couldn't answer:
    /// then none of this is cached, and the next host that records asks
    /// again.
    pub h264: Option<GstH264Caps>,
}

/// The variables GStreamer finds its plugins with.
const PLUGIN_PATH_VARS: [&str; 4] = [
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_PATH",
];

/// What GStreamer's answers depend on: where it finds plugins, and its
/// registry, which a GStreamer init rewrites when plugins come or go
/// (`gpu::plan_gst_env` points `GST_REGISTRY_1_0` at Qrec's own).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GstEnv {
    /// [`PLUGIN_PATH_VARS`]' values, in that order.
    pub plugin_paths: Vec<Option<String>>,
    /// The registry file (`GST_REGISTRY_1_0`, else `GST_REGISTRY`).
    pub registry: Option<PathBuf>,
}

impl GstEnv {
    /// This process's.
    pub fn from_process() -> Self {
        let var = |key: &str| std::env::var(key).ok();
        Self {
            plugin_paths: PLUGIN_PATH_VARS.iter().map(|key| var(key)).collect(),
            registry: var("GST_REGISTRY_1_0")
                .or_else(|| var("GST_REGISTRY"))
                .map(PathBuf::from),
        }
    }

    /// The key of GStreamer's answers as things are now: the plugin paths,
    /// and the registry's size and modification time.
    fn key(&self) -> GstKey {
        let meta = self.registry.as_deref().and_then(|p| fs::metadata(p).ok());
        GstKey {
            plugin_paths: self.plugin_paths.clone(),
            registry: self
                .registry
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            registry_len: meta.as_ref().map(fs::Metadata::len),
            registry_mtime_ns: meta.as_ref().and_then(mtime_ns),
        }
    }
}

/// Where and how to detect.
#[derive(Debug, Clone)]
pub struct CapsOptions {
    /// The ffmpeg to detect (looked up on `PATH` like the media runner does).
    pub ffmpeg: OsString,
    /// The ffprobe the media runner runs (looked up on `PATH` the same way).
    pub ffprobe: OsString,
    /// Where render nodes are listed (`/sys/class/drm`)…
    pub sys_class_drm: PathBuf,
    /// …and where their device files are (`/dev/dri`).
    pub dev_dri: PathBuf,
    /// Run the h264_vaapi export check (else `ffmpeg_h264` is false), and
    /// let [`gst_probe`](Self::gst_probe) look for VA encoders (else it only
    /// asks for the software ones). Off for the fake backend.
    pub probe_va: bool,
    /// `capabilities.json`; `None` disables the cache.
    pub cache_file: Option<PathBuf>,
    /// Where the probes' stderr goes.
    pub log_dir: Option<PathBuf>,
    /// Fills `Capabilities::gst_h264` and, with `probe_va` (the real
    /// backend), the VA factories bound to the node: hosts that record
    /// (see [`gst_probe`]). `None` leaves them unknown (capture plans then
    /// use x264).
    pub gst_probe: Option<GstProbe>,
    /// What the probe's answers depend on (the cache's key for them).
    pub gst_env: GstEnv,
    /// Libva's driver files: the checks' driver when this process's
    /// doesn't fit the node, and the retry's (`gpu::retry_driver`).
    pub va_drivers: gpu::VaDrivers,
    /// This process's `LIBVA_DRIVER_NAME`, which `main()` pinned: the
    /// checks' driver when it fits the node (`gpu::child_libva_driver`).
    pub libva_driver: Option<String>,
}

impl CapsOptions {
    /// The machine's ffmpeg, render nodes and libva drivers, cached in
    /// `cache_file`.
    pub fn system(cache_file: Option<PathBuf>, log_dir: Option<PathBuf>) -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            sys_class_drm: "/sys/class/drm".into(),
            dev_dri: "/dev/dri".into(),
            probe_va: true,
            cache_file,
            log_dir,
            gst_probe: None,
            gst_env: GstEnv::from_process(),
            va_drivers: gpu::VaDrivers::from_process(),
            libva_driver: std::env::var(gpu::LIBVA_DRIVER_NAME)
                .ok()
                .filter(|d| !d.is_empty()),
        }
    }
}

/// The devices configured for VA-API (A§2.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaConfig {
    /// `recording.vaapiDevice` of the project.
    pub project: Option<String>,
    /// `encoder.vaapi_device` of the user config.
    pub config: Option<String>,
}

/// What detection found, and what it noticed on the way.
#[derive(Debug, Clone, PartialEq)]
pub struct Detected {
    pub caps: Capabilities,
    pub notes: Vec<String>,
    /// The ffmpeg and VA checks came from the cache.
    pub cached: bool,
}

/// The [`GstProbe`] of hosts that record: the capture owner's probe child
/// (`gpu::run_probe_child`: this executable run again with
/// `QREC_GPU_PROBE=h264-encoders[:<node>]`, 10 s timeout), so GStreamer's
/// init and plugin scan, which touches the GPU, never run in the host
/// itself, nor in a command that only needs ffmpeg. Only for a `qrec`
/// whose `main` calls `gpu::run_probe_if_requested` first.
pub fn gst_probe(device: Option<&str>, log: Option<&Path>) -> GstFound {
    let Ok(exe) = std::env::current_exe() else {
        return GstFound::default();
    };
    let probe = gpu::Probe::H264Encoders {
        device: device.map(str::to_owned),
    };
    match gpu::run_probe_child(&exe, &probe, log, gpu::PROBE_TIMEOUT) {
        Ok(answer) => {
            if let Some(error) = &answer.error {
                tracing::warn!("the GStreamer H.264 encoder probe: {error}");
            }
            GstFound {
                va_factory: answer.factory,
                va_lp_factory: answer.lp_factory,
                h264: answer.software,
            }
        }
        Err(e) => {
            tracing::warn!("the GStreamer H.264 encoder probe failed: {e}");
            GstFound::default()
        }
    }
}

/// Detects the capabilities (from the cache when nothing changed). ffprobe
/// is only looked up on `PATH` (nothing runs), so it is never cached.
/// GStreamer's answers ([`CapsOptions::gst_probe`]) are cached with their
/// own key ([`GstEnv`]): when only that changed, only the probe runs again.
pub fn detect(opts: &CapsOptions, va: &VaConfig) -> Detected {
    let cores = std::thread::available_parallelism()
        .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX));
    let mut notes = Vec::new();
    let ffprobe = resolve_program(&opts.ffprobe).map(|p| p.to_string_lossy().into_owned());
    if ffprobe.is_none() {
        notes.push(format!(
            "{} is not installed (or not on PATH): exports and take probes won't work",
            opts.ffprobe.to_string_lossy()
        ));
    }
    let Some(ffmpeg) = resolve_program(&opts.ffmpeg) else {
        notes.push(format!(
            "{} is not installed (or not on PATH): exports, frames and autocut won't work",
            opts.ffmpeg.to_string_lossy()
        ));
        return Detected {
            caps: Capabilities {
                ffmpeg: None,
                ffprobe,
                vaapi: None,
                gst_h264: None,
                cores,
            },
            notes,
            cached: false,
        };
    };
    let nodes = gpu::scan_render_nodes(&opts.sys_class_drm, &opts.dev_dri);
    let choice = gpu::effective_va_device(va.project.as_deref(), va.config.as_deref(), &nodes);
    let node = choice
        .as_ref()
        .and_then(|c| gpu::node_for(&c.device, &nodes))
        .cloned();
    // The node the VA checks run on (as configured, which the core compares
    // with the configuration), its vendor, its sysfs entry: never NVIDIA's.
    let target = match (&choice, &node) {
        (Some(choice), _) if choice.vendor.is_none() => {
            notes.push(format!(
                "{} isn't a render node this machine lists; VA-API is off",
                choice.device
            ));
            None
        }
        (Some(choice), _) if choice.vendor == Some(gpu::VENDOR_NVIDIA) => {
            notes.push(format!(
                "{} is an NVIDIA node: NVIDIA has no VA-API encoder, so it isn't probed and \
                 VA-API is off",
                choice.device
            ));
            None
        }
        (Some(choice), Some(node)) => choice.vendor.map(|v| (choice.device.clone(), v, node)),
        _ => None,
    };
    // The libva driver of the checks: the one the node's children load.
    let driver = target.as_ref().and_then(|(_, _, node)| {
        gpu::child_libva_driver(node, opts.libva_driver.as_deref(), &opts.va_drivers)
    });
    let key = CacheKey {
        ffmpeg: ffmpeg.to_string_lossy().into_owned(),
        ffmpeg_len: fs::metadata(&ffmpeg).map_or(0, |m| m.len()),
        ffmpeg_mtime_ns: fs::metadata(&ffmpeg)
            .ok()
            .as_ref()
            .and_then(mtime_ns)
            .unwrap_or(0),
        va_device: choice.as_ref().map(|c| c.device.clone()),
        va_vendor: choice.as_ref().and_then(|c| c.vendor),
        va_device_id: node.as_ref().and_then(|n| n.device_id),
        probed_va: opts.probe_va,
        libva_driver: opts.libva_driver.clone(),
        libva_drivers: driver_stamps(&opts.va_drivers),
    };
    let mut log = ProbeLog::new(opts.log_dir.as_deref());
    if let Some(mut cached) = opts.cache_file.as_deref().and_then(|f| read_cache(f, &key)) {
        // GStreamer's answers may be missing (a host that doesn't record
        // filled the cache, or a probe failed) or stale (a plugin came or
        // went): ask for them alone.
        let gst_key = opts.gst_env.key();
        if let Some(probe) = opts.gst_probe
            && cached.gst.as_ref().is_none_or(|gst| gst.key != gst_key)
        {
            let device = cached
                .vaapi
                .as_ref()
                .filter(|_| opts.probe_va)
                .map(|v| v.device.clone());
            let found = probe(device.as_deref(), log.path());
            cached.gst = remember_gst(&found, cached.vaapi.as_mut(), &opts.gst_env);
            if let Some(file) = &opts.cache_file {
                write_cache(file, &cached);
            }
        }
        return Detected {
            caps: Capabilities {
                ffmpeg: Some(cached.ffmpeg),
                ffprobe,
                vaapi: cached.vaapi,
                gst_h264: cached.gst.map(|gst| gst.h264),
                cores,
            },
            notes,
            cached: true,
        };
    }

    let run = |args: &[&str], env: Option<(&str, &str)>, log: &mut ProbeLog| {
        let mut spec = Spec::new(&ffmpeg, Limit::Timeout(PROBE_TIMEOUT))
            .label("ffmpeg")
            .args(args)
            .capture_stdout(4 << 20);
        if let Some((k, v)) = env {
            spec = spec.env(k, v);
        }
        if let Some(file) = log.file() {
            spec = spec.stderr_to(file);
        }
        spec.run()
    };

    let version = match run(&["-hide_banner", "-version"], None, &mut log) {
        Ok(out) if out.success() => parse_version(&out.stdout_text()),
        Ok(out) => {
            notes.push(format!("`ffmpeg -version`: {}", out.describe()));
            None
        }
        Err(e) => {
            notes.push(format!("`ffmpeg -version`: {e}"));
            None
        }
    };
    let encoders = match run(&["-hide_banner", "-encoders"], None, &mut log) {
        Ok(out) if out.success() => parse_encoders(&out.stdout_text()),
        Ok(out) => {
            notes.push(format!("`ffmpeg -encoders`: {}", out.describe()));
            BTreeSet::new()
        }
        Err(e) => {
            notes.push(format!("`ffmpeg -encoders`: {e}"));
            BTreeSet::new()
        }
    };
    let ffmpeg_caps = FfmpegCaps {
        version: version.unwrap_or_else(|| "unknown".to_owned()),
        identity: key.ffmpeg.clone(),
        encoders,
    };

    // Intel's other driver, where the check passed with it alone.
    let mut export_driver = None;
    let mut vaapi = target.as_ref().map(|(device, vendor, node)| {
        let ffmpeg_h264 = if !opts.probe_va {
            false
        } else if !ffmpeg_caps.encoders.contains("h264_vaapi") {
            notes.push("this ffmpeg has no h264_vaapi encoder".to_owned());
            false
        } else {
            let argv = va_probe_args(device);
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            // A vendor without a known driver keeps the inherited one.
            let mut check = |driver: Option<&str>| match run(
                &args,
                driver.map(|d| (gpu::LIBVA_DRIVER_NAME, d)),
                &mut log,
            ) {
                Ok(out) if out.success() => Ok(()),
                Ok(out) => Err(out.describe()),
                Err(e) => Err(e.to_string()),
            };
            match check(driver.as_deref()) {
                Ok(()) => true,
                Err(why) => {
                    let retry = driver.as_deref().and_then(|first| {
                        let other = gpu::retry_driver(node, first, &opts.va_drivers)?;
                        Some((first, other, check(Some(other))))
                    });
                    match retry {
                        Some((first, other, Ok(()))) => {
                            notes.push(format!(
                                "the h264_vaapi check on {device} failed with {first}: {why}; \
                                 it passed with {other}, which exports load there"
                            ));
                            export_driver = Some(other);
                            true
                        }
                        Some((_, other, Err(again))) => {
                            notes.push(format!(
                                "the h264_vaapi check on {device} failed: {why}; with {other} \
                                 too: {again}"
                            ));
                            false
                        }
                        None => {
                            notes.push(format!("the h264_vaapi check on {device} failed: {why}"));
                            false
                        }
                    }
                }
            }
        };
        VaapiCaps {
            device: device.clone(),
            vendor: *vendor,
            ffmpeg_h264,
            gst_h264_factory: None,
            gst_h264_lp_factory: None,
        }
    });
    let gst = opts.gst_probe.and_then(|probe| {
        let device = vaapi
            .as_ref()
            .filter(|_| opts.probe_va)
            .map(|v| v.device.clone());
        let found = probe(device.as_deref(), log.path());
        remember_gst(&found, vaapi.as_mut(), &opts.gst_env)
    });
    // What exports load on the node, when it isn't its own driver.
    let export_driver = export_driver
        .zip(target.as_ref())
        .map(|(driver, (_, _, node))| VaDriverPick {
            device: node.path.clone(),
            device_id: node.device_id,
            driver: driver.to_owned(),
        });
    let gst_h264 = gst.as_ref().map(|gst| gst.h264);
    if let Some(file) = &opts.cache_file {
        write_cache(
            file,
            &CacheFile {
                key,
                ffmpeg: ffmpeg_caps.clone(),
                vaapi: vaapi.clone(),
                gst,
                export_driver,
            },
        );
    }
    Detected {
        caps: Capabilities {
            ffmpeg: Some(ffmpeg_caps),
            ffprobe,
            vaapi,
            gst_h264,
            cores,
        },
        notes,
        cached: false,
    }
}

/// The libva driver VA exports load on the node where only Intel's other
/// driver passed the h264_vaapi check (`capabilities.json`'s
/// `exportDriver`), whatever the rest of the file says:
/// `gpu::export_libva_driver_for` reads it when an export starts.
pub fn read_export_driver(cache_file: &Path) -> Option<VaDriverPick> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Pick {
        #[serde(default)]
        export_driver: Option<VaDriverPick>,
    }
    // The file is Qrec's own and small: anything bigger isn't read.
    if fs::metadata(cache_file).ok()?.len() > 1 << 20 {
        return None;
    }
    let bytes = fs::read(cache_file).ok()?;
    serde_json::from_slice::<Pick>(&bytes).ok()?.export_driver
}

/// Qrec's libva driver files as the cache key holds them: each one's
/// resolved path (a NixOS update changes it) and modification time (an
/// update in place changes that).
fn driver_stamps(drivers: &gpu::VaDrivers) -> BTreeMap<String, DriverStamp> {
    drivers
        .found
        .iter()
        .map(|(name, file)| {
            let path = fs::canonicalize(file).unwrap_or_else(|_| file.clone());
            let stamp = DriverStamp {
                mtime_ns: fs::metadata(&path).ok().as_ref().and_then(mtime_ns),
                path: path.to_string_lossy().into_owned(),
            };
            (name.clone(), stamp)
        })
        .collect()
}

/// What a GStreamer probe answered, to keep: its VA factories go into
/// `vaapi`, and the cache entry for its answers under `env`'s key as it is
/// now (after the probe, whose GStreamer init may have rewritten the
/// registry). Nothing when it couldn't answer, so the next host asks again.
fn remember_gst(found: &GstFound, vaapi: Option<&mut VaapiCaps>, env: &GstEnv) -> Option<GstCache> {
    let h264 = found.h264?;
    if let Some(vaapi) = vaapi {
        vaapi.gst_h264_factory = found.va_factory.clone();
        vaapi.gst_h264_lp_factory = found.va_lp_factory.clone();
    }
    Some(GstCache {
        key: env.key(),
        h264,
    })
}

/// The log of one detection's probes (`<state>/logs/<stamp>-<pid>-caps.log`),
/// created when the first probe runs.
struct ProbeLog<'a> {
    dir: Option<&'a Path>,
    created: Option<Option<(fs::File, PathBuf)>>,
}

impl<'a> ProbeLog<'a> {
    fn new(dir: Option<&'a Path>) -> Self {
        Self { dir, created: None }
    }

    fn get(&mut self) -> Option<&(fs::File, PathBuf)> {
        let dir = self.dir;
        self.created
            .get_or_insert_with(|| {
                let stamp = proc::log_stamp(chrono::Local::now());
                proc::create_log(dir?, &format!("{stamp}-{}-caps", std::process::id())).ok()
            })
            .as_ref()
    }

    /// A handle for a child's stderr.
    fn file(&mut self) -> Option<fs::File> {
        self.get().and_then(|(file, _)| file.try_clone().ok())
    }

    fn path(&mut self) -> Option<&Path> {
        self.get().map(|(_, path)| path.as_path())
    }
}

/// A file's modification time in ns since the epoch.
fn mtime_ns(meta: &fs::Metadata) -> Option<u128> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
}

/// The exact VA export chain of A§9.3 on two frames, as the core builds
/// it (`media_argv::vaapi_check_argv`, which `qrec doctor` runs too),
/// without the program name: the caller runs the resolved ffmpeg.
pub fn va_probe_args(device: &str) -> Vec<String> {
    qrec_core::media_argv::vaapi_check_argv(device)
        .into_iter()
        .skip(1)
        .collect()
}

/// `ffmpeg version 8.1.2 Copyright …` → `8.1.2`.
pub fn parse_version(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("ffmpeg version "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
}

/// The encoder names of `ffmpeg -encoders` (the lines after ` ------`).
pub fn parse_encoders(text: &str) -> BTreeSet<String> {
    text.lines()
        .skip_while(|line| line.trim() != "------")
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            let name = parts.next()?;
            (flags.len() == 6 && flags.starts_with(['V', 'A', 'S'])).then(|| name.to_owned())
        })
        .collect()
}

/// `program` resolved like `exec` would, then canonicalized.
fn resolve_program(program: &std::ffi::OsStr) -> Option<PathBuf> {
    let found = which::which(program).ok()?;
    Some(fs::canonicalize(&found).unwrap_or(found))
}

// ------------------------------------------------------------------ cache

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheKey {
    ffmpeg: String,
    ffmpeg_len: u64,
    ffmpeg_mtime_ns: u128,
    va_device: Option<String>,
    va_vendor: Option<u16>,
    /// The node's PCI device id: another GPU behind that node.
    va_device_id: Option<u16>,
    probed_va: bool,
    /// This process's `LIBVA_DRIVER_NAME`.
    libva_driver: Option<String>,
    /// Qrec's libva driver files, by name ([`driver_stamps`]).
    libva_drivers: BTreeMap<String, DriverStamp>,
}

/// A libva driver file, as the cache key holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriverStamp {
    path: String,
    mtime_ns: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheFile {
    key: CacheKey,
    ffmpeg: FfmpegCaps,
    vaapi: Option<VaapiCaps>,
    /// GStreamer's answers (with `vaapi`'s factories): `None` until a host
    /// that records asked, and in caches from before them.
    #[serde(default)]
    gst: Option<GstCache>,
    /// The driver exports load on the node where only Intel's other
    /// driver passed the h264_vaapi check ([`read_export_driver`]).
    #[serde(default)]
    export_driver: Option<VaDriverPick>,
}

/// GStreamer's software H.264 encoders, and the GStreamer they were found
/// in (`vaapi`'s factories were found with them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GstCache {
    key: GstKey,
    h264: GstH264Caps,
}

/// [`GstEnv::key`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GstKey {
    plugin_paths: Vec<Option<String>>,
    registry: Option<String>,
    registry_len: Option<u64>,
    registry_mtime_ns: Option<u128>,
}

fn read_cache(path: &Path, key: &CacheKey) -> Option<CacheFile> {
    let bytes = fs::read(path).ok()?;
    let file: CacheFile = serde_json::from_slice(&bytes).ok()?;
    (file.key == *key).then_some(file)
}

fn write_cache(path: &Path, file: &CacheFile) {
    let Ok(bytes) = serde_json::to_vec_pretty(file) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Err(e) = fsx::replace(path, &bytes, fsx::Sync::None) {
        tracing::debug!("can't write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::gpu::{VENDOR_AMD, VENDOR_INTEL};

    const ENCODERS: &str = "Encoders:\n V..... = Video\n A..... = Audio\n ------\n V....D libx264              libx264 H.264\n V....D h264_vaapi           H.264/AVC (VAAPI) (codec h264)\n VF...D png                  PNG image\n A....D aac                  AAC\n";

    /// A fake ffmpeg: canned `-version`/`-encoders`; the VA check records
    /// `LIBVA_DRIVER_NAME` and its arguments (the last run's in `va-probe`,
    /// each run's driver in `va-runs`), and fails when `va-fails` exists,
    /// or `va-fails-<driver>` for its driver.
    fn fake_ffmpeg(dir: &Path) -> PathBuf {
        let script = dir.join("ffmpeg");
        let body = format!(
            r#"#!/bin/sh
echo run >> "{dir}/runs"
case "$*" in
  *-version*) echo "ffmpeg version 9.9-test Copyright (c) the FFmpeg developers"; echo "built with tests" ;;
  *-encoders*) printf '{encoders}' ;;
  *h264_vaapi*)
    echo "$LIBVA_DRIVER_NAME $*" > "{dir}/va-probe"
    echo "$LIBVA_DRIVER_NAME" >> "{dir}/va-runs"
    if [ -e "{dir}/va-fails" ] || [ -e "{dir}/va-fails-$LIBVA_DRIVER_NAME" ]; then exit 1; fi
    exit 0 ;;
esac
"#,
            dir = dir.display(),
            encoders = ENCODERS.replace('\n', "\\n"),
        );
        fs::write(&script, body).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn fake_sysfs(dir: &Path, nodes: &[(&str, &str)]) -> PathBuf {
        let sys = dir.join("sys");
        for (name, vendor) in nodes {
            fs::create_dir_all(sys.join(name).join("device")).unwrap();
            fs::write(sys.join(name).join("device/vendor"), format!("{vendor}\n")).unwrap();
        }
        fs::create_dir_all(sys.join("card0")).unwrap();
        sys
    }

    /// A node's PCI device id in a fake sysfs.
    fn fake_device_id(sys: &Path, name: &str, device_id: &str) {
        fs::write(
            sys.join(name).join("device/device"),
            format!("{device_id}\n"),
        )
        .unwrap();
    }

    /// A libva driver directory with these drivers' files.
    fn fake_drivers(dir: &Path, names: &[&str]) -> gpu::VaDrivers {
        let dri = dir.join("dri");
        fs::create_dir_all(&dri).unwrap();
        for name in names {
            fs::write(dri.join(format!("{name}_drv_video.so")), "").unwrap();
        }
        gpu::VaDrivers::new(Some(&dri.to_string_lossy()))
    }

    fn runs(dir: &Path) -> usize {
        fs::read_to_string(dir.join("runs")).map_or(0, |s| s.lines().count())
    }

    /// An executable `ffprobe` that does nothing.
    fn fake_ffprobe(dir: &Path) -> PathBuf {
        let script = dir.join("ffprobe");
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn options(dir: &Path, sys: PathBuf) -> CapsOptions {
        CapsOptions {
            ffmpeg: fake_ffmpeg(dir).into_os_string(),
            ffprobe: fake_ffprobe(dir).into_os_string(),
            sys_class_drm: sys,
            dev_dri: "/dev/dri".into(),
            probe_va: true,
            cache_file: Some(dir.join("cache/capabilities.json")),
            log_dir: Some(dir.join("logs")),
            gst_probe: None,
            gst_env: GstEnv::default(),
            va_drivers: gpu::VaDrivers::default(),
            libva_driver: None,
        }
    }

    #[test]
    fn detects_ffmpeg_and_the_intel_node_never_nvidia() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = fake_sysfs(
            tmp.path(),
            &[("renderD128", "0x10de"), ("renderD129", "0x8086")],
        );
        let opts = options(tmp.path(), sys);
        let found = detect(&opts, &VaConfig::default());
        assert!(!found.cached);
        let ffmpeg = found.caps.ffmpeg.as_ref().unwrap();
        assert_eq!(ffmpeg.version, "9.9-test");
        assert_eq!(
            ffmpeg.identity,
            fs::canonicalize(&opts.ffmpeg).unwrap().to_string_lossy()
        );
        assert_eq!(
            ffmpeg
                .encoders
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["aac", "h264_vaapi", "libx264", "png"]
        );
        let va = found.caps.vaapi.as_ref().unwrap();
        assert_eq!(
            (va.device.as_str(), va.vendor, va.ffmpeg_h264),
            ("/dev/dri/renderD129", VENDOR_INTEL, true)
        );
        assert_eq!(va.gst_h264_factory, None);
        let probe = fs::read_to_string(tmp.path().join("va-probe")).unwrap();
        assert!(probe.starts_with("iHD "), "{probe}");
        assert!(
            probe.contains("-init_hw_device vaapi=va:/dev/dri/renderD129"),
            "{probe}"
        );
        assert!(found.caps.cores >= 1);
        assert_eq!(runs(tmp.path()), 3);
        let logs = fs::read_dir(tmp.path().join("logs")).unwrap().count();
        assert_eq!(logs, 1, "one log for the three probes");

        assert_eq!(
            found.caps.ffprobe.as_deref(),
            Some(
                fs::canonicalize(&opts.ffprobe)
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            )
        );

        // Nothing changed: the cache answers without running anything.
        let again = detect(&opts, &VaConfig::default());
        assert!(again.cached);
        assert_eq!(again.caps, found.caps);
        assert_eq!(runs(tmp.path()), 3);

        // ffprobe is looked up every time, cache or not (exports need it).
        let mut no_probe = opts.clone();
        no_probe.ffprobe = "qrec-no-such-ffprobe".into();
        let missing = detect(&no_probe, &VaConfig::default());
        assert!(missing.cached);
        assert_eq!(missing.caps.ffprobe, None);
        assert!(
            missing
                .notes
                .iter()
                .any(|n| n.contains("qrec-no-such-ffprobe")),
            "{:?}",
            missing.notes
        );

        // A configured NVIDIA node changes the key but is never probed
        // (NVIDIA has no VA-API encoder, A§8.10): VA-API is off.
        let nvidia = VaConfig {
            project: None,
            config: Some("/dev/dri/renderD128".into()),
        };
        let before = runs(tmp.path());
        let chosen = detect(&opts, &nvidia);
        assert!(!chosen.cached);
        assert_eq!(chosen.caps.vaapi, None);
        assert!(
            chosen
                .notes
                .iter()
                .any(|n| n.contains("NVIDIA node: NVIDIA has no VA-API encoder")),
            "{:?}",
            chosen.notes
        );
        assert_eq!(runs(tmp.path()), before + 2, "-version and -encoders only");
        let probe = fs::read_to_string(tmp.path().join("va-probe")).unwrap();
        assert!(probe.contains("renderD129"), "only the Intel node: {probe}");
        // Nor does GStreamer's probe get it.
        let mut recording = opts.clone();
        recording.gst_probe = Some(fedora_probe);
        let asked = detect(&recording, &nvidia);
        assert_eq!(asked.caps.vaapi, None);
        assert_eq!(gst_probes(tmp.path()), ["gst probe None"]);
    }

    /// The checks load the node's libva driver: this process's when it
    /// fits the node (main() pinned it), else Intel's i965 on the chips
    /// only it drives or where only it is installed, else iHD. Installing
    /// or updating a driver probes again.
    #[test]
    fn the_checks_load_the_nodes_libva_driver() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = fake_sysfs(tmp.path(), &[("renderD128", "0x8086")]);
        fake_device_id(&sys, "renderD128", "0x0416");
        let mut opts = options(tmp.path(), sys.clone());
        let va_probe = || fs::read_to_string(tmp.path().join("va-probe")).unwrap();
        // Haswell: i965, whatever is installed.
        opts.va_drivers = fake_drivers(&tmp.path().join("both"), &["iHD", "i965"]);
        detect(&opts, &VaConfig::default());
        assert!(va_probe().starts_with("i965 "), "{}", va_probe());
        // A chip iHD drives with only i965 installed: i965.
        fake_device_id(&sys, "renderD128", "0x9a49");
        opts.va_drivers = fake_drivers(&tmp.path().join("i965"), &["i965"]);
        let found = detect(&opts, &VaConfig::default());
        assert!(!found.cached, "another GPU and other driver files");
        assert!(va_probe().starts_with("i965 "), "{}", va_probe());
        // iHD installed meanwhile: probed again, with it.
        fs::write(tmp.path().join("i965/dri/iHD_drv_video.so"), "").unwrap();
        opts.va_drivers = gpu::VaDrivers::new(Some(&tmp.path().join("i965/dri").to_string_lossy()));
        assert!(!detect(&opts, &VaConfig::default()).cached);
        assert!(va_probe().starts_with("iHD "), "{}", va_probe());
        assert!(detect(&opts, &VaConfig::default()).cached);
        // This process's i965 (main() pinned it) fits Intel: kept.
        opts.libva_driver = Some("i965".into());
        assert!(!detect(&opts, &VaConfig::default()).cached);
        assert!(va_probe().starts_with("i965 "), "{}", va_probe());
        // Another vendor's: the node's own.
        opts.libva_driver = Some("radeonsi".into());
        detect(&opts, &VaConfig::default());
        assert!(va_probe().starts_with("iHD "), "{}", va_probe());
    }

    /// Where the h264_vaapi check fails with the node's driver on an Intel
    /// node, it runs again with Intel's other one, installed and driving
    /// the chip; when only that one passes, exports load it there: the
    /// cache keeps the pick, for that GPU, until a check runs again.
    /// GStreamer is asked once, with the node's driver (its VA plugin has
    /// no encoder with i965). Both failing, or the node's passing, leave no
    /// pick.
    #[test]
    fn exports_load_intels_other_driver_where_only_it_encodes() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = fake_sysfs(tmp.path(), &[("renderD128", "0x8086")]);
        // Kaby Lake with Ubuntu's free iHD build, which encodes no H.264.
        fake_device_id(&sys, "renderD128", "0x5916");
        let mut opts = options(tmp.path(), sys.clone());
        opts.va_drivers = fake_drivers(tmp.path(), &["iHD", "i965"]);
        opts.libva_driver = Some("iHD".into());
        opts.gst_probe = Some(fedora_probe);
        fs::write(tmp.path().join("va-fails-iHD"), "").unwrap();
        let file = opts.cache_file.clone().unwrap();
        let va_runs = || -> Vec<String> {
            let runs = tmp.path().join("va-runs");
            let text = fs::read_to_string(&runs).unwrap_or_default();
            let _ = fs::remove_file(&runs);
            text.lines().map(str::to_owned).collect()
        };
        let noted = |found: &Detected, note: &str| {
            assert!(
                found.notes.iter().any(|n| n == note),
                "{note:?} isn't in {:?}",
                found.notes
            );
        };
        // The file's key forgotten: the checks run again.
        let forget_checks = || {
            let mut json: serde_json::Value =
                serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
            json.as_object_mut().unwrap().remove("key");
            fs::write(&file, serde_json::to_vec(&json).unwrap()).unwrap();
        };

        let found = detect(&opts, &VaConfig::default());
        assert!(found.caps.vaapi.as_ref().unwrap().ffmpeg_h264);
        assert_eq!(va_runs(), ["iHD", "i965"]);
        let pick = VaDriverPick {
            device: "/dev/dri/renderD128".into(),
            device_id: Some(0x5916),
            driver: "i965".into(),
        };
        assert_eq!(read_export_driver(&file), Some(pick.clone()));
        noted(
            &found,
            "the h264_vaapi check on /dev/dri/renderD128 failed with iHD: ffmpeg exited with \
             status 1; it passed with i965, which exports load there",
        );
        assert_eq!(
            gst_probes(tmp.path()),
            [r#"gst probe Some("/dev/dri/renderD128")"#]
        );
        // From the cache: nothing runs again, the pick stays.
        assert!(detect(&opts, &VaConfig::default()).cached);
        assert!(va_runs().is_empty());
        assert_eq!(read_export_driver(&file), Some(pick));

        // Both fail: no pick, both noted.
        fs::write(tmp.path().join("va-fails"), "").unwrap();
        forget_checks();
        let failed = detect(&opts, &VaConfig::default());
        assert!(!failed.caps.vaapi.as_ref().unwrap().ffmpeg_h264);
        assert_eq!(va_runs(), ["iHD", "i965"]);
        assert_eq!(read_export_driver(&file), None);
        noted(
            &failed,
            "the h264_vaapi check on /dev/dri/renderD128 failed: ffmpeg exited with status 1; \
             with i965 too: ffmpeg exited with status 1",
        );
        fs::remove_file(tmp.path().join("va-fails")).unwrap();
        // iHD encodes after all (the full build): one check, no pick.
        fs::remove_file(tmp.path().join("va-fails-iHD")).unwrap();
        forget_checks();
        assert!(
            detect(&opts, &VaConfig::default())
                .caps
                .vaapi
                .unwrap()
                .ffmpeg_h264
        );
        assert_eq!(va_runs(), ["iHD"]);
        assert_eq!(read_export_driver(&file), None);

        // A Haswell, which only i965 drives: never iHD.
        fake_device_id(&sys, "renderD128", "0x0416");
        opts.libva_driver = Some("i965".into());
        fs::write(tmp.path().join("va-fails-i965"), "").unwrap();
        let haswell = detect(&opts, &VaConfig::default());
        assert!(!haswell.cached);
        assert!(!haswell.caps.vaapi.unwrap().ffmpeg_h264);
        assert_eq!(va_runs(), ["i965"]);
        assert_eq!(read_export_driver(&file), None);

        // A file that isn't a cache: no pick.
        fs::write(&file, "{not json").unwrap();
        assert_eq!(read_export_driver(&file), None);
    }

    /// A fake [`GstProbe`]: notes each run (and its node) in the caps log,
    /// and answers with factories named after the node, x264enc missing and
    /// openh264enc there.
    fn fedora_probe(device: Option<&str>, log: Option<&Path>) -> GstFound {
        let log = log.expect("the probe logs to the caps log");
        let mut file = fs::OpenOptions::new().append(true).open(log).unwrap();
        std::io::Write::write_all(&mut file, format!("gst probe {device:?}\n").as_bytes()).unwrap();
        GstFound {
            va_factory: device.map(|d| format!("va-for-{d}")),
            va_lp_factory: device.map(|d| format!("lp-for-{d}")),
            h264: Some(GstH264Caps {
                x264: false,
                openh264: true,
            }),
        }
    }

    /// Every probe run the caps logs note, in order.
    fn gst_probes(dir: &Path) -> Vec<String> {
        let mut lines = Vec::new();
        let mut logs: Vec<PathBuf> = fs::read_dir(dir.join("logs"))
            .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        logs.sort();
        for log in logs {
            let text = fs::read_to_string(log).unwrap();
            lines.extend(
                text.lines()
                    .filter(|l| l.starts_with("gst probe"))
                    .map(str::to_owned),
            );
        }
        lines
    }

    #[test]
    fn gstreamers_answers_are_asked_once_and_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = fake_sysfs(tmp.path(), &[("renderD129", "0x8086")]);
        let mut opts = options(tmp.path(), sys);
        let registry = tmp.path().join("registry.bin");
        fs::write(&registry, "v1").unwrap();
        opts.gst_env = GstEnv {
            plugin_paths: vec![Some("/plugins".into()), None, None, None],
            registry: Some(registry.clone()),
        };
        let plain = detect(&opts, &VaConfig::default());
        let va = plain.caps.vaapi.as_ref().unwrap();
        assert_eq!(
            (&va.gst_h264_factory, &va.gst_h264_lp_factory),
            (&None, &None)
        );
        assert_eq!(plain.caps.gst_h264, None, "nothing asked GStreamer");
        let ran = runs(tmp.path());

        // A host that records asks GStreamer: only that runs, with the node.
        opts.gst_probe = Some(fedora_probe);
        let recording = detect(&opts, &VaConfig::default());
        assert!(recording.cached);
        let fedora = Some(GstH264Caps {
            x264: false,
            openh264: true,
        });
        assert_eq!(recording.caps.gst_h264, fedora);
        let va = recording.caps.vaapi.as_ref().unwrap();
        assert_eq!(
            (
                va.gst_h264_factory.as_deref(),
                va.gst_h264_lp_factory.as_deref()
            ),
            (
                Some("va-for-/dev/dri/renderD129"),
                Some("lp-for-/dev/dri/renderD129")
            )
        );
        assert_eq!(runs(tmp.path()), ran, "ffmpeg isn't asked again");
        assert_eq!(
            gst_probes(tmp.path()),
            [r#"gst probe Some("/dev/dri/renderD129")"#]
        );

        // Then nobody asks again while GStreamer is the same, and hosts that
        // don't record get its answers from the cache too.
        assert_eq!(detect(&opts, &VaConfig::default()).caps, recording.caps);
        opts.gst_probe = None;
        assert_eq!(detect(&opts, &VaConfig::default()).caps, recording.caps);
        assert_eq!(gst_probes(tmp.path()).len(), 1);

        // Another plugin path, or a rewritten registry (plugins came or
        // went), asks again, and nothing else.
        opts.gst_probe = Some(fedora_probe);
        opts.gst_env.plugin_paths[0] = Some("/elsewhere".into());
        assert!(detect(&opts, &VaConfig::default()).cached);
        assert_eq!(gst_probes(tmp.path()).len(), 2);
        fs::write(&registry, "v2 with more plugins").unwrap();
        detect(&opts, &VaConfig::default());
        assert_eq!(gst_probes(tmp.path()).len(), 3);
        detect(&opts, &VaConfig::default());
        assert_eq!(gst_probes(tmp.path()).len(), 3);
        assert_eq!(runs(tmp.path()), ran, "ffmpeg isn't asked again");

        // A probe that couldn't answer isn't remembered: the next host asks.
        opts.gst_env.plugin_paths[0] = Some("/broken".into());
        opts.gst_probe = Some(|_, _| GstFound::default());
        let failed = detect(&opts, &VaConfig::default());
        assert_eq!(failed.caps.gst_h264, None);
        opts.gst_probe = Some(fedora_probe);
        assert_eq!(detect(&opts, &VaConfig::default()).caps.gst_h264, fedora);
        assert_eq!(gst_probes(tmp.path()).len(), 4);

        // Without the VA checks (the fake backend), GStreamer is asked for
        // its software encoders alone, in the same detection as ffmpeg.
        opts.probe_va = false;
        let fake = detect(&opts, &VaConfig::default());
        assert!(!fake.cached);
        assert_eq!(fake.caps.gst_h264, fedora);
        let va = fake.caps.vaapi.as_ref().unwrap();
        assert_eq!(
            (&va.gst_h264_factory, &va.gst_h264_lp_factory),
            (&None, &None)
        );
        assert_eq!(gst_probes(tmp.path()).last().unwrap(), "gst probe None");
        let logs = fs::read_dir(tmp.path().join("logs")).unwrap().count();
        assert!(logs >= 2, "each detection that probes has its log");
    }

    /// A cache written before GStreamer's software encoders and low-power
    /// factories (its `gstProbed`, no `gst`) still answers for ffmpeg; a
    /// host that records asks GStreamer alone.
    #[test]
    fn caches_from_before_the_encoder_chain_still_load() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = fake_sysfs(tmp.path(), &[("renderD129", "0x8086")]);
        let mut opts = options(tmp.path(), sys);
        detect(&opts, &VaConfig::default());
        let file = opts.cache_file.clone().unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        let cache = json.as_object_mut().unwrap();
        assert!(cache.contains_key("gst"));
        cache.remove("gst");
        cache.insert("gstProbed".into(), true.into());
        let va = cache["vaapi"].as_object_mut().unwrap();
        va.remove("gstH264LpFactory");
        va.insert("gstH264Factory".into(), "vah264enc".into());
        fs::write(&file, serde_json::to_vec(&json).unwrap()).unwrap();
        let ran = runs(tmp.path());

        let old = detect(&opts, &VaConfig::default());
        assert!(old.cached);
        let va = old.caps.vaapi.as_ref().unwrap();
        assert_eq!(
            (
                va.gst_h264_factory.as_deref(),
                va.gst_h264_lp_factory.as_deref()
            ),
            (Some("vah264enc"), None)
        );
        assert_eq!(old.caps.gst_h264, None);

        opts.gst_probe = Some(fedora_probe);
        let asked = detect(&opts, &VaConfig::default());
        assert!(asked.cached);
        assert_eq!(asked.caps.gst_h264.map(|g| g.openh264), Some(true));
        assert_eq!(runs(tmp.path()), ran, "ffmpeg isn't asked again");
        assert_eq!(gst_probes(tmp.path()).len(), 1);
    }

    #[test]
    fn failures_and_switches() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = fake_sysfs(tmp.path(), &[("renderD128", "0x10de")]);
        let mut opts = options(tmp.path(), sys);
        opts.cache_file = None;
        let only_nvidia = detect(&opts, &VaConfig::default());
        assert_eq!(
            only_nvidia.caps.vaapi, None,
            "NVIDIA is never picked automatically"
        );

        let sys = fake_sysfs(tmp.path(), &[("renderD130", "0x1002")]);
        opts.sys_class_drm = sys;
        fs::write(tmp.path().join("va-fails"), "").unwrap();
        let failed = detect(&opts, &VaConfig::default());
        let va = failed.caps.vaapi.unwrap();
        assert_eq!((va.vendor, va.ffmpeg_h264), (VENDOR_AMD, false));
        assert!(
            failed.notes.iter().any(|n| n.contains("h264_vaapi check")),
            "{:?}",
            failed.notes
        );
        assert!(
            fs::read_to_string(tmp.path().join("va-probe"))
                .unwrap()
                .starts_with("radeonsi ")
        );

        opts.probe_va = false;
        let before = runs(tmp.path());
        let skipped = detect(&opts, &VaConfig::default());
        assert!(!skipped.caps.vaapi.unwrap().ffmpeg_h264);
        assert_eq!(runs(tmp.path()), before + 2, "no VA check");

        let unknown = detect(
            &opts,
            &VaConfig {
                project: Some("/dev/dri/renderD199".into()),
                config: None,
            },
        );
        assert_eq!(unknown.caps.vaapi, None);

        opts.ffmpeg = "qrec-no-such-ffmpeg".into();
        let missing = detect(&opts, &VaConfig::default());
        assert_eq!((missing.caps.ffmpeg, missing.caps.vaapi), (None, None));
        assert!(
            missing.caps.ffprobe.is_some(),
            "ffprobe is looked up anyway"
        );
        assert!(missing.notes[0].contains("not installed"));
    }

    #[test]
    fn the_real_ffmpeg_without_touching_a_gpu() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = CapsOptions {
            sys_class_drm: tmp.path().join("no-sysfs"),
            cache_file: None,
            log_dir: Some(tmp.path().join("logs")),
            ..CapsOptions::system(None, None)
        };
        let found = detect(&opts, &VaConfig::default());
        let ffmpeg = found.caps.ffmpeg.expect("the dev shell has ffmpeg");
        assert!(
            ffmpeg.version.starts_with(char::is_numeric),
            "{}",
            ffmpeg.version
        );
        for encoder in ["libx264", "libvpx-vp9", "aac", "libopus", "png"] {
            assert!(ffmpeg.encoders.contains(encoder), "{encoder} missing");
        }
        assert!(Path::new(&ffmpeg.identity).is_absolute());
        assert_eq!(found.caps.vaapi, None);
    }

    #[test]
    fn parsers() {
        assert_eq!(
            parse_version("ffmpeg version 8.1.2 Copyright (c) 2000-2026").as_deref(),
            Some("8.1.2")
        );
        assert_eq!(parse_version("nope"), None);
        assert_eq!(parse_encoders(ENCODERS).len(), 4);
        assert!(
            parse_encoders("V....D x\n").is_empty(),
            "nothing before the separator counts"
        );
        let args = va_probe_args("/dev/dri/renderD129");
        assert!(args.windows(2).any(|w| w == ["-c:v", "h264_vaapi"]));
        assert_eq!(args.last().unwrap(), "-");
    }
}
