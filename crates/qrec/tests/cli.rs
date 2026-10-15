//! `qrec` end to end: the real binary in an isolated environment
//! (`TestEnv`: temp HOME/XDG dirs, a private runtime dir and
//! `QREC_CAPTURE_BACKEND=fake`), with ffmpeg-generated fixture media.
//! Project commands run through the shell linked into the binary: an
//! in-process `cli` host per command, or (recording) a spawned headless
//! host that later commands are routed to.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use qrec_core::error::NOT_MY_SWAMP;
use qrec_shell::testkit::{MediaSpec, TestEnv, ffprobe, fixture_project, run_tool};
use serde_json::{Value, json};

/// What one `qrec` run printed.
#[derive(Debug)]
struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Out {
    /// The final `{"ok":…}` object (the last stdout line).
    fn reply(&self) -> Value {
        let last = self
            .stdout
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or_else(|| panic!("no output; stderr:\n{}", self.stderr));
        serde_json::from_str(last).unwrap_or_else(|e| panic!("{last}: {e}"))
    }

    /// Exactly one JSON object on stdout.
    fn only_reply(&self) -> Value {
        assert_eq!(
            self.stdout.lines().count(),
            1,
            "--json prints one object:\n{}",
            self.stdout
        );
        self.reply()
    }

    /// Every stdout line as JSON (progress events, then the reply).
    fn json_lines(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
            .collect()
    }

    #[track_caller]
    fn ok(&self) -> &Self {
        assert_eq!(
            self.code,
            Some(0),
            "stdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        self
    }
}

/// The `qrec` under test: Cargo's, unless `$CARGO_BIN_EXE_qrec` is set when
/// the tests run (a copy of this build's binary, for checkouts that share
/// one target dir, where `target/debug/qrec` is whichever built last).
fn qrec_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_qrec")
        .map_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_qrec")), PathBuf::from)
}

/// Runs `qrec <args>` in `dir`, with a timeout.
fn qrec(env: &TestEnv, dir: &Path, args: &[&str]) -> Out {
    qrec_with(env, dir, args, &[])
}

/// [`qrec`] with extra environment variables (e.g. a `PATH` without ffmpeg).
fn qrec_with(env: &TestEnv, dir: &Path, args: &[&str], vars: &[(&str, &OsStr)]) -> Out {
    let mut command = env.command(&qrec_bin());
    command
        .current_dir(dir)
        .args(args)
        .envs(vars.iter().copied());
    let out = run_tool(&mut command, Duration::from_secs(120));
    Out {
        code: out.status.code(),
        stdout: String::from_utf8(out.stdout).expect("stdout is UTF-8"),
        stderr: out.stderr,
    }
}

/// A project whose one take is `spec` (clip `c001`, take `take-0001`).
fn project_with(env: &TestEnv, name: &str, specs: &[MediaSpec]) -> PathBuf {
    let dir = env.empty_dir(name);
    fixture_project(&dir, specs);
    dir
}

fn video(frames: u64) -> MediaSpec {
    MediaSpec::video(320, 180, 30, frames).with_audio(48_000, 1, &[(0, 60_000)])
}

fn seq(out: &Out) -> u64 {
    out.reply()["data"]["seq"]
        .as_u64()
        .unwrap_or_else(|| panic!("no seq in {}", out.stdout))
}

fn created(out: &Out) -> Vec<String> {
    out.reply()["data"]["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

fn clip_ids(env: &TestEnv, dir: &Path) -> Vec<String> {
    let out = qrec(env, dir, &["clip", "list", "--json"]);
    out.ok();
    out.reply()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect()
}

// ------------------------------------------------------- works without the shell

#[test]
fn version_help_and_usage_errors() {
    let env = TestEnv::new();
    let dir = env.empty_dir("any");
    let version = qrec(&env, &dir, &["--version"]);
    version.ok();
    assert_eq!(
        version.stdout,
        format!("qrec {}\n", env!("CARGO_PKG_VERSION"))
    );
    let json = qrec(&env, &dir, &["version", "--json"]);
    assert_eq!(
        json.only_reply(),
        json!({"ok": true, "data": {"version": env!("CARGO_PKG_VERSION")}})
    );
    let help = qrec(&env, &dir, &["--help"]);
    help.ok();
    assert!(help.stdout.contains("EXIT CODES"), "{}", help.stdout);
    // Under --json, help and the version are one object too (SPEC §14).
    for args in [&["--json", "--version"][..], &["--json", "-V"]] {
        let out = qrec(&env, &dir, args);
        out.ok();
        assert_eq!(
            out.only_reply(),
            json!({"ok": true, "data": {"version": env!("CARGO_PKG_VERSION")}}),
            "{args:?}"
        );
    }
    let json_help = qrec(&env, &dir, &["--json", "--help"]);
    json_help.ok();
    assert_eq!(json_help.only_reply()["data"], help.stdout.as_str());

    let usage = qrec(&env, &dir, &["clip", "split", "c1", "--json"]);
    assert_eq!(usage.code, Some(2));
    assert_eq!(usage.stderr, "");
    assert_eq!(usage.only_reply()["error"]["code"], "usage");
    let bad_time = qrec(&env, &dir, &["--json", "clip", "split", "c1", "--at", "13"]);
    assert_eq!(bad_time.code, Some(2), "a bare number is not a time");
    assert!(
        bad_time.only_reply()["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("412f")
    );
    let completions = qrec(&env, &dir, &["completions", "zsh"]);
    completions.ok();
    assert!(completions.stdout.contains("qrec"));
}

#[test]
fn a_folder_with_other_files_is_not_my_swamp() {
    let env = TestEnv::new();
    let dir = env.empty_dir("busy");
    fs::write(dir.join(".envrc"), "use flake\n").unwrap();
    for args in [
        &["status"][..],
        &["init"],
        &["clip", "split", "c1", "--at", "1f"],
    ] {
        let out = qrec(&env, &dir, args);
        assert_eq!(out.code, Some(3), "{args:?}: {out:?}");
        assert_eq!(out.stdout, "");
        assert_eq!(out.stderr.lines().next(), Some(NOT_MY_SWAMP), "{args:?}");
    }
    let json = qrec(&env, &dir, &["--json", "init"]);
    assert_eq!(json.code, Some(3));
    assert_eq!(json.stderr, "", "--json keeps stderr for JSON lines");
    let error = &json.only_reply()["error"];
    assert_eq!(error["code"], "not_project");
    assert_eq!(error["message"], NOT_MY_SWAMP);
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        1,
        "nothing was written"
    );
}

#[test]
fn inside_a_project_the_hint_names_it() {
    let env = TestEnv::new();
    let project = env.empty_dir("swamp");
    fs::write(project.join("qrec.json"), "{}").unwrap();
    let sub = project.join("exports");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("old.mp4"), "").unwrap();
    let out = qrec(&env, &sub, &["clip", "list"]);
    assert_eq!(out.code, Some(3));
    assert_eq!(
        out.stderr,
        format!(
            "{NOT_MY_SWAMP}\nhint: you're inside project {0}; run there or pass --project {0}\n",
            project.display()
        )
    );
    let missing = qrec(&env, &sub, &["--project", "nope", "status"]);
    assert_eq!(missing.code, Some(3));
    assert!(
        missing.stderr.contains("doesn't exist"),
        "{}",
        missing.stderr
    );
}

#[test]
fn an_empty_folder_answers_queries_without_writing() {
    let env = TestEnv::new();
    let dir = env.empty_dir("fresh");
    let status = qrec(&env, &dir, &["status", "--json"]);
    status.ok();
    let data = &status.only_reply()["data"];
    assert_eq!(data["initialized"], false);
    assert_eq!(data["path"], dir.display().to_string());
    let human = qrec(&env, &dir, &["status"]);
    human.ok();
    assert!(
        human.stdout.starts_with("Project  none yet\n"),
        "{}",
        human.stdout
    );
    for (args, empty) in [
        (&["clip", "list", "--json"][..], json!([])),
        (&["take", "list", "--json"], json!([])),
        (&["history", "--json"], json!([])),
    ] {
        let out = qrec(&env, &dir, args);
        out.ok();
        assert_eq!(
            out.only_reply(),
            json!({"ok": true, "data": empty}),
            "{args:?}"
        );
    }
    let missing = qrec(&env, &dir, &["take", "show", "take-0001", "--json"]);
    assert_eq!(missing.code, Some(7));
    assert_eq!(missing.only_reply()["error"]["code"], "not_found");
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        0,
        "queries write nothing"
    );
}

#[test]
fn config_lives_in_the_isolated_config_home() {
    let env = TestEnv::new();
    let dir = env.empty_dir("any");
    let theme = qrec(&env, &dir, &["config", "get", "theme"]);
    theme.ok();
    assert_eq!(theme.stdout, "swamp-night\n");
    let config_dir = env.config_home().join("qrec");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "theme = \"ogre-mode\"\n[autocut]\nthreshold_db = -42.5\n",
    )
    .unwrap();
    let all = qrec(&env, &dir, &["config", "--json"]);
    all.ok();
    let data = &all.only_reply()["data"];
    assert_eq!(
        data["file"],
        config_dir.join("config.toml").display().to_string()
    );
    assert_eq!(data["config"]["theme"], "ogre-mode");
    assert_eq!(data["config"]["autocut"]["threshold_db"], -42.5);
    assert_eq!(
        data["config"]["canvas"]["width"], 1920,
        "defaults fill the rest"
    );
    let unknown = qrec(&env, &dir, &["config", "get", "nope"]);
    assert_eq!(unknown.code, Some(2));
}

// ------------------------------------------------------------ project commands

#[test]
fn init_and_status_shapes() {
    let env = TestEnv::new();
    let dir = env.empty_dir("my-video");
    let init = qrec(&env, &dir, &["init", "--canvas", "1280x720@25", "--json"]);
    init.ok();
    let data = &init.only_reply()["data"];
    assert!(
        data["message"].as_str().unwrap().contains("my-video"),
        "{data}"
    );
    assert_eq!(data["result"]["canvas"]["width"], 1280);
    for entry in ["qrec.json", "takes", ".qrec"] {
        assert!(dir.join(entry).exists(), "{entry} is missing");
    }
    let again = qrec(&env, &dir, &["init", "--json"]);
    assert_eq!(again.code, Some(1));
    assert_eq!(again.only_reply()["error"]["code"], "invalid");

    let status = qrec(&env, &dir, &["status", "--json"]);
    status.ok();
    let data = &status.only_reply()["data"];
    assert_eq!(data["initialized"], true);
    // No host runs (the human view says "none running"): `host` and `pid`
    // name only a process that holds the project, never the reader itself.
    assert_eq!(data["host"], Value::Null, "{data}");
    assert_eq!(data["pid"], Value::Null, "{data}");
    assert_eq!(data["project"]["name"], "my-video");
    assert_eq!(
        data["project"]["canvas"]["fps"],
        json!({"num": 25, "den": 1})
    );
    assert_eq!(data["runtime"]["record"]["phase"], "idle");
    assert_eq!(data["history"]["cursor"], 0);
    let human = qrec(&env, &dir, &["status"]);
    human.ok();
    assert!(
        human.stdout.starts_with("Project    my-video\n"),
        "{}",
        human.stdout
    );
    assert!(
        human.stdout.contains("\nHost       none running;"),
        "{}",
        human.stdout
    );
}

#[test]
fn the_first_change_creates_the_project() {
    let env = TestEnv::new();
    let dir = env.empty_dir("lazy");
    let add = qrec(
        &env,
        &dir,
        &[
            "scene", "layer", "add", "screen", "--output", "FAKE-1", "--json",
        ],
    );
    add.ok();
    assert_eq!(created(&add), ["L1"]);
    assert!(dir.join("qrec.json").is_file());
    let notice: Value = serde_json::from_str(add.stderr.lines().next().unwrap()).unwrap();
    assert_eq!(notice["event"], "notice");
}

/// SPEC §4.4: only a change that sets a project up (or a recording) makes
/// an empty folder a project; `export` or `undo` there write nothing.
#[test]
fn commands_on_nothing_leave_an_empty_folder_empty() {
    let env = TestEnv::new();
    let dir = env.empty_dir("untouched");
    for args in [&["--json", "export"][..], &["--json", "undo"]] {
        let out = qrec(&env, &dir, args);
        assert_eq!(out.code, Some(3), "{args:?}: {out:?}");
        let error = &out.only_reply()["error"];
        assert_eq!(error["code"], "not_project");
        assert_eq!(error["message"], "this folder has no Qrec project yet");
    }
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        0,
        "nothing was written"
    );
}

#[test]
fn a_change_that_cant_be_saved_is_not_made() {
    // An in-process CLI host exits right after it replies: a change it
    // couldn't save is gone, so the reply must not say it was made and
    // gets saved "with the next change".
    use std::os::unix::fs::PermissionsExt;
    let mode = |dir: &Path, mode: u32| {
        fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
    };
    let env = TestEnv::new();
    let dir = env.empty_dir("ro");
    qrec(&env, &dir, &["init", "-q"]).ok();
    qrec(&env, &dir, &["project", "set", "--encoder", "x264", "-q"]).ok();
    mode(&dir, 0o555);
    if fs::write(dir.join(".probe"), b"").is_ok() {
        // Root writes anyway: nothing to test.
        let _ = fs::remove_file(dir.join(".probe"));
        return mode(&dir, 0o755);
    }
    let out = qrec(
        &env,
        &dir,
        &["project", "set", "--name", "renamed", "--json"],
    );
    mode(&dir, 0o755);
    assert_eq!(out.code, Some(1), "{out:?}");
    let error = out.only_reply()["error"].clone();
    assert_eq!(error["code"], "io");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.starts_with("couldn't save the project, so nothing changed: write "),
        "{message}"
    );
    assert!(
        error["hint"]
            .as_str()
            .unwrap()
            .ends_with("run the command again"),
        "{error}"
    );
    assert!(
        !error.to_string().contains("saved with the next"),
        "{error}"
    );

    // The next command finds the project as it was, and saves only itself.
    qrec(&env, &dir, &["project", "set", "--countdown", "1", "-q"]).ok();
    let status = qrec(&env, &dir, &["status", "--json"]);
    status.ok();
    assert_eq!(status.only_reply()["data"]["project"]["name"], "ro");
    let history = qrec(&env, &dir, &["history", "--json"]);
    history.ok();
    let labels: Vec<String> = history.reply()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["label"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(labels.len(), 2, "{labels:?}");
    assert!(!labels.iter().any(|l| l.contains("renamed")), "{labels:?}");
}

#[test]
fn scene_commands_return_seqs_and_ids() {
    let env = TestEnv::new();
    let dir = env.empty_dir("scene");
    qrec(&env, &dir, &["init"]).ok();
    let screen = qrec(
        &env,
        &dir,
        &[
            "scene", "layer", "add", "screen", "--output", "FAKE-1", "--json",
        ],
    );
    screen.ok();
    assert_eq!((seq(&screen), created(&screen)), (1, vec!["L1".to_owned()]));
    let camera = qrec(
        &env,
        &dir,
        &[
            "scene",
            "layer",
            "add",
            "camera",
            "--device",
            "/dev/fake0",
            "--json",
        ],
    );
    camera.ok();
    assert_eq!((seq(&camera), created(&camera)), (2, vec!["L2".to_owned()]));
    let mic = qrec(&env, &dir, &["scene", "audio", "add", "mic", "--json"]);
    mic.ok();
    assert_eq!(created(&mic), ["A1"]);
    let set = qrec(
        &env,
        &dir,
        &[
            "scene",
            "layer",
            "set",
            "L2",
            "--opacity",
            "0.5",
            "--rect",
            "-10,0,320,240",
            "--json",
        ],
    );
    set.ok();
    assert_eq!(seq(&set), 4);
    qrec(&env, &dir, &["scene", "layer", "order", "L2", "--to", "0"]).ok();
    qrec(
        &env,
        &dir,
        &[
            "scene",
            "audio",
            "set",
            "A1",
            "--gain-db",
            "-3",
            "--sync-offset-ms",
            "-80",
        ],
    )
    .ok();
    let unknown = qrec(
        &env,
        &dir,
        &[
            "scene", "layer", "add", "screen", "--output", "NOPE-9", "--json",
        ],
    );
    assert_eq!(unknown.code, Some(7));
    let status = qrec(&env, &dir, &["status", "--json"]);
    let scene = &status.reply()["data"]["project"]["scene"];
    assert_eq!(scene["layers"][0]["id"], "L2", "moved to the bottom");
    assert_eq!(scene["layers"][0]["opacity"], 0.5);
    assert_eq!(scene["audio"][0]["gainDb"], -3.0);
    let remove = qrec(&env, &dir, &["scene", "layer", "remove", "L1", "--json"]);
    remove.ok();
}

#[test]
fn clip_edits_return_seqs_and_created_ids() {
    let env = TestEnv::new();
    let dir = project_with(&env, "clips", &[video(90), video(60)]);
    assert_eq!(clip_ids(&env, &dir), ["c001", "c002"]);

    let split = qrec(
        &env,
        &dir,
        &["clip", "split", "c001", "--at", "30f", "--json"],
    );
    split.ok();
    assert_eq!(seq(&split), 1);
    let new = created(&split);
    assert_eq!(new.len(), 1);
    assert_eq!(clip_ids(&env, &dir), ["c001", new[0].as_str(), "c002"]);

    let moved = qrec(&env, &dir, &["clip", "move", "c002", "--to", "0", "--json"]);
    moved.ok();
    assert_eq!(seq(&moved), 2);
    assert_eq!(clip_ids(&env, &dir), ["c002", "c001", new[0].as_str()]);

    // c001 is [0, 30): +2f moves the start in, -3f moves the end back.
    let trim = qrec(
        &env,
        &dir,
        &[
            "clip", "trim", "c001", "--start", "+2f", "--end", "-3f", "--json",
        ],
    );
    trim.ok();
    assert_eq!(seq(&trim), 3);
    let rows = qrec(&env, &dir, &["clip", "list", "--json"]).reply()["data"].clone();
    let c001 = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "c001")
        .unwrap();
    assert_eq!(
        (c001["in"].as_u64(), c001["out"].as_u64()),
        (Some(2), Some(27))
    );

    let remove = qrec(&env, &dir, &["clip", "remove", "c002", "--json"]);
    remove.ok();
    assert_eq!(remove.reply()["data"]["removed"], json!(["c002"]));
    let add = qrec(
        &env,
        &dir,
        &["clip", "add", "take-0002", "--at", "0", "--json"],
    );
    add.ok();
    assert_eq!(seq(&add), 5);
    assert_eq!(created(&add).len(), 1);

    let human = qrec(&env, &dir, &["clip", "list"]);
    human.ok();
    assert!(human.stdout.starts_with("#  CLIP"), "{}", human.stdout);
}

#[test]
fn undo_redo_and_history() {
    let env = TestEnv::new();
    let dir = project_with(&env, "history", &[video(90)]);
    qrec(&env, &dir, &["clip", "split", "c001", "--at", "30f"]).ok();
    qrec(&env, &dir, &["clip", "split", "c001", "--at", "10f"]).ok();
    assert_eq!(clip_ids(&env, &dir).len(), 3);

    let undo = qrec(&env, &dir, &["undo", "--json"]);
    undo.ok();
    assert_eq!(seq(&undo), 1);
    assert_eq!(clip_ids(&env, &dir).len(), 2);
    let redo = qrec(&env, &dir, &["redo", "--json"]);
    redo.ok();
    assert_eq!(seq(&redo), 2);
    let both = qrec(&env, &dir, &["undo", "2", "--json"]);
    both.ok();
    assert_eq!(seq(&both), 0);
    assert_eq!(clip_ids(&env, &dir), ["c001"]);
    let too_many = qrec(&env, &dir, &["undo", "5", "--json"]);
    assert_eq!(too_many.code, Some(1));
    assert_eq!(too_many.reply()["ok"], false);

    let history = qrec(&env, &dir, &["history", "--json"]);
    history.ok();
    let rows = history.reply()["data"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|r| r["current"] == false),
        "everything is undone"
    );
    let restore = qrec(&env, &dir, &["history", "restore", "2", "--json"]);
    assert_eq!(restore.code, Some(4), "restore needs --yes");
    assert_eq!(restore.reply()["error"]["code"], "confirmation_required");
    qrec(&env, &dir, &["history", "restore", "2", "--yes"]).ok();
    assert_eq!(clip_ids(&env, &dir).len(), 3);
    let human = qrec(&env, &dir, &["history"]);
    human.ok();
    assert!(human.stdout.contains("Split clip c001"), "{}", human.stdout);
    // An empty window would read "No history yet.": like `undo 0`, a
    // usage error.
    let none = qrec(&env, &dir, &["history", "--limit", "0"]);
    assert_eq!(none.code, Some(2), "{none:?}");
    assert!(!none.stdout.contains("No history yet."));
    let newest = qrec(&env, &dir, &["history", "--limit", "1", "--json"]);
    assert_eq!(newest.reply()["data"].as_array().unwrap().len(), 1);
}

#[test]
fn bad_ids_are_not_found() {
    let env = TestEnv::new();
    let dir = project_with(&env, "ids", &[video(30)]);
    for args in [
        &["clip", "split", "c404", "--at", "1f"][..],
        &["clip", "remove", "c404"],
        &["take", "show", "take-0099"],
        &["scene", "layer", "set", "L9", "--opacity", "0.5"],
        &["clip", "add", "take-0099"],
    ] {
        let out = qrec(&env, &dir, args);
        assert_eq!(out.code, Some(7), "{args:?}: {out:?}");
    }
}

#[test]
fn purging_a_take_needs_yes() {
    let env = TestEnv::new();
    let dir = project_with(&env, "purge", &[video(30), video(30)]);
    let in_use = qrec(
        &env,
        &dir,
        &["take", "purge", "take-0001", "--yes", "--json"],
    );
    assert_ne!(in_use.code, Some(0), "a take with clips can't be purged");
    qrec(&env, &dir, &["clip", "remove", "c001"]).ok();
    let refused = qrec(&env, &dir, &["take", "purge", "take-0001", "--json"]);
    assert_eq!(refused.code, Some(4));
    let error = &refused.only_reply()["error"];
    assert_eq!(error["code"], "confirmation_required");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("confirmation required: "),
        "{error}"
    );
    assert!(!error["details"]["files"].as_array().unwrap().is_empty());
    assert!(dir.join("takes/take-0001").is_dir(), "nothing deleted yet");
    qrec(&env, &dir, &["take", "purge", "take-0001", "--yes"]).ok();
    assert!(!dir.join("takes/take-0001").exists());
    assert!(dir.join("takes/take-0002").is_dir());
    let takes = qrec(&env, &dir, &["take", "list", "--json"]);
    assert_eq!(takes.reply()["data"].as_array().unwrap().len(), 1);
}

/// A take whose folder was moved to another disk and linked back can't be
/// purged: the purge never follows a symlink, so it would delete nothing.
/// It is refused (`invalid`, exit 1) before anything changes, with or
/// without `--yes`, instead of a confirmation listing files it won't delete
/// and a purge that drops the take and its history while they stay
/// (SPEC §11, A§5.6). Once the link is gone, the purge goes ahead.
#[test]
fn a_take_whose_folder_is_a_link_is_not_purged_halfway() {
    let env = TestEnv::new();
    let dir = project_with(&env, "purge-link", &[video(30), video(30)]);
    let disk = env.root().join("disk");
    fs::create_dir(&disk).unwrap();
    qrec(&env, &dir, &["clip", "remove", "c002"]).ok();
    fs::rename(dir.join("takes/take-0002"), disk.join("take-0002")).unwrap();
    symlink(disk.join("take-0002"), dir.join("takes/take-0002")).unwrap();
    let files = names_in(&disk.join("take-0002"));
    let history = qrec(&env, &dir, &["history", "--json"]).reply()["data"].clone();

    for args in [
        &["take", "purge", "take-0002", "--json"][..],
        &["take", "purge", "take-0002", "--yes", "--json"],
    ] {
        let out = qrec(&env, &dir, args);
        assert_eq!(out.code, Some(1), "{args:?}: {out:?}");
        let error = &out.only_reply()["error"];
        assert_eq!(error["code"], "invalid", "{error}");
        assert_eq!(
            error["message"],
            "can't purge take 2 from disk: takes/take-0002/ is a symlink (not followed)"
        );
        assert_eq!(
            error["details"]["left"],
            json!(["takes/take-0002/ is a symlink (not followed)"])
        );
    }
    assert_eq!(names_in(&disk.join("take-0002")), files, "nothing deleted");
    let takes = qrec(&env, &dir, &["take", "list", "--json"]);
    assert_eq!(takes.reply()["data"].as_array().unwrap().len(), 2);
    assert_eq!(
        qrec(&env, &dir, &["history", "--json"]).reply()["data"],
        history,
        "the history is kept"
    );

    // Its files deleted by hand and the link removed: the purge drops it.
    fs::remove_file(dir.join("takes/take-0002")).unwrap();
    let out = qrec(&env, &dir, &["take", "purge", "take-0002", "--json"]);
    assert_eq!(out.code, Some(4), "{out:?}");
    assert_eq!(out.only_reply()["error"]["details"]["files"], json!([]));
    qrec(&env, &dir, &["take", "purge", "take-0002", "--yes"]).ok();
    let takes = qrec(&env, &dir, &["take", "list", "--json"]);
    assert_eq!(takes.reply()["data"].as_array().unwrap().len(), 1);
}

/// A purge that can't be saved deletes nothing, changes nothing and says
/// so (A§5.6, A§7.6). It used to reply "purged take 1 from the project, but
/// some files could not be deleted … delete the leftover files by hand"
/// while the project still listed the take.
#[test]
fn a_purge_that_cant_be_saved_deletes_nothing_and_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |dir: &Path, mode: u32| {
        fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
    };
    let env = TestEnv::new();
    let dir = project_with(&env, "purge-ro", &[video(30), video(30)]);
    qrec(&env, &dir, &["clip", "remove", "c001", "-q"]).ok();
    let state = dir.join(".qrec");
    mode(&state, 0o555);
    if fs::write(state.join(".probe"), b"").is_ok() {
        // Root writes anyway: nothing to test.
        let _ = fs::remove_file(state.join(".probe"));
        return mode(&state, 0o755);
    }
    let out = qrec(
        &env,
        &dir,
        &["--json", "take", "purge", "take-0001", "--yes"],
    );
    mode(&state, 0o755);
    assert_eq!(out.code, Some(1), "{out:?}");
    let error = out.only_reply()["error"].clone();
    assert_eq!(error["code"], "io");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.starts_with("couldn't save the project, so nothing changed: "),
        "{error}"
    );
    assert!(!error.to_string().contains("leftover"), "{error}");
    assert!(dir.join("takes/take-0001").is_dir(), "nothing deleted");
    let row = take_row(&env, &dir, "take-0001");
    assert_eq!(row["unused"], true, "still in the bin: {row}");

    // Once the folder is writable again the purge goes through.
    qrec(&env, &dir, &["take", "purge", "take-0001", "--yes", "-q"]).ok();
    assert!(!dir.join("takes/take-0001").exists());
}

#[test]
fn autocut_dry_run_then_apply() {
    let env = TestEnv::new();
    // 3 s at 30 fps; speech 0-1 s and 2-3 s: one 1 s silence.
    let spec = MediaSpec::video(320, 180, 30, 90).with_audio(48_000, 1, &[(0, 1000), (2000, 3000)]);
    let dir = project_with(&env, "autocut", &[spec]);

    let dry = qrec(&env, &dir, &["autocut", "--dry-run", "--json"]);
    dry.ok();
    let plan = &dry.reply()["data"]["result"];
    let clip = &plan["clips"][0];
    assert_eq!(clip["clip"], "c001");
    assert_eq!(clip["silences"], 1);
    let kept = clip["kept"].as_array().unwrap();
    assert_eq!(kept.len(), 2, "{plan}");
    // 120 ms of padding: [0, ~1.12 s) and [~1.88 s, 3 s), snapped outward.
    let first_end = kept[0][1].as_u64().unwrap();
    let second_start = kept[1][0].as_u64().unwrap();
    assert!((33..=35).contains(&first_end), "{plan}");
    assert!((55..=57).contains(&second_start), "{plan}");
    assert_eq!(kept[1][1], 90);
    assert_eq!(clip_ids(&env, &dir), ["c001"], "a dry run changes nothing");
    let human = qrec(&env, &dir, &["autocut", "--dry-run"]);
    human.ok();
    assert!(human.stdout.starts_with("CLIP"), "{}", human.stdout);

    let apply = qrec(&env, &dir, &["autocut", "--json"]);
    apply.ok();
    assert_eq!(seq(&apply), 1);
    assert_eq!(created(&apply).len(), 1);
    let rows = qrec(&env, &dir, &["clip", "list", "--json"]).reply()["data"].clone();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "c001", "the first fragment keeps the id");
    assert_eq!(rows[0]["out"].as_u64(), Some(first_end));
    assert_eq!(rows[1]["in"].as_u64(), Some(second_start));
    qrec(&env, &dir, &["undo"]).ok();
    assert_eq!(clip_ids(&env, &dir), ["c001"]);
}

#[test]
fn export_end_to_end_never_overwrites() {
    let env = TestEnv::new();
    let dir = project_with(&env, "export", &[video(30), video(30)]);
    let first = qrec(&env, &dir, &["export", "--quality", "draft", "--json"]);
    first.ok();
    let lines = first.json_lines();
    let reply = lines.last().unwrap();
    assert_eq!(reply["ok"], true);
    assert!(
        lines[..lines.len() - 1]
            .iter()
            .all(|l| l["event"] == "progress"),
        "{}",
        first.stdout
    );
    let path = PathBuf::from(reply["data"]["result"]["path"].as_str().unwrap());
    assert!(path.starts_with(dir.join("exports")), "{}", path.display());
    let probe = ffprobe(&path);
    let duration: f64 = probe["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((duration - 2.0).abs() < 0.05, "duration {duration}");
    let video_stream = probe["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == "video")
        .unwrap();
    assert_eq!(video_stream["nb_read_packets"], "60");
    assert_eq!(reply["data"]["result"]["frames"], 60);

    let bytes = fs::read(&path).unwrap();
    let second = qrec(&env, &dir, &["export", "--quality", "draft", "--json"]);
    second.ok();
    let second_path = second.reply()["data"]["result"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(PathBuf::from(&second_path), path, "a new name every time");
    assert_eq!(
        fs::read(&path).unwrap(),
        bytes,
        "the first export is untouched"
    );

    let mine = dir.join("mine.mp4");
    fs::write(&mine, "precious").unwrap();
    let refused = qrec(&env, &dir, &["export", "--out", "mine.mp4", "--json"]);
    assert_eq!(refused.code, Some(4));
    assert_eq!(fs::read_to_string(&mine).unwrap(), "precious");
    qrec(
        &env,
        &dir,
        &["export", "--quality", "draft", "--out", "mine.mp4", "--yes"],
    )
    .ok();
    assert_ne!(fs::read(&mine).unwrap(), b"precious");

    let human = qrec(&env, &dir, &["export", "--quality", "draft"]);
    human.ok();
    assert!(
        human.stderr.contains('%'),
        "a progress line: {}",
        human.stderr
    );
}

/// A hand-picked `--out` that someone else creates while the export runs
/// is kept, and so is the export, next to it (`<stem>-kept.<ext>`); the
/// command then needs confirmation (exit 4, both paths named), like an
/// `--out` that existed before (A§8.8, SPEC §11). The kept export is the
/// result: the host-start sweep of stale temp files (A§8.7) never takes it,
/// even in `exports/` and an hour later.
#[test]
fn an_out_that_appears_during_the_export_needs_confirmation() {
    let env = TestEnv::new();
    let dir = project_with(&env, "race", &[video(300)]);
    fs::create_dir(dir.join("exports")).unwrap();
    let out = dir.join("exports/race.mp4");
    let out_arg = out.to_str().unwrap();
    let segments = dir.join(".qrec/cache/segments");
    let rendering = || {
        fs::read_dir(&segments).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().contains(".qrec-part-"))
        })
    };
    let export = thread::scope(|scope| {
        let job = scope.spawn(|| {
            qrec(
                &env,
                &dir,
                &["export", "--quality", "draft", "--out", out_arg, "--json"],
            )
        });
        // Once the export renders, someone else writes the file.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !rendering() {
            assert!(
                !job.is_finished() && Instant::now() < deadline,
                "the export never rendered a segment"
            );
            thread::sleep(Duration::from_millis(2));
        }
        fs::write(&out, "someone else's").unwrap();
        job.join().unwrap()
    });
    assert_eq!(
        export.code,
        Some(4),
        "stdout:\n{}\nstderr:\n{}",
        export.stdout,
        export.stderr
    );
    let error = &export.reply()["error"];
    assert_eq!(error["code"], "confirmation_required");
    let kept = PathBuf::from(error["details"]["kept"].as_str().unwrap());
    assert_eq!(
        error["details"]["files"],
        json!([{ "path": out_arg, "bytes": 14 }])
    );
    let message = error["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("confirmation required: {out_arg} appeared"))
            && message.ends_with(&format!("kept as {}", kept.display())),
        "{message}"
    );
    assert!(error["hint"].as_str().unwrap().contains("--yes"));
    assert_eq!(fs::read_to_string(&out).unwrap(), "someone else's");
    assert_eq!(kept, dir.join("exports/race-kept.mp4"));
    let packets = |file: &Path| {
        let probe = ffprobe(file);
        let video_stream = probe["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["codec_type"] == "video")
            .unwrap()
            .clone();
        video_stream["nb_read_packets"].clone()
    };
    assert_eq!(packets(&kept), "300", "the export was kept whole");
    let leftovers: Vec<_> = fs::read_dir(dir.join("exports"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().contains(".qrec-part-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // An hour later the next host sweeps stale temps, never the result.
    let stale = dir.join("exports/.race.qrec-part-1-1.mp4");
    fs::write(&stale, "left over").unwrap();
    let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
    for file in [&kept, &stale] {
        fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(two_hours_ago)
            .unwrap();
    }
    qrec(
        &env,
        &dir,
        &["autocut", "settings", "--threshold-db", "-36", "-q"],
    )
    .ok();
    assert!(!stale.exists(), "the sweep ran");
    assert_eq!(packets(&kept), "300", "the kept export is still there");
}

#[test]
fn frame_save_writes_a_png() {
    let env = TestEnv::new();
    let dir = project_with(&env, "frames", &[video(30)]);
    let saved = qrec(&env, &dir, &["frame", "save", "--at", "10f", "--json"]);
    saved.ok();
    let path = PathBuf::from(saved.reply()["data"]["result"]["path"].as_str().unwrap());
    assert!(path.starts_with(dir.join("frames")), "{}", path.display());
    let probe = ffprobe(&path);
    assert_eq!(probe["streams"][0]["codec_name"], "png");
    assert_eq!(probe["streams"][0]["width"], 320);
    let again = qrec(&env, &dir, &["frame", "save", "--at", "10f", "--json"]);
    again.ok();
    assert_ne!(
        again.reply()["data"]["result"]["path"],
        saved.reply()["data"]["result"]["path"]
    );
    let bad = qrec(&env, &dir, &["frame", "save", "--at", "+1f"]);
    assert_eq!(bad.code, Some(2), "a relative time is no frame");
}

#[test]
fn schema_keys_and_agent_guide() {
    let env = TestEnv::new();
    let dir = env.empty_dir("anywhere");
    let schema = qrec(&env, &dir, &["schema"]);
    schema.ok();
    let schema: Value = serde_json::from_str(&schema.stdout).unwrap();
    assert!(schema["properties"]["timeline"].is_object(), "{schema}");
    let keys = qrec(&env, &dir, &["keys", "--json"]);
    keys.ok();
    let rows = keys.only_reply()["data"].as_array().unwrap().clone();
    assert!(
        rows.iter().any(|r| r["cli"] == "qrec record toggle"),
        "{rows:?}"
    );
    let human = qrec(&env, &dir, &["keys"]);
    human.ok();
    assert!(human.stdout.contains("qrec record toggle"));
    let guide = qrec(&env, &dir, &["agent-guide"]);
    guide.ok();
    assert!(guide.stdout.contains("--json"), "{}", guide.stdout);
    let json = qrec(&env, &dir, &["agent-guide", "--json"]);
    assert!(json.only_reply()["data"].as_str().unwrap().contains("qrec"));
}

#[test]
fn sources_and_doctor_use_the_fake_inventory() {
    let env = TestEnv::new();
    let dir = env.empty_dir("anywhere");
    let sources = qrec(&env, &dir, &["sources", "list", "--json"]);
    sources.ok();
    let data = &sources.only_reply()["data"];
    let outputs: Vec<&str> = data["screens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["output"].as_str().unwrap())
        .collect();
    assert_eq!(outputs, ["FAKE-1", "FAKE-2"]);
    assert_eq!(data["cameras"][0]["device"], "/dev/fake0");
    assert_eq!(data["mics"][0]["name"], "alsa_input.fake");
    let human = qrec(&env, &dir, &["sources", "list"]);
    human.ok();
    assert!(human.stdout.contains("FAKE-2"));
    let doctor = qrec(&env, &dir, &["doctor", "--json"]);
    assert!(matches!(doctor.code, Some(0) | Some(5)), "{doctor:?}");
    let reply = doctor.only_reply();
    let checks = if reply["ok"] == true {
        reply["data"]["checks"].clone()
    } else {
        reply["error"]["details"]["checks"].clone()
    };
    assert!(checks.as_array().is_some_and(|c| !c.is_empty()), "{reply}");
}

#[test]
fn record_start_and_stop_with_the_fake_backend() {
    let env = TestEnv::new();
    let dir = env.empty_dir("rec");
    qrec(&env, &dir, &["init", "--canvas", "640x360@30"]).ok();
    qrec(
        &env,
        &dir,
        &["scene", "layer", "add", "screen", "--output", "FAKE-1"],
    )
    .ok();
    qrec(&env, &dir, &["scene", "audio", "add", "mic"]).ok();

    let start = qrec(&env, &dir, &["record", "start", "--json"]);
    start.ok();
    let started = &start.only_reply()["data"]["result"];
    assert_eq!(started["take"], "take-0001");
    assert!(!started["files"].as_array().unwrap().is_empty());
    let status = qrec(&env, &dir, &["status", "--json"]);
    status.ok();
    assert_eq!(status.reply()["data"]["host"], "headless");
    let busy = qrec(&env, &dir, &["clip", "list", "--json"]);
    busy.ok();
    let edit = qrec(&env, &dir, &["undo", "--json"]);
    assert_eq!(edit.code, Some(6), "editing waits for the recording");

    thread::sleep(Duration::from_millis(2000));
    let stop = qrec(&env, &dir, &["record", "stop", "--json"]);
    stop.ok();
    let finalized = &stop.only_reply()["data"]["result"];
    assert_eq!(finalized["status"], "ok");
    assert!(
        finalized["durationFrames"].as_u64().unwrap() >= 30,
        "{finalized}"
    );
    assert_eq!(finalized["placed"], true);
    assert_eq!(clip_ids(&env, &dir).len(), 1);
    assert!(dir.join("takes/take-0001").is_dir());

    // The headless host exits once idle (A§8.6): its socket goes away.
    wait_until_no_host(&env);
}

/// A take finalization whose commit record couldn't be written (A§7.6:
/// the history line and qrec.json were, the head wasn't) is finished by
/// the next open: the take is `ok` with its clip and nothing is called
/// invalid. It used to come back `recording`, and the next command said
/// Qrec's own qrec.json was invalid, then dropped the clip and the entry.
#[test]
fn a_take_finalized_without_its_commit_record_is_finished_by_the_next_open() {
    let env = TestEnv::new();
    let dir = env.empty_dir("unfinished");
    qrec(&env, &dir, &["init", "--canvas", "640x360@30"]).ok();
    qrec(
        &env,
        &dir,
        &["scene", "layer", "add", "screen", "--output", "FAKE-1"],
    )
    .ok();
    qrec(&env, &dir, &["record", "start", "--json"]).ok();
    thread::sleep(Duration::from_millis(1500));
    // From now on only the commit record (history.head.json) can't be
    // written: its temp file's name is taken by a folder.
    let blocker = dir.join(".qrec/history.head.json.tmp");
    fs::create_dir(&blocker).unwrap();
    let stop = qrec(&env, &dir, &["record", "stop", "--json"]);
    assert_eq!(stop.code, Some(1), "{stop:?}");
    let error = stop.only_reply()["error"].clone();
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("the change was made but couldn't be saved"),
        "{error}"
    );
    // The headless host exits (its last write fails the same way).
    wait_until_no_host(&env);
    fs::remove_dir(&blocker).unwrap();

    let row = take_row(&env, &dir, "take-0001");
    assert_eq!(row["status"], "ok", "{row}");
    assert_eq!(row["clips"], 1, "{row}");
    let edit = qrec(&env, &dir, &["project", "set", "--name", "x"]);
    edit.ok();
    assert!(!edit.stderr.contains("invalid"), "{}", edit.stderr);
    assert_eq!(clip_ids(&env, &dir).len(), 1, "the clip is still placed");
    let history = qrec(&env, &dir, &["history", "--json"]);
    history.ok();
    let labels: Vec<String> = history.reply()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["label"].as_str().unwrap().to_owned())
        .collect();
    assert!(labels.contains(&"Record take 1".to_owned()), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "External edit"), "{labels:?}");
    assert!(!dir.join(".qrec/rejected").exists());
}

/// Waits until every host of `env` exited (its socket went away, A§8.6).
fn wait_until_no_host(env: &TestEnv) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let sockets = || {
        fs::read_dir(env.runtime_dir())
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".sock"))
                    .count()
            })
            .unwrap_or(0)
    };
    while sockets() > 0 {
        assert!(Instant::now() < deadline, "the headless host didn't exit");
        thread::sleep(Duration::from_millis(100));
    }
}

// ------------------------------------------------------- review round 1

/// `qrec init` acts on its own folder (SPEC §4.4): with another project's
/// host running, a folder that is neither empty nor a project is still "Not
/// my swamp" (exit 3), never a request to that project.
#[test]
fn init_never_goes_to_another_projects_host() {
    let env = TestEnv::new();
    let a = env.empty_dir("a");
    qrec(&env, &a, &["init", "--canvas", "640x360@30"]).ok();
    qrec(
        &env,
        &a,
        &["scene", "layer", "add", "screen", "--output", "FAKE-1"],
    )
    .ok();
    qrec(&env, &a, &["record", "start", "--json"]).ok();
    let b = env.empty_dir("b");
    fs::write(b.join("notes.txt"), "mine").unwrap();

    let init = qrec(&env, &b, &["--json", "init"]);
    assert_eq!(init.code, Some(3), "{init:?}");
    assert_eq!(init.stderr, "", "no `using project` notice: {init:?}");
    let error = &init.only_reply()["error"];
    assert_eq!(error["code"], "not_project");
    assert_eq!(error["message"], NOT_MY_SWAMP);
    // Other commands from there still use the running host (key binds).
    let status = qrec(&env, &b, &["--json", "status"]);
    status.ok();
    assert_eq!(
        status.reply()["data"]["path"],
        fs::canonicalize(&a).unwrap().to_string_lossy().as_ref()
    );

    // Once the recording stopped, while the host idles out, the same.
    qrec(&env, &a, &["record", "cancel"]).ok();
    let again = qrec(&env, &b, &["--json", "init"]);
    assert_eq!(again.code, Some(3), "{again:?}");
    assert_eq!(again.only_reply()["error"]["code"], "not_project");
    assert_eq!(
        fs::read_dir(&b).unwrap().count(),
        1,
        "b is untouched: {:?}",
        fs::read_dir(&b).unwrap().collect::<Vec<_>>()
    );
    wait_until_no_host(&env);
}

/// `PATH` without the folders that hold ffmpeg or ffprobe, after `first`.
fn path_without_ffmpeg(first: Option<&Path>) -> OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let kept = std::env::split_paths(&path)
        .filter(|dir| !dir.join("ffmpeg").exists() && !dir.join("ffprobe").exists());
    std::env::join_paths(first.map(Path::to_path_buf).into_iter().chain(kept)).unwrap()
}

/// The ffmpeg on `PATH`.
fn ffmpeg_on_path() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("ffmpeg"))
        .find(|file| file.is_file())
        .expect("ffmpeg on PATH")
}

/// SPEC §14, A§4.8: a missing tool is `missing_dependency` (exit 5) for
/// every command that needs it, before anything is written.
#[test]
fn missing_ffmpeg_or_ffprobe_is_a_missing_dependency() {
    let env = TestEnv::new();
    let dir = project_with(&env, "deps", &[video(30)]);
    let no_ffmpeg = path_without_ffmpeg(None);
    for args in [
        &["--json", "frame", "save", "--at", "1f"][..],
        &["--json", "autocut", "--dry-run"],
        &["--json", "autocut"],
        &["--json", "export"],
    ] {
        let out = qrec_with(&env, &dir, args, &[("PATH", no_ffmpeg.as_os_str())]);
        assert_eq!(out.code, Some(5), "{args:?}: {out:?}");
        let error = &out.only_reply()["error"];
        assert_eq!(error["code"], "missing_dependency", "{args:?}");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("ffmpeg wasn't found"),
            "{args:?}: {error}"
        );
        assert!(
            error["hint"].as_str().unwrap().contains("qrec doctor"),
            "{args:?}: {error}"
        );
    }
    let human = qrec_with(
        &env,
        &dir,
        &["frame", "save", "--at", "1f"],
        &[("PATH", no_ffmpeg.as_os_str())],
    );
    assert_eq!(human.code, Some(5), "{human:?}");
    let pngs = fs::read_dir(dir.join("frames")).map_or(0, |d| d.count());
    assert_eq!(pngs, 0, "no frame was written");
    assert_eq!(clip_ids(&env, &dir), ["c001"], "autocut changed nothing");

    // ffmpeg without ffprobe: the export is refused before any segment
    // renders (every segment is checked with ffprobe, A§9.3).
    let only_ffmpeg = env.root().join("only-ffmpeg");
    fs::create_dir(&only_ffmpeg).unwrap();
    symlink(ffmpeg_on_path(), only_ffmpeg.join("ffmpeg")).unwrap();
    let path = path_without_ffmpeg(Some(&only_ffmpeg));
    let out = qrec_with(
        &env,
        &dir,
        &["--json", "export"],
        &[("PATH", path.as_os_str())],
    );
    assert_eq!(out.code, Some(5), "{out:?}");
    let error = &out.only_reply()["error"];
    assert_eq!(error["code"], "missing_dependency");
    assert_eq!(error["message"], "ffprobe wasn't found");
    let rendered = fs::read_dir(dir.join(".qrec/cache/segments")).map_or(0, |d| d.count());
    assert_eq!(rendered, 0, "nothing was rendered");
    // The same frame save works with ffmpeg alone.
    qrec_with(
        &env,
        &dir,
        &["frame", "save", "--at", "1f"],
        &[("PATH", path.as_os_str())],
    )
    .ok();
}

/// A GStreamer element that isn't installed is `missing_dependency` (exit
/// 5) naming it, not `capture_failed` (A§4.8).
#[test]
fn record_start_without_a_gstreamer_element_is_a_missing_dependency() {
    let env = TestEnv::new();
    let dir = env.empty_dir("nogst");
    qrec(&env, &dir, &["init", "--canvas", "640x360@30"]).ok();
    qrec(
        &env,
        &dir,
        &["scene", "layer", "add", "screen", "--output", "FAKE-1"],
    )
    .ok();
    let plugins = env.root().join("no-plugins");
    fs::create_dir(&plugins).unwrap();
    let registry = env.root().join("empty-registry.bin");
    let out = qrec_with(
        &env,
        &dir,
        &["--json", "record", "start"],
        &[
            ("GST_PLUGIN_SYSTEM_PATH_1_0", plugins.as_os_str()),
            ("GST_PLUGIN_PATH_1_0", OsStr::new("")),
            ("GST_REGISTRY_1_0", registry.as_os_str()),
        ],
    );
    assert_eq!(out.code, Some(5), "{out:?}");
    let error = &out.only_reply()["error"];
    assert_eq!(error["code"], "missing_dependency");
    assert!(
        error["message"].as_str().unwrap().contains("is missing"),
        "{error}"
    );
    assert!(
        error["hint"]
            .as_str()
            .unwrap()
            .contains("install the GStreamer plugin that provides "),
        "{error}"
    );
    wait_until_no_host(&env);
}

/// Only a closed pipe is quiet (A§8.6): a stdout that can't take the
/// result (`qrec schema > /dev/full`) fails the run with exit 1.
#[test]
fn a_full_stdout_fails_the_run() {
    if !Path::new("/dev/full").exists() {
        return;
    }
    let env = TestEnv::new();
    let dir = env.empty_dir("full");
    for args in [
        &["schema"][..],
        &["--json", "version"],
        &["--help"],
        &["--json", "--version"],
    ] {
        let full = fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let mut command = env.command(&qrec_bin());
        command
            .current_dir(&dir)
            .args(args)
            .stdout(full)
            .stderr(Stdio::piped());
        let out = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(
            stderr.contains("can't write to stdout"),
            "{args:?}: {stderr}"
        );
    }
}

/// `--out` means what the OS says (A§5.9): `link/..` is the parent of the
/// link's target, so a path that resolves into `takes/` is refused, and
/// one that resolves elsewhere is written there.
#[test]
fn out_paths_resolve_dot_dot_like_the_os() {
    let env = TestEnv::new();
    let dir = project_with(&env, "dotdot", &[video(30)]);
    let lnk = env.root().join("lnk");
    fs::create_dir(&lnk).unwrap();
    symlink(dir.join("takes/take-0001"), lnk.join("dirlink")).unwrap();
    let sneaky = lnk.join("dirlink/../x.png");
    let out = qrec(
        &env,
        &dir,
        &[
            "--json",
            "frame",
            "save",
            "--at",
            "1f",
            "--out",
            sneaky.to_str().unwrap(),
        ],
    );
    assert_eq!(out.code, Some(1), "{out:?}");
    let error = &out.only_reply()["error"];
    assert_eq!(error["code"], "invalid");
    assert!(
        error["message"].as_str().unwrap().contains("inside takes/"),
        "{error}"
    );
    assert!(!lnk.join("x.png").exists() && !dir.join("takes/x.png").exists());

    let real = env.root().join("real");
    fs::create_dir_all(real.join("sub")).unwrap();
    fs::create_dir_all(real.join("stills")).unwrap();
    let a = env.root().join("a");
    fs::create_dir(&a).unwrap();
    symlink(real.join("sub"), a.join("cur")).unwrap();
    let wanted = a.join("cur/../stills/still.png");
    let saved = qrec(
        &env,
        &dir,
        &[
            "--json",
            "frame",
            "save",
            "--at",
            "1f",
            "--out",
            wanted.to_str().unwrap(),
        ],
    );
    saved.ok();
    assert!(real.join("stills/still.png").is_file(), "{saved:?}");
    assert!(!a.join("stills").exists());
}

/// `takes/` moved to another disk and linked back (A§5.9): an `--out` that
/// reaches a take folder, through the link or straight at its target, is
/// refused (`invalid`, exit 1) with or without `--yes`, so no export or
/// frame replaces a recording or lands among its files (SPEC §11). The
/// project still exports elsewhere through the link.
#[test]
fn a_symlinked_takes_folder_still_guards_the_recordings() {
    let env = TestEnv::new();
    let dir = project_with(&env, "moved", &[video(30)]);
    let disk = env.root().join("disk");
    fs::create_dir(&disk).unwrap();
    fs::rename(dir.join("takes"), disk.join("takes")).unwrap();
    symlink(disk.join("takes"), dir.join("takes")).unwrap();
    let take = dir.join("takes/take-0001");
    let listing = || {
        let mut names: Vec<String> = fs::read_dir(&take)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    let files = listing();
    let media = take.join("L1-screen-FAKE-1.mkv");
    let recorded = fs::read(&media).unwrap();

    let refused = |args: &[&str]| {
        let out = qrec(&env, &dir, args);
        assert_eq!(out.code, Some(1), "{args:?}: {out:?}");
        let error = &out.only_reply()["error"];
        assert_eq!(error["code"], "invalid", "{args:?}");
        assert!(
            error["message"].as_str().unwrap().contains("inside takes/"),
            "{error}"
        );
    };
    let media_out = "takes/take-0001/L1-screen-FAKE-1.mkv";
    refused(&["--json", "export", "--quality", "draft", "--out", media_out]);
    refused(&[
        "--json",
        "--yes",
        "export",
        "--quality",
        "draft",
        "--out",
        media_out,
    ]);
    refused(&[
        "--json",
        "--yes",
        "export",
        "--out",
        "takes/take-0001/inside.mp4",
    ]);
    let png = take.join("inside.png");
    refused(&[
        "--json",
        "frame",
        "save",
        "--at",
        "3f",
        "--out",
        png.to_str().unwrap(),
    ]);
    let direct = disk.join("takes/take-0001/inside.mp4");
    refused(&[
        "--json",
        "--yes",
        "export",
        "--out",
        direct.to_str().unwrap(),
    ]);
    assert_eq!(
        fs::read(&media).unwrap(),
        recorded,
        "the recording is untouched"
    );
    assert_eq!(listing(), files, "nothing was written into the take folder");

    let export = qrec(&env, &dir, &["--json", "export", "--quality", "draft"]);
    export.ok();
    let path = PathBuf::from(export.reply()["data"]["result"]["path"].as_str().unwrap());
    assert!(path.starts_with(dir.join("exports")), "{}", path.display());
}

/// The sorted names in `dir`.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Runs `args` and asserts an `invalid` refusal (exit 1) whose message
/// says why: never a confirmation that `--yes` would turn into an
/// overwrite.
#[track_caller]
fn refused_out(env: &TestEnv, dir: &Path, args: &[&str], why: &str) {
    let out = qrec(env, dir, args);
    assert_eq!(out.code, Some(1), "{args:?}: {out:?}");
    let error = &out.only_reply()["error"];
    assert_eq!(error["code"], "invalid", "{args:?}: {error}");
    assert!(
        error["message"].as_str().unwrap().contains(why),
        "{args:?}: {error}"
    );
}

/// Links *below* `takes/` and `.qrec/` guard the recordings too (A§5.9):
/// a take folder or a media file moved to another disk and linked back,
/// or a linked `.qrec/cache`. An `--out` through the link or straight at
/// its target is refused (`invalid`, exit 1), with or without `--yes`, so
/// no export or frame replaces a recording or lands among its files
/// (SPEC §11).
#[test]
fn links_below_takes_and_qrec_still_guard_the_recordings() {
    let env = TestEnv::new();
    let disk = env.root().join("disk");
    fs::create_dir(&disk).unwrap();

    // One take folder moved to another disk and linked back.
    let dir = project_with(&env, "take-linked", &[video(30)]);
    fs::rename(dir.join("takes/take-0001"), disk.join("take-0001")).unwrap();
    symlink(disk.join("take-0001"), dir.join("takes/take-0001")).unwrap();
    let media = disk.join("take-0001/L1-screen-FAKE-1.mkv");
    let recorded = fs::read(&media).unwrap();
    let files = names_in(&disk.join("take-0001"));
    let direct = disk.join("take-0001/L1-screen-FAKE-1.mkv");
    let new_png = disk.join("take-0001/new.png");
    for args in [
        &[
            "--json",
            "export",
            "--out",
            "takes/take-0001/L1-screen-FAKE-1.mkv",
        ][..],
        &[
            "--json",
            "--yes",
            "export",
            "--out",
            "takes/take-0001/L1-screen-FAKE-1.mkv",
        ],
        &["--json", "export", "--out", "takes/take-0001/new.mp4"],
        &[
            "--json",
            "frame",
            "save",
            "--at",
            "3f",
            "--out",
            "takes/take-0001/A1-mic.mka",
        ],
        &[
            "--json",
            "--yes",
            "export",
            "--out",
            direct.to_str().unwrap(),
        ],
        &[
            "--json",
            "frame",
            "save",
            "--at",
            "3f",
            "--out",
            new_png.to_str().unwrap(),
        ],
    ] {
        refused_out(&env, &dir, args, "inside takes/");
    }
    assert_eq!(
        fs::read(&media).unwrap(),
        recorded,
        "the recording is untouched"
    );
    assert_eq!(
        names_in(&disk.join("take-0001")),
        files,
        "nothing was added"
    );
    let beside = disk.join("beside.mp4");
    qrec(
        &env,
        &dir,
        &[
            "--json",
            "export",
            "--quality",
            "draft",
            "--out",
            beside.to_str().unwrap(),
        ],
    )
    .ok();
    assert!(beside.is_file(), "next to the moved take is fine");

    // One media file of a take moved to another disk and linked back.
    let dir = project_with(&env, "file-linked", &[video(30)]);
    let moved = disk.join("A1-mic.mka");
    fs::rename(dir.join("takes/take-0001/A1-mic.mka"), &moved).unwrap();
    symlink(&moved, dir.join("takes/take-0001/A1-mic.mka")).unwrap();
    let audio = fs::read(&moved).unwrap();
    for args in [
        &[
            "--json",
            "--yes",
            "export",
            "--out",
            "takes/take-0001/A1-mic.mka",
        ][..],
        &[
            "--json",
            "--yes",
            "export",
            "--out",
            moved.to_str().unwrap(),
        ],
        &[
            "--json",
            "--yes",
            "frame",
            "save",
            "--at",
            "3f",
            "--out",
            moved.to_str().unwrap(),
        ],
    ] {
        refused_out(&env, &dir, args, "inside takes/");
    }
    assert_eq!(fs::read(&moved).unwrap(), audio);
    assert!(dir.join("takes/take-0001/A1-mic.mka").is_symlink());

    // .qrec/cache moved and linked back.
    let dir = project_with(&env, "cache-linked", &[video(30)]);
    let cache = disk.join("cache2");
    fs::create_dir(&cache).unwrap();
    let _ = fs::remove_dir_all(dir.join(".qrec/cache"));
    fs::create_dir_all(dir.join(".qrec")).unwrap();
    symlink(&cache, dir.join(".qrec/cache")).unwrap();
    let straight = cache.join("y.mp4");
    for args in [
        &["--json", "export", "--out", ".qrec/cache/x.mp4"][..],
        &["--json", "export", "--out", straight.to_str().unwrap()],
    ] {
        refused_out(&env, &dir, args, "inside .qrec/");
    }
    assert!(!cache.join("x.mp4").exists() && !straight.exists());
    // Exports still use the cache through the link.
    qrec(&env, &dir, &["--json", "export", "--quality", "draft"]).ok();
    assert!(!names_in(&cache.join("segments")).is_empty());
}

/// Whether this machine lets an unprivileged user mount in a namespace of
/// its own (`unshare -rm`).
fn unshare_works() -> bool {
    std::process::Command::new("unshare")
        .args(["-rm", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A bind mount of one take folder, with no symlink anywhere, is known by
/// identity (A§5.9): an `--out` through the mount is refused even with
/// `--yes`, and the recording is untouched. Runs in a mount namespace
/// (`unshare -rm`), so nothing is mounted outside the test; skipped where
/// unprivileged user namespaces are off.
#[test]
fn a_bind_mounted_take_folder_still_guards_the_recordings() {
    if !unshare_works() {
        eprintln!("skipped: `unshare -rm` isn't allowed here");
        return;
    }
    let env = TestEnv::new();
    let dir = project_with(&env, "bound", &[video(30)]);
    let mount = env.root().join("bm");
    fs::create_dir(&mount).unwrap();
    let take = dir.join("takes/take-0001");
    let media = take.join("L1-screen-FAKE-1.mkv");
    let recorded = fs::read(&media).unwrap();
    let files = names_in(&take);
    for (args, out) in [
        (
            &["--json", "--yes", "export", "--out"][..],
            "L1-screen-FAKE-1.mkv",
        ),
        (&["--json", "export", "--out"], "new.mp4"),
        (
            &["--json", "frame", "save", "--at", "3f", "--out"],
            "new.png",
        ),
    ] {
        let mut command = env.command(Path::new("unshare"));
        command
            .current_dir(&dir)
            .args([
                "-rm",
                "sh",
                "-c",
                r#"mount --bind "$1" "$2" && shift 2 && exec "$@""#,
            ])
            .arg("sh")
            .arg(&take)
            .arg(&mount)
            .arg(qrec_bin())
            .args(args)
            .arg(mount.join(out));
        let run = run_tool(&mut command, Duration::from_secs(120));
        let stdout = String::from_utf8(run.stdout).unwrap();
        assert_eq!(run.status.code(), Some(1), "{out}: {stdout}{}", run.stderr);
        let reply: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
        assert_eq!(reply["error"]["code"], "invalid", "{reply}");
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("inside takes/"),
            "{reply}"
        );
    }
    assert_eq!(fs::read(&media).unwrap(), recorded);
    assert_eq!(names_in(&take), files);
}

/// `--out dir/` names a folder (A§5.9): refused (`invalid`), whether the
/// folder exists or not, for exports and frame saves; no file named like
/// the folder appears.
#[test]
fn an_out_that_names_a_folder_is_refused() {
    let env = TestEnv::new();
    let dir = project_with(&env, "slash", &[video(30)]);
    let newdir = env.root().join("newdir");
    let with_slash = format!("{}/", newdir.display());
    let with_dot = format!("{}/.", newdir.display());
    refused_out(
        &env,
        &dir,
        &["--json", "export", "--out", &with_slash],
        "names a folder",
    );
    refused_out(
        &env,
        &dir,
        &["--json", "frame", "save", "--at", "3f", "--out", &with_dot],
        "names a folder",
    );
    assert!(!newdir.exists(), "no file named like the folder");
    // An existing file followed by `/` isn't that file.
    let file = env.root().join("final.webm");
    fs::write(&file, "mine").unwrap();
    refused_out(
        &env,
        &dir,
        &[
            "--json",
            "--yes",
            "export",
            "--out",
            &format!("{}/", file.display()),
        ],
        "names a folder",
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "mine");
    // An existing folder is a directory, as before.
    fs::create_dir_all(dir.join("exports")).unwrap();
    refused_out(
        &env,
        &dir,
        &["--json", "export", "--out", "exports/"],
        "directory",
    );
}

/// Overwriting a hand-picked `--out` is confirmed knowing what is lost:
/// the file and its size, in the message and in `details.files` (SPEC §11,
/// A§4.8), for exports and frame saves alike.
#[test]
fn an_overwrite_confirmation_names_the_file_and_its_size() {
    let env = TestEnv::new();
    let dir = project_with(&env, "sizes", &[video(30)]);
    let mp4 = env.root().join("existing.mp4");
    fs::write(&mp4, "my own file\n").unwrap();
    let png = env.root().join("existing.png");
    fs::write(&png, vec![0u8; 2_500]).unwrap();
    let cases = [
        (
            vec!["--json", "export", "--out", mp4.to_str().unwrap()],
            &mp4,
            12,
            "12 B",
        ),
        (
            vec![
                "--json",
                "frame",
                "save",
                "--at",
                "3f",
                "--out",
                png.to_str().unwrap(),
            ],
            &png,
            2_500,
            "2.5 kB",
        ),
    ];
    for (args, path, bytes, size) in cases {
        let out = qrec(&env, &dir, &args);
        assert_eq!(out.code, Some(4), "{out:?}");
        let error = &out.only_reply()["error"];
        assert_eq!(error["code"], "confirmation_required");
        assert_eq!(
            error["message"],
            format!(
                "confirmation required: overwrite {} ({size})",
                path.display()
            )
        );
        assert_eq!(
            error["details"]["files"],
            json!([{ "path": path, "bytes": bytes }])
        );
    }
    let human = qrec(&env, &dir, &["export", "--out", mp4.to_str().unwrap()]);
    assert_eq!(human.code, Some(4), "{human:?}");
    assert!(human.stderr.contains("(12 B)"), "{}", human.stderr);
    assert_eq!(fs::read_to_string(&mp4).unwrap(), "my own file\n");
    assert_eq!(fs::read(&png).unwrap().len(), 2_500);
}

/// Host start deletes stale temp files (A§8.7), and only those: exports and
/// frames are never deleted, also when the project's name contains the
/// temp marker (SPEC §11).
#[test]
fn the_host_start_sweep_deletes_only_temp_files() {
    let env = TestEnv::new();
    let dir = project_with(&env, "sweep", &[video(30)]);
    qrec(
        &env,
        &dir,
        &["project", "set", "--name", "talk.qrec-part-final", "-q"],
    )
    .ok();
    let export = qrec(&env, &dir, &["--json", "export", "--quality", "draft"]);
    export.ok();
    let frame = qrec(&env, &dir, &["--json", "frame", "save", "--at", "3f"]);
    frame.ok();
    let results: Vec<PathBuf> = [&export, &frame]
        .iter()
        .map(|out| PathBuf::from(out.reply()["data"]["result"]["path"].as_str().unwrap()))
        .collect();
    let stale = [
        dir.join("exports/.talk.qrec-part-1-1.mp4"),
        dir.join("frames/.talk.qrec-part-1-2.png"),
    ];
    for file in &stale {
        fs::write(file, "left over").unwrap();
    }
    let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
    for file in results.iter().chain(&stale) {
        fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(two_hours_ago)
            .unwrap();
    }
    // Any command that takes the project lock starts a host.
    qrec(
        &env,
        &dir,
        &["autocut", "settings", "--threshold-db", "-36", "-q"],
    )
    .ok();
    for file in &stale {
        assert!(!file.exists(), "{} is a stale temp", file.display());
    }
    for file in &results {
        assert!(file.is_file(), "{} was deleted", file.display());
        let name = file.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("talk_qrec-part-final-"), "{name}");
    }
}

/// Ctrl+C under --json: stdout still ends with exactly one `{"ok":…}`
/// object (SPEC §14) and the run exits 130 (A§8.6).
#[test]
fn an_interrupted_export_still_ends_with_one_object() {
    let env = TestEnv::new();
    let spec = MediaSpec::video(1280, 720, 30, 600).with_audio(48_000, 1, &[(0, 20_000)]);
    let dir = project_with(&env, "interrupt", &[spec]);
    let mut command = env.command(&qrec_bin());
    command
        .current_dir(&dir)
        .args(["--json", "export", "--format", "webm", "--quality", "high"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = lines.next().unwrap().unwrap();
    assert!(first.starts_with(r#"{"event":"progress""#), "{first}");
    let killed = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let rest: Vec<String> = lines.map(Result::unwrap).collect();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the export didn't stop");
        }
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(130), "{rest:?}");
    let last: Value = serde_json::from_str(rest.last().unwrap_or(&first)).unwrap();
    assert_eq!(last["ok"], false, "{rest:?}");
    assert_eq!(last["error"]["code"], "interrupted");
    assert_eq!(last["error"]["message"], "export interrupted");
    assert_eq!(
        last["error"]["details"]["reply"]["message"], "export cancelled",
        "{last}"
    );
    assert!(
        rest[..rest.len().saturating_sub(1)]
            .iter()
            .all(|l| l.starts_with(r#"{"event":"progress""#)),
        "{rest:?}"
    );
    let exports = fs::read_dir(dir.join("exports")).map_or(0, |d| d.count());
    assert_eq!(exports, 0, "no export and no temp file is left");
}

// ------------------------------------------------------- history recovery

/// `qrec init`, then two entries: 1 (encoder) and 2 (countdown).
fn two_entries(env: &TestEnv, name: &str) -> PathBuf {
    let dir = env.empty_dir(name);
    qrec(env, &dir, &["init", "-q"]).ok();
    qrec(env, &dir, &["project", "set", "--encoder", "x264", "-q"]).ok();
    qrec(env, &dir, &["project", "set", "--countdown", "3", "-q"]).ok();
    dir
}

/// The `.qrec/qrec.corrupt-*` copies' contents.
fn corrupt_copies(dir: &Path) -> Vec<Vec<u8>> {
    fs::read_dir(dir.join(".qrec"))
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().starts_with("qrec.corrupt-"))
        .map(|e| fs::read(e.path()).unwrap())
        .collect()
}

/// Edits `qrec.json` by hand (valid JSON, Qrec's formatting).
fn hand_edit(dir: &Path, edit: impl FnOnce(&mut Value)) {
    let path = dir.join("qrec.json");
    let mut json: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut json);
    fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
}

#[test]
fn an_unconfirmed_restore_leaves_a_broken_project_alone() {
    let env = TestEnv::new();
    let dir = two_entries(&env, "unconfirmed");
    // A crash in the middle of an append, then a broken hand edit: the
    // store needs repair and qrec.json is unusable.
    let history = dir.join(".qrec/history.jsonl");
    let mut lines = fs::read(&history).unwrap();
    lines.extend_from_slice(b"{\"seq\":3,\"ts\":\"2026-10-1");
    fs::write(&history, lines).unwrap();
    let broken: &[u8] = b"{\"version\":1,\"name\":\"my unsaved hand edit\", ";
    fs::write(dir.join("qrec.json"), broken).unwrap();

    let refused = qrec(&env, &dir, &["history", "restore", "1", "--json"]);
    assert_eq!(refused.code, Some(4), "{}", refused.stderr);
    assert_eq!(
        fs::read(dir.join("qrec.json")).unwrap(),
        broken,
        "SPEC §11: without --yes nothing runs"
    );
    assert!(corrupt_copies(&dir).is_empty());
    assert!(!dir.join(".qrec/rejected").exists());

    qrec(&env, &dir, &["history", "restore", "1", "--yes", "--json"]).ok();
    assert_eq!(
        corrupt_copies(&dir),
        [broken.to_vec()],
        "the user's own bytes"
    );
    let status = qrec(&env, &dir, &["status", "--json"]);
    status.ok();
    assert_eq!(status.reply()["data"]["history"]["cursor"], 1);
}

#[test]
fn a_project_rebuilt_from_the_history_gets_its_takes_back() {
    let env = TestEnv::new();
    let dir = env.empty_dir("rebuild");
    qrec(&env, &dir, &["init", "--canvas", "640x360@30", "-q"]).ok();
    qrec(
        &env,
        &dir,
        &[
            "scene", "layer", "add", "screen", "--output", "FAKE-1", "-q",
        ],
    )
    .ok();
    qrec(&env, &dir, &["record", "start", "--json"]).ok();
    thread::sleep(Duration::from_millis(1500));
    let stop = qrec(&env, &dir, &["record", "stop", "--json"]);
    stop.ok();
    let frames = stop.only_reply()["data"]["result"]["durationFrames"].clone();
    wait_until_no_host(&env);
    let status = qrec(&env, &dir, &["status", "--json"]);
    let cursor = status.reply()["data"]["history"]["cursor"].to_string();
    // qrec.json and the commit record are gone: only the base (older than
    // the take) and the history are left (A§7.7).
    fs::write(dir.join("qrec.json"), b"garbage").unwrap();
    fs::remove_file(dir.join(".qrec/history.head.json")).unwrap();

    let refused = qrec(&env, &dir, &["history", "restore", &cursor, "--json"]);
    assert_eq!(refused.code, Some(4), "{}", refused.stderr);
    assert_eq!(fs::read(dir.join("qrec.json")).unwrap(), b"garbage");

    let restore = qrec(
        &env,
        &dir,
        &["history", "restore", &cursor, "--yes", "--json"],
    );
    restore.ok();
    assert_eq!(corrupt_copies(&dir), [b"garbage".to_vec()]);
    // The take was recovered before the restore wrote: a normal open works.
    let takes = qrec(&env, &dir, &["take", "list", "--json"]);
    takes.ok();
    let take = &takes.reply()["data"][0];
    assert_eq!(take["status"], "ok", "{take}");
    assert_eq!(take["durationFrames"], frames);
    assert_eq!(take["clips"], 1);
    qrec(&env, &dir, &["status"]).ok();
    wait_until_no_host(&env);
}

#[test]
fn a_restore_keeps_a_valid_hand_edit_as_an_entry() {
    let env = TestEnv::new();
    let dir = two_entries(&env, "hand-edit");
    hand_edit(&dir, |json| json["name"] = "my hand edit".into());
    let refused = qrec(&env, &dir, &["history", "restore", "1", "--json"]);
    assert_eq!(refused.code, Some(4));
    assert!(
        refused.reply()["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("undoes 2 history entries"),
        "the edit is an entry to undo: {}",
        refused.stdout
    );
    qrec(&env, &dir, &["history", "restore", "1", "--yes", "-q"]).ok();
    assert!(corrupt_copies(&dir).is_empty(), "a valid file isn't broken");
    let history = qrec(&env, &dir, &["history", "--json"]);
    let rows = history.reply()["data"].as_array().unwrap().clone();
    assert_eq!(rows.last().unwrap()["label"], "External edit");
    qrec(&env, &dir, &["redo", "2", "-q"]).ok();
    let status = qrec(&env, &dir, &["status", "--json"]);
    assert_eq!(status.reply()["data"]["project"]["name"], "my hand edit");
}

/// A hand edit that is valid alone but not once merged (A§7.4: facts in
/// memory win) is kept as it is, even when the files need repair (SPEC
/// §16, A§7.7): commands refuse to run until it is fixed or restored, and a
/// restore keeps a copy. It used to be replaced by the commit record's
/// project, with no copy kept anywhere, and the next command succeeded.
#[test]
fn a_hand_edit_that_doesnt_merge_is_never_overwritten() {
    let env = TestEnv::new();
    let dir = project_with(&env, "unmerged", &[video(30)]);
    qrec(&env, &dir, &["project", "set", "--name", "named", "-q"]).ok();
    // A torn last history line (a crash mid-append): the next open has
    // repairs to write.
    let history = dir.join(".qrec/history.jsonl");
    let mut lines = fs::read(&history).unwrap();
    lines.extend_from_slice(b"{\"seq\":");
    fs::write(&history, lines).unwrap();
    hand_edit(&dir, |json| {
        json["name"] = json!("my hand edit");
        json["takes"]["take-0001"]["durationFrames"] = json!(500);
        json["timeline"]["clips"][0]["out"] = json!(500);
    });
    let edited = fs::read(dir.join("qrec.json")).unwrap();

    for _ in 0..2 {
        let out = qrec(&env, &dir, &["project", "set", "--name", "z", "--json"]);
        assert_eq!(out.code, Some(1), "{out:?}");
        let error = out.only_reply()["error"].clone();
        assert_eq!(error["code"], "invalid");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .starts_with("qrec.json on disk is invalid: "),
            "{error}"
        );
        assert_eq!(
            fs::read(dir.join("qrec.json")).unwrap(),
            edited,
            "the hand edit is kept"
        );
    }
    qrec(&env, &dir, &["history", "restore", "1", "--yes", "-q"]).ok();
    assert_eq!(corrupt_copies(&dir), [edited], "a restore keeps a copy");
    let status = qrec(&env, &dir, &["status", "--json"]);
    assert_eq!(status.reply()["data"]["project"]["name"], "named");
}

#[test]
fn readers_report_only_the_history_on_disk() {
    let env = TestEnv::new();
    let dir = two_entries(&env, "reader");
    hand_edit(&dir, |json| json["recording"]["countdownSecs"] = 9.into());
    let before = fs::read(dir.join("qrec.json")).unwrap();
    let status = qrec(&env, &dir, &["status", "--json"]);
    status.ok();
    assert!(
        !status.stderr.contains("Merged an external edit"),
        "{}",
        status.stderr
    );
    let data = &status.reply()["data"];
    assert_eq!(data["project"]["recording"]["countdownSecs"], 9);
    assert_eq!(data["history"]["cursor"], 2);
    assert_eq!(data["history"]["nextSeq"], 3);
    let history = qrec(&env, &dir, &["history", "--json"]);
    let rows = history.reply()["data"].as_array().unwrap().clone();
    let current: Vec<&Value> = rows.iter().filter(|r| r["current"] == true).collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0]["seq"], 2, "the same cursor as status");
    assert_eq!(fs::read(dir.join("qrec.json")).unwrap(), before);
    // The next change records the edit for real.
    qrec(&env, &dir, &["project", "set", "--name", "renamed", "-q"]).ok();
    let history = qrec(&env, &dir, &["history", "--json"]);
    let labels: Vec<Value> = history.reply()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["label"].clone())
        .collect();
    assert_eq!(labels[2], "External edit");
}

#[test]
fn a_corrupt_history_line_names_the_way_out() {
    let env = TestEnv::new();
    let dir = two_entries(&env, "corrupt-line");
    let history = dir.join(".qrec/history.jsonl");
    let text = fs::read_to_string(&history).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines[0] = "{not json";
    fs::write(&history, format!("{}\n", lines.join("\n"))).unwrap();
    for args in [["status", "--json"], ["history", "--json"]] {
        let out = qrec(&env, &dir, &args);
        assert_eq!(out.code, Some(1));
        let error = out.reply()["error"].clone();
        assert!(
            error["message"].as_str().unwrap().contains("line 1"),
            "{error}"
        );
        let hint = error["hint"].as_str().unwrap();
        assert!(!hint.contains("qrec history"), "{args:?} fails too: {hint}");
        assert!(hint.contains(".qrec/history.jsonl"), "{hint}");
    }
}

#[test]
fn a_command_finishes_recovering_a_crashed_take_before_it_exits() {
    let env = TestEnv::new();
    let dir = project_with(&env, "crashed", &[video(30), video(30)]);
    // Take 2 as a crash leaves it: still `recording`, no media facts, no clip.
    hand_edit(&dir, |json| {
        let clips = json["timeline"]["clips"].as_array_mut().unwrap();
        clips.retain(|c| c["take"] != "take-0002");
        let take = &mut json["takes"]["take-0002"];
        take["status"] = "recording".into();
        take["durationFrames"] = 0.into();
        take["placed"] = false.into();
        for stream in take["streams"].as_array_mut().unwrap() {
            let stream = stream.as_object_mut().unwrap();
            stream.retain(|k, _| ["id", "kind", "file", "startOffsetNs"].contains(&k.as_str()));
        }
    });
    // Any command's in-process host starts the Recover probe; it must not
    // exit (killing ffprobe) before the take is recovered and saved (A§8.6).
    qrec(&env, &dir, &["project", "set", "--name", "after", "-q"]).ok();
    let json: Value = serde_json::from_slice(&fs::read(dir.join("qrec.json")).unwrap()).unwrap();
    let take = &json["takes"]["take-0002"];
    assert_eq!(take["status"], "ok", "{take}");
    assert_eq!(take["durationFrames"], 30);
    assert_eq!(json["name"], "after");
}

#[test]
fn the_history_cursor_file_holds_the_current_seq() {
    let env = TestEnv::new();
    let dir = two_entries(&env, "cursor-file");
    let cursor = || fs::read_to_string(dir.join(".qrec/history.cursor")).unwrap();
    assert_eq!(cursor(), "2\n", "SPEC §6.2, §8.1");
    qrec(&env, &dir, &["undo", "-q"]).ok();
    assert_eq!(cursor(), "1\n");
    let status = qrec(&env, &dir, &["status", "--json"]);
    assert_eq!(status.reply()["data"]["history"]["cursor"], 1);
}

// ------------------------------------------------------------- robustness

/// Runs every command line at once, each in its own `qrec` process.
fn all_at_once(env: &TestEnv, dir: &Path, lines: &[Vec<String>]) -> Vec<Out> {
    thread::scope(|scope| {
        let runs: Vec<_> = lines
            .iter()
            .map(|line| {
                scope.spawn(move || {
                    let args: Vec<&str> = line.iter().map(String::as_str).collect();
                    qrec(env, dir, &args)
                })
            })
            .collect();
        runs.into_iter().map(|run| run.join().unwrap()).collect()
    })
}

fn line(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

/// Commands started together each run exactly once and succeed, whichever
/// process ends up hosting the project: a host that decides to exit stops
/// accepting before it answers what it already admitted, and the clients it
/// bounces route again (A§8.3). In a fresh folder none of them takes a
/// project being initialized for someone else's folder (A§8.4). They used
/// to fail with `busy` (exit 6), `io` "dropped this request" or exit 3.
#[test]
fn commands_started_together_all_run_once() {
    let env = TestEnv::new();
    let dir = project_with(&env, "together", &[video(30)]);
    let mut changes = 0;
    for round in 0..3 {
        // Distinct, non-zero values: each one is a change.
        let lines: Vec<Vec<String>> = (0..8)
            .map(|i| {
                let gain = format!("-{}.{}", round + 1, i + 1);
                line(&["scene", "audio", "set", "A1", "--gain-db", &gain, "--json"])
            })
            .collect();
        for out in all_at_once(&env, &dir, &lines) {
            assert_eq!(out.code, Some(0), "round {round}: {out:?}");
            assert_eq!(out.only_reply()["ok"], true, "{out:?}");
        }
        changes += 8;
    }
    let history = qrec(&env, &dir, &["history", "--limit", "100", "--json"]);
    history.ok();
    assert_eq!(
        history.reply()["data"].as_array().unwrap().len(),
        changes,
        "every change ran once"
    );

    for round in 0..2 {
        let fresh = env.empty_dir(&format!("fresh-{round}"));
        let lines: Vec<Vec<String>> = (0..8)
            .map(|i| {
                let db = format!("-{}", 30 + i);
                line(&["autocut", "settings", "--threshold-db", &db, "--json"])
            })
            .collect();
        for out in all_at_once(&env, &fresh, &lines) {
            assert_eq!(out.code, Some(0), "fresh folder {round}: {out:?}");
        }
        // qrec.json's temp file lives in .qrec/, never beside it.
        let names: Vec<String> = fs::read_dir(&fresh)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");
    }
}

/// The fixture's take 2 as a crash leaves it: still `recording`, its
/// streams not probed, no clip. Its files are fine.
fn crash_take_two(dir: &Path, mut project: Value) {
    let take = &mut project["takes"]["take-0002"];
    take["status"] = json!("recording");
    take["durationFrames"] = json!(0);
    take.as_object_mut().unwrap().remove("placed");
    for stream in take["streams"].as_array_mut().unwrap() {
        stream
            .as_object_mut()
            .unwrap()
            .retain(|k, _| ["id", "kind", "file", "startOffsetNs"].contains(&k.as_str()));
    }
    project["timeline"]["clips"]
        .as_array_mut()
        .unwrap()
        .retain(|c| c["take"] != "take-0002");
    qrec_shell::testkit::write_project(dir, &project);
}

fn take_row(env: &TestEnv, dir: &Path, take: &str) -> Value {
    let list = qrec(env, dir, &["take", "list", "--json"]);
    list.ok();
    list.reply()["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == take)
        .unwrap_or_else(|| panic!("no {take} in {}", list.stdout))
        .clone()
}

/// After a crash, the next command that opens the project (an in-process
/// one) finishes the recovery it starts before its host exits: the take is
/// recovered and the person hears it (A§5.3 "Recovery", A§8.6). It used to
/// stay `recording` until some long-lived host ran.
#[test]
fn the_next_command_recovers_a_crashed_take() {
    let env = TestEnv::new();
    let dir = env.empty_dir("crash");
    let project = fixture_project(&dir, &[video(60), video(45)]);
    crash_take_two(&dir, project);
    assert_eq!(take_row(&env, &dir, "take-0002")["status"], "recording");

    let edit = qrec(
        &env,
        &dir,
        &["scene", "audio", "set", "A1", "--gain-db", "-3"],
    );
    edit.ok();
    assert!(
        edit.stderr.contains("Recovered take 2 from the bog"),
        "{}",
        edit.stderr
    );
    let row = take_row(&env, &dir, "take-0002");
    assert_eq!(row["status"], "ok", "{row}");
    assert_eq!(row["durationFrames"], 45, "{row}");
    assert_eq!(row["unused"], true, "recovered into the bin");
    qrec(&env, &dir, &["clip", "add", "take-0002", "--json"]).ok();
}

/// A folder with an `ffprobe` that sleeps `secs` before running the real
/// one (a probe of a long take on a slow disk).
fn slow_ffprobe(env: &TestEnv, secs: u32) -> OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let real = std::env::split_paths(&path)
        .map(|dir| dir.join("ffprobe"))
        .find(|file| file.is_file())
        .expect("ffprobe on PATH");
    let dir = env.root().join("slow-ffprobe");
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("ffprobe");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nsleep {secs}\nexec '{}' \"$@\"\n",
            real.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    std::env::join_paths(std::iter::once(dir).chain(std::env::split_paths(&path))).unwrap()
}

/// Ctrl+C on a command whose in-process host is still recovering a crashed
/// take, its probe slow (A§8.6): the command, which hadn't started, never
/// runs, and the host stops waiting for the probe after the 15 s grace (the
/// next open recovers the take). The open used to wait for the probe before
/// the signal was even seen; with a probe slower than the open's wait, an
/// interrupted `take purge --yes` then deleted the take.
#[test]
fn a_command_interrupted_during_the_startup_recovery_never_runs() {
    let env = TestEnv::new();
    let dir = env.empty_dir("interrupted");
    let mut project = fixture_project(&dir, &[video(30), video(45)]);
    project["timeline"]["clips"]
        .as_array_mut()
        .unwrap()
        .retain(|c| c["take"] != "take-0001");
    // Take 2's files are 1 GiB each (sparse, and new files: the fixtures
    // are shared hard links), so its probe may take 50 s (A§8.7).
    for stream in project["takes"]["take-0002"]["streams"].as_array().unwrap() {
        let file = dir.join(stream["file"].as_str().unwrap());
        fs::remove_file(&file).unwrap();
        fs::File::create(&file).unwrap().set_len(1 << 30).unwrap();
    }
    crash_take_two(&dir, project);
    let path = slow_ffprobe(&env, 25);
    let mut command = env.command(&qrec_bin());
    command
        .current_dir(&dir)
        .args(["--json", "take", "purge", "take-0001", "--yes"])
        .env("PATH", &path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    // Its host is up (and recovering take 2) before Ctrl+C.
    let lock = dir.join(".qrec/lock");
    let ready = || {
        fs::read(&lock)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|r| r["pid"] == child.id() && r["state"] == "ready")
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "the host never got ready");
        thread::sleep(Duration::from_millis(20));
    }
    thread::sleep(Duration::from_millis(500));
    let killed = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let signalled = Instant::now();
    let deadline = signalled + Duration::from_secs(40);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the interrupted command didn't exit");
        }
        thread::sleep(Duration::from_millis(50));
    };
    let waited = signalled.elapsed();
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    assert_eq!(status.code(), Some(130), "{stdout}");
    assert!(
        waited < Duration::from_secs(21),
        "the 15 s grace ends the wait for the probe: {waited:?}"
    );
    let last: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(last["error"]["code"], "interrupted", "{last}");
    assert!(dir.join("takes/take-0001").is_dir(), "nothing was purged");
    let row = take_row(&env, &dir, "take-0001");
    assert_eq!(row["status"], "ok", "{row}");
    let row = take_row(&env, &dir, "take-0002");
    assert_eq!(
        row["status"], "recording",
        "recovered by the next open: {row}"
    );
}

/// ffprobe missing when a take stops: the take keeps its facts (still
/// `recording`, nothing marked missing), `record stop` says what is
/// missing (exit 5), and the next command with ffprobe back recovers it.
/// It used to be marked broken for good, with exit 0.
#[test]
fn a_missing_ffprobe_never_breaks_a_take() {
    let env = TestEnv::new();
    let dir = env.empty_dir("noprobe");
    qrec(&env, &dir, &["init", "--canvas", "320x180@30"]).ok();
    qrec(
        &env,
        &dir,
        &["scene", "layer", "add", "screen", "--output", "FAKE-1"],
    )
    .ok();
    // A PATH with a shell but no ffmpeg or ffprobe.
    let bin = env.root().join("no-ffprobe");
    fs::create_dir(&bin).unwrap();
    let path = std::env::var_os("PATH").unwrap();
    for tool in ["sh", "env"] {
        let found = std::env::split_paths(&path)
            .map(|d| d.join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("no {tool} on PATH"));
        std::os::unix::fs::symlink(found, bin.join(tool)).unwrap();
    }
    let no_ffprobe: &[(&str, &OsStr)] = &[("PATH", bin.as_os_str())];
    qrec_with(&env, &dir, &["record", "start", "--json"], no_ffprobe).ok();
    thread::sleep(Duration::from_millis(1500));
    let stop = qrec_with(&env, &dir, &["record", "stop", "--json"], no_ffprobe);
    assert_eq!(stop.code, Some(5), "{stop:?}");
    let error = &stop.reply()["error"];
    assert_eq!(error["code"], "missing_dependency", "{error}");
    assert!(
        error["message"].as_str().unwrap().contains("ffprobe"),
        "{error}"
    );

    let project: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("qrec.json")).unwrap()).unwrap();
    let take = &project["takes"]["take-0001"];
    assert_eq!(take["status"], "recording", "{take}");
    assert!(!take["streams"].as_array().unwrap().is_empty(), "{take}");
    assert_ne!(take["scene"]["layers"][0]["missing"], true, "{take}");

    // The recording host idles out; then, with ffprobe back, the next
    // command recovers the take.
    let deadline = Instant::now() + Duration::from_secs(15);
    while fs::read_dir(env.runtime_dir())
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().ends_with(".sock"))
        })
        .unwrap_or(false)
    {
        assert!(Instant::now() < deadline, "the headless host didn't exit");
        thread::sleep(Duration::from_millis(100));
    }
    let edit = qrec(&env, &dir, &["project", "set", "--countdown", "0"]);
    edit.ok();
    assert!(
        edit.stderr.contains("Recovered take 1 from the bog"),
        "{}",
        edit.stderr
    );
    assert_eq!(take_row(&env, &dir, "take-0001")["status"], "ok");
}

/// A deleted qrec.json is restored from the history in `.qrec/` (A§7.7):
/// `history` lists it and `history restore` rebuilds the same file, also
/// with `--project` and with nothing but `.qrec/` left. Other commands say
/// how. It used to be "Not my swamp" (exit 3), or with only `.qrec/` left
/// an empty history and a hint naming the command that had just failed.
#[test]
fn a_deleted_qrec_json_is_restored_from_the_history() {
    let env = TestEnv::new();
    for keep_takes in [true, false] {
        let name = if keep_takes { "lost" } else { "lost-bare" };
        let dir = project_with(&env, name, &[video(90)]);
        qrec(&env, &dir, &["clip", "split", "c001", "--at", "30f"]).ok();
        let original = fs::read(dir.join("qrec.json")).unwrap();
        fs::remove_file(dir.join("qrec.json")).unwrap();
        if !keep_takes {
            fs::remove_dir_all(dir.join("takes")).unwrap();
        }

        let other = qrec(&env, &dir, &["clip", "list", "--json"]);
        assert_eq!(other.code, Some(1), "{other:?}");
        let error = &other.reply()["error"];
        assert_eq!(error["code"], "invalid", "{error}");
        assert!(
            error["hint"]
                .as_str()
                .unwrap()
                .contains("history restore 1 --yes"),
            "{error}"
        );

        let history = qrec(&env, &dir, &["history", "--json"]);
        history.ok();
        let rows = history.reply()["data"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["seq"], 1);

        let elsewhere = env.empty_dir(&format!("{name}-cwd"));
        let project = dir.to_string_lossy().into_owned();
        qrec(
            &env,
            &elsewhere,
            &["--project", &project, "history", "--json"],
        )
        .ok();
        let restore = qrec(
            &env,
            &elsewhere,
            &[
                "--project",
                &project,
                "--yes",
                "history",
                "restore",
                "1",
                "--json",
            ],
        );
        restore.ok();
        assert_eq!(fs::read(dir.join("qrec.json")).unwrap(), original);
        if keep_takes {
            assert_eq!(clip_ids(&env, &dir).len(), 2);
        }
    }
}

/// A host whose project folder isn't UTF-8 is found from elsewhere and its
/// exact folder is used (A§8.4): "using project …", then the answer, never
/// "Not my swamp".
#[test]
fn a_live_host_in_a_folder_that_isnt_utf8_is_used() {
    use std::os::unix::ffi::OsStrExt;
    let env = TestEnv::new();
    let dir = env
        .root()
        .join("projects")
        .join(std::ffi::OsStr::from_bytes(b"\xff\xfe-bad"));
    fs::create_dir(&dir).unwrap();
    qrec(&env, &dir, &["init", "--canvas", "160x90@30"]).ok();
    let mut host = env.command(Path::new(env!("CARGO_BIN_EXE_qrec")));
    let mut host = host
        .current_dir(&dir)
        .args(["host", "--headless", "--project"])
        .arg(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let elsewhere = env.empty_dir("elsewhere");
    fs::write(elsewhere.join("notes.txt"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        let status = qrec(&env, &elsewhere, &["status", "--json"]);
        if status.code == Some(0) || Instant::now() >= deadline {
            break status;
        }
        thread::sleep(Duration::from_millis(100));
    };
    // It idles out 2 s after serving; never leave it behind.
    let gone = Instant::now() + Duration::from_secs(10);
    while host.try_wait().unwrap().is_none() && Instant::now() < gone {
        thread::sleep(Duration::from_millis(100));
    }
    let _ = host.kill();
    let _ = host.wait();
    assert_eq!(status.code, Some(0), "{status:?}");
    assert!(status.stderr.contains("using project"), "{}", status.stderr);
    assert_eq!(status.reply()["data"]["host"], "headless");
}

/// A project folder whose path isn't UTF-8 (A§4.9, A§8.4) exports and
/// saves frames like any other (SPEC §13): ffmpeg gets every temp path byte
/// for byte, and replies and hints name files exactly (`\xFF` for a byte
/// that isn't UTF-8), never with a lossy `�`.
#[test]
fn a_project_folder_that_isnt_utf8_exports_and_saves_frames() {
    use std::os::unix::ffi::OsStrExt;
    let env = TestEnv::new();
    let dir = env
        .root()
        .join("projects")
        .join(std::ffi::OsStr::from_bytes(b"pr\xffoj"));
    fs::create_dir(&dir).unwrap();
    fixture_project(&dir, &[video(30)]);
    let shown = format!(r"{}/pr\xFFoj", env.root().join("projects").display());

    let export = qrec(&env, &dir, &["--json", "export", "--quality", "draft"]);
    export.ok();
    let path = export.reply()["data"]["result"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(path.starts_with(&format!("{shown}/exports/")), "{path}");
    let exported: Vec<PathBuf> = fs::read_dir(dir.join("exports"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(exported.len(), 1, "{exported:?}");
    assert!(fs::metadata(&exported[0]).unwrap().len() > 0);

    let frame = qrec(&env, &dir, &["--json", "frame", "save", "--at", "3f"]);
    frame.ok();
    let still = frame.reply()["data"]["result"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(still.starts_with(&format!("{shown}/frames/")), "{still}");
    assert_eq!(fs::read_dir(dir.join("frames")).unwrap().count(), 1);

    // A hand-picked output elsewhere works too (segments come from the cache).
    let out = env.root().join("out.mp4");
    qrec(
        &env,
        &dir,
        &[
            "--json",
            "export",
            "--quality",
            "draft",
            "--out",
            out.to_str().unwrap(),
        ],
    )
    .ok();
    assert!(out.is_file());

    // A failure points at its log exactly.
    fs::remove_file(dir.join("takes/take-0001/L1-screen-FAKE-1.mkv")).unwrap();
    let failed = qrec(&env, &dir, &["--json", "frame", "save", "--at", "4f"]);
    assert_eq!(failed.code, Some(1), "{failed:?}");
    let error = &failed.only_reply()["error"];
    let hint = error["hint"].as_str().unwrap();
    assert!(hint.contains(&format!("{shown}/.qrec/logs/")), "{error}");
    assert!(!failed.stdout.contains('\u{FFFD}'), "{}", failed.stdout);
}

/// SPEC §10.2, A§8.4: an absolute `--project` names the project folder,
/// so commands (an export with an absolute `--out` too) work from a
/// current directory that was deleted; without `--project` the error says
/// to pass an absolute one, not the flag that was already there.
#[test]
fn an_absolute_project_works_from_a_deleted_folder() {
    let env = TestEnv::new();
    let dir = project_with(&env, "far", &[video(30)]);
    let gone = env.root().join("gone");
    let from_gone = |args: &[&str]| {
        fs::create_dir_all(&gone).unwrap();
        let mut command = env.command(Path::new("sh"));
        command
            .current_dir(env.root())
            .args(["-c", r#"cd "$1" && rmdir "$1" && shift && exec "$@""#, "sh"])
            .arg(&gone)
            .arg(qrec_bin())
            .args(args);
        let out = run_tool(&mut command, Duration::from_secs(120));
        Out {
            code: out.status.code(),
            stdout: String::from_utf8(out.stdout).expect("stdout is UTF-8"),
            stderr: out.stderr,
        }
    };
    let project = dir.to_str().unwrap();
    let listed = from_gone(&["--json", "--project", project, "clip", "list"]);
    listed.ok();
    assert_eq!(listed.only_reply()["data"][0]["id"], "c001");
    let out = env.root().join("far.mp4");
    from_gone(&[
        "--json",
        "--project",
        project,
        "export",
        "--quality",
        "draft",
        "--out",
        out.to_str().unwrap(),
    ])
    .ok();
    assert!(out.is_file());

    let lost = from_gone(&["--json", "clip", "list"]);
    assert_eq!(lost.code, Some(1), "{lost:?}");
    let error = &lost.only_reply()["error"];
    assert_eq!(error["code"], "io");
    assert!(
        error["hint"]
            .as_str()
            .unwrap()
            .contains("absolute --project"),
        "{error}"
    );
    let relative = from_gone(&["--json", "--project", project, "export", "--out", "x.mp4"]);
    assert_eq!(relative.code, Some(1), "{relative:?}");
    assert!(
        relative.only_reply()["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("--out x.mp4 is relative"),
        "{relative:?}"
    );
}

/// The pid of the host serving `dir` (`qrec status`).
fn host_pid(env: &TestEnv, dir: &Path) -> u32 {
    let status = qrec(env, dir, &["status", "--json"]);
    status.ok();
    let data = &status.reply()["data"];
    assert_eq!(data["host"], "headless", "{data}");
    data["pid"].as_u64().expect("a host has a pid") as u32
}

/// SIGKILLs `pid` (a crash: nothing is finalized) and waits until it is gone.
fn crash(pid: u32) {
    let killed = run_tool(
        std::process::Command::new("kill").args(["-KILL", &pid.to_string()]),
        Duration::from_secs(10),
    );
    assert!(killed.status.success(), "kill: {}", killed.stderr);
    let deadline = Instant::now() + Duration::from_secs(10);
    let gone = || {
        fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| stat.contains(") Z "))
    };
    while !gone() {
        assert!(Instant::now() < deadline, "host {pid} didn't die");
        thread::sleep(Duration::from_millis(20));
    }
}

/// The take as `qrec take show --json` prints it.
fn take_show(env: &TestEnv, dir: &Path, take: &str) -> Value {
    let out = qrec(env, dir, &["take", "show", take, "--json"]);
    out.ok();
    out.reply()["data"].clone()
}

#[test]
fn an_audio_only_take_cut_short_by_a_crash_is_recovered_by_the_next_command() {
    let env = TestEnv::new();
    let dir = env.empty_dir("crash-audio");
    qrec(&env, &dir, &["init", "--canvas", "640x360@30"]).ok();
    qrec(&env, &dir, &["scene", "audio", "add", "mic"]).ok();
    qrec(&env, &dir, &["record", "start", "--json"]).ok();
    thread::sleep(Duration::from_millis(2500));
    crash(host_pid(&env, &dir));

    // A one-shot command recovers the take before it runs (SPEC §5.3.7),
    // so the take is usable at once.
    let add = qrec(&env, &dir, &["clip", "add", "take-0001", "--json"]);
    add.ok();
    assert_eq!(add.reply()["data"]["created"].as_array().unwrap().len(), 1);
    assert!(
        add.stderr.contains("Recovered take 1 from the bog."),
        "{}",
        add.stderr
    );

    // matroskamux never wrote a duration; the length comes from the FLAC
    // packets (4608 samples each at 48 kHz), not 0 frames.
    let take = take_show(&env, &dir, "take-0001");
    assert_eq!(take["status"], "ok", "{take}");
    let probe = ffprobe(&dir.join("takes/take-0001/A1-mic.mka"));
    assert!(
        probe["format"]["duration"].is_null(),
        "a crashed file has no duration: {probe}"
    );
    let packets: i64 = probe["streams"][0]["nb_read_packets"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let offset = take["streams"][0]["startOffsetNs"].as_i64().unwrap();
    let expected = (offset + packets * 96_000_000) * 30 / 1_000_000_000;
    assert_eq!(take["durationFrames"].as_i64(), Some(expected), "{take}");
    assert!(expected >= 45, "~2.5 s were recorded: {take}");
}

#[test]
fn a_take_recovered_after_a_crash_keeps_its_start_offsets() {
    let env = TestEnv::new();
    let dir = env.empty_dir("crash-offsets");
    qrec(&env, &dir, &["init", "--canvas", "640x360@30"]).ok();
    qrec(
        &env,
        &dir,
        &["scene", "layer", "add", "screen", "--output", "FAKE-2"],
    )
    .ok();
    qrec(&env, &dir, &["scene", "audio", "add", "mic"]).ok();
    qrec(&env, &dir, &["record", "start", "--json"]).ok();
    thread::sleep(Duration::from_millis(2000));
    crash(host_pid(&env, &dir));

    let rename = qrec(&env, &dir, &["project", "set", "--name", "after the crash"]);
    rename.ok();
    assert!(
        rename.stderr.contains("Recovered take 1 from the bog."),
        "{}",
        rename.stderr
    );
    let take = take_show(&env, &dir, "take-0001");
    assert_eq!(take["status"], "ok", "{take}");
    assert!(take["durationFrames"].as_u64().unwrap() >= 30, "{take}");
    // Each stream keeps the offset the engine measured when it started
    // (recorder.log: "<id>: first buffer at T0 + <ms> ms").
    let log = fs::read_to_string(dir.join("takes/take-0001/recorder.log")).unwrap();
    let streams = take["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 2, "{take}");
    for stream in streams {
        let id = stream["id"].as_str().unwrap();
        let prefix = format!(" {id}: first buffer at T0 + ");
        let line = log
            .lines()
            .find(|l| l.contains(&prefix))
            .unwrap_or_else(|| panic!("no first buffer of {id} in:\n{log}"));
        let ms: f64 = line
            .split(&prefix)
            .nth(1)
            .unwrap()
            .trim_end_matches(" ms")
            .parse()
            .unwrap();
        let offset = stream["startOffsetNs"].as_i64().unwrap();
        assert!(offset > 0, "{id}: {stream}");
        assert!(
            (offset as f64 - ms * 1e6).abs() <= 1_000.0,
            "{id}: {offset} ns vs {ms} ms in recorder.log"
        );
    }
}

#[test]
fn a_double_tap_of_record_toggle_is_too_short_not_broken() {
    let env = TestEnv::new();
    let dir = env.empty_dir("double-tap");
    qrec(&env, &dir, &["init", "--canvas", "640x360@30"]).ok();
    qrec(
        &env,
        &dir,
        &["scene", "layer", "add", "screen", "--output", "FAKE-1"],
    )
    .ok();
    qrec(&env, &dir, &["scene", "audio", "add", "mic"]).ok();

    // Two presses of a key bound to `qrec record toggle`, no host running:
    // the second one stops the take while it is still starting.
    let taps: Vec<Out> = thread::scope(|s| {
        let presses: Vec<_> = (0..2)
            .map(|_| s.spawn(|| qrec(&env, &dir, &["record", "toggle", "--json"])))
            .collect();
        presses.into_iter().map(|p| p.join().unwrap()).collect()
    });
    for tap in &taps {
        tap.ok();
    }
    let stop = taps
        .iter()
        .find(|t| t.reply()["data"]["result"]["status"].is_string())
        .unwrap_or_else(|| panic!("no finalized reply: {taps:?}"));
    let view = &stop.reply()["data"]["result"];
    // Never broken: every file is readable, the take is just short.
    assert_ne!(view["status"], "broken", "{view}");
    let take = take_show(&env, &dir, "take-0001");
    assert_eq!(take["status"], view["status"], "{take}");
    assert_eq!(take["error"], Value::Null, "{take}");
    let streams = take["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 2, "no stream left as unreadable: {take}");
    for stream in streams {
        let file = dir.join(stream["file"].as_str().unwrap());
        assert!(
            ffprobe(&file)["streams"][0].is_object(),
            "{}",
            file.display()
        );
    }
    if view["status"] == "tooShort" {
        let message = stop.reply()["data"]["message"].as_str().unwrap().to_owned();
        assert!(message.contains("was too short"), "{message}");
    }
}

// ------------------------------------------------------- review round 2

/// A `qrec` run in the background: its output is read as it comes (a full
/// pipe never blocks it) and it is killed if the test fails before it ends.
struct Background {
    child: std::process::Child,
    stdout: Option<thread::JoinHandle<Vec<String>>>,
    stderr: Option<thread::JoinHandle<String>>,
}

impl Background {
    fn spawn(mut command: std::process::Command) -> Self {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let mut err = child.stderr.take().unwrap();
        Self {
            child,
            stdout: Some(thread::spawn(move || {
                BufReader::new(out).lines().map_while(Result::ok).collect()
            })),
            stderr: Some(thread::spawn(move || {
                let mut text = String::new();
                let _ = std::io::Read::read_to_string(&mut err, &mut text);
                text
            })),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// `kill -<signal>` to the `qrec` process alone (not its children).
    fn signal(&self, signal: &str) {
        let sent = std::process::Command::new("kill")
            .args([format!("-{signal}"), self.pid().to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
    }

    /// Waits for the exit (at most `timeout`): its code, stdout lines and stderr.
    fn finish(mut self, timeout: Duration) -> (Option<i32>, Vec<String>, String) {
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "qrec (pid {}) didn't exit",
                self.pid()
            );
            thread::sleep(Duration::from_millis(20));
        };
        let out = self.stdout.take().unwrap().join().unwrap();
        let err = self.stderr.take().unwrap().join().unwrap();
        (status.code(), out, err)
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The 20 s, 720p fixture: an export of it takes a moment.
fn long_take() -> MediaSpec {
    MediaSpec::video(1280, 720, 30, 600).with_audio(48_000, 1, &[(0, 20_000)])
}

/// Temp outputs (`.<stem>.qrec-part-…`) left in the export and segment dirs.
fn temp_outputs(dir: &Path) -> Vec<PathBuf> {
    [dir.join("exports"), dir.join(".qrec/cache/segments")]
        .iter()
        .filter_map(|d| fs::read_dir(d).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains(".qrec-part-"))
        .collect()
}

/// Polls `cond` every 10 ms; panics with `what` after `timeout`.
fn wait_until(timeout: Duration, what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// A command run in-process (an export, whose host another client used
/// meanwhile) prints its result as soon as it has it and exits; a
/// recording asked right then runs in a headless host of its own. It used
/// to start inside the export's process, which then held its result back
/// for as long as the take lasted and whose Ctrl+C stopped the take
/// (A§8.5).
#[test]
fn a_recording_asked_as_a_command_finishes_gets_its_own_host() {
    let env = TestEnv::new();
    let dir = project_with(&env, "own-host", &[long_take()]);
    let mut command = env.command(&qrec_bin());
    command
        .current_dir(&dir)
        .args(["--json", "export", "--quality", "draft"]);
    let export = Background::spawn(command);
    // Another client uses its host meanwhile (an agent polling status).
    wait_until(Duration::from_secs(60), "the export's host", || {
        let status = qrec(&env, &dir, &["status", "--json"]);
        status.code == Some(0) && status.reply()["data"]["pid"] == export.pid()
    });
    // The moment its file is placed, a recording is asked for.
    let placed = || {
        fs::read_dir(dir.join("exports")).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.ends_with(".mp4") && !name.starts_with('.')
            })
        })
    };
    wait_until(Duration::from_secs(120), "the export", placed);
    let start = qrec(&env, &dir, &["record", "start", "--json"]);
    start.ok();
    assert_eq!(start.only_reply()["data"]["result"]["take"], "take-0002");

    // The export ended on its own with its one result: it never waited
    // for the take.
    let export_pid = export.pid();
    let (code, out, _) = export.finish(Duration::from_secs(30));
    assert_eq!(code, Some(0), "{out:?}");
    let last: Value = serde_json::from_str(out.last().unwrap()).unwrap();
    assert_eq!(last["ok"], true, "{out:?}");
    assert!(last["data"]["result"]["path"].is_string(), "{last}");
    assert_eq!(out.iter().filter(|l| l.starts_with(r#"{"ok""#)).count(), 1);
    // The take records in a headless host.
    let status = qrec(&env, &dir, &["status", "--json"]);
    status.ok();
    let data = &status.reply()["data"];
    assert_eq!(data["host"], "headless", "{data}");
    assert_ne!(data["pid"], export_pid);
    assert_eq!(data["runtime"]["record"]["phase"], "recording", "{data}");
    thread::sleep(Duration::from_millis(1500));
    qrec(&env, &dir, &["record", "stop"]).ok();
    wait_until_no_host(&env);
}

/// Ctrl+C twice on an in-process export whose ffmpeg doesn't die of
/// SIGTERM: the second one kills it at once and the run still ends the
/// normal way, with exactly one final object under --json (or a warning
/// for people), no temp output left, exit 130 (A§4.9, A§8.6). It used to
/// `_exit(130)` on the spot.
#[test]
fn a_second_ctrl_c_still_ends_the_run_properly() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    let dir = project_with(&env, "twice", &[long_take()]);
    // An ffmpeg that, once done or stopped, lingers ignoring SIGTERM: only
    // the SIGKILL 2 s after a cancel ends it.
    let bin = env.root().join("stubborn-bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("ffmpeg");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ntrap '' TERM INT\n'{}' \"$@\"\nstatus=$?\nsleep 30\nexit $status\n",
            ffmpeg_on_path().display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&path)))
            .unwrap();
    for json in [true, false] {
        let mut command = env.command(&qrec_bin());
        command.current_dir(&dir).env("PATH", &path);
        if json {
            command.arg("--json");
        }
        command.arg("export");
        let run = Background::spawn(command);
        // A segment is being rendered (its temp output exists).
        wait_until(Duration::from_secs(60), "a segment", || {
            !temp_outputs(&dir).is_empty()
        });
        run.signal("INT");
        thread::sleep(Duration::from_millis(300));
        let second = Instant::now();
        run.signal("INT");
        let (code, out, err) = run.finish(Duration::from_secs(20));
        assert_eq!(code, Some(130), "{out:?}\n{err}");
        assert!(
            second.elapsed() < Duration::from_secs(8),
            "{:?}",
            second.elapsed()
        );
        if json {
            let last: Value = serde_json::from_str(out.last().unwrap()).unwrap();
            assert_eq!(last["ok"], false, "{out:?}");
            assert_eq!(last["error"]["code"], "interrupted", "{last}");
            assert_eq!(last["error"]["message"], "export interrupted");
            assert!(
                out[..out.len() - 1]
                    .iter()
                    .all(|l| l.starts_with(r#"{"event":"progress""#)),
                "{out:?}"
            );
            assert!(
                err.lines().all(|l| l.starts_with(r#"{"event":"notice""#)),
                "--json keeps stderr for JSON lines: {err}"
            );
        } else {
            assert!(err.contains("qrec: warning: export interrupted"), "{err}");
        }
        assert_eq!(temp_outputs(&dir), Vec::<PathBuf>::new(), "json: {json}");
        let exports = fs::read_dir(dir.join("exports")).map_or(0, |d| d.count());
        assert_eq!(exports, 0, "nothing was exported");
        // The project is free again: no host was left behind.
        qrec(&env, &dir, &["status", "--json"]).ok();
    }
}

/// Ctrl+C on an export sent to a headless host: that host cancels the
/// jobs of a requester that is gone (A§5.8), so the final object says it
/// was cancelled, not that the host "may still finish it" (A§8.6).
#[test]
fn an_interrupted_export_routed_to_a_headless_host_says_it_was_cancelled() {
    let env = TestEnv::new();
    let dir = project_with(&env, "routed", &[long_take()]);
    let mut command = env.command(&qrec_bin());
    command.current_dir(&dir).args(["host", "--headless"]);
    let host = Background::spawn(command);
    wait_until(Duration::from_secs(30), "the headless host", || {
        let status = qrec(&env, &dir, &["status", "--json"]);
        status.code == Some(0) && status.reply()["data"]["host"] == "headless"
    });
    let mut command = env.command(&qrec_bin());
    command.current_dir(&dir).args(["--json", "export"]);
    let export = Background::spawn(command);
    wait_until(Duration::from_secs(60), "a segment", || {
        !temp_outputs(&dir).is_empty()
    });
    export.signal("INT");
    let (code, out, _) = export.finish(Duration::from_secs(30));
    assert_eq!(code, Some(130), "{out:?}");
    let last: Value = serde_json::from_str(out.last().unwrap()).unwrap();
    assert_eq!(last["error"]["code"], "interrupted", "{last}");
    assert_eq!(
        last["error"]["hint"],
        format!(
            "the headless host (pid {}) cancelled it when this command stopped; run the command again",
            host.pid()
        )
        .as_str()
    );
    // And so it did: nothing was exported, no temp output is left, and the
    // host idles out.
    let (code, _, _) = host.finish(Duration::from_secs(30));
    assert_eq!(code, Some(0));
    assert_eq!(temp_outputs(&dir), Vec::<PathBuf>::new());
    let exports = fs::read_dir(dir.join("exports")).map_or(0, |d| d.count());
    assert_eq!(exports, 0);
}

// ------------------------------------------------------- review round 3

/// The fixture project with take 2 left `recording` by a crash, and a
/// `PATH` whose ffprobe takes `secs` seconds: the next host holds its
/// commands that long for take 2's Recover probe (A§5.3).
fn held_by_a_slow_recovery(env: &TestEnv, name: &str, secs: u32) -> (PathBuf, OsString) {
    let dir = env.empty_dir(name);
    let project = fixture_project(&dir, &[video(30), video(45)]);
    crash_take_two(&dir, project);
    (dir, slow_ffprobe(env, secs))
}

/// `qrec <args>` in `dir`, with `PATH` set to `path`, in the background.
fn background(env: &TestEnv, dir: &Path, args: &[&str], path: &OsStr) -> Background {
    let mut command = env.command(&qrec_bin());
    command.current_dir(dir).args(args).env("PATH", path);
    Background::spawn(command)
}

/// Waits until a `kind` host of `dir` is ready (its lock record says so).
fn wait_for_host(dir: &Path, kind: &str) {
    let lock = dir.join(".qrec/lock");
    wait_until(Duration::from_secs(20), "the host", || {
        fs::read(&lock)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|r| r["kind"] == kind && r["state"] == "ready")
    });
}

/// Once take 2 is recovered, nothing records and no take 3 was made.
fn nothing_recorded_after_the_recovery(env: &TestEnv, dir: &Path) {
    wait_until(Duration::from_secs(40), "the recovery", || {
        take_row(env, dir, "take-0002")["status"] == "ok"
    });
    let status = qrec(env, dir, &["status", "--json"]);
    status.ok();
    let data = &status.reply()["data"];
    assert_eq!(data["runtime"]["record"]["phase"], "idle", "{data}");
    assert!(!dir.join("takes/take-0003").exists());
}

/// `record stop` while `record start` waits for the startup recovery of a
/// crashed take (A§5.3): the stop cancels the start, like a countdown, and
/// nothing records once the take is recovered. The stop used to answer
/// "not recording" and the start then recorded take 3 until a second stop.
#[test]
fn a_stop_cancels_a_start_waiting_for_the_startup_recovery() {
    let env = TestEnv::new();
    let (dir, path) = held_by_a_slow_recovery(&env, "held-start", 6);
    let start = background(&env, &dir, &["--json", "record", "start"], &path);
    wait_for_host(&dir, "headless");
    thread::sleep(Duration::from_millis(500));

    let stop = qrec(&env, &dir, &["--json", "record", "stop"]);
    stop.ok();
    assert_eq!(
        stop.only_reply()["data"]["message"],
        "Recording cancelled before it started"
    );
    let (code, out, _) = start.finish(Duration::from_secs(30));
    assert_eq!(code, Some(1), "{out:?}");
    let last: Value = serde_json::from_str(out.last().unwrap()).unwrap();
    assert_eq!(last["error"]["code"], "invalid", "{last}");
    assert_eq!(
        last["error"]["message"], "recording cancelled before it started",
        "{last}"
    );
    nothing_recorded_after_the_recovery(&env, &dir);
    wait_until_no_host(&env);
}

/// The first Ctrl+C of `record start --wait` while the start waits for the
/// startup recovery (A§8.6): its `RecordStop` cancels the start, so the
/// command ends with that answer and nothing records. The client used to
/// stream "Recording take 3: 00:01…" after the Ctrl+C, and a second one
/// left the take recording in the background.
#[test]
fn ctrl_c_on_a_waiting_record_start_wait_cancels_it() {
    let env = TestEnv::new();
    let (dir, path) = held_by_a_slow_recovery(&env, "held-wait", 6);
    let start = background(&env, &dir, &["--json", "record", "start", "--wait"], &path);
    wait_for_host(&dir, "headless");
    thread::sleep(Duration::from_millis(500));
    start.signal("INT");
    let (code, out, _) = start.finish(Duration::from_secs(30));
    assert_eq!(code, Some(1), "{out:?}");
    assert!(
        !out.iter().any(|l| l.contains("Recording take")),
        "nothing recorded: {out:?}"
    );
    let last: Value = serde_json::from_str(out.last().unwrap()).unwrap();
    assert_eq!(
        last["error"]["message"], "recording cancelled before it started",
        "{last}"
    );
    nothing_recorded_after_the_recovery(&env, &dir);
    wait_until_no_host(&env);
}

/// `export cancel` while an export waits for the startup recovery in its
/// own (in-process) host: the export is cancelled before it starts and
/// nothing is exported. It used to answer "no export is running" and the
/// export then ran to the end.
#[test]
fn export_cancel_cancels_an_export_waiting_for_the_startup_recovery() {
    let env = TestEnv::new();
    let (dir, path) = held_by_a_slow_recovery(&env, "held-export", 6);
    let export = background(&env, &dir, &["--json", "export"], &path);
    wait_for_host(&dir, "cli");
    thread::sleep(Duration::from_millis(500));

    let cancel = qrec(&env, &dir, &["--json", "export", "cancel"]);
    cancel.ok();
    assert_eq!(
        cancel.only_reply()["data"]["message"],
        "Export cancelled before it started"
    );
    let (code, out, _) = export.finish(Duration::from_secs(30));
    assert_eq!(code, Some(1), "{out:?}");
    let last: Value = serde_json::from_str(out.last().unwrap()).unwrap();
    assert_eq!(last["error"]["message"], "export cancelled", "{last}");
    assert_eq!(take_row(&env, &dir, "take-0002")["status"], "ok");
    let exports = fs::read_dir(dir.join("exports")).map_or(0, |d| d.count());
    assert_eq!(exports, 0, "nothing was exported");
}

/// A confirmed `history restore` over a broken `qrec.json` whose commit
/// record can't be written says nothing changed, and so it is: `qrec.json`
/// still holds the person's bytes (A§7.6, SPEC §16), and the restore works
/// once the disk does. The dropped commit used to leave the commit
/// record's project there instead.
#[test]
fn a_restore_that_cant_be_saved_keeps_the_broken_file() {
    let env = TestEnv::new();
    let dir = env.empty_dir("restore-unsaved");
    qrec(&env, &dir, &["init"]).ok();
    qrec(
        &env,
        &dir,
        &["scene", "layer", "add", "screen", "--output", "FAKE-1"],
    )
    .ok();
    qrec(&env, &dir, &["scene", "audio", "add", "mic"]).ok();
    let broken: &[u8] = b"{ \"my\": \"half-finished edit\"\n";
    fs::write(dir.join("qrec.json"), broken).unwrap();
    // Only the commit record can't be written (a stand-in for a full disk).
    let blocker = dir.join(".qrec/history.head.json.tmp");
    fs::create_dir(&blocker).unwrap();

    let restore = qrec(&env, &dir, &["--json", "history", "restore", "1", "--yes"]);
    assert_eq!(restore.code, Some(1), "{restore:?}");
    let error = &restore.reply()["error"];
    assert_eq!(error["code"], "io", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("couldn't save the project, so nothing changed"),
        "{error}"
    );
    fs::remove_dir(&blocker).unwrap();
    assert_eq!(fs::read(dir.join("qrec.json")).unwrap(), broken);
    let head: Value =
        serde_json::from_slice(&fs::read(dir.join(".qrec/history.head.json")).unwrap()).unwrap();
    assert_eq!(head["cursor"], 2);

    qrec(&env, &dir, &["history", "restore", "1", "--yes"]).ok();
    let status = qrec(&env, &dir, &["status", "--json"]);
    status.ok();
    let project = &status.reply()["data"]["project"];
    assert_eq!(project["scene"]["layers"][0]["id"], "L1", "{project}");
    assert_eq!(project["scene"]["audio"], json!([]), "{project}");
}

/// An unreadable `.qrec/history.base.json` is rebuilt by the next command,
/// which says so once, so the history can still rebuild a lost `qrec.json`
/// (A§7.7). It used to be called repaired by every command and never
/// written, and `history restore` then said there was nothing to recover
/// from.
#[test]
fn an_unreadable_history_base_is_rebuilt_by_the_next_command() {
    let env = TestEnv::new();
    let dir = env.empty_dir("unreadable-base");
    qrec(&env, &dir, &["init"]).ok();
    qrec(
        &env,
        &dir,
        &["scene", "layer", "add", "screen", "--output", "FAKE-1"],
    )
    .ok();
    let base_file = dir.join(".qrec/history.base.json");
    fs::write(&base_file, "garbage\n").unwrap();

    let first = qrec(&env, &dir, &["--json", "project", "set", "--name", "a"]);
    first.ok();
    assert!(
        first.stderr.contains("Qrec repaired the project's files")
            && first
                .stderr
                .contains("rebuilt the unreadable .qrec/history.base.json"),
        "{}",
        first.stderr
    );
    let base: Value = serde_json::from_slice(&fs::read(&base_file).unwrap()).unwrap();
    assert!(base["project"]["scene"].is_object(), "{base}");
    let second = qrec(&env, &dir, &["--json", "project", "set", "--name", "b"]);
    second.ok();
    assert!(!second.stderr.contains("repaired"), "{}", second.stderr);

    fs::remove_file(dir.join("qrec.json")).unwrap();
    fs::remove_file(dir.join(".qrec/history.head.json")).unwrap();
    qrec(&env, &dir, &["history", "restore", "3", "--yes"]).ok();
    let status = qrec(&env, &dir, &["status", "--json"]);
    status.ok();
    let project = &status.reply()["data"]["project"];
    assert_eq!(project["name"], "b", "{project}");
    assert_eq!(project["scene"]["layers"][0]["id"], "L1", "{project}");
}

/// An `ffmpeg` first on `PATH` that waits `secs` before each run that
/// reports progress (export segments, the stitch), so an export is still
/// running when the test looks; probes run at once.
fn slow_renders(env: &TestEnv, secs: u32) -> OsString {
    use std::os::unix::fs::PermissionsExt;
    let bin = env.root().join("slow-renders");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("ffmpeg");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \" $* \" in *' -progress '*) sleep {secs} ;; esac\nexec '{}' \"$@\"\n",
            ffmpeg_on_path().display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).unwrap()
}

/// Recording is blocked during an export, whichever process runs it (SPEC
/// §13.1, A§5.3): `record toggle` while an in-process `qrec export` runs is
/// refused at once (exit 6, the core's "an export is running"), as when a
/// headless or GUI host exports. It used to print nothing, wait for the
/// export to end and then start a take nobody was told about (or, past
/// 20 s, blame the export's command for not recording).
#[test]
fn a_recording_asked_during_an_in_process_export_is_refused_at_once() {
    let env = TestEnv::new();
    let dir = project_with(&env, "exporting", &[video(30)]);
    let mut command = env.command(&qrec_bin());
    command
        .current_dir(&dir)
        .env("PATH", slow_renders(&env, 50))
        .args(["--json", "export", "--quality", "draft"]);
    let export = Background::spawn(command);
    // The export's own process hosts the project and runs the export.
    wait_until(Duration::from_secs(60), "the export's host", || {
        let status = qrec(&env, &dir, &["status", "--json"]);
        status.code == Some(0) && {
            let data = &status.reply()["data"];
            data["host"] == "cli"
                && data["pid"] == export.pid()
                && data["runtime"]["jobs"]
                    .as_object()
                    .is_some_and(|jobs| jobs.values().any(|j| j["kind"]["kind"] == "export"))
        }
    });
    let asked = Instant::now();
    let toggle = qrec(&env, &dir, &["record", "toggle", "--json"]);
    let took = asked.elapsed();
    assert_eq!(toggle.code, Some(6), "{toggle:?}");
    // At once: the routing deadline it used to wait out is 20 s.
    assert!(
        took < Duration::from_secs(10),
        "answered at once, not when the export ends: {took:?}"
    );
    let error = &toggle.only_reply()["error"];
    assert_eq!(error["code"], "busy", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("an export is running (job-"),
        "{error}"
    );
    assert_eq!(
        error["hint"],
        "wait for it to finish, or cancel it with qrec export cancel"
    );
    // Nothing started meanwhile: the export's process still hosts the
    // project, recording nothing.
    let status = qrec(&env, &dir, &["status", "--json"]);
    status.ok();
    let data = &status.reply()["data"];
    assert_eq!(data["pid"], export.pid(), "{data}");
    assert_eq!(data["runtime"]["record"]["phase"], "idle", "{data}");
    qrec(&env, &dir, &["export", "cancel", "--json"]).ok();
    let (code, out, _) = export.finish(Duration::from_secs(30));
    assert_ne!(code, Some(0), "cancelled: {out:?}");
    wait_until_no_host(&env);
    let takes = qrec(&env, &dir, &["take", "list", "--json"]);
    takes.ok();
    assert_eq!(
        takes.reply()["data"].as_array().unwrap().len(),
        1,
        "no take was started: {}",
        takes.stdout
    );
}

/// The background recorder keeps nothing of its caller's but what the
/// hand-off gives it (A§8.3 step 5: the lock, the ready pipe, the host
/// log). A descriptor the caller of `qrec record start` holds open — the
/// `exec 9>lock; flock -n 9 || exit; qrec record toggle` of a debounced key
/// bind — is closed in it, so the next press isn't locked out for the whole
/// take. The recorder used to keep every inherited descriptor until the
/// take ended.
#[test]
fn the_background_recorder_keeps_none_of_the_callers_descriptors() {
    let env = TestEnv::new();
    let dir = env.empty_dir("fds");
    qrec(&env, &dir, &["init", "--canvas", "640x360@30"]).ok();
    qrec(
        &env,
        &dir,
        &["scene", "layer", "add", "screen", "--output", "FAKE-1"],
    )
    .ok();
    let held_file = env.root().join("held.txt");
    let mut command = env.command(Path::new("sh"));
    command
        .current_dir(&dir)
        .args(["-c", r#"exec 9>>"$1"; exec "$0" record start --json"#])
        .arg(qrec_bin())
        .arg(&held_file);
    let start = run_tool(&mut command, Duration::from_secs(60));
    assert!(start.status.success(), "{}", start.stderr);
    let held_file = fs::canonicalize(&held_file).unwrap();
    let pid = host_pid(&env, &dir);
    let held: Vec<PathBuf> = fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
        .collect();
    qrec(&env, &dir, &["record", "stop"]).ok();
    wait_until_no_host(&env);
    assert!(
        !held.contains(&held_file),
        "the recorder kept the caller's fd 9: {held:?}"
    );
    let lock = fs::canonicalize(dir.join(".qrec/lock")).unwrap();
    assert!(held.contains(&lock), "it still holds the lock: {held:?}");
}
