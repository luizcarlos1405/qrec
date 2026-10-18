//! Starting a child process without forking Qrec (A§8.7). The runners
//! ([`crate::proc`] and `media::child`) start every ffmpeg, ffprobe,
//! pactl and wl-copy through [`Command::spawn`]: `clone(CLONE_VM |
//! CLONE_VFORK)`, the way `posix_spawn(3)` starts a child, with the
//! runners' own steps in the child before `execve` (Rust's `Command` only
//! spawns that way when nothing runs in the child: with a `pre_exec` it
//! forks). The child gets what the runners' `pre_exec` gave it:
//!
//! - its own process group (`setpgid(0, 0)`), and stdio as given;
//! - nothing past its stdio: every other descriptor is close-on-exec
//!   ([`crate::proc::close_on_exec_from`]), even one Qrec inherited, but the
//!   one it is told to keep ([`Command::keep_open`]: an output's temp file,
//!   A§8.8);
//! - SIGKILL when the thread that spawned it dies (`PR_SET_PDEATHSIG`), and
//!   no start at all when the spawning process is gone already;
//! - default signal handling and an empty signal mask, like `Command`'s.
//!
//! Why not `fork`: it copies the page tables of the whole process (a
//! window's GPU mappings, its heaps), and glibc holds every malloc arena's
//! lock meanwhile, so every other thread that allocates waits for it. In
//! the window under gles2 a fork took 25–48 ms and its main thread waited
//! out each one: the 70–190 ms main-loop stalls of WP10's smoke at a take's
//! landing (an ffprobe per track) and at an export's start (an ffmpeg per
//! segment), G§8.6.
//!
//! Between `clone` and `execve` the child shares the parent's memory (the
//! spawning thread is suspended meanwhile; the others run on), so it only
//! makes async-signal-safe system calls on what [`Command::spawn`] prepared
//! beforehand: it allocates nothing, takes no lock and never unwinds.

use std::ffi::{CString, OsStr, OsString, c_char, c_int, c_void};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicI32, Ordering};
use std::{iter, mem, ptr};

/// Where `execvp(3)` looks when there is no `PATH` (glibc's `_CS_PATH`).
const DEFAULT_PATH: &str = "/bin:/usr/bin";

/// The child's stack until it execs: a few system calls deep.
const STACK: usize = 64 * 1024;

/// Linux's signals are 1–64.
const SIGNALS: c_int = 65;

/// Where one of the child's stdio streams goes.
#[derive(Debug)]
pub(crate) enum Stdio {
    /// `/dev/null`.
    Null,
    /// A pipe whose other end the parent gets ([`Child::stdout`],
    /// [`Child::stderr`]). For stdin there is no such end (the runners
    /// give stdin a file or `/dev/null`): the child reads its end of file.
    Piped,
    /// This descriptor (a file, a log): the child gets a copy of it.
    Fd(OwnedFd),
}

impl From<File> for Stdio {
    fn from(file: File) -> Self {
        Stdio::Fd(file.into())
    }
}

/// A child to start: the subset of `std::process::Command` the runners use,
/// with their child-side steps built in (the module's list).
#[derive(Debug)]
pub(crate) struct Command {
    program: OsString,
    args: Vec<OsString>,
    /// Changes to the inherited environment, in order (`None`: removed).
    env: Vec<(OsString, Option<OsString>)>,
    cwd: Option<PathBuf>,
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio,
    keep_open: Option<RawFd>,
    die_with_parent: bool,
}

impl Command {
    /// `program` (looked up in the child's `PATH` unless it has a `/`),
    /// stdio all `/dev/null`, dying with the spawning thread.
    pub(crate) fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            stdin: Stdio::Null,
            stdout: Stdio::Null,
            stderr: Stdio::Null,
            keep_open: None,
            die_with_parent: true,
        }
    }

    pub(crate) fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_owned()));
        self
    }

    pub(crate) fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.env
            .push((key.as_ref().to_owned(), Some(value.as_ref().to_owned())));
        self
    }

    pub(crate) fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.env.push((key.as_ref().to_owned(), None));
        self
    }

    pub(crate) fn current_dir(&mut self, dir: impl Into<PathBuf>) -> &mut Self {
        self.cwd = Some(dir.into());
        self
    }

    pub(crate) fn stdin(&mut self, stdin: impl Into<Stdio>) -> &mut Self {
        self.stdin = stdin.into();
        self
    }

    pub(crate) fn stdout(&mut self, stdout: impl Into<Stdio>) -> &mut Self {
        self.stdout = stdout.into();
        self
    }

    pub(crate) fn stderr(&mut self, stderr: impl Into<Stdio>) -> &mut Self {
        self.stderr = stderr.into();
        self
    }

    /// The child keeps `fd` (above 2) open across `exec`, under the same
    /// number: the clone copies the descriptor table, so `/proc/self/fd/<fd>`
    /// names the same file in the child.
    pub(crate) fn keep_open(&mut self, fd: RawFd) -> &mut Self {
        self.keep_open = Some(fd);
        self
    }

    /// Whether the child gets SIGKILL when the spawning thread dies
    /// (default true).
    pub(crate) fn die_with_parent(&mut self, yes: bool) -> &mut Self {
        self.die_with_parent = yes;
        self
    }

    /// Starts the child. `NotFound` when no `PATH` entry has the program,
    /// `PermissionDenied` when the ones found can't be run, the child's
    /// own error when a step before `exec` failed.
    pub(crate) fn spawn(self) -> io::Result<Child> {
        let env = environment(&self.env);
        let path = env
            .iter()
            .find(|(key, _)| key.as_bytes() == b"PATH")
            .map(|(_, value)| value.as_os_str());
        let candidates = candidates(&self.program, path)?;
        let args = iter::once(&self.program)
            .chain(&self.args)
            .map(|arg| c_string(arg))
            .collect::<io::Result<Vec<_>>>()?;
        let vars = env
            .iter()
            .map(|(key, value)| {
                let mut var = key.clone();
                var.push("=");
                var.push(value);
                c_string(&var)
            })
            .collect::<io::Result<Vec<_>>>()?;
        let cwd = self
            .cwd
            .as_ref()
            .map(|dir| c_string(dir.as_os_str()))
            .transpose()?;
        if let Some(fd) = self.keep_open
            && fd < 3
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("can't keep descriptor {fd} open: it is a stdio number"),
            ));
        }
        let (stdin, _) = ends(self.stdin, true)?;
        let (stdout, stdout_parent) = ends(self.stdout, false)?;
        let (stderr, stderr_parent) = ends(self.stderr, false)?;
        let plan = Plan {
            candidates: candidates.iter().map(|c| c.as_ptr()).collect(),
            argv: pointers(&args),
            envp: pointers(&vars),
            cwd: cwd.as_ref().map(|dir| dir.as_ptr()),
            stdio: [stdin.as_raw_fd(), stdout.as_raw_fd(), stderr.as_raw_fd()],
            keep_open: self.keep_open,
            // SAFETY: getpid(2) has no preconditions.
            parent: self.die_with_parent.then(|| unsafe { libc::getpid() }),
            error: AtomicI32::new(0),
        };
        let pid = clone_child(&plan)?;
        // The child has its own copies by now: these were for it.
        drop((stdin, stdout, stderr));
        let error = plan.error.load(Ordering::Acquire);
        if error != 0 {
            // It exited without exec'ing: reap it.
            let _ = wait_pid(pid, 0);
            return Err(io::Error::from_raw_os_error(error));
        }
        Ok(Child {
            pid,
            stdout: stdout_parent,
            stderr: stderr_parent,
            status: None,
        })
    }
}

/// A started child; nothing happens to it when this is dropped (like
/// `std::process::Child`): whoever spawned it waits for it.
#[derive(Debug)]
pub(crate) struct Child {
    pid: u32,
    /// The read end of a piped stdout.
    pub stdout: Option<File>,
    /// The read end of a piped stderr.
    pub stderr: Option<File>,
    status: Option<ExitStatus>,
}

impl Child {
    /// Its pid, and its process group's id.
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    /// Waits for it to exit (once reaped, its status again).
    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let status =
            wait_pid(self.pid, 0)?.ok_or_else(|| io::Error::other("waitpid returned 0"))?;
        self.status = Some(status);
        Ok(status)
    }

    /// Its status if it exited, without waiting.
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        let status = wait_pid(self.pid, libc::WNOHANG)?;
        self.status = status;
        Ok(status)
    }
}

/// `waitpid(pid, flags)`: `None` when `WNOHANG` found it still running.
fn wait_pid(pid: u32, flags: c_int) -> io::Result<Option<ExitStatus>> {
    let pid =
        libc::pid_t::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    loop {
        let mut raw = 0;
        // SAFETY: waitpid(2) on our own child, into a local.
        let rc = unsafe { libc::waitpid(pid, &mut raw, flags) };
        if rc == pid {
            return Ok(Some(ExitStatus::from_raw(raw)));
        }
        if rc == 0 {
            return Ok(None);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// The inherited environment with `changes` applied in order.
fn environment(changes: &[(OsString, Option<OsString>)]) -> Vec<(OsString, OsString)> {
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (key, value) in changes {
        vars.retain(|(k, _)| k != key);
        if let Some(value) = value {
            vars.push((key.clone(), value.clone()));
        }
    }
    vars
}

/// The paths `execve` tries, in order: `program` itself when it names a
/// path, else each `PATH` directory's (an empty entry: the current one), as
/// `execvp(3)` looks.
fn candidates(program: &OsStr, path: Option<&OsStr>) -> io::Result<Vec<CString>> {
    if program.is_empty() {
        return Err(io::Error::from_raw_os_error(libc::ENOENT));
    }
    if program.as_bytes().contains(&b'/') {
        return Ok(vec![c_string(program)?]);
    }
    let path = path.unwrap_or(OsStr::new(DEFAULT_PATH));
    path.as_bytes()
        .split(|&b| b == b':')
        .map(|dir| {
            let mut full = dir.to_vec();
            if !full.is_empty() {
                full.push(b'/');
            }
            full.extend_from_slice(program.as_bytes());
            c_string(OsStr::from_bytes(&full))
        })
        .collect()
}

fn c_string(text: &OsStr) -> io::Result<CString> {
    CString::new(text.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a NUL byte in {text:?}"),
        )
    })
}

/// A null-terminated array of `strings`' pointers (`argv`, `envp`).
fn pointers(strings: &[CString]) -> Vec<*const c_char> {
    strings
        .iter()
        .map(|s| s.as_ptr())
        .chain(iter::once(ptr::null()))
        .collect()
}

/// The child's descriptor for a stdio stream (above 2, so putting one in
/// place never covers another) and the parent's end of its pipe.
fn ends(stdio: Stdio, read: bool) -> io::Result<(OwnedFd, Option<File>)> {
    let (child, parent) = match stdio {
        Stdio::Null => {
            let null = File::options().read(read).write(!read).open("/dev/null")?;
            (OwnedFd::from(null), None)
        }
        Stdio::Piped => {
            let (r, w) = pipe()?;
            if read {
                (r, Some(File::from(w)))
            } else {
                (w, Some(File::from(r)))
            }
        }
        Stdio::Fd(fd) => (fd, None),
    };
    Ok((above_stdio(child)?, parent))
}

/// `fd`, or a close-on-exec copy of it above 2 when it is a stdio number.
fn above_stdio(fd: OwnedFd) -> io::Result<OwnedFd> {
    if fd.as_raw_fd() > 2 {
        return Ok(fd);
    }
    // SAFETY: fcntl(2) on a descriptor we own.
    let high = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if high < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_DUPFD_CLOEXEC returned a new descriptor nobody else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(high) })
}

/// A close-on-exec pipe: (read end, write end).
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds: [c_int; 2] = [0; 2];
    // SAFETY: pipe2(2) writes two descriptors into the array.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both are new descriptors nobody else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

// ------------------------------------------------------------ the child

/// Everything the child uses, prepared before `clone`: it reads it and
/// writes `error`, nothing else.
struct Plan {
    /// The paths to try, in order.
    candidates: Vec<*const c_char>,
    argv: Vec<*const c_char>,
    envp: Vec<*const c_char>,
    cwd: Option<*const c_char>,
    /// The descriptors to put at 0, 1 and 2.
    stdio: [RawFd; 3],
    keep_open: Option<RawFd>,
    /// The spawning process, when the child dies with its thread.
    parent: Option<libc::pid_t>,
    /// The child's errno when it couldn't exec (0: it did).
    error: AtomicI32,
}

/// The child's stack (`mmap`ed; unmapped once the child exec'd).
struct Stack {
    base: *mut c_void,
}

impl Stack {
    fn new() -> io::Result<Stack> {
        // SAFETY: an anonymous private mapping; nothing else refers to it.
        let base = unsafe {
            libc::mmap(
                ptr::null_mut(),
                STACK,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_STACK,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Stack { base })
    }

    /// Its top (stacks grow down on every Linux target Qrec builds for).
    fn top(&self) -> *mut c_void {
        self.base.wrapping_byte_add(STACK)
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // SAFETY: the mapping `new` made; the child no longer runs on it
        // (CLONE_VFORK: clone returned once it exec'd or exited).
        unsafe { libc::munmap(self.base, STACK) };
    }
}

/// `clone(CLONE_VM | CLONE_VFORK)` into [`child`]: returns once the child
/// exec'd or exited.
fn clone_child(plan: &Plan) -> io::Result<u32> {
    let stack = Stack::new()?;
    // Every signal stays blocked on this thread from before the clone until
    // the child has reset its handlers: none can run a handler of Qrec's on
    // the child's stack (glibc's posix_spawn does the same).
    // SAFETY: sigset_t is plain data; sigfillset fills it in.
    let mut all: libc::sigset_t = unsafe { mem::zeroed() };
    // SAFETY: as above.
    let mut old: libc::sigset_t = unsafe { mem::zeroed() };
    // SAFETY: sigfillset(3) and pthread_sigmask(3) on locals.
    unsafe {
        libc::sigfillset(&mut all);
        libc::pthread_sigmask(libc::SIG_SETMASK, &all, &mut old);
    }
    // SAFETY: `child` only reads `plan` (alive until clone returns: this
    // thread is suspended until the child exec'd or exited) and runs on its
    // own stack; see `child_steps` for what it calls.
    let pid = unsafe {
        libc::clone(
            child,
            stack.top(),
            libc::CLONE_VM | libc::CLONE_VFORK | libc::SIGCHLD,
            ptr::from_ref(plan).cast_mut().cast(),
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: restores this thread's own mask.
    unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &old, ptr::null_mut()) };
    drop(stack);
    if pid < 0 {
        return Err(error);
    }
    u32::try_from(pid).map_err(|_| io::Error::other(format!("clone returned {pid}")))
}

/// The child, between `clone` and `execve`: when a step fails it leaves its
/// errno in the plan and exits with 127, which the parent turns into an
/// error.
extern "C" fn child(plan: *mut c_void) -> c_int {
    // SAFETY: the `Plan` `clone_child` passed: alive and unchanged until
    // this child exec'd or exited (the spawning thread is suspended).
    let plan = unsafe { &*plan.cast::<Plan>() };
    // SAFETY: async-signal-safe system calls on the plan's data only.
    let errno = unsafe { child_steps(plan) };
    plan.error.store(
        if errno == 0 { libc::EINVAL } else { errno },
        Ordering::Release,
    );
    // SAFETY: _exit(2) ends this child at once, running nothing of the
    // parent's (no atexit handlers, no stdio flush).
    unsafe { libc::_exit(127) }
}

/// The child's steps, in order; returns why it couldn't exec. Only system
/// calls, on data prepared before the clone: no allocation, no lock, no
/// panic (this runs on the parent's memory).
unsafe fn child_steps(plan: &Plan) -> c_int {
    // Default handling for every signal Qrec handles (a handler would run
    // on the parent's memory) and for SIGPIPE (Rust ignores it), then
    // nothing blocked: what Rust's `Command` gives a child.
    for sig in 1..SIGNALS {
        // SAFETY: sigaction is plain data.
        let mut old: libc::sigaction = unsafe { mem::zeroed() };
        // SAFETY: sigaction(2) reading this signal's disposition.
        if unsafe { libc::sigaction(sig, ptr::null(), &mut old) } != 0 {
            continue;
        }
        let handled = old.sa_sigaction != libc::SIG_DFL && old.sa_sigaction != libc::SIG_IGN;
        if handled || (sig == libc::SIGPIPE && old.sa_sigaction == libc::SIG_IGN) {
            // SAFETY: as above; a zeroed sigaction is SIG_DFL, no flags.
            let dfl: libc::sigaction = unsafe { mem::zeroed() };
            // SAFETY: sigaction(2) setting the default.
            unsafe { libc::sigaction(sig, &dfl, ptr::null_mut()) };
        }
    }
    // SAFETY: sigset_t is plain data.
    let mut none: libc::sigset_t = unsafe { mem::zeroed() };
    // SAFETY: sigemptyset(3) and sigprocmask(2) on a local.
    if unsafe {
        libc::sigemptyset(&mut none);
        libc::sigprocmask(libc::SIG_SETMASK, &none, ptr::null_mut())
    } != 0
    {
        return errno();
    }
    // Its own process group: a kill reaches everything it starts.
    // SAFETY: setpgid(2) on itself.
    if unsafe { libc::setpgid(0, 0) } != 0 {
        return errno();
    }
    for (target, &fd) in [0, 1, 2].into_iter().zip(plan.stdio.iter()) {
        // dup2 clears close-on-exec on the copy.
        // SAFETY: dup2(2) on descriptors the parent opened for the child.
        if unsafe { libc::dup2(fd, target) } < 0 {
            return errno();
        }
    }
    // Only its stdio crosses (A§8.7)…
    crate::proc::close_on_exec_from(3);
    // …and the one descriptor it keeps (A§8.8).
    if let Some(fd) = plan.keep_open
        && keep_open(fd) < 0
    {
        return errno();
    }
    if let Some(parent) = plan.parent {
        // It dies with the thread that spawned it: a SIGKILLed host leaves
        // no orphaned ffmpeg behind.
        // SAFETY: prctl(2) with integer arguments.
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) } != 0 {
            return errno();
        }
        // The parent died before the prctl: don't run orphaned.
        // SAFETY: getppid(2) has no preconditions.
        if unsafe { libc::getppid() } != parent {
            return libc::ESRCH;
        }
    }
    if let Some(dir) = plan.cwd {
        // SAFETY: chdir(2) on a NUL-terminated path the plan keeps alive.
        if unsafe { libc::chdir(dir) } != 0 {
            return errno();
        }
    }
    let mut denied = false;
    for &path in &plan.candidates {
        // SAFETY: execve(2) with NUL-terminated strings and null-terminated
        // arrays the plan keeps alive; it only returns on failure.
        unsafe { libc::execve(path, plan.argv.as_ptr(), plan.envp.as_ptr()) };
        match errno() {
            libc::EACCES => denied = true,
            libc::ENOENT | libc::ENOTDIR | libc::ESTALE | libc::ENODEV | libc::ETIMEDOUT => {}
            e => return e,
        }
    }
    if denied { libc::EACCES } else { libc::ENOENT }
}

/// Clears `fd`'s close-on-exec flag (async-signal-safe): < 0 on failure.
fn keep_open(fd: RawFd) -> c_int {
    // SAFETY: fcntl(2) on a descriptor number.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return flags;
    }
    // SAFETY: as above.
    unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) }
}

/// This thread's errno (the child's, before it exec'd).
fn errno() -> c_int {
    // SAFETY: __errno_location(3) returns this thread's errno slot.
    unsafe { *libc::__errno_location() }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    use super::*;

    /// `sh -c script`, its stdout piped.
    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]).stdout(Stdio::Piped);
        cmd
    }

    /// Runs it to the end: (status, stdout).
    fn output(cmd: Command) -> (ExitStatus, String) {
        let mut child = cmd.spawn().expect("spawn");
        let mut out = String::new();
        if let Some(mut pipe) = child.stdout.take() {
            pipe.read_to_string(&mut out).expect("read stdout");
        }
        (child.wait().expect("wait"), out)
    }

    /// Waits up to `within` for `child` to exit.
    fn wait_within(child: &mut Child, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// SIGKILL to `pid` (a test's leftover).
    fn kill(pid: u32) {
        // SAFETY: kill(2) on our own child's pid.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }

    #[test]
    fn a_child_gets_its_arguments_environment_and_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = sh("printf '%s|%s|%s|%s' \"$0\" \"$1\" \"$QREC_SPAWN_TEST\" \"$(pwd -P)\"");
        cmd.args(["zero", "one two"])
            .env("QREC_SPAWN_TEST", "yes")
            .env_remove("HOME")
            .current_dir(dir.path());
        let (status, out) = output(cmd);
        assert!(status.success(), "{status}");
        let dir = dir.path().canonicalize().unwrap();
        assert_eq!(out, format!("zero|one two|yes|{}", dir.display()));
        let (_, home) = output({
            let mut cmd = sh("printf '%s' \"${HOME-unset}\"");
            cmd.env_remove("HOME");
            cmd
        });
        assert_eq!(home, "unset");
    }

    #[test]
    fn its_exit_status_and_signal_come_back() {
        let (status, _) = output(sh("exit 3"));
        assert_eq!(status.code(), Some(3));
        let (status, _) = output(sh("kill -TERM $$"));
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        // A reaped child's status stays.
        let mut child = sh("exit 0").spawn().unwrap();
        assert!(child.wait().unwrap().success());
        assert!(child.wait().unwrap().success());
        assert!(child.try_wait().unwrap().is_some_and(|s| s.success()));
    }

    #[test]
    fn a_missing_or_unrunnable_program_is_an_error_at_once() {
        let missing = Command::new("qrec-no-such-program-anywhere").spawn();
        assert_eq!(
            missing.map(|_| ()).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("not-executable");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
        let denied = Command::new(&script).spawn();
        assert_eq!(
            denied.map(|_| ()).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let mut nowhere = Command::new("sh");
        nowhere.current_dir(dir.path().join("gone"));
        assert_eq!(
            nowhere.spawn().map(|_| ()).unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "a missing folder fails the start"
        );
        // Found in the child's PATH, not the parent's.
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let tool = bin.join("qrec-spawn-test-tool");
        std::fs::write(&tool, "#!/bin/sh\necho found\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut found = Command::new("qrec-spawn-test-tool");
        let path = format!(
            "{}:/bin:/usr/bin:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        found.env("PATH", path).stdout(Stdio::Piped);
        let (status, out) = output(found);
        assert!(status.success());
        assert_eq!(out, "found\n");
    }

    /// The child leads its own process group (a kill reaches everything it
    /// starts).
    #[test]
    fn a_child_leads_its_own_process_group() {
        let mut child = sh("sleep 5").spawn().unwrap();
        let pid = child.id();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let after = stat.rsplit_once(") ").map(|(_, rest)| rest).unwrap();
        let pgrp: u32 = after.split(' ').nth(2).unwrap().parse().unwrap();
        assert_eq!(pgrp, pid, "{stat}");
        kill(pid);
        assert!(wait_within(&mut child, Duration::from_secs(5)).is_some());
    }

    /// Only its stdio crosses: a descriptor the parent left inheritable
    /// (what a caller of Qrec's may have) doesn't, unless it is the one to
    /// keep, which keeps its number.
    #[test]
    fn only_stdio_and_the_kept_descriptor_cross() {
        let file = tempfile::tempfile().unwrap();
        // SAFETY: F_DUPFD (no close-on-exec) on a descriptor we own.
        let leaky = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 300) };
        assert!(leaky >= 300);
        // SAFETY: the new descriptor is ours.
        let leaky = unsafe { OwnedFd::from_raw_fd(leaky) };
        let kept = tempfile::tempfile().unwrap();
        let probe = |fd: RawFd| {
            format!("if [ -e /proc/self/fd/{fd} ]; then echo open; else echo closed; fi")
        };
        let (_, out) = output(sh(&probe(leaky.as_raw_fd())));
        assert_eq!(out, "closed\n", "an inheritable descriptor stays home");
        let mut cmd = sh(&format!(
            "{}; echo written >&{}",
            probe(kept.as_raw_fd()),
            kept.as_raw_fd()
        ));
        cmd.keep_open(kept.as_raw_fd());
        let (status, out) = output(cmd);
        assert!(status.success());
        assert_eq!(out, "open\n");
        let mut kept = kept;
        let mut written = String::new();
        std::io::Seek::rewind(&mut kept).unwrap();
        kept.read_to_string(&mut written).unwrap();
        assert_eq!(written, "written\n", "the very file, under the same number");
        let mut stdio = sh("true");
        stdio.keep_open(2);
        assert_eq!(
            stdio.spawn().map(|_| ()).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        drop(leaky);
    }

    /// stderr goes where it is told (a log file here), stdin reads a file.
    #[test]
    fn stdio_goes_where_it_is_told() {
        let mut input = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut input, b"from stdin").unwrap();
        std::io::Seek::rewind(&mut input).unwrap();
        let log = tempfile::tempfile().unwrap();
        let mut cmd = sh("cat; echo to-stderr >&2");
        cmd.stdin(input).stderr(log.try_clone().unwrap());
        let (status, out) = output(cmd);
        assert!(status.success());
        assert_eq!(out, "from stdin");
        let mut log = log;
        let mut text = String::new();
        std::io::Seek::rewind(&mut log).unwrap();
        log.read_to_string(&mut text).unwrap();
        assert_eq!(text, "to-stderr\n");
        let mut piped = sh("echo oops >&2");
        piped.stdout(Stdio::Null).stderr(Stdio::Piped);
        let mut child = piped.spawn().unwrap();
        assert!(child.stdout.is_none());
        let mut err = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        assert_eq!(err, "oops\n");
        assert!(child.wait().unwrap().success());
    }

    /// The child starts with no signal blocked and SIGPIPE at its default,
    /// whatever the spawning thread blocks (Rust ignores SIGPIPE).
    #[test]
    fn the_child_starts_with_default_signals() {
        // SAFETY: sigset_t is plain data; pthread_sigmask on this thread.
        let mut usr1: libc::sigset_t = unsafe { mem::zeroed() };
        let mut old: libc::sigset_t = unsafe { mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut usr1);
            libc::sigaddset(&mut usr1, libc::SIGUSR1);
            libc::pthread_sigmask(libc::SIG_BLOCK, &usr1, &mut old);
        }
        let mut grep = Command::new("grep");
        grep.args(["-E", "^(SigBlk|SigIgn):", "/proc/self/status"])
            .stdout(Stdio::Piped);
        let (_, out) = output(grep);
        // SAFETY: restores this thread's mask.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &old, ptr::null_mut()) };
        let field = |name: &str| {
            let line = out.lines().find(|l| l.starts_with(name)).unwrap();
            u64::from_str_radix(line.split_whitespace().nth(1).unwrap(), 16).unwrap()
        };
        assert_eq!(field("SigBlk:"), 0, "{out}");
        let pipe_bit = 1u64 << (libc::SIGPIPE - 1);
        assert_eq!(
            field("SigIgn:") & pipe_bit,
            0,
            "SIGPIPE isn't ignored: {out}"
        );
    }

    /// The child gets SIGKILL when the thread that spawned it ends (a
    /// SIGKILLed host leaves no orphan), unless told otherwise.
    #[test]
    fn a_child_dies_with_the_thread_that_spawned_it() {
        let spawn = |die: bool| {
            std::thread::spawn(move || {
                let mut cmd = sh("sleep 30");
                cmd.stdout(Stdio::Null).die_with_parent(die);
                cmd.spawn().unwrap()
            })
            .join()
            .unwrap()
        };
        let mut dying = spawn(true);
        let status = wait_within(&mut dying, Duration::from_secs(10)).expect("killed");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let mut living = spawn(false);
        std::thread::sleep(Duration::from_millis(200));
        assert!(living.try_wait().unwrap().is_none(), "it lives on");
        kill(living.id());
        assert!(wait_within(&mut living, Duration::from_secs(5)).is_some());
    }

    /// G§8.6: the process runner starts its children without `fork`
    /// (which took 25–48 ms in the window, every allocating thread waiting
    /// on glibc's malloc locks meanwhile); `Command` with a `pre_exec`, as
    /// the runners used to, does fork. (The media runner's own test:
    /// `media::child`'s.)
    #[test]
    fn the_process_runner_starts_children_without_fork() {
        // The counter sees a fork: `Command` with a `pre_exec`, on a thread
        // of that name.
        let control = "spawn_control_without_fork";
        let forked = fork_counter::forks_of(control, || {
            let mut forking = std::process::Command::new("true");
            // SAFETY: an empty closure.
            unsafe {
                std::os::unix::process::CommandExt::pre_exec(&mut forking, || Ok(()));
            }
            assert!(forking.status().unwrap().success());
        });
        assert_eq!(forked, 1, "the counter works");
        // The runner's owner thread is named after the label
        // (`proc-<label>`).
        let label = "proc_runner_without_fork";
        let forked = fork_counter::forks_of(label, move || {
            let ran = crate::proc::Spec::new(
                "true",
                crate::proc::Limit::Timeout(Duration::from_secs(20)),
            )
            .label(label)
            .run()
            .expect("the process runner ran true");
            assert!(ran.success(), "{}", ran.describe());
        });
        assert_eq!(forked, 0, "the process runner forked");
    }
}

/// Counts forks by thread name: a `pthread_atfork` handler, which glibc
/// runs in every `fork()` and never in a `clone(CLONE_VM | CLONE_VFORK)`.
#[cfg(test)]
pub(crate) mod fork_counter {
    use std::sync::{Mutex, Once};

    /// (a thread name's ending, the forks of threads whose name ends so).
    static WATCHED: Mutex<Vec<(&'static str, usize)>> = Mutex::new(Vec::new());

    extern "C" fn count() {
        let Some(name) = std::thread::current().name().map(str::to_owned) else {
            return;
        };
        if let Ok(mut watched) = WATCHED.lock() {
            for (ending, forks) in watched.iter_mut() {
                if name.ends_with(*ending) {
                    *forks += 1;
                }
            }
        }
    }

    /// How many forks threads whose name ends with `ending` made while `f`
    /// ran on a thread named `ending` (a test's own name: concurrent tests
    /// don't count for each other). A panic in `f` fails the caller.
    pub(crate) fn forks_of(ending: &'static str, f: impl FnOnce() + Send + 'static) -> usize {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            // SAFETY: registers a handler that reads the thread's name and
            // bumps a counter under a lock no fork path holds.
            assert_eq!(unsafe { libc::pthread_atfork(Some(count), None, None) }, 0);
        });
        {
            let mut watched = WATCHED.lock().unwrap();
            match watched.iter_mut().find(|(e, _)| *e == ending) {
                Some((_, forks)) => *forks = 0,
                None => watched.push((ending, 0)),
            }
        }
        let ran = std::thread::Builder::new()
            .name(ending.to_owned())
            .spawn(f)
            .unwrap()
            .join();
        assert!(ran.is_ok(), "{ending}: see the panic above");
        let watched = WATCHED.lock().unwrap();
        watched
            .iter()
            .find(|(e, _)| *e == ending)
            .map_or(0, |(_, forks)| *forks)
    }
}
