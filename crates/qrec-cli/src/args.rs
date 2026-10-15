//! The clap command tree: every command of SPEC §14 and the A§4.4 table,
//! plus the A§10 extensions.
//!
//! Ids, rects, enums and counts are checked here, so a typo fails early with
//! exit code 2. Times (`412f`, `13.7s`, `00:00:13:21`, `-2s`) and canvas specs
//! stay strings: the core parses them (`TimeArg`, `CanvasSpec`). Every
//! numeric or time option accepts a leading `-` (`--by -1`, `--gain-db -3`,
//! `--start -2s`, `--threshold-db -35`).

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum, value_parser};
use qrec_core::{AudioRole, ClipId, Color, Crop, EncoderPref, Fit, LayerId, Rect, TakeId, TrackId};

pub use crate::values::AutoOr;
use crate::values::{
    parse_crop, parse_device_or_auto, parse_opacity, parse_rect, parse_track_or_auto,
};

const LONG_ABOUT: &str = "\
Qrec records screens, cameras, a mic and system audio as layered takes on one
timeline, edits them (split, trim, move, autocut silences) and exports video.
The GUI and this CLI run the same commands; every edit is undoable.

PROJECT FOLDER
  Project commands use the folder given with --project DIR, else the
  current directory. It must contain qrec.json or be strictly empty (hidden
  files such as .git count); an empty folder becomes a project with `qrec
  init` or on the first change. Without --project, a current directory that
  is neither falls back to the single running Qrec host (so compositor key
  binds work from $HOME), except inside a project's subfolder and for `qrec
  init`, which only acts on its own folder. When no project is found the
  command exits 3 with
    \"Not my swamp: this folder isn't empty and has no qrec.json\"
  Commands that need no project work anywhere: --help, version, doctor,
  sources list, schema, keys, agent-guide, config, completions.

TIMES
  412f (frames), 13.7s, 00:00:13:21 (HH:MM:SS:FF), 00:00:13.700. Relative
  values (+1f, -2s) only where a help text says so. Clip times count from
  the clip's start unless --timeline is given.

OUTPUT
  Humans get short sentences and tables on stdout, notices on stderr. With
  --json every command prints exactly one JSON object on stdout:
  {\"ok\":true,\"data\":...} or {\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"}}.
  Streaming commands (export, record --wait) first print JSON lines such as
  {\"event\":\"progress\",...}. Changes return the new history seq and the ids
  they created.

DESTRUCTIVE ACTIONS
  Purging takes, overwriting an existing --out file, clearing or restoring
  history and changing the fps of a project with takes do nothing without
  --yes: they exit 4 with \"confirmation required: <what would be lost>\".

EXIT CODES
  0 ok, 1 error, 2 usage error, 3 not a project folder / folder not empty,
  4 confirmation required, 5 missing dependency, 6 busy (recording or
  exporting), 7 not found (bad id), 130 interrupted (Ctrl+C or SIGTERM).";

/// The `qrec` command line.
#[derive(Clone, Debug, PartialEq, Parser)]
#[command(
    name = "qrec",
    version,
    about = "Quick Wayland screen and camera recorder: press R, talk, press R",
    long_about = LONG_ABOUT,
    after_help = "Run `qrec agent-guide` for a concise guide for LLM agents, \
                  `qrec <command> --help` for details.",
    max_term_width = 100
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    // No command opens the GUI in the current folder.
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

const GLOBAL: &str = "Global options";

/// Flags accepted by every command, before or after it.
#[derive(Clone, Debug, Default, PartialEq, Args)]
pub struct Global {
    /// Project folder (default: the current directory; see PROJECT FOLDER in `qrec --help`).
    #[arg(long, global = true, value_name = "DIR", help_heading = GLOBAL)]
    pub project: Option<PathBuf>,
    /// Print exactly one JSON object ({"ok":…}); streaming commands print JSON lines first.
    #[arg(long, global = true, help_heading = GLOBAL)]
    pub json: bool,
    /// Confirm a destructive action (purge, overwrite, history clear/restore, fps change).
    #[arg(long, global = true, help_heading = GLOBAL)]
    pub yes: bool,
    /// Print only errors.
    #[arg(short, long, global = true, conflicts_with = "verbose", help_heading = GLOBAL)]
    pub quiet: bool,
    /// Print more detail on stderr (-vv for debug logs).
    #[arg(short, long, global = true, action = ArgAction::Count, help_heading = GLOBAL)]
    pub verbose: u8,
}

/// Top-level commands.
#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum Cmd {
    /// Create a project in the current folder (or --project DIR), which must be empty.
    ///
    /// Writes qrec.json, takes/ and .qrec/. Hidden files count as content, so
    /// the folder must be strictly empty. Other project commands also create
    /// the project on their first change.
    Init(InitArgs),
    /// Show a project summary; with --json the full state plus host and recording status.
    Status,
    /// Check dependencies, portal and devices; print the niri key-bind snippet.
    ///
    /// Lists every check with a hint for each failure. Exits 5 when a required
    /// dependency is missing.
    Doctor,
    /// List capture sources.
    #[command(subcommand)]
    Sources(SourcesCmd),
    /// Edit the scene: video layers and audio tracks.
    #[command(subcommand)]
    Scene(SceneCmd),
    /// Record takes: start, stop, toggle, cancel.
    #[command(subcommand)]
    Record(RecordCmd),
    /// Inspect takes (recordings), discard the last one, purge unused ones from disk.
    #[command(subcommand)]
    Take(TakeCmd),
    /// Edit timeline clips: list, add, move, trim, split, remove.
    #[command(subcommand)]
    Clip(ClipCmd),
    /// Remove silences from clips (every clip unless --clips); `autocut settings` changes the defaults.
    ///
    /// Finds silences on one audio track with ffmpeg silencedetect, keeps
    /// --padding-ms of audio around speech, and cuts the rest: each clip becomes
    /// several clips of the same take, in one undoable step. Kept fragments
    /// shorter than --min-keep-ms are dropped; a clip that is silent throughout
    /// is left alone. Flags override the project settings for this run only.
    Autocut(AutocutArgs),
    /// Undo the last N changes (default 1).
    ///
    /// Undo covers the timeline, the scenes, autocut, canvas and recording
    /// settings and the project name. Recorded media stays: undoing a recording
    /// removes its clip, and the take shows up in the bin.
    Undo(StepsArgs),
    /// Redo the last N undone changes (default 1).
    Redo(StepsArgs),
    /// List undo history entries; `history restore` and `history clear` change it.
    History(HistoryArgs),
    /// Export the timeline to a video file; `export cancel` stops a running export.
    ///
    /// Renders at the canvas size and fps, never overwriting a file unless
    /// --out names it and --yes is given. Clips render to cached segments, so
    /// re-exporting after a small edit only re-renders what changed. Humans see
    /// one progress line; --json streams {"event":"progress",...} lines, then
    /// the result. Editing can continue meanwhile, but `qrec record start` is
    /// refused (busy, exit 6) until the export ends or `qrec export cancel`
    /// stops it.
    Export(ExportArgs),
    /// Save frames as PNG images.
    #[command(subcommand)]
    Frame(FrameCmd),
    /// Move the GUI playhead (GUI host only).
    ///
    /// Without a running GUI it fails with code unavailable (exit 1).
    Seek(SeekArgs),
    /// Start timeline playback (GUI host only).
    Play,
    /// Pause playback (GUI host only).
    Pause,
    /// Change project settings: name, canvas, background, recording.
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Show or change user settings in ~/.config/qrec/config.toml.
    ///
    /// Without a subcommand, prints every setting. Keys: theme,
    /// canvas.{width,height,fps,background}, recording.countdown_secs,
    /// autocut.{auto_run_after_take,threshold_db,min_silence_ms,padding_ms,min_keep_ms},
    /// encoder.{preference,vaapi_device}, ui.{scale,reduced_motion,sounds}.
    Config(ConfigArgs),
    /// Print the effective keymap: defaults plus ~/.config/qrec/keys.toml overrides.
    Keys,
    /// Print the JSON Schema of qrec.json.
    Schema,
    /// Print a concise usage guide for LLM agents.
    AgentGuide,
    /// Print the Qrec version.
    Version,
    /// Print a shell completion script, e.g. `qrec completions zsh > _qrec`.
    Completions(CompletionsArgs),
    /// Internal: run a headless host (spawned by `qrec record start`).
    #[command(hide = true)]
    Host(HostArgs),
}

// ------------------------------------------------------------------ project

#[derive(Clone, Debug, PartialEq, Args)]
pub struct InitArgs {
    /// Canvas size and frame rate, e.g. 1920x1080@30 or 2560x1440@30000/1001 (default from config).
    #[arg(long, value_name = "WxH@FPS")]
    pub canvas: Option<String>,
    /// Project name (default: the folder name).
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum ProjectCmd {
    /// Change project settings (undoable). Changing the fps of a project with takes needs --yes.
    ///
    /// A new canvas fps re-quantizes every clip's bounds; on a project with
    /// takes it needs --yes, and without it exits 4 listing the affected clips.
    Set(ProjectSetArgs),
}

#[derive(Clone, Debug, PartialEq, Args)]
#[group(required = true, multiple = true)]
pub struct ProjectSetArgs {
    /// Project name (used in export and frame file names).
    #[arg(long)]
    pub name: Option<String>,
    /// Canvas size and frame rate, e.g. 1920x1080@30 or 1920x1080@30000/1001.
    #[arg(long, value_name = "WxH@FPS")]
    pub canvas: Option<String>,
    /// Canvas background color as #RRGGBB.
    #[arg(long, value_name = "#RRGGBB")]
    pub background: Option<Color>,
    /// Countdown before each recording, in seconds (0-10).
    #[arg(long, value_name = "SECS", allow_negative_numbers = true, value_parser = value_parser!(u32).range(0..=10))]
    pub countdown: Option<u32>,
    /// H.264 encoder for recording and MP4 export.
    #[arg(long, value_enum)]
    pub encoder: Option<EncoderArg>,
    /// VA-API render node, e.g. /dev/dri/renderD129, or `auto` for the first Intel/AMD node (never NVIDIA).
    #[arg(long, value_name = "NODE|auto", value_parser = parse_device_or_auto)]
    pub vaapi_device: Option<AutoOr<String>>,
}

// ------------------------------------------------------------------ sources

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum SourcesCmd {
    /// List screens, cameras, mics and system-audio monitors with the names other commands take.
    List,
}

// -------------------------------------------------------------------- scene

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum SceneCmd {
    /// Video layers: screens and cameras placed on the canvas.
    #[command(subcommand)]
    Layer(LayerCmd),
    /// Audio tracks: mics and system audio, each recorded to its own file.
    #[command(subcommand)]
    Audio(AudioCmd),
    /// Copy a layout between the project scene and take snapshots.
    ///
    /// Copies layout only (rect, crop, opacity, visibility, fit and z-order of
    /// layers; gain, mute and sync offset of tracks), matching layers and tracks
    /// by id. `--take take-0003` resets that take to the project scene;
    /// `--from-take take-0003` makes its layout the one new takes use;
    /// `--from-take take-0003 --to-all-takes` applies it to every take.
    Apply(SceneApplyArgs),
}

/// `--take`: which scene a command edits.
#[derive(Clone, Debug, Default, PartialEq, Args)]
pub struct SceneTargetArgs {
    /// Edit this take's scene snapshot instead of the project scene (used by new takes).
    #[arg(long, value_name = "TAKE")]
    pub take: Option<TakeId>,
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum LayerCmd {
    /// Add a layer to the project scene; new takes record it.
    #[command(subcommand)]
    Add(LayerAddCmd),
    /// Change a layer's placement, crop, opacity, visibility, fit, name or lock.
    ///
    /// Without --take this edits the project scene, which new takes copy. With
    /// --take it edits only that take's snapshot, which is what its clips show.
    Set(LayerSetArgs),
    /// Move a layer in the z-order (0 = bottom, the last layer is on top).
    Order(LayerOrderArgs),
    /// Remove a layer. With --take only that take's snapshot changes; its file stays.
    Remove(LayerRemoveArgs),
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum LayerAddCmd {
    /// A monitor, captured through the ScreenCast portal.
    Screen(ScreenLayerArgs),
    /// A V4L2 camera.
    Camera(CameraLayerArgs),
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct ScreenLayerArgs {
    /// Output name as `qrec sources list` shows it, e.g. DP-4.
    #[arg(long)]
    pub output: String,
    #[command(flatten)]
    pub layout: LayerAddLayout,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct CameraLayerArgs {
    /// Camera device as `qrec sources list` shows it, e.g. /dev/video0.
    #[arg(long)]
    pub device: String,
    #[command(flatten)]
    pub layout: LayerAddLayout,
}

#[derive(Clone, Debug, Default, PartialEq, Args)]
pub struct LayerAddLayout {
    /// Placement as X,Y,W,H in canvas pixels (default: full canvas for screens, bottom-right for cameras).
    #[arg(long, value_name = "X,Y,W,H", allow_hyphen_values = true, value_parser = parse_rect)]
    pub rect: Option<Rect>,
    /// Layer name (default: "Screen <output>" or "Camera").
    #[arg(long)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct LayerSetArgs {
    /// Layer id, e.g. L1.
    pub layer: LayerId,
    #[command(flatten)]
    pub scene: SceneTargetArgs,
    #[command(flatten)]
    pub patch: LayerPatchArgs,
}

/// What `scene layer set` changes; at least one is required.
#[derive(Clone, Debug, Default, PartialEq, Args)]
#[group(required = true, multiple = true)]
pub struct LayerPatchArgs {
    /// Placement as X,Y,W,H in canvas pixels; it may extend past the canvas.
    #[arg(long, value_name = "X,Y,W,H", allow_hyphen_values = true, value_parser = parse_rect)]
    pub rect: Option<Rect>,
    /// Source pixels to trim as TOP,RIGHT,BOTTOM,LEFT.
    #[arg(long, value_name = "T,R,B,L", allow_hyphen_values = true, value_parser = parse_crop)]
    pub crop: Option<Crop>,
    /// Opacity from 0.0 (invisible) to 1.0 (opaque).
    #[arg(long, allow_negative_numbers = true, value_parser = parse_opacity)]
    pub opacity: Option<f64>,
    /// Show (true) or hide (false) the layer; hidden layers are still recorded.
    #[arg(long, value_name = "BOOL")]
    pub visible: Option<bool>,
    /// How the source fills its rect.
    #[arg(long, value_enum)]
    pub fit: Option<FitArg>,
    /// New layer name.
    #[arg(long)]
    pub name: Option<String>,
    /// Lock (true) or unlock (false) the layer against canvas drags.
    #[arg(long, value_name = "BOOL")]
    pub locked: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct LayerOrderArgs {
    /// Layer id, e.g. L2.
    pub layer: LayerId,
    /// New z-order index: 0 = bottom.
    #[arg(long, value_name = "INDEX", allow_negative_numbers = true)]
    pub to: usize,
    #[command(flatten)]
    pub scene: SceneTargetArgs,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct LayerRemoveArgs {
    /// Layer id, e.g. L2.
    pub layer: LayerId,
    #[command(flatten)]
    pub scene: SceneTargetArgs,
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum AudioCmd {
    /// Add an audio track to the project scene; new takes record it.
    Add(AudioAddArgs),
    /// Change a track's gain, mute, sync offset, label or source.
    Set(AudioSetArgs),
    /// Remove a track. With --take only that take's snapshot changes; its file stays.
    Remove(AudioRemoveArgs),
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct AudioAddArgs {
    /// mic (a real input) or system (what the speakers play).
    #[arg(value_enum)]
    pub role: AudioRoleArg,
    /// Source name from `qrec sources list` (default: the default mic, or the default sink's monitor).
    #[arg(long, value_name = "NAME")]
    pub source: Option<String>,
    /// Display label (default: the device description).
    #[arg(long)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct AudioSetArgs {
    /// Track id, e.g. A1.
    pub track: TrackId,
    #[command(flatten)]
    pub scene: SceneTargetArgs,
    #[command(flatten)]
    pub patch: AudioPatchArgs,
}

/// What `scene audio set` changes; at least one is required.
#[derive(Clone, Debug, Default, PartialEq, Args)]
#[group(required = true, multiple = true)]
pub struct AudioPatchArgs {
    /// Gain in dB, -60 to 60, e.g. -3.
    #[arg(long, value_name = "DB", allow_negative_numbers = true)]
    pub gain_db: Option<f64>,
    /// Mute (true) or unmute (false) the track.
    #[arg(long, value_name = "BOOL")]
    pub mute: Option<bool>,
    /// Sync correction in ms, -5000 to 5000; positive plays the audio later.
    #[arg(long, value_name = "MS", allow_negative_numbers = true)]
    pub sync_offset_ms: Option<i32>,
    /// Display label.
    #[arg(long)]
    pub label: Option<String>,
    /// Source name from `qrec sources list`.
    #[arg(long, value_name = "NAME")]
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct AudioRemoveArgs {
    /// Track id, e.g. A2.
    pub track: TrackId,
    #[command(flatten)]
    pub scene: SceneTargetArgs,
}

/// Source and destination of `scene apply`. Layout fields only (rect, crop,
/// opacity, visibility, fit, z-order; gain, mute, sync offset), matched by id.
#[derive(Clone, Debug, Default, PartialEq, Args)]
#[group(required = true, multiple = true)]
pub struct SceneApplyArgs {
    /// Copy into these takes' snapshots, e.g. to reset a take to the project scene.
    #[arg(long, value_name = "TAKE", num_args = 1.., value_delimiter = ',', conflicts_with = "to_all_takes")]
    pub take: Vec<TakeId>,
    /// Copy into every take's snapshot.
    #[arg(long)]
    pub to_all_takes: bool,
    /// Copy from this take's snapshot instead of the project scene; alone, it updates the project scene.
    #[arg(long, value_name = "TAKE")]
    pub from_take: Option<TakeId>,
}

// ------------------------------------------------------------------- record

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum RecordCmd {
    /// Start recording a take; it is added to the timeline when it stops.
    ///
    /// Without a running Qrec host, a background recorder starts and this
    /// command returns once capture runs; `qrec record stop` finalizes the take.
    /// Each source is written to its own file in takes/take-NNNN/. --wait
    /// stays attached until the take is finalized.
    Start(RecordStartArgs),
    /// Stop recording and wait until the take is finalized and placed.
    Stop,
    /// Start recording, or stop if recording (for a key bind).
    Toggle(WaitArgs),
    /// Abort the countdown or recording; the take's files are kept and it goes to the bin.
    Cancel,
}

#[derive(Clone, Debug, Default, PartialEq, Args)]
pub struct RecordStartArgs {
    /// Record over these contiguous clips: the new take replaces them in place.
    #[arg(long, value_name = "CLIP", num_args = 1.., value_delimiter = ',', conflicts_with = "over_selection")]
    pub over: Vec<ClipId>,
    /// Record over the clips selected in the running GUI.
    #[arg(long)]
    pub over_selection: bool,
    /// Countdown in seconds, 0-10 (default: the project setting).
    #[arg(long, value_name = "SECS", allow_negative_numbers = true, value_parser = value_parser!(u32).range(0..=10))]
    pub countdown: Option<u32>,
    #[command(flatten)]
    pub wait: WaitArgs,
}

#[derive(Clone, Debug, Default, PartialEq, Args)]
pub struct WaitArgs {
    /// Keep running until the take is finalized, streaming progress (JSON lines with --json).
    #[arg(long)]
    pub wait: bool,
}

// --------------------------------------------------------------------- take

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum TakeCmd {
    /// List takes with status, duration and the clips that use them.
    List(TakeListArgs),
    /// Show one take: status, streams, scene snapshot and clips.
    Show(TakeShowArgs),
    /// Take the newest take's clips off the timeline; the take goes to the bin (undoable).
    ///
    /// Acts on the newest placed take whatever is selected; running it again
    /// doesn't reach older takes. Files are kept. After a record-over, the clips
    /// it replaced come back.
    DiscardLast,
    /// Delete unused takes' files from disk. Not undoable; needs --yes.
    ///
    /// Only takes that no clip uses can be purged. Their folders under takes/
    /// are deleted and the history entries that refer to them are dropped.
    /// Without --yes it exits 4 and lists the files and sizes it would delete.
    Purge(TakePurgeArgs),
}

#[derive(Clone, Debug, Default, PartialEq, Args)]
pub struct TakeListArgs {
    /// Only takes no clip uses (the bin), including broken and too-short ones.
    #[arg(long)]
    pub unused: bool,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct TakeShowArgs {
    /// Take id, e.g. take-0003.
    pub take: TakeId,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct TakePurgeArgs {
    /// Take ids, e.g. take-0003 take-0004.
    #[arg(required = true, num_args = 1.., value_delimiter = ',')]
    pub takes: Vec<TakeId>,
}

// --------------------------------------------------------------------- clip

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum ClipCmd {
    /// List timeline clips with take, in/out and timeline position.
    List,
    /// Put a whole take on the timeline as one clip (default: at the end).
    Add(ClipAddArgs),
    /// Move clips as one block, keeping their order.
    ///
    /// The timeline has no gaps: moving a block shifts its neighbors.
    /// `--by -1` moves one slot left; `--to 0` makes the block first.
    Move(ClipMoveArgs),
    /// Move a clip's start and/or end edge (up to the take's bounds).
    ///
    /// Times count from the clip's start: `--start 13f` drops the first 13
    /// frames, `--end 400f` ends the clip 400 frames after its start. Signed
    /// values move an edge: `--start -2s` brings back 2 s before the start,
    /// `--end +1f` extends the end by one frame. Edges can grow back out to the
    /// take's bounds, so trimmed media is never lost. With --timeline, T is a
    /// timeline position inside the clip.
    Trim(ClipTrimArgs),
    /// Split a clip in two at a time.
    ///
    /// `--at 13f` splits 13 frames into the clip; the left part keeps the clip
    /// id and the right part gets a new one. With --timeline, T is a timeline
    /// position and the clip under it is split. The rendered output doesn't
    /// change.
    Split(ClipSplitArgs),
    /// Remove clips from the timeline; their takes stay (unused ones show in the bin).
    Remove(ClipRemoveArgs),
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct ClipAddArgs {
    /// Take id, e.g. take-0003.
    pub take: TakeId,
    #[command(flatten)]
    pub at: InsertAtArgs,
}

/// Where `clip add` inserts; the end of the timeline when none is given.
#[derive(Clone, Debug, Default, PartialEq, Args)]
#[group(multiple = false)]
pub struct InsertAtArgs {
    /// Timeline index (0 = first).
    #[arg(long, value_name = "INDEX", allow_negative_numbers = true)]
    pub at: Option<usize>,
    /// Right after this clip.
    #[arg(long, value_name = "CLIP")]
    pub after: Option<ClipId>,
    /// Right after the clips selected in the running GUI.
    #[arg(long)]
    pub after_selection: bool,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct ClipMoveArgs {
    /// Clip ids; several clips move as one block in timeline order.
    #[arg(required = true, num_args = 1.., value_delimiter = ',')]
    pub clips: Vec<ClipId>,
    #[command(flatten)]
    pub to: MoveToArgs,
}

/// Where `clip move` puts the block; exactly one is required.
#[derive(Clone, Debug, Default, PartialEq, Args)]
#[group(required = true, multiple = false)]
pub struct MoveToArgs {
    /// Index in the timeline without the moved clips (0 = first).
    #[arg(long, value_name = "INDEX", allow_negative_numbers = true)]
    pub to: Option<usize>,
    /// Shift by N slots: negative moves left, positive right (clamped).
    #[arg(long, value_name = "N", allow_negative_numbers = true)]
    pub by: Option<i64>,
    /// Right before this clip.
    #[arg(long, value_name = "CLIP")]
    pub before: Option<ClipId>,
    /// Right after this clip.
    #[arg(long, value_name = "CLIP")]
    pub after: Option<ClipId>,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct ClipTrimArgs {
    /// Clip id, e.g. c7f3.
    pub clip: ClipId,
    #[command(flatten)]
    pub edges: TrimArgs,
    /// Read T as a timeline position instead of a time inside the clip.
    #[arg(long)]
    pub timeline: bool,
}

/// The edges `clip trim` moves; at least one is required.
#[derive(Clone, Debug, Default, PartialEq, Args)]
#[group(required = true, multiple = true)]
pub struct TrimArgs {
    /// New start: a time inside the clip (13f, 0.5s), or relative to the edge (+1f, -2s).
    #[arg(long, value_name = "T", allow_hyphen_values = true)]
    pub start: Option<String>,
    /// New end: a time inside the clip (400f, 12s), or relative to the edge (+1f, -2s).
    #[arg(long, value_name = "T", allow_hyphen_values = true)]
    pub end: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct ClipSplitArgs {
    /// Clip id, e.g. c7f3; omit it with --timeline to split the clip under T.
    #[arg(required_unless_present = "timeline")]
    pub clip: Option<ClipId>,
    /// Split point: a time inside the clip (13f, 0.5s, 00:00:01:02).
    #[arg(long, value_name = "T", allow_hyphen_values = true)]
    pub at: String,
    /// Read --at as a timeline position.
    #[arg(long)]
    pub timeline: bool,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct ClipRemoveArgs {
    /// Clip ids, e.g. c7f3 c8a1.
    #[arg(required = true, num_args = 1.., value_delimiter = ',')]
    pub clips: Vec<ClipId>,
}

// ------------------------------------------------------------------ autocut

#[derive(Clone, Debug, PartialEq, Args)]
pub struct AutocutArgs {
    #[command(subcommand)]
    pub command: Option<AutocutCmd>,
    #[command(flatten)]
    pub scope: AutocutScopeArgs,
    #[command(flatten)]
    pub params: AutocutParamArgs,
    /// Print the planned cuts without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum AutocutCmd {
    /// Change the project's autocut settings (undoable); they apply to `qrec autocut` and auto-run.
    Settings(AutocutSettingsArgs),
}

/// Which clips `autocut` processes: every clip when neither is given.
#[derive(Clone, Debug, Default, PartialEq, Args)]
#[group(multiple = false)]
pub struct AutocutScopeArgs {
    /// Only these clips.
    #[arg(long, value_name = "CLIP", num_args = 1.., value_delimiter = ',')]
    pub clips: Vec<ClipId>,
    /// Every clip on the timeline (the default).
    #[arg(long)]
    pub all: bool,
}

/// One-run overrides of the project's autocut settings.
#[derive(Clone, Debug, Default, PartialEq, Args)]
pub struct AutocutParamArgs {
    /// Audio track that drives detection, e.g. A1 (default: the setting, else the take's first mic).
    #[arg(long, value_name = "TRACK")]
    pub track: Option<TrackId>,
    /// Silence threshold in dB, -90 to 0, e.g. -35.
    #[arg(long, value_name = "DB", allow_negative_numbers = true)]
    pub threshold_db: Option<f64>,
    /// Only silences at least this long are removed.
    #[arg(long, value_name = "MS", allow_negative_numbers = true)]
    pub min_silence_ms: Option<u32>,
    /// Audio kept on each side of speech.
    #[arg(long, value_name = "MS", allow_negative_numbers = true)]
    pub padding_ms: Option<u32>,
    /// Kept fragments shorter than this are dropped.
    #[arg(long, value_name = "MS", allow_negative_numbers = true)]
    pub min_keep_ms: Option<u32>,
}

/// What `autocut settings` changes; at least one is required.
#[derive(Clone, Debug, Default, PartialEq, Args)]
#[group(required = true, multiple = true)]
pub struct AutocutSettingsArgs {
    /// Autocut each new take right after recording (true or false).
    #[arg(long, value_name = "BOOL")]
    pub auto_run: Option<bool>,
    /// Audio track that drives detection, e.g. A1, or `auto` for each take's first mic track.
    #[arg(long, value_name = "TRACK|auto", value_parser = parse_track_or_auto)]
    pub track: Option<AutoOr<TrackId>>,
    /// Silence threshold in dB, -90 to 0, e.g. -35.
    #[arg(long, value_name = "DB", allow_negative_numbers = true)]
    pub threshold_db: Option<f64>,
    /// Only silences at least this long are removed.
    #[arg(long, value_name = "MS", allow_negative_numbers = true)]
    pub min_silence_ms: Option<u32>,
    /// Audio kept on each side of speech.
    #[arg(long, value_name = "MS", allow_negative_numbers = true)]
    pub padding_ms: Option<u32>,
    /// Kept fragments shorter than this are dropped.
    #[arg(long, value_name = "MS", allow_negative_numbers = true)]
    pub min_keep_ms: Option<u32>,
}

// ------------------------------------------------------------------ history

#[derive(Clone, Debug, PartialEq, Args)]
pub struct StepsArgs {
    /// How many steps.
    #[arg(value_name = "N", default_value_t = 1, allow_negative_numbers = true, value_parser = value_parser!(u32).range(1..))]
    pub steps: u32,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct HistoryArgs {
    #[command(subcommand)]
    pub command: Option<HistoryCmd>,
    /// Show only the newest N entries (N ≥ 1).
    #[arg(long, value_name = "N", allow_negative_numbers = true, value_parser = value_parser!(u32).range(1..))]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum HistoryCmd {
    /// Undo or redo to entry SEQ; needs --yes. Also recovers a corrupt qrec.json.
    ///
    /// When qrec.json can't be read, this rebuilds it from the history (the
    /// broken file is kept as .qrec/qrec.corrupt-<time>.json). A valid hand
    /// edit of qrec.json is first recorded as an "External edit" entry, so a
    /// later redo brings it back. Without --yes nothing is written.
    Restore(HistoryRestoreArgs),
    /// Delete the undo/redo history (the project stays as it is); needs --yes.
    Clear,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct HistoryRestoreArgs {
    /// History entry seq, as `qrec history` lists it.
    #[arg(allow_negative_numbers = true)]
    pub seq: u64,
}

// ------------------------------------------------------------------- output

#[derive(Clone, Debug, PartialEq, Args)]
pub struct ExportArgs {
    #[command(subcommand)]
    pub command: Option<ExportCmd>,
    /// Container: mp4 (H.264 + AAC) or webm (VP9 or AV1 + Opus).
    #[arg(long, value_enum, default_value_t = FormatArg::Mp4)]
    pub format: FormatArg,
    /// Quality preset.
    #[arg(long, value_enum, default_value_t = QualityArg::Standard)]
    pub quality: QualityArg,
    /// Video codec: h264 for mp4; vp9 (default) or av1 for webm.
    #[arg(long, value_enum)]
    pub codec: Option<CodecArg>,
    /// Output file (default: exports/<name>-YYYYMMDD-HHMMSS.<ext>); an existing file needs --yes.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum ExportCmd {
    /// Cancel the running export.
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum FrameCmd {
    /// Save the composited canvas frame at a timeline time as PNG.
    ///
    /// Rendered through the export pipeline, so it matches the export exactly,
    /// at the canvas size. --layer saves one layer's raw source frame instead.
    Save(FrameSaveArgs),
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct FrameSaveArgs {
    /// Timeline time, e.g. 412f, 13.7s or 00:00:13:21.
    #[arg(long, value_name = "T", allow_hyphen_values = true)]
    pub at: String,
    /// Save this layer's raw source frame at native resolution instead, e.g. L2.
    #[arg(long, value_name = "LAYER")]
    pub layer: Option<LayerId>,
    /// Output file (default: frames/<name>-<take>-<HHMMSSFF>.png); an existing file needs --yes.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

// ----------------------------------------------------------------- playback

/// Where `seek` moves the playhead; exactly one is required.
#[derive(Clone, Debug, Default, PartialEq, Args)]
#[group(required = true, multiple = false)]
pub struct SeekArgs {
    /// A timeline time (412f, 13.7s, 00:00:13:21), `start` or `end`.
    #[arg(long, value_name = "T|start|end", allow_hyphen_values = true)]
    pub to: Option<String>,
    /// A relative time, e.g. -1f, +1s, -00:00:01:00.
    #[arg(long, value_name = "±T", allow_hyphen_values = true)]
    pub by: Option<String>,
    /// The previous or next edit point (clip boundary).
    #[arg(long, value_enum)]
    pub edit: Option<EditArg>,
}

// ------------------------------------------------------------------- config

#[derive(Clone, Debug, PartialEq, Args)]
pub struct ConfigArgs {
    // Without a subcommand, every setting is printed.
    #[command(subcommand)]
    pub command: Option<ConfigCmd>,
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum ConfigCmd {
    /// Print one setting, e.g. `qrec config get autocut.threshold_db`.
    Get(ConfigGetArgs),
    /// Change one setting, keeping the file's comments, e.g. `qrec config set theme swamp-day`.
    Set(ConfigSetArgs),
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct ConfigGetArgs {
    /// Dotted key: theme, canvas.fps, recording.countdown_secs, autocut.threshold_db, encoder.vaapi_device, ui.scale, ...
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct ConfigSetArgs {
    /// Dotted key, e.g. autocut.threshold_db.
    pub key: String,
    /// New value, e.g. -40.
    #[arg(allow_hyphen_values = true)]
    pub value: String,
}

// -------------------------------------------------------------------- misc

#[derive(Clone, Debug, PartialEq, Args)]
pub struct CompletionsArgs {
    /// Target shell.
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

#[derive(Clone, Debug, PartialEq, Args)]
pub struct HostArgs {
    /// Run without a window: capture and serve IPC until idle (the only host mode for now).
    #[arg(long, required = true)]
    pub headless: bool,
    /// Inherited fd that holds the project lock.
    #[arg(long, value_name = "FD", allow_negative_numbers = true, value_parser = value_parser!(i32).range(0..))]
    pub lock_fd: Option<i32>,
    /// Inherited fd to report readiness on: one JSON line, then close.
    #[arg(long, value_name = "FD", allow_negative_numbers = true, value_parser = value_parser!(i32).range(0..))]
    pub ready_fd: Option<i32>,
}

// -------------------------------------------------------------- value enums

/// How a (cropped) source fills its rect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum FitArg {
    /// Fit inside the rect, letterboxed.
    Contain,
    /// Fill the rect, cropping the overflow.
    Cover,
}

impl From<FitArg> for Fit {
    fn from(fit: FitArg) -> Fit {
        match fit {
            FitArg::Contain => Fit::Contain,
            FitArg::Cover => Fit::Cover,
        }
    }
}

/// What an audio track records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum AudioRoleArg {
    /// A microphone (alsa_input.* sources only).
    Mic,
    /// System audio: a sink's monitor.
    System,
}

impl From<AudioRoleArg> for AudioRole {
    fn from(role: AudioRoleArg) -> AudioRole {
        match role {
            AudioRoleArg::Mic => AudioRole::Mic,
            AudioRoleArg::System => AudioRole::System,
        }
    }
}

/// Which H.264 encoder records and exports MP4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum EncoderArg {
    /// VA-API when it works on the chosen node, else x264.
    Auto,
    /// VA-API hardware encoding.
    Vaapi,
    /// x264 software encoding.
    X264,
}

impl From<EncoderArg> for EncoderPref {
    fn from(encoder: EncoderArg) -> EncoderPref {
        match encoder {
            EncoderArg::Auto => EncoderPref::Auto,
            EncoderArg::Vaapi => EncoderPref::Vaapi,
            EncoderArg::X264 => EncoderPref::X264,
        }
    }
}

/// Export container.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum FormatArg {
    /// H.264 video and AAC audio.
    Mp4,
    /// VP9 or AV1 video and Opus audio.
    Webm,
}

/// Export quality preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum QualityArg {
    /// Fast, larger artifacts.
    Draft,
    /// The default balance.
    Standard,
    /// Slow, best quality.
    High,
}

/// Export video codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum CodecArg {
    /// For mp4.
    H264,
    /// For webm (the default there).
    Vp9,
    /// For webm (libsvtav1).
    Av1,
}

/// Direction for `seek --edit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum EditArg {
    /// The previous edit point.
    Prev,
    /// The next edit point.
    Next,
}
