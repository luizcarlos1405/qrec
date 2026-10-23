//! The platform this process runs on (issue #9), detected once.
//!
//! [`current`] reads the variables in [`ENV_KEYS`], os-release
//! (`/etc/os-release`, else `/usr/lib/os-release`, as os-release(5) says),
//! whether `/.flatpak-info` exists and whether this executable lies under
//! `$SNAP`, then keeps the core's pure [`detect`] of that for the rest of
//! the run. `main()` calls it before any thread starts, the same moment
//! the GPU environment is decided (A§8.10), so every later reader sees
//! the session the process started in. No device is opened and GStreamer
//! isn't initialized.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use qrec_core::platform::{
    Desktop, DistroFamily, ENV_KEYS, HostPlatform, PlatformEnv, Sandbox, SessionType, detect,
};

/// More than any os-release holds (they are under 1 KiB).
const OS_RELEASE_MAX: u64 = 64 * 1024;

static PLATFORM: OnceLock<HostPlatform> = OnceLock::new();

/// The platform, detected on the first call (`main()` makes that call
/// before any thread starts).
pub fn current() -> &'static HostPlatform {
    PLATFORM.get_or_init(|| detect(&env_from_process()))
}

/// What detection reads, from this process and the root file system.
pub fn env_from_process() -> PlatformEnv {
    let exe = std::env::current_exe().ok();
    env_from(Path::new("/"), |key| std::env::var_os(key), exe.as_deref())
}

/// [`env_from_process`] with the files under `root`, the variables from
/// `var` and this executable's path `exe` (tests pass a temporary root and
/// their own variables and path).
fn env_from(
    root: &Path,
    var: impl Fn(&str) -> Option<OsString>,
    exe: Option<&Path>,
) -> PlatformEnv {
    let vars = ENV_KEYS
        .iter()
        .filter_map(|key| {
            let value = var(key)?;
            Some((key.to_string(), value.to_string_lossy().into_owned()))
        })
        .collect();
    PlatformEnv {
        vars,
        os_release: ["etc/os-release", "usr/lib/os-release"]
            .iter()
            .find_map(|file| read_small(&root.join(file))),
        flatpak_info: root.join(".flatpak-info").exists(),
        snap: var("SNAP")
            .map(PathBuf::from)
            .filter(|snap| snap.is_absolute())
            .zip(exe)
            .is_some_and(|(snap, exe)| inside(exe, &snap)),
    }
}

/// `exe` lies under the directory `dir`, as given or with symlinks
/// resolved (`/snap` links to `/var/lib/snapd/snap` on Fedora and Arch).
/// Only Qrec's own snap puts it there: a classic snap's terminal leaks
/// its `SNAP` into a host qrec, whose executable is elsewhere.
fn inside(exe: &Path, dir: &Path) -> bool {
    let resolved = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    exe.starts_with(dir) || resolved(exe).starts_with(resolved(dir))
}

/// A regular file's text (at most [`OS_RELEASE_MAX`] bytes, lossy UTF-8);
/// `None` if it can't be read.
fn read_small(path: &Path) -> Option<String> {
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(OS_RELEASE_MAX)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    #[test]
    fn reads_the_files_and_only_its_variables() {
        let root = tempfile::tempdir().unwrap();
        // No os-release at all.
        let env = env_from(root.path(), |_| None, None);
        assert_eq!(env, PlatformEnv::default());

        // /usr/lib/os-release when /etc has none; /etc's when both exist.
        fs::create_dir_all(root.path().join("usr/lib")).unwrap();
        fs::write(root.path().join("usr/lib/os-release"), "ID=fedora\n").unwrap();
        let env = env_from(root.path(), |_| None, None);
        assert_eq!(env.os_release.as_deref(), Some("ID=fedora\n"));
        fs::create_dir_all(root.path().join("etc")).unwrap();
        fs::write(root.path().join("etc/os-release"), "ID=debian\n").unwrap();
        fs::write(root.path().join(".flatpak-info"), "[Application]\n").unwrap();
        let env = env_from(
            root.path(),
            |key| match key {
                "DISPLAY" => Some(OsString::from(":1")),
                "XDG_CURRENT_DESKTOP" => Some(OsString::from_vec(b"XFCE\xff".to_vec())),
                "HOME" => Some(OsString::from("/home/me")),
                _ => None,
            },
            None,
        );
        assert_eq!(env.os_release.as_deref(), Some("ID=debian\n"));
        assert!(env.flatpak_info);
        let vars: Vec<(&str, &str)> = env
            .vars
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            vars,
            [("DISPLAY", ":1"), ("XDG_CURRENT_DESKTOP", "XFCE\u{fffd}")]
        );
        let platform = detect(&env);
        assert_eq!(platform.session, SessionType::X11);
        assert_eq!(platform.distro, DistroFamily::Debian);
        assert_eq!(platform.sandbox, Sandbox::Flatpak);
    }

    #[test]
    fn a_directory_is_not_an_os_release() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc/os-release")).unwrap();
        fs::create_dir_all(root.path().join("usr/lib")).unwrap();
        fs::write(root.path().join("usr/lib/os-release"), "ID=arch\n").unwrap();
        let env = env_from(root.path(), |_| None, None);
        assert_eq!(env.os_release.as_deref(), Some("ID=arch\n"));
    }

    #[test]
    fn snap_only_when_this_executable_is_inside_it() {
        let root = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(root.path()).unwrap();
        let snaps = root.join("var/lib/snapd/snap");
        let write_exe = |path: &Path| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        };
        let snap = |dir: PathBuf| move |key: &str| (key == "SNAP").then(|| dir.clone().into());

        // VS Code, a classic snap, leaks its SNAP into the host qrec its
        // terminal starts.
        fs::create_dir_all(snaps.join("code/200")).unwrap();
        let host_exe = root.join("usr/bin/qrec");
        write_exe(&host_exe);
        let env = env_from(&root, snap(snaps.join("code/200")), Some(&host_exe));
        assert!(!env.snap);
        assert_eq!(detect(&env).sandbox, Sandbox::None);

        // Qrec's own snap, also through the /snap symlink of Fedora and Arch.
        let snap_exe = snaps.join("qrec/12/usr/bin/qrec");
        write_exe(&snap_exe);
        std::os::unix::fs::symlink(&snaps, root.join("snap")).unwrap();
        for dir in [snaps.join("qrec/12"), root.join("snap/qrec/12")] {
            let env = env_from(&root, snap(dir.clone()), Some(&snap_exe));
            assert!(env.snap, "{dir:?}");
            assert_eq!(detect(&env).sandbox, Sandbox::Snap);
        }

        // An empty or relative SNAP, or no known executable: no snap.
        for dir in ["", "usr", "."] {
            assert!(!env_from(&root, snap(dir.into()), Some(&host_exe)).snap);
        }
        assert!(!env_from(&root, snap(snaps.join("qrec/12")), None).snap);
    }
}
