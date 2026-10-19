# Qrec

**Qrec** (_Quick Recording_, rhymes with Shrek) is a Wayland screen and camera
recorder built for one loop: **press R, talk, press R, repeat**. Every take
lands on a magnetic timeline, so when you stop recording the video is
already mostly edited: trim, split, reorder, autocut the silences, export.

_Ogres have layers. So do your recordings._

- `SPEC.md` — the product spec (what Qrec does and why).
- `docs/ARCHITECTURE.md` — the engineering contract (types, protocols, rules).
- `docs/GUI.md` — the window's plan: its architecture, work packages and tests.
- `docs/RECORDING.md` — capture lessons from the earlier `qrec-tui`.
- `design/README.md` — the pen.dev designs and the decisions waiting for review.

## Status

| Milestone (SPEC §18) | State |
| --- | --- |
| M1 Core + CLI | Done: project model, folder rule, every timeline operation, undo/redo with persistent history, autocut, export (MP4/WebM, VP9/AV1), frame save, `--json` |
| M2 Capture engine + hosts | Done: GStreamer engine (screens through the portal, cameras, mics, system audio), fake backend, headless host, IPC, locking, crash recovery, `doctor` |
| M0 Spikes | Export ↔ GES parity verified (per-layer SSIM ≥ 0.986, exact frames). Portal restore tokens, screen/system-audio sync and a 10-minute take verified on real hardware; the camera clap test needs a person (below) |
| M3 Design | Swamp Night vertical slice in `design/qrec.pen`, **approved** 2026-10-15 (contrast decisions in DESIGN.md) |
| M4 GUI | Done: the approved slice (S01–S18b) in GTK4/libadwaita: header, stage and canvas, timeline, onion layers panel, bin, export, toasts, modals and Settings › Project, with the adaptive layouts and the end-to-end smoke; the product owner left the review's calls to engineering (docs/GUI.md G§0). A pause shows the frame it parks at, and a CLI or agent seek lands while the window plays. The drafted screens came in M5 (below) |
| M5 Polish | Done: every drafted screen as drawn (2026-10-17 export): the blocking pages (S23, X1, X2, X3, and the undrawn ones in their shapes but two rare error pages, a folder Qrec can't read and a host that failed, which keep the interim layout), the command palette and the cheat sheet, Settings › Appearance, Keyboard, Recording and Autocut (edits keep `config.toml`'s and `keys.toml`'s comments; a conflicting key runs neither binding), a refused screen on the stage (S24) and S25's toasts in the frame's words, each triggered for real; every icon is the frames' Lucide icon and the art and app icon the pen's. Themes as tokens, proved on the main screens and holding AA contrast; `qrec config set theme`, `ui.scale`, `ui.reduced_motion` and `keys.toml` apply to an open window; screen readers hear the record loop, the toasts and the export's end, and the keyboard reaches every control in BRIEF's order; every screen is checked at 1.5× and 2× and with reduced motion; `nix build` installs a desktop entry and the app icon; the window's main loop holds 50 ms on GL. Each screen is shot next to its frame, the departures listed (docs/GUI.md G§7.2) |

## Build and run

Everything comes from the Nix flake (GStreamer, GTK and Rust aren't
installed system-wide):

```sh
nix develop            # or: direnv allow
cargo build            # target/debug/qrec
nix build              # the packaged, wrapped binary in ./result/bin/qrec
nix run . -- --help
```

## The window

`qrec` with no command opens the window on the current folder (`--project
DIR` names another; an empty folder becomes a project named after it).
While the window is open, `qrec …` from a terminal or a compositor bind
talks to it instead of starting a host of its own, and both change the
project through the same commands.

```sh
nix run . -- --project ~/videos/my-video    # the packaged build
cargo run -- --project ~/videos/my-video    # the dev build (inside nix develop)
QREC_CAPTURE_BACKEND=fake cargo run -- --project tmp/try   # test patterns, no devices
```

The package also installs `share/applications/com.rsensato.qrec.desktop`
and hicolor icons, so `nix profile install .` puts Qrec in the app
launcher (the app icon is a placeholder until the design's is exported).
A launcher starts Qrec in `$HOME`, which isn't a project: the window says
"Not my swamp". Start it from a project folder, or with `--project`.

## Quick start (CLI)

```sh
mkdir my-video && cd my-video        # an empty folder becomes a project
qrec init
qrec sources list                    # screens, cameras, mics, system-audio monitors
qrec scene layer add screen --output DP-4
qrec scene layer add camera --device /dev/video0
qrec scene audio add mic
qrec scene audio add system

qrec record start                    # the first screen layer shows the portal picker once
qrec record stop                     # the take is probed and appended to the timeline
qrec take discard-last               # didn't like it? (undoable)

qrec clip list
qrec clip split c7f3 --at 13.7s
qrec autocut --dry-run               # see what would be cut
qrec autocut
qrec undo
qrec export                          # exports/<name>-YYYYMMDD-HHMMSS.mp4
qrec frame save --at 00:00:42:10     # frames/<name>-<take>-<HHMMSSFF>.png
```

Times accept frames (`412f`), seconds (`13.7s`) and timecodes
(`00:00:13:21`, `00:00:13.700`). Destructive commands (`take purge`,
`history clear`, overwriting an export) need `--yes`. Run `qrec --help` or
`qrec <command> --help` for everything.

### Global key binds (niri)

`qrec doctor` prints a ready-to-paste snippet; record from any app with:

```kdl
binds {
    Mod+Shift+R { spawn "qrec" "record" "toggle"; }
    Mod+Shift+D { spawn "qrec" "take" "discard-last"; }
}
```

Compositor binds run in `$HOME`; the CLI then finds the single running
Qrec (a headless recording host or the window). With nothing running
yet, `$HOME` isn't a project ("Not my swamp"): either start the first take
from the project folder, or name it in the bind with
`"qrec" "--project" "/home/me/videos/my-video" "record" "toggle"`.

### For LLM agents

`qrec agent-guide` prints a concise guide. Every command takes `--json` and
prints exactly one `{"ok":…}` object; exit codes are 0 ok, 1 error,
2 usage, 3 not a project, 4 confirmation required, 5 missing dependency,
6 busy, 7 not found, 130 interrupted. `qrec schema` prints the JSON Schema
of `qrec.json`.

## Project folder

```
my-video/
├── qrec.json        # the project (source of truth, hand-editable)
├── takes/take-0001/ # one file per layer and track + recorder.log; never modified
├── exports/         # never overwritten
├── frames/
└── .qrec/           # lock, history, UI state, caches, logs
```

## Development

```sh
cargo test --workspace                               # ~1,900 tests; CLI tests spawn ffmpeg
cargo nextest run --workspace                        # the same but the doc-tests, a process each, ~2x faster
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p qrec-core                             # the pure core on its own
QREC_CAPTURE_BACKEND=fake qrec record start          # record test patterns, no devices
```

The window's tests are ignored by default: each `crates/qrec/tests/gui_*.rs`
suite runs the window in a nested headless sway, never on your desktop,
with the fake capture backend. Screenshots and logs go to
`tmp/gui-shots/<area>/` (docs/GUI.md G§8.4):

```sh
scripts/check-quick                  # fmt, clippy, the changed crates' tests and areas' suites
scripts/check-quick gui_stage gui_panel   # named suites (`all`: every one)
scripts/check-full                   # the milestone gate; summary in tmp/check-full/<date>/
cargo nextest run -p qrec --run-ignored only --test gui_smoke   # the end-to-end smoke (G§8.5)
QREC_TEST_WLR=gles2 cargo nextest run -p qrec --run-ignored only --test gui_smoke
QREC_TEST_SCALE=2 cargo test -p qrec --test gui_stage -- --ignored  # libtest works too
nix build && cargo test -p qrec --test gui_smoke packaged -- --ignored   # the package in nested sway
```

Several cases run at once, each with its own compositor; the
timing-sensitive ones run alone (`Options::exclusive`, G§8.4).

The compositor renders with pixman by default (software: nothing opens a
GPU node). `QREC_TEST_WLR=gles2` uses GL on the first Intel or AMD render
node, never NVIDIA's, and `QREC_TEST_SCALE` sets the output's scale. With
`QREC_GUI_PROBE=1` the window logs every main-loop stall over 50 ms and
what was running (the smoke runs with it).

The crates follow FCIS: `qrec-core` is pure (no I/O, clocks or
randomness; enforced by clippy), `qrec-shell` runs the effects, `qrec-cli`
maps arguments to commands, and `qrec` wires them. See `CLAUDE.md` for the
rules every change follows.

### GPU note

On machines with an NVIDIA GPU, the NVIDIA VA/CUDA drivers can hang when
probed. Qrec pins VA-API to the configured (or first Intel/AMD) render
node, sets `LIBVA_DRIVER_NAME` before any thread starts, keeps GES in
software, and ships GStreamer without the `nvcodec`/`qsv` plugins.

## Manual checks on real hardware

Automated tests use the fake backend. These need the real machine; the
results are from the dev machine (niri, PipeWire 1.6.6, GStreamer 1.26,
2026-10-15):

1. **Portal restore tokens (M0a):** `qrec scene layer add screen --output <name>`,
   `qrec record start`, pick the monitor in the portal dialog, stop. Repeat
   `record start` — no dialog should appear. Then try it from a niri bind
   (headless host) and after a reboot.
   *Result:* the second take started in ~0.75 s with no dialog. The niri bind
   and reboot cases are still to try.
2. **A/V sync (M0b):** two screens + camera + mic + system audio; clap on
   camera; export and check the clap's audio and video land on the same frame.
   Adjust per-track `qrec scene audio set A1 --sync-offset-ms N` if not.
   *Result:* screen vs system audio, measured with a flash+beep clip played
   by `ffplay`: a constant offset within each playback (audio 42–52 ms
   ahead in a screen + system take, 75 ms with camera and mic running too),
   no drift within a run. The offset includes the player's own output
   latency, so it isn't Qrec's. The clap test (camera + mic) still needs a
   person.
3. **Long take:** a 10-minute take, then `qrec export`; check duration and sync.
   *Result:* screen + camera + mic + system audio for 10 min 0.8 s: take
   `ok`, 18,024 frames (exact), every file to the end (screen 600.80 s,
   camera 600.27 s after a 0.53 s start, both audio tracks 600.77 s), the
   stop immediate, the export 18,024 frames with 600.8 s of audio. The
   flash runs at the start and the end measured 75 and 143 ms; each run is
   a separate player launch on a sink whose latency PipeWire changed in
   between, so this can't separate drift from playback latency. The stream
   lengths match the clock within 30 ms over 600 s.
4. `qrec doctor` on the machine reports every dependency and the VA node.
   *Result:* every check passes (VA on `renderD129`).

A still screen used to stop the screen stream for good (PipeWire 1.6's
`pipewiresrc` keepalive) and broke long takes; see A§8.11 "Still screens".
A camera that hangs mid-take is now lost after 3 s instead (SPEC §16).
