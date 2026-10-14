# Qrec — Architecture

This document turns `SPEC.md` into concrete module boundaries, types and
protocols. `SPEC.md` is the product contract; this file is the engineering
contract. Where the two disagree, `SPEC.md` wins and this file gets fixed.
Section references like "§9" point at `SPEC.md`.

---

## 1. Crates

```
crates/
  qrec-core/   pure functional core: model, commands, planners, state machine
  qrec-shell/  imperative shell: Session, files, history store, lock, IPC,
               hosts, ffmpeg/ffprobe runners, GStreamer capture, portal, doctor
  qrec-cli/    clap definitions -> Request; output formatting; exit codes
  qrec-gui/    GTK views (blocked until the pen.dev designs are approved, §15.1)
  qrec/        the `qrec` binary: wires CLI, shell and GUI together
```

Dependency direction: `qrec -> qrec-cli -> qrec-shell -> qrec-core`, and
`qrec -> qrec-gui -> qrec-shell`. Nothing depends on `qrec-cli` or `qrec-gui`
except the binary. **`qrec-core` depends on no other workspace crate.**

### 1.1 Purity rules for `qrec-core` (enforced)

- No `std::fs`, `std::process`, `std::net`, `std::thread`, `std::env`.
- No clocks: no `SystemTime::now`, `Instant::now`, `chrono::Utc::now`
  (`chrono` is built **without** its `clock` feature, so it doesn't compile).
- No randomness: no `rand`, no `getrandom`. Ids derive from `Ctx::entropy`.
- Paths are plain `String`s, project-relative, `/`-separated.
- `crates/qrec-core/clippy.toml` bans the APIs above through
  `disallowed-methods` / `disallowed-types`. `cargo clippy` runs in CI.

Everything the core needs from the outside world arrives as data: in a
`Command`, in the `Ctx` (time, entropy, confirmation) or in an `Event`.

---

## 2. Project model (`qrec.json`, schema v1)

All JSON is `camelCase`. Rust types live in `qrec_core::model` and
`qrec_core::ids`. Field order below is serialization order. All model structs
use `#[serde(deny_unknown_fields)]` so a typo in a hand edit is reported
instead of silently ignored (§4.3 external edits).

```rust
// ids.rs: newtypes over String, #[serde(transparent)], Display, FromStr,
// Ord, Hash, JsonSchema. FromStr validates the shape.
pub struct LayerId(String); // "L1", "L2", …            ^L[1-9][0-9]*$
pub struct TrackId(String); // "A1", "A2", …            ^A[1-9][0-9]*$
pub struct TakeId(String);  // "take-0001" (≥4 digits)  ^take-[0-9]{4,}$
pub struct ClipId(String);  // "c7f3" ("c" + ≥3 hex)    ^c[0-9a-f]{3,}$
pub struct JobId(u64);      // runtime only
pub type Frame = u64;       // a frame index/count at canvas fps

// model.rs
pub struct Project {
    pub version: u32,                     // CURRENT_VERSION = 1
    pub name: String,                     // defaults to the folder name
    pub created_at: DateTime<Utc>,        // RFC 3339, second precision, "Z"
    pub canvas: Canvas,
    pub scene: Scene,                     // the project scene, used for NEW takes
    pub recording: RecordingSettings,
    pub autocut: AutocutSettings,
    pub takes: BTreeMap<TakeId, Take>,    // media pool, append-only, not undoable
    pub next_take_number: u32,            // never reused, not undoable
    pub timeline: Timeline,
}
pub struct Canvas { pub width: u32, pub height: u32, pub fps: Fps, pub background: Color }
pub struct Fps { pub num: u32, pub den: u32 }           // 30/1, 30000/1001, …
pub struct Color(String);                               // "#RRGGBB", validated
pub struct Scene { pub layers: Vec<Layer>, pub audio: Vec<AudioTrack> } // layers: last = top
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub opacity: f64,                     // 0.0..=1.0
    pub source: VideoSource,
    pub rect: Rect,                       // canvas px
    pub crop: Crop,                       // source px
    pub fit: Fit,
    #[serde(default, skip_serializing_if = "is_false")] pub locked: bool,  // UI lock (panel)
    #[serde(default, skip_serializing_if = "is_false")] pub missing: bool, // take scenes: source lost mid-take (§16)
}
#[serde(tag = "kind")]
pub enum VideoSource {
    Screen { output: String, label: String },   // "DP-4"
    Camera { device: String, label: String },   // "/dev/video0"
}
pub struct Rect { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }  // may extend past the canvas
pub struct Crop { pub top: u32, pub right: u32, pub bottom: u32, pub left: u32 }
pub enum Fit { Contain, Cover }                                   // "contain" | "cover"
pub struct AudioTrack {
    pub id: TrackId,
    pub role: AudioRole,                  // "mic" | "system" (labels only)
    pub source: String,                   // pulse source name
    pub label: String,
    pub gain_db: f64,
    pub muted: bool,
    pub sync_offset_ms: i32,              // + = audio is placed later
    #[serde(default, skip_serializing_if = "is_false")] pub missing: bool,
}
pub struct RecordingSettings {
    pub countdown_secs: u32,              // 0..=10, default 0
    pub encoder: EncoderPref,             // "auto" | "vaapi" | "x264"
    pub vaapi_device: Option<String>,     // "/dev/dri/renderD129"; null = auto (never NVIDIA)
}
pub struct AutocutSettings {
    pub auto_run_after_take: bool,
    pub track: Option<TrackId>,           // null = first mic track of the take
    pub threshold_db: f64,                // -35.0
    pub min_silence_ms: u32,              // 400
    pub padding_ms: u32,                  // 120
    pub min_keep_ms: u32,                 // 200
}
pub struct Take {
    pub status: TakeStatus,               // "recording" | "ok" | "broken" | "tooShort"
    pub recorded_at: DateTime<Utc>,
    pub duration_frames: Frame,           // 0 while recording; derived (see §2.2)
    pub scene: Scene,                     // snapshot, editable, undoable
    pub streams: Vec<Stream>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub error: Option<String>,
}
#[serde(tag = "kind")]
pub enum Stream {
    Video { id: String, file: String, width: u32, height: u32,
            start_offset_ns: i64, #[serde(default)] duration_ns: Option<i64> },
    Audio { id: String, file: String, sample_rate: u32, channels: u32,
            start_offset_ns: i64, #[serde(default)] duration_ns: Option<i64> },
}
pub struct Timeline { pub clips: Vec<Clip> }
pub struct Clip { pub id: ClipId, pub take: TakeId, #[serde(rename = "in")] pub in_: Frame, pub out: Frame }
```

Notes:

- A stream `id` equals the layer or track id it was recorded for (`"L1"`,
  `"A2"`). `file` is project-relative (`takes/take-0001/L1-screen-DP-4.mkv`).
- While a take is `recording`, its `streams` already list the planned files,
  with zeroed media info (`width`/`height`/`sampleRate`/`channels` = 0,
  `startOffsetNs` = 0, `durationNs` = null). Recovery (§5.3.7) relies on this.
- `durationNs` is an addition to the spec example: the probed duration of
  the stream. It's needed to recompute `durationFrames` when the canvas fps
  changes (§11), and to know when a layer runs out inside a take.

### 2.1 Defaults

| Field | Default |
| --- | --- |
| canvas | 1920×1080, 30/1 fps, `#000000` (from user config if set) |
| recording | countdown 0, encoder `auto`, vaapiDevice `null` (from user config) |
| autocut | autoRun false, track null, -35 dB, 400/120/200 ms (from user config) |
| new screen layer | name `Screen <output>`, rect = full canvas, fit `contain` |
| new camera layer | name `Camera`, 20 % of canvas width at 4:3, bottom-right, 2 % margin, fit `cover` |
| new audio track | label = device description (or `Mic` / `System audio`), gain 0, not muted, offset 0 |

### 2.2 Derived values and invariants

`durationFrames` of a take = `floor(min_i(startOffsetNs_i + durationNs_i) / frameNs)`
over its video streams that aren't `missing`, or over its audio streams when
it has no video. It is recomputed whenever the canvas fps changes (and after
an undo/redo that changes fps).

`qrec_core::validate::validate(&Project) -> Vec<Issue>` checks (each
`Issue { severity: Error|Warning, path: JSON Pointer, message }`):

- `version == CURRENT_VERSION`; `nextTakeNumber` > every take number.
- Clips: unique ids; take exists and is `ok`; `0 ≤ in < out ≤ durationFrames`.
- Scenes: unique layer/track ids; id shapes; `0 ≤ opacity ≤ 1`; `w,h ≥ 1`.
- Take scenes: every layer/track id has a stream, otherwise a **Warning**
  ("missing media"), not an error.
- Canvas: `16 ≤ w,h ≤ 8192`, even sizes, `fps.num, fps.den > 0`, fps ≤ 240.
- `countdownSecs ≤ 10`; autocut numbers in sane ranges; `autocut.track`
  exists in the project scene when set.

Only `Error`s make a project invalid. The timeline is gapless by
construction (it's an ordered list; positions are prefix sums).

---

## 3. Runtime state

```rust
pub struct State {
    pub project: Project,
    pub ui: UiState,        // persisted in .qrec/ui.json; never undoable
    pub history: History,  // mirror of .qrec/history.*; the core decides, the shell persists
    pub runtime: Runtime,  // never persisted
}

pub struct UiState {                       // camelCase JSON, all fields #[serde(default)]
    pub selection: Vec<ClipId>,            // kept in timeline order
    pub playhead: Frame,                   // timeline frame
    pub canvas_mode: CanvasMode,           // "live" | "program"
    pub selected_layer: Option<LayerId>,
    pub snapping: bool,                    // default true
    pub bin_open: bool,
    pub zoom: Option<f64>,                 // px per frame; None = fit
    pub scroll_px: f64,
    pub solo: Option<TrackId>,             // preview-only lane solo
    pub panels: BTreeMap<String, f64>,     // panel sizes
}

pub struct Runtime {
    pub host: HostKind,                    // Gui | Headless | InProcess
    pub record: RecordPhase,
    pub jobs: BTreeMap<JobId, Job>,        // running async work (see §5.4)
    pub silence: BTreeMap<SilenceKey, Vec<Interval>>, // analysis results known to the core
    pub capabilities: Capabilities,        // filled by the shell (Event::Capabilities)
    pub next_job: u64,
    pub playing: bool,
}

pub enum RecordPhase {
    Idle,
    Countdown { remaining: u32, mode: RecordMode, timer: TimerId },
    Starting { take: TakeId, mode: RecordMode },        // waiting for CaptureStarted (health check)
    Recording { take: TakeId, mode: RecordMode, since: DateTime<Utc> },
    Finalizing { take: TakeId, mode: RecordMode, cancelled: bool },
}
pub enum RecordMode { Append, Over { clips: Vec<ClipId> } }   // Over: contiguous, timeline order

pub struct Capabilities {                 // all default false/None until detected
    pub ffmpeg_encoders: BTreeSet<String>, // "libx264", "h264_vaapi", "libvpx-vp9", "libsvtav1", "aac", "libopus"
    pub vaapi_device: Option<String>,      // resolved, usable render node (never NVIDIA)
    pub vaapi_h264_export: bool,           // h264_vaapi verified on that node
    pub gst_va_h264: bool,                 // a va H.264 GStreamer encoder exists for that node
    pub cores: u32,
}
```

`State::new(project) -> State` builds defaults. The CLI never needs `ui`
except for `--over-selection` and default autocut scope (selection).

---

## 4. The dispatch protocol

### 4.1 Entry points (pure)

```rust
pub struct Ctx {
    pub now: DateTime<Utc>,   // injected wall clock
    pub entropy: u64,         // fresh per dispatch; seeds IdGen
    pub confirmed: bool,      // --yes / modal confirmed
    pub req: Option<ReqId>,   // the request being served, if any
}

pub struct Outcome {
    pub state: State,
    pub effects: Vec<Effect>,
    pub reply: Option<Reply>, // Some = the request is complete now; None = it completes later via Effect::Reply
}

pub fn apply(state: &State, ctx: &Ctx, cmd: Command) -> Result<Outcome, CoreError>;
pub fn on_event(state: &State, ctx: &Ctx, ev: Event) -> Outcome;
pub fn query(state: &State, q: &Query) -> Result<serde_json::Value, CoreError>;
```

`apply` returns `Err` for anything that should not change state (bad id,
busy, needs confirmation, usage). It never returns a partially applied
state. `on_event` can't fail; failures become state (`broken` takes,
failed jobs) plus `Notify` toasts and `Reply` errors.

A command that finishes synchronously returns `reply: Some(..)`. A command
that needs the shell (record, autocut analysis, export, frame save, purge)
returns `reply: None`; the core later emits `Effect::Reply { req, reply }`
from `on_event`, and may emit `Effect::Progress { req, .. }` before that.
`Ctx::req` is stored in the job or record phase so the reply finds its
requester.

```rust
pub struct Reply(pub Result<CmdResult, CoreError>);
pub struct CmdResult {
    pub seq: Option<u64>,          // new history seq, for undoable changes
    pub created: Vec<String>,      // ids created (clips, layers, tracks, takes)
    pub removed: Vec<String>,      // ids removed (clips, layers, tracks)
    pub message: String,           // one human line ("Split c7f3 at 00:00:13:20 → c9b2")
    pub data: serde_json::Value,   // command-specific payload (dry-run plan, export path, …)
}
```

### 4.2 Ids, time and confirmation

- `IdGen::new(ctx.entropy)` produces clip ids `"c" + 3 hex` derived from
  `sha256(entropy ‖ counter)`, skipping ids already used anywhere in the
  project (timeline, history isn't checked). After 64 collisions it
  widens to 4, then 5 hex digits. Same entropy ⇒ same ids (tests rely on this).
- Layer ids: `L{n}` with `n = 1 + max number used by the project scene or any
  take scene`; same for `A{n}`. Ids are never recycled while a take uses them.
- Destructive commands check `ctx.confirmed`; without it they return
  `CoreError { code: ConfirmationRequired, message: "confirmation required: <what is lost>" }`.

### 4.3 Commands

`Command` is `#[serde(tag = "cmd", rename_all = "camelCase", rename_all_fields = "camelCase")]`,
so IPC lines read like `{"cmd":"clipSplit","clip":"c7f3","at":"13f"}`. Time
arguments serialize as their string form (§6.1). `U` = undoable (creates a
history entry), `A` = async (reply comes later), `!` = needs confirmation.

| Command | Fields | Flags | CLI |
| --- | --- | --- | --- |
| `Init` | `canvas: Option<CanvasSpec>`, `name: Option<String>` | | `qrec init [--canvas 1920x1080@30]` |
| `ProjectSet` | `name`, `canvas: Option<CanvasSpec>`, `background`, `countdownSecs`, `encoder`, `vaapiDevice: Option<Option<String>>` | U, `!` if fps changes with takes | `qrec project set …` (extension, §12) |
| `LayerAdd` | `target: SceneTarget`, `source: VideoSource`, `name`, `rect` | U | `qrec scene layer add screen\|camera …` |
| `LayerSet` | `target`, `layer`, `patch: LayerPatch` | U | `qrec scene layer set <id> …` |
| `LayerOrder` | `target`, `layer`, `to: usize` | U | `qrec scene layer order <id> --to N` |
| `LayerRemove` | `target`, `layer` | U | `qrec scene layer remove <id>` |
| `AudioAdd` | `target`, `role`, `source: String`, `label` | U | `qrec scene audio add mic\|system [--source]` |
| `AudioSet` | `target`, `track`, `patch: AudioPatch` | U | `qrec scene audio set <id> …` |
| `AudioRemove` | `target`, `track` | U | `qrec scene audio remove <id>` |
| `SceneApply` | `from: SceneTarget`, `to: SceneDest` | U | `qrec scene apply …` |
| `AutocutSettings` | `patch: AutocutPatch` | U | `qrec autocut settings …` (extension) |
| `RecordStart` | `mode: RecordModeArg`, `countdown: Option<u32>` | A | `qrec record start [--over <clip>… \| --over-selection] [--countdown N]` |
| `RecordStop` / `RecordToggle` / `RecordCancel` | – | A | `qrec record stop\|toggle\|cancel` |
| `TakeDiscardLast` | – | U | `qrec take discard-last` |
| `TakePurge` | `takes: Vec<TakeId>` | A, `!`, not undoable | `qrec take purge <take>… --yes` |
| `ClipAdd` | `take`, `at: InsertAt` | U | `qrec clip add <take> [--at N \| --after <clip>]` |
| `ClipMove` | `clips: Vec<ClipId>`, `to: MoveTo` | U | `qrec clip move <clip>… (--to N \| --by ±N \| --before C \| --after C)` |
| `ClipTrim` | `clip`, `start: Option<TimeArg>`, `end: Option<TimeArg>`, `timeline: bool` | U | `qrec clip trim <clip> [--start T] [--end T] [--timeline]` |
| `ClipSplit` | `clip: Option<ClipId>`, `at: TimeArg`, `timeline: bool` | U | `qrec clip split <clip> --at T` / `--timeline --at T` |
| `ClipRemove` | `clips: Vec<ClipId>` | U | `qrec clip remove <clip>…` |
| `Autocut` | `scope: ClipScope`, `overrides: AutocutOverrides`, `dryRun: bool` | U, A | `qrec autocut …` |
| `Undo` / `Redo` | `steps: u32` | | `qrec undo [N]` / `qrec redo [N]` |
| `HistoryRestore` | `seq: u64` | `!` | `qrec history restore <seq> --yes` |
| `HistoryClear` | – | `!` | `qrec history clear --yes` |
| `Export` | `format`, `quality`, `codec: Option<VideoCodec>`, `out: Option<String>` | A | `qrec export …` |
| `ExportCancel` | `job: Option<JobId>` | | `qrec export cancel` (extension) |
| `FrameSave` | `at: TimeArg`, `layer: Option<LayerId>`, `out: Option<String>` | A | `qrec frame save --at T [--layer L2] [--out P]` |
| `Select` | `op: SelectOp` | UI | — (UI state) |
| `Seek` | `to: SeekTarget` | UI, GUI host only | `qrec seek --to T \| --by ±T \| --edit prev\|next` |
| `Play` / `Pause` | – | UI, GUI host only | `qrec play` / `qrec pause` |
| `SetCanvasMode` / `SetUi` | `mode` / `patch: UiPatch` | UI | — |

Supporting types (all serde `camelCase`):

```rust
pub enum SceneTarget { Project, Take(TakeId) }        // serialized "project" | "take-0001"
pub enum SceneDest { Project, Takes(Vec<TakeId>), AllTakes }
pub struct CanvasSpec { pub width: u32, pub height: u32, pub fps: Fps } // "1920x1080@30", "@30000/1001", "@29.97"
pub struct LayerPatch { pub name, visible, opacity, rect, crop, fit, locked: Option<…> }
pub struct AudioPatch { pub label, source, gain_db, muted, sync_offset_ms: Option<…> }
pub struct AutocutPatch { pub auto_run_after_take, track: Option<Option<TrackId>>, threshold_db, min_silence_ms, padding_ms, min_keep_ms }
pub struct AutocutOverrides { pub track, threshold_db, min_silence_ms, padding_ms, min_keep_ms: Option<…> }
pub enum RecordModeArg { Append, Over { clips: Vec<ClipId> }, OverSelection }
pub enum InsertAt { End, Index(usize), After(ClipId), AfterSelection }
pub enum MoveTo { Index(usize), By(i64), Before(ClipId), After(ClipId) }
pub enum ClipScope { Default, All, Clips(Vec<ClipId>) }  // Default = selection if any, else all
pub enum SelectOp { Set(Vec<ClipId>), Toggle(ClipId), RangeTo(ClipId), Prev, Next, All, Clear }
pub enum SeekTarget { To(TimeArg), By(TimeArg), EditPrev, EditNext, Start, End }
pub enum ExportFormat { Mp4, Webm }
pub enum Quality { Draft, Standard, High }
pub enum VideoCodec { H264, Vp9, Av1 }               // mp4 ⇒ h264; webm ⇒ vp9 (default) | av1
```

Scene rules: `LayerAdd`, `LayerRemove`, `AudioAdd`, `AudioRemove` only accept
`SceneTarget::Project` (a take's streams are fixed facts). `Set`, `Order`
and `SceneApply` work on both. `SceneApply` matches layers and tracks **by
id** and copies layout fields only (`rect`, `crop`, `opacity`, `visible`,
`fit`, z-order of matched layers; `gainDb`, `muted`, `syncOffsetMs` for
tracks). It never adds or removes layers. CLI mapping:
`--take T` = project → take T ("Reset take to project scene");
`--to-all-takes` = project → all takes (or `--from-take T --to-all-takes` =
"Apply this layout to all takes"); `--from-take T` alone = take T → project
("Use this layout for new takes").

### 4.4 Queries

```rust
pub enum Query { Status, TakeList { unused_only: bool }, TakeShow { take: TakeId },
                 ClipList, History { limit: usize }, State }
```

`query` returns JSON (the CLI renders the human view). `Status` includes the
project summary, recording phase, running jobs and host kind. `State` is the
full `State` minus history patches.

### 4.5 Effects

```rust
pub enum Effect {
    // persistence (run synchronously by the shell, in order)
    WriteProject,                         // atomically write state.project (+ shadow, §8.4)
    WriteUi,                              // write state.ui to .qrec/ui.json
    History(HistoryOp),                   // persist history changes
    EnsureDir { path: String },
    DeleteFiles { job: JobId, paths: Vec<String> },     // purge only

    // capture
    StartCapture { plan: CapturePlan },   // → CaptureStarted | CaptureFailed
    StopCapture { take: TakeId, cancel: bool },          // → CaptureStopped
    ProbeTake { take: TakeId, purpose: ProbePurpose, files: Vec<ProbeTarget> }, // → TakeProbed

    // media jobs (async)
    DetectSilence { job: JobId, key: SilenceKey, file: String, threshold_db: f64, min_silence_ms: u32 },
    RenderExport { job: JobId, plan: ExportPlan },        // → ExportProgress… ExportFinished|Failed|Cancelled
    SaveFrame { job: JobId, plan: FramePlan },            // → FrameSaved|FrameFailed
    CancelJob { job: JobId },

    // time
    StartTimer { timer: TimerId, after_ms: u64 },         // → TimerFired
    CancelTimer { timer: TimerId },

    // talking to people and requesters
    Notify(Toast),                                        // { level: Info|Success|Warning|Error, title, body, actions }
    Progress { req: ReqId, progress: Progress },          // streamed to the requester
    Reply { req: ReqId, reply: Reply },                   // completes the request
    HostIdle,                                             // headless host may exit (§9.3)
}

pub enum HistoryOp {
    Append(HistoryEntry),                 // drops the redo tail first, sets cursor = entry.seq
    SetCursor(u64),                       // undo / redo
    Rewrite { entries: Vec<HistoryEntry>, cursor: u64, base: Option<Box<Project>> }, // purge, compaction, clear
}
```

### 4.6 Events

```rust
pub enum Event {
    Capabilities(Capabilities),
    CaptureStarted { take: TakeId, t0_ns: u64 },
    CaptureFailed { take: Option<TakeId>, source: Option<String>, message: String },
    StreamLost { take: TakeId, stream: String, message: String },
    CaptureStopped { take: TakeId, streams: Vec<RecordedStream>, timed_out: bool },
    TakeProbed { take: TakeId, purpose: ProbePurpose, results: Vec<ProbeResult> },
    TimerFired { timer: TimerId },
    SilenceDetected { job: JobId, key: SilenceKey, silences: Vec<Interval> },
    SilenceFailed { job: JobId, key: SilenceKey, message: String },
    ExportProgress { job: JobId, segment: usize, frames_done: u64 },
    ExportStitchProgress { job: JobId, out_time_ns: u64 },
    ExportFinished { job: JobId, path: String, bytes: u64 },
    ExportFailed { job: JobId, message: String, stderr_tail: Vec<String>, log: String },
    ExportCancelled { job: JobId },
    FrameSaved { job: JobId, path: String },
    FrameFailed { job: JobId, message: String },
    FilesDeleted { job: JobId, deleted: Vec<String>, errors: Vec<String> },
    ProjectChangedOnDisk { project: Result<Box<Project>, String> }, // already parsed + migrated by the shell
}

pub struct RecordedStream { pub id: String, pub file: String, pub start_offset_ns: i64, pub eos: bool }
pub struct ProbeResult { pub id: String, pub file: String, pub info: Result<MediaInfo, String> }
pub struct MediaInfo { pub kind: StreamKind, pub duration_ns: i64, pub width: u32, pub height: u32,
                       pub sample_rate: u32, pub channels: u32, pub start_time_ns: i64 }
pub struct Interval { pub start_ns: i64, pub end_ns: i64 }   // half-open
```

### 4.7 Errors and exit codes

```rust
pub struct CoreError { pub code: ErrorCode, pub message: String, pub hint: Option<String> }
pub enum ErrorCode {            // JSON: snake_case strings
    Usage,                      // exit 2
    NotProject,                 // exit 3  "Not my swamp: this folder isn't empty and has no qrec.json"
    ConfirmationRequired,       // exit 4  message "confirmation required: <description>"
    MissingDependency,          // exit 5
    Busy,                       // exit 6  recording or exporting, or another host holds the lock
    NotFound,                   // exit 7  bad id
    Invalid,                    // exit 1  validation failure, impossible edit
    Unavailable,                // exit 1  e.g. "needs the GUI host"
    Io,                         // exit 1
    Internal,                   // exit 1
}
```

---

## 5. Core behavior

### 5.1 Timeline operations (`qrec_core::timeline`)

Pure functions over `&[Clip]` plus the take pool. Positions are prefix sums.

- `position_of(clips, i)`, `clip_at(clips, timeline_frame) -> Option<(index, offset)>`,
  `total_frames(clips)`, `edit_points(clips)`.
- `split(clips, i, take_frame, new_id)`: requires `in < p < out`; left keeps
  the id. **Property:** the frame mapping timeline frame → (take, take frame)
  is identical before and after.
- `trim(clips, i, new_in, new_out, take_duration)`: `0 ≤ in < out ≤ duration`.
  Out-of-range values are clamped to the take bounds and the result reports
  the clamped values; a zero-length result is an `Invalid` error.
- `move_block(clips, ids, to)`: the selected clips move as one contiguous
  block in their original order. `Index(i)` is the index in the list with the
  block removed. `By(n)` shifts the block's first index by `n` in that list
  (clamped). `Before/After(c)` anchors on a clip outside the block.
  **Property:** the result is a permutation of the input.
- `remove(clips, ids)`, `insert(clips, at, clip)`, `replace_run(clips, ids, new)` (record over).
- Time arguments for clips are clip-relative unless `timeline` is set: for
  `ClipTrim`, absolute `start = T` means new `in = in + T`, absolute `end = T`
  means new `out = in + T`; relative `±T` moves that edge by `±T`.
  With `--timeline`, `T` is a timeline position and must fall inside the clip.

### 5.2 Recording state machine (`apply::record`, `on_event` capture events)

```
Idle ─RecordStart─► Countdown(n) ─TimerFired×n─► Starting ─CaptureStarted─► Recording
  ▲                     │ RecordCancel/Toggle         │ CaptureFailed              │ RecordStop/Toggle
  │                     ▼                             ▼ (take broken, toast)      ▼
  └──────────────────── Idle ◄─────────────────────── Idle          Finalizing ─CaptureStopped─► ProbeTake
                                                                            ─TakeProbed─► Idle (take placed)
```

- **RecordStart** rejects with `Busy` while recording, finalizing or while an
  export job runs, and with `Invalid` when the scene has no layer and no
  track. `Over` needs a contiguous run of existing clips (`OverSelection`
  resolves `ui.selection`). Countdown 0 skips straight to start.
- **Begin take:** allocate `take-NNNN` from `nextTakeNumber` (then +1);
  insert the take (`status: recording`, scene snapshot, planned streams);
  effects `EnsureDir takes/take-NNNN`, `WriteProject` **then**
  `StartCapture(plan)` (the take is on disk before capture begins).
- **CaptureFailed** during `Starting` (health check, 500 ms) or `Recording`:
  take → `broken` with `error`, phase → `Idle`, `Notify` + `Reply` error.
  The shell keeps the files.
- **StreamLost:** mark the layer/track `missing` in the take scene; keep going.
- **RecordStop:** phase → `Finalizing`; `StopCapture { cancel: false }`.
  **RecordCancel:** same with `cancel: true` (during countdown: just cancel
  the timer, no take).
- **CaptureStopped:** `timed_out` ⇒ take `broken` (files stay). Otherwise
  record `startOffsetNs` per stream and `ProbeTake { purpose: Finalize }`.
- **TakeProbed (Finalize):** fill stream media info, drop unreadable streams
  (mark their layer/track `missing`), compute `durationFrames` (§2.2).
  `< 10` frames ⇒ `tooShort`, to the bin, toast "That was shorter than Lord
  Farquaad. Take moved to the bin.". Cancelled ⇒ `ok`, bin, no clip.
  Otherwise `ok` and place it: Append ⇒ clip `[0, durationFrames)` at the end;
  Over ⇒ replace the run in place. That placement is **one history entry**
  (`label: "Record take 3"`, `cmd: {"event":"takeFinalized","take":"take-0003"}`).
  Then, if `autocut.autoRunAfterTake`, start an autocut job on the new clip;
  it becomes its **own** history entry when it completes.
- **Recovery** (`Session::open`, lock held, so nobody is recording): for each
  take still `recording`, `ProbeTake { purpose: Recover }`; readable ⇒ `ok`,
  bin, toast "Recovered take 7 from the bog."; otherwise `broken`.
- **TakeDiscardLast:** the take with the highest number. If clips reference
  it, remove them all (one history entry "Discard take N"); otherwise
  `NotFound` "Take N has no clips on the timeline". It never looks further back.
- **TakePurge:** only takes no clip references (bin, broken, tooShort); needs
  confirmation listing files and sizes (sizes come from the shell, see §8.6);
  removes the takes from the pool, `DeleteFiles`, and rewrites history
  (§7.5). Not undoable.
- During `Recording`/`Finalizing` only `RecordStop`, `RecordToggle`,
  `RecordCancel`, `Select`, `Seek`, `Play/Pause`, `SetUi` and queries are
  accepted; other commands return `Busy`.

### 5.3 Autocut (`qrec_core::autocut`)

Plan (pure, §9.2), for one clip and one track:

1. Silences come in **file time** (from `silencedetect`). Shift to take time:
   `t = x + startOffsetNs + syncOffsetMs·10⁶`.
2. Shrink each silence by `paddingMs` on both sides; drop empty ones.
3. Kept = clip range minus shrunk silences.
4. Drop kept fragments shorter than `minKeepMs`.
5. Snap each fragment outward to frames (`in = floor`, `out = ceil`), clamp
   to the clip, merge touching fragments.
6. If nothing is kept, the clip is **left unchanged** and reported as
   "entirely silent at −35 dB" (autocut never deletes a whole clip).

Track choice: `overrides.track` → `autocut.track` → first `mic` track of the
clip's take scene → first audio track. A clip whose take lacks the track is
skipped with a warning in the result.

Flow: `Autocut` resolves clips and keys `SilenceKey = sha256(file, thresholdDb, minSilenceMs)`.
Missing keys produce `DetectSilence` effects (one job); when all results
are in (`SilenceDetected`), the core plans every clip and applies the whole
command as **one** history entry ("Autocut take 3: removed 4 silences (6.2 s)"),
or returns the plan for `dryRun`. New fragments: the first keeps the
original clip id. Property tests: kept ⊇ every padded-speech region whose
fragment is ≥ `minKeepMs`; output clips stay inside the original clip; a clip
with no silence is unchanged.

### 5.4 Jobs

`Job { id, kind: Autocut{..} | Export{..} | FrameSave{..} | Purge{..}, req: Option<ReqId>, started: DateTime<Utc> }`.
Export progress = `0.9 · framesDone / totalFrames + 0.1 · stitchFraction`;
ETA from `ctx.now − started`. `Progress` payloads mirror the CLI JSON lines:
`{"event":"progress","phase":"render","done":1234,"total":5678,"etaSecs":42,"label":"Rendering clip 4/12"}`.

---

## 6. Time

### 6.1 `TimeArg` grammar (`qrec_core::time`)

| Input | Meaning |
| --- | --- |
| `412f` | frames |
| `13.7s` | seconds (decimal, parsed exactly to ns, no floats) |
| `00:00:13:21` | timecode `HH:MM:SS:FF`, `FF < ceil(fps)` |
| `00:00:13.700` | `HH:MM:SS.mmm` (1–9 fraction digits) |
| `+1f`, `-2s`, `+00:00:01:00` | relative: sign + any of the above |

A bare number is a usage error with the hint "add f (frames) or s
(seconds)". `TimeArg` round-trips through `Display`/`FromStr`.

### 6.2 Conversions (all integer math, `u128` intermediates)

- `frame_ns(f, fps) = round(f · 10⁹ · den / num)` — the start of frame `f`.
- `ns_to_frame_floor/round/ceil(ns, fps)`.
- Seconds → frames rounds to the nearest frame. Timecode frames use the
  nominal rate `ceil(num/den)` (non-drop-frame).
- `timecode(f, fps) -> "HH:MM:SS:FF"`, `timecode_compact` → `HHMMSSFF` (frame names).

---

## 7. History and undo

### 7.1 Entries

```rust
pub struct HistoryEntry {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub label: String,                 // "Split clip c7f3 at 00:00:13:20"
    pub cmd: serde_json::Value,        // serialized Command, or {"event":…}, or {"external":true}
    pub forward: json_patch::Patch,    // RFC 6902, against qrec.json
    pub inverse: json_patch::Patch,
    pub after_hash: String,            // sha256 of the canonical undoable view after this entry
}
pub struct History { pub entries: Vec<HistoryEntry>, pub cursor: u64 } // cursor = seq of the last applied entry, 0 = none
```

### 7.2 The undoable view

`undoable_view(&Project) -> Value` is `qrec.json` with `version`,
`createdAt` and `nextTakeNumber` removed and every take reduced to
`{ "scene": … }`. JSON Pointers into it are valid pointers into `qrec.json`.
To diff `before → after`, both views are built over the **same set of
takes** (takes created by the command are copied into `before`; takes gone
from `after` are dropped from both), so patches only ever touch
`/takes/<id>/scene/…`, never whole takes.

`forward = diff(view(before), view(after))`, `inverse = diff(view(after), view(before))`,
each prefixed with RFC 6902 **`test` ops** for every value it replaces or
removes. Undo/redo applies the patch to the full `qrec.json` value; a failed
`test` means the state drifted and the undo is refused (`Invalid`, "history
doesn't match the project; see `qrec history`") instead of corrupting
anything. After applying, the result is deserialized, `durationFrames` are
re-derived if fps changed, and the project is validated.

### 7.3 Operations

- **Append:** a new entry gets `seq = cursor + 1`; entries after the cursor
  (the redo tail) are dropped. History is linear.
- **Undo N / Redo N:** apply `inverse` / `forward` one entry at a time;
  `HistoryOp::SetCursor`. Undoing a recording removes its clip; the take
  stays in the pool (bin). Redo puts it back.
- **Compaction:** above 5,000 entries, the oldest are dropped down to 4,000
  and `history.base.json` becomes the state before the oldest kept entry
  (computed by walking inverses back from the current state).
- **Clear** (`!`): drop all entries; cursor 0.
- **Restore `<seq>`** (`!`): undo/redo to that seq. When `qrec.json` itself
  is corrupt, the shell starts from the shadow `history.head.json` (§8.4),
  which holds the state at the cursor.

### 7.4 External edits

The shell keeps `.qrec/history.head.json`, the exact project it last wrote.
On open and on inotify events, if `qrec.json` differs from the shadow:
the new file is parsed, migrated and validated. Invalid ⇒ keep the last good
state, `Notify` error with the validation message (host), or exit with
`Invalid` (CLI). Valid ⇒ `Event::ProjectChangedOnDisk`, and the core appends
an `External edit` entry with the diff of the undoable views (if they differ)
and adopts the file (including non-undoable fields). The shell then refreshes
the shadow. This is how "insert an External edit entry so undo stays correct"
(§8.1) is implemented without false positives from non-undoable writes.

### 7.5 Purge rewrite

Purging take T must leave no reachable state that references T. Walking
back from the cursor (applying inverses), find the newest state whose
timeline references T; everything up to and including that entry is
dropped (the base becomes the state after it). Walking forward through the
redo tail, drop everything from the first state that references T. In the
kept entries, ops under `/takes/T/…` are removed (entries left empty are
dropped) and `afterHash` is recomputed. Emitted as `HistoryOp::Rewrite`.

---

## 8. Shell

### 8.1 Session

```rust
pub struct Session { /* dir, state, lock, store, history store, effect runner, inbox */ }
impl Session {
    pub fn open(dir: &Path, opts: OpenOpts) -> Result<Session, ShellError>;
    pub fn dispatch(&mut self, req: Request, to: Box<dyn Responder>);  // the only way in
    pub fn handle(&mut self, msg: Msg);                                // events from workers
    pub fn run_until_idle(&mut self);                                  // in-process CLI
}
pub struct Request { pub id: ReqId, pub body: RequestBody, pub confirmed: bool }
pub enum RequestBody { Command(Command), Query(Query) }
pub trait Responder: Send { fn progress(&mut self, p: &Progress); fn reply(&mut self, r: Reply); }
```

`dispatch` builds a `Ctx` (clock, `getrandom` entropy, `confirmed`, `req`),
pre-resolves anything that needs I/O (default audio sources, existence of a
hand-picked `--out` path ⇒ confirmation, take file sizes for purge), calls
`core::apply`, then runs the effects. Cheap effects run inline in order
(`WriteProject`, `WriteUi`, `History`, `EnsureDir`, `Notify`, `Reply`,
`Progress`); slow ones are handed to workers that post `Event`s back to the
session inbox, where `handle` feeds them to `core::on_event`.

`OpenOpts { host: HostKind, mutating: bool }`: the folder rule (§4.4) is
checked first. A strictly empty folder is initialized only when `mutating`
(lazy init) — read-only commands in an empty folder see an uninitialized
project and write nothing.

### 8.2 Files (`store`)

- Atomic write: `qrec.json.tmp` → `fsync` → `rename` → `fsync(dir)`. The same
  helper writes `ui.json`, history files and the shadow.
- Loading: read → `migrate` (backup `.qrec/qrec.v{N}.json.bak` first) →
  deserialize → validate. A `qrec.json` that doesn't parse or validate is
  **never** overwritten; opening fails with `Invalid` and a hint pointing at
  `qrec history restore <seq> --yes`.
- `history.jsonl` (one entry per line), `history.cursor` (`{"seq":N}`),
  `history.base.json`, `history.head.json` (shadow), `ui.json`, `logs/`,
  `cache/{silence,segments,thumbs,waveforms}/`.

### 8.3 Lock, hosts and IPC

- The host holds `flock(LOCK_EX)` on `.qrec/lock` for its lifetime. An
  in-process CLI command takes it for the duration of the command.
- Socket: `$XDG_RUNTIME_DIR/qrec/<sha256(canonical project path) hex>.sock`.
- Protocol: JSON lines, one request per connection.
  - client → host: `{"v":1,"id":"…","request":{"command":{…}} | {"query":{…}} | "hello","confirmed":false}`
  - host → client: zero or more `{"id":"…","progress":{…}}`, then exactly one
    `{"id":"…","reply":{"ok":true,"data":…}|{"ok":false,"error":{…}}}`.
  - `hello` replies `{pid, projectDir, host: "gui"|"headless", recording: <phase>, version}`.
- Project resolution in the CLI (§10.2): `--project` → the current directory
  if it holds `qrec.json` or is strictly empty → the single live host found
  by `hello`-probing `$XDG_RUNTIME_DIR/qrec/*.sock` (stale sockets are
  removed) → otherwise exit 3 (or a usage error listing several hosts).
- CLI routing: live host for the project ⇒ send over IPC and stream. No
  host ⇒ `record start`/`toggle` spawn `qrec host --headless --project DIR`
  (`setsid`, stdio to `.qrec/logs/host-*.log`), wait ≤ 5 s for its socket,
  then send over IPC. `seek`/`play`/`pause` without a GUI host ⇒
  `Unavailable`. Everything else runs in-process through the same `Session`.

### 8.4 Headless host

`qrec host --headless` opens the session as `HostKind::Headless`, serves IPC,
and exits after the core emits `HostIdle` (recording finalized and no jobs
running) or after 60 s without ever recording. It removes its socket on exit.

### 8.5 ffmpeg runners

Every child process writes stderr to a log file (`.qrec/logs/<job>.log`,
`takes/<take>/recorder.log`) and never leaves a pipe unread. Progress comes
from `-progress pipe:1` on stdout, parsed line by line. Segment renders run
in parallel, at most `max(1, cores / 2)` at a time; a segment whose cache file
already exists is skipped. Outputs are written to a temp name and renamed when
complete. Auto-named exports and frames never overwrite: on a name clash the
shell appends `-2`, `-3`, …. Cancel sends `SIGTERM`, then `SIGKILL` after 2 s.

### 8.6 Pre-resolution before `apply`

The core never touches the disk, so `Session::dispatch` adds facts first:
`AudioAdd` without `--source` gets the default mic (`pactl get-default-source`
when it's an `alsa_input.*`) or the default sink's `.monitor`;
`Export`/`FrameSave` with a hand-picked `--out` that exists needs
confirmation; `TakePurge` gets file sizes for the confirmation text
(`Ctx`-adjacent `Facts` passed to `apply`).

### 8.7 GPU safety and GStreamer init

On the dev machine the NVIDIA driver on `renderD128` can wedge, and anything
that initializes it blocks forever: CUDA (`nvcodec`), oneVPL (`qsv`) and
libva's NVIDIA driver (the `va` plugin probes every render node during the
registry scan). Rules:

- Commands that only need ffmpeg never initialize GStreamer.
- Before `gst::init()`, if `LIBVA_DRIVER_NAME` is unset, Qrec sets it from the
  PCI vendor of the VA-API node it will use (Intel `0x8086` ⇒ `iHD`, AMD
  `0x1002` ⇒ `radeonsi`). With no configured node it picks the first Intel or
  AMD render node; NVIDIA nodes are never picked automatically.
- `gst::init()` runs under a watchdog (30 s). On timeout Qrec reports
  "GStreamer is stuck scanning plugins (a GPU driver is probably hung)" with
  the `doctor` hint, instead of freezing.
- The Nix package ships gst-plugins-bad without `nvcodec` and `qsv`.

### 8.8 Capture engine (M2)

```rust
pub trait CaptureEngine: Send {
    fn start(&mut self, plan: CapturePlan, events: EventSink) -> Result<(), String>;
    fn stop(&mut self, take: &TakeId, cancel: bool);
}
```

`QREC_CAPTURE_BACKEND=fake` swaps sources for `videotestsrc is-live=true`
(at the layer's planned size) and `audiotestsrc is-live=true`, so the full
loop runs headless. The real backend uses the portal (`ashpd`, persist mode
2, restore tokens in `~/.local/state/qrec/portal-tokens.json` keyed by
output name), `v4l2src` and `pulsesrc`. Record bins attach at running time
`T0`; a pad probe records each file's first-buffer running time to produce
`startOffsetNs`. Health check 500 ms, EOS timeout 10 s.

---

## 9. Planners (pure, snapshot-tested with `insta`)

### 9.1 Layer placement (`qrec_core::layout`)

`place(src_w, src_h, crop, rect, fit) -> Placement { src: (x,y,w,h), dst: (x,y,w,h) }`.
The cropped source is `src`; `contain` scales it into `rect` keeping aspect,
centered (`dst ⊆ rect`); `cover` additionally crops `src` (centered) to the
rect's aspect so `dst == rect`. Export, frame save, GES preview and the GUI
canvas all use this one function, so they can't disagree.

### 9.2 Export plan (`qrec_core::export_plan`)

`plan_export(project, req, caps, ctx) -> Result<ExportPlan, CoreError>`:

```rust
pub struct ExportPlan { pub out: String, pub segments: Vec<SegmentPlan>, pub stitch: StitchPlan,
                        pub total_frames: u64, pub log_dir: String }
pub struct SegmentPlan { pub clip: ClipId, pub key: String, pub out: String, pub frames: u64, pub argv: Vec<String> }
pub struct StitchPlan { pub list_file: String, pub list_content: String, pub argv: Vec<String> }
```

- argv are run with cwd = project root; paths are project-relative.
- Per segment: one input per visible layer with media and one per unmuted
  track, each with input-side `-ss` = take time of the clip start minus the
  stream's offset (`startOffsetNs`, plus `syncOffsetMs` for audio). A negative
  value seeks 0 and delays the stream instead (`setpts=PTS+d/TB` for video,
  `adelay=<samples>S:all=1` for audio).
- Video graph: `color=c=<bg>:s=WxH:r=<fps>` base; per layer `crop` (src) →
  `scale` (dst size) → `format=yuva420p` → `colorchannelmixer=aa=<opacity>`
  (when < 1) → `overlay=x:y:eof_action=pass` in z-order → `format=yuv420p`,
  limited-range BT.709 (`-color_range tv -colorspace bt709 -color_primaries
  bt709 -color_trc bt709`). Exact length with `-frames:v N`.
- Audio graph: per track `aresample=48000`, stereo, `volume=<gain>dB`, then
  `amix=normalize=0` (or `anullsrc` when no track is audible), `apad`,
  `atrim=end=<duration>`, and a 5 ms `afade` in/out unless the boundary is a
  natural take start (`in == 0`) or end (`out == durationFrames`).
- Segment codec: target video codec (`libx264 -crf 20 -preset medium`,
  `h264_vaapi` on the verified node when available and preferred,
  `libvpx-vp9 -crf 32 -b:v 0 -row-mt 1`, `libsvtav1`), PCM audio, Matroska.
  Quality presets scale crf/qp/preset (Draft/Standard/High).
- Cache key = `sha256(PLAN_VERSION ‖ argv without the output path ‖ input
  paths)`; out = `.qrec/cache/segments/<key>.mkv`.
- Stitch: concat demuxer list (in the segments dir, file names only),
  `-c:v copy`, audio encoded once (`aac -b:a 192k` / `libopus -b:a 128k`),
  `-movflags +faststart` for MP4, to `exports/<name>-YYYYMMDD-HHMMSS.<ext>`.

### 9.3 Frame plan (`qrec_core::frame_plan`)

The same graph as a one-frame segment at the playhead's take frame, PNG
(`-frames:v 1`, RGB via an explicit `bt709`/`tv` → `pc` conversion), to
`frames/<name>-<take>-<HHMMSSFF>.png` (timeline timecode). `--layer L2`:
the raw stream frame at native resolution, no crop.

### 9.4 Capture plan (`qrec_core::capture_plan`)

Describes the record bins as data: per layer `{ id, source, file, encoder }`
with `encoder = Va { device, qp } | X264 { speed_preset: "veryfast", tune:
"zerolatency", quantizer: 18 }`, `key_int_max = round(fps)` (1 s), CFR caps at
canvas fps; per track `{ id, device, file }` (FLAC in Matroska, 48 kHz).
File names: `L1-screen-DP-4.mkv`, `L2-camera-0.mkv`, `A1-mic.mka`,
`A2-system.mka` (sanitized). Encoder choice: `encoder = x264` ⇒ x264;
otherwise VA when `caps.gst_va_h264` on the resolved node, else x264.

### 9.5 GES plan (`qrec_core::ges_plan`)

`plan_ges(project) -> GesTimeline { canvas, duration_ns, video: Vec<GesVideoClip>, audio: Vec<GesAudioClip> }`.
Each timeline clip expands to one GES clip per visible layer and audible
track: `{ file, start_ns, inpoint_ns, duration_ns, z (0 = bottom), placement, alpha }`
/ `{ file, start_ns, inpoint_ns, duration_ns, volume, fade_in_ns, fade_out_ns }`.
Frame boundaries use `frame_ns`. The parity test renders frames through GES
and through the frame plan and requires SSIM ≥ 0.98 (§12).

### 9.6 Keymap (`qrec_core::keymap`)

Static table of every bindable command (§10.2): `{ id: "clip.split", group,
label, default_keys, cli: Option<&str>, contexts, allowed_while_recording }`.
`resolve(overrides: &str /* keys.toml */) -> Keymap { bindings, conflicts, errors }`;
`hints(HintContext) -> Vec<Hint>` for the hint bar; `palette()` lists every
command with key and CLI form. `qrec keys [--json]` prints the effective map.
`keys.toml`: `[bindings]` table, command id → key string or array
(`"clip.split" = "s"`, `"history.redo" = ["ctrl+shift+z", "ctrl+y"]`).

### 9.7 User config (`qrec_core::config`)

`config.toml` (snake_case): `theme`, `[canvas] width/height/fps`,
`[autocut] threshold_db/min_silence_ms/padding_ms/min_keep_ms`,
`[encoder] preference/vaapi_device`, `[ui] scale/reduced_motion/sounds`.
`get(key)`/`set(key, value)` on dotted keys with validation; the shell owns
the file.

---

## 10. CLI

- Global flags: `--project DIR`, `--json`, `--yes`, `-q`, `-v`.
- `--json`: exactly one object `{"ok":true,"data":…}` or
  `{"ok":false,"error":{"code","message","hint"}}`; streaming commands
  (export, `record … --wait`) print JSON lines, the last one being that object.
- Mutating commands include `seq` and created ids in `data`.
- Human output: short sentences and aligned tables; export shows a one-line
  progress bar with phase, percent and ETA.
- Project-less commands: `--help`, `version`, `doctor`, `sources list`,
  `schema`, `keys`, `agent-guide`, `config`, `completions`.
- Extensions beyond the §14 list (for GUI parity): `qrec project set`,
  `qrec autocut settings`, `qrec export cancel`, `qrec history clear`,
  `qrec completions <shell>`, hidden `qrec host --headless`.

---

## 11. Canvas fps changes

Changing fps on a project with takes needs confirmation (the dialog lists all
clips). Clip bounds are re-quantized through time (`in` floored, `out`
ceiled, then clamped), and every take's `durationFrames` is re-derived from
its streams. Undo restores the old bounds through the patch and re-derives
`durationFrames` again.

---

## 12. Testing

| What | Where |
| --- | --- |
| Core unit + `proptest` | `crates/qrec-core/src/**` and `tests/props.rs`: split identity, trim bounds, move permutation, undo∘apply = id, autocut padding, migration round trip |
| Planner snapshots | `insta` snapshots of export/frame argv, capture plan, GES plan, keymap |
| Shell | store atomicity, lock exclusion, history persistence, IPC round trip |
| CLI integration | `crates/qrec/tests/`: temp folders, fixture media from ffmpeg (`testsrc2`, `sine` with known silence gaps), `assert_cmd` |
| Fake capture | `QREC_CAPTURE_BACKEND=fake` record start/stop end to end (M2) |
| Parity | GES vs ffmpeg SSIM ≥ 0.98 on 5 frames (M0 d) |

Fixture helpers live in `crates/qrec-shell/src/testkit.rs` (behind the
`testkit` feature) so every crate's tests can build projects with real media.

---

## 13. GUI (blocked)

`qrec-gui` stays a stub until the designs in `design/qrec.pen` are approved
(§15.1). When it starts, it will: run the Session on the GTK main context,
render from `State`, send every input as a `Command`, use
`gtk4paintablesink` for the Live and Program canvases, and import the
theme tokens from `crates/qrec-gui/resources/theme/*.css`.
