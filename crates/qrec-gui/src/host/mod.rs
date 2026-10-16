//! The window's side of the GUI host (G§3.4, G§3.5): the [`GuiPort`] the
//! host and the Session call ([`Port`]), the [`Responder`] of the window's
//! own requests ([`GuiResponder`]), the Session it opens ([`opener`]) and
//! the queue into the main thread ([`queue`]).
//!
//! Every call here returns at once: it only pushes to the queue (R2).
//! Only `store/`, `host/` and `startup/` touch `GuiHost` (R8).

pub mod opener;
pub mod queue;

use std::sync::Arc;

use qrec_core::protocol::{Progress, Toast};
use qrec_core::{CoreError, State};
use qrec_shell::host::{GuiPort, HostEvent};
use qrec_shell::msg::Responder;
use serde_json::Value;

use queue::{Inbound, Queue, Ticket};

/// The window's [`GuiPort`]: everything the host and its Session report
/// goes to the queue, in order.
#[derive(Clone, Debug)]
pub struct Port {
    queue: Queue,
}

impl Port {
    pub fn new(queue: Queue) -> Self {
        Self { queue }
    }

    /// The queue it pushes to.
    pub fn queue(&self) -> &Queue {
        &self.queue
    }
}

impl GuiPort for Port {
    fn snapshot(&self, rev: u64, state: Arc<State>) {
        self.queue.push(Inbound::Snapshot(rev, state));
    }

    fn toast(&self, toast: Toast, recovery: bool) {
        self.queue.push(Inbound::Toast { toast, recovery });
    }

    fn host(&self, event: HostEvent) {
        self.queue.push(Inbound::Host(event));
    }
}

/// Where the Session answers one request of the window: the queue, tagged
/// with the request's ticket. Immediate and late replies stay apart (H-7).
pub struct GuiResponder {
    ticket: Ticket,
    queue: Queue,
    answered: bool,
}

impl GuiResponder {
    pub fn new(ticket: Ticket, queue: Queue) -> Self {
        Self {
            ticket,
            queue,
            answered: false,
        }
    }

    fn answer(&mut self, result: Result<Value, CoreError>, late: bool) {
        if std::mem::replace(&mut self.answered, true) {
            tracing::warn!("request {} was answered twice", self.ticket);
            return;
        }
        self.queue.push(Inbound::Reply {
            ticket: self.ticket,
            result,
            late,
        });
    }
}

impl Responder for GuiResponder {
    fn accepted(&mut self) {
        self.queue.push(Inbound::Accepted(self.ticket));
    }

    fn progress(&mut self, p: &Progress) {
        self.queue.push(Inbound::Progress(self.ticket, p.clone()));
    }

    fn reply(&mut self, r: Result<Value, CoreError>) {
        self.answer(r, false);
    }

    fn reply_late(&mut self, r: Result<Value, CoreError>) {
        self.answer(r, true);
    }
}

impl Drop for GuiResponder {
    /// Every request gets one answer: one the Session dropped unanswered
    /// (it stopped) gets an error, so its `done` still runs.
    fn drop(&mut self) {
        if !self.answered {
            self.answer(
                Err(CoreError::io("the Qrec session stopped before it answered")),
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use qrec_core::testkit;

    use super::*;

    fn queue() -> Queue {
        Queue::new(|| {})
    }

    #[test]
    fn the_port_queues_in_order() {
        let q = queue();
        let port = Port::new(q.clone());
        port.host(HostEvent::Ready);
        port.snapshot(1, Arc::new(testkit::design_state()));
        port.toast(
            Toast::new(qrec_core::protocol::ToastLevel::Info, "hello"),
            false,
        );
        port.host(HostEvent::Opened);
        let items: Vec<String> = q.take_all().iter().map(|i| format!("{i:?}")).collect();
        assert_eq!(
            items,
            [
                "Host(Ready)",
                "Snapshot(1)",
                "Toast(\"hello\", recovery false)",
                "Host(Opened)"
            ]
        );
    }

    /// R8: only `store/`, `host/` and `startup/` touch the `GuiHost`, and
    /// nothing else submits requests.
    #[test]
    fn only_store_host_and_startup_touch_the_gui_host() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut dirs = vec![src.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let relative = path.strip_prefix(&src).unwrap().to_path_buf();
                let top = relative
                    .components()
                    .next()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .unwrap_or_default();
                if ["store", "host", "startup"].contains(&top.as_str()) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (n, line) in text.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or_default();
                    if code.contains("GuiHost") || code.contains(".submit(") {
                        offenders.push(format!("{}:{}", relative.display(), n + 1));
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "R8: {offenders:?}");
    }

    #[test]
    fn replies_keep_their_kind_and_come_once() {
        let q = queue();
        let mut immediate = GuiResponder::new(1, q.clone());
        immediate.accepted();
        immediate.reply(Ok(Value::Null));
        immediate.reply(Ok(Value::Null));
        let mut late = GuiResponder::new(2, q.clone());
        late.reply_late(Err(CoreError::invalid("late")));
        drop(immediate);
        drop(late);
        // Dropped unanswered: an error reply all the same.
        drop(GuiResponder::new(3, q.clone()));
        let items: Vec<String> = q.take_all().iter().map(|i| format!("{i:?}")).collect();
        assert_eq!(
            items,
            [
                "Accepted(1)",
                "Reply(1, ok, late false)",
                "Reply(2, err, late true)",
                "Reply(3, err, late false)"
            ]
        );
    }
}
