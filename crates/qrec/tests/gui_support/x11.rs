//! The harness's X11 session (docs/GUI.md G§8.4, issue #9): `qrec`'s
//! window on a private Xvfb instead of the nested sway, for
//! `QREC_TEST_DISPLAY=x11` or a case's
//! [`Options::x11`](super::Options::x11) (the `gui_x11` suite's).
//!
//! - The server is the testkit's [`Xvfb`]: `-displayfd`, `-noreset`, its
//!   own cookie (`-auth`), RandR and GLX, no TCP, on a free display from
//!   64 on, never `:0` or the inherited `DISPLAY`'s ([`private_display`]
//!   checks it again). Its screen is the options' size in device px. The
//!   harness logs its number and pid, and keeps its log with the case's
//!   (`<shots>/logs/<case>-xvfb.log`).
//! - No window manager (i3 may come later, for tiling parity): GTK maps a
//!   window at 0,0 at the size it asks for (`window::first_size`: 90 % of
//!   the screen, at most 1600×1000) and gives it the keyboard focus as it
//!   presents it. With no compositing manager either, GTK draws its solid
//!   frame around the editor, so the window's (0, 0) lies inside its X
//!   window ([`X11Session::origin`]: 5 px in at scale 1).
//! - A window's variables ([`X11Session::window_env`]): `DISPLAY`,
//!   `XAUTHORITY`, `GDK_BACKEND=x11` (none with `Launch::session_backend`:
//!   the window picks the display itself) and `GDK_SCALE`, 1 or 2 (GTK's
//!   X11 backend scales by whole numbers: no 1.5 run); no `WAYLAND_*`. The
//!   bus, settings, design inventory and fake capture backend are the sway
//!   session's. Xvfb has no GPU: pixman's runs draw with Cairo
//!   (`GDK_DISABLE=gl,vulkan`), gles2's with GL on llvmpipe
//!   (`LIBGL_ALWAYS_SOFTWARE=1`, Mesa's EGL pinned), never on a render
//!   node.
//! - Tools, each with a deadline and this display's variables only
//!   ([`X11Session::tool`]): keys through `xdotool` (the suites' `wtype`
//!   arguments translated: [`xdotool_keys`]), clicks through `xdotool
//!   mousemove --sync`, shots through `ffmpeg -f x11grab`, a window's X
//!   window and rect through `xdotool search --pid`, its close through
//!   [`Xvfb::delete_window`] (a window manager's `WM_DELETE_WINDOW`), the
//!   clipboard through `xclip`. There is no virtual pointer
//!   (`Nested::pointer`) yet: a case clicks with [`X11Session::click`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use qrec_shell::testkit::run_tool;
use qrec_shell::testkit::x11::{Xvfb, display_number};

use super::{Gui, TOOL_TIMEOUT};

/// The window's WM_CLASS: GLib's prgname, the app id.
pub const CLASS: &str = qrec_gui::app::APP_ID;

/// A private X server with no window manager.
pub struct X11Session {
    xvfb: Xvfb,
    /// The screen, in device px.
    screen: (u32, u32),
    /// `GDK_SCALE`.
    scale: u32,
    /// The env's `XDG_RUNTIME_DIR`, for the tools.
    runtime: PathBuf,
}

impl X11Session {
    /// Starts the server of case `case`: a `width`×`height` screen (device
    /// px) for windows at `scale`. Panics on a scale GTK's X11 backend
    /// can't do and on a display that isn't private.
    pub fn start(case: &str, (width, height): (u32, u32), scale: f64, runtime: PathBuf) -> Self {
        let scale = gdk_scale(scale).unwrap_or_else(|e| panic!("{case}: {e}"));
        let xvfb = Xvfb::start(width, height);
        if let Err(e) = private_display(xvfb.number(), std::env::var("DISPLAY").ok().as_deref()) {
            panic!("{case}: {e}");
        }
        eprintln!(
            "gui_support: {case} on Xvfb {} (pid {}): a {width}x{height} screen, GDK_SCALE={scale}, \
             no window manager; its log {}",
            xvfb.display(),
            xvfb.pid(),
            xvfb.log().display()
        );
        Self {
            xvfb,
            screen: (width, height),
            scale,
            runtime,
        }
    }

    /// `:N`.
    pub fn display(&self) -> String {
        self.xvfb.display()
    }

    /// `GDK_SCALE`: device px per logical px.
    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// The server's log (the case keeps a copy).
    pub fn log(&self) -> PathBuf {
        self.xvfb.log()
    }

    /// The variables that put a window on this display: `DISPLAY`,
    /// `XAUTHORITY`, `GDK_BACKEND=x11` and `GDK_SCALE`.
    pub fn window_env(&self) -> Vec<(&'static str, OsString)> {
        let mut vars = Vec::from(self.xvfb.vars());
        vars.push(("GDK_BACKEND", "x11".into()));
        vars.push(("GDK_SCALE", self.scale.to_string().into()));
        vars
    }

    /// `program` on this display only: its `DISPLAY` and `XAUTHORITY`,
    /// the env's runtime dir and no Wayland display (the test's own
    /// variables are the person's session).
    pub fn tool(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .envs(self.xvfb.vars())
            .env("XDG_RUNTIME_DIR", &self.runtime);
        command
    }

    /// `xdotool <args>`'s output, or `None` when it failed (a search that
    /// found nothing).
    fn xdotool(&self, args: &[&str]) -> Option<String> {
        let out = run_tool(self.tool("xdotool").args(args), TOOL_TIMEOUT);
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Process `pid`'s window and its rect (device px): the largest of its
    /// mapped X windows of Qrec's class (a popover is a window of its own).
    pub fn toplevel(&self, pid: u32) -> Option<(u32, [i32; 4])> {
        let pid = pid.to_string();
        let found = self.xdotool(&[
            "search",
            "--all",
            "--onlyvisible",
            "--pid",
            &pid,
            "--class",
            CLASS,
            "getwindowgeometry",
            "--shell",
            "%@",
        ])?;
        parse_geometries(&found)
            .into_iter()
            .max_by_key(|(_, [_, _, w, h])| i64::from(*w) * i64::from(*h))
    }

    /// Gives X window `id` the keyboard focus (`XSetInputFocus`, which a
    /// server with no window manager takes from any client). It doesn't
    /// wait: the caller asks the window whether it is active.
    pub fn focus(&self, id: u32) {
        let _ = self.xdotool(&["windowfocus", &id.to_string()]);
    }

    /// The X window with the keyboard focus, if one has it.
    pub fn focused(&self) -> Option<u32> {
        self.xdotool(&["getwindowfocus"])?.trim().parse().ok()
    }

    /// Asks X window `id` to close, as a window manager's close button
    /// does.
    pub fn close(&self, id: u32) {
        self.xvfb.delete_window(id);
    }

    /// Where `gui`'s window coordinates (the test channel's regions) start
    /// on the screen, in device px: its X window's corner plus the frame
    /// GTK draws around the editor (the window's own bounds start that far
    /// above and to the left of its (0, 0)).
    pub fn origin(&self, gui: &Gui) -> (f64, f64) {
        let (_, [x, y, _, _]) = self
            .toplevel(gui.pid())
            .unwrap_or_else(|| panic!("window {} has no X window", gui.pid()));
        let bounds = gui
            .regions(&["qrec"])
            .get("qrec")
            .copied()
            .unwrap_or_else(|| panic!("window {} gives no bounds of its own", gui.pid()));
        let s = f64::from(self.scale);
        (f64::from(x) - bounds[0] * s, f64::from(y) - bounds[1] * s)
    }

    /// A left click at (`x`, `y`) device px of the screen, once the
    /// pointer is there.
    pub fn click_at(&self, x: f64, y: f64) {
        let (x, y) = (x.round().to_string(), y.round().to_string());
        let out = run_tool(
            self.tool("xdotool")
                .args(["mousemove", "--sync", &x, &y, "click", "1"]),
            TOOL_TIMEOUT,
        );
        assert!(
            out.status.success(),
            "xdotool: a click at {x},{y}: {}",
            out.stderr
        );
    }

    /// A left click on the center of `gui`'s widget `name`.
    pub fn click(&self, gui: &Gui, name: &str) {
        let [x, y, w, h] = gui
            .regions(&[name])
            .get(name)
            .copied()
            .unwrap_or_else(|| panic!("no widget {name} to click"));
        let (ox, oy) = self.origin(gui);
        let s = f64::from(self.scale);
        self.click_at(ox + (x + w / 2.0) * s, oy + (y + h / 2.0) * s);
    }

    /// An `xdotool` run that lets go of every key `args` (a `wtype` run's)
    /// press, for a run killed before it did; `None` when they press none.
    pub fn release(&self, args: &[&str]) -> Option<Command> {
        let pressed = pressed_keys(args);
        if pressed.is_empty() {
            return None;
        }
        let mut command = self.tool("xdotool");
        for key in pressed {
            command.args(["keyup", &key]);
        }
        Some(command)
    }

    /// Types `args` (a [`xdotool_keys`] translation) into the window with
    /// the focus.
    pub fn type_keys(&self, args: &[String]) {
        if args.is_empty() {
            return;
        }
        let out = run_tool(self.tool("xdotool").args(args), TOOL_TIMEOUT);
        assert!(out.status.success(), "xdotool {args:?}: {}", out.stderr);
    }

    /// The whole screen into `path`: one x11grab frame, without the
    /// pointer.
    pub fn screenshot(&self, path: &Path) {
        let (w, h) = self.screen;
        let out = run_tool(
            self.tool("ffmpeg")
                .args(["-v", "error", "-y", "-f", "x11grab", "-draw_mouse", "0"])
                .args(["-video_size", &format!("{w}x{h}")])
                .args(["-i", &format!("{}+0,0", self.display())])
                .args(["-frames:v", "1"])
                .arg(path),
            TOOL_TIMEOUT,
        );
        assert!(out.status.success(), "x11grab failed: {}", out.stderr);
    }

    /// What the clipboard's owner gives for `target` (`xclip -selection
    /// clipboard -t <target> -o`), or `None` when it gives nothing.
    pub fn clipboard(&self, target: &str) -> Option<Vec<u8>> {
        let out = run_tool(
            self.tool("xclip")
                .args(["-selection", "clipboard", "-t", target, "-o"]),
            TOOL_TIMEOUT,
        );
        out.status.success().then_some(out.stdout)
    }

    /// `xprop -id <id> <properties>`'s text.
    pub fn xprop(&self, id: u32, properties: &[&str]) -> String {
        let out = run_tool(
            self.tool("xprop")
                .args(["-id", &id.to_string()])
                .args(properties),
            TOOL_TIMEOUT,
        );
        assert!(out.status.success(), "xprop: {}", out.stderr);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// GTK's X11 backend scales by whole numbers (`GDK_SCALE`): an X11 run is
/// at 1 or 2.
pub fn gdk_scale(scale: f64) -> Result<u32, String> {
    if scale == 1.0 {
        Ok(1)
    } else if scale == 2.0 {
        Ok(2)
    } else {
        Err(format!(
            "an X11 session runs at scale 1 or 2 (GDK_SCALE is a whole number), not {scale} \
             (QREC_TEST_SCALE)"
        ))
    }
}

/// Refuses an X server that isn't the test's own: display `:0` (the first
/// local server, a person's session) and the inherited `DISPLAY`'s (the
/// session the tests were started from), which the testkit never picks
/// either.
pub fn private_display(number: u32, inherited: Option<&str>) -> Result<(), String> {
    if number == 0 {
        return Err("display :0 is a person's X server, never a test's (G§8.4)".to_owned());
    }
    if let Some(inherited) = inherited
        && display_number(inherited) == Some(number)
    {
        return Err(format!(
            "display :{number} is the inherited DISPLAY={inherited}, never a test's (G§8.4)"
        ));
    }
    Ok(())
}

/// `xdotool getwindowgeometry --shell`'s windows: each block's `WINDOW`,
/// `X`, `Y`, `WIDTH` and `HEIGHT`, as (id, [x, y, width, height]); a block
/// missing one is left out.
pub fn parse_geometries(text: &str) -> Vec<(u32, [i32; 4])> {
    let mut found = Vec::new();
    let mut current: Option<(u32, [Option<i32>; 4])> = None;
    let finish = |current: Option<(u32, [Option<i32>; 4])>, found: &mut Vec<(u32, [i32; 4])>| {
        if let Some((id, [Some(x), Some(y), Some(w), Some(h)])) = current {
            found.push((id, [x, y, w, h]));
        }
    };
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        let at = match key {
            "WINDOW" => {
                finish(current.take(), &mut found);
                current = value.parse().ok().map(|id| (id, [None; 4]));
                continue;
            }
            "X" => 0,
            "Y" => 1,
            "WIDTH" => 2,
            "HEIGHT" => 3,
            _ => continue,
        };
        if let Some((_, rect)) = current.as_mut() {
            rect[at] = value.parse().ok();
        }
    }
    finish(current, &mut found);
    found
}

/// `xdotool`'s arguments for a `wtype` run's (`-k`, `-P`/`-p`, `-M`/`-m`,
/// `-s`, `-d`, text), so the suites' key runs need no change: `-k` is
/// `key`, `-P`/`-p` `keydown`/`keyup`, `-M`/`-m` the modifier's
/// `keydown`/`keyup`, `-s` `sleep` (in seconds), a text `type --args 1`
/// (at `-d`'s delay): a bare `type` takes every argument after it as its
/// text, so the run's later keys and sleeps would be typed, not run. Then
/// `hold_ms` more (a held run's keyboard), and the keys still down go up,
/// as they do when `wtype` exits and its keyboard goes.
pub fn xdotool_keys(args: &[&str], hold_ms: u32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut down: Vec<String> = Vec::new();
    let mut delay: Option<String> = None;
    let mut rest = args.iter();
    while let Some(&arg) = rest.next() {
        let mut value = |flag: &str| {
            (*rest
                .next()
                .unwrap_or_else(|| panic!("wtype {flag} needs a value: {args:?}")))
            .to_owned()
        };
        match arg {
            "-k" => out.extend(["key".to_owned(), value("-k")]),
            "-P" | "-M" => {
                let key = value(arg);
                let key = if arg == "-M" { modifier(&key) } else { key };
                out.extend(["keydown".to_owned(), key.clone()]);
                down.push(key);
            }
            "-p" | "-m" => {
                let key = value(arg);
                let key = if arg == "-m" { modifier(&key) } else { key };
                down.retain(|k| *k != key);
                out.extend(["keyup".to_owned(), key]);
            }
            "-s" => {
                let ms: u32 = value("-s")
                    .parse()
                    .unwrap_or_else(|e| panic!("wtype -s takes milliseconds ({e}): {args:?}"));
                out.extend(["sleep".to_owned(), seconds(ms)]);
            }
            "-d" => delay = Some(value("-d")),
            text => {
                assert!(
                    !text.starts_with('-'),
                    "no xdotool for wtype's {text}: {args:?}"
                );
                // Just this argument: the commands after it still chain.
                out.extend(["type", "--args", "1"].map(str::to_owned));
                if let Some(ms) = &delay {
                    out.extend(["--delay".to_owned(), ms.clone()]);
                }
                out.push(text.to_owned());
            }
        }
    }
    if hold_ms > 0 {
        out.extend(["sleep".to_owned(), seconds(hold_ms)]);
    }
    for key in down.into_iter().rev() {
        out.extend(["keyup".to_owned(), key]);
    }
    out
}

/// The keys a `wtype` run's `args` press (`-P`'s and `-M`'s), as xdotool
/// names them.
fn pressed_keys(args: &[&str]) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = args.iter();
    while let Some(&arg) = rest.next() {
        match arg {
            "-P" => keys.extend(rest.next().map(|key| (*key).to_owned())),
            "-M" => keys.extend(rest.next().map(|key| modifier(key))),
            "-k" | "-p" | "-m" | "-s" | "-d" => {
                rest.next();
            }
            _ => {}
        }
    }
    keys
}

/// A `wtype` modifier's key for xdotool.
fn modifier(name: &str) -> String {
    match name {
        "shift" | "ctrl" | "alt" => name,
        "logo" | "win" => "super",
        "altgr" => "ISO_Level3_Shift",
        "capslock" => "Caps_Lock",
        other => panic!("no wtype modifier {other}"),
    }
    .to_owned()
}

/// `ms` as xdotool's `sleep` takes it: seconds.
fn seconds(ms: u32) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Never `:0`, never the display the tests were started from (G§8.4):
    /// whatever form the inherited `DISPLAY` takes.
    #[test]
    fn the_harness_refuses_display_0_and_the_inherited_one() {
        assert!(private_display(0, None).is_err());
        assert!(private_display(0, Some(":64")).is_err());
        for inherited in [":64", ":64.0", "unix:64", "localhost:64.0"] {
            let refused = private_display(64, Some(inherited));
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|e| e.contains("inherited DISPLAY")),
                "{inherited}: {refused:?}"
            );
        }
        assert_eq!(private_display(64, None), Ok(()));
        assert_eq!(private_display(64, Some(":0")), Ok(()));
        assert_eq!(private_display(65, Some(":64")), Ok(()));
        assert_eq!(private_display(64, Some("nowhere")), Ok(()));
    }

    /// GTK's X11 backend scales by whole numbers: a 1.5 run fails at once.
    #[test]
    fn x11_runs_are_at_scale_1_or_2() {
        assert_eq!(gdk_scale(1.0), Ok(1));
        assert_eq!(gdk_scale(2.0), Ok(2));
        for scale in [1.5, 1.25, 3.0, 0.5] {
            assert!(gdk_scale(scale).is_err(), "{scale}");
        }
    }

    /// The suites' `wtype` runs, as xdotool types them: a text types just
    /// itself (`--args 1`), and the keys and sleeps after it still run.
    #[test]
    fn wtype_runs_translate_to_xdotool() {
        let rows: [(&[&str], u32, &str); 11] = [
            (&["-k", "Escape"], 0, "key Escape"),
            (
                &["-M", "ctrl", "-k", "q", "-m", "ctrl"],
                0,
                "keydown ctrl key q keyup ctrl",
            ),
            (
                &["-P", "r", "-s", "1500", "-p", "r"],
                0,
                "keydown r sleep 1.500 keyup r",
            ),
            (&["-k", "Tab", "-k", "Return"], 0, "key Tab key Return"),
            (&["#12", "-k", "Return"], 0, "type --args 1 #12 key Return"),
            (
                &["-M", "ctrl", "-k", "a", "-m", "ctrl", "500", "-k", "Return"],
                0,
                "keydown ctrl key a keyup ctrl type --args 1 500 key Return",
            ),
            (
                &["-M", "shift", "-k", "ISO_Left_Tab", "-m", "shift"],
                0,
                "keydown shift key ISO_Left_Tab keyup shift",
            ),
            // A held run: its keys, the hold, then what is still down goes up.
            (&["-P", "Left"], 2500, "keydown Left sleep 2.500 keyup Left"),
            (
                &["-M", "ctrl", "-s", "1200", "-m", "ctrl"],
                400,
                "keydown ctrl sleep 1.200 keyup ctrl sleep 0.400",
            ),
            (
                &[
                    "-M", "ctrl", "-k", "a", "-m", "ctrl", "1280", "-s", "2500", "-k", "Return",
                ],
                5000,
                "keydown ctrl key a keyup ctrl type --args 1 1280 sleep 2.500 key Return \
                 sleep 5.000",
            ),
            (
                &["-d", "20", "snapping", "-M", "logo"],
                0,
                "type --args 1 --delay 20 snapping keydown super keyup super",
            ),
        ];
        for (wtype, hold, want) in rows {
            assert_eq!(xdotool_keys(wtype, hold).join(" "), want, "{wtype:?}");
        }
        assert!(xdotool_keys(&[], 0).is_empty());
        assert_eq!(xdotool_keys(&[], 250), ["sleep", "0.250"]);
        // What a held run killed early lets go of.
        assert_eq!(
            pressed_keys(&[
                "-M", "ctrl", "-k", "a", "-P", "Left", "-s", "300", "-p", "x"
            ]),
            ["ctrl", "Left"]
        );
        assert!(pressed_keys(&["-k", "Tab", "#12"]).is_empty());
    }

    /// `getwindowgeometry --shell` for each window of a search.
    #[test]
    fn window_geometries_parse() {
        let text = "WINDOW=2097156\nX=0\nY=0\nWIDTH=1600\nHEIGHT=1000\nSCREEN=0\n\
                    WINDOW=2097170\nX=40\nY=-3\nWIDTH=200\nHEIGHT=80\nSCREEN=0\n\
                    WINDOW=7\nX=1\n";
        assert_eq!(
            parse_geometries(text),
            [
                (2_097_156, [0, 0, 1600, 1000]),
                (2_097_170, [40, -3, 200, 80])
            ]
        );
        assert!(parse_geometries("").is_empty());
    }
}
