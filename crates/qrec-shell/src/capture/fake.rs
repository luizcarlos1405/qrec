//! The fake capture backend's devices (`QREC_CAPTURE_BACKEND=fake`, A§8.11).
//!
//! Discovery under the fake backend returns exactly these devices (the
//! `sources` module and `Facts` use [`fake_inventory`]); the engine turns
//! them into `videotestsrc`/`audiotestsrc` sources and never touches the
//! portal, cameras or PulseAudio.

use qrec_core::Fps;
use qrec_core::ctx::{AudioDeviceInfo, CameraInfo, CameraMode, ScreenInfo};

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
/// `/dev/fake0`, mic `alsa_input.fake`, monitor `alsa_output.fake.monitor`.
pub fn fake_inventory() -> FakeInventory {
    let (width, height) = FAKE_SCREEN_SIZE;
    let screens = FAKE_SCREENS
        .iter()
        .enumerate()
        .map(|(i, output)| ScreenInfo {
            output: (*output).to_owned(),
            description: format!("Fake screen {}", i + 1),
            x: (i as i32) * width as i32,
            y: 0,
            width,
            height,
            scale: 1.0,
        })
        .collect();
    FakeInventory {
        screens,
        cameras: vec![CameraInfo {
            device: FAKE_CAMERA.to_owned(),
            label: "Fake camera".to_owned(),
            modes: fake_camera_modes(),
        }],
        mics: vec![AudioDeviceInfo {
            name: FAKE_MIC.to_owned(),
            description: "Fake mic".to_owned(),
            is_monitor: false,
        }],
        monitors: vec![AudioDeviceInfo {
            name: FAKE_MONITOR.to_owned(),
            description: "Fake system audio".to_owned(),
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
        let inv = fake_inventory();
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
}
