//! Project creation and settings (A§5.4, A§2.1): `Init`, `ProjectSet` and
//! the project's autocut settings (`AutocutSettings`). Owner: scene.
//!
//! **A folder without `qrec.json`** is the state [`uninitialized_state`]
//! returns: a placeholder project with `version: 0` that only `Init`
//! replaces ([`is_initialized`] tells them apart). The shell builds it for
//! an empty folder and dispatches `Command::Init`, explicitly (`qrec init`)
//! or lazily before the first mutating command (SPEC §4.4). Every other
//! command of this module refuses a placeholder with `not_project`.

use chrono::DateTime;
use serde_json::{Value, json};

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::error::{CoreError, ErrorCode};
use crate::history::{commit, persist};
use crate::model::{AutocutSettings, Canvas, Color, EncoderPref, Fps, Project, ProjectDefaults};
use crate::protocol::command::{AutocutPatch, CanvasSpec, Command, Nullable};
use crate::protocol::reply::CmdResult;
use crate::requantize::{self, Requantized};
use crate::state::{History, HostKind, State, UiState};

/// Canvas sides: even, 16 to 8192 px (A§2.3).
const MIN_CANVAS: u32 = 16;
const MAX_CANVAS: u32 = 8192;
const MAX_FPS: f64 = 240.0;
/// `recording.countdownSecs` (SPEC §5.5).
const MAX_COUNTDOWN_SECS: u32 = 10;
/// Autocut ranges (A§2.3).
const MIN_THRESHOLD_DB: f64 = -90.0;
const MAX_AUTOCUT_MS: u32 = 60_000;

const CANVAS_HINT: &str = "use WIDTHxHEIGHT@FPS, e.g. 1920x1080@30, 1280x720 or @29.97";

// ------------------------------------------------------------ uninitialized

/// The state of a folder that has no `qrec.json` yet: a placeholder
/// project (`version: 0`, never valid, never written) for `Init` to replace.
pub fn uninitialized_state(host: HostKind) -> State {
    let mut project = Project::new("", DateTime::UNIX_EPOCH, &ProjectDefaults::default());
    project.version = 0;
    State::new(project, History::new(), UiState::default(), host)
}

/// False for the placeholder of [`uninitialized_state`] (`StatusView.initialized`).
pub fn is_initialized(project: &Project) -> bool {
    project.version != 0
}

/// `not_project` unless a project exists (every command but `Init`).
pub(crate) fn require_project(state: &State) -> Result<(), CoreError> {
    if is_initialized(&state.project) {
        Ok(())
    } else {
        Err(
            CoreError::new(ErrorCode::NotProject, "there is no project here yet")
                .with_hint("run `qrec init` first"),
        )
    }
}

// --------------------------------------------------------------------- init

/// `qrec init`: `Invalid` "already a project" if one exists. Uses
/// `facts.config` (`UserConfig::project_defaults`, which never copies a
/// VA-API device) and `facts.folder_name`; `canvas` and `name` override
/// them. Not undoable: the new project starts with an empty history.
pub fn init(
    state: &State,
    ctx: &Ctx,
    canvas: Option<CanvasSpec>,
    name: Option<String>,
) -> Result<Outcome, CoreError> {
    if is_initialized(&state.project) {
        return Err(CoreError::invalid("already a project").with_hint(format!(
            "this folder already has a qrec.json (project {:?}); change its settings with `qrec project set`",
            state.project.name
        )));
    }
    let name = match name {
        Some(name) => clean_text("project name", &name)?,
        None => folder_project_name(ctx.facts.folder_name.as_deref()),
    };
    let mut defaults = ctx.facts.config.project_defaults();
    if let Some(spec) = canvas {
        defaults.canvas = with_canvas_spec(&defaults.canvas, &spec)?;
    }
    // Arguments were checked above, so what still fails came from the config.
    if let Some(problem) = defaults_problem(&defaults) {
        return Err(
            CoreError::invalid(format!("the user config is invalid: {problem}")).with_hint(
                "fix ~/.config/qrec/config.toml (`qrec config set <key> <value>`), \
                 or pass the canvas with --canvas",
            ),
        );
    }

    let mut next = state.clone();
    next.project = Project::new(name, ctx.now, &defaults);
    next.history = History::new();
    next.ui = UiState::default();
    let project = &next.project;
    let result = CmdResult::new(format!(
        "Created project {:?} ({})",
        project.name,
        canvas_label(&project.canvas)
    ))
    .with_result(project);
    Ok(Outcome::done(persist(state, next), result))
}

/// The folder's name without control characters; `qrec` if nothing is left.
fn folder_project_name(folder: Option<&str>) -> String {
    let name: String = folder
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    match name.trim() {
        "" => "qrec".to_owned(),
        name => name.to_owned(),
    }
}

fn defaults_problem(defaults: &ProjectDefaults) -> Option<String> {
    canvas_problem(&defaults.canvas)
        .or_else(|| countdown_problem(defaults.recording.countdown_secs))
        .or_else(|| autocut_problem(&defaults.autocut))
}

// -------------------------------------------------------------- project set

/// `qrec project set`. A new canvas fps re-quantizes every clip
/// (`requantize`); on a project with takes it needs confirmation, whose
/// `details` list every clip (`clips: [{id, take, before, after}]`) and the
/// clips that would end up empty (`removedClips: [{id, take, before}]`).
/// Layer rects stay in canvas pixels when the size changes.
#[allow(clippy::too_many_arguments)]
pub fn project_set(
    state: &State,
    ctx: &Ctx,
    name: Option<String>,
    canvas: Option<CanvasSpec>,
    background: Option<Color>,
    countdown_secs: Option<u32>,
    encoder: Option<EncoderPref>,
    vaapi_device: Nullable<String>,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::ProjectSet {
        name: name.clone(),
        canvas,
        background: background.clone(),
        countdown_secs,
        encoder,
        vaapi_device: vaapi_device.clone(),
    }
    .to_json();
    if name.is_none()
        && canvas.is_none()
        && background.is_none()
        && countdown_secs.is_none()
        && encoder.is_none()
        && vaapi_device.is_keep()
    {
        return Err(CoreError::usage("nothing to change").with_hint(
            "qrec project set [--name N] [--canvas WxH@FPS] [--background #RRGGBB] \
             [--countdown SECS] [--encoder auto|vaapi|x264] [--vaapi-device NODE|auto]",
        ));
    }

    // Check every argument before anything else (and before confirming).
    let name = name.map(|n| clean_text("project name", &n)).transpose()?;
    let current = &state.project;
    let new_canvas = match &canvas {
        Some(spec) => with_canvas_spec(&current.canvas, spec)?,
        None => current.canvas.clone(),
    };
    if let Some(problem) = countdown_secs.and_then(countdown_problem) {
        return Err(CoreError::usage(problem));
    }
    if let Nullable::Set(device) = &vaapi_device {
        check_vaapi_device(device)?;
    }

    let mut next = state.clone();
    let mut changes = Vec::new();
    let p = &mut next.project;
    if let Some(name) = name.filter(|n| *n != p.name) {
        changes.push(format!("name {name:?}"));
        p.name = name;
    }
    if (new_canvas.width, new_canvas.height) != (p.canvas.width, p.canvas.height) {
        changes.push(format!("canvas {}x{}", new_canvas.width, new_canvas.height));
        p.canvas.width = new_canvas.width;
        p.canvas.height = new_canvas.height;
    }
    let new_fps = new_canvas.fps;
    if new_fps != p.canvas.fps {
        changes.push(format!("frame rate {} fps", fps_label(new_fps)));
    }
    if let Some(color) = background.filter(|c| *c != p.canvas.background) {
        changes.push(format!("background {color}"));
        p.canvas.background = color;
    }
    if let Some(secs) = countdown_secs.filter(|s| *s != p.recording.countdown_secs) {
        changes.push(format!("countdown {secs} s"));
        p.recording.countdown_secs = secs;
    }
    if let Some(encoder) = encoder.filter(|e| *e != p.recording.encoder) {
        changes.push(format!("encoder {}", encoder_label(encoder)));
        p.recording.encoder = encoder;
    }
    let device = vaapi_device.apply(p.recording.vaapi_device.clone());
    if device != p.recording.vaapi_device {
        changes.push(format!(
            "VA-API device {}",
            device.as_deref().unwrap_or("auto")
        ));
        p.recording.vaapi_device = device;
    }

    let mut plan = None;
    if new_fps != current.canvas.fps {
        let requantized = requantize::requantize(current, new_fps);
        if !current.takes.is_empty() && !ctx.confirmed {
            return Err(CoreError::confirmation_required(fps_change_summary(
                current,
                &requantized,
            ))
            .with_details(requantize_details(&requantized)));
        }
        requantize::apply_requantized(&mut next.project, &requantized);
        plan = Some(requantized);
    }

    if changes.is_empty() {
        let committed = commit(state, ctx, String::new(), cmd, next);
        let result = CmdResult::new("Nothing changed: the project already has these settings");
        return Ok(Outcome::done(committed, result));
    }
    let mut summary = changes.join(", ");
    let mut removed = Vec::new();
    let mut view = json!({
        "name": next.project.name,
        "canvas": next.project.canvas,
        "recording": next.project.recording,
    });
    if let Some(plan) = &plan {
        if !plan.mapped.is_empty() || !plan.removed.is_empty() {
            summary.push_str(&format!(" ({})", requantized_note(plan)));
        }
        removed = plan.removed.iter().map(|c| c.id.to_string()).collect();
        view["requantized"] = requantize_details(plan);
    }
    let label = format!("Project settings: {summary}");
    let message = format!("Changed project settings: {summary}");
    let committed = commit(state, ctx, label, cmd, next);
    let result = CmdResult::new(message)
        .with_removed(removed)
        .with_result(&view);
    Ok(Outcome::done(committed, result))
}

/// What the fps change re-quantizes, for the confirmation message.
fn fps_change_summary(project: &Project, plan: &Requantized) -> String {
    let change = format!(
        "change the frame rate from {} to {} fps",
        fps_label(plan.from),
        fps_label(plan.to)
    );
    let total = plan.mapped.len() + plan.removed.len();
    let takes = project.takes.len();
    let takes = format!("{takes} take{}", plural(takes));
    match (total, plan.removed.len()) {
        (0, _) => {
            format!("{change}: the lengths of {takes} are re-derived (no clips on the timeline)")
        }
        (total, 0) => format!(
            "{change}: the bounds of {} are re-quantized",
            all_clips(total)
        ),
        (total, removed) => format!(
            "{change}: the bounds of {} are re-quantized and {removed} that end{} up empty {} removed",
            all_clips(total),
            if removed == 1 { "s" } else { "" },
            if removed == 1 { "is" } else { "are" },
        ),
    }
}

/// "re-quantized 12 clips, removed 1 that ended up empty (c004)".
fn requantized_note(plan: &Requantized) -> String {
    let total = plan.mapped.len() + plan.removed.len();
    let mut note = format!("re-quantized {total} clip{}", plural(total));
    if !plan.removed.is_empty() {
        let ids: Vec<&str> = plan.removed.iter().map(|c| c.id.as_str()).collect();
        note.push_str(&format!(
            ", removed {} that ended up empty ({})",
            ids.len(),
            ids.join(", ")
        ));
    }
    note
}

/// The confirmation `details` (A§4.8): every clip before/after and the
/// clips that would end up empty.
pub fn requantize_details(plan: &Requantized) -> Value {
    let clips: Vec<Value> = plan
        .mapped
        .iter()
        .map(|c| json!({"id": c.id, "take": c.take, "before": c.before, "after": c.after}))
        .collect();
    let removed: Vec<Value> = plan
        .removed
        .iter()
        .map(|c| json!({"id": c.id, "take": c.take, "before": c.before}))
        .collect();
    json!({"from": plan.from, "to": plan.to, "clips": clips, "removedClips": removed})
}

fn all_clips(n: usize) -> String {
    if n == 1 {
        "the only clip".to_owned()
    } else {
        format!("all {n} clips")
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `current` with the parts `spec` gives; bad values are `Usage`.
fn with_canvas_spec(current: &Canvas, spec: &CanvasSpec) -> Result<Canvas, CoreError> {
    let mut canvas = current.clone();
    match (spec.width, spec.height) {
        (Some(width), Some(height)) => {
            if let Some(problem) =
                side_problem("width", width).or_else(|| side_problem("height", height))
            {
                return Err(CoreError::usage(problem).with_hint(CANVAS_HINT));
            }
            canvas.width = width;
            canvas.height = height;
        }
        (None, None) => {}
        _ => {
            return Err(
                CoreError::usage("give both the canvas width and height").with_hint(CANVAS_HINT)
            );
        }
    }
    if let Some(fps) = spec.fps {
        if let Some(problem) = fps_problem(fps) {
            return Err(CoreError::usage(problem).with_hint(CANVAS_HINT));
        }
        canvas.fps = fps;
    }
    Ok(canvas)
}

fn canvas_problem(canvas: &Canvas) -> Option<String> {
    side_problem("width", canvas.width)
        .or_else(|| side_problem("height", canvas.height))
        .or_else(|| fps_problem(canvas.fps))
}

fn side_problem(what: &str, value: u32) -> Option<String> {
    (value % 2 != 0 || !(MIN_CANVAS..=MAX_CANVAS).contains(&value)).then(|| {
        format!("the canvas {what} must be even and {MIN_CANVAS} to {MAX_CANVAS} px (got {value})")
    })
}

fn fps_problem(fps: Fps) -> Option<String> {
    (fps.num == 0 || fps.den == 0 || fps.as_f64() > MAX_FPS)
        .then(|| format!("the frame rate must be above 0 and at most 240 fps (got {fps})"))
}

fn countdown_problem(secs: u32) -> Option<String> {
    (secs > MAX_COUNTDOWN_SECS)
        .then(|| format!("the countdown must be 0 to {MAX_COUNTDOWN_SECS} s (got {secs})"))
}

fn check_vaapi_device(device: &str) -> Result<(), CoreError> {
    if device.starts_with('/') && !device.chars().any(char::is_control) {
        return Ok(());
    }
    Err(
        CoreError::usage(format!("{device:?} is not a device path")).with_hint(
            "use a render node such as /dev/dri/renderD129, or `--vaapi-device auto` \
             to let Qrec pick an Intel or AMD node",
        ),
    )
}

// ---------------------------------------------------------- autocut settings

/// `qrec autocut settings …`: edits `project.autocut` (undoable). A track
/// that is in no scene at all is `NotFound`; one only some takes have is
/// accepted with a warning (takes without it are skipped, A§2.3).
pub fn autocut_settings(
    state: &State,
    ctx: &Ctx,
    patch: AutocutPatch,
) -> Result<Outcome, CoreError> {
    require_project(state)?;
    let cmd = Command::AutocutSettings {
        patch: patch.clone(),
    }
    .to_json();
    let AutocutPatch {
        auto_run_after_take,
        track,
        threshold_db,
        min_silence_ms,
        padding_ms,
        min_keep_ms,
    } = patch;
    if auto_run_after_take.is_none()
        && track.is_keep()
        && threshold_db.is_none()
        && min_silence_ms.is_none()
        && padding_ms.is_none()
        && min_keep_ms.is_none()
    {
        return Err(CoreError::usage("nothing to change").with_hint(
            "qrec autocut settings [--auto-run BOOL] [--track A1|auto] [--threshold-db DB] \
             [--min-silence-ms MS] [--padding-ms MS] [--min-keep-ms MS]",
        ));
    }

    let mut settings = state.project.autocut.clone();
    if let Some(v) = auto_run_after_take {
        settings.auto_run_after_take = v;
    }
    let track_given = !track.is_keep();
    settings.track = track.apply(settings.track);
    if let Some(v) = threshold_db {
        settings.threshold_db = v;
    }
    if let Some(v) = min_silence_ms {
        settings.min_silence_ms = v;
    }
    if let Some(v) = padding_ms {
        settings.padding_ms = v;
    }
    if let Some(v) = min_keep_ms {
        settings.min_keep_ms = v;
    }
    if let Some(problem) = autocut_problem(&settings) {
        return Err(CoreError::usage(problem));
    }
    let project = &state.project;
    let mut warning = None;
    if let Some(id) = settings.track.as_ref().filter(|_| track_given) {
        if project.scene.track(id).is_none() {
            if !project.takes.values().any(|t| t.scene.track(id).is_some()) {
                let ids: Vec<&str> = project.scene.audio.iter().map(|t| t.id.as_str()).collect();
                return Err(
                    CoreError::not_found(format!("no audio track {id}")).with_hint(if ids.is_empty() {
                        "the project scene has no audio tracks (add one with `qrec scene audio add mic`)".to_owned()
                    } else {
                        format!("tracks in the project scene: {}", ids.join(", "))
                    }),
                );
            }
            warning = Some(format!(
                "{id} isn't in the project scene; takes without it are skipped"
            ));
        }
    }

    let before = &project.autocut;
    let mut changes = Vec::new();
    if settings.auto_run_after_take != before.auto_run_after_take {
        let on = if settings.auto_run_after_take {
            "on"
        } else {
            "off"
        };
        changes.push(format!("auto-run {on}"));
    }
    if settings.track != before.track {
        let track = settings
            .track
            .as_ref()
            .map_or("auto (first mic)".to_owned(), |t| t.to_string());
        changes.push(format!("track {track}"));
    }
    if settings.threshold_db != before.threshold_db {
        changes.push(format!("threshold {} dB", settings.threshold_db));
    }
    if settings.min_silence_ms != before.min_silence_ms {
        changes.push(format!("min silence {} ms", settings.min_silence_ms));
    }
    if settings.padding_ms != before.padding_ms {
        changes.push(format!("padding {} ms", settings.padding_ms));
    }
    if settings.min_keep_ms != before.min_keep_ms {
        changes.push(format!("min keep {} ms", settings.min_keep_ms));
    }

    let mut next = state.clone();
    next.project.autocut = settings;
    let view = serde_json::to_value(&next.project.autocut).expect("settings serialize");
    let mut message = if changes.is_empty() {
        "Nothing changed: autocut already uses these settings".to_owned()
    } else {
        format!("Changed autocut settings: {}", changes.join(", "))
    };
    if let Some(warning) = warning {
        message.push_str(&format!(" (warning: {warning})"));
    }
    let label = format!("Autocut settings: {}", changes.join(", "));
    let committed = commit(state, ctx, label, cmd, next);
    Ok(Outcome::done(
        committed,
        CmdResult::new(message).with_result(&view),
    ))
}

fn autocut_problem(a: &AutocutSettings) -> Option<String> {
    if !a.threshold_db.is_finite() || !(MIN_THRESHOLD_DB..=0.0).contains(&a.threshold_db) {
        return Some(format!(
            "the silence threshold must be -90 to 0 dB (got {})",
            a.threshold_db
        ));
    }
    [
        ("the minimum silence", a.min_silence_ms),
        ("the padding", a.padding_ms),
        ("the minimum kept fragment", a.min_keep_ms),
    ]
    .into_iter()
    .find(|(_, ms)| *ms > MAX_AUTOCUT_MS)
    .map(|(what, ms)| format!("{what} must be at most {MAX_AUTOCUT_MS} ms (got {ms})"))
}

// ------------------------------------------------------------------ helpers

/// A name or label as given, trimmed; empty or with control characters is `Usage`.
pub(crate) fn clean_text(what: &str, text: &str) -> Result<String, CoreError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(CoreError::usage(format!("the {what} can't be empty")));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(CoreError::usage(format!(
            "the {what} can't contain control characters (got {text:?})"
        )));
    }
    Ok(trimmed.to_owned())
}

/// `30`, `29.97`, `23.976`, `12.5`.
pub(crate) fn fps_label(fps: Fps) -> String {
    if fps.den == 1 {
        return fps.num.to_string();
    }
    let text = format!("{:.3}", fps.as_f64());
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// `1920x1080 @ 30 fps`.
pub(crate) fn canvas_label(canvas: &Canvas) -> String {
    format!(
        "{}x{} @ {} fps",
        canvas.width,
        canvas.height,
        fps_label(canvas.fps)
    )
}

fn encoder_label(encoder: EncoderPref) -> &'static str {
    match encoder {
        EncoderPref::Auto => "auto",
        EncoderPref::Vaapi => "vaapi",
        EncoderPref::X264 => "x264",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::history::view::{project_value, undoable_view};
    use crate::ids::{Frame, TakeId, TrackId};
    use crate::model::CURRENT_VERSION;
    use crate::protocol::effect::Effect;
    use crate::testkit::{self, ProjectBuilder};

    fn ctx() -> Ctx {
        testkit::ctx("2026-10-14T12:30:00Z", 7)
    }

    fn confirmed() -> Ctx {
        let mut ctx = ctx();
        ctx.confirmed = true;
        ctx
    }

    fn blank() -> State {
        uninitialized_state(HostKind::Cli)
    }

    fn reply(out: &Outcome) -> &CmdResult {
        out.reply.as_ref().unwrap().as_ref().unwrap()
    }

    fn bounds(project: &Project) -> Vec<(&str, Frame, Frame)> {
        project
            .timeline
            .clips
            .iter()
            .map(|c| (c.id.as_str(), c.in_, c.out))
            .collect()
    }

    fn take_id(n: u32) -> TakeId {
        TakeId::numbered(n)
    }

    fn track(id: &str) -> TrackId {
        id.parse().unwrap()
    }

    /// Two takes at 30 fps; take 1 is split at 150.
    fn with_takes() -> State {
        testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-1")
                .mic("A1", "alsa_input.mic")
                .take("take-0001", 300)
                .take("take-0002", 90)
                .clip("c001", "take-0001", 0, 150)
                .clip("c002", "take-0001", 150, 300)
                .clip("c003", "take-0002", 0, 90)
                .build(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn set(
        state: &State,
        ctx: &Ctx,
        name: Option<&str>,
        canvas: Option<&str>,
        background: Option<&str>,
        countdown: Option<u32>,
        encoder: Option<EncoderPref>,
        device: Nullable<String>,
    ) -> Result<Outcome, CoreError> {
        project_set(
            state,
            ctx,
            name.map(str::to_owned),
            canvas.map(|c| c.parse().unwrap()),
            background.map(|c| c.parse().unwrap()),
            countdown,
            encoder,
            device,
        )
    }

    fn canvas_only(state: &State, ctx: &Ctx, canvas: &str) -> Result<Outcome, CoreError> {
        set(
            state,
            ctx,
            None,
            Some(canvas),
            None,
            None,
            None,
            Nullable::Keep,
        )
    }

    // ------------------------------------------------------------------ init

    #[test]
    fn init_builds_the_project_from_config_and_folder() {
        let mut ctx = ctx();
        ctx.facts.folder_name = Some("my-video".into());
        ctx.facts.config = toml::from_str(
            "[canvas]\nfps = \"30000/1001\"\nbackground = \"#12170f\"\n\
             [recording]\ncountdown_secs = 3\n\
             [autocut]\nthreshold_db = -40.0\nauto_run_after_take = true\n\
             [encoder]\npreference = \"x264\"\nvaapi_device = \"/dev/dri/renderD129\"\n",
        )
        .unwrap();
        let out = init(&blank(), &ctx, None, None).unwrap();
        let p = &out.state.project;
        assert!(is_initialized(p));
        assert_eq!(p.version, CURRENT_VERSION);
        assert_eq!(p.name, "my-video");
        assert_eq!(p.created_at, ctx.now);
        assert_eq!((p.canvas.width, p.canvas.height), (1920, 1080));
        assert_eq!(p.canvas.fps, Fps::new(30000, 1001));
        assert_eq!(p.canvas.background.as_str(), "#12170F");
        assert_eq!(p.recording.countdown_secs, 3);
        assert_eq!(
            p.recording.encoder,
            EncoderPref::Auto,
            "the config preference applies when `auto` is resolved"
        );
        assert_eq!(
            p.recording.vaapi_device, None,
            "init never copies a VA device"
        );
        assert_eq!(p.autocut.threshold_db, -40.0);
        assert!(p.autocut.auto_run_after_take);
        assert_eq!(p.autocut.track, None);
        assert_eq!(p.next_take_number, 1);
        assert!(p.scene.is_empty() && p.takes.is_empty() && p.timeline.clips.is_empty());
        assert_eq!(out.state.history, History::new());
        assert_eq!(out.effects, vec![Effect::WriteProject]);
        let r = reply(&out);
        assert_eq!(r.seq, None, "init is not undoable");
        assert_eq!(
            r.message,
            "Created project \"my-video\" (1920x1080 @ 29.97 fps)"
        );
        assert_eq!(r.result["name"], "my-video");
    }

    #[test]
    fn init_arguments_override_the_config() {
        let mut ctx = ctx();
        ctx.facts.folder_name = Some("folder".into());
        let canvas = Some("1280x720@60".parse().unwrap());
        let out = init(&blank(), &ctx, canvas, Some("  Demo ".into())).unwrap();
        let p = &out.state.project;
        assert_eq!(p.name, "Demo");
        assert_eq!(
            (p.canvas.width, p.canvas.height, p.canvas.fps),
            (1280, 720, Fps::new(60, 1))
        );

        let out = init(&blank(), &ctx, Some("@25".parse().unwrap()), None).unwrap();
        let p = &out.state.project;
        assert_eq!((p.canvas.width, p.canvas.fps), (1920, Fps::new(25, 1)));
        assert_eq!(p.name, "folder");
    }

    #[test]
    fn init_refuses_an_existing_project() {
        let state = testkit::state(ProjectBuilder::new().name("talk").build());
        let err = init(&state, &ctx(), None, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(err.message, "already a project");
        assert!(err.hint.unwrap().contains("\"talk\""));
    }

    #[test]
    fn init_checks_arguments_and_config() {
        let usage = |canvas: Option<&str>, name: Option<&str>| {
            let canvas = canvas.map(|c| c.parse().unwrap());
            init(&blank(), &ctx(), canvas, name.map(str::to_owned))
                .unwrap_err()
                .code
        };
        assert_eq!(usage(Some("1921x1080"), None), ErrorCode::Usage);
        assert_eq!(usage(Some("8194x1080"), None), ErrorCode::Usage);
        assert_eq!(usage(Some("14x14"), None), ErrorCode::Usage);
        assert_eq!(usage(None, Some("  ")), ErrorCode::Usage);
        assert_eq!(usage(None, Some("a\nb")), ErrorCode::Usage);

        // Bad config values are the config's fault: Invalid, with a hint.
        let mut ctx = ctx();
        ctx.facts.config.recording.countdown_secs = 99;
        let err = init(&blank(), &ctx, None, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert!(err.message.contains("countdown"), "{}", err.message);
        assert!(err.hint.unwrap().contains("config.toml"));
        ctx.facts.config.recording.countdown_secs = 0;
        ctx.facts.config.autocut.min_keep_ms = 70_000;
        assert_eq!(
            init(&blank(), &ctx, None, None).unwrap_err().code,
            ErrorCode::Invalid
        );
        ctx.facts.config.autocut.min_keep_ms = 200;
        ctx.facts.config.canvas.width = 1001;
        assert_eq!(
            init(&blank(), &ctx, None, None).unwrap_err().code,
            ErrorCode::Invalid
        );
        // An explicit canvas replaces the bad config value.
        let canvas = Some("1280x720".parse().unwrap());
        assert!(init(&blank(), &ctx, canvas, None).is_ok());
    }

    #[test]
    fn folder_names() {
        assert_eq!(folder_project_name(None), "qrec");
        assert_eq!(folder_project_name(Some(" talk\u{7} ")), "talk");
        assert_eq!(folder_project_name(Some("\n")), "qrec");
        assert_eq!(folder_project_name(Some("Mon film")), "Mon film");
    }

    #[test]
    fn only_init_runs_without_a_project() {
        let state = blank();
        assert!(!is_initialized(&state.project));
        let err = set(
            &state,
            &ctx(),
            Some("x"),
            None,
            None,
            None,
            None,
            Nullable::Keep,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotProject);
        let patch = AutocutPatch {
            padding_ms: Some(1),
            ..Default::default()
        };
        let err = autocut_settings(&state, &ctx(), patch).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotProject);
    }

    // ----------------------------------------------------------- project set

    #[test]
    fn project_set_changes_settings_in_one_entry() {
        let state = with_takes();
        let out = set(
            &state,
            &ctx(),
            Some("Talk"),
            Some("1280x720"),
            Some("#12170f"),
            Some(3),
            Some(EncoderPref::X264),
            Nullable::Set("/dev/dri/renderD129".into()),
        )
        .unwrap();
        let p = &out.state.project;
        assert_eq!(p.name, "Talk");
        assert_eq!((p.canvas.width, p.canvas.height), (1280, 720));
        assert_eq!(p.canvas.fps, Fps::new(30, 1));
        assert_eq!(p.canvas.background.as_str(), "#12170F");
        assert_eq!(p.recording.countdown_secs, 3);
        assert_eq!(p.recording.encoder, EncoderPref::X264);
        assert_eq!(
            p.recording.vaapi_device.as_deref(),
            Some("/dev/dri/renderD129")
        );
        assert_eq!(
            p.timeline, state.project.timeline,
            "a size change keeps the clips"
        );
        assert_eq!(p.scene, state.project.scene, "rects stay in canvas pixels");
        assert_eq!(p.takes, state.project.takes);

        assert_eq!(reply(&out).seq, Some(1));
        // A GUI host in Live mode also gets the live pipeline update.
        assert!(matches!(
            out.effects[..],
            [
                Effect::History(_),
                Effect::WriteProject,
                Effect::SetLive(Some(_))
            ]
        ));
        let entry = &out.state.history.entries[0];
        assert_eq!(
            entry.label,
            "Project settings: name \"Talk\", canvas 1280x720, background #12170F, \
             countdown 3 s, encoder x264, VA-API device /dev/dri/renderD129"
        );
        assert_eq!(entry.cmd["cmd"], "projectSet");
        assert_eq!(entry.cmd["vaapiDevice"], "/dev/dri/renderD129");

        // `auto` clears the device.
        let cleared = set(
            &out.state,
            &ctx(),
            None,
            None,
            None,
            None,
            None,
            Nullable::Clear,
        )
        .unwrap();
        assert_eq!(cleared.state.project.recording.vaapi_device, None);
        assert_eq!(
            cleared.state.history.entries[1].label,
            "Project settings: VA-API device auto"
        );

        // Setting what is already there appends nothing.
        let same = set(
            &cleared.state,
            &ctx(),
            Some(" Talk "),
            Some("1280x720@30"),
            None,
            Some(3),
            None,
            Nullable::Clear,
        )
        .unwrap();
        assert_eq!(reply(&same).seq, None);
        assert!(same.effects.is_empty());
        assert_eq!(same.state, cleared.state);
        assert!(reply(&same).message.starts_with("Nothing changed"));
    }

    #[test]
    fn project_set_checks_its_arguments() {
        let state = with_takes();
        let code = |r: Result<Outcome, CoreError>| r.unwrap_err().code;
        assert_eq!(
            code(set(
                &state,
                &ctx(),
                None,
                None,
                None,
                None,
                None,
                Nullable::Keep
            )),
            ErrorCode::Usage,
            "nothing to change"
        );
        assert_eq!(
            code(set(
                &state,
                &ctx(),
                None,
                None,
                None,
                Some(11),
                None,
                Nullable::Keep
            )),
            ErrorCode::Usage
        );
        assert_eq!(
            code(canvas_only(&state, &ctx(), "1920x1081")),
            ErrorCode::Usage
        );
        assert_eq!(
            code(canvas_only(&state, &ctx(), "16384x1080")),
            ErrorCode::Usage
        );
        assert_eq!(
            code(set(
                &state,
                &ctx(),
                Some(""),
                None,
                None,
                None,
                None,
                Nullable::Keep
            )),
            ErrorCode::Usage
        );
        let device = Nullable::Set("renderD129".to_owned());
        assert_eq!(
            code(set(&state, &ctx(), None, None, None, None, None, device)),
            ErrorCode::Usage
        );
        let half = CanvasSpec {
            width: Some(1280),
            height: None,
            fps: None,
        };
        let r = project_set(
            &state,
            &ctx(),
            None,
            Some(half),
            None,
            None,
            None,
            Nullable::Keep,
        );
        assert_eq!(code(r), ErrorCode::Usage);
        let fast = CanvasSpec {
            width: None,
            height: None,
            fps: Some(Fps::new(480, 1)),
        };
        let r = project_set(
            &state,
            &confirmed(),
            None,
            Some(fast),
            None,
            None,
            None,
            Nullable::Keep,
        );
        assert_eq!(code(r), ErrorCode::Usage);
        // A bad argument is reported before the confirmation.
        let r = set(
            &state,
            &ctx(),
            None,
            Some("@60"),
            None,
            Some(11),
            None,
            Nullable::Keep,
        );
        assert_eq!(code(r), ErrorCode::Usage);
    }

    #[test]
    fn an_fps_change_with_takes_needs_confirmation() {
        let state = with_takes();
        let err = canvas_only(&state, &ctx(), "@60").unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfirmationRequired);
        assert_eq!(
            err.message,
            "confirmation required: change the frame rate from 30 to 60 fps: \
             the bounds of all 3 clips are re-quantized"
        );
        let details = err.details.unwrap();
        assert_eq!(details["from"], json!({"num": 30, "den": 1}));
        assert_eq!(details["to"], json!({"num": 60, "den": 1}));
        assert_eq!(
            details["clips"],
            json!([
                {"id": "c001", "take": "take-0001", "before": [0, 150], "after": [0, 300]},
                {"id": "c002", "take": "take-0001", "before": [150, 300], "after": [300, 600]},
                {"id": "c003", "take": "take-0002", "before": [0, 90], "after": [0, 180]},
            ])
        );
        assert_eq!(details["removedClips"], json!([]));

        let out = canvas_only(&state, &confirmed(), "@60").unwrap();
        let p = &out.state.project;
        assert_eq!(p.canvas.fps, Fps::new(60, 1));
        assert_eq!(
            bounds(p),
            [("c001", 0, 300), ("c002", 300, 600), ("c003", 0, 180)]
        );
        assert_eq!(p.takes[&take_id(1)].duration_frames, 600);
        assert_eq!(p.takes[&take_id(2)].duration_frames, 180);
        let r = reply(&out);
        assert_eq!(r.seq, Some(1));
        assert!(r.removed.is_empty());
        assert_eq!(r.result["requantized"], details);
        assert_eq!(
            out.state.history.entries[0].label,
            "Project settings: frame rate 60 fps (re-quantized 3 clips)"
        );
    }

    #[test]
    fn clips_that_end_up_empty_are_removed_in_the_same_entry() {
        let state = testkit::state(
            ProjectBuilder::new()
                .canvas(1920, 1080, Fps::new(60, 1))
                .screen_layer("L1", "DP-1")
                .take("take-0001", 600)
                .clip("c001", "take-0001", 3, 11)
                .clip("c002", "take-0001", 11, 12)
                .clip("c003", "take-0001", 12, 41)
                .build(),
        );
        let err = canvas_only(&state, &ctx(), "@30").unwrap_err();
        assert_eq!(
            err.message,
            "confirmation required: change the frame rate from 60 to 30 fps: the bounds of \
             all 3 clips are re-quantized and 1 that ends up empty is removed"
        );
        let details = err.details.unwrap();
        assert_eq!(
            details["removedClips"],
            json!([{"id": "c002", "take": "take-0001", "before": [11, 12]}])
        );
        assert_eq!(details["clips"].as_array().unwrap().len(), 2);

        let out = canvas_only(&state, &confirmed(), "@30").unwrap();
        assert_eq!(
            bounds(&out.state.project),
            [("c001", 1, 6), ("c003", 6, 21)]
        );
        let r = reply(&out);
        assert_eq!(r.removed, ["c002"]);
        assert_eq!(
            r.message,
            "Changed project settings: frame rate 30 fps \
             (re-quantized 3 clips, removed 1 that ended up empty (c002))"
        );
        assert_eq!(
            out.state.history.entries.len(),
            1,
            "one entry for everything"
        );
    }

    #[test]
    fn an_fps_change_without_takes_needs_no_confirmation() {
        let state = testkit::state(ProjectBuilder::new().screen_layer("L1", "DP-1").build());
        let out = canvas_only(&state, &ctx(), "@29.97").unwrap();
        assert_eq!(out.state.project.canvas.fps, Fps::new(30000, 1001));
        assert_eq!(reply(&out).seq, Some(1));
        assert_eq!(
            out.state.history.entries[0].label,
            "Project settings: frame rate 29.97 fps"
        );

        // Takes without clips still ask (SPEC §11: "a project with takes").
        let state = testkit::state(
            ProjectBuilder::new()
                .screen_layer("L1", "DP-1")
                .take("take-0001", 300)
                .build(),
        );
        let err = canvas_only(&state, &ctx(), "@60").unwrap_err();
        assert_eq!(
            err.message,
            "confirmation required: change the frame rate from 30 to 60 fps: \
             the lengths of 1 take are re-derived (no clips on the timeline)"
        );
        let out = canvas_only(&state, &confirmed(), "@60").unwrap();
        assert_eq!(out.state.project.takes[&take_id(1)].duration_frames, 600);
    }

    /// Take 2 recorded over c00b (take 1, frames 30..60); take 3 has no media
    /// info (like the SPEC §6.3 example), so its length can't be derived.
    fn with_record_over() -> State {
        let mut project = ProjectBuilder::new()
            .screen_layer("L1", "DP-1")
            .mic("A1", "alsa_input.mic")
            .take("take-0001", 300)
            .take("take-0002", 90)
            .take("take-0003", 90)
            .clip("c00a", "take-0001", 0, 30)
            .clip("c003", "take-0002", 0, 90)
            .clip("c00c", "take-0001", 60, 300)
            .clip("c004", "take-0003", 0, 90)
            .build();
        let t2 = project.takes.get_mut(&take_id(2)).unwrap();
        t2.placed = true;
        t2.replaced = vec![crate::model::Clip {
            id: "c00b".parse().unwrap(),
            take: take_id(1),
            in_: 30,
            out: 60,
        }];
        for stream in &mut project.takes.get_mut(&take_id(3)).unwrap().streams {
            stream.frames = None;
            stream.duration_ns = None;
            stream.fps = None;
        }
        testkit::state(project)
    }

    #[test]
    fn undoing_an_fps_change_restores_bounds_and_durations() {
        // Through the history owner's undo and redo (A§2.2: durations are
        // re-derived on undo/redo of an fps change; A§5.5: `replaced` must
        // keep pointing at the frames the record-over replaced).
        let state = with_record_over();
        let at60 = canvas_only(&state, &confirmed(), "@60").unwrap().state;
        let t2 = &at60.project.takes[&take_id(2)];
        assert_eq!(
            (t2.replaced[0].in_, t2.replaced[0].out),
            (60, 120),
            "the plan re-quantizes replaced clips"
        );
        assert_eq!(at60.project.takes[&take_id(3)].duration_frames, 180);

        let undone = crate::apply::history::undo(&at60, &ctx(), 1).unwrap().state;
        assert_eq!(undone.project, state.project, "every fact back at 30 fps");
        let mut view = undoable_view(&undone.project);
        assert_eq!(view, undoable_view(&state.project));
        let redone = crate::apply::history::redo(&undone, &ctx(), 1)
            .unwrap()
            .state;
        assert_eq!(redone.project, at60.project);
        let restored = crate::apply::history::history_restore(&redone, &confirmed(), 0)
            .unwrap()
            .state;
        assert_eq!(restored.project, state.project);

        // Discarding take 2 after the undo puts back frames 30..60, not 60..120.
        let discarded = crate::apply::record::take_discard_last(&undone, &ctx()).unwrap();
        assert_eq!(
            bounds(&discarded.state.project),
            [
                ("c00a", 0, 30),
                ("c00b", 30, 60),
                ("c00c", 60, 300),
                ("c004", 0, 90)
            ]
        );
        // The patch alone restores the undoable view; facts come from retiming.
        let entry = &at60.history.entries[0];
        view = project_value(&at60.project);
        json_patch::patch(&mut view, &entry.inverse).unwrap();
        let patched: Project = serde_json::from_value(view).unwrap();
        assert_ne!(patched.takes, state.project.takes, "the patch has no facts");
    }

    #[test]
    fn undoing_a_lower_fps_keeps_takes_without_media_info_long_enough() {
        // 30 → 15 fps halves take 3 (no media info) to 45 frames; undo must
        // convert it back, or c004 (0..90 at 30 fps) wouldn't fit any more.
        let state = with_record_over();
        let at15 = canvas_only(&state, &confirmed(), "@15").unwrap().state;
        assert_eq!(at15.project.takes[&take_id(3)].duration_frames, 45);
        let undone = crate::apply::history::undo(&at15, &ctx(), 1).unwrap().state;
        assert_eq!(undone.project.takes[&take_id(3)].duration_frames, 90);
        assert_eq!(undone.project, state.project);
    }

    #[test]
    fn labels_for_rates() {
        assert_eq!(fps_label(Fps::new(30, 1)), "30");
        assert_eq!(fps_label(Fps::new(30000, 1001)), "29.97");
        assert_eq!(fps_label(Fps::new(24000, 1001)), "23.976");
        assert_eq!(fps_label(Fps::new(60000, 1001)), "59.94");
        assert_eq!(fps_label(Fps::new(25, 2)), "12.5");
    }

    // ------------------------------------------------------ autocut settings

    #[test]
    fn autocut_settings_edit_the_project_defaults() {
        let state = with_takes();
        let patch = AutocutPatch {
            auto_run_after_take: Some(true),
            track: Nullable::Set(track("A1")),
            threshold_db: Some(-40.0),
            min_silence_ms: Some(500),
            padding_ms: Some(100),
            min_keep_ms: Some(250),
        };
        let out = autocut_settings(&state, &ctx(), patch).unwrap();
        let a = &out.state.project.autocut;
        assert!(a.auto_run_after_take);
        assert_eq!(a.track, Some(track("A1")));
        assert_eq!(
            (
                a.threshold_db,
                a.min_silence_ms,
                a.padding_ms,
                a.min_keep_ms
            ),
            (-40.0, 500, 100, 250)
        );
        let entry = &out.state.history.entries[0];
        assert_eq!(
            entry.label,
            "Autocut settings: auto-run on, track A1, threshold -40 dB, \
             min silence 500 ms, padding 100 ms, min keep 250 ms"
        );
        assert_eq!(entry.cmd["cmd"], "autocutSettings");
        assert_eq!(reply(&out).result["track"], "A1");

        let patch = AutocutPatch {
            track: Nullable::Clear,
            ..Default::default()
        };
        let out = autocut_settings(&out.state, &ctx(), patch).unwrap();
        assert_eq!(out.state.project.autocut.track, None);
        assert_eq!(
            reply(&out).message,
            "Changed autocut settings: track auto (first mic)"
        );

        // The same values again: no entry.
        let patch = AutocutPatch {
            padding_ms: Some(100),
            ..Default::default()
        };
        let same = autocut_settings(&out.state, &ctx(), patch).unwrap();
        assert_eq!(reply(&same).seq, None);
        assert!(same.effects.is_empty());
    }

    #[test]
    fn autocut_settings_check_values_and_tracks() {
        let state = with_takes();
        let code = |patch: AutocutPatch| autocut_settings(&state, &ctx(), patch).unwrap_err().code;
        assert_eq!(code(AutocutPatch::default()), ErrorCode::Usage);
        for db in [5.0, -91.0, f64::NAN] {
            let patch = AutocutPatch {
                threshold_db: Some(db),
                ..Default::default()
            };
            assert_eq!(code(patch), ErrorCode::Usage, "{db}");
        }
        let patch = AutocutPatch {
            min_keep_ms: Some(60_001),
            ..Default::default()
        };
        assert_eq!(code(patch), ErrorCode::Usage);
        let patch = AutocutPatch {
            track: Nullable::Set(track("A9")),
            ..Default::default()
        };
        let err = autocut_settings(&state, &ctx(), patch).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.hint.as_deref(), Some("tracks in the project scene: A1"));

        // A track only some takes have: accepted, with a warning.
        let mut state = state;
        state.project.scene.audio.clear();
        let patch = AutocutPatch {
            track: Nullable::Set(track("A1")),
            ..Default::default()
        };
        let out = autocut_settings(&state, &ctx(), patch).unwrap();
        assert_eq!(out.state.project.autocut.track, Some(track("A1")));
        let message = &reply(&out).message;
        assert!(
            message.ends_with(
                "(warning: A1 isn't in the project scene; takes without it are skipped)"
            ),
            "{message}"
        );
    }

    #[test]
    fn clean_text_trims_and_rejects() {
        assert_eq!(clean_text("name", "  Talk ").unwrap(), "Talk");
        assert_eq!(clean_text("name", "").unwrap_err().code, ErrorCode::Usage);
        assert_eq!(
            clean_text("name", "a\tb").unwrap_err().code,
            ErrorCode::Usage
        );
    }
}
