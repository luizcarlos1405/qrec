//! Messages into the session thread (A§8.1, foundation).
//!
//! Workers (process runners, timers, the inotify watcher, IPC connections,
//! the capture engine thread) only talk to the session through an [`Inbox`].

use qrec_core::protocol::{Command, Progress, Query, WireBody};
use qrec_core::{CoreError, Event};

/// A request to the Session from the CLI (in-process), an IPC connection or
/// the GUI. The Session allocates its `ReqId`; client ids stay in the IPC layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub body: WireBody,
    /// `--yes` or a confirmed modal (`Ctx::confirmed`).
    pub confirmed: bool,
}

impl Request {
    pub fn command(cmd: Command, confirmed: bool) -> Self {
        Self {
            body: WireBody::Command(cmd),
            confirmed,
        }
    }

    pub fn query(q: Query) -> Self {
        Self {
            body: WireBody::Query(q),
            confirmed: false,
        }
    }
}

/// Everything the session thread handles.
pub enum Msg {
    /// A request and where its accepted/progress/reply messages go.
    Request {
        req: Request,
        to: Box<dyn Responder>,
    },
    /// The result of an effect (or an outside change) for `on_event`.
    Event(Event),
    /// The watcher saw `qrec.json` change (debounced, A§8.9).
    DiskChanged,
    /// Ctrl+C / SIGTERM in the CLI (A§8.6).
    Interrupt,
    /// Stop accepting, let jobs finish, exit.
    Shutdown,
}

impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Msg::Request { req, .. } => f
                .debug_struct("Request")
                .field("req", req)
                .finish_non_exhaustive(),
            Msg::Event(ev) => f.debug_tuple("Event").field(ev).finish(),
            Msg::DiskChanged => f.write_str("DiskChanged"),
            Msg::Interrupt => f.write_str("Interrupt"),
            Msg::Shutdown => f.write_str("Shutdown"),
        }
    }
}

/// The sending side of the session's queue. Cheap to clone; every worker gets one.
#[derive(Clone, Debug)]
pub struct Inbox(crossbeam_channel::Sender<Msg>);

impl Inbox {
    /// A new unbounded queue: the inbox and the session's receiving end.
    pub fn channel() -> (Inbox, crossbeam_channel::Receiver<Msg>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (Inbox(tx), rx)
    }

    pub fn new(sender: crossbeam_channel::Sender<Msg>) -> Self {
        Inbox(sender)
    }

    /// Sends a message; `false` when the session is gone.
    pub fn send(&self, msg: Msg) -> bool {
        self.0.send(msg).is_ok()
    }

    /// Sends `Msg::Event(ev)`; `false` when the session is gone.
    pub fn event(&self, ev: Event) -> bool {
        self.send(Msg::Event(ev))
    }
}

/// Where the session sends what happens to a request: the CLI's stdout, an
/// IPC connection (`WireMessage` lines) or the GUI.
pub trait Responder: Send {
    /// The request parsed and was queued (`{"id","accepted":true}`).
    fn accepted(&mut self);
    fn progress(&mut self, p: &Progress);
    /// The final answer: a `CmdResult`, a query result or `HelloInfo` as JSON.
    fn reply(&mut self, r: Result<serde_json::Value, CoreError>);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_delivers_in_order() {
        let (inbox, rx) = Inbox::channel();
        let other = inbox.clone();
        assert!(inbox.send(Msg::DiskChanged));
        assert!(other.event(Event::EngineWedged {
            message: "x".into()
        }));
        assert!(matches!(rx.recv().unwrap(), Msg::DiskChanged));
        assert!(matches!(
            rx.recv().unwrap(),
            Msg::Event(Event::EngineWedged { .. })
        ));
        drop(rx);
        assert!(!inbox.send(Msg::Shutdown), "the session is gone");
    }
}
