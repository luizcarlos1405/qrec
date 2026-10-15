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
use std::time::{Duration, Instant};

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
    assert_eq!(data["host"], "cli");
    assert!(data["pid"].as_u64().is_some());
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

/// Waits until every host of `env` exited (its socket went away, A§8.6).
fn wait_until_no_host(env: &TestEnv) {
    let deadline = Instant::now() + Duration::from_secs(15);
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
