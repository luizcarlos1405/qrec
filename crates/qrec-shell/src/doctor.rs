//! `qrec doctor` (A§8.12 `DoctorReport`, SPEC §3.3). Owner: media.
//!
//! One check per runtime dependency, each with a hint when it fails and
//! what needs it ([`Feature`], JSON `neededFor`):
//!
//! - the app itself: GStreamer ≥ 1.24 (initialized under the init
//!   watchdog), GES, the preview's elements and a software H.264 decoder,
//!   ffmpeg/ffprobe with the decoders and the demuxer the takes need;
//! - screen capture: the ScreenCast portal (D-Bus, with a timeout; v3
//!   works with the picker on every take) and pipewiresrc;
//! - cameras: v4l2src; audio capture: pulsesrc, `pactl` 16 or newer and
//!   the sound server it reaches (PipeWire or PulseAudio);
//! - recording: an H.264 encoder (the `va*h264enc` bound to the render
//!   node, which a probe child finds, else x264enc), h264parse,
//!   matroskamux and flacenc;
//! - each export format: ffmpeg's encoders (for MP4's video, h264_vaapi
//!   on the render node once the h264_vaapi check passed, else libx264).
//!
//! Checks nothing needs only inform: the optional `wl-copy`, the portal's
//! app registry and the VA-API report (render nodes, the effective device,
//! `LIBVA_DRIVER_NAME`, the h264_vaapi export check as a 10 s child
//! process; NVIDIA nodes are never probed). The report also says where
//! Qrec runs (`platform::current`) and how to bind the global shortcuts
//! there (SPEC §10.2, `keymap::global_snippet`: niri's KDL, sway's
//! `bindsym`, GNOME's custom shortcuts, …).
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
//! backend, `nix develop` in a Nix shell. The check also carries it as
//! `fix` (packages, command, note), whose command S23 offers to copy.
//!
//! Everything that touches the system goes through [`Probe`], so tests run
//! against a fake machine.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use qrec_core::keymap::{GlobalSnippet, NIRI_SNIPPET, global_snippet};
use qrec_core::media_argv::vaapi_check_argv;
use qrec_core::platform::{Hint, Need, VaGpu, install_hint};
use qrec_core::{CoreError, ErrorCode};
use serde::{Deserialize, Serialize};

use crate::gpu::{
    self, DeviceOrigin, VENDOR_AMD as PCI_AMD, VENDOR_INTEL as PCI_INTEL,
    VENDOR_NVIDIA as PCI_NVIDIA, libva_driver_for, vendor_name,
};
use crate::platform::HostPlatform;
use crate::sources;

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
    /// Where Qrec runs.
    pub platform: HostPlatform,
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
    /// The `LIBVA_DRIVER_NAME` the device needs (`iHD`, `radeonsi`).
    pub expected_driver: Option<String>,
    /// `LIBVA_DRIVER_NAME` in this process (main() sets it at startup).
    pub libva_driver_name: Option<String>,
    /// The h264_vaapi export chain encoded two frames; `None` = not probed.
    pub ffmpeg_h264: Option<bool>,
    /// `va*h264enc` GStreamer encoders.
    pub gst_encoders: Vec<String>,
    /// The one of them bound to `device` (the probe child of A§8.10, run
    /// with the h264_vaapi check); `None` when not probed or none is.
    pub gst_factory: Option<String>,
    /// What `main()` changed in the GPU environment at startup (A§8.10).
    pub startup: Vec<String>,
    pub note: Option<String>,
}

/// A DRM render node (`gpu::RenderNode` with its vendor's name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderNode {
    pub path: String,
    /// PCI vendor id from sysfs; `None` when unreadable.
    pub vendor: Option<u16>,
    pub vendor_name: Option<String>,
    /// Kernel driver (`i915`, `xe`, `amdgpu`, `nvidia`).
    pub driver: Option<String>,
}

impl From<&gpu::RenderNode> for RenderNode {
    fn from(node: &gpu::RenderNode) -> Self {
        Self {
            path: node.path.clone(),
            vendor: node.vendor,
            vendor_name: node.vendor.map(vendor_name),
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
    /// `QREC_CAPTURE_BACKEND=fake`: test sources instead of the portal,
    /// cameras and PipeWire, x264 for recordings and libx264 for MP4
    /// exports.
    pub fake_backend: bool,
    /// `recording.vaapiDevice` of the project in scope, if any.
    pub project_vaapi_device: Option<String>,
    /// `encoder.vaapi_device` from the user config, if any.
    pub config_vaapi_device: Option<String>,
    /// Run the VA-API child processes on an Intel or AMD node: the
    /// h264_vaapi export check and the probe for the `va*h264enc` bound to
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
    /// The `va*h264enc` factories.
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
    fn portal(&self) -> Result<PortalInfo, String>;
    /// The first line of `pactl --version` (`pactl 17.0`).
    fn pactl_version(&self) -> Result<String, String>;
    /// `pactl --format=json info` → `server_name`.
    fn pulse_server(&self) -> Result<String, String>;
    /// `/sys/class/drm/renderD*` (sysfs only; nodes are never opened).
    fn render_nodes(&self) -> Vec<gpu::RenderNode>;
    fn env(&self, key: &str) -> Option<String>;
    /// The h264_vaapi export chain on `device` (two frames to the null muxer).
    fn vaapi_h264(&self, device: &str, driver: Option<&str>) -> Result<(), String>;
    /// The `va*h264enc` whose `device-path` is `device`, from the probe
    /// child (A§8.10): `None` when no factory is bound to it.
    fn gst_va_factory(&self, device: &str) -> Result<Option<String>, String>;
    /// What `gpu::prepare_gpu_env` changed and noted at startup.
    fn gpu_startup(&self) -> Vec<String>;
    /// Where Qrec runs.
    fn platform(&self) -> HostPlatform;
}

/// Runs every check against this machine.
pub fn run(opts: &DoctorOptions) -> DoctorReport {
    run_with(&SystemProbe, opts)
}

/// Runs every check against `probe`.
pub fn run_with(probe: &dyn Probe, opts: &DoctorOptions) -> DoctorReport {
    let platform = probe.platform();
    let fix = |need| install_hint(need, &platform);
    let mut checks = Vec::new();
    // Asked only once GStreamer works: a stuck plugin scan isn't waited for twice.
    let gst_va_encoders = gstreamer_checks(probe, opts, &fix, &mut checks);
    let va = va_report(probe, opts, gst_va_encoders.clone().unwrap_or_default());
    if gst_va_encoders.is_some() {
        encoder_checks(&va, opts, &fix, &mut checks);
        checks.push(match probe.ges() {
            Ok(()) => ok("ges", APP, "GStreamer Editing Services"),
            Err(e) => fail_fix(
                "ges",
                APP,
                format!("GStreamer Editing Services: {e}"),
                fix(Need::Ges),
            ),
        });
    }
    ffmpeg_checks(probe, &va, opts, &fix, &mut checks);
    desktop_checks(probe, opts, &fix, &mut checks);
    checks.push(match probe.which("wl-copy") {
        Some(path) => ok("wl-copy", NOTHING, path.display().to_string()),
        None => fail_fix(
            "wl-copy",
            NOTHING,
            "not found: saved frames can't be copied to the clipboard",
            fix(Need::WlClipboard),
        ),
    });
    checks.push(va_check(&va, &fix));
    DoctorReport {
        features_off: features_off(&checks),
        checks,
        shortcuts: global_snippet(&platform.desktop),
        platform,
        niri_snippet: NIRI_SNIPPET.to_owned(),
        va: Some(va),
    }
}

/// The fix of a [`Need`] where Qrec runs (`run_with`'s platform).
type Fix<'a> = &'a dyn Fn(Need) -> Hint;

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

const ELEMENTS: &[Element] = &[
    Element {
        name: "pipewiresrc",
        role: "screen capture",
        need: Need::GstPipewire,
        needed_for: SCREENS,
        real_only: true,
    },
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
/// elements and the H.264 decoder. Returns the `va*h264enc` encoders when
/// GStreamer works, `None` when it doesn't.
fn gstreamer_checks(
    probe: &dyn Probe,
    opts: &DoctorOptions,
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
                fix(Need::Gstreamer).also(
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
            format!("update GStreamer to {min_major}.{min_minor} or newer"),
        ),
        _ => ok("gstreamer", APP, version),
    });
    let extra: &[Element] = if opts.fake_backend {
        FAKE_ELEMENTS
    } else {
        &[]
    };
    for element in ELEMENTS.iter().chain(extra) {
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
                fix(element.need),
            )
        });
    }
    let decoders = probe.gst_h264_decoders();
    checks.push(if decoders.is_empty() {
        fail_fix(
            "gst.h264dec",
            APP,
            "no software H.264 decoder: the editor can't play the takes",
            fix(Need::GstH264Decoder),
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

/// Whether the doctor asks the probe child for the `va*h264enc` bound to
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
/// first one GStreamer lists when that node is Intel or AMD. Never with
/// the fake backend, which records with x264enc.
fn va_recorder(va: &VaReport, opts: &DoctorOptions) -> Option<String> {
    if opts.fake_backend {
        None
    } else if probes_va_factory(opts, va.vendor, &va.gst_encoders) {
        va.gst_factory.clone()
    } else if matches!(va.vendor, Some(PCI_INTEL | PCI_AMD)) {
        va.gst_encoders.first().cloned()
    } else {
        None
    }
}

/// The VA-API GPU of a PCI vendor: Intel or AMD (NVIDIA has no VA-API
/// encoder, A§8.10). The generation isn't known here: Intel's hint names
/// both drivers.
fn va_gpu(vendor: Option<u16>) -> Option<VaGpu> {
    match vendor? {
        PCI_INTEL => Some(VaGpu::Intel),
        PCI_AMD => Some(VaGpu::Amd),
        _ => None,
    }
}

/// What gives GStreamer a VA encoder when it lists none. On an Intel or
/// AMD node it is usually the node's VA driver: GStreamer's VA plugin
/// registers no encoder for a node whose driver is missing or doesn't
/// encode H.264 (Fedora's own Mesa), so the hint is the driver's, with
/// the plugin's after it. Once ffmpeg's h264_vaapi check encoded on that
/// node, the driver works and the plugin is what's missing (Arch packages
/// it on its own, gst-plugin-va). Without an Intel or AMD node, the
/// plugin's hint alone.
fn no_va_encoder_fix(va: &VaReport, fix: Fix<'_>) -> Hint {
    match (va_gpu(va.vendor), va.ffmpeg_h264) {
        (Some(gpu), None | Some(false)) => fix(Need::VaDriver(gpu)).also(&format!(
            "if GStreamer still has no VA encoder: {}",
            fix(Need::GstVa).line()
        )),
        _ => fix(Need::GstVa),
    }
}

/// The recording encoder: `gst.x264enc` isn't needed when a VA encoder
/// records (its check is already in `checks`), and `gst.vah264enc` says
/// which VA encoder that is, or what gives GStreamer one.
fn encoder_checks(va: &VaReport, opts: &DoctorOptions, fix: Fix<'_>, checks: &mut Vec<Check>) {
    let recorder = va_recorder(va, opts);
    if let Some(x264) = checks.iter_mut().find(|c| c.id == "gst.x264enc") {
        match (&recorder, x264.ok) {
            (Some(recorder), ok) => {
                x264.needed_for.clear();
                x264.required = false;
                if !ok {
                    let _ = write!(x264.detail, ": recordings use {recorder}");
                }
            }
            (None, false) => x264
                .detail
                .push_str(" (and no VA-API encoder records instead)"),
            (None, true) => {}
        }
    }
    let id = "gst.vah264enc";
    let device = va.device.as_deref().unwrap_or("the render node");
    checks.push(if va.gst_encoders.is_empty() {
        fail_fix(
            id,
            NOTHING,
            "no VA-API H.264 encoder: recordings use x264 on the CPU",
            no_va_encoder_fix(va, fix),
        )
    } else if probes_va_factory(opts, va.vendor, &va.gst_encoders) {
        match &va.gst_factory {
            Some(factory) => ok(id, NOTHING, format!("{factory} records on {device}")),
            None => {
                let detail = format!(
                    "none of {} encodes on {device}: recordings use x264 on the CPU",
                    va.gst_encoders.join(", ")
                );
                // A probed node is Intel or AMD.
                match va_gpu(va.vendor) {
                    Some(gpu) => fail_fix(id, NOTHING, detail, fix(Need::VaDriver(gpu))),
                    None => fail(id, NOTHING, detail, "install the VA driver of that node"),
                }
            }
        }
    } else {
        ok(id, NOTHING, va.gst_encoders.join(", "))
    });
}

/// Encoder name, what needs it, what for.
const ENCODERS: &[(&str, &[Feature], &str)] = &[
    // Not needed when h264_vaapi encodes instead (`mp4_encoder_check`).
    ("libx264", MP4, "MP4 exports"),
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
                fix(Need::Ffmpeg),
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
                    "reinstall ffmpeg",
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
            "reinstall ffmpeg",
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
            fix(Need::FullFfmpeg),
        ),
        Err(e) => fail(
            "ffmpeg.demuxers",
            APP,
            format!("can't list ffmpeg's demuxers: {e}"),
            "reinstall ffmpeg",
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
                        fix(Need::FullFfmpeg),
                    )
                });
            }
            mp4_encoder_check(va_exporter(va, opts, &encoders), checks);
        }
        Err(e) => checks.push(fail(
            "ffmpeg.encoders",
            EXPORTS,
            format!("can't list ffmpeg's encoders: {e}"),
            "reinstall ffmpeg",
        )),
    }
}

/// The render node MP4 exports encode on with h264_vaapi instead of
/// libx264 (A§9.3 `export_vaapi`), if any, as [`va_recorder`] says for
/// recordings: the effective node once its h264_vaapi check passed.
/// Unprobed (the GUI's doctor, whose host runs the check), that node when
/// it is Intel or AMD and ffmpeg has h264_vaapi. Never with the fake
/// backend, which exports with libx264.
fn va_exporter<'a>(
    va: &'a VaReport,
    opts: &DoctorOptions,
    encoders: &BTreeSet<String>,
) -> Option<&'a str> {
    let exports = if opts.fake_backend {
        false
    } else if opts.probe_vaapi {
        va.ffmpeg_h264 == Some(true)
    } else {
        matches!(va.vendor, Some(PCI_INTEL | PCI_AMD)) && encoders.contains("h264_vaapi")
    };
    va.device.as_deref().filter(|_| exports)
}

/// MP4's video encoder, as [`encoder_checks`] does the recordings': when
/// h264_vaapi encodes the exports on `exporter` ([`va_exporter`]),
/// `ffmpeg.libx264` is needed for nothing and MP4 needs only aac and
/// pcm_s16le.
fn mp4_encoder_check(exporter: Option<&str>, checks: &mut [Check]) {
    let Some(device) = exporter else {
        return;
    };
    if let Some(x264) = checks.iter_mut().find(|c| c.id == "ffmpeg.libx264") {
        x264.needed_for.clear();
        x264.required = false;
        if !x264.ok {
            x264.detail =
                format!("ffmpeg has no libx264 encoder: MP4 exports use h264_vaapi on {device}");
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
            fix(Need::FullFfmpeg),
        )
    }
}

fn desktop_checks(probe: &dyn Probe, opts: &DoctorOptions, fix: Fix<'_>, checks: &mut Vec<Check>) {
    // The fake backend captures test sources: it needs none of these.
    let real = |features: &'static [Feature]| {
        if opts.fake_backend { NOTHING } else { features }
    };
    let screens = real(SCREENS);
    let portal = probe.portal();
    checks.push(match &portal {
        // The backend that answers can't: the desktop's own can.
        Ok(info) if info.source_types.is_some_and(|t| t & 1 == 0) => fail_fix(
            "portal.screencast",
            screens,
            "the ScreenCast portal can't capture monitors",
            fix(Need::ScreenCastPortal),
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
            fix(Need::ScreenCastPortal),
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
    audio_checks(probe, real(AUDIO), fix, checks);
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
            fix(Need::Pactl),
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
                "reinstall the PulseAudio client tools",
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
            format!("update the PulseAudio client tools (pactl) to {min_major} or newer"),
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
            fix(Need::SoundServer),
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
    let expected_driver = vendor.and_then(libva_driver_for).map(str::to_owned);
    let mut notes = Vec::new();
    let ffmpeg_h264 = match (&device, vendor) {
        (Some(device), Some(PCI_INTEL | PCI_AMD)) if opts.probe_vaapi => {
            match probe.vaapi_h264(device, expected_driver.as_deref()) {
                Ok(()) => Some(true),
                Err(e) => {
                    notes.push(format!("the h264_vaapi check failed: {e}"));
                    Some(false)
                }
            }
        }
        (Some(_), Some(PCI_NVIDIA)) => {
            notes.push("NVIDIA nodes are never probed: VA-API on them can hang".to_owned());
            None
        }
        (Some(device), None) => {
            notes.push(format!("{device} is not a render node of this machine"));
            None
        }
        _ => None,
    };
    let gst_factory = match &device {
        Some(device) if probes_va_factory(opts, vendor, &gst_encoders) => {
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
        expected_driver,
        libva_driver_name: probe.env("LIBVA_DRIVER_NAME"),
        ffmpeg_h264,
        gst_encoders,
        gst_factory,
        startup: probe.gpu_startup(),
        note: (!notes.is_empty()).then(|| notes.join("; ")),
    }
}

fn va_check(va: &VaReport, fix: Fix<'_>) -> Check {
    let Some(device) = &va.device else {
        return fail(
            "vaapi",
            NOTHING,
            "no Intel or AMD render node: recordings and exports use x264 on the CPU",
            "Qrec never picks an NVIDIA node by itself; set encoder.vaapi_device in \
             ~/.config/qrec/config.toml to use one",
        );
    };
    let mut detail = format!(
        "{device} ({}, {})",
        va.vendor.map_or_else(|| "unknown".to_owned(), vendor_name),
        va.origin.as_deref().unwrap_or("auto")
    );
    let env = va.libva_driver_name.as_deref();
    let _ = write!(detail, " · LIBVA_DRIVER_NAME={}", env.unwrap_or("(unset)"));
    match va.ffmpeg_h264 {
        Some(true) => detail.push_str(" · ffmpeg h264_vaapi works"),
        Some(false) => detail.push_str(" · ffmpeg h264_vaapi failed"),
        None => {}
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
    match (va_gpu(va.vendor), va.ffmpeg_h264) {
        (Some(gpu), Some(false)) => fail_fix(
            "vaapi",
            NOTHING,
            detail,
            fix(Need::VaDriver(gpu)).also("exports use libx264 meanwhile"),
        ),
        (Some(_), _) => ok("vaapi", NOTHING, detail),
        _ => fail(
            "vaapi",
            NOTHING,
            detail,
            "pick an Intel or AMD render node with encoder.vaapi_device",
        ),
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

    /// The human report: one line per check (`ok`, `MISSING` for an app
    /// check, `off` when features need it, else `optional`), a hint under
    /// each failure, the VA-API summary, the global shortcuts for the
    /// desktop, then a line per feature that is off and the verdict.
    pub fn to_text(&self) -> String {
        let mut out = String::from("Qrec doctor\n\n");
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
                        format!("{} ({vendor})", n.path)
                    })
                    .collect();
                out.push_str(&nodes.join(", "));
            }
            out.push('\n');
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

    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn gpu_startup(&self) -> Vec<String> {
        let Some(report) = gpu::startup_report() else {
            return Vec::new();
        };
        let changes = report.changes.iter().map(|c| match &c.previous {
            Some(previous) => format!("set {}={} (was {previous})", c.key, c.value),
            None => format!("set {}={}", c.key, c.value),
        });
        let mut lines: Vec<String> = changes.collect();
        if !report.applied && !report.changes.is_empty() {
            lines.push("those changes were not applied (more than one thread)".to_owned());
        }
        lines.extend(report.notes.iter().cloned());
        lines
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
        let probe = gpu::Probe::VaH264Factory {
            device: device.to_owned(),
        };
        let answer = gpu::run_probe_child(&exe, &probe, None, gpu::PROBE_TIMEOUT)?;
        match answer.error {
            Some(error) => Err(error),
            None => Ok(answer.factory),
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
        .filter(|n| n.starts_with("va") && n.ends_with("h264enc"))
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
    use super::*;
    use crate::platform::{Desktop, PlatformEnv, detect};

    /// A machine described by data.
    #[derive(Clone)]
    struct Fake {
        gst: Result<String, String>,
        elements: BTreeSet<&'static str>,
        va_encoders: Vec<String>,
        h264_decoders: Vec<String>,
        programs: BTreeMap<&'static str, &'static str>,
        encoders: BTreeSet<String>,
        decoders: BTreeMap<String, String>,
        demuxers: BTreeSet<String>,
        portal: Result<PortalInfo, String>,
        pactl: Result<String, String>,
        pulse: Result<String, String>,
        nodes: Vec<gpu::RenderNode>,
        env: BTreeMap<&'static str, &'static str>,
        vaapi: Result<(), String>,
        va_factory: Result<Option<String>, String>,
        startup: Vec<String>,
        platform: HostPlatform,
        vaapi_probed: std::cell::Cell<u32>,
        factory_probed: std::cell::Cell<u32>,
    }

    fn node(path: &str, vendor: u16) -> gpu::RenderNode {
        gpu::RenderNode {
            path: path.to_owned(),
            vendor: Some(vendor),
            driver: None,
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

    impl Fake {
        /// The dev machine: niri on NixOS, everything present, Intel
        /// renderD129 + NVIDIA renderD128.
        fn healthy() -> Self {
            Fake {
                gst: Ok("GStreamer 1.26.11".to_owned()),
                elements: ELEMENTS
                    .iter()
                    .chain(FAKE_ELEMENTS)
                    .map(|e| e.name)
                    .collect(),
                va_encoders: vec!["vah264enc".to_owned()],
                h264_decoders: vec!["avdec_h264".to_owned(), "openh264dec".to_owned()],
                programs: [
                    ("ffmpeg", "/nix/bin/ffmpeg"),
                    ("ffprobe", "/nix/bin/ffprobe"),
                    ("pactl", "/nix/bin/pactl"),
                    ("wl-copy", "/nix/bin/wl-copy"),
                ]
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
                portal: Ok(PortalInfo {
                    screencast_version: 5,
                    source_types: Some(7),
                    registry_version: Some(1),
                }),
                pactl: Ok("pactl 17.0".to_owned()),
                pulse: Ok("PulseAudio (on PipeWire 1.6.6)".to_owned()),
                nodes: vec![
                    node("/dev/dri/renderD128", PCI_NVIDIA),
                    node("/dev/dri/renderD129", PCI_INTEL),
                ],
                env: [("LIBVA_DRIVER_NAME", "iHD")].into(),
                vaapi: Ok(()),
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
                vaapi_probed: std::cell::Cell::new(0),
                factory_probed: std::cell::Cell::new(0),
            }
        }

        /// One Intel node (renderD128) whose VA encoder is `vah264enc`.
        fn intel_only(mut self) -> Self {
            self.nodes = vec![node("/dev/dri/renderD128", PCI_INTEL)];
            self.va_factory = Ok(Some("vah264enc".to_owned()));
            self.startup.clear();
            self
        }

        /// The vendor of the node a probe runs on (also through a
        /// symlink), which is never NVIDIA.
        fn probed_vendor(&self, device: &str) -> Option<u16> {
            let real = std::fs::canonicalize(device).ok();
            let vendor = self
                .nodes
                .iter()
                .find(|n| n.path == device || std::fs::canonicalize(&n.path).ok() == real)
                .and_then(|n| n.vendor);
            assert_ne!(vendor, Some(PCI_NVIDIA), "NVIDIA must never be probed");
            vendor
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
        fn portal(&self) -> Result<PortalInfo, String> {
            self.portal.clone()
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
        fn env(&self, key: &str) -> Option<String> {
            self.env.get(key).map(|v| (*v).to_owned())
        }
        fn vaapi_h264(&self, device: &str, driver: Option<&str>) -> Result<(), String> {
            let vendor = self.probed_vendor(device);
            assert_eq!(driver, vendor.and_then(libva_driver_for));
            self.vaapi_probed.set(self.vaapi_probed.get() + 1);
            self.vaapi.clone()
        }
        fn gst_va_factory(&self, device: &str) -> Result<Option<String>, String> {
            self.probed_vendor(device);
            self.factory_probed.set(self.factory_probed.get() + 1);
            self.va_factory.clone()
        }
        fn gpu_startup(&self) -> Vec<String> {
            self.startup.clone()
        }
        fn platform(&self) -> HostPlatform {
            self.platform.clone()
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
            "ffmpeg.libvpx-vp9",
            "ffmpeg.libopus",
            "ffmpeg.libsvtav1",
            "portal.screencast",
            "portal.registry",
            "pactl",
            "sound.server",
            "wl-copy",
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
        assert_eq!(needs("portal.screencast"), [Feature::Screens]);
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
        for id in ["gst.vah264enc", "wl-copy", "vaapi", "portal.registry"] {
            assert!(needs(id).is_empty(), "{id}");
        }
        // The dev box records and exports MP4 with VA, so x264enc and
        // libx264 are needed for nothing.
        assert!(needs("gst.x264enc").is_empty());
        assert!(needs("ffmpeg.libx264").is_empty());
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
        assert_eq!(report.shortcuts, global_snippet(&Desktop::Niri));
        let text = report.to_text();
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
        fake.programs.remove("wl-copy");
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
        assert!(text.contains("optional  wl-copy"), "{text}");
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

    /// x264enc is needed for recording only when no VA encoder records.
    #[test]
    fn recording_needs_va_or_x264() {
        let mut fake = Fake::healthy();
        fake.elements.remove("x264enc");
        let report = run_with(&fake, &opts());
        let x264 = check(&report, "gst.x264enc");
        assert!(!x264.ok && x264.needed_for.is_empty(), "{x264:?}");
        assert!(x264.detail.ends_with(": recordings use vah264enc"));
        assert!(report.features_off.is_empty());

        // The probe found no encoder bound to the node: recording is off.
        fake.va_factory = Ok(None);
        let report = run_with(&fake, &opts());
        assert_eq!(off(&report), [Feature::Recording]);
        assert_eq!(
            report.features_off[0].message,
            "Recording is off: x264enc (H.264 recording) isn't in the GStreamer registry \
             (and no VA-API encoder records instead)"
        );
        let va = check(&report, "gst.vah264enc");
        assert!(!va.ok && va.needed_for.is_empty(), "{va:?}");
        assert!(
            va.detail
                .starts_with("none of vah264enc encodes on /dev/dri/renderD129")
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
        // The fake backend records with x264enc whatever the GPU has.
        let report = run_with(
            &fake,
            &DoctorOptions {
                fake_backend: true,
                probe_vaapi: false,
                ..opts()
            },
        );
        assert_eq!(off(&report), [Feature::Recording]);
        assert!(
            fake.factory_probed.get() == 3,
            "probed with probe_vaapi only"
        );
    }

    /// GStreamer lists no VA encoder: on an Intel or AMD node, the node's
    /// driver first, then GStreamer's VA plugin; the plugin alone once
    /// ffmpeg's h264_vaapi encoded on the node, or without such a node.
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
        fake.env = [("LIBVA_DRIVER_NAME", "radeonsi")].into();
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
        fake.env = [("LIBVA_DRIVER_NAME", "iHD")].into();
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
        // No Intel or AMD node: the plugin.
        fake.nodes = vec![node("/dev/dri/renderD128", PCI_NVIDIA)];
        fake.env.clear();
        let report = run_with(&fake, &opts());
        assert_eq!(check(&report, "gst.vah264enc").hint.as_deref(), plugin);
    }

    /// libx264 is needed for MP4 exports only when h264_vaapi doesn't
    /// encode them (the recordings' rule); aac and pcm_s16le always are.
    #[test]
    fn mp4_exports_need_vaapi_or_libx264() {
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
        let text = report.to_text();
        assert!(text.contains("optional  ffmpeg.libx264"), "{text}");
        assert!(text.ends_with("Everything Qrec needs is here.\n"), "{text}");

        // MP4's audio is still ffmpeg's.
        fake.encoders.remove("aac");
        let report = run_with(&fake, &opts());
        assert_eq!(off(&report), [Feature::ExportMp4]);
        assert_eq!(report.features_off[0].checks, ["ffmpeg.aac"]);
        fake.encoders.insert("aac".to_owned());

        // The h264_vaapi check failed: libx264 encodes MP4, and it's missing.
        fake.vaapi = Err("ffmpeg exited with status 1".to_owned());
        let report = run_with(&fake, &opts());
        assert_eq!(off(&report), [Feature::ExportMp4]);
        assert_eq!(report.features_off[0].checks, ["ffmpeg.libx264"]);
        assert_eq!(
            report.features_off[0].message,
            "MP4 export is off: ffmpeg has no libx264 encoder (MP4 exports)"
        );

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
        // Nor does the fake backend, which exports with libx264.
        let fake_backend = DoctorOptions {
            fake_backend: true,
            ..unprobed
        };
        assert_eq!(off(&run_with(&fake, &fake_backend)), [Feature::ExportMp4]);
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
        assert_eq!(fake.vaapi_probed.get(), 3, "probed with probe_vaapi only");
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
            "portal.screencast",
            "pactl",
        ] {
            assert!(check(&report, id).needed_for.is_empty(), "{id}");
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
        // Only NVIDIA: nothing is picked automatically.
        let mut fake = Fake::healthy();
        fake.nodes = vec![node("/dev/dri/renderD128", PCI_NVIDIA)];
        fake.env.clear();
        let report = run_with(&fake, &opts());
        let va = report.va.as_ref().unwrap();
        assert_eq!((va.device.as_deref(), va.ffmpeg_h264), (None, None));
        assert!(!check(&report, "vaapi").ok && report.ok());
        assert_eq!((fake.vaapi_probed.get(), fake.factory_probed.get()), (0, 0));
        // x264enc records.
        assert_eq!(
            check(&report, "gst.x264enc").needed_for,
            [Feature::Recording]
        );

        // Configured explicitly: reported, still never probed.
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
        assert!(va.note.as_deref().unwrap().contains("never probed"));
        assert_eq!((fake.vaapi_probed.get(), fake.factory_probed.get()), (0, 0));

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
        let fedora_fix = |need| install_hint(need, &platform(&[], "ID=fedora\n"));
        assert_eq!(
            decoders_check(&decoders, &fedora_fix).detail,
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
            decoders_check(&fedora, &fedora_fix).detail,
            "libopenh264 for h264 (the takes' video), flac (the takes' audio)"
        );
        let cuda: BTreeMap<String, String> = decoders
            .iter()
            .filter(|(n, _)| *n == "h264_cuvid" || *n == "flac")
            .map(|(n, c)| (n.clone(), c.clone()))
            .collect();
        let c = decoders_check(&cuda, &fedora_fix);
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
        let gnome = |os_release: &str| {
            platform(
                &[
                    ("WAYLAND_DISPLAY", "wayland-0"),
                    ("XDG_SESSION_TYPE", "wayland"),
                    ("XDG_CURRENT_DESKTOP", "GNOME"),
                ],
                os_release,
            )
        };
        // PulseAudio itself (no PipeWire): Debian with GNOME.
        let mut pulseaudio_16 = Fake::healthy().intel_only();
        pulseaudio_16.platform = gnome("ID=debian\n");
        pulseaudio_16.pactl = Ok("pactl 16.1".to_owned());
        pulseaudio_16.pulse = Ok("pulseaudio".to_owned());
        let mut pulseaudio_15 = pulseaudio_16.clone();
        pulseaudio_15.pactl = Ok("pactl 15.0".to_owned());

        // Xfce on Xorg: no portal on the session bus, no wl-copy.
        let mut xfce = Fake::healthy().intel_only();
        xfce.platform = platform(
            &[
                ("DISPLAY", ":0"),
                ("XDG_SESSION_TYPE", "x11"),
                ("XDG_CURRENT_DESKTOP", "XFCE"),
            ],
            "ID=debian\n",
        );
        xfce.portal = Err(
            "busctl exited with status 1: Call failed: The name org.freedesktop.portal.Desktop \
             was not provided by any .service files"
                .to_owned(),
        );
        xfce.programs.remove("wl-copy");
        xfce.pulse = Ok("PulseAudio (on PipeWire 1.4.2)".to_owned());

        // Stock Fedora: no x264enc (plugins-ugly-free), ffmpeg-free (no
        // libx264, OpenH264 decodes), VA on Intel, which records and
        // encodes the MP4 exports.
        let mut fedora = Fake::healthy().intel_only();
        fedora.platform = gnome("ID=fedora\nVERSION_ID=44\n");
        fedora.elements.remove("x264enc");
        fedora.h264_decoders = vec!["openh264dec".to_owned()];
        fedora.encoders.remove("libx264");
        fedora.encoders.insert("libopenh264".to_owned());
        fedora.decoders.remove("h264");
        fedora
            .decoders
            .insert("libopenh264".to_owned(), "h264".to_owned());
        fedora.programs.insert("wl-copy", "/usr/bin/wl-copy");

        // sway without xdg-desktop-portal-wlr: the frontend runs, nothing
        // answers for ScreenCast. AMD graphics.
        let mut sway = Fake::healthy();
        sway.platform = platform(
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
        sway.nodes = vec![node("/dev/dri/renderD128", PCI_AMD)];
        sway.env = [("LIBVA_DRIVER_NAME", "radeonsi")].into();
        sway.va_factory = Ok(Some("vah264enc".to_owned()));
        sway.startup.clear();

        // NVIDIA only: no VA node Qrec would pick; x264 records.
        let mut nvidia = Fake::healthy();
        nvidia.platform = gnome("ID=ubuntu\nID_LIKE=debian\n");
        nvidia.nodes = vec![node("/dev/dri/renderD128", PCI_NVIDIA)];
        nvidia.va_encoders.clear();
        nvidia.env.clear();
        nvidia.startup.clear();

        // A VM: virtio-gpu, no VA at all.
        let mut vm = Fake::healthy();
        vm.platform = platform(
            &[
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "KDE"),
            ],
            "ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n",
        );
        vm.nodes = vec![node("/dev/dri/renderD128", 0x1af4)];
        vm.va_encoders.clear();
        vm.env.clear();
        vm.startup.clear();

        // Debian without gst-libav: no software H.264 decoder (app).
        let mut debian = Fake::healthy().intel_only();
        debian.platform = gnome("ID=debian\nVERSION_ID=\"13\"\n");
        debian.h264_decoders.clear();

        vec![
            ("dev_box", Fake::healthy()),
            ("pulseaudio_15", pulseaudio_15),
            ("pulseaudio_16", pulseaudio_16),
            ("xfce_x11_no_portal", xfce),
            ("fedora_stock", fedora),
            ("sway_without_xdpw", sway),
            ("nvidia_only", nvidia),
            ("vm", vm),
            ("debian_without_gst_libav", debian),
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
                ("xfce_x11_no_portal", 0, vec![Screens]),
                ("fedora_stock", 0, vec![]),
                ("sway_without_xdpw", 0, vec![Screens]),
                ("nvidia_only", 0, vec![]),
                ("vm", 0, vec![]),
                ("debian_without_gst_libav", 5, vec![]),
            ]
        );
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
        fn portal(&self) -> Result<PortalInfo, String> {
            SystemProbe.portal()
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
        fn env(&self, key: &str) -> Option<String> {
            SystemProbe.env(key)
        }
        fn vaapi_h264(&self, device: &str, driver: Option<&str>) -> Result<(), String> {
            assert!(
                !device.ends_with("renderD128"),
                "never probe the NVIDIA node"
            );
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
    }

    #[test]
    #[ignore = "talks to the real machine (no GStreamer)"]
    fn real_doctor() {
        let report = run_with(&NoGst, &DoctorOptions::from_env());
        println!("{}", report.to_text());
        println!("{:#?}", report.va);
    }
}
