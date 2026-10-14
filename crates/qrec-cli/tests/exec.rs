//! The CLI end to end over a fake backend: the folder rule, empty folders
//! (answers and lazy init), routing options, human and `--json` output,
//! streaming progress and notices, exit codes, Ctrl+C, doctor, sources,
//! config and the hidden host command.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use qrec_cli::*;
use qrec_core::error::NOT_MY_SWAMP;
use qrec_core::protocol::{
    HelloInfo, Progress, ProgressPhase, Toast, ToastLevel, UiAction, WireBody,
};
use qrec_core::state::{HostKind, RecordPhase};
use qrec_core::{CoreError, PROJECT_FILE};
use serde_json::{Value, json};

// ------------------------------------------------------------------ the fake

/// One scripted request: what the host reports, then how it ends.
struct Script {
    routed: Option<HelloInfo>,
    progress: Vec<Progress>,
    notices: Vec<Toast>,
    finish: Finish,
}

impl Script {
    fn reply(data: Value) -> Self {
        Self {
            routed: None,
            progress: vec![],
            notices: vec![],
            finish: Finish::Reply(Ok(data)),
        }
    }

    fn error(error: CoreError) -> Self {
        Self {
            finish: Finish::Reply(Err(error)),
            ..Self::reply(Value::Null)
        }
    }
}

/// A request the fake saw.
#[derive(Debug)]
struct Seen {
    dir: PathBuf,
    body: Value,
    confirmed: bool,
    opts: RouteOpts,
}

struct Fake {
    config_dir: PathBuf,
    hosts: Vec<HelloInfo>,
    scripts: VecDeque<Script>,
    seen: Vec<Seen>,
    doctor: Result<Value, CoreError>,
    sources: Result<Value, CoreError>,
    hosted: Vec<(PathBuf, HostArgs)>,
    logging: Option<(u8, bool)>,
}

impl Fake {
    fn new(config_dir: &Path) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            hosts: vec![],
            scripts: VecDeque::new(),
            seen: vec![],
            doctor: Err(CoreError::unavailable("no doctor")),
            sources: Err(CoreError::unavailable("no sources")),
            hosted: vec![],
            logging: None,
        }
    }

    fn script(mut self, script: Script) -> Self {
        self.scripts.push_back(script);
        self
    }

    fn commands(&self) -> Vec<Value> {
        self.seen.iter().map(|s| s.body.clone()).collect()
    }
}

impl Backend for Fake {
    fn logging(&mut self, verbose: u8, quiet: bool) {
        self.logging = Some((verbose, quiet));
    }

    fn config_dir(&mut self) -> Result<PathBuf, CoreError> {
        Ok(self.config_dir.clone())
    }

    fn live_hosts(&mut self) -> Result<Vec<HelloInfo>, CoreError> {
        Ok(self.hosts.clone())
    }

    fn request(
        &mut self,
        project: &Path,
        request: ProjectRequest,
        opts: &RouteOpts,
        events: &mut dyn Events,
    ) -> Finish {
        self.seen.push(Seen {
            dir: project.to_path_buf(),
            body: serde_json::to_value(&request.body).unwrap(),
            confirmed: request.confirmed,
            opts: opts.clone(),
        });
        let script = self.scripts.pop_front().unwrap_or_else(|| {
            Script::reply(
                json!({"seq": null, "created": [], "removed": [], "message": "ok", "result": null}),
            )
        });
        if opts.fallback == Fallback::None && script.routed.is_none() {
            return Finish::NoHost;
        }
        events.routed(script.routed.as_ref());
        events.accepted();
        for p in &script.progress {
            events.progress(p);
        }
        for t in &script.notices {
            events.notice(t);
        }
        script.finish
    }

    fn doctor(&mut self) -> Result<Value, CoreError> {
        self.doctor.clone()
    }

    fn sources(&mut self) -> Result<Value, CoreError> {
        self.sources.clone()
    }

    fn host(&mut self, project: &Path, args: &HostArgs) -> Result<u8, CoreError> {
        self.hosted.push((project.to_path_buf(), args.clone()));
        Ok(0)
    }
}

// ------------------------------------------------------------------ helpers

/// A temp dir (this crate has no tempfile dependency).
struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "qrec-cli-exec-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A folder holding `qrec.json`.
    fn project(&self, name: &str) -> PathBuf {
        let dir = self.dir(name);
        fs::write(dir.join(PROJECT_FILE), "{}").unwrap();
        dir
    }

    /// A folder with a file in it.
    fn busy(&self, name: &str) -> PathBuf {
        let dir = self.dir(name);
        fs::write(dir.join("notes.txt"), "").unwrap();
        dir
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Run {
    code: u8,
    out: String,
    err: String,
}

impl Run {
    /// The single JSON object on stdout.
    fn json(&self) -> Value {
        let lines: Vec<&str> = self.out.lines().collect();
        assert_eq!(lines.len(), 1, "one object on stdout:\n{}", self.out);
        serde_json::from_str(lines[0]).unwrap()
    }

    /// Every stdout line as JSON.
    fn json_lines(&self) -> Vec<Value> {
        self.out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
            .collect()
    }
}

fn run(fake: &mut Fake, cwd: &Path, line: &str) -> Run {
    let args: Vec<OsString> = line.split_whitespace().map(OsString::from).collect();
    let env = RunEnv {
        cwd: Some(cwd.to_path_buf()),
        err_tty: false,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with_backend(args, fake, &env, &mut out, &mut err);
    Run {
        code,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

fn hello(dir: &Path, host: HostKind, pid: u32) -> HelloInfo {
    HelloInfo {
        pid,
        project_dir: dir.display().to_string(),
        host,
        record: RecordPhase::Idle,
        jobs: vec![],
        version: "0.1.0".into(),
    }
}

fn result(message: &str, seq: Option<u64>, created: &[&str], payload: Value) -> Value {
    json!({"seq": seq, "created": created, "removed": [], "message": message, "result": payload})
}

fn progress(phase: ProgressPhase, done: u64, total: u64, label: &str) -> Progress {
    Progress {
        phase,
        done,
        total,
        eta_secs: Some(5),
        label: label.into(),
        take: None,
    }
}

// ------------------------------------------------------------- folder rule

#[test]
fn a_busy_folder_is_not_my_swamp() {
    let tmp = Tmp::new();
    let busy = tmp.busy("busy");
    let mut fake = Fake::new(&tmp.dir("cfg"));
    let run1 = run(&mut fake, &busy, "qrec clip list");
    assert_eq!(run1.code, 3);
    assert_eq!(run1.out, "");
    assert_eq!(
        run1.err,
        format!(
            "{NOT_MY_SWAMP}\nhint: run qrec in a folder with qrec.json or in an empty folder, or pass --project DIR\n"
        )
    );
    let json = run(&mut fake, &busy, "qrec --json init");
    assert_eq!(json.code, 3);
    assert_eq!(json.err, "", "--json keeps stderr for JSON lines");
    assert_eq!(json.json()["error"]["code"], "not_project");
    assert_eq!(json.json()["error"]["message"], NOT_MY_SWAMP);
    assert!(fake.seen.is_empty(), "nothing was sent");
}

#[test]
fn subfolders_point_at_their_project() {
    let tmp = Tmp::new();
    let project = tmp.project("proj");
    let sub = project.join("exports");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("old.mp4"), "").unwrap();
    let mut fake = Fake::new(&tmp.dir("cfg"));
    fake.hosts = vec![hello(Path::new("/elsewhere"), HostKind::Gui, 4)];
    let run1 = run(&mut fake, &sub, "qrec status");
    assert_eq!(run1.code, 3);
    assert_eq!(
        run1.err,
        format!(
            "{NOT_MY_SWAMP}\nhint: you're inside project {0}; run there or pass --project {0}\n",
            project.display()
        )
    );
    // --project relative to the current directory fixes it.
    let ok = run(&mut fake, &sub, "qrec --project .. clip remove c1");
    assert_eq!(ok.code, 0, "{}", ok.err);
    assert_eq!(fake.seen[0].dir, sub.join(".."));
}

#[test]
fn the_single_running_host_is_the_fallback() {
    let tmp = Tmp::new();
    let home = tmp.busy("home");
    let project = tmp.project("swamp");
    let mut fake = Fake::new(&tmp.dir("cfg"));
    fake.hosts = vec![hello(&project, HostKind::Gui, 77)];
    let run1 = run(&mut fake, &home, "qrec record toggle");
    assert_eq!(run1.code, 0, "{}", run1.err);
    assert_eq!(
        run1.err,
        format!("qrec: using project {} (pid 77)\n", project.display())
    );
    assert_eq!(fake.seen[0].dir, project);

    // Not with --project, and never with two hosts.
    let explicit = run(
        &mut fake,
        &tmp.0,
        &format!("qrec --project {} status", home.display()),
    );
    assert_eq!(explicit.code, 3);
    fake.hosts
        .push(hello(&tmp.0.join("b"), HostKind::Headless, 78));
    let two = run(&mut fake, &home, "qrec --json status");
    assert_eq!(two.code, 2);
    let message = two.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(
        message.starts_with("several Qrec hosts are running: "),
        "{message}"
    );
    assert!(message.contains("(headless host, pid 78)"), "{message}");
}

// ----------------------------------------------------------- empty folders

#[test]
fn queries_in_an_empty_folder_create_nothing() {
    let tmp = Tmp::new();
    let empty = tmp.dir("empty");
    let mut fake = Fake::new(&tmp.dir("cfg"));
    let status = run(&mut fake, &empty, "qrec status");
    assert_eq!(status.code, 0);
    assert_eq!(
        status.out,
        format!(
            "Project  none yet\nFolder   {}\nNote     this empty folder becomes a project with `qrec init` or on the first change\n",
            empty.display()
        )
    );
    let json = run(&mut fake, &empty, "qrec status --json");
    assert_eq!(
        json.json(),
        json!({"ok": true, "data": {"path": empty.display().to_string(), "initialized": false,
               "host": null, "pid": null, "project": null, "ui": null, "history": null, "runtime": null}})
    );
    assert_eq!(
        run(&mut fake, &empty, "qrec clip list").out,
        "The timeline is empty.\n"
    );
    assert_eq!(
        run(&mut fake, &empty, "qrec --json take list").json(),
        json!({"ok": true, "data": []})
    );
    assert_eq!(
        run(&mut fake, &empty, "qrec history --json").json(),
        json!({"ok": true, "data": []})
    );
    let missing = run(&mut fake, &empty, "qrec take show take-0001");
    assert_eq!(missing.code, 7);
    assert!(
        missing.err.starts_with("there is no take-0001\n"),
        "{}",
        missing.err
    );
    assert!(fake.seen.is_empty());
    assert_eq!(
        fs::read_dir(&empty).unwrap().count(),
        0,
        "nothing was written"
    );
}

#[test]
fn the_first_change_creates_the_project() {
    let tmp = Tmp::new();
    let empty = tmp.dir("empty");
    let mut fake = Fake::new(&tmp.dir("cfg"))
        .script(Script::reply(result(
            "Created project empty (1920x1080 @ 30 fps)",
            None,
            &[],
            Value::Null,
        )))
        .script(Script::reply(result(
            "Added layer L1 (Screen DP-4)",
            Some(1),
            &["L1"],
            Value::Null,
        )));
    let run1 = run(
        &mut fake,
        &empty,
        "qrec scene layer add screen --output DP-4",
    );
    assert_eq!(run1.code, 0, "{}", run1.err);
    assert_eq!(run1.out, "Added layer L1 (Screen DP-4)\n");
    assert_eq!(
        run1.err,
        "qrec: Created project empty (1920x1080 @ 30 fps)\n"
    );
    assert_eq!(
        fake.commands(),
        [
            json!({"command": {"cmd": "init", "canvas": null, "name": null}}),
            json!({"command": {"cmd": "layerAdd", "target": "project", "source": {"screen": {"output": "DP-4"}}, "name": null, "rect": null}}),
        ]
    );
    assert!(fake.seen.iter().all(|s| s.dir == empty && s.opts.lock));

    // A failed init stops there.
    let mut failing = Fake::new(&tmp.dir("cfg")).script(Script::error(CoreError::io("disk full")));
    let run2 = run(&mut failing, &empty, "qrec --json undo");
    assert_eq!(run2.code, 1);
    assert_eq!(run2.json()["error"]["message"], "disk full");
    assert_eq!(failing.seen.len(), 1);

    // A project that appeared meanwhile is used as it is.
    let mut raced = Fake::new(&tmp.dir("cfg"))
        .script(Script::error(CoreError::invalid("already a project")))
        .script(Script::reply(result(
            "Nothing undone",
            None,
            &[],
            Value::Null,
        )));
    let run3 = run(&mut raced, &empty, "qrec undo");
    assert_eq!(
        (run3.code, run3.out.as_str(), run3.err.as_str()),
        (0, "Nothing undone\n", "")
    );
    assert_eq!(raced.seen.len(), 2);
}

#[test]
fn init_is_sent_as_is() {
    let tmp = Tmp::new();
    let empty = tmp.dir("empty");
    let mut fake = Fake::new(&tmp.dir("cfg")).script(Script::reply(result(
        "Created project Swamp",
        None,
        &[],
        Value::Null,
    )));
    let run1 = run(
        &mut fake,
        &empty,
        "qrec init --canvas 1280x720@25 --name Swamp --json",
    );
    assert_eq!(run1.code, 0);
    assert_eq!(run1.json()["data"]["message"], "Created project Swamp");
    assert_eq!(
        fake.commands(),
        [json!({"command": {"cmd": "init", "canvas": "1280x720@25", "name": "Swamp"}})]
    );
}

#[test]
fn gui_commands_in_an_empty_folder_need_a_host() {
    let tmp = Tmp::new();
    let empty = tmp.dir("empty");
    let mut fake = Fake::new(&tmp.dir("cfg"));
    let run1 = run(&mut fake, &empty, "qrec seek --to 13f");
    assert_eq!(run1.code, 1);
    assert!(
        run1.err.starts_with("`qrec seek` needs the Qrec GUI"),
        "{}",
        run1.err
    );
    assert!(fake.seen.is_empty(), "no project was created for it");
}

// --------------------------------------------------------- replies, codes

#[test]
fn commands_print_their_message_or_the_json_result() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let split = || {
        Script::reply(result(
            "Split c7f3 at 13f into c7f3 and c9b2",
            Some(12),
            &["c9b2"],
            Value::Null,
        ))
    };
    let mut fake = Fake::new(&tmp.dir("cfg"))
        .script(split())
        .script(split())
        .script(split())
        .script(split());
    let human = run(&mut fake, &project, "qrec clip split c7f3 --at 13f");
    assert_eq!(
        (human.code, human.out.as_str(), human.err.as_str()),
        (0, "Split c7f3 at 13f into c7f3 and c9b2\n", "")
    );
    let quiet = run(&mut fake, &project, "qrec -q clip split c7f3 --at 13f");
    assert_eq!((quiet.out.as_str(), quiet.err.as_str()), ("", ""));
    let verbose = run(&mut fake, &project, "qrec -v clip split c7f3 --at 13f");
    assert_eq!(
        verbose.err,
        "qrec: no host runs this project: running it in this process\nqrec: history seq 12\nqrec: created c9b2\n"
    );
    let json = run(&mut fake, &project, "qrec clip split c7f3 --at 13f --json");
    assert_eq!(
        json.json(),
        json!({"ok": true, "data": {"seq": 12, "created": ["c9b2"], "removed": [], "message": "Split c7f3 at 13f into c7f3 and c9b2", "result": null}})
    );
    assert_eq!(fake.logging, Some((0, false)));
    assert!(fake.seen.iter().all(|s| !s.confirmed));
}

#[test]
fn errors_map_to_exit_codes() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let cases = [
        (
            CoreError::not_found("no clip c404").with_hint("`qrec clip list` shows the clips"),
            7,
        ),
        (CoreError::busy("recording take-0003"), 6),
        (CoreError::invalid("nothing to undo"), 1),
        (
            CoreError::new(qrec_core::ErrorCode::MissingDependency, "ffmpeg is missing"),
            5,
        ),
        (CoreError::usage("relative times only work for trims"), 2),
    ];
    for (error, code) in cases {
        let mut fake = Fake::new(&tmp.dir("cfg")).script(Script::error(error.clone()));
        let human = run(&mut fake, &project, "qrec undo");
        assert_eq!(human.code, code, "{error:?}");
        assert!(
            human.err.starts_with(&format!("{}\n", error.message)),
            "{}",
            human.err
        );
        let mut fake = Fake::new(&tmp.dir("cfg")).script(Script::error(error.clone()));
        let json = run(&mut fake, &project, "qrec undo --json");
        assert_eq!(json.code, code);
        assert_eq!(
            json.json(),
            json!({"ok": false, "error": serde_json::to_value(&error).unwrap()})
        );
    }
}

#[test]
fn confirmations_need_yes() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let refused = CoreError::confirmation_required("purge take-0003 (1 file, 4.0 MB)")
        .with_details(
            json!({"files": [{"path": "takes/take-0003/A1-mic.mka", "bytes": 4_000_000}],
                             "historyEntriesLost": 2, "oldestKept": "Add layer L1"}),
        );
    let mut fake = Fake::new(&tmp.dir("cfg"))
        .script(Script::error(refused.clone()))
        .script(Script::reply(result(
            "Purged take-0003 (4.0 MB)",
            None,
            &[],
            Value::Null,
        )));
    let no = run(&mut fake, &project, "qrec take purge take-0003");
    assert_eq!(no.code, 4);
    assert_eq!(
        no.err,
        "confirmation required: purge take-0003 (1 file, 4.0 MB)\n  takes/take-0003/A1-mic.mka  4.0 MB\n  history entries lost: 2 (oldest kept: Add layer L1)\nhint: pass --yes to confirm\n"
    );
    let yes = run(&mut fake, &project, "qrec take purge take-0003 --yes");
    assert_eq!(
        (yes.code, yes.out.as_str()),
        (0, "Purged take-0003 (4.0 MB)\n")
    );
    assert_eq!(
        fake.seen.iter().map(|s| s.confirmed).collect::<Vec<_>>(),
        [false, true]
    );
}

#[test]
fn export_streams_progress_then_one_object() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let done = || Script {
        routed: None,
        progress: vec![
            progress(ProgressPhase::Render, 0, 100, "Rendering clip 1/2"),
            progress(ProgressPhase::Render, 50, 100, "Rendering clip 2/2"),
            progress(ProgressPhase::Stitch, 100, 100, "Stitching"),
        ],
        notices: vec![
            Toast::new(
                ToastLevel::Success,
                "Layers! Like an onion. Your video is ready.",
            )
            .with_action("Copy path", UiAction::CopyText("/v/out.mp4".into())),
        ],
        finish: Finish::Reply(Ok(result(
            "Exported exports/p-20261014-120000.mp4",
            None,
            &[],
            json!({"path": "/v/p/exports/p-20261014-120000.mp4", "bytes": 2_500_000, "frames": 90}),
        ))),
    };
    let mut fake = Fake::new(&tmp.dir("cfg")).script(done()).script(done());
    let json = run(&mut fake, &project, "qrec export --json --out out.mp4");
    assert_eq!(json.code, 0);
    let lines = json.json_lines();
    assert_eq!(lines.len(), 4);
    assert_eq!(
        lines[0],
        json!({"event": "progress", "phase": "render", "done": 0, "total": 100, "etaSecs": 5, "label": "Rendering clip 1/2"})
    );
    assert_eq!(lines[3]["ok"], true);
    assert_eq!(lines[3]["data"]["result"]["frames"], 90);
    let notice: Value = serde_json::from_str(json.err.trim()).unwrap();
    assert_eq!(notice["event"], "notice");
    assert_eq!(notice["level"], "success");
    assert_eq!(
        fake.seen[0].body["command"]["out"],
        json!(project.join("out.mp4").display().to_string()),
        "--out is absolute"
    );

    let human = run(&mut fake, &project, "qrec export");
    assert_eq!(
        human.out,
        "Exported exports/p-20261014-120000.mp4\n/v/p/exports/p-20261014-120000.mp4 (2.5 MB, 90f)\n"
    );
    assert_eq!(
        human.err,
        "Rendering clip 1/2 [....................]   0%  ETA 5s\n\
         Rendering clip 2/2 [##########..........]  50%  ETA 5s\n\
         Stitching [####################] 100%  ETA 5s\n\
         qrec: Layers! Like an onion. Your video is ready.\n"
    );
}

#[test]
fn only_streaming_commands_print_progress_lines() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let finalizing = || Script {
        routed: None,
        // Repeated progress (an encoder filling up) prints once.
        progress: vec![
            progress(ProgressPhase::Finalize, 0, 0, "Finalizing take 1"),
            progress(ProgressPhase::Finalize, 0, 0, "Finalizing take 1"),
        ],
        notices: vec![],
        finish: Finish::Reply(Ok(result("Recorded take 1 (2.0 s)", Some(1), &[], json!(null)))),
    };
    let mut fake = Fake::new(&tmp.dir("cfg"))
        .script(finalizing())
        .script(finalizing())
        .script(finalizing());
    // `record stop` isn't a streaming command: exactly one object (SPEC §14) …
    let stop = run(&mut fake, &project, "qrec record stop --json");
    assert_eq!(stop.code, 0);
    assert_eq!(stop.json()["ok"], true);
    // … `record start --wait` is.
    let wait = run(&mut fake, &project, "qrec record start --wait --json");
    let lines = wait.json_lines();
    assert_eq!(lines.len(), 2, "{}", wait.out);
    assert_eq!(lines[0]["event"], "progress");
    // People see the progress on stderr either way.
    let human = run(&mut fake, &project, "qrec record stop");
    assert!(human.err.contains("Finalizing take 1"), "{}", human.err);
}

#[test]
fn ctrl_c_exits_130() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let mut fake = Fake::new(&tmp.dir("cfg")).script(Script {
        routed: None,
        progress: vec![progress(ProgressPhase::Render, 1, 10, "Rendering clip 1/1")],
        notices: vec![],
        finish: Finish::Interrupted,
    });
    let run1 = run(&mut fake, &project, "qrec export");
    assert_eq!(run1.code, EXIT_INTERRUPTED);
    assert_eq!(run1.out, "");
    assert!(
        run1.err.ends_with("qrec: warning: export interrupted\n"),
        "{}",
        run1.err
    );
}

#[test]
fn routing_options_follow_the_command() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let mut fake = Fake::new(&tmp.dir("cfg"));
    for line in [
        "qrec record start --wait",
        "qrec clip list",
        "qrec history restore 3 --yes",
        "qrec autocut --dry-run",
    ] {
        assert_eq!(run(&mut fake, &project, line).code, 0, "{line}");
    }
    let opts: Vec<&RouteOpts> = fake.seen.iter().map(|s| &s.opts).collect();
    assert_eq!(opts[0].fallback, Fallback::SpawnHeadless);
    assert!(opts[0].stop_on_interrupt && opts[0].lock);
    assert_eq!(opts[0].deadline, RECORD_DEADLINE);
    assert!(!opts[1].lock, "queries take no lock");
    assert!(opts[2].recovery && opts[2].lock);
    assert!(fake.seen[2].confirmed);
    assert_eq!(opts[3].fallback, Fallback::InProcess);
    assert_eq!(opts[3].deadline, DEADLINE);
}

#[test]
fn selection_commands_need_a_live_host() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let mut fake = Fake::new(&tmp.dir("cfg"));
    let run1 = run(
        &mut fake,
        &project,
        "qrec record start --over-selection --json",
    );
    assert_eq!(run1.code, 1);
    let error = &run1.json()["error"];
    assert_eq!(error["code"], "unavailable");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("--over-selection"),
        "{error}"
    );

    let gui = hello(&project, HostKind::Gui, 5);
    let mut fake = Fake::new(&tmp.dir("cfg")).script(Script {
        routed: Some(gui),
        ..Script::reply(result("Playing", None, &[], Value::Null))
    });
    let play = run(&mut fake, &project, "qrec play");
    assert_eq!((play.code, play.out.as_str()), (0, "Playing\n"));
}

#[test]
fn status_for_people_and_for_programs() {
    let tmp = Tmp::new();
    let project = tmp.project("swamp");
    let view = json!({
        "project": "swamp", "path": null, "initialized": true, "host": "gui", "pid": null,
        "record": {"phase": "idle"}, "jobs": [], "takes": 2, "clips": 3,
        "durationFrames": 900, "cursor": 4
    });
    let state = json!({"project": {"name": "swamp"}, "ui": {}, "history": {"cursor": 4}, "runtime": {"host": "gui"}});
    let gui = hello(&project, HostKind::Gui, 31);
    let mut fake = Fake::new(&tmp.dir("cfg"))
        .script(Script {
            routed: Some(gui.clone()),
            ..Script::reply(view)
        })
        .script(Script {
            routed: Some(gui),
            ..Script::reply(state)
        })
        .script(Script::reply(
            json!({"project": {}, "ui": {}, "history": {}, "runtime": {}}),
        ));
    let human = run(&mut fake, &project, "qrec status");
    assert_eq!(
        human.out,
        format!(
            "Project    swamp\nFolder     {}\nHost       gui (pid 31)\nRecording  idle\nJobs       none\nTimeline   3 clips, 900f\nTakes      2\nHistory    at seq 4\n",
            project.display()
        )
    );
    let json = run(&mut fake, &project, "qrec status --json").json();
    assert_eq!(json["data"]["host"], "gui");
    assert_eq!(json["data"]["pid"], 31);
    assert_eq!(json["data"]["initialized"], true);
    assert_eq!(json["data"]["path"], project.display().to_string());
    assert_eq!(json["data"]["history"]["cursor"], 4);
    let in_process = run(&mut fake, &project, "qrec status --json").json();
    assert_eq!(in_process["data"]["host"], "cli");
    assert_eq!(in_process["data"]["pid"], std::process::id());
}

#[test]
fn queries_render_tables() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let mut fake = Fake::new(&tmp.dir("cfg"))
        .script(Script::reply(json!([
            {"id": "c001", "take": "take-0001", "in": 0, "out": 60, "start": 0, "duration": 60}
        ])))
        .script(Script::reply(json!({"unexpected": true})));
    let clips = run(&mut fake, &project, "qrec clip list");
    assert_eq!(
        clips.out,
        "#  CLIP  TAKE       IN  OUT  START  LENGTH\n0  c001  take-0001  0f  60f     0f     60f\n1 clip, 60f\n"
    );
    // A shape this CLI doesn't know is shown as JSON rather than dropped.
    let odd = run(&mut fake, &project, "qrec clip list");
    assert_eq!(odd.out, "{\n  \"unexpected\": true\n}\n");
}

#[test]
fn autocut_dry_runs_show_the_plan() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let plan = json!({
        "clips": [{"clip": "c001", "kept": [[0, 20], [40, 60]], "removedFrames": 20, "silences": 1}],
        "skipped": [{"clip": "c002", "reason": "entirely silent at -35 dB"}]
    });
    let mut fake = Fake::new(&tmp.dir("cfg"))
        .script(Script::reply(result(
            "Autocut preview: 1 silence (0.7 s)",
            None,
            &[],
            plan.clone(),
        )))
        .script(Script::reply(result(
            "Autocut take 1: removed 1 silence (0.7 s)",
            Some(5),
            &["c003"],
            plan,
        )));
    let dry = run(&mut fake, &project, "qrec -q autocut --dry-run");
    assert_eq!(
        dry.out,
        "CLIP  SILENCES  REMOVED  KEPT (take frames)\nc001         1      20f  0-20 40-60\nskipped c002: entirely silent at -35 dB\nAutocut preview: 1 silence (0.7 s)\n"
    );
    let real = run(&mut fake, &project, "qrec autocut");
    assert_eq!(
        real.out,
        "Autocut take 1: removed 1 silence (0.7 s)\nskipped c002: entirely silent at -35 dB\n"
    );
    assert_eq!(fake.seen[1].body["command"]["scope"], "all");
}

#[test]
fn record_replies_say_what_happens() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let mut fake = Fake::new(&tmp.dir("cfg"))
        .script(Script::reply(result(
            "Recording take-0002",
            None,
            &["take-0002"],
            json!({"take": "take-0002", "mode": "append", "files": ["takes/take-0002/L1-screen-FAKE-1.mkv"]}),
        )))
        .script(Script::reply(result(
            "Recorded take-0002 (90 frames)",
            Some(3),
            &["c004"],
            json!({"take": "take-0002", "status": "ok", "durationFrames": 90, "placed": true, "clips": ["c004"]}),
        )));
    let start = run(&mut fake, &project, "qrec record start");
    assert_eq!(
        start.out,
        "Recording take-0002\nWriting takes/take-0002/L1-screen-FAKE-1.mkv\nStop with `qrec record stop` (or `qrec record cancel` to bin it).\n"
    );
    let stop = run(&mut fake, &project, "qrec record stop");
    assert_eq!(stop.out, "Recorded take-0002 (90 frames)\nClips: c004\n");
}

#[test]
fn closed_stdout_is_not_a_crash() {
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let mut fake = Fake::new(&tmp.dir("cfg")).script(Script::reply(json!([])));
    let env = RunEnv {
        cwd: Some(project.clone()),
        err_tty: false,
    };
    let mut err = Vec::new();
    let code = run_with_backend(
        ["qrec", "clip", "list"].map(OsString::from),
        &mut fake,
        &env,
        &mut Closed,
        &mut err,
    );
    assert_eq!(code, 0);
    assert!(err.is_empty());
}

// ------------------------------------------------------------ project-less

#[test]
fn doctor_reports_and_exits_5_when_required_pieces_are_missing() {
    let tmp = Tmp::new();
    let mut fake = Fake::new(&tmp.dir("cfg"));
    fake.doctor = Ok(json!({
        "checks": [{"id": "ffmpeg", "required": true, "ok": true, "detail": "8.1.2", "hint": null}],
        "niriSnippet": "binds {\n    Mod+Shift+R { spawn \"qrec\" \"record\" \"toggle\"; }\n}",
        "va": null
    }));
    let ok = run(&mut fake, &tmp.0, "qrec doctor");
    assert_eq!(ok.code, 0);
    assert!(
        ok.out
            .contains("Mod+Shift+R { spawn \"qrec\" \"record\" \"toggle\"; }"),
        "{}",
        ok.out
    );
    assert_eq!(
        run(&mut fake, &tmp.0, "qrec doctor --json").json()["ok"],
        true
    );

    fake.doctor = Ok(
        json!({"checks": [{"id": "x264enc", "required": true, "ok": false,
                                         "detail": "GStreamer element missing", "hint": "install gst-plugins-ugly"}]}),
    );
    let missing = run(&mut fake, &tmp.0, "qrec doctor");
    assert_eq!(missing.code, 5);
    assert!(
        missing.out.contains("x264enc: install gst-plugins-ugly"),
        "{}",
        missing.out
    );
    let json = run(&mut fake, &tmp.0, "qrec doctor --json");
    assert_eq!(json.code, 5);
    assert_eq!(json.json()["error"]["code"], "missing_dependency");
    assert_eq!(
        json.json()["error"]["details"]["checks"][0]["id"],
        "x264enc"
    );
}

#[test]
fn sources_list() {
    let tmp = Tmp::new();
    let mut fake = Fake::new(&tmp.dir("cfg"));
    fake.sources = Ok(
        json!({"screens": [], "cameras": [], "mics": [], "monitors": [],
                            "defaultMic": null, "defaultMonitor": null}),
    );
    let run1 = run(&mut fake, &tmp.0, "qrec sources list");
    assert_eq!(
        run1.out,
        "Screens\n  none found\nCameras\n  none found\nMics\n  none found\nSystem audio\n  none found\n"
    );
    assert_eq!(
        run(&mut fake, &tmp.0, "qrec sources list --json").json()["data"]["screens"],
        json!([])
    );
    fake.sources = Err(CoreError::new(
        qrec_core::ErrorCode::MissingDependency,
        "pactl is missing",
    ));
    assert_eq!(run(&mut fake, &tmp.0, "qrec sources list").code, 5);
}

#[test]
fn config_reads_defaults_and_the_file() {
    let tmp = Tmp::new();
    let cfg = tmp.dir("cfg");
    let mut fake = Fake::new(&cfg);
    let all = run(&mut fake, &tmp.0, "qrec config");
    assert_eq!(all.code, 0, "{}", all.err);
    assert!(
        all.out.starts_with(&format!(
            "# {}\ntheme = swamp-night\n",
            cfg.join("config.toml").display()
        )),
        "{}",
        all.out
    );
    assert!(all.out.contains("canvas.fps = 30\n"), "{}", all.out);
    assert!(
        all.out.contains("encoder.vaapi_device = (not set)\n"),
        "{}",
        all.out
    );

    fs::write(
        cfg.join("config.toml"),
        "# mine\ntheme = \"dragons-keep\"\n[canvas]\nfps = \"30000/1001\"\n[autocut]\nthreshold_db = -40.0\n",
    )
    .unwrap();
    assert_eq!(
        run(&mut fake, &tmp.0, "qrec config get theme").out,
        "dragons-keep\n"
    );
    assert_eq!(
        run(&mut fake, &tmp.0, "qrec config get canvas.fps").out,
        "30000/1001\n"
    );
    assert_eq!(
        run(
            &mut fake,
            &tmp.0,
            "qrec config get autocut.threshold_db --json"
        )
        .json(),
        json!({"ok": true, "data": {"key": "autocut.threshold_db", "value": -40.0}})
    );
    let unknown = run(&mut fake, &tmp.0, "qrec config get canvas.fsp");
    assert_eq!(unknown.code, 2);
    assert!(
        unknown.err.contains("unknown setting \"canvas.fsp\""),
        "{}",
        unknown.err
    );

    fs::write(cfg.join("config.toml"), "[canvas]\nfsp = 25\n").unwrap();
    let invalid = run(&mut fake, &tmp.0, "qrec config");
    assert_eq!(invalid.code, 1);
    assert!(invalid.err.contains("fsp"), "{}", invalid.err);
    fs::write(cfg.join("config.toml"), "theme = [").unwrap();
    let broken = run(&mut fake, &tmp.0, "qrec config get theme");
    assert_eq!(broken.code, 1);
    assert!(broken.err.contains("is not valid TOML"), "{}", broken.err);
}

#[test]
fn config_set_keeps_comments() {
    let tmp = Tmp::new();
    let cfg = tmp.dir("cfg");
    fs::write(
        cfg.join("config.toml"),
        "# my settings\ntheme = \"swamp-day\"\n",
    )
    .unwrap();
    let mut fake = Fake::new(&cfg);
    let set = run(
        &mut fake,
        &tmp.0,
        "qrec config set autocut.threshold_db -40",
    );
    assert_eq!(
        (set.code, set.out.as_str()),
        (0, "Set autocut.threshold_db = -40.0\n"),
        "{}",
        set.err
    );
    let text = fs::read_to_string(cfg.join("config.toml")).unwrap();
    assert!(text.starts_with("# my settings\n"), "{text}");
    assert_eq!(
        run(&mut fake, &tmp.0, "qrec config get autocut.threshold_db").out,
        "-40.0\n"
    );
    let bad = run(
        &mut fake,
        &tmp.0,
        "qrec config set autocut.threshold_db loud",
    );
    assert_ne!(bad.code, 0);
    assert_eq!(
        fs::read_to_string(cfg.join("config.toml")).unwrap(),
        text,
        "unchanged on error"
    );
}

#[test]
fn keys_print_the_effective_keymap() {
    let tmp = Tmp::new();
    let cfg = tmp.dir("cfg");
    fs::write(
        cfg.join("keys.toml"),
        "[bindings]\n\"frame.save\" = \"f\"\n",
    )
    .unwrap();
    let mut fake = Fake::new(&cfg);
    let human = run(&mut fake, &tmp.0, "qrec keys");
    assert_eq!(human.code, 0, "{}", human.err);
    assert!(human.out.contains("qrec record toggle"), "{}", human.out);
    let json = run(&mut fake, &tmp.0, "qrec keys --json").json();
    let rows = json["data"].as_array().unwrap();
    let save = rows.iter().find(|r| r["id"] == "frame.save").unwrap();
    assert_eq!(save["keys"], json!(["f"]));
}

#[test]
fn the_host_command_gets_an_absolute_project() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let mut fake = Fake::new(&tmp.dir("cfg"));
    let run1 = run(
        &mut fake,
        &tmp.0,
        "qrec host --headless --project p --lock-fd 3 --ready-fd 4",
    );
    assert_eq!(run1.code, 0, "{}", run1.err);
    assert_eq!(fake.hosted.len(), 1);
    assert_eq!(fake.hosted[0].0, project);
    assert_eq!(
        (fake.hosted[0].1.lock_fd, fake.hosted[0].1.ready_fd),
        (Some(3), Some(4))
    );
}

#[test]
fn the_body_on_the_wire_is_the_core_protocol() {
    let tmp = Tmp::new();
    let project = tmp.project("p");
    let mut fake = Fake::new(&tmp.dir("cfg"));
    run(&mut fake, &project, "qrec clip move c1 c2 --by -1");
    let body: WireBody = serde_json::from_value(fake.seen[0].body.clone()).unwrap();
    assert!(matches!(body, WireBody::Command(_)));
}
