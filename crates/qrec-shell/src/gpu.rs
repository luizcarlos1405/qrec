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
//! 2. sets `LIBVA_DRIVER_NAME` for that node ([`libva_driver_for`]): AMD's
//!    `radeonsi`; for Intel `i965` on the chips only it drives (older than
//!    Broadwell, and Cherry View/Braswell: [`intel_i965_only`]), else `iHD`
//!    where its file is installed, else `i965` where its is (the files
//!    libva would load, [`VaDrivers`]: a stat each, never opening a
//!    device), else `iHD`. It overrides an inherited value of another
//!    vendor, and leaves the variable alone only when an NVIDIA node is
//!    explicitly configured,
//! 3. for the GUI, when an NVIDIA device exists and isn't the chosen one,
//!    points `__EGL_VENDOR_LIBRARY_FILENAMES` at Mesa and
//!    `VK_DRIVER_FILES`/`VK_ICD_FILENAMES` at the chosen vendor's Vulkan
//!    driver, unless the user set them; inside an AppImage EGL goes to the
//!    bundled Mesa whenever the chosen node is Intel's or AMD's,
//! 4. for the GUI, sets `GDK_DISABLE=vulkan`, so GTK renders with GSK GL
//!    (GUI H-3), unless the user set `GDK_DISABLE` or `GSK_RENDERER`: Mesa's
//!    Vulkan loader opens every render node, NVIDIA's included, while it
//!    enumerates devices, even with the driver pinned. Inside an AppImage
//!    with no render node it may open, `GDK_DISABLE=gl,vulkan`: GTK draws
//!    with Cairo, since the bundled Mesa has no llvmpipe.
//!
//! Inside an AppImage (`$APPDIR` holds this executable: [`appimage_dir`])
//! the graphics manifests come from the bundle first (`$APPDIR/share`), and
//! an inherited `__EGL_VENDOR_LIBRARY_FILENAMES` or `VK_DRIVER_FILES` that
//! points into `$APPDIR` is the bundle's, not the user's: sharun sets the
//! Vulkan one in every start and the EGL one, NVIDIA's vendor first, when
//! NVIDIA's kernel module is loaded. Qrec's rule then applies as if they
//! were unset.
//!
//! The decision is pure ([`plan_gpu_env`]); applying it uses
//! `std::env::set_var`, which is only sound while the process has a single
//! thread (edition 2024 makes it `unsafe`). [`prepare_gpu_env`] checks
//! `/proc/self/task` and refuses to touch the environment once another
//! thread exists, so call it first thing in `main()`. The report is kept for
//! `qrec doctor` ([`startup_report`]), and the host's and the window's logs
//! print it once they are set up ([`log_startup_report`]).
//!
//! Probes that touch a render node run in a child process with a 10 s
//! deadline ([`run_probe_child`]): the child is the `qrec` binary itself,
//! whose `main()` starts with [`run_probe_if_requested`]. Today that is the
//! lookup of the H.264 encoders GStreamer records with
//! ([`Probe::H264Encoders`]): the VA ones bound to a node for
//! `VaapiCaps.gst_h264_factory` and `gst_h264_lp_factory`
//! ([`va_h264_factories_for`], by `device-path`, never by name), and
//! x264enc and openh264enc for `Capabilities.gst_h264`. One child loads the
//! host's driver ([`child_libva_driver`]), so GStreamer's registry, which
//! notes `LIBVA_DRIVER_NAME` for the VA plugin, stays the host's. No other
//! driver is tried: GStreamer's VA plugin registers no encoder with `i965`
//! (blocklisted unless `GST_VA_ALL_DRIVERS` is set), so on the chips only
//! `i965` drives, recordings are the CPU's. An NVIDIA node is never probed:
//! NVIDIA has no VA-API encoder.
//!
//! ffmpeg's `h264_vaapi` does encode with `i965`. A VA export loads the
//! driver its check passed with ([`export_libva_driver_for`]): the node's,
//! or Intel's other one where capability detection found only that one
//! encodes there (`capabilities.json` keeps it, [`VaDriverPick`]).
//!
//! ```ignore
//! fn main() {
//!     qrec_shell::gpu::run_probe_if_requested(); // exits when it is a probe child
//!     let args: Vec<_> = std::env::args_os().collect();
//!     let report = qrec_shell::gpu::prepare_gpu_env(&GpuInputs::discover(&args, is_gui));
//!     // … only now threads, GTK, GStreamer
//! }
//! ```

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use qrec_core::state::GstH264Caps;
use serde::{Deserialize, Serialize};

/// PCI vendor ids.
pub const VENDOR_INTEL: u16 = 0x8086;
pub const VENDOR_AMD: u16 = 0x1002;
pub const VENDOR_NVIDIA: u16 = 0x10de;
/// The virtual machines' GPUs: virtio-gpu (QEMU/KVM), VMware's SVGA and
/// QXL (Red Hat's QEMU devices).
pub const VENDOR_VIRTIO: u16 = 0x1af4;
pub const VENDOR_VMWARE: u16 = 0x15ad;
pub const VENDOR_QXL: u16 = 0x1b36;

/// Intel's libva drivers: intel-media-driver's (Broadwell and newer) and
/// intel-vaapi-driver's (older chips, and Cherry View and Braswell; it
/// drives Intel up to Comet Lake).
pub const DRIVER_IHD: &str = "iHD";
pub const DRIVER_I965: &str = "i965";
/// AMD's libva drivers (Mesa): `radeonsi`, and `r600` for cards older than
/// GCN, which Qrec keeps when it is inherited.
pub const DRIVER_RADEONSI: &str = "radeonsi";
pub const DRIVER_R600: &str = "r600";
/// The libva drivers Qrec loads or keeps, whose files [`VaDrivers`] looks
/// for.
pub const QREC_LIBVA_DRIVERS: [&str; 4] = [DRIVER_IHD, DRIVER_I965, DRIVER_RADEONSI, DRIVER_R600];

/// The variable libva reads to pick its driver.
pub const LIBVA_DRIVER_NAME: &str = "LIBVA_DRIVER_NAME";
/// glvnd's EGL vendor override.
pub const EGL_VENDOR_FILES: &str = "__EGL_VENDOR_LIBRARY_FILENAMES";
/// GTK features to turn off (GTK ≥ 4.16): Qrec's window disables `vulkan`.
pub const GDK_DISABLE: &str = "GDK_DISABLE";
/// GTK's renderer choice; set by the user, Qrec leaves GTK's renderer alone.
pub const GSK_RENDERER: &str = "GSK_RENDERER";
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
    /// PCI device id from sysfs (`0x7d51`), which tells an Intel chip's
    /// generation; `None` when unreadable (a GPU that isn't on PCI).
    pub device_id: Option<u16>,
    /// Kernel driver (`i915`, `xe`, `amdgpu`, `nvidia`), from sysfs.
    pub driver: Option<String>,
}

/// `Intel`, `AMD`, `NVIDIA`, a virtual machine's GPU (`virtio-gpu`,
/// `VMware`, `QXL`) or the hex id.
pub fn vendor_name(vendor: u16) -> String {
    match vendor {
        VENDOR_INTEL => "Intel".into(),
        VENDOR_AMD => "AMD".into(),
        VENDOR_NVIDIA => "NVIDIA".into(),
        VENDOR_VIRTIO => "virtio-gpu".into(),
        VENDOR_VMWARE => "VMware".into(),
        VENDOR_QXL => "QXL".into(),
        other => format!("{other:#06x}"),
    }
}

/// Whether a vendor id is a virtual machine's GPU.
pub fn is_virtual_gpu(vendor: u16) -> bool {
    matches!(vendor, VENDOR_VIRTIO | VENDOR_VMWARE | VENDOR_QXL)
}

/// Whether only Intel's i965 driver drives an Intel GPU, by its PCI device
/// id: the chips older than Broadwell (G45 to Haswell, Bay Trail
/// included), where iHD starts, and the Broadwell-generation Atoms Cherry
/// View and Braswell, which iHD doesn't drive either (intel-vaapi-driver's
/// own lists of them).
pub fn intel_i965_only(device_id: u16) -> bool {
    // Haswell's ids: these low bytes under 0x04, 0x0a, 0x0c and 0x0d.
    const HASWELL: [u8; 15] = [
        0x02, 0x06, 0x0a, 0x0b, 0x0e, 0x12, 0x16, 0x1a, 0x1b, 0x1e, 0x22, 0x26, 0x2a, 0x2b, 0x2e,
    ];
    let [high, low] = device_id.to_be_bytes();
    match high {
        0x04 | 0x0a | 0x0c | 0x0d => HASWELL.contains(&low),
        _ => matches!(
            device_id,
            // G45/G43/G41, GM45, Q45/Q43, B43.
            0x2a42 | 0x2e02 | 0x2e12 | 0x2e22 | 0x2e32 | 0x2e42 | 0x2e92
            // Ironlake.
            | 0x0042 | 0x0046
            // Sandy Bridge.
            | 0x0102 | 0x0106 | 0x010a | 0x0112 | 0x0116 | 0x0122 | 0x0126
            // Ivy Bridge.
            | 0x0152 | 0x0156 | 0x015a | 0x0162 | 0x0166 | 0x016a
            // Bay Trail.
            | 0x0f31 | 0x0f32 | 0x0f33 | 0x0155 | 0x0157
            // Cherry View and Braswell.
            | 0x22b0 | 0x22b1 | 0x22b2 | 0x22b3
        ),
    }
}

/// The libva driver a node of `vendor` (and PCI `device_id`) loads, from
/// the driver files `drivers` found (A§8.10): for Intel `i965` on the chips
/// only it drives ([`intel_i965_only`]), else `iHD` where its file is, else
/// `i965` where its is, else `iHD`; for AMD `radeonsi`. `None` for any
/// other vendor: NVIDIA has no VA-API encoder, and a virtual machine's GPU
/// isn't one Qrec picks.
pub fn libva_driver_for(
    vendor: u16,
    device_id: Option<u16>,
    drivers: &VaDrivers,
) -> Option<&'static str> {
    match vendor {
        VENDOR_INTEL if device_id.is_some_and(intel_i965_only) => Some(DRIVER_I965),
        VENDOR_INTEL if !drivers.has(DRIVER_IHD) && drivers.has(DRIVER_I965) => Some(DRIVER_I965),
        VENDOR_INTEL => Some(DRIVER_IHD),
        VENDOR_AMD => Some(DRIVER_RADEONSI),
        _ => None,
    }
}

/// Intel's other libva driver: `i965` for `iHD` and back.
pub fn other_intel_driver(driver: &str) -> Option<&'static str> {
    match driver {
        DRIVER_IHD => Some(DRIVER_I965),
        DRIVER_I965 => Some(DRIVER_IHD),
        _ => None,
    }
}

/// The driver to run ffmpeg's h264_vaapi check with again where it failed
/// with `failed` on `node` (`caps::detect`, `qrec doctor`): on an Intel
/// node, Intel's other driver when its file is installed and it drives the
/// chip (`iHD` doesn't drive those only `i965` does). Never on another
/// vendor. Only ffmpeg's check: GStreamer's VA plugin registers no encoder
/// with `i965`, so its probe is never run again.
pub fn retry_driver(node: &RenderNode, failed: &str, drivers: &VaDrivers) -> Option<&'static str> {
    if node.vendor != Some(VENDOR_INTEL) {
        return None;
    }
    let i965_only = node.device_id.is_some_and(intel_i965_only);
    other_intel_driver(failed)
        .filter(|other| drivers.has(other))
        .filter(|other| !(*other == DRIVER_IHD && i965_only))
}

/// Whether an inherited `LIBVA_DRIVER_NAME` already fits the vendor (the
/// legacy drivers count: `i965` for old Intel, `r600` for old AMD).
pub fn driver_fits_vendor(driver: &str, vendor: u16) -> bool {
    match vendor {
        VENDOR_INTEL => matches!(driver, DRIVER_IHD | DRIVER_I965),
        VENDOR_AMD => matches!(driver, DRIVER_RADEONSI | DRIVER_R600),
        _ => false,
    }
}

/// The libva driver files Qrec's drivers ([`QREC_LIBVA_DRIVERS`]) have on
/// this machine: `<name>_drv_video.so` in the directories libva loads them
/// from. Found with a stat each: no driver, and no device, is opened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaDrivers {
    /// The directories libva looks in: `LIBVA_DRIVERS_PATH`'s when it is
    /// set (libva then looks nowhere else), else the standard ones that
    /// exist ([`standard_libva_dirs`]).
    pub dirs: Vec<PathBuf>,
    /// Each driver found, by name (`iHD`), with its file: the first
    /// directory's that has one.
    pub found: BTreeMap<String, PathBuf>,
}

impl VaDrivers {
    /// Looks in `LIBVA_DRIVERS_PATH`'s directories (`libva_drivers_path`,
    /// colon-separated, absolute ones only), else in the standard ones.
    pub fn new(libva_drivers_path: Option<&str>) -> Self {
        let dirs = match libva_drivers_path.filter(|p| !p.trim().is_empty()) {
            Some(path) => path
                .split(':')
                .filter(|d| d.starts_with('/'))
                .map(PathBuf::from)
                .collect(),
            None => standard_libva_dirs()
                .into_iter()
                .filter(|d| d.is_dir())
                .collect(),
        };
        Self::scan(dirs)
    }

    /// This process's (`LIBVA_DRIVERS_PATH`).
    pub fn from_process() -> Self {
        Self::new(std::env::var("LIBVA_DRIVERS_PATH").ok().as_deref())
    }

    /// Qrec's driver files in `dirs`.
    pub fn scan(mut dirs: Vec<PathBuf>) -> Self {
        dirs.dedup();
        let found = QREC_LIBVA_DRIVERS
            .iter()
            .filter_map(|name| {
                dirs.iter()
                    .map(|dir| dir.join(format!("{name}_drv_video.so")))
                    .find(|file| file.is_file())
                    .map(|file| ((*name).to_owned(), file))
            })
            .collect();
        Self { dirs, found }
    }

    /// The file of the driver `name`, when it is installed.
    pub fn file(&self, name: &str) -> Option<&Path> {
        self.found.get(name).map(PathBuf::as_path)
    }

    pub fn has(&self, name: &str) -> bool {
        self.found.contains_key(name)
    }
}

/// Where distributions install libva's drivers, in libva's own order where
/// it searches several: NixOS's `/run/opengl-driver/lib/dri`; Fedora's
/// RPM Fusion directories (`dri-nonfree`, `dri-freeworld`), then
/// `/usr/lib64/dri` (Fedora, openSUSE); Debian's and Ubuntu's
/// `/usr/lib/<arch>-linux-gnu/dri`; `/usr/lib/dri` (Arch, Alpine, Void);
/// Gentoo's `va/drivers`; and `/usr/local/lib/dri` (built from source).
pub fn standard_libva_dirs() -> Vec<PathBuf> {
    [
        "/run/opengl-driver/lib/dri".to_owned(),
        "/usr/lib64/dri-nonfree".to_owned(),
        "/usr/lib64/dri-freeworld".to_owned(),
        "/usr/lib64/dri".to_owned(),
        format!("/usr/lib/{}-linux-gnu/dri", std::env::consts::ARCH),
        "/usr/lib/dri".to_owned(),
        "/usr/lib64/va/drivers".to_owned(),
        "/usr/lib/va/drivers".to_owned(),
        "/usr/local/lib/dri".to_owned(),
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

/// The libva driver VA exports load on a node where ffmpeg's h264_vaapi
/// check failed with the node's own ([`child_libva_driver`]) and passed
/// with Intel's other one ([`retry_driver`]): `capabilities.json`'s
/// `exportDriver`, written by `caps::detect`. [`export_libva_driver`] uses
/// it while the node and its PCI device id are the same and its file is
/// installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaDriverPick {
    /// The render node (`/dev/dri/renderD128`), as sysfs lists it.
    pub device: String,
    /// Its PCI device id when it was checked.
    pub device_id: Option<u16>,
    /// `i965` or `iHD`.
    pub driver: String,
}

impl VaDriverPick {
    /// It was made for `node`, the same GPU.
    pub fn is_for(&self, node: &RenderNode) -> bool {
        self.device == node.path && self.device_id == node.device_id
    }
}

/// The `LIBVA_DRIVER_NAME` a child process that opens `node` gets (a GPU
/// probe, the h264_vaapi check): `inherited`, this process's, when it fits
/// the node's vendor (`main()` pinned it: [`plan_gpu_env`]), else the
/// node's ([`libva_driver_for`]). `None` for NVIDIA and other vendors: the
/// child keeps the inherited value.
pub fn child_libva_driver(
    node: &RenderNode,
    inherited: Option<&str>,
    drivers: &VaDrivers,
) -> Option<String> {
    let vendor = node.vendor?;
    match inherited {
        Some(driver) if driver_fits_vendor(driver, vendor) => Some(driver.to_owned()),
        _ => libva_driver_for(vendor, node.device_id, drivers).map(str::to_owned),
    }
}

/// The `LIBVA_DRIVER_NAME` of a VA export on `node`: the one its h264_vaapi
/// check passed with. That is the node's ([`child_libva_driver`]), unless
/// capability detection found only Intel's other driver encodes there
/// (`pick`): then that one, while the pick is for this node and GPU, fits
/// its vendor and its file is installed. `None` for NVIDIA and other
/// vendors.
pub fn export_libva_driver(
    node: &RenderNode,
    inherited: Option<&str>,
    drivers: &VaDrivers,
    pick: Option<&VaDriverPick>,
) -> Option<String> {
    let vendor = node.vendor?;
    match pick {
        Some(pick)
            if pick.is_for(node)
                && driver_fits_vendor(&pick.driver, vendor)
                && drivers.has(&pick.driver) =>
        {
            Some(pick.driver.clone())
        }
        _ => child_libva_driver(node, inherited, drivers),
    }
}

/// [`export_libva_driver`] for a device path on this machine: its render
/// node from sysfs, this process's `LIBVA_DRIVER_NAME`, libva's driver
/// files and the pick in Qrec's capabilities cache (read now: the session's
/// detection wrote it before any export). `None` when the path is no
/// render node of a known vendor.
pub fn export_libva_driver_for(device: &str) -> Option<String> {
    let nodes = render_nodes();
    let node = node_for(device, &nodes)?;
    let inherited = std::env::var(LIBVA_DRIVER_NAME).ok();
    let pick = crate::paths::UserDirs::resolve(&crate::paths::Env::from_process())
        .ok()
        .and_then(|dirs| crate::caps::read_export_driver(&dirs.capabilities_cache()));
    export_libva_driver(
        node,
        inherited.as_deref(),
        &VaDrivers::from_process(),
        pick.as_ref(),
    )
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
            let hex = |file: &str| {
                fs::read_to_string(device.join(file))
                    .ok()
                    .and_then(|text| parse_hex_u16(text.trim()))
            };
            let driver = fs::read_link(device.join("driver"))
                .ok()
                .and_then(|link| link.file_name().map(|n| n.to_string_lossy().into_owned()));
            let path = dev_dri.join(&name).to_string_lossy().into_owned();
            Some((
                number,
                RenderNode {
                    path,
                    vendor: hex("vendor"),
                    device_id: hex("device"),
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

/// Whether this process may open a render node's device file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAccess {
    pub path: String,
    /// Why it can't ("No such file or directory (os error 2)": sysfs lists
    /// the GPU, but a container didn't get its device); `None` when it can.
    pub error: Option<String>,
}

/// [`NodeAccess`] for each node, by access(2) for reading and writing: a
/// permission check, so no device is opened, NVIDIA's included (A§8.10).
pub fn node_access(nodes: &[RenderNode]) -> Vec<NodeAccess> {
    nodes
        .iter()
        .map(|node| NodeAccess {
            path: node.path.clone(),
            error: may_open(Path::new(&node.path)).err().map(|e| e.to_string()),
        })
        .collect()
}

/// access(2) with `R_OK | W_OK`.
fn may_open(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: access(2) reads a NUL-terminated path that outlives the call;
    // it opens nothing.
    if unsafe { libc::access(path.as_ptr(), libc::R_OK | libc::W_OK) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
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
pub fn node_for<'a>(device: &str, nodes: &'a [RenderNode]) -> Option<&'a RenderNode> {
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
    /// The libva driver the chosen node needs ([`libva_driver_for`]: its
    /// vendor and generation, and the driver files).
    pub va_driver: Option<&'a str>,
    /// The AppImage's folder when Qrec runs from one ([`appimage_dir`]).
    pub appdir: Option<&'a str>,
    /// Whether this process may open each of `nodes` ([`node_access`]).
    pub access: &'a [NodeAccess],
}

/// The GUI's `GDK_DISABLE`: Vulkan off (GSK GL), and GL too (Cairo).
const GDK_DISABLE_VULKAN: &str = "vulkan";
const GDK_DISABLE_GL_VULKAN: &str = "gl,vulkan";

/// Decides which variables to set (pure; A§8.10).
pub fn plan_gpu_env(inputs: &GpuEnvInputs<'_>) -> GpuEnvPlan {
    let mut plan = GpuEnvPlan::default();
    let inherited = |key: &str| (inputs.env)(key).filter(|v| !v.is_empty());
    // Inside an AppImage a value that points into it is the bundle's
    // (sharun's), which Qrec's rule replaces; any other is the user's.
    let bundles = |value: &str| {
        inputs.appdir.is_some_and(|dir| {
            value
                .split(':')
                .any(|entry| Path::new(entry).starts_with(dir))
        })
    };
    let users = |key: &str| inherited(key).filter(|value| !bundles(value));
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
            Some(vendor) => match inputs.va_driver {
                None => plan.notes.push(format!(
                    "no known libva driver for vendor {} ({}); LIBVA_DRIVER_NAME untouched",
                    vendor_name(vendor),
                    choice.device
                )),
                Some(driver) => {
                    let previous = inherited(LIBVA_DRIVER_NAME);
                    // An inherited value stays when it is right, or the same
                    // vendor's other driver (set by the user).
                    if !previous
                        .as_deref()
                        .is_some_and(|p| driver_fits_vendor(p, vendor))
                    {
                        if let Some(previous) = &previous {
                            plan.notes.push(format!(
                                "overriding inherited LIBVA_DRIVER_NAME={previous}: {} is {}, so \
                                 {driver}",
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
    let on_intel_or_amd =
        inputs.for_gui && matches!(chosen_vendor, Some(VENDOR_INTEL | VENDOR_AMD));
    // EGL away from NVIDIA's driver; inside an AppImage always to the
    // bundled Mesa, so neither NVIDIA's EGL nor the host's Mesa (NixOS's
    // /run/opengl-driver) loads among the bundled libraries.
    if on_intel_or_amd && (nvidia_present || inputs.appdir.is_some()) {
        let previous = inherited(EGL_VENDOR_FILES);
        if users(EGL_VENDOR_FILES).is_some() {
            plan.notes
                .push(format!("{EGL_VENDOR_FILES} is set by the user; left alone"));
        } else if let Some(file) = &inputs.mesa_egl_vendor {
            if previous.is_some() {
                plan.notes.push(format!(
                    "{EGL_VENDOR_FILES} came with the AppImage, not from the user: Qrec \
                     pins the bundled Mesa"
                ));
            }
            plan.changes.push(EnvChange {
                key: EGL_VENDOR_FILES.into(),
                value: file.clone(),
                previous,
            });
        } else {
            plan.notes
                .push("Mesa's EGL vendor file wasn't found; EGL may pick the NVIDIA driver".into());
        }
    }
    if on_intel_or_amd && nvidia_present {
        if users(VK_DRIVER_FILES).is_some() || users(VK_ICD_FILENAMES).is_some() {
            plan.notes.push(format!(
                "{VK_DRIVER_FILES}/{VK_ICD_FILENAMES} are set by the user; left alone"
            ));
        } else if let Some(file) = &inputs.vulkan_icd {
            for key in [VK_DRIVER_FILES, VK_ICD_FILENAMES] {
                plan.changes.push(EnvChange {
                    key: key.into(),
                    value: file.clone(),
                    previous: inherited(key),
                });
            }
        } else {
            plan.notes.push(
                "the chosen GPU's Vulkan driver manifest wasn't found; Vulkan may pick the NVIDIA driver"
                    .into(),
            );
        }
    }

    // GUI H-3: GSK GL, never Vulkan (which opens every render node while it
    // enumerates devices), unless the user chose GTK's renderer. Their
    // choice is noted for every command, so `qrec doctor` shows it. Inside
    // an AppImage with no render node to open, Cairo: the bundled Mesa has
    // no llvmpipe, and GL on its softpipe is slow.
    let no_node = inputs.access.iter().all(|node| node.error.is_some());
    let cairo = inputs.for_gui && inputs.appdir.is_some() && no_node;
    let disable = if cairo {
        GDK_DISABLE_GL_VULKAN
    } else {
        GDK_DISABLE_VULKAN
    };
    let chosen: Vec<String> = [GDK_DISABLE, GSK_RENDERER]
        .into_iter()
        .filter_map(|key| inherited(key).map(|value| format!("{key}={value}")))
        .collect();
    if !chosen.is_empty() {
        plan.notes.push(format!(
            "{} set by the user: the Qrec window renders as it says (Qrec would set {GDK_DISABLE}={disable})",
            chosen.join(" and ")
        ));
    } else if inputs.for_gui {
        if cairo {
            plan.notes.push(format!(
                "{}: the window draws with Cairo ({GDK_DISABLE}={disable}), since the \
                 AppImage's Mesa has no llvmpipe and its softpipe is slow",
                no_render_node(inputs.access)
            ));
        }
        plan.changes.push(EnvChange {
            key: GDK_DISABLE.into(),
            value: disable.into(),
            previous: None,
        });
    }
    plan
}

/// Why no render node draws: there is none, or each one's access error.
fn no_render_node(access: &[NodeAccess]) -> String {
    if access.is_empty() {
        return "no GPU render node".to_owned();
    }
    let why: Vec<String> = access
        .iter()
        .map(|node| format!("{}: {}", node.path, node.error.as_deref().unwrap_or("?")))
        .collect();
    format!("no render node can be opened ({})", why.join("; "))
}

/// GStreamer's registry file ([`plan_gst_env`]).
pub const GST_REGISTRY: &str = "GST_REGISTRY_1_0";

/// Search paths of gst-plugins-bad's `lv2`, `ladspa` and `frei0r`
/// elements. Qrec uses none of these effects.
pub const EFFECT_PLUGIN_PATHS: [&str; 3] = ["LV2_PATH", "LADSPA_PATH", "FREI0R_PATH"];

/// Step 4: GStreamer's plugin scan stays Qrec's own. The registry is Qrec's
/// cache file (`$XDG_CACHE_HOME/qrec/gstreamer-registry-<arch>.bin`) unless
/// the user names one, so other GStreamer apps never make it stale and the
/// scan, which opens every render node through the `va` plugin, runs only
/// when Qrec's plugins change. The LV2, LADSPA and frei0r search paths are
/// emptied, so the scan never loads the user's effect plugins (and their
/// warnings) into the recorder.
pub fn plan_gst_env(env: &dyn Fn(&str) -> Option<String>) -> GpuEnvPlan {
    let mut plan = GpuEnvPlan::default();
    let inherited = |key: &str| env(key).filter(|v| !v.is_empty());
    if inherited(GST_REGISTRY).is_none() {
        let cache = inherited("XDG_CACHE_HOME")
            .or_else(|| inherited("HOME").map(|home| format!("{home}/.cache")));
        match cache {
            Some(cache) => plan.changes.push(EnvChange {
                key: GST_REGISTRY.into(),
                value: format!(
                    "{cache}/qrec/gstreamer-registry-{}.bin",
                    std::env::consts::ARCH
                ),
                previous: None,
            }),
            None => plan.notes.push(
                "neither XDG_CACHE_HOME nor HOME is set: GStreamer keeps its shared registry"
                    .into(),
            ),
        }
    }
    for key in EFFECT_PLUGIN_PATHS {
        if env(key).as_deref() != Some("") {
            plan.changes.push(EnvChange {
                key: key.into(),
                value: String::new(),
                previous: inherited(key),
            });
        }
    }
    plan
}

/// Directories holding `glvnd/` and `vulkan/` data, in the order they are
/// searched: the AppImage's own (`$APPDIR/share`) when Qrec runs from one,
/// NixOS's driver dir, `$XDG_DATA_DIRS` (default
/// `/usr/local/share:/usr/share`) and `/etc`, each once. Inside an AppImage
/// the bundle's manifests win: NixOS's names its Mesa by a store path, a
/// host library among the bundled ones.
pub fn graphics_data_dirs(xdg_data_dirs: Option<&str>, appdir: Option<&str>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = appdir
        .map(|dir| Path::new(dir).join("share"))
        .into_iter()
        .collect();
    dirs.push(PathBuf::from("/run/opengl-driver/share"));
    let xdg = xdg_data_dirs
        .filter(|s| !s.is_empty())
        .unwrap_or("/usr/local/share:/usr/share");
    dirs.extend(
        xdg.split(':')
            .filter(|d| d.starts_with('/'))
            .map(PathBuf::from),
    );
    dirs.push(PathBuf::from("/etc"));
    let mut seen = std::collections::BTreeSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));
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
    /// The AppImage's folder when Qrec runs from one ([`appimage_dir`]).
    pub appdir: Option<String>,
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
        let exe = std::env::current_exe().ok();
        Self {
            project_device,
            config_device,
            for_gui,
            appdir: appimage_dir(std::env::var_os("APPDIR").as_deref(), exe.as_deref()),
        }
    }
}

/// `$APPDIR` (`appdir`) when it holds this executable (`exe`): Qrec runs
/// from that AppImage, its FUSE mount or the folder it extracted itself to.
/// An `APPDIR` another AppImage leaked into what it starts (a terminal's)
/// doesn't count, nor a relative one. Symlinks are resolved on both sides.
pub fn appimage_dir(appdir: Option<&OsStr>, exe: Option<&Path>) -> Option<String> {
    let dir = Path::new(appdir?);
    let exe = exe?;
    if !dir.is_absolute() {
        return None;
    }
    let resolved = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    (exe.starts_with(dir) || resolved(exe).starts_with(resolved(dir)))
        .then(|| dir.to_string_lossy().into_owned())
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

impl GpuReport {
    /// What changed and why, a line each, as `qrec doctor` and the logs
    /// print it: "set LIBVA_DRIVER_NAME=iHD (was nvidia)", then the notes.
    pub fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .changes
            .iter()
            .map(|c| match &c.previous {
                Some(previous) => format!("set {}={} (was {previous})", c.key, c.value),
                None => format!("set {}={}", c.key, c.value),
            })
            .collect();
        if !self.applied && !self.changes.is_empty() {
            lines.push("those changes were not applied (more than one thread)".to_owned());
        }
        lines.extend(self.notes.iter().cloned());
        lines
    }
}

static STARTUP_REPORT: OnceLock<GpuReport> = OnceLock::new();

/// The report of the first [`prepare_gpu_env`] call in this process.
pub fn startup_report() -> Option<&'static GpuReport> {
    STARTUP_REPORT.get()
}

/// Logs the startup report's lines ("gpu: set GDK_DISABLE=vulkan"), for a
/// process whose log is set up: [`prepare_gpu_env`] runs before any.
pub fn log_startup_report() {
    for line in startup_report().map(GpuReport::lines).unwrap_or_default() {
        tracing::info!("gpu: {line}");
    }
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
    let appdir = inputs.appdir.as_deref();
    let data_dirs = graphics_data_dirs(std::env::var("XDG_DATA_DIRS").ok().as_deref(), appdir);
    let access = node_access(&nodes);
    let chosen_vendor = choice.as_ref().and_then(|c| c.vendor);
    let va_driver = choice
        .as_ref()
        .and_then(|c| node_for(&c.device, &nodes))
        .and_then(|node| {
            libva_driver_for(node.vendor?, node.device_id, &VaDrivers::from_process())
        });
    let env = |key: &str| std::env::var(key).ok();
    let plan = plan_gpu_env(&GpuEnvInputs {
        choice: choice.as_ref(),
        nodes: &nodes,
        for_gui: inputs.for_gui,
        env: &env,
        mesa_egl_vendor: find_mesa_egl_vendor(&data_dirs),
        vulkan_icd: chosen_vendor.and_then(|v| find_vulkan_icd(&data_dirs, v)),
        va_driver,
        appdir,
        access: &access,
    });
    let gst = plan_gst_env(&env);
    let mut changes = plan.changes;
    changes.extend(gst.changes);
    let mut notes = plan.notes;
    notes.extend(gst.notes);
    let applied = match apply_changes(&changes, thread_count()) {
        Ok(()) => true,
        Err(note) => {
            notes.push(note);
            false
        }
    };
    // GStreamer writes its registry there once it initializes, later.
    if applied
        && let Some(registry) = changes.iter().find(|c| c.key == GST_REGISTRY)
        && let Some(dir) = Path::new(&registry.value).parent()
        && let Err(e) = fs::create_dir_all(dir)
    {
        notes.push(format!("can't create {}: {e}", dir.display()));
    }
    // Logged once the process has a log ([`log_startup_report`]).
    let report = GpuReport {
        nodes,
        choice,
        changes,
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

/// Which VA H.264 encoder an element factory is, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaH264Kind {
    /// `va…h264enc` (`vah264enc`, `varenderD129h264enc`): the encode slice
    /// entrypoint.
    Full,
    /// `va…h264lpenc`: the low-power (VDENC) one, the only H.264 encoder of
    /// Intel Arc, Meteor Lake and newer.
    LowPower,
}

/// The [`VaH264Kind`] of a factory name; `None` for anything that isn't a
/// VA H.264 encoder of GStreamer's VA plugin (`vah264dec`, `vah265enc`,
/// `x264enc`, `nvh264enc`, gstreamer-vaapi's `vaapih264enc`, which has no
/// `device-path`).
pub fn va_h264_kind(name: &str) -> Option<VaH264Kind> {
    let rest = name.strip_prefix("va")?;
    if rest.starts_with("api") {
        None
    } else if rest.ends_with("h264lpenc") {
        Some(VaH264Kind::LowPower)
    } else if rest.ends_with("h264enc") {
        Some(VaH264Kind::Full)
    } else {
        None
    }
}

/// The VA H.264 encoders bound to one render node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaH264Factories {
    /// Its `va*h264enc`.
    pub full: Option<String>,
    /// Its `va*h264lpenc`.
    pub low_power: Option<String>,
}

/// The `va*h264enc` and `va*h264lpenc` factories whose `device-path` is
/// `device` (A§8.10): VA elements are registered per render node and the
/// first usable node gets the plain `vah264enc` / `vah264lpenc` names, so
/// the name alone says nothing.
///
/// Reading `device-path` instantiates each candidate element but doesn't
/// open its device (the VA display is opened on the NULL→READY change).
/// Needs GStreamer initialized and `LIBVA_DRIVER_NAME` set first.
#[cfg(feature = "gst")]
pub fn va_h264_factories_for(device: &str) -> VaH264Factories {
    use gst::prelude::*;

    let wanted = fs::canonicalize(device).unwrap_or_else(|_| PathBuf::from(device));
    let mut names: Vec<(String, VaH264Kind)> = gst::Registry::get()
        .features(gst::ElementFactory::static_type())
        .into_iter()
        .filter_map(|f| {
            let name = f.name().to_string();
            va_h264_kind(&name).map(|kind| (name, kind))
        })
        .collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    let bound = |name: &str| {
        let Ok(element) = gst::ElementFactory::make(name).build() else {
            return false;
        };
        if element.find_property("device-path").is_none() {
            return false;
        }
        let path: Option<String> = element.property("device-path");
        path.is_some_and(|p| fs::canonicalize(&p).unwrap_or_else(|_| PathBuf::from(&p)) == wanted)
    };
    let find = |want: VaH264Kind| {
        names
            .iter()
            .filter(|(_, kind)| *kind == want)
            .map(|(name, _)| name)
            .find(|name| bound(name))
            .cloned()
    };
    VaH264Factories {
        full: find(VaH264Kind::Full),
        low_power: find(VaH264Kind::LowPower),
    }
}

/// GStreamer's software H.264 record encoders, from the registry (no
/// plugin is loaded). Needs GStreamer initialized.
#[cfg(feature = "gst")]
pub fn software_h264_encoders() -> GstH264Caps {
    let has = |name| gst::ElementFactory::find(name).is_some();
    GstH264Caps {
        x264: has("x264enc"),
        openh264: has("openh264enc"),
    }
}

// ------------------------------------------------------------ probe child

/// Set on a `qrec` child process to make it a GPU probe (A§8.10: probes
/// that touch a render node run as child processes with a 10 s timeout):
/// `h264-encoders:<render node>`, or `h264-encoders` without one.
pub const PROBE_ENV: &str = "QREC_GPU_PROBE";

/// How long a probe child may run.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// A probe a child process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// The H.264 encoders GStreamer records with: [`software_h264_encoders`]
    /// and, given a node, [`va_h264_factories_for`] it. GStreamer's plugin
    /// scan, which may run with the init, opens every render node.
    H264Encoders { device: Option<String> },
}

impl Probe {
    /// The [`PROBE_ENV`] value.
    pub fn env_value(&self) -> String {
        match self {
            Probe::H264Encoders {
                device: Some(device),
            } => format!("h264-encoders:{device}"),
            Probe::H264Encoders { device: None } => "h264-encoders".to_owned(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.split_once(':') {
            None if value == "h264-encoders" => Some(Probe::H264Encoders { device: None }),
            Some(("h264-encoders", device)) if !device.is_empty() => Some(Probe::H264Encoders {
                device: Some(device.to_owned()),
            }),
            _ => None,
        }
    }

    /// The render node the probe opens on purpose, if any.
    fn device(&self) -> Option<&str> {
        match self {
            Probe::H264Encoders { device } => device.as_deref(),
        }
    }
}

/// What a probe child prints (one JSON line on stdout).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeAnswer {
    /// The `va*h264enc` bound to the node, if any.
    pub factory: Option<String>,
    /// The `va*h264lpenc` bound to the node, if any.
    #[serde(default)]
    pub lp_factory: Option<String>,
    /// x264enc and openh264enc; `None` when the probe couldn't tell.
    #[serde(default)]
    pub software: Option<GstH264Caps>,
    /// Why the probe couldn't answer.
    pub error: Option<String>,
}

/// **Call first thing in `main()`** (before [`prepare_gpu_env`]): when this
/// process was started as a probe child ([`PROBE_ENV`]), runs the probe,
/// prints its [`ProbeAnswer`] and exits; otherwise returns at once. The
/// parent already gave the child its `LIBVA_DRIVER_NAME`: the probed
/// node's ([`child_libva_driver`]: the parent's own when it fits), or the
/// parent's own without a node ([`run_probe_child`]).
pub fn run_probe_if_requested() {
    let Some(value) = std::env::var_os(PROBE_ENV) else {
        return;
    };
    let (answer, code) = match Probe::parse(&value.to_string_lossy()) {
        Some(probe) => run_probe(&probe),
        None => (
            ProbeAnswer {
                error: Some(format!("unknown probe {value:?}")),
                ..ProbeAnswer::default()
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
        Probe::H264Encoders { device } => match crate::gst_init::init() {
            Ok(()) => {
                let va = device
                    .as_deref()
                    .map(va_h264_factories_for)
                    .unwrap_or_default();
                (
                    ProbeAnswer {
                        factory: va.full,
                        lp_factory: va.low_power,
                        software: Some(software_h264_encoders()),
                        error: None,
                    },
                    0,
                )
            }
            Err(e) => (
                ProbeAnswer {
                    error: Some(e.to_string()),
                    ..ProbeAnswer::default()
                },
                crate::gst_init::EXIT_CODE,
            ),
        },
        #[cfg(not(feature = "gst"))]
        Probe::H264Encoders { .. } => (
            ProbeAnswer {
                error: Some("this qrec was built without GStreamer".into()),
                ..ProbeAnswer::default()
            },
            5,
        ),
    }
}

/// Runs `probe` in a child `exe` (the `qrec` binary: `std::env::current_exe()`)
/// that calls [`run_probe_if_requested`], with the probed node's
/// `LIBVA_DRIVER_NAME` ([`child_libva_driver`]: this process's, which
/// `main()` pinned, when it fits the node's vendor, else the node's), so
/// the child loads the driver the host records with and GStreamer's
/// registry, which notes the variable for the VA plugin, isn't rescanned
/// and rewritten by the child and again by the host each time one of them
/// starts. One child: no other driver is tried, since GStreamer's VA
/// plugin registers no encoder with `i965` (blocklisted unless
/// `GST_VA_ALL_DRIVERS` is set) and a second driver would rewrite that
/// registry. Without a node the child keeps this process's. An NVIDIA node
/// is refused: NVIDIA has no VA-API encoder (A§8.10).
///
/// stdin is null, stdout goes to a temp file (no pipe to stall), stderr to
/// `log` (or nowhere); the child gets its own process group, which is
/// killed (SIGTERM, then SIGKILL after 2 s) when it outlives `timeout`.
/// Never waits longer than `timeout` + 4 s.
pub fn run_probe_child(
    exe: &Path,
    probe: &Probe,
    log: Option<&Path>,
    timeout: Duration,
) -> Result<ProbeAnswer, String> {
    let nodes = render_nodes();
    let node = probe.device().and_then(|device| node_for(device, &nodes));
    let inherited = std::env::var(LIBVA_DRIVER_NAME).ok();
    let drivers = VaDrivers::from_process();
    probe_node(
        exe,
        probe,
        node,
        inherited.as_deref(),
        &drivers,
        log,
        timeout,
    )
}

/// [`run_probe_child`] on `node` (the probe's device as sysfs lists it),
/// with this process's `LIBVA_DRIVER_NAME` (`inherited`) and libva's
/// driver files (`drivers`).
fn probe_node(
    exe: &Path,
    probe: &Probe,
    node: Option<&RenderNode>,
    inherited: Option<&str>,
    drivers: &VaDrivers,
    log: Option<&Path>,
    timeout: Duration,
) -> Result<ProbeAnswer, String> {
    if let Some(node) = node.filter(|n| n.vendor == Some(VENDOR_NVIDIA)) {
        return Err(format!(
            "{} is an NVIDIA node, which is never probed: NVIDIA has no VA-API encoder",
            node.path
        ));
    }
    let driver = node.and_then(|n| child_libva_driver(n, inherited, drivers));
    spawn_probe(exe, probe, driver.as_deref(), log, timeout)
}

/// One probe child, given `driver` as its `LIBVA_DRIVER_NAME` (else it
/// keeps this process's).
fn spawn_probe(
    exe: &Path,
    probe: &Probe,
    driver: Option<&str>,
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
    if let Some(driver) = driver {
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
            device_id: None,
            driver: None,
        }
    }

    /// Render nodes in a fake `/sys/class/drm`: name, PCI vendor and device
    /// ids (`-`: no file, as for a GPU that isn't on PCI), kernel driver.
    fn fake_sysfs(dir: &Path, nodes: &[(&str, &str, &str, &str)]) {
        for (name, vendor, device_id, driver) in nodes {
            let device = dir.join(name).join("device");
            fs::create_dir_all(&device).unwrap();
            for (file, value) in [("vendor", vendor), ("device", device_id)] {
                if *value != "-" {
                    fs::write(device.join(file), format!("{value}\n")).unwrap();
                }
            }
            let target = dir.join("drivers").join(driver);
            fs::create_dir_all(&target).unwrap();
            std::os::unix::fs::symlink(&target, device.join("driver")).unwrap();
        }
        // Not render nodes.
        fs::create_dir_all(dir.join("card0")).unwrap();
        fs::create_dir_all(dir.join("renderDx")).unwrap();
    }

    /// A libva driver directory holding these drivers' files.
    fn fake_drivers(dir: &Path, names: &[&str]) -> VaDrivers {
        let dri = dir.join("dri");
        fs::create_dir_all(&dri).unwrap();
        for name in names {
            fs::write(dri.join(format!("{name}_drv_video.so")), "").unwrap();
        }
        VaDrivers::new(Some(&dri.to_string_lossy()))
    }

    #[test]
    fn scans_render_nodes_without_opening_devices() {
        let tmp = tempfile::tempdir().unwrap();
        fake_sysfs(
            tmp.path(),
            &[
                ("renderD129", "0x8086", "0x7d51", "i915"),
                ("renderD128", "0x10de", "0x2db8", "nvidia"),
                ("renderD130", "garbage", "-", "amdgpu"),
            ],
        );
        let nodes = scan_render_nodes(tmp.path(), Path::new("/dev/dri"));
        assert_eq!(
            nodes,
            vec![
                RenderNode {
                    path: "/dev/dri/renderD128".into(),
                    vendor: Some(VENDOR_NVIDIA),
                    device_id: Some(0x2db8),
                    driver: Some("nvidia".into())
                },
                RenderNode {
                    path: "/dev/dri/renderD129".into(),
                    vendor: Some(VENDOR_INTEL),
                    device_id: Some(0x7d51),
                    driver: Some("i915".into())
                },
                RenderNode {
                    path: "/dev/dri/renderD130".into(),
                    vendor: None,
                    device_id: None,
                    driver: Some("amdgpu".into())
                },
            ]
        );
        assert!(scan_render_nodes(&tmp.path().join("missing"), Path::new("/dev/dri")).is_empty());
    }

    /// The machines of issue #9 by their sysfs and libva driver files: the
    /// node `main()` picks and the `LIBVA_DRIVER_NAME` it sets there (a
    /// stat of the driver files, never a device opened). Intel gets iHD
    /// where it is installed, i965 on the chips only it drives (older than
    /// Broadwell, Cherry View) or where only it is; AMD radeonsi; NVIDIA,
    /// a virtual machine's GPU or none: nothing picked, nothing set.
    #[test]
    fn the_va_driver_follows_the_node_and_its_driver_files() {
        /// A render node in sysfs: name, vendor, device id, kernel driver.
        type Node = (&'static str, &'static str, &'static str, &'static str);
        struct Machine {
            name: &'static str,
            sysfs: &'static [Node],
            /// Its libva driver files.
            files: &'static [&'static str],
            /// The node picked and `LIBVA_DRIVER_NAME` set there
            /// (`renderD128=i965`), or nothing.
            expected: &'static str,
        }
        let machines = [
            Machine {
                name: "Haswell with only i965",
                sysfs: &[("renderD128", "0x8086", "0x0416", "i915")],
                files: &["i965"],
                expected: "renderD128=i965",
            },
            Machine {
                // Ubuntu installs both: iHD doesn't drive Haswell.
                name: "Haswell with both drivers",
                sysfs: &[("renderD128", "0x8086", "0x0416", "i915")],
                files: &["iHD", "i965"],
                expected: "renderD128=i965",
            },
            Machine {
                // A Broadwell-generation Atom iHD doesn't drive.
                name: "Cherry View with both drivers",
                sysfs: &[("renderD128", "0x8086", "0x22b0", "i915")],
                files: &["iHD", "i965"],
                expected: "renderD128=i965",
            },
            Machine {
                name: "Gen12 with iHD",
                sysfs: &[("renderD128", "0x8086", "0x9a49", "i915")],
                files: &["iHD", "i965"],
                expected: "renderD128=iHD",
            },
            Machine {
                // i965 doesn't drive Gen12 either, but it's what there is.
                name: "Gen12 with only i965",
                sysfs: &[("renderD128", "0x8086", "0x9a49", "i915")],
                files: &["i965"],
                expected: "renderD128=i965",
            },
            Machine {
                name: "Gen12 without a VA driver",
                sysfs: &[("renderD128", "0x8086", "0x9a49", "xe")],
                files: &[],
                expected: "renderD128=iHD",
            },
            Machine {
                name: "AMD and NVIDIA",
                sysfs: &[
                    ("renderD128", "0x10de", "0x2db8", "nvidia"),
                    ("renderD129", "0x1002", "0x73df", "amdgpu"),
                ],
                files: &["radeonsi"],
                expected: "renderD129=radeonsi",
            },
            Machine {
                name: "NVIDIA only",
                sysfs: &[("renderD128", "0x10de", "0x2db8", "nvidia")],
                files: &["iHD"],
                expected: "",
            },
            Machine {
                name: "virtio-gpu",
                sysfs: &[("renderD128", "0x1af4", "0x0010", "virtio_gpu")],
                files: &[],
                expected: "",
            },
            Machine {
                name: "no GPU",
                sysfs: &[],
                files: &[],
                expected: "",
            },
        ];
        for machine in machines {
            let name = machine.name;
            let tmp = tempfile::tempdir().unwrap();
            let sys = tmp.path().join("sys");
            fs::create_dir_all(&sys).unwrap();
            fake_sysfs(&sys, machine.sysfs);
            let drivers = fake_drivers(tmp.path(), machine.files);
            let nodes = scan_render_nodes(&sys, Path::new("/dev/dri"));
            let choice = effective_va_device(None, None, &nodes);
            let driver = choice
                .as_ref()
                .and_then(|c| node_for(&c.device, &nodes))
                .and_then(|n| libva_driver_for(n.vendor?, n.device_id, &drivers));
            let plan = plan_for(choice.as_ref(), &nodes, driver, &[]);
            let set = plan
                .changes
                .iter()
                .find(|c| c.key == LIBVA_DRIVER_NAME)
                .map(|c| c.value.as_str());
            let got = choice.as_ref().map_or_else(String::new, |c| {
                format!(
                    "{}={}",
                    c.device.trim_start_matches("/dev/dri/"),
                    set.unwrap_or("(unset)")
                )
            });
            assert_eq!(got, machine.expected, "{name}: {plan:?}");
            if machine.expected.is_empty() {
                assert!(set.is_none(), "{name}: {plan:?}");
                assert!(plan.notes[0].starts_with("no Intel or AMD render node"));
            }
        }
    }

    /// Where the h264_vaapi check failed with the node's driver, it runs
    /// again with Intel's other one: installed, and one that drives the
    /// chip (never iHD on a chip only i965 drives); never on AMD or NVIDIA.
    #[test]
    fn the_h264_vaapi_check_is_retried_with_intels_other_driver() {
        let tmp = tempfile::tempdir().unwrap();
        let both = fake_drivers(&tmp.path().join("both"), &["iHD", "i965", "radeonsi"]);
        let only_ihd = fake_drivers(&tmp.path().join("ihd"), &["iHD"]);
        let gpu = |vendor: u16, device_id: u16| RenderNode {
            device_id: Some(device_id),
            ..node(128, Some(vendor))
        };
        // Kaby Lake, whose free iHD build has no H.264 encoder: i965.
        let kaby_lake = gpu(VENDOR_INTEL, 0x5916);
        assert_eq!(retry_driver(&kaby_lake, "iHD", &both), Some("i965"));
        assert_eq!(retry_driver(&kaby_lake, "iHD", &only_ihd), None);
        // An inherited i965 on Tiger Lake, which i965 doesn't drive: iHD.
        assert_eq!(
            retry_driver(&gpu(VENDOR_INTEL, 0x9a49), "i965", &both),
            Some("iHD")
        );
        // A Haswell or a Cherry View: iHD doesn't drive them, i965 only
        // after an inherited iHD.
        for id in [0x0416, 0x22b0] {
            let old = gpu(VENDOR_INTEL, id);
            assert_eq!(retry_driver(&old, "i965", &both), None, "{id:#06x}");
            assert_eq!(retry_driver(&old, "iHD", &both), Some("i965"), "{id:#06x}");
        }
        for (vendor, failed) in [(VENDOR_AMD, "radeonsi"), (VENDOR_NVIDIA, "iHD")] {
            assert_eq!(retry_driver(&gpu(vendor, 0x73df), failed, &both), None);
        }
    }

    /// A VA export loads the driver its h264_vaapi check passed with: the
    /// node's, or the pick where only Intel's other driver passed, for
    /// that node and GPU only, while its file is installed. `main()` never
    /// loads the pick: GStreamer keeps the node's driver.
    #[test]
    fn exports_load_the_pick_for_its_node_and_gpu() {
        let tmp = tempfile::tempdir().unwrap();
        let drivers = fake_drivers(tmp.path(), &["iHD", "i965"]);
        let gpu = RenderNode {
            device_id: Some(0x5916),
            ..node(128, Some(VENDOR_INTEL))
        };
        let pick = VaDriverPick {
            device: gpu.path.clone(),
            device_id: gpu.device_id,
            driver: DRIVER_I965.into(),
        };
        assert_eq!(
            export_libva_driver(&gpu, Some("iHD"), &drivers, Some(&pick)).as_deref(),
            Some("i965")
        );
        assert_eq!(
            export_libva_driver(&gpu, Some("iHD"), &drivers, None).as_deref(),
            Some("iHD")
        );
        // Another GPU on that node, another node, or no i965 file: the
        // node's driver.
        let tmp = tempfile::tempdir().unwrap();
        let only_ihd = fake_drivers(tmp.path(), &["iHD"]);
        for (other, drivers) in [
            (
                RenderNode {
                    device_id: Some(0x9a49),
                    ..gpu.clone()
                },
                &drivers,
            ),
            (
                RenderNode {
                    path: "/dev/dri/renderD129".into(),
                    ..gpu.clone()
                },
                &drivers,
            ),
            (gpu.clone(), &only_ihd),
        ] {
            assert_eq!(
                export_libva_driver(&other, None, drivers, Some(&pick)).as_deref(),
                Some("iHD"),
                "{other:?}"
            );
        }
        // Never a driver of another vendor, nor anything for NVIDIA.
        let amd = RenderNode {
            device_id: gpu.device_id,
            ..node(128, Some(VENDOR_AMD))
        };
        assert_eq!(
            export_libva_driver(&amd, None, &drivers, Some(&pick)).as_deref(),
            Some("radeonsi")
        );
        let nvidia = node(128, Some(VENDOR_NVIDIA));
        assert_eq!(
            export_libva_driver(&nvidia, None, &drivers, Some(&pick)),
            None
        );
        // What main() pins ignores it.
        let nodes = [gpu.clone()];
        let choice = effective_va_device(None, None, &nodes).unwrap();
        let plan = plan_for(
            Some(&choice),
            &nodes,
            libva_driver_for(VENDOR_INTEL, gpu.device_id, &drivers),
            &[],
        );
        assert_eq!(plan.changes[0].value, "iHD");
    }

    /// Libva's directories: `LIBVA_DRIVERS_PATH`'s alone when it is set
    /// (absolute ones), else the standard ones that exist; only the drivers
    /// Qrec loads or keeps (r600, inherited) are looked for, the first
    /// directory's file winning.
    #[test]
    fn libva_driver_files_are_found_where_libva_looks() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        for (dir, names) in [
            (&a, &["i965"][..]),
            (&b, &["iHD", "i965", "nvidia", "r600"][..]),
        ] {
            fs::create_dir_all(dir).unwrap();
            for name in names {
                fs::write(dir.join(format!("{name}_drv_video.so")), "").unwrap();
            }
        }
        let path = format!("{}:relative:{}", a.display(), b.display());
        let drivers = VaDrivers::new(Some(&path));
        assert_eq!(drivers.dirs, [a.clone(), b.clone()]);
        assert_eq!(
            drivers.found.keys().map(String::as_str).collect::<Vec<_>>(),
            ["i965", "iHD", "r600"]
        );
        assert_eq!(
            drivers.file("i965"),
            Some(a.join("i965_drv_video.so").as_path())
        );
        assert_eq!(
            drivers.file("iHD"),
            Some(b.join("iHD_drv_video.so").as_path())
        );
        assert!(!drivers.has("nvidia") && !drivers.has("radeonsi"));
        // Unset: the standard directories, those that exist.
        let standard = VaDrivers::new(None);
        assert!(standard.dirs.iter().all(|d| d.is_dir()), "{standard:?}");
        assert!(standard_libva_dirs().contains(&PathBuf::from("/usr/lib/dri")));
        assert!(standard_libva_dirs().contains(&PathBuf::from("/run/opengl-driver/lib/dri")));
    }

    #[test]
    fn intel_generations_by_pci_device_id() {
        // Haswell, Ivy Bridge, Sandy Bridge, Bay Trail, Ironlake, G45, and
        // the Cherry View and Braswell Atoms.
        for id in [
            0x0416, 0x0a16, 0x0d26, 0x0166, 0x0126, 0x0f31, 0x0046, 0x2e22, 0x22b0, 0x22b1, 0x22b2,
            0x22b3,
        ] {
            assert!(intel_i965_only(id), "{id:#06x}");
        }
        // Broadwell, Broxton (0x0a84, a Haswell-like prefix), Skylake,
        // Apollo Lake, Kaby Lake, Tiger Lake, Alder Lake, Arc, Arrow Lake.
        for id in [
            0x1616, 0x0a84, 0x1916, 0x5a85, 0x5916, 0x9a49, 0x46a6, 0x56a0, 0x7d51,
        ] {
            assert!(!intel_i965_only(id), "{id:#06x}");
        }
        let none = VaDrivers::default();
        for id in [0x0416, 0x22b0] {
            assert_eq!(
                libva_driver_for(VENDOR_INTEL, Some(id), &none),
                Some("i965")
            );
        }
        assert_eq!(libva_driver_for(VENDOR_INTEL, None, &none), Some("iHD"));
        assert_eq!(libva_driver_for(VENDOR_NVIDIA, None, &none), None);
        assert_eq!(libva_driver_for(VENDOR_VIRTIO, None, &none), None);
        assert_eq!(other_intel_driver("iHD"), Some("i965"));
        assert_eq!(other_intel_driver("i965"), Some("iHD"));
        assert_eq!(other_intel_driver("radeonsi"), None);
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

    /// The plan with the chosen node's default driver (no driver files).
    fn plan_with(
        choice: Option<&VaChoice>,
        nodes: &[RenderNode],
        for_gui: bool,
        vars: &[(&str, &str)],
    ) -> GpuEnvPlan {
        let driver = choice
            .and_then(|c| node_for(&c.device, nodes))
            .and_then(|n| libva_driver_for(n.vendor?, n.device_id, &VaDrivers::default()));
        plan_inputs(choice, nodes, for_gui, driver, vars)
    }

    /// The CLI's plan with `driver` for the chosen node.
    fn plan_for(
        choice: Option<&VaChoice>,
        nodes: &[RenderNode],
        driver: Option<&str>,
        vars: &[(&str, &str)],
    ) -> GpuEnvPlan {
        plan_inputs(choice, nodes, false, driver, vars)
    }

    fn plan_inputs(
        choice: Option<&VaChoice>,
        nodes: &[RenderNode],
        for_gui: bool,
        va_driver: Option<&str>,
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
            va_driver,
            appdir: None,
            access: &[],
        })
    }

    #[test]
    fn gstreamer_keeps_its_own_registry_and_no_effect_plugins() {
        let env_of = |vars: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                vars.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let plan = plan_gst_env(&env_of(&[
            ("XDG_CACHE_HOME", "/home/me/.cache"),
            ("LV2_PATH", "/home/me/.lv2"),
        ]));
        let registry = plan.changes.iter().find(|c| c.key == GST_REGISTRY).unwrap();
        assert_eq!(
            registry.value,
            format!(
                "/home/me/.cache/qrec/gstreamer-registry-{}.bin",
                std::env::consts::ARCH
            )
        );
        let lv2 = plan.changes.iter().find(|c| c.key == "LV2_PATH").unwrap();
        assert_eq!(
            (lv2.value.as_str(), lv2.previous.as_deref()),
            ("", Some("/home/me/.lv2"))
        );
        // Unset paths are emptied too (their defaults would be scanned).
        assert!(
            plan.changes
                .iter()
                .any(|c| c.key == "FREI0R_PATH" && c.value.is_empty())
        );

        // A registry the user chose and already-empty paths are left alone;
        // HOME stands in for XDG_CACHE_HOME.
        let plan = plan_gst_env(&env_of(&[
            ("GST_REGISTRY_1_0", "/tmp/mine.bin"),
            ("LV2_PATH", ""),
            ("LADSPA_PATH", ""),
            ("FREI0R_PATH", ""),
        ]));
        assert!(plan.changes.is_empty(), "{plan:?}");
        let plan = plan_gst_env(&env_of(&[("HOME", "/home/me")]));
        assert!(plan.changes[0].value.starts_with("/home/me/.cache/qrec/"));
        let plan = plan_gst_env(&env_of(&[]));
        assert!(plan.changes.iter().all(|c| c.key != GST_REGISTRY));
        assert!(plan.notes[0].contains("shared registry"));
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

        // Explicit NVIDIA: untouched (the GUI's own renderer variable aside).
        let nvidia = effective_va_device(None, Some("/dev/dri/renderD128"), &nodes).unwrap();
        let plan = plan_with(
            Some(&nvidia),
            &nodes,
            true,
            &[(LIBVA_DRIVER_NAME, "nvidia")],
        );
        assert!(
            plan.changes.iter().all(|c| c.key == GDK_DISABLE),
            "{plan:?}"
        );
        assert!(plan.notes[0].contains("configured explicitly"));

        // No usable node: untouched.
        let plan = plan_with(None, &[node(128, Some(VENDOR_NVIDIA))], false, &[]);
        assert!(plan.changes.is_empty());
    }

    /// A child that opens a node (a probe, the h264_vaapi check) gets the
    /// driver `main()` pinned when it fits the node's vendor, legacy ones
    /// included (the rule above), so the child and the host share one
    /// GStreamer registry state; otherwise the node's. Nothing for NVIDIA.
    #[test]
    fn child_processes_keep_a_fitting_libva_driver() {
        let tmp = tempfile::tempdir().unwrap();
        let only_i965 = fake_drivers(tmp.path(), &["i965"]);
        let none = VaDrivers::default();
        let haswell = RenderNode {
            device_id: Some(0x0416),
            ..node(128, Some(VENDOR_INTEL))
        };
        for (inherited, gpu, drivers, set) in [
            (
                Some("i965"),
                node(128, Some(VENDOR_INTEL)),
                &none,
                Some("i965"),
            ),
            (
                Some("iHD"),
                node(128, Some(VENDOR_INTEL)),
                &none,
                Some("iHD"),
            ),
            (Some("iHD"), haswell.clone(), &none, Some("iHD")),
            (
                Some("r600"),
                node(128, Some(VENDOR_AMD)),
                &none,
                Some("r600"),
            ),
            (
                Some("radeonsi"),
                node(128, Some(VENDOR_AMD)),
                &none,
                Some("radeonsi"),
            ),
            (None, node(128, Some(VENDOR_INTEL)), &none, Some("iHD")),
            (
                None,
                node(128, Some(VENDOR_INTEL)),
                &only_i965,
                Some("i965"),
            ),
            (None, haswell, &none, Some("i965")),
            (Some(""), node(128, Some(VENDOR_INTEL)), &none, Some("iHD")),
            (
                Some("radeonsi"),
                node(128, Some(VENDOR_INTEL)),
                &none,
                Some("iHD"),
            ),
            (
                Some("i965"),
                node(128, Some(VENDOR_AMD)),
                &none,
                Some("radeonsi"),
            ),
            (None, node(128, Some(VENDOR_AMD)), &none, Some("radeonsi")),
            (Some("nvidia"), node(128, Some(VENDOR_NVIDIA)), &none, None),
            (None, node(128, Some(VENDOR_NVIDIA)), &none, None),
            (None, node(128, None), &none, None),
        ] {
            assert_eq!(
                child_libva_driver(&gpu, inherited, drivers).as_deref(),
                set,
                "{inherited:?} on {gpu:?}"
            );
        }
    }

    /// GUI H-3: the window renders with GSK GL (`GDK_DISABLE=vulkan`) unless
    /// the user chose GTK's renderer; then both variables are left alone and
    /// a note says so (`qrec doctor` prints it).
    #[test]
    fn the_gui_disables_vulkan_unless_the_user_chose_a_renderer() {
        let intel_only = [node(129, Some(VENDOR_INTEL))];
        let intel = effective_va_device(None, None, &intel_only).unwrap();
        let gdk = |plan: &GpuEnvPlan| -> Vec<(String, String)> {
            plan.changes
                .iter()
                .filter(|c| c.key == GDK_DISABLE || c.key == GSK_RENDERER)
                .map(|c| (c.key.clone(), c.value.clone()))
                .collect()
        };
        let plan = plan_with(Some(&intel), &intel_only, true, &[]);
        assert_eq!(gdk(&plan), [(GDK_DISABLE.to_owned(), "vulkan".to_owned())]);
        assert!(plan.notes.is_empty(), "{plan:?}");
        // Also without any VA node.
        let plan = plan_with(None, &[], true, &[]);
        assert_eq!(gdk(&plan), [(GDK_DISABLE.to_owned(), "vulkan".to_owned())]);
        // Not for the CLI and hosts, which draw nothing.
        assert!(gdk(&plan_with(Some(&intel), &intel_only, false, &[])).is_empty());

        for (key, value) in [(GDK_DISABLE, "gl"), (GSK_RENDERER, "cairo")] {
            for for_gui in [true, false] {
                let plan = plan_with(Some(&intel), &intel_only, for_gui, &[(key, value)]);
                assert!(gdk(&plan).is_empty(), "{key}: {plan:?}");
                let note = plan.notes.last().expect("a note");
                assert!(
                    note.starts_with(&format!("{key}={value} set by the user")),
                    "{note}"
                );
            }
        }
        let plan = plan_with(
            Some(&intel),
            &intel_only,
            true,
            &[(GDK_DISABLE, "vulkan,dmabuf"), (GSK_RENDERER, "ngl")],
        );
        assert!(gdk(&plan).is_empty());
        assert!(
            plan.notes[0].starts_with("GDK_DISABLE=vulkan,dmabuf and GSK_RENDERER=ngl"),
            "{plan:?}"
        );
        // An empty value counts as unset.
        let plan = plan_with(Some(&intel), &intel_only, true, &[(GSK_RENDERER, "")]);
        assert_eq!(gdk(&plan).len(), 1);
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
                VK_ICD_FILENAMES,
                GDK_DISABLE
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
            [LIBVA_DRIVER_NAME, GDK_DISABLE]
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
        assert_eq!(keys(&plan), [LIBVA_DRIVER_NAME, GDK_DISABLE]);
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

        let dirs = graphics_data_dirs(Some("/a:relative:/b"), None);
        assert_eq!(
            dirs,
            ["/run/opengl-driver/share", "/a", "/b", "/etc"].map(PathBuf::from)
        );
        assert!(graphics_data_dirs(None, None).contains(&PathBuf::from("/usr/share")));
    }

    /// Inside an AppImage its own manifests come first, before NixOS's
    /// driver dir and any host dir `XDG_DATA_DIRS` names before the
    /// bundle's (each dir counts once): the host's Mesa never loads among
    /// the bundled libraries. On the NixOS dev box `/run/opengl-driver`
    /// holds a Mesa manifest, which the bundle's beats.
    #[test]
    fn an_appimage_finds_its_own_manifests_first() {
        let tmp = tempfile::tempdir().unwrap();
        let appdir = tmp.path().join("appimage_extracted_qrec1234");
        let bundle = appdir.join("share");
        let host = tmp.path().join("host/share");
        for share in [&bundle, &host] {
            fs::create_dir_all(share.join("glvnd/egl_vendor.d")).unwrap();
            fs::create_dir_all(share.join("vulkan/icd.d")).unwrap();
            fs::write(share.join("glvnd/egl_vendor.d/50_mesa.json"), "{}").unwrap();
            fs::write(share.join("vulkan/icd.d/intel_icd.x86_64.json"), "{}").unwrap();
        }
        let appdir = appdir.to_string_lossy().into_owned();
        let xdg = format!("{}:{}:/usr/share:/etc", host.display(), bundle.display());
        let dirs = graphics_data_dirs(Some(&xdg), Some(&appdir));
        assert_eq!(
            dirs,
            [
                bundle.clone(),
                PathBuf::from("/run/opengl-driver/share"),
                host.clone(),
                PathBuf::from("/usr/share"),
                PathBuf::from("/etc"),
            ]
        );
        assert_eq!(
            find_mesa_egl_vendor(&dirs).unwrap(),
            bundle
                .join("glvnd/egl_vendor.d/50_mesa.json")
                .to_string_lossy()
        );
        assert_eq!(
            find_vulkan_icd(&dirs, VENDOR_INTEL).unwrap(),
            bundle
                .join("vulkan/icd.d/intel_icd.x86_64.json")
                .to_string_lossy()
        );
        // Without an AppImage, NixOS's comes first, as before.
        assert_eq!(
            graphics_data_dirs(Some(&xdg), None)[0],
            PathBuf::from("/run/opengl-driver/share")
        );
    }

    /// Qrec runs from an AppImage when `$APPDIR` holds its executable, as
    /// given or with symlinks resolved; an `APPDIR` leaked into a qrec
    /// installed elsewhere, or a relative one, doesn't count.
    #[test]
    fn the_appimage_is_the_folder_that_holds_this_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let appdir = root.join(".mount_Qrec1a2b3c");
        let exe = appdir.join("bin/qrec");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        let dir = appdir.as_os_str();
        assert_eq!(
            appimage_dir(Some(dir), Some(&exe)).as_deref(),
            Some(appdir.to_str().unwrap())
        );
        // Through a symlink to the mount.
        let link = root.join("link");
        std::os::unix::fs::symlink(&appdir, &link).unwrap();
        assert!(appimage_dir(Some(link.as_os_str()), Some(&exe)).is_some());
        // A host qrec started from another AppImage's terminal.
        let host = root.join("usr/bin/qrec");
        fs::create_dir_all(host.parent().unwrap()).unwrap();
        fs::write(&host, "").unwrap();
        assert_eq!(appimage_dir(Some(dir), Some(&host)), None);
        for odd in ["", "relative", "."] {
            assert_eq!(
                appimage_dir(Some(OsStr::new(odd)), Some(&exe)),
                None,
                "{odd:?}"
            );
        }
        assert_eq!(appimage_dir(None, Some(&exe)), None);
        assert_eq!(appimage_dir(Some(dir), None), None);
    }

    /// access(2) says whether a node's device file may be opened, without
    /// opening it: a container that didn't get the device has none.
    #[test]
    fn node_access_checks_without_opening() {
        let tmp = tempfile::tempdir().unwrap();
        let there = tmp.path().join("renderD128");
        fs::write(&there, "").unwrap();
        let at = |path: &Path| RenderNode {
            path: path.to_string_lossy().into_owned(),
            ..node(128, Some(VENDOR_INTEL))
        };
        let access = node_access(&[at(&there), at(&tmp.path().join("renderD129"))]);
        assert_eq!(access[0].error, None);
        assert_eq!(
            access[1].error.as_deref(),
            Some("No such file or directory (os error 2)")
        );
        // Read-only: not enough to draw with (root may open anything).
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&there, fs::Permissions::from_mode(0o444)).unwrap();
        // SAFETY: geteuid(2) has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(
                node_access(&[at(&there)])[0].error.as_deref(),
                Some("Permission denied (os error 13)")
            );
        }
    }

    /// The GUI's plan from an AppImage (`appdir`) whose start (sharun) set
    /// `env`, with the bundle's manifests and `access` to the nodes.
    fn appimage_plan(
        nodes: &[RenderNode],
        access: &[NodeAccess],
        vars: &[(&str, &str)],
    ) -> GpuEnvPlan {
        let env = |key: &str| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        };
        let choice = effective_va_device(None, None, nodes);
        let driver = choice
            .as_ref()
            .and_then(|c| node_for(&c.device, nodes))
            .and_then(|n| libva_driver_for(n.vendor?, n.device_id, &VaDrivers::default()));
        plan_gpu_env(&GpuEnvInputs {
            choice: choice.as_ref(),
            nodes,
            for_gui: true,
            env: &env,
            mesa_egl_vendor: Some(format!("{APPDIR}/share/glvnd/egl_vendor.d/50_mesa.json")),
            vulkan_icd: Some(format!("{APPDIR}/share/vulkan/icd.d/intel_icd.json")),
            va_driver: driver,
            appdir: Some(APPDIR),
            access,
        })
    }

    const APPDIR: &str = "/tmp/.mount_Qrec1a2b3c";

    /// Every node there, as access(2) found it: `None` opens.
    fn opens(nodes: &[RenderNode], error: Option<&str>) -> Vec<NodeAccess> {
        nodes
            .iter()
            .map(|n| NodeAccess {
                path: n.path.clone(),
                error: error.map(str::to_owned),
            })
            .collect()
    }

    /// Inside an AppImage the values pointing into it are the bundle's:
    /// sharun's `VK_DRIVER_FILES` (the bundled drivers, NVIDIA's host one
    /// added) and, with NVIDIA's module loaded, its
    /// `__EGL_VENDOR_LIBRARY_FILENAMES` (NVIDIA's vendor first). Qrec's
    /// rule replaces them: EGL on the bundled Mesa whenever the chosen
    /// node is Intel's or AMD's (NVIDIA there or not), Vulkan on the
    /// chosen vendor's bundled driver when NVIDIA is there. A user's own
    /// values, which point elsewhere, stay.
    #[test]
    fn an_appimage_replaces_its_own_gpu_values_and_keeps_the_users() {
        let keys = |plan: &GpuEnvPlan| -> Vec<(String, String, Option<String>)> {
            plan.changes
                .iter()
                .map(|c| (c.key.clone(), c.value.clone(), c.previous.clone()))
                .collect()
        };
        let set = |key: &str, value: String, previous: Option<&str>| {
            (key.to_owned(), value, previous.map(str::to_owned))
        };
        let mesa = format!("{APPDIR}/share/glvnd/egl_vendor.d/50_mesa.json");
        let anv = format!("{APPDIR}/share/vulkan/icd.d/intel_icd.json");
        let sharun_egl = format!("/usr/share/glvnd/egl_vendor.d/10_nvidia.json:{mesa}");
        let sharun_vk =
            format!("{APPDIR}/share/vulkan/icd.d:/usr/share/vulkan/icd.d/nvidia_icd.json");

        // The dev box (Intel and NVIDIA, NVIDIA's module loaded).
        let dev_box = [
            node(128, Some(VENDOR_INTEL)),
            node(129, Some(VENDOR_NVIDIA)),
        ];
        let plan = appimage_plan(
            &dev_box,
            &opens(&dev_box, None),
            &[
                (EGL_VENDOR_FILES, &sharun_egl),
                (VK_DRIVER_FILES, &sharun_vk),
            ],
        );
        assert_eq!(
            keys(&plan),
            [
                set(LIBVA_DRIVER_NAME, "iHD".into(), None),
                set(EGL_VENDOR_FILES, mesa.clone(), Some(&sharun_egl)),
                set(VK_DRIVER_FILES, anv.clone(), Some(&sharun_vk)),
                set(VK_ICD_FILENAMES, anv.clone(), None),
                set(GDK_DISABLE, "vulkan".into(), None),
            ]
        );
        assert_eq!(
            plan.notes,
            [
                "__EGL_VENDOR_LIBRARY_FILENAMES came with the AppImage, not from the user: Qrec \
              pins the bundled Mesa"
            ]
        );

        // Intel alone: sharun set Vulkan only; EGL still goes to the bundled
        // Mesa, never the host's, and Vulkan stays the bundle's.
        let intel = [node(128, Some(VENDOR_INTEL))];
        let bundled_vk = format!("{APPDIR}/share/vulkan/icd.d");
        let plan = appimage_plan(
            &intel,
            &opens(&intel, None),
            &[(VK_DRIVER_FILES, &bundled_vk)],
        );
        assert_eq!(
            keys(&plan),
            [
                set(LIBVA_DRIVER_NAME, "iHD".into(), None),
                set(EGL_VENDOR_FILES, mesa.clone(), None),
                set(GDK_DISABLE, "vulkan".into(), None),
            ]
        );
        assert!(plan.notes.is_empty(), "{plan:?}");

        // The user's own values, outside the AppImage, stay.
        let plan = appimage_plan(
            &dev_box,
            &opens(&dev_box, None),
            &[
                (
                    EGL_VENDOR_FILES,
                    "/usr/share/glvnd/egl_vendor.d/10_nvidia.json",
                ),
                (VK_ICD_FILENAMES, "/usr/share/vulkan/icd.d/nvidia_icd.json"),
                (VK_DRIVER_FILES, &sharun_vk),
            ],
        );
        assert_eq!(
            keys(&plan),
            [
                set(LIBVA_DRIVER_NAME, "iHD".into(), None),
                set(GDK_DISABLE, "vulkan".into(), None),
            ]
        );
        assert_eq!(
            plan.notes,
            [
                "__EGL_VENDOR_LIBRARY_FILENAMES is set by the user; left alone",
                "VK_DRIVER_FILES/VK_ICD_FILENAMES are set by the user; left alone",
            ]
        );

        // NVIDIA alone: no Intel or AMD node to pin to; sharun's NVIDIA-first
        // EGL stays, so NVIDIA's driver draws.
        let nvidia = [node(128, Some(VENDOR_NVIDIA))];
        let plan = appimage_plan(
            &nvidia,
            &opens(&nvidia, None),
            &[(EGL_VENDOR_FILES, &sharun_egl)],
        );
        assert_eq!(keys(&plan), [set(GDK_DISABLE, "vulkan".into(), None)]);

        // A host qrec (no AppImage) takes the same values for the user's.
        let choice = effective_va_device(None, None, &dev_box).unwrap();
        let plan = plan_with(
            Some(&choice),
            &dev_box,
            true,
            &[
                (EGL_VENDOR_FILES, &sharun_egl),
                (VK_DRIVER_FILES, &sharun_vk),
            ],
        );
        assert_eq!(plan.notes.len(), 2, "{plan:?}");
        assert!(plan.changes.iter().all(|c| c.key != EGL_VENDOR_FILES));
    }

    /// Inside an AppImage with no render node it may open (a container
    /// without `/dev/dri`, a VM without 3D, a user the node's group leaves
    /// out), the window draws with Cairo: `GDK_DISABLE=gl,vulkan`, noted
    /// with why. Not when one node opens, not outside an AppImage (a
    /// distribution's Mesa has llvmpipe), never over the user's renderer.
    #[test]
    fn an_appimage_without_a_render_node_draws_with_cairo() {
        let gdk = |plan: &GpuEnvPlan| -> Vec<String> {
            plan.changes
                .iter()
                .filter(|c| c.key == GDK_DISABLE || c.key == GSK_RENDERER)
                .map(|c| format!("{}={}", c.key, c.value))
                .collect()
        };
        let container = [
            node(128, Some(VENDOR_INTEL)),
            node(129, Some(VENDOR_NVIDIA)),
        ];
        let missing = opens(&container, Some("No such file or directory (os error 2)"));
        let plan = appimage_plan(&container, &missing, &[]);
        assert_eq!(gdk(&plan), ["GDK_DISABLE=gl,vulkan"]);
        assert_eq!(
            plan.notes.last().unwrap(),
            "no render node can be opened (/dev/dri/renderD128: No such file or directory \
             (os error 2); /dev/dri/renderD129: No such file or directory (os error 2)): the \
             window draws with Cairo (GDK_DISABLE=gl,vulkan), since the AppImage's Mesa has no \
             llvmpipe and its softpipe is slow"
        );
        // No node in sysfs at all.
        let plan = appimage_plan(&[], &[], &[]);
        assert_eq!(gdk(&plan), ["GDK_DISABLE=gl,vulkan"]);
        assert!(
            plan.notes
                .last()
                .unwrap()
                .starts_with("no GPU render node: ")
        );
        // One node opens (the container got the Intel one): GSK GL.
        let mut one = missing.clone();
        one[0].error = None;
        assert_eq!(
            gdk(&appimage_plan(&container, &one, &[])),
            ["GDK_DISABLE=vulkan"]
        );
        // The user's renderer stays, and the note says what Qrec would set.
        let plan = appimage_plan(&container, &missing, &[(GSK_RENDERER, "ngl")]);
        assert!(gdk(&plan).is_empty(), "{plan:?}");
        assert_eq!(
            plan.notes.last().unwrap(),
            "GSK_RENDERER=ngl set by the user: the Qrec window renders as it says (Qrec would \
             set GDK_DISABLE=gl,vulkan)"
        );
        // A host qrec without a node keeps GL: llvmpipe draws it. The CLI
        // and hosts draw nothing.
        let choice = effective_va_device(None, None, &container);
        let env = |_: &str| -> Option<String> { None };
        let plan = |for_gui: bool, appdir: Option<&str>| {
            plan_gpu_env(&GpuEnvInputs {
                choice: choice.as_ref(),
                nodes: &container,
                for_gui,
                env: &env,
                mesa_egl_vendor: None,
                vulkan_icd: None,
                va_driver: Some(DRIVER_IHD),
                appdir,
                access: &missing,
            })
        };
        assert_eq!(gdk(&plan(true, None)), ["GDK_DISABLE=vulkan"]);
        assert!(gdk(&plan(false, Some(APPDIR))).is_empty());
    }

    /// The startup report as the doctor and the logs print it.
    #[test]
    fn the_startup_report_reads_a_line_each() {
        let report = GpuReport {
            nodes: Vec::new(),
            choice: None,
            changes: vec![
                EnvChange {
                    key: LIBVA_DRIVER_NAME.into(),
                    value: "iHD".into(),
                    previous: Some("nvidia".into()),
                },
                EnvChange {
                    key: GDK_DISABLE.into(),
                    value: "gl,vulkan".into(),
                    previous: None,
                },
            ],
            notes: vec!["no GPU render node: …".into()],
            applied: true,
        };
        assert_eq!(
            report.lines(),
            [
                "set LIBVA_DRIVER_NAME=iHD (was nvidia)",
                "set GDK_DISABLE=gl,vulkan",
                "no GPU render node: …",
            ]
        );
        let unapplied = GpuReport {
            applied: false,
            ..report
        };
        assert_eq!(
            unapplied.lines()[2],
            "those changes were not applied (more than one thread)"
        );
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
            appdir: None,
        });
        assert_eq!(report.choice.as_ref().unwrap().origin, DeviceOrigin::Config);
        // No GPU variable; the GStreamer ones (step 4) aren't applied here.
        assert!(
            report
                .changes
                .iter()
                .all(|c| c.key == GST_REGISTRY || EFFECT_PLUGIN_PATHS.contains(&c.key.as_str())),
            "{report:?}"
        );
        assert_eq!(report.applied, report.changes.is_empty(), "{report:?}");
        assert!(startup_report().is_some());
    }

    #[test]
    fn vendor_names() {
        assert_eq!(vendor_name(VENDOR_INTEL), "Intel");
        assert_eq!(vendor_name(VENDOR_VIRTIO), "virtio-gpu");
        assert_eq!(vendor_name(VENDOR_VMWARE), "VMware");
        assert_eq!(vendor_name(VENDOR_QXL), "QXL");
        assert_eq!(vendor_name(0x1234), "0x1234");
        assert!(is_virtual_gpu(VENDOR_QXL) && !is_virtual_gpu(VENDOR_NVIDIA));
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

    /// VA H.264 encoders by name: the full one and the low-power one (the
    /// only one of Intel Arc and Meteor Lake), per node or plain; nothing
    /// else, not even their decoders or other codecs' encoders.
    #[test]
    fn va_h264_encoders_are_told_apart_by_name() {
        use VaH264Kind::{Full, LowPower};
        for (name, kind) in [
            ("vah264enc", Some(Full)),
            ("varenderD129h264enc", Some(Full)),
            ("vah264lpenc", Some(LowPower)),
            ("varenderD129h264lpenc", Some(LowPower)),
            ("varenderD130h264lpenc", Some(LowPower)),
            ("vah264dec", None),
            ("vah265enc", None),
            ("vah265lpenc", None),
            ("vaav1enc", None),
            ("vajpegenc", None),
            ("x264enc", None),
            ("openh264enc", None),
            ("nvh264enc", None),
            ("qsvh264enc", None),
            ("vaapih264enc", None),
            ("h264enc", None),
        ] {
            assert_eq!(va_h264_kind(name), kind, "{name}");
        }
    }

    #[test]
    fn probes_run_in_a_child_with_a_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        // The probe is a fake node: no real device is ever named.
        let probe = Probe::H264Encoders {
            device: Some(tmp.path().join("renderD999").to_string_lossy().into_owned()),
        };
        assert_eq!(Probe::parse(&probe.env_value()), Some(probe.clone()));
        let nodeless = Probe::H264Encoders { device: None };
        assert_eq!(nodeless.env_value(), "h264-encoders");
        assert_eq!(Probe::parse("h264-encoders"), Some(nodeless));
        assert_eq!(Probe::parse("h264-encoders:"), None);
        assert_eq!(Probe::parse("va-h264-factory:/dev/dri/renderD129"), None);
        assert_eq!(Probe::parse("something-else:/x"), None);

        // The child sees the request and answers on stdout.
        let log = tmp.path().join("probe.log");
        let exe = fake_child(
            tmp.path(),
            r#"echo "probing $QREC_GPU_PROBE" >&2; echo noise; echo '{"factory":"varenderD129h264enc","lpFactory":"varenderD129h264lpenc","software":{"x264":false,"openh264":true}}'"#,
        );
        let answer = run_probe_child(&exe, &probe, Some(&log), PROBE_TIMEOUT).unwrap();
        assert_eq!(
            answer,
            ProbeAnswer {
                factory: Some("varenderD129h264enc".into()),
                lp_factory: Some("varenderD129h264lpenc".into()),
                software: Some(GstH264Caps {
                    x264: false,
                    openh264: true
                }),
                error: None,
            }
        );
        let logged = fs::read_to_string(&log).unwrap();
        assert!(logged.contains("probing h264-encoders:"), "{logged}");
        // An answer from before the low-power and software encoders.
        let exe = fake_child(tmp.path(), r#"echo '{"factory":"vah264enc"}'"#);
        let answer = run_probe_child(&exe, &probe, None, PROBE_TIMEOUT).unwrap();
        assert_eq!(
            (
                answer.factory.as_deref(),
                answer.lp_factory,
                answer.software
            ),
            (Some("vah264enc"), None, None)
        );

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

    /// One probe child, with the host's driver when it fits the node
    /// (else the node's), whatever it finds: never a second one with
    /// another driver, whose GStreamer would rescan the VA plugin and
    /// rewrite the registry the host shares (the plugin notes
    /// `LIBVA_DRIVER_NAME`), so the capabilities' GStreamer key would
    /// change at every start. NVIDIA is refused before any child.
    #[test]
    fn the_probe_child_runs_once_with_the_hosts_driver() {
        let tmp = tempfile::tempdir().unwrap();
        // The fake node's path is a file in the temp dir: no device.
        let path = tmp.path().join("renderD128");
        fs::write(&path, "").unwrap();
        let gpu = |vendor: u16, device_id: u16| RenderNode {
            path: path.to_string_lossy().into_owned(),
            vendor: Some(vendor),
            device_id: Some(device_id),
            driver: None,
        };
        let probe = Probe::H264Encoders {
            device: Some(path.to_string_lossy().into_owned()),
        };
        // GStreamer's registry as the fake child sees it: the driver of the
        // last scan, rewritten by a child that runs with another one.
        let registry = tmp.path().join("registry.bin");
        let log = tmp.path().join("probe.log");
        let exe = fake_child(
            tmp.path(),
            &format!(
                r#"echo "run $LIBVA_DRIVER_NAME" >&2
[ "$(cat '{registry}')" = "$LIBVA_DRIVER_NAME" ] || echo "$LIBVA_DRIVER_NAME" > '{registry}'
echo '{{"factory":null,"software":{{"x264":true,"openh264":false}}}}'"#,
                registry = registry.display()
            ),
        );
        let both = fake_drivers(&tmp.path().join("both"), &["iHD", "i965", "radeonsi"]);
        let run = |node: &RenderNode, inherited: Option<&str>| {
            let _ = fs::remove_file(&log);
            let answer = probe_node(
                &exe,
                &probe,
                Some(node),
                inherited,
                &both,
                Some(&log),
                PROBE_TIMEOUT,
            );
            let runs: Vec<String> = fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            (answer, runs)
        };

        // The host's driver, without a VA encoder (Kaby Lake's free iHD,
        // a Haswell's i965): one run, the registry left as the host wrote it.
        for (id, host) in [(0x5916, "iHD"), (0x0416, "i965"), (0x22b0, "i965")] {
            fs::write(&registry, format!("{host}\n")).unwrap();
            let before = fs::metadata(&registry).unwrap().modified().unwrap();
            let (answer, runs) = run(&gpu(VENDOR_INTEL, id), Some(host));
            let answer = answer.unwrap();
            assert_eq!(
                (answer.factory, answer.lp_factory, answer.error),
                (None, None, None)
            );
            assert_eq!(runs, [format!("run {host}")], "{id:#06x}");
            assert_eq!(fs::read_to_string(&registry).unwrap(), format!("{host}\n"));
            assert_eq!(fs::metadata(&registry).unwrap().modified().unwrap(), before);
        }
        // A host driver that doesn't fit: the node's (i965 on a Haswell or
        // a Cherry View by its PCI id, radeonsi on AMD), once.
        for (vendor, id, inherited, driver) in [
            (VENDOR_INTEL, 0x0416, Some("radeonsi"), "i965"),
            (VENDOR_INTEL, 0x22b0, None, "i965"),
            (VENDOR_AMD, 0x73df, Some("iHD"), "radeonsi"),
        ] {
            let (answer, runs) = run(&gpu(vendor, id), inherited);
            assert!(answer.is_ok());
            assert_eq!(runs, [format!("run {driver}")], "{id:#06x}");
        }
        // NVIDIA is never probed: no child at all.
        let (answer, runs) = run(&gpu(VENDOR_NVIDIA, 0x2db8), None);
        let err = answer.unwrap_err();
        assert!(err.contains("never probed"), "{err}");
        assert!(runs.is_empty());
    }
}
