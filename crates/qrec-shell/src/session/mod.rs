//! The Session (A§8.1): one per open project, on one session thread.
//! Owner: session. Stub: signatures only.
//!
//! `handle(Request)`: allocate a `ReqId`, `facts_needed` → gather facts →
//! `apply` → run the effects in the A§7.6 order → `Responder`. Events go to
//! `on_event` (with `facts_needed_for_event`). Queries are answered with
//! `qrec_core::query`. Every state change goes through here.
#![allow(clippy::todo, dead_code, unused_variables)]

use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;
use qrec_core::State;

use crate::deps::{Deps, OpenOpts};
use crate::error::ShellError;
use crate::msg::{Inbox, Msg};

pub struct Session {
    dir: PathBuf,
    state: State,
    deps: Deps,
    inbox: Inbox,
    rx: Receiver<Msg>,
}

impl Session {
    /// Opens the project in `dir` (A§8.3, A§8.4, A§7.6 recovery on open).
    pub fn open(dir: &Path, opts: OpenOpts, deps: Deps) -> Result<Session, ShellError> {
        todo!("session: Session::open")
    }

    /// A sender for workers, IPC connections and the GUI.
    pub fn inbox(&self) -> Inbox {
        self.inbox.clone()
    }

    /// Handles one message (request, event, disk change, interrupt, shutdown).
    pub fn handle(&mut self, msg: Msg) {
        todo!("session: Session::handle")
    }

    /// Drains the inbox until `done(state)` holds (CLI and hosts).
    pub fn run_until(&mut self, done: impl FnMut(&State) -> bool) {
        todo!("session: Session::run_until")
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// The project directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}
