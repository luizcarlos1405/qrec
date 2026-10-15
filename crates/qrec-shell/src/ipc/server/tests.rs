use std::io::Write;
use std::time::{Duration, Instant};

use qrec_core::CoreError;
use qrec_core::protocol::{CmdResult, Progress, ProgressPhase, WireBody};
use serde_json::{Value, json};

use super::*;
use crate::ipc::testing::{
    raw_exchange, read_all, request_line, session_thread, socket_dir, start_server, wait_for,
};

fn progress(done: u64) -> Progress {
    Progress {
        phase: ProgressPhase::Render,
        done,
        total: 100,
        eta_secs: None,
        label: format!("step {done}"),
        take: None,
    }
}

fn command_line(id: &str) -> String {
    request_line(id, json!({ "command": { "cmd": "undo" } }))
}

fn ok_result(message: &str) -> Value {
    serde_json::to_value(CmdResult::new(message)).unwrap()
}

#[test]
fn hello_before_attach_is_answered_by_the_server() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let lines = raw_exchange(&t.socket, &request_line("h1", json!("hello")));
    assert_eq!(lines[0], json!({ "id": "h1", "accepted": true }));
    let reply = &lines[1]["reply"];
    assert_eq!(reply["ok"], true, "{lines:?}");
    assert_eq!(reply["data"]["host"], "headless");
    assert_eq!(reply["data"]["pid"], std::process::id());
    assert_eq!(reply["data"]["projectDir"], "/projects/swamp");
    assert_eq!(reply["data"]["record"], json!({ "phase": "idle" }));
    assert_eq!(lines.len(), 2);
    // hello isn't a request for the exit rules.
    assert_eq!(t.server.stats().requests, 0);
}

#[test]
fn requests_wait_for_the_session_in_order() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let socket = t.socket.clone();
    let clients: Vec<_> = (0..3)
        .map(|i| {
            let socket = socket.clone();
            let handle =
                std::thread::spawn(move || raw_exchange(&socket, &command_line(&format!("r{i}"))));
            // Arrive one after another.
            assert!(wait_for(Duration::from_secs(5), || t
                .server
                .stats()
                .requests
                == i + 1));
            handle
        })
        .collect();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let order = Arc::clone(&seen);
    let (inbox, session) = session_thread(move |msg| {
        if let Msg::Request { req, mut to } = msg {
            order.lock().unwrap().push(req.body.clone());
            to.reply(Ok(json!({ "n": order.lock().unwrap().len() })));
        }
        true
    });
    t.server.attach(inbox);
    for (i, client) in clients.into_iter().enumerate() {
        let lines = client.join().unwrap();
        assert_eq!(lines[0], json!({ "id": format!("r{i}"), "accepted": true }));
        assert_eq!(lines[1]["reply"]["data"]["n"], i + 1, "{lines:?}");
    }
    assert_eq!(seen.lock().unwrap().len(), 3);
    assert!(matches!(seen.lock().unwrap()[0], WireBody::Command(_)));
    drop(t);
    session.join().unwrap();
}

#[test]
fn accepted_then_progress_then_one_reply_then_eof() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let (inbox, _session) = session_thread(|msg| {
        if let Msg::Request { mut to, req } = msg {
            assert!(req.confirmed);
            to.accepted();
            for done in [10, 20, 30] {
                to.progress(&progress(done));
                std::thread::sleep(Duration::from_millis(30));
            }
            to.reply(Ok(ok_result("done")));
            // Anything after the reply is dropped.
            to.reply(Err(CoreError::internal("second reply")));
            to.progress(&progress(99));
        }
        true
    });
    t.server.attach(inbox);
    let line =
        json!({ "v": 1, "id": "x", "confirmed": true, "body": { "command": { "cmd": "undo" } } });
    let lines = raw_exchange(&t.socket, &line.to_string());
    assert_eq!(lines.first(), Some(&json!({ "id": "x", "accepted": true })));
    assert_eq!(
        lines.last().unwrap()["reply"],
        json!({ "ok": true, "data": ok_result("done") })
    );
    let dones: Vec<u64> = lines[1..lines.len() - 1]
        .iter()
        .map(|l| l["progress"]["done"].as_u64().unwrap())
        .collect();
    assert!(!dones.is_empty() && dones.last() == Some(&30), "{lines:?}");
    assert!(dones.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(
        lines.iter().filter(|l| l.get("accepted").is_some()).count(),
        1
    );
}

#[test]
fn progress_is_coalesced_and_the_reply_is_never_dropped() {
    let out = Outgoing::default();
    out.set_id("7".into());
    out.push_accepted();
    for done in 0..1000 {
        out.push_progress(&progress(done));
    }
    {
        let st = out.state();
        assert_eq!(st.queue.len(), 2, "accepted + the newest progress");
        assert!(
            matches!(&st.queue[1], WireMessage::Progress { progress, .. } if progress.done == 999)
        );
    }
    out.push_reply(&Ok(json!(1)));
    out.push_progress(&progress(5000));
    let st = out.state();
    assert_eq!(st.queue.len(), 3);
    assert!(matches!(st.queue[2], WireMessage::Reply { .. }));
}

#[test]
fn a_full_queue_drops_progress_never_replies() {
    let out = Outgoing::default();
    {
        // Fill the queue with non-progress messages (a stand-in for a
        // client that stopped reading).
        let mut st = out.state();
        for _ in 0..CONN_QUEUE_CAP {
            st.queue.push_back(WireMessage::accepted("x"));
        }
    }
    out.push_progress(&progress(1));
    assert_eq!(out.state().queue.len(), CONN_QUEUE_CAP, "progress skipped");
    out.push_reply(&Ok(json!("r")));
    assert_eq!(
        out.state().queue.len(),
        CONN_QUEUE_CAP + 1,
        "the reply always fits"
    );
}

#[test]
fn malformed_requests_get_usage_errors() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |cfg| {
        cfg.max_line = 200;
        cfg.read_timeout = Duration::from_millis(300);
    });
    let lines = raw_exchange(&t.socket, "{not json");
    assert_eq!(lines.len(), 1, "no accepted for a bad line: {lines:?}");
    assert_eq!(lines[0]["id"], "");
    assert_eq!(lines[0]["reply"]["error"]["code"], "usage");

    let lines = raw_exchange(&t.socket, r#"{"v":2,"id":"9","body":"hello"}"#);
    assert_eq!(lines[0]["id"], "9");
    let error = &lines[0]["reply"]["error"];
    assert_eq!(error["code"], "usage");
    assert!(
        error["message"].as_str().unwrap().contains("version 2"),
        "{error}"
    );

    let lines = raw_exchange(
        &t.socket,
        r#"{"v":1,"id":"8","body":{"command":{"cmd":"nope"}}}"#,
    );
    assert_eq!(lines[0]["id"], "8");
    assert_eq!(lines[0]["reply"]["error"]["code"], "usage");

    let long = format!(r#"{{"v":1,"id":"{}","body":"hello"}}"#, "x".repeat(300));
    let lines = raw_exchange(&t.socket, &long);
    assert!(
        lines[0]["reply"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("longer than 200")
    );

    // A client that never finishes its line is cut off after the read timeout.
    let stream = UnixStream::connect(&t.socket).unwrap();
    (&stream).write_all(b"{\"v\":1").unwrap();
    let started = Instant::now();
    let lines = read_all(stream, Instant::now() + Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        lines[0]["reply"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no complete request line")
    );
    assert_eq!(t.server.stats().requests, 0);
}

#[test]
fn peers_with_another_uid_are_refused() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |cfg| {
        cfg.allowed_uid = peer::our_uid().wrapping_add(1)
    });
    let stream = UnixStream::connect(&t.socket).unwrap();
    let _ = (&stream).write_all(format!("{}\n", request_line("1", json!("hello"))).as_bytes());
    let lines = read_all(stream, Instant::now() + Duration::from_secs(5));
    assert!(lines.is_empty(), "closed without an answer: {lines:?}");
    assert!(wait_for(Duration::from_secs(2), || t.server.stats().open == 0));
}

#[test]
fn shutdown_stops_accepting_and_unlinks_the_socket() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let lines = raw_exchange(&t.socket, &request_line("s", json!("shutdown")));
    assert_eq!(lines[0], json!({ "id": "s", "accepted": true }));
    assert_eq!(lines[1]["reply"]["ok"], true);
    assert!(
        lines[1]["reply"]["data"]["message"]
            .as_str()
            .unwrap()
            .contains("shutting down")
    );
    assert!(t.events.lock().unwrap().contains(&ServerEvent::Shutdown));
    let stats = t.server.stats();
    assert!(stats.shutdown && !stats.accepting);
    assert!(!t.socket.exists());
    assert!(UnixStream::connect(&t.socket).is_err());
}

/// A host stops accepting only while no connection is open, atomically
/// with admission (A§8.3 step 3): one that is open keeps it serving; once
/// none is, the socket goes and nothing more is admitted.
#[test]
fn stopping_to_accept_waits_for_open_connections() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let held = UnixStream::connect(&t.socket).unwrap();
    assert!(wait_for(Duration::from_secs(5), || t.server.stats().open == 1));
    assert!(!t.server.stop_accepting_if_idle());
    assert!(t.server.stats().accepting);
    assert!(t.socket.exists());
    drop(held);
    assert!(wait_for(Duration::from_secs(5), || t.server.stats().open == 0));
    assert!(t.server.stop_accepting_if_idle());
    let stats = t.server.stats();
    assert!(!stats.accepting && !stats.shutdown);
    assert!(!t.socket.exists());
    assert!(UnixStream::connect(&t.socket).is_err());
    // Idempotent.
    assert!(t.server.stop_accepting_if_idle());
}

#[test]
fn every_accepted_request_gets_exactly_one_reply() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    // The Session drops the request without answering.
    let (inbox, _session) = session_thread(|msg| {
        drop(msg);
        true
    });
    t.server.attach(inbox);
    let lines = raw_exchange(&t.socket, &command_line("d"));
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["reply"]["error"]["code"], "io");

    // A failed Session answers parked and new requests with its error.
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let socket = t.socket.clone();
    let parked = std::thread::spawn(move || raw_exchange(&socket, &command_line("p")));
    assert!(wait_for(Duration::from_secs(5), || t
        .server
        .stats()
        .requests
        == 1));
    t.server
        .fail(CoreError::invalid("qrec.json on disk is invalid"));
    let lines = parked.join().unwrap();
    assert_eq!(lines[1]["reply"]["error"]["code"], "invalid");
    let lines = raw_exchange(&t.socket, &command_line("n"));
    assert_eq!(lines[1]["reply"]["error"]["code"], "invalid");
    let lines = raw_exchange(&t.socket, &request_line("h", json!("hello")));
    assert_eq!(lines[1]["reply"]["error"]["code"], "invalid");
}

#[test]
fn a_client_that_leaves_frees_its_connection() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let held = Arc::new(Mutex::new(Vec::new()));
    let keep = Arc::clone(&held);
    let (inbox, _session) = session_thread(move |msg| {
        // Hold the responder: the reply never comes.
        keep.lock().unwrap().push(msg);
        true
    });
    t.server.attach(inbox);
    let stream = UnixStream::connect(&t.socket).unwrap();
    (&stream)
        .write_all(format!("{}\n", command_line("g")).as_bytes())
        .unwrap();
    assert!(wait_for(Duration::from_secs(5), || held
        .lock()
        .unwrap()
        .len()
        == 1));
    assert_eq!(t.server.stats().open, 1);
    drop(stream);
    assert!(wait_for(Duration::from_secs(5), || t.server.stats().open == 0));
    assert!(t.events.lock().unwrap().contains(&ServerEvent::Idle));
    assert!(t.server.stats().idle_since.is_some());
}

#[test]
fn requester_gone_is_reported_once_the_request_is_bound() {
    let (inbox, rx) = Inbox::channel();
    // Gone before the Session bound it: reported at bind time.
    let out = Outgoing::default();
    out.mark_gone();
    out.bind(ReqId("r1".into()), inbox.clone());
    match rx.try_recv().unwrap() {
        Msg::Event(Event::RequesterGone { req }) => assert_eq!(req, ReqId("r1".into())),
        other => panic!("{other:?}"),
    }
    // Bound, then gone: reported once.
    let out = Outgoing::default();
    out.bind(ReqId("r2".into()), inbox.clone());
    out.mark_gone();
    out.mark_gone();
    assert!(matches!(
        rx.try_recv().unwrap(),
        Msg::Event(Event::RequesterGone { .. })
    ));
    assert!(rx.try_recv().is_err());
    // Already answered: nobody to tell.
    let out = Outgoing::default();
    out.bind(ReqId("r3".into()), inbox);
    out.push_reply(&Ok(json!(null)));
    out.mark_gone();
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_client_that_leaves_before_its_reply_is_reported_to_the_session() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let (gone_tx, gone_rx) = crossbeam_channel::unbounded();
    let held = Arc::new(Mutex::new(Vec::new()));
    let keep = Arc::clone(&held);
    // A Session that accepts the request (with its id current, like the
    // real one) and keeps it without answering.
    let (inbox, _session) = session_thread(move |msg| {
        match msg {
            Msg::Request { mut to, .. } => {
                crate::session::with_current(&ReqId("r7".into()), || to.accepted());
                keep.lock().unwrap().push(to);
            }
            Msg::Event(Event::RequesterGone { req }) => {
                let _ = gone_tx.send(req);
            }
            _ => {}
        }
        true
    });
    t.server.attach(inbox);
    let stream = UnixStream::connect(&t.socket).unwrap();
    (&stream)
        .write_all(format!("{}\n", command_line("x")).as_bytes())
        .unwrap();
    assert!(wait_for(Duration::from_secs(5), || held
        .lock()
        .unwrap()
        .len()
        == 1));
    // Ctrl+C in the CLI: the connection closes before the reply.
    drop(stream);
    let gone = gone_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(gone, ReqId("r7".into()));
}

#[test]
fn finish_lets_replies_flush_then_answers_stragglers() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    let held = Arc::new(Mutex::new(Vec::new()));
    let keep = Arc::clone(&held);
    let (inbox, _session) = session_thread(move |msg| {
        keep.lock().unwrap().push(msg);
        true
    });
    t.server.attach(inbox);
    let socket = t.socket.clone();
    let client = std::thread::spawn(move || raw_exchange(&socket, &command_line("w")));
    assert!(wait_for(Duration::from_secs(5), || held
        .lock()
        .unwrap()
        .len()
        == 1));
    let started = Instant::now();
    t.server.finish(
        CoreError::io("the host is exiting"),
        Duration::from_millis(300),
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let lines = client.join().unwrap();
    assert_eq!(
        lines.last().unwrap()["reply"]["error"]["message"],
        "the host is exiting"
    );
    assert_eq!(t.server.stats().open, 0);
    assert!(!t.socket.exists());
}

/// A signal while the Session opens (A§8.6): its `RecordStop` and
/// `Interrupt` reach the Session before the requests parked meanwhile, so
/// the Session refuses those instead of running them first (a queued
/// `record start` used to start recording, then stop at once).
#[test]
fn urgent_messages_go_ahead_of_parked_requests() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    assert!(t.server.send(Msg::DiskChanged));
    let sender = t.server.sender();
    assert!(sender.send_urgent(Msg::Shutdown));
    assert!(sender.send_urgent(Msg::Interrupt));
    let (inbox, rx) = Inbox::channel();
    t.server.attach(inbox);
    assert!(matches!(rx.try_recv().unwrap(), Msg::Shutdown));
    assert!(matches!(rx.try_recv().unwrap(), Msg::Interrupt));
    assert!(matches!(rx.try_recv().unwrap(), Msg::DiskChanged));
    // Attached, they keep their place in the queue.
    assert!(t.server.send(Msg::DiskChanged));
    assert!(sender.send_urgent(Msg::Interrupt));
    assert!(matches!(rx.try_recv().unwrap(), Msg::DiskChanged));
    assert!(matches!(rx.try_recv().unwrap(), Msg::Interrupt));
}

#[test]
fn messages_sent_before_attach_are_delivered_in_order() {
    let dir = socket_dir();
    let t = start_server(dir.path(), |_| {});
    assert!(t.server.send(Msg::DiskChanged));
    assert!(t.server.send(Msg::Interrupt));
    let (inbox, rx) = Inbox::channel();
    t.server.attach(inbox);
    assert!(t.server.send(Msg::Shutdown));
    assert!(matches!(rx.try_recv().unwrap(), Msg::DiskChanged));
    assert!(matches!(rx.try_recv().unwrap(), Msg::Interrupt));
    assert!(matches!(rx.try_recv().unwrap(), Msg::Shutdown));
    t.server.fail(CoreError::io("gone"));
    assert!(!t.server.send(Msg::DiskChanged));
}
