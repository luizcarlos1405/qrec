//! The fake capture backend's devices (`QREC_CAPTURE_BACKEND=fake`, A§8.11).
//!
//! Discovery under the fake backend returns exactly these devices (the
//! `sources` module and `Facts` use [`fake_inventory`]); the engine turns
//! them into `videotestsrc`/`audiotestsrc` sources and never touches the
//! portal, cameras or PulseAudio.
//!
//! **Test hooks** ([`FakeHooks`], docs/GUI.md G§8.4), read from the
//! environment and honored only with the fake backend:
//!
//! - `QREC_FAKE_EOS_DELAY_MS=<ms>`: a stopped take's files close that much
//!   later, so it stays finalizing (S05); past its EOS timeout it ends
//!   `timed_out`, like a take whose files didn't close.
//! - `QREC_FAKE_PORTAL=deny|cancel`: the fake screens go through a fake
//!   ScreenCast portal that refuses them (denied, or the dialog closed),
//!   answering like the real one (S13b, S24).
//! - `QREC_FAKE_RECORD_OFFSET_FRAMES=<n>`: every `RecordClock` reads `n`
//!   frames more, as if the take started earlier (S04's timer and ghost).
//! - `QREC_FAKE_INVENTORY=design`: the fake devices carry the design
//!   brief's names and sizes (BRIEF §3); their output names, device paths
//!   and test patterns stay the same.

use std::time::Duration;

use qrec_core::Fps;
use qrec_core::ctx::{AudioDeviceInfo, CameraInfo, CameraMode, ScreenInfo};

/// `QREC_FAKE_EOS_DELAY_MS` ([`FakeHooks::eos_delay`]).
pub const FAKE_EOS_DELAY_ENV: &str = "QREC_FAKE_EOS_DELAY_MS";
/// `QREC_FAKE_PORTAL` ([`FakeHooks::portal`]).
pub const FAKE_PORTAL_ENV: &str = "QREC_FAKE_PORTAL";
/// `QREC_FAKE_RECORD_OFFSET_FRAMES` ([`FakeHooks::record_offset_frames`]).
pub const FAKE_RECORD_OFFSET_ENV: &str = "QREC_FAKE_RECORD_OFFSET_FRAMES";
/// `QREC_FAKE_INVENTORY` ([`FakeHooks::inventory`]).
pub const FAKE_INVENTORY_ENV: &str = "QREC_FAKE_INVENTORY";

/// The fake backend's test hooks (docs/GUI.md G§8.4). The engine takes them
/// from `EngineOptions::fake`, which [`FakeHooks::from_env`] fills for the
/// fake backend; the real backend never looks at them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FakeHooks {
    /// How much later than its EOS a stopped take's files count as closed.
    pub eos_delay: Option<Duration>,
    /// The fake screens are refused through a fake portal.
    pub portal: Option<FakePortal>,
    /// Frames added to every `RecordClock`'s elapsed time.
    pub record_offset_frames: u64,
    /// Which names and sizes the fake devices carry.
    pub inventory: FakeInventoryKind,
}

/// How the fake portal answers a screen (`QREC_FAKE_PORTAL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakePortal {
    /// `deny`: sharing is denied.
    Deny,
    /// `cancel`: the screen-share dialog is closed.
    Cancel,
}

/// Which devices the fake backend discovers (`QREC_FAKE_INVENTORY`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FakeInventoryKind {
    /// "Fake screen 1" and the rest at 1280×720 (A§8.11).
    #[default]
    Plain,
    /// `design`: the brief's monitors, camera, mic and system audio (BRIEF §3).
    Design,
}

impl FakeHooks {
    /// The hooks this process's environment sets.
    pub fn from_env() -> Self {
        Self::parse(|key| std::env::var(key).ok())
    }

    /// The hooks `get` (an environment lookup) sets. A value that doesn't
    /// parse is ignored with a warning.
    pub fn parse(get: impl Fn(&str) -> Option<String>) -> Self {
        let value = |key: &str| {
            get(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let number = |key: &str| {
            value(key).and_then(|v| match v.parse::<u64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    tracing::warn!("{key}={v:?} isn't a whole number; ignored");
                    None
                }
            })
        };
        let portal = value(FAKE_PORTAL_ENV).and_then(|v| match v.as_str() {
            "deny" => Some(FakePortal::Deny),
            "cancel" => Some(FakePortal::Cancel),
            other => {
                tracing::warn!("{FAKE_PORTAL_ENV}={other:?} isn't deny or cancel; ignored");
                None
            }
        });
        Self {
            eos_delay: number(FAKE_EOS_DELAY_ENV).map(Duration::from_millis),
            portal,
            record_offset_frames: number(FAKE_RECORD_OFFSET_ENV).unwrap_or(0),
            inventory: FakeInventoryKind::parse(value(FAKE_INVENTORY_ENV).as_deref()),
        }
    }

    /// [`FakeHooks::record_offset_frames`] in nanoseconds at `fps`.
    pub fn record_offset_ns(&self, fps: Fps) -> u64 {
        if fps.num == 0 {
            return 0;
        }
        let ns = u128::from(self.record_offset_frames) * 1_000_000_000 * u128::from(fps.den)
            / u128::from(fps.num);
        u64::try_from(ns).unwrap_or(u64::MAX)
    }
}

impl FakePortal {
    /// Why the fake portal didn't share `output`, worded like the real
    /// portal's answers (`capture::portal`).
    pub fn refusal(self, output: &str) -> String {
        let why = match self {
            FakePortal::Deny => "the desktop portal denied access to the screen",
            FakePortal::Cancel => "screen sharing was cancelled in the dialog",
        };
        format!("screen sharing for {output} failed: {why}")
    }
}

impl FakeInventoryKind {
    /// `QREC_FAKE_INVENTORY` of this process.
    pub fn from_env() -> Self {
        Self::parse(std::env::var(FAKE_INVENTORY_ENV).ok().as_deref())
    }

    fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("") => FakeInventoryKind::Plain,
            Some("design") => FakeInventoryKind::Design,
            Some(other) => {
                tracing::warn!("{FAKE_INVENTORY_ENV}={other:?} isn't design; ignored");
                FakeInventoryKind::Plain
            }
        }
    }
}

/// The fake screens' output names: `FAKE-1` is a still test card,
/// `FAKE-2` a moving ball ([`fake_screen_is_still`]).
pub const FAKE_SCREENS: [&str; 2] = ["FAKE-1", "FAKE-2"];
/// Every fake screen's size.
pub const FAKE_SCREEN_SIZE: (u32, u32) = (1280, 720);
/// The rate a moving fake screen produces frames at (the record bin's
/// `videorate` turns it into the canvas rate).
pub const FAKE_SCREEN_FPS: Fps = Fps::new(60, 1);

/// Whether a fake screen shows a still picture. Like a damage-driven
/// compositor, it sends that frame once; the screen bridge's keepalive
/// sends every later one (A§8.11). Every fake screen but `FAKE-2` is still.
pub fn fake_screen_is_still(output: &str) -> bool {
    !output.ends_with('2')
}
/// The fake camera.
pub const FAKE_CAMERA: &str = "/dev/fake0";
/// The fake mic (an `alsa_input.*` source).
pub const FAKE_MIC: &str = "alsa_input.fake";
/// The fake system-audio monitor.
pub const FAKE_MONITOR: &str = "alsa_output.fake.monitor";

/// A fake audio source: a tone that sounds for `tone_ms` at the start of
/// every `period_ms`, digital silence otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeTone {
    pub channels: u32,
    pub freq_hz: u32,
    pub tone_ms: u32,
    pub period_ms: u32,
}

/// The fake mic: mono 440 Hz, 0.5 s of tone every 1.5 s (1 s silences, long
/// enough for autocut's 400 ms default).
pub const FAKE_MIC_TONE: FakeTone = FakeTone {
    channels: 1,
    freq_hz: 440,
    tone_ms: 500,
    period_ms: 1500,
};

/// The fake monitor: stereo 660 Hz, 0.75 s of tone every 2 s.
pub const FAKE_MONITOR_TONE: FakeTone = FakeTone {
    channels: 2,
    freq_hz: 660,
    tone_ms: 750,
    period_ms: 2000,
};

/// The tone of a fake audio device (unknown names behave like the mic).
pub fn fake_tone(device: &str) -> FakeTone {
    if device.ends_with(".monitor") {
        FAKE_MONITOR_TONE
    } else {
        FAKE_MIC_TONE
    }
}

/// The modes the fake camera offers.
pub fn fake_camera_modes() -> Vec<CameraMode> {
    let mode = |width, height, fps, format: &str| CameraMode {
        width,
        height,
        fps: Fps::new(fps, 1),
        format: format.to_owned(),
    };
    vec![
        mode(640, 480, 30, "YUY2"),
        mode(640, 480, 60, "YUY2"),
        mode(1280, 720, 30, "image/jpeg"),
    ]
}

/// Everything the fake backend "discovers" (mirrors `sources::SourceInventory`).
#[derive(Debug, Clone, PartialEq)]
pub struct FakeInventory {
    pub screens: Vec<ScreenInfo>,
    pub cameras: Vec<CameraInfo>,
    pub mics: Vec<AudioDeviceInfo>,
    pub monitors: Vec<AudioDeviceInfo>,
    pub default_mic: Option<String>,
    pub default_monitor: Option<String>,
}

/// Screens `FAKE-1`, `FAKE-2` (1280×720, side by side), camera
/// `/dev/fake0`, mic `alsa_input.fake`, monitor `alsa_output.fake.monitor`;
/// with `QREC_FAKE_INVENTORY=design`, the brief's names and sizes
/// ([`fake_inventory_of`]).
pub fn fake_inventory() -> FakeInventory {
    fake_inventory_of(FakeInventoryKind::from_env())
}

/// The design brief's devices (BRIEF §3.1): the monitors' names and sizes,
/// then the camera's, the mic's and system audio's names.
const DESIGN_SCREENS: [(&str, (u32, u32)); 2] = [
    ("GIGA-BYTE GS27QA", (2560, 1440)),
    ("Dell U2720Q", (3840, 2160)),
];
const DESIGN_CAMERA: &str = "Logitech C920";
const DESIGN_MIC: &str = "Yeti Nano";
const DESIGN_MONITOR: &str = "Built-in Audio Analog Stereo";

/// [`fake_inventory`] of one kind. The design kind only renames and
/// resizes what discovery reports: output names, device paths, camera
/// modes and the engine's test patterns (1280×720, the same 16:9) stay.
pub fn fake_inventory_of(kind: FakeInventoryKind) -> FakeInventory {
    let design = kind == FakeInventoryKind::Design;
    let mut x = 0;
    let screens = FAKE_SCREENS
        .iter()
        .enumerate()
        .map(|(i, output)| {
            let (description, (width, height)) = if design {
                (DESIGN_SCREENS[i].0.to_owned(), DESIGN_SCREENS[i].1)
            } else {
                (format!("Fake screen {}", i + 1), FAKE_SCREEN_SIZE)
            };
            let screen = ScreenInfo {
                output: (*output).to_owned(),
                description,
                x,
                y: 0,
                width,
                height,
                scale: 1.0,
            };
            x += width as i32;
            screen
        })
        .collect();
    let name = |plain: &str, designed: &str| if design { designed } else { plain }.to_owned();
    FakeInventory {
        screens,
        cameras: vec![CameraInfo {
            device: FAKE_CAMERA.to_owned(),
            label: name("Fake camera", DESIGN_CAMERA),
            modes: fake_camera_modes(),
        }],
        mics: vec![AudioDeviceInfo {
            name: FAKE_MIC.to_owned(),
            description: name("Fake mic", DESIGN_MIC),
            is_monitor: false,
        }],
        monitors: vec![AudioDeviceInfo {
            name: FAKE_MONITOR.to_owned(),
            description: name("Fake system audio", DESIGN_MONITOR),
            is_monitor: true,
        }],
        default_mic: Some(FAKE_MIC.to_owned()),
        default_monitor: Some(FAKE_MONITOR.to_owned()),
    }
}

/// Whether `output` is a fake screen.
pub fn is_fake_screen(output: &str) -> bool {
    FAKE_SCREENS.contains(&output)
}

/// Whether `device` is a fake camera (`/dev/fake<N>`).
pub fn is_fake_camera(device: &str) -> bool {
    device
        .strip_prefix("/dev/fake")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `device` is a fake audio source.
pub fn is_fake_audio(device: &str) -> bool {
    device == FAKE_MIC || device == FAKE_MONITOR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_matches_the_architecture() {
        let inv = fake_inventory_of(FakeInventoryKind::Plain);
        let outputs: Vec<&str> = inv.screens.iter().map(|s| s.output.as_str()).collect();
        assert_eq!(outputs, ["FAKE-1", "FAKE-2"]);
        assert!(
            inv.screens
                .iter()
                .all(|s| (s.width, s.height) == (1280, 720))
        );
        assert_eq!(inv.screens[1].x, 1280);
        assert_eq!(inv.cameras[0].device, "/dev/fake0");
        assert_eq!(inv.mics[0].name, "alsa_input.fake");
        assert!(!inv.mics[0].is_monitor);
        assert_eq!(inv.monitors[0].name, "alsa_output.fake.monitor");
        assert!(inv.monitors[0].is_monitor);
        assert_eq!(inv.default_mic.as_deref(), Some(FAKE_MIC));
        assert_eq!(inv.default_monitor.as_deref(), Some(FAKE_MONITOR));
    }

    #[test]
    fn fake_device_names() {
        assert!(is_fake_screen("FAKE-2") && !is_fake_screen("DP-4"));
        assert!(is_fake_camera("/dev/fake0") && is_fake_camera("/dev/fake12"));
        assert!(!is_fake_camera("/dev/fake") && !is_fake_camera("/dev/video0"));
        assert!(is_fake_audio(FAKE_MIC) && is_fake_audio(FAKE_MONITOR));
        assert!(!is_fake_audio("alsa_input.usb-yeti"));
        assert_eq!(fake_tone(FAKE_MONITOR).channels, 2);
        assert_eq!(fake_tone(FAKE_MIC), FAKE_MIC_TONE);
        // Silences are longer than autocut's default minimum.
        for tone in [FAKE_MIC_TONE, FAKE_MONITOR_TONE] {
            assert!(tone.period_ms - tone.tone_ms >= 400);
        }
    }

    /// `QREC_FAKE_INVENTORY=design`: the brief's names and sizes on the
    /// same fake devices (G§8.4).
    #[test]
    fn the_design_inventory_carries_the_briefs_labels() {
        let inv = fake_inventory_of(FakeInventoryKind::Design);
        let screens: Vec<(&str, &str, u32, u32, i32)> = inv
            .screens
            .iter()
            .map(|s| {
                (
                    s.output.as_str(),
                    s.description.as_str(),
                    s.width,
                    s.height,
                    s.x,
                )
            })
            .collect();
        assert_eq!(
            screens,
            [
                ("FAKE-1", "GIGA-BYTE GS27QA", 2560, 1440, 0),
                ("FAKE-2", "Dell U2720Q", 3840, 2160, 2560)
            ]
        );
        assert_eq!(
            (
                inv.cameras[0].device.as_str(),
                inv.cameras[0].label.as_str()
            ),
            (FAKE_CAMERA, "Logitech C920")
        );
        assert_eq!(inv.cameras[0].modes, fake_camera_modes());
        assert_eq!(
            (inv.mics[0].name.as_str(), inv.mics[0].description.as_str()),
            (FAKE_MIC, "Yeti Nano")
        );
        assert_eq!(
            (
                inv.monitors[0].name.as_str(),
                inv.monitors[0].description.as_str()
            ),
            (FAKE_MONITOR, "Built-in Audio Analog Stereo")
        );
        assert_eq!(inv.default_mic.as_deref(), Some(FAKE_MIC));
        assert_eq!(inv.default_monitor.as_deref(), Some(FAKE_MONITOR));
    }

    #[test]
    fn hooks_come_from_the_environment() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            FakeHooks::parse(|key| {
                vars.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            })
        };
        assert_eq!(env(&[]), FakeHooks::default());
        let hooks = env(&[
            (FAKE_EOS_DELAY_ENV, "8000"),
            (FAKE_PORTAL_ENV, "deny"),
            (FAKE_RECORD_OFFSET_ENV, "4842"),
            (FAKE_INVENTORY_ENV, "design"),
        ]);
        assert_eq!(
            hooks,
            FakeHooks {
                eos_delay: Some(Duration::from_millis(8000)),
                portal: Some(FakePortal::Deny),
                record_offset_frames: 4842,
                inventory: FakeInventoryKind::Design,
            }
        );
        assert_eq!(
            env(&[(FAKE_PORTAL_ENV, " cancel ")]).portal,
            Some(FakePortal::Cancel)
        );
        // What doesn't parse is ignored.
        let bad = env(&[
            (FAKE_EOS_DELAY_ENV, "soon"),
            (FAKE_PORTAL_ENV, "maybe"),
            (FAKE_RECORD_OFFSET_ENV, "-3"),
            (FAKE_INVENTORY_ENV, "brief"),
        ]);
        assert_eq!(bad, FakeHooks::default());
    }

    /// 4842 frames at 30 fps are S04's 00:02:41:12; 932 are S04b's 00:00:31:02.
    #[test]
    fn the_record_offset_is_in_frames_of_the_take() {
        let hooks = |frames| FakeHooks {
            record_offset_frames: frames,
            ..FakeHooks::default()
        };
        let fps = Fps::new(30, 1);
        assert_eq!(hooks(4842).record_offset_ns(fps), 161_400_000_000);
        assert_eq!(hooks(932).record_offset_ns(fps), 31_066_666_666);
        assert_eq!(
            hooks(30_000).record_offset_ns(Fps::new(30_000, 1001)),
            1_001_000_000_000
        );
        assert_eq!(hooks(0).record_offset_ns(fps), 0);
        assert_eq!(hooks(10).record_offset_ns(Fps::new(0, 1)), 0);
    }

    #[test]
    fn the_fake_portal_words_its_refusals_like_the_real_one() {
        assert_eq!(
            FakePortal::Deny.refusal("FAKE-1"),
            "screen sharing for FAKE-1 failed: the desktop portal denied access to the screen"
        );
        assert_eq!(
            FakePortal::Cancel.refusal("FAKE-2"),
            "screen sharing for FAKE-2 failed: screen sharing was cancelled in the dialog"
        );
    }
}
