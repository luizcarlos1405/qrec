//! X11 screens (issue #9): the X server's RandR monitors, through x11rb's
//! pure-Rust connection (no libxcb, no GStreamer). Read on a helper thread
//! the caller waits for at most its timeout, like `wayland::outputs`. The
//! thread's TCP connect (a `host:N` display) and its every wait on the
//! connection end at the same deadline, so it doesn't outlive a server that
//! stopped answering; only a host name's lookup (the resolver has its own
//! timeouts) or a full listen backlog on the server's socket can keep it
//! longer.
//!
//! - RandR 1.5 `GetMonitors` (the active ones): each monitor's name atom,
//!   position and size; the description is its first output's EDID
//!   monitor name, else the EDID's manufacturer ID, else nothing (the
//!   name alone, as `output_description` shapes it).
//! - Before RandR 1.5, or when that lists nothing: the CRTCs that drive an
//!   output (`GetScreenResourcesCurrent` + `GetCrtcInfo`, RandR 1.3), named
//!   after their first output.
//! - Without RandR, or with nothing lit: the root window, as `screen`.
//!
//! Every screen is clipped to the root window (nothing outside it can be
//! read), a name already taken is left out, and the list is sorted like
//! every backend's; the scale is 1 (X11 has no per-monitor scale). It fills
//! the same `ScreenInfo` as Wayland, so the core and the project schema
//! don't know which backend found a screen.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use qrec_core::ctx::ScreenInfo;
use x11rb::connection::Connection;
use x11rb::errors::{ConnectionError, ReplyError};
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::reexports::x11rb_protocol::parse_display::{ConnectAddress, parse_display};
use x11rb::reexports::x11rb_protocol::xauth::{Family, get_auth};
use x11rb::rust_connection::{DefaultStream, PollMode, RustConnection, Stream};
use x11rb::utils::RawFdContainer;

/// The name of the root-window fallback.
const ROOT_NAME: &str = "screen";

/// An EDID's base block, in the 4-byte units `GetOutputProperty` counts.
const EDID_LONGS: u32 = 128 / 4;

/// The screens of the X server at `display`, within `timeout`. `Err` when
/// it can't be reached or doesn't answer in time.
pub(super) fn monitors(display: &str, timeout: Duration) -> Result<Vec<ScreenInfo>, String> {
    let deadline = Instant::now() + timeout;
    let (tx, rx) = crossbeam_channel::bounded(1);
    let name = display.to_owned();
    std::thread::Builder::new()
        .name("qrec-x11-screens".into())
        .spawn(move || {
            let screens = connect(&name, deadline).and_then(|(conn, screen)| {
                read_screens(&conn, screen).map_err(|e| format!("can't read its monitors: {e}"))
            });
            let _ = tx.send(screens);
        })
        .map_err(|e| format!("can't start the X11 discovery thread: {e}"))?;
    rx.recv_timeout(timeout)
        .map_err(|_| format!("the X server didn't answer within {} s", timeout.as_secs()))?
}

/// Connects to `display` with the cookie `XAUTHORITY` (else
/// `~/.Xauthority`) holds for it, if any: what `RustConnection::connect`
/// does, over a [`DeadlineStream`].
fn connect(
    display: &str,
    deadline: Instant,
) -> Result<(RustConnection<DeadlineStream>, usize), String> {
    let parsed = parse_display(Some(display)).map_err(|e| format!("can't use it: {e}"))?;
    let screen = usize::from(parsed.screen);
    let mut first_error = None;
    for address in parsed.connect_instruction() {
        match open(&address, deadline) {
            Ok((stream, (family, peer))) => {
                let (auth_name, auth_data) = get_auth(family, &peer, parsed.display)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                let stream = DeadlineStream::new(stream, deadline);
                let conn = RustConnection::connect_to_stream_with_auth_info(
                    stream, screen, auth_name, auth_data,
                )
                .map_err(|e| format!("can't connect: {e}"))?;
                return Ok((conn, screen));
            }
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    Err(match first_error {
        Some(e) => format!("can't connect: {e}"),
        None => "can't connect: no address to try".to_owned(),
    })
}

/// `DefaultStream::connect`, whose TCP connect (a `host:N` display) gives
/// up at `deadline`. Looking the host up has the resolver's timeouts; a
/// Unix socket's connect only waits when the server's backlog is full.
fn open(
    address: &ConnectAddress<'_>,
    deadline: Instant,
) -> io::Result<(DefaultStream, (Family, Vec<u8>))> {
    let ConnectAddress::Hostname(host, port) = address else {
        return DefaultStream::connect(address);
    };
    let mut first_error = None;
    for peer in (*host, *port).to_socket_addrs()? {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out());
        }
        match TcpStream::connect_timeout(&peer, left) {
            Ok(stream) => return DefaultStream::from_tcp_stream(stream),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, format!("{host} has no address"))
    }))
}

fn timed_out() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "the X server didn't answer in time",
    )
}

/// x11rb's stream, whose waits end at `deadline` (then every request
/// fails with `TimedOut`). The testkit's connections to its Xvfb use it
/// too (`testkit::x11`).
pub struct DeadlineStream {
    inner: DefaultStream,
    deadline: Instant,
}

impl DeadlineStream {
    pub fn new(inner: DefaultStream, deadline: Instant) -> Self {
        Self { inner, deadline }
    }
}

impl Stream for DeadlineStream {
    fn poll(&self, mode: PollMode) -> io::Result<()> {
        let mut events = 0;
        if mode.readable() {
            events |= libc::POLLIN;
        }
        if mode.writable() {
            events |= libc::POLLOUT;
        }
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(timed_out());
            }
            let mut pfd = libc::pollfd {
                fd: self.inner.as_raw_fd(),
                events,
                revents: 0,
            };
            let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: one valid pollfd for the duration of the call.
            let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
            if rc > 0 {
                // Readable, writable or an error: the read or write tells.
                return Ok(());
            }
            if rc < 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    return Err(e);
                }
            }
        }
    }

    fn read(&self, buf: &mut [u8], fd_storage: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.inner.read(buf, fd_storage)
    }

    fn write(&self, buf: &[u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.inner.write(buf, fds)
    }

    fn write_vectored(
        &self,
        bufs: &[io::IoSlice<'_>],
        fds: &mut Vec<RawFdContainer>,
    ) -> io::Result<usize> {
        self.inner.write_vectored(bufs, fds)
    }
}

// ------------------------------------------------------------- requests

/// The screens of `conn`'s `screen`: its monitors, else its lit CRTCs,
/// else its root window. An X11 error moves on to the next of those; a
/// connection error (a timeout among them) ends the discovery.
pub(super) fn read_screens(
    conn: &impl Connection,
    screen: usize,
) -> Result<Vec<ScreenInfo>, String> {
    let root = conn
        .setup()
        .roots
        .get(screen)
        .ok_or_else(|| format!("the X server has no screen {screen}"))?;
    let size = Root {
        width: root.width_in_pixels,
        height: root.height_in_pixels,
    };
    let window = root.root;
    let version = fallback(randr_version(conn), "QueryVersion")?.flatten();
    if version >= Some((1, 5)) {
        let screens = fallback(monitor_replies(conn, window), "GetMonitors")?
            .map(|monitors| from_monitors(&monitors, size));
        if let Some(screens) = screens.filter(|s| !s.is_empty()) {
            return Ok(screens);
        }
    }
    if version >= Some((1, 3)) {
        let screens = fallback(crtc_replies(conn, window), "GetCrtcInfo")?
            .map(|crtcs| from_crtcs(&crtcs, size));
        if let Some(screens) = screens.filter(|s| !s.is_empty()) {
            return Ok(screens);
        }
    }
    tracing::debug!("X11: no RandR monitor or lit CRTC; the root window is the screen");
    Ok(root_screen(size))
}

/// An X11 error is `None` (logged: the next way is tried), a connection
/// error the discovery's error.
fn fallback<T>(result: Result<T, ReplyError>, what: &str) -> Result<Option<T>, String> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ReplyError::X11Error(e)) => {
            tracing::debug!("X11: {what} failed ({e:?}); trying the next way");
            Ok(None)
        }
        Err(ReplyError::ConnectionError(e)) => Err(e.to_string()),
    }
}

/// The server's RandR version (at most the 1.5 asked for); `None` without
/// the extension.
fn randr_version(conn: &impl Connection) -> Result<Option<(u32, u32)>, ReplyError> {
    if conn
        .extension_information(randr::X11_EXTENSION_NAME)?
        .is_none()
    {
        return Ok(None);
    }
    let reply = conn.randr_query_version(1, 5)?.reply()?;
    Ok(Some((reply.major_version, reply.minor_version)))
}

/// The `EDID` atom, when some output has one.
fn edid_atom(conn: &impl Connection) -> Result<Option<xproto::Atom>, ReplyError> {
    let atom = conn.intern_atom(true, b"EDID")?.reply()?.atom;
    Ok((atom != u32::from(xproto::AtomEnum::NONE)).then_some(atom))
}

/// What an X11 reply holds, or `None` for an X11 error (a missing atom
/// or output); a connection error stays one.
fn optional<T>(result: Result<T, ReplyError>) -> Result<Option<T>, ConnectionError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ReplyError::X11Error(_)) => Ok(None),
        Err(ReplyError::ConnectionError(e)) => Err(e),
    }
}

/// The base EDID block of `output`, if it has one. Asked for before any
/// reply is read (one round trip for all of them).
fn edid_cookie<C: Connection>(
    conn: &C,
    atom: Option<xproto::Atom>,
    output: Option<randr::Output>,
) -> Result<Option<x11rb::cookie::Cookie<'_, C, randr::GetOutputPropertyReply>>, ConnectionError> {
    let (Some(atom), Some(output)) = (atom, output) else {
        return Ok(None);
    };
    conn.randr_get_output_property(
        output,
        atom,
        xproto::AtomEnum::ANY,
        0,
        EDID_LONGS,
        false,
        false,
    )
    .map(Some)
}

fn edid_bytes(reply: Option<randr::GetOutputPropertyReply>) -> Option<Vec<u8>> {
    reply
        .filter(|r| r.format == 8 && !r.data.is_empty())
        .map(|r| r.data)
}

/// `GetMonitors` (the active ones), with each name atom's text and the
/// first output's EDID.
fn monitor_replies(
    conn: &impl Connection,
    window: xproto::Window,
) -> Result<Vec<MonitorReply>, ReplyError> {
    let reply = conn.randr_get_monitors(window, true)?.reply()?;
    let atom = edid_atom(conn)?;
    let mut asked = Vec::new();
    for monitor in &reply.monitors {
        let name = conn.get_atom_name(monitor.name)?;
        let edid = edid_cookie(conn, atom, monitor.outputs.first().copied())?;
        asked.push((monitor, name, edid));
    }
    let mut monitors = Vec::new();
    for (monitor, name, edid) in asked {
        let name = optional(name.reply())?.map(|r| String::from_utf8_lossy(&r.name).into_owned());
        let edid = match edid {
            Some(cookie) => edid_bytes(optional(cookie.reply())?),
            None => None,
        };
        monitors.push(MonitorReply {
            name,
            x: monitor.x,
            y: monitor.y,
            width: monitor.width,
            height: monitor.height,
            edid,
        });
    }
    Ok(monitors)
}

/// `GetScreenResourcesCurrent` and each CRTC's `GetCrtcInfo`, with the
/// name and EDID of its first output.
fn crtc_replies(
    conn: &impl Connection,
    window: xproto::Window,
) -> Result<Vec<CrtcReply>, ReplyError> {
    let resources = conn.randr_get_screen_resources_current(window)?.reply()?;
    let stamp = resources.config_timestamp;
    let asked = resources
        .crtcs
        .iter()
        .map(|&crtc| conn.randr_get_crtc_info(crtc, stamp))
        .collect::<Result<Vec<_>, _>>()?;
    let mut infos = Vec::new();
    for cookie in asked {
        infos.extend(optional(cookie.reply())?);
    }
    let atom = edid_atom(conn)?;
    let mut asked = Vec::new();
    for info in &infos {
        let first = info.outputs.first().copied();
        let output = match first {
            Some(output) => Some(conn.randr_get_output_info(output, stamp)?),
            None => None,
        };
        asked.push((info, output, edid_cookie(conn, atom, first)?));
    }
    let mut crtcs = Vec::new();
    for (info, output, edid) in asked {
        let name = match output {
            Some(cookie) => optional(cookie.reply())?,
            None => None,
        };
        let edid = match edid {
            Some(cookie) => edid_bytes(optional(cookie.reply())?),
            None => None,
        };
        crtcs.push(CrtcReply {
            x: info.x,
            y: info.y,
            width: info.width,
            height: info.height,
            mode: info.mode,
            output: name.map(|o| OutputReply {
                name: String::from_utf8_lossy(&o.name).into_owned(),
                edid,
            }),
        });
    }
    Ok(crtcs)
}

// -------------------------------------------------------------- mapping

/// The root window's size: every screen lies inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Root {
    width: u16,
    height: u16,
}

/// A monitor as `GetMonitors` gave it, with its name atom's text (`None`
/// when the atom has none) and its first output's EDID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MonitorReply {
    name: Option<String>,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    edid: Option<Vec<u8>>,
}

/// A CRTC as `GetCrtcInfo` gave it, with its first output (`None` when it
/// drives none).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CrtcReply {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    /// 0: off.
    mode: u32,
    output: Option<OutputReply>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct OutputReply {
    name: String,
    edid: Option<Vec<u8>>,
}

/// One screen per monitor; a monitor whose name atom has no text is
/// `MONITOR-<n>` (its place in the reply, from 1).
fn from_monitors(monitors: &[MonitorReply], root: Root) -> Vec<ScreenInfo> {
    let screens = monitors.iter().enumerate().map(|(i, m)| {
        let name = m
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map_or_else(|| format!("MONITOR-{}", i + 1), str::to_owned);
        screen(name, m.edid.as_deref(), (m.x, m.y, m.width, m.height))
    });
    tidy(screens, root)
}

/// One screen per CRTC that is on and drives an output, named after it.
fn from_crtcs(crtcs: &[CrtcReply], root: Root) -> Vec<ScreenInfo> {
    let screens = crtcs.iter().filter(|c| c.mode != 0).filter_map(|c| {
        let output = c.output.as_ref()?;
        let name = output.name.trim();
        (!name.is_empty()).then(|| {
            screen(
                name.to_owned(),
                output.edid.as_deref(),
                (c.x, c.y, c.width, c.height),
            )
        })
    });
    tidy(screens, root)
}

/// The whole root window, as [`ROOT_NAME`].
fn root_screen(root: Root) -> Vec<ScreenInfo> {
    let whole = (0, 0, root.width, root.height);
    tidy(
        std::iter::once(screen(ROOT_NAME.to_owned(), None, whole)),
        root,
    )
}

fn screen(name: String, edid: Option<&[u8]>, (x, y, w, h): (i16, i16, u16, u16)) -> ScreenInfo {
    let label = edid.and_then(edid_label).unwrap_or_default();
    ScreenInfo {
        description: super::output_description("", &label, &name),
        output: name,
        x: i32::from(x),
        y: i32::from(y),
        width: u32::from(w),
        height: u32::from(h),
        scale: 1.0,
    }
}

/// Clipped to the root (empty ones left out), the first of each name,
/// sorted.
fn tidy(screens: impl Iterator<Item = ScreenInfo>, root: Root) -> Vec<ScreenInfo> {
    let mut kept: Vec<ScreenInfo> = Vec::new();
    for screen in screens.filter_map(|s| clip(s, root)) {
        if kept.iter().any(|k| k.output == screen.output) {
            tracing::debug!("X11: a second screen named {:?} left out", screen.output);
            continue;
        }
        kept.push(screen);
    }
    super::sort_screens(&mut kept);
    kept
}

/// The part of `screen` inside the root window; `None` when nothing is.
fn clip(mut screen: ScreenInfo, root: Root) -> Option<ScreenInfo> {
    let (x0, y0) = (i64::from(screen.x).max(0), i64::from(screen.y).max(0));
    let x1 = (i64::from(screen.x) + i64::from(screen.width)).min(i64::from(root.width));
    let y1 = (i64::from(screen.y) + i64::from(screen.height)).min(i64::from(root.height));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    // Inside a u16 root, so every value fits.
    screen.x = x0 as i32;
    screen.y = y0 as i32;
    screen.width = (x1 - x0) as u32;
    screen.height = (y1 - y0) as u32;
    Some(screen)
}

// ----------------------------------------------------------------- EDID

/// What names a display in its EDID base block: the monitor-name
/// descriptor (`0xFC`), else the manufacturer's three-letter PNP ID
/// (`DEL`, `GSM`). `None` for anything that isn't an EDID.
fn edid_label(edid: &[u8]) -> Option<String> {
    const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    let block = edid.get(..128)?;
    if block[..8] != HEADER {
        return None;
    }
    let name = block[54..126]
        .chunks_exact(18)
        .find(|d| d[..3] == [0, 0, 0] && d[3] == 0xFC)
        .and_then(|d| descriptor_text(&d[5..]));
    name.or_else(|| pnp_id(u16::from_be_bytes([block[8], block[9]])))
}

/// A descriptor's text: up to the first newline, printable ASCII only,
/// trimmed; `None` when that leaves nothing.
fn descriptor_text(bytes: &[u8]) -> Option<String> {
    let text: String = bytes
        .iter()
        .take_while(|&&b| b != b'\n')
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .map(|&b| char::from(b))
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Three 5-bit letters (1 is `A`), big-endian; `None` if one isn't a letter.
fn pnp_id(id: u16) -> Option<String> {
    [10, 5, 0]
        .iter()
        .map(|shift| match (id >> shift) & 0x1F {
            letter @ 1..=26 => Some(char::from(b'A' + letter as u8 - 1)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// An EDID base block from `make` (a PNP ID) and descriptors (tag,
    /// text) in the four descriptor slots.
    fn edid(make: &str, descriptors: &[(u8, &[u8])]) -> Vec<u8> {
        let mut block = vec![0u8; 128];
        block[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        let id = make
            .bytes()
            .fold(0u16, |id, b| (id << 5) | u16::from(b - b'A' + 1));
        block[8..10].copy_from_slice(&id.to_be_bytes());
        // Slot 0: a detailed timing (a pixel clock), as real EDIDs have.
        block[54] = 0x02;
        block[55] = 0x3A;
        for (slot, (tag, text)) in descriptors.iter().enumerate() {
            let at = 72 + 18 * slot;
            block[at + 3] = *tag;
            let mut field = [b' '; 13];
            field[..text.len()].copy_from_slice(text);
            if text.len() < 13 {
                field[text.len()] = b'\n';
            }
            block[at + 5..at + 18].copy_from_slice(&field);
        }
        block
    }

    #[test]
    fn edid_names_the_monitor_else_its_maker() {
        // A Dell U2720Q: a serial descriptor, then the monitor name.
        let dell = edid("DEL", &[(0xFF, b"8XYZ123"), (0xFC, b"DELL U2720Q")]);
        assert_eq!(edid_label(&dell).as_deref(), Some("DELL U2720Q"));
        // Exactly 13 characters: no newline.
        let lg = edid("GSM", &[(0xFC, b"LG ULTRAWIDE ")]);
        assert_eq!(edid_label(&lg).as_deref(), Some("LG ULTRAWIDE"));
        // No name descriptor, or an empty one: the PNP ID.
        assert_eq!(
            edid_label(&edid("GSM", &[(0xFD, b"")])).as_deref(),
            Some("GSM")
        );
        assert_eq!(
            edid_label(&edid("BOE", &[(0xFC, b"  ")])).as_deref(),
            Some("BOE")
        );
        // Control bytes are dropped; a 256-byte EDID (with an extension
        // block) reads its base block.
        let mut odd = edid("AUO", &[(0xFC, b"B\x01140HAN\x7f")]);
        assert_eq!(edid_label(&odd).as_deref(), Some("B140HAN"));
        odd.extend([0u8; 128]);
        assert_eq!(edid_label(&odd).as_deref(), Some("B140HAN"));
        // A name tag in the timing slot isn't a descriptor (no zero clock).
        let mut timing = edid("DEL", &[]);
        timing[54..58].copy_from_slice(&[0x02, 0x3A, 0x00, 0xFC]);
        assert_eq!(edid_label(&timing).as_deref(), Some("DEL"));
        // Not an EDID: too short, a wrong header, an invalid PNP ID.
        assert_eq!(edid_label(&dell[..127]), None);
        let mut bad = dell.clone();
        bad[0] = 1;
        assert_eq!(edid_label(&bad), None);
        let mut no_make = edid("DEL", &[]);
        no_make[8..10].copy_from_slice(&[0, 0]);
        assert_eq!(edid_label(&no_make), None);
        assert_eq!(pnp_id(0x10AC).as_deref(), Some("DEL"));
    }

    const ROOT: Root = Root {
        width: 1920,
        height: 1080,
    };

    fn summary(screens: &[ScreenInfo]) -> Vec<(String, String, i32, i32, u32, u32)> {
        screens
            .iter()
            .inspect(|s| assert_eq!(s.scale, 1.0))
            .map(|s| {
                (
                    s.output.clone(),
                    s.description.clone(),
                    s.x,
                    s.y,
                    s.width,
                    s.height,
                )
            })
            .collect()
    }

    fn row(
        name: &str,
        description: &str,
        rect: (i32, i32, u32, u32),
    ) -> (String, String, i32, i32, u32, u32) {
        (
            name.into(),
            description.into(),
            rect.0,
            rect.1,
            rect.2,
            rect.3,
        )
    }

    #[test]
    fn monitors_become_screens() {
        let monitor = |name: &str, x, y, width, height| MonitorReply {
            name: Some(name.into()),
            x,
            y,
            width,
            height,
            edid: None,
        };
        let monitors = [
            // xrandr --setmonitor VIRT-R 960/254x1080/286+960+0 none
            monitor("VIRT-R", 960, 0, 960, 1080),
            MonitorReply {
                edid: Some(edid("DEL", &[(0xFC, b"DELL U2720Q")])),
                ..monitor("DP-1", 0, 0, 960, 1080)
            },
            // The automatic one of the output they split.
            monitor("screen", 0, 0, 1920, 1080),
            // Its name atom has no text.
            monitor("", 0, 540, 960, 540),
            // Only partly on the root, then not at all, then empty.
            monitor("HALF", 1440, 540, 960, 1080),
            monitor("OFF", 1920, 0, 800, 600),
            monitor("ZERO", 10, 10, 0, 600),
            // A name seen already.
            monitor("DP-1", 960, 540, 100, 100),
        ];
        assert_eq!(
            summary(&from_monitors(&monitors, ROOT)),
            [
                row("DP-1", "DELL U2720Q - DP-1", (0, 0, 960, 1080)),
                row("screen", "screen", (0, 0, 1920, 1080)),
                row("MONITOR-4", "MONITOR-4", (0, 540, 960, 540)),
                row("VIRT-R", "VIRT-R", (960, 0, 960, 1080)),
                row("HALF", "HALF", (1440, 540, 480, 540)),
            ]
        );
        assert!(from_monitors(&[], ROOT).is_empty());
    }

    #[test]
    fn lit_crtcs_become_screens_named_after_their_output() {
        let output = |name: &str| {
            Some(OutputReply {
                name: name.into(),
                edid: None,
            })
        };
        let crtcs = [
            CrtcReply {
                x: 1280,
                y: 0,
                width: 640,
                height: 480,
                mode: 0x41,
                output: Some(OutputReply {
                    name: "HDMI-1".into(),
                    edid: Some(edid("GSM", &[])),
                }),
            },
            CrtcReply {
                width: 1280,
                height: 1024,
                mode: 0x42,
                output: output("eDP-1"),
                ..CrtcReply::default()
            },
            // Off (disabled outputs leave their CRTC without a mode), or
            // on without an output.
            CrtcReply {
                width: 1920,
                height: 1080,
                mode: 0,
                output: output("DP-2"),
                ..CrtcReply::default()
            },
            CrtcReply {
                width: 1920,
                height: 1080,
                mode: 0x43,
                output: None,
                ..CrtcReply::default()
            },
        ];
        assert_eq!(
            summary(&from_crtcs(&crtcs, ROOT)),
            [
                row("eDP-1", "eDP-1", (0, 0, 1280, 1024)),
                row("HDMI-1", "GSM - HDMI-1", (1280, 0, 640, 480)),
            ]
        );
        assert!(from_crtcs(&crtcs[2..], ROOT).is_empty());
    }

    #[test]
    fn the_root_window_is_the_last_resort() {
        assert_eq!(
            summary(&root_screen(ROOT)),
            [row("screen", "screen", (0, 0, 1920, 1080))]
        );
        let none = Root {
            width: 0,
            height: 768,
        };
        assert!(root_screen(none).is_empty());
    }

    fn any_monitor() -> impl Strategy<Value = MonitorReply> {
        (
            prop::option::of("[A-Z]{1,2}(-[0-9])?"),
            -3000i16..5000,
            -3000i16..5000,
            0u16..5000,
            0u16..5000,
        )
            .prop_map(|(name, x, y, width, height)| MonitorReply {
                name,
                x,
                y,
                width,
                height,
                edid: None,
            })
    }

    proptest! {
        /// Whatever the server says, the screens have unique names, come
        /// sorted, lie inside the root and aren't empty.
        #[test]
        fn screens_are_unique_sorted_and_inside_the_root(
            monitors in prop::collection::vec(any_monitor(), 0..8),
            width in 1u16..8000,
            height in 1u16..8000,
        ) {
            let root = Root { width, height };
            let crtcs: Vec<CrtcReply> = monitors
                .iter()
                .map(|m| CrtcReply {
                    x: m.x,
                    y: m.y,
                    width: m.width,
                    height: m.height,
                    mode: 1,
                    output: m.name.clone().map(|name| OutputReply { name, edid: None }),
                })
                .collect();
            let all = [
                from_monitors(&monitors, root),
                from_crtcs(&crtcs, root),
                root_screen(root),
            ];
            for screens in all {
                let mut names: Vec<&str> = screens.iter().map(|s| s.output.as_str()).collect();
                names.sort_unstable();
                names.dedup();
                prop_assert_eq!(names.len(), screens.len());
                let keys: Vec<_> = screens.iter().map(|s| (s.x, s.y, s.output.clone())).collect();
                let mut sorted = keys.clone();
                sorted.sort();
                prop_assert_eq!(keys, sorted);
                for s in &screens {
                    prop_assert!(s.x >= 0 && s.y >= 0 && s.width > 0 && s.height > 0);
                    prop_assert!(s.x as u32 + s.width <= u32::from(width));
                    prop_assert!(s.y as u32 + s.height <= u32::from(height));
                }
            }
        }
    }

    /// The requests against a private Xvfb (the testkit's, never this
    /// session's display): its two RandR monitors and the output they
    /// split, then its CRTC, the way an X server before RandR 1.5 would be
    /// read.
    #[test]
    fn a_private_xvfb_lists_its_randr_monitors() {
        let xvfb = crate::testkit::x11::Xvfb::start(1920, 1080);
        xvfb.set_monitor("VIRT-L", 0, 0, 960, 1080);
        xvfb.set_monitor("VIRT-R", 960, 0, 960, 1080);
        let (conn, screen) = xvfb.connect();
        let screens = read_screens(&conn, screen).unwrap();
        assert_eq!(
            summary(&screens),
            [
                row("VIRT-L", "VIRT-L", (0, 0, 960, 1080)),
                row("screen", "screen", (0, 0, 1920, 1080)),
                row("VIRT-R", "VIRT-R", (960, 0, 960, 1080)),
            ]
        );
        let root = conn.setup().roots[screen].root;
        let crtcs = crtc_replies(&conn, root).unwrap();
        assert_eq!(
            summary(&from_crtcs(&crtcs, ROOT)),
            [row("screen", "screen", (0, 0, 1920, 1080))]
        );
        assert_eq!(randr_version(&conn).unwrap(), Some((1, 5)));
    }

    /// Nothing listens there: an error well within the timeout. (A
    /// display number no test server takes; its socket doesn't exist.)
    #[test]
    fn an_unreachable_display_is_an_error() {
        let started = Instant::now();
        let err = monitors(":59000", Duration::from_secs(2)).unwrap_err();
        assert!(err.starts_with("can't connect: "), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
        let err = monitors("not a display", Duration::from_secs(2)).unwrap_err();
        assert!(err.starts_with("can't use it: "), "{err}");
    }

    /// A server that accepts the connection and never answers (a stopped
    /// one): the connection's setup gives up at its stream's deadline.
    #[test]
    fn a_silent_server_times_out() {
        let dir = tempfile::Builder::new()
            .prefix("qx-")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().join("X");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let address = x11rb::reexports::x11rb_protocol::parse_display::ConnectAddress::Socket(
            path.to_string_lossy().into_owned(),
        );
        let (stream, _) = DefaultStream::connect(&address).unwrap();
        let deadline = Instant::now() + Duration::from_millis(300);
        let started = Instant::now();
        let err = RustConnection::connect_to_stream(DeadlineStream::new(stream, deadline), 0)
            .err()
            .expect("no setup reply")
            .to_string();
        assert_eq!(err, "the X server didn't answer in time");
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(250) && took < Duration::from_secs(2),
            "{took:?}"
        );
    }

    /// The same over TCP (a `host:N` display), through `monitors`: the
    /// caller gets its error at the timeout, and the thread is done by then
    /// too (it sent its setup request and dropped the connection).
    #[test]
    fn a_silent_tcp_server_times_out_and_its_thread_ends() {
        use std::io::Read as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port > 6000, "an ephemeral port: {port}");
        let display = format!("127.0.0.1:{}", port - 6000);
        let started = Instant::now();
        let err = monitors(&display, Duration::from_millis(300)).unwrap_err();
        let took = started.elapsed();
        assert!(err.contains("didn't answer"), "{err}");
        assert!(
            took >= Duration::from_millis(250) && took < Duration::from_secs(2),
            "{took:?}"
        );
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut sent = Vec::new();
        peer.read_to_end(&mut sent)
            .expect("the discovery thread closes its connection");
        assert!(matches!(sent.first(), Some(b'l' | b'B')), "{sent:?}");
    }
}
