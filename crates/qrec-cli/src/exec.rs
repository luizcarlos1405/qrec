//! Runs a parsed command line through a [`Backend`]: plans it, applies the
//! folder rule, sends the request and renders the reply with the exit
//! code of SPEC §14 / A§4.8.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use qrec_core::protocol::{
    AutocutPlanView, ClipRow, CmdResult, Command, ExportDone, FrameSavedView, HelloInfo,
    HistoryRow, Progress, Query, RecordStartedView, StatusView, TakeFinalizedView, TakeRow, Toast,
    ToastLevel, WireBody,
};
use qrec_core::{CoreError, ErrorCode};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::args::Cli;
use crate::backend::{Backend, Events, Fallback, Finish, ProjectRequest, RouteOpts};
use crate::output::Ui;
use crate::plan::{self, Local, OnEmpty, Plan, ProjectPlan, View};
use crate::resolve::{self, Target};
use crate::{config, human};

/// The exit code of a command stopped by Ctrl+C (128 + SIGINT).
pub const EXIT_INTERRUPTED: u8 = 130;

/// `qrec --json` without a command (`usage`, exit 2, docs/GUI.md G§3.2).
pub const GUI_NO_JSON: &str = "the GUI has no --json output; give a command";

/// The process context of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunEnv {
    /// The current directory; `None` when it can't be read (it was deleted).
    pub cwd: Option<PathBuf>,
    /// stderr is a terminal: the progress bar updates in place.
    pub err_tty: bool,
}

/// Runs a parsed command line; returns the exit code.
pub(crate) fn execute(
    cli: &Cli,
    backend: &mut dyn Backend,
    env: &RunEnv,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let g = &cli.global;
    let mut ui = Ui::new(out, err, g.json, g.quiet, g.verbose, env.err_tty);
    backend.logging(g.verbose, g.quiet);
    // A deleted current directory only matters for what is relative to it:
    // `--out`, `--project`, or no `--project` at all.
    let planned = plan::plan(cli, env.cwd.as_deref());
    let result = match planned {
        Err(e) => Err(e),
        // The window prints nothing on stdout: `--json` has nothing to say.
        Ok(Plan::Gui) if g.json => Err(CoreError::usage(GUI_NO_JSON)),
        Ok(Plan::Gui) => project_dir(cli, env.cwd.as_deref())
            .and_then(|(dir, explicit)| backend.gui(&dir, explicit)),
        Ok(Plan::Local(local)) => run_local(&local, cli, backend, env, &mut ui),
        Ok(Plan::Project(plan)) => project_dir(cli, env.cwd.as_deref())
            .map(|(dir, explicit)| run_project(&plan, &dir, explicit, backend, &mut ui)),
    };
    let code = match result {
        Ok(code) => code,
        Err(e) => ui.fail(&e),
    };
    ui.finish(code)
}

/// What `qrec version` and `qrec --version` print under `--json`.
pub(crate) fn version_data() -> Value {
    json!({ "version": env!("CARGO_PKG_VERSION") })
}

fn no_cwd() -> CoreError {
    CoreError::io("can't read the current directory (was it deleted?)")
        .with_hint("cd into an existing folder, or pass an absolute --project DIR")
}

/// The folder a project command starts from (A§8.4) and whether it came
/// from `--project`: that folder, made absolute against the current
/// directory, else the current directory. An absolute `--project` needs no
/// current directory (it may have been deleted).
fn project_dir(cli: &Cli, cwd: Option<&Path>) -> Result<(PathBuf, bool), CoreError> {
    match (&cli.global.project, cwd) {
        (Some(dir), cwd) => plan::absolute_from(cwd, dir, "--project").map(|dir| (dir, true)),
        (None, Some(cwd)) => Ok((cwd.to_path_buf(), false)),
        (None, None) => Err(no_cwd()),
    }
}

// --------------------------------------------------------------- project-less

fn run_local(
    local: &Local,
    cli: &Cli,
    backend: &mut dyn Backend,
    env: &RunEnv,
    ui: &mut Ui<'_>,
) -> Result<u8, CoreError> {
    match local {
        Local::Version => {
            if ui.json {
                ui.print_ok(&version_data());
            } else {
                ui.print(&format!("qrec {}", env!("CARGO_PKG_VERSION")));
            }
        }
        Local::Completions(shell) => {
            let mut script = Vec::new();
            clap_complete::generate(*shell, &mut Cli::command(), "qrec", &mut script);
            let script = String::from_utf8_lossy(&script);
            if ui.json {
                ui.print_ok(&json!({ "shell": shell.to_string(), "script": script }));
            } else {
                ui.print_text(&script);
            }
        }
        Local::Schema => {
            let schema = qrec_core::schema::project_schema();
            if ui.json {
                ui.print_ok(&schema);
            } else {
                let text = serde_json::to_string_pretty(&schema)
                    .map_err(|e| CoreError::internal(format!("can't print the schema: {e}")))?;
                ui.print_text(&text);
            }
        }
        Local::AgentGuide => {
            let guide = qrec_core::agent_guide::agent_guide();
            if ui.json {
                ui.print_ok(&Value::String(guide));
            } else {
                ui.print_text(&guide);
            }
        }
        Local::Keys => {
            let path = backend.config_dir()?.join("keys.toml");
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
                Err(e) => {
                    return Err(CoreError::io(format!("can't read {}: {e}", path.display())));
                }
            };
            let map = qrec_core::keymap::resolve(&text);
            if ui.json {
                for problem in human::key_problems(&map) {
                    ui.notice(&Toast::new(ToastLevel::Warning, problem));
                }
                ui.print_ok(&human::keys_json(&map));
            } else {
                for line in human::keys(&map) {
                    ui.print(&line);
                }
            }
        }
        Local::ConfigList => {
            let path = config::file(&backend.config_dir()?);
            let cfg = config::load(&path)?;
            let values = config::values(&cfg);
            if ui.json {
                ui.print_ok(&json!({ "file": path.display().to_string(), "config": values }));
            } else {
                ui.print(&format!("# {}", path.display()));
                for (key, value) in config::flatten(&values) {
                    ui.print(&format!("{key} = {}", config::display(&value)));
                }
            }
        }
        Local::ConfigGet(key) => {
            let path = config::file(&backend.config_dir()?);
            let value = config::get(&config::load(&path)?, key)?;
            if ui.json {
                ui.print_ok(&json!({ "key": key, "value": value }));
            } else {
                ui.print(&config::display(&value));
            }
        }
        Local::ConfigSet { key, value } => {
            let path = config::file(&backend.config_dir()?);
            let new = config::set(&path, key, value)?;
            if ui.json {
                ui.print_ok(&json!({
                    "key": key, "value": new, "file": path.display().to_string()
                }));
            } else if !ui.quiet {
                ui.print(&format!("Set {key} = {}", config::display(&new)));
            }
        }
        Local::Doctor => {
            let report = backend.doctor()?;
            let (lines, failed) = human::doctor(&report, ui.verbose);
            if failed {
                let error = CoreError::new(
                    ErrorCode::MissingDependency,
                    "some required dependencies are missing",
                )
                .with_hint("install what `qrec doctor` lists as MISSING");
                if ui.json {
                    return Err(error.with_details(report));
                }
                for line in &lines {
                    ui.print(line);
                }
                return Ok(error.exit_code());
            }
            if ui.json {
                ui.print_ok(&report);
            } else {
                for line in &lines {
                    ui.print(line);
                }
            }
        }
        Local::Sources => {
            let inventory = backend.sources()?;
            if ui.json {
                ui.print_ok(&inventory);
            } else {
                for line in human::sources(&inventory) {
                    ui.print(&line);
                }
            }
        }
        Local::Host(args) => {
            let (project, _) = project_dir(cli, env.cwd.as_deref())?;
            return backend.host(&project, args);
        }
    }
    Ok(0)
}

// ------------------------------------------------------------------- project

/// Prints what happens while a request runs.
struct Live<'u, 'a, 'p> {
    ui: &'u mut Ui<'a>,
    /// The live host that served the request; `None` in-process.
    host: Option<HelloInfo>,
    /// [`ProjectPlan::streams`](crate::plan::ProjectPlan::streams).
    streams: bool,
    /// How to show the reply when it comes early ([`Events::replied`]):
    /// the plan and the project folder; `None` shows it only at the end.
    show: Option<(&'p ProjectPlan, &'p Path)>,
    /// The exit code of the reply already shown.
    shown: Option<u8>,
}

impl Events for Live<'_, '_, '_> {
    fn routed(&mut self, host: Option<&HelloInfo>) {
        self.host = host.cloned();
        match host {
            Some(h) => self.ui.detail(&format!(
                "sent to the {} host of {} (pid {})",
                h.host.as_str(),
                h.project_dir,
                h.pid
            )),
            None => self
                .ui
                .detail("no host runs this project: running it in this process"),
        }
    }

    fn accepted(&mut self) {}

    fn progress(&mut self, progress: &Progress) {
        // Under --json only streaming commands print progress lines; the
        // others print exactly one object (SPEC §14).
        if self.streams || !self.ui.json {
            self.ui.progress(progress);
        }
    }

    fn notice(&mut self, toast: &Toast) {
        self.ui.notice(toast);
    }

    /// An in-process command's result is shown as soon as it is known,
    /// even while its process still hosts other clients' requests (A§8.5).
    fn replied(&mut self, reply: &Result<Value, CoreError>) {
        let Some((plan, dir)) = self.show else {
            return;
        };
        if self.shown.is_some() {
            return;
        }
        let code = match reply {
            Ok(data) => render(plan, data.clone(), dir, self.host.as_ref(), self.ui),
            Err(e) => self.ui.fail(e),
        };
        self.shown = Some(code);
    }
}

/// Runs a project command from `dir` ([`project_dir`]; `explicit` when it
/// came from `--project`).
fn run_project(
    plan: &ProjectPlan,
    dir: &Path,
    explicit: bool,
    backend: &mut dyn Backend,
    ui: &mut Ui<'_>,
) -> u8 {
    // `--project` names the folder; `qrec init` acts on the folder itself.
    let allow_host_fallback = !explicit && plan.host_fallback;
    let resolved = match resolve::resolve(dir, allow_host_fallback, || backend.live_hosts()) {
        Ok(resolved) => resolved,
        Err(e) => return ui.fail(&e),
    };
    if let Some(notice) = &resolved.notice {
        ui.info(notice.clone());
    }
    if let Target::Empty(dir) = &resolved.target {
        match plan.on_empty {
            OnEmpty::Answer => return answer_empty(plan, dir, ui),
            OnEmpty::Send => {}
            OnEmpty::Init | OnEmpty::Refuse if plan.route.fallback == Fallback::None => {
                // No host can hold a selection or a GUI for a folder that
                // isn't a project yet.
                return ui.fail(&needs_host(plan));
            }
            // SPEC §4.4: only a change that sets a project up (or a
            // recording) creates one; the rest have nothing to work on.
            OnEmpty::Refuse => return ui.fail(&qrec_core::apply::no_project_yet()),
            OnEmpty::Init => {
                if let Err(code) = lazy_init(plan, dir, backend, ui) {
                    return code;
                }
            }
        }
    }
    let dir = resolved.target.dir();
    let mut live = Live {
        ui: &mut *ui,
        host: None,
        streams: plan.streams,
        show: Some((plan, dir)),
        shown: None,
    };
    let finish = backend.request(dir, plan.request.clone(), &plan.route, &mut live);
    let (host, shown) = (live.host, live.shown);
    match (finish, shown) {
        // Shown when it came (an in-process command whose process then
        // went on serving other clients for a while).
        (Finish::Reply(_), Some(code)) => code,
        (Finish::Reply(Ok(data)), None) => render(plan, data, dir, host.as_ref(), ui),
        (Finish::Reply(Err(e)), None) => ui.fail(&e),
        (Finish::NoHost, _) => ui.fail(&needs_host(plan)),
        (Finish::Interrupted { reply }, shown) => {
            interrupted(plan, reply, shown.is_some(), dir, host.as_ref(), ui)
        }
    }
}

/// Ctrl+C or SIGTERM stopped the request: exit 130 (A§8.6), and a warning
/// for people on stderr. A command that finished (or committed) before the
/// signal still shows its result (once: `shown` says it is out already).
/// Otherwise `--json` still ends stdout with exactly one object (SPEC
/// §14): an `interrupted` error whose hint says whether the host may still
/// finish the request, with the host's own error (e.g. "export cancelled")
/// in `details.reply`.
fn interrupted(
    plan: &ProjectPlan,
    reply: Option<Result<Value, CoreError>>,
    shown: bool,
    dir: &Path,
    host: Option<&HelloInfo>,
    ui: &mut Ui<'_>,
) -> u8 {
    let what = plan.name.strip_prefix("qrec ").unwrap_or(&plan.name);
    let title = format!("{what} interrupted");
    let error = match reply {
        _ if shown => None,
        Some(Ok(data)) => {
            render(plan, data, dir, host, ui);
            None
        }
        Some(Err(e)) => {
            Some(interrupted_error(&title, plan, host).with_details(json!({ "reply": e })))
        }
        None => Some(interrupted_error(&title, plan, host)),
    };
    match error {
        Some(error) if ui.json => ui.fail(&error),
        _ => {
            ui.notice(&Toast::new(ToastLevel::Warning, title));
            EXIT_INTERRUPTED
        }
    }
}

/// The `interrupted` error (exit 130) of a request that got no answer. Its
/// hint says what becomes of the request (A§5.8): a GUI host may still
/// finish it, and so may any host for work that isn't a job of this
/// command (a recording, an edit, a purge); headless and CLI hosts cancel
/// the export, frame save or autocut of a command that stops.
fn interrupted_error(message: &str, plan: &ProjectPlan, host: Option<&HelloInfo>) -> CoreError {
    let job_ends = matches!(
        &plan.request.body,
        WireBody::Command(cmd) if cmd.job_ends_with_requester()
    );
    let hint = match host {
        Some(h) if job_ends && !h.host.keeps_orphaned_jobs() => format!(
            "the {} host (pid {}) cancelled it when this command stopped; run the command again",
            h.host.as_str(),
            h.pid
        ),
        Some(h) => format!(
            "the {} host (pid {}) may still finish it; check with `qrec status`",
            h.host.as_str(),
            h.pid
        ),
        None => "jobs it started were cancelled and their temporary files deleted; \
                 check with `qrec status`"
            .to_owned(),
    };
    CoreError::new(ErrorCode::Interrupted, message).with_hint(hint)
}

/// The `unavailable` error of a request only a live host answers when none
/// runs: the plan's own ([`ProjectPlan::needs_host`]), whose hint says what
/// works for that command.
fn needs_host(plan: &ProjectPlan) -> CoreError {
    plan.needs_host.clone().unwrap_or_else(|| {
        CoreError::unavailable(format!(
            "`{}` needs a running Qrec host, and none runs for this project",
            plan.name
        ))
        .with_hint("open the project in Qrec and try again")
    })
}

/// The core's answer to `Init` in a folder that already holds a project.
const ALREADY_A_PROJECT: &str = "already a project";

/// The first change in an empty folder creates the project with the
/// user's defaults (SPEC §4.4); the notice goes to stderr so stdout keeps
/// only the command's own reply. A project that appeared meanwhile (another
/// command initialized it first) is fine: the command runs on it.
fn lazy_init(
    plan: &ProjectPlan,
    dir: &Path,
    backend: &mut dyn Backend,
    ui: &mut Ui<'_>,
) -> Result<(), u8> {
    let request = ProjectRequest {
        body: WireBody::Command(Command::Init {
            canvas: None,
            name: None,
        }),
        confirmed: false,
    };
    let mut live = Live {
        ui: &mut *ui,
        host: None,
        streams: false,
        // Init's own reply becomes a notice below, never the command's.
        show: None,
        shown: None,
    };
    let finish = backend.request(dir, request, &RouteOpts::command(), &mut live);
    let host = live.host;
    match finish {
        Finish::Reply(Ok(data)) => {
            let message = parse::<CmdResult>(&data)
                .map(|r| r.message)
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| format!("created a project in {}", dir.display()));
            ui.info(message);
            Ok(())
        }
        Finish::Reply(Err(e)) if e.code == ErrorCode::Invalid && e.message == ALREADY_A_PROJECT => {
            Ok(())
        }
        Finish::Reply(Err(e)) => Err(ui.fail(&e)),
        Finish::NoHost => Err(ui.fail(&CoreError::internal(
            "creating the project needs no host, yet none was found",
        ))),
        // The command itself never ran.
        Finish::Interrupted { .. } => Err(interrupted(plan, None, false, dir, host.as_ref(), ui)),
    }
}

/// A query in an empty folder: there is nothing yet, and nothing is
/// created (A§8.4: read-only commands write nothing).
fn answer_empty(plan: &ProjectPlan, dir: &Path, ui: &mut Ui<'_>) -> u8 {
    let dir_text = dir.display().to_string();
    match plan.view {
        View::Status | View::StatusJson => {
            if ui.json {
                // The keys of `status_json`, with nothing in them.
                ui.print_ok(&json!({
                    "path": dir_text, "initialized": false, "host": null, "pid": null,
                    "project": null, "ui": null, "history": null, "runtime": null,
                }));
            } else {
                for line in human::status_empty(&dir_text) {
                    ui.print(&line);
                }
            }
        }
        View::TakeList => print_rows::<TakeRow>(ui, &[], human::take_list),
        View::ClipList => print_rows::<ClipRow>(ui, &[], human::clip_list),
        View::History => print_rows::<HistoryRow>(ui, &[], human::history),
        View::TakeShow => {
            let take = match &plan.request.body {
                WireBody::Query(Query::TakeShow { take }) => take.to_string(),
                _ => "such take".to_owned(),
            };
            return ui.fail(
                &CoreError::not_found(format!("there is no {take}"))
                    .with_hint("this folder isn't a project yet: it has no takes"),
            );
        }
        View::Command | View::Autocut { .. } | View::Export | View::FrameSave | View::Record => {
            return ui.fail(&CoreError::internal(format!(
                "{} has no answer for a folder without a project",
                plan.name
            )));
        }
    }
    0
}

fn print_rows<T: serde::Serialize>(ui: &mut Ui<'_>, rows: &[T], human: fn(&[T]) -> Vec<String>) {
    if ui.json {
        ui.print_ok(&serde_json::to_value(rows).unwrap_or(Value::Null));
    } else {
        for line in human(rows) {
            ui.print(&line);
        }
    }
}

fn parse<T: DeserializeOwned>(value: &Value) -> Option<T> {
    serde_json::from_value(value.clone()).ok()
}

/// Prints a reply; returns the exit code (0).
fn render(
    plan: &ProjectPlan,
    data: Value,
    dir: &Path,
    host: Option<&HelloInfo>,
    ui: &mut Ui<'_>,
) -> u8 {
    if ui.json {
        let data = match plan.view {
            View::StatusJson => status_json(data, dir, host),
            _ => data,
        };
        ui.print_ok(&data);
        return 0;
    }
    let dir_text = dir.display().to_string();
    let lines: Option<Vec<String>> = match plan.view {
        View::Status | View::StatusJson => {
            parse::<StatusView>(&data).map(|view| human::status(&view, &dir_text, host))
        }
        View::TakeList => parse::<Vec<TakeRow>>(&data).map(|rows| human::take_list(&rows)),
        View::TakeShow => Some(human::take_show(&data)),
        View::ClipList => parse::<Vec<ClipRow>>(&data).map(|rows| human::clip_list(&rows)),
        View::History => parse::<Vec<HistoryRow>>(&data).map(|rows| human::history(&rows)),
        View::Command | View::Autocut { .. } | View::Export | View::FrameSave | View::Record => {
            match parse::<CmdResult>(&data) {
                Some(result) => {
                    command_result(plan.view, &result, ui);
                    return 0;
                }
                None => None,
            }
        }
    };
    match lines {
        Some(lines) => {
            for line in lines {
                ui.print(&line);
            }
        }
        // A reply this CLI doesn't know how to draw: show it as it is.
        None => ui.print(&serde_json::to_string_pretty(&data).unwrap_or_default()),
    }
    0
}

/// `qrec status --json`: the `Query::State` object plus where it lives and
/// the host that holds the project (`host`, `pid`): the live process that
/// answered, or `null` and `null` when none runs and this process read the
/// project itself (A§8.4), like the human view's "none running". A host is
/// a process that holds the project lock (A§8.3), so `host` never names
/// this short-lived reader, whose pid is gone once the answer is read.
fn status_json(data: Value, dir: &Path, host: Option<&HelloInfo>) -> Value {
    let mut object = match data {
        Value::Object(object) => object,
        other => {
            let mut object = serde_json::Map::new();
            object.insert("state".into(), other);
            object
        }
    };
    let (kind, pid) = match host {
        Some(hello) => (json!(hello.host.as_str()), json!(hello.pid)),
        None => (Value::Null, Value::Null),
    };
    let path = host.map_or_else(|| dir.display().to_string(), |h| h.project_dir.clone());
    object.insert("path".into(), json!(path));
    object.insert("initialized".into(), json!(true));
    object.insert("host".into(), kind);
    object.insert("pid".into(), pid);
    Value::Object(object)
}

/// A command's reply for people: its message, then what its view adds.
fn command_result(view: View, result: &CmdResult, ui: &mut Ui<'_>) {
    let message = result.message.trim();
    let mut lines: Vec<String> = Vec::new();
    let mut extra: Vec<String> = Vec::new();
    match view {
        View::Autocut { dry_run } => {
            if let Some(plan) = parse::<AutocutPlanView>(&result.result) {
                let mut all = human::autocut_plan(&plan, dry_run);
                if dry_run {
                    // The table, then the core's summary instead of ours.
                    if !message.is_empty() {
                        all.pop();
                    }
                    lines.extend(all);
                } else {
                    // Only the clips autocut left alone, after the message.
                    extra.extend(all.into_iter().filter(|l| l.starts_with("skipped ")));
                }
            }
        }
        View::Export => {
            if let Some(done) = parse::<ExportDone>(&result.result)
                && !message.contains(&done.path)
            {
                extra.push(human::export_done(&done.path, done.bytes, done.frames));
            }
        }
        View::FrameSave => {
            if let Some(saved) = parse::<FrameSavedView>(&result.result)
                && !message.contains(&saved.path)
            {
                extra.push(saved.path);
            }
        }
        View::Record => {
            if let Some(started) = parse::<RecordStartedView>(&result.result) {
                extra.extend(human::record_started(&started));
            } else if let Some(done) = parse::<TakeFinalizedView>(&result.result) {
                extra.extend(human::take_finalized(&done));
            }
        }
        _ => {}
    }
    let dry_run = matches!(view, View::Autocut { dry_run: true });
    if !ui.quiet || dry_run {
        for line in lines {
            ui.print(&line);
        }
        if !message.is_empty() {
            ui.print(message);
        }
        for line in extra {
            ui.print(&line);
        }
    }
    if let Some(seq) = result.seq {
        ui.detail(&format!("history seq {seq}"));
    }
    if !result.created.is_empty() {
        ui.detail(&format!("created {}", result.created.join(", ")));
    }
    if !result.removed.is_empty() {
        ui.detail(&format!("removed {}", result.removed.join(", ")));
    }
}
