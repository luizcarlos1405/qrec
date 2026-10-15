//! Session tests with fake deps (no GStreamer, no ffmpeg jobs): timeline
//! commands dispatched against a project on disk, lazy init, the folder
//! rule, locking, read-only opens, hello and request ids, purge ordering,
//! crash points between the commit steps, recovery probes, capabilities,
//! the A§7.7 rules, external-edit merges, undo/redo/restore on disk, UI
//! coalescing and interrupts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use qrec_core::apply::Outcome;
use qrec_core::apply::project::is_initialized;
use qrec_core::history::ops::unchecked;
use qrec_core::model::StreamKind;
use qrec_core::model::TakeStatus;
use qrec_core::plans::{CapturePlan, ExportPlan, FramePlan, LiveScene};
use qrec_core::protocol::{
    Command, Effect, HelloInfo, HistoryOp, MediaInfo, MoveTo, ProbeJob, ProbePurpose, ProbeResult,
    Progress, RecordModeArg, RecordedStream, SceneTarget, SelectOp, Toast, VideoSourceArg,
    WireBody,
};
use qrec_core::state::{HostKind, Job, JobKind, RecordPhase, SilenceKey};
use qrec_core::{ClipId, CoreError, ErrorCode, Event, Fps, JobId, ReqId, TakeId};
use serde_json::Value;

use super::effects::{Origin, Ran};
use super::*;
use crate::deps::{CaptureEngine, Deps, MediaRunner, OpenOpts};
use crate::host::lockfile::{self, LockRecord};
use crate::msg::{Inbox, Msg, Request, Responder};
use crate::paths::{ProjectPaths, UserDirs};
use crate::store::{BaseFile, Head, Step, format, testdata};
use crate::testkit::{TestEnv, write_project};

// ------------------------------------------------------------------ fakes

/// What the fakes saw, in order.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, line: impl Into<String>) {
        self.0.lock().unwrap().push(line.into());
    }

    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    fn has(&self, prefix: &str) -> bool {
        self.lines().iter().any(|l| l.starts_with(prefix))
    }

    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

struct FakeCapture(Log);

impl CaptureEngine for FakeCapture {
    fn set_live(&mut self, live: Option<LiveScene>, _out: Inbox) {
        self.0.push(format!("set_live {}", live.is_some()));
    }

    fn start(&mut self, plan: CapturePlan, _out: Inbox) {
        self.0.push(format!("start {}", plan.take));
    }

    fn stop(&mut self, take: &TakeId, cancel: bool) {
        self.0.push(format!("stop {take} cancel={cancel}"));
    }

    fn shutdown(&mut self) {
        self.0.push("shutdown");
    }
}

struct FakeMedia {
    log: Log,
    dir: PathBuf,
}

impl MediaRunner for FakeMedia {
    fn probe(&mut self, take: TakeId, purpose: ProbePurpose, jobs: Vec<ProbeJob>, _out: Inbox) {
        let files: Vec<&str> = jobs.iter().map(|j| j.file.as_str()).collect();
        self.log
            .push(format!("probe {take} {purpose:?} {}", files.join(",")));
    }

    fn detect_silence(&mut self, job: JobId, _key: SilenceKey, _argv: Vec<String>, _out: Inbox) {
        self.log.push(format!("silence {job}"));
    }

    fn render_export(&mut self, job: JobId, _plan: ExportPlan, _out: Inbox) {
        self.log.push(format!("export {job}"));
    }

    fn save_frame(&mut self, job: JobId, _plan: FramePlan, _out: Inbox) {
        self.log.push(format!("frame {job}"));
    }

    fn delete_take_dirs(&mut self, job: JobId, takes: Vec<TakeId>, out: Inbox) {
        // A§7.6 step 4: the commit record no longer has the takes.
        let head: Head =
            serde_json::from_slice(&fs::read(self.dir.join(".qrec/history.head.json")).unwrap())
                .unwrap();
        let committed = takes.iter().all(|t| !head.project.takes.contains_key(t));
        let names: Vec<&str> = takes.iter().map(TakeId::as_str).collect();
        self.log
            .push(format!("delete {} committed={committed}", names.join(",")));
        let deleted = takes.iter().map(|t| format!("takes/{t}")).collect();
        out.event(Event::TakeDirsDeleted {
            job,
            deleted,
            errors: Vec::new(),
        });
    }

    fn cancel(&mut self, job: JobId) {
        self.log.push(format!("cancel {job}"));
    }
}

/// Everything a responder heard.
#[derive(Clone, Default)]
struct Answers {
    accepted: Arc<Mutex<Vec<Option<ReqId>>>>,
    progress: Arc<Mutex<Vec<Progress>>>,
    replies: Arc<Mutex<Vec<Result<Value, CoreError>>>>,
}

impl Answers {
    fn last(&self) -> Option<Result<Value, CoreError>> {
        self.replies.lock().unwrap().last().cloned()
    }

    fn responder(&self) -> Box<dyn Responder> {
        Box::new(Recorder(self.clone()))
    }
}

struct Recorder(Answers);

impl Responder for Recorder {
    fn accepted(&mut self) {
        self.0.accepted.lock().unwrap().push(current_request());
    }

    fn progress(&mut self, p: &Progress) {
        self.0.progress.lock().unwrap().push(p.clone());
    }

    fn reply(&mut self, r: Result<Value, CoreError>) {
        self.0.replies.lock().unwrap().push(r);
    }
}

// ---------------------------------------------------------------- harness

struct Harness {
    env: TestEnv,
    dir: PathBuf,
    log: Log,
    toasts: Arc<Mutex<Vec<Toast>>>,
}

impl Harness {
    /// An empty project folder `name` in a fresh test environment.
    fn new(name: &str) -> Self {
        let env = TestEnv::new();
        let dir = env.empty_dir(name);
        Self {
            env,
            dir,
            log: Log::default(),
            toasts: Arc::default(),
        }
    }

    /// With the sample project written by hand (no `.qrec/` yet).
    fn with_sample(name: &str) -> Self {
        let h = Self::new(name);
        write_project(&h.dir, &testdata::sample_json());
        h
    }

    fn paths(&self) -> ProjectPaths {
        ProjectPaths::new(fs::canonicalize(&self.dir).unwrap())
    }

    fn session_env(&self) -> SessionEnv {
        let dirs = UserDirs::resolve(&self.env.paths_env()).unwrap();
        let mut env = SessionEnv::for_dirs(dirs, true);
        // Tests never look at a GPU.
        env.caps.sys_class_drm = self.env.root().join("no-sysfs");
        env.caps.probe_va = false;
        env
    }

    fn config(&self) -> SessionConfig {
        let toasts = Arc::clone(&self.toasts);
        SessionConfig {
            env: Some(self.session_env()),
            notifier: Some(Box::new(move |t: &Toast| {
                toasts.lock().unwrap().push(t.clone())
            })),
            check: Some(unchecked),
            ..SessionConfig::default()
        }
    }

    fn deps(&self) -> Deps {
        let start: DateTime<Utc> = "2026-10-14T12:00:00Z".parse().unwrap();
        let mut tick = 0;
        let mut entropy = 0u64;
        Deps {
            capture: Box::new(FakeCapture(self.log.clone())),
            media: Box::new(FakeMedia {
                log: self.log.clone(),
                dir: self.dir.clone(),
            }),
            clock: Box::new(move || {
                tick += 1;
                (start + chrono::Duration::seconds(tick), 0)
            }),
            entropy: Box::new(move || {
                entropy += 1;
                entropy
            }),
        }
    }

    fn open_once(&self, opts: OpenOpts) -> Result<Session, ShellError> {
        Session::open_with(&self.dir, opts, self.deps(), self.config())
    }

    /// Opens, retrying a `Busy` lock for a moment: other tests fork
    /// children in parallel, and a child between `fork` and `exec` briefly
    /// holds a copy of every descriptor, a just-dropped session's lock
    /// (`O_CLOEXEC`) included. Use [`Harness::open_once`] to expect `Busy`.
    fn open_opts(&self, opts: OpenOpts) -> Result<Session, ShellError> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.open_once(opts) {
                Err(ShellError::Core(e))
                    if e.code == ErrorCode::Busy && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                other => return other,
            }
        }
    }

    fn open(&self, mutating: bool) -> Result<Session, ShellError> {
        self.open_opts(OpenOpts {
            host: HostKind::Cli,
            mutating,
            recovery: false,
        })
    }

    fn head(&self) -> Head {
        serde_json::from_slice(&fs::read(self.paths().history_head()).unwrap()).unwrap()
    }

    fn history_seqs(&self) -> Vec<u64> {
        let bytes = fs::read(self.paths().history_file()).unwrap_or_default();
        format::parse_history(&bytes)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.seq)
            .collect()
    }

    fn toast_titles(&self) -> Vec<String> {
        self.toasts
            .lock()
            .unwrap()
            .iter()
            .map(|t| t.title.clone())
            .collect()
    }
}

fn clip(id: &str) -> ClipId {
    ClipId::from_raw(id)
}

fn take(n: u32) -> TakeId {
    TakeId::numbered(n)
}

/// Sends a request, handles everything it caused and returns its answers.
fn request(session: &mut Session, body: WireBody, confirmed: bool) -> Answers {
    let answers = Answers::default();
    session.handle(Msg::Request {
        req: Request { body, confirmed },
        to: answers.responder(),
    });
    session.drain();
    answers
}

/// A command's reply (it must have one).
fn send(session: &mut Session, cmd: Command, confirmed: bool) -> Result<Value, CoreError> {
    request(session, WireBody::Command(cmd), confirmed)
        .last()
        .expect("the command was answered")
}

fn clip_layout(session: &Session) -> Vec<(String, u64, u64)> {
    session
        .state()
        .project
        .timeline
        .clips
        .iter()
        .map(|c| (c.id.to_string(), c.in_, c.out))
        .collect()
}

// ------------------------------------------------------------------ tests

#[test]
fn sessions_move_between_threads() {
    fn send<T: Send>() {}
    send::<Session>();
}

#[test]
fn the_keys_query_reads_keys_toml() {
    let h = Harness::with_sample("keys");
    let mut s = h.open(false).unwrap();
    let keys = |s: &mut Session| {
        request(s, WireBody::Query(qrec_core::protocol::Query::Keys), false)
            .last()
            .unwrap()
            .unwrap()
    };
    let chords_of = |rows: &Value, id: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .map(|r| r["keys"].clone())
            .unwrap()
    };
    let defaults = keys(&mut s);
    assert_eq!(chords_of(&defaults, "clip.split"), serde_json::json!(["s"]));
    let config = h.env.config_home().join("qrec");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("keys.toml"),
        "[bindings]\n\"clip.split\" = \"ctrl+b\"\n",
    )
    .unwrap();
    let custom = keys(&mut s);
    assert_eq!(
        chords_of(&custom, "clip.split"),
        serde_json::json!(["ctrl+b"])
    );
}

#[test]
fn timeline_commands_commit_in_order_and_reload() {
    let h = Harness::with_sample("swamp");
    let mut s = h.open(true).unwrap();
    assert!(s.is_host());
    let paths = h.paths();
    assert!(
        paths.history_head().is_file(),
        "the open wrote a commit record"
    );
    assert!(paths.history_base().is_file());

    let split = send(
        &mut s,
        Command::ClipSplit {
            clip: Some(clip("c002")),
            at: "10f".parse().unwrap(),
            timeline: false,
        },
        false,
    )
    .unwrap();
    assert_eq!(split["seq"], 1);
    let new_id = split["created"][0].as_str().unwrap().to_owned();
    let moved = send(
        &mut s,
        Command::ClipMove {
            clips: vec![clip("c001")],
            to: MoveTo::By(1),
        },
        false,
    )
    .unwrap();
    assert_eq!(moved["seq"], 2);
    let removed = send(
        &mut s,
        Command::ClipRemove {
            clips: vec![clip(&new_id)],
        },
        false,
    )
    .unwrap();
    assert_eq!(removed["seq"], 3);
    assert_eq!(
        clip_layout(&s),
        [("c002".to_owned(), 150, 160), ("c001".to_owned(), 0, 100)]
    );

    // On disk: qrec.json is the state, the head points at it, one line per command.
    let bytes = fs::read(paths.project_file()).unwrap();
    assert_eq!(bytes, format::project_bytes(&s.state().project));
    let head = h.head();
    assert_eq!((head.cursor, head.base_seq, head.next_seq), (3, 0, 4));
    assert_eq!(head.project_sha256, format::sha(&bytes));
    assert_eq!(head.project, s.state().project);
    assert_eq!(h.history_seqs(), [1, 2, 3]);
    let base: BaseFile = serde_json::from_slice(&fs::read(paths.history_base()).unwrap()).unwrap();
    assert_eq!(base.project, testdata::sample_project());
    let state = s.state().clone();
    drop(s);

    // A read-only reopen sees exactly the same project and history.
    let again = h.open(false).unwrap();
    assert!(!again.is_host());
    assert_eq!(again.state().project, state.project);
    assert_eq!(again.state().history.entries, state.history.entries);
    assert_eq!(again.state().history.cursor, 3);
    assert!(
        again
            .state()
            .history
            .seen_clip_ids
            .contains(&ClipId::from_raw(new_id.as_str())),
        "a removed clip's id is never handed out again"
    );
}

#[cfg(feature = "testkit")]
#[test]
fn a_project_builder_project_round_trips_through_a_session() {
    use qrec_core::testkit::ProjectBuilder;
    let h = Harness::new("built");
    let project = ProjectBuilder::new()
        .screen_layer("L1", "FAKE-1")
        .mic("A1", "alsa_input.fake")
        .take("take-0001", 300)
        .clip("c001", "take-0001", 0, 150)
        .clip("c002", "take-0001", 150, 300)
        .build();
    fs::write(h.dir.join("qrec.json"), format::project_bytes(&project)).unwrap();
    let mut s = h.open(true).unwrap();
    assert_eq!(s.state().project, project);
    let split = send(
        &mut s,
        Command::ClipSplit {
            clip: None,
            at: "200f".parse().unwrap(),
            timeline: true,
        },
        false,
    )
    .unwrap();
    assert_eq!(split["seq"], 1);
    send(
        &mut s,
        Command::ClipRemove {
            clips: vec![clip("c001")],
        },
        false,
    )
    .unwrap();
    let state = s.state().clone();
    drop(s);
    let again = h.open(false).unwrap();
    assert_eq!(again.state().project, state.project);
    assert_eq!(again.state().history.entries.len(), 2);
}

#[test]
fn the_first_command_in_an_empty_folder_creates_the_project() {
    let h = Harness::new("my-video");
    let mut s = h.open(true).unwrap();
    assert!(!is_initialized(&s.state().project));
    let paths = h.paths();
    assert!(paths.qrec_dir().is_dir(), "mkdir .qrec, then the lock");
    assert!(
        !paths.project_file().exists(),
        "nothing is written before a command"
    );

    let added = send(
        &mut s,
        Command::LayerAdd {
            target: SceneTarget::Project,
            source: VideoSourceArg::Screen {
                output: "FAKE-1".into(),
            },
            name: None,
            rect: None,
        },
        false,
    )
    .unwrap();
    assert_eq!(added["seq"], 1);
    let project = &s.state().project;
    assert_eq!(project.name, "my-video");
    assert_eq!(project.scene.layers.len(), 1);
    assert_eq!(project.scene.layers[0].source.label(), "Fake screen 1");
    assert!(paths.takes_dir().is_dir(), "takes/ comes with the project");
    let base: BaseFile = serde_json::from_slice(&fs::read(paths.history_base()).unwrap()).unwrap();
    assert_eq!(base.base_seq, 0);
    assert!(
        base.project.scene.layers.is_empty(),
        "the base is the new project"
    );
    assert_eq!(h.head().cursor, 1);
    assert_eq!(h.history_seqs(), [1]);

    let err = send(
        &mut s,
        Command::Init {
            canvas: None,
            name: None,
        },
        false,
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert_eq!(err.message, "already a project");
}

#[test]
fn only_mutating_commands_create_a_project() {
    let h = Harness::new("empty");
    let mut s = h.open(true).unwrap();
    for cmd in [
        Command::Select {
            op: SelectOp::Clear,
        },
        Command::Undo { steps: 1 },
        Command::Export {
            format: Default::default(),
            quality: Default::default(),
            codec: None,
            out: None,
        },
    ] {
        let name = cmd.name();
        let err = send(&mut s, cmd, false).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotProject, "{name}");
    }
    assert!(!h.dir.join("qrec.json").exists());
    assert!(dispatch_creates(&Command::ClipRemove { clips: vec![] }));
    assert!(dispatch_creates(&Command::RecordToggle { wait: false }));
    assert!(!dispatch_creates(&Command::Init {
        canvas: None,
        name: None
    }));
    assert!(!dispatch_creates(&Command::HistoryRestore { seq: 0 }));
}

fn dispatch_creates(cmd: &Command) -> bool {
    super::dispatch::creates_the_project(cmd)
}

#[test]
fn a_host_can_queue_requests_before_the_session_exists() {
    let h = Harness::with_sample("early");
    let lock = crate::lock::acquire_eventually(&h.dir);
    let (inbox, rx) = Inbox::channel();
    // IPC accepts a connection before recovery (A§8.3 step 1).
    let answers = Answers::default();
    inbox.send(Msg::Request {
        req: Request::command(
            Command::ClipRemove {
                clips: vec![clip("c001")],
            },
            false,
        ),
        to: answers.responder(),
    });
    let config = SessionConfig {
        lock: Some(lock),
        channel: Some((inbox, rx)),
        ..h.config()
    };
    let opts = OpenOpts {
        host: HostKind::Headless,
        mutating: true,
        recovery: false,
    };
    let mut s = Session::open_with(&h.dir, opts, h.deps(), config).unwrap();
    assert!(s.is_host());
    s.drain();
    assert_eq!(answers.last().unwrap().unwrap()["seq"], 1);

    // A lock of another project is refused.
    let other = Harness::with_sample("other");
    let foreign = crate::lock::acquire_eventually(&other.dir);
    drop(s);
    let config = SessionConfig {
        lock: Some(foreign),
        ..h.config()
    };
    let err = Session::open_with(&h.dir, opts, h.deps(), config).unwrap_err();
    assert!(matches!(err, ShellError::Internal(_)), "{err:?}");
}

#[test]
fn the_folder_rule() {
    let h = Harness::new("busy");
    fs::write(h.dir.join("notes.txt"), "hi").unwrap();
    let err = CoreError::from(h.open(true).unwrap_err());
    assert_eq!(err.code, ErrorCode::NotProject);
    assert!(
        !h.dir.join(".qrec").exists(),
        "nothing is created in someone else's folder"
    );

    // A folder holding only an interrupted init's .qrec/ counts as empty.
    let h = Harness::new("half");
    fs::create_dir(h.dir.join(".qrec")).unwrap();
    fs::write(h.dir.join(".qrec/lock"), "").unwrap();
    let mut s = h.open(true).unwrap();
    send(
        &mut s,
        Command::Init {
            canvas: Some("1280x720@25".parse().unwrap()),
            name: Some("Demo".into()),
        },
        false,
    )
    .unwrap();
    assert_eq!(
        (
            s.state().project.name.as_str(),
            s.state().project.canvas.fps
        ),
        ("Demo", Fps::new(25, 1))
    );
    drop(s);

    // A history without qrec.json: restore it rather than start over.
    fs::remove_file(h.dir.join("qrec.json")).unwrap();
    let err = CoreError::from(h.open(true).unwrap_err());
    assert_eq!(
        (err.code, err.message.as_str()),
        (ErrorCode::Invalid, "qrec.json is missing")
    );
    assert!(err.hint.unwrap().contains("qrec history restore 0 --yes"));

    let missing = h.env.root().join("projects/nope");
    let opts = OpenOpts {
        host: HostKind::Cli,
        mutating: true,
        recovery: false,
    };
    let err =
        CoreError::from(Session::open_with(&missing, opts, h.deps(), h.config()).unwrap_err());
    assert_eq!(err.code, ErrorCode::NotProject);
}

#[test]
fn read_only_sessions_take_no_lock_and_write_nothing() {
    let h = Harness::with_sample("ro");
    let mut ro = h.open(false).unwrap();
    assert!(!ro.is_host() && ro.lock().is_none());
    let hello = request(&mut ro, WireBody::Hello, false)
        .last()
        .unwrap()
        .unwrap();
    assert_eq!(hello["host"], "cli");
    let err = send(
        &mut ro,
        Command::ClipRemove {
            clips: vec![clip("c001")],
        },
        false,
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal);
    assert!(
        !h.dir.join(".qrec").exists(),
        "no lock file, no history, nothing"
    );
    // A host can open the same project meanwhile.
    let host = h.open(true).unwrap();
    assert!(host.is_host());
}

#[test]
fn two_hosts_exclude_each_other() {
    let h = Harness::with_sample("excl");
    let first = h.open(true).unwrap();
    let host = OpenOpts {
        host: HostKind::Cli,
        mutating: true,
        recovery: false,
    };
    let err = CoreError::from(h.open_once(host).unwrap_err());
    assert_eq!(err.code, ErrorCode::Busy);
    assert_eq!(err.message, "the project is open in another Qrec process");

    let record = LockRecord::starting(HostKind::Headless, Path::new("/x.sock"), Utc::now());
    lockfile::write(first.lock().unwrap(), &record).unwrap();
    let err = CoreError::from(h.open_once(host).unwrap_err());
    assert_eq!(
        err.message,
        format!(
            "the project is open in a headless host (pid {})",
            std::process::id()
        )
    );
    // Readers don't need the lock.
    assert!(h.open(false).is_ok());
    drop(first);
    assert!(h.open(true).is_ok(), "the lock goes with the session");
}

#[test]
fn requests_get_ids_and_hello_describes_the_host() {
    let h = Harness::with_sample("hello");
    let mut s = h.open(true).unwrap();
    let first = request(&mut s, WireBody::Hello, false);
    let info: HelloInfo = serde_json::from_value(first.last().unwrap().unwrap()).unwrap();
    assert_eq!(info.pid, std::process::id());
    assert_eq!(Path::new(&info.project_dir), s.dir());
    assert_eq!(
        (info.host, &info.record),
        (HostKind::Cli, &RecordPhase::Idle)
    );
    assert!(info.jobs.is_empty());
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(*first.accepted.lock().unwrap(), [Some(ReqId("r1".into()))]);
    let second = request(&mut s, WireBody::Hello, false);
    assert_eq!(*second.accepted.lock().unwrap(), [Some(ReqId("r2".into()))]);
    assert_eq!(current_request(), None, "only while a responder runs");

    let bye = request(&mut s, WireBody::Shutdown, false)
        .last()
        .unwrap()
        .unwrap();
    assert_eq!(bye["shuttingDown"], true);
    assert!(s.shutdown_requested());
    assert!(s.is_idle());
}

#[test]
fn a_purge_is_committed_before_anything_is_deleted() {
    let h = Harness::new("purge");
    let mut json = testdata::sample_json();
    json["takes"]["take-0002"] = testdata::take_json("take-0002", 60);
    json["nextTakeNumber"] = 3.into();
    write_project(&h.dir, &json);
    let take_dir = h.dir.join("takes/take-0002");
    fs::create_dir_all(&take_dir).unwrap();
    fs::write(take_dir.join("A1-mic.mka"), b"12345").unwrap();
    fs::write(take_dir.join("L1-screen-FAKE-1.mkv"), b"123").unwrap();
    let mut s = h.open(true).unwrap();

    let purge = Command::TakePurge {
        takes: vec![take(2)],
    };
    let err = send(&mut s, purge.clone(), false).unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfirmationRequired);
    let files = err.details.unwrap()["files"].as_array().unwrap().clone();
    assert_eq!(
        files.len(),
        2,
        "the take's files, with sizes, from the facts"
    );
    assert!(!h.log.has("delete"));

    let reply = send(&mut s, purge, true).unwrap();
    assert!(
        reply["message"].as_str().unwrap().starts_with("Purged"),
        "{reply}"
    );
    assert_eq!(
        h.log.lines().last().unwrap(),
        "delete take-0002 committed=true"
    );
    assert!(!s.state().project.takes.contains_key(&take(2)));
    let on_disk: Value =
        serde_json::from_slice(&fs::read(h.dir.join("qrec.json")).unwrap()).unwrap();
    assert!(on_disk["takes"].get("take-0002").is_none());
    assert!(s.state().runtime.jobs.is_empty(), "the purge job is done");
}

#[test]
fn a_crash_after_the_history_line_loses_only_that_command() {
    let h = Harness::with_sample("crash-history");
    let mut s = h.open(true).unwrap();
    send(
        &mut s,
        Command::ClipRemove {
            clips: vec![clip("c001")],
        },
        false,
    )
    .unwrap();
    s.store.crash_after = Some(Step::History);
    let answers = request(
        &mut s,
        WireBody::Command(Command::ClipRemove {
            clips: vec![clip("c002")],
        }),
        false,
    );
    assert!(
        answers.replies.lock().unwrap().is_empty(),
        "a dead process doesn't answer"
    );
    assert_eq!(h.history_seqs(), [1, 2], "the line made it to disk");
    drop(s);

    let s = h.open(true).unwrap();
    assert_eq!(
        clip_layout(&s),
        [("c002".to_owned(), 150, 300)],
        "the second command never happened"
    );
    assert_eq!(s.state().history.cursor, 1);
    assert_eq!(s.state().history.entries.len(), 1);
    assert_eq!(h.history_seqs(), [1], "the uncommitted line was cleaned up");
    assert!(
        h.toast_titles()
            .contains(&"Qrec repaired the project's files".to_owned())
    );
}

#[test]
fn a_crash_after_the_head_keeps_the_command() {
    let h = Harness::with_sample("crash-head");
    let mut s = h.open(true).unwrap();
    s.store.crash_after = Some(Step::Head);
    request(
        &mut s,
        WireBody::Command(Command::ClipRemove {
            clips: vec![clip("c001")],
        }),
        false,
    );
    drop(s);
    let s = h.open(true).unwrap();
    assert_eq!(clip_layout(&s), [("c002".to_owned(), 150, 300)]);
    assert_eq!(s.state().history.cursor, 1);
}

#[test]
fn a_crash_after_writing_qrec_json_is_merged_as_an_external_edit() {
    let h = Harness::with_sample("crash-project");
    let mut s = h.open(true).unwrap();
    s.store.crash_after = Some(Step::Project);
    request(
        &mut s,
        WireBody::Command(Command::ClipRemove {
            clips: vec![clip("c001")],
        }),
        false,
    );
    drop(s);

    let s = h.open(true).unwrap();
    assert_eq!(
        clip_layout(&s),
        [("c002".to_owned(), 150, 300)],
        "the file's change is kept"
    );
    let last = s.state().history.entries.last().unwrap();
    assert_eq!(last.label, "External edit");
    assert_eq!(h.head().cursor, last.seq);
    assert_eq!(
        fs::read(h.dir.join("qrec.json")).unwrap(),
        format::project_bytes(&s.state().project)
    );
}

#[test]
fn a_hand_edit_found_at_open_survives_the_repairs() {
    let h = Harness::with_sample("edit-and-torn");
    {
        let mut s = h.open(true).unwrap();
        send(
            &mut s,
            Command::ClipRemove {
                clips: vec![clip("c001")],
            },
            false,
        )
        .unwrap();
    }
    // A torn line from a crash, and a hand edit of qrec.json while no host ran.
    let mut history = fs::read(h.paths().history_file()).unwrap();
    history.extend_from_slice(b"{\"seq\":2,\"ts\"");
    fs::write(h.paths().history_file(), history).unwrap();
    let mut json: Value =
        serde_json::from_slice(&fs::read(h.dir.join("qrec.json")).unwrap()).unwrap();
    json["name"] = "edited while closed".into();
    write_project(&h.dir, &json);

    let s = h.open(true).unwrap();
    assert_eq!(s.state().project.name, "edited while closed");
    let labels: Vec<&str> = s
        .state()
        .history
        .entries
        .iter()
        .map(|e| e.label.as_str())
        .collect();
    assert_eq!(labels.last().copied(), Some("External edit"));
    assert_eq!(h.head().project.name, "edited while closed");
    assert_eq!(
        h.history_seqs(),
        s.state()
            .history
            .entries
            .iter()
            .map(|e| e.seq)
            .collect::<Vec<_>>()
    );
}

#[test]
fn external_edits_are_merged_and_undoable() {
    let h = Harness::with_sample("external");
    let mut s = h.open(true).unwrap();
    let mut json: Value =
        serde_json::from_slice(&fs::read(h.dir.join("qrec.json")).unwrap()).unwrap();
    json["name"] = "renamed by hand".into();
    write_project(&h.dir, &json);
    s.handle(Msg::DiskChanged);
    assert_eq!(s.state().project.name, "renamed by hand");
    assert_eq!(
        s.state().history.entries.last().unwrap().label,
        "External edit"
    );
    assert_eq!(h.head().project.name, "renamed by hand");
    // Our own write doesn't come back as an edit.
    let entries = s.state().history.entries.len();
    s.handle(Msg::DiskChanged);
    assert_eq!(s.state().history.entries.len(), entries);

    let undone = send(&mut s, Command::Undo { steps: 1 }, false).unwrap();
    assert_eq!(undone["seq"], 0);
    assert_eq!(s.state().project.name, "swamp");
    let on_disk: Value =
        serde_json::from_slice(&fs::read(h.dir.join("qrec.json")).unwrap()).unwrap();
    assert_eq!(on_disk["name"], "swamp");
}

#[test]
fn an_external_edit_goes_before_a_pending_command() {
    let h = Harness::with_sample("pre-check");
    let mut s = h.open(true).unwrap();
    let mut json: Value =
        serde_json::from_slice(&fs::read(h.dir.join("qrec.json")).unwrap()).unwrap();
    json["name"] = "edited".into();
    write_project(&h.dir, &json);
    // No DiskChanged yet: the command finds the edit before it writes.
    let reply = send(
        &mut s,
        Command::ClipRemove {
            clips: vec![clip("c001")],
        },
        false,
    )
    .unwrap();
    let labels: Vec<&str> = s
        .state()
        .history
        .entries
        .iter()
        .map(|e| e.label.as_str())
        .collect();
    assert_eq!(labels.first().copied(), Some("External edit"));
    assert_eq!(reply["seq"], 2);
    assert_eq!(s.state().project.name, "edited");
    assert_eq!(clip_layout(&s), [("c002".to_owned(), 150, 300)]);
}

#[test]
fn undo_and_redo_move_the_cursor_on_disk() {
    let h = Harness::with_sample("undo");
    let mut s = h.open(true).unwrap();
    for id in ["c001", "c002"] {
        send(
            &mut s,
            Command::ClipRemove {
                clips: vec![clip(id)],
            },
            false,
        )
        .unwrap();
    }
    let undone = send(&mut s, Command::Undo { steps: 2 }, false).unwrap();
    assert_eq!(undone["seq"], 0);
    assert_eq!(h.head().cursor, 0);
    assert_eq!(h.history_seqs(), [1, 2], "undo keeps the redo tail");
    assert_eq!(clip_layout(&s).len(), 2);
    drop(s);
    let mut s = h.open(true).unwrap();
    assert_eq!(s.state().history.cursor, 0);
    let redone = send(&mut s, Command::Redo { steps: 1 }, false).unwrap();
    assert_eq!(redone["seq"], 1);
    let split = send(
        &mut s,
        Command::ClipSplit {
            clip: Some(clip("c002")),
            at: "10f".parse().unwrap(),
            timeline: false,
        },
        false,
    )
    .unwrap();
    assert_eq!(split["seq"], 3, "seqs are never reused");
    assert_eq!(
        h.history_seqs(),
        [1, 3],
        "a new command drops the redo tail"
    );
    assert_eq!(h.head().cursor, 3);
}

#[test]
fn a_corrupt_qrec_json_fails_the_open_but_not_recovery_mode() {
    let h = Harness::with_sample("corrupt");
    {
        let mut s = h.open(true).unwrap();
        send(
            &mut s,
            Command::ClipRemove {
                clips: vec![clip("c001")],
            },
            false,
        )
        .unwrap();
    }
    fs::write(h.dir.join("qrec.json"), "{ oops").unwrap();
    let err = CoreError::from(h.open(true).unwrap_err());
    assert_eq!(err.code, ErrorCode::Invalid);
    assert!(
        err.hint
            .as_deref()
            .unwrap()
            .contains("qrec history restore 1 --yes"),
        "{err:?}"
    );
    let err = CoreError::from(h.open(false).unwrap_err());
    assert_eq!(
        err.code,
        ErrorCode::Invalid,
        "read-only opens fail the same way"
    );

    let s = h
        .open_opts(OpenOpts {
            host: HostKind::Cli,
            mutating: false,
            recovery: true,
        })
        .unwrap();
    assert_eq!(clip_layout(&s), [("c002".to_owned(), 150, 300)]);
    assert!(s.state().runtime.disk_invalid.is_some());
    assert_eq!(
        fs::read(h.dir.join("qrec.json")).unwrap(),
        b"{ oops",
        "never touched"
    );
}

#[test]
fn a_restore_rewrites_qrec_json_and_keeps_the_broken_one() {
    let h = Harness::with_sample("restore");
    {
        let mut s = h.open(true).unwrap();
        send(
            &mut s,
            Command::ClipRemove {
                clips: vec![clip("c001")],
            },
            false,
        )
        .unwrap();
    }
    fs::write(h.dir.join("qrec.json"), "{ oops").unwrap();
    let mut s = h
        .open_opts(OpenOpts {
            host: HostKind::Cli,
            mutating: true,
            recovery: true,
        })
        .unwrap();
    send(&mut s, Command::HistoryRestore { seq: 1 }, true).unwrap();
    assert_eq!(s.state().runtime.disk_invalid, None);
    assert_eq!(
        fs::read(h.dir.join("qrec.json")).unwrap(),
        format::project_bytes(&s.state().project)
    );
    let copies: Vec<_> = fs::read_dir(h.dir.join(".qrec"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("qrec.corrupt-"))
        .collect();
    assert_eq!(copies.len(), 1);
    assert_eq!(
        fs::read(h.dir.join(".qrec").join(&copies[0])).unwrap(),
        b"{ oops"
    );
}

fn recovery(mutating: bool) -> OpenOpts {
    OpenOpts {
        host: HostKind::Cli,
        mutating,
        recovery: true,
    }
}

/// Every file of the project but the lock and the logs, with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if relative == Path::new(".qrec/lock") || relative.starts_with(".qrec/logs") {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The `.qrec/qrec.corrupt-*` copies' contents.
fn corrupt_copies(h: &Harness) -> Vec<Vec<u8>> {
    fs::read_dir(h.dir.join(".qrec"))
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().starts_with("qrec.corrupt-"))
        .map(|e| fs::read(e.path()).unwrap())
        .collect()
}

#[test]
fn an_unconfirmed_restore_never_touches_the_files() {
    // Whatever the load finds to repair, a restore that doesn't run writes
    // nothing, and a confirmed one keeps the broken file's own bytes
    // (SPEC §11, §16; A§7.7).
    let broken: &[u8] = b"{\"version\":1,\"name\":\"my unsaved hand edit\", ";
    for case in [
        "torn-line",
        "uncommitted-line",
        "no-head",
        "nothing-to-repair",
    ] {
        let h = Harness::with_sample(&format!("unconfirmed-{case}"));
        {
            let mut s = h.open(true).unwrap();
            for id in ["c001", "c002"] {
                send(
                    &mut s,
                    Command::ClipRemove {
                        clips: vec![clip(id)],
                    },
                    false,
                )
                .unwrap();
            }
        }
        let history = h.paths().history_file();
        let mut lines = fs::read(&history).unwrap();
        match case {
            // A crash in the middle of an append…
            "torn-line" => lines.extend_from_slice(b"{\"seq\":3,\"ts\":\"2026-10-1"),
            // …or between the history line and the commit record.
            "uncommitted-line" => {
                let last = String::from_utf8(lines.clone()).unwrap();
                let mut entry: Value = serde_json::from_str(last.lines().last().unwrap()).unwrap();
                entry["seq"] = 3.into();
                lines.extend_from_slice(format!("{entry}\n").as_bytes());
            }
            "no-head" => fs::remove_file(h.paths().history_head()).unwrap(),
            _ => {}
        }
        fs::write(&history, lines).unwrap();
        fs::write(h.dir.join("qrec.json"), broken).unwrap();

        let before = snapshot(&h.dir);
        h.toasts.lock().unwrap().clear();
        let mut s = h.open_opts(recovery(true)).unwrap();
        let err = send(&mut s, Command::HistoryRestore { seq: 1 }, false).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfirmationRequired, "{case}");
        if case != "nothing-to-repair" {
            assert_eq!(
                h.toast_titles(),
                ["The project's files need repair"],
                "{case}: nothing is repaired yet"
            );
        }
        assert!(
            err.message
                .ends_with("the unusable qrec.json on disk is replaced (a copy goes to .qrec/)"),
            "{case}: {}",
            err.message
        );
        drop(s);
        assert!(snapshot(&h.dir) == before, "{case}: nothing was written");

        let mut s = h.open_opts(recovery(true)).unwrap();
        send(&mut s, Command::HistoryRestore { seq: 1 }, true).unwrap();
        assert_eq!(corrupt_copies(&h), [broken], "{case}: the user's own bytes");
        assert_eq!(
            fs::read(h.dir.join("qrec.json")).unwrap(),
            format::project_bytes(&s.state().project),
            "{case}"
        );
        drop(s);
        let s = h.open(true).unwrap();
        assert_eq!(s.state().history.cursor, 1, "{case}");
        assert_eq!(h.history_seqs(), [1, 2], "{case}: repaired by the restore");
    }
}

/// Records take 2 through the capture and probe events (the fake deps
/// record nothing) and returns its streams.
fn record_take_2(s: &mut Session) -> Vec<qrec_core::model::Stream> {
    let start = Command::RecordStart {
        mode: RecordModeArg::Append,
        countdown: Some(0),
        wait: false,
    };
    request(s, WireBody::Command(start), false);
    let planned = s.state().project.takes[&take(2)].streams.clone();
    let files: Vec<String> = planned.iter().map(|st| st.file.clone()).collect();
    s.handle(Msg::Event(Event::CaptureStarted {
        take: take(2),
        t0_ns: 0,
        files,
    }));
    request(s, WireBody::Command(Command::RecordStop), false);
    let streams = planned
        .iter()
        .map(|st| RecordedStream {
            id: st.id.clone(),
            file: st.file.clone(),
            start_offset_ns: 0,
            eos: true,
        })
        .collect();
    s.handle(Msg::Event(Event::CaptureStopped {
        take: take(2),
        streams,
        timed_out: false,
    }));
    s.handle(Msg::Event(Event::TakeProbed {
        take: take(2),
        purpose: ProbePurpose::Finalize,
        results: planned.iter().map(|st| probed(st, 90)).collect(),
    }));
    s.drain();
    assert_eq!(s.state().project.takes[&take(2)].status, TakeStatus::Ok);
    s.state().project.takes[&take(2)].streams.clone()
}

#[test]
fn a_restore_rebuilt_from_the_history_waits_for_the_recovered_takes() {
    let h = Harness::with_sample("rebuild");
    // Take 2 is recorded after the base (A§7.7's usual case): only qrec.json
    // and the commit record know its facts.
    let streams = {
        let mut s = h.open(true).unwrap();
        record_take_2(&mut s)
    };
    for st in &streams {
        fs::write(h.dir.join(&st.file), b"media").unwrap();
    }
    let head = h.head();
    let recorded = head.project.takes[&take(2)].clone();
    fs::write(h.dir.join("qrec.json"), b"garbage").unwrap();
    fs::remove_file(h.paths().history_head()).unwrap();
    let recover = |s: &mut Session| {
        assert!(h.log.has("probe take-0002 Recover"), "{:?}", h.log.lines());
        s.handle(Msg::Event(Event::TakeProbed {
            take: take(2),
            purpose: ProbePurpose::Recover,
            results: streams.iter().map(|st| probed(st, 90)).collect(),
        }));
        s.drain();
    };
    let restore = WireBody::Command(Command::HistoryRestore { seq: head.cursor });

    // Not confirmed: it waits for the probe, then asks; nothing is written.
    let before = snapshot(&h.dir);
    let mut s = h.open_opts(recovery(true)).unwrap();
    let answers = request(&mut s, restore.clone(), false);
    assert!(answers.last().is_none(), "waits for take 2's Recover probe");
    assert!(!s.is_idle());
    recover(&mut s);
    let err = answers.last().unwrap().unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfirmationRequired);
    drop(s);
    assert!(snapshot(&h.dir) == before, "nothing was written");

    // Confirmed: the restore sees the recovered take and writes a project
    // that validates, after keeping the broken file.
    h.log.clear();
    let mut s = h.open_opts(recovery(true)).unwrap();
    let answers = request(&mut s, restore, true);
    assert!(answers.last().is_none());
    recover(&mut s);
    answers.last().unwrap().unwrap();
    assert_eq!(corrupt_copies(&h), [b"garbage".to_vec()]);
    drop(s);

    let s = h.open(true).unwrap();
    let project = &s.state().project;
    qrec_core::validate::ensure_valid(project).unwrap();
    let t2 = &project.takes[&take(2)];
    assert_eq!((t2.status, t2.duration_frames), (TakeStatus::Ok, 90));
    assert_eq!(t2.scene, recorded.scene, "the scene it was recorded with");
    assert!(t2.placed);
    assert!(project.timeline.clips.iter().any(|c| c.take == take(2)));
    assert_eq!(s.state().history.cursor, head.cursor);
}

#[test]
fn a_restore_merges_a_valid_hand_edit_first() {
    let h = Harness::with_sample("restore-edit");
    {
        let mut s = h.open(true).unwrap();
        for id in ["c001", "c002"] {
            send(
                &mut s,
                Command::ClipRemove {
                    clips: vec![clip(id)],
                },
                false,
            )
            .unwrap();
        }
    }
    let mut json: Value =
        serde_json::from_slice(&fs::read(h.dir.join("qrec.json")).unwrap()).unwrap();
    json["name"] = "my hand edit".into();
    write_project(&h.dir, &json);

    let before = snapshot(&h.dir);
    let mut s = h.open_opts(recovery(true)).unwrap();
    let err = send(&mut s, Command::HistoryRestore { seq: 1 }, false).unwrap_err();
    assert_eq!(
        err.message,
        "confirmation required: restore entry 1 (\"Remove clip c001\"): undoes 2 history entries"
    );
    drop(s);
    assert!(snapshot(&h.dir) == before, "nothing was written");

    let mut s = h.open_opts(recovery(true)).unwrap();
    send(&mut s, Command::HistoryRestore { seq: 1 }, true).unwrap();
    assert!(corrupt_copies(&h).is_empty(), "a valid file isn't broken");
    assert!(
        !h.toast_titles()
            .contains(&"Kept the broken qrec.json".to_owned())
    );
    drop(s);
    // SPEC §8.1: the edit is an entry of its own, so it can be redone.
    let mut s = h.open(true).unwrap();
    let labels: Vec<&str> = s
        .state()
        .history
        .entries
        .iter()
        .map(|e| e.label.as_str())
        .collect();
    assert_eq!(labels.last().copied(), Some("External edit"));
    assert_eq!(s.state().history.cursor, 1);
    send(&mut s, Command::Redo { steps: 2 }, false).unwrap();
    assert_eq!(s.state().project.name, "my hand edit");
}

#[test]
fn a_reader_shows_a_hand_edit_without_making_up_history() {
    let h = Harness::with_sample("reader-edit");
    {
        let mut s = h.open(true).unwrap();
        send(
            &mut s,
            Command::ClipRemove {
                clips: vec![clip("c001")],
            },
            false,
        )
        .unwrap();
    }
    let mut json: Value =
        serde_json::from_slice(&fs::read(h.dir.join("qrec.json")).unwrap()).unwrap();
    json["recording"]["countdownSecs"] = 9.into();
    write_project(&h.dir, &json);
    let before = snapshot(&h.dir);

    for opts in [
        OpenOpts {
            host: HostKind::Cli,
            mutating: false,
            recovery: false,
        },
        recovery(false),
    ] {
        let s = h.open_opts(opts).unwrap();
        let state = s.state();
        assert_eq!(
            state.project.recording.countdown_secs, 9,
            "what the file says"
        );
        // No "External edit" entry that `qrec history` and restore don't know.
        assert_eq!(
            (state.history.cursor, state.history.next_seq),
            (1, 2),
            "{opts:?}"
        );
        assert_eq!(state.history.entries.len(), 1);
        assert_eq!(state.runtime.disk_invalid, None);
        drop(s);
    }
    assert!(h.toast_titles().is_empty(), "{:?}", h.toast_titles());
    assert!(snapshot(&h.dir) == before);

    // The next host records it.
    let s = h.open(true).unwrap();
    let last = s.state().history.entries.last().unwrap();
    assert_eq!((last.seq, last.label.as_str()), (2, "External edit"));
}

#[test]
fn takes_still_recording_are_probed_when_a_host_opens() {
    let h = Harness::new("recover");
    let mut json = testdata::sample_json();
    let mut recording = testdata::take_json("take-0002", 0);
    recording["status"] = "recording".into();
    for stream in recording["streams"].as_array_mut().unwrap() {
        let stream = stream.as_object_mut().unwrap();
        stream.retain(|k, _| ["id", "kind", "file", "startOffsetNs"].contains(&k.as_str()));
    }
    recording.as_object_mut().unwrap().remove("placed");
    json["takes"]["take-0002"] = recording;
    json["nextTakeNumber"] = 3.into();
    write_project(&h.dir, &json);

    let ro = h.open(false).unwrap();
    assert!(!h.log.has("probe"), "readers don't recover takes");
    drop(ro);
    let mut s = h.open(true).unwrap();
    assert!(
        h.log
            .has("probe take-0002 Recover takes/take-0002/L1-screen-FAKE-1.mkv,takes/take-0002/A1-mic.mka"),
        "{:?}",
        h.log.lines()
    );
    assert_eq!(
        s.state().project.takes[&take(2)].status,
        TakeStatus::Recording
    );
    // The probe is out: the host isn't idle, so even an in-process command
    // waits for it instead of exiting with the result undelivered (A§8.6).
    assert_eq!(
        s.state().runtime.recovering,
        [take(2)].into_iter().collect::<BTreeSet<_>>()
    );
    assert!(!s.is_idle());
    // A command meanwhile doesn't start a second probe.
    send(&mut s, Command::Undo { steps: 1 }, false).ok();
    assert_eq!(
        h.log
            .lines()
            .iter()
            .filter(|l| l.starts_with("probe "))
            .count(),
        1
    );

    // The result is applied and saved.
    let streams = s.state().project.takes[&take(2)].streams.clone();
    s.handle(Msg::Event(Event::TakeProbed {
        take: take(2),
        purpose: ProbePurpose::Recover,
        results: streams.iter().map(|st| probed(st, 90)).collect(),
    }));
    s.drain();
    assert!(s.state().runtime.recovering.is_empty());
    assert!(s.is_idle());
    assert_eq!(s.state().project.takes[&take(2)].status, TakeStatus::Ok);
    assert_eq!(h.head().project.takes[&take(2)].status, TakeStatus::Ok);
    assert!(
        h.toast_titles()
            .contains(&"Recovered take 2 from the bog.".to_owned()),
        "{:?}",
        h.toast_titles()
    );
}

#[test]
fn capabilities_are_known_before_the_first_dispatch() {
    let h = Harness::with_sample("caps");
    let s = h.open(false).unwrap();
    let caps = &s.state().runtime.capabilities;
    let ffmpeg = caps.ffmpeg.as_ref().expect("the dev shell has ffmpeg");
    assert!(ffmpeg.encoders.contains("libx264"));
    assert_eq!(caps.vaapi, None, "tests never look at a GPU");
    assert!(caps.cores >= 1);
    assert!(h.env.cache_home().join("qrec/capabilities.json").is_file());
}

#[test]
fn a_take_folder_that_exists_fails_the_capture_instead_of_starting_it() {
    let h = Harness::with_sample("take-dir");
    let mut s = h.open(true).unwrap();
    let plan = |n: u32| CapturePlan {
        take: take(n),
        dir: format!("takes/{}", take(n)),
        log: format!("takes/{}/recorder.log", take(n)),
        fps: Fps::new(30, 1),
        health_check_ms: 500,
        first_buffer_ms: 3000,
        eos_timeout_ms: 10_000,
        video: vec![],
        audio: vec![],
    };
    let begin = |s: &Session, n: u32| {
        let mut next = s.state().clone();
        let mut recording: qrec_core::model::Take =
            serde_json::from_value(testdata::take_json(take(n).as_str(), 0)).unwrap();
        recording.status = TakeStatus::Recording;
        next.project.takes.insert(take(n), recording);
        next.project.next_take_number = n + 1;
        Outcome::new(
            next,
            vec![
                Effect::CreateTakeDir {
                    path: format!("takes/{}", take(n)),
                },
                Effect::WriteProject,
                Effect::StartCapture { plan: plan(n) },
            ],
            None,
        )
    };

    fs::create_dir_all(h.dir.join("takes/take-0002")).unwrap();
    let outcome = begin(&s, 2);
    assert_eq!(
        s.run_outcome(outcome, Origin::Event, None, &mut None, false),
        Ran::Done
    );
    assert!(!h.log.has("start"), "never started");
    assert!(
        h.head().project.takes.contains_key(&take(2)),
        "saved before the failure is reported"
    );
    match s.rx.try_recv() {
        Ok(Msg::Event(Event::CaptureFailed {
            take: Some(t),
            message,
            ..
        })) => {
            assert_eq!(t, take(2));
            assert!(message.contains("already exists"), "{message}");
        }
        other => panic!("expected CaptureFailed, got {other:?}"),
    }

    let outcome = begin(&s, 3);
    s.run_outcome(outcome, Origin::Event, None, &mut None, false);
    assert!(h.dir.join("takes/take-0003").is_dir());
    assert_eq!(h.log.lines().last().unwrap(), "start take-0003");
    assert!(
        h.head().project.takes.contains_key(&take(3)),
        "committed before the capture started"
    );
}

#[test]
fn deleting_waits_for_a_durable_commit() {
    let h = Harness::with_sample("durable");
    let mut s = h.open(true).unwrap();
    let qrec = h.dir.join(".qrec");
    fs::set_permissions(&qrec, fs::Permissions::from_mode(0o500)).unwrap();
    let mut next = s.state().clone();
    next.project.name = "renamed".into();
    let outcome = Outcome::new(
        next,
        vec![
            Effect::WriteProject,
            Effect::DeleteTakeDirs {
                job: JobId(9),
                takes: vec![take(1)],
            },
        ],
        None,
    );
    s.run_outcome(outcome, Origin::Event, None, &mut None, false);
    fs::set_permissions(&qrec, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        !h.log.has("delete"),
        "nothing is deleted without a commit record"
    );
    match s.rx.try_recv() {
        Ok(Msg::Event(Event::TakeDirsDeleted {
            job,
            deleted,
            errors,
        })) => {
            assert_eq!(job, JobId(9));
            assert!(deleted.is_empty() && !errors.is_empty());
        }
        other => panic!("expected TakeDirsDeleted, got {other:?}"),
    }
    assert!(
        h.toast_titles()
            .contains(&"Couldn't save the project".to_owned())
    );
    assert!(s.store.is_dirty());

    // The next change saves everything again.
    send(
        &mut s,
        Command::ClipRemove {
            clips: vec![clip("c001")],
        },
        false,
    )
    .unwrap();
    assert!(!s.store.is_dirty());
    let head = h.head();
    assert_eq!(head.project.name, "renamed");
    assert_eq!(head.project, s.state().project);
}

/// Makes `dir` read-only; `false` (nothing to test) when that doesn't stop
/// this process from writing there (root).
fn make_read_only(dir: &Path) -> bool {
    fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
    let probe = dir.join(".write-probe");
    if fs::write(&probe, b"").is_ok() {
        let _ = fs::remove_file(&probe);
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        return false;
    }
    true
}

fn make_writable(dir: &Path) {
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_command_that_cant_be_saved_changes_nothing() {
    let h = Harness::with_sample("unsaved");
    let mut s = h.open(true).unwrap();
    let before = clip_layout(&s);
    if !make_read_only(&h.dir) {
        return;
    }
    // qrec.json can't be replaced (step 2): the command didn't happen.
    let err = send(
        &mut s,
        Command::ClipRemove {
            clips: vec![clip("c001")],
        },
        false,
    )
    .unwrap_err();
    make_writable(&h.dir);
    assert_eq!(err.code, ErrorCode::Io);
    assert!(
        err.message
            .starts_with("couldn't save the project, so nothing changed: write "),
        "{}",
        err.message
    );
    assert!(
        err.hint
            .as_deref()
            .unwrap()
            .ends_with("then run the command again"),
        "{err:?}"
    );
    assert_eq!(clip_layout(&s), before, "the state is as it was");
    assert_eq!(s.state().history.cursor, 0);
    // …and so are the files: the appended history line is gone again.
    assert_eq!(h.history_seqs(), Vec::<u64>::new());
    assert_eq!(h.head().project, s.state().project);
    assert!(!s.store.is_dirty());

    // The next change saves only itself: the dropped one never comes back.
    send(
        &mut s,
        Command::ClipRemove {
            clips: vec![clip("c002")],
        },
        false,
    )
    .unwrap();
    assert_eq!(clip_layout(&s), [("c001".to_owned(), 0, 100)]);
    assert_eq!(h.head().project, s.state().project);
    assert_eq!(h.history_seqs(), [1]);
    drop(s);
    let s = h.open(true).unwrap();
    assert_eq!(clip_layout(&s), [("c001".to_owned(), 0, 100)]);
}

#[test]
fn a_dropped_change_is_taken_back_out_of_qrec_json() {
    // CLI hosts exit right after replying; the next open must not find
    // the dropped change in qrec.json (it would merge it as an edit).
    let h = Harness::with_sample("unsaved-head");
    let mut s = h.open(true).unwrap();
    let qrec = h.dir.join(".qrec");
    if !make_read_only(&qrec) {
        return;
    }
    // The history line and qrec.json are written; the head (step 3) isn't.
    let err = send(
        &mut s,
        Command::ClipRemove {
            clips: vec![clip("c001")],
        },
        false,
    )
    .unwrap_err();
    make_writable(&qrec);
    assert!(
        err.message
            .starts_with("couldn't save the project, so nothing changed"),
        "{}",
        err.message
    );
    assert_eq!(
        fs::read(h.dir.join("qrec.json")).unwrap(),
        format::project_bytes(&s.state().project),
        "qrec.json describes the state again"
    );
    assert_eq!(s.state().project.timeline.clips.len(), 2);
    drop(s);

    let s = h.open(true).unwrap();
    assert_eq!(s.state().project.timeline.clips.len(), 2);
    assert!(
        s.state()
            .history
            .entries
            .iter()
            .all(|e| e.label != "External edit"),
        "{:?}",
        s.state().history.entries
    );
}

#[test]
fn a_dropped_first_commit_leaves_no_base_behind() {
    // The base written for the project's first commit describes that
    // commit's project; when the commit is dropped, so is the base.
    let h = Harness::new("unsaved-init");
    let mut s = h.open(true).unwrap();
    let init = |canvas: &str| Command::Init {
        canvas: Some(canvas.parse().unwrap()),
        name: None,
    };
    if !make_read_only(&h.dir) {
        return;
    }
    let err = send(&mut s, init("1280x720@25"), false).unwrap_err();
    make_writable(&h.dir);
    assert!(
        err.message
            .starts_with("couldn't save the project, so nothing changed"),
        "{}",
        err.message
    );
    assert!(!is_initialized(&s.state().project));
    assert!(!h.paths().history_base().exists());

    send(&mut s, init("640x360@30"), false).unwrap();
    let base: BaseFile =
        serde_json::from_slice(&fs::read(h.paths().history_base()).unwrap()).unwrap();
    assert_eq!(base.project, s.state().project);
    assert_eq!(base.project.canvas.width, 640);
}

#[test]
fn replies_after_an_unsaved_event_say_so() {
    // Events keep their facts in memory (saved with the next commit), and
    // a request they answer hears that it isn't saved yet.
    let h = Harness::with_sample("unsaved-event");
    let mut s = h.open(true).unwrap();
    let waiting = Answers::default();
    s.pending.insert(ReqId("r9".into()), waiting.responder());
    let mut next = s.state().clone();
    next.project.name = "renamed".into();
    let outcome = Outcome::new(
        next,
        vec![
            Effect::WriteProject,
            Effect::Reply {
                req: ReqId("r9".into()),
                reply: Ok(qrec_core::CmdResult::new("Done")),
            },
        ],
        None,
    );
    if !make_read_only(&h.dir) {
        return;
    }
    s.run_outcome(outcome, Origin::Event, None, &mut None, false);
    make_writable(&h.dir);
    let err = waiting.last().unwrap().unwrap_err();
    assert_eq!(err.code, ErrorCode::Io);
    assert!(
        err.message
            .starts_with("the change was made but couldn't be saved: "),
        "{}",
        err.message
    );
    assert!(
        err.hint
            .as_deref()
            .unwrap()
            .contains("loses it if it exits"),
        "{err:?}"
    );
    assert_eq!(s.state().project.name, "renamed", "kept in memory");
    assert!(s.store.is_dirty());
}

/// An in-process (or headless) host can't save a change "later": it exits
/// right after. A command whose commit fails gets the real cause and
/// changes nothing (it used to say "the change was made … it is saved
/// with the next change", then lose it).
#[test]
fn a_change_a_short_lived_host_cant_save_is_not_made() {
    let h = Harness::with_sample("unwritable");
    let mut s = h.open(true).unwrap();
    let before = s.state().clone();
    let qrec = h.dir.join(".qrec");
    let rename = |name: &str| Command::ProjectSet {
        name: Some(name.into()),
        canvas: None,
        background: None,
        countdown_secs: None,
        encoder: None,
        vaapi_device: qrec_core::protocol::Nullable::Keep,
    };
    fs::set_permissions(&qrec, fs::Permissions::from_mode(0o500)).unwrap();
    let reply = send(&mut s, rename("lost"), false);
    fs::set_permissions(&qrec, fs::Permissions::from_mode(0o755)).unwrap();
    let err = reply.unwrap_err();
    assert_eq!(err.code, ErrorCode::Io);
    assert!(
        err.message
            .starts_with("couldn't save the project, so nothing changed:"),
        "{err:?}"
    );
    assert!(err.message.contains("Permission denied"), "{err:?}");
    assert!(
        !err.hint.as_deref().unwrap().contains("next change"),
        "{err:?}"
    );
    assert_eq!(s.state().project, before.project, "not made in memory");
    assert_eq!(s.state().history, before.history);

    // The next change is the next seq, and the files hold no trace of the
    // lost one.
    send(&mut s, rename("kept"), false).unwrap();
    assert!(!s.store.is_dirty());
    let head = h.head();
    assert_eq!(head.project.name, "kept");
    assert_eq!(head.project, s.state().project);
    assert_eq!(h.history_seqs(), [before.history.next_seq]);
}

#[test]
fn ui_writes_are_coalesced() {
    let h = Harness::with_sample("ui");
    let mut s = h.open(true).unwrap();
    let select = |ids: &[&str]| Command::Select {
        op: SelectOp::Set(ids.iter().map(|i| clip(i)).collect()),
    };
    let ui = |h: &Harness| -> Value {
        serde_json::from_slice(&fs::read(h.paths().ui_file()).unwrap()).unwrap()
    };
    send(&mut s, select(&["c001"]), false).unwrap();
    assert_eq!(ui(&h)["selection"], serde_json::json!(["c001"]));
    send(&mut s, select(&["c002"]), false).unwrap();
    assert_eq!(
        ui(&h)["selection"],
        serde_json::json!(["c001"]),
        "coalesced"
    );
    let due = s.next_deadline().expect("a write is pending");
    while Instant::now() < due {
        std::thread::sleep(Duration::from_millis(20));
    }
    s.tick();
    assert_eq!(ui(&h)["selection"], serde_json::json!(["c002"]));
    assert_eq!(s.next_deadline(), None);
    assert_eq!(
        h.history_seqs(),
        Vec::<u64>::new(),
        "selection isn't undoable"
    );
    drop(s);
    let again = h.open(false).unwrap();
    assert_eq!(again.state().ui.selection, [clip("c002")]);
}

#[test]
fn an_invalid_file_is_moved_aside_before_facts_are_saved() {
    let h = Harness::with_sample("invalid");
    let mut s = h.open(true).unwrap();
    fs::write(h.dir.join("qrec.json"), "{ broken").unwrap();
    s.handle(Msg::DiskChanged);
    assert!(s.state().runtime.disk_invalid.is_some());
    assert!(
        h.toast_titles()
            .contains(&"qrec.json on disk is invalid".to_owned())
    );
    // The user's edits wait until it is fixed…
    let err = send(
        &mut s,
        Command::ClipRemove {
            clips: vec![clip("c001")],
        },
        false,
    )
    .unwrap_err();
    assert!(
        err.message.starts_with("qrec.json on disk is invalid"),
        "{}",
        err.message
    );
    assert_eq!(fs::read(h.dir.join("qrec.json")).unwrap(), b"{ broken");

    // …but facts are saved: the broken file goes to .qrec/rejected first.
    let mut next = s.state().clone();
    next.project.takes.get_mut(&take(1)).unwrap().status = TakeStatus::Broken;
    s.run_outcome(
        Outcome::new(next, vec![Effect::WriteProject], None),
        Origin::Event,
        None,
        &mut None,
        false,
    );
    let rejected: Vec<PathBuf> = fs::read_dir(h.dir.join(".qrec/rejected"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(rejected.len(), 1);
    assert_eq!(fs::read(&rejected[0]).unwrap(), b"{ broken");
    assert_eq!(
        fs::read(h.dir.join("qrec.json")).unwrap(),
        format::project_bytes(&s.state().project)
    );
    assert_eq!(
        s.state().runtime.disk_invalid,
        None,
        "the file is ours again"
    );
    assert!(
        h.toast_titles()
            .contains(&"Moved the invalid qrec.json aside".to_owned())
    );
}

#[test]
fn clearing_disk_invalid_keeps_a_copy_of_the_broken_file() {
    let h = Harness::with_sample("copy-aside");
    let mut s = h.open(true).unwrap();
    fs::write(h.dir.join("qrec.json"), "{ broken").unwrap();
    s.handle(Msg::DiskChanged);
    let mut next = s.state().clone();
    next.runtime.disk_invalid = None;
    let cursor = next.history.cursor;
    let effects = vec![
        Effect::History(HistoryOp::SetCursor(cursor)),
        Effect::WriteProject,
    ];
    s.run_outcome(
        Outcome::new(next, effects, None),
        Origin::Event,
        None,
        &mut None,
        false,
    );
    let copies: Vec<String> = fs::read_dir(h.dir.join(".qrec"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("qrec.corrupt-"))
        .collect();
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(
        fs::read(h.dir.join(".qrec").join(&copies[0])).unwrap(),
        b"{ broken"
    );
    assert_eq!(
        fs::read(h.dir.join("qrec.json")).unwrap(),
        format::project_bytes(&s.state().project)
    );
    assert!(
        h.toast_titles()
            .contains(&"Kept the broken qrec.json".to_owned())
    );
}

#[test]
fn an_interrupt_cancels_jobs_but_lets_a_purge_finish() {
    let h = Harness::with_sample("interrupt");
    let mut s = h.open(true).unwrap();
    let job = |id: u64, kind: JobKind| Job {
        id: JobId(id),
        kind,
        req: None,
        started: Utc::now(),
    };
    s.state.runtime.jobs.insert(
        JobId(5),
        job(
            5,
            JobKind::FrameSave {
                out: "frames/x.png".into(),
                files: BTreeSet::new(),
            },
        ),
    );
    s.state
        .runtime
        .jobs
        .insert(JobId(6), job(6, JobKind::Purge { takes: vec![] }));
    // (A second interrupt would SIGKILL every child of this test process.)
    s.handle(Msg::Interrupt);
    assert_eq!(s.interrupts(), 1);
    assert!(h.log.has("cancel job-5"));
    assert!(!h.log.has("cancel job-6"), "a purge finishes");
    s.state.runtime.jobs.clear();
}

#[test]
fn the_live_pipeline_follows_the_gui_from_the_start() {
    let h = Harness::with_sample("live");
    let gui = h
        .open_opts(OpenOpts {
            host: HostKind::Gui,
            mutating: true,
            recovery: false,
        })
        .unwrap();
    assert!(h.log.has("set_live true"), "{:?}", h.log.lines());
    drop(gui);
    assert!(h.log.has("shutdown"));
    h.log.clear();
    let cli = h.open(true).unwrap();
    assert!(!h.log.has("set_live"));
    drop(cli);
}

/// What ffprobe would say about one planned stream of `frames` frames at 30 fps.
fn probed(stream: &qrec_core::model::Stream, frames: u64) -> ProbeResult {
    let duration_ns = frames as i64 * 1_000_000_000 / 30;
    let info = match stream.kind {
        StreamKind::Video => MediaInfo {
            kind: StreamKind::Video,
            frames: Some(frames),
            fps: Some(Fps::new(30, 1)),
            width: Some(1280),
            height: Some(720),
            sample_rate: None,
            channels: None,
            duration_ns: Some(duration_ns),
            start_time_ns: 0,
            color_range: None,
            color_space: None,
        },
        StreamKind::Audio => MediaInfo {
            kind: StreamKind::Audio,
            frames: None,
            fps: None,
            width: None,
            height: None,
            sample_rate: Some(48_000),
            channels: Some(1),
            duration_ns: Some(duration_ns),
            start_time_ns: 0,
            color_range: None,
            color_space: None,
        },
    };
    ProbeResult {
        id: stream.id.clone(),
        file: stream.file.clone(),
        info: Ok(info),
    }
}

#[test]
fn a_take_is_committed_before_capture_and_finalized_from_events() {
    let h = Harness::with_sample("record");
    let mut s = h.open(true).unwrap();
    let start = Answers::default();
    s.handle(Msg::Request {
        req: Request::command(
            Command::RecordStart {
                mode: RecordModeArg::Append,
                countdown: Some(0),
                wait: false,
            },
            false,
        ),
        to: start.responder(),
    });
    s.drain();
    // The take folder, then the commit, then the capture (A§7.6).
    assert!(h.dir.join("takes/take-0002").is_dir());
    assert_eq!(
        h.head().project.takes[&take(2)].status,
        TakeStatus::Recording
    );
    assert!(h.log.has("start take-0002"), "{:?}", h.log.lines());
    assert!(start.last().is_none(), "answered once capture runs");

    let planned = s.state().project.takes[&take(2)].streams.clone();
    let files: Vec<String> = planned.iter().map(|st| st.file.clone()).collect();
    s.handle(Msg::Event(Event::CaptureStarted {
        take: take(2),
        t0_ns: 0,
        files: files.clone(),
    }));
    let started = start.last().unwrap().unwrap();
    assert_eq!(started["result"]["take"], "take-0002");

    let stop = Answers::default();
    s.handle(Msg::Request {
        req: Request::command(Command::RecordStop, false),
        to: stop.responder(),
    });
    assert!(h.log.has("stop take-0002 cancel=false"));
    let streams = planned
        .iter()
        .map(|st| RecordedStream {
            id: st.id.clone(),
            file: st.file.clone(),
            start_offset_ns: 0,
            eos: true,
        })
        .collect();
    s.handle(Msg::Event(Event::CaptureStopped {
        take: take(2),
        streams,
        timed_out: false,
    }));
    assert!(h.log.has("probe take-0002 Finalize"), "{:?}", h.log.lines());
    s.handle(Msg::Event(Event::TakeProbed {
        take: take(2),
        purpose: ProbePurpose::Finalize,
        results: planned.iter().map(|st| probed(st, 90)).collect(),
    }));
    s.drain();

    let finalized = stop.last().expect("the stop request hears back").unwrap();
    assert_eq!(finalized["result"]["take"], "take-0002", "{finalized}");
    assert_eq!(finalized["result"]["status"], "ok");
    assert!(s.state().runtime.record.is_idle());
    let head = h.head();
    assert_eq!(head.project.takes[&take(2)].status, TakeStatus::Ok);
    assert!(
        head.project
            .timeline
            .clips
            .iter()
            .any(|c| c.take == take(2))
    );
    let last = s.state().history.entries.last().unwrap();
    assert_eq!(last.label, "Record take 2");
    assert_eq!(h.history_seqs().last(), Some(&last.seq));
    assert_eq!(s.pending_requests(), 0);
}

#[test]
fn a_gone_requester_is_forgotten() {
    let h = Harness::with_sample("gone");
    let mut s = h.open(true).unwrap();
    let start = Answers::default();
    s.handle(Msg::Request {
        req: Request::command(
            Command::RecordStart {
                mode: RecordModeArg::Append,
                countdown: Some(0),
                wait: false,
            },
            false,
        ),
        to: start.responder(),
    });
    assert_eq!(s.pending_requests(), 1);
    let req = start.accepted.lock().unwrap()[0].clone().unwrap();
    s.handle(Msg::Event(Event::RequesterGone { req }));
    assert_eq!(s.pending_requests(), 0);
    s.handle(Msg::Event(Event::CaptureStarted {
        take: take(2),
        t0_ns: 0,
        files: vec![],
    }));
    assert!(start.last().is_none(), "nobody to answer");
    assert!(!s.state().runtime.record.is_idle(), "the recording goes on");
}

#[test]
fn a_session_on_its_own_thread_answers_through_run_until() {
    let h = Harness::with_sample("thread");
    let mut s = h.open(true).unwrap();
    let inbox = s.inbox();
    let answers = Answers::default();
    let done = answers.clone();
    let worker = std::thread::spawn(move || {
        s.run_until(|_| !done.replies.lock().unwrap().is_empty());
        s
    });
    inbox.send(Msg::Request {
        req: Request::command(
            Command::ClipRemove {
                clips: vec![clip("c001")],
            },
            false,
        ),
        to: answers.responder(),
    });
    let s = worker.join().unwrap();
    assert_eq!(answers.last().unwrap().unwrap()["seq"], 1);
    assert_eq!(clip_layout(&s), [("c002".to_owned(), 150, 300)]);
}
