//! The Onion layers panel (G§4.7, C09, C10, C12, C13): the header and the
//! take banner, the layer rows, the audio rows, the inspector, the Add
//! menus and the commands every control sends. Owner: WP5.
//!
//! - The rows and the inspector show the scene the canvas shows
//!   (`ui_normalize::canvas_scene`): the project scene in Live mode, the
//!   take of the first selected clip in Program mode, and with nothing
//!   selected the take under the playhead, read-only (D-06).
//! - Edits go to `ui_normalize::edit_target`. A control is enabled when
//!   there is one and the gates every command shares let its command
//!   through (`view::enabled`, D-43: not while recording, not with an
//!   invalid `qrec.json`).
//! - Layers and tracks are added to the project scene only (A§4.4), so the
//!   Add menus work in Live mode only (D-07).
//! - Rows are topmost first: row `r` of `n` is the layer at z-index
//!   `n − 1 − r`.

use serde::{Deserialize, Serialize};

use crate::ctx::{AudioDeviceInfo, CameraInfo, ScreenInfo};
use crate::ids::{LayerId, TakeId, TrackId};
use crate::layout;
use crate::model::{AudioRole, Crop, Fit, Project, Rect, Scene, VideoSource};
use crate::protocol::command::{
    AudioPatch, AutocutPatch, Command, LayerPatch, Nullable, SceneDest, SceneTarget, UiPatch,
    VideoSourceArg,
};
use crate::state::{CanvasMode, State};
use crate::ui_normalize::{NO_EDIT_TARGET, canvas_scene, edit_target};
use crate::view::format::{self, MINUS};
use crate::view::{LiveSourceState, LiveSources, enabled};

/// The panel's title.
pub const TITLE: &str = "Onion layers";
/// The subtitle in Live mode.
pub const PROJECT_SUBTITLE: &str = "Project scene \u{b7} new takes use this";
/// The Layers section with no layer (S01a).
pub const NO_LAYERS: &str = "No layers yet";
/// The Audio section with no track (S01a).
pub const NO_TRACKS: &str = "No audio tracks";
/// The inspector with no layer selected.
pub const INSPECTOR_EMPTY: &str =
    "Select a layer on the canvas or in the list to edit its position, size and crop.";
/// The tooltip of the disabled Add buttons in a take layout (D-07).
pub const ADD_IN_LIVE: &str = "Add sources in Live mode (Tab): they go into the project scene";
/// A screen the portal refused (S13b, S24).
pub const ACCESS_DENIED: &str = "Access denied by the desktop portal";
/// A screen whose picker was closed (S24's variant).
pub const PICKER_CLOSED: &str = "Screen picker closed";
/// A take-scene layer whose source is missing (S13b).
pub const MISSING_IN_TAKE: &str = "Missing in this take";
/// A Live audio track whose device doesn't deliver (C10).
pub const DEVICE_NOT_FOUND: &str = "Device not found. Plug it in, or pick another source.";
/// The inspector's position limit: X and Y stay within ±8192 canvas px.
pub const MAX_POSITION: i32 = 8192;
/// The inspector's size limit (canvas px).
pub const MAX_SIZE: u32 = 16384;

// ------------------------------------------------------------- the header

/// The panel header (C13).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelHeader {
    /// "Onion layers".
    pub title: String,
    /// "Project scene · new takes use this", "Take 5 layout · edits this
    /// take only", or `ui_normalize::NO_EDIT_TARGET` (D-06).
    pub subtitle: String,
    /// "Changes here only affect take 5." with the `Layout ▾` menu.
    pub banner: Option<String>,
    /// The `Layout ▾` items, when the banner shows.
    pub layout_menu: Vec<LayoutItem>,
}

/// A `Layout ▾` item and the command it sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutItem {
    /// "Apply this layout to all takes", …
    pub label: String,
    pub from: SceneTarget,
    pub to: SceneDest,
}

impl LayoutItem {
    /// The item's `SceneApply`.
    pub fn command(&self) -> Command {
        Command::SceneApply {
            from: self.from.clone(),
            to: self.to.clone(),
        }
    }
}

/// The header for `state`: the subtitle names the scene edits go to; a
/// take layout adds the banner and its `Layout ▾` menu (S06).
pub fn header(state: &State) -> PanelHeader {
    let (subtitle, banner, layout_menu) = match edit_target(&state.project, &state.ui) {
        Some(SceneTarget::Project) => (PROJECT_SUBTITLE.to_owned(), None, Vec::new()),
        Some(SceneTarget::Take(take)) => {
            let n = take.number();
            (
                format!("Take {n} layout \u{b7} edits this take only"),
                Some(format!("Changes here only affect take {n}.")),
                layout_menu(&take),
            )
        }
        None => (NO_EDIT_TARGET.to_owned(), None, Vec::new()),
    };
    PanelHeader {
        title: TITLE.to_owned(),
        subtitle,
        banner,
        layout_menu,
    }
}

/// The `Layout ▾` items of take `take`'s banner (S06, BRIEF Q8).
pub fn layout_menu(take: &TakeId) -> Vec<LayoutItem> {
    vec![
        LayoutItem {
            label: "Apply this layout to all takes".to_owned(),
            from: SceneTarget::Take(take.clone()),
            to: SceneDest::AllTakes,
        },
        LayoutItem {
            label: "Use this layout for new takes".to_owned(),
            from: SceneTarget::Take(take.clone()),
            to: SceneDest::Project,
        },
        LayoutItem {
            label: "Reset take to project scene".to_owned(),
            from: SceneTarget::Project,
            to: SceneDest::Takes(vec![take.clone()]),
        },
    ]
}

/// Whether the `Layout ▾` menu works now (the gates of `SceneApply`).
pub fn layout_enabled(state: &State) -> Result<(), String> {
    let probe = Command::SceneApply {
        from: SceneTarget::Project,
        to: SceneDest::AllTakes,
    };
    enabled(state, &probe)
}

// ------------------------------------------------------------- layer rows

/// One layer row, topmost first (C09).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerRow {
    pub layer: LayerId,
    pub name: String,
    /// "Camera · Logitech C920", "Screen · DP-4 · GIGA-BYTE GS27QA".
    pub source: String,
    pub selected: bool,
    pub hidden: bool,
    pub locked: bool,
    /// Live: "Access denied by the desktop portal", "Screen picker
    /// closed", or the source's message.
    pub error: Option<String>,
    /// "Missing in this take".
    pub missing: Option<String>,
    /// D-43, or no edit target (D-06).
    pub enabled: bool,
}

/// The layer rows of the scene the canvas shows; `live` gives the errors
/// in Live mode.
pub fn layer_rows(state: &State, live: &LiveSources) -> Vec<LayerRow> {
    let scene = canvas_scene(&state.project, &state.ui);
    let live_mode = state.ui.canvas_mode == CanvasMode::Live;
    let editable = layers_editable(state);
    scene
        .layers
        .iter()
        .rev()
        .map(|layer| LayerRow {
            layer: layer.id.clone(),
            name: layer.name.clone(),
            source: source_line(&layer.source),
            selected: state.ui.selected_layer.as_ref() == Some(&layer.id),
            hidden: !layer.visible,
            locked: layer.locked,
            error: if live_mode {
                live_error(&layer.source, live)
            } else {
                None
            },
            missing: layer.missing.then(|| MISSING_IN_TAKE.to_owned()),
            enabled: editable,
        })
        .collect()
}

/// A layer's source as its row's second line: "Camera · Logitech C920",
/// "Screen · DP-4 · GIGA-BYTE GS27QA" (the label left out when it is empty
/// or the output itself).
pub fn source_line(source: &VideoSource) -> String {
    match source {
        VideoSource::Screen { output, label } if label.is_empty() || label == output => {
            format!("Screen \u{b7} {output}")
        }
        VideoSource::Screen { output, label } => format!("Screen \u{b7} {output} \u{b7} {label}"),
        VideoSource::Camera { device, label } if label.is_empty() => {
            format!("Camera \u{b7} {device}")
        }
        VideoSource::Camera { label, .. } => format!("Camera \u{b7} {label}"),
    }
}

/// How the live status calls a layer's source (its key in [`LiveSources`]).
pub fn source_key(source: &VideoSource) -> &str {
    match source {
        VideoSource::Screen { output, .. } => output,
        VideoSource::Camera { device, .. } => device,
    }
}

/// The error a Live layer row shows for its source's status.
fn live_error(source: &VideoSource, live: &LiveSources) -> Option<String> {
    match &live.get(source_key(source))?.state {
        LiveSourceState::Refused {
            cancelled: true, ..
        } => Some(PICKER_CLOSED.to_owned()),
        LiveSourceState::Refused { .. } => Some(ACCESS_DENIED.to_owned()),
        LiveSourceState::Dead { message } => Some(format::capitalized(message)),
        LiveSourceState::AwaitingPortal | LiveSourceState::Live => None,
    }
}

/// Whether the rows, the inspector and the layer menus edit now: an edit
/// target and the gates of `LayerSet` (D-06, D-43).
fn layers_editable(state: &State) -> bool {
    edit_target(&state.project, &state.ui).is_some_and(|target| {
        let probe = Command::LayerSet {
            target,
            layer: LayerId::numbered(1),
            patch: LayerPatch::default(),
        };
        enabled(state, &probe).is_ok()
    })
}

/// The scene `target` names, if it exists.
fn scene_of<'a>(project: &'a Project, target: &SceneTarget) -> Option<&'a Scene> {
    match target {
        SceneTarget::Project => Some(&project.scene),
        SceneTarget::Take(take) => project.take(take).map(|t| &t.scene),
    }
}

/// A click on a row: the layer becomes the selected one (`SetUi`).
pub fn select_layer(layer: &LayerId) -> Command {
    Command::SetUi {
        patch: UiPatch {
            selected_layer: Nullable::Set(layer.clone()),
            ..UiPatch::default()
        },
    }
}

/// `LayerSet{patch}` on the edited scene (the eye, the lock, a rename);
/// `None` with nothing to edit (D-06).
pub fn layer_set(state: &State, layer: &LayerId, patch: LayerPatch) -> Option<Command> {
    let target = edit_target(&state.project, &state.ui)?;
    Some(Command::LayerSet {
        target,
        layer: layer.clone(),
        patch,
    })
}

/// The overflow's Remove (`LayerRemove`; on a take, from its snapshot only).
pub fn layer_remove(state: &State, layer: &LayerId) -> Option<Command> {
    let target = edit_target(&state.project, &state.ui)?;
    Some(Command::LayerRemove {
        target,
        layer: layer.clone(),
    })
}

/// A rename's command: `LayerSet{name}`, or `None` when the name is blank
/// or unchanged (Esc and an empty field change nothing).
pub fn layer_rename(state: &State, layer: &LayerId, name: &str) -> Option<Command> {
    let name = name.trim();
    let target = edit_target(&state.project, &state.ui)?;
    let current = scene_of(&state.project, &target)?.layer(layer)?;
    if name.is_empty() || name == current.name {
        return None;
    }
    layer_set(
        state,
        layer,
        LayerPatch {
            name: Some(name.to_owned()),
            ..LayerPatch::default()
        },
    )
}

// ----------------------------------------------------------- the reorder

/// The gap a dragged row would drop into: `y` (from the top of the first
/// row) rounded to the nearest row boundary, 0 (above the first row) …
/// `rows` (below the last).
pub fn drop_slot(y: f64, row_height: f64, rows: usize) -> usize {
    if row_height <= 0.0 || !y.is_finite() {
        return 0;
    }
    let slot = (y / row_height).round().max(0.0) as usize;
    slot.min(rows)
}

/// Dropping `layer`'s row into gap `slot` (0 … n, rows topmost first):
/// `LayerOrder{target, layer, to}` with `to = n − 1 − new row`; `None`
/// when the row stays where it is or there is nothing to edit.
pub fn reorder(state: &State, layer: &LayerId, slot: usize) -> Option<Command> {
    let target = edit_target(&state.project, &state.ui)?;
    let scene = scene_of(&state.project, &target)?;
    let index = scene.layer_index(layer)?;
    let n = scene.layers.len();
    let row = n - 1 - index;
    let slot = slot.min(n);
    let new_row = if slot > row { slot - 1 } else { slot };
    if new_row == row {
        return None;
    }
    Some(Command::LayerOrder {
        target,
        layer: layer.clone(),
        to: n - 1 - new_row,
    })
}

/// The overflow's Move up: one row up (one z-index higher); `None` on top.
pub fn move_up(state: &State, layer: &LayerId) -> Option<Command> {
    let row = row_of(state, layer)?;
    reorder(state, layer, row.checked_sub(1)?)
}

/// The overflow's Move down: one row down; `None` at the bottom.
pub fn move_down(state: &State, layer: &LayerId) -> Option<Command> {
    let row = row_of(state, layer)?;
    reorder(state, layer, row + 2)
}

/// `layer`'s row in the edited scene (topmost first).
fn row_of(state: &State, layer: &LayerId) -> Option<usize> {
    let target = edit_target(&state.project, &state.ui)?;
    let scene = scene_of(&state.project, &target)?;
    let index = scene.layer_index(layer)?;
    Some(scene.layers.len() - 1 - index)
}

// ------------------------------------------------------------- audio rows

/// One audio row (C10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioRow {
    pub track: TrackId,
    pub role: AudioRole,
    pub label: String,
    /// The "Autocut" tag.
    pub autocut: bool,
    pub muted: bool,
    /// "0.0 dB".
    pub gain: String,
    pub gain_db: f64,
    /// "+0 ms".
    pub sync: String,
    pub sync_offset_ms: i32,
    /// "Device not found. Plug it in, or pick another source."
    pub device_missing: Option<String>,
    pub enabled: bool,
}

/// The audio rows of the scene the canvas shows. The "Autocut" tag sits on
/// `autocut.track`, or on the scene's first mic (else its first track)
/// when it is unset: the track autocut then uses. In Live mode, once the
/// engine reported its sources, a track whose device isn't live reads
/// "Device not found".
pub fn audio_rows(state: &State, live: &LiveSources) -> Vec<AudioRow> {
    let scene = canvas_scene(&state.project, &state.ui);
    let autocut = autocut_track(state, scene);
    let live_mode = state.ui.canvas_mode == CanvasMode::Live;
    let editable = edit_target(&state.project, &state.ui).is_some_and(|target| {
        let probe = Command::AudioSet {
            target,
            track: TrackId::numbered(1),
            patch: AudioPatch::default(),
        };
        enabled(state, &probe).is_ok()
    });
    scene
        .audio
        .iter()
        .map(|track| AudioRow {
            track: track.id.clone(),
            role: track.role,
            label: track.label.clone(),
            autocut: autocut.as_ref() == Some(&track.id),
            muted: track.muted,
            gain: format::gain_db(track.gain_db),
            gain_db: track.gain_db,
            sync: format::offset_ms(track.sync_offset_ms),
            sync_offset_ms: track.sync_offset_ms,
            device_missing: (live_mode && device_missing(&track.source, live))
                .then(|| DEVICE_NOT_FOUND.to_owned()),
            enabled: editable,
        })
        .collect()
}

/// The track autocut listens to in `scene`: the project's `autocut.track`,
/// else the scene's [`default_track`](crate::autocut::default_track) (its
/// first mic, else its first track).
pub fn autocut_track(state: &State, scene: &Scene) -> Option<TrackId> {
    state
        .project
        .autocut
        .track
        .clone()
        .or_else(|| crate::autocut::default_track(scene).map(|t| t.id.clone()))
}

/// No `Live` status for `source` once the engine reported any (an empty
/// list means nothing was reported yet: the pipeline is still starting).
fn device_missing(source: &str, live: &LiveSources) -> bool {
    !live.is_empty()
        && live
            .get(source)
            .is_none_or(|s| s.state != LiveSourceState::Live)
}

/// `AudioSet{patch}` on the edited scene (`M`, gain, sync, a rename);
/// `None` with nothing to edit.
pub fn audio_set(state: &State, track: &TrackId, patch: AudioPatch) -> Option<Command> {
    let target = edit_target(&state.project, &state.ui)?;
    Some(Command::AudioSet {
        target,
        track: track.clone(),
        patch,
    })
}

/// The overflow's Remove (`AudioRemove`).
pub fn audio_remove(state: &State, track: &TrackId) -> Option<Command> {
    let target = edit_target(&state.project, &state.ui)?;
    Some(Command::AudioRemove {
        target,
        track: track.clone(),
    })
}

/// A track rename: `AudioSet{label}`, or `None` when blank or unchanged.
pub fn audio_rename(state: &State, track: &TrackId, label: &str) -> Option<Command> {
    let label = label.trim();
    let target = edit_target(&state.project, &state.ui)?;
    let current = scene_of(&state.project, &target)?.track(track)?;
    if label.is_empty() || label == current.label {
        return None;
    }
    audio_set(
        state,
        track,
        AudioPatch {
            label: Some(label.to_owned()),
            ..AudioPatch::default()
        },
    )
}

/// What ending an inline rename does (R46).
#[derive(Debug, Clone, PartialEq)]
pub enum RenameOutcome {
    /// Send the rename (`LayerSet{name}`, `AudioSet{label}`).
    Send(Command),
    /// Nothing to send (blank, the same name, nothing to edit): the row
    /// shows this name again instead of what was typed.
    Restore(String),
}

/// The name layer `layer`'s row shows (the scene the canvas shows).
pub fn layer_name(state: &State, layer: &LayerId) -> Option<String> {
    let scene = canvas_scene(&state.project, &state.ui);
    scene.layer(layer).map(|l| l.name.clone())
}

/// The label track `track`'s row shows (the scene the canvas shows).
pub fn track_label(state: &State, track: &TrackId) -> Option<String> {
    let scene = canvas_scene(&state.project, &state.ui);
    scene.track(track).map(|t| t.label.clone())
}

/// A layer rename that ended with `name`: [`layer_rename`]'s command, or
/// the row's name back.
pub fn layer_rename_outcome(state: &State, layer: &LayerId, name: &str) -> RenameOutcome {
    match layer_rename(state, layer, name) {
        Some(cmd) => RenameOutcome::Send(cmd),
        None => RenameOutcome::Restore(layer_name(state, layer).unwrap_or_default()),
    }
}

/// A track rename that ended with `label`: [`audio_rename`]'s command, or
/// the row's label back.
pub fn track_rename_outcome(state: &State, track: &TrackId, label: &str) -> RenameOutcome {
    match audio_rename(state, track, label) {
        Some(cmd) => RenameOutcome::Send(cmd),
        None => RenameOutcome::Restore(track_label(state, track).unwrap_or_default()),
    }
}

/// The overflow's "Use for autocut": `AutocutSettings{track}`.
pub fn use_for_autocut(track: &TrackId) -> Command {
    Command::AutocutSettings {
        patch: AutocutPatch {
            track: Nullable::Set(track.clone()),
            ..AutocutPatch::default()
        },
    }
}

/// The gain field's text as a gain: −60 … 60 dB in 0.5 dB steps (C10).
pub fn parse_gain(text: &str) -> Result<f64, String> {
    let v = parse_number(text, &["dB", "db"])
        .ok_or_else(|| "Gain must be a number of dB".to_owned())?;
    if !(-60.0..=60.0).contains(&v) {
        return Err(format!("Gain must be between {MINUS}60 and 60 dB"));
    }
    Ok((v * 2.0).round() / 2.0)
}

/// The sync field's text as an offset: −5000 … 5000 ms in 10 ms steps.
pub fn parse_sync(text: &str) -> Result<i32, String> {
    let v = parse_number(text, &["ms"]).ok_or_else(|| "Sync must be a number of ms".to_owned())?;
    if !(-5000.0..=5000.0).contains(&v) {
        return Err(format!("Sync must be between {MINUS}5000 and 5000 ms"));
    }
    Ok(((v / 10.0).round() * 10.0) as i32)
}

/// A number typed by a person: spaces, a trailing unit, the true minus and
/// a leading `+` allowed.
pub fn parse_number(text: &str, units: &[&str]) -> Option<f64> {
    let mut t = text.trim();
    for unit in units {
        if let Some(rest) = t.strip_suffix(unit) {
            t = rest.trim_end();
            break;
        }
    }
    let t = t.replace(MINUS, "-");
    let t = t.strip_prefix('+').unwrap_or(&t);
    if t.is_empty() || t.contains(char::is_whitespace) {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

// -------------------------------------------------------------- inspector

/// The inspector of the selected layer (C12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspector {
    pub layer: LayerId,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub crop: [u32; 4],
    /// 0–100.
    pub opacity_percent: u32,
    pub fit: Fit,
    pub enabled: bool,
}

/// The inspector, or `None` with no layer selected (the panel then reads
/// "Select a layer on the canvas or in the list to edit its position, size
/// and crop.").
pub fn inspector(state: &State) -> Option<Inspector> {
    let id = state.ui.selected_layer.as_ref()?;
    let layer = canvas_scene(&state.project, &state.ui).layer(id)?;
    let Rect { x, y, w, h } = layer.rect;
    let Crop {
        top,
        right,
        bottom,
        left,
    } = layer.crop;
    Some(Inspector {
        layer: id.clone(),
        x,
        y,
        w,
        h,
        crop: [top, right, bottom, left],
        opacity_percent: percent(layer.opacity),
        fit: layer.fit,
        enabled: layers_editable(state),
    })
}

/// Whether the panel shows the inspector section at all: not for an empty
/// scene (BRIEF S01a: "the inspector is hidden"); with layers and none
/// selected it reads [`INSPECTOR_EMPTY`].
pub fn inspector_shown(state: &State) -> bool {
    !canvas_scene(&state.project, &state.ui).layers.is_empty()
}

/// The inspector section's caption: the selected layer, "Face · L2".
pub fn inspector_heading(state: &State) -> Option<String> {
    let id = state.ui.selected_layer.as_ref()?;
    let layer = canvas_scene(&state.project, &state.ui).layer(id)?;
    Some(format!("{} \u{b7} {}", layer.name, layer.id))
}

fn percent(opacity: f64) -> u32 {
    (opacity.clamp(0.0, 1.0) * 100.0).round() as u32
}

/// The inspector's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InspectorField {
    X,
    Y,
    W,
    H,
    CropTop,
    CropRight,
    CropBottom,
    CropLeft,
    Opacity,
}

impl InspectorField {
    /// Every field, in the inspector's order.
    pub const ALL: [InspectorField; 9] = [
        InspectorField::X,
        InspectorField::Y,
        InspectorField::W,
        InspectorField::H,
        InspectorField::CropTop,
        InspectorField::CropRight,
        InspectorField::CropBottom,
        InspectorField::CropLeft,
        InspectorField::Opacity,
    ];

    /// The caption over the field (S06).
    pub fn label(self) -> &'static str {
        match self {
            InspectorField::X => "Position X",
            InspectorField::Y => "Position Y",
            InspectorField::W => "Width",
            InspectorField::H => "Height",
            InspectorField::CropTop => "Crop top",
            InspectorField::CropRight => "Right",
            InspectorField::CropBottom => "Bottom",
            InspectorField::CropLeft => "Left",
            InspectorField::Opacity => "Opacity",
        }
    }

    /// The unit after the value: canvas px for the rect, source px for the
    /// crop, % for the opacity.
    pub fn unit(self) -> &'static str {
        match self {
            InspectorField::Opacity => "%",
            _ => "px",
        }
    }

    /// The step of the arrow keys and the scroll wheel.
    pub fn step(self) -> f64 {
        1.0
    }

    /// The field's value in `inspector` as its text ("1500", "100").
    pub fn text(self, inspector: &Inspector) -> String {
        let n: i64 = match self {
            InspectorField::X => i64::from(inspector.x),
            InspectorField::Y => i64::from(inspector.y),
            InspectorField::W => i64::from(inspector.w),
            InspectorField::H => i64::from(inspector.h),
            InspectorField::CropTop => i64::from(inspector.crop[0]),
            InspectorField::CropRight => i64::from(inspector.crop[1]),
            InspectorField::CropBottom => i64::from(inspector.crop[2]),
            InspectorField::CropLeft => i64::from(inspector.crop[3]),
            InspectorField::Opacity => i64::from(inspector.opacity_percent),
        };
        if n < 0 {
            format!("{MINUS}{}", n.unsigned_abs())
        } else {
            n.to_string()
        }
    }

    /// The name in helper texts ("X must be between …").
    fn noun(self) -> &'static str {
        match self {
            InspectorField::X => "X",
            InspectorField::Y => "Y",
            InspectorField::W => "Width",
            InspectorField::H => "Height",
            InspectorField::CropTop
            | InspectorField::CropRight
            | InspectorField::CropBottom
            | InspectorField::CropLeft => "Crop",
            InspectorField::Opacity => "Opacity",
        }
    }
}

/// What an inspector commit knows besides `State` (panel-local facts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommitOptions {
    /// The aspect lock (default on in the panel): a new width scales the
    /// height with it and the other way round.
    pub aspect_locked: bool,
    /// The source's size in px, for the crop check; `None`: the take
    /// stream's (Program), or unchecked (Live without a reported size).
    pub source_size: Option<(u32, u32)>,
}

/// An inspector field's text checked and turned into its `LayerSet`, or the
/// helper text ("Width must be at least 1", "X must be between −8192 and
/// 8192", "Crop leaves nothing to show"). [`inspector_commit_with`] without
/// the aspect lock; an unchanged value still makes its `LayerSet`.
pub fn inspector_commit(
    state: &State,
    field: InspectorField,
    text: &str,
) -> Result<Command, String> {
    let options = CommitOptions::default();
    match inspector_commit_with(state, field, text, &options)? {
        Some(cmd) => Ok(cmd),
        None => {
            let (target, layer) = selected_layer(state)?;
            let patch = unchanged_patch(field, &layer);
            Ok(Command::LayerSet {
                target,
                layer: layer.id,
                patch,
            })
        }
    }
}

/// One commit of `field` (Enter, blur): the one `LayerSet` it makes,
/// `Ok(None)` when the value doesn't change, or the helper text.
pub fn inspector_commit_with(
    state: &State,
    field: InspectorField,
    text: &str,
    options: &CommitOptions,
) -> Result<Option<Command>, String> {
    let (target, layer) = selected_layer(state)?;
    let rect = layer.rect;
    let mut patch = LayerPatch::default();
    match field {
        InspectorField::X | InspectorField::Y => {
            let v = whole(field, text)?;
            if v < -i64::from(MAX_POSITION) || v > i64::from(MAX_POSITION) {
                return Err(format!(
                    "{} must be between {MINUS}{MAX_POSITION} and {MAX_POSITION}",
                    field.noun()
                ));
            }
            let v = v as i32;
            let next = match field {
                InspectorField::X => Rect { x: v, ..rect },
                _ => Rect { y: v, ..rect },
            };
            if next != rect {
                patch.rect = Some(next);
            }
        }
        InspectorField::W | InspectorField::H => {
            let v = whole(field, text)?;
            if v < 1 {
                return Err(format!("{} must be at least 1", field.noun()));
            }
            if v > i64::from(MAX_SIZE) {
                return Err(format!("{} must be at most {MAX_SIZE}", field.noun()));
            }
            let v = v as u32;
            let (w, h) = match field {
                InspectorField::W => (v, locked_other(options, v, rect.h, rect.w)),
                _ => (locked_other(options, v, rect.w, rect.h), v),
            };
            let next = Rect { w, h, ..rect };
            if next != rect {
                patch.rect = Some(next);
            }
        }
        InspectorField::CropTop
        | InspectorField::CropRight
        | InspectorField::CropBottom
        | InspectorField::CropLeft => {
            let v = whole(field, text)?;
            if v < 0 {
                return Err("Crop can't be negative".to_owned());
            }
            let v = u32::try_from(v).map_err(|_| "Crop leaves nothing to show".to_owned())?;
            let mut crop = layer.crop;
            match field {
                InspectorField::CropTop => crop.top = v,
                InspectorField::CropRight => crop.right = v,
                InspectorField::CropBottom => crop.bottom = v,
                _ => crop.left = v,
            }
            let size = options
                .source_size
                .or_else(|| stream_size(&state.project, &target, &layer.id));
            if let Some((sw, sh)) = size
                && layout::place(sw, sh, crop, rect, layer.fit).is_none()
            {
                return Err("Crop leaves nothing to show".to_owned());
            }
            if crop != layer.crop {
                patch.crop = Some(crop);
            }
        }
        InspectorField::Opacity => {
            let v = whole(field, text)?;
            if !(0..=100).contains(&v) {
                return Err("Opacity must be between 0 and 100".to_owned());
            }
            if v as u32 != percent(layer.opacity) {
                patch.opacity = Some(v as f64 / 100.0);
            }
        }
    }
    if patch == LayerPatch::default() {
        return Ok(None);
    }
    Ok(Some(Command::LayerSet {
        target,
        layer: layer.id,
        patch,
    }))
}

/// The opacity slider's release: one `LayerSet{opacity}`, `None` when it
/// ends where it started.
pub fn opacity_commit(state: &State, percent_value: f64) -> Option<Command> {
    let text = format!("{}", percent_value.round().clamp(0.0, 100.0));
    inspector_commit_with(
        state,
        InspectorField::Opacity,
        &text,
        &CommitOptions::default(),
    )
    .ok()
    .flatten()
}

/// The Fit segment: `LayerSet{fit}`, `None` when it already fits so.
pub fn fit_commit(state: &State, fit: Fit) -> Option<Command> {
    let (target, layer) = selected_layer(state).ok()?;
    if layer.fit == fit {
        return None;
    }
    Some(Command::LayerSet {
        target,
        layer: layer.id,
        patch: LayerPatch {
            fit: Some(fit),
            ..LayerPatch::default()
        },
    })
}

/// The edit target and the selected layer there, or why not.
fn selected_layer(state: &State) -> Result<(SceneTarget, crate::model::Layer), String> {
    let target = edit_target(&state.project, &state.ui).ok_or_else(|| NO_EDIT_TARGET.to_owned())?;
    let id = state
        .ui
        .selected_layer
        .as_ref()
        .ok_or_else(|| "Select a layer first".to_owned())?;
    let layer = scene_of(&state.project, &target)
        .and_then(|s| s.layer(id))
        .ok_or_else(|| format!("Layer {id} isn't in this scene"))?;
    Ok((target, layer.clone()))
}

/// The patch of an unchanged commit (the frozen `inspector_commit`).
fn unchanged_patch(field: InspectorField, layer: &crate::model::Layer) -> LayerPatch {
    match field {
        InspectorField::X | InspectorField::Y | InspectorField::W | InspectorField::H => {
            LayerPatch {
                rect: Some(layer.rect),
                ..LayerPatch::default()
            }
        }
        InspectorField::Opacity => LayerPatch {
            opacity: Some(layer.opacity),
            ..LayerPatch::default()
        },
        _ => LayerPatch {
            crop: Some(layer.crop),
            ..LayerPatch::default()
        },
    }
}

/// The other side of a size under the aspect lock: `other × v / this`,
/// rounded, at least 1 (unchanged without the lock).
fn locked_other(options: &CommitOptions, v: u32, other: u32, this: u32) -> u32 {
    if !options.aspect_locked || this == 0 {
        return other;
    }
    let scaled = (f64::from(other) * f64::from(v) / f64::from(this)).round();
    scaled.clamp(1.0, f64::from(MAX_SIZE)) as u32
}

/// A whole number typed in `field`, or its helper text.
fn whole(field: InspectorField, text: &str) -> Result<i64, String> {
    let v = parse_number(text, &[field.unit()])
        .ok_or_else(|| format!("{} must be a whole number", field.noun()))?;
    if v.fract() != 0.0 || v.abs() > 1e12 {
        return Err(format!("{} must be a whole number", field.noun()));
    }
    Ok(v as i64)
}

/// The recorded size of `layer`'s stream in a take (`None` for the
/// project scene: the live size comes from the engine).
fn stream_size(project: &Project, target: &SceneTarget, layer: &LayerId) -> Option<(u32, u32)> {
    let SceneTarget::Take(take) = target else {
        return None;
    };
    let stream = project.take(take)?.stream(layer.as_str())?;
    Some((stream.width?, stream.height?))
}

// ------------------------------------------------------------ Add menus

/// The submenus of the Add menus (BRIEF §2.5: Screen ▸ …, Camera ▸ …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AddGroup {
    Screen,
    Camera,
    Mic,
    SystemAudio,
}

impl AddGroup {
    /// The submenu's label.
    pub fn label(self) -> &'static str {
        match self {
            AddGroup::Screen => "Screen",
            AddGroup::Camera => "Camera",
            AddGroup::Mic => "Mic",
            AddGroup::SystemAudio => "System audio",
        }
    }
}

/// One source of an Add menu (or of a track's Change source).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddItem {
    pub group: AddGroup,
    /// "DP-4 · GIGA-BYTE GS27QA", "Logitech C920", "Yeti Nano".
    pub label: String,
    /// The scene has it already (the item shows disabled).
    pub in_use: bool,
    pub command: Command,
}

/// Whether the Add menus work now: only in Live mode, where layers and
/// tracks go into the project scene (D-07), and when the gates let a
/// `LayerAdd` through (D-43). The reason is the disabled buttons' tooltip.
pub fn add_enabled(state: &State) -> Result<(), String> {
    if edit_target(&state.project, &state.ui) != Some(SceneTarget::Project) {
        return Err(ADD_IN_LIVE.to_owned());
    }
    let probe = Command::LayerAdd {
        target: SceneTarget::Project,
        source: VideoSourceArg::Screen {
            output: String::new(),
        },
        name: None,
        rect: None,
    };
    enabled(state, &probe)
}

/// The Add layer menu from what discovery found: every screen ("DP-4 ·
/// GIGA-BYTE GS27QA"), then every camera ("Logitech C920"); a source the
/// project scene already has is `in_use`.
pub fn add_layer_items(
    state: &State,
    screens: &[ScreenInfo],
    cameras: &[CameraInfo],
) -> Vec<AddItem> {
    let scene = &state.project.scene;
    let uses = |key: &str| scene.layers.iter().any(|l| source_key(&l.source) == key);
    let screens = screens.iter().map(|s| AddItem {
        group: AddGroup::Screen,
        label: if s.description.is_empty() || s.description == s.output {
            s.output.clone()
        } else {
            format!("{} \u{b7} {}", s.output, s.description)
        },
        in_use: uses(&s.output),
        command: Command::LayerAdd {
            target: SceneTarget::Project,
            source: VideoSourceArg::Screen {
                output: s.output.clone(),
            },
            name: None,
            rect: None,
        },
    });
    let cameras = cameras.iter().map(|c| AddItem {
        group: AddGroup::Camera,
        label: if c.label.is_empty() {
            c.device.clone()
        } else {
            c.label.clone()
        },
        in_use: uses(&c.device),
        command: Command::LayerAdd {
            target: SceneTarget::Project,
            source: VideoSourceArg::Camera {
                device: c.device.clone(),
            },
            name: None,
            rect: None,
        },
    });
    screens.chain(cameras).collect()
}

/// The Add audio track menu: every mic, then every system-audio monitor,
/// by their descriptions; a source the project scene has is `in_use`.
pub fn add_audio_items(
    state: &State,
    mics: &[AudioDeviceInfo],
    monitors: &[AudioDeviceInfo],
) -> Vec<AddItem> {
    let scene = &state.project.scene;
    let item = |group: AddGroup, role: AudioRole, d: &AudioDeviceInfo| AddItem {
        group,
        label: device_label(d),
        in_use: scene.audio.iter().any(|t| t.source == d.name),
        command: Command::AudioAdd {
            target: SceneTarget::Project,
            role,
            source: Some(d.name.clone()),
            label: None,
        },
    };
    mics.iter()
        .map(|d| item(AddGroup::Mic, AudioRole::Mic, d))
        .chain(
            monitors
                .iter()
                .map(|d| item(AddGroup::SystemAudio, AudioRole::System, d)),
        )
        .collect()
}

/// A track's Change source submenu (project scene only): the devices of
/// its role; its own source is `in_use`. Empty when the track isn't in the
/// project scene or the canvas edits a take.
pub fn source_items(
    state: &State,
    track: &TrackId,
    mics: &[AudioDeviceInfo],
    monitors: &[AudioDeviceInfo],
) -> Vec<AddItem> {
    if edit_target(&state.project, &state.ui) != Some(SceneTarget::Project) {
        return Vec::new();
    }
    let Some(current) = state.project.scene.track(track) else {
        return Vec::new();
    };
    let (group, devices) = match current.role {
        AudioRole::Mic => (AddGroup::Mic, mics),
        AudioRole::System => (AddGroup::SystemAudio, monitors),
    };
    devices
        .iter()
        .map(|d| AddItem {
            group,
            label: device_label(d),
            in_use: d.name == current.source,
            command: Command::AudioSet {
                target: SceneTarget::Project,
                track: track.clone(),
                patch: AudioPatch {
                    source: Some(d.name.clone()),
                    ..AudioPatch::default()
                },
            },
        })
        .collect()
}

fn device_label(d: &AudioDeviceInfo) -> String {
    if d.description.is_empty() {
        d.name.clone()
    } else {
        d.description.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use proptest::prelude::*;

    use super::*;
    use crate::ids::ClipId;
    use crate::state::{RecordMode, RecordPhase};
    use crate::testkit;
    use crate::view::LiveSourceView;

    /// S02: Live, nothing selected.
    fn s02() -> State {
        testkit::design_state()
    }

    /// S01a hides the inspector (an empty scene); S02 shows its empty line,
    /// S06 the selected layer's fields.
    #[test]
    fn the_inspector_hides_in_an_empty_scene() {
        let empty = testkit::state(testkit::ProjectBuilder::new().build());
        assert!(!inspector_shown(&empty));
        assert!(inspector(&empty).is_none());
        assert!(inspector_shown(&s02()) && inspector(&s02()).is_none());
        assert!(inspector_shown(&s06()) && inspector(&s06()).is_some());
    }

    /// S06: Program, c2d0 selected, L2 selected.
    fn s06() -> State {
        let mut state = testkit::design_state();
        state.ui.canvas_mode = CanvasMode::Program;
        state.ui.selection = vec!["c2d0".parse::<ClipId>().unwrap()];
        state.ui.selected_layer = Some(LayerId::numbered(2));
        state
    }

    /// S07: Program, nothing selected.
    fn s07() -> State {
        let mut state = testkit::design_state();
        state.ui.canvas_mode = CanvasMode::Program;
        state
    }

    /// S03/S04: recording.
    fn recording() -> State {
        let mut state = testkit::design_state();
        state.runtime.record = RecordPhase::Recording {
            take: TakeId::numbered(10),
            mode: RecordMode::Append,
            elapsed_ns: 0,
            waiters: vec![],
        };
        state
    }

    fn live(statuses: &[(&str, LiveSourceState)]) -> LiveSources {
        statuses
            .iter()
            .map(|(name, state)| {
                (
                    (*name).to_owned(),
                    LiveSourceView {
                        state: state.clone(),
                        size: None,
                    },
                )
            })
            .collect()
    }

    fn all_live() -> LiveSources {
        live(&[
            ("DP-4", LiveSourceState::Live),
            ("/dev/video0", LiveSourceState::Live),
            (testkit::DESIGN_MIC, LiveSourceState::Live),
            (testkit::DESIGN_MONITOR, LiveSourceState::Live),
        ])
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Panel {
        header: PanelHeader,
        layers: Vec<LayerRow>,
        audio: Vec<AudioRow>,
        inspector: Option<Inspector>,
        inspector_heading: Option<String>,
        add: Result<(), String>,
    }

    fn panel(state: &State, live: &LiveSources) -> Panel {
        Panel {
            header: header(state),
            layers: layer_rows(state, live),
            audio: audio_rows(state, live),
            inspector: inspector(state),
            inspector_heading: inspector_heading(state),
            add: add_enabled(state),
        }
    }

    #[test]
    fn the_design_scene_in_each_screen() {
        let mut s13 = s02();
        s13.ui.selected_layer = Some(LayerId::numbered(2));
        let screens: BTreeMap<&str, Panel> = [
            ("S02 live", panel(&s02(), &all_live())),
            (
                "S06 program, c2d0 and L2 selected",
                panel(&s06(), &BTreeMap::new()),
            ),
            (
                "S07 program, nothing selected",
                panel(&s07(), &BTreeMap::new()),
            ),
            ("S13 live, L2 selected", panel(&s13, &all_live())),
            ("S03 recording", panel(&recording(), &all_live())),
        ]
        .into_iter()
        .collect();
        insta::assert_json_snapshot!("design_scene", screens);
    }

    #[test]
    fn s13b_row_states() {
        // Hidden and locked (the CLI's `scene layer set`), the portal's
        // refusal, a dead camera; then take 5's missing L2 in Program.
        let mut state = s02();
        let l1 = state
            .project
            .scene
            .layer_mut(&LayerId::numbered(1))
            .unwrap();
        l1.visible = false;
        l1.locked = true;
        let refused = live(&[
            (
                "DP-4",
                LiveSourceState::Refused {
                    cancelled: false,
                    message: "denied".into(),
                },
            ),
            (
                "/dev/video0",
                LiveSourceState::Dead {
                    message: "the camera stopped sending frames".into(),
                },
            ),
            (testkit::DESIGN_MIC, LiveSourceState::Live),
        ]);
        let closed = live(&[(
            "DP-4",
            LiveSourceState::Refused {
                cancelled: true,
                message: "closed".into(),
            },
        )]);
        let mut missing = s06();
        let take5 = missing.project.takes.get_mut(&TakeId::numbered(5)).unwrap();
        take5
            .scene
            .layer_mut(&LayerId::numbered(2))
            .unwrap()
            .missing = true;
        let rows: BTreeMap<&str, (Vec<LayerRow>, Vec<AudioRow>)> = [
            (
                "hidden, locked, refused, dead, device missing",
                (layer_rows(&state, &refused), audio_rows(&state, &refused)),
            ),
            (
                "picker closed",
                (layer_rows(&state, &closed), audio_rows(&state, &closed)),
            ),
            (
                "missing in take 5",
                (
                    layer_rows(&missing, &BTreeMap::new()),
                    audio_rows(&missing, &BTreeMap::new()),
                ),
            ),
        ]
        .into_iter()
        .collect();
        insta::assert_json_snapshot!("s13b_states", rows);
    }

    #[test]
    fn nothing_reported_yet_is_not_a_missing_device() {
        let rows = audio_rows(&s02(), &BTreeMap::new());
        assert!(rows.iter().all(|r| r.device_missing.is_none()));
        // Program never reads the live statuses.
        let rows = audio_rows(&s06(), &live(&[("x", LiveSourceState::Live)]));
        assert!(rows.iter().all(|r| r.device_missing.is_none()));
    }

    #[test]
    fn the_autocut_tag_follows_the_setting() {
        let mut state = s02();
        state.project.autocut.track = Some(TrackId::numbered(2));
        let tags: Vec<bool> = audio_rows(&state, &BTreeMap::new())
            .iter()
            .map(|r| r.autocut)
            .collect();
        assert_eq!(tags, [false, true]);
        // Unset: the scene's first mic, which autocut then uses.
        state.project.autocut.track = None;
        let tags: Vec<bool> = audio_rows(&state, &BTreeMap::new())
            .iter()
            .map(|r| r.autocut)
            .collect();
        assert_eq!(tags, [true, false]);
    }

    #[test]
    fn source_lines() {
        let screen = |label: &str| VideoSource::Screen {
            output: "DP-4".into(),
            label: label.into(),
        };
        assert_eq!(
            source_line(&screen("GIGA-BYTE GS27QA")),
            "Screen · DP-4 · GIGA-BYTE GS27QA"
        );
        assert_eq!(source_line(&screen("")), "Screen · DP-4");
        assert_eq!(source_line(&screen("DP-4")), "Screen · DP-4");
        let camera = |label: &str| VideoSource::Camera {
            device: "/dev/video0".into(),
            label: label.into(),
        };
        assert_eq!(
            source_line(&camera("Logitech C920")),
            "Camera · Logitech C920"
        );
        assert_eq!(source_line(&camera("")), "Camera · /dev/video0");
    }

    #[test]
    fn the_layout_menu_sends_scene_applies() {
        let commands: Vec<serde_json::Value> = header(&s06())
            .layout_menu
            .iter()
            .map(|item| serde_json::json!({"label": item.label, "command": item.command().to_json()}))
            .collect();
        insta::assert_json_snapshot!("layout_menu", commands);
        assert!(header(&s02()).layout_menu.is_empty());
        assert_eq!(layout_enabled(&s06()), Ok(()));
        assert_eq!(
            layout_enabled(&recording()),
            Err("Not while recording".into())
        );
    }

    fn order(cmd: Option<Command>) -> Option<(SceneTarget, String, usize)> {
        match cmd? {
            Command::LayerOrder { target, layer, to } => Some((target, layer.to_string(), to)),
            other => panic!("not a LayerOrder: {other:?}"),
        }
    }

    #[test]
    fn reorder_index_math() {
        let state = s02();
        let (l1, l2) = (LayerId::numbered(1), LayerId::numbered(2));
        // Rows: L2 (row 0, z 1), L1 (row 1, z 0).
        assert_eq!(
            order(reorder(&state, &l1, 0)),
            Some((SceneTarget::Project, "L1".into(), 1))
        );
        assert_eq!(order(reorder(&state, &l1, 1)), None, "dropped above itself");
        assert_eq!(order(reorder(&state, &l1, 2)), None, "dropped below itself");
        assert_eq!(
            order(reorder(&state, &l2, 2)),
            Some((SceneTarget::Project, "L2".into(), 0))
        );
        assert_eq!(
            order(reorder(&state, &l2, 9)),
            Some((SceneTarget::Project, "L2".into(), 0)),
            "clamped"
        );
        assert_eq!(
            order(move_up(&state, &l1)),
            Some((SceneTarget::Project, "L1".into(), 1))
        );
        assert_eq!(order(move_up(&state, &l2)), None, "already on top");
        assert_eq!(
            order(move_down(&state, &l2)),
            Some((SceneTarget::Project, "L2".into(), 0))
        );
        assert_eq!(order(move_down(&state, &l1)), None, "already at the bottom");
        // A take layout edits the take; nothing selected in Program edits nothing.
        assert_eq!(
            order(reorder(&s06(), &l1, 0)),
            Some((SceneTarget::Take(TakeId::numbered(5)), "L1".into(), 1))
        );
        assert_eq!(order(reorder(&s07(), &l1, 0)), None);
        // Drop slots: the nearest row boundary.
        assert_eq!(drop_slot(-30.0, 44.0, 2), 0);
        assert_eq!(drop_slot(21.0, 44.0, 2), 0);
        assert_eq!(drop_slot(23.0, 44.0, 2), 1);
        assert_eq!(drop_slot(70.0, 44.0, 2), 2);
        assert_eq!(drop_slot(400.0, 44.0, 2), 2);
        assert_eq!(drop_slot(f64::NAN, 44.0, 2), 0);
    }

    /// A project scene of `n` screen layers L1 … Ln (L1 at the bottom).
    fn layers(n: usize) -> State {
        let mut builder = testkit::ProjectBuilder::new();
        for i in 1..=n {
            builder = builder.screen_layer(&format!("L{i}"), &format!("DP-{i}"));
        }
        testkit::state(builder.build())
    }

    /// The row order (topmost first) of the project scene.
    fn rows_of(state: &State) -> Vec<String> {
        layer_rows(state, &BTreeMap::new())
            .into_iter()
            .map(|r| r.layer.to_string())
            .collect()
    }

    proptest! {
        /// Dropping row `row` into gap `slot` and applying the command
        /// moves exactly that row there, the others keeping their order.
        #[test]
        fn a_drop_lands_where_the_line_was(n in 1usize..7, row in 0usize..7, slot in 0usize..8) {
            let state = layers(n);
            let row = row % n;
            let slot = slot % (n + 1);
            let before = rows_of(&state);
            let moved = before[row].clone();
            let mut expected = before.clone();
            expected.remove(row);
            let at = if slot > row { slot - 1 } else { slot };
            expected.insert(at, moved.clone());
            let layer: LayerId = moved.parse().unwrap();
            match reorder(&state, &layer, slot) {
                None => prop_assert_eq!(&expected, &before),
                Some(cmd) => {
                    let ctx = testkit::ctx(testkit::CREATED_AT, 1);
                    let next = crate::apply(&state, &ctx, cmd).unwrap().state;
                    prop_assert_eq!(rows_of(&next), expected);
                }
            }
        }
    }

    fn set(cmd: Result<Option<Command>, String>) -> Result<Option<serde_json::Value>, String> {
        cmd.map(|c| c.map(|c| c.to_json()))
    }

    #[test]
    fn one_command_per_inspector_commit() {
        let state = s06();
        let plain = CommitOptions::default();
        let locked = CommitOptions {
            aspect_locked: true,
            source_size: None,
        };
        let commit = |field, text: &str, options: &CommitOptions| {
            set(inspector_commit_with(&state, field, text, options))
        };
        let cases: Vec<(String, Result<Option<serde_json::Value>, String>)> = vec![
            ("X 1280".into(), commit(InspectorField::X, "1280", &plain)),
            (
                "X 1280 px".into(),
                commit(InspectorField::X, " 1280 px ", &plain),
            ),
            (
                "X −20 (true minus)".into(),
                commit(InspectorField::X, "\u{2212}20", &plain),
            ),
            (
                "X unchanged".into(),
                commit(InspectorField::X, "1500", &plain),
            ),
            ("X 9000".into(), commit(InspectorField::X, "9000", &plain)),
            ("X abc".into(), commit(InspectorField::X, "abc", &plain)),
            ("X 12.5".into(), commit(InspectorField::X, "12.5", &plain)),
            ("Y 700".into(), commit(InspectorField::Y, "700", &plain)),
            (
                "W 480 unlocked".into(),
                commit(InspectorField::W, "480", &plain),
            ),
            (
                "W 480 locked".into(),
                commit(InspectorField::W, "480", &locked),
            ),
            (
                "H 144 locked".into(),
                commit(InspectorField::H, "144", &locked),
            ),
            ("W 0".into(), commit(InspectorField::W, "0", &plain)),
            ("H 99999".into(), commit(InspectorField::H, "99999", &plain)),
            (
                "crop top 40".into(),
                commit(InspectorField::CropTop, "40", &plain),
            ),
            (
                "crop left 640 of 1280".into(),
                commit(InspectorField::CropLeft, "640", &plain),
            ),
            (
                "crop right 1280 of 1280".into(),
                commit(InspectorField::CropRight, "1280", &plain),
            ),
            (
                "crop bottom −1".into(),
                commit(InspectorField::CropBottom, "-1", &plain),
            ),
            (
                "opacity 50".into(),
                commit(InspectorField::Opacity, "50", &plain),
            ),
            (
                "opacity 100 unchanged".into(),
                commit(InspectorField::Opacity, "100 %", &plain),
            ),
            (
                "opacity 120".into(),
                commit(InspectorField::Opacity, "120", &plain),
            ),
        ];
        insta::assert_json_snapshot!("inspector_commits", cases);
        // Every commit is at most one command.
        assert_eq!(
            opacity_commit(&state, 42.4).map(|c| c.to_json()),
            Some(serde_json::json!({
                "cmd": "layerSet", "target": "take-0005", "layer": "L2",
                "patch": {"opacity": 0.42}
            }))
        );
        assert_eq!(opacity_commit(&state, 100.0), None);
        assert_eq!(fit_commit(&state, Fit::Cover), None, "L2 already covers");
        assert!(matches!(
            fit_commit(&state, Fit::Contain),
            Some(Command::LayerSet { .. })
        ));
        // The frozen form always makes its LayerSet.
        assert!(matches!(
            inspector_commit(&state, InspectorField::X, "1500"),
            Ok(Command::LayerSet { .. })
        ));
        // Live: the live size checks the crop when known.
        let mut live = s02();
        live.ui.selected_layer = Some(LayerId::numbered(1));
        let sized = CommitOptions {
            aspect_locked: false,
            source_size: Some((2560, 1440)),
        };
        assert_eq!(
            inspector_commit_with(&live, InspectorField::CropTop, "1440", &sized),
            Err("Crop leaves nothing to show".into())
        );
        assert!(
            inspector_commit_with(&live, InspectorField::CropTop, "1440", &plain)
                .unwrap()
                .is_some(),
            "unknown size: unchecked"
        );
        // No edit target: the reason.
        let mut idle = s07();
        idle.ui.selected_layer = Some(LayerId::numbered(2));
        assert_eq!(
            inspector_commit(&idle, InspectorField::X, "10"),
            Err(NO_EDIT_TARGET.into())
        );
    }

    #[test]
    fn inspector_fields_read_like_s06() {
        let inspector = inspector(&s06()).unwrap();
        let texts: Vec<String> = InspectorField::ALL
            .iter()
            .map(|f| format!("{} {} {}", f.label(), f.text(&inspector), f.unit()))
            .collect();
        assert_eq!(
            texts,
            [
                "Position X 1500 px",
                "Position Y 760 px",
                "Width 384 px",
                "Height 288 px",
                "Crop top 0 px",
                "Right 0 px",
                "Bottom 0 px",
                "Left 0 px",
                "Opacity 100 %"
            ]
        );
        assert_eq!(inspector_heading(&s06()).as_deref(), Some("Face · L2"));
        let negative = Inspector {
            x: -20,
            ..inspector
        };
        assert_eq!(InspectorField::X.text(&negative), "\u{2212}20");
    }

    #[test]
    fn row_commands() {
        let state = s02();
        let l2 = LayerId::numbered(2);
        let a1 = TrackId::numbered(1);
        let cmds: Vec<serde_json::Value> = [
            Some(select_layer(&l2)),
            layer_set(
                &state,
                &l2,
                LayerPatch {
                    visible: Some(false),
                    ..LayerPatch::default()
                },
            ),
            layer_set(
                &state,
                &l2,
                LayerPatch {
                    locked: Some(true),
                    ..LayerPatch::default()
                },
            ),
            layer_rename(&state, &l2, "  Face cam "),
            layer_remove(&state, &l2),
            audio_set(
                &state,
                &a1,
                AudioPatch {
                    muted: Some(true),
                    ..AudioPatch::default()
                },
            ),
            audio_rename(&state, &a1, "Desk mic"),
            audio_remove(&state, &a1),
            Some(use_for_autocut(&TrackId::numbered(2))),
            audio_set(
                &s06(),
                &a1,
                AudioPatch {
                    gain_db: Some(-3.5),
                    ..AudioPatch::default()
                },
            ),
        ]
        .into_iter()
        .map(|c| c.expect("a command").to_json())
        .collect();
        insta::assert_json_snapshot!("row_commands", cmds);
        assert_eq!(layer_rename(&state, &l2, "Face"), None, "unchanged");
        assert_eq!(layer_rename(&state, &l2, "   "), None, "blank");
        assert_eq!(audio_rename(&state, &a1, "Yeti Nano"), None);
        assert_eq!(layer_set(&s07(), &l2, LayerPatch::default()), None);
    }

    /// R46: a rename that sends nothing (blank, the same name trimmed,
    /// nothing to edit) puts the row's name back; one that sends is its
    /// command.
    #[test]
    fn a_rename_sends_or_puts_the_name_back() {
        let state = s02();
        let (l2, a1) = (LayerId::numbered(2), TrackId::numbered(1));
        let restore = |name: &str| RenameOutcome::Restore(name.to_owned());
        assert_eq!(layer_rename_outcome(&state, &l2, ""), restore("Face"));
        assert_eq!(layer_rename_outcome(&state, &l2, " Face "), restore("Face"));
        assert_eq!(
            layer_rename_outcome(&state, &l2, "Webcam"),
            RenameOutcome::Send(layer_rename(&state, &l2, "Webcam").unwrap())
        );
        assert_eq!(
            track_rename_outcome(&state, &a1, "  "),
            restore("Yeti Nano")
        );
        assert!(matches!(
            track_rename_outcome(&state, &a1, "Desk mic"),
            RenameOutcome::Send(Command::AudioSet { .. })
        ));
        // Program without a selection edits nothing: the shown name back.
        assert_eq!(layer_rename_outcome(&s07(), &l2, "Webcam"), restore("Face"));
        assert_eq!(layer_name(&state, &l2).as_deref(), Some("Face"));
        assert_eq!(track_label(&state, &a1).as_deref(), Some("Yeti Nano"));
    }

    #[test]
    fn gain_and_sync_fields() {
        assert_eq!(parse_gain("0.0 dB"), Ok(0.0));
        assert_eq!(parse_gain("\u{2212}6.0 dB"), Ok(-6.0));
        assert_eq!(parse_gain("-6.3"), Ok(-6.5));
        assert_eq!(parse_gain("+3"), Ok(3.0));
        assert_eq!(
            parse_gain("61"),
            Err("Gain must be between −60 and 60 dB".into())
        );
        assert_eq!(
            parse_gain("loud"),
            Err("Gain must be a number of dB".into())
        );
        assert_eq!(parse_sync("+0 ms"), Ok(0));
        assert_eq!(parse_sync("84"), Ok(80));
        assert_eq!(parse_sync("\u{2212}25 ms"), Ok(-30));
        assert_eq!(
            parse_sync("5001"),
            Err("Sync must be between −5000 and 5000 ms".into())
        );
        assert_eq!(parse_number("1 2", &[]), None);
        assert_eq!(parse_number("", &[]), None);
        assert_eq!(parse_number("inf", &[]), None);
    }

    #[test]
    fn add_menus_from_the_inventory() {
        let screens = [
            ScreenInfo {
                output: "DP-4".into(),
                description: "GIGA-BYTE GS27QA".into(),
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
                scale: 1.0,
            },
            ScreenInfo {
                output: "HDMI-A-1".into(),
                description: "Dell U2720Q".into(),
                x: 2560,
                y: 0,
                width: 3840,
                height: 2160,
                scale: 1.0,
            },
        ];
        let cameras = [CameraInfo {
            device: "/dev/video0".into(),
            label: "Logitech C920".into(),
            modes: vec![],
        }];
        let mics = [AudioDeviceInfo {
            name: testkit::DESIGN_MIC.into(),
            description: "Yeti Nano".into(),
            is_monitor: false,
        }];
        let monitors = [AudioDeviceInfo {
            name: testkit::DESIGN_MONITOR.into(),
            description: "Built-in Audio Analog Stereo".into(),
            is_monitor: true,
        }];
        let state = s02();
        let menus = serde_json::json!({
            "addLayer": add_layer_items(&state, &screens, &cameras),
            "addAudio": add_audio_items(&state, &mics, &monitors),
            "changeSourceA1": source_items(&state, &TrackId::numbered(1), &mics, &monitors),
            "changeSourceInATake": source_items(&s06(), &TrackId::numbered(1), &mics, &monitors),
        });
        insta::assert_json_snapshot!("add_menus", menus);
        assert_eq!(add_enabled(&s02()), Ok(()));
        assert_eq!(add_enabled(&s06()), Err(ADD_IN_LIVE.into()), "D-07");
        assert_eq!(add_enabled(&s07()), Err(ADD_IN_LIVE.into()));
        assert_eq!(add_enabled(&recording()), Err("Not while recording".into()));
    }
}
