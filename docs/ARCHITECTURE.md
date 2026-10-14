# Qrec — Architecture (v2)

`SPEC.md` is the product contract; this file is the engineering contract:
module boundaries, exact types, protocols and the rules every module
follows. Where they disagree, `SPEC.md` wins and this file gets fixed.

**References:** `SPEC §N` points at SPEC.md, `A§N` at this file.

v2 folds in a five-lens review (spec coverage, state/history, shell and
processes, media, implementability). Several media rules below were
measured on the dev machine with ffmpeg 8.1.2 and GStreamer/GES 1.26.11;
they are marked *(measured)* and must not be "simplified" away.

---

## A§1 Crates, dependencies, purity, ownership

### A§1.1 Crates

```
crates/
  qrec-core/   pure functional core: model, protocol, planners, state machine
  qrec-shell/  imperative shell: Session, files, history store, lock, IPC,
               hosts, process runner, ffmpeg/ffprobe, GStreamer capture,
               portal, sources, doctor
  qrec-cli/    clap definitions → Request; output formatting; exit codes
  qrec-gui/    GTK views (blocked until the pen.dev designs are approved)
  qrec/        the `qrec` binary
```

`qrec → qrec-cli → qrec-shell → qrec-core`, `qrec → qrec-gui → qrec-shell`.
**`qrec-core` depends on no other workspace crate**, not even in dev-deps.

### A§1.2 Dependencies (pinned in the root `Cargo.toml`)

Agents never add or bump dependencies; they ask. CI asserts
`cargo tree -d -i glib` is empty (one glib for gstreamer, gtk4, ashpd).

| Crate | Version / features | Used by |
| --- | --- | --- |
| serde, serde_json | 1, **never** `preserve_order` | all |
| schemars | 1.2, `chrono04` | core (`qrec schema`) |
| chrono | 0.4, `std, serde`, no default features; the shell adds `clock` | core, shell |
| json-patch | 4.2 (use its re-exported `jsonptr`) | core |
| sha2 0.11 + hex | only through `qrec_core::hash` | core |
| toml 1.1, toml_edit 0.25 | config and keys files | core |
| proptest, insta, pretty_assertions | tests; core feature `testkit` exposes proptest strategies | core |
| gstreamer(-app/-video/-audio/-pbutils), gstreamer-editing-services | 0.25, features ≤ `v1_24` | shell (feature `gst`, default on) |
| ashpd | 0.13, `default-features = false`, `async-io, screencast, camera` (no tokio anywhere) | shell |
| futures-lite | 2.6 (`block_on` for ashpd) | shell |
| crossbeam-channel | 0.5 | shell |
| notify | 8 (inotify) | shell |
| wayland-client | 0.31 (wl_output names) | shell |
| rustix | 1.1 `process, termios` (setsid, kill, process groups) | shell |
| libc | 0.2 (`renameat2`, `SO_PEERCRED`, `prctl`) | shell |
| tempfile, getrandom, dirs, which, tracing(-subscriber), anyhow | | shell, cli |
| clap 4.6 (derive), clap_complete | | cli |
| assert_cmd, predicates | tests | qrec |

File locking uses std `File::try_lock` (flock), no extra crate.

### A§1.3 Purity rules for `qrec-core` (enforced)

- No fs, processes, network, threads, env, clocks or randomness.
  `crates/qrec-core/clippy.toml` bans them; `lib.rs` has
  `#![deny(clippy::disallowed_methods, clippy::disallowed_types)]`.
- `HashMap`/`HashSet` are banned too (nondeterministic iteration breaks
  snapshots): use `BTreeMap`/`BTreeSet`.
- chrono is built without `clock`; CI also runs `cargo check -p qrec-core`
  alone so workspace feature unification can't hide a `Utc::now()`.
- Paths in the core are `String`s: project-relative and `/`-separated,
  except the absolute `out` of an export or frame save (A§5.9).
- Everything else arrives as data: in a `Command`, in `Ctx` (time,
  entropy, confirmation, `Facts`) or in an `Event`.

### A§1.4 Module ownership

The foundation lands every shared type, the dispatch skeletons and the
test kits before any parallel work. After that, each owner edits only
their files; shared files (`model.rs`, `state.rs`, `ctx.rs`, `protocol/*`,
`plans.rs`, `Cargo.toml`, `lib.rs`) change only through the integrator.

```
crates/qrec-core/src/
  lib.rs ids.rs model.rs error.rs time.rs layout.rs hash.rs      foundation (done)
  state.rs ctx.rs plans.rs protocol/{command,query,effect,event,
    reply,progress,toast,wire}.rs testkit.rs                      foundation
  apply/mod.rs on_event/mod.rs history/commit.rs                  foundation (skeleton + commit helper)
  timeline.rs apply/{clip,ui}.rs ui_normalize.rs                  owner: timeline
  history/{view,diff,ops,purge,external}.rs apply/history.rs     owner: history
  apply/{project,scene}.rs requantize.rs                          owner: scene
  apply/record.rs on_event/record.rs capture_plan.rs              owner: record
  autocut.rs apply/autocut.rs on_event/autocut.rs
    parse/silencedetect.rs                                        owner: autocut
  export_plan.rs frame_plan.rs ges_plan.rs naming.rs
    media_argv.rs apply/output.rs on_event/output.rs
    parse/{ffprobe,progress}.rs                                   owner: output
  validate.rs migrate.rs schema.rs query.rs keymap.rs config.rs
    agent_guide.rs                                                owner: meta
crates/qrec-shell/src/
  lib.rs paths.rs msg.rs deps.rs wire.rs testkit/                foundation (skeleton)
  store/ lock.rs host/{lockfile,publish}.rs watcher.rs           owner: store
  session/ facts.rs caps.rs timers.rs proc.rs                    owner: session
  ipc/ host/{headless,cli_host,signals,spawn}.rs                 owner: ipc
  media/ sources.rs doctor.rs                                     owner: media
  capture/ gpu.rs gst_init.rs ges_render.rs                       owner: capture
crates/qrec-cli/, crates/qrec/                                    owner: cli
```

Tests live with their owner: unit tests in the module, properties in
`crates/qrec-core/tests/props_<area>.rs`, CLI tests in
`crates/qrec/tests/<area>.rs`, snapshots next to their tests.

---

## A§2 Project model (`qrec.json`, schema v1)

The model is implemented in `crates/qrec-core/src/model.rs` and round-trips
the SPEC §6.3 example byte for byte (`tests/spec_example.rs`). Serde rules,
verified: every struct `#[serde(rename_all = "camelCase", deny_unknown_fields)]`;
tagged enums `#[serde(tag = "kind", rename_all = "camelCase",
rename_all_fields = "camelCase", deny_unknown_fields)]`; unit enums
`rename_all = "camelCase"` (`tooShort`). Never combine `flatten` with
`deny_unknown_fields`.

Additions to the SPEC §6.3 shape (all optional, omitted when default):

| Field | Type | Why |
| --- | --- | --- |
| `Layer.locked` | bool | panel lock (SPEC §12) |
| `Layer.missing`, `AudioTrack.missing` | bool | source lost or unreadable (SPEC §16); a fact, set only on take scenes, never copied by `SceneApply` |
| `Take.error` | string | why a take is broken |
| `Take.placed` | bool | finalization placed clips; drives discard-last (A§5.5) |
| `Take.replaced` | `Vec<Clip>` | clips a record-over replaced; discard puts them back (A§5.5) |
| `Stream.durationNs` | i64 | probed duration (audio-only takes, re-derivation) |
| `Stream.frames` | u64 | video packet count from `ffprobe -count_packets` (A§2.2) |
| `Stream.fps` | Fps | the recorded rate of a video stream |

`Stream` is one flat struct (`id, kind, file, width?, height?, sampleRate?,
channels?, startOffsetNs, durationNs?, frames?, fps?`); media fields are
absent while the take is recording.

### A§2.1 Defaults

| Field | Default |
| --- | --- |
| canvas | 1920×1080 @ 30/1, `#000000`; overridden by config `[canvas]` |
| recording | countdown from config (`recording.countdown_secs`, default 0), `encoder: "auto"`, `vaapiDevice: null` (init never copies a device into a project) |
| autocut | from config `[autocut]`: autoRun false, track null, −35 dB, 400/120/200 ms |
| new screen layer | name `Screen <output>`, label from `Facts.screens`, rect = full canvas, fit `contain` |
| new camera layer | name `Camera`, label from `Facts.cameras`, 20 % of canvas width at 4:3, bottom-right with a 2 % margin, fit `cover` |
| new audio track | source from `Facts.default_mic` / `default_monitor` unless given; label = device description, else `Mic` / `System audio`; gain 0, not muted, offset 0 |

**Effective encoder settings** (capture and export use the same pair):
device = project `recording.vaapiDevice` if not null, else config
`encoder.vaapi_device`, else the first Intel (`0x8086`) or AMD (`0x1002`)
render node — never NVIDIA automatically. Encoder = project
`recording.encoder` if not `auto`, else config `encoder.preference`.

### A§2.2 Derived values

`Take.durationFrames` comes from video packet counts, never from container
durations (MKV durations are ms-quantized and N/A after a crash) *(measured)*.
For each non-missing video stream with `off = ns_to_frame_round(startOffsetNs)`:
- recorded fps == canvas fps: `off + frames`;
- otherwise: `ns_to_frame_floor(startOffsetNs + frame_ns_rec(frames) + frame_ns(1)/2)`.

`durationFrames` is the minimum over those streams. Audio-only takes use
`ns_to_frame_floor(startOffsetNs + durationNs)` of the shortest track. It is
re-derived whenever the canvas fps changes (including undo/redo of one).

### A§2.3 Validation (`validate::validate(&Project) -> Vec<Issue>`)

`Issue { severity: Error | Warning, path: String /* JSON Pointer */, message }`.
Errors make a project invalid:
- `version == CURRENT_VERSION`; `nextTakeNumber` > every take number.
- Clips: unique ids; take exists with status `ok`; `0 ≤ in < out ≤ durationFrames`.
- Scenes: unique layer/track ids; `0 ≤ opacity ≤ 1`; `rect.w, rect.h ≥ 1`;
  gain within ±60 dB; `|syncOffsetMs| ≤ 5000`.
- Streams: `file` must be `takes/<takeId>/<name>` with `<name>` matching
  `^[A-Za-z0-9._-]+$` (guards purge, A§5.6); non-recording takes have media
  fields for their kind.
- Canvas: even `16 ≤ w, h ≤ 8192`; `fps.num, fps.den > 0`, fps ≤ 240.
- `countdownSecs ≤ 10`; autocut numbers in range (threshold −90…0 dB,
  ms values ≤ 60 000).

Warnings: a take-scene layer/track without a stream ("missing media");
`autocut.track` not in the project scene ("takes lacking it are skipped").

---

## A§3 Runtime state (`state.rs`)

```rust
pub struct State {
    pub project: Project,
    pub ui: UiState,          // .qrec/ui.json; never undoable
    pub history: History,     // the core decides, the shell persists
    pub runtime: Runtime,     // never persisted
}

#[derive(Default)] #[serde(rename_all = "camelCase", default)]
pub struct UiState {
    pub selection: Vec<ClipId>,          // timeline order, kept valid by ui_normalize
    pub playhead: Frame,
    pub canvas_mode: CanvasMode,         // "live" | "program"
    pub selected_layer: Option<LayerId>,
    pub snapping: bool,                  // default true
    pub bin_open: bool,
    pub zoom: Option<f64>,               // px per frame; None = fit
    pub scroll_px: f64,
    pub solo: Option<TrackId>,
    pub panels: BTreeMap<String, f64>,
}

pub struct History {
    pub entries: Arc<Vec<HistoryEntry>>, // oldest first; Arc::make_mut to append
    pub cursor: u64,                     // seq of the last applied entry, or base_seq
    pub base_seq: u64,                   // seq of the base snapshot (0 for a new project)
    pub next_seq: u64,                   // seqs are never reused
    pub seen_clip_ids: Arc<BTreeSet<ClipId>>, // derived: every clip id in entries + base
}

pub struct Runtime {
    pub host: HostKind,                  // Gui | Headless | Cli
    pub record: RecordPhase,
    pub jobs: BTreeMap<JobId, Job>,
    pub silence: BTreeMap<SilenceKey, Vec<Interval>>,
    pub capabilities: Capabilities,
    pub next_id: u64,                    // allocates JobId and TimerId
    pub playing: bool,
    pub disk_invalid: Option<String>,    // qrec.json on disk failed to load (A§7.7)
}

#[serde(tag = "phase", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RecordPhase {
    Idle,
    Countdown { remaining: u32, mode: RecordMode, timer: TimerId, start_req: Option<ReqId>, wait: bool },
    Starting { take: TakeId, mode: RecordMode, start_req: Option<ReqId>, wait: bool },
    Recording { take: TakeId, mode: RecordMode, elapsed_ns: u64, waiters: Vec<ReqId> },
    Finalizing { take: TakeId, mode: RecordMode, cancelled: bool,
                 stop_reqs: Vec<ReqId>, waiters: Vec<ReqId>, discard: Option<DiscardRequest> },
}
pub struct DiscardRequest { pub req: Option<ReqId> }
pub enum RecordMode { Append, Over { clips: Vec<ClipRef> } }
pub struct ClipRef { pub id: ClipId, pub take: TakeId, pub in_: Frame, pub out: Frame } // "in" in JSON

pub struct Capabilities {
    pub ffmpeg: Option<FfmpegCaps>,      // None until detected; Some(..) before the first dispatch
    pub vaapi: Option<VaapiCaps>,
    pub cores: u32,
}
pub struct FfmpegCaps { pub version: String, pub identity: String /* resolved binary path */, pub encoders: BTreeSet<String> }
pub struct VaapiCaps {
    pub device: String,                  // effective node (A§2.1)
    pub vendor: u16,
    pub ffmpeg_h264: bool,               // the exact VA export chain encoded 2 frames (A§9.4)
    pub gst_h264_factory: Option<String>,// the va*h264enc whose device-path is this node
}

pub struct Job { pub id: JobId, pub kind: JobKind, pub req: Option<ReqId>, pub started: DateTime<Utc> }
pub enum JobKind {
    Autocut { cmd: serde_json::Value, targets: Vec<AutocutTarget>, params: AutocutParams,
              dry_run: bool, pending: BTreeSet<SilenceKey>, ready: bool },
    Export { out: String, files: BTreeSet<String>, total_frames: u64,
             seg_frames: Vec<u64>, seg_done: Vec<u64>, stitch_total_ns: u64, stitch_done_ns: u64,
             stitching: bool },
    FrameSave { out: String, files: BTreeSet<String> },
    Purge { takes: Vec<TakeId> },
}
pub struct AutocutTarget { pub clip: ClipRef, pub track: TrackId, pub key: SilenceKey }
pub struct AutocutParams { pub threshold_db: f64, pub min_silence_ms: u32, pub padding_ms: u32, pub min_keep_ms: u32 }
pub struct SilenceKey(pub String);       // sha256_hex("{file}\n{size}\n{mtime_ns}\n{threshold_db:.2}\n{min_silence_ms}")
pub enum HostKind { Gui, Headless, Cli }
pub enum CanvasMode { Live, Program }
```

`State::new(project, history, ui, host)` builds a runtime with defaults.
Snapshots (`insta`) of `Outcome`s require `Debug, Clone, PartialEq,
Serialize, Deserialize` on every state, protocol and plan type.

---

## A§4 The dispatch protocol

### A§4.1 Entry points (pure)

```rust
pub struct Ctx {
    pub now: DateTime<Utc>,       // truncated to whole seconds by the shell
    pub utc_offset_secs: i32,     // local offset, only for human file names
    pub entropy: u64,             // fresh per dispatch; seeds IdGen
    pub confirmed: bool,          // --yes / modal confirmed
    pub req: Option<ReqId>,       // the request being served
    pub facts: Facts,             // I/O-derived inputs gathered before apply
}

pub struct Outcome {
    pub state: State,
    pub effects: Vec<Effect>,
    pub reply: Option<Result<CmdResult, CoreError>>, // Some = complete now; None = Effect::Reply later
}

pub fn facts_needed(state: &State, cmd: &Command) -> FactsNeeded;
pub fn apply(state: &State, ctx: &Ctx, cmd: Command) -> Result<Outcome, CoreError>;
pub fn on_event(state: &State, ctx: &Ctx, ev: Event) -> Outcome;
pub fn query(state: &State, ctx: &Ctx, q: &Query) -> Result<serde_json::Value, CoreError>;
```

`apply` returns `Err` for anything that changes nothing (bad id, busy,
needs confirmation, usage); it never returns a partial state. `on_event`
can't fail: failures become state plus `Notify` and `Reply` errors.
**on_event ignores** (with a debug-level `Effect::Log`) any event whose take,
timer or job doesn't match the current phase or job table.

**Gestures** (SPEC §7.1 undo groups): during a drag the GUI calls the pure
`apply` on a cloned `State`, renders the result and discards the effects.
On release it dispatches exactly one `Command`. Transient state is never
stored in `State` or persisted. Canvas layer drags follow the same rule.

### A§4.2 Facts

```rust
#[derive(Default)]
pub struct Facts {
    pub config: UserConfig,                         // always filled (defaults if no file)
    pub folder_name: Option<String>,                // Init
    pub default_mic: Option<AudioDeviceInfo>,       // AudioAdd mic without source
    pub default_monitor: Option<AudioDeviceInfo>,   // AudioAdd system without source
    pub audio_devices: Vec<AudioDeviceInfo>,        // labels for an explicit source
    pub screens: Vec<ScreenInfo>,                   // LayerAdd screen (label, size; unknown → NotFound)
    pub cameras: Vec<CameraInfo>,                   // LayerAdd camera
    pub out: Option<OutFacts>,                      // Export/FrameSave with --out
    pub take_files: BTreeMap<TakeId, Vec<FileFact>>,// purge confirmation
    pub min_take_number: u32,                       // 1 + highest takes/take-N folder
    pub files: BTreeMap<String, FileFact>,          // size/mtime for silence keys
}
pub struct AudioDeviceInfo { pub name: String, pub description: String, pub is_monitor: bool }
pub struct ScreenInfo { pub output: String, pub description: String, pub x: i32, pub y: i32, pub width: u32, pub height: u32, pub scale: f64 }
pub struct CameraInfo { pub device: String, pub label: String, pub modes: Vec<CameraMode> }
pub struct CameraMode { pub width: u32, pub height: u32, pub fps: Fps, pub format: String }
pub struct OutFacts { pub abs: String, pub exists: bool, pub forbidden: Option<String> /* why: inside takes/, .qrec/, or qrec.json */ }
pub struct FileFact { pub path: String, pub bytes: u64, pub mtime_ns: i64 }

#[derive(Default)]
pub struct FactsNeeded {
    pub folder_name: bool, pub audio_devices: bool, pub video_sources: bool,
    pub out: Option<String>, pub take_files: Vec<TakeId>, pub take_dirs: bool,
    pub files: Vec<String>,
}
```

`Session::dispatch` calls `facts_needed`, gathers exactly that (no `pactl`
for a clip split), then calls `apply`. Under `QREC_CAPTURE_BACKEND=fake`
device facts come from the fake inventory (A§8.12).

### A§4.3 Ids, time, confirmation

- `IdGen::new(ctx.entropy)` yields clip ids `"c" + 3 hex` from
  `sha256(entropy ‖ counter)`, skipping every id on the timeline, in
  `ui.selection` and in `history.seen_clip_ids`; after 64 collisions it
  widens to 4, then 5 hex digits. Same state + entropy ⇒ same ids.
- Layers `L{n}` / tracks `A{n}`: `n = 1 + max` over the project scene, every
  take scene and every id seen in history.
- Takes: `n = max(nextTakeNumber, facts.min_take_number, 1 + max take in pool)`.
- Destructive commands need `ctx.confirmed`; otherwise
  `CoreError::confirmation_required(what)` with structured `details`.

### A§4.4 Commands (`protocol/command.rs`)

Serde rules:
- `Command`: `#[serde(tag = "cmd", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]`;
  `bool`/`Vec` fields are `#[serde(default)]`.
- Nested enums use serde's default **external** tagging with
  `rename_all = "camelCase"`: `{"cmd":"clipMove","clips":["c7f3"],"to":{"by":-1}}`,
  unit variants as strings (`"end"`).
- `SceneTarget`, `TimeArg`, `CanvasSpec`, `Fps`, `Color` serialize through
  `Display`/`FromStr` strings (`"project"`, `"take-0001"`, `"13f"`,
  `"1920x1080@30"`).
- Clearable patch fields use `Nullable<T>` (A§4.5): absent = keep,
  `null` = clear, value = set. Never `Option<Option<T>>`.
- A test round-trips every variant through JSON.

`U` = undoable (one history entry), `A` = async (reply later), `!` = may
need confirmation, `G` = GUI host only, `R` = allowed while recording (A§5.3).

| Command | Fields | Flags | CLI |
| --- | --- | --- | --- |
| `Init` | `canvas: Option<CanvasSpec>`, `name: Option<String>` | | `qrec init [--canvas 1920x1080@30] [--name N]` |
| `ProjectSet` | `name`, `canvas: Option<CanvasSpec>`, `background: Option<Color>`, `countdownSecs`, `encoder`, `vaapiDevice: Nullable<String>` | U, `!` when fps changes with takes | `qrec project set …` |
| `LayerAdd` | `target`, `source: VideoSourceArg`, `name: Option`, `rect: Option<Rect>` | U | `qrec scene layer add screen --output DP-4 [--rect X,Y,W,H] [--name ..]` / `camera --device /dev/video0` |
| `LayerSet` | `target`, `layer`, `patch: LayerPatch` | U | `qrec scene layer set <id> [--rect] [--crop T,R,B,L] [--opacity] [--visible] [--fit] [--name] [--locked]` |
| `LayerOrder` | `target`, `layer`, `to: usize` | U | `qrec scene layer order <id> --to N` |
| `LayerRemove` | `target`, `layer` | U | `qrec scene layer remove <id>` |
| `AudioAdd` | `target`, `role`, `source: Option<String>`, `label: Option<String>` | U | `qrec scene audio add mic\|system [--source NAME]` |
| `AudioSet` | `target`, `track`, `patch: AudioPatch` | U | `qrec scene audio set <id> [--gain-db] [--mute] [--sync-offset-ms] [--label] [--source]` |
| `AudioRemove` | `target`, `track` | U | `qrec scene audio remove <id>` |
| `SceneApply` | `from: SceneTarget`, `to: SceneDest` | U | `qrec scene apply --take T \| --to-all-takes \| --from-take T [--to-all-takes]` |
| `AutocutSettings` | `patch: AutocutPatch` | U | `qrec autocut settings [--auto-run] [--track] [--threshold-db] …` |
| `RecordStart` | `mode: RecordModeArg`, `countdown: Option<u32>`, `wait` | A | `qrec record start [--over <clip>… \| --over-selection] [--countdown N] [--wait]` |
| `RecordStop` / `RecordCancel` | – | A, R | `qrec record stop` / `cancel` |
| `RecordToggle` | `wait` | A, R | `qrec record toggle [--wait]` |
| `TakeDiscardLast` | – | U, R (Finalizing) | `qrec take discard-last` |
| `TakePurge` | `takes` | A, `!`, not undoable | `qrec take purge <take>… --yes` |
| `ClipAdd` | `take`, `at: InsertAt` | U | `qrec clip add <take> [--at N \| --after <clip>]` |
| `ClipMove` | `clips`, `to: MoveTo` | U | `qrec clip move <clip>… (--to N \| --by ±N \| --before C \| --after C)` |
| `ClipTrim` | `clip`, `start: Option<TimeArg>`, `end: Option<TimeArg>`, `timeline` | U | `qrec clip trim <clip> [--start T] [--end T] [--timeline]` |
| `ClipSplit` | `clip: Option<ClipId>`, `at: TimeArg`, `timeline` | U | `qrec clip split <clip> --at T` / `--timeline --at T` |
| `ClipRemove` | `clips` | U | `qrec clip remove <clip>…` |
| `Autocut` | `scope: ClipScope`, `overrides: AutocutOverrides`, `dryRun` | U, A | `qrec autocut [--clips …\|--all] [--track] [--threshold-db] [--min-silence-ms] [--padding-ms] [--min-keep-ms] [--dry-run]` |
| `Undo` / `Redo` | `steps: u32` | | `qrec undo [N]` / `qrec redo [N]` |
| `HistoryRestore` | `seq` | `!` | `qrec history restore <seq> --yes` |
| `HistoryClear` | – | `!` | `qrec history clear --yes` |
| `Export` | `format`, `quality`, `codec: Option<VideoCodec>`, `out: Option<String>` | A, `!` (hand-picked existing out) | `qrec export [--format mp4\|webm] [--quality draft\|standard\|high] [--codec h264\|vp9\|av1] [--out PATH]` |
| `ExportCancel` | `job: Option<JobId>` | | `qrec export cancel` |
| `FrameSave` | `at: TimeArg` (timeline frame), `layer: Option<LayerId>`, `out: Option<String>` | A, `!` | `qrec frame save --at T [--layer L2] [--out P]` |
| `Select` | `op: SelectOp` | R | — (UI state) |
| `Seek` | `to: SeekTarget` | G, R | `qrec seek --to T\|start\|end \| --by ±T \| --edit prev\|next` |
| `Play` / `Pause` | – | G (Pause also R) | `qrec play` / `qrec pause` |
| `Shuttle` | `dir: Reverse\|Stop\|Forward` | G | — (`j`/`k`/`l`) |
| `SetCanvasMode` | `mode` | R | — (`Tab`) |
| `SetUi` | `patch: UiPatch` | R | — (zoom, scroll, panels, bin, snapping, solo, selected layer) |

```rust
pub enum SceneTarget { Project, Take(TakeId) }          // "project" | "take-0001"
pub enum SceneDest { Project, Takes(Vec<TakeId>), AllTakes }
pub enum VideoSourceArg { Screen { output: String }, Camera { device: String } }
pub struct CanvasSpec { pub width: u32, pub height: u32, pub fps: Fps } // "1920x1080@30", "@29.97", "@30000/1001"
pub struct LayerPatch { name, visible, opacity, rect, crop, fit, locked: Option<…> }
pub struct AudioPatch { label, source, gain_db, muted, sync_offset_ms: Option<…> }
pub struct AutocutPatch { auto_run_after_take: Option<bool>, track: Nullable<TrackId>,
                          threshold_db, min_silence_ms, padding_ms, min_keep_ms: Option<…> }
pub struct AutocutOverrides { track: Option<TrackId>, threshold_db, min_silence_ms, padding_ms, min_keep_ms: Option<…> }
pub enum RecordModeArg { Append, Over(Vec<ClipId>), OverSelection }
pub enum InsertAt { End, Index(usize), After(ClipId), AfterSelection }
pub enum MoveTo { Index(usize), By(i64), Before(ClipId), After(ClipId) }
pub enum ClipScope { All, Clips(Vec<ClipId>) }          // the CLI sends All when no flag is given
pub enum SelectOp { Set(Vec<ClipId>), Toggle(ClipId), RangeTo(ClipId), Prev, Next, All, Clear }
pub enum SeekTarget { To(TimeArg), By(TimeArg), EditPrev, EditNext, Start, End }
pub enum ShuttleDir { Reverse, Stop, Forward }
pub enum ExportFormat { Mp4, Webm }                      // default mp4
pub enum Quality { Draft, Standard, High }               // default standard
pub enum VideoCodec { H264, Vp9, Av1 }                   // mp4 ⇒ h264 only; webm ⇒ vp9 (default) | av1; else Usage
pub struct UiPatch { zoom: Nullable<f64>, scroll_px, bin_open, snapping, solo: Nullable<TrackId>,
                     selected_layer: Nullable<LayerId>, panels: Option<BTreeMap<String, f64>> }
```

Scene rules: `LayerAdd`/`AudioAdd` accept only `SceneTarget::Project`
(a take's media is a fact). `LayerRemove`/`AudioRemove` on a take remove
the layer from that snapshot only; the stream stays, and a later
`SceneApply` from the project re-adds layers that have a stream.
`SceneApply` matches by id and copies layout fields only (`rect, crop,
opacity, visible, fit`, z-order of matched layers; `gainDb, muted,
syncOffsetMs` of tracks); it never copies `missing` or sources, and adds
to a take only layers whose stream exists. `AudioRemove` of the track named
by `autocut.track` also sets it to null in the same entry.

### A§4.5 Queries, replies, progress, toasts

```rust
#[serde(tag = "query", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum Query { Status, State, TakeList { #[serde(default)] unused_only: bool }, TakeShow { take: TakeId },
                 ClipList, History { limit: Option<usize> }, Keys }

pub struct CmdResult {
    pub seq: Option<u64>,             // new cursor for undoable changes and undo/redo
    pub created: Vec<String>,         // ids created
    pub removed: Vec<String>,         // ids removed
    pub message: String,              // one human line
    pub result: serde_json::Value,    // command-specific payload (typed views below), or null
}

pub enum Nullable<T> { Keep, Clear, Set(T) }  // custom serde: absent / null / value

pub struct Progress { pub phase: ProgressPhase, pub done: u64, pub total: u64,
                      pub eta_secs: Option<u64>, pub label: String, pub take: Option<TakeId> }
pub enum ProgressPhase { Analyze, Render, Stitch, Record, Finalize }

pub struct Toast { pub level: ToastLevel, pub title: String, pub body: Option<String>, pub actions: Vec<ToastAction> }
pub enum ToastLevel { Info, Success, Warning, Error }
pub struct ToastAction { pub label: String, pub action: UiAction }
pub enum UiAction { OpenPath(String), RevealPath(String), CopyText(String), CopyImage(String), Dispatch(Box<Command>) }
```

Typed result views (in `protocol/reply.rs`, all camelCase):
`StatusView { project, path, initialized, host, pid, record, jobs, takes, clips, durationFrames, cursor }`,
`TakeRow`, `ClipRow { id, take, in, out, start, duration }`, `HistoryRow { seq, ts, label, current }`,
`AutocutPlanView { clips: [{ clip, kept: [[in, out]], removedFrames, silences }], skipped }`,
`ExportDone { path, bytes, frames }`, `FrameSavedView { path }`,
`TakeFinalizedView { take, status, durationFrames, placed, clips }`,
`RecordStartedView { take, mode, files }`.

### A§4.6 Effects

```rust
pub enum Effect {
    // persistence: committed by the shell in the order of A§7.6
    WriteProject,
    WriteUi,                                   // coalesced by the shell (≤ 1 write / 500 ms, no fsync)
    History(HistoryOp),
    CreateTakeDir { path: String },            // mkdir, NOT -p: EEXIST aborts the take
    DeleteTakeDirs { job: JobId, takes: Vec<TakeId> }, // purge only, after the commit (A§5.6)

    // capture (A§8.11)
    SetLive(Option<LiveScene>),                // desired live pipeline (GUI Live mode, recording)
    StartCapture { plan: CapturePlan },        // → CaptureStarted | CaptureFailed
    StopCapture { take: TakeId, cancel: bool },// → CaptureStopped
    // media jobs: argv built in the core (A§9.2)
    Probe { take: TakeId, purpose: ProbePurpose, jobs: Vec<ProbeJob> },      // → TakeProbed
    DetectSilence { job: JobId, key: SilenceKey, argv: Vec<String> },        // → SilenceDetected | SilenceFailed
    RenderExport { job: JobId, plan: ExportPlan },
    SaveFrame { job: JobId, plan: FramePlan },
    CancelJob { job: JobId },
    // time
    StartTimer { timer: TimerId, after_ms: u64 },
    CancelTimer { timer: TimerId },
    // people and requesters
    Notify(Toast),
    Progress { req: ReqId, progress: Progress },
    Reply { req: ReqId, reply: Result<CmdResult, CoreError> },
    Log { level: LogLevel, message: String },
}
pub enum HistoryOp {
    Append(HistoryEntry),                       // drops the redo tail first; cursor = entry.seq
    SetCursor(u64),                             // undo / redo / restore
    Rewrite { entries: Vec<HistoryEntry>, base: Box<Project>, base_seq: u64, cursor: u64, next_seq: u64 },
}
pub enum ProbePurpose { Finalize, Recover }
pub struct ProbeJob { pub id: String, pub file: String, pub kind: StreamKind, pub argv: Vec<String> }
```

### A§4.7 Events

```rust
pub enum Event {
    Capabilities(Capabilities),
    CaptureStarted { take: TakeId, t0_ns: u64, files: Vec<String> },
    CaptureFailed { take: Option<TakeId>, source: Option<String>, message: String },
    StreamLost { take: TakeId, stream: String, message: String },
    RecordClock { take: TakeId, elapsed_ns: u64 },          // ~4 Hz, pipeline clock
    CaptureStopped { take: TakeId, streams: Vec<RecordedStream>, timed_out: bool },
    EngineWedged { message: String },
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
    FrameFailed { job: JobId, message: String, log: String },
    TakeDirsDeleted { job: JobId, deleted: Vec<String>, errors: Vec<String> },
    ProjectChangedOnDisk { project: Result<Box<Project>, String> }, // parsed + migrated by the shell
    RequesterGone { req: ReqId },                                  // IPC client disconnected
}
pub struct RecordedStream { pub id: String, pub file: String, pub start_offset_ns: i64, pub eos: bool }
pub struct ProbeResult { pub id: String, pub file: String, pub info: Result<MediaInfo, String> }
pub struct MediaInfo { pub kind: StreamKind, pub frames: Option<u64>, pub fps: Option<Fps>,
                       pub width: Option<u32>, pub height: Option<u32>, pub sample_rate: Option<u32>,
                       pub channels: Option<u32>, pub duration_ns: Option<i64>, pub start_time_ns: i64,
                       pub color_range: Option<String>, pub color_space: Option<String> }
pub struct Interval { pub start_ns: i64, pub end_ns: i64 }   // half-open, file time
```

### A§4.8 Errors and exit codes

`CoreError { code, message, hint: Option<String>, details: Option<Value> }`.
`details` carries structured data for confirmations
(`{files:[{path,bytes}], historyEntriesLost, oldestKept, clips:[{id,before,after}], removedClips}`).

| Code | Exit | Used for |
| --- | --- | --- |
| `usage` | 2 | bad arguments, clap errors |
| `not_project` | 3 | folder rule ("Not my swamp: …") |
| `confirmation_required` | 4 | `confirmation required: <what is lost>` |
| `missing_dependency` | 5 | a required tool or element is missing |
| `busy` | 6 | recording/finalizing/exporting, lock held by an unresponsive host |
| `not_found` | 7 | bad id, unknown output/device |
| `invalid` | 1 | impossible edit, invalid project, not recording, already a project |
| `capture_failed` | 1 | health check or start failure; hint names the source |
| `unavailable` | 1 | needs the GUI host, no host holds a selection |
| `io`, `internal` | 1 | |

### A§4.9 Wire format (IPC and `--json`)

One request per connection, JSON lines, `v: 1`:

```
client → host  {"v":1,"id":"7","confirmed":false,"body":{"command":{"cmd":"clipSplit","clip":"c7f3","at":"13f"}}}
               {"v":1,"id":"8","confirmed":false,"body":{"query":{"query":"takeList","unusedOnly":true}}}
               {"v":1,"id":"9","body":"hello"}        {"v":1,"id":"10","body":"shutdown"}
host → client  {"id":"7","accepted":true}
               {"id":"7","progress":{"phase":"render","done":1234,"total":5678,"etaSecs":42,"label":"Rendering clip 4/12"}}
               {"id":"7","reply":{"ok":true,"data":{"seq":12,"created":["c9b2"],"removed":[],"message":"…","result":null}}}
               {"id":"7","reply":{"ok":false,"error":{"code":"not_found","message":"…","hint":"…"}}}
```

`hello` → `{pid, projectDir, host: "gui"|"headless"|"cli", record: RecordPhase, jobs, version}`.
`shutdown` → the host stops accepting, lets jobs finish, exits (A§8.6).
The shell's `ReqId(u64)` is allocated per request by the Session; client
ids stay inside the IPC layer. `--json` prints `to_wire(&result)` — the
same `{"ok":…}` object — and streaming commands print progress lines
(`{"event":"progress",…}`) before it. Toasts never go to stdout: humans get
them on stderr, `--json` gets `{"event":"notice",…}` lines on stderr.

---

## A§5 Core behavior

### A§5.1 Commit helper (`history/commit.rs`, foundation)

Every handler builds the next `State` and finishes through one helper:

```rust
pub fn commit(prev: &State, ctx: &Ctx, label: String, cmd: serde_json::Value, next: State) -> Committed;
pub fn persist(prev: &State, next: State) -> Committed;   // non-undoable changes (facts, UI)
pub struct Committed { pub state: State, pub effects: Vec<Effect>, pub seq: Option<u64> }
```

`commit` runs `ui_normalize`, diffs the undoable views (A§7.2); if they're
equal it appends nothing; otherwise it appends an entry and emits
`History(Append)` + `WriteProject` (+ `WriteUi` if the UI changed).
**Facts first:** a step that changes facts *and* makes an edit (take
finalization, recovery) first applies the facts with `persist`, then the
edit with `commit`, so a patch never contains facts (`missing`, statuses,
streams). Handlers never emit `WriteProject` or `History` by hand.

### A§5.2 Timeline operations (`timeline.rs`)

Pure functions over `&[Clip]` and the take pool; positions are prefix sums.
- `position_of`, `clip_at(timeline_frame) -> Option<(index, offset)>`,
  `total_frames`, `edit_points`, `is_contiguous(ids)`.
- `split(clips, i, take_frame, new_id)`: needs `in < p < out`; the left part
  keeps the id. **Property:** timeline frame → (take, take frame) mapping is
  unchanged, and `plan_export`/`plan_ges` are unchanged (A§9.3).
- `trim(clips, i, new_in, new_out, take_duration)`: values outside the take
  are clamped and reported; an empty result is `Invalid`.
- `move_block(clips, ids, to)`: the clips move as one block in original
  order; `Index(i)` indexes the list without the block; `By(n)` shifts the
  block's first index (clamped); `Before/After(c)` anchors on a clip outside
  the block. **Property:** the result is a permutation.
- `remove`, `insert`, `replace_run`.
- Clip time arguments are clip-relative unless `timeline`: absolute
  `start = T` ⇒ `in + T`; absolute `end = T` ⇒ `in + T`; relative `±T` moves
  that edge. With `timeline`, `T` is a timeline position inside the clip.
  `FrameSave.at` and `SeekTarget::To` are timeline frames; relative values
  are only valid for `seek --by` and trim edges (else `Usage`).

### A§5.3 Recording state machine

While `record != Idle`, `apply` accepts only the commands marked R in A§4.4
(`RecordStop/Toggle/Cancel`, `TakeDiscardLast` in Finalizing, `Select`,
`Seek`, `Pause`, `SetCanvasMode`, `SetUi`) and queries; everything else,
including `Play`, `Undo` and `Export`, is `Busy` (SPEC §5.3.3).

| Phase | Input | Result |
| --- | --- | --- |
| Idle | Start/Toggle | `Busy` if an export runs; `Invalid` if the scene is empty; Over needs contiguous existing clips (stored as `ClipRef`s; `OverSelection` resolves `ui.selection` and is `Unavailable` without a selection). countdown > 0 ⇒ Countdown + `StartTimer`; else begin take ⇒ Starting |
| Idle | Stop/Cancel | `Invalid` "not recording" |
| Countdown | TimerFired (current timer) | next tick, or begin take ⇒ Starting |
| Countdown | Stop/Toggle/Cancel | `CancelTimer` ⇒ Idle; start req gets `Invalid` "countdown cancelled"; caller gets Ok |
| Starting | CaptureStarted | ⇒ Recording; reply start req `RecordStartedView` (unless `wait`) |
| Starting | CaptureFailed | take `broken` (+`error`) ⇒ Idle; reply start req `capture_failed` |
| Starting | Stop/Toggle | `StopCapture{cancel:false}` ⇒ Finalizing; start req gets its started reply |
| Starting / Recording | Cancel | `StopCapture{cancel:true}` ⇒ Finalizing(cancelled) |
| Recording | Stop/Toggle | `StopCapture{cancel:false}` ⇒ Finalizing; the caller waits for the finalized reply |
| Recording | RecordClock | update `elapsed_ns`; `Progress{phase:Record}` to `wait` requesters (≥ 500 ms apart) |
| Recording | StreamLost | mark layer/track `missing` (persist); last stream lost ⇒ act as Stop |
| Finalizing | Stop | join the stop waiters; Start/Toggle/Cancel ⇒ `Busy` "finalizing take N" |
| Finalizing | TakeDiscardLast | remember `discard` (applied after placement, A§5.5) |
| Finalizing | CaptureStopped | `timed_out` ⇒ `broken`; else record offsets, `Probe{Finalize}` |
| Finalizing | TakeProbed | see below ⇒ Idle |
| any | EngineWedged | take `broken`; future starts fail with "the capture engine is stuck (GPU driver?); restart Qrec" |

**Begin take:** allocate the take number (A§4.3); insert the take
(`status: recording`, scene snapshot, planned streams) with `persist`;
effects `CreateTakeDir`, `WriteProject`, then `SetLive` (if needed) and
`StartCapture(plan)`.

**TakeProbed (Finalize), in order:** fill media info, mark unreadable
streams `missing` (persist); zero readable streams ⇒ `broken` + toast;
`durationFrames < 10` ⇒ `tooShort`, bin, toast "That was shorter than Lord
Farquaad. Take moved to the bin."; cancelled ⇒ `ok`, bin, no clip; otherwise
`ok` and place it with `commit` ("Record take N",
`cmd: {"event":"takeFinalized","take":"take-0003"}`): Append adds
`[0, durationFrames)` at the end; Over replaces the run if every `ClipRef`
still matches and is contiguous (recording `Take.replaced`), else appends
with a toast "record-over target changed; take appended at the end". Set
`Take.placed`. Then the pending discard (A§5.5), the ready autocut jobs
(A§5.7), then `autocut.autoRunAfterTake` starts a job for the new clip. Reply
to every stop request and waiter with `TakeFinalizedView`.

**Recovery** (`Session::open` with the lock): each take still `recording`
gets `Probe{Recover}`; readable ⇒ `ok`, bin, toast "Recovered take 7 from
the bog."; otherwise `broken`.

**CaptureFailed vs StreamLost** (engine contract, A§8.11): after
CaptureStarted, an error attributable to one branch (its source, encoder,
muxer or filesink) EOSes that branch and emits `StreamLost`. `CaptureFailed`
is for the start/health window and pipeline-wide errors.

### A§5.4 Scene and project commands

`Init` (`Invalid` "already a project" if one exists) uses `facts.config` and
`facts.folder_name`. `ProjectSet` with an fps change and takes needs
confirmation listing every affected clip; re-quantization (`requantize.rs`):
for adjacent clips with the same take and `out == next.in`, the shared
boundary is mapped once with round-to-nearest; other `in`s use floor, other
`out`s ceil; clamp to the new `durationFrames`; clips that end up empty are
removed in the same entry and listed separately. Take `durationFrames` are
re-derived from streams. Undo restores the bounds through the patch and
re-derives durations.

### A§5.5 Discard last (`d`)

Target: the highest-numbered take with `placed == true` (tooShort, broken,
cancelled and still-recording takes are skipped). If clips reference it,
remove them all in one entry "Discard take N"; if it was a record-over and
none of `Take.replaced`'s clip ids is on the timeline, those clips are put
back at the position of the first removed clip in the same entry. If no clip
references it: `NotFound` "Take N is already in the bin" — `d` never reaches
older takes. In Finalizing the discard is applied right after placement
("Record take N" then "Discard take N", both undoable); if the take wasn't
placed, reply Ok "take N was not placed".

### A§5.6 Purge

Only takes that no clip references and that aren't `recording`; `Busy` while
any job's `files` include a file of the take. Confirmation `details`: the
files with sizes (`facts.take_files`), the number of history entries lost and
the oldest surviving label. Effects in order: `History(Rewrite)` (A§7.5),
`WriteProject`, then `DeleteTakeDirs` — the shell deletes exactly
`takes/<takeId>/` (O_NOFOLLOW, regular files only, then rmdir) only after the
commit is durable. Not undoable.

### A§5.7 Autocut

Plan (pure, `autocut.rs`), per clip and track:
1. Silences arrive in **file time**; shift to take time
   `t = x + startOffsetNs + syncOffsetMs·10⁶`.
2. Shrink each by `paddingMs` on both sides; drop empty ones.
3. Kept = clip range minus shrunk silences.
4. Drop kept fragments shorter than `minKeepMs`.
5. Snap outward to frames (`in = ns_to_frame_floor`, `out = ns_to_frame_ceil`),
   clamp to the clip, merge touching fragments.
6. Nothing kept ⇒ the clip is left unchanged and reported "entirely silent at
   −35 dB" (autocut never deletes a whole clip).

Track: override → `autocut.track` → first `mic` of the take scene → first
track; a take without it is skipped (warning in the result).

Flow: `Autocut` snapshots params and targets (`AutocutTarget` with a
`ClipRef`), emits `DetectSilence` for keys not in `runtime.silence`
(argv from `media_argv`), and completes when all keys are known:
- If `record != Idle`, mark the job `ready` and apply after placement.
- Apply only to targets whose current clip equals its `ClipRef`; others go to
  `result.skipped` ("changed while analyzing").
- One history entry for the whole command ("Autocut take 3: removed 4
  silences (6.2 s)"); the first fragment keeps the clip id; if nothing
  changes, no entry. `dryRun` returns `AutocutPlanView` without applying.
Properties: kept ⊇ every padded-speech region of a fragment ≥ `minKeepMs`;
fragments stay inside the clip; a clip without silence is unchanged.

### A§5.8 Jobs

One export at a time (`Busy`); `ExportCancel{job: None}` cancels it;
`FrameSave` may run alongside. Each job holds what it reads (`files`) and
its own plan (a snapshot), so editing continues during an export
(SPEC §13.1). Export progress = `0.9·Σseg_done/total + 0.1·stitch`,
ETA from `ctx.now − started`, `label` "Rendering clip 4/12" / "Stitching".
`RequesterGone`: clear `job.req`; in Headless/Cli hosts cancel the jobs that
requester started if no other requester waits; never stop a recording.

### A§5.9 Export and frame-save commands

`out` from the CLI is already absolute (resolved against the CLI's cwd).
`facts.out.forbidden` (inside `takes/`, `.qrec/` or `qrec.json`) ⇒ `Invalid`
(not confirmable); `exists && !confirmed` ⇒ `confirmation_required`.
Auto names: `exports/<stem>-YYYYMMDD-HHMMSS.<ext>` and
`frames/<stem>-<take>-<HHMMSSFF>.png` (timeline timecode) with local time
from `utc_offset_secs` and `stem = naming::file_stem(project.name)`.
Timeline empty ⇒ `Invalid` "nothing to export". Done toast for export:
"Layers! Like an onion. Your video is ready." with Open file / Open folder /
Copy path; frame toast: path with Copy image / Open folder.

### A§5.10 UI normalization (`ui_normalize.rs`)

`ui_normalize(prev: &Project, next: &Project, ui: &UiState) -> UiState` runs
in every `commit`/`persist`: drop selection ids not on the timeline and sort
the rest by index; after a record-over select the new clip; when autocut
replaced a selected clip select all its fragments; clear `selected_layer` and
`solo` when they no longer exist in the scene `canvas_mode` targets; on an
fps change convert `playhead` and `zoom` through nanoseconds and clamp the
playhead to the timeline length.

---

## A§6 Time (`time.rs`, done)

- `frame_ns(f) = floor(f·10⁹·den/num)` — identical to GStreamer's
  `gst_util_uint64_scale`. Rounding made GES show the previous clip's frame
  at about one cut in three *(measured)*.
- `ns_to_frame_floor(t)`: the `f` with `frame_ns(f) ≤ t < frame_ns(f+1)`;
  `ns_to_frame_ceil(t)`: the smallest `f` with `frame_ns(f) ≥ t`.
- Range durations are `frame_ns(end) − frame_ns(start)`; never sum ns.
- `TimeArg`: `412f`, `13.7s`, `00:00:13:21`, `00:00:13.700`, optional `+`/`-`;
  a bare number is a usage error with a hint. Timecodes use the nominal rate.

---

## A§7 History and undo

### A§7.1 Entries and seqs

```rust
pub struct HistoryEntry {
    pub seq: u64, pub ts: DateTime<Utc>, pub label: String,
    pub cmd: serde_json::Value,           // the Command, {"event":…} or {"external":true}
    pub forward: json_patch::Patch, pub inverse: json_patch::Patch,
    pub after_hash: String,               // informational: hash of the view after (shown by `qrec history`)
}
```

Append: `seq = next_seq; next_seq += 1` (never reused) and the redo tail
(entries after the cursor) is dropped. Undo/redo move by **position** in the
entry list. `Undo N`/`Redo N` are all-or-nothing: every step is computed
first; a failure returns `Invalid` naming the failing seq and changes
nothing. `CmdResult.seq` for undo/redo is the new cursor. Restore accepts
`base_seq` or an existing seq, else `NotFound`.

### A§7.2 Undoable view and diff

`undoable_view(&Project) -> Value`: `qrec.json` without `version`,
`createdAt`, `nextTakeNumber`, every take reduced to `{scene}`, and every
optional member written explicitly (`locked: false`, `missing: false`) so
patches never add or remove object members. Patches are applied to
`project_value(&Project)` (serde value with the same explicit members),
never to file text, then deserialized and validated.

Diff (`history/diff.rs`, custom, not `json_patch::diff`):
- Both views are built over the same set of takes (takes created by the step
  are copied into "before"; takes gone from "after" are dropped from both),
  so patches only touch `/takes/<id>/scene/…`.
- Arrays of objects with `id` (`timeline.clips`, `scene.layers`,
  `scene.audio`) are diffed by id with an LCS: removed ids ⇒ `test`+`remove`,
  inserted ⇒ `add` at the final index, moved ⇒ remove + add; matching ids are
  diffed field by field. Other arrays are replaced whole.
- Each `replace`/`remove` is **immediately preceded** by a `test` of the value
  at that path in the intermediate document (ops are generated by applying
  them one by one to a scratch copy). A failed test ⇒ the state drifted ⇒
  undo refused with `Invalid` instead of corrupting anything.
- Property: patch size grows with the number of changed clips, not with the
  timeline length; undo∘apply = id and redo∘undo = id over random command
  sequences, with every test op passing.

### A§7.3 Compaction, clear, restore

Above 5,000 entries the oldest are dropped down to 4,000; `history.base.json`
becomes the project before the oldest kept entry (walking inverses back from
the current state) and `base_seq` its seq. `Clear` (`!`): no entries, base =
current project, `base_seq = cursor`. `Restore <seq>` (`!`) = undo/redo to it.

### A§7.4 External edits (merge, not adoption)

On `ProjectChangedOnDisk(Ok(file))` the core merges:
- From the file only the undoable fields: `name, canvas, scene, recording,
  autocut, timeline, takes[*].scene`.
- `nextTakeNumber = max(file, current)`; takes missing from the file are kept
  (warning: "takes can only be removed with `qrec take purge`"); takes only
  in the file are added; for takes in both, the current `status, streams,
  durationFrames, recordedAt, error, placed, replaced` win (warning listing
  ignored pointers); the in-flight take is always kept as in memory.
- Re-derive durations if fps changed, validate the merged project, append an
  "External edit" entry (diff of views) and emit `WriteProject` of the merge.

`ProjectChangedOnDisk(Err(msg))` sets `runtime.disk_invalid` (A§7.7).

### A§7.5 Purge rewrite

States `S_0` (base) … `S_n`, `S_i` after entry `e_i`. Let
`k = max{i ≤ cursor : S_i references T}` (timeline only). Drop `e_1 … e_{k+1}`;
the new base is `S_{k+1}` with T removed from its pool (it can't reference T,
since the current state doesn't). In the redo tail let
`j = min{i > cursor : S_i references T}` and drop `e_j … e_n`. In kept
entries remove ops under `/takes/T/…` with their guarding tests; entries left
empty are dropped; recompute `after_hash`. If no state references T, only the
`/takes/T` ops are removed. Emitted as `HistoryOp::Rewrite`.

### A§7.6 Persistence order and the commit record

`.qrec/history.head.json` is the commit record:
`{"cursor":N,"baseSeq":B,"nextSeq":K,"projectSha256":H,"project":{…}}`
(there is no `history.cursor` file). Per outcome, in order:
1. History: `Append` = one line via `O_APPEND` + `fdatasync`; redo-tail drop
   and `Rewrite` = tmp + rename (and `history.base.json`).
2. `qrec.json`: tmp → fsync → `rename` → fsync(dir).
3. `history.head.json`: tmp → rename. **This is the commit point.**
4. Only then the effects that touch the world: `StartCapture`,
   `DeleteTakeDirs`, `Reply`, `Notify`, jobs.

Non-undoable writes do steps 2–3. On open (lock held): drop history lines with
`seq ≥ nextSeq`; drop a torn last line; a bad line elsewhere ⇒ `Invalid`
with its line number. If `sha256(qrec.json) == H` the state is clean;
otherwise a valid file is an external edit against `head.project` (covers a
crash between 2 and 3); an unparsable or invalid one takes A§7.7.

### A§7.7 Invalid or corrupt `qrec.json`

- Host running: `runtime.disk_invalid = Some(msg)`; user mutating commands
  return `Invalid` "qrec.json on disk is invalid: <msg>; fix it or run `qrec
  history restore <cursor> --yes`"; queries work; a later valid read clears
  it. Writes that must save facts (begin take, finalization, recovery) first
  move the invalid file to `.qrec/rejected/qrec-YYYYMMDD-HHMMSS.json` (never
  overwritten) and say so in a toast.
- Opening: a corrupt file makes `Session::open` fail with `Invalid` and that
  hint, except with `OpenOpts { recovery: true }` (used by `qrec history` and
  `qrec history restore`), which loads `history.head.json` + history and never
  reads `qrec.json`. Restore copies the corrupt file to
  `.qrec/qrec.corrupt-<ts>.json` before writing. If the head file is missing
  too, rebuild from `history.base.json` + forward patches and re-add takes
  found under `takes/` through the Recover probe.

---

## A§8 Shell

### A§8.1 Threading model

- In every host the `Session` lives on **one session thread** draining a
  `crossbeam_channel::Receiver<Msg>`; it never blocks on I/O other than its
  own small atomic writes.
  ```rust
  pub enum Msg { Request { req: Request, to: Box<dyn Responder> }, Event(Event),
                 DiskChanged, Interrupt, Shutdown }
  #[derive(Clone)] pub struct Inbox(crossbeam_channel::Sender<Msg>);
  pub trait Responder: Send { fn accepted(&mut self); fn progress(&mut self, p: &Progress);
                              fn reply(&mut self, r: Result<serde_json::Value, CoreError>); }
  ```
- Workers are std threads that only talk to the session through `Inbox`:
  process runners, timers, the inotify watcher, IPC connections, and the
  capture engine thread. No tokio; ashpd runs under `futures_lite::block_on`
  on the capture thread. GStreamer bus messages reach the engine thread via
  `bus.set_sync_handler`, so no GLib main loop is needed outside the GUI.
- The GUI (later) runs the session thread too and receives `Arc<State>`
  snapshots on the GTK main context.

```rust
pub struct Deps {
    pub capture: Box<dyn CaptureEngine>,          // real, fake, or none
    pub media: Box<dyn MediaRunner>,
    pub clock: Box<dyn FnMut() -> (DateTime<Utc>, i32) + Send>,
    pub entropy: Box<dyn FnMut() -> u64 + Send>,
}
impl Session {
    pub fn open(dir: &Path, opts: OpenOpts, deps: Deps) -> Result<Session, ShellError>;
    pub fn inbox(&self) -> Inbox;
    pub fn handle(&mut self, msg: Msg);
    pub fn run_until(&mut self, done: impl FnMut(&State) -> bool);   // CLI and hosts
    pub fn state(&self) -> &State;
}
pub struct OpenOpts { pub host: HostKind, pub mutating: bool, pub recovery: bool }
pub trait MediaRunner: Send {
    fn probe(&mut self, take: TakeId, purpose: ProbePurpose, jobs: Vec<ProbeJob>, out: Inbox);
    fn detect_silence(&mut self, job: JobId, key: SilenceKey, argv: Vec<String>, out: Inbox);
    fn render_export(&mut self, job: JobId, plan: ExportPlan, out: Inbox);
    fn save_frame(&mut self, job: JobId, plan: FramePlan, out: Inbox);
    fn delete_take_dirs(&mut self, job: JobId, takes: Vec<TakeId>, out: Inbox);
    fn cancel(&mut self, job: JobId);
}
```

`Session::handle(Request)`: allocate `ReqId`, `facts_needed` → gather facts →
`apply` → run effects (A§7.6 order) → `Responder`. Events go to `on_event`.
Fake `Deps` make the Session testable without GStreamer or ffmpeg.

### A§8.2 Paths (`paths.rs`)

Runtime dir: `$QREC_RUNTIME_DIR`, else `$XDG_RUNTIME_DIR/qrec`, else
`/tmp/qrec-<uid>`; created 0700, then `lstat`-checked (real dir, our uid,
0700) or refused. Config `$XDG_CONFIG_HOME/qrec`, state
`$XDG_STATE_HOME/qrec` (portal tokens 0600, `last-session.log`), cache
`$XDG_CACHE_HOME/qrec` (capabilities). Socket:
`<runtime>/<sha256(canonical project dir) hex>.sock`; refuse paths over 107
bytes.

### A§8.3 Lock and socket protocol

Every process that holds the project lock is a **host** (Gui, Headless or
Cli) and serves the socket.
1. **Start:** open `.qrec/lock` (O_CLOEXEC), `try_lock` (exclusive, never
   blocking). Held ⇒ route (step 4). Under the lock: write
   `{pid, kind, socket, since, state:"starting"}` to the lock file, unlink
   `<hash>.sock`, bind `<hash>.sock.<pid>`, listen, `rename` it to
   `<hash>.sock` (atomic publish), set `state:"ready"`. This happens before
   recovery and `gst::init()`; early requests queue in the inbox. The host
   then opens its Session with a second descriptor of the lock (`dup`, same
   open file description): a second `open` + `flock` in the same process
   would be refused, and dropping the Session only closes its copy.
2. Only the lock holder unlinks its socket. Scanners never delete anything.
3. **Exit:** `state:"stopping"` → stop accepting → unlink the socket →
   flush replies → exit (closing the lock fd releases the lock). Routing
   clients treat `starting` and `stopping` alike: retry.
4. **CLI routing** (deadline 3 s; 20 s for `record *`): connect. On
   ENOENT/ECONNREFUSED try the lock; if acquired, become the host (in-process,
   or hand off to a spawned headless host). If not, read the lock file: when
   `ready`, connect to its socket; otherwise retry every 50 ms. `hello` timeout
   2 s ⇒ `Busy` "host pid N is not responding"; deadline ⇒ `Busy` naming pid,
   kind and start time.
5. **Spawn hand-off** (`record start`/`toggle` with no host): the CLI takes
   the lock and spawns `qrec host --headless --project DIR --lock-fd 3
   --ready-fd 4` (fds dup'ed in `pre_exec`, CLOEXEC cleared, `setsid`, stdio
   to `.qrec/logs/…-host.log`). flock belongs to the open file description,
   so the child keeps it; the CLI closes its fd and **never unlocks**. The
   host writes one JSON line on fd 4 (`{"ok":true}` once listening, or the
   `CoreError`); EOF ⇒ the CLI reports the host log path.
6. **Protocol safety:** the host sends `{"id","accepted":true}` once parsed.
   EOF before `accepted` ⇒ re-route once; EOF after ⇒ exit 1 "host
   disconnected" with the log path (never retry: it may have run).
7. Peers must have our uid (`SO_PEERCRED`). Request lines are capped at 1 MiB
   with a 5 s read timeout; each connection has a bounded 64-message queue
   (progress coalesced, replies never dropped) and a 10 s write timeout.

### A§8.4 Project resolution and init

`--project DIR` → cwd if it holds `qrec.json` or is strictly empty → if an
ancestor holds `qrec.json`, exit 3 with "you're inside project <dir>; run
there or pass --project <dir>" → the single live host found by `hello` on
`<runtime>/*.sock` (stderr: `qrec: using project <dir> (pid N)`) → exit 3
(several hosts: `Usage` listing them). Init order: `mkdir .qrec` → lock →
re-check the folder rule (a folder whose only entry is `.qrec/` counts as
empty) → write `qrec.json` + head → `mkdir takes`. Read-only commands take no
lock and write nothing (no migration backups either). `--over-selection` with
no live host ⇒ `Unavailable`.

### A§8.5 In-process commands

With no host, a command runs in-process and its process **is** the host
(`HostKind::Cli`) until it is done: short commands finish immediately;
async ones (export, frame save, autocut with analysis, purge) keep serving
IPC so other commands run against the same Session (SPEC §13.1 "editing can
continue"). `seek`/`play`/`pause`/`shuttle` without a GUI host ⇒
`Unavailable`.

### A§8.6 Hosts: exit, signals, GUI start

- Exit (Headless and Cli): once Idle + no running jobs + no open connections
  has held for 2 s, or (Headless) 60 s after `ready` without a request.
  `shutdown` requests: stop accepting, let jobs finish, exit.
- Signals in every host: the first SIGTERM/SIGINT/SIGHUP dispatches
  `RecordStop` (finalize, not cancel) and cancels jobs; exit when idle or
  after 15 s. A second signal: SIGKILL child process groups, `_exit(130)`;
  startup recovery handles the take.
- Client CLI: SIGINT/SIGTERM (signal-hook) → `Msg::Interrupt`. In-process: the
  first cancels running jobs (SIGTERM to each child group, SIGKILL after 2 s),
  deletes temp outputs, exits 130; the second SIGKILLs and exits 130.
  IPC-routed: Ctrl+C closes the connection and exits 130; for `record --wait`
  the first Ctrl+C sends `RecordStop` on a second connection and keeps
  streaming until finalized, a second detaches. Stdout is written with
  `writeln!`; `BrokenPipe` exits quietly.
- GUI start with a live non-GUI host: recording ⇒ SPEC §4.3 modal (Stop &
  open: `RecordStop`, wait for its reply, `shutdown`, wait ≤ 20 s for the
  lock); running job ⇒ "An export from the CLI is running (43 %)" with Wait
  and Quit; idle ⇒ `shutdown` and wait. The GTK app uses
  `ApplicationFlags::NON_UNIQUE` and `run_with_args(&[])` after clap.

### A§8.7 Process runner (`proc.rs`) and logs

One runner for every child: stdin null, `process_group(0)`, stdout piped only
when parsed (drained on its own thread), stderr appended to a log file.
Timeouts: ffprobe 20 s, pactl 3 s, VA/encoder probes 10 s, `wl-copy` 5 s
(`--type image/png`, stdin from the PNG file). Long ffmpeg jobs have a stall
watchdog (no `-progress` block for 60 s ⇒ kill, "ffmpeg stalled"). Kill:
SIGTERM to the group, SIGKILL after 2 s, wait ≤ 2 s more, then hand the
child to a reaper thread and fail the job — the session thread never blocks in
`wait()`. Success = exit 0 and `progress=end` (and, for segments, the counts
in A§9.3). Logs: `.qrec/logs/YYYYMMDD-HHMMSS-<pid>-j<job>-<step>.log`
(O_EXCL); at host start prune to the newest 200 and delete
`*.qrec-part-*` temp files older than 1 h in `exports/`, `frames/`,
`.qrec/cache/segments/`.

### A§8.8 Output files: never overwrite

Temp outputs live in the destination directory as
`.<stem>.qrec-part-<pid>-<job>.<ext>`; the final step is
`renameat2(RENAME_NOREPLACE)` (fallback `link` + `unlink`). Auto names loop
`-2`, `-3`, … on EEXIST; a hand-picked unconfirmed `--out` that appeared
meanwhile ⇒ keep the temp, fail `confirmation_required` naming both paths.
`ExportFinished`/`FrameSaved` carry the real path. Migration backups use the
same no-replace rule.

### A§8.9 External edits watcher

Watch the project **directory** (`IN_CLOSE_WRITE|IN_MOVED_TO|IN_CREATE|IN_DELETE`),
filter on `qrec.json`, debounce 200 ms, then compare the bytes' sha256 with
the hash of what the host last wrote: equal ⇒ ignore (our own write);
different ⇒ parse + migrate + post `ProjectChangedOnDisk`. Just before each
`qrec.json` rename the host re-checks the file against that hash; if it
changed, the external edit is processed first and the request re-dispatched
against the new state (no effect of the old outcome has run yet).

### A§8.10 GPU safety and GStreamer init

- At the very top of `main()`, before any thread or GTK/GStreamer call
  (edition 2024 `set_var` is only sound there): resolve the effective VA node
  (A§2.1, reading the project's `qrec.json` if `--project`/cwd has one), then
  set `LIBVA_DRIVER_NAME` from its PCI vendor (`0x8086` ⇒ `iHD`, `0x1002` ⇒
  `radeonsi`), overriding an inherited mismatching value (logged; `doctor`
  reports it). Leave it alone only when an NVIDIA node is explicitly
  configured. When an NVIDIA device exists and isn't chosen, also set
  `__EGL_VENDOR_LIBRARY_FILENAMES` (Mesa) and `VK_DRIVER_FILES` /
  `VK_ICD_FILENAMES` (chosen vendor) unless the user set them (GUI).
- Commands that only need ffmpeg never initialize GStreamer.
- `gst::init()` runs on a helper thread; the caller waits ≤ 30 s, then prints
  "GStreamer is stuck scanning plugins (a GPU driver is probably hung); see
  `qrec doctor`" and `process::exit(5)`. Never on the GTK main thread.
- The VA H.264 element is found by enumerating `va*h264enc` factories and
  matching each one's `device-path` to the node — never by name (the first
  usable node gets plain `vah264enc`).
- Probes that touch a render node (the h264_vaapi check, doctor) run as
  child processes with a 10 s timeout.
- The Nix package ships gst-plugins-bad without `nvcodec` and `qsv`, which
  hang the plugin scan on the dev machine.

### A§8.11 Capture engine

```rust
pub trait CaptureEngine: Send {
    fn set_live(&mut self, live: Option<LiveScene>, out: Inbox); // build/update the live pipeline; idempotent
    fn start(&mut self, plan: CapturePlan, out: Inbox);           // attach record bins at T0
    fn stop(&mut self, take: &TakeId, cancel: bool);              // EOS record bins, detach
    fn shutdown(&mut self);
}
pub struct LiveScene { pub canvas: Canvas, pub scene: Scene, pub preview: bool }
```

All GStreamer calls happen on one engine thread. `CaptureStarted` is sent only
after every branch's filesink got its first buffer (≤ 3 s, else
`CaptureFailed{source}`); the 500 ms health check catches early bus errors.
Stop: EOS within 10 s, teardown within 5 s; a missed teardown ⇒
`EngineWedged` (a headless host then exits via `process::exit`). Level meters
and the preview paintable go to the GUI over a side channel, never through
`on_event`. `record start` while the GUI isn't in Live mode starts the
pipeline first. Paths in the plan are joined with the project dir.

**Record bins** *(measured)*: T0 = the pipeline running time chosen at start;
a buffer probe on each bin's sink pad drops buffers with PTS < T0 and
`startOffsetNs = first passed PTS − T0`; T1 = running time at stop, video is
filled to T1 before EOS.
- Video: `queue ! videorate skip-to-first=true ! video/x-raw,framerate=N/D !
  videoconvert ! video/x-raw,format=I420|NV12,colorimetry=bt709 ! <encoder>
  ! h264parse ! matroskamux offset-to-zero=true ! filesink`. Without
  `skip-to-first`, a bin attached after N s of Live mode starts with N s of
  frozen frame.
- Audio: `queue ! audioconvert ! audioresample ! audiorate skip-to-first=true
  ! audio/x-raw,rate=48000 ! flacenc ! matroskamux offset-to-zero=true !
  filesink`.
- `pipewiresrc keepalive-time=<floor(1000·den/num)>` so a static screen still
  produces frames to the end (screencasts are damage-driven).
- Encoders: `x264enc pass=qual quantizer=18 speed-preset=veryfast
  tune=zerolatency key-int-max=<round(fps)>` (without `pass=qual` x264enc
  records 2 Mbit/s CBR); VA: the resolved factory with `rate-control=cqp
  qpi=18 qpp=18 qpb=18 key-int-max=<round(fps)>`.
- Cameras: the closest mode ≥ canvas fps, preferring the largest raw/MJPEG
  size (rule in `capture_plan`, applied by the engine from `CameraInfo.modes`).
- Portal: one ScreenCast session per screen layer, `persist_mode = 2`, restore
  tokens in `<state>/portal-tokens.json` (0600) keyed by output name and
  matched to streams by position and size; every process that talks to the
  portal first registers the app id `com.rsensato.qrec` (host Registry,
  xdg-desktop-portal ≥ 1.19).
- `QREC_CAPTURE_BACKEND=fake`: `videotestsrc is-live=true` /
  `audiotestsrc is-live=true` (a tone with periodic silence), never touches the
  portal, and fakes discovery: screens `FAKE-1`, `FAKE-2` (1280×720), camera
  `/dev/fake0`, mic `alsa_input.fake`, monitor `alsa_output.fake.monitor`.

### A§8.12 Sources and doctor

```rust
pub struct SourceInventory { pub screens: Vec<ScreenInfo>, pub cameras: Vec<CameraInfo>,
                             pub mics: Vec<AudioDeviceInfo>, pub monitors: Vec<AudioDeviceInfo>,
                             pub default_mic: Option<String>, pub default_monitor: Option<String> }
pub struct DoctorReport { pub checks: Vec<Check>, pub niri_snippet: String, pub va: Option<VaReport> }
pub struct Check { pub id: String, pub required: bool, pub ok: bool, pub detail: String, pub hint: Option<String> }
```

Screens from `wl_output` v4 `name`/`description` + geometry (fallback
`niri msg --json outputs`); cameras via GStreamer's DeviceMonitor
(`Video/Source`, no capture); audio via `pactl --format=json` (mics are
`alsa_input.*` only; monitors are `*.monitor`). `doctor` checks every SPEC §3.3
item (+ `pactl`), prints a hint per failure and the niri bind snippet
(SPEC §10.2); exit 5 when a required check fails; the GUI's "The swamp is
missing something" screen renders the same report.

---

## A§9 Planners (pure, snapshot-tested with `insta`)

### A§9.1 Layer placement (`layout.rs`, done)

`place(src_w, src_h, crop, rect, fit) -> Option<Placement { src, dst }>`:
the cropped source region (even-aligned) and the canvas rect it is scaled
into (even size, even position: ffmpeg floors odd overlay positions, GES
rounds them up *(measured)*). `contain` letterboxes inside `rect`; `cover`
crops the source (centered) to the rect aspect. `None` when nothing would be
visible. Export, frame save, GES and the GUI canvas use the values verbatim.

### A§9.2 The argv contract (`media_argv.rs`)

Every ffmpeg/ffprobe command line is built in the core (SPEC §3.1):
- `argv[0]` is `ffmpeg` or `ffprobe`; ffmpeg argv start with
  `-hide_banner -nostdin -nostats -loglevel info -y`, plus `-progress pipe:1`
  for jobs that report progress.
- The **output path is the last element**, preceded by an explicit muxer
  (`-f mov|mp4|webm|matroska`, or `-f image2 -update 1` for PNG); every path
  is prefixed with `file:`. The runner may replace only that last element
  (with its temp path).
- `probe_argv(file, kind)`: video `ffprobe -v error -select_streams v:0
  -count_packets -show_entries stream=nb_read_packets,r_frame_rate,width,height,pix_fmt,color_range,color_space:format=start_time
  -of json`; audio adds `sample_rate,channels,duration`. Parsers live in
  `parse/ffprobe.rs` (pure).
- `silence_argv(file, threshold_db, min_silence_ms)`: `ffmpeg … -i file:<f>
  -af silencedetect=noise=<t>dB:d=<s> -vn -f null -`; `parse/silencedetect.rs`
  turns stderr into intervals (a trailing silence without `silence_end` ends
  at the file duration).
- `parse/progress.rs` reads `-progress` blocks (`frame`, `out_time_us` — in
  ffmpeg 8.1 `out_time_ms` is also µs — `N/A` = 0, `progress=end`).

### A§9.3 Export plan (`export_plan.rs`)

```rust
pub struct ExportPlan { pub out: String, pub tmp_ext: String, pub segments: Vec<SegmentPlan>,
                        pub stitch: StitchPlan, pub total_frames: u64, pub files: BTreeSet<String> }
pub struct SegmentPlan { pub clips: Vec<ClipId>, pub take: TakeId, pub in_: Frame, pub out: Frame,
                         pub frames: u64, pub samples: u64, pub key: String, pub path: String, pub argv: Vec<String> }
pub struct StitchPlan { pub list_path: String, pub list_content: String, pub argv: Vec<String> }
```

- **Render units:** maximal runs of *continuous* clips (same take,
  `clips[i].out == clips[i+1].in`) become one segment, so a split never
  changes the output and its cache key survives. Property:
  `plan_export(split(p)) == plan_export(p)` (and the same for GES).
- **Video inputs** *(measured; the old setpts/sub-frame plan was wrong in 30
  of 66 segments)*: per visible layer with media, `off =
  ns_to_frame_round(startOffsetNs)`. For a unit `[in, out)` with `n` frames:
  `k0 = in − off` (if the recorded fps ≠ canvas fps:
  `k0 = round((frame_ns(in) − startOffsetNs) / frame_ns_rec(1))`),
  `lead = max(0, −k0)`, `k0 = max(0, k0)`. Input:
  `-ss <frame_ns_rec(k0) − 2 ms>` (omitted when ≤ 0; the 2 ms absorbs MKV's
  ms timestamps) `-i file:<f>`. Chain:
  `[i:v]fps=<num>/<den>,setpts=PTS-STARTPTS+<lead>,crop=<src.w>:<src.h>:<src.x>:<src.y>,scale=<dst.w>:<dst.h>:flags=bilinear,format=yuva420p[,lutyuv=a=val*<opacity>][li]`.
  `bilinear` matches GES's compositor best *(measured)*; `lutyuv` avoids an
  RGB round trip.
- **Compositing:** `color=c=<bg>:s=<W>x<H>:r=<num>/<den>[base]`, then
  `overlay=x=<dst.x>:y=<dst.y>:eof_action=pass` per layer in z-order, then
  `trim=end_frame=<n>,setsar=1,format=yuv420p[v]`. Length is enforced in the
  graph — never `-frames:v` or `-t` (they truncate the audio at 29.97 fps)
  *(measured)*. Color: `-color_range tv -colorspace bt709 -color_primaries
  bt709 -color_trc bt709`.
- **Audio inputs:** per unmuted track with media: seek
  `t = frame_ns(in) − startOffsetNs − syncOffsetMs·10⁶`; `t > 0` ⇒ `-ss t`;
  `t < 0` ⇒ `adelay=<samples>S:all=1`. Chain:
  `[i:a]aresample=48000,<up>,volume=<gain>dB[,adelay=…][ai]` where `<up>` is
  `pan=stereo|c0=c0|c1=c0` for mono (0 dB, like GStreamer's audioconvert;
  ffmpeg's default upmix is −3 dB) and `aformat=channel_layouts=stereo` for
  stereo. `amix=inputs=K:normalize=0:duration=longest` when K ≥ 2. Then
  `apad,atrim=end_sample=<S>` with `S = round(n·48000·den/num)`; fades
  `afade=t=in:ns=240` unless the unit starts at `in == 0`, and
  `afade=t=out:ss=<S−240>:ns=240` unless it ends at `durationFrames`. No
  audible track ⇒ `anullsrc=r=48000:cl=stereo,atrim=end_sample=<S>`.
- **Segment codecs:** H.264 segments are `.mov` with `pcm_s16le` (MKV
  timestamps make the stitched MP4 VFR-looking); VP9/AV1 segments are `.mkv`.
  The shell verifies each segment (`ffprobe -count_packets`: video packets
  == n, decoded samples == S) before renaming it into
  `.qrec/cache/segments/<key>.<ext>`; a mismatch fails the export.
- **Presets** (Draft / Standard / High; Standard is SPEC §13.1):
  - libx264: `-preset veryfast -crf 23` / `medium -crf 20` / `slow -crf 18`
  - h264_vaapi: `-rc_mode CQP -qp 26` / `22` / `19`
  - libvpx-vp9: `-crf 36/32/28 -b:v 0 -row-mt 1 -deadline good -cpu-used 5/4/2`
  - libsvtav1: `-preset 10/8/6 -crf 38/35/30`
  - all H.264: `-pix_fmt yuv420p -profile:v high`; keyframe at each segment start.
- **VA export chain** (only when `caps.vaapi.ffmpeg_h264`):
  `-init_hw_device vaapi=va:<device> -filter_hw_device va … [v]format=nv12,hwupload[vv]
  -c:v h264_vaapi -rc_mode CQP -qp <q>`, child env `LIBVA_DRIVER_NAME` per
  A§8.10. One encoder per export: if a VA segment fails, the whole export is
  re-planned with libx264 (different argv, different keys, nothing mixes).
- **Cache key** = `sha256(PLAN_VERSION ‖ caps.ffmpeg.identity ‖ VA device for
  VA plans ‖ argv[..len-1] joined with \0)`; identical keys in one export are
  rendered once.
- **Stitch:** concat demuxer list in the segments dir (file names only),
  `-c:v copy`, audio encoded once (`aac -b:a 192k` + `-movflags +faststart`
  for MP4; `libopus -b:a 128k` for WebM), `-f mp4|webm`, output last.

### A§9.4 Frame plan (`frame_plan.rs`)

The playhead's frame through the same graph as a one-frame unit (`n = 1`,
no audio), then `scale=out_color_matrix=bt709:out_range=pc,format=rgb24,setparams=color_trc=iec61966-2-1`
(an sRGB PNG) `-frames:v 1 -f image2 -update 1`, to
`frames/<stem>-<take>-<HHMMSSFF>.png`. `--layer L2`: the stream's raw frame at
native resolution (same frame selection, no crop or scale).

### A§9.5 Capture plan (`capture_plan.rs`)

```rust
pub struct CapturePlan { pub take: TakeId, pub dir: String, pub log: String, pub fps: Fps,
                         pub health_check_ms: u32, pub first_buffer_ms: u32, pub eos_timeout_ms: u32,
                         pub video: Vec<VideoRecord>, pub audio: Vec<AudioRecord> }
pub struct VideoRecord { pub id: LayerId, pub source: VideoSource, pub file: String,
                         pub encoder: VideoEncoder, pub key_int_max: u32, pub camera_mode: Option<CameraMode> }
pub enum VideoEncoder { Va { factory: String, qp: u32 },
                        X264 { pass: String, quantizer: u32, speed_preset: String, tune: String } }
pub struct AudioRecord { pub id: TrackId, pub device: String, pub file: String, pub sample_rate: u32 }
```

File names: `L1-screen-DP-4.mkv`, `L2-camera-0.mkv`, `A1-mic.mka`,
`A2-system.mka` (sanitized to `[A-Za-z0-9._-]`). Visible and hidden layers
are both recorded (visibility is a layout choice). Encoder: `x264` ⇒ X264;
otherwise Va when `caps.vaapi.gst_h264_factory` is known; else X264.

### A§9.6 GES plan (`ges_plan.rs`)

```rust
pub struct GesTimeline { pub canvas: Canvas, pub duration_ns: u64, pub video: Vec<GesVideoClip>, pub audio: Vec<GesAudioClip> }
pub struct GesVideoClip { pub file: String, pub layer_priority: u32, pub start_ns: u64, pub inpoint_ns: u64,
                          pub duration_ns: u64, pub placement: Placement, pub alpha: f64 }
pub struct GesAudioClip { pub file: String, pub start_ns: u64, pub inpoint_ns: u64, pub duration_ns: u64,
                          pub volume: f64, pub fade_in_samples: u32, pub fade_out_samples: u32 }
```

Same render units, frame selection and lead as A§9.3:
`inpoint_ns = frame_ns_rec(k0)`, `start_ns = frame_ns(pos + lead)`,
`duration_ns = frame_ns(pos + n) − start_ns`. Video: a `videocrop` effect
from `placement.src` and always `posx/posy/width/height` from
`placement.dst`. Audio track caps `audio/x-raw,rate=48000,channels=2`;
fades are 240-sample linear ramps on the source's `volume` through a
`GstInterpolationControlSource`. The parity test (A§11) checks GES against
the frame plan.

### A§9.7 Keymap (`keymap.rs`)

```rust
pub struct KeyChord { pub key: String /* GDK keyval name: "s", "less", "question", "bracketleft", "Left" */,
                      pub ctrl: bool, pub alt: bool, pub shift: bool, pub super_: bool }
pub enum KeyContext { Global, Timeline, Canvas, Recording, Overlay }
pub enum HintContext { Idle, Countdown, Recording, Finalizing, ClipSelected, Dragging }
pub struct Binding { pub id: &'static str, pub group: &'static str, pub label: &'static str,
                     pub keys: Vec<KeyChord>, pub cli: Option<&'static str>, pub contexts: Vec<KeyContext> }
pub struct Keymap { pub bindings: Vec<Binding>, pub conflicts: Vec<Conflict>, pub errors: Vec<String> }
pub struct Conflict { pub chord: KeyChord, pub ids: Vec<String>, pub context: KeyContext }
pub fn resolve(keys_toml: &str) -> Keymap;
pub fn hints(ctx: HintContext, map: &Keymap) -> Vec<Hint>;  // "r record · o record over · s split · c autocut · ? all keys"
pub fn command_for(id: &str, state: &State) -> Option<Command>;  // shared by the GUI and the palette
```

Grammar `ctrl+shift+z`; shift is recorded only for letters and named keys
(`<` is `less`). The default table is SPEC §10.2 plus Backspace for remove.
`keys.toml`: `[bindings]` id → string or array. `qrec keys --json` prints
`[{"id","group","label","keys":[..],"cli","contexts"}]`.

### A§9.8 User config (`config.rs`)

```rust
#[serde(default, deny_unknown_fields)]   // snake_case keys
pub struct UserConfig { pub theme: Theme, pub canvas: CanvasDefaults, pub recording: RecordingDefaults,
                        pub autocut: AutocutDefaults, pub encoder: EncoderDefaults, pub ui: UiDefaults }
// canvas: width, height, fps ("30" or "30000/1001"), background
// recording: countdown_secs        autocut: auto_run_after_take, threshold_db, min_silence_ms, padding_ms, min_keep_ms
// encoder: preference, vaapi_device    ui: scale, reduced_motion, sounds
// theme: "swamp-night" | "swamp-day" | "dragons-keep" | "ogre-mode"
pub fn get(cfg: &UserConfig, key: &str) -> Result<toml::Value, CoreError>;
pub fn set(doc: &mut toml_edit::DocumentMut, key: &str, value: &str) -> Result<(), CoreError>; // validated by re-parsing
```

`qrec config set` edits with `toml_edit`, so comments and order survive; the
shell only reads and atomically writes the file.

### A§9.9 Naming (`naming.rs`)

`file_stem(name)`: replace `/`, NUL, control characters and `%` with `_`,
strip leading `.` and `-`, cap at 120 bytes on a char boundary, fall back to
`qrec`. Used for exports, frames and capture file names.

---

## A§10 CLI

- Global flags: `--project DIR`, `--json`, `--yes`, `-q`, `-v`. Numeric and
  time arguments allow hyphen values (`--by -1`, `--gain-db -3`). clap errors
  under `--json` print `{"ok":false,"error":{"code":"usage",…}}`, exit 2.
- `--out` is resolved against the CLI's cwd before sending.
- `qrec autocut` without `--clips`/`--all` sends `ClipScope::All`.
- `qrec status --json` returns `Query::State` plus host info.
- Project-less: `--help`, `version`, `doctor`, `sources list`, `schema`,
  `keys`, `agent-guide`, `config`, `completions <shell>`.
- Extensions to SPEC §14 (for GUI parity): `qrec project set`,
  `qrec autocut settings`, `qrec export cancel`, `qrec history clear`,
  `qrec completions`, `--codec`, `seek --to start|end`,
  `scene layer set --name/--locked`, hidden `qrec host --headless`.
- Human output: short sentences and aligned tables; export shows one updating
  progress line (phase, percent, ETA); toasts on stderr.

---

## A§11 Testing

| What | Where |
| --- | --- |
| Core unit + properties | per module; `tests/props_<area>.rs`: split identity (plans equal), trim bounds, move permutation, undo∘apply = id, redo∘undo = id, patch size, autocut padding, fps requantize, migration round trip, time and layout (done) |
| Planner snapshots | `insta`: export/frame/probe/silence argv, capture plan, GES plan, keymap, every `Command` JSON |
| Shell | store order and crash points, lock/socket protocol, history persistence, IPC round trip, process runner timeouts |
| CLI integration | `crates/qrec/tests/<area>.rs` with `assert_cmd` |
| Fake capture | `QREC_CAPTURE_BACKEND=fake` record start/stop end to end (M2) |
| Parity | GES vs frame plan: SSIM ≥ 0.98 **per layer rect**, plus a frame-ID fixture (frame index drawn as binary bars) asserting the exact source frame in both renderers |

**`qrec_core::testkit`** (feature `testkit`, also under `cfg(test)`; pure):
`ctx(now, entropy)`, `ProjectBuilder` (`.canvas()`, `.screen_layer()`,
`.mic()`, `.take(id, frames)`, `.clip(id, take, in, out)`, `.build()`),
`state(project)`, `drive(&State, &[Command], fake_shell) -> State`, proptest
strategies for projects and commands.

**`qrec_shell::testkit`** (feature `testkit`): `TestEnv::new()` — a temp dir
that sets `HOME`, `QREC_RUNTIME_DIR`, `XDG_{CONFIG,STATE,CACHE,RUNTIME}_HOME`
and `QREC_CAPTURE_BACKEND=fake` for every spawned `qrec` (tests never touch
the developer's config, tokens or running GUI); `.qrec()` returns an
`assert_cmd::Command`. `MediaSpec { video: Option<(w, h, fps, frames)>,
audio: Option<{ sample_rate, channels, speech_ms: [(start, end)] }> }` and
`fixture(&MediaSpec) -> PathBuf`, generated with ffmpeg (`testsrc2`, `sine`
gated by speech intervals) and cached in `target/qrec-fixtures/<hash>.<ext>`
via tmp + rename. `fixture_project(&dir, takes)` writes a project whose takes
point at fixture media.

---

## A§12 GUI (blocked)

`qrec-gui` stays a stub until `design/qrec.pen` is approved (SPEC §15.1).
Then: the Session on its thread with `Arc<State>` snapshots on the GTK main
context; every input becomes a `Command`; `gtk4paintablesink` for Live and
Program; theme tokens from `crates/qrec-gui/resources/theme/*.css`;
`ApplicationFlags::NON_UNIQUE`; gestures per A§4.1.

---

## A§13 Decisions taken without the product owner (to confirm)

1. `d` after a record-over also restores the clips the take replaced (A§5.5).
2. `d` skips tooShort/broken/cancelled takes and targets the newest placed
   take (A§5.5).
3. `layer remove --take` / `audio remove --take` are allowed and touch only
   the snapshot (A§4.4).
4. Autocut never removes a whole clip (A§5.7).
5. Hand edits can't remove takes or lower `nextTakeNumber`; those parts of an
   external edit are ignored with a warning (A§7.4).
6. In-process CLI commands become temporary hosts while they run jobs
   (A§8.5).
7. Export quality presets for Draft and High (A§9.3).
