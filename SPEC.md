# Qrec — Specification

> **Qrec** (_Quick Recording_, rhymes with Shrek). A Wayland screen and camera
> recorder built for one loop: **press R, talk, press R, repeat**. When you stop
> recording, the video is already mostly edited.
>
> _Ogres have layers. So do your recordings._

|                |                                                                                                                                |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| Status         | Draft v1, written 2026-10-14 from an interview with the product owner                                                          |
| Target         | Linux, Wayland (developed on NixOS + niri)                                                                                     |
| App ID         | `com.rsensato.qrec` (GApplication id, `.desktop` file, icon name, D-Bus name)                                                  |
| Related docs   | `docs/RECORDING.md` covers wf-recorder lessons from `qrec-tui`                                                                 |
| Prior attempts | `qrec-tui` (Rust, FCIS, solid), `qrec-electron` (fragile browser playback), `qrec-sky` (never compiled), `qrec-glm` (scaffold) |

---

## 1. Goals and non-goals

### 1.1 Goals

1. **Quick recording.** Recording starts with one key and no dialogs. Each
   stop adds a _take_ to the timeline. You can discard the last take or record
   over a selected one with one key.
2. **Mostly edited at the end.** Trimming, cutting, reordering and **autocut
   (silence removal)** are built in. The finished timeline exports straight to
   a video.
3. **Layers.** Combine any number of monitors and cameras. Each one is a layer
   you can move and resize on a preview canvas, like OBS. Mic and system
   audio are recorded as **separate tracks**.
4. **Non-destructive by default.** Media files are never deleted or
   overwritten unless you confirm it. Every project change can be undone.
5. **CLI parity.** The GUI and the CLI are one binary and share one command
   dispatcher. Anything you can do in the UI, an LLM can do from the command
   line, and it runs the same code.
6. **Smooth and good-looking.** Native, GPU-rendered, frame-accurate. The
   design is fully swamp-themed.

### 1.2 Non-goals for v1

- X11. The design keeps it possible later: GStreamer `ximagesrc` would be a
  new source kind, not a rewrite.
- Project management (recent projects, project browser). You launch Qrec in
  the right folder.
- Multi-track editing, B-roll, transitions, titles, effects, color grading.
- Capturing a single window, and drawing a screen region. A layer's `crop`
  can still trim a monitor numerically (§12), but there is no "draw a region"
  gesture.
- Windows and macOS. GNOME and KDE should work through the portal but aren't
  tested.

---

## 2. Glossary

| Term            | Meaning                                                                                                                                                 |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Project**     | A folder containing `qrec.json` and its media.                                                                                                          |
| **Source**      | A capture device: a monitor (screen), a camera, a mic, or system audio.                                                                                 |
| **Layer**       | A video source placed on the canvas with a rect, z-order, opacity, crop and visibility. In the UI these are called "Onion layers".                      |
| **Audio track** | An audio source (`mic` or `system`) with gain, mute and sync offset. Each one is recorded to its own file.                                              |
| **Scene**       | The full set of layers and audio tracks. The **project scene** is used for new takes. Each take keeps its own **snapshot** of the scene.                |
| **Take**        | One recording session (start → stop). It is a folder with one file per source, all on one shared clock. Takes are immutable media.                      |
| **Clip**        | A timeline item that references a take with an in/out range `[in, out)` in frames. One take can back several clips, for example after a cut or autocut. |
| **Timeline**    | An ordered, gapless list of clips (one magnetic track).                                                                                                 |
| **Bin**         | Takes that no clip references ("unused takes"). They're shown in a drawer and can be put back on the timeline.                                          |
| **Canvas**      | The output frame size and fps. Default **1920×1080 @ 30 fps**.                                                                                          |
| **Host**        | The single process that owns a project while it is open: the GUI, or a headless recorder.                                                               |

---

## 3. Stack

### 3.1 Decision

| Concern                           | Choice                                                                                    | Why                                                                                                                                                                                                                                                                                                                                                          |
| --------------------------------- | ----------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Language                          | **Rust** (stable, edition 2024)                                                           | Fits FCIS (pure core, rich types, property tests). `qrec-tui` already proved the approach. One static binary with both GUI and CLI.                                                                                                                                                                                                                          |
| GUI toolkit                       | **GTK4 + libadwaita** (`gtk4-rs`, `libadwaita-rs`). The theme is fully custom CSS on top. | Native Wayland and GPU-rendered (GSK/Vulkan/GL). Smooth video through `gtk4paintablesink`. Custom widgets (timeline, canvas) draw with `GtkSnapshot`.                                                                                                                                                                                                        |
| Live capture and preview playback | **GStreamer ≥ 1.24** (`gstreamer-rs`, GES via `gstreamer-editing-services`)               | One pipeline clock for every source, so audio and video stay in sync with no manual delay (this was qrec-tui's biggest workaround). It also provides live compositing for the canvas, `pipewiresrc` for portal screencasts, `v4l2src` for cameras, `pulsesrc` for PipeWire audio, and VA-API encoding. GES gives frame-accurate seeks for timeline playback. |
| Portal / D-Bus                    | **`ashpd`**                                                                               | xdg-desktop-portal ScreenCast with restore tokens (`persist_mode = 2`), so the source picker appears once per monitor.                                                                                                                                                                                                                                       |
| Offline media work                | **ffmpeg / ffprobe** (CLI, spawned)                                                       | Probing, `silencedetect`, waveform peaks, thumbnails, **export** and **frame save**. We don't reinvent any of this. Every ffmpeg command line is built as pure data in the core.                                                                                                                                                                             |
| CLI                               | **clap** (derive)                                                                         | Subcommands, `--json`, completions.                                                                                                                                                                                                                                                                                                                          |
| Config                            | TOML (`~/.config/qrec/config.toml`, `keys.toml`)                                          | Meant for hand editing. Project state stays JSON (§6.1).                                                                                                                                                                                                                                                                                                     |
| Packaging                         | **Nix flake** (dev shell + package). Add a Flatpak later if wanted.                       | All dependencies are pinned. GStreamer isn't installed system-wide on the dev machine, so the flake provides it.                                                                                                                                                                                                                                             |

### 3.2 Alternatives rejected

- **Electron / Tauri + web UI.** `qrec-electron` spent most of its history
  fighting `<video>` and playhead desync, black frames, and portal and
  permission quirks. Tauri's WebKitGTK video is weaker still. Frame-accurate
  multi-layer playback in a browser is the wrong battle.
- **Qt6/QML with Rust (cxx-qt).** It renders well, but the Rust↔C++ bridge
  adds a lot of friction, and composited multi-source capture on Wayland is
  less proven there than in GStreamer.
- **wf-recorder as the capture engine.** It captures only one monitor, can't
  show live frames inside the app, records variable fps, and its audio and
  video drift apart. It stays as a documented **fallback** for capturing a
  single monitor (see `docs/RECORDING.md`).

### 3.3 Runtime dependencies (checked by `qrec doctor`)

- GStreamer elements: `pipewiresrc`, `v4l2src`, `pulsesrc`, `compositor`,
  `videorate`, `x264enc`, `vah264enc` (optional), `matroskamux`, `flacenc`,
  `gtk4paintablesink` (gst-plugins-rs), GES.
- `ffmpeg` and `ffprobe` with libx264, libvpx-vp9, libopus and libsvtav1
  (optional).
- xdg-desktop-portal with a ScreenCast backend. On niri that is
  `xdg-desktop-portal-gnome`, which is present on the dev machine.
- PipeWire with `pipewire-pulse`.
- Optional: `wl-copy` (copy a saved frame to the clipboard).

When something is missing, `doctor` lists it and prints a hint for each item.
Startup runs the same check and shows a blocking "The swamp is missing
something" screen with that list.

---

## 4. Architecture (FCIS)

### 4.1 Layout

```
crates/
  qrec-core/    # FUNCTIONAL CORE: no I/O, no clocks, no randomness (ids/timestamps are injected)
  qrec-shell/   # IMPERATIVE SHELL: fs, locking, history store, IPC, ffmpeg, GStreamer engine, portal
  qrec-cli/     # clap args -> core::Command; output formatting (human / --json)
  qrec-gui/     # GTK views; user input -> core::Command; render from core::State
  qrec/         # the binary: wires everything; `qrec` with no args = GUI
```

### 4.2 Data flow

```
          ┌──────────── GUI input ───────────┐
          │                                  ▼
CLI args ─┼──► Command ──► Session::dispatch ──► core::apply(state, cmd)
IPC msg ──┘                       ▲                   │ returns (new_state, Vec<Effect>)
                                  │                   ▼
                       core::on_event(state, ev) ◄── shell runs effects
                                  ▲                   (write project, append history,
                                  └──── Event ◄────── start capture, run ffmpeg, …)
```

- **`core::apply(&State, Command) -> Result<(State, Vec<Effect>), CoreError>`**
  is pure. Commands are serializable enums such as `ClipSplit { clip, at }`,
  `RecordStart { mode }` and `Export { preset, out }`.
- **`core::on_event(&State, Event) -> (State, Vec<Effect>)`** handles results
  coming back from the shell, such as `TakeFinalized`, `SilenceDetected`,
  `ExportProgress`, `CaptureFailed` and `ProjectChangedOnDisk`.
- **Effects** are plain data, for example `WriteProject(json)`,
  `AppendHistory(entry)`, `StartCapture(CapturePlan)`,
  `RunFfmpeg(FfmpegJob)`, `ProbeFile(path)` and `Notify(toast)`. The shell
  runs them and turns the results into events.
- **Pure planners inside the core:**
  - The timeline-to-GES description used for preview.
  - The capture plan, describing GStreamer pipeline elements as data.
  - The export plan, made of ffmpeg argv for each segment plus the concat
    command.
  - The autocut plan.
  - The frame-save plan.
  - Keymap resolution.
  - Undo diffs.

  All of them get snapshot tests.

- **`Session::dispatch` is the only entry point**, and the GUI, the CLI and
  IPC all call it. This satisfies requirement 21: the CLI always exercises the
  same code paths as the UI.

### 4.3 Hosts, locking and IPC (CLI ↔ GUI)

- One **host** process owns a project at a time. It holds an exclusive `flock`
  on `.qrec/lock` and listens on a Unix socket at
  `$XDG_RUNTIME_DIR/qrec/<sha256(canonical project path)>.sock`.
- **CLI behavior.** When a host is alive, the CLI sends its `Command` over the
  socket as JSON lines and streams back the result and progress events.
  Otherwise the CLI takes the lock, runs the command in-process through the
  same `Session`, writes the result and exits.
- **The GUI** is always a host.
- **Headless recorder.** If you run `qrec record start` with no host, Qrec
  spawns a detached `qrec host --headless` with `setsid`. That process runs
  the same capture engine without the preview branch, and it exits once the
  recording stops and is finalized.
- **GUI start while a headless recording runs.** The GUI shows a modal: "A
  take is being recorded from the CLI." The choices are **Stop & open** and
  **Quit**.
- **External manual edits** to `qrec.json` while a host is running are
  detected with inotify plus a content hash. They are reloaded and recorded in
  history as an `External edit` entry. If the file no longer validates, the
  host keeps the last good state and shows an error toast pointing at the
  validation message.

### 4.4 Folder rule (requirements 15–17)

When the GUI or any project-scoped CLI command starts, the working directory,
or the directory given with `--project <dir>`, must be one of:

1. **A directory containing `qrec.json`.** That project is used.
2. **A strictly empty directory.** Any entry counts, including hidden files
   and folders such as `.git` or `.envrc`. A new project is initialized there
   (`qrec.json`, `takes/`, `.qrec/`). The CLI does this lazily on the first
   mutating command, or explicitly with `qrec init`.

Anything else is rejected with the message _"Not my swamp: this folder isn't
empty and has no qrec.json"_ and exit code 3. Commands that don't use a
project still work anywhere: `--help`, `version`, `doctor`, `sources list`,
`schema`, `keys`, `agent-guide`.

---

## 5. Recording

### 5.1 Sources

| Kind             | Capture                                                               | Notes                                                                                                                                                                                       |
| ---------------- | --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Screen (monitor) | Portal ScreenCast → `pipewiresrc`                                     | The picker is shown once per monitor. Restore tokens are saved in `~/.local/state/qrec/portal-tokens.json`, keyed by output name, so later sessions (including headless ones) don't prompt. |
| Camera           | `v4l2src` (or `pipewiresrc` through the camera portal when available) | Caps are negotiated at the closest mode ≥ canvas fps.                                                                                                                                       |
| Mic              | `pulsesrc device=<alsa_input…>`                                       | Only real inputs (`alsa_input.*`) are offered as mics. Fixes a qrec-tui data bug where a `.monitor` source got picked as the mic.                                                           |
| System audio     | `pulsesrc device=<sink>.monitor`                                      | The default sink monitor unless one is chosen.                                                                                                                                              |

A scene may contain **any number** of screen and camera layers and **zero or
more** audio tracks. The `mic` and `system` roles are labels. Multiple mics
are allowed.

### 5.2 Live pipeline

While the GUI is in **Live** canvas mode, or a host is recording, one
GStreamer pipeline runs:

```
per video source:  src → videoconvert → tee ─┬─ preview: videoscale → compositor(sink_N with layer rect/zorder/alpha) → gtk4paintablesink
                                             └─ record bin (attached only while recording)
per audio source:  src → audioconvert → level → tee ─┬─ meter (UI)
                                                     └─ record bin
```

- The pipeline keeps running between takes, so **recording starts with no
  delay**. Pressing R only attaches record bins.
- **Record bin for video:** `queue → videorate (CFR = canvas fps) →
videoconvert → encoder → h264parse → matroskamux → filesink`.
  - Encoder: `vah264enc` on the configured render node, falling back to
    `x264enc speed-preset=veryfast tune=zerolatency`.
  - Keyframe every 1 s, so preview seeks and exports stay fast.
  - Quality is roughly CRF 18.
  - Video is recorded at the source's **native resolution**. Scaling to the
    canvas happens at preview and export time, so layouts can change later.
- **Record bin for audio:** `queue → audioresample (48 kHz) → flacenc →
matroskamux → filesink`. That gives lossless audio per track.
- **Container: Matroska** for everything. A truncated MKV, for example after a
  crash, is still playable, while an MP4 without its `moov` atom is not
  (`RECORDING.md` §5).
- **Sync:** all bins attach at the same pipeline running time `T0`. For each
  file, Qrec stores `startOffsetNs` = (running time of its first buffer) −
  `T0`. Preview and export apply these offsets. Each audio track also has a
  manual `syncOffsetMs` to compensate for device latency (cameras are often
  50–150 ms late).

### 5.3 Take lifecycle

```
Idle ──r──► [Countdown n s] ──► Recording ──r──► Finalizing ──► Idle (take added)
              │ Esc                 │ Esc                 └─fail─► Idle (take marked broken, toast + log)
              ▼                     ▼
             Idle                 Cancelled (files kept; take goes to bin, no clip)
```

1. **Start.**
   - The core allocates `take-NNNN`, a monotonic number that is never reused,
     and writes the take with `status: "recording"` **before** capture
     begins.
   - Files are written to `takes/take-NNNN/`, with one file per layer and
     per audio track plus `recorder.log`.
   - The scene is snapshotted into the take.
2. **Health check.** If a record bin errors within 500 ms, for example
   because the device is busy or a portal is denied, the take is aborted at
   once. The UI shows the error. It doesn't sit in a fake "Recording…" state
   (`RECORDING.md` pitfall 4).
3. **While recording:**
   - The canvas shows a pulsing REC badge and a timer driven by the pipeline
     clock.
   - Each audio track shows a live level meter.
   - The timeline shows a growing ghost clip.
   - Editing commands are disabled. Allowed: stop (`r`), cancel (`Esc`), and
     changing the zoom or view.
4. **Stop.**
   - Qrec sends EOS to the record bins only and waits for EOS on every
     filesink, with a 10 s timeout.
   - On a timeout the take is marked `broken` and stays on disk for
     inspection.
   - Durations come from `ffprobe` (the source of truth), and `durationFrames`
     = shortest video stream.
   - Takes shorter than **10 frames** become `tooShort`. They go to the bin
     with no clip, and a toast reports it.
5. **Place the take:**
   - **Normal record:** a clip `[0, durationFrames)` is **appended** to the
     end of the timeline.
   - **Record over (`o`):** the selected clip, or a contiguous run of selected
     clips, is **replaced in place** by the new full-take clip. The replaced
     takes stay in the bin. Over-recording is disabled when the selection
     isn't contiguous.
6. **Autocut.** If `autocut.autoRunAfterTake` is on, autocut runs on the new
   clip as a **separate history entry**, so you can undo just the autocut.
7. **Recovery.** On startup, a take still marked `recording` (for example
   after a crash) is probed. If its files are readable, it is finalized and
   added to the bin with a toast: _"Recovered take 7 from the bog"_.
   Otherwise it is marked `broken`.

### 5.4 Discard (`d`)

`d` removes the clip(s) created by the **most recent take** from the
timeline, regardless of what is selected. The take moves to the bin. Files
are kept, and the action can be undone. Pressing `d` again doesn't remove the
take before that one. It only acts on the newest take. (Use `Delete` on a
selection for anything else.)

### 5.5 Countdown

`recording.countdownSecs` can be 0–10 and defaults to **0**. During a
countdown the canvas shows a big number, and `Esc` or `r` aborts it.

---

## 6. Project file and on-disk layout

### 6.1 Why JSON (and not SQLite)

The question was raised and settled in favor of **JSON**:

- State is small, at most a few hundred takes and clips. SQLite's query and
  storage strengths don't help at that size.
- JSON can be read and diffed by humans, git and LLMs. An agent can `cat` it
  to understand a project. `qrec schema` publishes a JSON Schema for it.
- The things SQLite would add (atomicity, concurrency, history) are covered
  in other ways:
  - **Atomic writes:** write `qrec.json.tmp`, `fsync` it, `rename` it over
    `qrec.json`, then `fsync` the directory.
  - **Concurrency:** the single-host lock plus IPC (§4.3).
  - **History:** an append-only JSONL file (§8).
- Binary or derived data (waveform peaks, thumbnails, silence analysis) lives
  as plain files in `.qrec/cache/` and can be deleted at any time.

### 6.2 Folder layout

```
my-video/
├── qrec.json                 # the project (source of truth)
├── takes/
│   └── take-0001/
│       ├── L1-screen-DP-4.mkv
│       ├── L2-camera-0.mkv
│       ├── A1-mic.mka
│       ├── A2-system.mka
│       └── recorder.log
├── exports/                  # never overwritten (timestamped names)
├── frames/                   # saved stills (PNG)
└── .qrec/
    ├── lock                  # flock held by the host
    ├── history.jsonl         # undo/redo log
    ├── history.cursor        # current position in the log
    ├── ui.json               # non-undoable view state (zoom, scroll, selection, panel sizes)
    └── cache/                # waveforms, thumbnails, silencedetect results, export segments
```

### 6.3 `qrec.json` (schema v1)

Times are **integer frames at canvas fps**. Clips use half-open ranges
`[in, out)`. Ids are short, stable strings. UI-only state (selection,
playhead, zoom) is **not** part of the project. It lives in `.qrec/ui.json`
and doesn't take part in undo.

```jsonc
{
  "version": 1,
  "name": "my-video", // defaults to folder name
  "createdAt": "2026-10-14T12:00:00Z",
  "canvas": {
    "width": 1920,
    "height": 1080,
    "fps": { "num": 30, "den": 1 },
    "background": "#000000",
  },

  "scene": {
    // project scene: used for NEW takes
    "layers": [
      {
        "id": "L1",
        "name": "Main screen",
        "visible": true,
        "opacity": 1.0,
        "source": {
          "kind": "screen",
          "output": "DP-4",
          "label": "GIGA-BYTE GS27QA",
        },
        "rect": { "x": 0, "y": 0, "w": 1920, "h": 1080 }, // canvas px
        "crop": { "top": 0, "right": 0, "bottom": 0, "left": 0 }, // source px
        "fit": "contain",
      },
      {
        "id": "L2",
        "name": "Face",
        "visible": true,
        "opacity": 1.0,
        "source": {
          "kind": "camera",
          "device": "/dev/video0",
          "label": "Logitech C920",
        },
        "rect": { "x": 1500, "y": 760, "w": 384, "h": 288 },
        "crop": { "top": 0, "right": 0, "bottom": 0, "left": 0 },
        "fit": "cover",
      },
    ], // array order = z-order (last on top)
    "audio": [
      {
        "id": "A1",
        "role": "mic",
        "source": "alsa_input.usb-Blue_Microphones_Yeti_Nano…",
        "label": "Yeti Nano",
        "gainDb": 0.0,
        "muted": false,
        "syncOffsetMs": 0,
      },
      {
        "id": "A2",
        "role": "system",
        "source": "alsa_output.…analog-stereo.monitor",
        "label": "System audio",
        "gainDb": -6.0,
        "muted": false,
        "syncOffsetMs": 0,
      },
    ],
  },

  "recording": { "countdownSecs": 0, "encoder": "auto", "vaapiDevice": null },

  "autocut": {
    "autoRunAfterTake": false,
    "track": "A1", // audio track id used for silence detection
    "thresholdDb": -35.0,
    "minSilenceMs": 400, // only silences at least this long are removed
    "paddingMs": 120, // kept on each side of speech
    "minKeepMs": 200, // drop leftover fragments shorter than this
  },

  "takes": {
    // MEDIA POOL: append-only, not affected by undo (§8.2)
    "take-0001": {
      "status": "ok", // recording | ok | broken | tooShort
      "recordedAt": "2026-10-14T12:03:10Z",
      "durationFrames": 912,
      "scene": {
        /* snapshot of project scene at record time, editable per take */
      },
      "streams": [
        {
          "id": "L1",
          "kind": "video",
          "file": "takes/take-0001/L1-screen-DP-4.mkv",
          "width": 2560,
          "height": 1440,
          "startOffsetNs": 0,
        },
        {
          "id": "L2",
          "kind": "video",
          "file": "takes/take-0001/L2-camera-0.mkv",
          "width": 1280,
          "height": 720,
          "startOffsetNs": 41000000,
        },
        {
          "id": "A1",
          "kind": "audio",
          "file": "takes/take-0001/A1-mic.mka",
          "sampleRate": 48000,
          "channels": 1,
          "startOffsetNs": 3000000,
        },
        {
          "id": "A2",
          "kind": "audio",
          "file": "takes/take-0001/A2-system.mka",
          "sampleRate": 48000,
          "channels": 2,
          "startOffsetNs": 2500000,
        },
      ],
    },
  },
  "nextTakeNumber": 2,

  "timeline": {
    "clips": [
      { "id": "c7f3", "take": "take-0001", "in": 0, "out": 410 },
      { "id": "c8a1", "take": "take-0001", "in": 455, "out": 912 },
    ],
  },
}
```

**Invariants** (enforced by the core and covered by property tests):

- `0 ≤ in < out ≤ take.durationFrames`.
- Every clip references an existing take whose status is `ok`.
- Every layer and track id in a take scene exists in that take's `streams`,
  or the layer is shown as "missing media".
- Clip ids are unique.
- The timeline has no gaps: the position of clip _i_ is the sum of the
  durations before it.

**Versioning:** `version` is bumped on schema changes, and `qrec-core` ships
pure migration functions `vN → vN+1`. Before migrating, Qrec writes a backup
copy to `.qrec/qrec.v{N}.json.bak`.

### 6.4 User config (`~/.config/qrec/`)

- `config.toml`:
  - theme
  - default canvas
  - default autocut parameters
  - encoder preference and VA-API device (the Intel node `renderD129` on the
    dev machine; never let the encoder auto-pick, see `RECORDING.md`)
  - UI scale
  - reduced motion
- `keys.toml`: keybinding overrides (§10).
- `~/.local/state/qrec/`: portal restore tokens and the logs of the last
  session.

---

## 7. Timeline editing

### 7.1 Model

There is a single magnetic track. Clips sit back to back, so removing one
closes the gap and moving one shifts its neighbors. All operations are pure
functions on `timeline.clips`.

| Operation            | Behavior                                                                                                                                                                                                                                                                      |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Select**           | Click, Shift+click (range), Ctrl+click (toggle), `,`/`.` (previous/next clip), Ctrl+A (all). Selection is UI state.                                                                                                                                                           |
| **Move**             | Drag and drop, with an insertion caret between clips. `<`/`>` move the selection one slot left or right. A multi-selection moves as a contiguous block, in its original order.                                                                                                |
| **Trim start / end** | Drag a clip edge, or press `[` / `]` to trim the clip under the playhead (or the selected clip) to the playhead. You can extend an edge back out up to the take's bounds, so nothing is ever lost. While dragging, the canvas shows the frame at the edge (scrub-while-trim). |
| **Cut (split)**      | `s` splits the clip under the playhead at the playhead frame into `[in, p)` and `[p, out)`, both pointing at the same take. Rendered output is identical before and after (a property test checks this).                                                                      |
| **Remove**           | `Delete`/`Backspace`/`x` removes the selected clips from the timeline. Takes and files stay. A take no longer referenced by any clip shows up in the bin.                                                                                                                     |
| **Insert from bin**  | Drag from the bin, or use _Insert at end_ / _Insert after selection_.                                                                                                                                                                                                         |
| **Autocut**          | `c` runs on the selected clips, or on all clips when nothing is selected (§9).                                                                                                                                                                                                |
| **Edit take layout** | With a clip selected in Program mode, layer changes on the canvas edit **that take's** scene snapshot. Actions: _Apply this layout to all takes_, _Use this layout for new takes_, _Reset take to project scene_.                                                             |

Undo groups:

- A drag-move or drag-trim is **one** history entry, committed on mouse
  release.
- During a drag the core works on a transient state that isn't saved.

### 7.2 Timeline view

- **Length is proportional to duration:** clip width = `frames ×
pxPerFrame`, with no minimum width distortion. A clip that is too narrow
  for its label still shows a 1 px edge marker.
- **Zoom** goes from fitting the whole project down to **frame level**, where
  frames are at least 24 px wide and you see per-frame ticks, frame numbers
  and individual thumbnails.
  - `=`/`-` zoom in and out.
  - Ctrl+wheel zooms around the cursor.
  - `\` fits the whole project.
  - `z` zooms to the selection.
  - Zoom eases smoothly at 60 fps.
- **Rows:**
  - A ruler with timecode `HH:MM:SS:FF`, with adaptive tick density.
  - The video row: a filmstrip of thumbnails from the topmost visible layer,
    the take label, and a take color stripe. Clips from the same take share
    that color.
  - One waveform lane per audio track, linked to the clips, each with mute
    and solo.
  - Silence regions detected by autocut appear hatched in the lane of the
    track autocut uses.
- **Navigation:**
  - Wheel scrolls horizontally, and middle-drag pans.
  - Clicking the ruler seeks, and dragging it scrubs (with audio scrub).
  - The playhead follows playback and auto-pages.
- **Snapping** (`n` toggles it) snaps to the playhead and clip edges.
- **Hover** shows a tooltip with the clip's take, in/out, duration and the
  relevant shortcuts.

### 7.3 Bin (unused takes)

The bin is a drawer (`b`) listing takes that no clip references, plus
`broken` and `tooShort` takes. Each row shows a thumbnail, number, duration,
recording time and status, with actions:

- Preview.
- Insert at end.
- Insert after selection.
- Reveal in file manager.
- **Purge from disk.** This is destructive and needs confirmation (§11). It is
  the only way Qrec deletes recorded media.

---

## 8. Undo / redo

### 8.1 Mechanics

- Each mutating command appends an entry to `.qrec/history.jsonl`. An entry
  contains:
  - `seq`
  - `ts`
  - `label` (for example _"Split clip c7f3 at 00:00:13:20"_)
  - `cmd` (the serialized command)
  - `forward` and `inverse` as RFC 6902 JSON Patches against `qrec.json`
  - `afterHash`
- `history.cursor` stores the current `seq`.
- **Undo** applies `inverse` and moves the cursor back. **Redo** applies
  `forward`. A new command after an undo drops the redo tail. History is
  linear.
- History persists across sessions and is shared by the GUI and the CLI
  (`qrec undo`, `qrec redo`, `qrec history`).
- If the on-disk hash doesn't match the last `afterHash` (because of a manual
  edit), Qrec first inserts an `External edit` entry so that undo stays
  correct.
- Compaction: when the log grows past 5,000 entries, the oldest are folded
  into a snapshot (`history.base.json`).

### 8.2 What is undoable

- **Undoable:** the timeline, the project and take scenes, autocut settings,
  canvas, recording settings, and the project name.
- **Not undoable:**
  - The `takes` media pool and `nextTakeNumber`, which are append-only facts
    about the disk. Undoing a recording removes **its clip**, and the take
    shows up in the bin. Redo puts the clip back.
  - UI state.
  - Purge, which is destructive and confirmed. It also removes history
    entries that reference the purged take.

---

## 9. Autocut (silence removal)

1. **Analysis (shell).**
   - Runs `ffmpeg -i <track file> -af
silencedetect=noise=<thresholdDb>dB:d=<minSilence> -vn -f null -` on
     the audio file chosen by `autocut.track` (requirement 23: you pick which
     track drives detection).
   - Results are cached in `.qrec/cache/silence/` keyed by file hash and
     parameters.
   - The analysis runs in the background. The UI shows a spinner on the
     affected clips.
2. **Plan (core, pure):**
   - For each clip `[in, out)`, intersect it with the silence intervals
     shifted by the stream's `startOffsetNs` and `syncOffsetMs`.
   - Shrink each silence by `paddingMs` on both sides.
   - Keep the complement.
   - Drop kept fragments shorter than `minKeepMs`.
   - Snap boundaries to frames (rounding outward, so speech is never clipped).
3. **Apply.** The clip is replaced by N clips from the same take as **one
   history entry**, for example _"Autocut take 3: removed 4 silences
   (6.2 s)"_.
   - Removed audio and video can always be brought back by extending a clip
     edge, or with undo.
4. **Preview first (optional).**
   - Shift+C opens autocut settings with a live preview of what would be cut:
     hatched regions with sliders.
   - The CLI equivalent is `qrec autocut --dry-run`, which prints the planned
     segments.
5. **Clicks at cuts.** Preview and export apply a 5 ms audio fade at every
   clip boundary that isn't a natural take start or end, to avoid clicks at
   autocut and split points.

---

## 10. Keyboard shortcuts

### 10.1 Principles

- **Single keys** for the hot loop, with no modifiers needed while recording
  and editing quickly.
- **Every timeline operation has a shortcut.** Every shortcut is a command
  and every command has a CLI equivalent.
- Everything can be rebound in `~/.config/qrec/keys.toml`. Conflicts are
  reported on load.
- **Discoverability, on four layers:**
  1. **Hint bar.** The bottom status bar shows context-aware hints, for
     example `r record · o record over · s split · c autocut · ? all keys`.
     The hints change with state (idle, recording, clip selected, dragging).
  2. **`?` cheat sheet.** A full-screen overlay grouped by category. It is
     searchable and shows the current (rebound) keys.
  3. **Command palette.** `Ctrl+K` or `:` opens a fuzzy palette listing every
     command with its key and its CLI form, for example
     `Split clip — s — qrec clip split`.
  4. **Tooltips and menus** always show the shortcut.

### 10.2 Default keymap

| Group        | Key                                       | Command                                                                                            | CLI                                     |
| ------------ | ----------------------------------------- | -------------------------------------------------------------------------------------------------- | --------------------------------------- |
| **Record**   | `r`                                       | Toggle record (start / stop)                                                                       | `qrec record toggle`                    |
|              | `o`                                       | Record over the selection                                                                          | `qrec record start --over <clip>…`      |
|              | `d`                                       | Discard the last take (to the bin)                                                                 | `qrec take discard-last`                |
|              | `Esc`                                     | Cancel the countdown or recording (take goes to the bin), clear the selection, or close an overlay | `qrec record cancel`                    |
| **Playback** | `Space`                                   | Play / pause                                                                                       | `qrec play` (GUI host only)             |
|              | `j` / `k` / `l`                           | Shuttle reverse / stop / forward (press again for 2×, 4×)                                          | —                                       |
|              | `←` / `→`                                 | Previous / next frame                                                                              | `qrec seek --by -1f`                    |
|              | `Shift+←` / `Shift+→`                     | Back / forward 1 s                                                                                 | `qrec seek --by -1s`                    |
|              | `↑` / `↓`                                 | Previous / next edit point                                                                         | `qrec seek --edit prev`                 |
|              | `Home` / `End`                            | Timeline start / end                                                                               | `qrec seek --to start`                  |
| **Edit**     | `,` / `.`                                 | Select previous / next clip                                                                        | — (UI state)                            |
|              | `<` / `>`                                 | Move the selection left / right one slot                                                           | `qrec clip move <id> --by -1`           |
|              | `s`                                       | Split at the playhead                                                                              | `qrec clip split <id> --at <t>`         |
|              | `[` / `]`                                 | Trim start / end to the playhead                                                                   | `qrec clip trim <id> --start/--end <t>` |
|              | `Alt+[` / `Alt+]`                         | Nudge the start / end edge by ±1 frame (`Shift` = 10)                                              | `qrec clip trim <id> --start +1f`       |
|              | `Delete` / `x`                            | Remove the selected clips (to the bin)                                                             | `qrec clip remove <id>…`                |
|              | `c`                                       | Autocut the selection, or everything                                                               | `qrec autocut [--clips …]`              |
|              | `Shift+C`                                 | Autocut settings + preview                                                                         | `qrec autocut --dry-run`                |
|              | `Ctrl+Z` / `Ctrl+Shift+Z` (also `Ctrl+Y`) | Undo / redo                                                                                        | `qrec undo` / `qrec redo`               |
|              | `Ctrl+A`                                  | Select all                                                                                         | —                                       |
| **Output**   | `p`                                       | Save the frame at the playhead as PNG                                                              | `qrec frame save --at <t>`              |
|              | `Ctrl+E`                                  | Export…                                                                                            | `qrec export`                           |
| **View**     | `=` / `-`                                 | Zoom the timeline in / out                                                                         | —                                       |
|              | `\`                                       | Zoom to fit                                                                                        | —                                       |
|              | `z`                                       | Zoom to the selection                                                                              | —                                       |
|              | `n`                                       | Toggle snapping                                                                                    | —                                       |
|              | `Tab`                                     | Switch the canvas between Live and Program                                                         | —                                       |
|              | `b`                                       | Toggle the bin                                                                                     | `qrec take list --unused`               |
|              | `?`                                       | Cheat sheet                                                                                        | `qrec keys`                             |
|              | `Ctrl+K` or `:`                           | Command palette                                                                                    | —                                       |
|              | `Ctrl+,`                                  | Settings                                                                                           | `qrec config`                           |

**Global binding** (requirement 25: record while another app has focus). The
docs and `qrec doctor` print a ready-to-paste niri snippet:

```kdl
binds {
    Mod+Shift+R { spawn "qrec" "record" "toggle"; }
    Mod+Shift+O { spawn "qrec" "record" "start" "--over-selection"; }
    Mod+Shift+D { spawn "qrec" "take" "discard-last"; }
}
```

`--over-selection` resolves the selection held by the host. `qrec record
toggle` needs the project folder. Compositor binds run in `$HOME`, so the CLI
looks for the target project in this order:

1. `--project <dir>`.
2. The current directory.
3. **The single running Qrec host.** It is found through
   `$XDG_RUNTIME_DIR/qrec/*.sock`. If several hosts are running, the
   command fails with a list of them.

---

## 11. Non-destructive rules and confirmations

- Recorded media is **never** modified. Exports and frames get new,
  timestamped filenames.
- Every command that deletes or overwrites a file needs confirmation:

  | Action                                    | What gets lost                                                    |
  | ----------------------------------------- | ----------------------------------------------------------------- |
  | Purge take(s) from disk                   | The take's media files                                            |
  | Export to an existing path chosen by hand | The existing file                                                 |
  | Clear history                             | Undo/redo history                                                 |
  | Change canvas fps on a project with takes | Clip bounds are re-quantized (all clips are listed in the dialog) |

- **GUI:** the confirmation is a modal titled _"Get out of my swamp?"_. It
  states exactly what will be lost (files, sizes, counts). Focus starts on
  **Cancel**, and the destructive button is torch-red and needs a deliberate
  click or Enter on it.
- **CLI:** without `--yes` the command doesn't run. It exits with code 4 and
  the message `confirmation required: <description>`, so an LLM has to
  choose to pass `--yes`.

---

## 12. Preview canvas

There are two modes, switched with `Tab`. A badge in the corner of the canvas
shows which one is active.

| Mode        | Shows                                                                                   | Layer edits affect                             |
| ----------- | --------------------------------------------------------------------------------------- | ---------------------------------------------- |
| **Live**    | Live sources composited with the **project scene**. Used to set up and while recording. | The project scene, used by new takes           |
| **Program** | Timeline playback (GES) at the playhead                                                 | The scene snapshot of the selected clip's take |

**Layer editing on the canvas:**

- Click to select a layer. Drag to move it.
- Corner handles resize it, keeping the aspect ratio by default (`Shift`
  frees it).
- `Alt`-drag on an edge crops it.
- Layers snap to canvas edges, the center and thirds, with guides.
- Arrow keys nudge the selected layer when the canvas has focus; `Shift` moves
  it 10 px.

**Onion layers panel:**

- Layer list with z-order (drag to reorder), visibility, lock and rename.
- _Add layer_: screen or camera, from the detected sources.
- Audio tracks with a meter, gain, mute and sync offset.
- An inspector with numeric fields for rect, crop, opacity and fit.

**Playback and quality:**

- Seeking is frame-accurate.
- Playback must hold canvas fps with 2 screen layers + 1 camera layer on the
  dev machine.
- When playback can't keep up, it may drop to a lower-resolution proxy, which
  is shown with a "proxy" badge.

**Preview/export parity:** a test renders frames N₁…N₅ through GES (appsink)
and through the ffmpeg export plan, and requires SSIM ≥ 0.98 for each frame.

---

## 13. Export and frame save

### 13.1 Export

- **Formats in v1:**
  - **MP4:** H.264 + AAC 192 kbps. `h264_vaapi` when available, otherwise
    `libx264 -crf 20 -preset medium`.
  - **WebM:** VP9 + Opus 128 kbps (`libvpx-vp9 -crf 32 -b:v 0 -row-mt 1`),
    with AV1 (`libsvtav1`) as an option.
- **Quality presets:** Draft, Standard (the default) and High.
- **Output:** the canvas size and fps.
- **Pipeline** (planned purely in the core and run by the shell):
  1. **Segments.** Each clip is rendered to `.qrec/cache/segments/<hash>.mkv`
     with the target video codec and PCM audio.
     - Input-side `-ss`/`-t` with re-encoding gives frame-accurate trims.
     - The filter graph scales and overlays each visible layer onto a canvas
       `color` source in z-order, applies opacity and crop, mixes the audio
       tracks (`volume`, `amix`), applies the `startOffsetNs`/`syncOffsetMs`
       shifts and the 5 ms boundary fades.
     - Segments render in parallel (at most cores/2 at once).
     - The cache key is a hash of the clip, its take scene, the canvas and
       the preset. **Re-exporting after a small edit only re-renders the
       clips that changed.**
  2. **Stitch.** The concat demuxer joins the segments with `-c:v copy` and
     encodes the audio **once** (AAC or Opus), so there are no clicks from
     encoder priming at segment boundaries.
  3. The result is written to `exports/<name>-YYYYMMDD-HHMMSS.<ext>`.
- **Progress:**
  - ffmpeg runs with `-progress pipe:1`.
  - Total progress = frames done across all segments ÷ total frames, plus a
    weighted stitch phase.
  - The UI shows an export panel with:
    - a **progress bar** drawn as a swamp filling with mud, with bubbles (a
      plain bar when reduced motion is on)
    - a percentage
    - the phase, for example "Rendering clip 4/12" or "Stitching"
    - elapsed time and ETA
    - **Cancel**
  - When the export finishes: **Open file**, **Open folder**, **Copy path**.
  - When it fails: the last 30 lines of ffmpeg stderr and a link to the
    full log.
- **Snapshot:** export copies the project state when it starts, so editing
  can continue while it runs. Recording is blocked during an export.
- **CLI:** `qrec export [--format mp4|webm] [--quality draft|standard|high]
[--out PATH]`.
  - It prints a human progress bar by default.
  - With `--json` it prints JSON-lines events instead:
    `{"event":"progress","phase":"render","done":1234,"total":5678,"etaSecs":42}`.
  - When the GUI is the host, the export runs inside the GUI (and shows the
    same panel) while the CLI streams the events.

### 13.2 Save frame (requirement 26)

- `p`, or the camera-icon button under the canvas, saves the **composited**
  frame at the playhead.
- It is rendered through the same ffmpeg plan as export (one frame), so it
  matches the export exactly. The output is a canvas-size PNG at
  `frames/<name>-<take>-<HHMMSSFF>.png`.
- A toast shows the path, with _Copy image_ (`wl-copy`) and _Open folder_.
- Option: `--layer L2` saves the raw source frame of one layer at native
  resolution.

---

## 14. CLI reference (v1)

Global flags:

- `--project DIR` (default: the current directory)
- `--json` (machine-readable output)
- `--yes` (confirm destructive actions)
- `-q` / `-v`

Time arguments accept:

- frames: `412f`
- seconds: `13.7s`
- timecode: `00:00:13:21` or `00:00:13.700`
- relative values: `+1f`, `-2s`

Timeline positions are clip-relative unless `--timeline` is given.

```
qrec                         # open GUI in cwd (folder rule §4.4)
qrec init [--canvas 1920x1080@30]
qrec status                  # summary; --json = full state + host/recording status
qrec doctor                  # dependency + portal + device check, niri bind snippet
qrec sources list            # screens, cameras, mics, system-audio monitors

qrec scene layer add screen --output DP-4 [--rect X,Y,W,H] [--name ..]
qrec scene layer add camera --device /dev/video0 [--rect ..]
qrec scene layer set <id> [--rect ..] [--crop T,R,B,L] [--opacity 0.8] [--visible true|false] [--fit contain|cover]
qrec scene layer order <id> --to <index>
qrec scene layer remove <id>
qrec scene audio add mic|system [--source NAME]
qrec scene audio set <id> [--gain-db -3] [--mute true|false] [--sync-offset-ms 80]
qrec scene audio remove <id>
qrec scene apply --take <take> | --to-all-takes | --from-take <take>
        # (scene commands accept --take <take> to edit a take's snapshot instead of the project scene)

qrec record start [--over <clip>… | --over-selection] [--countdown N]
qrec record stop | toggle | cancel
qrec take list [--unused] | show <take>
qrec take discard-last
qrec take purge <take>… --yes          # destructive

qrec clip list
qrec clip add <take> [--at <index> | --after <clip>]
qrec clip move <clip> (--to <index> | --by ±N | --before <clip> | --after <clip>)
qrec clip trim <clip> [--start T] [--end T]
qrec clip split <clip> --at T          # or: qrec clip split --timeline --at T
qrec clip remove <clip>…

qrec autocut [--clips <clip>… | --all] [--track A1] [--threshold-db -35] [--min-silence-ms 400]
             [--padding-ms 120] [--min-keep-ms 200] [--dry-run]
qrec undo [N] | redo [N] | history [--limit 20]

qrec export [--format mp4|webm] [--quality draft|standard|high] [--out PATH]
qrec frame save --at T [--layer <id>] [--out PATH]

qrec seek --to T | --by ±T | --edit prev|next      # GUI host only (moves playhead)
qrec play | pause                                  # GUI host only

qrec config get|set <key> [value]
qrec keys [--json]                    # effective keymap
qrec schema                           # JSON Schema of qrec.json
qrec agent-guide                      # concise usage guide for LLM agents
```

**Output contract:**

- With `--json`, every command prints exactly one JSON object
  `{"ok":true,"data":…}` or
  `{"ok":false,"error":{"code":"…","message":"…","hint":"…"}}`.
- Streaming commands (export, record with `--wait`) print JSON lines instead.
- Every mutating command returns the new history `seq`, plus the ids it
  created (for example the new clip ids after a split).

**Exit codes:**

| Code | Meaning                                 |
| ---- | --------------------------------------- |
| 0    | OK                                      |
| 1    | Error                                   |
| 2    | Usage error                             |
| 3    | Not a project folder / folder not empty |
| 4    | Confirmation required                   |
| 5    | Missing dependency                      |
| 6    | Busy (recording or exporting)           |
| 7    | Not found (bad id)                      |
| 130  | Interrupted (Ctrl+C or SIGTERM)         |

---

## 15. Visual design and theme

### 15.1 Process gates (must happen in this order)

1. **Install the impeccable skills** (<https://impeccable.style/>) before any
   UI work. They aren't installed yet. The pen.dev (Pencil) MCP server is
   already connected.
2. **Design in pen.dev.** Every screen and state below is designed in
   `design/qrec.pen` through the Pencil MCP and reviewed by the product owner
   **before** any GTK UI code is written.
3. Implement GTK views from the approved designs. Design tokens come out of
   Pencil into `crates/qrec-gui/resources/theme/*.css` as CSS variables, so
   the code and the design share one source.

**Screens and states to design:**

- First run / empty swamp
- Main window:
  - Live mode, idle
  - Countdown
  - Recording
  - Finalizing
  - Program mode with a selection
- Timeline at 3 zoom levels (fit, seconds, frame-level)
- Trimming, with the edge-frame preview
- Drag-move with the insertion caret
- Autocut preview
- Layer editing on the canvas, and the Onion layers panel
- Bin drawer
- Export panel: running, done and failed
- Destructive confirmation modal
- Command palette
- Cheat sheet
- Settings, including keybinding editing with conflict display
- Missing-dependency screen
- Portal-denied error
- Toasts

### 15.2 Themes

The default is **Swamp Night**. Every theme defines the same token names.

| Token      | Swamp Night (default)     | Swamp Day               | Dragon's Keep          | Ogre Mode                |
| ---------- | ------------------------- | ----------------------- | ---------------------- | ------------------------ |
| `bg`       | `#12170F` bog at midnight | `#F1EEDA` parchment     | `#151316` castle stone | `#1E2A0E` deep ogre      |
| `surface`  | `#1C2416` wet moss        | `#E6E3C6` dried reed    | `#201D22` dungeon      | `#2E4214` ogre shade     |
| `raised`   | `#27321F` log             | `#FAF8EA` onion skin    | `#2C2830` tower        | `#3F5A1B` ogre skin dark |
| `border`   | `#3A4A2C` reeds           | `#C9C29A` straw         | `#433C47` chain        | `#5E7F2A` ear trumpet    |
| `text`     | `#E8E6C8` parchment       | `#2A2A18` mud ink       | `#EDE6E8` bone         | `#F4EDD0` linen shirt    |
| `muted`    | `#9BA582` lichen          | `#6E6A4B` bark          | `#A49AA5` ash          | `#C2B98F` burlap         |
| `accent`   | `#8FC43A` ogre skin       | `#4F7A19` fern          | `#E0457B` dragon scale | `#B5D334` bright ogre    |
| `accent-2` | `#E3B23C` onion gold      | `#B5831A` honey         | `#F28C28` lava         | `#E7C86A` onion          |
| `record`   | `#E5483B` torch           | `#C8352A` fire          | `#FF3B30` fire breath  | `#E8432F` red            |
| `warning`  | `#D98A2B` swamp gas       | `#B8661A`               | `#F28C28`              | `#D98A2B`                |
| `wood`     | `#6B4A2B` outhouse door   | `#7A5532` cottage beams | `#5A4636` drawbridge   | `#7A4E2A` leather vest   |

- **Take colors** for clip stripes form a separate 8-color categorical set
  derived from each theme (mossy green, onion gold, mud, dragon pink, donkey
  grey, fairy blue, gingerbread brown, lily-pad teal).
- **Contrast:** every text/background pair must meet WCAG AA, and the
  `record` color must stay distinct from `accent` for color-blind users. The
  REC state also uses a shape (●) and a pulse, not color alone.

### 15.3 Character

**Typography:**

- A storybook serif for display moments only: the logo, empty states and
  modal titles. _Fraunces_ or similar OFL font.
- A highly legible sans for the UI: _Atkinson Hyperlegible_ or _Inter_.
- A monospace with tabular numbers for timecodes: _JetBrains Mono_.
- All fonts are bundled.

**Copy:** swamp flavor goes in secondary text. Primary labels stay plain
verbs ("Record", "Export"), so the app stays usable.

| Situation      | Text                                                            |
| -------------- | --------------------------------------------------------------- |
| Empty state    | _"This swamp is quiet. Press **R** and make some noise."_       |
| Delete modal   | _"Get out of my swamp?"_                                        |
| Export done    | _"Layers! Like an onion. Your video is ready."_                 |
| Too-short take | _"That was shorter than Lord Farquaad. Take moved to the bin."_ |
| Recovered take | _"Recovered take 7 from the bog."_                              |

- **Icon:** an onion with ogre-ear trumpets, also used as the REC badge
  motif.
- **Motion:** subtle and purposeful.
  - The REC badge pulses like a torch flicker.
  - The export bar bubbles.
  - Zoom eases.
  - Everything respects reduced motion.
- **Sound (optional, off by default):** a short "squelch" on record start and
  stop.

---

## 16. Error handling and robustness

**Capture:**

- Every failure (portal denied, device busy, encoder failure, VA-API hang) is
  reported quickly and to the specific source. A 500 ms startup health check,
  bus error watching and a 10 s EOS timeout ensure that the UI never freezes
  (`RECORDING.md` pitfalls 2–4).
- If a source disappears mid-take (camera unplugged), the take finalizes with
  the remaining streams. The missing layer is marked in the take scene.
- Never let VA-API pick its device on its own. Use the configured node, and
  on the dev machine Intel `renderD129`, because NVIDIA `renderD128` hangs.

**Files and history:**

- Every child process logs to a file, and no unread pipes are left open:
  - `takes/<take>/recorder.log` for capture.
  - `.qrec/logs/` for ffmpeg jobs.
- Project writes are atomic (§6.1).
- A corrupted `qrec.json` is never overwritten. The host refuses to start and
  points at the last history snapshot for recovery
  (`qrec history restore <seq> --yes`).

---

## 17. Testing strategy

| Layer           | How                                                                                                                                                                                                                                                    |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Core            | Unit tests plus `proptest` properties: split → render identity, trim bounds, move permutations, undo∘apply = id, autocut never cuts inside padded speech, migrations round-trip.                                                                       |
| Planners        | Snapshot tests (`insta`) of ffmpeg argv, the GES description and the capture plan.                                                                                                                                                                     |
| CLI integration | Temp project folders. Fixture media is generated by ffmpeg (`testsrc2`, plus `sine` with known silence gaps). Covers record (fake capture), autocut, export and frame save end to end, with assertions on durations, frame hashes and silence removal. |
| Fake capture    | With `QREC_CAPTURE_BACKEND=fake`, the capture engine uses `videotestsrc`/`audiotestsrc`. The whole recording loop runs headless in CI and in LLM sessions.                                                                                             |
| Parity          | SSIM check between the GES preview and the ffmpeg export (§12).                                                                                                                                                                                        |
| GUI             | A thin smoke test (launch, dispatch a scripted command list over IPC, screenshot). Behavior is covered by the tests above because the GUI only sends Commands.                                                                                         |
| Manual          | A checklist on niri: two monitors + camera + mic + system audio, a 10-minute take, the A/V sync clap test, a portal restore after reboot.                                                                                                              |

---

## 18. Milestones

| #      | Milestone                      | Exit criteria                                                                                                                                                                                                                                                                                                                                                |
| ------ | ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **M0** | **Spikes** (de-risk)           | (a) portal + `pipewiresrc` on niri, with the restore token reused headlessly. (b) 2 screens + camera + 2 audio tracks in one pipeline, with measured A/V offset < 1 frame and CFR output. (c) GES playback of a 2-layer, 10-clip timeline with frame-accurate seeks and smooth `gtk4paintablesink`. (d) ffmpeg segment export matching GES with SSIM ≥ 0.98. |
| M1     | Core + CLI, no capture         | Project model, folder rule, all timeline ops, undo/redo, autocut, export, frame save, `--json`. All of it is tested with fixture media.                                                                                                                                                                                                                      |
| M2     | Capture engine + headless host | `qrec record start/stop` (real + fake), IPC, locking, recovery, `doctor`.                                                                                                                                                                                                                                                                                    |
| M3     | Design (parallel with M1–M2)   | Impeccable installed. All screens from §15.1 in `design/qrec.pen`, approved.                                                                                                                                                                                                                                                                                 |
| M4     | GUI                            | Live/Program canvas, layer editing, timeline with zoom down to frames, drag, trim and split, bin, export panel, confirmations.                                                                                                                                                                                                                               |
| M5     | Polish                         | Keymap config + cheat sheet + palette, themes, motion, toasts, niri bind docs, Nix packaging.                                                                                                                                                                                                                                                                |

---

## 19. Requirement traceability

| #           | Requirement                                              | Section         |
| ----------- | -------------------------------------------------------- | --------------- |
| 1           | Wayland first, X11 only if simple                        | §1.2, §3        |
| 2           | Best stack, nice and smooth                              | §3              |
| 3           | Name Qrec                                                | header          |
| 4           | Shrek theme and color schemes                            | §15             |
| 5           | Impeccable before UI                                     | §15.1           |
| 6           | Pen.dev design before UI                                 | §15.1           |
| 7           | CLI operation                                            | §4.3, §14       |
| 8           | JSON project file is the state                           | §6              |
| 9           | Non-destructive, confirm destructive actions             | §11             |
| 10          | Undo / redo                                              | §8              |
| 11–12       | Recording takes onto the timeline                        | §5              |
| 13          | Preview of the timeline                                  | §12             |
| 14          | Select, move, trim, cut, delete                          | §7.1            |
| 15–17       | Folder rule, no project management                       | §4.4            |
| 18–19       | Export with clear progress                               | §13.1           |
| 20          | FCIS                                                     | §4              |
| 21          | CLI and UI share functions                               | §4.2            |
| 22          | NLE timeline with zoom to frames and proportional widths | §7.2            |
| 23          | Autocut silences (choose the track)                      | §9              |
| 24          | Rely on ffmpeg                                           | §3.1, §9, §13   |
| 25          | Quick-record loop (r / d / o)                            | §5.3, §5.4, §10 |
| 26          | Save frame as image                                      | §13.2           |
| 27          | Keyboard shortcuts, discoverable                         | §10             |
| (interview) | Layers, multiple screens and cameras, OBS-like canvas    | §5.1, §12       |
| (interview) | Separate mic and system tracks, autocut track choice     | §5.1, §9        |
| (interview) | Compositor bind + in-app shortcuts                       | §10.2           |
| (interview) | Headless CLI recording                                   | §4.3            |

---

## 20. Open questions and risks

1. **Portal restore tokens on niri.** It hasn't been verified that
   `xdg-desktop-portal-gnome` on niri honors `persist_mode = 2` and restores
   without showing a dialog. Spike M0(a) checks this. **Fallback:** a custom
   `wlr-screencopy` source (niri supports it; that's what wf-recorder uses),
   implemented as an `appsrc` in the shell.
2. **Load from several native-resolution encoders.** Two 1440p screens plus a
   camera mean three encoders running at once. VA-API on Intel should cope,
   but this needs measuring. **Mitigation:** an option to record screens
   already scaled to the canvas.
3. **GES vs ffmpeg parity** in scaling filters and color range. wf-recorder's
   output was full-range `yuvj420p`. All recording and export is normalized
   to limited-range BT.709, and the parity test from §12 guards it.

### Resolved

- **App ID:** `com.rsensato.qrec`.
- **Draw screen region:** not in v1. Numeric `crop` on a screen layer stays.
- **Folder rule:** strictly empty. Hidden files and folders count as content.
