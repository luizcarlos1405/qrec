//! The GTK-side model (G§3.6, G§3.7): the newest `State` snapshot, the
//! GUI-only [`LocalUi`], and the one way the window changes anything:
//! [`Store::dispatch`] (R8, SPEC §4.2).
//!
//! - Snapshots arrive through the queue (`host::queue`), each a whole
//!   `Arc<State>`; subscribers get the previous and the next state
//!   ([`Change`]) and re-render from the newest without diffing.
//! - [`Store::dispatch`] submits a Command to the GUI host, in one FIFO with
//!   the CLI's requests, and applies the default reply policy
//!   ([`replies`]); [`Store::dispatch_with`] hands the reply to the caller.
//!   A caller that gets `confirmation_required` for a flow it follows opens
//!   the modal with [`Store::ask_confirmation`]: the confirmed re-dispatch
//!   goes back to it (the export panel's own export, D-24).
//! - [`Store::notify`] shows a toast made by the GUI; the core's toasts come
//!   the same way. The toast host subscribes ([`Store::subscribe_notices`]);
//!   notices that come before it are kept for it.
//! - The echo rule ([`SentValues`], [`echo`]): a widget that sent a UI value
//!   keeps its own until a snapshot shows that value or one it didn't send.
//!
//! Main thread only (`Rc`). Only `store/`, `host/` and `startup/` touch the
//! `GuiHost` (R8).

pub mod local;
pub mod replies;

use std::cell::{Cell, Ref, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use qrec_core::keymap::Keymap;
use qrec_core::protocol::command::{Nullable, SeekTarget, UiPatch};
use qrec_core::protocol::{Progress, Toast};
use qrec_core::state::JobKind;
use qrec_core::view::notices::Notice;
use qrec_core::{Command, CoreError, LayerId, State, TrackId};
use qrec_shell::host::GuiHost;
use qrec_shell::msg::{Request, Responder};
use serde_json::Value;

pub use local::{LocalUi, Overlay};

use crate::host::GuiResponder;
use crate::host::queue::{Queue, Ticket};
use replies::{ReplyAction, default_action};

/// A snapshot replacing the previous one.
#[derive(Debug, Clone)]
pub struct Change {
    pub prev: Arc<State>,
    pub next: Arc<State>,
}

impl Change {
    /// The timeline (clips) changed.
    pub fn timeline(&self) -> bool {
        self.prev.project.timeline != self.next.project.timeline
    }

    /// The project scene changed.
    pub fn scene(&self) -> bool {
        self.prev.project.scene != self.next.project.scene
    }

    /// `ui.json`'s state changed (selection, playhead, mode, zoom, …).
    pub fn ui(&self) -> bool {
        self.prev.ui != self.next.ui
    }

    /// The record phase changed (`RecordClock` included).
    pub fn record(&self) -> bool {
        self.prev.runtime.record != self.next.runtime.record
    }

    /// Jobs started, progressed or ended.
    pub fn jobs(&self) -> bool {
        self.prev.runtime.jobs != self.next.runtime.jobs
    }

    /// The media pool (takes and their scenes) changed.
    pub fn takes(&self) -> bool {
        self.prev.project.takes != self.next.project.takes
    }

    /// The history changed (cursor or entries).
    pub fn history(&self) -> bool {
        let (prev, next) = (&self.prev.history, &self.next.history);
        prev.cursor != next.cursor
            || (!Arc::ptr_eq(&prev.entries, &next.entries) && prev.entries != next.entries)
    }
}

/// The answer to one request of the window.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub result: Result<Value, CoreError>,
    /// An event's answer (H-7): the core toasted a late error itself.
    pub late: bool,
}

/// The last value the window sent for each `UiPatch` field and for `Seek`
/// (the echo rule, G§3.6). `None`: never sent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SentValues {
    /// `Some(None)`: fit.
    pub zoom: Option<Option<f64>>,
    pub scroll_px: Option<f64>,
    pub bin_open: Option<bool>,
    pub snapping: Option<bool>,
    pub solo: Option<Option<TrackId>>,
    pub lane_mute: Option<BTreeSet<TrackId>>,
    pub selected_layer: Option<Option<LayerId>>,
    pub panels: Option<BTreeMap<String, f64>>,
    pub seek: Option<SeekTarget>,
}

impl SentValues {
    fn record(&mut self, cmd: &Command) {
        match cmd {
            Command::SetUi { patch } => self.record_patch(patch),
            Command::Seek { to } => self.seek = Some(to.clone()),
            _ => {}
        }
    }

    fn record_patch(&mut self, patch: &UiPatch) {
        fn nullable<T: Clone>(n: &Nullable<T>) -> Option<Option<T>> {
            match n {
                Nullable::Keep => None,
                Nullable::Clear => Some(None),
                Nullable::Set(v) => Some(Some(v.clone())),
            }
        }
        if let Some(zoom) = nullable(&patch.zoom) {
            self.zoom = Some(zoom);
        }
        if let Some(solo) = nullable(&patch.solo) {
            self.solo = Some(solo);
        }
        if let Some(layer) = nullable(&patch.selected_layer) {
            self.selected_layer = Some(layer);
        }
        if let Some(v) = patch.scroll_px {
            self.scroll_px = Some(v);
        }
        if let Some(v) = patch.bin_open {
            self.bin_open = Some(v);
        }
        if let Some(v) = patch.snapping {
            self.snapping = Some(v);
        }
        if let Some(v) = &patch.lane_mute {
            self.lane_mute = Some(v.clone());
        }
        if let Some(v) = &patch.panels {
            self.panels = Some(v.clone());
        }
    }
}

/// What a widget does with a snapshot's value of something it may have
/// sent (the echo rule, G§3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Echo {
    /// Keep the widget's own value: the snapshot doesn't show the sent
    /// value yet and nobody else changed it.
    Keep,
    /// Take the snapshot's value: it is the sent one, or someone else's.
    Adopt,
}

/// The echo rule for one value: `sent` is the last value the widget sent,
/// `prev` and `next` the snapshots' values.
pub fn echo<T: PartialEq>(sent: Option<&T>, prev: &T, next: &T) -> Echo {
    match sent {
        Some(sent) if next == sent => Echo::Adopt,
        _ if next != prev => Echo::Adopt,
        None => Echo::Adopt,
        Some(_) => Echo::Keep,
    }
}

// ------------------------------------------------------------ subscriptions

/// A subscriber.
type Callback<T> = Rc<dyn Fn(&T)>;

/// A change to `LocalUi` waiting for the subscribers to finish.
type LocalEdit = Box<dyn FnOnce(&mut LocalUi)>;

/// Callbacks that follow something; [`Subscription`]s remove them.
pub struct Subscribers<T: ?Sized> {
    inner: Rc<SubscriberList<T>>,
}

struct SubscriberList<T: ?Sized> {
    next: Cell<u64>,
    list: RefCell<Vec<(u64, Callback<T>)>>,
}

impl<T: ?Sized + 'static> Default for Subscribers<T> {
    fn default() -> Self {
        Self {
            inner: Rc::new(SubscriberList {
                next: Cell::new(1),
                list: RefCell::new(Vec::new()),
            }),
        }
    }
}

impl<T: ?Sized + 'static> Subscribers<T> {
    /// Adds `f`; it is called until the subscription is dropped.
    pub fn add(&self, f: impl Fn(&T) + 'static) -> Subscription {
        let id = self.inner.next.get();
        self.inner.next.set(id + 1);
        self.inner.list.borrow_mut().push((id, Rc::new(f)));
        let list = Rc::downgrade(&self.inner);
        Subscription {
            remove: Some(Box::new(move || {
                if let Some(list) = list.upgrade() {
                    list.list.borrow_mut().retain(|(i, _)| *i != id);
                }
            })),
        }
    }

    /// Calls every subscriber with `value` (those added meanwhile wait for
    /// the next call). A subscriber that panics is reported and the others
    /// still run (`guard`, G§3.12).
    pub fn emit(&self, value: &T) {
        let list: Vec<Callback<T>> = self
            .inner
            .list
            .borrow()
            .iter()
            .map(|(_, f)| Rc::clone(f))
            .collect();
        for f in list {
            crate::guard::run("a subscriber", || f(value));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.list.borrow().is_empty()
    }

    /// How many are subscribed.
    pub fn len(&self) -> usize {
        self.inner.list.borrow().len()
    }
}

/// Keeps a callback subscribed; dropping it unsubscribes.
#[must_use = "dropping a Subscription unsubscribes at once; keep it or detach() it"]
pub struct Subscription {
    remove: Option<Box<dyn FnOnce()>>,
}

impl Subscription {
    /// Stays subscribed for the life of what it follows (regions that live
    /// as long as the window).
    pub fn detach(mut self) {
        self.remove = None;
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(remove) = self.remove.take() {
            remove();
        }
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Subscription")
    }
}

// -------------------------------------------------------------------- store

/// The window's model. Cheap to clone (clones share it); main thread only.
#[derive(Clone)]
pub struct Store {
    inner: Rc<Inner>,
}

struct Inner {
    queue: Queue,
    project_dir: PathBuf,
    host: RefCell<Option<Arc<GuiHost>>>,
    state: RefCell<Arc<State>>,
    rev: Cell<u64>,
    local: RefCell<LocalUi>,
    /// `update_local` calls made while subscribers run, applied after.
    deferred: RefCell<VecDeque<LocalEdit>>,
    notifying_local: Cell<bool>,
    keymap: RefCell<Rc<Keymap>>,
    sent: RefCell<SentValues>,
    next_ticket: Cell<Ticket>,
    pending: RefCell<BTreeMap<Ticket, Pending>>,
    /// Who takes the confirmed re-dispatch of an open `Overlay::Confirm`
    /// ([`Store::ask_confirmation`]); a hook goes when its overlay closes.
    confirm_hooks: RefCell<Vec<ConfirmHook>>,
    changes: Subscribers<Change>,
    locals: Subscribers<LocalUi>,
    notices: Subscribers<Notice>,
    /// Notices made before anyone shows them (the toast host comes with
    /// the window).
    unshown: RefCell<Vec<Notice>>,
    /// Who sends what they debounced before the host closes
    /// ([`Store::subscribe_close`]).
    closing: Subscribers<()>,
    /// Test mode's `QREC_GUI_TEST_SEND_DELAY_MS`: the commands on their
    /// way to the host, oldest first.
    delayed: RefCell<VecDeque<(Arc<GuiHost>, Request, GuiResponder)>>,
}

struct Pending {
    cmd: Command,
    done: Box<dyn FnOnce(Reply)>,
    /// It reported progress: a long job runs for it (G§3.18).
    progressed: bool,
}

/// The confirmed re-dispatch of `cmd`, for the region that asked.
struct ConfirmHook {
    cmd: Command,
    confirmed: Box<dyn FnOnce(Command)>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("rev", &self.inner.rev.get())
            .field("pending", &self.inner.pending.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Store {
    /// A store of the project in `project_dir` showing `state` (the
    /// placeholder before the Session opens) that answers through `queue`.
    pub fn new(
        queue: Queue,
        project_dir: PathBuf,
        state: Arc<State>,
        keymap: Keymap,
        local: LocalUi,
    ) -> Self {
        Self {
            inner: Rc::new(Inner {
                queue,
                project_dir,
                host: RefCell::new(None),
                state: RefCell::new(state),
                rev: Cell::new(0),
                local: RefCell::new(local),
                deferred: RefCell::new(VecDeque::new()),
                notifying_local: Cell::new(false),
                keymap: RefCell::new(Rc::new(keymap)),
                sent: RefCell::new(SentValues::default()),
                next_ticket: Cell::new(1),
                pending: RefCell::new(BTreeMap::new()),
                confirm_hooks: RefCell::new(Vec::new()),
                changes: Subscribers::default(),
                locals: Subscribers::default(),
                notices: Subscribers::default(),
                unshown: RefCell::new(Vec::new()),
                closing: Subscribers::default(),
                delayed: RefCell::new(VecDeque::new()),
            }),
        }
    }

    /// The newest snapshot.
    pub fn state(&self) -> Arc<State> {
        Arc::clone(&self.inner.state.borrow())
    }

    /// The newest snapshot's revision (0 before the Session opened).
    pub fn rev(&self) -> u64 {
        self.inner.rev.get()
    }

    /// The GUI-only state.
    pub fn local(&self) -> Ref<'_, LocalUi> {
        self.inner.local.borrow()
    }

    /// Changes the GUI-only state, then tells its subscribers. A call made
    /// while they run is applied after them, in order.
    pub fn update_local(&self, f: impl FnOnce(&mut LocalUi) + 'static) {
        if self.inner.notifying_local.get() {
            self.inner.deferred.borrow_mut().push_back(Box::new(f));
            return;
        }
        let mut next: Option<LocalEdit> = Some(Box::new(f));
        while let Some(f) = next {
            let changed = {
                let mut local = self.inner.local.borrow_mut();
                let before = local.clone();
                f(&mut local);
                *local != before
            };
            if changed {
                self.inner.notifying_local.set(true);
                let local = self.inner.local.borrow();
                self.inner.locals.emit(&local);
                drop(local);
                self.inner.notifying_local.set(false);
            }
            next = self.inner.deferred.borrow_mut().pop_front();
        }
        // A confirmation's hook goes with its overlay (cancelled, `Esc`).
        let local = self.inner.local.borrow();
        self.inner.confirm_hooks.borrow_mut().retain(|hook| {
            local
                .overlays
                .iter()
                .any(|o| matches!(o, Overlay::Confirm { cmd, .. } if *cmd == hook.cmd))
        });
    }

    /// Follows snapshots.
    pub fn subscribe(&self, f: impl Fn(&Change) + 'static) -> Subscription {
        self.inner.changes.add(f)
    }

    /// Follows the GUI-only state (called after each change).
    pub fn subscribe_local(&self, f: impl Fn(&LocalUi) + 'static) -> Subscription {
        self.inner.locals.add(f)
    }

    /// Runs `f` right before the window closes its host (G§3.4.1): what a
    /// region still debounces (the timeline's `SetUi`) goes out now, ahead
    /// of the close in the host's FIFO, instead of after it, refused.
    pub fn subscribe_close(&self, f: impl Fn(&()) + 'static) -> Subscription {
        self.inner.closing.add(f)
    }

    /// The window closes its host next ([`Store::subscribe_close`]).
    pub fn before_close(&self) {
        self.inner.closing.emit(&());
    }

    /// Follows the toasts to show: the GUI's and the core's. Notices made
    /// before the first subscriber are handed to it at once.
    pub fn subscribe_notices(&self, f: impl Fn(&Notice) + 'static) -> Subscription {
        let unshown = std::mem::take(&mut *self.inner.unshown.borrow_mut());
        for notice in &unshown {
            f(notice);
        }
        self.inner.notices.add(f)
    }

    /// Sends `cmd` with the default reply policy (G§3.7).
    pub fn dispatch(&self, cmd: Command) {
        let weak = Rc::downgrade(&self.inner);
        let sent = cmd.clone();
        self.dispatch_with(cmd, false, move |reply| {
            if let Some(inner) = weak.upgrade() {
                Store { inner }.apply_default(&sent, reply);
            }
        });
    }

    /// [`Store::dispatch`] (the default reply policy), then `after` with the
    /// reply: a control that keeps showing its pick until the snapshot
    /// comes puts the project's value back when the pick is refused
    /// (Settings' Size and Background, D-43's races).
    pub fn dispatch_then(&self, cmd: Command, after: impl FnOnce(&Reply) + 'static) {
        let weak = Rc::downgrade(&self.inner);
        let sent = cmd.clone();
        self.dispatch_with(cmd, false, move |reply| {
            if let Some(inner) = weak.upgrade() {
                Store { inner }.apply_default(&sent, reply.clone());
            }
            after(&reply);
        });
    }

    /// Sends `cmd` again, confirmed (a confirmation modal's button), with
    /// the default reply policy: the core checks again. When the region
    /// that asked left a hook ([`Store::ask_confirmation`]), it gets `cmd`
    /// instead and sends it with its own reply handler.
    pub fn dispatch_confirmed(&self, cmd: Command) {
        let hook = {
            let mut hooks = self.inner.confirm_hooks.borrow_mut();
            hooks
                .iter()
                .position(|h| h.cmd == cmd)
                .map(|at| hooks.remove(at))
        };
        if let Some(hook) = hook {
            (hook.confirmed)(cmd);
            return;
        }
        let weak = Rc::downgrade(&self.inner);
        let sent = cmd.clone();
        self.dispatch_with(cmd, true, move |reply| {
            if let Some(inner) = weak.upgrade() {
                Store { inner }.apply_default(&sent, reply);
            }
        });
    }

    /// Opens `Overlay::Confirm { cmd, error }` for a command the caller
    /// sent itself and follows: confirming it in the modal hands `cmd` to
    /// `confirmed` (which sends it again with `confirmed: true` and its own
    /// reply handler) instead of the default policy. The hook goes when the
    /// overlay closes unconfirmed (D-24: the export panel's own export ends
    /// in its Done or Failed page, not only in the core's toasts).
    pub fn ask_confirmation(
        &self,
        cmd: Command,
        error: CoreError,
        confirmed: impl FnOnce(Command) + 'static,
    ) {
        self.inner.confirm_hooks.borrow_mut().push(ConfirmHook {
            cmd: cmd.clone(),
            confirmed: Box::new(confirmed),
        });
        self.update_local(move |l| l.overlays.push(Overlay::Confirm { cmd, error }));
    }

    /// The live preview's "Try again" (D-50): `Msg::RetryLive` through the
    /// host, ordered with the Session's other messages.
    pub fn retry_live(&self) {
        if let Some(host) = self.host() {
            host.retry_live();
        }
    }

    /// The canonical project folder.
    pub fn project_dir(&self) -> &Path {
        &self.inner.project_dir
    }

    /// Sends `cmd` (`confirmed`: the person confirmed it, SPEC §11) and
    /// hands its reply to `done` on the main thread. Without a host yet,
    /// `done` gets `busy`.
    pub fn dispatch_with(&self, cmd: Command, confirmed: bool, done: impl FnOnce(Reply) + 'static) {
        let ticket = self.inner.next_ticket.get();
        self.inner.next_ticket.set(ticket + 1);
        self.inner.sent.borrow_mut().record(&cmd);
        tracing::debug!("dispatch #{ticket} {}", cmd.name());
        self.inner.pending.borrow_mut().insert(
            ticket,
            Pending {
                cmd: cmd.clone(),
                done: Box::new(done),
                progressed: false,
            },
        );
        let mut responder = GuiResponder::new(ticket, self.inner.queue.clone());
        let host = self.inner.host.borrow().clone();
        match host {
            Some(host) => {
                let request = Request::command(cmd, confirmed);
                match crate::testctl::mode().send_delay {
                    Some(delay) => self.send_later(host, request, responder, delay),
                    // `false`: the host stopped and answered the responder.
                    None => {
                        host.submit(request, Box::new(responder));
                    }
                }
            }
            None => responder.reply(Err(CoreError::busy("the project isn't open yet"))),
        }
    }

    /// Test mode's `QREC_GUI_TEST_SEND_DELAY_MS` (G§8.4): `request` goes to
    /// `host` `delay` from now, after each one sent before it, as a slow
    /// Session would answer it (a reply then comes after the next gesture:
    /// the stage's commits, R23; a key's seek while playing, R22).
    fn send_later(
        &self,
        host: Arc<GuiHost>,
        request: Request,
        responder: GuiResponder,
        delay: std::time::Duration,
    ) {
        self.inner
            .delayed
            .borrow_mut()
            .push_back((host, request, responder));
        let weak = Rc::downgrade(&self.inner);
        crate::guard::timeout_local_once(delay, "store: a delayed send (test mode)", move || {
            let next = weak
                .upgrade()
                .and_then(|i| i.delayed.borrow_mut().pop_front());
            if let Some((host, request, responder)) = next {
                host.submit(request, Box::new(responder));
            }
        });
    }

    /// Shows a toast made by the GUI.
    pub fn notify(&self, notice: Notice) {
        if self.inner.notices.is_empty() {
            self.inner.unshown.borrow_mut().push(notice);
        } else {
            self.inner.notices.emit(&notice);
        }
    }

    /// The effective keymap (defaults plus `keys.toml`).
    pub fn keymap(&self) -> Rc<Keymap> {
        Rc::clone(&self.inner.keymap.borrow())
    }

    /// Replaces the keymap (`keys.toml` changed, WP10) and its problems.
    pub fn set_keymap(&self, keymap: Keymap, problems: Vec<String>) {
        *self.inner.keymap.borrow_mut() = Rc::new(keymap);
        self.update_local(move |l| l.keymap_problems = problems);
    }

    /// What the window sent last per UI value (the echo rule).
    pub fn sent(&self) -> Ref<'_, SentValues> {
        self.inner.sent.borrow()
    }

    /// Requests sent and not answered yet.
    pub fn pending_requests(&self) -> usize {
        self.inner.pending.borrow().len()
    }

    /// The unanswered requests `waitIdle` waits for (G§3.18): all but the
    /// window's own exports whose job runs (it reported progress and an
    /// export is in the job table). Their progress comes as snapshots and
    /// their answer only when they end, so they settle once started.
    pub fn unsettled_requests(&self) -> usize {
        let exporting = self
            .state()
            .runtime
            .jobs
            .values()
            .any(|job| matches!(job.kind, JobKind::Export { .. }));
        self.inner
            .pending
            .borrow()
            .values()
            .filter(|p| !(exporting && p.progressed && matches!(p.cmd, Command::Export { .. })))
            .count()
    }

    /// The queue the host answers through.
    pub fn queue(&self) -> &Queue {
        &self.inner.queue
    }

    // ------------------------------------------------- the inbound side

    /// The GUI host requests go to (startup step 4); `None` once it stopped.
    pub(crate) fn set_host(&self, host: Option<Arc<GuiHost>>) {
        *self.inner.host.borrow_mut() = host;
    }

    /// The host requests go to, if any.
    pub(crate) fn host(&self) -> Option<Arc<GuiHost>> {
        self.inner.host.borrow().clone()
    }

    /// A snapshot from the Session.
    pub(crate) fn on_snapshot(&self, rev: u64, next: Arc<State>) {
        let prev = std::mem::replace(&mut *self.inner.state.borrow_mut(), Arc::clone(&next));
        self.inner.rev.set(rev);
        self.inner.changes.emit(&Change { prev, next });
    }

    /// The answer to the window's request `ticket`.
    pub(crate) fn on_reply(&self, ticket: Ticket, result: Result<Value, CoreError>, late: bool) {
        let Some(pending) = self.inner.pending.borrow_mut().remove(&ticket) else {
            tracing::debug!("a reply to unknown request #{ticket}");
            return;
        };
        if let Err(e) = &result {
            tracing::debug!("#{ticket} {} failed: {e}", pending.cmd.name());
        }
        (pending.done)(Reply { result, late });
    }

    /// A request was accepted or reported progress (logged; progress marks
    /// it as a long job's, [`Store::unsettled_requests`]).
    pub(crate) fn on_progress(&self, ticket: Ticket, progress: Option<&Progress>) {
        match progress {
            None => tracing::trace!("#{ticket} accepted"),
            Some(p) => {
                tracing::trace!("#{ticket} progress {p:?}");
                if let Some(pending) = self.inner.pending.borrow_mut().get_mut(&ticket) {
                    pending.progressed = true;
                }
            }
        }
    }

    /// A core toast.
    pub(crate) fn on_toast(&self, toast: Toast, recovery: bool) {
        tracing::debug!(
            "toast{}: {} {}",
            if recovery { " (recovery)" } else { "" },
            toast.title,
            toast.body.as_deref().unwrap_or_default()
        );
        self.notify(Notice::from(toast));
    }

    /// The default reply policy (G§3.7) for `cmd`'s `reply`: what
    /// [`Store::dispatch`] does, for a `dispatch_with` handler that adds its
    /// own step first (a refused rename puts the name back, R46).
    pub fn apply_default(&self, cmd: &Command, reply: Reply) {
        match default_action(cmd, &reply, &self.state()) {
            ReplyAction::Nothing => {}
            ReplyAction::Confirm { cmd, error } => {
                self.update_local(move |l| l.overlays.push(Overlay::Confirm { cmd, error }));
            }
            ReplyAction::Toast(notice) => self.notify(notice),
            ReplyAction::Log(line) => tracing::warn!("{line}"),
        }
    }

    /// A weak handle, for callbacks that mustn't keep the store alive.
    pub fn downgrade(&self) -> WeakStore {
        WeakStore(Rc::downgrade(&self.inner))
    }
}

/// A [`Store`] that doesn't keep it alive.
#[derive(Clone)]
pub struct WeakStore(Weak<Inner>);

impl WeakStore {
    pub fn upgrade(&self) -> Option<Store> {
        self.0.upgrade().map(|inner| Store { inner })
    }
}

#[cfg(test)]
mod tests {
    use qrec_core::keymap;
    use qrec_core::protocol::command::SelectOp;
    use qrec_core::protocol::{ToastLevel, UiAction};
    use qrec_core::{ErrorCode, TakeId, testkit};
    use serde_json::json;

    use super::*;
    use crate::host::queue::Inbound;

    fn store() -> Store {
        Store::new(
            Queue::new(|| {}),
            PathBuf::from("/projects/rust-traits-talk"),
            Arc::new(testkit::design_state()),
            keymap::resolve(""),
            LocalUi::default(),
        )
    }

    /// The replies the store answered through its queue (no host).
    fn drain(store: &Store) {
        for item in store.queue().take_all() {
            if let Inbound::Reply {
                ticket,
                result,
                late,
            } = item
            {
                store.on_reply(ticket, result, late);
            }
        }
    }

    /// G§3.4.1: right before the host closes, each region sends what it
    /// still debounces (`before_close`), in the order they subscribed; a
    /// dropped subscription is gone, and a panicking one stops no other.
    #[test]
    fn before_close_runs_the_close_subscribers() {
        let store = store();
        let ran = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&ran);
        let _timeline = store.subscribe_close(move |_| seen.borrow_mut().push("timeline"));
        let seen = Rc::clone(&ran);
        let gone = store.subscribe_close(move |_| seen.borrow_mut().push("gone"));
        let _broken = store.subscribe_close(|_| panic!("a broken region"));
        let seen = Rc::clone(&ran);
        let _stage = store.subscribe_close(move |_| seen.borrow_mut().push("stage"));
        drop(gone);
        assert!(ran.borrow().is_empty(), "nothing before the close");
        store.before_close();
        assert_eq!(*ran.borrow(), ["timeline", "stage"]);
    }

    #[test]
    fn without_a_host_requests_are_answered_busy_in_order() {
        let store = store();
        let got = Rc::new(RefCell::new(Vec::new()));
        for n in 0..3 {
            let got = Rc::clone(&got);
            store.dispatch_with(Command::Select { op: SelectOp::All }, false, move |r| {
                got.borrow_mut().push((n, r.result.unwrap_err().code));
            });
        }
        assert_eq!(store.pending_requests(), 3);
        drain(&store);
        assert_eq!(
            *got.borrow(),
            [
                (0, ErrorCode::Busy),
                (1, ErrorCode::Busy),
                (2, ErrorCode::Busy)
            ]
        );
        assert_eq!(store.pending_requests(), 0);
    }

    #[test]
    fn the_default_policy_opens_a_confirmation_or_toasts() {
        let store = store();
        let toasts = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&toasts);
        store
            .subscribe_notices(move |n| seen.borrow_mut().push(n.title.clone()))
            .detach();
        let purge = Command::TakePurge {
            takes: vec![TakeId::numbered(2)],
        };
        store.dispatch(purge.clone());
        store.dispatch(Command::Select { op: SelectOp::All });
        let items = store.queue().take_all();
        assert_eq!(items.len(), 2);
        // Answer them as the core would.
        store.on_reply(
            1,
            Err(CoreError::new(ErrorCode::ConfirmationRequired, "sure?")),
            false,
        );
        store.on_reply(2, Err(CoreError::invalid("nothing to select")), false);
        assert_eq!(
            store.local().overlays,
            [Overlay::Confirm {
                cmd: purge,
                error: CoreError::new(ErrorCode::ConfirmationRequired, "sure?")
            }]
        );
        assert_eq!(*toasts.borrow(), ["Nothing to select"]);
    }

    /// `dispatch_then`: the default policy (here a toast), then the caller
    /// sees the reply, refused or not.
    #[test]
    fn dispatch_then_applies_the_policy_then_tells_the_caller() {
        let store = store();
        let toasts = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&toasts);
        store
            .subscribe_notices(move |n| seen.borrow_mut().push(n.title.clone()))
            .detach();
        let got = Rc::new(RefCell::new(Vec::new()));
        for _ in 0..2 {
            let (got, toasts) = (Rc::clone(&got), Rc::clone(&toasts));
            store.dispatch_then(Command::Select { op: SelectOp::All }, move |reply| {
                got.borrow_mut()
                    .push((reply.result.is_ok(), toasts.borrow().len()));
            });
        }
        store.on_reply(1, Err(CoreError::invalid("nothing to select")), false);
        store.on_reply(2, Ok(json!({})), false);
        assert_eq!(*toasts.borrow(), ["Nothing to select"]);
        assert_eq!(*got.borrow(), [(false, 1), (true, 1)], "after the toast");
        assert_eq!(store.pending_requests(), 0);
    }

    /// D-24: a confirmation a region asked for itself goes back to it once
    /// confirmed (the export panel follows its own export), not to the
    /// default policy; once its overlay closes unconfirmed the hook is gone
    /// and a confirmation of the same command takes the default path.
    #[test]
    fn a_confirmation_asked_for_goes_back_to_its_asker() {
        let store = store();
        let export = Command::Export {
            format: qrec_core::protocol::command::ExportFormat::Mp4,
            quality: qrec_core::protocol::command::Quality::Draft,
            codec: None,
            out: Some("/home/me/talk.mp4".into()),
        };
        let error = CoreError::confirmation_required("overwrite /home/me/talk.mp4");
        let got = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&got);
        store.ask_confirmation(export.clone(), error.clone(), move |cmd| {
            seen.borrow_mut().push(cmd);
        });
        let open = Overlay::Confirm {
            cmd: export.clone(),
            error: error.clone(),
        };
        assert_eq!(store.local().overlays, std::slice::from_ref(&open));
        // The modal's Confirm: the asker gets the command, nothing is sent
        // with the default policy.
        store.dispatch_confirmed(export.clone());
        assert_eq!(*got.borrow(), std::slice::from_ref(&export));
        assert_eq!(store.pending_requests(), 0);
        store.update_local(|l| l.overlays.clear());

        // Asked again, then cancelled: the hook goes with the overlay.
        let seen = Rc::clone(&got);
        store.ask_confirmation(export.clone(), error, move |cmd| {
            seen.borrow_mut().push(cmd);
        });
        store.update_local(move |l| {
            l.close_overlays(|o| *o == open);
        });
        store.dispatch_confirmed(export);
        assert_eq!(got.borrow().len(), 1, "not the cancelled asker's");
        assert_eq!(store.pending_requests(), 1, "the default path sent it");
        drain(&store);
    }

    /// `waitIdle` (G§3.18) settles the window's own export once its job
    /// runs (it reported progress and an export is in the job table): its
    /// answer comes only when it ends. Its other requests still count.
    #[test]
    fn an_own_export_settles_once_its_job_runs() {
        let store = store();
        let export = Command::Export {
            format: qrec_core::protocol::command::ExportFormat::Mp4,
            quality: qrec_core::protocol::command::Quality::Draft,
            codec: None,
            out: None,
        };
        store.dispatch_with(export, false, |_| {});
        assert_eq!(store.unsettled_requests(), 1, "sent, not started");
        let progress = Progress {
            phase: qrec_core::protocol::ProgressPhase::Render,
            done: 0,
            total: 90,
            eta_secs: None,
            label: "Rendering clip 1/3".into(),
            take: None,
        };
        store.on_progress(1, Some(&progress));
        assert_eq!(store.unsettled_requests(), 1, "no export in the table yet");
        let mut next = (*store.state()).clone();
        let ctx = testkit::ctx("2026-10-14T15:30:12Z", 1);
        let cmd = Command::Export {
            format: qrec_core::protocol::command::ExportFormat::Mp4,
            quality: qrec_core::protocol::command::Quality::Draft,
            codec: None,
            out: None,
        };
        next = qrec_core::apply(&next, &ctx, cmd).unwrap().state;
        store.on_snapshot(1, Arc::new(next));
        assert_eq!(store.unsettled_requests(), 0, "its job runs");
        assert_eq!(store.pending_requests(), 1, "still unanswered");
        store.dispatch(Command::Select { op: SelectOp::All });
        assert_eq!(store.unsettled_requests(), 1, "another request counts");
        // The export ends: the job leaves the table, the request counts
        // again until its answer is in.
        store.on_snapshot(2, Arc::new(testkit::design_state()));
        assert_eq!(store.unsettled_requests(), 2);
        drain(&store);
        assert_eq!(store.unsettled_requests(), 0, "both answered");
    }

    #[test]
    fn notices_wait_for_the_toast_host() {
        let store = store();
        store.notify(Notice::new(ToastLevel::Warning, "early"));
        store.on_toast(
            Toast::new(ToastLevel::Success, "Take 9 landed").with_action(
                "Undo",
                UiAction::Dispatch(Box::new(Command::Undo { steps: 1 })),
            ),
            false,
        );
        let got = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&got);
        let sub = store.subscribe_notices(move |n| seen.borrow_mut().push(n.clone()));
        store.notify(Notice::new(ToastLevel::Info, "late"));
        let titles: Vec<String> = got.borrow().iter().map(|n| n.title.clone()).collect();
        assert_eq!(titles, ["early", "Take 9 landed", "late"]);
        assert_eq!(got.borrow()[1].actions.len(), 1, "the core's action stays");
        drop(sub);
        store.notify(Notice::new(ToastLevel::Info, "kept for the next host"));
        assert_eq!(got.borrow().len(), 3);
    }

    #[test]
    fn snapshots_reach_subscribers_with_the_previous_state() {
        let store = store();
        let changes = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&changes);
        let sub = store.subscribe(move |c| {
            seen.borrow_mut()
                .push((c.prev.ui.playhead, c.next.ui.playhead, c.ui(), c.timeline()));
        });
        let mut next = (*store.state()).clone();
        next.ui.playhead = 10;
        store.on_snapshot(5, Arc::new(next));
        assert_eq!(store.rev(), 5);
        assert_eq!(*changes.borrow(), [(2437, 10, true, false)]);
        drop(sub);
        store.on_snapshot(6, Arc::new(testkit::design_state()));
        assert_eq!(changes.borrow().len(), 1, "unsubscribed");
    }

    #[test]
    fn local_updates_made_by_subscribers_run_after_them() {
        let store = store();
        let log = Rc::new(RefCell::new(Vec::new()));
        let (seen, inner) = (Rc::clone(&log), store.clone());
        store
            .subscribe_local(move |l| {
                seen.borrow_mut().push(l.overlays.len());
                if l.overlays.len() == 1 {
                    inner.update_local(|l| l.overlays.push(Overlay::Menu));
                }
            })
            .detach();
        store.update_local(|l| l.overlays.push(Overlay::Palette));
        assert_eq!(*log.borrow(), [1, 2]);
        // No change, no call.
        store.update_local(|_| {});
        assert_eq!(log.borrow().len(), 2);
    }

    #[test]
    fn sent_values_and_the_echo_rule() {
        let store = store();
        store.dispatch(Command::SetUi {
            patch: UiPatch {
                zoom: Nullable::Set(4.0),
                scroll_px: Some(120.0),
                ..UiPatch::default()
            },
        });
        store.dispatch(Command::Seek {
            to: SeekTarget::To(qrec_core::TimeArg::frames(40)),
        });
        let sent = store.sent().clone();
        assert_eq!(sent.zoom, Some(Some(4.0)));
        assert_eq!(sent.scroll_px, Some(120.0));
        assert_eq!(sent.bin_open, None);
        assert!(matches!(sent.seek, Some(SeekTarget::To(_))));
        // The snapshot doesn't show it yet: keep the widget's value.
        assert_eq!(echo(Some(&120.0), &0.0, &0.0), Echo::Keep);
        // It shows it, or someone else's value: adopt.
        assert_eq!(echo(Some(&120.0), &0.0, &120.0), Echo::Adopt);
        assert_eq!(echo(Some(&120.0), &0.0, &64.0), Echo::Adopt);
        assert_eq!(echo(None::<&f64>, &0.0, &0.0), Echo::Adopt);
        let _ = json!(null);
    }
}
