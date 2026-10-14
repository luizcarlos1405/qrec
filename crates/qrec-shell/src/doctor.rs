//! `qrec doctor` (A§8.12 `DoctorReport`, SPEC §3.3). Owner: media.
//!
//! One check per runtime dependency, each with a hint when it fails:
//! GStreamer (initialized under the init watchdog) and its elements, GES,
//! ffmpeg/ffprobe and their encoders, the ScreenCast portal (D-Bus, with a
//! timeout), PipeWire through `pactl`, the optional `wl-copy`, and the VA-API
//! report (render nodes, the effective device, `LIBVA_DRIVER_NAME`, the
//! h264_vaapi export check as a 10 s child process; NVIDIA nodes are never
//! probed). Plus the niri bind snippet (SPEC §10.2). A failed required
//! check means exit 5 (`missing_dependency`); the GUI's "The swamp is
//! missing something" screen renders the same report.
//!
//! Everything that touches the system goes through [`Probe`], so tests run
//! against a fake machine.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use qrec_core::{CoreError, ErrorCode};
use serde::{Deserialize, Serialize};

use crate::sources;

/// The exit code when a required check fails (`missing_dependency`).
pub const MISSING_DEPENDENCY_EXIT: u8 = 5;

/// `ffmpeg -version`, `-encoders` and the VA-API check (A§8.7).
pub const FFMPEG_TIMEOUT: Duration = Duration::from_secs(10);

/// D-Bus property reads of the portal (may start xdg-desktop-portal).
pub const PORTAL_TIMEOUT: Duration = Duration::from_secs(5);

/// The niri snippet for global shortcuts (SPEC §10.2).
pub const NIRI_SNIPPET: &str = r#"binds {
    Mod+Shift+R { spawn "qrec" "record" "toggle"; }
    Mod+Shift+O { spawn "qrec" "record" "start" "--over-selection"; }
    Mod+Shift+D { spawn "qrec" "take" "discard-last"; }
}
"#;

const PCI_INTEL: u16 = 0x8086;
const PCI_AMD: u16 = 0x1002;
const PCI_NVIDIA: u16 = 0x10de;

/// What `qrec doctor` found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
    pub niri_snippet: String,
    pub va: Option<VaReport>,
}

/// One dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Check {
    /// Stable id: `gstreamer`, `gst.x264enc`, `ffmpeg.libx264`, `portal.screencast`, …
    pub id: String,
    pub required: bool,
    pub ok: bool,
    /// What was found (or why it failed), one line.
    pub detail: String,
    /// How to fix it; set when the check failed.
    pub hint: Option<String>,
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
    pub note: Option<String>,
}

/// A DRM render node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderNode {
    pub path: String,
    pub vendor: u16,
    pub vendor_name: String,
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
    /// cameras and PipeWire.
    pub fake_backend: bool,
    /// `recording.vaapiDevice` of the project in scope, if any.
    pub project_vaapi_device: Option<String>,
    /// `encoder.vaapi_device` from the user config, if any.
    pub config_vaapi_device: Option<String>,
    /// Run the h264_vaapi export check (a child process, 10 s).
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
    /// GStreamer Editing Services initializes.
    fn ges(&self) -> Result<(), String>;
    fn which(&self, program: &str) -> Option<PathBuf>;
    /// The first line of `<program> -version`.
    fn version(&self, program: &str) -> Result<String, String>;
    /// `ffmpeg -encoders` names.
    fn ffmpeg_encoders(&self) -> Result<BTreeSet<String>, String>;
    fn portal(&self) -> Result<PortalInfo, String>;
    /// `pactl --format=json info` → `server_name`.
    fn pulse_server(&self) -> Result<String, String>;
    fn render_nodes(&self) -> Vec<RenderNode>;
    fn env(&self, key: &str) -> Option<String>;
    /// The h264_vaapi export chain on `device` (two frames to the null muxer).
    fn vaapi_h264(&self, device: &str, driver: Option<&str>) -> Result<(), String>;
}

/// Runs every check against this machine.
pub fn run(opts: &DoctorOptions) -> DoctorReport {
    run_with(&SystemProbe, opts)
}

/// Runs every check against `probe`.
pub fn run_with(probe: &dyn Probe, opts: &DoctorOptions) -> DoctorReport {
    let mut checks = Vec::new();
    gstreamer_checks(probe, opts, &mut checks);
    ffmpeg_checks(probe, &mut checks);
    desktop_checks(probe, opts, &mut checks);
    checks.push(match probe.which("wl-copy") {
        Some(path) => ok("wl-copy", false, path.display().to_string()),
        None => fail(
            "wl-copy",
            false,
            "not found: saved frames can't be copied to the clipboard",
            "install wl-clipboard",
        ),
    });
    let va = va_report(probe, opts);
    checks.push(va_check(&va));
    DoctorReport {
        checks,
        niri_snippet: NIRI_SNIPPET.to_owned(),
        va: Some(va),
    }
}

fn ok(id: &str, required: bool, detail: impl Into<String>) -> Check {
    Check {
        id: id.to_owned(),
        required,
        ok: true,
        detail: detail.into(),
        hint: None,
    }
}

fn fail(id: &str, required: bool, detail: impl Into<String>, hint: impl Into<String>) -> Check {
    Check {
        id: id.to_owned(),
        required,
        ok: false,
        detail: detail.into(),
        hint: Some(hint.into()),
    }
}

/// A GStreamer element: what it is for and where it comes from.
struct Element {
    name: &'static str,
    role: &'static str,
    package: &'static str,
    /// Needed only with the real capture backend.
    real_only: bool,
}

const ELEMENTS: &[Element] = &[
    Element {
        name: "pipewiresrc",
        role: "screen capture",
        package: "PipeWire's GStreamer plugin (pipewire built with GStreamer support)",
        real_only: true,
    },
    Element {
        name: "v4l2src",
        role: "cameras",
        package: "gst-plugins-good",
        real_only: true,
    },
    Element {
        name: "pulsesrc",
        role: "audio capture",
        package: "gst-plugins-good",
        real_only: true,
    },
    Element {
        name: "compositor",
        role: "live preview",
        package: "gst-plugins-base",
        real_only: false,
    },
    Element {
        name: "videorate",
        role: "constant frame rate",
        package: "gst-plugins-base",
        real_only: false,
    },
    Element {
        name: "x264enc",
        role: "H.264 recording",
        package: "gst-plugins-ugly",
        real_only: false,
    },
    Element {
        name: "h264parse",
        role: "H.264 recording",
        package: "gst-plugins-bad",
        real_only: false,
    },
    Element {
        name: "matroskamux",
        role: "MKV files",
        package: "gst-plugins-good",
        real_only: false,
    },
    Element {
        name: "flacenc",
        role: "lossless audio",
        package: "gst-plugins-good",
        real_only: false,
    },
    Element {
        name: "gtk4paintablesink",
        role: "the GUI preview",
        package: "gst-plugins-rs (gtk4)",
        real_only: false,
    },
];

/// Only with `QREC_CAPTURE_BACKEND=fake`.
const FAKE_ELEMENTS: &[Element] = &[
    Element {
        name: "videotestsrc",
        role: "fake screens and cameras",
        package: "gst-plugins-base",
        real_only: false,
    },
    Element {
        name: "audiotestsrc",
        role: "fake audio",
        package: "gst-plugins-base",
        real_only: false,
    },
];

fn gstreamer_checks(probe: &dyn Probe, opts: &DoctorOptions, checks: &mut Vec<Check>) {
    let version = match probe.gstreamer() {
        Ok(version) => version,
        Err(e) => {
            checks.push(fail(
                "gstreamer",
                true,
                e,
                "install GStreamer ≥ 1.24 (the Qrec Nix package ships it); if it hangs, \
                 a GPU driver blocks the plugin scan: check the VA-API section",
            ));
            return;
        }
    };
    checks.push(ok("gstreamer", true, version));
    let extra: &[Element] = if opts.fake_backend {
        FAKE_ELEMENTS
    } else {
        &[]
    };
    for element in ELEMENTS.iter().chain(extra) {
        let required = !(element.real_only && opts.fake_backend);
        let id = format!("gst.{}", element.name);
        checks.push(if probe.gst_element(element.name) {
            ok(
                &id,
                required,
                format!("{} ({})", element.name, element.role),
            )
        } else {
            fail(
                &id,
                required,
                format!(
                    "{} ({}) isn't in the GStreamer registry",
                    element.name, element.role
                ),
                format!("install {}", element.package),
            )
        });
    }
    let va = probe.gst_va_encoders();
    checks.push(if va.is_empty() {
        fail(
            "gst.vah264enc",
            false,
            "no VA-API H.264 encoder: recordings use x264 on the CPU",
            "install gst-plugins-bad with the va plugin and a VA driver \
             (intel-media-driver for Intel, mesa for AMD)",
        )
    } else {
        ok("gst.vah264enc", false, va.join(", "))
    });
    checks.push(match probe.ges() {
        Ok(()) => ok("ges", true, "GStreamer Editing Services"),
        Err(e) => fail(
            "ges",
            true,
            format!("GStreamer Editing Services: {e}"),
            "install gst-editing-services",
        ),
    });
}

/// Encoder name, required, what for.
const ENCODERS: &[(&str, bool, &str)] = &[
    ("libx264", true, "MP4 exports"),
    ("aac", true, "MP4 audio"),
    ("pcm_s16le", true, "export segments"),
    ("libvpx-vp9", true, "WebM exports"),
    ("libopus", true, "WebM audio"),
    ("libsvtav1", false, "AV1 WebM exports"),
];

fn ffmpeg_checks(probe: &dyn Probe, checks: &mut Vec<Check>) {
    let mut ffmpeg_ok = false;
    for program in ["ffmpeg", "ffprobe"] {
        let check = match probe.which(program) {
            None => fail(
                program,
                true,
                format!("{program} wasn't found on PATH"),
                "install ffmpeg (a full build with libx264, libvpx and libopus; NixOS: ffmpeg-full)",
            ),
            Some(path) => match probe.version(program) {
                Ok(version) => {
                    ffmpeg_ok |= program == "ffmpeg";
                    ok(program, true, format!("{version} ({})", path.display()))
                }
                Err(e) => fail(
                    program,
                    true,
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
    match probe.ffmpeg_encoders() {
        Ok(encoders) => {
            for (name, required, role) in ENCODERS {
                let id = format!("ffmpeg.{name}");
                checks.push(if encoders.contains(*name) {
                    ok(&id, *required, format!("{name} ({role})"))
                } else {
                    fail(
                        &id,
                        *required,
                        format!("ffmpeg has no {name} encoder ({role})"),
                        "install a full ffmpeg build (NixOS: ffmpeg-full)",
                    )
                });
            }
        }
        Err(e) => checks.push(fail(
            "ffmpeg.encoders",
            true,
            format!("can't list ffmpeg's encoders: {e}"),
            "reinstall ffmpeg",
        )),
    }
}

fn desktop_checks(probe: &dyn Probe, opts: &DoctorOptions, checks: &mut Vec<Check>) {
    let required = !opts.fake_backend;
    let portal = probe.portal();
    checks.push(match &portal {
        Ok(info)
            if info.screencast_version >= 4 && info.source_types.is_none_or(|t| t & 1 != 0) =>
        {
            ok(
                "portal.screencast",
                required,
                format!(
                    "xdg-desktop-portal ScreenCast v{}{}",
                    info.screencast_version,
                    info.source_types
                        .map(|t| format!(" ({})", source_types(t)))
                        .unwrap_or_default()
                ),
            )
        }
        Ok(info) if info.screencast_version < 4 => fail(
            "portal.screencast",
            required,
            format!(
                "ScreenCast v{} can't remember the screen choice (needs v4)",
                info.screencast_version
            ),
            "update xdg-desktop-portal and its backend",
        ),
        Ok(_) => fail(
            "portal.screencast",
            required,
            "the ScreenCast portal can't capture monitors",
            "use a portal backend that captures monitors (on niri: xdg-desktop-portal-gnome)",
        ),
        Err(e) => fail(
            "portal.screencast",
            required,
            format!("no ScreenCast portal: {e}"),
            "install xdg-desktop-portal with a ScreenCast backend (on niri: \
             xdg-desktop-portal-gnome) and make sure it runs in your session",
        ),
    });
    if let Ok(info) = &portal {
        checks.push(match info.registry_version {
            Some(v) => ok("portal.registry", false, format!("host app registry v{v}")),
            None => fail(
                "portal.registry",
                false,
                "no host app registry (xdg-desktop-portal < 1.19): the screen choice \
                 may not be remembered for Qrec",
                "update xdg-desktop-portal to 1.19 or newer",
            ),
        });
    }

    let pactl = probe.which("pactl");
    let server = pactl.as_ref().map(|_| probe.pulse_server());
    checks.push(match (&pactl, &server) {
        (Some(path), Some(Ok(_))) => ok("pactl", required, path.display().to_string()),
        (Some(path), Some(Err(e))) => fail(
            "pactl",
            required,
            format!("{} can't reach the sound server: {e}", path.display()),
            "start PipeWire with pipewire-pulse",
        ),
        _ => fail(
            "pactl",
            required,
            "pactl wasn't found (it lists microphones and system audio)",
            "install the PulseAudio client tools (NixOS: pkgs.pulseaudio)",
        ),
    });
    checks.push(match &server {
        Some(Ok(name)) if name.contains("PipeWire") => ok("pipewire", required, name.clone()),
        Some(Ok(name)) => fail(
            "pipewire",
            required,
            format!("the sound server is {name}, not PipeWire"),
            "run PipeWire with pipewire-pulse (NixOS: services.pipewire.pulse.enable = true)",
        ),
        _ => fail(
            "pipewire",
            required,
            "can't ask the sound server (needs pactl)",
            "run PipeWire with pipewire-pulse (NixOS: services.pipewire.pulse.enable = true)",
        ),
    });
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

pub fn vendor_name(vendor: u16) -> String {
    match vendor {
        PCI_INTEL => "Intel".to_owned(),
        PCI_AMD => "AMD".to_owned(),
        PCI_NVIDIA => "NVIDIA".to_owned(),
        other => format!("vendor 0x{other:04x}"),
    }
}

/// `iHD` for Intel, `radeonsi` for AMD (A§8.10).
pub fn libva_driver_for(vendor: u16) -> Option<&'static str> {
    match vendor {
        PCI_INTEL => Some("iHD"),
        PCI_AMD => Some("radeonsi"),
        _ => None,
    }
}

fn va_report(probe: &dyn Probe, opts: &DoctorOptions) -> VaReport {
    let nodes = probe.render_nodes();
    let (device, origin) = if let Some(d) = &opts.project_vaapi_device {
        (Some(d.clone()), Some("project"))
    } else if let Some(d) = &opts.config_vaapi_device {
        (Some(d.clone()), Some("config"))
    } else {
        let auto = nodes
            .iter()
            .find(|n| matches!(n.vendor, PCI_INTEL | PCI_AMD))
            .map(|n| n.path.clone());
        let origin = auto.as_ref().map(|_| "auto");
        (auto, origin)
    };
    let vendor = device
        .as_ref()
        .and_then(|d| nodes.iter().find(|n| &n.path == d))
        .map(|n| n.vendor);
    let expected_driver = vendor.and_then(libva_driver_for).map(str::to_owned);
    let mut note = None;
    let ffmpeg_h264 = match (&device, vendor) {
        (Some(device), Some(PCI_INTEL | PCI_AMD)) if opts.probe_vaapi => {
            match probe.vaapi_h264(device, expected_driver.as_deref()) {
                Ok(()) => Some(true),
                Err(e) => {
                    note = Some(format!("the h264_vaapi check failed: {e}"));
                    Some(false)
                }
            }
        }
        (Some(_), Some(PCI_NVIDIA)) => {
            note = Some("NVIDIA nodes are never probed: VA-API on them can hang".to_owned());
            None
        }
        (Some(device), None) => {
            note = Some(format!("{device} is not a render node of this machine"));
            None
        }
        _ => None,
    };
    VaReport {
        nodes,
        device,
        origin: origin.map(str::to_owned),
        vendor,
        expected_driver,
        libva_driver_name: probe.env("LIBVA_DRIVER_NAME"),
        ffmpeg_h264,
        gst_encoders: probe.gst_va_encoders(),
        note,
    }
}

fn va_check(va: &VaReport) -> Check {
    let Some(device) = &va.device else {
        return fail(
            "vaapi",
            false,
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
    if let (Some(expected), Some(actual)) = (va.expected_driver.as_deref(), env) {
        if expected != actual {
            return fail(
                "vaapi",
                false,
                format!("{detail}, but {device} needs {expected}"),
                format!(
                    "unset LIBVA_DRIVER_NAME or set it to {expected} (Qrec sets it at startup)"
                ),
            );
        }
    }
    match (va.vendor, va.ffmpeg_h264) {
        (Some(PCI_INTEL | PCI_AMD), Some(false)) => fail(
            "vaapi",
            false,
            detail,
            "install the VA driver (intel-media-driver for Intel, mesa for AMD); \
             exports use libx264 meanwhile",
        ),
        (Some(PCI_INTEL | PCI_AMD), _) => ok("vaapi", false, detail),
        _ => fail(
            "vaapi",
            false,
            detail,
            "pick an Intel or AMD render node with encoder.vaapi_device",
        ),
    }
}

impl DoctorReport {
    /// Every required check passed.
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok || !c.required)
    }

    /// 0, or 5 (`missing_dependency`) when a required check failed.
    pub fn exit_code(&self) -> u8 {
        if self.ok() {
            0
        } else {
            MISSING_DEPENDENCY_EXIT
        }
    }

    /// The required checks that failed.
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

    /// The human report: one line per check, a hint under each failure, the
    /// VA-API summary and the niri snippet.
    pub fn to_text(&self) -> String {
        let mut out = String::from("Qrec doctor\n\n");
        let width = self.checks.iter().map(|c| c.id.len()).max().unwrap_or(0);
        for check in &self.checks {
            let status = match (check.ok, check.required) {
                (true, _) => "ok",
                (false, true) => "MISSING",
                (false, false) => "optional",
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
                    .map(|n| format!("{} ({})", n.path, n.vendor_name))
                    .collect();
                out.push_str(&nodes.join(", "));
            }
            out.push('\n');
            if let Some(note) = &va.note {
                let _ = writeln!(out, "  {note}");
            }
        }
        let _ = write!(
            out,
            "\nGlobal shortcuts for niri (paste into ~/.config/niri/config.kdl):\n\n{}\n",
            self.niri_snippet
        );
        let missing = self.missing();
        if missing.is_empty() {
            out.push_str("Everything Qrec needs is here.\n");
        } else {
            let ids: Vec<&str> = missing.iter().map(|c| c.id.as_str()).collect();
            let _ = writeln!(
                out,
                "The swamp is missing something: {} required check{} failed ({}).",
                missing.len(),
                if missing.len() == 1 { "" } else { "s" },
                ids.join(", ")
            );
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

    fn ges(&self) -> Result<(), String> {
        gst_impl::ges()
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        sources::find_program(program)
    }

    fn version(&self, program: &str) -> Result<String, String> {
        let out = sources::tool_output(&[program, "-hide_banner", "-version"], FFMPEG_TIMEOUT)?;
        out.lines()
            .next()
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .ok_or_else(|| format!("{program} -version printed nothing"))
    }

    fn ffmpeg_encoders(&self) -> Result<BTreeSet<String>, String> {
        let out = sources::tool_output(&["ffmpeg", "-hide_banner", "-encoders"], FFMPEG_TIMEOUT)?;
        Ok(parse_encoders(&out))
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

    fn pulse_server(&self) -> Result<String, String> {
        let json =
            sources::tool_output(&["pactl", "--format=json", "info"], sources::TOOL_TIMEOUT)?;
        sources::pulse_server_name(&json).ok_or_else(|| "pactl info has no server_name".to_owned())
    }

    fn render_nodes(&self) -> Vec<RenderNode> {
        scan_render_nodes(Path::new("/sys/class/drm"), Path::new("/dev/dri"))
    }

    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
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
}

/// The names in `ffmpeg -encoders` (after the `------` line).
pub fn parse_encoders(text: &str) -> BTreeSet<String> {
    text.lines()
        .skip_while(|l| !l.trim_start().starts_with("------"))
        .skip(1)
        .filter_map(|l| l.split_whitespace().nth(1))
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

/// `/sys/class/drm/renderD*` and their PCI vendors. Only sysfs is read: the
/// device nodes are never opened (an NVIDIA node can hang VA-API).
pub fn scan_render_nodes(sys_drm: &Path, dev_dri: &Path) -> Vec<RenderNode> {
    let Ok(entries) = fs::read_dir(sys_drm) else {
        return Vec::new();
    };
    let mut nodes: Vec<RenderNode> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.strip_prefix("renderD")?.parse::<u32>().ok()?;
            let vendor = fs::read_to_string(entry.path().join("device/vendor")).ok()?;
            let vendor = u16::from_str_radix(vendor.trim().trim_start_matches("0x"), 16).ok()?;
            Some(RenderNode {
                path: dev_dri.join(&name).to_string_lossy().into_owned(),
                vendor,
                vendor_name: vendor_name(vendor),
            })
        })
        .collect();
    nodes.sort_by(|a, b| a.path.cmp(&b.path));
    nodes
}

/// Two frames through the VA export chain (A§9.3) to the null muxer. Built
/// here until the core's `media_argv` has the VA check argv.
pub fn vaapi_check_argv(device: &str) -> Vec<String> {
    [
        "ffmpeg",
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-loglevel",
        "error",
        "-y",
        "-init_hw_device",
        &format!("vaapi=va:{device}"),
        "-filter_hw_device",
        "va",
        "-f",
        "lavfi",
        "-i",
        "color=c=0x000000:s=256x144:r=30/1",
        "-filter_complex",
        "[0:v]trim=end_frame=2,format=nv12,hwupload[vv]",
        "-map",
        "[vv]",
        "-c:v",
        "h264_vaapi",
        "-rc_mode",
        "CQP",
        "-qp",
        "22",
        "-f",
        "null",
        "-",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

#[cfg(feature = "gst")]
mod gst_impl {
    use std::time::Duration;

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

    pub(super) fn ges() -> Result<(), String> {
        init_gstreamer(GST_INIT_TIMEOUT)?;
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::Builder::new()
            .name("qrec-ges-init".into())
            .spawn(move || {
                let _ = tx.send(ges::init().map_err(|e| e.to_string()));
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

    pub(super) fn ges() -> Result<(), String> {
        Err(NO_GST.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A machine described by data.
    #[derive(Clone)]
    struct Fake {
        gst: Result<String, String>,
        elements: BTreeSet<&'static str>,
        va_encoders: Vec<String>,
        programs: BTreeMap<&'static str, &'static str>,
        encoders: BTreeSet<String>,
        portal: Result<PortalInfo, String>,
        pulse: Result<String, String>,
        nodes: Vec<RenderNode>,
        env: BTreeMap<&'static str, &'static str>,
        vaapi: Result<(), String>,
        vaapi_probed: std::cell::Cell<u32>,
    }

    fn node(path: &str, vendor: u16) -> RenderNode {
        RenderNode {
            path: path.to_owned(),
            vendor,
            vendor_name: vendor_name(vendor),
        }
    }

    impl Fake {
        /// The dev machine: everything present, Intel renderD129 + NVIDIA renderD128.
        fn healthy() -> Self {
            Fake {
                gst: Ok("GStreamer 1.26.11".to_owned()),
                elements: ELEMENTS
                    .iter()
                    .chain(FAKE_ELEMENTS)
                    .map(|e| e.name)
                    .collect(),
                va_encoders: vec!["vah264enc".to_owned()],
                programs: [
                    ("ffmpeg", "/nix/bin/ffmpeg"),
                    ("ffprobe", "/nix/bin/ffprobe"),
                    ("pactl", "/nix/bin/pactl"),
                    ("wl-copy", "/nix/bin/wl-copy"),
                ]
                .into(),
                encoders: ENCODERS.iter().map(|(n, ..)| (*n).to_owned()).collect(),
                portal: Ok(PortalInfo {
                    screencast_version: 5,
                    source_types: Some(7),
                    registry_version: Some(1),
                }),
                pulse: Ok("PulseAudio (on PipeWire 1.6.6)".to_owned()),
                nodes: vec![
                    node("/dev/dri/renderD128", PCI_NVIDIA),
                    node("/dev/dri/renderD129", PCI_INTEL),
                ],
                env: [("LIBVA_DRIVER_NAME", "iHD")].into(),
                vaapi: Ok(()),
                vaapi_probed: std::cell::Cell::new(0),
            }
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
        fn portal(&self) -> Result<PortalInfo, String> {
            self.portal.clone()
        }
        fn pulse_server(&self) -> Result<String, String> {
            self.pulse.clone()
        }
        fn render_nodes(&self) -> Vec<RenderNode> {
            self.nodes.clone()
        }
        fn env(&self, key: &str) -> Option<String> {
            self.env.get(key).map(|v| (*v).to_owned())
        }
        fn vaapi_h264(&self, device: &str, driver: Option<&str>) -> Result<(), String> {
            assert_ne!(device, "/dev/dri/renderD128", "NVIDIA must never be probed");
            assert_eq!(driver, Some("iHD"));
            self.vaapi_probed.set(self.vaapi_probed.get() + 1);
            self.vaapi.clone()
        }
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

    #[test]
    fn a_healthy_machine_passes_every_spec_check() {
        let fake = Fake::healthy();
        let report = run_with(&fake, &opts());
        assert!(report.ok(), "{}", report.to_text());
        assert_eq!(report.exit_code(), 0);
        assert!(report.missing_dependency().is_none());
        for id in [
            "gstreamer",
            "gst.pipewiresrc",
            "gst.v4l2src",
            "gst.pulsesrc",
            "gst.compositor",
            "gst.videorate",
            "gst.x264enc",
            "gst.vah264enc",
            "gst.matroskamux",
            "gst.flacenc",
            "gst.gtk4paintablesink",
            "ges",
            "ffmpeg",
            "ffprobe",
            "ffmpeg.libx264",
            "ffmpeg.libvpx-vp9",
            "ffmpeg.libopus",
            "ffmpeg.libsvtav1",
            "portal.screencast",
            "portal.registry",
            "pipewire",
            "pactl",
            "wl-copy",
            "vaapi",
        ] {
            let c = check(&report, id);
            assert!(c.ok && c.hint.is_none(), "{c:?}");
        }
        assert!(!check(&report, "gst.vah264enc").required);
        assert!(!check(&report, "ffmpeg.libsvtav1").required);
        assert!(!check(&report, "wl-copy").required);
        assert_eq!(
            check(&report, "portal.screencast").detail,
            "xdg-desktop-portal ScreenCast v5 (monitors, windows, virtual)"
        );
        let va = report.va.as_ref().unwrap();
        assert_eq!(va.device.as_deref(), Some("/dev/dri/renderD129"));
        assert_eq!(va.origin.as_deref(), Some("auto"));
        assert_eq!(va.expected_driver.as_deref(), Some("iHD"));
        assert_eq!(va.ffmpeg_h264, Some(true));
        assert_eq!(fake.vaapi_probed.get(), 1);
        assert_eq!(
            check(&report, "vaapi").detail,
            "/dev/dri/renderD129 (Intel, auto) · LIBVA_DRIVER_NAME=iHD · ffmpeg h264_vaapi works"
        );
        assert_eq!(report.niri_snippet, NIRI_SNIPPET);
        let text = report.to_text();
        assert!(
            text.contains("Mod+Shift+R { spawn \"qrec\" \"record\" \"toggle\"; }"),
            "{text}"
        );
        assert!(text.ends_with("Everything Qrec needs is here.\n"), "{text}");
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["checks"][0]["id"], "gstreamer");
        assert!(json["niriSnippet"].as_str().unwrap().starts_with("binds {"));
        assert_eq!(json["va"]["expectedDriver"], "iHD");
    }

    #[test]
    fn missing_required_things_mean_exit_5_with_hints() {
        let mut fake = Fake::healthy();
        fake.elements.remove("x264enc");
        fake.encoders.remove("libx264");
        fake.encoders.remove("libsvtav1");
        fake.programs.remove("wl-copy");
        fake.portal = Err("busctl exited with status 1".to_owned());
        let report = run_with(&fake, &opts());
        assert_eq!(report.exit_code(), MISSING_DEPENDENCY_EXIT);
        let missing: Vec<&str> = report.missing().iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            missing,
            ["gst.x264enc", "ffmpeg.libx264", "portal.screencast"]
        );
        for c in report.checks.iter().filter(|c| !c.ok) {
            assert!(c.hint.is_some(), "{c:?}");
        }
        assert_eq!(
            check(&report, "gst.x264enc").hint.as_deref(),
            Some("install gst-plugins-ugly")
        );
        assert!(!check(&report, "ffmpeg.libsvtav1").ok);
        assert!(!check(&report, "wl-copy").ok);
        assert!(report.checks.iter().all(|c| c.id != "portal.registry"));
        let error = report.missing_dependency().unwrap();
        assert_eq!(error.code, ErrorCode::MissingDependency);
        assert_eq!(error.exit_code(), 5);
        assert_eq!(
            error.message,
            "missing: gst.x264enc, ffmpeg.libx264, portal.screencast"
        );
        let text = report.to_text();
        assert!(text.contains("MISSING   gst.x264enc"), "{text}");
        assert!(text.contains("→ install gst-plugins-ugly"), "{text}");
        assert!(text.contains("optional  wl-copy"), "{text}");
        assert!(text.contains("3 required checks failed"), "{text}");
    }

    #[test]
    fn broken_gstreamer_and_ffmpeg_stop_their_sub_checks() {
        let mut fake = Fake::healthy();
        fake.gst = Err(sources::GST_STUCK.to_owned());
        fake.programs.remove("ffmpeg");
        let report = run_with(&fake, &opts());
        assert!(!check(&report, "gstreamer").ok);
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
        assert!(!check(&report, "gst.pipewiresrc").required);
        assert!(check(&report, "gst.videotestsrc").required);
        assert!(!check(&report, "portal.screencast").required);
        assert!(!check(&report, "pactl").required && !check(&report, "pactl").ok);
        assert!(!check(&report, "pipewire").required);
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
        assert!(!check(&report, "pipewire").ok);
        assert!(check(&report, "pactl").ok);
        assert!(check(&report, "portal.screencast").detail.contains("v3"));
        assert!(!check(&report, "portal.registry").ok);
        assert!(!check(&report, "portal.registry").required);

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
        assert_eq!(fake.vaapi_probed.get(), 0);

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
        assert_eq!(va.ffmpeg_h264, None);
        assert!(va.note.as_deref().unwrap().contains("never probed"));
        assert_eq!(fake.vaapi_probed.get(), 0);

        // The project's device wins; a mismatching LIBVA_DRIVER_NAME is reported.
        let mut fake = Fake::healthy();
        fake.env.insert("LIBVA_DRIVER_NAME", "nvidia");
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

        // A failing VA encode is a note and an optional failure.
        let mut fake = Fake::healthy();
        fake.vaapi = Err("ffmpeg exited with status 1".to_owned());
        let report = run_with(&fake, &opts());
        assert_eq!(report.va.as_ref().unwrap().ffmpeg_h264, Some(false));
        assert!(!check(&report, "vaapi").ok && report.ok());

        // Not probing at all.
        let fake = Fake::healthy();
        let report = run_with(
            &fake,
            &DoctorOptions {
                probe_vaapi: false,
                ..opts()
            },
        );
        assert_eq!(report.va.as_ref().unwrap().ffmpeg_h264, None);
        assert_eq!(fake.vaapi_probed.get(), 0);
        assert!(check(&report, "vaapi").ok);
    }

    #[test]
    fn parsers_and_scans() {
        let encoders = parse_encoders(
            "Encoders:\n V..... = Video\n ------\n V....D libx264              libx264 H.264\n \
             A....D aac                  AAC (Advanced Audio Coding)\n",
        );
        assert_eq!(encoders, ["aac".to_owned(), "libx264".to_owned()].into());
        assert_eq!(parse_dbus_u32("u 5\n"), Some(5));
        assert_eq!(parse_dbus_u32("(<uint32 7>,)\n"), Some(7));
        assert_eq!(parse_dbus_u32("nothing"), None);

        let sys = tempfile::tempdir().unwrap();
        for (name, vendor) in [
            ("renderD129", "0x8086\n"),
            ("renderD128", "0x10de\n"),
            ("card1", "0x8086\n"),
        ] {
            let dir = sys.path().join(name).join("device");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("vendor"), vendor).unwrap();
        }
        let nodes = scan_render_nodes(sys.path(), Path::new("/dev/dri"));
        assert_eq!(
            nodes,
            [
                node("/dev/dri/renderD128", PCI_NVIDIA),
                node("/dev/dri/renderD129", PCI_INTEL)
            ]
        );
        assert_eq!(libva_driver_for(PCI_AMD), Some("radeonsi"));
        assert_eq!(libva_driver_for(PCI_NVIDIA), None);
        assert_eq!(vendor_name(0x1234), "vendor 0x1234");

        let argv = vaapi_check_argv("/dev/dri/renderD129");
        assert!(argv.contains(&"vaapi=va:/dev/dri/renderD129".to_owned()));
        assert_eq!(&argv[argv.len() - 3..], ["-f", "null", "-"]);
    }
}
