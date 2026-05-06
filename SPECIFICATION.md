# qrec — Quick Recording TUI

A terminal user interface for quick screen recording on Wayland, powered by `wf-recorder`.

## Overview

`qrec` is a Rust TUI application that wraps `wf-recorder` to provide an interactive screen recording workflow. It manages a timeline of recording chunks, allowing the user to record, preview, reorder, discard, and concatenate video segments — all from the terminal using vim-like keybindings.

## Architecture

### Layer Separation

The codebase is split into three layers with strict boundaries:

1. **Pure functions** — all business logic (timeline management, chunk ordering, JSON serialization, filename generation, command construction). These functions take data in and return data out with zero side effects. They are fully unit-testable.
2. **Side-effect boundary** — a thin adapter module that interacts with the outside world: spawning `wf-recorder` / `ffmpeg` / `mpv` processes, reading `wf-recorder -L` output, reading/writing `qrec.json`, and listing directory contents.
3. **TUI layer** — renders the UI and maps user input to calls into the pure and side-effect layers.

### Project Structure

```
src/
├── main.rs              # entry point, bootstraps the TUI
├── app.rs               # application state machine (pure)
├── timeline.rs          # timeline / chunk logic (pure)
├── config.rs            # qrec.json read/write types and defaults (pure serde)
├── command.rs           # builds shell command structs from intent (pure)
├── effects.rs           # side-effect adapter (process spawn, fs, wf-recorder queries)
└── ui.rs                # ratatui rendering (side-effect: terminal drawing only)
```

### Dev Environment

A `shell.nix` (or `flake.nix`) provides:

- `rustc`, `cargo`, `rustfmt`, `clippy`
- `wf-recorder`
- `ffmpeg`
- `mpv`
  Developers enter the environment with `nix-shell` (or `nix develop`).

## Data Model

### `qrec.json`

Stored in the working directory where `qrec` is invoked. Created on first run if absent.

```json
{
  "chunks": [
    {
      "id": "chunk-1",
      "file": "chunk-1.mp4",
      "duration_secs": 12.4,
      "recorded_at": "2026-05-03T14:22:01Z"
    },
    {
      "id": "chunk-2",
      "file": "chunk-2.mp4",
      "duration_secs": 5.1,
      "recorded_at": "2026-05-03T14:23:18Z"
    }
  ],
  "selected_screen": "HDMI-A-1",
  "selected_microphone": "alsa_output.pci-0000_00_1f.3.analog-stereo",
  "next_chunk_number": 3
}
```

- `chunks` — ordered list of all chunks in the timeline. The order in this array is the final render order.
- `next_chunk_number` — monotonically increasing counter. Never resets, even after deletions.
- `selected_screen` / `selected_microphone` — last-used device, restored on startup.

### Chunk Naming

Files are saved in the current working directory as `chunk-{n}.mp4` where `n` is `next_chunk_number` at the time of recording. `next_chunk_number` increments after each recording starts. Deleted chunks have their files removed from disk and their entries removed from `qrec.json`.

## TUI Layout

```
┌──────────────────────────────────────────────────────┐
│  qrec — Quick Recording                              │
│                                                      │
│  Screen:   [HDMI-A-1        ] ▸  list of outputs     │
│  Microphone:[Default         ] ▸  list of sources    │
│                                                      │
│  ● REC  [Start/Stop]                                 │
│                                                      │
│  ── Timeline ─────────────────────────────────────── │
│  ▓▓▓▓▓▓▓▓ ██ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓                        │
│                                                      │
├──────────────────────────────────────────────────────┤
│  Ready                                               │
└──────────────────────────────────────────────────────┘
```

### Timeline Strip

The timeline is a single-row horizontal strip that renders chunks as contiguous solid blocks (`▓`) side by side. No labels or numbers are drawn inside the blocks.

- **Width calculation** — each chunk's character width is `ceil(chunk_frames / frames_per_char)`, where `chunk_frames = duration_secs * fps` (fps defaulting to 30). The `frames_per_char` value is controlled by the zoom level.
- **Selected chunk** — the selected chunk is visually distinguished (e.g., inverted foreground/background or a different character like `█`). When a chunk is selected, the status bar shows its full details: filename, duration, and timestamp.
- **Empty state** — when no chunks exist, the strip displays a dim placeholder message (e.g., "No chunks recorded").
- **Recording indicator** — while recording, a pulsing block (`▒`) grows at the end of the strip to represent the in-progress chunk.

### Timeline Zoom and Panning

The zoom level controls `frames_per_char` — how many video frames each terminal character represents.

- **Maximum zoom in** (`i`) — `frames_per_char = 1`. One character per frame. A 10-second clip at 30fps renders as 300 characters.
- **Zoom out** (`o`) — each zoom-out step doubles `frames_per_char` (1 → 2 → 4 → 8 → 16 → ...).
- **Zoom in** (`i`) — each zoom-in step halves `frames_per_char`, down to a minimum of 1.
- **Default zoom** — chosen so that the total timeline fits within the viewport width. Specifically, `frames_per_char = ceil(total_frames / viewport_width)`, rounded up to the nearest power of 2.

When the total rendered width exceeds the viewport, the strip scrolls horizontally. The viewport auto-pans to keep the selected chunk visible after navigation or zoom changes.

### Status Bar

The bottom row of the TUI is a status bar that displays:

- Current state ("Ready", "Recording...", "Rendering...")
- Selected chunk info when the timeline is focused (e.g., "chunk-2.mp4 — 5.1s — 14:23:18")
- Error messages (e.g., "No chunk to remove")
- Action confirmations (e.g., "Deleted chunk-2.mp4")

Messages persist until the next action overwrites them or a short timeout clears them.

### Focus Regions

The TUI has two focus regions cycled with `Tab`:

1. **Controls** — screen selector, microphone selector, record start/stop.
2. **Timeline** — the horizontal strip of chunks.

The currently focused region is visually highlighted. Within the Controls region, `j`/`k` move between the screen, microphone, and record row. Within each selector row, `h`/`l` cycle through the available options.

When recording is in progress, all keys except `Enter` (stop recording) and `q` (quit, which stops recording first) are ignored. The TUI displays a locked state with a recording indicator.

## Keybindings

| Key       | Context        | Action                                                      |
| --------- | -------------- | ----------------------------------------------------------- |
| `q`       | global         | Quit the TUI                                                |
| `Tab`     | global         | Toggle focus between Controls and Timeline                  |
| `j` / `k` | Controls       | Move up/down between screen selector, mic selector, and rec |
| `h` / `l` | Controls (row) | Cycle through screen or microphone options                  |
| `Enter`   | Controls (rec) | Start or stop recording                                     |
| `h` / `l` | Timeline       | Move selection left/right through chunks                    |
| `H` / `L` | Timeline       | Reorder selected chunk left/right in the timeline           |
| `i` / `o` | Timeline       | Zoom in / zoom out the timeline scale                       |
| `d`       | Timeline       | Delete the currently selected chunk (file + entry)          |
| `d`       | Controls       | Discard the latest recorded chunk (file + entry)            |
| `r`       | global         | Render (concatenate) all chunks in timeline order           |
| `p`       | Timeline       | Preview the selected chunk in mpv                           |
| `P`       | global         | Preview the full rendered timeline in mpv                   |

### Keybinding Notes

- `d` is context-sensitive: in the Timeline it deletes the selected chunk; in the Controls region it discards the most recently recorded chunk (regardless of selection). If no chunks exist, the status bar shows "No chunk to remove".
- `H` and `L` (uppercase) swap the selected chunk with its left or right neighbor in the timeline array and update `qrec.json`.
- `i` and `o` adjust the zoom level of the timeline strip. The viewport auto-pans to keep the selected chunk visible after zooming.
- Preview (`p` / `P`) is blocked while recording is in progress.

## Functional Requirements

### FR-1: Screen Discovery

On startup, `qrec` runs `wf-recorder -L` and parses the output to build the list of available outputs. These populate the screen selector. If only one output exists, it is auto-selected.

### FR-2: Microphone Discovery

On startup, `qrec` runs `pactl list short sources` (or equivalent PipeWire/PulseAudio command) to list available audio sources. These populate the microphone selector. An option for "No microphone" (mute) is always present as the first entry.

### FR-3: Recording

When the user starts a recording:

1. Determine `next_chunk_number` from `qrec.json`.
2. Record the start timestamp.
3. Construct the `wf-recorder` command:
   ```
   wf-recorder --output <selected_screen> --audio <selected_mic> --file chunk-<n>.mp4
   ```
   If "No microphone" is selected, omit the `--audio` flag.
4. Spawn the process in the background.
5. The TUI enters recording mode: all input is locked except `Enter` (stop) and `q` (quit). The status bar shows "Recording...".
6. On stop (user presses `Enter`), the process receives `SIGINT`. Once the process exits, the chunk's duration is read from the output file.
7. `next_chunk_number` increments. The new chunk (with file, duration, and timestamp) is appended to `chunks` in `qrec.json`. Status bar shows "Recorded chunk-<n>.mp4".

### FR-4: Chunk Deletion

Deleting a chunk removes its entry from the `chunks` array in `qrec.json` and deletes the corresponding `.mp4` file from disk. Chunk numbers are **not** reused. `next_chunk_number` is never decremented.

### FR-5: Chunk Reordering

`H` moves the selected chunk one position earlier in the array. `L` moves it one position later. The change is persisted to `qrec.json` immediately.

### FR-6: Render (Concatenate)

Pressing `r` concatenates all chunks in timeline order into `output.mp4` using `ffmpeg`:

```
ffmpeg -f concat -safe 0 -i <file_list> -c copy output.mp4
```

A temporary concat demuxer file is created, used, and deleted. If `output.mp4` already exists, the user is prompted to overwrite.

### FR-7: Preview Chunk

Pressing `p` while a chunk is selected in the timeline opens it in `mpv`:

```
mpv chunk-<n>.mp4
```

`mpv` runs in the foreground; the TUI is suspended and resumes when `mpv` exits.

### FR-8: Preview Timeline

Pressing `P` plays all chunks in order:

```
mpv chunk-1.mp4 chunk-2.mp4 chunk-3.mp4 ...
```

The order follows the `chunks` array. The TUI is suspended and resumes when `mpv` exits.

### FR-9: State Persistence

`qrec.json` is written after every mutation:

- Chunk added
- Chunk deleted
- Chunk reordered
- Screen or microphone selection changed
- Recording started (next_chunk_number incremented)

On startup, `qrec.json` is read to restore state. If it does not exist, it is created with defaults.

### FR-10: Quit

Pressing `q` quits the TUI. If a recording is in progress, the user is prompted to stop it first or confirm quit (which stops the recording).

## Dependency Graph

```
qrec
├── ratatui          (TUI framework)
├── crossterm        (terminal backend)
├── serde + serde_json  (qrec.json serialization)
├── anyhow           (error handling)
└── tempfile         (temporary concat list for ffmpeg)
```

Runtime dependencies (provided by nix-shell):

- `wf-recorder`
- `ffmpeg`
- `mpv`
- `pulseaudio` / `pipewire` (for `pactl`)

## File Outputs

| File            | Purpose                                    |
| --------------- | ------------------------------------------ |
| `chunk-{n}.mp4` | Individual recorded segments               |
| `qrec.json`     | Project state (timeline, devices, counter) |
| `output.mp4`    | Final rendered concatenation               |

## Testing Strategy

Because the pure layer contains all business logic, tests focus there:

- **timeline tests** — add, remove, reorder chunks; verify array order and next_chunk_number.
- **command tests** — given a recording intent, verify the correct `wf-recorder` command string is built.
- **config tests** — serialize/deserialize `qrec.json`; verify round-trip fidelity.
- **app state tests** — simulate key events and verify state transitions (focus changes, selection changes, recording state machine).

The side-effect layer (`effects.rs`) is tested with integration tests that run actual `wf-recorder -L` and `pactl` commands in the nix-shell environment, or are mocked in CI.

## Error Handling

- If `wf-recorder` is not found, display an error message in the TUI and exit gracefully.
- If a chunk file is missing from disk but referenced in `qrec.json`, show a warning in the timeline and offer to clean up the entry.
- If `ffmpeg` concatenation fails, surface the error output to the status bar.
- If `mpv` fails to launch, show an error in the status bar.
