//! GPU safety (A§8.10, A§2.1).
//!
//! VA-API must never pick its own device: on the dev machine NVIDIA's
//! `renderD128` hangs anything that initializes it, including GStreamer's
//! plugin scan. So, before any thread starts, `main()` calls
//! [`prepare_gpu_env`], which
//!
//! 1. resolves the effective VA render node (project `recording.vaapiDevice`
//!    → config `encoder.vaapi_device` → the first Intel (`0x8086`) or AMD
//!    (`0x1002`) node; never NVIDIA automatically),
//! 2. sets `LIBVA_DRIVER_NAME` from that node's PCI vendor (`iHD`,
//!    `radeonsi`), overriding an inherited mismatching value, and leaves it
//!    alone only when an NVIDIA node is explicitly configured,
//! 3. for the GUI, when an NVIDIA device exists and isn't the chosen one,
//!    points `__EGL_VENDOR_LIBRARY_FILENAMES` at Mesa and
//!    `VK_DRIVER_FILES`/`VK_ICD_FILENAMES` at the chosen vendor's Vulkan
//!    driver, unless the user set them.
//!
//! The decision is pure ([`plan_gpu_env`]); applying it uses
//! `std::env::set_var`, which is only sound while the process has a single
//! thread (edition 2024 makes it `unsafe`). [`prepare_gpu_env`] checks
//! `/proc/self/task` and refuses to touch the environment once another
//! thread exists, so call it first thing in `main()`. The report is kept for
//! `qrec doctor` ([`startup_report`]).
//!
//! Probes that touch a render node run in a child process with a 10 s
//! deadline ([`run_probe_child`]): the child is the `qrec` binary itself,
//! whose `main()` starts with [`run_probe_if_requested`]. Today that is the
//! VA H.264 encoder lookup for `VaapiCaps.gst_h264_factory`
//! ([`va_h264_factory_for`], by `device-path`, never by name).
//!
//! ```ignore
//! fn main() {
//!     qrec_shell::gpu::run_probe_if_requested(); // exits when it is a probe child
//!     let args: Vec<_> = std::env::args_os().collect();
//!     let report = qrec_shell::gpu::prepare_gpu_env(&GpuInputs::discover(&args, is_gui));
//!     // … only now threads, GTK, GStreamer
//! }
//! ```

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;

/// PCI vendor ids.
pub const VENDOR_INTEL: u16 = 0x8086;
pub const VENDOR_AMD: u16 = 0x1002;
pub const VENDOR_NVIDIA: u16 = 0x10de;

/// The variable libva reads to pick its driver.
pub const LIBVA_DRIVER_NAME: &str = "LIBVA_DRIVER_NAME";
/// glvnd's EGL vendor override.
pub const EGL_VENDOR_FILES: &str = "__EGL_VENDOR_LIBRARY_FILENAMES";
/// The Vulkan loader's driver list (current name).
pub const VK_DRIVER_FILES: &str = "VK_DRIVER_FILES";
/// The Vulkan loader's driver list (legacy name).
pub const VK_ICD_FILENAMES: &str = "VK_ICD_FILENAMES";

/// A DRM render node (`/dev/dri/renderD129`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderNode {
    pub path: String,
    /// PCI vendor id from sysfs; `None` when unreadable.
    pub vendor: Option<u16>,
    /// Kernel driver (`i915`, `xe`, `amdgpu`, `nvidia`), from sysfs.
    pub driver: Option<String>,
}

/// `Intel`, `AMD`, `NVIDIA` or the hex id.
pub fn vendor_name(vendor: u16) -> String {
    match vendor {
        VENDOR_INTEL => "Intel".into(),
        VENDOR_AMD => "AMD".into(),
        VENDOR_NVIDIA => "NVIDIA".into(),
        other => format!("{other:#06x}"),
    }
}

/// The libva driver for a vendor: `iHD` for Intel, `radeonsi` for AMD.
pub fn libva_driver_for(vendor: u16) -> Option<&'static str> {
    match vendor {
        VENDOR_INTEL => Some("iHD"),
        VENDOR_AMD => Some("radeonsi"),
        _ => None,
    }
}

/// Whether an inherited `LIBVA_DRIVER_NAME` already fits the vendor (the
/// legacy drivers count: `i965` for old Intel, `r600` for old AMD).
fn driver_fits_vendor(driver: &str, vendor: u16) -> bool {
    match vendor {
        VENDOR_INTEL => matches!(driver, "iHD" | "i965"),
        VENDOR_AMD => matches!(driver, "radeonsi" | "r600"),
        _ => false,
    }
}

/// The render nodes listed in `sys_class_drm` (`/sys/class/drm`), as paths
/// under `dev_dri` (`/dev/dri`), sorted by node number. Never opens a device.
pub fn scan_render_nodes(sys_class_drm: &Path, dev_dri: &Path) -> Vec<RenderNode> {
    let Ok(entries) = fs::read_dir(sys_class_drm) else {
        return Vec::new();
    };
    let mut nodes: Vec<(u32, RenderNode)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let number: u32 = name.strip_prefix("renderD")?.parse().ok()?;
            let device = entry.path().join("device");
            let vendor = fs::read_to_string(device.join("vendor"))
                .ok()
                .and_then(|text| parse_hex_u16(text.trim()));
            let driver = fs::read_link(device.join("driver"))
                .ok()
                .and_then(|link| link.file_name().map(|n| n.to_string_lossy().into_owned()));
            let path = dev_dri.join(&name).to_string_lossy().into_owned();
            Some((
                number,
                RenderNode {
                    path,
                    vendor,
                    driver,
                },
            ))
        })
        .collect();
    nodes.sort_by_key(|(number, _)| *number);
    nodes.into_iter().map(|(_, node)| node).collect()
}

/// The machine's render nodes (`/sys/class/drm/renderD*`).
pub fn render_nodes() -> Vec<RenderNode> {
    scan_render_nodes(Path::new("/sys/class/drm"), Path::new("/dev/dri"))
}

fn parse_hex_u16(text: &str) -> Option<u16> {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    u16::from_str_radix(digits, 16).ok()
}

/// Where the effective VA device came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceOrigin {
    /// The project's `recording.vaapiDevice`.
    Project,
    /// The user config's `encoder.vaapi_device`.
    Config,
    /// The first Intel or AMD render node.
    Auto,
}

/// The effective VA render node (A§2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaChoice {
    /// The node as configured or found (`/dev/dri/renderD129`).
    pub device: String,
    /// Its PCI vendor, when the node is known.
    pub vendor: Option<u16>,
    pub origin: DeviceOrigin,
}

/// Resolves the effective VA render node: the project's device, else the
/// config's, else the first Intel or AMD node. NVIDIA is used only when
/// configured explicitly; `None` = no usable node.
pub fn effective_va_device(
    project: Option<&str>,
    config: Option<&str>,
    nodes: &[RenderNode],
) -> Option<VaChoice> {
    let configured = |device: &str, origin| VaChoice {
        device: device.to_owned(),
        vendor: node_for(device, nodes).and_then(|n| n.vendor),
        origin,
    };
    fn non_empty(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    if let Some(device) = non_empty(project) {
        return Some(configured(device, DeviceOrigin::Project));
    }
    if let Some(device) = non_empty(config) {
        return Some(configured(device, DeviceOrigin::Config));
    }
    nodes
        .iter()
        .find(|n| matches!(n.vendor, Some(VENDOR_INTEL | VENDOR_AMD)))
        .map(|n| VaChoice {
            device: n.path.clone(),
            vendor: n.vendor,
            origin: DeviceOrigin::Auto,
        })
}

/// The node a configured path refers to: the same path, or the same file
/// after resolving symlinks (`/dev/dri/by-path/…-render`).
fn node_for<'a>(device: &str, nodes: &'a [RenderNode]) -> Option<&'a RenderNode> {
    if let Some(node) = nodes.iter().find(|n| n.path == device) {
        return Some(node);
    }
    let canonical = fs::canonicalize(device).ok()?;
    nodes
        .iter()
        .find(|n| fs::canonicalize(&n.path).ok().as_deref() == Some(canonical.as_path()))
}

/// One environment variable to set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvChange {
    pub key: String,
    pub value: String,
    /// The inherited value it replaces.
    pub previous: Option<String>,
}

/// What [`plan_gpu_env`] decided.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuEnvPlan {
    pub changes: Vec<EnvChange>,
    /// Human notes for logs and `doctor` (overrides, skipped steps).
    pub notes: Vec<String>,
}

/// The inputs of [`plan_gpu_env`], gathered by the caller.
pub struct GpuEnvInputs<'a> {
    pub choice: Option<&'a VaChoice>,
    pub nodes: &'a [RenderNode],
    /// The GUI renders with EGL/Vulkan; the CLI and headless hosts don't.
    pub for_gui: bool,
    /// Reads an inherited environment variable.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Mesa's glvnd EGL vendor file, if found.
    pub mesa_egl_vendor: Option<String>,
    /// The chosen vendor's Vulkan driver manifest, if found.
    pub vulkan_icd: Option<String>,
}

/// Decides which variables to set (pure; A§8.10).
pub fn plan_gpu_env(inputs: &GpuEnvInputs<'_>) -> GpuEnvPlan {
    let mut plan = GpuEnvPlan::default();
    let inherited = |key: &str| (inputs.env)(key).filter(|v| !v.is_empty());
    match inputs.choice {
        None => plan.notes.push(
            "no Intel or AMD render node: VA-API stays off, LIBVA_DRIVER_NAME untouched".into(),
        ),
        Some(choice) => match choice.vendor {
            None => plan.notes.push(format!(
                "can't read the PCI vendor of {}; LIBVA_DRIVER_NAME untouched",
                choice.device
            )),
            Some(VENDOR_NVIDIA) => plan.notes.push(format!(
                "{} is an NVIDIA node and was configured explicitly; LIBVA_DRIVER_NAME untouched",
                choice.device
            )),
            Some(vendor) => match libva_driver_for(vendor) {
                None => plan.notes.push(format!(
                    "no known libva driver for vendor {} ({}); LIBVA_DRIVER_NAME untouched",
                    vendor_name(vendor),
                    choice.device
                )),
                Some(driver) => {
                    let previous = inherited(LIBVA_DRIVER_NAME);
                    if previous
                        .as_deref()
                        .is_some_and(|p| driver_fits_vendor(p, vendor))
                    {
                        // Already right (or a legacy driver of the same vendor).
                    } else {
                        if let Some(previous) = &previous {
                            plan.notes.push(format!(
                                "overriding inherited LIBVA_DRIVER_NAME={previous}: {} is {}, so {driver}",
                                choice.device,
                                vendor_name(vendor)
                            ));
                        }
                        plan.changes.push(EnvChange {
                            key: LIBVA_DRIVER_NAME.into(),
                            value: driver.into(),
                            previous,
                        });
                    }
                }
            },
        },
    }

    let nvidia_present = inputs.nodes.iter().any(|n| n.vendor == Some(VENDOR_NVIDIA));
    let chosen_vendor = inputs.choice.and_then(|c| c.vendor);
    let pin_away_from_nvidia = inputs.for_gui
        && nvidia_present
        && matches!(chosen_vendor, Some(VENDOR_INTEL | VENDOR_AMD));
    if pin_away_from_nvidia {
        if inherited(EGL_VENDOR_FILES).is_some() {
            plan.notes
                .push(format!("{EGL_VENDOR_FILES} is set by the user; left alone"));
        } else if let Some(file) = &inputs.mesa_egl_vendor {
            plan.changes.push(EnvChange {
                key: EGL_VENDOR_FILES.into(),
                value: file.clone(),
                previous: None,
            });
        } else {
            plan.notes
                .push("Mesa's EGL vendor file wasn't found; EGL may pick the NVIDIA driver".into());
        }
        if inherited(VK_DRIVER_FILES).is_some() || inherited(VK_ICD_FILENAMES).is_some() {
            plan.notes.push(format!(
                "{VK_DRIVER_FILES}/{VK_ICD_FILENAMES} are set by the user; left alone"
            ));
        } else if let Some(file) = &inputs.vulkan_icd {
            for key in [VK_DRIVER_FILES, VK_ICD_FILENAMES] {
                plan.changes.push(EnvChange {
                    key: key.into(),
                    value: file.clone(),
                    previous: None,
                });
            }
        } else {
            plan.notes.push(
                "the chosen GPU's Vulkan driver manifest wasn't found; Vulkan may pick the NVIDIA driver"
                    .into(),
            );
        }
    }
    plan
}

/// Directories holding `glvnd/` and `vulkan/` data: NixOS's driver dir,
/// `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`) and `/etc`.
pub fn graphics_data_dirs(xdg_data_dirs: Option<&str>) -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/run/opengl-driver/share")];
    let xdg = xdg_data_dirs
        .filter(|s| !s.is_empty())
        .unwrap_or("/usr/local/share:/usr/share");
    dirs.extend(
        xdg.split(':')
            .filter(|d| d.starts_with('/'))
            .map(PathBuf::from),
    );
    dirs.push(PathBuf::from("/etc"));
    dirs.dedup();
    dirs
}

/// `<dir>/glvnd/egl_vendor.d/*mesa*.json` in the first dir that has one.
pub fn find_mesa_egl_vendor(data_dirs: &[PathBuf]) -> Option<String> {
    data_dirs.iter().find_map(|dir| {
        json_files(&dir.join("glvnd/egl_vendor.d"))
            .into_iter()
            .find(|name| name.to_ascii_lowercase().contains("mesa"))
            .map(|name| {
                dir.join("glvnd/egl_vendor.d")
                    .join(name)
                    .to_string_lossy()
                    .into_owned()
            })
    })
}

/// `<dir>/vulkan/icd.d/<vendor manifest>` in the first dir that has one:
/// `intel_icd*.json` (before `intel_hasvk_icd*`) or `radeon_icd*.json`.
pub fn find_vulkan_icd(data_dirs: &[PathBuf], vendor: u16) -> Option<String> {
    let prefixes: &[&str] = match vendor {
        VENDOR_INTEL => &["intel_icd", "intel_hasvk_icd"],
        VENDOR_AMD => &["radeon_icd"],
        _ => return None,
    };
    prefixes.iter().find_map(|prefix| {
        data_dirs.iter().find_map(|dir| {
            let icd_dir = dir.join("vulkan/icd.d");
            json_files(&icd_dir)
                .into_iter()
                .find(|name| name.starts_with(prefix))
                .map(|name| icd_dir.join(name).to_string_lossy().into_owned())
        })
    })
}

fn json_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|name| name.ends_with(".json"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// What `main()` knows before parsing arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GpuInputs {
    /// The project's `recording.vaapiDevice`.
    pub project_device: Option<String>,
    /// The user config's `encoder.vaapi_device`.
    pub config_device: Option<String>,
    /// Also pin EGL and Vulkan (the GUI).
    pub for_gui: bool,
}

impl GpuInputs {
    /// Reads the devices from the project named by `--project DIR` (or the
    /// current directory, when it holds `qrec.json`) and from the user
    /// config. Unreadable files count as "not configured".
    pub fn discover(args: &[OsString], for_gui: bool) -> Self {
        let cwd = std::env::current_dir().ok();
        let project_device =
            project_dir_from_args(args, cwd.as_deref()).and_then(|dir| project_va_device(&dir));
        let env = crate::paths::Env::from_process();
        let config_device = crate::paths::UserDirs::resolve(&env)
            .ok()
            .and_then(|dirs| config_va_device(&dirs.config_file()));
        Self {
            project_device,
            config_device,
            for_gui,
        }
    }
}

/// The project directory a command line refers to: `--project DIR` /
/// `--project=DIR` (relative to `cwd`), else `cwd` when it holds `qrec.json`.
pub fn project_dir_from_args(args: &[OsString], cwd: Option<&Path>) -> Option<PathBuf> {
    let mut iter = args.iter().skip(1);
    let mut explicit = None;
    while let Some(arg) = iter.next() {
        let Some(text) = arg.to_str() else { continue };
        if text == "--" {
            break;
        }
        if text == "--project" {
            explicit = iter.next().map(PathBuf::from);
        } else if let Some(value) = text.strip_prefix("--project=") {
            explicit = Some(PathBuf::from(value));
        }
    }
    match explicit {
        Some(dir) if dir.is_absolute() => Some(dir),
        Some(dir) => cwd.map(|cwd| cwd.join(dir)),
        None => cwd
            .filter(|cwd| cwd.join(qrec_core::PROJECT_FILE).is_file())
            .map(Path::to_path_buf),
    }
}

/// Files bigger than this aren't read here (a project file is small).
const MAX_FILE_BYTES: u64 = 64 << 20;

fn read_small(path: &Path) -> Option<Vec<u8>> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    fs::read(path).ok()
}

/// `recording.vaapiDevice` of `<dir>/qrec.json`, if set.
pub fn project_va_device(project_dir: &Path) -> Option<String> {
    let bytes = read_small(&project_dir.join(qrec_core::PROJECT_FILE))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("recording")?
        .get("vaapiDevice")?
        .as_str()
        .map(str::to_owned)
}

/// `encoder.vaapi_device` of a `config.toml`, if set.
///
/// A deliberately narrow reader (the shell has no TOML parser and this runs
/// before anything else): it understands `[encoder]` tables, dotted
/// `encoder.vaapi_device` keys, basic and literal strings and comments.
pub fn config_va_device(config_file: &Path) -> Option<String> {
    let bytes = read_small(config_file)?;
    toml_string_at(&String::from_utf8_lossy(&bytes), "encoder.vaapi_device")
}

fn toml_string_at(text: &str, dotted_key: &str) -> Option<String> {
    let normalize = |key: &str| -> String {
        key.split('.')
            .map(|part| part.trim().trim_matches('"').trim_matches('\''))
            .collect::<Vec<_>>()
            .join(".")
    };
    let mut table = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let header = line.split('#').next().unwrap_or(line).trim();
            let inner = header.trim_start_matches('[').trim_end_matches(']');
            table = normalize(inner);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = normalize(key);
        let full = if table.is_empty() {
            key
        } else {
            format!("{table}.{key}")
        };
        if full == dotted_key {
            return parse_toml_string(value.trim());
        }
    }
    None
}

fn parse_toml_string(value: &str) -> Option<String> {
    if let Some(rest) = value.strip_prefix('\'') {
        return rest.split_once('\'').map(|(s, _)| s.to_owned());
    }
    let rest = value.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

/// What [`prepare_gpu_env`] found and did, for logs and `qrec doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuReport {
    pub nodes: Vec<RenderNode>,
    pub choice: Option<VaChoice>,
    pub changes: Vec<EnvChange>,
    pub notes: Vec<String>,
    /// Whether `changes` were applied to this process's environment.
    pub applied: bool,
}

static STARTUP_REPORT: OnceLock<GpuReport> = OnceLock::new();

/// The report of the first [`prepare_gpu_env`] call in this process.
pub fn startup_report() -> Option<&'static GpuReport> {
    STARTUP_REPORT.get()
}

/// Resolves the VA device and sets the GPU variables (A§8.10).
///
/// **Call it at the very top of `main()`, before any thread, GTK or
/// GStreamer call.** Setting variables is only sound while the process has
/// one thread; when `/proc/self/task` shows more (or can't be read), nothing
/// is changed and the report says so (`applied: false`).
pub fn prepare_gpu_env(inputs: &GpuInputs) -> GpuReport {
    let nodes = render_nodes();
    let choice = effective_va_device(
        inputs.project_device.as_deref(),
        inputs.config_device.as_deref(),
        &nodes,
    );
    let data_dirs = graphics_data_dirs(std::env::var("XDG_DATA_DIRS").ok().as_deref());
    let chosen_vendor = choice.as_ref().and_then(|c| c.vendor);
    let env = |key: &str| std::env::var(key).ok();
    let plan = plan_gpu_env(&GpuEnvInputs {
        choice: choice.as_ref(),
        nodes: &nodes,
        for_gui: inputs.for_gui,
        env: &env,
        mesa_egl_vendor: find_mesa_egl_vendor(&data_dirs),
        vulkan_icd: chosen_vendor.and_then(|v| find_vulkan_icd(&data_dirs, v)),
    });
    let mut notes = plan.notes;
    let applied = match apply_changes(&plan.changes, thread_count()) {
        Ok(()) => true,
        Err(note) => {
            notes.push(note);
            false
        }
    };
    for note in &notes {
        tracing::info!("gpu: {note}");
    }
    let report = GpuReport {
        nodes,
        choice,
        changes: plan.changes,
        notes,
        applied,
    };
    let _ = STARTUP_REPORT.set(report.clone());
    report
}

/// Sets `changes` when `threads` says the process is single-threaded;
/// otherwise changes nothing and says why.
fn apply_changes(changes: &[EnvChange], threads: Option<usize>) -> Result<(), String> {
    if changes.is_empty() {
        return Ok(());
    }
    match threads {
        Some(1) => {
            for change in changes {
                // SAFETY: the process has exactly one thread (checked by the
                // caller just before), so no other thread can read or write
                // the environment concurrently.
                unsafe { std::env::set_var(&change.key, &change.value) };
            }
            Ok(())
        }
        other => Err(format!(
            "the GPU variables were not set: the process already runs {} threads",
            other.map_or_else(|| "an unknown number of".to_owned(), |n| n.to_string())
        )),
    }
}

/// The number of threads of this process (`/proc/self/task` entries).
fn thread_count() -> Option<usize> {
    fs::read_dir("/proc/self/task")
        .ok()
        .map(|entries| entries.count())
}

/// The `va*h264enc` factory whose `device-path` is `device` (A§8.10): VA
/// elements are registered per render node and the first usable node gets
/// the plain `vah264enc` name, so the name alone says nothing.
///
/// Reading `device-path` instantiates each candidate element but doesn't
/// open its device (the VA display is opened on the NULL→READY change).
/// Needs GStreamer initialized and `LIBVA_DRIVER_NAME` set first.
#[cfg(feature = "gst")]
pub fn va_h264_factory_for(device: &str) -> Option<String> {
    use gst::prelude::*;

    let wanted = fs::canonicalize(device).unwrap_or_else(|_| PathBuf::from(device));
    let mut names: Vec<String> = gst::Registry::get()
        .features(gst::ElementFactory::static_type())
        .into_iter()
        .map(|f| f.name().to_string())
        .filter(|name| name.starts_with("va") && name.ends_with("h264enc"))
        .collect();
    names.sort();
    names.into_iter().find(|name| {
        let Ok(element) = gst::ElementFactory::make(name).build() else {
            return false;
        };
        if element.find_property("device-path").is_none() {
            return false;
        }
        let path: Option<String> = element.property("device-path");
        path.is_some_and(|p| fs::canonicalize(&p).unwrap_or_else(|_| PathBuf::from(&p)) == wanted)
    })
}

// ------------------------------------------------------------ probe child

/// Set on a `qrec` child process to make it a GPU probe (A§8.10: probes
/// that touch a render node run as child processes with a 10 s timeout):
/// `va-h264-factory:<render node>`.
pub const PROBE_ENV: &str = "QREC_GPU_PROBE";

/// How long a probe child may run.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// A probe a child process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// [`va_h264_factory_for`] the node.
    VaH264Factory { device: String },
}

impl Probe {
    /// The [`PROBE_ENV`] value.
    pub fn env_value(&self) -> String {
        match self {
            Probe::VaH264Factory { device } => format!("va-h264-factory:{device}"),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.split_once(':')? {
            ("va-h264-factory", device) if !device.is_empty() => Some(Probe::VaH264Factory {
                device: device.to_owned(),
            }),
            _ => None,
        }
    }
}

/// What a probe child prints (one JSON line on stdout).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeAnswer {
    /// The VA H.264 encoder bound to the node, if any.
    pub factory: Option<String>,
    /// Why the probe couldn't answer.
    pub error: Option<String>,
}

/// **Call first thing in `main()`** (before [`prepare_gpu_env`]): when this
/// process was started as a probe child ([`PROBE_ENV`]), runs the probe,
/// prints its [`ProbeAnswer`] and exits; otherwise returns at once. The
/// parent already set the child's `LIBVA_DRIVER_NAME` for the probed node.
pub fn run_probe_if_requested() {
    let Some(value) = std::env::var_os(PROBE_ENV) else {
        return;
    };
    let (answer, code) = match Probe::parse(&value.to_string_lossy()) {
        Some(probe) => run_probe(&probe),
        None => (
            ProbeAnswer {
                factory: None,
                error: Some(format!("unknown probe {value:?}")),
            },
            2,
        ),
    };
    println!("{}", serde_json::to_string(&answer).unwrap_or_default());
    std::process::exit(code);
}

fn run_probe(probe: &Probe) -> (ProbeAnswer, i32) {
    match probe {
        #[cfg(feature = "gst")]
        Probe::VaH264Factory { device } => match crate::gst_init::init() {
            Ok(()) => (
                ProbeAnswer {
                    factory: va_h264_factory_for(device),
                    error: None,
                },
                0,
            ),
            Err(e) => (
                ProbeAnswer {
                    factory: None,
                    error: Some(e.to_string()),
                },
                crate::gst_init::EXIT_CODE,
            ),
        },
        #[cfg(not(feature = "gst"))]
        Probe::VaH264Factory { .. } => (
            ProbeAnswer {
                factory: None,
                error: Some("this qrec was built without GStreamer".into()),
            },
            5,
        ),
    }
}

/// Runs `probe` in a child `exe` (the `qrec` binary: `std::env::current_exe()`)
/// that calls [`run_probe_if_requested`], with `LIBVA_DRIVER_NAME` set for
/// the probed node's vendor. stdin is null, stdout goes to a temp file (no
/// pipe to stall), stderr to `log` (or nowhere); the child gets its own
/// process group, which is killed (SIGTERM, then SIGKILL after 2 s) when it
/// outlives `timeout`. Never waits longer than `timeout` + 4 s.
pub fn run_probe_child(
    exe: &Path,
    probe: &Probe,
    log: Option<&Path>,
    timeout: Duration,
) -> Result<ProbeAnswer, String> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut stdout = tempfile::tempfile().map_err(|e| format!("can't create a temp file: {e}"))?;
    let stderr = match log {
        Some(path) => fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map(Stdio::from)
            .map_err(|e| format!("can't open {}: {e}", path.display()))?,
        None => Stdio::null(),
    };
    let mut command = Command::new(exe);
    command
        .env(PROBE_ENV, probe.env_value())
        .stdin(Stdio::null())
        .stdout(
            stdout
                .try_clone()
                .map_err(|e| format!("can't dup the temp file: {e}"))?,
        )
        .stderr(stderr)
        .process_group(0);
    // SAFETY: the closure runs in the forked child before exec and only
    // makes async-signal-safe system calls (close_range or fcntl).
    unsafe {
        command.pre_exec(|| {
            // Only its stdio crosses: nothing Qrec inherited (A§8.7).
            crate::proc::close_on_exec_from(3);
            Ok(())
        });
    }
    let Probe::VaH264Factory { device } = probe;
    let vendor = render_nodes()
        .into_iter()
        .find(|n| n.path == *device)
        .and_then(|n| n.vendor);
    if let Some(driver) = vendor.and_then(libva_driver_for) {
        command.env(LIBVA_DRIVER_NAME, driver);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("can't start the probe {}: {e}", exe.display()))?;
    let status = match wait_with_deadline(&mut child, timeout) {
        Some(status) => status,
        None => {
            kill_group(child);
            return Err(format!(
                "the GPU probe didn't finish within {} s (a hung driver?)",
                timeout.as_secs()
            ));
        }
    };
    let mut text = String::new();
    stdout
        .seek(SeekFrom::Start(0))
        .and_then(|_| stdout.read_to_string(&mut text))
        .map_err(|e| format!("can't read the probe's answer: {e}"))?;
    let answer = text
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<ProbeAnswer>(line).ok());
    match answer {
        Some(answer) => Ok(answer),
        None => Err(format!("the GPU probe failed ({status}) without an answer")),
    }
}

fn wait_with_deadline(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => return None,
        }
    }
}

/// SIGTERM to the child's process group, SIGKILL after 2 s, then at most
/// 2 s more of waiting; a child that still lives is left to a reaper thread.
fn kill_group(mut child: std::process::Child) {
    let pgid = child.id() as i32;
    // SAFETY: kill(2) with a negative pid signals the process group the
    // child leads (it was spawned with process_group(0)); no memory is shared.
    unsafe { libc::kill(-pgid, libc::SIGTERM) };
    if wait_with_deadline(&mut child, Duration::from_secs(2)).is_some() {
        return;
    }
    // SAFETY: as above.
    unsafe { libc::kill(-pgid, libc::SIGKILL) };
    if wait_with_deadline(&mut child, Duration::from_secs(2)).is_some() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("qrec-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(n: u32, vendor: Option<u16>) -> RenderNode {
        RenderNode {
            path: format!("/dev/dri/renderD{n}"),
            vendor,
            driver: None,
        }
    }

    fn fake_sysfs(dir: &Path, nodes: &[(&str, &str, &str)]) {
        for (name, vendor, driver) in nodes {
            let device = dir.join(name).join("device");
            fs::create_dir_all(&device).unwrap();
            fs::write(device.join("vendor"), format!("{vendor}\n")).unwrap();
            let target = dir.join("drivers").join(driver);
            fs::create_dir_all(&target).unwrap();
            std::os::unix::fs::symlink(&target, device.join("driver")).unwrap();
        }
        // Not render nodes.
        fs::create_dir_all(dir.join("card0")).unwrap();
        fs::create_dir_all(dir.join("renderDx")).unwrap();
    }

    #[test]
    fn scans_render_nodes_without_opening_devices() {
        let tmp = tempfile::tempdir().unwrap();
        fake_sysfs(
            tmp.path(),
            &[
                ("renderD129", "0x8086", "i915"),
                ("renderD128", "0x10de", "nvidia"),
                ("renderD130", "garbage", "amdgpu"),
            ],
        );
        let nodes = scan_render_nodes(tmp.path(), Path::new("/dev/dri"));
        assert_eq!(
            nodes,
            vec![
                RenderNode {
                    path: "/dev/dri/renderD128".into(),
                    vendor: Some(VENDOR_NVIDIA),
                    driver: Some("nvidia".into())
                },
                RenderNode {
                    path: "/dev/dri/renderD129".into(),
                    vendor: Some(VENDOR_INTEL),
                    driver: Some("i915".into())
                },
                RenderNode {
                    path: "/dev/dri/renderD130".into(),
                    vendor: None,
                    driver: Some("amdgpu".into())
                },
            ]
        );
        assert!(scan_render_nodes(&tmp.path().join("missing"), Path::new("/dev/dri")).is_empty());
    }

    #[test]
    fn effective_device_order_never_picks_nvidia_automatically() {
        let nodes = [
            node(128, Some(VENDOR_NVIDIA)),
            node(129, Some(VENDOR_INTEL)),
        ];
        let auto = effective_va_device(None, None, &nodes).unwrap();
        assert_eq!(
            auto,
            VaChoice {
                device: "/dev/dri/renderD129".into(),
                vendor: Some(VENDOR_INTEL),
                origin: DeviceOrigin::Auto
            }
        );
        let config = effective_va_device(None, Some("/dev/dri/renderD128"), &nodes).unwrap();
        assert_eq!(
            (config.origin, config.vendor),
            (DeviceOrigin::Config, Some(VENDOR_NVIDIA))
        );
        let project = effective_va_device(
            Some("/dev/dri/renderD129"),
            Some("/dev/dri/renderD128"),
            &nodes,
        )
        .unwrap();
        assert_eq!(project.origin, DeviceOrigin::Project);
        // Blank values don't count; only NVIDIA ⇒ nothing.
        assert_eq!(
            effective_va_device(Some(" "), None, &nodes).unwrap().origin,
            DeviceOrigin::Auto
        );
        assert_eq!(
            effective_va_device(None, None, &[node(128, Some(VENDOR_NVIDIA))]),
            None
        );
        // AMD counts too; an unknown configured node has no vendor.
        let amd = effective_va_device(None, None, &[node(128, Some(VENDOR_AMD))]).unwrap();
        assert_eq!(amd.vendor, Some(VENDOR_AMD));
        let unknown = effective_va_device(Some("/dev/dri/renderD200"), None, &nodes).unwrap();
        assert_eq!(unknown.vendor, None);
    }

    fn plan_with(
        choice: Option<&VaChoice>,
        nodes: &[RenderNode],
        for_gui: bool,
        vars: &[(&str, &str)],
    ) -> GpuEnvPlan {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let env = move |key: &str| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        plan_gpu_env(&GpuEnvInputs {
            choice,
            nodes,
            for_gui,
            env: &env,
            mesa_egl_vendor: Some(
                "/run/opengl-driver/share/glvnd/egl_vendor.d/50_mesa.json".into(),
            ),
            vulkan_icd: Some("/run/opengl-driver/share/vulkan/icd.d/intel_icd.x86_64.json".into()),
        })
    }

    #[test]
    fn libva_driver_follows_the_chosen_vendor() {
        let nodes = [
            node(128, Some(VENDOR_NVIDIA)),
            node(129, Some(VENDOR_INTEL)),
        ];
        let intel = effective_va_device(None, None, &nodes).unwrap();

        let plan = plan_with(Some(&intel), &nodes, false, &[]);
        assert_eq!(
            plan.changes,
            vec![EnvChange {
                key: LIBVA_DRIVER_NAME.into(),
                value: "iHD".into(),
                previous: None
            }]
        );

        // An inherited mismatch is overridden and noted; a fitting value is kept.
        let plan = plan_with(
            Some(&intel),
            &nodes,
            false,
            &[(LIBVA_DRIVER_NAME, "nvidia")],
        );
        assert_eq!(plan.changes[0].previous.as_deref(), Some("nvidia"));
        assert!(plan.notes[0].contains("overriding inherited LIBVA_DRIVER_NAME=nvidia"));
        assert!(
            plan_with(Some(&intel), &nodes, false, &[(LIBVA_DRIVER_NAME, "iHD")])
                .changes
                .is_empty()
        );
        assert!(
            plan_with(Some(&intel), &nodes, false, &[(LIBVA_DRIVER_NAME, "i965")])
                .changes
                .is_empty()
        );

        let amd_nodes = [node(128, Some(VENDOR_AMD))];
        let amd = effective_va_device(None, None, &amd_nodes).unwrap();
        assert_eq!(
            plan_with(Some(&amd), &amd_nodes, false, &[]).changes[0].value,
            "radeonsi"
        );

        // Explicit NVIDIA: untouched.
        let nvidia = effective_va_device(None, Some("/dev/dri/renderD128"), &nodes).unwrap();
        let plan = plan_with(
            Some(&nvidia),
            &nodes,
            true,
            &[(LIBVA_DRIVER_NAME, "nvidia")],
        );
        assert!(plan.changes.is_empty(), "{plan:?}");
        assert!(plan.notes[0].contains("configured explicitly"));

        // No usable node: untouched.
        let plan = plan_with(None, &[node(128, Some(VENDOR_NVIDIA))], true, &[]);
        assert!(plan.changes.is_empty());
    }

    #[test]
    fn gui_pins_egl_and_vulkan_away_from_nvidia() {
        let nodes = [
            node(128, Some(VENDOR_NVIDIA)),
            node(129, Some(VENDOR_INTEL)),
        ];
        let intel = effective_va_device(None, None, &nodes).unwrap();
        let keys = |plan: &GpuEnvPlan| -> Vec<String> {
            plan.changes.iter().map(|c| c.key.clone()).collect()
        };

        let plan = plan_with(Some(&intel), &nodes, true, &[]);
        assert_eq!(
            keys(&plan),
            [
                LIBVA_DRIVER_NAME,
                EGL_VENDOR_FILES,
                VK_DRIVER_FILES,
                VK_ICD_FILENAMES
            ]
        );
        assert!(plan.changes[1].value.ends_with("50_mesa.json"));
        assert!(plan.changes[3].value.ends_with("intel_icd.x86_64.json"));

        // Not for the CLI; not without an NVIDIA device; never over the user's values.
        assert_eq!(
            keys(&plan_with(Some(&intel), &nodes, false, &[])),
            [LIBVA_DRIVER_NAME]
        );
        let intel_only = [node(129, Some(VENDOR_INTEL))];
        assert_eq!(
            keys(&plan_with(Some(&intel), &intel_only, true, &[])),
            [LIBVA_DRIVER_NAME]
        );
        let plan = plan_with(
            Some(&intel),
            &nodes,
            true,
            &[
                (EGL_VENDOR_FILES, "/mine.json"),
                (VK_ICD_FILENAMES, "/vk.json"),
            ],
        );
        assert_eq!(keys(&plan), [LIBVA_DRIVER_NAME]);
        assert_eq!(plan.notes.len(), 2);
    }

    #[test]
    fn finds_graphics_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        let share = tmp.path().join("share");
        fs::create_dir_all(share.join("glvnd/egl_vendor.d")).unwrap();
        fs::create_dir_all(share.join("vulkan/icd.d")).unwrap();
        for name in ["10_nvidia.json", "50_mesa.json"] {
            fs::write(share.join("glvnd/egl_vendor.d").join(name), "{}").unwrap();
        }
        for name in [
            "intel_hasvk_icd.x86_64.json",
            "intel_icd.x86_64.json",
            "nvidia_icd.json",
            "radeon_icd.x86_64.json",
        ] {
            fs::write(share.join("vulkan/icd.d").join(name), "{}").unwrap();
        }
        let dirs = vec![tmp.path().join("empty"), share.clone()];
        assert_eq!(
            find_mesa_egl_vendor(&dirs).unwrap(),
            share
                .join("glvnd/egl_vendor.d/50_mesa.json")
                .to_string_lossy()
        );
        assert!(
            find_vulkan_icd(&dirs, VENDOR_INTEL)
                .unwrap()
                .ends_with("/intel_icd.x86_64.json")
        );
        assert!(
            find_vulkan_icd(&dirs, VENDOR_AMD)
                .unwrap()
                .ends_with("/radeon_icd.x86_64.json")
        );
        assert_eq!(find_vulkan_icd(&dirs, VENDOR_NVIDIA), None);

        let dirs = graphics_data_dirs(Some("/a:relative:/b"));
        assert_eq!(
            dirs,
            ["/run/opengl-driver/share", "/a", "/b", "/etc"].map(PathBuf::from)
        );
        assert!(graphics_data_dirs(None).contains(&PathBuf::from("/usr/share")));
    }

    #[test]
    fn project_dir_comes_from_args_or_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let args = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };
        let cwd = tmp.path();
        assert_eq!(
            project_dir_from_args(&args(&["qrec", "--project", "/p", "status"]), Some(cwd)),
            Some(PathBuf::from("/p"))
        );
        assert_eq!(
            project_dir_from_args(&args(&["qrec", "--project=rel", "status"]), Some(cwd)),
            Some(cwd.join("rel"))
        );
        // After `--` it's an argument, not a flag.
        assert_eq!(
            project_dir_from_args(&args(&["qrec", "--", "--project", "/p"]), Some(cwd)),
            None
        );
        // The cwd counts only when it holds qrec.json.
        assert_eq!(project_dir_from_args(&args(&["qrec"]), Some(cwd)), None);
        fs::write(
            cwd.join("qrec.json"),
            r#"{"recording":{"vaapiDevice":"/dev/dri/renderD129"}}"#,
        )
        .unwrap();
        assert_eq!(
            project_dir_from_args(&args(&["qrec"]), Some(cwd)),
            Some(cwd.to_path_buf())
        );
        assert_eq!(
            project_va_device(cwd).as_deref(),
            Some("/dev/dri/renderD129")
        );
        fs::write(
            cwd.join("qrec.json"),
            r#"{"recording":{"vaapiDevice":null}}"#,
        )
        .unwrap();
        assert_eq!(project_va_device(cwd), None);
        fs::write(cwd.join("qrec.json"), "not json").unwrap();
        assert_eq!(project_va_device(cwd), None);
    }

    #[test]
    fn reads_the_configured_vaapi_device() {
        let cases = [
            (
                "[encoder]\nvaapi_device = \"/dev/dri/renderD129\"\n",
                Some("/dev/dri/renderD129"),
            ),
            (
                "theme = \"swamp-night\"\n[encoder] # GPU\npreference = \"vaapi\"\nvaapi_device = '/dev/dri/renderD130' # intel\n",
                Some("/dev/dri/renderD130"),
            ),
            ("encoder.vaapi_device = \"/dev/x\"\n", Some("/dev/x")),
            ("[ui]\nvaapi_device = \"/dev/x\"\n", None),
            ("[encoder]\n# vaapi_device = \"/dev/x\"\n", None),
            (
                "[encoder]\nvaapi_device = \"a\\\"b\\u0041\"\n",
                Some("a\"bA"),
            ),
            ("[encoder]\nvaapi_device = \"unterminated\n", None),
            ("", None),
        ];
        let tmp = tempfile::tempdir().unwrap();
        for (text, expected) in cases {
            let file = tmp.path().join("config.toml");
            fs::write(&file, text).unwrap();
            assert_eq!(config_va_device(&file).as_deref(), expected, "{text:?}");
        }
        assert_eq!(config_va_device(&tmp.path().join("missing.toml")), None);
    }

    #[test]
    fn never_mutates_a_threaded_process() {
        let change = EnvChange {
            key: "QREC_GPU_TEST_NEVER_SET".into(),
            value: "1".into(),
            previous: None,
        };
        let err = apply_changes(std::slice::from_ref(&change), Some(3)).unwrap_err();
        assert!(err.contains("already runs 3 threads"), "{err}");
        let err = apply_changes(std::slice::from_ref(&change), None).unwrap_err();
        assert!(err.contains("unknown number"), "{err}");
        assert_eq!(apply_changes(&[], Some(3)), Ok(()));
        assert!(std::env::var_os("QREC_GPU_TEST_NEVER_SET").is_none());
        // Test binaries run several threads.
        assert!(thread_count().unwrap() > 1);

        // A device that isn't a render node: nothing to set, nothing applied.
        let report = prepare_gpu_env(&GpuInputs {
            project_device: None,
            config_device: Some("/dev/null".into()),
            for_gui: false,
        });
        assert_eq!(report.choice.as_ref().unwrap().origin, DeviceOrigin::Config);
        assert!(report.changes.is_empty() && report.applied, "{report:?}");
        assert!(startup_report().is_some());
    }

    #[test]
    fn vendor_names() {
        assert_eq!(vendor_name(VENDOR_INTEL), "Intel");
        assert_eq!(vendor_name(0x1234), "0x1234");
        assert_eq!(parse_hex_u16("0x10de"), Some(VENDOR_NVIDIA));
        assert_eq!(parse_hex_u16("zz"), None);
    }

    /// An executable script standing in for `qrec` as a probe child (the
    /// real probe would load the VA plugin, which opens every render node).
    fn fake_child(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-qrec");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn probes_run_in_a_child_with_a_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        // The probe is a fake node: no real device is ever named.
        let probe = Probe::VaH264Factory {
            device: tmp.path().join("renderD999").to_string_lossy().into_owned(),
        };
        assert_eq!(Probe::parse(&probe.env_value()), Some(probe.clone()));
        assert_eq!(Probe::parse("va-h264-factory:"), None);
        assert_eq!(Probe::parse("something-else:/x"), None);

        // The child sees the request and answers on stdout.
        let log = tmp.path().join("probe.log");
        let exe = fake_child(
            tmp.path(),
            r#"echo "probing $QREC_GPU_PROBE" >&2; echo noise; echo '{"factory":"varenderD129h264enc"}'"#,
        );
        let answer = run_probe_child(&exe, &probe, Some(&log), PROBE_TIMEOUT).unwrap();
        assert_eq!(answer.factory.as_deref(), Some("varenderD129h264enc"));
        let logged = fs::read_to_string(&log).unwrap();
        assert!(logged.contains("probing va-h264-factory:"), "{logged}");

        let exe = fake_child(
            tmp.path(),
            r#"echo '{"factory":null,"error":"no VA"}'; exit 5"#,
        );
        let answer = run_probe_child(&exe, &probe, None, PROBE_TIMEOUT).unwrap();
        assert_eq!(
            (answer.factory, answer.error.as_deref()),
            (None, Some("no VA"))
        );

        let exe = fake_child(tmp.path(), "exit 3");
        let err = run_probe_child(&exe, &probe, None, PROBE_TIMEOUT).unwrap_err();
        assert!(err.contains("without an answer"), "{err}");

        // A hung child (and its own children) is killed at the deadline.
        let exe = fake_child(tmp.path(), "sleep 30 & sleep 30");
        let started = std::time::Instant::now();
        let err = run_probe_child(&exe, &probe, None, Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("didn't finish"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );

        let missing = run_probe_child(&tmp.path().join("nope"), &probe, None, PROBE_TIMEOUT);
        assert!(missing.unwrap_err().contains("can't start the probe"));
    }
}
