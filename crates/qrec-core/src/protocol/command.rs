//! Commands: every change a person, the CLI, the GUI or an agent can ask for (A§4.4).
//!
//! JSON shape: `Command` is internally tagged by `"cmd"` (camelCase, unknown
//! fields rejected); nested enums are externally tagged
//! (`{"cmd":"clipMove","clips":["c7f3"],"to":{"by":-1}}`, unit variants as
//! strings such as `"end"`); `SceneTarget`, `CanvasSpec` and `TimeArg` are
//! strings; clearable patch fields are [`Nullable`] (absent = keep, `null` =
//! clear, value = set).
//!
//! Flags from the A§4.4 table are available as methods ([`Command::is_undoable`],
//! [`Command::allowed_while_recording`], …).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::CoreError;
use crate::ids::{ClipId, JobId, LayerId, TakeId, TrackId};
use crate::model::{AudioRole, Color, Crop, EncoderPref, Fit, Fps, Rect};
use crate::state::{CanvasMode, RecordPhase};
use crate::time::TimeArg;

/// A request to change the project, the history, the recording or the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "cmd",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Command {
    /// Creates `qrec.json` in an empty folder (`Invalid` if one exists).
    Init {
        canvas: Option<CanvasSpec>,
        name: Option<String>,
    },
    /// U, `!` when the fps changes and there are takes (clips are re-quantized).
    ProjectSet {
        name: Option<String>,
        canvas: Option<CanvasSpec>,
        background: Option<Color>,
        countdown_secs: Option<u32>,
        encoder: Option<EncoderPref>,
        #[serde(default, skip_serializing_if = "Nullable::is_keep")]
        vaapi_device: Nullable<String>,
    },
    /// U. Only `SceneTarget::Project` (a take's media is a fact).
    LayerAdd {
        target: SceneTarget,
        source: VideoSourceArg,
        name: Option<String>,
        rect: Option<Rect>,
    },
    /// U.
    LayerSet {
        target: SceneTarget,
        layer: LayerId,
        patch: LayerPatch,
    },
    /// U. Moves the layer to z-index `to` (0 = bottom).
    LayerOrder {
        target: SceneTarget,
        layer: LayerId,
        to: usize,
    },
    /// U. On a take: removes the layer from that snapshot only; the stream stays.
    LayerRemove { target: SceneTarget, layer: LayerId },
    /// U. Only `SceneTarget::Project`.
    AudioAdd {
        target: SceneTarget,
        role: AudioRole,
        source: Option<String>,
        label: Option<String>,
    },
    /// U.
    AudioSet {
        target: SceneTarget,
        track: TrackId,
        patch: AudioPatch,
    },
    /// U. Removing the `autocut.track` track also sets it to null.
    AudioRemove { target: SceneTarget, track: TrackId },
    /// U. Copies layout fields between scenes, matched by id.
    SceneApply { from: SceneTarget, to: SceneDest },
    /// U.
    AutocutSettings { patch: AutocutPatch },
    /// A.
    RecordStart {
        mode: RecordModeArg,
        countdown: Option<u32>,
        #[serde(default)]
        wait: bool,
    },
    /// A, R.
    RecordStop,
    /// A, R.
    RecordCancel,
    /// A, R.
    RecordToggle {
        #[serde(default)]
        wait: bool,
    },
    /// U, R (while finalizing).
    TakeDiscardLast,
    /// A, `!`, not undoable.
    TakePurge {
        #[serde(default)]
        takes: Vec<TakeId>,
    },
    /// U.
    ClipAdd { take: TakeId, at: InsertAt },
    /// U.
    ClipMove {
        #[serde(default)]
        clips: Vec<ClipId>,
        to: MoveTo,
    },
    /// U. Times are clip-relative unless `timeline`.
    ClipTrim {
        clip: ClipId,
        start: Option<TimeArg>,
        end: Option<TimeArg>,
        #[serde(default)]
        timeline: bool,
    },
    /// U. `clip: None` with `timeline` splits the clip under `at`.
    ClipSplit {
        clip: Option<ClipId>,
        at: TimeArg,
        #[serde(default)]
        timeline: bool,
    },
    /// U.
    ClipRemove {
        #[serde(default)]
        clips: Vec<ClipId>,
    },
    /// U, A.
    Autocut {
        scope: ClipScope,
        overrides: AutocutOverrides,
        #[serde(default)]
        dry_run: bool,
    },
    /// Steps default to 1 (`{"cmd":"undo"}`).
    Undo {
        #[serde(default = "one")]
        steps: u32,
    },
    /// Steps default to 1.
    Redo {
        #[serde(default = "one")]
        steps: u32,
    },
    /// `!`.
    HistoryRestore { seq: u64 },
    /// `!`.
    HistoryClear,
    /// A, `!` (hand-picked existing `out`). `out` is absolute.
    Export {
        #[serde(default)]
        format: ExportFormat,
        #[serde(default)]
        quality: Quality,
        codec: Option<VideoCodec>,
        out: Option<String>,
    },
    /// `job: None` cancels the running export.
    ExportCancel { job: Option<JobId> },
    /// A, `!`. `at` is a timeline frame; `out` is absolute.
    FrameSave {
        at: TimeArg,
        layer: Option<LayerId>,
        out: Option<String>,
    },
    /// R.
    Select { op: SelectOp },
    /// G, R.
    Seek { to: SeekTarget },
    /// G.
    Play,
    /// G, R.
    Pause,
    /// G.
    Shuttle { dir: ShuttleDir },
    /// R.
    SetCanvasMode { mode: CanvasMode },
    /// R.
    SetUi { patch: UiPatch },
}

fn one() -> u32 {
    1
}

impl Command {
    /// The `"cmd"` tag, e.g. `clipSplit`.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Init { .. } => "init",
            Command::ProjectSet { .. } => "projectSet",
            Command::LayerAdd { .. } => "layerAdd",
            Command::LayerSet { .. } => "layerSet",
            Command::LayerOrder { .. } => "layerOrder",
            Command::LayerRemove { .. } => "layerRemove",
            Command::AudioAdd { .. } => "audioAdd",
            Command::AudioSet { .. } => "audioSet",
            Command::AudioRemove { .. } => "audioRemove",
            Command::SceneApply { .. } => "sceneApply",
            Command::AutocutSettings { .. } => "autocutSettings",
            Command::RecordStart { .. } => "recordStart",
            Command::RecordStop => "recordStop",
            Command::RecordCancel => "recordCancel",
            Command::RecordToggle { .. } => "recordToggle",
            Command::TakeDiscardLast => "takeDiscardLast",
            Command::TakePurge { .. } => "takePurge",
            Command::ClipAdd { .. } => "clipAdd",
            Command::ClipMove { .. } => "clipMove",
            Command::ClipTrim { .. } => "clipTrim",
            Command::ClipSplit { .. } => "clipSplit",
            Command::ClipRemove { .. } => "clipRemove",
            Command::Autocut { .. } => "autocut",
            Command::Undo { .. } => "undo",
            Command::Redo { .. } => "redo",
            Command::HistoryRestore { .. } => "historyRestore",
            Command::HistoryClear => "historyClear",
            Command::Export { .. } => "export",
            Command::ExportCancel { .. } => "exportCancel",
            Command::FrameSave { .. } => "frameSave",
            Command::Select { .. } => "select",
            Command::Seek { .. } => "seek",
            Command::Play => "play",
            Command::Pause => "pause",
            Command::Shuttle { .. } => "shuttle",
            Command::SetCanvasMode { .. } => "setCanvasMode",
            Command::SetUi { .. } => "setUi",
        }
    }

    /// The command as JSON (the `cmd` of its history entry).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("commands always serialize")
    }

    /// `U`: a successful run appends one history entry (none when nothing changed).
    pub fn is_undoable(&self) -> bool {
        matches!(
            self,
            Command::ProjectSet { .. }
                | Command::LayerAdd { .. }
                | Command::LayerSet { .. }
                | Command::LayerOrder { .. }
                | Command::LayerRemove { .. }
                | Command::AudioAdd { .. }
                | Command::AudioSet { .. }
                | Command::AudioRemove { .. }
                | Command::SceneApply { .. }
                | Command::AutocutSettings { .. }
                | Command::TakeDiscardLast
                | Command::ClipAdd { .. }
                | Command::ClipMove { .. }
                | Command::ClipTrim { .. }
                | Command::ClipSplit { .. }
                | Command::ClipRemove { .. }
                | Command::Autocut { .. }
        )
    }

    /// `A`: may answer later through `Effect::Reply` (`Outcome::reply == None`).
    pub fn is_async(&self) -> bool {
        matches!(
            self,
            Command::RecordStart { .. }
                | Command::RecordStop
                | Command::RecordCancel
                | Command::RecordToggle { .. }
                | Command::TakeDiscardLast
                | Command::TakePurge { .. }
                | Command::Autocut { .. }
                | Command::Export { .. }
                | Command::FrameSave { .. }
        )
    }

    /// `!`: may fail with `confirmation_required` unless `Ctx::confirmed`.
    pub fn may_need_confirmation(&self) -> bool {
        matches!(
            self,
            Command::ProjectSet { .. }
                | Command::TakePurge { .. }
                | Command::HistoryRestore { .. }
                | Command::HistoryClear
                | Command::Export { .. }
                | Command::FrameSave { .. }
        )
    }

    /// `G`: only a GUI host can run it (elsewhere `Unavailable`).
    pub fn gui_only(&self) -> bool {
        matches!(
            self,
            Command::Seek { .. } | Command::Play | Command::Pause | Command::Shuttle { .. }
        )
    }

    /// A UI-state command: it changes only `State::ui` and
    /// `runtime.playing`, never the project, the history or a take. A
    /// recovery hold never parks these (D-47); parked commands resolve the
    /// selection and the playhead when they run.
    pub fn ui_only(&self) -> bool {
        matches!(
            self,
            Command::Select { .. }
                | Command::Seek { .. }
                | Command::SetUi { .. }
                | Command::SetCanvasMode { .. }
                | Command::Play
                | Command::Pause
                | Command::Shuttle { .. }
        )
    }

    /// `record start` or `record toggle`: may start a recording, which only
    /// a GUI or a headless host runs, never a CLI command's own (A§5.3, A§8.5).
    pub fn may_start_recording(&self) -> bool {
        matches!(
            self,
            Command::RecordStart { .. } | Command::RecordToggle { .. }
        )
    }

    /// May start a job (A§5.8): an export, a frame save, an autocut analysis
    /// or a purge. A CLI command's host takes none once its own command is
    /// done (A§8.5).
    pub fn may_start_job(&self) -> bool {
        matches!(
            self,
            Command::Export { .. }
                | Command::FrameSave { .. }
                | Command::Autocut { .. }
                | Command::TakePurge { .. }
        )
    }

    /// Its job stops when its requester goes away in a host that doesn't
    /// keep orphaned jobs ([`HostKind::keeps_orphaned_jobs`], A§5.8):
    /// exports, frame saves and autocut analysis. A purge always finishes.
    ///
    /// [`HostKind::keeps_orphaned_jobs`]: crate::state::HostKind::keeps_orphaned_jobs
    pub fn job_ends_with_requester(&self) -> bool {
        matches!(
            self,
            Command::Export { .. } | Command::FrameSave { .. } | Command::Autocut { .. }
        )
    }

    /// `R`: accepted while `phase` isn't `Idle` (A§5.3). `TakeDiscardLast`
    /// only while finalizing.
    pub fn allowed_while_recording(&self, phase: &RecordPhase) -> bool {
        match self {
            Command::RecordStop
            | Command::RecordCancel
            | Command::RecordToggle { .. }
            | Command::Select { .. }
            | Command::Seek { .. }
            | Command::Pause
            | Command::SetCanvasMode { .. }
            | Command::SetUi { .. } => true,
            Command::TakeDiscardLast => matches!(phase, RecordPhase::Finalizing { .. }),
            _ => false,
        }
    }

    /// A user edit of `qrec.json` or its history, refused while the file on
    /// disk is invalid (A§7.7). Recording (it saves facts after moving the bad
    /// file aside), `HistoryRestore` (the way out), outputs and UI commands
    /// are not edits.
    pub fn edits_project(&self) -> bool {
        match self {
            Command::Init { .. }
            | Command::ProjectSet { .. }
            | Command::LayerAdd { .. }
            | Command::LayerSet { .. }
            | Command::LayerOrder { .. }
            | Command::LayerRemove { .. }
            | Command::AudioAdd { .. }
            | Command::AudioSet { .. }
            | Command::AudioRemove { .. }
            | Command::SceneApply { .. }
            | Command::AutocutSettings { .. }
            | Command::TakeDiscardLast
            | Command::TakePurge { .. }
            | Command::ClipAdd { .. }
            | Command::ClipMove { .. }
            | Command::ClipTrim { .. }
            | Command::ClipSplit { .. }
            | Command::ClipRemove { .. }
            | Command::Undo { .. }
            | Command::Redo { .. }
            | Command::HistoryClear => true,
            Command::Autocut { dry_run, .. } => !dry_run,
            Command::RecordStart { .. }
            | Command::RecordStop
            | Command::RecordCancel
            | Command::RecordToggle { .. }
            | Command::HistoryRestore { .. }
            | Command::Export { .. }
            | Command::ExportCancel { .. }
            | Command::FrameSave { .. }
            | Command::Select { .. }
            | Command::Seek { .. }
            | Command::Play
            | Command::Pause
            | Command::Shuttle { .. }
            | Command::SetCanvasMode { .. }
            | Command::SetUi { .. } => false,
        }
    }

    /// Whether this command first makes a strictly empty folder a project
    /// (SPEC §4.4: "lazily on the first mutating command"): setting the
    /// project or its scene up, and recording. A command that works on what
    /// a project already holds (clips, takes, history, outputs, playback)
    /// is refused with `not_project` instead, and nothing is written.
    /// `Init` creates the project itself.
    pub fn creates_project(&self) -> bool {
        matches!(
            self,
            Command::ProjectSet { .. }
                | Command::LayerAdd { .. }
                | Command::LayerSet { .. }
                | Command::LayerOrder { .. }
                | Command::LayerRemove { .. }
                | Command::AudioAdd { .. }
                | Command::AudioSet { .. }
                | Command::AudioRemove { .. }
                | Command::AutocutSettings { .. }
                | Command::RecordStart { .. }
                | Command::RecordToggle { .. }
        )
    }
}

// ------------------------------------------------------------- Nullable

/// A clearable patch field: absent = [`Nullable::Keep`], `null` =
/// [`Nullable::Clear`], a value = [`Nullable::Set`]. Never `Option<Option<T>>`.
///
/// Fields must be declared with
/// `#[serde(default, skip_serializing_if = "Nullable::is_keep")]`;
/// serializing a `Keep` that wasn't skipped is an error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum Nullable<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

impl<T> Nullable<T> {
    pub fn is_keep(&self) -> bool {
        matches!(self, Nullable::Keep)
    }

    /// The field's new value: `Keep` leaves `current`, `Clear` empties it, `Set` replaces it.
    pub fn apply(self, current: Option<T>) -> Option<T> {
        match self {
            Nullable::Keep => current,
            Nullable::Clear => None,
            Nullable::Set(v) => Some(v),
        }
    }

    pub fn as_ref(&self) -> Nullable<&T> {
        match self {
            Nullable::Keep => Nullable::Keep,
            Nullable::Clear => Nullable::Clear,
            Nullable::Set(v) => Nullable::Set(v),
        }
    }
}

impl<T> From<Option<T>> for Nullable<T> {
    /// `None` clears, `Some` sets.
    fn from(value: Option<T>) -> Self {
        match value {
            None => Nullable::Clear,
            Some(v) => Nullable::Set(v),
        }
    }
}

impl<T: Serialize> Serialize for Nullable<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Nullable::Set(v) => s.serialize_some(v),
            Nullable::Clear => s.serialize_none(),
            Nullable::Keep => Err(serde::ser::Error::custom(
                "Nullable::Keep must be skipped: declare the field with \
                 #[serde(default, skip_serializing_if = \"Nullable::is_keep\")]",
            )),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Nullable<T> {
    /// Only called for present fields (absent ones use `Default`): `null` clears.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(d).map(Nullable::from)
    }
}

impl<T: JsonSchema> JsonSchema for Nullable<T> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        format!("Nullable_{}", T::schema_name()).into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <Option<T>>::json_schema(generator)
    }
}

// ------------------------------------------------------- string-typed args

/// Implements `Serialize`/`Deserialize` through `Display`/`FromStr` and a
/// string JSON schema.
macro_rules! string_serde {
    ($ty:ident, $description:literal, [$($example:literal),*]) => {
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(|e: CoreError| serde::de::Error::custom(e.message))
            }
        }

        impl JsonSchema for $ty {
            fn schema_name() -> Cow<'static, str> {
                stringify!($ty).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "description": $description,
                    "examples": [$($example),*]
                })
            }
        }
    };
}

/// The scene a command edits: `"project"` or a take id (`"take-0001"`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SceneTarget {
    Project,
    Take(TakeId),
}

impl fmt::Display for SceneTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SceneTarget::Project => f.write_str("project"),
            SceneTarget::Take(take) => write!(f, "{take}"),
        }
    }
}

impl FromStr for SceneTarget {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, CoreError> {
        if s == "project" {
            return Ok(SceneTarget::Project);
        }
        s.parse::<TakeId>().map(SceneTarget::Take).map_err(|_| {
            CoreError::usage(format!(
                "invalid scene {s:?} (expected \"project\" or a take id like take-0001)"
            ))
        })
    }
}

string_serde!(
    SceneTarget,
    "\"project\" or a take id",
    ["project", "take-0001"]
);

/// A canvas size and/or frame rate: `1920x1080@30`, `1280x720`, `@29.97`,
/// `@30000/1001`. Missing parts keep their current (or default) value.
/// Width and height are both present or both absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanvasSpec {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<Fps>,
}

impl CanvasSpec {
    pub fn new(width: u32, height: u32, fps: Fps) -> Self {
        Self {
            width: Some(width),
            height: Some(height),
            fps: Some(fps),
        }
    }
}

impl fmt::Display for CanvasSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let (Some(w), Some(h)) = (self.width, self.height) {
            write!(f, "{w}x{h}")?;
        }
        if let Some(fps) = self.fps {
            write!(f, "@{fps}")?;
        }
        Ok(())
    }
}

impl FromStr for CanvasSpec {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, CoreError> {
        let bad = || {
            CoreError::usage(format!("invalid canvas {s:?}")).with_hint(
                "use WIDTHxHEIGHT@FPS, e.g. 1920x1080@30, 1280x720, @29.97 or @30000/1001",
            )
        };
        let dim = |d: &str| -> Result<u32, CoreError> {
            if d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            d.parse().map_err(|_| bad())
        };
        let text = s.trim();
        let (size, fps) = match text.split_once('@') {
            Some((size, fps)) => (size, Some(fps)),
            None => (text, None),
        };
        let (width, height) = if size.is_empty() {
            (None, None)
        } else {
            let (w, h) = size.split_once(['x', 'X']).ok_or_else(bad)?;
            (Some(dim(w)?), Some(dim(h)?))
        };
        let fps = fps
            .map(|f| f.parse::<Fps>().map_err(|_| bad()))
            .transpose()?;
        if width.is_none() && fps.is_none() {
            return Err(bad());
        }
        Ok(Self { width, height, fps })
    }
}

string_serde!(
    CanvasSpec,
    "WIDTHxHEIGHT@FPS; either part may be omitted",
    ["1920x1080@30", "1280x720", "@29.97"]
);

// --------------------------------------------------------- nested types

/// Where `SceneApply` copies the layout to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SceneDest {
    Project,
    Takes(Vec<TakeId>),
    AllTakes,
}

/// The source of a new layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum VideoSourceArg {
    /// A monitor by bare output name (`DP-4`).
    Screen { output: String },
    /// A V4L2 device (`/dev/video0`).
    Camera { device: String },
}

/// Layer fields to change; absent fields are kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LayerPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Crop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<Fit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
}

/// Audio track fields to change; absent fields are kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_offset_ms: Option<i32>,
}

/// Project autocut settings to change; absent fields are kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutocutPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_run_after_take: Option<bool>,
    /// `null` = the take's first mic track.
    #[serde(default, skip_serializing_if = "Nullable::is_keep")]
    pub track: Nullable<TrackId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_silence_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_keep_ms: Option<u32>,
}

/// One-off autocut parameters; absent fields come from the project settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutocutOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_silence_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_keep_ms: Option<u32>,
}

/// Where a recording goes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RecordModeArg {
    #[default]
    Append,
    /// Record over these contiguous clips.
    Over(Vec<ClipId>),
    /// Record over the host's selection (`Unavailable` without one).
    OverSelection,
}

/// Where `ClipAdd` inserts the new clip.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InsertAt {
    #[default]
    End,
    Index(usize),
    After(ClipId),
    AfterSelection,
}

/// Where `ClipMove` moves the block of clips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MoveTo {
    /// An index in the list without the block.
    Index(usize),
    /// Shift the block's first index (clamped).
    By(i64),
    Before(ClipId),
    After(ClipId),
}

/// Which clips autocut analyzes. The CLI sends `All` when no flag is given.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ClipScope {
    #[default]
    All,
    Clips(Vec<ClipId>),
}

/// A selection change (UI state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SelectOp {
    Set(Vec<ClipId>),
    Toggle(ClipId),
    RangeTo(ClipId),
    Prev,
    Next,
    All,
    Clear,
}

/// Where `Seek` moves the playhead. `To` is a timeline position; `By` is relative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SeekTarget {
    To(TimeArg),
    By(TimeArg),
    EditPrev,
    EditNext,
    Start,
    End,
}

/// `j` / `k` / `l`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ShuttleDir {
    Reverse,
    Stop,
    Forward,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ExportFormat {
    #[default]
    Mp4,
    Webm,
}

impl ExportFormat {
    /// The file extension (and ffmpeg muxer name): `mp4` or `webm`.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Mp4 => "mp4",
            ExportFormat::Webm => "webm",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Quality {
    Draft,
    #[default]
    Standard,
    High,
}

/// mp4 ⇒ h264 only; webm ⇒ vp9 (default) or av1; anything else is `Usage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum VideoCodec {
    H264,
    Vp9,
    Av1,
}

/// UI fields to change; absent fields are kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiPatch {
    /// `null` = fit.
    #[serde(default, skip_serializing_if = "Nullable::is_keep")]
    pub zoom: Nullable<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_px: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin_open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapping: Option<bool>,
    #[serde(default, skip_serializing_if = "Nullable::is_keep")]
    pub solo: Nullable<TrackId>,
    /// The whole set of lanes muted for playback (D-08), replacing the
    /// stored one; ids that no scene has are dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_mute: Option<BTreeSet<TrackId>>,
    #[serde(default, skip_serializing_if = "Nullable::is_keep")]
    pub selected_layer: Nullable<LayerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panels: Option<BTreeMap<String, f64>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_specs() {
        let full: CanvasSpec = "1920x1080@30".parse().unwrap();
        assert_eq!(full, CanvasSpec::new(1920, 1080, Fps::new(30, 1)));
        assert_eq!(full.to_string(), "1920x1080@30");
        let fps_only: CanvasSpec = "@29.97".parse().unwrap();
        assert_eq!(
            fps_only,
            CanvasSpec {
                width: None,
                height: None,
                fps: Some(Fps::new(30000, 1001))
            }
        );
        assert_eq!(fps_only.to_string(), "@30000/1001");
        let size_only: CanvasSpec = "1280x720".parse().unwrap();
        assert_eq!(size_only.fps, None);
        assert_eq!(size_only.to_string(), "1280x720");
        for bad in [
            "",
            "@",
            "1920x",
            "x1080",
            "1920x1080@",
            "1920*1080",
            "1920x1080@abc",
            "-1x2",
            // Rates that used to overflow u32 while parsing (a panic).
            "1920x1080@500000000.5",
            "@4995004.995005",
        ] {
            assert!(bad.parse::<CanvasSpec>().is_err(), "{bad:?} should fail");
        }
        // Over IPC the same text is a decoding error the server answers
        // with `usage`, not a panic of the connection thread.
        for json in [
            r#"{"cmd":"projectSet","canvas":"1920x1080@500000000.5"}"#,
            r#"{"cmd":"init","canvas":"@4995004.995005"}"#,
        ] {
            assert!(serde_json::from_str::<Command>(json).is_err(), "{json}");
        }
    }

    #[test]
    fn scene_targets() {
        assert_eq!(
            "project".parse::<SceneTarget>().unwrap(),
            SceneTarget::Project
        );
        assert_eq!(
            "take-0001".parse::<SceneTarget>().unwrap(),
            SceneTarget::Take(TakeId::numbered(1))
        );
        assert!("take1".parse::<SceneTarget>().is_err());
        assert_eq!(
            serde_json::to_string(&SceneTarget::Take(TakeId::numbered(2))).unwrap(),
            "\"take-0002\""
        );
    }

    #[test]
    fn nullable_states() {
        let patch: UiPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(patch.zoom, Nullable::Keep);
        let patch: UiPatch = serde_json::from_str(r#"{"zoom":null}"#).unwrap();
        assert_eq!(patch.zoom, Nullable::Clear);
        let patch: UiPatch = serde_json::from_str(r#"{"zoom":2.5}"#).unwrap();
        assert_eq!(patch.zoom, Nullable::Set(2.5));
        assert_eq!(Nullable::Set(1).apply(Some(0)), Some(1));
        assert_eq!(Nullable::<i32>::Clear.apply(Some(0)), None);
        assert_eq!(Nullable::<i32>::Keep.apply(Some(0)), Some(0));
        assert!(serde_json::to_string(&Nullable::<i32>::Keep).is_err());
    }

    #[test]
    fn lane_mute_is_set_whole() {
        let patch: UiPatch = serde_json::from_str(r#"{"laneMute":["A2","A1"]}"#).unwrap();
        assert_eq!(
            patch.lane_mute,
            Some(BTreeSet::from([TrackId::numbered(1), TrackId::numbered(2)]))
        );
        let none: UiPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(none.lane_mute, None, "absent keeps the stored set");
        let cleared: UiPatch = serde_json::from_str(r#"{"laneMute":[]}"#).unwrap();
        assert_eq!(cleared.lane_mute, Some(BTreeSet::new()), "[] unmutes all");
        assert_eq!(
            serde_json::to_string(&patch).unwrap(),
            r#"{"laneMute":["A1","A2"]}"#
        );
    }

    #[test]
    fn ui_only_commands() {
        let ui = [
            Command::Select { op: SelectOp::Next },
            Command::Seek {
                to: SeekTarget::Start,
            },
            Command::SetUi {
                patch: UiPatch::default(),
            },
            Command::SetCanvasMode {
                mode: CanvasMode::Program,
            },
            Command::Play,
            Command::Pause,
            Command::Shuttle {
                dir: ShuttleDir::Stop,
            },
        ];
        for cmd in &ui {
            assert!(cmd.ui_only(), "{}", cmd.name());
            assert!(!cmd.is_undoable() && !cmd.edits_project(), "{}", cmd.name());
        }
        for cmd in [
            Command::ClipSplit {
                clip: None,
                at: TimeArg::frames(3),
                timeline: true,
            },
            Command::RecordToggle { wait: false },
            Command::Undo { steps: 1 },
            Command::ExportCancel { job: None },
        ] {
            assert!(!cmd.ui_only(), "{}", cmd.name());
        }
    }
}
