//! A child-process runner that follows A§8.7, used by the media runner,
//! `sources` and `doctor` while `crate::proc` (owner: session) is a stub.
//!
//! - stdin is null; the child leads its own process group (`process_group(0)`)
//!   and gets SIGKILL if the thread that spawned it dies (`PR_SET_PDEATHSIG`);
//! - it gets nothing but its stdio (`proc::close_on_exec_from`): not even a
//!   descriptor Qrec inherited from whoever started it;
//! - stdout is piped only when it is parsed, and always drained on its own
//!   thread (ffmpeg `-progress pipe:1` blocks, or captured bytes);
//! - stderr is appended to a log file, or captured through an anonymous
//!   temp file for short project-less tools (`pactl`, `busctl`): never a pipe;
//! - every run has a deadline: an overall timeout and/or a stall watchdog
//!   (no advancing `-progress` block for a while ⇒ "ffmpeg stalled");
//! - stopping (cancel, timeout, stall) is SIGTERM to the group, SIGKILL after
//!   the grace period, then at most one more grace period of waiting before
//!   the child is handed to a reaper thread. The caller never blocks in
//!   `wait()` for longer than that.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use qrec_core::parse::progress::{ProgressBlock, ProgressParser};

/// How often a running child is polled.
const POLL: Duration = Duration::from_millis(20);

/// SIGTERM → SIGKILL, and SIGKILL → giving up on `wait()` (A§8.7).
pub(crate) const KILL_GRACE: Duration = Duration::from_secs(2);

/// How long to wait for the stdout drain after the child exited.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// The most stderr kept in memory for [`Stderr::Capture`].
const CAPTURE_STDERR_LIMIT: u64 = 1 << 20;

/// A flag that stops a job's children (cancel, or a sibling's failure).
#[derive(Debug, Default)]
pub(crate) struct StopFlag(AtomicBool);

impl StopFlag {
    pub(crate) fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub(crate) fn clear(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Where the child's stdout goes.
pub(crate) enum Stdout {
    /// `/dev/null`: nothing parses it.
    Null,
    /// Read into memory; bytes past `limit` are drained and dropped.
    Capture { limit: usize },
    /// ffmpeg `-progress pipe:1`: every block goes to the callback (on the
    /// drain thread); blocks that advance feed the stall watchdog.
    Progress(Box<dyn FnMut(&ProgressBlock) + Send>),
}

/// Where the child's stderr goes (never a pipe).
pub(crate) enum Stderr<'a> {
    /// Appended to a log file (A§8.7).
    Log(&'a File),
    /// An anonymous temp file read back into [`Finished::stderr`].
    Capture,
}

/// One child process to run.
pub(crate) struct Spec<'a> {
    /// The program and its arguments, byte for byte: a path in them may
    /// not be UTF-8 (a project folder whose path isn't).
    pub argv: Vec<OsString>,
    pub cwd: Option<&'a Path>,
    /// Extra environment variables.
    pub env: Vec<(OsString, OsString)>,
    pub stdout: Stdout,
    pub stderr: Stderr<'a>,
    /// Wall-clock limit.
    pub timeout: Option<Duration>,
    /// Kill when no `-progress` block advanced for this long.
    pub stall: Option<Duration>,
    pub stop: Option<&'a StopFlag>,
    pub kill_grace: Duration,
}

impl<'a> Spec<'a> {
    /// `argv` with stdout and stderr discarded/captured, no deadline yet.
    pub(crate) fn new<I, S>(argv: I, stderr: Stderr<'a>) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Self {
            argv: argv
                .into_iter()
                .map(|arg| arg.as_ref().to_os_string())
                .collect(),
            cwd: None,
            env: Vec::new(),
            stdout: Stdout::Null,
            stderr,
            timeout: None,
            stall: None,
            stop: None,
            kill_grace: KILL_GRACE,
        }
    }

    pub(crate) fn cwd(mut self, dir: &'a Path) -> Self {
        self.cwd = Some(dir);
        self
    }

    pub(crate) fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub(crate) fn stdout(mut self, stdout: Stdout) -> Self {
        self.stdout = stdout;
        self
    }

    pub(crate) fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub(crate) fn stall(mut self, stall: Duration) -> Self {
        self.stall = Some(stall);
        self
    }

    pub(crate) fn stop(mut self, stop: &'a StopFlag) -> Self {
        self.stop = Some(stop);
        self
    }

    pub(crate) fn kill_grace(mut self, grace: Duration) -> Self {
        self.kill_grace = grace;
        self
    }
}

/// Why a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    /// It exited on its own.
    Exited,
    /// The wall-clock limit expired.
    TimedOut(Duration),
    /// No progress for this long.
    Stalled(Duration),
    /// The stop flag was set.
    Stopped,
}

/// What a finished (or abandoned) child left behind.
#[derive(Debug)]
pub(crate) struct Finished {
    /// `None` when the child survived SIGKILL and went to a reaper thread.
    pub status: Option<ExitStatus>,
    pub ended: Ended,
    /// [`Stdout::Capture`] bytes.
    pub stdout: Vec<u8>,
    /// [`Stderr::Capture`] text.
    pub stderr: String,
    /// A `progress=end` block was seen.
    pub progress_end: bool,
    /// The progress parser panicked (blocks after it were drained unparsed).
    pub parser_error: Option<String>,
    pub elapsed: Duration,
}

impl Finished {
    /// Exited on its own with status 0.
    pub(crate) fn exited_ok(&self) -> bool {
        self.ended == Ended::Exited && self.status.is_some_and(|s| s.success())
    }

    /// A one-line reason for a failure, e.g. "ffmpeg exited with status 1".
    pub(crate) fn describe(&self, program: &str) -> String {
        match self.ended {
            Ended::TimedOut(limit) => format!("{program} timed out after {}", human(limit)),
            Ended::Stalled(limit) => {
                format!("{program} stalled (no progress for {})", human(limit))
            }
            Ended::Stopped => "cancelled".to_owned(),
            Ended::Exited => match self.status {
                Some(status) => match (status.code(), status.signal()) {
                    (Some(0), _) => format!("{program} finished"),
                    (Some(code), _) => format!("{program} exited with status {code}"),
                    (None, Some(signal)) => format!("{program} was killed by signal {signal}"),
                    (None, None) => format!("{program} ended abnormally"),
                },
                None => format!("{program} didn't exit"),
            },
        }
    }
}

/// `20 s`, `1.5 s`, `250 ms`.
pub(crate) fn human(d: Duration) -> String {
    if d.as_millis() < 1000 {
        format!("{} ms", d.as_millis())
    } else if d.subsec_millis() == 0 {
        format!("{} s", d.as_secs())
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

/// Watchdog and progress state shared with the drain thread.
struct Shared {
    started: Instant,
    /// Milliseconds after `started` of the last advancing progress block.
    last_progress_ms: AtomicU64,
    progress_end: AtomicBool,
}

impl Shared {
    fn touch(&self) {
        let ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.last_progress_ms.store(ms, Ordering::SeqCst);
    }

    fn since_progress(&self, now: Instant) -> Duration {
        let last = Duration::from_millis(self.last_progress_ms.load(Ordering::SeqCst));
        now.saturating_duration_since(self.started + last)
    }
}

struct Drained {
    bytes: Vec<u8>,
    parser_error: Option<String>,
}

/// Why a child couldn't be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartError {
    pub message: String,
    /// The program isn't installed (or not on `PATH`): a missing dependency.
    pub not_found: bool,
}

impl From<String> for StartError {
    fn from(message: String) -> Self {
        Self {
            message,
            not_found: false,
        }
    }
}

/// Runs a child to completion (or to its deadline). `Err` only when it
/// can't be started.
pub(crate) fn run(spec: Spec<'_>) -> Result<Finished, String> {
    start(spec).map_err(|e| e.message)
}

/// [`run`], saying whether a failed start means the program is missing.
pub(crate) fn start(spec: Spec<'_>) -> Result<Finished, StartError> {
    let Some((program_os, args)) = spec.argv.split_first() else {
        return Err("internal error: an empty command line".to_owned().into());
    };
    // For messages and thread names.
    let program_name = program_os.to_string_lossy();
    let program: &str = &program_name;
    let mut cmd = Command::new(program_os);
    cmd.args(args).stdin(Stdio::null()).process_group(0);
    if let Some(dir) = spec.cwd {
        cmd.current_dir(dir);
    }
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    // SAFETY: the closure runs in the forked child before exec and only
    // makes async-signal-safe system calls (close_range or fcntl, prctl).
    unsafe {
        cmd.pre_exec(|| {
            // Only its stdio crosses: nothing Qrec inherited (A§8.7).
            crate::proc::close_on_exec_from(3);
            // The child dies with the thread that spawned it; a SIGKILLed
            // host leaves no orphaned ffmpeg behind.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let captured_stderr = match spec.stderr {
        Stderr::Log(file) => {
            let handle = file
                .try_clone()
                .map_err(|e| format!("can't share the log file with {program}: {e}"))?;
            cmd.stderr(handle);
            None
        }
        Stderr::Capture => {
            let file = tempfile::tempfile()
                .map_err(|e| format!("can't create a temp file for {program}'s errors: {e}"))?;
            let handle = file
                .try_clone()
                .map_err(|e| format!("can't share a temp file with {program}: {e}"))?;
            cmd.stderr(handle);
            Some(file)
        }
    };
    match spec.stdout {
        Stdout::Null => cmd.stdout(Stdio::null()),
        _ => cmd.stdout(Stdio::piped()),
    };

    let mut child = cmd.spawn().map_err(|e| StartError {
        message: spawn_error(program, &e),
        not_found: e.kind() == io::ErrorKind::NotFound,
    })?;
    let shared = Arc::new(Shared {
        started: Instant::now(),
        last_progress_ms: AtomicU64::new(0),
        progress_end: AtomicBool::new(false),
    });
    let limits = Limits {
        timeout: spec.timeout,
        stall: spec.stall,
        stop: spec.stop,
        kill_grace: spec.kill_grace,
    };
    let drain = match (child.stdout.take(), spec.stdout) {
        (Some(out), mode) => Some(spawn_drain(program, out, mode, Arc::clone(&shared))),
        (None, _) => None,
    };

    let (status, ended) = supervise(&mut child, &limits, &shared);
    let status = match status {
        Some(status) => Some(status),
        None => {
            reap_later(program, child);
            None
        }
    };

    let drained = match drain {
        Some(rx) => {
            let wait = if status.is_some() {
                DRAIN_GRACE
            } else {
                Duration::ZERO
            };
            rx.recv_timeout(wait).ok()
        }
        None => None,
    };
    let (stdout, parser_error) = match drained {
        Some(d) => (d.bytes, d.parser_error),
        None => (Vec::new(), None),
    };
    let stderr = captured_stderr
        .map(|mut file| read_capped(&mut file, CAPTURE_STDERR_LIMIT))
        .unwrap_or_default();
    Ok(Finished {
        status,
        ended,
        stdout,
        stderr,
        progress_end: shared.progress_end.load(Ordering::SeqCst),
        parser_error,
        elapsed: shared.started.elapsed(),
    })
}

/// The deadlines of one run.
struct Limits<'a> {
    timeout: Option<Duration>,
    stall: Option<Duration>,
    stop: Option<&'a StopFlag>,
    kill_grace: Duration,
}

/// Polls the child until it exits or a deadline passes; stops it with
/// SIGTERM, then SIGKILL. `None` = still alive after SIGKILL + grace.
fn supervise(child: &mut Child, spec: &Limits<'_>, shared: &Shared) -> (Option<ExitStatus>, Ended) {
    let pid = child.id();
    let mut reason: Option<Ended> = None;
    let mut term_at: Option<Instant> = None;
    let mut kill_at: Option<Instant> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (Some(status), reason.unwrap_or(Ended::Exited)),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("can't poll child {pid}: {e}");
                signal_group(pid, rustix::process::Signal::KILL);
                return (None, reason.unwrap_or(Ended::Exited));
            }
        }
        let now = Instant::now();
        if reason.is_none() {
            let stopped = spec
                .stop
                .is_some_and(StopFlag::is_set)
                .then_some(Ended::Stopped);
            let timed_out = || {
                spec.timeout
                    .filter(|t| now - shared.started >= *t)
                    .map(Ended::TimedOut)
            };
            let stalled = || {
                spec.stall
                    .filter(|s| shared.since_progress(now) >= *s)
                    .map(Ended::Stalled)
            };
            reason = stopped.or_else(timed_out).or_else(stalled);
            if reason.is_some() {
                signal_group(pid, rustix::process::Signal::TERM);
                term_at = Some(now);
            }
        }
        match (term_at, kill_at) {
            (Some(term), None) if now - term >= spec.kill_grace => {
                signal_group(pid, rustix::process::Signal::KILL);
                kill_at = Some(now);
            }
            (_, Some(kill)) if now - kill >= spec.kill_grace => {
                return (None, reason.unwrap_or(Ended::Exited));
            }
            _ => {}
        }
        thread::sleep(POLL);
    }
}

/// Signals the child's process group (its pid: `process_group(0)`). Only
/// called while the child is unreaped, so the group id can't be reused.
fn signal_group(pid: u32, signal: rustix::process::Signal) {
    let Some(pgid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    if let Err(e) = rustix::process::kill_process_group(pgid, signal) {
        if e != rustix::io::Errno::SRCH {
            tracing::warn!("can't signal process group {pid}: {e}");
        }
    }
}

/// A child that survived SIGKILL (stuck in the kernel): a detached thread
/// waits for it so it doesn't stay a zombie.
fn reap_later(program: &str, mut child: Child) {
    let pid = child.id();
    tracing::warn!("{program} (pid {pid}) didn't exit after SIGKILL; leaving it to a reaper");
    let spawned = thread::Builder::new()
        .name(format!("qrec-reap-{pid}"))
        .spawn(move || {
            let _ = child.wait();
        });
    if let Err(e) = spawned {
        tracing::error!("can't start a reaper thread for pid {pid}: {e}");
    }
}

fn spawn_drain(
    program: &str,
    out: ChildStdout,
    mode: Stdout,
    shared: Arc<Shared>,
) -> crossbeam_channel::Receiver<Drained> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let name = format!("qrec-drain-{program}");
    let body = move || {
        let drained = match mode {
            Stdout::Null => Drained {
                bytes: Vec::new(),
                parser_error: None,
            },
            Stdout::Capture { limit } => Drained {
                bytes: drain_bytes(out, limit),
                parser_error: None,
            },
            Stdout::Progress(callback) => drain_progress(out, callback, &shared),
        };
        let _ = tx.send(drained);
    };
    if let Err(e) = thread::Builder::new().name(name).spawn(body) {
        // Without a drain thread the child could block on a full pipe; the
        // watchdog (or the caller's timeout) still ends it.
        tracing::error!("can't start a stdout drain thread: {e}");
    }
    rx
}

fn drain_bytes(mut out: ChildStdout, limit: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match out.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let room = limit.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    kept
}

fn drain_progress(
    out: ChildStdout,
    mut callback: Box<dyn FnMut(&ProgressBlock) + Send>,
    shared: &Shared,
) -> Drained {
    let mut reader = BufReader::new(out);
    let mut parser = ProgressParser::new();
    let mut parser_error: Option<String> = None;
    let mut line = Vec::new();
    let mut last: Option<(Option<u64>, Option<i64>)> = None;
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        if parser_error.is_some() {
            continue; // keep the pipe drained
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\n', '\r']);
        match catch_unwind(AssertUnwindSafe(|| parser.feed(text))) {
            Ok(Some(block)) => {
                let position = (block.frame, block.out_time_ns);
                if block.end || last != Some(position) {
                    shared.touch();
                }
                last = Some(position);
                if block.end {
                    shared.progress_end.store(true, Ordering::SeqCst);
                }
                callback(&block);
            }
            Ok(None) => {}
            Err(panic) => {
                parser_error = Some(format!(
                    "the -progress parser failed: {}",
                    panic_message(panic.as_ref())
                ))
            }
        }
    }
    Drained {
        bytes: Vec::new(),
        parser_error,
    }
}

/// The text of a caught panic.
pub(crate) fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

/// The last `limit` bytes of `file` as text.
fn read_capped(file: &mut File, limit: u64) -> String {
    let len = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let start = len.saturating_sub(limit);
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_ok() {
        let _ = file.take(limit).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn spawn_error(program: &str, e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::NotFound {
        format!("{program} wasn't found (is it installed and on PATH?)")
    } else {
        format!("can't run {program}: {e}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn captures_stdout_and_stderr() {
        let cmd = argv(&["sh", "-c", "printf out; printf err >&2; exit 3"]);
        let done = run(Spec::new(&cmd, Stderr::Capture)
            .stdout(Stdout::Capture { limit: 1024 })
            .timeout(Duration::from_secs(10)))
        .unwrap();
        assert_eq!(done.status.unwrap().code(), Some(3));
        assert_eq!(done.ended, Ended::Exited);
        assert!(!done.exited_ok());
        assert_eq!(done.stdout, b"out");
        assert_eq!(done.stderr, "err");
        assert_eq!(done.describe("sh"), "sh exited with status 3");
    }

    /// Like `proc`'s runner (A§8.7): an ffmpeg an export starts never holds
    /// a descriptor Qrec inherited from whoever started it.
    #[test]
    fn children_get_no_inherited_descriptor() {
        let fd = crate::proc::tests::inheritable_fd();
        let script = crate::proc::tests::holds_fd_script(&fd);
        let cmd = argv(&["sh", "-c", &script]);
        let done = run(Spec::new(&cmd, Stderr::Capture)
            .stdout(Stdout::Capture { limit: 1024 })
            .timeout(Duration::from_secs(10)))
        .unwrap();
        assert!(done.exited_ok(), "{}", done.describe("sh"));
        assert_eq!(done.stdout, b"free\n");
    }

    #[test]
    fn stderr_goes_to_the_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.log");
        let log = File::create(&path).unwrap();
        let cmd = argv(&["sh", "-c", "echo to-the-log >&2; pwd"]);
        let done = run(Spec::new(&cmd, Stderr::Log(&log))
            .cwd(dir.path())
            .stdout(Stdout::Capture { limit: 4096 })
            .timeout(Duration::from_secs(10)))
        .unwrap();
        assert!(done.exited_ok());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "to-the-log\n");
        let cwd = String::from_utf8(done.stdout).unwrap();
        assert_eq!(
            std::fs::canonicalize(cwd.trim()).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
    }

    #[test]
    fn capture_limit_still_drains_everything() {
        // 1 MiB through a 64 KiB pipe: the child would block without the drain.
        let cmd = argv(&["sh", "-c", "head -c 1048576 /dev/zero"]);
        let done = run(Spec::new(&cmd, Stderr::Capture)
            .stdout(Stdout::Capture { limit: 10 })
            .timeout(Duration::from_secs(10)))
        .unwrap();
        assert!(done.exited_ok());
        assert_eq!(done.stdout.len(), 10);
    }

    #[test]
    fn timeout_kills_the_whole_group() {
        let cmd = argv(&["sh", "-c", "sleep 30 & sleep 30; wait"]);
        let started = Instant::now();
        let done = run(Spec::new(&cmd, Stderr::Capture)
            .timeout(Duration::from_millis(200))
            .kill_grace(Duration::from_millis(500)))
        .unwrap();
        assert_eq!(done.ended, Ended::TimedOut(Duration::from_millis(200)));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(done.status.unwrap().signal(), Some(libc::SIGTERM));
        assert_eq!(done.describe("sleep"), "sleep timed out after 200 ms");
    }

    #[test]
    fn sigterm_is_followed_by_sigkill() {
        let cmd = argv(&[
            "sh",
            "-c",
            "trap '' TERM; echo ready; while :; do sleep 0.05; done",
        ]);
        let stop = StopFlag::default();
        let started = Instant::now();
        let done = thread::scope(|scope| {
            // Stop only once the shell ignores SIGTERM.
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(300));
                stop.set();
            });
            run(Spec::new(&cmd, Stderr::Capture)
                .stdout(Stdout::Capture { limit: 64 })
                .stop(&stop)
                .kill_grace(Duration::from_millis(300)))
            .unwrap()
        });
        assert_eq!(done.stdout, b"ready\n");
        assert_eq!(done.ended, Ended::Stopped);
        assert_eq!(done.status.unwrap().signal(), Some(libc::SIGKILL));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(done.describe("sh"), "cancelled");
    }

    #[test]
    fn missing_programs_are_reported() {
        let cmd = argv(&["qrec-no-such-tool-here"]);
        let err = run(Spec::new(&cmd, Stderr::Capture)).unwrap_err();
        assert!(err.contains("wasn't found"), "{err}");
        assert!(run(Spec::new(Vec::<String>::new(), Stderr::Capture)).is_err());
    }

    #[test]
    fn human_durations() {
        assert_eq!(human(Duration::from_secs(20)), "20 s");
        assert_eq!(human(Duration::from_millis(1500)), "1.5 s");
        assert_eq!(human(Duration::from_millis(250)), "250 ms");
    }

    #[test]
    fn progress_blocks_feed_the_callback_and_the_watchdog() {
        let script = "printf 'frame=1\\nout_time_us=1000\\nprogress=continue\\n'; \
                      printf 'frame=2\\nout_time_us=2000\\nprogress=end\\n'";
        let cmd = argv(&["sh", "-c", script]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let done = run(Spec::new(&cmd, Stderr::Capture)
            .stdout(Stdout::Progress(Box::new(move |b| {
                sink.lock().unwrap().push(b.frame)
            })))
            .stall(Duration::from_secs(10)))
        .unwrap();
        assert!(done.exited_ok() && done.progress_end, "{done:?}");
        assert_eq!(*seen.lock().unwrap(), [Some(1), Some(2)]);

        // A child that never reports progress is killed as stalled.
        let cmd = argv(&["sleep", "30"]);
        let done = run(Spec::new(&cmd, Stderr::Capture)
            .stdout(Stdout::Progress(Box::new(|_| {})))
            .stall(Duration::from_millis(200))
            .kill_grace(Duration::from_millis(300)))
        .unwrap();
        assert_eq!(done.ended, Ended::Stalled(Duration::from_millis(200)));
        assert_eq!(
            done.describe("ffmpeg"),
            "ffmpeg stalled (no progress for 200 ms)"
        );
    }

    #[test]
    fn a_silent_child_stalls_even_without_a_parser() {
        let cmd = argv(&["sleep", "30"]);
        let done = run(Spec::new(&cmd, Stderr::Capture)
            .stdout(Stdout::Progress(Box::new(|_| {})))
            .stall(Duration::from_millis(200))
            .kill_grace(Duration::from_millis(300)))
        .unwrap();
        assert_eq!(done.ended, Ended::Stalled(Duration::from_millis(200)));
        assert!(!done.progress_end);
    }
}
