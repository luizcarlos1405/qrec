//! A private X server for X11 tests (issue #9), never the developer's:
//! `Xvfb` with RandR and GLX, no TCP, its own cookie, and no reset when
//! its last client leaves (a reset drops the RandR monitors a test set).
//!
//! The display is a free number from 64 on (never 0 or the inherited
//! `DISPLAY`'s), and the server says on `-displayfd` once it takes
//! connections, within [`STARTUP_TIMEOUT`]. Its output goes to a log in its
//! own short temp dir under `/tmp`, beside the `-auth` file; the panic of a
//! failed start quotes it. It runs in a process group of its own, which
//! dies with the thread that started it ([`die_with_parent`]) and is
//! killed on drop.
//!
//! A command gets the display with [`Xvfb::vars`] (after
//! `TestEnv::command`, which removes the developer's); the test itself
//! talks to it through [`Xvfb::connect`], whose waits end
//! [`CONNECTION_TIMEOUT`] after it opens, so a server that stops answering
//! fails the test instead of hanging it (the testkit's own panics quote its
//! log).

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::reexports::x11rb_protocol::parse_display::ConnectAddress;
use x11rb::rust_connection::{DefaultStream, RustConnection};

pub use crate::sources::x11::DeadlineStream;

use super::process::{Signal, die_with_parent, signal_group};

/// How long the server may take to say its display.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a connection's waits may last, from its opening
/// ([`Xvfb::connect`]): connect again for later requests.
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

/// How long it may take to exit once signalled.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The first display number tried (low ones are real servers').
const FIRST_DISPLAY: u32 = 64;

/// How many numbers a start tries.
const ATTEMPTS: u32 = 16;

const COOKIE_NAME: &[u8] = b"MIT-MAGIC-COOKIE-1";

/// A running private `Xvfb`.
pub struct Xvfb {
    child: Child,
    number: u32,
    /// The cookie in [`Xvfb::xauthority`].
    cookie: [u8; 16],
    dir: TempDir,
}

impl Xvfb {
    /// Starts one with a `width`×`height`×24 screen. Panics (with its log)
    /// when none starts.
    pub fn start(width: u32, height: u32) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("qx-")
            .tempdir_in("/tmp")
            .expect("create the Xvfb dir");
        let mut cookie = [0u8; 16];
        getrandom::fill(&mut cookie).expect("the OS random source is available");
        fs::write(dir.path().join("xauth"), xauth_file(&cookie)).expect("write the Xvfb cookie");
        let inherited = std::env::var("DISPLAY")
            .ok()
            .and_then(|d| display_number(&d));
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let mut failures = Vec::new();
        for number in candidates(std::process::id(), inherited) {
            if Instant::now() >= deadline {
                break;
            }
            if taken(number) {
                continue;
            }
            match spawn(dir.path(), number, (width, height), deadline) {
                Ok(child) => {
                    let xvfb = Xvfb {
                        child,
                        number,
                        cookie,
                        dir,
                    };
                    eprintln!(
                        "testkit: Xvfb {width}x{height} on {} (log {})",
                        xvfb.display(),
                        xvfb.log().display()
                    );
                    return xvfb;
                }
                Err(failure) => failures.push(format!(":{number}: {failure}")),
            }
        }
        panic!(
            "no private Xvfb started (the Nix dev shell has Xvfb):\n{}",
            failures.join("\n")
        );
    }

    /// `:N`.
    pub fn display(&self) -> String {
        format!(":{}", self.number)
    }

    pub fn number(&self) -> u32 {
        self.number
    }

    /// The `-auth` file, for `XAUTHORITY`.
    pub fn xauthority(&self) -> PathBuf {
        self.dir.path().join("xauth")
    }

    /// The server's output.
    pub fn log(&self) -> PathBuf {
        log_path(self.dir.path(), self.number)
    }

    /// `DISPLAY` and `XAUTHORITY` for a command that should use it.
    pub fn vars(&self) -> [(&'static str, OsString); 2] {
        [
            ("DISPLAY", self.display().into()),
            ("XAUTHORITY", self.xauthority().into()),
        ]
    }

    /// A connection to it (screen 0), with its cookie, whose waits end
    /// [`CONNECTION_TIMEOUT`] from now (then its requests fail).
    pub fn connect(&self) -> (RustConnection<DeadlineStream>, usize) {
        let socket = ConnectAddress::Socket(format!("/tmp/.X11-unix/X{}", self.number));
        let (stream, _) = DefaultStream::connect(&socket)
            .unwrap_or_else(|e| panic!("connect to {}: {e}{}", self.display(), self.log_tail()));
        let stream = DeadlineStream::new(stream, Instant::now() + CONNECTION_TIMEOUT);
        let conn = RustConnection::connect_to_stream_with_auth_info(
            stream,
            0,
            COOKIE_NAME.to_vec(),
            self.cookie.to_vec(),
        )
        .unwrap_or_else(|e| {
            panic!(
                "set up a connection to {}: {e}{}",
                self.display(),
                self.log_tail()
            )
        });
        (conn, 0)
    }

    /// `; its log (<path>) ends:` and the log's last lines, for a panic.
    fn log_tail(&self) -> String {
        format!(
            "; its log ({}) ends:\n  {}",
            self.log().display(),
            tail(&self.log())
        )
    }

    /// Adds a RandR monitor `name` (no output: a part of the screen, like
    /// `xrandr --setmonitor <name> <w>/<mm>x<h>/<mm>+<x>+<y> none`) and
    /// waits until the server has it.
    pub fn set_monitor(&self, name: &str, x: i16, y: i16, width: u16, height: u16) {
        let (conn, screen) = self.connect();
        let root = conn.setup().roots[screen].root;
        let done = || -> Result<(), Box<dyn std::error::Error>> {
            conn.extension_information(randr::X11_EXTENSION_NAME)?
                .ok_or("no RandR")?;
            conn.randr_query_version(1, 5)?.reply()?;
            let atom = conn.intern_atom(false, name.as_bytes())?.reply()?.atom;
            // 96 dpi.
            let mm = |px: u16| u32::from(px) * 254 / 960;
            let monitor = randr::MonitorInfo {
                name: atom,
                primary: false,
                automatic: false,
                x,
                y,
                width,
                height,
                width_in_millimeters: mm(width),
                height_in_millimeters: mm(height),
                outputs: Vec::new(),
            };
            conn.randr_set_monitor(root, monitor)?.check()?;
            Ok(())
        };
        done().unwrap_or_else(|e| {
            panic!(
                "set the monitor {name} on {}: {e}{}",
                self.display(),
                self.log_tail()
            )
        });
    }
}

impl Drop for Xvfb {
    /// SIGTERM to its group, then SIGKILL after 5 s; reaped.
    fn drop(&mut self) {
        stop(&mut self.child);
    }
}

/// Stops `child`'s process group and reaps it. Once reaped, its group is
/// never signalled again (its id may belong to another one by then).
fn stop(child: &mut Child) {
    let exits_within = |child: &mut Child, within: Duration| {
        let deadline = Instant::now() + within;
        loop {
            if !matches!(child.try_wait(), Ok(None)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    if exits_within(child, Duration::ZERO) {
        return;
    }
    signal_group(child.id(), Signal::Term);
    if exits_within(child, STOP_TIMEOUT) {
        return;
    }
    if !signal_group(child.id(), Signal::Kill) {
        let _ = child.kill();
    }
    if !exits_within(child, STOP_TIMEOUT) {
        eprintln!(
            "testkit: Xvfb (pid {}) outlived SIGKILL; left behind",
            child.id()
        );
    }
}

/// The display numbers a start tries: [`ATTEMPTS`] in a row from a place
/// the process id picks (so parallel test processes rarely race), from
/// [`FIRST_DISPLAY`] on, never `inherited`.
fn candidates(pid: u32, inherited: Option<u32>) -> impl Iterator<Item = u32> {
    let span = 900;
    (0..ATTEMPTS)
        .map(move |i| FIRST_DISPLAY + pid.wrapping_add(i) % span)
        .filter(move |&n| Some(n) != inherited && n != 0)
}

/// Another server has `number` (its lock or socket exists).
fn taken(number: u32) -> bool {
    Path::new(&format!("/tmp/.X{number}-lock")).exists()
        || Path::new(&format!("/tmp/.X11-unix/X{number}")).exists()
}

/// The display number of a `DISPLAY` value (`:1`, `:1.0`, `host:10.0`,
/// `unix:2`); `None` if it has none.
fn display_number(display: &str) -> Option<u32> {
    let (_, rest) = display.rsplit_once(':')?;
    rest.split('.').next()?.parse().ok()
}

/// One `.Xauthority` entry: any address (`FamilyWild`), any display
/// number, the MIT cookie. The server takes every cookie in its `-auth`
/// file; clients match it for whatever display it got.
fn xauth_file(cookie: &[u8; 16]) -> Vec<u8> {
    let mut entry = 0xFFFFu16.to_be_bytes().to_vec();
    for field in [&b""[..], b"", COOKIE_NAME, cookie] {
        entry.extend((field.len() as u16).to_be_bytes());
        entry.extend(field);
    }
    entry
}

fn log_path(dir: &Path, number: u32) -> PathBuf {
    dir.join(format!("xvfb-{number}.log"))
}

/// The last 8 lines of the log at `path`, joined by indented newlines.
fn tail(path: &Path) -> String {
    let log = fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<&str> = log.lines().rev().take(8).collect();
    lines.reverse();
    lines.join("\n  ")
}

/// Starts `Xvfb :<number>` and waits until it says the number on
/// `-displayfd`. `Err` (with its log's tail) when it exits first, says
/// another or doesn't say it by `deadline`.
fn spawn(dir: &Path, number: u32, (w, h): (u32, u32), deadline: Instant) -> Result<Child, String> {
    let log_file = log_path(dir, number);
    let log = File::create(&log_file).map_err(|e| format!("create its log: {e}"))?;
    let err_log = log.try_clone().map_err(|e| format!("dup its log: {e}"))?;
    let (reader, writer) = io::pipe().map_err(|e| format!("a pipe for -displayfd: {e}"))?;
    let fd = writer.as_raw_fd();
    let mut command = Command::new("Xvfb");
    command
        .arg(format!(":{number}"))
        .args(["-displayfd", &fd.to_string()])
        .args(["-screen", "0", &format!("{w}x{h}x24")])
        .args(["-nolisten", "tcp", "-noreset", "-auth"])
        .arg(dir.join("xauth"))
        .args(["+extension", "RANDR", "+extension", "GLX"])
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err_log)
        .process_group(0);
    die_with_parent(&mut command);
    // SAFETY: runs in the forked child before exec and only calls fcntl(2),
    // which is async-signal-safe: the pipe's write end (close-on-exec in
    // this process) stays open in the server.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("can't run Xvfb: {e}"))?;
    drop(command);
    drop(writer);
    // The line on another thread: a server that hangs holds the pipe open,
    // and the group kill below ends that read.
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = BufReader::new(reader).read_line(&mut line).map(|_| line);
        let _ = tx.send(read);
    });
    let left = deadline.saturating_duration_since(Instant::now());
    let said = match rx.recv_timeout(left) {
        Ok(Ok(line)) => line,
        Ok(Err(e)) => format!("(-displayfd unreadable: {e})"),
        Err(_) => format!("(nothing on -displayfd within {STARTUP_TIMEOUT:?})"),
    };
    match said.trim().parse::<u32>() {
        Ok(n) if n == number && n != 0 => Ok(child),
        _ => {
            stop(&mut child);
            Err(format!(
                "it said {:?}, then {}; its log ends:\n  {}",
                said.trim(),
                child
                    .try_wait()
                    .ok()
                    .flatten()
                    .map_or("still ran".to_owned(), |s| s.to_string()),
                tail(&log_file)
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_numbers_never_include_0_or_the_inherited_one() {
        assert_eq!(display_number(":0"), Some(0));
        assert_eq!(display_number(":1.0"), Some(1));
        assert_eq!(display_number("localhost:10.0"), Some(10));
        assert_eq!(display_number("unix:2"), Some(2));
        assert_eq!(display_number("[::1]:3"), Some(3));
        assert_eq!(display_number("nowhere"), None);
        assert_eq!(display_number(":x"), None);
        for pid in [0, 1, 835, 899, 900, 4_000_000] {
            let numbers: Vec<u32> = candidates(pid, None).collect();
            assert_eq!(numbers.len() as u32, ATTEMPTS);
            assert!(numbers.iter().all(|&n| n >= FIRST_DISPLAY), "{numbers:?}");
            let first = numbers[0];
            let without: Vec<u32> = candidates(pid, Some(first)).collect();
            assert!(!without.contains(&first), "{without:?}");
            assert_eq!(without.len() as u32, ATTEMPTS - 1);
        }
    }

    #[test]
    fn the_cookie_file_has_one_wildcard_entry() {
        let cookie = *b"0123456789abcdef";
        let file = xauth_file(&cookie);
        let mut want = vec![0xFF, 0xFF, 0, 0, 0, 0, 0, 18];
        want.extend(b"MIT-MAGIC-COOKIE-1");
        want.extend([0, 16]);
        want.extend(cookie);
        assert_eq!(file, want);
    }

    /// A started server takes connections with its cookie, keeps the
    /// monitors set on it, also for a libX11 client given its variables,
    /// and is gone once dropped.
    #[test]
    fn a_started_xvfb_is_private_and_stops_with_its_handle() {
        let xvfb = Xvfb::start(1280, 720);
        assert!(xvfb.number() >= FIRST_DISPLAY);
        assert_ne!(
            std::env::var("DISPLAY").ok().as_deref(),
            Some(xvfb.display().as_str())
        );
        xvfb.set_monitor("HALF", 0, 0, 640, 720);
        let (conn, screen) = xvfb.connect();
        let root = &conn.setup().roots[screen];
        assert_eq!((root.width_in_pixels, root.height_in_pixels), (1280, 720));
        let out = crate::testkit::run_tool(
            Command::new("xrandr")
                .arg("--listactivemonitors")
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .envs(xvfb.vars()),
            Duration::from_secs(30),
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{text}{}", out.stderr);
        assert!(text.contains("HALF 640/169x720/190+0+0"), "{text}");
        let (pid, number) = (xvfb.child.id(), xvfb.number());
        drop(conn);
        drop(xvfb);
        // Reaped, and it cleaned up after itself (SIGTERM, not SIGKILL).
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert!(!taken(number), "its lock or socket is left");
    }
}
