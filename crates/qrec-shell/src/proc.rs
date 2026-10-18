//! The process runner (A§8.7): one way to run every child process.
//!
//! - stdin is `/dev/null` (or a file, e.g. the PNG `wl-copy` reads);
//! - the child leads its own process group (`process_group(0)`), so a kill
//!   reaches everything it started;
//! - it gets nothing but its stdio: every other descriptor Qrec holds is
//!   close-on-exec, including any it inherited from whoever started it
//!   ([`close_on_exec_from`]);
//! - stdout is piped only when the caller parses it, and then drained on
//!   its own thread (lines to a callback, or collected up to a limit);
//! - stderr is appended to a log file (never a pipe nobody reads);
//! - every run has a bound: a whole-run timeout, a stall watchdog (no
//!   stdout activity for a while, e.g. no ffmpeg `-progress` block for
//!   60 s), or both;
//! - stopping a child: SIGTERM to its group, SIGKILL after
//!   [`TERM_GRACE`], wait at most [`KILL_WAIT`] more, then give up on it:
//!   the thread that spawned it keeps blocking in `wait()` (it is the
//!   reaper), the caller gets its result. Nobody else ever blocks in
//!   `wait()`, so the session thread can't hang on a stuck child.
//!
//! Children are spawned by a dedicated owner thread that lives exactly as
//! long as the child, which makes `PR_SET_PDEATHSIG` safe to use: if the
//! host dies (even by SIGKILL), its children get SIGKILL too.
//!
//! Logs live in `.qrec/logs/YYYYMMDD-HHMMSS-<pid>-j<job>-<step>.log`
//! (created with `O_EXCL`); [`prune_logs`] keeps the newest
//! [`KEEP_LOGS`] and [`prune_temp_outputs`] deletes temp outputs
//! (`.<stem>.qrec-part-<pid>-<job>[.<ext>]`) left over for longer than
//! [`TEMP_MAX_AGE`] (both run at host start).
//!
//! Owner: session.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::fd::RawFd;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Local};
use crossbeam_channel::{Receiver, Sender, bounded};
use qrec_core::{CoreError, ErrorCode, JobId};
use rustix::process::{Pid, Signal};

use crate::media::{is_stamped_log_name, is_temp_name};
use crate::paths::ProjectPaths;
use crate::spawn::{Command, Stdio};

/// `ffprobe` runs (A§8.7).
pub const FFPROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// `pactl` runs.
pub const PACTL_TIMEOUT: Duration = Duration::from_secs(3);
/// VA-API and encoder probes (`ffmpeg -version`, `-encoders`, the h264_vaapi check).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// `wl-copy --type image/png`.
pub const WL_COPY_TIMEOUT: Duration = Duration::from_secs(5);
/// Long ffmpeg jobs: no `-progress` block for this long ⇒ "ffmpeg stalled".
pub const STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// SIGTERM → SIGKILL.
pub const TERM_GRACE: Duration = Duration::from_secs(2);
/// After SIGKILL, how long the caller waits before leaving the child to its reaper.
pub const KILL_WAIT: Duration = Duration::from_secs(2);
/// After the child exits, how long its stdout may stay open (a leftover
/// grandchild holding the pipe) before the group is killed.
pub const STDOUT_DRAIN: Duration = Duration::from_secs(2);
/// Logs kept by [`prune_logs`] at host start.
pub const KEEP_LOGS: usize = 200;
/// Temp outputs (`.<stem>.qrec-part-<pid>-<job>[.<ext>]`) older than this
/// are deleted at host start.
pub const TEMP_MAX_AGE: Duration = Duration::from_secs(3600);

// ------------------------------------------------------------------ errors

/// A child that couldn't be started.
#[derive(Debug, thiserror::Error)]
pub enum ProcError {
    /// The program isn't installed (or not on `PATH`).
    #[error("{program} is not installed (or not on PATH)")]
    NotFound { program: String },
    #[error("can't start {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("empty command line")]
    EmptyArgv,
}

impl ProcError {
    /// `missing_dependency` for a missing program, `io` otherwise.
    pub fn to_core(&self) -> CoreError {
        match self {
            ProcError::NotFound { program } => CoreError::new(
                ErrorCode::MissingDependency,
                format!("{program} is not installed (or not on PATH)"),
            )
            .with_hint("run `qrec doctor` to see what is missing"),
            ProcError::Spawn { .. } => CoreError::io(self.to_string()),
            ProcError::EmptyArgv => CoreError::internal(self.to_string()),
        }
    }
}

impl From<ProcError> for CoreError {
    fn from(e: ProcError) -> Self {
        e.to_core()
    }
}

// -------------------------------------------------------------------- spec

/// How long a child may run. Every child has a bound (A§8.7: every wait
/// has a timeout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// Killed when it runs longer than this.
    Timeout(Duration),
    /// Killed when stdout shows no activity for this long (see
    /// [`Spec::activity_marker`]). Needs a piped stdout.
    Stall(Duration),
    /// Both.
    Both { timeout: Duration, stall: Duration },
}

impl Limit {
    fn timeout(self) -> Option<Duration> {
        match self {
            Limit::Timeout(t) | Limit::Both { timeout: t, .. } => Some(t),
            Limit::Stall(_) => None,
        }
    }

    fn stall(self) -> Option<Duration> {
        match self {
            Limit::Stall(s) | Limit::Both { stall: s, .. } => Some(s),
            Limit::Timeout(_) => None,
        }
    }
}

/// A line-by-line stdout consumer, called on the stdout thread.
pub type LineHandler = Box<dyn FnMut(&str) + Send>;

enum StdoutMode {
    Null,
    Capture(usize),
    Lines(LineHandler),
}

/// What to run and how. Build with [`Spec::new`], then [`Spec::run`]
/// (blocking, bounded) or [`Spec::start`] (returns at once).
pub struct Spec {
    label: String,
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, Option<OsString>)>,
    cwd: Option<PathBuf>,
    stdin: Option<PathBuf>,
    stdout: StdoutMode,
    marker: Option<String>,
    stderr: Option<File>,
    limit: Limit,
    die_with_parent: bool,
}

impl std::fmt::Debug for Spec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spec")
            .field("label", &self.label)
            .field("program", &self.program)
            .field("args", &self.args)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

impl Spec {
    /// `program` with no arguments, stdin/stdout/stderr all `/dev/null`.
    pub fn new(program: impl AsRef<OsStr>, limit: Limit) -> Self {
        let program = program.as_ref().to_owned();
        let label = Path::new(&program)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string_lossy().into_owned());
        Self {
            label,
            program,
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            stdin: None,
            stdout: StdoutMode::Null,
            marker: None,
            stderr: None,
            limit,
            die_with_parent: true,
        }
    }

    /// `argv[0]` is the program (the core's argv contract, A§9.2).
    pub fn from_argv(argv: &[String], limit: Limit) -> Result<Self, ProcError> {
        let (program, args) = argv.split_first().ok_or(ProcError::EmptyArgv)?;
        Ok(Self::new(program, limit).args(args))
    }

    /// The name used in messages (default: the program's file name).
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_owned()));
        self
    }

    /// Sets an environment variable for the child.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_owned(), Some(value.as_ref().to_owned())));
        self
    }

    /// Removes an inherited environment variable.
    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.env.push((key.as_ref().to_owned(), None));
        self
    }

    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// stdin reads this file instead of `/dev/null`.
    pub fn stdin_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.stdin = Some(path.into());
        self
    }

    /// Collects stdout (drained on its own thread), keeping at most `limit`
    /// bytes; the rest is read and dropped ([`Finished::stdout_truncated`]).
    pub fn capture_stdout(mut self, limit: usize) -> Self {
        self.stdout = StdoutMode::Capture(limit);
        self
    }

    /// Hands every stdout line (without its newline, lossy UTF-8) to
    /// `handler` on the stdout thread.
    pub fn stdout_lines(mut self, handler: impl FnMut(&str) + Send + 'static) -> Self {
        self.stdout = StdoutMode::Lines(Box::new(handler));
        self
    }

    /// Only stdout lines starting with `marker` count as activity for the
    /// stall watchdog (ffmpeg: `progress=`). Default: every line.
    pub fn activity_marker(mut self, marker: impl Into<String>) -> Self {
        self.marker = Some(marker.into());
        self
    }

    /// Appends stderr to `log` (opened for appending). The runner writes a
    /// `#` header line with the command before the child starts and a
    /// footer with how it ended.
    pub fn stderr_to(mut self, log: File) -> Self {
        self.stderr = Some(log);
        self
    }

    /// Whether the child gets SIGKILL when its owner thread (and so the
    /// host) dies. Default true.
    pub fn die_with_parent(mut self, yes: bool) -> Self {
        self.die_with_parent = yes;
        self
    }

    /// The command line, shell-quoted, for logs and messages.
    pub fn command_line(&self) -> String {
        std::iter::once(&self.program)
            .chain(&self.args)
            .map(|a| shell_quote(&a.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Runs the child to the end (or to its bound) on this thread's behalf.
    /// Blocks at most the bound plus [`TERM_GRACE`] + [`KILL_WAIT`] (+
    /// [`STDOUT_DRAIN`] when a grandchild keeps stdout open).
    pub fn run(self) -> Result<Finished, ProcError> {
        self.start().map(Running::wait)
    }

    /// Starts the child and returns at once. Call [`Running::wait`] on a
    /// thread that may block; cancel from anywhere with a [`Canceller`].
    pub fn start(mut self) -> Result<Running, ProcError> {
        if self.limit.stall().is_some() && matches!(self.stdout, StdoutMode::Null) {
            // The watchdog needs stdout activity; drain it so it can see some.
            self.stdout = StdoutMode::Capture(0);
        }
        // Started without forking Qrec (`crate::spawn`): its own process
        // group, only its stdio crossing (nothing Qrec inherited), and
        // SIGKILL when its owner thread dies.
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        for (key, value) in &self.env {
            match value {
                Some(v) => cmd.env(key, v),
                None => cmd.env_remove(key),
            };
        }
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        let stdin = match &self.stdin {
            None => Stdio::Null,
            Some(path) => match File::open(path) {
                Ok(file) => Stdio::from(file),
                Err(source) => {
                    return Err(ProcError::Spawn {
                        program: self.label.clone(),
                        source: io::Error::new(
                            source.kind(),
                            format!("can't open its input {}: {source}", path.display()),
                        ),
                    });
                }
            },
        };
        cmd.stdin(stdin);
        cmd.stdout(match self.stdout {
            StdoutMode::Null => Stdio::Null,
            _ => Stdio::Piped,
        });
        let mut log = self.stderr.take();
        match &log {
            Some(file) => match file.try_clone() {
                Ok(clone) => {
                    cmd.stderr(Stdio::from(clone));
                }
                Err(source) => {
                    return Err(ProcError::Spawn {
                        program: self.label.clone(),
                        source,
                    });
                }
            },
            None => {
                cmd.stderr(Stdio::Null);
            }
        }
        cmd.die_with_parent(self.die_with_parent);

        if let Some(file) = log.as_mut() {
            let _ = writeln!(
                file,
                "# {} $ {}",
                Local::now().format("%Y-%m-%d %H:%M:%S"),
                self.command_line()
            );
        }

        let (spawn_tx, spawn_rx) = bounded::<io::Result<(u32, Option<File>)>>(1);
        let (exit_tx, exit_rx) = bounded::<io::Result<ExitStatus>>(1);
        let (cancel_tx, cancel_rx) = bounded(1);
        let control = Arc::new(Control {
            pgid: AtomicU32::new(0),
            exited: AtomicBool::new(false),
            cancel_tx,
        });
        let owner_control = Arc::clone(&control);
        let owner = thread::Builder::new()
            .name(format!("proc-{}", self.label))
            .spawn(move || {
                let mut child = match cmd.spawn() {
                    Ok(child) => child,
                    Err(e) => {
                        let _ = spawn_tx.send(Err(e));
                        return;
                    }
                };
                let pid = child.id();
                register_group(pid);
                owner_control.pgid.store(pid, Ordering::Release);
                let stdout = child.stdout.take();
                let _ = spawn_tx.send(Ok((pid, stdout)));
                // This thread is the child's reaper: it blocks here until
                // the child is gone, however long that takes.
                let status = child.wait();
                // From now on the pid may be reused: never signal it again.
                owner_control.exited.store(true, Ordering::Release);
                unregister_group(pid);
                let _ = exit_tx.send(status);
            });
        if let Err(source) = owner {
            return Err(ProcError::Spawn {
                program: self.label,
                source,
            });
        }
        let (pid, stdout) = match spawn_rx.recv() {
            Ok(Ok(spawned)) => spawned,
            Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(file) = log.as_mut() {
                    let _ = writeln!(file, "# not found: {}", self.program.to_string_lossy());
                }
                return Err(ProcError::NotFound {
                    program: self.program.to_string_lossy().into_owned(),
                });
            }
            Ok(Err(source)) => {
                if let Some(file) = log.as_mut() {
                    let _ = writeln!(file, "# can't start: {source}");
                }
                return Err(ProcError::Spawn {
                    program: self.label,
                    source,
                });
            }
            Err(_) => {
                return Err(ProcError::Spawn {
                    program: self.label,
                    source: io::Error::other("the process owner thread died"),
                });
            }
        };

        let started = Instant::now();
        let activity = Arc::new(AtomicU64::new(0));
        let stdout_rx = stdout.map(|pipe| {
            let mode = std::mem::replace(&mut self.stdout, StdoutMode::Null);
            spawn_reader(
                pipe,
                mode,
                self.marker.clone(),
                started,
                Arc::clone(&activity),
            )
        });
        Ok(Running {
            label: self.label,
            pid,
            started,
            exit_rx,
            stdout_rx,
            activity,
            control,
            cancel_rx,
            limit: self.limit,
            log,
        })
    }
}

/// What the stdout thread hands back at EOF.
struct StdoutDone {
    bytes: Vec<u8>,
    truncated: bool,
}

fn spawn_reader(
    pipe: File,
    mode: StdoutMode,
    marker: Option<String>,
    started: Instant,
    activity: Arc<AtomicU64>,
) -> Receiver<StdoutDone> {
    let (tx, rx) = bounded(1);
    let spawned = thread::Builder::new()
        .name("proc-stdout".into())
        .spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut line = Vec::new();
            let mut done = StdoutDone {
                bytes: Vec::new(),
                truncated: false,
            };
            let (mut handler, limit) = match mode {
                StdoutMode::Lines(h) => (Some(h), 0),
                StdoutMode::Capture(limit) => (None, limit),
                StdoutMode::Null => (None, 0),
            };
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end_matches(['\n', '\r']);
                if marker.as_deref().is_none_or(|m| text.starts_with(m)) {
                    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    activity.store(ms, Ordering::Relaxed);
                }
                match handler.as_mut() {
                    Some(h) => h(text),
                    None => {
                        let room = limit.saturating_sub(done.bytes.len());
                        if line.len() > room {
                            done.truncated = true;
                        }
                        done.bytes.extend_from_slice(&line[..line.len().min(room)]);
                    }
                }
            }
            let _ = tx.send(done);
        });
    if spawned.is_err() {
        // No thread: nobody drains the pipe. The caller's bound still
        // applies; report an empty stdout.
        let (tx, rx) = bounded(1);
        let _ = tx.send(StdoutDone {
            bytes: Vec::new(),
            truncated: true,
        });
        return rx;
    }
    rx
}

// ----------------------------------------------------------------- running

/// Shared by the waiter, cancellers and the owner (reaper) thread.
struct Control {
    /// The child's pid = process group id (0 until spawned).
    pgid: AtomicU32,
    /// Set by the reaper right after `wait()` returned: the pid may be reused.
    exited: AtomicBool,
    cancel_tx: Sender<String>,
}

impl Control {
    /// Signals the child's group unless it has been reaped.
    fn signal(&self, sig: Signal) {
        let pgid = self.pgid.load(Ordering::Acquire);
        if pgid != 0 && !self.exited.load(Ordering::Acquire) {
            let _ = signal_group(pgid, sig);
        }
    }
}

/// A started child. [`Running::wait`] blocks (bounded); a [`Canceller`]
/// stops it from any thread without blocking.
pub struct Running {
    label: String,
    pid: u32,
    started: Instant,
    exit_rx: Receiver<io::Result<ExitStatus>>,
    stdout_rx: Option<Receiver<StdoutDone>>,
    activity: Arc<AtomicU64>,
    control: Arc<Control>,
    cancel_rx: Receiver<String>,
    limit: Limit,
    log: Option<File>,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running")
            .field("label", &self.label)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// Stops a running child from any thread: SIGTERM to its group now; the
/// thread in [`Running::wait`] sends SIGKILL after [`TERM_GRACE`].
#[derive(Clone)]
pub struct Canceller {
    control: Arc<Control>,
}

impl std::fmt::Debug for Canceller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Canceller")
            .field("pid", &self.pid())
            .finish()
    }
}

impl Canceller {
    /// Never blocks. Cancelling a finished child does nothing.
    pub fn cancel(&self, why: impl Into<String>) {
        if self.control.exited.load(Ordering::Acquire) {
            return;
        }
        let _ = self.control.cancel_tx.try_send(why.into());
        self.control.signal(Signal::TERM);
    }

    /// The child's pid (and process group id).
    pub fn pid(&self) -> u32 {
        self.control.pgid.load(Ordering::Acquire)
    }
}

impl Running {
    /// The child's pid, which is also its process group id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn canceller(&self) -> Canceller {
        Canceller {
            control: Arc::clone(&self.control),
        }
    }

    /// Waits for the child within its bound, then collects stdout.
    pub fn wait(mut self) -> Finished {
        let timeout_at = self.limit.timeout().map(|t| self.started + t);
        let stall = self.limit.stall();
        let mut end = End::Exited;
        let status = loop {
            let now = Instant::now();
            let stall_at = stall.map(|s| {
                let last = Duration::from_millis(self.activity.load(Ordering::Relaxed));
                self.started + last + s
            });
            if timeout_at.is_some_and(|t| now >= t) {
                end = End::TimedOut;
                break self.stop();
            }
            if stall_at.is_some_and(|t| now >= t) {
                end = End::Stalled;
                break self.stop();
            }
            let next = [timeout_at, stall_at]
                .into_iter()
                .flatten()
                .min()
                .map_or(Duration::from_secs(3600), |t| {
                    t.saturating_duration_since(now)
                });
            crossbeam_channel::select! {
                recv(self.exit_rx) -> status => break status.ok().and_then(Result::ok),
                recv(self.cancel_rx) -> why => {
                    end = End::Cancelled(why.unwrap_or_default());
                    break self.stop();
                }
                default(next) => {}
            }
        };
        let (stdout, truncated) = self.collect_stdout(status.is_some());
        let finished = Finished {
            label: self.label.clone(),
            pid: self.pid,
            end,
            status,
            stdout,
            stdout_truncated: truncated,
            elapsed: self.started.elapsed(),
        };
        if let Some(log) = self.log.as_mut() {
            let _ = writeln!(
                log,
                "# {} ({:.1} s)",
                finished.describe(),
                finished.elapsed.as_secs_f64()
            );
        }
        finished
    }

    /// SIGTERM, SIGKILL after the grace period, then give up after
    /// [`KILL_WAIT`] (the owner thread keeps reaping).
    fn stop(&self) -> Option<ExitStatus> {
        self.control.signal(Signal::TERM);
        if let Ok(status) = self.exit_rx.recv_timeout(TERM_GRACE) {
            return status.ok();
        }
        self.control.signal(Signal::KILL);
        match self.exit_rx.recv_timeout(KILL_WAIT) {
            Ok(status) => status.ok(),
            Err(_) => {
                tracing::warn!(
                    "{} (pid {}) survived SIGKILL for {:?}; leaving it to its reaper thread",
                    self.label,
                    self.pid,
                    KILL_WAIT
                );
                None
            }
        }
    }

    /// The stdout thread's result. A grandchild may still hold the pipe
    /// after the child exited: then the group is killed and the wait is
    /// bounded again.
    fn collect_stdout(&mut self, exited: bool) -> (Vec<u8>, bool) {
        let Some(rx) = self.stdout_rx.take() else {
            return (Vec::new(), false);
        };
        let wait = if exited {
            STDOUT_DRAIN
        } else {
            Duration::from_millis(200)
        };
        if let Ok(done) = rx.recv_timeout(wait) {
            return (done.bytes, done.truncated);
        }
        // The leader may be gone while its group lives on in a grandchild
        // (the group id isn't reused while any member exists).
        if exited {
            let _ = signal_group(self.pid, Signal::KILL);
        } else {
            self.control.signal(Signal::KILL);
        }
        match rx.recv_timeout(KILL_WAIT) {
            Ok(done) => (done.bytes, done.truncated),
            Err(_) => (Vec::new(), true),
        }
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// The child exited on its own (see [`Finished::status`]).
    Exited,
    /// It ran longer than its timeout and was killed.
    TimedOut,
    /// The stall watchdog fired and it was killed.
    Stalled,
    /// A [`Canceller`] stopped it (with this reason).
    Cancelled(String),
}

/// The result of a run.
#[derive(Debug)]
pub struct Finished {
    pub label: String,
    pub pid: u32,
    pub end: End,
    /// `None` when the child couldn't be reaped in time (left to its reaper).
    pub status: Option<ExitStatus>,
    /// Collected stdout (empty unless [`Spec::capture_stdout`]).
    pub stdout: Vec<u8>,
    pub stdout_truncated: bool,
    pub elapsed: Duration,
}

impl Finished {
    /// Exited on its own with status 0.
    pub fn success(&self) -> bool {
        self.end == End::Exited && self.status.is_some_and(|s| s.success())
    }

    /// stdout as text (lossy UTF-8).
    pub fn stdout_text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }

    /// One line: "ffprobe exited with status 1", "ffmpeg stalled (no
    /// progress for 60 s)", "pactl timed out after 3 s", …
    pub fn describe(&self) -> String {
        let label = &self.label;
        match &self.end {
            End::TimedOut => format!("{label} timed out after {}", secs(self.elapsed)),
            End::Stalled => format!("{label} stalled (no progress)"),
            End::Cancelled(why) if why.is_empty() => format!("{label} was cancelled"),
            End::Cancelled(why) => format!("{label} was cancelled ({why})"),
            End::Exited => match self.status {
                Some(s) if s.success() => format!("{label} finished"),
                Some(s) => match (s.code(), s.signal()) {
                    (Some(code), _) => format!("{label} exited with status {code}"),
                    (None, Some(sig)) => format!("{label} was killed by signal {sig}"),
                    _ => format!("{label} failed ({s})"),
                },
                None => format!("{label} couldn't be waited for"),
            },
        }
    }
}

fn secs(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s >= 10.0 {
        format!("{} s", s.round())
    } else {
        format!("{s:.1} s")
    }
}

// ------------------------------------------------- inherited descriptors

/// The most descriptors [`close_on_exec_from`] walks one by one when the
/// kernel can't do it at once and the descriptor limit is unlimited.
const FD_SWEEP_MAX: libc::rlim_t = 1 << 20;

/// Marks every descriptor from `first` up close-on-exec. For a child
/// between its start and exec (`crate::spawn`'s, and a `pre_exec`'s after
/// a fork: the headless host's): the program then starts with its stdio and
/// what the caller placed below `first` (a headless host's lock and ready
/// pipe, A§8.3 step 5), never with a descriptor Qrec itself inherited from
/// whoever started it. Rust opens everything close-on-exec, but a key-bind
/// script's `flock` (`exec 9>lock; flock -n 9 …; qrec record toggle`) or a
/// harness's pipe isn't: a child that outlives the command, above all the
/// background recorder, would hold it for the whole take, and whoever waits
/// for that lock or for the pipe's end would wait for the take too.
///
/// `close_range(first, ~0, CLOSE_RANGE_CLOEXEC)` (Linux 5.11), else
/// `fcntl(F_SETFD)` on each descriptor below the `RLIMIT_NOFILE` soft limit
/// (at most [`FD_SWEEP_MAX`]). Close-on-exec rather than closed, so std's
/// own exec-error pipe still reports a failed `exec`. Best effort, and only
/// async-signal-safe system calls.
pub(crate) fn close_on_exec_from(first: RawFd) {
    let Ok(low) = libc::c_uint::try_from(first) else {
        return;
    };
    // SAFETY: close_range(2) only sets the close-on-exec flag of this
    // process's descriptors; the arguments are plain integers.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            low,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if rc == 0 {
        return;
    }
    // ENOSYS (before 5.9), EINVAL (no CLOSE_RANGE_CLOEXEC before 5.11), a
    // seccomp filter: one descriptor at a time.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit(2) writes into the struct we own.
    let top = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        limit.rlim_cur.min(FD_SWEEP_MAX)
    } else {
        1024
    };
    mark_close_on_exec(first..RawFd::try_from(top).unwrap_or(RawFd::MAX));
}

/// [`close_on_exec_from`] without `close_range`: `fcntl(F_SETFD)` on each
/// open descriptor in `fds`.
fn mark_close_on_exec(fds: std::ops::Range<RawFd>) {
    for fd in fds {
        // SAFETY: fcntl(2) on a descriptor number; a closed one is EBADF.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
            // SAFETY: as above, on a descriptor that is open.
            unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        }
    }
}

// --------------------------------------------------------- process groups

fn groups() -> &'static Mutex<BTreeSet<u32>> {
    static GROUPS: OnceLock<Mutex<BTreeSet<u32>>> = OnceLock::new();
    GROUPS.get_or_init(Mutex::default)
}

fn register_group(pid: u32) {
    if let Ok(mut set) = groups().lock() {
        set.insert(pid);
    }
}

fn unregister_group(pid: u32) {
    if let Ok(mut set) = groups().lock() {
        set.remove(&pid);
    }
}

fn signal_group(pgid: u32, sig: Signal) -> io::Result<()> {
    let pid = i32::try_from(pgid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    rustix::process::kill_process_group(pid, sig).map_err(io::Error::from)
}

/// The process groups of every child that hasn't been reaped yet.
pub fn live_groups() -> Vec<u32> {
    groups()
        .lock()
        .map(|set| set.iter().copied().collect())
        .unwrap_or_default()
}

/// Sends SIGTERM (`kill = false`) or SIGKILL to every live child's process
/// group (a host's second interrupt, A§8.6). Returns how many were signaled.
pub fn signal_all(kill: bool) -> usize {
    signal_groups(live_groups(), kill)
}

/// [`signal_all`] limited to `pgids` that are still live children.
fn signal_groups(pgids: impl IntoIterator<Item = u32>, kill: bool) -> usize {
    let sig = if kill { Signal::KILL } else { Signal::TERM };
    let live = live_groups();
    pgids
        .into_iter()
        .filter(|pgid| live.contains(pgid))
        .filter(|pgid| signal_group(*pgid, sig).is_ok())
        .count()
}

// -------------------------------------------------------------------- logs

/// `YYYYMMDD-HHMMSS` in local time, the prefix of log names.
pub fn log_stamp(now: DateTime<Local>) -> String {
    now.format("%Y%m%d-%H%M%S").to_string()
}

/// Keeps `[A-Za-z0-9._-]`, so a step name can't escape the logs dir.
pub fn sanitize_step(step: &str) -> String {
    let clean: String = step
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    match clean.trim_matches('.') {
        "" => "step".to_owned(),
        _ => clean,
    }
}

/// Creates `dir/<name>.log` with `O_EXCL` (never reusing a log), adding
/// `-2`, `-3`, … to the name while it exists. Creates `dir` if needed.
pub fn create_log(dir: &Path, name: &str) -> io::Result<(File, PathBuf)> {
    fs::create_dir_all(dir)?;
    let name = sanitize_step(name);
    for n in 1..=1000u32 {
        let file_name = if n == 1 {
            format!("{name}.log")
        } else {
            format!("{name}-{n}.log")
        };
        let path = dir.join(file_name);
        match OpenOptions::new().append(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("too many logs named {name} in {}", dir.display()),
    ))
}

/// `.qrec/logs/YYYYMMDD-HHMMSS-<pid>[-j<job>]-<step>.log`, created with `O_EXCL`.
pub fn job_log(
    paths: &ProjectPaths,
    job: Option<JobId>,
    step: &str,
) -> io::Result<(File, PathBuf)> {
    let stamp = log_stamp(Local::now());
    let pid = std::process::id();
    let step = sanitize_step(step);
    let name = match job {
        Some(job) => format!("{stamp}-{pid}-j{}-{step}", job.0),
        None => format!("{stamp}-{pid}-{step}"),
    };
    create_log(&paths.logs_dir(), &name)
}

/// The last `n` lines of a log (lossy UTF-8; reads at most the last 64 KiB).
pub fn tail_lines(path: &Path, n: usize) -> Vec<String> {
    const MAX: u64 = 64 * 1024;
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(MAX);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // probably cut in the middle
    }
    let skip = lines.len().saturating_sub(n);
    lines[skip..].iter().map(|l| (*l).to_owned()).collect()
}

/// Keeps the newest `keep` timestamp-named logs in `dir` (names start with
/// their timestamp, so name order is age order) and deletes the rest.
/// Other files are left alone and don't count, the GUI's rolling
/// `preview.log` and `preview.1.log` included ([`is_stamped_log_name`],
/// docs/GUI.md G§3.11). Returns how many were deleted. A missing dir is
/// fine.
pub fn prune_logs(dir: &Path, keep: usize) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut logs: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            is_stamped_log_name(&name).then(|| (name, e.path()))
        })
        .collect();
    if logs.len() <= keep {
        return 0;
    }
    logs.sort();
    let excess = logs.len() - keep;
    logs.into_iter()
        .take(excess)
        .filter(|(_, path)| fs::remove_file(path).is_ok())
        .count()
}

/// Deletes temp outputs older than `max_age` in `exports/`, `frames/`,
/// `.qrec/cache/segments/` and the silence cache (regular files only).
/// Returns how many were deleted.
///
/// A temp output is a name of exactly the shape the media runner gives
/// them, `.<stem>.qrec-part-<pid>-<job>[.<ext>]` ([`is_temp_name`]), never
/// any name that contains `.qrec-part-`: exports and frames (also of a
/// project whose name contains it) and results kept aside
/// (`<stem>-kept.<ext>`, A§8.8) are never deleted.
pub fn prune_temp_outputs(paths: &ProjectPaths, max_age: Duration) -> usize {
    let now = SystemTime::now();
    [
        paths.exports_dir(),
        paths.frames_dir(),
        paths.segments_dir(),
        paths.silence_cache_dir(),
    ]
    .iter()
    .filter_map(|dir| fs::read_dir(dir).ok())
    .flatten()
    .filter_map(Result::ok)
    .filter(|e| is_temp_name(&e.file_name()))
    .filter(|e| {
        e.path().symlink_metadata().is_ok_and(|m| {
            m.file_type().is_file()
                && m.modified()
                    .ok()
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|age| age > max_age)
        })
    })
    .filter(|e| fs::remove_file(e.path()).is_ok())
    .count()
}

/// Quotes `arg` for a POSIX shell when it needs it (log readability only).
pub fn shell_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
    if plain {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for c in arg.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Describes a list of finished runs for a log line (`"a finished; b exited with status 1"`).
pub fn summary(runs: &[Finished]) -> String {
    let mut out = String::new();
    for (i, run) in runs.iter().enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        let _ = write!(out, "{}", run.describe());
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// A descriptor this process holds without close-on-exec, the way a
    /// shell hands `exec 9>file` down to the command it starts: at 200 or
    /// above, so a shell started by the test doesn't open one with that
    /// number itself.
    pub(crate) fn inheritable_fd() -> OwnedFd {
        let file = tempfile::tempfile().unwrap();
        // SAFETY: F_DUPFD (which doesn't set close-on-exec) on a
        // descriptor we own.
        let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 200) };
        assert!(fd >= 200, "{}", io::Error::last_os_error());
        // SAFETY: a fresh descriptor we own.
        unsafe { OwnedFd::from_raw_fd(fd) }
    }

    /// A `sh -c` script that prints whether the shell holds `fd`.
    pub(crate) fn holds_fd_script(fd: &OwnedFd) -> String {
        format!(
            "if test -e /proc/$$/fd/{0}; then echo held; else echo free; fi",
            fd.as_raw_fd()
        )
    }

    /// A child gets its stdio and nothing else: not a descriptor Qrec
    /// inherited from whoever started it (a key-bind script's flock, a
    /// harness's pipe), which a child outliving the command would keep.
    #[test]
    fn children_get_no_inherited_descriptor() {
        let fd = inheritable_fd();
        let script = holds_fd_script(&fd);
        // A plain spawn hands it down: the check sees leaks.
        let plain = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&plain.stdout), "held\n");
        for die_with_parent in [true, false] {
            let out = sh(&script, Limit::Timeout(Duration::from_secs(10)))
                .die_with_parent(die_with_parent)
                .capture_stdout(1024)
                .run()
                .unwrap();
            assert!(out.success(), "{}", out.describe());
            assert_eq!(
                out.stdout_text(),
                "free\n",
                "die_with_parent {die_with_parent}"
            );
        }
        // Still open here: only the child's copy was closed at exec.
        // SAFETY: fcntl query on our descriptor.
        assert!(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } >= 0);
    }

    /// The sweep a kernel without `close_range(CLOSE_RANGE_CLOEXEC)` gets:
    /// open descriptors in the range become close-on-exec, others are left
    /// alone. (Only this test's own descriptor: other tests running
    /// meanwhile hold theirs.)
    #[test]
    fn the_fallback_sweep_marks_open_descriptors() {
        let fd = inheritable_fd();
        let raw = fd.as_raw_fd();
        // SAFETY: fcntl queries on our descriptor.
        let flags = || unsafe { libc::fcntl(raw, libc::F_GETFD) };
        assert_eq!(flags() & libc::FD_CLOEXEC, 0);
        mark_close_on_exec(raw + 1..raw + 1);
        assert_eq!(flags() & libc::FD_CLOEXEC, 0, "outside the range");
        mark_close_on_exec(raw..raw + 1);
        assert_ne!(flags() & libc::FD_CLOEXEC, 0);
        let script = holds_fd_script(&fd);
        let plain = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&plain.stdout), "free\n");
    }

    fn alive(pid: u32) -> bool {
        // Signal 0 checks existence; a zombie still exists until reaped.
        let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
            return false;
        };
        rustix::process::test_kill_process(pid).is_ok()
    }

    fn sh(script: &str, limit: Limit) -> Spec {
        Spec::new("sh", limit).args(["-c", script])
    }

    #[test]
    fn captures_stdout_and_logs_stderr() {
        let tmp = tempfile::tempdir().unwrap();
        let (log, path) = create_log(tmp.path(), "t").unwrap();
        let out = sh(
            "echo out; echo err >&2; exit 3",
            Limit::Timeout(Duration::from_secs(10)),
        )
        .capture_stdout(1 << 20)
        .stderr_to(log)
        .run()
        .unwrap();
        assert_eq!(out.end, End::Exited);
        assert_eq!(out.status.and_then(|s| s.code()), Some(3));
        assert!(!out.success());
        assert_eq!(out.stdout_text(), "out\n");
        assert_eq!(out.describe(), "sh exited with status 3");
        let log = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert!(
            lines[0].starts_with("# ")
                && lines[0].ends_with("$ sh -c 'echo out; echo err >&2; exit 3'"),
            "{log}"
        );
        assert_eq!(lines[1], "err");
        assert!(lines[2].starts_with("# sh exited with status 3"), "{log}");
    }

    #[test]
    fn stdout_capture_is_capped_but_drained() {
        let out = sh(
            "head -c 100000 /dev/zero",
            Limit::Timeout(Duration::from_secs(10)),
        )
        .capture_stdout(10)
        .run()
        .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout.len(), 10);
        assert!(out.stdout_truncated);
    }

    #[test]
    fn lines_reach_the_handler_in_order() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let out = sh(
            "printf 'a\\nb\\r\\nc'",
            Limit::Timeout(Duration::from_secs(10)),
        )
        .stdout_lines(move |line| sink.lock().unwrap().push(line.to_owned()))
        .run()
        .unwrap();
        assert!(out.success());
        assert_eq!(*seen.lock().unwrap(), ["a", "b", "c"]);
    }

    #[test]
    fn a_timeout_kills_a_sleeping_child_and_its_group() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("grandchild");
        let script = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        let started = Instant::now();
        let out = sh(&script, Limit::Timeout(Duration::from_millis(300)))
            .run()
            .unwrap();
        assert_eq!(out.end, End::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert!(
            out.describe().starts_with("sh timed out after"),
            "{}",
            out.describe()
        );
        // SIGTERM went to the whole group: the background sleep is gone too.
        let grandchild: u32 = fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive(grandchild) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(grandchild), "the grandchild survived");
        assert!(!alive(out.pid));
        assert!(!live_groups().contains(&out.pid));
    }

    #[test]
    fn sigterm_is_followed_by_sigkill() {
        let started = Instant::now();
        // `exec sleep` would reset the trap; a shell loop keeps ignoring TERM.
        let out = sh(
            "trap '' TERM; while :; do sleep 0.05; done",
            Limit::Timeout(Duration::from_millis(200)),
        )
        .run()
        .unwrap();
        let took = started.elapsed();
        assert_eq!(out.end, End::TimedOut);
        assert!(
            took >= TERM_GRACE,
            "SIGKILL came before the grace period: {took:?}"
        );
        assert!(took < TERM_GRACE + Duration::from_secs(2), "{took:?}");
        assert_eq!(out.status.and_then(|s| s.signal()), Some(libc::SIGKILL));
    }

    #[test]
    fn the_stall_watchdog_kills_a_silent_child() {
        let started = Instant::now();
        let progress = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&progress);
        let out = sh(
            "echo progress=continue; echo noise; sleep 0.1; echo progress=continue; sleep 30",
            Limit::Stall(Duration::from_millis(400)),
        )
        .activity_marker("progress=")
        .stdout_lines(move |line| {
            if line.starts_with("progress=") {
                count.fetch_add(1, Ordering::Relaxed);
            }
        })
        .run()
        .unwrap();
        assert_eq!(out.end, End::Stalled);
        assert_eq!(progress.load(Ordering::Relaxed), 2);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(out.describe(), "sh stalled (no progress)");
    }

    #[test]
    fn a_canceller_stops_a_child_from_another_thread() {
        let running = Spec::new("sleep", Limit::Timeout(Duration::from_secs(30)))
            .arg("30")
            .start()
            .unwrap();
        let canceller = running.canceller();
        assert_eq!(canceller.pid(), running.pid());
        let waiter = thread::spawn(move || running.wait());
        thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        canceller.cancel("user");
        let out = waiter.join().unwrap();
        assert_eq!(out.end, End::Cancelled("user".into()));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(out.describe(), "sleep was cancelled (user)");
        // Cancelling again is harmless.
        canceller.cancel("again");
    }

    #[test]
    fn a_grandchild_holding_stdout_doesnt_hang_the_caller() {
        let started = Instant::now();
        let out = sh(
            "sleep 30 & echo done",
            Limit::Timeout(Duration::from_secs(20)),
        )
        .capture_stdout(1024)
        .run()
        .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout_text(), "done\n");
        assert!(started.elapsed() < STDOUT_DRAIN + Duration::from_secs(2));
    }

    #[test]
    fn missing_programs_are_missing_dependencies() {
        let err = Spec::new("qrec-no-such-tool", Limit::Timeout(Duration::from_secs(1)))
            .run()
            .unwrap_err();
        assert!(matches!(err, ProcError::NotFound { .. }), "{err:?}");
        assert_eq!(err.to_core().code, ErrorCode::MissingDependency);
        assert!(matches!(
            Spec::from_argv(&[], Limit::Timeout(Duration::from_secs(1))),
            Err(ProcError::EmptyArgv)
        ));
    }

    #[test]
    fn stdin_env_and_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("in.txt");
        fs::write(&input, "from a file\n").unwrap();
        let out = sh(
            "cat; echo \"$QREC_PROC_TEST\"; echo \"${HOME:-unset}\"; pwd",
            Limit::Timeout(Duration::from_secs(10)),
        )
        .stdin_file(&input)
        .env("QREC_PROC_TEST", "yes")
        .env_remove("HOME")
        .current_dir(tmp.path())
        .capture_stdout(4096)
        .run()
        .unwrap();
        let canonical = fs::canonicalize(tmp.path()).unwrap();
        assert_eq!(
            out.stdout_text(),
            format!("from a file\nyes\nunset\n{}\n", canonical.display())
        );
        // Without a file, stdin is /dev/null (cat ends at once).
        let out = sh("cat", Limit::Timeout(Duration::from_secs(10)))
            .capture_stdout(16)
            .run()
            .unwrap();
        assert!(out.success() && out.stdout.is_empty());
    }

    #[test]
    fn signal_all_reaches_running_children() {
        let running = Spec::new("sleep", Limit::Timeout(Duration::from_secs(30)))
            .arg("30")
            .start()
            .unwrap();
        let pid = running.pid();
        assert!(live_groups().contains(&pid));
        // Only this test's child: other tests run in parallel in this process.
        assert_eq!(signal_groups([pid, 0], true), 1);
        let out = running.wait();
        assert_eq!(out.status.and_then(|s| s.signal()), Some(libc::SIGKILL));
    }

    #[test]
    fn logs_are_never_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, a) = create_log(tmp.path(), "20261014-120000-1-j7-segment 3").unwrap();
        let (_, b) = create_log(tmp.path(), "20261014-120000-1-j7-segment 3").unwrap();
        assert_eq!(a.file_name().unwrap(), "20261014-120000-1-j7-segment_3.log");
        assert_eq!(
            b.file_name().unwrap(),
            "20261014-120000-1-j7-segment_3-2.log"
        );

        let paths = ProjectPaths::new(tmp.path());
        let (_, log) = job_log(&paths, Some(JobId(7)), "stitch").unwrap();
        let name = log.file_name().unwrap().to_string_lossy().into_owned();
        assert!(log.starts_with(paths.logs_dir()));
        assert_eq!(
            &name[15..],
            format!("-{}-j7-stitch.log", std::process::id()),
            "{name}"
        );
        let stamp = &name.as_bytes()[..15];
        assert!(
            stamp[8] == b'-'
                && stamp
                    .iter()
                    .enumerate()
                    .all(|(i, b)| i == 8 || b.is_ascii_digit()),
            "{name}"
        );
        let (_, log) = job_log(&paths, None, "pactl").unwrap();
        let name = log.to_string_lossy().into_owned();
        assert!(
            name.ends_with(&format!("-{}-pactl.log", std::process::id())),
            "{name}"
        );
        assert_eq!(sanitize_step("../x"), ".._x");
        assert_eq!(sanitize_step(".."), "step");
    }

    #[test]
    fn tail_returns_the_last_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("x.log");
        let text: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        fs::write(&path, text).unwrap();
        assert_eq!(tail_lines(&path, 2), ["line 49", "line 50"]);
        assert_eq!(tail_lines(&path, 100).len(), 50);
        assert!(tail_lines(&tmp.path().join("missing"), 3).is_empty());
    }

    /// The prune at host start counts only timestamp-named logs: the GUI's
    /// rolling `preview.log` and `preview.1.log` stay and don't count
    /// (docs/GUI.md G§3.11), so 250 job logs leave the newest 200 and both.
    #[test]
    fn pruning_leaves_the_preview_logs_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let logs = ProjectPaths::new(tmp.path()).logs_dir();
        fs::create_dir_all(&logs).unwrap();
        for i in 0..250 {
            let name = format!("20261015-{:06}-77-j{i}-seg0.log", 100_000 + i);
            fs::write(logs.join(name), "").unwrap();
        }
        fs::write(logs.join("preview.log"), "[1 strip] ok").unwrap();
        fs::write(logs.join("preview.1.log"), "[1 strip] older").unwrap();
        assert_eq!(prune_logs(&logs, KEEP_LOGS), 50);
        let mut left: Vec<String> = fs::read_dir(&logs)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left.len(), 202);
        assert_eq!(left[0], "20261015-100050-77-j50-seg0.log");
        assert_eq!(&left[200..], ["preview.1.log", "preview.log"]);
    }

    #[test]
    fn pruning_keeps_the_newest_logs_and_old_temp_outputs_go() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::new(tmp.path());
        let logs = paths.logs_dir();
        fs::create_dir_all(&logs).unwrap();
        for i in 0..5 {
            fs::write(logs.join(format!("2026101{i}-120000-1-x.log")), "").unwrap();
        }
        fs::write(logs.join("notes.txt"), "").unwrap();
        assert_eq!(prune_logs(&logs, 3), 2);
        let mut left: Vec<String> = fs::read_dir(&logs)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "20261012-120000-1-x.log",
                "20261013-120000-1-x.log",
                "20261014-120000-1-x.log",
                "notes.txt"
            ]
        );
        assert_eq!(prune_logs(&tmp.path().join("nope"), 3), 0);

        fs::create_dir_all(paths.exports_dir()).unwrap();
        fs::create_dir_all(paths.frames_dir()).unwrap();
        fs::create_dir_all(paths.segments_dir()).unwrap();
        let old = paths.exports_dir().join(".v.qrec-part-1-2.mp4");
        let old_frame = paths.frames_dir().join(".f.qrec-part-1-4.png");
        let fresh = paths.segments_dir().join(".k.qrec-part-1-3.mov");
        // Results, however old: an export, a result kept aside (A§8.8) and
        // the outputs of a project called `talk.qrec-part-final`.
        let keep = [
            paths.exports_dir().join("v-20261014-120000.mp4"),
            paths.exports_dir().join("final-kept.webm"),
            paths
                .exports_dir()
                .join("talk.qrec-part-final-20261014-120000.mp4"),
            paths
                .frames_dir()
                .join("talk.qrec-part-final-take-0001-00000003.png"),
            paths.frames_dir().join(".mine.qrec-part-1-2-kept.png"),
        ];
        let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
        for f in keep.iter().chain([&old, &old_frame, &fresh]) {
            fs::write(f, "x").unwrap();
            if f != &fresh {
                File::options()
                    .write(true)
                    .open(f)
                    .unwrap()
                    .set_modified(two_hours_ago)
                    .unwrap();
            }
        }
        assert_eq!(prune_temp_outputs(&paths, TEMP_MAX_AGE), 2);
        assert!(!old.exists() && !old_frame.exists() && fresh.exists());
        for f in &keep {
            assert!(f.exists(), "{} was deleted", f.display());
        }
    }

    #[test]
    fn quoting_and_summaries() {
        assert_eq!(shell_quote("-hide_banner"), "-hide_banner");
        assert_eq!(shell_quote("file:takes/a b.mkv"), "'file:takes/a b.mkv'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
        let spec = Spec::new("ffmpeg", Limit::Timeout(Duration::from_secs(1))).args(["-i", "a b"]);
        assert_eq!(spec.command_line(), "ffmpeg -i 'a b'");
        let ok = sh("true", Limit::Timeout(Duration::from_secs(5)))
            .run()
            .unwrap();
        let bad = sh("exit 2", Limit::Timeout(Duration::from_secs(5)))
            .label("x")
            .run()
            .unwrap();
        assert_eq!(summary(&[ok, bad]), "sh finished; x exited with status 2");
    }
}
