//! Where Qrec runs: the session type, the desktop, the distribution's
//! family, a sandbox and a Nix shell (issue #9). The doctor, install
//! hints, the cheat sheet's shortcut snippet and the screen backend pick
//! their wording and behavior from it.
//!
//! Detection is pure over a [`PlatformEnv`]: the environment variables in
//! [`ENV_KEYS`], the text of os-release, whether `/.flatpak-info` exists
//! and whether Qrec's executable lies under `$SNAP`, which the shell reads
//! once at startup (`qrec_shell::platform`). Nothing here touches a file
//! or a device.
//!
//! [`install_hint`] is the doctor's table of fixes: what a failed check
//! needs ([`Need`]) as this distribution's packages and install command,
//! the desktop's ScreenCast backend, a daemon or an X server extension to
//! start, or a generic sentence where the package names aren't known.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The environment variables [`detect`] reads.
pub const ENV_KEYS: [&str; 12] = [
    "WAYLAND_DISPLAY",
    "WAYLAND_SOCKET",
    "DISPLAY",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "DESKTOP_SESSION",
    "NIRI_SOCKET",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "SWAYSOCK",
    "I3SOCK",
    "IN_NIX_SHELL",
];

/// What [`detect`] reads, as plain data. The shell builds it from the
/// process (`qrec_shell::platform::env_from_process`); tests build their
/// own with [`PlatformEnv::with`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlatformEnv {
    /// The variables of [`ENV_KEYS`] that are set; others are ignored.
    pub vars: BTreeMap<String, String>,
    /// The text of `/etc/os-release`, else `/usr/lib/os-release`; `None`
    /// when neither could be read.
    pub os_release: Option<String>,
    /// `/.flatpak-info` exists: Qrec runs inside a Flatpak sandbox.
    pub flatpak_info: bool,
    /// Qrec's own executable lies under `$SNAP`: Qrec runs inside a Snap.
    /// `SNAP` alone says nothing, since classic snaps (VS Code, IDEs,
    /// terminals) leak it into every program they start.
    pub snap: bool,
}

impl PlatformEnv {
    /// Sets `key` (builder style).
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(key.into(), value.into());
        self
    }

    /// The os-release text (builder style).
    pub fn with_os_release(mut self, text: impl Into<String>) -> Self {
        self.os_release = Some(text.into());
        self
    }

    /// `key`'s value when it is set and not empty.
    fn var(&self, key: &str) -> Option<&str> {
        self.vars
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }
}

/// The detected platform. JSON: `{"session": "wayland", "desktop":
/// "niri", "distro": "nixos", "sandbox": "none", "nixShell": true}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPlatform {
    pub session: SessionType,
    pub desktop: Desktop,
    pub distro: DistroFamily,
    pub sandbox: Sandbox,
    /// Inside a `nix develop` or `nix-shell` environment (`IN_NIX_SHELL`).
    pub nix_shell: bool,
}

/// One log line: `session=wayland desktop=niri distro=nixos sandbox=none nixShell=true`.
impl fmt::Display for HostPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "session={} desktop={} distro={} sandbox={} nixShell={}",
            self.session.as_str(),
            self.desktop.as_str(),
            self.distro.as_str(),
            self.sandbox.as_str(),
            self.nix_shell
        )
    }
}

/// The kind of graphical session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionType {
    Wayland,
    X11,
    /// No display: a tty, ssh without X forwarding, a service.
    Unknown,
}

impl SessionType {
    /// `wayland`, `x11` or `unknown`.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionType::Wayland => "wayland",
            SessionType::X11 => "x11",
            SessionType::Unknown => "unknown",
        }
    }
}

/// The desktop environment or compositor. JSON: its name as a string
/// (`niri`, `gnome`, …; another desktop's own name, lowercased).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", from = "String")]
pub enum Desktop {
    Niri,
    Gnome,
    Kde,
    Sway,
    Hyprland,
    River,
    Wayfire,
    Labwc,
    Cosmic,
    Xfce,
    Mate,
    Cinnamon,
    I3,
    /// A desktop Qrec has no special case for, by the name it gave
    /// (lowercased, without an `X-` prefix): `budgie`, `lxqt`, …
    Other(String),
    /// Nothing says which.
    Unknown,
}

impl Desktop {
    /// The name in JSON and logs.
    pub fn as_str(&self) -> &str {
        match self {
            Desktop::Niri => "niri",
            Desktop::Gnome => "gnome",
            Desktop::Kde => "kde",
            Desktop::Sway => "sway",
            Desktop::Hyprland => "hyprland",
            Desktop::River => "river",
            Desktop::Wayfire => "wayfire",
            Desktop::Labwc => "labwc",
            Desktop::Cosmic => "cosmic",
            Desktop::Xfce => "xfce",
            Desktop::Mate => "mate",
            Desktop::Cinnamon => "cinnamon",
            Desktop::I3 => "i3",
            Desktop::Other(name) => name,
            Desktop::Unknown => "unknown",
        }
    }

    /// The desktop a (lowercase) name means: the names above, a few
    /// aliases desktops use in `XDG_SESSION_DESKTOP` and `DESKTOP_SESSION`
    /// (`plasma`, `gnome-classic`), else [`Desktop::Other`]. Empty or
    /// `unknown` is [`Desktop::Unknown`].
    pub fn from_name(name: &str) -> Desktop {
        match name {
            "niri" => Desktop::Niri,
            "gnome" | "gnome-classic" | "gnome-flashback" => Desktop::Gnome,
            "kde" | "plasma" | "plasmawayland" | "plasmax11" => Desktop::Kde,
            "sway" => Desktop::Sway,
            "hyprland" => Desktop::Hyprland,
            "river" => Desktop::River,
            "wayfire" => Desktop::Wayfire,
            "labwc" => Desktop::Labwc,
            "cosmic" => Desktop::Cosmic,
            "xfce" => Desktop::Xfce,
            "mate" => Desktop::Mate,
            "cinnamon" => Desktop::Cinnamon,
            "i3" => Desktop::I3,
            "" | "unknown" => Desktop::Unknown,
            other => Desktop::Other(other.to_owned()),
        }
    }

    fn is_known(&self) -> bool {
        !matches!(self, Desktop::Other(_) | Desktop::Unknown)
    }
}

impl From<Desktop> for String {
    fn from(desktop: Desktop) -> String {
        match desktop {
            Desktop::Other(name) => name,
            known => known.as_str().to_owned(),
        }
    }
}

impl From<String> for Desktop {
    fn from(name: String) -> Desktop {
        Desktop::from_name(&name)
    }
}

/// The distribution's family, which decides package names and the install
/// command. Derivatives join their parent through os-release's `ID_LIKE`
/// (Mint is Ubuntu, Rocky is Fedora, Manjaro is Arch).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DistroFamily {
    Debian,
    Ubuntu,
    Fedora,
    Arch,
    Suse,
    #[serde(rename = "nixos")]
    NixOs,
    Alpine,
    Void,
    Gentoo,
    Unknown,
}

impl DistroFamily {
    /// The name in JSON and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            DistroFamily::Debian => "debian",
            DistroFamily::Ubuntu => "ubuntu",
            DistroFamily::Fedora => "fedora",
            DistroFamily::Arch => "arch",
            DistroFamily::Suse => "suse",
            DistroFamily::NixOs => "nixos",
            DistroFamily::Alpine => "alpine",
            DistroFamily::Void => "void",
            DistroFamily::Gentoo => "gentoo",
            DistroFamily::Unknown => "unknown",
        }
    }

    /// The family of an os-release text: its `ID` when that is a family
    /// Qrec knows, else the first known one in `ID_LIKE` (in order), else
    /// [`DistroFamily::Unknown`].
    pub fn from_os_release(text: &str) -> DistroFamily {
        let id = os_release_value(text, "ID").unwrap_or_default();
        let like = os_release_value(text, "ID_LIKE").unwrap_or_default();
        std::iter::once(id.as_str())
            .chain(like.split_ascii_whitespace())
            .find_map(|id| DistroFamily::from_id(&id.to_ascii_lowercase()))
            .unwrap_or(DistroFamily::Unknown)
    }

    fn from_id(id: &str) -> Option<DistroFamily> {
        Some(match id {
            "debian" => DistroFamily::Debian,
            "ubuntu" => DistroFamily::Ubuntu,
            "fedora" => DistroFamily::Fedora,
            "arch" => DistroFamily::Arch,
            "suse" | "opensuse" | "sles" | "sled" => DistroFamily::Suse,
            _ if id.starts_with("opensuse-") => DistroFamily::Suse,
            "nixos" => DistroFamily::NixOs,
            "alpine" => DistroFamily::Alpine,
            "void" => DistroFamily::Void,
            "gentoo" => DistroFamily::Gentoo,
            _ => return None,
        })
    }
}

/// A sandbox Qrec runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Sandbox {
    None,
    Flatpak,
    Snap,
}

impl Sandbox {
    /// `none`, `flatpak` or `snap`.
    pub fn as_str(self) -> &'static str {
        match self {
            Sandbox::None => "none",
            Sandbox::Flatpak => "flatpak",
            Sandbox::Snap => "snap",
        }
    }
}

/// The platform `env` describes. Total: any input gives an answer.
pub fn detect(env: &PlatformEnv) -> HostPlatform {
    let sandbox = if env.flatpak_info {
        Sandbox::Flatpak
    } else if env.snap {
        Sandbox::Snap
    } else {
        Sandbox::None
    };
    HostPlatform {
        session: detect_session(env),
        desktop: detect_desktop(env),
        distro: env
            .os_release
            .as_deref()
            .map_or(DistroFamily::Unknown, DistroFamily::from_os_release),
        sandbox,
        nix_shell: env.var("IN_NIX_SHELL").is_some(),
    }
}

/// A Wayland display wins, then a login that says Wayland (with its
/// Wayland variables dropped, an X display is XWayland, which shows only X
/// clients), then an X display, then a login that says X11.
fn detect_session(env: &PlatformEnv) -> SessionType {
    let login = env
        .var("XDG_SESSION_TYPE")
        .map(|t| t.trim().to_ascii_lowercase());
    let wayland_display =
        env.var("WAYLAND_DISPLAY").is_some() || env.var("WAYLAND_SOCKET").is_some();
    if wayland_display || login.as_deref() == Some("wayland") {
        SessionType::Wayland
    } else if env.var("DISPLAY").is_some() || login.as_deref() == Some("x11") {
        SessionType::X11
    } else {
        SessionType::Unknown
    }
}

/// A compositor's own IPC socket wins (niri, Hyprland, sway, i3: sway also
/// sets `I3SOCK`); then the first of `XDG_CURRENT_DESKTOP`,
/// `XDG_SESSION_DESKTOP` and `DESKTOP_SESSION` that names anything decides:
/// its first known entry (`ubuntu:GNOME` is GNOME), else its first entry.
fn detect_desktop(env: &PlatformEnv) -> Desktop {
    const SOCKETS: [(&str, Desktop); 4] = [
        ("NIRI_SOCKET", Desktop::Niri),
        ("HYPRLAND_INSTANCE_SIGNATURE", Desktop::Hyprland),
        ("SWAYSOCK", Desktop::Sway),
        ("I3SOCK", Desktop::I3),
    ];
    if let Some((_, desktop)) = SOCKETS.iter().find(|(key, _)| env.var(key).is_some()) {
        return desktop.clone();
    }
    for key in [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ] {
        let Some(value) = env.var(key) else { continue };
        let names: Vec<Desktop> = value
            .split(':')
            .map(desktop_token)
            .filter(|name| !name.is_empty())
            .map(|name| Desktop::from_name(&name))
            .collect();
        if let Some(known) = names.iter().find(|d| d.is_known()) {
            return known.clone();
        }
        if let Some(first) = names.into_iter().next() {
            return first;
        }
    }
    Desktop::Unknown
}

/// One entry of a desktop variable as a name: trimmed, lowercased, the
/// last part of a path (old display managers put a session file's path in
/// `DESKTOP_SESSION`), without an `x-` prefix (`X-Cinnamon`).
fn desktop_token(raw: &str) -> String {
    let name = raw.trim().rsplit('/').next().unwrap_or_default();
    let name = name.to_lowercase();
    match name.strip_prefix("x-") {
        Some(rest) => rest.to_owned(),
        None => name,
    }
}

/// `key`'s value in an os-release text (os-release(5)): `KEY=value`,
/// optionally in single or double quotes (backslash escapes in double
/// quotes); `#` starts a comment line. The last assignment wins.
fn os_release_value(text: &str, key: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let (k, v) = line.trim().split_once('=')?;
        (k.trim_end() == key).then(|| unquote(v.trim()))
    })
}

fn unquote(raw: &str) -> String {
    let Some(quote) = raw.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
        return raw.to_owned();
    };
    let mut out = String::new();
    let mut escaped = false;
    for c in raw[1..].chars() {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' && quote == '"' {
            escaped = true;
        } else if c == quote {
            break;
        } else {
            out.push(c);
        }
    }
    out
}

// ------------------------------------------------------------ install hints

/// What a failed doctor check needs: a package, the sound server, the
/// PipeWire daemon, the desktop's ScreenCast portal backend, the X
/// server's RandR extension or a GPU's VA-API driver. [`install_hint`]
/// says how to get it where Qrec runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Need {
    /// GStreamer itself, 1.24 or newer.
    Gstreamer,
    /// GStreamer Editing Services.
    Ges,
    /// gst-plugins-base: compositor, videorate, the test sources.
    GstPluginsBase,
    /// gst-plugins-good: v4l2src, pulsesrc, ximagesrc (X11 screens), the
    /// Matroska and FLAC elements.
    GstPluginsGood,
    /// gst-plugins-bad: h264parse.
    GstPluginsBad,
    /// x264enc, from gst-plugins-ugly.
    GstX264,
    /// openh264enc, gst-plugins-bad's OpenH264 plugin (the record encoder
    /// where x264enc isn't packaged).
    GstOpenH264,
    /// A software H.264 decoder for the editor's preview: gst-libav's
    /// avdec_h264 (or OpenH264's decoder).
    GstH264Decoder,
    /// gtk4paintablesink, gst-plugins-rs's GTK 4 plugin.
    GstGtk4,
    /// pipewiresrc, PipeWire's GStreamer plugin.
    GstPipewire,
    /// GStreamer's VA plugin (`va*h264enc`, `va*h264lpenc`).
    GstVa,
    /// ffmpeg and ffprobe.
    Ffmpeg,
    /// An ffmpeg with the encoders, decoders and demuxer Qrec uses.
    FullFfmpeg,
    /// pactl 16 or newer.
    Pactl,
    /// A sound server pactl reaches: PipeWire with pipewire-pulse, or
    /// PulseAudio.
    SoundServer,
    /// The PipeWire daemon (its `pipewire-0` socket), which the ScreenCast
    /// portal streams the screens through.
    PipeWire,
    /// xdg-desktop-portal with the desktop's ScreenCast backend.
    ScreenCastPortal,
    /// The X server's RandR extension, which lists its monitors (without
    /// it an X11 session's whole screen is one).
    RandR,
    /// A GPU's VA-API driver.
    VaDriver(VaGpu),
}

impl Need {
    /// Every need, in the table's order.
    pub const ALL: [Need; 21] = [
        Need::Gstreamer,
        Need::Ges,
        Need::GstPluginsBase,
        Need::GstPluginsGood,
        Need::GstPluginsBad,
        Need::GstX264,
        Need::GstOpenH264,
        Need::GstH264Decoder,
        Need::GstGtk4,
        Need::GstPipewire,
        Need::GstVa,
        Need::Ffmpeg,
        Need::FullFfmpeg,
        Need::Pactl,
        Need::SoundServer,
        Need::PipeWire,
        Need::ScreenCastPortal,
        Need::RandR,
        Need::VaDriver(VaGpu::Intel),
        Need::VaDriver(VaGpu::IntelLegacy),
        Need::VaDriver(VaGpu::Amd),
    ];

    /// What it is, as the generic sentence names it: upstream's names
    /// say "upstream", so no one takes them for this distribution's.
    fn what(self) -> &'static str {
        match self {
            Need::Gstreamer => "GStreamer 1.24 or newer",
            Need::Ges => "GStreamer Editing Services (upstream gst-editing-services)",
            Need::GstPluginsBase => "GStreamer's base plugins (upstream gst-plugins-base)",
            Need::GstPluginsGood => "GStreamer's good plugins (upstream gst-plugins-good)",
            Need::GstPluginsBad => "GStreamer's bad plugins (upstream gst-plugins-bad)",
            Need::GstX264 => "GStreamer's x264enc (upstream gst-plugins-ugly)",
            Need::GstOpenH264 => {
                "GStreamer's openh264enc (upstream gst-plugins-bad, built with OpenH264)"
            }
            Need::GstH264Decoder => "GStreamer's libav plugin, avdec_h264 (upstream gst-libav)",
            Need::GstGtk4 => {
                "GStreamer's GTK 4 plugin, gtk4paintablesink (upstream gst-plugins-rs)"
            }
            Need::GstPipewire => "PipeWire's GStreamer plugin (pipewiresrc)",
            Need::GstVa => "GStreamer's VA plugin (upstream gst-plugins-bad)",
            Need::Ffmpeg => "ffmpeg (with ffprobe)",
            Need::FullFfmpeg => "a full ffmpeg build (with libx264, libvpx, libopus and libsvtav1)",
            Need::Pactl => "the PulseAudio client tools (pactl) 16 or newer",
            Need::SoundServer => "the sound server: PipeWire with pipewire-pulse, or PulseAudio",
            Need::PipeWire => "the PipeWire daemon",
            Need::ScreenCastPortal => {
                "xdg-desktop-portal and your desktop's ScreenCast backend \
                 (xdg-desktop-portal-gnome, -kde, -wlr, -hyprland or -cosmic)"
            }
            Need::RandR => "the X server's RandR extension",
            Need::VaDriver(VaGpu::Intel) => {
                "Intel's iHD VA-API driver (chips older than Broadwell need the i965 one instead)"
            }
            Need::VaDriver(VaGpu::IntelLegacy) => {
                "Intel's i965 VA-API driver (for chips older than Broadwell)"
            }
            Need::VaDriver(VaGpu::Amd) => "Mesa's VA-API driver (radeonsi)",
        }
    }

    /// The attribute of the Nix package set that Qrec's flake ships it as
    /// (its dev shell and its package); `None` for what the system
    /// provides (the sound server, the PipeWire daemon, the portal, the X
    /// server, GPU drivers).
    fn nix_attribute(self) -> Option<&'static str> {
        Some(match self {
            Need::Gstreamer => "gst_all_1.gstreamer",
            Need::Ges => "gst_all_1.gst-editing-services",
            Need::GstPluginsBase => "gst_all_1.gst-plugins-base",
            Need::GstPluginsGood => "gst_all_1.gst-plugins-good",
            Need::GstPluginsBad | Need::GstVa | Need::GstOpenH264 => "gst_all_1.gst-plugins-bad",
            Need::GstX264 => "gst_all_1.gst-plugins-ugly",
            Need::GstH264Decoder => "gst_all_1.gst-libav",
            Need::GstGtk4 => "gst_all_1.gst-plugins-rs",
            Need::GstPipewire => "pipewire",
            Need::Ffmpeg | Need::FullFfmpeg => "ffmpeg-full",
            Need::Pactl => "pulseaudio",
            Need::SoundServer
            | Need::PipeWire
            | Need::ScreenCastPortal
            | Need::RandR
            | Need::VaDriver(_) => return None,
        })
    }
}

/// A GPU that encodes H.264 through VA-API, by vendor and generation
/// (A§8.10: NVIDIA never does).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VaGpu {
    /// Intel, Broadwell (2014) or newer: the iHD driver. Its hint names the
    /// older chips' i965 too, for when the generation isn't known.
    Intel,
    /// Intel older than Broadwell: the i965 driver.
    IntelLegacy,
    /// AMD: Mesa's radeonsi.
    Amd,
}

/// How to get a [`Need`] here ([`install_hint`]). JSON: `{"packages":
/// ["gstreamer1.0-gtk4"], "command": "sudo apt install gstreamer1.0-gtk4",
/// "note": null}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hint {
    /// This distribution's packages, or the Nix attribute on NixOS; empty
    /// where Qrec doesn't know their names, or when nothing is installed.
    pub packages: Vec<String>,
    /// The command to copy: the family's installer with `packages`
    /// (`sudo apt install …`), Fedora's `dnf swap`, or `nix develop` in a
    /// Nix shell; `None` when there is no command to give.
    pub command: Option<String>,
    /// What else to know or do, as a clause ("it comes from RPM Fusion:
    /// enable that repository first"); without a command, the whole hint.
    pub note: Option<String>,
}

impl Hint {
    /// A hint that is only its note: nothing to install or copy.
    fn advice(note: impl Into<String>) -> Hint {
        Hint {
            note: Some(note.into()),
            ..Hint::default()
        }
    }

    /// The hint in one line: the command with the note in parentheses, or
    /// the note alone (the doctor's `hint`).
    pub fn line(&self) -> String {
        match (&self.command, &self.note) {
            (Some(command), Some(note)) => format!("{command} ({note})"),
            (Some(command), None) => command.clone(),
            (None, Some(note)) => note.clone(),
            (None, None) => self.packages.join(" "),
        }
    }

    /// `more` after the note: the doctor's own advice for a check ("if
    /// it hangs, …").
    pub fn also(mut self, more: &str) -> Hint {
        self.note = Some(match self.note.take() {
            Some(note) => format!("{note}; {more}"),
            None => more.to_owned(),
        });
        self
    }
}

/// How to get `need` on `platform`: inside a Nix shell, `nix develop` for
/// what Qrec's flake ships; on NixOS, its Nix package; elsewhere this
/// family's packages with its install command (`sudo apt install …`,
/// `sudo dnf install …`, `sudo pacman -S …`, `sudo zypper install …`)
/// where their names are known, else a sentence naming what to install and
/// the family's package manager. The ScreenCast portal follows the session
/// and the desktop (an X11 session needs none); the sound server and the
/// PipeWire daemon are started, not installed, and RandR comes with the X
/// server. Package names are the ones the distributions' repositories list
/// (checked against their package lists, October 2026); a family whose
/// name for it isn't known gets the sentence.
pub fn install_hint(need: Need, platform: &HostPlatform) -> Hint {
    let family = platform.distro;
    match need {
        Need::ScreenCastPortal => return portal_hint(platform),
        Need::SoundServer => return sound_server_hint(family),
        Need::PipeWire => return pipewire_hint(family),
        Need::RandR => {
            return Hint::advice(
                "start the X server with its RandR extension (Xvfb: +extension RANDR) to list \
                 each monitor",
            );
        }
        _ => {}
    }
    if let Some(attribute) = need.nix_attribute() {
        let name = attribute.trim_start_matches("gst_all_1.");
        if platform.nix_shell {
            return Hint {
                packages: Vec::new(),
                command: Some("nix develop".to_owned()),
                note: Some(format!("the flake provides {name}")),
            };
        }
        if family == DistroFamily::NixOs {
            return Hint {
                packages: vec![attribute.to_owned()],
                command: None,
                note: Some(format!(
                    "run qrec from its Nix package, which ships {attribute}"
                )),
            };
        }
    }
    match known_packages(need, family) {
        Some(known) => {
            let command = known
                .command
                .map(str::to_owned)
                .or_else(|| family.install_command(known.packages));
            Hint {
                packages: known.packages.iter().map(|p| (*p).to_owned()).collect(),
                command,
                note: known.note.map(str::to_owned),
            }
        }
        None => Hint::advice(family.generic_install(need.what())),
    }
}

/// A family's packages for a need.
struct Known {
    packages: &'static [&'static str],
    /// The command when it isn't the family's installer with `packages`.
    command: Option<&'static str>,
    note: Option<&'static str>,
}

const RPM_FUSION: &str = "it comes from RPM Fusion: enable that repository first";

/// The packages each family names a need with, where they are known: the
/// distributions' package lists (Debian 13, Ubuntu 26.04, Fedora 44, Arch,
/// Tumbleweed) and Fedora's codec situation (RPM Fusion). Every other
/// pair is `None`: the generic sentence.
fn known_packages(need: Need, family: DistroFamily) -> Option<Known> {
    use DistroFamily::{Arch, Debian, Fedora, Suse, Ubuntu};
    let known = |packages, note| {
        Some(Known {
            packages,
            command: None,
            note,
        })
    };
    match (need, family) {
        (Need::GstGtk4, Debian | Ubuntu) => known(&["gstreamer1.0-gtk4"], None),
        (Need::GstGtk4, Fedora) => known(&["gstreamer1-plugin-gtk4"], None),
        (Need::GstGtk4, Arch) => known(&["gst-plugin-gtk4"], None),
        (Need::GstPipewire, Debian | Ubuntu) => known(&["gstreamer1.0-pipewire"], None),
        (Need::GstPipewire, Fedora) => known(&["pipewire-gstreamer"], None),
        (Need::GstPipewire, Arch) => known(&["gst-plugin-pipewire"], None),
        (Need::GstPipewire, Suse) => known(&["gstreamer-plugin-pipewire"], None),
        (Need::GstX264, Fedora) => known(
            &["gstreamer1-plugins-ugly"],
            Some(
                "it comes from RPM Fusion, since Fedora's own plugins have no x264enc: \
                 enable that repository first",
            ),
        ),
        (Need::GstX264, Arch) => known(&["gst-plugins-ugly"], None),
        (Need::GstOpenH264, Fedora) => known(&["gstreamer1-plugin-openh264"], None),
        (Need::GstH264Decoder, Fedora) => known(
            &["gstreamer1-plugin-openh264"],
            Some("OpenH264 may not decode High-profile takes"),
        ),
        (Need::GstH264Decoder, Arch) => known(&["gst-libav"], None),
        (Need::GstPluginsBase, Arch) => known(&["gst-plugins-base"], None),
        (Need::GstPluginsGood, Arch) => known(&["gst-plugins-good"], None),
        (Need::GstPluginsBad, Arch) => known(&["gst-plugins-bad"], None),
        (Need::GstVa, Arch) => known(&["gst-plugin-va"], None),
        (Need::Ges, Arch) => known(&["gst-editing-services"], None),
        (Need::Ffmpeg, Debian | Ubuntu | Arch | Suse) => known(&["ffmpeg"], None),
        (Need::Ffmpeg, Fedora) => known(
            &["ffmpeg-free"],
            Some(
                "RPM Fusion's ffmpeg also has libx264 for MP4 exports: \
                 sudo dnf swap ffmpeg-free ffmpeg --allowerasing",
            ),
        ),
        (Need::FullFfmpeg, Fedora) => Some(Known {
            packages: &["ffmpeg"],
            command: Some("sudo dnf swap ffmpeg-free ffmpeg --allowerasing"),
            note: Some(RPM_FUSION),
        }),
        (Need::VaDriver(VaGpu::Intel), Fedora) => known(
            &["intel-media-driver"],
            Some(
                "it comes from RPM Fusion's nonfree repository: enable it first; Intel \
                 chips older than Broadwell need the i965 driver instead",
            ),
        ),
        (Need::VaDriver(VaGpu::Amd), Fedora) => known(
            &["mesa-va-drivers-freeworld"],
            Some(
                "it comes from RPM Fusion, since Fedora's own Mesa doesn't encode H.264: \
                 enable that repository first",
            ),
        ),
        _ => None,
    }
}

impl DistroFamily {
    /// The command that installs `packages` here; `None` for a family
    /// whose installer Qrec doesn't spell out.
    fn install_command(self, packages: &[&str]) -> Option<String> {
        let installer = match self {
            DistroFamily::Debian | DistroFamily::Ubuntu => "sudo apt install",
            DistroFamily::Fedora => "sudo dnf install",
            DistroFamily::Arch => "sudo pacman -S",
            DistroFamily::Suse => "sudo zypper install",
            DistroFamily::NixOs
            | DistroFamily::Alpine
            | DistroFamily::Void
            | DistroFamily::Gentoo
            | DistroFamily::Unknown => return None,
        };
        Some(format!("{installer} {}", packages.join(" ")))
    }

    /// The generic sentence: install `what` with this family's package
    /// manager (NixOS: your configuration).
    fn generic_install(self, what: &str) -> String {
        let manager = match self {
            DistroFamily::Debian | DistroFamily::Ubuntu => "apt",
            DistroFamily::Fedora => "dnf",
            DistroFamily::Arch => "pacman",
            DistroFamily::Suse => "zypper",
            DistroFamily::Alpine => "apk",
            DistroFamily::Void => "xbps-install",
            DistroFamily::Gentoo => "emerge",
            DistroFamily::NixOs => return format!("add {what} to your NixOS configuration"),
            DistroFamily::Unknown => "your distribution's package manager",
        };
        format!("install {what} with {manager}")
    }
}

/// A sound server pactl can't reach is usually one that isn't running:
/// nothing to install, start one.
fn sound_server_hint(family: DistroFamily) -> Hint {
    Hint::advice(match family {
        DistroFamily::NixOs => "start the sound server: PipeWire with \
                                services.pipewire.pulse.enable = true, or PulseAudio"
            .to_owned(),
        _ => format!("start {}", Need::SoundServer.what()),
    })
}

/// A PipeWire daemon that isn't running is usually one that wasn't
/// started (the portal's backends come with PipeWire): start it, which
/// systemd sessions do through its user socket.
fn pipewire_hint(family: DistroFamily) -> Hint {
    const WHY: &str = "the ScreenCast portal streams the screens through it";
    let how = match family {
        DistroFamily::NixOs => "services.pipewire.enable = true",
        DistroFamily::Alpine => "run /usr/libexec/pipewire-launcher with your session",
        DistroFamily::Gentoo => "with OpenRC, run gentoo-pipewire-launcher with your session",
        DistroFamily::Void => "run pipewire from your session's autostart",
        DistroFamily::Unknown => {
            "with systemd: systemctl --user enable --now pipewire.socket; without it, run \
             pipewire from your session's autostart"
        }
        DistroFamily::Debian
        | DistroFamily::Ubuntu
        | DistroFamily::Fedora
        | DistroFamily::Arch
        | DistroFamily::Suse => "with systemd: systemctl --user enable --now pipewire.socket",
    };
    Hint::advice(format!("start {} ({how}): {WHY}", Need::PipeWire.what()))
}

/// The ScreenCast backend of the desktop, and what it needs set up. An X11
/// session needs none; Qrec doesn't know the backends' package names per
/// distribution, so the hint names the backend.
fn portal_hint(platform: &HostPlatform) -> Hint {
    const RELOGIN: &str = "then log out and back in";
    if platform.session == SessionType::X11 {
        return Hint::advice("an X11 session needs no ScreenCast portal");
    }
    Hint::advice(match platform.desktop {
        Desktop::Niri => format!("on niri, install xdg-desktop-portal-gnome, {RELOGIN}"),
        Desktop::Gnome => {
            format!("install xdg-desktop-portal-gnome, GNOME's ScreenCast backend, {RELOGIN}")
        }
        Desktop::Kde => {
            format!("install xdg-desktop-portal-kde, KDE's ScreenCast backend, {RELOGIN}")
        }
        Desktop::Sway | Desktop::River | Desktop::Wayfire | Desktop::Labwc => {
            "install xdg-desktop-portal-wlr and a screen chooser it runs (slurp, wofi, rofi or \
             fuzzel; without one it picks any output); choose it in \
             ~/.config/xdg-desktop-portal/portals.conf (org.freedesktop.impl.portal.ScreenCast=wlr \
             under [preferred]); have your compositor run `dbus-update-activation-environment \
             --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP`; start the portal with \
             XDPW_PERSIST_MODE=permanent so it remembers the screen choice"
                .to_owned()
        }
        Desktop::Hyprland => format!(
            "install xdg-desktop-portal-hyprland, {RELOGIN}; in its screen picker, tick the box \
             that allows a restore token so the screen choice is remembered"
        ),
        Desktop::Cosmic => {
            format!("install xdg-desktop-portal-cosmic, COSMIC's ScreenCast backend, {RELOGIN}")
        }
        Desktop::Xfce
        | Desktop::Mate
        | Desktop::Cinnamon
        | Desktop::I3
        | Desktop::Other(_)
        | Desktop::Unknown => format!("install {}, {RELOGIN}", Need::ScreenCastPortal.what()),
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use proptest::prelude::*;

    use super::*;

    fn env(vars: &[(&str, &str)]) -> PlatformEnv {
        vars.iter()
            .fold(PlatformEnv::default(), |acc, (k, v)| acc.with(*k, *v))
    }

    #[test]
    fn niri_socket_beats_xdg_current_desktop() {
        let e = env(&[
            ("NIRI_SOCKET", "/run/user/1000/niri.wayland-1.sock"),
            ("XDG_CURRENT_DESKTOP", "GNOME"),
            ("XDG_SESSION_DESKTOP", "gnome"),
        ]);
        assert_eq!(detect(&e).desktop, Desktop::Niri);
        // Without the socket the variable decides.
        let e = env(&[("XDG_CURRENT_DESKTOP", "GNOME")]);
        assert_eq!(detect(&e).desktop, Desktop::Gnome);
    }

    #[test]
    fn wayland_display_beats_display() {
        let e = env(&[
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("DISPLAY", ":0"),
            ("XDG_SESSION_TYPE", "x11"),
        ]);
        assert_eq!(detect(&e).session, SessionType::Wayland);
        let e = env(&[("WAYLAND_SOCKET", "3"), ("DISPLAY", ":0")]);
        assert_eq!(detect(&e).session, SessionType::Wayland);
    }

    #[test]
    fn sessions() {
        let session = |vars: &[(&str, &str)]| detect(&env(vars)).session;
        assert_eq!(session(&[("DISPLAY", ":1")]), SessionType::X11);
        // startx from a tty: logind says tty, the X display is real.
        assert_eq!(
            session(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "tty")]),
            SessionType::X11
        );
        // A Wayland login whose Wayland variables were dropped: DISPLAY is
        // XWayland.
        assert_eq!(
            session(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "wayland")]),
            SessionType::Wayland
        );
        assert_eq!(session(&[("XDG_SESSION_TYPE", "x11")]), SessionType::X11);
        assert_eq!(
            session(&[("WAYLAND_DISPLAY", ""), ("DISPLAY", "")]),
            SessionType::Unknown
        );
        assert_eq!(
            session(&[("XDG_SESSION_TYPE", "tty")]),
            SessionType::Unknown
        );
    }

    #[test]
    fn desktops() {
        let desktop = |vars: &[(&str, &str)]| detect(&env(vars)).desktop;
        assert_eq!(
            desktop(&[("XDG_CURRENT_DESKTOP", "ubuntu:GNOME")]),
            Desktop::Gnome
        );
        assert_eq!(
            desktop(&[("XDG_CURRENT_DESKTOP", "X-Cinnamon")]),
            Desktop::Cinnamon
        );
        assert_eq!(desktop(&[("XDG_CURRENT_DESKTOP", "KDE")]), Desktop::Kde);
        assert_eq!(
            desktop(&[("XDG_CURRENT_DESKTOP", "labwc:wlroots")]),
            Desktop::Labwc
        );
        assert_eq!(
            desktop(&[("XDG_CURRENT_DESKTOP", "Budgie:GNOME")]),
            Desktop::Gnome
        );
        assert_eq!(
            desktop(&[("XDG_CURRENT_DESKTOP", "LXQt")]),
            Desktop::Other("lxqt".into())
        );
        // XDG_CURRENT_DESKTOP decides over the older variables.
        assert_eq!(
            desktop(&[("XDG_CURRENT_DESKTOP", "LXQt"), ("DESKTOP_SESSION", "xfce")]),
            Desktop::Other("lxqt".into())
        );
        assert_eq!(
            desktop(&[("DESKTOP_SESSION", "/usr/share/xsessions/plasma")]),
            Desktop::Kde
        );
        // Sway sets I3SOCK too; i3 alone is i3.
        assert_eq!(
            desktop(&[("SWAYSOCK", "/run/sway.sock"), ("I3SOCK", "/run/sway.sock")]),
            Desktop::Sway
        );
        assert_eq!(desktop(&[("I3SOCK", "/run/i3/ipc")]), Desktop::I3);
        assert_eq!(
            desktop(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc_123")]),
            Desktop::Hyprland
        );
        assert_eq!(
            desktop(&[
                ("XDG_CURRENT_DESKTOP", ":"),
                ("XDG_SESSION_DESKTOP", "mate")
            ]),
            Desktop::Mate
        );
        assert_eq!(desktop(&[]), Desktop::Unknown);
    }

    #[test]
    fn sandbox_and_nix_shell() {
        let mut e = env(&[("IN_NIX_SHELL", "impure")]);
        e.snap = true;
        assert_eq!(detect(&e).sandbox, Sandbox::Snap);
        assert!(detect(&e).nix_shell);
        e.flatpak_info = true;
        assert_eq!(detect(&e).sandbox, Sandbox::Flatpak);
        // A host qrec started from a classic snap's terminal (VS Code
        // here) inherits its SNAP: that is no sandbox.
        let leaked = detect(&env(&[
            ("SNAP", "/snap/code/200"),
            ("SNAP_NAME", "code"),
            ("IN_NIX_SHELL", ""),
        ]));
        assert_eq!(leaked.sandbox, Sandbox::None);
        assert!(!leaked.nix_shell);
    }

    #[test]
    fn json_and_log_line() {
        let e = env(&[
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("NIRI_SOCKET", "/run/niri.sock"),
            ("IN_NIX_SHELL", "impure"),
        ])
        .with_os_release("ID=nixos\n");
        let platform = detect(&e);
        let json = serde_json::to_value(&platform).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "session": "wayland",
                "desktop": "niri",
                "distro": "nixos",
                "sandbox": "none",
                "nixShell": true,
            })
        );
        assert_eq!(
            serde_json::from_value::<HostPlatform>(json).unwrap(),
            platform
        );
        assert_eq!(
            platform.to_string(),
            "session=wayland desktop=niri distro=nixos sandbox=none nixShell=true"
        );
        let other = serde_json::to_value(Desktop::Other("lxqt".into())).unwrap();
        assert_eq!(other, serde_json::json!("lxqt"));
    }

    #[test]
    fn os_release_quoting() {
        let text = "# comment\nID='opensuse-leap'\nID_LIKE=\"suse \\\"opensuse\\\"\"\n";
        assert_eq!(
            os_release_value(text, "ID").as_deref(),
            Some("opensuse-leap")
        );
        assert_eq!(
            os_release_value(text, "ID_LIKE").as_deref(),
            Some("suse \"opensuse\"")
        );
        assert_eq!(os_release_value(text, "NAME"), None);
        assert_eq!(DistroFamily::from_os_release(""), DistroFamily::Unknown);
        assert_eq!(
            DistroFamily::from_os_release("ID=Fedora"),
            DistroFamily::Fedora
        );
    }

    /// Real os-release files (tests/fixtures/os-release).
    const OS_RELEASES: [(&str, &str); 12] = [
        (
            "debian-13",
            include_str!("../tests/fixtures/os-release/debian-13"),
        ),
        (
            "ubuntu-26.04",
            include_str!("../tests/fixtures/os-release/ubuntu-26.04"),
        ),
        (
            "linuxmint-22",
            include_str!("../tests/fixtures/os-release/linuxmint-22"),
        ),
        (
            "fedora-44",
            include_str!("../tests/fixtures/os-release/fedora-44"),
        ),
        (
            "rocky-10",
            include_str!("../tests/fixtures/os-release/rocky-10"),
        ),
        ("arch", include_str!("../tests/fixtures/os-release/arch")),
        (
            "opensuse-tumbleweed",
            include_str!("../tests/fixtures/os-release/opensuse-tumbleweed"),
        ),
        (
            "nixos-26.05",
            include_str!("../tests/fixtures/os-release/nixos-26.05"),
        ),
        (
            "alpine-3.22",
            include_str!("../tests/fixtures/os-release/alpine-3.22"),
        ),
        ("void", include_str!("../tests/fixtures/os-release/void")),
        (
            "gentoo",
            include_str!("../tests/fixtures/os-release/gentoo"),
        ),
        ("empty", ""),
    ];

    #[test]
    fn os_release_families() {
        let mut table = String::new();
        for (name, text) in OS_RELEASES {
            let family = DistroFamily::from_os_release(text);
            writeln!(table, "{name:<20} {}", family.as_str()).unwrap();
        }
        insta::assert_snapshot!("os_release_families", table);
    }

    /// Any of the variables detection reads, set to anything.
    fn any_env() -> impl Strategy<Value = PlatformEnv> {
        let vars = proptest::collection::btree_map(
            proptest::sample::select(ENV_KEYS.to_vec()).prop_map(str::to_owned),
            prop_oneof![
                any::<String>(),
                "[a-zA-Z:/_ -]{0,24}",
                proptest::sample::select(vec![
                    "",
                    ":0",
                    "wayland-1",
                    "wayland",
                    "x11",
                    "tty",
                    "GNOME",
                    "ubuntu:GNOME",
                    "X-Cinnamon",
                    "unknown",
                    "x-",
                    "KDE",
                    "niri",
                ])
                .prop_map(str::to_owned),
            ],
            0..ENV_KEYS.len(),
        );
        let os_release = proptest::option::of(prop_oneof![
            any::<String>(),
            "(ID|ID_LIKE)=[\"']?[a-z -]{0,16}[\"']?(\n(ID|ID_LIKE)=[a-z\"' -]{0,16}){0,3}",
        ]);
        (vars, os_release, any::<bool>(), any::<bool>()).prop_map(
            |(vars, os_release, flatpak_info, snap)| PlatformEnv {
                vars,
                os_release,
                flatpak_info,
                snap,
            },
        )
    }

    proptest! {
        #[test]
        fn detect_is_total_and_round_trips(e in any_env()) {
            let platform = detect(&e);
            // Whatever came in, the JSON reads back as the same platform.
            let json = serde_json::to_string(&platform).unwrap();
            prop_assert_eq!(serde_json::from_str::<HostPlatform>(&json).unwrap(), platform);
        }

        #[test]
        fn never_x11_with_a_wayland_display(
            e in any_env(),
            key in proptest::sample::select(vec!["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]),
            value in ".+",
        ) {
            let e = e.with(key, value);
            prop_assert_eq!(detect(&e).session, SessionType::Wayland);
        }
    }

    // ------------------------------------------------------- install hints

    const FAMILIES: [DistroFamily; 10] = [
        DistroFamily::Debian,
        DistroFamily::Ubuntu,
        DistroFamily::Fedora,
        DistroFamily::Arch,
        DistroFamily::Suse,
        DistroFamily::NixOs,
        DistroFamily::Alpine,
        DistroFamily::Void,
        DistroFamily::Gentoo,
        DistroFamily::Unknown,
    ];

    fn all_desktops() -> Vec<Desktop> {
        vec![
            Desktop::Niri,
            Desktop::Gnome,
            Desktop::Kde,
            Desktop::Sway,
            Desktop::Hyprland,
            Desktop::River,
            Desktop::Wayfire,
            Desktop::Labwc,
            Desktop::Cosmic,
            Desktop::Xfce,
            Desktop::Mate,
            Desktop::Cinnamon,
            Desktop::I3,
            Desktop::Other("lxqt".into()),
            Desktop::Unknown,
        ]
    }

    fn host(
        distro: DistroFamily,
        desktop: Desktop,
        session: SessionType,
        nix_shell: bool,
    ) -> HostPlatform {
        HostPlatform {
            session,
            desktop,
            distro,
            sandbox: Sandbox::None,
            nix_shell,
        }
    }

    /// A Wayland session of `distro` on an unknown desktop.
    fn wayland(distro: DistroFamily) -> HostPlatform {
        host(distro, Desktop::Unknown, SessionType::Wayland, false)
    }

    /// One table cell: `$ command`, the note, `[packages]`.
    fn cell(hint: &Hint) -> String {
        let mut parts = Vec::new();
        parts.extend(hint.command.iter().map(|c| format!("$ {c}")));
        parts.extend(hint.note.clone());
        if !hint.packages.is_empty() {
            parts.push(format!("[{}]", hint.packages.join(", ")));
        }
        parts.join(" · ")
    }

    /// The whole table: every need for every family (on an unknown Wayland
    /// desktop, and inside a Nix shell on Debian), then the ScreenCast
    /// portal for every desktop and without a Wayland session. The desktop
    /// only changes the portal's hint and the family never does
    /// (`hints_follow_their_inputs_only`), so this covers family × desktop.
    #[test]
    fn install_hint_table() {
        let mut table = String::new();
        for need in Need::ALL {
            writeln!(table, "{need:?}").unwrap();
            for family in FAMILIES {
                let hint = install_hint(need, &wayland(family));
                writeln!(table, "  {:<9} {}", family.as_str(), cell(&hint)).unwrap();
            }
            let in_shell = install_hint(
                need,
                &host(
                    DistroFamily::Debian,
                    Desktop::Unknown,
                    SessionType::Wayland,
                    true,
                ),
            );
            writeln!(table, "  {:<9} {}", "nix shell", cell(&in_shell)).unwrap();
        }
        writeln!(table, "ScreenCastPortal by desktop (Wayland)").unwrap();
        for desktop in all_desktops() {
            let hint = install_hint(
                Need::ScreenCastPortal,
                &host(
                    DistroFamily::Debian,
                    desktop.clone(),
                    SessionType::Wayland,
                    false,
                ),
            );
            writeln!(table, "  {:<9} {}", desktop.as_str(), cell(&hint)).unwrap();
        }
        for (label, desktop, session) in [
            ("x11 xfce", Desktop::Xfce, SessionType::X11),
            ("x11 kde", Desktop::Kde, SessionType::X11),
            ("no display", Desktop::Unknown, SessionType::Unknown),
        ] {
            let hint = install_hint(
                Need::ScreenCastPortal,
                &host(DistroFamily::Debian, desktop, session, false),
            );
            writeln!(table, "  {label:<9} {}", cell(&hint)).unwrap();
        }
        insta::assert_snapshot!("install_hints", table);
    }

    #[test]
    fn hints_name_this_familys_packages() {
        let gtk4 = |family| install_hint(Need::GstGtk4, &wayland(family));
        assert_eq!(
            gtk4(DistroFamily::Ubuntu).command.as_deref(),
            Some("sudo apt install gstreamer1.0-gtk4")
        );
        assert_eq!(
            gtk4(DistroFamily::Fedora).packages,
            ["gstreamer1-plugin-gtk4"]
        );
        assert_eq!(
            gtk4(DistroFamily::Arch).command.as_deref(),
            Some("sudo pacman -S gst-plugin-gtk4")
        );
        // openSUSE's name isn't known: the sentence, with zypper.
        let suse = gtk4(DistroFamily::Suse);
        assert!(suse.packages.is_empty() && suse.command.is_none());
        assert_eq!(
            suse.line(),
            "install GStreamer's GTK 4 plugin, gtk4paintablesink (upstream gst-plugins-rs) \
             with zypper"
        );
        // NixOS: the package's attribute, no command; in a Nix shell, the
        // dev shell (S23's frame).
        let nixos = gtk4(DistroFamily::NixOs);
        assert_eq!(nixos.packages, ["gst_all_1.gst-plugins-rs"]);
        assert_eq!(nixos.command, None);
        let shell = install_hint(
            Need::GstGtk4,
            &host(
                DistroFamily::NixOs,
                Desktop::Niri,
                SessionType::Wayland,
                true,
            ),
        );
        assert_eq!(
            shell.line(),
            "nix develop (the flake provides gst-plugins-rs)"
        );
        // Fedora's codecs come from RPM Fusion.
        let fedora = wayland(DistroFamily::Fedora);
        assert_eq!(
            install_hint(Need::FullFfmpeg, &fedora).command.as_deref(),
            Some("sudo dnf swap ffmpeg-free ffmpeg --allowerasing")
        );
        assert!(
            install_hint(Need::GstX264, &fedora)
                .note
                .unwrap()
                .contains("RPM Fusion")
        );
        assert_eq!(
            install_hint(Need::VaDriver(VaGpu::Amd), &fedora)
                .command
                .as_deref(),
            Some("sudo dnf install mesa-va-drivers-freeworld")
        );
    }

    #[test]
    fn the_portal_follows_the_desktop_and_the_session() {
        let portal = |desktop, session| {
            install_hint(
                Need::ScreenCastPortal,
                &host(DistroFamily::Arch, desktop, session, false),
            )
            .line()
        };
        assert_eq!(
            portal(Desktop::Niri, SessionType::Wayland),
            "on niri, install xdg-desktop-portal-gnome, then log out and back in"
        );
        let wlr = portal(Desktop::Sway, SessionType::Wayland);
        for part in [
            "xdg-desktop-portal-wlr",
            "slurp",
            "portals.conf",
            "dbus-update-activation-environment",
            "XDPW_PERSIST_MODE=permanent",
        ] {
            assert!(wlr.contains(part), "{part}: {wlr}");
        }
        assert!(portal(Desktop::Hyprland, SessionType::Wayland).contains("restore token"));
        assert!(
            portal(Desktop::Cosmic, SessionType::Wayland).contains("xdg-desktop-portal-cosmic")
        );
        // KDE on X11 registers a ScreenCast portal that can't capture: on
        // X11 Qrec needs none.
        assert_eq!(
            portal(Desktop::Kde, SessionType::X11),
            "an X11 session needs no ScreenCast portal"
        );
    }

    /// The PipeWire daemon (the portal's screens) and RandR (an X11
    /// session's monitors) are started, not installed: a sentence, with no
    /// package or command; NixOS starts PipeWire from its configuration,
    /// and a Nix shell can't start either.
    #[test]
    fn the_pipewire_daemon_and_randr_are_started() {
        let pipewire = |platform: &HostPlatform| install_hint(Need::PipeWire, platform);
        let debian = pipewire(&wayland(DistroFamily::Debian));
        assert_eq!((debian.packages.len(), &debian.command), (0, &None));
        assert_eq!(
            debian.line(),
            "start the PipeWire daemon (with systemd: systemctl --user enable --now \
             pipewire.socket): the ScreenCast portal streams the screens through it"
        );
        assert_eq!(
            pipewire(&wayland(DistroFamily::NixOs)).line(),
            "start the PipeWire daemon (services.pipewire.enable = true): the ScreenCast \
             portal streams the screens through it"
        );
        let in_shell = host(
            DistroFamily::Debian,
            Desktop::Unknown,
            SessionType::Wayland,
            true,
        );
        assert_eq!(pipewire(&in_shell), debian);
        let randr = install_hint(
            Need::RandR,
            &host(DistroFamily::Arch, Desktop::I3, SessionType::X11, true),
        );
        assert_eq!((randr.packages.len(), &randr.command), (0, &None));
        assert_eq!(
            randr.line(),
            "start the X server with its RandR extension (Xvfb: +extension RANDR) to list each \
             monitor"
        );
    }

    #[test]
    fn a_hint_reads_in_one_line() {
        let hint = Hint {
            packages: vec!["gstreamer1-plugins-ugly".into()],
            command: Some("sudo dnf install gstreamer1-plugins-ugly".into()),
            note: Some("it comes from RPM Fusion".into()),
        };
        assert_eq!(
            hint.line(),
            "sudo dnf install gstreamer1-plugins-ugly (it comes from RPM Fusion)"
        );
        let hint = hint.also("then run qrec doctor again");
        assert_eq!(
            hint.note.as_deref(),
            Some("it comes from RPM Fusion; then run qrec doctor again")
        );
        assert_eq!(
            Hint::advice("start it").also("or not").line(),
            "start it; or not"
        );
        let json = serde_json::to_value(&hint).unwrap();
        assert_eq!(
            json["command"], "sudo dnf install gstreamer1-plugins-ugly",
            "{json}"
        );
        assert_eq!(serde_json::from_value::<Hint>(json).unwrap(), hint);
    }

    /// Any platform the table can be asked about.
    fn any_host() -> impl Strategy<Value = HostPlatform> {
        (
            proptest::sample::select(vec![
                SessionType::Wayland,
                SessionType::X11,
                SessionType::Unknown,
            ]),
            proptest::sample::select(all_desktops()),
            proptest::sample::select(FAMILIES.to_vec()),
            proptest::sample::select(vec![Sandbox::None, Sandbox::Flatpak, Sandbox::Snap]),
            any::<bool>(),
        )
            .prop_map(
                |(session, desktop, distro, sandbox, nix_shell)| HostPlatform {
                    session,
                    desktop,
                    distro,
                    sandbox,
                    nix_shell,
                },
            )
    }

    proptest! {
        /// Every hint says something; `nix develop` only inside a Nix
        /// shell; Nix words only on NixOS or in a Nix shell; a command is
        /// the family's installer with exactly the hint's packages, or
        /// `nix develop`, or Fedora's swap.
        #[test]
        fn hints_are_for_this_platform(
            need in proptest::sample::select(Need::ALL.to_vec()),
            platform in any_host(),
        ) {
            let hint = install_hint(need, &platform);
            let line = hint.line();
            prop_assert!(!line.trim().is_empty(), "{need:?}: {hint:?}");
            if !platform.nix_shell {
                prop_assert!(!line.contains("nix develop"), "{line}");
                prop_assert!(hint.command.as_deref() != Some("nix develop"));
            }
            if !platform.nix_shell && platform.distro != DistroFamily::NixOs {
                prop_assert!(!line.to_lowercase().contains("nix"), "{line}");
            }
            if let Some(command) = &hint.command {
                let packages: Vec<&str> = hint.packages.iter().map(String::as_str).collect();
                let expected = platform.distro.install_command(&packages);
                prop_assert!(
                    command == "nix develop"
                        || command.starts_with("sudo dnf swap ")
                        || Some(command) == expected.as_ref(),
                    "{command} for {packages:?} on {:?}", platform.distro
                );
            }
        }

        /// The desktop and the session change the portal's hint alone, and
        /// the family never changes it.
        #[test]
        fn hints_follow_their_inputs_only(
            need in proptest::sample::select(Need::ALL.to_vec()),
            a in any_host(),
            b in any_host(),
        ) {
            let elsewhere = HostPlatform {
                distro: a.distro,
                nix_shell: a.nix_shell,
                sandbox: a.sandbox,
                ..b.clone()
            };
            if need == Need::ScreenCastPortal {
                let other_family = HostPlatform {
                    distro: b.distro,
                    nix_shell: b.nix_shell,
                    ..a.clone()
                };
                prop_assert_eq!(install_hint(need, &a), install_hint(need, &other_family));
            } else {
                prop_assert_eq!(install_hint(need, &a), install_hint(need, &elsewhere));
            }
        }
    }
}
