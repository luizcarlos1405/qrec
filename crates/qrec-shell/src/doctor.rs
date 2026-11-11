//! `qrec doctor` (A§8.12 `DoctorReport`, SPEC §3.3). Owner: media.
//!
//! One check per runtime dependency, each with a hint when it fails and
//! what needs it ([`Feature`], JSON `neededFor`):
//!
//! - the app itself: GStreamer ≥ 1.24 (initialized under the init
//!   watchdog), GES, the preview's elements and a software H.264 decoder,
//!   ffmpeg/ffprobe with the decoders and the demuxer the takes need;
//! - screen capture, as the session's screen backend records it
//!   (`capture::ScreenBackend`): under the portal, pipewiresrc, the
//!   PipeWire daemon's socket (a file system check) and the ScreenCast
//!   portal (D-Bus, with a timeout; v3 works with the picker on every
//!   take); on X11, ximagesrc and the X server (connected within 2 s, its
//!   RandR version and the screens discovery lists), never the portal;
//!   without a display, nothing records screens;
//! - cameras: v4l2src; audio capture: pulsesrc, `pactl` 16 or newer and
//!   the sound server it reaches (PipeWire or PulseAudio);
//! - recording: an H.264 encoder (the `va*h264enc`, else the
//!   `va*h264lpenc`, bound to the render node, which a probe child finds,
//!   else x264enc, else openh264enc), h264parse, matroskamux and flacenc;
//! - each export format: ffmpeg's encoders (for MP4's video, h264_vaapi
//!   on the render node once the h264_vaapi check passed, else libx264,
//!   else libopenh264).
//!
//! Checks nothing needs only inform: the portal's app registry, an X
//! server without RandR (its whole screen is one) and the VA-API report
//! (render nodes, the effective device, `LIBVA_DRIVER_NAME`, the node's
//! libva driver and its file, the h264_vaapi export check as a 10 s child
//! process, run again with Intel's other driver where the node's fails, as
//! capability detection does; NVIDIA nodes are never probed). A machine
//! without an Intel or AMD GPU (NVIDIA only, a virtual machine's GPU, none)
//! has nothing to fix there: its VA-API rows say what encodes on the CPU
//! instead, as information, and so does `gst.vah264enc` on the chips only
//! i965 drives, which GStreamer's VA plugin doesn't encode with. Before the
//! VA-API rows ask an Intel or AMD node to encode, the node is opened and
//! closed again: sysfs may list a GPU whose device file a container didn't
//! get, or that this user may not open, and then the rows say that instead
//! of naming a driver to install (an NVIDIA node is never opened). The
//! report also says where Qrec runs (`platform::current`, and the screen
//! backend: its session line; the AppImage it runs from, with `APPDIR`) and
//! how to bind the global shortcuts there (SPEC §10.2,
//! `keymap::global_snippet`: niri's KDL, sway's `bindsym`, GNOME's custom
//! shortcuts, …, run by the AppImage's path from one).
//!
//! Only a failed app check is `missing_dependency` (JSON `required`, exit
//! 5, and the GUI's "The swamp is missing something" page, which lists
//! those alone). Any other failure turns its features off (`featuresOff`,
//! a line each in the text): Qrec still opens projects, edits and exports
//! with what it has.
//!
//! A failed check's hint is the fix where Qrec runs, from the core's table
//! (`platform::install_hint` for the probe's platform): this
//! distribution's packages and install command, the desktop's ScreenCast
//! backend, `nix develop` in a Nix shell; inside an AppImage, no package
//! for what it bundles. The check also carries it as `fix` (packages,
//! command, note), whose command S23 offers to copy.
//!
//! Everything that touches the system goes through [`Probe`], so tests run
//! against a fake machine.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use qrec_core::ctx::ScreenBackendKind;
use qrec_core::keymap::{GlobalSnippet, global_snippet};
use qrec_core::media_argv::vaapi_check_argv;
use qrec_core::platform::{Hint, Need, VaGpu, install_hint};
use qrec_core::{CoreError, ErrorCode};
use serde::{Deserialize, Serialize};

use crate::capture::ScreenBackend;
use crate::gpu::{
    self, DeviceOrigin, VENDOR_AMD as PCI_AMD, VENDOR_INTEL as PCI_INTEL,
    VENDOR_NVIDIA as PCI_NVIDIA, vendor_name,
};
use crate::platform::{Desktop, HostPlatform, SessionType};
use crate::sources::{self, X11Server};

/// The exit code when an app check fails (`missing_dependency`).
pub const MISSING_DEPENDENCY_EXIT: u8 = 5;

/// `ffmpeg -version`, its lists (`-encoders`, `-decoders`, `-demuxers`)
/// and the VA-API check (A§8.7).
pub const FFMPEG_TIMEOUT: Duration = Duration::from_secs(10);

/// D-Bus property reads of the portal (may start xdg-desktop-portal).
pub const PORTAL_TIMEOUT: Duration = Duration::from_secs(5);

/// The oldest GStreamer Qrec runs on.
pub const GSTREAMER_MIN: (u32, u32) = (1, 24);

/// The oldest `pactl` Qrec reads (`--format=json` came in PulseAudio 16).
pub const PACTL_MIN: (u32, u32) = (16, 0);

/// What `qrec doctor` found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
    /// The features a failed check turns off, in [`Feature::ALL`]'s order
    /// (never [`Feature::App`]: that is [`DoctorReport::missing`]).
    pub features_off: Vec<FeatureOff>,
    /// Where Qrec runs, the AppImage it runs from included (the texts
    /// print "AppImage: <path> (APPDIR=<dir>)" under the session line).
    pub platform: HostPlatform,
    /// How screens are listed and recorded there, which the screen checks
    /// follow: the session's `ScreenBackend`, with the same values as
    /// `qrec sources list` (`portal`, `x11` or `none`). Under the fake
    /// capture backend this is still the session's backend, where
    /// `sources list` says `portal`.
    pub screen_backend: ScreenBackendKind,
    /// The session line for people: the session, its desktop and the
    /// screen backend with its display, "X11 (xfce) · screens: x11
    /// (DISPLAY=:0)" (the text prints it as "Session: …").
    pub session: String,
    /// How to bind the global shortcuts on `platform`'s desktop.
    pub shortcuts: GlobalSnippet,
    /// SPEC §10.2's niri binds, whatever the desktop: `shortcuts` replaced
    /// it, and it stays for one release.
    pub niri_snippet: String,
    pub va: Option<VaReport>,
}

/// One dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Check {
    /// Stable id: `gstreamer`, `gst.x264enc`, `ffmpeg.libx264`, `portal.screencast`, …
    pub id: String,
    /// An app check (`needed_for` has [`Feature::App`]): failing it is
    /// `missing_dependency` (exit 5) and S23.
    pub required: bool,
    /// What needs it; empty: it only informs.
    pub needed_for: Vec<Feature>,
    pub ok: bool,
    /// What was found (or why it failed), one line.
    pub detail: String,
    /// How to fix it; set when the check failed.
    pub hint: Option<String>,
    /// The fix for this machine when it is something to install or set
    /// up (`platform::install_hint`, with the check's own advice in its
    /// note): `hint` is its line, S23 shows its command and note. Absent
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<Hint>,
}

impl Check {
    /// It failed, and `feature` needs it.
    pub fn turns_off(&self, feature: Feature) -> bool {
        !self.ok && self.needed_for.contains(&feature)
    }
}

/// What a check is needed for (JSON `neededFor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Feature {
    /// Qrec itself: opening a project, the editor and its preview, the
    /// takes' probes.
    #[serde(rename = "app")]
    App,
    #[serde(rename = "screens")]
    Screens,
    #[serde(rename = "cameras")]
    Cameras,
    /// Mics and system audio.
    #[serde(rename = "audio")]
    Audio,
    /// Recording takes: the H.264 encoder and the files' elements.
    #[serde(rename = "recording")]
    Recording,
    #[serde(rename = "export.mp4")]
    ExportMp4,
    #[serde(rename = "export.webm")]
    ExportWebm,
    #[serde(rename = "export.av1")]
    ExportAv1,
}

impl Feature {
    pub const ALL: [Feature; 8] = [
        Feature::App,
        Feature::Screens,
        Feature::Cameras,
        Feature::Audio,
        Feature::Recording,
        Feature::ExportMp4,
        Feature::ExportWebm,
        Feature::ExportAv1,
    ];

    /// What people call it: "Screen capture is off".
    pub fn label(self) -> &'static str {
        match self {
            Feature::App => "Qrec",
            Feature::Screens => "Screen capture",
            Feature::Cameras => "Camera capture",
            Feature::Audio => "Audio capture",
            Feature::Recording => "Recording",
            Feature::ExportMp4 => "MP4 export",
            Feature::ExportWebm => "WebM export",
            Feature::ExportAv1 => "AV1 export",
        }
    }
}

/// A feature this machine can't do, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeatureOff {
    pub feature: Feature,
    /// The failed checks it needs, in the doctor's order.
    pub checks: Vec<String>,
    /// One line for people: "Screen capture is off: no ScreenCast portal: …".
    pub message: String,
}

/// The VA-API situation (A§2.1, A§8.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VaReport {
    /// Every `/dev/dri/renderD*` with its PCI vendor (from sysfs; never opened).
    pub nodes: Vec<RenderNode>,
    /// The effective device: project, then config, then the first Intel or
    /// AMD node (never NVIDIA automatically).
    pub device: Option<String>,
    /// `project`, `config` or `auto`.
    pub origin: Option<String>,
    pub vendor: Option<u16>,
    /// The device's PCI device id (an Intel chip's generation).
    #[serde(default)]
    pub device_id: Option<u16>,
    /// Why the device can't be opened, when it is an Intel or AMD node
    /// sysfs lists: its device file missing (a container or sandbox that
    /// didn't get it) or this user's permissions. Then nothing asks it to
    /// encode. Absent when it opens or isn't opened (NVIDIA's never is).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_error: Option<DeviceError>,
    /// The `LIBVA_DRIVER_NAME` the device's children load
    /// (`gpu::child_libva_driver`): this process's when it fits the
    /// vendor (main() pinned it), else `radeonsi` for AMD, `i965` for the
    /// Intel chips only it drives (older than Broadwell, Cherry View and
    /// Braswell) or where only it is installed, else `iHD`.
    pub expected_driver: Option<String>,
    /// `LIBVA_DRIVER_NAME` in this process (main() sets it at startup).
    pub libva_driver_name: Option<String>,
    /// Where libva loads its drivers from: `LIBVA_DRIVERS_PATH`'s
    /// directories, else the standard ones that exist.
    #[serde(default)]
    pub driver_dirs: Vec<String>,
    /// The expected driver's file there (`iHD_drv_video.so`); `None` when
    /// it isn't installed.
    #[serde(default)]
    pub driver_file: Option<String>,
    /// The libva drivers Qrec loads or keeps whose files are there (`iHD`,
    /// `i965`, `radeonsi`, `r600`: `gpu::VaDrivers`).
    #[serde(default)]
    pub installed_drivers: Vec<String>,
    /// The h264_vaapi export chain encoded two frames, with the expected
    /// driver or else `export_driver`; `None` = not probed.
    pub ffmpeg_h264: Option<bool>,
    /// GStreamer's VA H.264 encoders (`va*h264enc`, `va*h264lpenc`).
    pub gst_encoders: Vec<String>,
    /// The one of them recordings use on `device`: its `va*h264enc`, else
    /// its `va*h264lpenc` (the probe child of A§8.10, run with the
    /// h264_vaapi check); `None` when not probed or none is bound to it.
    pub gst_factory: Option<String>,
    /// On an Intel device where the h264_vaapi check failed with the
    /// expected driver: Intel's other driver, with which it passed
    /// (`gpu::retry_driver`). Exports load it there (`caps::detect` keeps
    /// the pick); recordings keep the expected one.
    #[serde(default)]
    pub export_driver: Option<String>,
    /// What `main()` changed in the GPU environment at startup (A§8.10).
    pub startup: Vec<String>,
    pub note: Option<String>,
}

/// Why a render node can't be opened (`VaReport::device_error`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceError {
    pub kind: DeviceErrorKind,
    /// The system's words: "No such file or directory (os error 2)".
    pub message: String,
}

/// What kept a render node from opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceErrorKind {
    /// No device file (ENOENT): sysfs lists the GPU, but a container or a
    /// sandbox didn't get its node.
    Missing,
    /// EACCES or EPERM: this user may not open it.
    Denied,
    /// Anything else.
    Failed,
}

impl DeviceError {
    /// The error of opening a node.
    pub fn of(error: &io::Error) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::NotFound => DeviceErrorKind::Missing,
            io::ErrorKind::PermissionDenied => DeviceErrorKind::Denied,
            _ => DeviceErrorKind::Failed,
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }

    /// What to do about it on `device`: give the container the node, or
    /// let the user open it.
    fn hint(&self, device: &str) -> String {
        match self.kind {
            DeviceErrorKind::Missing => format!(
                "sysfs lists this GPU but there is no {device}: give the container or sandbox \
                 Qrec runs in that device (docker run --device {device})"
            ),
            DeviceErrorKind::Denied => format!(
                "let your user open {device}: join the group that owns it (ls -l {device} shows \
                 it, usually render), then log in again"
            ),
            DeviceErrorKind::Failed => format!("check {device} (ls -l {device})"),
        }
    }
}

/// A DRM render node (`gpu::RenderNode` with its vendor's name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderNode {
    pub path: String,
    /// PCI vendor id from sysfs; `None` when unreadable.
    pub vendor: Option<u16>,
    pub vendor_name: Option<String>,
    /// PCI device id from sysfs.
    #[serde(default)]
    pub device_id: Option<u16>,
    /// Kernel driver (`i915`, `xe`, `amdgpu`, `nvidia`).
    pub driver: Option<String>,
}

impl From<&gpu::RenderNode> for RenderNode {
    fn from(node: &gpu::RenderNode) -> Self {
        Self {
            path: node.path.clone(),
            vendor: node.vendor,
            vendor_name: node.vendor.map(vendor_name),
            device_id: node.device_id,
            driver: node.driver.clone(),
        }
    }
}

/// The ScreenCast portal as seen over D-Bus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PortalInfo {
    pub screencast_version: u32,
    /// Bit 1 monitor, 2 window, 4 virtual.
    pub source_types: Option<u32>,
    /// `org.freedesktop.host.portal.Registry` (xdg-desktop-portal ≥ 1.19).
    pub registry_version: Option<u32>,
}

/// How to run the checks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorOptions {
    /// `QREC_CAPTURE_BACKEND=fake`: test sources instead of the screen
    /// backend, cameras and PipeWire (their checks only inform), x264enc
    /// (else openh264enc) for recordings and libx264 (else libopenh264) for
    /// MP4 exports.
    pub fake_backend: bool,
    /// `recording.vaapiDevice` of the project in scope, if any.
    pub project_vaapi_device: Option<String>,
    /// `encoder.vaapi_device` from the user config, if any.
    pub config_vaapi_device: Option<String>,
    /// Run the VA-API child processes on an Intel or AMD node: the
    /// h264_vaapi export check and the probe for the VA encoders bound to
    /// it (10 s each).
    pub probe_vaapi: bool,
}

impl DoctorOptions {
    /// The backend from the environment; probe VA-API; no configured device.
    pub fn from_env() -> Self {
        Self {
            fake_backend: sources::fake_backend(),
            probe_vaapi: true,
            ..Self::default()
        }
    }
}

/// Everything the doctor asks the machine.
pub trait Probe {
    /// GStreamer's version once initialized (under the 30 s watchdog).
    fn gstreamer(&self) -> Result<String, String>;
    /// Whether each element factory exists (GStreamer is initialized).
    fn gst_element(&self, name: &str) -> bool;
    /// The VA H.264 encoder factories (`va*h264enc`, `va*h264lpenc`).
    fn gst_va_encoders(&self) -> Vec<String>;
    /// The software H.264 decoders GStreamer autoplugs: klass
    /// Decoder/Video without Hardware, sink caps `video/x-h264`, rank
    /// marginal or more (`avdec_h264`, `openh264dec`).
    fn gst_h264_decoders(&self) -> Vec<String>;
    /// GStreamer Editing Services initializes.
    fn ges(&self) -> Result<(), String>;
    fn which(&self, program: &str) -> Option<PathBuf>;
    /// The first line of `<program> -version`.
    fn version(&self, program: &str) -> Result<String, String>;
    /// `ffmpeg -encoders` names.
    fn ffmpeg_encoders(&self) -> Result<BTreeSet<String>, String>;
    /// `ffmpeg -decoders`: each decoder's name and its codec.
    fn ffmpeg_decoders(&self) -> Result<BTreeMap<String, String>, String>;
    /// `ffmpeg -demuxers` names (`matroska,webm` is both).
    fn ffmpeg_demuxers(&self) -> Result<BTreeSet<String>, String>;
    /// The session's screen backend (`ScreenBackend::from_env`): which
    /// screen checks run.
    fn screen_backend(&self) -> ScreenBackend;
    /// Whether a Unix socket is at `path` (the file system only: nothing
    /// connects to it); `Err` says what is there instead, if anything.
    fn unix_socket(&self, path: &Path) -> Result<(), String>;
    /// The ScreenCast portal (asked only when the portal records screens).
    fn portal(&self) -> Result<PortalInfo, String>;
    /// The X server at `display` (`sources::x11_server`: connected within
    /// 2 s); `Err` is discovery's warning (`X11 (DISPLAY=:1): …`).
    fn x11(&self, display: &str) -> Result<X11Server, String>;
    /// The first line of `pactl --version` (`pactl 17.0`).
    fn pactl_version(&self) -> Result<String, String>;
    /// `pactl --format=json info` → `server_name`.
    fn pulse_server(&self) -> Result<String, String>;
    /// `/sys/class/drm/renderD*` (sysfs only; nodes are never opened).
    fn render_nodes(&self) -> Vec<gpu::RenderNode>;
    /// Libva's driver files (a stat each; nothing is opened).
    fn va_drivers(&self) -> gpu::VaDrivers;
    /// Opens the render node `path` for reading and writing and closes it
    /// again. Asked only for an Intel or AMD node, never NVIDIA's (A§8.10).
    fn open_node(&self, path: &str) -> io::Result<()>;
    fn env(&self, key: &str) -> Option<String>;
    /// The h264_vaapi export chain on `device` (two frames to the null muxer).
    fn vaapi_h264(&self, device: &str, driver: Option<&str>) -> Result<(), String>;
    /// The VA encoder recordings use on `device`, from the probe child
    /// (A§8.10): the `va*h264enc` whose `device-path` it is, else such a
    /// `va*h264lpenc`; `None` when no factory is bound to it.
    fn gst_va_factory(&self, device: &str) -> Result<Option<String>, String>;
    /// What `gpu::prepare_gpu_env` changed and noted at startup.
    fn gpu_startup(&self) -> Vec<String>;
    /// Where Qrec runs.
    fn platform(&self) -> HostPlatform;
    /// The user's `config.toml` (`paths::UserDirs`), which hints name.
    fn config_file(&self) -> Option<PathBuf>;
}

/// Runs every check against this machine.
pub fn run(opts: &DoctorOptions) -> DoctorReport {
    run_with(&SystemProbe, opts)
}

/// Runs every check against `probe`.
pub fn run_with(probe: &dyn Probe, opts: &DoctorOptions) -> DoctorReport {
    let platform = probe.platform();
    let screens = probe.screen_backend();
    let fix = Fix(&platform);
    let mut checks = Vec::new();
    // Asked only once GStreamer works: a stuck plugin scan isn't waited for twice.
    let gst_va_encoders = gstreamer_checks(probe, opts, &screens, fix, &mut checks);
    let va = va_report(probe, opts, gst_va_encoders.clone().unwrap_or_default());
    if gst_va_encoders.is_some() {
        encoder_checks(&va, opts, fix, &mut checks);
        checks.push(match probe.ges() {
            Ok(()) => ok("ges", APP, "GStreamer Editing Services"),
            Err(e) => fail_fix(
                "ges",
                APP,
                format!("GStreamer Editing Services: {e}"),
                fix.need(Need::Ges),
            ),
        });
    }
    ffmpeg_checks(probe, &va, opts, fix, &mut checks);
    // The fake backend captures test sources: it needs none of these.
    let real = |features: &'static [Feature]| {
        if opts.fake_backend { NOTHING } else { features }
    };
    screen_checks(probe, &screens, real(SCREENS), fix, &mut checks);
    audio_checks(probe, real(AUDIO), fix, &mut checks);
    let cpu = on_the_cpu(&checks);
    checks.push(va_check(&va, probe.config_file().as_deref(), fix, cpu));
    // From an AppImage the shortcuts run its path, not `qrec`.
    let program = platform.program();
    DoctorReport {
        features_off: features_off(&checks),
        checks,
        shortcuts: global_snippet(&platform.desktop, program),
        session: session_line(&platform, &screens),
        screen_backend: sources::backend_kind(&screens),
        niri_snippet: global_snippet(&Desktop::Niri, program).text,
        platform,
        va: Some(va),
    }
}

/// [`DoctorReport::session`]: the session ("no display" without one), its
/// desktop when known, and the screen backend as [`ScreenBackend`] says
/// it (`portal`, `x11 (DISPLAY=:0)` or `none`).
fn session_line(platform: &HostPlatform, screens: &ScreenBackend) -> String {
    let session = match platform.session {
        SessionType::Wayland => "Wayland",
        SessionType::X11 => "X11",
        SessionType::Unknown => "no display",
    };
    let desktop = match &platform.desktop {
        Desktop::Unknown => String::new(),
        desktop => format!(" ({})", desktop.as_str()),
    };
    format!("{session}{desktop} · screens: {screens}")
}

/// The fixes where Qrec runs (`run_with`'s platform).
#[derive(Clone, Copy)]
struct Fix<'a>(&'a HostPlatform);

impl Fix<'_> {
    /// How to get `need` here (`platform::install_hint`).
    fn need(self, need: Need) -> Hint {
        install_hint(need, self.0)
    }

    /// Qrec runs from an AppImage, which bundles everything the checks
    /// look for but the host's parts.
    fn bundled(self) -> bool {
        self.0.app_image.is_some()
    }

    /// `advice` for a part that is there and doesn't work ("reinstall
    /// ffmpeg"), unless an AppImage bundles it: then its hint for `need`.
    fn repair(self, need: Need, advice: impl Into<String>) -> String {
        if self.bundled() {
            self.need(need).line()
        } else {
            advice.into()
        }
    }
}

const NOTHING: &[Feature] = &[];
const APP: &[Feature] = &[Feature::App];
const SCREENS: &[Feature] = &[Feature::Screens];
const CAMERAS: &[Feature] = &[Feature::Cameras];
const AUDIO: &[Feature] = &[Feature::Audio];
const RECORDING: &[Feature] = &[Feature::Recording];
const MP4: &[Feature] = &[Feature::ExportMp4];
const WEBM: &[Feature] = &[Feature::ExportWebm];
const AV1: &[Feature] = &[Feature::ExportAv1];
/// libopus: WebM's audio, AV1's too (AV1 exports are WebM).
const OPUS: &[Feature] = &[Feature::ExportWebm, Feature::ExportAv1];
const EXPORTS: &[Feature] = &[Feature::ExportMp4, Feature::ExportWebm, Feature::ExportAv1];

fn ok(id: &str, needed_for: &[Feature], detail: impl Into<String>) -> Check {
    Check {
        id: id.to_owned(),
        required: needed_for.contains(&Feature::App),
        needed_for: needed_for.to_vec(),
        ok: true,
        detail: detail.into(),
        hint: None,
        fix: None,
    }
}

/// A failed check whose hint is advice, not something to install.
fn fail(
    id: &str,
    needed_for: &[Feature],
    detail: impl Into<String>,
    hint: impl Into<String>,
) -> Check {
    Check {
        ok: false,
        hint: Some(hint.into()),
        ..ok(id, needed_for, detail)
    }
}

/// A failed check that `fix` fixes (its hint is the fix's line).
fn fail_fix(id: &str, needed_for: &[Feature], detail: impl Into<String>, fix: Hint) -> Check {
    Check {
        ok: false,
        hint: Some(fix.line()),
        fix: Some(fix),
        ..ok(id, needed_for, detail)
    }
}

/// Each feature but the app that a failed check turns off.
fn features_off(checks: &[Check]) -> Vec<FeatureOff> {
    Feature::ALL
        .into_iter()
        .filter(|f| *f != Feature::App)
        .filter_map(|feature| {
            let failed: Vec<&Check> = checks.iter().filter(|c| c.turns_off(feature)).collect();
            if failed.is_empty() {
                return None;
            }
            let details: Vec<&str> = failed.iter().map(|c| c.detail.as_str()).collect();
            Some(FeatureOff {
                feature,
                checks: failed.iter().map(|c| c.id.clone()).collect(),
                message: format!("{} is off: {}", feature.label(), details.join("; ")),
            })
        })
        .collect()
}

/// `major.minor` of the first word that starts with a digit
/// (`GStreamer 1.26.11` → (1, 26), `pactl 16.1` → (16, 1)).
pub fn parse_version(text: &str) -> Option<(u32, u32)> {
    let word = text
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut parts = word.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// A GStreamer element: what it is for, where it comes from and who needs it.
struct Element {
    name: &'static str,
    role: &'static str,
    /// What provides it.
    need: Need,
    needed_for: &'static [Feature],
    /// Needed only with the real capture backend (the fake one needs
    /// nothing of it).
    real_only: bool,
}

/// What records the portal's screens (its PipeWire streams).
const PIPEWIRESRC: Element = Element {
    name: "pipewiresrc",
    role: "screen capture",
    need: Need::GstPipewire,
    needed_for: SCREENS,
    real_only: true,
};

/// What records an X server's screens (parts of its root window).
const XIMAGESRC: Element = Element {
    name: "ximagesrc",
    role: "screen capture",
    need: Need::GstPluginsGood,
    needed_for: SCREENS,
    real_only: true,
};

/// The element that records screens under `screens`; none without a
/// display.
fn screen_element(screens: &ScreenBackend) -> Option<&'static Element> {
    match screens {
        ScreenBackend::Portal => Some(&PIPEWIRESRC),
        ScreenBackend::X11 { .. } => Some(&XIMAGESRC),
        ScreenBackend::None => None,
    }
}

/// The rest, after the screens' element ([`screen_element`]).
const ELEMENTS: &[Element] = &[
    Element {
        name: "v4l2src",
        role: "cameras",
        need: Need::GstPluginsGood,
        needed_for: CAMERAS,
        real_only: true,
    },
    Element {
        name: "pulsesrc",
        role: "audio capture",
        need: Need::GstPluginsGood,
        needed_for: AUDIO,
        real_only: true,
    },
    Element {
        name: "compositor",
        role: "live preview",
        need: Need::GstPluginsBase,
        needed_for: APP,
        real_only: false,
    },
    Element {
        name: "videorate",
        role: "constant frame rate",
        need: Need::GstPluginsBase,
        needed_for: APP,
        real_only: false,
    },
    // Not needed when a VA encoder records instead (`encoder_checks`).
    Element {
        name: "x264enc",
        role: "H.264 recording",
        need: Need::GstX264,
        needed_for: RECORDING,
        real_only: false,
    },
    // Needed only when neither a VA encoder nor x264enc records.
    Element {
        name: "openh264enc",
        role: "H.264 recording",
        need: Need::GstOpenH264,
        needed_for: RECORDING,
        real_only: false,
    },
    Element {
        name: "h264parse",
        role: "H.264 recording",
        need: Need::GstPluginsBad,
        needed_for: RECORDING,
        real_only: false,
    },
    Element {
        name: "matroskamux",
        role: "MKV files",
        need: Need::GstPluginsGood,
        needed_for: RECORDING,
        real_only: false,
    },
    Element {
        name: "flacenc",
        role: "lossless audio",
        need: Need::GstPluginsGood,
        needed_for: RECORDING,
        real_only: false,
    },
    Element {
        name: "matroskademux",
        role: "reading the takes' MKV files",
        need: Need::GstPluginsGood,
        needed_for: APP,
        real_only: false,
    },
    Element {
        name: "flacdec",
        role: "the takes' FLAC audio",
        need: Need::GstPluginsGood,
        needed_for: APP,
        real_only: false,
    },
    Element {
        name: "gtk4paintablesink",
        role: "the GUI preview",
        need: Need::GstGtk4,
        needed_for: APP,
        real_only: false,
    },
];

/// Only with `QREC_CAPTURE_BACKEND=fake`.
const FAKE_ELEMENTS: &[Element] = &[
    Element {
        name: "videotestsrc",
        role: "fake screens and cameras",
        need: Need::GstPluginsBase,
        needed_for: &[Feature::Screens, Feature::Cameras],
        real_only: false,
    },
    Element {
        name: "audiotestsrc",
        role: "fake audio",
        need: Need::GstPluginsBase,
        needed_for: AUDIO,
        real_only: false,
    },
];

/// The GStreamer checks but the encoders' and GES's: the version, the
/// elements (the one that records `screens` first) and the H.264 decoder.
/// Returns the VA H.264 encoders when GStreamer works, `None` when it
/// doesn't.
fn gstreamer_checks(
    probe: &dyn Probe,
    opts: &DoctorOptions,
    screens: &ScreenBackend,
    fix: Fix<'_>,
    checks: &mut Vec<Check>,
) -> Option<Vec<String>> {
    let version = match probe.gstreamer() {
        Ok(version) => version,
        Err(e) => {
            checks.push(fail_fix(
                "gstreamer",
                APP,
                e,
                fix.need(Need::Gstreamer).also(
                    "if it hangs, a GPU driver blocks the plugin scan: check the VA-API section",
                ),
            ));
            return None;
        }
    };
    let (min_major, min_minor) = GSTREAMER_MIN;
    checks.push(match parse_version(&version) {
        Some(found) if found < GSTREAMER_MIN => fail(
            "gstreamer",
            APP,
            format!("{version} is too old: Qrec needs GStreamer {min_major}.{min_minor} or newer"),
            fix.repair(
                Need::Gstreamer,
                format!("update GStreamer to {min_major}.{min_minor} or newer"),
            ),
        ),
        _ => ok("gstreamer", APP, version),
    });
    let extra: &[Element] = if opts.fake_backend {
        FAKE_ELEMENTS
    } else {
        &[]
    };
    for element in screen_element(screens)
        .into_iter()
        .chain(ELEMENTS)
        .chain(extra)
    {
        let needed_for = if element.real_only && opts.fake_backend {
            NOTHING
        } else {
            element.needed_for
        };
        let id = format!("gst.{}", element.name);
        checks.push(if probe.gst_element(element.name) {
            ok(
                &id,
                needed_for,
                format!("{} ({})", element.name, element.role),
            )
        } else {
            fail_fix(
                &id,
                needed_for,
                format!(
                    "{} ({}) isn't in the GStreamer registry",
                    element.name, element.role
                ),
                fix.need(element.need),
            )
        });
    }
    let decoders = probe.gst_h264_decoders();
    checks.push(if decoders.is_empty() {
        fail_fix(
            "gst.h264dec",
            APP,
            "no software H.264 decoder: the editor can't play the takes",
            fix.need(Need::GstH264Decoder),
        )
    } else if decoders.iter().all(|d| d == "openh264dec") {
        ok(
            "gst.h264dec",
            APP,
            "openh264dec (the editor's preview of the takes; OpenH264 may not decode \
             High-profile takes, gst-libav's avdec_h264 does)",
        )
    } else {
        ok(
            "gst.h264dec",
            APP,
            format!(
                "{} (the editor's preview of the takes)",
                decoders.join(", ")
            ),
        )
    });
    Some(probe.gst_va_encoders())
}

/// Whether the doctor asks the probe child for the VA encoder bound to
/// the effective node: the real backend, `probe_vaapi`, an Intel or AMD
/// node (never NVIDIA, as for the h264_vaapi check) and a VA encoder in
/// GStreamer's registry at all.
fn probes_va_factory(opts: &DoctorOptions, vendor: Option<u16>, encoders: &[String]) -> bool {
    opts.probe_vaapi
        && !opts.fake_backend
        && matches!(vendor, Some(PCI_INTEL | PCI_AMD))
        && !encoders.is_empty()
}

/// The VA encoder recordings use instead of x264enc (A§9.5
/// `choose_encoder`), if any: the one the probe child bound to the
/// effective node. Unprobed (the GUI's doctor, whose host probes), the
/// first full one GStreamer lists, else its first low-power one, when that
/// node is Intel or AMD and opens. Never with the fake backend, which
/// records with x264enc (else openh264enc).
fn va_recorder(va: &VaReport, opts: &DoctorOptions) -> Option<String> {
    if opts.fake_backend || va.device_error.is_some() {
        None
    } else if probes_va_factory(opts, va.vendor, &va.gst_encoders) {
        va.gst_factory.clone()
    } else if matches!(va.vendor, Some(PCI_INTEL | PCI_AMD)) {
        let first = |kind| {
            va.gst_encoders
                .iter()
                .find(|name| gpu::va_h264_kind(name) == Some(kind))
        };
        first(gpu::VaH264Kind::Full)
            .or_else(|| first(gpu::VaH264Kind::LowPower))
            .cloned()
    } else {
        None
    }
}

/// The VA-API GPU of the effective node, by its PCI vendor and device id:
/// Intel (`IntelLegacy` for the chips only i965 drives: older than
/// Broadwell, Cherry View and Braswell) or AMD. `None` for anything else,
/// where VA-API doesn't record: NVIDIA (no VA-API encoder, A§8.10), a
/// virtual machine's GPU, an unknown vendor, a configured path that isn't
/// a node, or no node at all.
fn va_gpu(va: &VaReport) -> Option<VaGpu> {
    match va.vendor? {
        PCI_INTEL if va.device_id.is_some_and(gpu::intel_i965_only) => Some(VaGpu::IntelLegacy),
        PCI_INTEL => Some(VaGpu::Intel),
        PCI_AMD => Some(VaGpu::Amd),
        _ => None,
    }
}

/// What gives GStreamer a VA encoder when it lists none on an Intel or AMD
/// node (`gpu`) whose driver isn't i965 ([`i965_records_nothing`]):
/// usually the node's VA driver, since GStreamer's VA plugin registers no
/// encoder for a node whose driver is missing or doesn't encode H.264
/// (Fedora's own Mesa), so the hint is the driver's, with the plugin's
/// after it. Once ffmpeg's h264_vaapi check encoded on that node with that
/// driver, the driver works and the plugin is what's missing (Arch
/// packages it on its own, gst-plugin-va). An AppImage bundles both: the
/// driver's hint says so, alone.
fn no_va_encoder_fix(gpu: VaGpu, va: &VaReport, fix: Fix<'_>) -> Hint {
    match (va.ffmpeg_h264, &va.export_driver) {
        (Some(true), None) => fix.need(Need::GstVa),
        _ if fix.bundled() => fix.need(Need::VaDriver(gpu)),
        _ => fix.need(Need::VaDriver(gpu)).also(&format!(
            "if GStreamer still has no VA encoder: {}",
            fix.need(Need::GstVa).line()
        )),
    }
}

/// `gst.vah264enc` where the node's driver is i965 and no VA encoder is
/// bound to the node: GStreamer's VA plugin registers none with i965 (it
/// is blocklisted for encoding unless `GST_VA_ALL_DRIVERS` is set), so no
/// install of the driver or the plugin helps. On the chips only i965
/// drives (`IntelLegacy`) recordings stay on the CPU, which the row only
/// says. On newer ones iHD records: `LIBVA_DRIVER_NAME=i965` came from the
/// user where iHD is installed (Qrec loads iHD there), else iHD is missing.
fn i965_records_nothing(gpu: VaGpu, va: &VaReport, fix: Fix<'_>, on_the_cpu: &str) -> Check {
    let id = "gst.vah264enc";
    if gpu == VaGpu::IntelLegacy {
        return ok(
            id,
            NOTHING,
            format!(
                "GStreamer's VA plugin doesn't encode with i965, the only VA-API driver of this \
                 Intel GPU: {on_the_cpu}"
            ),
        );
    }
    let detail = format!("GStreamer's VA plugin doesn't encode with i965: {on_the_cpu}");
    if va.installed_drivers.iter().any(|d| d == gpu::DRIVER_IHD) {
        return fail(
            id,
            NOTHING,
            detail,
            "unset LIBVA_DRIVER_NAME=i965: Qrec then loads iHD, which GStreamer records with",
        );
    }
    let hint = fix.need(Need::VaDriver(VaGpu::Intel));
    let hint = if va.gst_encoders.is_empty() && !fix.bundled() {
        hint.also(&format!(
            "if GStreamer still has no VA encoder: {}",
            fix.need(Need::GstVa).line()
        ))
    } else {
        hint
    };
    fail_fix(id, NOTHING, detail, hint)
}

/// Why no node was chosen for VA-API (the `vaapi` row): no render node at
/// all (a virtual machine without 3D acceleration, no GPU driver), or none
/// of Intel's or AMD's, each one named: NVIDIA's (no VA-API encoder), a
/// virtual machine's GPU, another vendor's, one that isn't on PCI.
fn no_va_node(nodes: &[RenderNode]) -> String {
    if nodes.is_empty() {
        return "no GPU render node (a virtual machine without 3D acceleration, or no GPU \
                driver)"
            .to_owned();
    }
    let named: Vec<String> = nodes
        .iter()
        .map(|node| {
            let what = match node.vendor {
                Some(PCI_NVIDIA) => "NVIDIA, which has no VA-API encoder".to_owned(),
                Some(vendor) if gpu::is_virtual_gpu(vendor) => {
                    format!("{}, a virtual machine's GPU", vendor_name(vendor))
                }
                Some(vendor) => format!("PCI vendor {vendor:#06x}"),
                None => node.driver.as_deref().map_or_else(
                    || "an unknown vendor".to_owned(),
                    |driver| format!("{driver}, not on PCI"),
                ),
            };
            format!("{}: {what}", node.path)
        })
        .collect();
    format!("no Intel or AMD GPU ({})", named.join("; "))
}

/// Why VA-API doesn't record where the effective node isn't Intel's or
/// AMD's ([`va_gpu`] is `None`), in a few words (`gst.vah264enc`).
fn not_a_va_gpu(va: &VaReport) -> String {
    match (&va.device, va.vendor) {
        (None, _) => "no Intel or AMD GPU".to_owned(),
        (Some(device), Some(PCI_NVIDIA)) => {
            format!("{device} is NVIDIA's, which has no VA-API encoder")
        }
        (Some(device), Some(_)) => format!("{device} isn't an Intel or AMD GPU"),
        (Some(device), None) => format!("{device} isn't a render node of this machine"),
    }
}

/// The recording encoder (A§9.5 `choose_encoder`): the VA encoder bound
/// to the node, else x264enc, else openh264enc. Only the first one there
/// records, so only it is needed: the others' checks (already in
/// `checks`) inform, a missing one saying what records instead. With none,
/// both software encoders are needed, since either one would record.
/// `gst.vah264enc` says which VA encoder records, or what gives GStreamer
/// one on the node's GPU ([`va_gpu`]: Intel's drivers by generation,
/// AMD's), never i965, which its VA plugin doesn't encode with
/// ([`i965_records_nothing`]); without an Intel or AMD node it only says
/// what records.
fn encoder_checks(va: &VaReport, opts: &DoctorOptions, fix: Fix<'_>, checks: &mut Vec<Check>) {
    let has = |id: &str| checks.iter().any(|c| c.id == id && c.ok);
    let software = if has("gst.x264enc") {
        Some("x264enc")
    } else if has("gst.openh264enc") {
        Some("openh264enc")
    } else {
        None
    };
    let recorder = va_recorder(va, opts).or_else(|| software.map(str::to_owned));
    // What records when no VA encoder does.
    let on_the_cpu = match software {
        Some("x264enc") => "recordings use x264 on the CPU",
        Some(_) => "recordings use OpenH264 on the CPU",
        None => "no software H.264 encoder records either",
    };
    for (name, other) in [("x264enc", "openh264enc"), ("openh264enc", "x264enc")] {
        let id = format!("gst.{name}");
        let Some(check) = checks.iter_mut().find(|c| c.id == id) else {
            continue;
        };
        match &recorder {
            Some(recorder) if recorder == name => {}
            Some(recorder) => {
                check.needed_for.clear();
                check.required = false;
                if !check.ok {
                    let _ = write!(check.detail, ": recordings use {recorder}");
                }
            }
            None => {
                let _ = write!(
                    check.detail,
                    " (and no VA-API encoder or {other} records instead)"
                );
            }
        }
    }
    let id = "gst.vah264enc";
    let device = va.device.as_deref().unwrap_or("the render node");
    let Some(gpu) = va_gpu(va) else {
        // Nothing to install: VA-API doesn't record here.
        checks.push(ok(
            id,
            NOTHING,
            format!("{}: {on_the_cpu}", not_a_va_gpu(va)),
        ));
        return;
    };
    // No driver can help a node that doesn't open.
    if let Some(error) = &va.device_error {
        checks.push(fail(
            id,
            NOTHING,
            format!("{device} can't be opened: {}: {on_the_cpu}", error.message),
            error.hint(device),
        ));
        return;
    }
    let probed = probes_va_factory(opts, va.vendor, &va.gst_encoders);
    let none_bound = va.gst_encoders.is_empty() || (probed && va.gst_factory.is_none());
    checks.push(
        if none_bound && va.expected_driver.as_deref() == Some(gpu::DRIVER_I965) {
            i965_records_nothing(gpu, va, fix, on_the_cpu)
        } else if va.gst_encoders.is_empty() {
            fail_fix(
                id,
                NOTHING,
                format!("no VA-API H.264 encoder: {on_the_cpu}"),
                no_va_encoder_fix(gpu, va, fix),
            )
        } else if probed {
            match &va.gst_factory {
                Some(factory) => ok(id, NOTHING, format!("{factory} records on {device}")),
                None => fail_fix(
                    id,
                    NOTHING,
                    format!(
                        "none of {} encodes on {device}: {on_the_cpu}",
                        va.gst_encoders.join(", ")
                    ),
                    fix.need(Need::VaDriver(gpu)),
                ),
            }
        } else {
            ok(id, NOTHING, va.gst_encoders.join(", "))
        },
    );
}

/// Encoder name, what needs it, what for.
const ENCODERS: &[(&str, &[Feature], &str)] = &[
    // MP4's video: only the one that encodes it is needed, h264_vaapi
    // first (`mp4_encoder_checks`).
    ("libx264", MP4, "MP4 exports"),
    ("libopenh264", MP4, "MP4 exports"),
    ("aac", MP4, "MP4 audio"),
    ("pcm_s16le", EXPORTS, "export segments"),
    ("libvpx-vp9", WEBM, "WebM exports"),
    ("libopus", OPUS, "WebM audio"),
    ("libsvtav1", AV1, "AV1 WebM exports"),
];

/// Codec, what for: ffmpeg decodes these to probe, analyze and export the
/// takes.
const DECODERS: &[(&str, &str)] = &[("h264", "the takes' video"), ("flac", "the takes' audio")];

fn ffmpeg_checks(
    probe: &dyn Probe,
    va: &VaReport,
    opts: &DoctorOptions,
    fix: Fix<'_>,
    checks: &mut Vec<Check>,
) {
    let mut ffmpeg_ok = false;
    for program in ["ffmpeg", "ffprobe"] {
        let check = match probe.which(program) {
            None => fail_fix(
                program,
                APP,
                format!("{program} wasn't found on PATH"),
                fix.need(Need::Ffmpeg),
            ),
            Some(path) => match probe.version(program) {
                Ok(version) => {
                    ffmpeg_ok |= program == "ffmpeg";
                    ok(program, APP, format!("{version} ({})", path.display()))
                }
                Err(e) => fail(
                    program,
                    APP,
                    format!("{} doesn't run: {e}", path.display()),
                    fix.repair(Need::Ffmpeg, "reinstall ffmpeg"),
                ),
            },
        };
        checks.push(check);
    }
    if !ffmpeg_ok {
        return;
    }
    checks.push(match probe.ffmpeg_decoders() {
        Ok(decoders) => decoders_check(&decoders, fix),
        Err(e) => fail(
            "ffmpeg.decoders",
            APP,
            format!("can't list ffmpeg's decoders: {e}"),
            fix.repair(Need::Ffmpeg, "reinstall ffmpeg"),
        ),
    });
    checks.push(match probe.ffmpeg_demuxers() {
        Ok(demuxers) if demuxers.contains("matroska") => {
            ok("ffmpeg.demuxers", APP, "matroska (the takes' MKV files)")
        }
        Ok(_) => fail_fix(
            "ffmpeg.demuxers",
            APP,
            "ffmpeg can't read matroska (the takes' MKV files)",
            fix.need(Need::FullFfmpeg),
        ),
        Err(e) => fail(
            "ffmpeg.demuxers",
            APP,
            format!("can't list ffmpeg's demuxers: {e}"),
            fix.repair(Need::Ffmpeg, "reinstall ffmpeg"),
        ),
    });
    match probe.ffmpeg_encoders() {
        Ok(encoders) => {
            for (name, needed_for, role) in ENCODERS {
                let id = format!("ffmpeg.{name}");
                checks.push(if encoders.contains(*name) {
                    ok(&id, needed_for, format!("{name} ({role})"))
                } else {
                    fail_fix(
                        &id,
                        needed_for,
                        format!("ffmpeg has no {name} encoder ({role})"),
                        fix.need(Need::FullFfmpeg),
                    )
                });
            }
            mp4_encoder_checks(va_exporter(va, opts, &encoders), checks);
        }
        Err(e) => checks.push(fail(
            "ffmpeg.encoders",
            EXPORTS,
            format!("can't list ffmpeg's encoders: {e}"),
            fix.repair(Need::Ffmpeg, "reinstall ffmpeg"),
        )),
    }
}

/// The render node MP4 exports encode on with h264_vaapi instead of the
/// software encoders (A§9.3 `export_vaapi`), if any, as [`va_recorder`]
/// says for recordings: the effective node once its h264_vaapi check
/// passed. Unprobed (the GUI's doctor, whose host runs the check), that
/// node when it is Intel or AMD, opens, and ffmpeg has h264_vaapi. Never
/// with the fake backend, which exports with libx264, else libopenh264.
fn va_exporter<'a>(
    va: &'a VaReport,
    opts: &DoctorOptions,
    encoders: &BTreeSet<String>,
) -> Option<&'a str> {
    let exports = if opts.fake_backend || va.device_error.is_some() {
        false
    } else if opts.probe_vaapi {
        va.ffmpeg_h264 == Some(true)
    } else {
        matches!(va.vendor, Some(PCI_INTEL | PCI_AMD)) && encoders.contains("h264_vaapi")
    };
    va.device.as_deref().filter(|_| exports)
}

/// MP4's video encoder (A§9.3 `plan_export`), as [`encoder_checks`] does
/// the recordings': h264_vaapi on `exporter` ([`va_exporter`]), else
/// libx264, else libopenh264. Only that one is needed (with aac and
/// pcm_s16le): the other ffmpeg encoder's check informs, a missing one
/// saying what exports instead. A missing libopenh264 has no fix then: it
/// only stands in for libx264, and the full ffmpeg build the fix names is
/// libx264's (Debian's, Ubuntu's and Arch's ffmpeg have no libopenh264).
/// A missing libx264 keeps its fix: it is the better encoder, and the one
/// an x264 preference exports with. With none, both are needed, since
/// either one would export. An x264 preference is the export's to name
/// (the doctor reads no preference).
fn mp4_encoder_checks(exporter: Option<&str>, checks: &mut [Check]) {
    let has = |id: &str| checks.iter().any(|c| c.id == id && c.ok);
    let encoder = match exporter {
        Some(device) => Some(format!("h264_vaapi on {device}")),
        None if has("ffmpeg.libx264") => Some("libx264".to_owned()),
        None if has("ffmpeg.libopenh264") => Some("libopenh264".to_owned()),
        None => None,
    };
    for (name, other) in [("libx264", "libopenh264"), ("libopenh264", "libx264")] {
        let id = format!("ffmpeg.{name}");
        let Some(check) = checks.iter_mut().find(|c| c.id == id) else {
            continue;
        };
        match &encoder {
            Some(encoder) if encoder == name => {}
            Some(encoder) => {
                check.needed_for.clear();
                check.required = false;
                if !check.ok {
                    check.detail =
                        format!("ffmpeg has no {name} encoder: MP4 exports use {encoder}");
                    if name == "libopenh264" {
                        check.hint = None;
                        check.fix = None;
                    }
                }
            }
            None => {
                check.detail = format!(
                    "ffmpeg has no {name} encoder (MP4 exports), and no h264_vaapi or {other} \
                     encodes them instead"
                );
            }
        }
    }
}

/// What encodes H.264 on the CPU where VA-API doesn't: for recordings
/// x264enc, else openh264enc ([`encoder_checks`]), for MP4 exports
/// libx264, else libopenh264 ([`mp4_encoder_checks`]); each `None` when
/// neither passed.
#[derive(Debug, Clone, Copy)]
struct OnTheCpu {
    records: Option<&'static str>,
    exports: Option<&'static str>,
}

fn on_the_cpu(checks: &[Check]) -> OnTheCpu {
    let has = |id: &str| checks.iter().any(|c| c.id == id && c.ok);
    OnTheCpu {
        records: if has("gst.x264enc") {
            Some("x264")
        } else if has("gst.openh264enc") {
            Some("OpenH264")
        } else {
            None
        },
        exports: if has("ffmpeg.libx264") {
            Some("libx264")
        } else if has("ffmpeg.libopenh264") {
            Some("libopenh264")
        } else {
            None
        },
    }
}

impl OnTheCpu {
    /// "recordings and exports use x264 on the CPU", or what each uses.
    fn sentence(self) -> String {
        let family = |encoder: &str| {
            match encoder {
                "libx264" => "x264",
                "libopenh264" => "OpenH264",
                other => other,
            }
            .to_owned()
        };
        match (self.records, self.exports.map(family)) {
            (Some(records), Some(exports)) if records == exports => {
                format!("recordings and exports use {records} on the CPU")
            }
            (Some(records), Some(exports)) => {
                format!("recordings use {records} and exports {exports} on the CPU")
            }
            (Some(records), None) => format!("recordings use {records} on the CPU"),
            (None, Some(exports)) => format!("exports use {exports} on the CPU"),
            (None, None) => "nothing encodes H.264 on the CPU either".to_owned(),
        }
    }

    /// The software encoder MP4 exports use while h264_vaapi fails.
    fn meanwhile(self) -> String {
        match self.exports {
            Some(encoder) => format!("exports use {encoder} meanwhile"),
            None => "MP4 exports are off meanwhile".to_owned(),
        }
    }
}

/// `ffmpeg.decoders`: a software decoder for each of [`DECODERS`]'s
/// codecs: ffmpeg's own or an external library's (`libopenh264` on
/// Fedora's ffmpeg-free); hardware ones (`h264_cuvid`, `h264_qsv`) don't
/// count.
fn decoders_check(decoders: &BTreeMap<String, String>, fix: Fix<'_>) -> Check {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for (codec, role) in DECODERS {
        let decoder = decoders
            .iter()
            .filter(|(name, of)| of == codec && (name == codec || name.starts_with("lib")))
            .map(|(name, _)| name.as_str())
            // ffmpeg's own first.
            .min_by_key(|name| *name != *codec);
        match decoder {
            Some(name) if name == *codec => found.push(format!("{codec} ({role})")),
            Some(name) => found.push(format!("{name} for {codec} ({role})")),
            None => missing.push(format!("{codec} ({role})")),
        }
    }
    if missing.is_empty() {
        ok("ffmpeg.decoders", APP, found.join(", "))
    } else {
        fail_fix(
            "ffmpeg.decoders",
            APP,
            format!("ffmpeg can't decode {}", missing.join(", ")),
            fix.need(Need::FullFfmpeg),
        )
    }
}

/// The screen backend's checks besides its element's
/// ([`screen_element`]), needed for `screens` (nothing under the fake
/// backend): under the portal, the PipeWire daemon and the ScreenCast
/// portal; on X11, the X server, and never the portal (KDE's answers on
/// X11 and can't capture there); without a display, `display`, which
/// says there is none.
fn screen_checks(
    probe: &dyn Probe,
    backend: &ScreenBackend,
    screens: &[Feature],
    fix: Fix<'_>,
    checks: &mut Vec<Check>,
) {
    match backend {
        ScreenBackend::Portal => {
            checks.push(pipewire_check(probe, screens, fix));
            portal_checks(probe, screens, fix, checks);
        }
        ScreenBackend::X11 { display } => checks.push(x11_check(probe, display, screens, fix)),
        ScreenBackend::None => checks.push(fail(
            "display",
            screens,
            sources::NO_DISPLAY,
            "start Qrec in a Wayland or X11 session: it records that session's screens",
        )),
    }
}

/// `pipewire.daemon`: the PipeWire daemon's socket where its clients look
/// for it, `pipewire-0` in `$PIPEWIRE_RUNTIME_DIR`, else in
/// `$XDG_RUNTIME_DIR` (the portal streams the screens through the
/// daemon). Only the file is looked at: nothing connects to it.
fn pipewire_check(probe: &dyn Probe, screens: &[Feature], fix: Fix<'_>) -> Check {
    const ID: &str = "pipewire.daemon";
    let dir = ["PIPEWIRE_RUNTIME_DIR", "XDG_RUNTIME_DIR"]
        .into_iter()
        .find_map(|key| probe.env(key));
    let Some(dir) = dir else {
        return fail(
            ID,
            screens,
            "no PipeWire daemon to find: neither PIPEWIRE_RUNTIME_DIR nor XDG_RUNTIME_DIR is set",
            "start Qrec in your desktop session, which sets XDG_RUNTIME_DIR",
        );
    };
    let socket = Path::new(&dir).join("pipewire-0");
    match probe.unix_socket(&socket) {
        Ok(()) => ok(
            ID,
            screens,
            format!("{} (the portal's screen casts)", socket.display()),
        ),
        Err(e) => fail_fix(
            ID,
            screens,
            format!("no PipeWire daemon: {e}"),
            fix.need(Need::PipeWire),
        ),
    }
}

/// `x11.randr`: the X server at `display` (connected within 2 s), its
/// RandR version and the screens discovery lists. A server Qrec can't
/// reach, or that shows no screen, turns screen capture off with
/// discovery's warning; without RandR, or before 1.3 (Qrec reads the
/// outputs from 1.3 on, the monitors from 1.5), its whole screen is one
/// screen, which only informs.
fn x11_check(probe: &dyn Probe, display: &str, screens: &[Feature], fix: Fix<'_>) -> Check {
    const ID: &str = "x11.randr";
    let server = match probe.x11(display) {
        Ok(server) => server,
        Err(e) => {
            return fail(
                ID,
                screens,
                e,
                format!(
                    "check that DISPLAY={display} names a running X server that lets Qrec in \
                     (its cookie in XAUTHORITY, else ~/.Xauthority)"
                ),
            );
        }
    };
    let Some(first) = server.screens.first() else {
        return fail(
            ID,
            screens,
            format!("X11 (DISPLAY={display}): its screen is empty"),
            "give the X server a screen to record",
        );
    };
    let whole = format!(
        "Qrec records its whole screen ({}x{}) as one",
        first.width, first.height
    );
    match server.randr {
        Some((major, minor)) if (major, minor) >= (1, 3) => {
            let names: Vec<&str> = server.screens.iter().map(|s| s.output.as_str()).collect();
            let count = match names.len() {
                1 => "1 screen".to_owned(),
                n => format!("{n} screens"),
            };
            ok(
                ID,
                screens,
                format!("RandR {major}.{minor}: {count} ({})", names.join(", ")),
            )
        }
        Some((major, minor)) => fail(
            ID,
            NOTHING,
            format!("RandR {major}.{minor}: {whole}; it reads the outputs from RandR 1.3 on"),
            "update the X server: Qrec lists its outputs with RandR 1.3, its monitors with 1.5",
        ),
        None => fail_fix(
            ID,
            NOTHING,
            format!("the X server has no RandR: {whole}"),
            fix.need(Need::RandR),
        ),
    }
}

/// `portal.screencast`, and `portal.registry` (which only informs) once
/// the portal answered.
fn portal_checks(probe: &dyn Probe, screens: &[Feature], fix: Fix<'_>, checks: &mut Vec<Check>) {
    let portal = probe.portal();
    checks.push(match &portal {
        // The backend that answers can't: the desktop's own can.
        Ok(info) if info.source_types.is_some_and(|t| t & 1 == 0) => fail_fix(
            "portal.screencast",
            screens,
            "the ScreenCast portal can't capture monitors",
            fix.need(Need::ScreenCastPortal),
        ),
        Ok(info) if info.screencast_version >= 3 => {
            let mut detail = format!(
                "xdg-desktop-portal ScreenCast v{}{}",
                info.screencast_version,
                info.source_types
                    .map(|t| format!(" ({})", source_types(t)))
                    .unwrap_or_default()
            );
            if info.screencast_version == 3 {
                detail
                    .push_str(": the screen picker shows on every take (v4 remembers the choice)");
            }
            ok("portal.screencast", screens, detail)
        }
        Ok(info) => fail(
            "portal.screencast",
            screens,
            format!(
                "ScreenCast v{} is too old (Qrec needs v3, and v4 to remember the screen choice)",
                info.screencast_version
            ),
            "update xdg-desktop-portal and its backend",
        ),
        Err(e) => fail_fix(
            "portal.screencast",
            screens,
            format!("no ScreenCast portal: {e}"),
            fix.need(Need::ScreenCastPortal),
        ),
    });
    if let Ok(info) = &portal {
        checks.push(match info.registry_version {
            Some(v) => ok(
                "portal.registry",
                NOTHING,
                format!("host app registry v{v}"),
            ),
            None => fail(
                "portal.registry",
                NOTHING,
                "no host app registry (xdg-desktop-portal < 1.19): the screen choice \
                 may not be remembered for Qrec",
                "update xdg-desktop-portal to 1.19 or newer",
            ),
        });
    }
}

/// `pactl` (its version first: Qrec reads `--format=json`) and, once it
/// reached the sound server, `sound.server` with the server's name.
fn audio_checks(probe: &dyn Probe, audio: &[Feature], fix: Fix<'_>, checks: &mut Vec<Check>) {
    let (min_major, _) = PACTL_MIN;
    let Some(path) = probe.which("pactl") else {
        checks.push(fail_fix(
            "pactl",
            audio,
            "pactl wasn't found (it lists microphones and system audio)",
            fix.need(Need::Pactl),
        ));
        return;
    };
    let version = match probe.pactl_version() {
        Ok(version) => version,
        Err(e) => {
            checks.push(fail(
                "pactl",
                audio,
                format!("{} doesn't run: {e}", path.display()),
                fix.repair(Need::Pactl, "reinstall the PulseAudio client tools"),
            ));
            return;
        }
    };
    if parse_version(&version).is_some_and(|found| found < PACTL_MIN) {
        checks.push(fail(
            "pactl",
            audio,
            format!(
                "{version} is too old: Qrec needs pactl {min_major} or newer ({})",
                path.display()
            ),
            fix.repair(
                Need::Pactl,
                format!("update the PulseAudio client tools (pactl) to {min_major} or newer"),
            ),
        ));
        return;
    }
    match probe.pulse_server() {
        Ok(name) => {
            checks.push(ok(
                "pactl",
                audio,
                format!("{version} ({})", path.display()),
            ));
            checks.push(ok("sound.server", audio, name));
        }
        Err(e) => checks.push(fail_fix(
            "pactl",
            audio,
            format!("{} can't reach the sound server: {e}", path.display()),
            fix.need(Need::SoundServer),
        )),
    }
}

fn source_types(bits: u32) -> String {
    let names: Vec<&str> = [(1, "monitors"), (2, "windows"), (4, "virtual")]
        .iter()
        .filter(|(bit, _)| bits & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    if names.is_empty() {
        "no sources".to_owned()
    } else {
        names.join(", ")
    }
}

/// Whether a `LIBVA_DRIVER_NAME` fits the vendor (the legacy `i965` and
/// `r600` drivers count, like `gpu::prepare_gpu_env` treats them).
fn driver_fits(driver: &str, vendor: u16) -> bool {
    match vendor {
        PCI_INTEL => matches!(driver, "iHD" | "i965"),
        PCI_AMD => matches!(driver, "radeonsi" | "r600"),
        _ => false,
    }
}

/// The VA-API report; `gst_encoders` are GStreamer's VA H.264 encoders
/// (none when GStreamer doesn't work: then no probe child is asked).
fn va_report(probe: &dyn Probe, opts: &DoctorOptions, gst_encoders: Vec<String>) -> VaReport {
    let nodes = probe.render_nodes();
    // The same resolution as main()'s (A§2.1): project, config, first Intel/AMD.
    let choice = gpu::effective_va_device(
        opts.project_vaapi_device.as_deref(),
        opts.config_vaapi_device.as_deref(),
        &nodes,
    );
    let device = choice.as_ref().map(|c| c.device.clone());
    let origin = choice.as_ref().map(|c| match c.origin {
        DeviceOrigin::Project => "project",
        DeviceOrigin::Config => "config",
        DeviceOrigin::Auto => "auto",
    });
    let vendor = choice.as_ref().and_then(|c| c.vendor);
    let node = choice
        .as_ref()
        .and_then(|c| gpu::node_for(&c.device, &nodes));
    let drivers = probe.va_drivers();
    // The driver the node's children load: main()'s pin when it fits.
    let env_driver = probe.env("LIBVA_DRIVER_NAME");
    let expected_driver =
        node.and_then(|n| gpu::child_libva_driver(n, env_driver.as_deref(), &drivers));
    let driver_file = expected_driver
        .as_deref()
        .and_then(|d| drivers.file(d))
        .map(|f| f.to_string_lossy().into_owned());
    let mut notes = Vec::new();
    // The node, opened and closed again before anything asks it to encode:
    // sysfs may list a GPU whose device file a container didn't get, or that
    // this user may not open. Never NVIDIA's (A§8.10).
    let device_error = match (&device, vendor) {
        (Some(device), Some(PCI_INTEL | PCI_AMD)) if !opts.fake_backend => {
            probe.open_node(device).err().map(|e| DeviceError::of(&e))
        }
        _ => None,
    };
    let opens = device_error.is_none();
    // Intel's other driver, where the check passed with it alone: what
    // exports load (`caps::detect` runs the same checks).
    let mut export_driver = None;
    let ffmpeg_h264 = match (&device, vendor) {
        (Some(device), Some(PCI_INTEL | PCI_AMD)) if opts.probe_vaapi && opens => {
            match probe.vaapi_h264(device, expected_driver.as_deref()) {
                Ok(()) => Some(true),
                Err(e) => {
                    let retry = node
                        .zip(expected_driver.as_deref())
                        .and_then(|(node, first)| {
                            let other = gpu::retry_driver(node, first, &drivers)?;
                            Some((first, other, probe.vaapi_h264(device, Some(other))))
                        });
                    match retry {
                        Some((first, other, Ok(()))) => {
                            notes.push(format!(
                                "the h264_vaapi check failed with {first}: {e}; it passed with \
                                 {other}, which exports load"
                            ));
                            export_driver = Some(other.to_owned());
                            Some(true)
                        }
                        Some((_, other, Err(again))) => {
                            notes.push(format!(
                                "the h264_vaapi check failed: {e}; with {other} too: {again}"
                            ));
                            Some(false)
                        }
                        None => {
                            notes.push(format!("the h264_vaapi check failed: {e}"));
                            Some(false)
                        }
                    }
                }
            }
        }
        (Some(device), Some(PCI_NVIDIA)) => {
            notes.push(format!(
                "{device} isn't probed: NVIDIA has no VA-API encoder (and VA-API on it can hang)"
            ));
            None
        }
        (Some(device), None) => {
            notes.push(format!("{device} is not a render node of this machine"));
            None
        }
        _ => None,
    };
    let gst_factory = match &device {
        Some(device) if opens && probes_va_factory(opts, vendor, &gst_encoders) => {
            match probe.gst_va_factory(device) {
                Ok(factory) => factory,
                Err(e) => {
                    notes.push(format!("the VA encoder probe failed: {e}"));
                    None
                }
            }
        }
        _ => None,
    };
    VaReport {
        nodes: nodes.iter().map(RenderNode::from).collect(),
        device,
        origin: origin.map(str::to_owned),
        vendor,
        device_id: node.and_then(|n| n.device_id),
        device_error,
        expected_driver,
        libva_driver_name: env_driver,
        driver_dirs: drivers
            .dirs
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect(),
        driver_file,
        installed_drivers: drivers.found.keys().cloned().collect(),
        ffmpeg_h264,
        gst_encoders,
        gst_factory,
        export_driver,
        startup: probe.gpu_startup(),
        note: (!notes.is_empty()).then(|| notes.join("; ")),
    }
}

/// `vaapi`: the effective node, its driver and the h264_vaapi check; `cpu`
/// says what encodes without it. Information without an Intel or AMD GPU
/// (nothing to install or set); a configured node that isn't one says
/// where it is configured (the project, or `config_file`).
fn va_check(va: &VaReport, config_file: Option<&Path>, fix: Fix<'_>, cpu: OnTheCpu) -> Check {
    let Some(device) = &va.device else {
        return ok(
            "vaapi",
            NOTHING,
            format!("{}: {}", no_va_node(&va.nodes), cpu.sentence()),
        );
    };
    let mut detail = format!(
        "{device} ({}, {})",
        va.vendor.map_or_else(|| "unknown".to_owned(), vendor_name),
        va.origin.as_deref().unwrap_or("auto")
    );
    let env = va.libva_driver_name.as_deref();
    let _ = write!(detail, " · LIBVA_DRIVER_NAME={}", env.unwrap_or("(unset)"));
    match (va.ffmpeg_h264, &va.export_driver) {
        (Some(true), Some(other)) => {
            let first = va.expected_driver.as_deref().unwrap_or("the node's driver");
            let _ = write!(
                detail,
                " · ffmpeg h264_vaapi works with {other}, which exports load ({first} fails)"
            );
        }
        (Some(true), None) => detail.push_str(" · ffmpeg h264_vaapi works"),
        (Some(false), _) => detail.push_str(" · ffmpeg h264_vaapi failed"),
        (None, _) => {}
    }
    if let Some(error) = &va.device_error {
        return fail(
            "vaapi",
            NOTHING,
            format!("{detail} · can't be opened: {}", error.message),
            format!("{}; {}", error.hint(device), cpu.meanwhile()),
        );
    }
    if let (Some(expected), Some(actual), Some(vendor)) =
        (va.expected_driver.as_deref(), env, va.vendor)
        && !driver_fits(actual, vendor)
    {
        return fail(
            "vaapi",
            NOTHING,
            format!("{detail}, but {device} needs {expected}"),
            format!("unset LIBVA_DRIVER_NAME or set it to {expected} (Qrec sets it at startup)"),
        );
    }
    let Some(gpu) = va_gpu(va) else {
        return fail(
            "vaapi",
            NOTHING,
            detail,
            configured_node_hint(va, config_file),
        );
    };
    // Its driver file, when the check didn't show it works.
    let missing = va
        .expected_driver
        .as_deref()
        .filter(|_| va.driver_file.is_none() && va.ffmpeg_h264 != Some(true));
    if let Some(driver) = missing {
        let _ = write!(detail, " · {driver} isn't installed");
    }
    match va.ffmpeg_h264 {
        Some(false) => fail_fix(
            "vaapi",
            NOTHING,
            detail,
            fix.need(Need::VaDriver(gpu)).also(&cpu.meanwhile()),
        ),
        // Not probed (the GUI's doctor): the driver file is all there is.
        None if missing.is_some() => fail_fix(
            "vaapi",
            NOTHING,
            detail,
            fix.need(Need::VaDriver(gpu)).also(&cpu.meanwhile()),
        ),
        _ => ok("vaapi", NOTHING, detail),
    }
}

/// What to do about a configured VA node that isn't an Intel or AMD GPU
/// (NVIDIA's, which has no VA-API encoder, a virtual machine's, another
/// vendor's, or no render node): set the configuration that names it to
/// an Intel or AMD node, or remove it (Qrec then picks the first one).
fn configured_node_hint(va: &VaReport, config_file: Option<&Path>) -> String {
    let why = match va.vendor {
        Some(PCI_NVIDIA) => "NVIDIA has no VA-API encoder: ",
        _ => "",
    };
    let first = "(Qrec then takes the first one)";
    match va.origin.as_deref() {
        Some("project") => format!(
            "{why}set the project's recording.vaapiDevice to an Intel or AMD render node, or to \
             null {first}"
        ),
        _ => {
            let file = config_file
                .map_or_else(|| "the user config".to_owned(), |f| f.display().to_string());
            format!(
                "{why}set encoder.vaapi_device in {file} to an Intel or AMD render node, or \
                 remove it {first}"
            )
        }
    }
}

impl DoctorReport {
    /// The report as JSON (camelCase), e.g. for `qrec doctor --json`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("a doctor report serializes")
    }

    /// Every app check passed (what is off doesn't count).
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok || !c.required)
    }

    /// 0, or 5 (`missing_dependency`) when an app check failed.
    pub fn exit_code(&self) -> u8 {
        if self.ok() {
            0
        } else {
            MISSING_DEPENDENCY_EXIT
        }
    }

    /// The app checks that failed.
    pub fn missing(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| c.required && !c.ok).collect()
    }

    /// The `missing_dependency` error for a failed report (CLI `--json`).
    pub fn missing_dependency(&self) -> Option<CoreError> {
        let missing = self.missing();
        if missing.is_empty() {
            return None;
        }
        let ids: Vec<&str> = missing.iter().map(|c| c.id.as_str()).collect();
        Some(
            CoreError::new(
                ErrorCode::MissingDependency,
                format!("missing: {}", ids.join(", ")),
            )
            .with_hint("run `qrec doctor` to see how to fix each one"),
        )
    }

    /// The human report: the session line (and the AppImage's), then one
    /// line per check (`ok`,
    /// `MISSING` for an app check, `off` when features need it, else
    /// `optional`), a hint under each failure, the VA-API summary, the
    /// global shortcuts for the desktop, then a line per feature that is
    /// off and the verdict.
    pub fn to_text(&self) -> String {
        let mut out = format!("Qrec doctor\n\nSession: {}\n", self.session);
        if let Some(app_image) = &self.platform.app_image {
            let _ = writeln!(out, "AppImage: {}", app_image.describe());
        }
        out.push('\n');
        let width = self.checks.iter().map(|c| c.id.len()).max().unwrap_or(0);
        for check in &self.checks {
            let status = match (check.ok, check.required, check.needed_for.is_empty()) {
                (true, ..) => "ok",
                (false, true, _) => "MISSING",
                (false, false, false) => "off",
                (false, false, true) => "optional",
            };
            let _ = writeln!(out, "  {status:<8}  {:<width$}  {}", check.id, check.detail);
            if let (false, Some(hint)) = (check.ok, &check.hint) {
                let _ = writeln!(out, "  {:<8}  {:<width$}  → {hint}", "", "");
            }
        }
        if let Some(va) = &self.va {
            out.push_str("\nVA-API render nodes: ");
            if va.nodes.is_empty() {
                out.push_str("none");
            } else {
                let nodes: Vec<String> = va
                    .nodes
                    .iter()
                    .map(|n| {
                        let vendor = n.vendor_name.as_deref().unwrap_or("unknown vendor");
                        match n.device_id {
                            Some(id) => format!("{} ({vendor} {id:#06x})", n.path),
                            None => format!("{} ({vendor})", n.path),
                        }
                    })
                    .collect();
                out.push_str(&nodes.join(", "));
            }
            out.push('\n');
            if let Some(driver) = &va.expected_driver {
                let _ = match (&va.driver_file, va.driver_dirs.is_empty()) {
                    (Some(file), _) => writeln!(out, "  libva driver {driver}: {file}"),
                    (None, false) => writeln!(
                        out,
                        "  libva driver {driver}: not installed (libva looks in {})",
                        va.driver_dirs.join(", ")
                    ),
                    (None, true) => writeln!(
                        out,
                        "  libva driver {driver}: not installed (no libva driver directory)"
                    ),
                };
            }
            for line in va.startup.iter().chain(&va.note) {
                let _ = writeln!(out, "  {line}");
            }
        }
        let _ = write!(
            out,
            "\n{}\n\n{}\n\n",
            self.shortcuts.heading, self.shortcuts.text
        );
        for off in &self.features_off {
            let _ = writeln!(out, "{}", off.message);
        }
        let missing = self.missing();
        if !missing.is_empty() {
            let ids: Vec<&str> = missing.iter().map(|c| c.id.as_str()).collect();
            let _ = writeln!(
                out,
                "The swamp is missing something: {} required check{} failed ({}).",
                missing.len(),
                if missing.len() == 1 { "" } else { "s" },
                ids.join(", ")
            );
        } else if self.features_off.is_empty() {
            out.push_str("Everything Qrec needs is here.\n");
        } else {
            out.push_str("Everything else Qrec needs is here.\n");
        }
        out
    }
}

// --------------------------------------------------------- the machine

/// The real machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn gstreamer(&self) -> Result<String, String> {
        gst_impl::version()
    }

    fn gst_element(&self, name: &str) -> bool {
        gst_impl::element(name)
    }

    fn gst_va_encoders(&self) -> Vec<String> {
        gst_impl::va_encoders()
    }

    fn gst_h264_decoders(&self) -> Vec<String> {
        gst_impl::h264_decoders()
    }

    fn ges(&self) -> Result<(), String> {
        gst_impl::ges()
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        sources::find_program(program)
    }

    fn version(&self, program: &str) -> Result<String, String> {
        let out = sources::tool_output(&[program, "-hide_banner", "-version"], FFMPEG_TIMEOUT)?;
        // "ffmpeg version 8.1.2 Copyright (c) …" → "ffmpeg version 8.1.2".
        out.lines()
            .next()
            .map(|l| l.split(" Copyright").next().unwrap_or(l).trim().to_owned())
            .filter(|l| !l.is_empty())
            .ok_or_else(|| format!("{program} -version printed nothing"))
    }

    fn ffmpeg_encoders(&self) -> Result<BTreeSet<String>, String> {
        let out = sources::tool_output(&["ffmpeg", "-hide_banner", "-encoders"], FFMPEG_TIMEOUT)?;
        Ok(parse_encoders(&out))
    }

    fn ffmpeg_decoders(&self) -> Result<BTreeMap<String, String>, String> {
        let out = sources::tool_output(&["ffmpeg", "-hide_banner", "-decoders"], FFMPEG_TIMEOUT)?;
        Ok(parse_decoders(&out))
    }

    fn ffmpeg_demuxers(&self) -> Result<BTreeSet<String>, String> {
        let out = sources::tool_output(&["ffmpeg", "-hide_banner", "-demuxers"], FFMPEG_TIMEOUT)?;
        Ok(parse_demuxers(&out))
    }

    fn screen_backend(&self) -> ScreenBackend {
        ScreenBackend::from_env()
    }

    fn unix_socket(&self, path: &Path) -> Result<(), String> {
        use std::os::unix::fs::FileTypeExt as _;
        match std::fs::metadata(path) {
            Ok(meta) if meta.file_type().is_socket() => Ok(()),
            Ok(_) => Err(format!("{} isn't a socket", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(format!("nothing at {}", path.display()))
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn x11(&self, display: &str) -> Result<X11Server, String> {
        sources::x11_server(display)
    }

    fn portal(&self) -> Result<PortalInfo, String> {
        let version = portal_property("org.freedesktop.portal.ScreenCast", "version")?;
        let source_types =
            portal_property("org.freedesktop.portal.ScreenCast", "AvailableSourceTypes").ok();
        let registry_version =
            portal_property("org.freedesktop.host.portal.Registry", "version").ok();
        Ok(PortalInfo {
            screencast_version: version,
            source_types,
            registry_version,
        })
    }

    fn pactl_version(&self) -> Result<String, String> {
        let out = sources::tool_output(&["pactl", "--version"], sources::TOOL_TIMEOUT)?;
        out.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "pactl --version printed nothing".to_owned())
    }

    fn pulse_server(&self) -> Result<String, String> {
        let json =
            sources::tool_output(&["pactl", "--format=json", "info"], sources::TOOL_TIMEOUT)?;
        sources::pulse_server_name(&json).ok_or_else(|| "pactl info has no server_name".to_owned())
    }

    fn render_nodes(&self) -> Vec<gpu::RenderNode> {
        gpu::render_nodes()
    }

    fn va_drivers(&self) -> gpu::VaDrivers {
        gpu::VaDrivers::from_process()
    }

    fn open_node(&self, path: &str) -> io::Result<()> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map(drop)
    }

    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn config_file(&self) -> Option<PathBuf> {
        crate::paths::UserDirs::resolve(&crate::paths::Env::from_process())
            .ok()
            .map(|dirs| dirs.config_file())
    }

    fn gpu_startup(&self) -> Vec<String> {
        gpu::startup_report()
            .map(gpu::GpuReport::lines)
            .unwrap_or_default()
    }

    fn vaapi_h264(&self, device: &str, driver: Option<&str>) -> Result<(), String> {
        let argv = vaapi_check_argv(device);
        let mut spec = crate::media::ChildSpec::new(&argv, crate::media::ChildStderr::Capture)
            .timeout(FFMPEG_TIMEOUT);
        if let Some(driver) = driver {
            spec = spec.env("LIBVA_DRIVER_NAME", driver);
        }
        let done = crate::media::run_child(spec)?;
        if done.exited_ok() {
            Ok(())
        } else {
            let mut message = done.describe("ffmpeg");
            if let Some(line) = done.stderr.lines().map(str::trim).rfind(|l| !l.is_empty()) {
                let _ = write!(message, ": {line}");
            }
            Err(message)
        }
    }

    /// This executable re-run as the probe child (`main` answers
    /// `QREC_GPU_PROBE` first), so only the `qrec` binary may call it.
    fn gst_va_factory(&self, device: &str) -> Result<Option<String>, String> {
        let exe = std::env::current_exe().map_err(|e| format!("can't find qrec itself: {e}"))?;
        let probe = gpu::Probe::H264Encoders {
            device: Some(device.to_owned()),
        };
        let answer = gpu::run_probe_child(&exe, &probe, None, gpu::PROBE_TIMEOUT)?;
        match answer.error {
            Some(error) => Err(error),
            None => Ok(answer.factory.or(answer.lp_factory)),
        }
    }

    fn platform(&self) -> HostPlatform {
        crate::platform::current().clone()
    }
}

/// The rows of an `ffmpeg -encoders`, `-decoders` or `-demuxers` list:
/// after the line of dashes as wide as the flags column (`------`,
/// `---`), each row's name and the rest (the flags may hold spaces:
/// `D d alsa`).
fn ffmpeg_rows(text: &str) -> Vec<(&str, &str)> {
    let mut lines = text.lines();
    let Some(width) = lines.by_ref().find_map(|l| {
        let dashes = l.trim();
        (dashes.len() >= 2 && dashes.bytes().all(|b| b == b'-')).then_some(dashes.len())
    }) else {
        return Vec::new();
    };
    lines
        .filter_map(|line| {
            let row = line.strip_prefix(' ').unwrap_or(line);
            let rest = row.get(width..)?.trim_start();
            let (name, description) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            (!name.is_empty()).then(|| (name, description.trim()))
        })
        .collect()
}

/// The names in `ffmpeg -encoders`.
pub fn parse_encoders(text: &str) -> BTreeSet<String> {
    ffmpeg_rows(text)
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// `ffmpeg -decoders`: each name and its codec, which a decoder named
/// otherwise gives at the end of its line (`libopenh264 … (codec h264)`).
pub fn parse_decoders(text: &str) -> BTreeMap<String, String> {
    ffmpeg_rows(text)
        .into_iter()
        .map(|(name, description)| {
            let codec = description
                .rsplit_once("(codec ")
                .and_then(|(_, codec)| codec.strip_suffix(')'))
                .unwrap_or(name);
            (name.to_owned(), codec.to_owned())
        })
        .collect()
}

/// The names in `ffmpeg -demuxers` (`matroska,webm` gives both).
pub fn parse_demuxers(text: &str) -> BTreeSet<String> {
    ffmpeg_rows(text)
        .into_iter()
        .flat_map(|(names, _)| names.split(','))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The first unsigned number in `busctl` (`u 5`) or `gdbus` (`(<uint32 5>,)`) output.
pub fn parse_dbus_u32(text: &str) -> Option<u32> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse().ok())
}

/// Reads a property of the portal (busctl, else gdbus).
fn portal_property(interface: &str, property: &str) -> Result<u32, String> {
    const DEST: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    let busctl = sources::tool_output(
        &[
            "busctl",
            "--user",
            "get-property",
            DEST,
            PATH,
            interface,
            property,
        ],
        PORTAL_TIMEOUT,
    );
    let text = match busctl {
        Ok(text) => text,
        Err(e) if e.contains("wasn't found") => sources::tool_output(
            &[
                "gdbus",
                "call",
                "--session",
                "--dest",
                DEST,
                "--object-path",
                PATH,
                "--method",
                "org.freedesktop.DBus.Properties.Get",
                interface,
                property,
            ],
            PORTAL_TIMEOUT,
        )?,
        Err(e) => return Err(e),
    };
    parse_dbus_u32(&text).ok_or_else(|| format!("unexpected D-Bus answer {:?}", text.trim()))
}

#[cfg(feature = "gst")]
mod gst_impl {
    use std::time::Duration;

    use gst::prelude::*;

    use crate::sources::{GST_INIT_TIMEOUT, init_gstreamer};

    pub(super) fn version() -> Result<String, String> {
        init_gstreamer(GST_INIT_TIMEOUT)?;
        Ok(gst::version_string().to_string())
    }

    pub(super) fn element(name: &str) -> bool {
        init_gstreamer(GST_INIT_TIMEOUT).is_ok() && gst::ElementFactory::find(name).is_some()
    }

    pub(super) fn va_encoders() -> Vec<String> {
        if init_gstreamer(GST_INIT_TIMEOUT).is_err() {
            return Vec::new();
        }
        let mut names: Vec<String> = gst::ElementFactory::factories_with_type(
            gst::ElementFactoryType::VIDEO_ENCODER,
            gst::Rank::NONE,
        )
        .iter()
        .map(|f| f.name().to_string())
        .filter(|n| crate::gpu::va_h264_kind(n).is_some())
        .collect();
        names.sort();
        names
    }

    /// From the registry's metadata and pad templates: no plugin is
    /// loaded (the VA one would open every render node, A§8.10).
    pub(super) fn h264_decoders() -> Vec<String> {
        if init_gstreamer(GST_INIT_TIMEOUT).is_err() {
            return Vec::new();
        }
        let h264 = gst::Caps::new_empty_simple("video/x-h264");
        let mut names: Vec<String> = gst::ElementFactory::factories_with_type(
            gst::ElementFactoryType::DECODER | gst::ElementFactoryType::MEDIA_VIDEO,
            gst::Rank::MARGINAL,
        )
        .iter()
        .filter(|f| {
            !f.metadata(gst::ELEMENT_METADATA_KLASS)
                .unwrap_or_default()
                .contains("Hardware")
        })
        .filter(|f| f.can_sink_any_caps(&h264))
        .map(|f| f.name().to_string())
        .collect();
        names.sort();
        names
    }

    pub(super) fn ges() -> Result<(), String> {
        init_gstreamer(GST_INIT_TIMEOUT)?;
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::Builder::new()
            .name("qrec-ges-init".into())
            .spawn(move || {
                // The renderer's own init: GES picks its mixer by loading
                // every compositor factory, and loading the VA plugin opens
                // every render node (A§8.10), so GES is set up in software
                // only, exactly as the preview renderer will use it.
                let _ = tx.send(crate::ges_render::init().map_err(|e| e.to_string()));
            })
            .map_err(|e| format!("can't start the GES init thread: {e}"))?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| "GES didn't initialize within 10 s".to_owned())?
    }
}

#[cfg(not(feature = "gst"))]
mod gst_impl {
    const NO_GST: &str = "this qrec was built without GStreamer (cargo feature `gst`)";

    pub(super) fn version() -> Result<String, String> {
        Err(NO_GST.to_owned())
    }

    pub(super) fn element(_name: &str) -> bool {
        false
    }

    pub(super) fn va_encoders() -> Vec<String> {
        Vec::new()
    }

    pub(super) fn h264_decoders() -> Vec<String> {
        Vec::new()
    }

    pub(super) fn ges() -> Result<(), String> {
        Err(NO_GST.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use qrec_core::ctx::ScreenInfo;
    use qrec_core::keymap::NIRI_SNIPPET;
    use qrec_core::platform::{AppImage, QREC};

    use super::*;
    use crate::platform::{PlatformEnv, detect};

    /// A machine described by data.
    #[derive(Clone)]
    struct Fake {
        gst: Result<String, String>,
        elements: BTreeSet<&'static str>,
        va_encoders: Vec<String>,
        h264_decoders: Vec<String>,
        programs: BTreeMap<&'static str, String>,
        encoders: BTreeSet<String>,
        decoders: BTreeMap<String, String>,
        demuxers: BTreeSet<String>,
        screens: ScreenBackend,
        /// The Unix sockets there are.
        sockets: BTreeSet<PathBuf>,
        portal: Result<PortalInfo, String>,
        x11: Result<X11Server, String>,
        pactl: Result<String, String>,
        pulse: Result<String, String>,
        nodes: Vec<gpu::RenderNode>,
        drivers: gpu::VaDrivers,
        /// Opening an Intel or AMD node: `None` opens, else this errno.
        node_errno: Option<i32>,
        env: BTreeMap<&'static str, &'static str>,
        /// The h264_vaapi check with the node's driver…
        vaapi: Result<(), String>,
        /// …and with Intel's other one, where it runs again.
        vaapi_other: Result<(), String>,
        va_factory: Result<Option<String>, String>,
        startup: Vec<String>,
        platform: HostPlatform,
        /// The h264_vaapi checks with the node's driver…
        vaapi_probed: Cell<u32>,
        /// …and with Intel's other one.
        vaapi_retried: Cell<u32>,
        factory_probed: Cell<u32>,
        nodes_opened: Cell<u32>,
        portal_asked: Cell<u32>,
        x11_asked: Cell<u32>,
    }

    fn node(path: &str, vendor: u16) -> gpu::RenderNode {
        gpu::RenderNode {
            path: path.to_owned(),
            vendor: Some(vendor),
            device_id: None,
            driver: None,
        }
    }

    /// A node with its PCI device id and kernel driver.
    fn gpu_node(path: &str, vendor: u16, device_id: u16, driver: &str) -> gpu::RenderNode {
        gpu::RenderNode {
            device_id: Some(device_id),
            driver: Some(driver.to_owned()),
            ..node(path, vendor)
        }
    }

    /// Libva's directory `dir` holding these drivers (as data: nothing on
    /// disk is looked at).
    fn drivers_in(dir: &str, names: &[&str]) -> gpu::VaDrivers {
        gpu::VaDrivers {
            dirs: vec![PathBuf::from(dir)],
            found: names
                .iter()
                .map(|n| {
                    (
                        (*n).to_owned(),
                        Path::new(dir).join(format!("{n}_drv_video.so")),
                    )
                })
                .collect(),
        }
    }

    /// The platform of these variables and os-release.
    fn platform(vars: &[(&str, &str)], os_release: &str) -> HostPlatform {
        let env = vars
            .iter()
            .fold(PlatformEnv::default(), |env, (key, value)| {
                env.with(*key, *value)
            })
            .with_os_release(os_release);
        detect(&env)
    }

    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    /// An X server's answer: its RandR version and its screens, each a
    /// name and a size, side by side.
    fn x_server(randr: Option<(u32, u32)>, screens: &[(&str, u32, u32)]) -> X11Server {
        let mut x = 0;
        let screens = screens
            .iter()
            .map(|&(output, width, height)| {
                let screen = ScreenInfo {
                    output: output.to_owned(),
                    description: output.to_owned(),
                    x,
                    y: 0,
                    width,
                    height,
                    scale: 1.0,
                };
                x += i32::try_from(width).unwrap();
                screen
            })
            .collect();
        X11Server { randr, screens }
    }

    impl Fake {
        /// The dev machine: niri on NixOS, everything present, Intel
        /// renderD129 + NVIDIA renderD128.
        fn healthy() -> Self {
            Fake {
                gst: Ok("GStreamer 1.26.11".to_owned()),
                elements: [&PIPEWIRESRC, &XIMAGESRC]
                    .into_iter()
                    .chain(ELEMENTS)
                    .chain(FAKE_ELEMENTS)
                    .map(|e| e.name)
                    .collect(),
                va_encoders: vec!["vah264enc".to_owned()],
                h264_decoders: vec!["avdec_h264".to_owned(), "openh264dec".to_owned()],
                programs: ["ffmpeg", "ffprobe", "pactl"]
                    .map(|name| (name, format!("/nix/bin/{name}")))
                    .into(),
                // ffmpeg-full's: the doctor's encoders and h264_vaapi.
                encoders: ENCODERS
                    .iter()
                    .map(|(n, ..)| *n)
                    .chain(["h264_vaapi"])
                    .map(str::to_owned)
                    .collect(),
                decoders: [("h264", "h264"), ("flac", "flac"), ("libdav1d", "av1")]
                    .iter()
                    .map(|(n, c)| ((*n).to_owned(), (*c).to_owned()))
                    .collect(),
                demuxers: names(&["matroska", "webm", "mov"]),
                screens: ScreenBackend::Portal,
                sockets: [PathBuf::from("/run/user/1000/pipewire-0")].into(),
                portal: Ok(PortalInfo {
                    screencast_version: 5,
                    source_types: Some(7),
                    registry_version: Some(1),
                }),
                // Nobody asks it on Wayland; an X11 machine's has two
                // monitors.
                x11: Ok(x_server(
                    Some((1, 6)),
                    &[("eDP-1", 1920, 1080), ("HDMI-1", 2560, 1440)],
                )),
                pactl: Ok("pactl 17.0".to_owned()),
                pulse: Ok("PulseAudio (on PipeWire 1.6.6)".to_owned()),
                nodes: vec![
                    gpu_node("/dev/dri/renderD128", PCI_NVIDIA, 0x2db8, "nvidia"),
                    gpu_node("/dev/dri/renderD129", PCI_INTEL, 0x7d51, "i915"),
                ],
                drivers: drivers_in("/run/opengl-driver/lib/dri", &["iHD", "i965"]),
                node_errno: None,
                env: [
                    ("LIBVA_DRIVER_NAME", "iHD"),
                    ("XDG_RUNTIME_DIR", "/run/user/1000"),
                ]
                .into(),
                vaapi: Ok(()),
                // i965 doesn't drive Arrow Lake.
                vaapi_other: Err("ffmpeg exited with status 1".to_owned()),
                va_factory: Ok(Some("vah264enc".to_owned())),
                startup: vec!["set LIBVA_DRIVER_NAME=iHD (was nvidia)".to_owned()],
                platform: platform(
                    &[
                        ("WAYLAND_DISPLAY", "wayland-1"),
                        ("NIRI_SOCKET", "/run/user/1000/niri.wayland-1.sock"),
                        ("XDG_CURRENT_DESKTOP", "niri"),
                    ],
                    "ID=nixos\n",
                ),
                vaapi_probed: Cell::new(0),
                vaapi_retried: Cell::new(0),
                factory_probed: Cell::new(0),
                nodes_opened: Cell::new(0),
                portal_asked: Cell::new(0),
                x11_asked: Cell::new(0),
            }
        }

        /// The session of these variables and os-release: its platform and
        /// the screen backend it picks (`QREC_SCREEN_BACKEND` among them).
        fn on(mut self, vars: &[(&str, &str)], os_release: &str) -> Self {
            self.platform = platform(vars, os_release);
            let var = |key: &str| vars.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
            self.screens = ScreenBackend::select(
                self.platform.session,
                var("DISPLAY"),
                var("QREC_SCREEN_BACKEND"),
            );
            self
        }

        /// One Intel node (renderD128) whose VA encoder is `vah264enc`.
        fn intel_only(mut self) -> Self {
            self.nodes = vec![gpu_node("/dev/dri/renderD128", PCI_INTEL, 0x7d51, "i915")];
            self.va_factory = Ok(Some("vah264enc".to_owned()));
            self.startup.clear();
            self
        }

        /// The node a probe runs on (also through a symlink), which is
        /// never NVIDIA.
        fn probed_node(&self, device: &str) -> Option<&gpu::RenderNode> {
            let node = gpu::node_for(device, &self.nodes);
            assert_ne!(
                node.and_then(|n| n.vendor),
                Some(PCI_NVIDIA),
                "NVIDIA must never be probed"
            );
            node
        }
    }

    impl Probe for Fake {
        fn gstreamer(&self) -> Result<String, String> {
            self.gst.clone()
        }
        fn gst_element(&self, name: &str) -> bool {
            self.elements.contains(name)
        }
        fn gst_va_encoders(&self) -> Vec<String> {
            self.va_encoders.clone()
        }
        fn gst_h264_decoders(&self) -> Vec<String> {
            self.h264_decoders.clone()
        }
        fn ges(&self) -> Result<(), String> {
            self.gst.clone().map(|_| ())
        }
        fn which(&self, program: &str) -> Option<PathBuf> {
            self.programs.get(program).map(PathBuf::from)
        }
        fn version(&self, program: &str) -> Result<String, String> {
            Ok(format!("{program} version 8.1.2"))
        }
        fn ffmpeg_encoders(&self) -> Result<BTreeSet<String>, String> {
            Ok(self.encoders.clone())
        }
        fn ffmpeg_decoders(&self) -> Result<BTreeMap<String, String>, String> {
            Ok(self.decoders.clone())
        }
        fn ffmpeg_demuxers(&self) -> Result<BTreeSet<String>, String> {
            Ok(self.demuxers.clone())
        }
        fn screen_backend(&self) -> ScreenBackend {
            self.screens.clone()
        }
        fn unix_socket(&self, path: &Path) -> Result<(), String> {
            if self.sockets.contains(path) {
                Ok(())
            } else {
                Err(format!("nothing at {}", path.display()))
            }
        }
        fn portal(&self) -> Result<PortalInfo, String> {
            self.portal_asked.set(self.portal_asked.get() + 1);
            self.portal.clone()
        }
        fn x11(&self, display: &str) -> Result<X11Server, String> {
            assert_eq!(self.screens.to_string(), format!("x11 (DISPLAY={display})"));
            self.x11_asked.set(self.x11_asked.get() + 1);
            self.x11.clone()
        }
        fn pactl_version(&self) -> Result<String, String> {
            self.pactl.clone()
        }
        fn pulse_server(&self) -> Result<String, String> {
            self.pulse.clone()
        }
        fn render_nodes(&self) -> Vec<gpu::RenderNode> {
            self.nodes.clone()
        }
        fn va_drivers(&self) -> gpu::VaDrivers {
            self.drivers.clone()
        }
        fn open_node(&self, path: &str) -> io::Result<()> {
            self.probed_node(path);
            self.nodes_opened.set(self.nodes_opened.get() + 1);
            match self.node_errno {
                None => Ok(()),
                Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            }
        }
        fn env(&self, key: &str) -> Option<String> {
            self.env.get(key).map(|v| (*v).to_owned())
        }
        /// The check loads the node's driver (the pinned one when it
        /// fits), then, only where that one failed, Intel's other driver.
        fn vaapi_h264(&self, device: &str, driver: Option<&str>) -> Result<(), String> {
            let node = self.probed_node(device).expect("a node of this machine");
            let pinned = self.env.get("LIBVA_DRIVER_NAME").copied();
            let first = gpu::child_libva_driver(node, pinned, &self.drivers);
            if driver == first.as_deref() {
                self.vaapi_probed.set(self.vaapi_probed.get() + 1);
                return self.vaapi.clone();
            }
            assert!(self.vaapi.is_err(), "a retry after a failure only");
            assert_eq!(
                driver,
                first.and_then(|f| gpu::retry_driver(node, &f, &self.drivers))
            );
            self.vaapi_retried.set(self.vaapi_retried.get() + 1);
            self.vaapi_other.clone()
        }
        fn gst_va_factory(&self, device: &str) -> Result<Option<String>, String> {
            self.probed_node(device);
            self.factory_probed.set(self.factory_probed.get() + 1);
            self.va_factory.clone()
        }
        fn gpu_startup(&self) -> Vec<String> {
            self.startup.clone()
        }
        fn platform(&self) -> HostPlatform {
            self.platform.clone()
        }
        fn config_file(&self) -> Option<PathBuf> {
            Some(PathBuf::from("/home/me/.config/qrec/config.toml"))
        }
    }

    #[test]
    fn configured_symlinks_resolve_to_their_node() {
        // Stand-ins for /dev/dri: a node file and a by-path symlink to it.
        let dir = tempfile::tempdir().unwrap();
        let node_path = dir.path().join("renderD129");
        std::fs::write(&node_path, "").unwrap();
        let link = dir.path().join("pci-0000:00:02.0-render");
        std::os::unix::fs::symlink(&node_path, &link).unwrap();
        let mut fake = Fake::healthy();
        fake.nodes = vec![node(node_path.to_str().unwrap(), PCI_INTEL)];
        let report = run_with(
            &fake,
            &DoctorOptions {
                config_vaapi_device: Some(link.to_string_lossy().into_owned()),
                ..opts()
            },
        );
        let va = report.va.as_ref().unwrap();
        assert_eq!(va.origin.as_deref(), Some("config"));
        assert_eq!(va.vendor, Some(PCI_INTEL));
        assert_eq!(va.gst_factory.as_deref(), Some("vah264enc"));
        assert_eq!(va.startup, ["set LIBVA_DRIVER_NAME=iHD (was nvidia)"]);
        assert!(
            report
                .to_text()
                .contains("set LIBVA_DRIVER_NAME=iHD (was nvidia)")
        );
    }

    fn opts() -> DoctorOptions {
        DoctorOptions {
            probe_vaapi: true,
            ..DoctorOptions::default()
        }
    }

    fn check<'a>(report: &'a DoctorReport, id: &str) -> &'a Check {
        report
            .checks
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no check {id}: {:#?}", report.checks))
    }

    fn off(report: &DoctorReport) -> Vec<Feature> {
        report.features_off.iter().map(|f| f.feature).collect()
    }

    #[test]
    fn a_healthy_machine_passes_every_spec_check() {
        let fake = Fake::healthy();
        let report = run_with(&fake, &opts());
        assert!(report.ok(), "{}", report.to_text());
        assert_eq!(report.exit_code(), 0);
        assert!(report.missing_dependency().is_none());
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        for id in [
            "gstreamer",
            "gst.pipewiresrc",
            "gst.v4l2src",
            "gst.pulsesrc",
            "gst.compositor",
            "gst.videorate",
            "gst.x264enc",
            "gst.openh264enc",
            "gst.matroskamux",
            "gst.flacenc",
            "gst.matroskademux",
            "gst.flacdec",
            "gst.gtk4paintablesink",
            "gst.h264dec",
            "gst.vah264enc",
            "ges",
            "ffmpeg",
            "ffprobe",
            "ffmpeg.decoders",
            "ffmpeg.demuxers",
            "ffmpeg.libx264",
            "ffmpeg.libopenh264",
            "ffmpeg.libvpx-vp9",
            "ffmpeg.libopus",
            "ffmpeg.libsvtav1",
            "pipewire.daemon",
            "portal.screencast",
            "portal.registry",
            "pactl",
            "sound.server",
            "vaapi",
        ] {
            let c = check(&report, id);
            assert!(c.ok && c.hint.is_none(), "{c:?}");
            assert_eq!(c.required, c.needed_for.contains(&Feature::App), "{c:?}");
        }
        let needs = |id: &str| check(&report, id).needed_for.clone();
        for id in [
            "gstreamer",
            "gst.gtk4paintablesink",
            "gst.h264dec",
            "ges",
            "ffmpeg",
        ] {
            assert_eq!(needs(id), [Feature::App], "{id}");
        }
        // niri records screens through the portal: its checks, no X
        // server's, and no wl-copy (the GUI copies through GDK).
        for id in ["gst.pipewiresrc", "pipewire.daemon", "portal.screencast"] {
            assert_eq!(needs(id), [Feature::Screens], "{id}");
        }
        for id in ["gst.ximagesrc", "x11.randr", "display", "wl-copy"] {
            assert!(report.checks.iter().all(|c| c.id != id), "{id}");
        }
        assert_eq!((fake.portal_asked.get(), fake.x11_asked.get()), (1, 0));
        assert_eq!(
            check(&report, "pipewire.daemon").detail,
            "/run/user/1000/pipewire-0 (the portal's screen casts)"
        );
        assert_eq!(needs("gst.v4l2src"), [Feature::Cameras]);
        assert_eq!(needs("pactl"), [Feature::Audio]);
        assert_eq!(needs("sound.server"), [Feature::Audio]);
        assert_eq!(needs("gst.matroskamux"), [Feature::Recording]);
        assert_eq!(needs("ffmpeg.aac"), [Feature::ExportMp4]);
        assert_eq!(
            needs("ffmpeg.libopus"),
            [Feature::ExportWebm, Feature::ExportAv1]
        );
        assert_eq!(needs("ffmpeg.libsvtav1"), [Feature::ExportAv1]);
        for id in ["gst.vah264enc", "vaapi", "portal.registry"] {
            assert!(needs(id).is_empty(), "{id}");
        }
        // The dev box records and exports MP4 with VA, so x264enc,
        // openh264enc, libx264 and libopenh264 are needed for nothing.
        assert!(needs("gst.x264enc").is_empty());
        assert!(needs("gst.openh264enc").is_empty());
        assert!(needs("ffmpeg.libx264").is_empty());
        assert!(needs("ffmpeg.libopenh264").is_empty());
        assert_eq!(
            check(&report, "gst.vah264enc").detail,
            "vah264enc records on /dev/dri/renderD129"
        );
        assert_eq!(
            check(&report, "portal.screencast").detail,
            "xdg-desktop-portal ScreenCast v5 (monitors, windows, virtual)"
        );
        assert_eq!(
            check(&report, "sound.server").detail,
            "PulseAudio (on PipeWire 1.6.6)"
        );
        let va = report.va.as_ref().unwrap();
        assert_eq!(va.device.as_deref(), Some("/dev/dri/renderD129"));
        assert_eq!(va.origin.as_deref(), Some("auto"));
        assert_eq!(va.expected_driver.as_deref(), Some("iHD"));
        assert_eq!(va.ffmpeg_h264, Some(true));
        assert_eq!(va.gst_factory.as_deref(), Some("vah264enc"));
        assert_eq!((fake.vaapi_probed.get(), fake.factory_probed.get()), (1, 1));
        assert_eq!(
            check(&report, "vaapi").detail,
            "/dev/dri/renderD129 (Intel, auto) · LIBVA_DRIVER_NAME=iHD · ffmpeg h264_vaapi works"
        );
        assert_eq!(report.niri_snippet, NIRI_SNIPPET);
        assert_eq!(report.shortcuts, global_snippet(&Desktop::Niri, QREC));
        assert_eq!(report.screen_backend, ScreenBackendKind::Portal);
        assert_eq!(report.session, "Wayland (niri) · screens: portal");
        let text = report.to_text();
        assert!(
            text.starts_with(
                "Qrec doctor\n\nSession: Wayland (niri) · screens: portal\n\n  ok        gstreamer "
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "\nGlobal shortcuts for niri (paste into ~/.config/niri/config.kdl):\n\n\
                 binds {\n    Mod+Shift+R { spawn \"qrec\" \"record\" \"toggle\"; }\n"
            ),
            "{text}"
        );
        assert!(text.ends_with("Everything Qrec needs is here.\n"), "{text}");
        let json = report.to_json();
        assert_eq!(json["checks"][0]["id"], "gstreamer");
        assert_eq!(json["checks"][0]["required"], true);
        assert_eq!(json["checks"][0]["neededFor"], serde_json::json!(["app"]));
        assert_eq!(json["featuresOff"], serde_json::json!([]));
        assert_eq!(json["platform"]["desktop"], "niri");
        assert_eq!(json["screenBackend"], "portal");
        assert_eq!(json["session"], "Wayland (niri) · screens: portal");
        assert_eq!(json["shortcuts"]["desktop"], "niri");
        assert_eq!(json["shortcuts"]["text"], NIRI_SNIPPET);
        assert_eq!(json["niriSnippet"], NIRI_SNIPPET);
        assert_eq!(json["va"]["expectedDriver"], "iHD");
        assert_eq!(json["va"]["gstFactory"], "vah264enc");
        let back: DoctorReport = serde_json::from_value(json).unwrap();
        assert_eq!(back, report);
    }

    /// Only an app check is `missing_dependency`; the rest turn their
    /// features off, a line each, and the verdict stays exit 0.
    #[test]
    fn only_app_checks_mean_exit_5() {
        let mut fake = Fake::healthy();
        fake.encoders.remove("aac");
        fake.encoders.remove("libsvtav1");
        fake.portal = Err("busctl exited with status 1".to_owned());
        let report = run_with(&fake, &opts());
        assert_eq!(report.exit_code(), 0, "{}", report.to_text());
        assert!(report.missing().is_empty());
        assert!(report.missing_dependency().is_none());
        assert_eq!(
            off(&report),
            [Feature::Screens, Feature::ExportMp4, Feature::ExportAv1]
        );
        assert_eq!(report.features_off[0].checks, ["portal.screencast"]);
        assert_eq!(
            report.features_off[0].message,
            "Screen capture is off: no ScreenCast portal: busctl exited with status 1"
        );
        for c in report.checks.iter().filter(|c| !c.ok) {
            assert!(c.hint.is_some() && !c.required, "{c:?}");
        }
        assert!(report.checks.iter().all(|c| c.id != "portal.registry"));
        let text = report.to_text();
        assert!(text.contains("off       portal.screencast"), "{text}");
        assert!(
            text.contains("MP4 export is off: ffmpeg has no aac encoder (MP4 audio)\n"),
            "{text}"
        );
        assert!(
            text.ends_with("Everything else Qrec needs is here.\n"),
            "{text}"
        );

        // An app check fails: exit 5, naming only the app checks.
        fake.elements.remove("gtk4paintablesink");
        fake.h264_decoders.clear();
        let report = run_with(&fake, &opts());
        assert_eq!(report.exit_code(), MISSING_DEPENDENCY_EXIT);
        let missing: Vec<&str> = report.missing().iter().map(|c| c.id.as_str()).collect();
        assert_eq!(missing, ["gst.gtk4paintablesink", "gst.h264dec"]);
        // The dev box is NixOS: its fix is the Nix package's.
        let h264dec = check(&report, "gst.h264dec");
        assert_eq!(
            h264dec.hint.as_deref(),
            Some("run qrec from its Nix package, which ships gst_all_1.gst-libav")
        );
        assert_eq!(
            h264dec.fix.as_ref().map(|f| f.packages.clone()),
            Some(vec!["gst_all_1.gst-libav".to_owned()])
        );
        let error = report.missing_dependency().unwrap();
        assert_eq!(error.code, ErrorCode::MissingDependency);
        assert_eq!(error.exit_code(), 5);
        assert_eq!(error.message, "missing: gst.gtk4paintablesink, gst.h264dec");
        let text = report.to_text();
        assert!(text.contains("MISSING   gst.h264dec"), "{text}");
        assert!(
            text.contains("→ run qrec from its Nix package, which ships gst_all_1.gst-libav"),
            "{text}"
        );
        assert!(text.contains("Screen capture is off: "), "{text}");
        assert!(
            text.ends_with(
                "The swamp is missing something: 2 required checks failed \
                 (gst.gtk4paintablesink, gst.h264dec).\n"
            ),
            "{text}"
        );

        // In the flake's dev shell, `nix develop` (S23's frame).
        fake.platform.nix_shell = true;
        let report = run_with(&fake, &opts());
        let gtk4 = check(&report, "gst.gtk4paintablesink").fix.clone().unwrap();
        assert_eq!(gtk4.command.as_deref(), Some("nix develop"));
        assert_eq!(
            gtk4.note.as_deref(),
            Some("the flake provides gst-plugins-rs")
        );
        let json = report.to_json();
        assert_eq!(json["checks"][0]["id"], "gstreamer");
        assert!(
            json["checks"][0].get("fix").is_none(),
            "a passed check has no fix"
        );
        let back: DoctorReport = serde_json::from_value(json).unwrap();
        assert_eq!(back, report);
    }

    /// Recordings use the VA encoder bound to the node (its full one, else
    /// its low-power one), else x264enc, else openh264enc (A§9.5): only the
    /// one that records is needed, and with none, either software encoder
    /// would turn recording on.
    #[test]
    fn recording_needs_va_or_x264_or_openh264() {
        let mut fake = Fake::healthy();
        fake.elements.remove("x264enc");
        let report = run_with(&fake, &opts());
        let x264 = check(&report, "gst.x264enc");
        assert!(!x264.ok && x264.needed_for.is_empty(), "{x264:?}");
        assert!(x264.detail.ends_with(": recordings use vah264enc"));
        assert!(check(&report, "gst.openh264enc").needed_for.is_empty());
        assert!(report.features_off.is_empty());

        // The probe found no encoder bound to the node: openh264enc records.
        fake.va_factory = Ok(None);
        let report = run_with(&fake, &opts());
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        assert!(
            check(&report, "gst.x264enc")
                .detail
                .ends_with(": recordings use openh264enc")
        );
        assert_eq!(
            check(&report, "gst.openh264enc").needed_for,
            [Feature::Recording]
        );
        let va = check(&report, "gst.vah264enc");
        assert!(!va.ok && va.needed_for.is_empty(), "{va:?}");
        assert_eq!(
            va.detail,
            "none of vah264enc encodes on /dev/dri/renderD129: recordings use OpenH264 on the CPU"
        );

        // Neither software encoder either: recording is off, and either one
        // turns it on.
        fake.elements.remove("openh264enc");
        let report = run_with(&fake, &opts());
        assert_eq!(off(&report), [Feature::Recording]);
        assert_eq!(
            report.features_off[0].checks,
            ["gst.x264enc", "gst.openh264enc"]
        );
        assert_eq!(
            report.features_off[0].message,
            "Recording is off: x264enc (H.264 recording) isn't in the GStreamer registry \
             (and no VA-API encoder or openh264enc records instead); openh264enc (H.264 \
             recording) isn't in the GStreamer registry (and no VA-API encoder or x264enc \
             records instead)"
        );
        assert!(
            check(&report, "gst.vah264enc")
                .detail
                .ends_with(": no software H.264 encoder records either")
        );

        // A failed probe: noted, and nothing records either.
        fake.va_factory = Err("the GPU probe didn't finish within 10 s".to_owned());
        let report = run_with(&fake, &opts());
        assert_eq!(off(&report), [Feature::Recording]);
        assert!(
            report
                .va
                .as_ref()
                .unwrap()
                .note
                .as_deref()
                .unwrap()
                .contains("VA encoder probe failed")
        );

        // Unprobed (the GUI's doctor): an Intel node and a VA encoder do.
        let report = run_with(
            &fake,
            &DoctorOptions {
                probe_vaapi: false,
                ..opts()
            },
        );
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        assert_eq!(check(&report, "gst.vah264enc").detail, "vah264enc");
        // The fake backend records with x264enc, else openh264enc, whatever
        // the GPU has.
        let fake_backend = DoctorOptions {
            fake_backend: true,
            probe_vaapi: false,
            ..opts()
        };
        assert_eq!(off(&run_with(&fake, &fake_backend)), [Feature::Recording]);
        fake.elements.insert("openh264enc");
        let report = run_with(&fake, &fake_backend);
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        assert_eq!(
            check(&report, "gst.openh264enc").needed_for,
            [Feature::Recording]
        );
        assert!(
            fake.factory_probed.get() == 4,
            "probed with probe_vaapi only"
        );
    }

    /// Intel Arc and Meteor Lake have only a low-power VA encoder: it
    /// records, probed or not, and a full one wins where both exist.
    #[test]
    fn a_low_power_va_encoder_records() {
        let mut fake = Fake::healthy();
        fake.elements.remove("x264enc");
        fake.elements.remove("openh264enc");
        fake.va_encoders = vec!["vah264lpenc".to_owned()];
        fake.va_factory = Ok(Some("vah264lpenc".to_owned()));
        let report = run_with(&fake, &opts());
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        assert_eq!(
            check(&report, "gst.vah264enc").detail,
            "vah264lpenc records on /dev/dri/renderD129"
        );
        assert!(
            check(&report, "gst.x264enc")
                .detail
                .ends_with(": recordings use vah264lpenc")
        );
        assert_eq!(
            report.va.as_ref().unwrap().gst_factory.as_deref(),
            Some("vah264lpenc")
        );

        // Unprobed: the first full encoder GStreamer lists, else the
        // first low-power one.
        let unprobed = DoctorOptions {
            probe_vaapi: false,
            ..opts()
        };
        let report = run_with(&fake, &unprobed);
        assert!(
            check(&report, "gst.x264enc")
                .detail
                .ends_with(": recordings use vah264lpenc")
        );
        fake.va_encoders = vec!["vah264lpenc".to_owned(), "varenderD129h264enc".to_owned()];
        let report = run_with(&fake, &unprobed);
        assert!(
            check(&report, "gst.x264enc")
                .detail
                .ends_with(": recordings use varenderD129h264enc")
        );
        assert_eq!(
            check(&report, "gst.vah264enc").detail,
            "vah264lpenc, varenderD129h264enc"
        );
    }

    /// GStreamer lists no VA encoder: on an Intel or AMD node, the node's
    /// driver first, then GStreamer's VA plugin; the plugin alone once
    /// ffmpeg's h264_vaapi encoded on the node. Without such a node,
    /// nothing to install: the row informs.
    #[test]
    fn no_va_encoder_hints_the_driver_then_the_plugin() {
        let gnome = |os_release: &str| {
            platform(
                &[
                    ("WAYLAND_DISPLAY", "wayland-0"),
                    ("XDG_CURRENT_DESKTOP", "GNOME"),
                ],
                os_release,
            )
        };
        // Stock Fedora on AMD: its Mesa doesn't encode H.264, so the VA
        // plugin registers no encoder and h264_vaapi fails.
        let mut fake = Fake::healthy();
        fake.platform = gnome("ID=fedora\nVERSION_ID=44\n");
        fake.nodes = vec![node("/dev/dri/renderD128", PCI_AMD)];
        fake.drivers = drivers_in("/usr/lib64/dri", &["radeonsi"]);
        fake.env.insert("LIBVA_DRIVER_NAME", "radeonsi");
        fake.startup.clear();
        fake.va_encoders.clear();
        fake.vaapi = Err("ffmpeg exited with status 1".to_owned());
        let report = run_with(&fake, &opts());
        assert_eq!(report.va.as_ref().unwrap().ffmpeg_h264, Some(false));
        let va = check(&report, "gst.vah264enc");
        let fix = va.fix.clone().unwrap();
        assert_eq!(
            fix.command.as_deref(),
            Some("sudo dnf install mesa-va-drivers-freeworld")
        );
        assert_eq!(
            fix.note.as_deref(),
            Some(
                "it comes from RPM Fusion, since Fedora's own Mesa doesn't encode H.264: \
                 enable that repository first; if GStreamer still has no VA encoder: \
                 install GStreamer's VA plugin (upstream gst-plugins-bad) with dnf"
            )
        );
        assert_eq!(va.hint, Some(fix.line()));
        // The `vaapi` check names the same driver.
        let vaapi = check(&report, "vaapi").fix.clone().unwrap();
        assert_eq!(vaapi.command, fix.command);
        // Unprobed (the GUI's doctor), the driver isn't known to work: the same.
        let unprobed = DoctorOptions {
            probe_vaapi: false,
            ..opts()
        };
        let report = run_with(&fake, &unprobed);
        assert_eq!(check(&report, "gst.vah264enc").fix, Some(fix));

        // Arch on Intel, whose h264_vaapi encodes: only the plugin is missing.
        fake.platform = gnome("ID=arch\n");
        fake.nodes = vec![node("/dev/dri/renderD128", PCI_INTEL)];
        fake.drivers = drivers_in("/usr/lib/dri", &["iHD"]);
        fake.env.insert("LIBVA_DRIVER_NAME", "iHD");
        fake.vaapi = Ok(());
        let report = run_with(&fake, &opts());
        assert_eq!(report.va.as_ref().unwrap().ffmpeg_h264, Some(true));
        let plugin = Some("sudo pacman -S gst-plugin-va");
        assert_eq!(check(&report, "gst.vah264enc").hint.as_deref(), plugin);
        let report = run_with(&fake, &unprobed);
        assert_eq!(
            check(&report, "gst.vah264enc").hint.as_deref(),
            Some(
                "install Intel's iHD VA-API driver (chips older than Broadwell need the i965 \
                 one instead) with pacman; if GStreamer still has no VA encoder: \
                 sudo pacman -S gst-plugin-va"
            )
        );
        // A Haswell, which only i965 drives: GStreamer's VA plugin doesn't
        // encode with i965, so neither the driver nor the plugin helps and
        // the row informs, i965 installed (exports encode) or not, probed
        // or not.
        fake.nodes = vec![gpu_node("/dev/dri/renderD128", PCI_INTEL, 0x0416, "i915")];
        fake.va_factory = Ok(None);
        fake.env.insert("LIBVA_DRIVER_NAME", "i965");
        for (files, vaapi) in [
            (&["iHD", "i965"][..], Ok(())),
            (&["iHD"][..], Err("ffmpeg exited with status 1".to_owned())),
        ] {
            fake.drivers = drivers_in("/usr/lib/dri", files);
            fake.vaapi = vaapi;
            for options in [opts(), unprobed.clone()] {
                let va = check(&run_with(&fake, &options), "gst.vah264enc").clone();
                assert!(va.ok && va.hint.is_none() && va.fix.is_none(), "{va:?}");
                assert_eq!(
                    va.detail,
                    "GStreamer's VA plugin doesn't encode with i965, the only VA-API driver of \
                     this Intel GPU: recordings use x264 on the CPU"
                );
            }
        }
        // A Kaby Lake with i965 alone, whose exports it encodes: iHD would
        // record, then the plugin. With iHD installed too, the i965 is the
        // user's LIBVA_DRIVER_NAME: unsetting it is the fix.
        fake.nodes = vec![gpu_node("/dev/dri/renderD128", PCI_INTEL, 0x5916, "i915")];
        fake.drivers = drivers_in("/usr/lib/dri", &["i965"]);
        fake.vaapi = Ok(());
        let report = run_with(&fake, &opts());
        assert!(check(&report, "vaapi").ok);
        assert_eq!(
            check(&report, "gst.vah264enc").hint.as_deref(),
            Some(
                "install Intel's iHD VA-API driver (chips older than Broadwell need the i965 \
                 one instead) with pacman; if GStreamer still has no VA encoder: \
                 sudo pacman -S gst-plugin-va"
            )
        );
        fake.drivers = drivers_in("/usr/lib/dri", &["iHD", "i965"]);
        let va = check(&run_with(&fake, &opts()), "gst.vah264enc").clone();
        assert!(!va.ok && va.fix.is_none(), "{va:?}");
        assert_eq!(
            va.hint.as_deref(),
            Some("unset LIBVA_DRIVER_NAME=i965: Qrec then loads iHD, which GStreamer records with")
        );
        // No Intel or AMD node (NVIDIA only, a virtual machine): the VA
        // plugin can't help, nothing to install, the row informs.
        for vendor in [PCI_NVIDIA, gpu::VENDOR_VIRTIO] {
            fake.nodes = vec![node("/dev/dri/renderD128", vendor)];
            fake.env.remove("LIBVA_DRIVER_NAME");
            let report = run_with(&fake, &opts());
            let va = check(&report, "gst.vah264enc");
            assert!(va.ok && va.hint.is_none() && va.fix.is_none(), "{va:?}");
            assert_eq!(
                va.detail,
                "no Intel or AMD GPU: recordings use x264 on the CPU"
            );
        }
    }

    /// MP4's video is h264_vaapi's where its check passed, else libx264's,
    /// else libopenh264's (the export's chain, the recordings' rule): only
    /// the one that encodes is needed, and with none either software
    /// encoder turns MP4 export on; aac and pcm_s16le always are.
    #[test]
    fn mp4_exports_need_vaapi_libx264_or_libopenh264() {
        // Stock Fedora's ffmpeg-free on the dev box's Intel node.
        let mut fake = Fake::healthy();
        fake.encoders.remove("libx264");
        let report = run_with(&fake, &opts());
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        let x264 = check(&report, "ffmpeg.libx264");
        assert!(
            !x264.ok && !x264.required && x264.needed_for.is_empty(),
            "{x264:?}"
        );
        assert_eq!(
            x264.detail,
            "ffmpeg has no libx264 encoder: MP4 exports use h264_vaapi on /dev/dri/renderD129"
        );
        // Its fix stays: the better encoder, the one x264 by name needs.
        assert!(x264.fix.is_some() && x264.hint.is_some(), "{x264:?}");
        assert!(check(&report, "ffmpeg.libopenh264").needed_for.is_empty());
        let text = report.to_text();
        assert!(text.contains("optional  ffmpeg.libx264"), "{text}");
        assert!(text.ends_with("Everything Qrec needs is here.\n"), "{text}");

        // MP4's audio is still ffmpeg's.
        fake.encoders.remove("aac");
        let report = run_with(&fake, &opts());
        assert_eq!(off(&report), [Feature::ExportMp4]);
        assert_eq!(report.features_off[0].checks, ["ffmpeg.aac"]);
        fake.encoders.insert("aac".to_owned());

        // The h264_vaapi check failed: libopenh264 encodes MP4, and the VA
        // check says so.
        fake.vaapi = Err("ffmpeg exited with status 1".to_owned());
        let report = run_with(&fake, &opts());
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        assert_eq!(
            check(&report, "ffmpeg.libopenh264").needed_for,
            [Feature::ExportMp4]
        );
        let x264 = check(&report, "ffmpeg.libx264");
        assert_eq!(
            x264.detail,
            "ffmpeg has no libx264 encoder: MP4 exports use libopenh264"
        );
        assert!(x264.fix.is_some(), "{x264:?}");
        let vaapi = check(&report, "vaapi").fix.clone().unwrap();
        assert!(
            vaapi
                .note
                .as_deref()
                .is_some_and(|n| n.ends_with("exports use libopenh264 meanwhile")),
            "{vaapi:?}"
        );

        // Neither: MP4 export is off, and either one turns it on.
        fake.encoders.remove("libopenh264");
        let report = run_with(&fake, &opts());
        assert_eq!(off(&report), [Feature::ExportMp4]);
        assert_eq!(
            report.features_off[0].checks,
            ["ffmpeg.libx264", "ffmpeg.libopenh264"]
        );
        assert_eq!(
            report.features_off[0].message,
            "MP4 export is off: ffmpeg has no libx264 encoder (MP4 exports), and no h264_vaapi \
             or libopenh264 encodes them instead; ffmpeg has no libopenh264 encoder (MP4 \
             exports), and no h264_vaapi or libx264 encodes them instead"
        );
        let openh264 = check(&report, "ffmpeg.libopenh264");
        assert!(openh264.fix.is_some(), "{openh264:?}");
        assert!(
            check(&report, "vaapi")
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("MP4 exports are off meanwhile")),
            "{report:?}"
        );

        // libx264 alone (Debian's, Ubuntu's and Arch's ffmpeg): libopenh264
        // is needed for nothing, and has no fix to offer.
        fake.encoders.insert("libx264".to_owned());
        let report = run_with(&fake, &opts());
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        let openh264 = check(&report, "ffmpeg.libopenh264");
        assert!(
            !openh264.ok && openh264.needed_for.is_empty(),
            "{openh264:?}"
        );
        assert_eq!((&openh264.hint, &openh264.fix), (&None, &None));
        assert_eq!(
            openh264.detail,
            "ffmpeg has no libopenh264 encoder: MP4 exports use libx264"
        );
        fake.encoders.remove("libx264");

        // Unprobed (the GUI's doctor, whose host runs the check): an Intel
        // node and an ffmpeg with h264_vaapi do, as for recordings.
        let unprobed = DoctorOptions {
            probe_vaapi: false,
            ..opts()
        };
        let report = run_with(&fake, &unprobed);
        assert_eq!(report.va.as_ref().unwrap().ffmpeg_h264, None);
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        assert!(check(&report, "ffmpeg.libx264").needed_for.is_empty());
        // An ffmpeg without h264_vaapi doesn't.
        fake.encoders.remove("h264_vaapi");
        assert_eq!(off(&run_with(&fake, &unprobed)), [Feature::ExportMp4]);
        fake.encoders.insert("h264_vaapi".to_owned());
        // Nor does the fake backend, which exports in software: libopenh264
        // would.
        let fake_backend = DoctorOptions {
            fake_backend: true,
            ..unprobed
        };
        assert_eq!(off(&run_with(&fake, &fake_backend)), [Feature::ExportMp4]);
        fake.encoders.insert("libopenh264".to_owned());
        let report = run_with(&fake, &fake_backend);
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        assert_eq!(
            check(&report, "ffmpeg.libopenh264").needed_for,
            [Feature::ExportMp4]
        );
        fake.encoders.remove("libopenh264");
        // Nor a configured NVIDIA node, which the doctor never probes.
        let nvidia = DoctorOptions {
            config_vaapi_device: Some("/dev/dri/renderD128".to_owned()),
            ..opts()
        };
        assert_eq!(off(&run_with(&fake, &nvidia)), [Feature::ExportMp4]);
        let nvidia_unprobed = DoctorOptions {
            probe_vaapi: false,
            ..nvidia
        };
        assert_eq!(
            off(&run_with(&fake, &nvidia_unprobed)),
            [Feature::ExportMp4]
        );
        assert_eq!(fake.vaapi_probed.get(), 5, "probed with probe_vaapi only");
    }

    /// The `vaapi` check without an Intel or AMD node informs: it names
    /// what the machine has (no node, NVIDIA's, a virtual machine's GPU)
    /// and what encodes on the CPU instead, x264 for both, else what each
    /// uses (stock Fedora: OpenH264 for both). Nothing to install or set.
    #[test]
    fn without_a_va_node_the_cpu_encoders_are_named() {
        let mut fake = Fake::healthy();
        fake.nodes = vec![node("/dev/dri/renderD128", PCI_NVIDIA)];
        fake.env.remove("LIBVA_DRIVER_NAME");
        let vaapi = |fake: &Fake| check(&run_with(fake, &opts()), "vaapi").clone();
        let nvidia = "no Intel or AMD GPU (/dev/dri/renderD128: NVIDIA, which has no VA-API \
                      encoder)";
        let c = vaapi(&fake);
        assert!(c.ok && c.hint.is_none(), "{c:?}");
        assert_eq!(
            c.detail,
            format!("{nvidia}: recordings and exports use x264 on the CPU")
        );
        fake.encoders.remove("libx264");
        assert_eq!(
            vaapi(&fake).detail,
            format!("{nvidia}: recordings use x264 and exports OpenH264 on the CPU")
        );
        fake.elements.remove("x264enc");
        assert_eq!(
            vaapi(&fake).detail,
            format!("{nvidia}: recordings and exports use OpenH264 on the CPU")
        );
        fake.encoders.remove("libopenh264");
        assert_eq!(
            vaapi(&fake).detail,
            format!("{nvidia}: recordings use OpenH264 on the CPU")
        );
        fake.elements.remove("openh264enc");
        assert_eq!(
            vaapi(&fake).detail,
            format!("{nvidia}: nothing encodes H.264 on the CPU either")
        );
    }

    /// The VA-API rows (`gst.vah264enc`, `vaapi`) and the text's VA-API
    /// section on the GPUs Qrec meets: the libva driver per node (iHD, i965
    /// by generation or because only it is installed), whether its file is
    /// there, GStreamer recording nothing with i965 (information on the
    /// chips only it drives, iHD's hint on newer ones), Intel's other
    /// driver where only it passes the h264_vaapi check (exports load it),
    /// NVIDIA never probed, a configured node that can't encode, and
    /// machines without an Intel or AMD GPU informing rather than failing.
    #[test]
    fn va_rows_per_gpu() {
        let distro = |os_release: &str| {
            platform(
                &[
                    ("WAYLAND_DISPLAY", "wayland-0"),
                    ("XDG_CURRENT_DESKTOP", "GNOME"),
                ],
                os_release,
            )
        };
        let intel = |id: u16, driver: &str| gpu_node("/dev/dri/renderD128", PCI_INTEL, id, driver);
        // An Intel node of `id` on `os_release`, these driver files, this
        // pinned driver: its VA-API works.
        let machine = |os_release: &str, id: u16, drivers: gpu::VaDrivers, pinned: &'static str| {
            let mut fake = Fake::healthy();
            fake.platform = distro(os_release);
            fake.nodes = vec![intel(id, "i915")];
            fake.drivers = drivers;
            fake.env.insert("LIBVA_DRIVER_NAME", pinned);
            fake.startup.clear();
            fake
        };
        // Its VA-API doesn't: no VA encoder, and the h264_vaapi check fails.
        let broken = |mut fake: Fake| {
            fake.vaapi = Err("ffmpeg exited with status 1".to_owned());
            fake.va_encoders.clear();
            fake.va_factory = Ok(None);
            fake
        };
        // It loads i965: ffmpeg encodes, GStreamer's VA plugin registers no
        // encoder (i965 is blocklisted for encoding there).
        let on_i965 = |mut fake: Fake| {
            fake.va_encoders.clear();
            fake.va_factory = Ok(None);
            fake
        };
        let both = |dir: &str| drivers_in(dir, &["iHD", "i965"]);
        let multiarch = "/usr/lib/x86_64-linux-gnu/dri";
        let debian = "ID=debian\n";
        let fedora = "ID=fedora\nVERSION_ID=44\n";
        let ubuntu = "ID=ubuntu\nID_LIKE=debian\n";
        let unprobed = DoctorOptions {
            probe_vaapi: false,
            ..opts()
        };
        let nvidia_config = DoctorOptions {
            config_vaapi_device: Some("/dev/dri/renderD128".to_owned()),
            ..opts()
        };

        let mut cases: Vec<(&str, Fake, DoctorOptions)> = vec![
            (
                "dev box: Arrow Lake and NVIDIA, NixOS",
                Fake::healthy(),
                opts(),
            ),
            (
                "Haswell with i965, Debian",
                on_i965(machine(debian, 0x0416, both(multiarch), "i965")),
                opts(),
            ),
            (
                "Haswell without i965, Fedora",
                broken(machine(
                    fedora,
                    0x0416,
                    drivers_in("/usr/lib64/dri", &["iHD"]),
                    "i965",
                )),
                opts(),
            ),
            (
                "Tiger Lake without a VA driver, Debian",
                broken(machine(
                    debian,
                    0x9a49,
                    drivers_in("/usr/lib/x86_64-linux-gnu/dri", &[]),
                    "iHD",
                )),
                opts(),
            ),
            (
                "Tiger Lake without a VA driver, the window's doctor (unprobed)",
                broken(machine(
                    debian,
                    0x9a49,
                    drivers_in("/usr/lib/x86_64-linux-gnu/dri", &[]),
                    "iHD",
                )),
                unprobed.clone(),
            ),
            (
                "Cherry View, which iHD doesn't drive, Ubuntu",
                on_i965(machine(ubuntu, 0x22b0, both(multiarch), "i965")),
                opts(),
            ),
            (
                "Kaby Lake whose free iHD encodes nothing, i965 exports, Ubuntu",
                {
                    let mut fake = broken(machine(ubuntu, 0x5916, both(multiarch), "iHD"));
                    fake.vaapi_other = Ok(());
                    fake
                },
                opts(),
            ),
            (
                "Kaby Lake with only i965, Debian",
                on_i965(machine(
                    debian,
                    0x5916,
                    drivers_in(multiarch, &["i965"]),
                    "i965",
                )),
                opts(),
            ),
            (
                "Kaby Lake with the user's LIBVA_DRIVER_NAME=i965, Arch",
                on_i965(machine("ID=arch\n", 0x5916, both("/usr/lib/dri"), "i965")),
                opts(),
            ),
            (
                "NVIDIA configured on the dev box",
                Fake::healthy(),
                nvidia_config,
            ),
            (
                "a configured path that isn't a render node",
                Fake::healthy(),
                DoctorOptions {
                    config_vaapi_device: Some("/nonexistent/renderD999".to_owned()),
                    ..opts()
                },
            ),
        ];
        // Machines without an Intel or AMD GPU.
        for (name, nodes) in [
            (
                "NVIDIA only, Ubuntu",
                vec![gpu_node(
                    "/dev/dri/renderD128",
                    PCI_NVIDIA,
                    0x2db8,
                    "nvidia",
                )],
            ),
            (
                "virtio-gpu, a KVM guest",
                vec![gpu_node(
                    "/dev/dri/renderD128",
                    gpu::VENDOR_VIRTIO,
                    0x0010,
                    "virtio_gpu",
                )],
            ),
            (
                "VMware SVGA, a VMware guest",
                vec![gpu_node(
                    "/dev/dri/renderD128",
                    gpu::VENDOR_VMWARE,
                    0x0405,
                    "vmwgfx",
                )],
            ),
            (
                "QXL, a QEMU guest",
                vec![gpu_node(
                    "/dev/dri/renderD128",
                    gpu::VENDOR_QXL,
                    0x0100,
                    "qxl",
                )],
            ),
            (
                "an Arm board's GPU, not on PCI",
                vec![gpu::RenderNode {
                    path: "/dev/dri/renderD128".to_owned(),
                    vendor: None,
                    device_id: None,
                    driver: Some("panfrost".to_owned()),
                }],
            ),
            ("no GPU render node", Vec::new()),
        ] {
            let mut fake = Fake::healthy();
            fake.platform = distro(ubuntu);
            fake.nodes = nodes;
            fake.drivers = drivers_in(
                "/usr/lib/x86_64-linux-gnu/dri",
                &["iHD", "i965", "radeonsi"],
            );
            fake.va_encoders.clear();
            fake.env.remove("LIBVA_DRIVER_NAME");
            fake.startup.clear();
            cases.push((name, fake, opts()));
        }

        let mut out = String::new();
        for (name, fake, options) in &cases {
            let report = run_with(fake, options);
            let _ = writeln!(out, "=== {name}");
            for id in ["gst.vah264enc", "vaapi"] {
                let c = check(&report, id);
                let status = if c.ok { "ok" } else { "optional" };
                let _ = writeln!(out, "{status} {id}: {}", c.detail);
                if let Some(hint) = &c.hint {
                    let _ = writeln!(out, "  → {hint}");
                }
                assert!(c.needed_for.is_empty() && !c.required, "{name}: {c:?}");
            }
            // The text's VA-API section.
            let text = report.to_text();
            let start = text
                .find("\nVA-API render nodes: ")
                .expect("the VA-API section");
            let section = &text[start + 1..];
            let end = section.find("\n\n").unwrap_or(section.len());
            let _ = writeln!(out, "{}\n", &section[..end]);
            assert!(
                report.features_off.is_empty(),
                "{name}: {:?}",
                report.features_off
            );
            let back: DoctorReport = serde_json::from_value(report.to_json()).unwrap();
            assert_eq!(back, report, "{name}");
        }
        insta::assert_snapshot!("va_rows_per_gpu", out);
    }

    #[test]
    fn broken_gstreamer_and_ffmpeg_stop_their_sub_checks() {
        let mut fake = Fake::healthy();
        fake.gst = Err(sources::GST_STUCK.to_owned());
        fake.programs.remove("ffmpeg");
        let report = run_with(&fake, &opts());
        assert!(!check(&report, "gstreamer").ok);
        assert!(
            report.va.as_ref().unwrap().gst_encoders.is_empty(),
            "a stuck GStreamer isn't asked again"
        );
        assert_eq!(fake.factory_probed.get(), 0);
        assert!(
            check(&report, "gstreamer")
                .detail
                .contains("stuck scanning plugins")
        );
        assert!(
            report
                .checks
                .iter()
                .all(|c| !c.id.starts_with("gst.") && c.id != "ges")
        );
        assert!(!check(&report, "ffmpeg").ok);
        assert!(check(&report, "ffprobe").ok);
        assert!(report.checks.iter().all(|c| !c.id.starts_with("ffmpeg.")));
        assert_eq!(
            report.missing_dependency().unwrap().message,
            "missing: gstreamer, ffmpeg"
        );

        // A GStreamer older than 1.24 is an app failure.
        let mut fake = Fake::healthy();
        fake.gst = Ok("GStreamer 1.22.12".to_owned());
        let report = run_with(&fake, &opts());
        let c = check(&report, "gstreamer");
        assert!(!c.ok && c.required, "{c:?}");
        assert_eq!(
            c.detail,
            "GStreamer 1.22.12 is too old: Qrec needs GStreamer 1.24 or newer"
        );
        fake.gst = Ok("GStreamer 1.27.0.1 (GIT)".to_owned());
        assert!(check(&run_with(&fake, &opts()), "gstreamer").ok);
    }

    #[test]
    fn the_fake_backend_needs_test_sources_instead_of_devices() {
        let mut fake = Fake::healthy();
        fake.elements.remove("pipewiresrc");
        fake.sockets.clear();
        fake.portal = Err("no session bus".to_owned());
        fake.programs.remove("pactl");
        let report = run_with(
            &fake,
            &DoctorOptions {
                fake_backend: true,
                ..opts()
            },
        );
        assert!(report.ok(), "{}", report.to_text());
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        for id in [
            "gst.pipewiresrc",
            "gst.v4l2src",
            "pipewire.daemon",
            "portal.screencast",
            "pactl",
        ] {
            assert!(check(&report, id).needed_for.is_empty(), "{id}");
        }
        // The session's screen checks only inform there, whichever backend
        // the session has: an X server out of reach, no display at all.
        for (vars, id, element) in [
            (&[("DISPLAY", ":0")][..], "x11.randr", Some("gst.ximagesrc")),
            (&[][..], "display", None),
        ] {
            let mut session = fake.clone().on(vars, "ID=debian\n");
            session.x11 = Err("X11 (DISPLAY=:0): can't connect: no X server listens there".into());
            session.elements.remove("ximagesrc");
            let report = run_with(
                &session,
                &DoctorOptions {
                    fake_backend: true,
                    ..opts()
                },
            );
            assert!(report.features_off.is_empty(), "{:?}", report.features_off);
            let c = check(&report, id);
            assert!(!c.ok && c.needed_for.is_empty(), "{c:?}");
            match element {
                Some(element) => {
                    let e = check(&report, element);
                    assert!(!e.ok && e.needed_for.is_empty(), "{e:?}");
                }
                None => assert!(
                    report
                        .checks
                        .iter()
                        .all(|c| c.id != "gst.pipewiresrc" && c.id != "gst.ximagesrc"),
                    "no element records screens without a display: {:?}",
                    report.checks
                ),
            }
        }
        assert_eq!(
            check(&report, "gst.videotestsrc").needed_for,
            [Feature::Screens, Feature::Cameras]
        );
        assert_eq!(
            check(&report, "gst.audiotestsrc").needed_for,
            [Feature::Audio]
        );
        assert_eq!(
            check(&report, "gst.x264enc").needed_for,
            [Feature::Recording]
        );
        assert!(report.checks.iter().all(|c| c.id != "sound.server"));
        assert_eq!(
            fake.factory_probed.get(),
            0,
            "no render node under the fake backend"
        );

        fake.elements.remove("videotestsrc");
        let report = run_with(
            &fake,
            &DoctorOptions {
                fake_backend: true,
                ..opts()
            },
        );
        assert_eq!(report.exit_code(), 0);
        assert_eq!(off(&report), [Feature::Screens, Feature::Cameras]);
    }

    /// The screen checks follow the session's screen backend: an X server
    /// Qrec can't reach turns screen capture off with discovery's warning,
    /// an old RandR records its whole screen (which only informs), no
    /// display at all turns it off; PipeWire's socket is looked for where
    /// its clients look; `QREC_SCREEN_BACKEND=portal` brings the portal's
    /// checks to an X11 session.
    #[test]
    fn the_screen_checks_follow_the_screen_backend() {
        let x11 = Fake::healthy().on(&[("DISPLAY", ":0")], "ID=arch\n");
        let report = run_with(&x11, &opts());
        assert_eq!(report.session, "X11 · screens: x11 (DISPLAY=:0)");
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);

        // Nothing listens on the display: discovery's words, screens off.
        let mut gone = x11.clone();
        gone.x11 = Err("X11 (DISPLAY=:0): can't connect: no X server listens there".to_owned());
        let report = run_with(&gone, &opts());
        let c = check(&report, "x11.randr");
        assert_eq!(
            (c.ok, &c.needed_for, c.detail.as_str()),
            (
                false,
                &vec![Feature::Screens],
                "X11 (DISPLAY=:0): can't connect: no X server listens there"
            )
        );
        assert!(
            c.hint
                .as_deref()
                .is_some_and(|h| h.starts_with("check that DISPLAY=:0 names a running X server")),
            "{c:?}"
        );
        assert_eq!(
            report.features_off[0].message,
            "Screen capture is off: X11 (DISPLAY=:0): can't connect: no X server listens there"
        );
        // ximagesrc missing: off too, with the good plugins' fix.
        let mut no_ximagesrc = x11.clone();
        no_ximagesrc.elements.remove("ximagesrc");
        let report = run_with(&no_ximagesrc, &opts());
        assert_eq!(off(&report), [Feature::Screens]);
        assert_eq!(
            check(&report, "gst.ximagesrc").hint.as_deref(),
            Some("sudo pacman -S gst-plugins-good")
        );

        // RandR 1.3 and 1.4 list the lit outputs; before that, the whole
        // screen is one, which only informs.
        let mut old = x11.clone();
        old.x11 = Ok(x_server(Some((1, 4)), &[("LVDS-1", 1366, 768)]));
        assert_eq!(
            check(&run_with(&old, &opts()), "x11.randr").detail,
            "RandR 1.4: 1 screen (LVDS-1)"
        );
        old.x11 = Ok(x_server(Some((1, 2)), &[("screen", 1024, 768)]));
        let report = run_with(&old, &opts());
        let c = check(&report, "x11.randr");
        assert!(!c.ok && c.needed_for.is_empty(), "{c:?}");
        assert_eq!(
            c.detail,
            "RandR 1.2: Qrec records its whole screen (1024x768) as one; it reads the \
             outputs from RandR 1.3 on"
        );
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);

        // No display: nothing records screens.
        let tty = Fake::healthy().on(&[("XDG_SESSION_TYPE", "tty")], "ID=debian\n");
        let report = run_with(&tty, &opts());
        assert_eq!(report.screen_backend, ScreenBackendKind::None);
        assert_eq!(report.session, "no display · screens: none");
        assert_eq!(
            report.features_off[0].message,
            "Screen capture is off: no Wayland (WAYLAND_DISPLAY) or X11 (DISPLAY) display"
        );
        assert_eq!(report.features_off[0].checks, ["display"]);
        for id in [
            "gst.pipewiresrc",
            "gst.ximagesrc",
            "portal.screencast",
            "x11.randr",
        ] {
            assert!(report.checks.iter().all(|c| c.id != id), "{id}");
        }
        assert_eq!((tty.portal_asked.get(), tty.x11_asked.get()), (0, 0));

        // PipeWire's own runtime dir wins over XDG_RUNTIME_DIR; without
        // either there is nowhere to look.
        let mut moved = Fake::healthy();
        moved.env.insert("PIPEWIRE_RUNTIME_DIR", "/run/pipewire");
        assert_eq!(off(&run_with(&moved, &opts())), [Feature::Screens]);
        moved
            .sockets
            .insert(PathBuf::from("/run/pipewire/pipewire-0"));
        assert_eq!(
            check(&run_with(&moved, &opts()), "pipewire.daemon").detail,
            "/run/pipewire/pipewire-0 (the portal's screen casts)"
        );
        moved.env.remove("PIPEWIRE_RUNTIME_DIR");
        moved.env.remove("XDG_RUNTIME_DIR");
        let report = run_with(&moved, &opts());
        let c = check(&report, "pipewire.daemon");
        assert_eq!(
            (c.ok, c.detail.as_str()),
            (
                false,
                "no PipeWire daemon to find: neither PIPEWIRE_RUNTIME_DIR nor XDG_RUNTIME_DIR \
                 is set"
            )
        );

        // The override: the portal's checks in an X11 session.
        let forced = Fake::healthy().on(
            &[("DISPLAY", ":0"), ("QREC_SCREEN_BACKEND", "portal")],
            "ID=arch\n",
        );
        let report = run_with(&forced, &opts());
        assert_eq!(report.session, "X11 · screens: portal");
        assert!(check(&report, "portal.screencast").ok);
        assert!(report.checks.iter().all(|c| c.id != "x11.randr"));
    }

    #[test]
    fn audio_servers_and_portal_versions() {
        let mut fake = Fake::healthy();
        fake.pulse = Ok("pulseaudio".to_owned());
        fake.portal = Ok(PortalInfo {
            screencast_version: 3,
            source_types: Some(1),
            registry_version: None,
        });
        let report = run_with(&fake, &opts());
        assert!(report.features_off.is_empty(), "{:?}", report.features_off);
        let server = check(&report, "sound.server");
        assert!(server.ok && server.detail == "pulseaudio", "{server:?}");
        assert_eq!(
            check(&report, "pactl").detail,
            "pactl 17.0 (/nix/bin/pactl)"
        );
        assert_eq!(
            check(&report, "portal.screencast").detail,
            "xdg-desktop-portal ScreenCast v3 (monitors): the screen picker shows on every \
             take (v4 remembers the choice)"
        );
        assert!(check(&report, "portal.screencast").ok);
        assert!(!check(&report, "portal.registry").ok);
        assert!(check(&report, "portal.registry").needed_for.is_empty());

        fake.portal = Ok(PortalInfo {
            screencast_version: 5,
            source_types: Some(2),
            registry_version: Some(1),
        });
        fake.pulse = Err("Connection refused".to_owned());
        let report = run_with(&fake, &opts());
        assert!(
            check(&report, "portal.screencast")
                .detail
                .contains("can't capture monitors")
        );
        assert!(
            check(&report, "pactl")
                .detail
                .contains("can't reach the sound server")
        );
        assert!(report.checks.iter().all(|c| c.id != "sound.server"));
        assert_eq!(off(&report), [Feature::Screens, Feature::Audio]);

        fake.portal = Ok(PortalInfo {
            screencast_version: 2,
            source_types: None,
            registry_version: None,
        });
        fake.pactl = Ok("pactl 15.0".to_owned());
        let report = run_with(&fake, &opts());
        assert!(
            check(&report, "portal.screencast")
                .detail
                .starts_with("ScreenCast v2 is too old")
        );
        assert_eq!(
            check(&report, "pactl").detail,
            "pactl 15.0 is too old: Qrec needs pactl 16 or newer (/nix/bin/pactl)"
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn va_api_never_touches_nvidia() {
        // Only NVIDIA: nothing is picked automatically, and that is no
        // failure (NVIDIA has no VA-API encoder).
        let mut fake = Fake::healthy();
        fake.nodes = vec![node("/dev/dri/renderD128", PCI_NVIDIA)];
        fake.env.remove("LIBVA_DRIVER_NAME");
        let report = run_with(&fake, &opts());
        let va = report.va.as_ref().unwrap();
        assert_eq!((va.device.as_deref(), va.ffmpeg_h264), (None, None));
        assert_eq!(va.expected_driver, None);
        assert!(check(&report, "vaapi").ok && report.ok());
        assert_eq!((fake.vaapi_probed.get(), fake.factory_probed.get()), (0, 0));
        // x264enc records.
        assert_eq!(
            check(&report, "gst.x264enc").needed_for,
            [Feature::Recording]
        );

        // Configured explicitly: reported, still never probed, and the hint
        // names the configuration that picks it.
        let report = run_with(
            &fake,
            &DoctorOptions {
                config_vaapi_device: Some("/dev/dri/renderD128".to_owned()),
                ..opts()
            },
        );
        let va = report.va.as_ref().unwrap();
        assert_eq!(va.origin.as_deref(), Some("config"));
        assert_eq!((va.ffmpeg_h264, va.gst_factory.as_deref()), (None, None));
        assert!(
            va.note
                .as_deref()
                .unwrap()
                .contains("isn't probed: NVIDIA has no VA-API encoder"),
            "{va:?}"
        );
        assert_eq!((fake.vaapi_probed.get(), fake.factory_probed.get()), (0, 0));
        let c = check(&report, "vaapi");
        assert!(!c.ok && c.needed_for.is_empty(), "{c:?}");
        assert_eq!(
            c.hint.as_deref(),
            Some(
                "NVIDIA has no VA-API encoder: set encoder.vaapi_device in \
                 /home/me/.config/qrec/config.toml to an Intel or AMD render node, or remove it \
                 (Qrec then takes the first one)"
            )
        );
        assert_eq!(
            check(&report, "gst.vah264enc").detail,
            "/dev/dri/renderD128 is NVIDIA's, which has no VA-API encoder: recordings use x264 \
             on the CPU"
        );
        // The project's: its recording.vaapiDevice.
        let report = run_with(
            &fake,
            &DoctorOptions {
                project_vaapi_device: Some("/dev/dri/renderD128".to_owned()),
                ..opts()
            },
        );
        assert!(
            check(&report, "vaapi")
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("set the project's recording.vaapiDevice")),
            "{report:?}"
        );

        // The project's device wins; a mismatching LIBVA_DRIVER_NAME is reported.
        let mut fake = Fake::healthy();
        fake.env.insert("LIBVA_DRIVER_NAME", "nvidia");
        fake.va_factory = Ok(None);
        let report = run_with(
            &fake,
            &DoctorOptions {
                project_vaapi_device: Some("/dev/dri/renderD129".to_owned()),
                config_vaapi_device: Some("/dev/dri/renderD128".to_owned()),
                ..opts()
            },
        );
        let c = check(&report, "vaapi");
        assert!(!c.ok, "{c:?}");
        assert!(c.detail.contains("needs iHD"), "{c:?}");
        assert_eq!(
            report.va.as_ref().unwrap().origin.as_deref(),
            Some("project")
        );

        // A failing VA encode is a note and an informative failure.
        let mut fake = Fake::healthy();
        fake.vaapi = Err("ffmpeg exited with status 1".to_owned());
        let report = run_with(&fake, &opts());
        assert_eq!(report.va.as_ref().unwrap().ffmpeg_h264, Some(false));
        assert!(!check(&report, "vaapi").ok && report.ok());
        assert!(report.features_off.is_empty());

        // Not probing at all.
        let fake = Fake::healthy();
        let report = run_with(
            &fake,
            &DoctorOptions {
                probe_vaapi: false,
                ..opts()
            },
        );
        let va = report.va.as_ref().unwrap();
        assert_eq!((va.ffmpeg_h264, va.gst_factory.as_deref()), (None, None));
        assert_eq!((fake.vaapi_probed.get(), fake.factory_probed.get()), (0, 0));
        assert!(check(&report, "vaapi").ok);
    }

    #[test]
    fn parsers_and_scans() {
        let encoders = parse_encoders(
            "Encoders:\n V..... = Video\n ------\n V....D libx264              libx264 H.264\n \
             A....D aac                  AAC (Advanced Audio Coding)\n",
        );
        assert_eq!(encoders, names(&["aac", "libx264"]));
        let decoders = parse_decoders(
            "Decoders:\n V..... = Video\n ------\n VFS..D h264                 H.264 / AVC / \
             MPEG-4 AVC / MPEG-4 part 10\n V....D libopenh264          OpenH264 H.264 / AVC / \
             MPEG-4 AVC / MPEG-4 part 10 (codec h264)\n V..... h264_cuvid           Nvidia CUVID \
             H264 decoder (codec h264)\n AF...D flac                 FLAC (Free Lossless Audio \
             Codec)\n",
        );
        let pairs: Vec<(&str, &str)> = decoders
            .iter()
            .map(|(n, c)| (n.as_str(), c.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("flac", "flac"),
                ("h264", "h264"),
                ("h264_cuvid", "h264"),
                ("libopenh264", "h264")
            ]
        );
        // Fedora's fixes: the failed check carries RPM Fusion's ffmpeg.
        let fedora_platform = platform(&[], "ID=fedora\n");
        let fedora_fix = Fix(&fedora_platform);
        assert_eq!(
            decoders_check(&decoders, fedora_fix).detail,
            "h264 (the takes' video), flac (the takes' audio)"
        );
        // Fedora's ffmpeg-free: only OpenH264 decodes H.264; a hardware
        // decoder alone doesn't count.
        let fedora: BTreeMap<String, String> = decoders
            .iter()
            .filter(|(n, _)| *n != "h264")
            .map(|(n, c)| (n.clone(), c.clone()))
            .collect();
        assert_eq!(
            decoders_check(&fedora, fedora_fix).detail,
            "libopenh264 for h264 (the takes' video), flac (the takes' audio)"
        );
        let cuda: BTreeMap<String, String> = decoders
            .iter()
            .filter(|(n, _)| *n == "h264_cuvid" || *n == "flac")
            .map(|(n, c)| (n.clone(), c.clone()))
            .collect();
        let c = decoders_check(&cuda, fedora_fix);
        assert!(!c.ok && c.required, "{c:?}");
        assert_eq!(c.detail, "ffmpeg can't decode h264 (the takes' video)");
        assert_eq!(
            c.fix.as_ref().and_then(|f| f.command.as_deref()),
            Some("sudo dnf swap ffmpeg-free ffmpeg --allowerasing")
        );
        assert_eq!(c.hint.as_deref(), c.fix.as_ref().map(Hint::line).as_deref());
        // `---` (ffmpeg 7+) or `--` (older); a device's flags hold a space.
        let demuxers = parse_demuxers(
            "Formats:\n D.. = Demuxing supported\n ..d = Is a device\n ---\n D   3dostr          \
             3DO STR\n D d alsa            ALSA audio input\n D   matroska,webm   Matroska / \
             WebM\n",
        );
        assert_eq!(demuxers, names(&["3dostr", "alsa", "matroska", "webm"]));
        assert_eq!(
            parse_demuxers(" D. = Demuxing\n --\n D  matroska,webm   Matroska / WebM\n"),
            names(&["matroska", "webm"])
        );
        assert!(parse_demuxers("no list\n").is_empty());
        assert_eq!(parse_version("GStreamer 1.26.11"), Some((1, 26)));
        assert_eq!(parse_version("pactl 16.1"), Some((16, 1)));
        assert_eq!(parse_version("pactl 17"), Some((17, 0)));
        assert_eq!(parse_version("pactl"), None);
        assert_eq!(parse_dbus_u32("u 5\n"), Some(5));
        assert_eq!(parse_dbus_u32("(<uint32 7>,)\n"), Some(7));
        assert_eq!(parse_dbus_u32("nothing"), None);

        let report_node = RenderNode::from(&node("/dev/dri/renderD129", PCI_INTEL));
        assert_eq!(report_node.vendor_name.as_deref(), Some("Intel"));
        assert!(driver_fits("i965", PCI_INTEL) && driver_fits("radeonsi", PCI_AMD));
        assert!(!driver_fits("nvidia", PCI_INTEL) && !driver_fits("iHD", PCI_NVIDIA));

        let argv = vaapi_check_argv("/dev/dri/renderD129");
        assert!(argv.contains(&"vaapi=va:/dev/dri/renderD129".to_owned()));
        assert_eq!(&argv[argv.len() - 3..], ["-f", "null", "-"]);
    }

    /// The machines of issue #9 as fakes, each with what `qrec doctor`
    /// would print and its `--json` report.
    fn machines() -> Vec<(&'static str, Fake)> {
        let gnome = |fake: Fake, os_release: &str| {
            fake.on(
                &[
                    ("WAYLAND_DISPLAY", "wayland-0"),
                    ("XDG_SESSION_TYPE", "wayland"),
                    ("XDG_CURRENT_DESKTOP", "GNOME"),
                ],
                os_release,
            )
        };
        // Debian's, Ubuntu's and Arch's ffmpeg: libx264, no libopenh264.
        let distro_ffmpeg = |mut fake: Fake| {
            fake.encoders.remove("libopenh264");
            fake
        };
        // A session bus without xdg-desktop-portal.
        let no_portal = || {
            Err("busctl exited with status 1: Call failed: The name \
                 org.freedesktop.portal.Desktop was not provided by any .service files"
                .to_owned())
        };
        // Debian's libva drivers (intel-media-va-driver).
        let debian_drivers = || drivers_in("/usr/lib/x86_64-linux-gnu/dri", &["iHD"]);

        // PulseAudio itself (PipeWire only for the portal's screen casts):
        // Debian with GNOME.
        let mut pulseaudio_16 = gnome(distro_ffmpeg(Fake::healthy().intel_only()), "ID=debian\n");
        pulseaudio_16.drivers = debian_drivers();
        pulseaudio_16.pactl = Ok("pactl 16.1".to_owned());
        pulseaudio_16.pulse = Ok("pulseaudio".to_owned());
        let mut pulseaudio_15 = pulseaudio_16.clone();
        pulseaudio_15.pactl = Ok("pactl 15.0".to_owned());

        // Xfce on Xorg: no portal on the session bus, which X11 doesn't
        // need: ximagesrc records the X server's two monitors.
        let mut xfce = distro_ffmpeg(Fake::healthy().intel_only()).on(
            &[
                ("DISPLAY", ":0"),
                ("XDG_SESSION_TYPE", "x11"),
                ("XDG_CURRENT_DESKTOP", "XFCE"),
            ],
            "ID=debian\n",
        );
        xfce.portal = no_portal();
        xfce.drivers = debian_drivers();
        xfce.pulse = Ok("PulseAudio (on PipeWire 1.4.2)".to_owned());

        // KDE Plasma on X11 (Kubuntu): its portal registers ScreenCast and
        // can't capture there; the X server records.
        let kde_x11 = distro_ffmpeg(Fake::healthy().intel_only()).on(
            &[
                ("DISPLAY", ":0"),
                ("XDG_SESSION_TYPE", "x11"),
                ("XDG_CURRENT_DESKTOP", "KDE"),
            ],
            "ID=ubuntu\nID_LIKE=debian\n",
        );

        // Ubuntu's "GNOME on Xorg": X11 too, the portal running.
        let mut gnome_xorg = distro_ffmpeg(Fake::healthy().intel_only()).on(
            &[
                ("DISPLAY", ":1"),
                ("XDG_SESSION_TYPE", "x11"),
                ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
            ],
            "ID=ubuntu\nID_LIKE=debian\n",
        );
        gnome_xorg.x11 = Ok(x_server(Some((1, 6)), &[("eDP-1", 2880, 1800)]));

        // A bare Xvfb started without RandR (a CI container): its whole
        // screen is one screen; no portal, no session bus.
        let mut xvfb = distro_ffmpeg(Fake::healthy().intel_only())
            .on(&[("DISPLAY", ":99")], "ID=debian\nVERSION_ID=\"13\"\n");
        xvfb.x11 = Ok(x_server(None, &[("screen", 1280, 1024)]));
        xvfb.portal = Err("busctl exited with status 1: Failed to connect to bus".to_owned());

        // Hyprland whose PipeWire daemon isn't running (PulseAudio plays
        // the sound): the portal answers and can't cast a screen.
        let mut hyprland = distro_ffmpeg(Fake::healthy()).on(
            &[
                ("WAYLAND_DISPLAY", "wayland-1"),
                ("HYPRLAND_INSTANCE_SIGNATURE", "v0.50_1700000000_1234"),
                ("XDG_CURRENT_DESKTOP", "Hyprland"),
            ],
            "ID=arch\n",
        );
        hyprland.sockets.clear();
        hyprland.pulse = Ok("pulseaudio".to_owned());

        // Stock Fedora: no x264enc (plugins-ugly-free), ffmpeg-free (no
        // libx264, OpenH264 decodes), VA on Intel, which records and
        // encodes the MP4 exports.
        let mut fedora = gnome(Fake::healthy().intel_only(), "ID=fedora\nVERSION_ID=44\n");
        fedora.drivers = drivers_in("/usr/lib64/dri", &["iHD", "radeonsi"]);
        fedora.elements.remove("x264enc");
        fedora.h264_decoders = vec!["openh264dec".to_owned()];
        fedora.encoders.remove("libx264");
        fedora.encoders.insert("libopenh264".to_owned());
        fedora.decoders.remove("h264");
        fedora
            .decoders
            .insert("libopenh264".to_owned(), "h264".to_owned());

        // Stock Fedora on AMD: Fedora's Mesa encodes no H.264, so GStreamer
        // has no VA encoder and ffmpeg's h264_vaapi check fails; with no
        // x264enc either, openh264enc records, and with no libx264,
        // libopenh264 exports MP4.
        let mut fedora_amd = fedora.clone();
        fedora_amd.nodes = vec![gpu_node("/dev/dri/renderD128", PCI_AMD, 0x73df, "amdgpu")];
        fedora_amd.env.insert("LIBVA_DRIVER_NAME", "radeonsi");
        fedora_amd.startup.clear();
        fedora_amd.va_encoders.clear();
        fedora_amd.va_factory = Ok(None);
        fedora_amd.vaapi = Err("ffmpeg exited with status 1".to_owned());

        // sway without xdg-desktop-portal-wlr: the frontend runs, nothing
        // answers for ScreenCast. AMD graphics.
        let mut sway = distro_ffmpeg(Fake::healthy()).on(
            &[
                ("WAYLAND_DISPLAY", "wayland-1"),
                ("SWAYSOCK", "/run/user/1000/sway-ipc.1000.1234.sock"),
                ("XDG_CURRENT_DESKTOP", "sway"),
            ],
            "ID=arch\n",
        );
        sway.portal = Err(
            "busctl exited with status 1: Failed to get property version on interface \
             org.freedesktop.portal.ScreenCast: No such interface"
                .to_owned(),
        );
        sway.nodes = vec![gpu_node("/dev/dri/renderD128", PCI_AMD, 0x73df, "amdgpu")];
        sway.drivers = drivers_in("/usr/lib/dri", &["radeonsi"]);
        sway.env.insert("LIBVA_DRIVER_NAME", "radeonsi");
        sway.va_factory = Ok(Some("vah264enc".to_owned()));
        sway.startup.clear();

        // NVIDIA only: no VA node Qrec would pick; x264 records.
        let mut nvidia = gnome(
            distro_ffmpeg(Fake::healthy()),
            "ID=ubuntu\nID_LIKE=debian\n",
        );
        nvidia.nodes = vec![gpu_node(
            "/dev/dri/renderD128",
            PCI_NVIDIA,
            0x2db8,
            "nvidia",
        )];
        nvidia.drivers = drivers_in(
            "/usr/lib/x86_64-linux-gnu/dri",
            &["iHD", "i965", "radeonsi"],
        );
        nvidia.va_encoders.clear();
        nvidia.env.remove("LIBVA_DRIVER_NAME");
        nvidia.startup.clear();

        // A VM: virtio-gpu, no VA at all.
        let mut vm = Fake::healthy().on(
            &[
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "KDE"),
            ],
            "ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n",
        );
        vm.nodes = vec![gpu_node(
            "/dev/dri/renderD128",
            gpu::VENDOR_VIRTIO,
            0x0010,
            "virtio_gpu",
        )];
        vm.drivers = drivers_in("/usr/lib64/dri", &[]);
        vm.va_encoders.clear();
        vm.env.remove("LIBVA_DRIVER_NAME");
        vm.startup.clear();

        // Debian without gst-libav: no software H.264 decoder (app).
        let mut debian = gnome(
            distro_ffmpeg(Fake::healthy().intel_only()),
            "ID=debian\nVERSION_ID=\"13\"\n",
        );
        debian.drivers = debian_drivers();
        debian.h264_decoders.clear();

        // Debian's render nodes belong to the render group, and a session
        // logind gives no seat (ssh, a service) can't open them without it.
        let mut node_denied = distro_ffmpeg(Fake::healthy().intel_only()).on(
            &[
                ("DISPLAY", ":0"),
                ("XDG_SESSION_TYPE", "x11"),
                ("XDG_CURRENT_DESKTOP", "XFCE"),
            ],
            "ID=debian\nVERSION_ID=\"13\"\n",
        );
        node_denied.drivers = debian_drivers();
        node_denied.portal = no_portal();
        node_denied.va_encoders.clear();
        node_denied.node_errno = Some(13);

        // The AppImage (issue #9): everything Qrec runs comes from it,
        // GStreamer 1.28, Arch's ffmpeg (no libopenh264), pactl 17 and the
        // VA-API drivers, in its FUSE mount or the folder it extracted
        // itself to.
        let from_appimage = |mut fake: Fake, appdir: &'static str, path: &str| {
            fake.platform.app_image = Some(AppImage {
                path: path.to_owned(),
                dir: appdir.to_owned(),
            });
            fake.gst = Ok("GStreamer 1.28.8".to_owned());
            fake.programs = ["ffmpeg", "ffprobe", "pactl"]
                .map(|name| (name, format!("{appdir}/bin/{name}")))
                .into();
            fake.drivers = drivers_in(
                &format!("{appdir}/lib/dri"),
                &["iHD", "i965", "radeonsi", "r600"],
            );
            fake.pactl = Ok("pactl 17.0".to_owned());
            distro_ffmpeg(fake)
        };
        let ubuntu_24_04 = "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n";

        // On Ubuntu 24.04's GNOME in a virtual machine without 3D: no render
        // node at all, so x264 records; its xdg-desktop-portal (1.18) has no
        // host app registry, a host's part.
        let mut appimage_vm = from_appimage(
            gnome(Fake::healthy(), ubuntu_24_04),
            "/tmp/.mount_QrecAbC123",
            "/home/me/Applications/Qrec-0.1.0-x86_64.AppImage",
        );
        appimage_vm.nodes.clear();
        appimage_vm.va_encoders.clear();
        appimage_vm.env.remove("LIBVA_DRIVER_NAME");
        appimage_vm.startup = vec![
            "no Intel or AMD render node: VA-API stays off, LIBVA_DRIVER_NAME untouched".to_owned(),
        ];
        appimage_vm.portal = Ok(PortalInfo {
            screencast_version: 5,
            source_types: Some(7),
            registry_version: None,
        });
        appimage_vm.pulse = Ok("PulseAudio (on PipeWire 1.0.5)".to_owned());

        // In an Ubuntu 24.04 container that didn't get the GPU (docker run
        // without --device), as packaging/appimage/test.sh runs it: sysfs
        // lists the host's Intel and NVIDIA nodes, /dev/dri has neither;
        // no FUSE, so it extracted itself; no display, no sound server.
        let mut appimage_container = from_appimage(
            Fake::healthy().on(&[], ubuntu_24_04),
            "/tmp/appimage_extracted_qrec6877858091878431995",
            "/opt/qrec.AppImage",
        );
        appimage_container.nodes = vec![
            gpu_node("/dev/dri/renderD128", PCI_INTEL, 0x7d51, "i915"),
            gpu_node("/dev/dri/renderD129", PCI_NVIDIA, 0x2db8, "nvidia"),
        ];
        appimage_container.node_errno = Some(2);
        appimage_container.va_encoders.clear();
        appimage_container.env = [
            ("LIBVA_DRIVER_NAME", "iHD"),
            ("XDG_RUNTIME_DIR", "/tmp/runtime"),
        ]
        .into();
        appimage_container.startup = vec!["set LIBVA_DRIVER_NAME=iHD".to_owned()];
        appimage_container.sockets.clear();
        appimage_container.pulse = Err(
            "pactl exited with status 1: pa_context_connect() failed: Connection refused"
                .to_owned(),
        );

        vec![
            ("dev_box", Fake::healthy()),
            ("pulseaudio_15", pulseaudio_15),
            ("pulseaudio_16", pulseaudio_16),
            ("xfce_x11_no_portal", xfce),
            ("kde_x11_with_portal", kde_x11),
            ("gnome_xorg", gnome_xorg),
            ("xvfb_without_randr", xvfb),
            ("wayland_without_pipewire", hyprland),
            ("fedora_stock", fedora),
            ("fedora_stock_amd", fedora_amd),
            ("sway_without_xdpw", sway),
            ("nvidia_only", nvidia),
            ("vm", vm),
            ("debian_without_gst_libav", debian),
            ("debian_node_denied", node_denied),
            ("ubuntu_appimage_without_gpu", appimage_vm),
            ("ubuntu_appimage_node_missing", appimage_container),
        ]
    }

    #[test]
    fn machines_of_issue_9() {
        let mut verdicts = Vec::new();
        for (name, fake) in machines() {
            let report = run_with(&fake, &opts());
            let text = report.to_text();
            verdicts.push((name, report.exit_code(), off(&report)));
            insta::assert_json_snapshot!(
                format!("machine_{name}"),
                serde_json::json!({
                    "exitCode": report.exit_code(),
                    "text": text.lines().collect::<Vec<_>>(),
                    "report": report.to_json(),
                })
            );
        }
        use Feature::*;
        assert_eq!(
            verdicts,
            [
                ("dev_box", 0, vec![]),
                ("pulseaudio_15", 0, vec![Audio]),
                ("pulseaudio_16", 0, vec![]),
                ("xfce_x11_no_portal", 0, vec![]),
                ("kde_x11_with_portal", 0, vec![]),
                ("gnome_xorg", 0, vec![]),
                ("xvfb_without_randr", 0, vec![]),
                ("wayland_without_pipewire", 0, vec![Screens]),
                ("fedora_stock", 0, vec![]),
                ("fedora_stock_amd", 0, vec![]),
                ("sway_without_xdpw", 0, vec![Screens]),
                ("nvidia_only", 0, vec![]),
                ("vm", 0, vec![]),
                ("debian_without_gst_libav", 5, vec![]),
                ("debian_node_denied", 0, vec![]),
                ("ubuntu_appimage_without_gpu", 0, vec![]),
                ("ubuntu_appimage_node_missing", 0, vec![Screens, Audio]),
            ]
        );
        let machine = |wanted: &str| {
            let (_, fake) = machines()
                .into_iter()
                .find(|(name, _)| *name == wanted)
                .unwrap();
            let report = run_with(&fake, &opts());
            (fake, report)
        };
        // On X11 the X server records the screens: ximagesrc and the
        // server, never the portal (KDE's would answer, and can't capture
        // there), nor PipeWire's socket.
        for name in [
            "xfce_x11_no_portal",
            "kde_x11_with_portal",
            "gnome_xorg",
            "xvfb_without_randr",
        ] {
            let (fake, report) = machine(name);
            assert_eq!(report.screen_backend, ScreenBackendKind::X11, "{name}");
            assert_eq!((fake.portal_asked.get(), fake.x11_asked.get()), (0, 1));
            assert_eq!(check(&report, "gst.ximagesrc").needed_for, [Screens]);
            for c in &report.checks {
                assert!(
                    !c.id.starts_with("portal.")
                        && !c.id.starts_with("pipewire")
                        && c.id != "gst.pipewiresrc",
                    "{name}: {c:?}"
                );
            }
        }
        let (_, kde) = machine("kde_x11_with_portal");
        assert_eq!(kde.session, "X11 (kde) · screens: x11 (DISPLAY=:0)");
        assert_eq!(
            check(&kde, "x11.randr").detail,
            "RandR 1.6: 2 screens (eDP-1, HDMI-1)"
        );
        // Without RandR its whole screen is one: that only informs.
        let (_, xvfb) = machine("xvfb_without_randr");
        assert_eq!(xvfb.session, "X11 · screens: x11 (DISPLAY=:99)");
        let randr = check(&xvfb, "x11.randr");
        assert!(!randr.ok && randr.needed_for.is_empty(), "{randr:?}");
        assert_eq!(
            randr.detail,
            "the X server has no RandR: Qrec records its whole screen (1280x1024) as one"
        );
        assert_eq!(
            randr.hint.as_deref(),
            Some(
                "start the X server with its RandR extension (Xvfb: +extension RANDR) to list \
                 each monitor"
            )
        );
        // A Wayland session without PipeWire's socket can't cast a screen.
        let (_, hyprland) = machine("wayland_without_pipewire");
        assert_eq!(
            hyprland.features_off[0].message,
            "Screen capture is off: no PipeWire daemon: nothing at /run/user/1000/pipewire-0"
        );
        assert_eq!(
            check(&hyprland, "pipewire.daemon").hint.as_deref(),
            Some(
                "start the PipeWire daemon (with systemd: systemctl --user enable --now \
                 pipewire.socket): the ScreenCast portal streams the screens through it"
            )
        );
        // Stock Fedora on AMD records with openh264enc and exports MP4 with
        // libopenh264, which is all it needs.
        let (_, report) = machine("fedora_stock_amd");
        assert_eq!(check(&report, "gst.openh264enc").needed_for, [Recording]);
        let x264 = check(&report, "gst.x264enc");
        assert!(x264.needed_for.is_empty(), "{x264:?}");
        assert!(
            x264.detail.ends_with(": recordings use openh264enc"),
            "{x264:?}"
        );
        assert_eq!(check(&report, "ffmpeg.libopenh264").needed_for, [ExportMp4]);
        let x264 = check(&report, "ffmpeg.libx264");
        assert!(x264.needed_for.is_empty(), "{x264:?}");
        assert_eq!(
            x264.detail,
            "ffmpeg has no libx264 encoder: MP4 exports use libopenh264"
        );

        // From an AppImage: its line under the session's, its path and
        // APPDIR in the JSON, its shortcuts (niri's too), and no package to
        // install for what it bundles; the host's parts keep their hints.
        for name in [
            "ubuntu_appimage_without_gpu",
            "ubuntu_appimage_node_missing",
        ] {
            let (fake, report) = machine(name);
            let app_image = fake.platform.app_image.clone().unwrap();
            let text = report.to_text();
            assert!(
                text.starts_with(&format!(
                    "Qrec doctor\n\nSession: {}\nAppImage: {}\n\n",
                    report.session,
                    app_image.describe()
                )),
                "{name}: {text}"
            );
            let json = report.to_json();
            assert_eq!(
                json["platform"]["appImage"]["path"],
                app_image.path.as_str()
            );
            assert_eq!(json["platform"]["appImage"]["dir"], app_image.dir.as_str());
            assert!(report.shortcuts.text.contains(&app_image.path), "{name}");
            assert!(
                report.niri_snippet.contains(&format!(
                    "spawn \"{}\" \"record\" \"toggle\"",
                    app_image.path
                )),
                "{name}: {}",
                report.niri_snippet
            );
            for failed in report.checks.iter().filter(|c| !c.ok) {
                let hint = failed.hint.as_deref().unwrap_or_default();
                let words: Vec<&str> = hint.split(|c: char| !c.is_ascii_alphanumeric()).collect();
                for manager in ["apt", "dnf", "pacman", "zypper", "sudo", "nix"] {
                    assert!(!words.contains(&manager), "{name}: {failed:?}");
                }
                assert!(
                    failed.fix.as_ref().is_none_or(|fix| fix.command.is_none()),
                    "{name}: {failed:?}"
                );
            }
        }
        let (_, vm) = machine("ubuntu_appimage_without_gpu");
        assert_eq!(
            check(&vm, "portal.registry").hint.as_deref(),
            Some("update xdg-desktop-portal to 1.19 or newer")
        );
        // A container that didn't get the GPU: its node is opened once and
        // can't be, so nothing asks it to encode, and both VA-API rows say
        // why instead of naming a driver to install.
        let (fake, report) = machine("ubuntu_appimage_node_missing");
        assert_eq!(
            (
                fake.nodes_opened.get(),
                fake.vaapi_probed.get(),
                fake.factory_probed.get()
            ),
            (1, 0, 0)
        );
        let vaapi = check(&report, "vaapi");
        assert_eq!(
            vaapi.detail,
            "/dev/dri/renderD128 (Intel, auto) · LIBVA_DRIVER_NAME=iHD · can't be opened: No \
             such file or directory (os error 2)"
        );
        assert_eq!(
            vaapi.hint.as_deref(),
            Some(
                "sysfs lists this GPU but there is no /dev/dri/renderD128: give the container or \
                 sandbox Qrec runs in that device (docker run --device /dev/dri/renderD128); \
                 exports use libx264 meanwhile"
            )
        );
        assert_eq!(
            check(&report, "gst.vah264enc").detail,
            "/dev/dri/renderD128 can't be opened: No such file or directory (os error 2): \
             recordings use x264 on the CPU"
        );
        assert_eq!(
            report.va.as_ref().unwrap().device_error,
            Some(DeviceError {
                kind: DeviceErrorKind::Missing,
                message: "No such file or directory (os error 2)".into(),
            })
        );
        // The same everywhere: a host qrec whose user may not open it.
        let (fake, report) = machine("debian_node_denied");
        assert_eq!((fake.nodes_opened.get(), fake.vaapi_probed.get()), (1, 0));
        assert_eq!(
            check(&report, "vaapi").hint.as_deref(),
            Some(
                "let your user open /dev/dri/renderD128: join the group that owns it (ls -l \
                 /dev/dri/renderD128 shows it, usually render), then log in again; exports use \
                 libx264 meanwhile"
            )
        );
        // NVIDIA's node is never opened, nor one under the fake backend.
        assert_eq!(machine("nvidia_only").0.nodes_opened.get(), 0);
        let fake = Fake::healthy();
        run_with(
            &fake,
            &DoctorOptions {
                fake_backend: true,
                ..opts()
            },
        );
        assert_eq!(fake.nodes_opened.get(), 0);
    }

    /// The text's global shortcuts on each desktop, from the variables it
    /// sets: lines for niri's, sway's, i3's, Hyprland's or river's config,
    /// GNOME's or KDE Plasma's settings, else the generic list; the JSON's
    /// `shortcuts` says which (null: the generic list), and `niriSnippet`
    /// stays niri's everywhere.
    #[test]
    fn shortcuts_follow_the_desktop() {
        let wayland = ("WAYLAND_DISPLAY", "wayland-1");
        let x11 = ("DISPLAY", ":0");
        // Each case: its variables and the desktop `shortcuts` is for.
        let desktops = [
            (
                "niri",
                vec![wayland, ("NIRI_SOCKET", "/run/user/1000/niri.sock")],
                Some("niri"),
            ),
            (
                "sway",
                vec![wayland, ("SWAYSOCK", "/run/user/1000/sway-ipc.sock")],
                Some("sway"),
            ),
            (
                "i3",
                vec![x11, ("I3SOCK", "/run/user/1000/i3/ipc-socket.1")],
                Some("i3"),
            ),
            (
                "Hyprland",
                vec![wayland, ("HYPRLAND_INSTANCE_SIGNATURE", "v0.50_1")],
                Some("hyprland"),
            ),
            (
                "river",
                vec![wayland, ("XDG_CURRENT_DESKTOP", "river")],
                Some("river"),
            ),
            (
                "Ubuntu's GNOME",
                vec![wayland, ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME")],
                Some("gnome"),
            ),
            (
                "KDE Plasma",
                vec![wayland, ("XDG_CURRENT_DESKTOP", "KDE")],
                Some("kde"),
            ),
            ("Xfce", vec![x11, ("XDG_CURRENT_DESKTOP", "XFCE")], None),
            ("a tty", vec![], None),
        ];
        let mut out = String::new();
        for (name, vars, desktop) in desktops {
            let mut fake = Fake::healthy();
            fake.platform = platform(&vars, "ID=arch\n");
            let report = run_with(&fake, &opts());
            let text = report.to_text();
            let heading = format!("\n{}\n", report.shortcuts.heading);
            let start = text
                .find(&heading)
                .unwrap_or_else(|| panic!("{name}: {text}"));
            let end = text
                .rfind("Everything Qrec needs is here.")
                .unwrap_or_else(|| panic!("{name}: {text}"));
            let _ = writeln!(out, "=== {name}\n{}\n", text[start + 1..end].trim_end());
            let json = report.to_json();
            assert_eq!(json["shortcuts"]["desktop"].as_str(), desktop, "{name}");
            assert_eq!(json["niriSnippet"], NIRI_SNIPPET, "{name}");
            assert_eq!(
                serde_json::from_value::<DoctorReport>(json).unwrap(),
                report
            );
        }
        insta::assert_snapshot!("shortcuts_per_desktop", out);
    }

    /// The real machine minus GStreamer (whose plugin scan may touch every
    /// GPU) and the VA encoder probe (a child of the `qrec` binary): ffmpeg,
    /// the portal over D-Bus (read-only), pactl, sysfs.
    /// Run by hand: `cargo test -p qrec-shell real_doctor -- --ignored --nocapture`.
    struct NoGst;

    impl Probe for NoGst {
        fn gstreamer(&self) -> Result<String, String> {
            Ok("(skipped)".to_owned())
        }
        fn gst_element(&self, _: &str) -> bool {
            true
        }
        fn gst_va_encoders(&self) -> Vec<String> {
            Vec::new()
        }
        fn gst_h264_decoders(&self) -> Vec<String> {
            vec!["(skipped)".to_owned()]
        }
        fn ges(&self) -> Result<(), String> {
            Ok(())
        }
        fn which(&self, program: &str) -> Option<PathBuf> {
            SystemProbe.which(program)
        }
        fn version(&self, program: &str) -> Result<String, String> {
            SystemProbe.version(program)
        }
        fn ffmpeg_encoders(&self) -> Result<BTreeSet<String>, String> {
            SystemProbe.ffmpeg_encoders()
        }
        fn ffmpeg_decoders(&self) -> Result<BTreeMap<String, String>, String> {
            SystemProbe.ffmpeg_decoders()
        }
        fn ffmpeg_demuxers(&self) -> Result<BTreeSet<String>, String> {
            SystemProbe.ffmpeg_demuxers()
        }
        fn screen_backend(&self) -> ScreenBackend {
            SystemProbe.screen_backend()
        }
        fn unix_socket(&self, path: &Path) -> Result<(), String> {
            SystemProbe.unix_socket(path)
        }
        fn portal(&self) -> Result<PortalInfo, String> {
            SystemProbe.portal()
        }
        fn x11(&self, display: &str) -> Result<X11Server, String> {
            SystemProbe.x11(display)
        }
        fn pactl_version(&self) -> Result<String, String> {
            SystemProbe.pactl_version()
        }
        fn pulse_server(&self) -> Result<String, String> {
            SystemProbe.pulse_server()
        }
        fn render_nodes(&self) -> Vec<gpu::RenderNode> {
            SystemProbe.render_nodes()
        }
        fn va_drivers(&self) -> gpu::VaDrivers {
            SystemProbe.va_drivers()
        }
        fn open_node(&self, path: &str) -> io::Result<()> {
            let nvidia = gpu::render_nodes()
                .into_iter()
                .any(|n| n.path == path && n.vendor == Some(PCI_NVIDIA));
            assert!(!nvidia, "never open the NVIDIA node");
            SystemProbe.open_node(path)
        }
        fn env(&self, key: &str) -> Option<String> {
            SystemProbe.env(key)
        }
        fn vaapi_h264(&self, device: &str, driver: Option<&str>) -> Result<(), String> {
            let nvidia = gpu::render_nodes()
                .into_iter()
                .any(|n| n.path == device && n.vendor == Some(PCI_NVIDIA));
            assert!(!nvidia, "never probe the NVIDIA node");
            SystemProbe.vaapi_h264(device, driver)
        }
        fn gst_va_factory(&self, _: &str) -> Result<Option<String>, String> {
            Err("skipped (the probe child is the qrec binary)".to_owned())
        }
        fn gpu_startup(&self) -> Vec<String> {
            SystemProbe.gpu_startup()
        }
        fn platform(&self) -> HostPlatform {
            SystemProbe.platform()
        }
        fn config_file(&self) -> Option<PathBuf> {
            SystemProbe.config_file()
        }
    }

    #[test]
    #[ignore = "talks to the real machine (no GStreamer)"]
    fn real_doctor() {
        let report = run_with(&NoGst, &DoctorOptions::from_env());
        println!("{}", report.to_text());
        println!("{:#?}", report.va);
    }
}
