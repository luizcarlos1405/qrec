//! Qrec on X11 (issue #9): the real binary with the real capture backend
//! against a private Xvfb (`testkit::x11::Xvfb`: its own display number,
//! never 0 or the developer's, and its own cookie), whose screen is split
//! into two RandR monitors as `xrandr --setmonitor` would. Every command
//! gets only that display (`TestEnv::command` removes the session's
//! variables), no PulseAudio server and a `niri` that tells when it runs.
//!
//! Ignored: they start an X server. `scripts/check-full` runs them, as
//! does `scripts/check-quick x11`:
//!
//! ```sh
//! cargo nextest run -p qrec --run-ignored only --test x11
//! ```

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use qrec_shell::testkit::x11::Xvfb;
use qrec_shell::testkit::{TestEnv, run_tool};
use serde_json::Value;

/// The `qrec` under test, as `cli.rs` picks it.
fn qrec_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_qrec")
        .map_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_qrec")), PathBuf::from)
}

/// A private X server whose 1920×1080 screen holds `VIRT-L` (the left
/// half) and `VIRT-R` (the right half), and an isolated env whose `PATH`
/// starts with a `niri` that records its arguments in `niri-calls`.
struct X11Setup {
    xvfb: Xvfb,
    env: TestEnv,
    bin: PathBuf,
}

impl X11Setup {
    fn new() -> Self {
        let xvfb = Xvfb::start(1920, 1080);
        xvfb.set_monitor("VIRT-L", 0, 0, 960, 1080);
        xvfb.set_monitor("VIRT-R", 960, 0, 960, 1080);
        let env = TestEnv::new();
        let bin = env.root().join("bin");
        fs::create_dir(&bin).unwrap();
        let niri = bin.join("niri");
        let calls = env.root().join("niri-calls");
        fs::write(
            &niri,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\necho '{{}}'\n",
                calls.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&niri, fs::Permissions::from_mode(0o755)).unwrap();
        Self { xvfb, env, bin }
    }

    /// `qrec <args>` in `dir` on the X server, with `vars` on top.
    fn qrec(&self, dir: &Path, args: &[&str], vars: &[(&str, &str)]) -> (Value, String) {
        let mut path = OsString::from(&self.bin);
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let mut command = self.env.command(&qrec_bin());
        command
            .current_dir(dir)
            .args(args)
            .envs(self.xvfb.vars())
            .env("PATH", path)
            .env("QREC_CAPTURE_BACKEND", "real")
            .env("PULSE_SERVER", "unix:/nonexistent")
            .envs(vars.iter().copied());
        let out = run_tool(&mut command, Duration::from_secs(120));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "qrec {args:?}: {}\n{stdout}\n{}",
            out.status,
            out.stderr
        );
        let reply = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{stdout}: {e}"));
        (reply, out.stderr)
    }

    /// What the fake `niri` was run with, one line per run.
    fn niri_calls(&self) -> String {
        fs::read_to_string(self.env.root().join("niri-calls")).unwrap_or_default()
    }
}

fn screens(data: &Value) -> Vec<(String, i64, i64, i64, i64)> {
    data["screens"]
        .as_array()
        .unwrap_or_else(|| panic!("no screens in {data}"))
        .iter()
        .map(|s| {
            assert_eq!(s["scale"], 1.0, "{s}");
            let n = |key: &str| s[key].as_i64().unwrap();
            (
                s["output"].as_str().unwrap().to_owned(),
                n("x"),
                n("y"),
                n("width"),
                n("height"),
            )
        })
        .collect()
}

/// `sources list` shows the RandR monitors (and the automatic one of the
/// output they split), and never asks niri; a screen layer takes one. The
/// probe of issue #9 listed no screens and got `not_found` here.
#[test]
#[ignore = "starts a private Xvfb (scripts/check-full)"]
fn x11_screens_are_the_randr_monitors() {
    let x = X11Setup::new();
    let dir = x.env.empty_dir("x11");

    let (list, stderr) = x.qrec(&dir, &["--json", "sources", "list"], &[]);
    let data = &list["data"];
    assert_eq!(
        screens(data),
        [
            ("VIRT-L".to_owned(), 0, 0, 960, 1080),
            ("screen".to_owned(), 0, 0, 1920, 1080),
            ("VIRT-R".to_owned(), 960, 0, 960, 1080),
        ],
        "{stderr}"
    );
    let warnings = data["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        !warnings
            .iter()
            .any(|w| w.as_str().unwrap().starts_with("screens:")),
        "{warnings:?}"
    );

    let (add, stderr) = x.qrec(
        &dir,
        &[
            "--json", "scene", "layer", "add", "screen", "--output", "VIRT-R",
        ],
        &[],
    );
    assert_eq!(add["data"]["created"][0], "L1", "{add}\n{stderr}");
    let (status, _) = x.qrec(&dir, &["--json", "status"], &[]);
    let layer = &status["data"]["project"]["scene"]["layers"][0];
    assert_eq!(layer["source"]["output"], "VIRT-R", "{layer}");
    assert_eq!(x.niri_calls(), "", "niri msg never runs on X11");
}

/// `QREC_SCREEN_BACKEND=portal` overrides the session: the Wayland path
/// (no compositor here, then `niri msg`) instead of RandR.
#[test]
#[ignore = "starts a private Xvfb (scripts/check-full)"]
fn the_screen_backend_override_wins_over_the_session() {
    let x = X11Setup::new();
    let dir = x.env.empty_dir("portal");
    let (list, stderr) = x.qrec(
        &dir,
        &["--json", "sources", "list"],
        &[("QREC_SCREEN_BACKEND", "portal")],
    );
    assert_eq!(screens(&list["data"]), [], "{stderr}");
    assert_eq!(x.niri_calls(), "msg --json outputs\n");
}
