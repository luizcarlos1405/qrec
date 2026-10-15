//! `apply`: commands in, the next state and effects out (A§4.1, foundation).
//!
//! `apply` first enforces the gates every command shares, then dispatches
//! to one handler per command in its owner's file:
//!
//! | file | owner | commands |
//! | --- | --- | --- |
//! | `clip.rs` | timeline | `ClipAdd/Move/Trim/Split/Remove` |
//! | `ui.rs` | timeline | `Select`, `Seek`, `Play`, `Pause`, `Shuttle`, `SetCanvasMode`, `SetUi` |
//! | `history.rs` | history | `Undo`, `Redo`, `HistoryRestore`, `HistoryClear`, `TakePurge` |
//! | `project.rs` | scene | `Init`, `ProjectSet` |
//! | `scene.rs` | scene | `Layer*`, `Audio*`, `SceneApply` |
//! | `record.rs` | record | `Record*`, `TakeDiscardLast` |
//! | `autocut.rs` | autocut | `Autocut` (`AutocutSettings` lives in `project.rs`) |
//! | `output.rs` | output | `Export`, `ExportCancel`, `FrameSave` |
//!
//! Handlers return `Err` for anything that changes nothing (bad id, busy,
//! needs confirmation, usage) and never a partial state. They finish
//! through `history::commit` / `history::persist`.

pub mod autocut;
pub mod clip;
pub mod history;
pub mod output;
pub mod project;
pub mod record;
pub mod scene;
pub mod ui;

use serde::{Deserialize, Serialize};

use crate::ctx::{Ctx, FactsNeeded};
use crate::error::CoreError;
use crate::history::Committed;
use crate::model::{Project, StreamKind, VideoSource};
use crate::protocol::command::{ClipScope, Command};
use crate::protocol::effect::Effect;
use crate::protocol::reply::CmdResult;
use crate::state::{HostKind, RecordPhase, State};

/// What `apply` and `on_event` return.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Outcome {
    pub state: State,
    pub effects: Vec<Effect>,
    /// `Some` = the request completes now; `None` = an `Effect::Reply` comes later.
    pub reply: Option<Result<CmdResult, CoreError>>,
}

impl Outcome {
    pub fn new(
        state: State,
        effects: Vec<Effect>,
        reply: Option<Result<CmdResult, CoreError>>,
    ) -> Self {
        Self {
            state,
            effects,
            reply,
        }
    }

    /// Nothing changes and nobody gets a reply (ignored events).
    pub fn unchanged(state: &State) -> Self {
        Self {
            state: state.clone(),
            effects: Vec::new(),
            reply: None,
        }
    }

    /// A committed change that completes now: the reply's `seq` is the commit's.
    pub fn done(committed: Committed, result: CmdResult) -> Self {
        let seq = committed.seq.or(result.seq);
        Self {
            state: committed.state,
            effects: committed.effects,
            reply: Some(Ok(result.with_seq(seq))),
        }
    }

    /// A committed change whose reply comes later (async commands).
    pub fn pending(committed: Committed) -> Self {
        Self {
            state: committed.state,
            effects: committed.effects,
            reply: None,
        }
    }
}

/// Runs one command against `state` (pure).
pub fn apply(state: &State, ctx: &Ctx, cmd: Command) -> Result<Outcome, CoreError> {
    check_initialized(state, &cmd)?;
    check_recording(state, &cmd)?;
    check_host(state, &cmd)?;
    check_disk(state, &cmd)?;
    match cmd {
        Command::Init { canvas, name } => project::init(state, ctx, canvas, name),
        Command::ProjectSet {
            name,
            canvas,
            background,
            countdown_secs,
            encoder,
            vaapi_device,
        } => project::project_set(
            state,
            ctx,
            name,
            canvas,
            background,
            countdown_secs,
            encoder,
            vaapi_device,
        ),
        Command::LayerAdd {
            target,
            source,
            name,
            rect,
        } => scene::layer_add(state, ctx, target, source, name, rect),
        Command::LayerSet {
            target,
            layer,
            patch,
        } => scene::layer_set(state, ctx, target, layer, patch),
        Command::LayerOrder { target, layer, to } => {
            scene::layer_order(state, ctx, target, layer, to)
        }
        Command::LayerRemove { target, layer } => scene::layer_remove(state, ctx, target, layer),
        Command::AudioAdd {
            target,
            role,
            source,
            label,
        } => scene::audio_add(state, ctx, target, role, source, label),
        Command::AudioSet {
            target,
            track,
            patch,
        } => scene::audio_set(state, ctx, target, track, patch),
        Command::AudioRemove { target, track } => scene::audio_remove(state, ctx, target, track),
        Command::SceneApply { from, to } => scene::scene_apply(state, ctx, from, to),
        Command::AutocutSettings { patch } => project::autocut_settings(state, ctx, patch),
        Command::RecordStart {
            mode,
            countdown,
            wait,
        } => record::record_start(state, ctx, mode, countdown, wait),
        Command::RecordStop => record::record_stop(state, ctx),
        Command::RecordCancel => record::record_cancel(state, ctx),
        Command::RecordToggle { wait } => record::record_toggle(state, ctx, wait),
        Command::TakeDiscardLast => record::take_discard_last(state, ctx),
        Command::TakePurge { takes } => history::take_purge(state, ctx, takes),
        Command::ClipAdd { take, at } => clip::clip_add(state, ctx, take, at),
        Command::ClipMove { clips, to } => clip::clip_move(state, ctx, clips, to),
        Command::ClipTrim {
            clip,
            start,
            end,
            timeline,
        } => clip::clip_trim(state, ctx, clip, start, end, timeline),
        Command::ClipSplit { clip, at, timeline } => {
            clip::clip_split(state, ctx, clip, at, timeline)
        }
        Command::ClipRemove { clips } => clip::clip_remove(state, ctx, clips),
        Command::Autocut {
            scope,
            overrides,
            dry_run,
        } => autocut::autocut(state, ctx, scope, overrides, dry_run),
        Command::Undo { steps } => history::undo(state, ctx, steps),
        Command::Redo { steps } => history::redo(state, ctx, steps),
        Command::HistoryRestore { seq } => history::history_restore(state, ctx, seq),
        Command::HistoryClear => history::history_clear(state, ctx),
        Command::Export {
            format,
            quality,
            codec,
            out,
        } => output::export(state, ctx, format, quality, codec, out),
        Command::ExportCancel { job } => output::export_cancel(state, ctx, job),
        Command::FrameSave { at, layer, out } => output::frame_save(state, ctx, at, layer, out),
        Command::Select { op } => ui::select(state, ctx, op),
        Command::Seek { to } => ui::seek(state, ctx, to),
        Command::Play => ui::play(state, ctx),
        Command::Pause => ui::pause(state, ctx),
        Command::Shuttle { dir } => ui::shuttle(state, ctx, dir),
        Command::SetCanvasMode { mode } => ui::set_canvas_mode(state, ctx, mode),
        Command::SetUi { patch } => ui::set_ui(state, ctx, patch),
    }
}

/// A folder without `qrec.json` holds a placeholder project (version 0): only
/// `Init` runs there. The shell initializes lazily before mutating commands
/// (A§8.4), so this mostly guards IPC clients and tests.
fn check_initialized(state: &State, cmd: &Command) -> Result<(), CoreError> {
    if project::is_initialized(&state.project) || matches!(cmd, Command::Init { .. }) {
        return Ok(());
    }
    Err(no_project_yet())
}

/// What a command that doesn't create a project
/// ([`Command::creates_project`]) gets in an empty folder (SPEC §4.4).
pub fn no_project_yet() -> CoreError {
    CoreError::new(
        crate::ErrorCode::NotProject,
        "this folder has no Qrec project yet",
    )
    .with_hint("run `qrec init`, set up a scene or start recording to create one here")
}

/// A§5.3: while not `Idle`, only the commands marked R run; the rest are `Busy`.
fn check_recording(state: &State, cmd: &Command) -> Result<(), CoreError> {
    let phase = &state.runtime.record;
    if phase.is_idle() || cmd.allowed_while_recording(phase) {
        return Ok(());
    }
    let what = match phase {
        RecordPhase::Idle => return Ok(()),
        RecordPhase::Countdown { .. } => "a recording countdown is running".to_owned(),
        RecordPhase::Starting { take, .. } | RecordPhase::Recording { take, .. } => {
            format!("recording take {}", take.number())
        }
        RecordPhase::Finalizing { take, .. } => format!("finalizing take {}", take.number()),
    };
    Err(CoreError::busy(what)
        .with_hint("stop the recording first (qrec record stop) or cancel it (qrec record cancel)"))
}

/// A§8.5: commands marked G need the GUI host.
fn check_host(state: &State, cmd: &Command) -> Result<(), CoreError> {
    if cmd.gui_only() && state.runtime.host != HostKind::Gui {
        return Err(
            CoreError::unavailable(format!("{} needs the Qrec window", cmd.name()))
                .with_hint("open the project in the GUI (run qrec in its folder) and try again"),
        );
    }
    Ok(())
}

/// A§7.7: user edits are refused while `qrec.json` on disk is invalid.
fn check_disk(state: &State, cmd: &Command) -> Result<(), CoreError> {
    match &state.runtime.disk_invalid {
        Some(msg) if cmd.edits_project() => Err(CoreError::invalid(format!(
            "qrec.json on disk is invalid: {msg}; fix it or run `qrec history restore {} --yes`",
            state.history.cursor
        ))),
        _ => Ok(()),
    }
}

/// The facts `cmd` needs (A§4.2). The shell gathers exactly these before
/// calling `apply`; `Facts::config` is always filled.
pub fn facts_needed(state: &State, cmd: &Command) -> FactsNeeded {
    let mut need = FactsNeeded::default();
    match cmd {
        Command::Init { .. } => need.folder_name = true,
        Command::LayerAdd { .. } => need.video_sources = true,
        Command::AudioAdd { .. } => need.audio_devices = true,
        Command::AudioSet { patch, .. } => need.audio_devices = patch.source.is_some(),
        // Without a countdown the take begins right away; otherwise the
        // last countdown tick asks again (`facts_needed_for_event`).
        Command::RecordStart { countdown, .. }
            if state.runtime.record.is_idle()
                && countdown.unwrap_or(state.project.recording.countdown_secs) == 0 =>
        {
            begin_take_facts(&state.project, &mut need);
        }
        Command::RecordToggle { .. }
            if state.runtime.record.is_idle() && state.project.recording.countdown_secs == 0 =>
        {
            begin_take_facts(&state.project, &mut need);
        }
        Command::TakePurge { takes } => need.take_files = takes.clone(),
        Command::Export { out: Some(out), .. } | Command::FrameSave { out: Some(out), .. } => {
            need.out = Some(out.clone());
        }
        Command::Autocut { scope, .. } => need.files = audio_files_in_scope(&state.project, scope),
        _ => {}
    }
    need
}

/// Beginning a take allocates its number (`takes/` scan) and, with camera
/// layers, picks camera modes for the capture plan.
pub(crate) fn begin_take_facts(project: &Project, need: &mut FactsNeeded) {
    need.take_dirs = true;
    need.video_sources = project
        .scene
        .layers
        .iter()
        .any(|l| matches!(l.source, VideoSource::Camera { .. }));
}

/// The audio files of the takes used by the clips in `scope` (for silence keys).
fn audio_files_in_scope(project: &Project, scope: &ClipScope) -> Vec<String> {
    let clips: Vec<_> = match scope {
        ClipScope::All => project.timeline.clips.iter().collect(),
        ClipScope::Clips(ids) => project
            .timeline
            .clips
            .iter()
            .filter(|c| ids.contains(&c.id))
            .collect(),
    };
    let mut files: Vec<String> = clips
        .iter()
        .filter_map(|c| project.take(&c.take))
        .flat_map(|t| {
            t.streams
                .iter()
                .filter(|s| s.kind == StreamKind::Audio)
                .map(|s| s.file.clone())
        })
        .collect();
    files.sort();
    files.dedup();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ClipId, TakeId};
    use crate::protocol::command::{AutocutOverrides, MoveTo, SelectOp};
    use crate::state::RecordMode;
    use crate::testkit::{self, ProjectBuilder};

    fn project() -> Project {
        ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .camera_layer("L2", "/dev/video0")
            .mic("A1", "alsa_input.mic")
            .system("A2", "alsa_output.monitor")
            .take("take-0001", 300)
            .take("take-0002", 200)
            .clip("c001", "take-0001", 0, 300)
            .build()
    }

    fn recording(state: &mut State) {
        state.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(3),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
    }

    #[test]
    fn recording_gate() {
        let mut state = testkit::state(project());
        recording(&mut state);
        let ctx = testkit::ctx("2026-10-14T12:00:00Z", 1);
        let cmd = Command::ClipMove {
            clips: vec![ClipId::from_raw("c001")],
            to: MoveTo::By(1),
        };
        let err = apply(&state, &ctx, cmd).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Busy);
        assert_eq!(err.message, "recording take 3");
        let err = apply(&state, &ctx, Command::Play).unwrap_err();
        assert_eq!(
            err.code,
            crate::ErrorCode::Busy,
            "Play isn't allowed while recording"
        );
        let err = apply(&state, &ctx, Command::TakeDiscardLast).unwrap_err();
        assert_eq!(
            err.code,
            crate::ErrorCode::Busy,
            "discard only while finalizing"
        );
        assert!(
            Command::Select {
                op: SelectOp::Clear
            }
            .allowed_while_recording(&state.runtime.record)
        );
    }

    #[test]
    fn host_and_disk_gates() {
        let mut state = testkit::state(project());
        let ctx = testkit::ctx("2026-10-14T12:00:00Z", 1);
        state.runtime.host = HostKind::Headless;
        let err = apply(&state, &ctx, Command::Play).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Unavailable);

        state.runtime.disk_invalid = Some("expected value at line 3".into());
        let cmd = Command::ClipRemove {
            clips: vec![ClipId::from_raw("c001")],
        };
        let err = apply(&state, &ctx, cmd).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::Invalid);
        assert!(
            err.message
                .starts_with("qrec.json on disk is invalid: expected value at line 3;"),
            "{}",
            err.message
        );
        assert!(err.message.contains("qrec history restore 0 --yes"));
    }

    #[test]
    fn facts_for_commands() {
        let state = testkit::state(project());
        assert!(facts_needed(&state, &Command::ClipRemove { clips: vec![] }).is_empty());
        assert!(
            facts_needed(
                &state,
                &Command::Init {
                    canvas: None,
                    name: None
                }
            )
            .folder_name
        );
        let start = Command::RecordStart {
            mode: Default::default(),
            countdown: None,
            wait: false,
        };
        let need = facts_needed(&state, &start);
        assert!(
            need.take_dirs && need.video_sources,
            "camera layer ⇒ camera modes"
        );
        let counted = Command::RecordStart {
            mode: Default::default(),
            countdown: Some(3),
            wait: false,
        };
        assert!(
            facts_needed(&state, &counted).is_empty(),
            "the last countdown tick asks"
        );
        let autocut = Command::Autocut {
            scope: ClipScope::All,
            overrides: AutocutOverrides::default(),
            dry_run: true,
        };
        assert_eq!(
            facts_needed(&state, &autocut).files,
            vec![
                "takes/take-0001/A1-mic.mka".to_owned(),
                "takes/take-0001/A2-system.mka".to_owned()
            ]
        );
        let export = Command::Export {
            format: Default::default(),
            quality: Default::default(),
            codec: None,
            out: Some("/tmp/x.mp4".into()),
        };
        assert_eq!(
            facts_needed(&state, &export).out.as_deref(),
            Some("/tmp/x.mp4")
        );
    }
}
