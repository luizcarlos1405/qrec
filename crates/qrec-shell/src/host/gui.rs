//! The GUI's host (docs/GUI.md G§3.4.1, A§8.3, A§8.6). Owner: ipc.
//!
//! GTK-free: the window talks to it through a [`GuiPort`] (implemented by
//! `qrec-gui`; a fake in tests) and drives it with a [`GuiHost`]:
//!
//! - [`GuiHost::start`] (startup step 4, on a worker thread, under the lock
//!   the caller took): the folder rule again, the lock record says
//!   `starting`, the `first` request (D-35: `Init` in an empty folder) is
//!   queued ahead of everything, the socket is published and the record
//!   says `ready`. From then on CLI requests (a niri `qrec record toggle`
//!   while the window opens) queue in this host and signals are handled,
//!   also before the Session is open. A thread `qrec-session` waits for
//!   what comes next.
//! - [`GuiHost::open`] (step 7): that thread opens the Session with a share
//!   of the lock, attaches it (the queued requests follow `first`, in
//!   order), posts the first snapshot and `Opened`, serves until the exit
//!   rules say stop, finishes (socket gone, replies flushed, lock released)
//!   and posts `Stopped`. A failed open answers the queue with its error,
//!   finishes and posts `OpenFailed`; a panic while opening or serving
//!   answers the queue too and posts `Crashed`.
//! - [`GuiHost::fail`] (GStreamer stuck, a missing dependency, before
//!   `open`): every queued and later request gets the error, the socket
//!   goes and the lock is released at once (the CLI then works in-process
//!   while the window shows the problem); `Failed`, then `Stopped`.
//! - [`GuiHost::close`]: the window closed (after it sent what ends a
//!   recording or an export, D-12). The close takes the window's FIFO, so
//!   everything it submitted before runs first; then the host stops
//!   accepting and exits once quiet (autocut analyses aside: the close drops
//!   them, D-12), or after the close grace (60 s) whatever still runs, a
//!   recovery probe included.
//! - [`GuiHost::submit`]: the window's own requests, in one FIFO with the
//!   CLI's (parked until the Session is attached). Each gets exactly one
//!   answer: one the Session never handled (the host stopped first) hears
//!   why from [`AnswerOnDrop`].
//!
//! A GUI host never idles out and refuses the wire `shutdown` (D-13).
//! Signals work as in every host (A§8.6): the first finalizes a recording
//! and cancels jobs, and the host exits once quiet or 15 s later (before the
//! Session is open: at once); the second hard-exits (130).
//!
//! Events, in order: `Ready` (from `start`); then `Opened`, `OpenFailed`,
//! `Failed` or `Crashed`; always `Stopped` last, once the lock is released
//! ([`GuiHost::is_finished`] already holds then).

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, Sender};
use qrec_core::protocol::{Progress, Query, Toast, ToastLevel};
use qrec_core::state::HostKind;
use qrec_core::{CoreError, ErrorCode, State};
use serde_json::Value;

use super::signals::Interrupts;
use super::{ExitRules, Host, HostCtl, HostEnd, HostOptions, HostSession, Opener};
use crate::deps::OpenOpts;
use crate::ipc::resolve::{Folder, classify};
use crate::ipc::server::ServerSender;
use crate::lock::ProjectLock;
use crate::msg::{Msg, Request, Responder};
use crate::paths::{Env, ProjectPaths, ensure_runtime_dir, socket_path};
use crate::session::{Notifier, StateObserver};

/// What the window hears from its host. Implemented by `qrec-gui`: every
/// call is O(1) and never blocks (it queues for the GTK main loop, G§3.5),
/// since the session thread makes most of them.
pub trait GuiPort: Send + Sync + 'static {
    /// A state the Session holds now (H-6): after every assignment
    /// ([`PortObserver`]) and once when the Session opened.
    fn snapshot(&self, rev: u64, state: Arc<State>);
    /// A toast of the core (`Effect::Notify`, [`PortNotifier`]); `recovery`
    /// for the startup recovery's own news. Also an error of the `first`
    /// request.
    fn toast(&self, toast: Toast, recovery: bool);
    /// The host's own life ([`HostEvent`]).
    fn host(&self, event: HostEvent);
}

/// The host's life, in this order: `Ready`; then one of `Opened`,
/// `OpenFailed`, `Failed` or `Crashed`; `Stopped` always last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// Socket published, requests queue (from [`GuiHost::start`]).
    Ready,
    /// The Session is attached; its first snapshot came just before.
    Opened,
    /// The Session couldn't open (X3: a broken `qrec.json` and its restore
    /// hint): the queued requests got this error and the lock is released.
    OpenFailed(CoreError),
    /// [`GuiHost::fail`] ran: requests got this error, the lock is released.
    Failed(CoreError),
    /// The host stopped and released the lock; nothing follows.
    Stopped { end: HostEnd },
    /// The session thread panicked (the message); requests got an
    /// `internal` error and the lock is released.
    Crashed(String),
}

/// How a GUI host starts (G§3.4.1).
#[derive(Debug)]
pub struct GuiHostSpec {
    /// The canonical project folder: a project, a project that lost its
    /// `qrec.json`, or an empty folder.
    pub dir: PathBuf,
    /// The project lock, taken by the caller (startup step 4 or the
    /// takeover, G§3.4.3).
    pub lock: ProjectLock,
    /// For the runtime dir (the socket).
    pub env: Env,
    /// The process's signal counter (`signals::install(&HOST_SIGNALS)`);
    /// every interrupt it ever counted is a signal for this host. `None`:
    /// signals aren't watched (tests).
    pub interrupts: Option<Interrupts>,
    pub rules: ExitRules,
    /// D-35: run ahead of every request the socket brings (`Init` with the
    /// user's defaults in an empty folder). An error the Session gives it
    /// while the host serves is toasted ([`FirstResponder`]).
    pub first: Option<Request>,
}

/// A running GUI host (G§3.4.1). Dropping it closes it ([`close`](Self::close)).
pub struct GuiHost {
    sender: ServerSender,
    ctl: Arc<HostCtl>,
    control: Sender<Control>,
    opened: AtomicBool,
    /// [`close`](Self::close) sent its [`CloseMark`].
    closing: AtomicBool,
    finished: Arc<AtomicBool>,
    /// The `qrec-session` thread; never joined (the GTK thread never waits),
    /// `Stopped` says it is done.
    _worker: JoinHandle<()>,
}

/// What the `qrec-session` thread waits for before it serves.
enum Control {
    Open(Box<dyn Opener + Send>),
    Fail(CoreError),
    Close,
    Signal,
}

impl std::fmt::Debug for GuiHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuiHost")
            .field("opened", &self.opened.load(Ordering::SeqCst))
            .field("finished", &self.is_finished())
            .finish_non_exhaustive()
    }
}

/// What requests still queued get once the window closed.
const CLOSING: &str = "the Qrec window is closing";

impl GuiHost {
    /// A§8.3 step 1 for the GUI under `spec.lock`: the folder rule again,
    /// record `starting`, `first` queued, socket published, signals
    /// watched, record `ready`; requests queue until [`open`](Self::open).
    /// Posts `Ready`. Errors (the folder is no project any more, the socket
    /// can't be published) leave nothing behind: the lock is released.
    pub fn start(spec: GuiHostSpec, port: Arc<dyn GuiPort>) -> Result<GuiHost, CoreError> {
        let GuiHostSpec {
            dir,
            lock,
            env,
            interrupts,
            rules,
            first,
        } = spec;
        // Another process may have filled an empty folder meanwhile (A§8.4
        // init order: the rule again, under the lock).
        match classify(&dir, true) {
            Ok(Folder::Project | Folder::MissingFile | Folder::Empty) => {}
            Ok(Folder::Other) => return Err(CoreError::not_project()),
            Err(e) => {
                return Err(CoreError::io(format!("can't read {}: {e}", dir.display())));
            }
        }
        let runtime = ensure_runtime_dir(&env)?;
        let socket = socket_path(&runtime, &dir)?;
        let stopping = Arc::new(AtomicBool::new(false));
        let attached = Arc::new(AtomicBool::new(false));
        let first_host = Arc::new(OnceLock::new());
        let first: Vec<Msg> = first
            .map(|req| Msg::Request {
                req,
                to: Box::new(FirstResponder {
                    port: Arc::clone(&port),
                    stopping: Arc::clone(&stopping),
                    attached: Arc::clone(&attached),
                    host: Arc::clone(&first_host),
                }),
            })
            .into_iter()
            .collect();
        let opts = HostOptions {
            rules,
            interrupts,
            // A signal that slipped in after the startup workers last looked
            // still counts.
            signal_baseline: 0,
            // The second signal: unlink the socket, kill child groups,
            // `_exit(130)` (A§8.6).
            on_second_signal: None,
        };
        let host = Host::start_with(
            HostKind::Gui,
            ProjectPaths::new(&dir),
            lock,
            &socket,
            opts,
            first,
        )?;
        // Before anything can attach a Session: `first` asks it whether a
        // signal came.
        let _ = first_host.set(Arc::clone(&host.ctl));
        let (control, controls) = crossbeam_channel::unbounded();
        let on_signal = control.clone();
        let _ = host.ctl.on_signal.set(Box::new(move || {
            let _ = on_signal.send(Control::Signal);
        }));
        let sender = host.server.sender();
        let ctl = Arc::clone(&host.ctl);
        let finished = Arc::new(AtomicBool::new(false));
        tracing::info!("serving {} (ready)", dir.display());
        let worker = {
            let finished = Arc::clone(&finished);
            let flags = Flags { stopping, attached };
            thread::Builder::new()
                .name("qrec-session".into())
                .spawn(move || {
                    // From this thread, so nothing it posts can come first.
                    port.host(HostEvent::Ready);
                    let end = run(host, &dir, &controls, &*port, &flags);
                    finished.store(true, Ordering::SeqCst);
                    tracing::info!("the GUI host stopped ({end:?})");
                    port.host(HostEvent::Stopped { end });
                })
                .map_err(|e| CoreError::io(format!("can't start the session thread: {e}")))?
        };
        Ok(GuiHost {
            sender,
            ctl,
            control,
            opened: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            finished,
            _worker: worker,
        })
    }

    /// Opens the Session on the `qrec-session` thread (startup step 7):
    /// `Opened` or `OpenFailed` follows. Only the first call counts.
    pub fn open(&self, opener: Box<dyn Opener + Send>) {
        if self.opened.swap(true, Ordering::SeqCst) {
            tracing::warn!("the GUI host was asked to open its Session twice; ignored");
            return;
        }
        if self.control.send(Control::Open(opener)).is_err() {
            tracing::warn!("the GUI host stopped before its Session could open");
        }
    }

    /// The window can't go on (GStreamer stuck, a missing dependency):
    /// every queued and later request gets `err`, the host stops and
    /// releases the lock at once; `Failed`, then `Stopped`. Only before
    /// [`open`](Self::open) (both need startup steps that exclude each
    /// other); afterwards it is logged and ignored.
    pub fn fail(&self, err: CoreError) {
        if self.opened.load(Ordering::SeqCst) {
            tracing::warn!("the GUI host can't fail after its Session opened ({err}); ignored");
            return;
        }
        let _ = self.control.send(Control::Fail(err));
    }

    /// One request of the window, in the same FIFO as the CLI's (parked
    /// until the Session is attached). `to` hears exactly one answer: the
    /// Session's, or why the host stopped before the Session handled it
    /// ([`AnswerOnDrop`]). `false` when it can't be delivered: `to` was
    /// answered with why (the host stopped).
    pub fn submit(&self, req: Request, to: Box<dyn Responder>) -> bool {
        let to = Box::new(AnswerOnDrop {
            to,
            server: self.sender.clone(),
            answered: false,
        });
        self.sender.send(Msg::Request { req, to })
    }

    /// The live preview's "Try again" (D-50): `Msg::RetryLive`, ordered with
    /// the Session's other messages.
    pub fn retry_live(&self) {
        self.sender.send(Msg::RetryLive);
    }

    /// The window closed: after the Session handled everything the window
    /// submitted before (the close takes the same FIFO, [`CloseMark`]),
    /// stop accepting and exit once quiet, or after the close grace (60 s)
    /// whatever still runs; then `Stopped { Closed }`. Before the Session
    /// is open it stops at once. Never blocks.
    pub fn close(&self) {
        if !self.closing.swap(true, Ordering::SeqCst) {
            self.sender.send(Msg::Request {
                req: Request::query(Query::Status),
                to: Box::new(CloseMark(Arc::clone(&self.ctl))),
            });
        }
        let _ = self.control.send(Control::Close);
    }

    /// The host stopped and released the lock (`Stopped` follows or came).
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}

impl Drop for GuiHost {
    fn drop(&mut self) {
        self.close();
    }
}

/// What the `first` request's responder learns about its host.
struct Flags {
    /// The host is stopping: its own event says why.
    stopping: Arc<AtomicBool>,
    /// A Session is attached. Every answer before that comes from the host
    /// failing or ending (the server failed or was dropped).
    attached: Arc<AtomicBool>,
}

/// The `qrec-session` thread: wait for open, fail, close or a signal; run
/// it; return how the host ended (the lock is released by then).
fn run(
    host: Host,
    dir: &Path,
    controls: &Receiver<Control>,
    port: &dyn GuiPort,
    flags: &Flags,
) -> HostEnd {
    // A signal before the hook was set doesn't come as a message.
    let next = if host.signalled() {
        Control::Signal
    } else {
        controls.recv().unwrap_or(Control::Close)
    };
    let stopping = &flags.stopping;
    match next {
        Control::Open(opener) => serve(host, dir, opener, port, flags),
        Control::Fail(err) => {
            stopping.store(true, Ordering::SeqCst);
            tracing::warn!("the GUI host failed: {err}");
            host.server.fail(err.clone());
            host.finish(None, err.clone());
            port.host(HostEvent::Failed(err));
            HostEnd::Failed
        }
        Control::Close => {
            stopping.store(true, Ordering::SeqCst);
            host.finish(None, CoreError::io(CLOSING));
            HostEnd::Closed
        }
        Control::Signal => {
            stopping.store(true, Ordering::SeqCst);
            host.finish(None, CoreError::busy("this Qrec host is stopping"));
            HostEnd::Signal
        }
    }
}

/// Opens the Session, attaches it, serves it, finishes.
fn serve(
    host: Host,
    dir: &Path,
    mut opener: Box<dyn Opener + Send>,
    port: &dyn GuiPort,
    flags: &Flags,
) -> HostEnd {
    let stopping = &*flags.stopping;
    let opts = OpenOpts {
        host: HostKind::Gui,
        mutating: true,
        recovery: false,
    };
    let opened = catch_unwind(AssertUnwindSafe(
        || -> Result<Box<dyn HostSession>, CoreError> {
            let lock = host.session_lock()?;
            opener.open(dir, opts, Some(lock)).map_err(|e| e.to_core())
        },
    ));
    drop(opener);
    let mut session = match opened {
        Ok(Ok(session)) => session,
        Ok(Err(err)) => {
            stopping.store(true, Ordering::SeqCst);
            tracing::warn!("can't open the project: {err}");
            host.server.fail(err.clone());
            host.finish(None, err.clone());
            port.host(HostEvent::OpenFailed(err));
            return HostEnd::Failed;
        }
        Err(panic) => return crashed(host, None, &*panic, port, stopping),
    };
    flags.attached.store(true, Ordering::SeqCst);
    host.attach(session.inbox());
    port.snapshot(session.rev(), Arc::new(session.state().clone()));
    port.host(HostEvent::Opened);
    let served = catch_unwind(AssertUnwindSafe(|| host.serve(&mut *session, None)));
    match served {
        Ok(end) => {
            stopping.store(true, Ordering::SeqCst);
            tracing::info!("exiting ({end:?})");
            host.finish(Some(session), CoreError::io(CLOSING));
            end
        }
        Err(panic) => crashed(host, Some(session), &*panic, port, stopping),
    }
}

/// A panic on the session thread: requests get an `internal` error, the
/// Session is dropped (carefully: its state may be half changed), the lock
/// released; `Crashed`.
fn crashed(
    host: Host,
    session: Option<Box<dyn HostSession>>,
    panic: &(dyn Any + Send),
    port: &dyn GuiPort,
    stopping: &AtomicBool,
) -> HostEnd {
    stopping.store(true, Ordering::SeqCst);
    let message = panic_message(panic);
    tracing::error!("the Qrec session crashed: {message}");
    let err = CoreError::internal(format!("the Qrec session crashed: {message}"));
    host.server.fail(err.clone());
    if let Some(session) = session
        && catch_unwind(AssertUnwindSafe(move || drop(session))).is_err()
    {
        tracing::error!("closing the crashed Session panicked too");
    }
    host.finish(None, err);
    port.host(HostEvent::Crashed(message));
    HostEnd::Failed
}

/// The text of a panic payload.
fn panic_message(panic: &(dyn Any + Send)) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic without a message".to_owned()
    }
}

/// The core's answer to `Init` in a folder that is already a project: the
/// CLI got there first, which is fine.
fn already_a_project(err: &CoreError) -> bool {
    err.code == ErrorCode::Invalid && err.message == "already a project"
}

/// Where the `first` request's answers go: the log, and a toast for an
/// error the Session gave while the host serves. Errors that only say the
/// host ends aren't toasted, since its own event says why: any answer
/// before a Session is attached (a start that failed after the server
/// started, a close, a failed open), any once the host is stopping, and
/// the refusal of a Session interrupted by a signal (the signal thread
/// notes the signal before it sends the `Interrupt`).
struct FirstResponder {
    port: Arc<dyn GuiPort>,
    stopping: Arc<AtomicBool>,
    attached: Arc<AtomicBool>,
    /// Set by `GuiHost::start` before a Session can be attached.
    host: Arc<OnceLock<Arc<HostCtl>>>,
}

impl FirstResponder {
    /// The host serves a Session that wasn't interrupted.
    fn serving(&self) -> bool {
        self.attached.load(Ordering::SeqCst)
            && !self.stopping.load(Ordering::SeqCst)
            && self
                .host
                .get()
                .is_none_or(|ctl| ctl.signalled_at().is_none())
    }
}

impl Responder for FirstResponder {
    fn accepted(&mut self) {}

    fn progress(&mut self, _p: &Progress) {}

    fn reply(&mut self, r: Result<Value, CoreError>) {
        match r {
            Ok(_) => tracing::info!("the window's first request ran"),
            Err(e) if already_a_project(&e) => {
                tracing::info!("the folder became a project meanwhile: {e}");
            }
            Err(e) => {
                tracing::warn!("the window's first request failed: {e}");
                if self.serving() {
                    let mut toast = Toast::new(ToastLevel::Error, e.message.clone());
                    toast.body = e.hint.clone();
                    self.port.toast(toast, false);
                }
            }
        }
    }
}

/// [`GuiHost::close`]'s place in the window's FIFO: a status query whose
/// answer, whatever it is (an error once the host stopped), asks the host
/// to close. So the close comes after every request the window submitted
/// before it, as the window sent them (D-12: what ends a recording or an
/// export, then the close).
struct CloseMark(Arc<HostCtl>);

impl Responder for CloseMark {
    fn accepted(&mut self) {}

    fn progress(&mut self, _p: &Progress) {}

    fn reply(&mut self, _r: Result<Value, CoreError>) {
        self.0.request_close();
    }
}

impl Drop for CloseMark {
    /// Dropped unanswered, the host is ending anyway; asking again is free.
    fn drop(&mut self) {
        self.0.request_close();
    }
}

/// The window's responder as its host hands it on ([`GuiHost::submit`]):
/// it forwards everything and lets one answer through. Dropped unanswered
/// — still queued, parked or waiting for a late reply when the Session
/// stopped, or a send that failed — it answers with why the host stopped
/// (the server's failure). So every request of the window hears back
/// exactly once (msg.rs), like an IPC client's (`ConnResponder`).
pub(crate) struct AnswerOnDrop {
    to: Box<dyn Responder>,
    server: ServerSender,
    answered: bool,
}

impl Responder for AnswerOnDrop {
    fn accepted(&mut self) {
        self.to.accepted();
    }

    fn progress(&mut self, p: &Progress) {
        if !self.answered {
            self.to.progress(p);
        }
    }

    fn reply(&mut self, r: Result<Value, CoreError>) {
        if !std::mem::replace(&mut self.answered, true) {
            self.to.reply(r);
        }
    }

    fn reply_late(&mut self, r: Result<Value, CoreError>) {
        if !std::mem::replace(&mut self.answered, true) {
            self.to.reply_late(r);
        }
    }
}

impl Drop for AnswerOnDrop {
    fn drop(&mut self) {
        if !std::mem::replace(&mut self.answered, true) {
            let why = self
                .server
                .failure()
                .unwrap_or_else(|| CoreError::io(UNANSWERED));
            self.to.reply(Err(why));
        }
    }
}

/// What a window's request the host dropped hears when nothing said why.
const UNANSWERED: &str = "the Qrec host stopped before it answered";

/// The Session's [`StateObserver`] of a GUI host (H-6): every state goes to
/// the port, cloned into an `Arc` (cheap at Qrec's sizes, G§3.5).
pub struct PortObserver(pub Arc<dyn GuiPort>);

impl StateObserver for PortObserver {
    fn changed(&mut self, rev: u64, state: &State) {
        self.0.snapshot(rev, Arc::new(state.clone()));
    }
}

/// The Session's [`Notifier`] of a GUI host: the core's toasts go to the
/// port, the startup recovery's marked as such.
pub struct PortNotifier(pub Arc<dyn GuiPort>);

impl Notifier for PortNotifier {
    fn notify(&mut self, toast: &Toast) {
        self.0.toast(toast.clone(), false);
    }

    fn notify_recovery(&mut self, toast: &Toast) {
        self.0.toast(toast.clone(), true);
    }
}

#[cfg(test)]
#[path = "gui_tests.rs"]
mod tests;
