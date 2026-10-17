//! A virtual pointer that stays alive for the whole case (G§8.4).
//!
//! Headless sway has no pointer device, so its seat offers no pointer to
//! clients until one exists, and a pointer that comes and goes with each
//! click (one `wlrctl` run per click) loses the click. This client speaks
//! the Wayland wire protocol over the compositor's socket (std only, no
//! crate): it binds `wl_seat` and `zwlr_virtual_pointer_manager_v1`,
//! creates one `zwlr_virtual_pointer_v1` and keeps the connection open
//! until dropped. Positions are absolute output-layout pixels (the one
//! headless output at (0, 0)); every move or click ends with a `frame` and
//! a `wl_display.sync` round trip, so the compositor has handled it before
//! the call returns.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// `BTN_LEFT`, `BTN_RIGHT`, `BTN_MIDDLE` (linux/input-event-codes.h).
pub const BUTTON_LEFT: u32 = 0x110;
pub const BUTTON_RIGHT: u32 = 0x111;
pub const BUTTON_MIDDLE: u32 = 0x112;

const DISPLAY: u32 = 1;
// New ids must come in order, without gaps (libwayland's object map):
// the registry, the first sync's callback, the seat, the manager, the
// pointer, then one callback per round trip.
const REGISTRY: u32 = 2;
const FIRST_CALLBACK: u32 = 3;
const SEAT: u32 = 4;
const MANAGER: u32 = 5;
const POINTER: u32 = 6;

/// One message: its object, opcode and arguments (already encoded).
struct Message {
    object: u32,
    opcode: u16,
    args: Vec<u8>,
}

impl Message {
    fn new(object: u32, opcode: u16) -> Self {
        Self {
            object,
            opcode,
            args: Vec::new(),
        }
    }

    fn uint(mut self, v: u32) -> Self {
        self.args.extend_from_slice(&v.to_ne_bytes());
        self
    }

    fn string(mut self, s: &str) -> Self {
        let len = u32::try_from(s.len() + 1).expect("a short string");
        self.args.extend_from_slice(&len.to_ne_bytes());
        self.args.extend_from_slice(s.as_bytes());
        self.args.push(0);
        while !self.args.len().is_multiple_of(4) {
            self.args.push(0);
        }
        self
    }

    fn encode(&self) -> Vec<u8> {
        let size = u32::try_from(8 + self.args.len()).expect("a short message");
        let mut out = Vec::with_capacity(size as usize);
        out.extend_from_slice(&self.object.to_ne_bytes());
        out.extend_from_slice(&((size << 16) | u32::from(self.opcode)).to_ne_bytes());
        out.extend_from_slice(&self.args);
        out
    }
}

/// An event as read: its object, opcode and raw arguments.
struct Event {
    object: u32,
    opcode: u16,
    args: Vec<u8>,
}

impl Event {
    fn uint(&self, at: usize) -> u32 {
        u32::from_ne_bytes(self.args[at..at + 4].try_into().expect("4 bytes"))
    }

    /// The string at byte `at` and the offset after it.
    fn string(&self, at: usize) -> (String, usize) {
        let len = self.uint(at) as usize;
        let start = at + 4;
        let text =
            String::from_utf8_lossy(&self.args[start..start + len.saturating_sub(1)]).into_owned();
        (text, start + len.div_ceil(4) * 4)
    }
}

/// The virtual pointer. Dropping it closes the connection (the device
/// goes away).
pub struct VirtualPointer {
    stream: UnixStream,
    next_callback: u32,
    started: Instant,
    position: (f64, f64),
    /// The output's size in logical px (what absolute positions span).
    output: (f64, f64),
    pending: Vec<u8>,
}

impl VirtualPointer {
    /// Connects to the compositor at `socket`, whose one output is
    /// `output` (width, height) logical px, and creates the pointer.
    pub fn connect(socket: &Path, output: (f64, f64)) -> VirtualPointer {
        let stream = UnixStream::connect(socket)
            .unwrap_or_else(|e| panic!("connect to {}: {e}", socket.display()));
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a read timeout");
        let mut pointer = VirtualPointer {
            stream,
            next_callback: FIRST_CALLBACK,
            started: Instant::now(),
            position: (0.0, 0.0),
            output,
            pending: Vec::new(),
        };
        pointer.send(&Message::new(DISPLAY, 1).uint(REGISTRY));
        let globals = pointer.roundtrip();
        let find = |interface: &str| {
            globals
                .iter()
                .find(|(_, name, _)| name == interface)
                .map(|(id, _, version)| (*id, *version))
                .unwrap_or_else(|| panic!("the compositor has no {interface}"))
        };
        let (seat, _) = find("wl_seat");
        let (manager, _) = find("zwlr_virtual_pointer_manager_v1");
        pointer.send(
            &Message::new(REGISTRY, 0)
                .uint(seat)
                .string("wl_seat")
                .uint(1)
                .uint(SEAT),
        );
        pointer.send(
            &Message::new(REGISTRY, 0)
                .uint(manager)
                .string("zwlr_virtual_pointer_manager_v1")
                .uint(1)
                .uint(MANAGER),
        );
        // create_virtual_pointer(seat, id)
        pointer.send(&Message::new(MANAGER, 0).uint(SEAT).uint(POINTER));
        pointer.next_callback = POINTER + 1;
        pointer.roundtrip();
        pointer
    }

    fn time(&self) -> u32 {
        u32::try_from(self.started.elapsed().as_millis() % u128::from(u32::MAX)).unwrap_or(0)
    }

    fn send(&mut self, message: &Message) {
        self.stream
            .write_all(&message.encode())
            .expect("write to the compositor");
    }

    /// `wl_display.sync`, then every event until its `done`; returns the
    /// registry's globals seen meanwhile (name, interface, version).
    fn roundtrip(&mut self) -> Vec<(u32, String, u32)> {
        let callback = self.next_callback;
        self.next_callback += 1;
        self.send(&Message::new(DISPLAY, 0).uint(callback));
        let mut globals = Vec::new();
        loop {
            let event = self.read_event();
            match (event.object, event.opcode) {
                (DISPLAY, 0) => {
                    let (message, _) = event.string(8);
                    panic!(
                        "Wayland error on object {} (code {}): {message}",
                        event.uint(0),
                        event.uint(4)
                    );
                }
                (REGISTRY, 0) => {
                    let name = event.uint(0);
                    let (interface, at) = event.string(4);
                    globals.push((name, interface, event.uint(at)));
                }
                (id, 0) if id == callback => return globals,
                _ => {}
            }
        }
    }

    fn read_event(&mut self) -> Event {
        loop {
            if self.pending.len() >= 8 {
                let object = u32::from_ne_bytes(self.pending[0..4].try_into().expect("4"));
                let word = u32::from_ne_bytes(self.pending[4..8].try_into().expect("4"));
                let size = (word >> 16) as usize;
                if size >= 8 && self.pending.len() >= size {
                    let args = self.pending[8..size].to_vec();
                    self.pending.drain(..size);
                    return Event {
                        object,
                        opcode: (word & 0xffff) as u16,
                        args,
                    };
                }
            }
            let mut buf = [0u8; 4096];
            let n = self
                .stream
                .read(&mut buf)
                .expect("read from the compositor (5 s)");
            assert!(n > 0, "the compositor closed the connection");
            self.pending.extend_from_slice(&buf[..n]);
        }
    }

    /// Moves to (`x`, `y`) in output pixels.
    pub fn move_to(&mut self, x: f64, y: f64) {
        let extent = 100_000u32;
        let (w, h) = self.output;
        let scale = |v: f64, size: f64| ((v / size) * f64::from(extent)).round().max(0.0) as u32;
        let time = self.time();
        self.send(
            &Message::new(POINTER, 1)
                .uint(time)
                .uint(scale(x, w))
                .uint(scale(y, h))
                .uint(extent)
                .uint(extent),
        );
        self.send(&Message::new(POINTER, 4));
        self.roundtrip();
        self.position = (x, y);
    }

    /// Presses or releases `button`.
    pub fn button(&mut self, button: u32, pressed: bool) {
        let time = self.time();
        self.send(
            &Message::new(POINTER, 2)
                .uint(time)
                .uint(button)
                .uint(u32::from(pressed)),
        );
        self.send(&Message::new(POINTER, 4));
        self.roundtrip();
    }

    /// `notches` clicks of a mouse wheel where the pointer is (positive:
    /// down), as a real wheel sends them: a `wheel` axis source and one
    /// discrete step per notch (`axis_source`, `axis_discrete`, `frame`).
    pub fn wheel(&mut self, notches: i32) {
        const AXIS_VERTICAL: u32 = 0;
        const SOURCE_WHEEL: u32 = 0;
        let step = notches.signum();
        for _ in 0..notches.unsigned_abs() {
            let time = self.time();
            // wl_fixed: 24.8 fixed point; 15 is a notch's usual distance.
            let value = (15 * step * 256) as u32;
            self.send(&Message::new(POINTER, 5).uint(SOURCE_WHEEL));
            self.send(
                &Message::new(POINTER, 7)
                    .uint(time)
                    .uint(AXIS_VERTICAL)
                    .uint(value)
                    .uint(step as u32),
            );
            self.send(&Message::new(POINTER, 4));
            self.roundtrip();
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    /// A left click at (`x`, `y`).
    pub fn click(&mut self, x: f64, y: f64) {
        self.move_to(x, y);
        self.button(BUTTON_LEFT, true);
        std::thread::sleep(Duration::from_millis(30));
        self.button(BUTTON_LEFT, false);
    }

    /// A left drag from where the pointer is to (`x`, `y`) in `steps`.
    pub fn drag_to(&mut self, x: f64, y: f64, steps: u32) {
        let (x0, y0) = self.position;
        self.button(BUTTON_LEFT, true);
        let steps = steps.max(1);
        for i in 1..=steps {
            let t = f64::from(i) / f64::from(steps);
            self.move_to(x0 + (x - x0) * t, y0 + (y - y0) * t);
            std::thread::sleep(Duration::from_millis(16));
        }
        self.button(BUTTON_LEFT, false);
    }

    /// Where the pointer is.
    pub fn position(&self) -> (f64, f64) {
        self.position
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_encode_as_the_wire_wants() {
        let bind = Message::new(2, 0).uint(7).string("wl_seat").uint(1).uint(3);
        let bytes = bind.encode();
        // header + name + (len, "wl_seat\0") + version + id
        assert_eq!(bytes.len(), 8 + 4 + 4 + 8 + 4 + 4);
        assert_eq!(u32::from_ne_bytes(bytes[0..4].try_into().unwrap()), 2);
        let word = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!((word >> 16) as usize, bytes.len());
        assert_eq!(word & 0xffff, 0);
        assert_eq!(&bytes[16..24], b"wl_seat\0");
    }
}
