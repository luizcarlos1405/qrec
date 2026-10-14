//! Runs a tool (ffmpeg, ffprobe) to completion with a timeout. Its output
//! goes to anonymous temp files, never to pipes, so nothing can stall on a
//! full pipe buffer; the child is killed when the timeout expires.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// What a finished tool wrote.
#[derive(Debug)]
pub struct ToolOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// Runs `command` with stdin closed. Panics if it can't start or outlives
/// `timeout`; a failed exit status is returned, not a panic.
pub fn run_tool(command: &mut Command, timeout: Duration) -> ToolOutput {
    let mut stdout = tempfile::tempfile().expect("create a temp file for stdout");
    let mut stderr = tempfile::tempfile().expect("create a temp file for stderr");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone().expect("dup the stdout file"))
        .stderr(stderr.try_clone().expect("dup the stderr file"))
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "can't run {:?}: {e} (tests need the Nix dev shell's ffmpeg on PATH)",
                command.get_program()
            )
        });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().expect("poll the child") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{command:?} didn't finish within {timeout:?}");
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    };
    ToolOutput {
        status,
        stdout: read_all(&mut stdout),
        stderr: String::from_utf8_lossy(&read_all(&mut stderr)).into(),
    }
}

fn read_all(file: &mut File) -> Vec<u8> {
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_to_end(&mut bytes))
        .expect("read a tool's output");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_both_streams_and_the_status() {
        let out = run_tool(
            Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]),
            Duration::from_secs(10),
        );
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, "err\n");
    }

    #[test]
    #[should_panic(expected = "didn't finish within")]
    fn kills_a_child_that_outlives_the_timeout() {
        run_tool(Command::new("sleep").arg("5"), Duration::from_millis(100));
    }
}
