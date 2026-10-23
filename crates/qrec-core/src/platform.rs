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
}
