//! Source discovery (A§8.12, `SourceInventory`). Owner: media.
//!
//! - Screens: `wl_output` v4 `name`/`description` + geometry, current mode
//!   and scale, read without blocking past a 2 s deadline; fallback
//!   `niri msg --json outputs` (3 s).
//! - Cameras: GStreamer's `DeviceMonitor` (`Video/Source`, no capture) under
//!   the GStreamer init watchdog when the `gst` feature is on; otherwise, or
//!   when that fails, `/sys/class/video4linux` (names only, no modes).
//! - Audio: `pactl --format=json` (3 s each): mics are `alsa_input.*` only
//!   (SPEC §5.1), monitors are `*.monitor`.
//! - `QREC_CAPTURE_BACKEND=fake`: the fake inventory of A§8.11 (screens
//!   `FAKE-1`, `FAKE-2` at 1280×720, camera `/dev/fake0`, mic
//!   `alsa_input.fake`, monitor `alsa_output.fake.monitor`); nothing real is
//!   touched.
//!
//! Every child runs through the A§8.7 runner (`media::run_child`) with a
//! timeout and its stderr in a temp file. Nothing here opens a device for
//! capture.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use qrec_core::ctx::{AudioDeviceInfo, CameraInfo, CameraMode, ScreenInfo};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::media::{ChildSpec, ChildStderr, ChildStdout, run_child};

/// Selects the capture backend: `fake` uses test sources (A§8.11).
pub const BACKEND_ENV: &str = "QREC_CAPTURE_BACKEND";

/// How long the Wayland output listing may take.
pub const WAYLAND_TIMEOUT: Duration = Duration::from_secs(2);

/// `pactl` and `niri msg` (A§8.7).
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(3);

/// How long camera discovery through GStreamer may take (after init).
pub const CAMERA_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `gst::init()` may take before it counts as stuck (A§8.10).
pub const GST_INIT_TIMEOUT: Duration = Duration::from_secs(30);

/// The most a tool's JSON may be.
const STDOUT_LIMIT: usize = 8 << 20;

/// Everything that can be recorded (`qrec sources list`, `Facts`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceInventory {
    pub screens: Vec<ScreenInfo>,
    pub cameras: Vec<CameraInfo>,
    pub mics: Vec<AudioDeviceInfo>,
    pub monitors: Vec<AudioDeviceInfo>,
    /// The source name of the default mic.
    pub default_mic: Option<String>,
    /// The source name of the default sink's monitor.
    pub default_monitor: Option<String>,
    /// What couldn't be discovered and why (e.g. "pactl wasn't found").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Which kinds of sources to discover (a clip split needs none, an
/// `AudioAdd` only audio: A§4.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Discover {
    pub screens: bool,
    pub cameras: bool,
    pub audio: bool,
}

impl Discover {
    pub const ALL: Discover = Discover {
        screens: true,
        cameras: true,
        audio: true,
    };
    /// `FactsNeeded::video_sources`.
    pub const VIDEO: Discover = Discover {
        screens: true,
        cameras: true,
        audio: false,
    };
    /// `FactsNeeded::audio_devices`.
    pub const AUDIO: Discover = Discover {
        screens: false,
        cameras: false,
        audio: true,
    };
}

/// `QREC_CAPTURE_BACKEND=fake`.
pub fn fake_backend() -> bool {
    std::env::var(BACKEND_ENV).is_ok_and(|v| v.trim() == "fake")
}

impl SourceInventory {
    /// Discovers what `what` asks for: the fake inventory under
    /// `QREC_CAPTURE_BACKEND=fake`, else the real devices. Never fails:
    /// problems become `warnings` and empty lists.
    pub fn discover(what: Discover) -> Self {
        if fake_backend() {
            return Self::fake().only(what);
        }
        let mut inventory = SourceInventory::default();
        if what.screens {
            match discover_screens() {
                Ok(screens) => inventory.screens = screens,
                Err(e) => inventory.warnings.push(format!("screens: {e}")),
            }
        }
        if what.cameras {
            let (cameras, warning) = cameras_and_warning();
            inventory.cameras = cameras;
            inventory
                .warnings
                .extend(warning.map(|w| format!("cameras: {w}")));
        }
        if what.audio {
            match discover_audio() {
                Ok(audio) => {
                    inventory.default_mic = audio.default_mic();
                    inventory.default_monitor = audio.default_monitor();
                    inventory.mics = audio.mics;
                    inventory.monitors = audio.monitors;
                }
                Err(e) => inventory.warnings.push(format!("audio: {e}")),
            }
        }
        inventory
    }

    /// The fake backend's devices (A§8.11): two 1280×720 screens side by
    /// side, one camera, one mic and one system-audio monitor.
    pub fn fake() -> Self {
        // One definition, shared with the fake capture engine.
        let fake = crate::capture::fake_inventory();
        SourceInventory {
            screens: fake.screens,
            cameras: fake.cameras,
            mics: fake.mics,
            monitors: fake.monitors,
            default_mic: fake.default_mic,
            default_monitor: fake.default_monitor,
            warnings: Vec::new(),
        }
    }

    /// Keeps only the kinds `what` asks for.
    fn only(mut self, what: Discover) -> Self {
        if !what.screens {
            self.screens.clear();
        }
        if !what.cameras {
            self.cameras.clear();
        }
        if !what.audio {
            self.mics.clear();
            self.monitors.clear();
            self.default_mic = None;
            self.default_monitor = None;
        }
        self
    }

    /// The inventory as JSON (camelCase), e.g. for `qrec sources list --json`.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("a source inventory serializes")
    }

    /// `Facts::default_mic`.
    pub fn default_mic_info(&self) -> Option<AudioDeviceInfo> {
        let name = self.default_mic.as_deref()?;
        self.mics.iter().find(|d| d.name == name).cloned()
    }

    /// `Facts::default_monitor`.
    pub fn default_monitor_info(&self) -> Option<AudioDeviceInfo> {
        let name = self.default_monitor.as_deref()?;
        self.monitors.iter().find(|d| d.name == name).cloned()
    }

    /// `Facts::audio_devices`: mics, then monitors.
    pub fn audio_devices(&self) -> Vec<AudioDeviceInfo> {
        self.mics.iter().chain(&self.monitors).cloned().collect()
    }

    pub fn screen(&self, output: &str) -> Option<&ScreenInfo> {
        self.screens.iter().find(|s| s.output == output)
    }

    /// The modes of a camera (empty when unknown).
    pub fn camera_modes(&self, device: &str) -> Vec<CameraMode> {
        self.cameras
            .iter()
            .find(|c| c.device == device)
            .map(|c| c.modes.clone())
            .unwrap_or_default()
    }
}

/// The capture engine's device facts from a discovery snapshot.
impl crate::capture::DeviceInfo for SourceInventory {
    fn screens(&self) -> Vec<ScreenInfo> {
        self.screens.clone()
    }

    fn camera_modes(&self, device: &str) -> Vec<CameraMode> {
        SourceInventory::camera_modes(self, device)
    }
}

/// The capture engine's device facts, discovered when asked (screens can
/// come and go between takes); the fake inventory under the fake backend.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveDevices;

impl crate::capture::DeviceInfo for LiveDevices {
    fn screens(&self) -> Vec<ScreenInfo> {
        let what = Discover {
            screens: true,
            ..Discover::default()
        };
        SourceInventory::discover(what).screens
    }

    fn camera_modes(&self, device: &str) -> Vec<CameraMode> {
        let what = Discover {
            cameras: true,
            ..Discover::default()
        };
        SourceInventory::discover(what).camera_modes(device)
    }
}

// ---------------------------------------------------------------- tools

/// Runs a short tool (C locale, stderr captured) and returns its stdout.
fn run_tool(argv: &[&str], timeout: Duration) -> Result<String, String> {
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
    let program = argv[0].clone();
    let done = run_child(
        ChildSpec::new(&argv, ChildStderr::Capture)
            .env("LC_ALL", "C")
            .stdout(ChildStdout::Capture {
                limit: STDOUT_LIMIT,
            })
            .timeout(timeout),
    )?;
    if !done.exited_ok() {
        let mut message = done.describe(&program);
        if let Some(line) = done.stderr.lines().map(str::trim).rfind(|l| !l.is_empty()) {
            message.push_str(&format!(": {line}"));
        }
        return Err(message);
    }
    Ok(String::from_utf8_lossy(&done.stdout).into_owned())
}

// -------------------------------------------------------------- screens

/// `wl_output` v4, else `niri msg --json outputs`; sorted left to right.
pub fn discover_screens() -> Result<Vec<ScreenInfo>, String> {
    let wayland = wayland::outputs(WAYLAND_TIMEOUT);
    let mut screens = match wayland {
        Ok(screens) if !screens.is_empty() => screens,
        other => match niri_outputs() {
            Ok(screens) => screens,
            Err(niri) => match other {
                Ok(empty) => empty,
                Err(wayland) => return Err(format!("{wayland}; {niri}")),
            },
        },
    };
    screens.sort_by(|a, b| (a.x, a.y, &a.output).cmp(&(b.x, b.y, &b.output)));
    Ok(screens)
}

fn niri_outputs() -> Result<Vec<ScreenInfo>, String> {
    let json = run_tool(&["niri", "msg", "--json", "outputs"], TOOL_TIMEOUT)?;
    parse_niri_outputs(&json)
}

/// `niri msg --json outputs`: an object keyed by output name; disabled
/// outputs (no current mode or logical position) are skipped. The size is
/// the current mode's (physical pixels, rotated with the output).
pub fn parse_niri_outputs(json: &str) -> Result<Vec<ScreenInfo>, String> {
    let value: Value = serde_json::from_str(json)
        .map_err(|e| format!("niri printed something that isn't JSON: {e}"))?;
    let outputs = value
        .as_object()
        .ok_or("niri's output list isn't a JSON object")?;
    let mut screens = Vec::new();
    for (key, output) in outputs {
        let name = output["name"].as_str().unwrap_or(key).to_owned();
        let (Some(index), Some(logical)) = (
            output["current_mode"].as_u64(),
            output["logical"].as_object(),
        ) else {
            continue;
        };
        let mode = &output["modes"][index as usize];
        let (Some(w), Some(h)) = (mode["width"].as_u64(), mode["height"].as_u64()) else {
            continue;
        };
        let transform = logical
            .get("transform")
            .and_then(Value::as_str)
            .unwrap_or("Normal");
        let (width, height) = if transform.contains("90") || transform.contains("270") {
            (h, w)
        } else {
            (w, h)
        };
        let description = output_description(
            output["make"].as_str().unwrap_or(""),
            output["model"].as_str().unwrap_or(""),
            &name,
        );
        screens.push(ScreenInfo {
            output: name,
            description,
            x: logical.get("x").and_then(Value::as_i64).unwrap_or(0) as i32,
            y: logical.get("y").and_then(Value::as_i64).unwrap_or(0) as i32,
            width: width as u32,
            height: height as u32,
            scale: logical.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
        });
    }
    Ok(screens)
}

/// `Make - Model - Name`, the shape compositors give `wl_output` v4
/// descriptions (unknown parts left out).
fn output_description(make: &str, model: &str, name: &str) -> String {
    [make, model, name]
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "Unknown")
        .collect::<Vec<_>>()
        .join(" - ")
}

/// `wl_output` listing with a hard deadline: the socket is polled, never
/// read with a blocking roundtrip.
mod wayland {
    use std::io;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    use qrec_core::ctx::ScreenInfo;
    use wayland_client::protocol::{wl_callback, wl_output, wl_registry};
    use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};

    #[derive(Debug, Default)]
    struct Output {
        name: Option<String>,
        description: Option<String>,
        make: String,
        model: String,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        rotated: bool,
        scale: i32,
    }

    #[derive(Debug, Default)]
    struct State {
        outputs: Vec<Output>,
        synced: bool,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
                && interface == wl_output::WlOutput::interface().name
            {
                let index = state.outputs.len();
                registry.bind::<wl_output::WlOutput, usize, State>(name, version.min(4), qh, index);
                state.outputs.push(Output {
                    scale: 1,
                    ..Output::default()
                });
            }
        }
    }

    impl Dispatch<wl_output::WlOutput, usize> for State {
        fn event(
            state: &mut Self,
            _: &wl_output::WlOutput,
            event: wl_output::Event,
            index: &usize,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let Some(out) = state.outputs.get_mut(*index) else {
                return;
            };
            match event {
                wl_output::Event::Geometry {
                    x,
                    y,
                    make,
                    model,
                    transform,
                    ..
                } => {
                    out.x = x;
                    out.y = y;
                    out.make = make;
                    out.model = model;
                    out.rotated = matches!(
                        transform,
                        WEnum::Value(
                            wl_output::Transform::_90
                                | wl_output::Transform::_270
                                | wl_output::Transform::Flipped90
                                | wl_output::Transform::Flipped270
                        )
                    );
                }
                wl_output::Event::Mode {
                    flags: WEnum::Value(flags),
                    width,
                    height,
                    ..
                } if flags.contains(wl_output::Mode::Current) => {
                    out.width = width;
                    out.height = height;
                }
                wl_output::Event::Scale { factor } => out.scale = factor,
                wl_output::Event::Name { name } => out.name = Some(name),
                wl_output::Event::Description { description } => {
                    out.description = Some(description);
                }
                _ => {}
            }
        }
    }

    impl Dispatch<wl_callback::WlCallback, ()> for State {
        fn event(
            state: &mut Self,
            _: &wl_callback::WlCallback,
            event: wl_callback::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_callback::Event::Done { .. } = event {
                state.synced = true;
            }
        }
    }

    /// Every output with its v4 name. `Err` when there is no compositor,
    /// it is too slow, or its outputs have no names (version < 4).
    pub(super) fn outputs(timeout: Duration) -> Result<Vec<ScreenInfo>, String> {
        let deadline = Instant::now() + timeout;
        let conn = Connection::connect_to_env()
            .map_err(|e| format!("can't connect to the Wayland compositor: {e}"))?;
        let mut queue: EventQueue<State> = conn.new_event_queue();
        let qh = queue.handle();
        let display = conn.display();
        display.get_registry(&qh, ());
        let mut state = State::default();
        // The first sync delivers the globals (and the binds go out); the
        // second the outputs' events.
        for _ in 0..2 {
            state.synced = false;
            display.sync(&qh, ());
            roundtrip(&mut queue, &mut state, deadline)?;
        }
        let mut screens = Vec::new();
        for out in state.outputs {
            let Some(name) = out.name else {
                return Err("the compositor's wl_output has no names (version < 4)".to_owned());
            };
            let (width, height) = if out.rotated {
                (out.height, out.width)
            } else {
                (out.width, out.height)
            };
            let description = out
                .description
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| super::output_description(&out.make, &out.model, &name));
            screens.push(ScreenInfo {
                output: name,
                description,
                x: out.x,
                y: out.y,
                width: width.max(0) as u32,
                height: height.max(0) as u32,
                scale: f64::from(out.scale.max(1)),
            });
        }
        Ok(screens)
    }

    /// Dispatches until the pending sync is answered or the deadline passes.
    fn roundtrip(
        queue: &mut EventQueue<State>,
        state: &mut State,
        deadline: Instant,
    ) -> Result<(), String> {
        let wayland = |e: &dyn std::fmt::Display| format!("Wayland error: {e}");
        loop {
            queue.dispatch_pending(state).map_err(|e| wayland(&e))?;
            if state.synced {
                return Ok(());
            }
            queue.flush().map_err(|e| wayland(&e))?;
            let Some(guard) = queue.prepare_read() else {
                continue; // events arrived meanwhile: dispatch them first
            };
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("the Wayland compositor didn't answer within 2 s".to_owned());
            }
            let mut pfd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: one valid pollfd for the duration of the call.
            let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
            if rc < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(wayland(&e));
            }
            if rc == 0 {
                return Err("the Wayland compositor didn't answer within 2 s".to_owned());
            }
            guard.read().map_err(|e| wayland(&e))?;
        }
    }
}

// -------------------------------------------------------------- cameras

/// The cameras (GStreamer's DeviceMonitor with modes, else sysfs without
/// them). Shaped like the facts' discovery hooks: never `Err` in practice,
/// since sysfs always answers; a GStreamer problem is logged.
pub fn discover_cameras() -> Result<Vec<CameraInfo>, String> {
    let (cameras, warning) = cameras_and_warning();
    if let Some(warning) = warning {
        tracing::warn!("cameras: {warning}");
    }
    Ok(cameras)
}

/// GStreamer's DeviceMonitor (with modes), else sysfs. Returns the cameras
/// and, when something failed, why.
#[cfg(feature = "gst")]
fn cameras_and_warning() -> (Vec<CameraInfo>, Option<String>) {
    match gst_cameras(CAMERA_TIMEOUT) {
        Ok(cameras) => (cameras, None),
        Err(e) => (sysfs_cameras(), Some(format!("{e}; listed without modes"))),
    }
}

/// Without GStreamer: the `video4linux` capture nodes, without modes.
#[cfg(not(feature = "gst"))]
fn cameras_and_warning() -> (Vec<CameraInfo>, Option<String>) {
    (sysfs_cameras(), None)
}

fn sysfs_cameras() -> Vec<CameraInfo> {
    scan_video4linux(Path::new("/sys/class/video4linux"), Path::new("/dev"))
}

/// Capture nodes in a `video4linux` sysfs dir: `videoN` entries whose
/// `index` is 0 (other indexes are metadata nodes). Nothing is opened.
pub fn scan_video4linux(sys: &Path, dev: &Path) -> Vec<CameraInfo> {
    let Ok(entries) = fs::read_dir(sys) else {
        return Vec::new();
    };
    let mut cameras: Vec<(u32, CameraInfo)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(number) = name
            .strip_prefix("video")
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let path = entry.path();
        let index = fs::read_to_string(path.join("index")).unwrap_or_default();
        if !index.trim().is_empty() && index.trim() != "0" {
            continue;
        }
        let label = fs::read_to_string(path.join("name"))
            .map(|s| s.trim().to_owned())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| name.clone());
        cameras.push((
            number,
            CameraInfo {
                device: dev.join(&name).to_string_lossy().into_owned(),
                label,
                modes: Vec::new(),
            },
        ));
    }
    cameras.sort_by_key(|(n, _)| *n);
    cameras.into_iter().map(|(_, c)| c).collect()
}

#[cfg(feature = "gst")]
fn gst_cameras(timeout: Duration) -> Result<Vec<CameraInfo>, String> {
    use gst::prelude::*;

    init_gstreamer(GST_INIT_TIMEOUT)?;
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("qrec-camera-monitor".into())
        .spawn(move || {
            let monitor = gst::DeviceMonitor::new();
            let _ = monitor.add_filter(Some("Video/Source"), None);
            let result = match monitor.start() {
                Ok(()) => {
                    let devices: Vec<CameraInfo> =
                        monitor.devices().iter().filter_map(camera_info).collect();
                    monitor.stop();
                    Ok(devices)
                }
                Err(e) => Err(format!("GStreamer's device monitor didn't start: {e}")),
            };
            let _ = tx.send(result);
        })
        .map_err(|e| format!("can't start the camera discovery thread: {e}"))?;
    let mut cameras = rx
        .recv_timeout(timeout)
        .map_err(|_| "GStreamer's device monitor didn't answer in time".to_owned())??;
    cameras.sort_by(|a, b| a.device.cmp(&b.device));
    cameras.dedup_by(|a, b| a.device == b.device);
    Ok(cameras)
}

/// A `Video/Source` device with a `/dev/video*` node.
#[cfg(feature = "gst")]
fn camera_info(device: &gst::Device) -> Option<CameraInfo> {
    use gst::prelude::*;

    let props = device.properties()?;
    let text = |key: &str| props.get::<String>(key).ok();
    let path = text("api.v4l2.path")
        .or_else(|| text("device.path"))
        .or_else(|| text("object.path").and_then(|p| p.strip_prefix("v4l2:").map(str::to_owned)))?;
    if !path.starts_with("/dev/video") {
        return None;
    }
    let modes = device
        .caps()
        .map(|caps| caps_modes(&caps))
        .unwrap_or_default();
    Some(CameraInfo {
        device: path,
        label: device.display_name().to_string(),
        modes,
    })
}

/// Fixed raw/MJPEG sizes with every listed frame rate. DMA-BUF variants of
/// the raw modes (`DMA_DRM`, `memory:DMABuf`) are left out: the camera
/// branch reads system memory, and they only repeat the raw modes.
#[cfg(feature = "gst")]
fn caps_modes(caps: &gst::Caps) -> Vec<CameraMode> {
    let mut modes = Vec::new();
    for (s, features) in caps.iter_with_features() {
        if features.contains("memory:DMABuf") {
            continue;
        }
        let format = match s.name().as_str() {
            "video/x-raw" => match s.get::<String>("format") {
                Ok(format) if format == "DMA_DRM" => continue,
                Ok(format) => format,
                Err(_) => continue,
            },
            "image/jpeg" => "image/jpeg".to_owned(),
            _ => continue,
        };
        let (Ok(width), Ok(height)) = (s.get::<i32>("width"), s.get::<i32>("height")) else {
            continue;
        };
        let mut rates: Vec<gst::Fraction> = Vec::new();
        if let Ok(rate) = s.get::<gst::Fraction>("framerate") {
            rates.push(rate);
        } else if let Ok(list) = s.get::<gst::List>("framerate") {
            rates.extend(list.iter().filter_map(|v| v.get::<gst::Fraction>().ok()));
        } else if let Ok(range) = s.get::<gst::FractionRange>("framerate") {
            rates.push(range.max());
        }
        for rate in rates {
            let (num, den) = (rate.numer(), rate.denom());
            if num > 0 && den > 0 && width > 0 && height > 0 {
                modes.push(CameraMode {
                    width: width as u32,
                    height: height as u32,
                    fps: qrec_core::Fps::new(num as u32, den as u32),
                    format: format.clone(),
                });
            }
        }
    }
    modes.sort_by(|a, b| {
        let (fa, fb) = (
            u64::from(a.fps.num) * u64::from(b.fps.den),
            u64::from(b.fps.num) * u64::from(a.fps.den),
        );
        (a.width, a.height, fa, &a.format).cmp(&(b.width, b.height, fb, &b.format))
    });
    modes.dedup();
    modes
}

/// `gst::init()` under the shared init watchdog (`gst_init`, A§8.10): at
/// most `timeout`, then [`GST_STUCK`].
#[cfg(feature = "gst")]
pub fn init_gstreamer(timeout: Duration) -> Result<(), String> {
    crate::gst_init::init_with_timeout(timeout).map_err(|e| e.to_string())
}

/// What a stuck `gst::init()` means (A§8.10; `gst_init::STUCK_MESSAGE`,
/// also available without the `gst` feature).
pub const GST_STUCK: &str =
    "GStreamer is stuck scanning plugins (a GPU driver is probably hung); see `qrec doctor`";

// ---------------------------------------------------------------- audio

/// PulseAudio/PipeWire sources and the server's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioDiscovery {
    pub mics: Vec<AudioDeviceInfo>,
    pub monitors: Vec<AudioDeviceInfo>,
    pub default_source: Option<String>,
    pub default_sink: Option<String>,
}

impl AudioDiscovery {
    /// The default source when it is a mic, else the first mic.
    pub fn default_mic(&self) -> Option<String> {
        self.default_source
            .as_ref()
            .filter(|name| self.mics.iter().any(|m| &m.name == *name))
            .cloned()
            .or_else(|| self.mics.first().map(|m| m.name.clone()))
    }

    /// The default sink's monitor, else the first monitor.
    pub fn default_monitor(&self) -> Option<String> {
        self.default_sink
            .as_ref()
            .map(|sink| format!("{sink}.monitor"))
            .filter(|name| self.monitors.iter().any(|m| &m.name == name))
            .or_else(|| self.monitors.first().map(|m| m.name.clone()))
    }
}

/// `pactl --format=json list sources` and `pactl --format=json info`.
pub fn discover_audio() -> Result<AudioDiscovery, String> {
    let sources = run_tool(&["pactl", "--format=json", "list", "sources"], TOOL_TIMEOUT)?;
    let (mics, monitors) = parse_pactl_sources(&sources)?;
    let info = run_tool(&["pactl", "--format=json", "info"], TOOL_TIMEOUT)?;
    let (default_source, default_sink) = parse_pactl_defaults(&info);
    Ok(AudioDiscovery {
        mics,
        monitors,
        default_source,
        default_sink,
    })
}

/// Mics (`alsa_input.*`) and monitors (`*.monitor`) from `pactl --format=json
/// list sources`. pactl prints `(null)` for descriptions it can't encode
/// (non-ASCII), so the description falls back to the device's properties.
pub fn parse_pactl_sources(
    json: &str,
) -> Result<(Vec<AudioDeviceInfo>, Vec<AudioDeviceInfo>), String> {
    let value: Value = serde_json::from_str(json)
        .map_err(|e| format!("pactl printed something that isn't JSON: {e}"))?;
    let sources = value
        .as_array()
        .ok_or("pactl's source list isn't a JSON array")?;
    let (mut mics, mut monitors) = (Vec::new(), Vec::new());
    for source in sources {
        let Some(name) = source["name"].as_str() else {
            continue;
        };
        let is_monitor = name.ends_with(".monitor");
        if !is_monitor && !name.starts_with("alsa_input.") {
            continue;
        }
        let device = AudioDeviceInfo {
            name: name.to_owned(),
            description: description(source, name, is_monitor),
            is_monitor,
        };
        if is_monitor {
            monitors.push(device);
        } else {
            mics.push(device);
        }
    }
    Ok((mics, monitors))
}

fn description(source: &Value, name: &str, is_monitor: bool) -> String {
    let usable = |v: &Value| {
        v.as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "(null)")
            .map(str::to_owned)
    };
    if let Some(d) = usable(&source["description"]) {
        return d;
    }
    let props = &source["properties"];
    let base = [
        "device.description",
        "node.nick",
        "device.product.name",
        "alsa.card_name",
    ]
    .iter()
    .find_map(|key| usable(&props[*key]))
    .unwrap_or_else(|| name.to_owned());
    if is_monitor {
        format!("Monitor of {base}")
    } else {
        base
    }
}

/// `(default_source_name, default_sink_name)` from `pactl --format=json info`.
pub fn parse_pactl_defaults(json: &str) -> (Option<String>, Option<String>) {
    let value: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    let text = |key: &str| {
        value[key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "(null)")
            .map(str::to_owned)
    };
    (text("default_source_name"), text("default_sink_name"))
}

/// The PulseAudio server name (`PulseAudio (on PipeWire 1.6.6)`).
pub fn pulse_server_name(info_json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(info_json).ok()?;
    value["server_name"].as_str().map(str::to_owned)
}

/// Where `program` is on `PATH`.
pub fn find_program(program: &str) -> Option<PathBuf> {
    which::which(program).ok()
}

pub(crate) fn tool_output(argv: &[&str], timeout: Duration) -> Result<String, String> {
    run_tool(argv, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v4l2src lists DMA-BUF copies of the raw modes (seen on a Dell
    /// webcam and a Brio); they would win a raw/raw tie in
    /// `choose_camera_mode` and then fail to negotiate.
    #[cfg(feature = "gst")]
    #[test]
    fn dma_buf_camera_modes_are_left_out() {
        use std::str::FromStr;
        gst::init().unwrap();
        let caps = gst::Caps::from_str(
            "video/x-raw, format=YUY2, width=640, height=480, framerate=30/1; \
             video/x-raw(memory:DMABuf), format=DMA_DRM, drm-format=YUYV, width=640, height=480, framerate=30/1; \
             video/x-raw, format=DMA_DRM, width=1920, height=1080, framerate=5/1; \
             image/jpeg, width=1920, height=1080, framerate=30/1",
        )
        .unwrap();
        let formats: Vec<String> = caps_modes(&caps).into_iter().map(|m| m.format).collect();
        assert_eq!(formats, ["YUY2", "image/jpeg"]);
    }

    #[test]
    fn the_fake_inventory_matches_the_architecture() {
        let fake = SourceInventory::fake();
        let outputs: Vec<&str> = fake.screens.iter().map(|s| s.output.as_str()).collect();
        assert_eq!(outputs, ["FAKE-1", "FAKE-2"]);
        // What the capture engine sees through `DeviceInfo`.
        let devices: &dyn crate::capture::DeviceInfo = &fake;
        assert_eq!(devices.screens(), fake.screens);
        assert_eq!(devices.camera_modes("/dev/fake0").len(), 3);
        assert!(
            fake.screens
                .iter()
                .all(|s| (s.width, s.height) == (1280, 720))
        );
        assert_eq!(fake.screens[1].x, 1280);
        assert_eq!(fake.cameras[0].device, "/dev/fake0");
        assert_eq!(fake.camera_modes("/dev/fake0").len(), 3);
        assert!(fake.camera_modes("/dev/video9").is_empty());
        assert_eq!(fake.default_mic_info().unwrap().name, "alsa_input.fake");
        let monitor = fake.default_monitor_info().unwrap();
        assert_eq!(
            (monitor.name.as_str(), monitor.is_monitor),
            ("alsa_output.fake.monitor", true)
        );
        assert_eq!(fake.audio_devices().len(), 2);
        assert_eq!(fake.screen("FAKE-2").unwrap().description, "Fake screen 2");

        let audio = fake.clone().only(Discover::AUDIO);
        assert!(audio.screens.is_empty() && audio.cameras.is_empty());
        assert_eq!(audio.mics.len(), 1);
        let video = fake.only(Discover::VIDEO);
        assert!(video.mics.is_empty() && video.default_mic.is_none());
        assert_eq!(video.screens.len(), 2);
    }

    #[test]
    fn the_inventory_serializes_in_camel_case() {
        let fake = SourceInventory::fake().only(Discover::ALL);
        assert_eq!(fake, SourceInventory::fake());
        let json = fake.to_json();
        assert_eq!(json["screens"][0]["output"], "FAKE-1");
        assert_eq!(json["defaultMic"], "alsa_input.fake");
        assert!(json.get("warnings").is_none(), "no warnings key when empty");
    }

    #[test]
    fn pactl_sources_split_into_mics_and_monitors() {
        let json = r#"[
          {"name":"alsa_output.pci-0000.HiFi__HDMI1__sink.monitor","description":"Monitor of HDMI 1",
           "properties":{}},
          {"name":"alsa_input.pci-0000.HiFi__Mic__source","description":"SoundWire microphones",
           "properties":{"device.description":"Arrow Lake"}},
          {"name":"alsa_input.usb-Yeti.analog-stereo","description":"(null)",
           "properties":{"device.description":"Yeti Nano","node.nick":"Yeti"}},
          {"name":"alsa_output.usb-Yeti.analog-stereo.monitor","description":"(null)",
           "properties":{"device.description":"Yeti Nano"}},
          {"name":"bluez_input.00_11","description":"Headset","properties":{}},
          {"name":"alsa_input.weird","properties":{}}
        ]"#;
        let (mics, monitors) = parse_pactl_sources(json).unwrap();
        let names = |v: &[AudioDeviceInfo]| {
            v.iter()
                .map(|d| (d.name.clone(), d.description.clone(), d.is_monitor))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&mics),
            [
                (
                    "alsa_input.pci-0000.HiFi__Mic__source".into(),
                    "SoundWire microphones".into(),
                    false
                ),
                (
                    "alsa_input.usb-Yeti.analog-stereo".into(),
                    "Yeti Nano".into(),
                    false
                ),
                ("alsa_input.weird".into(), "alsa_input.weird".into(), false),
            ]
        );
        assert_eq!(
            names(&monitors),
            [
                (
                    "alsa_output.pci-0000.HiFi__HDMI1__sink.monitor".into(),
                    "Monitor of HDMI 1".into(),
                    true
                ),
                (
                    "alsa_output.usb-Yeti.analog-stereo.monitor".into(),
                    "Monitor of Yeti Nano".into(),
                    true
                ),
            ]
        );
        assert!(parse_pactl_sources("{oops").is_err());
        assert!(parse_pactl_sources("{}").is_err());
    }

    #[test]
    fn pactl_defaults_pick_mics_and_the_sink_monitor() {
        let info = r#"{"server_name":"PulseAudio (on PipeWire 1.6.6)",
                       "default_sink_name":"alsa_output.usb-Yeti.analog-stereo",
                       "default_source_name":"alsa_output.pci.monitor"}"#;
        let (source, sink) = parse_pactl_defaults(info);
        assert_eq!(source.as_deref(), Some("alsa_output.pci.monitor"));
        assert_eq!(
            pulse_server_name(info).unwrap(),
            "PulseAudio (on PipeWire 1.6.6)"
        );
        let device = |name: &str, is_monitor| AudioDeviceInfo {
            name: name.into(),
            description: name.into(),
            is_monitor,
        };
        let audio = AudioDiscovery {
            mics: vec![device("alsa_input.a", false), device("alsa_input.b", false)],
            monitors: vec![
                device("alsa_output.pci.monitor", true),
                device("alsa_output.usb-Yeti.analog-stereo.monitor", true),
            ],
            default_source: source,
            default_sink: sink,
        };
        // The default source is a monitor, so the first mic is the default mic.
        assert_eq!(audio.default_mic().as_deref(), Some("alsa_input.a"));
        assert_eq!(
            audio.default_monitor().as_deref(),
            Some("alsa_output.usb-Yeti.analog-stereo.monitor")
        );
        let audio = AudioDiscovery {
            default_source: Some("alsa_input.b".into()),
            default_sink: None,
            ..audio
        };
        assert_eq!(audio.default_mic().as_deref(), Some("alsa_input.b"));
        assert_eq!(
            audio.default_monitor().as_deref(),
            Some("alsa_output.pci.monitor")
        );
        assert_eq!(AudioDiscovery::default().default_mic(), None);
        assert_eq!(parse_pactl_defaults("not json"), (None, None));
    }

    #[test]
    fn niri_outputs_are_parsed() {
        let json = r#"{
          "DP-3": {"name":"DP-3","make":"LG Electronics","model":"LG ULTRAWIDE","serial":"x",
                   "modes":[{"width":2560,"height":1080,"refresh_rate":60000,"is_preferred":true}],
                   "current_mode":0,
                   "logical":{"x":4480,"y":360,"width":2560,"height":1080,"scale":1.0,"transform":"Normal"}},
          "eDP-1": {"name":"eDP-1","make":"BOE","model":"0x0CC0",
                    "modes":[{"width":1920,"height":1200,"refresh_rate":60000,"is_preferred":true}],
                    "current_mode":0,
                    "logical":{"x":0,"y":0,"width":1200,"height":1920,"scale":1.5,"transform":"90"}},
          "HDMI-A-1": {"name":"HDMI-A-1","make":"Unknown","model":"Unknown","modes":[],
                       "current_mode":null,"logical":null}
        }"#;
        let screens = parse_niri_outputs(json).unwrap();
        assert_eq!(screens.len(), 2, "the disabled output is skipped");
        let dp = screens.iter().find(|s| s.output == "DP-3").unwrap();
        assert_eq!(
            (dp.x, dp.y, dp.width, dp.height, dp.scale),
            (4480, 360, 2560, 1080, 1.0)
        );
        assert_eq!(dp.description, "LG Electronics - LG ULTRAWIDE - DP-3");
        assert_eq!(output_description("Unknown", " ", "HDMI-A-1"), "HDMI-A-1");
        let edp = screens.iter().find(|s| s.output == "eDP-1").unwrap();
        assert_eq!((edp.width, edp.height, edp.scale), (1200, 1920, 1.5));
        assert!(parse_niri_outputs("[]").is_err());
    }

    #[test]
    fn video4linux_lists_capture_nodes_only() {
        let sys = tempfile::tempdir().unwrap();
        for (node, name, index) in [
            ("video0", "Integrated_Webcam_FHD", "0"),
            ("video1", "Integrated_Webcam_FHD", "1"),
            ("video10", "Brio 500", "0"),
            ("video2", "IR camera", "0"),
        ] {
            let dir = sys.path().join(node);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("name"), format!("{name}\n")).unwrap();
            fs::write(dir.join("index"), format!("{index}\n")).unwrap();
        }
        fs::create_dir_all(sys.path().join("v4l-subdev0")).unwrap();
        let cameras = scan_video4linux(sys.path(), Path::new("/dev"));
        let found: Vec<(&str, &str)> = cameras
            .iter()
            .map(|c| (c.device.as_str(), c.label.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("/dev/video0", "Integrated_Webcam_FHD"),
                ("/dev/video2", "IR camera"),
                ("/dev/video10", "Brio 500"),
            ]
        );
        assert!(cameras.iter().all(|c| c.modes.is_empty()));
        assert!(scan_video4linux(&sys.path().join("missing"), Path::new("/dev")).is_empty());
    }

    #[test]
    fn the_stuck_message_is_gst_inits() {
        #[cfg(feature = "gst")]
        assert_eq!(GST_STUCK, crate::gst_init::STUCK_MESSAGE);
        assert!(GST_STUCK.contains("stuck scanning plugins"));
    }

    #[test]
    fn missing_tools_are_reported() {
        let err = run_tool(&["qrec-no-such-pactl", "info"], TOOL_TIMEOUT).unwrap_err();
        assert!(err.contains("wasn't found"), "{err}");
        let err = run_tool(&["sh", "-c", "echo nope >&2; exit 2"], TOOL_TIMEOUT).unwrap_err();
        assert_eq!(err, "sh exited with status 2: nope");
        assert_eq!(
            run_tool(&["sh", "-c", "echo $LC_ALL"], TOOL_TIMEOUT).unwrap(),
            "C\n"
        );
    }

    /// Lists this machine's real screens (read-only Wayland/niri queries).
    /// Run by hand: `cargo test -p qrec-shell real_screens -- --ignored --nocapture`.
    #[test]
    #[ignore = "talks to the real compositor"]
    fn real_screens() {
        println!("{:#?}", wayland::outputs(WAYLAND_TIMEOUT));
        println!("{:#?}", niri_outputs());
        println!("{:#?}", discover_screens());
    }
}
