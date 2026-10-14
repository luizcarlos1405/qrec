//! What a parsed command line runs (A§4.4, A§10): a request for the
//! project's host with its routing options and the way to show the reply,
//! or a project-less action.
//!
//! Pure apart from the current directory it is given: times and canvas
//! specs are parsed with the core's grammars (`TimeArg`, `CanvasSpec`), so
//! a bad value is a `usage` error (exit 2) with the core's hint, and
//! `--out`/`--project` paths are made absolute against `cwd`.

use std::path::{Component, Path, PathBuf};

use qrec_core::protocol::{
    AudioPatch, AutocutOverrides, AutocutPatch, CanvasSpec, ClipScope, Command, ExportFormat,
    InsertAt, LayerPatch, MoveTo, Nullable, Quality, Query, RecordModeArg, SceneDest, SceneTarget,
    SeekTarget, VideoCodec, VideoSourceArg, WireBody,
};
use qrec_core::{CoreError, TimeArg};

use crate::args::*;
use crate::backend::{Fallback, ProjectRequest, RECORD_DEADLINE, RouteOpts};

/// What a command line runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    /// `qrec` without a command: the GUI.
    Gui,
    /// A command that needs no project (A§10).
    Local(Local),
    /// A request for the project's host.
    Project(ProjectPlan),
}

/// Project-less commands: they work in any folder.
#[derive(Clone, Debug, PartialEq)]
pub enum Local {
    Version,
    Completions(clap_complete::Shell),
    Doctor,
    Sources,
    Schema,
    AgentGuide,
    Keys,
    /// `qrec config`: every setting.
    ConfigList,
    ConfigGet(String),
    ConfigSet {
        key: String,
        value: String,
    },
    /// The hidden `qrec host --headless`.
    Host(HostArgs),
}

/// A request for the project's host and how to show its reply.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectPlan {
    /// The command as typed, e.g. `qrec clip split`, for messages.
    pub name: String,
    pub request: ProjectRequest,
    pub route: RouteOpts,
    pub view: View,
    pub on_empty: OnEmpty,
    /// Why only a live host can answer ([`Fallback::None`]): the
    /// `unavailable` message when none runs.
    pub needs_host: Option<String>,
    /// A streaming command (SPEC §14: export, `record … --wait`): under
    /// `--json` its progress goes to stdout as JSON lines before the reply.
    /// Every other command prints exactly one object (people still see
    /// its progress on stderr).
    pub streams: bool,
}

/// What a strictly empty folder, which isn't a project yet (SPEC §4.4),
/// does with the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnEmpty {
    /// `qrec init`: send it as it is.
    Send,
    /// Create the project first (`Init` with the user's defaults), then
    /// send the request: the first project command initializes an empty folder.
    Init,
    /// A query: answer it here (no takes, no clips, no history) without
    /// creating anything.
    Answer,
}

/// How a reply is shown to people (`--json` prints it as it is, except
/// for `status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// A command: its one-line `message`.
    Command,
    /// `Query::Status`.
    Status,
    /// `Query::State` plus host information (`qrec status --json`).
    StatusJson,
    TakeList,
    TakeShow,
    ClipList,
    History,
    /// `AutocutPlanView` in `result`: the planned or applied cuts.
    Autocut {
        dry_run: bool,
    },
    /// `ExportDone` in `result`, with a progress bar while it runs.
    Export,
    /// `FrameSavedView` in `result`.
    FrameSave,
    /// `RecordStartedView` or `TakeFinalizedView` in `result`.
    Record,
}

/// Maps a parsed command line to its plan. `cwd` makes `--out` absolute.
pub fn plan(cli: &Cli, cwd: &Path) -> Result<Plan, CoreError> {
    let Some(command) = &cli.command else {
        return Ok(Plan::Gui);
    };
    let local = |local| Ok(Plan::Local(local));
    let builder = Builder {
        confirmed: cli.global.yes,
        json: cli.global.json,
    };
    match command {
        Cmd::Version => local(Local::Version),
        Cmd::Completions(args) => local(Local::Completions(args.shell)),
        Cmd::Doctor => local(Local::Doctor),
        Cmd::Sources(SourcesCmd::List) => local(Local::Sources),
        Cmd::Schema => local(Local::Schema),
        Cmd::AgentGuide => local(Local::AgentGuide),
        Cmd::Keys => local(Local::Keys),
        Cmd::Config(args) => local(match &args.command {
            None => Local::ConfigList,
            Some(ConfigCmd::Get(get)) => Local::ConfigGet(get.key.clone()),
            Some(ConfigCmd::Set(set)) => Local::ConfigSet {
                key: set.key.clone(),
                value: set.value.clone(),
            },
        }),
        Cmd::Host(args) => local(Local::Host(args.clone())),
        other => builder.project(other, cwd).map(Plan::Project),
    }
}

/// Builds project plans with the global flags that shape every request.
struct Builder {
    confirmed: bool,
    json: bool,
}

impl Builder {
    fn request(&self, body: WireBody) -> ProjectRequest {
        ProjectRequest {
            body,
            confirmed: self.confirmed,
        }
    }

    /// A command: run under the lock, the first one initializes an empty folder.
    fn command(&self, name: &str, cmd: Command, view: View) -> ProjectPlan {
        ProjectPlan {
            name: format!("qrec {name}"),
            request: self.request(WireBody::Command(cmd)),
            route: RouteOpts::command(),
            view,
            on_empty: OnEmpty::Init,
            needs_host: None,
            streams: false,
        }
    }

    /// A query: no lock, answered here for an empty folder.
    fn query(&self, name: &str, query: Query, view: View) -> ProjectPlan {
        ProjectPlan {
            name: format!("qrec {name}"),
            request: self.request(WireBody::Query(query)),
            route: RouteOpts::query(),
            view,
            on_empty: OnEmpty::Answer,
            needs_host: None,
            streams: false,
        }
    }

    fn project(&self, command: &Cmd, cwd: &Path) -> Result<ProjectPlan, CoreError> {
        use View::Command as Msg;
        let plan = match command {
            Cmd::Init(args) => ProjectPlan {
                on_empty: OnEmpty::Send,
                ..self.command(
                    "init",
                    Command::Init {
                        canvas: canvas(args.canvas.as_deref())?,
                        name: args.name.clone(),
                    },
                    Msg,
                )
            },
            Cmd::Status if self.json => self.query("status", Query::State, View::StatusJson),
            Cmd::Status => self.query("status", Query::Status, View::Status),
            Cmd::Project(ProjectCmd::Set(args)) => self.command(
                "project set",
                Command::ProjectSet {
                    name: args.name.clone(),
                    canvas: canvas(args.canvas.as_deref())?,
                    background: args.background.clone(),
                    countdown_secs: args.countdown,
                    encoder: args.encoder.map(Into::into),
                    vaapi_device: auto_or(args.vaapi_device.clone()),
                },
                Msg,
            ),
            Cmd::Scene(scene) => self.scene(scene),
            Cmd::Record(record) => self.record(record),
            Cmd::Take(take) => self.take(take),
            Cmd::Clip(clip) => self.clip(clip)?,
            Cmd::Autocut(args) => self.autocut(args),
            Cmd::Undo(args) => self.command("undo", Command::Undo { steps: args.steps }, Msg),
            Cmd::Redo(args) => self.command("redo", Command::Redo { steps: args.steps }, Msg),
            Cmd::History(args) => self.history(args),
            Cmd::Export(args) => self.export(args, cwd)?,
            Cmd::Frame(FrameCmd::Save(args)) => self.command(
                "frame save",
                Command::FrameSave {
                    at: time(&args.at)?,
                    layer: args.layer.clone(),
                    out: args.out.as_deref().map(|p| out_path(cwd, p)).transpose()?,
                },
                View::FrameSave,
            ),
            Cmd::Seek(args) => self.gui_only("seek", Command::Seek { to: seek(args)? }),
            Cmd::Play => self.gui_only("play", Command::Play),
            Cmd::Pause => self.gui_only("pause", Command::Pause),
            Cmd::Version
            | Cmd::Completions(_)
            | Cmd::Doctor
            | Cmd::Sources(_)
            | Cmd::Schema
            | Cmd::AgentGuide
            | Cmd::Keys
            | Cmd::Config(_)
            | Cmd::Host(_) => {
                return Err(CoreError::internal(
                    "a project-less command reached the project planner",
                ));
            }
        };
        Ok(plan)
    }

    /// Only a running GUI can answer (A§8.5): nothing runs in-process.
    fn gui_only(&self, name: &str, cmd: Command) -> ProjectPlan {
        let mut plan = self.command(name, cmd, View::Command);
        plan.route.fallback = Fallback::None;
        plan.needs_host = Some(format!(
            "`qrec {name}` needs the Qrec GUI: open this project in Qrec first"
        ));
        plan
    }

    /// Acts on the selection of the running host, so it needs one (A§8.4).
    fn needs_selection(mut plan: ProjectPlan, flag: &str) -> ProjectPlan {
        plan.route.fallback = Fallback::None;
        plan.needs_host = Some(format!(
            "{flag} uses the selection of a running Qrec, and none runs for this project"
        ));
        plan
    }

    fn scene(&self, scene: &SceneCmd) -> ProjectPlan {
        use View::Command as Msg;
        match scene {
            SceneCmd::Layer(LayerCmd::Add(add)) => {
                let (source, layout) = match add {
                    LayerAddCmd::Screen(a) => (
                        VideoSourceArg::Screen {
                            output: a.output.clone(),
                        },
                        &a.layout,
                    ),
                    LayerAddCmd::Camera(a) => (
                        VideoSourceArg::Camera {
                            device: a.device.clone(),
                        },
                        &a.layout,
                    ),
                };
                self.command(
                    "scene layer add",
                    Command::LayerAdd {
                        target: SceneTarget::Project,
                        source,
                        name: layout.name.clone(),
                        rect: layout.rect,
                    },
                    Msg,
                )
            }
            SceneCmd::Layer(LayerCmd::Set(args)) => {
                let p = &args.patch;
                self.command(
                    "scene layer set",
                    Command::LayerSet {
                        target: target(&args.scene),
                        layer: args.layer.clone(),
                        patch: LayerPatch {
                            name: p.name.clone(),
                            visible: p.visible,
                            opacity: p.opacity,
                            rect: p.rect,
                            crop: p.crop,
                            fit: p.fit.map(Into::into),
                            locked: p.locked,
                        },
                    },
                    Msg,
                )
            }
            SceneCmd::Layer(LayerCmd::Order(args)) => self.command(
                "scene layer order",
                Command::LayerOrder {
                    target: target(&args.scene),
                    layer: args.layer.clone(),
                    to: args.to,
                },
                Msg,
            ),
            SceneCmd::Layer(LayerCmd::Remove(args)) => self.command(
                "scene layer remove",
                Command::LayerRemove {
                    target: target(&args.scene),
                    layer: args.layer.clone(),
                },
                Msg,
            ),
            SceneCmd::Audio(AudioCmd::Add(args)) => self.command(
                "scene audio add",
                Command::AudioAdd {
                    target: SceneTarget::Project,
                    role: args.role.into(),
                    source: args.source.clone(),
                    label: args.label.clone(),
                },
                Msg,
            ),
            SceneCmd::Audio(AudioCmd::Set(args)) => {
                let p = &args.patch;
                self.command(
                    "scene audio set",
                    Command::AudioSet {
                        target: target(&args.scene),
                        track: args.track.clone(),
                        patch: AudioPatch {
                            label: p.label.clone(),
                            source: p.source.clone(),
                            gain_db: p.gain_db,
                            muted: p.mute,
                            sync_offset_ms: p.sync_offset_ms,
                        },
                    },
                    Msg,
                )
            }
            SceneCmd::Audio(AudioCmd::Remove(args)) => self.command(
                "scene audio remove",
                Command::AudioRemove {
                    target: target(&args.scene),
                    track: args.track.clone(),
                },
                Msg,
            ),
            SceneCmd::Apply(args) => {
                let from = args
                    .from_take
                    .clone()
                    .map_or(SceneTarget::Project, SceneTarget::Take);
                let to = if args.to_all_takes {
                    SceneDest::AllTakes
                } else if !args.take.is_empty() {
                    SceneDest::Takes(args.take.clone())
                } else {
                    SceneDest::Project
                };
                self.command("scene apply", Command::SceneApply { from, to }, Msg)
            }
        }
    }

    fn record(&self, record: &RecordCmd) -> ProjectPlan {
        let (name, cmd, fallback, wait) = match record {
            RecordCmd::Start(args) => {
                let mode = if args.over_selection {
                    RecordModeArg::OverSelection
                } else if !args.over.is_empty() {
                    RecordModeArg::Over(args.over.clone())
                } else {
                    RecordModeArg::Append
                };
                let cmd = Command::RecordStart {
                    mode,
                    countdown: args.countdown,
                    wait: args.wait.wait,
                };
                ("record start", cmd, Fallback::SpawnHeadless, args.wait.wait)
            }
            RecordCmd::Stop => (
                "record stop",
                Command::RecordStop,
                Fallback::InProcess,
                false,
            ),
            RecordCmd::Toggle(args) => (
                "record toggle",
                Command::RecordToggle { wait: args.wait },
                Fallback::SpawnHeadless,
                args.wait,
            ),
            RecordCmd::Cancel => (
                "record cancel",
                Command::RecordCancel,
                Fallback::InProcess,
                false,
            ),
        };
        let over_selection = matches!(
            &cmd,
            Command::RecordStart {
                mode: RecordModeArg::OverSelection,
                ..
            }
        );
        let mut plan = self.command(name, cmd, View::Record);
        plan.route.deadline = RECORD_DEADLINE;
        plan.route.fallback = fallback;
        plan.route.stop_on_interrupt = wait;
        plan.streams = wait;
        if over_selection {
            plan = Self::needs_selection(plan, "--over-selection");
        }
        plan
    }

    fn take(&self, take: &TakeCmd) -> ProjectPlan {
        match take {
            TakeCmd::List(args) => self.query(
                "take list",
                Query::TakeList {
                    unused_only: args.unused,
                },
                View::TakeList,
            ),
            TakeCmd::Show(args) => self.query(
                "take show",
                Query::TakeShow {
                    take: args.take.clone(),
                },
                View::TakeShow,
            ),
            TakeCmd::DiscardLast => {
                self.command("take discard-last", Command::TakeDiscardLast, View::Command)
            }
            TakeCmd::Purge(args) => self.command(
                "take purge",
                Command::TakePurge {
                    takes: args.takes.clone(),
                },
                View::Command,
            ),
        }
    }

    fn clip(&self, clip: &ClipCmd) -> Result<ProjectPlan, CoreError> {
        use View::Command as Msg;
        Ok(match clip {
            ClipCmd::List => self.query("clip list", Query::ClipList, View::ClipList),
            ClipCmd::Add(args) => {
                let at = if let Some(index) = args.at.at {
                    InsertAt::Index(index)
                } else if let Some(after) = &args.at.after {
                    InsertAt::After(after.clone())
                } else if args.at.after_selection {
                    InsertAt::AfterSelection
                } else {
                    InsertAt::End
                };
                let after_selection = at == InsertAt::AfterSelection;
                let plan = self.command(
                    "clip add",
                    Command::ClipAdd {
                        take: args.take.clone(),
                        at,
                    },
                    Msg,
                );
                if after_selection {
                    Self::needs_selection(plan, "--after-selection")
                } else {
                    plan
                }
            }
            ClipCmd::Move(args) => {
                let to = &args.to;
                let to = if let Some(index) = to.to {
                    MoveTo::Index(index)
                } else if let Some(by) = to.by {
                    MoveTo::By(by)
                } else if let Some(before) = &to.before {
                    MoveTo::Before(before.clone())
                } else if let Some(after) = &to.after {
                    MoveTo::After(after.clone())
                } else {
                    return Err(CoreError::usage(
                        "`qrec clip move` needs one of --to, --by, --before or --after",
                    ));
                };
                self.command(
                    "clip move",
                    Command::ClipMove {
                        clips: args.clips.clone(),
                        to,
                    },
                    Msg,
                )
            }
            ClipCmd::Trim(args) => self.command(
                "clip trim",
                Command::ClipTrim {
                    clip: args.clip.clone(),
                    start: args.edges.start.as_deref().map(time).transpose()?,
                    end: args.edges.end.as_deref().map(time).transpose()?,
                    timeline: args.timeline,
                },
                Msg,
            ),
            ClipCmd::Split(args) => self.command(
                "clip split",
                Command::ClipSplit {
                    clip: args.clip.clone(),
                    at: time(&args.at)?,
                    timeline: args.timeline,
                },
                Msg,
            ),
            ClipCmd::Remove(args) => self.command(
                "clip remove",
                Command::ClipRemove {
                    clips: args.clips.clone(),
                },
                Msg,
            ),
        })
    }

    fn autocut(&self, args: &AutocutArgs) -> ProjectPlan {
        if let Some(AutocutCmd::Settings(s)) = &args.command {
            return self.command(
                "autocut settings",
                Command::AutocutSettings {
                    patch: AutocutPatch {
                        auto_run_after_take: s.auto_run,
                        track: auto_or(s.track.clone()),
                        threshold_db: s.threshold_db,
                        min_silence_ms: s.min_silence_ms,
                        padding_ms: s.padding_ms,
                        min_keep_ms: s.min_keep_ms,
                    },
                },
                View::Command,
            );
        }
        let scope = if args.scope.clips.is_empty() {
            ClipScope::All
        } else {
            ClipScope::Clips(args.scope.clips.clone())
        };
        let p = &args.params;
        self.command(
            "autocut",
            Command::Autocut {
                scope,
                overrides: AutocutOverrides {
                    track: p.track.clone(),
                    threshold_db: p.threshold_db,
                    min_silence_ms: p.min_silence_ms,
                    padding_ms: p.padding_ms,
                    min_keep_ms: p.min_keep_ms,
                },
                dry_run: args.dry_run,
            },
            View::Autocut {
                dry_run: args.dry_run,
            },
        )
    }

    fn history(&self, args: &HistoryArgs) -> ProjectPlan {
        let mut plan = match &args.command {
            None => self.query(
                "history",
                Query::History { limit: args.limit },
                View::History,
            ),
            Some(HistoryCmd::Restore(restore)) => self.command(
                "history restore",
                Command::HistoryRestore { seq: restore.seq },
                View::Command,
            ),
            Some(HistoryCmd::Clear) => {
                return self.command("history clear", Command::HistoryClear, View::Command);
            }
        };
        // Both work while qrec.json is corrupt: they read the history (A§7.7).
        plan.route.recovery = true;
        plan
    }

    fn export(&self, args: &ExportArgs, cwd: &Path) -> Result<ProjectPlan, CoreError> {
        if let Some(ExportCmd::Cancel) = &args.command {
            return Ok(self.command(
                "export cancel",
                Command::ExportCancel { job: None },
                View::Command,
            ));
        }
        let plan = self.command(
            "export",
            Command::Export {
                format: match args.format {
                    FormatArg::Mp4 => ExportFormat::Mp4,
                    FormatArg::Webm => ExportFormat::Webm,
                },
                quality: match args.quality {
                    QualityArg::Draft => Quality::Draft,
                    QualityArg::Standard => Quality::Standard,
                    QualityArg::High => Quality::High,
                },
                codec: args.codec.map(|codec| match codec {
                    CodecArg::H264 => VideoCodec::H264,
                    CodecArg::Vp9 => VideoCodec::Vp9,
                    CodecArg::Av1 => VideoCodec::Av1,
                }),
                out: args.out.as_deref().map(|p| out_path(cwd, p)).transpose()?,
            },
            View::Export,
        );
        Ok(ProjectPlan {
            streams: true,
            ..plan
        })
    }
}

fn target(args: &SceneTargetArgs) -> SceneTarget {
    args.take
        .clone()
        .map_or(SceneTarget::Project, SceneTarget::Take)
}

/// `auto` clears the field (`null`: let Qrec choose); absent keeps it.
fn auto_or<T>(value: Option<AutoOr<T>>) -> Nullable<T> {
    match value {
        None => Nullable::Keep,
        Some(AutoOr::Auto) => Nullable::Clear,
        Some(AutoOr::Value(v)) => Nullable::Set(v),
    }
}

/// A time argument in the core's grammar (`412f`, `13.7s`, `00:00:13:21`).
fn time(text: &str) -> Result<TimeArg, CoreError> {
    text.parse()
}

fn canvas(text: Option<&str>) -> Result<Option<CanvasSpec>, CoreError> {
    text.map(str::parse).transpose()
}

fn seek(args: &SeekArgs) -> Result<SeekTarget, CoreError> {
    if let Some(to) = &args.to {
        return Ok(match to.trim() {
            "start" => SeekTarget::Start,
            "end" => SeekTarget::End,
            t => SeekTarget::To(time(t)?),
        });
    }
    if let Some(by) = &args.by {
        return Ok(SeekTarget::By(time(by)?));
    }
    match args.edit {
        Some(EditArg::Prev) => Ok(SeekTarget::EditPrev),
        Some(EditArg::Next) => Ok(SeekTarget::EditNext),
        None => Err(CoreError::usage(
            "`qrec seek` needs one of --to, --by or --edit",
        )),
    }
}

/// `path` made absolute against `cwd`, without `.` components. `..` is
/// kept: only the file system knows what it means next to a symlink.
pub fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    let joined = cwd.join(path);
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// An output path for the core: absolute (A§5.9) and UTF-8.
fn out_path(cwd: &Path, path: &Path) -> Result<String, CoreError> {
    let abs = absolute(cwd, path);
    abs.to_str().map(str::to_owned).ok_or_else(|| {
        CoreError::usage(format!(
            "the output path {} is not valid UTF-8",
            abs.display()
        ))
        .with_hint("pick a file name made of UTF-8 text")
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use pretty_assertions::assert_eq;
    use qrec_core::ErrorCode;
    use serde_json::{Value, json};

    use super::*;
    use crate::parse_args;

    fn cli(line: &str) -> Cli {
        let words = line.split_whitespace().map(OsString::from);
        parse_args(words).unwrap_or_else(|e| panic!("`{line}`: {e}"))
    }

    fn project_plan(line: &str) -> ProjectPlan {
        match plan(&cli(line), Path::new("/work/dir")) {
            Ok(Plan::Project(plan)) => plan,
            other => panic!("`{line}` planned {other:?}"),
        }
    }

    /// The request body as it goes on the wire.
    fn body(line: &str) -> Value {
        serde_json::to_value(&project_plan(line).request.body).unwrap()
    }

    fn command(line: &str) -> Value {
        body(line)["command"].clone()
    }

    #[test]
    fn local_commands_need_no_project() {
        let local = |line| match plan(&cli(line), Path::new("/")) {
            Ok(Plan::Local(local)) => local,
            other => panic!("`{line}` planned {other:?}"),
        };
        assert_eq!(local("qrec version"), Local::Version);
        assert_eq!(local("qrec doctor"), Local::Doctor);
        assert_eq!(local("qrec sources list"), Local::Sources);
        assert_eq!(local("qrec schema"), Local::Schema);
        assert_eq!(local("qrec agent-guide"), Local::AgentGuide);
        assert_eq!(local("qrec keys --json"), Local::Keys);
        assert_eq!(local("qrec config"), Local::ConfigList);
        assert_eq!(
            local("qrec config get canvas.fps"),
            Local::ConfigGet("canvas.fps".into())
        );
        assert_eq!(
            local("qrec config set autocut.threshold_db -40"),
            Local::ConfigSet {
                key: "autocut.threshold_db".into(),
                value: "-40".into()
            }
        );
        assert!(matches!(local("qrec host --headless"), Local::Host(_)));
        assert_eq!(plan(&cli("qrec"), Path::new("/")), Ok(Plan::Gui));
    }

    #[test]
    fn init_and_status() {
        assert_eq!(
            command("qrec init --canvas 1280x720@25 --name Swamp"),
            json!({"cmd": "init", "canvas": "1280x720@25", "name": "Swamp"})
        );
        let init = project_plan("qrec init");
        assert_eq!(init.on_empty, OnEmpty::Send);
        assert!(init.route.lock);

        let status = project_plan("qrec status");
        assert_eq!(body("qrec status"), json!({"query": {"query": "status"}}));
        assert_eq!(
            (status.view, status.on_empty),
            (View::Status, OnEmpty::Answer)
        );
        assert!(!status.route.lock, "queries take no lock");
        assert_eq!(
            body("qrec status --json"),
            json!({"query": {"query": "state"}})
        );
        assert_eq!(project_plan("qrec --json status").view, View::StatusJson);
    }

    #[test]
    fn bad_times_and_canvases_are_core_usage_errors() {
        for line in [
            "qrec clip split c1 --at 13",
            "qrec clip trim c1 --start 1.5",
            "qrec frame save --at soon",
            "qrec seek --by 3",
            "qrec init --canvas 1920x1080@abc",
            "qrec project set --canvas big",
        ] {
            let err = plan(&cli(line), Path::new("/")).unwrap_err();
            assert_eq!(err.code, ErrorCode::Usage, "{line}");
            assert!(err.hint.is_some(), "{line}: the core's hint is kept");
        }
    }

    #[test]
    fn scene_commands() {
        assert_eq!(
            command("qrec scene layer add screen --output DP-4 --rect 0,0,960,540 --name Main"),
            json!({"cmd": "layerAdd", "target": "project", "source": {"screen": {"output": "DP-4"}},
                   "name": "Main", "rect": {"x": 0, "y": 0, "w": 960, "h": 540}})
        );
        assert_eq!(
            command("qrec scene layer add camera --device /dev/video0"),
            json!({"cmd": "layerAdd", "target": "project", "source": {"camera": {"device": "/dev/video0"}},
                   "name": null, "rect": null})
        );
        assert_eq!(
            command(
                "qrec scene layer set L2 --take take-0003 --opacity 0.5 --visible false --fit cover --locked true"
            ),
            json!({"cmd": "layerSet", "target": "take-0003", "layer": "L2",
                   "patch": {"opacity": 0.5, "visible": false, "fit": "cover", "locked": true}})
        );
        assert_eq!(
            command("qrec scene layer order L2 --to 0"),
            json!({"cmd": "layerOrder", "target": "project", "layer": "L2", "to": 0})
        );
        assert_eq!(
            command("qrec scene layer remove L2 --take take-0001"),
            json!({"cmd": "layerRemove", "target": "take-0001", "layer": "L2"})
        );
        assert_eq!(
            command("qrec scene audio add system --source sink.monitor --label Speakers"),
            json!({"cmd": "audioAdd", "target": "project", "role": "system",
                   "source": "sink.monitor", "label": "Speakers"})
        );
        assert_eq!(
            command("qrec scene audio set A1 --gain-db -3 --mute true --sync-offset-ms -80"),
            json!({"cmd": "audioSet", "target": "project", "track": "A1",
                   "patch": {"gainDb": -3.0, "muted": true, "syncOffsetMs": -80}})
        );
        assert_eq!(
            command("qrec scene audio remove A2"),
            json!({"cmd": "audioRemove", "target": "project", "track": "A2"})
        );
        let apply = |line| command(line);
        assert_eq!(
            apply("qrec scene apply --take take-0003"),
            json!({"cmd": "sceneApply", "from": "project", "to": {"takes": ["take-0003"]}})
        );
        assert_eq!(
            apply("qrec scene apply --to-all-takes"),
            json!({"cmd": "sceneApply", "from": "project", "to": "allTakes"})
        );
        assert_eq!(
            apply("qrec scene apply --from-take take-0002"),
            json!({"cmd": "sceneApply", "from": "take-0002", "to": "project"})
        );
        assert_eq!(
            apply("qrec scene apply --from-take take-0002 --to-all-takes"),
            json!({"cmd": "sceneApply", "from": "take-0002", "to": "allTakes"})
        );
    }

    #[test]
    fn record_commands_route_to_a_host() {
        let start = project_plan("qrec record start --countdown 3");
        assert_eq!(
            serde_json::to_value(&start.request.body).unwrap()["command"],
            json!({"cmd": "recordStart", "mode": "append", "countdown": 3, "wait": false})
        );
        assert_eq!(start.route.fallback, Fallback::SpawnHeadless);
        assert_eq!(start.route.deadline, RECORD_DEADLINE);
        assert!(!start.route.stop_on_interrupt);
        assert_eq!(start.view, View::Record);

        let over = project_plan("qrec record start --over c1 c2 --wait");
        assert_eq!(
            serde_json::to_value(&over.request.body).unwrap()["command"]["mode"],
            json!({"over": ["c1", "c2"]})
        );
        assert!(over.route.stop_on_interrupt);

        let selection = project_plan("qrec record start --over-selection");
        assert_eq!(selection.route.fallback, Fallback::None);
        assert!(selection.needs_host.unwrap().contains("--over-selection"));
        assert_eq!(
            command("qrec record start --over-selection")["mode"],
            json!("overSelection")
        );

        let toggle = project_plan("qrec record toggle --wait");
        assert_eq!(toggle.route.fallback, Fallback::SpawnHeadless);
        assert!(toggle.route.stop_on_interrupt);
        assert_eq!(
            command("qrec record toggle"),
            json!({"cmd": "recordToggle", "wait": false})
        );

        for (line, cmd) in [
            ("qrec record stop", "recordStop"),
            ("qrec record cancel", "recordCancel"),
        ] {
            let plan = project_plan(line);
            assert_eq!(command(line), json!({"cmd": cmd}));
            assert_eq!(plan.route.fallback, Fallback::InProcess, "{line}");
            assert_eq!(plan.route.deadline, RECORD_DEADLINE, "{line}");
        }
    }

    #[test]
    fn take_commands() {
        assert_eq!(
            body("qrec take list --unused"),
            json!({"query": {"query": "takeList", "unusedOnly": true}})
        );
        assert_eq!(
            body("qrec take show take-0002"),
            json!({"query": {"query": "takeShow", "take": "take-0002"}})
        );
        assert_eq!(
            command("qrec take discard-last"),
            json!({"cmd": "takeDiscardLast"})
        );
        let purge = project_plan("qrec take purge take-0003 take-0004 --yes");
        assert!(purge.request.confirmed);
        assert_eq!(
            command("qrec take purge take-0003,take-0004"),
            json!({"cmd": "takePurge", "takes": ["take-0003", "take-0004"]})
        );
        assert!(!project_plan("qrec take purge take-0003").request.confirmed);
    }

    #[test]
    fn clip_commands() {
        assert_eq!(
            body("qrec clip list"),
            json!({"query": {"query": "clipList"}})
        );
        assert_eq!(
            command("qrec clip add take-0002"),
            json!({"cmd": "clipAdd", "take": "take-0002", "at": "end"})
        );
        assert_eq!(
            command("qrec clip add take-0002 --at 0")["at"],
            json!({"index": 0})
        );
        assert_eq!(
            command("qrec clip add take-0002 --after c1")["at"],
            json!({"after": "c1"})
        );
        let after_selection = project_plan("qrec clip add take-0002 --after-selection");
        assert_eq!(after_selection.route.fallback, Fallback::None);
        assert_eq!(
            command("qrec clip add take-0002 --after-selection")["at"],
            json!("afterSelection")
        );
        assert_eq!(
            command("qrec clip move c1 c2 --by -1"),
            json!({"cmd": "clipMove", "clips": ["c1", "c2"], "to": {"by": -1}})
        );
        assert_eq!(
            command("qrec clip move c1 --to 0")["to"],
            json!({"index": 0})
        );
        assert_eq!(
            command("qrec clip move c1 --before c9")["to"],
            json!({"before": "c9"})
        );
        assert_eq!(
            command("qrec clip move c1 --after c9")["to"],
            json!({"after": "c9"})
        );
        assert_eq!(
            command("qrec clip trim c1 --start +2f --end 00:00:01:00"),
            json!({"cmd": "clipTrim", "clip": "c1", "start": "+2f", "end": "00:00:01:00", "timeline": false})
        );
        assert_eq!(
            command("qrec clip trim c1 --end -2s --timeline"),
            json!({"cmd": "clipTrim", "clip": "c1", "start": null, "end": "-2s", "timeline": true})
        );
        assert_eq!(
            command("qrec clip split c1 --at 13f"),
            json!({"cmd": "clipSplit", "clip": "c1", "at": "13f", "timeline": false})
        );
        assert_eq!(
            command("qrec clip split --timeline --at 1.5s"),
            json!({"cmd": "clipSplit", "clip": null, "at": "1.5s", "timeline": true})
        );
        assert_eq!(
            command("qrec clip remove c1 c2"),
            json!({"cmd": "clipRemove", "clips": ["c1", "c2"]})
        );
    }

    #[test]
    fn autocut_commands() {
        assert_eq!(
            command("qrec autocut"),
            json!({"cmd": "autocut", "scope": "all", "overrides": {}, "dryRun": false})
        );
        let dry = project_plan(
            "qrec autocut --clips c1 c2 --track A2 --threshold-db -40 --min-silence-ms 300 --padding-ms 100 --min-keep-ms 150 --dry-run",
        );
        assert_eq!(dry.view, View::Autocut { dry_run: true });
        assert_eq!(
            serde_json::to_value(&dry.request.body).unwrap()["command"],
            json!({"cmd": "autocut", "scope": {"clips": ["c1", "c2"]},
                   "overrides": {"track": "A2", "thresholdDb": -40.0, "minSilenceMs": 300, "paddingMs": 100, "minKeepMs": 150},
                   "dryRun": true})
        );
        assert_eq!(command("qrec autocut --all")["scope"], json!("all"));
        assert_eq!(
            command("qrec autocut settings --auto-run true --track auto --threshold-db -30"),
            json!({"cmd": "autocutSettings", "patch": {"autoRunAfterTake": true, "track": null, "thresholdDb": -30.0}})
        );
        assert_eq!(
            command("qrec autocut settings --track A1")["patch"],
            json!({"track": "A1"})
        );
    }

    #[test]
    fn history_commands() {
        assert_eq!(command("qrec undo"), json!({"cmd": "undo", "steps": 1}));
        assert_eq!(command("qrec redo 3"), json!({"cmd": "redo", "steps": 3}));
        let history = project_plan("qrec history --limit 5");
        assert_eq!(
            serde_json::to_value(&history.request.body).unwrap(),
            json!({"query": {"query": "history", "limit": 5}})
        );
        assert!(history.route.recovery && !history.route.lock);
        let restore = project_plan("qrec history restore 7 --yes");
        assert!(restore.route.recovery && restore.route.lock && restore.request.confirmed);
        assert_eq!(
            command("qrec history restore 7"),
            json!({"cmd": "historyRestore", "seq": 7})
        );
        let clear = project_plan("qrec history clear");
        assert!(!clear.route.recovery);
        assert_eq!(
            command("qrec history clear"),
            json!({"cmd": "historyClear"})
        );
    }

    #[test]
    fn output_commands_get_absolute_paths() {
        assert_eq!(
            command("qrec export"),
            json!({"cmd": "export", "format": "mp4", "quality": "standard", "codec": null, "out": null})
        );
        assert_eq!(
            command("qrec export --format webm --quality high --codec av1 --out ../out/./v.webm"),
            json!({"cmd": "export", "format": "webm", "quality": "high", "codec": "av1",
                   "out": "/work/dir/../out/v.webm"})
        );
        assert_eq!(
            command("qrec export --out /tmp/x.mp4")["out"],
            json!("/tmp/x.mp4")
        );
        assert_eq!(project_plan("qrec export").view, View::Export);
        assert_eq!(
            command("qrec export cancel"),
            json!({"cmd": "exportCancel", "job": null})
        );
        assert_eq!(
            command("qrec frame save --at 00:00:01:02 --layer L2 --out still.png"),
            json!({"cmd": "frameSave", "at": "00:00:01:02", "layer": "L2", "out": "/work/dir/still.png"})
        );
        assert_eq!(
            project_plan("qrec frame save --at 1f").view,
            View::FrameSave
        );
    }

    #[test]
    fn gui_commands_need_a_live_host() {
        for (line, expected) in [
            (
                "qrec seek --to 13f",
                json!({"cmd": "seek", "to": {"to": "13f"}}),
            ),
            (
                "qrec seek --to start",
                json!({"cmd": "seek", "to": "start"}),
            ),
            ("qrec seek --to end", json!({"cmd": "seek", "to": "end"})),
            (
                "qrec seek --by -1s",
                json!({"cmd": "seek", "to": {"by": "-1s"}}),
            ),
            (
                "qrec seek --edit prev",
                json!({"cmd": "seek", "to": "editPrev"}),
            ),
            (
                "qrec seek --edit next",
                json!({"cmd": "seek", "to": "editNext"}),
            ),
            ("qrec play", json!({"cmd": "play"})),
            ("qrec pause", json!({"cmd": "pause"})),
        ] {
            let plan = project_plan(line);
            assert_eq!(command(line), expected, "{line}");
            assert_eq!(plan.route.fallback, Fallback::None, "{line}");
            assert!(
                plan.needs_host.unwrap().contains("needs the Qrec GUI"),
                "{line}"
            );
        }
    }

    #[test]
    fn project_set() {
        assert_eq!(
            command(
                "qrec project set --name Ogre --canvas @29.97 --background #12170F --countdown 3 --encoder x264 --vaapi-device auto"
            ),
            json!({"cmd": "projectSet", "name": "Ogre", "canvas": "@30000/1001", "background": "#12170F",
                   "countdownSecs": 3, "encoder": "x264", "vaapiDevice": null})
        );
        assert_eq!(
            command("qrec project set --vaapi-device /dev/dri/renderD129")["vaapiDevice"],
            json!("/dev/dri/renderD129")
        );
        let keep = command("qrec project set --name x");
        assert!(keep.get("vaapiDevice").is_none(), "absent = keep: {keep}");
    }

    #[test]
    fn every_command_round_trips_through_the_core() {
        // What the CLI sends must deserialize as the core's protocol.
        for line in [
            "qrec init",
            "qrec scene layer set L1 --crop 0,0,40,0 --rect -10,0,100,100",
            "qrec record start --over c1",
            "qrec clip trim c1 --start 13f",
            "qrec autocut --dry-run",
            "qrec export --format webm",
            "qrec seek --to 1s",
            "qrec project set --vaapi-device auto",
            "qrec autocut settings --track auto",
        ] {
            let plan = project_plan(line);
            let text = serde_json::to_string(&plan.request.body).unwrap();
            let back: WireBody = serde_json::from_str(&text).unwrap();
            assert_eq!(back, plan.request.body, "{line}");
        }
    }

    #[test]
    fn absolute_paths() {
        let cwd = Path::new("/a/b");
        assert_eq!(absolute(cwd, Path::new("c/./d")), Path::new("/a/b/c/d"));
        assert_eq!(absolute(cwd, Path::new("../c")), Path::new("/a/b/../c"));
        assert_eq!(absolute(cwd, Path::new("/x/./y")), Path::new("/x/y"));
        assert_eq!(absolute(cwd, Path::new(".")), Path::new("/a/b"));
    }
}
