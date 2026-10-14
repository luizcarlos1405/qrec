//! JSON-lines framing for the project socket (A§4.9, A§8.3 step 7): line
//! reads with a size cap and a deadline, and whole-line writes with one.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde::Serialize;

/// Why [`LineReader::read_line`] returned no line.
#[derive(Debug)]
pub enum ReadError {
    /// The peer closed the connection at a line boundary.
    Eof,
    /// The peer closed the connection in the middle of a line.
    Truncated,
    /// The line is longer than the limit.
    TooLong,
    /// The deadline passed before a whole line arrived.
    Timeout,
    Io(io::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Eof => f.write_str("connection closed"),
            ReadError::Truncated => f.write_str("connection closed in the middle of a line"),
            ReadError::TooLong => f.write_str("line too long"),
            ReadError::Timeout => f.write_str("timed out"),
            ReadError::Io(e) => write!(f, "{e}"),
        }
    }
}

const CHUNK: usize = 16 * 1024;
/// Socket timeouts must be non-zero.
const MIN_TIMEOUT: Duration = Duration::from_millis(1);

/// Reads `\n`-terminated lines from a stream; bytes after a line stay
/// buffered for the next call, so a reader survives timeouts.
#[derive(Debug)]
pub struct LineReader {
    stream: UnixStream,
    buf: Vec<u8>,
    /// Bytes of `buf` already known to hold no newline.
    scanned: usize,
}

impl LineReader {
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            scanned: 0,
        }
    }

    pub fn get_ref(&self) -> &UnixStream {
        &self.stream
    }

    /// The next line without its `\n` (or `\r\n`), read before `deadline`.
    /// A line longer than `max` bytes is [`ReadError::TooLong`]; the reader
    /// is then unusable.
    pub fn read_line(&mut self, max: usize, deadline: Instant) -> Result<Vec<u8>, ReadError> {
        let mut chunk = [0u8; CHUNK];
        loop {
            if let Some(pos) = self.buf[self.scanned..].iter().position(|&b| b == b'\n') {
                let end = self.scanned + pos;
                let mut line: Vec<u8> = self.buf.drain(..=end).collect();
                self.scanned = 0;
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.len() > max {
                    return Err(ReadError::TooLong);
                }
                return Ok(line);
            }
            self.scanned = self.buf.len();
            if self.buf.len() > max {
                return Err(ReadError::TooLong);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(ReadError::Timeout);
            }
            self.stream
                .set_read_timeout(Some((deadline - now).max(MIN_TIMEOUT)))
                .map_err(ReadError::Io)?;
            match self.stream.read(&mut chunk) {
                Ok(0) if self.buf.is_empty() => return Err(ReadError::Eof),
                Ok(0) => return Err(ReadError::Truncated),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                // A reset peer is gone, like EOF.
                Err(e) if is_disconnect(&e) && self.buf.is_empty() => return Err(ReadError::Eof),
                Err(e) if is_disconnect(&e) => return Err(ReadError::Truncated),
                Err(e) => return Err(ReadError::Io(e)),
            }
        }
    }
}

/// Whether an I/O error means the peer is gone.
pub fn is_disconnect(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    )
}

/// `value` as one JSON line (`\n`-terminated).
pub fn encode_line(value: &impl Serialize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).expect("wire values always serialize");
    bytes.push(b'\n');
    bytes
}

/// Writes all of `bytes` before `deadline`; a peer that doesn't read for
/// that long gets [`io::ErrorKind::TimedOut`].
pub fn write_all_before(stream: &UnixStream, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    let mut rest = bytes;
    while !rest.is_empty() {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the peer stopped reading",
            ));
        }
        stream.set_write_timeout(Some((deadline - now).max(MIN_TIMEOUT)))?;
        match (&*stream).write(rest) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => rest = &rest[n..],
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soon(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn reads_lines_and_keeps_the_rest_buffered() {
        let (a, b) = UnixStream::pair().unwrap();
        (&a).write_all(b"{\"x\":1}\n{\"y\"").unwrap();
        let mut reader = LineReader::new(b);
        assert_eq!(reader.read_line(100, soon(1000)).unwrap(), b"{\"x\":1}");
        // The second line is incomplete: a timeout keeps what arrived.
        assert!(matches!(
            reader.read_line(100, soon(50)),
            Err(ReadError::Timeout)
        ));
        (&a).write_all(b":2}\r\nlast\n").unwrap();
        assert_eq!(reader.read_line(100, soon(1000)).unwrap(), b"{\"y\":2}");
        assert_eq!(reader.read_line(100, soon(1000)).unwrap(), b"last");
        drop(a);
        assert!(matches!(
            reader.read_line(100, soon(1000)),
            Err(ReadError::Eof)
        ));
    }

    #[test]
    fn caps_line_length() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut reader = LineReader::new(b);
        // Exactly the limit is fine.
        (&a).write_all(&[b'x'; 10]).unwrap();
        (&a).write_all(b"\n").unwrap();
        assert_eq!(reader.read_line(10, soon(1000)).unwrap().len(), 10);
        // One byte more, without a newline yet, is already too long.
        (&a).write_all(&[b'x'; 11]).unwrap();
        assert!(matches!(
            reader.read_line(10, soon(1000)),
            Err(ReadError::TooLong)
        ));
    }

    #[test]
    fn a_line_cut_by_eof_is_truncated() {
        let (a, b) = UnixStream::pair().unwrap();
        (&a).write_all(b"{\"half\"").unwrap();
        drop(a);
        let mut reader = LineReader::new(b);
        assert!(matches!(
            reader.read_line(100, soon(1000)),
            Err(ReadError::Truncated)
        ));
    }

    #[test]
    fn writes_time_out_when_the_peer_stops_reading() {
        let (a, _b) = UnixStream::pair().unwrap();
        let big = vec![b'x'; 8 << 20];
        let started = Instant::now();
        let err = write_all_before(&a, &big, soon(200)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn encodes_one_line() {
        assert_eq!(encode_line(&serde_json::json!({"a": 1})), b"{\"a\":1}\n");
    }
}
