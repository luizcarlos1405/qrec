# Qrec — Architecture (v2)

`SPEC.md` is the product contract; this file is the engineering contract:
module boundaries, exact types, protocols and the rules every module
follows. Where they disagree, `SPEC.md` wins and this file gets fixed.

**References:** `SPEC §N` points at SPEC.md, `A§N` at this file, `G§N`
and the decisions `GUI D-N`, `GUI H-N` at docs/GUI.md.

v2 folds in a five-lens review (spec coverage, state/history, shell and
processes, media, implementability). Several media rules below were
measured on the dev machine with ffmpeg 8.1.2 and GStreamer/GES 1.26.11;
they are marked *(measured)* and must not be "simplified" away.

---

## A§1 Crates, dependencies, purity, ownership

### A§1.1 Crates

```
crates/
  qrec-core/   pure functional core: model, protocol, planners, state machine,
               the GUI's view models (view/)
  qrec-shell/  imperative shell: Session, files, history store, lock, IPC,
               hosts (the GUI's too), process runner, ffmpeg/ffprobe,
               GStreamer capture, portal, sources, doctor
  qrec-cli/    clap definitions → Request; output formatting; exit codes
  qrec-gui/    GTK views (docs/GUI.md)
  qrec/        the `qrec` binary
```

`qrec → qrec-cli → qrec-core`, `qrec → qrec-shell → qrec-core`,
`qrec → qrec-gui → qrec-shell`. `qrec-cli` doesn't depend on the shell: it
maps arguments, resolves the folder (A§8.4) and renders replies, and reaches
everything else (routing, hosts, Ctrl+C, `doctor`, `sources list`, `qrec
host --headless`, the window) through its `Backend` port, which
`crates/qrec/src/shell.rs` implements over `qrec-shell`.
**`qrec-core` depends on no other workspace crate**, not even in dev-deps.

The GUI is no longer blocked: the Swamp Night slice of the pen.dev designs
was approved on 2026-10-15 (SPEC §15.1), and `docs/GUI.md` is the GUI's plan
and contract. `qrec-gui` owns every GTK call and changes State only with
Commands. Its view models are pure, in `qrec-core/src/view/` (`State` and a
few numbers the GUI knows in, final strings, flags and geometry out; A§1.3
applies), and its host is GTK-free, in `qrec-shell/src/host/gui.rs`
(A§8.3, A§8.6).

### A§1.2 Dependencies (pinned in the root `Cargo.toml`)

Agents never add or bump dependencies; they ask. One glib serves
gstreamer, gtk4, libadwaita and ashpd: CI runs `cargo tree -i glib --depth
0`, which fails when two glib versions are in the graph (`cargo tree -d -i
glib` isn't empty even with one glib: it lists the build-time smallvec
twice).

| Crate | Version / features | Used by |
| --- | --- | --- |
| serde, serde_json | 1, **never** `preserve_order` | all |
| schemars | 1.2, `chrono04` | core (`qrec schema`) |
| chrono | 0.4, `std, serde`, no default features; the shell adds `clock` | core, shell |
| json-patch | 4.2 (use its re-exported `jsonptr`) | core |
| sha2 0.11 + hex | only through `qrec_core::hash` | core |
| toml 1.1, toml_edit 0.25 | config and keys files | core |
| proptest, insta, pretty_assertions | tests; core feature `testkit` exposes proptest strategies | core |
| gstreamer(-app/-video/-audio/-pbutils/-allocators), gstreamer-editing-services | 0.25, features ≤ `v1_24` (`gstreamer-allocators` approved after the fact, 2026-10-15) | shell (feature `gst`, default on); `gstreamer` also gui |
| gtk4 (`gtk`), libadwaita (`adw`), pango | 0.11.5 `v4_22`, 0.9.2 `v1_9`, 0.22.9 `v1_56` (`FontMap::add_font_file` for the bundled fonts); approved 2026-10-15 (docs/GUI.md G§0.1, G§6.1). Built against GTK 4.22, libadwaita 1.9, Pango 1.57: `gtk/v4_24` and `adw/v1_10` don't build in the dev env. No `gst-plugin-gtk4` crate (a second glib): the system `gtk4paintablesink` plugin; no `glib-build-tools` (`build.rs` runs `glib-compile-resources`) | gui; gtk also qrec's tests (`gui_a11y`'s stand-in AT-SPI registry: GIO's D-Bus, WP10) |
| ashpd | 0.13, `default-features = false`, `async-io, screencast, camera` (no tokio anywhere) | shell |
| futures-lite | 2.6 (`block_on` for ashpd) | shell |
| crossbeam-channel | 0.5 | shell, gui |
| notify | 8 (inotify) | shell |
| wayland-client | 0.31 (wl_output names) | shell |
| rustix | 1.1 `process, termios` (setsid, kill, process groups) | shell |
| libc | 0.2 (`renameat2`, `SO_PEERCRED`, `prctl`, `flock`, `sigaction`) | shell |
| tempfile, getrandom, dirs, which, tracing(-subscriber), anyhow | | shell, cli; tracing also gui |
| clap 4.6 (derive), clap_complete | | cli |
| assert_cmd, predicates | tests | qrec |

The workspace MSRV is Rust 1.92 (`rust-version`): gtk4 0.11.5, glib
0.22.10 and gstreamer 0.25.4 need it. File locking stays `flock(2)` through
libc (std's `File::try_lock`, Rust 1.89, would do now; the code didn't
change). Signals: libc `sigaction` handlers
write to a self-pipe that a thread turns into `host::signals::Interrupts`
(no signal-hook).

### A§1.3 Purity rules for `qrec-core` (enforced)

- No fs, processes, network, threads, env, clocks or randomness.
  `crates/qrec-core/clippy.toml` bans them; `lib.rs` has
  `#![deny(clippy::disallowed_methods, clippy::disallowed_types)]`.
- `HashMap`/`HashSet` are banned too (nondeterministic iteration breaks
  snapshots): use `BTreeMap`/`BTreeSet`.
- chrono is built without `clock`; CI also runs `cargo check -p qrec-core`
  alone so workspace feature unification can't hide a `Utc::now()`.
- Paths in the core are `String`s: project-relative and `/`-separated,
  except a hand-picked `out` of an export or frame save, which is absolute
  (A§5.9). An auto name stays project-relative (`exports/…`, `frames/…`):
  that is how the runner tells it apart and bumps it `-2`, `-3` (A§8.8).
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
  view/                                                           owners: G§3.17
crates/qrec-shell/src/
  lib.rs paths.rs msg.rs deps.rs wire.rs testkit/                foundation (skeleton)
  store/ lock.rs host/{lockfile,publish}.rs watcher.rs           owner: store
  session/ facts.rs caps.rs timers.rs proc.rs                    owner: session
  ipc/ host/{headless,cli_host,gui,takeover,signals,spawn}.rs     owner: ipc
  media/ sources.rs doctor.rs                                     owner: media
  capture/ gpu.rs gst_init.rs ges_render.rs player.rs             owner: capture
  config_file.rs keys_file.rs                                     owner: meta
crates/qrec-cli/, crates/qrec/                                    owner: cli
crates/qrec-gui/                                                  owners: G§3.16
```

The GUI's files keep the owner of their area: the GUI host and the
takeover (`host/{gui,takeover}.rs`) are ipc's, the Program player
(`player.rs`) is capture's, the preview cache and take sizes
(`media/{preview,sizes}.rs`) are media's, and the bounded readers of
`config.toml` and `keys.toml` (`config_file.rs`, `keys_file.rs`) are meta's.
The core's `view/` modules and `qrec-gui` belong to the GUI's work
packages, which edit only the files docs/GUI.md G§3.16–G§3.17 give them;
G§7.0 adds `view/mod.rs`, the `capture/mod.rs` types and the module lists
to the shared files.

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
| `Take.recovered` | bool | startup recovery finished the take after Qrec closed mid-take (A§5.3): set when it ends `ok` or `tooShort`; a fact like `placed` (A§7.4); in `TakeRow` and `take show`'s JSON |
| `Stream.durationNs` | i64 | probed duration (audio-only takes, re-derivation) |
| `Stream.frames` | u64 | video packet count from `ffprobe -count_packets` (A§2.2) |
| `Stream.fps` | Fps | the recorded rate of a video stream |

They stay in schema v1: each came before the first release, without a
`version` bump. `Take.recovered` came last (for the GUI's bin): a dev
build from before it rejects a file that has it (`deny_unknown_fields`),
which is accepted before the first release. The first schema change after
the first release bumps `version` with a migration (SPEC §6.3, A§13).

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
Facts aren't in history patches (A§7.2), so whatever reaches another fps
through the history (undo, redo, restore, replay, compaction and purge
bases, external merges) retimes the take facts from the rate they are at
(`requantize::retime_takes`): durations re-derived, takes without media
info (the SPEC §6.3 shape) converted through nanoseconds, and
`Take.replaced` re-quantized like the timeline (A§5.4), so discard-last
puts back the frames a record-over replaced.
An audio `durationNs` is the container's; a file cut short by a crash has
none, so it comes from the packets too: FLAC frames of 4608 samples
(`capture_plan::FLAC_BLOCK_SAMPLES`, the `blocksize` the engine pins; only
the last frame of a normally closed file is shorter) *(measured: 36 packets
= 3.456 s, as decoded)*. A finished take whose length its files don't tell
is `broken` with that reason, never a 0-frame `tooShort`.

### A§2.3 Validation (`validate::validate(&Project) -> Vec<Issue>`)

`Issue { severity: Error | Warning, path: String /* JSON Pointer */, message }`.
Errors make a project invalid:
- `version == CURRENT_VERSION`; `nextTakeNumber` > every take number.
- Clips: unique ids; take exists with status `ok`; `0 ≤ in < out ≤ durationFrames`.
- Scenes: layer and track ids unique in one namespace per scene (a stream's
  id is its layer's or track's); `0 ≤ opacity ≤ 1`; `rect.w, rect.h ≥ 1`;
  gain within ±60 dB; `|syncOffsetMs| ≤ 5000`.
- Streams: ids unique per take; `file` must be `takes/<takeId>/<name>` with
  `<name>` matching `^[A-Za-z0-9._-]+$` and not `.` or `..` (guards purge,
  A§5.6). Finalized takes (`ok`, `tooShort`) have the media fields of their
  kind for every stream a non-missing layer or track uses; `recording` and
  `broken` takes and missing sources may lack them (a take that broke at
  start was never probed, and undo must still validate).
- Canvas: even `16 ≤ w, h ≤ 8192`; `fps.num, fps.den > 0`, fps ≤ 240.
- `countdownSecs ≤ 10`; autocut numbers in range (threshold −90…0 dB,
  ms values ≤ 60 000).
- Lengths: a take's `durationFrames` and the timeline's total length (at
  the canvas fps) and every stream's `startOffsetNs`, `durationNs` and
  `frames` (at its fps) are at most 2⁶⁰ ns (about 36 years), so sums of
  clip lengths and offsets plus durations can't overflow on a valid
  project, however it was edited.

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
    pub lane_mute: BTreeSet<TrackId>,    // "laneMute": lanes muted for playback only (below)
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
    pub engine_stuck: Option<String>,    // the capture engine wedged (A§5.3)
    pub recovering: BTreeSet<TakeId>,    // startup recovery probes still out (A§5.3)
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
    pub ffprobe: Option<String>,         // resolved on PATH at every detection (never cached); None: not installed
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
    Autocut { cmd: serde_json::Value /* + "skipped" until it completes, A§5.7 */,
              targets: Vec<AutocutTarget>, params: AutocutParams,
              dry_run: bool, pending: BTreeSet<SilenceKey>, ready: bool },
    Export { out: String, files: BTreeSet<String>, total_frames: u64,
             seg_frames: Vec<u64>, seg_done: Vec<u64>,
             seg_clips: Vec<Vec<u64>> /* each segment's timeline clips as frame counts, A§5.8; serde default */,
             stitch_total_ns: u64, stitch_done_ns: u64, stitching: bool },
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

`laneMute` is the timeline lanes' `M` (`solo` is their `S`; GUI D-08), for
playback only: the Program player (A§9.6, `ges_plan::without_tracks`)
leaves those tracks out, while a scene's `AudioTrack.muted` (the Onion
layers panel's `M`) changes the export. `SetUi` replaces the whole set, ids
no scene has are dropped (A§5.10), and a `ui.json` without it mutes
nothing.

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
pub fn facts_needed_for_event(state: &State, ev: &Event) -> FactsNeeded;
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
`apply` on a cloned `State` (with `Ctx::preview(now, &config)`: no
requester, not confirmed, fixed entropy, no facts but the config), renders
the result and discards the effects.
On release it dispatches exactly one `Command`. Transient state is never
stored in `State` or persisted. Canvas layer drags follow the same rule.

### A§4.2 Facts

```rust
#[derive(Default)]
pub struct Facts {
    pub config: UserConfig,                         // always filled (defaults if no file)
    pub keys_toml: String,                          // keys.toml text, for Query::Keys ("" if none)
    pub folder_name: Option<String>,                // Init
    pub default_mic: Option<AudioDeviceInfo>,       // AudioAdd mic without source
    pub default_monitor: Option<AudioDeviceInfo>,   // AudioAdd system without source
    pub audio_devices: Vec<AudioDeviceInfo>,        // labels for an explicit source
    pub screens: Vec<ScreenInfo>,                   // LayerAdd screen (label, size; unknown → NotFound)
    pub cameras: Vec<CameraInfo>,                   // LayerAdd camera
    pub out: Option<OutFacts>,                      // Export/FrameSave with --out
    pub take_files: BTreeMap<TakeId, Vec<FileFact>>,// purge confirmation: the files it deletes
    pub take_left: BTreeMap<TakeId, Vec<String>>,   // purge: what it would leave on disk (A§5.6)
    pub min_take_number: u32,                       // 1 + highest takes/take-N folder
    pub files: BTreeMap<String, FileFact>,          // size/mtime for silence keys
}
pub struct AudioDeviceInfo { pub name: String, pub description: String, pub is_monitor: bool }
pub struct ScreenInfo { pub output: String, pub description: String, pub x: i32, pub y: i32, pub width: u32, pub height: u32, pub scale: f64 }
pub struct CameraInfo { pub device: String, pub label: String, pub modes: Vec<CameraMode> }
pub struct CameraMode { pub width: u32, pub height: u32, pub fps: Fps, pub format: String }
pub struct OutFacts { pub abs: String, pub exists: bool, pub bytes: Option<u64> /* lstat size when it exists */, pub id: Option<FileId> /* lstat identity when it exists: the one file a confirmed overwrite may replace (A§5.9) */, pub forbidden: Option<String> /* why: in takes/ or .qrec/ (also through a link or bind mount, A§5.9), qrec.json, a directory, a path naming a folder or not UTF-8, a name over 255 bytes or in the temp-file shape (A§8.7) */ }
pub struct FileId { pub dev: u64, pub ino: u64 }    // st_dev, st_ino (lstat: a symlink is a file of its own)
pub struct FileFact { pub path: String, pub bytes: u64, pub mtime_ns: i64 }

#[derive(Default)]
pub struct FactsNeeded {
    pub folder_name: bool, pub audio_devices: bool, pub video_sources: bool,
    pub out: Option<String>, pub take_files: Vec<TakeId>, pub take_dirs: bool,
    pub files: Vec<String>,
}
```

`Session::dispatch` calls `facts_needed`, gathers exactly that (no `pactl`
for a clip split), then calls `apply`. Events do the same through
`facts_needed_for_event` before `on_event`: the last countdown tick gets
the begin-take facts (take number, camera modes), `TakeProbed{Finalize}`
the file facts an `autoRunAfterTake` autocut needs. Under
`QREC_CAPTURE_BACKEND=fake` device facts come from the fake inventory
(A§8.12).

`Facts.config` is `config.toml` read with `config::parse` (A§9.8), again
whenever its size or mtime changed, so `qrec config set` reaches a running
host. A file that can't be used (bad TOML, an unknown key such as a typo, a
wrong type, a value out of range, a file or folder that can't be read, one
that isn't a regular file or holds over 1 MiB: `config_file::read_text`, so
a FIFO never blocks) gives the defaults for every setting, and people are
told: a warning toast "config.toml is invalid" whose body is the problem and
"; using the default settings", once per change of the file. The shell's
`ConfigFile` keeps the problem until a reporter takes it, so a read that
doesn't report (the open's capability check, a query) never swallows it: the
Session says it as the project opens, or at the first query or dispatch that
reads a changed file. Its clones share what was read and said, so the
Sessions of one process opened with the same `SessionEnv::config` say it
once between them. Toasts reach the GUI, a headless host's log, or the CLI
(stderr; `--json`: one `{"event":"notice",…}` line, A§4.9); the CLI also
checks the file before routing each request, so a run warns once whichever
host answers (a live GUI or headless host, the one a recording spawns), and
an empty folder's lazy init (two Sessions) still warns once.

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
| `TakeDiscardLast` | – | U, R and A (Finalizing: replies after placement) | `qrec take discard-last` |
| `TakePurge` | `takes` | A, `!`, not undoable | `qrec take purge <take>… --yes` |
| `ClipAdd` | `take`, `at: InsertAt` | U | `qrec clip add <take> [--at N \| --after <clip> \| --after-selection]` |
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
| `SetUi` | `patch: UiPatch` | R | — (zoom, scroll, panels, bin, snapping, solo, lane mutes, selected layer) |

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
                     lane_mute: Option<BTreeSet<TrackId>> /* the whole set */,
                     selected_layer: Nullable<LayerId>, panels: Option<BTreeMap<String, f64>> }
```

Scene rules: `LayerAdd`/`AudioAdd` accept only `SceneTarget::Project`
(a take's media is a fact), and a take's sources never change (`AudioSet`
with a source on a take is `Invalid`). A source already in the scene is
`Invalid` (`LayerAdd`, `AudioAdd`, `AudioSet`: a camera opens once, portal
tokens are keyed by output); a sink's monitor can't be a mic (SPEC §5.1).
A take still `recording` can't be edited (`Invalid`). `LayerRemove`/
`AudioRemove` on a take remove the entry from that snapshot only; the
stream stays, and a later `SceneApply` from the project re-adds it.
`SceneApply` matches by id and copies layout fields only (`rect, crop,
opacity, visible, fit`, z-order of matched layers; `gainDb, muted,
syncOffsetMs` of tracks); it never copies `missing` or sources, adds to a
take only the layers and tracks it has a stream for (their `missing` says
whether that stream has media), and `AllTakes` skips takes still
recording. `AudioRemove` of the project-scene track named by
`autocut.track` also sets it to null in the same entry. `LayerOrder` past
the top means the top.

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
`Query::State` → `{project, ui, history: {cursor, baseSeq, nextSeq,
entries: [HistoryRow]}, runtime}`: the whole state but the patches and
`seenClipIds` (`qrec status --json` adds the host, A§10).

A patch command with no field set (`ProjectSet`, `LayerSet`, `AudioSet`,
`AutocutSettings`) is `Usage` "nothing to change"; a command that changes
nothing replies Ok with `seq: null` and appends no entry (`HistoryClear` on
an empty history: Ok, no confirmation asked).

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
    CaptureStarted { take: TakeId, t0_ns: u64, files: Vec<String>,
                     start_offset_ns: BTreeMap<String, i64> }, // per stream id, measured (SPEC §5.2)
    CaptureFailed { take: Option<TakeId>, source: Option<String>, message: String,
                    missing: Option<String> },              // a GStreamer element that isn't installed
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
    ExportFailed { job: JobId, message: String, stderr_tail: Vec<String>, log: String }, // log "" = none written: nothing names it
    ExportCancelled { job: JobId },
    FrameSaved { job: JobId, path: String },
    FrameFailed { job: JobId, message: String, log: String },
    OutputAppeared { job: JobId, out: String, bytes: u64, kept: String }, // A§8.8, ends either job
    TakeDirsDeleted { job: JobId, deleted: Vec<String>, errors: Vec<String> },
    PurgeFailed { job: JobId, message: String }, // its commit failed and wasn't undone: nothing deleted (A§5.6)
    ProjectChangedOnDisk { project: Result<Box<Project>, String> }, // parsed + migrated by the shell
    RequesterGone { req: ReqId },                                  // IPC client disconnected
}
pub struct RecordedStream { pub id: String, pub file: String, pub start_offset_ns: i64, pub eos: bool }
pub struct ProbeResult { pub id: String, pub file: String, pub info: Result<MediaInfo, ProbeError> }
pub struct ProbeError { pub kind: ProbeFailure, pub message: String }
pub enum ProbeFailure {
    Media,       // missing, empty or unreadable file: ffprobe judged it (a fact)
    MissingTool, // ffprobe can't be run (not installed): nothing is known about the file
    Tool,        // ffprobe timed out or was stopped, or the prober failed: nothing is known
}
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
| `missing_dependency` | 5 | a required tool or element is missing: no ffmpeg (export, frame save, autocut whose analysis failed), no ffprobe (export, before rendering), a GStreamer element (`CaptureFailed.missing`); the hint points at `qrec doctor` |
| `busy` | 6 | recording/finalizing/exporting, lock held by an unresponsive host |
| `not_found` | 7 | bad id, unknown output/device |
| `invalid` | 1 | impossible edit, invalid project, not recording, already a project |
| `capture_failed` | 1 | health check or start failure; hint names the source |
| `unavailable` | 1 | needs the GUI host, no host holds a selection |
| `io`, `internal` | 1 | |
| `interrupted` | 130 | CLI only: Ctrl+C/SIGTERM before a reply (A§8.6) |

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

`hello` → `{pid, projectDir, host: "gui"|"headless"|"cli", record: RecordPhase, jobs, version}`,
plus `projectDirHex` (the folder's exact bytes) when its path isn't UTF-8:
`projectDir` is then only its display form, and clients use the bytes (A§8.4).
Until the host's Session opens, its server answers `hello` itself (idle, no
jobs), so a host busy recovering never looks stuck. A host running another
Qrec version gets a stderr notice ("the running … host (pid N) is qrec X;
this is qrec Y") and is used all the same.
`shutdown` → the host stops accepting, finalizes a recording
(`RecordStop`), lets jobs finish, exits (A§8.6); a GUI host refuses it
(`busy`: only closing its window ends it).
The Session allocates a `ReqId` per request (a string: `r1`, `r2`, …);
client ids stay inside the IPC layer. `--json` prints `to_wire(&result)` — the
same `{"ok":…}` object — and streaming commands print progress lines
(`{"event":"progress",…}`) before it. Every run ends stdout with exactly one
such object: help and the version are `{"ok":true,"data":<help text>}` and
`{"ok":true,"data":{"version":…}}`, and an interrupted run ends with one too
(A§8.6). Toasts never go to stdout: humans get them on stderr, `--json`
gets `{"event":"notice",…}` lines on stderr.

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
`History(Append)` + `WriteProject` (+ `WriteUi` if the UI changed). An
append that compacts the history (A§7.3) emits one `History(Rewrite)`,
which holds the new entry, instead of the `Append`. `commit` and `persist`
are the only source of `SetLive`: they emit it whenever
`apply::ui::live_scene` changes (the project scene while the GUI shows the
Live canvas, with preview; or while `record != Idle` in any host, preview
only in GUI Live mode); handlers add `StartCapture`/`StopCapture` after it.
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
| Idle | Start/Toggle | `Busy` "an export is running (job-N)" if an export runs, in every host and checked first (`export_blocks_recording`; SPEC §13.1); else `Busy` in a CLI command's own host (`HostKind::Cli` never records, A§8.5); `Invalid` if the scene is empty; Over needs contiguous existing clips (stored as `ClipRef`s; `OverSelection` resolves `ui.selection` and is `Unavailable` without a selection). countdown > 0 ⇒ Countdown + `StartTimer`; else begin take ⇒ Starting |
| Idle | Stop/Cancel | `Invalid` "not recording" |
| Countdown | TimerFired (current timer) | next tick, or begin take ⇒ Starting |
| Countdown | Stop/Toggle/Cancel | `CancelTimer` ⇒ Idle; start req gets `Invalid` "countdown cancelled"; caller gets Ok |
| Starting | CaptureStarted | ⇒ Recording; record the streams' `startOffsetNs` (persist: every bin passed its first buffer, and a crash never reaches `CaptureStopped`); reply start req `RecordStartedView` (unless `wait`) |
| Starting | CaptureFailed | take `broken` (+`error`) ⇒ Idle; reply start req `capture_failed` (`missing_dependency` when `missing` names an element that isn't installed) |
| Recording / Finalizing | CaptureFailed (pipeline-wide) | take `broken` ⇒ Idle; while Recording `wait` requesters get `capture_failed`, while Finalizing stop requests and waiters get the broken `TakeFinalizedView` |
| Idle / Countdown | CaptureFailed (no take: the live pipeline) | toast; a countdown ends and its start req gets `capture_failed` |
| Starting | Stop/Toggle | `StopCapture{cancel:false}` ⇒ Finalizing; start req gets its started reply |
| Starting / Recording | Cancel | `StopCapture{cancel:true}` ⇒ Finalizing(cancelled) |
| Recording | Stop/Toggle | `StopCapture{cancel:false}` ⇒ Finalizing; the caller waits for the finalized reply |
| Recording | RecordClock | update `elapsed_ns`; `Progress{phase:Record}` to `wait` requesters (≥ 500 ms apart) |
| Recording | StreamLost | mark layer/track `missing` (persist); last stream lost ⇒ act as Stop |
| Finalizing | Stop | join the stop waiters; Start/Toggle/Cancel ⇒ `Busy` "finalizing take N" |
| Finalizing | TakeDiscardLast | remember `discard` (applied after placement, A§5.5); a second one while one is queued ⇒ `Busy` |
| Finalizing | CaptureStopped | `timed_out` ⇒ `broken` (`EOS_TIMEOUT_REASON`: "the recording didn't stop within 10 s"); else record offsets (the same values, or the first ones for a take stopped while Starting), `Probe{Finalize}` |
| Finalizing | TakeProbed | see below ⇒ Idle |
| any | EngineWedged | take `broken`; future starts fail with "the capture engine is stuck (GPU driver?); restart Qrec" |

**Begin take:** allocate the take number (A§4.3); insert the take
(`status: recording`, scene snapshot, planned streams) with `persist`;
effects `CreateTakeDir`, `WriteProject`, then `SetLive` (if needed) and
`StartCapture(plan)`.

**TakeProbed (Finalize), in order:** fill media info, mark unreadable
streams `missing` (persist); zero readable streams, or only streams of
sources lost during the take ("every source was lost during the take"), ⇒
`broken` + toast;
`durationFrames < 10` ⇒ `tooShort`, bin, toast "That was shorter than Lord
Farquaad. Take moved to the bin."; cancelled ⇒ `ok`, bin, no clip; otherwise
`ok` and place it with `commit` ("Record take N",
`cmd: {"event":"takeFinalized","take":"take-0003"}`): Append adds
`[0, durationFrames)` at the end; Over replaces the run if every `ClipRef`
still matches and is contiguous (recording `Take.replaced`), else appends
with a toast "record-over target changed; take appended at the end". Set
`Take.placed`. Then the pending discard (A§5.5), the ready autocut jobs
(A§5.7), then `autocut.autoRunAfterTake` starts a job for the new clip. Reply
to every stop request and waiter with `TakeFinalizedView`. A broken take's
`error` names what the probe said about each stream.

**A probe that failed on its own** (`ProbeFailure::MissingTool` or `Tool`:
ffprobe isn't installed, timed out, the prober died) says nothing about the
files, so it changes no fact: the take stays `recording` with its streams and
offsets, the phase goes back to Idle, stop requests and waiters get
`missing_dependency` (exit 5, hint: install ffprobe, `qrec doctor`) or `io`,
a pending discard hears "take N was not placed", toast + log name the real
cause. Startup recovery finalizes the take (into the bin) once the probe
works.

**Recovery** (`Session::open` with the lock, `on_event::record::start_recovery`):
each take still `recording` gets `Probe{Recover}` and is in
`runtime.recovering` until its `TakeProbed(Recover)` comes back (whatever it
says). Readable ⇒ `ok` (`tooShort` under 10 frames) and `recovered`
(A§2), bin, toast "Recovered take 7 from the bog.";
unreadable ⇒ `broken`; a probe that failed on its own ⇒ the take stays
`recording` (warning toast) for the next start. Commands wait for the
answers (at most `RECOVERY_WAIT`, 60 s after the open) before they run, so
even a one-shot CLI command recovers the take and already sees it recovered
(`qrec clip add take-0007` works right after a crash). The Session holds
them itself instead of blocking in `Session::open`, so it handles events,
`hello`, queries and signals meanwhile; commands that only stop something
(`RecordStop`, `RecordCancel`, `ExportCancel`, a `RecordToggle` that stops)
never wait. They act on what waits too, which would otherwise run after
them: `RecordStop`, `RecordCancel` and `RecordToggle` abort every waiting
`RecordStart` and `RecordToggle`, like a countdown (each gets `Invalid`
"recording cancelled before it started", the caller Ok "Recording cancelled
before it started"; a toggle that finds no start waiting is a start and
waits), and `ExportCancel` without a job aborts the waiting `Export`s
(`Invalid` "export cancelled"; a job id only names a running export).
UI-state commands never wait either (`Command::ui_only`: `Select`, `Seek`,
`SetUi`, `SetCanvasMode`, `Play`, `Pause`, `Shuttle`, which change only
`ui` and `runtime.playing`; GUI D-47), so a window stays live meanwhile;
the parked commands keep their order and resolve the selection and the
playhead when they run, like every command at its turn.
Commands whose wait is over run before any later message but a signal
(which drops them, A§8.6), also before a tick released them, so a stop
always finds the take their start began. A probe still out, whether
commands still wait for it or not, is work like a running job: a host
isn't idle meanwhile (A§8.6) and `Session::run_until` doesn't return
before its answer, so no host exits and drops it (the runner always
answers, A§8.7) — unless the host was interrupted, or a GUI host's close
grace ran out (it sends `Msg::Abandon`): then the host's own grace decides,
and the next open recovers the take. `run_until` still asks its
host's `done` after every message meanwhile: that is where an in-process
host passes its command's reply on (A§8.5), so the reply of a stop, which
didn't wait, is printed at once, the recovery's toasts after it.

**CaptureFailed vs StreamLost** (engine contract, A§8.11): after
CaptureStarted, an error attributable to one branch (its source, encoder,
muxer or filesink) EOSes that branch and emits `StreamLost`, and so does a
video stream that passed no frame for `STALL_LIMIT` (3 s: a camera that
hangs without an error; checked at every `RecordClock` tick). `CaptureFailed`
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
back at the position of the first removed clip in the same entry (only
clips whose take is still `ok`, clamped to its current `durationFrames`).
If no clip references it: `NotFound` "Take N is already in the bin" — `d`
never reaches older takes. In Finalizing the discard is applied right after
placement ("Record take N" then "Discard take N", both undoable); if the
take wasn't placed, reply Ok "take N was not placed".

### A§5.6 Purge

Only takes that no clip references and that aren't `recording`; `Busy` while
any job's `files` include a file of the take, an autocut job targets one of
its clips (autocut jobs list no files) or a purge of it runs. The purged
takes' clips also leave every other take's `Take.replaced`, so a later
discard can't bring them back. Confirmation `details`: the
files with sizes (`facts.take_files`), the number of history entries lost and
the oldest surviving label. Effects in order: `History(Rewrite)` (A§7.5),
`WriteProject`, then `DeleteTakeDirs` — the shell deletes exactly
`takes/<takeId>/` (O_NOFOLLOW, regular files only, then rmdir) only after the
commit is durable. Not undoable. The facts come from that same walk
(`media::take_contents`), so `take_files` lists exactly the files deleted,
never what is behind a symlinked `takes/` or take folder. What the walk would
leave on disk (`facts.take_left`: a symlinked or unreadable `takes/` or take
folder with something behind it, or a symlink, folder or special file in the
take folder) makes the purge `Invalid` before anything changes, confirmed or
not: "can't purge take 2 from disk: takes/take-0002/ is a symlink (not
followed)", `details: {takes, left}`, hint to move or delete those entries by
hand first. A purge never drops a take and its history while its media
stays. A purge whose commit fails changes nothing
(A§7.6: the store puts the base it replaced back) and replies `io`
"couldn't save the project, so nothing changed"; if its files can't be put
back it is kept in memory, the shell deletes nothing and sends
`PurgeFailed`, whose reply says nothing was deleted and never that the
take was purged.

### A§5.7 Autocut

Plan (pure, `autocut.rs`), per clip and track:
1. Silences arrive in **file time**; shift to take time
   `t = x + startOffsetNs + syncOffsetMs·10⁶`.
2. Merge overlapping or touching silences (shrinking them one by one would
   leave fake speech where they meet), then shrink each by `paddingMs` on
   both sides; drop empty ones.
3. Kept = clip range minus shrunk silences.
4. Drop leftover fragments shorter than `minKeepMs` (a clip no shrunk
   silence reaches stays whole, however short).
5. Snap outward to frames (`in = ns_to_frame_floor`, `out = ns_to_frame_ceil`),
   clamp to the clip, merge touching fragments.
6. Nothing kept ⇒ the clip is left unchanged and reported "entirely silent at
   −35 dB" when the silences cover it, else "nothing of 200 ms or more to
   keep" (autocut never deletes a whole clip).

Track: override → `autocut.track` → first `mic` of the take scene → first
track; a take without it is skipped (warning in the result).

Flow: `Autocut` snapshots params and targets (`AutocutTarget` with a
`ClipRef`), emits `DetectSilence` for keys not in `runtime.silence`
(argv from `media_argv`), and completes when all keys are known:
- If `record != Idle`, mark the job `ready`. Ready jobs are applied when the
  phase returns to Idle, whether or not the take was placed; any still
  `ready` are applied before another run completes or a real run starts
  (oldest first, one entry each).
- Apply only to targets whose current clip equals its `ClipRef`; others go to
  `result.skipped` ("changed while analyzing").
- One history entry for the whole command ("Autocut take 3: removed 4
  silences (6.2 s)"); the first fragment keeps the clip id; if nothing
  changes, no entry. `dryRun` returns `AutocutPlanView` without applying.
- No clip analyzed and an analysis failed ⇒ `missing_dependency` without
  ffmpeg, else `io` (hint: the ffmpeg log), with the plan in `details`;
  clips only skipped for project reasons (no such track, missing media or
  file) ⇒ Ok "Nothing to cut: …". Clips skipped before completion ride in
  the job's `cmd` under `skipped`, stripped from the history entry.
Properties: kept ⊇ every padded-speech region of a fragment ≥ `minKeepMs`;
fragments stay inside the clip; a clip without silence is unchanged.

### A§5.8 Jobs

One export at a time (`Busy`); `ExportCancel{job: None}` cancels it;
`FrameSave` may run alongside. Each job holds what it reads (`files`) and
its own plan (a snapshot), so editing continues during an export
(SPEC §13.1). Export progress = `0.9·Σseg_done/total + 0.1·stitch`,
ETA from `ctx.now − started`, `label` "Rendering clip 4/12" / "Stitching"
(`on_event::output::export_progress_view`, which a window also applies to
the jobs of another host's `hello`). The label counts the timeline's
clips, not the segments: a render unit joins continuous clips (A§9.3), so
the job keeps each segment's clips as their frame counts (`seg_clips`) and
"clip k" is the clips rendered whole plus one; a job without `seg_clips`
counts segments.
`RequesterGone`: clear `job.req`; in Headless/Cli hosts cancel the jobs that
requester started if no other requester waits (exports and frame saves are
cancelled, autocut analysis dropped, a purge finishes); never stop a
recording. A cancelled export ends with `ExportCancelled`, a frame save
with `FrameFailed { message: "cancelled" }` and an analysis with
`SilenceFailed { message: "cancelled" }` (they have no cancel event);
exports and frame saves stay in the job table until then, so purge still
sees their files busy, while an autocut job leaves at once and its late
results are ignored. Only a GUI keeps an orphaned job
(`HostKind::keeps_orphaned_jobs`); the CLI's interrupted hint follows the
same rule (A§8.6).

### A§5.9 Export and frame-save commands

`out` from the CLI is already absolute (resolved against the CLI's cwd).
It means what the OS says: `..` stays in `facts.out.abs` (the path the job
opens its folder by and reports while it still leads there) and the checks
run where the kernel resolves it, so
`link/..` is the parent of the link's target, never lexically the folder
holding `link`. What is checked is what the job touches: the entry the
final rename replaces or creates (its folder resolved, symlinks and `..`
followed, its own name kept; the temp file lands in that folder too) and,
when that entry is a symlink, where the link leads.
`facts.out.forbidden` (in `takes/` or `.qrec/`, `qrec.json`, the project
folder, a directory, a path that names a folder — it ends with `/`, `.` or
`..`; the CLI keeps the trailing `/` when it makes `--out` absolute — a
path that isn't UTF-8, a name over 255 bytes, or a name in the temp-file
shape `.<stem>.qrec-part-<pid>-<job>[.<ext>]` that host start would sweep,
A§8.7) ⇒ `Invalid` (not confirmable). `qrec.json`, `takes/` and `.qrec/`
are matched by name under the project folder and by identity (`st_dev`,
`st_ino`, symlinks followed), and so is everything found in `takes/` and
`.qrec/` (`facts::Protected`, a few levels deep): every folder, real or
behind a symlink (a take folder, `.qrec/cache`), and every file a symlink
there leads to (a media file moved to another disk). A touched place is
refused when it is one of those files or lies in one of those folders by
path or by the identity of it or of any existing folder above it. So a
symlinked `takes/`, take folder, media file or `.qrec/cache`, or a bind
mount of any of their folders, still guards the recordings, through the
link or mount or straight at its target, and a dangling link (a disk that
isn't plugged in) still guards where it points. `exists && !confirmed` ⇒
`confirmation_required` "overwrite <abs> (<size>)" with `details:
{files:[{path,bytes}]}` (`facts.out.bytes`, the `lstat` size: a symlink at
`out` is what gets replaced, not its target).
The plan's `replace` is, when `exists && confirmed`, the confirmed file's
identity (`facts.out.id`: `st_dev`, `st_ino` from `lstat`): the runner
replaces that file and no other. Another file at `out` by the time the job
ends (the confirmed one moved away or replaced) counts as one that appeared.
These checks run on paths when the command is dispatched; the job runs them
again on the folder it holds open when it writes (A§8.8): at its start,
before the temp file is created and before the final rename, the open
folder (by identity, walked up through `..` from its descriptor, and by its
path now) may not be or lie in `takes/` or `.qrec/`, nor may the name in
the project folder be `qrec.json`, `takes` or `.qrec`, or else the job
fails (`ExportFailed`/`FrameFailed`) before it writes anything there. So a
folder on the path swapped for a symlink after the check can't lead an
output into the project, and auto names (`exports/`, `frames/` behind a
link) get the same check. `OutputAppeared` (a
hand-picked `out` appeared while the job ran, A§8.8) ends the job with
`confirmation_required` (exit 4) naming both paths, `details:
{files:[{path,bytes}], kept}` (`kept` is `<stem>-kept.<ext>` next to `out`),
hint "rename <kept>, or run the command again
with --yes to replace <out>", and a warning toast ("Export kept aside" /
"Frame kept aside") with Open folder / Copy path for the kept file.
Frame saves need ffmpeg like exports (`missing_dependency` before the job
starts); exports also need ffprobe, which checks every segment (A§9.3).
Auto names: `exports/<stem>-YYYYMMDD-HHMMSS.<ext>` and
`frames/<stem>-<take>-<HHMMSSFF>.png` (timeline timecode) with local time
from `utc_offset_secs` and `stem = naming::file_stem(project.name)`.
Timeline empty ⇒ `Invalid` "nothing to export". Done toast for export:
"Layers! Like an onion. Your video is ready." with the body "<path> · <size>"
and Open file / Open folder (S16b; Copy path is in the GUI export panel's
Done page); its path is project-relative for an auto name, as the job named
it (`on_event::output::shown_output`), while the reply's `path` stays
absolute; frame toast: path with Copy image / Open folder. Toast actions'
paths are absolute or project-relative (`UiAction`): a front end opens the
relative ones under the project folder.

### A§5.10 UI normalization (`ui_normalize.rs`)

`ui_normalize(prev: &Project, next: &Project, ui: &UiState) -> UiState` runs
in every `commit`/`persist`, and on the `ui.json` a session opens with
(`prev` = `next` = the loaded project: a UI written before the project
changed, by a crash between their writes or a hand edit, is made
consistent before anything reads it; GUI WP7 integration audit): drop
selection ids not on the timeline and sort the rest by index; after a
record-over select the new clip; a removed run
that held a selected clip passes the selection to the clips put in its
place (discard putting replaced clips back), and a selected clip that shrank
and is followed by clips carved from its old range passes it to them
(autocut fragments, a split's right half); clear `selected_layer` and `solo`
when they no longer exist in the scene the canvas targets
(`canvas_target`: Live ⇒ the project scene; Program ⇒ the take of the
first selected clip, else of the clip under the playhead — the last clip's
at the end — else the project scene); keep only the `lane_mute` ids some
scene has (the project's or a take's: the timeline's lanes); on an fps
change convert `playhead` and `zoom` through nanoseconds and clamp the
playhead to the timeline length. `edit_target` is the scene layout edits
go to (GUI D-06): Live ⇒ the project scene; Program ⇒ the take of the
first selected clip, else `None`: the canvas still shows the take under the
playhead, read-only (`NO_EDIT_TARGET` says why).

UI commands: `RangeTo(c)` selects the smallest run holding the selection and
`c`; `Prev`/`Next` with nothing selected pick the clip under the playhead;
`SetUi.panels` entries merge into the stored map, and `SetUi.lane_mute`
replaces the set (ids no scene has are dropped, not refused, so a stale
window never fails). `Play` and `Shuttle` switch the canvas to Program
(playback renders there): at the end playback restarts at 0, an empty
timeline is `Invalid`, reverse at 0 stops; switching to Live and a starting
countdown or take stop it. The core keeps only `runtime.playing`; the GUI's
player owns the shuttle speed.

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
    pub after_hash: String,               // hash of the view after: shown by `qrec history`, and how an open places the cursor (A§7.6)
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
  diffed field by field. Other arrays are replaced whole, and so are objects
  whose members differ (a source switching `kind`), so no patch adds or
  removes an object member.
- Each `replace`/`remove` is **immediately preceded** by a `test` of the value
  at that path in the intermediate document (ops are generated by applying
  them one by one to a scratch copy). A failed test ⇒ the state drifted ⇒
  undo refused with `Invalid` instead of corrupting anything.
- Property: patch size grows with the number of changed clips, not with the
  timeline length; undo∘apply = id and redo∘undo = id over random command
  sequences, with every test op passing.

### A§7.3 Compaction, clear, restore

Above 5,000 entries the oldest are dropped down to 4,000; `history.base.json`
(`{"baseSeq","project"}`; a bare project is still read, its seq taken from
the head) becomes the project before the oldest kept entry (walking
inverses back from the current state) and `base_seq` its seq. The same walk
(`history::ops::derive_base`) gives back a base that is lost: an unreadable
`history.base.json`, or a missing one under a history with entries, is
derived on open from the project at the cursor and written with the open's
repairs (A§7.6); an entry whose inverse no longer applies is dropped with
everything older, as in compaction. `Clear` (`!`): no entries, base =
current project, `base_seq = cursor`. `Restore <seq>` (`!`) = undo/redo to it;
restoring the cursor itself applies no patch but its project is validated
all the same (it is what gets written, A§7.7).

### A§7.4 External edits (merge, not adoption)

On `ProjectChangedOnDisk(Ok(file))` the core merges:
- From the file only the undoable fields: `name, canvas, scene, recording,
  autocut, timeline, takes[*].scene`.
- `nextTakeNumber = max(file, current)`; takes missing from the file are kept
  (warning: "takes can only be removed with `qrec take purge`"); takes only
  in the file are added; for takes in both, the current `status, streams,
  durationFrames, recordedAt, error, placed, replaced, recovered` win
  (warning listing ignored pointers); the in-flight take is always kept as
  in memory.
  Layer and track `missing` flags are facts too: a take scene keeps the
  in-memory flag of every entry it already had, and the project scene never
  gets one.
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
`/takes/T` ops are removed. An entry whose patch no longer applies is
dropped with everything beyond it (it couldn't be undone or redone
anyway), as in compaction. Emitted as `HistoryOp::Rewrite`.

### A§7.6 Persistence order and the commit record

`.qrec/history.head.json` is the commit record:
`{"cursor":N,"baseSeq":B,"nextSeq":K,"projectSha256":H,"project":{…}}`.
`.qrec/history.cursor` is SPEC §6.2/§8.1's "current seq" (`N\n`), a copy of
the head's cursor for readers: written right after the head, never read
back, rewritten by the next commit or host open when stale. Per outcome
that saves (a `History` op or `WriteProject`), in order:
1. History: `Append` = one line via `O_APPEND` + `fdatasync`; redo-tail drop
   and `Rewrite` = tmp + rename (and `history.base.json`).
2. `qrec.json`: tmp → fsync → `rename` → fsync(dir). The tmp is
   `.qrec/qrec.json.tmp` (same file system), so a folder being initialized
   shows nothing but `.qrec/` to another command's folder rule (A§8.4).
3. `history.head.json`: tmp → fsync → rename → fsync(`.qrec/`). **This is
   the commit point.** Then `history.cursor` (a failure there is logged,
   not a failed commit).
4. Only then the effects that touch the world: `StartCapture`,
   `DeleteTakeDirs`, `Reply`, `Notify`, jobs.

Take folders (`CreateTakeDir`) are made before step 1 (after the A§8.9
check). A `StartCapture` whose folder couldn't be made or whose commit
isn't durable doesn't run: the core gets `CaptureFailed { take }` instead
(a purge's `DeleteTakeDirs` gets `PurgeFailed`, A§5.6).

An outcome that saves nothing writes nothing, even when the files need
repair: a failed commit or a repair the load found makes the next commit
that saves rewrite the history file, `qrec.json` and the head, and a host's
open (outside recovery mode) writes the repair right away (and an
unreadable `ui.json`; its toast says the files were repaired only when it
wrote them), unless its `qrec.json` didn't merge (A§7.7): that file is
never written over in place, so the repair waits for the write that moves
it aside or copies it first. A Session that goes away with a dirty store
tries one last full write (in recovery mode only once a request saved,
A§7.7; never over a `qrec.json` that failed to load). Non-undoable writes
do steps 2–3. On open (lock held): drop history lines with
`seq ≥ nextSeq`; drop a torn last line; a bad line elsewhere ⇒ `Invalid`
with its line number; derive an unreadable `history.base.json` (or a
missing one under entries) from the project at the cursor (A§7.3) and
write it as a repair; a base whose seq isn't the head's `baseSeq` (a
rewrite that didn't finish) wins and the head is written again. If
`sha256(qrec.json) == H` the state is clean.
Otherwise a valid file whose undoable view is the `afterHash` of the
uncommitted lines (they follow the head: the first is its `nextSeq`, then
one after the other, after a history ending at its cursor), and that still
has every take the head knows, is that commit's own write (a crash, or a
failed head write, between 2 and 3): load finishes the commit — keeps those
lines and the cursor at the last, takes the file with its facts (a
finalized take, its clip) and rewrites the head as a repair. Any other
valid file is an external edit against `head.project` (so is a commit
without a line, facts only, interrupted there: the head's facts win and
the Recover probe re-derives a take's); an unparsable or invalid one takes
A§7.7. Without a head (a project written by hand, a lost head) a valid
`qrec.json` is the project, the history lines are kept and the cursor is
the newest entry whose `afterHash` is the file's view (else the base's,
else the last entry, noted); the head is rebuilt as a repair.

**A failed commit** (disk full, permissions): a command's change didn't
happen. The Session keeps the previous state, runs none of the outcome's
effects and puts back what the commit wrote: `qrec.json` as it was before
step 2 replaced it, byte for byte, or no file if there was none (else the
next open would merge the change as an external edit; and the file isn't
always the head's project: a restore replaces an unusable or unmerged one,
A§7.7, which stays the person's), the history line (else the next open
drops it as uncommitted), a first base, and the base a history `Rewrite`
(purge, clear, compaction) replaced (the store keeps the bytes of both
files for that, `Store::mark`). It answers `io` "couldn't save the
project, so nothing changed" — an in-process CLI host exits right after it
replies, so a change can't wait in memory for a later save. An event's
outcome carries facts (a finished take, a recovery), and a change whose
files can't be put back stays too: those are kept in memory, the store
rewrites every file with its next commit, and the replies they send are
`io` "the change was made but couldn't be saved", not `ok`.

### A§7.7 Invalid or corrupt `qrec.json`

- Host running: `runtime.disk_invalid = Some(msg)`; user mutating commands
  return `Invalid` "qrec.json on disk is invalid: <msg>; fix it or run `qrec
  history restore <cursor> --yes`"; queries work; a later valid read clears
  it. Writes that must save facts (begin take, finalization, recovery) first
  move the invalid file to `.qrec/rejected/qrec-YYYYMMDD-HHMMSS.json` (never
  overwritten) and say so in a toast. A host whose open finds a file that is
  valid alone but not once merged (A§7.4: facts in memory win) is in the
  same state, and its open writes none of the repairs the load found
  (A§7.6): the toast says the files need repair.
- Opening: a corrupt file makes `Session::open` fail with `Invalid` and that
  hint, except with `OpenOpts { recovery: true }` (used by `qrec history` and
  `qrec history restore`), which loads `history.head.json` + history and
  never loads an unusable `qrec.json`: a missing or invalid one only sets
  `disk_invalid`. A valid file that differs from the head is a hand edit and
  is merged first, like on any open (SPEC §8.1: an "External edit" entry, so
  the restore can be redone back to it; it is not "broken").
- Recovery mode writes none of the project's state (history, `qrec.json`,
  head, `ui.json`) until a request saves (SPEC §11: an unconfirmed restore
  leaves them as they were; only child-process logs are written). Events'
  changes (that merge, recovered takes) stay in memory for the restore's
  commit, which rewrites everything, and their toasts only go to the log; a
  request such an event answers gets `io` "the change was made but isn't
  saved", never `ok`. Once a request saved (the restore, or a command of
  another client the host serves meanwhile, e.g. `record start`), the files
  hold everything and later events save as on any host (that recording's
  start offsets and finalization). While `qrec.json` is unusable a request
  that would save without replacing it is refused. Restore copies the
  corrupt file to `.qrec/qrec.corrupt-<ts>.json` before writing.
- If the head file is missing too, rebuild from `history.base.json` +
  forward patches (without a usable base there is nothing to recover from,
  and the error says whether the base is missing or unreadable; an open
  that had the head derived a lost base, A§7.3, so this rarely happens).
  Patches carry no take facts and the base usually predates the takes, so
  each take found under `takes/` joins the pool right before the first
  entry that uses it (its clip or its scene), `recording`, with
  the project scene of that moment (what `begin_take` copied) and `placed`
  if that entry put its clip on the timeline; takes no entry uses go to the
  bin. `nextTakeNumber` goes past every take any entry names (SPEC §5.3.1:
  a number is never reused, even when its folder is gone, or a redo would
  put an old take's clip on a new recording). The Recover probe finalizes
  the takes found; in recovery mode commands wait for those probes (and a
  host stays up for them, A§8.6), so the restore writes a project that
  validates. The other take facts a take's length depends on aren't in the
  media files, so a take found on disk reads them from its `recorder.log`
  (A§8.11; the first 1 MiB, the last line of a file winning): each
  stream's id and start offset (`<id>: <file> startOffsetNs=<n>`, logged
  once recording starts and again at the stop), and the streams lost before
  the stop (`<id> lost: …`), whose layers or tracks are then missing in its
  scene as the core marked them; so it comes out as long as it was
  recorded. A stream the log doesn't name (a take that crashed before it
  started recording, a lost log) starts at 0, with the id of the scene's
  layer or track whose file name it has, else the name up to its first
  `-`; such a take can come out shorter than recorded, and then a restore
  whose clips no longer fit fails naming the clip (restore an older entry).

---

## A§8 Shell

### A§8.1 Threading model

- In every host the `Session` lives on **one session thread** draining a
  `crossbeam_channel::Receiver<Msg>`; it never blocks on I/O other than its
  own small atomic writes and the bounded device discovery a command's
  facts need (A§4.2, A§8.12; a GUI's come from a cache, below).
  ```rust
  pub enum Msg { Request { req: Request, to: Box<dyn Responder> }, Event(Event),
                 DiskChanged, Interrupt, Shutdown, RetryLive, Abandon }
  #[derive(Clone)] pub struct Inbox(crossbeam_channel::Sender<Msg>);
  pub trait Responder: Send { fn accepted(&mut self); fn progress(&mut self, p: &Progress);
                              fn reply(&mut self, r: Result<serde_json::Value, CoreError>);
                              fn reply_late(&mut self, r: Result<serde_json::Value, CoreError>) { self.reply(r) } }
  ```
  Every request gets one answer: `reply` for its own outcome (also when it
  ran later, held for a recovery, A§5.3), `reply_late` for an
  `Effect::Reply` that an event's outcome sends (a started or finalized
  take, a finished export; GUI H-7). A request the Session never handled
  (still queued, parked or waiting for a late reply when its host stops) is
  dropped, so the responders someone waits on answer from their own `Drop`
  (`ConnResponder`, `ProxyResponder`, the GUI host's `AnswerOnDrop`).
  `Abandon` tells a Session its host stopped waiting (a GUI host's close
  grace ran out): `run_until` then ends with a recovery probe still out
  (A§5.3). The core toasts every late `Err`
  itself (an `on_event` outcome that replies one also has a `Notify`, held
  by `tests/props_late_errors.rs`), so the GUI only logs late errors and
  toasts immediate ones. `RetryLive` is the GUI's live-preview retry: the
  session thread calls `CaptureEngine::retry_live` (forget the sources the
  portal refused, those that failed to start and those that died, and
  rebuild, A§8.11; the default does nothing), in order with the `SetLive`s
  and `StartCapture`s it sent, and logs it; it changes no state.
- Workers are std threads that only talk to the session through `Inbox`:
  process runners, timers, the inotify watcher, IPC connections, and the
  capture engine thread. No tokio; ashpd runs under `futures_lite::block_on`
  on the capture thread. GStreamer bus messages reach the engine thread via
  `bus.set_sync_handler`, so no GLib main loop is needed outside the GUI.
- A GUI host runs the session thread too (`qrec-session`, A§8.3). Its
  `SessionConfig.observer` (`StateObserver::changed(rev, &State)`, GUI H-6)
  sees every state the Session holds: right after each assignment of it,
  durable or not (every outcome of a command or an event, runtime-only ones
  too: `RecordClock`, export progress, `playing`, recovery, autocut; a
  change kept in memory in recovery mode), and the opened state before
  `open_with` returns; always before that outcome's toasts, progress and
  replies, so a reply never reaches the window before the state it
  describes. `rev` counts the calls (`Session::rev`); an event the core
  ignores still hands over a state, maybe an equal one. The observer runs on
  the session thread and returns at once: the GUI's
  (`host::gui::PortObserver`) clones the state into an `Arc` for its queue
  (about 45 µs for a 300-clip project in a debug build *(measured)*; each
  call is timed, and the first and any over 1 ms are logged at debug level).

```rust
pub struct Deps {
    pub capture: Box<dyn CaptureEngine>,          // real, fake, or none
    pub media: Box<dyn MediaRunner>,
    pub clock: Box<dyn FnMut() -> (DateTime<Utc>, i32) + Send>,
    pub entropy: Box<dyn FnMut() -> u64 + Send>,
}
impl Session {
    pub fn open(dir: &Path, opts: OpenOpts, deps: Deps) -> Result<Session, ShellError>;
    pub fn open_with(dir: &Path, opts: OpenOpts, deps: Deps, cfg: SessionConfig) -> Result<Session, ShellError>;
    pub fn inbox(&self) -> Inbox;
    pub fn handle(&mut self, msg: Msg);
    pub fn run_until(&mut self, done: impl FnMut(&State) -> bool);   // CLI and hosts
    pub fn state(&self) -> &State;
    pub fn rev(&self) -> u64;                                        // states the observer saw
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
`SessionConfig { lock, channel, env, notifier, observer, sources,
recovery_wait }`: a host passes the lock it holds (A§8.3 step 1) and the
inbox its server already fills; without a lock, a mutating open takes it
(`Busy` when held). A GUI passes `facts::SharedSources` as `sources` (GUI
H-5): the device facts a worker of the window discovered last (`refresh`
discovers on the caller's thread; an empty cache discovers once, on the
thread that asks first, and a caller that comes during a refresh waits for
it), so `LayerAdd`, `AudioAdd` and a take's start never wait for `pactl`,
Wayland or the camera monitor on the session thread once a discovery ran.
During a responder's calls `session::current_request()` is its `ReqId`,
which an IPC responder keeps to post `RequesterGone` when its client leaves.

### A§8.2 Paths (`paths.rs`)

Runtime dir: `$QREC_RUNTIME_DIR`, else `$XDG_RUNTIME_DIR/qrec`, else
`/tmp/qrec-<uid>`; created 0700, then `lstat`-checked (real dir, our uid,
0700) or refused. Config `$XDG_CONFIG_HOME/qrec` (`config.toml`,
`keys.toml`: read whole, at most 1 MiB (`config::MAX_FILE_BYTES`) and
regular files only, a FIFO refused at once instead of waited on, by
`config_file::read_text`: the Sessions and the GUI through `config_file`
and `keys_file`, `qrec config` and `qrec keys` through
`Backend::settings_text`, which a build without the shell implements by
checking the file before it opens it; only `gpu.rs`'s early
`config_va_device` reads `config.toml` itself, with its own 64 MiB bound
and also regular files only), state `$XDG_STATE_HOME/qrec` (portal tokens 0600;
`gui-<first 12 hex of sha256(canonical project dir)>.log`, 0600, the log of
the GUI windows of one project: each window appends a header line `=== qrec
<version> pid <pid> <RFC 3339 time> <dir> ===` and its own lines, never
truncating, and holds a shared `flock` on it while it runs; a start moves a
log past 8 MiB to `.log.1` first, but only under the exclusive lock, so
never while a window writes to it (a second `qrec` on a held project
appends instead) and never twice at once; "host disconnected" names it for
a GUI host), cache `$XDG_CACHE_HOME/qrec`
(capabilities). Socket:
`<runtime>/<sha256(canonical project dir) hex>.sock`; refuse paths over 107
bytes. The publish name (A§8.3 step 1) is never longer, so every socket path
that fits works for hosts too, whatever their pid.

### A§8.3 Lock and socket protocol

Every process that holds the project lock is a **host** (Gui, Headless or
Cli) and serves the socket.
1. **Start:** open `.qrec/lock` (O_CLOEXEC), `try_lock` (exclusive, never
   blocking). Held ⇒ route (step 4). Under the lock: write
   `{pid, kind, socket, since, state:"starting"}` to the lock file, unlink
   `<hash>.sock`, bind `<first 40 hex of the hash>.<pid>.tmp` (shorter than
   the socket path), listen, `rename` it to `<hash>.sock` (atomic publish),
   set `state:"ready"`. This happens before recovery and `gst::init()`;
   early requests queue in the server until the Session is attached (a
   signal's messages go ahead of them, A§8.6). The host then opens its
   Session with a second descriptor of the lock (`dup`, same open file
   description): a second `open` + `flock` in the same process would be
   refused, and dropping the Session only closes its copy.
2. Only the lock holder unlinks its socket. Scanners never delete anything.
3. **Exit:** stop accepting → unlink the socket → `state:"stopping"` →
   flush replies → exit (closing the lock fd releases the lock). The exit
   rules only fire with no connection open, and the server stops accepting
   only while that still holds, under the lock admission takes: a host never
   admits (tells `accepted`) a client and then drops its request. A client
   the acceptor took but didn't admit sees EOF before `accepted` (step 6).
   Routing clients treat `starting` and `stopping` alike: retry.
4. **CLI routing** (deadline 3 s; 20 s for `record *`): connect. On
   ENOENT/ECONNREFUSED try the lock; if acquired, become the host (in-process,
   or hand off to a spawned headless host). If not, read the lock file: when
   `ready`, connect to its socket; otherwise retry every 50 ms. `hello` timeout
   2 s ⇒ `Busy` "host pid N is not responding"; deadline ⇒ `Busy` naming pid,
   kind and start time. A record outlives its host (it says `stopping`, or
   whatever a crash left), so these name a pid only while that process still
   runs and started before the record (not a reused pid); otherwise "another
   process holds the project lock … but runs no Qrec host".
5. **Spawn hand-off** (`record start`/`toggle` with no host): the CLI takes
   the lock and spawns `qrec host --headless --project DIR --lock-fd 3
   --ready-fd 4` (fds dup'ed in `pre_exec`, CLOEXEC cleared, `setsid`, stdio
   to `.qrec/logs/…-host.log`, every descriptor above 4 close-on-exec:
   nothing the CLI inherited from its own caller, such as a pipe that caller
   reads to its end or a key-bind script's `flock`, reaches the detached
   host to live as long as the take, A§8.7). flock belongs to the open file
   description, so the child keeps it; the CLI closes its fd and **never
   unlocks**. The host writes one JSON line on fd 4 (`{"ok":true}` once
   listening, or the `CoreError`); EOF ⇒ the CLI reports the host log path.
   It keeps fd 4 until its Session opened: if that fails (a corrupt
   `qrec.json`, A§7.7) it writes the error as a second line before freeing
   the lock, so a CLI whose request found the host gone reports that error
   and its hint (plus the log path), not "exited right after starting".
6. **Protocol safety:** the host sends `{"id","accepted":true}` once parsed.
   EOF before `accepted` ⇒ the request never ran: route again after 50 ms,
   like for a starting or stopping host, until the deadline (given once
   more from the first bounce); EOF after ⇒ exit 1 "host disconnected"
   with the log path (never retry: it may have run). A host also bounces
   this way what it doesn't take: a CLI command's own host bounces
   recordings while it runs no export (during one, a recording reaches
   its core, which refuses it at once like every host's, A§5.3), and new
   jobs once its command is answered (A§8.5). Past the deadline the CLI
   says so (`Busy` "the project is held by another qrec command (pid N),
   which doesn't record"). A client keeps its connection open until the
   reply: closing or half-closing it means the requester is gone
   (`RequesterGone`, A§5.8). A request that stays silent is checked with
   `hello` every 30 s.
7. Peers must have our uid (`SO_PEERCRED`). Request lines are capped at 1 MiB
   with a 5 s read timeout; each connection has a bounded 64-message queue
   (progress coalesced, replies never dropped) and a 10 s write timeout.

**GUI host** (`host::gui::GuiHost`, GTK-free; the window drives it and hears
it through a `GuiPort`, docs/GUI.md G§3.4.1). `GuiHost::start` runs step 1
on a worker of the window, under the lock the window took (free, or after a
takeover, A§8.6): the folder rule again (another process may have filled an
empty folder: `not_project`), the record says `starting`, the socket is
published, the window's `first` request (`Init` in an empty folder; an
error the Session gives it while the host serves is toasted, unless it is
"already a project"; errors that only say the host ends aren't: any before
a Session is attached, the refusal of a Session a signal interrupted, and
any once the host is stopping, since `Stopped` says why) is parked in the
server before its acceptor runs, so it reaches the Session ahead of every
client's
request (one that connects meanwhile waits in the listen backlog), signals
are watched, the record says `ready`. Its thread `qrec-session` then waits
for `open` (the Session opens there with a share of the lock and is
attached; the window gets its first snapshot), `fail` (GStreamer stuck, a
missing dependency), `close` or a signal. `fail` and an open that fails
answer every queued and later request with that error, remove the socket and
release the lock at once, so the CLI works in-process while the window says
why. The window's own requests (`GuiHost::submit`) join the CLI's in the
server's one FIFO, each answered exactly once: one the Session never
handled (still queued, parked or waiting for a late reply when the host
stops) answers from its wrapper's `Drop` (`AnswerOnDrop`) with why the
host stopped, and once the host finishes, requests are refused at once
with that reason (`Host::finish` fails the server before it drops the
Session, which can take seconds). `PortObserver` and `PortNotifier` hand
its Session's states (A§8.1) and toasts (the startup recovery's marked as
such) to the port. The port hears `Ready`; then `Opened`, `OpenFailed`, `Failed` or
`Crashed` (a panic on the session thread: requests get `internal`);
`Stopped` last, once the lock is released.

### A§8.4 Project resolution and init

`--project DIR` → cwd if it holds `qrec.json` or is strictly empty → if an
ancestor holds `qrec.json`, exit 3 with "you're inside project <dir>; run
there or pass --project <dir>" → the single live host found by `hello` on
`<runtime>/*.sock` (stderr: `qrec: using project <dir> (pid N)`; the exact
path from `projectDirHex` when it isn't UTF-8) → exit 3 (several hosts:
`Usage` listing them). `qrec init` never takes the host step: its target is
the folder itself, so a folder that is neither empty nor a project is exit 3
whatever runs elsewhere. Init order: `mkdir .qrec` → lock → re-check the
folder rule (a folder whose only entry is a `.qrec/` without a history
counts as empty, here and in the resolution above: another command may be
initializing it, and two commands started together in a fresh folder must
not race to exit 3) → write `qrec.json` + head → `mkdir takes`. In an
empty folder only a command with `Command::creates_project()` (project
set, layer and audio edits, autocut settings, record start/toggle) sends
`Init` first (the CLI's
`OnEmpty::Init`, the Session's lazy init); every other command gets
`not_project` "this folder has no Qrec project yet" (`apply::no_project_yet`,
exit 3) from the CLI before anything is opened or written, and queries are
answered as empty. A folder without `qrec.json` whose
`.qrec/` holds a history (`history.head.json`, `history.jsonl` or
`history.base.json`) is a project that lost its file, whatever else is in
it: it is that project (from the cwd or `--project`), never "Not my swamp"
and never initialized again; `qrec history [restore]` work on it in recovery
mode (A§7.7) and other commands get the A§7.7 `Invalid` with the restore
hint. Read-only commands take no lock and write nothing (no migration
backups either). A valid hand edit a read-only open finds is shown as the
project a host would merge (`history::external::merged_preview`), with the
history as it is on disk: no "External edit" entry, cursor or seq is made
up, and no merge is announced; the next host records it (A§7.4).
`--over-selection` and `--after-selection` with no live host ⇒
`Unavailable`, whose hint offers the GUI or naming the clips ("open the
project in the Qrec GUI and select clips there, or name them with --over
<clip>…" / "…or insert it with --after <clip> or --at <index>"), never a
recording: a host that records answers both `busy` (A§5.3). A current
directory that can't be read (it was deleted) only matters for what is
relative to it: an absolute `--project` (and `--out`) works without one,
like `qrec host`; no `--project` is `io` "can't read the current directory
(was it deleted?)" with the hint "cd into an existing folder, or pass an
absolute --project DIR", and a relative `--project` or `--out` is `io`
naming the flag.

### A§8.5 In-process commands

With no host, a command runs in-process and its process **is** the host
(`HostKind::Cli`) until it is done: short commands finish immediately;
async ones (export, frame save, autocut with analysis, purge) keep serving
IPC so other commands run against the same Session (SPEC §13.1 "editing can
continue"). `seek`/`play`/`pause`/`shuttle` without a GUI host ⇒
`Unavailable`: with no host at all, the CLI's hint is "open the project
in the Qrec GUI (run qrec in its folder) and try again", the core's own
for another host; no other host answers them (a recording's refuses
`play` as `busy`, and `seek`/`pause` as `Unavailable`).

Such a host belongs to a command someone is waiting for in a terminal, so
nothing of anyone else's may be tied to it for long:

- **It never records** (SPEC §4.3: a recording runs in a setsid'd headless
  host). The core refuses Start/Toggle there (`Busy`, A§5.3), the shell
  gives it no capture engine, and its server bounces `record start|toggle`
  before `accepted` (A§8.3 step 6; the CLI, seeing `host: "cli"` in
  `hello`, doesn't even send them): the client routes again every 50 ms
  until that command is gone (≤ 20 s), then spawns its own headless host.
  That wait is only for a command that runs no export. Recording is
  blocked during an export whichever process runs it (SPEC §13.1): when
  the `hello` lists an export, the client answers at once with the core's
  own `Busy` "an export is running (job-N)" and its hint
  (`export_blocks_recording`, exit 6), as a GUI or headless host does; and
  the server, told by its host loop that an export runs
  (`IpcServer::set_exporting`), lets a recording through to the core,
  which gives the same answer, instead of bouncing it.
- **Its result comes at once.** The command's reply is printed as soon as
  it comes (`RequestEvents::replied`), even while the process still runs
  other clients' requests or finishes the startup recovery its open began
  (which a stop never waits for, A§5.3; `Session::run_until` asks the
  host's `done`, where the reply is passed on, after every message); a
  signal after that only adds the stderr warning and exit 130 (A§8.6).
  Toasts that come after the reply are about other clients' requests and
  aren't shown, except the recovery's, the process's own news
  (`Notifier::notify_recovery`).
- **Once answered it takes no new job** (export, frame save, autocut,
  purge: bounced like a recording, `IpcServer::own_command_done`), keeps
  serving queries and quick edits, and exits as soon as what it still runs
  for others is over and no connection is open: no idle grace (A§8.6).

### A§8.6 Hosts: exit, signals, GUI start

- Exit (Headless and Cli): once Idle + no running jobs + no recovery probe
  out (`runtime.recovering`, `Runtime::is_busy`) + no open connections has
  held for 2 s, or (Headless) 60 s after `ready` without a command or query
  (`hello` and `shutdown` don't count). An
  in-process command's host exits as soon as its command is answered,
  nothing is busy and no connection is open, whoever else used it (A§8.5;
  it still finishes the startup recovery it began, A§5.3: its answer would
  otherwise be lost and the take would stay `recording`; the command's
  reply is printed meanwhile, the recovery's toasts after it). `shutdown`
  requests: stop accepting, finalize a recording (`RecordStop`), let jobs
  finish, exit.
- Exit (GUI): a window holds the host, so it never ends for being unused
  or idle, and it refuses `shutdown` (`busy` "the Qrec window holds this
  project; close it there", GUI D-13). `GuiHost::close` (the window closed,
  after it sent what ends a recording or an export, GUI D-12) takes the
  window's FIFO (a status query whose answer asks to close), so everything
  the window submitted before runs first; then the record says `stopping`
  and the host stops accepting. It exits (`HostEnd::Closed`, exit 0) once
  nothing is busy and no connection is open — autocut analyses don't
  count: the close drops them (D-12; dropping the Session cancels their
  ffmpeg runs before anything is cut, also the analysis "autocut after
  every take" starts for the last take), though one a CLI still waits for
  keeps its connection open — or `ExitRules::close_grace` (60 s) after the
  close whatever still runs: the Session then stops waiting for a recovery
  probe too (`Msg::Abandon`, A§5.3), and dropping it cancels jobs and
  finalizes a take within their own deadlines. Before its Session opens, a
  close or the first signal stops it at once, and what was queued gets the
  reason. UI-state commands are never held for a recovery (A§5.3, GUI
  D-47), so the window stays live while one runs.
- Signals in every host: the first SIGTERM/SIGINT/SIGHUP dispatches
  `RecordStop` (finalize, not cancel) and cancels jobs; exit when idle or
  after 15 s (a recovery probe still out counts as busy only that long).
  Its `RecordStop` and `Msg::Interrupt` go ahead of requests parked while
  the Session opened. From then on the Session runs no command that hadn't
  started — those held for a recovery (A§5.3), parked before it opened or
  arriving later get `busy` "this Qrec host is stopping" — except the ones
  that stop something (`RecordStop`, `RecordCancel`, `ExportCancel`, a
  `RecordToggle` that stops), and an in-process host never sends its own
  command once signalled. A second signal SIGKILLs the child process
  groups. A headless (or GUI) host then deletes the temp outputs of its
  running jobs and `_exit(130)`s; startup recovery handles the take. An
  in-process command (someone reads its output) instead stops serving as
  soon as its jobs report the end of their killed children (they delete
  their temps), at most 2 s later (`ExitRules::hurry_grace`; whatever is
  left is killed and its temps deleted then), so it still ends the normal
  way below; a watchdog `_exit(130)`s 5 s after the second signal if the
  process is still there.
- Client CLI: SIGINT/SIGTERM (`host::signals`, A§1.2) → `Msg::Interrupt`.
  In-process: the first cancels running jobs (SIGTERM to each child group,
  SIGKILL after 2 s), deletes temp outputs, exits 130; the second SIGKILLs
  them at once, and the run still ends as below with exit 130.
  IPC-routed: Ctrl+C closes the connection and exits 130; for `record --wait`
  the first Ctrl+C sends `RecordStop` on a second connection and keeps
  streaming until finalized, a second detaches (two presses count as two
  even when they arrive together). A host answers `accepted` only once the
  request is queued for its Session, so that stop always comes after the
  start: it stops the take, or cancels the countdown or the start still
  waiting for a recovery (A§5.3), and the start's reply ends the command.
  When the stop finds nothing to stop (`invalid`) and the start stays
  unanswered 1 s more (a host that holds the start where a stop can't
  reach it), the client detaches like on a second press, so that host
  drops the start (`RequesterGone`). Under `--json` an interrupted run still
  ends stdout with one object: the reply when one came before the signal (a
  command that finished or committed; printed only once, also when it was
  printed as it came, A§8.5), else
  `{"ok":false,"error":{"code":"interrupted",…}}` whose hint says whether
  the host may still finish it, with the host's own error (e.g. "export
  cancelled") in `details.reply`. The hint follows A§5.8: in-process, the
  jobs were cancelled and their temps deleted; a headless or CLI host
  "cancelled it when this command stopped; run the command again" for an
  export, frame save or autocut; otherwise (a GUI host, or a recording,
  edit or purge) "the … host (pid N) may still finish it". People get a
  warning on stderr. Stdout is
  written with `writeln!`; `BrokenPipe` exits quietly, any other write
  error (a full disk) is said on stderr and makes a run that would exit 0
  exit 1.
- GUI start with the lock held (`host::takeover`; the window's pages drive
  it, docs/GUI.md G§3.4.3). `find_holder` takes the lock when it is free,
  else asks `hello` (2 s) on this runtime's socket, then on the one the lock
  record names: a host answers (its kind, record phase and jobs); or none
  does yet, starting or stopping (ask again every 50 ms; after 3 s it counts
  as not responding, `unresponsive`: the lock record while its pid runs); or
  its socket took the connection but `hello` got no answer (`Unresponsive`).
  A recording (SPEC §4.3, Stop & open): `stop_recording` (`RecordStop`,
  whose reply comes once the take is finalized, `Stopped::Finalized`; ≤ 60
  s, else `busy`; when the holder still counts down, the stop cancels the
  countdown and the reply comes at once, `Stopped::Cancelled`, with no
  take), then
  `request_shutdown`, then `wait_for_lock` (`try_acquire` every 50 ms,
  ≤ 20 s, else `busy` naming the holder); Quit leaves it recording. A
  holder in its countdown has no take to show yet: the window asks `hello`
  again, like for one starting, until it records or is idle. An
  export: wait until the holder's `hello` lists no job (every 500 ms; the
  percent from `export_progress_view` over its jobs), then
  `request_shutdown` (a CLI command's own host exits by itself) and
  `wait_for_lock`. Idle: `request_shutdown` (2 s; a host already gone is
  fine) and `wait_for_lock`. A GUI holder refuses `shutdown`, so a second
  window on its project quits (exit 6). Each request goes on a connection
  of its own. The GTK app uses `ApplicationFlags::NON_UNIQUE` and
  `run_with_args(&[])` after clap.

### A§8.7 Process runner (`proc.rs`) and logs

Two runners apply these rules: `proc.rs` (the session's: capability
checks, `pactl` facts) and `media/child.rs` (the media runner, `sources`,
`doctor`). Every child: stdin null, `process_group(0)`, stdout piped only
when parsed (drained on its own thread), stderr appended to a log file
(an anonymous temp file for project-less runs: `sources list`, `doctor`), and
nothing else (but an output's temp file, which the ffmpeg writing it keeps
under the same descriptor number, A§8.8): in `pre_exec`, every descriptor
from 3 up is made
close-on-exec (`proc::close_on_exec_from`: `close_range(…,
CLOSE_RANGE_CLOEXEC)`, else an `fcntl` sweep below `RLIMIT_NOFILE`; never
closed outright, so std still hears a failed `exec`). Rust opens everything
close-on-exec already; this also keeps out what Qrec itself inherited from
whoever started it. The headless host spawn (A§8.3 step 5) and the GPU
probe child do the same.
Arguments are passed byte for byte (`OsString`): the media runner replaces a
plan's last element with `file:<temp path>` (a segment's; and the segment
check's input) or `file:/proc/self/fd/<n>` (an export's or frame's, A§8.8)
keeping the path's exact bytes, so a project folder whose path isn't UTF-8
renders like any other. Paths the shell reports (`ExportFinished`,
`FrameSaved`, `OutputAppeared`, log paths in hints and `details.log`) are
exact, or written `\xHH` byte by byte where they aren't UTF-8
(`paths::path_text`), never with a lossy `�`.
Timeouts: ffprobe 20 s, pactl 3 s, VA/encoder probes 10 s, `wl-copy` 5 s
(`--type image/png`, stdin from the PNG file). ffprobe and silencedetect get
30 s more per GiB of input, in proportion (`-count_packets` reads the whole
file), so small files get exactly the base. Long ffmpeg jobs have a stall
watchdog (no `-progress` block for 60 s ⇒ kill, "ffmpeg stalled"). Kill:
SIGTERM to the group, SIGKILL after 2 s, wait ≤ 2 s more, then hand the
child to a reaper thread and fail the job — the session thread never blocks in
`wait()`. Success = exit 0 and `progress=end` (and, for segments, the counts
in A§9.3). Logs: `.qrec/logs/YYYYMMDD-HHMMSS-<pid>[-j<job>]-<step>.log`
(O_EXCL; `-j<job>` only for a job's steps: a probe's is
`…-<pid>-probe-<take>-<stream>.log`); at host start prune to the newest 200
and delete temp files older than 1 h in `exports/`, `frames/`,
`.qrec/cache/segments/` and the silence cache
(`.qrec/cache/silence/<key>.json`: analysis results, reused for the same
`SilenceKey`). A temp file is a regular file whose whole name has the
shape A§8.8 gives temps, `.<stem>.qrec-part-<pid>-<job>` plus an optional
`.<ext>` (`media::is_temp_name`), never any name that merely contains
`.qrec-part-`: exports, frames and results kept aside (`<stem>-kept.<ext>`)
are never deleted. Auto names can't take that shape (`file_stem`, A§9.9)
and a hand-picked `--out` in it is refused (A§5.9).

The GUI's thumbnail, frame and waveform runs (`media::preview`, docs/GUI.md
G§3.11) follow these rules with one rolling log instead of one per run: their
stderr is piped to a thread that reads it line by line (`child::Stderr::Lines`)
and appends each line, prefixed `[<pid> <kind>]`, to `.qrec/logs/preview.log`
(the command first, how the run ended last), moved to `preview.1.log` past
8 MiB; the first failed run of a file version and kind (per cache) also
writes its last 30 lines to its own `YYYYMMDD-HHMMSS-<our pid>-preview-<kind>.log`,
and later ones only add a line naming it to `preview.log` (and a video whose
header can't be read answers from memory for 30 s: its chunks share one
probe). Timeout 20 s + 30 s/GiB of input, at most 2 at once (1 while an
export runs). Both prunes (`proc` and `media`) count only timestamp-named
logs (`media::is_stamped_log_name`: `YYYYMMDD-HHMMSS-….log`) and leave
`preview*.log` and any other name alone, so preview runs that work never
evict export and host logs, and a broken file costs one such log per kind.

### A§8.8 Output files: never overwrite

Temp outputs live in the destination directory as
`.<stem>.qrec-part-<pid>-<job>.<ext>`, the stem cut so the name fits in
`NAME_MAX` (255 bytes): any destination name that fits gets a temp that fits.
A hand-picked name over 255 bytes is refused up front (`facts.out.forbidden`,
A§5.9). The final step is
`renameat2(RENAME_NOREPLACE)` (fallback `link` + `unlink`). Auto names loop
`-2`, `-3`, … on EEXIST; a hand-picked `--out` is replaced only when the
plan says so (`replace`: the file that existed at dispatch and the person
confirmed, by identity, A§5.9 — never guessed from the file system when the
job starts), and only while the entry there is still that file
(`fstatat(AT_SYMLINK_NOFOLLOW)`). A
hand-picked unconfirmed `--out` that appeared meanwhile ⇒ keep the result
(it is the result now): the temp is renamed, without replacing anything,
to `<stem>-kept.<ext>` next to `out` (then `-kept-2`, `-kept-3`, …; the stem
cut to fit `NAME_MAX`), a name the A§8.7 sweep never deletes (only if that
rename fails does it stay at the temp path, with a warning), and the job
ends with `OutputAppeared { out, bytes, kept }`, which the core turns into
`confirmation_required` naming both paths (exit 4).
`ExportFinished`/`FrameSaved` carry the real path. Migration backups use the
same no-replace rule.

**The destination folder is held open.** When an export or frame save
starts it opens the folder `out` names (`O_PATH | O_DIRECTORY`, symlinks
followed as the OS reads the path then) and never looks it up by path
again: the temp file is created in it with `openat(O_CREAT | O_EXCL |
O_NOFOLLOW)` (a new file, never one that was there, never through a
symlink); ffmpeg keeps that file's descriptor (only its close-on-exec flag
is cleared in the child, so nothing of the child's own is moved) and writes
`/proc/self/fd/<n>` (so re-opening it, as the mp4 muxer does for
`+faststart`, reaches the same file, and every muxer and encoder is named
in the argv, never inferred from a name, A§9.2); the size and PNG checks
and the `fsync` use the job's own descriptor; and the final
`renameat2`, a result kept aside and a temp's deletion (cancel, failure,
hard exit) are relative to the folder's descriptor. The folder is checked
against `takes/`, `.qrec/` and `qrec.json` at the start, before the temp
is created and before the final rename (A§5.9). A folder on the path
swapped for a symlink while the job runs leads the output nowhere new: it
lands in the folder the job opened, reported under the path the plan gave
while that still leads there and where the folder is now otherwise; a
folder deleted meanwhile fails the job ("the folder … was deleted").
Segment temps and the concat list live in the project's own cache and are
written by path.

### A§8.9 External edits watcher

Watch the project **directory** (`IN_CLOSE_WRITE|IN_MOVED_TO|IN_CREATE|IN_DELETE`),
filter on `qrec.json`, debounce 200 ms, then compare the bytes' sha256 with
the hash of what the host last wrote: equal ⇒ ignore (our own write);
different ⇒ parse + migrate + post `ProjectChangedOnDisk`. Before an
outcome writes anything (its take folders, the history, `qrec.json`) the
host re-checks the file against that hash; if it changed, the external edit
is processed first and the request re-dispatched against the new state (no
effect of the old outcome has run yet). After 3 re-dispatches in a row the
outcome is saved over the change.

**The user's settings** (`watcher::watch_settings`, GUI WP10, docs/GUI.md
G§3.14): the window follows `config.toml` and `keys.toml` live. A thread
of its own (`qrec-settings`) watches the config dir the same way (the
directory, `IN_CLOSE_WRITE|IN_MOVED_*|IN_CREATE|IN_DELETE`; reads never
count), from the nearest existing folder while the config dir doesn't
exist yet, moving the watch in once it is made (and back up when it goes,
by its inode). A burst is one change: `SettingsEvent::Pending` at its
first event, `Settled` once quiet 200 ms (at most 1 s) with both files read
there (`Settings::read`: the config through the window's `ConfigFile`,
whose problem it takes, so the "config.toml is invalid" warning is still
once per change of the file between the window and its Session; the keymap
and its problems through `keys_file::load`), and one pair as soon as the
watch is up. Dropping the watcher waits at most 2 s for its thread. Hosts
without a window don't watch: a Session reads `config.toml` again whenever
its size, mtime, inode or change time changed (`ConfigFile`), and
`keys.toml` per `Query::Keys`.

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
  The GUI also gets `GDK_DISABLE=vulkan` (GUI H-3), so GTK renders with
  GSK GL: Mesa's Vulkan loader opens every render node, NVIDIA's included,
  while it enumerates devices, even with the driver pinned *(measured,
  docs/GUI.md G§3.13)*. A user who set `GDK_DISABLE` or `GSK_RENDERER`
  keeps both as they are, and every command notes it (`doctor` prints the
  note).
- In the same step (`gpu::plan_gst_env`): `GST_REGISTRY_1_0` becomes
  `$XDG_CACHE_HOME/qrec/gstreamer-registry-<arch>.bin` unless the user set
  it, so other GStreamer apps never make Qrec's registry stale and the
  plugin scan (whose `va` plugin opens every render node) runs only when
  Qrec's plugins change; `LV2_PATH`, `LADSPA_PATH` and `FREI0R_PATH` become
  empty, so the scan never loads the user's effect plugins (Qrec uses none).
- Commands that only need ffmpeg never initialize GStreamer.
- `gst::init()` runs on a helper thread; the caller waits ≤ 30 s, then prints
  "GStreamer is stuck scanning plugins (a GPU driver is probably hung); see
  `qrec doctor`" and `process::exit(5)`. Never on the GTK main thread.
- The VA H.264 element is found by enumerating `va*h264enc` factories and
  matching each one's `device-path` to the node — never by name (the first
  usable node gets plain `vah264enc`).
- Probes that touch a render node (the h264_vaapi check, doctor) run as
  child processes with a 10 s timeout. `main` calls
  `gpu::run_probe_if_requested()` before anything else: a `qrec` re-run
  with `QREC_GPU_PROBE=va-h264-factory:<node>` (and that node's
  `LIBVA_DRIVER_NAME`) is the probe child that finds the `va*h264enc` bound
  to the node (`VaapiCaps::gst_h264_factory`), prints it and exits. Only Gui
  and Headless hosts run it. Under `QREC_CAPTURE_BACKEND=fake` no render
  node is probed at all (no factory, no h264_vaapi check, none in `doctor`).
- The Nix package ships gst-plugins-bad without `nvcodec` and `qsv`, which
  hang the plugin scan on the dev machine.

### A§8.11 Capture engine

```rust
pub trait CaptureEngine: Send {
    fn set_live(&mut self, live: Option<LiveScene>, out: Inbox); // build/update the live pipeline; idempotent
    fn start(&mut self, plan: CapturePlan, out: Inbox);           // attach record bins at T0
    fn stop(&mut self, take: &TakeId, cancel: bool);              // EOS record bins, detach
    fn shutdown(&mut self);
    fn retry_live(&mut self, out: Inbox) {}                       // the GUI's retry (Msg::RetryLive, A§8.1)
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

**For the GUI** (docs/GUI.md G§3.9):
- **Preview sink** (`EngineOptions.preview`, GUI H-1): the preview branch
  ends in `compositor ! video/x-raw,format=BGRx,<fit_size(canvas,
  max_size)>,framerate=fps ! queue leaky=downstream max-size-buffers=1 !
  <the GUI's sink, sync=false async=false>`; compositor pads get the
  layout's rects × output / canvas, edge by edge. The sink is one element
  for the engine's life: a torn-down pipeline lets go of it at NULL, the
  next one takes it. Its errors are preview problems (`LiveError`), never a
  source's. Without one: `compositor ! <canvas size> ! videoconvert ! RGBA !
  appsink` → `SideEvent::Preview` (tests). A new canvas (size, rate,
  background) or new preview layers rebuild; a layout only moves pads and
  sets crops. A rebuilt pipeline is laid out (crops and pads) before it
  starts, at the size each source will deliver: a camera mode's own size,
  else the last one that source (its whole spec: another mode or device
  misses it) delivered; a source that dies before it negotiated forgets
  it, so a crop sized for larger frames than the new ones (which leaves no
  caps) is never used twice.
- **Crops never race a negotiation.** A `videocrop` reads its crop once
  per caps transform, so a crop that changed while a source's caps query
  crossed it (two transforms, each with another crop) left no common caps
  and the source died "not-negotiated", and a take recording it lost that
  stream. So each preview layer is `queue ! videoscale ! videocrop ! pad`:
  the `videoscale` (passthrough while the sizes match) answers the source's
  caps and accept-caps queries for any size, whatever the crop. While the
  pipeline streams, the engine thread never sets a crop: a layout that
  changes one leaves it, with the pad's geometry, in the layer's slot,
  and a probe on the `videocrop`'s sink applies both on the streaming
  thread before the next caps event or buffer (a still screen: at its
  500 ms keepalive); a layout that keeps the crop moves the pad at once,
  unless the probe is writing a layout it took (it does so outside the
  slot's lock): then it waits in the slot for the next frame too, so the
  older layout never lands over it.
  Before the pipeline starts (a rebuild is laid out first) both are set at
  once. Only the crop sides that change are written (each write makes the
  element renegotiate), and record bins link to their tee without
  RECONFIGURE, so a take's start doesn't make its sources query their caps
  again. Tests: `transient_crops_never_lose_a_source` (layouts every
  millisecond through rebuilds) and `takes_keep_every_stream_under_transient_crops`
  (takes under the same hammer).
- **`LiveControl`** (`control()`): `transient_layout(Some(scene))` lays the
  preview out (pads at once, changed crops on the streaming thread) from
  that scene's layers, without a rebuild, until the next `SetLive` (any)
  or `None` (D-32, D-33). Preview only: it never reaches a take. Its
  `progress()` (`LiveProgress { settled, generation }`, from a
  `LiveTracker` the engine shares) says whether every `set_live` and
  `retry_live` so far was handled and each video source of the live
  pipeline negotiated in it (or died; trivially without a pipeline), and
  which pipeline the preview comes from. The asks are counted on the
  caller's thread, so a reader after a request's answer never sees the
  old pipeline as the new one; the engine thread publishes after each
  command. A rebuild whose sources keep their states and sizes sends no
  `LiveStatus` (a rebuilt branch reports its cached size at once), so
  this is how the GUI's test channel waits for it (GUI.md G§3.18).
- **Left out**: a screen the portal refused, or a source that failed to
  start, stays out of the live pipeline, which runs with the rest, until
  the scene's sources change (a layout change, or Program mode and back,
  keep it out) or `retry_live` (D-50), which also forgets the dead
  sources and the casts of screens that failed or died, and rebuilds (a
  take running: once it ends). A source error, a stall while recording, or
  its stream's EOS makes a branch dead: the next live scene, take or retry
  rebuilds it.
- **Cast grace** (GUI H-4): once no live scene is wanted (and no take
  runs), screen casts stay open for `cast_grace` (the GUI's 60 s; zero
  elsewhere), so a live scene within it asks the portal nothing; then they
  close.
- **`SideEvent::LiveStatus`**, whenever it changes: the live scene's
  sources in its order (layers in the scene's array order, bottom first
  and the last on top, then audio; one per device; consumers look a source
  up by name, `live_sources_view`): `Live` with the size it delivers (the
  expected one until it negotiates), `Dead { message }` (it died, or
  failed to start), `Refused { cancelled }` (`cancelled`: the portal's
  "cancelled in the dialog"), `AwaitingPortal` (a screen asked of the
  portal, also again: before its old dead branch or refusal). A source not
  started yet (its pipeline waits for another screen's answer, or for the
  take to end) has no entry; no live scene, none.
- **`MeterMonitor`** (`capture/meters.rs`, D-03): per device, `pulsesrc
  device=X ! audioconvert ! level interval=50ms ! fakesink sync=false` (fake
  backend: the engine's tone) on `qrec-meters`; readings go out as
  `SideEvent::Level` from the bus's sync handler. `stop()` returns at once
  (no reading after it); the pipelines reach NULL within 5 s
  (`is_finished()`). A device that can't open stays idle (logged).

**Record bins** *(measured)*: T0 = the pipeline running time chosen at start;
a buffer probe on each bin's sink pad drops buffers with PTS < T0 and
`startOffsetNs = first passed PTS − T0`; T1 = running time at stop, video is
filled to T1 before EOS. A stream's first buffer at or after T0 always
passes, even past T1: a stop while Starting (a double-tap of `r`) would
otherwise close bins that got nothing, and those write a header-only file
nothing can read, which turned a too-short take into a `broken` one
*(measured)*. Such a bin waits for that first buffer until the start's
first-buffer deadline, then closes like the others. Once every bin passed
its first buffer (`CaptureStarted`), `recorder.log` gets one
`<id>: <file> startOffsetNs=<n>` line per stream (the stop summary repeats
them with `eos=`), a lost stream gets `<id> lost: <message>`, and the stop
`stop|cancel at T1 = …`. A rebuild without `qrec.json` and the commit
record reads them back (A§7.7; `spec::start_offset_line`,
`stream_lost_line`, `stop_line`, `read_take_log`).
A buffer about to reach a bin that has no segment yet gets the bin's zero
segment pushed first (with the stream start and the caps if those are
missing too), and `recorder.log` gets `<id>: no segment before the first
buffer; pushed one` (once per bin; `record::segment_pushed_line`). Once, a
stop-and-cancel while starting gave the audio bin a buffer before any
segment ("Got data flow before segment event" on every pad of the bin,
then `gst_segment_to_running_time` assertions in the muxer) and its file
came out unreadable; the round 3 integration's repeated runs never
brought it back, so the cause is unknown: the guard keeps the file valid
and leaves the line as evidence. `a_record_bin_gets_its_segment_before_a_buffer_without_one`
drops the segment on its way into a bin (the same warnings without the
guard) and checks the file reads like finalization reads it.
- Video: `queue ! videorate skip-to-first=true ! video/x-raw,framerate=N/D !
  videoconvert ! video/x-raw,format=I420|NV12,colorimetry=bt709 ! <encoder>
  ! h264parse ! matroskamux offset-to-zero=true ! filesink`. Without
  `skip-to-first`, a bin attached after N s of Live mode starts with N s of
  frozen frame.
- Audio: `queue ! audioconvert ! audioresample ! audiorate skip-to-first=true
  ! audio/x-raw,rate=48000 ! flacenc blocksize=4608 ! matroskamux
  offset-to-zero=true ! filesink` (the block size is pinned: a crashed
  file's length is its packets × 4608, A§2.2). A stream shorter than one
  FLAC block (4608 samples) is padded with silence to 120 ms before EOS:
  flacenc sets its caps with its first block, and an earlier EOS pushes the
  segment before the caps *(measured)*.
- Screen branch: `pipewiresrc ! capsfilter ! appsink` → bridge → `appsrc !
  videoconvert …` (`capture::bridge`; the appsink is `async=false`, so the
  pipeline plays without waiting for a screen's first frame, and a screen
  that sends none fails the first-buffer check, naming it). niri offers
  DMA-BUF only (BGRx with modifiers; shared memory fails with "no more input
  formats") *(measured, niri 2026-08, PipeWire 1.6.6, GStreamer 1.26)*. The
  capsfilter (its caps set after linking, which DMA_DRM caps would fail)
  takes **linear** DMA-BUF (`XR24/AR24/XB24/AB24`) or shared memory (GNOME,
  KDE). Linear frames are CPU-mappable (a 2560×1440 frame copies in ~2 ms);
  GL import samples black and the tiled/compressed layouts lose their extra
  planes in pipewiresrc, and `vapostproc` can't import `XR24` *(measured)*.
  pipewiresrc wraps niri's frames in 1-byte memories (PipeWire's data size),
  so the bridge resizes each dmabuf memory to the buffer object's size
  (`lseek(fd, 0, SEEK_END)`). It converts each frame to I420 bt709 (a pooled
  buffer, ~4.5 ms for 2560×1440 on one thread *(measured)*) while
  pipewiresrc still holds it, so the compositor never renders into a frame
  being read, and pushes it on with its timestamps (`videoconvert` then
  passes it through). `cast_probe` (an ignored test) reruns these
  experiments against a real portal session.
- **Still screens** (screencasts are damage-driven: no damage, no frames).
  Never `pipewiresrc keepalive-time`: PipeWire 1.6 stamps its keepalive
  buffer with the absolute clock time, and basesrc waits for that time
  (hundreds of hours) before pushing it, so the branch stalls for good the
  first time the screen is still for a frame period; a 10-minute take then
  ended with ~18,000 fill frames that couldn't be encoded within the EOS
  timeout *(measured)*. Instead the bridge's keepalive thread pushes the last
  converted frame again (a new buffer over the same memory, so no
  conversion), stamped with the running time, once no frame arrived for
  `KEEPALIVE_IDLE` (500 ms), and every 500 ms while the screen stays still;
  pushed PTS only grow. Repeats only fill long silences: a repeat stamped
  close before a real frame would take that frame's slot in the record bins'
  `videorate` (nearest frame wins), dropping it and doubling the old one, so
  no repeat goes out while a frame is being converted, and a screen that
  changes at any real rate (≥ 2 fps) is never repeated. The engine kicks the
  keepalive when it attaches the bins (a repeat at T0, so a still screen's
  bins start there) and around T1 (one repeat stamped before it, which
  passes, and one after, which closes the bins at once); a kick during a
  conversion goes out right after that frame. The thread ends at EOS or
  when the appsrc is gone, and every wait has a timeout.
- **Stalls.** A camera can hang without an error (uvcvideo USB errors). Its
  bin would then wait for a T1 it never reaches and be filled at the stop
  for as long as the hang lasted, more frames than the EOS timeout lets it
  encode, and the whole take would end `broken` *(the failure the
  still-screen stall above produced)*. So while recording, a video stream
  whose last passed frame is more than `STALL_LIMIT` (3 s) old is lost
  (`StreamLost`, SPEC §16: its file ends where it hung, unfilled, and the
  layer is marked missing), and a stop fills at most that much. Its live
  branch is marked dead, so a kept live pipeline (the GUI's) rebuilds it
  after the take instead of attaching the next take to a silent tee.
  Still screens never stall (the keepalive); audio isn't watched (a stop
  doesn't fill it). `FakeFailure::Stall` hangs a fake source for the
  tests.
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
  Fake screens are BGRx through the same bridge as real ones; `FAKE-1` is a
  still test card sent once (like a damage-driven compositor), so every
  later frame of it is the keepalive's, and `FAKE-2` is a moving ball. Its
  test hooks (`QREC_FAKE_*`) are in A§11.

### A§8.12 Sources and doctor

```rust
pub struct SourceInventory { pub screens: Vec<ScreenInfo>, pub cameras: Vec<CameraInfo>,
                             pub mics: Vec<AudioDeviceInfo>, pub monitors: Vec<AudioDeviceInfo>,
                             pub default_mic: Option<String>, pub default_monitor: Option<String>,
                             pub warnings: Vec<String> /* what couldn't be discovered; omitted when empty */ }
pub struct DoctorReport { pub checks: Vec<Check>, pub niri_snippet: String, pub va: Option<VaReport> }
pub struct Check { pub id: String, pub required: bool, pub ok: bool, pub detail: String, pub hint: Option<String> }
```

Screens from `wl_output` v4 `name`/`description` + geometry within 2 s
(fallback `niri msg --json outputs`); cameras via GStreamer's DeviceMonitor
(`Video/Source`, no capture; without GStreamer, or when it fails,
`/sys/class/video4linux`: names, no modes); audio via `pactl --format=json`
(mics are `alsa_input.*` only; monitors are `*.monitor`; other inputs such
as `bluez_input.*` are never listed or offered as mics but stay in the
audio facts, so a `--source` naming one keeps its description; a `(null)`
description falls back to the device's properties). One parser,
`sources::parse_pactl_audio`, gives every host its audio facts (the GUI's
`SharedSources` and the CLI's and headless hosts' `SystemSources` agree).
`doctor` checks
every SPEC §3.3 item (+ `pactl`, `gst.h264parse`, a ScreenCast portal ≥ v4
that captures monitors, and the optional `portal.registry`), prints a hint
per failure and the niri bind snippet (SPEC §10.2); exit 5 when a required
check fails; the GUI's "The swamp is missing something" screen renders the
same report.
Under `QREC_CAPTURE_BACKEND=fake` the portal, sound-server and real-source
element checks are optional and `videotestsrc`/`audiotestsrc` required.

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
  (`-f mov|mp4|webm|matroska`, or `-c:v png -f image2 -update 1` for PNG)
  and explicit encoders, never inferred from the output's name; every path
  is prefixed with `file:`. The runner may replace only that last element
  (with its temp path, or `/proc/self/fd/<n>` for an output's temp file,
  A§8.8).
- `probe_argv(file, kind)`: video `ffprobe -v error -select_streams v:0
  -count_packets -show_entries stream=nb_read_packets,r_frame_rate,width,height,pix_fmt,color_range,color_space,duration:stream_tags=DURATION:format=start_time,duration
  -of json`; audio selects `a:0` and adds `codec_name,sample_rate,channels`.
  Parsers live in `parse/ffprobe.rs` (pure). A duration is the stream's, the
  `DURATION` tag or the format's; without any (a crash) it comes from the
  counted packets: video frames at `r_frame_rate`, FLAC frames × 4608
  samples (A§2.2).
- `silence_argv(file, threshold_db, min_silence_ms)`: `ffmpeg … -i file:<f>
  -af silencedetect=noise=<t>dB:d=<s> -vn -f null -`; `parse/silencedetect.rs`
  turns stderr into intervals (a trailing silence without `silence_end` ends
  at the file duration).
- `parse/progress.rs` reads `-progress` blocks (`frame`, `out_time_us` — in
  ffmpeg 8.1 `out_time_ms` is also µs — `N/A` = 0, `progress=end`).
- The GUI's previews (docs/GUI.md G§3.11): `strip_argv`, `frames_argv` and
  `peaks_argv` write raw RGBA or mono s16le to `pipe:1` after `-f rawvideo
  -pix_fmt rgba` or `-f s16le`; `preview_probe_argv` reads a video's size
  and rate from its header (no `-count_packets`); `preview_width` is
  `scale=-2:<h>`'s width; `preview_key`, `bucket_peaks` and `peak_levels`
  are the cache key and the waveform's 1, 16 and 256 ms buckets.

### A§9.3 Export plan (`export_plan.rs`)

```rust
pub struct ExportPlan { pub out: String, pub replace: Option<FileId> /* A§5.9, A§8.8 */, pub tmp_ext: String, pub segments: Vec<SegmentPlan>,
                        pub stitch: StitchPlan, pub total_frames: u64, pub files: BTreeSet<String> }
pub struct SegmentPlan { pub clips: Vec<ClipId>, pub take: TakeId, pub in_: Frame, pub out: Frame,
                         pub frames: u64, pub samples: u64, pub key: String, pub path: String, pub argv: Vec<String> }
pub struct StitchPlan { pub list_path: String, pub list_content: String, pub argv: Vec<String> }
```

- **Render units:** maximal runs of *continuous* clips (same take,
  `clips[i].out == clips[i+1].in`) become one segment, so a split never
  changes the output and its cache key survives. Property:
  `plan_export(split(p)) == plan_export(p)` (and the same for GES).
- **Frame selection** (one rule for export, frame save and GES): take frame
  `f` shows recorded frame `shown_frame(f) = ⌈x − ½⌉`, `x = (f/C −
  startOffsetNs·10⁻⁹)·R`: the frame nearest to the canvas frame's time in
  the file, the earlier one on a tie, in exact rational arithmetic (so it
  never depends on where a unit starts). `first_showing(j) = ⌊C·((j − ½)/R
  + startOffset)⌋ + 1` is the first take frame showing `j` or later. At the
  canvas rate this is `f − off` with `off = round(startOffsetNs/frame)`.
- **Video inputs** *(measured; the old setpts/sub-frame plan was wrong in 30
  of 66 segments)*: per visible layer with media, for a unit `[in, out)`
  with `n` frames: `lead = max(0, first_showing(0) − in)`,
  `k0 = shown_frame(in + lead)`. Input: `-ss <frame_ns_rec(k0) − 2 ms>`
  (omitted when ≤ 0; the 2 ms absorbs MKV's ms timestamps) `-i file:<f>`.
  Chain: `[i:v]<head>,crop=<src.w>:<src.h>:<src.x>:<src.y>,scale=<dst.w>:<dst.h>:flags=bilinear,format=yuva420p[,lutyuv=a=val*<opacity>][li]`.
  At the canvas rate `<head>` = `fps=<num>/<den>,setpts=PTS-STARTPTS+<lead>`
  (frame `k0 + i` is unit frame `lead + i`). At another recorded rate that
  head re-based the stream at `k0` and let `fps` pick frames on a grid
  shifted by `k0`'s rounding: frame saves differed from the export, a
  trimmed start moved every frame of a clip by a canvas frame, and units
  showed frames that weren't the nearest. There `<head>` puts every recorded
  frame where the rule starts it:
  `settb=1/1000000,setpts='max(floor((a*round((T+<ss>)*<Rn>/<Rd>)+b)/q)+c,0)*<Cd>*1000000/<Cn>',settb=<Cd>/<Cn>,fps=<Cn>/<Cd>`
  — `j = round((T + ss)·R)` from the frame's time in the file (`ss` the
  input seek, 0 without one; Matroska stamps are within 1 ms), its start
  `⌊(a·j + b)/q⌋ + c = first_showing(j) − in`, then `fps` holds each frame
  until the next starts. With `C/R = p/q'` reduced and `Y = ⌊2q'·C·
  startOffset⌋`: `a = 2p`, `b = Y − p`, `q = 2q'`, `c = 1 − in` (whole
  numbers that ffmpeg's doubles evaluate exactly). The end of the stream
  maps to `first_showing(frames)`, so the background shows from there.
  `bilinear` matches GES's compositor best *(measured)*; `lutyuv` avoids an
  RGB round trip.
- **Compositing:** `color=c=<bg>:s=<W>x<H>:r=<num>/<den>[base]`, then
  `overlay=x=<dst.x>:y=<dst.y>:eof_action=pass` per layer in z-order, then
  `trim=end_frame=<n>,setsar=1,format=yuv420p,setparams=color_primaries=bt709:color_trc=bt709[v]`.
  Length is enforced in the graph — never `-frames:v` or `-t` (they
  truncate the audio at 29.97 fps) *(measured)*. Color: `-color_range tv -colorspace bt709 -color_primaries
  bt709 -color_trc bt709`; ffmpeg 8.1.2 negotiates the graph with range and
  matrix but takes primaries and transfer from the frames, hence the
  `setparams` (tagging range or matrix there too draws the background with
  BT.601 coefficients) *(measured)*.
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
- **Filter graphs:** the video chains (ending in `[v]`, or `[vv]` for VA)
  and the audio chains (ending in `[a]`) are two `-filter_complex` graphs,
  video first, then `-map [v] -map [a]`. In one graph, ffmpeg 8.1.2
  sometimes never finished a unit with several layers and tracks: once the
  video output ended the audio output stopped being fed, while `-progress`
  kept reporting (3 % of runs of a 3-layer, 2-track unit; 0 of 400 with
  two graphs, which decode to the same frames and samples) *(measured)*.
- **Segment codecs:** every segment's audio is `pcm_s16le`; H.264 segments
  are `.mov` (MKV timestamps make the stitched MP4 VFR-looking), VP9/AV1
  segments `.mkv`. The shell verifies each segment without decoding
  (`segment_check_argv`: `ffprobe -v error -show_entries
  packet=codec_type,size -of csv=p=0`; video packets == n, PCM bytes / 4
  == S) before renaming it into
  `.qrec/cache/segments/<key>.<ext>`; a mismatch fails the export. So the
  planner requires `caps.ffprobe` as well as ffmpeg and its encoders
  (`missing_dependency` before anything renders).
- **Presets** (Draft / Standard / High; Standard is SPEC §13.1):
  - libx264: `-preset veryfast -crf 23` / `medium -crf 20` / `slow -crf 18`
  - h264_vaapi: `-rc_mode CQP -qp 26` / `22` / `19`
  - libvpx-vp9: `-crf 36/32/28 -b:v 0 -row-mt 1 -deadline good -cpu-used 5/4/2`
  - libsvtav1: `-preset 10/8/6 -crf 38/35/30`
  - all H.264: `-bf 0 -profile:v high`, plus `-pix_fmt yuv420p` for
    libx264 only (h264_vaapi takes hwupload frames; ffmpeg 8.1.2 warns the
    option conflicts *(measured)*); keyframe at each segment start. No
    B-frames *(measured)*: with them a segment's DTS start a reorder delay
    early, except in a 1–2-frame segment (nothing to
    reorder), so after such a unit the next segment's first DTS collide in
    the stitch, ffmpeg bumps them by one tick each and the MP4's sample
    table comes out up to two frames short.
- **VA export chain** (only when `export_plan::export_vaapi(project,
  config, caps)` names the node: the effective encoder of A§2.1 isn't x264,
  the verified node is the effective device, and `caps.vaapi.ffmpeg_h264`;
  the GUI's export caption asks the same function, docs/GUI.md G§4.8):
  `-init_hw_device vaapi=va:<device> -filter_hw_device va … [v]format=nv12,hwupload[vv]
  -c:v h264_vaapi -rc_mode CQP -qp <q>`, child env `LIBVA_DRIVER_NAME` per
  A§8.10. One encoder per export: if a VA segment fails, the whole export is
  re-planned with libx264 (different argv, different keys, nothing mixes).
- **Cache key** = `sha256(PLAN_VERSION ‖ caps.ffmpeg.identity ‖ VA device for
  VA plans ‖ argv[..len-1] joined with \0)`; identical keys in one export are
  rendered once.
- **Stitch:** concat demuxer list in the segments dir (file names only),
  `-c:v copy`, audio encoded once (`aac -b:a 192k` + `-movflags +faststart`
  for MP4; `libopus -b:a 128k` for WebM), `-f mp4|webm`, output last. Each
  `file '<key>.<ext>'` line is followed by `duration <s>` = `us(end) −
  us(start)`, the unit's timeline boundaries in microseconds (rounded; the
  list's precision), so every segment starts within 0.5 µs of its timeline
  time *(measured)*. Without it the demuxer adds up the segments' container
  durations, which Matroska rounds to milliseconds: the copied VP9/AV1 video
  drifted from the once-encoded audio by up to 0.5 ms per unit (+49 ms after
  150 autocut fragments).

### A§9.4 Frame plan (`frame_plan.rs`)

The playhead's frame through the same graph as a one-frame unit (`n = 1`,
no audio), then `scale=out_color_matrix=bt709:out_range=pc,format=rgb24,setparams=color_primaries=bt709:color_trc=iec61966-2-1`
(an sRGB PNG: with the primaries ffmpeg writes `sRGB`/`cHRM`/`gAMA` chunks,
not a `cICP` with unspecified primaries *(measured)*)
`-frames:v 1 -c:v png -f image2 -update 1`, to
`frames/<stem>-<take>-<HHMMSSFF>.png`. `--layer L2`: the stream's raw frame at
native resolution (same frame selection, no crop or scale). Because the
frame selection doesn't depend on where a unit starts (A§9.3), the
one-frame unit at the playhead shows exactly the export's frame, at any
recorded rate *(measured: frame-ID fixtures at 25→30, 30→60, 50→30 and
29.97→25 fps)*.

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
pub struct GesVideoClip { pub take: TakeId, pub layer: LayerId, pub file: String, pub layer_priority: u32,
                          pub start_ns: u64, pub inpoint_ns: u64, pub duration_ns: u64, pub placement: Placement, pub alpha: f64 }
pub struct GesAudioClip { pub take: TakeId, pub track: TrackId, pub file: String, pub start_ns: u64, pub inpoint_ns: u64,
                          pub duration_ns: u64, pub volume: f64, pub fade_in_samples: u32, pub fade_out_samples: u32 }
```

`take` and `layer`/`track` say what a clip shows or plays, so the GUI's
player can leave out muted lanes (A§3 `laneMute`); the renderer only needs
the file.

Same render units, frame selection and lead as A§9.3:
`inpoint_ns = frame_ns_rec(k0)`, `start_ns = frame_ns(pos + lead)`,
`duration_ns = frame_ns(pos + n) − start_ns`. A layer recorded at another
rate instead gets `inpoint_ns = max(0, frame_ns(in + lead) − startOffsetNs +
⌊frame_ns_rec(1)/2⌋ − 3 ms)`: aligned with the take's time wherever the
unit starts (from `k0` it was off by `k0`'s rounding); GES shows the last
frame started, so half a recorded frame later makes that the nearest one;
−3 ms undoes the renderer's 2 ms Matroska nudge and keeps a stamp
truncated to the millisecond from winning a tie the export gives to the
earlier frame *(measured with GES: 25 fps camera on a 30 fps canvas)*.
Near a tie (within ~1 ms) GES may still differ by one frame. Video: a `videocrop` effect
from `placement.src` and always `posx/posy/width/height` from
`placement.dst`. Audio track caps `audio/x-raw,rate=48000,channels=2`;
fades are 240-sample linear ramps on the source's `volume` through a
`GstInterpolationControlSource` whose points span the clip, in-point to
end: a one-sided fade holds the volume to the other end (GES clamps a
control source to its element's edges by extending the nearest segment, so
a lone fade-out once reached a volume of thousands at the in-point).
`layer_priority` 0 is the take scene's top layer. A clip is cut at its
stream's known end (`frames` for video,
`durationNs` for audio: GES refuses `inpoint + duration` past the media)
and left out when nothing remains, where the export shows the background
and silence. An audio clip fades in or out only where it is playing at the
unit's cut (the export fades the unit's mix). The parity test (A§11)
checks GES against the frame plan.

For the GUI's player (docs/GUI.md G§3.10): `plan_ges_take(&Project,
&TakeId)` plays one take whole, `[0, durationFrames)` in its take scene, as
if it were the only clip (S10's trim scrub, D-17's bin preview; an unknown
or empty take plans nothing); `without_tracks(plan, solo, &muted)` drops
the audio of lanes muted for playback or not soloed (D-08; video stays, the
export never sees it); `scaled(plan, w, h)` composites at a smaller size
(H-9: destination rects scaled edge by edge to even pixels; source crops
stay in file pixels, GES crops before it scales); `fit_size(w, h, max)` is
a canvas fitted into a bound (even, never larger): the player's `out_size`
and the live preview's size. Properties: `without_tracks(p, None, ∅) == p`
and `scaled(p, canvas size) == p`.

**Program player** (`player.rs`, GUI H-2): one GES timeline and
`FULL_PREVIEW` pipeline per session on thread `qrec-player`, into the GUI's
Program sink; calls return at once, loads and seeks coalesce (newest wins).
- `load`: pause, `ges_render::rebuild_into` (layers cleared, restriction
  caps from the plan), commit and wait ≤ 10 s for "commited" (a stopped
  pipeline prerolls first: it applies a commit only as it starts; seeking
  before that met the old timeline), an accurate seek to `at_ns`, `Loaded`
  once that frame reached the sink, then play again if it was playing. An
  empty plan stops the pipeline (no picture). The layers taken off stay in
  a `ges_render::Retired` until that commit applied or the pipeline is
  READY or NULL (abandoned past 5 s: leaked): GES frees a removed clip's
  sources with it while their decoders may still be starting.
- Seeks are FLUSH|ACCURATE and frame-aligned, one in flight, done when the
  first frame after the flush reaches the sink. Forward rates are seeks
  with the rate; reverse is stepping (accurate seeks, ≤ 15 a second, video
  only), `Ended { reverse: true }` at 0. `scrub` plays 60 ms of sound (a
  plain seek while playing, or while a load waits: that load still says
  `Loaded`).
- Frames shorter than half a canvas frame are dropped at the sink: the 2 ms
  Matroska in-point nudge leaves slivers of background where a file runs
  out. QoS drops are counted (`Late`, ≤ once a second; H-8).
- An error is reported before the pipeline stops. Stopping (READY after an
  error or for an empty plan, NULL at `shutdown`) runs on a helper thread
  for at most 5 s: a pipeline wedged past that (a streaming thread stuck
  where flushing doesn't reach) is abandoned, leaked whole (the Program
  sink is inside it), and every later load reports `player::STUCK`. A bus
  error while a load or seek is in flight fails it at once; only a
  reported failure's follow-on errors are logged.
- `shutdown`: NULL within 5 s, then `Stopped` (at once when no pipeline is
  left). `AudioOut::Silent` under the fake backend and in tests.
- Measured (G§0.4): canvas size holds a 1080p30 canvas with two 1440p
  screens and a 720p camera (37.5–42.6 fps throughput; no late frames but
  in a run starved of CPU), so `out_size` stays `None`.

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
pub fn action_for(id: &str, state: &State) -> Option<Action>;   // Run(Command) | Gui | Unavailable(reason)
pub fn command_for(id: &str, state: &State) -> Option<Command>;  // action_for's Run; shared by the GUI and the palette
pub fn gate_reason(state: &State, cmd: &Command) -> Option<String>; // why apply's shared gates (phase, GUI host, disk) refuse cmd
pub fn usable_keys<'a>(map: &Keymap, b: &'a Binding) -> Vec<&'a KeyChord>; // its chords outside conflicts
impl KeyChord { pub fn cap_legends(&self) -> Vec<String>; }     // the GUI's key caps: ["Ctrl", "Shift", "Z"]
pub fn conflict_notes(map: &Keymap) -> Vec<ConflictNote>;       // Q5/S22's banner: { chord, key: "X", ids, labels }
pub fn set_binding(keys_toml: &str, id: &str, keys: &[KeyChord]) -> Result<String, CoreError>; // one line edited (WP9)
pub fn reset_binding(keys_toml: &str, id: &str) -> Result<String, CoreError>;               // its lines removed
pub const KEYLESS: &[KeylessAction];                            // the palette's actions with no key: history.clear (D-19)
```

Grammar `ctrl+shift+z`; shift is recorded only for letters and named keys
(`<` is `less`). The default table is SPEC §10.2 plus Backspace for remove,
the SPEC §12 canvas arrow nudges, and the window's `region.next` (`F6`),
`region.prev` (`Shift+F6`) and `app.quit` (`Ctrl+Q`, also over overlays),
which are `Gui` actions (GUI D-10). `Alt+[`/`Alt+]` trim the start/end
inward by one frame; with Shift they arrive as `alt+braceleft`/
`alt+braceright` and trim 10. A chord two bindings share in one context is
a `Conflict`: `lookup` runs neither. In Live mode `play.toggle`,
`shuttle.reverse` and `shuttle.forward` are `Unavailable` "Switch to
Program (Tab) to play the timeline" (GUI D-01; an empty timeline or a
refusing gate says so first; the `Play` command itself still switches,
A§5.10), and the layer nudges move a layer of `edit_target`'s scene, or are
`Unavailable` "Select a clip to edit its take layout" (GUI D-06, A§5.10).
From Idle `record.toggle` and `record.over` are `Unavailable` while a take
can't start: the capture engine is stuck ("The capture engine is stuck
(GPU driver?); restart Qrec"), the scene is empty, an export runs (the
GUI's Record button says the same); an invalid `qrec.json` isn't one, since
a take moves the bad file aside first (A§7.7).
`keys.toml`: `[bindings]` id → string or array. `qrec keys --json` prints
`[{"id","group","label","keys":[..],"cli","contexts"}]`. Settings ›
Keyboard edits it (WP9): `set_binding` / `reset_binding` change one line
of the text at `toml_edit`'s spans and leave every other byte (comments,
order, spacing), refuse a file that isn't TOML, and fall back to a
document edit for an inline `bindings` table; the shell writes the result
atomically (`keys_file::{set_binding, reset_binding}`). `history.clear`
is an id `action_for` knows without being a binding (no key, not in
`keys.toml`): the palette's "Clear undo history". An open window
reloads it when the file changes (A§8.9's settings watcher): the new keys
work and show at once, and each problem it didn't have is a toast (GUI
G§3.14).

### A§9.8 User config (`config.rs`)

```rust
#[serde(default, deny_unknown_fields)]   // snake_case keys
pub struct UserConfig { pub theme: Theme, pub canvas: CanvasDefaults, pub recording: RecordingDefaults,
                        pub autocut: AutocutDefaults, pub encoder: EncoderDefaults, pub ui: UiDefaults }
// canvas: width, height, fps ("30" or "30000/1001"), background
// recording: countdown_secs        autocut: auto_run_after_take, threshold_db, min_silence_ms, padding_ms, min_keep_ms
// encoder: preference, vaapi_device    ui: scale, reduced_motion, sounds
// theme: "swamp-night" | "swamp-day" | "dragons-keep" | "ogre-mode"
pub fn parse(text: &str) -> Result<UserConfig, CoreError>;        // missing keys default; unknown keys, wrong types, out of range ⇒ Invalid
pub fn problems(cfg: &UserConfig) -> Vec<String>;                 // what is wrong with the values (empty when valid)
pub fn get(cfg: &UserConfig, key: &str) -> Result<toml::Value, CoreError>;
pub fn get_json(cfg: &UserConfig, key: &str) -> Result<serde_json::Value, CoreError>; // null for an unset optional key
pub fn list(cfg: &UserConfig) -> Vec<ConfigEntry>;                // { key, value } for every setting
pub fn set(doc: &mut toml_edit::DocumentMut, key: &str, value: &str) -> Result<(), CoreError>; // validated by re-parsing
pub fn set_text(text: &str, key: &str, value: &str) -> Result<String, CoreError>; // `set` on the text: one line, the rest as it was (WP9)
```

`qrec config set` edits with `toml_edit`, so comments and order survive; the
shell only reads and atomically writes the file. Since WP9 the edit is
`set_text` (a `DocumentMut` printed a dotted table's keys together, `ui.scale`
next to `ui.sounds`): the value replaced at its span, or one line added or
removed; and the write goes through `Backend::write_settings`, which the
`qrec` binary implements with `qrec_shell::config_file::write_text`, the
Settings pages' writer (`config_file::set`): a temp file renamed over the
file, its permissions kept, a symlink written through. Hosts read it as
`Facts.config`: a file `parse` refuses gives all the defaults, and people
get a "config.toml is invalid" warning once per change of the file
(A§4.2), so a typo never drops the settings silently. An open window
applies `theme`, `ui.scale` and `ui.reduced_motion` as soon as the file
changes (A§8.9's settings watcher; GUI G§3.14).

### A§9.9 Naming (`naming.rs`)

`file_stem(name)`: replace `/`, NUL, control characters and `%` with `_`,
`.qrec-part` with `_qrec-part` (no export or frame name contains the temp
marker `.qrec-part-`, A§8.7, even when the cut stem is followed by `-`),
strip leading `.` and `-`, cap at 120 bytes on a char boundary, fall back to
`qrec`. Used for exports, frames and capture file names.

---

## A§10 CLI

- Global flags: `--project DIR`, `--json`, `--yes`, `-q`, `-v`. Numeric and
  time arguments allow hyphen values (`--by -1`, `--gain-db -3`), so a
  negative count, index or seq gets its range error (`undo -1`: "-1 is not
  in 1..=…"; `--to -1`, `--at -1`, `history restore -1`: "-1 is not in
  0..=…"), never "invalid digit found in string", which stays for what
  isn't a number. clap errors under `--json` print
  `{"ok":false,"error":{"code":"usage",…}}`, exit 2; `--help`, `help` and
  `--version` under `--json` print one `{"ok":true,…}` object (A§4.9).
- `--out` is resolved against the CLI's cwd before sending, keeping a
  trailing `/` (`dir/` or `dir/.` names a folder, which the host refuses,
  A§5.9). Without a readable cwd only an absolute `--out` or `--project`
  works (A§8.4).
- `qrec autocut` without `--clips`/`--all` sends `ClipScope::All`.
- `qrec status --json` returns `Query::State` plus `path`, `initialized`
  and the host that holds the project: `host` (`gui`, `headless` or `cli`)
  and `pid` of the live process that answered, or `null` and `null` when
  none runs and the CLI read the project itself (A§8.4; the human view
  says "none running"). `host` never names that short-lived reader (its
  read-only Session's `runtime.host` is `cli`, but it holds no lock, A§8.3).
- Project-less: `--help`, `version`, `doctor`, `sources list`, `schema`,
  `keys`, `agent-guide`, `config`, `completions <shell>`.
- `qrec` without a command opens the window on `--project DIR` (made
  absolute) or the cwd, as is (`Backend::gui`: the window applies its own
  folder rule, GUI D-14), and exits with its code; under `--json` it is
  `usage` "the GUI has no --json output; give a command" (exit 2). Until the
  binary runs `qrec-gui`, it logs to the project's GUI log (A§8.2), then
  answers `unavailable` "The Qrec GUI is not built yet…".
- Extensions to SPEC §14 (for GUI parity): `qrec project set`,
  `qrec autocut settings`, `qrec export cancel`, `qrec history clear`,
  `qrec completions`, `--codec`, `seek --to start|end`,
  `scene layer set --name/--locked`, `clip add --after-selection`, hidden
  `qrec host --headless`.
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
| Fake capture | `QREC_CAPTURE_BACKEND=fake` record start/stop end to end (M2); the GUI's parts (`capture/tests.rs`): the preview into an appsink standing in for the paintable sink (BGRx at the fitted size, one element through rebuilds), `transient_layout`, `retry_live`, the cast grace (a sharing fake portal's call count), `LiveStatus` for builds, refusals and deaths, a rebuilt preview keeping a cropped camera (both paths), `MeterMonitor` |
| Player | `player_tests.rs`: an appsink and the frame-ID fixture: accurate seeks show the frame plan's frame, `load` keeps the position, back-to-back loads free no running source (`Retired`), rates and reverse stepping, `Ended`, `Error`, `out_size`, shutdown ≤ 5 s |
| Parity | GES vs frame plan: SSIM ≥ 0.98 **per layer rect**, plus a frame-ID fixture (frame index drawn as binary bars) asserting the exact source frame in both renderers; at another recorded rate, export segments, frame saves and GES all show `shown_frame` (A§9.3) in units starting anywhere |
| Late errors | `tests/props_late_errors.rs`: an `on_event` outcome that replies `Err` to a requester also has a `Notify` (A§8.1), over random requests answered late, out of order or as failures, plus a table of every known path |
| GUI host | `host/gui_tests.rs`: `GuiHost` with a fake `GuiPort` and Session (`ipc::testing`) and shortened `ExitRules`: no idle exit, `first` ahead of early requests, one FIFO, `fail`, a failed open, close (after what the window submitted before it; dropping autocut analyses; the grace ending a recovery hold), every window request answered once (also unhandled ones, and at once while the Session drops), `first`'s toasts only while serving, signals (the second in a child process), the `shutdown` refusal, a crash; with a real Session and no capture (a closing window's autocut cuts nothing; a signal while it opens toasts nothing); and once with a real Session on the fake capture backend (the port as its observer and notifier: `first` initializes an empty folder, a CLI take records, the window's stop is answered after its state, close); `host/takeover_tests.rs` against hosts with a fake Session, and Stop & open on a real fake-backend recording and countdown (`crates/qrec/tests/cli.rs`) |
| Session for the GUI | `session/tests.rs`: the observer's snapshots (commands, `RecordClock`, export progress, before their replies), `reply_late` only for `Effect::Reply`, D-47 (UI commands answered while edits wait for a recovery), `RetryLive` in order with `SetLive`; `SharedSources` never discovering on the session thread once cached; `laneMute` through `.qrec/ui.json` |
| View models | `insta` per `view` module over `testkit::design_state()` and its owner's variants (docs/GUI.md G§8.1) |
| GUI | headless screenshots against the design fixture and the end-to-end smoke run (docs/GUI.md G§8.4–G§8.5; not built yet) |

**`qrec_core::testkit`** (feature `testkit`, also under `cfg(test)`; pure):
`ctx(now, entropy)`, `ProjectBuilder` (`.canvas()`, `.screen_layer()`,
`.mic()`, `.take(id, frames)`, `.clip(id, take, in, out)`, `.build()`),
`state(project)`, `drive(&State, &[Command], fake_shell) -> State`, proptest
strategies for projects and commands. `design_project()` and
`design_state()` are the sample every GUI design shows (design/BRIEF.md §3:
`rust-traits-talk`, takes 1–9 with their statuses and facts, the 10 clips
c7f3 … c3e9, 7,391 frames, playhead 2437; `DESIGN_*` constants);
`perf_project(clips)` is a long timeline.

**`qrec_shell::testkit`** (feature `testkit`): `TestEnv::new()` — a temp dir
that sets `HOME`, `QREC_RUNTIME_DIR`, `XDG_{CONFIG,STATE,CACHE,RUNTIME}_HOME`,
`QREC_CAPTURE_BACKEND=fake` and `TZ=UTC0` (local times read as UTC on any
machine, so the design fixture's UTC times show as BRIEF's) for every
spawned `qrec` (tests never touch the developer's config, tokens or
running GUI); `.qrec()` returns a `std::process::Command`. Other inherited
`QREC_*` variables are dropped, so a fake hook (below) applies only where a
test sets it, and so are `DISPLAY`, `WAYLAND_DISPLAY` and `WAYLAND_SOCKET`
(`qrec` without a command opens the window, never on the developer's
display). Dropping a `TestEnv` SIGKILLs every process still running with
its `QREC_RUNTIME_DIR` (what it spawned, the hosts a `qrec record start`
detached, their children), also after a failed test, then deletes its
dirs. `MediaSpec { video:
Option<(w, h, fps, frames)>, audio: Option<{ sample_rate, channels,
speech_ms: [(start, end)] }> }` and
`fixture(&MediaSpec) -> PathBuf`, generated with ffmpeg (`testsrc2`, `sine`
gated by speech intervals) and cached in `target/qrec-fixtures/<hash>.<ext>`
via tmp + rename. `fixture_project(&dir, takes)` writes a project whose takes
point at fixture media. `design::design_fixture(dir, DesignVariant)` writes
the design sample with fixture media on the fake backend's sources
(`FAKE-1`, `/dev/fake0`, `alsa_input.fake`, `alsa_output.fake.monitor`;
screens 640×360, the camera 320×240; A1's speech placed so `qrec autocut`
finds BRIEF §3.3's and §3.6's silences) and no history, as `Base`,
`Take10Landed` (with take 10's placement as its one history entry, the
cursor on it), `MissingL2` or `Smoke`, always into an empty folder (the
take files are hard links to the shared cache, made only as new files, so
nothing writes through them; an empty cached file is generated again);
`perf_fixture(dir, clips)`,
`playback_fixture(dir)` and `write_ui(dir, &UiState)` (a case's
`.qrec/ui.json`) go with it.

**Fake hooks** (`capture::fake::FakeHooks`, docs/GUI.md G§8.4): honored only
with the fake backend; a value that doesn't parse is ignored with a
warning. Only code that runs as the process reads them from the
environment: the binary's hosts (`FakeHooks::from_env` into
`EngineOptions::fake`), `SessionEnv::from_process` (the inventory kind its
`FakeSources` use), discovery (`SourceInventory::discover`) and the GUI's
opener. `EngineOptions::new`, `SessionEnv::for_dirs`, `FakeSources` and
`SourceInventory::fake` take none, so an engine or Session a test builds in
its own process sees a hook only when the test sets it, whatever the
developer exported for manual shots.
- `QREC_FAKE_EOS_DELAY_MS=<ms>`: a stopped take's `CaptureStopped` comes
  that much later; past its EOS timeout the take ends `timed_out` (broken
  with `EOS_TIMEOUT_REASON`, "the recording didn't stop within 10 s").
- `QREC_FAKE_PORTAL=deny|cancel`: the fake screens go through a fake portal
  that refuses them (denied, or the dialog closed), answered like the real
  one's refusals.
- `QREC_FAKE_RECORD_OFFSET_FRAMES=<n>`: every `RecordClock` reads n frames
  more, as if the take had started earlier.
- `QREC_FAKE_INVENTORY=design`: discovery reports BRIEF's labels and sizes
  ("GIGA-BYTE GS27QA" 2560×1440, "Dell U2720Q" 3840×2160, "Logitech C920",
  "Yeti Nano", "Built-in Audio Analog Stereo"); names, paths, modes and
  the test patterns stay.

---

## A§12 GUI

See `docs/GUI.md`, the GUI's plan and contract; it replaces this section.

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
   (A§8.5). They never record (a recording asked meanwhile waits for them,
   ≤ 20 s, then gets its own headless host; while one of them exports, it
   is refused at once instead, as in every host, since recording is
   blocked during an export), print their result as soon as they have it,
   and take no new job once answered.
7. Export quality presets for Draft and High (A§9.3).
8. UI-state commands (`Command::ui_only`) are never held for a startup
   recovery, so they can overtake edits that wait for it; those resolve
   the selection and the playhead when they run (A§5.3, GUI D-47).
9. `Take.recovered` joined schema v1 without a `version` bump: dev builds
   from before it reject a project that has it; the first schema change
   after the first release bumps `version` with a migration (A§2,
   docs/GUI.md G§9 R24).
